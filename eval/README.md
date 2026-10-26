# Speech model evaluation

These scripts compare speech models on the same clips. Each model transcribes every clip and is scored on accuracy (word error rate against a reference transcript), speed (time from the end of the audio to the text) and memory. Tamka's default model was chosen this way; the results are in [docs/benchmark.md](../docs/benchmark.md).

You can run the public clip set, or record your own clips to see which model handles your voice, microphone and vocabulary best.

## Requirements

- A Mac with Apple silicon
- [uv](https://docs.astral.sh/uv/) for the Python scripts
- Xcode 26 or later, to build Tamka's speech engine. `bench.sh` builds it on first run.
- For the Whisper baseline, whisper.cpp's `whisper-server` (for example `brew install whisper-cpp`). `bench.sh` downloads the model on first use.
- Qwen3-ASR and Granite run through MLX; uv installs what they need.

## The public clip set

117 clips from LibriSpeech, FLEURS, Earnings-22 and AMI, with human-checked transcripts. The audio isn't stored in the repository; `fetch_public.py` downloads it and cuts the clips.

```bash
cd eval
uv run fetch_public.py
SET=public ./bench.sh
```

## Your own clips

About 25 clips and 15–20 minutes in total gives a useful result. Most dictations are 20–90 seconds long, so most clips should be too. Speak the way you normally dictate, in your usual room and with your usual microphone.

A good mix:

| Clips | Kind | Example |
|---|---|---|
| 6 | Instructions full of jargon | File paths, library and product names |
| 4 | Messages | Names of people and companies |
| 3 | Emails | A greeting, a few paragraphs and a sign-off |
| 2 | Numbers | Dates, times, money and versions ("version 2.1.308", "$1,250 by October 24th") |
| 2 | Spoken punctuation | "open paren", "new line" |
| 2 | Corrections and filler | "um", "actually no, make that Thursday" |
| 2 | Fast speech | Your quickest natural pace |
| 2 | Hard audio | A quiet voice, a fan or music in the background |
| 2 | Very short | 2–5 seconds: "yes go ahead" |
| 1 | Long | 2–3 minutes in one go |

### Recording

Each clip is two files with the same name in `eval/clips/`:

```text
clips/07-client-email.wav   the audio (wav, m4a, mp3 and others are converted)
clips/07-client-email.txt   exactly what you said
```

The recorder script records from the default microphone until you press `q`, then opens the transcript file in TextEdit:

```bash
cd eval && ./record.sh 07-client-email
```

To record from another microphone, set `MIC` to its name: `MIC="AirPods Pro" ./record.sh 08-headset`. The first run asks for microphone access for your terminal app. Recordings from Voice Memos or QuickTime work too: put the file in `clips/` and add the matching `.txt`.

`clips/` is git-ignored, so your recordings never end up in a repository.

### Writing the transcripts

- Write the words you actually said, the way you'd want them typed: normal capitals, punctuation and numbers.
- For a spoken command such as "open paren", write the words, not the symbol. Tamka's command rules run after the model and are tested separately.
- Filler words ("um", "uh") are optional; the scorer ignores them.
- Spell names and jargon correctly. Those are the words models get wrong most often.

### Your vocabulary

Models that accept a prompt or hotwords get the words in `glossary.txt`, one per line. Put your own in `glossary.local.txt`, which is git-ignored and used instead when it exists.

## Running

```bash
./bench.sh                                   # every contender over clips/, then the scores
./bench.sh parakeet-ultra qwen3-asr-1.7b     # only these models
uv run score.py --clips                      # print the scores again, clip by clip
```

The scorer reports plain word accuracy (case and punctuation ignored) and a formatting score (whether punctuation and capitals match your transcript). Model outputs go to `results/` (or `sets/public/results/`), which are git-ignored.
