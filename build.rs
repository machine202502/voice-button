fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
        let version_u64 = version
            .split('.')
            .chain(std::iter::repeat("0"))
            .take(4)
            .filter_map(|p| p.parse::<u64>().ok())
            .fold(0u64, |acc, n| (acc << 16) | (n & 0xFFFF));
        for (bin, internal, original) in [
            ("voice-button", "voice-button", "voice-button.exe"),
            ("voice-button-ui", "voice-button-ui", "voice-button-ui.exe"),
        ] {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.set("FileDescription", "Voice Button");
            res.set("ProductName", "Voice Button");
            res.set("LegalCopyright", "Copyright (c) 2026 machine202502");
            res.set(
                "Comments",
                "MIT License — https://github.com/machine202502/voice-button. Third-party notices are included in release packages.",
            );
            res.set("InternalName", internal);
            res.set("OriginalFilename", original);
            res.set_version_info(winresource::VersionInfo::FILEVERSION, version_u64);
            res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, version_u64);
            res.set_version_info(winresource::VersionInfo::FILEOS, 0x40004);
            res.set_version_info(winresource::VersionInfo::FILETYPE, 1);
            res.compile_for(bin)
                .unwrap_or_else(|e| panic!("failed to embed windows resources for {bin}: {e}"));
        }
    }
}
