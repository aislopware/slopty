//! A file tile that shows rather than edits: a picture fitted to the tile, or a PDF's pages.
//!
//! The bytes come whole from the worker (`FileRead::Media`); the platform decodes them
//! ([`super::decode`]) off the UI thread, and only at the size they are drawn. A picture is
//! fitted inside the tile at its own resolution at most (a 144-dpi Retina screenshot at its
//! real size, a large photo scaled down), decoded to exactly the pixels it covers, and decoded
//! again when the tile grows past them or shrinks well below. A PDF is a list of its pages, each
//! as wide as the tile; a page is drawn when it scrolls into view, one at a time in order, and
//! only the pages near the view stay drawn.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use bytes::Bytes;
use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, ImageSource, InteractiveElement as _,
    IntoElement as _, ListAlignment, ListState, ParentElement as _, Pixels, RenderImage,
    SharedString, Size, StatefulInteractiveElement as _, Styled as _, Task, Window, canvas, div,
    img, list, px,
};
use parking_lot::Mutex;

mod pages;

pub use pages::{
    CTX as PAGES_CTX, CopyText, FirstPage, LastPage, NextPage, NextScreen, PreviousPage,
    PreviousScreen, ScrollDown, ScrollUp, SelectAllText, palette_items,
};
use slopty_theme::alpha;

use super::FileView;
use super::decode::{self, Pdf, PictureSize, PreviewError};
use crate::colors::{hsla, hsla_alpha};
use crate::icons::IconName;
use crate::kit::size_label;

/// What a file tile says of a picture or PDF it cannot show, over the reason.
pub(crate) const CANNOT_SHOW: &str = "Cannot show this file";

/// Pages kept drawn at most: those on screen and a few either side.
const PAGES_KEPT: usize = 12;

/// How far past the view the page list lays pages out (and so draws them), in points.
const OVERDRAW: f32 = 800.0;

/// A tile's picture or PDF, and how far it is shown.
pub(crate) struct Preview {
    /// The worker's word for what it is (`image/png`).
    media_type: String,
    /// The file's size.
    size: u64,
    body: Body,
}

enum Body {
    Picture(Picture),
    Pdf(Box<Pages>),
    Failed(PreviewError),
}

/// The drawing area as last laid out: its size in points and the window's pixels per point.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    size: Size<Pixels>,
    scale: f32,
}

struct Picture {
    bytes: Bytes,
    /// What the header said, once the first decode read it.
    size: Option<PictureSize>,
    /// The decode on screen, with the longer side it was asked for.
    shown: Option<(u32, Arc<RenderImage>)>,
    /// The decode running, with the longer side it asks for.
    decoding: Option<(u32, Task<()>)>,
    area: Option<Area>,
    /// Shown at its own size, scrolled, rather than fitted to the tile.
    actual: bool,
}

struct Pages {
    doc: Option<Arc<Mutex<Pdf>>>,
    /// Opening the document, off the UI thread.
    _opening: Task<()>,
    /// Each page's size in points.
    sizes: Vec<(f32, f32)>,
    list: ListState,
    /// Pages drawn, with the width drawn at and when each was last on screen.
    drawn: HashMap<usize, Drawn>,
    /// Pages on screen not yet drawn at the width, in order.
    queue: VecDeque<usize>,
    /// The page being drawn.
    drawing: Option<Task<()>>,
    /// The width pages are drawn at, in pixels, once the list is laid out.
    width: Option<u32>,
    /// Counts layouts, for which pages were on screen last.
    clock: u64,
    /// The file's bytes, for `PDFKit` when the text is first wanted.
    bytes: Bytes,
    /// `PDFKit`'s reading of the text, opened at the first press on a page; `None` inside when
    /// it cannot read it.
    text: std::cell::OnceCell<Option<super::pdf_text::PdfText>>,
    /// The text selected.
    selection: Option<pages::Selection>,
}

struct Drawn {
    width: u32,
    image: Arc<RenderImage>,
    seen: u64,
}

impl Preview {
    /// What the tile shows, in words: "image/png, 1200 × 800, 240 KB", "PDF, 12 pages, 2.4 MB".
    pub(crate) fn summary(&self) -> String {
        let size = size_label(self.size);
        match &self.body {
            Body::Picture(p) => match p.size {
                Some(s) => format!("{}, {} × {}, {size}", self.media_type, s.pixels.0, s.pixels.1),
                None => format!("{}, {size}", self.media_type),
            },
            Body::Pdf(pages) if pages.sizes.len() == 1 => format!("PDF, 1 page, {size}"),
            Body::Pdf(pages) if !pages.sizes.is_empty() => {
                format!("PDF, {} pages, {size}", pages.sizes.len())
            }
            Body::Pdf(_) => format!("PDF, {size}"),
            Body::Failed(e) => format!("{}, {size}: {}", self.media_type, e.say()),
        }
    }

    /// What the foot says, parted by middle dots: `PNG · 1200 × 800 · 240 KB`,
    /// `PDF · 12 pages · 2.4 MB`.
    pub(crate) fn facts(&self) -> String {
        let kind = kind_label(&self.media_type);
        let size = size_label(self.size);
        let middle = match &self.body {
            Body::Picture(p) => p.size.map(|s| format!("{} × {}", s.pixels.0, s.pixels.1)),
            Body::Pdf(pages) if pages.sizes.len() == 1 => Some("1 page".to_owned()),
            Body::Pdf(pages) if !pages.sizes.is_empty() => {
                Some(format!("{} pages", pages.sizes.len()))
            }
            Body::Pdf(_) | Body::Failed(_) => None,
        };
        [Some(kind), middle, Some(size)].into_iter().flatten().collect::<Vec<_>>().join(" · ")
    }

    /// The picture's decoded size in pixels (as page 0), or each page drawn with its size: what
    /// the tile holds.
    fn drawn(&self) -> Vec<(usize, u32, u32)> {
        match &self.body {
            Body::Picture(p) => p
                .shown
                .iter()
                .map(|(_, image)| {
                    let s = image.size(0);
                    (
                        0,
                        u32::try_from(s.width.0).unwrap_or(0),
                        u32::try_from(s.height.0).unwrap_or(0),
                    )
                })
                .collect(),
            Body::Pdf(pages) => {
                let mut drawn: Vec<_> = pages
                    .drawn
                    .iter()
                    .map(|(ix, d)| {
                        let s = d.image.size(0);
                        (
                            *ix,
                            u32::try_from(s.width.0).unwrap_or(0),
                            u32::try_from(s.height.0).unwrap_or(0),
                        )
                    })
                    .collect();
                drawn.sort_unstable();
                drawn
            }
            Body::Failed(_) => Vec::new(),
        }
    }
}

/// A media type as a person names the format: `image/png` is PNG, `image/svg+xml` SVG.
fn kind_label(media_type: &str) -> String {
    let sub = media_type.rsplit('/').next().unwrap_or(media_type);
    let sub = sub.split(['+', ';']).next().unwrap_or(sub);
    let sub = sub.strip_prefix("x-").unwrap_or(sub);
    sub.to_ascii_uppercase()
}

/// The largest the picture shows inside `area`, in points: its own size, or less to fit.
fn fitted(size: PictureSize, area: Area) -> (f32, f32) {
    #[expect(clippy::cast_possible_truncation, reason = "a size on screen")]
    let (w, h) = (size.points.0 as f32, size.points.1 as f32);
    if w <= 0.0 || h <= 0.0 {
        return (0.0, 0.0);
    }
    let (aw, ah) = (f32::from(area.size.width), f32::from(area.size.height));
    let fit = (aw / w).min(ah / h).min(1.0);
    (w * fit, h * fit)
}

/// The size the picture shows at in points: its own when `actual`, else [`fitted`].
fn shown_at(size: PictureSize, area: Area, actual: bool) -> (f32, f32) {
    if actual {
        #[expect(clippy::cast_possible_truncation, reason = "a size on screen")]
        let own = (size.points.0 as f32, size.points.1 as f32);
        own
    } else {
        fitted(size, area)
    }
}

/// Whether `size` is larger than `area`, so fitting it scales it down.
fn outgrows(size: PictureSize, area: Area) -> bool {
    let (w, h) = fitted(size, area);
    #[expect(clippy::cast_possible_truncation, reason = "a size on screen")]
    let own = (size.points.0 as f32, size.points.1 as f32);
    w < own.0 - 0.5 || h < own.1 - 0.5
}

/// The longer side to decode `size` at to cover its pixels on screen one to one, shown at
/// its own size (`actual`) or fitted to `area`.
fn wanted(size: PictureSize, area: Area, actual: bool) -> u32 {
    let (w, h) = shown_at(size, area, actual);
    whole_pixels(w.max(h) * area.scale)
}

/// A length in pixels, rounded up.
fn whole_pixels(length: f32) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive length on screen, clamped to the texture limit"
    )]
    let n = f64::from(length.ceil()).clamp(1.0, f64::from(decode::LARGEST)) as u32;
    n
}

/// Whether a decode of longer side `have` serves `want`: sharp enough, and not so large that it
/// holds memory for nothing.
const fn serves(have: u32, want: u32, most: u32) -> bool {
    (have >= want || have >= most) && have <= want.saturating_mul(2)
}

/// Frames as GPUI keeps a picture.
fn render_image(frames: Vec<decode::Frame>) -> Option<Arc<RenderImage>> {
    let frames: Vec<image::Frame> = frames
        .into_iter()
        .filter_map(|f| {
            let buffer =
                image::RgbaImage::from_raw(f.pixels.width, f.pixels.height, f.pixels.bgra)?;
            let delay = image::Delay::from_numer_denom_ms(f.delay_ms, 1);
            Some(image::Frame::from_parts(buffer, 0, 0, delay))
        })
        .collect();
    (!frames.is_empty()).then(|| Arc::new(RenderImage::new(frames)))
}

impl FileView {
    /// What the tile holds drawn of its picture or PDF: `(page, width, height)` in pixels, the
    /// picture as page 0; empty for a text, or before anything is drawn.
    #[must_use]
    pub fn preview_drawn(&self) -> Vec<(usize, u32, u32)> {
        self.preview.as_ref().map(Preview::drawn).unwrap_or_default()
    }

    /// The worker sent a picture or PDF: show it, dropping what showed before.
    pub(super) fn show_media(&mut self, media_type: &str, bytes: &Bytes, cx: &mut Context<Self>) {
        let size = bytes.len() as u64;
        let body = if media_type == "application/pdf" {
            Body::Pdf(Box::new(Self::open_pdf(bytes.clone(), cx)))
        } else if media_type.starts_with("image/") {
            // The last decode stays on screen until the new file's is ready, at its old size.
            let (shown, area) = match self.preview.take().map(|p| p.body) {
                Some(Body::Picture(old)) => (old.shown, old.area),
                _ => (None, None),
            };
            Body::Picture(Picture {
                bytes: bytes.clone(),
                size: None,
                shown,
                decoding: None,
                area,
                actual: false,
            })
        } else {
            Body::Failed(PreviewError::Unreadable)
        };
        self.preview = Some(Preview { media_type: media_type.to_owned(), size, body });
        self.decode_picture(cx);
        cx.notify();
    }

    fn open_pdf(bytes: Bytes, cx: &Context<Self>) -> Pages {
        let kept = bytes.clone();
        let opening = cx.spawn(async move |this, cx| {
            let opened = cx
                .background_spawn(async move {
                    let pdf = Pdf::open(bytes)?;
                    let sizes: Vec<(f32, f32)> = (0..pdf.pages())
                        .map(|ix| {
                            #[expect(clippy::cast_possible_truncation, reason = "a size on screen")]
                            pdf.page_size(ix).map_or((1.0, 1.0), |(w, h)| (w as f32, h as f32))
                        })
                        .collect();
                    Ok::<_, PreviewError>((pdf, sizes))
                })
                .await;
            let _gone = this.update(cx, |this, cx| {
                let Some(preview) = this.preview.as_mut() else { return };
                let Body::Pdf(pages) = &mut preview.body else { return };
                match opened {
                    Ok((pdf, sizes)) => {
                        pages.list.reset(sizes.len());
                        pages.sizes = sizes;
                        pages.doc = Some(Arc::new(Mutex::new(pdf)));
                    }
                    Err(e) => preview.body = Body::Failed(e),
                }
                cx.notify();
            });
        });
        Pages {
            doc: None,
            _opening: opening,
            sizes: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            drawn: HashMap::new(),
            queue: VecDeque::new(),
            drawing: None,
            width: None,
            clock: 0,
            bytes: kept,
            text: std::cell::OnceCell::new(),
            selection: None,
        }
    }

    /// Decode the picture for its area, unless what shows (or is on its way) serves it.
    fn decode_picture(&mut self, cx: &Context<Self>) {
        let Some(Preview { body: Body::Picture(p), .. }) = self.preview.as_mut() else { return };
        let Some(area) = p.area else { return };
        let actual = p.actual;
        let want = p.size.map(|size| wanted(size, area, actual));
        let most = p.size.map_or(u32::MAX, |s| s.pixels.0.max(s.pixels.1));
        let settled = |have: Option<u32>| match (have, want) {
            (Some(have), Some(want)) => serves(have, want, most),
            _ => false,
        };
        let pending = p.decoding.as_ref().map(|(n, _)| *n);
        let shown = p.shown.as_ref().map(|(n, _)| *n);
        if settled(pending) || (pending.is_none() && settled(shown)) {
            return;
        }
        let bytes = p.bytes.clone();
        let task = cx.spawn(async move |this, cx| {
            let decoded = cx
                .background_spawn(async move {
                    let mut longest = 0;
                    let picture = decode::picture(bytes, |size| {
                        longest = wanted(size, area, actual);
                        longest
                    })?;
                    Ok::<_, PreviewError>((picture, longest))
                })
                .await;
            let _gone = this.update(cx, |this, cx| {
                let Some(preview) = this.preview.as_mut() else { return };
                let Body::Picture(p) = &mut preview.body else { return };
                p.decoding = None;
                match decoded {
                    Ok((got, longest)) => {
                        p.size = Some(got.size);
                        p.shown = render_image(got.frames).map(|image| (longest, image));
                        // The tile may have moved on while this ran.
                        this.decode_picture(cx);
                    }
                    Err(e) => preview.body = Body::Failed(e),
                }
                cx.notify();
            });
        });
        p.decoding = Some((want.unwrap_or(0), task));
    }

    /// The picture's area was laid out at `area`; whether that is news.
    fn picture_area(&mut self, area: Area, cx: &Context<Self>) -> bool {
        let Some(Preview { body: Body::Picture(p), .. }) = self.preview.as_mut() else {
            return false;
        };
        if p.area == Some(area) {
            return false;
        }
        p.area = Some(area);
        self.decode_picture(cx);
        true
    }

    /// The page list was laid out `width` points wide at `scale` pixels a point: pages are
    /// drawn that wide, less the margins. Whether that is news.
    fn pages_width(&mut self, width: f32, scale: f32) -> bool {
        let margin = self.pad * self.zoom * 2.0;
        let Some(Preview { body: Body::Pdf(pages), .. }) = self.preview.as_mut() else {
            return false;
        };
        let drawn = whole_pixels((width - margin).max(1.0) * scale);
        let news = pages.width != Some(drawn);
        pages.width = Some(drawn);
        news
    }

    /// Page `ix` is being laid out: queue it to be drawn if it is not at the width.
    fn want_page(&mut self, ix: usize, cx: &Context<Self>) {
        let Some(Preview { body: Body::Pdf(pages), .. }) = self.preview.as_mut() else { return };
        let Some(width) = pages.width else { return };
        let clock = pages.clock;
        match pages.drawn.get_mut(&ix) {
            Some(d) if d.width == width => d.seen = clock,
            Some(d) => {
                d.seen = clock;
                if !pages.queue.contains(&ix) {
                    pages.queue.push_back(ix);
                }
            }
            None if !pages.queue.contains(&ix) => pages.queue.push_back(ix),
            None => {}
        }
        self.draw_next(cx);
    }

    /// Draw the next page the view wants, one at a time.
    fn draw_next(&mut self, cx: &Context<Self>) {
        let Some(Preview { body: Body::Pdf(pages), .. }) = self.preview.as_mut() else { return };
        if pages.drawing.is_some() {
            return;
        }
        let (Some(doc), Some(width)) = (pages.doc.clone(), pages.width) else { return };
        let Some(ix) = pages.queue.pop_front() else { return };
        pages.drawing = Some(cx.spawn(async move |this, cx| {
            let drawn = cx.background_spawn(async move { doc.lock().render(ix, width) }).await;
            let _gone = this.update(cx, |this, cx| {
                let Some(Preview { body: Body::Pdf(pages), .. }) = this.preview.as_mut() else {
                    return;
                };
                pages.drawing = None;
                let frame = drawn.ok().map(|pixels| decode::Frame { pixels, delay_ms: 0 });
                if let Some(image) = frame.and_then(|f| render_image(vec![f])) {
                    let seen = pages.clock;
                    pages.drawn.insert(ix, Drawn { width, image, seen });
                    pages.forget_far();
                }
                cx.notify();
                this.draw_next(cx);
            });
        }));
    }

    /// The body of a tile showing a picture or PDF.
    pub(super) fn render_preview(&mut self, cx: &Context<Self>) -> AnyElement {
        let Some(preview) = self.preview.as_mut() else {
            return div().size_full().into_any_element();
        };
        let summary = preview.summary();
        match &mut preview.body {
            Body::Failed(e) => {
                let detail = e.say().to_owned();
                self.notice(IconName::Image, CANNOT_SHOW, Some(detail), None)
            }
            Body::Picture(p) => {
                let shown = p.shown.as_ref().map(|(_, image)| Arc::clone(image));
                let size = match (p.size, p.area) {
                    (Some(size), Some(area)) => Some(fitted(size, area)),
                    _ => None,
                };
                self.render_picture(shown, size, &summary, cx)
            }
            Body::Pdf(pages) => {
                pages.clock = pages.clock.wrapping_add(1);
                pages.queue.clear();
                if pages.doc.is_none() {
                    return div().size_full().into_any_element();
                }
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(div().flex_1().min_h_0().child(self.render_pages(summary, cx)))
                    .child(self.preview_foot(cx))
                    .into_any_element()
            }
        }
    }

    fn render_picture(
        &self,
        shown: Option<Arc<RenderImage>>,
        size: Option<(f32, f32)>,
        summary: &str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        let measure = canvas(
            move |bounds: Bounds<Pixels>, window, cx| {
                let area = Area { size: bounds.size, scale: window.scale_factor() };
                if entity.update(cx, |this, cx| this.picture_area(area, cx)) {
                    // Laid out at the old area in this frame: the next one fits the new.
                    redraw_next_frame(&entity, window);
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full();
        let id = *self.id.as_uuid();
        let actual = self.picture_actual();
        let checker = self.checker();
        let picture = shown.zip(size).map(|(image, (w, h))| {
            div()
                .id("file-picture-image")
                .flex_none()
                .relative()
                .w(px(w))
                .h(px(h))
                .role(Role::Image)
                .aria_label(SharedString::from(summary.to_owned()))
                .child(checker)
                .child(img(ImageSource::Render(image)).absolute().size_full())
        });
        // At its own size the picture scrolls, centred while it is smaller than the tile on an
        // axis; fitted, it is centred and never larger than the tile.
        let stage = if actual {
            div()
                .id("file-picture-stage")
                .size_full()
                .overflow_scroll()
                .flex()
                .children(picture.map(gpui::Styled::m_auto))
        } else {
            div()
                .id("file-picture-stage")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .children(picture)
        };
        div()
            .id("file-picture")
            .debug_selector(move || format!("file-picture-{id}"))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p(px(self.pad * self.zoom))
                    .child(div().relative().size_full().child(measure).child(stage)),
            )
            .child(self.preview_foot(cx))
            .into_any_element()
    }

    /// Whether the picture shows at its own size rather than fitted.
    const fn picture_actual(&self) -> bool {
        matches!(&self.preview, Some(Preview { body: Body::Picture(p), .. }) if p.actual)
    }

    /// Whether the picture is larger than its area, so its own size differs from the fitted.
    fn picture_outgrows(&self) -> bool {
        match &self.preview {
            Some(Preview { body: Body::Picture(p), .. }) => {
                p.size.zip(p.area).is_some_and(|(size, area)| outgrows(size, area))
            }
            _ => false,
        }
    }

    /// Show the picture at its own size, scrolled, or fitted to the tile again.
    pub(crate) fn toggle_actual_size(&mut self, cx: &mut Context<Self>) {
        let Some(Preview { body: Body::Picture(p), .. }) = self.preview.as_mut() else { return };
        p.actual = !p.actual;
        tracing::info!(actual = p.actual, "picture zoom");
        self.decode_picture(cx);
        cx.notify();
    }

    /// The board a picture's transparent pixels show over: two neutral steps of the content
    /// plane in squares, as Preview and Figma show one.
    fn checker(&self) -> AnyElement {
        let theme = &self.theme;
        let (even, odd) = (hsla(theme.content()), hsla(theme.surfaces.hover));
        let cell = px(theme.spacing.sm * self.zoom);
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, _| {
                window.paint_quad(gpui::fill(bounds, even));
                let mut y = px(0.0);
                let mut shifted = false;
                while y < bounds.size.height {
                    let mut x = if shifted { px(0.0) } else { cell };
                    while x < bounds.size.width {
                        let side = gpui::size(
                            cell.min(bounds.size.width - x),
                            cell.min(bounds.size.height - y),
                        );
                        let at = bounds.origin + gpui::point(x, y);
                        window.paint_quad(gpui::fill(Bounds::new(at, side), odd));
                        x += cell + cell;
                    }
                    y += cell;
                    shifted = !shifted;
                }
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }

    /// The foot of a picture or PDF: what it is in a quiet line (its type, its pixels or pages,
    /// its size), and for a picture larger than the tile the way between fitted and its own
    /// size.
    fn preview_foot(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = self.zoom;
        let facts = self.preview.as_ref().map(Preview::facts).unwrap_or_default();
        let zoom = (self.picture_outgrows() || self.picture_actual()).then(|| {
            let label = if self.picture_actual() { "Fit" } else { "Actual size" };
            div()
                .id("file-picture-zoom")
                .debug_selector(|| "file-picture-zoom".to_owned())
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .cursor_pointer()
                .text_color(hsla(s.text_secondary))
                .hover(|el| el.text_color(hsla(s.text)))
                .child(label)
                .on_click(cx.listener(|this, _ev, _window, cx| this.toggle_actual_size(cx)))
        });
        div()
            .id("file-preview-foot")
            .debug_selector(|| "file-preview-foot".to_owned())
            .flex_none()
            .h(px(theme.density.row * k))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(theme.spacing.sm * k))
            .px(px(theme.spacing.inset() * k))
            .whitespace_nowrap()
            .font_family(theme.typography.ui_family.clone())
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .id("file-preview-facts")
                    .role(Role::Label)
                    .aria_label(SharedString::from(facts.clone()))
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(facts),
            )
            .children(zoom)
            .into_any_element()
    }

    fn render_pages(&self, summary: String, cx: &Context<Self>) -> AnyElement {
        let Some(Preview { body: Body::Pdf(pages), .. }) = self.preview.as_ref() else {
            return div().into_any_element();
        };
        let s = self.theme.surfaces;
        let entity = cx.entity();
        let listen = entity.clone();
        let measure = canvas(
            move |bounds: Bounds<Pixels>, window, cx| {
                let scale = window.scale_factor();
                if entity
                    .update(cx, |this, _| this.pages_width(f32::from(bounds.size.width), scale))
                {
                    redraw_next_frame(&entity, window);
                }
            },
            move |_, (), window, _| Self::follow_pointer(&listen, window),
        )
        .absolute()
        .size_full();
        let id = *self.id.as_uuid();
        let items = list(
            pages.list.clone(),
            cx.processor(|this, ix: usize, _window, cx| {
                this.want_page(ix, cx);
                this.render_page(ix)
            }),
        )
        .size_full();
        div()
            .id("file-pages")
            .debug_selector(move || format!("file-pages-{id}"))
            .relative()
            .size_full()
            .bg(hsla(s.canvas))
            .role(Role::Document)
            .aria_label(SharedString::from(summary))
            .cursor(gpui::CursorStyle::IBeam)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, window, cx| {
                    this.press_pages(e.position, e.click_count, window, cx);
                }),
            )
            .child(measure)
            .child(items)
            .into_any_element()
    }

    fn render_page(&self, ix: usize) -> AnyElement {
        let Some(Preview { body: Body::Pdf(pages), .. }) = self.preview.as_ref() else {
            return div().into_any_element();
        };
        let theme = &self.theme;
        let s = theme.surfaces;
        let k = self.zoom;
        let (w, h) = pages.sizes.get(ix).copied().unwrap_or((1.0, 1.0));
        let count = pages.sizes.len();
        let last = ix.saturating_add(1) == count;
        let image = pages.drawn.get(&ix).map(|d| Arc::clone(&d.image));
        let selected = pages.selection.as_ref().map(|s| s.on_page(ix)).unwrap_or_default();
        let tint = hsla_alpha(s.accent, alpha::TINT);
        // The page's selected text, over it.
        let overlay = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, _| {
                for &area in &selected {
                    #[expect(clippy::cast_possible_truncation, reason = "a place on screen")]
                    let [across, down, wide, tall] =
                        [area.0 as f32, area.1 as f32, area.2 as f32, area.3 as f32];
                    let page = bounds.size;
                    let origin = gpui::point(
                        bounds.origin.x + page.width * across,
                        bounds.origin.y + page.height * down,
                    );
                    let size = gpui::size(page.width * wide, page.height * tall);
                    window.paint_quad(gpui::fill(Bounds { origin, size }, tint));
                }
            },
        )
        .absolute()
        .size_full();
        div()
            .px(px(self.pad * k))
            .pt(px(if ix == 0 { self.pad } else { theme.spacing.sm } * k))
            .when(last, |el| el.pb(px(self.pad * k)))
            .child(
                div()
                    .id(("file-page", ix))
                    .w_full()
                    .aspect_ratio(w / h.max(1.0))
                    .bg(hsla(s.elevated))
                    .border(crate::kit::hair(theme))
                    .border_color(hsla(s.border))
                    .role(Role::Image)
                    .aria_label(SharedString::from(format!(
                        "Page {} of {count}",
                        ix.saturating_add(1)
                    )))
                    .relative()
                    .children(image.map(|image| img(ImageSource::Render(image)).size_full()))
                    .child(overlay),
            )
            .into_any_element()
    }
}

/// Redraw `view` on the next frame: what it measured while drawing this one changes its layout.
fn redraw_next_frame(view: &gpui::Entity<FileView>, window: &Window) {
    let view = view.downgrade();
    window.on_next_frame(move |_window, cx| {
        let _gone = view.update(cx, |_, cx| cx.notify());
    });
}

impl Pages {
    /// Let go of the drawn pages seen longest ago, past [`PAGES_KEPT`].
    fn forget_far(&mut self) {
        while self.drawn.len() > PAGES_KEPT {
            let Some(oldest) = self.drawn.iter().min_by_key(|(_, d)| d.seen).map(|(ix, _)| *ix)
            else {
                return;
            };
            self.drawn.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: f32, h: f32, scale: f32) -> Area {
        Area { size: gpui::size(px(w), px(h)), scale }
    }

    #[test]
    fn a_picture_fits_the_tile_at_its_own_size_at_most() {
        let photo = PictureSize { pixels: (4000, 3000), points: (4000.0, 3000.0) };
        assert_eq!(fitted(photo, area(800.0, 800.0, 2.0)), (800.0, 600.0), "scaled to fit");
        assert_eq!(
            wanted(photo, area(800.0, 800.0, 2.0), false),
            1600,
            "one pixel a pixel on Retina"
        );
        let icon = PictureSize { pixels: (64, 64), points: (64.0, 64.0) };
        assert_eq!(fitted(icon, area(800.0, 800.0, 2.0)), (64.0, 64.0), "never enlarged");
        let shot = PictureSize { pixels: (2880, 1800), points: (1440.0, 900.0) };
        assert_eq!(fitted(shot, area(1600.0, 1000.0, 2.0)), (1440.0, 900.0), "its real size");
        assert_eq!(wanted(shot, area(1600.0, 1000.0, 2.0), false), 2880);
    }

    /// At its own size a picture shows and decodes at its own resolution, however large;
    /// only one larger than its area is scaled down to fit.
    #[test]
    fn a_picture_at_its_own_size_is_decoded_whole() {
        let photo = PictureSize { pixels: (4000, 3000), points: (4000.0, 3000.0) };
        let tile = area(800.0, 800.0, 2.0);
        assert_eq!(shown_at(photo, tile, true), (4000.0, 3000.0));
        assert_eq!(wanted(photo, tile, true), decode::LARGEST.min(8000));
        assert!(outgrows(photo, tile));
        let icon = PictureSize { pixels: (64, 64), points: (64.0, 64.0) };
        assert!(!outgrows(icon, tile), "fitted is its own size");
        assert_eq!(kind_label("image/png"), "PNG");
        assert_eq!(kind_label("image/svg+xml"), "SVG");
        assert_eq!(kind_label("application/pdf"), "PDF");
    }

    #[test]
    fn a_decode_serves_until_the_tile_outgrows_it_or_shrinks_far_below() {
        assert!(serves(1600, 1600, 4000));
        assert!(!serves(1000, 1600, 4000), "too soft");
        assert!(serves(4000, 6000, 4000), "as sharp as the picture gets");
        assert!(!serves(4000, 1000, 4000), "far more than it shows");
        assert!(serves(1900, 1000, 4000));
    }
}
