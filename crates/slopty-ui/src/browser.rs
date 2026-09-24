//! A browser tile: a web page, usually a server on the worker reached through a forwarded
//! port, in a native web view laid over the tile.
//!
//! GPUI cannot draw a page, so the page is the platform's web view (`slopty_platform::web`),
//! a native view that always draws above everything GPUI draws. The workspace measures the
//! tile's body every frame and asks [`placement`] where the page goes: over the body, cut to
//! the strip, or nowhere when GPUI has something to show on top (the palette, a menu, the
//! overview) or the tile is off the strip. While the page is hidden the body shows its last
//! snapshot, so covering it never leaves a hole.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::{
    AppContext as _, Bounds, Context, EventEmitter, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, Pixels, Render, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, Window, canvas, div, img,
    px,
};
use slopty_client::layout::Rect;
use slopty_core::ItemId;
use slopty_theme::Theme;

use crate::colors::hsla;

/// Below this the tile is fading out (or in) and the page stays hidden; a native view would
/// not fade with it.
pub const MIN_ALPHA: f32 = 0.05;

/// How often a shown page is asked for its title and address, which scripts change without
/// a navigation.
const POLL: Duration = Duration::from_secs(1);

/// What GPUI is drawing over the strip this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cover {
    /// An overlay: the palette, the picker, a titlebar menu, a dialog of the app's.
    pub overlay: bool,
    /// The overview, open or on its way.
    pub overview: bool,
}

/// Where a page goes this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Out of sight: covered, off the strip, fading, or not drawn at all.
    Hidden,
    /// Over `frame`, cut to `clip`, at `alpha`; window coordinates in points.
    Shown {
        /// The strip's area: nothing of the page shows outside it.
        clip: Rect,
        /// The tile's body.
        frame: Rect,
        /// The tile's opacity.
        alpha: f32,
    },
}

/// Where the page of a tile goes: `body` is where its tile's body was drawn this frame (none
/// when the tile was not drawn), `strip` the strip's area, `alpha` the tile's opacity.
#[must_use]
pub fn placement(body: Option<Rect>, strip: Rect, alpha: f32, cover: Cover) -> Placement {
    let Some(frame) = body else { return Placement::Hidden };
    let visible = frame.w >= 1.0 && frame.h >= 1.0 && frame.intersects(&strip);
    if cover.overlay || cover.overview || alpha < MIN_ALPHA || !visible {
        return Placement::Hidden;
    }
    Placement::Shown { clip: strip, frame, alpha: alpha.min(1.0) }
}

/// Where a tile's body was drawn, and in which of the workspace's frames.
type Drawn = Rc<Cell<Option<(u64, Bounds<Pixels>)>>>;

/// What a browser tile tells the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserEvent {
    /// The page was clicked and has the keyboard: its tile takes the focus.
    Focused,
    /// The page gave the keyboard back (a click elsewhere, ⌃Tab, Esc twice).
    Released,
}

/// What the page shows, as last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    /// The document's title, empty before one loads.
    pub title: String,
    /// The address shown now.
    pub url: String,
    /// A navigation is under way.
    pub loading: bool,
    /// There is a page to go back to.
    pub can_go_back: bool,
    /// Why the last navigation failed, until the next one starts.
    pub failed: Option<String>,
}

/// The view of one browser item.
pub struct BrowserView {
    id: ItemId,
    url: String,
    page: PageState,
    /// Where the body was drawn, and in which frame of the workspace's.
    drawn: Drawn,
    /// The tile's opacity this frame.
    alpha: f32,
    /// The page's last picture, shown while the page is hidden.
    snapshot: Option<Arc<RenderImage>>,
    theme: Theme,
    native: native::Native,
    /// The page's messages, and the poll for its title while it is open.
    tasks: Vec<Task<()>>,
}

impl std::fmt::Debug for BrowserView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserView")
            .field("id", &self.id)
            .field("url", &self.url)
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<BrowserEvent> for BrowserView {}

impl BrowserView {
    /// A tile for `url`; the page itself opens the first time the tile is shown.
    #[must_use]
    pub fn new(id: ItemId, url: &str, theme: Theme) -> Self {
        Self {
            id,
            url: url.to_owned(),
            page: PageState { url: url.to_owned(), loading: true, ..PageState::default() },
            drawn: Rc::default(),
            alpha: 1.0,
            snapshot: None,
            theme,
            native: native::Native::default(),
            tasks: Vec::new(),
        }
    }

    /// Item this tile belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// The address the item names.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// What the page shows, as last read.
    #[must_use]
    pub const fn page(&self) -> &PageState {
        &self.page
    }

    /// What the page shows this moment, read from the view itself (the self-test's
    /// readback); the last read when there is no view.
    #[must_use]
    pub fn live_page(&self) -> PageState {
        self.native.page().map_or_else(
            || self.page.clone(),
            |page| PageState { failed: self.page.failed.clone(), ..page },
        )
    }

    /// Whether the page is on screen now.
    #[must_use]
    pub fn shown(&self) -> bool {
        self.native.shown()
    }

    /// Whether the page has the keyboard.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.native.focused()
    }

    /// Whether a picture of the page is ready for when it is hidden.
    #[must_use]
    pub const fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    /// Where the body was drawn in `frame` of the workspace's, if it was.
    #[must_use]
    pub fn drawn_in(&self, frame: u64) -> Option<Bounds<Pixels>> {
        self.drawn.get().filter(|(f, _)| *f == frame).map(|(_, b)| b)
    }

    /// The tile's opacity this frame.
    pub const fn set_alpha(&mut self, alpha: f32) {
        self.alpha = alpha;
    }

    /// The tile's opacity as last set.
    #[must_use]
    pub const fn alpha(&self) -> f32 {
        self.alpha
    }

    /// Draw by another theme.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    /// Put the page where [`placement`] says, opening it the first time it is shown. A
    /// failed page stays hidden, so its reason shows instead of a blank page.
    pub fn apply(&mut self, placement: Placement, window: &Window, cx: &mut Context<Self>) {
        match placement {
            Placement::Shown { clip, frame, alpha } if self.page.failed.is_none() => {
                if !self.native.open() {
                    self.open(window, cx);
                }
                self.native.place(clip, frame, alpha);
            }
            Placement::Shown { .. } | Placement::Hidden => {
                if self.native.shown() {
                    // The picture it leaves behind is the page as it was a moment ago.
                    self.native.snapshot();
                    self.native.hide();
                }
            }
        }
    }

    fn open(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<native::Event>();
        let sink: Rc<dyn Fn(native::Event)> = Rc::new(move |event| {
            let _sent = tx.send(event);
        });
        if !self.native.create(window, &self.url, sink) {
            self.page.failed = Some("This device has no web view".to_owned());
            cx.notify();
            return;
        }
        tracing::info!(item = %self.id, url = %self.url, "browser tile opened");
        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                if this.update(cx, |view, cx| view.native_event(event, cx)).is_err() {
                    break;
                }
            }
        });
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, Self::refresh).is_err() {
                    break;
                }
            }
        });
        self.tasks = vec![events, poll];
    }

    /// A message from the page.
    fn native_event(&mut self, event: native::Event, cx: &mut Context<Self>) {
        if !matches!(event, native::Event::Snapshot(_)) {
            tracing::debug!(item = %self.id, ?event, "page event");
        }
        match event {
            native::Event::Loaded => {
                self.page.failed = None;
                self.refresh(cx);
                self.native.snapshot();
            }
            native::Event::Failed(why) => {
                tracing::info!(item = %self.id, %why, "page failed");
                self.page.failed = Some(why);
                self.page.loading = false;
                self.native.hide();
                cx.notify();
            }
            native::Event::Clicked => cx.emit(BrowserEvent::Focused),
            native::Event::Released => {
                self.native.snapshot();
                cx.emit(BrowserEvent::Released);
            }
            native::Event::Snapshot(png) => {
                let decoding = cx.background_spawn(async move { texture(&png) });
                cx.spawn(async move |this, cx| {
                    if let Some(image) = decoding.await {
                        let _gone = this.update(cx, |view, cx| {
                            view.snapshot = Some(image);
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
        }
    }

    /// Read the page's title and address again; a change repaints the header.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(page) = self.native.page() else { return };
        let page = PageState { failed: self.page.failed.clone(), ..page };
        if page != self.page {
            self.page = page;
            cx.notify();
        }
    }

    /// Back one page.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        self.native.back();
        self.page.loading = true;
        cx.notify();
    }

    /// Load the page again; a failed page gets another try.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.page.failed.take().is_some() {
            self.native.load(&self.url);
        } else {
            self.native.reload();
        }
        self.page.loading = true;
        cx.notify();
    }

    /// Give the page the keyboard (a click on the tile's body when the page is hidden does
    /// nothing; the page takes its own clicks).
    pub fn focus(&self) {
        self.native.focus();
    }

    /// Take the keyboard back from the page.
    pub fn release(&self) {
        self.native.release();
    }

    /// The header's words: the page's title, else its address without the scheme.
    #[must_use]
    pub fn title(&self) -> String {
        if self.page.title.trim().is_empty() {
            short_url(&self.page.url).to_owned()
        } else {
            self.page.title.trim().to_owned()
        }
    }

    /// The address as the header shows it beside a title: host, port and path.
    #[must_use]
    pub fn short_url(&self) -> &str {
        short_url(&self.page.url)
    }
}

/// `url` without its scheme and a lone trailing slash: `localhost:5173/app`.
#[must_use]
pub fn short_url(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.strip_suffix('/').filter(|r| !r.contains('/')).unwrap_or(rest)
}

/// Whether `text` is an address a browser tile opens: http or https, a host, at most 2 KiB
/// (the worker's own limit).
#[must_use]
pub fn is_web_url(text: &str) -> bool {
    let text = text.trim();
    let Some(rest) = text.strip_prefix("http://").or_else(|| text.strip_prefix("https://")) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // `[::1]` is a host whose colons are not a port.
    let port_ok = authority.ends_with(']')
        || authority
            .rsplit_once(':')
            .is_none_or(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
    text.len() <= 2048 && !authority.is_empty() && port_ok && !rest.contains(char::is_whitespace)
}

/// `text` as an address: as it is when it names a scheme, else `http://` in front
/// (`localhost:5173` is a dev server, not a search).
#[must_use]
pub fn web_url(text: &str) -> Option<String> {
    let text = text.trim();
    let url = if text.contains("://") { text.to_owned() } else { format!("http://{text}") };
    is_web_url(&url).then_some(url)
}

/// A PNG as GPUI keeps pictures: premultiplied BGRA.
fn texture(png: &[u8]) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory(png).ok()?.into_rgba8();
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(rgba)])))
}

impl Render for BrowserView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let id = *self.id.as_uuid();
        let drawn = Rc::clone(&self.drawn);
        let frame = cx.try_global::<FrameCount>().map_or(0, |f| f.0);
        let measure = canvas(
            move |bounds, _window, _cx| drawn.set(Some((frame, bounds))),
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        let notice = |text: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .px(px(theme.spacing.md))
                .text_size(px(theme.typography.small()))
                .font_family(theme.typography.ui_family.clone())
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(text))
        };
        let body = match (&self.page.failed, &self.snapshot) {
            (Some(why), _) => {
                notice(format!("Can't open {}: {why}", short_url(&self.url))).into_any_element()
            }
            (None, Some(image)) => {
                img(Arc::clone(image)).size_full().object_fit(ObjectFit::Cover).into_any_element()
            }
            (None, None) => notice(format!("Opening {}…", short_url(&self.url))).into_any_element(),
        };
        div()
            .id(SharedString::from(format!("browser-{id}")))
            .debug_selector(move || format!("browser-{id}"))
            .role(Role::Document)
            .aria_label(SharedString::from(format!("Page {}", self.page.url)))
            .aria_value(SharedString::from(self.title()))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hsla(s.canvas))
            .child(body)
            .child(measure)
    }
}

/// The workspace's frame number, bumped every time it draws: a body measured in an older
/// frame was not drawn in this one.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCount(pub u64);

impl gpui::Global for FrameCount {}

/// The platform's web view (`WKWebView` on macOS and iOS) behind the tile's API.
mod native {
    use std::rc::Rc;

    use gpui::Window;
    use slopty_client::layout::Rect;
    pub use slopty_platform::web::WebEvent as Event;

    use super::PageState;

    /// The page, once it is open.
    #[derive(Default)]
    pub struct Native {
        view: Option<slopty_platform::web::WebView>,
    }

    fn frame(r: Rect) -> slopty_platform::web::Frame {
        slopty_platform::web::Frame {
            x: f64::from(r.x),
            y: f64::from(r.y),
            w: f64::from(r.w),
            h: f64::from(r.h),
        }
    }

    impl Native {
        pub const fn open(&self) -> bool {
            self.view.is_some()
        }

        /// Open the page in `window`'s view; `false` when the platform has none to give.
        pub fn create(&mut self, window: &Window, url: &str, sink: Rc<dyn Fn(Event)>) -> bool {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let host = match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
                Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view,
                Ok(RawWindowHandle::UiKit(handle)) => handle.ui_view,
                _ => return false,
            };
            self.view = slopty_platform::web::WebView::new(host, url, sink);
            self.view.is_some()
        }

        pub fn place(&self, clip: Rect, at: Rect, alpha: f32) {
            if let Some(view) = &self.view {
                view.place(frame(clip), frame(at), f64::from(alpha));
            }
        }

        pub fn hide(&self) {
            if let Some(view) = &self.view {
                view.hide();
            }
        }

        pub fn shown(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::shown)
        }

        pub fn focused(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::focused)
        }

        pub fn page(&self) -> Option<PageState> {
            self.view.as_ref().map(|v| {
                let p = v.page();
                PageState {
                    title: p.title,
                    url: p.url,
                    loading: p.loading,
                    can_go_back: p.can_go_back,
                    failed: None,
                }
            })
        }

        pub fn snapshot(&self) {
            if let Some(view) = &self.view {
                view.snapshot();
            }
        }

        pub fn load(&self, url: &str) {
            if let Some(view) = &self.view {
                view.load(url);
            }
        }

        pub fn back(&self) {
            if let Some(view) = &self.view {
                view.back();
            }
        }

        pub fn reload(&self) {
            if let Some(view) = &self.view {
                view.reload();
            }
        }

        pub fn focus(&self) {
            if let Some(view) = &self.view {
                view.focus();
            }
        }

        pub fn release(&self) {
            if let Some(view) = &self.view {
                view.release();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIP: Rect = Rect { x: 0.0, y: 40.0, w: 1200.0, h: 760.0 };
    const BODY: Rect = Rect { x: 8.0, y: 76.0, w: 590.0, h: 700.0 };

    #[test]
    fn a_drawn_uncovered_tile_shows_its_page_cut_to_the_strip() {
        assert_eq!(
            placement(Some(BODY), STRIP, 1.0, Cover::default()),
            Placement::Shown { clip: STRIP, frame: BODY, alpha: 1.0 }
        );
        // Half off the strip's left edge: still shown, the clip cuts it.
        let half = Rect { x: -300.0, ..BODY };
        assert_eq!(
            placement(Some(half), STRIP, 1.0, Cover::default()),
            Placement::Shown { clip: STRIP, frame: half, alpha: 1.0 }
        );
    }

    #[test]
    fn anything_gpui_draws_on_top_hides_the_page() {
        let covered = |cover| placement(Some(BODY), STRIP, 1.0, cover);
        assert_eq!(covered(Cover { overlay: true, overview: false }), Placement::Hidden);
        assert_eq!(covered(Cover { overlay: false, overview: true }), Placement::Hidden);
    }

    #[test]
    fn a_tile_off_the_strip_fading_or_not_drawn_hides_the_page() {
        assert_eq!(placement(None, STRIP, 1.0, Cover::default()), Placement::Hidden);
        let gone = Rect { x: 1300.0, ..BODY };
        assert_eq!(placement(Some(gone), STRIP, 1.0, Cover::default()), Placement::Hidden);
        assert_eq!(placement(Some(BODY), STRIP, 0.01, Cover::default()), Placement::Hidden);
        let flat = Rect { h: 0.5, ..BODY };
        assert_eq!(placement(Some(flat), STRIP, 1.0, Cover::default()), Placement::Hidden);
        // Fading in past the threshold: shown, at the tile's opacity.
        assert_eq!(
            placement(Some(BODY), STRIP, 0.5, Cover::default()),
            Placement::Shown { clip: STRIP, frame: BODY, alpha: 0.5 }
        );
    }

    #[test]
    fn addresses_are_http_or_https_with_a_host() {
        assert!(is_web_url("http://localhost:5173/"));
        assert!(is_web_url("https://example.test/a?b=c"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("http://"));
        assert!(!is_web_url("http://localhost:"));
        assert!(is_web_url("http://[::1]"));
        assert!(is_web_url("http://[::1]:5173/"));
        assert!(!is_web_url(&format!("http://h/{}", "a".repeat(2048))));
        assert_eq!(web_url("localhost:5173").as_deref(), Some("http://localhost:5173"));
        assert_eq!(web_url("https://a.test").as_deref(), Some("https://a.test"));
        assert_eq!(web_url("ftp://a.test"), None);
        assert_eq!(short_url("http://localhost:5173/"), "localhost:5173");
        assert_eq!(short_url("http://localhost:5173/app/"), "localhost:5173/app/");
    }

    #[test]
    fn an_address_typed_into_the_palette_opens_in_a_tile() {
        let items = crate::palette::path_items("http://localhost:5173/");
        let runs: Vec<String> = items.iter().map(|i| format!("{:?}", i.run)).collect();
        assert_eq!(runs, [r#"OpenInTile("http://localhost:5173/")"#]);
        assert_eq!(items.first().map(|i| i.label.as_str()), Some("Open localhost:5173 in a tile"));
        assert!(crate::palette::path_items("http://localhost:").is_empty(), "no host yet");
        assert!(crate::palette::path_items("javascript://x").is_empty());
    }
}
