# Voice Button

Offline voice-to-text for Windows 10/11. Voice Button lives in the tray, records
the microphone, shows live transcription, copies the result to the clipboard,
and keeps a persistent history.

Source: https://github.com/machine202502/voice-button
Site: https://machine202502.github.io/voice-button/

## Use

- Click the tray microphone to start recording; click again to stop.
- Middle-click the tray microphone to record and paste the result at the caret
  of the focused window (same as the paste hotkey). **Settings → Mouse** can
  swap the left and middle button actions.
- If transcriptions are empty, pick the real microphone under
  **Settings → Microphone**: a virtual cable such as VB-Audio records silence.
  When the Windows default input is a virtual cable, a real microphone is
  selected automatically.
- **Settings → Paste** chooses what follows inserted text in record-and-paste
  mode: a space (default) or a new line.
- Open **Capture** to start/stop from the window and watch the live transcript.
- The tray menu also contains **Settings**, **Capture**, **Last Copy**, and **Quit**.
- Recording is unavailable until a usable model is downloaded. During an active
  download the tray click opens Settings instead of starting recording.

Long dictation is the normal case, not an exception. While it records, the
daemon writes the microphone to `recordings/<session>/audio.wav`, cuts the stream
into segments at pauses (and, if speech never pauses, at its quietest point near
25 s) and transcribes each segment as soon as it ends — without waiting for the
recording to stop. Silence never reaches the model. Text therefore grows while
you speak, and every minute of speech costs the same to transcribe as a short
note instead of the quadratic growth of re-reading the whole buffer.

The recognizer runs in a child process (`voice-button.exe --asr-worker`), so a
failing model costs one segment: the worker is replaced and the segment retried
while the recording continues. If the daemon itself dies, `--guard` restarts it,
plays the system error sound, turns the tray microphone blue and transcribes the
interrupted recording from disk into the history with an "incomplete" mark;
recording stays blocked until that finishes.

### Global hotkeys

Both shortcuts start unbound; set them in **Settings → Global hotkey**.

- **Global hotkey** — starts and stops a recording that becomes a new history
  entry.
- **Append hotkey (continue the last message)** — records a short piece and
  appends it to the last history entry, starting a new line. This is meant for
  dictating a long message in small pieces instead of knowing the whole text up
  front. If the history is empty, the piece becomes the first entry.
- **Paste hotkey (type at the cursor)** — records a new history entry and, once
  the text is ready, pastes it at the caret of the window that was active when
  recording started (the text is placed in the clipboard and Ctrl+V is sent).
  The clipboard content that was there before is restored afterwards.

After any hotkey stops, the clipboard receives the complete message: the new
text for the plain hotkey, the whole merged message in append mode, and the new
text in paste mode (only while pasting). The **Last Copy** tray item and the
**Copy** button in the history list also copy the complete message. In the
expanded history row each appended piece is listed with its own time; press the
small **Copy** on a piece to copy just that piece.

The application has two executables that must stay beside each other:

- `voice-button.exe` — tray daemon, recording, transcription, downloads, history.
  It also runs two hidden roles: `--asr-worker` (the recognizer process) and
  `--guard` (the supervisor that restarts a crashed daemon).
- `voice-button-ui.exe` — the window, started only while it is visible.

Autostart and the shortcuts start `voice-button.exe --guard`, so the app comes
back after a crash instead of staying down until the next sign-in.

All application assets and Windows resources are embedded at build time.
Models are not bundled.

## Local API

The app can also transcribe audio that a page, an editor plugin or a script
sends it, instead of using its own microphone. Enable it under **API**, pick the
host (default `127.0.0.1`) and port (default `8931`). The caller streams 16-bit
PCM and receives live and final text; the result is saved to history with the
`server` source.

- `GET /api/help` — the whole contract as JSON: endpoints, audio format, session
  rules, error codes and runnable examples. Hand it to an agent or a tool that
  has to integrate the app; it does not need the binary.
- `POST /api/record/start` → `POST /api/record/audio?session=N` (PCM16 LE mono)
  → `POST /api/record/stop`, then poll `GET /api/status` and read
  `GET /api/record/text`.
- `GET /api/models` lists the models this build can use. `start` may override
  the engine and model for that session only, without touching the user's
  settings.
- One session at a time: while the user is dictating in the app, the API answers
  409 instead of interrupting. The user can stop an API session from the window;
  the text is still transcribed and saved.

A ready-made browser demo (microphone → app) lives in `api-demo/`:

```powershell
node api-demo/server.mjs      # http://127.0.0.1:8080/
```

There is no authentication: keep the host on `127.0.0.1` unless the network is
trusted. Browser microphones work only on localhost or over HTTPS.

## Models

Settings offers Whisper (whisper.cpp) and ONNX Runtime models. Multilingual
entries are listed first; language-specific Russian models follow. Each entry
shows its approximate download size.

- Whisper (whisper.cpp): quantized `large-v3-turbo-q5_0` (default), `medium-q5_0`,
  `small-q5_1`, plus full-precision `large-v3-turbo`, `large-v3`, `medium`,
  `small`.
- Multilingual ONNX: `whisper-base-onnx` (Whisper base converted for sherpa-onnx)
  and `sense-voice` (Chinese, English, Japanese, Korean, Cantonese).
- Russian ONNX (GigaAM, sherpa-onnx conversions): `gigaam-v3-sherpa-int8`
  (default), `gigaam-v3-ctc-punct`, `gigaam-v3-rnnt`, `gigaam-v3-rnnt-punct`,
  and `gigaam-v2-ctc`.

Whisper and SenseVoice can detect the spoken language; GigaAM models use an
explicit language setting. Custom Whisper files and custom ONNX CTC/RNN-T,
Whisper, and SenseVoice folders are also supported.

## Settings and data

Interface language is picked under **Settings → Interface language**: Russian,
English and Chinese ship as JSON catalogs in `locales/` next to the executable,
and "System" follows the Windows language (falling back to English). Adding a
language means dropping another `<code>.json` into that folder (or into
`%APPDATA%\voice-button\locales`): no rebuild, and the file appears in the
picker. On the first launch the app also suggests a model that fits the system
language: GigaAM RNN-T with punctuation for Russian, SenseVoice for English,
Chinese, Japanese and Korean, multilingual Whisper for everything else. After
that the choice belongs to the user.

Sound toggles, sound volume, language, and autostart apply immediately. A model
selection applies only when its files are ready. Downloads run in the daemon,
show progress, and can be cancelled; closing the UI does not cancel them.

Data lives in `%APPDATA%\voice-button`:

- `config.json` — applied settings.
- `history.jsonl` — persistent history, one JSON entry per line.
- `models/` — downloaded models.
- `recordings/` — audio of the recording in progress; removed once its text is
  saved, kept when the transcription failed.
- `crash.txt` — crashes reported by the guard (exit code and time).
- `log.txt` and rotated copies — application activity; the previous run is kept
  as `log.txt.1`, so a crash is not erased by the restart.

History indexes only timestamps, previews, and byte offsets. Full text is read
on demand for expansion or copying, and the scrolling list renders only visible
rows.

## Build and test

Requirements: Rust MSVC, CMake, and Visual Studio Build Tools for whisper.cpp.
The ONNX Runtime dependency obtains its native binaries during the build.

```powershell
cargo build --release --bins
cargo test --release
```

End-to-end checks decode the eight-minute recording in `tests/assets` and need a
downloaded model plus the app binary next to the test executable. They are
ignored by default:

```powershell
cargo test --release --test e2e -- --ignored --nocapture
```

`VB_E2E_MODEL` (default `Onnx/gigaam-v3-rnnt-punct`) selects the model.

To refresh the UI snapshots in `Tests/`:

```powershell
$env:VB_SNAPSHOT = "Tests"
.\target\release\voice-button.exe
Remove-Item Env:VB_SNAPSHOT
```

## Packages

With Inno Setup 6 installed:

```powershell
./scripts/package.ps1
```

This creates both release forms in `dist/`:

- `voice-button-VERSION-windows-x64.zip` — portable, run `voice-button.exe`.
- `voice-button-VERSION-windows-x64-setup.exe` — installer for all users.

Both packages contain the two executables, `LICENSE`, `NOTICE`, `AISOFTWARE`,
and `THIRD-PARTY.txt`. The portable zip also carries the Visual C++ runtime
DLLs and the Vulkan loader, so it runs on a clean machine. The installer runs
`vc_redist.x64.exe` silently during setup and installs the same DLLs app-local.
`scripts/publish.ps1` attaches them to a GitHub release
using the authenticated GitHub CLI.

CI tests pushes and pull requests. A version change in `Cargo.toml` on `master`
publishes a release. The static site in `site/` deploys to GitHub Pages and
resolves the latest release links, sizes, and SHA-256 hashes.

## License and acknowledgments

Voice Button source code is available under the MIT License; see `LICENSE`.
`AISOFTWARE` describes the use of neural networks in development.

Third-party notices are in `NOTICE` and generated `THIRD-PARTY.txt`. This
includes ONNX Runtime, sherpa-onnx (the engine behind every ONNX model: NeMo
GigaAM, Whisper and SenseVoice, with its feature extraction and decoders),
whisper.cpp, Rust crates, and bundled CC0 sound cues.

Model downloads are recommendations external to the release package; model
license/source notices are therefore not included as bundled-model notices.
