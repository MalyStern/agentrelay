//! AgentRelay desktop backend.
//!
//! The whole reason this exists is to remove the two things that make the CLI
//! unfriendly to someone who does not live in a terminal: starting the daemon, and
//! pasting a bearer token to reach it. Here the Rust side does both. It finds the
//! `agentrelay` binary, starts `agentrelay serve` for the chosen workspace, reads the
//! address and token the daemon writes to `.agentrelay/ipc.json`, and makes the HTTP
//! calls on the page's behalf.
//!
//! The token never leaves this process. The webview asks for `status` and `report`
//! and gets back the already-fetched JSON — it never sees, stores, or renders the
//! token, which is exactly the rule the browser UI has to ask the user to uphold and
//! this app simply enforces.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

/// Address + token, read from a workspace's `.agentrelay/ipc.json`. The token stays
/// in the backend; `Connection` is never handed to the frontend.
#[derive(Debug, Clone)]
struct Connection {
    addr: String,
    token: String,
}

#[derive(Default)]
struct AppState {
    /// The workspace the user chose. Everything is scoped to it.
    workspace: Mutex<Option<PathBuf>>,
    /// The daemon we started, so we can stop it when the app closes rather than
    /// leaking a background process the user never sees.
    child: Mutex<Option<std::process::Child>>,
}

/// What the frontend is allowed to know about the connection: whether there is one,
/// and where — never the token.
#[derive(Serialize)]
struct ConnStatus {
    connected: bool,
    addr: Option<String>,
    workspace: Option<String>,
    detail: String,
}

fn ipc_path(workspace: &Path) -> PathBuf {
    workspace.join(".agentrelay").join("ipc.json")
}

/// Read the loopback address and token the running daemon published. `None` when the
/// daemon is not serving (the file only exists while `serve` runs).
fn read_connection(workspace: &Path) -> Option<Connection> {
    let raw = std::fs::read_to_string(ipc_path(workspace)).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    Some(Connection {
        addr: v.get("addr")?.as_str()?.to_string(),
        token: v.get("token")?.as_str()?.to_string(),
    })
}

/// Locate the `agentrelay` binary.
///
/// The search ORDER matters for safety. In a release build we trust PATH first: the
/// walk-up looks for a sibling `daemon/target/...` binary, and preferring that over
/// PATH means an attacker who can drop a file into a parent of the install directory
/// could get their binary launched. That is a real, if narrow, escalation, so a
/// shipped app treats PATH — an installed, chosen binary — as authoritative and only
/// falls back to the sibling for a source checkout that has no install. A debug build
/// keeps the developer-friendly order (sibling first) because there PATH usually has
/// nothing and the whole point is to run against the just-built daemon.
fn find_agentrelay() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        sibling_daemon().or_else(|| which_on_path(&bin_name()))
    }
    #[cfg(not(debug_assertions))]
    {
        which_on_path(&bin_name()).or_else(sibling_daemon)
    }
}

/// A daemon binary built alongside a source checkout: walk up a few levels looking for
/// `daemon/target/{release,debug}/agentrelay`. Release preferred over debug.
fn sibling_daemon() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut here = exe.parent()?.to_path_buf();
    for _ in 0..6 {
        for profile in ["release", "debug"] {
            let cand = here.join("daemon").join("target").join(profile).join(bin_name());
            if cand.is_file() {
                return Some(cand);
            }
        }
        match here.parent() {
            Some(p) => here = p.to_path_buf(),
            None => break,
        }
    }
    None
}

fn bin_name() -> String {
    if cfg!(windows) {
        "agentrelay.exe".to_string()
    } else {
        "agentrelay".to_string()
    }
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Do the actual HTTP GET against the daemon, attaching the bearer token here so the
/// frontend never has to.
fn daemon_get(conn: &Connection, route: &str) -> Result<Value, String> {
    let url = format!("http://{}{}", conn.addr, route);
    let resp = ureq::get(&url)
        .set("Authorization", &format!("Bearer {}", conn.token))
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("could not reach the daemon at {}: {e}", conn.addr))?;
    let body = resp.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| format!("daemon returned invalid JSON: {e}"))
}

// ── commands the frontend can call ───────────────────────────────────────────

/// Open a native folder picker and remember the chosen workspace.
#[tauri::command]
async fn choose_workspace(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    // The dialog is modal and returns on a callback thread; a channel bridges it back
    // to this async command.
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.recv().map_err(|e| e.to_string())?;
    let Some(folder) = folder else {
        return Ok(None); // the user cancelled — not an error
    };
    let path = folder.to_string();
    let state = app.state::<AppState>();
    *state.workspace.lock().unwrap() = Some(PathBuf::from(&path));
    Ok(Some(path))
}

/// Set the workspace without a dialog (for a path typed or remembered).
#[tauri::command]
fn set_workspace(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.join(".agentrelay").exists() {
        return Err(format!(
            "{path} is not an AgentRelay workspace — run `agentrelay init` there first, \
             or choose a different folder."
        ));
    }
    let state = app.state::<AppState>();
    *state.workspace.lock().unwrap() = Some(p);
    Ok(())
}

/// Where we are: connected, and to which workspace. Deliberately never returns the
/// token.
#[tauri::command]
fn connection_status(app: tauri::AppHandle) -> ConnStatus {
    let state = app.state::<AppState>();
    let ws = state.workspace.lock().unwrap().clone();
    let Some(ws) = ws else {
        return ConnStatus {
            connected: false,
            addr: None,
            workspace: None,
            detail: "Choose a workspace to begin — a folder where you ran `agentrelay init`."
                .to_string(),
        };
    };
    match read_connection(&ws) {
        Some(c) => ConnStatus {
            connected: true,
            addr: Some(c.addr),
            workspace: Some(ws.to_string_lossy().into_owned()),
            detail: "Connected to the running daemon.".to_string(),
        },
        None => ConnStatus {
            connected: false,
            addr: None,
            workspace: Some(ws.to_string_lossy().into_owned()),
            detail: "The daemon is not running for this workspace. Start it to see live data."
                .to_string(),
        },
    }
}

/// Start `agentrelay serve` for the current workspace and wait until it has published
/// its address. Errors loudly if the binary cannot be found — the alternative, a
/// window that silently never connects, is the failure this app exists to remove.
#[tauri::command]
fn start_daemon(app: tauri::AppHandle) -> Result<String, String> {
    let state = app.state::<AppState>();
    let ws = state
        .workspace
        .lock()
        .unwrap()
        .clone()
        .ok_or("choose a workspace first")?;

    if read_connection(&ws).is_some() {
        return Ok("already running".to_string());
    }

    let bin = find_agentrelay().ok_or(
        "could not find the `agentrelay` program. Build the daemon (cargo build in daemon/) \
         or install it, then try again.",
    )?;

    let child = std::process::Command::new(&bin)
        .arg("serve")
        .current_dir(&ws)
        .spawn()
        .map_err(|e| format!("could not start the daemon: {e}"))?;
    *state.child.lock().unwrap() = Some(child);

    // Wait for ipc.json to appear — the daemon writes it once it is listening.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if read_connection(&ws).is_some() {
            return Ok("started".to_string());
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    Err("the daemon started but did not begin listening within 10 seconds".to_string())
}

/// The workspace's status, fetched with the token attached here.
#[tauri::command]
fn get_status(app: tauri::AppHandle) -> Result<Value, String> {
    let conn = current_connection(&app)?;
    daemon_get(&conn, "/status")
}

/// The morning report — the thing a person opens this for. Same token handling.
#[tauri::command]
fn get_report(app: tauri::AppHandle) -> Result<Value, String> {
    let conn = current_connection(&app)?;
    daemon_get(&conn, "/report")
}

fn current_connection(app: &tauri::AppHandle) -> Result<Connection, String> {
    let state = app.state::<AppState>();
    let ws = state
        .workspace
        .lock()
        .unwrap()
        .clone()
        .ok_or("no workspace chosen")?;
    read_connection(&ws)
        .ok_or_else(|| "the daemon is not running — start it first".to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            choose_workspace,
            set_workspace,
            connection_status,
            start_daemon,
            get_status,
            get_report,
        ])
        .setup(|app| {
            // A tray icon so a supervised overnight run has somewhere to live while
            // the window is closed — the natural place for "still working / needs
            // you" to surface without stealing focus. Menu is minimal on purpose.
            let show = MenuItem::with_id(app, "show", "Show AgentRelay", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("AgentRelay")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Killing the daemon we started, when the app closes, is not optional:
            // this app's whole promise is that it does not leave background things
            // running that the user cannot see.
            if let tauri::WindowEvent::Destroyed = event {
                let state = window.state::<AppState>();
                let taken = state.child.lock().unwrap().take();
                if let Some(mut child) = taken {
                    let _ = child.kill();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building the AgentRelay app")
        .run(|_app, _event| {});
}
