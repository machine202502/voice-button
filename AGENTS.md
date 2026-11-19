# Voice Button project rules

This file applies to work in this repository. It does not change the global
agent rules; it only records the project-specific workflow.

## Every completed change

1. Bump at least the patch version in `Cargo.toml`; let `Cargo.lock` follow. The app and the installer must report the new version.
2. Regenerate or verify the UI snapshots in `tests/snapshots` and the site screenshots in `site/assets`; update every image that changed.
3. Run the project's checks and tests and show the commands and their results.
4. Build the release installer so the release directory contains the latest `voice-button-<version>-windows-x64-setup.msi`. Locally run `scripts\package.ps1`; it writes to `tmp\dist\`. Packaging uses Velopack, so it needs the .NET 8 SDK and `vpk` (`dotnet tool install -g vpk`). It also writes the update feed to `site\updates\`. The user needs the installer, not the bare executable.
5. Never commit or push unless the user explicitly asks.

An explicit version instruction (patch, minor, major, or "fix the version")
overrides the automatic bump for that one change only. On the next change,
resume the automatic patch bump unless the user explicitly says otherwise
again.

## Snapshots and e2e

`tests/snapshot.rs` runs the app in `VB_SNAPSHOT` mode. The committed images live
in `tests/snapshots/`; the site copies live in `site/assets/`.

Regenerate the UI snapshots after building release:

```powershell
.\scripts\build.ps1
$env:VB_SNAPSHOT = "tests/snapshots"
.\tmp\release\voice-button.exe
Remove-Item Env:VB_SNAPSHOT
```

Then refresh the site screenshots with `scripts/site-shots.py` when their
content changed. If image dimensions change, update the `width`/`height`
attributes in the three `site/*/index.html` files.

Run the e2e checks with:

```powershell
cargo test --release --test e2e -- --ignored --nocapture
```

The page has Node tests (`node tests\*.mjs`) and a browser check
(`python tests\phone-e2e.py`). If the snapshot or screenshot tooling is missing
or cannot run, stop and ask instead of silently skipping the images.

`.tools/` is a gitignored scratch area for task-local utilities. It is not part
of the official project, its scripts may be stale, and agents must not rely on
it as the snapshot or build path. Put new one-off tools there; use
`scripts/build.ps1`, `tests/snapshot.rs`, and `scripts/package.ps1` for the
official work.

## Updates

The installed app uses Velopack and checks
`https://machine202502.github.io/voice-button/updates` when About opens.
`scripts\package.ps1` writes `releases.win.json` and the `.nupkg` to
`site\updates\`; `.github/workflows/site.yml` downloads the same files from the
GitHub release before deploying Pages. A release must attach
`releases.win.json` and the `.nupkg` packages, or the feed cannot work.

The Velopack pack id is `VoiceButtonApp`, so the install root is separate from
the app's `%APPDATA%\voice-button` data, models, and logs. Keep that split.

0.8.x is an MSI. Uninstall it through Settings → Apps, or with `msiexec /x` on
the MSI. `Update.exe --uninstall` refuses MSI installs with "MSI installation
detected"; deleting the folder leaves the MSI registration and makes the next
installer offer repair instead of a fresh install.

`tests/snapshot.rs` and `VB_DATA_DIR` runs must not touch the real install or
the network update check. The update checker stays out of snapshot mode.
