//! slopty: a page an agent shows inline, as its card in the transcript (Slopty's own; not in
//! zeron): Slopty's `show_page`, an MCP App's UI, an `.html` or `.svg` file the agent wrote.
//!
//! The web view is the app's (`slopty_platform::web::page::PageHost`), handed over by the page's
//! key through [`crate::live::page`]; this card frames it in zeron's language for foreign
//! pixels: a code or diagram card's frame (its corner, hairline and surface), a header bar with
//! the page's title and its source (an app's server, a file's path; never the tool that showed
//! it) and the actions on the right, over the body, opaque on the transcript's surface. At rest the page's last picture stands for it, so it
//! frosts, fades and clips like any row; it goes live while the person engages it (a pointer
//! over it, the keyboard in it) and rests again 2 s after they leave, or once it has scrolled
//! wholly out of the transcript's clear band. A live page shows only within that band, under
//! the titlebar's fade and over the composer's frost, where a native view would read as a seam:
//! past it the card draws the page's picture, which fades and frosts as the chat does.
//!
//! The header's actions show on hover (`docs/decisions/ui.md`, "A page's card carries its
//! own actions"): Copy image (the page's last picture, as a PNG), Save (its HTML in the chat's
//! folder), Copy source or Copy table for a chart fence or a table, Open in pane (the same web
//! view, moved, not reloaded: [`pane`]) and Expand. A PDF or a CSV file draws natively
//! ([`doc`]); a chart fence is a page of its own ([`fence`]).
//!
//! A page never answers for the person (`docs/decisions/ui.md`, "A page never answers for the
//! person"): what it asks shows on its card. A tool call it wants to make waits for the
//! person's Allow, network its app declares waits for theirs too, and each call made says so
//! under the page. A message it sends lands in the chat's draft and context it offers rides
//! there as a chip, both the view model's.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Bounds, Context, ObjectFit, Pixels, Render, SharedString, StyledImage as _,
    Subscription, Task, Window, canvas, div, img, prelude::*, px,
};
use slopty_view_doc::{PageCard, PageState, calls};

use crate::live::{self, PageAct, PagePresence};
use crate::motion::HoverFade as _;

pub mod doc;
pub mod fence;
pub mod pane;

pub use doc::{PageDoc, PageTable, PdfDoc};
pub use fence::{FenceChart, FenceCards};
pub use pane::{PagePaneAsk, PagePaneView};
use crate::state::EngineHandle;
use crate::theme::Theme;
use crate::transcript::{Row, RowKind};

/// The height a page's card keeps before the page says its own.
pub const PAGE_RESERVED_HEIGHT: f32 = 240.0;
/// The shortest a page shows, as the host clamps it.
const PAGE_MIN_HEIGHT: f32 = 80.0;
/// The tallest a page shows inline, and the share of the window it may take.
const PAGE_MAX_HEIGHT: f32 = 720.0;
const PAGE_VIEWPORT_SHARE: f32 = 0.8;
/// How long a page stays live after the pointer and the keyboard leave it.
const REST_AFTER: Duration = Duration::from_secs(2);
/// slopty: the header bar over a page, the height of a code card's (`markdown::render`).
const HEADER_HEIGHT: f32 = 28.0;
/// slopty: the card's corner, the transcript's one card corner (a code or diagram card's).
const CARD_RADIUS: f32 = crate::transcript::TRANSCRIPT_CARD_RADIUS;
/// How long an action shows it is done.
const DONE_FOR: Duration = Duration::from_millis(1500);
/// The actions that show they are done, by name.
const COPY_IMAGE: &str = "copy-image";
const COPY_TABLE: &str = "copy-table";
const COPY_SOURCE: &str = "copy-source";
const SAVE: &str = "save";

type ActionFn = Rc<dyn Fn(&mut Window, &mut gpui::App)>;

// slopty: a card's header action is the surfaces' own icon button (`live::chrome::icon_button`:
// its size, its faded hover, its focus ring and tab stop); a quiet one fades in with the card's
// hover, or shows while it has the keyboard.
/// One of a card's header actions: a button named `label`, faded out until the card under
/// `reveal` is hovered when `quiet`, a check for a moment once `done`.
#[expect(clippy::too_many_arguments, reason = "an action is drawn from the card's parts")]
pub(crate) fn header_action(
    id: SharedString,
    label: &'static str,
    icon: &'static str,
    quiet: bool,
    done: bool,
    reveal: f32,
    theme: &Theme,
    handler: ActionFn,
) -> AnyElement {
    live::chrome::icon_button(id, if done { crate::icons::CHECK } else { icon }, label, theme)
        .when(quiet && !done, |el| el.opacity(reveal).focus_visible(|style| style.opacity(1.0)))
        .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            handler(window, cx);
        })
        .into_any_element()
}

/// What a page's card or pane says when its page could not be shown: why, and Try again when
/// a try could go otherwise (`retry`), which opens the page afresh.
pub(crate) fn failed_body(id: &str, why: SharedString, retry: bool, page: &str, theme: &Theme) -> AnyElement {
    let page = page.to_owned();
    let again = retry.then(|| {
        live::chrome::button(format!("{id}-try-again"), live::screen::TRY_AGAIN, live::chrome::Tone::Ghost, theme)
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                live::page_act(&page, PageAct::Reload, cx);
            })
            .into_any_element()
    });
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(live::chrome::empty_state(
            SharedString::from(format!("{id}-failed")),
            None,
            "Couldn't show the page",
            Some(why),
            again,
            theme,
        ))
        .into_any_element()
}

/// The row of a page part.
pub fn page_row(
    entry: &slopty_view_doc::SessionMessageEntry,
    part_id: &str,
    card: &PageCard,
) -> Row {
    let mut acc = entry.device_id.as_bytes().to_vec();
    crate::transcript::work::hash_into(&mut acc, card);
    Row {
        id: format!("{}#{part_id}", entry.id).into(),
        version: fnv(&acc),
        turn_start: false,
        kind: RowKind::Page {
            owner: entry.device_id.clone(),
            card: Arc::new(card.clone()),
        },
        entry_id: entry.id.clone().into(),
        timestamp: None,
        copy_text: None,
        compact_fold: None,
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The theme a page draws in, as MCP Apps' CSS variables: the transcript's surface, text and
/// hairlines, its sans and mono faces (in single quotes: the host drops a value with a double
/// quote), and the six chart colours charts draw their series in.
pub fn theme_variables(theme: &Theme) -> Vec<(String, String)> {
    let css = |color: gpui::Hsla| {
        let rgba = gpui::Rgba::from(color);
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", byte(rgba.r), byte(rgba.g), byte(rgba.b))
    };
    let mut variables = vec![
        ("--color-background-primary".to_owned(), css(theme.bg)),
        ("--color-background-secondary".to_owned(), css(theme.surface)),
        ("--color-text-primary".to_owned(), css(theme.text)),
        ("--color-text-secondary".to_owned(), css(theme.text_muted)),
        ("--color-border-primary".to_owned(), css(theme.border)),
        ("--color-border-secondary".to_owned(), css(theme.border.opacity(0.5))),
        ("--font-sans".to_owned(), format!("'{}', system-ui, sans-serif", theme.font_sans)),
        ("--font-mono".to_owned(), format!("'{}', ui-monospace, monospace", theme.font_mono)),
    ];
    for (n, color) in crate::visuals::chart_series(theme).into_iter().enumerate() {
        variables.push((format!("--color-chart-{}", n + 1), css(color)));
    }
    variables
}

/// What the person reads of how a page stands: its card's assistive value.
///
/// slopty: live only once its document has `loaded`: a web view made live comes up empty and
/// takes no touch until its document is in it.
pub fn presence_words(
    presence: Option<PagePresence>,
    snapshot: bool,
    failed: bool,
    loaded: bool,
) -> &'static str {
    match presence {
        _ if failed => "error",
        Some(PagePresence::Live) if loaded => "live",
        Some(PagePresence::Live) if snapshot => "snapshot",
        Some(PagePresence::Warm | PagePresence::Cold) if snapshot => "snapshot",
        _ => "loading",
    }
}

/// slopty: what a card says while its document waits for `machine`, out of reach.
pub fn waiting_words(machine: &str) -> SharedString {
    format!("Waiting for {machine}…").into()
}

/// slopty: a page card's header words past its title: where the page comes from, an MCP App's
/// server, a file's path or a chart's language. The tool that showed it is never named: HTML the
/// agent wrote (`show_page`) has no source of its own.
pub fn source_words(card: &PageCard) -> Option<String> {
    let words = match card.kind.as_str() {
        PageCard::APP => card.server.clone()?,
        PageCard::HTML => return None,
        _ => card.source.clone(),
    };
    (!words.is_empty()).then_some(words)
}

/// One page card in a transcript.
pub struct PageCardView {
    card: Arc<PageCard>,
    /// The chat the page is in, whose Files pane opens its file.
    chat: Option<String>,
    engine: Option<EngineHandle>,
    /// What the view model says the page asks, from `slopty.WatchPage`.
    state: Option<PageState>,
    /// Where the card's body was drawn last, and the band of the transcript a live page may
    /// sit in without a seam.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    clear: Option<(f32, f32)>,
    width: f32,
    /// The card drew a live page where it lies, so a scroll that moves it asks it to look again.
    placed_live: bool,
    /// The pointer or the keyboard is in the card.
    engaged: bool,
    /// The person made the page tall (or it asked to be): the window's share, not the cap.
    expanded: bool,
    /// Answers sent, until the view model drops what they answered.
    answered: std::collections::HashSet<String>,
    /// The action that just did what it says, showing a check for a moment.
    done: Option<(&'static str, Task<()>)>,
    rest: Option<Task<()>>,
    _watch: Option<Task<()>>,
    _host: Option<Subscription>,
}

impl PageCardView {
    pub fn new(
        card: Arc<PageCard>,
        chat: Option<String>,
        engine: Option<EngineHandle>,
        cx: &mut Context<Self>,
    ) -> Self {
        let watch = engine.clone().map(|engine| {
            let page = card.page.clone();
            cx.spawn(async move |this, cx| {
                let params = serde_json::json!({ "page": page });
                let Ok(mut rx) = engine.client().subscribe_checked(calls::WATCH_PAGE, params).await
                else {
                    tracing::warn!(%page, "page watch failed");
                    return;
                };
                while let Some(value) = rx.recv().await {
                    let Ok(state) = serde_json::from_value::<PageState>(value) else {
                        continue;
                    };
                    if this
                        .update(cx, |this, cx| {
                            this.answered.retain(|request| {
                                state.confirm.as_ref().is_some_and(|ask| &ask.request == request)
                            });
                            this.state = Some(state);
                            cx.notify();
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
        });
        let weak = cx.weak_entity();
        let host = live::watch_page(
            &card.page,
            Box::new(move |cx| {
                weak.update(cx, |_, cx| cx.notify()).ok();
            }),
            cx,
        );
        cx.on_release(|this: &mut Self, cx| live::page_act(&this.card.page, PageAct::Unmount, cx))
            .detach();
        Self {
            card,
            chat,
            engine,
            state: None,
            bounds: Rc::default(),
            clear: None,
            width: 0.0,
            placed_live: false,
            engaged: false,
            expanded: false,
            answered: Default::default(),
            done: None,
            rest: None,
            _watch: watch,
            _host: host,
        }
    }

    /// The card's page now (a streamed call goes on writing it).
    pub fn set_card(&mut self, card: Arc<PageCard>, cx: &mut Context<Self>) {
        if self.card != card {
            self.card = card;
            cx.notify();
        }
    }

    /// The band of the window a live page may sit in: below the titlebar's fade, above the
    /// composer's frost.
    pub fn set_clear_band(&mut self, top: f32, bottom: f32) {
        self.clear = Some((top, bottom));
    }

    /// Whether the card drew a live page where it lies: a scroll the window composites moves the
    /// card without drawing it again, so its transcript builds it again to see whether it went
    /// into a fade band.
    #[must_use]
    pub fn placed_live(&self) -> bool {
        self.placed_live
    }

    /// Whether some of the card's body lies in the band a live page may show in (or nothing says
    /// where either is yet).
    fn shows_in_band(&self) -> bool {
        match (self.bounds.get(), self.clear) {
            (Some(bounds), Some(band)) => meets_band(bounds, band),
            _ => true,
        }
    }

    fn engage(&mut self, cx: &mut Context<Self>) {
        // slopty: a card with none of its body in the clear band would rest the moment it went
        // live, and the picture its rest draws takes the hover again: it waits for the band.
        if !self.shows_in_band() {
            return;
        }
        self.rest = None;
        // slopty: a page rested while the card stayed engaged (a fade band, the device's cap)
        // wakes again at the next press or hover.
        if !self.engaged || !self.placed_live {
            self.engaged = true;
            live::page_act(&self.card.page, PageAct::Engage, cx);
        }
    }

    fn leave(&mut self, cx: &mut Context<Self>) {
        self.engaged = false;
        let page = self.card.page.clone();
        self.rest = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REST_AFTER).await;
            this.update(cx, |this, cx| {
                this.rest = None;
                if !this.engaged {
                    live::page_act(&page, PageAct::Rest, cx);
                }
            })
            .ok();
        }));
    }

    fn call(&self, method: &'static str, params: serde_json::Value, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else { return };
        cx.spawn(async move |_, _| {
            if let Err(err) = engine.client().call(method, params).await {
                tracing::warn!(error = %err, method, "a page's answer failed");
            }
        })
        .detach();
    }

    fn answer(&mut self, request: String, allow: bool, cx: &mut Context<Self>) {
        if !self.answered.insert(request.clone()) {
            return;
        }
        let params = serde_json::json!({ "page": self.card.page, "request": request, "allow": allow });
        self.call(calls::ANSWER_PAGE, params, cx);
        cx.notify();
    }

    fn grant_network(&mut self, cx: &mut Context<Self>) {
        if self.answered.insert("network".to_owned()) {
            let params = serde_json::json!({ "page": self.card.page });
            self.call(calls::GRANT_PAGE_NETWORK, params, cx);
            cx.notify();
        }
    }

    /// Call `method`, and show `action` done once it answers.
    fn call_done(
        &mut self,
        action: &'static str,
        method: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.engine.clone() else { return };
        let params = serde_json::json!({ "page": self.card.page });
        cx.spawn(async move |this, cx| match engine.client().call(method, params).await {
            Ok(_) => {
                let _gone = this.update(cx, |this, cx| this.show_done(action, cx));
            }
            Err(err) => tracing::warn!(error = %err, method, "a page's action failed"),
        })
        .detach();
    }

    /// Show `action` done for a moment.
    fn show_done(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let reset = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DONE_FOR).await;
            let _gone = this.update(cx, |this, cx| {
                this.done = None;
                cx.notify();
            });
        });
        self.done = Some((action, reset));
        cx.notify();
    }

    /// Whether the page shows in the right pane now.
    fn in_pane(&self) -> bool {
        self.state.as_ref().is_some_and(|state| state.display_mode == "pane")
    }

    /// Show the page in the chat's right pane, or bring it back to the card.
    fn move_to_pane(&mut self, show: bool, cx: &mut Context<Self>) {
        let Some(chat) = self.chat.clone() else { return };
        self.rest = None;
        pane::move_page(self.engine.clone(), &self.card.page, show, cx);
        pane::ask_pane(&chat, &self.card.page, &self.card.title, show, cx);
    }

    /// The key the card's hover fades under, which reveals its quiet actions.
    fn hover_key(&self) -> String {
        format!("{}-card-hover", self.card.page)
    }

    /// The header's actions for what the page is.
    fn actions(&self, doc: Option<&PageDoc>, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let page = self.card.page.clone();
        let id = |name: &str| SharedString::from(format!("{page}-{name}"));
        let done = |action: &str| self.done.as_ref().is_some_and(|(done, _)| *done == action);
        let weak = cx.weak_entity();
        let on = |f: fn(&mut Self, &mut Window, &mut Context<Self>)| -> ActionFn {
            let weak = weak.clone();
            Rc::new(move |window, cx| {
                let _gone = weak.update(cx, |this, cx| f(this, window, cx));
            })
        };
        let mut actions = Vec::new();
        if self.in_pane() {
            return actions;
        }
        let reveal = crate::motion::hover_t(&self.hover_key());
        let fence = fence::is_fence(&self.card.page);
        match doc {
            Some(PageDoc::Table(_)) => actions.push(header_action(
                id("copy-table"),
                "Copy table",
                crate::icons::COPY,
                true,
                done(COPY_TABLE),
                reveal,
                theme,
                on(|this, window, cx| {
                    let Some(PageDoc::Table(table)) =
                        live::page(&this.card.page, window, cx).and_then(|page| page.doc)
                    else {
                        return;
                    };
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(table.to_tsv()));
                    this.show_done(COPY_TABLE, cx);
                }),
            )),
            _ => actions.push(header_action(
                id("copy-image"),
                "Copy image",
                crate::icons::GALLERY,
                true,
                done(COPY_IMAGE),
                reveal,
                theme,
                on(|this, _, cx| this.call_done(COPY_IMAGE, calls::COPY_PAGE_IMAGE, cx)),
            )),
        }
        if fence {
            actions.push(header_action(
                id("copy-source"),
                "Copy source",
                crate::icons::COPY,
                true,
                done(COPY_SOURCE),
                reveal,
                theme,
                on(|this, _, cx| {
                    if let Some(fence) = fence::fence(&this.card.page, cx) {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(fence.source));
                        this.show_done(COPY_SOURCE, cx);
                    }
                }),
            ));
        } else if doc.is_none() {
            actions.push(header_action(
                id("save"),
                "Save",
                crate::icons::FLOPPY_DISK,
                true,
                done(SAVE),
                reveal,
                theme,
                on(|this, _, cx| this.call_done(SAVE, calls::SAVE_PAGE, cx)),
            ));
        }
        if self.chat.is_some() {
            actions.push(header_action(
                id("open-in-pane"),
                "Open in pane",
                crate::icons::SPLIT_COLUMNS,
                true,
                false,
                reveal,
                theme,
                on(|this, _, cx| this.move_to_pane(true, cx)),
            ));
        }
        if doc.is_none() {
            actions.push(header_action(
                id("expand"),
                if self.expanded { "Collapse page" } else { "Expand page" },
                if self.expanded {
                    crate::icons::COLLAPSE_ARROWS
                } else {
                    crate::icons::EXPAND_ARROWS
                },
                true,
                false,
                reveal,
                theme,
                on(|this, _, cx| {
                    this.expanded = !this.expanded;
                    cx.notify();
                }),
            ));
        }
        actions
    }

    /// The card's quiet line while its page shows in the pane.
    fn pane_line(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(SharedString::from(format!("{}-in-pane", self.card.page)))
            .role(gpui::Role::Group)
            .aria_label("Shown in the pane")
            .w_full()
            .h(px(36.0))
            .px(px(Theme::SPACE_MD))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text_muted)
            .child(crate::icons::icon(crate::icons::SPLIT_COLUMNS).size(px(13.0)).text_color(theme.text_faint))
            .child(div().flex_1().min_w_0().truncate().child(SharedString::from("Shown in the pane")))
            .child(
                div()
                    .id(SharedString::from(format!("{}-bring-back", self.card.page)))
                    .role(gpui::Role::Button)
                    .aria_label("Bring back")
                    .h(px(22.0))
                    .px(px(8.0))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_color(theme.text)
                    // slopty: the faded hover.
                    .bg(crate::motion::hover_blend(
                        &format!("{}-bring-back", self.card.page),
                        crate::theme::ink(0.06),
                        crate::theme::ink(0.1),
                    ))
                    .hover_fade(format!("{}-bring-back", self.card.page))
                    .tab_index(0) // slopty: a tab stop.
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    .on_click(cx.listener(|this, _, _, cx| this.move_to_pane(false, cx)))
                    .child(SharedString::from("Bring back")),
            )
            .into_any_element()
    }

    /// The header's source: a file opens in the chat's Files pane.
    fn source(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let text = div()
            .min_w_0()
            .truncate()
            .child(SharedString::from(source_words(&self.card).unwrap_or_default()));
        match (&self.card.path, &self.chat) {
            (Some(path), Some(chat)) => {
                let (path, chat) = (path.clone(), chat.clone());
                let fade = format!("{}-source", self.card.page);
                text.id("page-source")
                    .role(gpui::Role::Link)
                    .aria_label(SharedString::from(format!("Open {path}")))
                    .cursor_pointer()
                    // slopty: a faded hover, and a tab stop.
                    .text_color(crate::motion::hover_blend(&fade, theme.text_muted, theme.text))
                    .hover_fade(fade)
                    .tab_index(0)
                    .rounded(px(4.0))
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        live::open_file(&chat, &path, cx);
                    }))
                    .into_any_element()
            }
            _ => text.into_any_element(),
        }
    }
}

impl Render for PageCardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let hosted = live::page(&self.card.page, window, cx);
        let state = self.state.clone();
        let failed = self.card.failed
            || hosted.as_ref().is_some_and(|page| page.failed.is_some())
            || state.as_ref().is_some_and(|state| state.failed.is_some());
        let presence = hosted.as_ref().map(|page| page.presence);
        let in_pane = self.in_pane();
        let doc = hosted.as_ref().and_then(|page| page.doc.clone());

        // A live page that scrolled wholly out of the clear band rests at once; one in the pane is
        // the pane's.
        // slopty: asked once the frame is drawn. The host tells the card what changed; asked
        // while the window draws, the card it tells would wait for whatever draws next.
        let key = self.card.page.clone();
        let ask = |act: PageAct, cx: &mut Context<Self>| {
            let page = key.clone();
            cx.defer(move |cx| live::page_act(&page, act, cx));
        };
        if presence == Some(PagePresence::Live)
            && !in_pane
            && !self.shows_in_band()
        {
            ask(PageAct::Rest, cx);
        }
        if let Some(bounds) = self.bounds.get() {
            let width = f32::from(bounds.size.width);
            if (width - self.width).abs() > 0.5 {
                self.width = width;
                if !in_pane && doc.is_none() {
                    ask(PageAct::Width(width), cx);
                }
            }
        }

        let viewport = f32::from(window.viewport_size().height);
        let cap = (viewport * PAGE_VIEWPORT_SHARE).clamp(PAGE_MIN_HEIGHT, PAGE_MAX_HEIGHT);
        let asked_full = state
            .as_ref()
            .is_some_and(|state| state.display_mode == "fullscreen");
        let height = if self.expanded || asked_full {
            viewport * PAGE_VIEWPORT_SHARE
        } else {
            hosted
                .as_ref()
                .and_then(|page| page.height)
                // The view model's height is whole points.
                .or_else(|| state.as_ref().and_then(|state| state.height).map(|h| u16::try_from(h).map_or(f32::MAX, f32::from)))
                .unwrap_or(PAGE_RESERVED_HEIGHT)
                .clamp(PAGE_MIN_HEIGHT, cap)
        };
        let snapshot = hosted.as_ref().and_then(|page| page.snapshot.clone());
        // slopty: a document that waits for its machine out of reach says so.
        let waiting = hosted.as_ref().and_then(|page| page.waiting.clone()).filter(|_| {
            !failed && doc.is_none() && snapshot.is_none() && presence != Some(PagePresence::Live)
        });
        // slopty: a PDF or a table draws here natively: once read, it is live.
        let value = if doc.is_some() && !failed {
            "live"
        } else if waiting.is_some() {
            "waiting"
        } else {
            presence_words(
                presence,
                snapshot.is_some(),
                failed,
                state.as_ref().is_some_and(|state| state.loaded),
            )
        };
        let why = hosted
            .as_ref()
            .and_then(|page| page.failed.clone())
            .or_else(|| state.as_ref().and_then(|state| state.failed.clone()).map(Into::into))
            .unwrap_or_else(|| "The page could not be shown".into());

        let body_content: AnyElement = match (&hosted, presence) {
            // slopty: why, and Try again unless the call that showed it failed.
            _ if failed => failed_body(&self.card.page, why.clone(), !self.card.failed, &self.card.page, &theme),
            (
                Some(live::HostedPage {
                    native: Some(native),
                    ..
                }),
                Some(PagePresence::Live),
            ) => {
                // slopty: the wheel passes to the chat under the card. On the Mac the page
                // takes it first, and hands the window what it cannot scroll
                // (`slopty_platform::web::page`'s wheel); GPUI then scrolls what is under it.
                // Over the page the native's hitbox is topmost, so the body hears no hover: the
                // native says it, or the card would rest under a pointer still over it.
                let native = gpui::composition::native_view(native)
                    .id(SharedString::from(format!("{}-native", self.card.page)))
                    .block_mouse_except_scroll()
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.engage(cx);
                        }
                    }))
                    .size_full();
                // slopty: the page shows within the clear band alone; past it, under the
                // titlebar's fade and the composer's frost, its picture stands in, so a page
                // taller than the band (a phone's, an expanded one) is live where it shows.
                div()
                    .size_full()
                    .relative()
                    .children(snapshot.clone().map(|picture| {
                        img(gpui::ImageSource::Render(picture))
                            .absolute()
                            .inset_0()
                            .size_full()
                            .object_fit(ObjectFit::Fill)
                    }))
                    .child(BandClip { band: self.clear, child: native.into_any_element() })
                    .into_any_element()
            }
            _ => match snapshot {
                Some(picture) => img(gpui::ImageSource::Render(picture))
                    .size_full()
                    .object_fit(ObjectFit::Fill)
                    .into_any_element(),
                // slopty: the surfaces' loading anatomy, the spinner over its caption.
                // slopty: the machine it waits for is read out, as a status.
                None => match waiting.as_ref().filter(|_| !self.card.streaming) {
                    Some(machine) => {
                        let words = waiting_words(machine);
                        live::chrome::loading(words.clone(), &theme, cx.entity_id(), cx)
                            .id(SharedString::from(format!("{}-waiting", self.card.page)))
                            .role(gpui::Role::Status)
                            .aria_label(words)
                            .into_any_element()
                    }
                    None => live::chrome::loading(
                        if self.card.streaming { "Writing the page…" } else { "Loading the page…" },
                        &theme,
                        cx.entity_id(),
                        cx,
                    )
                    .into_any_element(),
                },
            },
        };
        let probe = self.bounds.clone();
        // slopty: the card is built again when where it lies asks something of it (a live page
        // gone into a fade band rests, a new width is told to the page), so the frame shown is
        // the one a frame built from scratch, which reads the same bounds, draws.
        let live_now = presence == Some(PagePresence::Live) && !in_pane;
        self.placed_live = live_now;
        let (clear, width, card) = (self.clear, self.width, cx.weak_entity());
        let measure = move |probe: Rc<Cell<Option<Bounds<Pixels>>>>| {
            let card = card.clone();
            canvas(
                move |bounds, _, cx| {
                    if probe.get() != Some(bounds) {
                        probe.set(Some(bounds));
                        let out = clear.is_some_and(|band| !meets_band(bounds, band));
                        let resized = (f32::from(bounds.size.width) - width).abs() > 0.5;
                        if (live_now && out) || resized {
                            let card = card.clone();
                            cx.defer(move |cx| {
                                card.update(cx, |_, cx| cx.notify()).ok();
                            });
                        }
                    }
                },
                |_, _, _, _| (),
            )
            .absolute()
            .inset_0()
        };
        let doc_body = match &doc {
            _ if in_pane || failed => None,
            Some(PageDoc::Pdf(pdf)) => {
                let width = if self.width > 0.0 { self.width } else { PAGE_RESERVED_HEIGHT * 2.5 };
                let (shown, _) = doc::fit(width, cap, pdf.aspect_of(0));
                if pdf.pages > 0 && pdf.picture(0, shown).is_none() {
                    live::page_act(&self.card.page, PageAct::Draw { index: 0, width: shown }, cx);
                }
                Some(
                    div()
                        .id("page-pdf")
                        .cursor_pointer()
                        .when(self.chat.is_some(), |el| {
                            el.on_click(cx.listener(|this, _, _, cx| this.move_to_pane(true, cx)))
                        })
                        .child(doc::pdf_first_page(
                            SharedString::from(format!("{}-pdf-page-0", self.card.page)),
                            pdf,
                            width,
                            cap,
                            &theme,
                        ))
                        .into_any_element(),
                )
            }
            Some(PageDoc::Table(table)) => Some(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .child(doc::table(
                        SharedString::from(self.card.page.clone()),
                        SharedString::from(self.card.title.clone()),
                        table,
                        PageTable::INLINE_ROWS,
                        &theme,
                    ))
                    .children(doc::more_rows_words(table, PageTable::INLINE_ROWS).map(|words| {
                        div()
                            .pt(px(Theme::SPACE_XS))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_faint)
                            .child(SharedString::from(words))
                    }))
                    .into_any_element(),
            ),
            None => None,
        };
        let body = div()
            .id("page-body")
            .relative()
            .w_full()
            .h(px(height))
            // slopty: the frame's lower corners, inside its hairline.
            .rounded_b(px(CARD_RADIUS - 1.0))
            .overflow_hidden()
            .bg(theme.bg)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if *hovered {
                    this.engage(cx);
                } else {
                    this.leave(cx);
                }
            }))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.engage(cx)))
            .child(body_content)
            .child(measure(probe.clone()));
        let body: AnyElement = if in_pane {
            self.pane_line(&theme, cx)
        } else if let Some(doc_body) = doc_body {
            div().relative().w_full().child(doc_body).child(measure(probe)).into_any_element()
        } else {
            body.into_any_element()
        };

        let actions = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0))
            .children(self.actions(doc.as_ref(), &theme, cx));
        // slopty: a code card's header bar: the page's title and where it comes from, its actions
        // on the right.
        let header = div()
            .h(px(HEADER_HEIGHT))
            .w_full()
            .min_w_0()
            .flex_none()
            .pl(px(crate::markdown::render::CODE_PADDING_X))
            .pr(px(5.0))
            .border_b_1()
            .border_color(theme.border)
            .bg(crate::theme::ink(0.02))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .child(
                div()
                    .flex_none()
                    .max_w(px(280.0))
                    .truncate()
                    .text_color(theme.text.opacity(0.85))
                    .child(SharedString::from(self.card.title.clone())),
            )
            .when(source_words(&self.card).is_some(), |header| {
                header
                    .child(SharedString::from("·"))
                    .child(self.source(&theme, cx))
            })
            .child(div().flex_1())
            .child(actions);

        // What the page asks of the person, each on its own line under it.
        let mut asks = div().flex().flex_col().gap(px(Theme::SPACE_XS)).pt(px(Theme::SPACE_XS));
        if let Some(network) = state.as_ref().and_then(|state| state.network.clone())
            && !self.answered.contains("network")
        {
            let hosts: Vec<String> = network
                .connect
                .iter()
                .chain(network.resources.iter())
                .cloned()
                .collect();
            asks = asks.child(ask_line(
                "page-network",
                format!("Wants network: {}", hosts.join(", ")),
                None,
                theme.text_muted,
                &theme,
                vec![(
                    "Allow network",
                    "Allow",
                    listener(cx.listener(|this, _, _, cx| this.grant_network(cx))),
                )],
            ));
        }
        if let Some(confirm) = state.as_ref().and_then(|state| state.confirm.clone())
            && !self.answered.contains(&confirm.request)
        {
            let (allow, deny) = (confirm.request.clone(), confirm.request.clone());
            asks = asks.child(ask_line(
                "page-confirm",
                format!("The page wants to call {}", confirm.tool),
                Some(confirm.arguments.clone()),
                theme.warning,
                &theme,
                vec![
                    (
                        "Allow the call",
                        "Allow",
                        listener(cx.listener(move |this, _, _, cx| {
                            this.answer(allow.clone(), true, cx);
                        })),
                    ),
                    (
                        "Deny the call",
                        "Deny",
                        listener(cx.listener(move |this, _, _, cx| {
                            this.answer(deny.clone(), false, cx);
                        })),
                    ),
                ],
            ));
        }
        for tool in state.iter().flat_map(|state| state.called.iter()) {
            asks = asks.child(
                div()
                    .text_size(crate::typography::ui_rems(11.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!("Page called {tool}"))),
            );
        }

        div()
            .id(SharedString::from(format!("{}-card", self.card.page)))
            .group("page-card")
            .hover_fade(self.hover_key())
            .role(gpui::Role::Group)
            .aria_label(SharedString::from(format!("Page: {}", self.card.title)))
            .aria_value(value)
            .w_full()
            .py(px(Theme::SPACE_XS))
            .flex()
            .flex_col()
            .child(
                // slopty: one frame for every card that shows something made (a page, an app's
                // view, a chart), a code or diagram card's: its corner, hairline and surface.
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .rounded(px(CARD_RADIUS))
                    .bg(crate::theme::ink(0.035))
                    .border_1()
                    .border_color(theme.border)
                    .overflow_hidden()
                    .child(header)
                    .child(body),
            )
            .child(asks)
    }
}

/// slopty: whether `bounds` (window points) meets the band `(top, bottom)` a live page shows in.
fn meets_band(bounds: Bounds<Pixels>, (top, bottom): (f32, f32)) -> bool {
    f32::from(bounds.top()) < bottom && f32::from(bounds.bottom()) > top
}

/// slopty: draws its child clipped to the band `(top, bottom)` of the window (the transcript's, as
/// the frame began), so a native placed in it shows, and takes the pointer, only there.
struct BandClip {
    band: Option<(f32, f32)>,
    child: AnyElement,
}

impl BandClip {
    fn mask(&self, window: &Window) -> Option<gpui::ContentMask<Pixels>> {
        let (top, bottom) = self.band?;
        let width = window.viewport_size().width;
        let size = gpui::size(width, px((bottom - top).max(0.0)));
        Some(gpui::ContentMask { bounds: Bounds::new(gpui::point(px(0.0), px(top)), size) })
    }
}

impl IntoElement for BandClip {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl gpui::Element for BandClip {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let mask = self.mask(window);
        window.with_content_mask(mask, |window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let mask = self.mask(window);
        window.with_content_mask(mask, |window| self.child.paint(window, cx));
    }
}

type Listener = Rc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App)>;

/// A click handler as [`ask_line`] takes it.
fn listener(on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static) -> Listener {
    Rc::new(on_click)
}

/// One thing a page asks: its words, what it would send, and the person's answers.
fn ask_line(
    id: &'static str,
    words: String,
    detail: Option<String>,
    tint: gpui::Hsla,
    theme: &Theme,
    actions: Vec<(&'static str, &'static str, Listener)>,
) -> AnyElement {
    let buttons = actions.into_iter().map(|(label, text, on_click)| {
        div()
            .id(SharedString::from(format!("{id}-{text}")))
            .role(gpui::Role::Button)
            .aria_label(label)
            .h(px(22.0))
            .px(px(8.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(theme.text)
            // slopty: the faded hover.
            .bg(crate::motion::hover_blend(
                &format!("{id}-{text}-hover"),
                crate::theme::ink(0.06),
                crate::theme::ink(0.1),
            ))
            .hover_fade(format!("{id}-{text}-hover"))
            .tab_index(0) // slopty: a tab stop.
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .on_click(move |event, window, cx| on_click(event, window, cx))
            .child(SharedString::from(text))
    });
    div()
        .id(SharedString::from(id))
        .role(gpui::Role::Group)
        .aria_label(SharedString::from(words.clone()))
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(crate::typography::ui_rems(12.0))
                        .text_color(tint)
                        .child(SharedString::from(words)),
                )
                .children(buttons),
        )
        .children(detail.filter(|d| !d.trim().is_empty()).map(|detail| {
            div()
                .max_h(px(68.0))
                .overflow_hidden()
                .rounded(px(6.0))
                .bg(crate::theme::ink(0.04))
                .px(px(8.0))
                .py(px(4.0))
                .font_family(theme.font_mono.clone())
                .text_size(px(11.0))
                .text_color(theme.text_muted)
                .child(SharedString::from(detail))
        }))
        .into_any_element()
}

#[cfg(test)]
mod card_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // slopty: Slopty's own conversation surfaces follow the font-size setting (`ui_rems`); only
    // code, diffs, tool output and terminal text keep px. Each file lists the px sizes it may
    // keep, so a new chrome size in px fails here.
    #[test]
    fn slopty_surfaces_size_their_words_by_the_setting() {
        let allowed: &[(&str, &str, usize)] = &[
            ("account_usage.rs", include_str!("account_usage.rs"), 0),
            ("running_pill.rs", include_str!("running_pill.rs"), 0),
            ("notice.rs", include_str!("notice.rs"), 0),
            ("toast.rs", include_str!("toast.rs"), 0),
            ("visuals.rs", include_str!("visuals.rs"), 0),
            ("attachments.rs", include_str!("attachments.rs"), 0),
            ("image_media.rs", include_str!("image_media.rs"), 0),
            ("page/pane.rs", include_str!("page/pane.rs"), 0),
            ("inspector.rs", include_str!("inspector.rs"), 0),
            ("shell/new_session.rs", include_str!("shell/new_session.rs"), 0),
            // A page's console lines.
            ("page.rs", include_str!("page.rs"), 1),
            // An error's raw words, a turn's diff counts, a terminal run's output.
            ("transcript/extras.rs", include_str!("transcript/extras.rs"), 3),
            // A call's output, twice, and its raw detail.
            ("transcript/work.rs", include_str!("transcript/work.rs"), 3),
            // A subagent's call label, in the tool rows' size.
            ("transcript/subagents.rs", include_str!("transcript/subagents.rs"), 1),
        ];
        let needle = concat!("text_size(", "px(");
        for (file, text, most) in allowed {
            let found = text.matches(needle).count();
            assert!(found <= *most, "{file}: {found} px text sizes, {most} allowed; use ui_rems");
        }
    }

    /// slopty: a wheel event the page hands on (one it cannot scroll) scrolls the chat under its
    /// card, the whole of it, as a browser's nested scroll goes on to what holds it.
    #[gpui::test]
    fn a_wheel_the_page_hands_on_scrolls_the_chat(cx: &mut gpui::TestAppContext) {
        struct Chat {
            card: gpui::AnyView,
            scroll: gpui::ScrollHandle,
        }
        impl Render for Chat {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div()
                    .id("chat")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().h(px(100.0)))
                    .child(self.card.clone())
                    .child(div().h(px(2000.0)))
            }
        }
        let held: Rc<std::cell::RefCell<Option<gpui::composition::NativeHost>>> = Rc::default();
        let dir = tempfile::tempdir().unwrap();
        let serve = Rc::clone(&held);
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(true);
            live::serve_pages(
                cx,
                move |_, window, cx| {
                    let mut native = serve.borrow_mut();
                    if native.is_none() {
                        *native = window.create_native_host(Default::default(), cx).ok();
                    }
                    Some(live::HostedPage {
                        presence: PagePresence::Live,
                        native: native.clone(),
                        snapshot: None,
                        height: None,
                        failed: None,
                        waiting: None,
                        doc: None,
                    })
                },
                |_, _, _| (),
                |_, _, _| None,
            );
        });
        let page = Arc::new(PageCard {
            id: "slopty-page:1/html:page".into(),
            page: "slopty-page:1/html".into(),
            title: "Plot".into(),
            kind: PageCard::HTML.into(),
            ..PageCard::default()
        });
        let card = cx.new(|cx| PageCardView::new(page, None, None, cx));
        let scroll = gpui::ScrollHandle::new();
        let chat = Chat { card: card.into(), scroll: scroll.clone() };
        let (_, cx) = cx.add_window_view(|_, _| chat);
        cx.simulate_resize(gpui::size(px(900.0), px(800.0)));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw_and_present(cx));
        // Under the header, in the page's body.
        let over = gpui::point(px(450.0), px(250.0));
        let placed = cx.update(|window, _| window.native_hit_map());
        assert!(
            placed.is_some_and(|map| map.native_at(over).is_some()),
            "the page is live where the wheel turns"
        );
        cx.simulate_mouse_move(over, None, gpui::Modifiers::default());
        cx.simulate_event(gpui::ScrollEvent {
            position: over,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-144.0))),
            ..Default::default()
        });
        cx.run_until_parked();
        assert_eq!(scroll.offset().y, px(-144.0), "the chat took the whole wheel");
    }

    /// A card's value says how its page stands: its picture at rest, live while engaged.
    #[test]
    fn a_page_says_how_it_stands() {
        assert_eq!(presence_words(Some(PagePresence::Warm), true, false, true), "snapshot");
        assert_eq!(presence_words(Some(PagePresence::Cold), true, false, true), "snapshot");
        assert_eq!(presence_words(Some(PagePresence::Live), true, false, true), "live");
        assert_eq!(presence_words(None, false, false, false), "loading");
        assert_eq!(presence_words(Some(PagePresence::Live), true, true, true), "error");
        // Made live, its document not in it yet: what shows until it is.
        assert_eq!(presence_words(Some(PagePresence::Live), false, false, false), "loading");
        assert_eq!(presence_words(Some(PagePresence::Live), true, false, false), "snapshot");
    }

    /// slopty: a card's header says its title and where the page comes from (an app's server, a
    /// file's path), never the tool that showed it.
    #[test]
    fn a_cards_header_names_its_source_and_no_tool() {
        let mut card = PageCard {
            title: "echo".into(),
            kind: PageCard::APP.into(),
            source: "apps · echo".into(),
            server: Some("apps".into()),
            tool: Some("echo".into()),
            ..PageCard::default()
        };
        assert_eq!(source_words(&card).as_deref(), Some("apps"), "an app's server");
        card.kind = PageCard::HTML.into();
        card.source = "show_page".into();
        assert_eq!(source_words(&card), None, "HTML the agent wrote: no tool named");
        card.kind = PageCard::FILE.into();
        card.source = "/work/chart.html".into();
        assert_eq!(source_words(&card).as_deref(), Some("/work/chart.html"));
        card.source = String::new();
        assert_eq!(source_words(&card), None);
    }
}
