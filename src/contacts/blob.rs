use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

pub const BLOB_VERSION: u8 = 1;
pub const MAX_DISPLAY_NAME: usize = 64;
pub const SCHEME_PREFIX: &str = "cord1:";
pub const NOISE_KEY_LEN: usize = 32;
pub const HS_ID_LEN: usize = 32;
pub const CHECKSUM_LEN: usize = 4;
const MIN_BODY_LEN: usize = 1 + NOISE_KEY_LEN + HS_ID_LEN + 1 + CHECKSUM_LEN;

#[derive(Clone, PartialEq, Eq)]
pub struct ContactBlob {
    pub noise_static_pub: [u8; NOISE_KEY_LEN],
    pub hs_id: [u8; HS_ID_LEN],
    pub display_name: Option<String>,
}

impl fmt::Debug for ContactBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactBlob")
            .field("display_name", &self.display_name)
            .field("noise_static_pub", &"<32 bytes>")
            .field("hs_id", &"<32 bytes>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobError {
    UnknownScheme,
    UnsupportedVersion(u8),
    BadEncoding(String),
    BadLength { expected_min: usize, actual: usize },
    ChecksumMismatch,
    DisplayNameInvalid(String),
}

impl fmt::Display for BlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlobError::UnknownScheme => write!(f, "not a cord contact blob (missing cord1: prefix)"),
            BlobError::UnsupportedVersion(v) => {
                write!(f, "unsupported blob version {v}. update cord.")
            }
            BlobError::BadEncoding(msg) => write!(f, "blob decode failed: {msg}"),
            BlobError::BadLength { expected_min, actual } => {
                write!(f, "blob too short: {actual} bytes, need at least {expected_min}")
            }
            BlobError::ChecksumMismatch => write!(f, "blob checksum mismatch (paste corrupt?)"),
            BlobError::DisplayNameInvalid(msg) => write!(f, "display name invalid: {msg}"),
        }
    }
}

impl std::error::Error for BlobError {}

impl ContactBlob {
    pub fn encode(&self) -> String {
        let name_bytes = self.display_name.as_deref().unwrap_or("").as_bytes();
        let mut body = Vec::with_capacity(MIN_BODY_LEN + name_bytes.len());
        body.push(BLOB_VERSION);
        body.extend_from_slice(&self.noise_static_pub);
        body.extend_from_slice(&self.hs_id);
        body.push(name_bytes.len() as u8);
        body.extend_from_slice(name_bytes);
        let mut hasher = Sha256::new();
        hasher.update(&body);
        let digest = hasher.finalize();
        body.extend_from_slice(&digest[..CHECKSUM_LEN]);
        format!("{SCHEME_PREFIX}{}", URL_SAFE_NO_PAD.encode(&body))
    }

    pub fn decode(s: &str) -> Result<Self, BlobError> {
        let stripped: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let body_b64 = stripped
            .strip_prefix(SCHEME_PREFIX)
            .ok_or(BlobError::UnknownScheme)?;
        let body = URL_SAFE_NO_PAD
            .decode(body_b64)
            .map_err(|e| BlobError::BadEncoding(e.to_string()))?;

        if body.len() < MIN_BODY_LEN {
            return Err(BlobError::BadLength {
                expected_min: MIN_BODY_LEN,
                actual: body.len(),
            });
        }

        let version = body[0];
        if version != BLOB_VERSION {
            return Err(BlobError::UnsupportedVersion(version));
        }

        let mut noise_static_pub = [0u8; NOISE_KEY_LEN];
        noise_static_pub.copy_from_slice(&body[1..1 + NOISE_KEY_LEN]);

        let hs_start = 1 + NOISE_KEY_LEN;
        let mut hs_id = [0u8; HS_ID_LEN];
        hs_id.copy_from_slice(&body[hs_start..hs_start + HS_ID_LEN]);

        let name_len_idx = hs_start + HS_ID_LEN;
        let name_len = body[name_len_idx] as usize;
        if name_len > MAX_DISPLAY_NAME {
            return Err(BlobError::DisplayNameInvalid(format!(
                "name length {name_len} exceeds maximum {MAX_DISPLAY_NAME}"
            )));
        }

        let name_start = name_len_idx + 1;
        let name_end = name_start + name_len;
        if body.len() != name_end + CHECKSUM_LEN {
            return Err(BlobError::BadLength {
                expected_min: name_end + CHECKSUM_LEN,
                actual: body.len(),
            });
        }

        let display_name = if name_len == 0 {
            None
        } else {
            let name_bytes = &body[name_start..name_end];
            let s = std::str::from_utf8(name_bytes)
                .map_err(|e| BlobError::DisplayNameInvalid(e.to_string()))?;
            if s.chars().any(|c| c.is_control()) {
                return Err(BlobError::DisplayNameInvalid(
                    "control characters not allowed".into(),
                ));
            }
            if s.trim() != s {
                return Err(BlobError::DisplayNameInvalid(
                    "leading or trailing whitespace not allowed".into(),
                ));
            }
            Some(s.to_string())
        };

        let checksum_actual = &body[name_end..name_end + CHECKSUM_LEN];
        let mut hasher = Sha256::new();
        hasher.update(&body[..name_end]);
        let digest = hasher.finalize();
        if &digest[..CHECKSUM_LEN] != checksum_actual {
            return Err(BlobError::ChecksumMismatch);
        }

        Ok(Self {
            noise_static_pub,
            hs_id,
            display_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: Option<&str>) -> ContactBlob {
        ContactBlob {
            noise_static_pub: [7u8; NOISE_KEY_LEN],
            hs_id: [11u8; HS_ID_LEN],
            display_name: name.map(|s| s.to_string()),
        }
    }

    #[test]
    fn roundtrips_with_name() {
        let original = sample(Some("alice"));
        let encoded = original.encode();
        assert!(encoded.starts_with(SCHEME_PREFIX));
        let decoded = ContactBlob::decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn roundtrips_without_name() {
        let original = sample(None);
        let decoded = ContactBlob::decode(&original.encode()).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn tolerates_internal_whitespace_on_decode() {
        let original = sample(Some("bob"));
        let encoded = original.encode();
        let split = format!("{}\n   {}", &encoded[..20], &encoded[20..]);
        let decoded = ContactBlob::decode(&split).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn rejects_missing_prefix() {
        let err = ContactBlob::decode("notablob").unwrap_err();
        assert_eq!(err, BlobError::UnknownScheme);
    }

    #[test]
    fn rejects_wrong_version() {
        let original = sample(None);
        let mut encoded = original.encode();
        let body = URL_SAFE_NO_PAD
            .decode(encoded.strip_prefix(SCHEME_PREFIX).unwrap())
            .unwrap();
        let mut tampered = body;
        tampered[0] = 9;
        encoded = format!("{SCHEME_PREFIX}{}", URL_SAFE_NO_PAD.encode(&tampered));
        match ContactBlob::decode(&encoded).unwrap_err() {
            BlobError::UnsupportedVersion(v) => assert_eq!(v, 9),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn rejects_corrupted_checksum() {
        let original = sample(Some("charlie"));
        let encoded = original.encode();
        let body = URL_SAFE_NO_PAD
            .decode(encoded.strip_prefix(SCHEME_PREFIX).unwrap())
            .unwrap();
        let mut tampered = body;
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let corrupted = format!("{SCHEME_PREFIX}{}", URL_SAFE_NO_PAD.encode(&tampered));
        assert_eq!(
            ContactBlob::decode(&corrupted).unwrap_err(),
            BlobError::ChecksumMismatch
        );
    }

    #[test]
    fn rejects_truncated() {
        let original = sample(Some("dana"));
        let encoded = original.encode();
        let truncated = &encoded[..encoded.len() - 4];
        let err = ContactBlob::decode(truncated).unwrap_err();
        assert!(
            matches!(
                err,
                BlobError::BadLength { .. } | BlobError::ChecksumMismatch | BlobError::BadEncoding(_)
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_control_chars_in_name() {
        let bad = ContactBlob {
            noise_static_pub: [0u8; NOISE_KEY_LEN],
            hs_id: [0u8; HS_ID_LEN],
            display_name: Some("ab\x01cd".to_string()),
        };
        let encoded = bad.encode();
        match ContactBlob::decode(&encoded).unwrap_err() {
            BlobError::DisplayNameInvalid(_) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn encoded_length_is_compact() {
        let blob = sample(None);
        let encoded = blob.encode();
        assert!(
            encoded.len() < 110,
            "empty-name blob encoded to {} chars, expected under 110",
            encoded.len()
        );
    }
}
