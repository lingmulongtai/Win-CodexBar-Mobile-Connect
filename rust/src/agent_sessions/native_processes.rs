//! Native Windows process snapshot for the Stay Awake scan.
//!
//! The Agent Sessions list shells out to PowerShell (`Get-CimInstance
//! Win32_Process`), which is too heavy to run every 30 seconds. Stay Awake only
//! needs to know whether a live agent process exists, so it reads the
//! ToolHelp snapshot instead. Records go through the same classifier as the
//! CIM path; like that path, no command lines are collected.

use super::*;

impl LocalAgentSessionScanner {
    /// Whether at least one live local agent process (Codex, Claude, Pi/OMP)
    /// exists right now. Idle processes waiting for a prompt count. File-only
    /// rollouts and remote hosts are never consulted, so they cannot count.
    pub fn has_live_agent_process() -> Result<bool, String> {
        Ok(Self::holds_system_awake(&snapshot_processes()?))
    }

    /// Pure predicate behind [`Self::has_live_agent_process`].
    pub(super) fn holds_system_awake(processes: &[AgentProcessRecord]) -> bool {
        processes
            .iter()
            .any(|process| process.pid > 0 && process.is_agent())
    }
}

#[cfg(windows)]
fn snapshot_processes() -> Result<Vec<AgentProcessRecord>, String> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: a successful call returns a unique snapshot handle owned below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|error| format!("process snapshot failed: {error}"))?;
    // SAFETY: `snapshot` is a valid, unique handle returned just above; the
    // `OwnedHandle` closes it when dropped.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let handle = HANDLE(snapshot.as_raw_handle());

    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>())
            .map_err(|error| format!("invalid process entry size: {error}"))?,
        ..Default::default()
    };
    let mut records = Vec::new();
    // SAFETY: the snapshot handle is live and `entry.dwSize` is correct.
    match unsafe { Process32FirstW(handle, &mut entry) } {
        Ok(()) => {}
        Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => return Ok(records),
        Err(error) => return Err(format!("process enumeration failed: {error}")),
    }
    loop {
        let len = entry
            .szExeFile
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
        records.push(WindowsProcessOutputParser::record(
            entry.th32ProcessID,
            entry.th32ParentProcessID,
            None,
            Some(name),
            None,
        ));
        // SAFETY: `entry` is valid for the next snapshot entry.
        match unsafe { Process32NextW(handle, &mut entry) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_NO_MORE_FILES.to_hresult() => break,
            Err(error) => return Err(format!("process enumeration failed: {error}")),
        }
    }
    Ok(records)
}

/// Recover presence detection when the more detailed CIM query fails. Process
/// creation times are needed to identify a long-running open Codex turn.
#[cfg(windows)]
pub(super) fn snapshot_for_presence() -> Result<Vec<AgentProcessRecord>, String> {
    let mut processes = snapshot_processes()?;
    for process in &mut processes {
        if process.provider == Some(AgentSessionProvider::Codex) && process.pid > 0 {
            process.started_at = process_start_time(process.pid);
        }
    }
    Ok(processes)
}

#[cfg(not(windows))]
pub(super) fn snapshot_for_presence() -> Result<Vec<AgentProcessRecord>, String> {
    Err("native process snapshots are only available on Windows".to_string())
}

#[cfg(windows)]
fn process_start_time(pid: u32) -> Option<DateTime<Utc>> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::{FILETIME, HANDLE};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: OpenProcess returns a new handle owned and closed below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    // SAFETY: the successful OpenProcess handle is valid and uniquely owned.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the handle is live and each output points to an initialized FILETIME.
    unsafe {
        GetProcessTimes(
            HANDLE(handle.as_raw_handle()),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    }
    .ok()?;
    filetime_to_utc((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}

#[cfg(any(windows, test))]
fn filetime_to_utc(ticks: u64) -> Option<DateTime<Utc>> {
    let unix_ticks = ticks.checked_sub(116_444_736_000_000_000)?;
    let seconds = i64::try_from(unix_ticks / 10_000_000).ok()?;
    let nanos = u32::try_from((unix_ticks % 10_000_000) * 100).ok()?;
    DateTime::from_timestamp(seconds, nanos)
}

#[cfg(not(windows))]
fn snapshot_processes() -> Result<Vec<AgentProcessRecord>, String> {
    Err("native process snapshots are only available on Windows".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32, name: &str) -> AgentProcessRecord {
        WindowsProcessOutputParser::record(pid, 1, None, Some(name.to_string()), None)
    }

    #[test]
    fn agent_processes_hold_the_system_awake() {
        for name in ["claude.exe", "codex.exe"] {
            let processes = [record(10, "explorer.exe"), record(11, name)];
            assert!(
                LocalAgentSessionScanner::holds_system_awake(&processes),
                "{name} should count"
            );
        }
    }

    #[test]
    fn helpers_and_unrelated_processes_do_not_hold_the_system_awake() {
        let processes = [
            record(10, "explorer.exe"),
            record(11, "Codex (Renderer)"),
            record(12, "codex app-server"),
            record(13, "node.exe"),
        ];
        assert!(!LocalAgentSessionScanner::holds_system_awake(&processes));
        assert!(!LocalAgentSessionScanner::holds_system_awake(&[]));
    }

    #[test]
    fn pid_zero_never_counts() {
        let processes = [record(0, "claude.exe")];
        assert!(!LocalAgentSessionScanner::holds_system_awake(&processes));
    }

    #[cfg(windows)]
    #[test]
    fn native_snapshot_lists_the_current_process() {
        let processes = snapshot_processes().expect("snapshot");
        assert!(processes.iter().any(|p| p.pid == std::process::id()));
        let start = process_start_time(std::process::id()).expect("current process creation time");
        assert!(start <= Utc::now());
    }

    #[test]
    fn filetime_conversion_uses_the_windows_epoch_and_100ns_ticks() {
        let epoch = 116_444_736_000_000_000;
        assert_eq!(filetime_to_utc(epoch).unwrap().timestamp(), 0);
        assert_eq!(
            filetime_to_utc(epoch + 10_000_001)
                .unwrap()
                .timestamp_subsec_nanos(),
            100
        );
        assert!(filetime_to_utc(epoch - 1).is_none());
    }
}
