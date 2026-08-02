//! Mission templates for non-technical users (plan §יד Phase-4: "מצב-פשוט +
//! Templates").
//!
//! A safe Config is not obvious to assemble. It is guards AND validators AND a
//! scope AND a budget cap, each of which the person who most needs the safety is
//! least equipped to reason about. A template is a named, complete starting point —
//! "fix-failing-tests", "safe-refactor", "dependency-update" — so that someone who
//! could not write the JSON by hand still gets guards that block a push, a
//! completion gate that trusts evidence over the model's word, a scope drift is
//! measured against, and — the one that matters most for an unattended run — a
//! spending cap.
//!
//! Two rules this module keeps:
//!
//! * **Every template sets a budget.** The whole point is autopilot for someone who
//!   will not be watching, and the cost kill-switch is the only guard protecting a
//!   thing git cannot restore. A template without one would be the most dangerous
//!   config we ship, handed to the least prepared user.
//! * **A template's own output must load in its own loader.** This is the lastgood
//!   lesson: a tool whose output its own loader rejects is worse than no tool. Every
//!   template round-trips through the real `Daemon` config loader in a test — if one
//!   ever produced JSON the daemon could not read, the suite goes red.
//!
//! The one thing a template cannot know is your project's test command. So the
//! validator commands here are *examples* (`npm test`), and that is stated out
//! loud — `review_notes()` says to set them, and the CLI repeats it. A validator
//! that does not actually run your tests would pass a night on no evidence at all,
//! which is exactly the failure the completion gate exists to prevent; presenting a
//! guessed command as if it were yours would be the fail-quiet shape in a new place.

use crate::cost::Budget;
use crate::daemon::Config;
use crate::drift::UnitScope;
use crate::error::{AgentRelayError, ErrorCode, Result};
use crate::provider::{Guard, GuardKind};
use crate::validator::{Validator, ValidatorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    FixFailingTests,
    SafeRefactor,
    DependencyUpdate,
}

/// A named starting configuration.
#[derive(Debug, Clone, Copy)]
pub struct Template {
    pub name: &'static str,
    /// One line for the `templates` list.
    pub summary: &'static str,
    kind: Kind,
}

/// The safe guards every template starts from: no push, no deploy, and the agent
/// kept inside the workspace. These are the daemon's own defaults — a template
/// loosens or adds to them, never drops below them.
fn safe_guards() -> Vec<Guard> {
    vec![
        Guard::new(GuardKind::NoPush),
        Guard::new(GuardKind::NoDeploy),
        Guard::new(GuardKind::WorkspaceBoundary),
    ]
}

/// A cap for an unattended run. Modest on purpose: a template user is the least
/// likely to be watching, so the ceiling is low enough that a runaway is caught
/// while it is still cheap.
fn autopilot_budget() -> Budget {
    Budget {
        mission_usd: Some(5.0),
        turn_usd: Some(1.0),
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

impl Template {
    /// Every built-in template.
    pub fn all() -> Vec<Template> {
        vec![
            Template {
                name: "fix-failing-tests",
                summary: "Make a red test suite green without loosening the tests or the scope.",
                kind: Kind::FixFailingTests,
            },
            Template {
                name: "safe-refactor",
                summary: "Restructure code with the tests as a safety net; tight scope, no new dependencies.",
                kind: Kind::SafeRefactor,
            },
            Template {
                name: "dependency-update",
                summary: "Bump dependencies and prove the build and tests still pass.",
                kind: Kind::DependencyUpdate,
            },
        ]
    }

    /// Look a template up by name. An unknown name is refused with the list of valid
    /// ones — the codebase's standard for a bad id, never a silent nearest-guess.
    pub fn for_name(name: &str) -> Result<Template> {
        Self::all()
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                let valid = Self::all()
                    .iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join(", ");
                AgentRelayError::new(
                    ErrorCode::Internal,
                    format!(
                        "unknown template \"{name}\". Valid templates: {valid}. Run \
                     `agentrelay templates` to see what each one sets."
                    ),
                )
            })
    }

    /// The complete Config this template stands for — guards, validators, scope and
    /// budget, ready to write to `.agentrelay/config.json`.
    pub fn to_config(&self) -> Config {
        let mut cfg = Config {
            budget: autopilot_budget(),
            ..Config::default()
        };
        match self.kind {
            Kind::FixFailingTests => {
                cfg.guards = safe_guards();
                cfg.validators = vec![Validator::named(ValidatorKind::Tests, "tests", "npm test")];
                // A test fix touches source and tests; it should not rewrite the
                // whole tree, so a size ceiling makes a runaway visible as drift.
                cfg.scope = UnitScope {
                    touches: strings(&["src/**", "tests/**", "test/**"]),
                    max_files: Some(20),
                };
            }
            Kind::SafeRefactor => {
                // A refactor must not quietly pull in new dependencies — that is a
                // behaviour change wearing a cleanup's clothes.
                let mut guards = safe_guards();
                guards.push(Guard::new(GuardKind::NoDependency));
                cfg.guards = guards;
                cfg.validators = vec![
                    Validator::named(ValidatorKind::Tests, "tests", "npm test"),
                    Validator::named(ValidatorKind::Typecheck, "typecheck", "npm run typecheck"),
                ];
                // Refactors balloon; a small ceiling turns a "quick cleanup" that
                // rewrote forty files into a drift signal in the morning.
                cfg.scope = UnitScope {
                    touches: strings(&["src/**"]),
                    max_files: Some(10),
                };
            }
            Kind::DependencyUpdate => {
                cfg.guards = safe_guards();
                cfg.validators = vec![
                    Validator::named(ValidatorKind::Build, "build", "npm run build"),
                    Validator::named(ValidatorKind::Tests, "tests", "npm test"),
                ];
                // The manifests and lockfiles this work is allowed to touch. Anything
                // outside them, in a dependency bump, is worth a second look.
                cfg.scope = UnitScope {
                    touches: strings(&[
                        "package.json",
                        "package-lock.json",
                        "yarn.lock",
                        "pnpm-lock.yaml",
                        "Cargo.toml",
                        "Cargo.lock",
                        "requirements.txt",
                        "go.mod",
                        "go.sum",
                    ]),
                    max_files: Some(15),
                };
            }
        }
        cfg
    }

    /// What the user must review before an autopilot run — chiefly that the validator
    /// commands are examples, not their project's real ones. Shown by the CLI so the
    /// guess is never mistaken for a fact.
    pub fn review_notes(&self) -> Vec<String> {
        let mut notes = vec![
            "The validator commands are EXAMPLES (e.g. `npm test`). Set them to your project's \
             real test/build commands in .agentrelay/config.json before relying on this — a \
             validator that does not run your tests proves nothing, and the run would checkpoint \
             on no evidence."
                .to_string(),
        ];
        match self.kind {
            Kind::FixFailingTests => notes.push(
                "The scope allows src/ and tests/. If your tests live elsewhere, widen `scope.touches`."
                    .to_string(),
            ),
            Kind::SafeRefactor => notes.push(
                "New dependencies are blocked (a refactor should not add any) and the file ceiling \
                 is low. Raise `scope.max_files` if the refactor is genuinely large."
                    .to_string(),
            ),
            Kind::DependencyUpdate => notes.push(
                "The scope lists common manifest/lockfiles. Remove the ones your project does not \
                 use, or add the ones it does."
                    .to_string(),
            ),
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "ar-templates-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn every_template_produces_a_config_the_real_loader_accepts_and_reproduces() {
        // The lastgood lesson, pinned: a template whose output its own loader could
        // not read would be worse than no template. This writes each template's
        // config to the real file and opens it through the real Daemon loader, then
        // checks the loaded config serialises identically — a true round-trip, not a
        // shape check.
        use crate::daemon::Daemon;
        for t in Template::all() {
            let root = tmp();
            std::fs::create_dir_all(root.join(".agentrelay")).unwrap();
            let produced = t.to_config();
            let json = serde_json::to_string_pretty(&produced).unwrap();
            std::fs::write(root.join(".agentrelay/config.json"), &json).unwrap();

            let d = Daemon::open(&root).unwrap_or_else(|e| {
                panic!(
                    "template `{}` produced a config the loader rejected: {e}",
                    t.name
                )
            });

            let reloaded = serde_json::to_value(d.config()).unwrap();
            let original = serde_json::to_value(&produced).unwrap();
            assert_eq!(
                reloaded, original,
                "template `{}` did not round-trip",
                t.name
            );
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn every_template_sets_a_spending_cap_because_the_user_is_not_watching() {
        // A template is autopilot for someone who will not be at the keyboard. The
        // cost cap is the only guard over a thing git cannot restore, so shipping one
        // without it would be the most dangerous config we hand out.
        for t in Template::all() {
            assert!(
                t.to_config().budget.is_set(),
                "template `{}` has no budget",
                t.name
            );
        }
    }

    #[test]
    fn every_template_requires_evidence_and_keeps_the_safe_guards() {
        // No validators = a night that checkpoints on the model's word. And no
        // template may drop below the no-push / no-deploy / workspace-boundary floor.
        for t in Template::all() {
            let cfg = t.to_config();
            assert!(
                !cfg.validators.is_empty(),
                "template `{}` gates completion on nothing",
                t.name
            );
            let kinds: Vec<GuardKind> = cfg.guards.iter().map(|g| g.kind).collect();
            for required in [
                GuardKind::NoPush,
                GuardKind::NoDeploy,
                GuardKind::WorkspaceBoundary,
            ] {
                assert!(
                    kinds.contains(&required),
                    "template `{}` dropped {:?}",
                    t.name,
                    required
                );
            }
            // A stated scope, so drift is actually measured rather than "unchecked".
            assert!(
                cfg.scope.is_stated(),
                "template `{}` has no scope for drift to measure",
                t.name
            );
        }
    }

    #[test]
    fn an_unknown_template_is_refused_and_lists_the_valid_ones() {
        let err = Template::for_name("make-it-fast").unwrap_err();
        assert!(err.message.contains("unknown template"), "{}", err.message);
        // The valid names are in the message, so the user can fix the typo without
        // reading source.
        for t in Template::all() {
            assert!(
                err.message.contains(t.name),
                "{} not listed: {}",
                t.name,
                err.message
            );
        }
        // Names match case-insensitively, but a real name resolves.
        assert_eq!(
            Template::for_name("Fix-Failing-Tests").unwrap().name,
            "fix-failing-tests"
        );
    }

    #[test]
    fn the_refactor_template_blocks_new_dependencies() {
        // The property that distinguishes this template: a refactor that adds a
        // dependency is a behaviour change, not a cleanup.
        let cfg = Template::for_name("safe-refactor").unwrap().to_config();
        assert!(cfg.guards.iter().any(|g| g.kind == GuardKind::NoDependency));
        assert!(
            cfg.scope.max_files.unwrap() <= 10,
            "a refactor's file ceiling is deliberately tight"
        );
    }

    #[test]
    fn the_review_notes_say_the_commands_are_examples() {
        // The one thing a template cannot know is the project's real test command, so
        // it must not present its guess as fact.
        for t in Template::all() {
            let notes = t.review_notes();
            assert!(
                notes.iter().any(|n| n.contains("EXAMPLES")),
                "template `{}`: {:?}",
                t.name,
                notes
            );
        }
    }

    #[test]
    fn the_names_are_unique() {
        let mut names: Vec<&str> = Template::all().iter().map(|t| t.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate template name");
    }
}
