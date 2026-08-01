//! Keep-awake (plan 00 §, 02 §ז.5, fix #2). Holding a `KeepAwake` guard prevents the
//! machine from sleeping during an overnight run; dropping it releases the request.
//!
//! Windows: `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED)` — the
//! `ES_CONTINUOUS` flag is what makes it persist rather than reset the idle timer
//! once (the exact bug fix #2 corrected). macOS: hold a `caffeinate` child. Linux:
//! hold a `systemd-inhibit` child. Unix helpers are best-effort (no-op if absent).

use crate::error::Result;

#[cfg(windows)]
pub struct KeepAwake {
    active: bool,
}

#[cfg(windows)]
impl KeepAwake {
    /// Keep the system awake. `keep_display` also keeps the screen on.
    pub fn new(keep_display: bool) -> Result<Self> {
        use windows::Win32::System::Power::{
            SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
        if keep_display {
            flags |= ES_DISPLAY_REQUIRED;
        }
        let prev = unsafe { SetThreadExecutionState(flags) };
        if prev.0 == 0 {
            return Err(crate::error::AgentRelayError::new(
                crate::error::ErrorCode::Internal,
                "SetThreadExecutionState failed",
            ));
        }
        Ok(KeepAwake { active: true })
    }
}

#[cfg(windows)]
impl Drop for KeepAwake {
    fn drop(&mut self) {
        if self.active {
            use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
            // Clearing SYSTEM_REQUIRED (leaving only CONTINUOUS) releases the request.
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
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
}
