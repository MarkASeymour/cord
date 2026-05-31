//! Passphrase based encryption for cord secrets at rest.
//!
//! Step 7 uses this to seal the on disk message queue. The user picks a
//! passphrase; an Argon2id key is derived from it and used to wrap a random
//! 32 byte master key, which is what actually seals the queue files. See the
//! threat model spec for why the queue is sealed with a passphrase rather than
//! a key derived from the identity.
//!
//! On disk the wrapped master key lives at `<config_dir>/queue.key`:
//!
//! ```text
//! magic "cordvk01" (8) | salt (16) | nonce (24) | sealed master key (48)
//! ```
//!
//! Each queue file is `nonce (24) | ciphertext` sealed under the master key.

use std::path::{Path, PathBuf};

use argon2::Argon2;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use rand::rngs::OsRng;
use rand::RngCore;
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"cordvk01";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 16;
const SEALED_KEY_LEN: usize = KEY_LEN + TAG_LEN;
const VAULT_FILE_LEN: usize = MAGIC.len() + SALT_LEN + NONCE_LEN + SEALED_KEY_LEN;

/// File name of the wrapped queue master key inside the config directory.
pub const VAULT_FILE: &str = "queue.key";

/// Path to the wrapped queue master key for a given config directory.
pub fn vault_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(VAULT_FILE)
}

#[derive(Debug)]
pub enum VaultError {
    WrongPassphrase,
    Corrupt(String),
    Kdf(String),
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::WrongPassphrase => write!(f, "wrong passphrase"),
            VaultError::Corrupt(m) => write!(f, "vault corrupt: {m}"),
            VaultError::Kdf(m) => write!(f, "key derivation failed: {m}"),
        }
    }
}

impl std::error::Error for VaultError {}

/// An unlocked vault holding the master key in memory. The key is zeroized on
/// drop.
pub struct Vault {
    master: Zeroizing<[u8; KEY_LEN]>,
}

impl Vault {
    /// Create a brand new vault from a passphrase. Returns the unlocked vault
    /// plus the bytes to persist at `<config_dir>/queue.key`.
    pub fn create(passphrase: &str) -> Result<(Self, Vec<u8>), VaultError> {
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);

        let mut master = Zeroizing::new([0u8; KEY_LEN]);
        OsRng.fill_bytes(master.as_mut_slice());

        let kek = derive_kek(passphrase, &salt)?;
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let sealed = aead_seal(kek.as_slice(), &nonce, master.as_slice())
            .ok_or_else(|| VaultError::Corrupt("seal of master key failed".into()))?;

        let mut file = Vec::with_capacity(VAULT_FILE_LEN);
        file.extend_from_slice(MAGIC);
        file.extend_from_slice(&salt);
        file.extend_from_slice(&nonce);
        file.extend_from_slice(&sealed);

        Ok((Vault { master }, file))
    }

    /// Unlock an existing vault from its persisted bytes and a passphrase. A
    /// wrong passphrase surfaces as `WrongPassphrase` because the wrapped master
    /// key fails authentication.
    pub fn unlock(passphrase: &str, file: &[u8]) -> Result<Self, VaultError> {
        if file.len() != VAULT_FILE_LEN {
            return Err(VaultError::Corrupt(format!(
                "expected {VAULT_FILE_LEN} bytes, found {}",
                file.len()
            )));
        }
        if &file[..MAGIC.len()] != MAGIC {
            return Err(VaultError::Corrupt("bad magic".into()));
        }
        let salt_start = MAGIC.len();
        let nonce_start = salt_start + SALT_LEN;
        let sealed_start = nonce_start + NONCE_LEN;
        let salt = &file[salt_start..nonce_start];
        let nonce = &file[nonce_start..sealed_start];
        let sealed = &file[sealed_start..];

        let kek = derive_kek(passphrase, salt)?;
        let opened = aead_open(kek.as_slice(), nonce, sealed).ok_or(VaultError::WrongPassphrase)?;
        if opened.len() != KEY_LEN {
            return Err(VaultError::Corrupt("master key length wrong".into()));
        }
        let mut master = Zeroizing::new([0u8; KEY_LEN]);
        master.copy_from_slice(&opened);
        Ok(Vault { master })
    }

    /// Seal arbitrary bytes under the master key. Output is `nonce | ciphertext`.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let ct = aead_seal(self.master.as_slice(), &nonce, plaintext)
            .ok_or_else(|| VaultError::Corrupt("seal failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open bytes produced by `seal`. A failure here means tampering or the
    /// wrong vault, not a wrong passphrase (the passphrase was already checked
    /// at unlock time).
    pub fn open(&self, blob: &[u8]) -> Result<Vec<u8>, VaultError> {
        if blob.len() < NONCE_LEN {
            return Err(VaultError::Corrupt("sealed blob shorter than its nonce".into()));
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        aead_open(self.master.as_slice(), nonce, ct)
            .ok_or_else(|| VaultError::Corrupt("open failed (tampered or wrong key)".into()))
    }
}

fn derive_kek(passphrase: &str, salt: &[u8]) -> Result<Zeroizing<[u8; KEY_LEN]>, VaultError> {
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut_slice())
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    Ok(key)
}

fn aead_seal(key: &[u8], nonce: &[u8], plaintext: &[u8]) -> Option<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).ok()?;
    cipher.encrypt(XNonce::from_slice(nonce), plaintext).ok()
}

fn aead_open(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new_from_slice(key).ok()?;
    cipher.decrypt(XNonce::from_slice(nonce), ciphertext).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_unlock_recovers_master_key() {
        let (vault, file) = Vault::create("correct horse battery staple").unwrap();
        assert_eq!(file.len(), VAULT_FILE_LEN);
        let sealed = vault.seal(b"undelivered message").unwrap();

        let reopened = Vault::unlock("correct horse battery staple", &file).unwrap();
        assert_eq!(reopened.open(&sealed).unwrap(), b"undelivered message");
    }

    #[test]
    fn wrong_passphrase_rejected() {
        let (_vault, file) = Vault::create("right").unwrap();
        assert!(matches!(
            Vault::unlock("wrong", &file),
            Err(VaultError::WrongPassphrase)
        ));
    }

    #[test]
    fn seal_is_not_plaintext_and_round_trips() {
        let (vault, _file) = Vault::create("pw").unwrap();
        let ct = vault.seal(b"hello").unwrap();
        assert_ne!(&ct[NONCE_LEN..], b"hello");
        assert_eq!(vault.open(&ct).unwrap(), b"hello");
    }

    #[test]
    fn tampered_ciphertext_rejected() {
        let (vault, _file) = Vault::create("pw").unwrap();
        let mut ct = vault.seal(b"hello").unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 0xff;
        assert!(vault.open(&ct).is_err());
    }

    #[test]
    fn corrupt_vault_file_rejected() {
        let (_v, mut file) = Vault::create("pw").unwrap();
        file[0] ^= 0xff; // break the magic
        assert!(matches!(
            Vault::unlock("pw", &file),
            Err(VaultError::Corrupt(_))
        ));
    }

    #[test]
    fn two_vaults_have_distinct_master_keys() {
        let (_a, fa) = Vault::create("pw").unwrap();
        let (_b, fb) = Vault::create("pw").unwrap();
        // same passphrase, different random salt and master key, so the files differ
        assert_ne!(fa, fb);
    }
}
