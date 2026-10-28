//! Privacy-limited mapping of local coding sessions to Discord activity.

use crate::agent_sessions::{AgentSession, AgentSessionProvider, AgentSessionState};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde_json::{Value, json};

mod metadata;

/// Immutable public fork asset, proxied by Discord for the activity image.
const APP_ICON_URL: &str = "https://raw.githubusercontent.com/lingmulongtai/Win-CodexBar-Mobile-Connect/9ea38f9b6e815ea1d28827ee6423bf4350abe34d/rust/assets/icons/CodexBar-app-icon.png";
const CODEX_ICON_URL: &str = "https://raw.githubusercontent.com/lingmulongtai/Win-CodexBar-Mobile-Connect/9ea38f9b6e815ea1d28827ee6423bf4350abe34d/apps/desktop-tauri/src/assets/brand-icons/Brand-ProviderIcon-codex.png";
const CLAUDE_ICON_URL: &str = "https://claude.ai/images/claude_app_icon.png";
const REPOSITORY_URL: &str = "https://github.com/lingmulongtai/Win-CodexBar-Mobile-Connect";

#[derive(Debug, Clone, Copy, Default)]
pub struct UsagePeriods {
    pub today: u64,
    pub week: u64,
    pub month: u64,
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
    let (history, incomplete) = crate::cost_scanner::get_daily_token_history(name, 31);
    // The chart's Codex incomplete flag treats absent (zero-use) days as
    // uncovered. The independently checked summary establishes coverage for
    // the full window, including those legitimate empty days.
    if !history_usable(provider, codex_coverage, incomplete) {
        return None;
    }
    let today = crate::cost_reporting_period::cost_bucket_zone().date(now);
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
        "assets": {"large_image": APP_ICON_URL, "large_text": "Win-CodexBar + Mobile Connect"},
        "buttons": [{"label": "View on GitHub", "url": REPOSITORY_URL}]
    })
}

pub fn idle_activity_with_usage(
    provider: Option<AgentSessionProvider>,
    periods: Option<UsagePeriods>,
) -> Value {
    let mut activity = idle_activity();
    if let Some(provider) = provider {
        let (name, icon) = match provider {
            AgentSessionProvider::Codex => ("Codex", CODEX_ICON_URL),
            AgentSessionProvider::Claude => ("Claude Code", CLAUDE_ICON_URL),
            AgentSessionProvider::Pi => ("Pi / OMP", APP_ICON_URL),
        };
        activity["details"] = json!(format!("💤 {name} · idle"));
        activity["assets"] = json!({"large_image": icon, "large_text": name});
    }
    if let Some(p) = periods {
        activity["state"] = json!(format!(
            "📊 D {} · W {} · M {}",
            compact_tokens(p.today),
            compact_tokens(p.week),
            compact_tokens(p.month)
        ));
    }
    activity
}

fn current_session(sessions: &[AgentSession], now: DateTime<Utc>) -> Option<&AgentSession> {
    sessions
        .iter()
        .filter(|session| {
            session.state == AgentSessionState::Active
                && session.activity.last_activity_at.is_some_and(|last| {
                    let age = now.signed_duration_since(last);
                    age >= Duration::zero() && age <= Duration::seconds(120)
                })
        })
        .max_by_key(|session| {
            session
                .activity
                .last_activity_at
                .or(session.activity.started_at)
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
    let mut details = meta.model.map_or_else(
        || format!("⚡ Using {name}"),
        |model| format!("⚡ {name} · {model}"),
    );
    if let Some(effort) = meta.effort {
        details.push_str(&format!(" ({effort})"));
    }
    match (session.provider, meta.fast) {
        (AgentSessionProvider::Codex, Some(true)) => details.push_str(" · 🚀 Fast requested"),
        (AgentSessionProvider::Claude, Some(true)) => details.push_str(" · 🚀 Fast"),
        (_, Some(false)) => details.push_str(" · Standard"),
        _ => {}
    }
    let icon = match session.provider {
        AgentSessionProvider::Codex => CODEX_ICON_URL,
        AgentSessionProvider::Claude => CLAUDE_ICON_URL,
        AgentSessionProvider::Pi => APP_ICON_URL,
    };
    let mut activity = json!({
        "details": details,
        "status_display_type": 2,
        "assets": {"large_image": icon, "large_text": name},
        "buttons": [{"label": "View on GitHub", "url": REPOSITORY_URL}]
    });
    let mut labels = Vec::new();
    if let Some(tokens) = meta.tokens {
        labels.push(format!("S {}", compact_tokens(tokens)));
    }
    if let Some(p) = periods {
        labels.extend([
            format!("D {}", compact_tokens(p.today)),
            format!("W {}", compact_tokens(p.week)),
            format!("M {}", compact_tokens(p.month)),
        ]);
    }
    if !labels.is_empty() {
        activity["state"] = json!(format!("📊 {}", labels.join(" · ")));
    }
    if let Some(start) = meta.started_at {
        activity["timestamps"] = json!({"start": start.timestamp()});
    }
    Some(activity)
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
    fn idle_activity_does_not_claim_a_session_is_active() {
        let activity = idle_activity();
        assert_eq!(activity["details"], "📊 Monitoring AI usage");
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
            }),
        );
        assert_eq!(activity["details"], "💤 Claude Code · idle");
        assert_eq!(activity["state"], "📊 D 1.0k · W 2.4k · M 11.0k");
        assert!(activity.get("timestamps").is_none());
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
            let icon = match provider {
                AgentSessionProvider::Codex => CODEX_ICON_URL,
                AgentSessionProvider::Claude => CLAUDE_ICON_URL,
                AgentSessionProvider::Pi => APP_ICON_URL,
            };
            assert_eq!(activity["assets"]["large_image"], icon);
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
