//! Credential Broker (plan 02 §ח — the #1 thing that was missing). Secrets live in
//! the OS keychain; the database stores only a `keyring_ref` (a name), never the
//! value. Secrets reach an agent ONLY as environment variables injected at spawn,
//! and are redacted from logs. The storage backend is a trait so tests use an
//! in-memory store and production uses the OS keychain.

use crate::error::{AgentRelayError, ErrorCode, Result};
use std::collections::HashMap;
use std::sync::Mutex;

/// Abstract secret storage. The value never touches the DB or disk (in prod).
pub trait SecretStore: Send + Sync {
    fn set(&self, name: &str, secret: &str) -> Result<()>;
    fn get(&self, name: &str) -> Result<String>;
    fn delete(&self, name: &str) -> Result<()>;
}

/// Deterministic in-memory store for tests.
#[derive(Default)]
pub struct InMemoryStore {
    map: Mutex<HashMap<String, String>>,
}

impl SecretStore for InMemoryStore {
    fn set(&self, name: &str, secret: &str) -> Result<()> {
        self.map
            .lock()
            .unwrap()
            .insert(name.to_string(), secret.to_string());
        Ok(())
    }
    fn get(&self, name: &str) -> Result<String> {
        self.map.lock().unwrap().get(name).cloned().ok_or_else(|| {
            AgentRelayError::new(
                ErrorCode::CredentialUnavailable,
                format!("no secret '{name}'"),
            )
        })
    }
    fn delete(&self, name: &str) -> Result<()> {
        self.map.lock().unwrap().remove(name);
        Ok(())
    }
}

/// The real OS keychain (Windows Credential Manager / macOS Keychain / Linux Secret
/// Service), via the `keyring` crate.
pub struct OsKeychain {
    service: String,
}

impl OsKeychain {
    pub fn new(service: impl Into<String>) -> Self {
        OsKeychain {
            service: service.into(),
        }
    }
    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, name)
            .map_err(|e| AgentRelayError::new(ErrorCode::CredentialUnavailable, e.to_string()))
    }
}

impl SecretStore for OsKeychain {
    fn set(&self, name: &str, secret: &str) -> Result<()> {
        self.entry(name)?
            .set_password(secret)
            .map_err(|e| AgentRelayError::new(ErrorCode::CredentialUnavailable, e.to_string()))
    }
    fn get(&self, name: &str) -> Result<String> {
        self.entry(name)?
            .get_password()
            .map_err(|e| AgentRelayError::new(ErrorCode::CredentialUnavailable, e.to_string()))
    }
    fn delete(&self, name: &str) -> Result<()> {
        self.entry(name)?
            .delete_credential()
            .map_err(|e| AgentRelayError::new(ErrorCode::CredentialUnavailable, e.to_string()))
    }
}

/// A DB-safe reference to a stored credential — the metadata only. NO secret value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredRef {
    pub name: String,
    pub keyring_ref: String,
}

pub struct CredentialBroker {
    store: Box<dyn SecretStore>,
}

impl CredentialBroker {
    pub fn new(store: Box<dyn SecretStore>) -> Self {
        CredentialBroker { store }
    }
    /// Production broker backed by the OS keychain under service "agentrelay".
    pub fn os() -> Self {
        CredentialBroker::new(Box::new(OsKeychain::new("agentrelay")))
    }

    /// Store a secret; returns the DB-safe ref (the secret goes only to the store).
    pub fn store_secret(&self, name: &str, secret: &str) -> Result<CredRef> {
        self.store.set(name, secret)?;
        Ok(CredRef {
            name: name.to_string(),
            keyring_ref: format!("agentrelay:{name}"),
        })
    }
    pub fn get_secret(&self, name: &str) -> Result<String> {
        self.store.get(name)
    }
    pub fn delete_secret(&self, name: &str) -> Result<()> {
        self.store.delete(name)
    }

    /// Resolve credential refs into an env map for injection at spawn:
    /// `(ENV_VAR, credential_name)` -> `{ENV_VAR: secret}`. Secrets exist only in
    /// this map, passed straight to the child process env.
    pub fn env_for(&self, bindings: &[(String, String)]) -> Result<HashMap<String, String>> {
        let mut env = HashMap::new();
        for (var, cred) in bindings {
            env.insert(var.clone(), self.get_secret(cred)?);
        }
        Ok(env)
    }
}

/// Replace every secret value with `***` in a string (for logs). Longest-first so a
/// secret that contains another isn't partially left behind.
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut ordered: Vec<&String> = secrets.iter().filter(|s| !s.is_empty()).collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out = text.to_string();
    for s in ordered {
        out = out.replace(s.as_str(), "***");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker() -> CredentialBroker {
        CredentialBroker::new(Box::new(InMemoryStore::default()))
    }

    #[test]
    fn store_get_delete_round_trip() {
        let b = broker();
        let r = b.store_secret("github_token", "ghp_secret_value").unwrap();
        assert_eq!(r.name, "github_token");
        assert_eq!(r.keyring_ref, "agentrelay:github_token");
        // the ref never contains the secret
        assert!(!format!("{r:?}").contains("ghp_secret_value"));
        assert_eq!(b.get_secret("github_token").unwrap(), "ghp_secret_value");
        b.delete_secret("github_token").unwrap();
        assert_eq!(
            b.get_secret("github_token").unwrap_err().code,
            ErrorCode::CredentialUnavailable
        );
    }

    #[test]
    fn env_for_resolves_bindings() {
        let b = broker();
        b.store_secret("gh", "tok1").unwrap();
        b.store_secret("stripe", "tok2").unwrap();
        let env = b
            .env_for(&[
                ("GITHUB_TOKEN".into(), "gh".into()),
                ("STRIPE_KEY".into(), "stripe".into()),
            ])
            .unwrap();
        assert_eq!(env.get("GITHUB_TOKEN").map(String::as_str), Some("tok1"));
        assert_eq!(env.get("STRIPE_KEY").map(String::as_str), Some("tok2"));
        // a missing credential errors
        assert!(b.env_for(&[("X".into(), "nope".into())]).is_err());
    }

    #[test]
    fn redact_masks_secrets_longest_first() {
        let out = redact(
            "token=ghp_abc123 and short=abc",
            &["abc".into(), "ghp_abc123".into()],
        );
        assert!(!out.contains("ghp_abc123"));
        assert_eq!(out, "token=*** and short=***");
    }
}
