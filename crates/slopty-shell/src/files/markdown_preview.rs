// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! Native, virtualized preview of a file's current Markdown buffer.
#![allow(
    let_underscore_drop,
    trivial_casts,
    clippy::arithmetic_side_effects,
    clippy::assigning_clones,
    clippy::cast_possible_truncation,
    clippy::clone_on_ref_ptr,
    clippy::items_after_statements,
    clippy::let_underscore_must_use,
    clippy::let_underscore_untyped,
    clippy::needless_collect,
    clippy::needless_for_each,
    clippy::needless_pass_by_ref_mut,
    clippy::or_fun_call,
    clippy::partial_pub_fields,
    clippy::type_complexity,
    reason = "shell lint migration: slice C"
)]
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, FocusHandle, ListAlignment, ListOffset, ListSizingBehavior, ListState,
    Render, SharedString, Task, Window, div, list, px,
};

use crate::image_media::release_media;
use crate::markdown::parser::{self, Block, BlockTree, InlineRun};
use crate::markdown::render::{self, LinkUi, RenderCache, RenderOptions};
use crate::theme::Theme;

const MAX_MEDIA_BYTES: usize = 64 * 1024 * 1024;
const MAX_MEDIA_ENTRIES: usize = 32;
const MAX_MARKDOWN_BYTES: usize = 2 * 1024 * 1024;
const MAX_PREVIEW_CONTENT_WIDTH: f32 = 900.0;
const PREVIEW_VERTICAL_PADDING: f32 = 16.0;

/// A preview of a file beyond the workspace renders its local links as
/// plain text: they would resolve against the linking chat's checkout, not
/// the document's own folder, so their styling drops before render. Web
/// links keep their normal handling.
fn strip_outside_links(block: &mut Block) {
    fn strip_runs(runs: &mut [InlineRun]) {
        for run in runs {
            if run.style.image.is_none()
                && run.style.link.as_deref().is_some_and(|target| !web_target(target))
            {
                run.style.link = None;
            }
            if let Some(image) = &mut run.style.image
                && image.link.as_deref().is_some_and(|target| !web_target(target))
            {
                image.link = None;
            }
        }
    }
    match block {
        Block::Paragraph { runs } | Block::Heading { runs, .. } => strip_runs(runs),
        Block::BlockQuote { children } => children.iter_mut().for_each(strip_outside_links),
        Block::List { items, .. } => items.iter_mut().flatten().for_each(strip_outside_links),
        Block::Table { header, rows, .. } => {
            header.iter_mut().for_each(|runs| strip_runs(runs));
            rows.iter_mut().flatten().for_each(|runs| strip_runs(runs));
        }
        Block::CodeBlock { .. } | Block::Math { .. } | Block::Rule => {}
    }
}

/// Destinations a preview still treats as links: web navigation and mail,
/// mirroring `preview_link_outcome`.
fn web_target(target: &str) -> bool {
    crate::browser::model::transcript_address(target).is_ok()
        || (!target.chars().any(char::is_control)
            && url::Url::parse(target).is_ok_and(|url| url.scheme() == "mailto"))
}

pub(super) fn is_markdown(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown"))
}

fn preview_link_outcome(activation: &render::LinkActivation) -> render::LinkOutcome {
    let target = &activation.target.original;
    // File previews already support mail links; chat's web-only policy does
    // not replace this surface's specialized routing.
    if !target.chars().any(char::is_control)
        && url::Url::parse(target).is_ok_and(|url| url.scheme() == "mailto")
    {
        render::LinkOutcome::External(target.clone())
    } else {
        activation.web_outcome(false)
    }
}

/// URL path resolution is independent of the UI host's filesystem.
pub(super) fn relative_target(document: &str, target: &str) -> Option<(String, Option<String>)> {
    if target.starts_with('/') || target.contains(':') || target.contains('\\') {
        return None;
    }
    let (path, anchor) =
        target.split_once('#').map_or((target, None), |(p, a)| (p, Some(a.to_owned())));
    let decode = |s: &str| -> Option<String> {
        let mut bytes = Vec::new();
        let mut chars = s.as_bytes().iter().copied();
        while let Some(c) = chars.next() {
            if c == b'%' {
                let a = (chars.next()? as char).to_digit(16)?;
                let b = (chars.next()? as char).to_digit(16)?;
                bytes.push((a * 16 + b) as u8);
            } else {
                bytes.push(c);
            }
        }
        String::from_utf8(bytes).ok()
    };
    let path = decode(path)?;
    if path.contains(['\\', ':', '\0']) || path.starts_with('/') {
        return None;
    }
    if path.is_empty() {
        return Some((document.into(), anchor.and_then(|a| decode(&a))));
    }
    let mut parts: Vec<&str> = document.split('/').collect();
    parts.pop();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some((parts.join("/"), anchor.and_then(|a| decode(&a))))
}

pub(super) struct MarkdownPreview {
    pub focus: FocusHandle,
    pub version: Option<(u64, u64, Option<String>)>,
    path: String,
    media_location: Option<(Option<String>, String)>,
    scope: String,
    tree: BlockTree,
    parsed_source: Arc<str>,
    pub editor: Option<gpui::WeakEntity<super::editor::FileEditorState>>,
    list: ListState,
    cache: Rc<RefCell<RenderCache>>,
    highlights: HashMap<usize, Arc<slopty_syntax::HighlightedDocument>>,
    anchors: HashMap<String, usize>,
    parse_task: Option<Task<()>>,
    epoch: u64,
    loading: bool,
    truncated: bool,
    pub media_client: Option<(super::client::WorkspaceFilesClient, String)>,
    images: HashMap<String, Result<crate::image_media::MediaImage, String>>,
    image_task: Option<Task<()>>,
    image_generation: u64,
    media_dirty: bool,
    image_allowed: Rc<HashSet<String>>,
    diagram_allowed: Rc<HashSet<String>>,
    image_snapshot: Rc<HashMap<String, Result<crate::image_media::MediaImage, String>>>,
    diagram_snapshot: Rc<HashMap<String, Result<crate::image_media::MediaImage, String>>>,
    visible_rows: HashSet<SharedString>,
    code_fences: HashMap<SharedString, render::CodeFenceRuntime>,
    copied_code: Option<(SharedString, usize)>,
    copied_clear: Option<Task<()>>,
    needs_focus: bool,
    suspended: bool,
    selection_pointer: Option<gpui::Point<gpui::Pixels>>,
    selection_task: Option<Task<()>>,
    diagrams: HashMap<String, Result<crate::image_media::MediaImage, String>>,
    diagram_task: Option<Task<()>>,
    diagram_style: u32,
    source_visible: HashSet<String>,
    preview_image: Option<crate::attachments::PreviewImage>,
    zoom_source: Option<crate::image_media::MediaImage>,
    zoom_render: Option<crate::image_media::MediaImage>,
    preview_focus: FocusHandle,
    open_file: Rc<dyn Fn(String, &mut gpui::App)>,
    open_web_link: Option<WebLinkHandler>,
}

pub(super) type WebLinkHandler = Rc<dyn Fn(&render::LinkActivation, &mut gpui::App)>;

impl MarkdownPreview {
    fn close_media_preview(&mut self, cx: &mut gpui::App) {
        if let Some(preview) = self.preview_image.take()
            && self
                .zoom_source
                .as_ref()
                .is_some_and(|source| !Arc::ptr_eq(&source.image, &preview.image))
        {
            cx.defer(move |cx| gpui::ImageSource::Image(preview.image).evict(None, cx));
        }
        self.zoom_source = None;
        self.zoom_render = None;
    }

    fn resize_media(&mut self, window: &Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size();
        let panel_width = self.list.viewport_bounds().size.width;
        let target = (
            (f32::from(if panel_width > px(0.0) { panel_width } else { viewport.width }) - 48.0)
                .clamp(1.0, MAX_PREVIEW_CONTENT_WIDTH),
            480.0,
        );
        let mut retired = Vec::new();
        // Admission accounts current rasters, so a sharper one must still fit
        // the document budget.
        let mut used: usize = self
            .images
            .values()
            .chain(self.diagrams.values())
            .filter_map(|m| m.as_ref().ok())
            .map(|m| m.bytes)
            .sum();
        for media in self
            .images
            .values_mut()
            .chain(self.diagrams.values_mut())
            .filter_map(|m| m.as_mut().ok())
        {
            let others = used - media.bytes;
            let next = media.preview_within(
                target,
                window.scale_factor(),
                MAX_MEDIA_BYTES.saturating_sub(others),
            );
            if !Arc::ptr_eq(&media.image, &next.image) {
                used = others + next.bytes;
                retired.push(std::mem::replace(media, next));
                self.media_dirty = true;
            }
        }
        release_media(retired, cx);
        if let (Some(source), Some(preview)) = (&self.zoom_source, &mut self.preview_image) {
            let used: usize = self
                .images
                .values()
                .chain(self.diagrams.values())
                .filter_map(|m| m.as_ref().ok())
                .map(|m| m.bytes)
                .sum();
            let next = source.enlarged(
                (f32::from(viewport.width) * 0.9, f32::from(viewport.height) * 0.85),
                window.scale_factor(),
                MAX_MEDIA_BYTES.saturating_sub(used),
                self.zoom_render.as_ref(),
            );
            // Keep a stable image identity while the viewport is unchanged.
            if !Arc::ptr_eq(&preview.image, &next.image) {
                let old = std::mem::replace(&mut preview.image, next.image.clone());
                if !Arc::ptr_eq(&old, &source.image) {
                    cx.defer(move |cx| gpui::ImageSource::Image(old).evict(None, cx));
                }
            }
            self.zoom_render = Some(next);
        }
    }

    pub fn new(
        path: String,
        open_file: Rc<dyn Fn(String, &mut gpui::App)>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|view, cx| {
            view.close_media_preview(cx);
            render::clear_selection_surface(&view.scope);
            let media = view
                .images
                .drain()
                .chain(view.diagrams.drain())
                .filter_map(|(_, result)| result.ok());
            release_media(media, cx);
        })
        .detach();
        Self {
            parsed_source: Arc::from(""),
            editor: None,
            image_generation: 0,
            media_dirty: true,
            image_allowed: Rc::default(),
            diagram_allowed: Rc::default(),
            image_snapshot: Rc::default(),
            diagram_snapshot: Rc::default(),
            visible_rows: HashSet::new(),
            code_fences: HashMap::new(),
            copied_code: None,
            copied_clear: None,
            needs_focus: true,
            suspended: false,
            selection_pointer: None,
            selection_task: None,
            diagrams: HashMap::new(),
            diagram_task: None,
            diagram_style: 0,
            source_visible: HashSet::new(),
            preview_image: None,
            zoom_source: None,
            zoom_render: None,
            preview_focus: cx.focus_handle(),
            media_client: None,
            images: HashMap::new(),
            image_task: None,
            focus: cx.focus_handle(),
            version: None,
            path,
            media_location: None,
            scope: format!("md-preview-{}|", cx.entity_id()),
            tree: BlockTree::default(),
            list: ListState::new(0, ListAlignment::Top, px(400.0)),
            cache: Rc::new(RefCell::new(RenderCache::default())),
            highlights: HashMap::new(),
            anchors: HashMap::new(),
            parse_task: None,
            epoch: 0,
            loading: false,
            truncated: false,
            open_file,
            open_web_link: None,
        }
    }

    pub fn set_source(&mut self, mut source: String, truncated: bool, cx: &mut Context<Self>) {
        let clipped = source.len() > MAX_MARKDOWN_BYTES;
        if clipped {
            let mut end = MAX_MARKDOWN_BYTES;
            while !source.is_char_boundary(end) {
                end -= 1;
            }
            source.truncate(end);
        }
        render::clear_selection_surface(&self.scope);
        self.epoch = self.epoch.wrapping_add(1);
        self.image_task = None;
        self.diagram_task = None;
        self.close_media_preview(cx);
        self.source_visible.clear();
        self.copied_clear = None;
        self.copied_code = None;
        let epoch = self.epoch;
        self.loading = self.tree.is_empty();
        self.truncated = truncated || clipped;
        let parsed_source: Arc<str> = Arc::from(source.as_str());
        // Outside documents render their local links as plain text.
        let outside = super::path_is_outside(&self.path);
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(120)).await;
            let (tree, highlights, anchors) = cx
                .background_executor()
                .spawn(async move {
                    let mut tree = parser::parse_full(&source);
                    if outside {
                        for top in &mut tree.blocks {
                            strip_outside_links(&mut Arc::make_mut(top).block);
                        }
                    }
                    let mut highlights = HashMap::new();
                    let mut anchors = HashMap::new();
                    for (ix, top) in tree.blocks.iter().enumerate() {
                        if let Block::CodeBlock { language, code } = &top.block
                            && let Ok(doc) =
                                slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                                    source: code,
                                    path: None,
                                    fence_tag: language.as_deref(),
                                })
                        {
                            highlights.insert(ix, Arc::new(doc));
                        }
                        if let Block::Heading { runs, .. } = &top.block {
                            let text: String = runs.iter().map(|r| r.text.as_str()).collect();
                            let slug: String = text
                                .to_lowercase()
                                .chars()
                                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
                                .map(|c| if c == ' ' { '-' } else { c })
                                .collect();
                            let mut unique = slug.clone();
                            let mut n = 1;
                            while anchors.contains_key(&unique) {
                                unique = format!("{slug}-{n}");
                                n += 1;
                            }
                            anchors.insert(unique, ix);
                        }
                    }
                    (tree, highlights, anchors)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.epoch != epoch {
                    return;
                }
                let offset = view.list.logical_scroll_top();
                view.list.reset(tree.len());
                if !tree.is_empty() {
                    view.list.scroll_to(ListOffset {
                        item_ix: offset.item_ix.min(tree.len() - 1),
                        offset_in_item: offset.offset_in_item,
                    });
                }
                view.tree = tree;
                let scope = view.scope.clone();
                let active_code_fences: HashSet<SharedString> = view
                    .tree
                    .blocks
                    .iter()
                    .enumerate()
                    .flat_map(move |(top_ix, top)| {
                        let scope = scope.clone();
                        render::code_block_indices(&top.block, top_ix)
                            .into_iter()
                            .map(move |ix| SharedString::from(format!("{scope}{top_ix}#code{ix}")))
                    })
                    .collect();
                view.code_fences.retain(|key, _| active_code_fences.contains(key));
                view.parsed_source = parsed_source;
                view.highlights = highlights;
                view.anchors = anchors;
                view.cache.borrow_mut().clear();
                view.loading = false;
                view.load_images(cx);
                view.load_diagrams(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn load_images(&mut self, cx: &mut Context<Self>) {
        self.media_dirty = true;
        self.image_generation = self.image_generation.wrapping_add(1);
        let generation = self.image_generation;
        use futures::{StreamExt as _, stream};
        let sources = super::markdown_media::image_sources(&self.tree);
        self.image_allowed = Rc::new(sources.iter().take(MAX_MEDIA_ENTRIES).cloned().collect());
        let removed: Vec<_> =
            self.images.keys().filter(|source| !sources.contains(source)).cloned().collect();
        release_media(
            removed
                .into_iter()
                .filter_map(|source| self.images.remove(&source).and_then(Result::ok)),
            cx,
        );
        let Some((client, checkout)) = self.media_client.clone() else {
            for source in sources.into_iter().take(MAX_MEDIA_ENTRIES) {
                self.images
                    .entry(source)
                    .or_insert_with(|| Err("Workspace image connection unavailable".into()));
            }
            return;
        };
        let jobs: Vec<_> = sources
            .into_iter()
            .take(MAX_MEDIA_ENTRIES)
            .filter(|s| !self.images.contains_key(s))
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|source| {
                if source.starts_with("https://") || source.starts_with("http://") {
                    return None;
                }
                if let Some((path, _)) = relative_target(&self.path, &source) {
                    Some((source, path))
                } else {
                    self.images.insert(source, Err("Image path is outside the workspace".into()));
                    None
                }
            })
            .collect();
        let epoch = self.epoch;
        self.image_task = Some(cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let mut results = stream::iter(jobs)
                .map(|(source, path)| {
                    let client = client.clone();
                    let checkout = checkout.clone();
                    let executor = executor.clone();
                    async move {
                        let read = Box::pin(client.read_image(path, checkout));
                        let deadline = Box::pin(executor.timer(Duration::from_secs(30)));
                        let response = match futures::future::select(read, deadline).await {
                            futures::future::Either::Left((result, _)) => result,
                            futures::future::Either::Right(_) => {
                                Err(super::client::FilesClientError::Transport(
                                    "Image preview timed out".into(),
                                ))
                            }
                        };
                        let result = match response {
                            Ok((mime, bytes)) => executor
                                .spawn(
                                    async move { crate::image_media::decode_image(&mime, bytes) },
                                )
                                .await,
                            Err(error) => Err(error.to_string()),
                        };
                        (source, result)
                    }
                })
                .buffer_unordered(3);
            while let Some((source, result)) = results.next().await {
                if this
                    .update(cx, |view, cx| {
                        if view.epoch != epoch || view.image_generation != generation {
                            return;
                        }
                        let result = view.admit_media(result);
                        view.images.insert(source, result);
                        view.media_dirty = true;
                        view.list.remeasure_items(0..view.tree.len());
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn load_diagrams(&mut self, cx: &mut Context<Self>) {
        self.media_dirty = true;
        let sources = super::markdown_media::diagram_sources(&self.tree);
        self.diagram_allowed = Rc::new(sources.iter().take(MAX_MEDIA_ENTRIES).cloned().collect());
        let removed: Vec<_> =
            self.diagrams.keys().filter(|code| !sources.contains(code)).cloned().collect();
        release_media(
            removed.into_iter().filter_map(|code| self.diagrams.remove(&code).and_then(Result::ok)),
            cx,
        );
        let style = crate::theme::style_generation();
        if self.diagram_style != style {
            self.close_media_preview(cx);
            release_media(self.diagrams.drain().filter_map(|(_, result)| result.ok()), cx);
            self.diagram_style = style;
        }
        let palette = crate::markdown::mermaid::Palette::from_theme(Theme::of(cx));
        let jobs: Vec<_> = sources
            .into_iter()
            .take(MAX_MEDIA_ENTRIES)
            .filter(|code| !self.diagrams.contains_key(code))
            .collect();
        let epoch = self.epoch;
        self.diagram_task = Some(cx.spawn(async move |this, cx| {
            for code in jobs {
                let source = code.clone();
                let palette = palette.clone();
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let svg = crate::markdown::mermaid::render(&source, &palette)?;
                        crate::image_media::decode_image("image/svg+xml", svg.into_bytes())
                    })
                    .await;
                if this
                    .update(cx, |view, cx| {
                        if view.epoch != epoch || view.diagram_style != style {
                            return;
                        }
                        let result = view.admit_media(result);
                        view.diagrams.insert(code, result);
                        view.media_dirty = true;
                        view.list.remeasure_items(0..view.tree.len());
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }

    fn admit_media(
        &self,
        result: Result<crate::image_media::MediaImage, String>,
    ) -> Result<crate::image_media::MediaImage, String> {
        let used: usize = self
            .images
            .values()
            .chain(self.diagrams.values())
            .filter_map(|r| r.as_ref().ok())
            .map(|m| m.bytes)
            .sum();
        result.and_then(|media| {
            if used.saturating_add(media.bytes) > MAX_MEDIA_BYTES {
                Err("Document media preview memory limit reached".into())
            } else {
                Ok(media)
            }
        })
    }

    pub fn activate(
        &mut self,
        path: &str,
        location: &(Option<String>, String),
        cx: &mut Context<Self>,
    ) {
        if self.path != path || self.media_location.as_ref() != Some(location) {
            self.path = path.to_owned();
            self.media_location = Some(location.clone());
            self.version = None;
            self.image_generation = self.image_generation.wrapping_add(1);
            self.image_task = None;
            self.close_media_preview(cx);
            release_media(self.images.drain().filter_map(|(_, result)| result.ok()), cx);
            self.media_dirty = true;
        }
        if self.suspended {
            self.suspended = false;
            self.needs_focus = true;
            self.version = None;
        }
    }

    pub fn suspend(&mut self, cx: &mut Context<Self>) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        self.media_dirty = true;
        self.epoch = self.epoch.wrapping_add(1);
        self.parse_task = None;
        self.image_task = None;
        self.diagram_task = None;
        self.selection_task = None;
        self.copied_clear = None;
        self.copied_code = None;
        self.code_fences.clear();
        self.close_media_preview(cx);
        self.image_snapshot = Rc::default();
        self.diagram_snapshot = Rc::default();
        self.image_allowed = Rc::default();
        self.diagram_allowed = Rc::default();
        self.version = None;
        self.tree = BlockTree::default();
        self.parsed_source = Arc::from("");
        self.editor = None;
        self.highlights.clear();
        self.anchors.clear();
        self.cache.borrow_mut().clear();
        render::clear_selection_surface(&self.scope);
        release_media(
            self.images.drain().chain(self.diagrams.drain()).filter_map(|(_, result)| result.ok()),
            cx,
        );
    }

    pub fn invalidate_images(&mut self, changed: Option<&str>, cx: &mut Context<Self>) {
        let removed: Vec<_> = self
            .images
            .keys()
            .filter(|source| {
                changed.is_none_or(|changed| {
                    relative_target(&self.path, source).is_some_and(|(path, _)| {
                        path == changed || path.starts_with(&format!("{changed}/"))
                    })
                })
            })
            .cloned()
            .collect();
        // Include pending loads in invalidation: a watcher can arrive before the first response.
        let affected = changed.is_none_or(|changed| {
            super::markdown_media::image_sources(&self.tree).iter().any(|source| {
                relative_target(&self.path, source).is_some_and(|(path, _)| {
                    path == changed || path.starts_with(&format!("{changed}/"))
                })
            })
        });
        if !affected {
            return;
        }
        self.close_media_preview(cx);
        release_media(
            removed
                .into_iter()
                .filter_map(|source| self.images.remove(&source).and_then(Result::ok)),
            cx,
        );
        if !self.suspended {
            self.load_images(cx);
        }
        self.list.remeasure_items(0..self.tree.len());
        cx.notify();
    }

    fn owns_selection(&self) -> bool {
        crate::markdown::selection::anchor_key().is_some_and(|key| key.starts_with(&self.scope))
    }

    fn selection_move(
        &mut self,
        event: &gpui::MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() || !self.owns_selection() || !crate::markdown::selection::is_dragging()
        {
            self.selection_task = None;
            self.selection_pointer = None;
            return;
        }
        self.selection_pointer = Some(event.position);
        if render::update_drag_at(event.position) {
            cx.notify();
        }
        self.schedule_selection_scroll(cx);
    }

    fn schedule_selection_scroll(&mut self, cx: &mut Context<Self>) {
        if self.selection_task.is_some() {
            return;
        }
        let Some(position) = self.selection_pointer else {
            return;
        };
        if crate::transcript::selection_scroll_step(self.list.viewport_bounds(), position) == 0.0 {
            return;
        }
        self.selection_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let _ = this.update(cx, |view, cx| {
                view.selection_task = None;
                if !view.owns_selection() || !crate::markdown::selection::is_dragging() {
                    return;
                }
                let Some(position) = view.selection_pointer else {
                    return;
                };
                render::update_drag_at(position);
                view.list.scroll_by(px(crate::transcript::selection_scroll_step(
                    view.list.viewport_bounds(),
                    position,
                )));
                cx.notify();
                view.schedule_selection_scroll(cx);
            });
        }));
    }

    fn copy_ui_for(&self, row_id: &SharedString, cx: &mut Context<Self>) -> render::CopyUi {
        let copied_ix = self.copied_code.as_ref().filter(|(id, _)| id == row_id).map(|(_, ix)| *ix);
        let row_key = row_id.clone();
        let entity = cx.weak_entity();
        let handler =
            Rc::new(move |ix, code: SharedString, _window: &mut Window, cx: &mut gpui::App| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.to_string()));
                let row_key = row_key.clone();
                let _ = entity.update(cx, |view, cx| {
                    view.copied_code = Some((row_key, ix));
                    view.copied_clear = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(1200)).await;
                        let _ = this.update(cx, |view, cx| {
                            view.copied_code = None;
                            view.copied_clear = None;
                            cx.notify();
                        });
                    }));
                    cx.notify();
                });
            });
        render::CopyUi { handler, copied_ix }
    }

    fn code_ui_for(
        &mut self,
        row_id: &SharedString,
        block_ix: usize,
        cx: &mut Context<Self>,
    ) -> render::CodeUi {
        let key: SharedString = format!("{row_id}#code{block_ix}").into();
        let runtime = self.code_fences.entry(key.clone()).or_default();
        render::code_ui_for(key, runtime, cx.weak_entity(), |view| &mut view.code_fences)
    }

    fn code_uis_for(
        &mut self,
        row_id: &SharedString,
        block: &Block,
        block_ix: usize,
        cx: &mut Context<Self>,
    ) -> Option<HashMap<usize, render::CodeUi>> {
        let indices = render::code_block_indices(block, block_ix);
        (!indices.is_empty())
            .then(|| indices.into_iter().map(|ix| (ix, self.code_ui_for(row_id, ix, cx))).collect())
    }

    fn media_element(
        loaded: &crate::image_media::MediaImage,
        id: SharedString,
        name: String,
        plate: Option<gpui::Hsla>,
        weak: gpui::WeakEntity<Self>,
    ) -> AnyElement {
        let mut preview = crate::attachments::PreviewImage::new(name, loaded.image.clone());
        preview.plate = plate;
        let source = loaded.clone();
        crate::image_media::preview_element(loaded, id, move |window, cx| {
            let _ = weak.update(cx, |view, cx| {
                view.close_media_preview(cx);
                view.zoom_source = Some(source.clone());
                preview.viewer.reset();
                view.preview_image = Some(preview.clone());
                window.focus(&view.preview_focus, cx);
                cx.notify();
            });
        })
    }

    #[cfg(test)]
    pub(super) const fn test_tree(&self) -> &BlockTree {
        &self.tree
    }

    #[cfg(test)]
    pub(super) const fn test_suspended(&self) -> bool {
        self.suspended
    }

    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_rows(ix..ix + 1, window, cx)
            .pop()
            .unwrap_or_else(|| gpui::Empty.into_any_element())
    }

    pub(super) fn set_web_link_handler(&mut self, handler: WebLinkHandler) {
        self.open_web_link = Some(handler);
    }

    pub(super) fn link_ui(&self, cx: &Context<Self>) -> LinkUi {
        let weak = cx.weak_entity();
        let open_web_link = self.open_web_link.clone();
        LinkUi {
            source_session: None,
            source_local: false,
            file_roots: None,
            handler: Rc::new(move |activation, _, cx| {
                if weak.upgrade().is_none() {
                    return render::LinkOutcome::Rejected;
                }
                if activation.target.navigation.is_ok()
                    && let Some(open_web_link) = &open_web_link
                {
                    // Emit through the owning FilesSurface before borrowing
                    // this preview: selecting Browser can suspend this view.
                    open_web_link(activation, cx);
                    return render::LinkOutcome::Internal;
                }
                let target = &activation.target.original;
                weak.update(cx, |view, cx| {
                    let Some((path, anchor)) = relative_target(&view.path, target) else {
                        return preview_link_outcome(activation);
                    };
                    if path == view.path {
                        if let Some(ix) = anchor.as_ref().and_then(|a| view.anchors.get(a)) {
                            view.list
                                .scroll_to(ListOffset { item_ix: *ix, offset_in_item: px(0.0) });
                            cx.notify();
                        }
                    } else {
                        (view.open_file)(path, cx);
                    }
                    render::LinkOutcome::Internal
                })
                .unwrap_or(render::LinkOutcome::Rejected)
            }),
        }
    }

    fn render_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = Theme::of(cx).clone();
        let link = self.link_ui(cx);
        range
            .filter_map(|ix| {
                let top = self.tree.blocks.get(ix)?.clone();
                let mut opts = RenderOptions::settled(
                    format!("{}{ix}", self.scope).into(),
                    crate::clock::now(cx),
                );
                opts.code = self.code_uis_for(&opts.row_key, &top.block, ix, cx);
                let editor = self.editor.as_ref().and_then(gpui::WeakEntity::upgrade);
                let source = self.parsed_source.clone();
                opts.tasks = Some(render::TaskUi {
                    toggle: editor
                        .filter(|editor| {
                            !self.truncated
                                && editor.read(cx).context_menu_capabilities().is_editable()
                        })
                        .map(|editor| {
                            let editor = editor.downgrade();
                            Rc::new(
                                move |task: &parser::TaskMarker,
                                      window: &mut Window,
                                      cx: &mut gpui::App| {
                                    if let Some(editor) = editor.upgrade() {
                                        super::editor::toggle_markdown_task(
                                            &editor, &source, task, window, cx,
                                        );
                                    }
                                },
                            )
                                as Rc<dyn Fn(&parser::TaskMarker, &mut Window, &mut gpui::App)>
                        }),
                });
                self.visible_rows.insert(opts.row_key.clone());
                opts.cache = Some(self.cache.clone());
                let images = self.image_snapshot.clone();
                let image_owner = cx.weak_entity();
                let image_link = link.clone();
                let diagram_owner = cx.weak_entity();
                let diagrams = self.diagram_snapshot.clone();
                let source_visible = self.source_visible.clone();
                let image_allowed = self.image_allowed.clone();
                let diagram_allowed = self.diagram_allowed.clone();
                opts.media = Some(render::MediaUi {
                    chart: None, // a chart fence is the transcript's (M)
                    diagram: Some(Rc::new(move |code, id, theme| {
                        let state = diagrams.get(code);
                        let allowed = diagram_allowed.contains(code);
                        let source_shown = source_visible.contains(id.as_ref()) || !allowed;
                        let owner = diagram_owner.clone();
                        let toggle_id = id.to_string();
                        let body = match state {
                            Some(Ok(loaded)) => Self::media_element(
                                loaded,
                                format!("{id}-image").into(),
                                "Mermaid diagram".into(),
                                Some(crate::markdown::mermaid::Palette::plate(theme)),
                                diagram_owner.clone(),
                            ),
                            Some(Err(error)) => div()
                                .p(px(12.0))
                                .text_size(crate::typography::ui_rems(12.0))
                                .text_color(theme.warning_muted)
                                .child(format!("Couldn't draw this diagram · {error}"))
                                .into_any_element(),
                            None => div()
                                .p(px(12.0))
                                .text_color(theme.text_muted)
                                .child(if diagram_allowed.contains(code) {
                                    "Rendering diagram…"
                                } else {
                                    "Document diagram preview limit reached"
                                })
                                .into_any_element(),
                        };
                        render::DiagramView::Diagram(render::DiagramUi {
                            body,
                            show_source: source_shown,
                            toggle_source: Rc::new(move |_, cx| {
                                let _ = owner.update(cx, |view, cx| {
                                    if !view.source_visible.remove(&toggle_id) {
                                        view.source_visible.insert(toggle_id.clone());
                                    }
                                    view.list.remeasure_items(0..view.tree.len());
                                    cx.notify();
                                });
                            }),
                        })
                    })),
                    image: Some(Rc::new(move |image, id, theme| {
                        match images.get(&image.source) {
                            Some(Ok(loaded)) => {
                                let mut el = div().flex().flex_col().gap(px(4.0)).child(
                                    Self::media_element(
                                        loaded,
                                        id,
                                        if image.alt.is_empty() {
                                            image.source.clone()
                                        } else {
                                            image.alt.clone()
                                        },
                                        None,
                                        image_owner.clone(),
                                    ),
                                );
                                if let Some(target) = image.link.clone() {
                                    let link = image_link.clone();
                                    el = el.child(
                                        super::toolbar_button(
                                            "markdown-image-link",
                                            "Open image link",
                                            theme,
                                        )
                                        .on_click(move |_, window, cx| {
                                            render::activate_link(
                                                render::LinkTarget::new(&target, &target),
                                                render::LinkAction::Primary,
                                                Some(&link),
                                                window,
                                                cx,
                                            );
                                        })
                                        .child(
                                            crate::icons::icon(crate::icons::ARROW_UP_RIGHT)
                                                .size(px(crate::surface_chrome::ICON_SIZE))
                                                .text_color(theme.text_muted),
                                        ),
                                    );
                                }
                                el.into_any_element()
                            }
                            state => {
                                let text = if image.source.starts_with("https://")
                                    || image.source.starts_with("http://")
                                {
                                    format!("{} — {}", image.alt, image.source)
                                } else {
                                    format!(
                                        "{} — {}",
                                        image.alt,
                                        state.and_then(|s| s.as_ref().err()).map_or(
                                            if image_allowed.contains(&image.source) {
                                                "Loading image…"
                                            } else {
                                                "Document image preview limit reached"
                                            },
                                            String::as_str
                                        )
                                    )
                                };
                                let target = image.source.clone();
                                let external =
                                    target.starts_with("https://") || target.starts_with("http://");
                                div()
                                    .id(id)
                                    .text_color(theme.text_muted)
                                    .child(text)
                                    .when(external, |el| {
                                        let link = image_link.clone();
                                        // A keyboard stop with the shell's ring (M8).
                                        el.cursor_pointer()
                                            .role(gpui::Role::Link)
                                            .map(|el| crate::surface_chrome::key_stop(el, theme))
                                            .on_click(move |_, window, cx| {
                                                render::activate_link(
                                                    render::LinkTarget::new(&target, &target),
                                                    render::LinkAction::Primary,
                                                    Some(&link),
                                                    window,
                                                    cx,
                                                );
                                            })
                                    })
                                    .into_any_element()
                            }
                        }
                    })),
                });
                opts.link = Some(link.clone());
                opts.copy = Some(self.copy_ui_for(&opts.row_key, cx));
                Some(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .pb(px(render::MD_BLOCK_GAP))
                        .child(
                            div().w_full().flex().justify_center().px(px(24.0)).child(
                                div()
                                    .w_full()
                                    .max_w(px(MAX_PREVIEW_CONTENT_WIDTH))
                                    .min_w_0()
                                    .relative()
                                    .child(render::render_block(
                                        &top.block,
                                        ix,
                                        ix,
                                        &opts,
                                        &theme,
                                        window,
                                        self.highlights.get(&ix).map(|h| h.lines.as_slice()),
                                    )),
                            ),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for MarkdownPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        if self.needs_focus {
            self.needs_focus = false;
            window.focus(&self.focus, cx);
        }
        self.resize_media(window, cx);
        self.cache.borrow_mut().retain_rows(&std::mem::take(&mut self.visible_rows));
        if self.diagram_style != crate::theme::style_generation() {
            self.load_diagrams(cx);
        }
        if self.media_dirty {
            self.media_dirty = false;
            self.image_snapshot = Rc::new(self.images.clone());
            self.diagram_snapshot = Rc::new(self.diagrams.clone());
        }
        let mut root = div()
            .id("markdown-file-preview")
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .font_family(theme.font_sans.clone())
            .text_color(theme.text)
            .track_focus(&self.focus)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|view, _, window, cx| window.focus(&view.focus, cx)),
            )
            .on_mouse_move(cx.listener(Self::selection_move))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|view, _, _, cx| {
                    if view.owns_selection() {
                        crate::markdown::selection::end_active_drag();
                        view.selection_task = None;
                        view.selection_pointer = None;
                        cx.notify();
                    }
                }),
            )
            .on_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "c"
                    && (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
                    && let Some(text) = crate::markdown::selection::selected_text()
                {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                    cx.stop_propagation();
                }
            }))
            .child(render::selection_surface_reset(self.scope.clone()))
            // One loading voice; the truncation is information, not a warning (M30, M34).
            .when(self.loading, |el| {
                el.child(
                    div()
                        .px(px(24.0))
                        .child(crate::loaders::pending("markdown-preview", "Loading preview…")),
                )
            })
            .when(self.truncated, |el| {
                el.child(super::preview::info_row(
                    &theme,
                    "Large file: the preview shows its start, read-only",
                ))
            })
            .child(
                list(self.list.clone(), cx.processor(Self::render_row))
                    .flex_1()
                    .min_h_0()
                    // Scroll the breathing room with the document so content
                    // clips at the viewport edge, directly below the toolbar.
                    .py(px(PREVIEW_VERTICAL_PADDING))
                    .with_sizing_behavior(ListSizingBehavior::Auto),
            );
        if let Some(preview) = &self.preview_image {
            let weak = cx.weak_entity();
            let display_size = self
                .zoom_source
                .as_ref()
                .map(|source| gpui::size(px(source.width), px(source.height)));
            root = root.child(crate::attachments::lightbox_with_size(
                window,
                preview,
                &self.preview_focus,
                display_size,
                move |window, cx| {
                    let _ = weak.update(cx, |view, cx| {
                        view.close_media_preview(cx);
                        window.focus(&view.focus, cx);
                        cx.notify();
                    });
                },
                cx,
            ));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_retains_mail_links_without_allowing_active_schemes() {
        for (url, allowed) in [
            ("mailto:reader@example.com", true),
            ("https://example.com", true),
            ("javascript:alert(1)", false),
        ] {
            let a = render::LinkActivation {
                target: render::LinkTarget::new("label", url),
                action: render::LinkAction::Internal,
                source_session: None,
            };
            assert_eq!(
                matches!(preview_link_outcome(&a), render::LinkOutcome::External(_)),
                allowed
            );
        }
    }
    #[test]
    fn markdown_paths_and_links() {
        assert!(is_markdown("docs/README.MD"));
        assert!(is_markdown("x.markdown"));
        assert!(!is_markdown("x.mdx"));
        assert_eq!(
            relative_target("docs/readme.md", "../a%20b.md#hello"),
            Some(("a b.md".into(), Some("hello".into())))
        );
        assert_eq!(relative_target("readme.md", "../secret"), None);
        assert_eq!(relative_target("docs/readme.md", "%2Fetc/passwd"), None);
        assert_eq!(relative_target("docs/readme.md", "https://example.com"), None);
        assert_eq!(
            relative_target("docs/readme.md", "#hello"),
            Some(("docs/readme.md".into(), Some("hello".into())))
        );
    }
    #[test]
    fn outside_documents_drop_local_links_and_keep_web_links() {
        let mut tree = parser::parse_full(
            "[local](other.md) [web](https://example.com/) [mail](mailto:reader@example.com) [![pic](shot.png)](gallery.md)",
        );
        for top in &mut tree.blocks {
            strip_outside_links(&mut Arc::make_mut(top).block);
        }
        let Block::Paragraph { runs } = &tree.blocks[0].block else {
            panic!("paragraph");
        };
        let links: Vec<&str> = runs
            .iter()
            // An image run carries its own source in `link`; only real
            // anchors are inspected here.
            .filter(|run| run.style.image.is_none())
            .filter_map(|run| run.style.link.as_deref())
            .collect();
        assert_eq!(links, ["https://example.com/", "mailto:reader@example.com"]);
        let text: String = runs.iter().map(|run| run.text.as_str()).collect();
        assert!(text.contains("local"), "the label stays as plain text: {text}");
        assert!(
            !runs.iter().any(|run| run.style.link.as_deref() == Some("other.md")),
            "a local target must lose its link styling"
        );
        for run in runs {
            let Some(image) = &run.style.image else {
                continue;
            };
            assert_eq!(image.source, "shot.png");
            assert!(image.link.is_none());
        }
    }
}
