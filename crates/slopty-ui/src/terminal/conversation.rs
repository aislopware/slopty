//! The conversation view: a Claude Code session's transcript read like a chat, shown in
//! place of the grid.
//!
//! The host tails the agent's transcript file and streams [`TranscriptEntry`]s; the view keeps
//! them in a bottom-aligned [`gpui::list`] (a chat log: new entries append at the bottom and the
//! view stays there unless the reader scrolled up). Typing still goes to the terminal, so a
//! phone can read what the agent said in Markdown and answer without switching back.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    ParentElement as _, SharedString, Styled as _, Window, div, list, px,
};
use gpui_kit::component::text::TextView;
use slopty_proto::agent::{TranscriptEntry, TranscriptUpdate};
use slopty_theme::Theme;

use crate::colors::{hsla, hsla_alpha};

/// A session's conversation as received so far.
pub struct Conversation {
    /// Entries, oldest first; shared with the list's render closure.
    entries: Rc<[TranscriptEntry]>,
    list: ListState,
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conversation").field("entries", &self.entries.len()).finish_non_exhaustive()
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

impl Conversation {
    /// Empty, waiting for the host's snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self { entries: Rc::from([]), list: ListState::new(0, ListAlignment::Bottom, px(512.0)) }
    }

    /// The entries shown.
    #[must_use]
    pub fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// Take a slice from the host: a reset replaces everything, otherwise the entries append.
    pub fn apply(&mut self, update: TranscriptUpdate) {
        let mut entries = if update.reset { Vec::new() } else { self.entries.to_vec() };
        let before = entries.len();
        entries.extend(update.entries);
        let after = entries.len();
        self.entries = Rc::from(entries);
        if update.reset {
            self.list.reset(after);
        } else {
            self.list.splice(before..before, after.saturating_sub(before));
        }
    }

    /// The chat, filling its container.
    #[must_use]
    pub fn render(&self, theme: &Theme, ui_size: f32) -> AnyElement {
        let entries = Rc::clone(&self.entries);
        let theme = theme.clone();
        let empty = entries.is_empty();
        div()
            .id("conversation")
            .debug_selector(|| "conversation".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(theme.surfaces.panel))
            .text_size(px(ui_size))
            .text_color(hsla(theme.surfaces.text))
            .font_family(theme.typography.ui_family.clone())
            .when(empty, |el| {
                el.items_center()
                    .justify_center()
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child("waiting for the agent's conversation…")
            })
            .when(!empty, |el| {
                el.child(
                    list(self.list.clone(), move |ix, _window: &mut Window, _cx: &mut App| {
                        entries.get(ix).map_or_else(
                            || div().into_any_element(),
                            |e| entry(ix, e, &theme, ui_size),
                        )
                    })
                    .size_full(),
                )
            })
            .into_any_element()
    }
}

/// One entry: the human's prompt in an accent bubble, the agent's Markdown, a muted line
/// for a tool call.
fn entry(ix: usize, entry: &TranscriptEntry, theme: &Theme, ui_size: f32) -> AnyElement {
    let s = &theme.surfaces;
    let row = div().w_full().px(px(ui_size)).py(px(ui_size * 0.35));
    match entry {
        TranscriptEntry::User { text } => row
            .flex()
            .justify_end()
            .child(
                div()
                    .max_w(px(ui_size * 40.0))
                    .px(px(ui_size * 0.8))
                    .py(px(ui_size * 0.45))
                    .rounded(px(ui_size * 0.6))
                    .bg(hsla_alpha(s.accent, 0.22))
                    .child(SharedString::from(text.clone())),
            )
            .into_any_element(),
        TranscriptEntry::Assistant { markdown } => row
            .child(TextView::markdown(
                ElementId::NamedInteger(
                    "conversation-entry".into(),
                    u64::try_from(ix).unwrap_or(0),
                ),
                SharedString::from(markdown.clone()),
            ))
            .into_any_element(),
        TranscriptEntry::ToolUse { name, summary } => row
            .flex()
            .gap(px(ui_size * 0.5))
            .text_size(px(ui_size * 0.85))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(format!("▸ {name}")))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .font_family(
                        theme.typography.mono_families.first().cloned().unwrap_or_default(),
                    )
                    .child(SharedString::from(summary.clone())),
            )
            .into_any_element(),
    }
}
