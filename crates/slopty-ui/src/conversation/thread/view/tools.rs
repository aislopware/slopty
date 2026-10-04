//! A call, drawn as Zed's thread draws it.
//!
//! A read, a search, a fetch: one muted line with its mark, a disclosure that shows under the
//! pointer, and once opened its output under a hairline rule, unfilled. An edit, a write, a
//! command, and any call that waits on the person: a card resting on the thread, its edge in
//! the warn tone while it waits and in the error tone once it failed, its file named first and
//! its folder after, its diff or its command and output under a hairline inside. A request
//! whose call is on screen is answered on the call's card.

use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_proto::thread::{ItemBody, ItemId, ToolCall, ToolDetail, ToolState};
use slopty_theme::{Rgb, Surfaces, alpha};

use super::{PEEK_LINES, TOOL_ROW, ThreadView, call_path, patch_of, path_patch, tail, tool_icon};
use crate::colors::{hsla, hsla_alpha};
use crate::conversation::diff;
use crate::conversation::lines::{self, Ink};
use crate::conversation::thread::rows;
use crate::icons::{FileType, Glyph, IconName};
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

/// The tone a call's card is edged in, and how much of it: the warn tone while the call waits
/// on the person, the error tone once it failed; none for the rest, which rests as any card.
const fn edge_tone(state: &ToolState, s: &Surfaces) -> Option<(Rgb, f32)> {
    if matches!(state, ToolState::Pending { .. }) {
        Some((s.warn, alpha::ASKING_EDGE))
    } else if failed(state) {
        Some((s.error, alpha::FAILED_EDGE))
    } else {
        None
    }
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
        let body = open
            .then(|| self.sources(id, call, look).or_else(|| self.tool_body(id, call, look)))
            .flatten();
        let pictures = if open { self.pictures_row(&call.images, false, cx) } else { None };
        let answers = self
            .answered_inline(ix)
            .then(|| self.shown_waiting(cx).cloned())
            .flatten()
            .map(|request| self.answers_row(self.answer_buttons(&request, cx)));
        if let Some(ToolDetail::Plan { text }) = &call.detail {
            let answers = answers.map(gpui::IntoElement::into_any_element);
            return self.plan_card(id, call, text, answers, cx);
        }
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
            Look::Card => kit::card(theme)
                .debug_selector({
                    let id = id.0.clone();
                    move || format!("call-card-{id}")
                })
                .w_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(self.z(theme.radii.md))
                // A dashed edge said "failed" by drawing, which GPUI fakes: the tone says it.
                .map(|el| match edge_tone(&call.state, &s) {
                    Some((tone, share)) => {
                        el.border(kit::hair(theme)).border_color(hsla_alpha(tone, share))
                    }
                    None => el,
                })
                .child(line)
                .children(body.map(|b| {
                    div()
                        .w_full()
                        .border_t(kit::hair(theme))
                        .border_color(hsla(s.border_subtle))
                        .child(b)
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
            _ => {
                let kind = Glyph::Icon(tool_icon(&call.kind));
                let glyph = call_path(call).and_then(FileType::of).map_or(kind, Glyph::File);
                crate::icons::glyph(
                    theme,
                    glyph,
                    self.z(theme.typography.icon()),
                    hsla(s.text_muted),
                )
            }
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
        let changes = patch_of(call).and_then(|p| kit::changes(theme, p.added, p.removed));
        let found = match &call.detail {
            Some(ToolDetail::WebSearch(search)) => sources_words(&search.links),
            _ => None,
        };
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
            .children(found.map(|words| {
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(words))
            }))
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
                    .text_size(self.z(theme.typography.small()))
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

    /// What a web search came back with, opened: its links numbered, each its title and its
    /// site, opening its page on a press, or on ↵ once Tab is on it.
    fn sources(&self, id: &ItemId, call: &ToolCall, look: Look) -> Option<AnyElement> {
        let Some(ToolDetail::WebSearch(search)) = &call.detail else { return None };
        if search.links.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let rows = search.links.iter().enumerate().map(|(ix, link)| {
            let url = link.url.clone();
            let number = ix.saturating_add(1);
            let title =
                if link.title.trim().is_empty() { link.url.clone() } else { link.title.clone() };
            let selector = format!("source-{}-{number}", id.0);
            crate::a11y::tab_stop(
                div()
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector)
                    .role(Role::Link)
                    .aria_label(SharedString::from(format!("{title}, {}", host(&link.url))))
                    .w_full()
                    .flex()
                    .items_baseline()
                    .gap(self.z(theme.spacing.xs))
                    .min_h(self.z(TOOL_ROW))
                    .px(self.z(theme.spacing.xxs))
                    .rounded(self.z(theme.radii.xs))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)))
                    .child(
                        kit::tabular(div())
                            .flex_none()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(number.to_string())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_shrink_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(gpui::relative(0.4))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(host(&link.url).to_owned())),
                    ),
                s.accent,
            )
            .on_click(move |_ev, _w, cx| cx.open_url(&url))
        });
        let list = div()
            .id(ElementId::Name(format!("sources-{}", id.0).into()))
            .w_full()
            .flex()
            .flex_col()
            .role(Role::List)
            .aria_label("Sources")
            .text_size(self.z(theme.typography.small()))
            .children(rows);
        Some(match look {
            Look::Card => list.p(self.z(theme.spacing.xs)).into_any_element(),
            Look::Line => self.ruled(div().w_full().child(list)).into_any_element(),
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
                .border_l(kit::hair(theme))
                .border_color(hsla(theme.surfaces.border_subtle))
                .text_color(hsla(theme.surfaces.text_muted))
                .child(content),
        )
    }
}

/// The site a link is on: its host, without the scheme or a leading `www.`.
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
}

/// A search's links in a few words beside its query: "3 sources · docs.rs · github.com", the
/// first two sites it found, each once.
fn sources_words(links: &[slopty_proto::thread::detail::WebLink]) -> Option<String> {
    if links.is_empty() {
        return None;
    }
    let mut sites: Vec<&str> = Vec::new();
    for link in links {
        let site = host(&link.url);
        if !site.is_empty() && !sites.contains(&site) {
            sites.push(site);
        }
    }
    let count = kit::count(links.len() as u64, "source", "sources");
    Some(
        std::iter::once(count)
            .chain(sites.into_iter().take(2).map(str::to_owned))
            .collect::<Vec<_>>()
            .join(" \u{b7} "),
    )
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AskId, Clipped, ToolCall, ToolState, kind};

    use super::{Look, host, sources_words, tidy};

    /// A search's links read as how many and the first two sites, each once.
    #[test]
    fn a_search_says_its_sources_and_their_sites() {
        use slopty_proto::thread::detail::WebLink;
        let link = |url: &str| WebLink { title: "t".to_owned(), url: url.to_owned() };
        assert_eq!(host("https://www.docs.rs/gpui?x=1"), "docs.rs");
        assert_eq!(host("github.com/a/b"), "github.com");
        let links = [
            link("https://docs.rs/a"),
            link("https://docs.rs/b"),
            link("https://github.com/c"),
            link("https://zed.dev"),
        ];
        assert_eq!(
            sources_words(&links).as_deref(),
            Some("4 sources \u{b7} docs.rs \u{b7} github.com")
        );
        assert_eq!(sources_words(&links[..1]).as_deref(), Some("1 source \u{b7} docs.rs"));
        assert_eq!(sources_words(&[]), None);
    }

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
