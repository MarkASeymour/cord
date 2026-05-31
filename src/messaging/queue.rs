//! On disk store for undelivered outgoing messages.
//!
//! Each contact gets one file under `<config_dir>/queue/<noise-hex>`, sealed
//! with the session [`Vault`] (see [`crate::crypto`]). A message stays in the
//! queue until the peer acks it, at which point [`Queue::remove`] drops it and
//! deletes the file once empty.
//!
//! The plaintext inside a file is a flat list of records:
//!
//! ```text
//! id (8 big endian) | text length (4 big endian) | UTF-8 text
//! ```
//!
//! Queues are small (one peer's backlog), so every mutation rewrites the whole
//! file. The whole file is resealed with a fresh nonce on each write.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::crypto::{Vault, VaultError};
use crate::identity::store::{ensure_dir, write_atomic_0600};

const QUEUE_DIR: &str = "queue";
const RECORD_HEADER: usize = 8 + 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedMessage {
    pub id: u64,
    pub text: String,
}

#[derive(Debug)]
pub enum QueueError {
    Io(std::io::Error),
    Vault(VaultError),
    Corrupt(String),
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueueError::Io(e) => write!(f, "queue i/o: {e}"),
            QueueError::Vault(e) => write!(f, "queue crypto: {e}"),
            QueueError::Corrupt(m) => write!(f, "queue file corrupt: {m}"),
        }
    }
}

impl std::error::Error for QueueError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            QueueError::Io(e) => Some(e),
            QueueError::Vault(e) => Some(e),
            QueueError::Corrupt(_) => None,
        }
    }
}

impl From<std::io::Error> for QueueError {
    fn from(e: std::io::Error) -> Self {
        QueueError::Io(e)
    }
}

impl From<VaultError> for QueueError {
    fn from(e: VaultError) -> Self {
        QueueError::Vault(e)
    }
}

pub struct Queue {
    dir: PathBuf,
    vault: Arc<Vault>,
}

impl Queue {
    pub fn new(config_dir: &Path, vault: Arc<Vault>) -> Self {
        Self {
            dir: config_dir.join(QUEUE_DIR),
            vault,
        }
    }

    fn path_for(&self, contact_hex: &str) -> PathBuf {
        self.dir.join(contact_hex)
    }

    /// Load the queued messages for a contact, oldest first. A missing file is
    /// an empty queue.
    pub fn load(&self, contact_hex: &str) -> Result<Vec<QueuedMessage>, QueueError> {
        let path = self.path_for(contact_hex);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let sealed = fs::read(&path)?;
        let plain = self.vault.open(&sealed)?;
        decode_messages(&plain)
    }

    /// Append a message to a contact's queue.
    pub fn enqueue(&self, contact_hex: &str, msg: QueuedMessage) -> Result<(), QueueError> {
        let mut msgs = self.load(contact_hex)?;
        msgs.push(msg);
        self.save(contact_hex, &msgs)
    }

    /// Drop the message with `id` from a contact's queue. Deletes the file when
    /// the queue becomes empty. A no-op if the id is not present.
    pub fn remove(&self, contact_hex: &str, id: u64) -> Result<(), QueueError> {
        let mut msgs = self.load(contact_hex)?;
        let before = msgs.len();
        msgs.retain(|m| m.id != id);
        if msgs.len() == before {
            return Ok(());
        }
        if msgs.is_empty() {
            let path = self.path_for(contact_hex);
            if path.exists() {
                fs::remove_file(&path)?;
            }
            Ok(())
        } else {
            self.save(contact_hex, &msgs)
        }
    }

    fn save(&self, contact_hex: &str, msgs: &[QueuedMessage]) -> Result<(), QueueError> {
        ensure_dir(&self.dir).map_err(|e| QueueError::Io(std::io::Error::other(e.to_string())))?;
        let plain = encode_messages(msgs);
        let sealed = self.vault.seal(&plain)?;
        write_atomic_0600(&self.path_for(contact_hex), &sealed)
            .map_err(|e| QueueError::Io(std::io::Error::other(e.to_string())))
    }

    /// The contact hex ids that currently have a queue file. Used to seed the
    /// retry loop on startup.
    pub fn contacts_with_pending(&self) -> Result<Vec<String>, QueueError> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(name) = entry.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        Ok(out)
    }
}

fn encode_messages(msgs: &[QueuedMessage]) -> Vec<u8> {
    let mut out = Vec::new();
    for m in msgs {
        out.extend_from_slice(&m.id.to_be_bytes());
        let bytes = m.text.as_bytes();
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

fn decode_messages(mut buf: &[u8]) -> Result<Vec<QueuedMessage>, QueueError> {
    let mut out = Vec::new();
    while !buf.is_empty() {
        if buf.len() < RECORD_HEADER {
            return Err(QueueError::Corrupt("truncated record header".into()));
        }
        let id = u64::from_be_bytes(buf[..8].try_into().unwrap());
        let len = u32::from_be_bytes(buf[8..RECORD_HEADER].try_into().unwrap()) as usize;
        buf = &buf[RECORD_HEADER..];
        if buf.len() < len {
            return Err(QueueError::Corrupt("truncated record body".into()));
        }
        let text = std::str::from_utf8(&buf[..len])
            .map_err(|_| QueueError::Corrupt("record text is not valid UTF-8".into()))?
            .to_string();
        buf = &buf[len..];
        out.push(QueuedMessage { id, text });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Vault;

    fn temp_queue() -> (PathBuf, Queue) {
        let dir = std::env::temp_dir().join(format!("cord-queue-{:x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let vault = Arc::new(Vault::create("test passphrase").unwrap().0);
        let queue = Queue::new(&dir, vault);
        (dir, queue)
    }

    fn hex(byte: u8) -> String {
        std::iter::repeat(format!("{byte:02x}")).take(32).collect()
    }

    #[test]
    fn enqueue_then_load_round_trips_in_order() {
        let (dir, q) = temp_queue();
        let c = hex(0xaa);
        q.enqueue(&c, QueuedMessage { id: 1, text: "first".into() }).unwrap();
        q.enqueue(&c, QueuedMessage { id: 2, text: "second 〜 unicode".into() }).unwrap();

        let loaded = q.load(&c).unwrap();
        assert_eq!(
            loaded,
            vec![
                QueuedMessage { id: 1, text: "first".into() },
                QueuedMessage { id: 2, text: "second 〜 unicode".into() },
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn remove_by_id_keeps_the_rest() {
        let (dir, q) = temp_queue();
        let c = hex(0xbb);
        q.enqueue(&c, QueuedMessage { id: 10, text: "keep".into() }).unwrap();
        q.enqueue(&c, QueuedMessage { id: 11, text: "drop".into() }).unwrap();
        q.remove(&c, 11).unwrap();

        let loaded = q.load(&c).unwrap();
        assert_eq!(loaded, vec![QueuedMessage { id: 10, text: "keep".into() }]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn removing_last_message_deletes_the_file() {
        let (dir, q) = temp_queue();
        let c = hex(0xcc);
        q.enqueue(&c, QueuedMessage { id: 5, text: "only".into() }).unwrap();
        q.remove(&c, 5).unwrap();

        assert!(q.load(&c).unwrap().is_empty());
        assert!(q.contacts_with_pending().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_missing_contact_is_empty() {
        let (dir, q) = temp_queue();
        assert!(q.load(&hex(0xdd)).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn contacts_with_pending_lists_each_hex() {
        let (dir, q) = temp_queue();
        let a = hex(0x01);
        let b = hex(0x02);
        q.enqueue(&a, QueuedMessage { id: 1, text: "x".into() }).unwrap();
        q.enqueue(&b, QueuedMessage { id: 2, text: "y".into() }).unwrap();

        let mut listed = q.contacts_with_pending().unwrap();
        listed.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(listed, expected);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn remove_missing_id_is_noop() {
        let (dir, q) = temp_queue();
        let c = hex(0xee);
        q.enqueue(&c, QueuedMessage { id: 1, text: "stay".into() }).unwrap();
        q.remove(&c, 999).unwrap();
        assert_eq!(q.load(&c).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
