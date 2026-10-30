//! A chart fence in a reply (```` ```vega-lite ````, ```` ```vega ````, ```` ```plotly ````)
//! drawn as a page of its own.
//!
//! The fence's JSON becomes a page card like any page an agent shows, its key
//! `slopty-fence:{row}-chart-{n}`: the view model reads the fence here ([`fence`]) and builds
//! the page from the chart libraries the app vendors, served from the page's own origin, so a
//! chart draws offline and reaches no network. A fence that is not a chart's JSON stays code.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, Entity, Global, IntoElement, RenderOnce, SharedString, Window, div};
use slopty_view_doc::PageCard;

use crate::page::PageCardView;
use crate::state::EngineHandle;

/// The scheme of a fence page's key.
pub const FENCE_SCHEME: &str = "slopty-fence:";
/// A fence page's card kind.
pub const CHART_KIND: &str = "chart";

/// The chart a fence's language names, by its name in the card's header; `None` for code.
/// The view model builds the page by `slopty_platform::web::page::libs::Chart::of`, which
/// reads the same names.
#[must_use]
pub fn chart_language(language: &str) -> Option<&'static str> {
    match language.trim().to_ascii_lowercase().as_str() {
        "vega-lite" | "vegalite" | "vl" => Some("Vega-Lite"),
        "vega" => Some("Vega"),
        "plotly" | "plotly.js" | "plotly-json" => Some("Plotly"),
        _ => None,
    }
}

/// Every language a chart fence may name.
pub const CHART_LANGUAGES: [&str; 7] =
    ["vega-lite", "vegalite", "vl", "vega", "plotly", "plotly.js", "plotly-json"];

/// Whether page `key` is a fence's.
#[must_use]
pub fn is_fence(key: &str) -> bool {
    key.starts_with(FENCE_SCHEME)
}

/// A chart fence, as its page reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fence {
    /// The fence's language, as written.
    pub language: String,
    /// Its JSON.
    pub source: String,
    /// The chat it is in.
    pub chat: Option<String>,
}

/// The fences drawn, by page key.
#[derive(Default)]
struct Fences(HashMap<String, Fence>);

impl Global for Fences {}

/// Fence `key` as it stands now.
pub fn put_fence(key: &str, fence: Fence, cx: &mut App) {
    let fences = cx.default_global::<Fences>();
    if fences.0.get(key) != Some(&fence) {
        fences.0.insert(key.to_owned(), fence);
    }
}

/// Fence `key`, once a transcript drew it.
#[must_use]
pub fn fence(key: &str, cx: &App) -> Option<Fence> {
    cx.try_global::<Fences>()?.0.get(key).cloned()
}

/// A transcript's fence cards, by page key, kept so their web views outlive a frame.
pub type FenceCards = Rc<RefCell<HashMap<String, Entity<PageCardView>>>>;

/// A chart fence's card, made the first time it draws.
#[derive(IntoElement)]
pub struct FenceChart {
    /// The fence's page key.
    pub key: String,
    /// What the fence says.
    pub fence: Fence,
    /// The transcript's engine.
    pub engine: Option<EngineHandle>,
    /// The transcript's fence cards.
    pub cards: FenceCards,
}

impl FenceChart {
    /// The card a fence shows as.
    #[must_use]
    pub fn card(key: &str, fence: &Fence) -> PageCard {
        let name = chart_language(&fence.language).unwrap_or("Chart");
        let title = serde_json::from_str::<serde_json::Value>(&fence.source)
            .ok()
            .and_then(|spec| chart_title(&spec))
            .unwrap_or_else(|| format!("{name} chart"));
        PageCard {
            id: format!("{key}:page"),
            page: key.to_owned(),
            title,
            kind: CHART_KIND.to_owned(),
            source: name.to_owned(),
            ..PageCard::default()
        }
    }
}

/// The title a chart's spec gives itself: Vega's `title` (a string or `{text}`), Plotly's
/// `layout.title`.
fn chart_title(spec: &serde_json::Value) -> Option<String> {
    let title =
        spec.get("title").or_else(|| spec.get("layout").and_then(|layout| layout.get("title")))?;
    let text = match title {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Object(title) => title.get("text")?.as_str()?.to_owned(),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(80).collect())
}

impl RenderOnce for FenceChart {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let card = Arc::new(Self::card(&self.key, &self.fence));
        put_fence(&self.key, self.fence.clone(), cx);
        let existing = self.cards.borrow().get(&self.key).cloned();
        let view = match existing {
            Some(view) => {
                view.update(cx, |view, cx| view.set_card(card, cx));
                view
            }
            None => {
                let (chat, engine) = (self.fence.chat.clone(), self.engine.clone());
                let view = cx.new(|cx| PageCardView::new(card, chat, engine, cx));
                self.cards.borrow_mut().insert(self.key.clone(), view.clone());
                view
            }
        };
        div().w_full().id(SharedString::from(format!("{}-fence", self.key))).child(view)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fence's language names its chart; any other stays code. The header takes the spec's
    /// own title when it gives one.
    #[test]
    fn a_fence_names_its_chart() {
        for language in CHART_LANGUAGES {
            assert!(chart_language(language).is_some(), "{language} is a chart");
        }
        assert_eq!(chart_language(" Vega-Lite "), Some("Vega-Lite"));
        assert_eq!(chart_language("json"), None);
        let fence = Fence {
            language: "plotly".into(),
            source: r#"{"data":[],"layout":{"title":{"text":"p99 by region"}}}"#.into(),
            chat: None,
        };
        assert_eq!(FenceChart::card("slopty-fence:r-chart-0", &fence).title, "p99 by region");
        let bare = Fence { language: "vl".into(), source: "{}".into(), chat: None };
        let card = FenceChart::card("slopty-fence:r-chart-1", &bare);
        assert_eq!((card.title.as_str(), card.kind.as_str()), ("Vega-Lite chart", CHART_KIND));
        assert!(is_fence(&card.page));
    }
}
