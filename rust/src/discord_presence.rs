//! Privacy-limited mapping of local coding sessions to Discord activity.

use crate::agent_sessions::{AgentSession, AgentSessionProvider, AgentSessionState};
use crate::settings::DiscordPresenceOptions;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde_json::{Value, json};

mod metadata;
pub mod pet_links;

pub(crate) const MAX_OPEN_TURN_SECONDS: i64 = 4 * 60 * 60;

/// User-approved public GIF of the selected Codex pet's idle animation.
const PET_ICON_URL: &str = "https://raw.githubusercontent.com/lingmulongtai/Win-CodexBar-Mobile-Connect/d40229141dc33ca54f86aa2d6eb6b42c8667ee7e/rust/assets/discord/pets/maomao-idle.gif";
const REPOSITORY_URL: &str = "https://github.com/lingmulongtai/Win-CodexBar-Mobile-Connect";

#[derive(Debug, Clone, Copy, Default)]
pub struct UsagePeriods {
    pub today: u64,
    pub week: u64,
    pub month: u64,
    /// Calendar totals rebuilt from currently readable local files only.
    pub local_files_only: bool,
}

fn compact_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Calendar totals are based on the same local bucket zone and token rules as
/// the Usage & Spend chart. A missing or incomplete scan stays unavailable.
pub fn usage_periods(provider: AgentSessionProvider, now: DateTime<Utc>) -> Option<UsagePeriods> {
    if !crate::settings::Settings::load().cost_usage_enabled {
        return None;
    }
    let name = match provider {
        AgentSessionProvider::Codex => "codex",
        AgentSessionProvider::Claude => "claude",
        AgentSessionProvider::Pi => return None,
    };
    let codex_coverage = if provider == AgentSessionProvider::Codex {
        let summary = crate::cost_scanner::CostScanner::new(31).scan_codex();
        summary.known_zero || (summary.history_coverage_established && summary.sessions_count > 0)
    } else {
        false
    };
    let today = crate::cost_reporting_period::cost_bucket_zone().date(now);
    if provider == AgentSessionProvider::Codex && !codex_coverage {
        let (history_today, history) = crate::cost_scanner::isolated_codex_presence_history(now)?;
        let mut periods = periods_from_days(&history, history_today)?;
        periods.local_files_only = true;
        return Some(periods);
    }
    let (history, incomplete) = crate::cost_scanner::get_daily_token_history(name, 31);
    // Codex coverage comes from the summary because zero-use days need not
    // have entries in the chart's per-day cache.
    if !history_usable(provider, codex_coverage, incomplete) {
        return None;
    }
    periods_from_days(&history, today)
}

fn history_usable(provider: AgentSessionProvider, codex_coverage: bool, incomplete: bool) -> bool {
    if provider == AgentSessionProvider::Codex {
        codex_coverage
    } else {
        !incomplete
    }
}

fn periods_from_days(history: &[(String, u64)], today: NaiveDate) -> Option<UsagePeriods> {
    let week_start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let month_start = today.with_day(1)?;
    let mut result = UsagePeriods::default();
    for (day, tokens) in history {
        let day = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
        if day == today {
            result.today = result.today.checked_add(*tokens)?;
        }
        if day >= week_start && day <= today {
            result.week = result.week.checked_add(*tokens)?;
        }
        if day >= month_start && day <= today {
            result.month = result.month.checked_add(*tokens)?;
        }
    }
    Some(result)
}

/// Keep the resident application visible while no coding session is active.
pub fn idle_activity() -> Value {
    json!({
        "details": "📊 Monitoring AI usage",
        "status_display_type": 2,
        "assets": {"large_image": PET_ICON_URL, "large_text": "Win-CodexBar + Mobile Connect"},
        "buttons": [{"label": "View on GitHub", "url": REPOSITORY_URL}]
    })
}

pub fn idle_activity_with_usage(
    provider: Option<AgentSessionProvider>,
    periods: Option<UsagePeriods>,
) -> Value {
    let mut activity = idle_activity();
    if let Some(provider) = provider {
        let name = match provider {
            AgentSessionProvider::Codex => "Codex",
            AgentSessionProvider::Claude => "Claude Code",
            AgentSessionProvider::Pi => "Pi / OMP",
        };
        activity["details"] = json!(format!("💤 {name} · idle"));
        activity["assets"] = json!({"large_image": PET_ICON_URL, "large_text": name});
    }
    if let Some(p) = periods {
        activity["state"] = json!(format!(
            "📊 D{} {} · W{} {} · M{} {}",
            if p.local_files_only { "~" } else { "" },
            compact_tokens(p.today),
            if p.local_files_only { "~" } else { "" },
            compact_tokens(p.week),
            if p.local_files_only { "~" } else { "" },
            compact_tokens(p.month)
        ));
    }
    activity
}

pub fn current_session(sessions: &[AgentSession], now: DateTime<Utc>) -> Option<&AgentSession> {
    current_session_with_priority(sessions, now, "recent")
}

pub fn current_session_with_priority<'a>(
    sessions: &'a [AgentSession],
    now: DateTime<Utc>,
    priority: &str,
) -> Option<&'a AgentSession> {
    // Scanner output is usually sorted, but callers need not supply that
    // order. Limit stale transcript reads so presence cannot stall the UI.
    let mut candidates: Vec<_> = sessions.iter().collect();
    candidates.sort_by_key(|session| {
        let preferred = matches!(
            (priority, session.provider),
            ("codex", AgentSessionProvider::Codex) | ("claude", AgentSessionProvider::Claude)
        );
        (
            std::cmp::Reverse(preferred),
            std::cmp::Reverse(session.activity.last_activity_at),
        )
    });
    let mut stale_reads = 0;
    candidates.into_iter().find(|session| {
        let Some(last) = session.activity.last_activity_at else {
            return false;
        };
        let age = now.signed_duration_since(last);
        if age < Duration::zero() {
            return false;
        }
        if session.state == AgentSessionState::Active && age <= Duration::seconds(120) {
            return true;
        }
        if session.provider != AgentSessionProvider::Codex
            || age > Duration::seconds(MAX_OPEN_TURN_SECONDS)
        {
            return false;
        }
        let Some(path) = session.transcript_path.as_deref() else {
            return false;
        };
        if stale_reads >= 64 {
            return false;
        }
        stale_reads += 1;
        // Give the four newest candidates a wider search, while bounding
        // total I/O if many stale sessions are present.
        let Some(started) =
            metadata::codex_open_turn(std::path::Path::new(path), now, stale_reads <= 4)
        else {
            return false;
        };
        // Process-to-transcript correlation is approximate. Require a
        // live Codex process that existed when the turn began, and bound
        // old incomplete records to avoid retaining a crashed session.
        sessions.iter().any(|candidate| {
            candidate.provider == AgentSessionProvider::Codex
                && candidate.pid.is_some()
                && candidate
                    .activity
                    .started_at
                    .is_some_and(|time| time <= started)
        })
    })
}

pub fn current_provider(
    sessions: &[AgentSession],
    now: DateTime<Utc>,
) -> Option<AgentSessionProvider> {
    current_session(sessions, now).map(|s| s.provider)
}

/// Pick the most recently active session with a recent local transcript.
/// Requiring a timestamp avoids displaying the app's own short-lived CLI probes.
pub fn activity_for_sessions(sessions: &[AgentSession], now: DateTime<Utc>) -> Option<Value> {
    activity_with_usage(sessions, now, false, None)
}

/// Include only observed model, session start, and explicitly enabled token
/// totals; no transcripts, project names or paths enter the activity payload.
pub fn activity_with_usage(
    sessions: &[AgentSession],
    now: DateTime<Utc>,
    tokens_enabled: bool,
    periods: Option<UsagePeriods>,
) -> Option<Value> {
    let session = current_session(sessions, now)?;
    Some(activity_for_session(session, now, tokens_enabled, periods))
}

pub fn activity_for_session(
    session: &AgentSession,
    now: DateTime<Utc>,
    tokens_enabled: bool,
    periods: Option<UsagePeriods>,
) -> Value {
    activity_for_session_with_options(
        session,
        now,
        tokens_enabled,
        periods,
        &DiscordPresenceOptions::default(),
    )
}

pub fn activity_for_session_with_options(
    session: &AgentSession,
    now: DateTime<Utc>,
    tokens_enabled: bool,
    periods: Option<UsagePeriods>,
    options: &DiscordPresenceOptions,
) -> Value {
    let name = match session.provider {
        AgentSessionProvider::Codex => "Codex",
        AgentSessionProvider::Claude => "Claude Code",
        AgentSessionProvider::Pi => "Pi / OMP",
    };
    let meta = session
        .transcript_path
        .as_deref()
        .map(|p| {
            metadata::read(
                std::path::Path::new(p),
                session.provider,
                now,
                tokens_enabled,
            )
        })
        .unwrap_or_default();
    let mut details = meta
        .model
        .filter(|_| options.show_model && options.format != "service")
        .map_or_else(
            || format!("⚡ Using {name}"),
            |model| match options.format.as_str() {
                "compact" => format!("⚡ {name}: {model}"),
                "modelFirst" => format!("⚡ {model} · {name}"),
                _ => format!("⚡ {name} · {model}"),
            },
        );
    if let Some(effort) = meta.effort.filter(|_| options.show_mode) {
        details.push_str(&format!(" ({effort})"));
    }
    match (session.provider, meta.fast.filter(|_| options.show_mode)) {
        (AgentSessionProvider::Codex, Some(true)) => details.push_str(" · 🚀 Fast requested"),
        (AgentSessionProvider::Claude, Some(true)) => details.push_str(" · 🚀 Fast"),
        (_, Some(false)) => details.push_str(" · Standard"),
        _ => {}
    }
    let mut activity = json!({
        "details": details,
        "status_display_type": 2,
        "assets": {"large_image": PET_ICON_URL, "large_text": name},
        "buttons": [{"label": "View on GitHub", "url": REPOSITORY_URL}]
    });
    let mut labels = Vec::new();
    if let Some(tokens) = meta.tokens.filter(|_| options.show_session_tokens) {
        labels.push(format!("S {}", compact_tokens(tokens)));
    }
    if let Some(p) = periods {
        if options.show_today {
            labels.push(format!(
                "D{} {}",
                if p.local_files_only { "~" } else { "" },
                compact_tokens(p.today)
            ));
        }
        if options.show_week {
            labels.push(format!(
                "W{} {}",
                if p.local_files_only { "~" } else { "" },
                compact_tokens(p.week)
            ));
        }
        if options.show_month {
            labels.push(format!(
                "M{} {}",
                if p.local_files_only { "~" } else { "" },
                compact_tokens(p.month)
            ));
        }
    }
    if !labels.is_empty() {
        activity["state"] = json!(format!("📊 {}", labels.join(" · ")));
    }
    if let Some(start) = meta.started_at.filter(|_| options.show_elapsed) {
        activity["timestamps"] = json!({"start": start.timestamp()});
    }
    apply_presentation(&mut activity, options);
    activity
}

/// Apply the same privacy and appearance choices to active and idle activity.
pub fn apply_presentation(activity: &mut Value, options: &DiscordPresenceOptions) {
    if !options.show_github
        && let Some(object) = activity.as_object_mut()
    {
        object.remove("buttons");
    }
    if options.show_github
        && let Ok(url) = reqwest::Url::parse(options.github_url.trim())
        && url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && options.github_url.len() <= 512
    {
        activity["buttons"][0]["url"] = json!(url.as_str());
    }
    if !options.show_emoji {
        for key in ["details", "state"] {
            if let Some(text) = activity[key].as_str() {
                let cleaned = text
                    .replace("⚡ ", "")
                    .replace("💤 ", "")
                    .replace("📊 ", "")
                    .replace("🚀 ", "");
                activity[key] = json!(cleaned);
            }
        }
    }
    if options.format == "compact"
        && let Some(details) = activity["details"].as_str()
    {
        activity["details"] = json!(details.replace("Using ", ""));
    }
    // Only public HTTPS URLs may be sent to Discord. Never transmit local paths,
    // credentials, fragments, or a malformed value as an image reference.
    let url = options.image_url.trim();
    if url.len() <= 512
        && let Ok(parsed) = reqwest::Url::parse(url)
        && parsed.scheme() == "https"
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.fragment().is_none()
    {
        activity["assets"]["large_image"] = json!(url);
    }
}

pub fn idle_activity_with_options(
    provider: Option<AgentSessionProvider>,
    periods: Option<UsagePeriods>,
    options: &DiscordPresenceOptions,
) -> Value {
    let mut activity = idle_activity_with_usage(provider, None);
    let mut labels = Vec::new();
    if let Some(p) = periods {
        if options.show_today {
            labels.push(format!(
                "D{} {}",
                if p.local_files_only { "~" } else { "" },
                compact_tokens(p.today)
            ));
        }
        if options.show_week {
            labels.push(format!(
                "W{} {}",
                if p.local_files_only { "~" } else { "" },
                compact_tokens(p.week)
            ));
        }
        if options.show_month {
            labels.push(format!(
                "M{} {}",
                if p.local_files_only { "~" } else { "" },
                compact_tokens(p.month)
            ));
        }
    }
    if !labels.is_empty() {
        activity["state"] = json!(format!("📊 {}", labels.join(" · ")));
    }
    apply_presentation(&mut activity, options);
    activity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::{
        AgentSessionActivity, AgentSessionFocusTarget, AgentSessionSource, AgentSessionWorkspace,
    };

    fn session(provider: AgentSessionProvider, now: DateTime<Utc>) -> AgentSession {
        AgentSession {
            id: "private-session".into(),
            provider,
            dialect: None,
            session_name: Some("private-title".into()),
            source: AgentSessionSource::Cli,
            state: AgentSessionState::Active,
            pid: Some(1),
            transcript_path: Some("private-path".into()),
            host: "private-host".into(),
            workspace: AgentSessionWorkspace {
                cwd: Some("private-cwd".into()),
                project_name: Some("private-project".into()),
            },
            activity: AgentSessionActivity {
                started_at: Some(now),
                last_activity_at: Some(now),
            },
            focus_target: AgentSessionFocusTarget::None,
        }
    }

    #[test]
    fn preferred_provider_wins_only_when_it_is_eligible() {
        let now = Utc::now();
        let mut codex = session(AgentSessionProvider::Codex, now);
        codex.activity.last_activity_at = Some(now - Duration::seconds(30));
        let claude = session(AgentSessionProvider::Claude, now);
        let sessions = vec![codex.clone(), claude.clone()];
        assert_eq!(
            current_session_with_priority(&sessions, now, "codex")
                .unwrap()
                .provider,
            AgentSessionProvider::Codex
        );
        assert_eq!(
            current_session_with_priority(&sessions, now, "recent")
                .unwrap()
                .provider,
            AgentSessionProvider::Claude
        );
        codex.state = AgentSessionState::Idle;
        codex.transcript_path = None;
        let sessions = vec![codex, claude];
        assert_eq!(
            current_session_with_priority(&sessions, now, "codex")
                .unwrap()
                .provider,
            AgentSessionProvider::Claude
        );
    }

    #[test]
    fn idle_preferences_hide_selected_fields_and_reject_local_image_paths() {
        let options = DiscordPresenceOptions {
            show_today: false,
            show_week: true,
            show_month: false,
            show_github: false,
            show_emoji: false,
            image_url: "C:/private/pet.gif".into(),
            ..Default::default()
        };
        let activity = idle_activity_with_options(
            Some(AgentSessionProvider::Codex),
            Some(UsagePeriods {
                today: 1,
                week: 2,
                month: 3,
                local_files_only: false,
            }),
            &options,
        );
        assert_eq!(activity["details"], "Codex · idle");
        assert_eq!(activity["state"], "W 2");
        assert!(activity.get("buttons").is_none());
        assert_eq!(activity["assets"]["large_image"], PET_ICON_URL);
    }

    #[test]
    fn active_customization_hides_selected_metadata_and_changes_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, concat!(
            "{\"type\":\"session_meta\",\"timestamp\":\"2026-01-01T01:00:00Z\",\"payload\":{\"id\":\"s\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"model\":\"gpt-6.1\",\"reasoning_effort\":\"high\",\"service_tier\":\"priority\"}}}\n"
        )).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut s = session(AgentSessionProvider::Codex, now);
        s.transcript_path = Some(path.to_string_lossy().to_string());
        let mut options = DiscordPresenceOptions {
            format: "compact".into(),
            show_mode: false,
            show_elapsed: false,
            show_emoji: false,
            image_url: "https://example.com/pet.gif".into(),
            ..Default::default()
        };
        let activity = activity_for_session_with_options(&s, now, false, None, &options);
        assert_eq!(activity["details"], "Codex: gpt-6.1");
        assert!(activity.get("timestamps").is_none());
        assert_eq!(
            activity["assets"]["large_image"],
            "https://example.com/pet.gif"
        );
        options.format = "modelFirst".into();
        let activity = activity_for_session_with_options(&s, now, false, None, &options);
        assert_eq!(activity["details"], "gpt-6.1 · Codex");
        options.format = "service".into();
        let activity = activity_for_session_with_options(&s, now, false, None, &options);
        assert_eq!(activity["details"], "Using Codex");
        options.format = "compact".into();
        options.show_model = false;
        options.image_url = "https://secret@example.com/pet.gif".into();
        let activity = activity_for_session_with_options(&s, now, false, None, &options);
        assert_eq!(activity["details"], "Codex");
        assert_eq!(activity["assets"]["large_image"], PET_ICON_URL);
    }

    #[test]
    fn github_button_uses_the_chosen_link_only_when_valid_and_enabled() {
        let mut options = DiscordPresenceOptions {
            github_url: "https://github.com/example/project".into(),
            ..Default::default()
        };
        let activity = idle_activity_with_options(None, None, &options);
        assert_eq!(
            activity["buttons"][0]["url"],
            "https://github.com/example/project"
        );
        options.github_url = "https://github.com.attacker.example/project".into();
        let activity = idle_activity_with_options(None, None, &options);
        assert_eq!(activity["buttons"][0]["url"], REPOSITORY_URL);
        options.show_github = false;
        assert!(
            idle_activity_with_options(None, None, &options)
                .get("buttons")
                .is_none()
        );
    }

    #[test]
    fn presence_preferences_round_trip_and_are_local_only() {
        let mut settings: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(
            settings.discord_presence_options,
            DiscordPresenceOptions::default()
        );
        settings.discord_presence_options.priority = "claude".into();
        let saved = serde_json::to_string(&settings).unwrap();
        let restored: crate::settings::Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(restored.discord_presence_options.priority, "claude");
        let portable = crate::settings::PreferencesDocument::from_settings(&settings).unwrap();
        assert!(!portable.to_json().contains("discord_presence_options"));
    }

    #[test]
    fn idle_activity_does_not_claim_a_session_is_active() {
        let activity = idle_activity();
        assert_eq!(activity["details"], "📊 Monitoring AI usage");
        assert_eq!(activity["assets"]["large_image"], PET_ICON_URL);
        assert!(PET_ICON_URL.ends_with(".gif"));
        assert!(activity.get("timestamps").is_none());
    }

    #[test]
    fn calendar_totals_cross_month_without_mixing_last_week() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 2).unwrap(); // Monday
        let history = vec![
            ("2026-02-28".into(), 100),
            ("2026-03-01".into(), 50),
            ("2026-03-02".into(), 25),
        ];
        let actual = periods_from_days(&history, day).unwrap();
        assert_eq!((actual.today, actual.week, actual.month), (25, 25, 75));
    }

    #[test]
    fn codex_complete_history_accepts_zero_use_days() {
        assert!(history_usable(AgentSessionProvider::Codex, true, true));
        assert!(!history_usable(AgentSessionProvider::Codex, false, false));
        assert!(!history_usable(AgentSessionProvider::Claude, false, true));
    }

    #[test]
    fn idle_presence_labels_previous_provider_and_calendar_usage() {
        let activity = idle_activity_with_usage(
            Some(AgentSessionProvider::Claude),
            Some(UsagePeriods {
                today: 1000,
                week: 2400,
                month: 11000,
                local_files_only: false,
            }),
        );
        assert_eq!(activity["details"], "💤 Claude Code · idle");
        assert_eq!(activity["state"], "📊 D 1.0k · W 2.4k · M 11.0k");
        assert!(activity.get("timestamps").is_none());
    }

    #[test]
    fn current_files_only_calendar_totals_are_marked() {
        let activity = idle_activity_with_options(
            Some(AgentSessionProvider::Codex),
            Some(UsagePeriods {
                today: 1000,
                week: 2000,
                month: 3000,
                local_files_only: true,
            }),
            &DiscordPresenceOptions::default(),
        );
        assert_eq!(activity["state"], "📊 D~ 1.0k · W~ 2.0k · M~ 3.0k");
    }

    #[test]
    fn active_presence_includes_model_session_totals_and_calendar_totals_without_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private-project.jsonl");
        let row = r#"{"type":"assistant","timestamp":"2026-01-01T01:00:00Z","requestId":"r1","message":{"id":"m1","model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":20}}}"#;
        std::fs::write(&path, format!("{row}\n")).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut s = session(AgentSessionProvider::Claude, now);
        s.transcript_path = Some(path.to_string_lossy().to_string());
        let activity = activity_with_usage(
            &[s],
            now,
            true,
            Some(UsagePeriods {
                today: 2000,
                week: 3000,
                month: 4000,
                local_files_only: false,
            }),
        )
        .unwrap();
        assert_eq!(activity["details"], "⚡ Claude Code · claude-sonnet-4-6");
        assert_eq!(activity["state"], "📊 S 120 · D 2.0k · W 3.0k · M 4.0k");
        assert_eq!(activity["timestamps"]["start"], 1767229200);
        assert_eq!(activity["buttons"][0]["url"], REPOSITORY_URL);
        assert!(!activity.to_string().contains("private-project"));
    }

    #[test]
    fn codex_effort_and_requested_fast_mode_are_visible() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, concat!(
            "{\"type\":\"session_meta\",\"timestamp\":\"2026-01-01T01:00:00Z\",\"payload\":{\"id\":\"s\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"model\":\"gpt-6.1\",\"reasoning_effort\":\"high\",\"service_tier\":\"priority\"}}}\n"
        )).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut s = session(AgentSessionProvider::Codex, now);
        s.transcript_path = Some(path.to_string_lossy().to_string());
        let activity = activity_with_usage(&[s], now, false, None).unwrap();
        assert_eq!(
            activity["details"],
            "⚡ Codex · gpt-6.1 (High) · 🚀 Fast requested"
        );
    }

    #[test]
    fn maps_all_supported_providers_without_private_data() {
        let now = Utc::now();
        for (provider, expected) in [
            (AgentSessionProvider::Codex, "Codex"),
            (AgentSessionProvider::Claude, "Claude Code"),
            (AgentSessionProvider::Pi, "Pi / OMP"),
        ] {
            let activity = activity_for_sessions(&[session(provider, now)], now).unwrap();
            assert_eq!(activity["details"], format!("⚡ Using {expected}"));
            assert_eq!(activity["assets"]["large_image"], PET_ICON_URL);
            assert_eq!(activity["assets"]["large_text"], expected);
            assert_eq!(activity["status_display_type"], 2);
            assert_eq!(activity["buttons"][0]["url"], REPOSITORY_URL);
            assert_eq!(activity.as_object().unwrap().len(), 4);
            assert!(!activity.to_string().contains("private"));
        }
    }

    #[test]
    fn ignores_idle_stale_and_future_file_only_sessions() {
        let now = Utc::now();
        let mut s = session(AgentSessionProvider::Codex, now);
        s.state = AgentSessionState::Idle;
        assert!(activity_for_sessions(&[s.clone()], now).is_none());
        s.state = AgentSessionState::Active;
        s.pid = None;
        for age in [-1, 121] {
            s.activity.last_activity_at = Some(now - Duration::seconds(age));
            assert!(activity_for_sessions(&[s.clone()], now).is_none());
        }
        s.activity.last_activity_at = Some(now - Duration::seconds(120));
        assert!(activity_for_sessions(&[s], now).is_some());
    }

    #[test]
    fn ignores_process_without_recent_transcript() {
        let now = Utc::now();
        let mut s = session(AgentSessionProvider::Claude, now);
        s.activity.last_activity_at = None;
        assert!(activity_for_sessions(&[s], now).is_none());
    }

    #[test]
    fn keeps_codex_active_for_a_long_open_turn_but_not_after_completion() {
        let now = Utc::now();
        let started = now - Duration::minutes(10);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let start = format!(
            "{{\"type\":\"event_msg\",\"timestamp\":\"{}\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"turn-a\"}}}}\n",
            started.to_rfc3339()
        );
        let mut s = session(AgentSessionProvider::Codex, now);
        s.state = AgentSessionState::Idle;
        s.activity.last_activity_at = Some(started);
        s.activity.started_at = Some(started - Duration::minutes(1));
        s.transcript_path = Some(path.to_string_lossy().to_string());
        std::fs::write(&path, &start).unwrap();
        assert_eq!(
            activity_for_sessions(&[s.clone()], now).unwrap()["details"],
            "⚡ Using Codex"
        );

        for end in ["task_complete", "turn_aborted"] {
            let close = format!(
                "{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"{end}\",\"turn_id\":\"turn-a\"}}}}\n"
            );
            std::fs::write(&path, format!("{start}{close}")).unwrap();
            assert!(activity_for_sessions(&[s.clone()], now).is_none(), "{end}");
        }
        std::fs::write(&path, format!("{start}{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"task_complete\",\"turn_id\":\"other\"}}}}\n")).unwrap();
        assert!(activity_for_sessions(&[s.clone()], now).is_some());
        std::fs::write(&path, format!("{start}{{\"type\":\"event_msg\"")).unwrap();
        assert!(activity_for_sessions(&[s], now).is_none());
    }

    #[test]
    fn open_turn_requires_a_plausible_live_codex_process_and_recent_evidence() {
        let now = Utc::now();
        let started = now - Duration::minutes(10);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let write_start = |time: DateTime<Utc>| {
            std::fs::write(&path, format!(
                "{{\"type\":\"event_msg\",\"timestamp\":\"{}\",\"payload\":{{\"type\":\"task_started\",\"turn_id\":\"turn-a\"}}}}\n",
                time.to_rfc3339()
            )).unwrap();
        };
        let mut s = session(AgentSessionProvider::Codex, now);
        s.state = AgentSessionState::Idle;
        s.activity.last_activity_at = Some(started);
        s.activity.started_at = Some(started - Duration::minutes(1));
        s.transcript_path = Some(path.to_string_lossy().to_string());
        write_start(started);
        let mut file_only = s.clone();
        file_only.pid = None;
        assert!(activity_for_sessions(&[file_only.clone()], now).is_none());
        let mut process = s.clone();
        process.transcript_path = None;
        process.activity.last_activity_at = None;
        assert!(activity_for_sessions(&[file_only.clone(), process.clone()], now).is_some());
        process.activity.started_at = Some(started + Duration::seconds(1));
        assert!(activity_for_sessions(&[file_only, process], now).is_none());
        write_start(now + Duration::seconds(1));
        assert!(activity_for_sessions(&[s.clone()], now).is_none());
        write_start(now - Duration::seconds(MAX_OPEN_TURN_SECONDS + 1));
        assert!(activity_for_sessions(&[s], now).is_none());
    }

    #[test]
    fn chooses_most_recent_active_session() {
        let now = Utc::now();
        let older = session(AgentSessionProvider::Codex, now - Duration::seconds(10));
        let newer = session(AgentSessionProvider::Claude, now);
        assert_eq!(
            activity_for_sessions(&[older, newer], now).unwrap()["details"],
            "⚡ Using Claude Code"
        );
    }
}
