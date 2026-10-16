//! slopty: a page an agent shows inline, as its card in the transcript (Slopty's own; not in
//! zeron): Slopty's `show_page`, an MCP App's UI, an `.html` or `.svg` file the agent wrote.
//!
//! The web view is the app's (`slopty_platform::web::page::PageHost`), handed over by the page's
//! key through [`crate::live::page`]; this card frames it in zeron's language for foreign
//! pixels: one muted header line (title · kind · source) over a hairline, the body rounded and
//! opaque on the transcript's surface. At rest the page's last picture stands for it, so it
//! frosts, fades and clips like any row; it goes live while the person engages it (a pointer
//! over it, the keyboard in it) and rests again 2 s after they leave, or as soon as it scrolls
//! into the composer's frost or the transcript's fade bands, where a native view would read as a
//! seam.
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
/// The header line over a page.
const HEADER_HEIGHT: f32 = 26.0;
/// How long an action shows it is done.
const DONE_FOR: Duration = Duration::from_millis(1500);
/// The actions that show they are done, by name.
const COPY_IMAGE: &str = "copy-image";
const COPY_TABLE: &str = "copy-table";
const COPY_SOURCE: &str = "copy-source";
const SAVE: &str = "save";

type ActionFn = Rc<dyn Fn(&mut Window, &mut gpui::App)>;

/// One of a card's header actions, in the code block's icon chrome: a button named `label`
/// with its tooltip, hidden until the card is hovered when `quiet`, a check for a moment once
/// `done`.
pub(crate) fn card_action(
    id: SharedString,
    label: &'static str,
    icon: &'static str,
    quiet: bool,
    done: bool,
    theme: &Theme,
    handler: ActionFn,
) -> AnyElement {
    let fade_key = id.to_string();
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .size(px(22.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(quiet && !done, |el| el.opacity(0.0).group_hover("page-card", |s| s.opacity(1.0)))
        .bg(crate::motion::hover_blend(&fade_key, gpui::transparent_black(), crate::theme::ink(0.08)))
        .on_hover(crate::motion::hover_listener(fade_key))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            handler(window, cx);
        })
        .tooltip(crate::settings::widgets::text_tooltip(label))
        .child(
            crate::icons::icon(if done { crate::icons::CHECK } else { icon })
                .size(px(13.0))
                .text_color(theme.text_muted),
        )
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
pub fn presence_words(presence: Option<PagePresence>, snapshot: bool, failed: bool) -> &'static str {
    match presence {
        _ if failed => "error",
        Some(PagePresence::Live) => "live",
        Some(PagePresence::Warm | PagePresence::Cold) if snapshot => "snapshot",
        _ => "loading",
    }
}

/// A page card's kind in words: "Page", "MCP App: {server}", "File".
pub fn kind_words(card: &PageCard) -> String {
    match card.kind.as_str() {
        PageCard::APP => match &card.server {
            Some(server) => format!("MCP App: {server}"),
            None => "MCP App".to_owned(),
        },
        PageCard::FILE => "File".to_owned(),
        fence::CHART_KIND => "Chart".to_owned(),
        _ => "Page".to_owned(),
    }
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

    fn engage(&mut self, cx: &mut Context<Self>) {
        self.rest = None;
        if !self.engaged {
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
        let fence = fence::is_fence(&self.card.page);
        match doc {
            Some(PageDoc::Table(_)) => actions.push(card_action(
                id("copy-table"),
                "Copy table",
                crate::icons::COPY,
                true,
                done(COPY_TABLE),
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
            _ => actions.push(card_action(
                id("copy-image"),
                "Copy image",
                crate::icons::GALLERY,
                true,
                done(COPY_IMAGE),
                theme,
                on(|this, _, cx| this.call_done(COPY_IMAGE, calls::COPY_PAGE_IMAGE, cx)),
            )),
        }
        if fence {
            actions.push(card_action(
                id("copy-source"),
                "Copy source",
                crate::icons::COPY,
                true,
                done(COPY_SOURCE),
                theme,
                on(|this, _, cx| {
                    if let Some(fence) = fence::fence(&this.card.page, cx) {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(fence.source));
                        this.show_done(COPY_SOURCE, cx);
                    }
                }),
            ));
        } else if doc.is_none() {
            actions.push(card_action(
                id("save"),
                "Save",
                crate::icons::FLOPPY_DISK,
                true,
                done(SAVE),
                theme,
                on(|this, _, cx| this.call_done(SAVE, calls::SAVE_PAGE, cx)),
            ));
        }
        if self.chat.is_some() {
            actions.push(card_action(
                id("open-in-pane"),
                "Open in pane",
                crate::icons::SPLIT_COLUMNS,
                true,
                false,
                theme,
                on(|this, _, cx| this.move_to_pane(true, cx)),
            ));
        }
        if doc.is_none() {
            actions.push(card_action(
                id("expand"),
                if self.expanded { "Collapse page" } else { "Expand page" },
                if self.expanded {
                    crate::icons::COLLAPSE_ARROWS
                } else {
                    crate::icons::EXPAND_ARROWS
                },
                true,
                false,
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
            .rounded(px(8.0))
            .border_1()
            .border_color(crate::theme::hairline(0.07))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .text_size(px(12.0))
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
                    .bg(crate::theme::ink(0.06))
                    .hover(|s| s.bg(crate::theme::ink(0.1)))
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
            .child(SharedString::from(self.card.source.clone()));
        match (&self.card.path, &self.chat) {
            (Some(path), Some(chat)) => {
                let (path, chat) = (path.clone(), chat.clone());
                text.id("page-source")
                    .role(gpui::Role::Link)
                    .aria_label(SharedString::from(format!("Open {path}")))
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme.text))
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

        // A live page that scrolled into a fade band or under the composer rests at once; one
        // in the pane is the pane's.
        if presence == Some(PagePresence::Live)
            && !in_pane
            && let (Some(bounds), Some((top, bottom))) = (self.bounds.get(), self.clear)
            && (f32::from(bounds.top()) < top || f32::from(bounds.bottom()) > bottom)
        {
            live::page_act(&self.card.page, PageAct::Rest, cx);
        }
        if let Some(bounds) = self.bounds.get() {
            let width = f32::from(bounds.size.width);
            if (width - self.width).abs() > 0.5 {
                self.width = width;
                if !in_pane && doc.is_none() {
                    live::page_act(&self.card.page, PageAct::Width(width), cx);
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
        let value = presence_words(presence, snapshot.is_some(), failed);
        let why = hosted
            .as_ref()
            .and_then(|page| page.failed.clone())
            .or_else(|| state.as_ref().and_then(|state| state.failed.clone()).map(Into::into))
            .unwrap_or_else(|| "The page could not be shown".into());

        let body_content: AnyElement = match (&hosted, presence) {
            _ if failed => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.0))
                .text_color(theme.text_muted)
                .child(why.clone())
                .into_any_element(),
            (
                Some(live::HostedPage {
                    native: Some(native),
                    ..
                }),
                Some(PagePresence::Live),
            ) => {
                gpui::composition::native_view(native)
                    .id(SharedString::from(format!("{}-native", self.card.page)))
                    .size_full()
                    .into_any_element()
            }
            _ => match snapshot {
                Some(picture) => img(gpui::ImageSource::Render(picture))
                    .size_full()
                    .object_fit(ObjectFit::Fill)
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(if self.card.streaming {
                        "Writing the page…"
                    } else {
                        "Loading the page…"
                    }))
                    .into_any_element(),
            },
        };
        let probe = self.bounds.clone();
        let measure = move |probe: Rc<Cell<Option<Bounds<Pixels>>>>| {
            canvas(
                move |bounds, _, _| {
                    if probe.get() != Some(bounds) {
                        probe.set(Some(bounds));
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
                            .text_size(px(12.0))
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
            .rounded(px(8.0))
            .overflow_hidden()
            .bg(theme.bg)
            .border_1()
            .border_color(crate::theme::hairline(0.07))
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
        let header = div()
            .h(px(HEADER_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .child(
                div()
                    .flex_none()
                    .max_w(px(280.0))
                    .truncate()
                    .text_color(theme.text.opacity(0.85))
                    .child(SharedString::from(self.card.title.clone())),
            )
            .child(SharedString::from("·"))
            .child(div().flex_none().child(SharedString::from(kind_words(&self.card))))
            .when(!self.card.source.is_empty(), |header| {
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
                    .text_size(px(11.5))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!("Page called {tool}"))),
            );
        }

        div()
            .id(SharedString::from(format!("{}-card", self.card.page)))
            .group("page-card")
            .role(gpui::Role::Group)
            .aria_label(SharedString::from(format!("Page: {}", self.card.title)))
            .aria_value(value)
            .w_full()
            .py(px(Theme::SPACE_XS))
            .flex()
            .flex_col()
            .child(header)
            .child(div().h(px(1.0)).w_full().mb(px(6.0)).bg(theme.border))
            .child(body)
            .child(asks)
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
            .text_size(px(12.0))
            .text_color(theme.text)
            .bg(crate::theme::ink(0.06))
            .hover(|s| s.bg(crate::theme::ink(0.1)))
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
                        .text_size(px(12.0))
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

    /// A card's value says how its page stands: its picture at rest, live while engaged.
    #[test]
    fn a_page_says_how_it_stands() {
        assert_eq!(presence_words(Some(PagePresence::Warm), true, false), "snapshot");
        assert_eq!(presence_words(Some(PagePresence::Cold), true, false), "snapshot");
        assert_eq!(presence_words(Some(PagePresence::Live), true, false), "live");
        assert_eq!(presence_words(None, false, false), "loading");
        assert_eq!(presence_words(Some(PagePresence::Live), true, true), "error");
    }

    /// The header names what the page is.
    #[test]
    fn a_page_names_its_kind() {
        let mut card = PageCard {
            kind: PageCard::APP.into(),
            server: Some("charts".into()),
            ..PageCard::default()
        };
        assert_eq!(kind_words(&card), "MCP App: charts");
        card.kind = PageCard::FILE.into();
        assert_eq!(kind_words(&card), "File");
        card.kind = PageCard::HTML.into();
        assert_eq!(kind_words(&card), "Page");
    }
}
