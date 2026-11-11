//! Two-factor authentication with time-based one-time passwords (RFC 6238),
//! through `totp-rs`: HMAC-SHA1, 6 digits, a 30-second step, the
//! authenticator apps' defaults. The crate does not stop a code from being
//! used twice; `verify` takes the last step used for that.

use crate::util::sha256_hex;
use rand::{Rng, RngExt};
use std::time::{SystemTime, UNIX_EPOCH};
use totp_rs::{Algorithm, Builder, Secret, Totp};

const STEP_SECONDS: u64 = 30;
/// A code from the step before or after the current one is accepted too, for
/// a phone clock that is a little off.
const SKEW: u16 = 1;
/// How many recovery codes a user gets at a time.
pub const RECOVERY_CODES: usize = 10;

/// The TOTP of a base32 secret, labelled with the account for the app.
fn totp(secret: &str, account: &str) -> Option<Totp> {
    let bytes = Secret::try_from_base32(secret).ok()?;
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_skew(SKEW)
        .with_step_duration(STEP_SECONDS)
        .with_secret(bytes)
        .with_account_name(account)
        .with_issuer(Some("Organizer"))
        .build()
        .ok()
}

/// A new secret: 20 random bytes (160 bits, RFC 4226's recommendation),
/// base32, as the authenticator apps take it.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    Secret::from(bytes.to_vec()).to_base32()
}

/// The `otpauth://` URI an authenticator app scans as a QR code.
pub fn uri(secret: &str, email: &str) -> Option<String> {
    totp(secret, email)?.to_url().ok()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The current time step.
pub fn current_step() -> u64 {
    unix_now() / STEP_SECONDS
}

/// The step `code` belongs to, if it is the code of the secret for a step
/// within the allowed skew of the step `now` and later than `after` (the
/// last step used, so a code works once). The caller stores the step it
/// returns.
pub fn verify(secret: &str, code: &str, now: u64, after: u64) -> Option<u64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // The account name does not count for a code.
    totp(secret, "user")?
        .check(&code, now * STEP_SECONDS)
        .filter(|&step| step > after)
}

/// The code of a secret for a time step (the tests use it as an app would).
pub fn code_at(secret: &str, step: u64) -> Option<String> {
    Some(
        totp(secret, "user")?
            .generate(step * STEP_SECONDS)
            .to_string(),
    )
}

/// New recovery codes, as shown to the user once: ten groups of
/// `xxxxx-xxxxx` from an alphabet without look-alikes (50 bits each).
pub fn new_recovery_codes() -> Vec<String> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::rng();
    (0..RECOVERY_CODES)
        .map(|_| {
            let mut code = String::with_capacity(11);
            for i in 0..10 {
                if i == 5 {
                    code.push('-');
                }
                code.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
            }
            code
        })
        .collect()
}

/// What is stored for a recovery code. The codes are random and long, so a
/// plain hash is enough (no password hashing); spaces, dashes and case do
/// not count, so a code typed differently still matches.
pub fn recovery_code_hash(code: &str) -> String {
    let normalized: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    sha256_hex(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238, appendix B: the SHA-1 key is "12345678901234567890" and the
    // test values are 8 digits; their last 6 are what a 6-digit code is.
    #[test]
    fn matches_the_rfc_6238_test_vectors() {
        let secret = Secret::from(b"12345678901234567890".to_vec()).to_base32();
        for (time, expected) in [
            (59u64, "94287082"),
            (1111111109, "07081804"),
            (1111111111, "14050471"),
            (1234567890, "89005924"),
            (2000000000, "69279037"),
            (20000000000, "65353130"),
        ] {
            let step = time / STEP_SECONDS;
            assert_eq!(code_at(&secret, step).unwrap(), &expected[2..], "at {time}");
        }
    }

    #[test]
    fn a_code_works_within_the_skew_and_only_once() {
        let secret = new_secret();
        assert_eq!(secret.len(), 32);
        let now = 1_000_000;
        let code = code_at(&secret, now).unwrap();
        assert_eq!(verify(&secret, &code, now, 0), Some(now));
        let spaced = format!(" {} {} ", &code[..3], &code[3..]);
        assert_eq!(verify(&secret, &spaced, now, 0), Some(now));
        // Used: the same step is refused, an earlier one too.
        assert_eq!(verify(&secret, &code, now, now), None);
        let previous = code_at(&secret, now - 1).unwrap();
        assert_eq!(verify(&secret, &previous, now, 0), Some(now - 1));
        assert_eq!(verify(&secret, &previous, now, now), None);
        // Too old, too new, or not a code.
        let old = code_at(&secret, now - 2).unwrap();
        let new = code_at(&secret, now + 2).unwrap();
        // (A code may repeat within the window by chance: skip that case.)
        if ![code.as_str(), previous.as_str()].contains(&old.as_str()) {
            assert_eq!(verify(&secret, &old, now, 0), None);
        }
        if new != code && new != previous && Some(new.clone()) != code_at(&secret, now + 1) {
            assert_eq!(verify(&secret, &new, now, 0), None);
        }
        assert_eq!(verify(&secret, "12345", now, 0), None);
        assert_eq!(verify(&secret, "abcdef", now, 0), None);
        assert_eq!(verify("not base32!", "123456", now, 0), None);
    }

    #[test]
    fn recovery_codes_are_distinct_and_typed_loosely() {
        let codes = new_recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        assert!(
            codes
                .iter()
                .all(|c| c.len() == 11 && c.as_bytes()[5] == b'-')
        );
        let mut unique = codes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), RECOVERY_CODES);
        assert_eq!(
            recovery_code_hash("abcde-fghjk"),
            recovery_code_hash(" ABCDE FGHJK ")
        );
    }

    #[test]
    fn the_uri_names_the_issuer_and_the_account() {
        let secret = new_secret();
        let uri = uri(&secret, "a+b@example.com").unwrap();
        assert!(
            uri.starts_with("otpauth://totp/Organizer:a%2Bb%40example.com?"),
            "{uri}"
        );
        assert!(uri.contains(&format!("secret={secret}")), "{uri}");
        assert!(uri.contains("issuer=Organizer"), "{uri}");
    }
}
