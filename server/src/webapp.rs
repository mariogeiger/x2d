//! Signing in from the bot's Mini App: Telegram opens the page with its
//! init data, signed with a key drawn from the bot's token, which the page
//! hands to the server. The data is checked as Telegram documents it
//! (core.telegram.org/bots/webapps, "Validating data received via the Mini
//! App"): every field but `hash`, sorted and joined by line feeds, signed
//! by HMAC-SHA-256 under the HMAC-SHA-256 of the token keyed by
//! `WebAppData`.

use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;

/// Who opened the Mini App, as Telegram vouches.
#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct User {
    /// Their Telegram id.
    pub id: i64,
    /// Their first name.
    pub first_name: String,
}

/// How long init data stays good after Telegram signed it, in seconds.
const VALID_FOR: i64 = 24 * 3600;

/// The user of `init_data`, if the bot of `token` signed it less than a
/// day before `now`.
pub fn verify(init_data: &str, token: &str, now: i64) -> Option<User> {
    let mut fields: Vec<(String, String)> = form_urlencoded::parse(init_data.as_bytes()).into_owned().collect();
    let position = fields.iter().position(|(key, _)| key == "hash")?;
    let hash = decode_hex(&fields.remove(position).1)?;
    fields.sort();
    let check = fields.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join("\n");
    let mut key = Hmac::<Sha256>::new_from_slice(b"WebAppData").ok()?;
    key.update(token.as_bytes());
    let mut signature = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes()).ok()?;
    signature.update(check.as_bytes());
    signature.verify_slice(&hash).ok()?;
    let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    let signed_at: i64 = field("auth_date")?.parse().ok()?;
    if !(0..VALID_FOR).contains(&(now - signed_at)) {
        return None;
    }
    serde_json::from_str(field("user")?).ok()
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Init data signed for this test with Python's `hmac`, as Telegram
    /// would sign it for the bot of `TOKEN`.
    const SIGNED: &str = "query_id=AAH0&user=%7B%22id%22%3A42%2C%22first_name%22%3A%22Ada%22%2C%22username%22%3A%22ada%22%7D&auth_date=1792757600&signature=c2lnbmVk&hash=da9e2a14927d58cc07e145d12b4f7e0935775f4a0ca60ed1042cc228177b6f55";
    const TOKEN: &str = "123456:TEST-TOKEN-for-x2d";
    const SIGNED_AT: i64 = 1_792_757_600;

    #[test]
    fn data_signed_by_the_bot_names_its_user() {
        assert_eq!(verify(SIGNED, TOKEN, SIGNED_AT + 60), Some(User { id: 42, first_name: "Ada".into() }));
    }

    #[test]
    fn anything_else_is_refused() {
        assert_eq!(verify(SIGNED, "123456:another-token", SIGNED_AT + 60), None, "another bot");
        assert_eq!(verify(SIGNED, TOKEN, SIGNED_AT + 2 * VALID_FOR), None, "too old");
        assert_eq!(verify(&SIGNED.replace("%3A42", "%3A43"), TOKEN, SIGNED_AT + 60), None, "altered");
        assert_eq!(verify(&SIGNED.replace("&hash=", "&hush="), TOKEN, SIGNED_AT + 60), None, "unsigned");
        assert_eq!(verify("", TOKEN, SIGNED_AT), None);
    }
}
