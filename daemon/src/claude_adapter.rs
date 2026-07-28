//! Real Claude adapter: spawn `claude -p --output-format stream-json`, pipe its
//! NDJSON stdout through `ClaudeStreamParser` into a normalized event stream (plan
//! 00 §5.1). The spawn→parse→stream glue is a free function so it can be integration-
//! tested with any command that emits NDJSON (the test uses `node`); no real `claude`
//! or quota needed.

use crate::adapter::{EventStream, ProviderAdapter};
use crate::claude_stream::ClaudeStreamParser;
use crate::error::{AgentRelayError, ErrorCode, Result};
use crate::provider::{
    Capabilities, Guard, QuotaState, SessionHandle, SessionSpec, TurnInput,
};
use async_trait::async_trait;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Spawn `program args` in `cwd`, stream its stdout lines through the Claude
/// stream-json parser, and yield normalized events. Flushes a newline-less final
/// line at EOF.
pub fn spawn_stream_json(program: &str, args: &[String], cwd: &str) -> Result<EventStream> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| AgentRelayError::new(ErrorCode::ProcessSpawnFailed, format!("{program}: {e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentRelayError::new(ErrorCode::ProcessSpawnFailed, "no stdout"))?;

    let stream = async_stream::stream! {
        let mut parser = ClaudeStreamParser::new();
        let mut lines = BufReader::new(stdout).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    for e in parser.push(&format!("{line}\n")) {
                        yield e;
                    }
                }
                Ok(None) => break, // EOF
                Err(_) => break,
            }
        }
        for e in parser.flush() {
            yield e;
        }
        let _ = child.wait().await;
    };
    Ok(Box::pin(stream))
}

pub struct ClaudeAdapter {
    /// The engine program — "claude" in production, a stub in tests.
    program: String,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        ClaudeAdapter {
            program: "claude".to_string(),
        }
    }
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_program(program: impl Into<String>) -> Self {
        ClaudeAdapter {
            program: program.into(),
        }
    }

    fn turn_args(&self, handle: &SessionHandle, input: &TurnInput) -> Vec<String> {
        let mut args = vec![
            "-p".into(),
            input.prompt.clone(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ];
        if let Some(ext) = &handle.external_id {
            args.push("--resume".into());
            args.push(ext.clone());
        }
        args
    }
}

#[async_trait]
impl ProviderAdapter for ClaudeAdapter {
    fn id(&self) -> &str {
        "claude-code"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            resume: true,
            interrupt: true,
            steer: true, // between turns only
            guards: true,
            fork_session: false,
            transform_tool_result: true, // via the Agent SDK / PostToolUse
        }
    }

    async fn start_session(&self, spec: SessionSpec) -> Result<SessionHandle> {
        // Claude discovers its session_id from the first turn's result line; the
        // daemon fills external_id in after parsing. Here we return the shell.
        Ok(SessionHandle {
            id: format!("claude-{}", spec.cwd),
            external_id: spec.resume_external_id,
            provider: "claude-code".into(),
            cwd: spec.cwd,
        })
    }

    async fn resume_session(&self, external_id: &str, spec: SessionSpec) -> Result<SessionHandle> {
        Ok(SessionHandle {
            id: format!("claude-{}", spec.cwd),
            external_id: Some(external_id.to_string()),
            provider: "claude-code".into(),
            cwd: spec.cwd,
        })
    }

    async fn run_turn(&self, handle: &SessionHandle, input: TurnInput) -> Result<EventStream> {
        let args = self.turn_args(handle, &input);
        spawn_stream_json(&self.program, &args, &handle.cwd)
    }

    async fn steer(&self, _handle: &SessionHandle, _message: &str) -> Result<()> {
        // Claude steers only between turns (via the next turn's input) — no-op here.
        Ok(())
    }

    async fn interrupt(&self, _handle: &SessionHandle) -> Result<()> {
        // Real process-tree kill is the Process Manager's job (Windows Job Object /
        // Unix pgid) — the next increment. This is the signal hook.
        Ok(())
    }

    async fn quota(&self) -> Result<QuotaState> {
        // Under a subscription there's no live counter — rate-limits are reactive
        // from the stream. Report an empty snapshot for now.
        Ok(QuotaState {
            provider: "claude-code".into(),
            windows: vec![],
        })
    }

    async fn install_guards(&self, _handle: &SessionHandle, _guards: &[Guard]) -> Result<()> {
        // Writes .claude/settings.json hooks — implemented with the hook layer.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AgentEvent;
    use futures::StreamExt;

    /// A cross-platform NDJSON emitter using node (always present in this project).
    fn node_emit(lines: &[serde_json::Value]) -> Vec<String> {
        let script = lines
            .iter()
            .map(|l| format!("process.stdout.write({}+'\\n');", serde_json::to_string(&l.to_string()).unwrap()))
            .collect::<String>();
        vec!["-e".into(), script]
    }

    #[tokio::test]
    async fn spawns_and_parses_ndjson_stdout() {
        use serde_json::json;
        let args = node_emit(&[
            json!({"type":"system","subtype":"init","session_id":"sess_x"}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}),
            json!({"type":"result","subtype":"success","session_id":"sess_x","usage":{"input_tokens":1,"output_tokens":1}}),
        ]);
        let stream = spawn_stream_json("node", &args, ".").expect("spawn node");
        let events: Vec<AgentEvent> = stream.collect().await;
        let kinds: Vec<_> = events.iter().map(std::mem::discriminant).collect();
        let expected = [
            AgentEvent::TurnStarted,
            AgentEvent::TextDelta { text: String::new() },
            AgentEvent::TurnCompleted { usage: crate::events::Usage::zero() },
        ];
        assert_eq!(kinds, expected.iter().map(std::mem::discriminant).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn spawn_failure_is_a_structured_error() {
        let err = spawn_stream_json("definitely-not-a-real-program-xyz", &[], ".")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
    }

    #[tokio::test]
    async fn run_turn_builds_resume_args() {
        let a = ClaudeAdapter::new();
        let h = SessionHandle { id: "x".into(), external_id: Some("sess_1".into()), provider: "claude-code".into(), cwd: ".".into() };
        let args = a.turn_args(&h, &TurnInput { prompt: "go".into(), read_only: false });
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess_1".to_string()));
        assert!(args.windows(2).any(|w| w == ["--output-format", "stream-json"]));
    }
}
