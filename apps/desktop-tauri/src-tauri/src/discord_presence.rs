//! Opt-in local Discord RPC. Public activity may include service, model,
//! elapsed time and token counts; conversation content and paths stay local.
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

// Only the privacy-limited activity accepted by Discord is exposed to the UI.
struct SnapshotState {
    generation: u64,
    enabled: bool,
    value: Value,
}
static SNAPSHOT: OnceLock<Mutex<SnapshotState>> = OnceLock::new();

fn snapshot_store() -> &'static Mutex<SnapshotState> {
    SNAPSHOT.get_or_init(|| {
        Mutex::new(SnapshotState {
            generation: 0,
            enabled: false,
            value: json!({"status": "disabled", "activity": null, "usage": "unavailable"}),
        })
    })
}

#[cfg(windows)]
fn configure_snapshot(enabled: bool) {
    let mut state = snapshot_store().lock().unwrap_or_else(|e| e.into_inner());
    state.generation = state.generation.wrapping_add(1);
    state.enabled = enabled;
    state.value = json!({"status": if enabled { "connecting" } else { "disabled" }, "activity": null, "usage": "unavailable"});
}

#[cfg(windows)]
fn snapshot_generation() -> Option<u64> {
    let state = snapshot_store().lock().unwrap_or_else(|e| e.into_inner());
    state.enabled.then_some(state.generation)
}

#[cfg(windows)]
fn publish_snapshot(generation: u64, status: &str, activity: Option<Value>, usage: &str) {
    let mut state = snapshot_store().lock().unwrap_or_else(|e| e.into_inner());
    if state.enabled && state.generation == generation {
        state.value = json!({"status": status, "activity": activity, "usage": usage});
    }
}

#[tauri::command]
pub fn get_discord_presence_status() -> Value {
    snapshot_store()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .value
        .clone()
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, Instant};

    use codexbar::agent_sessions::LocalAgentSessionScanner;
    use serde_json::{Value, json};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use tokio::sync::watch;

    const APPLICATION_ID: &str = "[REMOVED]";
    const INTERVAL: Duration = Duration::from_secs(20);
    const IO_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_FRAME: usize = 64 * 1024;
    const USAGE_INTERVAL: Duration = Duration::from_secs(300);
    static SETTINGS: OnceLock<watch::Sender<bool>> = OnceLock::new();

    pub fn install(enabled: bool) {
        super::configure_snapshot(enabled);
        let (sender, receiver) = watch::channel(enabled);
        if SETTINGS.set(sender).is_ok() {
            tauri::async_runtime::spawn(run(receiver));
        }
    }

    pub fn settings_changed(enabled: bool) {
        super::configure_snapshot(enabled);
        if let Some(sender) = SETTINGS.get() {
            sender.send_replace(enabled);
        }
    }

    pub fn shutdown() {
        settings_changed(false);
    }

    async fn write_frame<S: AsyncWrite + Unpin>(
        stream: &mut S,
        opcode: u32,
        body: &[u8],
    ) -> io::Result<()> {
        let length =
            u32::try_from(body.len()).map_err(|_| io::Error::other("RPC frame too large"))?;
        stream.write_all(&opcode.to_le_bytes()).await?;
        stream.write_all(&length.to_le_bytes()).await?;
        stream.write_all(body).await?;
        stream.flush().await
    }

    async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<(u32, Vec<u8>)> {
        let opcode = stream.read_u32_le().await?;
        let length = stream.read_u32_le().await? as usize;
        if length > MAX_FRAME {
            return Err(io::Error::other("RPC frame too large"));
        }
        let mut bytes = vec![0; length];
        stream.read_exact(&mut bytes).await?;
        Ok((opcode, bytes))
    }

    async fn response<S: AsyncRead + AsyncWrite + Unpin>(
        stream: &mut S,
        nonce: Option<&str>,
    ) -> io::Result<()> {
        loop {
            let (opcode, bytes) = read_frame(stream).await?;
            match opcode {
                3 => write_frame(stream, 4, &bytes).await?,
                1 => {
                    let value: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                    if value["evt"] == "ERROR" {
                        return Err(io::Error::other("Discord rejected RPC request"));
                    }
                    if nonce.is_some_and(|n| value["nonce"] == n)
                        || (nonce.is_none() && value["evt"] == "READY")
                    {
                        return Ok(());
                    }
                }
                2 => return Err(io::Error::other("Discord closed RPC")),
                _ => {}
            }
        }
    }

    async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> io::Result<()> {
        let handshake = json!({"v": 1, "client_id": APPLICATION_ID}).to_string();
        write_frame(stream, 0, handshake.as_bytes()).await?;
        response(stream, None).await
    }

    async fn connect() -> io::Result<NamedPipeClient> {
        for index in 0..10 {
            let path = format!(r"\\?\pipe\discord-ipc-{index}");
            let Ok(mut stream) = ClientOptions::new().open(path) else {
                continue;
            };
            let result = tokio::time::timeout(IO_TIMEOUT, handshake(&mut stream)).await;
            if matches!(result, Ok(Ok(()))) {
                return Ok(stream);
            }
        }
        Err(io::Error::other("Discord IPC unavailable"))
    }

    async fn set_activity<S: AsyncRead + AsyncWrite + Unpin>(
        stream: &mut S,
        activity: Option<Value>,
    ) -> io::Result<()> {
        let nonce = uuid::Uuid::new_v4().to_string();
        let request = json!({"cmd": "SET_ACTIVITY", "args": {"pid": std::process::id(), "activity": activity}, "nonce": nonce}).to_string();
        tokio::time::timeout(IO_TIMEOUT, async {
            write_frame(stream, 1, request.as_bytes()).await?;
            response(stream, Some(&nonce)).await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Discord IPC timed out"))?
    }

    fn usage_status(
        activity: &Value,
        options: &codexbar::settings::DiscordPresenceOptions,
        tokens_enabled: bool,
        has_session: bool,
        has_periods: bool,
        collecting: bool,
    ) -> &'static str {
        let state = activity.get("state").and_then(Value::as_str);
        let has_session_tokens = state.is_some_and(|text| {
            text.split(" · ")
                .any(|part| part.trim_start_matches("📊 ").starts_with("S "))
        });
        let missing_session = has_session && options.show_session_tokens && !has_session_tokens;
        let missing_periods =
            !has_periods && (options.show_today || options.show_week || options.show_month);
        if !tokens_enabled {
            "trackingOff"
        } else if !options.show_session_tokens
            && !options.show_today
            && !options.show_week
            && !options.show_month
        {
            "hidden"
        } else if state.is_some() {
            if missing_session || missing_periods {
                if collecting && missing_periods {
                    "partialLoading"
                } else {
                    "partial"
                }
            } else {
                "available"
            }
        } else if collecting && missing_periods {
            "loading"
        } else {
            "unavailable"
        }
    }

    async fn run(mut enabled: watch::Receiver<bool>) {
        let mut stream: Option<NamedPipeClient> = None;
        let mut last_provider = None;
        let mut clear_before_refresh = false;
        let mut usage = [None, None];
        let mut usage_tasks: [Option<
            tokio::task::JoinHandle<Option<codexbar::discord_presence::UsagePeriods>>,
        >; 2] = [None, None];
        let mut usage_refreshed: [Option<Instant>; 2] = [None, None];
        let mut usage_day = None;
        let mut usage_task_valid = [false; 2];
        let metadata_slot = Arc::new(tokio::sync::Semaphore::new(1));
        loop {
            if !*enabled.borrow_and_update() {
                usage_task_valid = [false; 2];
                usage = [None, None];
                usage_refreshed = [None, None];
                usage_day = None;
                if let Some(mut current) = stream.take() {
                    let _result = set_activity(&mut current, None).await;
                }
                if enabled.changed().await.is_err() {
                    break;
                }
                continue;
            }
            if clear_before_refresh {
                if let Some(mut current) = stream.take() {
                    let _result = set_activity(&mut current, None).await;
                }
                clear_before_refresh = false;
            }
            let Some(generation) = super::snapshot_generation() else {
                tokio::task::yield_now().await;
                continue;
            };
            let scanner = LocalAgentSessionScanner::default();
            let scan = tokio::select! {
                biased;
                changed = enabled.changed() => { if changed.is_err() { break; } clear_before_refresh = true; continue; },
                result = scanner.scan_for_presence() => result,
            };
            let now = chrono::Utc::now();
            let settings = codexbar::settings::Settings::load();
            let tokens_enabled = settings.cost_usage_enabled;
            let options = settings.discord_presence_options;
            let today = codexbar::cost_reporting_period::cost_bucket_zone().date(now);
            if usage_day.is_some_and(|day| day != today) {
                usage_task_valid = [false; 2];
                usage = [None, None];
                usage_refreshed = [None, None];
            }
            usage_day = Some(today);
            if !tokens_enabled {
                usage_task_valid = [false; 2];
                usage = [None, None];
                usage_refreshed = [None, None];
            } else {
                use codexbar::agent_sessions::AgentSessionProvider::{Claude, Codex};
                for (index, provider) in [Codex, Claude].into_iter().enumerate() {
                    if usage_tasks[index]
                        .as_ref()
                        .is_some_and(|task| task.is_finished())
                        && let Some(task) = usage_tasks[index].take()
                    {
                        let completed = task.await.unwrap_or(None);
                        if usage_task_valid[index] {
                            usage[index] = completed;
                            usage_refreshed[index] = Some(Instant::now());
                        }
                    }
                    if usage_tasks[index].is_none()
                        && usage_refreshed[index]
                            .is_none_or(|time| time.elapsed() >= USAGE_INTERVAL)
                    {
                        usage_tasks[index] = Some(tokio::task::spawn_blocking(move || {
                            codexbar::discord_presence::usage_periods(provider, now)
                        }));
                        usage_task_valid[index] = true;
                    }
                }
            }
            // Session selection may read bounded transcript tails. Keep it
            // off the async runtime and reuse the result for the payload.
            let priority = options.priority.clone();
            let active_session = tokio::select! {
                biased;
                changed = enabled.changed() => { if changed.is_err() { break; } clear_before_refresh = true; continue; },
                result = tokio::task::spawn_blocking(move || {
                    codexbar::discord_presence::current_session_with_priority(&scan.sessions, now, &priority).cloned()
                }) => result.ok().flatten(),
            };
            let provider = active_session.as_ref().map(|session| session.provider);
            if provider.is_some() {
                last_provider = provider;
            }
            use codexbar::agent_sessions::AgentSessionProvider::{Claude, Codex};
            // On a fresh start with only one usable history, identify it
            // instead of silently hiding its calendar totals.
            let selected = provider.or(last_provider).or(match usage {
                [Some(_), None] => Some(Codex),
                [None, Some(_)] => Some(Claude),
                _ => None,
            });
            let periods = match selected {
                Some(Codex) => usage[0],
                Some(Claude) => usage[1],
                None => usage[0].zip(usage[1]).and_then(|(a, b)| {
                    Some(codexbar::discord_presence::UsagePeriods {
                        today: a.today.checked_add(b.today)?,
                        week: a.week.checked_add(b.week)?,
                        month: a.month.checked_add(b.month)?,
                    })
                }),
                _ => None,
            };
            let has_session = active_session.is_some();
            let active_options = options.clone();
            let permit = tokio::select! {
                biased;
                changed = enabled.changed() => { if changed.is_err() { break; } clear_before_refresh = true; continue; },
                result = metadata_slot.clone().acquire_owned() => match result { Ok(permit) => permit, Err(_) => break },
            };
            let activity = tokio::select! {
                biased;
                changed = enabled.changed() => { if changed.is_err() { break; } clear_before_refresh = true; continue; },
                result = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    active_session.as_ref().map(|session| {
                        codexbar::discord_presence::activity_for_session_with_options(session, now, tokens_enabled, periods, &active_options)
                    })
                }) => result.ok().flatten(),
            }.unwrap_or_else(|| codexbar::discord_presence::idle_activity_with_options(selected, periods, &options));
            if stream.is_none() {
                tokio::select! {
                    biased;
                    changed = enabled.changed() => { if changed.is_err() { break; } clear_before_refresh = true; continue; },
                    result = connect() => { stream = result.ok(); }
                }
            }
            let collecting = match selected {
                Some(Codex) => usage_tasks[0].is_some(),
                Some(Claude) => usage_tasks[1].is_some(),
                _ => usage_tasks.iter().any(Option::is_some),
            };
            let usage_status = usage_status(
                &activity,
                &options,
                tokens_enabled,
                has_session,
                periods.is_some(),
                collecting,
            );
            if let Some(current) = stream.as_mut() {
                let result = tokio::select! {
                    biased;
                    changed = enabled.changed() => { if changed.is_err() { break; } clear_before_refresh = true; None },
                    result = set_activity(current, Some(activity.clone())) => Some(result),
                };
                if matches!(result, Some(Ok(()))) && *enabled.borrow() {
                    super::publish_snapshot(generation, "connected", Some(activity), usage_status);
                } else {
                    stream = None;
                    super::publish_snapshot(generation, "connecting", None, "unavailable");
                }
            } else {
                super::publish_snapshot(generation, "connecting", None, "unavailable");
            }
            tokio::select! {
                changed = enabled.changed() => { if changed.is_err() { break; } clear_before_refresh = true; },
                () = tokio::time::sleep(INTERVAL) => {},
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn usage_status_distinguishes_loading_partial_and_disabled_tracking() {
            let options = codexbar::settings::DiscordPresenceOptions::default();
            let empty = json!({"details": "Claude Code"});
            let session = json!({"state": "📊 S 20"});
            let periods = json!({"state": "📊 D 10 · W 20 · M 30"});
            let all = json!({"state": "📊 S 20 · D 10 · W 20 · M 30"});
            assert_eq!(
                usage_status(&empty, &options, false, true, false, false),
                "trackingOff"
            );
            assert_eq!(
                usage_status(&empty, &options, true, true, false, true),
                "loading"
            );
            assert_eq!(
                usage_status(&empty, &options, true, true, false, false),
                "unavailable"
            );
            assert_eq!(
                usage_status(&session, &options, true, true, false, true),
                "partialLoading"
            );
            assert_eq!(
                usage_status(&periods, &options, true, true, true, false),
                "partial"
            );
            assert_eq!(
                usage_status(&all, &options, true, true, true, false),
                "available"
            );
        }

        #[test]
        fn stale_activity_cannot_replace_disabled_or_reconfigured_snapshot() {
            super::super::configure_snapshot(true);
            let previous = super::super::snapshot_generation().unwrap();
            super::super::configure_snapshot(false);
            super::super::publish_snapshot(
                previous,
                "connected",
                Some(json!({"details": "old"})),
                "available",
            );
            assert_eq!(
                super::super::get_discord_presence_status()["status"],
                "disabled"
            );
            super::super::configure_snapshot(true);
            super::super::publish_snapshot(
                previous,
                "connected",
                Some(json!({"details": "old"})),
                "available",
            );
            assert_eq!(
                super::super::get_discord_presence_status()["status"],
                "connecting"
            );
            let current = super::super::snapshot_generation().unwrap();
            super::super::publish_snapshot(
                current,
                "connected",
                Some(json!({"details": "new"})),
                "available",
            );
            assert_eq!(
                super::super::get_discord_presence_status()["activity"]["details"],
                "new"
            );
            super::super::configure_snapshot(false);
        }

        #[test]
        fn opt_in_defaults_off_round_trips_and_stays_out_of_portable_preferences() {
            let mut settings: codexbar::settings::Settings = serde_json::from_str("{}").unwrap();
            assert!(!settings.discord_presence_enabled);
            settings.discord_presence_enabled = true;
            let saved = serde_json::to_value(&settings).unwrap();
            let restored: codexbar::settings::Settings = serde_json::from_value(saved).unwrap();
            assert!(restored.discord_presence_enabled);
            let portable =
                codexbar::settings::PreferencesDocument::from_settings(&settings).unwrap();
            assert!(!portable.to_json().contains("discord_presence_enabled"));
        }

        #[test]
        fn rpc_handshake_waits_for_ready_with_the_owned_application_id() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (mut client, mut server) = tokio::io::duplex(4096);
                let server_task = tokio::spawn(async move {
                    let (opcode, body) = read_frame(&mut server).await.unwrap();
                    assert_eq!(opcode, 0);
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(request, json!({"v": 1, "client_id": APPLICATION_ID}));
                    write_frame(&mut server, 1, br#"{"evt":"READY"}"#)
                        .await
                        .unwrap();
                });
                handshake(&mut client).await.unwrap();
                server_task.await.unwrap();
            });
        }

        #[test]
        fn rpc_frames_handle_ping_and_clear_activity() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (mut client, mut server) = tokio::io::duplex(4096);
                let server_task = tokio::spawn(async move {
                    let (opcode, body) = read_frame(&mut server).await.unwrap();
                    assert_eq!(opcode, 1);
                    let request: Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(request["cmd"], "SET_ACTIVITY");
                    assert!(request["args"]["activity"].is_null());
                    write_frame(&mut server, 3, b"ping").await.unwrap();
                    let (opcode, body) = read_frame(&mut server).await.unwrap();
                    assert_eq!((opcode, body), (4, b"ping".to_vec()));
                    let reply = json!({"cmd":"SET_ACTIVITY", "nonce":request["nonce"]}).to_string();
                    write_frame(&mut server, 1, reply.as_bytes()).await.unwrap();
                });
                set_activity(&mut client, None).await.unwrap();
                server_task.await.unwrap();
            });
        }

        #[test]
        fn rpc_rejects_oversized_frames_and_errors() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (mut client, mut server) = tokio::io::duplex(4096);
                server.write_all(&1_u32.to_le_bytes()).await.unwrap();
                server
                    .write_all(&(u32::try_from(MAX_FRAME).unwrap() + 1).to_le_bytes())
                    .await
                    .unwrap();
                assert!(read_frame(&mut client).await.is_err());
                let (mut client, mut server) = tokio::io::duplex(4096);
                write_frame(&mut server, 1, br#"{"evt":"ERROR"}"#)
                    .await
                    .unwrap();
                assert!(response(&mut client, None).await.is_err());
            });
        }
    }
}

#[cfg(windows)]
pub use windows::{install, settings_changed, shutdown};

#[cfg(not(windows))]
pub fn install(_: bool) {}
#[cfg(not(windows))]
pub fn settings_changed(_: bool) {}
#[cfg(not(windows))]
pub fn shutdown() {}

#[tauri::command]
pub async fn resolve_codex_pet(link: String) -> Result<String, String> {
    codexbar::discord_presence::pet_links::resolve(&link).await
}
