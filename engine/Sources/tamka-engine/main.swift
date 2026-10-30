// Tamka's speech engine: a helper process that keeps one model loaded and turns
// WAV files into text. Tamka.app runs it from Contents/Helpers; eval/bench.sh
// runs the same binary for benchmarks.
//
//   tamka-engine parakeet <v2|v3|ultra|phonon2|redux> [--boost | --boost-safe] [--itn]
//     --itn  write spoken numbers, money and dates as digits ("three hundred" → "300")
//   tamka-engine apple            (SpeechTranscriber, no vocabulary support)
//   tamka-engine apple-dictation  (DictationTranscriber, takes the glossary as hints)
//   tamka-engine status | download <model> | remove <model>   (model management)
//   tamka-engine custom <owner/name>   (a Parakeet model added from Hugging Face)
//   tamka-engine inspect <owner/name> | discover [known…]   (Hugging Face models Tamka can run)
//   tamka-engine decode <audio or video file> <out.wav>   (16 kHz mono WAV for Transcribe Files)
//
// Protocol (JSON lines; frameworks may print noise to stdout, so readers search for
// the markers rather than expecting clean lines):
//   out: READY {"pid": .., "load_ms": ..}       after the model is loaded and warmed up
//   in:  {"wav": "/abs/path.wav", "prompt": ""}  one request per line
//   out: {"text": "..", "ms": ..[, "error": ".."]}
//   in:  QUIT, or end of input -> exit

import AVFoundation
import CoreML
import FluidAudio
import Foundation
import Speech

setvbuf(stdout, nil, _IOLBF, 0)

func emit(_ obj: [String: Any]) {
    let data = try! JSONSerialization.data(withJSONObject: obj)
    print(String(data: data, encoding: .utf8)!)
}

func nowMs() -> Double { Double(DispatchTime.now().uptimeNanoseconds) / 1_000_000 }

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write((msg + "\n").data(using: .utf8)!)
    exit(1)
}

protocol Engine {
    func transcribe(_ url: URL, prompt: String) async throws -> String
    /// Run one tiny inference so the first real dictation doesn't pay for warm-up.
    func warmUp() async
}

// MARK: Parakeet (FluidAudio)

final class ParakeetEngine: Engine {
    let asr: AsrManager
    let boost: Bool
    let rescorerConfig: VocabularyRescorer.Config?
    var ctc: CtcModels?
    var session: VocabularyBoostingSession?
    var sessionPrompt = ""

    /// `safe` turns off the spotter-anchored rescue pass, which FluidAudio's own notes
    /// recommend for short vocabularies: it over-fires and replaces whole phrases.
    /// Loads an already-downloaded model. Downloads only happen through `download`,
    /// when the user asks for them in Settings.
    init(version: AsrModelVersion, boost: Bool, safe: Bool) async throws {
        let dir = AsrModels.defaultCacheDirectory(for: version)
        guard AsrModels.modelsExist(at: dir, version: version) else {
            fail("NOT_DOWNLOADED This model isn't downloaded yet.")
        }
        let models = try await AsrModels.load(from: dir, version: version)
        asr = AsrManager(config: .default)
        try await asr.loadModels(models)
        self.boost = boost || safe
        rescorerConfig = safe ? VocabularyRescorer.Config(spotterRescueEnabled: false) : nil
        if self.boost { ctc = try await CtcModels.downloadAndLoad() }
    }

    /// A model added from Hugging Face (see `CustomModels`), loaded from its own folder.
    init(custom repo: String) async throws {
        guard let model = CustomModels.installed(repo) else {
            fail("NOT_DOWNLOADED This model isn't downloaded yet.")
        }
        let models = try AsrModels.loadLocal(from: model.dir, version: model.version)
        asr = AsrManager(config: .default)
        try await asr.loadModels(models)
        boost = false
        rescorerConfig = nil
    }

    func vocabularySession(for prompt: String) async throws -> VocabularyBoostingSession? {
        guard boost, let ctc, !prompt.isEmpty else { return nil }
        if let session, sessionPrompt == prompt { return session }
        let terms = prompt.split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
            .map { CustomVocabularyTerm(text: $0) }
        session = try await VocabularyBoostingSession(
            vocabulary: CustomVocabularyContext(terms: terms), ctcModels: ctc, config: rescorerConfig)
        sessionPrompt = prompt
        return session
    }

    func warmUp() async {
        let silence = [Float](repeating: 0, count: 16_000)
        var state = try? TdtDecoderState(decoderLayers: await asr.decoderLayerCount)
        if state != nil { _ = try? await asr.transcribe(silence, decoderState: &state!) }
    }

    func transcribe(_ url: URL, prompt: String) async throws -> String {
        let samples = try AudioConverter().resampleAudioFile(url)
        var state = try TdtDecoderState(decoderLayers: await asr.decoderLayerCount)
        let result = try await asr.transcribe(samples, decoderState: &state)
        if let session = try await vocabularySession(for: prompt),
           let timings = result.tokenTimings,
           let out = await session.rescore(text: result.text, tokenTimings: timings, audioSamples: samples) {
            return out.text
        }
        return result.text
    }
}

// MARK: Apple SpeechAnalyzer

final class AppleEngine: Engine {
    let locale: Locale
    let dictation: Bool

    init(dictation: Bool) async throws {
        self.dictation = dictation
        guard let loc = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: "en-US")) else {
            fail("en-US not supported by SpeechTranscriber")
        }
        locale = loc
        guard await AppleModel.isInstalled(loc) else {
            fail("NOT_DOWNLOADED Apple's English speech model isn't installed yet.")
        }
    }

    func warmUp() async {}

    func makeModule() -> any SpeechModule {
        if dictation {
            return DictationTranscriber(locale: locale, contentHints: [], transcriptionOptions: [.punctuation],
                                        reportingOptions: [], attributeOptions: [])
        }
        return SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])
    }

    func transcribe(_ url: URL, prompt: String) async throws -> String {
        let file = try AVAudioFile(forReading: url)
        let module = makeModule()
        let analyzer = SpeechAnalyzer(modules: [module])
        if dictation, !prompt.isEmpty {
            let context = AnalysisContext()
            context.contextualStrings[.general] = prompt.split(separator: ",")
                .map { $0.trimmingCharacters(in: .whitespaces) }
            try await analyzer.setContext(context)
        }
        let collector = Task { () -> String in
            var text = ""
            if let t = module as? SpeechTranscriber {
                for try await r in t.results where r.isFinal { text += String(r.text.characters) }
            } else if let t = module as? DictationTranscriber {
                for try await r in t.results where r.isFinal { text += String(r.text.characters) }
            }
            return text
        }
        try await analyzer.start(inputAudioFile: file, finishAfterFile: true)
        return try await collector.value
    }
}

// MARK: Model management (status / download / remove)

let parakeetVersions: [String: AsrModelVersion] = [
    "v2": .v2, "v3": .v3, "ultra": .ultra, "phonon2": .phonon2, "redux": .redux,
]

enum AppleModel {
    static func locale() async -> Locale? {
        await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: "en-US"))
    }

    static func isInstalled(_ locale: Locale) async -> Bool {
        await SpeechTranscriber.installedLocales.contains { $0.identifier(.bcp47) == locale.identifier(.bcp47) }
    }
}

/// Bytes on disk under `url` (0 when missing).
func folderBytes(_ url: URL) -> Int64 {
    guard let walker = FileManager.default.enumerator(
        at: url, includingPropertiesForKeys: [.totalFileAllocatedSizeKey], options: [])
    else { return 0 }
    var total: Int64 = 0
    for case let file as URL in walker {
        total += Int64((try? file.resourceValues(forKeys: [.totalFileAllocatedSizeKey]))?.totalFileAllocatedSize ?? 0)
    }
    return total
}

/// `tamka-engine status`: one line per model: {"id", "downloaded", "bytes"}.
func modelStatus() async {
    for (name, version) in parakeetVersions.sorted(by: { $0.key < $1.key }) {
        let dir = AsrModels.defaultCacheDirectory(for: version)
        let ok = AsrModels.modelsExist(at: dir, version: version)
        emit(["id": "parakeet \(name)", "downloaded": ok, "bytes": ok ? folderBytes(dir) : 0])
    }
    if let loc = await AppleModel.locale() {
        emit(["id": "apple", "downloaded": await AppleModel.isInstalled(loc), "bytes": 0])
    }
    for repo in CustomModels.all() {
        emit(["id": "custom \(repo)", "downloaded": true, "bytes": folderBytes(CustomModels.folder(repo))])
    }
}

/// `tamka-engine download <model…>`: progress lines {"progress": 0–1, "phase": …},
/// then {"done": true} or {"error": …}.
func downloadModel(_ model: [String]) async {
    do {
        if model.first == "apple" {
            guard let loc = await AppleModel.locale() else { throw EngineError.message("English isn't supported here.") }
            let module = SpeechTranscriber(locale: loc, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [module]) {
                let progress = request.progress
                let ticker = Task {
                    while !Task.isCancelled {
                        emit(["progress": progress.fractionCompleted, "phase": "downloading"])
                        try? await Task.sleep(nanoseconds: 250_000_000)
                    }
                }
                try await request.downloadAndInstall()
                ticker.cancel()
            }
        } else if model.first == "custom", model.count > 1 {
            try await CustomModels.download(model[1], throttle: ProgressThrottle())
        } else if model.first == "parakeet", model.count > 1, let version = parakeetVersions[model[1]] {
            let throttle = ProgressThrottle()
            _ = try await AsrModels.download(version: version) { p in
                let phase: String
                switch p.phase {
                case .listing: phase = "preparing"
                case .downloading: phase = "downloading"
                case .compiling: phase = "optimizing"
                }
                throttle.emit(p.fractionCompleted, phase)
            }
            // Compile for the Neural Engine now, so the first dictation doesn't wait for it.
            emit(["progress": 1.0, "phase": "optimizing"])
            _ = try await AsrModels.load(from: AsrModels.defaultCacheDirectory(for: version), version: version)
        } else {
            throw EngineError.message("Unknown model \(model.joined(separator: " "))")
        }
        emit(["done": true])
    } catch {
        emit(["error": error.localizedDescription])
    }
}

/// `tamka-engine remove <model…>`: Parakeet files go to the Trash (recoverable);
/// Apple's model is released back to macOS, which removes it when it wants the space.
func removeModel(_ model: [String]) async {
    do {
        if model.first == "apple" {
            if let loc = await AppleModel.locale() { _ = await AssetInventory.release(reservedLocale: loc) }
        } else if model.first == "custom", model.count > 1 {
            // Also clears a download that was stopped part way.
            let dir = CustomModels.folder(model[1])
            let partial = dir.deletingLastPathComponent().appendingPathComponent("." + dir.lastPathComponent + ".partial")
            for url in [dir, partial] where FileManager.default.fileExists(atPath: url.path) {
                try FileManager.default.trashItem(at: url, resultingItemURL: nil)
            }
            let owner = dir.deletingLastPathComponent()
            if (try? FileManager.default.contentsOfDirectory(atPath: owner.path))?.isEmpty == true {
                try? FileManager.default.removeItem(at: owner)
            }
        } else if model.first == "parakeet", model.count > 1, let version = parakeetVersions[model[1]] {
            let dir = AsrModels.defaultCacheDirectory(for: version)
            if FileManager.default.fileExists(atPath: dir.path) {
                try FileManager.default.trashItem(at: dir, resultingItemURL: nil)
            }
        } else {
            throw EngineError.message("Unknown model \(model.joined(separator: " "))")
        }
        emit(["done": true])
    } catch {
        emit(["error": error.localizedDescription])
    }
}

/// Download callbacks fire thousands of times; report every half percent or phase change.
final class ProgressThrottle: @unchecked Sendable {
    private let lock = NSLock()
    private var last = -1.0
    private var phase = ""

    func emit(_ value: Double, _ phase: String) {
        lock.lock()
        defer { lock.unlock() }
        guard phase != self.phase || value - last >= 0.005 || value >= 1 else { return }
        last = value
        self.phase = phase
        // FluidAudio reports downloading as 0–0.5 and compiling as 0.5–1; show the
        // download as a full bar of its own.
        let shown = phase == "downloading" ? min(value * 2, 1) : value
        print("{\"progress\":\(shown),\"phase\":\"\(phase)\"}")
    }
}

enum EngineError: LocalizedError {
    case message(String)
    var errorDescription: String? {
        switch self { case .message(let m): return m }
    }
}

// MARK: Models added from Hugging Face

/// Parakeet models from Hugging Face that use the same Core ML layout as FluidAudio's
/// own: Preprocessor, Encoder, Decoder and JointDecision (v2) or JointDecisionv3 (v3
/// and its retrains), plus parakeet_vocab.json. Each is kept in
/// ~/Library/Application Support/Tamka/Models/<owner>/<name>/ with a tamka-model.json
/// that records which design it uses.
enum CustomModels {
    struct Plan {
        let repo: String
        let family: String  // "v2" or "v3"
        let files: [(path: String, size: Int64)]  // repo paths
        let prefix: String  // folder inside the repo holding the model, "" at the top
        var bytes: Int64 { files.reduce(0) { $0 + $1.size } }
    }

    struct Installed {
        let dir: URL
        let family: String
        var version: AsrModelVersion { family == "v2" ? .v2 : .v3 }
    }

    static var root: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Tamka/Models", isDirectory: true)
    }

    static func folder(_ repo: String) -> URL { root.appendingPathComponent(repo, isDirectory: true) }

    static func installed(_ repo: String) -> Installed? {
        let dir = folder(repo)
        guard let data = try? Data(contentsOf: dir.appendingPathComponent("tamka-model.json")),
              let info = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let family = info["family"] as? String
        else { return nil }
        return Installed(dir: dir, family: family)
    }

    /// Every model downloaded this way, as "owner/name".
    static func all() -> [String] {
        let fm = FileManager.default
        guard let owners = try? fm.contentsOfDirectory(atPath: root.path) else { return [] }
        var found: [String] = []
        for owner in owners.sorted() where !owner.hasPrefix(".") {
            let names = (try? fm.contentsOfDirectory(atPath: root.appendingPathComponent(owner).path)) ?? []
            for name in names.sorted() where !name.hasPrefix(".") && installed("\(owner)/\(name)") != nil {
                found.append("\(owner)/\(name)")
            }
        }
        return found
    }

    static func get(_ url: URL) async throws -> Data {
        var request = URLRequest(url: url)
        request.timeoutInterval = 30
        let (data, response) = try await URLSession.shared.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        if status == 404 || status == 401 {
            throw EngineError.message("Hugging Face has no public model at that link.")
        }
        guard status == 200 else { throw EngineError.message("Hugging Face answered with error \(status). Try again later.") }
        return data
    }

    /// Read the model's file list (and its vocabulary, to tell v2 from v3) and work out
    /// what to download. Throws a plain explanation when Tamka can't run it.
    static func plan(_ repo: String) async throws -> Plan {
        guard let url = URL(string: "https://huggingface.co/api/models/\(repo)/tree/main?recursive=true") else {
            throw EngineError.message("That isn't a Hugging Face model link.")
        }
        let data = try await get(url)
        guard let items = try JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            throw EngineError.message("Hugging Face sent a file list Tamka couldn't read.")
        }
        var files: [String: Int64] = [:]
        for item in items where item["type"] as? String == "file" {
            guard let path = item["path"] as? String else { continue }
            let lfs = (item["lfs"] as? [String: Any])?["size"] as? NSNumber
            files[path] = (lfs ?? item["size"] as? NSNumber)?.int64Value ?? 0
        }
        func has(_ component: String, _ prefix: String) -> Bool {
            files.keys.contains { $0.hasPrefix(prefix + component + "/") }
        }
        // The model may sit at the top of the repo or in a folder of its own.
        let prefixes = Set(files.keys.compactMap { path -> String? in
            guard let r = path.range(of: "Preprocessor.mlmodelc/") else { return nil }
            return String(path[..<r.lowerBound])
        }).sorted { $0.count < $1.count }
        guard !prefixes.isEmpty else {
            throw EngineError.message(
                "This isn't a Core ML Parakeet model Tamka can run. Tamka needs the Core ML files (Preprocessor, Encoder, Decoder and JointDecision) that FluidInference's Parakeet models use.")
        }
        for prefix in prefixes {
            let joint: String
            if has("JointDecisionv3.mlmodelc", prefix) { joint = "JointDecisionv3.mlmodelc" }
            else if has("JointDecision.mlmodelc", prefix) { joint = "JointDecision.mlmodelc" }
            else { continue }
            let vocab = prefix + "parakeet_vocab.json"
            guard has("Encoder.mlmodelc", prefix), has("Decoder.mlmodelc", prefix), files[vocab] != nil else { continue }
            // v2 has 1,024 tokens before its blank; v3 and its retrains have 8,192.
            guard let vocabURL = URL(string: "https://huggingface.co/\(repo)/resolve/main/\(vocab)") else { continue }
            let tokens = (try? JSONSerialization.jsonObject(with: try await get(vocabURL)) as? [String: Any])?.count ?? 0
            let family: String
            if joint == "JointDecisionv3.mlmodelc", tokens >= 8192 { family = "v3" }
            else if joint == "JointDecision.mlmodelc", (1024..<2048).contains(tokens) { family = "v2" }
            else { continue }
            let wanted = ["Preprocessor.mlmodelc/", "Encoder.mlmodelc/", "Decoder.mlmodelc/", joint + "/"].map { prefix + $0 }
            let picked = files.filter { path, _ in path == vocab || wanted.contains { path.hasPrefix($0) } }
                .map { (path: $0.key, size: $0.value) }
                .sorted { $0.path < $1.path }
            return Plan(repo: repo, family: family, files: picked, prefix: prefix)
        }
        throw EngineError.message(
            "This Parakeet model uses a design Tamka's engine doesn't support yet. Tamka runs models built like Parakeet v2 or v3.")
    }

    /// Download the planned files into a temporary folder, check the model loads, then
    /// move it into place. Progress lines as for the built-in models.
    static func download(_ repo: String, throttle: ProgressThrottle) async throws {
        throttle.emit(0, "preparing")
        let plan = try await plan(repo)
        let fm = FileManager.default
        let final = folder(repo)
        let partial = final.deletingLastPathComponent().appendingPathComponent("." + final.lastPathComponent + ".partial")
        try? fm.removeItem(at: partial)
        try fm.createDirectory(at: partial, withIntermediateDirectories: true)
        let total = max(plan.bytes, 1)
        var done: Int64 = 0
        for file in plan.files {
            let relative = String(file.path.dropFirst(plan.prefix.count))
            let target = partial.appendingPathComponent(relative)
            try fm.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
            guard let url = URL(string: "https://huggingface.co/\(repo)/resolve/main/\(file.path)") else { continue }
            let base = done
            try await fetch(url, to: target) { received in
                throttle.emit(Double(base + received) / Double(total) / 2, "downloading")
            }
            done += file.size
        }
        let info: [String: Any] = ["repo": repo, "family": plan.family, "bytes": plan.bytes]
        try JSONSerialization.data(withJSONObject: info).write(to: partial.appendingPathComponent("tamka-model.json"))
        // Load it once: proves it works and prepares it for the Neural Engine, so the first
        // dictation doesn't wait.
        throttle.emit(1, "optimizing")
        do {
            _ = try AsrModels.loadLocal(from: partial, version: plan.family == "v2" ? .v2 : .v3)
        } catch {
            try? fm.trashItem(at: partial, resultingItemURL: nil)
            throw EngineError.message("The model downloaded but didn't load: \(error.localizedDescription)")
        }
        if fm.fileExists(atPath: final.path) { try fm.trashItem(at: final, resultingItemURL: nil) }
        try fm.moveItem(at: partial, to: final)
    }

    /// One file, reporting bytes received so far.
    static func fetch(_ url: URL, to target: URL, progress: @escaping @Sendable (Int64) -> Void) async throws {
        try await withCheckedThrowingContinuation { (done: CheckedContinuation<Void, Error>) in
            let box = TaskBox()
            let task = URLSession.shared.downloadTask(with: url) { temp, response, error in
                box.timer?.cancel()
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if let error { return done.resume(throwing: error) }
                guard status == 200, let temp else {
                    return done.resume(throwing: EngineError.message("Downloading \(url.lastPathComponent) failed (error \(status))."))
                }
                do {
                    try? FileManager.default.removeItem(at: target)
                    try FileManager.default.moveItem(at: temp, to: target)
                    done.resume()
                } catch {
                    done.resume(throwing: error)
                }
            }
            let timer = DispatchSource.makeTimerSource(queue: .global())
            timer.schedule(deadline: .now() + 0.25, repeating: 0.25)
            timer.setEventHandler { progress(task.countOfBytesReceived) }
            box.timer = timer
            timer.resume()
            task.resume()
        }
    }

    final class TaskBox: @unchecked Sendable { var timer: DispatchSourceTimer? }

    /// Parakeet models FluidInference publishes that Tamka can run, other than the ones
    /// already in its list.
    static func discover(skipping known: Set<String>) async throws -> [Plan] {
        guard let url = URL(string: "https://huggingface.co/api/models?author=FluidInference&search=parakeet&limit=100")
        else { return [] }
        let data = try await get(url)
        let listed = (try JSONSerialization.jsonObject(with: data) as? [[String: Any]]) ?? []
        var found: [Plan] = []
        for item in listed {
            guard let id = item["id"] as? String, id.lowercased().hasSuffix("coreml"), !known.contains(id) else { continue }
            if let plan = try? await plan(id) { found.append(plan) }
        }
        return found
    }
}

// MARK: Decoding files (Transcribe Files)

/// Read the sound from any audio or video file AVFoundation can open (m4a, mp3, wav,
/// aiff, flac, caf, mov, mp4, …), mix every audio track down to 16 kHz mono and write
/// it as a 16-bit WAV, the format the engines read. Returns the length in seconds.
func decode(_ input: URL, to output: URL) async throws -> Double {
    let asset = AVURLAsset(url: input)
    let tracks: [AVAssetTrack]
    do {
        tracks = try await asset.loadTracks(withMediaType: .audio)
    } catch {
        throw EngineError.message("Tamka can't open this file.")
    }
    guard !tracks.isEmpty else { throw EngineError.message("This file has no sound to transcribe.") }
    let reader = try AVAssetReader(asset: asset)
    let out = AVAssetReaderAudioMixOutput(audioTracks: tracks, audioSettings: [
        AVFormatIDKey: kAudioFormatLinearPCM,
        AVSampleRateKey: 16_000,
        AVNumberOfChannelsKey: 1,
        AVLinearPCMBitDepthKey: 16,
        AVLinearPCMIsFloatKey: false,
        AVLinearPCMIsBigEndianKey: false,
        AVLinearPCMIsNonInterleaved: false,
    ])
    guard reader.canAdd(out) else { throw EngineError.message("Tamka can't read the sound in this file.") }
    reader.add(out)
    guard reader.startReading() else {
        throw EngineError.message(reader.error?.localizedDescription ?? "Tamka can't read the sound in this file.")
    }

    FileManager.default.createFile(atPath: output.path, contents: nil)
    let file = try FileHandle(forWritingTo: output)
    defer { try? file.close() }
    try file.write(contentsOf: wavHeader(dataBytes: 0))
    var bytes: UInt32 = 0
    while let buffer = out.copyNextSampleBuffer() {
        guard let block = CMSampleBufferGetDataBuffer(buffer) else { continue }
        let length = CMBlockBufferGetDataLength(block)
        var data = Data(count: length)
        let status = data.withUnsafeMutableBytes { raw in
            CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: raw.baseAddress!)
        }
        guard status == kCMBlockBufferNoErr else { continue }
        try file.write(contentsOf: data)
        bytes &+= UInt32(clamping: length)
    }
    if reader.status == .failed {
        throw EngineError.message(reader.error?.localizedDescription ?? "Tamka couldn't read the whole file.")
    }
    guard bytes > 0 else { throw EngineError.message("This file has no sound to transcribe.") }
    try file.seek(toOffset: 0)
    try file.write(contentsOf: wavHeader(dataBytes: bytes))
    return Double(bytes) / 32_000
}

/// A 44-byte header for 16 kHz, 16-bit mono PCM.
func wavHeader(dataBytes: UInt32) -> Data {
    var d = Data()
    func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
    d.append(contentsOf: Array("RIFF".utf8)); u32(36 &+ dataBytes)
    d.append(contentsOf: Array("WAVEfmt ".utf8)); u32(16); u16(1); u16(1)
    u32(16_000); u32(32_000); u16(2); u16(16)
    d.append(contentsOf: Array("data".utf8)); u32(dataBytes)
    return d
}

// MARK: Main loop

let args = Array(CommandLine.arguments.dropFirst())
guard let kind = args.first else {
    fail("usage: tamka-engine parakeet <version> [--boost] | apple | apple-dictation | status | download <model> | remove <model>")
}
switch kind {
case "status":
    await modelStatus()
    exit(0)
case "download":
    await downloadModel(Array(args.dropFirst()))
    exit(0)
case "remove":
    await removeModel(Array(args.dropFirst()))
    exit(0)
case "inspect":
    // `tamka-engine inspect <owner/name>`: whether Tamka can run a Hugging Face model.
    guard args.count == 2 else { fail("usage: tamka-engine inspect <owner/name>") }
    do {
        let plan = try await CustomModels.plan(args[1])
        emit(["repo": plan.repo, "family": plan.family, "bytes": plan.bytes])
    } catch {
        emit(["error": error.localizedDescription])
    }
    exit(0)
case "discover":
    // `tamka-engine discover [known repos…]`: FluidInference's other Parakeet models Tamka can run.
    do {
        for plan in try await CustomModels.discover(skipping: Set(args.dropFirst())) {
            emit(["repo": plan.repo, "family": plan.family, "bytes": plan.bytes])
        }
        emit(["done": true])
    } catch {
        emit(["error": error.localizedDescription])
    }
    exit(0)
case "decode":
    guard args.count == 3 else { fail("usage: tamka-engine decode <audio or video file> <out.wav>") }
    do {
        let seconds = try await decode(URL(fileURLWithPath: args[1]), to: URL(fileURLWithPath: args[2]))
        emit(["seconds": seconds])
        exit(0)
    } catch {
        fail(error.localizedDescription)
    }
default:
    break
}

let t0 = nowMs()
let useITN = args.contains("--itn")
let engine: Engine
switch kind {
case "parakeet":
    let versions: [String: AsrModelVersion] = ["v2": .v2, "v3": .v3, "ultra": .ultra, "phonon2": .phonon2, "redux": .redux]
    guard args.count > 1, let v = versions[args[1]] else { fail("parakeet needs one of \(versions.keys.sorted())") }
    engine = try await ParakeetEngine(
        version: v, boost: args.contains("--boost"), safe: args.contains("--boost-safe"))
case "custom":
    guard args.count > 1 else { fail("custom needs the model's owner/name") }
    engine = try await ParakeetEngine(custom: args[1])
case "apple":
    engine = try await AppleEngine(dictation: false)
case "apple-dictation":
    engine = try await AppleEngine(dictation: true)
default:
    fail("unknown engine \(kind)")
}
await engine.warmUp()
print("READY " + String(data: try JSONSerialization.data(withJSONObject: [
    "pid": Int(getpid()), "load_ms": Int(nowMs() - t0),
]), encoding: .utf8)!)

while let line = readLine() {
    if line == "QUIT" { break }
    guard let data = line.data(using: .utf8),
          let req = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          let wav = req["wav"] as? String else { continue }
    let prompt = req["prompt"] as? String ?? ""
    let start = nowMs()
    do {
        var text = try await engine.transcribe(URL(fileURLWithPath: wav), prompt: prompt)
        if useITN { text = TextNormalizer.shared.normalizeSentence(text) }
        emit(["text": text, "ms": nowMs() - start])
    } catch {
        emit(["text": "", "ms": nowMs() - start, "error": "\(error)"])
    }
}
