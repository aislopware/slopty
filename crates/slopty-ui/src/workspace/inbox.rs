//! The bell's inbox: what waits on the human and what finished while they looked away.
//!
//! *Needs you* lists the agents waiting, *Finished* the long commands that ended unwatched
//! (the header badges). Each row is a status mark, a title and the worker it ran on, and goes
//! there when clicked. The bell counts both.

use gpui::accesskit::Role;
use gpui::{
    Context, ElementId, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_core::SessionId;

use super::WorkspaceView;
use super::navigator::{caption, heading, row, title};
use crate::colors::hsla;
use crate::icons::{Status, status_mark};

/// Where a tile is: workspace, column, tile.
type Place = (usize, usize, usize);

/// The popover's width, in points.
const INBOX_W: f32 = 320.0;

impl WorkspaceView {
    /// What the bell counts: agents waiting and commands finished unwatched.
    #[must_use]
    pub fn inbox_count(&self) -> usize {
        self.needs_you_count().saturating_add(self.finished.len())
    }

    /// The finished commands, in the strip's reading order.
    fn finished_sessions(&self) -> Vec<SessionId> {
        let mut sessions: Vec<(Option<Place>, SessionId)> = self
            .finished
            .keys()
            .map(|session| {
                let pos = self
                    .tile_of_session(*session)
                    .and_then(|t| self.layout.position(t))
                    .map(|p| (p.workspace, p.column, p.tile));
                (pos, *session)
            })
            .collect();
        sessions.sort_unstable();
        sessions.into_iter().map(|(_, s)| s).collect()
    }

    /// The popover's panel.
    pub(super) fn render_inbox(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let waiting = self.drawn_waiting.clone();
        if !waiting.is_empty() {
            rows.push(heading(theme, "inbox-needs-you", "Needs you").into_any_element());
            rows.extend(waiting.into_iter().map(|w| self.waiting_row("inbox", w, cx)));
        }
        let finished = self.finished_sessions();
        if !finished.is_empty() {
            rows.push(heading(theme, "inbox-finished", "Finished").into_any_element());
        }
        for session in finished {
            let Some(done) = self.finished.get(&session) else { continue };
            let status = match done.exit {
                Some(0) | None => Status::Done,
                Some(_) => Status::Failed,
            };
            let command = done.command.trim();
            let what = if command.is_empty() { "Command".to_owned() } else { command.to_owned() };
            let worker = self
                .worker_of_session(session)
                .and_then(|k| self.workers.get(&k))
                .map(|w| w.name.clone())
                .unwrap_or_default();
            let label = SharedString::from(format!("{what}, {}, on {worker}", done.label()));
            rows.push(
                row(
                    theme,
                    ElementId::Name(format!("inbox-finished-{session}").into()),
                    format!("inbox-finished-{session}"),
                    label,
                    false,
                )
                .child(status_mark(theme, Some(status), 1.0))
                .child(title(what, hsla(s.text)))
                .child(caption(theme, worker))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.menu = None;
                    this.reveal_session(session, cx);
                }))
                .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(
                div()
                    .px(px(theme.spacing.md))
                    .py(px(theme.spacing.sm))
                    .text_color(hsla(s.text_muted))
                    .child("Nothing new")
                    .into_any_element(),
            );
        }
        div()
            .id("inbox")
            .debug_selector(|| "inbox".to_owned())
            .role(Role::Dialog)
            .aria_label("Inbox")
            .occlude()
            .w(px(INBOX_W))
            .flex()
            .flex_col()
            .pb(px(theme.spacing.xs))
            .rounded(px(theme.radii.md))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .shadow_sm()
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .children(rows)
            .into_any_element()
    }
}
