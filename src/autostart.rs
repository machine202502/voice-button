use std::env::current_exe;
use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_NAME: &str = "VoiceButton";

pub fn set_enabled(silent: bool, enabled: bool) -> anyhow::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)?;
    if enabled {
        let exe = current_exe()?;
        // The guard owns the daemon: it restarts it after a crash and records
        // the exit code, which no handler inside the dying process can do.
        let command = if silent {
            format!("\"{}\" --guard /silent", exe.display())
        } else {
            format!("\"{}\" --guard", exe.display())
        };
        key.set_value(APP_NAME, &command)?;
    } else {
        let _ = key.delete_value(APP_NAME);
    }
    Ok(())
}
