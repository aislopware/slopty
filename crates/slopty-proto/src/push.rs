//! The push path to a pocketed phone (`docs/decisions/platform.md`, `slopty-push`).
//!
//! A phone gives the server its device token and the public half of a key only it holds
//! ([`PushDevice`]). When a notice finds the person at no client and the phone not listening,
//! the server seals the notice to that key ([`PushBody`]) and sends it through the relay to
//! APNs, which carries ciphertext. The phone's notification extension opens it and shows what a
//! linked client would have posted.

use serde::{Deserialize, Serialize};
use slopty_core::ClientId;

use crate::thread::AskId;
use crate::thread::attention::Notice;

/// A phone the server may push to, as it tells the server on every link and whenever its token
/// changes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PushDevice {
    /// The client, by the identity it keeps, so the phone is known after its link is gone.
    pub client: ClientId,
    /// Its device token from APNs, in hex.
    pub token: String,
    /// The public half of its X25519 key, which bodies are sealed to.
    pub key: [u8; 32],
    /// A development build's token, which only APNs' sandbox knows.
    pub sandbox: bool,
    /// The app's bundle identifier, APNs' topic.
    pub topic: String,
    /// How long a turn runs before its end is worth a word on this phone, in milliseconds: a
    /// shorter finished turn is not pushed, as a linked phone would not post it.
    pub quiet_ms: u64,
}

/// What is sealed to the phone: the server's notice, as a linked client hears it, and the
/// request its note's Allow and Deny answer, when it asks a yes or no they can.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PushBody {
    /// The notice.
    pub notice: Notice,
    /// The request a note's buttons answer, on the notice's thread.
    pub ask: Option<AskId>,
}
