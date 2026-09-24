//! One tile: the header (status dot, title, the actions shown on hover or focus) and the body
//! (a terminal, a remote window or display, a note, a file card).

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
use slopty_proto::agent::AgentSource;
use slopty_proto::items::{Item, ItemKind};
use slopty_theme::{Theme, alpha};

use super::agents::needs_human;
use super::{WorkerStatus, WorkspaceView};
use crate::a11y::tab_stop;
use crate::browser::BrowserView;
use crate::chrome_text::ChromeText;
use crate::colors::{hsla, hsla_alpha};

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

        let header = self.render_header(placed, &item, chrome, worker_up, cx);
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
        worker_up: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
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
        let badge = agent.map(|(session, a)| self.agent_badge(tile, session, a, chrome, cx));
        let finished = match &item.kind {
            ItemKind::Terminal { session } => self
                .finished
                .get(session)
                .map(|f| self.finished_badge(tile, *session, f, chrome, cx)),
            _ => None,
        };
        // An agent the worker had to guess at: offer the hooks that would make it precise.
        let hooks = agent
            .filter(|(_, a)| a.source != AgentSource::Hook && !self.hooks_offered(tile.worker))
            .map(|_| {
                let worker = tile.worker;
                let pill = pill("hooks", id, "hooks", theme.surfaces.warn, theme, chrome)
                    .role(Role::Button)
                    .aria_label(INSTALL_HOOKS);
                tab_stop(pill, theme.surfaces.accent)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.install_hooks(worker, cx)))
                    .into_any_element()
            });
        // An upload in flight says how far it got; a click stops it.
        let upload = self.upload_on(tile).map(|(xfer, upload)| {
            let pill =
                pill("upload", id, upload.label(), theme.surfaces.text_secondary, theme, chrome)
                    .role(Role::Button)
                    .aria_label("Cancel upload");
            let bar = div()
                .absolute()
                .left_0()
                .bottom_0()
                .h(px(PROGRESS * k))
                .w(gpui::relative(upload.fraction()))
                .bg(hsla(theme.surfaces.accent));
            let pill = tab_stop(pill, theme.surfaces.accent)
                .on_click(cx.listener(move |this, _ev, _w, cx| this.cancel_upload(xfer, cx)))
                .into_any_element();
            (pill, bar)
        });
        let (upload, progress) = upload.unzip();
        // The ports a shell listens on, served here: the number opens the page in a tile, the
        // arrow beside it in the default browser.
        let ports: Vec<gpui::AnyElement> = match &item.kind {
            ItemKind::Terminal { session } => self
                .forwards(*session)
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
                    let out =
                        pill(format!("port-out-{number}"), id, "\u{2197}", tone, theme, chrome)
                            .role(Role::Link)
                            .aria_label(SharedString::from(format!(
                                "Open port {number} in the browser"
                            )));
                    let url = forward.worker_url();
                    let worker = tile.worker;
                    let forward = forward.clone();
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(theme.spacing.xxs * k))
                        .child(tab_stop(in_tile, theme.surfaces.accent).on_click(cx.listener(
                            move |this, _ev, _w, cx| this.open_browser(Some(worker), &url, cx),
                        )))
                        .child(
                            tab_stop(out, theme.surfaces.accent)
                                .on_click(move |_ev, _w, _cx| Self::open_forward(&forward)),
                        )
                        .into_any_element()
                })
                .collect(),
            _ => Vec::new(),
        };
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
        // The actions stay out of sight until the tile is hovered or focused: a wall of tiles
        // reads as titles, not buttons. Touch has no hover, so the focused tile shows them.
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .when(!focused, |el| el.invisible().group_hover(TILE_GROUP, gpui::Styled::visible))
            .children(actions);
        let tabs = placed.tabs.map(|(active, count)| {
            div().flex_none().text_color(hsla(theme.surfaces.text_muted)).child(ChromeText::new(
                format!("{}/{count}", active.saturating_add(1)),
                px(theme.typography.small()),
                k,
            ))
        });
        // The dot says one thing, that the tile's worker is away; the ring already says which
        // tile has the focus.
        let away = (!worker_up).then(|| {
            div()
                .flex_none()
                .size(px(theme.spacing.xs * k))
                .rounded_full()
                .bg(hsla(theme.surfaces.warn))
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
                .bg(hsla(theme.surfaces.text_secondary))
        });
        // A page's address, quietly after its title, when the title is not already it.
        let address = self.browsers.get(&id).and_then(|v| {
            let v = v.read(cx);
            (item.name.is_some() || !v.page().title.trim().is_empty()).then(|| {
                div()
                    .flex_none()
                    .max_w(px(HEADER_URL_MAX * k))
                    .overflow_hidden()
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child(
                        ChromeText::new(v.short_url().to_owned(), px(theme.typography.small()), k)
                            .zooming(chrome.zooming),
                    )
            })
        });
        let proxy = self.file_proxy(item, tile, chrome, cx);
        let renaming = self.rename.as_ref().filter(|r| r.tile == tile).map(|r| r.input.clone());
        let heading = SharedString::from(if kind == title {
            title.clone()
        } else {
            format!("{kind} {title}")
        });
        div()
            .id("title")
            .debug_selector(move || format!("title-{}", id.as_uuid()))
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
            .bg(hsla(theme.surfaces.panel))
            .border_b_1()
            .border_color(hsla(theme.surfaces.border))
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(if focused { theme.surfaces.text } else { theme.surfaces.text_muted }))
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
            .when_some(away, gpui::ParentElement::child)
            .when_some(proxy, gpui::ParentElement::child)
            .child(match renaming {
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
                    .flex_1()
                    .overflow_hidden()
                    .child(
                        ChromeText::new(title, px(theme.typography.small()), k)
                            .fill()
                            .zooming(chrome.zooming),
                    )
                    .into_any_element(),
            })
            .when_some(address, gpui::ParentElement::child)
            .when_some(unsaved, gpui::ParentElement::child)
            .when_some(tabs, gpui::ParentElement::child)
            .children(ports)
            .when_some(upload, gpui::ParentElement::child)
            .when_some(hooks, gpui::ParentElement::child)
            .when_some(finished, gpui::ParentElement::child)
            .when_some(badge, gpui::ParentElement::child)
            .child(actions)
            .when_some(progress, gpui::ParentElement::child)
            .into_any_element()
    }

    /// A file tile's proxy: a small page before its title, as a Mac document window has.
    /// Dragged, the file leaves the app as a promise the worker keeps; the rest of the header
    /// still moves the tile.
    #[cfg(target_os = "macos")]
    fn file_proxy(
        &self,
        item: &Item,
        tile: TileRef,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let ItemKind::File { path } = &item.kind else { return None };
        let theme = &self.theme;
        let k = chrome.k;
        let id = item.id;
        let path = path.clone();
        let worker = tile.worker;
        let page = div()
            .w(px(theme.spacing.sm * k))
            .h(px(theme.spacing.md * k))
            .border_1()
            .border_color(hsla(theme.surfaces.text_secondary))
            .rounded(px(theme.radii.xs * k * 0.5));
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
                .size(px(theme.spacing.lg * k))
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
                .child(page)
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
        let reconnecting = || -> SharedString {
            let worker = self.workers.get(&placed.tile.worker);
            let name = worker.map_or("worker", |w| w.name.as_str());
            match worker.map(|w| &w.status) {
                Some(WorkerStatus::Unreachable) => format!("{name} is unreachable").into(),
                Some(WorkerStatus::Gone) => format!("{name} is gone").into(),
                _ => format!("{name} is away · reconnecting…").into(),
            }
        };
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
                None if !worker_up => muted_line(reconnecting()),
                None if self.summary(*session).is_some() => muted_line("attaching…".into()),
                None => muted_line("session ended".into()),
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
                    None if !worker_up => picture_wait(reconnecting()),
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
                None if !worker_up => muted_line(reconnecting()),
                None => muted_line("reading…".into()),
            },
        }
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
