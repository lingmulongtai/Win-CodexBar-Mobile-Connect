use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, OsRng, Payload, rand_core::RngCore},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::IpAddr;

pub(super) const AUTH_CONTEXT: &[u8] = b"codexbar-windows-auth-v1";
const ENCRYPTION_CONTEXT: &[u8] = b"codexbar-windows-encryption-v1";
const RESOURCE: &str = "snapshot";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pairing {
    pub companion_id: String,
    pub shared_key_base64_url: String,
}

impl Pairing {
    pub fn generate() -> Self {
        let mut key = [0_u8; 32];
        OsRng.fill_bytes(&mut key);
        Self {
            companion_id: uuid::Uuid::new_v4().to_string(),
            shared_key_base64_url: URL_SAFE_NO_PAD.encode(key),
        }
    }

    pub fn validate(&self) -> bool {
        uuid::Uuid::parse_str(&self.companion_id).is_ok() && self.master_key().is_some()
    }

    pub fn code(&self, address: IpAddr, port: u16) -> Option<String> {
        (self.validate() && is_private_address(address) && port >= 1024).then(|| {
            format!(
                "CBWIN1|{address}|{port}|{}|{}",
                self.companion_id, self.shared_key_base64_url
            )
        })
    }

    pub(super) fn master_key(&self) -> Option<[u8; 32]> {
        URL_SAFE_NO_PAD
            .decode(&self.shared_key_base64_url)
            .ok()?
            .try_into()
            .ok()
    }
}

impl std::fmt::Debug for Pairing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Pairing(<redacted>)")
    }
}

pub fn is_private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

/// Only an opaque digest is retained alongside the desktop cache.
pub fn account_scope(provider: &str, identity: &str) -> [u8; 32] {
    Sha256::digest(format!("codexbar-windows-subject-v1\n{provider}\n{identity}").as_bytes()).into()
}

pub fn account_subject(pairing: &Pairing, scope: &[u8; 32]) -> Option<String> {
    let key = pairing.master_key()?;
    Some(URL_SAFE_NO_PAD.encode(hmac_sha256(&key, scope)))
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    pub protocol_version: u8,
    pub companion_id: String,
    pub requested_at_epoch_seconds: i64,
    pub nonce: String,
    pub signature: String,
}

impl Request {
    pub(super) fn signed(pairing: &Pairing, now: i64) -> Option<Self> {
        let mut nonce = [0_u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let nonce = URL_SAFE_NO_PAD.encode(nonce);
        let canonical = format!("1\n{}\n{now}\n{nonce}\n{RESOURCE}", pairing.companion_id);
        let key = hmac_sha256(&pairing.master_key()?, AUTH_CONTEXT);
        Some(Self {
            protocol_version: 1,
            companion_id: pairing.companion_id.clone(),
            requested_at_epoch_seconds: now,
            nonce,
            signature: URL_SAFE_NO_PAD.encode(hmac_sha256(&key, canonical.as_bytes())),
        })
    }

    pub(super) fn authenticate(&self, pairing: &Pairing, now: i64) -> bool {
        if self.protocol_version != 1
            || self.companion_id != pairing.companion_id
            || self.requested_at_epoch_seconds.abs_diff(now) > 120
            || URL_SAFE_NO_PAD.decode(&self.nonce).ok().map(|n| n.len()) != Some(16)
        {
            return false;
        }
        let Some(master_key) = pairing.master_key() else {
            return false;
        };
        let canonical = format!(
            "1\n{}\n{}\n{}\n{RESOURCE}",
            self.companion_id, self.requested_at_epoch_seconds, self.nonce
        );
        let key = hmac_sha256(&master_key, AUTH_CONTEXT);
        let expected = hmac_sha256(&key, canonical.as_bytes());
        let Ok(actual) = URL_SAFE_NO_PAD.decode(&self.signature) else {
            return false;
        };
        constant_time_eq(&actual, &expected)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Envelope {
    pub protocol_version: u8,
    pub companion_id: String,
    pub request_nonce: String,
    pub sent_at_epoch_seconds: i64,
    pub iv: String,
    pub ciphertext: String,
}

pub(super) fn encrypt(
    pairing: &Pairing,
    request: &Request,
    plaintext: &[u8],
    now: i64,
) -> Option<Envelope> {
    let key = hmac_sha256(&pairing.master_key()?, ENCRYPTION_CONTEXT);
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let mut iv = [0_u8; 12];
    OsRng.fill_bytes(&mut iv);
    let aad = format!(
        "1\n{}\n{}\n{now}\n{RESOURCE}",
        pairing.companion_id, request.nonce
    );
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .ok()?;
    Some(Envelope {
        protocol_version: 1,
        companion_id: pairing.companion_id.clone(),
        request_nonce: request.nonce.clone(),
        sent_at_epoch_seconds: now,
        iv: URL_SAFE_NO_PAD.encode(iv),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

pub(super) fn decrypt(
    pairing: &Pairing,
    request: &Request,
    envelope: &Envelope,
    now: i64,
) -> Option<Vec<u8>> {
    if envelope.protocol_version != 1
        || envelope.companion_id != pairing.companion_id
        || envelope.request_nonce != request.nonce
        || envelope.sent_at_epoch_seconds.abs_diff(now) > 120
    {
        return None;
    }
    let key = hmac_sha256(&pairing.master_key()?, ENCRYPTION_CONTEXT);
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let iv: [u8; 12] = URL_SAFE_NO_PAD.decode(&envelope.iv).ok()?.try_into().ok()?;
    let ciphertext = URL_SAFE_NO_PAD.decode(&envelope.ciphertext).ok()?;
    let aad = format!(
        "1\n{}\n{}\n{}\n{RESOURCE}",
        pairing.companion_id, request.nonce, envelope.sent_at_epoch_seconds
    );
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .ok()?;
    (plaintext.len() <= 128 * 1024).then_some(plaintext)
}

// RFC 2104 HMAC using the existing SHA-256 dependency. Keys longer than the
// hash block are hashed first; pairing and derived keys are exactly 32 bytes.
pub(super) fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|byte| byte ^ 0x36));
    inner.update(message);
    let mut outer = Sha256::new();
    outer.update(block.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231() {
        let actual = hmac_sha256(&[0x0b; 20], b"Hi There");
        let hex = actual
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            hex,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        let long = hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        let hex = long
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            hex,
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn request_matches_the_android_and_node_interop_vector() {
        let pairing = Pairing {
            companion_id: "5b017391-6dc4-4ab7-b0ad-2255dada62d7".into(),
            shared_key_base64_url: URL_SAFE_NO_PAD.encode(std::array::from_fn::<_, 32, _>(
                |index| u8::try_from(index).unwrap(),
            )),
        };
        let request = Request {
            protocol_version: 1,
            companion_id: pairing.companion_id.clone(),
            requested_at_epoch_seconds: 1700000000,
            nonce: URL_SAFE_NO_PAD.encode([1_u8; 16]),
            signature: "EcBHhL1ayalXOhEJMO2C1tWFeqcBHsdjugdzUJBNVdQ".into(),
        };
        assert!(request.authenticate(&pairing, 1700000000));
        assert!(!request.authenticate(&pairing, 1700000121));
    }

    #[test]
    fn pairing_and_private_addresses_are_bounded() {
        let pairing = Pairing::generate();
        assert!(pairing.validate());
        assert!(!format!("{pairing:?}").contains(&pairing.shared_key_base64_url));
        for address in [
            "127.0.0.1",
            "192.168.1.3",
            "10.2.3.4",
            "100.74.102.71",
            "::1",
            "fd00::1",
        ] {
            assert!(pairing.code(address.parse().unwrap(), 43825).is_some());
        }
        for address in [
            "0.0.0.0",
            "8.8.8.8",
            "100.128.0.1",
            "::",
            "2001:4860:4860::8888",
        ] {
            assert!(pairing.code(address.parse().unwrap(), 43825).is_none());
        }
    }

    #[test]
    fn subjects_are_provider_and_pairing_scoped() {
        let pairing = Pairing::generate();
        let scope = account_scope("codex", "account-one");
        assert_ne!(scope, account_scope("claude", "account-one"));
        assert_ne!(
            account_subject(&pairing, &scope),
            account_subject(&Pairing::generate(), &scope)
        );
    }

    #[test]
    fn ciphertext_is_authenticated_and_binds_the_request_and_time() {
        let pairing = Pairing::generate();
        let request = Request {
            protocol_version: 1,
            companion_id: pairing.companion_id.clone(),
            requested_at_epoch_seconds: 1700000000,
            nonce: URL_SAFE_NO_PAD.encode([1_u8; 16]),
            signature: String::new(),
        };
        let envelope = encrypt(&pairing, &request, b"private usage", 1700000000).unwrap();
        let key = hmac_sha256(&pairing.master_key().unwrap(), ENCRYPTION_CONTEXT);
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let iv = URL_SAFE_NO_PAD.decode(envelope.iv).unwrap();
        let mut encrypted = URL_SAFE_NO_PAD.decode(envelope.ciphertext).unwrap();
        let aad = format!(
            "1\n{}\n{}\n1700000000\nsnapshot",
            pairing.companion_id, request.nonce
        );
        assert_eq!(
            cipher
                .decrypt(
                    Nonce::from_slice(&iv),
                    Payload {
                        msg: &encrypted,
                        aad: aad.as_bytes()
                    }
                )
                .unwrap(),
            b"private usage"
        );
        assert!(
            cipher
                .decrypt(
                    Nonce::from_slice(&iv),
                    Payload {
                        msg: &encrypted,
                        aad: b"wrong request"
                    }
                )
                .is_err()
        );
        encrypted[0] ^= 1;
        assert!(
            cipher
                .decrypt(
                    Nonce::from_slice(&iv),
                    Payload {
                        msg: &encrypted,
                        aad: aad.as_bytes()
                    }
                )
                .is_err()
        );
    }
}
