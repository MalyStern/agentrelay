//! Deterministic guard enforcement via a PreToolUse hook (plan §ז.1, §ז.4).
//!
//! Until now Watchtower layer-1 was **advisory**. The daemon watched the agent's
//! NDJSON stream, saw a forbidden tool call go past, and killed the process tree.
//! That is a reaction on an asynchronous pipe: by the time we read the line the CLI
//! has already started running the tool, and nothing guarantees our kill lands
//! first. The plan is explicit that a guard has to *block*, not chase.
//!
//! A PreToolUse hook is the mechanism that actually blocks: the agent runs it and
//! waits for the answer before the tool executes. The hook is this same binary —
//! `agentrelay hook` — reading the tool call on stdin and answering with the same
//! `evaluate_guards` the daemon uses, so there is exactly one set of rules and no
//! way for the two paths to drift.
//!
//! Two things this module refuses to do:
//!
//! * **Fail open.** Unparseable input, a missing config, a config that does not
//!   load — every one of them denies. A hook that allows whatever it does not
//!   understand is decoration.
//! * **Silently replace the user's hooks.** `install` merges into an existing
//!   `.claude/settings.json` and leaves every other hook alone.
//!
//! The exact wire contract (field names, how a denial is signalled) is a documented
//! Phase-0 kill-test that needs one run against a real Claude to confirm. Both
//! documented signals are emitted — a non-zero exit AND the JSON decision — so a
//! denial cannot be missed because only one of them was being read.

use crate::guard::{derive_actions, evaluate_guards, GuardDecision};
use crate::provider::Guard;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a PreToolUse hook receives. Unknown fields are ignored on purpose: a
/// provider adding one must not turn every guard decision into a parse failure.
#[derive(Debug, Clone, Deserialize)]
pub struct HookInput {
    #[serde(default, alias = "toolName")]
    pub tool_name: String,
    #[serde(default, alias = "toolInput")]
    pub tool_input: Value,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HookVerdict {
    pub allow: bool,
    pub reason: String,
    /// Exit code to leave with: 0 allows, 2 is the documented "block" signal.
    pub exit_code: i32,
    /// The JSON a hook prints to convey the same decision structurally.
    pub json: String,
}

fn deny_json(reason: &str) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
    .to_string()
}

fn allow_json() -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow"
        }
    })
    .to_string()
}

pub fn deny(reason: impl Into<String>) -> HookVerdict {
    let reason = reason.into();
    HookVerdict { allow: false, json: deny_json(&reason), reason, exit_code: 2 }
}

pub fn allow() -> HookVerdict {
    HookVerdict {
        allow: true,
        reason: String::new(),
        exit_code: 0,
        json: allow_json(),
    }
}

/// Decide on one tool call. Pure — the CLI supplies stdin, the config and the cwd.
pub fn decide(raw_stdin: &str, guards: &[Guard], fallback_cwd: &str) -> HookVerdict {
    let input: HookInput = match serde_json::from_str(raw_stdin) {
        Ok(v) => v,
        // We were asked to judge something and could not read it. The only safe
        // answer to "I don't know what this is" is no.
        Err(e) => {
            return deny(format!(
                "AgentRelay could not read this tool call ({e}), so it is refused rather than \
                 guessed at."
            ))
        }
    };
    if input.tool_name.trim().is_empty() {
        return deny("AgentRelay received a tool call with no tool name — refused.");
    }

    let cwd = input.cwd.clone().unwrap_or_else(|| fallback_cwd.to_string());
    let actions = derive_actions(&input.tool_name, &input.tool_input);
    let decision: GuardDecision = evaluate_guards(&actions, guards, &cwd);

    if decision.allow {
        return allow();
    }
    let reasons: Vec<String> = decision
        .violations
        .iter()
        .map(|v| {
            let detail = v.detail.clone().unwrap_or_default();
            if detail.is_empty() {
                v.reason_code.clone()
            } else {
                format!("{} ({})", v.reason_code, detail)
            }
        })
        .collect();
    deny(format!(
        "Blocked by AgentRelay: {}. This is a guard the user configured for this run — do not \
         retry it, and do not work around it. Choose a different approach or stop and report.",
        reasons.join("; ")
    ))
}

// ── installing the hook into the agent's settings ────────────────────────────

pub const SETTINGS_REL_PATH: &str = ".claude/settings.json";

/// Build the settings object with our hook merged in, preserving everything else.
///
/// Returns None when our hook is already present, so installing twice is a no-op
/// rather than a growing pile of duplicate entries.
pub fn merge_hook_settings(existing: &Value, command: &str) -> Option<Value> {
    let mut root = existing.as_object().cloned().unwrap_or_default();

    let hooks = root.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let hooks_obj = match hooks.as_object_mut() {
        Some(o) => o,
        None => return None, // malformed settings: refuse to rewrite what we can't read
    };

    let pre = hooks_obj
        .entry("PreToolUse")
        .or_insert_with(|| Value::Array(vec![]));
    let arr = pre.as_array_mut()?;

    // Already installed? Match on the command, not on position.
    if arr.iter().any(|m| {
        m.get("hooks")
            .and_then(|h| h.as_array())
            .map(|hs| hs.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command)))
            .unwrap_or(false)
    }) {
        return None;
    }

    arr.push(serde_json::json!({
        // Empty matcher = every tool. A guard that only sees some tools is not a
        // guard; `derive_actions` already decides what is actually relevant.
        "matcher": "",
        "hooks": [{ "type": "command", "command": command }]
    }));

    Some(Value::Object(root))
}

/// Is an AgentRelay PreToolUse hook already registered?
///
/// Matches on the recognisable shape of our command (it contains "agentrelay" and
/// "hook") rather than an exact string, because the binary path and the `--cwd` vary
/// per machine and per workspace — an exact-match check would report "not installed"
/// for a hook that is in fact ours, and a supervised run would then reinstall a
/// duplicate or, worse, believe blocking is off when it is on.
pub fn agentrelay_hook_present(existing: &Value) -> bool {
    existing
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter().any(|m| {
                m.get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|hs| {
                        hs.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .map(|c| {
                                    let l = c.to_ascii_lowercase();
                                    l.contains("agentrelay") && l.contains("hook")
                                })
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Remove our hook and leave the user's others untouched.
pub fn remove_hook_settings(existing: &Value, command: &str) -> Value {
    let mut root = existing.as_object().cloned().unwrap_or_default();
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        if let Some(arr) = hooks.get_mut("PreToolUse").and_then(|p| p.as_array_mut()) {
            arr.retain(|m| {
                !m.get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|hs| {
                        hs.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command))
                    })
                    .unwrap_or(false)
            });
        }
    }
    Value::Object(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::GuardKind;
    use serde_json::json;

    fn guards() -> Vec<Guard> {
        vec![
            Guard::new(GuardKind::NoPush),
            Guard::new(GuardKind::WorkspaceBoundary),
        ]
    }

    const CWD: &str = "/home/u/proj";

    #[test]
    fn a_forbidden_command_is_denied_with_a_reason_the_agent_can_act_on() {
        let v = decide(
            &json!({"tool_name":"Bash","tool_input":{"command":"git push origin main"},"cwd":CWD})
                .to_string(),
            &guards(),
            CWD,
        );
        assert!(!v.allow);
        assert_eq!(v.exit_code, 2, "the documented block signal");
        assert!(v.reason.contains("Blocked by AgentRelay"), "{}", v.reason);
        assert!(v.reason.contains("no_push"), "{}", v.reason);
        // Telling the agent not to route around the guard matters as much as the
        // denial: otherwise it tries the next thing that achieves the same effect.
        assert!(v.reason.contains("do not work around it"), "{}", v.reason);
        assert!(v.json.contains("\"permissionDecision\":\"deny\""), "{}", v.json);
    }

    #[test]
    fn a_write_outside_the_workspace_is_denied() {
        let v = decide(
            &json!({"tool_name":"Write","tool_input":{"file_path":"/etc/passwd"},"cwd":CWD})
                .to_string(),
            &guards(),
            CWD,
        );
        assert!(!v.allow);
        assert!(v.reason.contains("workspace"), "{}", v.reason);
    }

    #[test]
    fn ordinary_work_is_allowed_and_exits_zero() {
        let v = decide(
            &json!({"tool_name":"Write","tool_input":{"file_path":"/home/u/proj/src/a.rs"},"cwd":CWD})
                .to_string(),
            &guards(),
            CWD,
        );
        assert!(v.allow);
        assert_eq!(v.exit_code, 0);
        // A guard that blocks ordinary work gets switched off, so this matters.
        assert!(v.json.contains("allow"));
    }

    #[test]
    fn unreadable_input_is_denied_not_waved_through() {
        for bad in ["", "not json", "[]", "{\"tool_name\":\"\"}"] {
            let v = decide(bad, &guards(), CWD);
            assert!(!v.allow, "allowed: {bad:?}");
            assert_eq!(v.exit_code, 2);
        }
    }

    #[test]
    fn an_unfamiliar_argument_shape_is_denied_because_we_cannot_see_the_path() {
        // apply_patch carries its paths inside the patch body. We cannot tell what
        // it touches, so we do not let it.
        let v = decide(
            &json!({"tool_name":"apply_patch","tool_input":{"patch":"*** Update File: /etc/passwd"},"cwd":CWD})
                .to_string(),
            &guards(),
            CWD,
        );
        assert!(!v.allow, "{}", v.reason);
    }

    #[test]
    fn camelCase_field_names_are_accepted_too() {
        // The wire contract is a Phase-0 kill-test; accepting both spellings means a
        // naming difference cannot silently turn every call into a denial.
        let v = decide(
            &json!({"toolName":"Bash","toolInput":{"command":"ls -la"},"cwd":CWD}).to_string(),
            &guards(),
            CWD,
        );
        assert!(v.allow, "{}", v.reason);
    }

    #[test]
    fn installing_preserves_hooks_the_user_already_had() {
        let existing = json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type":"command","command":"my-own-linter"}]}
                ],
                "Stop": [{"matcher":"","hooks":[{"type":"command","command":"notify-me"}]}]
            }
        });
        let merged = merge_hook_settings(&existing, "agentrelay hook").unwrap();

        let pre = merged["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2, "ours is added, theirs is kept");
        assert_eq!(pre[0]["hooks"][0]["command"], "my-own-linter");
        assert_eq!(pre[1]["hooks"][0]["command"], "agentrelay hook");
        // Nothing else in their settings is disturbed.
        assert_eq!(merged["model"], "opus");
        assert_eq!(merged["hooks"]["Stop"][0]["hooks"][0]["command"], "notify-me");
    }

    #[test]
    fn installing_twice_does_nothing_the_second_time() {
        let once = merge_hook_settings(&json!({}), "agentrelay hook").unwrap();
        assert!(merge_hook_settings(&once, "agentrelay hook").is_none());
    }

    #[test]
    fn uninstalling_removes_only_ours() {
        let existing = json!({
            "hooks": {"PreToolUse": [
                {"matcher": "Bash", "hooks": [{"type":"command","command":"my-own-linter"}]}
            ]}
        });
        let installed = merge_hook_settings(&existing, "agentrelay hook").unwrap();
        let removed = remove_hook_settings(&installed, "agentrelay hook");
        let pre = removed["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["command"], "my-own-linter");
    }

    #[test]
    fn malformed_settings_are_not_rewritten() {
        // Someone's settings.json has `hooks: "yes"`. Overwriting it would be worse
        // than not installing.
        assert!(merge_hook_settings(&json!({"hooks": "yes"}), "agentrelay hook").is_none());
    }
}
