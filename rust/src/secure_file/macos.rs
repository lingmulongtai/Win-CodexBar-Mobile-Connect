//! AES-GCM files with a master key stored only in the macOS login Keychain.
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, OsRng, Payload, rand_core::RngCore},
};
use std::io;

const AAD: &[u8] = b"codexbar.secure-file\n1\nmacos-keychain-aes256-gcm";

pub(super) fn encrypt(plain: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| io::Error::other("Invalid secure storage key"))?;
    let mut iv = [0_u8; 12];
    OsRng.fill_bytes(&mut iv);
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: plain,
                aad: AAD,
            },
        )
        .map_err(|_| io::Error::other("Secure file encryption failed"))?;
    Ok([iv.as_slice(), &encrypted].concat())
}

pub(super) fn decrypt(encrypted: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>> {
    if encrypted.len() < 28 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Truncated secure file",
        ));
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| io::Error::other("Invalid secure storage key"))?;
    cipher
        .decrypt(
            Nonce::from_slice(&encrypted[..12]),
            Payload {
                msg: &encrypted[12..],
                aad: AAD,
            },
        )
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Secure file authentication failed",
            )
        })
}

// Kept separate from OS access so unit tests exercise the missing-key policy
// without changing the user's Keychain or the global keyring backend.
#[cfg(any(test, not(feature = "test-secure-storage")))]
fn resolve_key(
    existing: Result<Vec<u8>, keyring::Error>,
    may_create: bool,
    initialized: bool,
    save: impl FnOnce(&[u8]) -> Result<(), keyring::Error>,
) -> io::Result<[u8; 32]> {
    match existing {
        Ok(key) => key.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "The secure storage key is invalid",
            )
        }),
        Err(keyring::Error::NoEntry) if may_create && !initialized => {
            let mut key = [0_u8; 32];
            OsRng.fill_bytes(&mut key);
            save(&key).map_err(|_| {
                io::Error::other("Cannot save the secure storage key in macOS Keychain")
            })?;
            Ok(key)
        }
        Err(_) => Err(io::Error::other(
            "Cannot read the secure storage key from macOS Keychain. Existing files have been preserved.",
        )),
    }
}

#[cfg(all(target_os = "macos", not(any(test, feature = "test-secure-storage"))))]
fn entry() -> io::Result<keyring::Entry> {
    keyring::Entry::new(
        "com.lingmulongtai.codexbar.mac.secure-files",
        "master-key-v1",
    )
    .map_err(|_| io::Error::other("macOS Keychain is unavailable"))
}

#[cfg(all(target_os = "macos", not(any(test, feature = "test-secure-storage"))))]
pub(super) fn read_key() -> io::Result<[u8; 32]> {
    // Reading an existing encrypted file must also work when the config
    // directory is read-only. Only the write path initializes the store.
    resolve_key(entry()?.get_secret(), false, true, |_| {
        Err(keyring::Error::NoEntry)
    })
}

#[cfg(all(target_os = "macos", not(any(test, feature = "test-secure-storage"))))]
pub(super) fn master_key(may_create: bool) -> io::Result<[u8; 32]> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, sync::Mutex};
    static KEY_LOCK: Mutex<()> = Mutex::new(());
    let _guard = KEY_LOCK
        .lock()
        .map_err(|_| io::Error::other("Secure storage is unavailable"))?;
    let root = crate::logging::config_root()
        .ok_or_else(|| io::Error::other("Cannot locate secure storage"))?;
    std::fs::create_dir_all(&root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join(".secure-storage.lock"))?;
    // Serialize first use across the desktop and CLI. Dropping the File unlocks
    // it even after an error/crash; an existing master key is never replaced.
    lock.lock()?;
    let marker = root.join(".secure-storage-initialized");
    let entry = entry()?;
    let key = resolve_key(entry.get_secret(), may_create, marker.exists(), |key| {
        entry.set_secret(key)
    })?;
    if !marker.exists() {
        crate::atomic_file::write_atomic_private(&marker, b"1").map_err(io::Error::other)?;
    }
    Ok(key)
}

#[cfg(all(target_os = "macos", any(test, feature = "test-secure-storage")))]
pub(super) fn read_key() -> io::Result<[u8; 32]> {
    master_key(false)
}

#[cfg(all(target_os = "macos", any(test, feature = "test-secure-storage")))]
pub(super) fn master_key(_may_create: bool) -> io::Result<[u8; 32]> {
    // Includes dependent shell unit tests, where this library lacks cfg(test).
    // Normal builds and the native integration probe still exercise Keychain.
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    Ok(*KEY.get_or_init(|| {
        let mut key = [0_u8; 32];
        OsRng.fill_bytes(&mut key);
        key
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ciphertext_is_random_and_rejects_tampering_wrong_keys_and_truncation() {
        let first = encrypt(b"synthetic secret", &[7; 32]).unwrap();
        let second = encrypt(b"synthetic secret", &[7; 32]).unwrap();
        assert_ne!(first, second);
        assert_eq!(decrypt(&first, &[7; 32]).unwrap(), b"synthetic secret");
        assert!(decrypt(&first, &[8; 32]).is_err());
        for length in [0, 11, 12, 27] {
            assert!(decrypt(&first[..length], &[7; 32]).is_err());
        }
        let mut tampered = first;
        tampered[12] ^= 1;
        assert!(decrypt(&tampered, &[7; 32]).is_err());
    }
    #[test]
    fn missing_or_invalid_key_never_replaces_an_initialized_key() {
        for (may_create, initialized) in [(false, false), (false, true), (true, true)] {
            assert!(
                resolve_key(
                    Err(keyring::Error::NoEntry),
                    may_create,
                    initialized,
                    |_| panic!("Must not replace a missing key")
                )
                .is_err()
            );
        }
        assert!(
            resolve_key(Ok(vec![1; 31]), true, false, |_| panic!(
                "Must not replace an invalid key"
            ))
            .is_err()
        );
        let key = resolve_key(Ok(vec![3; 32]), true, true, |_| {
            panic!("Must not rotate an existing key")
        })
        .unwrap();
        assert_eq!(key, [3; 32]);
        assert!(
            resolve_key(Err(keyring::Error::NoEntry), true, false, |_| Err(
                keyring::Error::NoEntry
            ))
            .is_err()
        );
        assert!(resolve_key(Err(keyring::Error::NoEntry), true, false, |_| Ok(())).is_ok());
    }
}
