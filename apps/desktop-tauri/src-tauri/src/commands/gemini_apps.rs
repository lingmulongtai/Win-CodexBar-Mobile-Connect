use super::*;
use codexbar::providers::geminiapps::{GeminiAppsProvider, desktop_session};

pub(super) mod browser;

static MUTATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn changed(app: &tauri::AppHandle, header: Option<&str>) -> Result<(), String> {
    let pending = {
        let state = app.state::<Mutex<AppState>>();
        let mut state = state.lock().map_err(|_| "Usage state unavailable")?;
        codexbar::providers::geminiapps::session_cache::select(header)
            .map_err(|_| "Could not save Gemini Apps session securely")?;
        super::invalidate_account_usage(&mut state, ProviderId::GeminiApps)
    };
    crate::events::emit_provider_updated(app, &pending);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = super::refresh_providers(handle).await;
    });
    Ok(())
}

/// A deliberate user action imports only Gemini's scoped session. Verify the
/// session before replacing the saved one; never send its values to the UI.
#[tauri::command]
pub async fn connect_gemini_apps(app: tauri::AppHandle) -> Result<(), String> {
    let _mutation = MUTATION
        .try_lock()
        .map_err(|_| "Gemini Apps connection is already in progress")?;
    let header = tauri::async_runtime::spawn_blocking(desktop_session::import)
        .await
        .map_err(|_| "Gemini session import failed")??;
    let header = GeminiAppsProvider::verify_session(&header, FetchContext::default().web_timeout)
        .await
        .map_err(|error| codexbar::logging::safe_error_message(&error))?;
    replace(&app, &header).await
}

#[tauri::command]
pub fn list_gemini_browser_profiles(browser_type: String) -> Result<Vec<String>, String> {
    use codexbar::browser::detection::{BrowserDetector, BrowserType};
    let kind = BrowserType::all()
        .iter()
        .copied()
        .find(|kind| kind.key() == browser_type && kind.is_chromium_based())
        .ok_or("Choose a supported Chromium browser")?;
    let browser = BrowserDetector::detect(kind).ok_or("Selected browser is unavailable")?;
    Ok(browser
        .profiles
        .into_iter()
        .map(|profile| profile.name)
        .collect())
}

#[tauri::command]
pub async fn connect_gemini_apps_from_browser(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    browser_type: String,
    profile_name: String,
) -> Result<(), String> {
    trusted_window(&window)?;
    let _mutation = MUTATION
        .try_lock()
        .map_err(|_| "Gemini Apps connection is already in progress")?;
    let header = tauri::async_runtime::spawn_blocking(move || {
        desktop_session::import_browser(&browser_type, &profile_name)
    })
    .await
    .map_err(|_| "Gemini browser import failed")??;
    let header = GeminiAppsProvider::verify_session(&header, FetchContext::default().web_timeout)
        .await
        .map_err(|error| codexbar::logging::safe_error_message(&error))?;
    replace(&app, &header).await
}

#[tauri::command]
pub async fn disconnect_gemini_apps(app: tauri::AppHandle) -> Result<(), String> {
    revoke(&app).await
}

pub(super) async fn revoke(app: &tauri::AppHandle) -> Result<(), String> {
    let _mutation = MUTATION
        .try_lock()
        .map_err(|_| "Gemini Apps connection is already in progress")?;
    // Revoke first, even if the browser's subsequent cleanup fails.
    changed(app, None)?;
    browser::clear(app).await
}

async fn replace(app: &tauri::AppHandle, header: &str) -> Result<(), String> {
    let old = ManualCookies::load();
    let old = browser::selection(old.get("geminiapps"));
    changed(app, Some(header))?;
    // Only retire the previous profile once its replacement has been saved.
    if let Some(old) = old.filter(|id| Some(*id) != browser::selection(Some(header)))
        && browser::retire(app, old).await.is_err()
    {
        tracing::warn!("Could not clear the previous Gemini browser; disconnect can retry cleanup");
    }
    if browser::selection(Some(header)).is_none() && browser::cancel(app).await.is_err() {
        tracing::warn!("Could not clear unfinished Gemini sign-in; disconnect can retry cleanup");
    }
    Ok(())
}

fn trusted_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    let url = window.url().map_err(|_| "Connection window unavailable")?;
    if matches!(window.label(), "main" | "settings")
        && (matches!(url.scheme(), "tauri" | "http" | "https")
            && matches!(url.host_str(), Some("tauri.localhost" | "localhost")))
    {
        Ok(())
    } else {
        Err("Connection commands are available only in CodexBar settings".into())
    }
}

#[tauri::command]
pub async fn begin_gemini_browser(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    trusted_window(&window)?;
    let _mutation = MUTATION
        .try_lock()
        .map_err(|_| "Gemini Apps connection is already in progress")?;
    browser::begin(&app)
}

#[tauri::command]
pub async fn finish_gemini_browser(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    trusted_window(&window)?;
    let _mutation = MUTATION
        .try_lock()
        .map_err(|_| "Gemini Apps connection is already in progress")?;
    let marker = browser::finish(&app).await?;
    replace(&app, &marker).await?;
    browser::complete(&app);
    Ok(())
}

#[tauri::command]
pub async fn cancel_gemini_browser(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    trusted_window(&window)?;
    let _mutation = MUTATION
        .try_lock()
        .map_err(|_| "Gemini Apps connection is already in progress")?;
    browser::cancel(&app).await
}
