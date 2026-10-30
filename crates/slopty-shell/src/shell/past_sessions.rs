//! slopty: Claude Code's past sessions, found and taken up again (`docs/decisions/agents.md`,
//! "Past Claude Code sessions are seen and continued in the GUI").
//!
//! - **Finding** one is typing in the session picker (`picker`, ⌘K): every project's machine is
//!   asked for the Claude Code sessions kept in its folders (the TUI's and `claude -p`'s alike)
//!   whose prompts hold the words, each named by its first prompt, the prompt that matched lit.
//! - **Opening** a session the machine keeps a chat of opens that chat; any other is taken up by
//!   Claude Code's own `--resume` in its folder, and the chat opens with its whole conversation
//!   once its machine has started it. Its machine's words say why one does not open. An ended
//!   session's Resume takes it up the same way.
//!
//! The view model answers both calls (`slopty_ui`'s `view_model::past`) from the session's own
//! machine, so a remote project reads as a local one.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, Task, div};
use serde::Deserialize;

use super::Shell;
use crate::popover;
use crate::theme::Theme;

/// What the shell asks the view model.
pub(super) mod calls {
    pub(in crate::shell) const LIST: &str = "slopty.PastSessions";
    pub(in crate::shell) const OPEN: &str = "slopty.OpenPastSession";
}

/// What a past session's row is called before its title, for a screen reader.
pub(super) const PAST_SESSION: &str = "Past session:";
/// How long a session taken up is waited for in the sidebar's list.
const CHAT_WITHIN: Duration = Duration::from_secs(10);
/// How often it is looked for meanwhile.
const CHAT_POLL: Duration = Duration::from_millis(40);

/// A prompt as the machine found it: its words, where the asked words are in it, and whether
/// it was cut.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PromptLine {
    pub(super) text: String,
    pub(super) spans: Vec<[u32; 2]>,
    pub(super) cut_before: bool,
    pub(super) cut_after: bool,
}

/// One past session, as the view model lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct PastRow {
    /// Its machine and id, unique across machines.
    pub(super) key: String,
    pub(super) device_id: String,
    pub(super) device: Option<String>,
    pub(super) native: String,
    /// Its first prompt, when the person typed one.
    pub(super) title: Option<String>,
    /// The prompt that matched the words, else its last prompt.
    pub(super) prompt: Option<PromptLine>,
    pub(super) cwd: Option<String>,
    pub(super) space_id: Option<String>,
    /// When it last changed, in ms since the epoch.
    pub(super) updated_at: Option<i64>,
    pub(super) branch: Option<String>,
    /// How a live Claude Code elsewhere holds it, when one does.
    pub(super) running: Option<String>,
    /// The chat it already is here.
    pub(super) chat_id: Option<String>,
    /// The words that take it up again.
    pub(super) resume: Vec<String>,
}

impl PastRow {
    /// What it is called: its first prompt, else its last, else its id.
    pub(super) fn label(&self) -> String {
        let said = self
            .title
            .as_deref()
            .or(self.prompt.as_ref().map(|p| p.text.as_str()))
            .map(crate::transcript::single_line)
            .filter(|t| !t.trim().is_empty());
        said.unwrap_or_else(|| {
            format!("Session {}", self.native.chars().take(8).collect::<String>())
        })
    }

    /// When it last changed, where known.
    pub(super) fn updated(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        chrono::DateTime::from_timestamp_millis(self.updated_at?)
    }

    /// What its time slot says while a live Claude Code elsewhere holds it.
    pub(super) fn running_words(&self) -> Option<&'static str> {
        self.running.as_deref().map(|how| match how {
            "interactive" => "In a terminal",
            _ => "Running",
        })
    }

    /// Why it cannot be taken up now, in words, while a live Claude Code elsewhere holds it.
    pub(super) fn held_elsewhere(&self) -> Option<String> {
        let how = self.running.as_deref()?;
        let place = if how == "interactive" { "in a terminal" } else { "in the background" };
        Some(format!(
            "Claude Code is running this session {place} on {}. Close it there to continue it here.",
            self.device.as_deref().filter(|d| !d.is_empty()).unwrap_or("its machine")
        ))
    }
}

/// A machine that could not answer, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Failure {
    pub(super) device: Option<String>,
    pub(super) words: String,
}

/// What the view model answered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Listed {
    pub(super) sessions: Vec<PastRow>,
    pub(super) failures: Vec<Failure>,
    pub(super) cut: Option<String>,
}

/// The past sessions' state, one per shell.
#[derive(Default)]
pub(super) struct PastSessions {
    /// The session being opened, by its key, and the open on its way.
    opening: Option<(String, Task<()>)>,
}

impl PastSessions {
    fn opening(&self, key: &str) -> bool {
        self.opening.as_ref().is_some_and(|(k, _)| k == key)
    }
}

impl Shell {
    /// Open `row`: the chat it already is, else Claude Code takes it up again on its machine and
    /// its chat opens once started. Why it does not open is said in its machine's words.
    pub(super) fn open_past_session(&mut self, row: PastRow, cx: &mut Context<Self>) {
        if let Some(chat) = row.chat_id.clone() {
            self.open_chat(chat, cx);
            return;
        }
        if self.past_sessions.opening(&row.key) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            self.show_notice(crate::toast::ToastKind::Error, "Couldn\u{2019}t reach the Slopty engine. Wait for it to connect, then try again.", cx);
            return;
        };
        let params = serde_json::json!({
            "deviceId": row.device_id,
            "cwd": row.cwd,
            "resume": row.resume,
        });
        let task = cx.spawn(async move |this, cx| {
            let opened = engine.client().call(calls::OPEN, params).await;
            let chat = match opened {
                Ok(answer) => answer.get("chatId").and_then(|c| c.as_str()).map(str::to_owned),
                Err(error) => {
                    let _gone = this.update(cx, |this, cx| this.past_refused(&error.to_string(), cx));
                    return;
                }
            };
            let Some(chat) = chat else {
                let _gone =
                    this.update(cx, |this, cx| this.past_refused("It did not say its chat", cx));
                return;
            };
            // The chat opens once the sidebar's list has it.
            let since = cx.background_executor().now();
            loop {
                let listed = this
                    .update(cx, |this, cx| this.state.read(cx).chats.iter().any(|c| c.id == chat))
                    .unwrap_or(true);
                if listed || cx.background_executor().now().saturating_duration_since(since) >= CHAT_WITHIN {
                    break;
                }
                cx.background_executor().timer(CHAT_POLL).await;
            }
            let _gone = this.update(cx, |this, cx| {
                this.past_sessions.opening = None;
                // slopty: its terminal shows once its machine names it.
                this.mark_starting(chat.clone(), cx);
                this.open_chat(chat, cx);
            });
        });
        self.past_sessions.opening = Some((row.key, task));
        cx.notify();
    }

    /// slopty: resume ended session `chat` in a new terminal, by Claude Code's own `--resume`.
    pub(super) fn resume_ended_session(&mut self, chat: &slopty_view_proto::Chat, cx: &mut Context<Self>) {
        let Some(native) = chat.harness_session_id.clone().filter(|id| !id.is_empty()) else {
            return;
        };
        let row = PastRow {
            key: format!("{}:{native}", chat.device_id),
            device_id: chat.device_id.clone(),
            cwd: chat.cwd.clone(),
            resume: vec!["--resume".to_owned(), native.clone()],
            native,
            ..PastRow::default()
        };
        self.open_past_session(row, cx);
    }

    /// slopty: whether ended session `chat` is being resumed now.
    pub(super) fn resuming(&self, chat: &slopty_view_proto::Chat) -> bool {
        chat.harness_session_id
            .as_deref()
            .is_some_and(|native| self.past_sessions.opening(&format!("{}:{native}", chat.device_id)))
    }

    /// The session `key` did not open, for `why`: said in a notice.
    fn past_refused(&mut self, why: &str, cx: &mut Context<Self>) {
        self.past_sessions.opening = None;
        self.show_error_detail("Couldn\u{2019}t open the Claude Code session. Try again.", why.to_owned(), cx);
        cx.notify();
    }
}

/// A prompt on one line with the asked words lit: `…` where it was cut.
pub(super) fn prompt_text(prompt: &PromptLine, theme: &Theme) -> AnyElement {
    let (line, ranges) = prompt_line(prompt);
    div()
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_faint)
        .child(popover::search_highlight_ranges(line.into(), ranges, theme))
        .into_any_element()
}

/// `prompt` on one line, `…` before and after where it was cut, and where the words are in it.
/// A line break becomes a space, byte for byte, so the spans still land; a span off the text's
/// character boundaries is dropped.
pub(super) fn prompt_line(prompt: &PromptLine) -> (String, Vec<std::ops::Range<usize>>) {
    const ELLIPSIS: &str = "\u{2026}";
    let lead = if prompt.cut_before { ELLIPSIS.len() } else { 0 };
    let mut line = String::with_capacity(prompt.text.len() + 2 * ELLIPSIS.len());
    if prompt.cut_before {
        line.push_str(ELLIPSIS);
    }
    line.extend(prompt.text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }));
    if prompt.cut_after {
        line.push_str(ELLIPSIS);
    }
    let ranges = prompt
        .spans
        .iter()
        .filter_map(|[start, end]| {
            let (start, end) = (usize::try_from(*start).ok()?, usize::try_from(*end).ok()?);
            (start < end
                && end <= prompt.text.len()
                && prompt.text.is_char_boundary(start)
                && prompt.text.is_char_boundary(end))
            .then(|| start + lead..end + lead)
        })
        .collect();
    (line, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(native: &str) -> PastRow {
        PastRow { key: format!("d:{native}"), native: native.to_owned(), ..PastRow::default() }
    }

    /// A session is called by its first prompt, else its last, else its id; on one line.
    #[test]
    fn a_session_is_named_by_what_was_asked_in_it() {
        let mut past = row("0123456789abcdef");
        assert_eq!(past.label(), "Session 01234567");
        past.prompt = Some(PromptLine { text: "the last\nprompt".to_owned(), ..PromptLine::default() });
        assert_eq!(past.label(), "the last prompt");
        past.title = Some("Fix the build".to_owned());
        assert_eq!(past.label(), "Fix the build");
    }

    /// The view model's answer reads whole, and a field it leaves out reads as none.
    #[test]
    fn an_answer_reads_as_the_view_model_writes_it() {
        let answer = serde_json::json!({
            "sessions": [{
                "key": "dev:abc", "deviceId": "dev", "device": "Studio", "native": "abc",
                "title": "Fix it", "prompt": { "text": "fix it", "spans": [[0, 3]],
                "cutBefore": false, "cutAfter": true }, "cwd": "/work", "spaceId": "dev:/work",
                "updatedAt": 1_700_000_000_000_i64, "branch": "main", "running": null,
                "chatId": null, "resume": ["--resume", "abc"],
            }],
            "failures": [{ "deviceId": "other", "device": "Laptop", "words": "Laptop is out of reach" }],
            "more": true,
            "cut": "read the newest 500",
        });
        let listed: Listed = serde_json::from_value(answer).unwrap();
        assert_eq!(listed.sessions[0].branch.as_deref(), Some("main"));
        assert_eq!(listed.sessions[0].prompt.as_ref().unwrap().spans, vec![[0, 3]]);
        assert_eq!(listed.cut.as_deref(), Some("read the newest 500"));
        assert_eq!(listed.failures[0].words, "Laptop is out of reach");
        let empty: Listed =
            serde_json::from_value(serde_json::json!({ "failures": [{ "words": "unreadable" }] }))
                .unwrap();
        assert!(empty.sessions.is_empty(), "a field left out reads as none");
    }

    /// The asked words stay lit after the line is made one line and marked where it was cut, and
    /// a span off a character boundary is dropped.
    #[test]
    fn the_asked_words_stay_lit_on_one_line() {
        let prompt = PromptLine {
            text: "fix\nthe é build".to_owned(),
            spans: vec![[4, 7], [9, 10], [0, 99]],
            cut_before: true,
            cut_after: true,
        };
        let (line, ranges) = prompt_line(&prompt);
        assert_eq!(line, "\u{2026}fix the é build\u{2026}");
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert_eq!(&line[ranges[0].clone()], "the");
    }

    /// A session held by a live Claude Code elsewhere says so, naming its machine.
    #[test]
    fn a_session_held_elsewhere_says_where() {
        let mut past = row("a");
        assert_eq!(past.held_elsewhere(), None);
        past.running = Some("interactive".to_owned());
        past.device = Some("Studio".to_owned());
        let said = past.held_elsewhere().unwrap();
        assert!(said.contains("in a terminal on Studio"), "{said}");
        assert_eq!(past.running_words(), Some("In a terminal"));
    }
}
