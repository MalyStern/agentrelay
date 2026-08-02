//! IPC surface (plan §ד: "UI ── loopback + token ──> Daemon"). A tiny loopback-only
//! HTTP/JSON server so a UI, CLI, or remote push-handler can drive the daemon. Bound
//! to 127.0.0.1, every route except /health requires `Authorization: Bearer <token>`
//! (a 256-bit token minted at start). Hand-rolled over tokio TCP — no web framework.
//!
//! Routes: GET /health · GET /status · POST /run {prompt,cwd,mission}.

use crate::daemon::Daemon;
use crate::error::{AgentRelayError, ErrorCode, Result};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

pub struct IpcHandle {
    pub addr: SocketAddr,
    pub token: String,
    pub task: tokio::task::JoinHandle<()>,
}

/// A 256-bit hex token for loopback auth.
///
/// Fails rather than returning a weak token. Ignoring the getrandom error left the
/// buffer as 32 zero bytes, i.e. a completely predictable token protecting POST
/// /run — which executes an arbitrary prompt in an arbitrary directory. An entropy
/// failure has to stop the server from listening, not quietly downgrade it.
pub fn gen_token() -> Result<String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|e| {
        AgentRelayError::new(
            ErrorCode::Internal,
            format!("no secure randomness available for the IPC token ({e}) — refusing to listen"),
        )
    })?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Largest request body we will read. The IPC surface takes a prompt and a path;
/// nothing legitimate approaches this. Without a cap, an unauthenticated client on
/// loopback could declare `Content-Length: 4000000000` and we would allocate until
/// the daemon died — in the middle of the night, which is the whole point of it.
const MAX_BODY: usize = 1024 * 1024;

/// Bind and start serving. `bind` is e.g. "127.0.0.1:0" (ephemeral for tests).
pub async fn serve(daemon: Arc<Mutex<Daemon>>, bind: &str) -> Result<IpcHandle> {
    let token = gen_token()?;
    let listener = TcpListener::bind(bind)
        .await
        .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
    let addr = listener
        .local_addr()
        .map_err(|e| AgentRelayError::new(ErrorCode::Internal, e.to_string()))?;
    let tok = token.clone();
    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let d = daemon.clone();
                    let t = tok.clone();
                    tokio::spawn(async move {
                        let _ = handle_conn(stream, d, t).await;
                    });
                }
                Err(_) => break,
            }
        }
    });
    Ok(IpcHandle { addr, token, task })
}

#[derive(Deserialize)]
struct RunReq {
    prompt: String,
    #[serde(default = "dot")]
    cwd: String,
    #[serde(default = "default_mission")]
    mission: String,
}
fn dot() -> String {
    ".".into()
}
fn default_mission() -> String {
    "default".into()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

async fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &str) -> std::io::Result<()> {
    let res = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(res.as_bytes()).await?;
    stream.flush().await
}

async fn handle_conn(mut stream: TcpStream, daemon: Arc<Mutex<Daemon>>, token: String) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 64 * 1024 {
            return respond(&mut stream, 431, "Too Large", "{\"error\":\"headers too large\"}").await;
        }
    };

    let header_str = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = header_str.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut content_length = 0usize;
    let mut auth = String::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = v.trim().parse().unwrap_or(0),
                "authorization" => auth = v.trim().to_string(),
                _ => {}
            }
        }
    }

    // /health needs no auth.
    if method == "GET" && path == "/health" {
        return respond(&mut stream, 200, "OK", "{\"ok\":true}").await;
    }
    // Everything else requires the bearer token — checked BEFORE we read a byte of
    // body. Reading first meant an unauthenticated caller could make us allocate
    // whatever it asked for, and only then be told it was unauthorised.
    if auth != format!("Bearer {token}") {
        return respond(&mut stream, 401, "Unauthorized", "{\"error\":\"unauthorized\"}").await;
    }
    if content_length > MAX_BODY {
        return respond(&mut stream, 413, "Payload Too Large", "{\"error\":\"body too large\"}").await;
    }

    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
        if body.len() > MAX_BODY {
            return respond(&mut stream, 413, "Payload Too Large", "{\"error\":\"body too large\"}")
                .await;
        }
    }
    body.truncate(content_length);

    match (method.as_str(), path.as_str()) {
        ("GET", "/status") => {
            let report = daemon.lock().await.status();
            let json = serde_json::to_string(&report).unwrap_or_else(|_| "{}".into());
            respond(&mut stream, 200, "OK", &json).await
        }
        ("POST", "/run") => match serde_json::from_slice::<RunReq>(&body) {
            Ok(req) => {
                let result = {
                    let d = daemon.lock().await;
                    d.run_turn(&req.mission, &req.prompt, &req.cwd).await
                };
                match result {
                    Ok(outcome) => {
                        let json = serde_json::to_string(&outcome).unwrap_or_else(|_| "{}".into());
                        respond(&mut stream, 200, "OK", &json).await
                    }
                    Err(e) => {
                        let json = format!(
                            "{{\"code\":\"{}\",\"message\":{}}}",
                            e.code.as_str(),
                            serde_json::to_string(&e.message).unwrap_or_else(|_| "\"\"".into())
                        );
                        respond(&mut stream, 500, "Error", &json).await
                    }
                }
            }
            Err(_) => respond(&mut stream, 400, "Bad Request", "{\"error\":\"invalid body\"}").await,
        },
        _ => respond(&mut stream, 404, "Not Found", "{\"error\":\"not found\"}").await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(addr: SocketAddr, raw: &str) -> (u16, String) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(raw.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp).to_string();
        let code = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (code, body)
    }

    async fn test_daemon() -> Arc<Mutex<Daemon>> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static C: AtomicUsize = AtomicUsize::new(0);
        let n = C.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("agentrelay-ipc-{}-{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Arc::new(Mutex::new(Daemon::init(&root).unwrap()))
    }

    #[test]
    fn the_token_is_random_and_never_a_fallback() {
        // The failure this pins: ignoring the getrandom error left 32 zero bytes —
        // a fully predictable token guarding POST /run, which executes an arbitrary
        // prompt in an arbitrary directory.
        let a = gen_token().unwrap();
        let b = gen_token().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_ne!(a, "0".repeat(64));
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn an_unauthenticated_giant_body_is_refused_before_it_is_read() {
        // Declaring a 4 GB body used to make us allocate it and only then check the
        // token — a one-line way for anything on loopback to kill the daemon
        // overnight.
        let h = serve(test_daemon().await, "127.0.0.1:0").await.unwrap();
        let (code, _) = request(
            h.addr,
            "POST /run HTTP/1.1\r\nHost: x\r\nContent-Length: 4000000000\r\n\r\n",
        )
        .await;
        assert_eq!(code, 401, "no token, so it never gets as far as the body");
        h.task.abort();
    }

    #[tokio::test]
    async fn an_authenticated_giant_body_is_capped() {
        let h = serve(test_daemon().await, "127.0.0.1:0").await.unwrap();
        let (code, body) = request(
            h.addr,
            &format!(
                "POST /run HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\nContent-Length: 4000000000\r\n\r\n",
                h.token
            ),
        )
        .await;
        assert_eq!(code, 413, "{body}");
        h.task.abort();
    }

    #[tokio::test]
    async fn health_needs_no_auth() {
        let h = serve(test_daemon().await, "127.0.0.1:0").await.unwrap();
        let (code, body) = request(h.addr, "GET /health HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert_eq!(code, 200);
        assert!(body.contains("\"ok\":true"));
        h.task.abort();
    }

    #[tokio::test]
    async fn status_requires_token() {
        let h = serve(test_daemon().await, "127.0.0.1:0").await.unwrap();
        // no token -> 401
        let (code, _) = request(h.addr, "GET /status HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert_eq!(code, 401);
        // wrong token -> 401
        let (code2, _) = request(h.addr, "GET /status HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer nope\r\n\r\n").await;
        assert_eq!(code2, 401);
        // correct token -> 200 with the status JSON
        let req = format!("GET /status HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n", h.token);
        let (code3, body) = request(h.addr, &req).await;
        assert_eq!(code3, 200);
        assert!(body.contains("schema_version"));
        h.task.abort();
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let h = serve(test_daemon().await, "127.0.0.1:0").await.unwrap();
        let req = format!("GET /nope HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n", h.token);
        let (code, _) = request(h.addr, &req).await;
        assert_eq!(code, 404);
        h.task.abort();
    }
}
