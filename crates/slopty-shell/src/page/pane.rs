//! slopty: a page shown in the right pane (Slopty's own; not in zeron).
//!
//! A card's "Open in pane" moves its page into a tab of the chat's right pane: the same web
//! view, laid out to fill the pane, its document and state kept (`slopty.MovePage`); the card
//! keeps its place in the transcript as one quiet line. A PDF shows every page there, each drawn
//! as it scrolls into view, and a table every row. Closing the tab, or the card's "Bring back",
//! puts the page back in its card.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, Global, ObjectFit, Pixels, Render, SharedString,
    StyledImage as _, Subscription, UniformListScrollHandle, Window, canvas, div, img,
    prelude::*, px, uniform_list,
};
use slopty_view_doc::calls;

use super::doc::{self, PageDoc, TABLE_ROW_HEIGHT};
use crate::live::{self, PageAct, PagePresence};
use crate::state::EngineHandle;
use crate::theme::Theme;

/// A card asks the shell to show its page in the chat's right pane, or to take it out again.
/// The shell observes this global; `seq` grows with every ask.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PagePaneAsk {
    /// The chat whose pane shows it.
    pub chat: String,
    /// The page's key.
    pub page: String,
    /// The tab's title.
    pub title: String,
    /// Show it, or take it out.
    pub show: bool,
    pub seq: u64,
}

impl Global for PagePaneAsk {}

/// Ask for page `page` to show in `chat`'s right pane (`show`), or to leave it.
pub fn ask_pane(chat: &str, page: &str, title: &str, show: bool, cx: &mut App) {
    let seq = cx.try_global::<PagePaneAsk>().map_or(0, |ask| ask.seq).wrapping_add(1);
    cx.set_global(PagePaneAsk {
        chat: chat.to_owned(),
        page: page.to_owned(),
        title: title.to_owned(),
        show,
        seq,
    });
}

/// The gap between a PDF's pages in the pane, and around them.
const PAGE_GAP: f32 = 16.0;
/// The widest a PDF's page draws in the pane.
const PAGE_MAX_WIDTH: f32 = 960.0;
/// The pane's header line.
const HEADER_HEIGHT: f32 = 32.0;

/// A page in a tab of the right pane.
pub struct PagePaneView {
    page: String,
    title: SharedString,
    chat: String,
    engine: Option<EngineHandle>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    width: f32,
    scroll: UniformListScrollHandle,
    _host: Option<Subscription>,
}

impl PagePaneView {
    pub fn new(
        page: String,
        title: String,
        chat: String,
        engine: Option<EngineHandle>,
        cx: &mut Context<Self>,
    ) -> Self {
        let weak = cx.weak_entity();
        let host = live::watch_page(
            &page,
            Box::new(move |cx| {
                weak.update(cx, |_, cx| cx.notify()).ok();
            }),
            cx,
        );
        cx.on_release(|this: &mut Self, cx| {
            live::page_act(&this.page, PageAct::Rest, cx);
            move_page(this.engine.clone(), &this.page, false, cx);
        })
        .detach();
        Self {
            page,
            title: title.into(),
            chat,
            engine,
            bounds: Rc::default(),
            width: 0.0,
            scroll: UniformListScrollHandle::new(),
            _host: host,
        }
    }

    /// The tab's title.
    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The page it shows.
    pub fn page(&self) -> &str {
        &self.page
    }

    /// A PDF's pages, each drawn as it scrolls into view.
    fn pdf_pages(&self, pages: usize, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        uniform_list("page-pane-pdf", pages, move |range, window, app| {
            entity.update(app, |this, cx| {
                let theme = Theme::of(cx).clone();
                let width = (this.width - 2.0 * PAGE_GAP).clamp(120.0, PAGE_MAX_WIDTH);
                let Some(PageDoc::Pdf(pdf)) =
                    live::page(&this.page, window, cx).and_then(|page| page.doc)
                else {
                    return Vec::new();
                };
                let height = width * pdf.aspect.max(0.05);
                range
                    .map(|index| {
                        let picture = pdf.picture(index, width);
                        if picture.is_none() {
                            live::page_act(&this.page, PageAct::Draw { index, width }, cx);
                        }
                        let picture = picture.or_else(|| pdf.any_picture(index));
                        div()
                            .w_full()
                            .h(px(height + PAGE_GAP))
                            .flex()
                            .justify_center()
                            .pt(px(PAGE_GAP))
                            .child(doc::page_image(
                                SharedString::from(format!("{}-pane-page-{index}", this.page)),
                                picture,
                                width,
                                height,
                                index,
                                pdf.pages,
                                &theme,
                            ))
                            .into_any_element()
                    })
                    .collect()
            })
        })
        .size_full()
        .track_scroll(&self.scroll)
        .into_any_element()
    }

    /// A table's every row, its header held over them.
    fn table_rows(&self, table: &doc::PageTable, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let geometry = table.geometry();
        let id = format!("{}-pane", self.page);
        let header = (!table.header.is_empty())
            .then(|| doc::table_row(&id, 0, &table.header, true, &geometry, theme));
        let rows = uniform_list("page-pane-table", table.rows.len(), move |range, window, app| {
            entity.update(app, |this, cx| {
                let theme = Theme::of(cx).clone();
                let Some(PageDoc::Table(table)) =
                    live::page(&this.page, window, cx).and_then(|page| page.doc)
                else {
                    return Vec::new();
                };
                let geometry = table.geometry();
                let id = format!("{}-pane", this.page);
                let hairline = crate::markdown::render::table_hairline();
                range
                    .filter_map(|ix| {
                        let record = table.rows.get(ix)?;
                        Some(
                            div()
                                .h(px(TABLE_ROW_HEIGHT))
                                .w_full()
                                .flex()
                                .flex_col()
                                .child(div().flex_none().h(px(1.0)).w_full().bg(hairline))
                                .child(doc::table_row(
                                    &id,
                                    ix.saturating_add(1),
                                    record,
                                    false,
                                    &geometry,
                                    &theme,
                                ))
                                .into_any_element(),
                        )
                    })
                    .collect()
            })
        })
        .flex_1()
        .w_full()
        .track_scroll(&self.scroll);
        div()
            .id(SharedString::from(format!("{id}-table")))
            .role(gpui::Role::Table)
            .aria_label(self.title.clone())
            .size_full()
            .px(px(PAGE_GAP))
            .flex()
            .flex_col()
            .children(header)
            .child(rows)
            .children(doc::more_rows_words(table, table.rows.len()).map(|words| {
                div()
                    .py(px(Theme::SPACE_SM))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(words))
            }))
            .into_any_element()
    }
}

/// Ask the view model to lay page `page` out for the pane (`pane`) or for its card.
pub(crate) fn move_page(engine: Option<EngineHandle>, page: &str, pane: bool, cx: &mut App) {
    let Some(engine) = engine else { return };
    let params = serde_json::json!({ "page": page, "to": if pane { "pane" } else { "inline" } });
    cx.spawn(async move |_| {
        if let Err(err) = engine.client().call(calls::MOVE_PAGE, params).await {
            tracing::warn!(error = %err, "a page did not move");
        }
    })
    .detach();
}

impl Render for PagePaneView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let hosted = live::page(&self.page, window, cx);
        if let Some(bounds) = self.bounds.get() {
            let width = f32::from(bounds.size.width);
            if (width - self.width).abs() > 0.5 {
                self.width = width;
                if hosted.as_ref().is_some_and(|page| page.doc.is_none()) {
                    live::page_act(&self.page, PageAct::Width(width), cx);
                }
            }
        }
        let presence = hosted.as_ref().map(|page| page.presence);
        let web = hosted.as_ref().is_some_and(|page| page.doc.is_none());
        if web && presence.is_some_and(|presence| presence != PagePresence::Live) {
            // The pane is where the person works with it: it stays live while shown.
            live::page_act(&self.page, PageAct::Engage, cx);
        }
        let body: AnyElement = match hosted.as_ref() {
            // slopty: why, and Try again.
            Some(live::HostedPage { failed: Some(why), .. }) => {
                super::failed_body(&format!("{}-pane", self.page), why.clone(), true, &self.page, &theme)
            }
            Some(live::HostedPage { doc: Some(PageDoc::Pdf(pdf)), .. }) => {
                self.pdf_pages(pdf.pages, cx)
            }
            Some(live::HostedPage { doc: Some(PageDoc::Table(table)), .. }) => {
                let table = table.clone();
                self.table_rows(&table, &theme, cx)
            }
            Some(live::HostedPage { native: Some(native), presence: PagePresence::Live, .. }) => {
                gpui::composition::native_view(native)
                    .id(SharedString::from(format!("{}-pane-native", self.page)))
                    .size_full()
                    .into_any_element()
            }
            Some(live::HostedPage { snapshot: Some(picture), .. }) => {
                img(gpui::ImageSource::Render(picture.clone()))
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element()
            }
            // slopty: the surfaces' loading anatomy.
            Some(live::HostedPage { waiting: Some(machine), .. }) => {
                live::chrome::loading(super::waiting_words(machine), &theme, cx.entity_id(), cx).into_any_element()
            }
            _ => live::chrome::loading("Loading the page…", &theme, cx.entity_id(), cx).into_any_element(),
        };
        let back = live::chrome::icon_button(
            format!("{}-pane-back", self.page),
            crate::icons::ALT_ARROW_LEFT,
            "Back to the chat",
            &theme,
        )
        .on_click({
            let (page, title, chat) = (self.page.clone(), self.title.to_string(), self.chat.clone());
            move |_, _, cx| ask_pane(&chat, &page, &title, false, cx)
        });
        let probe = self.bounds.clone();
        div()
            .id(SharedString::from(format!("{}-pane", self.page)))
            .role(gpui::Role::Group)
            .aria_label(SharedString::from(format!("Page: {}", self.title)))
            .aria_value("pane")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(
                div()
                    .h(px(HEADER_HEIGHT))
                    .flex_none()
                    .w_full()
                    .px(px(Theme::SPACE_MD))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_color(theme.text.opacity(0.85))
                            .child(self.title.clone()),
                    )
                    .child(back),
            )
            .child(div().h(px(1.0)).w_full().flex_none().bg(theme.border))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_hidden()
                    .child(body)
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                if probe.get() != Some(bounds) {
                                    probe.set(Some(bounds));
                                }
                            },
                            |_, _, _, _| (),
                        )
                        .absolute()
                        .inset_0(),
                    ),
            )
    }
}
