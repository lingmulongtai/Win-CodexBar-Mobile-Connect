use super::*;
use crate::core::CodexSourceUsageRow;
use serde::Deserialize;

#[derive(Deserialize)]
struct SavedHistory {
    version: u32,
    backup_sha256: String,
    root: String,
    sessions: Vec<SavedSession>,
}
#[derive(Deserialize)]
struct SavedSession {
    session_id: String,
    rows: Vec<CodexSourceUsageRow>,
}

fn root_key(path: &Path) -> String {
    let value = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let value = value
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    if cfg!(windows) {
        value.to_lowercase()
    } else {
        value
    }
}

pub(super) fn load(cache: &mut CostUsageCache, roots: &[PathBuf], base: &Path) {
    cache.codex_saved_rows.clear();
    for filename in ["saved-codex-history.json", "saved-codex-history-extra.json"] {
        load_one(cache, roots, &base.join(filename));
    }
}

fn load_one(cache: &mut CostUsageCache, roots: &[PathBuf], path: &Path) {
    if fs::metadata(path).is_ok_and(|m| m.len() > 8 * 1024 * 1024) {
        return;
    }
    let Ok(bytes) = fs::read(path) else { return };
    let Ok(saved) = serde_json::from_slice::<SavedHistory>(&bytes) else {
        return;
    };
    if saved.version != 1
        || saved.backup_sha256.len() != 64
        || !saved.backup_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || !roots
            .iter()
            .any(|root| root.is_dir() && root_key(root) == root_key(Path::new(&saved.root)))
    {
        return;
    }
    let mut ids = HashSet::new();
    for session in saved.sessions {
        if session.session_id.is_empty()
            || cache.codex_saved_rows.contains_key(&session.session_id)
            || !ids.insert(session.session_id.clone())
        {
            continue;
        }
        let mut rows = session.rows;
        if rows.iter().any(|r| {
            r.timestamp.is_none()
                || r.input < 0
                || r.cached < 0
                || r.output < 0
                || r.cached > r.input
        }) {
            continue;
        }
        for row in &mut rows {
            row.day_key = crate::cost_reporting_period::cost_bucket_zone()
                .date(row.timestamp.unwrap())
                .format("%Y-%m-%d")
                .to_string();
        }
        cache.codex_saved_rows.insert(session.session_id, rows);
    }
}

pub(super) fn earliest_day(cache: &CostUsageCache) -> Option<NaiveDate> {
    cache
        .codex_saved_rows
        .values()
        .flatten()
        .filter_map(|row| CostUsageDayRange::parse_day_key(&row.day_key))
        .min()
}

pub(super) fn active_rows(
    cache: &CostUsageCache,
) -> impl Iterator<Item = (&String, &Vec<CodexSourceUsageRow>)> {
    cache.codex_saved_rows.iter().filter(|(id, _)| {
        !cache.codex_scan_incomplete
            && !cache
                .files
                .values()
                .any(|u| u.codex_session_id.as_ref() == Some(*id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn backup_is_source_bound_excluded_while_pending_and_deduplicated() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        let data = json!({"version":1,"backup_sha256":"a".repeat(64),"root":root,
        "sessions":[{"session_id":"saved-id","rows":[{
            "timestamp":"2026-10-12T10:00:00Z","day_key":"old","model":"gpt-6-astra",
            "input":100,"cached":10,"output":5,"reasoning":3,"source_end_offset":42,
            "pricing":{"pricing_model":"gpt-6-astra","pricing_mode":"standard"}
        }]}]});
        fs::write(
            temp.path().join("saved-codex-history.json"),
            data.to_string(),
        )
        .unwrap();
        let mut extra = data.clone();
        extra["sessions"][0]["session_id"] = json!("extra-id");
        fs::write(
            temp.path().join("saved-codex-history-extra.json"),
            extra.to_string(),
        )
        .unwrap();
        let mut cache = CostUsageCache::default();
        load(&mut cache, std::slice::from_ref(&root), temp.path());
        assert_eq!(active_rows(&cache).count(), 2);
        cache.codex_saved_rows.remove("extra-id");
        super::super::cache_days::rebuild_cache_days(&mut cache);
        let date = cache.codex_saved_rows["saved-id"][0].day_key.clone();
        assert_eq!(cache.days[&date]["gpt-6-astra"], vec![100, 10, 5, 3]);
        let day = CostUsageDayRange::parse_day_key(&date).unwrap();
        assert_eq!(earliest_day(&cache), Some(day));
        let mut summary = CostSummary::default();
        let range = CostUsageDayRange::new(day, day);
        add_codex_days_map_to_summary(&mut summary, &cache.days, &range);
        super::super::cache_days::price_summary_from_source_rows(&mut summary, &cache, &range);
        assert!((summary.total_cost_usd - 0.00116).abs() < 1e-9);
        assert!(!summary.model_pricing_completeness.is_partial());
        cache.codex_scan_incomplete = true;
        assert_eq!(active_rows(&cache).count(), 0);
        cache.codex_scan_incomplete = false;
        let usage: crate::core::CostUsageFileUsage = serde_json::from_value(json!({
            "mtime_unix_ms":0,"size":0,"days":{},"parsed_bytes":0,"last_model":null,
            "last_totals":null,"codex_session_id":"saved-id"
        }))
        .unwrap();
        cache.files.insert("live.jsonl".into(), usage);
        assert_eq!(active_rows(&cache).count(), 0);
        load(&mut cache, &[temp.path().join("other")], temp.path());
        assert!(cache.codex_saved_rows.is_empty());
    }
    #[test]
    fn deleted_history_does_not_guess_a_long_context_request_price() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        let data = json!({"version":1,"backup_sha256":"a".repeat(64),"root":root,
        "sessions":[{"session_id":"saved-id","rows":[{
            "timestamp":"2035-01-01T12:00:00Z","day_key":"old","model":"gpt-6-astra",
            "input":300000,"cached":0,"output":5,"source_end_offset":42,
            "pricing":{"pricing_model":"gpt-6-astra","pricing_mode":"standard"}
        }]}]});
        fs::write(
            temp.path().join("saved-codex-history.json"),
            data.to_string(),
        )
        .unwrap();
        let mut cache = CostUsageCache::default();
        load(&mut cache, &[root], temp.path());
        super::super::cache_days::rebuild_cache_days(&mut cache);
        let date = cache.codex_saved_rows["saved-id"][0].day_key.clone();
        let day = CostUsageDayRange::parse_day_key(&date).unwrap();
        let range = CostUsageDayRange::new(day, day);
        let mut summary = CostSummary::default();
        add_codex_days_map_to_summary(&mut summary, &cache.days, &range);
        super::super::cache_days::price_summary_from_source_rows(&mut summary, &cache, &range);
        assert_eq!(summary.input_tokens, 300000);
        assert!(summary.model_pricing_completeness.is_partial());
    }
}
