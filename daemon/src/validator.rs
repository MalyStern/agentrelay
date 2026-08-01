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
    /// Optional label. Two `custom` validators are indistinguishable in a report
    /// without one, so the name (when given) is what gets shown and persisted.
    #[serde(default)]
    pub name: Option<String>,
}

impl Validator {
    pub fn new(kind: ValidatorKind, command: impl Into<String>) -> Self {
        Validator { kind, command: command.into(), name: None }
    }

    pub fn named(kind: ValidatorKind, name: impl Into<String>, command: impl Into<String>) -> Self {
        Validator { kind, command: command.into(), name: Some(name.into()) }
    }

    /// What to show the user: the label if there is one, else the kind.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.kind.as_str().to_string())
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
    // On Windows the command must be handed to cmd.exe VERBATIM. Rust's normal
    // argument handling quotes and escapes each arg for a C-runtime parser, which
    // cmd does not use — so a perfectly ordinary validator like
    //   npm test -- --grep "my test"
    // arrives with mangled quotes and fails for reasons that have nothing to do
    // with the user's tests. `raw_arg` bypasses that escaping, which is exactly
    // what a shell string needs.
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("cmd");
        c.raw_arg("/C ");
        c.raw_arg(command);
        tokio::process::Command::from(c)
    };
    #[cfg(unix)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
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
    // Compress rather than truncate. A test suite prints its failures in the
    // middle and its summary at the end, so cutting at 4000 bytes reliably keeps
    // the part that says nothing and drops the part that says what broke.
    let output = if output.len() > 4000 {
        crate::context::compress_output(&output, 5, 15, 40).text
    } else {
        output
    };

    ValidationResult {
        validator: v.label(),
        passed,
        output: truncate(output, 16_000),
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
    async fn a_named_validator_reports_its_name_not_its_kind() {
        let v = Validator::named(ValidatorKind::Custom, "no-todo-left", "node --version");
        let r = run_validator(&v, ".").await;
        assert_eq!(r.validator, "no-todo-left");
        // …and an unnamed one still falls back to the kind.
        assert_eq!(Validator::new(ValidatorKind::Custom, "x").label(), "custom");
    }

    #[test]
    fn name_is_optional_in_config_json() {
        let v: Validator =
            serde_json::from_str(r#"{"kind":"tests","command":"npm test"}"#).unwrap();
        assert_eq!(v.label(), "tests");
        let v: Validator =
            serde_json::from_str(r#"{"kind":"custom","name":"docs","command":"x"}"#).unwrap();
        assert_eq!(v.label(), "docs");
    }

    #[tokio::test]
    async fn a_command_with_quoted_arguments_runs_as_written() {
        // `npm test -- --grep "my test"` is an entirely ordinary validator. If the
        // shell string is escaped as if it were a single argv entry, the quotes
        // reach the program mangled and it fails for reasons unrelated to the code.
        let dir = std::env::temp_dir().join(format!("ar-quote-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("echo arg.js");
        std::fs::write(&script, "console.log('ARG=' + process.argv[2]);\n").unwrap();

        let v = Validator::new(
            ValidatorKind::Custom,
            // A quoted path WITH A SPACE, and a quoted argument containing one.
            &format!("node \"{}\" \"two words\"", script.display()),
        );
        let r = run_validator(&v, ".").await;
        assert!(r.passed, "the command should just run: {}", r.output);
        assert!(r.output.contains("ARG=two words"), "got: {}", r.output);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_huge_failing_log_keeps_the_failure_not_just_the_first_4kb() {
        // The bug this prevents: plain truncation keeps the banner and throws away
        // the reason the build broke, leaving the agent to guess.
        // Written to a file rather than passed with -e: quoting a script through
        // cmd /C on Windows mangles it, and a test that silently runs nothing would
        // have "passed" here by finding no output to complain about.
        let dir = std::env::temp_dir().join(format!("ar-vlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("noisy.js");
        std::fs::write(
            &script,
            "for (let i = 0; i < 3000; i++) console.log('  ok ' + i + ' - passing test with padding');\n\
             console.log('FAIL src/thing.test.ts > the important one');\n\
             console.log('Tests: 1 failed, 3000 passed');\n\
             process.exit(1);\n",
        )
        .unwrap();

        let v = Validator::new(ValidatorKind::Tests, &format!("node \"{}\"", script.display()));
        let r = run_validator(&v, ".").await;

        assert!(!r.passed);
        assert!(r.output.contains("FAIL src/thing.test.ts"), "the failure survives");
        assert!(r.output.contains("Tests: 1 failed"), "and the summary line");
        assert!(r.output.contains("omitted"), "and it admits what it cut");
        assert!(r.output.len() < 20_000, "while staying small: {} bytes", r.output.len());
        let _ = std::fs::remove_dir_all(&dir);
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
