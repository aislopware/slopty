// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
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

// slopty: the Loro document layer is cut; the entry types and their JSON decode stay.
use serde::{Deserialize, Serialize};

// slopty: the command and tail constants served only the cut document writer.
use crate::parts::{MessagePart, MessageStatus, SubagentStatus};

#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error("schema: {0}")]
    Schema(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

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

/// The doc-resident flat part map (`DocMessagePart` in TS). Distinct from the app-layer
/// [`MessagePart`]: input parts key on their request id, error parts store `message`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocPartJson {
    id: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    /// Thinking body for `kind: "reasoning"` (additive). Deliberately NOT the
    /// `text` field: old readers' unknown-kind fallback renders `text` as
    /// prose, so reasoning riding `text` would leak raw thinking into old
    /// transcripts; on this field they degrade to an invisible empty part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    questions: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resolved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// Fork seam (`kind: "fork"`, additive): the chat the history above was
    /// copied from, and its title at the time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_title: Option<String>,
    /// Tool output summary (additive — absent on old rows and old writers;
    /// pre-strip writers stored up to 4KB of capped output here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<String>,
    /// Capped inline tool diff (additive; pre-strip writers only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff: Option<serde_json::Value>,
    /// Sidecar key of the full output (additive, docs/chat2-sync.md A1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_ref: Option<String>,
    /// Full-output byte length (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_bytes: Option<u64>,
    /// Sidecar key of the full diff JSON (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff_ref: Option<String>,
    /// Per-file diff stats (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diff_stats: Option<serde_json::Value>,
    /// Subagent doc/blob ref carried by a spawn chip (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagent_ref: Option<String>,
    /// Subagent lifecycle ("running"/"done"/"failed", additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagent_status: Option<String>,
    /// One-line live tail of the subagent's output (additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagent_tail: Option<String>,
}

/// App parts → doc part json (mirror of `toDocParts`).
#[allow(dead_code)] // slopty: its writer, `SessionDoc`, is cut.
fn to_doc_part(part: &MessagePart) -> Result<DocPartJson, DocError> {
    Ok(match part {
        MessagePart::Image {
            id,
            path,
            name,
            mime_type,
        } => DocPartJson {
            id: id.clone(),
            kind: "image".into(),
            path: Some(path.clone()),
            name: Some(name.clone()),
            mime_type: Some(mime_type.clone()),
            ..Default::default()
        },
        MessagePart::Text { id, text } => DocPartJson {
            id: id.clone(),
            kind: "text".into(),
            text: Some(text.clone()),
            ..Default::default()
        },
        MessagePart::Reasoning { id, text } => DocPartJson {
            id: id.clone(),
            kind: "reasoning".into(),
            reasoning: Some(text.clone()),
            ..Default::default()
        },
        MessagePart::Tool {
            id,
            call,
            is_error,
            resolved,
            output,
            diff,
            output_ref,
            output_bytes,
            diff_ref,
            diff_stats,
            subagent_ref,
            subagent_status,
            subagent_tail,
        } => DocPartJson {
            id: id.clone(),
            kind: "tool".into(),
            call: Some(serde_json::to_value(call)?),
            // TS shape parity: `isError` is written only once the tool result arrived;
            // its presence IS the resolution marker.
            is_error: if *resolved { Some(*is_error) } else { None },
            output: output.clone(),
            diff: diff.as_ref().map(serde_json::to_value).transpose()?,
            output_ref: output_ref.clone(),
            output_bytes: *output_bytes,
            diff_ref: diff_ref.clone(),
            diff_stats: diff_stats.as_ref().map(serde_json::to_value).transpose()?,
            subagent_ref: subagent_ref.clone(),
            subagent_status: subagent_status.map(|s| {
                match s {
                    SubagentStatus::Running => "running",
                    SubagentStatus::Done => "done",
                    SubagentStatus::Failed => "failed",
                }
                .to_owned()
            }),
            subagent_tail: subagent_tail.clone(),
            ..Default::default()
        },
        MessagePart::Input {
            id: _,
            request_id,
            questions,
            resolved,
        } => DocPartJson {
            id: request_id.clone(),
            kind: "input".into(),
            questions: Some(serde_json::to_value(questions)?),
            resolved: Some(*resolved),
            ..Default::default()
        },
        MessagePart::Error { id, message } => DocPartJson {
            id: id.clone(),
            kind: "error".into(),
            message: Some(message.clone()),
            ..Default::default()
        },
        MessagePart::Fork {
            id,
            source_chat_id,
            source_title,
        } => DocPartJson {
            id: id.clone(),
            kind: "fork".into(),
            source_chat_id: Some(source_chat_id.clone()),
            source_title: Some(source_title.clone()),
            ..Default::default()
        },
    })
}

/// Doc part json → app part (mirror of `fromDocParts`; malformed degrades to empty text).
fn from_doc_part(p: DocPartJson) -> MessagePart {
    match p.kind.as_str() {
        "image" => image_part(p.id, p.path, p.name, p.mime_type),
        "tool" => match p.call.and_then(|c| serde_json::from_value(c).ok()) {
            Some(call) => MessagePart::Tool {
                id: p.id,
                call,
                is_error: p.is_error.unwrap_or(false),
                resolved: p.is_error.is_some(),
                output: p.output,
                diff: p.diff.and_then(|d| serde_json::from_value(d).ok()),
                output_ref: p.output_ref,
                output_bytes: p.output_bytes,
                diff_ref: p.diff_ref,
                diff_stats: p.diff_stats.and_then(|s| serde_json::from_value(s).ok()),
                subagent_ref: p.subagent_ref,
                subagent_status: p.subagent_status.as_deref().and_then(|s| match s {
                    "running" => Some(SubagentStatus::Running),
                    "done" => Some(SubagentStatus::Done),
                    "failed" => Some(SubagentStatus::Failed),
                    _ => None,
                }),
                subagent_tail: p.subagent_tail,
            },
            None => MessagePart::Text {
                id: p.id,
                text: String::new(),
            },
        },
        "input" => MessagePart::Input {
            id: p.id.clone(),
            request_id: p.id,
            questions: p
                .questions
                .and_then(|q| serde_json::from_value(q).ok())
                .unwrap_or_default(),
            resolved: p.resolved.unwrap_or(false),
        },
        "error" => MessagePart::Error {
            id: p.id,
            message: p.message.unwrap_or_default(),
        },
        "reasoning" => MessagePart::Reasoning {
            id: p.id,
            text: p.reasoning.unwrap_or_default(),
        },
        "fork" => MessagePart::Fork {
            id: p.id,
            source_chat_id: p.source_chat_id.unwrap_or_default(),
            source_title: p.source_title.unwrap_or_default(),
        },
        _ => MessagePart::Text {
            id: p.id,
            text: p.text.unwrap_or_default(),
        },
    }
}

fn image_part(
    id: String,
    path: Option<String>,
    name: Option<String>,
    mime_type: Option<String>,
) -> MessagePart {
    match (path, name, mime_type) {
        (Some(path), Some(name), Some(mime_type))
            if !path.is_empty()
                && !name.is_empty()
                && matches!(
                    mime_type.as_str(),
                    "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                ) =>
        {
            MessagePart::Image {
                id,
                path,
                name,
                mime_type,
            }
        }
        _ => MessagePart::Error {
            id,
            message: "Generated image unavailable".into(),
        },
    }
}

/// Decode ONE `messages` entry map's deep JSON value (same strict-then-salvage
/// policy as [`SessionDoc::read_entries`]). The per-entry seam for viewers
/// that keep an incremental transcript mirror: they re-decode only the entry
/// maps a commit touched instead of materializing the whole list.
pub fn decode_entry_json(v: serde_json::Value) -> Result<SessionMessageEntry, DocError> {
    entry_from_json(v)
}

fn entry_from_json(v: serde_json::Value) -> Result<SessionMessageEntry, DocError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawEntry {
        id: String,
        role: MessageRole,
        #[serde(default)]
        parts: Vec<DocPartJson>,
        created_at: i64,
        device_id: String,
        #[serde(default)]
        status: Option<MessageStatus>,
        #[serde(default)]
        continuation_of: Option<String>,
        #[serde(default)]
        duration_ms: Option<i64>,
    }
    match serde_json::from_value::<RawEntry>(v.clone()) {
        Ok(raw) => Ok(SessionMessageEntry {
            id: raw.id,
            role: raw.role,
            parts: raw.parts.into_iter().map(from_doc_part).collect(),
            created_at: raw.created_at,
            device_id: raw.device_id,
            status: raw.status,
            continuation_of: raw.continuation_of,
            duration_ms: raw.duration_ms,
        }),
        // 2026-08-10 incident rule: a missing field must cost AT MOST what
        // the field carried — never the entry, never the transcript. Rooms
        // merge writes from every device and app version; one bad writer
        // (or one mangled export) blanking whole sessions for every reader
        // is exactly what tonight looked like.
        Err(strict_err) => salvage_entry(v, strict_err),
    }
}

/// Field-level salvage for entries the strict shape rejects. Missing
/// identity/attribution fields get deterministic stand-ins (content-hashed
/// id, so repeated reads and continuation joins stay stable); parts are
/// salvaged individually — a part missing `kind` is inferred from its
/// content shape, and only truly contentless parts are dropped.
fn salvage_entry(
    v: serde_json::Value,
    strict_err: serde_json::Error,
) -> Result<SessionMessageEntry, DocError> {
    let Some(obj) = v.as_object() else {
        return Err(DocError::Json(strict_err));
    };
    let stable_hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        v.to_string().hash(&mut hasher);
        hasher.finish()
    };
    let str_field = |key: &str| obj.get(key).and_then(|x| x.as_str()).map(str::to_owned);
    let id = str_field("id").unwrap_or_else(|| format!("recovered-{stable_hash:016x}"));
    let role = obj
        .get("role")
        .and_then(|r| serde_json::from_value::<MessageRole>(r.clone()).ok())
        .unwrap_or(MessageRole::Assistant);
    let mut parts = Vec::new();
    let mut dropped_parts = 0usize;
    if let Some(raw_parts) = obj.get("parts").and_then(|p| p.as_array()) {
        for (ix, part) in raw_parts.iter().enumerate() {
            match serde_json::from_value::<DocPartJson>(part.clone()) {
                Ok(p) => parts.push(from_doc_part(p)),
                Err(_) => match salvage_part(part, &id, ix) {
                    Some(p) => parts.push(p),
                    None => dropped_parts += 1,
                },
            }
        }
    }
    tracing::warn!(
        entry = %id,
        error = %strict_err,
        salvaged_parts = parts.len(),
        dropped_parts,
        "transcript entry failed strict parse; salvaged"
    );
    Ok(SessionMessageEntry {
        id,
        role,
        parts,
        created_at: obj.get("createdAt").and_then(|x| x.as_i64()).unwrap_or(0),
        device_id: str_field("deviceId").unwrap_or_default(),
        status: obj
            .get("status")
            .and_then(|s| serde_json::from_value(s.clone()).ok()),
        continuation_of: str_field("continuationOf"),
        duration_ms: obj.get("durationMs").and_then(|x| x.as_i64()),
    })
}

/// Salvage one part whose strict `DocPartJson` parse failed: infer the kind
/// from the content shape (`text` → text part, parseable `call` → tool
/// part). `None` only when nothing renderable survives.
fn salvage_part(part: &serde_json::Value, entry_id: &str, ix: usize) -> Option<MessagePart> {
    let obj = part.as_object()?;
    let id = obj
        .get("id")
        .and_then(|x| x.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{entry_id}#recovered-{ix}"));
    if obj.get("kind").and_then(|v| v.as_str()) == Some("image") {
        let field = |key| obj.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        return Some(image_part(
            id,
            field("path"),
            field("name"),
            field("mimeType"),
        ));
    }
    if let Some(reasoning) = obj.get("reasoning").and_then(|x| x.as_str()) {
        return Some(MessagePart::Reasoning {
            id,
            text: reasoning.to_owned(),
        });
    }
    if let Some(text) = obj.get("text").and_then(|x| x.as_str()) {
        return Some(MessagePart::Text {
            id,
            text: text.to_owned(),
        });
    }
    if let Some(call) = obj
        .get("call")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
    {
        return Some(MessagePart::Tool {
            id,
            call,
            is_error: obj
                .get("isError")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            resolved: obj
                .get("resolved")
                .and_then(|x| x.as_bool())
                .unwrap_or(true),
            output: obj
                .get("output")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
        });
    }
    if let Some(message) = obj.get("message").and_then(|x| x.as_str()) {
        return Some(MessagePart::Error {
            id,
            message: message.to_owned(),
        });
    }
    None
}

/// Render-time continuation join at the entry level (`joinContinuations` in TS):
/// concatenate continuation entries' parts onto their root, in list order.
pub fn join_continuation_entries(entries: Vec<SessionMessageEntry>) -> Vec<SessionMessageEntry> {
    if !entries.iter().any(|e| e.continuation_of.is_some()) {
        return entries;
    }
    let mut out: Vec<SessionMessageEntry> = Vec::with_capacity(entries.len());
    let mut root_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for entry in entries {
        match &entry.continuation_of {
            Some(root_id) => {
                if let Some(&at) = root_index.get(root_id) {
                    out[at].parts.extend(entry.parts);
                    if entry.duration_ms.is_some() {
                        out[at].duration_ms = entry.duration_ms;
                    }
                } else {
                    // Orphan continuation — surface as its own entry rather than dropping.
                    out.push(entry);
                }
            }
            None => {
                root_index.insert(entry.id.clone(), out.len());
                out.push(entry);
            }
        }
    }
    out
}

// slopty: the streaming segment writer, the tail materializer and the Loro-backed tests are
// cut with the document layer.
