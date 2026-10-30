//! Short codes a player types (from an email) and signed links (for the
//! web page later). Only an HMAC of a code is stored, keyed by the server
//! secret: a copy of the database alone cannot be used to verify or reset
//! anything.

use platform_sdk::b64;
use ring::hmac;

/// No 0/O, 1/I/L (the same alphabet as invite codes).
pub const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
pub const CODE_LEN: usize = 8;

/// A random code: 8 characters (about 39 bits; codes are also limited to a
/// few tries and minutes, see `store`).
pub fn new_code() -> String {
    let n = CODE_ALPHABET.len() as u16;
    let limit = 256 - 256 % n;
    let mut out = String::with_capacity(CODE_LEN);
    while out.len() < CODE_LEN {
        for b in crate::random_bytes::<16>() {
            if u16::from(b) < limit && out.len() < CODE_LEN {
                out.push(char::from(CODE_ALPHABET[usize::from(u16::from(b) % n)]));
            }
        }
    }
    out
}

/// A code as shown: `ABCD-EFGH`.
pub fn shown(code: &str) -> String {
    format!("{}-{}", &code[..4], &code[4..])
}

/// A code as typed: upper case, no dashes or spaces.
pub fn normalize(code: &str) -> String {
    code.chars().filter(|c| !matches!(c, '-' | ' ')).map(|c| c.to_ascii_uppercase()).collect()
}

pub struct Keys {
    key: hmac::Key,
}

impl Keys {
    pub fn new(secret: &[u8; 32]) -> Self {
        Self { key: hmac::Key::new(hmac::HMAC_SHA256, secret) }
    }

    fn mac(&self, text: &str) -> Vec<u8> {
        hmac::sign(&self.key, text.as_bytes()).as_ref().to_vec()
    }

    /// The stored form of `code` for one purpose and account.
    pub fn code_hash(&self, purpose: &str, user_id: i64, code: &str) -> String {
        b64::encode(&self.mac(&format!("code|{purpose}|{user_id}|{}", normalize(code))))
    }

    /// `v1.<purpose>.<token id>.<expires>.<signature>`: the token id names
    /// the database row (single use); the signature proves this server made
    /// the link and fixes its expiry.
    pub fn link(&self, purpose: &str, token_id: &str, expires: i64) -> String {
        let body = format!("v1.{purpose}.{token_id}.{expires}");
        let sig = self.mac(&body);
        format!("{body}.{}", b64::encode(&sig[..16]))
    }

    /// The (purpose, token id) of a link this server signed that has not
    /// expired at `now`.
    pub fn open_link(&self, link: &str, now: i64) -> Option<(String, String)> {
        let (body, sig) = link.rsplit_once('.')?;
        let sig = b64::decode(sig)?;
        let full = self.mac(body);
        // Constant time: ring's verify over the full tag is not usable for a
        // truncated one, so compare all 16 bytes without an early exit.
        if sig.len() != 16 || sig.iter().zip(&full[..16]).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
            return None;
        }
        let mut parts = body.split('.');
        let (v, purpose, id, expires) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        if v != "v1" || parts.next().is_some() || expires.parse::<i64>().ok()? <= now {
            return None;
        }
        Some((purpose.to_string(), id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_from_the_alphabet_and_normalise() {
        let c = new_code();
        assert_eq!(c.len(), CODE_LEN);
        assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(normalize(&shown(&c).to_lowercase()), c);
        assert_ne!(new_code(), new_code());
    }

    #[test]
    fn hashes_depend_on_secret_purpose_and_account() {
        let (a, b) = (Keys::new(&[1; 32]), Keys::new(&[2; 32]));
        let h = a.code_hash("verify_email", 1, "ABCD-EFGH");
        assert_eq!(h, a.code_hash("verify_email", 1, "abcdefgh"));
        assert_ne!(h, b.code_hash("verify_email", 1, "ABCDEFGH"));
        assert_ne!(h, a.code_hash("reset_password", 1, "ABCDEFGH"));
        assert_ne!(h, a.code_hash("verify_email", 2, "ABCDEFGH"));
    }

    #[test]
    fn links_are_signed_and_expire() {
        let k = Keys::new(&[7; 32]);
        let link = k.link("verify_email", "abc123", 1000);
        assert_eq!(k.open_link(&link, 999), Some(("verify_email".into(), "abc123".into())));
        assert_eq!(k.open_link(&link, 1000), None);
        assert_eq!(Keys::new(&[8; 32]).open_link(&link, 0), None);
        let longer = link.replace(".1000.", ".9999.");
        assert_eq!(k.open_link(&longer, 0), None, "the expiry is signed");
    }
}
