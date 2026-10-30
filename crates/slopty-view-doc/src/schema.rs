// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! A transcript's entry: a message of the person's, or an answer of the agent's with its parts,
//! as the view model's transcript watch (`WatchDocMessages`) carries it.

use serde::{Deserialize, Serialize};

use crate::parts::{MessagePart, MessageStatus};

/// Who an entry is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// The person.
    User,
    /// The agent.
    Assistant,
    /// Slopty or the harness, about the session.
    System,
}

/// One entry of a transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    /// Its id.
    pub id: String,
    /// Who it is from.
    pub role: MessageRole,
    /// What it says, in order.
    pub parts: Vec<MessagePart>,
    /// Epoch millis.
    pub created_at: i64,
    /// The machine it came from.
    pub device_id: String,
    /// How far it is, for the agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
    /// The entry it continues, when a long one was split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_of: Option<String>,
    /// Wall-clock length of this assistant turn, stamped when the segment
    /// finishes. Absent on user rows and live streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}
