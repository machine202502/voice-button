//! The `--guard` process: starts the daemon, survives its crashes and restarts
//! it. A crash of the daemon is a `__fastfail` abort that no handler inside the
//! process can catch, so the only place that can react to it is this one.
//!
//! The guard reports the exit code to `crash.txt`; the restarted daemon plays
//! the error sound and recovers the interrupted recording.

use crate::config;
use crate::log;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Restarting faster than this in a loop is not recovery any more.
const MAX_FAST_RESTARTS: usize = 5;
const FAST_WINDOW: Duration = Duration::from_secs(120);

pub fn run() -> ! {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => std::process::exit(1),
    };
    let dirs = config::app_dirs();
    log::init_child(&dirs.root);
    log::log(&format!("guard started for {}", exe.display()));
    let mut restarts: Vec<Instant> = Vec::new();
    loop {
        let started = Instant::now();
        let mut child = match Command::new(&exe)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                log::append_crash_report(
                    &dirs.root,
                    &format!("guard: cannot start {}: {e}", exe.display()),
                );
                std::process::exit(1);
            }
        };
        let status = child.wait();
        let code = match status {
            Ok(status) => status.code(),
            Err(_) => None,
        };
        let uptime = started.elapsed();
        match code {
            Some(0) => {
                log::log("guard: the daemon exited normally");
                std::process::exit(0);
            }
            other => {
                log::append_crash_report(
                    &dirs.root,
                    &format!(
                        "guard: the daemon died (code {}) after {:.0} s",
                        other
                            .map(|c| format!("0x{c:08X}"))
                            .unwrap_or_else(|| "unknown".to_string()),
                        uptime.as_secs_f64()
                    ),
                );
                log::log("guard: the daemon died, restarting");
            }
        }
        let now = Instant::now();
        restarts.retain(|at| now.duration_since(*at) < FAST_WINDOW);
        restarts.push(now);
        if restarts.len() > MAX_FAST_RESTARTS {
            log::append_crash_report(
                &dirs.root,
                "guard: restarting too often, giving up",
            );
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(600));
    }
}
