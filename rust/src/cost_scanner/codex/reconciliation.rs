use super::*;

pub(super) fn paused_codex_summary(
    cache: &CostUsageCache,
    start_date: NaiveDate,
    today: NaiveDate,
    range: &CostUsageDayRange,
    compatible_context: bool,
) -> CostSummary {
    if compatible_context
        && let Some(summary) = codex_current_window_summary(cache, range, start_date, today)
    {
        return summary;
    }

    // Without a retained report, rebuild one for the requested days only; the
    // cache can hold days outside this window (#755: Today showed 30d totals).
    let report = cache
        .previous_report
        .clone()
        .unwrap_or_else(|| JsonlScanner::cached_cost_report_for_range(cache, range));
    summary_from_cached_report(&report, start_date, today)
}

/// Whether incomplete Codex work is confined to history older than the
/// requested reporting window. This is deliberately conservative: an unknown
/// queue context, source error, malformed path, or unconsumed file tail keeps
/// the current window incomplete until a later scan proves it.
pub(super) fn codex_current_window_is_established(
    cache: &CostUsageCache,
    range: &CostUsageDayRange,
) -> bool {
    if !cache.codex_scan_incomplete {
        return true;
    }
    if cache.codex_pending_paths.is_empty()
        || matches!(
            cache.codex_scan_pause_reason,
            Some(CodexScanPauseReason::Error(_))
        )
    {
        return false;
    }

    let Some(pending_since) = cache.codex_pending_scan_since_key.as_deref() else {
        return false;
    };
    let Some(pending_until) = cache.codex_pending_scan_until_key.as_deref() else {
        return false;
    };
    if pending_since > range.scan_since_key.as_str()
        || pending_until < range.scan_until_key.as_str()
        || cache.codex_pending_scan_root_paths.is_empty()
        || cache.codex_pending_scan_timezone.is_none()
    {
        return false;
    }

    cache
        .codex_pending_paths
        .iter()
        .all(|path| !codex_pending_path_affects_current_window(cache, path, range))
}

/// Current-window publication requires positive decoded evidence. A missing
/// day cannot be treated as a validated zero while historical discovery is
/// still pending.
pub(super) fn codex_current_window_has_evidence(
    cache: &CostUsageCache,
    range: &CostUsageDayRange,
) -> bool {
    cache
        .days
        .keys()
        .any(|day| CostUsageDayRange::is_in_range(day, &range.since_key, &range.until_key))
}

pub(super) fn codex_current_window_report(
    cache: &CostUsageCache,
    range: &CostUsageDayRange,
) -> Option<CachedCostReport> {
    (codex_current_window_is_established(cache, range)
        && codex_current_window_has_evidence(cache, range))
    .then(|| JsonlScanner::cached_cost_report_for_range(cache, range))
}

/// Rebuild the model breakdown from the same validated day map as the
/// current-window report. A cached report alone has no model rows, so using
/// it here would silently discard the estimated cost in Usage & Spend.
pub(super) fn codex_current_window_summary(
    cache: &CostUsageCache,
    range: &CostUsageDayRange,
    start_date: NaiveDate,
    today: NaiveDate,
) -> Option<CostSummary> {
    let report = codex_current_window_report(cache, range)?;
    // Cache entries outside the currently bound roots must never contribute to
    // a newly published breakdown, even if an older scan left them on disk.
    let roots: Vec<PathBuf> = cache
        .codex_pending_scan_root_paths
        .iter()
        .map(PathBuf::from)
        .collect();
    let all_in_bound_roots = cache.files.keys().all(|path| {
        let Ok(path) = fs::canonicalize(path) else {
            return false;
        };
        #[cfg(windows)]
        let path = PathBuf::from(path.to_string_lossy().to_ascii_lowercase());
        roots.iter().any(|root| path.starts_with(root))
    });
    if !all_in_bound_roots {
        return None;
    }
    let mut summary = CostSummary {
        period_start: Some(start_date),
        period_end: Some(today),
        sessions_count: u32::try_from(report.sessions_count.max(0)).unwrap_or(0),
        history_coverage_established: true,
        ..CostSummary::default()
    };
    let (cost, _) = add_codex_days_map_to_summary(&mut summary, &cache.days, range);
    summary.total_cost_usd = cost;
    Some(summary)
}

fn codex_pending_path_affects_current_window(
    cache: &CostUsageCache,
    path_key: &str,
    range: &CostUsageDayRange,
) -> bool {
    let Some(usage) = cache.files.get(path_key) else {
        return true;
    };
    if usage.codex_unresolved_fork_parent || usage.days.is_empty() {
        return true;
    }
    if usage.days.keys().any(|day| {
        CostUsageDayRange::parse_day_key(day).is_none()
            || day >= &range.scan_since_key
            || CostUsageDayRange::is_in_range(day, &range.since_key, &range.until_key)
    }) {
        return true;
    }

    let Ok(metadata) = fs::metadata(path_key) else {
        return true;
    };
    #[allow(clippy::cast_possible_wrap, reason = "file sizes are clamped to i64")]
    let observed_size = metadata.len().min(i64::MAX as u64) as i64;
    if codex_logical_target_has_unconsumed_tail(observed_size, usage) {
        return true;
    }
    let identity_matches = super::codex_file_identity_matches(
        usage.codex_file_identity.as_deref(),
        JsonlScanner::codex_file_identity(Path::new(path_key), &metadata).as_deref(),
    );
    if !identity_matches
        || usage.mtime_unix_ms != system_time_to_unix_ms(metadata.modified().ok())
        || usage.size != observed_size
    {
        return true;
    }

    false
}

/// Return cached Codex files that are provably gone from the portion of the
/// sessions tree covered by this scan. Entries outside the current roots or
/// date directories are intentionally retained for a later scan.
pub(super) fn missing_codex_cache_paths(
    cache: &CostUsageCache,
    sessions_dirs: &[PathBuf],
    range: &CostUsageDayRange,
) -> Vec<String> {
    let scanned_date_dirs: Vec<PathBuf> = sessions_dirs
        .iter()
        .flat_map(|sessions_dir| {
            codex_scan_dates(range).into_iter().map(|date| {
                sessions_dir
                    .join(date.format("%Y").to_string())
                    .join(date.format("%m").to_string())
                    .join(date.format("%d").to_string())
            })
        })
        .collect();

    cache
        .files
        .keys()
        .filter(|path_key| {
            let path = Path::new(path_key.as_str());
            let in_scanned_root = sessions_dirs
                .iter()
                .any(|sessions_dir| path.starts_with(sessions_dir));
            let in_scanned_date = scanned_date_dirs
                .iter()
                .any(|date_dir| path.starts_with(date_dir));
            let in_scanned_flat_dir = is_flat_codex_path_in_scan_window(path, sessions_dirs, range);
            ((in_scanned_root && in_scanned_date) || in_scanned_flat_dir) && !path.exists()
        })
        .cloned()
        .collect()
}

/// Remove cached Codex files that are provably gone after an explicit refresh.
pub(super) fn reconcile_missing_codex_cache_files(
    cache: &mut CostUsageCache,
    sessions_dirs: &[PathBuf],
    range: &CostUsageDayRange,
) {
    for path in missing_codex_cache_paths(cache, sessions_dirs, range) {
        cache.files.remove(&path);
    }
    cache
        .codex_pending_paths
        .retain(|path| Path::new(path).exists());
    cache
        .codex_source_rows
        .retain(|path, _| Path::new(path).exists());
}

/// Whether the cache contains a path that depends on this source partition.
/// Missing optional roots are normal; only a root/date partition that has
/// previously contributed a cached or queued path can make discovery
/// incomplete.
pub(super) fn cache_has_codex_path_under(cache: &CostUsageCache, parent: &Path) -> bool {
    cache
        .files
        .keys()
        .chain(cache.codex_pending_paths.iter())
        .any(|path| Path::new(path).starts_with(parent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(day: &str, input: i64) -> CostUsageFileUsage {
        CostUsageFileUsage {
            mtime_unix_ms: 0,
            size: 100,
            codex_file_identity: None,
            days: HashMap::from([(
                day.to_string(),
                HashMap::from([("gpt-5.6-sol".to_string(), vec![input, 0, 10])]),
            )]),
            parsed_bytes: Some(100),
            codex_scan_target_size: None,
            last_model: Some("gpt-5.6-sol".to_string()),
            last_totals: None,
            codex_token_timestamps_monotonic: Some(true),
            codex_last_token_timestamp: None,
            codex_session_id: None,
            codex_forked_from_id: None,
            codex_fork_accounting_state: None,
            codex_lineage: CodexSessionLineage::Root,
            codex_fork_timestamp: None,
            codex_unresolved_fork_parent: false,
        }
    }

    fn historical_pending_cache(old_path: &str, current_path: &str) -> CostUsageCache {
        CostUsageCache {
            last_scan_unix_ms: 1,
            files: HashMap::from([
                (old_path.to_string(), usage("2026-09-01", 900)),
                (current_path.to_string(), usage("2026-09-19", 100)),
            ]),
            days: HashMap::from([
                (
                    "2026-09-01".to_string(),
                    HashMap::from([("gpt-5.6-sol".to_string(), vec![900, 0, 10])]),
                ),
                (
                    "2026-09-19".to_string(),
                    HashMap::from([("gpt-5.6-sol".to_string(), vec![100, 0, 10])]),
                ),
            ]),
            codex_pending_paths: vec![old_path.to_string()],
            codex_scan_incomplete: true,
            codex_pending_scan_since_key: Some("2026-09-01".to_string()),
            codex_pending_scan_until_key: Some("2026-09-20".to_string()),
            codex_pending_scan_root_paths: vec!["C:\\sessions".to_string()],
            codex_pending_scan_timezone: Some("UTC".to_string()),
            ..CostUsageCache::default()
        }
    }

    fn active_range() -> CostUsageDayRange {
        CostUsageDayRange {
            since_key: "2026-09-19".to_string(),
            until_key: "2026-09-19".to_string(),
            scan_since_key: "2026-09-18".to_string(),
            scan_until_key: "2026-09-20".to_string(),
        }
    }

    #[test]
    fn historical_pending_work_keeps_current_window_publishable() {
        let root = tempfile::tempdir().unwrap();
        let old_path = root.path().join("old.jsonl");
        let current_path = root.path().join("current.jsonl");
        std::fs::write(&old_path, vec![0_u8; 100]).unwrap();
        std::fs::write(&current_path, vec![0_u8; 100]).unwrap();
        let old_key = old_path.to_string_lossy().into_owned();
        let current_key = current_path.to_string_lossy().into_owned();
        let mut cache = historical_pending_cache(&old_key, &current_key);
        let bound_root = std::fs::canonicalize(root.path())
            .unwrap()
            .to_string_lossy()
            .to_string();
        #[cfg(windows)]
        let bound_root = bound_root.to_ascii_lowercase();
        cache.codex_pending_scan_root_paths = vec![bound_root];
        let metadata = std::fs::metadata(&old_path).unwrap();
        let old_usage = cache.files.get_mut(&old_key).unwrap();
        old_usage.mtime_unix_ms = system_time_to_unix_ms(metadata.modified().ok());
        old_usage.size = i64::try_from(metadata.len()).unwrap();
        old_usage.codex_file_identity = JsonlScanner::codex_file_identity(&old_path, &metadata);
        let range = active_range();

        assert!(codex_current_window_is_established(&cache, &range));
        assert!(codex_current_window_has_evidence(&cache, &range));
        let report = codex_current_window_report(&cache, &range).unwrap();
        assert_eq!(report.input_tokens, 100);
        assert_eq!(report.sessions_count, 1);

        let day = NaiveDate::from_ymd_opt(2026, 9, 19).unwrap();
        let summary = codex_current_window_summary(&cache, &range, day, day).unwrap();
        assert!(summary.history_coverage_established);
        assert_eq!(summary.input_tokens, 100);
        assert_eq!(summary.sessions_count, 1);
        assert!(
            summary
                .by_model
                .get("gpt-5.6-sol")
                .is_some_and(|cost| *cost > 0.0)
        );
        assert_eq!(summary.by_model_tokens["gpt-5.6-sol"].input_tokens, 100);
        assert!(summary.total_cost_usd > 0.0);

        let paused = paused_codex_summary(&cache, day, day, &range, true);
        assert_eq!(paused.by_model, summary.by_model);
        assert_eq!(paused.by_model_tokens, summary.by_model_tokens);
        let incompatible = paused_codex_summary(&cache, day, day, &range, false);
        assert!(!incompatible.history_coverage_established);
        assert!(incompatible.by_model.is_empty());
        cache.codex_pending_scan_root_paths =
            vec![root.path().join("other-root").to_string_lossy().to_string()];
        assert!(codex_current_window_summary(&cache, &range, day, day).is_none());
    }

    #[test]
    fn historical_pending_entry_without_or_mismatched_identity_blocks_publication() {
        let root = tempfile::tempdir().unwrap();
        let old_path = root.path().join("old.jsonl");
        let current_path = root.path().join("current.jsonl");
        std::fs::write(&old_path, vec![0_u8; 100]).unwrap();
        std::fs::write(&current_path, vec![0_u8; 100]).unwrap();
        let old_key = old_path.to_string_lossy().into_owned();
        let current_key = current_path.to_string_lossy().into_owned();
        let metadata = std::fs::metadata(&old_path).unwrap();
        let range = active_range();

        for cached_identity in [None, Some("different-file".to_string())] {
            let mut cache = historical_pending_cache(&old_key, &current_key);
            let old_usage = cache.files.get_mut(&old_key).unwrap();
            old_usage.mtime_unix_ms = system_time_to_unix_ms(metadata.modified().ok());
            old_usage.size = i64::try_from(metadata.len()).unwrap();
            old_usage.codex_file_identity = cached_identity;

            assert!(codex_pending_path_affects_current_window(
                &cache, &old_key, &range
            ));
            assert!(!codex_current_window_is_established(&cache, &range));
        }
    }

    #[test]
    fn metadata_failure_blocks_historical_pending_publication() {
        let old_path = r"C:\sessions\missing-old.jsonl";
        let current_path = r"C:\sessions\current.jsonl";
        let cache = historical_pending_cache(old_path, current_path);
        let range = active_range();

        assert!(!codex_current_window_is_established(&cache, &range));
        assert!(codex_current_window_report(&cache, &range).is_none());
    }

    #[test]
    fn current_pending_work_and_source_errors_block_publication() {
        let old_path = r"C:\sessions\old.jsonl";
        let current_path = r"C:\sessions\current.jsonl";
        let range = active_range();

        let mut current_pending = historical_pending_cache(old_path, current_path);
        current_pending.codex_pending_paths = vec![current_path.to_string()];
        assert!(!codex_current_window_is_established(
            &current_pending,
            &range
        ));

        let mut source_error = historical_pending_cache(old_path, current_path);
        source_error.codex_scan_pause_reason = Some(CodexScanPauseReason::Error(
            "source unavailable".to_string(),
        ));
        assert!(!codex_current_window_is_established(&source_error, &range));
        assert!(codex_current_window_report(&source_error, &range).is_none());
    }
}
