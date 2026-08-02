//! ACP — the Agent Client Protocol (plan §ו.3, Phase-3).
//!
//! ACP is JSON-RPC 2.0 over stdio: roughly "LSP for coding agents". Around
//! twenty-five of them already speak it — Gemini CLI, Codex, OpenCode, Copilot CLI,
//! Junie, Cline, Goose, OpenHands — which is why this is worth more than any single
//! adapter. One protocol covers the field; a bespoke adapter covers one agent and
//! rots when that agent changes its CLI flags.
//!
//! This module is the wire layer only: framing, request/response correlation, and
//! translating ACP notifications into the same `AgentEvent` stream every other part
//! of the daemon already consumes. Everything above it — guards, checkpoints, the
//! budget, the report — works unchanged, which is the whole reason the event stream
//! is normalised in the first place.
//!
//! It is written against the published protocol shape and is **not** yet verified
//! against a live agent, so it is defensive throughout: an unrecognised method is
//! ignored rather than fatal, and a malformed frame never takes the run down. When
//! the contract test happens, the failures should be missing features rather than
//! crashes.

use crate::events::{AgentEvent, Provenance, Usage};
use serde_json::{json, Value};

/// Build a JSON-RPC request. `id` correlates the eventual response.
pub fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// A notification — no id, so no response is expected.
pub fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

/// What a parsed incoming frame turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A response to a request we sent.
    Response { id: u64, result: Value },
    /// The peer reported an error for one of our requests.
    Error { id: Option<u64>, code: i64, message: String },
    /// A stream update, translated into our own events. May be empty when the
    /// notification carries nothing we model.
    Events(Vec<AgentEvent>),
    /// The agent is asking permission to do something. Carries the request id so a
    /// decision can be sent back — this is where our guards get their say.
    PermissionRequest { id: u64, tool: String, args: Value },
    /// Understood as valid JSON-RPC, but nothing we act on.
    Ignored,
}

fn usage_from(v: &Value) -> Usage {
    let g = |k: &str| v.get(k).and_then(|x| x.as_u64());
    Usage {
        // Both spellings appear in the wild; accepting one and silently zeroing the
        // other would make every turn look free.
        input_tokens: g("inputTokens").or_else(|| g("input_tokens")).unwrap_or(0),
        output_tokens: g("outputTokens").or_else(|| g("output_tokens")).unwrap_or(0),
        cache_read_tokens: g("cacheReadTokens").or_else(|| g("cache_read_tokens")),
        cache_creation_tokens: g("cacheCreationTokens").or_else(|| g("cache_creation_tokens")),
        total_tokens: g("totalTokens").or_else(|| g("total_tokens")),
        provenance: Provenance::Provider,
    }
}

/// Parse one incoming frame.
///
/// Never fails: an unparseable or unknown frame is `Ignored`. A protocol layer that
/// panics on an unexpected message would make every new agent version a crash.
pub fn parse_incoming(line: &str) -> Incoming {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Incoming::Ignored;
    };

    if let Some(err) = v.get("error") {
        return Incoming::Error {
            id: v.get("id").and_then(|i| i.as_u64()),
            code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
            message: err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string(),
        };
    }

    // A response carries both an id and a result.
    if let (Some(id), Some(result)) = (v.get("id").and_then(|i| i.as_u64()), v.get("result")) {
        return Incoming::Response { id, result: result.clone() };
    }

    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = v.get("params").cloned().unwrap_or(Value::Null);

    // A permission request is a REQUEST (it has an id) and must be answered.
    if method.contains("requestPermission") || method.contains("permission/request") {
        if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
            let tool = params
                .pointer("/toolCall/title")
                .or_else(|| params.pointer("/toolCall/kind"))
                .or_else(|| params.get("tool"))
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            let args = params
                .pointer("/toolCall/rawInput")
                .or_else(|| params.get("arguments"))
                .cloned()
                .unwrap_or(Value::Null);
            return Incoming::PermissionRequest { id, tool, args };
        }
    }

    match method {
        "session/update" => Incoming::Events(session_update_events(&params)),
        _ => Incoming::Ignored,
    }
}

fn session_update_events(params: &Value) -> Vec<AgentEvent> {
    let update = params.get("update").unwrap_or(params);
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("type"))
        .and_then(|k| k.as_str())
        .unwrap_or("");

    let text_of = |v: &Value| -> Option<String> {
        v.pointer("/content/text")
            .or_else(|| v.get("text"))
            .and_then(|t| t.as_str())
            .map(String::from)
    };

    match kind {
        "agent_message_chunk" => text_of(update)
            .map(|text| vec![AgentEvent::TextDelta { text }])
            .unwrap_or_default(),
        // Thinking is the model's scratchpad, not its answer. Emitting it as text
        // would put it in the timeline and the morning report as though it were.
        "agent_thought_chunk" => vec![],
        "tool_call" => {
            let tool = update
                .get("title")
                .or_else(|| update.get("kind"))
                .and_then(|t| t.as_str())
                .unwrap_or("tool")
                .to_string();
            vec![AgentEvent::ToolPre {
                tool,
                args: update.get("rawInput").cloned().unwrap_or(Value::Null),
            }]
        }
        "tool_call_update" => {
            // Only a finished call is a ToolPost; an in-progress update is noise.
            let status = update.get("status").and_then(|s| s.as_str()).unwrap_or("");
            if status == "completed" || status == "failed" {
                let tool = update
                    .get("toolCallId")
                    .or_else(|| update.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("tool")
                    .to_string();
                vec![AgentEvent::ToolPost {
                    tool,
                    result: update.get("rawOutput").cloned().unwrap_or(Value::Null),
                }]
            } else {
                vec![]
            }
        }
        _ => {
            // Token usage arrives under several names depending on the agent.
            if let Some(u) = update
                .get("tokenUsage")
                .or_else(|| update.get("usage"))
                .or_else(|| update.get("token_usage"))
            {
                return vec![AgentEvent::TokenUsage { usage: usage_from(u) }];
            }
            vec![]
        }
    }
}

/// The answer to a permission request, in the shape ACP expects.
///
/// `allow` comes from our own guards, so an ACP agent is held to exactly the same
/// rules as Claude — which is the point of normalising at this layer.
pub fn permission_response(id: u64, allow: bool, reason: &str) -> String {
    let outcome = if allow {
        json!({"outcome": "selected", "optionId": "allow"})
    } else {
        // `rejected` rather than "cancelled": the agent should learn the action was
        // refused, not that the user went away.
        json!({"outcome": "rejected", "reason": reason})
    };
    json!({"jsonrpc": "2.0", "id": id, "result": outcome}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_is_matched_back_to_its_request() {
        let out = parse_incoming(r#"{"jsonrpc":"2.0","id":7,"result":{"sessionId":"s1"}}"#);
        match out {
            Incoming::Response { id, result } => {
                assert_eq!(id, 7);
                assert_eq!(result["sessionId"], "s1");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_error_frame_is_surfaced_not_swallowed() {
        let out = parse_incoming(
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"method not found"}}"#,
        );
        match out {
            Incoming::Error { id, code, message } => {
                assert_eq!(id, Some(3));
                assert_eq!(code, -32601);
                assert_eq!(message, "method not found");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn garbage_never_takes_the_run_down() {
        // A protocol layer that panics on an unexpected frame makes every new agent
        // version a crash instead of a missing feature.
        for junk in ["", "not json", "[]", "{}", r#"{"jsonrpc":"2.0"}"#, r#"{"method":"who?"}"#] {
            assert_eq!(parse_incoming(junk), Incoming::Ignored, "{junk:?}");
        }
    }

    #[test]
    fn assistant_text_becomes_a_text_delta() {
        let out = parse_incoming(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{
                 "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}}"#,
        );
        assert_eq!(out, Incoming::Events(vec![AgentEvent::TextDelta { text: "hello".into() }]));
    }

    #[test]
    fn thinking_is_not_emitted_as_the_answer() {
        // Same rule the Claude parser follows: the scratchpad is not the reply, and
        // putting it in the timeline would misrepresent what the agent said.
        let out = parse_incoming(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{
                 "sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}}}"#,
        );
        assert_eq!(out, Incoming::Events(vec![]));
    }

    #[test]
    fn a_tool_call_becomes_the_event_the_guards_already_watch() {
        // The whole value of this module: an ACP agent is guarded by exactly the
        // same code as Claude, because it produces exactly the same events.
        let out = parse_incoming(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{
                 "sessionUpdate":"tool_call","title":"Bash",
                 "rawInput":{"command":"git push origin main"}}}}"#,
        );
        match out {
            Incoming::Events(ev) => match &ev[0] {
                AgentEvent::ToolPre { tool, args } => {
                    assert_eq!(tool, "Bash");
                    assert_eq!(args["command"], "git push origin main");
                    // Prove it: run it through the real guard.
                    let actions = crate::guard::derive_actions(tool, args);
                    let guards = vec![crate::provider::Guard::new(
                        crate::provider::GuardKind::NoPush,
                    )];
                    assert!(!crate::guard::evaluate_guards(&actions, &guards, "/w").allow);
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_a_finished_tool_call_produces_a_post_event() {
        let running = parse_incoming(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{
                 "sessionUpdate":"tool_call_update","status":"in_progress","toolCallId":"t1"}}}"#,
        );
        assert_eq!(running, Incoming::Events(vec![]));

        let done = parse_incoming(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{
                 "sessionUpdate":"tool_call_update","status":"completed","toolCallId":"t1",
                 "rawOutput":{"ok":true}}}}"#,
        );
        match done {
            Incoming::Events(ev) => assert!(matches!(&ev[0], AgentEvent::ToolPost { tool, .. } if tool == "t1")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn token_usage_is_read_under_either_spelling() {
        // camelCase and snake_case both appear across ACP implementations. Reading
        // one and zeroing the other would make a turn look free and disarm the
        // budget.
        for body in [
            r#"{"tokenUsage":{"inputTokens":10,"outputTokens":5}}"#,
            r#"{"usage":{"input_tokens":10,"output_tokens":5}}"#,
        ] {
            let frame = format!(
                r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"update":{body}}}}}"#
            );
            match parse_incoming(&frame) {
                Incoming::Events(ev) => match &ev[0] {
                    AgentEvent::TokenUsage { usage } => {
                        assert_eq!(usage.input_tokens, 10, "{body}");
                        assert_eq!(usage.output_tokens, 5, "{body}");
                    }
                    other => panic!("{other:?}"),
                },
                other => panic!("{other:?} for {body}"),
            }
        }
    }

    #[test]
    fn a_permission_request_carries_the_id_needed_to_answer_it() {
        let out = parse_incoming(
            r#"{"jsonrpc":"2.0","id":42,"method":"session/requestPermission","params":{
                 "toolCall":{"title":"Write","rawInput":{"file_path":"/etc/passwd"}}}}"#,
        );
        match out {
            Incoming::PermissionRequest { id, tool, args } => {
                assert_eq!(id, 42);
                assert_eq!(tool, "Write");
                assert_eq!(args["file_path"], "/etc/passwd");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_refusal_tells_the_agent_it_was_refused_not_that_nobody_answered() {
        let allow = permission_response(42, true, "");
        assert!(allow.contains("\"outcome\":\"selected\""), "{allow}");

        let deny = permission_response(42, false, "blocked by AgentRelay: no_push");
        assert!(deny.contains("\"outcome\":\"rejected\""), "{deny}");
        assert!(deny.contains("no_push"), "{deny}");
        // "cancelled" would read as the user walking away, which invites a retry.
        assert!(!deny.contains("cancelled"), "{deny}");
    }

    #[test]
    fn requests_and_notifications_are_shaped_correctly() {
        let r = request(1, "initialize", json!({"protocolVersion": 1}));
        assert!(r.contains("\"id\":1"));
        assert!(r.contains("\"method\":\"initialize\""));

        let n = notification("session/cancel", json!({"sessionId": "s1"}));
        assert!(!n.contains("\"id\""), "a notification expects no reply: {n}");
    }
}
