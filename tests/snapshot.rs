//! Snapshot test: runs the app in the VB_SNAPSHOT mode, which renders the
//! main UI states (Settings, History, About, download progress) and captures
//! the window into PNG files, then checks that all snapshots were produced.
//! The committed copies in `tests/snapshots/` are refreshed with:
//! `VB_SNAPSHOT=tests/snapshots cargo run --release`.

#[test]
fn ui_snapshots_render() {
    // The hosted Windows runner has no interactive desktop: the app starts but
    // never renders the window, so no PNGs appear. The visual check therefore
    // runs only on a real desktop; the committed copies in `tests/snapshots/` are still
    // regenerated locally with `VB_SNAPSHOT=tests/snapshots cargo run --release`.
    if std::env::var_os("CI").is_some() {
        eprintln!("skipping UI snapshot test on CI (no interactive desktop)");
        return;
    }
    let exe = env!("CARGO_BIN_EXE_voice-button");
    let out = std::env::temp_dir().join(format!("voice-button-snaptest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();

    // Keep the output: when the generator writes nothing, its own words are the
    // only clue (a missing library, a busy desktop, a data directory that cannot
    // be opened).
    let run = std::process::Command::new(exe)
        .env("VB_SNAPSHOT", &out)
        .output()
        .expect("failed to run voice-button in snapshot mode");
    assert!(
        run.status.success(),
        "snapshot run exited with {}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let committed = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    for name in [
        "settings.png",
        "api.png",
        "history.png",
        "history-expanded.png",
        "about.png",
        "download-progress.png",
        "capture-recording.png",
        "record.png",
        "phone.png",
        "settings-light.png",
        "history-light.png",
        "record-light.png",
        "capture-recording-light.png",
        "api-light.png",
        "about-light.png",
        "download-progress-light.png",
        "phone-light.png",
        "history-expanded-light.png",
    ] {
        let path = out.join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("missing snapshot {name}: {e}"));
        assert!(bytes.starts_with(b"\x89PNG"), "{name} is not a PNG file");
        assert!(bytes.len() > 20_000, "{name} is suspiciously small ({} bytes)", bytes.len());
        let kept = committed.join(name);
        assert!(
            kept.is_file(),
            "committed snapshot missing: {}",
            kept.display()
        );
    }

    let _ = std::fs::remove_dir_all(&out);
}
