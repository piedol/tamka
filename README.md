<p align="center">
  <img src="assets/logo.png" width="128" alt="Tamka icon">
</p>

<h1 align="center">Tamka</h1>

<p align="center">
  Private, on-device dictation for the Mac.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-26%2B-1c1c1e?logo=apple" alt="macOS 26 or later">
  <img src="https://img.shields.io/badge/Apple%20silicon-required-1c1c1e" alt="Apple silicon required">
  <a href="LICENSE.md"><img src="https://img.shields.io/badge/license-PolyForm%20Noncommercial-1c1c1e" alt="License: PolyForm Noncommercial 1.0.0"></a>
</p>

<p align="center">
  <img src="docs/images/showcase.png" width="880" alt="Tamka's Models settings, its menu with recent dictations, and the listening indicator">
</p>

Press a shortcut, speak, and press it again: Tamka types what you said wherever your cursor is. Speech recognition runs on your Mac's Neural Engine with NVIDIA's Parakeet models, so your voice never leaves the computer, and text appears about 0.2 seconds after you stop, even after 50 seconds of speech.

Tamka removes "um" and "uh", collapses stutters, and turns spoken commands such as "comma", "new line" or "open paren" into the real thing. It lives in the menu bar and uses about 80 MB for the app and its speech engine, plus the model you choose.

<p align="center"><sub><em>Tamka</em> (TAHM-kah) is Swahili for "to pronounce" or "to utter".</sub></p>

## Contents

- [Dictating](#dictating)
- [Features](#features)
- [Privacy](#privacy)
- [Accuracy and speed](#accuracy-and-speed)
- [Install](#install)
- [Troubleshooting](#troubleshooting)
- [Development](#development)
- [Credits](#credits) · [License](#license)

## Dictating

1. Click where the text should go: an email, a chat, a code editor or a search box.
2. Press **⌥V** and speak. A small indicator shows that Tamka is listening, and for how long.
3. Press **⌥V** again. Tamka types what you said at the cursor and keeps a copy on the clipboard.

Press **Esc** while Tamka is listening to throw the recording away.

<p align="center">
  <img src="docs/images/listening.png" width="326" alt="The indicator while listening: a red dot, a timer at 0:12 and the shortcut hints">
  &nbsp;&nbsp;
  <img src="docs/images/transcribing.png" width="147" alt="The indicator while transcribing">
</p>

⌥V is the default shortcut. You can pick any combination that includes ⌘, ⌃ or ⌥ in **Settings › General**.

## Features

### An indicator you can move

The indicator never takes focus from the app you're typing in. Drag it anywhere, including onto another display, and Tamka puts it in the same place for every dictation after that. If that display is disconnected, the indicator returns to the bottom centre of the main screen. **Settings › General › Indicator › Reset Position** moves it back there yourself.

### Recent dictations

The menu bar menu shows your last five dictations. Click one to copy it again.

<p align="center">
  <img src="docs/images/menu.png" width="352" alt="The Tamka menu with three recent dictations, the active model and Settings"><br>
  <sub>Example dictations</sub>
</p>

- **Start Dictation** becomes **Stop and Paste** while you're speaking.
- **Transcribe Last Recording Again** runs your last recording through the current model, which is useful after switching models.
- Problems that need attention, such as a missing permission or a model that hasn't been downloaded, appear in the menu with a button to fix them.
- The menu bar icon turns red while Tamka is listening.

Recent dictations are kept in memory only and are gone when Tamka quits.

### Speech models with letter grades

Tamka doesn't include a speech model, which keeps the app small. On first launch, **Settings › Models** opens with one model marked **Recommended**. Each model downloads once and then works offline.

<p align="center">
  <img src="docs/images/settings-models.png" width="520" alt="Settings, Models tab: five models graded on size, speed and accuracy">
</p>

| Model | Size | Speed | Accuracy | Best for |
|---|:-:|:-:|:-:|---|
| **Parakeet Ultra** (recommended) | B | A | A | Everyday dictation; the most accurate, with the best punctuation |
| Parakeet Phonon-2 | A | A | A− | Casual, unscripted speech |
| Parakeet v3 | A | A | A− | Speaking any of 25 European languages |
| Parakeet v2 | A | A+ | B+ | The fastest results |
| Apple Speech | A+ | B | B | Nothing extra to download |

Size covers the download and the memory the model uses while loaded. Speed is how soon text appears after you stop speaking. Accuracy is how few words the model gets wrong. Hover over a grade in the app to see the measurements behind it. Removed models go to the Trash.

### Text cleanup and spoken commands

Tamka tidies the text before typing it. Say a command and it types the symbol with the spacing fixed: "call foo open paren bar close paren" becomes `call foo (bar)`.

| Say | Get | Say | Get |
|---|:-:|---|:-:|
| "comma" | , | "new line" | a line break |
| "question mark" | ? | "new paragraph" | a blank line |
| "open quote" … "close quote" | " … " | "open paren" … "close paren" | ( … ) |
| "at sign" | @ | "em dash" | — |
| "percent sign" | % | "smiley face" | :) |

These are 10 of the 112 commands Tamka starts with.

<p align="center">
  <img src="docs/images/settings-text.png" width="520" alt="Settings, Text tab: cleanup options and a searchable table of spoken commands">
</p>

In **Settings › Text** you can turn filler removal and stutter cleanup on or off, write numbers, prices and dates as digits (experimental), and search, add, edit or remove commands. ⌘Z undoes changes, and **Restore Defaults…** brings back the original list.

### Settings

<p align="center">
  <img src="docs/images/settings-general.png" width="520" alt="Settings, General tab: shortcut, recording, indicator, paste, startup, permissions and troubleshooting">
</p>

**Settings › General** holds the shortcut, a recording time limit, the start and stop sounds, the indicator position, whether Tamka pastes for you and keeps the text on the clipboard, launch at login, the microphone and Accessibility permissions, and the activity log. Changes apply immediately; there is no Save button.

## Privacy

- Speech recognition runs entirely on your Mac. Tamka connects to the internet only to download a model: Parakeet models come from Hugging Face, and Apple Speech comes from Apple.
- The microphone is on only between your two shortcut presses.
- Tamka keeps one recording, so it can transcribe it again: `~/Library/Caches/Tamka/last.wav`. Each new dictation replaces it.
- The log at `~/Library/Logs/Tamka.log` records events and timings, never your words.

## Accuracy and speed

Parakeet Ultra was tested against Whisper large-v3-turbo on 117 public recordings (32 minutes) of read speech, meetings and earnings calls with human-checked transcripts:

| | Whisper large-v3-turbo | Tamka with Parakeet Ultra |
|---|:-:|:-:|
| Words wrong, all recordings | 9.9% | **7.7%** |
| Words wrong, without meeting recordings | 6.9% | **6.2%** |
| Memory | 1.2–1.6 GB | **about 0.5 GB** |
| Wait after a 50-second dictation | 471 ms | **170 ms** |

The [full benchmark](docs/benchmark.md) covers 13 configurations across five model families. To compare models on your own voice and vocabulary, see [eval/README.md](eval/README.md).

## Install

Tamka is built from source. You need:

- a Mac with Apple silicon and macOS 26 or later;
- Xcode 26 or later;
- a current stable Rust toolchain from [rustup](https://rustup.rs).

```bash
git clone https://github.com/piedol/tamka.git
```

```bash
cd tamka && scripts/bundle.sh --install
```

The script builds the app and its speech engine, signs both, copies `Tamka.app` to `/Applications` and opens it. It signs with the first Developer ID Application certificate in your keychain, or the identity in `TAMKA_SIGN_IDENTITY`. Without either, it uses an ad-hoc signature, and macOS asks for permissions again after every build.

On first launch:

1. **Download a model.** Settings › Models opens by itself. Click **Download** next to **Parakeet Ultra**. It takes a minute or two, once.
2. **Allow Accessibility,** which Tamka needs to paste for you. Without it, Tamka copies the text and you paste it yourself.
3. **Allow the microphone** the first time you dictate.

Tamka has no Dock icon. To start it when you log in, turn on **Launch Tamka at login** in Settings › General.

## Troubleshooting

| Problem | Fix |
|---|---|
| The text is copied but not pasted | Accessibility is off. Click **Allow…** next to it in Settings › General. |
| Nothing happens when you press the shortcut | Another app may use the same keys. Choose a different shortcut in Settings › General. |
| The indicator covers what you're typing | Drag it somewhere else; it stays there. |
| The menu says no model is downloaded | Choose **Download a Speech Model…** in the menu. |
| Something else goes wrong | Open the activity log from Settings › General. It shows events and timings, never your words. |

## Development

```text
Tamka.app
├── Contents/MacOS/Tamka             Rust: menu bar, shortcut, microphone, text cleanup, paste, Settings
└── Contents/Helpers/tamka-engine    Swift: FluidAudio running the speech model on the Neural Engine
```

The menu bar app is written in Rust against AppKit through [objc2](https://github.com/madsmtm/objc2). The speech engine is a small Swift helper that keeps the model loaded and talks to the app over stdin and stdout in JSON lines. If the helper stops, Tamka restarts it on the next dictation.

| Path | What it does |
|---|---|
| `src/app.rs` | The menu bar and the dictation flow: idle, listening, transcribing, paste |
| `src/hud.rs` | The draggable indicator and notifications |
| `src/audio.rs` | Microphone capture with AVAudioEngine, only while listening |
| `src/engine.rs` | Starts and supervises the speech helper |
| `src/models.rs` | The model catalog and grades; download, removal and status |
| `src/text.rs` | Filler removal, stutter cleanup, spoken commands and spacing |
| `src/paste.rs` | The clipboard, then ⌘V once your modifier keys are released |
| `src/settings.rs` | Preferences, saved to `~/Library/Application Support/Tamka/settings.json` |
| `src/settings_window.rs` | The Settings window |
| `engine/` | The Swift speech helper |
| `eval/` | The benchmark harness |
| `scripts/` | Build, sign, icon and README image scripts |

```bash
cargo test --release                          # unit tests
scripts/bundle.sh                             # build and sign dist/Tamka.app
target/release/Tamka --transcribe clip.wav    # transcribe a file without the microphone
open "tamka://settings-models"                # open Settings on a tab: settings, settings-models, settings-text
open "tamka://hud-demo"                       # show the indicator's states
swift scripts/make-showcase.swift             # redraw docs/images/showcase.png from the screenshots
```

## Credits

- Speech runtime: [FluidAudio](https://github.com/FluidInference/FluidAudio) (Apache-2.0).
- Models: [NVIDIA Parakeet](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2) (CC-BY-4.0); Parakeet Ultra by [Moondream](https://huggingface.co/moondream/parakeet-ultra); Phonon-2 by Fermion Research. Each model's authors set its license, so check its page before redistributing it.
- Benchmark recordings: [LibriSpeech](https://www.openslr.org/12), [FLEURS](https://huggingface.co/datasets/google/fleurs) and [AMI](https://groups.inf.ed.ac.uk/ami/corpus/) (CC-BY-4.0), and [Earnings-22](https://huggingface.co/datasets/distil-whisper/earnings22) (CC-BY-SA-4.0).

## License

[PolyForm Noncommercial 1.0.0](LICENSE.md). You may use, change and share Tamka for any noncommercial purpose.
