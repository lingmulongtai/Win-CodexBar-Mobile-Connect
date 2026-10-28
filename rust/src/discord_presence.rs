//! Privacy-limited mapping of local coding sessions to Discord activity.

use crate::agent_sessions::{AgentSession, AgentSessionProvider, AgentSessionState};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

/// Immutable public fork asset, proxied by Discord for the activity image.
const APP_ICON_URL: &str = "https://raw.githubusercontent.com/lingmulongtai/Win-CodexBar-Mobile-Connect/9ea38f9b6e815ea1d28827ee6423bf4350abe34d/rust/assets/icons/CodexBar-app-icon.png";

/// Pick the most recently active session with a recent local transcript.
/// Requiring a timestamp avoids displaying the app's own short-lived CLI probes.
pub fn activity_for_sessions(sessions: &[AgentSession], now: DateTime<Utc>) -> Option<Value> {
    let session = sessions
        .iter()
        .filter(|session| {
            if session.state != AgentSessionState::Active {
                return false;
            }
            session.activity.last_activity_at.is_some_and(|last| {
                let age = now.signed_duration_since(last);
                age >= Duration::zero() && age <= Duration::seconds(120)
            })
        })
        .max_by_key(|session| {
            session
                .activity
                .last_activity_at
                .or(session.activity.started_at)
        })?;
    let name = match session.provider {
        AgentSessionProvider::Codex => "Codex",
        AgentSessionProvider::Claude => "Claude Code",
        AgentSessionProvider::Pi => "Pi / OMP",
    };
    // Never serialize session IDs, transcripts, project names or paths.
    Some(json!({
        "details": format!("Using {name}"),
        "status_display_type": 2,
        "assets": {"large_image": APP_ICON_URL, "large_text": "Win-CodexBar + Mobile Connect"}
    }))
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
    fn maps_all_supported_providers_without_private_data() {
        let now = Utc::now();
        for (provider, expected) in [
            (AgentSessionProvider::Codex, "Codex"),
            (AgentSessionProvider::Claude, "Claude Code"),
            (AgentSessionProvider::Pi, "Pi / OMP"),
        ] {
            let activity = activity_for_sessions(&[session(provider, now)], now).unwrap();
            assert_eq!(activity["details"], format!("Using {expected}"));
            assert_eq!(activity["assets"]["large_image"], APP_ICON_URL);
            assert_eq!(
                activity["assets"]["large_text"],
                "Win-CodexBar + Mobile Connect"
            );
            assert_eq!(activity["status_display_type"], 2);
            assert_eq!(activity.as_object().unwrap().len(), 3);
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
            "Using Claude Code"
        );
    }
}
