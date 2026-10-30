// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Device-local browser tabs. GPUI owns chrome; the native host owns pages.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as native;
pub mod model;
pub(crate) mod profile;
mod view;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription,
    Window,
};
use model::{PageState, Presentation};

use crate::composer::{ComposerInput, ComposerInputEvent};
mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    gpui::actions!(
        browser,
        [
            /// Load the page again (or, after a failed load, try its address again).
            Reload,
            /// Put the keyboard in the address field, its text selected.
            FocusAddress,
            /// Open a new browser tab beside this one.
            NewTab,
            /// Close this browser tab.
            CloseTab,
            /// Go back in the page's history.
            Back,
            /// Go forward in the page's history.
            Forward,
        ]
    );
}
pub use actions::{Back, CloseTab, FocusAddress, Forward, NewTab, Reload};

pub(crate) fn bind_keys(cx: &mut App, keymap: &crate::settings::KeymapConfig) {
    use crate::settings::{ShortcutId, parse_combo, platform_combo};
    // A chord the person gave a window command wins: Changes' key on ⇧⌘R, say.
    let available = |combo: &str, own: Option<ShortcutId>| {
        !combo.is_empty()
            && !ShortcutId::ALL.iter().any(|id| {
                Some(*id) != own && parse_combo(keymap.get(*id)).ok() == parse_combo(combo).ok()
            })
    };
    macro_rules! bind {
        ($combo:literal, $action:ident) => {
            if available($combo, None) {
                cx.bind_keys(crate::keys::binding(
                    &platform_combo($combo),
                    $action,
                    Some("Browser"),
                    cx,
                ));
            }
        };
    }
    bind!("mod-l", FocusAddress);
    bind!("mod-t", NewTab);
    bind!("mod-w", CloseTab);
    bind!("mod-[", Back);
    bind!("mod-]", Forward);
    let reload = keymap.get(ShortcutId::BrowserReload);
    if available(reload, Some(ShortcutId::BrowserReload)) && parse_combo(reload).is_ok() {
        cx.bind_keys(crate::keys::binding(&platform_combo(reload), Reload, Some("Browser"), cx));
    }
}

/// What a browser tab tells the shell.
#[derive(Clone, Debug)]
pub enum BrowserEvent {
    /// Its page, title, icon or loading state changed.
    Changed,
    /// It asks for a new tab, at the address given or blank.
    NewTab(Option<String>),
    /// It asks to be closed.
    Close,
}

/// A profile's website data, allocated on first navigation. Default contexts are
/// ephemeral, for unresolved identities and isolated fixtures.
#[derive(Clone, Default)]
#[cfg_attr(
    not(target_os = "macos"),
    expect(
        missing_copy_implementations,
        reason = "the macOS context holds the page store; iOS opens pages outside and keeps none"
    )
)]
pub struct BrowserContext {
    #[cfg(target_os = "macos")]
    data: native::BrowserData,
}

impl std::fmt::Debug for BrowserContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserContext").finish_non_exhaustive()
    }
}

#[cfg(target_os = "macos")]
impl BrowserContext {
    /// The context holding `profile`'s website data.
    pub(crate) fn for_profile(profile: profile::BrowserProfile) -> Self {
        Self { data: native::BrowserData::for_profile(profile) }
    }
}

/// A browser tab: its address bar and toolbar drawn by GPUI, its page by the native host (on
/// iOS a page opens in the system browser).
pub struct BrowserSurface {
    #[cfg(target_os = "macos")]
    context: BrowserContext,
    address: Entity<ComposerInput>,
    focus: FocusHandle,
    page: PageState,
    favicon: Option<std::sync::Arc<gpui::Image>>,
    address_edited: bool,
    validation: Option<String>,
    remote: bool,
    previews: slopty_view_proto::PreviewSnapshot,
    previews_loading: bool,
    previews_task: Option<gpui::Task<()>>,
    presentation: Presentation,
    #[cfg(target_os = "macos")]
    resize_inset: gpui::Pixels,
    _input_sub: Subscription,
    #[cfg(target_os = "macos")]
    native: Option<native::NativePage>,
    #[cfg(target_os = "macos")]
    native_tx: tokio::sync::mpsc::Sender<native::NativeEvent>,
    #[cfg(target_os = "macos")]
    _native_task: gpui::Task<()>,
    #[cfg(target_os = "macos")]
    favicon_task: Option<gpui::Task<()>>,
    #[cfg(target_os = "macos")]
    favicon_generation: u64,
}

impl std::fmt::Debug for BrowserSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSurface")
            .field("page", &self.page)
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<BrowserEvent> for BrowserSurface {}
impl Focusable for BrowserSurface {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BrowserSurface {
    /// A blank tab over `context`'s website data; `remote` when its session runs on another
    /// device, whose localhost this one cannot reach.
    pub fn new(
        context: BrowserContext,
        remote: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| {
            ComposerInput::with_context("Website or localhost:3000", "PaletteSearch", cx)
                .with_text_metrics(11.0, 16.0)
                .with_single_line()
                .with_accessibility_role(gpui::Role::TextInput)
        });
        let input_sub = cx.subscribe(&address, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.address_edited =
                    this.address.read(cx).text() != this.page.url.as_deref().unwrap_or_default();
                this.validation = None;
                cx.notify();
            }
        });
        #[cfg(target_os = "macos")]
        let (native_tx, mut events) = tokio::sync::mpsc::channel(64);
        #[cfg(target_os = "macos")]
        let native_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_native_event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        #[cfg(not(target_os = "macos"))]
        let _: (&Window, BrowserContext) = (window, context);
        Self {
            #[cfg(target_os = "macos")]
            context,
            address,
            focus: cx.focus_handle(),
            page: PageState::default(),
            favicon: None,
            address_edited: false,
            validation: None,
            remote,
            previews: slopty_view_proto::PreviewSnapshot::default(),
            previews_loading: true,
            previews_task: None,
            presentation: Presentation::Hidden,
            #[cfg(target_os = "macos")]
            resize_inset: gpui::px(0.0),
            _input_sub: input_sub,
            #[cfg(target_os = "macos")]
            native: None,
            #[cfg(target_os = "macos")]
            native_tx,
            #[cfg(target_os = "macos")]
            _native_task: native_task,
            #[cfg(target_os = "macos")]
            favicon_task: None,
            #[cfg(target_os = "macos")]
            favicon_generation: 0,
        }
    }

    /// The tab's title: the page's, or its address while it has none.
    pub fn title(&self) -> gpui::SharedString {
        self.page.label().into()
    }

    /// The page shown, kept to show it again.
    pub fn url(&self) -> Option<&str> {
        self.page.url.as_deref()
    }

    /// The page's icon, once it drew one.
    pub const fn favicon(&self) -> Option<&std::sync::Arc<gpui::Image>> {
        self.favicon.as_ref()
    }

    /// Whether the tab's session runs on another device.
    pub const fn set_remote(&mut self, remote: bool) {
        self.remote = remote;
    }

    /// Tell the page which chords are the app's, so it hands them back rather than keeping
    /// them.
    #[cfg(target_os = "macos")]
    pub fn set_shortcuts(&self, keymap: &crate::settings::KeymapConfig) {
        if let Some(native) = &self.native {
            native.set_shortcuts(
                crate::settings::ShortcutId::ALL
                    .iter()
                    .filter(|id| **id != crate::settings::ShortcutId::SaveFile)
                    .map(|id| crate::settings::platform_combo(keymap.get(*id)))
                    .collect(),
            );
        }
    }

    /// Put the keyboard in the address field, its text selected.
    pub fn focus_address(&self, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.focus_chrome();
        }
        window.focus(&self.address.focus_handle(cx), cx);
        window.dispatch_action(Box::new(crate::composer::SelectAll), cx);
    }

    /// Reserve the overlapping part of the shell divider for GPUI hit testing.
    #[cfg(target_os = "macos")]
    pub fn set_resize_inset(&mut self, inset: gpui::Pixels, cx: &mut Context<Self>) {
        if self.resize_inset != inset {
            self.resize_inset = inset;
            cx.notify();
        }
    }

    /// How the page shows: live, as a still while the panes move, or hidden.
    pub fn set_presentation(&mut self, presentation: Presentation, cx: &mut Context<Self>) {
        if self.presentation == presentation {
            return;
        }
        self.presentation = presentation;
        #[cfg(target_os = "macos")]
        if let Some(native) = &mut self.native {
            native.present(presentation);
        }
        cx.notify();
    }

    /// The daemon resolves the session's current cwd for every update, so a
    /// checkout change cannot leave this tab discovering the previous project.
    pub fn watch_previews(
        &mut self,
        handle: crate::state::EngineHandle,
        chat_id: String,
        cx: &Context<Self>,
    ) {
        self.previews_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let subscription = handle
                    .client()
                    .subscribe(
                        slopty_view_rpc::methods::WATCH_PREVIEWS,
                        serde_json::json!({"chatId": chat_id}),
                    )
                    .await;
                if let Ok(mut updates) = subscription {
                    while let Some(value) = updates.recv().await {
                        if let Ok(snapshot) =
                            serde_json::from_value::<slopty_view_proto::PreviewSnapshot>(value)
                            && this
                                .update(cx, |this, cx| {
                                    this.previews = snapshot;
                                    this.previews_loading = false;
                                    cx.notify();
                                })
                                .is_err()
                        {
                            return;
                        }
                    }
                }
                if this
                    .update(cx, |this, cx| {
                        // The watch ended: its list is gone until a new one answers.
                        this.previews.services.clear();
                        this.previews_loading = true;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
            }
        }));
    }

    /// Go to what the person typed: an address, or a host such as `localhost:3000`. An address
    /// the browser refuses leaves the page and says why under the address field.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "the macOS page takes the window; iOS opens the page outside"
        )
    )]
    pub fn navigate(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = match model::normalize_address(input) {
            Ok(url) => url,
            Err(message) => {
                self.validation = Some(message.into());
                cx.notify();
                return;
            }
        };
        self.validation = None;
        self.address.update(cx, |input, cx| input.set_text(url.clone(), cx));
        self.address_edited = false;
        self.page.url = Some(url.clone());
        self.page.title.clear();
        self.page.error = None;
        self.clear_favicon(cx);
        #[cfg(target_os = "macos")]
        {
            self.favicon_generation = self.favicon_generation.wrapping_add(1);
            self.favicon_task = None;
            let result = if let Some(native) = &self.native {
                native.load(&url)
            } else {
                // gpui-fast's native host takes the app too.
                native::NativePage::new(window, &self.context.data, self.native_tx.clone(), cx)
                    .and_then(|native| {
                        native.present(self.presentation);
                        let loaded = native.load(&url);
                        self.native = Some(native);
                        loaded
                    })
            };
            self.page.loading = result.is_ok();
            if let Err(error) = result {
                self.page.error = Some(error);
            }
            window.focus(&self.focus, cx);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _: &Window = window;
            cx.open_url(&url);
        }
        cx.emit(BrowserEvent::Changed);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.address.read(cx).text().to_owned();
        self.navigate(&url, window, cx);
    }

    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::needless_pass_by_ref_mut,
            reason = "the macOS page clears its error; iOS opens the page outside"
        )
    )]
    fn reload(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            if self.page.error.is_some() {
                if let Some(url) = &self.page.url
                    && let Err(error) = native.load(url)
                {
                    // The retry failed as the load did: the page keeps saying why.
                    self.page.error = Some(error);
                    cx.notify();
                    return;
                }
            } else {
                native.reload();
            }
            self.page.error = None;
        }
        #[cfg(not(target_os = "macos"))]
        self.open_external(cx);
        cx.notify();
    }

    fn open_external(&self, cx: &Context<Self>) {
        if let Some(url) = self.page.url.as_deref().filter(|url| model::allowed_navigation(url)) {
            cx.open_url(url);
        }
    }

    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            clippy::unused_self,
            clippy::missing_const_for_fn,
            reason = "the macOS page walks its history; iOS opens pages outside, with none"
        )
    )]
    fn history(&self, forward: bool) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.history(forward);
        }
        #[cfg(not(target_os = "macos"))]
        let _: bool = forward;
    }

    /// Explicitly close even if an async callback temporarily retains an entity.
    fn clear_favicon(&mut self, cx: &mut Context<Self>) {
        if let Some(image) = self.favicon.take() {
            cx.defer(move |cx| gpui::ImageSource::Image(image).evict(None, cx));
        }
    }

    /// Put the page away and let go of it, its icon too.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.set_presentation(Presentation::Hidden, cx);
        self.clear_favicon(cx);
        #[cfg(target_os = "macos")]
        {
            if let Some(native) = &mut self.native {
                native.present(Presentation::Hidden);
            }
            self.native = None;
            self.favicon_task = None;
            self.favicon_generation = self.favicon_generation.wrapping_add(1);
        }
    }
}

#[cfg(target_os = "macos")]
impl BrowserSurface {
    fn on_native_event(
        &mut self,
        event: native::NativeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(native) = &mut self.native else {
            return;
        };
        match event {
            native::NativeEvent::Changed | native::NativeEvent::Finished => {
                let finished = matches!(event, native::NativeEvent::Finished);
                let mut page = native.state();
                // A failed provisional request has no committed WebKit URL.
                if page.url.is_none() {
                    page.url.clone_from(&self.page.url);
                }
                if page.error.is_some() {
                    page.loading = false;
                }
                // A same-document move (history push, fragment) keeps its icon and
                // asks the page again, as a page may change its icon with its route.
                let same_document = page.url != self.page.url
                    && page.url.is_some()
                    && self.page.url.is_some()
                    && !page.loading
                    && !self.page.loading;
                if (page.url != self.page.url && !same_document)
                    || (!self.page.loading && page.loading)
                {
                    if let Some(image) = self.favicon.take() {
                        cx.defer(move |cx| gpui::ImageSource::Image(image).evict(None, cx));
                    }
                    self.favicon_generation = self.favicon_generation.wrapping_add(1);
                    self.favicon_task = None;
                }
                if !self.address.focus_handle(cx).is_focused(window) {
                    if let Some(url) = &page.url
                        && self.address.read(cx).text() != url
                    {
                        self.address.update(cx, |input, cx| input.set_text(url.clone(), cx));
                    }
                    self.address_edited = false;
                }
                native.present(self.presentation);
                if (finished || same_document) && page.url.is_some() {
                    native.discover_favicon();
                }
                if page != self.page {
                    self.page = page;
                    cx.emit(BrowserEvent::Changed);
                }
                cx.notify();
            }
            native::NativeEvent::NewTab(url) => {
                if self.presentation == Presentation::Live {
                    cx.emit(BrowserEvent::NewTab(Some(url)));
                }
            }
            native::NativeEvent::Key(key) => {
                if self.presentation == Presentation::Live {
                    window.focus(&self.focus, cx);
                    window.defer(cx, move |window, cx| {
                        window.dispatch_keystroke(key, cx);
                    });
                }
            }
            // The page draws its own icon (`macos::FAVICON_SCRIPT`), decoded off the main thread
            // and kept only for the page it is of.
            native::NativeEvent::Favicon { page, data } => {
                if self.page.url.as_deref() != Some(&page) {
                    return;
                }
                let generation = self.favicon_generation;
                let decode = cx.background_spawn(async move { favicon_png(&data) });
                self.favicon_task = Some(cx.spawn(async move |this, cx| {
                    let Some(bytes) = decode.await else {
                        return;
                    };
                    // A gone tab has no icon to show.
                    let _gone = this.update(cx, |this, cx| {
                        if this.favicon_generation == generation
                            && this.page.url.as_deref() == Some(&page)
                        {
                            if let Some(image) = this.favicon.take() {
                                cx.defer(move |cx| {
                                    gpui::ImageSource::Image(image).evict(None, cx);
                                });
                            }
                            this.favicon = Some(std::sync::Arc::new(gpui::Image::from_bytes(
                                gpui::ImageFormat::Png,
                                bytes,
                            )));
                            cx.emit(BrowserEvent::Changed);
                            cx.notify();
                        }
                    });
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn address_enter_rejects_unsupported_schemes_and_escape_restores_url(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::composer::init(cx);
            cx.set_global(crate::theme::Theme::default());
            bind_keys(cx, &crate::settings::KeymapConfig::default());
        });
        let window = cx.add_window(|window, cx| {
            BrowserSurface::new(BrowserContext::default(), false, window, cx)
        });
        window
            .update(cx, |browser, window, cx| {
                browser.page.url = Some("https://example.com/".into());
                browser.address.update(cx, |input, cx| input.set_text("javascript:alert(1)", cx));
                browser.focus_address(window, cx);
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "enter");
        window
            .update(cx, |browser, _, _| {
                assert!(
                    browser.validation.is_some(),
                    "Enter did not reach browser address handling"
                );
                assert_eq!(browser.page.url.as_deref(), Some("https://example.com/"));
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "escape");
        window
            .update(cx, |browser, window, cx| {
                assert!(browser.validation.is_none());
                assert_eq!(browser.address.read(cx).text(), "https://example.com/");
                assert!(browser.focus.is_focused(window));
                window.blur(cx);
            })
            .unwrap();
    }

    #[gpui::test]
    fn customized_app_shortcuts_win_over_browser_defaults(cx: &gpui::TestAppContext) {
        cx.update(|cx| {
            let mut config = crate::settings::KeymapConfig::default();
            bind_keys(cx, &config);
            assert_eq!(cx.key_bindings().borrow().bindings_for_action(&FocusAddress).count(), 1);
            assert_eq!(cx.key_bindings().borrow().bindings_for_action(&Reload).count(), 1);
            cx.clear_key_bindings();
            config.toggle_sidebar = crate::settings::platform_combo("mod-l");
            config.toggle_changes = "mod-shift-r".into();
            bind_keys(cx, &config);
            assert_eq!(cx.key_bindings().borrow().bindings_for_action(&FocusAddress).count(), 0);
            assert_eq!(cx.key_bindings().borrow().bindings_for_action(&Reload).count(), 0);
            assert_eq!(cx.key_bindings().borrow().bindings_for_action(&NewTab).count(), 1);
        });
    }
}

/// The most a favicon message may carry before it is even parsed.
#[cfg(any(target_os = "macos", test))]
const FAVICON_MESSAGE_MAX: usize = 512 * 1024;
/// The side a page's icon is kept at (the pane header draws it at 13 pt).
#[cfg(any(target_os = "macos", test))]
const FAVICON_SIDE: u32 = 32;

/// The page and icon a `FAVICON_SCRIPT` message names; any other message, or one too
/// large, is `None`.
#[cfg(any(target_os = "macos", test))]
fn favicon_message(body: &str) -> Option<(String, String)> {
    if body.len() > FAVICON_MESSAGE_MAX {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let page = value.get("sloptyFavicon")?.as_str()?.to_owned();
    let data = value.get("data")?.as_str()?.to_owned();
    Some((page, data))
}

/// A page's icon as the PNG the tab chip shows, from a base64 `data:` URL: the PNG the
/// page's canvas makes or, where the canvas cannot be read back, the icon's own file, read by
/// what its bytes are (PNG, ICO, JPEG, GIF, WebP, BMP), never by the type it claims. At most 256
/// px a side, scaled down to [`FAVICON_SIDE`]. Anything else (an SVG, a link) is `None` and the
/// tab keeps the default icon.
#[cfg(any(target_os = "macos", test))]
fn favicon_png(data: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let (_, encoded) = data.strip_prefix("data:")?.split_once(";base64,")?;
    if encoded.len() > FAVICON_MESSAGE_MAX {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let mut reader =
        image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    if !matches!(
        reader.format(),
        Some(
            image::ImageFormat::Png
                | image::ImageFormat::Ico
                | image::ImageFormat::Jpeg
                | image::ImageFormat::Gif
                | image::ImageFormat::WebP
                | image::ImageFormat::Bmp
        )
    ) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(256);
    limits.max_image_height = Some(256);
    limits.max_alloc = Some(1024 * 1024);
    reader.limits(limits);
    let icon = reader.decode().ok()?;
    let icon = if icon.width() > FAVICON_SIDE || icon.height() > FAVICON_SIDE {
        icon.thumbnail(FAVICON_SIDE, FAVICON_SIDE)
    } else {
        icon
    };
    let mut png = std::io::Cursor::new(Vec::new());
    icon.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

// The page's icon is taken only as a small picture, kept as a PNG, and anything else
// leaves the globe.
#[cfg(test)]
mod favicon_tests {
    use base64::Engine as _;

    use super::{FAVICON_SIDE, favicon_message, favicon_png};

    fn png_url(side: u32) -> String {
        picture_url("image/png", side, image::ImageFormat::Png)
    }

    /// A `side` px picture in `format`, as a `data:` URL that says it is `mime`.
    fn picture_url(mime: &str, side: u32, format: image::ImageFormat) -> String {
        let image = image::RgbaImage::from_pixel(side, side, image::Rgba([200, 30, 30, 255]));
        let mut file = std::io::Cursor::new(Vec::new());
        image.write_to(&mut file, format).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(file.into_inner());
        format!("data:{mime};base64,{encoded}")
    }

    fn side_of(bytes: &[u8]) -> (u32, u32) {
        let icon = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap();
        (icon.width(), icon.height())
    }

    #[test]
    fn a_pages_icon_is_kept_small() {
        let large = favicon_png(&png_url(64)).expect("a 64 px canvas");
        assert_eq!(side_of(&large), (FAVICON_SIDE, FAVICON_SIDE));
        let small = favicon_png(&png_url(16)).expect("a 16 px icon");
        assert_eq!(side_of(&small), (16, 16), "never scaled up");
    }

    /// Where the page's canvas cannot be read back, the icon's own file comes, read by what its
    /// bytes are.
    #[test]
    fn an_icons_own_file_is_read_by_what_it_is() {
        let ico = favicon_png(&picture_url("image/x-icon", 48, image::ImageFormat::Ico));
        assert_eq!(side_of(&ico.expect("an ICO")), (FAVICON_SIDE, FAVICON_SIDE));
        let named_wrong = picture_url("application/octet-stream", 16, image::ImageFormat::Bmp);
        assert_eq!(side_of(&favicon_png(&named_wrong).expect("a BMP")), (16, 16));
        assert!(favicon_png("data:,").is_none(), "a canvas that could not be read");
    }

    #[test]
    fn anything_but_a_small_picture_leaves_the_default_icon() {
        assert!(favicon_png("https://example.com/favicon.ico").is_none(), "a link, not data");
        assert!(favicon_png("data:image/svg+xml;base64,PHN2Zz4=").is_none(), "an SVG's own file");
        assert!(favicon_png("data:image/png;base64,!!!").is_none(), "not base64");
        let not_png = base64::engine::general_purpose::STANDARD.encode(b"GIF89a....");
        assert!(favicon_png(&format!("data:image/png;base64,{not_png}")).is_none(), "cut short");
        assert!(favicon_png(&png_url(300)).is_none(), "past 256 px a side");
        let huge = format!("data:image/png;base64,{}", "A".repeat(600 * 1024));
        assert!(favicon_png(&huge).is_none(), "past the byte cap");
    }

    #[test]
    fn only_the_favicon_message_is_taken() {
        let body =
            r#"{"sloptyFavicon":"http://localhost:5173/","data":"data:image/png;base64,AA"}"#;
        assert_eq!(
            favicon_message(body),
            Some(("http://localhost:5173/".into(), "data:image/png;base64,AA".into()))
        );
        assert_eq!(favicon_message(r#"{"data":"x"}"#), None, "another page message");
        assert_eq!(favicon_message("not json"), None);
        assert_eq!(favicon_message(r#"{"sloptyFavicon":3,"data":"x"}"#), None);
        let huge = format!(r#"{{"sloptyFavicon":"p","data":"{}"}}"#, "A".repeat(600 * 1024));
        assert_eq!(favicon_message(&huge), None, "past the message cap");
    }
}
