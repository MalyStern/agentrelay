//! Git checkpoints (plan §7.2 — resume that distinguishes commits, §14 Phase-1).
//!
//! An overnight run that cannot be undone is not worth starting. A checkpoint is a
//! real commit made by the supervisor after a unit of work passes its validators,
//! so the morning question — "what did it do, and can I go back to before it?" —
//! has an answer in git rather than in a log file.
//!
//! Two rules the rest of the design leans on:
//!
//! 1. **Our commits are identifiable.** They are authored as `AgentRelay
//!    <agentrelay@localhost>` and carry an `AgentRelay-Checkpoint:` trailer, so a
//!    resume can tell the supervisor's commits from the agent's from the human's.
//!    Without that, "continue where you left off" has no way to know where that was.
//! 2. **Nothing here can lose work.** We stage and commit; we never reset, clean,
//!    force, or push. Undo is the user's decision, so we hand them the exact
//!    command instead of running it.

use crate::error::{AgentRelayError, ErrorCode, Result};
use std::process::Command;

/// The identity every supervisor commit is made under. Deliberately not the user's
/// — a checkpoint should never be mistaken for something they wrote.
pub const CHECKPOINT_AUTHOR_NAME: &str = "AgentRelay";
pub const CHECKPOINT_AUTHOR_EMAIL: &str = "agentrelay@localhost";
/// Trailer that marks a commit as ours. Grep-able, and stable across versions.
pub const CHECKPOINT_TRAILER: &str = "AgentRelay-Checkpoint:";

#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    pub id: String,
    /// The commit this checkpoint points at (the new one if we committed, else HEAD).
    pub head: String,
    pub branch: Option<String>,
    /// False when the tree was already clean — there was nothing to record.
    pub created: bool,
    /// Files in the commit, for the morning report.
    pub files_changed: usize,
}

/// Raw stdout, untrimmed. `git status --porcelain` is column-sensitive: the first
/// two characters are the status code and the third is a separator, so trimming
/// the output eats the leading column of the FIRST line and silently shortens that
/// file's path by one character. Anything parsing fixed columns must use this.
fn git_raw(cwd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| AgentRelayError::new(ErrorCode::Internal, format!("git not runnable: {e}")))?;
    if !out.status.success() {
        return Err(AgentRelayError::new(
            ErrorCode::Internal,
            format!(
                "git {} failed: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Trimmed stdout — for the many commands whose output is a single value.
fn git(cwd: &str, args: &[&str]) -> Result<String> {
    Ok(git_raw(cwd, args)?.trim().to_string())
}

/// Is this a git worktree at all? Everything else here assumes it is.
pub fn is_repo(cwd: &str) -> bool {
    git(cwd, &["rev-parse", "--is-inside-work-tree"]).map(|s| s == "true").unwrap_or(false)
}

pub fn head(cwd: &str) -> Option<String> {
    git(cwd, &["rev-parse", "HEAD"]).ok().filter(|s| !s.is_empty())
}

/// The branch name, or None on a detached HEAD.
pub fn branch(cwd: &str) -> Option<String> {
    git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|s| !s.is_empty() && s != "HEAD")
}

/// Paths with uncommitted changes, staged or not, including untracked files.
pub fn dirty_paths(cwd: &str) -> Result<Vec<String>> {
    let out = git_raw(cwd, &["status", "--porcelain", "--untracked-files=all"])?;
    Ok(out
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| l[3..].to_string())
        .filter(|p| !p.is_empty())
        .collect())
}

pub fn is_clean(cwd: &str) -> Result<bool> {
    Ok(dirty_paths(cwd)?.is_empty())
}

/// Commit whatever the agent changed, as a checkpoint.
///
/// Returns `created: false` when the tree was already clean — a turn that changed
/// nothing must not leave an empty commit behind, or the history stops meaning
/// "here is a point work actually reached".
pub fn create(cwd: &str, id: &str, summary: &str) -> Result<Checkpoint> {
    if !is_repo(cwd) {
        return Err(AgentRelayError::new(
            ErrorCode::Internal,
            format!("{cwd} is not a git repository — checkpoints need one"),
        ));
    }
    let changed = dirty_paths(cwd)?;
    if changed.is_empty() {
        return Ok(Checkpoint {
            id: id.to_string(),
            head: head(cwd).unwrap_or_default(),
            branch: branch(cwd),
            created: false,
            files_changed: 0,
        });
    }

    git(cwd, &["add", "--all"])?;

    // `-c user.…` rather than `git config`: we set the identity for this one commit
    // and never write to the user's repo config.
    let message = format!("{summary}\n\n{CHECKPOINT_TRAILER} {id}\n");
    git(
        cwd,
        &[
            "-c",
            &format!("user.name={CHECKPOINT_AUTHOR_NAME}"),
            "-c",
            &format!("user.email={CHECKPOINT_AUTHOR_EMAIL}"),
            "commit",
            "--no-verify",
            "-q",
            "-m",
            &message,
        ],
    )?;

    Ok(Checkpoint {
        id: id.to_string(),
        head: head(cwd).unwrap_or_default(),
        branch: branch(cwd),
        created: true,
        files_changed: changed.len(),
    })
}

/// Was this commit made by us? Used by resume to tell our checkpoints apart from
/// commits the agent or the human made in the same window.
pub fn is_ours(cwd: &str, rev: &str) -> bool {
    git(cwd, &["log", "-1", "--format=%B", rev])
        .map(|body| body.contains(CHECKPOINT_TRAILER))
        .unwrap_or(false)
}

/// Our checkpoint commits, newest first, as (sha, checkpoint-id).
pub fn list(cwd: &str, limit: usize) -> Result<Vec<(String, String)>> {
    let raw = git(
        cwd,
        &["log", &format!("-{}", limit.max(1)), "--format=%H%x1f%B%x1e", &format!("--grep={CHECKPOINT_TRAILER}")],
    )?;
    let mut out = Vec::new();
    for rec in raw.split('\u{1e}') {
        let rec = rec.trim_start_matches('\n');
        let Some((sha, body)) = rec.split_once('\u{1f}') else { continue };
        if sha.is_empty() {
            continue;
        }
        let id = body
            .lines()
            .find_map(|l| l.trim().strip_prefix(CHECKPOINT_TRAILER))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        out.push((sha.to_string(), id));
    }
    Ok(out)
}

/// A one-line-per-file summary of what changed since `rev`. Read-only.
pub fn diff_stat_since(cwd: &str, rev: &str) -> Result<String> {
    git(cwd, &["diff", "--stat", rev, "HEAD"])
}

/// The command that rewinds to `rev`, discarding everything committed after it.
/// We return these as text and never run them: discarding a night's work is the
/// user's call, and a supervisor that can reset the tree can also destroy it.
pub fn rewind_command(rev: &str) -> String {
    format!("git reset --hard {rev}   # discards every change made AFTER this checkpoint")
}

/// The command that undoes `rev` itself — the one you want right after a
/// checkpoint, when `reset --hard <that same sha>` would be a no-op.
pub fn undo_command(rev: &str) -> String {
    format!("git reset --hard {rev}~1   # discards this unit's work too")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    /// A throwaway git repo with one commit.
    fn repo() -> String {
        let dir = std::env::temp_dir().join(format!(
            "ar-cp-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.to_str().unwrap().to_string();
        git(&p, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&p, &["add", "-A"]).unwrap();
        git(
            &p,
            &["-c", "user.name=T", "-c", "user.email=t@e.com", "commit", "-q", "-m", "base"],
        )
        .unwrap();
        p
    }

    fn cleanup(p: &str) {
        let _ = std::fs::remove_dir_all(p);
    }

    #[test]
    fn dotfile_paths_survive_status_parsing() {
        // The bug this caught: trimming the whole `git status --porcelain` output
        // removes the leading status column of the FIRST line only, so the first
        // modified file silently lost a character — ".agentrelay/config.json"
        // was reported as "agentrelay/config.json". A report that renames the
        // user's files is worse than no report.
        let p = repo();
        std::fs::create_dir_all(std::path::Path::new(&p).join(".config")).unwrap();
        std::fs::write(std::path::Path::new(&p).join(".config/settings.json"), "{}\n").unwrap();
        git(&p, &["add", "-A"]).unwrap();
        git(&p, &["-c", "user.name=T", "-c", "user.email=t@e.com", "commit", "-q", "-m", "add"]).unwrap();

        std::fs::write(std::path::Path::new(&p).join(".config/settings.json"), "{\"a\":1}\n").unwrap();
        std::fs::write(std::path::Path::new(&p).join("zz.txt"), "second\n").unwrap();

        let dirty = dirty_paths(&p).unwrap();
        assert!(
            dirty.contains(&".config/settings.json".to_string()),
            "the dot must survive: {dirty:?}"
        );
        assert!(dirty.contains(&"zz.txt".to_string()), "{dirty:?}");
        cleanup(&p);
    }

    #[test]
    fn a_clean_tree_produces_no_commit() {
        let p = repo();
        let before = head(&p).unwrap();
        let cp = create(&p, "c1", "nothing happened").unwrap();
        assert!(!cp.created, "a turn that changed nothing must not leave an empty commit");
        assert_eq!(cp.files_changed, 0);
        assert_eq!(head(&p).unwrap(), before, "HEAD must not move");
        cleanup(&p);
    }

    #[test]
    fn a_dirty_tree_is_committed_and_identifiable_as_ours() {
        let p = repo();
        let before = head(&p).unwrap();
        std::fs::write(std::path::Path::new(&p).join("a.txt"), "two\n").unwrap();
        std::fs::write(std::path::Path::new(&p).join("new.txt"), "added\n").unwrap();

        let cp = create(&p, "c7", "unit 3: add the parser").unwrap();
        assert!(cp.created);
        assert_eq!(cp.files_changed, 2, "modified + untracked both count");
        assert_ne!(cp.head, before);
        assert_eq!(cp.branch.as_deref(), Some("main"));
        assert!(is_clean(&p).unwrap(), "the tree is clean once checkpointed");

        // The point of the trailer: resume can tell our commits from everyone else's.
        assert!(is_ours(&p, &cp.head));
        assert!(!is_ours(&p, &before));
        let author = git(&p, &["log", "-1", "--format=%an <%ae>", &cp.head]).unwrap();
        assert_eq!(author, format!("{CHECKPOINT_AUTHOR_NAME} <{CHECKPOINT_AUTHOR_EMAIL}>"));
        let subject = git(&p, &["log", "-1", "--format=%s", &cp.head]).unwrap();
        assert_eq!(subject, "unit 3: add the parser");
        cleanup(&p);
    }

    #[test]
    fn list_returns_only_our_checkpoints_newest_first() {
        let p = repo();
        std::fs::write(std::path::Path::new(&p).join("a.txt"), "x\n").unwrap();
        create(&p, "c1", "first unit").unwrap();

        // A commit the human (or the agent) made — must not appear as a checkpoint.
        std::fs::write(std::path::Path::new(&p).join("b.txt"), "human\n").unwrap();
        git(&p, &["add", "-A"]).unwrap();
        git(&p, &["-c", "user.name=H", "-c", "user.email=h@e.com", "commit", "-q", "-m", "by hand"]).unwrap();

        std::fs::write(std::path::Path::new(&p).join("a.txt"), "y\n").unwrap();
        create(&p, "c2", "second unit").unwrap();

        let found = list(&p, 20).unwrap();
        assert_eq!(found.len(), 2, "the hand-made commit is not a checkpoint");
        assert_eq!(found[0].1, "c2", "newest first");
        assert_eq!(found[1].1, "c1");
        cleanup(&p);
    }

    #[test]
    fn diff_since_a_checkpoint_shows_what_came_after() {
        let p = repo();
        std::fs::write(std::path::Path::new(&p).join("a.txt"), "x\n").unwrap();
        let cp = create(&p, "c1", "first").unwrap();
        std::fs::write(std::path::Path::new(&p).join("later.txt"), "after\n").unwrap();
        create(&p, "c2", "second").unwrap();

        let stat = diff_stat_since(&p, &cp.head).unwrap();
        assert!(stat.contains("later.txt"), "got: {stat}");
        assert!(!stat.contains("a.txt"));
        cleanup(&p);
    }

    #[test]
    fn a_non_repo_is_refused_with_a_useful_message() {
        let dir = std::env::temp_dir().join(format!("ar-nogit-{}", N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.to_str().unwrap();
        assert!(!is_repo(p));
        let err = match create(p, "c1", "x") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("must not checkpoint outside a repo"),
        };
        assert!(err.contains("not a git repository"), "got: {err}");
        cleanup(p);
    }

    #[test]
    fn undo_is_offered_never_executed() {
        // The supervisor hands back the command; running it is the user's decision.
        assert!(rewind_command("abc1234").starts_with("git reset --hard abc1234 "));
        // Undoing the checkpoint itself needs its parent — resetting to the sha we
        // just committed would be a no-op, which is the opposite of what "undo" means.
        assert!(undo_command("abc1234").starts_with("git reset --hard abc1234~1"));
    }

    #[test]
    fn the_offered_undo_actually_removes_the_units_work() {
        // Not a claim about a string: run the command we print and check the result.
        let p = repo();
        let before = head(&p).unwrap();
        std::fs::write(std::path::Path::new(&p).join("unit.txt"), "work\n").unwrap();
        let cp = create(&p, "c1", "unit 1").unwrap();
        assert!(std::path::Path::new(&p).join("unit.txt").exists());

        git(&p, &["reset", "--hard", &format!("{}~1", cp.head)]).unwrap();
        assert_eq!(head(&p).unwrap(), before, "back to before the unit");
        assert!(!std::path::Path::new(&p).join("unit.txt").exists());
        cleanup(&p);
    }
}
