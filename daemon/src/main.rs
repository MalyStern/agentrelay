//! `agentrelay` CLI — the entry point that drives the daemon library.

use agentrelay::daemon::Daemon;
use clap::{Parser, Subcommand};
use std::process::ExitCode;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(name = "agentrelay", version, about = "Supervise overnight AI coding-agent runs — locally.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold .agentrelay/ (config + database) in the current directory.
    Init,
    /// Show the workspace status (db, schema, missions, config).
    Status,
    /// Run one supervised turn with the configured engine.
    Run {
        /// The prompt to send the agent.
        prompt: String,
        /// Working directory (the worktree).
        #[arg(long, default_value = ".")]
        cwd: String,
        /// Mission id to log under.
        #[arg(long, default_value = "default")]
        mission: String,
    },
    /// Check that the required tools are installed.
    Doctor,
    /// Start the loopback IPC server (for a UI / remote control).
    Serve {
        /// Address to bind (loopback only).
        #[arg(long, default_value = "127.0.0.1:7717")]
        bind: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = std::env::current_dir().expect("cwd");
    match run(cli, &root).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("agentrelay: {msg}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli, root: &std::path::Path) -> Result<(), String> {
    match cli.cmd {
        Cmd::Init => {
            Daemon::init(root).map_err(|e| e.to_string())?;
            println!("Initialized .agentrelay/ (config.json + agentrelay.db).");
            println!("Next: `agentrelay run \"<your task>\"` — or edit .agentrelay/config.json first.");
            Ok(())
        }
        Cmd::Status => {
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let s = d.status();
            println!("AgentRelay status");
            println!("  db:          {}", s.db_path);
            println!("  schema:      v{}", s.schema_version);
            println!("  missions:    {}", s.missions);
            println!("  engine:      {}", s.program);
            println!("  keep-awake:  {}", s.keep_awake);
            Ok(())
        }
        Cmd::Run { prompt, cwd, mission } => {
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let out = d.run_turn(&mission, &prompt, &cwd).await.map_err(|e| e.to_string())?;
            println!("Turn finished:");
            println!("  events:      {}", out.events_seen);
            println!("  tokens used: {}", out.used_tokens);
            println!("  completed:   {}", out.completed);
            if out.interrupted {
                println!("  INTERRUPTED by a guard:");
                for v in &out.violations {
                    println!("    - {} ({})", v.reason_code, v.detail.clone().unwrap_or_default());
                }
            }
            if let Some(p) = out.resume_plan {
                println!("  RATE-LIMITED — resume at {} (wait {} ms)", p.resume_at_utc, p.wait_ms);
            }
            Ok(())
        }
        Cmd::Doctor => {
            println!("AgentRelay doctor");
            let mut ok = true;
            for tool in ["git", "node", "claude"] {
                let present = which(tool);
                println!("  [{}] {}", if present { "OK" } else { "--" }, tool);
                if tool != "claude" && !present {
                    ok = false;
                }
            }
            if ok {
                Ok(())
            } else {
                Err("missing a required tool (git/node)".to_string())
            }
        }
        Cmd::Serve { bind } => {
            let daemon = Daemon::open(root).map_err(|e| e.to_string())?;
            let shared = Arc::new(Mutex::new(daemon));
            let handle = agentrelay::ipc::serve(shared, &bind).await.map_err(|e| e.to_string())?;
            // Write addr + token so a local UI can connect (loopback only).
            let info = format!("{{\"addr\":\"{}\",\"token\":\"{}\"}}", handle.addr, handle.token);
            let _ = std::fs::write(root.join(".agentrelay").join("ipc.json"), &info);
            println!("AgentRelay IPC listening on http://{}", handle.addr);
            println!("  token written to .agentrelay/ipc.json — GET /status, POST /run, GET /health");
            let _ = handle.task.await;
            Ok(())
        }
    }
}

/// Best-effort "is this program runnable?" — try `<tool> --version`.
fn which(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
