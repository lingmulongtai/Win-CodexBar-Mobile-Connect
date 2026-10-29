//! Shared library surface for CodexBar.
//!
//! This keeps the current Rust implementation usable from the existing CLI/bin
//! while giving the rewrite a stable crate dependency for future shells.

#[cfg(all(feature = "test-secure-storage", not(debug_assertions)))]
compile_error!("The test-secure-storage fixture must never be enabled in release builds");

pub mod agent_sessions;
pub mod atomic_file;
pub mod browser;
pub mod cli;
pub mod codex_accounts;
pub mod codex_cli;
pub mod codex_workspaces;
pub mod core;
pub mod cost_reporting_period;
pub mod cost_scanner;
pub mod currency;
pub mod discord_presence;
pub mod host;
pub mod locale;
pub mod logging;
pub mod login;
#[cfg(windows)]
pub mod managed_process;
pub mod mobile_bridge;
pub mod notifications;
pub mod power_assertion;
pub mod process_environment;
pub mod providers;
pub mod secure_file;
pub mod settings;
pub mod sound;
pub mod spend_contract;
pub mod spend_trend;

pub mod status;
pub mod switcher_shortcuts;
pub mod tray;
pub mod updater;
pub mod wsl;

pub mod codex_costs;
mod codex_sessions;
mod pi_session_cost;
