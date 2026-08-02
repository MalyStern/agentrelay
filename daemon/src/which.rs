//! Finding and launching the agent's executable.
//!
//! This exists because of a bug that made the whole product unusable on Windows
//! and was invisible until someone actually installed Claude Code.
//!
//! `npm install -g @anthropic-ai/claude-code` puts three files on PATH:
//! `claude` (a shell script), `claude.cmd`, and `claude.ps1`. There is **no
//! `claude.exe`**. Rust's `Command::new("claude")` goes through `CreateProcessW`,
//! which searches PATH but only ever appends `.exe` — so it finds nothing, and the
//! daemon reports that Claude is not installed on a machine where it plainly is.
//! Even once found, a `.cmd` cannot be executed directly; it needs `cmd.exe`.
//!
//! So: resolve the program the way the shell would (PATHEXT on Windows), and route
//! a batch file through the interpreter that can run it.

use std::path::{Path, PathBuf};

/// Extensions the Windows shell would try, in order. `PATHEXT` is authoritative
/// when set; the fallback is what a default install has.
#[cfg(windows)]
fn path_extensions() -> Vec<String> {
    let raw = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut exts: Vec<String> = raw
        .split(';')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    // npm's launcher is a .cmd; make sure it is tried even if PATHEXT is unusual.
    for needed in [".cmd", ".bat", ".exe"] {
        if !exts.iter().any(|e| e == needed) {
            exts.push(needed.to_string());
        }
    }
    // An extensionless match (the shell script npm also writes) is last resort.
    exts.push(String::new());
    exts
}

#[cfg(unix)]
fn path_extensions() -> Vec<String> {
    vec![String::new()]
}

/// Where this program actually lives, or None if it is not on PATH.
pub fn resolve(program: &str) -> Option<PathBuf> {
    // An explicit path is used as given — that is what the user meant.
    if program.contains('/') || program.contains('\\') {
        let p = PathBuf::from(program);
        return if p.is_file() { Some(p) } else { None };
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in path_extensions() {
            let candidate = dir.join(format!("{program}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// True when the program can actually be launched.
pub fn is_available(program: &str) -> bool {
    resolve(program).is_some()
}

/// A command that will really start `program`, whatever kind of launcher it is.
///
/// Returns None when the program is not on PATH, so the caller can say "not
/// installed" instead of failing later with a confusing spawn error.
pub fn command_for(program: &str) -> Option<tokio::process::Command> {
    let resolved = resolve(program)?;

    #[cfg(windows)]
    {
        let ext = resolved
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "cmd" || ext == "bat" {
            // A batch file is not an executable; only cmd.exe can run it. `/C` runs
            // it and exits. Arguments the caller adds afterwards are passed through
            // normally, which is what every caller here expects.
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(&resolved);
            return Some(c);
        }
        if ext == "ps1" {
            let mut c = tokio::process::Command::new("powershell");
            c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(&resolved);
            return Some(c);
        }
    }

    Some(tokio::process::Command::new(resolved))
}

/// The same, for the synchronous std API (the doctor's version check).
pub fn std_command_for(program: &str) -> Option<std::process::Command> {
    let resolved = resolve(program)?;

    #[cfg(windows)]
    {
        let ext = resolved
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "cmd" || ext == "bat" {
            let mut c = std::process::Command::new("cmd");
            c.arg("/C").arg(&resolved);
            return Some(c);
        }
        if ext == "ps1" {
            let mut c = std::process::Command::new("powershell");
            c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(&resolved);
            return Some(c);
        }
    }

    Some(std::process::Command::new(resolved))
}

/// Advice for a program we could not find, naming the thing the user would type.
pub fn install_hint(program: &str) -> String {
    match program {
        "claude" => "not found on PATH — install with `npm install -g @anthropic-ai/claude-code`, \
                     then open a new terminal so PATH is refreshed"
            .to_string(),
        "git" => "not found on PATH — install Git and reopen your terminal".to_string(),
        "node" => "not found on PATH — install Node.js 18 or newer".to_string(),
        other => format!("`{other}` was not found on PATH"),
    }
}

/// Absolute path of a resolved program, for display.
pub fn describe(program: &str) -> String {
    match resolve(program) {
        Some(p) => p.display().to_string(),
        None => install_hint(program),
    }
}

pub fn exists_at(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_exists_is_found() {
        // node is required by this project's own test suite, so it is present.
        let p = resolve("node").expect("node must be on PATH for these tests");
        assert!(p.is_file(), "{p:?}");
        assert!(is_available("node"));
    }

    #[test]
    fn a_program_that_does_not_exist_is_not_invented() {
        assert!(resolve("definitely-not-a-real-program-xyz-123").is_none());
        assert!(!is_available("definitely-not-a-real-program-xyz-123"));
        assert!(command_for("definitely-not-a-real-program-xyz-123").is_none());
    }

    #[tokio::test]
    async fn a_resolved_program_actually_runs() {
        let mut cmd = command_for("node").expect("node resolves");
        let out = cmd.arg("--version").output().await.unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).starts_with('v'));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_cmd_launcher_is_run_through_cmd_exe_not_spawned_directly() {
        // The exact shape npm uses for `claude`: a .cmd with no .exe beside it.
        // Command::new on that path fails with "%1 is not a valid Win32
        // application", which is how the daemon concluded Claude was not installed
        // on a machine where it was.
        let dir = std::env::temp_dir().join(format!("ar-which-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bat = dir.join("ar-fake-agent.cmd");
        std::fs::write(&bat, "@echo off\r\necho fake-agent-ran\r\n").unwrap();

        // Put it on PATH exactly as npm's global bin dir would be.
        let old = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![dir.clone()];
        dirs.extend(std::env::split_paths(&old));
        std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());

        let resolved = resolve("ar-fake-agent").expect("a .cmd on PATH must be found");
        assert_eq!(resolved.extension().unwrap(), "cmd");

        let out = command_for("ar-fake-agent").unwrap().output().await.unwrap();
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("fake-agent-ran"),
            "the launcher did not actually run"
        );

        std::env::set_var("PATH", old);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_program_gets_advice_naming_the_command_to_run() {
        let hint = install_hint("claude");
        assert!(hint.contains("npm install -g @anthropic-ai/claude-code"), "{hint}");
        // The PATH-refresh step is the one people miss on Windows.
        assert!(hint.contains("new terminal"), "{hint}");
    }
}
