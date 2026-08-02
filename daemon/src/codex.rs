//! Codex — `codex exec --json` (plan §ו.2, Phase-3).
//!
//! Codex has two surfaces: a long-lived app-server speaking JSON-RPC, and a
//! one-shot `codex exec --json` that streams NDJSON and exits. This uses the
//! one-shot, for the same reason the Claude adapter uses `-p`: a unit of overnight
//! work is a bounded task, and a process that exits when the task ends is a process
//! that cannot be left running by a crash. The app-server is worth adding when
//! steering mid-turn becomes a requirement; it is not one yet.
//!
//! Enforcement here is weaker than for Claude or ACP, and that has to be said out
//! loud rather than papered over. Claude blocks via a PreToolUse hook and ACP blocks
//! on a permission reply — both are gates the agent waits at. `codex exec` streams
//! events as it goes, so the supervisor watches and interrupts, which is a reaction
//! on an asynchronous pipe. `capabilities()` reports `guards: false` for exactly
//! this reason, and a guard violation kills the process tree rather than pretending
//! the action was prevented.

use crate::events::{AgentEvent, Provenance, Usage};
use serde_json::Value;

/// How well we can actually enforce policy on this provider.
///
/// Stated as a value rather than a comment because the difference is material: on a
/// blocking provider a forbidden action does not happen, and here it may already
/// have happened by the time we see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// The agent waits for our answer. A refusal means it did not happen.
    Blocking,
    /// We see it in the stream and stop the run. It may already have happened.
    Reactive,
}

impl Enforcement {
    pub fn describe(&self) -> &'static str {
        match self {
            Enforcement::Blocking => {
                "Guards BLOCK: the agent waits for a decision, so a refused action never runs."
            }
            Enforcement::Reactive => {
                "Guards REACT: this provider does not ask before acting, so AgentRelay stops the \
                 run when it sees a forbidden action — which may be a moment after it started. \
                 Use isolation for anything that matters."
            }
        }
    }
}

pub const CODEX_ENFORCEMENT: Enforcement = Enforcement::Reactive;

/// Arguments for a one-shot Codex run.
pub fn exec_args(prompt: &str) -> Vec<String> {
    vec!["exec".into(), "--json".into(), prompt.into()]
}

fn usage_from(v: &Value) -> Usage {
    let g = |k: &str| v.get(k).and_then(|x| x.as_u64());
    Usage {
        input_tokens: g("input_tokens").or_else(|| g("inputTokens")).unwrap_or(0),
        output_tokens: g("output_tokens").or_else(|| g("outputTokens")).unwrap_or(0),
        cache_read_tokens: g("cached_input_tokens").or_else(|| g("cacheReadTokens")),
        cache_creation_tokens: None,
        total_tokens: g("total_tokens").or_else(|| g("totalTokens")),
        provenance: Provenance::Provider,
    }
}

/// Parse one line of `codex exec --json` output.
///
/// Total: an unrecognised line yields nothing rather than failing. Codex's event
/// vocabulary is still moving, and a parser that rejects what it does not know
/// would break on every release.
pub fn parse_codex_line(line: &Value) -> Vec<AgentEvent> {
    let Some(obj) = line.as_object() else {
        return vec![];
    };
    // The event name lives under different keys across versions; accept any.
    let kind = ["type", "msg_type", "event"]
        .iter()
        .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
        .unwrap_or("");

    // Some versions nest the payload, some do not.
    let body = obj.get("msg").or_else(|| obj.get("data")).unwrap_or(line);

    let text_of = |v: &Value| -> Option<String> {
        ["text", "message", "delta", "content"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|t| t.as_str()))
            .map(String::from)
    };

    match kind {
        k if k.contains("task_started") || k == "session.created" => vec![AgentEvent::TurnStarted],

        // Reasoning is the model's scratchpad. Same rule as every other adapter
        // here: it is not the answer and must not enter the timeline as one.
        k if k.contains("reasoning") || k.contains("thinking") => vec![],

        k if k.contains("agent_message") || k.contains("message.delta") => {
            text_of(body).map(|text| vec![AgentEvent::TextDelta { text }]).unwrap_or_default()
        }

        // A command Codex is about to run. This is the guard's only chance, and it
        // is a chance to react rather than to prevent.
        k if k.contains("exec_command_begin") || k.contains("command.start") => {
            let cmd = body
                .get("command")
                .and_then(|c| match c {
                    // Codex reports argv as an array in some versions.
                    Value::Array(a) => Some(
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            if cmd.is_empty() {
                vec![]
            } else {
                // Reported as a Bash ToolPre so the existing guards apply unchanged.
                vec![AgentEvent::ToolPre {
                    tool: "Bash".into(),
                    args: serde_json::json!({ "command": cmd }),
                }]
            }
        }

        k if k.contains("patch_apply_begin") || k.contains("file.write") => {
            // A file edit. The path may be a map of path -> change.
            let paths: Vec<String> = body
                .get("changes")
                .and_then(|c| c.as_object())
                .map(|m| m.keys().cloned().collect())
                .or_else(|| body.get("path").and_then(|p| p.as_str()).map(|p| vec![p.to_string()]))
                .unwrap_or_default();
            paths
                .into_iter()
                .map(|path| AgentEvent::ToolPre {
                    tool: "Write".into(),
                    args: serde_json::json!({ "file_path": path }),
                })
                .collect()
        }

        k if k.contains("token_count") || k.contains("tokenUsage") || k.contains("usage") => {
            let u = body.get("info").or_else(|| body.get("usage")).unwrap_or(body);
            vec![AgentEvent::TokenUsage { usage: usage_from(u) }]
        }

        k if k.contains("task_complete") || k.contains("turn.completed") => {
            let u = body.get("usage").unwrap_or(&Value::Null);
            vec![AgentEvent::TurnCompleted { usage: usage_from(u) }]
        }

        k if k.contains("error") => {
            let message = text_of(body).unwrap_or_else(|| "codex error".into());
            let rate_limited = crate::claude_stream::looks_like_rate_limit(&message);
            if rate_limited {
                vec![
                    AgentEvent::RateLimited {
                        reset_at: crate::claude_stream::parse_reset_at(&message).unwrap_or_default(),
                        window: None,
                        provider: Some("codex".into()),
                    },
                    AgentEvent::Error { message, code: Some("quota.rate_limited".into()) },
                ]
            } else {
                vec![AgentEvent::Error { message, code: Some("provider.stream_error".into()) }]
            }
        }

        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_enforcement_difference_is_stated_not_hidden() {
        // A supervisor that claims to block when it can only react is lying about
        // the one thing the user is trusting it for.
        assert_eq!(CODEX_ENFORCEMENT, Enforcement::Reactive);
        let d = CODEX_ENFORCEMENT.describe();
        assert!(d.contains("REACT"), "{d}");
        assert!(d.contains("may be a moment after it started"), "{d}");
        assert!(d.contains("Use isolation"), "the honest mitigation: {d}");
        assert!(Enforcement::Blocking.describe().contains("never runs"));
    }

    #[test]
    fn a_command_becomes_the_event_the_guards_already_understand() {
        // Reported as a Bash ToolPre so no guard needs to learn about Codex.
        let ev = parse_codex_line(&json!({
            "type": "exec_command_begin",
            "msg": {"command": ["git", "push", "origin", "main"]}
        }));
        match &ev[0] {
            AgentEvent::ToolPre { tool, args } => {
                assert_eq!(tool, "Bash");
                assert_eq!(args["command"], "git push origin main");
                let actions = crate::guard::derive_actions(tool, args);
                let guards = vec![crate::provider::Guard::new(crate::provider::GuardKind::NoPush)];
                assert!(!crate::guard::evaluate_guards(&actions, &guards, "/w").allow);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_command_given_as_a_string_works_too() {
        let ev = parse_codex_line(&json!({"type":"exec_command_begin","msg":{"command":"npm test"}}));
        assert!(matches!(&ev[0], AgentEvent::ToolPre { args, .. } if args["command"] == "npm test"));
    }

    #[test]
    fn a_patch_becomes_one_write_event_per_file() {
        let ev = parse_codex_line(&json!({
            "type": "patch_apply_begin",
            "msg": {"changes": {"src/a.rs": {"add": 1}, "src/b.rs": {"add": 2}}}
        }));
        assert_eq!(ev.len(), 2);
        let paths: Vec<String> = ev
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolPre { args, .. } => {
                    args["file_path"].as_str().map(String::from)
                }
                _ => None,
            })
            .collect();
        assert!(paths.contains(&"src/a.rs".to_string()), "{paths:?}");
        assert!(paths.contains(&"src/b.rs".to_string()), "{paths:?}");
    }

    #[test]
    fn reasoning_is_not_the_answer() {
        assert!(parse_codex_line(&json!({"type":"agent_reasoning","msg":{"text":"hmm"}})).is_empty());
    }

    #[test]
    fn assistant_text_comes_through() {
        let ev = parse_codex_line(&json!({"type":"agent_message","msg":{"message":"done"}}));
        assert!(matches!(&ev[0], AgentEvent::TextDelta { text } if text == "done"));
    }

    #[test]
    fn token_counts_are_read_so_the_budget_is_not_blind() {
        let ev = parse_codex_line(&json!({
            "type": "token_count",
            "msg": {"info": {"input_tokens": 100, "output_tokens": 20, "cached_input_tokens": 5}}
        }));
        match &ev[0] {
            AgentEvent::TokenUsage { usage } => {
                assert_eq!(usage.input_tokens, 100);
                assert_eq!(usage.output_tokens, 20);
                assert_eq!(usage.cache_read_tokens, Some(5));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_rate_limit_reaches_the_quota_layer() {
        // The bug already found once in the Claude parser: detection that emits an
        // event nothing listens for. RateLimited is what the quota layer matches on.
        let ev = parse_codex_line(&json!({
            "type": "error",
            "msg": {"message": "rate limit exceeded, try again after 2026-08-03T04:00:00Z"}
        }));
        assert!(ev.iter().any(|e| matches!(e, AgentEvent::RateLimited { .. })), "{ev:?}");
        match ev.iter().find(|e| matches!(e, AgentEvent::RateLimited { .. })) {
            Some(AgentEvent::RateLimited { reset_at, provider, .. }) => {
                assert_eq!(reset_at, "2026-08-03T04:00:00Z");
                assert_eq!(provider.as_deref(), Some("codex"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn an_ordinary_error_is_not_mistaken_for_a_quota_problem() {
        let ev = parse_codex_line(&json!({"type":"error","msg":{"message":"compile failed"}}));
        assert!(!ev.iter().any(|e| matches!(e, AgentEvent::RateLimited { .. })));
        assert!(matches!(&ev[0], AgentEvent::Error { code: Some(c), .. } if c == "provider.stream_error"));
    }

    #[test]
    fn unknown_lines_are_ignored_rather_than_fatal() {
        // Codex's event vocabulary is still moving; rejecting the unfamiliar would
        // break the adapter on every release.
        for v in [
            json!({}),
            json!({"type": "something_new_in_v9"}),
            json!("not an object"),
            json!({"msg": {"text": "no type at all"}}),
        ] {
            let _ = parse_codex_line(&v); // must not panic
        }
    }

    #[test]
    fn the_exec_arguments_are_the_one_shot_form() {
        let a = exec_args("do the thing");
        assert_eq!(a[0], "exec");
        assert!(a.contains(&"--json".to_string()));
        assert_eq!(a.last().unwrap(), "do the thing");
    }
}
