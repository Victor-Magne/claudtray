pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod http;

use crate::model::ProviderSnapshot;
use crate::state::AppState;

/// A monitored AI coding assistant. `collect` returns a snapshot with the raw
/// per-window usage filled in; for token-based providers the budget/percentage
/// are derived later by the monitor.
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn collect(&self, state: &AppState) -> ProviderSnapshot;
}

/// All providers, in display order.
pub fn all() -> Vec<Box<dyn Provider + Send + Sync>> {
    vec![
        Box::new(claude::ClaudeProvider),
        Box::new(antigravity::AntigravityProvider),
        Box::new(codex::CodexProvider),
        Box::new(copilot::CopilotProvider),
    ]
}

// ---- Shared helpers ----

use chrono::{DateTime, Duration, Local, Utc};
use std::path::{Path, PathBuf};

/// Convert an epoch-seconds reset timestamp to a local RFC3339 string.
pub fn reset_from_epoch(secs: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(secs, 0).map(|dt| dt.with_timezone(&Local).to_rfc3339())
}

/// True when the file was modified within the last `days` days.
pub fn modified_within(path: &Path, days: i64) -> bool {
    let cutoff = Utc::now() - Duration::days(days);
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(modified) => {
            let modified: DateTime<Utc> = modified.into();
            modified >= cutoff
        }
        Err(_) => false,
    }
}

/// Recursively collect `*.jsonl` files under `root` modified within `days`,
/// sorted newest-first. Depth-bounded.
pub fn newest_jsonl_files(root: &Path, days: i64) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    collect_jsonl(root, days, 0, &mut found);
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, p)| p).collect()
}

fn collect_jsonl(
    dir: &Path,
    days: i64,
    depth: usize,
    out: &mut Vec<(std::time::SystemTime, PathBuf)>,
) {
    const MAX_DEPTH: usize = 6;
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, days, depth + 1, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            && modified_within(&path, days)
        {
            if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                out.push((mtime, path));
            }
        }
    }
}
