// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The pending-message queue on a session doc.
//!
//! What you typed while the agent was busy, held where every device can see it
//! (and edit it) until the host has somewhere to put it. Distinct from the
//! `commands` ledger next door: commands are append-only and immutable by
//! design (rule 1), and a queue whose rows can be retyped, reordered and
//! dropped is the opposite of that. So it gets its own container.
//!
//! `queue` is a **LoroMovableList** rather than the plain `LoroList` the rest
//! of the doc uses. A plain list has no move: reordering means delete+insert,
//! which two devices doing it at once resolve into duplicated or lost rows.
//! MovableList carries a real move op, so concurrent reorders converge on one
//! order with every row still present exactly once.
//!
//! Writers: any device (unlike `messages`, which is host-only). The host is
//! the only one that *takes* from the queue — see `DocHost::drain_queue`.

// slopty: the Loro queue container is cut; the queued message types stay.
use serde::{Deserialize, Serialize};


/// One unsent message waiting its turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedMessage {
    /// Stable through promotion: the host uses this as the transcript user
    /// message id when the row is finally dispatched.
    pub id: String,
    /// What the user typed. Never empty — emptying it deletes the row.
    pub text: String,
    /// Committed upload paths, staged at queue time so the row never points at
    /// files that only exist on the device that typed it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    /// Do not automatically steer this row into a live turn. The row remains
    /// visible until turn end or an explicit Steer now / Send now action.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hold_for_turn_end: bool,
    /// Device that queued it.
    pub issued_by: String,
    /// Epoch millis.
    pub issued_at: i64,
    /// Epoch millis of the last text edit, when there has been one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_at: Option<i64>,
    /// Host-authoritative barrier preventing this row from reaching the
    /// agent while a client has an edit open. Expired edits fail closed into
    /// `ReviewRequired`; they never silently become sendable again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_gate: Option<QueueDeliveryGate>,
}

/// Why a queued row is not currently eligible for automatic or explicit
/// delivery. Kept on the row so moves preserve it and deleting the row cannot
/// leave an orphaned lease behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum QueueDeliveryGate {
    Editing {
        lease_id: String,
        owner_device_id: String,
        owner_instance_id: String,
        acquired_at_ms: i64,
        expires_at_ms: i64,
        base_text_hash: String,
    },
    ReviewRequired {
        previous_lease_id: String,
        owner_device_id: String,
        since_ms: i64,
        base_text_hash: String,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl QueuedMessage {
    pub fn new(
        id: impl Into<String>,
        text: impl Into<String>,
        issued_by: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            attachments: Vec::new(),
            hold_for_turn_end: false,
            issued_by: issued_by.into(),
            issued_at: 0,
            edited_at: None,
            delivery_gate: None,
        }
    }
}
