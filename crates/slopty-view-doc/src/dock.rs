//! slopty: what a session says beside its terminal, read-only: the inspector reads it (Slopty's
//! own; not in zeron).
//!
//! The view model serves it on `slopty.WatchDock` ([`crate::calls::WATCH_DOCK`]) for one chat:
//! one [`ChatDock`] each time anything in it changes. It mirrors the agent's own prompt box as
//! its TUI holds it ([`PromptBox`]). Nothing in it asks the agent anything, and nothing of it is
//! ever typed back: the person writes in the TUI (`docs/decisions/agents.md`, "TUI-first").
//!
//! It also keeps the shapes the transcript's own notes and retries are drawn with
//! ([`RetryLine`], [`NoteLine`]).

use serde::{Deserialize, Serialize};

/// A chat's dock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDock {
    /// The agent's prompt box, while it holds anything: a draft, a pasted picture, a prompt
    /// queued behind the turn under way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptBox>,
}

/// The agent's prompt box as its TUI holds it now, mirrored live.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptBox {
    /// The prompts queued behind the turn under way, in the order they go.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<String>,
    /// The draft as the box shows it: a pasted picture as its placeholder (`[Image #2]`), a
    /// long paste as its own (`[Pasted text #1 +5 lines]`). Empty while the box holds none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// The pictures pasted into the draft, in the order their placeholders stand in it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PromptImage>,
}

impl PromptBox {
    /// Whether it holds nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty() && self.text.trim().is_empty() && self.images.is_empty()
    }
}

/// A picture pasted into the prompt box.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptImage {
    /// Its number in the box: the `N` of its `[Image #N]`.
    pub number: u32,
    /// Where its bytes are read back from (`ReadAttachmentChunk`); a new picture is a new path.
    pub path: String,
    /// Its media type (`image/png`).
    pub mime_type: String,
}

/// A request the agent tries again, as the line under way says it: "API error · Retrying in 5s ·
/// attempt 2/10".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryLine {
    /// The attempt that comes next, from 1.
    pub attempt: u32,
    /// How many it makes before it gives up, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    /// When the next attempt goes, when it says: the countdown runs to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    /// What failed, in a line ("API error", "Overloaded", "Session limit reached"): the TUI's
    /// first words, from the third attempt the error itself.
    pub what: String,
    /// The status page to look at when it persists, from the third attempt of an overload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl RetryLine {
    /// The line as it reads with `in_ms` left before the next attempt: "API error · Retrying in
    /// 5s · attempt 2/10".
    #[must_use]
    pub fn words(&self, in_ms: Option<u64>) -> String {
        let mut out = self.what.clone();
        match in_ms {
            Some(ms) if ms >= 1_000 => {
                out.push_str(&format!(" · Retrying in {}", crate::work::duration_words(ms)));
            }
            _ => out.push_str(" · Retrying now"),
        }
        match self.max {
            Some(max) => out.push_str(&format!(" · attempt {}/{max}", self.attempt)),
            None => out.push_str(&format!(" · attempt {}", self.attempt)),
        }
        out
    }
}

/// A passing notification of the agent's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteLine {
    /// Its key: a later one of the same key took the place of any before.
    pub key: String,
    /// What it says.
    pub text: String,
    /// How it is drawn: [`NoteLine::INFO`], [`NoteLine::WARNING`], [`NoteLine::ERROR`].
    pub tone: String,
    /// The agent's own priority (`low`, `medium`, `high`, `immediate`): an immediate one may
    /// toast.
    pub priority: String,
    /// When it came.
    pub at: i64,
    /// When it stops showing, when the agent says; the shell hides it then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

impl NoteLine {
    /// Something gone wrong.
    pub const ERROR: &'static str = "error";
    /// Plain news.
    pub const INFO: &'static str = "info";
    /// Worth a look.
    pub const WARNING: &'static str = "warning";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retry counts down to its next attempt, out of the most it makes when it says.
    #[test]
    fn a_retry_reads_its_countdown_and_attempt() {
        let retry =
            RetryLine { attempt: 2, max: Some(10), what: "API error".into(), ..RetryLine::default() };
        assert_eq!(retry.words(Some(5_000)), "API error · Retrying in 5s · attempt 2/10");
        assert_eq!(retry.words(None), "API error · Retrying now · attempt 2/10");
    }

    /// A box with nothing but blanks in it shows nothing; a queued prompt or a picture alone is
    /// something.
    #[test]
    fn an_empty_box_shows_nothing() {
        assert!(PromptBox { text: "  \n".into(), ..PromptBox::default() }.is_empty());
        assert!(!PromptBox { queued: vec!["next".into()], ..PromptBox::default() }.is_empty());
        let image = PromptImage { number: 1, ..PromptImage::default() };
        assert!(!PromptBox { images: vec![image], ..PromptBox::default() }.is_empty());
    }
}
