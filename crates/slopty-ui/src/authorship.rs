//! Who wrote a line, as a file tile and a review tile say it: the thread and its turn, in a
//! quiet tag that opens the thread at that turn.
//!
//! The worker answers once per file as it was read ([`Authors`], kept by the hub until the file
//! changes); a tile holds the answer with the names of the threads in it ([`Authored`]), so the
//! line under the caret or the pointer is said with no ask. A thread only a commit's trailer
//! names may be another machine's: it is named when one of the workers here knows it, and
//! opens only then.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::{
    InteractiveElement as _, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_core::WallMs;
use slopty_proto::thread::wire::{AuthorRun, Authors};
use slopty_proto::thread::{AgentId, ThreadId, TurnId};
use slopty_theme::Theme;

use crate::colors::hsla;

/// A thread that wrote lines, as a worker here knows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Writer {
    /// Its agent.
    pub agent: AgentId,
    /// Its title; empty when it has none yet.
    pub title: String,
}

/// A file's authors with the names of the threads among them.
#[derive(Clone, Debug)]
pub struct Authored {
    /// The worker's answer.
    pub authors: Arc<Authors>,
    /// The threads it names that a worker here knows.
    pub writers: HashMap<ThreadId, Writer>,
}

impl PartialEq for Authored {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.authors, &other.authors) && self.writers == other.writers
    }
}

impl Authored {
    /// Who wrote line `line` (from 1), if a thread did.
    #[must_use]
    pub fn at(&self, line: u32) -> Option<&AuthorRun> {
        slopty_client::threads::authors::run_at(&self.authors, line)
    }
}

/// Where a tag goes: the thread, and its turn when the worker still keeps it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Opens {
    /// The thread.
    pub thread: ThreadId,
    /// The turn that wrote the lines.
    pub turn: Option<TurnId>,
}

/// How long ago `at` was, as the tag says it.
fn age(now: WallMs, at: WallMs) -> String {
    crate::palette::age_label(Duration::from_millis(now.millis_since(at)))
}

/// The widest a thread's title stands in a tag, in points; the rest fades.
const TITLE_MAX: f32 = 240.0;

/// What a tag says of `run`: within `own`'s own review, its turn alone; else the thread by its
/// title (its agent while untitled), then the turn, or the commit where only a trailer names
/// it; and how long ago.
#[must_use]
pub fn words(
    run: &AuthorRun,
    writer: Option<&Writer>,
    own: Option<ThreadId>,
    now: WallMs,
) -> String {
    let (who, rest) = parts(run, writer, own, now);
    who.into_iter().chain(std::iter::once(rest)).collect::<Vec<_>>().join(" \u{b7} ")
}

/// [`words`] as the thread, when another's, and the rest.
fn parts(
    run: &AuthorRun,
    writer: Option<&Writer>,
    own: Option<ThreadId>,
    now: WallMs,
) -> (Option<String>, String) {
    let when = age(now, run.at_ms);
    let own = own == Some(run.thread);
    let what = match (run.turn, &run.commit) {
        (Some(turn), _) if own => format!("Turn {}", turn.0),
        (Some(turn), _) => format!("turn {}", turn.0),
        (None, Some(commit)) => commit.chars().take(7).collect(),
        (None, None) => String::new(),
    };
    let rest =
        [what, when].into_iter().filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" \u{b7} ");
    if own {
        return (None, rest);
    }
    let who = writer.map_or_else(
        || "Another thread".to_owned(),
        |w| {
            if w.title.is_empty() {
                crate::conversation::thread::view::agent_label(&w.agent)
            } else {
                w.title.clone()
            }
        },
    );
    (Some(who), rest)
}

/// The tag that says who wrote `run`, with `writer`'s mark: muted, one line, and, for a thread
/// a worker here knows, a press away from it.
#[must_use]
pub fn tag(
    theme: &Theme,
    id: SharedString,
    run: &AuthorRun,
    writer: Option<&Writer>,
    own: Option<ThreadId>,
    now: WallMs,
) -> Stateful<gpui::Div> {
    let s = theme.surfaces;
    let label = format!("Written in {}", words(run, writer, own, now));
    let (who, rest) = parts(run, writer, own, now);
    let selector = id.to_string();
    let who = who.map(|who| {
        let fit = crate::kit::fit_label(format!("{selector}-who"), who, theme);
        div().min_w_0().max_w(px(TITLE_MAX)).child(fit)
    });
    let rest = if who.is_some() { format!("\u{b7} {rest}") } else { rest };
    let glyph = writer.map(|w| {
        crate::icons::glyph(
            theme,
            crate::icons::Glyph::agent(&w.agent.0),
            px(theme.typography.small()),
            hsla(s.text_muted),
        )
    });
    let tag = div()
        .id(id)
        .debug_selector(move || selector)
        .aria_label(SharedString::from(label))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme.spacing.xs))
        .px(px(theme.spacing.xs))
        .rounded(px(theme.radii.xs))
        .font_family(theme.typography.ui_family.clone())
        .text_size(px(theme.typography.small()))
        .text_color(hsla(s.text_muted))
        .whitespace_nowrap()
        .children(glyph)
        .children(who)
        .child(SharedString::from(rest));
    if writer.is_some() || own == Some(run.thread) {
        tag.role(Role::Link)
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text_secondary)))
            .active(move |el| el.bg(hsla(s.pressed)))
    } else {
        tag.role(Role::Label)
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::AuthorRun;
    use slopty_proto::thread::{AgentId, ThreadId, TurnId};

    use super::{Writer, words};

    /// A line is said by its turn in its own thread's review, else by its thread's title, or
    /// its agent while untitled; a trailer's commit stands for the turn; a thread no worker
    /// here knows is another thread.
    #[test]
    fn a_tag_names_the_thread_and_turn() {
        let thread = ThreadId::new();
        let run = |turn: Option<u32>, commit: Option<&str>| AuthorRun {
            start: 1,
            lines: 1,
            thread,
            turn: turn.map(TurnId),
            commit: commit.map(str::to_owned),
            at_ms: WallMs::from_millis(1_000),
        };
        let now = WallMs::from_millis(1_000 + 2 * 3_600_000);
        let titled = Writer {
            agent: AgentId(AgentId::CODEX.to_owned()),
            title: "Fix the parser".to_owned(),
        };
        let untitled =
            Writer { agent: AgentId(AgentId::CLAUDE_CODE.to_owned()), title: String::new() };
        assert_eq!(
            words(&run(Some(3), None), Some(&titled), None, now),
            "Fix the parser \u{b7} turn 3 \u{b7} 2h"
        );
        assert_eq!(
            words(&run(Some(3), None), Some(&titled), Some(thread), now),
            "Turn 3 \u{b7} 2h"
        );
        assert_eq!(
            words(&run(Some(1), None), Some(&untitled), None, now),
            "Claude Code \u{b7} turn 1 \u{b7} 2h"
        );
        assert_eq!(
            words(&run(None, Some("0123456789ab")), None, None, now),
            "Another thread \u{b7} 0123456 \u{b7} 2h"
        );
    }
}
