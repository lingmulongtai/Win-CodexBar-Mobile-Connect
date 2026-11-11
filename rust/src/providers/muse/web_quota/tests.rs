//! Fixtures follow the upstream Muse plugin test matrix (CodexBar v0.68.0,
//! `MusePluginTests.swift`): same paths, field names, and team/plan values.

use super::*;

#[test]
fn browser_quota_keeps_the_lock_reason_and_only_uses_session_cookies() {
    use crate::browser::session_scan::test_support::{FakeCookieStore, FakeRead};

    let store = FakeCookieStore::default()
        .cookies(
            BrowserType::Chrome,
            DEV_META_DOMAIN,
            &[("consent", "fixture")],
        )
        .with(BrowserType::Vivaldi, DEV_META_DOMAIN, FakeRead::Locked)
        .cookies(
            BrowserType::Firefox,
            DEV_META_DOMAIN,
            &[(SESSION_COOKIE, "fixture")],
        );
    let sessions =
        resolve_browser_sessions(BrowserCookieScan::with_store(&store, &[DEV_META_DOMAIN]));

    assert_eq!(sessions.cookies, ["llama_dev_sess=fixture"]);
    assert_eq!(
        sessions.blocking_note,
        Some(TeamNote::CookiesLocked(BrowserType::Vivaldi))
    );
}
use std::sync::Mutex;

const TEAM_ID: &str = "424242424242";
const NOW: i64 = 1_790_341_873;
const FIXTURE_COOKIE: &str = "llama_dev_sess=fixture";
const TEAMS: &str = r#"{"teams":[{"team_id":424242424242,"team_name":"My Team"}]}"#;
const ME: &str = r#"{"userId":"1","email":"Ada@Example.com","accountType":"META_ACCOUNT"}"#;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(NOW, 0).unwrap()
}

fn identity() -> LoginIdentity {
    LoginIdentity {
        email: Some("ada@example.com".to_string()),
        plan: Some("Muse Code Power Usage".to_string()),
    }
}

/// A dev.meta.ai team quota; the login fixture reports "Muse Code Power Usage".
fn quota(
    tier: &str,
    window_used: &str,
    window_resets_at: Option<i64>,
    weekly_used: &str,
) -> String {
    quota_with_weekly_reset(
        tier,
        window_used,
        window_resets_at,
        weekly_used,
        1_790_553_600,
    )
}

fn quota_with_weekly_reset(
    tier: &str,
    window_used: &str,
    window_resets_at: Option<i64>,
    weekly_used: &str,
    weekly_resets_at: i64,
) -> String {
    let window_reset = window_resets_at
        .map(|value| format!(r#","window_resets_at":{value}"#))
        .unwrap_or_default();
    format!(
        r#"{{"subscription_quota":{{"tier_id":"1","tier":"{tier}","as_of":{NOW},"window_weighted_limit":"20000000000","window_duration_secs":18000,"weekly_weighted_limit":"60000000000","weekly_resets_at":{weekly_resets_at},"window_weighted_used":"{window_used}","weekly_weighted_used":"{weekly_used}"{window_reset}}}}}"#
    )
}

fn idle_quota() -> String {
    quota("Muse Code Power Usage", "0", None, "9043782620")
}

type Handler = Box<dyn Fn(&str, &str) -> (u16, String) + Send + Sync>;

/// Serves canned dev.meta.ai responses and records `(cookie, path)` per request.
struct FakeApi {
    handler: Handler,
    log: Mutex<Vec<(String, String)>>,
}

impl FakeApi {
    fn new(handler: impl Fn(&str, &str) -> (u16, String) + Send + Sync + 'static) -> Self {
        Self {
            handler: Box::new(handler),
            log: Mutex::new(Vec::new()),
        }
    }

    /// The upstream `web(quota:)` fixture: me, teams, and the team quota.
    fn with_quota(quota: String) -> Self {
        Self::new(move |_, path| match path {
            "/api/auth/me" => (200, ME.to_string()),
            "/api/portal/teams" => (200, TEAMS.to_string()),
            path if path == format!("/api/portal/teams/{TEAM_ID}/subscription-quota") => {
                (200, quota.clone())
            }
            _ => (404, "{}".to_string()),
        })
    }

    fn paths(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(_, path)| path.clone())
            .collect()
    }

    fn cookies(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(cookie, _)| cookie.clone())
            .collect()
    }

    fn quota_requested(&self) -> bool {
        self.paths()
            .iter()
            .any(|path| path.ends_with("/subscription-quota"))
    }
}

#[async_trait]
impl DevMetaApi for FakeApi {
    async fn get(&self, path: &str, session_cookie: &str) -> Result<DevMetaResponse, Abort> {
        self.log
            .lock()
            .unwrap()
            .push((session_cookie.to_string(), path.to_string()));
        let (status, body) = (self.handler)(session_cookie, path);
        Ok(DevMetaResponse {
            status,
            body: body.into_bytes(),
        })
    }
}

async fn read(api: &FakeApi, team_id: &str) -> Option<WebReading> {
    read_team_quota(
        api,
        &[FIXTURE_COOKIE.to_string()],
        &identity(),
        team_id,
        now(),
    )
    .await
}

fn detail_values(result: &ProviderFetchResult) -> Vec<(String, String, String)> {
    result
        .display_details()
        .iter()
        .map(|row| {
            (
                row.id().to_string(),
                row.title().to_string(),
                row.value().to_string(),
            )
        })
        .collect()
}

fn has_detail(result: &ProviderFetchResult, id: &str) -> bool {
    result.display_details().iter().any(|row| row.id() == id)
}

fn detail<'a>(result: &'a ProviderFetchResult, id: &str) -> &'a ProviderDisplayDetail {
    result
        .display_details()
        .iter()
        .find(|row| row.id() == id)
        .unwrap_or_else(|| panic!("missing detail {id}: {:?}", detail_values(result)))
}

fn fallback_result(reading: Option<WebReading>) -> ProviderFetchResult {
    windowless_result(&identity(), reading)
}

#[tokio::test]
async fn selected_team_quota_fills_omitted_login_quotas() {
    let api = FakeApi::with_quota(quota(
        "Muse Code Power Usage",
        "4000000000",
        Some(NOW + 3600),
        "9043782620",
    ));
    let reading = read(&api, TEAM_ID).await;
    let result = fallback_result(reading);

    assert_eq!(result.source_label, "oauth+web");
    assert!(!result.pace_authoritative);
    let primary = &result.usage.primary;
    assert_eq!(primary.used_percent, 20.0);
    assert_eq!(primary.window_minutes, Some(300));
    assert_eq!(primary.resets_at, DateTime::from_timestamp(NOW + 3600, 0));
    let weekly = result.usage.secondary.as_ref().unwrap();
    assert!((weekly.used_percent - 15.07297103).abs() < 0.0001);
    assert_eq!(weekly.window_minutes, Some(10080));
    assert_eq!(weekly.resets_at, DateTime::from_timestamp(1_790_553_600, 0));
    assert_eq!(result.usage.login_method.as_deref(), Some("Muse login"));
    assert_eq!(
        result.usage.account_email.as_deref(),
        Some("ada@example.com")
    );

    assert_eq!(detail(&result, "browser-team").value(), "My Team");
    assert_eq!(
        detail(&result, "browser-team").title(),
        "Browser team quota (dev.meta.ai)"
    );
    assert_eq!(detail(&result, "five-hour").value(), "20%");
    assert_eq!(detail(&result, "weekly").value(), "15%");
    assert_eq!(detail(&result, "plan").value(), "Muse Code Power Usage");
    assert!(!has_detail(&result, "quota"));
    assert_eq!(
        detail(&result, &format!("browser-team-{TEAM_ID}")).value(),
        TEAM_ID
    );

    // Only the session cookie is sent, and only to the three portal paths.
    assert_eq!(
        api.paths(),
        vec![
            "/api/auth/me".to_string(),
            "/api/portal/teams".to_string(),
            format!("/api/portal/teams/{TEAM_ID}/subscription-quota"),
        ]
    );
    assert!(api.cookies().iter().all(|cookie| cookie == FIXTURE_COOKIE));
}

#[tokio::test]
async fn idle_and_expired_five_hour_windows_carry_no_usage_or_reset() {
    for quota in [
        idle_quota(),
        quota(
            "Muse Code Power Usage",
            "4000000000",
            Some(NOW - 60),
            "9043782620",
        ),
    ] {
        let result = fallback_result(read(&FakeApi::with_quota(quota), TEAM_ID).await);
        assert_eq!(result.source_label, "oauth+web");
        assert_eq!(result.usage.primary.used_percent, 0.0);
        assert!(result.usage.primary.resets_at.is_none());
        assert_eq!(result.usage.primary.window_minutes, Some(300));
        assert!(result.usage.secondary.is_some());
    }
}

#[tokio::test]
async fn weekly_quota_past_its_reset_withholds_the_whole_browser_reading() {
    let quota = quota_with_weekly_reset(
        "Muse Code Power Usage",
        "4000000000",
        Some(NOW + 3600),
        "9043782620",
        NOW - 60,
    );
    let result = fallback_result(read(&FakeApi::with_quota(quota), TEAM_ID).await);
    assert_eq!(result.source_label, "oauth");
    assert!(result.usage.primary.is_informational);
    assert!(result.usage.secondary.is_none());
    assert!(has_detail(&result, "quota"));
}

#[tokio::test]
async fn invalid_five_hour_resets_never_invent_an_idle_window() {
    let valid = format!(r#""window_resets_at":{}"#, NOW + 3600);
    for reset in ["\"invalid\"", "1e30", "-1", "true", "null", "0"] {
        let body = quota(
            "Muse Code Power Usage",
            "4000000000",
            Some(NOW + 3600),
            "9043782620",
        )
        .replace(&valid, &format!(r#""window_resets_at":{reset}"#));
        let result = fallback_result(read(&FakeApi::with_quota(body), TEAM_ID).await);
        assert!(result.usage.secondary.is_none(), "reset {reset}");
        assert_eq!(result.source_label, "oauth", "reset {reset}");
        assert!(has_detail(&result, "quota"), "reset {reset}");
    }
}

#[tokio::test]
async fn nonzero_idle_window_is_rejected_even_when_its_percent_underflows() {
    let body = idle_quota()
        .replace(
            "\"window_weighted_used\":\"0\"",
            "\"window_weighted_used\":1e-300",
        )
        .replace(
            "\"window_weighted_limit\":\"20000000000\"",
            "\"window_weighted_limit\":1e308",
        );
    let result = fallback_result(read(&FakeApi::with_quota(body), TEAM_ID).await);

    assert_eq!(result.source_label, "oauth");
    assert!(result.usage.primary.is_informational);
    assert!(result.usage.secondary.is_none());
    assert!(has_detail(&result, "quota"));
}

#[tokio::test]
async fn selected_quota_retains_the_team_list_for_settings() {
    let result = fallback_result(read(&FakeApi::with_quota(idle_quota()), TEAM_ID).await);
    let row = detail(&result, &format!("browser-team-{TEAM_ID}"));
    assert_eq!(row.title(), "My Team");
    assert_eq!(row.value(), TEAM_ID);
}

#[tokio::test]
async fn a_session_without_teams_reads_no_quota() {
    let api = FakeApi::new(|_, path| match path {
        "/api/auth/me" => (200, ME.to_string()),
        "/api/portal/teams" => (200, r#"{"teams":[]}"#.to_string()),
        _ => (200, idle_quota()),
    });
    let result = fallback_result(read(&api, TEAM_ID).await);
    assert!(result.usage.secondary.is_none());
    assert!(!api.quota_requested());
    assert_eq!(
        detail(&result, "browser-teams-status").value(),
        "The selected browser team is not visible to this session"
    );
}

#[tokio::test]
async fn without_a_selected_team_the_visible_teams_are_listed_and_no_quota_is_read() {
    for team_id in ["", "  "] {
        let api = FakeApi::with_quota(idle_quota());
        let result = fallback_result(
            read_team_quota(
                &api,
                &[FIXTURE_COOKIE.to_string()],
                &identity(),
                team_id.trim(),
                now(),
            )
            .await,
        );
        assert_eq!(result.source_label, "oauth");
        assert!(result.usage.secondary.is_none());
        assert!(!api.quota_requested());
        assert_eq!(
            detail(&result, "browser-team-424242424242").title(),
            "My Team"
        );
        assert_eq!(
            detail(&result, "browser-teams-status").value(),
            "Choose a browser team in Muse Code settings"
        );
    }
}

#[tokio::test]
async fn a_team_the_session_cannot_see_is_never_queried() {
    let api = FakeApi::new(|_, path| match path {
        "/api/auth/me" => (200, ME.to_string()),
        "/api/portal/teams" => (200, TEAMS.to_string()),
        _ => (200, idle_quota()),
    });
    let result = fallback_result(read(&api, "123").await);
    assert!(result.usage.secondary.is_none());
    assert!(!api.quota_requested());
    assert!(has_detail(&result, "browser-team-424242424242"));
}

#[tokio::test]
async fn the_selected_team_decides_the_quota_regardless_of_list_order() {
    for teams in [
        r#"{"teams":[{"team_id":"11","team_name":"Alpha"},{"team_id":"22","team_name":"Beta"}]}"#,
        r#"{"teams":[{"team_id":"22","team_name":"Beta"},{"team_id":"11","team_name":"Alpha"}]}"#,
    ] {
        let api = FakeApi::new(move |_, path| match path {
            "/api/auth/me" => (200, ME.to_string()),
            "/api/portal/teams" => (200, teams.to_string()),
            "/api/portal/teams/11/subscription-quota" => {
                (200, quota("Muse Code Power Usage", "0", None, "6000000000"))
            }
            "/api/portal/teams/22/subscription-quota" => (
                200,
                quota("Muse Code Power Usage", "0", None, "48000000000"),
            ),
            _ => (404, "{}".to_string()),
        });
        let result = fallback_result(read(&api, "22").await);
        assert_eq!(result.usage.secondary.as_ref().unwrap().used_percent, 80.0);
        assert_eq!(detail(&result, "browser-team").value(), "Beta");
    }
}

#[tokio::test]
async fn a_team_quota_for_a_different_plan_is_not_shown() {
    let api = FakeApi::with_quota(quota("Muse Code Everyday Usage", "0", None, "9043782620"));
    let result = fallback_result(read(&api, TEAM_ID).await);
    assert_eq!(result.source_label, "oauth");
    assert!(result.usage.secondary.is_none());
    assert_eq!(
        detail(&result, "browser-teams-status").value(),
        "The selected team's plan differs from the Muse login"
    );
}

#[tokio::test]
async fn login_without_a_plan_never_matches_a_team_quota() {
    let api = FakeApi::with_quota(idle_quota());
    let identity = LoginIdentity {
        plan: None,
        ..identity()
    };
    let reading = read_team_quota(
        &api,
        &[FIXTURE_COOKIE.to_string()],
        &identity,
        TEAM_ID,
        now(),
    )
    .await
    .unwrap();
    assert!(reading.quota.is_none());
    assert_eq!(reading.note, Some(TeamNote::PlanDiffers));
}

#[tokio::test]
async fn unusable_web_quotas_keep_the_login_response_result() {
    let zero_limit = idle_quota().replace(
        r#""window_weighted_limit":"20000000000""#,
        r#""window_weighted_limit":"0""#,
    );
    let huge_duration = idle_quota().replace("18000", "1e30");
    let short_duration = idle_quota().replace("18000", "59");
    let fractional_duration = idle_quota().replace("18000", "18000.5");
    let missing_weekly_limit = idle_quota().replace("weekly_weighted_limit", "weekly_other");
    for (status, body) in [
        (401_u16, r#"{"error":"Not authenticated"}"#.to_string()),
        (200, r#"{"subscription_quota":null}"#.to_string()),
        (200, zero_limit),
        (200, "<html>".to_string()),
        (200, huge_duration),
        (200, short_duration),
        (200, fractional_duration),
        (200, missing_weekly_limit),
        (500, "{}".to_string()),
    ] {
        let api = FakeApi::new(move |_, path| match path {
            "/api/auth/me" => (200, ME.to_string()),
            "/api/portal/teams" => (200, TEAMS.to_string()),
            _ => (status, body.clone()),
        });
        let result = fallback_result(read(&api, TEAM_ID).await);
        assert_eq!(result.source_label, "oauth", "status {status}");
        assert!(result.usage.primary.is_informational, "status {status}");
        assert!(result.usage.secondary.is_none(), "status {status}");
        assert_eq!(result.usage.login_method.as_deref(), Some("Muse login"));
        assert!(has_detail(&result, "quota"), "status {status}");
    }
}

#[tokio::test]
async fn a_browser_session_for_another_account_never_supplies_quotas() {
    for me in [
        r#"{"email":"bob@example.com"}"#,
        r#"{"userId":"1"}"#,
        r#"{"email":7}"#,
    ] {
        let api = FakeApi::new(move |_, path| match path {
            "/api/auth/me" => (200, me.to_string()),
            "/api/portal/teams" => (200, TEAMS.to_string()),
            _ => (200, idle_quota()),
        });
        let reading = read(&api, TEAM_ID).await;
        assert!(reading.is_none(), "{me}");
        assert!(
            api.paths()
                .iter()
                .all(|path| !path.starts_with("/api/portal")),
            "{me}"
        );
        let result = fallback_result(reading);
        assert_eq!(
            result.usage.account_email.as_deref(),
            Some("ada@example.com")
        );
        assert!(result.usage.secondary.is_none());
    }
}

#[tokio::test]
async fn a_login_without_an_email_never_matches_a_session() {
    let api = FakeApi::with_quota(idle_quota());
    let identity = LoginIdentity {
        email: None,
        ..identity()
    };
    let reading = read_team_quota(
        &api,
        &[FIXTURE_COOKIE.to_string()],
        &identity,
        TEAM_ID,
        now(),
    )
    .await;
    assert!(reading.is_none());
    assert!(!api.quota_requested());
}

#[tokio::test]
async fn rejected_and_wrong_account_sessions_advance_to_the_matching_account() {
    for first_status in [200_u16, 401, 403, 500] {
        let api = FakeApi::new(move |cookie, path| {
            if cookie == "llama_dev_sess=first" {
                assert_eq!(path, "/api/auth/me");
                return (first_status, r#"{"email":"other@example.com"}"#.to_string());
            }
            match path {
                "/api/auth/me" => (200, ME.to_string()),
                "/api/portal/teams" => (200, TEAMS.to_string()),
                _ => (200, idle_quota()),
            }
        });
        let sessions = [
            "llama_dev_sess=first".to_string(),
            "llama_dev_sess=matching".to_string(),
        ];
        let reading = read_team_quota(&api, &sessions, &identity(), TEAM_ID, now()).await;
        let result = fallback_result(reading);
        assert_eq!(result.source_label, "oauth+web", "status {first_status}");
        assert!(result.usage.secondary.is_some());
    }
}

#[tokio::test]
async fn a_session_rejected_after_the_email_check_advances() {
    let api = FakeApi::new(|cookie, path| match path {
        "/api/auth/me" => (200, ME.to_string()),
        "/api/portal/teams" if cookie == "llama_dev_sess=first" => (401, "{}".to_string()),
        "/api/portal/teams" => (200, TEAMS.to_string()),
        _ => (200, idle_quota()),
    });
    let sessions = [
        "llama_dev_sess=first".to_string(),
        "llama_dev_sess=second".to_string(),
    ];
    let reading = read_team_quota(&api, &sessions, &identity(), TEAM_ID, now()).await;
    assert_eq!(fallback_result(reading).source_label, "oauth+web");
}

#[tokio::test]
async fn browser_session_retries_stay_within_the_request_budget() {
    let api = FakeApi::new(|_, _| (200, r#"{"email":"other@example.com"}"#.to_string()));
    let sessions: Vec<String> = (0..10)
        .map(|index| format!("llama_dev_sess=s{index}"))
        .collect();
    let reading = read_team_quota(&api, &sessions, &identity(), TEAM_ID, now()).await;
    assert!(reading.is_none());
    assert_eq!(api.paths().len(), 5);
}

#[tokio::test]
async fn an_exhausted_budget_stops_before_the_quota_request() {
    // Four wrong-account sessions use four requests, so the matching session
    // has one left for /api/auth/me and none for the team list.
    let api = FakeApi::new(|cookie, path| match path {
        "/api/auth/me" if cookie == "llama_dev_sess=match" => (200, ME.to_string()),
        "/api/auth/me" => (200, r#"{"email":"other@example.com"}"#.to_string()),
        "/api/portal/teams" => (200, TEAMS.to_string()),
        _ => (200, idle_quota()),
    });
    let mut sessions: Vec<String> = (0..4)
        .map(|index| format!("llama_dev_sess=s{index}"))
        .collect();
    sessions.push("llama_dev_sess=match".to_string());
    let reading = read_team_quota(&api, &sessions, &identity(), TEAM_ID, now()).await;
    assert!(reading.is_none());
    assert_eq!(api.paths().len(), 5);
    assert!(!api.quota_requested());
}

#[tokio::test]
async fn a_non_array_team_list_ends_the_browser_path() {
    let api = FakeApi::new(|_, path| match path {
        "/api/auth/me" => (200, ME.to_string()),
        "/api/portal/teams" => (200, r#"{"teams":"none"}"#.to_string()),
        _ => (200, idle_quota()),
    });
    assert!(read(&api, TEAM_ID).await.is_none());
    assert!(!api.quota_requested());
}

#[tokio::test]
async fn team_entries_need_a_digits_only_id_and_fall_back_to_it_for_a_name() {
    let teams = r#"{"teams":[
        {"team_id":"11","team_name":"  Alpha  "},
        {"team_id":22},
        {"team_id":"abc","team_name":"Bad"},
        {"team_id":-4,"team_name":"Negative"},
        {"team_id":1.5,"team_name":"Fractional"},
        {"team_name":"No id"},
        "not an object",
        {"team_id":"33","team_name":7}
    ]}"#;
    let api = FakeApi::new(move |_, path| match path {
        "/api/auth/me" => (200, ME.to_string()),
        "/api/portal/teams" => (200, teams.to_string()),
        _ => (404, "{}".to_string()),
    });
    let reading = read(&api, "").await.unwrap();
    let listed: Vec<(String, String)> = reading
        .teams
        .iter()
        .map(|team| (team.id.clone(), team.name.clone()))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("11".to_string(), "Alpha".to_string()),
            ("22".to_string(), "22".to_string()),
            ("33".to_string(), "33".to_string()),
        ]
    );
}

#[test]
fn cookie_access_is_off_until_a_cookie_or_browser_import_is_configured() {
    let mut ctx = FetchContext::default();
    assert_eq!(CookieAccess::from_context(&ctx), CookieAccess::Off);
    ctx.manual_cookie_header = Some("   ".to_string());
    assert_eq!(CookieAccess::from_context(&ctx), CookieAccess::Off);
    ctx.manual_cookie_missing = true;
    assert_eq!(CookieAccess::from_context(&ctx), CookieAccess::Off);
    ctx.browser_cookie_import = true;
    ctx.manual_cookie_header = None;
    assert_eq!(CookieAccess::from_context(&ctx), CookieAccess::Browser);
    ctx.manual_cookie_header = Some("llama_dev_sess=abc".to_string());
    assert_eq!(
        CookieAccess::from_context(&ctx),
        CookieAccess::Manual("llama_dev_sess=abc".to_string())
    );
}

#[tokio::test]
async fn disabled_cookies_never_contact_dev_meta_ai() {
    // The default context is the Windows default: manual source, no cookie.
    assert!(
        fetch_reading(&FetchContext::default(), &identity())
            .await
            .is_none()
    );
}

#[test]
fn only_the_session_cookie_is_forwarded() {
    assert_eq!(
        session_cookie("theme=dark; llama_dev_sess=abc123; other=1").as_deref(),
        Some("llama_dev_sess=abc123")
    );
    assert_eq!(session_cookie("theme=dark"), None);
    assert_eq!(session_cookie("llama_dev_sess="), None);
    // Duplicates are ambiguous and rejected.
    assert_eq!(session_cookie("llama_dev_sess=a; llama_dev_sess=b"), None);
}

#[test]
fn manual_cookies_accept_headers_cookie_lines_and_curl_captures() {
    let expected = Some("llama_dev_sess=abc123".to_string());
    assert_eq!(manual_session_cookie("llama_dev_sess=abc123"), expected);
    assert_eq!(
        manual_session_cookie("Cookie: a=1; llama_dev_sess=abc123"),
        expected
    );
    assert_eq!(
        manual_session_cookie(
            "curl 'https://dev.meta.ai/api/auth/me' -H 'accept: */*' -H 'Cookie: a=1; llama_dev_sess=abc123' -H 'user-agent: x'"
        ),
        expected
    );
    assert_eq!(
        manual_session_cookie("curl 'https://dev.meta.ai/api/auth/me' -H 'accept: */*'"),
        None
    );
    assert_eq!(manual_session_cookie("unrelated=1"), None);
}

#[test]
fn weighted_amounts_accept_decimal_strings_and_reject_everything_else() {
    use serde_json::json;
    assert_eq!(amount(&json!("60000000000")), Some(60_000_000_000.0));
    assert_eq!(amount(&json!(5)), Some(5.0));
    for invalid in [
        json!("1e3"),
        json!("-1"),
        json!(""),
        json!(-1),
        json!(true),
        json!(null),
        json!("0x10"),
    ] {
        assert_eq!(amount(&invalid), None, "{invalid}");
    }
    assert_eq!(percent(&json!("50"), &json!("0")), None);
    assert_eq!(percent(&json!("500"), &json!("100")), Some(100.0));
}

#[test]
fn windowless_result_without_a_reading_matches_the_login_only_shape() {
    let result = fallback_result(None);
    assert_eq!(result.source_label, "oauth");
    assert!(!result.pace_authoritative);
    assert!(result.usage.primary.is_informational);
    assert_eq!(
        detail(&result, "quota").value(),
        "Not included in this login response"
    );
    assert_eq!(detail(&result, "plan").value(), "Muse Code Power Usage");
    assert_eq!(result.display_details().len(), 2);
}

#[test]
fn app_bound_encryption_is_reported_as_a_status_note() {
    let reading = WebReading {
        note: Some(TeamNote::CookiesProtected),
        ..WebReading::default()
    };
    let result = fallback_result(Some(reading));
    assert!(
        detail(&result, "browser-teams-status")
            .value()
            .contains("app-bound encryption")
    );
    assert!(has_detail(&result, "quota"));
}
