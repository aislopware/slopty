// Derived from zeron (MIT, github.com/zeronsh/zeron @ daf047b), see THIRD_PARTY_NOTICES.md
use std::{
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyElement, Context, Entity, FocusHandle, Focusable as _, ListAlignment, ListOffset,
    ListSizingBehavior, ListState, ScrollHandle, SharedString, Window, div, font, list, point,
    prelude::*, px,
};
use gpui_base::input::RopeExt as _;
use slopty_view_proto::{
    ReadWorkspaceFileRequest, WorkspaceReadOnlyReason, WriteWorkspaceFileOutcome,
    WriteWorkspaceFileRequest,
};

use super::{
    FileColumn, FilesCloseDisposition, FilesEvent, FilesSurface,
    client::{FilesRequestContext, WorkspaceFilesClient},
    document::{DocumentKey, DocumentPhase, FileDocument},
    toolbar, toolbar_button, toolbar_toggle_keyed,
};
use crate::{
    icons::{self, icon},
    syntax_cache::{DocumentHighlightKey, SyntaxHighlightCache},
    theme::Theme,
};
use crate::motion::HoverFade as _;

// slopty: one code size for the pane (the diff's, the editor's and a read-only file's), the
// `code_font_size` setting on a 20 px line at its default: opening a file from its diff, or a
// file that cannot be edited, no longer moves its text 1 or 1.5 px.
const PREVIEW_LINE_HEIGHT: f32 = 20.0;
const PREVIEW_LINE_HEIGHT_RATIO: f32 = PREVIEW_LINE_HEIGHT / crate::typography::CODE_FONT_SIZE_DEFAULT;
// A read response is capped at 8 MiB and editable files at 1 MiB. The byte
// budget bounds large previews while the entry cap bounds many tiny editors.
// Protected documents may exceed either limit rather than risk losing work.
const MAX_RETAINED_DOCUMENTS: usize = 16;
const MAX_RETAINED_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

struct HighlightedFile {
    content_hash: String,
    document: Arc<slopty_syntax::HighlightedDocument>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadDecision {
    ReloadNow,
    AwaitDiscardConfirmation,
}

pub(super) struct FilePreviewState {
    images_visible: bool,
    pub(super) documents: HashMap<String, FileDocument>,
    document_recency: VecDeque<String>,
    active: Option<String>,
    highlights: HashMap<String, HighlightedFile>,
    syntax_cache: SyntaxHighlightCache,
    list: ListState,
    horizontal_scroll: ScrollHandle,
    word_wrap: bool,
    editor_font_size: f32,
    autosave_enabled: bool,
    autosave_delay_ms: u64,
    reload_confirmation: Option<String>,
    close_requested: bool,
    typography_generation: u32,
}

impl FilePreviewState {
    pub(super) fn new(
        autosave_enabled: bool,
        autosave_delay_ms: u64,
        word_wrap: bool,
        editor_font_size: f32,
    ) -> Self {
        Self {
            images_visible: true,
            documents: HashMap::new(),
            document_recency: VecDeque::new(),
            active: None,
            highlights: HashMap::new(),
            syntax_cache: SyntaxHighlightCache::default(),
            list: ListState::new(0, ListAlignment::Top, px(520.0)),
            horizontal_scroll: ScrollHandle::new(),
            word_wrap,
            editor_font_size,
            autosave_enabled,
            autosave_delay_ms,
            reload_confirmation: None,
            close_requested: false,
            typography_generation: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.documents.clear();
        self.document_recency.clear();
        self.active = None;
        self.highlights.clear();
        self.list.reset(0);
        self.reload_confirmation = None;
        self.close_requested = false;
    }

    pub(super) fn has_active(&self) -> bool {
        self.active.is_some()
    }

    fn touch_document(&mut self, path: &str) {
        self.document_recency.retain(|candidate| candidate != path);
        self.document_recency.push_back(path.to_string());
    }

    fn retained_document_bytes(&self) -> usize {
        let documents = self.documents.values().fold(0usize, |total, document| {
            total.saturating_add(document.estimated_retained_bytes())
        });
        self.highlights
            .values()
            .fold(documents, |total, highlight| {
                total.saturating_add(estimated_highlighted_file_bytes(highlight))
            })
    }

    fn document_is_evictable(&self, path: &str, protected_paths: &HashSet<String>) -> bool {
        if self.active.as_deref() == Some(path)
            || self.reload_confirmation.as_deref() == Some(path)
            || protected_paths.contains(path)
        {
            return false;
        }
        self.documents.get(path).is_some_and(|document| {
            !document.is_dirty()
                && matches!(
                    document.phase,
                    DocumentPhase::Ready
                        | DocumentPhase::ReadOnly(_)
                        | DocumentPhase::Error(_)
                        | DocumentPhase::DeletedOnDisk
                )
                && document.read_task.is_none()
                && document.highlight_task.is_none()
                && document.autosave_task.is_none()
                && document.save_task.is_none()
                && document.reconcile_task.is_none()
                && document.pending_save.is_none()
                && document.pending_external_reload.is_none()
                && !document.reconcile_after_save
        })
    }

    fn evict_document(&mut self, path: &str) -> bool {
        if self.documents.remove(path).is_none() {
            return false;
        }
        self.document_recency.retain(|candidate| candidate != path);
        self.highlights.remove(path);
        true
    }

    fn trim_document_cache(&mut self, protected_paths: &HashSet<String>) -> Vec<String> {
        self.trim_document_cache_to(
            protected_paths,
            MAX_RETAINED_DOCUMENTS,
            MAX_RETAINED_DOCUMENT_BYTES,
        )
    }

    fn trim_document_cache_to(
        &mut self,
        protected_paths: &HashSet<String>,
        max_documents: usize,
        max_bytes: usize,
    ) -> Vec<String> {
        let mut evicted = Vec::new();
        while self.documents.len() > max_documents || self.retained_document_bytes() > max_bytes {
            let candidate = self
                .document_recency
                .iter()
                .find(|path| self.document_is_evictable(path, protected_paths))
                .cloned()
                .or_else(|| {
                    self.documents
                        .keys()
                        .find(|path| self.document_is_evictable(path, protected_paths))
                        .cloned()
                });
            let Some(candidate) = candidate else {
                break;
            };
            if self.evict_document(&candidate) {
                evicted.push(candidate);
            }
        }
        evicted
    }

    fn word_wrap(&self) -> bool {
        self.word_wrap
    }

    /// Row height for plain (non-editable) preview lines. The list's
    /// uniform-height hint and the painted rows must both read it from here: a
    /// fixed 20 px row clips glyphs at larger code sizes, and a hint that
    /// disagrees with the painted row desyncs the virtualized measurements.
    fn line_height(&self) -> gpui::Pixels {
        px((self.code_text_size() * PREVIEW_LINE_HEIGHT_RATIO).max(PREVIEW_LINE_HEIGHT))
    }

    /// Text size of the editable editor and the plain (non-editable) preview rows alike.
    fn code_text_size(&self) -> f32 {
        crate::typography::clamp_font_size(self.editor_font_size)
    }

    pub(super) fn set_editor_font_size(&mut self, editor_font_size: f32) {
        self.editor_font_size = editor_font_size;
    }

    pub(super) fn set_autosave_delay_ms(&mut self, delay_ms: u64) -> Vec<String> {
        self.autosave_delay_ms = delay_ms;
        let mut pending = Vec::new();
        for (path, document) in &mut self.documents {
            document.autosave_task = None;
            if self.autosave_enabled && document.can_autosave() {
                pending.push(path.clone());
            }
        }
        pending
    }

    pub(super) fn set_autosave_enabled(&mut self, enabled: bool) -> Vec<String> {
        self.autosave_enabled = enabled;
        let mut pending = Vec::new();
        for (path, document) in &mut self.documents {
            document.autosave_task = None;
            if enabled && document.can_autosave() {
                pending.push(path.clone());
            }
        }
        pending
    }

    pub(super) fn has_unsaved_changes(&self) -> bool {
        self.documents.values().any(FileDocument::is_dirty)
    }

    pub(super) fn dirty_paths(&self) -> Vec<String> {
        self.documents
            .iter()
            .filter(|(_, document)| document.is_dirty())
            .map(|(path, _)| path.clone())
            .collect()
    }

    pub(super) fn cancel_autosaves(&mut self) {
        for document in self.documents.values_mut() {
            document.autosave_task = None;
        }
    }

    fn request_reload(&mut self, path: &str) -> ReloadDecision {
        let Some(document) = self.documents.get_mut(path) else {
            return ReloadDecision::ReloadNow;
        };
        if !document.is_dirty() {
            self.reload_confirmation = None;
            return ReloadDecision::ReloadNow;
        }

        // A destructive reload must remain pending until the user explicitly
        // confirms it. Pause delayed autosave so it cannot race the choice.
        document.autosave_task = None;
        self.reload_confirmation = Some(path.to_string());
        ReloadDecision::AwaitDiscardConfirmation
    }

    fn autosave_paused_for_reload(&self, path: &str) -> bool {
        self.reload_confirmation.as_deref() == Some(path)
    }
}

fn estimated_highlighted_file_bytes(highlight: &HighlightedFile) -> usize {
    let document = &highlight.document;
    std::mem::size_of::<HighlightedFile>()
        .saturating_add(highlight.content_hash.capacity())
        .saturating_add(
            document
                .lines
                .capacity()
                .saturating_mul(std::mem::size_of::<Vec<slopty_syntax::HighlightSpan>>()),
        )
        .saturating_add(document.lines.iter().fold(0usize, |total, line| {
            total.saturating_add(
                line.capacity()
                    .saturating_mul(std::mem::size_of::<slopty_syntax::HighlightSpan>()),
            )
        }))
}

fn document_key(context: &FilesRequestContext, path: String) -> DocumentKey {
    DocumentKey {
        chat_id: context.target.chat_id.clone().unwrap_or_default(),
        checkout_id: context.checkout_id.clone(),
        path,
    }
}

fn document_blocks_lifecycle(document: &FileDocument) -> bool {
    document.is_dirty()
        && matches!(
            document.phase,
            DocumentPhase::SaveFailed(_)
                | DocumentPhase::Conflict { .. }
                | DocumentPhase::ExternallyModified { .. }
                | DocumentPhase::DeletedOnDisk
        )
}

fn file_highlight_result_is_current(
    document: &FileDocument,
    document_key: &DocumentKey,
    generation: u64,
    revision: u64,
    content_hash: &str,
) -> bool {
    document.accepts(document_key, generation)
        && document.revision == revision
        && document.content_hash() == Some(content_hash)
}

fn renamed_document_path(path: &str, old_path: &str, new_path: &str) -> Option<String> {
    if path == old_path {
        Some(new_path.to_string())
    } else {
        path.strip_prefix(&format!("{old_path}/"))
            .map(|suffix| format!("{new_path}/{suffix}"))
    }
}

fn path_is_same_or_descendant(path: &str, ancestor: &str) -> bool {
    path == ancestor || path.starts_with(&format!("{ancestor}/"))
}

impl FilesSurface {
    #[cfg(test)]
    pub(crate) fn test_images_visible(&self) -> bool {
        self.preview.images_visible
    }

    pub(crate) fn suspend_images(&mut self, cx: &mut Context<Self>) {
        if !self.preview.images_visible {
            return;
        }
        self.preview.images_visible = false;
        for document in self.preview.documents.values() {
            if let Some(view) = &document.image {
                view.update(cx, |view, cx| view.suspend(cx));
            }
        }
    }

    pub(crate) fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self
            .preview
            .active
            .as_ref()
            .and_then(|path| self.preview.documents.get(path))
            .and_then(|d| d.image.clone())
        {
            let focus = view.read(cx).focus.clone();
            window.defer(cx, move |window, cx| focus.focus(window, cx));
            return;
        }
        if let Some(view) = self
            .preview
            .active
            .as_ref()
            .and_then(|path| self.preview.documents.get(path))
            .filter(|d| d.show_markdown)
            .and_then(|d| d.markdown.clone())
        {
            let focus = view.read(cx).focus.clone();
            window.defer(cx, move |window, cx| focus.focus(window, cx));
            return;
        }
        let Some(editor) = self
            .preview
            .active
            .as_deref()
            .and_then(|path| self.preview.documents.get(path))
            .and_then(|document| document.editor.clone())
        else {
            return;
        };
        let focus = editor.focus_handle(cx);
        // Tab activation remounts this surface after the click handler returns.
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    /// A chat link can arrive before the file and editor have finished loading.
    pub(crate) fn navigate_to_line(
        &mut self,
        line: u32,
        column: Option<FileColumn>,
        cx: &mut Context<Self>,
    ) {
        if line == 0 {
            return;
        }
        self.line_navigation_generation = self.line_navigation_generation.wrapping_add(1);
        self.pending_line_navigation = Some((line, column));
        if let Some(document) = self
            .editor_path
            .as_ref()
            .and_then(|path| self.preview.documents.get_mut(path))
        {
            // Source line numbers do not map to the rendered Markdown blocks.
            if document.show_markdown {
                document.show_markdown = false;
                if let Some(view) = &document.markdown {
                    view.update(cx, |view, cx| view.suspend(cx));
                }
            }
        }
        cx.notify();
    }

    fn apply_line_navigation(
        &mut self,
        path: &str,
        editor: Option<&Entity<super::editor::FileEditorState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((line, column)) = self.pending_line_navigation else {
            return;
        };
        let Some(document) = self.preview.documents.get(path) else {
            return;
        };
        if document
            .file
            .as_ref()
            .and_then(|file| file.text.as_ref())
            .is_none()
        {
            if !matches!(document.phase, DocumentPhase::Loading) {
                self.pending_line_navigation = None;
            }
            return;
        }
        if let Some(editor) = editor {
            editor.update(cx, |state, cx| {
                let row = (line - 1).min(state.text().lines_len().saturating_sub(1) as u32);
                let character = match column {
                    Some(FileColumn::Character(column)) => column.saturating_sub(1),
                    Some(FileColumn::Byte(column)) => {
                        // Convert against the loaded line, not the trimmed and
                        // truncated search preview (which may omit indentation).
                        let text = state.text().slice_line(row as usize);
                        let byte = column.min(text.len() as u64) as usize;
                        text.byte_to_char_idx(byte) as u32
                    }
                    None => 0,
                };
                state.set_cursor_position(
                    gpui_base::input::Position::new(row, character),
                    window,
                    cx,
                );
            });
        }
        self.pending_line_navigation = None;
        self.schedule_line_center(line, self.line_navigation_generation, 16, window, cx);
    }

    fn schedule_line_center(
        &self,
        line: u32,
        generation: u64,
        retries: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let retry = owner
                .update(cx, |surface, cx| {
                    surface.line_navigation_generation == generation
                        && !surface.center_active_line(line, window, cx)
                })
                .unwrap_or(false);
            if retry && retries > 0 {
                owner
                    .update(cx, |surface, cx| {
                        surface.schedule_line_center(line, generation, retries - 1, window, cx);
                        cx.notify();
                    })
                    .ok();
            }
        });
    }

    fn center_active_line(
        &mut self,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(document) = self
            .preview
            .active
            .as_deref()
            .and_then(|path| self.preview.documents.get(path))
        else {
            return true;
        };
        if let Some(editor) = document.editor.clone() {
            let target = {
                let state = editor.read(cx);
                let cursor = state.cursor();
                let bounds = state.range_to_bounds(&(cursor..cursor));
                let viewport = state.input_bounds();
                bounds
                    .filter(|_| viewport.size.height > px(0.0))
                    .map(|bounds| {
                        (
                            state.scroll_offset(),
                            viewport.origin.y + viewport.size.height / 2.0,
                            bounds.origin.y + bounds.size.height / 2.0,
                        )
                    })
            };
            let Some((offset, target_y, line_y)) = target else {
                // A newly mounted editor has no layout when the cursor is first
                // placed. Repeat the move after layout so it can scroll there.
                let position = editor.read(cx).cursor_position();
                editor.update(cx, |state, cx| {
                    state.set_cursor_position(position, window, cx);
                });
                return false;
            };
            // The editor paints this offset before clamping it, so a line near
            // the top would briefly push the first line down the viewport.
            let y = (offset.y + target_y - line_y).min(px(0.0));
            editor.update(cx, |state, cx| {
                state.set_scroll_offset(point(offset.x, y), cx);
            });
            return true;
        }

        // Large or read-only text files use the plain virtual list, not EditorState.
        let viewport = self.preview.list.viewport_bounds();
        if viewport.size.height <= px(0.0) {
            return false;
        }
        let row = (line as usize)
            .saturating_sub(1)
            .min(document.lines.len().saturating_sub(1));
        let Some(bounds) = self.preview.list.bounds_for_item(row) else {
            self.preview.list.scroll_to(ListOffset {
                item_ix: row,
                offset_in_item: px(0.0),
            });
            cx.notify();
            return false;
        };
        let viewport_center = viewport.origin.y + viewport.size.height / 2.0;
        let row_center = bounds.origin.y + bounds.size.height / 2.0;
        self.preview.list.scroll_by(row_center - viewport_center);
        cx.notify();
        true
    }

    /// ⇧⌘V and the header's eye: the Markdown preview or its source.
    pub(super) fn toggle_markdown_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.preview.active.clone() else {
            return;
        };
        if !super::markdown_preview::is_markdown(&path) {
            return;
        }
        if let Some(document) = self.preview.documents.get_mut(&path) {
            document.show_markdown = !document.show_markdown;
            if !document.show_markdown {
                if let Some(view) = &document.markdown {
                    view.update(cx, |view, cx| view.suspend(cx));
                }
            }
        }
        if self
            .preview
            .documents
            .get(&path)
            .is_some_and(|d| !d.show_markdown)
        {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    /// Where the header's keys resolve: whatever holds focus in the open document (its image,
    /// its Markdown preview or its editor), else the tree.
    pub(super) fn keys_focus(&self, cx: &gpui::App) -> FocusHandle {
        let document = self
            .preview
            .active
            .as_ref()
            .and_then(|path| self.preview.documents.get(path));
        if let Some(view) = document.and_then(|d| d.image.clone()) {
            return view.read(cx).focus.clone();
        }
        if let Some(view) = document.filter(|d| d.show_markdown).and_then(|d| d.markdown.clone()) {
            return view.read(cx).focus.clone();
        }
        if let Some(editor) = document.and_then(|d| d.editor.clone()) {
            return editor.focus_handle(cx);
        }
        self.tree_focus.clone()
    }

    pub(super) fn toggle_word_wrap(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let word_wrap = !self.preview.word_wrap;
        cx.emit(FilesEvent::WordWrapChanged(word_wrap));
    }

    pub(super) fn apply_word_wrap(
        &mut self,
        word_wrap: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preview.word_wrap == word_wrap {
            return;
        }
        self.preview.word_wrap = word_wrap;
        self.preview.list.remeasure();
        let editors = self
            .preview
            .documents
            .values()
            .filter_map(|document| document.editor.clone())
            .collect::<Vec<_>>();
        for editor in editors {
            editor.update(cx, |state, cx| state.set_soft_wrap(word_wrap, window, cx));
        }
        cx.notify();
    }

    fn trim_document_cache(&mut self) {
        let mut protected_paths = HashSet::new();
        if let Some(path) = self.editor_path.as_ref() {
            protected_paths.insert(path.clone());
        }
        for path in self.preview.trim_document_cache(&protected_paths) {
            tracing::debug!(path = %path, "evicted inactive workspace document");
        }
    }

    pub(super) fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        if self
            .preview
            .active
            .as_ref()
            .is_some_and(|active| active != &path)
        {
            self.suspend_images(cx);
            if let Some(view) = self
                .preview
                .active
                .as_ref()
                .and_then(|active| self.preview.documents.get(active))
                .and_then(|d| d.markdown.clone())
            {
                view.update(cx, |view, cx| view.suspend(cx));
            }
        }

        let leaving_reload_confirmation = self
            .preview
            .reload_confirmation
            .as_deref()
            .is_some_and(|pending| pending != path);
        if leaving_reload_confirmation {
            self.cancel_reload_confirmation(cx);
        }
        self.preview.active = Some(path.clone());
        self.preview.touch_document(&path);
        if !self.preview.documents.contains_key(&path) {
            let Some(context) = self.request_context.as_ref() else {
                return;
            };
            self.preview.documents.insert(
                path.clone(),
                FileDocument::loading(document_key(context, path.clone())),
            );
            self.read_file(path, cx);
        } else if self.preview.documents.get(&path).is_some_and(|d| {
            d.image.is_none()
                && (super::image_preview::is_image(&path)
                    || (d.file.is_none() && d.read_task.is_none()))
        }) {
            self.read_file(path, cx);
        } else {
            self.sync_preview_list();
        }
        self.trim_document_cache();
        cx.notify();
    }

    fn read_file(&mut self, path: String, cx: &mut Context<Self>) {
        if super::image_preview::is_image(&path) {
            self.read_image_file(path, cx);
            return;
        }
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            let starting = self.starting_note(cx);
            if let Some(document) = self.preview.documents.get_mut(&path) {
                document.phase = DocumentPhase::Error(starting.into());
            }
            return;
        };
        let Some(document) = self.preview.documents.get_mut(&path) else {
            return;
        };
        let key = document_key(&context, path.clone());
        document.key = key.clone();
        let generation = document.begin_load();
        let request = ReadWorkspaceFileRequest {
            target: context.target.clone(),
            path: path.clone(),
        };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let task_path = path.clone();
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut result = client.read_file(request.clone()).await;
            if result.as_ref().is_err_and(|error| error.retryable()) {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                result = client.read_file(request).await;
            }
            let _ = this.update(cx, |surface, cx| {
                if surface.request_context.as_ref() != Some(&context) {
                    return;
                }
                let Some(document) = surface.preview.documents.get_mut(&task_path) else {
                    return;
                };
                if !document.accepts(&task_key, generation) {
                    return;
                }
                document.read_task = None;
                match result {
                    Ok(file) => {
                        let highlight = file
                            .text
                            .as_ref()
                            .zip(file.content_hash.as_ref())
                            .map(|(source, hash)| (source.clone(), hash.clone()));
                        document.set_loaded(file);
                        surface.sync_preview_list();
                        if let Some((source, hash)) = highlight {
                            surface.request_file_highlight(task_path.clone(), source, hash, cx);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(path = %task_path, error = %error, "workspace file load failed");
                        document.set_error(read_error_message(&error));
                        surface.sync_preview_list();
                    }
                }
                surface.trim_document_cache();
                cx.notify();
            });
        });
        if let Some(document) = self.preview.documents.get_mut(&path)
            && document.accepts(&key, generation)
        {
            document.read_task = Some(task);
        }
        self.sync_preview_list();
        cx.notify();
    }

    fn read_image_file(&mut self, path: String, cx: &mut Context<Self>) {
        // A rename can give an edited text buffer an image extension. Never
        // discard that buffer (or an in-flight save) to create a preview.
        if self.preview.documents.get(&path).is_some_and(|d| {
            d.is_dirty() || d.pending_save.is_some() || matches!(d.phase, DocumentPhase::Saving)
        }) {
            return;
        }
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            let starting = self.starting_note(cx);
            if let Some(document) = self.preview.documents.get_mut(&path) {
                document.set_error(starting.as_str());
            }
            return;
        };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let view =
            cx.new(|cx| super::image_preview::ImagePreview::new(path.clone(), context, client, cx));
        if !self.preview.images_visible {
            view.update(cx, |view, cx| view.suspend(cx));
        }
        if let Some(document) = self.preview.documents.get_mut(&path) {
            document.begin_load();
            document.image = Some(view);
            document.file = None;
            document.editor = None;
            document.editor_events = None;
            document.editor_observer = None;
            document.lines = Arc::new(Vec::new());
            document.phase = DocumentPhase::ReadOnly(WorkspaceReadOnlyReason::Binary);
        }
        cx.notify();
    }

    fn request_file_highlight(
        &mut self,
        path: String,
        source: String,
        content_hash: String,
        cx: &mut Context<Self>,
    ) {
        let Some(language) = slopty_syntax::language_for_path(&path) else {
            return;
        };
        let Some((document_key, generation, revision)) = self
            .preview
            .documents
            .get(&path)
            .map(|document| (document.key.clone(), document.generation, document.revision))
        else {
            return;
        };
        let key = DocumentHighlightKey::new(language, &source);
        if let Some(document) = self.preview.syntax_cache.get(&key) {
            if let Some(editor) = self
                .preview
                .documents
                .get(&path)
                .and_then(|file| file.editor.clone())
            {
                super::editor_adapter::install_highlighter(&editor, source, document.clone(), cx);
            }
            self.preview.highlights.insert(
                path,
                HighlightedFile {
                    content_hash,
                    document,
                },
            );
            cx.notify();
            return;
        }
        let highlight_path = path.clone();
        let task_document_key = document_key.clone();
        let source_for_install = source.clone();
        let task = cx.spawn(async move |this, cx| {
            let request_path = highlight_path.clone();
            let highlighted = cx
                .background_executor()
                .spawn(async move {
                    slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                        source: &source,
                        path: Some(&request_path),
                        fence_tag: None,
                    })
                    .ok()
                    .map(Arc::new)
                })
                .await;
            let _ = this.update(cx, |surface, cx| {
                let still_current = surface
                    .preview
                    .documents
                    .get_mut(&highlight_path)
                    .is_some_and(|document| {
                        let current = file_highlight_result_is_current(
                            document,
                            &task_document_key,
                            generation,
                            revision,
                            &content_hash,
                        );
                        if current {
                            document.highlight_task = None;
                        }
                        current
                    });
                if !still_current {
                    return;
                }
                if let Some(document) = highlighted {
                    surface.preview.syntax_cache.insert(key, document.clone());
                    if let Some(editor) = surface
                        .preview
                        .documents
                        .get(&highlight_path)
                        .and_then(|file| file.editor.clone())
                    {
                        super::editor_adapter::install_highlighter(
                            &editor,
                            source_for_install,
                            document.clone(),
                            cx,
                        );
                    }
                    surface.preview.highlights.insert(
                        highlight_path.clone(),
                        HighlightedFile {
                            content_hash,
                            document,
                        },
                    );
                    cx.notify();
                }
                surface.trim_document_cache();
            });
        });
        if let Some(document) = self.preview.documents.get_mut(&path)
            && document.accepts(&document_key, generation)
        {
            document.highlight_task = Some(task);
        }
    }

    pub(super) fn on_editor_change(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(editor) = self
            .preview
            .documents
            .get(path)
            .and_then(|document| document.editor.clone())
        else {
            return;
        };
        let source = editor.read(cx).value().to_string();
        let revision = {
            let Some(document) = self.preview.documents.get_mut(path) else {
                return;
            };
            document.mark_user_edit();
            document.revision
        };
        self.request_editor_highlight(path.to_string(), source, revision, cx);
        self.schedule_autosave(path.to_string(), cx);
        cx.emit(FilesEvent::TitleChanged);
        cx.notify();
    }

    fn request_editor_highlight(
        &mut self,
        path: String,
        source: String,
        revision: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(language) = slopty_syntax::language_for_path(&path) else {
            return;
        };
        let Some((document_key, generation)) = self
            .preview
            .documents
            .get(&path)
            .map(|document| (document.key.clone(), document.generation))
        else {
            return;
        };
        let cache_key = DocumentHighlightKey::new(language, &source);
        let task_path = path.clone();
        let task_document_key = document_key.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let request_path = task_path.clone();
            let source_for_parse = source.clone();
            let highlighted = cx
                .background_executor()
                .spawn(async move {
                    slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                        source: &source_for_parse,
                        path: Some(&request_path),
                        fence_tag: None,
                    })
                    .ok()
                    .map(Arc::new)
                })
                .await;
            let _ = this.update(cx, |surface, cx| {
                let Some(document) = surface.preview.documents.get_mut(&task_path) else {
                    return;
                };
                if !document.accepts(&task_document_key, generation)
                    || document.revision != revision
                {
                    return;
                }
                document.highlight_task = None;
                let Some(highlighted) = highlighted else {
                    surface.trim_document_cache();
                    return;
                };
                let editor = document.editor.clone();
                surface
                    .preview
                    .syntax_cache
                    .insert(cache_key, highlighted.clone());
                if let Some(editor) = editor {
                    super::editor_adapter::install_highlighter(&editor, source, highlighted, cx);
                }
                surface.trim_document_cache();
                cx.notify();
            });
        });
        if let Some(document) = self.preview.documents.get_mut(&path)
            && document.accepts(&document_key, generation)
        {
            document.highlight_task = Some(task);
        }
    }

    pub(super) fn schedule_autosave(&mut self, path: String, cx: &mut Context<Self>) {
        if self.mutation_blocks_path(&path)
            || !self.preview.autosave_enabled
            || self.preview.autosave_paused_for_reload(&path)
        {
            return;
        }
        let delay = Duration::from_millis(self.preview.autosave_delay_ms);
        let Some((key, generation, revision)) = self
            .preview
            .documents
            .get(&path)
            .filter(|document| document.can_autosave())
            .map(|document| (document.key.clone(), document.generation, document.revision))
        else {
            return;
        };
        let task_path = path.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |surface, cx| {
                let still_current = surface.preview.autosave_enabled
                    && !surface.preview.autosave_paused_for_reload(&task_path)
                    && surface
                        .preview
                        .documents
                        .get(&task_path)
                        .is_some_and(|document| {
                            document.accepts(&key, generation)
                                && document.revision == revision
                                && document.can_autosave()
                        });
                if still_current {
                    surface.save_document(task_path, cx);
                }
            });
        });
        if let Some(document) = self.preview.documents.get_mut(&path) {
            document.autosave_task = Some(task);
        }
    }

    pub(super) fn save_document(&mut self, path: String, cx: &mut Context<Self>) {
        if self.mutation_blocks_path(&path) || self.target_change_pending {
            return;
        }
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            if let Some(document) = self.preview.documents.get_mut(&path) {
                document.autosave_task = None;
                document.phase =
                    DocumentPhase::SaveFailed("Workspace service is unavailable.".into());
            }
            cx.notify();
            return;
        };
        let Some(editor) = self
            .preview
            .documents
            .get(&path)
            .and_then(|document| document.editor.clone())
        else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        let Some((key, generation, pending)) =
            self.preview.documents.get_mut(&path).and_then(|document| {
                let key = document.key.clone();
                let generation = document.generation;
                document
                    .begin_save(text)
                    .map(|pending| (key, generation, pending))
            })
        else {
            return;
        };
        let request = WriteWorkspaceFileRequest {
            expected_checkout_id: pending.expected_checkout_id.clone(),
            target: context.target.clone(),
            path: path.clone(),
            text: pending.text.clone(),
            expected_content_hash: pending.expected_content_hash.clone(),
            encoding: pending.encoding,
            line_ending: pending.line_ending,
        };
        let revision = pending.revision;
        let task_path = path.clone();
        let task_key = key.clone();
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let task = cx.spawn(async move |this, cx| {
            let result = client.write_file(request).await;
            let _ = this.update(cx, |surface, cx| {
                if surface.request_context.as_ref() != Some(&context) {
                    return;
                }
                let Some(document) = surface.preview.documents.get_mut(&task_path) else {
                    return;
                };
                if !document.accepts(&task_key, generation) {
                    return;
                }
                match result {
                    Ok(WriteWorkspaceFileOutcome::Written { file }) => {
                        let hash = file.content_hash.clone();
                        if document.finish_save(revision, hash.clone())
                            && let Some(loaded) = document.file.as_mut()
                        {
                            loaded.content_hash = Some(hash);
                            loaded.size = file.size;
                            loaded.modified_at = file.modified_at;
                        }
                    }
                    Ok(WriteWorkspaceFileOutcome::Conflict {
                        current_content_hash,
                        ..
                    }) => {
                        tracing::warn!(path = %task_path, "workspace file save conflicted");
                        document.conflict_save(revision, current_content_hash);
                    }
                    Err(error) => {
                        tracing::warn!(path = %task_path, error = %error, "workspace file save failed");
                        document.fail_save(revision, error.to_string());
                    }
                }
                let save_again = document.can_autosave();
                let reconcile = document.reconcile_after_save;
                document.reconcile_after_save = false;
                if save_again {
                    if surface.preview.close_requested || surface.target_change_pending {
                        surface.save_document(task_path.clone(), cx);
                    } else {
                        surface.schedule_autosave(task_path.clone(), cx);
                    }
                }
                if reconcile {
                    surface.reconcile_document(task_path.clone(), cx);
                }
                surface.finish_pending_lifecycle(cx);
                surface.trim_document_cache();
                cx.emit(FilesEvent::TitleChanged);
                cx.notify();
            });
        });
        if let Some(document) = self.preview.documents.get_mut(&path)
            && document.accepts(&key, generation)
        {
            document.save_task = Some(task);
        }
        cx.notify();
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.preview.has_unsaved_changes()
    }

    pub fn prepare_close(&mut self, cx: &mut Context<Self>) -> FilesCloseDisposition {
        let dirty_paths = self.preview.dirty_paths();
        if dirty_paths.is_empty() {
            self.suspend_images(cx);
            return FilesCloseDisposition::Allow;
        }
        self.preview.close_requested = true;
        let blocked = dirty_paths.iter().any(|path| {
            self.preview
                .documents
                .get(path)
                .is_some_and(document_blocks_lifecycle)
        });
        for path in dirty_paths {
            if self
                .preview
                .documents
                .get(&path)
                .is_some_and(FileDocument::can_autosave)
            {
                self.save_document(path, cx);
            }
        }
        cx.notify();
        if blocked {
            FilesCloseDisposition::Blocked
        } else {
            FilesCloseDisposition::Pending
        }
    }

    fn retry_pending_close(&mut self, cx: &mut Context<Self>) {
        let paths = self.preview.dirty_paths();
        for path in paths {
            if self
                .preview
                .documents
                .get(&path)
                .is_some_and(FileDocument::can_save)
            {
                self.save_document(path, cx);
            }
        }
    }

    fn keep_open(&mut self, cx: &mut Context<Self>) {
        self.preview.close_requested = false;
        cx.emit(FilesEvent::CloseCancelled);
        cx.notify();
    }

    fn discard_changes_and_close(&mut self, cx: &mut Context<Self>) {
        let closing = self.preview.close_requested;
        for document in self.preview.documents.values_mut() {
            document.discard_changes();
        }
        self.preview.close_requested = false;
        if closing {
            cx.emit(FilesEvent::CloseReady);
        } else if self.target_change_pending {
            self.apply_pending_target(cx);
        } else {
            cx.emit(FilesEvent::CloseReady);
        }
        cx.emit(FilesEvent::TitleChanged);
        cx.notify();
    }

    fn finish_pending_lifecycle(&mut self, cx: &mut Context<Self>) {
        if self.preview.close_requested && !self.preview.has_unsaved_changes() {
            self.preview.close_requested = false;
            cx.emit(FilesEvent::CloseReady);
        }
        if self.target_change_pending && !self.preview.has_unsaved_changes() {
            self.apply_pending_target(cx);
        }
    }

    pub(super) fn reconcile_document(&mut self, path: String, cx: &mut Context<Self>) {
        if super::image_preview::is_image(&path)
            && !self.preview.documents.get(&path).is_some_and(|d| {
                d.is_dirty() || d.pending_save.is_some() || matches!(d.phase, DocumentPhase::Saving)
            })
        {
            if self.preview.documents.contains_key(&path) {
                if self.preview.images_visible && self.preview.active.as_deref() == Some(&path) {
                    self.read_image_file(path, cx);
                } else if let Some(view) = self
                    .preview
                    .documents
                    .get(&path)
                    .and_then(|d| d.image.clone())
                {
                    view.update(cx, |view, cx| view.suspend(cx));
                }
            }
            return;
        }
        let Some(context) = self.request_context.clone() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        let Some(document) = self.preview.documents.get_mut(&path) else {
            return;
        };
        if matches!(document.phase, DocumentPhase::Saving) {
            document.reconcile_after_save = true;
            return;
        }
        let key = document.key.clone();
        let generation = document.generation;
        let request = ReadWorkspaceFileRequest {
            target: context.target.clone(),
            path: path.clone(),
        };
        let client = WorkspaceFilesClient::new(engine, context.clone());
        let task_path = path.clone();
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let started_at = crate::clock::stopwatch();
            let result = client.read_file(request).await;
            tracing::trace!(
                path = %task_path,
                elapsed_ms = crate::clock::spent(started_at).as_millis(),
                success = result.is_ok(),
                error = ?result.as_ref().err(),
                "workspace document reconciliation read completed"
            );
            let _ = this.update(cx, |surface, cx| {
                if surface.request_context.as_ref() != Some(&context) {
                    return;
                }
                let Some(document) = surface.preview.documents.get_mut(&task_path) else {
                    return;
                };
                if !document.accepts(&task_key, generation) {
                    return;
                }
                if matches!(document.phase, DocumentPhase::Saving) {
                    document.reconcile_after_save = true;
                    return;
                }
                document.reconcile_task = None;
                let Ok(file) = result else {
                    return;
                };
                if file.content_hash == document.saved_hash {
                    let recovered = if matches!(document.phase, DocumentPhase::DeletedOnDisk) {
                        document.restore_on_disk(file);
                        true
                    } else if matches!(document.phase, DocumentPhase::ExternallyModified { .. }) {
                        document.phase = DocumentPhase::Ready;
                        true
                    } else {
                        false
                    };
                    if document.can_autosave() {
                        surface.schedule_autosave(task_path.clone(), cx);
                    }
                    if recovered {
                        cx.notify();
                    }
                    return;
                }
                if document.is_dirty() {
                    document.mark_external(file.content_hash);
                } else {
                    document.queue_external_reload(file);
                }
                cx.notify();
            });
        });
        if let Some(document) = self.preview.documents.get_mut(&path)
            && document.accepts(&key, generation)
        {
            document.reconcile_task = Some(task);
        }
    }

    pub(super) fn reconcile_open_documents(&mut self, cx: &mut Context<Self>) {
        let paths = self.preview.documents.keys().cloned().collect::<Vec<_>>();
        for path in paths {
            self.reconcile_document(path, cx);
        }
    }

    pub(super) fn reconcile_created_documents(&mut self, path: &str, cx: &mut Context<Self>) {
        let paths = self
            .preview
            .documents
            .keys()
            .filter(|document_path| path_is_same_or_descendant(document_path, path))
            .cloned()
            .collect::<Vec<_>>();
        for path in paths {
            self.reconcile_document(path, cx);
        }
    }

    pub(super) fn invalidate_markdown_images(
        &mut self,
        path: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        for document in self.preview.documents.values() {
            if let Some(view) = &document.markdown {
                view.update(cx, |view, cx| view.invalidate_images(path, cx));
            }
        }
    }

    pub(super) fn mark_document_deleted(&mut self, path: &str, cx: &mut Context<Self>) {
        let mut changed = false;
        for (document_path, document) in &mut self.preview.documents {
            if path_is_same_or_descendant(document_path, path) {
                document.mark_deleted();
                if let Some(view) = &document.image {
                    view.update(cx, |view, cx| view.deleted(cx));
                }
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    pub(super) fn rename_documents(
        &mut self,
        old_path: &str,
        new_path: String,
        cx: &mut Context<Self>,
    ) -> Vec<(String, String)> {
        let renames = self
            .preview
            .documents
            .keys()
            .filter_map(|path| {
                renamed_document_path(path, old_path, &new_path)
                    .map(|renamed| (path.clone(), renamed))
            })
            .collect::<Vec<_>>();
        let image_renames: HashSet<_> = renames
            .iter()
            .filter(|(old, new)| {
                super::image_preview::is_image(old) || super::image_preview::is_image(new)
            })
            .map(|(_, new)| new.clone())
            .collect();
        for (old_document_path, new_document_path) in &renames {
            let Some(mut document) = self.preview.documents.remove(old_document_path) else {
                continue;
            };
            if let Some(view) = document.image.take() {
                view.update(cx, |view, cx| view.suspend(cx));
            }
            document.generation = document.generation.wrapping_add(1);
            let needs_review =
                document.is_dirty() || matches!(document.phase, DocumentPhase::Saving);
            document.read_task = None;
            document.highlight_task = None;
            document.autosave_task = None;
            document.save_task = None;
            document.reconcile_task = None;
            document.pending_save = None;
            document.key.path = new_document_path.clone();
            let was_markdown = super::markdown_preview::is_markdown(old_document_path);
            let is_markdown = super::markdown_preview::is_markdown(new_document_path);
            if was_markdown != is_markdown {
                document.show_markdown = is_markdown;
                if let Some(view) = document.markdown.take() {
                    view.update(cx, |view, cx| view.suspend(cx));
                }
            }
            if let Some(file) = document.file.as_mut() {
                file.path = new_document_path.clone();
            }
            if let Some(editor) = document.editor.clone() {
                // Retarget the existing editor so selection and undo survive.
                // Its previous adapter captures parsed spans for the old language.
                editor.update(cx, |editor, cx| {
                    editor.set_highlighter(new_document_path.clone(), cx);
                    editor.set_highlighter_factory(Rc::new(|_| None), cx);
                });
                let event_path = new_document_path.clone();
                document.editor_events =
                    Some(super::editor::subscribe_to_changes(&editor, event_path, cx));
            }
            if needs_review {
                document.mark_external(None);
            }
            if self.preview.active.as_deref() == Some(old_document_path) {
                self.preview.active = Some(new_document_path.clone());
            }
            if self.editor_path.as_deref() == Some(old_document_path) {
                self.editor_path = Some(new_document_path.clone());
            }
            self.preview.highlights.remove(old_document_path);
            self.preview.document_recency.retain(|recent_path| {
                recent_path != old_document_path && recent_path != new_document_path
            });
            self.preview
                .document_recency
                .push_back(new_document_path.clone());
            self.preview
                .documents
                .insert(new_document_path.clone(), document);
        }
        for (_, path) in &renames {
            if image_renames.contains(path)
                && self.preview.active.as_deref() == Some(path)
                && !self
                    .preview
                    .documents
                    .get(path)
                    .is_some_and(FileDocument::is_dirty)
            {
                self.read_file(path.clone(), cx);
            } else if let Some(document) = self.preview.documents.get(path) {
                if let Some(editor) = &document.editor {
                    let source = editor.read(cx).value().to_string();
                    self.request_editor_highlight(path.clone(), source, document.revision, cx);
                } else if let Some((text, hash)) = document
                    .file
                    .as_ref()
                    .and_then(|file| file.text.clone().zip(file.content_hash.clone()))
                {
                    self.request_file_highlight(path.clone(), text, hash, cx);
                }
            }
        }
        if !renames.is_empty() {
            cx.notify();
        }
        renames
    }

    fn apply_pending_external_reload(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self
            .preview
            .documents
            .get_mut(path)
            .and_then(|document| document.pending_external_reload.take())
        else {
            return;
        };
        let text = file.text.clone();
        let hash = file.content_hash.clone();
        let editor = self
            .preview
            .documents
            .get(path)
            .and_then(|document| document.editor.clone());
        if let (Some(editor), Some(text)) = (editor, text.clone()) {
            super::editor::replace_file_contents(&editor, text, window, cx);
        }
        if let Some(document) = self.preview.documents.get_mut(path) {
            document.apply_external_reload(file);
        }
        self.sync_preview_list();
        if let (Some(text), Some(hash)) = (text, hash) {
            self.request_file_highlight(path.to_string(), text, hash, cx);
        }
    }

    fn sync_preview_list(&self) {
        let count = self
            .preview
            .active
            .as_deref()
            .and_then(|path| self.preview.documents.get(path))
            .map(|document| document.lines.len())
            .unwrap_or(0);
        self.preview
            .list
            .reset_with_uniform_height(count, self.preview.line_height());
    }

    fn request_reload_active_document(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.active.clone() else {
            return;
        };
        match self.preview.request_reload(&path) {
            ReloadDecision::ReloadNow => self.read_file(path, cx),
            ReloadDecision::AwaitDiscardConfirmation => cx.notify(),
        }
    }

    fn confirm_reload_active_document(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.active.clone() else {
            return;
        };
        if self.preview.reload_confirmation.as_deref() != Some(path.as_str()) {
            return;
        }
        self.preview.reload_confirmation = None;
        self.read_file(path, cx);
    }

    fn cancel_reload_confirmation(&mut self, cx: &mut Context<Self>) {
        let pending = self.preview.reload_confirmation.take();
        if let Some(path) = pending
            && self
                .preview
                .documents
                .get(&path)
                .is_some_and(FileDocument::can_autosave)
        {
            self.schedule_autosave(path, cx);
        }
        cx.notify();
    }

    fn keep_external_edits(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.preview.active.clone() else {
            return;
        };
        if let Some(document) = self.preview.documents.get_mut(&path)
            && let DocumentPhase::ExternallyModified { disk_hash } = &document.phase
        {
            document.phase = DocumentPhase::Conflict {
                disk_hash: disk_hash.clone(),
            };
        }
        self.preview.reload_confirmation = None;
        cx.notify();
    }

    pub fn save_active_document(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.preview.active.clone() {
            self.save_document(path, cx);
        }
    }

    pub(super) fn render_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let typography_generation = crate::typography::generation(cx);
        if self.preview.typography_generation != typography_generation {
            self.preview.typography_generation = typography_generation;
            // Row heights are cached per item, including virtualized rows off
            // screen, so a code-size change leaves stale geometry behind.
            // `remeasure` re-derives them while holding the scroll position.
            self.preview.list.remeasure();
        }
        let Some(active) = self.preview.active.clone() else {
            return gpui::Empty.into_any_element();
        };
        let external = self.preview.documents.get(&active).is_some_and(|document| {
            matches!(
                document.phase,
                DocumentPhase::ExternallyModified { .. } | DocumentPhase::Conflict { .. }
            )
        });
        let confirming_reload = self.preview.reload_confirmation.as_deref() == Some(&active);
        let lifecycle_pending = self.preview.close_requested || self.target_change_pending;
        let lifecycle_blocked = lifecycle_pending
            && self
                .preview
                .documents
                .values()
                .any(document_blocks_lifecycle);
        let body = self.render_document_body(&active, &theme, window, cx);
        div()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when(lifecycle_pending, |element| {
                element.child(
                    div()
                        .min_h(px(32.0))
                        .py(px(6.0))
                        .flex_none()
                        .px(px(10.0))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.0))
                        .border_b_1()
                        .border_color(theme.warning.opacity(0.25))
                        .bg(theme.warning.opacity(0.055))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.warning_muted)
                        .child(div().min_w_0().max_w_full().whitespace_normal().child(
                            if self.target_change_pending {
                                "The folder changed. Switch back to save, or discard these edits."
                            } else if lifecycle_blocked {
                                "Changes could not be saved safely."
                            } else {
                                "Saving changes before closing…"
                            },
                        ))
                        .when(lifecycle_blocked || self.target_change_pending, |banner| {
                            banner
                                .child(
                                    div()
                                        .id("files-retry-close-save")
                                        .role(gpui::Role::Button)
                                        .rounded(px(4.0))
                                        // slopty: a keyboard stop with the shell's ring (M8).
                                        .map(|el| crate::surface_chrome::key_stop(el, &theme))
                                        .when(self.target_change_pending, |button| button.hidden())
                                        .ml_auto()
                                        .cursor_pointer()
                                        .text_color(theme.text)
                                        .child("Try again")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.retry_pending_close(cx)
                                        })),
                                )
                                .when(!self.target_change_pending, |banner| {
                                    banner.child(
                                        div()
                                            .id("files-keep-open")
                                            .role(gpui::Role::Button)
                                            .rounded(px(4.0))
                                            // slopty: a keyboard stop with the shell's ring (M8).
                                            .map(|el| crate::surface_chrome::key_stop(el, &theme))
                                            .cursor_pointer()
                                            .text_color(theme.text_muted)
                                            .child("Keep open")
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.keep_open(cx)),
                                            ),
                                    )
                                })
                                .child(
                                    div()
                                        .id("files-discard-close-changes")
                                        .role(gpui::Role::Button)
                                        .rounded(px(4.0))
                                        // slopty: a keyboard stop with the shell's ring (M8).
                                        .map(|el| crate::surface_chrome::key_stop(el, &theme))
                                        .cursor_pointer()
                                        .text_color(theme.danger_muted)
                                        .child("Discard changes")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.discard_changes_and_close(cx)
                                        })),
                                )
                        }),
                )
            })
            .when(
                (external || confirming_reload) && !lifecycle_pending,
                |element| {
                    element.child(
                        div()
                            .min_h(px(32.0))
                            .py(px(6.0))
                            .flex_none()
                            .px(px(10.0))
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.0))
                            .border_b_1()
                            .border_color(theme.warning.opacity(0.25))
                            .bg(theme.warning.opacity(0.055))
                            .text_size(crate::typography::ui_rems(11.0))
                            .text_color(theme.warning_muted)
                            .child(div().min_w_0().max_w_full().whitespace_normal().child(
                                if confirming_reload {
                                    "Discard unsaved changes?"
                                } else {
                                    "This file changed outside Slopty."
                                },
                            ))
                            .child(
                                div()
                                    .ml_auto()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap(px(8.0))
                                    .when(!confirming_reload, |actions| {
                                        actions
                                            .child(
                                                div()
                                                    .id("files-keep-external-edits")
                                                    .role(gpui::Role::Button)
                                                    .rounded(px(4.0))
                                                    // slopty: a keyboard stop with the shell's ring (M8).
                                                    .map(|el| crate::surface_chrome::key_stop(el, &theme))
                                                    .cursor_pointer()
                                                    .text_color(theme.text_muted)
                                                    .child("Keep editing")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.keep_external_edits(cx)
                                                    })),
                                            )
                                            .child(
                                                div()
                                                    .id("files-reload-external")
                                                    .role(gpui::Role::Button)
                                                    .rounded(px(4.0))
                                                    // slopty: a keyboard stop with the shell's ring (M8).
                                                    .map(|el| crate::surface_chrome::key_stop(el, &theme))
                                                    .cursor_pointer()
                                                    .text_color(theme.text)
                                                    .child("Reload from disk")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.request_reload_active_document(cx)
                                                    })),
                                            )
                                    })
                                    .when(confirming_reload, |actions| {
                                        actions
                                            .child(
                                                div()
                                                    .id("files-cancel-reload")
                                                    .role(gpui::Role::Button)
                                                    .rounded(px(4.0))
                                                    // slopty: a keyboard stop with the shell's ring (M8).
                                                    .map(|el| crate::surface_chrome::key_stop(el, &theme))
                                                    .cursor_pointer()
                                                    .text_color(theme.text_muted)
                                                    .child("Cancel")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.cancel_reload_confirmation(cx)
                                                    })),
                                            )
                                            .child(
                                                div()
                                                    .id("files-confirm-reload")
                                                    .role(gpui::Role::Button)
                                                    .rounded(px(4.0))
                                                    // slopty: a keyboard stop with the shell's ring (M8).
                                                    .map(|el| crate::surface_chrome::key_stop(el, &theme))
                                                    .cursor_pointer()
                                                    .text_color(theme.text)
                                                    .child("Discard and reload")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.confirm_reload_active_document(cx)
                                                    })),
                                            )
                                    }),
                            ),
                    )
                },
            )
            .child(body)
            .into_any_element()
    }

    pub(super) fn render_editor_header(
        &mut self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let path = self.preview.active.clone()?;
        Some(self.render_breadcrumb(&path, theme, cx))
    }

    /// Where the system's default app can open `path`. Only a workspace on
    /// this device shares the viewport's filesystem; remote spaces live on
    /// another device, so they get no path.
    fn system_open_path(&self, path: &str) -> Option<std::path::PathBuf> {
        self.request_context
            .as_ref()
            .filter(|context| context.target_device_id.is_none())
            .map(|context| std::path::Path::new(&context.cwd).join(path))
    }

    fn render_breadcrumb(
        &mut self,
        path: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let markdown = super::markdown_preview::is_markdown(path);
        let showing_markdown = self
            .preview
            .documents
            .get(path)
            .is_some_and(|d| d.show_markdown);
        // Outside paths are absolute: the empty crumb before their leading
        // slash is not a segment.
        let parts = path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let reveal_path = path.to_string();
        let system_path = self.system_open_path(path);
        let tooltip_path: SharedString = path.to_string().into();
        let can_save = !self.target_change_pending
            && self
                .preview
                .documents
                .get(path)
                .is_some_and(FileDocument::can_save);
        let save_status =
            self.preview
                .documents
                .get(path)
                .and_then(|document| match &document.phase {
                    DocumentPhase::SaveFailed(error) => Some((
                        "Save failed",
                        theme.danger_muted,
                        can_save,
                        Some(error.clone()),
                    )),
                    DocumentPhase::Conflict { .. } => Some((
                        "Save conflict",
                        theme.warning_muted,
                        false,
                        Some(SharedString::from(
                            "The file changed on disk. Your editor buffer was preserved.",
                        )),
                    )),
                    DocumentPhase::DeletedOnDisk => Some((
                        "Deleted on disk",
                        theme.warning_muted,
                        false,
                        Some(SharedString::from(
                            "The file was removed on disk. Your editor buffer was preserved.",
                        )),
                    )),
                    DocumentPhase::ExternallyModified { .. } => Some((
                        "Changed on disk",
                        theme.warning_muted,
                        false,
                        Some(SharedString::from(
                            "The file changed on disk. Review it before saving.",
                        )),
                    )),
                    _ => None,
                });
        let mut crumbs = div()
            .id("files-breadcrumb-path")
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .overflow_hidden();
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                crumbs = crumbs.child(
                    div()
                        .mx(px(4.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint.opacity(0.65))
                        .child("›"),
                );
            }
            crumbs = crumbs.child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.font_sans.clone())
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(if index + 1 == parts.len() {
                        theme.text_muted
                    } else {
                        theme.text_faint
                    })
                    .child((*part).to_string()),
            );
        }
        crumbs = crumbs
            .tooltip(crate::settings::widgets::text_tooltip(tooltip_path))
            .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY);
        let keys_focus = self.keys_focus(cx);
        toolbar(theme)
            .pr(px(crate::surface_chrome::CONTROL_GAP))
            .child(
                crate::file_icons::icon(
                    crate::file_icons::FileIconIdentity::file(path),
                    theme.appearance,
                )
                .size(px(14.0))
                .flex_none(),
            )
            .child(crumbs)
            .when(markdown, |element| {
                element.child(
                    toolbar_toggle_keyed(
                        "files-toggle-markdown",
                        if showing_markdown {
                            "Show Markdown code"
                        } else {
                            "Preview Markdown"
                        },
                        showing_markdown,
                        Box::new(super::ToggleMarkdownPreview),
                        keys_focus.clone(),
                        theme,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_markdown_preview(window, cx);
                    }))
                    .child(
                        icon(if showing_markdown {
                            icons::FILE_CODE
                        } else {
                            icons::EYE
                        })
                        .size(px(crate::surface_chrome::ICON_SIZE))
                        .text_color(if showing_markdown { theme.text } else { theme.text_muted }),
                    ),
                )
            })
            .when_some(save_status, |element, (label, color, retry, detail)| {
                element.child(
                    div()
                        .id("files-save-status")
                        .h(px(crate::surface_chrome::CONTROL_SIZE))
                        .px(px(6.0))
                        .rounded(px(crate::surface_chrome::CONTROL_RADIUS))
                        .flex()
                        .items_center()
                        .flex_none()
                        .font_family(theme.font_sans.clone())
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(color)
                        .when(retry, |element| {
                            // slopty: a control's hover, faded in, and its press (M23).
                            element
                                .cursor_pointer()
                                .role(gpui::Role::Button)
                                .aria_label("Save file")
                                .bg(crate::motion::hover_blend(
                                    "files-save-status-fade",
                                    crate::theme::wash(0.0),
                                    crate::theme::control_hover(),
                                ))
                                .hover_fade("files-save-status-fade")
                                .active(|style| style.bg(crate::theme::pressed_wash()))
                                // slopty: a keyboard stop with the shell's ring (M8).
                                .map(|el| crate::surface_chrome::key_stop(el, theme))
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.save_active_document(cx)),
                                )
                        })
                        // slopty: the retry names its key (⌘S); the reason is the detail line.
                        .when_some(detail, |element, detail| {
                            if retry {
                                element.tooltip(crate::settings::widgets::detail_tooltip(
                                    "Try again",
                                    detail,
                                    Some(crate::settings::widgets::Keys::Action(Box::new(
                                        crate::shell::SaveFile,
                                    ))),
                                ))
                            } else {
                                element.tooltip(crate::settings::widgets::text_tooltip(detail))
                            }
                            .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                        })
                        .child(label),
                )
            })
            .child(
                toolbar_button("files-reveal-active", "Reveal file in tree", theme)
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(FilesEvent::RevealFile(reveal_path.clone()));
                    }))
                    .child(
                        icon(icons::FOLDER)
                            .size(px(crate::surface_chrome::ICON_SIZE))
                            .text_color(theme.text_muted),
                    ),
            )
            .when_some(system_path, |element, system_path| {
                element.child(
                    toolbar_button("files-open-with-system", "Open in default editor", theme)
                        .on_click(move |_, _, cx| cx.open_with_system(&system_path))
                        .child(
                            icon(icons::ARROW_UP_RIGHT)
                                .size(px(crate::surface_chrome::ICON_SIZE))
                                .text_color(theme.text_muted),
                        ),
                )
            })
            .child(
                toolbar_toggle_keyed(
                    "files-toggle-word-wrap",
                    if self.preview.word_wrap() {
                        "Turn off word wrap"
                    } else {
                        "Turn on word wrap"
                    },
                    self.preview.word_wrap(),
                    Box::new(super::ToggleWordWrap),
                    keys_focus,
                    theme,
                    cx,
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_word_wrap(window, cx)))
                .child(
                    // slopty: the set's wrap glyph, as the diff's wrap toggle wears.
                    icon(icons::WRAP_TEXT)
                        .size(px(crate::surface_chrome::ICON_SIZE))
                        .text_color(if self.preview.word_wrap() {
                            theme.text
                        } else {
                            theme.text_muted
                        }),
                ),
            )
            .into_any_element()
    }

    fn markdown_web_link_handler(cx: &Context<Self>) -> super::markdown_preview::WebLinkHandler {
        let owner = cx.weak_entity();
        Rc::new(move |activation, cx| {
            let _ = owner.update(cx, |surface, cx| {
                let mut activation = activation.clone();
                activation.source_session = Some(surface.chat_id.clone());
                cx.emit(FilesEvent::OpenWebLink(activation));
            });
        })
    }

    fn prepare_markdown_preview(
        &mut self,
        path: &str,
        editor: Option<&Entity<super::editor::FileEditorState>>,
        cx: &mut Context<Self>,
    ) -> Option<Entity<super::markdown_preview::MarkdownPreview>> {
        if !self.preview.documents.get(path).is_some_and(|d| {
            d.show_markdown
                && !matches!(d.phase, DocumentPhase::Loading | DocumentPhase::Error(_))
                && d.file.as_ref().is_some_and(|f| f.text.is_some())
        }) {
            return None;
        }
        let document = self.preview.documents.get_mut(path).unwrap();
        let version = (
            document.generation,
            document.revision,
            document.loaded_hash.clone(),
        );
        let view = document
            .markdown
            .get_or_insert_with(|| {
                let owner = cx.weak_entity();
                let web_links = Self::markdown_web_link_handler(cx);
                cx.new(|cx| {
                    let mut view = super::markdown_preview::MarkdownPreview::new(
                        path.to_string(),
                        Rc::new(move |path, cx| {
                            let _ =
                                owner.update(cx, |surface, cx| surface.open_tree_file(path, cx));
                        }),
                        cx,
                    );
                    view.set_web_link_handler(web_links);
                    view
                })
            })
            .clone();
        let media_client = self
            .request_context
            .clone()
            .zip(self.state.read(cx).engine().cloned())
            .map(|(context, engine)| {
                (
                    WorkspaceFilesClient::new(engine, context),
                    document.file.as_ref().unwrap().checkout_id.clone(),
                )
            });
        let location = (
            self.request_context
                .as_ref()
                .and_then(|context| context.target_device_id.clone()),
            document.file.as_ref().unwrap().checkout_id.clone(),
        );
        view.update(cx, |view, cx| {
            view.media_client = media_client;
            view.editor = editor.map(|editor| editor.downgrade());
            view.activate(path, &location, cx);
        });
        if view.read(cx).version.as_ref() != Some(&version) {
            let source = editor
                .map(|editor| editor.read(cx).value().to_string())
                .unwrap_or_else(|| {
                    document
                        .file
                        .as_ref()
                        .and_then(|f| f.text.clone())
                        .unwrap_or_default()
                });
            let truncated = document.file.as_ref().is_some_and(|f| f.truncated);
            view.update(cx, |view, cx| {
                view.version = Some(version);
                view.set_source(source, truncated, cx);
            });
        }
        Some(view)
    }

    fn render_document_body(
        &mut self,
        path: &str,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.target_change_pending && super::image_preview::is_image(path) {
            return centered_state(
                "The folder changed. Switch back to see this image.",
                theme.text_muted,
            );
        }
        self.preview.images_visible = true;
        if let Some(view) = self
            .preview
            .documents
            .get(path)
            .and_then(|d| d.image.clone())
        {
            view.update(cx, |view, cx| view.activate(cx));
            return view.into_any_element();
        }
        self.apply_pending_external_reload(path, window, cx);
        if self.pending_line_navigation.is_some()
            && let Some(document) = self.preview.documents.get_mut(path)
        {
            document.show_markdown = false;
        }
        let editor = self.ensure_editor(path, theme, window, cx);
        self.apply_line_navigation(path, editor.as_ref(), window, cx);
        let outside = self
            .preview
            .documents
            .get(path)
            .and_then(|document| document.file.as_ref())
            .is_some_and(|file| {
                file.read_only_reason == Some(WorkspaceReadOnlyReason::OutsideWorkspace)
            });
        if let Some(view) = self.prepare_markdown_preview(path, editor.as_ref(), cx) {
            return if outside {
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(outside_read_only_row(theme))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .child(view.into_any_element()),
                    )
                    .into_any_element()
            } else {
                view.into_any_element()
            };
        }

        let Some(document) = self.preview.documents.get(path) else {
            return gpui::Empty.into_any_element();
        };
        // slopty: one loading voice that never flashes, and a failed read that says what failed
        // and offers the way on (M30, M31).
        if matches!(document.phase, DocumentPhase::Loading) {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(crate::loaders::pending(
                    SharedString::from(format!("file-{path}")),
                    "Loading file…",
                ))
                .into_any_element();
        }
        if let DocumentPhase::Error(error) = &document.phase {
            let retry_path = path.to_string();
            return super::failure_state(
                "files-preview-error",
                "Couldn't open this file",
                error.clone(),
                theme,
                cx.listener(move |this, _, _, cx| {
                    this.read_file(retry_path.clone(), cx);
                    cx.notify();
                }),
                cx,
            );
        }
        if let Some(editor) = editor {
            editor.update(cx, |state, _| {
                state.set_editor_style(super::editor_adapter::editor_style(theme));
            });
            return div()
                .id("files-editor-body")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .relative()
                .overflow_hidden()
                .font_family(theme.font_mono.clone())
                .text_size(px(self.preview.code_text_size()))
                .line_height(self.preview.line_height())
                .child(super::editor::editor_element(&editor))
                .into_any_element();
        }
        let Some(file) = document.file.as_ref() else {
            return centered_state("This file cannot be previewed.", theme.text_muted);
        };
        if file.text.is_none() {
            return centered_state(read_only_message(file.read_only_reason), theme.text_muted);
        }
        let truncated = file.truncated;
        let word_wrap = self.preview.word_wrap();
        let code_scroll = if word_wrap {
            div()
                .id("files-preview-code-scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(
                    list(
                        self.preview.list.clone(),
                        cx.processor(Self::render_preview_line),
                    )
                    .flex_1()
                    .min_h_0()
                    .with_sizing_behavior(ListSizingBehavior::Auto),
                )
        } else {
            let mut scroll = div()
                .id("files-preview-code-scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .overflow_x_scroll()
                .track_scroll(&self.preview.horizontal_scroll)
                .child(
                    div().flex_none().min_w_full().h_full().child(
                        list(
                            self.preview.list.clone(),
                            cx.processor(Self::render_preview_line),
                        )
                        .h_full()
                        .with_sizing_behavior(ListSizingBehavior::Infer),
                    ),
                );
            // GPUI otherwise maps a vertical wheel gesture to X for an x-only
            // scroller, preventing the list from receiving it.
            scroll.style().restrict_scroll_to_axis = Some(true);
            scroll
        };

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(outside, |element| {
                element.child(outside_read_only_row(theme))
            })
            .when(truncated, |element| {
                element.child(info_row(
                    theme,
                    "Large file: the preview shows its start, read-only",
                ))
            })
            .child(code_scroll)
            .into_any_element()
    }

    fn ensure_editor(
        &mut self,
        path: &str,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Entity<super::editor::FileEditorState>> {
        let document = self.preview.documents.get(path)?;
        if let Some(editor) = document.editor.clone() {
            return Some(editor);
        }
        if !document.is_editable() {
            return None;
        }
        let text = document.file.as_ref()?.text.clone()?;
        let focus_editor = !document.show_markdown;
        let editor =
            super::editor::new_file_editor(text, path, self.preview.word_wrap, theme, window, cx);
        let event_path = path.to_string();
        let editor_events = super::editor::subscribe_to_changes(&editor, event_path, cx);
        let editor_observer = cx.observe(&editor, |_, _, cx| cx.notify());
        if focus_editor {
            let focus = editor.focus_handle(cx);
            window.defer(cx, move |window, cx| focus.focus(window, cx));
        }
        let document = self.preview.documents.get_mut(path)?;
        document.editor = Some(editor.clone());
        document.editor_events = Some(editor_events);
        document.editor_observer = Some(editor_observer);
        let syntax = self.preview.highlights.get(path).and_then(|highlight| {
            let document = self.preview.documents.get(path)?;
            if document.content_hash() != Some(highlight.content_hash.as_str()) {
                return None;
            }
            Some((
                document.file.as_ref()?.text.clone()?,
                highlight.document.clone(),
            ))
        });
        if let Some((source, highlighted)) = syntax {
            super::editor_adapter::install_highlighter(&editor, source, highlighted, cx);
        }
        Some(editor)
    }

    fn render_preview_line(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(path) = self.preview.active.as_deref() else {
            return gpui::Empty.into_any_element();
        };
        let Some(document) = self.preview.documents.get(path) else {
            return gpui::Empty.into_any_element();
        };
        let Some(file) = document.file.as_ref() else {
            return gpui::Empty.into_any_element();
        };
        let Some(line) = document.lines.get(index) else {
            return gpui::Empty.into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let word_wrap = self.preview.word_wrap();
        let spans = self
            .preview
            .highlights
            .get(path)
            .filter(|highlight| {
                file.content_hash.as_deref() == Some(highlight.content_hash.as_str())
            })
            .and_then(|highlight| highlight.document.lines.get(index))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mono = font(theme.font_mono.clone());
        let runs = crate::markdown::render::runs_for_syntax_line_with_plain(
            line.as_ref(),
            spans,
            &mono,
            theme.text.opacity(0.93),
            &theme,
        );
        let row_height = self.preview.line_height();
        div()
            .min_h(row_height)
            .flex_none()
            .flex()
            .when(word_wrap, |element| element.w_full().items_stretch())
            .when(!word_wrap, |element| {
                element.h(row_height).min_w_full().items_center()
            })
            .child(
                div()
                    .w(px(48.0))
                    .when(!word_wrap, |element| element.h_full())
                    .flex_none()
                    .pr(px(10.0))
                    .border_r_1()
                    .border_color(theme.border.opacity(0.55))
                    .flex()
                    .items_center()
                    .justify_end()
                    .font_family(theme.font_mono.clone())
                    .text_size(px(10.0))
                    .text_color(theme.text_faint.opacity(0.7))
                    .child((index + 1).to_string()),
            )
            .child(
                div()
                    .when(word_wrap, |element| {
                        element.flex_1().min_w_0().py(px(2.0)).whitespace_normal()
                    })
                    .pl(px(12.0))
                    .pr(px(18.0))
                    .when(!word_wrap, |element| element.whitespace_nowrap())
                    .font_family(theme.font_mono.clone())
                    // Same source `row_height` derives from; a second reading of
                    // the code size could drift from the geometry the glyphs
                    // are measured against.
                    .text_size(px(self.preview.code_text_size()))
                    .child(gpui::StyledText::new(line.clone()).with_runs(runs)),
            )
            .into_any_element()
    }
}

fn centered_state(message: impl Into<SharedString>, color: gpui::Hsla) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(24.0))
        .text_center()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(color)
        .child(message.into())
        .into_any_element()
}

/// The read-only line an outside file shows above its content — text or
/// rendered Markdown — matching the truncated-preview row's shape.
fn outside_read_only_row(theme: &Theme) -> AnyElement {
    info_row(
        theme,
        read_only_message(Some(WorkspaceReadOnlyReason::OutsideWorkspace)),
    )
}

// slopty: a note over a file (zeron's amber strip), with the info glyph.
pub(super) fn info_row(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    div()
        .h(px(28.0))
        .flex_none()
        .px(px(10.0))
        .gap(px(6.0))
        .border_b_1()
        .border_color(theme.border)
        .bg(theme.warning.opacity(0.045))
        .flex()
        .items_center()
        .text_size(crate::typography::ui_rems(11.0))
        .text_color(theme.warning_muted)
        .child(
            icon(icons::INFO_CIRCLE)
                .size(px(12.0))
                .flex_none()
                .text_color(theme.warning_muted),
        )
        .child(div().min_w_0().truncate().child(text.into()))
        .into_any_element()
}

/// A failed read surfaces in the document's error state: a missing file
/// shows its own message instead of the transport's RPC framing; every
/// other failure keeps its wording.
fn read_error_message(error: &super::client::FilesClientError) -> SharedString {
    match error {
        super::client::FilesClientError::Request(message) if message == "file not found" => {
            "File not found.".into()
        }
        _ => error.to_string().into(),
    }
}

fn read_only_message(reason: Option<WorkspaceReadOnlyReason>) -> SharedString {
    match reason {
        Some(WorkspaceReadOnlyReason::Binary) => "Binary files cannot be previewed.",
        Some(WorkspaceReadOnlyReason::UnsupportedEncoding) => {
            "This file encoding is not supported."
        }
        Some(WorkspaceReadOnlyReason::Symlink) => "Symlink targets are read-only.",
        Some(WorkspaceReadOnlyReason::PermissionDenied) => "Permission denied.",
        Some(WorkspaceReadOnlyReason::TooLarge) => "This file is too large to preview.",
        Some(WorkspaceReadOnlyReason::MixedLineEndings) => {
            "Files with mixed line endings are read-only."
        }
        Some(WorkspaceReadOnlyReason::OutsideWorkspace) => "Read-only: outside this session's folder.",
        Some(WorkspaceReadOnlyReason::NotRegularFile) | None => "This file cannot be previewed.",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn renaming_markdown_to_text_keeps_dirty_editor_and_undo(cx: &mut TestAppContext) {
        let (files, cx) = super::super::test_support::setup(cx);
        let editor = files.update_in(cx, |files, window, cx| {
            let theme = Theme::of(cx).clone();
            let editor = super::super::editor::new_file_editor(
                "# Original",
                "note.md",
                false,
                &theme,
                window,
                cx,
            );
            let mut document = cached_document("note.md", "# Original");
            document.set_loaded(slopty_view_proto::WorkspaceFileText {
                checkout_id: "checkout".into(),
                path: "note.md".into(),
                text: Some("# Original".into()),
                content_hash: Some("saved-hash".into()),
                size: 10,
                modified_at: None,
                encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                read_only_reason: None,
                truncated: false,
            });
            editor.update(cx, |editor, cx| {
                editor.set_selected_range(10..10, cx);
                editor.replace(" unsaved", window, cx);
                editor.set_selected_range(2..10, cx);
            });
            document.editor = Some(editor.clone());
            document.mark_user_edit();
            files.preview.documents.insert("note.md".into(), document);
            files.preview.active = Some("note.md".into());
            files.editor_path = Some("note.md".into());
            files.presentation = super::super::FilesPresentation::Editor;
            assert!(
                files
                    .prepare_markdown_preview("note.md", Some(&editor), cx)
                    .is_some()
            );
            files.apply_semantic_mutation(
                &slopty_view_proto::WorkspaceFileChange {
                    operation_id: Some("rename-md".into()),
                    kind: slopty_view_proto::WorkspaceFileChangeKind::Renamed,
                    path: "note.txt".into(),
                    old_path: Some("note.md".into()),
                },
                None,
                true,
                cx,
            );
            let document = &files.preview.documents["note.txt"];
            assert!(!document.show_markdown);
            assert!(document.markdown.is_none());
            assert!(document.is_dirty());
            assert_eq!(document.phase, DocumentPhase::Ready);
            assert_eq!(document.editor.as_ref(), Some(&editor));
            assert_eq!(editor.read(cx).value().as_ref(), "# Original unsaved");
            assert_eq!(editor.read(cx).selected_range(), 2..10);
            assert_eq!(document.saved_hash.as_deref(), Some("saved-hash"));
            assert!(
                files
                    .prepare_markdown_preview("note.txt", Some(&editor), cx)
                    .is_none()
            );
            editor.update(cx, |editor, cx| editor.focus(window, cx));
            editor
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.dispatch_action(Box::new(gpui_base::input::Undo), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            editor.read_with(cx, |editor, _| editor.value().to_string()),
            "# Original"
        );
    }

    #[gpui::test]
    fn rename_refreshes_language_and_preserves_markdown_view_choice(cx: &mut TestAppContext) {
        let (files, cx) = super::super::test_support::setup(cx);
        files.update_in(cx, |files, window, cx| {
            let theme = Theme::of(cx).clone();
            let editor = super::super::editor::new_file_editor(
                "let value = 1;",
                "note.txt",
                false,
                &theme,
                window,
                cx,
            );
            let mut document = cached_document("note.txt", "let value = 1;");
            document.editor = Some(editor);
            files.preview.documents.insert("note.txt".into(), document);
            files.rename_documents("note.txt", "note.rs".into(), cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(120));
        cx.run_until_parked();
        files.update_in(cx, |files, _, cx| {
            let key = DocumentHighlightKey::new(
                slopty_syntax::language_for_path("note.rs").unwrap(),
                "let value = 1;",
            );
            assert!(
                files.preview.syntax_cache.get(&key).is_some(),
                "the open editor must be parsed in the new language without an edit"
            );
            files.rename_documents("note.rs", "note.md".into(), cx);
            assert!(files.preview.documents["note.md"].show_markdown);
            files
                .preview
                .documents
                .get_mut("note.md")
                .unwrap()
                .show_markdown = false;
            files.rename_documents("note.md", "renamed.md".into(), cx);
            assert!(
                !files.preview.documents["renamed.md"].show_markdown,
                "renaming within Markdown must preserve the user's code view choice"
            );
            files.rename_documents("renamed.md", "renamed.txt".into(), cx);
            assert!(
                files.preview.documents["renamed.txt"]
                    .highlight_task
                    .is_none()
            );
            assert!(!files.preview.highlights.contains_key("renamed.txt"));
        });
    }

    /// A missing file gets its own wording — the transport's RPC framing
    /// never reaches the reader — while every other failure keeps its own.
    #[test]
    fn missing_files_and_outside_reads_show_their_own_wording() {
        assert_eq!(
            read_error_message(&crate::files::client::FilesClientError::Request(
                "file not found".into()
            )),
            SharedString::from("File not found.")
        );
        assert_eq!(
            read_error_message(&crate::files::client::FilesClientError::Transport(
                "gone".into()
            ))
            .to_string(),
            "workspace connection unavailable: gone"
        );
        assert_eq!(
            read_only_message(Some(WorkspaceReadOnlyReason::OutsideWorkspace)),
            SharedString::from("Read-only: outside this session's folder.")
        );
    }

    // Rendering a preview needs a window, so this asserts on `line_height`,
    // the single source both the uniform-height hint passed to
    // `reset_with_uniform_height` and the painted row in
    // `render_preview_line` read.
    #[test]
    fn preview_rows_track_the_code_font_size_from_the_default_to_the_maximum() {
        let default = FilePreviewState::new(false, 900, false, 12.5);
        assert_eq!(default.line_height(), px(PREVIEW_LINE_HEIGHT));

        let maximum = FilePreviewState::new(false, 900, false, 32.0);
        assert!(maximum.line_height() > px(PREVIEW_LINE_HEIGHT));

        let minimum = FilePreviewState::new(false, 900, false, 8.0);
        assert_eq!(minimum.line_height(), px(PREVIEW_LINE_HEIGHT));
    }

    // slopty: one file, one size: the editor, a read-only file and the diff draw the same code
    // at the same size and line.
    #[test]
    fn the_pane_draws_code_at_one_size() {
        let default =
            FilePreviewState::new(false, 900, false, crate::typography::CODE_FONT_SIZE_DEFAULT);
        let theme = Theme::dark();
        assert_eq!(default.code_text_size(), crate::changes::diff_text_size(&theme));
        assert_eq!(f32::from(default.line_height()), crate::changes::diff_line_height(&theme));
        let doubled = FilePreviewState::new(
            false,
            900,
            false,
            2.0 * crate::typography::CODE_FONT_SIZE_DEFAULT,
        );
        assert_eq!(doubled.code_text_size(), 25.0);
        let maximum = FilePreviewState::new(false, 900, false, crate::typography::FONT_SIZE_MAX);
        assert_eq!(maximum.code_text_size(), crate::typography::FONT_SIZE_MAX);
    }

    fn cached_document(path: &str, text: &str) -> FileDocument {
        let mut document = FileDocument::loading(DocumentKey {
            chat_id: "chat-1".into(),
            checkout_id: Some("checkout-1".into()),
            path: path.into(),
        });
        document.phase = DocumentPhase::Ready;
        document.lines = Arc::new(vec![SharedString::from(text.to_string())]);
        document
    }

    #[test]
    fn read_only_reasons_have_specific_messages() {
        assert!(read_only_message(Some(WorkspaceReadOnlyReason::Binary)).contains("Binary"));
        assert!(
            read_only_message(Some(WorkspaceReadOnlyReason::UnsupportedEncoding))
                .contains("encoding")
        );
    }

    #[test]
    fn reset_drops_documents_and_active_preview_from_the_previous_target() {
        let mut preview = FilePreviewState::new(false, 900, false, 11.5);
        preview.active = Some("private.env".into());
        preview.documents.insert(
            "private.env".into(),
            FileDocument::loading(DocumentKey {
                chat_id: "chat-1".into(),
                checkout_id: Some("checkout-1".into()),
                path: "private.env".into(),
            }),
        );
        preview
            .documents
            .get_mut("private.env")
            .unwrap()
            .mark_external(None);

        preview.reset();

        assert!(preview.documents.is_empty());
        assert!(preview.active.is_none());
    }

    #[test]
    fn document_cache_evicts_the_oldest_safe_entries() {
        let mut preview = FilePreviewState::new(false, 900, false, 11.5);
        for index in 0..18 {
            let path = format!("src/{index}.rs");
            preview
                .documents
                .insert(path.clone(), cached_document(&path, "fn main() {}"));
            preview.touch_document(&path);
        }
        preview.active = Some("src/0.rs".into());
        preview.documents.get_mut("src/1.rs").unwrap().revision = 1;
        preview.documents.get_mut("src/2.rs").unwrap().phase =
            DocumentPhase::Conflict { disk_hash: None };
        preview.documents.get_mut("src/7.rs").unwrap().phase = DocumentPhase::Loading;
        let protected = HashSet::from(["src/4.rs".to_string()]);

        let evicted = preview.trim_document_cache_to(&protected, 15, usize::MAX);

        assert_eq!(evicted, vec!["src/3.rs", "src/5.rs", "src/6.rs"]);
        for retained in ["src/0.rs", "src/1.rs", "src/2.rs", "src/4.rs"] {
            assert!(preview.documents.contains_key(retained));
        }
        assert!(preview.documents.contains_key("src/7.rs"));
    }

    #[test]
    fn document_cache_uses_retained_bytes_not_only_entry_count() {
        let mut preview = FilePreviewState::new(false, 900, false, 11.5);
        for path in ["old.rs", "active.rs"] {
            preview
                .documents
                .insert(path.into(), cached_document(path, &"x".repeat(4 * 1024)));
            preview.touch_document(path);
        }
        preview.active = Some("active.rs".into());
        let active_bytes = preview.documents["active.rs"].estimated_retained_bytes();
        let total_bytes = preview.retained_document_bytes();

        let evicted = preview.trim_document_cache_to(
            &HashSet::new(),
            usize::MAX,
            total_bytes.saturating_sub(1),
        );

        assert_eq!(evicted, vec!["old.rs"]);
        assert!(preview.retained_document_bytes() <= active_bytes);
    }

    #[test]
    fn document_eviction_cleans_path_scoped_companion_state() {
        let mut preview = FilePreviewState::new(false, 900, false, 11.5);
        for path in ["old.rs", "active.rs"] {
            preview
                .documents
                .insert(path.into(), cached_document(path, "fn main() {}"));
            preview.touch_document(path);
        }
        preview.active = Some("active.rs".into());
        let highlighted = Arc::new(
            slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                source: "fn main() {}",
                path: Some("old.rs"),
                fence_tag: None,
            })
            .unwrap(),
        );
        preview.highlights.insert(
            "old.rs".into(),
            HighlightedFile {
                content_hash: "hash".into(),
                document: highlighted,
            },
        );

        let evicted = preview.trim_document_cache_to(&HashSet::new(), 1, usize::MAX);

        assert_eq!(evicted, vec!["old.rs"]);
        assert!(!preview.highlights.contains_key("old.rs"));
        assert!(!preview.document_recency.iter().any(|path| path == "old.rs"));
    }

    #[test]
    fn reset_preserves_global_editor_preferences() {
        let mut preview = FilePreviewState::new(false, 900, true, 15.0);

        preview.reset();

        assert!(!preview.autosave_enabled);
        assert!(preview.word_wrap());
        assert_eq!(preview.editor_font_size, 15.0);
    }

    #[test]
    fn file_highlight_completion_rejects_a_result_after_a_user_edit() {
        let path = "src/lib.rs";
        let disk_hash = "disk-hash-1";
        let stale_source = "ab";
        let updated_source = "é";
        let mut document = FileDocument::loading(DocumentKey {
            chat_id: "chat-1".into(),
            checkout_id: Some("checkout-1".into()),
            path: path.into(),
        });
        document.set_loaded(slopty_view_proto::WorkspaceFileText {
            checkout_id: "checkout-1".into(),
            path: path.into(),
            text: Some(stale_source.into()),
            content_hash: Some(disk_hash.into()),
            size: stale_source.len() as u64,
            modified_at: None,
            encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
            line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
            read_only_reason: None,
            truncated: false,
        });
        let task_document_key = document.key.clone();
        let task_generation = document.generation;
        let task_revision = document.revision;
        let stale_highlight_key =
            DocumentHighlightKey::new(slopty_syntax::LanguageId::Rust, stale_source);

        assert!(file_highlight_result_is_current(
            &document,
            &task_document_key,
            task_generation,
            task_revision,
            disk_hash,
        ));

        document.mark_user_edit();
        let current_highlight_key =
            DocumentHighlightKey::new(slopty_syntax::LanguageId::Rust, updated_source);

        assert_ne!(document.revision, task_revision);
        assert_ne!(current_highlight_key, stale_highlight_key);
        assert!(
            !file_highlight_result_is_current(
                &document,
                &task_document_key,
                task_generation,
                task_revision,
                disk_hash,
            ),
            "highlight for revision {task_revision} was accepted at revision {} after the editor source changed from {stale_source:?} to {updated_source:?}",
            document.revision
        );
    }

    #[test]
    fn autosave_is_opt_in_and_enabling_schedules_dirty_documents() {
        let path = "src/lib.rs";
        let mut preview = FilePreviewState::new(false, 900, false, 11.5);
        let mut document = FileDocument::loading(DocumentKey {
            chat_id: "chat-1".into(),
            checkout_id: Some("checkout-1".into()),
            path: path.into(),
        });
        document.set_loaded(slopty_view_proto::WorkspaceFileText {
            checkout_id: "checkout-1".into(),
            path: path.into(),
            text: Some("fn main() {}".into()),
            content_hash: Some("hash-1".into()),
            size: 12,
            modified_at: None,
            encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
            line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
            read_only_reason: None,
            truncated: false,
        });
        document.revision = 1;
        preview.documents.insert(path.into(), document);

        assert!(preview.set_autosave_delay_ms(600).is_empty());
        assert_eq!(preview.set_autosave_enabled(true), vec![path.to_string()]);
        assert!(preview.set_autosave_enabled(false).is_empty());
    }

    #[test]
    fn dirty_reload_waits_for_explicit_discard_confirmation() {
        let path = "src/lib.rs";
        let mut preview = FilePreviewState::new(false, 900, false, 11.5);
        let mut document = FileDocument::loading(DocumentKey {
            chat_id: "chat-1".into(),
            checkout_id: Some("checkout-1".into()),
            path: path.into(),
        });
        document.revision = 1;
        preview.documents.insert(path.into(), document);

        assert_eq!(
            preview.request_reload(path),
            ReloadDecision::AwaitDiscardConfirmation
        );
        assert_eq!(preview.reload_confirmation.as_deref(), Some(path));
        assert!(preview.documents[path].is_dirty());

        // Repeated toolbar clicks must not bypass the explicit confirmation.
        assert_eq!(
            preview.request_reload(path),
            ReloadDecision::AwaitDiscardConfirmation
        );
    }

    #[test]
    fn clean_reload_proceeds_without_confirmation() {
        let path = "src/lib.rs";
        let mut preview = FilePreviewState::new(false, 900, false, 11.5);
        preview.documents.insert(
            path.into(),
            FileDocument::loading(DocumentKey {
                chat_id: "chat-1".into(),
                checkout_id: Some("checkout-1".into()),
                path: path.into(),
            }),
        );
        preview.reload_confirmation = Some("stale.rs".into());

        assert_eq!(preview.request_reload(path), ReloadDecision::ReloadNow);
        assert!(preview.reload_confirmation.is_none());
    }

    #[test]
    fn failed_save_blocks_lifecycle_until_explicit_recovery() {
        let mut document = FileDocument::loading(DocumentKey {
            chat_id: "chat-1".into(),
            checkout_id: Some("checkout-1".into()),
            path: "src/lib.rs".into(),
        });
        document.revision = 1;
        document.phase = DocumentPhase::SaveFailed("offline".into());

        assert!(document_blocks_lifecycle(&document));

        document.phase = DocumentPhase::Ready;
        assert!(!document_blocks_lifecycle(&document));
    }

    #[test]
    fn directory_rename_updates_open_descendant_paths() {
        assert_eq!(
            renamed_document_path("src/files/mod.rs", "src", "crates/ui/src"),
            Some("crates/ui/src/files/mod.rs".into())
        );
        assert_eq!(
            renamed_document_path("src-old/lib.rs", "src", "crates/ui/src"),
            None
        );
    }

    #[test]
    fn document_paths_match_recreated_files_and_directory_descendants() {
        assert!(path_is_same_or_descendant("src/lib.rs", "src/lib.rs"));
        assert!(path_is_same_or_descendant("src/files/mod.rs", "src"));
        assert!(!path_is_same_or_descendant("src-old/lib.rs", "src"));
    }

}

#[cfg(test)]
impl FilesSurface {
    pub(crate) fn test_document_text(&self, path: &str) -> Option<String> {
        self.preview
            .documents
            .get(path)?
            .file
            .as_ref()?
            .text
            .clone()
    }

    pub(crate) fn test_document_phase(&self, path: &str) -> Option<String> {
        Some(format!("{:?}", self.preview.documents.get(path)?.phase))
    }

    pub(crate) fn seed_pending_exit_test_document(&mut self, failed: bool) {
        let mut document = FileDocument::loading(DocumentKey {
            chat_id: "test".into(),
            checkout_id: None,
            path: "test.rs".into(),
        });
        document.revision = 1;
        document.phase = if failed {
            DocumentPhase::SaveFailed("offline".into())
        } else {
            DocumentPhase::Saving
        };
        self.preview.documents.insert("test.rs".into(), document);
    }
}

#[cfg(test)]
mod markdown_buffer_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn content_search_byte_columns_place_cursor_at_match_after_unicode(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let path = "search.txt";
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_editor(
                state,
                "chat".into(),
                path.into(),
                false,
                1000,
                13.0,
                false,
                false,
                cx,
            )
        });
        for prefix in [
            "plain ",
            "café ",
            "🦀 ",
            "café 🦀 ",
            "\u{2003}\tcafé 🦀 ",
            "cafe\u{301} ",
        ] {
            let source = format!("first line\n{prefix}needle suffix\nlast line");
            window
                .update(cx, |surface, window, cx| {
                    surface.preview.documents.insert(
                        path.into(),
                        FileDocument::loading(DocumentKey {
                            chat_id: "chat".into(),
                            checkout_id: Some("checkout".into()),
                            path: path.into(),
                        }),
                    );
                    surface.navigate_to_line(2, Some(FileColumn::Byte(prefix.len() as u64)), cx);
                    // Navigation can arrive before the file contents do.
                    surface.apply_line_navigation(path, None, window, cx);
                    assert!(surface.pending_line_navigation.is_some());
                    surface.preview.documents.get_mut(path).unwrap().set_loaded(
                        slopty_view_proto::WorkspaceFileText {
                            checkout_id: "checkout".into(),
                            path: path.into(),
                            text: Some(source.clone()),
                            content_hash: Some("hash".into()),
                            size: source.len() as u64,
                            modified_at: None,
                            encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                            line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                            read_only_reason: None,
                            truncated: false,
                        },
                    );
                    let editor = cx.new(|cx| {
                        gpui_base::input::EditorState::new(window, cx).default_value(source.clone())
                    });
                    surface.apply_line_navigation(path, Some(&editor), window, cx);
                    assert!(surface.pending_line_navigation.is_none());
                    assert_eq!(
                        editor.read(cx).cursor(),
                        source.find("needle").unwrap(),
                        "{prefix:?}"
                    );
                    assert_eq!(
                        editor.read(cx).cursor_position(),
                        gpui_base::input::Position::new(1, prefix.chars().count() as u32)
                    );
                    // Opening a link to the same character must keep its existing
                    // one-based character semantics, even on a multibyte line.
                    surface.navigate_to_line(
                        2,
                        Some(FileColumn::Character(prefix.chars().count() as u32 + 1)),
                        cx,
                    );
                    surface.apply_line_navigation(path, Some(&editor), window, cx);
                    assert_eq!(
                        editor.read(cx).cursor(),
                        source.find("needle").unwrap(),
                        "file link: {prefix:?}"
                    );
                })
                .unwrap();
        }
    }

    #[gpui::test]
    fn chat_line_link_opens_markdown_source_and_centers_the_requested_line(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let path = "README.md";
        let source = (1..=180)
            .map(|line| format!("line {line} content"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_editor(
                state,
                "chat".into(),
                path.into(),
                false,
                1000,
                13.0,
                false,
                false,
                cx,
            )
        });
        window
            .update(cx, |surface, _, cx| {
                let document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: path.into(),
                });
                assert!(document.show_markdown);
                surface.navigate_to_line(20, Some(FileColumn::Character(3)), cx);
                surface.navigate_to_line(90, Some(FileColumn::Character(6)), cx);
                surface.preview.active = Some(path.into());
                surface.preview.documents.insert(path.into(), document);
            })
            .unwrap();

        cx.update_window(window.into(), |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        })
        .unwrap();
        window
            .update(cx, |surface, _, cx| {
                assert_eq!(
                    surface.pending_line_navigation,
                    Some((90, Some(FileColumn::Character(6))))
                );
                surface.preview.documents.get_mut(path).unwrap().set_loaded(
                    slopty_view_proto::WorkspaceFileText {
                        checkout_id: "checkout".into(),
                        path: path.into(),
                        text: Some(source.clone()),
                        content_hash: Some("hash".into()),
                        size: source.len() as u64,
                        modified_at: None,
                        encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                        line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                        read_only_reason: None,
                        truncated: false,
                    },
                );
                surface.sync_preview_list();
                cx.notify();
            })
            .unwrap();

        for _ in 0..5 {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
                window.simulate_next_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
        }

        window
            .update(cx, |surface, _, cx| {
                let document = &surface.preview.documents[path];
                assert!(!document.show_markdown);
                let editor = document.editor.as_ref().unwrap().read(cx);
                assert_eq!(
                    editor.cursor_position(),
                    gpui_base::input::Position::new(89, 5)
                );
                let cursor = editor.cursor();
                let line_bounds = editor.range_to_bounds(&(cursor..cursor)).unwrap_or_else(|| {
                    panic!(
                        "cursor outside layout: visible={:?} viewport={:?} scroll={:?} text={:?}",
                        editor.visible_row_range(),
                        editor.input_bounds(),
                        editor.scroll_offset(),
                        editor.text_bounds(),
                    )
                });
                let viewport = editor.input_bounds();
                let distance = f32::from(
                    line_bounds.origin.y + line_bounds.size.height / 2.0
                        - (viewport.origin.y + viewport.size.height / 2.0),
                );
                assert!(distance.abs() <= f32::from(line_bounds.size.height));
            })
            .unwrap();

        window
            .update(cx, |surface, _, cx| surface.navigate_to_line(180, None, cx))
            .unwrap();
        for _ in 0..5 {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
                window.simulate_next_frame(cx);
            })
            .unwrap();
        }
        window
            .update(cx, |surface, _, cx| {
                let editor = surface.preview.documents[path]
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx);
                assert_eq!(
                    editor.cursor_position(),
                    gpui_base::input::Position::new(179, 0)
                );
                let cursor = editor.cursor();
                let line_bounds = editor.range_to_bounds(&(cursor..cursor)).unwrap();
                let viewport = editor.input_bounds();
                let distance = f32::from(
                    line_bounds.origin.y + line_bounds.size.height / 2.0
                        - (viewport.origin.y + viewport.size.height / 2.0),
                );
                assert!(distance.abs() <= f32::from(line_bounds.size.height));
            })
            .unwrap();
    }

    #[gpui::test]
    fn chat_line_link_near_the_top_never_scrolls_above_the_first_line(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let path = "src/lib.rs";
        let source = (1..=180)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_editor(
                state,
                "chat".into(),
                path.into(),
                false,
                1000,
                13.0,
                false,
                false,
                cx,
            )
        });
        window
            .update(cx, |surface, _, _| {
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: path.into(),
                });
                document.set_loaded(slopty_view_proto::WorkspaceFileText {
                    checkout_id: "checkout".into(),
                    path: path.into(),
                    text: Some(source.clone()),
                    content_hash: Some("hash".into()),
                    size: source.len() as u64,
                    modified_at: None,
                    encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                    line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                    read_only_reason: None,
                    truncated: false,
                });
                surface.preview.active = Some(path.into());
                surface.preview.documents.insert(path.into(), document);
                surface.sync_preview_list();
            })
            .unwrap();
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
                window.simulate_next_frame(cx);
            })
            .unwrap();
        };
        let first_line_top = |cx: &mut TestAppContext| {
            window
                .update(cx, |surface, _, cx| {
                    let editor = surface.preview.documents[path].editor.as_ref()?.read(cx);
                    editor.range_to_bounds(&(0..0)).map(|bounds| bounds.top())
                })
                .unwrap()
        };
        for _ in 0..3 {
            draw(cx);
        }
        let resting_top = first_line_top(cx).expect("first line laid out");

        window
            .update(cx, |surface, _, cx| surface.navigate_to_line(3, None, cx))
            .unwrap();
        for _ in 0..5 {
            draw(cx);
            if let Some(top) = first_line_top(cx) {
                assert!(top <= resting_top, "{top:?} painted below {resting_top:?}");
            }
        }
        window
            .update(cx, |surface, _, cx| {
                let editor = surface.preview.documents[path]
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx);
                assert_eq!(
                    editor.cursor_position(),
                    gpui_base::input::Position::new(2, 0)
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn chat_line_link_suspends_an_open_markdown_preview(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let path = "README.md";
        let source = "# Title\n\nBody\n";
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_editor(
                state,
                "chat".into(),
                path.into(),
                false,
                1000,
                13.0,
                false,
                false,
                cx,
            )
        });
        window
            .update(cx, |surface, _, cx| {
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: path.into(),
                });
                document.set_loaded(slopty_view_proto::WorkspaceFileText {
                    checkout_id: "checkout".into(),
                    path: path.into(),
                    text: Some(source.into()),
                    content_hash: Some("hash".into()),
                    size: source.len() as u64,
                    modified_at: None,
                    encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                    line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                    read_only_reason: None,
                    truncated: false,
                });
                assert!(document.show_markdown);
                surface.preview.active = Some(path.into());
                surface.preview.documents.insert(path.into(), document);
                let view = surface.prepare_markdown_preview(path, None, cx).unwrap();
                assert!(!view.read(cx).test_suspended());

                surface.navigate_to_line(2, None, cx);

                assert!(!surface.preview.documents[path].show_markdown);
                assert!(view.read(cx).test_suspended());
            })
            .unwrap();
    }

    #[gpui::test]
    fn chat_line_link_centers_read_only_text(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let path = "large.txt";
        let source = (1..=180)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_editor(
                state,
                "chat".into(),
                path.into(),
                false,
                1000,
                13.0,
                false,
                false,
                cx,
            )
        });
        window
            .update(cx, |surface, _, cx| {
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: path.into(),
                });
                document.set_loaded(slopty_view_proto::WorkspaceFileText {
                    checkout_id: "checkout".into(),
                    path: path.into(),
                    text: Some(source.clone()),
                    content_hash: Some("hash".into()),
                    size: source.len() as u64,
                    modified_at: None,
                    encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                    line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                    read_only_reason: Some(WorkspaceReadOnlyReason::MixedLineEndings),
                    truncated: false,
                });
                assert!(!document.is_editable());
                surface.preview.active = Some(path.into());
                surface.preview.documents.insert(path.into(), document);
                surface.sync_preview_list();
                surface.navigate_to_line(90, None, cx);
            })
            .unwrap();
        for _ in 0..5 {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
                window.simulate_next_frame(cx);
            })
            .unwrap();
        }
        window
            .update(cx, |surface, _, _| {
                assert!(surface.preview.documents[path].editor.is_none());
                let row = surface.preview.list.bounds_for_item(89).unwrap();
                let viewport = surface.preview.list.viewport_bounds();
                let distance = f32::from(
                    row.origin.y + row.size.height / 2.0
                        - (viewport.origin.y + viewport.size.height / 2.0),
                );
                assert!(distance.abs() <= f32::from(row.size.height));
            })
            .unwrap();
    }

    #[gpui::test]
    fn default_editor_opens_only_files_on_this_device(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let surface = cx.new(|cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        });
        surface.update(cx, |surface, _| {
            assert_eq!(surface.system_open_path("src/lib.rs"), None);
            let local = FilesRequestContext {
                target: slopty_view_proto::WorkspaceTarget {
                    chat_id: Some("chat".into()),
                    space_id: None,
                    checkout_path: None,
                },
                target_device_id: None,
                cwd: "/workspace".into(),
                checkout_id: Some("checkout".into()),
            };
            surface.request_context = Some(local.clone());
            assert_eq!(
                surface.system_open_path("src/lib.rs"),
                Some(std::path::PathBuf::from("/workspace/src/lib.rs"))
            );
            surface.request_context = Some(FilesRequestContext {
                target_device_id: Some("other-device".into()),
                ..local
            });
            assert_eq!(surface.system_open_path("src/lib.rs"), None);
        });
    }

    #[gpui::test]
    fn image_rename_preserves_unsaved_text_on_reopen_and_watcher(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        });
        window
            .update(cx, |surface, window, cx| {
                surface.request_context = Some(FilesRequestContext {
                    target: slopty_view_proto::WorkspaceTarget {
                        chat_id: Some("chat".into()),
                        space_id: None,
                        checkout_path: None,
                    },
                    target_device_id: None,
                    cwd: "/workspace".into(),
                    checkout_id: Some("checkout".into()),
                });
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: "drawing.txt".into(),
                });
                document.set_loaded(slopty_view_proto::WorkspaceFileText {
                    checkout_id: "checkout".into(),
                    path: "drawing.txt".into(),
                    text: Some("disk text".into()),
                    content_hash: Some("disk-hash".into()),
                    size: 9,
                    modified_at: None,
                    encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                    line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                    read_only_reason: None,
                    truncated: false,
                });
                let theme = Theme::of(cx).clone();
                let editor = super::super::editor::new_file_editor(
                    "unsaved text",
                    "drawing.txt",
                    false,
                    &theme,
                    window,
                    cx,
                );
                document.editor = Some(editor.clone());
                document.mark_user_edit();
                surface.preview.active = Some("drawing.txt".into());
                surface
                    .preview
                    .documents
                    .insert("drawing.txt".into(), document);
                surface.rename_documents("drawing.txt", "drawing.svg".into(), cx);
                surface.open_file("drawing.svg".into(), cx);
                surface.reconcile_document("drawing.svg".into(), cx);
                let document = surface.preview.documents.get_mut("drawing.svg").unwrap();
                assert!(document.is_dirty());
                assert!(document.is_editable());
                assert_eq!(document.editor.as_ref(), Some(&editor));
                assert!(document.image.is_none());
                assert!(matches!(
                    document.phase,
                    DocumentPhase::ExternallyModified { .. }
                ));
                // After resolving the external-change warning, saving still uses
                // the original buffer and cannot be cancelled by preview creation.
                document.phase = DocumentPhase::Ready;
                assert!(document.can_save());
                let pending = document.begin_save("unsaved text".into()).unwrap();
                surface.read_image_file("drawing.svg".into(), cx);
                let document = &surface.preview.documents["drawing.svg"];
                assert_eq!(document.pending_save.as_ref(), Some(&pending));
                assert_eq!(document.editor.as_ref(), Some(&editor));
            })
            .unwrap();
    }

    #[gpui::test]
    fn markdown_preview_web_links_keep_the_file_session_and_full_target(cx: &mut TestAppContext) {
        use crate::markdown::render::{self, LinkAction, LinkTarget};
        use std::cell::RefCell;

        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "owner".into(), false, 1000, 13.0, false, false, cx)
        });
        let (owner, preview) = window
            .update(cx, |surface, _, cx| {
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "owner".into(),
                    checkout_id: Some("checkout".into()),
                    path: "README.md".into(),
                });
                document.set_loaded(slopty_view_proto::WorkspaceFileText {
                    checkout_id: "checkout".into(),
                    path: "README.md".into(),
                    text: Some("[Docs](https://example.com/docs)".into()),
                    content_hash: Some("hash".into()),
                    size: 32,
                    modified_at: None,
                    encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                    line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                    read_only_reason: None,
                    truncated: false,
                });
                document.show_markdown = true;
                surface.preview.active = Some("README.md".into());
                surface
                    .preview
                    .documents
                    .insert("README.md".into(), document);
                let preview = surface
                    .prepare_markdown_preview("README.md", None, cx)
                    .unwrap();
                // Changing the selected chat must not rewrite this file's owner.
                surface
                    .state
                    .update(cx, |state, _| state.selected_chat = Some("other".into()));
                (cx.entity(), preview)
            })
            .unwrap();
        let received = Rc::new(RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            cx.subscribe(&owner, {
                let received = received.clone();
                let preview = preview.clone();
                move |_, event, cx| {
                    if let FilesEvent::OpenWebLink(activation) = event {
                        received.borrow_mut().push(activation.clone());
                        // Browser selection can suspend this same preview. This
                        // must run after its owner/preview borrows have unwound.
                        preview.update(cx, |preview, cx| preview.suspend(cx));
                    }
                }
            })
        });
        let ui = preview.update(cx, |preview, cx| preview.link_ui(cx));
        assert!(
            ui.source_session.is_none(),
            "file preview labels are not truncated"
        );
        let target = LinkTarget::new("Different label", "https://example.com/docs?q=%C3%B1#full");
        for action in [
            LinkAction::Primary,
            LinkAction::Internal,
            LinkAction::External,
            LinkAction::Copy,
        ] {
            cx.update_window(window.into(), |_, window, cx| {
                render::activate_link(target.clone(), action, Some(&ui), window, cx);
            })
            .unwrap();
        }
        cx.run_until_parked();
        let events = received.borrow();
        assert_eq!(events.len(), 3);
        for (event, action) in events.iter().zip([
            LinkAction::Primary,
            LinkAction::Internal,
            LinkAction::External,
        ]) {
            assert_eq!(event.target, target);
            assert_eq!(event.action, action);
            assert_eq!(event.source_session.as_deref(), Some("owner"));
        }
        drop(events);
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(target.original.as_str())
            )
        });
        assert!(
            cx.opened_url().is_none(),
            "only the shell may open web destinations"
        );
        for url in [
            "javascript:alert(1)",
            "https://user:secret@example.com",
            "https://example.com/%ZZ",
            "https://example.com/\n",
        ] {
            for action in [LinkAction::Internal, LinkAction::External] {
                cx.update_window(window.into(), |_, window, cx| {
                    render::activate_link(
                        LinkTarget::new("Bad", url),
                        action,
                        Some(&ui),
                        window,
                        cx,
                    );
                })
                .unwrap();
            }
        }
        cx.run_until_parked();
        assert_eq!(received.borrow().len(), 3);
        assert!(cx.opened_url().is_none());
        // Mail links retain their existing OS handler.
        cx.update_window(window.into(), |_, window, cx| {
            render::activate_link(
                LinkTarget::new("Mail", "mailto:hello@example.com"),
                LinkAction::Internal,
                Some(&ui),
                window,
                cx,
            );
        })
        .unwrap();
        assert_eq!(cx.opened_url().as_deref(), Some("mailto:hello@example.com"));
    }

    #[gpui::test]
    fn preview_reads_unsaved_buffer_without_starting_a_save(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new(state, "chat".into(), false, 1000, 13.0, false, false, cx)
        });
        let preview = window
            .update(cx, |surface, window, cx| {
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: "README.md".into(),
                });
                document.set_loaded(slopty_view_proto::WorkspaceFileText {
                    checkout_id: "checkout".into(),
                    path: "README.md".into(),
                    text: Some("# Disk".into()),
                    content_hash: Some("disk-hash".into()),
                    size: 6,
                    modified_at: None,
                    encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
                    line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
                    read_only_reason: None,
                    truncated: false,
                });
                let theme = Theme::of(cx).clone();
                document.editor = Some(super::super::editor::new_file_editor(
                    "# Unsaved",
                    "README.md",
                    false,
                    &theme,
                    window,
                    cx,
                ));
                document.mark_user_edit();
                document.show_markdown = true;
                surface.preview.active = Some("README.md".into());
                surface
                    .preview
                    .documents
                    .insert("README.md".into(), document);
                let editor = surface.preview.documents["README.md"].editor.clone();
                surface
                    .prepare_markdown_preview("README.md", editor.as_ref(), cx)
                    .unwrap();
                let document = &surface.preview.documents["README.md"];
                assert!(document.is_dirty());
                assert!(document.save_task.is_none());
                assert!(document.autosave_task.is_none());
                document.markdown.clone().unwrap()
            })
            .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        preview.read_with(cx, |preview, _| {
            let crate::markdown::parser::Block::Heading { runs, .. } =
                &preview.test_tree().blocks[0].block
            else {
                panic!("missing heading");
            };
            assert_eq!(runs[0].text, "Unsaved");
        });
        let weak = preview.downgrade();
        drop(preview);
        window
            .update(cx, |surface, _, cx| {
                surface.preview.documents.clear();
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        assert!(weak.upgrade().is_none());
    }
}

/// Issue #280: macOS aborted in `MacTextSystem::layout_line` because a font run
/// ended inside '→'. These tests drive a clean external reload through the real
/// surface, editor and adapter, holding the reloaded text's highlight task until
/// the frame that applies the reload has been drawn.
#[cfg(test)]
mod reload_highlight_tests {
    use super::super::editor_adapter::probe;
    use super::*;
    use gpui::{AppContext, TestAppContext};
    use std::ops::Range;

    const RELOAD_PATH: &str = "repro-unicode.json";
    const RELOAD_BEFORE: &str = r#"{"": 1}"#;
    const RELOAD_AFTER: &str = "\" →\"";

    /// Whether the reloaded text's highlight exists when the reload is applied.
    #[derive(Clone, Copy)]
    enum ReloadHighlight {
        /// Highlighted by the task the reload spawns, delivered after the
        /// reload frame.
        Pending,
        /// Already in the syntax cache, so the reload installs it immediately.
        Cached,
    }

    struct ReloadTrace {
        /// Adapter events while the original text was highlighted and drawn.
        loaded: Vec<probe::Event>,
        /// Adapter events of the frame that applied the reload.
        reload_frame: Vec<probe::Event>,
        /// Whether that frame was drawn before the reloaded text's highlight
        /// had been delivered.
        reload_highlight_pending: bool,
        /// Editor text right after the reload frame.
        editor_text: String,
        /// Adapter events once the reloaded text's own highlight is installed.
        rehighlighted_frame: Vec<probe::Event>,
    }

    fn workspace_file(path: &str, text: &str, hash: &str) -> slopty_view_proto::WorkspaceFileText {
        slopty_view_proto::WorkspaceFileText {
            checkout_id: "checkout".into(),
            path: path.into(),
            text: Some(text.into()),
            content_hash: Some(hash.into()),
            size: text.len() as u64,
            modified_at: None,
            encoding: slopty_view_proto::WorkspaceTextEncoding::Utf8,
            line_ending: Some(slopty_view_proto::WorkspaceLineEnding::Lf),
            read_only_reason: None,
            truncated: false,
        }
    }

    fn trace_clean_external_reload(
        cx: &mut TestAppContext,
        path: &'static str,
        before: &str,
        after: &str,
        highlight: ReloadHighlight,
    ) -> ReloadTrace {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| crate::state::AppState::new());
            FilesSurface::new_editor(
                state,
                "chat".into(),
                path.into(),
                false,
                1000,
                13.0,
                false,
                false,
                cx,
            )
        });
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window.into(), |_, window, cx| {
                window.refresh();
                let _ = window.draw(cx);
                window.simulate_next_frame(cx);
            })
            .unwrap();
        };
        probe::take();
        window
            .update(cx, |surface, _, cx| {
                let mut document = FileDocument::loading(DocumentKey {
                    chat_id: "chat".into(),
                    checkout_id: Some("checkout".into()),
                    path: path.into(),
                });
                document.set_loaded(workspace_file(path, before, "hash-before"));
                surface.preview.active = Some(path.into());
                surface.preview.documents.insert(path.into(), document);
                surface.sync_preview_list();
                surface.request_file_highlight(
                    path.into(),
                    before.into(),
                    "hash-before".into(),
                    cx,
                );
            })
            .unwrap();
        // Deliver the original highlight, then let the surface create the
        // editor and install it, as when a highlighted file is opened.
        cx.run_until_parked();
        for _ in 0..3 {
            draw(cx);
        }
        let loaded = probe::take();

        // A clean disk change is queued by reconciliation and applied by the
        // next render of the document body.
        window
            .update(cx, |surface, _, cx| {
                if let ReloadHighlight::Cached = highlight {
                    let document = slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                        source: after,
                        path: Some(path),
                        fence_tag: None,
                    })
                    .unwrap();
                    surface.preview.syntax_cache.insert(
                        DocumentHighlightKey::new(
                            slopty_syntax::language_for_path(path).unwrap(),
                            after,
                        ),
                        Arc::new(document),
                    );
                }
                surface
                    .preview
                    .documents
                    .get_mut(path)
                    .unwrap()
                    .queue_external_reload(workspace_file(path, after, "hash-after"));
                cx.notify();
            })
            .unwrap();
        draw(cx);
        let reload_frame = probe::take();
        let (reload_highlight_pending, editor_text) = window
            .update(cx, |surface, _, cx| {
                let document = &surface.preview.documents[path];
                let editor = document.editor.as_ref().expect("editor created");
                (
                    document.highlight_task.is_some(),
                    editor.read(cx).value().to_string(),
                )
            })
            .unwrap();

        cx.run_until_parked();
        draw(cx);
        let rehighlighted_frame = probe::take();

        ReloadTrace {
            loaded,
            reload_frame,
            reload_highlight_pending,
            editor_text,
            rehighlighted_frame,
        }
    }

    /// The character of `text` whose UTF-8 encoding contains `byte`.
    fn char_around(text: &str, byte: usize) -> String {
        let (start, ch) = text
            .char_indices()
            .take_while(|(start, _)| *start <= byte)
            .last()
            .unwrap();
        format!("{ch:?} (bytes {start}..{})", start + ch.len_utf8())
    }

    fn range_violation(text: &str, range: &Range<usize>) -> Option<String> {
        if range.start > range.end || range.end > text.len() {
            return Some(format!(
                "{range:?} is not inside the {}-byte text",
                text.len()
            ));
        }
        [range.start, range.end]
            .into_iter()
            .find(|edge| !text.is_char_boundary(*edge))
            .map(|edge| {
                format!(
                    "{range:?} has edge {edge} inside {}",
                    char_around(text, edge)
                )
            })
    }

    /// Every way the adapter's events break the contract with the text the
    /// editor shapes: a highlighter the factory built for another text keeps
    /// none of its spans, kept spans lie inside the text, and style runs are
    /// ordered, contiguous, cover each request and sit on UTF-8 boundaries.
    fn adapter_contract_violations(text: &str, events: &[probe::Event]) -> Vec<String> {
        let mut violations = Vec::new();
        let mut stale_build = None;
        for (index, event) in events.iter().enumerate() {
            match event {
                probe::Event::FactoryBuilt { source } => {
                    stale_build = (source != text).then_some((index, source));
                }
                probe::Event::Updated {
                    edit,
                    new_end,
                    spans,
                } => {
                    if let Some((built, source)) = stale_build.take()
                        && !spans.is_empty()
                    {
                        violations.push(format!(
                            "event #{index}: highlighter built at event #{built} from {source:?} \
                             kept spans {spans:?} while the editor holds {text:?}"
                        ));
                    } else if let Some(problem) =
                        spans.iter().find_map(|span| range_violation(text, span))
                    {
                        violations.push(format!(
                            "event #{index}: after edit {edit:?}->..{new_end} the highlighter \
                             kept span {problem}"
                        ));
                    }
                }
                probe::Event::Styles { request, runs } => {
                    if let Some(problem) = range_violation(text, request) {
                        violations.push(format!("event #{index}: styles request {problem}"));
                        continue;
                    }
                    let mut cursor = request.start;
                    let mut broken = false;
                    for (run_index, (range, _)) in runs.iter().enumerate() {
                        let problem = if range.start != cursor || range.end > request.end {
                            Some(format!(
                                "{range:?} does not continue at byte {cursor} within {request:?}"
                            ))
                        } else {
                            range_violation(text, range)
                        };
                        if let Some(problem) = problem {
                            violations.push(format!(
                                "event #{index}: style run #{run_index} {problem}; runs {:?}",
                                runs.iter().map(|(range, _)| range).collect::<Vec<_>>()
                            ));
                            broken = true;
                            break;
                        }
                        cursor = range.end;
                    }
                    if !broken && cursor < request.end {
                        violations.push(format!(
                            "event #{index}: style runs stop at {cursor} before {request:?} ends"
                        ));
                    }
                }
            }
        }
        violations
    }

    fn assert_loaded_highlight_is_valid(before: &str, trace: &ReloadTrace) {
        assert!(
            trace.loaded.iter().any(|event| matches!(
                event,
                probe::Event::Styles { runs, .. }
                    if runs.windows(2).any(|pair| pair[0].1 != pair[1].1)
            )),
            "the original text was never drawn with more than one style: {:#?}",
            trace.loaded
        );
        assert_eq!(
            adapter_contract_violations(before, &trace.loaded),
            Vec::<String>::new()
        );
    }

    #[gpui::test]
    fn clean_external_reload_only_shapes_highlights_for_the_reloaded_text(cx: &mut TestAppContext) {
        let trace = trace_clean_external_reload(
            cx,
            RELOAD_PATH,
            RELOAD_BEFORE,
            RELOAD_AFTER,
            ReloadHighlight::Pending,
        );
        assert_loaded_highlight_is_valid(RELOAD_BEFORE, &trace);
        assert_eq!(trace.editor_text, RELOAD_AFTER);
        assert!(
            trace.reload_highlight_pending,
            "the reload frame must be drawn while the reloaded text's highlight is pending"
        );
        assert_eq!(
            adapter_contract_violations(RELOAD_AFTER, &trace.rehighlighted_frame),
            Vec::<String>::new(),
            "after its own highlight arrives the reloaded text must be valid"
        );
        let violations = adapter_contract_violations(RELOAD_AFTER, &trace.reload_frame);
        assert!(
            violations.is_empty(),
            "frame applying the reload broke the highlight contract.\n\
             First violation: {}\nAll violations:\n  {}\nAdapter events: {:#?}",
            violations[0],
            violations.join("\n  "),
            trace.reload_frame
        );
    }

    /// Control: the same reload with the new text's highlight already cached
    /// installs a fresh factory before the editor renders.
    #[gpui::test]
    fn clean_external_reload_with_cached_highlight_shapes_only_reloaded_text(
        cx: &mut TestAppContext,
    ) {
        let trace = trace_clean_external_reload(
            cx,
            RELOAD_PATH,
            RELOAD_BEFORE,
            RELOAD_AFTER,
            ReloadHighlight::Cached,
        );
        assert_loaded_highlight_is_valid(RELOAD_BEFORE, &trace);
        assert_eq!(trace.editor_text, RELOAD_AFTER);
        assert!(!trace.reload_highlight_pending);
        assert!(trace.reload_frame.iter().any(|event| matches!(
            event,
            probe::Event::FactoryBuilt { source } if source == RELOAD_AFTER
        )));
        assert_eq!(
            adapter_contract_violations(RELOAD_AFTER, &trace.reload_frame),
            Vec::<String>::new()
        );
    }
}
