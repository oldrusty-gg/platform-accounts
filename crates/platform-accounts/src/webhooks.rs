//! Supporter webhooks into supporter flags (foundation; no HTTP endpoint
//! is served yet).
//!
//! Ko-fi posts `application/x-www-form-urlencoded` with one field, `data`,
//! a JSON object carrying the account's `verification_token`; a supporter
//! is matched by a linked Ko-fi address or else by their verified platform
//! address. Patreon (v2 webhooks, HMAC-MD5 signature header) is described
//! in the plan and not implemented here.

use crate::store::{email_key, perks, PlatformStore};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct KofiEvent {
    pub verification_token: String,
    pub message_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub is_subscription_payment: bool,
    #[serde(default)]
    pub tier_name: Option<String>,
    #[serde(default)]
    pub kofi_transaction_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebhookError {
    Malformed,
    /// The verification token is not ours: ignore (and log) the request.
    Forged,
}

fn url_decode(text: &str) -> Option<String> {
    let mut out = Vec::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Read a Ko-fi delivery body and check its token (constant time).
pub fn parse_kofi(form_body: &str, expected_token: &str) -> Result<KofiEvent, WebhookError> {
    if form_body.len() > 64 * 1024 {
        return Err(WebhookError::Malformed);
    }
    let data = form_body
        .split('&')
        .find_map(|pair| pair.strip_prefix("data="))
        .and_then(url_decode)
        .ok_or(WebhookError::Malformed)?;
    let event: KofiEvent = serde_json::from_str(&data).map_err(|_| WebhookError::Malformed)?;
    let (a, b) = (event.verification_token.as_bytes(), expected_token.as_bytes());
    let same = a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0;
    if !same || expected_token.is_empty() {
        return Err(WebhookError::Forged);
    }
    Ok(event)
}

/// What applying an event did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    Supporter { user_id: i64, until: i64 },
    /// Seen before (a provider retry).
    Duplicate,
    /// No platform account matches: kept for the owner to link by hand.
    Unmatched,
}

/// A support payment's perks last this long (a monthly subscription is
/// renewed by its next payment; a one-off tip keeps the badge a month).
pub const SUPPORT_DAYS: i64 = 35;

pub fn apply_kofi(store: &PlatformStore, event: &KofiEvent, now: i64) -> Result<Applied, String> {
    if !store.first_delivery("kofi", &event.message_id, now)? {
        return Ok(Applied::Duplicate);
    }
    let Some(email) = event.email.as_deref().map(email_key) else { return Ok(Applied::Unmatched) };
    let user = match store.linked_user("kofi", &email)? {
        Some(u) => Some(u),
        None => store.user_for_email(&email)?,
    };
    let Some(user_id) = user else { return Ok(Applied::Unmatched) };
    let flags = if event.is_subscription_payment { perks::BADGE | perks::FLAIR } else { perks::BADGE };
    let until = now + SUPPORT_DAYS * 24 * 3600;
    store.set_supporter(user_id, "kofi", event.kofi_transaction_id.as_deref(), event.tier_name.as_deref(), flags, Some(until), now)?;
    Ok(Applied::Supporter { user_id, until })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(token: &str, id: &str, email: &str) -> String {
        let json = serde_json::json!({
            "verification_token": token, "message_id": id, "type": "Subscription", "email": email,
            "is_subscription_payment": true, "tier_name": "Server costs", "kofi_transaction_id": "tx-9",
            "from_name": "Yugi", "amount": "3.00", "currency": "GBP"
        });
        let encoded: String = json
            .to_string()
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        format!("data={encoded}")
    }

    #[test]
    fn a_kofi_payment_flags_the_matching_account_once() {
        let mut store = PlatformStore::in_memory(&[4; 32]).unwrap();
        let i = store.start_email_verification(11, "fan@example.com", 1000).unwrap().unwrap();
        store.verify_email(11, &i.code, 1000).unwrap().unwrap();
        let event = parse_kofi(&body("tok", "m1", "Fan@Example.com"), "tok").unwrap();
        assert_eq!(event.kind, "Subscription");
        assert!(matches!(apply_kofi(&store, &event, 2000).unwrap(), Applied::Supporter { user_id: 11, .. }));
        assert_eq!(store.supporter_flags(11, 2000).unwrap(), perks::BADGE | perks::FLAIR);
        assert_eq!(apply_kofi(&store, &event, 2001).unwrap(), Applied::Duplicate);
        let stranger = parse_kofi(&body("tok", "m2", "nobody@example.com"), "tok").unwrap();
        assert_eq!(apply_kofi(&store, &stranger, 2002).unwrap(), Applied::Unmatched);
    }

    #[test]
    fn forged_or_broken_deliveries_are_refused() {
        assert_eq!(parse_kofi(&body("wrong", "m1", "a@b.co"), "tok"), Err(WebhookError::Forged));
        assert_eq!(parse_kofi(&body("", "m1", "a@b.co"), ""), Err(WebhookError::Forged));
        assert_eq!(parse_kofi("data=%7Bnot json", "tok"), Err(WebhookError::Malformed));
        assert_eq!(parse_kofi("nothing", "tok"), Err(WebhookError::Malformed));
    }
}
