//! Muse (muse.ai) weekly usage from the signed-in web app.
//!
//! Ported from upstream CodexBar v0.73.0 (`Resources/Plugins/museai.js`,
//! `Providers/MuseAI/MuseAIProviderDescriptor.swift`). This is Meta's
//! personal agent at muse.ai, separate from Muse Code (`providers/muse`).
//! muse.ai has no usage API: the settings dialog's Next.js server action
//! `fetchSubscriptionAction` is posted with the session cookies. Its ID
//! changes on every deploy, so a stale ID (`404 Server action not found.`)
//! triggers a bounded scan of the page's chunks for the new one. Upstream
//! keeps the ID in plugin storage; Windows keeps it in memory for the life of
//! the app. Static chunks receive no cookies, and neither cookies nor
//! response bodies are logged or echoed in errors.

mod model;
#[cfg(test)]
mod tests;

use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{
    Client, RequestBuilder,
    header::{ACCEPT, CONTENT_TYPE, COOKIE, LOCATION, ORIGIN, USER_AGENT},
};

use crate::browser::session_scan::BrowserCookieStore;
use crate::core::{
    FetchContext, ManualEmptyCookiePolicy, Provider, ProviderError, ProviderFetchResult,
    ProviderId, ProviderMetadata, ProviderStateKind, SourceMode,
};
use crate::providers::{
    BoundedBodyError, BrowserCookieSessions, browser_cookie_sessions, normalize_cookie_header,
    read_bounded_response,
};

const BASE_URL: &str = "https://muse.ai";
const ORIGIN_VALUE: &str = "https://muse.ai";
const COOKIE_DOMAIN: &str = "muse.ai";
const DASHBOARD_URL: &str = "https://muse.ai/?settings_tab=general";
const SIGN_IN_URL: &str = "https://muse.ai";
/// A redirect here means muse.ai treats the session as signed out.
const SIGN_IN_REDIRECT: &str = "https://auth.muse.ai/";
/// Upstream sends a fixed Chrome 143 User-Agent (macOS); Windows sends the
/// Windows form of the same version.
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";
const ACTION_BODY: &str = r#"[{"includeAgreement":true}]"#;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Upstream discovery limits.
const MAX_PAGE_CHUNKS: usize = 96;
const MAX_SETTINGS_CHUNKS: usize = 32;
const CHUNK_BATCH: usize = 8;
const MAX_DISCOVERY_TEXT: usize = 8 * 1024 * 1024;
const MAX_ACTION_BYTES: usize = 1024 * 1024;

const SESSION_EXPIRED: &str = "muse.ai session expired. Sign in at muse.ai and refresh.";
const MISSING_SESSION: &str = "No muse.ai session found. Sign in at muse.ai in the selected browser, or paste a Cookie header in Settings.";
const COOKIES_OFF: &str = "muse.ai cookies are disabled.";

/// The last working action ID, shared by every provider instance in the app.
static SHARED_ACTION_ID: LazyLock<Arc<Mutex<Option<String>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(None)));

pub struct MuseAIProvider {
    metadata: ProviderMetadata,
    client: Option<Client>,
    base_url: String,
    action_id: Arc<Mutex<Option<String>>>,
}

impl MuseAIProvider {
    pub fn new() -> Self {
        let client = crate::core::credentialed_http_client_builder()
            .cookie_store(false)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok();
        Self::with_parts(BASE_URL, client, Arc::clone(&SHARED_ACTION_ID))
    }

    #[cfg(test)]
    fn with_client(base_url: &str, client: Client, action_id: Option<&str>) -> Self {
        Self::with_parts(
            base_url.trim_end_matches('/'),
            Some(client),
            Arc::new(Mutex::new(action_id.map(str::to_owned))),
        )
    }

    fn with_parts(
        base_url: &str,
        client: Option<Client>,
        action_id: Arc<Mutex<Option<String>>>,
    ) -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::MuseAI,
                display_name: "Muse (muse.ai)",
                session_label: "Weekly",
                weekly_label: "Plan",
                supports_opus: false,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some(DASHBOARD_URL),
                status_page_url: None,
                tertiary_label_key: None,
            },
            client,
            base_url: base_url.to_owned(),
            action_id,
        }
    }

    fn stored_action_id(&self) -> Option<String> {
        self.action_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn store_action_id(&self, id: Option<String>) {
        *self
            .action_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = id;
    }

    async fn fetch_web(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        let client = self.client.as_ref().ok_or_else(|| {
            ProviderError::Other("Could not create a secure muse.ai HTTP client.".into())
        })?;
        let site = Site {
            client,
            base_url: &self.base_url,
            timeout: Duration::from_secs(ctx.web_timeout.max(1)).min(REQUEST_TIMEOUT),
        };
        if let Some(raw) = ctx.manual_cookie_header.as_deref() {
            let header = normalize_cookie_header(raw).ok_or_else(missing_session)?;
            return self
                .try_session(&site, &header)
                .await?
                .ok_or_else(|| ProviderError::Other(SESSION_EXPIRED.into()));
        }
        self.fetch_browser_sessions(&site, browser_cookie_sessions(ctx, &[COOKIE_DOMAIN]))
            .await
    }

    async fn fetch_browser_sessions<S: BrowserCookieStore>(
        &self,
        site: &Site<'_>,
        mut sessions: BrowserCookieSessions<S>,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let mut rejected = false;
        for header in sessions
            .by_ref()
            .filter_map(|header| normalize_cookie_header(&header))
        {
            if let Some(result) = self.try_session(site, &header).await? {
                return Ok(result);
            }
            rejected = true;
        }
        match sessions.exhaustion_error_with_lock() {
            ProviderError::NoCookies if rejected => {
                Err(ProviderError::Other(SESSION_EXPIRED.into()))
            }
            ProviderError::NoCookies => Err(missing_session()),
            error => Err(error),
        }
    }

    async fn try_session(
        &self,
        site: &Site<'_>,
        cookie: &str,
    ) -> Result<Option<ProviderFetchResult>, ProviderError> {
        let Some((reply, action_id)) = self.subscription(site, cookie).await? else {
            return Ok(None);
        };
        if reply.status != 200 {
            return Err(http_failure(reply.status));
        }
        let result = model::result(&reply.body)?;
        // Upstream saves the ID only after a usable subscription response.
        self.store_action_id(Some(action_id));
        Ok(Some(result))
    }

    /// Upstream `subscription`: the stored action first, then one
    /// rediscovery. `None` means muse.ai treated the session as signed out.
    async fn subscription(
        &self,
        site: &Site<'_>,
        cookie: &str,
    ) -> Result<Option<(Reply, String)>, ProviderError> {
        if let Some(action_id) = self.stored_action_id() {
            let reply = site.post_action(cookie, &action_id).await?;
            if !reply.is_stale_action() {
                return Ok((!reply.is_signed_out()).then_some((reply, action_id)));
            }
        }
        self.store_action_id(None);
        let Some(action_id) = discover(site, cookie).await? else {
            return Ok(None);
        };
        let reply = site.post_action(cookie, &action_id).await?;
        if reply.is_stale_action() {
            return Err(model::discovery_failure());
        }
        Ok((!reply.is_signed_out()).then_some((reply, action_id)))
    }
}

impl Default for MuseAIProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for MuseAIProvider {
    fn id(&self) -> ProviderId {
        ProviderId::MuseAI
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        match ctx.source_mode {
            SourceMode::Auto | SourceMode::Web => self.fetch_web(ctx).await,
            // The shell maps an Off cookie source to Cli for web-only
            // providers; upstream reports that cookies are disabled.
            SourceMode::Cli => Err(ProviderError::Other(COOKIES_OFF.into())),
            SourceMode::OAuth => Err(ProviderError::UnsupportedSource(SourceMode::OAuth)),
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Web]
    }

    fn supports_web(&self) -> bool {
        true
    }

    fn owns_browser_cookie_resolution(&self) -> bool {
        true
    }

    /// An empty Manual source fails closed with the sign-in message instead
    /// of reporting that cookies are disabled.
    fn manual_empty_cookie_policy(&self) -> ManualEmptyCookiePolicy {
        ManualEmptyCookiePolicy::FailClosedWeb
    }

    fn error_state_kind(&self, error: &ProviderError) -> ProviderStateKind {
        match error {
            ProviderError::Other(message) if message == SESSION_EXPIRED => {
                ProviderStateKind::ExpiredSession
            }
            _ => error.state_kind(),
        }
    }
}

fn missing_session() -> ProviderError {
    ProviderError::BrowserSignInRequired {
        message: MISSING_SESSION.into(),
        sign_in_url: SIGN_IN_URL.into(),
    }
}

fn http_failure(status: u16) -> ProviderError {
    ProviderError::Other(format!("muse.ai returned HTTP {status}."))
}

/// Upstream `discover`: the page's chunks lead to the lazy settings module,
/// whose chunk holds the action ID. `None` means the session is signed out.
async fn discover(site: &Site<'_>, cookie: &str) -> Result<Option<String>, ProviderError> {
    let page = site.page(cookie).await?;
    if page.is_signed_out() {
        return Ok(None);
    }
    if page.status != 200 {
        return Err(http_failure(page.status));
    }
    let paths = model::chunk_paths(&page.body);
    let paths = &paths[..paths.len().min(MAX_PAGE_CHUNKS)];
    let mut text = String::new();
    for batch in paths.chunks(CHUNK_BATCH) {
        text.push('\n');
        text.push_str(&site.chunks(batch).await.join("\n"));
        if text.len() > MAX_DISCOVERY_TEXT {
            return Err(model::discovery_failure());
        }
        let Some(settings) = model::settings_chunk_paths(&text) else {
            continue;
        };
        let settings = &settings[..settings.len().min(MAX_SETTINGS_CHUNKS)];
        for batch in settings.chunks(CHUNK_BATCH) {
            if let Some(id) = site
                .chunks(batch)
                .await
                .iter()
                .find_map(|body| model::action_id(body))
            {
                return Ok(Some(id));
            }
        }
        break;
    }
    Err(model::discovery_failure())
}

struct Reply {
    status: u16,
    location: Option<String>,
    body: String,
}

impl Reply {
    /// Upstream `signedOut`: 401, 403 or a redirect to the sign-in host.
    fn is_signed_out(&self) -> bool {
        matches!(self.status, 401 | 403)
            || self
                .location
                .as_deref()
                .is_some_and(|location| location.starts_with(SIGN_IN_REDIRECT))
    }

    /// Upstream `staleAction`: the deploy no longer knows this action ID.
    fn is_stale_action(&self) -> bool {
        self.status == 404
            && self
                .body
                .to_ascii_lowercase()
                .contains("server action not found")
    }
}

struct Site<'a> {
    client: &'a Client,
    base_url: &'a str,
    timeout: Duration,
}

impl Site<'_> {
    async fn post_action(&self, cookie: &str, action_id: &str) -> Result<Reply, ProviderError> {
        let request = self
            .client
            .post(format!("{}/", self.base_url))
            .header(COOKIE, cookie)
            .header(ACCEPT, "text/x-component")
            .header(CONTENT_TYPE, "application/json")
            .header("Next-Action", action_id)
            .header(ORIGIN, ORIGIN_VALUE)
            // muse.ai answers 403 to server actions that do not look like
            // same-origin browser fetches.
            .header("Sec-Fetch-Site", "same-origin")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Dest", "empty")
            .header(USER_AGENT, BROWSER_USER_AGENT)
            .body(ACTION_BODY);
        self.send(request, MAX_ACTION_BYTES)
            .await?
            .ok_or_else(|| ProviderError::Parse("Unexpected muse.ai response.".into()))
    }

    async fn page(&self, cookie: &str) -> Result<Reply, ProviderError> {
        let request = self
            .client
            .get(format!("{}/", self.base_url))
            .header(COOKIE, cookie)
            .header(ACCEPT, "text/html")
            .header(USER_AGENT, BROWSER_USER_AGENT);
        self.send(request, MAX_DISCOVERY_TEXT)
            .await?
            .ok_or_else(model::discovery_failure)
    }

    /// Fetches one batch of static chunks without cookies. A failed or
    /// oversized chunk reads as empty, as upstream treats failures.
    async fn chunks(&self, paths: &[String]) -> Vec<String> {
        let requests = paths.iter().map(|path| async move {
            let request = self
                .client
                .get(format!("{}{path}", self.base_url))
                .header(ACCEPT, "*/*")
                .header(USER_AGENT, BROWSER_USER_AGENT);
            match self.send(request, MAX_DISCOVERY_TEXT).await {
                Ok(Some(reply)) if reply.status == 200 => reply.body,
                _ => String::new(),
            }
        });
        futures::future::join_all(requests).await
    }

    /// `Ok(None)` when the body exceeds `max_bytes`.
    async fn send(
        &self,
        request: RequestBuilder,
        max_bytes: usize,
    ) -> Result<Option<Reply>, ProviderError> {
        let response = request.timeout(self.timeout).send().await?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        match read_bounded_response(response, max_bytes).await {
            Ok(body) => Ok(Some(Reply {
                status,
                location,
                body: String::from_utf8_lossy(&body).into_owned(),
            })),
            Err(BoundedBodyError::TooLarge) => Ok(None),
            Err(BoundedBodyError::Read(error)) => Err(ProviderError::Network(error)),
        }
    }
}
