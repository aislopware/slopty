//! The provider token APNs takes from whoever holds the team's key: a JWT, ES256, its key id in
//! the header and the team id and the time in its claims.
//!
//! Apple asks that the token be made again no more often than every 20 minutes and no less
//! often than every 60, and refuses one made too often (`TooManyProviderTokenUpdates`). So that
//! a server started again, or more than one caller, never makes one too often, the time a
//! token says is the start of its half hour ([`EPOCH_SECONDS`]), and ECDSA signs deterministically
//! (RFC 6979, `p256`'s way): every caller in a half hour makes the same token, byte for byte, and
//! the token changes every 30 minutes, inside Apple's window.

use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey as _;

use crate::PushError;

/// How long one token is used: a half hour, between Apple's 20 and 60 minutes.
pub const EPOCH_SECONDS: u64 = 30 * 60;

/// The team's APNs key, its id and the team's id.
pub struct ProviderKey {
    key: SigningKey,
    key_id: String,
    team_id: String,
}

impl std::fmt::Debug for ProviderKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderKey")
            .field("key_id", &self.key_id)
            .field("team_id", &self.team_id)
            .finish_non_exhaustive()
    }
}

impl ProviderKey {
    /// The key in Apple's `.p8` (PKCS #8 PEM), with its 10-character key id and the team's
    /// 10-character id.
    ///
    /// # Errors
    /// [`PushError::ProviderKey`] when `pem` is not a P-256 key in PKCS #8 PEM.
    pub fn from_p8(pem: &str, key_id: &str, team_id: &str) -> Result<Self, PushError> {
        let key = SigningKey::from_pkcs8_pem(pem).map_err(|_pem| PushError::ProviderKey)?;
        Ok(Self { key, key_id: key_id.to_owned(), team_id: team_id.to_owned() })
    }

    /// The token for the half hour `now` (seconds since the Unix epoch) falls in.
    #[must_use]
    pub fn token(&self, now: u64) -> String {
        let iat = now.saturating_sub(now % EPOCH_SECONDS);
        let header = serde_json::json!({ "alg": "ES256", "kid": self.key_id }).to_string();
        let claims = serde_json::json!({ "iss": self.team_id, "iat": iat }).to_string();
        let signed = format!(
            "{}.{}",
            crate::b64::write(header.as_bytes()),
            crate::b64::write(claims.as_bytes())
        );
        let signature: Signature = self.key.sign(signed.as_bytes());
        format!("{signed}.{}", crate::b64::write(&signature.to_bytes()))
    }

    /// The public half, for a test or a stand-in APNs to check a token against.
    #[must_use]
    pub fn verifying_key(&self) -> p256::ecdsa::VerifyingKey {
        *self.key.verifying_key()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use p256::ecdsa::signature::Verifier as _;

    use super::*;

    /// A P-256 key in PKCS #8 PEM, as Apple's `.p8` is; made for these tests only.
    pub const TEST_P8: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgYtl9jDRddV6pwkyq
FIBOteoPkM2+FhjYQLQ0yVx2m8+hRANCAASuVG6o3Y/mV3PK3fUw+B+1OkwmPOuB
O9Cs4mgsPWzy3rCdjz0S5dMeQB6HJKHbFnUVlH91iBv0Iu4rrdBklonV
-----END PRIVATE KEY-----";

    /// What a token says: its header and its claims, read back, and whether `key` signed it.
    pub fn read(
        token: &str,
        key: &p256::ecdsa::VerifyingKey,
    ) -> (serde_json::Value, serde_json::Value, bool) {
        let mut parts = token.split('.');
        let (header, claims, signature) =
            (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        assert!(parts.next().is_none(), "three parts");
        let json = |part: &str| -> serde_json::Value {
            serde_json::from_slice(&crate::b64::read(part).unwrap()).unwrap()
        };
        let signature = Signature::from_slice(&crate::b64::read(signature).unwrap()).unwrap();
        let signed = format!("{header}.{claims}");
        (json(header), json(claims), key.verify(signed.as_bytes(), &signature).is_ok())
    }

    /// A token is ES256 under the key's id, from the team, signed by the key; within a half hour
    /// every one is the same, byte for byte, and the next half hour's is another.
    #[test]
    fn one_token_a_half_hour_the_same_from_every_caller() {
        let key = ProviderKey::from_p8(TEST_P8, "ABC123DEFG", "DEF123GHIJ").unwrap();
        let now = 1_791_200_000;
        let token = key.token(now);
        let (header, claims, signed) = read(&token, &key.verifying_key());
        assert_eq!(header, serde_json::json!({ "alg": "ES256", "kid": "ABC123DEFG" }));
        let iat = claims["iat"].as_u64().unwrap();
        assert_eq!(claims["iss"], "DEF123GHIJ");
        assert!(iat <= now && now - iat < EPOCH_SECONDS && iat % EPOCH_SECONDS == 0, "{iat}");
        assert!(signed, "signed by the team's key");

        let again = ProviderKey::from_p8(TEST_P8, "ABC123DEFG", "DEF123GHIJ").unwrap();
        let later = iat + EPOCH_SECONDS - 1;
        assert_eq!(again.token(later), token, "another caller, later in the half hour");
        assert_ne!(key.token(iat + EPOCH_SECONDS), token, "the next half hour's");
        assert_eq!(ProviderKey::from_p8("not a key", "A", "B").err(), Some(PushError::ProviderKey));
    }
}
