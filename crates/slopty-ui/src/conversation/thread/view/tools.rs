//! A call, drawn as Zed's and Codex's threads draw one: a line on the thread's plane.
//!
//! Every call is one line, whatever it did: its kind's mark (a file's type for a call on one
//! file), what it did (a file's verb, then the file named first and its folder after; a
//! command's own words), how it stands, how long it took, and a disclosure that shows under
//! the pointer. The file's name opens the file in a tile of its own; the rest of the line
//! opens its diff, its command and output or its sources under it, in a quiet well indented to
//! its words. A call that only looked is muted; one that changed or ran something is a step
//! stronger. How it stands is said by its mark while it runs or waits, and after it by an icon
//! and a word at its end, never by an edge: no call is a card, so no row of the thread is
//! boxed where its neighbours are not. A request whose call is on screen is answered under the
//! call's line.

use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_proto::thread::{ItemBody, ItemId, Patch, ToolCall, ToolDetail, ToolState};
use slopty_theme::{Rgb, Surfaces};

use super::{
    PEEK_LINES, TOOL_ROW, ThreadView, ThreadViewEvent, call_path, patch_of, path_patch, tail,
    tool_icon,
};
use crate::colors::hsla;
use crate::conversation::diff;
use crate::conversation::lines::{self, Ink};
use crate::conversation::thread::rows;
use crate::icons::Symbol;
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

/// What a call on one file did to it, in the tense of how it stands: "Edited", "Editing" while
/// it runs, "Edit" while it waits to be allowed. `None` for a call of another kind.
const fn file_verb(call: &ToolCall) -> Option<&'static str> {
    let [done, doing, asked] = match &call.detail {
        Some(ToolDetail::Read(_)) => ["Read", "Reading", "Read"],
        Some(ToolDetail::Edit(_)) => ["Edited", "Editing", "Edit"],
        Some(ToolDetail::Write(_)) => ["Wrote", "Writing", "Write"],
        _ => return None,
    };
    Some(match call.state {
        ToolState::Streaming | ToolState::Running => doing,
        ToolState::Pending { .. } => asked,
        _ => done,
    })
}

/// `path` as the machine opens it: absolute, or under `~`, as it is; else under the agent's
/// folder `cwd`.
pub(super) fn opened_at(path: &str, cwd: &str) -> String {
    if path.starts_with('/') || path == "~" || path.starts_with("~/") || cwd.is_empty() {
        return path.to_owned();
    }
    format!("{}/{path}", cwd.trim_end_matches('/'))
}

/// What a call's line says at its end of how it stands, and in which tone, once that is not
/// "done" or "under way" (which its mark and its time say): it waits on the person, it
/// failed, it was not allowed, or it was stopped. A failure is marked with an icon besides.
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
            open.then(|| self.sources(id, call).or_else(|| self.tool_body(id, call, cx))).flatten();
        let pictures = if open { self.pictures_row(&call.images, false, cx) } else { None };
        let answers =
            self.answered_inline(ix).then(|| self.shown_waiting(cx).cloned()).flatten().and_then(
                |request| {
                    self.decision(&request, cx)
                        .map(|d| div().w_full().py(px(self.theme.spacing.xs)).child(d))
                },
            );
        if let Some(ToolDetail::Plan { text }) = &call.detail {
            let answers = answers.map(gpui::IntoElement::into_any_element);
            return self.plan_card(id, call, text, answers, cx);
        }
        let theme = &self.theme;
        // Under the line, indented to its words: the body in its well, the pictures, the
        // answers.
        let under = |el: AnyElement| div().w_full().pl(px(TOOL_ROW + theme.spacing.xs)).child(el);
        div()
            .debug_selector({
                let id = id.0.clone();
                move || format!("call-{id}")
            })
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xxs))
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
                let glyph = call_path(call).map_or_else(|| kind.into(), crate::icons::file_mark);
                crate::icons::symbol(theme, glyph, px(theme.typography.icon()), hsla(s.text_muted))
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
        // Read aloud as it reads: a file's verb and the file, then how it stands.
        let said = match (file_verb(call), call_path(call)) {
            (Some(verb), Some(path)) => format!("{verb} {}", tidy(path, &cwd)),
            _ => title.clone(),
        };
        let label = match (&call.state, child) {
            (ToolState::Streaming, _) => format!("Preparing {}", call.name),
            (_, Some(_)) => format!("Subagent {title}"),
            _ => said,
        };
        let label = match standing(&call.state, &s) {
            Some((word, _)) => format!("{label}, {word}"),
            None => label,
        };
        let path = call_path(call).map(|path| opened_at(path, &cwd));
        let file = call_path(call).map(|path| {
            let (name, dir) = lines::name_first(path);
            (name.to_owned(), tidy(&format!("{dir}/"), &cwd).trim_end_matches('/').to_owned())
        });
        let verb = file_verb(call).filter(|_| file.is_some());
        let a_file = file.is_some();
        let file = file.or(mcp);
        let changes = patch_of(call).and_then(|p| kit::changes(theme, p.added, p.removed));
        let found = match &call.detail {
            Some(ToolDetail::WebSearch(search)) => sources_words(&search.links),
            Some(ToolDetail::Agent(agent)) => agent_words(agent),
            _ => None,
        };
        let quiet = rows::quiet(call);
        let ink = if quiet { s.text_muted } else { s.text_secondary };
        let lead_ink = if a_file { s.text } else { ink };
        let group: SharedString = format!("call-{}", id.0).into();
        let name_id = format!("call-file-{}", id.0);
        let what = match file {
            Some((name, dir)) => div()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(theme.spacing.xs))
                .children(verb.map(|v| div().flex_none().text_color(hsla(ink)).child(v)))
                .child(
                    div()
                        .id(ElementId::Name(name_id.clone().into()))
                        .debug_selector(move || name_id)
                        .flex_none()
                        .max_w(gpui::relative(0.7))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(lead_ink))
                        .child(SharedString::from(name))
                        // The name opens the file; the rest of the line, the call.
                        .when_some(path, |el, path| {
                            el.role(Role::Link)
                                .aria_label(SharedString::from(format!("Open {path}")))
                                .cursor_pointer()
                                .hover(gpui::Styled::underline)
                                .on_click(cx.listener(move |_this, _ev, _w, cx| {
                                    cx.stop_propagation();
                                    cx.emit(ThreadViewEvent::OpenFile { path: path.clone() });
                                }))
                        }),
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
            .gap(px(theme.spacing.xs))
            .min_h(px(TOOL_ROW))
            .text_size(px(theme.typography.small()))
            .cursor_pointer()
            .child(Self::slot().child(mark))
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
            .children(standing(&call.state, &s).map(|(word, tone)| {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xxs))
                    .text_color(hsla(tone))
                    .when(matches!(call.state, ToolState::Failed), |el| {
                        el.child(self.icon(Symbol::Xmark, tone))
                    })
                    .child(word)
            }))
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
                    .text_size(px(theme.typography.small()))
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
    /// A call with neither a diff nor a command (an MCP tool, a skill, a tool this client does
    /// not know) shows what it was called with, as JSON laid out to read, over its output.
    /// Each part shows its first [`PEEK_LINES`] lines (the output its last) until the reader
    /// asks for all of the call.
    fn tool_body(&self, id: &ItemId, call: &ToolCall, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        if let Some((path, patch)) = path_patch(call) {
            return Some(self.patch_well(id, (path, patch), true, cx).into_any_element());
        }
        let whole = self.whole.contains(id);
        let command = match &call.detail {
            Some(ToolDetail::Exec(exec)) => Some(format!("$ {}", exec.command.text)),
            _ => None,
        };
        // A call of a kind this client knows says its input on its line; an MCP tool's, a
        // skill's or an unknown one's is shown.
        let input = match &call.detail {
            None | Some(ToolDetail::Mcp(_)) => called_with(&call.input.text),
            Some(_) => None,
        };
        let output = call.output.as_ref().map(|o| o.text.trim_end().to_owned());
        let longest =
            [&command, &input, &output].into_iter().flatten().map(|t| t.lines().count()).max()?;
        let head = |t: String| if whole { t } else { head_lines(&t, PEEK_LINES) };
        let text = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xs))
            .font_family(self.mono())
            .text_size(px(theme.typography.small()))
            .children(command.map(|c| div().text_color(hsla(s.text)).child(SharedString::from(c))))
            .children(input.map(|i| {
                let id = id.0.clone();
                div()
                    .debug_selector(move || format!("call-input-{id}"))
                    .text_color(hsla(s.text))
                    .whitespace_normal()
                    .child(SharedString::from(head(i)))
            }))
            .children(output.map(|o| {
                div()
                    .text_color(hsla(s.text_secondary))
                    .whitespace_normal()
                    .child(SharedString::from(if whole { o } else { tail(&o, PEEK_LINES) }))
            }));
        let well = self.well(text.px(px(theme.spacing.sm)).py(px(theme.spacing.xs)));
        let more =
            (!whole && longest > PEEK_LINES).then(|| self.show_lines(id, longest as u64, cx));
        Some(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xxs))
                .child(well)
                .children(more)
                .into_any_element(),
        )
    }

    /// `patch` of the file at `path` in a [`Self::well`], under `key` (a call's id, or a
    /// request's). With `peek`, its first [`PEEK_LINES`] lines, then "Show all N lines" until
    /// the reader asks for all of them (a call's, in the thread); without, all of it (a
    /// request's, in a well that scrolls). Once all shows, how many more lines the agent's side
    /// left out, so a cut diff never reads as the whole change.
    pub(super) fn patch_well(
        &self,
        key: &ItemId,
        (path, patch): (&str, &Patch),
        peek: bool,
        cx: &Context<Self>,
    ) -> Div {
        let theme = &self.theme;
        let blocks = Rc::clone(
            self.diffs
                .borrow_mut()
                .entry(key.clone())
                .or_insert_with(|| diff::thread_blocks(path, patch)),
        );
        let ink = Ink { theme, digits: lines::digits(&blocks) };
        let total: usize = blocks.iter().map(|b| b.lines.len()).sum();
        let whole = !peek || self.whole.contains(key) || total <= PEEK_LINES;
        let cap = if whole { usize::MAX } else { PEEK_LINES };
        let mut shown = 0_usize;
        let mut children: Vec<AnyElement> = Vec::new();
        for (ix, block) in blocks.iter().enumerate() {
            if shown >= cap {
                break;
            }
            if ix > 0 {
                children.push(ink.hunk_head(block).into_any_element());
            }
            for line in block.lines.iter().take(cap.saturating_sub(shown)) {
                children.push(ink.unified(line).into_any_element());
                shown = shown.saturating_add(1);
            }
        }
        let selector = format!("patch-{}", key.0);
        let code = ink.code().py(px(theme.spacing.xxs)).children(children);
        let well = self.well(code).debug_selector(move || selector);
        let more = (!whole).then(|| self.show_lines(key, total as u64, cx));
        let left_out = (whole && patch.clipped_lines > 0).then(|| {
            let n = u64::from(patch.clipped_lines);
            div()
                .text_size(px(theme.typography.small()))
                .text_color(hsla(theme.surfaces.text_muted))
                .child(SharedString::from(format!(
                    "{} not shown here",
                    kit::count(n, "more line", "more lines")
                )))
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xxs))
            .child(well)
            .children(more)
            .children(left_out)
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
                    .gap(px(theme.spacing.xs))
                    .min_h(px(TOOL_ROW))
                    .px(px(theme.spacing.xxs))
                    .rounded(px(theme.radii.xs))
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
            .text_size(px(theme.typography.small()))
            .children(rows);
        Some(self.well(list.p(px(theme.spacing.xs))).into_any_element())
    }

    /// An opened call's well: set into the thread's plane ([`kit::inset`]) at a code shell's
    /// `radii.sm`, no edge, clipped to its corners.
    fn well(&self, content: impl gpui::IntoElement) -> Div {
        let theme = &self.theme;
        kit::inset(div(), theme)
            .w_full()
            .overflow_hidden()
            .rounded(px(theme.radii.sm))
            .child(content)
    }
}

/// What a call was called with, as JSON laid out to read; its own text when that is not
/// JSON (cut short on the wire); nothing when it was called with nothing.
fn called_with(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(input) {
        Ok(serde_json::Value::Object(map)) if map.is_empty() => None,
        Ok(serde_json::Value::Null) => None,
        Ok(value) => serde_json::to_string_pretty(&value).ok(),
        Err(_) => Some(input.to_owned()),
    }
}

/// The first `lines` lines of `text`.
fn head_lines(text: &str, lines: usize) -> String {
    text.lines().take(lines).collect::<Vec<_>>().join("\n")
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

/// What a subagent did, beside its line: its kind, the calls it made and the tokens it took
/// ("Explore · 14 tools · 12k tokens"), each as far as the agent says.
fn agent_words(agent: &slopty_proto::thread::detail::AgentDetail) -> Option<String> {
    let parts: Vec<String> = agent
        .agent_type
        .iter()
        .filter(|t| !t.trim().is_empty())
        .map(|t| t.trim().to_owned())
        .chain(agent.tool_uses.map(|n| kit::count(n, "tool", "tools")))
        .chain(agent.tokens.filter(|n| *n > 0).map(|n| format!("{} tokens", super::tokens(n))))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
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

    use super::{
        agent_words, called_with, head_lines, host, humane, sources_words, standing, tidy,
    };

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

    /// A call is shown with what it was called with, laid out to read; a cut input as it came;
    /// an empty one not at all.
    #[test]
    fn a_call_shows_what_it_was_called_with() {
        assert_eq!(
            called_with(r#"{"query":["gpui",3]}"#).as_deref(),
            Some("{\n  \"query\": [\n    \"gpui\",\n    3\n  ]\n}")
        );
        assert_eq!(called_with(r#"{"query":"gp"#).as_deref(), Some(r#"{"query":"gp"#));
        assert_eq!(called_with("{}"), None);
        assert_eq!(called_with("  "), None);
        assert_eq!(head_lines("a\nb\nc", 2), "a\nb");
    }

    /// A subagent's line says its kind, its calls and its tokens, as far as the agent said.
    #[test]
    fn a_subagent_says_what_it_did() {
        use slopty_proto::thread::Clipped;
        use slopty_proto::thread::detail::AgentDetail;
        let mut agent = AgentDetail {
            agent_type: Some("Explore".to_owned()),
            description: None,
            prompt: Clipped::default(),
            background: false,
            report: None,
            tokens: Some(12_000),
            tool_uses: Some(14),
            duration_ms: None,
        };
        assert_eq!(
            agent_words(&agent).as_deref(),
            Some("Explore \u{b7} 14 tools \u{b7} 12k tokens")
        );
        agent.tokens = None;
        agent.tool_uses = Some(1);
        assert_eq!(agent_words(&agent).as_deref(), Some("Explore \u{b7} 1 tool"));
        agent.agent_type = None;
        agent.tool_uses = None;
        assert_eq!(agent_words(&agent), None);
    }
}
