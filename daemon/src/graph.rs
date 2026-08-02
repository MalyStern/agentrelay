//! Optional knowledge-graph reports, via graphify (an external tool).
//!
//! AgentRelay does not build the graph itself. It invokes `graphify` — a separately
//! licensed (Apache-2.0/MIT) tool the user installs — the same way it already invokes
//! git, node and docker. Two rules follow from the project's own principles:
//!
//! * **Never install anything behind the user's back.** If graphify is not on PATH the
//!   feature is simply off, and every surface (`status`, `doctor`, the report) says so
//!   out loud with the one command that would enable it. It is a heuristic-free,
//!   opt-in convenience, not a dependency.
//! * **Don't spend tokens without saying so.** `graphify update` builds a code graph
//!   from the AST alone — no LLM, no cost — and that is the default. `graphify extract`
//!   is richer but calls an LLM, so it is opt-in (`deep = true`) and labelled as
//!   spending money wherever it appears.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GraphConfig {
    /// After a run, build a knowledge graph of the workspace and link it in the report.
    #[serde(default)]
    pub after_run: bool,
    /// Use graphify's LLM extraction (richer edges, but SPENDS TOKENS) instead of the
    /// free AST-only `update`. Off by default — the free path costs nothing.
    #[serde(default)]
    pub deep: bool,
    /// Extra flags passed straight through to graphify (e.g. "--directed").
    #[serde(default)]
    pub flags: Vec<String>,
}

/// Is graphify installed and runnable right now?
pub fn graphify_available() -> bool {
    crate::which::is_available("graphify")
}

/// The one line to show a user who wants this feature but hasn't installed the tool.
pub fn install_hint() -> String {
    "install graphify with `uv tool install graphifyy` (or `pip install graphifyy`), \
     then reopen your terminal so PATH is refreshed"
        .to_string()
}

impl GraphConfig {
    pub fn describe(&self) -> String {
        self.describe_with(graphify_available())
    }

    /// Pure so both branches are testable regardless of what's installed on the box
    /// running the tests.
    pub fn describe_with(&self, available: bool) -> String {
        if !self.after_run {
            return "off — no knowledge graph is built".to_string();
        }
        let engine = if self.deep {
            "graphify extract (LLM — spends tokens)"
        } else {
            "graphify update (AST only — no token cost)"
        };
        if available {
            format!("on — {engine} of the workspace after each run, linked in the report")
        } else {
            format!(
                "REQUESTED but graphify is not installed, so nothing will be built — {}",
                install_hint()
            )
        }
    }
}

/// The graphify subcommand + arguments to build or refresh the graph for `workspace`.
/// `update` is the free, AST-only, LLM-free build; `extract` is the LLM one.
pub fn build_args(workspace: &str, cfg: &GraphConfig) -> Vec<String> {
    let mut a = vec![
        if cfg.deep { "extract" } else { "update" }.to_string(),
        workspace.to_string(),
    ];
    a.extend(cfg.flags.iter().cloned());
    a
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphOutcome {
    /// Did graphify actually run? False means it wasn't installed, or the run failed.
    pub ran: bool,
    pub graph_json: Option<String>,
    pub report_md: Option<String>,
    pub html: Option<String>,
    /// An honest, human line about what happened — provenance, or why nothing was built.
    pub note: String,
}

impl GraphOutcome {
    fn absent() -> Self {
        GraphOutcome {
            ran: false,
            graph_json: None,
            report_md: None,
            html: None,
            note: format!("graphify is not installed — {}", install_hint()),
        }
    }
}

/// Build (or refresh) the knowledge graph for `workspace` by invoking graphify.
///
/// Never installs graphify. If it isn't present, returns `ran: false` with a note that
/// says exactly how to enable it — the caller surfaces that verbatim, so the user is
/// never left thinking a graph exists when none does.
pub fn build_graph(workspace: &Path, cfg: &GraphConfig) -> GraphOutcome {
    if !graphify_available() {
        return GraphOutcome::absent();
    }
    let mut cmd = match crate::which::std_command_for("graphify") {
        Some(c) => c,
        None => return GraphOutcome::absent(),
    };
    cmd.args(build_args(&workspace.to_string_lossy(), cfg));
    // Capture output so an automated after-run build stays quiet; on failure the tail
    // goes into the note rather than being swallowed.
    let output = cmd.output();

    let out_dir = workspace.join("graphify-out");
    let found = |name: &str| -> Option<String> {
        let p = out_dir.join(name);
        if p.is_file() {
            Some(p.to_string_lossy().to_string())
        } else {
            None
        }
    };
    // graphify writes graph.html by default; fall back to any .html it produced.
    let html = found("graph.html").or_else(|| first_html(&out_dir));

    match output {
        Ok(o) if o.status.success() => {
            let graph_json = found("graph.json");
            let report_md = found("GRAPH_REPORT.md");
            if graph_json.is_some() {
                let engine = if cfg.deep {
                    "graphify extract (LLM-assisted — this run spent tokens)"
                } else {
                    "graphify update (AST only — no tokens spent)"
                };
                GraphOutcome {
                    ran: true,
                    graph_json,
                    report_md,
                    html,
                    note: format!("knowledge graph built by {engine}"),
                }
            } else {
                GraphOutcome {
                    ran: false,
                    graph_json: None,
                    report_md,
                    html,
                    note: "graphify ran but produced no graph.json — nothing to link".to_string(),
                }
            }
        }
        Ok(o) => {
            let tail = String::from_utf8_lossy(&o.stderr);
            let tail = tail.trim();
            let tail = tail.rsplit('\n').next().unwrap_or(tail);
            GraphOutcome {
                ran: false,
                graph_json: None,
                report_md: None,
                html: None,
                note: format!("graphify failed — {tail}"),
            }
        }
        Err(e) => GraphOutcome {
            ran: false,
            graph_json: None,
            report_md: None,
            html: None,
            note: format!("could not run graphify — {e}"),
        },
    }
}

fn first_html(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("html") {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_and_says_so() {
        let cfg = GraphConfig::default();
        assert!(!cfg.after_run);
        assert_eq!(cfg.describe_with(true), "off — no knowledge graph is built");
    }

    #[test]
    fn requested_but_missing_is_stated_loudly_not_silently_skipped() {
        let cfg = GraphConfig {
            after_run: true,
            ..Default::default()
        };
        let d = cfg.describe_with(false);
        assert!(d.contains("REQUESTED but graphify is not installed"), "{d}");
        assert!(d.contains("uv tool install graphifyy"), "{d}");
    }

    #[test]
    fn the_free_path_is_the_default_and_the_llm_path_is_marked_as_costing() {
        let free = GraphConfig {
            after_run: true,
            deep: false,
            ..Default::default()
        };
        assert!(
            free.describe_with(true).contains("no token cost"),
            "{}",
            free.describe_with(true)
        );
        assert_eq!(build_args("/w", &free), vec!["update", "/w"]);

        let deep = GraphConfig {
            after_run: true,
            deep: true,
            ..Default::default()
        };
        assert!(
            deep.describe_with(true).contains("spends tokens"),
            "{}",
            deep.describe_with(true)
        );
        assert_eq!(build_args("/w", &deep), vec!["extract", "/w"]);
    }

    #[test]
    fn extra_flags_pass_through_in_order() {
        let cfg = GraphConfig {
            after_run: true,
            flags: vec!["--directed".into()],
            ..Default::default()
        };
        assert_eq!(build_args("/w", &cfg), vec!["update", "/w", "--directed"]);
    }

    #[test]
    fn a_missing_graphify_yields_an_honest_outcome_never_a_false_graph() {
        // Force the absent path deterministically (works on CI, where graphify isn't
        // installed, and locally, where it is) by asserting on the absent() shape.
        let o = GraphOutcome::absent();
        assert!(!o.ran);
        assert!(o.graph_json.is_none());
        assert!(o.note.contains("not installed"), "{}", o.note);
    }

    // A real graphify build. Ignored in CI (graphify isn't installed there); run
    // locally with `cargo test -- --ignored` on a machine that has `graphify` on PATH.
    #[test]
    #[ignore = "needs graphify on PATH; run locally with --ignored"]
    fn a_real_build_produces_a_graph_json() {
        if !graphify_available() {
            eprintln!("skipping: graphify not on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ar-graph-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mod.js"),
            "export function add(a,b){return a+b}\nexport function sub(a,b){return a-b}\n",
        )
        .unwrap();
        let out = build_graph(
            &dir,
            &GraphConfig {
                after_run: true,
                ..Default::default()
            },
        );
        assert!(out.ran, "note: {}", out.note);
        assert!(out.graph_json.is_some(), "note: {}", out.note);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
