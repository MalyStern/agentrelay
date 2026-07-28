//! Claude adapter core — Rust port of `runner/src/providers/claude/stream-json.ts`.
//! Parse `claude -p --output-format stream-json` NDJSON into normalized AgentEvents.
//! Pure; fully testable with sample lines. Defensive + flagged for a live contract
//! test (plan 02 §8.7).

use crate::events::{AgentEvent, Provenance, Usage};
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
        assert!(matches!(&r.events[0], AgentEvent::Error { code: Some(c), .. } if c == "quota.rate_limited"));
        let r2 = parse_claude_line(&json!({"type":"result","is_error":true,"error":"boom"}));
        assert!(matches!(&r2.events[0], AgentEvent::Error { code: Some(c), .. } if c == "provider.stream_error"));
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
