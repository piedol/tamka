# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Run one speech engine over every clip and save its transcripts and timings.

    uv run run.py whispercpp --model /path/ggml-large-v3-turbo-q8_0.bin --name whisper-turbo
    uv run run.py worker --name parakeet-v2 -- engines/swift-worker/.build/release/stt-worker parakeet v2
    uv run run.py --set public worker --name ...     (clips from sets/public/clips)

Each engine loads its model once, transcribes one warm-up clip, then every clip in
clips/ (or sets/<name>/clips with --set). Latency is measured per clip, from handing over the finished audio to getting
text back (the same moment as "stop dictating" -> "text ready"). Peak memory is the
engine process's physical footprint (vmmap), read before it exits.

Worker protocol (engines/*.py and any other language):
  stdout first line:  READY {"pid": <engine pid>, "load_ms": <model load ms>}
  stdin per clip:     {"wav": "/abs/path.wav", "prompt": "optional glossary"}
  stdout per clip:    {"text": "...", "ms": <inference ms>}
  stdin "QUIT" -> exit
"""

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
import urllib.request
import uuid
from pathlib import Path

HERE = Path(__file__).parent
CLIPS = HERE / "clips"
CACHE = HERE / ".cache" / "wav"
RESULTS = HERE / "results"


def use_set(name: str) -> None:
    """Point clips, cache and results at sets/<name>/ instead of the defaults."""
    global CLIPS, CACHE, RESULTS
    base = HERE / "sets" / name
    CLIPS, CACHE, RESULTS = base / "clips", base / ".cache" / "wav", base / "results"
AUDIO_EXT = {".wav", ".m4a", ".mp3", ".aiff", ".aif", ".caf", ".flac", ".ogg", ".mp4", ".mov", ".qta"}
GLOSSARY = HERE / "glossary.txt"


def clip_list() -> list[tuple[str, Path]]:
    clips = []
    for f in sorted(CLIPS.iterdir()) if CLIPS.exists() else []:
        if f.suffix.lower() in AUDIO_EXT and (CLIPS / f"{f.stem}.txt").exists():
            clips.append((f.stem, to_wav16k(f)))
    return clips


def to_wav16k(src: Path) -> Path:
    CACHE.mkdir(parents=True, exist_ok=True)
    dst = CACHE / f"{src.stem}.wav"
    if not dst.exists() or dst.stat().st_mtime < src.stat().st_mtime:
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(src),
                        "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le", str(dst)], check=True)
    return dst


def audio_seconds(wav: Path) -> float:
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(wav)],
                         capture_output=True, text=True, check=True).stdout
    return float(out.strip())


def footprint_mb(pid: int) -> tuple[float | None, float | None]:
    out = subprocess.run(["vmmap", "--summary", str(pid)], capture_output=True, text=True).stdout

    def grab(label: str) -> float | None:
        m = re.search(rf"^{re.escape(label)}:\s+([\d.]+)([KMG])", out, re.M)
        if not m:
            return None
        return float(m.group(1)) * {"K": 1 / 1024, "M": 1, "G": 1024}[m.group(2)]

    return grab("Physical footprint"), grab("Physical footprint (peak)")


def wired_mb() -> float:
    """System-wide wired memory. Neural Engine models keep their weights in wired
    kernel memory rather than in the app's own footprint, so the change in this
    number while a model is loaded is the fairer cost for them. Noisy on a busy Mac."""
    out = subprocess.run(["vm_stat"], capture_output=True, text=True).stdout
    page = int(re.search(r"page size of (\d+)", out).group(1))
    pages = int(re.search(r"Pages wired down:\s+(\d+)", out).group(1))
    return pages * page / 1048576


def glossary_prompt() -> str:
    if not GLOSSARY.exists():
        return ""
    words = [w.strip() for w in GLOSSARY.read_text().splitlines() if w.strip()]
    # Some glossary entries are lowercase (such as "postgres"); capitalize those so
    # no model is nudged toward lowercase proper nouns. Mixed case ("iPhone") is kept.
    words = [w[:1].upper() + w[1:] if w.islower() else w for w in words]
    return ", ".join(words)


class WhisperCpp:
    """Whisper through whisper.cpp's whisper-server (Metal), the baseline."""

    def __init__(self, model: str, prompt: str, extra: list[str]):
        if not shutil.which("whisper-server"):
            sys.exit("whisper-server not found: brew install whisper-cpp")
        self.port = 8977
        args = ["whisper-server", "-m", model, "--port", str(self.port), "-l", "en", *extra]
        if prompt:
            args += ["--prompt", prompt]
        t0 = time.perf_counter()
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(240):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{self.port}/", timeout=1)
                break
            except Exception:
                time.sleep(0.25)
        else:
            self.proc.kill()
            sys.exit("whisper-server did not start")
        self.load_ms = (time.perf_counter() - t0) * 1000
        self.pid = self.proc.pid

    def transcribe(self, wav: Path) -> tuple[str, float]:
        boundary = uuid.uuid4().hex
        body = (f"--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\ntext\r\n"
                f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\n"
                f"Content-Type: audio/wav\r\n\r\n").encode() + wav.read_bytes() + f"\r\n--{boundary}--\r\n".encode()
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}/inference", data=body,
                                     headers={"Content-Type": f"multipart/form-data; boundary={boundary}"})
        t0 = time.perf_counter()
        text = urllib.request.urlopen(req, timeout=600).read().decode()
        return " ".join(text.split()), (time.perf_counter() - t0) * 1000

    def close(self):
        self.proc.terminate()
        self.proc.wait(timeout=10)


class Worker:
    """Any engine that speaks the JSON-lines worker protocol (see module docstring)."""

    def __init__(self, cmd: list[str], prompt: str):
        self.prompt = prompt
        t0 = time.perf_counter()
        self.proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        # Frameworks sometimes print to stdout without a newline (Core ML does), so
        # READY can arrive glued to the end of someone else's message.
        while True:
            line = self.proc.stdout.readline()
            if not line:
                sys.exit("worker exited before READY")
            if "READY" in line:
                info = json.loads(line.split("READY", 1)[1].strip() or "{}")
                break
        self.pid = info.get("pid", self.proc.pid)
        self.load_ms = info.get("load_ms", (time.perf_counter() - t0) * 1000)

    def transcribe(self, wav: Path) -> tuple[str, float]:
        self.proc.stdin.write(json.dumps({"wav": str(wav.resolve()), "prompt": self.prompt}) + "\n")
        self.proc.stdin.flush()
        while True:
            line = self.proc.stdout.readline()
            if not line:
                sys.exit("worker exited mid-clip")
            start = line.find('{"')
            if start < 0:
                continue
            try:
                reply = json.loads(line[start:])
            except json.JSONDecodeError:
                continue
            if "text" in reply:
                break
        if reply.get("error"):
            print(f"  worker error: {reply['error']}", file=sys.stderr)
        return " ".join(reply["text"].split()), reply["ms"]

    def close(self):
        try:
            self.proc.stdin.write("QUIT\n")
            self.proc.stdin.flush()
            self.proc.wait(timeout=20)
        except Exception:
            self.proc.kill()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("engine", choices=["whispercpp", "worker"])
    ap.add_argument("--name", required=True, help="results folder name")
    ap.add_argument("--model", help="model path (whispercpp)")
    ap.add_argument("--no-prompt", action="store_true", help="don't pass the glossary as a prompt")
    ap.add_argument("--notes", default="")
    ap.add_argument("--set", help="use sets/<name>/clips and sets/<name>/results")
    argv = sys.argv[1:]
    worker_cmd = argv[argv.index("--") + 1:] if "--" in argv else []
    a = ap.parse_args(argv[:argv.index("--")] if "--" in argv else argv)
    a.cmd = worker_cmd
    if a.set:
        use_set(a.set)

    clips = clip_list()
    if not clips:
        sys.exit("No clips with transcripts in clips/ yet.")
    prompt = "" if a.no_prompt else glossary_prompt()

    wired_before = wired_mb()
    if a.engine == "whispercpp":
        engine = WhisperCpp(a.model, prompt, [])
    else:
        if not a.cmd:
            sys.exit("worker needs a command after --")
        engine = Worker(a.cmd, prompt)

    out = RESULTS / a.name
    out.mkdir(parents=True, exist_ok=True)
    meta = {"engine": a.engine, "model": a.model, "cmd": a.cmd, "prompt": prompt, "notes": a.notes,
            "load_ms": round(engine.load_ms), "clips": {}}
    try:
        engine.transcribe(clips[0][1])  # warm-up, not timed
        for name, wav in clips:
            text, ms = engine.transcribe(wav)
            (out / f"{name}.txt").write_text(text + "\n")
            meta["clips"][name] = {"latency_ms": round(ms), "audio_s": round(audio_seconds(wav), 2)}
            print(f"{name:<28} {ms:>7.0f} ms  {text[:90]}", flush=True)
        now, peak = footprint_mb(engine.pid)
        meta["footprint_mb"], meta["peak_footprint_mb"] = now and round(now), peak and round(peak)
        meta["wired_delta_mb"] = round(wired_mb() - wired_before)
    finally:
        engine.close()
    (out / "meta.json").write_text(json.dumps(meta, indent=2))
    print(f"\n{a.name}: load {meta['load_ms']} ms, footprint {meta.get('footprint_mb')} MB "
          f"(peak {meta.get('peak_footprint_mb')} MB), system wired +{meta.get('wired_delta_mb')} MB. "
          f"Results in {out}")


if __name__ == "__main__":
    main()
