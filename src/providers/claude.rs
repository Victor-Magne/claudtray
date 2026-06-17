use super::{modified_within, reset_at, window_sum, Provider, UsageRecord};
use crate::model::{ProviderSnapshot, WindowUsage};
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;

const SESSION_HOURS: i64 = 5;
const WEEK_DAYS: i64 = 7;

pub struct ClaudeProvider;

// --- JSONL parsing structs (Claude Code transcript format) ---

#[derive(Deserialize)]
struct JournalEntry {
    #[serde(rename = "type")]
    entry_type: Option<String>,
    message: Option<JournalMessage>,
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct JournalMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct TokenUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn name(&self) -> &'static str {
        "Claude"
    }

    fn collect(&self, _state: &AppState) -> ProviderSnapshot {
        let projects_dir = match dirs::home_dir() {
            Some(h) => h.join(".claude").join("projects"),
            None => {
                return ProviderSnapshot::unavailable(self.id(), self.name(), "Não detetado")
            }
        };

        if !projects_dir.exists() {
            return ProviderSnapshot::unavailable(
                self.id(),
                self.name(),
                "Claude Code não detetado",
            );
        }

        let records = collect_records(&projects_dir);
        let now = Utc::now();
        let session_cutoff = now - Duration::hours(SESSION_HOURS);
        let weekly_cutoff = now - Duration::days(WEEK_DAYS);

        let (session_used, session_first) = window_sum(&records, session_cutoff, |_| true);
        let (weekly_used, weekly_first) = window_sum(&records, weekly_cutoff, |_| true);
        let (opus_used, opus_first) = window_sum(&records, weekly_cutoff, |r| r.is_opus);

        let windows = vec![
            WindowUsage::raw(
                "session",
                "SESSION",
                session_used,
                reset_at(session_first, Duration::hours(SESSION_HOURS)),
            ),
            WindowUsage::raw(
                "weekly",
                "WEEKLY",
                weekly_used,
                reset_at(weekly_first, Duration::days(WEEK_DAYS)),
            ),
            WindowUsage::raw(
                "opus",
                "OPUS",
                opus_used,
                reset_at(opus_first, Duration::days(WEEK_DAYS)),
            ),
        ];

        ProviderSnapshot {
            id: self.id().to_string(),
            name: self.name().to_string(),
            available: true,
            note: None,
            windows,
        }
    }
}

/// Scan every project transcript modified in the last week and extract the
/// per-message usage records, de-duplicated by message id.
fn collect_records(projects_dir: &std::path::Path) -> Vec<UsageRecord> {
    let mut records = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    let Ok(projects) = fs::read_dir(projects_dir) else {
        return records;
    };

    for project_entry in projects.flatten() {
        let Ok(files) = fs::read_dir(project_entry.path()) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if !modified_within(&path, WEEK_DAYS) {
                continue;
            }
            parse_file(&path, &mut seen_ids, &mut records);
        }
    }

    records
}

fn parse_file(
    path: &std::path::Path,
    seen_ids: &mut HashSet<String>,
    records: &mut Vec<UsageRecord>,
) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };

    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<JournalEntry>(line) else {
            continue;
        };
        if entry.entry_type.as_deref() != Some("assistant") {
            continue;
        }

        let Some(ts) = entry
            .timestamp
            .as_ref()
            .and_then(|ts| ts.parse::<DateTime<Utc>>().ok())
        else {
            continue;
        };

        let Some(msg) = &entry.message else {
            continue;
        };

        // De-duplicate by message id (same id can appear across iteration lines).
        if let Some(id) = &msg.id {
            if !seen_ids.insert(id.clone()) {
                continue;
            }
        }

        let Some(usage) = &msg.usage else {
            continue;
        };
        let tokens = usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0);
        if tokens == 0 {
            continue;
        }

        let is_opus = msg
            .model
            .as_deref()
            .map(|m| m.to_ascii_lowercase().contains("opus"))
            .unwrap_or(false);

        records.push(UsageRecord { ts, tokens, is_opus });
    }
}
