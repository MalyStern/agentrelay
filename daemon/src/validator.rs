//! Watchtower layer-2: validators (plan 00 §7 layer-2, §9). Completion is gated on
//! EVIDENCE from the system, never the model's claim that it's done — the one place
//! the plan refuses to trust the agent. A validator runs a real command (tests,
//! typecheck, build, lint, secret-scan…); its exit code is the verdict.

use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidatorKind {
    Tests,
    Typecheck,
    Build,
    Lint,
    SecretScan,
    Custom,
}

impl ValidatorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ValidatorKind::Tests => "tests",
            ValidatorKind::Typecheck => "typecheck",
            ValidatorKind::Build => "build",
            ValidatorKind::Lint => "lint",
            ValidatorKind::SecretScan => "secret_scan",
            ValidatorKind::Custom => "custom",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Validator {
    pub kind: ValidatorKind,
    /// The shell command to run; exit 0 = pass.
    pub command: String,
}

impl Validator {
    pub fn new(kind: ValidatorKind, command: impl Into<String>) -> Self {
        Validator { kind, command: command.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValidationResult {
    pub validator: String,
    pub passed: bool,
    /// Captured stdout+stderr (truncated).
    pub output: String,
    pub duration_ms: u64,
}

fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        s
    } else {
        format!("{}… [+{} bytes]", &s[..max], s.len() - max)
    }
}

async fn run_shell(command: &str, cwd: &str) -> std::io::Result<std::process::Output> {
    use tokio::process::Command;
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    };
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.current_dir(cwd).output().await
}

/// Run one validator; its process exit code is the pass/fail verdict.
pub async fn run_validator(v: &Validator, cwd: &str) -> ValidationResult {
    let start = Instant::now();
    let (passed, output) = match run_shell(&v.command, cwd).await {
        Ok(o) => {
            let mut buf = o.stdout;
            buf.extend_from_slice(&o.stderr);
            (o.status.success(), String::from_utf8_lossy(&buf).into_owned())
        }
        Err(e) => (false, e.to_string()),
    };
    ValidationResult {
        validator: v.kind.as_str().to_string(),
        passed,
        output: truncate(output, 4000),
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

/// Run all validators (sequentially — they mutate the same workspace).
pub async fn run_all(validators: &[Validator], cwd: &str) -> Vec<ValidationResult> {
    let mut out = Vec::with_capacity(validators.len());
    for v in validators {
        out.push(run_validator(v, cwd).await);
    }
    out
}

/// The completion gate: a unit is done only if EVERY validator passed.
pub fn all_passed(results: &[ValidationResult]) -> bool {
    !results.is_empty() && results.iter().all(|r| r.passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passing_command_passes() {
        let r = run_validator(&Validator::new(ValidatorKind::Tests, "node --version"), ".").await;
        assert!(r.passed);
        assert!(r.output.to_lowercase().contains("v")); // node prints e.g. v24.x
        assert_eq!(r.validator, "tests");
    }

    #[tokio::test]
    async fn failing_command_fails() {
        let r = run_validator(&Validator::new(ValidatorKind::Build, "this-command-does-not-exist-xyz-123"), ".").await;
        assert!(!r.passed);
    }

    #[tokio::test]
    async fn completion_gate_requires_all_pass() {
        let vals = vec![
            Validator::new(ValidatorKind::Tests, "node --version"),
            Validator::new(ValidatorKind::Build, "this-command-does-not-exist-xyz-123"),
        ];
        let results = run_all(&vals, ".").await;
        assert_eq!(results.len(), 2);
        assert!(!all_passed(&results)); // one failed -> not done
        // an empty set is NOT "passed" (nothing was proven).
        assert!(!all_passed(&[]));
        // all-pass case
        let ok = run_all(&[Validator::new(ValidatorKind::Tests, "node --version")], ".").await;
        assert!(all_passed(&ok));
    }
}
