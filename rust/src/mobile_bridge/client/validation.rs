//! Keep the desktop receiver aligned with Android's DesktopBridgeClient contract.

use super::{MobileSnapshot, PeerClientError, SOURCE};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::collections::HashSet;

const YEAR: i64 = 366 * 86400;

fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty()
        && value.encode_utf16().count() <= max
        && !value.chars().any(char::is_control)
}

fn scope(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn currency(value: &str) -> bool {
    (1..=8).contains(&value.len()) && value.bytes().all(|c| c.is_ascii_alphabetic())
}

fn date(value: &str) -> bool {
    value.len() == 10
        && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .is_ok_and(|date| date.to_string() == value)
}

fn period(value: &str) -> bool {
    matches!(value, "all" | "month-to-date")
        || value
            .strip_prefix("rolling:")
            .and_then(|days| days.parse::<u16>().ok())
            .is_some_and(|days| (1..=365).contains(&days) && value == format!("rolling:{days}"))
}

fn zone(value: &str) -> bool {
    if !text(value, 80) {
        return false;
    }
    value.parse::<chrono_tz::Tz>().is_ok()
        || [
            Some(value),
            value.strip_prefix("UTC"),
            value.strip_prefix("GMT"),
            value.strip_prefix("UT"),
        ]
        .into_iter()
        .flatten()
        .any(|offset| offset.parse::<chrono::FixedOffset>().is_ok())
}

fn money(value: Option<f64>) -> bool {
    value.is_none_or(|value| value.is_finite() && (0.0..=1e12).contains(&value))
}

fn tokens(value: Option<u64>) -> bool {
    value.is_none_or(|value| value <= i64::MAX as u64)
}

pub(super) fn validate_snapshot(
    snapshot: &MobileSnapshot,
    now: i64,
) -> Result<(), PeerClientError> {
    if valid_snapshot(snapshot, now) {
        Ok(())
    } else {
        Err(PeerClientError::InvalidSnapshot)
    }
}

fn valid_snapshot(snapshot: &MobileSnapshot, now: i64) -> bool {
    if snapshot.schema_version != 1
        || snapshot.source != SOURCE
        || snapshot.generated_at_epoch_seconds.abs_diff(now) > 120
        || snapshot.providers.len() > 32
    {
        return false;
    }
    let future = now.saturating_add(120);
    let expiry = now.saturating_add(YEAR);
    let mut ids = HashSet::new();
    for provider in &snapshot.providers {
        if provider.id.is_empty()
            || provider.id.len() > 32
            || !provider
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            || !ids.insert(provider.id.as_str())
            || !matches!(
                provider.status.as_str(),
                "ready"
                    | "needsAuthentication"
                    | "expiredSession"
                    | "localRuntimeOffline"
                    | "unavailable"
            )
            || !(0..=future).contains(&provider.updated_at_epoch_seconds)
            || provider.windows.len() > 32
            || provider.subject_id.as_deref().is_some_and(|value| {
                !scope(value)
                    || URL_SAFE_NO_PAD
                        .decode(value)
                        .ok()
                        .is_none_or(|v| v.len() != 32)
            })
        {
            return false;
        }
        let mut labels = HashSet::new();
        for window in &provider.windows {
            if !text(&window.label, 64)
                || !labels.insert(window.label.to_lowercase())
                || !window.used_fraction.is_finite()
                || !(0.0..=1.0).contains(&window.used_fraction)
                || window
                    .window_duration_seconds
                    .is_some_and(|v| !(1..=YEAR as u64).contains(&v))
                || window
                    .resets_at_epoch_seconds
                    .is_some_and(|v| !(0..=expiry).contains(&v))
            {
                return false;
            }
        }
        if provider.budget.as_ref().is_some_and(|b| {
            !b.used.is_finite()
                || b.used < 0.0
                || !b.limit.is_finite()
                || b.limit <= 0.0
                || !currency(&b.currency)
        }) || provider
            .balance
            .as_ref()
            .is_some_and(|b| !b.amount.is_finite() || b.amount < 0.0 || !currency(&b.currency))
            || provider.reset_credits.as_ref().is_some_and(|c| {
                c.available_count > i32::MAX as u32
                    || c.expires_at_epoch_seconds.len() > 64
                    || c.expires_at_epoch_seconds
                        .iter()
                        .any(|v| !(0..=expiry).contains(v))
            })
        {
            return false;
        }
        if let Some(usage) = &provider.local_usage
            && (!date(&usage.today_date)
                || !(0..=future).contains(&usage.updated_at_epoch_seconds)
                || usage
                    .reporting_period
                    .as_deref()
                    .is_some_and(|v| !period(v))
                || [
                    usage.today_tokens,
                    usage.thirty_day_tokens,
                    usage.period_tokens,
                ]
                .into_iter()
                .any(|v| !tokens(v))
                || [
                    usage.today_cost_usd,
                    usage.thirty_day_cost_usd,
                    usage.period_cost_usd,
                ]
                .into_iter()
                .any(|v| !money(v))
                || (usage.reporting_period.is_none()
                    && (usage.period_tokens.is_some() || usage.period_cost_usd.is_some()))
                || usage.top_model.as_deref().is_some_and(|v| {
                    v.is_empty()
                        || v.len() > 64
                        || !v
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
                })
                || usage
                    .incomplete_request_count
                    .is_some_and(|v| v > i32::MAX as u32))
        {
            return false;
        }
        if provider
            .history_scope_id
            .as_deref()
            .is_some_and(|v| !scope(v))
            || provider.history.len() > 8
            || (!provider.history.is_empty() && provider.history_scope_id.is_none())
        {
            return false;
        }
        let mut series_labels = HashSet::new();
        for series in &provider.history {
            if !text(&series.label, 64)
                || !series_labels.insert(series.label.as_str())
                || !provider.windows.iter().any(|w| w.label == series.label)
                || series.samples.len() > 64
                || series.samples.windows(2).any(|p| p[0].t >= p[1].t)
                || series.samples.iter().any(|p| {
                    !(now.saturating_sub(90 * 86400)
                        ..=provider.updated_at_epoch_seconds.saturating_add(120))
                        .contains(&p.t)
                        || !p.u.is_finite()
                        || !(0.0..=1.0).contains(&p.u)
                        || p.r.is_some_and(|v| !(0..=expiry).contains(&v))
                })
            {
                return false;
            }
        }
    }
    if let Some(spend) = &snapshot.spend {
        if !period(&spend.reporting_period)
            || !zone(&spend.time_zone)
            || !(0..=future).contains(&spend.updated_at_epoch_seconds)
            || spend.rows.len() > 32
            || (!spend.cost_tracking_enabled && !spend.rows.is_empty())
        {
            return false;
        }
        let mut row_ids = HashSet::new();
        for row in &spend.rows {
            if !ids.contains(row.provider_id.as_str())
                || !row_ids.insert(row.provider_id.as_str())
                || !currency(&row.currency)
                || [row.seven_day, row.thirty_day, row.period_cost]
                    .into_iter()
                    .any(|v| !money(v))
                || [
                    row.seven_day_tokens,
                    row.thirty_day_tokens,
                    row.period_tokens,
                ]
                .into_iter()
                .any(|v| !tokens(v))
                || row.daily.len() > 30
                || row.daily.windows(2).any(|d| d[0].day >= d[1].day)
                || row.daily.iter().any(|d| {
                    !date(&d.day)
                        || !tokens(d.tokens)
                        || (!d.is_scanned && d.tokens.is_some())
                        || !money(d.cost_usd)
                })
            {
                return false;
            }
        }
    }
    true
}
