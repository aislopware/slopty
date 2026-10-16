//! slopty: a page part's row hosts its card ([`crate::page::PageCardView`]), kept per row so the
//! page's web view and watch outlive a frame; a chart fence in a reply hosts one too
//! ([`crate::page::FenceChart`]).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyElement, Context, Entity, SharedString, Window, div, prelude::*};
use slopty_view_doc::PageCard;

use super::{Row, Transcript};
use crate::page::PageCardView;
use crate::theme::Theme;

impl Transcript {
    /// A page part's card, kept per row so its web view and watch outlive a frame.
    pub(crate) fn render_page(
        &mut self,
        row: &Row,
        _owner: &str,
        card: &Arc<PageCard>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view: Entity<PageCardView> = match self.pages.get(&row.id) {
            Some(view) => {
                let card = card.clone();
                view.update(cx, |view, cx| view.set_card(card, cx));
                view.clone()
            }
            None => {
                let chat = self.work_chat();
                let engine = self.state.read(cx).engine().cloned();
                let card = card.clone();
                let view = cx.new(|cx| PageCardView::new(card, chat, engine, cx));
                self.pages.insert(row.id.clone(), view.clone());
                view
            }
        };
        let (top, bottom) = self.page_clear_band();
        view.update(cx, |view, _| view.set_clear_band(top, bottom));
        div().w_full().child(view).into_any_element()
    }

    /// Chart fences' media: a fence whose language names a chart draws as its page card, kept
    /// per fence; `None` leaves any other fence code.
    pub(super) fn chart_media(&self, cx: &Context<Self>) -> Rc<crate::markdown::render::ChartFn> {
        let cards = self.fences.clone();
        let chat = self.work_chat();
        let engine = self.state.read(cx).engine().cloned();
        Rc::new(move |language: &str, code: &str, id: SharedString, _theme: &Theme| {
            crate::page::fence::chart_language(language)?;
            let key = format!("{}{id}", crate::page::fence::FENCE_SCHEME);
            let fence = crate::page::fence::Fence {
                language: language.to_owned(),
                source: code.to_owned(),
                chat: chat.clone(),
            };
            Some(
                crate::page::FenceChart { key, fence, engine: engine.clone(), cards: cards.clone() }
                    .into_any_element(),
            )
        })
    }

    /// The band of the window a live page may sit in: below the titlebar's fade, above the
    /// composer stack and its fade. It is called while the list lays out its rows, so it reads the
    /// viewport as the frame began rather than the list.
    fn page_clear_band(&self) -> (f32, f32) {
        let viewport = self.page_viewport;
        let top = f32::from(viewport.top()) + Theme::TITLEBAR_HEIGHT + Theme::TRANSCRIPT_FADE_BAND;
        let bottom = f32::from(viewport.bottom()) - self.bottom_clearance - Theme::TRANSCRIPT_FADE_BAND;
        (top, bottom.max(top))
    }
}
