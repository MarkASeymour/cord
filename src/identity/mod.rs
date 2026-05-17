use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rand::RngCore;

use crate::noise::{NoiseError, StaticKey};

pub mod store;

const PEER_ID_FILE: &str = "peer_id";
const NOISE_STATIC_FILE: &str = "noise_static_key";

#[derive(Clone)]
pub struct Identity {
    pub peer_id: PeerId,
    pub noise_static: Arc<StaticKey>,
    pub config_dir: PathBuf,
    pub freshly_generated: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId([u8; PeerId::BYTE_LEN]);

impl PeerId {
    pub const BYTE_LEN: usize = 16;
    pub const HEX_LEN: usize = Self::BYTE_LEN * 2;

    pub fn generate() -> Self {
        let mut bytes = [0u8; Self::BYTE_LEN];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_bytes(bytes: [u8; Self::BYTE_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; Self::BYTE_LEN] {
        &self.0
    }

    pub fn short(&self) -> String {
        let s = self.to_string();
        format!("{}…", &s[..8])
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({self})")
    }
}

#[derive(Debug)]
pub enum IdentityError {
    NoConfigDir,
    Io(std::io::Error),
    Corrupt(String),
    Noise(NoiseError),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::NoConfigDir => write!(
                f,
                "no config directory available (HOME / APPDATA unset?)"
            ),
            IdentityError::Io(e) => write!(f, "identity i/o: {e}"),
            IdentityError::Corrupt(msg) => write!(f, "identity file corrupt: {msg}"),
            IdentityError::Noise(e) => write!(f, "identity noise: {e}"),
        }
    }
}

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IdentityError::Io(e) => Some(e),
            IdentityError::Noise(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for IdentityError {
    fn from(e: std::io::Error) -> Self {
        IdentityError::Io(e)
    }
}

impl From<NoiseError> for IdentityError {
    fn from(e: NoiseError) -> Self {
        IdentityError::Noise(e)
    }
}

pub fn load_or_generate(config_dir_override: Option<PathBuf>) -> Result<Identity, IdentityError> {
    let config_dir = store::resolve_config_dir(config_dir_override)?;
    store::ensure_dir(&config_dir)?;

    let (peer_id, freshly_generated) = load_or_generate_peer_id(&config_dir)?;
    let noise_static = load_or_generate_noise_static(&config_dir)?;

    Ok(Identity {
        peer_id,
        noise_static: Arc::new(noise_static),
        config_dir,
        freshly_generated,
    })
}

fn load_or_generate_peer_id(config_dir: &Path) -> Result<(PeerId, bool), IdentityError> {
    let path = config_dir.join(PEER_ID_FILE);
    if path.exists() {
        Ok((PeerId::from_bytes(load_peer_id(&path)?), false))
    } else {
        let peer_id = PeerId::generate();
        store::write_atomic_0600(&path, peer_id.as_bytes())?;
        Ok((peer_id, true))
    }
}

fn load_or_generate_noise_static(config_dir: &Path) -> Result<StaticKey, IdentityError> {
    let path = config_dir.join(NOISE_STATIC_FILE);
    if path.exists() {
        let raw = std::fs::read(&path)?;
        Ok(StaticKey::from_bytes(raw)?)
    } else {
        let key = StaticKey::generate()?;
        store::write_atomic_0600(&path, key.as_bytes())?;
        Ok(key)
    }
}

fn load_peer_id(path: &Path) -> Result<[u8; PeerId::BYTE_LEN], IdentityError> {
    let raw = std::fs::read(path)?;
    if raw.len() != PeerId::BYTE_LEN {
        return Err(IdentityError::Corrupt(format!(
            "expected {} bytes, found {}",
            PeerId::BYTE_LEN,
            raw.len()
        )));
    }
    let mut bytes = [0u8; PeerId::BYTE_LEN];
    bytes.copy_from_slice(&raw);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_then_reloads_same_peer_id() {
        let dir = std::env::temp_dir().join(format!("cord-id-{:x}", rand::random::<u64>()));

        let first = load_or_generate(Some(dir.clone())).unwrap();
        assert!(first.freshly_generated);

        let second = load_or_generate(Some(dir.clone())).unwrap();
        assert!(!second.freshly_generated);
        assert_eq!(first.peer_id, second.peer_id);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn noise_static_key_persists_across_load() {
        let dir = std::env::temp_dir().join(format!("cord-noise-{:x}", rand::random::<u64>()));

        let first = load_or_generate(Some(dir.clone())).unwrap();
        let first_bytes = first.noise_static.as_bytes().to_vec();
        assert_eq!(first_bytes.len(), 32);

        let second = load_or_generate(Some(dir.clone())).unwrap();
        assert_eq!(second.noise_static.as_bytes(), &first_bytes[..]);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
