//! Snapshot test: runs the app in the VB_SNAPSHOT mode, which renders the
//! main UI states (Settings, History, About, download progress) and captures
//! the window into PNG files, then checks that all snapshots were produced.
//! The committed copies in `Tests/` are refreshed with:
//! `VB_SNAPSHOT=Tests cargo run --release`.

#[test]
fn ui_snapshots_render() {
    // The hosted Windows runner has no interactive desktop: the app starts but
    // never renders the window, so no PNGs appear. The visual check therefore
    // runs only on a real desktop; the committed copies in `Tests/` are still
    // regenerated locally with `VB_SNAPSHOT=Tests cargo run --release`.
    if std::env::var_os("CI").is_some() {
        eprintln!("skipping UI snapshot test on CI (no interactive desktop)");
        return;
    }
    let exe = env!("CARGO_BIN_EXE_voice-button");
    let out = std::env::temp_dir().join(format!("voice-button-snaptest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();

    let status = std::process::Command::new(exe)
        .env("VB_SNAPSHOT", &out)
        .status()
        .expect("failed to run voice-button in snapshot mode");
    assert!(status.success(), "snapshot run exited with {status}");

    for name in [
        "settings.png",
        "history.png",
        "history-expanded.png",
        "about.png",
        "download-progress.png",
        "capture-recording.png",
    ] {
        let path = out.join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("missing snapshot {name}: {e}"));
        assert!(bytes.starts_with(b"\x89PNG"), "{name} is not a PNG file");
        assert!(bytes.len() > 20_000, "{name} is suspiciously small ({} bytes)", bytes.len());
    }

    let _ = std::fs::remove_dir_all(&out);
}
