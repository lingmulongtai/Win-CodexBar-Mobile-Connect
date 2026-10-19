use super::{PeerView, view};
use crate::commands::{
    DailyCostPoint, DailyTokenPoint, ProviderChartData, ProviderLocalUsageSummary,
    ProviderUsageSnapshot, UsageSpendDailyPoint, UsageSpendRow, UsageSpendSummary,
};
use codexbar::{
    core::ProviderId,
    cost_reporting_period::{CostReportingPeriod, CostTimeZone},
    mobile_bridge::{MobileProvider, MobileQuotaHistory},
    spend_contract::SpendContract,
};
use serde_json::json;

fn timestamp(time: i64) -> String {
    chrono::DateTime::from_timestamp(time, 0)
        .map(|time| time.to_rfc3339())
        .unwrap_or_default()
}

fn source_id(view: &PeerView, provider: &MobileProvider) -> String {
    format!(
        "{}|{}",
        view.source_id,
        provider
            .history_scope_id
            .as_deref()
            .or(provider.subject_id.as_deref())
            .unwrap_or("unknown")
    )
}

pub(super) fn providers(view: &PeerView) -> Vec<ProviderUsageSnapshot> {
    view.snapshot.iter().flat_map(|s| &s.providers).filter_map(|p| {
        let id = ProviderId::from_cli_name(&p.id)?;
        let window = |w: &codexbar::mobile_bridge::MobileWindow| json!({
            "usedPercent": w.used_fraction * 100.0,
            "remainingPercent": (1.0 - w.used_fraction) * 100.0,
            "windowMinutes": w.window_duration_seconds.map(|v| v / 60),
            "resetsAt": w.resets_at_epoch_seconds.map(timestamp),
            "isExhausted": w.used_fraction >= 1.0,
        });
        let state = match p.status.as_str() {
            "ready" if view.error.is_none() => "ready",
            "needsAuthentication" => "needsAuthentication",
            "expiredSession" => "expiredSession",
            "localRuntimeOffline" => "localRuntimeOffline",
            _ => "unknown",
        };
        let cost = p.budget.as_ref().map(|b| json!({
            "used": b.used, "limit": b.limit, "remaining": (b.limit - b.used).max(0.0),
            "currencyCode": b.currency,
            "balance": p.balance.as_ref().filter(|v| v.currency == b.currency).map(|v| v.amount),
        })).or_else(|| p.balance.as_ref().map(|b| json!({"used": 0.0, "currencyCode": b.currency, "balance": b.amount})));
        // Only validated display aggregates are mapped. No local identity, plan,
        // credentials or scanner results are consulted as fallback.
        serde_json::from_value(json!({
            "providerId": p.id, "displayName": id.display_name(),
            "sourceLabel": source_id(view, p), "updatedAt": timestamp(p.updated_at_epoch_seconds),
            "primary": p.windows.first().map(&window).unwrap_or_else(|| json!({"usedPercent": 0.0, "isInformational": true})),
            "primaryLabel": p.windows.first().map(|w| &w.label),
            "secondary": p.windows.get(1).map(&window), "secondaryLabel": p.windows.get(1).map(|w| &w.label),
            "extraRateWindows": p.windows.iter().enumerate().skip(2).map(|(i,w)| json!({"id": format!("peer-window-{i}"), "title": w.label, "window": window(w)})).collect::<Vec<_>>(),
            "inventory": p.reset_credits.iter().map(|credits| json!({"id": "reset-credits", "title": "Reset credits", "availableCount": credits.available_count, "nextExpiresAt": credits.expires_at_epoch_seconds.iter().min().copied().map(timestamp)})).collect::<Vec<_>>(),
            "cost": cost, "paceAuthoritative": false, "errorState": state,
            "error": view.error.clone().or_else(|| (p.status != "ready").then(|| format!("Sharing PC: {}", p.status))),
        })).ok()
    }).collect()
}

fn provider<'a>(view: &'a PeerView, id: &str) -> Option<&'a MobileProvider> {
    view.snapshot
        .as_ref()?
        .providers
        .iter()
        .find(|p| p.id == id)
}

fn usage(p: &MobileProvider) -> Option<ProviderLocalUsageSummary> {
    let u = p.local_usage.as_ref()?;
    Some(ProviderLocalUsageSummary {
        today_cost: u.today_cost_usd,
        thirty_day_cost: u.thirty_day_cost_usd,
        thirty_day_tokens: u.thirty_day_tokens,
        period_cost: u.period_cost_usd,
        period_tokens: u.period_tokens,
        reporting_period: u.reporting_period.clone().unwrap_or_default(),
        latest_tokens: u.today_tokens,
        top_model: u.top_model.clone(),
        estimate_note: "Shared PC log totals; independent of quota account".into(),
        token_cost_updated_at_ms: u.updated_at_epoch_seconds.saturating_mul(1000),
        incomplete_request_count: u.incomplete_request_count,
    })
}

pub(crate) fn local_usage(
    app: &tauri::AppHandle,
    id: &str,
) -> Option<Option<ProviderLocalUsageSummary>> {
    view(app).map(|v| provider(&v, id).and_then(usage))
}

pub(crate) fn chart(app: &tauri::AppHandle, id: &str) -> Option<ProviderChartData> {
    view(app).map(|v| {
        let daily = v
            .snapshot
            .as_ref()
            .and_then(|s| s.spend.as_ref())
            .and_then(|s| s.rows.iter().find(|r| r.provider_id == id))
            .map(|r| r.daily.as_slice())
            .unwrap_or_default();
        ProviderChartData {
            provider_id: id.to_owned(),
            cost_history: daily
                .iter()
                .map(|d| DailyCostPoint {
                    date: d.day.clone(),
                    value: d.cost_usd,
                    incomplete_request_count: None,
                })
                .collect(),
            tokens_history: daily
                .iter()
                .filter_map(|d| {
                    d.tokens.map(|tokens| DailyTokenPoint {
                        date: d.day.clone(),
                        tokens,
                    })
                })
                .collect(),
            local_usage: provider(&v, id).and_then(usage),
            credits_history: vec![],
            usage_breakdown: vec![],
            quota_window_history: None,
            tokens_incomplete: daily.iter().any(|d| !d.is_scanned || d.tokens.is_none()),
        }
    })
}

pub(crate) fn history(
    app: &tauri::AppHandle,
    id: &str,
    updated: &str,
    expected_source: &str,
) -> Option<Vec<MobileQuotaHistory>> {
    view(app).map(|v| {
        provider(&v, id)
            .filter(|p| {
                timestamp(p.updated_at_epoch_seconds) == updated
                    && source_id(&v, p) == expected_source
            })
            .map(|p| p.history.clone())
            .unwrap_or_default()
    })
}

pub(crate) fn spend(app: &tauri::AppHandle) -> Option<UsageSpendSummary> {
    view(app).map(|v| spend_for(&v))
}

pub(super) fn spend_for(view: &PeerView) -> UsageSpendSummary {
    let spend = view.snapshot.as_ref().and_then(|s| s.spend.as_ref());
    let period = spend.map_or_else(|| "rolling:30".to_owned(), |s| s.reporting_period.clone());
    let zone = spend.map_or_else(|| "UTC".to_owned(), |s| s.time_zone.clone());
    let at = spend.and_then(|s| chrono::DateTime::from_timestamp(s.updated_at_epoch_seconds, 0));
    let day = at
        .map(|at| {
            if let Ok(offset) = zone.parse::<chrono::FixedOffset>() {
                at.with_timezone(&offset).date_naive()
            } else {
                CostTimeZone::from_identifier(&zone).date(at)
            }
        })
        .map(|day| day.to_string())
        .unwrap_or_default();
    UsageSpendSummary {
        rows: spend
            .into_iter()
            .flat_map(|s| &s.rows)
            .map(|r| UsageSpendRow {
                provider_id: r.provider_id.clone(),
                display_name: ProviderId::from_cli_name(&r.provider_id)
                    .map_or_else(|| r.provider_id.clone(), |id| id.display_name().to_owned()),
                seven_day: r.seven_day,
                thirty_day: r.thirty_day,
                seven_day_estimate: None,
                thirty_day_estimate: None,
                seven_day_tokens: r.seven_day_tokens,
                thirty_day_tokens: r.thirty_day_tokens,
                seven_day_tokens_lower_bound: r.tokens_lower_bound,
                thirty_day_tokens_lower_bound: r.tokens_lower_bound,
                period_cost: r.period_cost,
                period_tokens: r.period_tokens,
                currency: r.currency.clone(),
                source: view.source_id.clone(),
                included_in_overview: true,
                daily: r
                    .daily
                    .iter()
                    .filter_map(|d| {
                        d.cost_usd.map(|amount| UsageSpendDailyPoint {
                            day: d.day.clone(),
                            amount,
                        })
                    })
                    .collect(),
                refreshing: r.refreshing,
                stale_updated_at: (view.error.is_some() || r.refreshing)
                    .then(|| at.map(|at| at.to_rfc3339()))
                    .flatten(),
            })
            .collect(),
        contract: SpendContract::empty(
            "codex",
            CostReportingPeriod::parse(&period).unwrap_or_default(),
        ),
        reporting_period: period,
        reporting_day: day,
        dashboard_timezone: zone,
        cost_tracking_enabled: spend.is_some_and(|s| s.cost_tracking_enabled),
        token_activity: vec![],
        trend: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> PeerView {
        PeerView {
            name: "Home".into(),
            source_id: "peer:home:2".into(), error: None,
            snapshot: Some(serde_json::from_value(json!({
                "schemaVersion": 1, "source": "win-codexbar", "generatedAtEpochSeconds": 1792432800,
                "providers": [{"id": "codex", "updatedAtEpochSeconds": 1792432800,
                    "status": "ready", "historyScopeId": "account-a",
                    "windows": [{"label": "Weekly", "usedFraction": 0.42}],
                    "localUsage": {"updatedAtEpochSeconds": 1792432800, "todayDate": "2026-10-20", "todayTokens": 900, "todayCostUsd": 1.5, "reportingPeriod": "rolling:7", "periodTokens": 1800}
                }],
                "spend": {"updatedAtEpochSeconds": 1792432800, "reportingPeriod": "rolling:7", "timeZone": "+09:00", "costTrackingEnabled": true,
                    "rows": [{"providerId": "codex", "currency": "USD", "refreshing": false, "periodTokens": 1800, "tokensLowerBound": true,
                        "daily": [{"day": "2026-10-20", "tokens": null, "costUsd": null, "isScanned": false}]}]}
            })).unwrap()),
        }
    }

    #[test]
    fn remote_projection_never_supplies_local_account_or_unknown_zero_usage() {
        let mut view = sample();
        let projected = providers(&view).remove(0);
        assert_eq!(projected.primary.used_percent, 42.0);
        assert_eq!(projected.primary_label.as_deref(), Some("Weekly"));
        assert!(projected.account_email.is_none() && projected.plan_name.is_none());
        assert_eq!(projected.source_label, "peer:home:2|account-a");
        view.snapshot.as_mut().unwrap().providers[0].windows.clear();
        assert!(providers(&view)[0].primary.is_informational);
        view.error = Some("Offline".into());
        assert!(providers(&view)[0].error_state.is_problem());
        assert_eq!(providers(&view)[0].error.as_deref(), Some("Offline"));
    }

    #[test]
    fn sender_period_and_unknown_costs_survive_without_local_scanner_fallback() {
        let view = sample();
        let summary = spend_for(&view);
        assert_eq!(summary.reporting_period, "rolling:7");
        assert_eq!(summary.dashboard_timezone, "+09:00");
        assert_eq!(summary.reporting_day, "2026-10-20");
        assert_eq!(summary.rows[0].period_tokens, Some(1800));
        assert!(summary.rows[0].period_cost.is_none());
        assert!(summary.rows[0].daily.is_empty());
        assert!(summary.contract.known_cost_usd.is_none());
        assert!(summary.contract.projects.is_empty() && summary.token_activity.is_empty());
        let usage = usage(&view.snapshot.unwrap().providers[0]).unwrap();
        assert_eq!(usage.today_cost, Some(1.5));
        assert_eq!(usage.period_tokens, Some(1800));
        assert_eq!(usage.latest_tokens, Some(900));
        assert!(summary.rows[0].stale_updated_at.is_none());
    }
}
