//! Watchtower layer-3: drift detection (plan §ז.4, Phase-2).
//!
//! Layers 1 and 2 answer different questions from this one. Layer 1 asks "is this
//! action allowed?" — one tool call at a time. Layer 2 asks "did it work?" — a test
//! suite's exit code. Neither notices the failure that actually ruins an overnight
//! run: an agent that does allowed things, that pass, **on the wrong problem**.
//!
//! You ask for a bug fix in the parser and wake up to a refactored build system.
//! Every command was permitted. Every test passed. The night is still wasted, and
//! reviewing it costs more than doing the work yourself would have.
//!
//! Drift is measured against what the unit said it would touch, because that is the
//! only statement of intent that exists before the work starts. The check is
//! deliberately conservative: it reports, it does not stop. A supervisor that halts
//! on a heuristic will halt on legitimate work, and a false stop at 3am costs a
//! whole night. Layers 1 and 2 are the ones allowed to be decisive, because they
//! are the ones that are certain.

use serde::{Deserialize, Serialize};

/// What a unit declared it was going to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UnitScope {
    /// Globs the unit expects to touch. Empty means "unstated", and an unstated
    /// scope cannot drift — we will not invent an intent to measure against.
    #[serde(default)]
    pub touches: Vec<String>,
    /// Rough size expectation. A one-line fix that rewrites 40 files is drift even
    /// if every file was in scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<usize>,
}

impl UnitScope {
    pub fn is_stated(&self) -> bool {
        !self.touches.is_empty() || self.max_files.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum DriftSignal {
    /// A file outside every declared glob.
    OutOfScope { path: String },
    /// More files than the unit said it would touch.
    TooManyFiles { changed: usize, expected: usize },
    /// Files whose names suggest a different kind of work entirely.
    UnrelatedArea { path: String, why: String },
}

impl DriftSignal {
    pub fn describe(&self) -> String {
        match self {
            DriftSignal::OutOfScope { path } => {
                format!("{path} is outside everything this unit said it would touch")
            }
            DriftSignal::TooManyFiles { changed, expected } => {
                format!("{changed} files changed; the unit expected at most {expected}")
            }
            DriftSignal::UnrelatedArea { path, why } => format!("{path}: {why}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DriftReport {
    pub signals: Vec<DriftSignal>,
    /// True when no scope was declared — so "no drift found" cannot be mistaken for
    /// "we checked and it was fine".
    pub unchecked: bool,
}

impl DriftReport {
    pub fn has_drift(&self) -> bool {
        !self.signals.is_empty()
    }

    /// What to put in the morning report. Says explicitly when nothing was checked.
    pub fn summary(&self) -> String {
        if self.unchecked {
            return "Drift was not checked: this unit did not say what it would touch.".to_string();
        }
        if self.signals.is_empty() {
            return "The changes stayed inside what this unit said it would touch.".to_string();
        }
        let mut s = format!(
            "{} sign(s) that the work drifted from the task:",
            self.signals.len()
        );
        for d in &self.signals {
            s.push_str(&format!("\n  - {}", d.describe()));
        }
        s.push_str(
            "\nThis is a heuristic, not a verdict — the work was not stopped. Read the diff \
             before trusting it.",
        );
        s
    }
}

/// Areas whose names say the work is of a different kind. Deliberately short: each
/// entry is a place where an unexpected change is a genuine signal, not a place
/// where changes are unusual.
fn unrelated_area(path: &str) -> Option<String> {
    let p = path.replace('\\', "/").to_ascii_lowercase();
    let hits: &[(&str, &str)] = &[
        (
            ".github/workflows/",
            "changes how CI runs, which is not usually part of a code task",
        ),
        ("dockerfile", "changes how the project is built or shipped"),
        ("docker-compose", "changes how services are wired"),
        (".env", "touches configuration that usually holds secrets"),
        ("package.json", "changes dependencies or scripts"),
        ("cargo.toml", "changes dependencies"),
        ("go.mod", "changes dependencies"),
        ("requirements.txt", "changes dependencies"),
        (".gitignore", "changes what git tracks"),
    ];
    hits.iter()
        .find(|(needle, _)| p.contains(needle))
        .map(|(_, why)| (*why).to_string())
}

fn glob_matches(glob: &str, path: &str) -> bool {
    crate::guard::glob_to_regex(glob).is_match(&path.replace('\\', "/"))
}

/// Compare what changed against what the unit said it would change.
pub fn detect(scope: &UnitScope, changed_paths: &[String]) -> DriftReport {
    if !scope.is_stated() {
        // No intent was declared, so there is nothing to drift from. Saying "no
        // drift" here would be a claim we did not earn.
        return DriftReport {
            signals: vec![],
            unchecked: true,
        };
    }

    let mut signals = Vec::new();

    if let Some(max) = scope.max_files {
        if changed_paths.len() > max {
            signals.push(DriftSignal::TooManyFiles {
                changed: changed_paths.len(),
                expected: max,
            });
        }
    }

    for path in changed_paths {
        // Our own directory is not the user's work and never counts as drift.
        if path.replace('\\', "/").starts_with(".agentrelay/") {
            continue;
        }
        let in_scope =
            scope.touches.is_empty() || scope.touches.iter().any(|g| glob_matches(g, path));
        if !in_scope {
            signals.push(DriftSignal::OutOfScope { path: path.clone() });
            // Already reported; a second signal for the same file is noise.
            continue;
        }
        if let Some(why) = unrelated_area(path) {
            signals.push(DriftSignal::UnrelatedArea {
                path: path.clone(),
                why,
            });
        }
    }

    DriftReport {
        signals,
        unchecked: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_unstated_scope_is_reported_as_unchecked_not_as_clean() {
        // "We looked and found nothing" and "we never looked" must not read the
        // same in the morning.
        let r = detect(&UnitScope::default(), &paths(&["src/anything.rs"]));
        assert!(!r.has_drift());
        assert!(r.unchecked);
        assert!(r.summary().contains("not checked"), "{}", r.summary());
        assert!(!r.summary().contains("stayed inside"));
    }

    #[test]
    fn work_inside_the_declared_scope_is_clean() {
        let scope = UnitScope {
            touches: paths(&["src/parser/**"]),
            max_files: None,
        };
        let r = detect(
            &scope,
            &paths(&["src/parser/lexer.rs", "src/parser/mod.rs"]),
        );
        assert!(!r.has_drift(), "{:?}", r.signals);
        assert!(!r.unchecked);
        assert!(r.summary().contains("stayed inside"));
    }

    #[test]
    fn the_night_that_fixed_the_wrong_thing_is_caught() {
        // The failure this layer exists for: every command allowed, every test
        // green, and the wrong problem solved.
        let scope = UnitScope {
            touches: paths(&["src/parser/**"]),
            max_files: None,
        };
        let r = detect(
            &scope,
            &paths(&[
                "src/parser/lexer.rs",
                "build/webpack.config.js",
                "scripts/deploy.sh",
            ]),
        );
        assert!(r.has_drift());
        let text = r.summary();
        assert!(text.contains("webpack.config.js"), "{text}");
        assert!(text.contains("deploy.sh"), "{text}");
        assert!(text.contains("outside everything this unit said"), "{text}");
    }

    #[test]
    fn a_small_task_that_rewrote_the_repo_is_drift_even_when_every_file_is_in_scope() {
        let scope = UnitScope {
            touches: paths(&["src/**"]),
            max_files: Some(3),
        };
        let changed: Vec<String> = (0..40).map(|i| format!("src/f{i}.rs")).collect();
        let r = detect(&scope, &changed);
        match r.signals.first() {
            Some(DriftSignal::TooManyFiles { changed, expected }) => {
                assert_eq!(*changed, 40);
                assert_eq!(*expected, 3);
            }
            other => panic!("expected a size signal, got {other:?}"),
        }
    }

    #[test]
    fn touching_ci_or_dependencies_is_flagged_even_when_technically_in_scope() {
        // A task allowed to touch everything still should not quietly rewrite how
        // CI runs or what the project depends on.
        let scope = UnitScope {
            touches: paths(&["**"]),
            max_files: None,
        };
        let r = detect(
            &scope,
            &paths(&[".github/workflows/ci.yml", "package.json", "src/ok.rs"]),
        );
        let text = r.summary();
        assert!(text.contains("changes how CI runs"), "{text}");
        assert!(text.contains("changes dependencies or scripts"), "{text}");
        assert!(
            !text.contains("src/ok.rs"),
            "ordinary code is not flagged: {text}"
        );
    }

    #[test]
    fn our_own_directory_is_never_drift() {
        let scope = UnitScope {
            touches: paths(&["src/**"]),
            max_files: None,
        };
        let r = detect(&scope, &paths(&[".agentrelay/config.json", "src/a.rs"]));
        assert!(!r.has_drift(), "{:?}", r.signals);
    }

    #[test]
    fn a_file_outside_scope_is_reported_once_not_twice() {
        // package.json is both out of scope AND a flagged area. Two lines about one
        // file is noise, and noise is how a real signal gets skimmed past.
        let scope = UnitScope {
            touches: paths(&["src/**"]),
            max_files: None,
        };
        let r = detect(&scope, &paths(&["package.json"]));
        assert_eq!(r.signals.len(), 1, "{:?}", r.signals);
    }

    #[test]
    fn drift_reports_and_never_stops_the_run() {
        // Layer 3 is a heuristic. A false stop at 3am costs a whole night, so being
        // decisive is left to the layers that are certain.
        let scope = UnitScope {
            touches: paths(&["src/**"]),
            max_files: None,
        };
        let r = detect(&scope, &paths(&["elsewhere/x.rs"]));
        assert!(r.has_drift());
        assert!(
            r.summary().contains("heuristic, not a verdict"),
            "{}",
            r.summary()
        );
        assert!(
            r.summary().contains("the work was not stopped"),
            "{}",
            r.summary()
        );
    }

    #[test]
    fn scope_round_trips_through_config() {
        let s = UnitScope {
            touches: paths(&["src/**", "tests/**"]),
            max_files: Some(10),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<UnitScope>(&json).unwrap(), s);
        assert!(!serde_json::from_str::<UnitScope>("{}").unwrap().is_stated());
    }
}
