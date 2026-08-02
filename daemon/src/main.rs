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
    Init {
        /// Start from a named mission template instead of the defaults — a complete,
        /// sensible config (guards + validators + scope + budget). See `templates`.
        #[arg(long)]
        template: Option<String>,
    },
    /// List the built-in mission templates and what each one sets up.
    Templates,
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
    /// Answer one PreToolUse hook call: reads the tool call on stdin, exits 2 to
    /// block. Meant to be run BY the agent, not by you.
    Hook {
        #[arg(long, default_value = ".")]
        cwd: String,
    },
    /// Install the PreToolUse hook so guards BLOCK instead of reacting.
    InstallHook {
        /// Remove it again.
        #[arg(long)]
        uninstall: bool,
    },
    /// Is there room in the quota window to start a unit of this length?
    /// Exits 0 to go ahead, 75 to wait — so a night script can gate on it.
    Fits {
        /// Roughly how long the unit takes, in minutes.
        #[arg(long, default_value_t = 30)]
        minutes: i64,
    },
    /// Does the calendar (quiet hours / busy blocks) allow a unit of this length to
    /// start now? Exits 0 to go ahead, 75 to wait — the sibling of `fits`, but for
    /// your day rather than the quota window.
    Schedule {
        /// Roughly how long the unit takes, in minutes.
        #[arg(long, default_value_t = 30)]
        minutes: i64,
    },
    /// Note what is already uncommitted, BEFORE the agent starts a unit.
    Begin {
        #[arg(long, default_value = ".")]
        cwd: String,
        #[arg(long, default_value = "default")]
        mission: String,
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
    /// Run the checks and keep fixing until they pass (or a person is needed).
    Fix {
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
        Cmd::Init { template } => {
            let existed = root.join(".agentrelay").join("config.json").exists();
            match template {
                Some(name) => {
                    // An unknown name is refused with the valid list, before anything
                    // is written.
                    let t = agentrelay::templates::Template::for_name(&name).map_err(|e| e.to_string())?;
                    Daemon::init_with_config(root, t.to_config()).map_err(|e| e.to_string())?;
                    if existed {
                        println!(
                            "A config already existed, so the `{}` template was NOT written over it.",
                            t.name
                        );
                        println!("Delete .agentrelay/config.json first if you want to start from the template.");
                        return Ok(());
                    }
                    println!("Initialized .agentrelay/ from the `{}` template.", t.name);
                    println!("  {}", t.summary);
                    println!();
                    println!("Before an unattended run, review:");
                    for note in t.review_notes() {
                        println!("  - {note}");
                    }
                }
                None => {
                    Daemon::init(root).map_err(|e| e.to_string())?;
                    println!("Initialized .agentrelay/ (config.json + agentrelay.db).");
                    println!("Next: `agentrelay run \"<your task>\"` — or edit .agentrelay/config.json first.");
                    println!("Or start from a template: `agentrelay templates`, then `agentrelay init --template <name>`.");
                }
            }
            Ok(())
        }
        Cmd::Templates => {
            println!("Mission templates — a complete, sensible config to start from:\n");
            for t in agentrelay::templates::Template::all() {
                println!("  {}", t.name);
                println!("    {}", t.summary);
                let cfg = t.to_config();
                println!(
                    "    guards: {} · validators: {} · scope: {} file(s) max · budget: {}",
                    cfg.guards.len(),
                    cfg.validators.len(),
                    cfg.scope.max_files.map(|n| n.to_string()).unwrap_or_else(|| "no".into()),
                    cfg.budget.describe().trim_end_matches('.'),
                );
                println!();
            }
            println!("Use one:  agentrelay init --template <name>");
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
            println!("  state:       {} — {}", s.state, s.state_meaning);
            println!();
            println!("{}", s.blocking);
            if s.waiting_for_you {
                println!("               ^ nothing will happen until you act.");
            }
            println!();
            println!("{}", s.isolation);
            println!();
            println!("{}", s.notifications);
            println!();
            println!("{}", s.cloud);
            println!();
            println!("{}", s.schedule);
            println!();
            println!("{} Spent so far: ${:.2}.", s.budget, s.spent_usd);
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
            let mut missing_required = false;
            for tool in ["git", "node", "claude"] {
                match agentrelay::which::resolve(tool) {
                    Some(p) => println!("  [OK] {tool:<7} {}", p.display()),
                    None => {
                        println!("  [--] {tool:<7} {}", agentrelay::which::install_hint(tool));
                        if tool != "claude" {
                            missing_required = true;
                        }
                    }
                }
            }
            if agentrelay::which::resolve("claude").is_none() {
                println!();
                println!("Without `claude` the daemon can supervise nothing — everything else");
                println!("here (guards, checkpoints, the report) still works, but there is no agent.");
            }
            if missing_required {
                Err("missing a required tool (git/node)".to_string())
            } else {
                Ok(())
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
        Cmd::Hook { cwd } => {
            use std::io::Read;
            // Read the tool call the agent is asking permission for. Every failure
            // path below denies: a hook that allows what it cannot read is not a hook.
            let mut raw = String::new();
            let _ = std::io::stdin().read_to_string(&mut raw);

            // The guards come from the workspace config. If it cannot be opened we
            // do NOT fall back to "no guards" — that is the shape of every fail-open
            // bug this codebase has had.
            let verdict = match Daemon::open(root) {
                Ok(d) => agentrelay::hook::decide(&raw, &d.config().guards, &cwd),
                Err(e) => agentrelay::hook::deny(format!(
                    "AgentRelay could not load its guards ({e}), so this tool call is refused \
                     rather than run unguarded."
                )),
            };

            // Both documented signals: the JSON decision on stdout and the exit
            // code. Emitting only one risks a denial being ignored by whichever the
            // agent actually reads.
            println!("{}", verdict.json);
            if verdict.allow {
                Ok(())
            } else {
                eprintln!("{}", verdict.reason);
                std::process::exit(2);
            }
        }
        Cmd::InstallHook { uninstall } => {
            let settings_path = root.join(".claude").join("settings.json");
            // Distinguish "missing" from "present but unreadable". Collapsing both to
            // {} would silently overwrite a settings.json we could not parse — the same
            // bug the daemon's ensure_blocking_hook was fixed for.
            let existing: serde_json::Value = match std::fs::read_to_string(&settings_path) {
                Err(_) => serde_json::json!({}),
                Ok(s) => {
                    let s = s.strip_prefix('\u{feff}').unwrap_or(&s).trim().to_string();
                    if s.is_empty() {
                        serde_json::json!({})
                    } else {
                        match serde_json::from_str::<serde_json::Value>(&s) {
                            Ok(v) if v.is_object() => v,
                            _ => {
                                return Err(format!(
                                    "{} exists but is not a valid JSON object — refusing to \
                                     rewrite it and lose your settings. Fix or delete it first.",
                                    settings_path.display()
                                ))
                            }
                        }
                    }
                }
            };

            // An absolute path: the agent's cwd is not necessarily ours.
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "agentrelay".to_string());
            let command = format!("\"{}\" hook --cwd \"{}\"", exe, root.display());

            let next = if uninstall {
                agentrelay::hook::remove_hook_settings(&existing, &command)
            } else {
                match agentrelay::hook::merge_hook_settings(&existing, &command) {
                    Some(v) => v,
                    None => {
                        println!(
                            "Already installed, or {} is not in a shape we can safely edit — \
                             nothing was changed.",
                            settings_path.display()
                        );
                        return Ok(());
                    }
                }
            };

            std::fs::create_dir_all(settings_path.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(&settings_path, serde_json::to_string_pretty(&next).unwrap())
                .map_err(|e| e.to_string())?;
            if uninstall {
                println!("Removed the AgentRelay hook from {}.", settings_path.display());
            } else {
                println!("Installed the AgentRelay hook in {}.", settings_path.display());
                println!("Guards now BLOCK a forbidden tool call before it runs, instead of");
                println!("stopping the agent after the fact. Your other hooks were left alone.");
            }
            Ok(())
        }
        Cmd::Fits { minutes } => {
            use agentrelay::quota::StartDecision;
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let now = chrono::Utc::now().to_rfc3339();
            match d.should_start_unit(minutes * 60_000, &now) {
                StartDecision::Start => {
                    println!("Go — a ~{minutes} minute unit fits in what is left of the window.");
                    Ok(())
                }
                StartDecision::StartUnknownWindow => {
                    println!("Go, but blind: no quota window has been reported yet, so there is");
                    println!("nothing to reason about. Run a turn and the provider will tell us.");
                    Ok(())
                }
                StartDecision::WaitUntilReset { resume_at_utc, wait_ms } => {
                    println!("Wait — a ~{minutes} minute unit would be cut off by the quota window.");
                    println!("  window reopens: {resume_at_utc}");
                    println!("  wait:           {} minute(s)", wait_ms / 60_000);
                    println!();
                    println!("Being stopped mid-unit costs more than waiting: the work would be");
                    println!("half-done, unvalidated and uncheckpointed.");
                    // A distinct code so a script can tell "wait" from a real failure.
                    std::process::exit(75);
                }
            }
        }
        Cmd::Schedule { minutes } => {
            use agentrelay::calendar::ScheduleDecision;
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            // A malformed schedule refuses loudly rather than running through a window
            // the user meant to block.
            match d.schedule_decision(minutes * 60_000).map_err(|e| e.to_string())? {
                ScheduleDecision::Start => {
                    println!("Go — a ~{minutes} minute unit does not run into any quiet-hours or busy window.");
                    Ok(())
                }
                ScheduleDecision::StartNoSchedule => {
                    println!("Go — no schedule is set, so there is nothing to wait for.");
                    Ok(())
                }
                ScheduleDecision::WaitUntil { resume_at_utc, wait_ms } => {
                    println!("Wait — a ~{minutes} minute unit would collide with a quiet-hours or busy window.");
                    println!("  clears at: {resume_at_utc}");
                    println!("  wait:      {} minute(s)", wait_ms / 60_000);
                    println!();
                    println!("Being stopped mid-unit costs more than waiting: the work would be");
                    println!("half-done, unvalidated and uncheckpointed.");
                    // The same distinct "wait, not a failure" code `fits` uses.
                    std::process::exit(75);
                }
            }
        }
        Cmd::Begin { cwd, mission, unit } => {
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let existing = d.begin_unit(&mission, &unit, &cwd).map_err(|e| e.to_string())?;
            if existing.is_empty() {
                println!("Tree is clean. Everything the agent changes from here is its own.");
            } else {
                println!("{} file(s) are already uncommitted and will be left alone:", existing.len());
                for p in existing.iter().take(10) {
                    println!("  {p}");
                }
                if existing.len() > 10 {
                    println!("  … and {} more", existing.len() - 10);
                }
                println!();
                println!("They will NOT be committed by any checkpoint for unit `{unit}`.");
            }
            Ok(())
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
                    if !out.left_alone.is_empty() {
                        println!("  Left alone ({} file(s) you had already changed): {}",
                            out.left_alone.len(),
                            out.left_alone.iter().take(5).cloned().collect::<Vec<_>>().join(", "));
                    }
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

            // Non-zero whenever a person should look — failures, but also units
            // nobody verified and units with no restore point. The report is meant
            // to be usable from a script that decides whether to wake someone, and
            // "we never checked" must not read as "fine".
            if r.needs_attention() {
                return Err(r.headline());
            }
            Ok(())
        }
        Cmd::Fix { cwd, mission, unit } => {
            let d = Daemon::open(root).map_err(|e| e.to_string())?;
            let out = d.fix_until_green(&mission, &unit, &cwd).await;
            if out.green {
                if out.attempts == 0 {
                    println!("Already green — nothing to fix.");
                } else {
                    println!("Green after {} fix attempt(s).", out.attempts);
                }
                return Ok(());
            }
            let why = out.gave_up.unwrap_or_else(|| "unknown".into());
            println!("Stopped after {} attempt(s).", out.attempts);
            println!("  {why}");
            println!();
            println!("The mission is now `waiting_input`: it will not continue by itself.");
            Err("the checks are still failing — a person is needed".to_string())
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
