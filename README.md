# Voice Button

Offline voice-to-text for Windows 10/11. Voice Button lives in the tray, records
the microphone, shows live transcription, copies the result to the clipboard,
and keeps a persistent history.

Source: https://github.com/machine202502/voice-button
Site: https://machine202502.github.io/voice-button/

## Use

- Click the tray microphone to start recording; click again to stop.
- Open **Capture** to start/stop from the window and watch the live transcript.
- The tray menu also contains **Settings**, **Capture**, **Last Copy**, and **Quit**.
- Recording is unavailable until a usable model is downloaded. During an active
  download the tray click opens Settings instead of starting recording.

The application has two executables that must stay beside each other:

- `voice-button.exe` — tray daemon, recording, transcription, downloads, history.
- `voice-button-ui.exe` — the window, started only while it is visible.

All application assets and Windows resources are embedded at build time.
Models are not bundled.

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

Sound toggles, sound volume, language, and autostart apply immediately. A model
selection applies only when its files are ready. Downloads run in the daemon,
show progress, and can be cancelled; closing the UI does not cancel them.

Data lives in `%APPDATA%\voice-button`:

- `config.json` — applied settings.
- `history.jsonl` — persistent history, one JSON entry per line.
- `models/` — downloaded models.
- `log.txt` and rotated copies — application activity.

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
and `THIRD-PARTY.txt`. `scripts/publish.ps1` attaches them to a GitHub release
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
