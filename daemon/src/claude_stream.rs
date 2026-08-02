//! Claude adapter core — Rust port of `runner/src/providers/claude/stream-json.ts`.
//! Parse `claude -p --output-format stream-json` NDJSON into normalized AgentEvents.
//! Pure; fully testable with sample lines. Defensive + flagged for a live contract
//! test (plan 02 §8.7).

use crate::events::{AgentEvent, Provenance, QuotaWindow, Usage};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClaudeParseResult {
    pub events: Vec<AgentEvent>,
    pub session_id: Option<String>,
    pub done: bool,
}

static RE_RATE_LIMIT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)rate.?limit|usage limit|too many requests|\b429\b|quota (?:exceeded|reached)").unwrap()
});

pub fn looks_like_rate_limit(text: &str) -> bool {
    RE_RATE_LIMIT.is_match(text)
}

/// Providers word the reset time differently and sometimes omit it entirely, so
/// this pulls out an RFC-3339 timestamp or a unix epoch if one is present and
/// returns None otherwise. An absent reset is not an error: the quota layer already
/// falls back to a conservative wait, and inventing a time would be worse than
/// admitting we do not know one.
pub fn parse_reset_at(text: &str) -> Option<String> {
    static RE_RFC3339: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?(?:[Zz]|[+-]\d{2}:?\d{2})?")
            .unwrap()
    });
    if let Some(m) = RE_RFC3339.find(text) {
        return Some(m.as_str().to_string());
    }
    // e.g. "resets at 1735689600" — a bare 10-digit epoch in a plausible range.
    static RE_EPOCH: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(1[6-9]\d{8}|2[0-9]\d{8})\b").unwrap());
    let secs: i64 = RE_EPOCH.find(text)?.as_str().parse().ok()?;
    chrono::DateTime::from_timestamp(secs, 0).map(|d| d.to_rfc3339())
}

fn u64_of(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(|x| x.as_u64())
}

fn map_usage(u: &Value) -> Usage {
    Usage {
        input_tokens: u64_of(u, "input_tokens").unwrap_or(0),
        output_tokens: u64_of(u, "output_tokens").unwrap_or(0),
        cache_read_tokens: u64_of(u, "cache_read_input_tokens"),
        cache_creation_tokens: u64_of(u, "cache_creation_input_tokens"),
        total_tokens: None,
        provenance: Provenance::Provider,
    }
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
}

pub fn parse_claude_line(line: &Value) -> ClaudeParseResult {
    let obj = match line.as_object() {
        Some(o) => o,
        None => return ClaudeParseResult::default(),
    };
    let session_id = obj.get("session_id").and_then(|v| v.as_str()).map(String::from);
    let mut out = ClaudeParseResult {
        session_id: session_id.clone(),
        ..Default::default()
    };
    let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match ty {
        "system" => {
            if obj.get("subtype").and_then(|v| v.as_str()) == Some("init") {
                out.events.push(AgentEvent::TurnStarted);
            }
        }
        "assistant" => {
            if let Some(content) = obj.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                for block in content {
                    match block.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            if let Some(text) = str_of(block, "text") {
                                out.events.push(AgentEvent::TextDelta { text });
                            }
                        }
                        Some("tool_use") => {
                            if let Some(tool) = str_of(block, "name") {
                                out.events.push(AgentEvent::ToolPre {
                                    tool,
                                    args: block.get("input").cloned().unwrap_or(Value::Null),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        "user" => {
            if let Some(content) = obj.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                        out.events.push(AgentEvent::ToolPost {
                            tool: str_of(block, "tool_use_id").unwrap_or_else(|| "tool".into()),
                            result: block.get("content").cloned().unwrap_or(Value::Null),
                        });
                    }
                }
            }
        }
        // A live quota signal — and the plan said there wasn't one.
        //
        // §ז.2 was written on the assumption that Claude headless gives no running
        // counter, so quota handling had to be purely reactive: hit the wall, then
        // work out when to come back. A contract test against claude 2.1.220 shows
        // otherwise. Every turn emits:
        //
        //   {"type":"rate_limit_event","rate_limit_info":{
        //      "status":"allowed","resetsAt":1785648600,"rateLimitType":"five_hour",...}}
        //
        // `resetsAt` is a unix epoch and it arrives BEFORE the limit is reached, so
        // the supervisor can know when the window reopens without having to hit it
        // first — and can schedule around the wall instead of walking into it.
        "rate_limit_event" => {
            let info = obj.get("rate_limit_info").unwrap_or(&Value::Null);
            let status = info.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let reset_at = info
                .get("resetsAt")
                .and_then(|v| v.as_i64())
                .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
                .map(|d| d.to_rfc3339())
                .unwrap_or_default();
            let window = match info.get("rateLimitType").and_then(|v| v.as_str()) {
                Some("five_hour") => Some(QuotaWindow::FiveHour),
                Some("weekly") => Some(QuotaWindow::Weekly),
                _ => None,
            };

            // "allowed" is the healthy case: record where the wall is, do not act.
            // Anything else means we are at or past it.
            if status == "allowed" {
                out.events.push(AgentEvent::QuotaHint {
                    reset_at,
                    window,
                    provider: Some("claude".into()),
                });
            } else {
                out.events.push(AgentEvent::RateLimited {
                    reset_at,
                    window,
                    provider: Some("claude".into()),
                });
            }
        }
        "result" => {
            out.done = true;
            let usage = map_usage(obj.get("usage").unwrap_or(&Value::Null));
            let subtype = obj.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
            let is_error = obj.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false)
                || subtype.starts_with("error");
            let text = [obj.get("result"), obj.get("error")]
                .iter()
                .filter_map(|v| v.and_then(|x| x.as_str()))
                .chain(std::iter::once(subtype))
                .collect::<Vec<_>>()
                .join(" ");
            if is_error && looks_like_rate_limit(&text) {
                // Emit RateLimited, not just Error. The entire quota layer —
                // QuotaTracker::interpret_event, the resume plan, the wait until the
                // window reopens — matches on AgentEvent::RateLimited, and nothing
                // outside tests was constructing one. Detection worked and then went
                // nowhere: the run simply ended, with no resume and no explanation.
                // This is the single line that connects "we hit the quota" to
                // "come back when it resets", which is the product's whole reason to
                // exist.
                out.events.push(AgentEvent::RateLimited {
                    reset_at: parse_reset_at(&text).unwrap_or_default(),
                    window: None,
                    provider: Some("claude".into()),
                });
                // The Error is kept as well so the timeline still shows what the
                // provider actually said.
                out.events.push(AgentEvent::Error {
                    message: if text.trim().is_empty() { "rate limited".into() } else { text },
                    code: Some("quota.rate_limited".into()),
                });
            } else if is_error {
                out.events.push(AgentEvent::Error {
                    message: if text.trim().is_empty() { "error".into() } else { text },
                    code: Some("provider.stream_error".into()),
                });
            } else {
                out.events.push(AgentEvent::TurnCompleted { usage });
            }
        }
        _ => {}
    }
    out
}

/// Buffered NDJSON reader: feed stdout chunks, get normalized events. Survives
/// chunk-split lines and tolerates non-JSON noise. `flush` recovers a newline-less
/// final line (fix #5).
#[derive(Default)]
pub struct ClaudeStreamParser {
    buffer: String,
    session_id: Option<String>,
    done: bool,
}

impl ClaudeStreamParser {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    pub fn done(&self) -> bool {
        self.done
    }

    fn consume(&mut self, line: &str) -> Vec<AgentEvent> {
        let line = line.trim();
        if line.is_empty() {
            return vec![];
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) => {
                let r = parse_claude_line(&v);
                if r.session_id.is_some() {
                    self.session_id = r.session_id;
                }
                if r.done {
                    self.done = true;
                }
                r.events
            }
            Err(_) => vec![], // tolerate non-JSON noise
        }
    }

    pub fn push(&mut self, chunk: &str) -> Vec<AgentEvent> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        while let Some(nl) = self.buffer.find('\n') {
            let line: String = self.buffer.drain(..=nl).collect();
            events.extend(self.consume(&line));
        }
        events
    }

    pub fn flush(&mut self) -> Vec<AgentEvent> {
        let line = std::mem::take(&mut self.buffer);
        self.consume(&line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn init_gives_turn_started_and_session() {
        let r = parse_claude_line(&json!({"type":"system","subtype":"init","session_id":"sess_abc"}));
        assert_eq!(r.events, vec![AgentEvent::TurnStarted]);
        assert_eq!(r.session_id.as_deref(), Some("sess_abc"));
    }

    #[test]
    fn assistant_text_and_tool() {
        let r = parse_claude_line(&json!({"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}));
        assert_eq!(r.events, vec![AgentEvent::TextDelta { text: "hello".into() }]);
        let r2 = parse_claude_line(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}));
        assert_eq!(r2.events, vec![AgentEvent::ToolPre { tool: "Bash".into(), args: json!({"command":"ls"}) }]);
    }

    #[test]
    fn user_tool_result() {
        let r = parse_claude_line(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}));
        assert_eq!(r.events, vec![AgentEvent::ToolPost { tool: "t1".into(), result: json!("ok") }]);
    }

    #[test]
    fn result_success_maps_usage() {
        let r = parse_claude_line(&json!({"type":"result","subtype":"success","is_error":false,"session_id":"s","usage":{"input_tokens":100,"output_tokens":40,"cache_read_input_tokens":20}}));
        assert!(r.done);
        assert_eq!(r.events, vec![AgentEvent::TurnCompleted { usage: Usage { input_tokens: 100, output_tokens: 40, cache_read_tokens: Some(20), cache_creation_tokens: None, total_tokens: None, provenance: Provenance::Provider } }]);
    }

    #[test]
    fn result_rate_limit_and_generic_error() {
        let r = parse_claude_line(&json!({"type":"result","subtype":"error_during_execution","is_error":true,"result":"You have hit the usage limit"}));
        // A rate limit now yields BOTH: the RateLimited the quota layer acts on,
        // and the Error the timeline shows.
        assert!(r.events.iter().any(|e| matches!(e, AgentEvent::RateLimited { .. })));
        assert!(r.events.iter().any(
            |e| matches!(e, AgentEvent::Error { code: Some(c), .. } if c == "quota.rate_limited")));
        let r2 = parse_claude_line(&json!({"type":"result","is_error":true,"error":"boom"}));
        assert!(matches!(&r2.events[0], AgentEvent::Error { code: Some(c), .. } if c == "provider.stream_error"));
    }

    #[test]
    fn the_live_quota_signal_is_read_and_does_not_stop_a_healthy_turn() {
        // Copied verbatim from a contract test against claude 2.1.220. The plan
        // assumed no live counter existed and built the whole quota layer to be
        // reactive; this line arrives on every turn and says where the wall is.
        let r = parse_claude_line(&json!({
            "type": "rate_limit_event",
            "rate_limit_info": {
                "status": "allowed",
                "resetsAt": 1785648600i64,
                "rateLimitType": "five_hour",
                "overageStatus": "rejected"
            },
            "session_id": "s1"
        }));
        match r.events.first() {
            Some(AgentEvent::QuotaHint { reset_at, window, .. }) => {
                assert!(reset_at.starts_with("2026-"), "epoch decoded: {reset_at}");
                assert_eq!(*window, Some(QuotaWindow::FiveHour));
            }
            other => panic!("expected a QuotaHint, got {other:?}"),
        }
        // Critically NOT RateLimited: status is "allowed". Treating a healthy
        // heartbeat as the wall would abort every turn the moment it started.
        assert!(!r.events.iter().any(|e| matches!(e, AgentEvent::RateLimited { .. })));
        assert!(!r.done);
    }

    #[test]
    fn a_rate_limit_event_that_is_not_allowed_is_the_wall_itself() {
        let r = parse_claude_line(&json!({
            "type": "rate_limit_event",
            "rate_limit_info": {"status":"blocked","resetsAt": 1785648600i64,"rateLimitType":"weekly"}
        }));
        match r.events.first() {
            Some(AgentEvent::RateLimited { window, reset_at, .. }) => {
                assert_eq!(*window, Some(QuotaWindow::Weekly));
                assert!(!reset_at.is_empty());
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn the_real_result_line_maps_to_usage_we_can_actually_count() {
        // Also from the live capture — the field names are the contract.
        let r = parse_claude_line(&json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "OK",
            "usage": {
                "input_tokens": 10,
                "cache_creation_input_tokens": 8807,
                "cache_read_input_tokens": 24405,
                "output_tokens": 37
            }
        }));
        assert!(r.done);
        match r.events.first() {
            Some(AgentEvent::TurnCompleted { usage }) => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 37);
                assert_eq!(usage.cache_read_tokens, Some(24405));
                assert_eq!(usage.cache_creation_tokens, Some(8807));
            }
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    #[test]
    fn a_thinking_block_is_not_mistaken_for_the_answer() {
        // Real assistant messages carry `thinking` blocks alongside `text`. Only the
        // text is the reply; emitting thinking as TextDelta would put the model's
        // scratchpad into the timeline and the report.
        let r = parse_claude_line(&json!({
            "type": "assistant",
            "message": {"content": [
                {"type": "thinking", "thinking": "the user wants OK", "signature": "x"},
                {"type": "text", "text": "OK"}
            ]}
        }));
        let texts: Vec<&str> = r
            .events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["OK"]);
    }

    #[test]
    fn a_real_rate_limit_result_emits_the_event_the_quota_layer_listens_for() {
        // The bug this pins: the parser emitted only Error{code:"quota.rate_limited"},
        // while every consumer — QuotaTracker, the resume plan, the wait — matches
        // on AgentEvent::RateLimited. Detection worked and led nowhere, so a real
        // night just stopped at the quota and never came back.
        let r = parse_claude_line(&json!({
            "type": "result",
            "is_error": true,
            "subtype": "error_rate_limit",
            "error": "rate limit exceeded, resets at 2026-08-03T04:00:00Z"
        }));
        assert!(
            r.events.iter().any(|e| matches!(e, AgentEvent::RateLimited { .. })),
            "no RateLimited emitted: {:?}",
            r.events
        );
        match r.events.iter().find(|e| matches!(e, AgentEvent::RateLimited { .. })) {
            Some(AgentEvent::RateLimited { reset_at, provider, .. }) => {
                assert_eq!(reset_at, "2026-08-03T04:00:00Z", "the reset time is carried through");
                assert_eq!(provider.as_deref(), Some("claude"));
            }
            _ => unreachable!(),
        }
        // The provider's own words survive too, for the timeline.
        assert!(r.events.iter().any(
            |e| matches!(e, AgentEvent::Error { code: Some(c), .. } if c == "quota.rate_limited")
        ));
    }

    #[test]
    fn a_rate_limit_without_a_reset_time_still_emits_the_event() {
        // Not knowing when the window reopens is normal; the quota layer has a
        // conservative fallback. Dropping the event because a field is missing
        // would turn a recoverable pause into a dead run.
        let r = parse_claude_line(&json!({"type":"result","is_error":true,"error":"429 too many requests"}));
        match r.events.iter().find(|e| matches!(e, AgentEvent::RateLimited { .. })) {
            Some(AgentEvent::RateLimited { reset_at, .. }) => assert!(reset_at.is_empty()),
            _ => panic!("expected RateLimited: {:?}", r.events),
        }
    }

    #[test]
    fn an_ordinary_error_is_not_mistaken_for_a_rate_limit() {
        let r = parse_claude_line(&json!({"type":"result","is_error":true,"error":"compile failed"}));
        assert!(!r.events.iter().any(|e| matches!(e, AgentEvent::RateLimited { .. })));
    }

    #[test]
    fn reset_times_are_parsed_from_the_shapes_providers_actually_use() {
        assert_eq!(
            parse_reset_at("try again after 2026-08-03T04:00:00Z").as_deref(),
            Some("2026-08-03T04:00:00Z")
        );
        assert!(parse_reset_at("resets at 1893456000").is_some(), "unix epoch");
        assert_eq!(parse_reset_at("rate limited, try later"), None);
        // A version number must not be read as a timestamp.
        assert_eq!(parse_reset_at("rate limit in v1.2.3"), None);
    }

    #[test]
    fn looks_like_rate_limit_works() {
        for s in ["rate limit exceeded", "usage limit reached", "Error 429", "too many requests"] {
            assert!(looks_like_rate_limit(s), "{s}");
        }
        assert!(!looks_like_rate_limit("build succeeded"));
    }

    #[test]
    fn parser_across_chunk_boundaries() {
        let lines = [
            json!({"type":"system","subtype":"init","session_id":"sess_1"}).to_string(),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"t"}]}}).to_string(),
            json!({"type":"result","subtype":"success","session_id":"sess_1","usage":{"input_tokens":5,"output_tokens":2}}).to_string(),
        ];
        let ndjson = format!("{}\n", lines.join("\n"));
        let mut parser = ClaudeStreamParser::new();
        let mid = ndjson.len() / 2;
        // Split at a byte boundary that lands on an ASCII char (JSON here is ASCII).
        let mut events = parser.push(&ndjson[..mid]);
        events.extend(parser.push(&ndjson[mid..]));
        assert_eq!(
            events.iter().map(|e| std::mem::discriminant(e)).collect::<Vec<_>>(),
            [AgentEvent::TurnStarted, AgentEvent::TextDelta { text: String::new() }, AgentEvent::TurnCompleted { usage: Usage::zero() }]
                .iter().map(std::mem::discriminant).collect::<Vec<_>>()
        );
        assert_eq!(parser.session_id(), Some("sess_1"));
        assert!(parser.done());
    }

    #[test]
    fn flush_recovers_newlineless_final_line() {
        let mut parser = ClaudeStreamParser::new();
        parser.push(&json!({"type":"result","subtype":"success","session_id":"sess_1","usage":{"input_tokens":1,"output_tokens":1}}).to_string());
        assert!(!parser.done()); // no newline yet
        let events = parser.flush();
        assert!(matches!(events[0], AgentEvent::TurnCompleted { .. }));
        assert!(parser.done());
        assert_eq!(parser.session_id(), Some("sess_1"));
    }
}
