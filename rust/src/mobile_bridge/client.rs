//! Desktop receiver for the same authenticated, encrypted protocol used by Android.

use super::protocol::{Envelope, Request, decrypt};
use super::{MobileSnapshot, Pairing, SOURCE, is_private_address};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const RESPONSE_LIMIT: usize = 256 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

mod validation;
use validation::validate_snapshot;

/// Persist this only through the platform's protected credential store.
/// Its Debug output deliberately inherits Pairing's secret redaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerEndpoint {
    pub address: SocketAddr,
    pub pairing: Pairing,
}

#[derive(Debug, thiserror::Error)]
pub enum PeerClientError {
    #[error("Invalid connection code. Copy a fresh code from the sharing PC.")]
    InvalidPairing,
    #[error(
        "Could not reach the sharing PC. Check its sharing status and LAN or Tailscale connection."
    )]
    Connection(#[source] std::io::Error),
    #[error("The sharing PC did not respond in time.")]
    Timeout,
    #[error("The sharing PC returned an incomplete or oversized response.")]
    InvalidResponse,
    #[error(
        "The response could not be authenticated. Check the connection code and both PCs' clocks."
    )]
    Authentication,
    #[error("The sharing PC returned unsupported or invalid usage data.")]
    InvalidSnapshot,
}

impl PeerEndpoint {
    pub fn from_code(code: &str) -> Result<Self, PeerClientError> {
        if code.len() > 1024 {
            return Err(PeerClientError::InvalidPairing);
        }
        let fields = code.trim().split('|').collect::<Vec<_>>();
        if fields.len() != 5 || fields[0] != "CBWIN1" {
            return Err(PeerClientError::InvalidPairing);
        }
        let ip = fields[1]
            .parse::<IpAddr>()
            .map_err(|_| PeerClientError::InvalidPairing)?;
        let port = fields[2]
            .parse::<u16>()
            .map_err(|_| PeerClientError::InvalidPairing)?;
        let endpoint = Self {
            address: SocketAddr::new(ip, port),
            pairing: Pairing {
                companion_id: fields[3].to_owned(),
                shared_key_base64_url: fields[4].to_owned(),
            },
        };
        if !endpoint.validate() {
            return Err(PeerClientError::InvalidPairing);
        }
        Ok(endpoint)
    }

    pub fn validate(&self) -> bool {
        is_private_address(self.address.ip())
            && self.address.port() >= 1024
            && self.pairing.validate()
    }
}

pub async fn fetch_peer_snapshot(
    endpoint: &PeerEndpoint,
) -> Result<MobileSnapshot, PeerClientError> {
    fetch_with_timeout(endpoint, REQUEST_TIMEOUT).await
}

async fn fetch_with_timeout(
    endpoint: &PeerEndpoint,
    timeout: Duration,
) -> Result<MobileSnapshot, PeerClientError> {
    if !endpoint.validate() {
        return Err(PeerClientError::InvalidPairing);
    }
    tokio::time::timeout(timeout, exchange(endpoint))
        .await
        .map_err(|_| PeerClientError::Timeout)?
}

async fn exchange(endpoint: &PeerEndpoint) -> Result<MobileSnapshot, PeerClientError> {
    let request = Request::signed(&endpoint.pairing, chrono::Utc::now().timestamp())
        .ok_or(PeerClientError::InvalidPairing)?;
    let mut stream = TcpStream::connect(endpoint.address)
        .await
        .map_err(PeerClientError::Connection)?;
    let mut bytes = serde_json::to_vec(&request).map_err(|_| PeerClientError::InvalidPairing)?;
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .await
        .map_err(PeerClientError::Connection)?;

    // Bound memory even when a peer sends no newline or never closes its socket.
    let mut reader = BufReader::new(stream).take((RESPONSE_LIMIT + 1) as u64);
    let mut response = Vec::new();
    reader
        .read_until(b'\n', &mut response)
        .await
        .map_err(PeerClientError::Connection)?;
    if response.len() > RESPONSE_LIMIT || response.last() != Some(&b'\n') {
        return Err(PeerClientError::InvalidResponse);
    }
    let envelope: Envelope =
        serde_json::from_slice(&response).map_err(|_| PeerClientError::InvalidResponse)?;
    let now = chrono::Utc::now().timestamp();
    let plaintext = decrypt(&endpoint.pairing, &request, &envelope, now)
        .ok_or(PeerClientError::Authentication)?;
    let snapshot: MobileSnapshot =
        serde_json::from_slice(&plaintext).map_err(|_| PeerClientError::InvalidSnapshot)?;
    validate_snapshot(&snapshot, now)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests;
