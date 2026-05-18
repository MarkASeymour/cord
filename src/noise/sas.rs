use hkdf::Hkdf;
use sha2::Sha256;

const SAS_INFO: &[u8] = b"cord SAS v1";
const SAS_DIGITS: usize = 18;
const SAS_MODULUS: u64 = 1_000_000_000_000_000_000;
const SAS_GROUP_LEN: usize = 3;

pub fn derive(handshake_hash: &[u8]) -> String {
    let hkdf = Hkdf::<Sha256>::new(None, handshake_hash);
    let mut bytes = [0u8; 8];
    hkdf.expand(SAS_INFO, &mut bytes)
        .expect("hkdf output 8 bytes always fits");
    let num = u64::from_be_bytes(bytes) % SAS_MODULUS;
    let digits = format!("{num:0>width$}", width = SAS_DIGITS);
    format_groups(&digits)
}

fn format_groups(digits: &str) -> String {
    let mut out = String::with_capacity(SAS_DIGITS + SAS_DIGITS / SAS_GROUP_LEN);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && i % SAS_GROUP_LEN == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_hash_same_sas() {
        let h = b"0123456789abcdef0123456789abcdef";
        assert_eq!(derive(h), derive(h));
    }

    #[test]
    fn different_hash_different_sas() {
        let a = derive(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let b = derive(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert_ne!(a, b);
    }

    #[test]
    fn format_is_six_groups_of_three_digits() {
        let s = derive(b"some handshake hash bytes here..");
        let groups: Vec<&str> = s.split(' ').collect();
        assert_eq!(groups.len(), 6, "expected 6 groups in {s:?}");
        for g in groups {
            assert_eq!(g.len(), 3, "group {g:?} not 3 chars");
            assert!(g.chars().all(|c| c.is_ascii_digit()), "non digit in {g:?}");
        }
    }
}
