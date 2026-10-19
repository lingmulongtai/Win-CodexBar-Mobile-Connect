//! Opt-in encrypted snapshots for the Android companion. Provider credentials
//! and raw provider responses are deliberately absent from this wire model.

mod client;
mod connections;
mod control;
mod history;
mod protocol;
mod server;
pub use client::{PeerClientError, PeerEndpoint, fetch_peer_snapshot};
pub use connections::{PeerConnection, PeerConnections};
pub use history::{quota_history, record_history};

pub use protocol::{Pairing, account_scope, account_subject, is_private_address};
use serde::{Deserialize, Serialize};
pub use server::{ControlSource, MobileServer, SnapshotSource};

pub const DEFAULT_PORT: u16 = 43825;
pub const SOURCE: &str = "win-codexbar";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileSnapshot {
    pub schema_version: u8,
    pub source: String,
    pub generated_at_epoch_seconds: i64,
    pub providers: Vec<MobileProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<MobileSpend>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileProvider {
    pub id: String,
    pub updated_at_epoch_seconds: i64,
    pub status: String,
    pub subject_id: Option<String>,
    pub windows: Vec<MobileWindow>,
    pub budget: Option<MobileBudget>,
    pub balance: Option<MobileBalance>,
    pub reset_credits: Option<MobileResetCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_usage: Option<MobileLocalUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_scope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<MobileQuotaHistory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileQuotaHistory {
    pub label: String,
    pub samples: Vec<MobileHistoryPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileHistoryPoint {
    pub t: i64,
    pub u: f64,
    pub r: Option<i64>,
}

/// Only aggregate data; project names, paths and conversations never enter this model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileSpend {
    pub updated_at_epoch_seconds: i64,
    pub reporting_period: String,
    pub time_zone: String,
    pub cost_tracking_enabled: bool,
    pub rows: Vec<MobileSpendRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileSpendRow {
    pub provider_id: String,
    pub currency: String,
    pub refreshing: bool,
    pub seven_day: Option<f64>,
    pub thirty_day: Option<f64>,
    pub period_cost: Option<f64>,
    pub seven_day_tokens: Option<u64>,
    pub thirty_day_tokens: Option<u64>,
    pub period_tokens: Option<u64>,
    pub tokens_lower_bound: bool,
    pub daily: Vec<MobileSpendDay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileSpendDay {
    pub day: String,
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub is_scanned: bool,
}

/// Provider-wide PC log totals; independent from the signed-in quota account.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileLocalUsage {
    pub updated_at_epoch_seconds: i64,
    pub today_date: String,
    pub today_tokens: Option<u64>,
    pub thirty_day_tokens: Option<u64>,
    pub period_tokens: Option<u64>,
    pub today_cost_usd: Option<f64>,
    pub thirty_day_cost_usd: Option<f64>,
    pub period_cost_usd: Option<f64>,
    pub reporting_period: Option<String>,
    pub top_model: Option<String>,
    pub incomplete_request_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileWindow {
    pub label: String,
    pub used_fraction: f64,
    pub resets_at_epoch_seconds: Option<i64>,
    pub window_duration_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileBudget {
    pub used: f64,
    pub limit: f64,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileBalance {
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileResetCredits {
    pub available_count: u32,
    pub expires_at_epoch_seconds: Vec<i64>,
}
