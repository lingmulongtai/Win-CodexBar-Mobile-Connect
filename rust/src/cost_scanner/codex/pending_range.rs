use super::*;

pub(super) struct CodexPendingScanContext {
    pub(super) scan_range: CostUsageDayRange,
    pub(super) is_incompatible: bool,
    pub(super) root_paths: Vec<String>,
    pub(super) timezone: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexPendingScanDisposition {
    Proceed,
    PreservePause,
    Complete,
    Cancelled,
    SourceUnavailable,
    UnresolvedForks,
    NoProgress,
    Pending,
}

impl CodexPendingScanDisposition {
    pub(super) fn before_scan(cache: &CostUsageCache, is_app_driven: bool) -> Self {
        if is_app_driven || !cache.codex_scan_incomplete {
            return Self::Proceed;
        }
        if cache.codex_scan_pause_reason.is_none() {
            return Self::Proceed;
        }
        if matches!(
            cache.codex_scan_pause_reason,
            Some(CodexScanPauseReason::NoProgress)
        ) && codex_only_unresolved_forks_pending(cache)
        {
            Self::Proceed
        } else {
            Self::PreservePause
        }
    }

    pub(super) fn after_scan(
        cache: &CostUsageCache,
        discovery_complete: bool,
        cancelled: bool,
        has_pruned_paths: bool,
        bytes_read: i64,
        completed_file: bool,
    ) -> Self {
        if !cache.codex_scan_incomplete {
            Self::Complete
        } else if cancelled {
            Self::Cancelled
        } else if !discovery_complete {
            Self::SourceUnavailable
        } else if codex_only_unresolved_forks_pending(cache) {
            Self::UnresolvedForks
        } else if has_pruned_paths
            || (bytes_read == 0 && !completed_file && !cache.codex_pending_paths.is_empty())
        {
            Self::NoProgress
        } else {
            Self::Pending
        }
    }

    pub(super) fn pause_reason(self) -> Option<CodexScanPauseReason> {
        match self {
            Self::SourceUnavailable => Some(CodexScanPauseReason::Error(
                "Codex session source unavailable".to_string(),
            )),
            Self::NoProgress => Some(CodexScanPauseReason::NoProgress),
            Self::Proceed
            | Self::PreservePause
            | Self::Complete
            | Self::Cancelled
            | Self::UnresolvedForks
            | Self::Pending => None,
        }
    }

    pub(super) fn keeps_live_rows(self) -> bool {
        matches!(self, Self::UnresolvedForks)
    }
}

impl CodexPendingScanContext {
    pub(super) fn new(
        cache: &CostUsageCache,
        range: &CostUsageDayRange,
        sessions_dirs: &[PathBuf],
        is_app_driven: bool,
    ) -> Self {
        let root_paths = codex_scan_root_keys(sessions_dirs);
        let timezone = crate::cost_reporting_period::cost_bucket_zone().identifier();
        let is_compatible = !is_app_driven
            && cache.codex_scan_incomplete
            && codex_pending_scan_context_is_compatible(
                cache,
                &range.scan_until_key,
                &root_paths,
                &timezone,
            );
        let is_incompatible = cache.codex_scan_incomplete
            && codex_pending_scan_context_is_known(cache)
            && !is_compatible;
        let scan_since_key = if is_compatible {
            std::cmp::min(
                cache
                    .codex_pending_scan_since_key
                    .as_deref()
                    .unwrap_or(range.scan_since_key.as_str()),
                range.scan_since_key.as_str(),
            )
            .to_string()
        } else {
            range.scan_since_key.clone()
        };

        Self {
            scan_range: CostUsageDayRange {
                // Parse the entire retained catch-up window even when the
                // current caller requested a narrower report.
                since_key: scan_since_key.clone(),
                until_key: range.scan_until_key.clone(),
                scan_since_key,
                scan_until_key: range.scan_until_key.clone(),
            },
            is_incompatible,
            root_paths,
            timezone,
        }
    }
}

/// File-level catch-up progress is reusable only for a range actually parsed
/// for that file. The caller first binds the pending context to current roots,
/// timezone and end date before consulting these entries.
pub(super) fn codex_cache_covers_file(
    cache: &CostUsageCache,
    path: &str,
    range: &CostUsageDayRange,
) -> bool {
    if let Some((since, until)) = cache.codex_pending_file_ranges.get(path) {
        return since <= &range.since_key && until >= &range.until_key;
    }
    // While catch-up is pending the global range may have been advanced for
    // live display, or entries may have been replaced by narrower parses.
    // Only explicit per-file evidence is safe in that state.
    !cache.codex_scan_incomplete && JsonlScanner::cache_covers_range(cache, range)
}

pub(super) fn codex_only_unresolved_forks_pending(cache: &CostUsageCache) -> bool {
    !cache.codex_pending_paths.is_empty()
        && cache.codex_pending_paths.iter().all(|path| {
            cache
                .files
                .get(path)
                .is_some_and(|usage| usage.codex_unresolved_fork_parent)
        })
}

pub(super) fn codex_cache_has_validated_state(cache: &CostUsageCache) -> bool {
    cache.scan_since_key.is_some()
        || cache.scan_until_key.is_some()
        || cache.previous_report.is_some()
        || !cache.files.is_empty()
        || !cache.days.is_empty()
        || !cache.codex_pending_paths.is_empty()
        || !cache.codex_source_rows.is_empty()
        || cache.codex_pending_scan_since_key.is_some()
}

fn codex_scan_root_keys(sessions_dirs: &[PathBuf]) -> Vec<String> {
    let mut roots = sessions_dirs
        .iter()
        .map(|root| {
            let resolved = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            let key = resolved.to_string_lossy().to_string();
            if cfg!(windows) {
                key.to_ascii_lowercase()
            } else {
                key
            }
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

fn codex_pending_scan_context_is_compatible(
    cache: &CostUsageCache,
    scan_until_key: &str,
    root_paths: &[String],
    timezone: &str,
) -> bool {
    cache
        .codex_pending_scan_since_key
        .as_deref()
        .is_some_and(|since| CostUsageDayRange::parse_day_key(since).is_some())
        && cache.codex_pending_scan_until_key.as_deref() == Some(scan_until_key)
        && cache.codex_pending_scan_root_paths == root_paths
        && cache.codex_pending_scan_timezone.as_deref() == Some(timezone)
}

fn codex_pending_scan_context_is_known(cache: &CostUsageCache) -> bool {
    cache.codex_pending_scan_since_key.is_some()
        || cache.codex_pending_scan_until_key.is_some()
        || !cache.codex_pending_scan_root_paths.is_empty()
        || cache.codex_pending_scan_timezone.is_some()
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn completed_empty_file_is_progress_but_unconsumed_tail_is_not() {
        let cache = CostUsageCache {
            codex_scan_incomplete: true,
            codex_pending_paths: vec!["still-pending".to_string()],
            ..CostUsageCache::default()
        };
        assert_eq!(
            CodexPendingScanDisposition::after_scan(&cache, true, false, false, 0, true),
            CodexPendingScanDisposition::Pending
        );
        assert_eq!(
            CodexPendingScanDisposition::after_scan(&cache, true, false, false, 0, false),
            CodexPendingScanDisposition::NoProgress
        );
        assert_eq!(
            CodexPendingScanDisposition::after_scan(&cache, true, false, true, 0, true),
            CodexPendingScanDisposition::NoProgress
        );
    }
}
