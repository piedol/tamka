# /// script
# requires-python = ">=3.11"
# dependencies = ["pyarrow>=17", "numpy>=2"]
# ///
"""Build sets/public: speech clips with human-checked transcripts from public datasets.

    uv run fetch_public.py

Downloads one parquet slice of each dataset into sets/public/.download/, then writes
sets/public/clips/<group>__<id>.wav + .txt and sets/public/manifest.json.

Groups (clip counts are targets):
  ls-clean    LibriSpeech test-clean: clean read speech            (CC-BY-4.0)
  ls-other    LibriSpeech test-other: harder read speech            (CC-BY-4.0)
  fleurs      FLEURS en_us test: read sentences, punctuated refs    (CC-BY-4.0)
  earnings    Earnings-22: earnings calls, jargon, numbers, accents (CC-BY-SA-4.0)
  ami         AMI headset mics: spontaneous meeting speech          (CC-BY-4.0)
  *-long      45–90 s clips stitched from consecutive segments, like a long dictation
"""

import io
import json
import random
import re
import subprocess
import urllib.request
import wave
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq

HERE = Path(__file__).parent
BASE = HERE / "sets" / "public"
DL = BASE / ".download"
CLIPS = BASE / "clips"
SR = 16000
GAP = np.zeros(int(0.35 * SR), dtype=np.int16)
rng = random.Random(20261024)
manifest: list[dict] = []


def parquet_urls(dataset: str, config: str, split: str) -> list[str]:
    url = f"https://datasets-server.huggingface.co/parquet?dataset={dataset}"
    data = json.load(urllib.request.urlopen(url, timeout=60))
    return [f["url"] for f in data["parquet_files"] if f["config"] == config and f["split"] == split]


def download(url: str, name: str) -> Path:
    DL.mkdir(parents=True, exist_ok=True)
    dst = DL / name
    if not dst.exists():
        print(f"  downloading {name}")
        subprocess.run(["curl", "-sSL", "--fail", "-C", "-", "-o", str(dst) + ".part", url], check=True)
        Path(str(dst) + ".part").rename(dst)
    return dst


def decode(audio_bytes: bytes) -> np.ndarray:
    pcm = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-i", "pipe:0",
                          "-f", "s16le", "-ac", "1", "-ar", str(SR), "pipe:1"],
                         input=audio_bytes, capture_output=True, check=True).stdout
    return np.frombuffer(pcm, dtype=np.int16)


def write_clip(group: str, cid: str, pieces: list[np.ndarray], text: str, source: dict) -> None:
    CLIPS.mkdir(parents=True, exist_ok=True)
    audio = pieces[0] if len(pieces) == 1 else np.concatenate([x for p in pieces for x in (p, GAP)][:-1])
    name = f"{group}__{cid}"
    with wave.open(str(CLIPS / f"{name}.wav"), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(audio.tobytes())
    (CLIPS / f"{name}.txt").write_text(" ".join(text.split()) + "\n")
    manifest.append({"clip": name, "group": group, "seconds": round(len(audio) / SR, 2), **source})


def stratified(cands: list[dict], n: int) -> list[dict]:
    """Pick n candidates spread evenly over short (<5 s), medium (5–15 s) and long (15–30 s)."""
    buckets = [[c for c in cands if lo <= c["dur"] < hi] for lo, hi in ((1, 5), (5, 15), (15, 30))]
    picks, per = [], n // 3
    for b in buckets:
        rng.shuffle(b)
        picks += b[:per]
    rest = [c for c in cands if c not in picks]
    rng.shuffle(rest)
    return picks + rest[: n - len(picks)]


def runs(items: list[dict], same, target=(45, 90), max_gap=None) -> list[list[dict]]:
    """Group consecutive items into runs of target length (seconds)."""
    out, cur = [], []
    for it in items:
        if cur and (not same(cur[-1], it) or (max_gap is not None and it["start"] - cur[-1]["end"] > max_gap)):
            cur = []
        cur.append(it)
        total = sum(c["dur"] for c in cur)
        if total >= target[0]:
            if total <= target[1]:
                out.append(cur)
            cur = []
    return out


# ---------------------------------------------------------------- LibriSpeech
def librispeech(config: str, group: str, n_single: int, n_long: int) -> None:
    path = download(parquet_urls("openslr/librispeech_asr", config, "test")[0], f"librispeech-{config}.parquet")
    t = pq.read_table(path, columns=["id", "text", "audio", "speaker_id", "chapter_id"]).to_pylist()
    rows = sorted(t, key=lambda r: r["id"])
    sample = rng.sample(rows, 160)
    for r in sample:
        r["pcm"] = decode(r["audio"]["bytes"])
        r["dur"] = len(r["pcm"]) / SR
    for r in stratified(sample, n_single):
        write_clip(group, r["id"], [r["pcm"]], r["text"], {"source": f"LibriSpeech test-{config}", "ids": [r["id"]]})
    chapters = sorted({r["chapter_id"] for r in rows})
    rng.shuffle(chapters)
    made = 0
    for ch in chapters:
        if made == n_long:
            break
        chap = [r for r in rows if r["chapter_id"] == ch]
        for r in chap:
            r["pcm"] = decode(r["audio"]["bytes"])
            r["dur"] = len(r["pcm"]) / SR
        found = runs(chap, lambda a, b: True)
        if found:
            seg = found[0]
            write_clip(f"{group}-long", f"{ch}-{seg[0]['id'].split('-')[-1]}", [s["pcm"] for s in seg],
                       " ".join(s["text"] for s in seg),
                       {"source": f"LibriSpeech test-{config}", "ids": [s["id"] for s in seg]})
            made += 1


# ---------------------------------------------------------------------- FLEURS
def fleurs(n: int) -> None:
    path = download(parquet_urls("google/fleurs", "en_us", "test")[0], "fleurs-en_us.parquet")
    cols = [c for c in pq.read_schema(path).names if c in ("id", "raw_transcription", "audio", "num_samples")]
    t = pq.read_table(path, columns=cols).to_pylist()
    seen, cands = set(), []
    for r in t:
        # FLEURS has several readings of each sentence; keep one per sentence.
        if r["raw_transcription"] in seen:
            continue
        seen.add(r["raw_transcription"])
        cands.append(r)
    sample = rng.sample(cands, 90)
    for r in sample:
        r["pcm"] = decode(r["audio"]["bytes"])
        r["dur"] = len(r["pcm"]) / SR
    for r in stratified(sample, n):
        write_clip("fleurs", str(r["id"]), [r["pcm"]], r["raw_transcription"],
                   {"source": "FLEURS en_us test", "ids": [r["id"]]})


# ------------------------------------------------------------------ Earnings-22
def earnings(n_single: int, n_long: int, shards: int = 2) -> None:
    urls = parquet_urls("distil-whisper/earnings22", "chunked", "test")
    rows = []
    for i, url in enumerate(urls[:shards]):
        path = download(url, f"earnings22-{i}.parquet")
        rows += pq.read_table(path, columns=["file_id", "segment_id", "transcription", "audio",
                                             "start_ts", "end_ts"]).to_pylist()
    for r in rows:
        r["dur"], r["start"], r["end"] = r["end_ts"] - r["start_ts"], r["start_ts"], r["end_ts"]
        r["seg"] = int(r["segment_id"])
    clean = [r for r in rows if "<" not in r["transcription"] and "[" not in r["transcription"] and r["dur"] >= 1]
    for r in stratified(clean, n_single):
        write_clip("earnings", f"{r['file_id']}-{r['seg']}", [decode(r["audio"]["bytes"])], r["transcription"],
                   {"source": "Earnings-22", "ids": [f"{r['file_id']}-{r['seg']}"]})
    by_file: dict[str, list[dict]] = {}
    for r in sorted(rows, key=lambda r: (r["file_id"], r["seg"])):
        by_file.setdefault(r["file_id"], []).append(r)
    long_runs = []
    for segs in by_file.values():
        ok = [s for s in segs if "<" not in s["transcription"] and "[" not in s["transcription"]]
        long_runs += runs(ok, lambda a, b: b["seg"] == a["seg"] + 1, max_gap=2.0)
    rng.shuffle(long_runs)
    for seg in long_runs[:n_long]:
        write_clip("earnings-long", f"{seg[0]['file_id']}-{seg[0]['seg']}",
                   [decode(s["audio"]["bytes"]) for s in seg], " ".join(s["transcription"] for s in seg),
                   {"source": "Earnings-22", "ids": [f"{s['file_id']}-{s['seg']}" for s in seg]})


# ------------------------------------------------------------------------- AMI
def ami(n_single: int, n_long: int) -> None:
    path = download(parquet_urls("edinburghcstr/ami", "ihm", "test")[0], "ami-ihm-test-0.parquet")
    rows = pq.read_table(path, columns=["meeting_id", "audio_id", "text", "audio", "begin_time",
                                        "end_time", "speaker_id"]).to_pylist()
    for r in rows:
        r["dur"], r["start"], r["end"] = r["end_time"] - r["begin_time"], r["begin_time"], r["end_time"]
    # Skip back-channel ("YEAH", "MM-HMM"): fewer than 4 words says little about accuracy.
    cands = [r for r in rows if r["dur"] >= 1 and len(r["text"].split()) >= 4]
    for r in stratified(cands, n_single):
        write_clip("ami", r["audio_id"].removeprefix("AMI_"), [decode(r["audio"]["bytes"])], r["text"],
                   {"source": "AMI IHM test", "ids": [r["audio_id"]]})
    long_runs = []
    for spk in sorted({(r["meeting_id"], r["speaker_id"]) for r in rows}):
        segs = sorted([r for r in cands if (r["meeting_id"], r["speaker_id"]) == spk], key=lambda r: r["start"])
        long_runs += runs(segs, lambda a, b: True, target=(40, 80), max_gap=20.0)
    rng.shuffle(long_runs)
    for seg in long_runs[:n_long]:
        write_clip("ami-long", seg[0]["audio_id"].removeprefix("AMI_"), [decode(s["audio"]["bytes"]) for s in seg],
                   " ".join(s["text"] for s in seg), {"source": "AMI IHM test", "ids": [s["audio_id"] for s in seg]})


def main() -> None:
    if CLIPS.exists():
        for f in CLIPS.iterdir():
            f.unlink()
    print("LibriSpeech clean"); librispeech("clean", "ls-clean", 18, 4)
    print("LibriSpeech other"); librispeech("other", "ls-other", 15, 3)
    print("FLEURS"); fleurs(21)
    print("Earnings-22"); earnings(24, 6)
    print("AMI"); ami(21, 5)
    (BASE / "manifest.json").write_text(json.dumps(manifest, indent=1))
    total = sum(m["seconds"] for m in manifest)
    print(f"\n{len(manifest)} clips, {total / 60:.1f} minutes of audio in {CLIPS}")
    for g in sorted({m["group"] for m in manifest}):
        ms = [m for m in manifest if m["group"] == g]
        print(f"  {g:<15} {len(ms):>3} clips, {min(m['seconds'] for m in ms):5.1f}–{max(m['seconds'] for m in ms):5.1f} s")


if __name__ == "__main__":
    main()
