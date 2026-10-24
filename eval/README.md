# Tamka speech model evaluation

These clips decide which speech model Tamka ships with. Every candidate model transcribes every clip. Each one is scored on accuracy (word error rate against your transcript), speed (time from end of audio to text), and memory.

## What to record

Aim for about **25 clips, 15–20 minutes in total**. A typical dictation is about 50 seconds, so most clips should be 20–90 seconds long. Talk the way you normally dictate. Don't slow down or over-enunciate.

| Count | Kind | Example |
|---|---|---|
| 6 | Prompts to AI coding assistants | Coding instructions with crate names, file paths, tools (Rust, objc2, Docker, Kubernetes) |
| 4 | Client or Discord messages | Client and company names, people's names |
| 3 | Emails | A normal email with a greeting and sign-off |
| 2 | Numbers | Dates, times, money, versions, phone numbers ("version 2.1.308", "$1,250 by October 24th") |
| 2 | Spoken punctuation | "open paren", "new line", the commands from your find/replace list |
| 2 | Self-corrections and filler | "um", "actually no, make that Thursday" |
| 2 | Fast speech | Your quickest natural pace |
| 2 | Hard audio | Quiet voice, a fan or music in the background, or across the room |
| 2 | Very short | 2–5 seconds: "yes go ahead", "commit and push" |
| 1 | Long | 2–3 minutes in one go |

Use the microphone and room you normally dictate in. If you also dictate with another mic (headset, AirPods), record a few clips with it too.

## How to record

Each clip is two files with the same name in `clips/`:

```text
clips/07-client-email.wav   the audio (wav, m4a, mp3 and others are fine; they get converted)
clips/07-client-email.txt   exactly what you said
```

The easy way is the recorder script. It records from your default mic, and you press `q` to stop. It then opens the matching transcript file in TextEdit:

```bash
cd "eval" && ./record.sh 07-client-email
```

To use a different mic, set `MIC` to its name: `MIC="AirPods Pro" ./record.sh 08-headset-test`. The first run asks for microphone access for your terminal app.

Voice Memos or QuickTime recordings also work: drop the file into `clips/` and add the matching `.txt`.

## How to write the transcripts

- Write **the words you actually said**, in the form you'd want pasted: normal capitalization, punctuation, and numbers written the way you'd type them.
- If you said a spoken command like "open paren", write the words ("open paren"), not the symbol. Your find/replace rules run after the model, so they're tested separately.
- Filler words ("um", "uh") are optional; the scorer ignores them.
- Spell names and jargon correctly. Those are exactly the words the models get wrong.

The scorer reports two numbers: plain word accuracy (case and punctuation ignored), and a formatting score (whether the punctuation and capitalization match your text).

Clips stay on this Mac. `clips/` is git-ignored so your voice never ends up in a repository.

## Running the benchmark

```bash
./bench.sh                         # every contender over clips/, then the scoreboard
./bench.sh parakeet-ultra qwen3-asr-1.7b   # just these
uv run score.py --clips            # re-print scores with a per-clip breakdown
```

## Public clip set

`sets/public/` holds 117 clips from LibriSpeech, FLEURS, Earnings-22 and AMI, with human-checked transcripts. Rebuild and run it with:

```bash
uv run fetch_public.py
SET=public ./bench.sh
```

Results and the recommendation are in [docs/benchmark-public.md](../docs/benchmark-public.md).
