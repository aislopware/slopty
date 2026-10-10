//! The push path to a pocketed phone (`docs/decisions/platform.md`, `slopty-push`).
//!
//! A phone gives the server its device token and the public half of a key only it holds
//! ([`PushDevice`]). When a notice finds the person at no client and the phone not listening,
//! the server seals the notice to that key ([`PushBody`]) and sends it through the relay to
//! APNs, which carries ciphertext. The phone's notification extension opens it and shows what a
//! linked client would have posted.

use serde::{Deserialize, Serialize};

use crate::thread::AskId;
use crate::thread::attention::Notice;
use crate::thread::wire::NoteChoice;

/// A phone the server may push to, as it tells the server on every link and whenever its token
/// changes ([`crate::server::ToServer::PushDevice`], which names the client).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PushDevice {
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

/// What is sealed to the phone: the server's notice, and what its note's buttons answer.
///
/// The notice is as a linked client hears it. The buttons are Allow and Deny on a yes or no
/// they can answer, or a question's options ([`Self::choices`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PushBody {
    /// The notice.
    pub notice: Notice,
    /// The request a note's buttons answer, on the notice's thread.
    pub ask: Option<AskId>,
    /// The answers the note offers as buttons of their own, each answering `ask` with its
    /// choice ([`RequestCard::buttons`](crate::thread::wire::RequestCard::buttons)): a small
    /// question's options. Empty, the buttons are Allow and Deny when there is an `ask`.
    pub choices: Vec<NoteChoice>,
    /// The note is already on the phone and only what its buttons answer moved (a request
    /// opened after the thread came to need the person, or the one it showed went): it
    /// replaces that note under the same collapse id without a sound or a banner.
    pub quiet: bool,
    /// The task a ready-to-merge note's Merge button merges, the oldest ready, in the notice's
    /// project ([`crate::thread::attention::NoticeKind::ReadyToMerge`]).
    pub merges: Option<crate::project::TaskId>,
}
