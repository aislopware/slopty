// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Stable conversation links shared by sidebar copy actions and inbound URL routing.

use sha2::{Digest as _, Sha256};
use slopty_view_proto::{Chat, HarnessId, WorkspaceScope};

/// A parsed `slopty://open/chat/…` link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationDeepLink {
    /// The chat it opens.
    pub chat_id: String,
    /// The [`workspace_locator`] of the workspace the chat lives in.
    pub workspace: String,
}

/// A link that opens a chat's conversation in its harness's own app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessConversationLink {
    /// The copy action's words.
    pub label: &'static str,
    /// The harness app's URL.
    pub url: String,
}

/// Opaque locator: enough to reject a link from another device's workspace without putting
/// the device's id in the URL. `None` until the engine has attached and named its device.
pub fn workspace_locator(
    scope: Option<WorkspaceScope>,
    local_device_id: Option<&str>,
) -> Option<String> {
    let scope = scope?;
    let identity = format!("device:{}", local_device_id?);
    let mut hash = Sha256::new();
    hash.update(format!("{scope:?}\0{identity}"));
    let digest = hash.finalize();
    Some(data_encoding::HEXLOWER.encode(digest.first_chunk::<8>()?))
}

/// The `slopty://` link that opens `chat_id` in the workspace `workspace` locates.
pub fn slopty_conversation_link(chat_id: &str, workspace: &str) -> String {
    format!(
        "slopty://open/chat/{}?workspace={}",
        encode_component(chat_id),
        encode_component(workspace)
    )
}

/// Reads a [`slopty_conversation_link`] back, or says why it is not one.
///
/// # Errors
///
/// When `url` is another scheme or path, lacks its workspace locator, or carries a malformed
/// escape or id.
pub fn parse_slopty_conversation_link(url: &str) -> Result<ConversationDeepLink, &'static str> {
    let rest = url.strip_prefix("slopty://open/chat/").ok_or("not a Slopty conversation link")?;
    let (chat_id, query) = rest.split_once('?').ok_or("missing workspace locator")?;
    if chat_id.is_empty() || chat_id.contains('/') {
        return Err("invalid conversation id");
    }
    let workspace = query
        .split('&')
        .find_map(|part| part.strip_prefix("workspace="))
        .ok_or("missing workspace locator")?;
    Ok(ConversationDeepLink {
        chat_id: decode_component(chat_id)?,
        workspace: decode_component(workspace)?,
    })
}

/// Only return schemes verified against the harness app. Hermes exposes a
/// candidate scheme, but its contract is not stable enough to put on users'
/// clipboards yet.
pub fn harness_conversation_link(chat: &Chat) -> Option<HarnessConversationLink> {
    let id = chat.harness_session_id.as_deref()?.trim();
    if id.is_empty() || chat.config.as_ref()?.harness != HarnessId::CODEX {
        return None;
    }
    Some(HarnessConversationLink {
        label: "Codex conversation link",
        url: format!("codex://threads/{}", encode_component(id)),
    })
}

fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            data_encoding::HEXUPPER.encode_append(&[byte], &mut out);
        }
    }
    out
}

fn decode_component(value: &str) -> Result<String, &'static str> {
    let mut out = Vec::with_capacity(value.len());
    let mut rest = value.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        if byte == b'%' {
            out.push(escaped_byte(&mut rest).ok_or("invalid URL escape")?);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out).ok().ok_or("invalid UTF-8 in URL")
}

/// The byte a `%XX` escape names, its two hex digits taken off the front of `rest`.
fn escaped_byte(rest: &mut &[u8]) -> Option<u8> {
    let (hex, tail) = rest.split_at_checked(2)?;
    *rest = tail;
    u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness_chat(harness: HarnessId) -> Chat {
        Chat {
            id: "chat".into(),
            device_id: "device".into(),
            title: None,
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: Some(slopty_view_proto::ChatConfig {
                harness,
                model: None,
                reasoning: None,
                model_options: serde_json::Map::default(),
                sandbox: slopty_view_proto::SandboxLevel::WorkspaceWrite,
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            harness_session_id: Some("thread/one".into()),
            harness_session_cwd: None,
            parent_chat_id: None,
            subagent: false,
            in_parent: false,
            space_id: None,
            last_seen_at: None,
            draft: None,
        }
    }

    #[test]
    fn slopty_link_round_trips_reserved_characters() {
        let link = slopty_conversation_link("chat/with space", "workspace:one");
        assert_eq!(
            parse_slopty_conversation_link(&link).unwrap(),
            ConversationDeepLink {
                chat_id: "chat/with space".into(),
                workspace: "workspace:one".into(),
            }
        );
    }

    #[test]
    fn malformed_or_foreign_links_are_rejected() {
        assert_eq!(
            parse_slopty_conversation_link("https://example.com"),
            Err("not a Slopty conversation link")
        );
        assert_eq!(
            parse_slopty_conversation_link("slopty://open/chat/id"),
            Err("missing workspace locator")
        );
        assert_eq!(
            parse_slopty_conversation_link("slopty://open/chat/%GG?workspace=x"),
            Err("invalid URL escape")
        );
        assert_eq!(
            parse_slopty_conversation_link("slopty://open/chat/id?workspace=%4"),
            Err("invalid URL escape")
        );
        assert_eq!(
            parse_slopty_conversation_link("slopty://open/chat/%FF?workspace=x"),
            Err("invalid UTF-8 in URL")
        );
    }

    #[test]
    fn local_workspace_locator_waits_for_device_identity() {
        assert_eq!(workspace_locator(Some(WorkspaceScope::Local), None), None);
        assert_eq!(workspace_locator(None, Some("device-a")), None);
        let first = workspace_locator(Some(WorkspaceScope::Local), Some("device-a"));
        let second = workspace_locator(Some(WorkspaceScope::Local), Some("device-b"));
        assert!(first.is_some());
        assert_ne!(first, second);
    }

    #[test]
    fn codex_link_is_exact_and_unverified_harnesses_are_omitted() {
        assert_eq!(
            harness_conversation_link(&harness_chat(HarnessId::CODEX)).unwrap().url,
            "codex://threads/thread%2Fone"
        );
        assert!(harness_conversation_link(&harness_chat(HarnessId::HERMES)).is_none());
    }
}
