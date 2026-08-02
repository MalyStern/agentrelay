//! Real Claude adapter: spawn `claude -p --output-format stream-json`, pipe its
//! NDJSON stdout through `ClaudeStreamParser` into a normalized event stream (plan
//! 00 §5.1). The spawn→parse→stream glue is a free function so it can be integration-
//! tested with any command that emits NDJSON (the test uses `node`); no real `claude`
//! or quota needed.

use crate::adapter::{EventStream, ProviderAdapter};
use crate::claude_stream::ClaudeStreamParser;
use crate::error::{AgentRelayError, ErrorCode, Result};
use crate::process::ProcessGroup;
use crate::provider::{Capabilities, Guard, QuotaState, SessionHandle, SessionSpec, TurnInput};
use async_trait::async_trait;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Spawn `program args` in `cwd` INTO A KILL-GROUP, stream its stdout through the
/// Claude parser, and return the event stream plus the group — so `interrupt` can
/// kill the whole process tree, not just the agent. Flushes a newline-less final
/// line at EOF.
pub fn spawn_stream_json(
    program: &str,
    args: &[String],
    cwd: &str,
) -> Result<(EventStream, Arc<ProcessGroup>)> {
    // Resolve rather than trusting the OS to find it. On Windows npm installs
    // `claude.cmd` with no `claude.exe`, and Command::new only ever looks for an
    // .exe — so this spawn failed on every Windows machine with Claude installed,
    // with an error that read as "not installed" rather than "cannot launch".
    let mut cmd = crate::which::command_for(program).ok_or_else(|| {
        AgentRelayError::new(
            ErrorCode::ProcessSpawnFailed,
            format!("{program} {}", crate::which::install_hint(program)),
        )
    })?;
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let group = Arc::new(ProcessGroup::new()?);
    let mut child = group.spawn(&mut cmd)?;
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
                Ok(None) => break, // EOF (also happens when the tree is killed)
                Err(_) => break,
            }
        }
        for e in parser.flush() {
            yield e;
        }
        let _ = child.wait().await;
    };
    Ok((Box::pin(stream), group))
}

pub struct ClaudeAdapter {
    /// The engine program — "claude" in production, a stub in tests.
    program: String,
    /// The active turn's process group, so interrupt() can kill the tree.
    current: Mutex<Option<Arc<ProcessGroup>>>,
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        ClaudeAdapter {
            program: "claude".to_string(),
            current: Mutex::new(None),
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
            current: Mutex::new(None),
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
        let (stream, group) = spawn_stream_json(&self.program, &args, &handle.cwd)?;
        *self.current.lock().unwrap() = Some(group);
        Ok(stream)
    }

    async fn steer(&self, _handle: &SessionHandle, _message: &str) -> Result<()> {
        // Claude steers only between turns (via the next turn's input) — no-op here.
        Ok(())
    }

    async fn interrupt(&self, _handle: &SessionHandle) -> Result<()> {
        // Kill the whole process tree of the active turn (agent + anything it spawned).
        let group = self.current.lock().unwrap().take();
        if let Some(g) = group {
            g.kill_tree()?;
        }
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
            .map(|l| {
                format!(
                    "process.stdout.write({}+'\\n');",
                    serde_json::to_string(&l.to_string()).unwrap()
                )
            })
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
        let (stream, _group) = match spawn_stream_json("node", &args, ".") {
            Ok(t) => t,
            Err(e) => panic!("spawn node: {e:?}"),
        };
        let events: Vec<AgentEvent> = stream.collect().await;
        let kinds: Vec<_> = events.iter().map(std::mem::discriminant).collect();
        let expected = [
            AgentEvent::TurnStarted,
            AgentEvent::TextDelta {
                text: String::new(),
            },
            AgentEvent::TurnCompleted {
                usage: crate::events::Usage::zero(),
            },
        ];
        assert_eq!(
            kinds,
            expected
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn spawn_failure_is_a_structured_error() {
        let err = spawn_stream_json("definitely-not-a-real-program-xyz", &[], ".")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProcessSpawnFailed);
    }

    #[tokio::test]
    async fn killing_the_group_ends_the_stream() {
        use std::time::Duration;
        use tokio::time::timeout;
        // Emit one line, then keep the process alive forever.
        let script = "process.stdout.write(JSON.stringify({type:'assistant',message:{content:[{type:'text',text:'hi'}]}})+'\\n');setTimeout(()=>{},60000);";
        let (mut stream, group) =
            match spawn_stream_json("node", &["-e".into(), script.into()], ".") {
                Ok(t) => t,
                Err(e) => panic!("spawn: {e:?}"),
            };
        let first = stream.next().await;
        assert!(matches!(first, Some(AgentEvent::TextDelta { .. })));
        group.kill_tree().unwrap();
        // With the process killed, stdout hits EOF and the stream ends promptly.
        let drained = timeout(Duration::from_secs(5), async {
            while stream.next().await.is_some() {}
        })
        .await;
        assert!(drained.is_ok(), "stream did not end after kill_tree");
    }

    #[tokio::test]
    async fn run_turn_builds_resume_args() {
        let a = ClaudeAdapter::new();
        let h = SessionHandle {
            id: "x".into(),
            external_id: Some("sess_1".into()),
            provider: "claude-code".into(),
            cwd: ".".into(),
        };
        let args = a.turn_args(
            &h,
            &TurnInput {
                prompt: "go".into(),
                read_only: false,
            },
        );
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess_1".to_string()));
        assert!(args
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"]));
    }
}
