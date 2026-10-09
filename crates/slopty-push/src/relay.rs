//! A server's request to the relay, and the relay's checks of it.
//!
//! The relay holds the team's APNs key, so it must not push for just anyone. Each server
//! install makes an Ed25519 key once ([`InstallKey`]) and signs every request with it: the
//! method, the path, the time and the body's hash. The relay takes a request signed within
//! [`SKEW_SECONDS`] of its own clock ([`admit`]), and binds a device token to the first
//! [`MAX_KEYS`] install keys that push to it ([`bind`]): a phone linked to a few servers
//! works, and a stranger who learned a token cannot push to it.
//!
//! The relay never reads the note: it gets the device's token, whether the push is urgent, two
//! opaque ids and the sealed body, and builds APNs' alert from its own words ([`forward`]). A
//! take-back carries only the opaque ids of the notes it takes back.
//! What it keeps is the binding and its rate limits, both the Worker's to store.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::apns::{self, Unfit};
use crate::provider::ProviderKey;
use crate::seal::Sealed;

/// The relay's one route.
pub const PATH: &str = "/push";
/// The header naming the install key, in base64.
pub const KEY_HEADER: &str = "x-slopty-key";
/// The header with the time the request was signed, in seconds since the Unix epoch.
pub const AT_HEADER: &str = "x-slopty-at";
/// The header with the signature, in base64.
pub const SIGNATURE_HEADER: &str = "x-slopty-signature";

/// How far a request's time may be from the relay's, either way.
pub const SKEW_SECONDS: u64 = 5 * 60;
/// The most install keys one device token is bound to.
pub const MAX_KEYS: usize = 4;
/// The most a request's body may be, in bytes: a sealed body that fits APNs, and its fields.
pub const MAX_BODY: usize = 6 * 1024;

/// An install key's public half.
pub type PublicKey = [u8; 32];

/// What a server asks the relay to push.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RelayPush {
    /// The device token, in hex.
    pub token: String,
    /// A development build's token.
    pub sandbox: bool,
    /// A note, or a take-back.
    pub what: RelayWhat,
}

/// What a [`RelayPush`] does on the phone ([`apns::What`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayWhat {
    /// Show a note.
    Note {
        /// An agent that needs the person.
        urgent: bool,
        /// What notes stack under, opaque.
        thread: String,
        /// What a later push replaces, opaque.
        collapse: String,
        /// The sealed body's encapsulated key, in base64.
        e: String,
        /// The sealed body's ciphertext, in base64.
        s: String,
    },
    /// Take back the notes pushed under these collapse ids.
    TakeBack {
        /// Their collapse ids.
        notes: Vec<String>,
    },
}

impl RelayPush {
    /// `push` as a server asks it of the relay.
    #[must_use]
    pub fn of(push: &apns::Push) -> Self {
        let what = match &push.what {
            apns::What::Note(note) => RelayWhat::Note {
                urgent: note.urgent,
                thread: note.thread.clone(),
                collapse: note.collapse.clone(),
                e: note.sealed.enc_text(),
                s: note.sealed.ct_text(),
            },
            apns::What::TakeBack(notes) => RelayWhat::TakeBack { notes: notes.clone() },
        };
        Self { token: push.token.clone(), sandbox: push.sandbox, what }
    }
}

/// A signed request, ready to send: its body and its three headers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Signed {
    /// The JSON body.
    pub body: Vec<u8>,
    /// The headers, by their lowercase names.
    pub headers: [(&'static str, String); 3],
}

/// A server install's own key, kept in its store.
pub struct InstallKey {
    key: SigningKey,
}

impl std::fmt::Debug for InstallKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallKey").field("public", &crate::b64::write(&self.public())).finish()
    }
}

impl InstallKey {
    /// A new key, from the system's random source.
    ///
    /// # Errors
    /// [`crate::PushError::Random`] when the random source fails.
    #[cfg(feature = "getrandom")]
    pub fn generate() -> Result<Self, crate::PushError> {
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret).map_err(|_os| crate::PushError::Random)?;
        Ok(Self { key: SigningKey::from_bytes(&secret) })
    }

    /// The key kept as `bytes` ([`Self::to_bytes`]).
    #[must_use]
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self { key: SigningKey::from_bytes(bytes) }
    }

    /// The key, to keep.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    /// Its public half, as the relay binds it.
    #[must_use]
    pub fn public(&self) -> PublicKey {
        self.key.verifying_key().to_bytes()
    }

    /// `push` signed at `now` (seconds since the Unix epoch).
    #[must_use]
    pub fn sign(&self, push: &RelayPush, now: u64) -> Signed {
        // A struct of strings and booleans always serialises.
        let body = serde_json::to_vec(push).unwrap_or_default();
        let signature = self.key.sign(&message(now, &body));
        Signed {
            body,
            headers: [
                (KEY_HEADER, crate::b64::write(&self.public())),
                (AT_HEADER, now.to_string()),
                (SIGNATURE_HEADER, crate::b64::write(&signature.to_bytes())),
            ],
        }
    }
}

/// What is signed: the route, the time and the body's hash.
fn message(at: u64, body: &[u8]) -> Vec<u8> {
    let mut message = format!("slopty relay\nPOST {PATH}\n{at}\n").into_bytes();
    message.extend_from_slice(blake3::hash(body).as_bytes());
    message
}

/// A request as it reached the relay: its three headers, where it had them, and its body.
#[derive(Clone, Copy, Debug)]
pub struct Incoming<'a> {
    /// [`KEY_HEADER`].
    pub key: Option<&'a str>,
    /// [`AT_HEADER`].
    pub at: Option<&'a str>,
    /// [`SIGNATURE_HEADER`].
    pub signature: Option<&'a str>,
    /// The body.
    pub body: &'a [u8],
}

/// A request the relay took: the push, and the install key that signed it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Admitted {
    /// What to push.
    pub push: apns::Push,
    /// Who asked.
    pub key: PublicKey,
}

/// Why the relay turned a request down.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Refusal {
    /// A header is missing, or not what it should be.
    #[error("not signed")]
    Unsigned,
    /// Signed too long ago, or too far ahead.
    #[error("signed at another time")]
    Stale,
    /// The signature is not the key's over this request.
    #[error("the signature does not hold")]
    Forged,
    /// The body is over [`MAX_BODY`], or the push over APNs' size.
    #[error("too large")]
    TooLarge,
    /// The body is not a push.
    #[error("not a push")]
    Malformed,
    /// The device token is bound to [`MAX_KEYS`] other installs.
    #[error("not this install's device")]
    NotBound,
    /// Over a rate limit, the device's or the install's.
    #[error("too many")]
    Limited,
}

impl Refusal {
    /// The HTTP status the relay answers with.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::Unsigned | Self::Stale | Self::Forged => 401,
            Self::TooLarge => 413,
            Self::Malformed => 400,
            Self::NotBound => 403,
            Self::Limited => 429,
        }
    }
}

/// Take `incoming` at `now`, or say why not: signed by the key it names, within
/// [`SKEW_SECONDS`], not too large, and a push.
///
/// # Errors
/// The [`Refusal`].
pub fn admit(incoming: &Incoming<'_>, now: u64) -> Result<Admitted, Refusal> {
    if incoming.body.len() > MAX_BODY {
        return Err(Refusal::TooLarge);
    }
    let (Some(key), Some(at), Some(signature)) = (incoming.key, incoming.at, incoming.signature)
    else {
        return Err(Refusal::Unsigned);
    };
    let key: PublicKey =
        crate::b64::read(key).and_then(|k| k.try_into().ok()).ok_or(Refusal::Unsigned)?;
    let at: u64 = at.parse().map_err(|_bad| Refusal::Unsigned)?;
    if now.abs_diff(at) > SKEW_SECONDS {
        return Err(Refusal::Stale);
    }
    let signature: [u8; 64] =
        crate::b64::read(signature).and_then(|s| s.try_into().ok()).ok_or(Refusal::Unsigned)?;
    let verifying = VerifyingKey::from_bytes(&key).map_err(|_bad| Refusal::Unsigned)?;
    verifying
        .verify_strict(&message(at, incoming.body), &Signature::from_bytes(&signature))
        .map_err(|_bad| Refusal::Forged)?;
    let asked: RelayPush =
        serde_json::from_slice(incoming.body).map_err(|_bad| Refusal::Malformed)?;
    let what = match asked.what {
        RelayWhat::Note { urgent, thread, collapse, e, s } => {
            let sealed = Sealed::from_text(&e, &s).ok_or(Refusal::Malformed)?;
            if !apns::is_id(&thread) || !apns::is_id(&collapse) {
                return Err(Refusal::Malformed);
            }
            apns::What::Note(apns::Note { urgent, thread, collapse, sealed })
        }
        RelayWhat::TakeBack { notes } => {
            let fits = (1..=apns::MAX_TAKE_BACK).contains(&notes.len());
            if !fits || !notes.iter().all(|n| apns::is_id(n)) {
                return Err(Refusal::Malformed);
            }
            apns::What::TakeBack(notes)
        }
    };
    if !apns::is_token(&asked.token) {
        return Err(Refusal::Malformed);
    }
    let push = apns::Push { token: asked.token, sandbox: asked.sandbox, what };
    Ok(Admitted { push, key })
}

/// The install keys a device token is bound to once `key` pushed to it.
///
/// Given those it was bound to: the same when `key` is among them, `key` added while there is
/// room, else refused. The Worker stores what this returns, which renews the binding's life.
///
/// # Errors
/// [`Refusal::NotBound`] when the token is bound to [`MAX_KEYS`] other keys.
pub fn bind(key: PublicKey, bound: &[PublicKey]) -> Result<Vec<PublicKey>, Refusal> {
    if bound.contains(&key) {
        return Ok(bound.to_vec());
    }
    if bound.len() >= MAX_KEYS {
        return Err(Refusal::NotBound);
    }
    Ok(bound.iter().copied().chain(std::iter::once(key)).collect())
}

/// The request to APNs for `admitted`, under `provider`'s token at `now` for the app `topic`.
///
/// # Errors
/// [`Refusal::TooLarge`] when the payload is over APNs' size, [`Refusal::Malformed`] when the
/// push is not one.
pub fn forward(
    admitted: &Admitted,
    provider: &ProviderKey,
    topic: &str,
    now: u64,
) -> Result<apns::Request, Refusal> {
    apns::request(&admitted.push, topic, &provider.token(now)).map_err(|unfit| match unfit {
        Unfit::TooLarge => Refusal::TooLarge,
        Unfit::Token | Unfit::Id => Refusal::Malformed,
    })
}

/// An install key as text, for a store's keys and values.
#[must_use]
pub fn key_text(key: &PublicKey) -> String {
    crate::b64::write(key)
}

/// An install key read from [`key_text`].
#[must_use]
pub fn key_of_text(text: &str) -> Option<PublicKey> {
    crate::b64::read(text)?.try_into().ok()
}

#[cfg(test)]
#[cfg(feature = "getrandom")]
mod tests {
    use super::*;
    use crate::provider::tests::{TEST_P8, read};
    use crate::seal::{self, DeviceKey};

    const TOPIC: &str = "dev.aislopware.slopty";
    const NOW: u64 = 1_791_200_000;

    fn incoming(signed: &Signed) -> Incoming<'_> {
        let header =
            |name: &str| signed.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str());
        Incoming {
            key: header(KEY_HEADER),
            at: header(AT_HEADER),
            signature: header(SIGNATURE_HEADER),
            body: &signed.body,
        }
    }

    /// The phone's push, sealed by the server and signed by its install, as it reaches the
    /// relay.
    fn asked(phone: &DeviceKey, words: &[u8], urgent: bool) -> RelayPush {
        let token = "0f".repeat(32);
        let sealed = seal::seal(&phone.public(), &token, words).unwrap();
        RelayPush::of(&apns::Push {
            token,
            sandbox: true,
            what: apns::What::Note(apns::Note {
                urgent,
                thread: "dGhyZWFk".to_owned(),
                collapse: "bm90ZQ".to_owned(),
                sealed,
            }),
        })
    }

    /// The relay takes a push its install signed, binds the device to that install, and hands
    /// APNs the sealed body byte for byte under the team's token, in its own words: nothing of
    /// the note reaches APNs, and the phone opens what APNs carried.
    #[test]
    fn the_relay_forwards_without_reading() {
        let phone = DeviceKey::generate().unwrap();
        let install = InstallKey::generate().unwrap();
        let provider = ProviderKey::from_p8(TEST_P8, "ABC123DEFG", "DEF123GHIJ").unwrap();
        let words = br#"{"title":"Fix the login bug","body":"Allow Bash? touch refused.txt"}"#;
        let push = asked(&phone, words, true);
        let RelayWhat::Note { thread, collapse, e, s, .. } = &push.what else { panic!("a note") };
        let signed = install.sign(&push, NOW);

        let admitted = admit(&incoming(&signed), NOW + 3).unwrap();
        assert_eq!(admitted.key, install.public());
        assert_eq!(bind(admitted.key, &[]).unwrap(), [install.public()], "bound on first use");
        let request = forward(&admitted, &provider, TOPIC, NOW).unwrap();

        assert_eq!(request.url(), format!("https://{}/3/device/{}", apns::SANDBOX, push.token));
        let header = |name: &str| {
            request.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone()).unwrap()
        };
        let bearer = header("authorization");
        let (_, claims, signed_by_team) =
            read(bearer.strip_prefix("bearer ").unwrap(), &provider.verifying_key());
        assert!(signed_by_team, "the team's token");
        assert_eq!(claims["iat"].as_u64().unwrap() % crate::provider::EPOCH_SECONDS, 0);
        assert_eq!(header("apns-topic"), TOPIC);
        assert_eq!(header("apns-push-type"), "alert");
        assert_eq!(header("apns-priority"), "10");
        assert_eq!(&header("apns-collapse-id"), collapse);

        let payload: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(payload["e"], *e, "the encapsulated key, byte for byte");
        assert_eq!(payload["s"], *s, "the ciphertext, byte for byte");
        assert_eq!(payload["aps"]["alert"]["title"], apns::TITLE);
        assert_eq!(payload["aps"]["interruption-level"], "time-sensitive");
        assert_eq!(payload["aps"]["thread-id"], *thread);
        let carried = [
            request.body.clone(),
            request.path.clone().into_bytes(),
            request.headers.iter().flat_map(|(_, v)| v.bytes()).collect(),
        ]
        .concat();
        for word in ["Fix", "login", "Allow", "Bash", "refused"] {
            let found = carried.windows(word.len()).any(|w| w == word.as_bytes());
            assert!(!found, "{word:?} reached APNs");
        }
        let sealed =
            Sealed::from_text(payload["e"].as_str().unwrap(), payload["s"].as_str().unwrap())
                .unwrap();
        assert_eq!(phone.open(&push.token, &sealed).unwrap(), words, "the phone reads it");
    }

    /// What the relay turns down, and the status it answers with: a request unsigned, signed by
    /// another key or over another body, signed too long ago, too large, not a push, or for a
    /// device bound to four other installs.
    #[test]
    fn the_relay_turns_down_what_it_cannot_trust() {
        let phone = DeviceKey::generate().unwrap();
        let install = InstallKey::generate().unwrap();
        let push = asked(&phone, b"words", false);
        let signed = install.sign(&push, NOW);
        let base = incoming(&signed);

        let unsigned = Incoming { signature: None, ..base };
        assert_eq!(admit(&unsigned, NOW), Err(Refusal::Unsigned));
        let stranger = InstallKey::generate().unwrap();
        let other_key = crate::b64::write(&stranger.public());
        let forged = Incoming { key: Some(&other_key), ..base };
        assert_eq!(admit(&forged, NOW), Err(Refusal::Forged), "another key's name");
        let mut changed = signed.body.clone();
        changed[3] ^= 1;
        let tampered = Incoming { body: &changed, ..base };
        assert_eq!(admit(&tampered, NOW), Err(Refusal::Forged), "another body");
        let late = NOW + SKEW_SECONDS + 1;
        assert_eq!(admit(&base, late), Err(Refusal::Stale));
        assert_eq!(admit(&base, NOW - SKEW_SECONDS - 1), Err(Refusal::Stale));
        let huge = vec![b' '; MAX_BODY + 1];
        assert_eq!(admit(&Incoming { body: &huge, ..base }, NOW), Err(Refusal::TooLarge));
        let mut odd = push.clone();
        odd.token = "a thread's title".to_owned();
        let odd = install.sign(&odd, NOW);
        assert_eq!(admit(&incoming(&odd), NOW), Err(Refusal::Malformed));
        for notes in [Vec::new(), vec!["a title".to_owned()], vec!["c".to_owned(); 17]] {
            let back = RelayPush { what: RelayWhat::TakeBack { notes }, ..push.clone() };
            let back = install.sign(&back, NOW);
            assert_eq!(admit(&incoming(&back), NOW), Err(Refusal::Malformed), "no take-back");
        }

        let full: Vec<PublicKey> = (1..=4_u8).map(|n| [n; 32]).collect();
        assert_eq!(bind(install.public(), &full), Err(Refusal::NotBound), "a fifth install");
        let mut room = full.clone();
        room.pop();
        assert_eq!(bind(install.public(), &room).unwrap().len(), MAX_KEYS);
        assert_eq!(bind([2; 32], &full).unwrap(), full, "a bound install keeps its place");
        assert_eq!(Refusal::NotBound.status(), 403);
        assert_eq!(Refusal::Forged.status(), 401);
        assert_eq!(key_of_text(&key_text(&install.public())), Some(install.public()));
    }

    /// A take-back goes through the relay's same checks and binding, and on to APNs as a
    /// background push naming only the opaque ids it was given.
    #[test]
    fn a_take_back_goes_through_as_a_background_push() {
        let install = InstallKey::generate().unwrap();
        let provider = ProviderKey::from_p8(TEST_P8, "ABC123DEFG", "DEF123GHIJ").unwrap();
        let notes = vec!["bm90ZQ".to_owned()];
        let what = RelayWhat::TakeBack { notes: notes.clone() };
        let back = RelayPush { token: "0f".repeat(32), sandbox: true, what };
        let signed = install.sign(&back, NOW);
        let admitted = admit(&incoming(&signed), NOW).unwrap();
        assert_eq!(admitted.push.what, apns::What::TakeBack(notes.clone()));
        let request = forward(&admitted, &provider, TOPIC, NOW).unwrap();
        assert!(request.headers.contains(&("apns-push-type", "background".to_owned())));
        let payload: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(payload[apns::TAKE_BACK], serde_json::json!(notes));
        assert!(payload["aps"].get("alert").is_none(), "nothing shown");
    }
}
