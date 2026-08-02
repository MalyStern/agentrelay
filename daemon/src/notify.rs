//! Push notifications (plan §9: ntfy primary, "Push פעולתי", Phase-1).
//!
//! The whole premise of an overnight run is that you are asleep. So the one thing
//! the supervisor must be able to do is reach you — when a unit fails, when it is
//! blocked waiting for a decision, when the night is over.
//!
//! **This is the only part of AgentRelay that talks to the network, and it is off
//! unless you turn it on.** Everything else in this daemon is local by
//! construction. A notification leaves your machine and reaches a server you do not
//! control, so:
//!
//! * it is disabled by default, and configuring it is an explicit act;
//! * the body is the report *headline* — counts and unit ids — never file contents,
//!   diffs, prompts or output. `Notification::new` takes a headline, not a report,
//!   so there is no path by which the code accidentally sends more;
//! * `describe_privacy()` states exactly what would be sent, so the user can read
//!   it before enabling anything rather than trusting this comment.
//!
//! ntfy is the primary channel because it needs no account and can be self-hosted:
//! point `server` at your own instance and nothing reaches a third party at all.

use crate::error::{AgentRelayError, ErrorCode, Result};
use serde::{Deserialize, Serialize};

/// How urgent — maps to ntfy's priority header, which drives whether a phone
/// actually wakes up for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    /// The night finished cleanly. Can wait for morning.
    Quiet,
    /// Something needs a person: a failed unit, a blocked guard.
    Attention,
}

impl Urgency {
    /// ntfy priorities: 1 min … 5 max. `low` will not buzz a phone; `high` will.
    pub fn ntfy_priority(&self) -> &'static str {
        match self {
            Urgency::Quiet => "low",
            Urgency::Attention => "high",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub title: String,
    /// A headline. Deliberately not a report — see the module comment.
    pub headline: String,
    pub urgency: Urgency,
}

impl Notification {
    pub fn new(title: impl Into<String>, headline: impl Into<String>, urgency: Urgency) -> Self {
        Notification { title: title.into(), headline: headline.into(), urgency }
    }
}

/// Where notifications go. `None` — the default — means nowhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotifyConfig {
    /// Explicit "do not send anything". The default.
    None,
    Ntfy {
        /// Your topic. Anyone who knows a public ntfy topic can read it, so this
        /// should be unguessable — hence the warning in `describe_privacy`.
        topic: String,
        /// Defaults to https://ntfy.sh. Point this at your own instance to keep
        /// everything on hardware you control.
        #[serde(default = "default_ntfy_server")]
        server: String,
    },
}

fn default_ntfy_server() -> String {
    "https://ntfy.sh".to_string()
}

impl Default for NotifyConfig {
    fn default() -> Self {
        NotifyConfig::None
    }
}

impl NotifyConfig {
    pub fn is_enabled(&self) -> bool {
        !matches!(self, NotifyConfig::None)
    }

    /// Exactly what leaves the machine, in plain words, so the user can decide
    /// before enabling rather than after. Shown by `agentrelay status`.
    pub fn describe_privacy(&self) -> String {
        match self {
            NotifyConfig::None => "Notifications are OFF. Nothing leaves this machine.".to_string(),
            NotifyConfig::Ntfy { topic, server } => {
                let third_party = server.trim_end_matches('/') == "https://ntfy.sh";
                let mut s = format!(
                    "Notifications are ON. A short line — a title and a headline such as \
                     \"2 unit(s) done, 1 FAILED\" — will be sent to {server} on topic \"{topic}\".\n\
                     No file contents, diffs, prompts, command output or paths are ever sent."
                );
                if third_party {
                    s.push_str(
                        "\nNOTE: ntfy.sh is a public server run by someone else, and anyone who \
                         guesses your topic name can read your notifications. Use a long random \
                         topic, or self-host and set `server` to your own instance.",
                    );
                }
                s
            }
        }
    }
}

/// Somewhere a notification can be delivered. A trait so tests can assert on what
/// would be sent without a network, and so Telegram can be added later.
pub trait Notifier: Send + Sync {
    fn send(&self, n: &Notification) -> Result<()>;
    fn describe(&self) -> String;
}

/// Sends nothing. Not a stub — this is the default configuration.
pub struct NullNotifier;

impl Notifier for NullNotifier {
    fn send(&self, _n: &Notification) -> Result<()> {
        Ok(())
    }
    fn describe(&self) -> String {
        "disabled".to_string()
    }
}

/// Records what it was asked to send. For tests and `--dry-run`.
#[derive(Default)]
pub struct RecordingNotifier {
    pub sent: std::sync::Mutex<Vec<Notification>>,
}

impl Notifier for RecordingNotifier {
    fn send(&self, n: &Notification) -> Result<()> {
        self.sent.lock().unwrap().push(n.clone());
        Ok(())
    }
    fn describe(&self) -> String {
        "recording (nothing is sent)".to_string()
    }
}

pub struct NtfyNotifier {
    server: String,
    topic: String,
}

impl NtfyNotifier {
    pub fn new(server: impl Into<String>, topic: impl Into<String>) -> Self {
        NtfyNotifier { server: server.into(), topic: topic.into() }
    }

    pub fn url(&self) -> String {
        format!("{}/{}", self.server.trim_end_matches('/'), self.topic)
    }
}

impl Notifier for NtfyNotifier {
    fn send(&self, n: &Notification) -> Result<()> {
        let body = n.headline.clone();
        let resp = ureq::post(&self.url())
            .header("Title", &sanitize_header(&n.title))
            .header("Priority", n.urgency.ntfy_priority())
            .header("Tags", "robot")
            .send(&body);

        match resp {
            Ok(_) => Ok(()),
            // A notification that cannot be delivered must never take the run down
            // with it — the caller decides, and every caller here logs and carries
            // on. Losing a night's work because a phone server was down would be
            // an absurd way to fail.
            Err(e) => Err(AgentRelayError::new(
                ErrorCode::Internal,
                format!("could not notify {}: {e}", self.url()),
            )),
        }
    }

    fn describe(&self) -> String {
        format!("ntfy → {}", self.url())
    }
}

/// HTTP headers cannot contain newlines; a title carrying one would either be
/// rejected or, worse, allow extra headers to be injected into the request.
fn sanitize_header(s: &str) -> String {
    s.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).take(200).collect()
}

/// Build the configured notifier. Returns `NullNotifier` when notifications are off.
pub fn from_config(cfg: &NotifyConfig) -> Box<dyn Notifier> {
    match cfg {
        NotifyConfig::None => Box::new(NullNotifier),
        NotifyConfig::Ntfy { topic, server } => Box::new(NtfyNotifier::new(server, topic)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A one-shot HTTP server that captures the request it receives. Real sockets,
    /// so this exercises the actual request the daemon would put on the wire —
    /// asserting on a mock would only prove the mock agrees with itself.
    fn capture_one_request() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut raw = String::new();
                let mut content_len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                    raw.push_str(&line);
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }
                let mut body = vec![0u8; content_len];
                let _ = reader.read_exact(&mut body);
                raw.push_str(&String::from_utf8_lossy(&body));
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
                let _ = tx.send(raw);
            }
        });
        (format!("http://{addr}"), rx)
    }

    #[test]
    fn off_by_default_and_nothing_is_sent() {
        // The default must be silence: this is the only network path in the daemon.
        let cfg = NotifyConfig::default();
        assert!(!cfg.is_enabled());
        assert!(cfg.describe_privacy().contains("Nothing leaves this machine"));
        assert_eq!(from_config(&cfg).describe(), "disabled");
    }

    #[test]
    fn the_privacy_note_says_what_would_actually_be_sent() {
        let cfg = NotifyConfig::Ntfy {
            topic: "my-secret-topic".into(),
            server: default_ntfy_server(),
        };
        let d = cfg.describe_privacy();
        assert!(d.contains("my-secret-topic"));
        assert!(d.contains("No file contents"), "{d}");
        // A public topic is readable by anyone who guesses it. Say so before the
        // user enables it, not in a doc they will not read.
        assert!(d.contains("anyone who guesses your topic"), "{d}");

        // Self-hosted: the third-party warning is not applicable and must not appear.
        let mine = NotifyConfig::Ntfy { topic: "t".into(), server: "https://ntfy.example.dev".into() };
        assert!(!mine.describe_privacy().contains("public server"));
    }

    #[test]
    fn a_real_post_carries_the_headline_and_the_right_priority() {
        let (base, rx) = capture_one_request();
        let n = NtfyNotifier::new(&base, "topic123");
        n.send(&Notification::new(
            "AgentRelay",
            "2 unit(s) done, 1 FAILED",
            Urgency::Attention,
        ))
        .unwrap();

        // HTTP header names are case-insensitive and ureq sends them lowercased,
        // so the assertions match the way the protocol actually works.
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().to_lowercase();
        assert!(raw.starts_with("post /topic123 "), "got: {}", &raw[..40.min(raw.len())]);
        assert!(raw.contains("title: agentrelay"), "RAW>>>{raw}<<<");
        assert!(raw.contains("priority: high"), "a failure has to buzz the phone: {raw}");
        assert!(raw.ends_with("2 unit(s) done, 1 failed"), "the body is the headline: {raw}");
    }

    #[test]
    fn a_quiet_night_does_not_buzz_the_phone() {
        let (base, rx) = capture_one_request();
        NtfyNotifier::new(&base, "t")
            .send(&Notification::new("AgentRelay", "3 unit(s) done", Urgency::Quiet))
            .unwrap();
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().to_lowercase();
        assert!(raw.contains("priority: low"), "{raw}");
    }

    #[test]
    fn a_newline_in_a_title_cannot_inject_a_header() {
        let (base, rx) = capture_one_request();
        NtfyNotifier::new(&base, "t")
            .send(&Notification::new(
                "hi\r\nX-Evil: yes",
                "body",
                Urgency::Quiet,
            ))
            .unwrap();
        let raw = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().to_lowercase();
        // The property is not "the text is absent" — it legitimately appears inside
        // the title's value. It is that no LINE begins with it, since that is what
        // would turn it into a header of its own.
        assert!(
            !raw.lines().any(|l| l.trim_start().starts_with("x-evil:")),
            "header injection: {raw}"
        );
        assert!(raw.contains("title: hi  x-evil: yes"), "the newline became a space: {raw}");
    }

    #[test]
    fn an_unreachable_server_is_an_error_not_a_panic() {
        // Nothing about a night's work should depend on a phone server being up.
        let n = NtfyNotifier::new("http://127.0.0.1:1", "t");
        let err = match n.send(&Notification::new("t", "b", Urgency::Quiet)) {
            Err(e) => e.to_string(),
            Ok(()) => panic!("a dead server must be reported"),
        };
        assert!(err.contains("could not notify"), "{err}");
    }

    #[test]
    fn config_round_trips_through_json_so_it_can_live_in_config_json() {
        let cfg = NotifyConfig::Ntfy { topic: "abc".into(), server: default_ntfy_server() };
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(serde_json::from_str::<NotifyConfig>(&json).unwrap(), cfg);

        // `server` may be omitted — the common case is the default.
        let short: NotifyConfig =
            serde_json::from_str(r#"{"kind":"ntfy","topic":"abc"}"#).unwrap();
        assert_eq!(short, cfg);
    }
}
