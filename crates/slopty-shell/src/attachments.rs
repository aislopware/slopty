// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Attachments: the pictures a transcript's message carries, read back from the machine that
//! holds them, their decoded-image cache and the full-size preview lightbox.
//!
//! A message's pictures ride its plain text as a trailer of paths, which the view model writes
//! ([`parse_user_message_images`]); the decoded images are cached by `(deviceId, path)`.
//!
//! TUI-first, nothing is staged or uploaded here: the GUI sends the agent nothing.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use gpui::prelude::*;
use gpui::{AnyElement, BackgroundExecutor, Image, ImageFormat, SharedString, Size, div, px};
use slopty_view_rpc::methods;

use crate::state::EngineHandle;
use crate::theme::ink;

/// The largest attachment read back.
pub const MAX_ATTACHMENT_BYTES: u64 = 24 * 1024 * 1024;
/// The most chunks one read-back takes, which bounds its loop.
const MAX_READ_CHUNKS: usize = 1_000;
/// How much of a file's first chunk a head reader sees, as base64 (64 KiB of bytes).
const HEAD_B64: usize = 64 * 1024 / 3 * 4;

// ---------------------------------------------------------------------------
// Text transport
// ---------------------------------------------------------------------------

/// Whether an attachment ref names an image, as opposed to any other file.
pub fn is_image_path(path: &str) -> bool {
    format_by_extension(Path::new(path)).is_some()
}

/// Image formats the whole pipeline supports: intersection of gpui's decoders
/// and the engine's `mime_by_ext` read-back jail.
fn format_by_extension(path: &Path) -> Option<ImageFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        "tif" | "tiff" => Some(ImageFormat::Tiff),
        _ => None,
    }
}

/// `text` with `paths` in the trailer the view model writes for a message's pictures
/// (`slopty_ui`'s `with_images`).
#[cfg(test)]
pub(crate) fn with_attachments(text: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return text.to_string();
    }
    let refs: Vec<String> = paths.iter().map(|p| format!("- {p}")).collect();
    format!("{text}\n\nAttached images (local files — open them to view):\n{}", refs.join("\n"))
}

/// An attachment ref parsed back out of a user message's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserImageAttachment {
    pub id: String,
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUserMessage {
    /// The visible words (the refs trailer stripped; empty when a message is only pictures).
    pub text: String,
    pub attachments: Vec<UserImageAttachment>,
}

fn name_from_path(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().map(str::trim).unwrap_or_default();
    if name.is_empty() { "image".to_string() } else { name.to_string() }
}

/// Find the refs trailer: a blank line, then a line starting (case-insensitive)
/// with `Attached images (local files` and ending `):`. Returns
/// `(body_end, refs_start)` byte offsets.
fn find_refs_marker(content: &str) -> Option<(usize, usize)> {
    let lower = content.to_ascii_lowercase();
    let needle = "\n\nattached images (local files";
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(needle) {
        let gap = from + rel;
        let line_start = gap + 2;
        let line_end =
            content[line_start..].find('\n').map(|p| line_start + p).unwrap_or(content.len());
        let line = content[line_start..line_end].trim_end_matches('\r');
        if line.ends_with("):") {
            let refs_start = (line_end + 1).min(content.len());
            return Some((gap, refs_start));
        }
        from = line_start;
    }
    None
}

/// Split the visible prompt from its attachment-ref trailer.
pub fn parse_user_message_images(content: &str) -> ParsedUserMessage {
    let Some((body_end, refs_start)) = find_refs_marker(content) else {
        return ParsedUserMessage { text: content.to_string(), attachments: Vec::new() };
    };
    let body = content[..body_end].trim_end();
    let attachments: Vec<UserImageAttachment> = content[refs_start..]
        .lines()
        .filter_map(|line| {
            let path = line.trim_start().strip_prefix("- ")?.trim();
            (!path.is_empty()).then(|| path.to_string())
        })
        .enumerate()
        .map(|(index, path)| UserImageAttachment {
            id: format!("{index}:{path}"),
            name: name_from_path(&path),
            path,
        })
        .collect();
    if attachments.is_empty() {
        return ParsedUserMessage { text: content.to_string(), attachments };
    }
    ParsedUserMessage { text: body.to_string(), attachments }
}

// ---------------------------------------------------------------------------
// Read-back
// ---------------------------------------------------------------------------

fn with_target(mut params: serde_json::Value, target_device_id: Option<&str>) -> serde_json::Value {
    if let (Some(target), Some(map)) = (target_device_id, params.as_object_mut()) {
        map.insert("targetDeviceId".into(), target.into());
    }
    params
}

/// Per-read deadline: a stalled but open link to the machine never fails a call on its own, so
/// every read races a timer.
const READ_CHUNK_TIMEOUT: Duration = Duration::from_secs(20);

/// Race an RPC against `timeout` on the gpui background executor (these
/// futures run under `cx.spawn`, so tokio's timer reactor isn't available).
pub(crate) async fn call_with_timeout(
    engine: &EngineHandle,
    executor: &BackgroundExecutor,
    method: &str,
    params: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let call = engine.client().call(method, params);
    let timer = executor.timer(timeout);
    futures::pin_mut!(call);
    match futures::future::select(call, timer).await {
        futures::future::Either::Left((result, _)) => result.map_err(|e| e.to_string()),
        futures::future::Either::Right(_) => Err(format!("{method} timed out")),
    }
}

/// A transcript image read back from the owning device.
pub struct LoadedAttachmentImage {
    pub name: String,
    pub image: Arc<Image>,
}

/// `ReadAttachmentChunk` loop: 45KB base64 chunks until `done` (bounded, with
/// a guard against an offset that stops moving). Returns the
/// file's name, MIME type and bytes.
pub(crate) async fn read_attachment_bytes(
    engine: &EngineHandle,
    executor: &BackgroundExecutor,
    target_device_id: Option<&str>,
    path: &str,
    expected_raster_mime: Option<&str>,
) -> Option<(String, String, Vec<u8>)> {
    read_attachment_bytes_with(
        engine,
        executor,
        target_device_id,
        path,
        expected_raster_mime,
        |_| {},
    )
    .await
}

/// [`read_attachment_bytes`], handing `head` the file's first bytes as soon as its first
/// chunk is in (a preview reads its size from them while the rest arrives).
pub(crate) async fn read_attachment_bytes_with(
    engine: &EngineHandle,
    executor: &BackgroundExecutor,
    target_device_id: Option<&str>,
    path: &str,
    expected_raster_mime: Option<&str>,
    mut head: impl FnMut(&[u8]),
) -> Option<(String, String, Vec<u8>)> {
    let mut name = String::new();
    let mut mime = String::new();
    let mut b64 = String::new();
    let mut offset = 0u64;
    let mut done = false;
    for _ in 0..MAX_READ_CHUNKS {
        let params =
            with_target(serde_json::json!({ "path": path, "offset": offset }), target_device_id);
        let chunk = call_with_timeout(
            engine,
            executor,
            methods::READ_ATTACHMENT_CHUNK,
            params,
            READ_CHUNK_TIMEOUT,
        )
        .await
        .ok()?;
        name = chunk.get("name")?.as_str()?.to_string();
        mime = chunk.get("mimeType")?.as_str()?.to_string();
        if expected_raster_mime.is_some_and(|expected| expected != mime) {
            return None;
        }
        let data = chunk.get("data")?.as_str()?;
        if b64.len().saturating_add(data.len()) > (MAX_ATTACHMENT_BYTES as usize).div_ceil(3) * 4 {
            return None;
        }
        if offset == 0 {
            // At most 64 KiB of the first chunk, cut on a base64 quantum.
            let cut = data.len().min(HEAD_B64) / 4 * 4;
            if let Ok(first) = BASE64.decode(&data.as_bytes()[..cut]) {
                head(&first);
            }
        }
        b64.push_str(data);
        done = chunk.get("done")?.as_bool()?;
        if done {
            break;
        }
        let next = chunk.get("nextOffset")?.as_u64()?;
        if next <= offset {
            return None;
        }
        offset = next;
    }
    if !done {
        return None;
    }
    // Callers await this on the UI thread; decode the (up to 24 MiB) payload
    // off it.
    let bytes = executor.spawn(async move { BASE64.decode(b64.as_bytes()).ok() }).await?;
    Some((name, mime, bytes))
}

pub async fn read_attachment_image(
    engine: &EngineHandle,
    executor: &BackgroundExecutor,
    target_device_id: Option<&str>,
    path: &str,
    expected_raster_mime: Option<&str>,
) -> Option<LoadedAttachmentImage> {
    let (name, mime, bytes) =
        read_attachment_bytes(engine, executor, target_device_id, path, expected_raster_mime)
            .await?;
    if bytes.is_empty() {
        return None;
    }
    let image = if let Some(expected) = expected_raster_mime {
        if expected != mime
            || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/gif")
        {
            return None;
        }
        let expected = expected.to_owned();
        executor
            .spawn(async move {
                crate::image_media::decode_generated_image(
                    bytes,
                    &expected,
                    MAX_ATTACHMENT_BYTES as usize,
                )
                .ok()
                .map(|media| media.image)
            })
            .await?
    } else {
        let format = ImageFormat::from_mime_type(&mime).unwrap_or(ImageFormat::Png);
        Arc::new(Image::from_bytes(format, bytes))
    };
    Some(LoadedAttachmentImage {
        name: if name.is_empty() { name_from_path(path) } else { name },
        image,
    })
}

// ---------------------------------------------------------------------------
// Transcript image cache
// ---------------------------------------------------------------------------

/// A decoded transcript image, ready for `img(...)`.
#[derive(Clone)]
pub struct CachedAttachmentImage {
    pub name: SharedString,
    pub image: Arc<Image>,
}

/// What a render pass sees for one `(deviceId, path)` source.
#[derive(Clone)]
pub enum AttachmentSnapshot {
    Loading,
    Loaded(CachedAttachmentImage),
    /// Load failed; `retry_in` is how long until `begin_load_for` would hand out
    /// another attempt (an exponential ladder from 2 s to 15 s).
    Error {
        retry_in: Duration,
    },
}

enum CacheEntry {
    Loading { attempts: u32 },
    Loaded { image: CachedAttachmentImage, bytes: usize, last_used: u64 },
    Error { attempts: u32, at: Instant },
}

fn retry_delay(attempts: u32) -> Duration {
    Duration::from_millis((2_000u64 << attempts.min(3)).min(15_000))
}

/// Retained encoded bytes plus estimated CPU/GPU pixels for normalized PNG images.
/// Generated images are always evictable and individually fit this budget.
/// Legacy user attachments retain their existing protection behavior.
const IMAGE_CACHE_BUDGET_BYTES: usize = 64 * 1024 * 1024;

/// Validation policy is part of identity, including in-flight loads and errors.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct AttachmentKey {
    device: String,
    path: String,
    raster_mime: Option<String>,
}

impl AttachmentKey {
    pub(crate) fn new(device: &str, path: &str, mime: Option<&str>) -> Self {
        Self { device: device.into(), path: path.into(), raster_mime: mime.map(str::to_owned) }
    }
}

#[derive(Default)]
struct ImageCache {
    map: HashMap<AttachmentKey, CacheEntry>,
    /// Monotonic access clock for LRU ordering.
    tick: u64,
    loaded_bytes: usize,
    generated_bytes: usize,
    /// Evicted images awaiting `flush_evicted` (freeing needs `&mut App`,
    /// which eviction sites — async load completions — don't always have).
    pending_free: Vec<Arc<Image>>,
}

pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

impl ImageCache {
    fn insert_loaded(&mut self, key: AttachmentKey, image: CachedAttachmentImage) {
        // Generated rasters are normalized to PNG; account for their decoded
        // CPU/GPU copies as well as encoded bytes in the existing cache budget.
        let pixels = png_dimensions(&image.image.bytes)
            .map_or(0, |(w, h)| (w as usize).saturating_mul(h as usize));
        let bytes = image.image.bytes.len().saturating_add(pixels.saturating_mul(8));
        let generated = key.raster_mime.is_some();
        self.tick += 1;
        if let Some(CacheEntry::Loaded { image, bytes, .. }) =
            self.map.insert(key.clone(), CacheEntry::Loaded { image, bytes, last_used: self.tick })
        {
            self.loaded_bytes = self.loaded_bytes.saturating_sub(bytes);
            if generated {
                self.generated_bytes = self.generated_bytes.saturating_sub(bytes);
            }
            self.pending_free.push(image.image);
        }
        self.loaded_bytes = self.loaded_bytes.saturating_add(bytes);
        if generated {
            self.generated_bytes = self.generated_bytes.saturating_add(bytes);
        }
        let shielded = protected().lock().unwrap().clone();
        // Separate budgets prevent protected legacy attachments from repeatedly
        // evicting visible generated previews, or vice versa.
        while (if generated {
            self.generated_bytes
        } else {
            self.loaded_bytes.saturating_sub(self.generated_bytes)
        }) > IMAGE_CACHE_BUDGET_BYTES
        {
            let oldest = self
                .map
                .iter()
                .filter(|(k, _)| {
                    **k != key
                        && k.raster_mime.is_some() == generated
                        && (k.raster_mime.is_some()
                            || !shielded.contains(&(k.device.clone(), k.path.clone())))
                })
                .filter_map(|(k, e)| match e {
                    CacheEntry::Loaded { last_used, .. } => Some((*last_used, k.clone())),
                    _ => None,
                })
                .min_by_key(|(tick, _)| *tick);
            let Some((_, evict_key)) = oldest else { break };
            if let Some(CacheEntry::Loaded { image, bytes, .. }) = self.map.remove(&evict_key) {
                self.loaded_bytes = self.loaded_bytes.saturating_sub(bytes);
                if generated {
                    self.generated_bytes = self.generated_bytes.saturating_sub(bytes);
                }
                self.pending_free.push(image.image);
            }
        }
    }
}

fn cache() -> &'static Mutex<ImageCache> {
    static CACHE: OnceLock<Mutex<ImageCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ImageCache::default()))
}

/// Empty the image cache and its eviction shield, as a new app starts with neither. Both
/// are the process's, which keeps them across a `#[gpui::test]`'s seeds: a load the last seed
/// claimed read as this one's (the nightly's GPUI seeds, 2026-10-29).
#[cfg(test)]
pub(crate) fn forget_for_test() {
    *cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner) = ImageCache::default();
    protected().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
}

/// Keys shielded from LRU eviction — the open transcript's attachments. The
/// gpui list caches rendered rows across frames, so a VISIBLE thumbnail's
/// `last_used` tick can go stale and budget pressure evicted images still on
/// screen (user report: "images unload before they are scrolled out of
/// view"). The transcript replaces this set on every row sync; other chats'
/// images stay evictable, so the budget still bounds the cache overall.
fn protected() -> &'static Mutex<std::collections::HashSet<(String, String)>> {
    static PROTECTED: OnceLock<Mutex<std::collections::HashSet<(String, String)>>> =
        OnceLock::new();
    PROTECTED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Replace the eviction shield with the given keys (see [`protected`]).
pub fn protect_attachments(keys: std::collections::HashSet<(String, String)>) {
    *protected().lock().unwrap() = keys;
}

pub(crate) fn attachment_snapshot_for(source: &AttachmentKey) -> AttachmentSnapshot {
    let mut cache = cache().lock().unwrap();
    let tick = {
        cache.tick += 1;
        cache.tick
    };
    match cache.map.get_mut(source) {
        Some(CacheEntry::Loaded { image, last_used, .. }) => {
            *last_used = tick;
            AttachmentSnapshot::Loaded(image.clone())
        }
        Some(CacheEntry::Error { attempts, at }) => AttachmentSnapshot::Error {
            retry_in: retry_delay(attempts.saturating_sub(1))
                .saturating_sub(crate::clock::spent(*at)),
        },
        Some(CacheEntry::Loading { .. }) | None => AttachmentSnapshot::Loading,
    }
}

/// Release gpui's decoded copies of evicted images: the asset-system entry
/// AND the sprite-atlas tiles (`ImageSource::evict` — `remove_asset` alone
/// left the tiles resident forever). Pass the window being updated when
/// calling from a render path, since that window is detached from
/// `App::windows` during its own update. Cheap when nothing was evicted.
pub fn flush_evicted(mut window: Option<&mut gpui::Window>, cx: &mut gpui::App) {
    let evicted = std::mem::take(&mut cache().lock().unwrap().pending_free);
    for image in evicted {
        gpui::ImageSource::Image(image).evict(window.as_deref_mut(), cx);
    }
}

/// Claim the load for a source: `true` ⇒ the caller should start fetching now
/// (the entry is marked Loading so concurrent renders don't double-fetch).
/// Errored sources hand out a retry only after their backoff has elapsed.
pub(crate) fn begin_load_for(source: &AttachmentKey) -> bool {
    let mut cache = cache().lock().unwrap();
    let entry = cache.map.entry(source.clone());
    match entry {
        std::collections::hash_map::Entry::Vacant(v) => {
            v.insert(CacheEntry::Loading { attempts: 0 });
            true
        }
        std::collections::hash_map::Entry::Occupied(mut o) => match o.get() {
            CacheEntry::Error { attempts, at }
                if crate::clock::spent(*at) >= retry_delay(attempts.saturating_sub(1)) =>
            {
                let attempts = *attempts;
                o.insert(CacheEntry::Loading { attempts });
                true
            }
            _ => false,
        },
    }
}

/// A cancelled view/task must release its claim so reopening can retry it.
/// Completed loads are left alone, including completions waiting on a UI notify.
pub(crate) struct AttachmentLoadGuard(pub AttachmentKey);

impl Drop for AttachmentLoadGuard {
    fn drop(&mut self) {
        let mut cache = cache().lock().unwrap();
        if let Some(entry @ CacheEntry::Loading { .. }) = cache.map.get_mut(&self.0) {
            let CacheEntry::Loading { attempts } = entry else { unreachable!() };
            *entry = CacheEntry::Error {
                attempts: attempts.saturating_add(1),
                at: crate::clock::stopwatch(), // a retry's wait, not a moment drawn
            };
        }
    }
}

pub(crate) fn store_loaded_for(source: &AttachmentKey, name: SharedString, image: Arc<Image>) {
    cache().lock().unwrap().insert_loaded(source.clone(), CachedAttachmentImage { name, image });
}

pub(crate) fn store_error_for(source: &AttachmentKey) {
    let mut cache = cache().lock().unwrap();
    let attempts = match cache.map.get(source) {
        Some(CacheEntry::Loading { attempts }) => attempts + 1,
        Some(CacheEntry::Error { attempts, .. }) => *attempts,
        _ => 1,
    };
    cache.map.insert(
        source.clone(),
        CacheEntry::Error {
            attempts,
            at: crate::clock::stopwatch(), // a retry's wait, not a moment drawn
        },
    );
}

// ---------------------------------------------------------------------------
// Preview lightbox
// ---------------------------------------------------------------------------

/// A full-size preview target (staged strip or transcript thumbnail).
#[derive(Clone)]
pub struct PreviewImage {
    pub name: SharedString,
    pub image: Arc<Image>,
    pub(crate) viewer: crate::image_viewer::ImageView,
    /// Fill behind an image drawn on a transparent canvas (diagrams).
    pub(crate) plate: Option<gpui::Hsla>,
}

impl PreviewImage {
    pub fn new(name: impl Into<SharedString>, image: Arc<Image>) -> Self {
        Self { name: name.into(), image, viewer: Default::default(), plate: None }
    }

    pub(crate) fn with_plate(mut self, plate: gpui::Hsla) -> Self {
        self.plate = Some(plate);
        self
    }
}

/// Shared image viewer over a dim scrim. Escape and a click close it;
/// dragging and zoom controls are consumed by the viewer.
pub fn lightbox(
    window: &mut gpui::Window,
    preview: &PreviewImage,
    focus: &gpui::FocusHandle,
    on_close: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
    cx: &mut gpui::App,
) -> AnyElement {
    lightbox_with_size(window, preview, focus, None, on_close, cx)
}

/// Sanitized SVG variants retain their source's natural logical dimensions.
pub(crate) fn lightbox_with_size(
    window: &mut gpui::Window,
    preview: &PreviewImage,
    focus: &gpui::FocusHandle,
    natural_size: Option<Size<gpui::Pixels>>,
    on_close: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
    cx: &mut gpui::App,
) -> AnyElement {
    let viewport = window.viewport_size();
    let max_h = px(f32::from(viewport.height) * 0.85);
    let max_w = px(f32::from(viewport.width) * 0.9);
    let natural_size = natural_size.or_else(|| {
        preview.image.clone().use_render_image(window, cx).map(|image| {
            let dimensions = image.size(0);
            gpui::size(px(dimensions.width.0 as f32), px(dimensions.height.0 as f32))
        })
    });
    let content = match natural_size {
        Some(natural) => {
            preview.viewer.render(preview.image.clone(), natural, None, preview.plate, window, cx)
        }
        // The house's pending line, shown only past its grace.
        None => {
            crate::loaders::pending("attachment-preview-load", "Loading image…").into_any_element()
        }
    };
    // The caption goes under the picture as it is fitted (the viewer's own fit, never
    // past its natural size), not under the room it may be zoomed in, which a wide picture
    // leaves far below it.
    let fitted_h = natural_size.map_or(max_h, |natural| {
        let (w, h) = (f32::from(natural.width), f32::from(natural.height));
        if w > 0.0 && h > 0.0 {
            px(h * (f32::from(max_w) / w).min(f32::from(max_h) / h).min(1.0))
        } else {
            max_h
        }
    });
    let caption_top = (viewport.height + fitted_h) / 2.0 + px(12.0);
    let on_close = std::rc::Rc::new(on_close);
    let close_on_key = on_close.clone();
    let close_on_button = on_close.clone();
    let press_state = preview.viewer.clone();
    let click_state = preview.viewer.clone();
    // A close a screen reader and a finger reach (a phone has no Escape), clear of the
    // system's edges.
    let safe = window.insets().safe_area;
    let close = div()
        .id("attachment-lightbox-close")
        .debug_selector(|| "attachment-lightbox-close".into())
        .role(gpui::Role::Button)
        .aria_label("Close")
        .absolute()
        .top(safe.top + px(12.0))
        .right(safe.right + px(12.0))
        .size(px(32.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(ink(0.10))
        .hover(|el| el.bg(ink(0.18)))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            close_on_button(window, cx);
        })
        .child(crate::icons::icon(crate::icons::CLOSE).size(px(16.0)).text_color(ink(0.8)));
    gpui::deferred(
        gpui::anchored().position(gpui::point(px(0.0), px(0.0))).child(
            div()
                .id("attachment-lightbox")
                // A dialog named for its image, which VoiceOver can reach and close.
                .role(gpui::Role::Dialog)
                .aria_label(preview.name.clone())
                .occlude()
                .track_focus(focus)
                .relative()
                .w(viewport.width)
                .h(viewport.height)
                .bg(crate::popover::scrim_alpha(0.7))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        cx.stop_propagation();
                        close_on_key(window, cx);
                    }
                })
                .capture_any_mouse_down(move |event, _, _| {
                    if event.button == gpui::MouseButton::Left {
                        press_state.begin_click();
                    }
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    if !click_state.dragged() {
                        on_close(window, cx);
                    }
                })
                .on_scroll(|_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("attachment-lightbox-image")
                        .role(gpui::Role::Image)
                        .aria_label(preview.name.clone())
                        .w(max_w)
                        .h(max_h)
                        .child(content),
                )
                .child(
                    div()
                        .absolute()
                        .top(caption_top)
                        .left_0()
                        .w(viewport.width)
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .debug_selector(|| "attachment-lightbox-caption".into())
                                .max_w(max_w)
                                .overflow_hidden()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(ink(0.45))
                                .child(preview.name.clone()),
                        ),
                )
                .child(close),
        ),
    )
    .priority(3)
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Shown {
        preview: PreviewImage,
        focus: gpui::FocusHandle,
        closed: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl gpui::Render for Shown {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let closed = self.closed.clone();
            div().size_full().child(lightbox(
                window,
                &self.preview,
                &self.focus,
                move |_, _| closed.set(true),
                cx,
            ))
        }
    }

    /// The lightbox is a dialog named for its image, with a Close a screen reader and a
    /// finger reach, which closes it.
    #[gpui::test]
    fn the_lightbox_is_a_dialog_that_closes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
        });
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let (_, cx) = cx.add_window_view({
            let closed = closed.clone();
            move |_, cx| {
                let png = {
                    let mut bytes = std::io::Cursor::new(Vec::new());
                    image::RgbImage::new(4, 4)
                        .write_to(&mut bytes, image::ImageFormat::Png)
                        .unwrap();
                    Image::from_bytes(ImageFormat::Png, bytes.into_inner())
                };
                Shown {
                    preview: PreviewImage::new("chart.png", Arc::new(png)),
                    focus: cx.focus_handle(),
                    closed,
                }
            }
        });
        let nodes = cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            let tree = window.a11y_tree().expect("a tree");
            tree.nodes
                .iter()
                .map(|(_, node)| (node.role(), node.label().unwrap_or_default().to_owned()))
                .collect::<Vec<_>>()
        });
        assert!(nodes.contains(&(gpui::Role::Dialog, "chart.png".to_owned())), "{nodes:?}");
        assert!(nodes.contains(&(gpui::Role::Image, "chart.png".to_owned())), "{nodes:?}");
        assert!(nodes.contains(&(gpui::Role::Button, "Close".to_owned())), "{nodes:?}");
        let at = cx.debug_bounds("attachment-lightbox-close").expect("the close").center();
        cx.simulate_click(at, gpui::Modifiers::default());
        assert!(closed.get(), "Close closes it");
    }

    /// A wide picture's name sits just under it, not under the room it may be zoomed in.
    #[gpui::test]
    fn the_lightbox_names_its_picture_just_under_it(cx: &mut gpui::TestAppContext) {
        struct Sized(PreviewImage, gpui::FocusHandle);
        impl gpui::Render for Sized {
            fn render(
                &mut self,
                window: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                let natural = Some(gpui::size(px(640.0), px(400.0)));
                div().size_full().child(lightbox_with_size(
                    window,
                    &self.0,
                    &self.1,
                    natural,
                    |_, _| {},
                    cx,
                ))
            }
        }
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::dark());
        });
        let (_, cx) = cx.add_window_view(|_, cx| {
            let png = Image::from_bytes(ImageFormat::Png, Vec::new());
            Sized(PreviewImage::new("plot.png", Arc::new(png)), cx.focus_handle())
        });
        cx.simulate_resize(gpui::size(px(1200.0), px(900.0)));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        // 640x400 fits 1080x765 at its own size: centred, its bottom at 650.
        let caption = cx.debug_bounds("attachment-lightbox-caption").expect("the caption");
        assert_eq!(caption.origin.y, px(662.0), "12 pt under the picture: {caption:?}");
        assert!((caption.center().x - px(600.0)).abs() < px(1.0), "centred: {caption:?}");
    }

    #[test]
    fn with_attachments_round_trips_through_parse() {
        let paths = vec!["/data/uploads/ab-cat.png".to_string(), "/x/dog.jpg".into()];
        let content = with_attachments("look at these", &paths);
        let parsed = parse_user_message_images(&content);
        assert_eq!(parsed.text, "look at these");
        assert_eq!(parsed.attachments.len(), 2);
        assert_eq!(parsed.attachments[0].path, "/data/uploads/ab-cat.png");
        assert_eq!(parsed.attachments[0].name, "ab-cat.png");
        assert_eq!(parsed.attachments[1].name, "dog.jpg");
        assert_eq!(parsed.attachments[0].id, "0:/data/uploads/ab-cat.png");
    }

    #[test]
    fn a_message_of_only_pictures_has_no_words() {
        let content = with_attachments("", &["/a/b.png".to_string()]);
        let parsed = parse_user_message_images(&content);
        assert_eq!(parsed.text, "");
        assert_eq!(parsed.attachments.len(), 1);
    }

    #[test]
    fn plain_text_passes_through_unchanged() {
        assert_eq!(with_attachments("hello", &[]), "hello");
        let parsed = parse_user_message_images("hello\n\nno images here");
        assert!(parsed.attachments.is_empty());
        assert_eq!(parsed.text, "hello\n\nno images here");
    }

    #[test]
    fn marker_is_case_insensitive_and_requires_ref_lines() {
        let parsed = parse_user_message_images(
            "hi\n\nATTACHED IMAGES (local files — open them to view):\n- /p/q.png",
        );
        assert_eq!(parsed.attachments.len(), 1);
        // A trailer with no valid `- path` lines is left as plain text.
        let empty = parse_user_message_images(
            "hi\n\nAttached images (local files — open them to view):\nnothing",
        );
        assert!(empty.attachments.is_empty());
        assert!(empty.text.contains("Attached images"));
    }

    #[test]
    fn supported_formats_match_engine_jail() {
        for (ext, expect) in [
            ("png", Some(ImageFormat::Png)),
            ("JPG", Some(ImageFormat::Jpeg)),
            ("webp", Some(ImageFormat::Webp)),
            ("svg", Some(ImageFormat::Svg)),
            ("ico", None),
            ("txt", None),
        ] {
            assert_eq!(format_by_extension(Path::new(&format!("f.{ext}"))), expect, "ext {ext}");
        }
    }

    #[test]
    fn retry_ladder_is_2s_doubling_capped_at_15s() {
        assert_eq!(retry_delay(0), Duration::from_millis(2_000));
        assert_eq!(retry_delay(1), Duration::from_millis(4_000));
        assert_eq!(retry_delay(2), Duration::from_millis(8_000));
        assert_eq!(retry_delay(3), Duration::from_millis(15_000));
        assert_eq!(retry_delay(9), Duration::from_millis(15_000));
    }
}

#[cfg(test)]
mod generated_image_tests {
    use super::*;

    #[test]
    fn generated_image_cache_isolates_policy_aliases_and_load_claims() {
        let owner = "policy-audit-owner";
        let path = "/uploads/policy01-image.png";
        let png = AttachmentKey::new(owner, path, Some("image/png"));
        let gif = AttachmentKey::new(owner, path, Some("image/gif"));
        let raw = Arc::new(Image::from_bytes(ImageFormat::Gif, b"GIF89a".to_vec()));
        store_loaded_for(&AttachmentKey::new(owner, path, None), "image.gif".into(), raw.clone());
        assert!(matches!(attachment_snapshot_for(&png), AttachmentSnapshot::Loading));
        assert!(begin_load_for(&png));
        assert!(!begin_load_for(&png));
        assert!(begin_load_for(&gif));
        drop(AttachmentLoadGuard(png.clone()));
        assert!(matches!(attachment_snapshot_for(&png), AttachmentSnapshot::Error { .. }));
        assert!(matches!(attachment_snapshot_for(&gif), AttachmentSnapshot::Loading));
        store_loaded_for(&gif, "image.gif".into(), raw);
        assert!(matches!(attachment_snapshot_for(&png), AttachmentSnapshot::Error { .. }));
        assert!(matches!(attachment_snapshot_for(&gif), AttachmentSnapshot::Loaded(_)));
        let changed = AttachmentKey::new(owner, path, Some("image/jpeg"));
        assert!(matches!(attachment_snapshot_for(&changed), AttachmentSnapshot::Loading));
    }

    #[test]
    fn generated_image_history_is_evicted_with_decoded_memory_accounting() {
        let mut cache = ImageCache::default();
        // Cache accounting reads only IHDR. No large allocations are needed
        // to exercise the production eviction policy across a long history.
        for i in 0..100 {
            let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            bytes.extend_from_slice(&2048_u32.to_be_bytes());
            bytes.extend_from_slice(&2048_u32.to_be_bytes());
            cache.insert_loaded(
                AttachmentKey::new("bounded-history", &format!("/{i}.png"), Some("image/png")),
                CachedAttachmentImage {
                    name: "generated.png".into(),
                    image: Arc::new(Image::from_bytes(ImageFormat::Png, bytes)),
                },
            );
            assert!(cache.loaded_bytes <= IMAGE_CACHE_BUDGET_BYTES);
            // The real render loop calls flush_evicted to release these CPU/GPU assets.
            cache.pending_free.clear();
        }
        assert!(!cache.map.contains_key(&AttachmentKey::new(
            "bounded-history",
            "/0.png",
            Some("image/png")
        )));
        assert!(cache.map.contains_key(&AttachmentKey::new(
            "bounded-history",
            "/99.png",
            Some("image/png")
        )));
    }

    #[test]
    fn generated_image_decoder_checks_actual_type_and_downsamples() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(3072, 16)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let bytes = png.into_inner();
        assert!(
            crate::image_media::decode_generated_image(
                bytes.clone(),
                "image/jpeg",
                MAX_ATTACHMENT_BYTES as usize
            )
            .is_err()
        );
        let decoded = crate::image_media::decode_generated_image(
            bytes,
            "image/png",
            MAX_ATTACHMENT_BYTES as usize,
        )
        .unwrap();
        assert_eq!(decoded.width, 2048.0);
        assert!(decoded.bytes < IMAGE_CACHE_BUDGET_BYTES);
        assert!(
            crate::image_media::decode_generated_image(
                b"GIF89a".to_vec(),
                "image/gif",
                MAX_ATTACHMENT_BYTES as usize
            )
            .is_err()
        );
    }

    #[test]
    fn generated_image_animation_retains_only_first_static_frame() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
                encoder
                    .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                        16,
                        16,
                        image::Rgba(color),
                    )))
                    .unwrap();
            }
        }
        let decoded = crate::image_media::decode_generated_image(
            bytes,
            "image/gif",
            MAX_ATTACHMENT_BYTES as usize,
        )
        .unwrap();
        assert_eq!(image::guess_format(&decoded.image.bytes).unwrap(), image::ImageFormat::Png);
        let pixels = image::load_from_memory(&decoded.image.bytes).unwrap().to_rgba8();
        assert_eq!(pixels.get_pixel(0, 0).0, [255, 0, 0, 255]);
    }

    struct ImageRpc {
        calls: Arc<Mutex<Vec<serde_json::Value>>>,
        bytes: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl slopty_view_rpc::RpcService for ImageRpc {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<slopty_view_rpc::RpcReply, slopty_view_rpc::RpcError> {
            assert_eq!(method, methods::READ_ATTACHMENT_CHUNK);
            self.calls.lock().unwrap().push(params);
            slopty_view_rpc::RpcReply::value(
                &serde_json::json!({"name":"generated.png", "mimeType":"image/png", "data":BASE64.encode(&self.bytes), "nextOffset": self.bytes.len(), "done":true}),
            )
        }
    }

    struct FileRpc(Vec<u8>);

    #[async_trait::async_trait]
    impl slopty_view_rpc::RpcService for FileRpc {
        async fn handle(
            &self,
            _: &str,
            _: serde_json::Value,
        ) -> Result<slopty_view_rpc::RpcReply, slopty_view_rpc::RpcError> {
            slopty_view_rpc::RpcReply::value(&serde_json::json!({
                "name": "ab12cd34-notes.md",
                "mimeType": "application/octet-stream",
                "data": BASE64.encode(&self.0),
                "nextOffset": self.0.len(),
                "done": true,
            }))
        }
    }

    #[tokio::test]
    #[ignore = "gpui-fast makes its Mac platform on the main thread only, and a test runs on another"]
    async fn an_image_read_back_never_accepts_an_empty_body() {
        let executor = gpui_platform::background_executor();
        let engine = EngineHandle::from_test_client(slopty_view_rpc::memory_client(Arc::new(
            FileRpc(Vec::new()),
        )));
        let image =
            read_attachment_image(&engine, &executor, None, "/uploads/ab12cd34-notes.md", None)
                .await;
        assert!(image.is_none());
    }

    #[tokio::test]
    #[ignore = "gpui-fast makes its Mac platform on the main thread only, and a test runs on another"]
    async fn generated_image_chunk_reader_targets_owner_and_bounds_decode() {
        let executor = gpui_platform::background_executor();
        for (width, target, expected, succeeds) in [
            (64, Some("remote-owner"), "image/png", true),
            (64, None, "image/png", true),
            (4097, None, "image/png", false),
            (64, None, "image/gif", false),
        ] {
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(width, 1)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            let calls = Arc::new(Mutex::new(vec![]));
            let engine = EngineHandle::from_test_client(slopty_view_rpc::memory_client(Arc::new(
                ImageRpc { calls: calls.clone(), bytes: png.into_inner() },
            )));
            let loaded = read_attachment_image(
                &engine,
                &executor,
                target,
                "/profile/uploads/image.png",
                Some(expected),
            )
            .await;
            assert_eq!(loaded.is_some(), succeeds);
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0]["path"], "/profile/uploads/image.png");
            assert_eq!(calls[0].get("targetDeviceId").and_then(|v| v.as_str()), target);
        }
    }

    #[test]
    fn generated_image_cancelled_load_releases_claim_without_losing_completed_images() {
        let owner = "cancelled-generated-owner";
        let path = "/fixture/cancelled-generated.png";
        let plain = AttachmentKey::new(owner, path, None);
        assert!(begin_load_for(&plain));
        drop(AttachmentLoadGuard(plain.clone()));
        assert!(matches!(attachment_snapshot_for(&plain), AttachmentSnapshot::Error { .. }));
        let image = Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new()));
        store_loaded_for(&plain, "generated.png".into(), image);
        drop(AttachmentLoadGuard(plain.clone()));
        assert!(matches!(attachment_snapshot_for(&plain), AttachmentSnapshot::Loaded(_)));
    }

    #[test]
    fn generated_image_decode_accepts_attachment_byte_cap_but_rejects_excess() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(1, 1).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut bytes = png.into_inner();
        // Padding represents a large source with small dimensions; the intake
        // cap is 24 MiB, independent of the workspace preview's 8 MiB cap.
        bytes.resize(9 * 1024 * 1024, 0);
        assert!(
            crate::image_media::decode_raster_image(bytes.clone(), MAX_ATTACHMENT_BYTES as usize)
                .is_ok()
        );
        assert!(crate::image_media::decode_raster_image(bytes, 8 * 1024 * 1024).is_err());
    }
}
