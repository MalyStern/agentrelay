//! Cloud-delegated runs (plan §ו.4, Phase-4).
//!
//! Every other engine in this daemon is a local subprocess we watch turn by turn:
//! a guard sees each tool call before it runs, a checkpoint follows every passing
//! unit, the budget cap is checked before the money is spent. A *cloud* run is a
//! different thing entirely. You hand a task to a hosted agent — Devin's REST API,
//! or GitHub's `copilot-swe-agent[bot]` — it works on the provider's own machines,
//! and hours later a pull request appears. There is no per-turn stream to inspect.
//!
//! So the one thing this module refuses to do is pretend otherwise:
//!
//! * **A cloud run is fire-and-review, not supervised.** The guards, the checkpoint
//!   safety net, the budget kill-switch — NONE of them apply, because the work does
//!   not happen here. `describe()` says this in the first breath, not a footnote,
//!   and the CLI repeats it. Claiming the local safety rails cover a cloud run would
//!   be lying about the exact thing the user is trusting us for.
//! * **We never hold the token.** A hosted API needs a bearer token; that token
//!   comes from an environment variable *named* in the config (`token_env`), read at
//!   submit time and never stored, never logged, never in a test. If the variable is
//!   unset, submitting is a loud error that names the missing variable — the same
//!   shape as the Credential Broker.
//!
//! The state machine and the request/response *shapes* are pure functions so they
//! can be tested against captured JSON without a network. **Honesty about the
//! shapes themselves:** the Devin fields below come from Devin's published API docs
//! and the Copilot flow from GitHub's documented issue-assignment mechanism — NOT
//! from a live call in this environment. The parsers are therefore deliberately
//! conservative: an unrecognised status is reported as `Unknown`, never guessed to
//! be `Done`, so a wire-format drift degrades into "go look" rather than a
//! fabricated "your PR is ready".

use crate::error::{AgentRelayError, ErrorCode, Result};
use serde::{Deserialize, Serialize};

/// Which hosted agent, if any. `None` — the default — keeps everything local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    /// No cloud delegation. Runs are local and supervised.
    #[default]
    None,
    /// Devin (Cognition), hosted session API.
    Devin,
    /// GitHub Copilot coding agent, via `copilot-swe-agent[bot]`.
    Copilot,
}

impl CloudProvider {
    /// A human name for messages and the honesty text.
    pub fn label(&self) -> &'static str {
        match self {
            CloudProvider::None => "none",
            CloudProvider::Devin => "Devin",
            CloudProvider::Copilot => "GitHub Copilot",
        }
    }
}

/// How (or whether) to delegate a task to a hosted agent. Off by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CloudConfig {
    #[serde(default)]
    pub provider: CloudProvider,
    /// The NAME of the environment variable holding the API token — never the token
    /// itself. Read at submit time, never stored. Unset here means delegation cannot
    /// authenticate, and `resolve_token` says so by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    /// `owner/name` of the repository a Copilot task targets. Devin infers the repo
    /// from the prompt, so this is only required for Copilot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Override the API base URL — for a self-hosted/enterprise endpoint, or a test
    /// server. Empty means the provider's public default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl CloudConfig {
    pub fn is_enabled(&self) -> bool {
        self.provider != CloudProvider::None
    }

    /// The base URL for this provider, honouring an override.
    pub fn base(&self) -> &str {
        if let Some(b) = &self.base_url {
            return b.trim_end_matches('/');
        }
        match self.provider {
            CloudProvider::Devin => "https://api.devin.ai",
            CloudProvider::Copilot => "https://api.github.com",
            CloudProvider::None => "",
        }
    }

    /// What cloud delegation means here, in plain words, for `status`. The
    /// fire-and-review limitation is the first thing said, because it is the thing a
    /// user must not miss: the local guards do NOT protect a cloud run.
    pub fn describe(&self) -> String {
        if !self.is_enabled() {
            return "Cloud delegation is OFF. Tasks run locally, where the guards, \
                    checkpoints and budget cap all apply."
                .to_string();
        }
        let p = self.provider.label();
        let mut s = format!(
            "Cloud delegation is ON: a task can be handed to {p}, which runs it on its own \
             servers and returns a pull request.\n\
             WARNING: a cloud run is fire-and-review, NOT supervised. The guards, the checkpoint \
             safety net and the budget cap do NOT apply to it — that work does not happen on this \
             machine, so AgentRelay cannot see a tool call before it runs or stop it on cost. \
             Review the pull request before you merge it."
        );
        match &self.token_env {
            Some(var) => s.push_str(&format!(
                "\nToken: read at submit time from the `{var}` environment variable. It is never \
                 stored, logged, or written to config."
            )),
            None => s.push_str(
                "\nToken: NOT configured. Set `cloud.token_env` to the name of an environment \
                 variable holding the API token before delegating — submitting will refuse until \
                 then.",
            ),
        }
        if self.provider == CloudProvider::Copilot && self.repo.is_none() {
            s.push_str("\nRepository: NOT set. Copilot needs `cloud.repo` as \"owner/name\".");
        }
        s.push_str(
            "\nNote: the exact provider wire format is taken from published docs, not verified \
             against a live call here — treat a first real run as a contract test.",
        );
        s
    }

    /// Resolve the API token from the named environment variable, at call time.
    ///
    /// Two distinct loud failures, never a silent empty token: no variable named at
    /// all, or a named variable that is unset in this process. Both name the thing
    /// the user has to fix.
    pub fn resolve_token(&self) -> Result<String> {
        let var = self.token_env.as_deref().ok_or_else(|| {
            AgentRelayError::new(
                ErrorCode::CredentialUnavailable,
                format!(
                    "cannot delegate to {}: no API-token environment variable is configured. \
                     Set `cloud.token_env` to the name of the variable that holds the token \
                     (AgentRelay reads it at submit time and never stores it).",
                    self.provider.label()
                ),
            )
        })?;
        std::env::var(var).map_err(|_| {
            AgentRelayError::new(
                ErrorCode::CredentialUnavailable,
                format!(
                    "cannot delegate to {}: the config names `{var}` as the API-token variable, \
                     but it is not set in this process. Export it before delegating; AgentRelay \
                     will not store or log it.",
                    self.provider.label()
                ),
            )
        })
    }
}

/// A fully-formed HTTP request, as data, so a test can assert on the exact bytes
/// that would go on the wire without a network — the same reason `isolation` returns
/// its docker args as a list.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudRequest {
    pub method: &'static str,
    pub url: String,
    /// `("Authorization", "Bearer <token>")`. Held here only long enough to send;
    /// redact it before logging a request.
    pub auth: (String, String),
    pub content_type: Option<&'static str>,
    pub body: Option<String>,
}

/// A handle to a submitted cloud session — the id we poll and, if given, a URL a
/// human can open to watch it.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudSession {
    pub id: String,
    pub web_url: Option<String>,
}

/// Where a delegated task is. `pending`/`running`/`done`/`failed`, plus two honest
/// extras: `Blocked` (the hosted agent is waiting on a person — the cloud analogue
/// of `waiting_input`) and `Unknown` (a status string we do not recognise, reported
/// as-is rather than guessed into one of the others).
#[derive(Debug, Clone, PartialEq)]
pub enum PollState {
    /// Queued, not started yet.
    Pending,
    /// The hosted agent is working.
    Running,
    /// The hosted agent stopped and needs a human on the provider's side.
    Blocked { reason: String },
    /// Finished. `pr_url` is the pull request to review — `None` when the provider
    /// reported completion but no PR link, which is itself worth surfacing.
    Done { pr_url: Option<String> },
    /// Ended without completing.
    Failed { reason: String },
    /// A status this build does not recognise. Never treated as done: a fabricated
    /// "your PR is ready" is the one wrong answer this whole module exists to avoid.
    Unknown { raw: String },
}

impl PollState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, PollState::Done { .. } | PollState::Failed { .. })
    }

    /// One line for a human or a notification headline.
    pub fn describe(&self) -> String {
        match self {
            PollState::Pending => "queued — the hosted agent has not started yet".to_string(),
            PollState::Running => "the hosted agent is working".to_string(),
            PollState::Blocked { reason } => {
                format!("the hosted agent is blocked and needs a person: {reason}")
            }
            PollState::Done { pr_url: Some(url) } => format!("finished — review the pull request: {url}"),
            PollState::Done { pr_url: None } => {
                "finished, but the provider reported no pull-request URL — open the session to check"
                    .to_string()
            }
            PollState::Failed { reason } => format!("the hosted agent failed: {reason}"),
            PollState::Unknown { raw } => format!(
                "status \"{raw}\" is not one AgentRelay recognises — open the session on the \
                 provider to see what happened"
            ),
        }
    }
}

fn auth_header(token: &str) -> (String, String) {
    ("Authorization".to_string(), format!("Bearer {token}"))
}

/// Pull a pull-request URL out of a variety of shapes providers use.
fn find_pr_url(v: &serde_json::Value) -> Option<String> {
    // Devin: pull_request may be an object {url|html_url} or a bare string.
    if let Some(pr) = v.get("pull_request") {
        if let Some(s) = pr.as_str() {
            return Some(s.to_string());
        }
        for k in ["html_url", "url"] {
            if let Some(s) = pr.get(k).and_then(|x| x.as_str()) {
                return Some(s.to_string());
            }
        }
    }
    // Some payloads nest it under structured_output.
    if let Some(s) = v
        .get("structured_output")
        .and_then(|o| o.get("pr_url").or_else(|| o.get("pull_request_url")))
        .and_then(|x| x.as_str())
    {
        return Some(s.to_string());
    }
    for k in ["pr_url", "pull_request_url"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Devin (published REST docs, not a live call)
// ---------------------------------------------------------------------------

/// Build the request that submits a task to Devin: `POST {base}/v3/sessions`.
pub fn devin_submit_request(base: &str, token: &str, prompt: &str) -> CloudRequest {
    CloudRequest {
        method: "POST",
        url: format!("{}/v3/sessions", base.trim_end_matches('/')),
        auth: auth_header(token),
        content_type: Some("application/json"),
        body: Some(serde_json::json!({ "prompt": prompt }).to_string()),
    }
}

/// Build the request that polls one Devin session: `GET {base}/v3/sessions/{id}`.
pub fn devin_poll_request(base: &str, token: &str, session_id: &str) -> CloudRequest {
    CloudRequest {
        method: "GET",
        url: format!("{}/v3/sessions/{}", base.trim_end_matches('/'), session_id),
        auth: auth_header(token),
        content_type: None,
        body: None,
    }
}

/// Parse Devin's submit response into a session handle.
pub fn parse_devin_submit(v: &serde_json::Value) -> Result<CloudSession> {
    let id = v
        .get("session_id")
        .or_else(|| v.get("id"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            AgentRelayError::new(
                ErrorCode::ProviderProtocolMismatch,
                "Devin submit response had no session_id — cannot poll a session we cannot name"
                    .to_string(),
            )
        })?;
    Ok(CloudSession {
        id: id.to_string(),
        web_url: v
            .get("url")
            .or_else(|| v.get("web_url"))
            .and_then(|x| x.as_str())
            .map(String::from),
    })
}

/// Parse Devin's poll response into a `PollState`.
pub fn parse_devin_poll(v: &serde_json::Value) -> PollState {
    let raw = v
        .get("status_enum")
        .or_else(|| v.get("status"))
        .and_then(|x| x.as_str())
        .unwrap_or("");
    match raw.to_ascii_lowercase().as_str() {
        // Devin's documented lifecycle strings.
        "queued" | "pending" | "starting" => PollState::Pending,
        "running" | "working" | "resumed" => PollState::Running,
        "blocked" | "suspended" => PollState::Blocked {
            reason: v
                .get("status_message")
                .and_then(|x| x.as_str())
                .unwrap_or("waiting for input on the provider's side")
                .to_string(),
        },
        "finished" | "completed" | "done" | "success" => PollState::Done {
            pr_url: find_pr_url(v),
        },
        "expired" | "failed" | "error" | "cancelled" | "canceled" => PollState::Failed {
            reason: v
                .get("status_message")
                .and_then(|x| x.as_str())
                .unwrap_or(raw)
                .to_string(),
        },
        // Anything else, including a missing status, is reported honestly rather
        // than assumed. "" becomes an Unknown with an empty raw, which describe()
        // still routes to "go look".
        _ => PollState::Unknown {
            raw: raw.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// GitHub Copilot (documented issue-assignment flow, not a live call)
// ---------------------------------------------------------------------------

/// Build the request that assigns a new issue to `copilot-swe-agent[bot]`, which is
/// GitHub's documented way to hand it a task: `POST {base}/repos/{repo}/issues`.
///
/// `repo` is `owner/name`. Copilot picks up the assigned issue and opens a pull
/// request; polling for that PR is the seam below.
pub fn copilot_submit_request(base: &str, token: &str, repo: &str, prompt: &str) -> CloudRequest {
    // Title is the first line; the whole prompt is the body so nothing is lost.
    let title = prompt
        .lines()
        .next()
        .unwrap_or("AgentRelay task")
        .chars()
        .take(120)
        .collect::<String>();
    CloudRequest {
        method: "POST",
        url: format!("{}/repos/{}/issues", base.trim_end_matches('/'), repo),
        auth: auth_header(token),
        content_type: Some("application/json"),
        body: Some(
            serde_json::json!({
                "title": title,
                "body": prompt,
                "assignees": ["copilot-swe-agent[bot]"]
            })
            .to_string(),
        ),
    }
}

/// Build the request that reads the issue Copilot is working from:
/// `GET {base}/repos/{repo}/issues/{number}`.
pub fn copilot_poll_request(
    base: &str,
    token: &str,
    repo: &str,
    issue_number: &str,
) -> CloudRequest {
    CloudRequest {
        method: "GET",
        url: format!(
            "{}/repos/{}/issues/{}",
            base.trim_end_matches('/'),
            repo,
            issue_number
        ),
        auth: auth_header(token),
        content_type: None,
        body: None,
    }
}

/// Parse Copilot's submit response (a created GitHub issue) into a session handle —
/// the session id is the issue number.
pub fn parse_copilot_submit(v: &serde_json::Value) -> Result<CloudSession> {
    let number = v.get("number").and_then(|x| x.as_i64()).ok_or_else(|| {
        AgentRelayError::new(
            ErrorCode::ProviderProtocolMismatch,
            "Copilot submit response had no issue number — cannot track a task we cannot name"
                .to_string(),
        )
    })?;
    Ok(CloudSession {
        id: number.to_string(),
        web_url: v.get("html_url").and_then(|x| x.as_str()).map(String::from),
    })
}

/// Parse Copilot's poll response (a GitHub issue) into a `PollState`.
///
/// Deliberately conservative: GitHub's issue JSON does not link the pull request
/// Copilot opens, so a closed issue with no discoverable PR is reported `Done` with
/// no URL rather than inventing one. If the linked PR does surface (some responses
/// carry `pull_request`), it is used.
pub fn parse_copilot_poll(v: &serde_json::Value) -> PollState {
    let pr = find_pr_url(v);
    match v.get("state").and_then(|x| x.as_str()).unwrap_or("") {
        "open" => match pr {
            // A PR already linked while the issue is open means the agent finished
            // and opened it.
            Some(url) => PollState::Done { pr_url: Some(url) },
            None => PollState::Running,
        },
        "closed" => PollState::Done { pr_url: pr },
        other => PollState::Unknown {
            raw: other.to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Provider-agnostic dispatch
// ---------------------------------------------------------------------------

/// Build the submit request for the configured provider, resolving the token from
/// the environment. Refuses loudly when delegation is off, the token is unset, or a
/// required field (Copilot's repo) is missing.
pub fn submit_request(cfg: &CloudConfig, prompt: &str) -> Result<CloudRequest> {
    let token = cfg.resolve_token()?;
    match cfg.provider {
        CloudProvider::None => Err(AgentRelayError::new(
            ErrorCode::Internal,
            "cloud delegation is off (cloud.provider = none) — nothing to submit".to_string(),
        )),
        CloudProvider::Devin => Ok(devin_submit_request(cfg.base(), &token, prompt)),
        CloudProvider::Copilot => {
            let repo = cfg.repo.as_deref().ok_or_else(|| {
                AgentRelayError::new(
                    ErrorCode::Internal,
                    "Copilot needs a target repository: set `cloud.repo` to \"owner/name\""
                        .to_string(),
                )
            })?;
            Ok(copilot_submit_request(cfg.base(), &token, repo, prompt))
        }
    }
}

/// Build the poll request for the configured provider.
pub fn poll_request(cfg: &CloudConfig, session_id: &str) -> Result<CloudRequest> {
    let token = cfg.resolve_token()?;
    match cfg.provider {
        CloudProvider::None => Err(AgentRelayError::new(
            ErrorCode::Internal,
            "cloud delegation is off — nothing to poll".to_string(),
        )),
        CloudProvider::Devin => Ok(devin_poll_request(cfg.base(), &token, session_id)),
        CloudProvider::Copilot => {
            let repo = cfg.repo.as_deref().ok_or_else(|| {
                AgentRelayError::new(
                    ErrorCode::Internal,
                    "Copilot needs a target repository: set `cloud.repo` to \"owner/name\""
                        .to_string(),
                )
            })?;
            Ok(copilot_poll_request(cfg.base(), &token, repo, session_id))
        }
    }
}

/// Parse a submit response for the configured provider.
pub fn parse_submit(provider: CloudProvider, v: &serde_json::Value) -> Result<CloudSession> {
    match provider {
        CloudProvider::Devin => parse_devin_submit(v),
        CloudProvider::Copilot => parse_copilot_submit(v),
        CloudProvider::None => Err(AgentRelayError::new(
            ErrorCode::Internal,
            "no cloud provider configured".to_string(),
        )),
    }
}

/// Parse a poll response for the configured provider.
pub fn parse_poll(provider: CloudProvider, v: &serde_json::Value) -> PollState {
    match provider {
        CloudProvider::Devin => parse_devin_poll(v),
        CloudProvider::Copilot => parse_copilot_poll(v),
        CloudProvider::None => PollState::Unknown {
            raw: "no provider".to_string(),
        },
    }
}

/// Perform a built request over the network and return the parsed JSON body.
///
/// The one impure function here, and the only one no test touches — the pure
/// builders and parsers above are what carry the behaviour. Kept thin on purpose so
/// there is nothing to test in it that the shapes above do not already cover.
pub fn execute(req: &CloudRequest) -> Result<serde_json::Value> {
    let net_err = |e: ureq::Error| {
        AgentRelayError::new(
            ErrorCode::ProviderUnavailable,
            format!("cloud request to {} failed: {e}", req.url),
        )
    };
    let resp = match req.method {
        "GET" => {
            let mut rb = ureq::get(&req.url)
                .header(&req.auth.0, &req.auth.1)
                .header("Accept", "application/json");
            if let Some(ct) = req.content_type {
                rb = rb.header("Content-Type", ct);
            }
            rb.call().map_err(net_err)?
        }
        "POST" => {
            let mut rb = ureq::post(&req.url)
                .header(&req.auth.0, &req.auth.1)
                .header("Accept", "application/json");
            if let Some(ct) = req.content_type {
                rb = rb.header("Content-Type", ct);
            }
            rb.send(req.body.as_deref().unwrap_or(""))
                .map_err(net_err)?
        }
        other => {
            return Err(AgentRelayError::new(
                ErrorCode::Internal,
                format!("unsupported cloud request method {other}"),
            ))
        }
    };
    let text = resp.into_body().read_to_string().map_err(|e| {
        AgentRelayError::new(
            ErrorCode::ProviderStreamError,
            format!("could not read the cloud response body: {e}"),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| {
        AgentRelayError::new(
            ErrorCode::ProviderProtocolMismatch,
            format!(
                "the cloud response was not JSON: {e} (first bytes: {})",
                // char-safe: slicing &text[..200] by byte index panics when byte 200
                // lands mid-character, and a provider's HTML error page (multibyte,
                // >200 bytes) is exactly the input that reaches this arm.
                text.chars().take(200).collect::<String>()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cloud_is_off_by_default_and_says_the_local_guards_apply() {
        let c = CloudConfig::default();
        assert!(!c.is_enabled());
        assert_eq!(c.provider, CloudProvider::None);
        let d = c.describe();
        assert!(d.contains("Cloud delegation is OFF"), "{d}");
        assert!(
            d.contains("guards, checkpoints and budget cap all apply"),
            "{d}"
        );
    }

    #[test]
    fn an_enabled_cloud_run_states_up_front_that_it_is_not_supervised() {
        // The limitation must be visible, not buried: the whole point of the local
        // supervisor is that it does NOT cover a cloud run, and a user must not
        // assume it does.
        let c = CloudConfig {
            provider: CloudProvider::Devin,
            token_env: Some("DEVIN_API_KEY".into()),
            ..Default::default()
        };
        let d = c.describe();
        assert!(d.contains("fire-and-review, NOT supervised"), "{d}");
        assert!(d.contains("do NOT apply"), "{d}");
        assert!(d.contains("Review the pull request"), "{d}");
        // And it names the env var without ever needing the token itself.
        assert!(d.contains("DEVIN_API_KEY"), "{d}");
        assert!(d.contains("never stored"), "{d}");
    }

    #[test]
    fn submitting_without_the_token_env_configured_is_a_loud_named_error() {
        // No token_env at all: the failure names what to set, it does not silently
        // send an empty Authorization header.
        let c = CloudConfig {
            provider: CloudProvider::Devin,
            ..Default::default()
        };
        let err = submit_request(&c, "fix the parser").unwrap_err();
        assert_eq!(err.code, ErrorCode::CredentialUnavailable);
        assert!(err.message.contains("cloud.token_env"), "{}", err.message);
    }

    #[test]
    fn submitting_with_an_unset_named_variable_names_the_variable() {
        // The variable is named in config but not exported. The error must name IT,
        // so the user knows exactly which variable to set — and no token is invented.
        let var = "AGENTRELAY_TEST_TOKEN_THAT_IS_NOT_SET_XYZ";
        assert!(
            std::env::var(var).is_err(),
            "test precondition: the var must be unset"
        );
        let c = CloudConfig {
            provider: CloudProvider::Devin,
            token_env: Some(var.to_string()),
            ..Default::default()
        };
        let err = c.resolve_token().unwrap_err();
        assert_eq!(err.code, ErrorCode::CredentialUnavailable);
        assert!(err.message.contains(var), "{}", err.message);
        assert!(err.message.contains("not set"), "{}", err.message);
    }

    #[test]
    fn the_devin_submit_request_is_the_shape_the_docs_describe() {
        let req = devin_submit_request("https://api.devin.ai", "tok_123", "fix the flaky test");
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://api.devin.ai/v3/sessions");
        assert_eq!(
            req.auth,
            ("Authorization".to_string(), "Bearer tok_123".to_string())
        );
        // The prompt is carried in the JSON body, not the URL — a task is not a query
        // parameter, and secrets in URLs are a rule this codebase keeps.
        let body: serde_json::Value = serde_json::from_str(req.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["prompt"], "fix the flaky test");
    }

    #[test]
    fn a_base_url_override_is_honoured_so_a_test_server_or_enterprise_host_works() {
        let c = CloudConfig {
            provider: CloudProvider::Devin,
            token_env: Some("DEVIN_API_KEY".into()),
            base_url: Some("http://127.0.0.1:9999/".into()),
            ..Default::default()
        };
        assert_eq!(c.base(), "http://127.0.0.1:9999");
        let req = devin_submit_request(c.base(), "t", "p");
        assert_eq!(req.url, "http://127.0.0.1:9999/v3/sessions");
    }

    #[test]
    fn a_devin_submit_response_yields_a_session_we_can_poll() {
        // Synthetic, shaped like the documented response.
        let v =
            json!({ "session_id": "devin-abc123", "url": "https://app.devin.ai/sessions/abc123" });
        let s = parse_devin_submit(&v).unwrap();
        assert_eq!(s.id, "devin-abc123");
        assert_eq!(
            s.web_url.as_deref(),
            Some("https://app.devin.ai/sessions/abc123")
        );

        // A response with no id cannot be polled, and that is an error — not a
        // session with an empty id that every later poll would 404 on.
        let bad = json!({ "message": "accepted" });
        assert!(parse_devin_submit(&bad).is_err());
    }

    #[test]
    fn the_devin_poll_parser_walks_the_whole_lifecycle() {
        assert_eq!(
            parse_devin_poll(&json!({ "status_enum": "queued" })),
            PollState::Pending
        );
        assert_eq!(
            parse_devin_poll(&json!({ "status_enum": "running" })),
            PollState::Running
        );
        match parse_devin_poll(
            &json!({ "status_enum": "blocked", "status_message": "needs a decision" }),
        ) {
            PollState::Blocked { reason } => assert!(reason.contains("needs a decision")),
            other => panic!("expected blocked, got {other:?}"),
        }
        match parse_devin_poll(&json!({
            "status_enum": "finished",
            "pull_request": { "html_url": "https://github.com/o/r/pull/7" }
        })) {
            PollState::Done { pr_url } => {
                assert_eq!(pr_url.as_deref(), Some("https://github.com/o/r/pull/7"))
            }
            other => panic!("expected done with a PR, got {other:?}"),
        }
        match parse_devin_poll(&json!({ "status_enum": "expired" })) {
            PollState::Failed { .. } => {}
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn an_unrecognised_status_is_reported_not_guessed_to_be_done() {
        // The one wrong answer this module exists to prevent: telling a sleeping
        // user their PR is ready when it is not. A new/unknown status must degrade
        // to "go look", never to Done.
        let s = parse_devin_poll(&json!({ "status_enum": "some_new_state_we_never_saw" }));
        assert_eq!(
            s,
            PollState::Unknown {
                raw: "some_new_state_we_never_saw".into()
            }
        );
        assert!(!s.is_terminal(), "an unknown status is not a finished one");
        assert!(s.describe().contains("not one AgentRelay recognises"));

        // A completely missing status is likewise not a success.
        let missing = parse_devin_poll(&json!({ "id": "x" }));
        assert!(matches!(missing, PollState::Unknown { .. }));
        assert!(!missing.is_terminal());
    }

    #[test]
    fn a_finished_run_with_no_pr_link_says_so_rather_than_claiming_a_url() {
        let s = parse_devin_poll(&json!({ "status_enum": "finished" }));
        assert_eq!(s, PollState::Done { pr_url: None });
        assert!(s.is_terminal());
        assert!(
            s.describe().contains("no pull-request URL"),
            "{}",
            s.describe()
        );
    }

    #[test]
    fn copilot_submits_by_assigning_an_issue_to_the_bot() {
        let c = CloudConfig {
            provider: CloudProvider::Copilot,
            token_env: Some("GITHUB_TOKEN".into()),
            repo: Some("acme/widgets".into()),
            ..Default::default()
        };
        let req =
            submit_request_with_token(&c, "gh_tok", "Fix login bug\nmore detail here").unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://api.github.com/repos/acme/widgets/issues");
        let body: serde_json::Value = serde_json::from_str(req.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["assignees"][0], "copilot-swe-agent[bot]");
        assert_eq!(body["title"], "Fix login bug", "title is the first line");
        assert_eq!(
            body["body"], "Fix login bug\nmore detail here",
            "the whole prompt is kept"
        );
    }

    #[test]
    fn copilot_without_a_repo_refuses_rather_than_building_a_broken_url() {
        let c = CloudConfig {
            provider: CloudProvider::Copilot,
            token_env: Some("GITHUB_TOKEN".into()),
            repo: None,
            ..Default::default()
        };
        let err = submit_request_with_token(&c, "gh_tok", "do the thing").unwrap_err();
        assert!(err.message.contains("cloud.repo"), "{}", err.message);
    }

    #[test]
    fn the_copilot_poll_parser_is_conservative_about_completion() {
        // Open issue, no PR yet: still running.
        assert_eq!(
            parse_copilot_poll(&json!({ "state": "open" })),
            PollState::Running
        );
        // Closed issue with a discoverable PR: done, with the URL.
        match parse_copilot_poll(
            &json!({ "state": "closed", "pr_url": "https://github.com/o/r/pull/9" }),
        ) {
            PollState::Done { pr_url } => {
                assert_eq!(pr_url.as_deref(), Some("https://github.com/o/r/pull/9"))
            }
            other => panic!("expected done, got {other:?}"),
        }
        // Closed with no PR we can find: done but honest about the missing link,
        // never a fabricated URL.
        assert_eq!(
            parse_copilot_poll(&json!({ "state": "closed" })),
            PollState::Done { pr_url: None }
        );
    }

    #[test]
    fn config_round_trips_through_json_and_defaults_to_off() {
        let c = CloudConfig {
            provider: CloudProvider::Devin,
            token_env: Some("DEVIN_API_KEY".into()),
            repo: None,
            base_url: None,
        };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<CloudConfig>(&s).unwrap(), c);
        // An absent cloud block is off, not an error.
        assert_eq!(
            serde_json::from_str::<CloudConfig>("{}").unwrap(),
            CloudConfig::default()
        );
        assert!(!serde_json::from_str::<CloudConfig>("{}")
            .unwrap()
            .is_enabled());
        // The token is NEVER part of the serialized config — only the variable name.
        assert!(!s.contains("Bearer"), "{s}");
    }

    /// A tiny test seam so the Copilot submit path can be exercised without an
    /// environment variable — production always resolves the token from ENV.
    fn submit_request_with_token(
        cfg: &CloudConfig,
        token: &str,
        prompt: &str,
    ) -> Result<CloudRequest> {
        match cfg.provider {
            CloudProvider::Copilot => {
                let repo = cfg.repo.as_deref().ok_or_else(|| {
                    AgentRelayError::new(
                        ErrorCode::Internal,
                        "Copilot needs a target repository: set `cloud.repo` to \"owner/name\""
                            .to_string(),
                    )
                })?;
                Ok(copilot_submit_request(cfg.base(), token, repo, prompt))
            }
            CloudProvider::Devin => Ok(devin_submit_request(cfg.base(), token, prompt)),
            CloudProvider::None => {
                Err(AgentRelayError::new(ErrorCode::Internal, "off".to_string()))
            }
        }
    }

    #[test]
    fn a_multibyte_error_body_would_not_panic_the_slice() {
        // The bug: `&text[..200]` panics when byte 200 lands mid-character, and a
        // provider's HTML error page (multibyte, >200 bytes) is exactly what reaches
        // that arm. char-based truncation is safe for any input. Prove the technique
        // on a hostile string of 3-byte chars, where byte 200 is always mid-char.
        let hostile: String = "\u{2764}".repeat(300);
        let cut: String = hostile.chars().take(200).collect();
        assert_eq!(cut.chars().count(), 200);
        assert!(
            cut.len() > 200,
            "bytes exceed chars, and taking chars did not panic"
        );
    }
}
