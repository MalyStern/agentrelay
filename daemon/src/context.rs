//! Context Optimizer core (plan §7.1): file receipts and output compression.
//!
//! This is the piece that decides whether an overnight run reaches the quota in
//! two hours or eight. Two mechanisms, and the plan is emphatic about the line
//! between them:
//!
//! * **File receipts.** A hash of file content is *truth* — it says with certainty
//!   whether a file changed since the agent last read it. That is the only claim we
//!   make. We never substitute a summary for a file: a summary is model output, and
//!   presenting it as the file's contents is how an agent ends up confidently
//!   editing something that no longer says what it thinks it says. A receipt only
//!   ever answers "you already have this exact content" — the full read stays
//!   available and is what happens the moment the hash differs.
//!
//! * **Output compression.** Test and build output is mostly noise: a thousand
//!   passing lines around the four that failed. Compression keeps what carries the
//!   signal, and always states how much was removed, so the agent is never left
//!   believing it saw everything.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hash of file content. Truth, not an estimate — the one thing here we assert.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileReceipt {
    pub path: String,
    pub hash: String,
    pub bytes: usize,
    /// Turn number when this content was last put into the agent's context.
    pub turn: u32,
}

/// What the supervisor should do about a read the agent asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadDecision {
    /// Never seen, or the content changed — send the file.
    Send { receipt: FileReceipt, changed: bool },
    /// Byte-identical to what the agent was already given. Skipping is safe
    /// *because the hash matched*, not because anything was summarised.
    AlreadyHave { receipt: FileReceipt, saved_bytes: usize },
}

impl ReadDecision {
    pub fn receipt(&self) -> &FileReceipt {
        match self {
            ReadDecision::Send { receipt, .. } => receipt,
            ReadDecision::AlreadyHave { receipt, .. } => receipt,
        }
    }
    pub fn should_send(&self) -> bool {
        matches!(self, ReadDecision::Send { .. })
    }
}

/// Tracks what content the agent already holds, per file, within one session.
#[derive(Debug, Default)]
pub struct ReceiptBook {
    seen: std::collections::HashMap<String, FileReceipt>,
    pub saved_bytes: usize,
}

impl ReceiptBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load receipts carried over from an earlier session (a resume).
    pub fn with_receipts(receipts: impl IntoIterator<Item = FileReceipt>) -> Self {
        let mut book = Self::new();
        for r in receipts {
            book.seen.insert(r.path.clone(), r);
        }
        book
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
    pub fn receipts(&self) -> impl Iterator<Item = &FileReceipt> {
        self.seen.values()
    }

    /// Decide whether this content needs to go into context, and record it.
    pub fn observe(&mut self, path: &str, content: &[u8], turn: u32) -> ReadDecision {
        let hash = content_hash(content);
        let receipt = FileReceipt {
            path: path.to_string(),
            hash: hash.clone(),
            bytes: content.len(),
            turn,
        };

        match self.seen.get(path) {
            Some(prev) if prev.hash == hash => {
                self.saved_bytes += content.len();
                let mut r = receipt;
                // Keep the turn it was ORIGINALLY sent on: that is the turn whose
                // context actually holds the bytes. Overwriting it would make a
                // later compaction think the content is fresher than it is.
                r.turn = prev.turn;
                self.seen.insert(path.to_string(), r.clone());
                ReadDecision::AlreadyHave { receipt: r, saved_bytes: content.len() }
            }
            prev => {
                let changed = prev.is_some();
                self.seen.insert(path.to_string(), receipt.clone());
                ReadDecision::Send { receipt, changed }
            }
        }
    }

    /// The line to hand the agent in place of a re-read. It states a fact about
    /// bytes, never a claim about meaning.
    pub fn skip_notice(r: &FileReceipt) -> String {
        format!(
            "[agentrelay] {} is byte-for-byte identical to the copy already in this \
             conversation (sha256 {}…, {} bytes, read on turn {}). Use that copy. \
             If you need it again verbatim, say so and it will be re-sent.",
            r.path,
            &r.hash[..12],
            r.bytes,
            r.turn
        )
    }
}

// ── Output compression ───────────────────────────────────────────────────────

/// Lines worth keeping even when everything around them is cut.
const SIGNAL: &[&str] = &[
    "error", "Error", "ERROR", "fail", "Fail", "FAIL", "panic", "assert",
    "✕", "✗", "×", "not ok", "Expected", "Received", "warning:", "cannot", "Cannot",
];

fn is_signal(line: &str) -> bool {
    SIGNAL.iter().any(|k| line.contains(k))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compressed {
    pub text: String,
    pub original_lines: usize,
    pub kept_lines: usize,
    pub original_bytes: usize,
}

impl Compressed {
    pub fn saved_bytes(&self) -> usize {
        self.original_bytes.saturating_sub(self.text.len())
    }
}

/// Compress command output while keeping every line that looks like a failure.
///
/// `head`/`tail` are how many lines of context to keep at each end — the start
/// says what ran, the end usually carries the summary. Everything between is kept
/// only if it carries signal.
///
/// The result always ends with a line saying what was removed. An agent that is
/// silently handed a truncated log will reason as though the missing lines said
/// nothing, which is exactly the failure this is supposed to prevent.
pub fn compress_output(raw: &str, head: usize, tail: usize, max_signal: usize) -> Compressed {
    let lines: Vec<&str> = raw.lines().collect();
    let total = lines.len();
    let original_bytes = raw.len();

    if total <= head + tail {
        return Compressed { text: raw.to_string(), original_lines: total, kept_lines: total, original_bytes };
    }

    let mut keep = vec![false; total];
    for i in 0..head.min(total) {
        keep[i] = true;
    }
    for i in total.saturating_sub(tail)..total {
        keep[i] = true;
    }

    let mut signal_kept = 0;
    let mut signal_dropped = 0;
    for (i, line) in lines.iter().enumerate() {
        if keep[i] || !is_signal(line) {
            continue;
        }
        if signal_kept < max_signal {
            keep[i] = true;
            signal_kept += 1;
            // One line either side of a failure is usually what makes it readable.
            if i > 0 {
                keep[i - 1] = true;
            }
            if i + 1 < total {
                keep[i + 1] = true;
            }
        } else {
            signal_dropped += 1;
        }
    }

    let mut out = String::new();
    let mut kept_lines = 0;
    let mut run_start: Option<usize> = None;
    for i in 0..total {
        if keep[i] {
            if let Some(start) = run_start.take() {
                let n = i - start;
                out.push_str(&format!("[agentrelay] … {n} line(s) omitted …\n"));
            }
            out.push_str(lines[i]);
            out.push('\n');
            kept_lines += 1;
        } else if run_start.is_none() {
            run_start = Some(i);
        }
    }
    if let Some(start) = run_start {
        out.push_str(&format!("[agentrelay] … {} line(s) omitted …\n", total - start));
    }

    let removed = total - kept_lines;
    out.push_str(&format!(
        "[agentrelay] compressed: {kept_lines} of {total} lines shown, {removed} removed"
    ));
    if signal_dropped > 0 {
        // Never let the agent believe it saw every failure when it did not.
        out.push_str(&format!(
            " — INCLUDING {signal_dropped} more line(s) that looked like failures. \
             This output was truncated; ask for the full log before concluding it passed"
        ));
    }
    out.push_str(". Full output is available on request.\n");

    Compressed { text: out, original_lines: total, kept_lines, original_bytes }
}

/// An *estimate* of context pressure. Named so nobody mistakes it for a
/// measurement: Claude headless gives no live counter, and a number presented as
/// exact would get trusted for decisions it cannot support (plan §7.1, §7.2).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContextHealthEstimated {
    pub estimated_tokens: u64,
    pub limit_tokens: u64,
    /// Always true. Serialised so any UI reading this has to see it.
    pub is_estimate: bool,
}

impl ContextHealthEstimated {
    /// ~4 bytes per token is the usual rough figure for English + code. It is a
    /// rule of thumb, which is precisely why the type says so.
    pub fn from_bytes(bytes: u64, limit_tokens: u64) -> Self {
        ContextHealthEstimated { estimated_tokens: bytes / 4, limit_tokens, is_estimate: true }
    }
    pub fn pressure(&self) -> f64 {
        if self.limit_tokens == 0 {
            return 0.0;
        }
        self.estimated_tokens as f64 / self.limit_tokens as f64
    }
    pub fn describe(&self) -> String {
        format!(
            "~{} of {} tokens ({:.0}% — ESTIMATED, not measured)",
            self.estimated_tokens,
            self.limit_tokens,
            self.pressure() * 100.0
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_content_is_recognised_by_hash_not_by_name() {
        let mut book = ReceiptBook::new();
        let d1 = book.observe("src/a.rs", b"fn main() {}", 1);
        assert!(d1.should_send(), "never seen before");

        let d2 = book.observe("src/a.rs", b"fn main() {}", 2);
        assert!(!d2.should_send(), "byte-identical: the agent already has it");
        match d2 {
            ReadDecision::AlreadyHave { saved_bytes, receipt } => {
                assert_eq!(saved_bytes, 12);
                assert_eq!(receipt.turn, 1, "the turn that actually holds the bytes");
            }
            _ => unreachable!(),
        }
        assert_eq!(book.saved_bytes, 12);
    }

    #[test]
    fn a_changed_file_is_always_re_sent() {
        // The whole safety argument: the hash decides, so an edited file can never
        // be skipped. Getting this wrong means an agent editing a file it misremembers.
        let mut book = ReceiptBook::new();
        book.observe("src/a.rs", b"fn main() {}", 1);
        let d = book.observe("src/a.rs", b"fn main() { changed(); }", 2);
        assert!(d.should_send());
        match d {
            ReadDecision::Send { changed, .. } => assert!(changed, "flagged as a change, not a first read"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn receipts_survive_a_resume() {
        let mut book = ReceiptBook::new();
        book.observe("a.rs", b"content", 3);
        let carried: Vec<FileReceipt> = book.receipts().cloned().collect();

        // New session, same file unchanged -> still no need to re-send it.
        let mut after = ReceiptBook::with_receipts(carried);
        assert!(!after.observe("a.rs", b"content", 9).should_send());
    }

    #[test]
    fn the_skip_notice_claims_only_what_the_hash_proves() {
        let mut book = ReceiptBook::new();
        book.observe("src/a.rs", b"x", 1);
        let d = book.observe("src/a.rs", b"x", 2);
        let notice = ReceiptBook::skip_notice(d.receipt());
        assert!(notice.contains("byte-for-byte identical"));
        assert!(notice.contains("re-sent"), "a full read must always be reachable");
        // It must never present a summary as if it were the file.
        assert!(!notice.to_lowercase().contains("summary"));
    }

    #[test]
    fn short_output_is_left_completely_alone() {
        let raw = "line 1\nline 2\nline 3\n";
        let c = compress_output(raw, 5, 5, 20);
        assert_eq!(c.text, raw, "nothing to gain, so change nothing");
        assert_eq!(c.kept_lines, c.original_lines);
    }

    #[test]
    fn failures_are_kept_even_when_buried_in_a_thousand_passes() {
        let mut raw = String::from("$ npm test\n");
        for i in 0..500 {
            raw.push_str(&format!("  ok {i} - some passing test\n"));
        }
        raw.push_str("  FAIL src/thing.test.ts > it works\n");
        raw.push_str("    Expected: 4\n    Received: 5\n");
        for i in 500..1000 {
            raw.push_str(&format!("  ok {i} - another passing test\n"));
        }
        raw.push_str("Tests: 1 failed, 1000 passed\n");

        let c = compress_output(&raw, 3, 3, 20);
        assert!(c.text.contains("FAIL src/thing.test.ts"), "the failure survives");
        assert!(c.text.contains("Expected: 4"), "and so does what it expected");
        assert!(c.text.contains("Tests: 1 failed"), "the summary line survives");
        assert!(c.kept_lines < 40, "but not the 1000 passes: kept {}", c.kept_lines);
        assert!(c.saved_bytes() > raw.len() / 2, "at least half the bytes saved");
        assert!(c.text.contains("omitted"), "and it says lines were removed");
    }

    #[test]
    fn truncating_past_the_signal_cap_says_so_loudly() {
        // The dangerous case: so many failures that we cannot show them all. The
        // agent must not read this and conclude the run was fine.
        let mut raw = String::new();
        for i in 0..100 {
            raw.push_str(&format!("FAIL test number {i}\n"));
        }
        let c = compress_output(&raw, 2, 2, 5);
        assert!(c.text.contains("INCLUDING"), "got: {}", c.text);
        assert!(c.text.contains("looked like failures"));
        assert!(c.text.contains("before concluding it passed"));
    }

    #[test]
    fn compression_always_admits_it_happened() {
        let raw: String = (0..200).map(|i| format!("noise line {i}\n")).collect();
        let c = compress_output(&raw, 2, 2, 10);
        assert!(c.text.contains("compressed:"));
        assert!(c.text.contains("Full output is available on request"));
        assert!(c.kept_lines < c.original_lines);
    }

    #[test]
    fn context_health_is_labelled_an_estimate_everywhere_it_appears() {
        // Claude headless has no live counter. A number that looks measured would
        // get trusted for decisions it cannot carry.
        let h = ContextHealthEstimated::from_bytes(400_000, 200_000);
        assert!(h.is_estimate);
        assert_eq!(h.estimated_tokens, 100_000);
        assert!((h.pressure() - 0.5).abs() < 1e-9);
        assert!(h.describe().contains("ESTIMATED"));
        assert!(serde_json::to_string(&h).unwrap().contains("\"is_estimate\":true"));
    }

    #[test]
    fn hashing_is_content_addressed() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
        assert_eq!(content_hash(b"abc").len(), 64);
    }
}
