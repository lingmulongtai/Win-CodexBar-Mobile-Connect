//! Android sharing reads the same generation-checked cache as the tray.
use crate::commands::{ProviderLocalUsageSummary, ProviderUsageSnapshot, RateWindowSnapshot};
use crate::state::AppState;
use codexbar::core::{ProviderFetchResult, ProviderId, ProviderStateKind, ResetCreditsObservation};
use codexbar::mobile_bridge::{
    self, MobileBalance, MobileBudget, MobileLocalUsage, MobileProvider, MobileResetCredits,
    MobileServer, MobileSnapshot, MobileWindow, Pairing,
};
use codexbar::{secure_file, settings::Settings};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use tauri::Manager;
mod remote;
mod shared_data;
pub(crate) use remote::settings_revision;
pub(crate) use shared_data::{publish_spend, schedule_spend_refresh};

#[derive(Clone)]
pub(crate) struct MobileObservation {
    scope: Option<[u8; 32]>,
    history_scope: Option<[u8; 32]>,
    reset_credits: Option<ResetCreditsObservation>,
    pub(crate) fresh: bool,
    credential_stamp: CredentialStamp,
    known: KnownWindows,
}

#[derive(Clone)]
struct KnownWindows {
    primary: bool,
    secondary: bool,
    tertiary: bool,
    model_specific: bool,
    extras: std::collections::HashSet<String>,
}

pub(crate) type CredentialStamp = Vec<(std::path::PathBuf, Option<std::time::SystemTime>)>;
pub(crate) fn credential_stamp() -> CredentialStamp {
    [
        codexbar::settings::ApiKeys::keys_path(),
        codexbar::settings::ManualCookies::cookies_path(),
        Some(codexbar::core::TokenAccountStore::default_path()),
    ]
    .into_iter()
    .flatten()
    .map(|path| {
        let modified = std::fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        (path, modified)
    })
    .collect()
}

impl MobileObservation {
    pub(crate) fn seeded(usage: &ProviderUsageSnapshot) -> Self {
        Self {
            scope: None,
            history_scope: None,
            reset_credits: None,
            fresh: true,
            credential_stamp: credential_stamp(),
            known: KnownWindows {
                primary: true,
                secondary: true,
                tertiary: true,
                model_specific: true,
                extras: usage
                    .extra_rate_windows
                    .iter()
                    .map(|extra| extra.id.clone())
                    .collect(),
            },
        }
    }
    pub(crate) fn from_result(
        id: ProviderId,
        result: &ProviderFetchResult,
        account: Option<uuid::Uuid>,
        credential_stamp: CredentialStamp,
        selected_login: Option<&str>,
    ) -> Self {
        let identity = result
            .account_identity()
            .map(str::to_owned)
            .or_else(|| account.map(|value| value.to_string()))
            .or_else(|| result.usage.account_email.clone());
        let scope = identity
            .filter(|value| !value.trim().is_empty())
            .map(|value| mobile_bridge::account_scope(id.cli_name(), value.trim()));
        let history_scope = scope.or_else(|| {
            // A selected Gemini login is a connection boundary, not a verified Google account ID.
            if id != ProviderId::GeminiApps {
                return None;
            }
            selected_login
                .map(|header| mobile_bridge::account_scope("geminiapps-selected-login", header))
        });
        Self {
            scope,
            history_scope,
            reset_credits: result.reset_credits,
            fresh: true,
            credential_stamp,
            known: KnownWindows {
                primary: result.usage.primary.usage_known(),
                secondary: result
                    .usage
                    .secondary
                    .as_ref()
                    .is_some_and(|window| window.usage_known()),
                tertiary: result
                    .usage
                    .tertiary
                    .as_ref()
                    .is_some_and(|window| window.usage_known()),
                model_specific: result
                    .usage
                    .model_specific
                    .as_ref()
                    .is_some_and(|window| window.usage_known()),
                extras: result
                    .usage
                    .extra_rate_windows
                    .iter()
                    .filter(|extra| extra.usage_known && extra.window.usage_known())
                    .map(|extra| extra.id.clone())
                    .collect(),
            },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    enabled: bool,
    address: String,
    port: u16,
    pairing: Pairing,
    #[serde(default)]
    allow_remote_settings: bool,
}

impl Default for Config {
    fn default() -> Self {
        let address = std::net::UdpSocket::bind("0.0.0.0:0")
            .ok()
            .and_then(|socket| {
                socket.connect("192.0.2.1:9").ok()?;
                Some(socket.local_addr().ok()?.ip())
            })
            .filter(|address| mobile_bridge::is_private_address(*address))
            .unwrap_or_else(|| "127.0.0.1".parse().unwrap());
        Self {
            enabled: false,
            address: address.to_string(),
            port: mobile_bridge::DEFAULT_PORT,
            pairing: Pairing::generate(),
            allow_remote_settings: false,
        }
    }
}

fn config_path() -> Result<std::path::PathBuf, String> {
    Settings::settings_path()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("mobile-sharing.json"))
        })
        .ok_or_else(|| "Mobile sharing storage is unavailable".into())
}

fn save(config: &Config) -> Result<(), String> {
    let json =
        serde_json::to_string(config).map_err(|_| "Cannot encode mobile sharing settings")?;
    secure_file::write_string(&config_path()?, &json)
        .map_err(|_| "Cannot save mobile sharing settings".into())
}

fn load() -> Result<Config, String> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw =
        secure_file::read_string(&path).map_err(|_| "Cannot unlock mobile sharing settings")?;
    let config: Config =
        serde_json::from_str(&raw).map_err(|_| "Invalid mobile sharing settings")?;
    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<SocketAddr, String> {
    let address: IpAddr = config
        .address
        .parse()
        .map_err(|_| "Enter a numeric private IP address")?;
    if !address.is_ipv4()
        || !mobile_bridge::is_private_address(address)
        || config.port < 1024
        || !config.pairing.validate()
    {
        return Err("Use a private LAN or Tailscale address and a port from 1024 to 65535".into());
    }
    Ok(SocketAddr::new(address, config.port))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MobileSharingStatus {
    allow_remote_settings: bool,
    enabled: bool,
    running: bool,
    address: String,
    port: u16,
    error: Option<String>,
}

pub(crate) struct MobileSharing(tokio::sync::Mutex<Runtime>);

impl Default for MobileSharing {
    fn default() -> Self {
        let mut runtime = Runtime::default();
        match load() {
            Ok(config) => runtime.config = Some(config),
            Err(error) => runtime.error = Some(error),
        }
        Self(tokio::sync::Mutex::new(runtime))
    }
}

#[derive(Default)]
struct Runtime {
    control_gate: Option<Arc<Mutex<bool>>>,
    config: Option<Config>,
    server: Option<MobileServer>,
    error: Option<String>,
}

impl Runtime {
    fn status(&self) -> MobileSharingStatus {
        let config = self.config.as_ref();
        MobileSharingStatus {
            allow_remote_settings: config.is_some_and(|config| config.allow_remote_settings),
            enabled: config.is_some_and(|config| config.enabled),
            running: self.server.is_some(),
            address: config.map_or_else(|| "127.0.0.1".into(), |config| config.address.clone()),
            port: config.map_or(mobile_bridge::DEFAULT_PORT, |config| config.port),
            error: self.error.clone(),
        }
    }
    async fn stop(&mut self) {
        if let Some(gate) = self.control_gate.take() {
            *gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        }
        if let Some(server) = self.server.take() {
            server.stop().await;
        }
    }
    async fn start(&mut self, app: &tauri::AppHandle) -> Result<(), String> {
        let config = self
            .config
            .as_ref()
            .ok_or("Mobile sharing is unavailable")?;
        if !config.enabled {
            return Ok(());
        }
        let address = validate(config)?;
        let handle = app.clone();
        let source = Arc::new(move |pairing: &Pairing| snapshot(&handle, pairing));
        let gate = Arc::new(Mutex::new(true));
        let controls = remote::source(app.clone(), gate.clone(), config.allow_remote_settings);
        self.control_gate = Some(gate);
        self.server = Some(MobileServer::start_with_controls(address, config.pairing.clone(), source, Some(controls)).await
            .map_err(|_| "Cannot listen on this address and port. Check that the address belongs to this PC and the port is free.")?);
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn set_mobile_remote_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, MobileSharing>,
    allowed: bool,
) -> Result<MobileSharingStatus, String> {
    let mut runtime = state.0.lock().await;
    let mut config = runtime
        .config
        .clone()
        .ok_or("Mobile sharing is unavailable")?;
    config.allow_remote_settings = allowed;
    save(&config)?;
    runtime.stop().await;
    runtime.config = Some(config);
    runtime.error = runtime.start(&app).await.err();
    Ok(runtime.status())
}

pub(crate) fn install(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let shared = app.state::<MobileSharing>();
        let mut runtime = shared.0.lock().await;
        if runtime.config.is_some() {
            runtime.error = runtime.start(&app).await.err();
        }
    });
}

#[tauri::command]
pub(crate) async fn get_mobile_sharing_status(
    state: tauri::State<'_, MobileSharing>,
) -> Result<MobileSharingStatus, String> {
    let runtime = state.0.lock().await;
    Ok(runtime.status())
}

#[tauri::command]
pub(crate) async fn configure_mobile_sharing(
    app: tauri::AppHandle,
    state: tauri::State<'_, MobileSharing>,
    enabled: bool,
    address: String,
    port: u16,
) -> Result<MobileSharingStatus, String> {
    let mut runtime = state.0.lock().await;
    let mut config = runtime
        .config
        .clone()
        .ok_or("Mobile sharing settings could not be loaded")?;
    config.enabled = enabled;
    config.address = address.trim().to_owned();
    config.port = port;
    validate(&config)?;
    // Failed persistence leaves the current listener untouched.
    save(&config)?;
    runtime.stop().await;
    runtime.config = Some(config);
    runtime.error = runtime.start(&app).await.err();
    Ok(runtime.status())
}

#[tauri::command]
pub(crate) async fn get_mobile_pairing_code(
    state: tauri::State<'_, MobileSharing>,
) -> Result<String, String> {
    let runtime = state.0.lock().await;
    if runtime.server.is_none() {
        return Err("Enable mobile sharing before pairing".into());
    }
    let config = runtime
        .config
        .as_ref()
        .ok_or("Mobile sharing is unavailable")?;
    config
        .pairing
        .code(validate(config)?.ip(), config.port)
        .ok_or_else(|| "Invalid pairing settings".into())
}

#[tauri::command]
pub(crate) async fn revoke_mobile_pairing(
    app: tauri::AppHandle,
    state: tauri::State<'_, MobileSharing>,
) -> Result<MobileSharingStatus, String> {
    let mut runtime = state.0.lock().await;
    let mut config = runtime.config.clone().unwrap_or_default();
    config.pairing = Pairing::generate();
    save(&config)?;
    runtime.stop().await;
    runtime.config = Some(config);
    runtime.error = runtime.start(&app).await.err();
    Ok(runtime.status())
}

const ANDROID_PROVIDERS: &[&str] = &[
    "codex",
    "claude",
    "gemini",
    "geminiapps",
    "copilot",
    "cursor",
    "zai",
    "zenmux",
    "kimi",
    "elevenlabs",
    "openrouter",
    "synthetic",
    "chutes",
    "deepseek",
    "venice",
    "kimik2",
    "clinepass",
    "ibmbob",
    "fireworks",
    "devin",
    "antigravity",
];

fn snapshot(app: &tauri::AppHandle, pairing: &Pairing) -> MobileSnapshot {
    let state = app.state::<Mutex<AppState>>();
    let providers: Vec<MobileProvider> = state
        .lock()
        .ok()
        .map(|guard| {
            guard
                .provider_cache
                .iter()
                .filter(|usage| ANDROID_PROVIDERS.contains(&usage.provider_id.as_str()))
                .map(|usage| {
                    let observation = ProviderId::from_cli_name(&usage.provider_id)
                        .and_then(|id| guard.mobile_observations.get(&id));
                    let mut provider = project(usage, observation, pairing);
                    let now = chrono::Utc::now().timestamp();
                    if observation.is_some_and(|item| {
                        item.fresh && item.credential_stamp == credential_stamp()
                    }) && usage.error.is_none()
                        && !usage.error_state.is_problem()
                        && (now - 7200..=now + 120).contains(&provider.updated_at_epoch_seconds)
                    {
                        provider.local_usage =
                            crate::commands::cached_provider_local_usage_summary(
                                &usage.provider_id,
                            )
                            .and_then(|summary| {
                                project_local_usage(summary, chrono::Utc::now().timestamp())
                            });
                        if provider.local_usage.is_some() && provider.status == "unavailable" {
                            provider.status = "ready".into();
                        }
                    }
                    provider
                })
                .collect()
        })
        .unwrap_or_default();
    MobileSnapshot {
        schema_version: 1,
        source: mobile_bridge::SOURCE.into(),
        generated_at_epoch_seconds: chrono::Utc::now().timestamp(),
        spend: shared_data::cached_spend(&providers),
        providers,
    }
}

fn project(
    usage: &ProviderUsageSnapshot,
    observation: Option<&MobileObservation>,
    pairing: &Pairing,
) -> MobileProvider {
    project_usage(usage, observation, Some(pairing))
}

fn project_usage(
    usage: &ProviderUsageSnapshot,
    observation: Option<&MobileObservation>,
    pairing: Option<&Pairing>,
) -> MobileProvider {
    let now = chrono::Utc::now().timestamp();
    let updated_at = parse_time(&usage.updated_at).unwrap_or(0);
    let fresh = (now - 7200..=now + 120).contains(&updated_at)
        && usage.error.is_none()
        && !usage.error_state.is_problem()
        && observation.is_some_and(|metadata| {
            metadata.fresh && metadata.credential_stamp == credential_stamp()
        });
    let status = if fresh {
        "ready"
    } else {
        match usage.error_state {
            ProviderStateKind::NeedsAuthentication => "needsAuthentication",
            ProviderStateKind::ExpiredSession => "expiredSession",
            ProviderStateKind::LocalRuntimeOffline => "localRuntimeOffline",
            _ => "unavailable",
        }
    };
    let mut windows = Vec::new();
    let known = observation.map(|metadata| &metadata.known);
    let mut add = |label: &str, window: &RateWindowSnapshot, known: bool| {
        if known
            && !window.is_informational
            && window.used_percent.is_finite()
            && window.used_percent >= 0.0
            && windows.len() < 32
        {
            let mut units = 0;
            let label: String = label
                .chars()
                .filter(|character| !character.is_control())
                .take_while(|character| {
                    units += character.len_utf16();
                    units <= 64
                })
                .collect();
            if !label.is_empty()
                && !windows
                    .iter()
                    .any(|item: &MobileWindow| item.label.to_lowercase() == label.to_lowercase())
            {
                windows.push(MobileWindow {
                    label,
                    used_fraction: (window.used_percent / 100.0).min(1.0),
                    resets_at_epoch_seconds: window
                        .resets_at
                        .as_deref()
                        .and_then(parse_time)
                        .filter(|time| (0..=now + 366 * 86400).contains(time)),
                    window_duration_seconds: window
                        .window_minutes
                        .filter(|minutes| (1..=366 * 24 * 60).contains(minutes))
                        .map(|minutes| u64::from(minutes) * 60),
                });
            }
        }
    };
    add(
        usage.primary_label.as_deref().unwrap_or("Primary"),
        &usage.primary,
        known.is_some_and(|known| known.primary),
    );
    if let Some(window) = &usage.secondary {
        add(
            usage.secondary_label.as_deref().unwrap_or("Secondary"),
            window,
            known.is_some_and(|known| known.secondary),
        );
    }
    if let Some(window) = &usage.tertiary {
        add(
            usage.tertiary_label.as_deref().unwrap_or("Tertiary"),
            window,
            known.is_some_and(|known| known.tertiary),
        );
    }
    if let Some(window) = &usage.model_specific {
        add(
            "Model",
            window,
            known.is_some_and(|known| known.model_specific),
        );
    }
    for extra in &usage.extra_rate_windows {
        if extra.id != "reset-credits" {
            add(
                &extra.title,
                &extra.window,
                known.is_some_and(|known| known.extras.contains(&extra.id)),
            );
        }
    }
    let cost = usage.cost.as_ref().filter(|cost| {
        (1..=8).contains(&cost.currency_code.len())
            && cost
                .currency_code
                .chars()
                .all(|character| character.is_ascii_alphabetic())
    });
    let budget = cost.filter(|cost| cost.period == "month").and_then(|cost| {
        cost.limit
            .filter(|limit| limit.is_finite() && *limit > 0.0)
            .filter(|_| cost.used.is_finite() && cost.used >= 0.0)
            .map(|limit| MobileBudget {
                used: cost.used,
                limit,
                currency: cost.currency_code.clone(),
            })
    });
    let balance = cost.and_then(|cost| {
        cost.balance
            .filter(|balance| balance.is_finite() && *balance >= 0.0)
            .map(|amount| MobileBalance {
                amount,
                currency: cost.currency_code.clone(),
            })
    });
    let mut provider = MobileProvider {
        id: usage.provider_id.clone(),
        updated_at_epoch_seconds: parse_time(&usage.updated_at)
            .filter(|time| (0..=now + 120).contains(time))
            .unwrap_or(0),
        status: status.into(),
        subject_id: observation
            .and_then(|observation| observation.scope.as_ref())
            .and_then(|scope| {
                pairing.and_then(|pairing| mobile_bridge::account_subject(pairing, scope))
            }),
        windows,
        budget,
        balance,
        local_usage: None,
        history_scope_id: observation
            .and_then(|item| item.history_scope.or(item.scope))
            .and_then(|scope| {
                pairing.and_then(|pairing| mobile_bridge::account_subject(pairing, &scope))
            }),
        history: if fresh && pairing.is_some() {
            observation
                .and_then(|item| item.history_scope.or(item.scope))
                .map(|scope| mobile_bridge::quota_history(&usage.provider_id, &scope, now))
                .unwrap_or_default()
        } else {
            Vec::new()
        },
        reset_credits: if fresh {
            observation
                .and_then(|metadata| metadata.reset_credits)
                .filter(|credits| credits.available_count <= i32::MAX.unsigned_abs())
                .map(|credits| MobileResetCredits {
                    available_count: credits.available_count,
                    expires_at_epoch_seconds: credits
                        .next_expires_at
                        .into_iter()
                        .map(|expiry| expiry.timestamp())
                        .filter(|expiry| (0..=now + 366 * 86400).contains(expiry))
                        .collect(),
                })
        } else {
            None
        },
    };
    if provider.status == "ready"
        && provider.windows.is_empty()
        && provider.budget.is_none()
        && provider.balance.is_none()
        && provider.reset_credits.is_none()
    {
        provider.status = "unavailable".into();
    }
    trim_history(&mut provider);
    provider
}

fn trim_history(provider: &mut MobileProvider) {
    provider.history.retain_mut(|series| {
        series
            .samples
            .retain(|sample| sample.t <= provider.updated_at_epoch_seconds + 120);
        !series.samples.is_empty()
            && provider
                .windows
                .iter()
                .any(|window| window.label == series.label)
    });
}

/// The desktop and paired devices read the same archive. The client cannot
/// choose an account scope or reuse an old response after an account switch.
#[tauri::command]
pub(crate) async fn get_provider_quota_history(
    app: tauri::AppHandle,
    provider_id: String,
    expected_updated_at: String,
    expected_source: Option<String>,
) -> Vec<mobile_bridge::MobileQuotaHistory> {
    if let Some(history) = crate::desktop_peer::history(
        &app,
        &provider_id,
        &expected_updated_at,
        expected_source.as_deref().unwrap_or("local"),
    ) {
        return history;
    }
    if expected_source
        .as_deref()
        .is_some_and(|source| source.starts_with("peer:"))
    {
        return Vec::new();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let Some(id) = ProviderId::from_cli_name(&provider_id) else {
            return Vec::new();
        };
        let state = app.state::<Mutex<AppState>>();
        let Ok(guard) = state.lock() else {
            return Vec::new();
        };
        let Some(usage) = guard
            .provider_cache
            .iter()
            .find(|usage| usage.provider_id == provider_id)
        else {
            return Vec::new();
        };
        let Some(scope) = desktop_history_scope(
            usage,
            guard.mobile_observations.get(&id),
            &expected_updated_at,
            &credential_stamp(),
        ) else {
            return Vec::new();
        };
        mobile_bridge::quota_history(&provider_id, &scope, chrono::Utc::now().timestamp())
    })
    .await
    .unwrap_or_default()
}

fn desktop_history_scope(
    usage: &ProviderUsageSnapshot,
    observation: Option<&MobileObservation>,
    expected_updated_at: &str,
    current_stamp: &CredentialStamp,
) -> Option<[u8; 32]> {
    let observation = observation?;
    (usage.updated_at == expected_updated_at
        && observation.fresh
        && &observation.credential_stamp == current_stamp)
        .then(|| observation.history_scope.or(observation.scope))
        .flatten()
}

pub(crate) fn record_published_history(
    app: &tauri::AppHandle,
    id: ProviderId,
    usage: &ProviderUsageSnapshot,
    captured: Option<&MobileObservation>,
    generation: u64,
) {
    if !ANDROID_PROVIDERS.contains(&id.cli_name()) {
        return;
    }
    let state = app.state::<Mutex<AppState>>();
    let Ok(guard) = state.lock() else {
        return;
    };
    let Some(observation) = captured else {
        return;
    };
    // Keep publication ownership stable through the history write. Never reinterpret
    // an older snapshot using a replacement account's observation.
    if guard.provider_refresh_generation != generation
        || !history_owner_matches(observation, guard.mobile_observations.get(&id))
        || !guard.provider_cache.iter().any(|current| {
            current.provider_id == usage.provider_id && current.updated_at == usage.updated_at
        })
    {
        return;
    }
    let Some(scope) = observation.history_scope.or(observation.scope) else {
        return;
    };
    let projected = project_usage(usage, Some(observation), None);
    if projected.status == "ready" {
        mobile_bridge::record_history(
            id.cli_name(),
            scope,
            projected.updated_at_epoch_seconds,
            &projected.windows,
        );
    }
}

fn history_owner_matches(
    captured: &MobileObservation,
    current: Option<&MobileObservation>,
) -> bool {
    current.is_some_and(|current| {
        current.fresh
            && captured.fresh
            && current.scope == captured.scope
            && current.history_scope == captured.history_scope
            && current.credential_stamp == captured.credential_stamp
    })
}

fn project_local_usage(summary: ProviderLocalUsageSummary, now: i64) -> Option<MobileLocalUsage> {
    use chrono::TimeZone;
    let updated_at = summary.token_cost_updated_at_ms / 1000;
    if !(now - 7200..=now + 120).contains(&updated_at) {
        return None;
    }
    let today = chrono::Local.timestamp_opt(updated_at, 0).single()?;
    let reporting_period = match summary.reporting_period.as_str() {
        "month-to-date" | "all" => Some(summary.reporting_period.clone()),
        raw if raw
            .strip_prefix("rolling:")
            .and_then(|days| days.parse::<u16>().ok())
            .is_some_and(|days| (1..=365).contains(&days)) =>
        {
            Some(summary.reporting_period.clone())
        }
        _ => None,
    };
    let tokens = |value: Option<u64>| value.filter(|value| *value <= i64::MAX as u64);
    let cost = |value: Option<f64>| {
        value.filter(|value| value.is_finite() && (0.0..=1e12).contains(value))
    };
    let usage = MobileLocalUsage {
        updated_at_epoch_seconds: updated_at,
        today_date: today.format("%Y-%m-%d").to_string(),
        today_tokens: tokens(summary.latest_tokens),
        thirty_day_tokens: tokens(summary.thirty_day_tokens),
        period_tokens: reporting_period
            .as_ref()
            .and_then(|_| tokens(summary.period_tokens)),
        today_cost_usd: cost(summary.today_cost),
        thirty_day_cost_usd: cost(summary.thirty_day_cost),
        period_cost_usd: reporting_period
            .as_ref()
            .and_then(|_| cost(summary.period_cost)),
        reporting_period,
        top_model: summary.top_model.filter(|model| {
            !model.is_empty()
                && model.len() <= 64
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
        }),
        incomplete_request_count: summary
            .incomplete_request_count
            .filter(|count| *count <= i32::MAX as u32),
    };
    (usage.today_tokens.is_some()
        || usage.thirty_day_tokens.is_some()
        || usage.period_tokens.is_some()
        || usage.today_cost_usd.is_some()
        || usage.thirty_day_cost_usd.is_some()
        || usage.period_cost_usd.is_some())
    .then_some(usage)
}

fn parse_time(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_archive_rejects_replaced_credentials_and_stale_requests() {
        let usage: ProviderUsageSnapshot = serde_json::from_value(serde_json::json!({
            "providerId":"codex", "primary":{"usedPercent":30.0}, "updatedAt":"current"
        }))
        .unwrap();
        let mut observation = MobileObservation::seeded(&usage);
        observation.scope = Some([1; 32]);
        let stamp = observation.credential_stamp.clone();
        assert_eq!(
            desktop_history_scope(&usage, Some(&observation), "current", &stamp),
            Some([1; 32])
        );
        assert!(desktop_history_scope(&usage, Some(&observation), "previous", &stamp).is_none());
        let different_stamp = vec![(std::path::PathBuf::from("different"), None)];
        assert!(
            desktop_history_scope(&usage, Some(&observation), "current", &different_stamp)
                .is_none()
        );
        assert!(desktop_history_scope(&usage, None, "current", &stamp).is_none());
        observation.scope = None;
        assert!(desktop_history_scope(&usage, Some(&observation), "current", &stamp).is_none());
    }
    #[test]
    fn gemini_history_uses_the_captured_connection_without_claiming_account_identity() {
        let quota = serde_json::json!([2, [[10, 0.25, 1, [[1800000000, 0]]]]]);
        let frame = format!(
            ")]}}'\n{}",
            serde_json::json!([["wrb.fr", "jSf9Qc", quota.to_string(), null]])
        );
        let result =
            codexbar::providers::geminiapps::GeminiAppsProvider::parse_browser_response(&frame)
                .unwrap();
        let observe = |selected| {
            MobileObservation::from_result(
                ProviderId::GeminiApps,
                &result,
                None,
                Vec::new(),
                selected,
            )
        };
        let a = observe(Some("connection-a"));
        assert!(a.scope.is_none());
        assert_eq!(a.history_scope, observe(Some("connection-a")).history_scope);
        assert_ne!(a.history_scope, observe(Some("connection-b")).history_scope);
        assert!(observe(None).history_scope.is_none());
    }
    #[test]
    fn local_totals_keep_their_own_clock_and_sanitize_optional_values() {
        let now = chrono::Utc::now().timestamp();
        let summary = ProviderLocalUsageSummary {
            today_cost: Some(f64::NAN),
            thirty_day_cost: Some(5.0),
            thirty_day_tokens: Some(1234),
            period_cost: Some(-1.0),
            period_tokens: Some(u64::MAX),
            reporting_period: "rolling:30".into(),
            latest_tokens: Some(100),
            top_model: Some("private@example.com".into()),
            estimate_note: "private file path".into(),
            token_cost_updated_at_ms: (now - 60) * 1000,
            incomplete_request_count: Some(2),
        };
        let usage = project_local_usage(summary.clone(), now).unwrap();
        assert_eq!(usage.updated_at_epoch_seconds, now - 60);
        assert_eq!(usage.today_tokens, Some(100));
        assert!(usage.today_cost_usd.is_none());
        assert!(usage.period_cost_usd.is_none());
        assert!(usage.period_tokens.is_none());
        assert!(usage.top_model.is_none());
        assert_eq!(usage.thirty_day_cost_usd, Some(5.0));
        assert!(!serde_json::to_string(&usage).unwrap().contains("private"));
        assert!(
            project_local_usage(
                ProviderLocalUsageSummary {
                    token_cost_updated_at_ms: (now - 7201) * 1000,
                    ..summary
                },
                now
            )
            .is_none()
        );
    }
    #[test]
    fn exported_providers_use_canonical_cache_ids() {
        for id in ANDROID_PROVIDERS {
            let provider = ProviderId::from_cli_name(id).expect("supported provider");
            assert_eq!(provider.cli_name(), *id);
        }
    }
    #[test]
    fn unknown_windows_are_omitted_and_utf16_and_optional_bounds_match_android() {
        let mut usage: ProviderUsageSnapshot = serde_json::from_value(serde_json::json!({ "providerId":"codex", "primary":{"usedPercent":0.0,"windowMinutes":0}, "errorState":"ready", "updatedAt":chrono::Utc::now().to_rfc3339(), "extraRateWindows":[{"id":"missing", "title":"Missing", "window":{"usedPercent":0.0}}] })).unwrap();
        let mut metadata = MobileObservation::seeded(&usage);
        metadata.known.primary = false;
        metadata.known.extras.clear();
        let unknown = project(&usage, Some(&metadata), &Pairing::generate());
        assert!(unknown.windows.is_empty());
        assert_eq!(unknown.status, "unavailable");
        metadata.known.primary = true;
        usage.primary_label = Some("😀".repeat(64));
        usage.primary.resets_at = Some("9999-01-01T00:00:00Z".into());
        let projected = project(&usage, Some(&metadata), &Pairing::generate());
        assert_eq!(projected.windows[0].label.encode_utf16().count(), 64);
        assert!(projected.windows[0].window_duration_seconds.is_none());
        assert!(projected.windows[0].resets_at_epoch_seconds.is_none());
        metadata.credential_stamp.clear();
        assert_eq!(
            project(&usage, Some(&metadata), &Pairing::generate()).status,
            "unavailable"
        );
    }
    #[test]
    fn configuration_rejects_global_bindings_and_invalid_pairings() {
        let mut config = Config::default();
        for address in ["0.0.0.0", "8.8.8.8", "example.com", "::"] {
            config.address = address.into();
            assert!(validate(&config).is_err());
        }
        config.address = "100.74.102.71".into();
        assert!(validate(&config).is_ok());
        config.pairing.shared_key_base64_url.clear();
        assert!(validate(&config).is_err());
    }
    #[test]
    fn projection_omits_identity_and_marks_retained_data_unavailable() {
        let now = chrono::Utc::now();
        let usage: ProviderUsageSnapshot = serde_json::from_value(serde_json::json!({ "providerId":"codex", "primary":{"usedPercent":25.0}, "updatedAt":now.to_rfc3339(), "errorState":"ready", "accountEmail":"private@example.com", "accountLabel":"private label" })).unwrap();
        let observation = MobileObservation {
            scope: Some(mobile_bridge::account_scope("codex", "private@example.com")),
            history_scope: None,
            reset_credits: Some(ResetCreditsObservation {
                available_count: 0,
                next_expires_at: None,
            }),
            fresh: true,
            credential_stamp: credential_stamp(),
            known: KnownWindows {
                primary: true,
                secondary: true,
                tertiary: true,
                model_specific: true,
                extras: std::collections::HashSet::new(),
            },
        };
        let projected = project(&usage, Some(&observation), &Pairing::generate());
        assert_eq!(projected.status, "ready");
        assert_eq!(projected.windows[0].used_fraction, 0.25);
        assert_eq!(projected.reset_credits.unwrap().available_count, 0);
        let json =
            serde_json::to_string(&project(&usage, Some(&observation), &Pairing::generate()))
                .unwrap();
        assert!(!json.contains("private"));
        let stale = MobileObservation {
            fresh: false,
            ..observation
        };
        let projected = project(&usage, Some(&stale), &Pairing::generate());
        assert_eq!(projected.status, "unavailable");
        assert!(projected.reset_credits.is_none());
        assert_eq!(projected.updated_at_epoch_seconds, now.timestamp());
    }

    #[test]
    fn a_late_observation_cannot_relabel_usage_as_a_replacement_account() {
        let usage: ProviderUsageSnapshot = serde_json::from_value(serde_json::json!({"providerId":"codex","primary":{"usedPercent":25.0},"updatedAt":chrono::Utc::now().to_rfc3339(),"errorState":"ready"})).unwrap();
        let mut a = MobileObservation::seeded(&usage);
        a.scope = Some([1; 32]);
        let mut b = a.clone();
        b.scope = Some([2; 32]);
        assert!(history_owner_matches(&a, Some(&a)));
        assert!(!history_owner_matches(&a, Some(&b)));
        b.scope = a.scope;
        b.fresh = false;
        assert!(!history_owner_matches(&a, Some(&b)));
        assert!(!history_owner_matches(&a, None));
    }

    #[test]
    fn changed_windows_cannot_invalidate_the_android_snapshot() {
        use codexbar::mobile_bridge::{MobileHistoryPoint, MobileQuotaHistory};
        let usage: ProviderUsageSnapshot = serde_json::from_value(serde_json::json!({"providerId":"codex","primary":{"usedPercent":25.0},"primaryLabel":"Session","updatedAt":chrono::Utc::now().to_rfc3339(),"errorState":"ready"})).unwrap();
        let mut provider = project_usage(&usage, Some(&MobileObservation::seeded(&usage)), None);
        let at = provider.updated_at_epoch_seconds;
        provider.history = vec![
            MobileQuotaHistory {
                label: "Old model".into(),
                samples: vec![MobileHistoryPoint {
                    t: at,
                    u: 0.2,
                    r: None,
                }],
            },
            MobileQuotaHistory {
                label: "Session".into(),
                samples: vec![
                    MobileHistoryPoint {
                        t: at,
                        u: 0.2,
                        r: None,
                    },
                    MobileHistoryPoint {
                        t: at + 121,
                        u: 0.4,
                        r: None,
                    },
                ],
            },
        ];
        trim_history(&mut provider);
        assert_eq!(provider.history.len(), 1);
        assert_eq!(provider.history[0].label, "Session");
        assert_eq!(provider.history[0].samples.len(), 1);
    }
}
