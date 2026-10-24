# /// script
# requires-python = ">=3.11"
# dependencies = ["mlx-audio==0.6.1", "jinja2"]
# ///
"""MLX benchmark worker (GPU) for speech models that need an LLM-style decoder.

    uv run engines/mlx_worker.py qwen3 mlx-community/Qwen3-ASR-1.7B-8bit
    uv run engines/mlx_worker.py granite ibm-granite/granite-speech-5.0-2b [--keywords]

Speaks the JSON-lines protocol in eval/run.py. The glossary ("prompt") goes in as
Qwen3-ASR hotwords, or as a Granite keyword list when --keywords is set.
"""

import json
import os
import sys
import time

from mlx_audio.stt import load

GRANITE_PUNCT = "transcribe the speech with proper punctuation and capitalization."
GRANITE_KEYWORDS = "transcribe the speech with proper punctuation and capitalization. Keywords: {}"


def main() -> None:
    family, repo = sys.argv[1], sys.argv[2]
    use_keywords = "--keywords" in sys.argv
    t0 = time.perf_counter()
    model = load(repo)
    print("READY " + json.dumps({"pid": os.getpid(), "load_ms": round((time.perf_counter() - t0) * 1000)}), flush=True)

    for line in sys.stdin:
        line = line.strip()
        if line == "QUIT":
            break
        if not line:
            continue
        req = json.loads(line)
        words = [w.strip() for w in req.get("prompt", "").split(",") if w.strip()]
        start = time.perf_counter()
        try:
            if family == "qwen3":
                out = model.generate(req["wav"], language="English", hotwords=words or None)
            else:
                prompt = GRANITE_KEYWORDS.format(", ".join(words)) if use_keywords and words else GRANITE_PUNCT
                out = model.generate(req["wav"], prompt=prompt)
            text, err = out.text, None
        except Exception as e:  # report and keep going
            text, err = "", repr(e)
        reply = {"text": text, "ms": (time.perf_counter() - start) * 1000}
        if err:
            reply["error"] = err
        print(json.dumps(reply), flush=True)


if __name__ == "__main__":
    main()
