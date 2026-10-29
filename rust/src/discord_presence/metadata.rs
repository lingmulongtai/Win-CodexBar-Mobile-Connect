//! Public-safe summaries of one local session; never return transcript content.
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    agent_sessions::AgentSessionProvider,
    core::{CostUsageDayRange, JsonlScanner},
};

#[cfg(test)]
const LARGE_SESSION_TEST_THRESHOLD: u64 = 16 * 1024 * 1024;
const MAX_CLAUDE_TRANSCRIPT_BYTES: u64 = 128 * 1024 * 1024;

struct ClaudeTokenCache {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
    tokens: Option<u64>,
}
static CLAUDE_TOKEN_CACHE: OnceLock<Mutex<Option<ClaudeTokenCache>>> = OnceLock::new();

fn claude_tokens(path: &Path) -> Option<u64> {
    let before = path.metadata().ok()?;
    let len = before.len();
    let modified = before.modified().ok()?;
    if len > MAX_CLAUDE_TRANSCRIPT_BYTES {
        return None;
    }
    let cache = CLAUDE_TOKEN_CACHE.get_or_init(|| Mutex::new(None));
    {
        let entry = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entry
            .as_ref()
            .filter(|e| e.path == path && e.len == len && e.modified == modified)
        {
            return entry.tokens;
        }
    }
    let tokens = crate::cost_scanner::claude_presence_tokens(path);
    let after = path.metadata().ok()?;
    if after.len() != len || after.modified().ok()? != modified {
        return None;
    }
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(ClaudeTokenCache {
        path: path.to_path_buf(),
        len,
        modified,
        tokens,
    });
    tokens
}
const METADATA_PREFIX_BYTES: u64 = 256 * 1024;
const METADATA_TAIL_BYTES: u64 = 512 * 1024;
const METADATA_SEARCH_BYTES: u64 = 32 * 1024 * 1024;

// Expand backwards only until a complete relevant record is found. Keep a
// fixed end offset so concurrent appends cannot change the inspected window.
fn metadata_tail(
    file: &mut File,
    size: u64,
    max_bytes: u64,
    relevant: impl Fn(&Value) -> bool,
) -> Option<(Vec<u8>, bool)> {
    let mut width = METADATA_TAIL_BYTES.min(size);
    loop {
        let offset = size.saturating_sub(width);
        let partial_first = if offset == 0 {
            false
        } else {
            file.seek(SeekFrom::Start(offset - 1)).ok()?;
            let mut previous = [0_u8; 1];
            file.read_exact(&mut previous).ok()?;
            previous[0] != b'\n'
        };
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut bytes = Vec::new();
        (&mut *file).take(width).read_to_end(&mut bytes).ok()?;
        if u64::try_from(bytes.len()).ok()? != width {
            return None;
        }
        let mut lines = bytes.split(|byte| *byte == b'\n');
        if partial_first {
            lines.next();
        }
        let found = lines.any(|line| {
            serde_json::from_slice::<Value>(line)
                .ok()
                .as_ref()
                .is_some_and(&relevant)
        });
        if found || offset == 0 || width >= max_bytes {
            return Some((bytes, partial_first));
        }
        width = width.saturating_mul(2).min(max_bytes).min(size);
    }
}

fn model_record(value: &Value, provider: AgentSessionProvider) -> bool {
    match (provider, value.get("type").and_then(Value::as_str)) {
        (AgentSessionProvider::Codex, Some("turn_context")) => {
            value.pointer("/payload/model").is_some()
        }
        (AgentSessionProvider::Codex, Some("event_msg"))
            if value.pointer("/payload/type").and_then(Value::as_str)
                == Some("thread_settings_applied") =>
        {
            value.pointer("/payload/thread_settings/model").is_some()
        }
        (AgentSessionProvider::Claude, Some("assistant")) => {
            value.pointer("/message/model").is_some()
        }
        _ => false,
    }
}

/// Read only lifecycle markers from a bounded tail; never return transcript text.
pub(super) fn codex_open_turn(
    path: &Path,
    now: DateTime<Utc>,
    expand: bool,
) -> Option<DateTime<Utc>> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let max_bytes = if expand {
        4 * 1024 * 1024
    } else {
        METADATA_TAIL_BYTES
    };
    let (tail, partial_first) = metadata_tail(&mut file, size, max_bytes, |value| {
        value.get("type").and_then(Value::as_str) == Some("event_msg")
            && match value.pointer("/payload/type").and_then(Value::as_str) {
                Some("task_started") => true,
                Some("task_complete" | "turn_aborted") => value
                    .pointer("/payload/turn_id")
                    .and_then(Value::as_str)
                    .is_none(),
                _ => false,
            }
    })?;
    // A partial final line may be a completion event still being written.
    if !tail.ends_with(b"\n") {
        return None;
    }
    let mut lines = tail.split(|byte| *byte == b'\n');
    if partial_first {
        lines.next(); // The first line is truncated.
    }
    let mut open: Option<(String, DateTime<Utc>)> = None;
    for line in lines {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let payload = &value["payload"];
        match payload.get("type").and_then(Value::as_str) {
            Some("task_started") => {
                open = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .and_then(|id| {
                        let started = value.get("timestamp")?.as_str()?;
                        let started = DateTime::parse_from_rfc3339(started)
                            .ok()?
                            .with_timezone(&Utc);
                        Some((id.to_owned(), started))
                    });
            }
            Some("task_complete" | "turn_aborted") => {
                let id = payload.get("turn_id").and_then(Value::as_str);
                if id.is_none()
                    || open
                        .as_ref()
                        .is_some_and(|(current, _)| Some(current.as_str()) == id)
                {
                    open = None;
                }
            }
            _ => {}
        }
    }
    let (_, started) = open?;
    let age = now.signed_duration_since(started);
    (age >= chrono::Duration::zero()
        && age <= chrono::Duration::seconds(super::MAX_OPEN_TURN_SECONDS))
    .then_some(started)
}

#[derive(Default)]
pub(super) struct PresenceMetadata {
    pub model: Option<String>,
    pub effort: Option<&'static str>,
    pub fast: Option<bool>,
    pub started_at: Option<DateTime<Utc>>,
    pub tokens: Option<u64>,
}

fn safe_model(value: &str, provider: AgentSessionProvider) -> Option<String> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let known_prefix = match provider {
        AgentSessionProvider::Codex => ["gpt-", "codex-", "o1", "o3", "o4"]
            .iter()
            .any(|p| lower.starts_with(p)),
        AgentSessionProvider::Claude => lower.starts_with("claude-"),
        AgentSessionProvider::Pi => false,
    };
    (known_prefix
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c)))
    .then(|| value.to_string())
}

pub(super) fn read(
    path: &Path,
    provider: AgentSessionProvider,
    now: DateTime<Utc>,
    tokens_enabled: bool,
) -> PresenceMetadata {
    let mut result = PresenceMetadata::default();
    let Ok(mut file) = File::open(path) else {
        return result;
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return result;
    };
    let mut prefix = Vec::new();
    if (&mut file)
        .take(METADATA_PREFIX_BYTES)
        .read_to_end(&mut prefix)
        .is_err()
    {
        return result;
    }
    scan_lines(&mut result, &prefix, provider, now, true);
    if size > METADATA_PREFIX_BYTES {
        // Only publish metadata from the latest complete model record; a
        // prefix value may have been superseded outside the search window.
        result.model = None;
        result.effort = None;
        result.fast = None;
        if let Some((tail, partial_first)) =
            metadata_tail(&mut file, size, METADATA_SEARCH_BYTES, |value| {
                model_record(value, provider)
            })
            && tail.ends_with(b"\n")
        {
            let complete = if !partial_first {
                tail.as_slice()
            } else {
                tail.iter()
                    .position(|b| *b == b'\n')
                    .map_or(&[][..], |i| &tail[i + 1..])
            };
            scan_lines(&mut result, complete, provider, now, false);
        }
    }
    result.tokens = if !tokens_enabled {
        None
    } else {
        match provider {
            AgentSessionProvider::Codex => codex_tokens(path, now),
            AgentSessionProvider::Claude => claude_tokens(path),
            _ => None,
        }
    };
    result
}

fn scan_lines(
    result: &mut PresenceMetadata,
    bytes: &[u8],
    provider: AgentSessionProvider,
    now: DateTime<Utc>,
    capture_start: bool,
) {
    // A truncated first/last line is ignored as invalid JSON.
    for line in BufReader::new(bytes).lines() {
        let Ok(line) = line else { continue };
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = value.get("type").and_then(Value::as_str);
        if capture_start
            && result.started_at.is_none()
            && (provider == AgentSessionProvider::Claude || kind == Some("session_meta"))
        {
            result.started_at = value
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&Utc))
                .filter(|t| *t <= now);
        }
        let settings = match (provider, kind) {
            (AgentSessionProvider::Codex, Some("turn_context")) => value.get("payload"),
            (AgentSessionProvider::Codex, Some("event_msg"))
                if value.pointer("/payload/type").and_then(Value::as_str)
                    == Some("thread_settings_applied") =>
            {
                value.pointer("/payload/thread_settings")
            }
            _ => None,
        };
        let model = match provider {
            AgentSessionProvider::Codex if kind == Some("turn_context") => {
                value.pointer("/payload/model")
            }
            AgentSessionProvider::Codex if settings.is_some() => {
                settings.and_then(|s| s.get("model"))
            }
            AgentSessionProvider::Claude if kind == Some("assistant") => {
                value.pointer("/message/model")
            }
            _ => None,
        };
        if let Some(raw_model) = model {
            let model = raw_model
                .as_str()
                .and_then(|value| safe_model(value, provider));
            if result.model != model {
                result.effort = None;
                result.fast = None;
            }
            result.model = model;
        }
        if let Some(settings) = settings {
            let effort = settings
                .get("effort")
                .and_then(Value::as_str)
                .or_else(|| settings.get("reasoning_effort").and_then(Value::as_str))
                .or_else(|| {
                    settings
                        .pointer("/collaboration_mode/settings/reasoning_effort")
                        .and_then(Value::as_str)
                });
            let has_effort_field = settings.get("effort").is_some()
                || settings.get("reasoning_effort").is_some()
                || settings
                    .pointer("/collaboration_mode/settings/reasoning_effort")
                    .is_some();
            // A new turn is a fresh snapshot; do not present a prior turn's
            // effort as current when the new one supplies no evidence.
            if kind == Some("turn_context") || has_effort_field {
                result.effort = match effort {
                    Some("minimal") => Some("Minimal"),
                    Some("low") => Some("Low"),
                    Some("medium") => Some("Medium"),
                    Some("high") => Some("High"),
                    Some("xhigh") => Some("XHigh"),
                    Some("max") => Some("Max"),
                    Some("ultra") => Some("Ultra"),
                    _ => None,
                };
            }
            if let Some(tier) = settings.get("service_tier") {
                result.fast = match tier.as_str() {
                    Some("priority" | "fast") => Some(true),
                    Some("default" | "standard") => Some(false),
                    _ => None,
                };
            }
        }
        if provider == AgentSessionProvider::Claude && kind == Some("assistant") {
            // Claude usage.speed reports the speed actually used for this
            // response. Missing or unknown evidence must clear stale status.
            result.fast = match value
                .pointer("/message/usage/speed")
                .and_then(Value::as_str)
            {
                Some("fast") => Some(true),
                Some("standard") => Some(false),
                _ => None,
            };
        }
    }
}

struct CodexTokenCache {
    path: PathBuf,
    len: u64,
    modified: SystemTime,
    day: NaiveDate,
    tokens: Option<u64>,
}
static CODEX_TOKEN_CACHE: OnceLock<Mutex<Option<CodexTokenCache>>> = OnceLock::new();

// Hash only the fixed prefix; an active session may append while it is read.
fn codex_prefix_digest(path: &Path, len: u64) -> Option<[u8; 32]> {
    let mut file = File::open(path).ok()?;
    let mut digest = Sha256::new();
    let mut remaining = len;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(64 * 1024)).ok()?;
        let count = file.read(&mut buffer[..wanted]).ok()?;
        if count == 0 {
            return None;
        }
        digest.update(&buffer[..count]);
        remaining -= u64::try_from(count).ok()?;
    }
    Some(digest.finalize().into())
}

fn codex_tokens(path: &Path, now: DateTime<Utc>) -> Option<u64> {
    let before = path.metadata().ok()?;
    let len = before.len();
    let modified = before.modified().ok()?;
    let day = now.date_naive();
    let cache = CODEX_TOKEN_CACHE.get_or_init(|| Mutex::new(None));
    {
        let entry = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entry
            .as_ref()
            .filter(|e| e.path == path && e.len == len && e.modified == modified && e.day == day)
        {
            return entry.tokens;
        }
    }
    let fingerprint = codex_prefix_digest(path, len)?;
    let metadata = JsonlScanner::read_codex_session_metadata(path).ok()?;
    // Child rollouts can include inherited counters. Without the parent baseline
    // they cannot be labelled as this session's own usage.
    if metadata.lineage.uses_parent_baseline() {
        return None;
    }
    let range = CostUsageDayRange::new(NaiveDate::from_ymd_opt(1970, 1, 1)?, day);
    // A single streaming pass preserves the parser's cumulative-watermark state.
    // Freeze the target so a live session cannot extend the scan indefinitely.
    let parsed = JsonlScanner::parse_codex_file_with_state_bounded_target(
        path,
        &range,
        0,
        None,
        None,
        None,
        None,
        None,
        Some(i64::try_from(len).ok()?),
        None,
    )
    .ok()?;
    if !parsed.is_complete
        || parsed.parsed_bytes != i64::try_from(len).ok()?
        || parsed.records.is_empty()
    {
        return None;
    }
    let tokens = parsed.records.iter().try_fold(0_u64, |sum, (record, _)| {
        sum.checked_add(u64::try_from(record.input).ok()?)?
            .checked_add(u64::try_from(record.output).ok()?)
    });
    let after = path.metadata().ok()?;
    // Verify the frozen prefix even if the file grew while parsing. This
    // rejects replacement/truncation rather than silently reporting wrong sums.
    if after.len() < len || codex_prefix_digest(path, len)? != fingerprint {
        return None;
    }
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(CodexTokenCache {
        path: path.to_path_buf(),
        len,
        modified,
        day,
        tokens,
    });
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_model_timestamp_and_deduplicated_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude.jsonl");
        let row = r#"{"type":"assistant","timestamp":"2026-01-01T01:00:00Z","requestId":"r1","message":{"id":"m1","model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":30,"cache_creation_input_tokens":5}}}"#;
        std::fs::write(&path, format!("{row}\n{row}\n")).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let result = read(&path, AgentSessionProvider::Claude, now, true);
        assert_eq!(result.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(result.started_at.unwrap().timestamp(), 1767229200);
        assert_eq!(result.tokens, Some(155));
        let without_tokens = read(&path, AgentSessionProvider::Claude, now, false);
        assert_eq!(without_tokens.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!(without_tokens.started_at.is_some());
        assert!(without_tokens.tokens.is_none());
    }

    #[test]
    fn rejects_unsafe_model_names() {
        assert!(safe_model("gpt-6.1", AgentSessionProvider::Codex).is_some());
        assert!(safe_model("C:/private/path", AgentSessionProvider::Codex).is_none());
        assert!(safe_model("private-secret", AgentSessionProvider::Claude).is_none());
    }

    #[test]
    fn codex_modes_follow_explicit_settings_and_clear_on_model_change() {
        let now = Utc::now();
        let mut result = PresenceMetadata::default();
        let rows = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"model\":\"gpt-6.1\",\"reasoning_effort\":\"high\",\"service_tier\":\"priority\"}}}\n";
        scan_lines(
            &mut result,
            rows.as_bytes(),
            AgentSessionProvider::Codex,
            now,
            false,
        );
        assert_eq!(result.effort, Some("High"));
        assert_eq!(result.fast, Some(true));
        scan_lines(
            &mut result,
            b"{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6.1\"}}\n",
            AgentSessionProvider::Codex,
            now,
            false,
        );
        assert_eq!(result.effort, None);
        assert_eq!(result.fast, Some(true));
        scan_lines(&mut result, b"{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6.1\",\"effort\":null,\"collaboration_mode\":{\"settings\":{\"reasoning_effort\":\"xhigh\"}}}}\n", AgentSessionProvider::Codex, now, false);
        assert_eq!(result.effort, Some("XHigh"));
        scan_lines(&mut result, b"{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6\",\"effort\":\"medium\",\"service_tier\":\"default\"}}\n", AgentSessionProvider::Codex, now, false);
        assert_eq!(result.effort, Some("Medium"));
        assert_eq!(result.fast, Some(false));
        scan_lines(
            &mut result,
            b"{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
            AgentSessionProvider::Codex,
            now,
            false,
        );
        assert_eq!(result.effort, None);
        assert_eq!(result.fast, None);
    }

    #[test]
    fn claude_speed_is_observed_and_missing_speed_clears_old_value() {
        let now = Utc::now();
        let mut result = PresenceMetadata::default();
        scan_lines(&mut result, b"{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{\"speed\":\"fast\"}}}\n", AgentSessionProvider::Claude, now, false);
        assert_eq!(result.fast, Some(true));
        scan_lines(
            &mut result,
            b"{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-6\",\"usage\":{}}}\n",
            AgentSessionProvider::Claude,
            now,
            false,
        );
        assert_eq!(result.fast, None);
    }

    #[test]
    fn metadata_tail_keeps_a_complete_line_at_the_search_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boundary.jsonl");
        let model = b"{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6.1\"}}\n";
        let mut bytes = b"prefix\n".to_vec();
        bytes.extend_from_slice(model);
        let target = usize::try_from(METADATA_TAIL_BYTES).unwrap();
        bytes.extend(std::iter::repeat_n(b' ', target - model.len() - 1));
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).unwrap();
        let mut file = File::open(&path).unwrap();
        let (tail, partial) = metadata_tail(
            &mut file,
            u64::try_from(bytes.len()).unwrap(),
            METADATA_TAIL_BYTES,
            |value| model_record(value, AgentSessionProvider::Codex),
        )
        .unwrap();
        assert!(!partial);
        assert!(tail.starts_with(model));
        let now = Utc::now();
        assert_eq!(
            read(&path, AgentSessionProvider::Codex, now, false)
                .model
                .as_deref(),
            Some("gpt-6.1")
        );
    }

    #[test]
    fn long_turn_finds_latest_model_and_open_turn_beyond_old_tail_limit() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long-turn.jsonl");
        let mut file = File::create(&path).unwrap();
        let padding = "x".repeat(700_000);
        writeln!(file, "{{\"type\":\"user\",\"padding\":\"{padding}\"}}").unwrap();
        writeln!(file, r#"{{"type":"turn_context","payload":{{"model":"gpt-6.1-terra","effort":"high","service_tier":"priority"}}}}"#).unwrap();
        writeln!(file, r#"{{"type":"event_msg","timestamp":"2026-01-01T01:00:00Z","payload":{{"type":"task_started","turn_id":"t1"}}}}"#).unwrap();
        writeln!(file, "{{\"type\":\"user\",\"padding\":\"{padding}\"}}").unwrap();
        writeln!(file, "{{\"type\":\"user\",\"padding\":\"{padding}\"}}").unwrap();
        drop(file);
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let result = read(&path, AgentSessionProvider::Codex, now, false);
        assert_eq!(result.model.as_deref(), Some("gpt-6.1-terra"));
        assert_eq!(result.effort, Some("High"));
        assert_eq!(result.fast, Some(true));
        assert!(codex_open_turn(&path, now, true).is_some());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"other"}}}}"#
        )
        .unwrap();
        drop(file);
        assert!(codex_open_turn(&path, now, true).is_some());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            file,
            r#"{{"type":"turn_context","payload":{{"model":"private/unsafe"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"type":"event_msg","payload":{{"type":"task_complete","turn_id":"t1"}}}}"#
        )
        .unwrap();
        drop(file);
        let updated = read(&path, AgentSessionProvider::Codex, now, false);
        assert!(updated.model.is_none());
        assert!(updated.effort.is_none());
        assert!(updated.fast.is_none());
        assert!(codex_open_turn(&path, now, true).is_none());
    }

    #[test]
    fn large_transcript_keeps_bounded_model_and_start_without_inventing_tokens() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"session_meta","timestamp":"2026-01-01T01:00:00Z","payload":{{"id":"s"}}}}"#
        )
        .unwrap();
        file.seek(SeekFrom::Start(LARGE_SESSION_TEST_THRESHOLD + 1))
            .unwrap();
        writeln!(
            file,
            "\n{{\"type\":\"turn_context\",\"payload\":{{\"model\":\"gpt-6.1\"}}}}"
        )
        .unwrap();
        drop(file);
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let result = read(&path, AgentSessionProvider::Codex, now, true);
        assert_eq!(result.model.as_deref(), Some("gpt-6.1"));
        assert!(result.started_at.is_some());
        assert!(result.tokens.is_none());
    }

    #[test]
    fn large_claude_session_tokens_refresh_when_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude.jsonl");
        let padding = "x".repeat(usize::try_from(LARGE_SESSION_TEST_THRESHOLD).unwrap() + 1);
        let row = r#"{"type":"assistant","timestamp":"2026-01-01T01:00:00Z","requestId":"r1","message":{"id":"m1","model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":2}}}"#;
        std::fs::write(
            &path,
            format!("{{\"type\":\"user\",\"padding\":\"{padding}\"}}\n{row}\n"),
        )
        .unwrap();
        assert_eq!(claude_tokens(&path), Some(12));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", row.replace("r1", "r2").replace("m1", "m2")).unwrap();
        drop(file);
        assert_eq!(claude_tokens(&path), Some(24));
    }

    #[test]
    fn large_codex_session_is_scanned_and_refreshed_after_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.jsonl");
        let padding = "x".repeat(usize::try_from(LARGE_SESSION_TEST_THRESHOLD).unwrap() + 1);
        let meta =
            r#"{"type":"session_meta","timestamp":"2026-01-01T01:00:00Z","payload":{"id":"abc"}}"#;
        let first = r#"{"type":"event_msg","timestamp":"2026-01-01T01:00:02Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":2}}}}"#;
        std::fs::write(
            &path,
            format!("{meta}\n{{\"type\":\"user\",\"padding\":\"{padding}\"}}\n{first}\n"),
        )
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(codex_tokens(&path, now), Some(12));
        use std::io::Write;
        let second = first
            .replace("\"input_tokens\":10", "\"input_tokens\":20")
            .replace("\"output_tokens\":2", "\"output_tokens\":4");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{second}").unwrap();
        drop(file);
        assert_eq!(codex_tokens(&path, now), Some(24));
    }

    #[test]
    fn large_codex_session_preserves_cumulative_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.jsonl");
        let padding = "x".repeat(usize::try_from(LARGE_SESSION_TEST_THRESHOLD).unwrap() + 1);
        let meta =
            r#"{"type":"session_meta","timestamp":"2026-01-01T01:00:00Z","payload":{"id":"abc"}}"#;
        let event = |input, output| {
            serde_json::json!({
                "type": "event_msg", "timestamp": "2026-01-01T01:00:02Z",
                "payload": {"type": "token_count", "info": {"total_token_usage": {
                    "input_tokens": input, "cached_input_tokens": 0, "output_tokens": output
                }}}
            })
            .to_string()
        };
        std::fs::write(
            &path,
            format!(
                "{meta}\n{}\n{}\n{{\"type\":\"user\",\"padding\":\"{padding}\"}}\n{}\n",
                event(100, 20),
                event(5, 1),
                event(101, 21)
            ),
        )
        .unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(codex_tokens(&path, now), Some(122));
    }

    #[test]
    fn codex_incomplete_tail_is_not_counted_until_finished() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.jsonl");
        let meta =
            r#"{"type":"session_meta","timestamp":"2026-01-01T01:00:00Z","payload":{"id":"abc"}}"#;
        let event = r#"{"type":"event_msg","timestamp":"2026-01-01T01:00:02Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"output_tokens":2}}}}"#;
        std::fs::write(&path, format!("{meta}\n{event}\n{{\"type\":")).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(codex_tokens(&path, now), None);
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "\"user\"}}").unwrap();
        drop(file);
        assert_eq!(codex_tokens(&path, now), Some(12));
    }

    #[test]
    fn codex_prefix_verification_accepts_append_and_detects_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.jsonl");
        std::fs::write(&path, b"original").unwrap();
        let digest = codex_prefix_digest(&path, 8).unwrap();
        std::fs::write(&path, b"original appended").unwrap();
        assert_eq!(codex_prefix_digest(&path, 8), Some(digest));
        std::fs::write(&path, b"modified appended").unwrap();
        assert_ne!(codex_prefix_digest(&path, 8), Some(digest));
        std::fs::write(&path, b"short").unwrap();
        assert_eq!(codex_prefix_digest(&path, 8), None);
    }

    #[test]
    fn codex_uses_cumulative_deltas_and_skips_inherited_forks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex.jsonl");
        let rows = [
            r#"{"type":"session_meta","timestamp":"2026-01-01T01:00:00Z","payload":{"id":"abc"}}"#,
            r#"{"type":"turn_context","timestamp":"2026-01-01T01:00:01Z","payload":{"model":"gpt-6.1"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T01:00:02Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":20}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T01:00:03Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":150,"cached_input_tokens":60,"output_tokens":30}}}}"#,
        ];
        std::fs::write(&path, rows.join("\n") + "\n").unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let result = read(&path, AgentSessionProvider::Codex, now, true);
        assert_eq!(result.model.as_deref(), Some("gpt-6.1"));
        assert_eq!(result.tokens, Some(180)); // Cached input is included in input.
        let fork = rows[0].replace(
            "\"id\":\"abc\"",
            "\"id\":\"abc\",\"forked_from_id\":\"parent\"",
        );
        std::fs::write(
            &path,
            format!("{fork}\n{}\n{}\n{}\n", rows[1], rows[2], rows[3]),
        )
        .unwrap();
        assert!(
            read(&path, AgentSessionProvider::Codex, now, true)
                .tokens
                .is_none()
        );
    }
}
