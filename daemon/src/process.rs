//! Process Manager — a stop that really stops (plan 00 §8, 02 §ז.3, fix #9). A
//! `ProcessGroup` spawns a child into an OS kill-group so that killing the group
//! kills the WHOLE tree — the agent AND anything it spawned (dev servers, test
//! watchers, grandchildren). Windows uses a Job Object with KILL_ON_JOB_CLOSE; Unix
//! uses a process group. `kill_tree` terminates them all.
//!
//! TODO (fix #9 hardening): on Windows, assign happens just after spawn — a
//! microsecond race the plan closes with CREATE_SUSPENDED→assign→resume. The
//! `external_resources` reconcile (docker/WSL/MCP outside the tree) is a follow-up.

use crate::error::{AgentRelayError, ErrorCode, Result};
use tokio::process::{Child, Command};

pub struct ProcessGroup {
    inner: Inner,
}

#[cfg(windows)]
struct Inner {
    /// Job Object handle, stored as isize so ProcessGroup is Send + Sync.
    job: isize,
}

#[cfg(unix)]
struct Inner {
    pgid: std::sync::atomic::AtomicI32,
}

impl ProcessGroup {
    pub fn new() -> Result<Self> {
        #[cfg(windows)]
        {
            Ok(ProcessGroup {
                inner: Inner {
                    job: win::create_kill_job()?,
                },
            })
        }
        #[cfg(unix)]
        {
            Ok(ProcessGroup {
                inner: Inner {
                    pgid: std::sync::atomic::AtomicI32::new(0),
                },
            })
        }
    }

    /// Spawn `cmd` as the root of this kill-group.
    pub fn spawn(&self, cmd: &mut Command) -> Result<Child> {
        #[cfg(unix)]
        {
            cmd.process_group(0); // the child becomes its own group leader
        }
        let child = cmd
            .spawn()
            .map_err(|e| AgentRelayError::new(ErrorCode::ProcessSpawnFailed, e.to_string()))?;
        #[cfg(windows)]
        {
            if let Some(pid) = child.id() {
                win::assign_pid_to_job(self.inner.job, pid)?;
            }
        }
        #[cfg(unix)]
        {
            self.inner
                .pgid
                .store(child.id().unwrap_or(0) as i32, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(child)
    }

    /// Kill the whole group (the root process and every descendant).
    pub fn kill_tree(&self) -> Result<()> {
        #[cfg(windows)]
        {
            win::terminate_job(self.inner.job)
        }
        #[cfg(unix)]
        {
            let pgid = self.inner.pgid.load(std::sync::atomic::Ordering::SeqCst);
            if pgid > 0 {
                // negative pid targets the whole process group.
                let r = unsafe { libc::kill(-pgid, libc::SIGKILL) };
                if r != 0 {
                    return Err(AgentRelayError::new(
                        ErrorCode::ProcessStopIncomplete,
                        format!("killpg {pgid} failed"),
                    ));
                }
            }
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Closing the job handle with KILL_ON_JOB_CLOSE also reaps survivors.
        win::close_handle(self.inner.job);
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, FALSE, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject, TerminateJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    fn err(e: windows::core::Error, code: ErrorCode) -> AgentRelayError {
        AgentRelayError::new(code, e.to_string())
    }

    pub fn create_kill_job() -> Result<isize> {
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null())
                .map_err(|e| err(e, ErrorCode::ProcessSpawnFailed))?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(|e| err(e, ErrorCode::ProcessSpawnFailed))?;
            Ok(job.0 as isize)
        }
    }

    pub fn assign_pid_to_job(job: isize, pid: u32) -> Result<()> {
        unsafe {
            let hproc = OpenProcess(PROCESS_TERMINATE | PROCESS_SET_QUOTA, FALSE, pid)
                .map_err(|e| err(e, ErrorCode::ProcessSpawnFailed))?;
            let r = AssignProcessToJobObject(HANDLE(job as *mut _), hproc);
            let _ = CloseHandle(hproc);
            r.map_err(|e| err(e, ErrorCode::ProcessSpawnFailed))?;
        }
        Ok(())
    }

    pub fn terminate_job(job: isize) -> Result<()> {
        unsafe {
            TerminateJobObject(HANDLE(job as *mut _), 1)
                .map_err(|e| err(e, ErrorCode::ProcessStopIncomplete))?;
        }
        Ok(())
    }

    pub fn close_handle(job: isize) {
        unsafe {
            let _ = CloseHandle(HANDLE(job as *mut _));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn kill_tree_kills_the_direct_child() {
        let group = ProcessGroup::new().unwrap();
        let mut cmd = Command::new("node");
        cmd.args(["-e", "setTimeout(()=>{}, 60000)"]);
        let mut child = group.spawn(&mut cmd).unwrap();
        sleep(Duration::from_millis(300)).await;
        group.kill_tree().unwrap();
        // The child must exit promptly now that the job was terminated.
        let waited = timeout(Duration::from_secs(5), child.wait()).await;
        assert!(waited.is_ok(), "child did not die after kill_tree");
    }

    #[tokio::test]
    async fn kill_tree_kills_a_grandchild_too() {
        let dir = std::env::temp_dir();
        let marker = dir.join(format!("agentrelay-killtest-{}.marker", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let child_js = dir.join(format!("agentrelay-child-{}.js", std::process::id()));
        let parent_js = dir.join(format!("agentrelay-parent-{}.js", std::process::id()));
        // grandchild: after 1.5s, write the marker (proof it survived).
        std::fs::write(&child_js, "setTimeout(()=>require('fs').writeFileSync(process.argv[2],'x'),1500);").unwrap();
        // root: wait 400ms (so it's already in the job), spawn the grandchild, then idle.
        std::fs::write(
            &parent_js,
            "const cp=require('child_process');setTimeout(()=>cp.spawn(process.execPath,[process.argv[2],process.argv[3]]),400);setTimeout(()=>{},60000);",
        )
        .unwrap();

        let group = ProcessGroup::new().unwrap();
        let mut cmd = Command::new("node");
        cmd.args([parent_js.to_str().unwrap(), child_js.to_str().unwrap(), marker.to_str().unwrap()]);
        let _child = group.spawn(&mut cmd).unwrap();

        sleep(Duration::from_millis(800)).await; // grandchild spawned (at 400ms) and joined the job
        group.kill_tree().unwrap();
        sleep(Duration::from_millis(2500)).await; // past the grandchild's 1.5s write

        let survived = marker.exists();
        for p in [&marker, &child_js, &parent_js] {
            let _ = std::fs::remove_file(p);
        }
        assert!(!survived, "grandchild survived kill_tree (tree kill failed)");
    }
}
