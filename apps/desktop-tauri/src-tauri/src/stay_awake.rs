//! Stay Awake scheduler: keeps Windows from idle-sleeping while a local agent
//! (Codex, Claude, Pi/OMP) process is running.
//!
//! While `stay_awake_enabled` is on, a local-only process scan runs every 30
//! seconds, independent of `agent_sessions_enabled`. It never touches SSH or
//! Tailscale hosts and never reads transcript files, so file-only rollouts and
//! remote sessions cannot hold the machine awake. The policy itself lives in
//! `codexbar::power_assertion`. The setting reaches the controller through
//! [`settings_changed`] (every save goes through `update_settings`) and once at
//! startup; the scan loop never re-reads the file, so it cannot re-enable a
//! toggle the user just turned off.

use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use codexbar::agent_sessions::LocalAgentSessionScanner;
use codexbar::power_assertion::{StayAwake, SystemPowerAssertion};
use codexbar::settings::Settings;
use tokio::sync::Notify;

const SCAN_INTERVAL: Duration = Duration::from_secs(30);

type Controller = StayAwake<SystemPowerAssertion>;

static CONTROLLER: OnceLock<Mutex<Controller>> = OnceLock::new();
static WAKE: Notify = Notify::const_new();

fn controller() -> MutexGuard<'static, Controller> {
    CONTROLLER
        .get_or_init(|| Mutex::new(StayAwake::new(SystemPowerAssertion::new())))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether an idle-sleep prevention is currently held (tray status line).
pub fn is_held() -> bool {
    controller().is_held()
}

#[tauri::command]
pub fn get_stay_awake_status() -> bool {
    is_held()
}

fn publish_status_change(app: &tauri::AppHandle) {
    crate::tray_bridge::rebuild_tray_menu(app);
    crate::events::emit_stay_awake_changed(app, is_held());
}

/// Apply a saved settings change: turning Stay Awake off releases right away,
/// turning it on triggers a scan without waiting for the next interval.
pub fn settings_changed(app: &tauri::AppHandle, enabled: bool) {
    let enabled = enabled && cfg!(windows);
    let changed = controller().set_enabled(enabled);
    if changed {
        publish_status_change(app);
    }
    if enabled {
        WAKE.notify_one();
    }
}

/// Release for good on app exit.
pub fn shutdown() {
    controller().shutdown();
}

pub fn install(app: tauri::AppHandle) {
    if !cfg!(windows) {
        return;
    }
    controller().set_enabled(Settings::load().stay_awake_enabled);
    tauri::async_runtime::spawn(async move {
        loop {
            scan_once(&app).await;
            // Sleep one interval, or wake early when the setting is turned on.
            tokio::time::timeout(SCAN_INTERVAL, WAKE.notified())
                .await
                .ok();
        }
    });
}

async fn scan_once(app: &tauri::AppHandle) {
    if !controller().is_enabled() {
        return;
    }
    let changed =
        match tokio::task::spawn_blocking(LocalAgentSessionScanner::has_live_agent_process).await {
            // The controller drops this result if the setting was turned off or
            // the app shut down while the scan was running.
            Ok(Ok(live)) => controller().observe(live),
            Ok(Err(error)) => {
                tracing::warn!("Stay Awake process scan failed: {error}");
                false
            }
            Err(error) => {
                tracing::warn!("Stay Awake scan task failed: {error}");
                false
            }
        };
    if changed {
        publish_status_change(app);
    }
}
