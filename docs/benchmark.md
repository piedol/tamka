# Speech model benchmark

Tamka's default model, Parakeet Ultra, was chosen with this benchmark, run on 2026-10-24. The test asked for a model that is at least as accurate as Whisper large-v3-turbo, a popular open model, while using less memory and returning text sooner.

**Result:** Parakeet Ultra on the Neural Engine uses about a third of Whisper's memory and is about 3x faster. It makes 23% fewer errors across all clips. On everyday speech it makes about 10% fewer, which is within the noise. Granite Speech 2B and Qwen3-ASR 1.7B are clearly more accurate on everyday speech, but they need 4.8 GB and 2.9 GB of memory.

## What was tested

117 clips, 32 minutes and 4,768 words from public datasets with human-checked transcripts. [`eval/fetch_public.py`](../eval/fetch_public.py) downloads and rebuilds the set.

| Group | Clips | Source | What it tests |
|---|---|---|---|
| ls-clean, ls-other | 33 + 7 long | LibriSpeech test-clean and test-other | Read speech, clean and harder |
| fleurs | 21 | FLEURS en_us | Read sentences with names and numbers; punctuated references |
| earnings | 24 + 6 long | Earnings-22 | Spontaneous business speech, jargon, figures and accents; punctuated references |
| ami | 21 + 5 long | AMI headset microphones | Spontaneous meeting speech, near-field microphone |

"Long" clips are 43–65 seconds, stitched from consecutive segments, to match a typical dictation of about 50 seconds.

Every model also received a six-word custom vocabulary (product names such as Zapier, and a surname) as its prompt or hotwords. None of those words occur in the references, so the test also catches models that insert vocabulary words that were never said. That list was private; [`eval/glossary.txt`](../eval/glossary.txt) is an example, so a rerun will give slightly different numbers.

Hardware: MacBook Pro with M5 Max, macOS 26.6. Latency is measured from the end of the audio to the final text. The finalists were timed again on an idle machine and the numbers matched.

## Results

| Model | WER % | WER without meetings | Formatting errors % | Short clip | 50 s clip | Memory |
|---|---|---|---|---|---|---|
| Granite Speech 5.0 2B + keywords | 5.45 | 4.63 | 18.97 | 268 ms | 1,752 ms | 4.8 GB, 13.4 GB peak |
| Granite Speech 5.0 2B | 5.96 | 5.15 | 15.32 | 295 ms | 1,733 ms | 4.8 GB, 13.4 GB peak |
| Qwen3-ASR 1.7B (8-bit) | 6.54 | 5.89 | 17.53 | 163 ms | 882 ms | 2.9 GB, 4.8 GB peak |
| Parakeet Phonon-2 | 7.26 | 6.64 | 22.29 | 43 ms | 172 ms | about 0.5 GB |
| Qwen3-ASR 0.6B (8-bit) | 7.26 | 6.42 | 18.30 | 98 ms | 501 ms | 1.4 GB, 3.5 GB peak |
| Parakeet v3 | 7.38 | 6.36 | 21.52 | 46 ms | 173 ms | about 0.5 GB |
| Parakeet v2 | 7.57 | 7.08 | 21.37 | 41 ms | 126 ms | about 0.35 GB |
| **Parakeet Ultra** (Tamka's default) | 7.66 | **6.20** | **18.16** | 45 ms | 170 ms | **about 0.5 GB** |
| Apple SpeechTranscriber | 8.58 | 7.27 | 19.02 | 119 ms | 682 ms | about 0.3 GB (system) |
| Whisper large-v3-turbo q8 (baseline) | 9.94 | 6.89 | 18.83 | 120 ms | 471 ms | 1.2–1.6 GB |
| Apple DictationTranscriber | 15.00 | 13.58 | 28.24 | 203 ms | 1,552 ms | about 0.7 GB (system) |

- WER is the word error rate, ignoring case and punctuation. Lower is better.
- Formatting errors is the same measure counting punctuation and capitals. It is scored only on FLEURS and Earnings-22, whose references are punctuated.
- Memory for Parakeet is the app's own memory (100–150 MB) plus the Neural Engine weights the system holds (240–370 MB, measured as the change in wired memory).
- Parakeet with FluidAudio's vocabulary boosting scored 7.80–7.82, worse than without it, and isn't shown.

### Confidence

Bootstrap over clips, 95% intervals:

| Fewer errors than Whisper | All clips | Without meetings |
|---|---|---|
| Granite 2B + keywords | 45% (36 to 54) | 33% (22 to 44) |
| Qwen3-ASR 1.7B | 34% (20 to 47) | 14% (1 to 28) |
| Parakeet Phonon-2 | 27% (13 to 39) | 4% (−10 to 17) |
| Parakeet Ultra | 23% (12 to 33) | 10% (−3 to 23) |
| Qwen3-ASR 0.6B | – | 7% (−9 to 22) |
| Apple SpeechTranscriber | 14% (−3 to 29) | −6% (−25 to 10) |

The Parakeet variants are within the noise of each other.

## What the numbers hide

- The meeting clips favour Parakeet. AMI's headset microphones pick up neighbours talking. Whisper (31% WER on short AMI clips) often transcribed the neighbour and invented whole sentences, and Parakeet Ultra did the same on short clips (23%). Whisper also drops stutters ("I had to, I had to"), which the verbatim references count as errors but which a dictation user would want removed. Without AMI, Parakeet's lead over Whisper is within the noise.
- Whisper repeats its prompt. Given the custom vocabulary as a prompt, Whisper inserted those words at the start of clips that never contained them. Qwen3-ASR's hotwords and Parakeet produced no false vocabulary words.
- FluidAudio's vocabulary boosting misfires on common words, even with its rescue pass off: "earlier" became "Zapier", and other everyday words turned into product names from the list. For anyone who dictates everyday words all day, that rules it out. Tamka uses find-and-replace rules for names instead.
- Parakeet writes verbatim. It keeps "uh", "um" and repeats ("at the at the"), sometimes spells numbers out ("seventy percent to one point one seven"), and once invented a currency sign. Qwen3-ASR and Whisper drop fillers and write digits. Tamka runs a cleanup step after Parakeet that removes fillers and collapses immediate repeats, followed by its spoken-command rules.
- FluidAudio's number formatter is not safe as a default. It fixed money and dates ("$1,250.", "January 5, 2025") but also wrote "1 of the things", turned "1.17 billion" into "1.17 1000000000", and dropped the "and" from "one question and two ideas". Tamka keeps it behind an experimental setting.
- Qwen3-ASR gives the cleanest text (no fillers, digits, good punctuation), and its accuracy lead is real. The cost is memory: 2.9 GB resident and 4.8 GB at peak in MLX.
- Granite is the most accurate but impractical at 4.8 GB (13 GB at peak on long clips). With keywords, it often drops punctuation and capitals entirely.
- Apple's models use almost no memory in the app and are about as accurate as Whisper, but they have no working vocabulary support and are slow on long clips (0.7–1.5 seconds).

## Choosing a model

| If you care most about | Pick | Trade-off |
|---|---|---|
| Low memory with at least Whisper's accuracy | **Parakeet Ultra** | Needs filler cleanup (built in); names rely on find-and-replace |
| The highest accuracy at a practical size | Qwen3-ASR 1.7B | 2–3 GB of memory; about 2x slower than Whisper on long clips |
| Casual, unscripted speech | Parakeet Phonon-2 | The weakest punctuation of the Parakeet models |

Tamka offers the four Parakeet models and Apple Speech. To test models on your own voice, see [eval/README.md](../eval/README.md).
