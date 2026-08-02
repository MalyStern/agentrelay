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
    /// Run the config's completion validators (tests/build/…) as evidence.
    Validate {
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value = "default")]
        unit: String,
    },
    /// Finish a unit of work: run the validators and, only if they pass, checkpoint.
    Done {
        /// What this unit did — becomes the commit subject.
        summary: String,
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value = "default")]
        mission: String,
        #[arg(long, default_value = "default")]
        unit: String,
    },
    /// List the checkpoints the supervisor made, newest first.
    Checkpoints {
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// The morning report: what actually happened overnight, with its sources.
    Report {
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value = "default")]
        mission: String,
        /// Print only the one-line headline (for a phone / a status bar).
        #[arg(long)]
        short: bool,
        /// Also push the headline to the configured notifier (off unless set).
        #[arg(long)]
        notify: bool,
    },
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
            println!();
            println!("{}", s.notifications);
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
        Cmd::Validate { cwd, unit } => {
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let results = d.run_validators(&unit, &cwd).await;
            if results.is_empty() {
                println!("No validators configured (.agentrelay/config.json → validators).");
                return Ok(());
            }
            for r in &results {
                println!("  [{}] {} ({} ms)", if r.passed { "PASS" } else { "FAIL" }, r.validator, r.duration_ms);
            }
            if agentrelay::validator::all_passed(&results) {
                println!("All validators passed — unit complete (evidence, not the model's word).");
                Ok(())
            } else {
                Err("validators failed — unit is NOT complete".to_string())
            }
        }
        Cmd::Done { summary, cwd, mission, unit } => {
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let out = d.complete_unit(&mission, &unit, &cwd, &summary).await;

            if out.validations.is_empty() {
                println!("No validators configured (.agentrelay/config.json → validators).");
                println!("Nothing was checkpointed: without evidence there is nothing to certify.");
                return Err("no completion evidence — unit is NOT complete".to_string());
            }
            for r in &out.validations {
                println!("  [{}] {} ({} ms)", if r.passed { "PASS" } else { "FAIL" }, r.validator, r.duration_ms);
            }
            if !out.gate_passed {
                println!("\nNot checkpointed — your work is still in the working tree, uncommitted.");
                return Err("validators failed — unit is NOT complete".to_string());
            }
            match (&out.checkpoint, &out.checkpoint_error) {
                (Some(cp), _) if cp.created => {
                    println!("\nCheckpointed {} ({} file(s)) on {}", &cp.head[..7.min(cp.head.len())], cp.files_changed, cp.branch.clone().unwrap_or_else(|| "detached HEAD".into()));
                    println!("  Undo this unit:   {}", agentrelay::checkpoint::undo_command(&cp.head));
                    println!("  Rewind here later: {}", agentrelay::checkpoint::rewind_command(&cp.head));
                }
                (Some(_), _) => println!("\nValidators passed; nothing to checkpoint (the tree was already clean)."),
                (None, Some(e)) => println!("\nValidators passed, but the checkpoint could not be made: {e}"),
                (None, None) => {}
            }
            Ok(())
        }
        Cmd::Report { cwd, mission, short, notify } => {
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let r = d.morning_report(&mission, &cwd);
            if short {
                println!("{}", r.headline());
            } else {
                print!("{}", r.render());
            }
            if notify {
                match d.notify_report(&r) {
                    Ok(true) => println!("\n(headline pushed to your notifier)"),
                    Ok(false) => println!(
                        "\n(--notify given, but notifications are off — set `notify` in \
                         .agentrelay/config.json; nothing was sent)"
                    ),
                    // Report the delivery failure, but never let it decide the exit
                    // code: whether the night succeeded has nothing to do with
                    // whether a phone server answered.
                    Err(e) => eprintln!("\nnotification failed (the report above still stands): {e}"),
                }
            }

            // A night with failed units exits non-zero: the report is meant to be
            // usable from a script that decides whether to wake someone.
            if r.units_failed > 0 {
                return Err(format!("{} unit(s) failed overnight", r.units_failed));
            }
            Ok(())
        }
        Cmd::Checkpoints { cwd, limit } => {
            let list = agentrelay::checkpoint::list(&cwd, limit).map_err(|e| e.to_string())?;
            if list.is_empty() {
                println!("No checkpoints yet — run `agentrelay done \"<what the unit did>\"`.");
                return Ok(());
            }
            println!("Checkpoints (newest first):");
            for (sha, id) in &list {
                println!("  {}  {}", &sha[..7.min(sha.len())], id);
            }
            println!("\nTo see what came after one:  git diff --stat <sha> HEAD");
            Ok(())
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
