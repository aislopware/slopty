//! The push path to a pocketed phone, where nothing of Slopty runs: what the server seals to the
//! phone, the request it hands APNs, and the token APNs takes from a provider
//! (`.research/push-2026-10-06.md`, `docs/decisions/platform.md`).
//!
//! Pure: no sockets and no clock. The server and the phone's notification extension each bring
//! their own I/O and their own `now`.
//!
//! - [`seal`]: the note's words, sealed to a key only the phone holds (HPKE, RFC 9180), the device
//!   token bound in, so APNs carries ciphertext and nothing else.
//! - [`apns`]: the request APNs takes, its generic alert the server's fixed words, and what its
//!   answer means.
//! - [`provider`]: the provider token, one per half hour, the same from every caller.

mod b64;

pub mod apns;
pub mod provider;
pub mod seal;

/// Why a push could not be made, sealed or opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PushError {
    /// The system's random source failed.
    #[error("the system's random source failed")]
    Random,
    /// A key was not one: the wrong length or not on its curve.
    #[error("not a key")]
    Key,
    /// The body could not be sealed.
    #[error("the body could not be sealed")]
    Seal,
    /// The sealed body does not open with this key, for this device: another key, another
    /// device, or a changed byte.
    #[error("the body does not open with this key")]
    Open,
    /// The provider key is not a P-256 key in PKCS #8 PEM, as Apple's `.p8` is.
    #[error("not an APNs key")]
    ProviderKey,
}
