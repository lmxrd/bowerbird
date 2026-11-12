//! How often someone may try to sign in, or enter a code: governor's
//! keyed rate limiters, in memory (a restart forgets them, which keeps the
//! no-storage promise). Every attempt counts, right or wrong, and going over
//! is a 429 with `Retry-After`, never a lockout: nobody can lock someone
//! else out, only slow down guessing. The quotas are Potoo's and Woodmouse's too.
//!
//! - per email: 5 at once, then 1 a minute;
//! - per client IP (the forwarded one with `TRUST_PROXY`): 30 at once, then
//!   1 every 6 seconds;
//! - per user, for codes from the authenticator app and the password: 5 at
//!   once, then 1 a minute.

use crate::error::HttpError;
use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota};
use std::hash::Hash;
use std::num::NonZeroU32;
use std::time::Duration;
use uuid::Uuid;

pub struct Limits {
    email: DefaultKeyedRateLimiter<String>,
    ip: DefaultKeyedRateLimiter<String>,
    user: DefaultKeyedRateLimiter<Uuid>,
}

fn quota(burst: u32, refill: Duration) -> Quota {
    Quota::with_period(refill)
        .expect("a period above zero")
        .allow_burst(NonZeroU32::new(burst).expect("a burst above zero"))
}

impl Default for Limits {
    fn default() -> Self {
        Self::with_bursts(5, 30, 5)
    }
}

impl Limits {
    /// The quotas above, with other bursts: the tests of everything but
    /// the limits themselves use large ones.
    pub fn with_bursts(email: u32, ip: u32, user: u32) -> Self {
        Self {
            email: DefaultKeyedRateLimiter::keyed(quota(email, Duration::from_secs(60))),
            ip: DefaultKeyedRateLimiter::keyed(quota(ip, Duration::from_secs(6))),
            user: DefaultKeyedRateLimiter::keyed(quota(user, Duration::from_secs(60))),
        }
    }
}

/// One attempt for a key, or a 429 saying how long to wait.
fn attempt<K: Hash + Eq + Clone>(
    limiter: &DefaultKeyedRateLimiter<K>,
    key: &K,
) -> Result<(), HttpError> {
    limiter.check_key(key).map_err(|not_until| {
        let wait = not_until.wait_time_from(DefaultClock::default().now());
        HttpError::TooManyRequests {
            retry_after: wait.as_secs_f64().ceil().max(1.0) as u64,
        }
    })
}

impl Limits {
    /// A password sign-in: from this address, for this email.
    pub fn sign_in(&self, ip: &str, email: &str) -> Result<(), HttpError> {
        attempt(&self.ip, &ip.to_string())?;
        attempt(&self.email, &email.to_lowercase())
    }

    /// A secret tried from an address without an email or a user (an
    /// invite's token).
    pub fn address(&self, ip: &str) -> Result<(), HttpError> {
        attempt(&self.ip, &ip.to_string())
    }

    /// A code (or the password) of a signed-in user, or of one in the
    /// middle of signing in (then from an address too).
    pub fn user(&self, user_id: Uuid, ip: Option<&str>) -> Result<(), HttpError> {
        if let Some(ip) = ip {
            attempt(&self.ip, &ip.to_string())?;
        }
        attempt(&self.user, &user_id)
    }

    /// Forget the keys that are back to their full quota, so memory stays
    /// bounded however many addresses and emails were tried.
    pub fn forget_idle(&self) {
        for limiter in [&self.email, &self.ip] {
            limiter.retain_recent();
            limiter.shrink_to_fit();
        }
        self.user.retain_recent();
        self.user.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_attempts_per_email_then_wait() {
        let limits = Limits::default();
        for _ in 0..5 {
            limits.sign_in("192.0.2.1", "a@example.com").unwrap();
        }
        let Err(HttpError::TooManyRequests { retry_after }) =
            limits.sign_in("192.0.2.1", "A@example.com")
        else {
            panic!("the sixth attempt is refused, whatever the case of the email");
        };
        assert!((1..=60).contains(&retry_after));
        // Another email is not affected.
        limits.sign_in("192.0.2.1", "b@example.com").unwrap();
    }

    #[test]
    fn thirty_attempts_per_address() {
        let limits = Limits::default();
        for i in 0..30 {
            limits
                .sign_in("192.0.2.1", &format!("{i}@example.com"))
                .unwrap();
        }
        assert!(limits.sign_in("192.0.2.1", "x@example.com").is_err());
        limits.sign_in("192.0.2.2", "x@example.com").unwrap();
    }

    #[test]
    fn five_codes_per_user() {
        let limits = Limits::default();
        let (one, other) = (Uuid::from_u128(7), Uuid::from_u128(8));
        for _ in 0..5 {
            limits.user(one, None).unwrap();
        }
        assert!(limits.user(one, None).is_err());
        limits.user(other, None).unwrap();
    }
}
