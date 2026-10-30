// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! Session doc schema over `loro` — Rust port of `packages/session-doc/src/schema.ts`.
//!
//! Container layout (MUST stay shape-compatible with the TS edge/tail materializer):
//! - `meta`:     LoroMap  { chatId: string, schemaVersion: number }         (host-only writer)
//! - `messages`: LoroList of LoroMap {
//!   id, role, parts: LoroList<part map>, createdAt, deviceId, status?,
//!   continuationOf?, durationMs? }
//! - `commands`: LoroList of LoroMap {
//!   id, kind, payload(json), issuedBy, issuedAt, basedOn?, expiresAt?, status, resolution? }
//! - `queue`:    LoroMovableList of LoroMap {
//!   id, text, attachments?, issuedBy, issuedAt, editedAt? }        (any device writes)
//!
//! Part maps: { id, kind: "text"|"reasoning"|"tool"|"input"|"error"|"image", text?: LoroText,
//! reasoning?: LoroText, call?: json, isError?, questions?: json, resolved?, message? }.
//! Text bodies are **LoroText** so streaming appends RLE-merge (1.03x oplog overhead vs
//! 125x for whole-value rewrites).

use serde::{Deserialize, Serialize};

use crate::parts::{MessagePart, MessageStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// One entry in the doc's `messages` list (`SessionMessageEntry` in TS).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessageEntry {
    pub id: String,
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
    /// Epoch millis.
    pub created_at: i64,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MessageStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_of: Option<String>,
    /// Wall-clock length of this assistant turn, stamped when the segment
    /// finishes. Absent on user rows, live streams, and docs written before
    /// the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}
