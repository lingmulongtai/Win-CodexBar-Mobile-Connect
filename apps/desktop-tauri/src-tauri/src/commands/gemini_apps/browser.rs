//! Keep Google authentication in its originating browser profile, including
//! device-bound session keys. This window has no Tauri IPC capability.
use codexbar::core::{ProviderError, ProviderFetchResult};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewUrl, WebviewWindow};
use uuid::Uuid;

const PREFIX: &str = "codexbar-gemini-browser:";
static PENDING: Mutex<Option<Uuid>> = Mutex::new(None);
static FETCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct PendingReply {
    key: String,
    deadline: Instant,
    sender: tokio::sync::oneshot::Sender<String>,
}
static REPLIES: LazyLock<Mutex<HashMap<Uuid, PendingReply>>> = LazyLock::new(Mutex::default);
static READY: LazyLock<Mutex<HashSet<Uuid>>> = LazyLock::new(Mutex::default);
static BLOCKED: LazyLock<Mutex<HashMap<Uuid, String>>> = LazyLock::new(Mutex::default);

fn pending_path() -> Result<std::path::PathBuf, String> {
    codexbar::settings::config_store_dir()
        .map(|root| root.join("gemini-browser-pending.json"))
        .ok_or_else(|| "Gemini browser storage is unavailable".into())
}

fn stored_pending() -> Option<Uuid> {
    let value = codexbar::secure_file::read_string(&pending_path().ok()?).ok()?;
    resumable_pending(
        &value,
        codexbar::settings::ManualCookies::load().get("geminiapps"),
        &codexbar::settings::config_store_dir()?,
    )
}

fn resumable_pending(
    document: &str,
    selected: Option<&str>,
    root: &std::path::Path,
) -> Option<Uuid> {
    // A failed reference-file cleanup must not turn an explicit reconnect into
    // account editing inside the already selected history scope.
    let value: String = serde_json::from_str(document).ok()?;
    let id = selection(Some(&value)).filter(|id| Some(*id) != selection(selected))?;
    root.join("gemini-browser")
        .join(id.to_string())
        .is_dir()
        .then_some(id)
}

fn forget_pending() -> Result<(), String> {
    let path = pending_path()?;
    // Revoke durably before committing a selection. A failed delete must not
    // leave a reusable reference to a connection's former history scope.
    codexbar::secure_file::write_string(&path, "null")
        .map_err(|_| "Could not securely complete the Gemini sign-in reference")?;
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("Could not remove the completed Gemini sign-in reference");
    }
    Ok(())
}

pub(crate) fn selection(header: Option<&str>) -> Option<Uuid> {
    let value = header?.strip_prefix(PREFIX)?;
    let id = Uuid::parse_str(value).ok()?;
    (id.to_string() == value).then_some(id)
}

fn marker(id: Uuid) -> String {
    format!("{PREFIX}{id}")
}

fn label(id: Uuid) -> String {
    format!("gemini-browser-{id}")
}

fn allowed_navigation(url: &tauri::Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some("gemini.google.com" | "accounts.google.com")
        )
}

fn gemini_origin(url: &tauri::Url) -> bool {
    allowed_navigation(url) && url.host_str() == Some("gemini.google.com")
}

fn default_app_route(url: &tauri::Url) -> bool {
    let accounts: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key == "authuser")
        .collect();
    gemini_origin(url)
        && (url.path() == "/app" || url.path().starts_with("/app/"))
        && (accounts.is_empty() || (accounts.len() == 1 && accounts[0].1 == "0"))
}

fn ensure_window(app: &tauri::AppHandle, id: Uuid, visible: bool) -> Result<WebviewWindow, String> {
    #[cfg(windows)]
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_some_and(|value| !value.is_empty()) {
        return Err("Gemini requires its own browser profile. Remove the WEBVIEW2_USER_DATA_FOLDER override before connecting.".into());
    }
    if let Some(window) = app.get_webview_window(&label(id)) {
        if visible {
            window.show().map_err(|_| "Could not show Gemini sign-in")?;
        }
        return Ok(window);
    }
    let directory = codexbar::settings::config_store_dir()
        .ok_or("Gemini browser storage is unavailable")?
        .join("gemini-browser")
        .join(id.to_string());
    let popup_app = app.clone();
    let builder = tauri::WebviewWindowBuilder::new(
        app,
        label(id),
        WebviewUrl::External("https://gemini.google.com/app".parse().unwrap()),
    )
    .title(format!(
        "Gemini — {}",
        app.config().product_name.as_deref().unwrap_or("CodexBar")
    ))
    .inner_size(1024.0, 760.0);
    // WKWebView ignores data_directory. Each sign-in needs its own persistent
    // macOS 14+ website data store, including device-bound session credentials.
    #[cfg(target_os = "macos")]
    let builder = {
        std::fs::create_dir_all(&directory)
            .map_err(|_| "Could not create Gemini profile reference")?;
        builder.data_store_identifier(*id.as_bytes())
    };
    #[cfg(not(target_os = "macos"))]
    let builder = builder.data_directory(directory);
    let window = builder
        .visible(visible)
        .focused(false)
        .on_navigation(move |url| {
            if receive(id, url) {
                return false;
            }
            let allowed = allowed_navigation(url);
            if !allowed {
                // Never log paths, query strings, fragments or the authentication URL.
                let host = url.host_str().unwrap_or("unknown").to_string();
                tracing::warn!(host, "Gemini sign-in navigation blocked");
                if let Ok(mut blocked) = BLOCKED.lock() {
                    blocked.insert(id, host);
                }
            }
            allowed
        })
        .on_new_window(move |url, _| {
            // A user-initiated Google sign-in popup stays inside this isolated profile.
            if allowed_navigation(&url)
                && let Some(window) = popup_app.get_webview_window(&label(id))
            {
                let _ = window.navigate(url);
            }
            tauri::webview::NewWindowResponse::Deny
        })
        .on_page_load(move |_, payload| {
            if let Ok(mut ready) = READY.lock() {
                if payload.event() == tauri::webview::PageLoadEvent::Finished
                    && default_app_route(payload.url())
                {
                    ready.insert(id);
                } else {
                    ready.remove(&id);
                }
            }
        })
        .build()
        .map_err(|_| "Could not open the Gemini browser")?;
    let handle = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = handle.hide();
        }
    });
    Ok(window)
}

fn receive(id: Uuid, url: &tauri::Url) -> bool {
    if !gemini_origin(url) || url.path() != "/__codexbar_usage_result" {
        return false;
    }
    if let Some(fragment) = url.fragment().filter(|value| value.len() <= 1024 * 1024) {
        let mut parsed = url.clone();
        parsed.set_query(Some(fragment));
        let pairs: Vec<_> = parsed.query_pairs().collect();
        if pairs.len() == 2
            && pairs[0].0 == "request"
            && pairs[1].0 == "reply"
            && let Ok(mut pending) = REPLIES.lock()
            && pending
                .get(&id)
                .is_some_and(|entry| entry.key == pairs[0].1 && Instant::now() <= entry.deadline)
            && let Some(entry) = pending.remove(&id)
        {
            let _ = entry.sender.send(pairs[1].1.to_string());
        }
    }
    true // Cancel malformed, expired, oversized and replayed callbacks too.
}

struct Request {
    id: Uuid,
    window: WebviewWindow,
    encoded_key: String,
}
impl Drop for Request {
    fn drop(&mut self) {
        if let Ok(mut pending) = REPLIES.lock() {
            pending.remove(&self.id);
        }
        let key = &self.encoded_key;
        let _ = self
            .window
            .eval(format!("window[{key}]?.abort?.(); delete window[{key}];"));
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind")]
enum Reply {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "ok")]
    Ok { body: String },
    #[serde(rename = "auth")]
    Auth,
    #[serde(rename = "network")]
    Network,
    #[serde(rename = "http")]
    Http,
    #[serde(rename = "too-large")]
    TooLarge,
}

pub(crate) async fn fetch(
    app: &tauri::AppHandle,
    id: Uuid,
) -> Result<ProviderFetchResult, ProviderError> {
    let _fetch = FETCH.lock().await;
    let window = ensure_window(app, id, false).map_err(ProviderError::Other)?;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if window.url().is_ok_and(|url| {
                url.host_str() == Some("accounts.google.com")
                    || (gemini_origin(&url) && !default_app_route(&url))
            }) {
                return Err(ProviderError::AuthRequired);
            }
            if READY.lock().is_ok_and(|ready| ready.contains(&id)) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| ProviderError::Timeout)??;
    if !window.url().is_ok_and(|url| default_app_route(&url)) {
        return Err(ProviderError::AuthRequired);
    }
    let key = format!("__codexbar_gemini_{}", Uuid::new_v4().simple());
    let encoded_key = serde_json::to_string(&key).unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    REPLIES
        .lock()
        .map_err(|_| ProviderError::Other("Gemini browser is unavailable.".into()))?
        .insert(
            id,
            PendingReply {
                key,
                deadline: Instant::now() + Duration::from_secs(20),
                sender,
            },
        );
    let _request = Request {
        id,
        window: window.clone(),
        encoded_key: encoded_key.clone(),
    };
    window
        .eval(include_str!("browser_probe.js").replace("__CODEXBAR_REQUEST_KEY__", &encoded_key))
        .map_err(|_| ProviderError::Other("Gemini browser is unavailable.".into()))?;
    let value = tokio::time::timeout(Duration::from_secs(20), receiver)
        .await
        .map_err(|_| ProviderError::Timeout)?
        .map_err(|_| ProviderError::Timeout)?;
    if value.len() > 256 * 1024 {
        return Err(ProviderError::Parse(
            "Gemini browser response is too large.".into(),
        ));
    }
    if !window.url().is_ok_and(|url| default_app_route(&url)) {
        return Err(ProviderError::AuthRequired);
    }
    match serde_json::from_str::<Reply>(&value)
        .map_err(|_| ProviderError::Parse("Invalid Gemini browser response.".into()))?
    {
        Reply::Ok { body } => {
            codexbar::providers::geminiapps::GeminiAppsProvider::parse_browser_response(&body)
        }
        Reply::Auth => Err(ProviderError::AuthRequired),
        Reply::Pending | Reply::Network | Reply::Http => Err(ProviderError::Other(
            "Gemini browser request failed. Retrying on the next refresh.".into(),
        )),
        Reply::TooLarge => Err(ProviderError::Parse(
            "Gemini usage response is too large.".into(),
        )),
    }
}

pub(super) fn begin(app: &tauri::AppHandle) -> Result<(), String> {
    let mut pending = PENDING
        .lock()
        .map_err(|_| "Gemini connection is unavailable")?;
    // A new connection gets its own profile. Opening sign-in must never let an
    // account switch publish usage under the old connection's history scope.
    let id = (*pending)
        .or_else(stored_pending)
        .unwrap_or_else(Uuid::new_v4);
    ensure_window(app, id, true)?;
    *pending = Some(id);
    codexbar::secure_file::write_string(
        &pending_path()?,
        &serde_json::to_string(&marker(id))
            .map_err(|_| "Could not save Gemini sign-in reference")?,
    )
    .map_err(|_| "Could not save Gemini sign-in reference")?;
    Ok(())
}

pub(super) async fn finish(app: &tauri::AppHandle) -> Result<String, String> {
    let id = PENDING
        .lock()
        .map_err(|_| "Gemini connection is unavailable")?
        .or_else(stored_pending)
        .ok_or("Open Gemini sign-in first")?;
    let window = ensure_window(app, id, false)?;
    // Only the user's explicit Finish action leaves a stuck sign-in page.
    // Reuse the same profile; an anonymous /app response still fails verification.
    if window
        .url()
        .is_ok_and(|url| url.host_str() == Some("accounts.google.com"))
    {
        READY
            .lock()
            .map_err(|_| "Gemini browser is unavailable")?
            .remove(&id);
        window
            .navigate("https://gemini.google.com/app".parse().unwrap())
            .map_err(|_| "Could not return to Gemini after sign-in")?;
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if window.url().is_ok_and(|url| default_app_route(&url))
                    && READY.lock().is_ok_and(|ready| ready.contains(&id))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.map_err(|_| "Google sign-in has not returned to Gemini. Reopen its window and finish signing in before retrying.")?;
    }
    fetch(app, id)
        .await
        .map_err(|error| {
            if let Some(host) = BLOCKED.lock().ok().and_then(|blocked| blocked.get(&id).cloned()) {
                format!("Google sign-in could not finish because a redirect to {host} was blocked. Your previous connection has been kept.")
            } else {
                match error {
                    ProviderError::AuthRequired => "Google sign-in has not completed in this browser. Your previous connection has been kept.".into(),
                    ProviderError::Timeout => "Gemini did not finish loading. Reopen the dedicated window and retry Finish connecting.".into(),
                    _ => "Gemini usage could not be verified. Your previous connection has been kept; retry after the page has loaded.".into(),
                }
            }
        })?;
    forget_pending()?;
    Ok(marker(id))
}

pub(super) fn complete(app: &tauri::AppHandle) {
    if let Ok(mut pending) = PENDING.lock()
        && let Some(id) = pending.take()
        && let Some(window) = app.get_webview_window(&label(id))
    {
        let _ = window.hide();
    }
}

pub(super) async fn retire(app: &tauri::AppHandle, id: Uuid) -> Result<(), String> {
    if let Ok(mut blocked) = BLOCKED.lock() {
        blocked.remove(&id);
    }
    if let Ok(mut ready) = READY.lock() {
        ready.remove(&id);
    }
    if let Some(window) = app.get_webview_window(&label(id)) {
        let _ = window.clear_all_browsing_data();
        window
            .destroy()
            .map_err(|_| "Could not close the Gemini browser session")?;
    }
    let root = codexbar::settings::config_store_dir()
        .ok_or("Gemini browser storage is unavailable")?
        .join("gemini-browser");
    #[cfg(target_os = "macos")]
    {
        let mut removed = !app
            .fetch_data_store_identifiers()
            .await
            .map_err(|_| "Could not inspect Gemini website data stores")?
            .contains(id.as_bytes());
        for _ in 0..30 {
            if removed {
                break;
            }
            if app.remove_data_store(*id.as_bytes()).await.is_ok() {
                removed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Preserve the UUID reference for a subsequent disconnect/cleanup retry.
        if !removed {
            return Err(
                "Could not clear the Gemini website data store. Disconnect again to retry.".into(),
            );
        }
    }
    remove_profile(&root, id)
}

fn remove_profile(root: &std::path::Path, id: Uuid) -> Result<(), String> {
    let target = root.join(id.to_string());
    if !target.exists() {
        return Ok(());
    }
    // Only delete this app's exact canonical UUID child, never a linked location.
    let root = root
        .canonicalize()
        .map_err(|_| "Gemini browser storage is unavailable")?;
    let target = target
        .canonicalize()
        .map_err(|_| "Gemini browser storage is unavailable")?;
    if target.parent() != Some(root.as_path())
        || target.file_name().and_then(|v| v.to_str()) != Some(&id.to_string())
    {
        return Err("Refusing to clear a browser profile outside Gemini storage".into());
    }
    // WebView2 closes asynchronously. Verify removal rather than relying on its
    // fire-and-forget ClearBrowsingDataAll completion callback.
    for _ in 0..30 {
        match std::fs::remove_dir_all(&target) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    Err("Gemini was disconnected, but its browser data is still busy. Disconnect again to retry cleanup.".into())
}

pub(super) async fn clear(app: &tauri::AppHandle) -> Result<(), String> {
    let selected = codexbar::settings::ManualCookies::load();
    let selected = selection(selected.get("geminiapps"));
    let pending = PENDING
        .lock()
        .map_err(|_| "Gemini connection is unavailable")?
        .take()
        .or_else(stored_pending);
    forget_pending()?;
    let mut profiles: HashSet<_> = [selected, pending].into_iter().flatten().collect();
    // Include profiles from an interrupted setup or a previously failed cleanup.
    if let Some(root) = codexbar::settings::config_store_dir()
        && let Ok(entries) = std::fs::read_dir(root.join("gemini-browser"))
    {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && let Some(name) = entry.file_name().to_str()
                && let Some(id) = selection(Some(&format!("{PREFIX}{name}")))
            {
                profiles.insert(id);
            }
        }
    }
    let mut result = Ok(());
    for id in profiles {
        if let Err(error) = retire(app, id).await {
            result = Err(error);
        }
    }
    result
}

pub(super) async fn cancel(app: &tauri::AppHandle) -> Result<(), String> {
    let id = PENDING
        .lock()
        .map_err(|_| "Gemini connection is unavailable")?
        .or_else(stored_pending);
    if let Some(id) = id {
        if selection(codexbar::settings::ManualCookies::load().get("geminiapps")) == Some(id) {
            *PENDING
                .lock()
                .map_err(|_| "Gemini connection is unavailable")? = None;
            forget_pending()?;
            return Ok(());
        }
        retire(app, id).await?;
        *PENDING
            .lock()
            .map_err(|_| "Gemini connection is unavailable")? = None;
        forget_pending()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_an_uncommitted_pending_profile_can_resume() {
        let pending = Uuid::new_v4();
        let selected = Uuid::new_v4();
        let root = std::env::temp_dir().join(format!("gemini-pending-test-{}", Uuid::new_v4()));
        let profile = root.join("gemini-browser").join(pending.to_string());
        std::fs::create_dir_all(&profile).unwrap();
        let reference = serde_json::to_string(&marker(pending)).unwrap();
        assert_eq!(resumable_pending(&reference, None, &root), Some(pending));
        assert_eq!(
            resumable_pending(&reference, Some(&marker(selected)), &root),
            Some(pending)
        );
        assert!(resumable_pending(&reference, Some(&marker(pending)), &root).is_none());
        assert!(resumable_pending("null", None, &root).is_none());
        assert!(resumable_pending("\"../../../other\"", None, &root).is_none());
        assert!(resumable_pending("\"SID=private\"", None, &root).is_none());
        std::fs::remove_dir(&profile).unwrap();
        // A stale reference surviving deletion cannot recreate a revoked scope.
        assert!(resumable_pending(&reference, None, &root).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn default_quota_never_substitutes_for_an_indexed_or_ambiguous_account() {
        for url in [
            "https://gemini.google.com/app",
            "https://gemini.google.com/app/chat?authuser=0",
        ] {
            assert!(default_app_route(&url.parse().unwrap()));
        }
        for url in [
            "https://gemini.google.com/u/1/app",
            "https://gemini.google.com/app?authuser=1",
            "https://gemini.google.com/app?authuser=0&authuser=1",
            "https://gemini.google.com/app?authuser=0&authuser=0",
            "https://gemini.google.com/app?authuser=private@example.test",
        ] {
            assert!(!default_app_route(&url.parse().unwrap()));
        }
    }
    #[test]
    fn retirement_removes_only_the_exact_owned_profile() {
        let root = std::env::temp_dir().join(format!("gemini-profile-test-{}", Uuid::new_v4()));
        let id = Uuid::new_v4();
        let profile = root.join(id.to_string());
        let other = root.join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(profile.join("synthetic-cookie"), "test").unwrap();
        remove_profile(&root, id).unwrap();
        assert!(!profile.exists());
        assert!(other.is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn navigation_never_allows_local_ipc_other_origins_or_insecure_login() {
        for value in [
            "https://gemini.google.com/app",
            "https://accounts.google.com/v3/signin",
        ] {
            assert!(allowed_navigation(&value.parse().unwrap()));
        }
        for value in [
            "http://gemini.google.com",
            "https://gemini.google.com:444",
            "https://gemini.google.com.attacker.test",
            "http://localhost:1420",
            "tauri://localhost",
            "https://user:password@gemini.google.com",
            "file:///C:/secret",
        ] {
            assert!(!allowed_navigation(&value.parse().unwrap()), "{value}");
        }
        assert!(!gemini_origin(
            &"https://accounts.google.com".parse().unwrap()
        ));
    }
    #[test]
    fn selection_is_an_opaque_canonical_connection_id_and_never_a_cookie() {
        let id = Uuid::new_v4();
        assert_eq!(selection(Some(&marker(id))), Some(id));
        for header in [
            "SID=private",
            "codexbar-gemini-browser:../../../other",
            "codexbar-gemini-browser:",
        ] {
            assert!(selection(Some(header)).is_none());
        }
        let permissions = include_str!("../../../capabilities/default.json");
        assert!(!permissions.contains("gemini-browser"));
        assert!(!permissions.contains("remote"));
    }

    #[test]
    fn callbacks_require_the_request_nonce_profile_and_deadline_and_are_one_shot() {
        let id = Uuid::new_v4();
        let key = Uuid::new_v4().to_string();
        let (sender, mut receiver) = tokio::sync::oneshot::channel();
        REPLIES.lock().unwrap().insert(
            id,
            PendingReply {
                key: key.clone(),
                deadline: Instant::now() + Duration::from_secs(20),
                sender,
            },
        );
        let url: tauri::Url =
            format!("https://gemini.google.com/__codexbar_usage_result#request={key}&reply=quota")
                .parse()
                .unwrap();
        assert!(receive(Uuid::new_v4(), &url));
        assert!(receiver.try_recv().is_err());
        let mut wrong = url.clone();
        wrong.set_fragment(Some("request=wrong&reply=quota"));
        assert!(receive(id, &wrong));
        assert!(receiver.try_recv().is_err());
        REPLIES.lock().unwrap().get_mut(&id).unwrap().deadline =
            Instant::now() - Duration::from_secs(1);
        assert!(receive(id, &url));
        assert!(receiver.try_recv().is_err());
        REPLIES.lock().unwrap().get_mut(&id).unwrap().deadline =
            Instant::now() + Duration::from_secs(20);
        assert!(receive(id, &url));
        assert_eq!(receiver.try_recv().unwrap(), "quota");
        assert!(!REPLIES.lock().unwrap().contains_key(&id));
        assert!(receive(id, &url));
        assert!(!receive(
            id,
            &"https://accounts.google.com/__codexbar_usage_result"
                .parse()
                .unwrap()
        ));
        assert!(!receive(
            id,
            &"https://gemini.google.com/app".parse().unwrap()
        ));
    }
}
