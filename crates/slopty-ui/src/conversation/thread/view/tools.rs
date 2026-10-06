//! A call, drawn as Zed's and Codex's threads draw one: a line on the thread's plane.
//!
//! Every call is one line, whatever it did: its mark, what it did (a file named first and its
//! folder after), how it stands, how long it took, and a disclosure that shows under the
//! pointer. Opened, its diff, its command and output or its sources sit under it in a quiet
//! well indented to its words. A call that only looked is muted; one that changed or ran
//! something is a step stronger. How it stands is said by its mark and a word, never by an
//! edge: no call is a card, so no row of the thread is boxed where its neighbours are not. A
//! request whose call is on screen is answered under the call's line.

use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_proto::thread::{ItemBody, ItemId, ToolCall, ToolDetail, ToolState};
use slopty_theme::{Rgb, Surfaces};

use super::{PEEK_LINES, TOOL_ROW, ThreadView, call_path, patch_of, path_patch, tail, tool_icon};
use crate::colors::hsla;
use crate::conversation::diff;
use crate::conversation::lines::{self, Ink};
use crate::conversation::thread::rows;
use crate::icons::{FileType, Symbol};
use crate::kit;

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

/// What a call's line says of how it stands, and in which tone, once that is not "done" or
/// "under way" (which its mark and its time say): it waits on the person, it failed, it was
/// not allowed, or it was stopped. The word is the state's one cue besides the mark's tone.
const fn standing(state: &ToolState, s: &Surfaces) -> Option<(&'static str, Rgb)> {
    match state {
        ToolState::Pending { .. } => Some(("Waiting for you", s.warn)),
        ToolState::Failed => Some(("Failed", s.error)),
        ToolState::Rejected => Some(("Not allowed", s.text_muted)),
        ToolState::Cancelled => Some(("Stopped", s.text_muted)),
        _ => None,
    }
}

impl ThreadView {
    pub(super) fn tool_row(&self, ix: usize, id: &ItemId, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.item(ix, id, cx).cloned() else { return div().into_any_element() };
        let ItemBody::Tool(call) = &item.body else { return div().into_any_element() };
        // A subagent's call opens its thread, once the table has it.
        let child = call.child.filter(|c| self.hub.read(cx).threads().rows().rows.contains_key(c));
        let waiting = matches!(call.state, ToolState::Pending { .. });
        // What a call asks the person to allow shows unasked.
        let open = self.items_open.contains(id) || (waiting && child.is_none());
        let took = call
            .ended_ms
            .filter(|end| *end > item.at_ms && !item.at_ms.is_zero())
            .map(|end| kit::duration(Duration::from_millis(end.millis_since(item.at_ms))));
        let line = self.call_line(id, call, open, child, took, cx);
        let body =
            open.then(|| self.sources(id, call).or_else(|| self.tool_body(id, call))).flatten();
        let pictures = if open { self.pictures_row(&call.images, false, cx) } else { None };
        let answers =
            self.answered_inline(ix).then(|| self.shown_waiting(cx).cloned()).flatten().and_then(
                |request| {
                    self.decision(&request, cx)
                        .map(|d| div().w_full().py(self.z(self.theme.spacing.xs)).child(d))
                },
            );
        if let Some(ToolDetail::Plan { text }) = &call.detail {
            let answers = answers.map(gpui::IntoElement::into_any_element);
            return self.plan_card(id, call, text, answers, cx);
        }
        let theme = &self.theme;
        // Under the line, indented to its words: the body in its well, the pictures, the
        // answers.
        let under =
            |el: AnyElement| div().w_full().pl(self.z(TOOL_ROW + theme.spacing.xs)).child(el);
        div()
            .debug_selector({
                let id = id.0.clone();
                move || format!("call-{id}")
            })
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xxs))
            .child(line)
            .children(body.map(under))
            .children(pictures.map(|p| under(p.into_any_element())))
            .children(answers.map(|a| under(a.into_any_element())))
            .into_any_element()
    }

    /// A call's own line: its mark (in the error tone once it failed), what it did (a file
    /// named first), how it stands, how long it took, and the disclosure.
    #[expect(clippy::too_many_arguments, reason = "the parts of one line, read once")]
    fn call_line(
        &self,
        id: &ItemId,
        call: &ToolCall,
        open: bool,
        child: Option<slopty_proto::thread::ThreadId>,
        took: Option<String>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let mark = match call.state {
            ToolState::Streaming | ToolState::Running => self.spinner(false),
            ToolState::Pending { .. } => self.needs_you(),
            _ => {
                let kind = tool_icon(&call.kind);
                let glyph = call_path(call).and_then(FileType::of).map_or(kind, FileType::symbol);
                let tone = if failed(&call.state) { s.error } else { s.text_muted };
                crate::icons::symbol(theme, glyph, self.z(theme.typography.icon()), hsla(tone))
            }
        };
        let cwd = self.state(cx).map(|st| st.meta.cwd.clone()).unwrap_or_default();
        // An MCP tool by what it does, then its server muted after it: "App click ·
        // computer-use", never the agent's own "computer-use: app_click". The raw id stays in
        // the open call's detail.
        let mcp = match &call.detail {
            Some(ToolDetail::Mcp(m)) => Some((humane(&m.tool), m.server.clone())),
            _ => None,
        };
        let title = match &mcp {
            Some((tool, server)) => format!("{tool} \u{b7} {server}"),
            None if call.title.is_empty() => call.name.clone(),
            None => tidy(&call.title, &cwd),
        };
        let label = match (&call.state, child) {
            (ToolState::Streaming, _) => format!("Preparing {}", call.name),
            (_, Some(_)) => format!("Subagent {title}"),
            _ => title.clone(),
        };
        let file = path_patch(call).map(|(path, _)| {
            let (name, dir) = lines::name_first(path);
            (name.to_owned(), tidy(&format!("{dir}/"), &cwd).trim_end_matches('/').to_owned())
        });
        let a_file = file.is_some();
        let file = file.or(mcp);
        let changes = patch_of(call).and_then(|p| kit::changes(theme, p.added, p.removed));
        let found = match &call.detail {
            Some(ToolDetail::WebSearch(search)) => sources_words(&search.links),
            _ => None,
        };
        let quiet = rows::quiet(call);
        let ink = if quiet { s.text_muted } else { s.text_secondary };
        let lead_ink = if a_file { s.text } else { ink };
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
                        .text_color(hsla(lead_ink))
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
            Symbol::ChevronRight
        } else if open {
            Symbol::ChevronDown
        } else {
            Symbol::ChevronRight
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
            .children(
                standing(&call.state, &s)
                    .map(|(word, tone)| div().flex_none().text_color(hsla(tone)).child(word)),
            )
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

    /// What an opened call shows, in its [`Self::well`]: its diff, or its command and output.
    fn tool_body(&self, id: &ItemId, call: &ToolCall) -> Option<AnyElement> {
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
            return Some(self.well(code).into_any_element());
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
        Some(
            self.well(text.px(self.z(theme.spacing.sm)).py(self.z(theme.spacing.xs)))
                .into_any_element(),
        )
    }

    /// What a web search came back with, opened: its links numbered, each its title and its
    /// site, opening its page on a press, or on ↵ once Tab is on it.
    fn sources(&self, id: &ItemId, call: &ToolCall) -> Option<AnyElement> {
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
                s.focus,
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
        Some(self.well(list.p(self.z(theme.spacing.xs))).into_any_element())
    }

    /// An opened call's well: set into the thread's plane ([`kit::inset`]) at a code shell's
    /// `radii.sm`, no edge, clipped to its corners.
    fn well(&self, content: impl gpui::IntoElement) -> Div {
        let theme = &self.theme;
        kit::inset(div(), theme)
            .w_full()
            .overflow_hidden()
            .rounded(self.z(theme.radii.sm))
            .child(content)
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

/// A tool's id as words: `app_click`, `appClick` and `app-click` read "App click".
fn humane(id: &str) -> String {
    let mut words = String::with_capacity(id.len().saturating_add(2));
    let mut last: Option<char> = None;
    for c in id.chars() {
        let gap = matches!(c, '_' | '-' | ' ' | '.');
        if gap {
            if last.is_some_and(|l| l != ' ') {
                words.push(' ');
                last = Some(' ');
            }
            continue;
        }
        if c.is_uppercase() && last.is_some_and(|l| l.is_lowercase() || l.is_ascii_digit()) {
            words.push(' ');
        }
        if words.is_empty() {
            words.extend(c.to_uppercase());
        } else {
            words.extend(c.to_lowercase());
        }
        last = Some(c);
    }
    let words = words.trim_end().to_owned();
    if words.is_empty() { id.to_owned() } else { words }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AskId, Clipped, ToolCall, ToolState, kind};

    use super::{host, humane, sources_words, standing, tidy};

    /// An MCP tool reads by what it does, whatever spelling its server gave its id.
    #[test]
    fn a_tool_id_reads_as_words() {
        assert_eq!(humane("app_click"), "App click");
        assert_eq!(humane("appClick"), "App click");
        assert_eq!(humane("app-click"), "App click");
        assert_eq!(humane("create_issue_v2"), "Create issue v2");
        assert_eq!(humane("__x__"), "X");
        assert_eq!(humane("___"), "___", "an id with no words stays as it is");
    }

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

    /// A call's line says how it stands in a word once that is more than done or under way:
    /// waiting and failed in their tones, the rest muted. No state draws an edge.
    #[test]
    fn a_call_says_how_it_stands_in_a_word() {
        let s = slopty_theme::Theme::default().surfaces;
        let asks = ToolState::Pending { ask: AskId("a".to_owned()) };
        assert_eq!(standing(&asks, &s), Some(("Waiting for you", s.warn)));
        assert_eq!(standing(&ToolState::Failed, &s), Some(("Failed", s.error)));
        assert_eq!(standing(&ToolState::Rejected, &s), Some(("Not allowed", s.text_muted)));
        assert_eq!(standing(&ToolState::Cancelled, &s), Some(("Stopped", s.text_muted)));
        assert_eq!(standing(&ToolState::Completed, &s), None);
        assert_eq!(standing(&ToolState::Running, &s), None);
        let _quiet = call(kind::READ, ToolState::Completed);
    }
}
