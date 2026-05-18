use std::fmt;

use super::ContactBlob;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contact {
    pub blob: ContactBlob,
    pub status: ContactStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactStatus {
    Pending,
    Verified,
    Rejected,
}

impl ContactStatus {
    pub fn label(self) -> &'static str {
        match self {
            ContactStatus::Pending => "pending",
            ContactStatus::Verified => "verified",
            ContactStatus::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(ContactStatus::Pending),
            "verified" => Some(ContactStatus::Verified),
            "rejected" => Some(ContactStatus::Rejected),
            _ => None,
        }
    }
}

impl Contact {
    pub fn short_label(&self) -> String {
        if let Some(name) = &self.blob.display_name {
            return name.clone();
        }
        let hex: String = self.blob.noise_static_pub.iter().take(4).fold(
            String::new(),
            |mut acc, b| {
                use std::fmt::Write;
                let _ = write!(&mut acc, "{:02x}", b);
                acc
            },
        );
        format!("{hex}…")
    }

    pub fn noise_hex(&self) -> String {
        self.blob.noise_static_pub.iter().fold(
            String::with_capacity(64),
            |mut acc, b| {
                use std::fmt::Write;
                let _ = write!(&mut acc, "{:02x}", b);
                acc
            },
        )
    }

    pub fn matches_query(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return false;
        }
        if let Some(name) = &self.blob.display_name {
            if name.to_lowercase() == q {
                return true;
            }
        }
        if q.chars().all(|c| c.is_ascii_hexdigit()) && q.len() >= 4 && q.len() <= 64 {
            return self.noise_hex().starts_with(&q);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contacts::ContactBlob;

    fn make(name: Option<&str>, noise: u8) -> Contact {
        Contact {
            blob: ContactBlob {
                noise_static_pub: [noise; 32],
                hs_id: [0; 32],
                display_name: name.map(|s| s.to_string()),
            },
            status: ContactStatus::Pending,
        }
    }

    #[test]
    fn matches_query_by_display_name() {
        let c = make(Some("Alice"), 0xab);
        assert!(c.matches_query("alice"));
        assert!(c.matches_query("ALICE"));
        assert!(!c.matches_query("bob"));
    }

    #[test]
    fn matches_query_by_hex_prefix() {
        let c = make(None, 0xab);
        assert!(c.matches_query("abab"));
        assert!(c.matches_query("ababab"));
        assert!(c.matches_query("abababab"));
        assert!(!c.matches_query("acac"));
    }

    #[test]
    fn rejects_short_or_nonhex_queries() {
        let c = make(None, 0xab);
        assert!(!c.matches_query("a"));
        assert!(!c.matches_query("ab"));
        assert!(!c.matches_query("xyz"));
        assert!(!c.matches_query(""));
    }
}

impl fmt::Display for Contact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.status.label(), self.short_label())
    }
}
