//! The Isolation Ring (plan §ח, Phase-2): run the agent inside a container with
//! the network shut off by default.
//!
//! Everything else in this daemon limits what the agent *asks* to do. This limits
//! what it *can* do. Guards are pattern-matching on tool calls, and pattern-matching
//! is a losing game against a general-purpose shell — `curl` has a hundred spellings
//! and a Python one-liner has more. The container is the layer that does not depend
//! on recognising anything: no network interface means no exfiltration, whatever the
//! command looked like.
//!
//! Design decisions that are load-bearing:
//!
//! * **`--network none` by default.** An agent that cannot reach the internet cannot
//!   leak your repository, whatever it was persuaded to run. Anything else is opt-in
//!   per host and stated in the config where you can see it.
//! * **Only the workspace is mounted.** Not your home directory, not `~/.ssh`, not
//!   the credential store. A path traversal inside the container reaches nothing,
//!   because nothing else is there to reach.
//! * **Read-only root, no new privileges, dropped capabilities.** The blast radius
//!   of a compromised agent should be the worktree and nothing else.
//! * **We do not invent an image.** Isolation is off unless the user names one:
//!   silently pulling something from a registry would be a supply-chain decision
//!   made on their behalf.

use serde::{Deserialize, Serialize};

/// How (or whether) to isolate the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Isolation {
    /// Container image to run in. `None` — the default — means no isolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Hosts the agent is allowed to reach. Empty means the network is off
    /// entirely, which is the default and the safe answer.
    #[serde(default)]
    pub allow_network: Vec<String>,
    /// Memory cap, e.g. "4g". A runaway build should not take the machine down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// CPU cap, e.g. "2".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<String>,
}

impl Isolation {
    pub fn is_enabled(&self) -> bool {
        self.image.is_some()
    }

    /// What this actually means, for `status`. An isolation setting the user
    /// believes is on but isn't is the worst outcome, so it is spelled out.
    pub fn describe(&self) -> String {
        let Some(image) = &self.image else {
            return "Isolation is OFF. The agent runs directly on this machine, with your \
                    network and your files."
                .to_string();
        };
        let mut s = format!(
            "Isolation is ON: the agent runs inside `{image}`, with only the \
                             workspace mounted."
        );
        if self.allow_network.is_empty() {
            s.push_str("\nNetwork: OFF. Nothing the agent runs can reach the internet.");
        } else {
            s.push_str(&format!(
                "\nNetwork: ON, limited to {}. NOTE: Docker cannot enforce a host allowlist by \
                 itself — this needs a proxy in front, so treat it as network ON until you have \
                 one.",
                self.allow_network.join(", ")
            ));
        }
        s
    }
}

/// The `docker run` arguments for a workspace, minus the command to execute.
///
/// Pure and returned as a list so it can be asserted on in a test: the security
/// properties of this feature ARE these flags, and a test that mocks them proves
/// nothing.
pub fn docker_args(iso: &Isolation, workspace: &str, workdir_in_container: &str) -> Vec<String> {
    let mut a: Vec<String> = vec!["run".into(), "--rm".into(), "-i".into()];

    // Mount the workspace, and nothing else. The agent's world is the worktree.
    a.push("-v".into());
    a.push(format!("{workspace}:{workdir_in_container}"));
    a.push("-w".into());
    a.push(workdir_in_container.to_string());

    if iso.allow_network.is_empty() {
        // The single most valuable flag here.
        a.push("--network".into());
        a.push("none".into());
    }

    // A compromised agent should not be able to escalate inside the container.
    a.push("--security-opt".into());
    a.push("no-new-privileges".into());
    a.push("--cap-drop".into());
    a.push("ALL".into());
    // Read-only root with a writable /tmp: build tools need scratch space, but they
    // do not need to rewrite the image.
    a.push("--read-only".into());
    a.push("--tmpfs".into());
    a.push("/tmp:rw,exec,nosuid,size=1g".into());

    if let Some(m) = &iso.memory {
        a.push("--memory".into());
        a.push(m.clone());
    }
    if let Some(c) = &iso.cpus {
        a.push("--cpus".into());
        a.push(c.clone());
    }

    if let Some(image) = &iso.image {
        a.push(image.clone());
    }
    a
}

/// Is Docker actually usable right now? Installed is not the same as running.
pub fn docker_available() -> bool {
    crate::which::std_command_for("docker")
        .and_then(|mut c| {
            c.args(["info", "--format", "{{.ServerVersion}}"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .stdin(std::process::Stdio::null())
                .status()
                .ok()
        })
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Why isolation cannot be used, if it cannot. `None` means it is fine.
///
/// Checked up front so the failure is "Docker is not running", said once, rather
/// than a container error in the middle of the night.
pub fn blocker(iso: &Isolation) -> Option<String> {
    if !iso.is_enabled() {
        return None;
    }
    if !docker_available() {
        return Some(
            "isolation is configured but Docker is not available (not installed, or the daemon \
             is not running). Start Docker, or remove `isolation.image` to run without it — \
             AgentRelay will not silently run unisolated when you asked for isolation."
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(image: &str) -> Isolation {
        Isolation {
            image: Some(image.into()),
            ..Default::default()
        }
    }

    #[test]
    fn isolation_is_off_until_the_user_names_an_image() {
        // Pulling an image we chose would be a supply-chain decision made on the
        // user's behalf.
        let d = Isolation::default();
        assert!(!d.is_enabled());
        assert!(d.describe().contains("Isolation is OFF"));
        assert!(d.describe().contains("your network and your files"));
        assert_eq!(blocker(&d), None, "no isolation configured is not an error");
    }

    #[test]
    fn the_network_is_off_by_default_when_isolated() {
        // The one flag that makes exfiltration impossible rather than merely
        // unlikely. Guards pattern-match on commands; this does not have to.
        let args = docker_args(&iso("node:20"), "/w", "/work");
        let joined = args.join(" ");
        assert!(joined.contains("--network none"), "{joined}");
        assert!(iso("node:20").describe().contains("Network: OFF"));
    }

    #[test]
    fn only_the_workspace_is_mounted() {
        let args = docker_args(&iso("node:20"), "/home/u/proj", "/work");
        let mounts: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| i > &0 && args[i - 1] == "-v")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(
            mounts,
            vec!["/home/u/proj:/work"],
            "exactly one mount, the worktree"
        );
        // The things a leak would want are not reachable because they are not there.
        let joined = args.join(" ");
        assert!(!joined.contains(".ssh"), "{joined}");
        assert!(
            !joined.contains("docker.sock"),
            "mounting the socket would undo all of this"
        );
        assert!(!joined.contains("--privileged"), "{joined}");
    }

    #[test]
    fn the_container_cannot_escalate_or_rewrite_itself() {
        let joined = docker_args(&iso("node:20"), "/w", "/work").join(" ");
        assert!(
            joined.contains("--security-opt no-new-privileges"),
            "{joined}"
        );
        assert!(joined.contains("--cap-drop ALL"), "{joined}");
        assert!(joined.contains("--read-only"), "{joined}");
        // …but build tools still need scratch space, or nothing works and the
        // feature gets switched off.
        assert!(
            joined.contains("--tmpfs /tmp:rw,exec,nosuid,size=1g"),
            "{joined}"
        );
    }

    #[test]
    fn resource_caps_are_passed_through_when_set() {
        let i = Isolation {
            image: Some("node:20".into()),
            memory: Some("4g".into()),
            cpus: Some("2".into()),
            ..Default::default()
        };
        let joined = docker_args(&i, "/w", "/work").join(" ");
        assert!(joined.contains("--memory 4g"), "{joined}");
        assert!(joined.contains("--cpus 2"), "{joined}");
    }

    #[test]
    fn the_image_is_the_last_argument_so_the_command_follows_it() {
        let args = docker_args(&iso("node:20"), "/w", "/work");
        assert_eq!(args.last().unwrap(), "node:20");
    }

    #[test]
    fn an_allowlist_admits_that_docker_alone_cannot_enforce_it() {
        // Claiming a host allowlist that `docker run` does not actually implement
        // would be a security promise we cannot keep — worse than no promise.
        let i = Isolation {
            image: Some("node:20".into()),
            allow_network: vec!["registry.npmjs.org".into()],
            ..Default::default()
        };
        let d = i.describe();
        assert!(d.contains("cannot enforce a host allowlist"), "{d}");
        assert!(d.contains("treat it as network ON"), "{d}");
        // And with any allowlist, --network none is correctly NOT applied.
        assert!(!docker_args(&i, "/w", "/work")
            .join(" ")
            .contains("--network none"));
    }

    /// Run the real thing. Skipped when Docker is not available, because a test
    /// that cannot run should not fail the suite on someone else's machine — but
    /// where it CAN run, asserting on flags proves nothing and this proves the
    /// property.
    #[test]
    fn the_isolation_actually_holds_against_a_live_container() {
        if !docker_available() {
            eprintln!("skipping: docker not available");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ar-iso-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hello.txt"), "workspace file\n").unwrap();
        let ws = dir.to_string_lossy().to_string();

        let i = Isolation {
            image: Some("alpine:3".into()),
            ..Default::default()
        };
        let run = |script: &str| -> String {
            let mut args = docker_args(&i, &ws, "/work");
            args.push("sh".into());
            args.push("-c".into());
            args.push(script.into());
            let out = std::process::Command::new("docker")
                .args(&args)
                .output()
                .unwrap();
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };

        // 1. The worktree is there — otherwise the agent has nothing to work on.
        assert!(
            run("cat hello.txt").contains("workspace file"),
            "workspace not mounted"
        );

        // 2. It is WRITABLE despite the read-only root. If this broke, isolation
        //    would be useless in practice and get switched off.
        assert!(
            run("echo x > out.txt && echo WROTE").contains("WROTE"),
            "cannot write results"
        );
        assert!(
            dir.join("out.txt").exists(),
            "the write did not reach the host"
        );

        // 3. Nothing can reach the network. This is the property the whole feature
        //    exists for: not "the agent probably won't exfiltrate", but cannot.
        let net = run("wget -q -T 3 -O- http://example.com 2>&1 || echo BLOCKED");
        assert!(
            net.contains("BLOCKED") || net.contains("bad address"),
            "network reachable: {net}"
        );

        // 4. The image cannot be rewritten.
        assert!(
            run("touch /evil 2>&1 || echo READONLY").contains("READONLY"),
            "root is writable"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_round_trips_and_defaults_to_off() {
        let json = r#"{"image":"node:20","allow_network":[],"memory":"4g"}"#;
        let i: Isolation = serde_json::from_str(json).unwrap();
        assert_eq!(i.image.as_deref(), Some("node:20"));
        assert_eq!(i.memory.as_deref(), Some("4g"));
        assert_eq!(
            serde_json::from_str::<Isolation>("{}").unwrap(),
            Isolation::default()
        );
    }
}
