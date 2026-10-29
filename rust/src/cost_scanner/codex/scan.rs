use super::cache_days::days_from_codex_source_rows;
use super::*;
use crate::core::{
    CodexSourceRowCache, CodexSourceUsageRow, read_source_rows, recover_rows, row_cache_matches,
    row_cache_needs_recovery,
};
use chrono::Local;

/// A complete, non-forked file whose source rows may be (re)priced this pass.
struct CodexSourceRowPlan {
    metadata: fs::Metadata,
    source_rows: Vec<CodexSourceUsageRow>,
    recoverable_cache: Option<CodexSourceRowCache>,
}

/// Decide whether a completed file qualifies for source-row evidence, and
/// whether its cached pricing needs recovery. `None` skips the file entirely:
/// the pass was partial, the file keeps an unconsumed tail, or the usage is
/// fork/parent-baseline shaped (whose source rows are not trusted here).
fn codex_source_row_plan(
    cache: &CostUsageCache,
    path: &Path,
    scan_range: &CostUsageDayRange,
) -> Option<CodexSourceRowPlan> {
    let usage_safe = cache
        .files
        .get(&path.to_string_lossy().to_string())
        .is_some_and(|usage| {
            usage.codex_forked_from_id.is_none()
                && !usage.codex_lineage.uses_parent_baseline()
                && !usage.codex_unresolved_fork_parent
        });
    if !usage_safe {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    if cache
        .codex_source_rows
        .get(&path.to_string_lossy().to_string())
        .is_some_and(|source| {
            source.size == i64::try_from(metadata.len()).unwrap_or(-1)
                && source.mtime_unix_ms == system_time_to_unix_ms(metadata.modified().ok())
                && Some(source.file_identity.clone())
                    == JsonlScanner::codex_file_identity(path, &metadata)
                && cache
                    .files
                    .get(&path.to_string_lossy().to_string())
                    .is_some_and(|usage| days_from_codex_source_rows(&source.rows) == usage.days)
        })
    {
        return None;
    }
    let source_rows = cache
        .codex_fork_rows
        .get(&path.to_string_lossy().to_string())
        .cloned()
        .or_else(|| read_source_rows(path, scan_range).ok())?;
    let recoverable_cache = cache
        .codex_source_rows
        .get(&path.to_string_lossy().to_string())
        .filter(|cached| {
            row_cache_needs_recovery(cached, &source_rows)
                && (cached.prefix_hash != 0 || native_prefix_matches(cached, &source_rows))
                && row_cache_matches(path, &metadata, cached)
        })
        .cloned();
    Some(CodexSourceRowPlan {
        metadata,
        source_rows,
        recoverable_cache,
    })
}

fn native_prefix_matches(cached: &CodexSourceRowCache, rows: &[CodexSourceUsageRow]) -> bool {
    let prefix: Vec<_> = rows
        .iter()
        .filter(|row| row.source_end_offset <= cached.size)
        .collect();
    cached.rows.len() == prefix.len()
        && cached.rows.iter().zip(prefix).all(|(old, current)| {
            let mut current = current.clone();
            current.pricing.clone_from(&old.pricing);
            *old == current
        })
}

/// Apply a source-row plan: recover cached pricing when required, retain the
/// resulting rows as evidence, and unprice the file's day map when recovery
/// left appended rows unvalidated.
fn apply_codex_source_row_plan(cache: &mut CostUsageCache, key: &str, plan: CodexSourceRowPlan) {
    let CodexSourceRowPlan {
        metadata,
        source_rows,
        recoverable_cache,
    } = plan;
    let requires_recovery = recoverable_cache
        .as_ref()
        .is_some_and(|cached| row_cache_needs_recovery(cached, &source_rows));
    let rows = if requires_recovery {
        let cached = recoverable_cache.expect("recovery cache checked above");
        recover_rows(&cached.rows, &source_rows, cached.size)
    } else {
        source_rows
    };
    let Some(file_identity) = JsonlScanner::codex_file_identity(Path::new(key), &metadata) else {
        return;
    };
    // The bounded parser has already validated the file identity and append
    // cursor. Retaining its rows must not reread an entire large transcript.
    let source_cache = CodexSourceRowCache {
        file_identity,
        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        mtime_unix_ms: system_time_to_unix_ms(metadata.modified().ok()),
        prefix_hash: 0,
        rows,
    };
    if requires_recovery && let Some(usage) = cache.files.get_mut(key) {
        // Rebuild from recovered rows even when a new row is intentionally
        // unresolved. Leaving the normal parser's model-priced days in place
        // would silently price an appended row that has no validated
        // historical evidence.
        usage.days = days_from_codex_source_rows(&source_cache.rows);
    }
    cache
        .codex_source_rows
        .insert(key.to_string(), source_cache);
}

/// Persist the Codex cache. Budget pruning subtracts each dropped file's
/// plain day totals from `days`, which cannot undo a Priority overlay and
/// would leave stale `-priority` totals beside negative base totals. When
/// pruning dropped a file, rebuild the aggregate from the retained files and
/// persist that instead.
pub(super) fn save_codex_cache(cache: &mut CostUsageCache, cache_root: Option<&Path>) {
    let files_before = cache.files.len();
    JsonlScanner::save_cache(ProviderId::Codex, cache, cache_root);
    if cache.files.len() != files_before {
        rebuild_cache_days(cache);
        JsonlScanner::save_cache(ProviderId::Codex, cache, cache_root);
    }
}

/// Path and presence of the configured trace database (upstream
/// `codexPriorityMetadataKey`); `None` when no database is configured.
fn codex_priority_metadata_key(scanner: &CostScanner) -> Option<String> {
    let path = scanner.codex_trace_database_path()?;
    let state = if path.exists() { "sqlite" } else { "missing" };
    Some(format!("{state}:{}", path.to_string_lossy()))
}

/// True when the persisted metadata key says `database_path` existed on the
/// last validated scan (upstream `previouslyObservedDatabase`).
fn codex_priority_database_previously_observed(
    persisted_key: Option<&str>,
    database_path: &Path,
) -> bool {
    persisted_key
        .and_then(|key| key.strip_prefix("sqlite:"))
        .is_some_and(|path| path == database_path.to_string_lossy())
}

/// True when a trace database appeared (or moved) since the last full scan.
/// Its evidence must reprice history even inside the debounce window, while
/// a database that went missing keeps the cached evidence (upstream
/// `codexPriorityMetadataChanged`).
pub(super) fn codex_priority_metadata_appeared(old: Option<&str>, new: Option<&str>) -> bool {
    matches!((old, new), (Some(old), Some(new)) if old != new && new.starts_with("sqlite:"))
}

/// Refresh the durable Priority-trace cursor before day totals are rebuilt.
/// A missing or unreadable trace database keeps the previous evidence, so a
/// transient failure never reprices history. Returns true when validation is
/// pending; the caller then keeps the persisted metadata key so the next scan
/// retries (upstream persists nothing for a pending pass).
fn resolve_codex_priority_evidence(
    scanner: &CostScanner,
    cache: &mut CostUsageCache,
    start_date: NaiveDate,
    cancel: Option<&AtomicBool>,
) -> bool {
    let Some(database_path) = scanner.codex_trace_database_path() else {
        return false;
    };
    // One day of slack covers the local/UTC offset at the window edge.
    let coverage_since_epoch = start_date
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
        .map_or(0, |start| start.timestamp() - 86_400)
        .max(0);
    let previously_observed = codex_priority_database_previously_observed(
        cache.codex_priority_metadata_key.as_deref(),
        &database_path,
    );
    let resolution = super::priority_trace::resolve_priority_turns(
        &database_path,
        cache.codex_priority_turns_cursor.take(),
        coverage_since_epoch,
        previously_observed,
        cancel,
    );
    cache.codex_priority_turns_cursor = resolution.cursor;
    if resolution.validation_pending {
        tracing::debug!("Codex priority trace validation is pending; retrying next scan");
    }
    resolution.validation_pending
}

pub(super) fn scan_codex_detailed_with_cache(
    scanner: &CostScanner,
    cancel: Option<&AtomicBool>,
    cache_root: Option<&Path>,
    sessions_dirs: &[PathBuf],
) -> (CostSummary, CostScanStats, CostUsageCache) {
    let mut summary = CostSummary::default();
    let mut stats = CostScanStats::default();
    let mut cache = JsonlScanner::load_cache(ProviderId::Codex, cache_root);
    if scanner.sessions_dirs_override.is_none()
        && let Some(base) = JsonlScanner::default_cache_root()
    {
        super::saved_history::load(&mut cache, sessions_dirs, &base);
    }
    let now = Utc::now();
    let today = crate::cost_reporting_period::cost_bucket_zone().date(now);
    // All-available history opens at the first existing partition or dated
    // flat rollout; with neither it is just today, so an empty tree costs no
    // directory probes.
    let earliest = (scanner.period == CostReportingPeriod::AllAvailable).then(|| {
        [
            first_codex_partition_date(sessions_dirs),
            earliest_flat_codex_day(sessions_dirs),
            super::saved_history::earliest_day(&cache),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(today)
    });
    let window = scanner.calendar_window(now, earliest);
    let (start_date, today) = (window.start, window.end);
    let range = CostUsageDayRange::new(start_date, today);
    let now_ms = unix_now_ms();

    summary.period_start = Some(start_date);
    summary.period_end = Some(today);

    rebuild_cache_days(&mut cache);
    let source_readable = source_roots_readable(scanner, &cache, sessions_dirs);
    let priority_metadata_key = codex_priority_metadata_key(scanner);
    let pending_scan = CodexPendingScanContext::new(&cache, &range, sessions_dirs);

    // A no-progress or source-error catch-up is terminal for background
    // synchronization. Keep the resumable queue and last validated report
    // intact until the user explicitly requests an app-driven refresh.
    if !pending_scan.is_incompatible
        && matches!(
            CodexPendingScanDisposition::before_scan(&cache, scanner.options.is_app_driven()),
            CodexPendingScanDisposition::PreservePause
        )
    {
        let mut summary = paused_codex_summary(
            &cache,
            start_date,
            today,
            &range,
            !pending_scan.is_incompatible,
        );
        append_pi_compatible_costs(scanner, &mut summary, cancel);
        summary.history_coverage_established =
            summary.history_coverage_established && !is_cancelled(cancel);
        summary.known_zero = summary.history_coverage_established && summary.sessions_count == 0;
        return (summary, stats, cache);
    }
    cache.codex_scan_pause_reason = None;
    cache.codex_pause_observation = None;

    let scan_range = &pending_scan.scan_range;

    // Debounce: rebuild from disk cache without re-walking session files.
    if source_readable
        && JsonlScanner::should_skip_cached_scan(&cache, scanner.options, now_ms)
        && !cache.codex_scan_incomplete
        && JsonlScanner::cache_covers_range(&cache, &range)
        && (!cache.days.is_empty() || !cache.files.is_empty())
        && !codex_priority_metadata_appeared(
            cache.codex_priority_metadata_key.as_deref(),
            priority_metadata_key.as_deref(),
        )
    {
        stats.used_cache_debounce = true;
        // A16 (upstream 0.48.0): cache hit within debounce = coverage established
        // when the cache has data and no catch-up is pending. Final publication
        // also waits for the cancellable Pi/OMP scan below.
        let cached_history_coverage_established = !cache.codex_scan_incomplete
            && cache.previous_report.is_none()
            && JsonlScanner::cache_covers_range(&cache, &range);
        let (cost, _) = add_codex_days_map_to_summary(&mut summary, &cache.days, &range);
        summary.total_cost_usd += cost;
        price_summary_from_source_rows(&mut summary, &cache, &range);
        // Session count is a display field; the cache holds far fewer files than u32::MAX.
        #[allow(clippy::cast_possible_truncation, reason = "cache file counts fit u32")]
        let sessions_count = cache
            .files
            .values()
            .filter(|usage| {
                usage.days.keys().any(|day| {
                    CostUsageDayRange::is_in_range(day, &range.since_key, &range.until_key)
                })
            })
            .count() as u32;
        summary.restored_sessions = saved_count(&cache, &range);
        summary.sessions_count = sessions_count + summary.restored_sessions;

        // Pi-compatible sessions are outside the Codex JSONL cache.
        // Skip when tests inject sessions roots — avoid scanning the real home tree.
        append_pi_compatible_costs(scanner, &mut summary, cancel);
        summary.history_coverage_established =
            cached_history_coverage_established && !is_cancelled(cancel);
        // Upstream 0.50.1 #2932: debounce cache hit with coverage
        // established but zero sessions in-range is a known-zero.
        summary.known_zero = summary.history_coverage_established && summary.sessions_count == 0;
        return (summary, stats, cache);
    }

    let established_report_before_scan = (!cache.codex_scan_incomplete
        && cache.previous_report.is_none()
        && (cache.scan_since_key.is_some() || !cache.days.is_empty() || !cache.files.is_empty()))
    .then(|| JsonlScanner::cached_cost_report_for_range(&cache, &range));

    // Keep per-file progress only while the source identity remains the same.
    // Explicit refreshes may resume the same bounded work, but a new root,
    // timezone or end date must not inherit this evidence.
    if cache.codex_pending_scan_root_paths != pending_scan.root_paths
        || cache.codex_pending_scan_timezone.as_deref() != Some(pending_scan.timezone.as_str())
        || cache.codex_pending_scan_until_key.as_deref() != Some(scan_range.scan_until_key.as_str())
    {
        cache.codex_pending_file_ranges.clear();
    }

    // Persist the source-bound work range before doing bounded work.
    cache.codex_pending_scan_since_key = Some(scan_range.scan_since_key.clone());
    cache.codex_pending_scan_until_key = Some(scan_range.scan_until_key.clone());
    cache.codex_pending_scan_root_paths = pending_scan.root_paths.clone();
    cache.codex_pending_scan_timezone = Some(pending_scan.timezone.clone());

    // Archiving moves a rollout between its date partition and the flat
    // archive. Follow those moves in the cache first, so a moved file keeps
    // its usage without a reread and never looks deleted.
    let flat_listing = CodexFlatListing::read(sessions_dirs, scan_range, &cache, cancel);
    relocate_moved_codex_rollouts(&mut cache, sessions_dirs, &flat_listing, scan_range);

    let cached_lineage = CodexLineagePlanner::new(&cache);
    let (mut candidates, discovery_complete) = scanner.collect_codex_candidates(
        sessions_dirs,
        scan_range,
        &cache,
        &cached_lineage,
        &flat_listing,
        cancel,
        &mut stats,
    );
    let discovery_complete = discovery_complete && source_readable;
    let candidate_limit = if scanner.options.codex_candidate_limit == 0 {
        usize::MAX
    } else {
        scanner.options.codex_candidate_limit
    };
    let refresh_byte_limit = if scanner.options.codex_max_scan_bytes_per_refresh <= 0 {
        i64::MAX
    } else {
        scanner.options.codex_max_scan_bytes_per_refresh
    };
    let per_file_limit = if scanner.options.codex_max_session_file_bytes <= 0 {
        i64::MAX
    } else {
        scanner.options.codex_max_session_file_bytes
    };
    let mut bytes_read_this_refresh = 0_i64;
    let mut pending_next = cache.codex_pending_paths.clone();
    let pending_paths_before_pass = cache.codex_pending_paths.clone();
    let mut invalidated_unsafe_lineage = false;
    prioritize_codex_pending_candidates(&mut candidates, &pending_paths_before_pass);
    defer_codex_locally_inferred_candidates(&mut candidates, &cache);
    if discovery_complete && !is_cancelled(cancel) {
        pending_next.retain(|path| {
            !cached_codex_file_is_complete_for_range(&cache, &cached_lineage, path, scan_range)
        });
    }

    // Admit one bounded set, inspect each admitted candidate once, and order
    // that set by lineage before reading token history. This makes cold
    // child-before-parent scans parent-first without a second parse pass.
    let deferred_candidates = candidates.split_off(candidate_limit.min(candidates.len()));
    let deferred_paths = deferred_candidates
        .into_iter()
        .map(|candidate| candidate.path)
        .collect::<Vec<_>>();
    let mut work_queue = Vec::with_capacity(candidates.len());
    let mut cancelled_during_preparation = Vec::new();
    for candidate in candidates {
        if is_cancelled(cancel) {
            cancelled_during_preparation.push(candidate.path);
            continue;
        }
        let key = candidate.path.to_string_lossy().to_string();
        stats.files_seen = stats.files_seen.saturating_add(1);
        stats.codex_metadata_read_paths.push(key);
        stats.codex_read_receipt.metadata_reads =
            stats.codex_read_receipt.metadata_reads.saturating_add(1);
        work_queue.push(CodexPreparedCandidate {
            session_metadata: JsonlScanner::read_codex_session_metadata(&candidate.path)
                .unwrap_or_default(),
            path: candidate.path,
            lineage_gate: CodexLineageGate::Eligible,
            parent_owner_expected: false,
        });
    }
    let mut unprocessed = Vec::new();
    let cancelled_before_plan = !cancelled_during_preparation.is_empty() || is_cancelled(cancel);
    let lineage_planner = if cancelled_before_plan {
        unprocessed.extend(work_queue.drain(..).map(|candidate| candidate.path));
        unprocessed.extend(cancelled_during_preparation);
        cached_lineage
    } else {
        let (planner, unsafe_cached_paths) = CodexLineagePlanner::plan_candidates_by_lineage(
            &cache,
            &mut work_queue,
            sessions_dirs,
            scan_range,
        );
        invalidated_unsafe_lineage = !unsafe_cached_paths.is_empty();
        if invalidated_unsafe_lineage {
            cache.previous_report = None;
        }
        invalidate_codex_unsafe_lineage(&mut cache, &unsafe_cached_paths);
        for path in unsafe_cached_paths {
            if !pending_next.contains(&path) {
                pending_next.push(path);
            }
        }
        planner
    };

    let mut incomplete_processed = Vec::new();
    let mut completed_file = false;
    for (index, candidate) in work_queue.iter().enumerate() {
        let refresh_remaining = refresh_byte_limit.saturating_sub(bytes_read_this_refresh);
        let allowance = per_file_limit.min(refresh_remaining);
        if is_cancelled(cancel) || allowance <= 0 {
            unprocessed.extend(
                work_queue[index..]
                    .iter()
                    .map(|candidate| candidate.path.clone()),
            );
            break;
        }

        let key = candidate.path.to_string_lossy().to_string();
        let previously_covered = codex_cache_covers_file(&cache, &key, scan_range);
        let outcome = scanner.parse_codex_file_bounded(
            &candidate.path,
            scan_range,
            &mut summary,
            &mut cache,
            cancel,
            &mut stats,
            Some(allowance),
            Some(candidate),
            &lineage_planner,
        );
        bytes_read_this_refresh = bytes_read_this_refresh.saturating_add(outcome.bytes_read.max(0));
        stats.codex_bytes_read = stats
            .codex_bytes_read
            .saturating_add(u64::try_from(outcome.bytes_read.max(0)).unwrap_or(u64::MAX));
        pending_next.retain(|pending| pending != &key);
        let observed_size = fs::metadata(&candidate.path)
            .ok()
            .map(|metadata| {
                #[allow(
                    clippy::cast_possible_wrap,
                    reason = "file sizes are clamped to i64::MAX"
                )]
                let size = metadata.len().min(i64::MAX as u64) as i64;
                size
            })
            .unwrap_or(0);
        let has_unconsumed_tail = cache
            .files
            .get(&key)
            .is_some_and(|usage| codex_logical_target_has_unconsumed_tail(observed_size, usage));
        let completely_consumed = outcome.is_complete && !has_unconsumed_tail;
        if (outcome.bytes_read > 0
            || completely_consumed
            || (previously_covered && outcome.reused_validated_prefix))
            && cache.files.contains_key(&key)
        {
            cache.codex_pending_file_ranges.insert(
                key.clone(),
                (scan_range.since_key.clone(), scan_range.until_key.clone()),
            );
        }
        completed_file |= completely_consumed
            && (outcome.bytes_read > 0 || pending_paths_before_pass.contains(&key))
            && cached_codex_file_is_complete_for_range(&cache, &lineage_planner, &key, scan_range);
        if !outcome.is_complete || has_unconsumed_tail {
            incomplete_processed.push(key);
            stats.files_deferred = stats.files_deferred.saturating_add(1);
        } else if let Some(plan) = codex_source_row_plan(&cache, &candidate.path, scan_range) {
            apply_codex_source_row_plan(&mut cache, &key, plan);
        }
    }
    unprocessed.extend(deferred_paths);
    stats.files_deferred = stats.files_deferred.saturating_add(
        u32::try_from(unprocessed.len().min(u32::MAX as usize)).unwrap_or(u32::MAX),
    );
    for path in unprocessed {
        let key = path.to_string_lossy().to_string();
        if !pending_next.contains(&key) {
            pending_next.push(key);
        }
    }
    pending_next.extend(incomplete_processed);

    let mut pruned_paths_pending = Vec::new();
    if discovery_complete && !is_cancelled(cancel) {
        pruned_paths_pending = missing_codex_cache_paths(&cache, sessions_dirs, scan_range);
        if scanner.options.is_app_driven() {
            reconcile_missing_codex_cache_files(&mut cache, sessions_dirs, scan_range);
            for path in &pending_paths_before_pass {
                if !Path::new(path).exists() {
                    cache.files.remove(path);
                }
            }
        } else {
            for path in &pruned_paths_pending {
                if !pending_next.contains(path) {
                    pending_next.push(path.clone());
                }
            }
        }
    }
    pending_next.retain(|path| {
        // An incomplete discovery is a source failure, not proof that a
        // queued path was pruned. Preserve the priority cursor verbatim so
        // the next explicit refresh can validate the source and resume it.
        if !discovery_complete
            || is_cancelled(cancel)
            || (!scanner.options.is_app_driven()
                && pruned_paths_pending.iter().any(|pending| pending == path))
        {
            return true;
        }
        Path::new(path).exists()
            && is_codex_path_in_scan_window(Path::new(path), sessions_dirs, scan_range)
    });
    if discovery_complete
        && !is_cancelled(cancel)
        && (pruned_paths_pending.is_empty() || scanner.options.is_app_driven())
    {
        pending_next.sort();
        pending_next.dedup();
    } else {
        // Preserve queue order while the source is unavailable or the
        // pass is cancelled; this is the durable priority cursor.
        let mut seen_pending = HashSet::new();
        pending_next.retain(|path| seen_pending.insert(path.clone()));
    }
    cache.codex_pending_paths = pending_next;
    cache.codex_scan_incomplete =
        !discovery_complete || is_cancelled(cancel) || !cache.codex_pending_paths.is_empty();
    let pending_disposition = CodexPendingScanDisposition::after_scan(
        &cache,
        discovery_complete,
        is_cancelled(cancel),
        !pruned_paths_pending.is_empty() && !scanner.options.is_app_driven(),
        bytes_read_this_refresh,
        completed_file,
    );
    let priority_validation_pending =
        resolve_codex_priority_evidence(scanner, &mut cache, start_date, cancel);
    if !is_cancelled(cancel) && !priority_validation_pending {
        cache.codex_priority_metadata_key = priority_metadata_key;
    }
    rebuild_cache_days(&mut cache);
    cache.last_scan_unix_ms = now_ms;
    if cache.codex_scan_incomplete {
        if pending_disposition.keeps_live_rows() {
            // Preserve live rows only when there is no validated report to
            // protect. v0.60.5 keeps the prior report for unresolved current
            // work; the current window is not authoritative until the fork
            // dependency is resolved.
            // The healthy files in this range are now validated. Keep the
            // incomplete flag as the coverage marker while acknowledging
            // the range so unchanged files stay on the cache fast path.
            cache.scan_since_key = Some(scan_range.scan_since_key.clone());
            cache.scan_until_key = Some(scan_range.scan_until_key.clone());
        } else if cache.previous_report.is_none() && !invalidated_unsafe_lineage {
            cache.previous_report = established_report_before_scan;
        }
        if !is_cancelled(cancel) {
            cache.codex_scan_pause_reason = if !discovery_complete {
                Some(CodexScanPauseReason::Error(
                    "Codex session source unavailable".to_string(),
                ))
            } else {
                pending_disposition.pause_reason()
            };
        }
    } else {
        cache.scan_since_key = Some(scan_range.scan_since_key.clone());
        cache.scan_until_key = Some(scan_range.scan_until_key.clone());
        cache.codex_pending_scan_since_key = None;
        cache.codex_pending_scan_until_key = None;
        cache.codex_pending_scan_root_paths.clear();
        cache.codex_pending_scan_timezone = None;
        cache.codex_pending_file_ranges.clear();
        cache.previous_report = None;
        cache.codex_scan_pause_reason = None;
    }
    cache.codex_pause_observation = cache
        .codex_scan_pause_reason
        .as_ref()
        .map(|_| super::pending_range::pause_observation(&cache));
    save_codex_cache(&mut cache, cache_root);

    // Build the current native summary from the complete decoded cache view,
    // including prior cached files that were not reread in this bounded pass.
    // A cancelled pass retains missing rows on disk for deletion
    // reconciliation, but must not publish those stale rows in its summary.
    let mut summary_cache = cache.clone();
    if is_cancelled(cancel) {
        summary_cache
            .files
            .retain(|path, _| Path::new(path).exists());
        rebuild_cache_days(&mut summary_cache);
    }
    let mut rebuilt = CostSummary {
        period_start: Some(start_date),
        period_end: Some(today),
        ..CostSummary::default()
    };
    let (native_cost, _) = add_codex_days_map_to_summary(&mut rebuilt, &summary_cache.days, &range);
    rebuilt.total_cost_usd += native_cost;
    price_summary_from_source_rows(&mut rebuilt, &summary_cache, &range);
    #[allow(clippy::cast_possible_truncation, reason = "cache file counts fit u32")]
    {
        rebuilt.sessions_count = summary_cache
            .files
            .values()
            .filter(|usage| {
                usage.days.keys().any(|day| {
                    CostUsageDayRange::is_in_range(day, &range.since_key, &range.until_key)
                })
            })
            .count() as u32;
    }
    rebuilt.sessions_count += saved_count(&summary_cache, &range);
    let cancelled_with_missing_cache_rows =
        is_cancelled(cancel) && cache.files.keys().any(|path| !Path::new(path).exists());
    let preserving_previous_report = cache.codex_scan_incomplete
        && cache.previous_report.is_some()
        && !cancelled_with_missing_cache_rows;
    let current_window_summary = (cache.codex_scan_incomplete && !is_cancelled(cancel))
        .then(|| codex_current_window_summary(&cache, &range, start_date, today))
        .flatten();
    let published_current_window = current_window_summary.is_some();
    summary = if let Some(summary) = current_window_summary {
        summary
    } else if preserving_previous_report {
        cache
            .previous_report
            .as_ref()
            .map(|report| summary_from_cached_report(report, start_date, today))
            .unwrap_or(rebuilt)
    } else {
        rebuilt
    };

    summary.restored_sessions = saved_count(&cache, &range);

    // OMP / pi-compatible agent sessions (upstream #2269). Dedup by entry id.
    // Skip when tests inject sessions roots — avoid scanning the real home tree.
    // A16 --provider-native-only: skip pi/OMP mirrors when disabled.
    append_pi_compatible_costs(scanner, &mut summary, cancel);

    // v0.56.1 #3279: only publish authoritative coverage after all
    // cancellable scan work, including Pi/OMP, has completed. Persistence
    // pruning may retain `previous_report`, but that must not make a
    // completed in-memory scan stale or make a cancelled partial scan look
    // complete.
    summary.history_coverage_established =
        !is_cancelled(cancel) && (!cache.codex_scan_incomplete || published_current_window);
    // Upstream 0.50.1 #2932: a completed scan with zero results is a
    // *known* zero. An incomplete scan may publish only a non-empty validated
    // current window; it must not infer zero from historical metadata alone.
    summary.known_zero = summary.history_coverage_established
        && summary.sessions_count == 0
        && (!cache.codex_scan_incomplete || published_current_window);

    (summary, stats, cache)
}

fn append_pi_compatible_costs(
    scanner: &CostScanner,
    summary: &mut CostSummary,
    cancel: Option<&AtomicBool>,
) {
    if !scanner.options.include_pi_sessions || scanner.sessions_dirs_override.is_some() {
        return;
    }

    let now = Utc::now();
    let cutoff = scanner.transcript_window(now, now.date_naive()).cutoff;
    let mut seen_pi = HashSet::new();
    crate::pi_session_cost::scan_pi_compatible_into(
        summary,
        crate::pi_session_cost::PiMappedProvider::Codex,
        cutoff,
        cancel,
        &mut seen_pi,
    );
}

fn saved_count(cache: &CostUsageCache, range: &CostUsageDayRange) -> u32 {
    u32::try_from(
        super::saved_history::active_rows(cache)
            .filter(|(_, rows)| {
                rows.iter().any(|row| {
                    CostUsageDayRange::is_in_range(&row.day_key, &range.since_key, &range.until_key)
                })
            })
            .count(),
    )
    .unwrap_or(u32::MAX)
}

fn source_roots_readable(scanner: &CostScanner, cache: &CostUsageCache, roots: &[PathBuf]) -> bool {
    if scanner.sessions_dirs_override.is_some() {
        return true;
    }
    let settings = Settings::load();
    let explicit =
        codex_sessions_dir_candidates(None, None, &settings.codex_custom_sessions_dirs, &[]);
    let optional_default = std::env::var_os("CODEX_HOME")
        .is_none()
        .then(|| dirs::home_dir().map(|home| home.join(".codex").join("sessions")))
        .flatten();
    let mut any_readable = false;
    for root in roots {
        match fs::read_dir(root) {
            Ok(_) => any_readable = true,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && optional_default.as_ref() == Some(root)
                    && !explicit.contains(root)
                    && !cache_has_codex_path_under(cache, root) => {}
            Err(_) => return false,
        }
    }
    any_readable
        && !cache.files.keys().any(|path| {
            let windows = path.replace('/', "\\").to_ascii_lowercase();
            (windows.starts_with(r"\\wsl.localhost\") || windows.starts_with(r"\\wsl$\"))
                && !roots.iter().any(|root| {
                    Path::new(path).starts_with(root)
                        || root.parent().is_some_and(|parent| {
                            Path::new(path).starts_with(parent.join("archived_sessions"))
                        })
                })
        })
}
