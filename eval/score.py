# /// script
# requires-python = ">=3.11"
# dependencies = ["jiwer>=3.0", "whisper-normalizer>=0.0.10"]
# ///
"""Score every model in results/ against the reference transcripts in clips/.

    uv run score.py                 your own clips (clips/, results/)
    uv run score.py --set public    sets/public/clips and sets/public/results
    uv run score.py --clips         add a per-clip breakdown

Clip names may carry a group prefix, "group__id" (e.g. "earnings__4483589-483").
Scores are then also reported per group.

results/<model>/<clip>.txt   hypothesis text for each clip
results/<model>/meta.json    timings and memory written by run.py
"""

import json
import re
import sys
import warnings
from pathlib import Path

warnings.filterwarnings("ignore", category=SyntaxWarning)

import jiwer  # noqa: E402
from whisper_normalizer.english import EnglishTextNormalizer  # noqa: E402

HERE = Path(__file__).parent
normalize = EnglishTextNormalizer()


def words_plain(text: str) -> str:
    """Case, punctuation and filler-insensitive form (Open ASR Leaderboard style)."""
    return normalize(text)


def words_formatted(text: str) -> str:
    """Keep case and punctuation: each punctuation mark becomes its own token."""
    text = (text.replace("“", '"').replace("”", '"').replace("‘", "'").replace("’", "'")
            .replace("…", "...").replace("—", " - ").replace("–", "-"))
    text = re.sub(r"\b(um+|uh+|erm|hmm+)\b[,.]?", " ", text, flags=re.I)
    text = " ".join(text.split())
    text = re.sub(r"^([a-z])", lambda m: m.group(1).upper(), text)  # removed filler may leave a lowercase start
    text = re.sub(r"([.,!?;:()\[\]{}\"'])", r" \1 ", text)
    return " ".join(text.split())


def wer(refs: list[str], hyps: list[str]) -> float | None:
    pairs = [(r, h) for r, h in zip(refs, hyps) if r.strip()]
    if not pairs:
        return None
    r, h = zip(*pairs)
    return jiwer.wer(list(r), list(h)) * 100


def group_of(clip: str) -> str:
    return clip.split("__", 1)[0] if "__" in clip else "all"


def is_formatted(texts: list[str]) -> bool:
    """References with lowercase letters were written with real casing and punctuation."""
    return any(re.search(r"[a-z]", t) for t in texts)


def cell(v, spec: str) -> str:
    width = int(re.match(r">(\d+)", spec).group(1))
    return f"{'—':>{width}}" if v is None else format(v, spec)


def main() -> None:
    args = sys.argv[1:]
    base = HERE / "sets" / args[args.index("--set") + 1] if "--set" in args else HERE
    clips_dir, results_dir = base / "clips", base / "results"

    refs = {t.stem: t.read_text().strip() for t in sorted(clips_dir.glob("*.txt")) if t.read_text().strip()}
    if not refs:
        sys.exit(f"No transcripts in {clips_dir} yet.")
    models = sorted(p for p in results_dir.iterdir() if p.is_dir()) if results_dir.exists() else []
    if not models:
        sys.exit("No results yet. Run a model first.")

    groups: dict[str, list[str]] = {}
    for n in refs:
        groups.setdefault(group_of(n), []).append(n)
    formatted_groups = {g for g, names in groups.items() if is_formatted([refs[n] for n in names])}

    rows, per_group, per_clip = [], {}, {}
    for mdir in models:
        meta = json.loads((mdir / "meta.json").read_text()) if (mdir / "meta.json").exists() else {}
        hyps = {n: (mdir / f"{n}.txt").read_text().strip() for n in refs if (mdir / f"{n}.txt").exists()}
        if not hyps:
            continue
        names = list(hyps)
        fmt_names = [n for n in names if group_of(n) in formatted_groups]
        cm = meta.get("clips", {})
        short = sorted(cm[n]["latency_ms"] for n in names if n in cm and cm[n]["audio_s"] < 15)
        long_ = sorted(cm[n]["latency_ms"] for n in names if n in cm and cm[n]["audio_s"] >= 30)
        rows.append({
            "model": mdir.name,
            "clips": f"{len(names)}/{len(refs)}",
            "wer": wer([words_plain(refs[n]) for n in names], [words_plain(hyps[n]) for n in names]),
            "fmt": wer([words_formatted(refs[n]) for n in fmt_names], [words_formatted(hyps[n]) for n in fmt_names]),
            "short_ms": short[len(short) // 2] if short else None,
            "long_ms": long_[len(long_) // 2] if long_ else None,
            "peak_mb": meta.get("peak_footprint_mb"),
            "wired_mb": meta.get("wired_delta_mb"),
            "load_ms": meta.get("load_ms"),
        })
        for g, gnames in groups.items():
            have = [n for n in gnames if n in hyps]
            if have:
                per_group.setdefault(g, {})[mdir.name] = wer(
                    [words_plain(refs[n]) for n in have], [words_plain(hyps[n]) for n in have])
                if g in formatted_groups:
                    per_group.setdefault(g + " (fmt)", {})[mdir.name] = wer(
                        [words_formatted(refs[n]) for n in have], [words_formatted(hyps[n]) for n in have])
        for n in names:
            per_clip.setdefault(n, {})[mdir.name] = wer([words_plain(refs[n])], [words_plain(hyps[n])])

    rows.sort(key=lambda r: r["wer"])
    print(f"{'model':<26} {'clips':>8} {'WER%':>6} {'fmtWER%':>8} {'<15s ms':>8} {'≥30s ms':>8} "
          f"{'peak MB':>8} {'+wired':>7} {'load ms':>8}")
    for r in rows:
        print(f"{r['model']:<26} {r['clips']:>8} {r['wer']:>6.2f} {cell(r['fmt'], '>8.2f')} "
              f"{cell(r['short_ms'], '>8.0f')} {cell(r['long_ms'], '>8.0f')} {cell(r['peak_mb'], '>8.0f')} "
              f"{cell(r['wired_mb'], '>7.0f')} {cell(r['load_ms'], '>8.0f')}")
    print("\nWER% ignores case and punctuation. fmtWER% counts them (only on sets whose references are formatted).")
    print("<15s / ≥30s ms = median time from end of audio to text, for short and long clips.")
    print("peak MB = the engine's own memory. +wired = extra system memory while loaded: where Neural Engine "
          "models (parakeet-*) keep weights, so add it for those; for GPU models it overlaps peak MB.")

    order = [r["model"] for r in rows]
    if len(per_group) > 1:
        short_names = [m[:13] for m in order]
        print(f"\nWER% by group\n{'group':<22} {'clips':>5} " + " ".join(f"{m:>13}" for m in short_names))
        for g in sorted(per_group):
            count = len(groups.get(g.replace(" (fmt)", ""), []))
            print(f"{g:<22} {count:>5} " + " ".join(cell(per_group[g].get(m), '>13.2f') for m in order))

    if "--clips" in args:
        print(f"\nPer-clip WER%\n{'clip':<34} " + " ".join(f"{m[:13]:>13}" for m in order))
        for clip, scores in per_clip.items():
            print(f"{clip:<34} " + " ".join(cell(scores.get(m), '>13.1f') for m in order))


if __name__ == "__main__":
    main()
