//! Time-based one-time passwords (RFC 6238 with RFC 4226 HOTP): HMAC-SHA1,
//! 30-second steps, 6 digits, as authenticator apps expect (ADR 0015).

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// Seconds per step.
pub const STEP_SECONDS: i64 = 30;
/// Steps accepted either side of now (clock drift).
pub const DRIFT_STEPS: i64 = 1;

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Base32 (RFC 4648) without padding.
#[must_use]
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// Decodes base32, ignoring spaces, padding and case.
#[must_use]
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let upper = c.to_ascii_uppercase();
        let value = ALPHABET.iter().position(|&a| char::from(a) == upper)?;
        buffer = (buffer << 5) | u32::try_from(value).ok()?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// A new random 160-bit secret, base32.
pub fn new_secret() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 20];
    getrandom::fill(&mut bytes)?;
    Ok(base32_encode(&bytes))
}

/// The `digits`-digit HOTP code for a counter.
#[must_use]
pub fn hotp(key: &[u8], counter: u64, digits: u32) -> Option<u32> {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).ok()?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest.get(19)? & 0x0f);
    let slice = digest.get(offset..offset + 4)?;
    let binary = u32::from_be_bytes([slice[0] & 0x7f, slice[1], slice[2], slice[3]]);
    Some(binary % 10_u32.checked_pow(digits)?)
}

/// The time step of a moment.
#[must_use]
pub fn step_at(at: DateTime<Utc>) -> i64 {
    at.timestamp().div_euclid(STEP_SECONDS)
}

/// The step a 6-digit code matches at `now` (± drift), if any.
#[must_use]
pub fn verify(secret_base32: &str, code: &str, now: DateTime<Utc>) -> Option<i64> {
    let code = code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let wanted: u32 = code.parse().ok()?;
    let key = base32_decode(secret_base32)?;
    let now_step = step_at(now);
    (now_step - DRIFT_STEPS..=now_step + DRIFT_STEPS).find(|&step| {
        u64::try_from(step)
            .ok()
            .and_then(|s| hotp(&key, s, 6))
            .is_some_and(|c| c == wanted)
    })
}

/// The `otpauth://` URI authenticator apps import (also as a QR code).
#[must_use]
pub fn otpauth_uri(issuer: &str, account: &str, secret_base32: &str) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    format!(
        "otpauth://totp/{}:{}?secret={secret_base32}&issuer={}&algorithm=SHA1&digits=6&period=30",
        enc(issuer),
        enc(account),
        enc(issuer)
    )
}

#[cfg(test)]
// Test code: a failed unwrap is a failed test (this crate has no clippy.toml
// allowing it in tests).
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn rfc_6238_sha1_vectors() {
        let key = b"12345678901234567890";
        for (time, eight_digits) in [
            (59_i64, 94_287_082_u32),
            (1_111_111_109, 7_081_804),
            (1_234_567_890, 89_005_924),
            (2_000_000_000, 69_279_037),
        ] {
            let step = u64::try_from(time / STEP_SECONDS).unwrap();
            assert_eq!(hotp(key, step, 8), Some(eight_digits), "t={time}");
        }
    }

    #[test]
    fn base32_round_trips_and_verification_allows_one_step_of_drift() {
        let key = b"12345678901234567890";
        let secret = base32_encode(key);
        assert_eq!(secret, "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(base32_decode(&secret.to_lowercase()).unwrap(), key);
        let at = Utc.timestamp_opt(1_111_111_109, 0).unwrap();
        assert_eq!(verify(&secret, "081804", at), Some(step_at(at)));
        assert_eq!(
            verify(&secret, "081804", at + chrono::Duration::seconds(30)),
            Some(step_at(at))
        );
        assert_eq!(
            verify(&secret, "081804", at + chrono::Duration::seconds(90)),
            None
        );
        assert_eq!(verify(&secret, "08180", at), None);
        assert_eq!(verify(&secret, "abcdef", at), None);
    }
}
