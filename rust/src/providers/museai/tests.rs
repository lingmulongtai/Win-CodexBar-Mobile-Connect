use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use reqwest::redirect::Policy;

use super::model::{chunk_paths, result, settings_chunk_paths, tokens_left};
use super::*;

/// Upstream `MuseAIPluginTests.actionID`.
const ACTION_ID: &str = "40447f7d32b58c622904d92f8ac01a13f4798568bd";
const STALE_ID: &str = "40aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COOKIE: &str = "hatch_sess=fixture";

/// Upstream `MuseAIPluginTests.maximum`.
const MAXIMUM: &str = concat!(
    r#"0:{"a":"$@1","f":"","q":"","i":false}"#,
    "\n",
    r#"1:{"success":true,"subscription":{"tier":{"name":"Maximum"},"usage":{"percentUsed":3,"resetsAt":1790634838},"usageRowValueLabel":"3% used (2.8B tokens left)","topupBalance":60000000,"topupTotal":80000000,"topupRowLabel":"Additional tokens","topupRowValueLabel":"25% used (1.5B tokens left)","agreement":{"currentPeriodEndTime":1790721238}}}"#,
);

/// Upstream `MuseAIPluginTests.free`.
const FREE: &str = r#"1:{"success":true,"subscription":{"tier":{"name":"Muse Free"},"usage":{"percentUsed":28,"resetsAt":1790634838},"usageRowValueLabel":"28% used","agreement":null}}"#;

/// Upstream `MuseAIPluginTests.chunks`.
fn upstream_chunks(path: &str) -> Option<String> {
    let body = match path {
        "/" => r#"<script src="/_next/static/chunks/app.js"></script><script src="/_next/static/chunks/loader.js">"#.to_owned(),
        "/_next/static/chunks/app.js" => "e.A(434304).then(({preloadHatchSettingsData:e})=>e({}))".to_owned(),
        "/_next/static/chunks/loader.js" => concat!(
            r#"},56115,s=>{s.v(t=>Promise.all(["static/chunks/other.js"]))},"#,
            r#"434304,s=>{s.v(t=>Promise.all(["static/chunks/settings.js"].map(t=>s.l(t))))}"#
        )
        .to_owned(),
        "/_next/static/chunks/settings.js" => format!(
            r#"(0,d.createServerReference)("{ACTION_ID}",d.callServer,void 0,d.findSourceMapURL,"fetchSubscriptionAction")"#
        ),
        _ => return None,
    };
    Some(body)
}

fn at(seconds: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(seconds, 0)
}

// ---------------------------------------------------------------- mapping

#[test]
fn the_maximum_plan_maps_weekly_usage_tokens_renewal_and_top_up() {
    let result = result(MAXIMUM).expect("maximum plan");
    let usage = &result.usage;
    assert_eq!(usage.primary.used_percent, 3.0);
    assert_eq!(usage.primary.window_minutes, Some(10_080));
    assert_eq!(usage.primary.resets_at, at(1_790_634_838));
    assert_eq!(
        usage.primary.reset_description.as_deref(),
        Some("2.8B tokens left")
    );
    assert!(usage.primary.description_is_detail);
    assert_eq!(
        usage.subscription.as_ref().and_then(|s| s.renews_at),
        at(1_790_721_238)
    );
    assert_eq!(usage.login_method.as_deref(), Some("Maximum"));
    assert!(usage.secondary.is_none());
    // Upstream returns the top-up as a detail row, never as a quota bar.
    assert!(usage.extra_rate_windows.is_empty());
    assert_eq!(result.display_details().len(), 1);
    let top_up = &result.display_details()[0];
    assert_eq!(top_up.id(), "topup");
    assert_eq!(top_up.title(), "Additional tokens");
    assert_eq!(top_up.value(), "1.5B tokens left");
    assert!(top_up.section_title().is_none());
    let progress = top_up.progress().expect("top-up progress");
    assert_eq!(progress.used() / progress.total(), 0.25);
    assert_eq!(result.source_label, "web");
}

#[test]
fn an_unlabeled_top_up_shows_its_micro_dollar_balance_in_usd() {
    let body = MAXIMUM
        .replace(
            r#""topupRowValueLabel":"25% used (1.5B tokens left)""#,
            r#""topupRowValueLabel":null"#,
        )
        .replace(
            r#""topupBalance":60000000,"topupTotal":80000000"#,
            r#""topupBalance":80000000,"topupTotal":80000000"#,
        );
    let result = result(&body).expect("unlabeled top-up");
    assert!(result.usage.extra_rate_windows.is_empty());
    let top_up = &result.display_details()[0];
    assert_eq!(top_up.value(), "$80.00 left");
    let progress = top_up.progress().expect("top-up progress");
    assert_eq!(progress.used() / progress.total(), 0.0);
}

#[test]
fn a_top_up_label_without_a_token_count_is_kept_verbatim() {
    let body = MAXIMUM.replace("25% used (1.5B tokens left)", "25% used");
    let result = result(&body).expect("plain top-up label");
    assert_eq!(result.display_details()[0].value(), "25% used");
}

#[test]
fn the_power_plan_reports_its_own_tokens_left() {
    let body = MAXIMUM
        .replace("Maximum", "Power")
        .replace("2.8B tokens left", "473M tokens left")
        .replace(r#""percentUsed":3"#, r#""percentUsed":5"#);
    let result = result(&body).expect("power plan");
    assert_eq!(result.usage.login_method.as_deref(), Some("Power"));
    assert_eq!(result.usage.primary.used_percent, 5.0);
    assert_eq!(
        result.usage.primary.reset_description.as_deref(),
        Some("473M tokens left")
    );
}

#[test]
fn the_free_plan_has_no_tokens_renewal_or_top_up() {
    let result = result(FREE).expect("free plan");
    let usage = &result.usage;
    assert_eq!(usage.primary.used_percent, 28.0);
    assert_eq!(usage.primary.resets_at, at(1_790_634_838));
    assert!(usage.primary.reset_description.is_none());
    assert!(!usage.primary.description_is_detail);
    assert!(usage.subscription.is_none());
    assert_eq!(usage.login_method.as_deref(), Some("Muse Free"));
    assert!(usage.extra_rate_windows.is_empty());
    assert!(result.display_details.is_empty());
}

#[test]
fn the_parity_pack_payload_maps_to_power_at_34_percent() {
    let body = concat!(
        r#"0:{"a":"$@1","f":"","q":"","i":false}"#,
        "\n",
        r#"1:{"success":true,"subscription":{"tier":{"name":"Power"},"usageRowLabel":"Weekly usage","usage":{"percentUsed":34,"resetsAt":1800000000},"usageRowValueLabel":"34% used (1.9B tokens left)","topupBalance":60000000,"topupTotal":80000000,"topupRowLabel":"Additional tokens","topupRowValueLabel":"25% used (1.5B tokens left)","agreement":{"currentPeriodEndTime":1801000000}}}"#,
        "\n"
    );
    let result = result(body).expect("pack payload");
    assert_eq!(result.usage.primary.used_percent, 34.0);
    assert_eq!(
        result.usage.primary.reset_description.as_deref(),
        Some("1.9B tokens left")
    );
    assert_eq!(result.usage.login_method.as_deref(), Some("Power"));
    assert!(result.usage.extra_rate_windows.is_empty());
    let progress = result.display_details()[0]
        .progress()
        .expect("top-up progress");
    assert_eq!(progress.used() / progress.total(), 0.25);
}

#[test]
fn usage_is_clamped_and_the_plan_falls_back_to_the_usage_row_label() {
    let over = result(
        r#"1:{"success":true,"subscription":{"usageRowLabel":"Weekly usage","usage":{"percentUsed":140}}}"#,
    )
    .expect("overage");
    assert_eq!(over.usage.primary.used_percent, 100.0);
    assert!(over.usage.primary.resets_at.is_none());
    assert_eq!(over.usage.login_method.as_deref(), Some("Weekly usage"));

    let under = result(r#"1:{"success":true,"subscription":{"usage":{"percentUsed":-5}}}"#)
        .expect("negative usage");
    assert_eq!(under.usage.primary.used_percent, 0.0);
    assert!(under.usage.login_method.is_none());
}

#[test]
fn a_zero_top_up_total_hides_the_top_up() {
    let body = MAXIMUM.replace(r#""topupTotal":80000000"#, r#""topupTotal":0"#);
    let result = result(&body).expect("zero total");
    assert!(result.usage.extra_rate_windows.is_empty());
    assert!(result.display_details().is_empty());
}

#[test]
fn the_last_successful_flight_row_wins_and_crlf_lines_parse() {
    let body = concat!(
        r#"1:{"success":true,"subscription":{"usage":{"percentUsed":10}}}"#,
        "\r\n",
        r#"2:{"success":false,"subscription":{"usage":{"percentUsed":90}}}"#,
        "\r\n",
        r#"a:{"success":true,"subscription":{"usage":{"percentUsed":20}}}"#,
        "\r\n",
        r#"Z:{"success":true,"subscription":{"usage":{"percentUsed":70}}}"#,
        "\r\n"
    );
    assert_eq!(
        result(body)
            .expect("flight rows")
            .usage
            .primary
            .used_percent,
        20.0
    );
}

#[test]
fn invalid_subscription_responses_are_parse_errors() {
    for body in [
        r#"1:{"success":false}"#,
        r#"1:{"success":true,"subscription":{"usage":{"percentUsed":"3"}}}"#,
        r#"1:{"success":true}"#,
        "Server action not found.",
        "",
    ] {
        let error = result(body).expect_err(body);
        assert_eq!(
            error.to_string(),
            "Parse error: Unexpected muse.ai response."
        );
    }
}

#[test]
fn tokens_left_reads_the_parenthesized_amount() {
    assert_eq!(
        tokens_left(Some("3% used (2.8B tokens left)")).as_deref(),
        Some("2.8B tokens left")
    );
    assert_eq!(tokens_left(Some("28% used")), None);
    assert_eq!(tokens_left(None), None);
}

// ---------------------------------------------------------------- discovery

#[test]
fn chunk_paths_are_prefixed_and_deduplicated_in_page_order() {
    let page = concat!(
        r#"<script src="/_next/static/chunks/b~1.js"></script>"#,
        r#"<script src="/_next/static/chunks/a.js"></script>"#,
        r#"<script src="/_next/static/chunks/b~1.js"></script>"#
    );
    assert_eq!(
        chunk_paths(page),
        vec![
            "/_next/static/chunks/b~1.js".to_owned(),
            "/_next/static/chunks/a.js".to_owned()
        ]
    );
}

#[test]
fn the_settings_loader_lists_only_the_preloaded_module_files() {
    let text = format!(
        "\n{}\n{}",
        upstream_chunks("/_next/static/chunks/app.js").expect("app chunk"),
        upstream_chunks("/_next/static/chunks/loader.js").expect("loader chunk")
    );
    assert_eq!(
        settings_chunk_paths(&text),
        Some(vec!["/_next/static/chunks/settings.js".to_owned()])
    );
    assert_eq!(
        settings_chunk_paths("e.A(1).then(({preloadHatchSettingsData:e})=>e({}))"),
        None
    );
}

#[test]
fn the_action_id_is_read_from_the_server_reference() {
    let chunk = upstream_chunks("/_next/static/chunks/settings.js").expect("settings chunk");
    assert_eq!(model::action_id(&chunk).as_deref(), Some(ACTION_ID));
    assert_eq!(
        model::action_id(
            r#"createServerReference("40447f",d.callServer,"fetchSubscriptionAction")"#
        ),
        None
    );
}

// ---------------------------------------------------------------- local server

/// One recorded request: method, path and lower-cased header lines.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Upstream's log line: `POST / <action>` or `GET <path>`.
    fn line(&self) -> String {
        match self.header("next-action") {
            Some(action) => format!("{} {} {action}", self.method, self.path),
            None => format!("{} {}", self.method, self.path),
        }
    }
}

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

fn reply(status: u16, body: impl Into<String>) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: body.into(),
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut data = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        data.extend_from_slice(&buffer[..read]);
        if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&data[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(key, _)| key == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while data.len() < header_end + length {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..read]);
    }
    let body = String::from_utf8_lossy(&data[header_end..]).into_owned();
    Some(Seen {
        method,
        path,
        headers,
        body,
    })
}

/// A local muse.ai stand-in. Each connection gets its own thread so batched
/// chunk requests can overlap; `route` decides every response.
fn serve<F>(route: F) -> (String, Arc<Mutex<Vec<Seen>>>)
where
    F: Fn(&Seen) -> Reply + Send + Sync + 'static,
{
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind local test server");
    let address = listener.local_addr().expect("local server address");
    let log = Arc::new(Mutex::new(Vec::new()));
    let route = Arc::new(route);
    let server_log = Arc::clone(&log);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let log = Arc::clone(&server_log);
            let route = Arc::clone(&route);
            std::thread::spawn(move || {
                let Some(seen) = read_request(&mut stream) else {
                    return;
                };
                let response = route(&seen);
                log.lock().expect("request log").push(seen);
                let mut head = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n",
                    response.status,
                    response.body.len()
                );
                for (name, value) in &response.headers {
                    head.push_str(&format!("{name}: {value}\r\n"));
                }
                head.push_str("\r\n");
                if stream.write_all(head.as_bytes()).is_ok() {
                    stream
                        .write_all(response.body.as_bytes())
                        .unwrap_or_default();
                }
            });
        }
    });
    (format!("http://{address}/"), log)
}

struct Options {
    stored: Option<&'static str>,
    action: String,
    page_status: u16,
    action_status: u16,
    chunks: fn(&str) -> Option<String>,
}

impl Options {
    fn new(stored: Option<&'static str>, action: &str) -> Self {
        Self {
            stored,
            action: action.to_owned(),
            page_status: 200,
            action_status: 200,
            chunks: upstream_chunks,
        }
    }
}

struct Run {
    result: Result<ProviderFetchResult, ProviderError>,
    requests: Vec<Seen>,
    stored: Option<String>,
}

impl Run {
    fn lines(&self) -> Vec<String> {
        self.requests.iter().map(Seen::line).collect()
    }
}

/// Upstream `MuseAIPluginTests.fetch`, against the local server.
async fn fetch(options: Options) -> Run {
    let Options {
        stored,
        action,
        page_status,
        action_status,
        chunks,
    } = options;
    let (base_url, log) = serve(move |seen| {
        if seen.method == "POST" {
            assert_eq!(seen.header("sec-fetch-site"), Some("same-origin"));
            return if seen.header("next-action") == Some(ACTION_ID) {
                reply(action_status, action.clone())
            } else {
                reply(404, "Server action not found.")
            };
        }
        if seen.path == "/" {
            let mut response = reply(page_status, chunks("/").unwrap_or_default());
            if page_status == 307 {
                response.headers.push((
                    "Location",
                    "https://auth.muse.ai/aymh/?origin=https%3A%2F%2Fmuse.ai".to_owned(),
                ));
            }
            return response;
        }
        match chunks(&seen.path) {
            Some(body) => reply(200, body),
            None => reply(404, ""),
        }
    });
    let client = Client::builder()
        .redirect(Policy::none())
        .build()
        .expect("test HTTP client");
    let provider = MuseAIProvider::with_client(&base_url, client, stored);
    let ctx = FetchContext {
        source_mode: SourceMode::Web,
        manual_cookie_header: Some(COOKIE.into()),
        ..FetchContext::default()
    };
    let result = provider.fetch_web(&ctx).await;
    let requests = log.lock().expect("request log").clone();
    Run {
        result,
        requests,
        stored: provider.stored_action_id(),
    }
}

#[tokio::test]
async fn a_stored_action_posts_once_with_browser_fetch_headers() {
    let run = fetch(Options::new(Some(ACTION_ID), MAXIMUM)).await;
    let result = run.result.as_ref().expect("maximum plan");
    assert_eq!(result.usage.primary.used_percent, 3.0);
    assert_eq!(run.lines(), vec![format!("POST / {ACTION_ID}")]);
    let post = &run.requests[0];
    assert_eq!(post.header("cookie"), Some(COOKIE));
    assert_eq!(post.header("accept"), Some("text/x-component"));
    assert_eq!(post.header("content-type"), Some("application/json"));
    assert_eq!(post.header("origin"), Some("https://muse.ai"));
    assert_eq!(post.header("sec-fetch-mode"), Some("cors"));
    assert_eq!(post.header("sec-fetch-dest"), Some("empty"));
    assert_eq!(post.header("user-agent"), Some(BROWSER_USER_AGENT));
    assert_eq!(post.body, r#"[{"includeAgreement":true}]"#);
    assert_eq!(run.stored.as_deref(), Some(ACTION_ID));
}

#[tokio::test]
async fn missing_or_stale_actions_are_rediscovered_and_saved() {
    for stored in [None, Some(STALE_ID)] {
        let run = fetch(Options::new(stored, FREE)).await;
        let result = run.result.as_ref().expect("free plan");
        assert_eq!(result.usage.primary.used_percent, 28.0);
        let lines = run.lines();
        assert!(lines.contains(&"GET /_next/static/chunks/settings.js".to_owned()));
        assert!(!lines.contains(&"GET /_next/static/chunks/other.js".to_owned()));
        assert_eq!(lines.last(), Some(&format!("POST / {ACTION_ID}")));
        assert_eq!(run.stored.as_deref(), Some(ACTION_ID));
        let page = run
            .requests
            .iter()
            .find(|seen| seen.method == "GET" && seen.path == "/")
            .expect("page request");
        assert_eq!(page.header("cookie"), Some(COOKIE));
        assert_eq!(page.header("accept"), Some("text/html"));
        // Static chunks never receive the session cookies.
        for chunk in run
            .requests
            .iter()
            .filter(|seen| seen.path.starts_with("/_next/"))
        {
            assert_eq!(chunk.header("cookie"), None, "{}", chunk.path);
            assert_eq!(chunk.header("accept"), Some("*/*"));
        }
        let posts = lines.iter().filter(|line| line.starts_with("POST")).count();
        assert_eq!(posts, if stored.is_some() { 2 } else { 1 });
    }
}

#[tokio::test]
async fn sign_in_redirects_and_forbidden_actions_report_an_expired_session() {
    let provider = MuseAIProvider::new();
    for (page_status, action_status) in [(307, 200), (200, 403)] {
        let run = fetch(Options {
            page_status,
            action_status,
            ..Options::new(None, "")
        })
        .await;
        let error = run.result.expect_err("expired session");
        assert_eq!(error.to_string(), SESSION_EXPIRED);
        assert_eq!(
            provider.error_state_kind(&error),
            ProviderStateKind::ExpiredSession
        );
        assert_eq!(run.stored, None);
    }
}

#[tokio::test]
async fn automatic_browser_retries_a_signed_out_muse_ai_session() {
    use crate::browser::detection::BrowserType;
    use crate::browser::session_scan::test_support::{FakeCookieStore, FakeRead};
    use crate::providers::browser_cookie_sessions_from;

    let (base_url, log) = serve(|seen| {
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.header("next-action"), Some(ACTION_ID));
        match seen.header("cookie") {
            Some("hatch_sess=stale") => reply(401, ""),
            Some(COOKIE) => reply(200, FREE),
            _ => panic!("unexpected fixture session"),
        }
    });
    let client = Client::builder().redirect(Policy::none()).build().unwrap();
    let provider = MuseAIProvider::with_client(&base_url, client, Some(ACTION_ID));
    let site = Site {
        client: provider.client.as_ref().unwrap(),
        base_url: &provider.base_url,
        timeout: Duration::from_secs(2),
    };
    let ctx = FetchContext::default();
    let store = FakeCookieStore::default()
        .cookies(
            BrowserType::Chrome,
            COOKIE_DOMAIN,
            &[("hatch_sess", "stale")],
        )
        .cookies(
            BrowserType::Vivaldi,
            COOKIE_DOMAIN,
            &[("hatch_sess", "fixture")],
        )
        .browser(BrowserType::Firefox);
    let sessions = browser_cookie_sessions_from(&store, &ctx, &[COOKIE_DOMAIN]);
    let result = provider
        .fetch_browser_sessions(&site, sessions)
        .await
        .unwrap();
    assert_eq!(result.usage.primary.used_percent, 28.0);
    assert_eq!(store.reads().len(), 2);

    let store = FakeCookieStore::default()
        .cookies(
            BrowserType::Chrome,
            COOKIE_DOMAIN,
            &[("hatch_sess", "stale")],
        )
        .with(BrowserType::Vivaldi, COOKIE_DOMAIN, FakeRead::Locked);
    let sessions = browser_cookie_sessions_from(&store, &ctx, &[COOKIE_DOMAIN]);
    let error = provider
        .fetch_browser_sessions(&site, sessions)
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Other(message) if message.contains("Vivaldi")));
    assert_eq!(log.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn a_signed_out_stored_action_keeps_the_id_and_expires() {
    let run = fetch(Options {
        action_status: 401,
        ..Options::new(Some(ACTION_ID), FREE)
    })
    .await;
    assert_eq!(
        run.result.as_ref().expect_err("expired").to_string(),
        SESSION_EXPIRED
    );
    assert_eq!(run.lines(), vec![format!("POST / {ACTION_ID}")]);
    assert_eq!(run.stored.as_deref(), Some(ACTION_ID));
}

fn many_page_chunks(path: &str) -> Option<String> {
    (path == "/").then(|| {
        (0..120)
            .map(|index| format!(r#"<script src="/_next/static/chunks/chunk{index}.js"></script>"#))
            .collect()
    })
}

#[tokio::test]
async fn discovery_caps_page_chunk_requests() {
    let run = fetch(Options {
        chunks: many_page_chunks,
        ..Options::new(None, FREE)
    })
    .await;
    let error = run.result.expect_err("discovery failure");
    assert_eq!(
        error.to_string(),
        format!("Parse error: {DISCOVERY_FAILURE_TEXT}")
    );
    let chunk_requests = run
        .requests
        .iter()
        .filter(|seen| seen.path.starts_with("/_next/"))
        .count();
    assert_eq!(chunk_requests, 96);
}

const DISCOVERY_FAILURE_TEXT: &str = "Could not find a working muse.ai subscription action. muse.ai may have changed. Refresh to retry.";

fn many_settings_chunks(path: &str) -> Option<String> {
    if path == "/_next/static/chunks/loader.js" {
        let paths: Vec<String> = (0..60)
            .map(|index| format!(r#""static/chunks/settings{index}.js""#))
            .collect();
        return Some(format!(
            "}},434304,s=>{{s.v(t=>Promise.all([{}].map(t=>s.l(t))))}}",
            paths.join(",")
        ));
    }
    upstream_chunks(path)
}

#[tokio::test]
async fn discovery_caps_lazy_settings_chunks() {
    let run = fetch(Options {
        chunks: many_settings_chunks,
        ..Options::new(None, FREE)
    })
    .await;
    let error = run.result.expect_err("discovery failure");
    assert_eq!(
        error.to_string(),
        format!("Parse error: {DISCOVERY_FAILURE_TEXT}")
    );
    let settings_requests = run
        .requests
        .iter()
        .filter(|seen| seen.path.starts_with("/_next/static/chunks/settings"))
        .count();
    assert_eq!(settings_requests, 32);
    assert!(!run.requests.iter().any(|seen| seen.method == "POST"));
}

#[tokio::test]
async fn failed_rediscovery_is_classified_and_never_caches_the_unusable_action() {
    let run = fetch(Options {
        action_status: 404,
        ..Options::new(Some(STALE_ID), "Server action not found.")
    })
    .await;
    let error = run.result.expect_err("rediscovery failure");
    assert!(matches!(error, ProviderError::Parse(_)));
    assert!(error.to_string().contains("subscription action"));
    let posts = run
        .requests
        .iter()
        .filter(|seen| seen.method == "POST")
        .count();
    assert_eq!(posts, 2);
    assert_eq!(run.stored, None);
}

#[tokio::test]
async fn invalid_subscription_responses_never_save_an_action() {
    let run = fetch(Options::new(None, r#"1:{"success":false}"#)).await;
    assert_eq!(
        run.result.expect_err("invalid").to_string(),
        "Parse error: Unexpected muse.ai response."
    );
    assert_eq!(run.stored, None);
}

#[tokio::test]
async fn non_success_statuses_name_the_http_code_without_the_body() {
    let page = fetch(Options {
        page_status: 500,
        ..Options::new(None, FREE)
    })
    .await;
    assert_eq!(
        page.result.expect_err("page failure").to_string(),
        "muse.ai returned HTTP 500."
    );

    let action = fetch(Options {
        action_status: 502,
        ..Options::new(Some(ACTION_ID), "secret-body hatch_sess=fixture")
    })
    .await;
    let message = action.result.expect_err("action failure").to_string();
    assert_eq!(message, "muse.ai returned HTTP 502.");
    assert!(!message.contains("secret-body"));
    assert!(!message.contains(COOKIE));
    // Upstream removes the stored ID only when it is stale.
    assert_eq!(action.stored.as_deref(), Some(ACTION_ID));
}

#[tokio::test]
async fn off_and_oauth_sources_make_no_requests() {
    let provider = MuseAIProvider::new();
    let off = FetchContext {
        source_mode: SourceMode::Cli,
        manual_cookie_header: Some(COOKIE.into()),
        ..FetchContext::default()
    };
    assert_eq!(
        provider
            .fetch_usage(&off)
            .await
            .expect_err("cookies off")
            .to_string(),
        COOKIES_OFF
    );
    let oauth = FetchContext {
        source_mode: SourceMode::OAuth,
        ..FetchContext::default()
    };
    assert!(matches!(
        provider.fetch_usage(&oauth).await,
        Err(ProviderError::UnsupportedSource(SourceMode::OAuth))
    ));
}

#[tokio::test]
async fn a_missing_manual_cookie_asks_for_a_sign_in() {
    let provider = MuseAIProvider::new();
    for ctx in [
        FetchContext {
            source_mode: SourceMode::Web,
            manual_cookie_missing: true,
            ..FetchContext::default()
        },
        FetchContext {
            source_mode: SourceMode::Web,
            manual_cookie_header: Some("   ".into()),
            ..FetchContext::default()
        },
    ] {
        match provider.fetch_web(&ctx).await {
            Err(ProviderError::BrowserSignInRequired {
                message,
                sign_in_url,
            }) => {
                assert_eq!(message, MISSING_SESSION);
                assert_eq!(sign_in_url, "https://muse.ai");
            }
            other => panic!("expected a sign-in prompt, got {other:?}"),
        }
    }
}

#[test]
fn metadata_is_distinct_from_muse_code() {
    let provider = MuseAIProvider::new();
    let metadata = provider.metadata();
    assert_eq!(provider.id(), ProviderId::MuseAI);
    assert_eq!(metadata.display_name, "Muse (muse.ai)");
    assert_eq!(metadata.session_label, "Weekly");
    assert_eq!(metadata.weekly_label, "Plan");
    assert_eq!(
        metadata.dashboard_url,
        Some("https://muse.ai/?settings_tab=general")
    );
    assert!(!metadata.default_enabled);
    assert_eq!(
        provider.available_sources(),
        vec![SourceMode::Auto, SourceMode::Web]
    );
    assert!(provider.supports_web());
    assert_eq!(
        provider.manual_empty_cookie_policy(),
        ManualEmptyCookiePolicy::FailClosedWeb
    );
    assert_eq!(ProviderId::MuseAI.cli_name(), "museai");
    assert_eq!(ProviderId::MuseAI.cookie_domain(), Some("muse.ai"));
    assert_eq!(
        ProviderId::from_cli_name("museai"),
        Some(ProviderId::MuseAI)
    );
    assert_eq!(
        ProviderId::from_cli_name("Muse (muse.ai)"),
        Some(ProviderId::MuseAI)
    );
    assert_eq!(ProviderId::from_cli_name("muse"), Some(ProviderId::Muse));
    assert_eq!(ProviderId::Muse.display_name(), "Muse Code");
}
