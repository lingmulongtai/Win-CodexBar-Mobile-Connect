//! Opt-in local Discord RPC. No provider data or conversation content leaves
//! this process: the payload contains a fixed provider label and public icon.
#[cfg(windows)]
mod windows {
    use std::io;
    use std::sync::OnceLock;
    use std::time::Duration;

    use codexbar::agent_sessions::LocalAgentSessionScanner;
    use serde_json::{Value, json};
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use tokio::sync::watch;

    const APPLICATION_ID: &str = "[REMOVED]";
    const INTERVAL: Duration = Duration::from_secs(20);
    const IO_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_FRAME: usize = 64 * 1024;
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
        loop {
            if !*enabled.borrow_and_update() {
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
            let activity = codexbar::discord_presence::activity_for_sessions(
                &scan.sessions,
                chrono::Utc::now(),
            );
            if activity.is_none() {
                if let Some(mut current) = stream.take() {
                    let _result = set_activity(&mut current, None).await;
                }
            } else {
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
                        result = set_activity(current, activity) => Some(result),
                    };
                    if result.is_none() || result.is_some_and(|result| result.is_err()) {
                        stream = None;
                    }
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
