//! The ACP adapter: an agent process driven over JSON-RPC/stdio (plan §ו.3, Phase-3).
//!
//! `acp.rs` is the wire format. This is the process: spawn the agent, write frames
//! to its stdin, read frames from its stdout, and turn the result into the same
//! `AgentEvent` stream `ClaudeAdapter` produces — so one adapter covers Gemini CLI,
//! Codex, OpenCode, Copilot CLI, Cline, Goose and the rest, and everything above it
//! (guards, checkpoints, budget, drift, the report) works without knowing which.
//!
//! The design decision that matters here is **where permission is answered**. An ACP
//! agent asks before acting, over the same pipe, and waits for the reply. That makes
//! it the same kind of enforcement as the Claude PreToolUse hook — a block, not a
//! reaction — and it uses the identical `evaluate_guards`. There is exactly one set
//! of rules in this product, and no path around it.
//!
//! Not yet verified against a live ACP agent. Written to fail toward silence rather
//! than toward crashes: unknown frames are ignored, and a permission request we
//! cannot parse is DENIED, because "we could not tell what this was" must never
//! resolve to yes.

use crate::acp::{self, Incoming};
use crate::adapter::EventStream;
use crate::error::{AgentRelayError, ErrorCode, Result};
use crate::events::AgentEvent;
use crate::process::ProcessGroup;
use crate::provider::Guard;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct AcpSession {
    group: Arc<ProcessGroup>,
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicU64,
}

impl AcpSession {
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    pub async fn send(&self, frame: &str) -> Result<()> {
        let mut w = self.stdin.lock().await;
        // Line-delimited: one frame per line, which is what every ACP
        // implementation on stdio uses.
        w.write_all(frame.as_bytes())
            .await
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        w.write_all(b"\n")
            .await
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        w.flush()
            .await
            .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(())
    }

    /// Kill the agent and everything it started.
    pub fn interrupt(&self) -> Result<()> {
        self.group.kill_tree()
    }
}

/// Spawn an ACP agent and return its normalized event stream.
///
/// `guards` are answered inline: when the agent asks permission, the reply is
/// computed from the same guard evaluation the rest of the daemon uses, and written
/// back before the agent can proceed.
pub fn spawn_acp(
    program: &str,
    args: &[String],
    cwd: &str,
    guards: Vec<Guard>,
) -> Result<(EventStream, Arc<AcpSession>)> {
    let mut cmd = crate::which::command_for(program).ok_or_else(|| {
        AgentRelayError::new(
            ErrorCode::ProcessSpawnFailed,
            format!("{program} {}", crate::which::install_hint(program)),
        )
    })?;
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let group = Arc::new(ProcessGroup::new()?);
    let mut child = group.spawn(&mut cmd)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentRelayError::new(ErrorCode::ProcessSpawnFailed, "no stdout"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AgentRelayError::new(ErrorCode::ProcessSpawnFailed, "no stdin"))?;

    let session = Arc::new(AcpSession {
        group: group.clone(),
        stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
        next_id: AtomicU64::new(1),
    });

    let s = session.clone();
    let cwd_owned = cwd.to_string();
    let stream = async_stream::stream! {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match acp::parse_incoming(&line) {
                Incoming::Events(events) => {
                    for e in events {
                        yield e;
                    }
                }
                Incoming::PermissionRequest { id, tool, args } => {
                    // Answered here, from the same rules as everywhere else. The
                    // agent is blocked on this reply, so this is a real gate rather
                    // than a note made after the fact.
                    let actions = crate::guard::derive_actions(&tool, &args);
                    let decision = crate::guard::evaluate_guards(&actions, &guards, &cwd_owned);

                    let reason = decision
                        .violations
                        .iter()
                        .map(|v| v.reason_code.clone())
                        .collect::<Vec<_>>()
                        .join("; ");
                    let reply = acp::permission_response(
                        id,
                        decision.allow,
                        &format!(
                            "Blocked by AgentRelay: {reason}. This is a guard the user configured \
                             for this run — do not retry it and do not work around it."
                        ),
                    );
                    // A write failure here means the agent is gone; the stream will
                    // end on the next read either way.
                    let _ = s.send(&reply).await;

                    if !decision.allow {
                        // Surface it so the morning report can say what was refused,
                        // exactly as the Claude path does.
                        yield AgentEvent::ToolDenied { tool, args };
                    }
                }
                Incoming::Error { code, message, .. } => {
                    yield AgentEvent::Error {
                        message: format!("acp error {code}: {message}"),
                        code: Some("provider.stream_error".into()),
                    };
                }
                Incoming::Response { .. } | Incoming::Ignored => {}
            }
        }
    };

    Ok((Box::pin(stream), session))
}

/// The handshake an ACP session opens with.
pub fn initialize_frame(id: u64) -> String {
    acp::request(
        id,
        "initialize",
        serde_json::json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                // We answer permission requests ourselves — that is the whole point.
                "fs": { "readTextFile": false, "writeTextFile": false }
            }
        }),
    )
}

/// Open a session rooted at a directory.
pub fn new_session_frame(id: u64, cwd: &str) -> String {
    acp::request(
        id,
        "session/new",
        serde_json::json!({ "cwd": cwd, "mcpServers": [] }),
    )
}

/// Send a turn's prompt.
pub fn prompt_frame(id: u64, session_id: &str, text: &str) -> String {
    acp::request(
        id,
        "session/prompt",
        serde_json::json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": text }]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::GuardKind;
    use futures::StreamExt;

    /// A fake ACP agent, in Node: emits frames, and asks permission for a push.
    /// Real subprocess, real pipes — a mocked transport would only prove the mock
    /// and this is precisely where the wiring can be wrong.
    fn fake_agent(script: &str) -> (std::path::PathBuf, Vec<String>) {
        use std::sync::atomic::AtomicUsize;
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ar-acp-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.js");
        std::fs::write(&path, script).unwrap();
        (dir, vec![path.to_string_lossy().to_string()])
    }

    #[tokio::test]
    async fn agent_output_becomes_our_events() {
        let script = r#"
console.log(JSON.stringify({jsonrpc:"2.0",method:"session/update",params:{update:{
  sessionUpdate:"agent_message_chunk", content:{type:"text",text:"working"}}}}));
console.log(JSON.stringify({jsonrpc:"2.0",method:"session/update",params:{update:{
  sessionUpdate:"agent_thought_chunk", content:{type:"text",text:"scratchpad"}}}}));
console.log(JSON.stringify({jsonrpc:"2.0",method:"session/update",params:{update:{
  tokenUsage:{inputTokens:12,outputTokens:3}}}}));
"#;
        let (dir, args) = fake_agent(script);
        let (mut stream, _s) = spawn_acp("node", &args, ".", vec![]).unwrap();

        let mut events = Vec::new();
        while let Some(e) = stream.next().await {
            events.push(e);
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "working")),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "scratchpad")),
            "the scratchpad must not reach the timeline: {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::TokenUsage { usage } if usage.input_tokens == 12)),
            "{events:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_forbidden_action_is_refused_over_the_wire_before_it_happens() {
        // The property that makes ACP worth having: the agent is BLOCKED on our
        // answer, so this is enforcement, not commentary. The fake agent records
        // what we replied so the test can prove the refusal actually arrived.
        let script = r#"
const out = [];
process.stdin.on('data', d => {
  for (const line of String(d).split('\n').filter(Boolean)) {
    const msg = JSON.parse(line);
    // Echo the decision back as a normal message so the test can observe it.
    console.log(JSON.stringify({jsonrpc:"2.0",method:"session/update",params:{update:{
      sessionUpdate:"agent_message_chunk",
      content:{type:"text",text:"DECISION:" + JSON.stringify(msg.result)}}}}));
    process.exit(0);
  }
});
console.log(JSON.stringify({jsonrpc:"2.0",id:9,method:"session/requestPermission",params:{
  toolCall:{title:"Bash", rawInput:{command:"git push origin main"}}}}));
"#;
        let (dir, args) = fake_agent(script);
        let guards = vec![Guard::new(GuardKind::NoPush)];
        let (mut stream, _s) = spawn_acp("node", &args, ".", guards).unwrap();

        let mut events = Vec::new();
        while let Some(e) = stream.next().await {
            events.push(e);
        }

        // We told the report it was denied…
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolDenied { tool, .. } if tool == "Bash")),
            "{events:?}"
        );
        // …and the agent actually received the refusal.
        let decision = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::TextDelta { text } if text.starts_with("DECISION:") => {
                    Some(text.clone())
                }
                _ => None,
            })
            .expect("the agent must have received a reply");
        assert!(decision.contains("rejected"), "{decision}");
        assert!(decision.contains("no_push"), "{decision}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_allowed_action_is_permitted() {
        let script = r#"
process.stdin.on('data', d => {
  const msg = JSON.parse(String(d).split('\n').filter(Boolean)[0]);
  console.log(JSON.stringify({jsonrpc:"2.0",method:"session/update",params:{update:{
    sessionUpdate:"agent_message_chunk",
    content:{type:"text",text:"DECISION:" + JSON.stringify(msg.result)}}}}));
  process.exit(0);
});
console.log(JSON.stringify({jsonrpc:"2.0",id:1,method:"session/requestPermission",params:{
  toolCall:{title:"Bash", rawInput:{command:"npm test"}}}}));
"#;
        let (dir, args) = fake_agent(script);
        let (mut stream, _s) =
            spawn_acp("node", &args, ".", vec![Guard::new(GuardKind::NoPush)]).unwrap();
        let mut texts = Vec::new();
        while let Some(e) = stream.next().await {
            if let AgentEvent::TextDelta { text } = e {
                texts.push(text);
            }
        }
        let d = texts
            .iter()
            .find(|t| t.starts_with("DECISION:"))
            .expect("a reply");
        assert!(d.contains("selected"), "ordinary work must be allowed: {d}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_noisy_agent_does_not_crash_the_stream() {
        // Real agents print banners, warnings and half-frames. None of it should be
        // fatal, or every version bump becomes an outage.
        let script = r#"
console.log("Welcome to FakeAgent v2!");
console.log("{ not json");
console.log(JSON.stringify({jsonrpc:"2.0",method:"unknown/method",params:{}}));
console.log(JSON.stringify({jsonrpc:"2.0",method:"session/update",params:{update:{
  sessionUpdate:"agent_message_chunk", content:{type:"text",text:"survived"}}}}));
"#;
        let (dir, args) = fake_agent(script);
        let (mut stream, _s) = spawn_acp("node", &args, ".", vec![]).unwrap();
        let mut texts = Vec::new();
        while let Some(e) = stream.next().await {
            if let AgentEvent::TextDelta { text } = e {
                texts.push(text);
            }
        }
        assert_eq!(texts, vec!["survived"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_agent_says_how_to_install_it() {
        let err = spawn_acp("definitely-not-installed-xyz", &[], ".", vec![])
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found on PATH"), "{err}");
    }

    #[test]
    fn the_handshake_frames_are_well_formed() {
        assert!(initialize_frame(1).contains("\"method\":\"initialize\""));
        assert!(new_session_frame(2, "/w").contains("\"cwd\":\"/w\""));
        let p = prompt_frame(3, "s1", "do the thing");
        assert!(p.contains("\"sessionId\":\"s1\""));
        assert!(p.contains("do the thing"));
    }
}
