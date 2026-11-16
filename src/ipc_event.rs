//! The wake-up signal of the window → daemon channel.
//!
//! The requests themselves still live in the runtime database, but the daemon no
//! longer reopens that database every 50 ms to look for them. It waits on a named
//! event instead and reads the requests only when the window says there is one.
//!
//! An idle daemon then spends nothing on the channel at all, and a request still
//! arrives at once: the window writes the value first and signals second, so the
//! daemon always wakes to something readable.

use std::path::Path;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateEventW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE,
};

/// The event name for one data directory: two profiles must not wake each other.
fn name(root: &Path) -> Vec<u16> {
    let text = root.to_string_lossy().to_lowercase();
    // FNV-1a: a stable name with no characters Windows reserves.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let name = format!("Local\\voice-button-ipc-{hash:016x}");
    OsStr::new(&name)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// The event a daemon waits on.
pub struct Wake(HANDLE);

// A kernel handle is just a number for this process: it may be moved to the
// thread that waits on it, and no other thread keeps a copy after the move.
unsafe impl Send for Wake {}

impl Wake {
    /// Create the event the daemon waits on.
    pub fn create(root: &Path) -> Option<Self> {
        let name = name(root);
        // SAFETY: the name is a NUL-terminated wide string that outlives the
        // call, and a null security descriptor means the default one.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, name.as_ptr()) };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// Block until the window asks for something. This costs no CPU while it
    /// waits, which is the whole point of the event.
    pub fn wait(&self) {
        // SAFETY: the handle was created by `create` and stays open until drop.
        unsafe { WaitForSingleObject(self.0, INFINITE) };
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: the handle belongs to this object and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// Tell the daemon that a request was written.
///
/// A window that signals before the daemon started has nothing to wake; the
/// request stays in the database and the daemon reads it at its next start.
pub fn notify(root: &Path) {
    let name = name(root);
    // SAFETY: the name is a NUL-terminated wide string that outlives the call.
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    if handle.is_null() {
        return;
    }
    // SAFETY: the handle was opened for exactly this.
    unsafe { SetEvent(handle) };
    // SAFETY: the handle is ours and is closed once.
    unsafe { CloseHandle(handle) };
}

#[cfg(test)]
mod tests {
    use super::{notify, Wake};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_notice_wakes_the_waiter() {
        let root = std::path::Path::new("C:\\vbt\\ipc-event-test");
        let wake = Wake::create(root).expect("the event is created");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            wake.wait();
            let _ = tx.send(());
        });
        // Give the waiter a moment to block, then ring exactly as the window does.
        std::thread::sleep(Duration::from_millis(50));
        notify(root);
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the waiter must return after the notice"
        );
    }

    #[test]
    fn a_notice_without_a_daemon_does_nothing() {
        // The window may signal before the daemon started; that must not panic.
        notify(std::path::Path::new("C:\\vbt\\ipc-event-nobody-home"));
    }
}
