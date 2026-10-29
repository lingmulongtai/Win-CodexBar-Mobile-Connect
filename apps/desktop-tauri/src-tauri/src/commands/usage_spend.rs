//! Usage & Spend settings tab: 7-day / 30-day compat columns plus the
//! selected reporting period (History window).

use chrono::Utc;
use codexbar::cost_reporting_period::{CostReportingPeriod, CostTimeZone};
use codexbar::cost_scanner::{
    CostScanner, CostSummary, TOKEN_ACTIVITY_DAYS, TokenActivityDay, daily_token_activity,
};
use codexbar::spend_contract::{
    SpendContract, build_contract_from_period_summary, build_local_spend_contract,
    build_local_spend_contract_for_period, build_local_spend_contract_from_summary,
    build_local_spend_totals_from_summary,
};
use codexbar::spend_trend::SpendTrend;
use serde::Serialize;
use tauri::{Emitter, Manager, State};

use super::ProviderUsageSnapshot;
use crate::state::AppState;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSpendDailyPoint {
    pub day: String,
    pub amount: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSpendRow {
    pub provider_id: String,
    pub display_name: String,
    pub seven_day: Option<f64>,
    pub thirty_day: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seven_day_estimate: Option<codexbar::spend_contract::LocalCostEstimate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thirty_day_estimate: Option<codexbar::spend_contract::LocalCostEstimate>,
    pub seven_day_tokens: Option<u64>,
    pub thirty_day_tokens: Option<u64>,
    /// The token figure is a floor from an incomplete scan ("at least N").
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub seven_day_tokens_lower_bound: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub thirty_day_tokens_lower_bound: bool,
    /// Cost over the selected reporting period (History window).
    pub period_cost: Option<f64>,
    /// Tokens over the selected reporting period.
    pub period_tokens: Option<u64>,
    pub currency: String,
    pub source: String,
    /// Included in the shared Overview spend denominator.
    pub included_in_overview: bool,
    #[serde(default)]
    pub daily: Vec<UsageSpendDailyPoint>,
    /// F8 (upstream 0.48.0): true when the totals are served from a stale cache
    /// while a background re-scan rebuilds the artifact. Frontend shows a
    /// "refreshing" indicator.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub refreshing: bool,
    /// Catch-up is blocked and requires attention rather than actively running.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history_paused: bool,
    /// ISO 8601 timestamp of the stale snapshot (when `refreshing` is true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_updated_at: Option<String>,
}

#[derive(Debug, Clone)]
struct SpendValues {
    seven_day: Option<f64>,
    thirty_day: Option<f64>,
    seven_day_tokens: Option<u64>,
    thirty_day_tokens: Option<u64>,
    period_cost: Option<f64>,
    period_tokens: Option<u64>,
    source: String,
    refreshing: bool,
    stale_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSpendSummary {
    pub rows: Vec<UsageSpendRow>,
    pub contract: SpendContract,
    /// Raw reporting period the rows' `period*` columns were built for.
    pub reporting_period: String,
    pub reporting_day: String,
    pub dashboard_timezone: String,
    /// UTC time this snapshot was produced; absent if no snapshot exists.
    pub updated_at: Option<String>,
    /// False when "Track costs" is off: nothing was scanned and the summary
    /// is empty, so the pane shows "Cost tracking is off".
    pub cost_tracking_enabled: bool,
    /// The last year of per-day tokens for each local-log row, independent of
    /// the selected period; feeds the token activity grid.
    pub token_activity: Vec<TokenActivitySource>,
    /// Per-source spend series for the trend explorer; absent while
    /// "Track costs" is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend: Option<SpendTrend>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenActivitySource {
    pub provider_id: String,
    pub days: Vec<TokenActivityDay>,
}

/// Rows whose local logs can establish per-day coverage, keeping only those
/// the pane lists. Pi stays out: its rows can also be projected into Codex or
/// Claude, which would count the same usage twice.
fn token_activity_sources(
    provider_ids: &BTreeSet<String>,
    codex: Vec<TokenActivityDay>,
    claude: Vec<TokenActivityDay>,
) -> Vec<TokenActivitySource> {
    [("codex", codex), ("claude", claude)]
        .into_iter()
        .filter(|(id, days)| provider_ids.contains(*id) && !days.is_empty())
        .map(|(id, days)| TokenActivitySource {
            provider_id: id.to_string(),
            days,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct CachedUsageSpendSummary {
    generation: u64,
    key: String,
    revision: String,
    summary: UsageSpendSummary,
    refresh_owner: Option<UsageSpendRefreshOwner>,
    built_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageSpendRefreshPhase {
    Indexing,
    Paused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageSpendRefreshOwner {
    generation: u64,
    scope: String,
}

#[derive(Default)]
struct UsageSpendCoordinator {
    next_generation: u64,
    scope_epoch: u64,
    current: Option<(UsageSpendRefreshOwner, UsageSpendRefreshPhase)>,
    cache: HashMap<String, CachedUsageSpendSummary>,
    revalidating: HashSet<String>,
}

impl UsageSpendCoordinator {
    fn may_publish(&self, generation: Option<u64>) -> bool {
        generation == Some(self.next_generation)
    }
    fn begin(&mut self, scope: String) -> UsageSpendRefreshOwner {
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let owner = UsageSpendRefreshOwner {
            generation: self.next_generation,
            scope,
        };
        self.current = Some((owner.clone(), UsageSpendRefreshPhase::Indexing));
        owner
    }

    fn pause(&mut self, owner: &UsageSpendRefreshOwner) {
        if let Some((current, phase)) = self.current.as_mut()
            && current == owner
            && *phase == UsageSpendRefreshPhase::Indexing
        {
            *phase = UsageSpendRefreshPhase::Paused;
        }
    }

    fn is_current(&self, owner: &UsageSpendRefreshOwner) -> bool {
        self.current
            .as_ref()
            .is_some_and(|(current, _)| current == owner)
    }

    fn clear_if_indexing(&mut self, owner: &UsageSpendRefreshOwner) -> bool {
        let Some((current, phase)) = self.current.as_ref() else {
            return false;
        };
        if current != owner || *phase != UsageSpendRefreshPhase::Indexing {
            return false;
        }
        self.current = None;
        true
    }

    /// The cached summary for `key`, unless the caller forces a rebuild.
    fn reusable(
        &self,
        key: &str,
        revision: &str,
        force_refresh: bool,
    ) -> Option<&CachedUsageSpendSummary> {
        self.cache.get(key).filter(|existing| {
            if force_refresh || existing.key != key || existing.revision != revision {
                return false;
            }
            let has_pending = existing
                .summary
                .rows
                .iter()
                .any(|row| row.refreshing && !row.history_paused);
            let ttl = if has_pending {
                Duration::from_secs(10)
            } else {
                Duration::from_secs(300)
            };
            existing.built_at.elapsed() < ttl
        })
    }
}

static USAGE_SPEND_COORDINATOR: OnceLock<Mutex<UsageSpendCoordinator>> = OnceLock::new();
static USAGE_SPEND_BUILD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn usage_spend_coordinator() -> &'static Mutex<UsageSpendCoordinator> {
    USAGE_SPEND_COORDINATOR.get_or_init(|| Mutex::new(UsageSpendCoordinator::default()))
}

fn clear_summary_refreshing(summary: &mut UsageSpendSummary) {
    for row in &mut summary.rows {
        row.refreshing = false;
        row.history_paused = false;
        row.stale_updated_at = None;
    }
}

fn summary_is_refreshing(summary: &UsageSpendSummary) -> bool {
    summary.rows.iter().any(|row| row.refreshing)
}

/// Keep the last complete values for providers still being rebuilt while
/// accepting fresh values for providers whose refresh has finished.
fn retain_refreshing_rows(
    summary: &UsageSpendSummary,
    previous: &UsageSpendSummary,
) -> UsageSpendSummary {
    let mut retained = summary.clone();
    retained.updated_at = previous.updated_at.clone();
    for row in &mut retained.rows {
        if row.refreshing
            && let Some(old) = previous
                .rows
                .iter()
                .find(|old| old.provider_id == row.provider_id)
        {
            let (refreshing, paused) = (row.refreshing, row.history_paused);
            *row = old.clone();
            row.refreshing = refreshing;
            row.history_paused = paused;
        }
    }
    for activity in &mut retained.token_activity {
        if summary
            .rows
            .iter()
            .any(|row| row.provider_id == activity.provider_id && row.refreshing)
            && let Some(old) = previous
                .token_activity
                .iter()
                .find(|old| old.provider_id == activity.provider_id)
        {
            *activity = old.clone();
        }
    }
    if summary
        .rows
        .iter()
        .any(|row| row.provider_id == "codex" && row.refreshing)
    {
        retained.contract = previous.contract.clone();
    }
    retained.trend = previous.trend.clone();
    retained
}

fn mark_refresh_paused_if_codex_scan_paused(
    coordinator: &mut UsageSpendCoordinator,
    owner: &UsageSpendRefreshOwner,
    refreshing: bool,
    codex_scan_pause_reason: Option<&codexbar::core::CodexScanPauseReason>,
) {
    if refreshing && codex_scan_pause_reason.is_some() {
        coordinator.pause(owner);
    }
}

/// Retire an invalidated owner without allowing it to clear a replacement.
fn clear_usage_spend_refresh_if_owned(owner: &UsageSpendRefreshOwner) {
    let Ok(mut coordinator) = usage_spend_coordinator().lock() else {
        return;
    };
    if !coordinator.clear_if_indexing(owner) {
        return;
    }
    if let Some(existing) = coordinator.cache.get_mut(&owner.scope)
        && existing.refresh_owner.as_ref() == Some(owner)
    {
        clear_summary_refreshing(&mut existing.summary);
        existing.refresh_owner = None;
    }
}

struct BuiltUsageSpendSummary {
    publication_generation: Option<u64>,
    key: String,
    summary: UsageSpendSummary,
    refresh_owner: Option<UsageSpendRefreshOwner>,
}

// Compare stable account IDs rather than changing token bytes. When identity
// is unavailable, fall back to conservative credential-file invalidation.
fn account_id_marker(fields: &[&str]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fields.hash(&mut hasher);
    format!("id:{:x}", hasher.finish())
}

fn credential_file_marker(path: &std::path::Path) -> String {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| format!("file:{:?}:{}", metadata.modified().ok(), metadata.len()))
        .unwrap_or_else(|| "missing".to_string())
}

fn codex_account_marker(home: &std::path::Path) -> String {
    if let Ok(identity) = codexbar::codex_accounts::load_identity(home)
        && let (Some(subject), Some(workspace)) = (
            identity.auth_subject.as_deref(),
            identity.provider_account_id.as_deref(),
        )
    {
        return account_id_marker(&[subject, workspace]);
    }
    credential_file_marker(&home.join("auth.json"))
}

fn account_file_marker(
    credential_path: &std::path::Path,
    identity_path: &std::path::Path,
    identity_fields: &[&str],
) -> String {
    if !credential_path.exists() {
        return "missing".to_string();
    }
    let identity = std::fs::read(identity_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let fields = identity.as_ref().and_then(|value| {
        identity_fields
            .iter()
            .map(|field| value.pointer(field)?.as_str().filter(|id| !id.is_empty()))
            .collect::<Option<Vec<_>>>()
    });
    if let Some(fields) = fields {
        account_id_marker(&fields)
    } else {
        credential_file_marker(credential_path)
    }
}

fn account_scope_marker(settings: &codexbar::settings::Settings) -> String {
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(codexbar::codex_accounts::file_locations::ambient_codex_home);
    let claude = if settings.claude_allow_reading_claude_code_credentials {
        codexbar::providers::claude::accounts::config_dir()
            .ok()
            .map(|p| {
                let config = if std::env::var_os("CLAUDE_CONFIG_DIR").is_some_and(|v| !v.is_empty())
                {
                    p.join(".claude.json")
                } else {
                    p.parent().unwrap_or(&p).join(".claude.json")
                };
                account_file_marker(
                    &p.join(".credentials.json"),
                    &config,
                    &[
                        "/oauthAccount/accountUuid",
                        "/oauthAccount/organizationUuid",
                    ],
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    format!("{}|{}", codex_account_marker(&codex_home), claude)
}

static LAST_ACCOUNT_SCOPE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
fn observe_account_scope(app: &tauri::AppHandle) {
    let marker = account_scope_marker(&codexbar::settings::Settings::load());
    if let Ok(mut previous) = LAST_ACCOUNT_SCOPE.get_or_init(|| Mutex::new(None)).lock() {
        if previous.as_ref().is_some_and(|old| old != &marker) {
            invalidate_usage_spend_account_scope(app);
        }
        *previous = Some(marker);
    }
}

#[tauri::command]
pub async fn get_usage_spend_summary(
    app: tauri::AppHandle,
    state: State<'_, Mutex<AppState>>,
    period: Option<String>,
    force_refresh: Option<bool>,
) -> Result<UsageSpendSummary, String> {
    if let Some(mut remote) = crate::desktop_peer::view(&app) {
        if force_refresh.unwrap_or(false) {
            crate::desktop_peer::refresh_desktop_peer(app.clone()).await;
            remote = crate::desktop_peer::view(&app)
                .filter(|current| current.source_id == remote.source_id)
                .ok_or("Display source changed while refreshing")?;
        }
        return Ok(remote.spend_summary());
    }
    observe_account_scope(&app);
    if !force_refresh.unwrap_or(false) {
        let settings = codexbar::settings::Settings::load();
        if settings.cost_usage_enabled {
            let selected = CostReportingPeriod::resolve_request(
                period.as_deref(),
                settings.cost_reporting_period,
            );
            let cached = state
                .lock()
                .map_err(|e| e.to_string())?
                .provider_cache
                .clone();
            let key = usage_spend_cache_key(&cached, selected, &settings);
            let revision = usage_spend_revision(&cached, selected, &settings);
            let stale = if let Ok(mut coordinator) = usage_spend_coordinator().lock() {
                if coordinator.reusable(&key, &revision, false).is_none() {
                    coordinator
                        .cache
                        .get(&key)
                        .map(|entry| entry.summary.clone())
                        .map(|summary| {
                            let start = coordinator.revalidating.insert(key.clone());
                            (summary, start)
                        })
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((mut summary, start)) = stale {
                for row in &mut summary.rows {
                    row.refreshing = true;
                }
                if start {
                    let app = app.clone();
                    let requested = selected.raw();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<Mutex<AppState>>();
                        let result =
                            get_local_usage_spend_summary(state, Some(requested), Some(false))
                                .await;
                        if let Ok(mut coordinator) = usage_spend_coordinator().lock() {
                            coordinator.revalidating.remove(&key);
                        }
                        if result.is_ok() {
                            let _ = app.emit("usage-spend-updated", ());
                        }
                    });
                }
                return Ok(summary);
            }
        }
    }
    get_local_usage_spend_summary(state, period, force_refresh).await
}

pub(crate) async fn get_local_usage_spend_summary(
    state: State<'_, Mutex<AppState>>,
    period: Option<String>,
    force_refresh: Option<bool>,
) -> Result<UsageSpendSummary, String> {
    let initial_marker = account_scope_marker(&codexbar::settings::Settings::load());
    let epoch = usage_spend_coordinator()
        .lock()
        .map_err(|e| e.to_string())?
        .scope_epoch;
    let cached = {
        let guard = state.lock().map_err(|e| e.to_string())?;
        guard.provider_cache.clone()
    };

    // An explicit `period` wins; otherwise use the saved History window.
    let settings = codexbar::settings::Settings::load();
    let period =
        CostReportingPeriod::resolve_request(period.as_deref(), settings.cost_reporting_period);
    // Checked before any cache read, so a summary built while tracking was on
    // is never served after it is turned off.
    if !settings.cost_usage_enabled {
        return Ok(cost_tracking_off_summary(period));
    }
    let force_refresh = force_refresh.unwrap_or(false);
    refresh_opencodex_pricing_before_build(&cached, period, force_refresh).await;
    let original_cached = cached.clone();
    let marker = account_scope_marker(&settings);
    if initial_marker != marker {
        return Err("Account changed while updating usage".into());
    }
    let built = tauri::async_runtime::spawn_blocking(move || {
        build_usage_spend_summary_cached(&cached, period, force_refresh, epoch, &marker)
    })
    .await
    .map_err(|e| format!("usage spend worker failed: {e}"))??;
    let current_cached = state
        .lock()
        .map_err(|e| e.to_string())
        .map(|guard| guard.provider_cache.clone())?;
    let current_settings = codexbar::settings::Settings::load();
    if !current_settings.cost_usage_enabled {
        return Ok(cost_tracking_off_summary(period));
    }
    let current_key = usage_spend_cache_key(&current_cached, period, &current_settings);
    if current_key != built.key {
        if let Some(owner) = built.refresh_owner.as_ref() {
            clear_usage_spend_refresh_if_owned(owner);
        }
        if usage_spend_cache_key(&original_cached, period, &current_settings) != built.key {
            return Err(
                "Usage history settings changed during refresh; retrying with the current source"
                    .into(),
            );
        }
        // Normal provider refreshes do not invalidate the local history just
        // scanned. Keep its pending marker so catch-up retries still run.
        return Ok(built.summary);
    }
    // A late build with the same cache key must not overwrite a newer generation.
    // Keep the coordinator lock through publication so begin() cannot race it.
    if let Ok(coordinator) = usage_spend_coordinator().lock()
        && (coordinator.may_publish(built.publication_generation)
            || coordinator.cache.get(&built.key).is_some_and(|entry| {
                Some(entry.generation) == built.publication_generation
                    && !coordinator.current.as_ref().is_some_and(|(owner, _)| {
                        owner.scope == built.key && owner.generation > entry.generation
                    })
            }))
    {
        let settings = codexbar::settings::Settings::load();
        if settings.cost_usage_enabled && period == settings.spend_dashboard_period {
            crate::mobile_bridge::publish_spend(&built.summary);
        }
    }
    Ok(built.summary)
}

/// Drop account-scoped snapshots and cancel publication of an older build.
pub(crate) fn invalidate_usage_spend_account_scope(app: &tauri::AppHandle) {
    if let Ok(mut coordinator) = usage_spend_coordinator().lock() {
        coordinator.cache.clear();
        coordinator.revalidating.clear();
        coordinator.current = None;
        coordinator.next_generation = coordinator.next_generation.wrapping_add(1).max(1);
        coordinator.scope_epoch = coordinator.scope_epoch.wrapping_add(1);
    }
    if let Some(scans) = COMMON_SCANS.get()
        && let Ok(mut scans) = scans.lock()
    {
        scans.clear();
    }
    let _ = app.emit("usage-spend-scope-changed", ());
}

/// Keep each common period warm even while the Settings window is closed.
pub(crate) fn install_usage_spend_background(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        loop {
            let settings = codexbar::settings::Settings::load();
            if settings.cost_usage_enabled {
                let selected = settings.spend_dashboard_period.raw();
                let mut periods = vec![selected];
                for value in [
                    "rolling:7",
                    "rolling:30",
                    "rolling:90",
                    "month-to-date",
                    "all",
                ] {
                    if !periods.iter().any(|period| period == value) {
                        periods.push(value.to_string());
                    }
                }
                for period in periods {
                    observe_account_scope(&app);
                    if !codexbar::settings::Settings::load().cost_usage_enabled {
                        break;
                    }
                    let state = app.state::<Mutex<AppState>>();
                    if get_local_usage_spend_summary(state, Some(period), Some(false))
                        .await
                        .is_ok()
                    {
                        let _ = app.emit("usage-spend-updated", ());
                    }
                    tokio::task::yield_now().await;
                }
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });
}

/// Refreshes models.dev prices for the OpenCodex ledger when this request
/// will rebuild the summary (upstream 0.60.4 fresh-load refresh). A cached
/// summary read never starts network work.
async fn refresh_opencodex_pricing_before_build(
    cached: &[ProviderUsageSnapshot],
    period: CostReportingPeriod,
    force_refresh: bool,
) {
    let settings = codexbar::settings::Settings::load();
    if !settings.open_codex_usage_logs_enabled {
        return;
    }
    let key = usage_spend_cache_key(cached, period, &settings);
    let revision = usage_spend_revision(cached, period, &settings);
    let rebuilds = usage_spend_coordinator().lock().is_ok_and(|coordinator| {
        coordinator
            .reusable(&key, &revision, force_refresh)
            .is_none()
    });
    if rebuilds {
        codexbar::spend_contract::refresh_opencodex_pricing_if_needed().await;
    }
}

#[tauri::command]
pub fn write_usage_spend_export(path: String, payload: String) -> Result<(), String> {
    const MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024;
    let path = path.trim();
    if path.is_empty() {
        return Err("Export path must not be empty".to_string());
    }
    if payload.len() > MAX_EXPORT_BYTES {
        return Err("Usage & Spend export exceeds 8 MiB".to_string());
    }
    std::fs::write(path, payload.as_bytes()).map_err(|error| error.to_string())
}

fn build_usage_spend_summary_cached(
    cached: &[ProviderUsageSnapshot],
    period: CostReportingPeriod,
    force_refresh: bool,
    epoch: u64,
    marker: &str,
) -> Result<BuiltUsageSpendSummary, String> {
    let settings = codexbar::settings::Settings::load();
    let quick_key = usage_spend_cache_key(cached, period, &settings);
    let revision = usage_spend_revision(cached, period, &settings);
    if account_scope_marker(&settings) != marker {
        return Err("Account changed while updating usage".into());
    }
    if let Ok(coordinator) = usage_spend_coordinator().lock()
        && coordinator.scope_epoch == epoch
        && let Some(existing) = coordinator.reusable(&quick_key, &revision, force_refresh)
    {
        return Ok(BuiltUsageSpendSummary {
            publication_generation: Some(existing.generation),
            key: existing.key.clone(),
            summary: existing.summary.clone(),
            refresh_owner: existing.refresh_owner.clone(),
        });
    }
    // Coalesce concurrent local callers before checking the cache. A waiting
    // caller can reuse the first result instead of starting a second scan.
    let _build_guard = USAGE_SPEND_BUILD_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| error.to_string())?;
    let settings = codexbar::settings::Settings::load();
    if account_scope_marker(&settings) != marker {
        return Err("Account changed while updating usage".into());
    }
    let key = usage_spend_cache_key(cached, period, &settings);
    {
        let guard = usage_spend_coordinator()
            .lock()
            .map_err(|error| error.to_string())?;
        if guard.scope_epoch != epoch {
            return Err("Account changed while updating usage".into());
        }
        if let Some(existing) = guard.reusable(&key, &revision, force_refresh) {
            return Ok(BuiltUsageSpendSummary {
                publication_generation: Some(existing.generation),
                key: existing.key.clone(),
                summary: existing.summary.clone(),
                refresh_owner: existing.refresh_owner.clone(),
            });
        }
    }
    let owner = {
        let mut coordinator = usage_spend_coordinator()
            .lock()
            .map_err(|error| error.to_string())?;
        if coordinator.scope_epoch != epoch {
            return Err("Account changed while updating usage".into());
        }
        coordinator.begin(key.clone())
    };
    let summary = build_usage_spend_summary(cached, period, &settings, force_refresh, epoch);
    let refreshing = summary_is_refreshing(&summary);
    let codex_scan_pause_reason =
        codexbar::core::JsonlScanner::load_cache_status(codexbar::core::ProviderId::Codex, None)
            .codex_scan_pause_reason;

    let mut coordinator = usage_spend_coordinator()
        .lock()
        .map_err(|error| error.to_string())?;
    if coordinator.scope_epoch != epoch
        || account_scope_marker(&codexbar::settings::Settings::load()) != marker
    {
        return Err("Account changed while updating usage".into());
    }
    mark_refresh_paused_if_codex_scan_paused(
        &mut coordinator,
        &owner,
        refreshing,
        codex_scan_pause_reason.as_ref(),
    );
    if !coordinator.is_current(&owner) {
        let mut summary = summary;
        clear_summary_refreshing(&mut summary);
        return Ok(BuiltUsageSpendSummary {
            publication_generation: None,
            key,
            summary,
            refresh_owner: Some(owner),
        });
    }
    if refreshing && let Some(previous) = coordinator.cache.get(&key) {
        let previous_summary = retain_refreshing_rows(&summary, &previous.summary);
        coordinator.cache.insert(
            key.clone(),
            CachedUsageSpendSummary {
                generation: owner.generation,
                key: key.clone(),
                revision: revision.clone(),
                summary: previous_summary.clone(),
                refresh_owner: Some(owner.clone()),
                built_at: Instant::now(),
            },
        );
        return Ok(BuiltUsageSpendSummary {
            publication_generation: Some(owner.generation),
            key,
            summary: previous_summary,
            refresh_owner: Some(owner),
        });
    }
    if !refreshing {
        coordinator.clear_if_indexing(&owner);
    }
    let refresh_owner = refreshing.then(|| owner.clone());
    if coordinator.cache.len() >= 20
        && let Some(oldest) = coordinator
            .cache
            .iter()
            .min_by_key(|(_, item)| item.built_at)
            .map(|(key, _)| key.clone())
    {
        coordinator.cache.remove(&oldest);
    }
    coordinator.cache.insert(
        key.clone(),
        CachedUsageSpendSummary {
            generation: owner.generation,
            key: key.clone(),
            revision: revision.clone(),
            summary: summary.clone(),
            refresh_owner: refresh_owner.clone(),
            built_at: Instant::now(),
        },
    );
    Ok(BuiltUsageSpendSummary {
        publication_generation: Some(owner.generation),
        key,
        summary,
        refresh_owner,
    })
}

fn usage_spend_cache_key(
    _cached: &[ProviderUsageSnapshot],
    period: CostReportingPeriod,
    settings: &codexbar::settings::Settings,
) -> String {
    let base = usage_spend_cache_key_with_privacy(
        &[],
        &period.identity(
            Utc::now(),
            codexbar::cost_reporting_period::cost_bucket_zone(),
        ),
        settings.open_codex_usage_logs_enabled,
        settings.hide_native_codex_cost_when_open_codex_present,
        settings.hide_personal_info,
    );
    let mut providers: Vec<_> = settings.enabled_providers.iter().collect();
    providers.sort();
    format!(
        "{}|{:?}|{:?}|{}|{}|{:?}",
        base,
        codexbar::core::JsonlScanner::default_codex_cache_root(),
        settings.codex_custom_sessions_dirs,
        codexbar::cost_reporting_period::cost_bucket_zone().identifier(),
        account_scope_marker(settings),
        providers
    )
}

fn usage_spend_revision(
    cached: &[ProviderUsageSnapshot],
    period: CostReportingPeriod,
    settings: &codexbar::settings::Settings,
) -> String {
    usage_spend_cache_key_with_privacy(
        cached,
        &period.identity(
            Utc::now(),
            codexbar::cost_reporting_period::cost_bucket_zone(),
        ),
        settings.open_codex_usage_logs_enabled,
        settings.hide_native_codex_cost_when_open_codex_present,
        settings.hide_personal_info,
    )
}

fn usage_spend_cache_key_with_privacy(
    cached: &[ProviderUsageSnapshot],
    period_identity: &str,
    include_opencodex: bool,
    hide_native: bool,
    hide_personal_info: bool,
) -> String {
    let mut revisions: Vec<String> = cached
        .iter()
        .map(|snapshot| {
            let cost = snapshot
                .cost
                .as_ref()
                .map(|cost| {
                    let daily = cost
                        .daily
                        .iter()
                        .map(|point| format!("{}:{:.8}", point.day, point.amount))
                        .collect::<Vec<_>>()
                        .join(",");
                    format!(
                        "{:.8}:{:?}:{:?}:{}:{}:{}",
                        cost.used, cost.limit, cost.balance, cost.currency_code, cost.period, daily
                    )
                })
                .unwrap_or_default();
            format!(
                "{}:{}:{}:{}",
                snapshot.provider_id,
                snapshot.error.is_some(),
                snapshot.source_label,
                cost
            )
        })
        .collect();
    revisions.sort();
    format!(
        "{}|{}|{}|{}|{}|{}",
        chrono::Local::now().date_naive(),
        period_identity,
        include_opencodex,
        hide_native,
        hide_personal_info,
        revisions.join(";")
    )
}

/// Log only fixed stage names and elapsed time; never include local paths or usage data.
fn trace_spend_stage<T>(stage: &'static str, work: impl FnOnce() -> T) -> T {
    let started = std::time::Instant::now();
    tracing::info!(stage, "usage spend stage started");
    let result = work();
    tracing::info!(
        stage,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "usage spend stage finished"
    );
    result
}

#[derive(Clone)]
struct MemoizedScan {
    summary: CostSummary,
    sampled_at: chrono::DateTime<Utc>,
    stored_at: Instant,
}

static COMMON_SCANS: OnceLock<Mutex<HashMap<String, MemoizedScan>>> = OnceLock::new();

/// Share provider scans across the five period snapshots in one warm-up cycle.
fn memoized_scan(
    key: String,
    force: bool,
    scan: impl FnOnce() -> CostSummary,
) -> (CostSummary, chrono::DateTime<Utc>) {
    let cache = COMMON_SCANS.get_or_init(|| Mutex::new(HashMap::new()));
    if !force
        && let Ok(cache) = cache.lock()
        && let Some(entry) = cache.get(&key)
    {
        let max_age = if entry.summary.history_coverage_established {
            240
        } else {
            10
        };
        if entry.stored_at.elapsed() < Duration::from_secs(max_age) {
            return (entry.summary.clone(), entry.sampled_at);
        }
    }
    let summary = scan();
    let sampled_at = Utc::now();
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= 30
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.stored_at)
                .map(|(key, _)| key.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(
            key,
            MemoizedScan {
                summary: summary.clone(),
                sampled_at,
                stored_at: Instant::now(),
            },
        );
    }
    (summary, sampled_at)
}

/// Expose the known portion of a partially priced Codex window without
/// presenting it as the complete total.
fn codex_partial_estimate(
    contract: &SpendContract,
) -> Option<codexbar::spend_contract::LocalCostEstimate> {
    if contract.price_coverage.unpriced == 0 && contract.price_coverage.unmetered == 0 {
        return None;
    }
    let mut saw = false;
    let model_total: f64 = contract
        .models
        .iter()
        .filter_map(|model| model.cost_usd.or(model.partial_cost_usd))
        .filter(|value| value.is_finite() && *value >= 0.0)
        .inspect(|_| saw = true)
        .sum();
    let known = match (contract.known_cost_usd, saw && model_total.is_finite()) {
        (Some(total), true) => Some(total.max(model_total)),
        (Some(total), false) => Some(total),
        (None, true) => Some(model_total),
        (None, false) => None,
    };
    Some(codexbar::spend_contract::LocalCostEstimate {
        known_subtotal_usd: known,
        coverage: contract.price_coverage.clone(),
        ..Default::default()
    })
}

fn build_usage_spend_summary(
    cached: &[ProviderUsageSnapshot],
    period: CostReportingPeriod,
    settings: &codexbar::settings::Settings,
    force_refresh: bool,
    epoch: u64,
) -> UsageSpendSummary {
    let now = Utc::now();
    let period_scan_days = period.scan_days(now);
    let include_opencodex = settings.open_codex_usage_logs_enabled;
    let hide_native = settings.hide_native_codex_cost_when_open_codex_present;
    let pi_selected = settings.enabled_providers.iter().any(|id| id == "pi")
        || cached.iter().any(|snapshot| snapshot.provider_id == "pi");
    let include_pi_in_native = !pi_selected;

    // Upstream 0.55.0 #3105: independent provider baselines load in parallel.
    // Keep each provider's 7d/30d scans serial so they can safely share that
    // provider's incremental cache, while Codex and Claude run concurrently.
    let codex_scan_options = if force_refresh {
        codexbar::core::CostScanOptions::app_driven()
    } else {
        codexbar::core::CostScanOptions::default()
    };
    let mut codex_scan_options = codex_scan_options;
    codex_scan_options.include_pi_sessions = include_pi_in_native;
    // Any period other than 7d/30d costs one extra scan per provider.
    let (
        (codex_7_summary, codex_30_summary, codex_period_summary, codex_activity),
        (
            claude_7_summary,
            claude_30_summary,
            claude_period_summary,
            claude_activity,
            claude_sampled,
        ),
        (pi_7_summary, pi_30_summary, pi_period_summary, pi_sampled),
    ) = std::thread::scope(|scope| {
        let codex = scope.spawn(move || {
            let (seven, thirty, selected) = scan_codex_spend_periods(period, |requested| {
                trace_spend_stage("codex period scan", || {
                    CostScanner::for_period(requested)
                        .with_options(codex_scan_options)
                        .scan_codex()
                })
            });
            let activity = trace_spend_stage("codex annual activity", || {
                daily_token_activity("codex", TOKEN_ACTIVITY_DAYS)
            });
            (seven, thirty, selected, activity)
        });
        let common_scope = format!(
            "{}|{:?}|{}|{}|{}|{}",
            account_scope_marker(settings),
            settings.codex_custom_sessions_dirs,
            codexbar::cost_reporting_period::cost_bucket_zone().date(now),
            codexbar::cost_reporting_period::cost_bucket_zone().identifier(),
            include_pi_in_native,
            epoch
        );
        let claude_scope = common_scope.clone();
        let claude = scope.spawn(move || {
            let scan = |requested: CostReportingPeriod| {
                memoized_scan(
                    format!(
                        "claude|{}|{}|{:?}",
                        claude_scope,
                        requested.raw(),
                        std::env::var_os("CLAUDE_CONFIG_DIR")
                    ),
                    force_refresh,
                    || {
                        CostScanner::for_period(requested)
                            .scan_claude_with_cancel_and_pi_sessions(None, include_pi_in_native)
                    },
                )
            };
            let seven = scan(CostReportingPeriod::Rolling(7));
            let thirty = scan(CostReportingPeriod::Rolling(30));
            let selected = selected_period_scan(period, &seven, &thirty, || scan(period));
            let sampled = seven.1.min(thirty.1).min(selected.1);
            let activity = trace_spend_stage("claude annual activity", || {
                daily_token_activity("claude", TOKEN_ACTIVITY_DAYS)
            });
            (seven.0, thirty.0, selected.0, activity, sampled)
        });
        let pi = scope.spawn(move || {
            let scan = |requested: CostReportingPeriod| {
                memoized_scan(
                    format!("pi|{}|{}", common_scope, requested.raw()),
                    force_refresh,
                    || CostScanner::for_period(requested).scan_pi(),
                )
            };
            let seven = scan(CostReportingPeriod::Rolling(7));
            let thirty = scan(CostReportingPeriod::Rolling(30));
            let selected = selected_period_scan(period, &seven, &thirty, || scan(period));
            let sampled = seven.1.min(thirty.1).min(selected.1);
            (seven.0, thirty.0, selected.0, sampled)
        });
        (
            codex.join().expect("Codex spend scan worker panicked"),
            claude.join().expect("Claude spend scan worker panicked"),
            pi.join().expect("Pi spend scan worker panicked"),
        )
    });

    let codex_restored = codex_period_summary.restored_sessions;
    let codex_stale = !codex_7_summary.history_coverage_established
        || !codex_30_summary.history_coverage_established
        || !codex_period_summary.history_coverage_established;
    let codex_status = codex_stale.then(|| {
        codexbar::core::JsonlScanner::load_cache_status(codexbar::core::ProviderId::Codex, None)
    });
    let codex_stale_updated_at = codex_status
        .as_ref()
        .and_then(|status| status.previous_report.as_ref())
        .and_then(|report| report.updated_at.clone());
    let codex_paused = codex_status
        .as_ref()
        .is_some_and(|status| status.codex_scan_pause_reason.is_some());

    let codex_7_contract = build_local_spend_totals_from_summary(
        "codex",
        7,
        include_opencodex,
        hide_native,
        settings.hide_personal_info,
        codex_7_summary.clone(),
    );
    let codex_30_contract = build_local_spend_totals_from_summary(
        "codex",
        30,
        include_opencodex,
        hide_native,
        settings.hide_personal_info,
        codex_30_summary.clone(),
    );
    let codex_period_contract = trace_spend_stage("codex period details", || {
        build_contract_from_period_summary(
            "codex",
            period,
            include_opencodex,
            hide_native,
            settings.hide_personal_info,
            codex_period_summary,
        )
    });
    let pi_period_contract = build_contract_from_period_summary(
        "pi",
        period,
        false,
        false,
        settings.hide_personal_info,
        pi_period_summary,
    );
    let pi_7_contract = build_local_spend_contract_from_summary(
        "pi",
        7,
        false,
        false,
        settings.hide_personal_info,
        pi_7_summary.clone(),
    );
    let pi_30_contract = build_local_spend_contract_from_summary(
        "pi",
        30,
        false,
        false,
        settings.hide_personal_info,
        pi_30_summary.clone(),
    );

    let mut provider_ids: BTreeSet<String> = settings.enabled_providers.iter().cloned().collect();
    provider_ids.extend(cached.iter().map(|snapshot| snapshot.provider_id.clone()));
    if include_opencodex {
        // OpenCodex is an enrichment source, never a standalone provider row.
        // Publish routed subscriptions even when no live provider snapshot exists.
        for id in ["codex", "opencodego", "kimi", "deepseek", "nous"] {
            let contract = match id {
                "codex" => None,
                _ => Some(build_local_spend_contract(id, 30, true)),
            };
            if contract
                .as_ref()
                .is_some_and(|contract| !contract.imports.is_empty())
            {
                provider_ids.insert(id.to_string());
            }
        }
    }

    let token_activity = token_activity_sources(&provider_ids, codex_activity, claude_activity);

    let cached_by_id: HashMap<&str, &ProviderUsageSnapshot> = cached
        .iter()
        .map(|snapshot| (snapshot.provider_id.as_str(), snapshot))
        .collect();

    let mut rows = Vec::new();
    for provider_id in provider_ids {
        let cached_snapshot = cached_by_id.get(provider_id.as_str()).copied();
        let display_name = cached_snapshot
            .map(|snapshot| snapshot.display_name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| {
                codexbar::core::ProviderId::from_cli_name(&provider_id).map(|id| {
                    codexbar::core::instantiate_provider(id)
                        .metadata()
                        .display_name
                        .to_string()
                })
            })
            .unwrap_or_else(|| provider_id.clone());

        let mut local_cost_estimates = None;
        let mut token_lower_bounds = (false, false);
        let spend = match provider_id.as_str() {
            "codex" => {
                let seven_estimate = codex_partial_estimate(&codex_7_contract);
                let thirty_estimate = codex_partial_estimate(&codex_30_contract);
                if seven_estimate.is_some() || thirty_estimate.is_some() {
                    local_cost_estimates = Some((
                        seven_estimate.clone().unwrap_or_default(),
                        thirty_estimate.clone().unwrap_or_default(),
                    ));
                }
                SpendValues {
                    seven_day: if seven_estimate.is_some() {
                        None
                    } else {
                        codex_7_contract.known_cost_usd
                    },
                    thirty_day: if thirty_estimate.is_some() {
                        None
                    } else {
                        codex_30_contract.known_cost_usd
                    },
                    seven_day_tokens: codex_7_contract.token_total,
                    thirty_day_tokens: codex_30_contract.token_total,
                    period_cost: codex_period_contract.known_cost_usd,
                    period_tokens: codex_period_contract.token_total,
                    source: if include_opencodex && !codex_30_contract.imports.is_empty() {
                        "local logs + OpenCodex".to_string()
                    } else {
                        format!(
                            "local logs{} · API list-price estimate",
                            if codex_restored > 0 {
                                format!(" + {codex_restored} restored backup sessions")
                            } else {
                                String::new()
                            }
                        )
                    },
                    refreshing: codex_stale,
                    stale_updated_at: codex_stale_updated_at.clone(),
                }
            }
            "claude" => SpendValues {
                seven_day: Some(claude_7_summary.total_cost_usd),
                thirty_day: Some(claude_30_summary.total_cost_usd),
                seven_day_tokens: Some(claude_7_summary.total_tokens_for_provider("claude")),
                thirty_day_tokens: Some(claude_30_summary.total_tokens_for_provider("claude")),
                period_cost: Some(claude_period_summary.total_cost_usd),
                period_tokens: Some(claude_period_summary.total_tokens_for_provider("claude")),
                source: "local logs".to_string(),
                refreshing: false,
                stale_updated_at: None,
            },
            "pi" => SpendValues {
                seven_day: pi_7_contract.known_cost_usd,
                thirty_day: pi_30_contract.known_cost_usd,
                seven_day_tokens: pi_7_contract.token_total,
                thirty_day_tokens: pi_30_contract.token_total,
                period_cost: pi_period_contract.known_cost_usd,
                period_tokens: pi_period_contract.token_total,
                source: "local Pi/OMP history".to_string(),
                refreshing: !pi_30_summary.history_coverage_established,
                stale_updated_at: None,
            },
            "opencodego" | "kimi" | "deepseek" | "nous" if include_opencodex => {
                let seven = build_local_spend_contract(&provider_id, 7, true);
                let thirty = build_local_spend_contract(&provider_id, 30, true);
                if !thirty.imports.is_empty() {
                    let selected =
                        build_local_spend_contract_for_period(&provider_id, period, true);
                    SpendValues {
                        seven_day: seven.known_cost_usd,
                        thirty_day: thirty.known_cost_usd,
                        seven_day_tokens: seven.token_total,
                        thirty_day_tokens: thirty.token_total,
                        period_cost: selected.known_cost_usd,
                        period_tokens: selected.token_total,
                        source: if provider_id == "opencodego" {
                            "local logs + OpenCodex".to_string()
                        } else {
                            "OpenCodex".to_string()
                        },
                        refreshing: false,
                        stale_updated_at: None,
                    }
                } else {
                    cached_spend(cached_snapshot, period, now)
                }
            }
            "opencodego" => {
                // Cost stays the provider's; recorded local tokens fill the token columns.
                let mut spend = cached_spend(cached_snapshot, period, now);
                let seven = CostScanner::new(7).scan_opencodego_usage_tokens_with_cancel(None);
                let thirty = CostScanner::new(30).scan_opencodego_usage_tokens_with_cancel(None);
                spend.period_tokens = selected_period_scan(period, &seven, &thirty, || {
                    CostScanner::for_period(period).scan_opencodego_usage_tokens_with_cancel(None)
                });
                spend.seven_day_tokens = seven;
                spend.thirty_day_tokens = thirty;
                spend
            }
            "cursor" => {
                let seven = codexbar::providers::cursor::local_csv::summarize(7);
                let thirty = codexbar::providers::cursor::local_csv::summarize(30);
                if thirty.row_count > 0 {
                    let selected =
                        codexbar::providers::cursor::local_csv::summarize(period_scan_days);
                    SpendValues {
                        seven_day: (seven.row_count > 0).then_some(seven.total_cost_usd),
                        thirty_day: Some(thirty.total_cost_usd),
                        seven_day_tokens: (seven.row_count > 0).then_some(seven.total_tokens),
                        thirty_day_tokens: Some(thirty.total_tokens),
                        period_cost: (selected.row_count > 0).then_some(selected.total_cost_usd),
                        period_tokens: (selected.row_count > 0).then_some(selected.total_tokens),
                        source: "local Cursor tokscale cache".to_string(),
                        refreshing: false,
                        stale_updated_at: None,
                    }
                } else {
                    cached_spend(cached_snapshot, period, now)
                }
            }
            "grok" => {
                let seven = codexbar::providers::grok::local_sessions::summarize(7);
                let thirty = codexbar::providers::grok::local_sessions::summarize(30);
                let selected =
                    codexbar::providers::grok::local_sessions::summarize(period_scan_days);
                let mut spend = cached_spend(cached_snapshot, period, now);
                spend.seven_day_tokens = (seven.session_count > 0).then_some(seven.total_tokens);
                spend.thirty_day_tokens = (thirty.session_count > 0).then_some(thirty.total_tokens);
                spend.period_tokens = (selected.session_count > 0).then_some(selected.total_tokens);
                if thirty.session_count > 0 {
                    spend.source = "local Grok sessions".to_string();
                }
                spend
            }
            "antigravity" => {
                use codexbar::providers::antigravity::local_sessions;
                let seven = local_sessions::summarize(7);
                let thirty = local_sessions::summarize(30);
                let selected = local_sessions::summarize(period_scan_days);
                let mut spend = cached_spend(cached_snapshot, period, now);
                // Upstream 0.64: the app refreshes unknown-model pricing in the
                // background; a later read (provider refresh or Refresh) reprices.
                if let Some(refresh) = local_sessions::background_pricing_refresh(&thirty) {
                    tauri::async_runtime::spawn(refresh);
                }
                {
                    use codexbar::providers::antigravity::local_sessions::LocalHistoryCoverage;
                    spend.seven_day_tokens =
                        matches!(seven.coverage, LocalHistoryCoverage::Complete)
                            .then_some(seven.total_tokens);
                    spend.thirty_day_tokens =
                        matches!(thirty.coverage, LocalHistoryCoverage::Complete)
                            .then_some(thirty.total_tokens);
                    spend.period_tokens =
                        matches!(selected.coverage, LocalHistoryCoverage::Complete)
                            .then_some(selected.total_tokens);
                    if matches!(thirty.coverage, LocalHistoryCoverage::Complete) {
                        spend.source = "local Antigravity history".to_string();
                    }
                }
                let spend = antigravity_spend_values(spend, &seven, &thirty);
                token_lower_bounds = (seven.lower_bound, thirty.lower_bound);
                local_cost_estimates = Some((seven.cost_estimate, thirty.cost_estimate));
                spend
            }
            _ => cached_spend(cached_snapshot, period, now),
        };

        let currency = cached_snapshot
            .and_then(|snapshot| snapshot.cost.as_ref())
            .map(|cost| cost.currency_code.clone())
            .unwrap_or_else(|| "USD".to_string());
        let daily = cached_snapshot
            .and_then(|snapshot| snapshot.cost.as_ref())
            .map(|cost| {
                cost.daily
                    .iter()
                    .map(|point| UsageSpendDailyPoint {
                        day: point.day.clone(),
                        amount: point.amount,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (seven_day_estimate, thirty_day_estimate) = local_cost_estimates
            .map(|(seven, thirty)| (Some(seven), Some(thirty)))
            .unwrap_or((None, None));
        rows.push(UsageSpendRow {
            provider_id: provider_id.clone(),
            display_name,
            seven_day: spend.seven_day,
            thirty_day: spend.thirty_day,
            seven_day_estimate,
            thirty_day_estimate,
            seven_day_tokens: spend.seven_day_tokens,
            thirty_day_tokens: spend.thirty_day_tokens,
            seven_day_tokens_lower_bound: token_lower_bounds.0,
            thirty_day_tokens_lower_bound: token_lower_bounds.1,
            period_cost: spend.period_cost,
            period_tokens: spend.period_tokens,
            currency,
            source: spend.source,
            included_in_overview: include_in_shared_overview(
                &provider_id,
                settings.enabled_providers.contains(&provider_id),
                cached_snapshot.is_some(),
            ),
            daily,
            refreshing: spend.refreshing,
            history_paused: provider_id == "codex" && codex_paused,
            stale_updated_at: spend.stale_updated_at,
        });
    }

    let contract = codex_period_contract;
    let mut updated_at = now.min(claude_sampled).min(pi_sampled);
    let claude_trend = rows.iter().any(|row| row.provider_id == "claude").then(|| {
        let (daily, hourly, sampled_at) =
            claude_trend_history(period.sidecar_days(now), now, force_refresh, epoch);
        updated_at = updated_at.min(sampled_at);
        (claude_period_summary.known_zero, daily, hourly)
    });
    let trend = build_spend_trend(&rows, &contract, claude_trend, period, now);
    let reporting_day = last_included_reporting_day(&contract);
    let dashboard_timezone = codexbar::core::local_timezone_name();
    UsageSpendSummary {
        rows,
        contract,
        reporting_period: period.raw(),
        reporting_day,
        dashboard_timezone,
        updated_at: Some(updated_at.to_rfc3339()),
        cost_tracking_enabled: true,
        token_activity,
        trend: Some(trend),
    }
}

/// The summary while "Track costs" is off: no rows, no scan.
fn cost_tracking_off_summary(period: CostReportingPeriod) -> UsageSpendSummary {
    UsageSpendSummary {
        rows: Vec::new(),
        contract: SpendContract::empty("codex", period),
        reporting_period: period.raw(),
        reporting_day: chrono::Local::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string(),
        dashboard_timezone: codexbar::core::local_timezone_name(),
        updated_at: None,
        cost_tracking_enabled: false,
        token_activity: Vec::new(),
        trend: None,
    }
}

type DailyCosts = Vec<(String, Option<f64>)>;
type HourlyCosts = Vec<codexbar::cost_scanner::HourlyCostPoint>;

/// Window length, local reporting day, and cost bucket zone of one Claude
/// trend walk.
type ClaudeTrendKey = (u32, chrono::NaiveDate, String, u64);

struct ClaudeTrendMemo {
    key: ClaudeTrendKey,
    stored_at: std::time::Instant,
    sampled_at: chrono::DateTime<Utc>,
    daily: DailyCosts,
    hourly: HourlyCosts,
}

/// The Claude trend walk reads every transcript in the window with no
/// incremental cache, so a complete result is reused for the cost scan
/// debounce interval instead of re-walking on every summary rebuild.
fn claude_trend_history(
    days: u32,
    now: chrono::DateTime<Utc>,
    force: bool,
    epoch: u64,
) -> (DailyCosts, HourlyCosts, chrono::DateTime<Utc>) {
    static MEMO: Mutex<Option<ClaudeTrendMemo>> = Mutex::new(None);
    let zone = codexbar::cost_reporting_period::cost_bucket_zone();
    let key: ClaudeTrendKey = (days, zone.date(now), zone.identifier(), epoch);
    let ttl = std::time::Duration::from_secs(
        codexbar::core::CostScanOptions::default().refresh_min_interval_secs,
    );
    if let Some(memo) = MEMO
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|memo| !force && memo.key == key && memo.stored_at.elapsed() <= ttl)
    {
        return (memo.daily.clone(), memo.hourly.clone(), memo.sampled_at);
    }
    let (daily, hourly, complete) =
        codexbar::cost_scanner::get_claude_daily_and_hourly_cost_history(days);
    let sampled_at = Utc::now();
    // A partial walk is never reused, so the next rebuild retries it.
    if complete {
        *MEMO
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ClaudeTrendMemo {
            key,
            stored_at: std::time::Instant::now(),
            sampled_at,
            daily: daily.clone(),
            hourly: hourly.clone(),
        });
    }
    (daily, hourly, sampled_at)
}
/// Claude's window known-zero flag with its daily and hourly cost series.
type ClaudeTrendSeries = (bool, DailyCosts, HourlyCosts);

/// Recorded daily (and, where the source keeps timestamps, hourly) spend per
/// source over the selected window. Local-log providers use their own scans;
/// every other USD row contributes its provider-reported daily costs.
fn build_spend_trend(
    rows: &[UsageSpendRow],
    codex: &SpendContract,
    claude: Option<ClaudeTrendSeries>,
    period: CostReportingPeriod,
    now: chrono::DateTime<Utc>,
) -> SpendTrend {
    use codexbar::spend_trend::{SpendTrendBuilder, SpendTrendSource};
    let mut series: Vec<(SpendTrendSource, DailyCosts, HourlyCosts)> = Vec::new();
    for row in rows {
        let source = |known_zero: bool| SpendTrendSource {
            id: row.provider_id.clone(),
            provider_id: row.provider_id.clone(),
            display_name: row.display_name.clone(),
            known_zero,
        };
        match row.provider_id.as_str() {
            "codex" => series.push((
                source(codex.known_zero),
                codex
                    .daily
                    .iter()
                    .map(|point| (point.day.clone(), point.cost_usd))
                    .collect(),
                Vec::new(),
            )),
            "claude" => {
                if let Some((known_zero, daily, hourly)) = claude.clone() {
                    series.push((source(known_zero), daily, hourly));
                }
            }
            _ if row.included_in_overview
                && row.currency.eq_ignore_ascii_case("USD")
                && !row.daily.is_empty() =>
            {
                series.push((
                    source(false),
                    row.daily
                        .iter()
                        .map(|point| (point.day.clone(), Some(point.amount)))
                        .collect(),
                    Vec::new(),
                ));
            }
            _ => {}
        }
    }
    // All available history opens at the first recorded spend, not at 1970.
    let earliest = series
        .iter()
        .flat_map(|(_, daily, _)| daily.iter())
        .filter(|(_, cost)| cost.is_some_and(|cost| cost > 0.0))
        .filter_map(|(day, _)| chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok())
        .min();
    let zone = codexbar::cost_reporting_period::cost_bucket_zone();
    let bounds = period.bounds(now, zone, earliest.or(Some(zone.date(now))));
    let mut builder = SpendTrendBuilder::new(bounds, zone);
    for (source, daily, hourly) in series {
        builder.add_source(source, daily, &hourly);
    }
    builder.finish()
}

/// Reuse the fixed scan when it already covers the selected period.
/// Populate the widest window first so smaller cards reuse that cache rather
/// than forcing repeated full parses as 7 days expands to 30 and then 90/all.
fn scan_codex_spend_periods<T: Clone>(
    period: CostReportingPeriod,
    mut scan: impl FnMut(CostReportingPeriod) -> T,
) -> (T, T, T) {
    if period == CostReportingPeriod::AllAvailable || period.scan_days(Utc::now()) > 30 {
        let selected = scan(period);
        let thirty = scan(CostReportingPeriod::Rolling(30));
        let seven = scan(CostReportingPeriod::Rolling(7));
        (seven, thirty, selected)
    } else {
        let thirty = scan(CostReportingPeriod::Rolling(30));
        let seven = scan(CostReportingPeriod::Rolling(7));
        let selected = selected_period_scan(period, &seven, &thirty, || scan(period));
        (seven, thirty, selected)
    }
}

fn selected_period_scan<T: Clone>(
    period: CostReportingPeriod,
    seven: &T,
    thirty: &T,
    scan: impl FnOnce() -> T,
) -> T {
    match period {
        CostReportingPeriod::Rolling(7) => seven.clone(),
        CostReportingPeriod::Rolling(30) => thirty.clone(),
        _ => scan(),
    }
}

/// Pi is an alternate local-history view over rows that may already be
/// projected into Codex or Claude. Keep it out of the shared denominator so
/// enabling Pi cannot double-count the same physical usage.
fn include_in_shared_overview(provider_id: &str, enabled: bool, cached: bool) -> bool {
    provider_id != "pi" && (enabled || cached)
}

fn last_included_reporting_day(contract: &SpendContract) -> String {
    contract
        .daily
        .iter()
        .filter_map(|point| chrono::NaiveDate::parse_from_str(&point.day, "%Y-%m-%d").ok())
        .max()
        .unwrap_or_else(|| chrono::Local::now().date_naive())
        .format("%Y-%m-%d")
        .to_string()
}

fn antigravity_spend_values(
    mut spend: SpendValues,
    seven: &codexbar::spend_contract::LocalTokenHistorySummary,
    thirty: &codexbar::spend_contract::LocalTokenHistorySummary,
) -> SpendValues {
    use codexbar::spend_contract::LocalHistoryCoverage;

    spend.seven_day = seven.total_usd();
    spend.thirty_day = thirty.total_usd();
    // Exact for a complete scan, a floor for a lower bound, unknown otherwise.
    spend.seven_day_tokens = seven.published_tokens();
    spend.thirty_day_tokens = thirty.published_tokens();
    if spend.thirty_day.is_some() {
        spend.source = "local Antigravity history · API list-price estimate".to_string();
    } else if thirty.cost_estimate.known_subtotal_usd.is_some() {
        spend.source = "local Antigravity history · known API list-price subtotal".to_string();
    } else if thirty.coverage == LocalHistoryCoverage::Complete {
        spend.source = "local Antigravity history · unpriced".to_string();
    }
    spend
}

/// Provider-reported daily costs bucket by UTC day, so the selected period is
/// resolved in UTC here. `None` when no day falls inside the window.
fn period_cost_from_daily(
    daily: &[super::bridge::CostDailyPointBridge],
    period: CostReportingPeriod,
    now: chrono::DateTime<Utc>,
) -> Option<f64> {
    let earliest = daily
        .iter()
        .filter_map(|point| chrono::NaiveDate::parse_from_str(&point.day, "%Y-%m-%d").ok())
        .min();
    let bounds = period.bounds(now, CostTimeZone::UTC, earliest);
    let mut total = 0.0;
    let mut saw = false;
    for point in daily {
        if bounds.contains_day_key(&point.day) {
            total += point.amount;
            saw = true;
        }
    }
    saw.then_some(total)
}

fn cached_spend(
    snapshot: Option<&ProviderUsageSnapshot>,
    reporting_period: CostReportingPeriod,
    now: chrono::DateTime<Utc>,
) -> SpendValues {
    let Some(snapshot) = snapshot else {
        return SpendValues {
            seven_day: None,
            thirty_day: None,
            seven_day_tokens: None,
            thirty_day_tokens: None,
            period_cost: None,
            period_tokens: None,
            source: "unavailable".to_string(),
            refreshing: false,
            stale_updated_at: None,
        };
    };
    let Some(cost) = snapshot.cost.as_ref() else {
        return SpendValues {
            seven_day: None,
            thirty_day: None,
            seven_day_tokens: None,
            thirty_day_tokens: None,
            period_cost: None,
            period_tokens: None,
            source: if snapshot.error.is_some() {
                "unavailable".to_string()
            } else {
                snapshot.source_label.clone()
            },
            refreshing: false,
            stale_updated_at: None,
        };
    };
    let period = cost.period.trim();
    let period_lower = period.to_ascii_lowercase();
    let (seven_day, thirty_day) = if cost.daily.is_empty() {
        (
            None,
            (period_lower.contains("30 day") || period_lower.contains("30-day"))
                .then_some(cost.used),
        )
    } else {
        let today = chrono::Utc::now().date_naive();
        let seven_cutoff = today - chrono::Duration::days(6);
        let thirty_cutoff = today - chrono::Duration::days(29);
        let mut seven = 0.0;
        let mut thirty = 0.0;
        let mut saw_seven = false;
        let mut saw_thirty = false;
        for point in &cost.daily {
            let Ok(day) = chrono::NaiveDate::parse_from_str(&point.day, "%Y-%m-%d") else {
                continue;
            };
            // Providers with long daily history (Bedrock keeps 14 months) must
            // not widen the fixed 30-day column.
            if day > today || day < thirty_cutoff {
                continue;
            }
            thirty += point.amount;
            saw_thirty = true;
            if day >= seven_cutoff {
                seven += point.amount;
                saw_seven = true;
            }
        }
        (saw_seven.then_some(seven), saw_thirty.then_some(thirty))
    };
    let period_cost = if cost.daily.is_empty() {
        // A provider-reported 30-day total only answers a 30-day selection.
        (reporting_period == CostReportingPeriod::Rolling(30))
            .then_some(thirty_day)
            .flatten()
    } else {
        period_cost_from_daily(&cost.daily, reporting_period, now)
    };
    SpendValues {
        seven_day,
        thirty_day,
        seven_day_tokens: None,
        thirty_day_tokens: None,
        period_cost,
        period_tokens: None,
        source: if period.is_empty() {
            snapshot.source_label.clone()
        } else {
            format!("period ({period})")
        },
        refreshing: false,
        stale_updated_at: None,
    }
}

#[cfg(test)]
mod cache_key_tests {
    use super::*;

    #[test]
    fn account_markers_survive_token_rotation_and_detect_account_changes() {
        let dir = std::env::temp_dir().join(format!("codexbar-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let auth = dir.join("auth.json");
        std::fs::write(
            &auth,
            r#"{"tokens":{"account_id":"account-one","access_token":"old","id_token":"e30.eyJzdWIiOiJ1c2VyLW9uZSJ9.signature"}}"#,
        )
        .unwrap();
        let original = codex_account_marker(&dir);
        std::fs::write(
            &auth,
            r#"{"tokens":{"account_id":"account-one","access_token":"new-token","id_token":"e30.eyJzdWIiOiJ1c2VyLW9uZSJ9.signature"}}"#,
        )
        .unwrap();
        assert_eq!(original, codex_account_marker(&dir));
        std::fs::write(
            &auth,
            r#"{"tokens":{"account_id":"account-two","access_token":"new-token","id_token":"e30.eyJzdWIiOiJ1c2VyLW9uZSJ9.signature"}}"#,
        )
        .unwrap();
        assert_ne!(original, codex_account_marker(&dir));
        std::fs::write(
            &auth,
            r#"{"tokens":{"account_id":"account-one","access_token":"new-token","id_token":"e30.eyJzdWIiOiJ1c2VyLXR3byJ9.signature"}}"#,
        )
        .unwrap();
        assert_ne!(original, codex_account_marker(&dir));

        let config = dir.join(".claude.json");
        std::fs::write(
            &config,
            r#"{"oauthAccount":{"accountUuid":"one","organizationUuid":"org"}}"#,
        )
        .unwrap();
        let claude = account_file_marker(
            &auth,
            &config,
            &[
                "/oauthAccount/accountUuid",
                "/oauthAccount/organizationUuid",
            ],
        );
        std::fs::write(&auth, "rotated credentials").unwrap();
        assert_eq!(
            claude,
            account_file_marker(
                &auth,
                &config,
                &[
                    "/oauthAccount/accountUuid",
                    "/oauthAccount/organizationUuid"
                ]
            )
        );
        std::fs::write(
            &config,
            r#"{"oauthAccount":{"accountUuid":"one","organizationUuid":"other"}}"#,
        )
        .unwrap();
        assert_ne!(
            claude,
            account_file_marker(
                &auth,
                &config,
                &[
                    "/oauthAccount/accountUuid",
                    "/oauthAccount/organizationUuid"
                ]
            )
        );
        std::fs::remove_file(auth).unwrap();
        std::fs::remove_file(config).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn codex_spend_scans_widest_first_and_reuses_selected_baselines() {
        for (period, expected) in [
            (
                CostReportingPeriod::Rolling(7),
                vec!["rolling:30", "rolling:7"],
            ),
            (
                CostReportingPeriod::Rolling(30),
                vec!["rolling:30", "rolling:7"],
            ),
            (
                CostReportingPeriod::Rolling(90),
                vec!["rolling:90", "rolling:30", "rolling:7"],
            ),
            (
                CostReportingPeriod::AllAvailable,
                vec!["all", "rolling:30", "rolling:7"],
            ),
        ] {
            let mut seen = Vec::new();
            let (seven, thirty, selected) = scan_codex_spend_periods(period, |p| {
                seen.push(p.raw());
                p.raw()
            });
            assert_eq!(seen, expected);
            assert_eq!(seven, "rolling:7");
            assert_eq!(thirty, "rolling:30");
            assert_eq!(selected, period.raw());
        }
    }

    fn local_history(
        total_tokens: u64,
        coverage: codexbar::spend_contract::LocalHistoryCoverage,
        known_subtotal_usd: Option<f64>,
        unpriced: u32,
    ) -> codexbar::spend_contract::LocalTokenHistorySummary {
        codexbar::spend_contract::LocalTokenHistorySummary {
            total_tokens,
            session_count: if total_tokens > 0 { 1 } else { 0 },
            coverage,
            cost_estimate: codexbar::spend_contract::LocalCostEstimate {
                known_subtotal_usd,
                coverage: codexbar::spend_contract::CostCoverageCounts {
                    estimated: if known_subtotal_usd.is_some() { 1 } else { 0 },
                    unpriced,
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn invalidated_owner_clears_orphaned_indexing_activity() {
        let mut coordinator = UsageSpendCoordinator::default();
        let owner = coordinator.begin("account:old".to_string());

        assert!(coordinator.clear_if_indexing(&owner));
        assert!(!coordinator.is_current(&owner));
    }

    #[test]
    fn old_owner_cleanup_cannot_clear_a_replacement() {
        let mut coordinator = UsageSpendCoordinator::default();
        let old = coordinator.begin("account:old".to_string());
        let replacement = coordinator.begin("account:new".to_string());

        assert!(!coordinator.clear_if_indexing(&old));
        assert!(coordinator.is_current(&replacement));
    }

    #[test]
    fn settings_replacement_preserves_an_intentional_pause() {
        let mut coordinator = UsageSpendCoordinator::default();
        let old = coordinator.begin("settings:old".to_string());
        let replacement = coordinator.begin("settings:new".to_string());
        let status = codexbar::core::CachedCostReadStatus {
            codex_scan_pause_reason: Some(codexbar::core::CodexScanPauseReason::NoProgress),
            ..Default::default()
        };
        mark_refresh_paused_if_codex_scan_paused(
            &mut coordinator,
            &replacement,
            true,
            status.codex_scan_pause_reason.as_ref(),
        );

        assert!(!coordinator.clear_if_indexing(&old));
        assert_eq!(
            coordinator.current.as_ref().map(|(_, phase)| *phase),
            Some(UsageSpendRefreshPhase::Paused)
        );
    }

    fn empty_summary() -> UsageSpendSummary {
        UsageSpendSummary {
            reporting_period: "rolling:30".to_string(),
            rows: Vec::new(),
            contract: SpendContract {
                provider_id: "codex".to_string(),
                reporting_period: "rolling:30".to_string(),
                history_days: 30,
                known_cost_usd: None,
                known_zero: false,
                provenance: codexbar::spend_contract::CostProvenance::Unknown,
                price_coverage: Default::default(),
                price_coverage_ratio: None,
                history_coverage_established: false,
                token_mix: Default::default(),
                token_total: None,
                conversation_count: 0,
                models: Vec::new(),
                projects: Vec::new(),
                conversations: Vec::new(),
                daily: Vec::new(),
                hourly_activity: Vec::new(),
                project_source_status: None,
                custom_pricing_active: false,
                imports: Vec::new(),
            },
            reporting_day: "2026-10-01".to_string(),
            dashboard_timezone: "UTC".to_string(),
            updated_at: None,
            cost_tracking_enabled: true,
            token_activity: Vec::new(),
            trend: None,
        }
    }

    fn activity_day(day: &str, total_tokens: Option<u64>) -> TokenActivityDay {
        TokenActivityDay {
            day: day.to_string(),
            total_tokens,
            is_scanned: true,
        }
    }

    fn trend_row(provider_id: &str, currency: &str, daily: &[(&str, f64)]) -> UsageSpendRow {
        UsageSpendRow {
            provider_id: provider_id.to_string(),
            display_name: provider_id.to_uppercase(),
            seven_day: None,
            thirty_day: None,
            seven_day_estimate: None,
            thirty_day_estimate: None,
            seven_day_tokens: None,
            thirty_day_tokens: None,
            seven_day_tokens_lower_bound: false,
            thirty_day_tokens_lower_bound: false,
            period_cost: None,
            period_tokens: None,
            currency: currency.to_string(),
            source: "fixture".to_string(),
            included_in_overview: provider_id != "pi",
            daily: daily
                .iter()
                .map(|(day, amount)| UsageSpendDailyPoint {
                    day: (*day).to_string(),
                    amount: *amount,
                })
                .collect(),
            refreshing: false,
            history_paused: false,
            stale_updated_at: None,
        }
    }

    #[test]
    fn token_activity_keeps_listed_local_log_rows_only() {
        let listed: BTreeSet<String> = ["codex".to_string(), "pi".to_string()].into();
        let sources = token_activity_sources(
            &listed,
            vec![activity_day("2026-10-01", Some(5))],
            vec![activity_day("2026-10-01", Some(7))],
        );
        let ids: Vec<_> = sources
            .iter()
            .map(|source| source.provider_id.as_str())
            .collect();
        assert_eq!(ids, ["codex"]);

        let both: BTreeSet<String> = ["codex".to_string(), "claude".to_string()].into();
        let sources = token_activity_sources(&both, Vec::new(), vec![activity_day("d", None)]);
        let json = serde_json::to_value(&sources).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!([{
                "providerId": "claude",
                "days": [{"day": "d", "totalTokens": null, "isScanned": true}],
            }])
        );
    }

    #[test]
    fn spend_trend_publishes_each_usd_source_once_inside_the_window() {
        let now = chrono::Utc::now();
        let zone = codexbar::cost_reporting_period::cost_bucket_zone();
        let today = zone.date(now);
        let key = |offset: i64| {
            (today - chrono::Duration::days(offset))
                .format("%Y-%m-%d")
                .to_string()
        };
        let mut codex = empty_summary().contract;
        codex.known_zero = true;
        codex.daily = vec![codexbar::spend_contract::SpendDailyPoint {
            day: key(1),
            cost_usd: Some(2.0),
            total_tokens: Some(10),
        }];
        let (old, recent) = (key(40), key(2));
        let rows = vec![
            trend_row("codex", "USD", &[(recent.as_str(), 99.0)]),
            trend_row("claude", "USD", &[]),
            trend_row(
                "openrouter",
                "USD",
                &[(old.as_str(), 5.0), (recent.as_str(), 3.0)],
            ),
            trend_row("mistral", "EUR", &[(recent.as_str(), 4.0)]),
            trend_row("pi", "USD", &[(recent.as_str(), 1.0)]),
        ];
        let claude = Some((false, vec![(key(0), Some(1.5))], Vec::new()));
        let trend = build_spend_trend(&rows, &codex, claude, CostReportingPeriod::Rolling(30), now);

        let ids: Vec<_> = trend.sources.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["claude", "codex", "openrouter"]);
        assert!(
            trend
                .sources
                .iter()
                .find(|s| s.id == "codex")
                .unwrap()
                .known_zero
        );
        // Codex comes from local logs, never the snapshot's daily series.
        let codex_days: Vec<_> = trend
            .daily
            .iter()
            .filter(|p| p.source_id == "codex")
            .map(|p| (p.day.clone(), p.cost_usd))
            .collect();
        assert_eq!(codex_days, vec![(key(1), Some(2.0))]);
        // The 40-day-old point falls outside a 30-day window.
        assert!(
            trend
                .daily
                .iter()
                .filter(|p| p.source_id == "openrouter")
                .all(|p| p.day == recent)
        );
        assert_eq!(trend.start_day, key(29));
        assert_eq!(trend.end_day, key(0));
    }

    #[test]
    fn cost_tracking_off_summary_is_empty_and_flagged() {
        let summary = cost_tracking_off_summary(CostReportingPeriod::MonthToDate);
        assert!(!summary.cost_tracking_enabled);
        assert!(summary.rows.is_empty());
        assert!(summary.token_activity.is_empty());
        assert_eq!(summary.reporting_period, "month-to-date");
        assert_eq!(summary.contract.known_cost_usd, None);
        assert!(summary.contract.daily.is_empty());
        let json = serde_json::to_value(&summary).expect("serialize");
        assert_eq!(json["costTrackingEnabled"], serde_json::json!(false));
    }

    #[test]
    fn only_a_summary_rebuild_may_refresh_opencodex_pricing() {
        let mut coordinator = UsageSpendCoordinator::default();
        assert!(coordinator.reusable("day|30", "r", false).is_none());
        coordinator.cache.insert(
            "day|30".into(),
            CachedUsageSpendSummary {
                generation: 0,
                key: "day|30".to_string(),
                revision: "r".into(),
                summary: empty_summary(),
                refresh_owner: None,
                built_at: Instant::now(),
            },
        );
        // A cached read starts no network work; a forced or new key rebuilds.
        assert!(coordinator.reusable("day|30", "r", false).is_some());
        assert!(coordinator.reusable("day|30", "r", true).is_none());
        assert!(coordinator.reusable("day|7", "r", false).is_none());
    }

    #[test]
    fn incomplete_history_expires_so_catch_up_can_continue() {
        let mut summary = empty_summary();
        let mut row = trend_row("codex", "USD", &[]);
        row.refreshing = true;
        summary.rows.push(row);
        let mut coordinator = UsageSpendCoordinator {
            cache: HashMap::from([(
                "day|30".into(),
                CachedUsageSpendSummary {
                    generation: 0,
                    key: "day|30".into(),
                    revision: "r".into(),
                    summary,
                    refresh_owner: None,
                    built_at: Instant::now() - Duration::from_secs(11),
                },
            )]),
            ..UsageSpendCoordinator::default()
        };
        assert!(coordinator.reusable("day|30", "r", false).is_none());
        coordinator.cache.get_mut("day|30").unwrap().summary.rows[0].history_paused = true;
        assert!(coordinator.reusable("day|30", "r", false).is_some());
        let mut other = trend_row("claude", "USD", &[]);
        other.refreshing = true;
        coordinator
            .cache
            .get_mut("day|30")
            .unwrap()
            .summary
            .rows
            .push(other);
        assert!(coordinator.reusable("day|30", "r", false).is_none());
        assert!(coordinator.reusable("day|30", "r", true).is_none());
    }

    #[test]
    fn partial_codex_window_reports_known_subtotal_as_incomplete() {
        let mut contract = SpendContract::empty("codex", CostReportingPeriod::Rolling(7));
        contract.known_cost_usd = Some(5.0);
        contract.price_coverage.unpriced = 1;
        contract
            .models
            .push(codexbar::spend_contract::SpendModelRow {
                model: "fixture".into(),
                cost_usd: None,
                partial_cost_usd: Some(8.0),
                input_tokens: 1,
                output_tokens: 0,
                cache_read_tokens: 0,
                total_tokens: 1,
                custom_pricing: false,
            });
        let estimate = codex_partial_estimate(&contract).unwrap();
        assert_eq!(estimate.known_subtotal_usd, Some(8.0));
        assert_eq!(estimate.coverage.unpriced, 1);
        contract.price_coverage.unpriced = 0;
        assert!(codex_partial_estimate(&contract).is_none());
    }

    #[test]
    fn pending_refresh_retains_old_values_but_keeps_new_status_and_other_providers() {
        let mut old = empty_summary();
        old.updated_at = Some("2026-01-01T00:00:00Z".into());
        let mut codex = trend_row("codex", "USD", &[]);
        codex.period_cost = Some(10.0);
        old.rows.push(codex.clone());
        let mut new = empty_summary();
        codex.period_cost = Some(1.0);
        codex.refreshing = true;
        codex.history_paused = true;
        new.rows.push(codex);
        let mut claude = trend_row("claude", "USD", &[]);
        claude.period_cost = Some(7.0);
        new.rows.push(claude);
        let retained = retain_refreshing_rows(&new, &old);
        assert_eq!(retained.rows[0].period_cost, Some(10.0));
        assert!(retained.rows[0].refreshing && retained.rows[0].history_paused);
        assert_eq!(retained.rows[1].period_cost, Some(7.0));
        assert_eq!(retained.updated_at, old.updated_at);
    }

    #[test]
    fn common_scans_reuse_results_and_force_refresh_rebuilds() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let key = format!("fixture-{}", uuid::Uuid::new_v4());
        let scan = || {
            calls.fetch_add(1, Ordering::SeqCst);
            CostSummary {
                history_coverage_established: true,
                ..CostSummary::default()
            }
        };
        let first = memoized_scan(key.clone(), false, scan);
        let second = memoized_scan(key.clone(), false, scan);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first.1, second.1);
        memoized_scan(key, true, scan);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cache_keeps_multiple_periods_independently() {
        let mut coordinator = UsageSpendCoordinator::default();
        for key in [
            "rolling:7",
            "rolling:30",
            "rolling:90",
            "month-to-date",
            "all",
        ] {
            coordinator.cache.insert(
                key.into(),
                CachedUsageSpendSummary {
                    generation: 1,
                    key: key.into(),
                    revision: "r".into(),
                    summary: empty_summary(),
                    refresh_owner: None,
                    built_at: Instant::now(),
                },
            );
        }
        for key in [
            "rolling:7",
            "rolling:30",
            "rolling:90",
            "month-to-date",
            "all",
        ] {
            assert!(coordinator.reusable(key, "r", false).is_some(), "{key}");
        }
    }

    #[test]
    fn a_late_same_key_build_cannot_overwrite_the_shared_spend() {
        let mut coordinator = UsageSpendCoordinator::default();
        let a = coordinator.begin("same-key".into());
        assert!(coordinator.may_publish(Some(a.generation)));
        let b = coordinator.begin("same-key".into());
        assert!(!coordinator.may_publish(Some(a.generation)));
        assert!(coordinator.may_publish(Some(b.generation)));
        coordinator.clear_if_indexing(&b);
        assert!(coordinator.may_publish(Some(b.generation)));
        assert!(!coordinator.may_publish(None));
    }

    #[test]
    fn privacy_mode_is_part_of_usage_spend_cache_identity() {
        let public = usage_spend_cache_key_with_privacy(&[], "rolling:30", false, false, false);
        let private = usage_spend_cache_key_with_privacy(&[], "rolling:30", false, false, true);
        assert_ne!(public, private);
    }

    #[test]
    fn pi_history_is_an_alternate_view_not_a_shared_overview_source() {
        assert!(!include_in_shared_overview("pi", true, true));
        assert!(include_in_shared_overview("codex", true, false));
        assert!(include_in_shared_overview("claude", false, true));
        assert!(!include_in_shared_overview("codex", false, false));
    }

    #[test]
    fn antigravity_partial_history_exposes_only_the_known_subtotal() {
        use codexbar::spend_contract::LocalHistoryCoverage;

        let seven = local_history(100, LocalHistoryCoverage::Partial, Some(1.25), 0);
        let thirty = local_history(200, LocalHistoryCoverage::Partial, Some(2.50), 0);
        let spend = antigravity_spend_values(
            cached_spend(None, CostReportingPeriod::Rolling(30), chrono::Utc::now()),
            &seven,
            &thirty,
        );

        assert_eq!(spend.seven_day, None);
        assert_eq!(spend.thirty_day, None);
        assert_eq!(spend.seven_day_tokens, None);
        assert_eq!(spend.thirty_day_tokens, None);
        assert!(spend.source.contains("known API list-price subtotal"));
    }

    #[test]
    fn antigravity_lower_bound_history_publishes_floors_not_exact_totals() {
        use codexbar::spend_contract::LocalHistoryCoverage;

        let mut seven = local_history(100, LocalHistoryCoverage::Partial, Some(1.25), 0);
        seven.lower_bound = true;
        let withheld = local_history(0, LocalHistoryCoverage::Partial, None, 0);
        let spend = antigravity_spend_values(
            cached_spend(None, CostReportingPeriod::Rolling(30), chrono::Utc::now()),
            &seven,
            &withheld,
        );

        assert_eq!(spend.seven_day, None);
        assert_eq!(spend.seven_day_tokens, Some(100));
        assert_eq!(spend.thirty_day, None);
        assert_eq!(spend.thirty_day_tokens, None);
    }

    #[test]
    fn antigravity_complete_empty_history_is_a_known_zero() {
        use codexbar::spend_contract::LocalHistoryCoverage;

        let seven = local_history(0, LocalHistoryCoverage::Complete, None, 0);
        let thirty = local_history(0, LocalHistoryCoverage::Complete, None, 0);
        let spend = antigravity_spend_values(
            cached_spend(None, CostReportingPeriod::Rolling(30), chrono::Utc::now()),
            &seven,
            &thirty,
        );

        assert_eq!(spend.seven_day, Some(0.0));
        assert_eq!(spend.thirty_day, Some(0.0));
        assert_eq!(spend.seven_day_tokens, Some(0));
        assert_eq!(spend.thirty_day_tokens, Some(0));
        assert!(spend.source.contains("API list-price estimate"));
    }
}
