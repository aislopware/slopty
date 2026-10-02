//! A call, drawn as Zed's thread draws it.
//!
//! A read, a search, a fetch: one muted line with its mark, a disclosure that shows under the
//! pointer, and once opened its output under a hairline rule, unfilled. An edit, a write, a
//! command, and any call that waits on the person: a card with a hairline, dashed once the call
//! failed, its file named first and its folder after, its diff or its command and output under
//! a hairline inside. A request whose call is on screen is answered on the call's card.

use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_proto::thread::{ItemBody, ItemId, ToolCall, ToolDetail, ToolState};

use super::{PEEK_LINES, TOOL_ROW, ThreadView, patch_of, path_patch, tail, tool_icon};
use crate::colors::hsla;
use crate::conversation::diff;
use crate::conversation::lines::{self, Ink};
use crate::conversation::thread::rows;
use crate::icons::IconName;
use crate::kit;

/// How a call is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Look {
    /// One quiet line: it only looked.
    Line,
    /// A card: it changed something, ran something, or waits on the person.
    Card,
}

impl Look {
    /// How `call` is drawn.
    pub(super) fn of(call: &ToolCall) -> Self {
        if rows::quiet(call) { Self::Line } else { Self::Card }
    }
}

/// `text` with the paths in it as a person reads them: under the agent's folder `cwd` relative
/// to it (`/w/app/src/x.rs` reads `src/x.rs`, by either of macOS's names for a temporary
/// folder), and under a home folder from `~`.
pub(super) fn tidy(text: &str, cwd: &str) -> String {
    let root = cwd.trim_end_matches('/');
    let mut roots: Vec<String> = Vec::new();
    if !root.is_empty() {
        roots.push(format!("{root}/"));
        // `/var` and `/tmp` are links into `/private`: an agent may name either.
        match root.strip_prefix("/private") {
            Some(bare) if bare.starts_with("/var/") || bare.starts_with("/tmp/") => {
                roots.push(format!("{bare}/"));
            }
            _ if root.starts_with("/var/") || root.starts_with("/tmp/") => {
                roots.push(format!("/private{root}/"));
            }
            _ => {}
        }
    }
    let mut out = roots.iter().fold(text.to_owned(), |t, r| t.replace(r.as_str(), ""));
    for home in ["/Users/", "/home/"] {
        let mut from = 0_usize;
        while let Some(at) =
            out.get(from..).and_then(|rest| rest.find(home)).map(|i| i.saturating_add(from))
        {
            let starts = at == 0
                || out
                    .get(..at)
                    .and_then(|head| head.chars().last())
                    .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '\'' | '"' | '`'));
            let name_end = out
                .get(at.saturating_add(home.len())..)
                .and_then(|rest| rest.find('/'))
                .map(|i| i.saturating_add(at).saturating_add(home.len()));
            match name_end.filter(|_| starts) {
                Some(end) => {
                    out.replace_range(at..=end, "~/");
                    from = at.saturating_add(2);
                }
                None => from = at.saturating_add(home.len()),
            }
        }
    }
    out
}

/// Whether `state` says the call did not do what it set out to.
const fn failed(state: &ToolState) -> bool {
    matches!(state, ToolState::Failed | ToolState::Rejected | ToolState::Cancelled)
}

impl ThreadView {
    pub(super) fn tool_row(&self, ix: usize, id: &ItemId, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.item(ix, id, cx).cloned() else { return div().into_any_element() };
        let ItemBody::Tool(call) = &item.body else { return div().into_any_element() };
        // A subagent's call opens its thread, once the table has it.
        let child = call.child.filter(|c| self.hub.read(cx).threads().rows().rows.contains_key(c));
        let look = Look::of(call);
        let waiting = matches!(call.state, ToolState::Pending { .. });
        // What a call asks the person to allow shows unasked.
        let open = self.items_open.contains(id) || (waiting && child.is_none());
        let took = call
            .ended_ms
            .filter(|end| *end > item.at_ms && !item.at_ms.is_zero())
            .map(|end| kit::duration(Duration::from_millis(end.millis_since(item.at_ms))));
        let line = self.call_line(id, call, look, open, child, took, cx);
        let body = open.then(|| self.tool_body(id, call, look)).flatten();
        let pictures = if open { self.pictures_row(&call.images, false, cx) } else { None };
        let answers = self
            .answered_inline(ix)
            .then(|| self.shown_waiting(cx).cloned())
            .flatten()
            .map(|request| self.answers_row(self.answer_buttons(&request, cx)));
        let theme = &self.theme;
        let s = theme.surfaces;
        match look {
            Look::Line => div()
                .w_full()
                .flex()
                .flex_col()
                .gap(self.z(theme.spacing.xxs))
                .child(line)
                .children(body)
                .children(pictures.map(|p| div().pl(self.z(TOOL_ROW)).child(p)))
                .children(answers)
                .into_any_element(),
            Look::Card => div()
                .debug_selector({
                    let id = id.0.clone();
                    move || format!("call-card-{id}")
                })
                .w_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(self.z(theme.radii.sm))
                .border_1()
                .border_color(hsla(s.border))
                .when(failed(&call.state), gpui::Styled::border_dashed)
                .child(line)
                .children(body.map(|b| {
                    div().w_full().border_t_1().border_color(hsla(s.border_subtle)).child(b)
                }))
                .children(pictures.map(|p| div().p(self.z(theme.spacing.xs)).child(p)))
                .children(answers)
                .into_any_element(),
        }
    }

    /// A call's own line: its mark, what it did (a file named first), how it stands, how long
    /// it took, and the disclosure.
    #[expect(clippy::too_many_arguments, reason = "the parts of one line, read once")]
    fn call_line(
        &self,
        id: &ItemId,
        call: &ToolCall,
        look: Look,
        open: bool,
        child: Option<slopty_proto::thread::ThreadId>,
        took: Option<String>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let mark = match call.state {
            ToolState::Streaming | ToolState::Running => self.spinner(false),
            ToolState::Pending { .. } => self.spinner(true),
            _ => self.icon(tool_icon(&call.kind), s.text_muted),
        };
        let cwd = self.state(cx).map(|st| st.meta.cwd.clone()).unwrap_or_default();
        let title = if call.title.is_empty() { call.name.clone() } else { tidy(&call.title, &cwd) };
        let label = match (&call.state, child) {
            (ToolState::Streaming, _) => format!("Preparing {}", call.name),
            (_, Some(_)) => format!("Subagent {title}"),
            _ => title.clone(),
        };
        let file = path_patch(call).map(|(path, _)| {
            let (name, dir) = lines::name_first(path);
            (name.to_owned(), tidy(&format!("{dir}/"), &cwd).trim_end_matches('/').to_owned())
        });
        let changes =
            patch_of(call).and_then(|p| kit::changes_at(theme, p.added, p.removed, self.zoom));
        let quiet = look == Look::Line;
        let ink = if quiet { s.text_muted } else { s.text_secondary };
        let group: SharedString = format!("call-{}", id.0).into();
        let what = match file {
            Some((name, dir)) => div()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(self.z(theme.spacing.xs))
                .child(
                    div()
                        .flex_none()
                        .max_w(gpui::relative(0.7))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text))
                        .child(SharedString::from(name)),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(dir)),
                ),
            None => div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(hsla(ink))
                .group_hover(group.clone(), move |el| el.text_color(hsla(s.text)))
                .child(SharedString::from(title.clone())),
        };
        let disclosure = if child.is_some() {
            IconName::ChevronRight
        } else if open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let called = title;
        let toggle = id.clone();
        div()
            .id(ElementId::Name(format!("tool-{}", id.0).into()))
            .debug_selector({
                let id = id.0.clone();
                move || format!("tool-{id}")
            })
            .group(group.clone())
            .role(Role::Button)
            .aria_label(SharedString::from(label))
            .when(child.is_none(), |el| el.aria_expanded(open))
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .when(!quiet, |el| el.pr(self.z(theme.spacing.sm)))
            .text_size(self.z(theme.typography.small()))
            .cursor_pointer()
            .child(self.slot().child(mark))
            .child(what)
            .children(changes)
            .when(failed(&call.state), |el| el.child(self.icon(IconName::X, s.text_muted)))
            .when(matches!(call.state, ToolState::Pending { .. }), |el| {
                el.child(div().flex_none().text_color(hsla(s.text_muted)).child("Waiting for you"))
            })
            .child(
                div()
                    .flex_none()
                    .when(!open && child.is_none(), |el| {
                        el.invisible().group_hover(group, gpui::StyleRefinement::visible)
                    })
                    .child(self.icon(disclosure, s.text_muted)),
            )
            .child(div().flex_1())
            .children(took.map(|t| {
                kit::tabular(div())
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(t))
            }))
            .on_click(cx.listener(move |this, _ev, window, cx| match child {
                Some(child) => this.open_subagent(child, called.clone(), window, cx),
                None => this.toggle_item(toggle.clone(), cx),
            }))
            .into_any_element()
    }

    /// What an opened call shows: its diff, or its command and output; on a card under its
    /// line, for a quiet call under a hairline rule.
    fn tool_body(&self, id: &ItemId, call: &ToolCall, look: Look) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        if let Some((path, patch)) = path_patch(call) {
            // A call's patch is whole once it shows: the call is over before it opens.
            let blocks = Rc::clone(
                self.diffs
                    .borrow_mut()
                    .entry(id.clone())
                    .or_insert_with(|| diff::thread_blocks(path, patch)),
            );
            let ink = Ink { theme, zoom: self.zoom, digits: lines::digits(&blocks) };
            let mut shown = 0_usize;
            let mut children: Vec<AnyElement> = Vec::new();
            for (ix, block) in blocks.iter().enumerate() {
                if shown >= PEEK_LINES {
                    break;
                }
                if ix > 0 {
                    children.push(ink.hunk_head(block).into_any_element());
                }
                for line in block.lines.iter().take(PEEK_LINES.saturating_sub(shown)) {
                    children.push(ink.unified(line).into_any_element());
                    shown = shown.saturating_add(1);
                }
            }
            let code = ink.code().py(self.z(theme.spacing.xxs)).children(children);
            return Some(match look {
                Look::Card => code.into_any_element(),
                Look::Line => self.ruled(code).into_any_element(),
            });
        }
        let command = match &call.detail {
            Some(ToolDetail::Exec(exec)) => Some(exec.command.text.clone()),
            _ => None,
        };
        let output = call.output.as_ref().map(|o| tail(&o.text, PEEK_LINES));
        if command.is_none() && output.is_none() {
            return None;
        }
        let text = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .font_family(self.mono())
            .text_size(self.z(theme.typography.small()))
            .children(command.map(|c| {
                div().text_color(hsla(s.text)).child(SharedString::from(format!("$ {c}")))
            }))
            .children(output.map(|o| {
                div()
                    .text_color(hsla(s.text_secondary))
                    .whitespace_normal()
                    .child(SharedString::from(o))
            }));
        Some(match look {
            Look::Card => {
                text.px(self.z(theme.spacing.sm)).py(self.z(theme.spacing.xs)).into_any_element()
            }
            Look::Line => self.ruled(text).into_any_element(),
        })
    }

    /// A quiet call's output: under its mark, past a one-point rule, unfilled.
    fn ruled(&self, content: Div) -> Div {
        let theme = &self.theme;
        div().w_full().pl(self.z(TOOL_ROW / 2.0)).child(
            div()
                .w_full()
                .pl(self.z(TOOL_ROW / 2.0 + theme.spacing.xs))
                .py(self.z(theme.spacing.xxs))
                .border_l_1()
                .border_color(hsla(theme.surfaces.border_subtle))
                .text_color(hsla(theme.surfaces.text_muted))
                .child(content),
        )
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AskId, Clipped, ToolCall, ToolState, kind};

    use super::{Look, tidy};

    fn call(of: &str, state: ToolState) -> ToolCall {
        ToolCall {
            name: of.to_owned(),
            kind: of.to_owned(),
            title: String::new(),
            input: Clipped::default(),
            state,
            output: None,
            images: Vec::new(),
            detail: None,
            child: None,
            ended_ms: None,
        }
    }

    /// Paths read from the agent's folder, by either name of a temporary folder, and from
    /// `~` under a home; a path elsewhere stays whole.
    #[test]
    fn paths_read_from_the_agents_folder() {
        let cwd = "/private/var/folders/x/T/home/code/atlas";
        assert_eq!(
            tidy("Read /private/var/folders/x/T/home/code/atlas/crates/api/src/lib.rs", cwd),
            "Read crates/api/src/lib.rs"
        );
        assert_eq!(tidy("Read /var/folders/x/T/home/code/atlas/a.rs", cwd), "Read a.rs");
        assert_eq!(tidy("cat /Users/me/notes.md /etc/hosts", "/w"), "cat ~/notes.md /etc/hosts");
        assert_eq!(tidy("ls /home/dev/src", "/w"), "ls ~/src");
        assert_eq!(tidy("ls /w2/src", "/w"), "ls /w2/src", "a sibling is not under it");
    }

    /// A call that only looks is a quiet line; one that changes or runs something, or waits
    /// on the person, is a card.
    #[test]
    fn a_call_that_acts_is_a_card() {
        assert_eq!(Look::of(&call(kind::READ, ToolState::Completed)), Look::Line);
        assert_eq!(Look::of(&call(kind::SEARCH, ToolState::Running)), Look::Line);
        assert_eq!(Look::of(&call(kind::EDIT, ToolState::Completed)), Look::Card);
        assert_eq!(Look::of(&call(kind::EXEC, ToolState::Failed)), Look::Card);
        let asks = ToolState::Pending { ask: AskId("a".to_owned()) };
        assert_eq!(Look::of(&call(kind::FETCH, asks)), Look::Card);
    }
}
