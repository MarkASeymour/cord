use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::identity::store::write_atomic_0600;

use super::{Contact, ContactBlob, ContactStatus};

const CONTACTS_FILE: &str = "contacts";

#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "contacts store i/o: {e}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io(e) => Some(e),
        }
    }
}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

pub fn contacts_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CONTACTS_FILE)
}

pub fn load(config_dir: &Path) -> Result<Vec<Contact>, StoreError> {
    let path = contacts_path(config_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((status_token, blob_token)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        let Some(status) = ContactStatus::parse(status_token) else {
            continue;
        };
        let Ok(blob) = ContactBlob::decode(blob_token.trim()) else {
            continue;
        };
        out.push(Contact { blob, status });
    }
    Ok(out)
}

pub fn save(config_dir: &Path, contacts: &[Contact]) -> Result<(), StoreError> {
    let path = contacts_path(config_dir);
    let mut body = String::new();
    for c in contacts {
        body.push_str(c.status.label());
        body.push(' ');
        body.push_str(&c.blob.encode());
        body.push('\n');
    }
    write_atomic_0600(&path, body.as_bytes()).map_err(|e| {
        StoreError::Io(io::Error::other(format!("atomic write: {e}")))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::ContactBlob;

    fn sample(name: &str) -> ContactBlob {
        ContactBlob {
            noise_static_pub: [0xab; 32],
            hs_id: [0xcd; 32],
            display_name: Some(name.to_string()),
        }
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("cord-contacts-{:x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();

        let contacts = vec![
            Contact { blob: sample("alice"), status: ContactStatus::Pending },
            Contact { blob: sample("bob"),   status: ContactStatus::Verified },
        ];
        save(&dir, &contacts).unwrap();

        let loaded = load(&dir).unwrap();
        assert_eq!(loaded, contacts);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn skips_unparseable_lines_on_load() {
        let dir = std::env::temp_dir().join(format!("cord-contacts-{:x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = contacts_path(&dir);

        let blob_str = sample("carol").encode();
        let body = format!(
            "# a comment\n\npending {blob_str}\nbroken line\nbogus cord1:notreal\n"
        );
        std::fs::write(&path, body).unwrap();

        let loaded = load(&dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].blob.display_name.as_deref(), Some("carol"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn returns_empty_when_file_missing() {
        let dir = std::env::temp_dir().join(format!("cord-contacts-{:x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        let loaded = load(&dir).unwrap();
        assert!(loaded.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
