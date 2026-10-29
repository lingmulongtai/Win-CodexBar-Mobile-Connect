use super::*;
use crate::commands::UsageSpendSummary;
use codexbar::mobile_bridge::{MobileSpend, MobileSpendDay, MobileSpendRow};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn path() -> Option<std::path::PathBuf> {
    codexbar::settings::config_store_dir().map(|root| root.join("mobile-spend.json"))
}
fn cache() -> &'static Mutex<Option<MobileSpend>> {
    static CACHE: OnceLock<Mutex<Option<MobileSpend>>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(
            path()
                .filter(|path| std::fs::metadata(path).is_ok_and(|meta| meta.len() < 1024 * 1024))
                .and_then(|path| secure_file::read_string(&path).ok())
                .and_then(|raw| serde_json::from_str(&raw).ok()),
        )
    })
}

pub(crate) fn cached_spend(providers: &[MobileProvider]) -> Option<MobileSpend> {
    let settings = Settings::load();
    if !settings.cost_usage_enabled {
        return None;
    }
    let mut spend = cache().lock().ok()?.clone()?;
    if spend.reporting_period != settings.spend_dashboard_period.to_string() {
        return None;
    }
    spend.rows.retain(|row| {
        providers
            .iter()
            .any(|provider| provider.id == row.provider_id)
    });
    Some(spend)
}

pub(crate) fn publish_spend(summary: &UsageSpendSummary) {
    let spend = project_spend(summary, chrono::Utc::now().timestamp());
    let Ok(mut cached) = cache().lock() else {
        return;
    };
    if let Some(path) = path()
        && let Ok(raw) = serde_json::to_string(&spend)
        && secure_file::write_string(&path, &raw).is_err()
    {
        tracing::warn!("Cannot persist Mobile Connect spend totals");
    }
    *cached = Some(spend);
}

fn project_spend(summary: &UsageSpendSummary, now: i64) -> MobileSpend {
    let amount = |amount: Option<f64>| {
        amount.filter(|value| value.is_finite() && (0.0..=1e12).contains(value))
    };
    let tokens = |tokens: Option<u64>| tokens.filter(|value| *value <= i64::MAX as u64);
    let rows = if summary.cost_tracking_enabled {
        summary
            .rows
            .iter()
            .filter(|row| {
                row.included_in_overview && ANDROID_PROVIDERS.contains(&row.provider_id.as_str())
            })
            .take(32)
            .map(|row| {
                let activity = summary
                    .token_activity
                    .iter()
                    .find(|source| source.provider_id == row.provider_id);
                let costs: std::collections::BTreeMap<_, _> = summary
                    .trend
                    .as_ref()
                    .into_iter()
                    .flat_map(|trend| trend.daily.iter())
                    .filter(|point| point.source_id == row.provider_id)
                    .map(|point| (point.day.clone(), amount(point.cost_usd)))
                    .collect();
                let days: std::collections::BTreeSet<_> = costs
                    .keys()
                    .cloned()
                    .chain(
                        activity
                            .into_iter()
                            .flat_map(|source| source.days.iter().map(|day| day.day.clone())),
                    )
                    .collect();
                let daily = days
                    .iter()
                    .rev()
                    .take(30)
                    .rev()
                    .map(|day| {
                        let token_day = activity
                            .and_then(|source| source.days.iter().find(|entry| &entry.day == day));
                        MobileSpendDay {
                            day: day.clone(),
                            tokens: tokens(token_day.and_then(|entry| entry.total_tokens)),
                            cost_usd: costs.get(day).copied().flatten(),
                            is_scanned: token_day.is_some_and(|entry| entry.is_scanned),
                        }
                    })
                    .collect();
                MobileSpendRow {
                    provider_id: row.provider_id.clone(),
                    currency: row.currency.clone(),
                    refreshing: row.refreshing,
                    seven_day: amount(row.seven_day),
                    thirty_day: amount(row.thirty_day),
                    // The current Mobile protocol has no partial-cost flag.
                    // Do not present an incomplete subtotal as a full amount.
                    period_cost: if row.provider_id == summary.contract.provider_id
                        && (summary.contract.price_coverage.unpriced > 0
                            || summary.contract.price_coverage.unmetered > 0)
                    {
                        None
                    } else {
                        amount(row.period_cost)
                    },
                    seven_day_tokens: tokens(row.seven_day_tokens),
                    thirty_day_tokens: tokens(row.thirty_day_tokens),
                    period_tokens: tokens(row.period_tokens),
                    tokens_lower_bound: row.seven_day_tokens_lower_bound
                        || row.thirty_day_tokens_lower_bound,
                    daily,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    MobileSpend {
        updated_at_epoch_seconds: summary
            .updated_at
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map_or(now, |value| value.timestamp()),
        reporting_period: summary.reporting_period.clone(),
        time_zone: summary.dashboard_timezone.clone(),
        cost_tracking_enabled: summary.cost_tracking_enabled,
        rows,
    }
}

/// Existing spend scans run once per PC, never once per connected device.
pub(crate) fn schedule_spend_refresh(app: &tauri::AppHandle) {
    let settings = Settings::load();
    if !settings.cost_usage_enabled {
        return;
    }
    let sharing = app.state::<MobileSharing>();
    if !sharing
        .0
        .try_lock()
        .is_ok_and(|runtime| runtime.config.as_ref().is_some_and(|config| config.enabled))
    {
        return;
    }
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let Ok(mut last) = LAST.get_or_init(|| Mutex::new(None)).lock() else {
        return;
    };
    if last.is_some_and(|at| at.elapsed() < Duration::from_secs(300)) {
        return;
    }
    *last = Some(Instant::now());
    static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        struct RunningGuard;
        impl Drop for RunningGuard {
            fn drop(&mut self) {
                RUNNING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _running = RunningGuard;
        if crate::commands::get_local_usage_spend_summary(
            app.state::<Mutex<AppState>>(),
            Some(settings.spend_dashboard_period.to_string()),
            Some(false),
        )
        .await
        .is_err()
        {
            tracing::warn!("Mobile Connect spend refresh unavailable");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_does_not_serialize_conversations_or_paths() {
        let summary = UsageSpendSummary {
            rows: Vec::new(),
            contract: codexbar::spend_contract::SpendContract::empty(
                "private-project-and-conversation",
                Default::default(),
            ),
            reporting_period: "rolling:30".into(),
            reporting_day: "2026-10-17".into(),
            dashboard_timezone: "UTC".into(),
            updated_at: None,
            cost_tracking_enabled: false,
            token_activity: Vec::new(),
            trend: None,
        };
        let value = project_spend(&summary, 1_700_000_000);
        assert!(!value.cost_tracking_enabled);
        assert!(value.rows.is_empty());
        let json = serde_json::to_string(&value).unwrap();
        assert!(!json.contains("contract"));
        assert!(!json.contains("conversation"));
        assert!(!json.contains("project"));
    }

    #[test]
    fn daily_projection_preserves_unknown_zeros_and_token_floors() {
        use crate::commands::{TokenActivitySource, UsageSpendRow};
        use codexbar::cost_scanner::TokenActivityDay;
        let summary = UsageSpendSummary {
            rows: vec![UsageSpendRow {
                provider_id: "codex".into(),
                display_name: "private display name".into(),
                seven_day: Some(0.0),
                thirty_day: None,
                seven_day_estimate: None,
                thirty_day_estimate: None,
                seven_day_tokens: Some(5),
                thirty_day_tokens: None,
                seven_day_tokens_lower_bound: true,
                thirty_day_tokens_lower_bound: false,
                period_cost: None,
                period_tokens: None,
                currency: "USD".into(),
                source: "private/path".into(),
                included_in_overview: true,
                daily: Vec::new(),
                refreshing: false,
                history_paused: false,
                stale_updated_at: None,
            }],
            contract: codexbar::spend_contract::SpendContract::empty(
                "private-contract",
                Default::default(),
            ),
            reporting_period: "rolling:30".into(),
            reporting_day: "2026-10-17".into(),
            dashboard_timezone: "UTC".into(),
            updated_at: None,
            cost_tracking_enabled: true,
            token_activity: vec![TokenActivitySource {
                provider_id: "codex".into(),
                days: vec![
                    TokenActivityDay {
                        day: "2026-10-16".into(),
                        total_tokens: None,
                        is_scanned: true,
                    },
                    TokenActivityDay {
                        day: "2026-10-17".into(),
                        total_tokens: Some(0),
                        is_scanned: true,
                    },
                ],
            }],
            trend: None,
        };
        let mut summary = summary;
        summary.updated_at = Some("2023-11-14T22:13:20Z".into());
        summary.contract.provider_id = "codex".into();
        summary.contract.price_coverage.unpriced = 1;
        summary.rows[0].period_cost = Some(5.0);
        let projection = project_spend(&summary, 1_700_000_100);
        assert_eq!(projection.updated_at_epoch_seconds, 1_700_000_000);
        assert_eq!(projection.rows[0].period_cost, None);
        summary.contract.price_coverage.unpriced = 0;
        assert_eq!(
            project_spend(&summary, 1_700_000_100).rows[0].period_cost,
            Some(5.0)
        );
        assert_eq!(projection.rows[0].seven_day, Some(0.0));
        assert_eq!(projection.rows[0].thirty_day, None);
        assert!(projection.rows[0].tokens_lower_bound);
        assert_eq!(projection.rows[0].daily[0].tokens, None);
        assert_eq!(projection.rows[0].daily[1].tokens, Some(0));
        assert!(
            !serde_json::to_string(&projection)
                .unwrap()
                .contains("private")
        );
    }
}
