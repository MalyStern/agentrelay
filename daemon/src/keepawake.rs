//! Keep-awake (plan 00 §, 02 §ז.5, fix #2). Holding a `KeepAwake` guard prevents the
//! machine from sleeping during an overnight run; dropping it releases the request.
//!
//! Windows: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` — the
//! `ES_CONTINUOUS` flag is what makes it persist rather than reset the idle timer
//! once (the exact bug fix #2 corrected). macOS: hold a `caffeinate` child. Linux:
//! hold a `systemd-inhibit` child. Unix helpers are best-effort (no-op if absent).

use crate::error::Result;

/// Windows keep-awake, pinned to one dedicated thread.
///
/// `SetThreadExecutionState` is **per-thread**: the request belongs to whichever
/// thread made the call, and only that thread can release it. The guard used to be
/// created and dropped inline on a multi-threaded tokio runtime, where the drop can
/// easily land on a different worker than the one that acquired it. The result was
/// the worst of both: the release did nothing, and the original thread's request
/// stayed alive — so a long-lived `agentrelay serve` would keep the machine awake
/// forever, with no way to turn it off short of restarting the process.
///
/// So the request lives on a thread of its own that exists only to hold it. Both
/// calls happen there, in order, on the same thread, whatever the runtime does with
/// the guard itself.
#[cfg(windows)]
pub struct KeepAwake {
    release: Option<std::sync::mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl KeepAwake {
    /// Keep the system awake. `keep_display` also keeps the screen on.
    pub fn new(keep_display: bool) -> Result<Self> {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };

        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();

        let worker = std::thread::Builder::new()
            .name("agentrelay-keepawake".into())
            .spawn(move || {
                let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
                if keep_display {
                    flags |= ES_DISPLAY_REQUIRED;
                }
                let acquired = unsafe { SetThreadExecutionState(flags) }.0 != 0;
                let _ = ready_tx.send(acquired);
                if !acquired {
                    return;
                }
                // Park until released (or until the sender is dropped, which is the
                // same thing and covers a panicking owner).
                let _ = release_rx.recv();
                // Clearing SYSTEM_REQUIRED, leaving only CONTINUOUS, releases it —
                // and this runs on the thread that made the request, which is the
                // only thread for which that is true.
                unsafe {
                    SetThreadExecutionState(ES_CONTINUOUS);
                }
            })
            .map_err(|e| {
                crate::error::AgentRelayError::new(
                    crate::error::ErrorCode::Internal,
                    format!("could not start the keep-awake thread: {e}"),
                )
            })?;

        match ready_rx.recv() {
            Ok(true) => Ok(KeepAwake {
                release: Some(release_tx),
                worker: Some(worker),
            }),
            _ => Err(crate::error::AgentRelayError::new(
                crate::error::ErrorCode::Internal,
                "SetThreadExecutionState failed",
            )),
        }
    }
}

#[cfg(windows)]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        // Signal, then WAIT. Returning before the request is actually released
        // would mean `drop` reports a state that has not happened yet — and the
        // caller's next act is often to let the machine sleep.
        drop(self.release.take());
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}

#[cfg(unix)]
pub struct KeepAwake {
    child: Option<std::process::Child>,
}

#[cfg(unix)]
impl KeepAwake {
    pub fn new(_keep_display: bool) -> Result<Self> {
        use std::process::Stdio;
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut c = std::process::Command::new("caffeinate");
            c.arg("-s"); // prevent system sleep while on AC
            c
        };
        #[cfg(not(target_os = "macos"))]
        let mut cmd = {
            let mut c = std::process::Command::new("systemd-inhibit");
            c.args([
                "--what=sleep:idle",
                "--who=agentrelay",
                "--why=overnight-run",
                "sleep",
                "infinity",
            ]);
            c
        };
        // Best-effort: if the helper isn't installed, we simply don't hold anything.
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
        Ok(KeepAwake { child })
    }
}

#[cfg(unix)]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_and_release_does_not_error() {
        // Smoke test: we can request keep-awake and drop it cleanly. (A true "system
        // won't sleep" assertion isn't feasible in a unit test.)
        let k = KeepAwake::new(false).expect("keep-awake");
        drop(k);
        let k2 = KeepAwake::new(true).expect("keep-awake + display");
        drop(k2);
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn it_survives_being_dropped_on_a_different_thread_than_it_was_made_on() {
        // The real bug: SetThreadExecutionState is per-thread, and on a
        // multi-threaded runtime the drop routinely lands on a different worker
        // than the acquire. The release then did nothing while the original
        // request stayed alive — a machine that never sleeps again.
        let guard = KeepAwake::new(false).expect("acquire");

        // Force the drop onto another thread, and make sure it completes rather
        // than hanging or panicking.
        let joined = tokio::task::spawn_blocking(move || {
            drop(guard);
            true
        })
        .await
        .expect("the release must not panic");
        assert!(joined);

        // Acquiring again immediately must still work — if the previous request
        // had leaked we would have no way to observe it, so at minimum prove the
        // path is re-entrant and the thread was cleaned up.
        let again = KeepAwake::new(false).expect("re-acquire after a cross-thread drop");
        drop(again);
    }

    #[cfg(windows)]
    #[test]
    fn many_acquire_release_cycles_do_not_leak_threads() {
        // Each guard owns a thread; if drop did not join, they would pile up for
        // the life of a long `agentrelay serve`.
        for _ in 0..25 {
            let k = KeepAwake::new(false).expect("acquire");
            drop(k);
        }
    }
}
