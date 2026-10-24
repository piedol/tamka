// Tamka's speech engine: a helper process that keeps one model loaded and turns
// WAV files into text. Tamka.app runs it from Contents/Helpers; eval/bench.sh
// runs the same binary for benchmarks.
//
//   tamka-engine parakeet <v2|v3|ultra|phonon2|redux> [--boost | --boost-safe] [--itn]
//     --itn  write spoken numbers, money and dates as digits ("three hundred" → "300")
//   tamka-engine apple            (SpeechTranscriber, no vocabulary support)
//   tamka-engine apple-dictation  (DictationTranscriber, takes the glossary as hints)
//   tamka-engine status | download <model> | remove <model>   (model management)
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
