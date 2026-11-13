//! Opening the phone server to the internet, the boring way.
//!
//! Two things have to happen outside the app: a Windows Firewall rule for the
//! port, and a port mapping on the router. The first always needs administrator
//! rights, the second needs a router that speaks UPnP. Both live in
//! `net.ps1`, which re-launches itself elevated, does the work and writes a
//! plain `key=value` hand-off file. The app imports that file into SQLite and
//! deletes it; the database is the only place the result stays.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// What the script managed to set up, as it wrote it down.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub firewall_rule: String,
    #[serde(default)]
    pub firewall_created: String,
    #[serde(default)]
    pub upnp: Option<Upnp>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub updated: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Upnp {
    #[serde(default)]
    pub router: String,
    #[serde(default)]
    pub external_port: u16,
    #[serde(default)]
    pub internal_ip: String,
    #[serde(default)]
    pub added: bool,
}

/// Path of the hand-off file the script writes. The app imports it into
/// SQLite immediately and deletes it; it is never the source of truth.
pub fn state_path(root: &Path) -> PathBuf {
    root.join("site").join("network.state")
}

fn truthy(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "1" | "yes")
}

fn parse(text: &str) -> Option<State> {
    let mut map = std::collections::BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    if map.is_empty() {
        return None;
    }
    let get = |key: &str| map.get(key).cloned().unwrap_or_default();
    let upnp = if get("upnp_router").is_empty() && get("upnp_external_port").is_empty() {
        None
    } else {
        Some(Upnp {
            router: get("upnp_router"),
            external_port: get("upnp_external_port").parse().unwrap_or(0),
            internal_ip: get("upnp_internal_ip"),
            added: truthy(&get("upnp_added")),
        })
    };
    Some(State {
        port: get("port").parse().unwrap_or(0),
        enabled: truthy(&get("enabled")),
        firewall_rule: get("firewall_rule"),
        firewall_created: get("firewall_created"),
        upnp,
        error: Some(get("error")).filter(|value| !value.is_empty()),
        updated: get("updated"),
    })
}

pub fn read(root: &Path) -> Option<State> {
    let store = crate::store::Store::open(root).ok()?;
    let conn = store.conn();
    conn.query_row(
        "SELECT port, enabled, firewall_rule, firewall_created, upnp_router,
            upnp_external_port, upnp_internal_ip, upnp_added, error, updated
         FROM site_network WHERE id = 1",
        [],
        |row| {
            Ok(State {
                port: row.get::<_, i64>(0)? as u16,
                enabled: row.get::<_, i64>(1)? != 0,
                firewall_rule: row.get(2)?,
                firewall_created: row.get(3)?,
                upnp: {
                    let router: String = row.get(4)?;
                    let external_port: i64 = row.get(5)?;
                    let internal_ip: String = row.get(6)?;
                    if router.is_empty() && external_port == 0 && internal_ip.is_empty() {
                        None
                    } else {
                        Some(Upnp {
                            router,
                            external_port: external_port as u16,
                            internal_ip,
                            added: row.get::<_, i64>(7)? != 0,
                        })
                    }
                },
                error: row.get(8)?,
                updated: row.get(9)?,
            })
        },
    )
    .ok()
}

pub fn write(root: &Path, state: &State) -> Result<()> {
    let store = crate::store::Store::open(root)?;
    store.conn().execute(
        "INSERT INTO site_network(id, port, enabled, firewall_rule, firewall_created,
            upnp_router, upnp_external_port, upnp_internal_ip, upnp_added, error, updated)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(id) DO UPDATE SET port = excluded.port, enabled = excluded.enabled,
            firewall_rule = excluded.firewall_rule, firewall_created = excluded.firewall_created,
            upnp_router = excluded.upnp_router, upnp_external_port = excluded.upnp_external_port,
            upnp_internal_ip = excluded.upnp_internal_ip, upnp_added = excluded.upnp_added,
            error = excluded.error, updated = excluded.updated",
        rusqlite::params![
            state.port as i64,
            state.enabled as i64,
            state.firewall_rule,
            state.firewall_created,
            state.upnp.as_ref().map(|upnp| upnp.router.as_str()).unwrap_or(""),
            state.upnp.as_ref().map(|upnp| upnp.external_port as i64).unwrap_or(0),
            state.upnp.as_ref().map(|upnp| upnp.internal_ip.as_str()).unwrap_or(""),
            state.upnp.as_ref().map(|upnp| upnp.added as i64).unwrap_or(0),
            state.error,
            state.updated
        ],
    )?;
    Ok(())
}

/// Import the script's hand-off file and remove it. The database is the only
/// place the state stays.
fn import(root: &Path) -> Option<State> {
    let path = state_path(root);
    let text = std::fs::read_to_string(&path).ok()?;
    let state = parse(&text)?;
    let _ = write(root, &state);
    let _ = std::fs::remove_file(path);
    Some(state)
}

/// Where the script lives: next to the executable, like the locales.
pub fn script_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    #[allow(unused_mut)]
    let mut candidates = vec![dir.join("net.ps1"), dir.join("scripts").join("net.ps1")];
    // A development build runs from a target directory that is not inside the
    // repository, so the packaged locations do not exist there. Tests are the
    // same case even in release: `cargo test --release` also runs outside the
    // repository. A shipped release binary never gets the build machine's path.
    #[cfg(any(debug_assertions, test))]
    if let Some(root) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(Path::new(root).join("scripts").join("net.ps1"));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Run the script and read back what it did. Blocking: the caller puts it on a
/// thread because the user may take a while to answer the UAC prompt.
pub fn apply(root: &Path, port: u16, internal_ip: &str, skip_upnp: bool) -> Result<State> {
    run(root, "enable", port, internal_ip, skip_upnp)?;
    import(root)
        .or_else(|| read(root))
        .context("the network script wrote no state")
}

/// Remove the firewall rule and the router mapping. Also blocking.
pub fn clear(root: &Path) -> Result<State> {
    run(root, "disable", 0, "", false)?;
    import(root)
        .or_else(|| read(root))
        .context("the network script wrote no state")
}

/// Remember that the last attempt failed, so the window can say so instead of
/// showing the stale result of an earlier success.
pub fn record_failure(root: &Path, message: &str) {
    let mut state = read(root).unwrap_or_else(|| State {
        port: 0,
        enabled: false,
        firewall_rule: String::new(),
        firewall_created: String::new(),
        upnp: None,
        error: None,
        updated: String::new(),
    });
    state.error = Some(message.to_string());
    state.updated = chrono::Local::now().to_rfc3339();
    let _ = write(root, &state);
}

/// Arguments for the script. Kept in one place so the caller and the script
/// cannot drift apart: a single unknown parameter makes PowerShell exit before
/// doing anything, and the user only sees "nothing happened".
fn arguments(root: &Path, action: &str, port: u16, internal_ip: &str, skip_upnp: bool) -> Vec<String> {
    let mut args = vec![
        "-Action".to_string(),
        action.to_string(),
        "-Port".to_string(),
        port.to_string(),
        "-StatePath".to_string(),
        state_path(root).to_string_lossy().to_string(),
    ];
    if !internal_ip.is_empty() {
        args.push("-InternalIp".to_string());
        args.push(internal_ip.to_string());
    }
    if skip_upnp {
        args.push("-SkipUpnp".to_string());
    }
    args
}

fn run(root: &Path, action: &str, port: u16, internal_ip: &str, skip_upnp: bool) -> Result<()> {
    let script = script_path().context("net.ps1 is not installed next to the app")?;
    let mut command = Command::new("powershell.exe");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&script);
    for argument in arguments(root, action, port, internal_ip, skip_upnp) {
        command.arg(argument);
    }
    crate::log::log(&format!(
        "network: running net.ps1 {action} on port {port} (upnp {})",
        if skip_upnp { "skipped" } else { "wanted" }
    ));
    let status = command
        .status()
        .context("cannot start PowerShell for the network setup")?;
    if !status.success() {
        crate::log::log(&format!(
            "network: net.ps1 {action} exited with {status}"
        ));
    }
    Ok(())
}

/// Does the saved setup belong to another port? Then it must be torn down
/// before the new one is applied, or the old hole stays open.
pub fn stale(state: &State, port: u16) -> bool {
    state.enabled && state.port != port
}

/// Human-readable summary for the interface, in the language of the app.
pub fn summary(state: &State) -> String {
    if !state.enabled {
        return crate::i18n::tr("Not configured");
    }
    let ports = state.port.to_string();
    let firewall = if state.firewall_rule.is_empty() {
        crate::i18n::tr("firewall: not created")
    } else {
        format!("{} · TCP {ports}", state.firewall_rule)
    };
    let upnp = match &state.upnp {
        Some(upnp) if upnp.added => format!(
            "{}: {}",
            crate::i18n::tr("router"),
            upnp.router
        ),
        _ => crate::i18n::tr("router: no mapping"),
    };
    match &state.error {
        Some(error) => format!("{firewall} · {upnp} · {error}"),
        None => format!("{firewall} · {upnp}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "voice-button-network-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// The app builds the command line for the script; a parameter that the
    /// script does not know makes PowerShell exit before doing anything, and
    /// the user only sees "nothing happened". This runs the real script with
    /// the real arguments (as `status`, which needs no administrator).
    #[test]
    fn the_script_accepts_the_arguments_we_pass() {
        let root = temp_root("arguments");
        let mut args = arguments(&root, "enable", 8443, "192.168.1.10", false);
        // Only the action changes: every other argument must be accepted.
        let index = args.iter().position(|a| a == "enable").expect("action");
        args[index] = "status".to_string();
        let script = script_path().expect("net.ps1");
        let output = std::process::Command::new("powershell.exe")
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&script)
            .args(&args)
            .output()
            .expect("run net.ps1");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "the script rejected the arguments we pass: {stderr}"
        );
        assert!(
            !stderr.contains("cannot be found"),
            "unknown parameter: {stderr}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_state_round_trips() {
        let root = temp_root("state");
        let state = State {
            port: 8443,
            enabled: true,
            firewall_rule: "Voice Button Site".to_string(),
            firewall_created: "2026-11-13T10:00:00".to_string(),
            upnp: Some(Upnp {
                router: "192.168.1.1".to_string(),
                external_port: 8443,
                internal_ip: "192.168.1.10".to_string(),
                added: true,
            }),
            error: None,
            updated: "2026-11-13T10:00:01".to_string(),
        };
        write(&root, &state).unwrap();
        assert_eq!(read(&root), Some(state));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_previous_port_is_reported_as_stale() {
        let state = State {
            port: 8443,
            enabled: true,
            ..Default::default()
        };
        assert!(!stale(&state, 8443));
        assert!(stale(&state, 8445));
        let disabled = State {
            port: 8443,
            enabled: false,
            ..Default::default()
        };
        assert!(!stale(&disabled, 8445));
    }

    #[test]
    fn a_missing_file_reads_as_nothing() {
        let root = temp_root("missing");
        assert!(read(&root).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_script_is_found_in_the_repository_layout() {
        // Debug builds run from target/debug; the script sits in scripts/.
        let found = script_path();
        assert!(
            found
                .as_ref()
                .map(|path| path.ends_with("net.ps1"))
                .unwrap_or(false),
            "net.ps1 should be found, got {found:?}"
        );
    }
}
