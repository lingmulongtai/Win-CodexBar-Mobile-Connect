use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountUsage {
    pub summary: CodexAccountUsageSummary,
    pub daily_usage_buckets: Option<Vec<CodexAccountUsageDay>>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountUsageSummary {
    pub lifetime_tokens: Option<u64>,
    pub peak_daily_tokens: Option<u64>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountUsageDay {
    pub start_date: String,
    pub tokens: u64,
}

/// Read the signed-in account's official profile; never expose credentials or raw errors.
#[tauri::command]
pub async fn get_codex_account_usage() -> Result<CodexAccountUsage, String> {
    let executable = codexbar::codex_cli::locate_codex_binary()
        .ok_or_else(|| "Codex CLI is unavailable".to_string())?;
    tokio::time::timeout(Duration::from_secs(20), read_usage(executable))
        .await
        .map_err(|_| "Codex account usage timed out".to_string())?
}

async fn read_usage(executable: std::path::PathBuf) -> Result<CodexAccountUsage, String> {
    let mut command = Command::new(executable);
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| "Could not start Codex app server".to_string())?;
    let mut input = child
        .stdin
        .take()
        .ok_or("Codex app server input unavailable")?;
    let output = child
        .stdout
        .take()
        .ok_or("Codex app server output unavailable")?;
    let mut lines = BufReader::new(output).lines();
    input.write_all(b"{\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"win_codexbar\",\"title\":\"Win CodexBar\",\"version\":\"0.72.0\"}}}\n").await.map_err(|_| "Codex initialization failed")?;
    let initialized = response(&mut lines, 1).await?;
    if initialized.get("result").is_none() {
        return Err("Codex initialization failed".into());
    }
    input.write_all(b"{\"method\":\"initialized\",\"params\":{}}\n{\"id\":2,\"method\":\"account/usage/read\"}\n").await.map_err(|_| "Codex usage request failed")?;
    let value = response(&mut lines, 2).await?;
    let result = value
        .get("result")
        .ok_or("Codex account usage is unavailable")?;
    serde_json::from_value(result.clone())
        .map_err(|_| "Codex account usage response was invalid".to_string())
}

async fn response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<serde_json::Value, String> {
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|_| "Codex app server read failed")?
    {
        if line.len() > 2_000_000 {
            return Err("Codex response was too large".into());
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
            if value.get("id").and_then(|v| v.as_u64()) == Some(id) {
                return Ok(value);
            }
        }
    }
    Err("Codex app server closed unexpectedly".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_nullable_profile_without_treating_missing_data_as_zero() {
        let value = serde_json::json!({"summary":{"lifetimeTokens":12345678901_u64,"peakDailyTokens":null},"dailyUsageBuckets":null});
        let parsed: CodexAccountUsage = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.summary.lifetime_tokens, Some(12345678901));
        assert_eq!(parsed.summary.peak_daily_tokens, None);
        assert!(parsed.daily_usage_buckets.is_none());
    }
}
