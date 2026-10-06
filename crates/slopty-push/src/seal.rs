//! The note's words, sealed to the phone.
//!
//! The phone makes an X25519 key once and hands its public half to the person's own server over
//! the tailnet, with its device token. The server seals each note's body to it: HPKE (RFC 9180)
//! in base mode, `DHKEM(X25519, HKDF-SHA256)`, HKDF-SHA256 and ChaCha20-Poly1305, under
//! [`INFO`], with the device token as the associated data. So what the relay and APNs carry is
//! ciphertext, and a body sealed for one device opens on no other, even with its key.

use hpke::aead::ChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::X25519HkdfSha256;
use hpke::{Deserializable as _, Kem as _, OpModeR, Serializable as _};

use crate::PushError;

/// The HPKE suite's KEM.
type Kem = X25519HkdfSha256;

/// What the seal is for, bound into its keys: a key made for anything else opens nothing here.
pub const INFO: &[u8] = b"slopty push";

/// How long a key is, private or public, and the encapsulated key: X25519's 32 bytes.
pub const KEY_LEN: usize = 32;

/// A body sealed to a device: the sender's encapsulated key, and the ciphertext with its tag.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sealed {
    /// The encapsulated key (`enc`).
    pub enc: [u8; KEY_LEN],
    /// The ciphertext, its tag at its end.
    pub ct: Vec<u8>,
}

impl Sealed {
    /// The encapsulated key, as the push carries it.
    #[must_use]
    pub fn enc_text(&self) -> String {
        crate::b64::write(&self.enc)
    }

    /// The ciphertext, as the push carries it.
    #[must_use]
    pub fn ct_text(&self) -> String {
        crate::b64::write(&self.ct)
    }

    /// The sealed body the push carried, `None` where either part is not one.
    #[must_use]
    pub fn from_text(enc: &str, ct: &str) -> Option<Self> {
        let enc = crate::b64::read(enc)?.try_into().ok()?;
        Some(Self { enc, ct: crate::b64::read(ct)? })
    }
}

/// The phone's own key: only the phone holds it, in its Keychain.
pub struct DeviceKey {
    secret: <Kem as hpke::Kem>::PrivateKey,
}

impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey").field("public", &crate::b64::write(&self.public())).finish()
    }
}

impl DeviceKey {
    /// A new key, from the system's random source.
    ///
    /// # Errors
    /// [`PushError::Random`] when the random source fails.
    #[cfg(feature = "getrandom")]
    pub fn generate() -> Result<Self, PushError> {
        let mut ikm = [0_u8; KEY_LEN];
        getrandom::fill(&mut ikm).map_err(|_os| PushError::Random)?;
        let (secret, _) = Kem::derive_keypair(&ikm);
        Ok(Self { secret })
    }

    /// The key kept as `bytes` ([`Self::to_bytes`]).
    ///
    /// # Errors
    /// [`PushError::Key`] when they are not one.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PushError> {
        let secret =
            <Kem as hpke::Kem>::PrivateKey::from_bytes(bytes).map_err(|_hpke| PushError::Key)?;
        Ok(Self { secret })
    }

    /// The key, to keep.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; KEY_LEN] {
        self.secret.to_bytes().into()
    }

    /// Its public half, which the server seals to.
    #[must_use]
    pub fn public(&self) -> [u8; KEY_LEN] {
        Kem::sk_to_pk(&self.secret).to_bytes().into()
    }

    /// Open `sealed`, sent to the device whose token is `token`.
    ///
    /// # Errors
    /// [`PushError::Open`] when it was sealed to another key or another device, or any byte of
    /// it changed.
    pub fn open(&self, token: &str, sealed: &Sealed) -> Result<Vec<u8>, PushError> {
        let enc = <Kem as hpke::Kem>::EncappedKey::from_bytes(&sealed.enc)
            .map_err(|_hpke| PushError::Open)?;
        hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
            &OpModeR::Base,
            &self.secret,
            &enc,
            INFO,
            &sealed.ct,
            token.as_bytes(),
        )
        .map_err(|_hpke| PushError::Open)
    }
}

/// Seal `body` to the device whose public key is `public` and whose token is `token`.
///
/// The ephemeral key comes from the system's random source through `hpke`, which panics if that
/// source fails; on the systems the server runs on it does not.
///
/// # Errors
/// [`PushError::Key`] when `public` is not an X25519 key, [`PushError::Seal`] when sealing
/// fails.
#[cfg(feature = "getrandom")]
pub fn seal(public: &[u8], token: &str, body: &[u8]) -> Result<Sealed, PushError> {
    let public =
        <Kem as hpke::Kem>::PublicKey::from_bytes(public).map_err(|_hpke| PushError::Key)?;
    let (enc, ct) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &hpke::OpModeS::Base,
        &public,
        INFO,
        body,
        token.as_bytes(),
    )
    .map_err(|_hpke| PushError::Seal)?;
    Ok(Sealed { enc: enc.to_bytes().into(), ct })
}

#[cfg(test)]
#[cfg(feature = "getrandom")]
mod tests {
    use super::*;

    const TOKEN: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
    const OTHER: &str = "ffb2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    /// A body sealed to a device's key opens with that key, for that device, and with nothing
    /// else: not another key, not another device's token, not a changed byte. The key keeps.
    #[test]
    fn a_push_body_decrypts_only_with_the_devices_key() {
        let phone = DeviceKey::generate().unwrap();
        let stranger = DeviceKey::generate().unwrap();
        let words = b"Allow Bash? touch refused.txt";
        let sealed = seal(&phone.public(), TOKEN, words).unwrap();
        assert!(!sealed.ct.windows(5).any(|w| w == b"Allow"), "no word in the clear");

        assert_eq!(phone.open(TOKEN, &sealed).unwrap(), words);
        assert_eq!(stranger.open(TOKEN, &sealed), Err(PushError::Open), "another key");
        assert_eq!(phone.open(OTHER, &sealed), Err(PushError::Open), "another device");
        for at in [0, sealed.ct.len() / 2, sealed.ct.len() - 1] {
            let mut changed = sealed.clone();
            changed.ct[at] ^= 1;
            assert_eq!(phone.open(TOKEN, &changed), Err(PushError::Open), "byte {at}");
        }
        let mut moved = sealed.clone();
        moved.enc[0] ^= 1;
        assert_eq!(phone.open(TOKEN, &moved), Err(PushError::Open), "the encapsulated key");

        let kept = DeviceKey::from_bytes(&phone.to_bytes()).unwrap();
        assert_eq!(kept.public(), phone.public());
        let carried = Sealed::from_text(&sealed.enc_text(), &sealed.ct_text()).unwrap();
        assert_eq!(kept.open(TOKEN, &carried).unwrap(), words, "kept, and carried as text");
        assert_ne!(seal(&phone.public(), TOKEN, words).unwrap(), sealed, "a fresh seal each time");
        assert_eq!(DeviceKey::from_bytes(&[1; 5]).err(), Some(PushError::Key));
        assert_eq!(seal(&[1; 5], TOKEN, words), Err(PushError::Key));
    }
}
