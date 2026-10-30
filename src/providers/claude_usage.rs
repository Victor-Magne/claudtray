//! Claude Code usage (tokens, cost, sessions, models) aggregated from the JSONL
//! transcripts in `~/.claude/projects`.
//!
//! One pass over every transcript feeds the whole dashboard: today / week /
//! month totals, tokens per weekday, tokens per model, session and message
//! counts, and the date of the first record. Re-reading ~hundreds of MB on every
//! refresh is out of the question, so each file's parsed entries are cached by
//! (path, mtime, size) in memory and mirrored to disk: a relaunch only re-reads
//! what changed since.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, UNIX_EPOCH};

/// Bump when `Entry`'s meaning changes so a stale on-disk cache is discarded.
const CACHE_VERSION: u32 = 2;
/// Don't rewrite the on-disk cache more often than this (the active transcript
/// changes on every refresh).
const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Totals {
    pub tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelTokens {
    pub model: String,
    pub tokens: u64,
}

/// Everything the Claude page needs besides the quota windows.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageStats {
    pub today: Totals,
    /// Calendar week, Monday to today.
    pub week: Totals,
    /// Last 30 days, today included.
    pub month: Totals,
    /// Tokens per weekday, Monday first (days after today are 0).
    pub daily: [u64; 7],
    /// Index into `daily` of today (0 = Monday).
    pub today_index: u8,
    /// Tokens per model family this week, largest first.
    pub models_week: Vec<ModelTokens>,
    /// Distinct sessions / assistant messages over the last 7 days.
    pub sessions_7d: u32,
    pub msgs_7d: u32,
    /// Date of the first record (`YYYY-MM-DD`) and totals over all history.
    pub since: Option<String>,
    pub sessions_total: u32,
    pub msgs_total: u32,
}

/// One assistant message's usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// `message.id:requestId`, empty when either is missing. Claude Code writes
    /// the same message several times (streaming chunks, resumed sessions), so
    /// this is what keeps totals from running above Claude's own figures.
    key: String,
    session: String,
    model: String,
    /// Local calendar day, `YYYY-MM-DD`.
    day: String,
    tokens: u64,
    cost: f64,
}

#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize, Clone, Copy, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

/// USD per million tokens.
struct Price {
    input: f64,
    output: f64,
    cache_read: f64,
}

/// Anthropic's published API prices by model id (checked 2026-10-30). Cache
/// reads are not a flat share of input across generations (Opus 5.5 reads at
/// 5%, Sonnet 5.5 at 10%), so each row carries its own. Cache writes use the
/// standard 1.25× input. Estimate only: the id is matched by substring, and
/// long-context or fast-mode premiums are ignored.
fn model_prices(model: &str) -> Price {
    let m = model.to_ascii_lowercase();
    let p = |input, output, cache_read| Price { input, output, cache_read };
    if m.contains("fable") || m.contains("mythos") {
        p(10.0, 50.0, 0.25)
    } else if m.contains("opus-5-5") {
        p(4.0, 20.0, 0.20)
    } else if m.contains("opus") {
        // Opus 4.1 and older were $15 / $75; 4.5 onwards is $5 / $25.
        if m.contains("opus-4-1") || m.contains("3-opus") || m.contains("opus-4-2025") {
            p(15.0, 75.0, 1.50)
        } else {
            p(5.0, 25.0, 0.50)
        }
    } else if m.contains("sonnet-5") {
        p(2.0, 10.0, 0.20)
    } else if m.contains("sonnet") {
        p(3.0, 15.0, 0.30)
    } else if m.contains("haiku-5") {
        p(0.10, 0.50, 0.01)
    } else if m.contains("haiku-4") {
        p(1.0, 5.0, 0.10)
    } else if m.contains("haiku") {
        p(0.80, 4.0, 0.08)
    } else {
        p(3.0, 15.0, 0.30) // unknown model: sonnet 4 tier, as before
    }
}

/// "claude-opus-4" → "Opus"; anything unrecognised keeps its raw name.
fn family(model: &str) -> String {
    let m = model.to_ascii_lowercase();
    for (needle, name) in [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")] {
        if m.contains(needle) {
            return name.to_string();
        }
    }
    model.to_string()
}

fn parse_line(line: &str, file_session: &str) -> Option<Entry> {
    // Cheap pre-filter: most lines (user turns, attachments) carry no usage.
    if !line.contains("\"usage\"") {
        return None;
    }
    let l: Line = serde_json::from_str(line).ok()?;
    let msg = l.message.as_ref();
    let usage = msg.and_then(|m| m.usage).or(l.usage)?;
    let model = msg
        .and_then(|m| m.model.clone())
        .or(l.model)
        .unwrap_or_else(|| "claude-sonnet".to_string());
    if model.starts_with('<') {
        return None; // "<synthetic>": Claude Code's own placeholder, no real usage
    }

    let (inp, out) = (usage.input_tokens, usage.output_tokens);
    let (cache_c, cache_r) = (usage.cache_creation_input_tokens, usage.cache_read_input_tokens);
    let tokens = inp + out + cache_c + cache_r;
    if tokens == 0 {
        return None;
    }
    let price = model_prices(&model);
    let cost = (inp as f64 * price.input
        + out as f64 * price.output
        + cache_c as f64 * price.input * 1.25 // cache write: 1.25× input
        + cache_r as f64 * price.cache_read)
        / 1_000_000.0;

    let day = DateTime::parse_from_rfc3339(l.timestamp.as_deref()?)
        .ok()?
        .with_timezone(&Local)
        .format("%Y-%m-%d")
        .to_string();
    let key = match (msg.and_then(|m| m.id.as_deref()), l.request_id.as_deref()) {
        (Some(m), Some(r)) => format!("{m}:{r}"),
        _ => String::new(),
    };
    Some(Entry {
        key,
        session: l.session_id.unwrap_or_else(|| file_session.to_string()),
        model,
        day,
        tokens,
        cost,
    })
}

/// Collapse repeated writes of the same message (the last one wins: streaming
/// chunks grow, so it carries the final counts).
fn collapse(raw: impl Iterator<Item = Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for e in raw {
        if e.key.is_empty() {
            out.push(e);
        } else if let Some(&i) = by_key.get(&e.key) {
            out[i] = e;
        } else {
            by_key.insert(e.key.clone(), out.len());
            out.push(e);
        }
    }
    out
}

fn parse_file(path: &Path) -> Vec<Entry> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    collapse(
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| parse_line(&line, &stem)),
    )
}

#[derive(Serialize, Deserialize)]
struct CachedFile {
    mtime: u64,
    size: u64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    version: u32,
    files: HashMap<String, CachedFile>,
}

struct Store {
    cache: Cache,
    last_save: Option<Instant>,
    dirty: bool,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);

fn cache_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("ClaudTray").join("claude_usage.json"))
}

fn load_cache() -> Cache {
    cache_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
        .filter(|c| c.version == CACHE_VERSION)
        .unwrap_or_else(|| Cache {
            version: CACHE_VERSION,
            files: HashMap::new(),
        })
}

fn save_cache(cache: &Cache) {
    let Some(path) = cache_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(cache) {
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn file_meta(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some((mtime, m.len()))
}

/// Bring `cache` in line with the transcripts under `root`: re-parse files whose
/// (mtime, size) changed, forget vanished ones. Returns whether anything moved.
fn refresh_cache(cache: &mut Cache, root: &Path) -> bool {
    let mut changed = false;
    let mut seen: HashSet<String> = HashSet::new();
    // Every transcript, however old — "Since" needs the whole history.
    for path in super::newest_jsonl_files(root, 36_500) {
        let Some((mtime, size)) = file_meta(&path) else { continue };
        let name = path.to_string_lossy().into_owned();
        seen.insert(name.clone());
        let fresh = cache
            .files
            .get(&name)
            .is_some_and(|c| c.mtime == mtime && c.size == size);
        if !fresh {
            let entries = parse_file(&path);
            cache.files.insert(name, CachedFile { mtime, size, entries });
            changed = true;
        }
    }
    let before = cache.files.len();
    cache.files.retain(|k, _| seen.contains(k));
    changed || cache.files.len() != before
}

fn aggregate<'a>(entries: impl Iterator<Item = &'a Entry>, today: NaiveDate) -> UsageStats {
    let iso = |d: NaiveDate| d.format("%Y-%m-%d").to_string();
    let today_s = iso(today);
    let week_start = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let week_s = iso(week_start);
    let month_s = iso(today - Duration::days(29));
    let d7_s = iso(today - Duration::days(6));
    let week_days: Vec<String> = (0..7).map(|i| iso(week_start + Duration::days(i))).collect();

    let mut st = UsageStats {
        today_index: today.weekday().num_days_from_monday() as u8,
        ..Default::default()
    };
    let mut seen: HashSet<&str> = HashSet::new();
    let mut sessions_all: HashSet<&str> = HashSet::new();
    let mut sessions_7d: HashSet<&str> = HashSet::new();
    let mut models: HashMap<String, u64> = HashMap::new();

    for e in entries {
        if !e.key.is_empty() && !seen.insert(e.key.as_str()) {
            continue; // same message already counted in another transcript
        }
        let day = e.day.as_str();
        st.msgs_total += 1;
        sessions_all.insert(&e.session);
        if st.since.as_deref().is_none_or(|s| day < s) {
            st.since = Some(e.day.clone());
        }
        if day > today_s.as_str() {
            continue; // clock skew: ignore the future
        }
        if day >= d7_s.as_str() {
            st.msgs_7d += 1;
            sessions_7d.insert(&e.session);
        }
        if day >= month_s.as_str() {
            st.month.tokens += e.tokens;
            st.month.cost += e.cost;
        }
        if day >= week_s.as_str() {
            st.week.tokens += e.tokens;
            st.week.cost += e.cost;
            if let Some(i) = week_days.iter().position(|d| d == day) {
                st.daily[i] += e.tokens;
            }
            *models.entry(family(&e.model)).or_default() += e.tokens;
        }
        if day == today_s {
            st.today.tokens += e.tokens;
            st.today.cost += e.cost;
        }
    }
    st.sessions_total = sessions_all.len() as u32;
    st.sessions_7d = sessions_7d.len() as u32;
    let mut m: Vec<ModelTokens> = models
        .into_iter()
        .map(|(model, tokens)| ModelTokens { model, tokens })
        .collect();
    m.sort_by(|a, b| b.tokens.cmp(&a.tokens).then_with(|| a.model.cmp(&b.model)));
    st.models_week = m;
    st
}

/// Usage stats from `~/.claude/projects`, or `None` when there are no transcripts.
pub fn collect() -> Option<UsageStats> {
    let root = dirs::home_dir()?.join(".claude").join("projects");
    if !root.exists() {
        return None;
    }
    let mut guard = STORE.lock().ok()?;
    let store = guard.get_or_insert_with(|| Store {
        cache: load_cache(),
        last_save: None,
        dirty: false,
    });
    store.dirty |= refresh_cache(&mut store.cache, &root);
    if store.dirty && store.last_save.is_none_or(|t| t.elapsed() >= SAVE_INTERVAL) {
        save_cache(&store.cache);
        store.last_save = Some(Instant::now());
        store.dirty = false;
    }
    let stats = aggregate(
        store.cache.files.values().flat_map(|f| f.entries.iter()),
        Local::now().date_naive(),
    );
    (stats.msgs_total > 0).then_some(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/claude/usage_week.jsonl");

    fn raw() -> Vec<Entry> {
        FIXTURE
            .lines()
            .filter_map(|l| parse_line(l, "file-session"))
            .collect()
    }

    // 2026-10-30 is a Friday; the week started Monday 2026-10-26.
    fn friday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 30).unwrap()
    }

    #[test]
    fn parse_line_sums_every_token_bucket_and_skips_noise() {
        let e = raw();
        // fixture: 8 lines; the user turn, the <synthetic> message, the
        // zero-token one and the malformed one produce no entry.
        assert_eq!(e.len(), 4);
        let sonnet = e.iter().rfind(|e| e.key == "msg_a:req_a").unwrap();
        assert_eq!(sonnet.tokens, 1000 + 500 + 200 + 4000);
        assert!(sonnet.cost > 0.0);
        assert_eq!(sonnet.day, "2026-10-30");
    }

    #[test]
    fn aggregate_dedupes_and_buckets_by_day_model_and_session() {
        let e = collapse(raw().into_iter());
        assert_eq!(e.len(), 3, "msg_a's two writes collapse into one");
        let st = aggregate(e.iter(), friday());

        assert_eq!(st.today.tokens, 5700);
        assert_eq!(st.daily, [3000, 0, 0, 0, 5700, 0, 0]);
        assert_eq!(st.today_index, 4);
        assert_eq!(st.week.tokens, 8700);
        assert_eq!(st.models_week[0], ModelTokens { model: "Sonnet".into(), tokens: 5700 });
        assert_eq!(st.models_week[1], ModelTokens { model: "Opus".into(), tokens: 3000 });
        assert_eq!(st.models_week.len(), 2);
        assert_eq!(st.since.as_deref(), Some("2026-07-05"));
        assert_eq!(st.msgs_7d, 2);
        assert_eq!(st.sessions_7d, 2);
        assert_eq!(st.msgs_total, 3);
        assert_eq!(st.sessions_total, 3);
        // the old message is outside the 30-day month, the others inside
        assert_eq!(st.month.tokens, 8700);
    }

    #[test]
    fn prices_follow_the_model_generation() {
        let rd = |m| model_prices(m).cache_read;
        assert_eq!(model_prices("claude-opus-5-5").input, 4.0);
        assert_eq!(rd("claude-opus-5-5"), 0.20);
        assert_eq!(model_prices("claude-opus-4-8").output, 25.0);
        assert_eq!(model_prices("claude-opus-4-1-20250805").input, 15.0);
        assert_eq!(model_prices("claude-sonnet-5").output, 10.0);
        assert_eq!(model_prices("claude-sonnet-4-6").input, 3.0);
        assert_eq!(model_prices("claude-haiku-5-5").input, 0.10);
        assert_eq!(model_prices("claude-haiku-4-5").input, 1.0);
        assert_eq!(rd("claude-fable-5-1"), 0.25);
        assert_eq!(model_prices("something-else").input, 3.0);
    }

    #[test]
    fn aggregate_counts_a_message_repeated_across_files_once() {
        let e = collapse(raw().into_iter());
        let dup: Vec<Entry> = e.iter().chain(e.iter()).cloned().collect();
        assert_eq!(aggregate(dup.iter(), friday()).week.tokens, 8700);
    }

    #[test]
    fn parse_file_keeps_the_last_write_of_a_repeated_message() {
        let dir = std::env::temp_dir().join(format!("claudtray-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s1.jsonl");
        std::fs::write(&path, FIXTURE).unwrap();
        let e = parse_file(&path);
        let a: Vec<_> = e.iter().filter(|e| e.key == "msg_a:req_a").collect();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].tokens, 5700, "the final chunk, not the partial one");
        std::fs::remove_dir_all(&dir).ok();
    }
}
