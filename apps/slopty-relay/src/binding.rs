//! Which installs a phone's token is bound to, as the relay keeps it in KV.
//!
//! A record holds the install keys ([`slopty_push::relay::key_text`]) and when it was last
//! written. KV keeps it for [`TTL_SECONDS`] after each write, and a push writes it again only
//! when its keys change or it is a day old ([`RENEW_SECONDS`]): KV takes few writes a day, and a
//! busy phone would otherwise write on every push.

use serde::{Deserialize, Serialize};
use slopty_push::relay::{self, PublicKey, Refusal};

/// How long KV keeps a record after it is written: a phone that hears nothing for a month binds
/// afresh.
pub const TTL_SECONDS: u64 = 30 * 24 * 60 * 60;
/// How old a record grows before a push writes it again.
pub const RENEW_SECONDS: u64 = 24 * 60 * 60;

/// A token's record.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Record {
    /// The install keys it is bound to, as text.
    pub keys: Vec<String>,
    /// When it was written, in seconds since the Unix epoch.
    pub at: u64,
}

/// The record to write once `key` pushes at `now` to a token whose record is `stored`, or
/// `None` when the stored one stands. A stored record that does not read binds afresh.
///
/// # Errors
/// [`Refusal::NotBound`] when the token is bound to [`relay::MAX_KEYS`] other installs.
pub fn bind(stored: Option<&str>, key: PublicKey, now: u64) -> Result<Option<String>, Refusal> {
    let stored = stored.and_then(|text| serde_json::from_str::<Record>(text).ok());
    let keys: Vec<PublicKey> = stored
        .as_ref()
        .map(|r| r.keys.iter().filter_map(|k| relay::key_of_text(k)).collect())
        .unwrap_or_default();
    let bound = relay::bind(key, &keys)?;
    let fresh = stored.as_ref().is_some_and(|r| now.saturating_sub(r.at) < RENEW_SECONDS);
    if bound == keys && fresh {
        return Ok(None);
    }
    let record = Record { keys: bound.iter().map(relay::key_text).collect(), at: now };
    // A record of strings and a number always serialises.
    Ok(Some(serde_json::to_string(&record).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first install binds a token; it pushes again without a write until the record is a
    /// day old; up to four installs share it and a fifth is refused; a record that does not read
    /// binds afresh.
    #[test]
    fn a_token_binds_to_its_first_installs() {
        let key = |n: u8| [n; 32];
        let first = bind(None, key(1), 1_000).unwrap().unwrap();
        let read = |text: &str| serde_json::from_str::<Record>(text).unwrap();
        assert_eq!(read(&first).keys, [relay::key_text(&key(1))]);
        assert_eq!(bind(Some(&first), key(1), 2_000).unwrap(), None, "nothing to write");
        let renewed = bind(Some(&first), key(1), 1_000 + RENEW_SECONDS).unwrap().unwrap();
        assert_eq!(read(&renewed).at, 1_000 + RENEW_SECONDS, "written again a day on");
        let mut stored = first;
        for n in 2..=4 {
            stored = bind(Some(&stored), key(n), 3_000).unwrap().unwrap();
        }
        assert_eq!(read(&stored).keys.len(), relay::MAX_KEYS);
        assert_eq!(bind(Some(&stored), key(5), 3_000), Err(Refusal::NotBound));
        assert_eq!(bind(Some(&stored), key(3), 3_000).unwrap(), None, "one of its four");
        let afresh = bind(Some("not a record"), key(9), 4_000).unwrap().unwrap();
        assert_eq!(read(&afresh).keys, [relay::key_text(&key(9))]);
    }
}
