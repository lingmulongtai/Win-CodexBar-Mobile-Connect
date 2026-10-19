use super::super::{
    MobileHistoryPoint, MobileProvider, MobileQuotaHistory, MobileServer, MobileSpend, MobileWindow,
};
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::sync::Arc;
use tokio::net::TcpListener;

fn fixture() -> MobileSnapshot {
    let now = chrono::Utc::now().timestamp();
    MobileSnapshot {
        schema_version: 1,
        source: SOURCE.to_owned(),
        generated_at_epoch_seconds: now,
        providers: vec![MobileProvider {
            id: "codex".to_owned(),
            updated_at_epoch_seconds: now - 30,
            status: "ready".to_owned(),
            subject_id: Some(URL_SAFE_NO_PAD.encode([1_u8; 32])),
            windows: vec![MobileWindow {
                label: "5h".to_owned(),
                used_fraction: 0.25,
                resets_at_epoch_seconds: Some(now + 300),
                window_duration_seconds: Some(18_000),
            }],
            budget: None,
            balance: None,
            reset_credits: None,
            local_usage: None,
            history_scope_id: Some(URL_SAFE_NO_PAD.encode([2_u8; 32])),
            history: vec![MobileQuotaHistory {
                label: "5h".to_owned(),
                samples: vec![MobileHistoryPoint {
                    t: now - 3600,
                    u: 0.1,
                    r: None,
                }],
            }],
        }],
        spend: Some(MobileSpend {
            updated_at_epoch_seconds: now - 60,
            reporting_period: "rolling:30".to_owned(),
            time_zone: "Asia/Tokyo".to_owned(),
            cost_tracking_enabled: true,
            rows: vec![],
        }),
    }
}

#[test]
fn pairing_codes_support_private_ipv4_ipv6_and_tailscale_and_redact_the_key() {
    let pairing = Pairing::generate();
    for ip in [
        "127.0.0.1",
        "192.168.1.23",
        "100.100.10.20",
        "fd7a:115c:a1e0::1",
    ] {
        let code = pairing.code(ip.parse().unwrap(), 43825).unwrap();
        let endpoint = PeerEndpoint::from_code(&format!("\n{code}\r\n")).unwrap();
        assert_eq!(endpoint.address.ip().to_string(), ip);
        assert!(!format!("{endpoint:?}").contains(&pairing.shared_key_base64_url));
        assert!(PeerEndpoint::from_code(&format!("{code}|extra")).is_err());
        assert!(PeerEndpoint::from_code(&code.replace("|43825|", "|80|")).is_err());
    }
    let local = pairing.code("127.0.0.1".parse().unwrap(), 43825).unwrap();
    for ip in ["8.8.8.8", "0.0.0.0", "224.0.0.1", "100.128.0.1"] {
        assert!(PeerEndpoint::from_code(&local.replace("127.0.0.1", ip)).is_err());
    }
    assert!(PeerEndpoint::from_code(&"x".repeat(1025)).is_err());
}

#[test]
fn desktop_data_obeys_the_android_wire_contract() {
    let now = chrono::Utc::now().timestamp();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let mut valid = serde_json::to_value(fixture()).unwrap();
    valid["providers"][0]["localUsage"] = serde_json::json!({
        "updatedAtEpochSeconds": now - 60, "todayDate": today, "todayTokens": 42,
        "todayCostUsd": 0.2, "reportingPeriod": "rolling:30", "periodTokens": 100,
        "periodCostUsd": 1.0, "topModel": "codex-test", "incompleteRequestCount": 0,
    });
    valid["spend"]["rows"] = serde_json::json!([{
        "providerId": "codex", "currency": "USD", "refreshing": false,
        "sevenDay": 0.2, "thirtyDay": 1.0, "periodCost": 1.0,
        "sevenDayTokens": 42, "thirtyDayTokens": 100, "periodTokens": 100,
        "tokensLowerBound": false,
        "daily": [{"day": today, "tokens": 42, "costUsd": 0.2, "isScanned": true}],
    }]);
    let decoded: MobileSnapshot = serde_json::from_value(valid.clone()).unwrap();
    validate_snapshot(&decoded, now).unwrap();
    let cases = [
        ("/schemaVersion", serde_json::json!(2)),
        ("/source", serde_json::json!("some-other-protocol")),
        ("/generatedAtEpochSeconds", serde_json::json!(now - 121)),
        ("/providers/0/status", serde_json::json!("ok")),
        ("/providers/0/id", serde_json::json!("../codex")),
        ("/providers/0/updatedAtEpochSeconds", serde_json::json!(-1)),
        (
            "/providers/0/subjectId",
            serde_json::json!("opaque-account"),
        ),
        ("/providers/0/historyScopeId", serde_json::Value::Null),
        (
            "/providers/0/windows/0/label",
            serde_json::json!("control\u{1}"),
        ),
        (
            "/providers/0/windows/0/usedFraction",
            serde_json::json!(1.01),
        ),
        (
            "/providers/0/windows/0/windowDurationSeconds",
            serde_json::json!(0),
        ),
        (
            "/providers/0/history/0/label",
            serde_json::json!("no-such-window"),
        ),
        (
            "/providers/0/history/0/samples",
            serde_json::json!([
                {"t": now - 600, "u": 0.1}, {"t": now - 600, "u": 0.2},
            ]),
        ),
        (
            "/providers/0/budget",
            serde_json::json!({"used": -1, "limit": 100, "currency": "USD"}),
        ),
        (
            "/providers/0/balance",
            serde_json::json!({"amount": -1, "currency": "USD"}),
        ),
        (
            "/providers/0/localUsage/reportingPeriod",
            serde_json::json!("rolling:030"),
        ),
        (
            "/providers/0/localUsage/todayDate",
            serde_json::json!("2026-02-30"),
        ),
        (
            "/providers/0/localUsage/topModel",
            serde_json::json!("a private path/model"),
        ),
        ("/spend/reportingPeriod", serde_json::json!("rolling:366")),
        ("/spend/timeZone", serde_json::json!("unknown/zone")),
        ("/spend/costTrackingEnabled", serde_json::json!(false)),
        ("/spend/rows/0/providerId", serde_json::json!("unknown")),
        ("/spend/rows/0/currency", serde_json::json!("$USD")),
        ("/spend/rows/0/sevenDay", serde_json::json!(-1)),
        ("/spend/rows/0/periodTokens", serde_json::json!(u64::MAX)),
        ("/spend/rows/0/daily/0/day", serde_json::json!("2026-02-30")),
        ("/spend/rows/0/daily/0/isScanned", serde_json::json!(false)),
    ];
    for (pointer, value) in cases {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let accepted = serde_json::from_value::<MobileSnapshot>(invalid)
            .is_ok_and(|snapshot| validate_snapshot(&snapshot, now).is_ok());
        assert!(!accepted, "accepted invalid field {pointer}");
    }
    let mut large_history = decoded;
    large_history.providers[0].history[0].samples = (0..65)
        .map(|i| MobileHistoryPoint {
            t: now - (65 - i) * 60,
            u: 0.1,
            r: None,
        })
        .collect();
    assert!(validate_snapshot(&large_history, now).is_err());
}

#[tokio::test]
async fn multiple_desktop_receivers_read_the_existing_server_without_losing_history_or_spend() {
    let expected = fixture();
    let snapshot = expected.clone();
    let pairing = Pairing::generate();
    let server = MobileServer::start(
        "127.0.0.1:0".parse().unwrap(),
        pairing.clone(),
        Arc::new(move |_| snapshot.clone()),
    )
    .await
    .unwrap();
    let endpoint = PeerEndpoint {
        address: server.address(),
        pairing,
    };
    let (first, second) = tokio::join!(
        fetch_peer_snapshot(&endpoint),
        fetch_peer_snapshot(&endpoint)
    );
    for result in [first, second] {
        assert_eq!(
            serde_json::to_value(result.unwrap()).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }
    let wrong = PeerEndpoint {
        address: endpoint.address,
        pairing: Pairing::generate(),
    };
    assert!(fetch_peer_snapshot(&wrong).await.is_err());
    server.stop().await;
    assert!(fetch_peer_snapshot(&endpoint).await.is_err());
}

async fn scripted_peer(
    snapshot: MobileSnapshot,
    edit: fn(&mut Envelope),
) -> (PeerEndpoint, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = PeerEndpoint {
        address: listener.local_addr().unwrap(),
        pairing: Pairing::generate(),
    };
    let pairing = endpoint.pairing.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let request: Request = serde_json::from_str(&line).unwrap();
        let now = chrono::Utc::now().timestamp();
        assert!(request.authenticate(&pairing, now));
        let mut response = super::super::protocol::encrypt(
            &pairing,
            &request,
            &serde_json::to_vec(&snapshot).unwrap(),
            now,
        )
        .unwrap();
        edit(&mut response);
        let mut bytes = serde_json::to_vec(&response).unwrap();
        bytes.push(b'\n');
        stream.get_mut().write_all(&bytes).await.unwrap();
    });
    (endpoint, task)
}

#[tokio::test]
async fn rejects_tampered_stale_and_misbound_envelopes() {
    let edits: [fn(&mut Envelope); 6] = [
        |e| e.protocol_version = 2,
        |e| e.companion_id = uuid::Uuid::new_v4().to_string(),
        |e| e.request_nonce.push('x'),
        |e| e.sent_at_epoch_seconds -= 121,
        |e| e.iv = URL_SAFE_NO_PAD.encode([0_u8; 11]),
        |e| {
            let mut bytes = URL_SAFE_NO_PAD.decode(&e.ciphertext).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            e.ciphertext = URL_SAFE_NO_PAD.encode(bytes);
        },
    ];
    for edit in edits {
        let (endpoint, task) = scripted_peer(fixture(), edit).await;
        assert!(matches!(
            fetch_peer_snapshot(&endpoint).await,
            Err(PeerClientError::Authentication)
        ));
        task.await.unwrap();
    }
}

#[tokio::test]
async fn authenticated_but_invalid_usage_is_rejected() {
    let mut invalid = fixture();
    invalid.providers[0].windows[0].used_fraction = -0.1;
    let (endpoint, task) = scripted_peer(invalid, |_| {}).await;
    assert!(matches!(
        fetch_peer_snapshot(&endpoint).await,
        Err(PeerClientError::InvalidSnapshot)
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn bounds_reads_and_timeout_when_the_peer_is_not_a_healthy_server() {
    for reply in [b"{}".to_vec(), vec![b' '; RESPONSE_LIMIT + 1]] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = PeerEndpoint {
            address: listener.local_addr().unwrap(),
            pairing: Pairing::generate(),
        };
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut request = String::new();
            stream.read_line(&mut request).await.unwrap();
            let _written = stream.get_mut().write_all(&reply).await;
        });
        assert!(matches!(
            fetch_peer_snapshot(&endpoint).await,
            Err(PeerClientError::InvalidResponse)
        ));
        task.await.unwrap();
    }
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = PeerEndpoint {
        address: stalled.local_addr().unwrap(),
        pairing: Pairing::generate(),
    };
    assert!(matches!(
        fetch_with_timeout(&endpoint, Duration::from_millis(100)).await,
        Err(PeerClientError::Timeout)
    ));
}
