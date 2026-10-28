//! Public-safe summaries of one local session; never return transcript content.
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::{
    agent_sessions::AgentSessionProvider,
    core::{CostUsageDayRange, JsonlScanner},
};

const MAX_TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;
const METADATA_PREFIX_BYTES: u64 = 256 * 1024;
const METADATA_TAIL_BYTES: u64 = 512 * 1024;

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
    let mut tail = Vec::new();
    if size > METADATA_PREFIX_BYTES {
        if file
            .seek(SeekFrom::Start(size.saturating_sub(METADATA_TAIL_BYTES)))
            .is_err()
            || (&mut file)
                .take(METADATA_TAIL_BYTES)
                .read_to_end(&mut tail)
                .is_err()
        {
            return result;
        }
    }
    scan_lines(&mut result, &prefix, provider, now, true);
    if !tail.is_empty() {
        // Do not misreport an old prefix model when no recent model is found.
        if size > METADATA_TAIL_BYTES {
            result.model = None;
            result.effort = None;
            result.fast = None;
        }
        scan_lines(&mut result, &tail, provider, now, false);
    }
    result.tokens = if !tokens_enabled || size > MAX_TRANSCRIPT_BYTES {
        None
    } else {
        match provider {
            AgentSessionProvider::Codex => codex_tokens(path, now),
            AgentSessionProvider::Claude => crate::cost_scanner::claude_presence_tokens(path),
            AgentSessionProvider::Pi => None,
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
                .or_else(|| settings.get("reasoning_effort"))
                .or_else(|| settings.pointer("/collaboration_mode/settings/reasoning_effort"));
            if let Some(effort) = effort {
                result.effort = match effort.as_str() {
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
        let rows = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"model\":\"gpt-6.1\",\"reasoning_effort\":\"high\",\"service_tier\":\"priority\"}}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6.1\"}}\n"
        );
        scan_lines(
            &mut result,
            rows.as_bytes(),
            AgentSessionProvider::Codex,
            now,
            false,
        );
        assert_eq!(result.effort, Some("High"));
        assert_eq!(result.fast, Some(true));
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
        file.seek(SeekFrom::Start(MAX_TRANSCRIPT_BYTES + 1))
            .unwrap();
        writeln!(
            file,
            "\n{}",
            r#"{"type":"turn_context","payload":{"model":"gpt-6.1"}}"#
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

fn codex_tokens(path: &Path, now: DateTime<Utc>) -> Option<u64> {
    let metadata = JsonlScanner::read_codex_session_metadata(path).ok()?;
    // Child rollouts can include inherited counters. Without the parent baseline
    // they cannot be labelled as this session's own usage.
    if metadata.lineage.uses_parent_baseline() {
        return None;
    }
    let range = CostUsageDayRange::new(NaiveDate::from_ymd_opt(1970, 1, 1)?, now.date_naive());
    let parsed = JsonlScanner::parse_codex_file_with_state_bounded(
        path,
        &range,
        0,
        None,
        None,
        None,
        None,
        None,
        Some(MAX_TRANSCRIPT_BYTES as i64),
    )
    .ok()?;
    if !parsed.is_complete || parsed.records.is_empty() {
        return None;
    }
    parsed.records.iter().try_fold(0_u64, |sum, (record, _)| {
        sum.checked_add(u64::try_from(record.input).ok()?)?
            .checked_add(u64::try_from(record.output).ok()?)
    })
}
