//! Upstream 0.53 Usage & Spend accounting bridge.

use codexbar::cost_reporting_period::CostReportingPeriod;
use codexbar::cost_scanner::CostScanner;
use codexbar::settings::Settings;
use codexbar::spend_contract::{SpendContract, build_contract_from_period_summary};

/// `period` is a raw reporting period (`rolling:N`, `month-to-date`, `all`);
/// without a valid one the saved `cost_reporting_period` applies.
#[tauri::command]
pub async fn get_spend_contract(
    app: tauri::AppHandle,
    provider_id: String,
    period: Option<String>,
    include_open_codex: Option<bool>,
) -> Result<SpendContract, String> {
    if let Some(summary) = crate::desktop_peer::spend(&app) {
        return Ok(SpendContract::empty(
            &provider_id,
            CostReportingPeriod::parse(&summary.reporting_period).unwrap_or_default(),
        ));
    }
    let provider = provider_id.trim().to_ascii_lowercase();
    if !matches!(provider.as_str(), "codex" | "claude" | "pi" | "opencodego") {
        return Err(format!(
            "Spend contract is unavailable for provider: {provider}"
        ));
    }
    let include_import = include_open_codex.unwrap_or(false) && provider == "codex";
    if include_import {
        // Upstream 0.60.4: price imported OpenCodex rows from a fresh catalog.
        codexbar::spend_contract::refresh_opencodex_pricing_if_needed().await;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let settings = Settings::load();
        let period =
            CostReportingPeriod::resolve_request(period.as_deref(), settings.cost_reporting_period);
        if !settings.cost_usage_enabled {
            return SpendContract::empty(&provider, period);
        }
        let scanner = CostScanner::for_period(period);
        let summary = match provider.as_str() {
            "codex" => scanner.scan_codex(),
            "claude" => scanner.scan_claude(),
            "pi" => scanner.scan_pi(),
            "opencodego" => scanner.scan_opencodego_with_cancel(None),
            _ => unreachable!(),
        };
        build_contract_from_period_summary(
            &provider,
            period,
            include_import,
            settings.hide_native_codex_cost_when_open_codex_present && provider == "codex",
            settings.hide_personal_info,
            summary,
        )
    })
    .await
    .map_err(|error| format!("spend contract worker failed: {error}"))
}
