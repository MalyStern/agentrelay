//! Watchtower layer-1: deterministic guard evaluation (plan 00 §7, 02 §ז.4). The
//! Rust port of `runner/src/policy/guard-eval.ts`, INCLUDING every QA fix (path
//! normalization so `..` can't defeat containment, the read-only command allowlist,
//! tightened classifiers, fail-closed `forbid_cmd`). Pure and total.

use crate::provider::{Guard, GuardKind};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposedAction {
    Write { path: String },
    Command { command: String },
    Network { target: Option<String> },
    /// A tool that can write or run things, whose arguments we could not read.
    /// Exists so that "we don't understand this" has a representation — without it
    /// an unparsed write tool produced an empty action list, and an empty list is
    /// indistinguishable from "nothing dangerous was proposed".
    Unknown { tool: String, detail: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GuardViolation {
    pub guard: GuardKind,
    pub reason_code: String,
    pub action: ProposedAction,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GuardDecision {
    pub allow: bool,
    pub violations: Vec<GuardViolation>,
}

// ── path normalization (resolves . and .., flags relative escapes) ─────────────
pub struct Norm {
    pub absolute: bool,
    pub escapes: bool,
    pub path: String,
}

pub fn norm_path(input: &str) -> Norm {
    let p = input.replace('\\', "/");
    let b = p.as_bytes();
    // `C:secret.txt` — no slash — is Windows' "relative to drive C's current
    // directory". Requiring the slash classified it as a relative path, and relative
    // paths are auto-approved, so it slipped straight past the workspace boundary
    // onto another drive. A drive letter makes it drive-anchored either way.
    let has_drive = p.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    let (drive, rest) = if has_drive {
        (
            format!("{}:", (b[0] as char).to_ascii_lowercase()),
            p[2..].to_string(),
        )
    } else {
        (String::new(), p.clone())
    };
    let absolute = !drive.is_empty() || rest.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let mut up = 0usize;
    for s in rest.split('/') {
        if s.is_empty() || s == "." {
            continue;
        }
        if s == ".." {
            if !out.is_empty() {
                out.pop();
            } else if !absolute {
                up += 1;
            }
        } else {
            out.push(s);
        }
    }
    let prefix = if !drive.is_empty() {
        format!("{}/", drive)
    } else if absolute {
        "/".to_string()
    } else {
        String::new()
    };
    Norm {
        absolute,
        escapes: up > 0,
        path: format!("{}{}{}", prefix, "../".repeat(up), out.join("/")),
    }
}

// ── tiny glob (**, *, ?) -> Regex against POSIX paths ──────────────────────────
pub fn glob_to_regex(glob: &str) -> Regex {
    // Case-insensitive on purpose. Windows and macOS treat `.ENV` and `.env` as the
    // same file, so a case-sensitive match on a FORBIDDEN path is a hole: the agent
    // writes `.ENV`, the guard sees no match, and the user's secrets are overwritten
    // anyway. A forbid-list that is slightly too broad is the safe direction to err.
    let mut re = String::from("(?i)^");
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if chars.get(i + 1) == Some(&'*') {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 2;
                } else {
                    re.push_str(".*");
                    i += 1;
                }
            } else {
                re.push_str("[^/]*");
            }
        } else if c == '?' {
            re.push_str("[^/]");
        } else if ".+^${}()|[]\\".contains(c) {
            re.push('\\');
            re.push(c);
        } else {
            re.push(c);
        }
        i += 1;
    }
    re.push('$');
    Regex::new(&re).unwrap_or_else(|_| Regex::new(r"^\x00$").unwrap())
}

// ── command classifiers ────────────────────────────────────────────────────────
//
// `X?` after a binary name (via the `\.(?:exe|cmd|bat)` groups) matters on Windows:
// npm installs `git.cmd`, `npm.cmd`, etc., and an agent that runs `git.exe push` or
// `npm.cmd install` would otherwise slip these classifiers — the whitespace the old
// patterns required right after the name is not there, the extension is. The codebase
// targets Windows, so this is a real hole, not a hypothetical.
static RE_PUSH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bgit(?:\.(?:exe|cmd|bat))?\b(?:\s+\S+)*\s+push\b").unwrap());
static RE_DEPLOY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:vercel|netlify|flyctl|serverless)(?:\.(?:exe|cmd|bat))?\b|\bfly(?:\.exe)?\s+deploy\b|\bkubectl(?:\.exe)?\b(?:\s+\S+)*\s+apply\b|\bterraform(?:\.exe)?\b(?:\s+\S+)*\s+apply\b|\bgcloud(?:\.cmd)?\b[^\n]*\bdeploy\b|\beb\s+deploy\b|\bheroku(?:\.cmd)?\b[^\n]*(?:push|deploy)\b|\bansible-playbook\b").unwrap()
});
static RE_NET: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:^|[\s;&|(`$])(?:curl|wget|nc|ncat|telnet|scp|rsync|ssh)(?:\.exe)?\b").unwrap()
});
static RE_DEP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:npm|pnpm|yarn|bun)(?:\.(?:exe|cmd|bat))?\s+(?:install|add|i)\b|\bpip3?(?:\.exe)?\s+install\b|\bcargo(?:\.exe)?\s+add\b|\bgo(?:\.exe)?\s+get\b|\bgem(?:\.cmd)?\s+install\b|\bpoetry(?:\.exe)?\s+add\b|\buv(?:\.exe)?\s+add\b").unwrap()
});
static RE_WRITE_TOOL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(Write|Edit|MultiEdit|NotebookEdit|str_replace_editor|create_file|apply_patch)$").unwrap()
});
static RE_SHELL_TOOL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(Bash|Shell|run_terminal_cmd|exec|terminal)$").unwrap());
static RE_NET_TOOL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(WebFetch|WebSearch|fetch|http_request)$").unwrap());

// `env` is deliberately NOT here: `env rm -rf src` is a write wearing a read-only
// name, and the same goes for any launcher that takes a command as an argument.
const READONLY_BINS: &[&str] = &[
    "ls", "cat", "head", "tail", "pwd", "echo", "printf", "grep", "egrep", "rg", "wc", "stat", "file",
    "which", "type", "printenv", "date", "whoami", "uname", "true", "basename", "dirname",
    "realpath", "sort", "uniq", "cut", "diff", "tree", "du", "df",
];

// Only git verbs that cannot change a ref, the worktree, or the user's config.
// `branch` (`-D` deletes commits), `stash` (empties the worktree) and `config`
// (writes to the user's file) were here and are not read-only in any useful sense.
const GIT_READONLY: &[&str] = &[
    "status", "log", "diff", "show", "rev-parse", "ls-files", "remote", "describe", "blame",
    "cat-file",
];

/// True only if EVERY sub-command is a pure inspection with no side effect.
///
/// Fails closed: anything this cannot confidently classify is not read-only.
pub fn is_read_only_command(command: &str) -> bool {
    if command.contains('>') {
        return false; // any redirection writes
    }
    // Command substitution hides an arbitrary command inside what looks like an
    // argument — `echo $(rm -rf src)` would otherwise be classified by its `echo`.
    if command.contains("$(") || command.contains('`') || command.contains("${") {
        return false;
    }
    // A NEWLINE separates commands exactly like `;` does, and multi-line scripts are
    // the normal thing a coding agent emits. Omitting it here meant "ls\nrm -rf src"
    // was judged solely on its first word and allowed.
    let parts: Vec<&str> = command
        .split(|c| c == ';' || c == '|' || c == '&' || c == '\n' || c == '\r')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return false;
    }
    parts.iter().all(|part| {
        let toks: Vec<&str> = part.split_whitespace().collect();
        let bin = toks
            .first()
            .map(|b| b.rsplit('/').next().unwrap_or(b))
            .unwrap_or("");
        if bin == "git" {
            let sub = toks.get(1).copied().unwrap_or("");
            // Match `-i` as its own token: `part.contains("-i")` also matched
            // `--ignore-all-space`, blocking a perfectly ordinary `git diff`.
            let interactive = toks.iter().any(|t| *t == "-i" || *t == "--interactive");
            GIT_READONLY.contains(&sub) && !interactive
        } else if bin == "find" {
            !Regex::new(r"-(?:delete|exec|execdir|fprint|fprintf)\b")
                .unwrap()
                .is_match(part)
        } else if bin == "sed" {
            !Regex::new(r"\s-i\b").unwrap().is_match(part)
        } else {
            READONLY_BINS.contains(&bin)
        }
    })
}

/// Derive provider-neutral actions from a `tool_pre` (tool name + args).
pub fn derive_actions(tool: &str, args: &Value) -> Vec<ProposedAction> {
    let get = |k: &str| -> Option<String> {
        args.get(k).and_then(|v| v.as_str()).map(|s| s.to_string())
    };
    if RE_WRITE_TOOL.is_match(tool) {
        if let Some(p) = get("file_path")
            .or_else(|| get("path"))
            .or_else(|| get("filePath"))
            .or_else(|| get("notebook_path"))
        {
            return vec![ProposedAction::Write { path: p }];
        }
    }
    if RE_SHELL_TOOL.is_match(tool) {
        if let Some(cmd) = get("command").or_else(|| get("cmd")).or_else(|| get("script")) {
            return vec![ProposedAction::Command { command: cmd }];
        }
    }
    if RE_NET_TOOL.is_match(tool) {
        return vec![ProposedAction::Network {
            target: get("url"),
        }];
    }

    // A write or shell tool whose arguments we could not read is the dangerous
    // case, not a harmless one: `apply_patch` carries its paths inside the patch
    // body, and a provider that names its argument `file` instead of `file_path`
    // would produce zero actions — and zero actions evaluates to "allow". Emit an
    // Unknown action so the guards get something to refuse.
    if RE_WRITE_TOOL.is_match(tool) || RE_SHELL_TOOL.is_match(tool) {
        return vec![ProposedAction::Unknown {
            tool: tool.to_string(),
            detail: "arguments not in a recognised shape — cannot tell what this would touch"
                .to_string(),
        }];
    }
    vec![]
}

fn inside_workspace(path: &str, cwd: &str) -> bool {
    let t = norm_path(path);
    if t.escapes {
        return false;
    }
    if !t.absolute {
        return true;
    }
    let root = norm_path(cwd).path.trim_end_matches('/').to_string();
    t.path == root || t.path.starts_with(&format!("{}/", root))
}

fn path_for_glob(path: &str, cwd: &str) -> String {
    let t = norm_path(path);
    if !t.absolute {
        return t.path;
    }
    let root = norm_path(cwd).path.trim_end_matches('/').to_string();
    if t.path == root {
        return String::new();
    }
    if let Some(stripped) = t.path.strip_prefix(&format!("{}/", root)) {
        return stripped.to_string();
    }
    t.path
}

fn as_globs(spec: &Option<Value>) -> Vec<String> {
    match spec {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    }
}

fn violation(guard: &Guard, action: &ProposedAction, detail: Option<String>) -> GuardViolation {
    GuardViolation {
        guard: guard.kind,
        reason_code: format!("guard.{}", serde_json::to_value(guard.kind).unwrap().as_str().unwrap()),
        action: action.clone(),
        detail,
    }
}

fn check_action(action: &ProposedAction, guard: &Guard, cwd: &str) -> Option<GuardViolation> {
    use GuardKind::*;
    // An action we could not parse is refused by EVERY guard. A supervisor that
    // permits what it does not understand is not a supervisor — and the parse
    // failure is exactly the case an attacker or an unfamiliar provider produces.
    if let ProposedAction::Unknown { tool, detail } = action {
        return Some(violation(guard, action, Some(format!("{tool}: {detail}"))));
    }
    match guard.kind {
        NoPush => match action {
            ProposedAction::Command { command } if RE_PUSH.is_match(command) => Some(violation(guard, action, None)),
            _ => None,
        },
        NoDeploy => match action {
            ProposedAction::Command { command } if RE_DEPLOY.is_match(command) => Some(violation(guard, action, None)),
            _ => None,
        },
        NoNet => match action {
            ProposedAction::Network { .. } => Some(violation(guard, action, None)),
            ProposedAction::Command { command } if RE_NET.is_match(command) => Some(violation(guard, action, None)),
            _ => None,
        },
        NoDependency => match action {
            ProposedAction::Command { command } if RE_DEP.is_match(command) => Some(violation(guard, action, None)),
            _ => None,
        },
        ForbidCmd => {
            let pat = guard.spec.as_ref().and_then(|v| v.as_str());
            match (action, pat) {
                (ProposedAction::Command { command }, Some(p)) => match Regex::new(&format!("(?i){}", p)) {
                    Ok(re) => {
                        if re.is_match(command) {
                            Some(violation(guard, action, Some(p.to_string())))
                        } else {
                            None
                        }
                    }
                    // An uncompilable spec fails CLOSED — block.
                    Err(_) => Some(violation(guard, action, Some(format!("invalid forbid_cmd pattern: {}", p)))),
                },
                _ => None,
            }
        }
        ReadOnly => match action {
            ProposedAction::Write { .. } | ProposedAction::Network { .. } => Some(violation(guard, action, None)),
            ProposedAction::Command { command } => {
                if is_read_only_command(command) {
                    None
                } else {
                    Some(violation(guard, action, Some(command.clone())))
                }
            }
            // Handled above, before the per-guard match.
            ProposedAction::Unknown { .. } => None,
        },
        WorkspaceBoundary => match action {
            ProposedAction::Write { path } if !inside_workspace(path, cwd) => {
                Some(violation(guard, action, Some(path.clone())))
            }
            _ => None,
        },
        ForbidPath => match action {
            ProposedAction::Write { path } => {
                let rel = path_for_glob(path, cwd);
                if as_globs(&guard.spec).iter().any(|g| glob_to_regex(g).is_match(&rel)) {
                    Some(violation(guard, action, Some(path.clone())))
                } else {
                    None
                }
            }
            _ => None,
        },
        AllowedPaths => match action {
            ProposedAction::Write { path } => {
                let globs = as_globs(&guard.spec);
                if globs.is_empty() {
                    return None;
                }
                let rel = path_for_glob(path, cwd);
                if globs.iter().any(|g| glob_to_regex(g).is_match(&rel)) {
                    None
                } else {
                    Some(violation(guard, action, Some(path.clone())))
                }
            }
            _ => None,
        },
        // Stateful / branch guards are enforced by the daemon with counters/VCS.
        AllowOnlyBranch | MaxFiles | MaxCost => None,
    }
}

pub fn evaluate_guards(actions: &[ProposedAction], guards: &[Guard], cwd: &str) -> GuardDecision {
    let mut violations = Vec::new();
    for action in actions {
        for guard in guards {
            if let Some(v) = check_action(action, guard, cwd) {
                violations.push(v);
            }
        }
    }
    GuardDecision {
        allow: violations.is_empty(),
        violations,
    }
}

pub fn evaluate_tool_pre(tool: &str, args: &Value, guards: &[Guard], cwd: &str) -> GuardDecision {
    evaluate_guards(&derive_actions(tool, args), guards, cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CWD: &str = "/home/u/proj";
    fn w(path: &str) -> Vec<ProposedAction> {
        vec![ProposedAction::Write { path: path.into() }]
    }
    fn c(cmd: &str) -> Vec<ProposedAction> {
        vec![ProposedAction::Command { command: cmd.into() }]
    }
    fn g(kind: GuardKind) -> Vec<Guard> {
        vec![Guard::new(kind)]
    }

    #[test]
    fn read_only_is_not_fooled_by_a_newline_or_a_launcher_or_substitution() {
        // Each of these was ALLOWED. A coding agent emits multi-line shell scripts
        // constantly, so the newline case is the common one, not the exotic one.
        for cmd in [
            "ls
rm -rf src",
            "ls -la
git reset --hard HEAD~5",
            "echo $(rm -rf src)",
            "echo `rm -rf src`",
            "env rm -rf src",
            "git branch -D main",
            "git stash",
            "git config user.email attacker@example.com",
        ] {
            assert!(!is_read_only_command(cmd), "still allowed: {cmd}");
            assert!(!evaluate_guards(&c(cmd), &g(GuardKind::ReadOnly), CWD).allow, "{cmd}");
        }
        // …while genuinely read-only commands still pass, including the one the
        // old `-i` substring check wrongly blocked.
        for cmd in ["ls -la", "git diff --ignore-all-space", "cat a.txt | grep x", "git log -5"] {
            assert!(is_read_only_command(cmd), "wrongly blocked: {cmd}");
        }
    }

    #[test]
    fn a_tool_we_cannot_parse_is_refused_by_every_guard() {
        // Zero actions used to mean "nothing dangerous proposed", so an unfamiliar
        // argument shape sailed through. apply_patch carries its paths inside the
        // patch body and hit this exact hole.
        for (tool, args) in [
            ("apply_patch", json!({"patch": "*** Update File: /etc/passwd"})),
            ("Write", json!({"file": "/etc/passwd"})),
            ("Bash", json!({"commands": ["rm -rf /"]})),
        ] {
            for kind in [GuardKind::WorkspaceBoundary, GuardKind::ReadOnly, GuardKind::NoPush] {
                let d = evaluate_tool_pre(tool, &args, &g(kind), CWD);
                assert!(!d.allow, "{tool} allowed under {kind:?}");
            }
        }
        // A tool that is neither a writer nor a shell is still not our business.
        assert!(evaluate_tool_pre("SomeReadTool", &json!({}), &g(GuardKind::ReadOnly), CWD).allow);
    }

    #[test]
    fn forbidden_paths_match_regardless_of_case() {
        // Windows and macOS treat .ENV and .env as one file, so a case-sensitive
        // forbid-list is not a forbid-list at all.
        let gs = vec![Guard::with_spec(
            GuardKind::ForbidPath,
            json!([".env", "**/*.pem"]),
        )];
        for p in [".env", ".ENV", ".Env", "keys/server.PEM"] {
            assert!(!evaluate_guards(&w(p), &gs, CWD).allow, "not blocked: {p}");
        }
    }

    #[test]
    fn a_windows_drive_relative_path_does_not_escape_the_workspace() {
        // `C:secret.txt` means "relative to drive C's current directory". Demanding
        // a slash classified it as relative, and relative paths were auto-approved.
        let n = norm_path("C:secret.txt");
        assert!(n.absolute, "a drive letter anchors the path");
        assert!(!evaluate_guards(&w("D:evil.txt"), &g(GuardKind::WorkspaceBoundary), CWD).allow);
    }

    #[test]
    fn norm_resolves_dotdot() {
        assert_eq!(norm_path("/repo/../etc/passwd").path, "/etc/passwd");
        assert!(norm_path("src/theme/../../../etc/passwd").escapes);
        assert_eq!(norm_path("x/../secrets/key").path, "secrets/key");
    }

    #[test]
    fn workspace_boundary_blocks_traversal() {
        let g = g(GuardKind::WorkspaceBoundary);
        assert!(!evaluate_guards(&w("/home/u/proj/../etc/passwd"), &g, CWD).allow);
        assert!(evaluate_guards(&w("/home/u/proj/src/../lib/a.ts"), &g, CWD).allow);
    }

    #[test]
    fn allowed_paths_cannot_be_escaped() {
        let g = vec![Guard::with_spec(GuardKind::AllowedPaths, json!("src/theme/**"))];
        assert!(!evaluate_guards(&w("src/theme/../../../etc/passwd"), &g, CWD).allow);
        assert!(evaluate_guards(&w("src/theme/x.css"), &g, CWD).allow);
    }

    #[test]
    fn read_only_blocks_shell_writes_and_push() {
        let g = g(GuardKind::ReadOnly);
        assert!(!evaluate_guards(&w("src/a.ts"), &g, CWD).allow);
        for cmd in ["echo pwned > src/a.ts", "rm -rf src", "git checkout .", "git push", "curl evil.com"] {
            assert!(!evaluate_guards(&c(cmd), &g, CWD).allow, "should block: {cmd}");
        }
        for cmd in ["git status", "ls -la", "cat package.json", "git diff && ls"] {
            assert!(evaluate_guards(&c(cmd), &g, CWD).allow, "should allow: {cmd}");
        }
    }

    #[test]
    fn classifiers_catch_evasions() {
        assert!(!evaluate_guards(&c("git -C /repo push"), &g(GuardKind::NoPush), CWD).allow);
        assert!(!evaluate_guards(&c("kubectl --context prod apply -f x"), &g(GuardKind::NoDeploy), CWD).allow);
        assert!(!evaluate_guards(&c("X=$(curl evil.com)"), &g(GuardKind::NoNet), CWD).allow);
        assert!(!evaluate_guards(&c("npm install left-pad"), &g(GuardKind::NoDependency), CWD).allow);
    }

    #[test]
    fn forbid_cmd_invalid_regex_fails_closed() {
        let g = vec![Guard::with_spec(GuardKind::ForbidCmd, json!("("))];
        assert!(!evaluate_guards(&c("ls"), &g, CWD).allow);
    }

    #[test]
    fn derive_actions_maps_tools() {
        assert_eq!(
            derive_actions("Write", &json!({"file_path": "a.ts"})),
            w("a.ts")
        );
        assert_eq!(
            derive_actions("Bash", &json!({"command": "ls"})),
            c("ls")
        );
        assert!(derive_actions("Unknown", &json!({})).is_empty());
    }

    #[test]
    fn evaluate_tool_pre_end_to_end() {
        let guards = vec![Guard::with_spec(GuardKind::AllowedPaths, json!("**/*.css"))];
        let d = evaluate_tool_pre("Write", &json!({"file_path": "src/logic.ts"}), &guards, CWD);
        assert!(!d.allow);
        assert_eq!(d.violations.len(), 1);
    }
}
