//! Opt-in local Discord RPC. Public activity may include service, model,
//! elapsed time and token counts; conversation content and paths stay local.
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

    const APPLICATION_ID: &str = "1564892477213638746";
    const INTERVAL: Duration = Duration::from_secs(20);
    const IO_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_FRAME: usize = 64 * 1024;
    const USAGE_INTERVAL: Duration = Duration::from_secs(300);
    static SETTINGS: OnceLock<watch::Sender<bool>> = OnceLock::new();

    pub fn install(enabled: bool) {
        let (sender, receiver) = watch::channel(enabled);
        if SETTINGS.set(sender).is_ok() {
            tauri::async_runtime::spawn(run(receiver));
        }
    }

    pub fn settings_changed(enabled: bool) {
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

    async fn run(mut enabled: watch::Receiver<bool>) {
        let mut stream: Option<NamedPipeClient> = None;
        let mut last_provider = None;
        let mut usage = (None, None);
        let mut usage_task: Option<tokio::task::JoinHandle<_>> = None;
        let mut usage_refreshed: Option<Instant> = None;
        let mut usage_day = None;
        let mut usage_task_valid = false;
        let metadata_slot = Arc::new(tokio::sync::Semaphore::new(1));
        loop {
            if !*enabled.borrow_and_update() {
                usage_task_valid = false;
                usage = (None, None);
                usage_refreshed = None;
                usage_day = None;
                if let Some(mut current) = stream.take() {
                    let _result = set_activity(&mut current, None).await;
                }
                if enabled.changed().await.is_err() {
                    break;
                }
                continue;
            }
            let scanner = LocalAgentSessionScanner::default();
            let scan = tokio::select! {
                biased;
                changed = enabled.changed() => { if changed.is_err() { break; } continue; },
                result = scanner.scan_for_presence() => result,
            };
            let now = chrono::Utc::now();
            let tokens_enabled = codexbar::settings::Settings::load().cost_usage_enabled;
            let today = codexbar::cost_reporting_period::cost_bucket_zone().date(now);
            if usage_day.is_some_and(|day| day != today) {
                usage_task_valid = false;
                usage = (None, None);
                usage_refreshed = None;
            }
            usage_day = Some(today);
            if !tokens_enabled {
                usage_task_valid = false;
                usage = (None, None);
                usage_refreshed = None;
            } else {
                if usage_task.as_ref().is_some_and(|task| task.is_finished()) {
                    if let Some(task) = usage_task.take() {
                        let completed = task.await.unwrap_or((None, None));
                        if usage_task_valid {
                            usage = completed;
                            usage_refreshed = Some(Instant::now());
                        }
                    }
                }
                if usage_task.is_none()
                    && usage_refreshed.is_none_or(|time| time.elapsed() >= USAGE_INTERVAL)
                {
                    usage_task = Some(tokio::task::spawn_blocking(move || {
                        use codexbar::agent_sessions::AgentSessionProvider::{Claude, Codex};
                        (
                            codexbar::discord_presence::usage_periods(Codex, now),
                            codexbar::discord_presence::usage_periods(Claude, now),
                        )
                    }));
                    usage_task_valid = true;
                }
            }
            let provider = codexbar::discord_presence::current_provider(&scan.sessions, now);
            if provider.is_some() {
                last_provider = provider;
            }
            use codexbar::agent_sessions::AgentSessionProvider::{Claude, Codex};
            // On a fresh start with only one usable history, identify it
            // instead of silently hiding its calendar totals.
            let selected = provider.or(last_provider).or_else(|| match usage {
                (Some(_), None) => Some(Codex),
                (None, Some(_)) => Some(Claude),
                _ => None,
            });
            let periods = match selected {
                Some(Codex) => usage.0,
                Some(Claude) => usage.1,
                None => usage.0.zip(usage.1).and_then(|(a, b)| {
                    Some(codexbar::discord_presence::UsagePeriods {
                        today: a.today.checked_add(b.today)?,
                        week: a.week.checked_add(b.week)?,
                        month: a.month.checked_add(b.month)?,
                    })
                }),
                _ => None,
            };
            let activity = if let Ok(permit) = metadata_slot.clone().try_acquire_owned() {
                tokio::select! {
                    biased;
                    changed = enabled.changed() => { if changed.is_err() { break; } continue; },
                    result = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        codexbar::discord_presence::activity_with_usage(&scan.sessions, now, tokens_enabled, periods)
                    }) => result.ok().flatten(),
                }
            } else { None }.unwrap_or_else(|| codexbar::discord_presence::idle_activity_with_usage(selected, periods));
            if stream.is_none() {
                tokio::select! {
                    biased;
                    changed = enabled.changed() => { if changed.is_err() { break; } continue; },
                    result = connect() => { stream = result.ok(); }
                }
            }
            if let Some(current) = stream.as_mut() {
                let result = tokio::select! {
                    biased;
                    changed = enabled.changed() => { if changed.is_err() { break; } None },
                    result = set_activity(current, Some(activity)) => Some(result),
                };
                if result.is_none() || result.is_some_and(|result| result.is_err()) {
                    stream = None;
                }
            }
            tokio::select! {
                changed = enabled.changed() => if changed.is_err() { break; },
                () = tokio::time::sleep(INTERVAL) => {},
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

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
