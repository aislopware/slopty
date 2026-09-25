//! One tile: the header (kind, title, place, status, the actions shown on hover or focus) and
//! the body (a terminal, a remote window or display, a note, a file card), with the pill that
//! says when the body cannot show what it should.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Div, ElementId, ExternalPaths, InteractiveElement as _, IntoElement as _,
    MouseButton, MouseDownEvent, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window, div, px,
};
use gpui_kit::component::input::Input;
use slopty_client::layout::{Placed, TileRef};
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::agent::AgentSource;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::terminal::{SessionState, TermRequest};
use slopty_theme::{Theme, alpha};

use super::actions::{CloseItem, FullscreenTile};
use super::agents::needs_human;
use super::{WorkerStatus, WorkspaceView};
use crate::a11y::tab_stop;
use crate::browser::BrowserView;
use crate::chrome_text::ChromeText;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize, Status};

/// A tile's header height, in points at zoom 1.
pub(super) const HEADER_H: f32 = 28.0;

/// The width of the focus ring, in points at zoom 1: the one accent hairline of the design
/// system, laid over the tile's own hairline, so the focused tile's frame turns accent.
const RING: f32 = 1.0;

/// The widest a page's address gets beside its title, in points at zoom 1: the title is what
/// tells tiles apart, the address only says where.
const HEADER_URL_MAX: f32 = 180.0;

/// The height of an upload's progress bar along the bottom of the header.
const PROGRESS: f32 = 2.0;

/// The group every tile's header actions hover with.
const TILE_GROUP: &str = "tile";

/// The group a header's window controls (fullscreen, close) hover with: they show while the
/// pointer is on the header itself, not anywhere over the body.
const HEADER_GROUP: &str = "tile-header";

/// What the in-body pill says while a tile's worker is being dialled again.
pub const RECONNECTING: &str = "Reconnecting…";

/// What the in-body pill says for a shell whose session is gone and whose status is not known.
pub const SESSION_ENDED: &str = "Session ended";

/// The accessible name of a tile's close button.
pub const CLOSE_TILE: &str = "Close tile";

/// The accessible name of a tile's fullscreen button.
pub const FULLSCREEN_TILE: &str = "Fullscreen tile";

/// The accessible name of the "hooks" pill.
pub const INSTALL_HOOKS: &str = "Install hooks";

/// The accessible name of the "take" pill.
pub const TAKE_OVER: &str = "Take over";

/// How much of a note's first line the header shows.
pub const NOTE_TITLE_CHARS: usize = 40;

/// How the chrome is scaled this frame: `k`, the overview's zoom, and whether that zoom is
/// in motion (chrome text then paints from the raster ladder).
#[derive(Clone, Copy, Debug)]
pub(super) struct Chrome {
    pub k: f32,
    pub zooming: bool,
}

/// A note's title, "note" while it is empty.
///
/// Its first non-empty line with Markdown's heading, list, quote and task marks stripped, cut
/// to [`NOTE_TITLE_CHARS`]. A note with task lines counts them after it: `Plan · 1/3`.
#[must_use]
pub fn note_title(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim())
        .map(|l| {
            ["[ ] ", "[x] ", "[X] "]
                .iter()
                .find_map(|mark| l.strip_prefix(mark))
                .unwrap_or(l)
                .trim()
        })
        .find(|l| !l.is_empty());
    let mut title = match line {
        None => "note".to_owned(),
        Some(line) if line.chars().count() > NOTE_TITLE_CHARS => {
            let cut: String = line.chars().take(NOTE_TITLE_CHARS).collect();
            format!("{}…", cut.trim_end())
        }
        Some(line) => line.to_owned(),
    };
    if let Some((done, total)) = note_progress(text) {
        title.push_str(" · ");
        title.push_str(&done.to_string());
        title.push('/');
        title.push_str(&total.to_string());
    }
    title
}

/// How many of a note's task lines are ticked, and how many there are; `None` without any.
#[must_use]
pub fn note_progress(text: &str) -> Option<(usize, usize)> {
    let mut done = 0_usize;
    let mut total = 0_usize;
    for segment in crate::markdown::segments(text) {
        if let crate::markdown::Segment::Task(task) = segment {
            total = total.saturating_add(1);
            done = done.saturating_add(usize::from(task.done));
        }
    }
    (total > 0).then_some((done, total))
}

/// A file card's title: the file's name, with the directory it is in when there is one
/// (`main.rs · src`), so two `mod.rs` cards can be told apart.
#[must_use]
pub fn file_title(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let name = parts.next().filter(|n| !n.is_empty()).unwrap_or(trimmed);
    match parts.next().filter(|d| !d.is_empty()) {
        Some(dir) => format!("{name} · {dir}"),
        None => name.to_owned(),
    }
}

/// Where a shell is, short: the last two components of `path`, with the home directory as `~`
/// (`~`, `~/src`, `oss/slopty`). Only the worker knows its home, so a home is recognised by
/// its shape: `/Users/<name>` on a Mac, `/home/<name>` or `/root` elsewhere.
#[must_use]
pub fn cwd_tail(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    let home = match parts.as_slice() {
        ["Users" | "home", _, ..] => 2,
        ["root", ..] => 1,
        _ => 0,
    };
    let (home, rest) = if trimmed.starts_with('/') && home > 0 {
        (true, parts.get(home..).unwrap_or_default())
    } else {
        (false, parts.as_slice())
    };
    match (home, rest) {
        (true, []) => "~".to_owned(),
        (true, [one]) => format!("~/{one}"),
        (_, [.., parent, name]) => format!("{parent}/{name}"),
        (false, [one]) if trimmed.starts_with('/') => format!("/{one}"),
        (false, [one]) => (*one).to_owned(),
        (false, []) => "/".to_owned(),
    }
}

/// The in-body state of a tile whose body cannot show what it should: what the pill says and
/// what it offers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum BodyState {
    /// The tile's worker is out of reach; the text says how.
    Away(SharedString),
    /// The shell's program exited with this status (a signal negated); its session is
    /// still listed, so it can be started again where it was.
    Exited(i32),
    /// The shell's session is gone and nothing says how it ended.
    Ended,
}

impl BodyState {
    /// What the pill says.
    pub(super) fn text(&self) -> SharedString {
        match self {
            Self::Away(text) => text.clone(),
            Self::Exited(status) if *status < 0 => {
                format!("Exited · signal {}", status.unsigned_abs()).into()
            }
            Self::Exited(status) => format!("Exited · code {status}").into(),
            Self::Ended => SESSION_ENDED.into(),
        }
    }

    /// The status its icon and tone come from.
    const fn status(&self) -> Status {
        match self {
            Self::Away(_) => Status::Away,
            Self::Exited(0) | Self::Ended => Status::Idle,
            Self::Exited(_) => Status::Failed,
        }
    }
}

/// The icon a tile's header leads with: what the tile is.
pub(super) const fn kind_icon(item: &Item, agent: bool) -> IconName {
    match item.kind {
        ItemKind::Terminal { .. } if agent => IconName::Bot,
        ItemKind::Terminal { .. } => IconName::SquareTerminal,
        ItemKind::Window { .. } => IconName::AppWindow,
        ItemKind::Display { .. } => IconName::Monitor,
        ItemKind::Note { .. } => IconName::StickyNote,
        ItemKind::File { .. } => IconName::FileText,
        ItemKind::Browser { .. } => IconName::Globe,
    }
}

/// The word for what an item is: `terminal`, `window`, `display`, `note`, `file`, `browser`.
pub(super) const fn kind_name(item: &Item) -> &'static str {
    match item.kind {
        ItemKind::Terminal { .. } => "terminal",
        ItemKind::Window { .. } => "window",
        ItemKind::Display { .. } => "display",
        ItemKind::Note { .. } => "note",
        ItemKind::File { .. } => "file",
        ItemKind::Browser { .. } => "browser",
    }
}

impl WorkspaceView {
    /// What a header says without a name: the shell's title, the window's, "Display N", a
    /// note's first line, a file's `name · parent`.
    pub(super) fn derived_title(&self, item: &Item, cx: &App) -> String {
        match &item.kind {
            ItemKind::Terminal { session } => self.terminal_title(*session, cx),
            ItemKind::Window { window } => {
                self.titles.get(&item.id).cloned().unwrap_or_else(|| format!("Window {}", window.0))
            }
            ItemKind::Display { display } => format!("Display {display}"),
            ItemKind::Note { text } => note_title(text),
            ItemKind::File { path } => file_title(path),
            ItemKind::Browser { url } => self
                .browsers
                .get(&item.id)
                .map_or_else(|| crate::browser::short_url(url).to_owned(), |v| v.read(cx).title()),
        }
    }

    /// What a header says: the name the human gave the tile, else its derived title.
    #[must_use]
    pub fn card_title(&self, _tile: TileRef, item: &Item, cx: &App) -> String {
        item.name.clone().unwrap_or_else(|| self.derived_title(item, cx))
    }

    /// A terminal's title: what the shell set, else what the worker last said, else "shell".
    #[must_use]
    pub fn terminal_title(&self, session: SessionId, cx: &App) -> String {
        self.terminals
            .get(&session)
            .and_then(|v| v.read(cx).state().title().map(str::to_owned))
            .or_else(|| self.summary(session).map(|s| s.title.clone()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "shell".to_owned())
    }

    /// One tile as the frame places it, `rect` already in the strip's coordinates.
    pub(super) fn render_tile(
        &self,
        placed: &Placed,
        chrome: Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let tile = placed.tile;
        let item = self.item(tile)?.clone();
        let theme = &self.theme;
        let k = chrome.k;
        let id = item.id;
        let focused = placed.focused;
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agent_state(session).map(|a| (session, a)),
            _ => None,
        };
        let needs_you = agent.is_some_and(|(_, a)| needs_human(a));
        let worker_up = self.workers.get(&tile.worker).is_some_and(|w| w.link.is_some());

        let header = self.render_header(placed, &item, chrome, cx);
        let body = self.render_body(placed, &item, chrome, window, cx);
        // Files dropped on a shell go to its directory; on a remote window, to the worker's
        // clipboard.
        let takes_files = worker_up
            && matches!(
                item.kind,
                ItemKind::Terminal { .. } | ItemKind::Window { .. } | ItemKind::Display { .. }
            );
        let accent = theme.surfaces.accent;

        // The open animation grows the tile about its centre.
        let rect = placed.rect;
        let (width, height) = (rect.w * placed.scale, rect.h * placed.scale);
        let (left, top) = (rect.x + (rect.w - width) / 2.0, rect.y + (rect.h - height) / 2.0);
        let radius = theme.radii.md * k;
        // The ring is drawn over the tile, so gaining or losing the focus never moves the
        // content by a point (a terminal would re-fit its grid).
        let ring = (focused || needs_you).then(|| {
            div().absolute().inset_0().rounded(px(radius)).border(px(RING * k)).border_color(hsla(
                if needs_you { theme.surfaces.warn } else { theme.surfaces.accent },
            ))
        });
        Some(
            div()
                .id(ElementId::Uuid(*id.as_uuid()))
                .debug_selector(move || format!("item-{}", id.as_uuid()))
                .group(TILE_GROUP)
                .role(Role::Group)
                .aria_label(SharedString::from(self.card_title(tile, &item, cx)))
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(height))
                .opacity(placed.alpha)
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(px(radius))
                .border_1()
                .border_color(hsla(theme.surfaces.border))
                .bg(hsla(theme.terminal.bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| this.click_tile(tile, cx)),
                )
                .when(takes_files, |el| {
                    el.drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.border_color(hsla(accent))
                    })
                    .on_drop(cx.listener(
                        move |this, paths: &ExternalPaths, _w, cx| {
                            this.drop_files(tile, paths.paths(), cx);
                        },
                    ))
                })
                .child(header)
                .child(body)
                .when_some(ring, gpui::ParentElement::child)
                .into_any_element(),
        )
    }

    /// Whether a tile's body may be drawn from its cached view. A cached view replays last
    /// frame's paint, keyboard input handler included, until the view itself is notified; the
    /// focused tile (and the one focused last frame) is where the keyboard is moving, so it
    /// always draws afresh. It is also the tile being typed into, so caching it would save
    /// nothing.
    fn cacheable(&self, placed: &Placed) -> bool {
        !placed.focused && self.drawn_focus != Some(placed.tile)
    }

    /// A tile fading out where it stood: its frame only, the content already gone.
    pub(super) fn render_closing(
        &self,
        rect: slopty_client::layout::Rect,
        alpha: f32,
        scale: f32,
        k: f32,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let (w, h) = (rect.w * scale, rect.h * scale);
        div()
            .absolute()
            .left(px(rect.x + (rect.w - w) / 2.0))
            .top(px(rect.y + (rect.h - h) / 2.0))
            .w(px(w))
            .h(px(h))
            .opacity(alpha)
            .rounded(px(theme.radii.md * k))
            .border_1()
            .border_color(hsla(theme.surfaces.border))
            .bg(hsla(theme.terminal.bg))
            .into_any_element()
    }

    fn render_header(
        &self,
        placed: &Placed,
        item: &Item,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tile = placed.tile;
        let id = item.id;
        let k = chrome.k;
        let focused = placed.focused;
        let title = self.card_title(tile, item, cx);
        let kind = kind_name(item);
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agent_state(session).map(|a| (session, a)),
            _ => None,
        };
        let ink = if focused { s.text } else { s.text_muted };
        let status = self.tile_status(tile, item, cx);
        let mark = status_slot(theme, status, id, k);
        let badge = agent.map(|(session, a)| self.agent_badge(tile, session, a, chrome, cx));
        let unwatched = match &item.kind {
            ItemKind::Terminal { session } => self.finished.get(session).map(|f| (*session, f)),
            _ => None,
        };
        let finished =
            unwatched.map(|(session, f)| self.finished_badge(tile, session, f, chrome, cx));
        // Something ended in this tile while the human was elsewhere; looking at it clears it.
        let unseen = unwatched.is_some().then(|| {
            div()
                .id("unseen")
                .debug_selector(move || format!("unseen-{}", id.as_uuid()))
                .role(Role::Image)
                .aria_label("Unseen")
                .flex_none()
                .size(px((theme.spacing.xs + theme.spacing.xxs) * k))
                .rounded_full()
                .bg(hsla(s.accent))
        });
        // An agent the worker had to guess at: offer the hooks that would make it precise.
        let hooks = agent
            .filter(|(_, a)| a.source != AgentSource::Hook && !self.hooks_offered(tile.worker))
            .map(|_| {
                let worker = tile.worker;
                let pill = pill("hooks", id, "hooks", s.warn, theme, chrome)
                    .role(Role::Button)
                    .aria_label(INSTALL_HOOKS);
                tab_stop(pill, s.accent)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.install_hooks(worker, cx)))
                    .into_any_element()
            });
        // An upload in flight says how far it got; a click stops it.
        let upload = self.upload_on(tile).map(|(xfer, upload)| {
            let pill = pill("upload", id, upload.label(), s.text_secondary, theme, chrome)
                .role(Role::Button)
                .aria_label("Cancel upload");
            let bar = div()
                .absolute()
                .left_0()
                .bottom_0()
                .h(px(PROGRESS * k))
                .w(gpui::relative(upload.fraction()))
                .bg(hsla(s.accent));
            let pill = tab_stop(pill, s.accent)
                .on_click(cx.listener(move |this, _ev, _w, cx| this.cancel_upload(xfer, cx)))
                .into_any_element();
            (pill, bar)
        });
        let (upload, progress) = upload.unzip();
        let ports = self.port_pills(tile, item, chrome, cx);
        let actions = self.header_actions(tile, item, chrome, cx);
        // The actions stay out of sight until the tile is hovered or focused: a wall of tiles
        // reads as titles, not buttons. Touch has no hover, so the focused tile shows them.
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .when(!focused, |el| el.invisible().group_hover(TILE_GROUP, gpui::Styled::visible))
            .children(actions);
        let controls = self.window_controls(tile, focused, k, cx);
        let tabs = placed.tabs.map(|(active, count)| {
            div().flex_none().text_color(hsla(s.text_muted)).child(ChromeText::new(
                format!("{}/{count}", active.saturating_add(1)),
                px(theme.typography.small()),
                k,
            ))
        });
        // A file with an edit not yet on disk says so with a dot after its name, as an editor's
        // tab does; saving keeps the dot until the worker has written it.
        let unsaved = self.files.get(&id).is_some_and(|v| {
            let v = v.read(cx);
            v.dirty() || v.saving()
        });
        let unsaved = unsaved.then(|| {
            div()
                .id("unsaved")
                .debug_selector(move || format!("unsaved-{}", id.as_uuid()))
                .role(Role::Image)
                .aria_label("Unsaved changes")
                .flex_none()
                .size(px(theme.spacing.sm * k))
                .rounded_full()
                .bg(hsla(s.text_secondary))
        });
        // Where the tile is: a shell's directory, a page's address when the title is not it.
        let place = match &item.kind {
            ItemKind::Terminal { session } => {
                self.summary(*session).and_then(|s| s.cwd.as_deref()).map(cwd_tail)
            }
            ItemKind::Browser { .. } => self.browsers.get(&id).and_then(|v| {
                let v = v.read(cx);
                (item.name.is_some() || !v.page().title.trim().is_empty())
                    .then(|| v.short_url().to_owned())
            }),
            _ => None,
        };
        let place = place.map(|text| {
            div()
                .id("place")
                .debug_selector(move || format!("place-{}", id.as_uuid()))
                .role(Role::Label)
                .aria_label(SharedString::from(text.clone()))
                .min_w_0()
                .max_w(px(HEADER_URL_MAX * k))
                .overflow_hidden()
                .text_color(hsla(s.text_muted))
                .child(
                    ChromeText::new(text, px(theme.typography.ui_size), k)
                        .fill()
                        .zooming(chrome.zooming),
                )
        });
        // The worker's name, where more than one could be meant.
        let worker_chip =
            (self.workers.len() > 1).then(|| self.workers.get(&tile.worker)).flatten().map(|w| {
                let name = w.name.clone();
                div()
                    .id("worker")
                    .debug_selector(move || format!("worker-{}", id.as_uuid()))
                    .role(Role::Label)
                    .aria_label(SharedString::from(name.clone()))
                    .flex_none()
                    .px(px(theme.spacing.sm * k))
                    .py(px(theme.spacing.xxs * k))
                    .rounded(px(theme.radii.xs * k))
                    .bg(hsla_alpha(s.text_secondary, alpha::FAINT))
                    .text_size(px(theme.typography.small() * k))
                    .text_color(hsla(s.text_secondary))
                    .child(
                        ChromeText::new(name, px(theme.typography.small()), k)
                            .zooming(chrome.zooming),
                    )
            });
        let lead = self.file_proxy(item, tile, ink, chrome, cx).unwrap_or_else(|| {
            let agent = agent.is_some();
            crate::icons::icon(theme, kind_icon(item, agent), IconSize::Inline, hsla(ink))
                .size(px(theme.typography.icon() * k))
                .into_any_element()
        });
        let renaming = self.rename.as_ref().filter(|r| r.tile == tile).map(|r| r.input.clone());
        let heading = SharedString::from(if kind == title {
            title.clone()
        } else {
            format!("{kind} {title}")
        });
        let name = match renaming {
            // The name field takes the title's place; a click in it must not start a move.
            Some(input) => div()
                .id("rename")
                .debug_selector(move || format!("rename-{}", id.as_uuid()))
                .flex_1()
                .overflow_hidden()
                .cursor_text()
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                .child(Input::new(&input).aria_label("Tile name"))
                .into_any_element(),
            None => div()
                .min_w_0()
                .overflow_hidden()
                .child(
                    ChromeText::new(title, px(theme.typography.ui_size), k)
                        .fill()
                        .zooming(chrome.zooming),
                )
                .into_any_element(),
        };
        // The title and its place share what the right side leaves, the place giving way
        // first only by being the shorter of the two.
        let names = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .child(name)
            .children(place);
        // The focused tile's header is its body's surface, so the tile reads as one piece; the
        // others step up to the panel with a hairline under them.
        let (surface, rule) = if focused {
            (gpui::transparent_black(), gpui::transparent_black())
        } else {
            (hsla(s.panel), hsla(s.border))
        };
        div()
            .id("title")
            .debug_selector(move || format!("title-{}", id.as_uuid()))
            .group(HEADER_GROUP)
            .role(Role::Heading)
            .aria_label(heading)
            .relative()
            .h(px(HEADER_H * k))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .px(px(theme.spacing.sm * k))
            .gap(px(theme.spacing.sm * k))
            .bg(surface)
            .border_b_1()
            .border_color(rule)
            .text_size(px(theme.typography.ui_size * k))
            .text_color(hsla(ink))
            .font_family(theme.typography.ui_family.clone())
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    // The second click of a double-click names the tile; the first began a
                    // move that its mouse-up ended.
                    if ev.click_count == 2 {
                        this.start_rename(tile, window, cx);
                    } else {
                        this.begin_move(tile, ev, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .child(lead)
            .child(names)
            .when_some(unsaved, gpui::ParentElement::child)
            .when_some(tabs, gpui::ParentElement::child)
            .children(ports)
            .when_some(upload, gpui::ParentElement::child)
            .when_some(hooks, gpui::ParentElement::child)
            .when_some(worker_chip, gpui::ParentElement::child)
            .child(mark)
            .when_some(badge, gpui::ParentElement::child)
            .when_some(finished, gpui::ParentElement::child)
            .when_some(unseen, gpui::ParentElement::child)
            .child(actions)
            .child(controls)
            .when_some(progress, gpui::ParentElement::child)
            .into_any_element()
    }

    /// How the tile is doing, in the one status vocabulary: its agent's state; else, out of
    /// reach; else a shell's last command, failed or finished unwatched.
    pub(super) fn tile_status(&self, tile: TileRef, item: &Item, cx: &App) -> Option<Status> {
        let session = match item.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        };
        if let Some(status) = session.and_then(|s| self.agent_state(s)).and_then(Status::of_agent) {
            return Some(status);
        }
        if self.workers.get(&tile.worker).is_none_or(|w| w.link.is_none()) {
            return Some(Status::Away);
        }
        let session = session?;
        let failed = |exit: i64| if exit == 0 { Status::Done } else { Status::Failed };
        if let Some(done) = self.finished.get(&session) {
            return Some(failed(done.exit.map_or(0, i64::from)));
        }
        if let Some(SessionState::Exited { status }) = self.summary(session).map(|s| &s.state) {
            return Some(failed(i64::from(*status)));
        }
        // The newest prompt carries the status of the command before it: one lookup in the
        // prompt index, never a walk over the rows.
        let state = self.terminals.get(&session)?.read(cx).state();
        if state.command_running() {
            return None;
        }
        let prompt = state.prompt_before(slopty_grid::LineIndex(u64::MAX))?;
        let exit = state.line(prompt)?.mark.exit()?;
        (exit != 0).then_some(Status::Failed)
    }

    /// The ports a shell listens on, served here: the number opens the page in a tile, the
    /// arrow beside it in the default browser.
    fn port_pills(
        &self,
        tile: TileRef,
        item: &Item,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let id = item.id;
        let ItemKind::Terminal { session } = &item.kind else { return Vec::new() };
        self.forwards(*session)
            .iter()
            .filter(|f| f.local.is_some())
            .map(|forward| {
                let number = forward.port.number;
                let label = match forward.local {
                    Some(local) if local != number => format!("{number} \u{2192} {local}"),
                    _ => format!("{number}"),
                };
                let tone = theme.surfaces.text_secondary;
                let in_tile = pill(format!("port-{number}"), id, label, tone, theme, chrome)
                    .role(Role::Link)
                    .aria_label(SharedString::from(format!("Open port {number} in a tile")));
                let out = pill(format!("port-out-{number}"), id, "\u{2197}", tone, theme, chrome)
                    .role(Role::Link)
                    .aria_label(SharedString::from(format!("Open port {number} in the browser")));
                let url = forward.worker_url();
                let worker = tile.worker;
                let forward = forward.clone();
                div()
                    .flex()
                    .flex_none()
                    .gap(px(theme.spacing.xxs * chrome.k))
                    .child(tab_stop(in_tile, theme.surfaces.accent).on_click(cx.listener(
                        move |this, _ev, _w, cx| this.open_browser(Some(worker), &url, cx),
                    )))
                    .child(
                        tab_stop(out, theme.surfaces.accent)
                            .on_click(move |_ev, _w, _cx| Self::open_forward(&forward)),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// What a tile's kind offers in its header: take the PTY's size, mute, a page's back and
    /// reload.
    fn header_actions(
        &self,
        tile: TileRef,
        item: &Item,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let id = item.id;
        // A muted window always says so: a silenced tile must not pass for a quiet one.
        let muted = self.screens.get(&id).is_some_and(|v| v.read(cx).muted());
        let mut actions: Vec<gpui::AnyElement> = Vec::new();
        match &item.kind {
            ItemKind::Terminal { session } => {
                // Another client's size rules this PTY: offer to take it.
                if self.terminals.get(session).is_some_and(|v| !v.read(cx).driving()) {
                    let pill = pill("take", id, "take", theme.surfaces.accent, theme, chrome)
                        .role(Role::Button)
                        .aria_label(TAKE_OVER);
                    actions.push(
                        tab_stop(pill, theme.surfaces.accent)
                            .on_click(
                                cx.listener(move |this, _ev, _w, cx| this.take_over(tile, cx)),
                            )
                            .into_any_element(),
                    );
                }
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                if let Some(view) = self.screens.get(&id).map(|v| v.read(cx))
                    && (muted || view.has_audio())
                {
                    let (label, tone) = if muted {
                        ("muted", theme.surfaces.warn)
                    } else {
                        ("mute", theme.surfaces.text_secondary)
                    };
                    let pill = pill("mute", id, label, tone, theme, chrome)
                        .role(Role::Button)
                        .aria_label(if muted { "unmute" } else { "mute" });
                    actions.push(
                        tab_stop(pill, theme.surfaces.accent)
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                if let Some(view) = this.screens.get(&id) {
                                    view.read(cx).toggle_mute();
                                    cx.notify();
                                }
                            }))
                            .into_any_element(),
                    );
                }
            }
            ItemKind::Browser { .. } => {
                if let Some(view) = self.browsers.get(&id).cloned() {
                    let tone = theme.surfaces.text_secondary;
                    if view.read(cx).page().can_go_back {
                        let back = pill("back", id, "\u{2190}", tone, theme, chrome)
                            .role(Role::Button)
                            .aria_label("Back");
                        let target = view.clone();
                        actions.push(
                            tab_stop(back, theme.surfaces.accent)
                                .on_click(move |_ev, _w, cx| target.update(cx, BrowserView::back))
                                .into_any_element(),
                        );
                    }
                    let reload = pill("reload", id, "\u{21bb}", tone, theme, chrome)
                        .role(Role::Button)
                        .aria_label("Reload");
                    actions.push(
                        tab_stop(reload, theme.surfaces.accent)
                            .on_click(move |_ev, _w, cx| view.update(cx, BrowserView::reload))
                            .into_any_element(),
                    );
                }
            }
            ItemKind::Note { .. } | ItemKind::File { .. } => {}
        }
        actions
    }

    /// Fullscreen and close, at the header's right end: shown while the pointer is on the
    /// header, and always on the focused tile (touch has no hover). Each focuses its tile and
    /// runs the action its key runs, so a click and ⌃⌘F or ⌘W do the same thing.
    fn window_controls(
        &self,
        tile: TileRef,
        focused: bool,
        k: f32,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let id = tile.item.as_uuid();
        let fullscreen = crate::kit::icon_button_at(
            theme,
            format!("fullscreen-{id}"),
            IconName::Maximize2,
            FULLSCREEN_TILE,
            k,
        )
        .on_click(cx.listener(move |this, _ev, window, cx| {
            this.focus_tile(tile, cx);
            // The action runs from the focused element up; the workspace takes the focus when
            // nothing inside it has it, so the action reaches the handler on its root.
            if !this.focus.contains_focused(window, cx) {
                window.focus(&this.focus, cx);
            }
            window.dispatch_action(Box::new(FullscreenTile), cx);
        }));
        let close =
            crate::kit::icon_button_at(theme, format!("close-{id}"), IconName::X, CLOSE_TILE, k)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    this.close_tile(tile, window, cx);
                }));
        div()
            .flex()
            .flex_none()
            .items_center()
            .when(!focused, |el| el.invisible().group_hover(HEADER_GROUP, gpui::Styled::visible))
            .child(fullscreen)
            .child(close)
            .into_any_element()
    }

    /// Close `tile` as ⌘W closes the focused one: a shell asks first while its command runs,
    /// and the closing can be taken back.
    fn close_tile(&mut self, tile: TileRef, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_tile(tile, cx);
        self.close_item(&CloseItem, window, cx);
    }

    /// Start an exited shell again: the same command in the same directory on the same worker,
    /// in a column beside the old one, which goes.
    fn restart_session(&mut self, tile: TileRef, session: SessionId, cx: &mut Context<Self>) {
        let Some(summary) = self.summary(session) else { return };
        let cwd = summary.cwd.clone();
        let command = summary.command.clone();
        let title = command.first().cloned();
        self.focus_tile(tile, cx);
        self.open_session_on(tile.worker, cwd, command, title, cx);
        self.send(tile.worker, ClientMsg::Term { session, req: TermRequest::Close });
        self.propose(tile.worker, ItemOp::Remove(tile.item), cx);
    }

    /// What the body cannot show and why, if anything: the worker out of reach, the shell
    /// exited, the session gone.
    pub(super) fn body_state(&self, tile: TileRef, item: &Item) -> Option<BodyState> {
        let worker = self.workers.get(&tile.worker);
        if worker.is_none_or(|w| w.link.is_none()) {
            let name = worker.map_or("The worker", |w| w.name.as_str());
            return Some(BodyState::Away(match worker.map(|w| &w.status) {
                Some(WorkerStatus::Unreachable) => format!("{name} is unreachable").into(),
                Some(WorkerStatus::Gone) => format!("{name} is gone").into(),
                _ => RECONNECTING.into(),
            }));
        }
        let ItemKind::Terminal { session } = item.kind else { return None };
        match self.summary(session).map(|s| &s.state) {
            Some(SessionState::Exited { status }) => Some(BodyState::Exited(*status)),
            Some(SessionState::Running) => None,
            None if self.terminals.contains_key(&session) => None,
            None => Some(BodyState::Ended),
        }
    }

    /// The pill at the foot of a body saying what is wrong and what to do about it, over
    /// whatever the body still shows. Never a dialog: the rest of the workspace goes on.
    fn render_state_pill(
        &self,
        tile: TileRef,
        item: &Item,
        state: &BodyState,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let id = tile.item;
        let status = state.status();
        let text = state.text();
        let button = |part: &'static str, label: &'static str| {
            let el = div()
                .id(part)
                .debug_selector(move || format!("{part}-{}", id.as_uuid()))
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .px(px(theme.spacing.sm * k))
                .py(px(theme.spacing.xxs * k))
                .rounded(px(theme.radii.sm * k))
                .text_color(hsla(s.accent))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.raised)))
                .child(ChromeText::new(label, px(theme.typography.small()), k));
            tab_stop(el, s.accent)
        };
        let session = match item.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        };
        let restart = match (state, session) {
            (BodyState::Exited(_), Some(session)) => Some(button("restart", "Restart").on_click(
                cx.listener(move |this, _ev, _w, cx| this.restart_session(tile, session, cx)),
            )),
            _ => None,
        };
        let close = matches!(state, BodyState::Exited(_) | BodyState::Ended).then(|| {
            button("close-ended", "Close").on_click(
                cx.listener(move |this, _ev, window, cx| this.close_tile(tile, window, cx)),
            )
        });
        let pill = div()
            .id("state")
            .debug_selector(move || format!("state-{}", id.as_uuid()))
            .role(Role::Status)
            .aria_label(text.clone())
            .occlude()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .pl(px(theme.spacing.md * k))
            .pr(px(if restart.is_some() || close.is_some() {
                theme.spacing.xs
            } else {
                theme.spacing.md
            } * k))
            .py(px(theme.spacing.xs * k))
            .rounded(px(theme.radii.md * k))
            .border_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .shadow_sm()
            .text_size(px(theme.typography.small() * k))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text_secondary))
            .child(
                crate::icons::icon(
                    theme,
                    status.icon(),
                    IconSize::Inline,
                    hsla(status.tone(theme)),
                )
                .size(px(theme.typography.icon() * k)),
            )
            .child(ChromeText::new(text, px(theme.typography.small()), k).zooming(chrome.zooming))
            .when_some(restart, gpui::ParentElement::child)
            .when_some(close, gpui::ParentElement::child);
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(theme.spacing.lg * k))
            .flex()
            .justify_center()
            .child(pill)
            .into_any_element()
    }

    /// A file tile's proxy: its kind icon made draggable, as a Mac document window's title
    /// icon is. Dragged, the file leaves the app as a promise the worker keeps; the rest of the
    /// header still moves the tile.
    #[cfg(target_os = "macos")]
    fn file_proxy(
        &self,
        item: &Item,
        tile: TileRef,
        ink: slopty_theme::Rgb,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let ItemKind::File { path } = &item.kind else { return None };
        let theme = &self.theme;
        let k = chrome.k;
        let id = item.id;
        let path = path.clone();
        let worker = tile.worker;
        Some(
            div()
                .id("file-proxy")
                .debug_selector(move || format!("file-proxy-{}", id.as_uuid()))
                .role(Role::Button)
                .aria_label("Drag the file out")
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(px(theme.typography.icon_large() * k))
                .rounded(px(theme.radii.xs * k))
                .hover(|s| s.bg(hsla_alpha(theme.surfaces.text_secondary, alpha::FAINT)))
                .cursor_grab()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| {
                        this.drag_out(worker, &path);
                        cx.stop_propagation();
                    }),
                )
                .child(
                    crate::icons::icon(theme, IconName::FileText, IconSize::Inline, hsla(ink))
                        .size(px(theme.typography.icon() * k)),
                )
                .into_any_element(),
        )
    }

    /// No file leaves the app by a drag here.
    #[cfg(not(target_os = "macos"))]
    #[expect(clippy::unused_self, reason = "the macOS twin draws the proxy")]
    const fn file_proxy(
        &self,
        _item: &Item,
        _tile: TileRef,
        _ink: slopty_theme::Rgb,
        _chrome: Chrome,
        _cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        None
    }

    /// The body. A terminal's grid is sized from where its tile comes to rest, not from the
    /// rectangle in motion, so a sliding or springing column resizes no PTY frame by frame:
    /// the grid is laid out at the resting size and clipped to the moving one.
    fn render_body(
        &self,
        placed: &Placed,
        item: &Item,
        chrome: Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let content = self.render_content(placed, item, chrome, window, cx);
        match self.body_state(placed.tile, item) {
            Some(state) => {
                let pill = self.render_state_pill(placed.tile, item, &state, chrome, cx);
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(content)
                    .child(pill)
                    .into_any_element()
            }
            None => content,
        }
    }

    /// What the body shows under any state pill: the view, or an empty well where the pill
    /// says why there is none.
    fn render_content(
        &self,
        placed: &Placed,
        item: &Item,
        chrome: Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        let worker_up = self.workers.get(&placed.tile.worker).is_some_and(|w| w.link.is_some());
        let rest_w = placed.target.w.mul_add(k, -2.0).max(1.0);
        let rest_h = (placed.target.h - HEADER_H).mul_add(k, -2.0).max(1.0);
        let fixed = |el: gpui::AnyElement| {
            div()
                .flex_1()
                .w_full()
                .relative()
                .overflow_hidden()
                .child(div().absolute().top_0().left_0().w(px(rest_w)).h(px(rest_h)).child(el))
                .into_any_element()
        };
        let muted_line = |text: SharedString| {
            div()
                .flex_1()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme.typography.small() * k))
                .text_color(hsla(theme.surfaces.text_muted))
                .font_family(theme.typography.ui_family.clone())
                .child(text)
                .into_any_element()
        };
        // A remote picture that has not arrived waits in a well of the canvas colour, so it
        // reads as a screen still to come and not as an empty shell.
        let picture_wait = |text: SharedString| {
            div()
                .flex_1()
                .w_full()
                .flex()
                .bg(hsla(theme.surfaces.canvas))
                .child(muted_line(text))
                .into_any_element()
        };
        // The state pill says what is wrong; the body under it stays empty.
        let well = || div().flex_1().w_full().into_any_element();
        let picture_well =
            || div().flex_1().w_full().bg(hsla(theme.surfaces.canvas)).into_any_element();
        match &item.kind {
            ItemKind::Terminal { session } => match self.terminals.get(session) {
                Some(view) => {
                    let restyled = view.update(cx, |v, _| {
                        v.set_zoom(k);
                        v.set_zooming(chrome.zooming)
                    });
                    let body = if self.cacheable(placed) && !restyled {
                        view.clone()
                            .cached(StyleRefinement::default().size_full())
                            .into_any_element()
                    } else {
                        view.clone().into_any_element()
                    };
                    fixed(body)
                }
                None if !worker_up => well(),
                None if self.summary(*session).is_some() => muted_line("attaching…".into()),
                None => well(),
            },
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                match self.screens.get(&item.id) {
                    Some(view) => {
                        let painted = placed.rect.w * window.scale_factor();
                        view.update(cx, |v, _| v.set_painted_width(painted));
                        let body = if self.cacheable(placed) {
                            view.clone()
                                .cached(StyleRefinement::default().size_full())
                                .into_any_element()
                        } else {
                            view.clone().into_any_element()
                        };
                        div().flex_1().w_full().overflow_hidden().child(body).into_any_element()
                    }
                    None if !worker_up => picture_well(),
                    None if item.sleeping => picture_wait("sleeping".into()),
                    None if self.parked.contains(&item.id) => {
                        picture_wait("paused off screen".into())
                    }
                    None => picture_wait("opening…".into()),
                }
            }
            ItemKind::Note { .. } => match self.notes.get(&item.id) {
                Some(view) => {
                    let (pad, text_size) = (theme.spacing.sm, theme.typography.ui_size);
                    view.update(cx, |v, _| v.set_layout(k, pad, text_size));
                    div()
                        .flex_1()
                        .w_full()
                        .overflow_hidden()
                        .font_family(theme.typography.ui_family.clone())
                        .text_color(hsla(theme.surfaces.text))
                        .child(view.clone())
                        .into_any_element()
                }
                None => muted_line("note".into()),
            },
            ItemKind::Browser { .. } => match self.browsers.get(&item.id) {
                Some(view) => {
                    view.update(cx, |v, _| v.set_alpha(placed.alpha));
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(view.clone())
                        .into_any_element()
                }
                None => muted_line("opening…".into()),
            },
            ItemKind::File { .. } => match self.files.get(&item.id) {
                Some(view) => {
                    let (pad, text_size) = (theme.spacing.sm, theme.typography.mono_size);
                    view.update(cx, |v, _| v.set_layout(k, pad, text_size));
                    let body = if self.cacheable(placed) {
                        view.clone()
                            .cached(StyleRefinement::default().size_full())
                            .into_any_element()
                    } else {
                        view.clone().into_any_element()
                    };
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(body)
                        .into_any_element()
                }
                None if !worker_up => well(),
                None => muted_line("reading…".into()),
            },
        }
    }
}

/// A tile's status in its fixed slot, as [`crate::icons::status_mark`] draws it everywhere
/// else, but at the chrome's zoom `k` (the overview shrinks the header) and named for a
/// screen reader.
fn status_slot(
    theme: &Theme,
    status: Option<Status>,
    item: slopty_core::ItemId,
    k: f32,
) -> gpui::AnyElement {
    let slot = div()
        .id("status")
        .debug_selector(move || format!("status-{}", item.as_uuid()))
        .flex_none()
        .size(px(theme.typography.icon_large() * k))
        .flex()
        .items_center()
        .justify_center();
    match status {
        Some(status) => slot
            .role(Role::Image)
            .aria_label(status.label())
            .child(
                crate::icons::icon(
                    theme,
                    status.icon(),
                    IconSize::Inline,
                    hsla(status.tone(theme)),
                )
                .size(px(theme.typography.icon() * k)),
            )
            .into_any_element(),
        None => slot.into_any_element(),
    }
}

/// A header pill: `small()` type on a faint fill of its tone, the tone as text, `radii.xs`;
/// hover deepens the fill. Scaled by the chrome's `k`. Its id is scoped by the tile's.
fn pill(
    part: impl Into<SharedString>,
    item: slopty_core::ItemId,
    label: impl Into<SharedString>,
    tone: slopty_theme::Rgb,
    theme: &Theme,
    chrome: Chrome,
) -> Stateful<Div> {
    let k = chrome.k;
    let part: SharedString = part.into();
    let selector = format!("{part}-{}", item.as_uuid());
    div()
        .id(part)
        .debug_selector(move || selector)
        .flex_none()
        .px(px(theme.spacing.sm * k))
        .py(px(theme.spacing.xxs * k))
        .rounded(px(theme.radii.xs * k))
        .bg(hsla_alpha(tone, alpha::FAINT))
        .text_size(px(theme.typography.small() * k))
        .text_color(hsla(tone))
        .cursor_pointer()
        .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT)))
        .child(ChromeText::new(label, px(theme.typography.small()), k).zooming(chrome.zooming))
}
