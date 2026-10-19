use super::*;
use std::sync::Arc;

#[test]
fn payload_env_includes_event_and_remaining() {
    let event = HookEvent::new(HookEventType::QuotaLow, "claude")
        .with_used_percent(80.0)
        .with_window("session")
        .with_account("user@example.com");
    let env = event.environment_variables();
    assert_eq!(
        env.get("CODEXBAR_EVENT").map(String::as_str),
        Some("quota_low")
    );
    assert_eq!(
        env.get("CODEXBAR_PROVIDER").map(String::as_str),
        Some("claude")
    );
    assert_eq!(
        env.get("CODEXBAR_WINDOW").map(String::as_str),
        Some("session")
    );
    assert_eq!(
        env.get("CODEXBAR_ACCOUNT").map(String::as_str),
        Some("user@example.com")
    );
    assert_eq!(
        env.get("CODEXBAR_USAGE_PERCENT").map(String::as_str),
        Some("0.8")
    );
    assert_eq!(
        env.get("CODEXBAR_REMAINING_PERCENT").map(String::as_str),
        Some("20")
    );
    let payload = event.json_payload().unwrap();
    assert!(payload.len() < HooksConfig::MAX_PAYLOAD_BYTES);
    let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(parsed["event"], "quota_low");
    assert_eq!(parsed["provider"], "claude");
    assert!((parsed["remaining_percent"].as_f64().unwrap() - 20.0).abs() < 0.01);
}

#[test]
fn usage_updated_payload_carries_both_windows_without_private_rate_key() {
    let event = HookEvent::new(HookEventType::UsageUpdated, "codex")
        .with_used_percent(25.0)
        .with_window_minutes(Some(300))
        .with_reset_at(Some("2026-09-20T12:00:00Z".into()))
        .with_secondary_usage_fraction(0.4)
        .with_secondary_window_minutes(Some(10080))
        .with_secondary_reset_at(Some("2026-09-27T12:00:00Z".into()))
        .with_account("user@example.com");

    let env = event.environment_variables();
    assert_eq!(
        env.get("CODEXBAR_EVENT").map(String::as_str),
        Some("usage_updated")
    );
    assert_eq!(
        env.get("CODEXBAR_WINDOW_MINUTES").map(String::as_str),
        Some("300")
    );
    assert_eq!(
        env.get("CODEXBAR_SECONDARY_USAGE_PERCENT")
            .map(String::as_str),
        Some("0.4")
    );
    assert_eq!(
        env.get("CODEXBAR_ACCOUNT").map(String::as_str),
        Some("user@example.com")
    );

    let payload = event.json_payload().unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(parsed["event"], "usage_updated");
    assert_eq!(parsed["window_minutes"], 300);
    assert_eq!(parsed["secondary_window_minutes"], 10080);
    // The payload stays public: no private limiter scope can ride along.
    assert!(parsed.get("rate_limit_key").is_none());
    assert!(parsed.get("rate_limit_scope").is_none());
}

#[test]
fn rate_limit_scope_keys_private_suppression_per_provider() {
    let limiter = HookRateLimiter::new(Duration::from_secs(600));
    let event = HookEvent::new(HookEventType::UsageUpdated, "codex").with_used_percent(10.0);
    assert!(limiter.allow(&event, Some("provider-account:user@example.com")));
    assert!(!limiter.allow(&event, Some("provider-account:user@example.com")));
    // Same account identity on another provider must not be suppressed.
    let other = HookEvent::new(HookEventType::UsageUpdated, "claude").with_used_percent(10.0);
    assert!(limiter.allow(&other, Some("provider-account:user@example.com")));
    // Empty scope falls back to public event-identity keys.
    assert!(limiter.allow(&event, None));
    assert!(!limiter.allow(&event, Some("")));
}

#[test]
fn build_hook_environment_whitelists_only_safe_keys() {
    let mut base = HashMap::new();
    base.insert("PATH".into(), "/usr/bin".into());
    base.insert("HOME".into(), "/home/u".into());
    base.insert("USER".into(), "u".into());
    base.insert("TEMP".into(), "C:\\Temp".into());
    base.insert("SECRET_API_KEY".into(), "should-not-pass".into());
    base.insert("OPENAI_API_KEY".into(), "nope".into());
    let event = HookEvent::new(HookEventType::QuotaReached, "codex").with_used_percent(100.0);
    let env = build_hook_environment(&base, &event);
    assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(env.get("HOME").map(String::as_str), Some("/home/u"));
    assert_eq!(env.get("USER").map(String::as_str), Some("u"));
    assert_eq!(env.get("TEMP").map(String::as_str), Some("C:\\Temp"));
    assert!(!env.contains_key("SECRET_API_KEY"));
    assert!(!env.contains_key("OPENAI_API_KEY"));
    assert_eq!(
        env.get("CODEXBAR_EVENT").map(String::as_str),
        Some("quota_reached")
    );
}

#[test]
fn rate_limiter_suppresses_repeat_within_window() {
    let limiter = HookRateLimiter::new(Duration::from_secs(600));
    let event = HookEvent::new(HookEventType::RefreshFailed, "cursor");
    assert!(limiter.allow(&event, None));
    assert!(!limiter.allow(&event, None));
    let other = HookEvent::new(HookEventType::RefreshFailed, "claude");
    assert!(limiter.allow(&other, None));
}

#[test]
fn rule_matches_event_list_and_provider() {
    #[cfg(windows)]
    let absolute = PathBuf::from(r"C:\Windows\System32\cmd.exe");
    #[cfg(not(windows))]
    let absolute = PathBuf::from("/usr/bin/true");
    let rule = HookRule {
        enabled: true,
        event: None,
        events: vec![HookEventType::QuotaLow, HookEventType::QuotaReached],
        provider: Some("codex".into()),
        threshold: Some(0.7),
        executable: absolute,
        arguments: vec![],
        timeout_secs: 10,
    };
    let match_event = HookEvent::new(HookEventType::QuotaLow, "codex").with_used_percent(80.0);
    assert!(rule.matches(&match_event));
    let low = HookEvent::new(HookEventType::QuotaLow, "codex").with_used_percent(50.0);
    assert!(!rule.matches(&low));
    let other = HookEvent::new(HookEventType::QuotaLow, "claude").with_used_percent(80.0);
    assert!(!rule.matches(&other));
}

#[test]
fn config_matching_respects_enabled_flag() {
    let rule = HookRule {
        enabled: true,
        event: Some(HookEventType::QuotaReset),
        events: vec![],
        provider: None,
        threshold: None,
        executable: absolute_executable("cmd.exe"),
        arguments: vec![],
        timeout_secs: 5,
    };
    let event = HookEvent::new(HookEventType::QuotaReset, "claude");
    let disabled = HooksConfig {
        enabled: false,
        events: vec![rule.clone()],
    };
    assert!(disabled.matching_rules(&event).is_empty());
    let enabled = HooksConfig {
        enabled: true,
        events: vec![rule],
    };
    assert_eq!(enabled.matching_rules(&event).len(), 1);
}

#[test]
fn relative_executable_never_matches() {
    let rule = HookRule {
        enabled: true,
        event: Some(HookEventType::QuotaLow),
        events: vec![],
        provider: None,
        threshold: None,
        executable: PathBuf::from("notify.sh"),
        arguments: vec![],
        timeout_secs: 10,
    };
    let event = HookEvent::new(HookEventType::QuotaLow, "codex").with_used_percent(90.0);
    assert!(!rule.matches(&event));
}

#[test]
fn rate_limiter_key_is_stable() {
    let a = HookEvent::new(HookEventType::ProviderUnavailable, "warp")
        .with_window("session")
        .with_account("a");
    let b = HookEvent::new(HookEventType::ProviderUnavailable, "warp")
        .with_window("session")
        .with_account("a");
    assert_eq!(rate_limit_key(&a, None), rate_limit_key(&b, None));
    let _ = Arc::new(a);
}

fn absolute_executable(name: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Tools").join(name)
    } else {
        PathBuf::from("/usr/local/bin").join(name)
    }
}

fn rule_with(arguments: usize) -> HookRule {
    HookRule {
        enabled: true,
        event: Some(HookEventType::QuotaReached),
        events: Vec::new(),
        provider: None,
        threshold: None,
        executable: absolute_executable("notify.exe"),
        arguments: (0..arguments).map(|i| format!("--arg{i}")).collect(),
        timeout_secs: 10,
    }
}

#[test]
fn event_names_parse_back_and_unknown_names_do_not() {
    let names: Vec<&str> = HookEventType::ALL.iter().map(|e| e.as_str()).collect();
    assert_eq!(
        names,
        [
            "quota_low",
            "quota_reached",
            "quota_reset",
            "usage_updated",
            "provider_unavailable",
            "provider_recovered",
            "refresh_failed",
        ]
    );
    assert_eq!(
        HookEventType::parse("quota_low"),
        Some(HookEventType::QuotaLow)
    );
    assert_eq!(
        HookEventType::parse("refresh_failed"),
        Some(HookEventType::RefreshFailed)
    );
    assert_eq!(HookEventType::parse("quota_exhausted"), None);
    assert_eq!(HookEventType::parse("QuotaLow"), None);
}

#[test]
fn validate_accepts_32_rules_and_32_arguments() {
    let config = HooksConfig {
        enabled: true,
        events: (0..32).map(|_| rule_with(32)).collect(),
    };
    assert_eq!(config.validate(), Ok(()));
}

#[test]
fn validate_rejects_a_33rd_rule() {
    let config = HooksConfig {
        enabled: true,
        events: (0..33).map(|_| rule_with(0)).collect(),
    };
    assert_eq!(
        config.validate(),
        Err(HooksConfigError::TooManyRules { count: 33, max: 32 })
    );
}

#[test]
fn validate_rejects_a_33rd_argument() {
    let config = HooksConfig {
        enabled: false,
        events: vec![rule_with(1), rule_with(33)],
    };
    assert_eq!(
        config.validate(),
        Err(HooksConfigError::TooManyArguments {
            rule: 1,
            count: 33,
            max: 32
        })
    );
}

#[test]
fn a_rule_over_the_argument_cap_never_matches() {
    let event = HookEvent::new(HookEventType::QuotaReached, "codex");
    assert!(rule_with(32).matches(&event));
    assert!(!rule_with(33).matches(&event));
}

#[test]
fn load_existing_tells_missing_from_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks.json");
    assert_eq!(HooksConfig::load_existing(&path), Ok(None));

    std::fs::write(&path, "{ \"enabled\": true, \"events\": [ ").unwrap();
    assert!(matches!(
        HooksConfig::load_existing(&path),
        Err(HooksConfigError::Unreadable(_))
    ));

    std::fs::write(
        &path,
        "\u{feff}{\"enabled\":true,\"events\":[{\"event\":\"quota_low\",\"threshold\":0.9,\"executable\":\"C:\\\\Tools\\\\a.exe\",\"arguments\":[\"x\"]}]}",
    )
    .unwrap();
    let loaded = HooksConfig::load_existing(&path).unwrap().unwrap();
    assert!(loaded.enabled);
    assert_eq!(loaded.events.len(), 1);
    assert_eq!(loaded.events[0].event, Some(HookEventType::QuotaLow));
    assert_eq!(loaded.events[0].threshold, Some(0.9));
    assert_eq!(
        loaded.events[0].executable,
        PathBuf::from(r"C:\Tools\a.exe")
    );
    assert_eq!(loaded.events[0].timeout_secs, 10);
}

#[test]
fn save_to_replaces_an_existing_file_and_leaves_no_staging_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("hooks.json");
    let mut config = HooksConfig {
        enabled: true,
        ..HooksConfig::default()
    };
    config.save_to(&path).unwrap();
    config.enabled = false;
    config.save_to(&path).unwrap();

    assert!(!HooksConfig::load_from(&path).enabled);
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["hooks.json".to_string()]);
}
