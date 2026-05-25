//! Progress reporting abstraction for long-running CLI commands.
//!
//! Two impls today:
//! - `Tty` — multi-progress bars + spinner (indicatif), for terminals.
//! - `Json` — one JSONL event per phase, for piped stdout / --json.
//!
//! Spec: §2.3 of docs/superpowers/specs/2026-05-25-ingest-ux-design.md

use serde_json::json;

/// Progress reporting interface for long-running CLI phases.
pub trait Reporter: Send {
    /// A phase started; payload is structured data for the JSON impl.
    fn phase(&mut self, name: &str, payload: serde_json::Value);
    /// A phase completed.
    fn phase_done(&mut self, name: &str, payload: serde_json::Value);
    /// Long-running phase progress (current, total, label).
    fn batch(&mut self, current: usize, total: usize, label: &str);
}

/// JSON-line reporter — emits one JSONL event per phase to stdout.
pub struct Json;

impl Reporter for Json {
    fn phase(&mut self, name: &str, payload: serde_json::Value) {
        let mut obj = serde_json::Map::new();
        obj.insert("phase".to_owned(), json!(name));
        if let serde_json::Value::Object(m) = payload {
            obj.extend(m);
        }
        println!("{}", serde_json::Value::Object(obj));
    }
    fn phase_done(&mut self, name: &str, payload: serde_json::Value) {
        self.phase(name, payload);
    }
    fn batch(&mut self, current: usize, total: usize, label: &str) {
        self.phase(label, json!({"current": current, "of": total}));
    }
}

/// TTY reporter — renders multi-progress bars + spinner via indicatif.
pub struct Tty {
    mp: indicatif::MultiProgress,
    bar: Option<indicatif::ProgressBar>,
}

impl Tty {
    /// Create a new [`Tty`] reporter.
    #[must_use]
    pub fn new() -> Self {
        Self {
            mp: indicatif::MultiProgress::new(),
            bar: None,
        }
    }
}

impl Default for Tty {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for Tty {
    fn phase(&mut self, name: &str, _payload: serde_json::Value) {
        let pb = self.mp.add(indicatif::ProgressBar::new_spinner());
        pb.set_message(name.to_owned());
        pb.enable_steady_tick(std::time::Duration::from_millis(120));
        self.bar = Some(pb);
    }
    fn phase_done(&mut self, name: &str, payload: serde_json::Value) {
        if let Some(pb) = self.bar.take() {
            let summary = format_summary(name, &payload);
            pb.finish_with_message(format!("✓ {summary}"));
        }
    }
    fn batch(&mut self, current: usize, total: usize, label: &str) {
        if let Some(pb) = &self.bar {
            pb.set_message(format!("{label}: batch {current}/{total}"));
        }
    }
}

fn format_summary(name: &str, payload: &serde_json::Value) -> String {
    let detail = payload
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if detail.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {detail}")
    }
}

/// Pick the right reporter based on `--json` and TTY detection.
#[must_use]
pub fn pick(json: bool) -> Box<dyn Reporter> {
    use std::io::IsTerminal as _;
    if json || !std::io::stdout().is_terminal() {
        Box::new(Json)
    } else {
        Box::new(Tty::new())
    }
}
