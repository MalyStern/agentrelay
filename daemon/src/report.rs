//! The morning report (plan §14 Phase-1: "דוח-בוקר (provenance)").
//!
//! You left it running and went to bed. The question at 8am is not "what did the
//! agent say it did" — it is "what actually happened, and can I trust it". So every
//! line here is sourced, and the source is named:
//!
//! * **Verified** — a validator ran and its exit code said so. Evidence.
//! * **Recorded** — the supervisor observed it (a commit exists, a guard fired).
//! * **Estimated** — a number we computed from a rule of thumb, not a measurement.
//!
//! Nothing the model *claimed* appears as fact. That distinction is the whole
//! point of the report: an overnight run you cannot audit is an overnight run you
//! have to redo by hand.

use crate::store::Store;

#[derive(Debug, Clone, PartialEq)]
pub enum Provenance {
    /// A command ran and its exit code decided this.
    Verified,
    /// The supervisor observed it directly (git, a guard, the process table).
    Recorded,
    /// Computed from a heuristic. Never to be read as a measurement.
    Estimated,
}

impl Provenance {
    pub fn tag(&self) -> &'static str {
        match self {
            Provenance::Verified => "VERIFIED",
            Provenance::Recorded => "RECORDED",
            Provenance::Estimated => "ESTIMATED",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub provenance: Provenance,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MorningReport {
    pub mission_id: String,
    pub lines: Vec<Line>,
    /// Units whose validators all passed and which produced a checkpoint.
    pub units_completed: usize,
    /// Units where a validator failed — work left in the tree for the human.
    pub units_failed: usize,
    pub checkpoints: Vec<(String, String)>,
    /// True when there is nothing at all to report — said plainly rather than
    /// dressed up as a successful quiet night.
    pub nothing_happened: bool,
}

impl MorningReport {
    /// Plain text, for a terminal or a push notification.
    pub fn render(&self) -> String {
        let mut out = format!("AgentRelay — overnight report ({})\n\n", self.mission_id);
        if self.nothing_happened {
            out.push_str(
                "Nothing ran. No units were completed, no checkpoints were made, and no\n\
                 validators reported. If you expected work overnight, something stopped\n\
                 before it started — check `agentrelay status`.\n",
            );
            return out;
        }
        for l in &self.lines {
            out.push_str(&format!("  [{}] {}\n", l.provenance.tag(), l.text));
        }
        out.push_str("\nEvery line above is labelled by where it came from. VERIFIED means a\n");
        out.push_str("command ran and its exit code decided it. ESTIMATED is a rule of thumb.\n");
        out.push_str("Nothing the agent merely claimed appears here at all.\n");
        out
    }

    /// One line, for a phone. The headline a human needs before opening a laptop.
    pub fn headline(&self) -> String {
        if self.nothing_happened {
            return "AgentRelay: nothing ran overnight".to_string();
        }
        if self.units_failed > 0 {
            format!(
                "AgentRelay: {} unit(s) done, {} FAILED — work left uncommitted for you",
                self.units_completed, self.units_failed
            )
        } else {
            format!(
                "AgentRelay: {} unit(s) done, all validators passed",
                self.units_completed
            )
        }
    }
}

/// Build the report from what was actually recorded — the store and git, never a
/// model's account of the night.
pub fn build(store: &Store, mission_id: &str, cwd: &str, saved_bytes: usize) -> MorningReport {
    let mut lines = Vec::new();

    let checkpoints = crate::checkpoint::list(cwd, 50).unwrap_or_default();
    let units = store.list_work_units(mission_id).unwrap_or_default();

    let mut completed = 0usize;
    let mut failed = 0usize;
    for u in &units {
        let validations = store.get_validations(&u.id).unwrap_or_default();
        if validations.is_empty() {
            continue;
        }
        let all_passed = validations.iter().all(|(_, passed)| *passed);
        if all_passed {
            completed += 1;
            lines.push(Line {
                provenance: Provenance::Verified,
                text: format!(
                    "{}: {} check(s) passed ({})",
                    u.id,
                    validations.len(),
                    validations.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
                ),
            });
        } else {
            failed += 1;
            let broken: Vec<&str> =
                validations.iter().filter(|(_, p)| !*p).map(|(n, _)| n.as_str()).collect();
            lines.push(Line {
                provenance: Provenance::Verified,
                text: format!(
                    "{}: FAILED — {} did not pass. This work was NOT committed; it is still \
                     in your working tree",
                    u.id,
                    broken.join(", ")
                ),
            });
        }
    }

    for (sha, id) in &checkpoints {
        lines.push(Line {
            provenance: Provenance::Recorded,
            text: format!("checkpoint {} ({}) — `git show {}` to read it", &sha[..7.min(sha.len())], id, &sha[..7.min(sha.len())]),
        });
    }

    if let Ok(dirty) = crate::checkpoint::dirty_paths(cwd) {
        if !dirty.is_empty() {
            lines.push(Line {
                provenance: Provenance::Recorded,
                text: format!(
                    "{} file(s) left uncommitted in the working tree: {}",
                    dirty.len(),
                    dirty.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                ),
            });
        }
    }

    if saved_bytes > 0 {
        lines.push(Line {
            provenance: Provenance::Estimated,
            text: format!(
                "~{} KB of file content was not re-sent because the files were unchanged \
                 (~{} tokens, at a rough 4 bytes per token)",
                saved_bytes / 1024,
                saved_bytes / 4
            ),
        });
    }

    let nothing_happened = lines.is_empty();
    MorningReport {
        mission_id: mission_id.to_string(),
        lines,
        units_completed: completed,
        units_failed: failed,
        checkpoints,
        nothing_happened,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, WorkUnitRow};
    use crate::validator::ValidationResult;

    fn unit(id: &str) -> WorkUnitRow {
        WorkUnitRow {
            id: id.to_string(),
            mission_id: "m1".to_string(),
            seq: None,
            title: Some(id.to_string()),
            contract: serde_json::json!({}),
            worker: None,
            status: Some("done".to_string()),
            worktree_path: None,
        }
    }

    fn result(name: &str, passed: bool) -> ValidationResult {
        ValidationResult {
            validator: name.to_string(),
            passed,
            output: String::new(),
            duration_ms: 1,
        }
    }

    fn tmp_repo() -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ar-report-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.to_str().unwrap().to_string();
        let g = |a: &[&str]| std::process::Command::new("git").args(a).current_dir(&p).output().unwrap();
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("seed.txt"), "s\n").unwrap();
        g(&["add", "-A"]);
        g(&["-c", "user.name=T", "-c", "user.email=t@e.com", "commit", "-q", "-m", "base"]);
        p
    }

    #[test]
    fn a_quiet_night_says_so_instead_of_looking_successful() {
        // The failure mode this guards: an empty report reading as "all good".
        let store = Store::open_in_memory().unwrap();
        let cwd = tmp_repo();
        let r = build(&store, "m1", &cwd, 0);
        assert!(r.nothing_happened);
        assert_eq!(r.units_completed, 0);
        assert!(r.render().contains("Nothing ran"));
        assert!(r.headline().contains("nothing ran"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_failed_unit_is_reported_as_failed_and_says_where_the_work_is() {
        let store = Store::open_in_memory().unwrap();
        let cwd = tmp_repo();
        store.upsert_work_unit(&unit("u1")).unwrap();
        store.save_validation("u1", &result("tests", true)).unwrap();
        store.save_validation("u1", &result("build", false)).unwrap();

        // The agent's half-finished work, still sitting in the tree.
        std::fs::write(std::path::Path::new(&cwd).join("half.txt"), "x\n").unwrap();

        let r = build(&store, "m1", &cwd, 0);
        assert_eq!(r.units_failed, 1);
        assert_eq!(r.units_completed, 0);
        let text = r.render();
        assert!(text.contains("FAILED"), "{text}");
        assert!(text.contains("build did not pass"), "{text}");
        assert!(text.contains("NOT committed"), "the human must know where the work is");
        assert!(text.contains("half.txt"), "and which files: {text}");
        assert!(r.headline().contains("FAILED"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_passing_unit_is_labelled_verified_not_claimed() {
        let store = Store::open_in_memory().unwrap();
        let cwd = tmp_repo();
        store.upsert_work_unit(&unit("u1")).unwrap();
        store.save_validation("u1", &result("tests", true)).unwrap();

        let r = build(&store, "m1", &cwd, 0);
        assert_eq!(r.units_completed, 1);
        assert_eq!(r.lines[0].provenance, Provenance::Verified);
        assert!(r.render().contains("[VERIFIED]"));
        assert!(r.headline().contains("all validators passed"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn checkpoints_are_recorded_and_come_with_a_way_to_read_them() {
        let store = Store::open_in_memory().unwrap();
        let cwd = tmp_repo();
        std::fs::write(std::path::Path::new(&cwd).join("work.txt"), "w\n").unwrap();
        crate::checkpoint::create(&cwd, "u1", "unit 1: the work").unwrap();

        let r = build(&store, "m1", &cwd, 0);
        let cp_line = r.lines.iter().find(|l| l.text.contains("checkpoint")).expect("listed");
        assert_eq!(cp_line.provenance, Provenance::Recorded);
        assert!(cp_line.text.contains("git show"), "a report you can act on: {}", cp_line.text);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn savings_are_labelled_estimated_because_they_are() {
        let store = Store::open_in_memory().unwrap();
        let cwd = tmp_repo();
        store.upsert_work_unit(&unit("u1")).unwrap();
        store.save_validation("u1", &result("tests", true)).unwrap();

        let r = build(&store, "m1", &cwd, 4_096_000);
        let saving = r.lines.iter().find(|l| l.text.contains("not re-sent")).unwrap();
        assert_eq!(saving.provenance, Provenance::Estimated);
        assert!(saving.text.contains("rough"), "the heuristic is stated: {}", saving.text);
        assert!(r.render().contains("[ESTIMATED]"));
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn the_report_explains_its_own_labels() {
        // A tag nobody can interpret is decoration. The reader gets the key.
        let store = Store::open_in_memory().unwrap();
        let cwd = tmp_repo();
        store.upsert_work_unit(&unit("u1")).unwrap();
        store.save_validation("u1", &result("tests", true)).unwrap();
        let text = build(&store, "m1", &cwd, 0).render();
        assert!(text.contains("VERIFIED means a"));
        assert!(text.contains("Nothing the agent merely claimed appears here"));
        let _ = std::fs::remove_dir_all(&cwd);
    }
}
