//! The Files surface: a workspace's tree, its search, a file read and saved, an entry moved or
//! trashed, and the tree's and git's changes watched, through the worker's file verbs.
//!
//! Paths in the shell's calls are relative to the workspace's folder
//! ([`WorkspaceView::surface_place`]); an absolute path reads a file outside it, which is never
//! written. A file's version is its size and modification time, which is what the worker's save
//! checks a conflict by.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt as _;
use gpui::{Context, Task};
use slopty_client::layout::WorkerKey;
use slopty_core::WallMs;
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::folder::{FsOp, FsOutcome, FsRefusal, Listing};
use slopty_proto::git::{GitDone, GitOp, GitOutcome};
use slopty_proto::orchestration::FileKind;
use slopty_proto::search::{SearchEvent, SearchQuery};
use slopty_view_proto::{
    CheckoutGitStatus, DeleteWorkspaceEntryRequest, GitFileState, GitFileStatus,
    ListWorkspaceDirectoryRequest, MoveWorkspaceEntryRequest, ReadWorkspaceFileRequest,
    ReadWorkspaceImageRequest, SearchWorkspaceContentRequest, SearchWorkspaceFilesRequest,
    WORKSPACE_IMAGE_CHUNK_BYTES, WatchWorkspaceFilesRequest, WorkspaceContentMatch,
    WorkspaceContentSearchResult, WorkspaceDirectoryPage, WorkspaceEntry, WorkspaceEntryKind,
    WorkspaceFileChange, WorkspaceFileChangeKind, WorkspaceFileChanges, WorkspaceFileSearchMatch,
    WorkspaceFileText, WorkspaceFileWriteResult, WorkspaceGitStatusFrame, WorkspaceImageChunk,
    WorkspaceLineEnding, WorkspaceMutationCapabilities, WorkspaceMutationOutcome,
    WorkspaceMutationRejection, WorkspaceReadOnlyReason, WorkspaceTextEncoding,
    WorkspaceWritableEncoding, WorkspaceWritableLineEnding, WriteWorkspaceFileOutcome,
    WriteWorkspaceFileRequest,
};
use slopty_view_rpc::{RpcError, RpcReply};

use super::{absolute, ask, join, value, within};
use crate::view_model::rows;
use crate::workspace::WorkspaceView;

/// How often a watched tree's listed folders are looked at again.
const RELIST: Duration = Duration::from_secs(2);

/// How often a watched repository's status is asked again.
const RESTATUS: Duration = Duration::from_secs(3);

/// The most matches a search returns when the shell names no limit.
const MATCHES: usize = 200;

/// How long a text search runs before it answers with what it found.
const SEARCH_BUDGET: Duration = Duration::from_secs(8);

/// How many pictures are kept while the shell reads them in chunks.
const KEPT_IMAGES: usize = 4;

/// The name git keeps its repository under, never shown in a tree.
const GIT_DIR: &str = ".git";

/// What the files surfaces keep between calls.
#[derive(Default)]
pub(super) struct Kept {
    /// The folders the shell has listed, by worker and workspace, with what each held.
    listed: HashMap<(WorkerKey, String), BTreeMap<String, Vec<Seen>>>,
    /// Pictures being read in chunks, by worker and absolute path.
    images: VecDeque<((WorkerKey, String), Picture)>,
}

/// An entry of a listed folder, as it was.
#[derive(Clone, PartialEq, Eq)]
struct Seen {
    name: String,
    dir: bool,
    size: u64,
    modified: u64,
}

/// A picture read whole, served in chunks.
#[derive(Clone)]
struct Picture {
    version: String,
    mime: String,
    bytes: Bytes,
}

/// A file's version as the shell's content hash: its size and modification time.
fn version(size: u64, modified: WallMs) -> String {
    format!("{size}:{}", modified.as_millis())
}

/// The modification time a version names.
fn version_time(version: &str) -> Option<WallMs> {
    let (_, ms) = version.split_once(':')?;
    ms.parse().ok().map(WallMs::from_millis)
}

/// A wall time as the shell's.
fn time(at: WallMs) -> Option<chrono::DateTime<chrono::Utc>> {
    (at.as_millis() > 0).then(|| rows::time(at))
}

/// The checkout id of the workspace at `root` on `key`.
fn checkout_id(key: WorkerKey, root: &str) -> String {
    format!("{key}:{root}")
}

/// `rel`'s last part.
fn base_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// How a text's lines end.
fn line_ending(text: &str) -> WorkspaceLineEnding {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    match (crlf, lf) {
        (0, 0) => WorkspaceLineEnding::None,
        (0, _) => WorkspaceLineEnding::Lf,
        (c, l) if c == l => WorkspaceLineEnding::Crlf,
        _ => WorkspaceLineEnding::Mixed,
    }
}

/// Base64 of `bytes`, standard alphabet, padded.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let symbol = |six: u32| char::from(ALPHABET.get((six & 63) as usize).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let at = |ix: usize| u32::from(chunk.get(ix).copied().unwrap_or(0));
        let triple = (at(0) << 16) | (at(1) << 8) | at(2);
        out.push(symbol(triple >> 18));
        out.push(symbol(triple >> 12));
        out.push(if chunk.len() > 1 { symbol(triple >> 6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(triple) } else { '=' });
    }
    out
}

/// Why a change to the files was not made, in the shell's words.
fn rejected(operation_id: String, outcome: FsOutcome) -> WorkspaceMutationOutcome {
    let (reason, message) = match outcome {
        FsOutcome::Refused(refusal) => {
            let reason = match &refusal {
                FsRefusal::NotAbsolute { .. } | FsRefusal::BadName { .. } => {
                    WorkspaceMutationRejection::InvalidPath
                }
                FsRefusal::Protected { .. } => WorkspaceMutationRejection::PermissionDenied,
                FsRefusal::Clash { .. } => WorkspaceMutationRejection::DestinationExists,
                FsRefusal::Missing { .. } => WorkspaceMutationRejection::SourceMissing,
                FsRefusal::IntoItself => WorkspaceMutationRejection::InvalidDestination,
                FsRefusal::OtherVolume | FsRefusal::NoTrash => {
                    WorkspaceMutationRejection::Unsupported
                }
                FsRefusal::Changed { .. } => WorkspaceMutationRejection::SourceChanged,
            };
            (reason, refusal_words(&refusal))
        }
        FsOutcome::Failed { error } => (WorkspaceMutationRejection::PartialFailure, error),
        FsOutcome::Done { .. } => (WorkspaceMutationRejection::Busy, String::new()),
    };
    WorkspaceMutationOutcome::Rejected { operation_id, reason, message }
}

/// A refusal for a person to read.
fn refusal_words(refusal: &FsRefusal) -> String {
    match refusal {
        FsRefusal::NotAbsolute { path } => format!("{path} is not a full path"),
        FsRefusal::BadName { name } => format!("“{name}” cannot be a name"),
        FsRefusal::Protected { path } => format!("{path} is protected"),
        FsRefusal::Clash { path } => format!("{path} already exists"),
        FsRefusal::Missing { path } => format!("{path} is not there any more"),
        FsRefusal::IntoItself => "A folder cannot go into itself".to_owned(),
        FsRefusal::OtherVolume => "It cannot move to another disk".to_owned(),
        FsRefusal::NoTrash => "That disk has no Trash".to_owned(),
        FsRefusal::Changed { .. } => "It changed since it was read".to_owned(),
    }
}

/// A git status letter as the shell's state.
const fn git_state(letter: char) -> GitFileState {
    match letter {
        'M' => GitFileState::Modified,
        'A' => GitFileState::Added,
        'D' => GitFileState::Deleted,
        'R' => GitFileState::Renamed,
        'C' => GitFileState::Copied,
        'U' => GitFileState::Unmerged,
        'T' => GitFileState::TypeChanged,
        '?' => GitFileState::Untracked,
        _ => GitFileState::Unchanged,
    }
}

/// A repository's status as the shell's; `None` when it is not one.
fn git_status(key: WorkerKey, root: &str, outcome: GitOutcome) -> Option<CheckoutGitStatus> {
    let GitOutcome::Done(GitDone::Status(status)) = outcome else { return None };
    // git names paths from the repository's root; the shell's are from the workspace's, which
    // may be a folder inside it.
    let inside = root
        .strip_prefix(status.root.trim_end_matches('/'))
        .map(|rest| rest.trim_matches('/'))
        .filter(|rest| !rest.is_empty());
    let local = |path: &str| -> Option<String> {
        match inside {
            None => Some(path.to_owned()),
            Some(inside) => path.strip_prefix(inside)?.strip_prefix('/').map(str::to_owned),
        }
    };
    let files: Vec<GitFileStatus> = status
        .files
        .iter()
        .filter(|file| file.xy != "!!")
        .filter_map(|file| {
            let path = local(&file.path)?;
            let mut letters = file.xy.chars();
            let (x, y) = (letters.next().unwrap_or('.'), letters.next().unwrap_or('.'));
            let unmerged =
                matches!(file.xy.as_str(), "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU");
            let (index, worktree) = if unmerged {
                (GitFileState::Unmerged, GitFileState::Unmerged)
            } else {
                (git_state(x), git_state(y))
            };
            let old_path = file.from.as_deref().and_then(local);
            Some(GitFileStatus { path, old_path, index, worktree })
        })
        .collect();
    let mut hasher = DefaultHasher::new();
    for file in &status.files {
        (&file.path, &file.xy).hash(&mut hasher);
    }
    status.head.hash(&mut hasher);
    Some(CheckoutGitStatus {
        checkout_id: checkout_id(key, root),
        device_id: key.to_string(),
        revision: format!("{:016x}", hasher.finish()),
        complete: status.more == 0,
        files,
    })
}

/// A listed folder's entries as the shell's, `dir` the folder relative to the workspace.
fn entries(dir: &str, listed: &[slopty_proto::folder::FolderEntry]) -> Vec<WorkspaceEntry> {
    listed
        .iter()
        .filter(|entry| entry.name != GIT_DIR)
        .map(|entry| WorkspaceEntry {
            mutation_revision: Some(version(entry.size, entry.modified_ms)),
            path: join(dir, &entry.name),
            name: entry.name.clone(),
            kind: match entry.kind {
                FileKind::Dir => WorkspaceEntryKind::Directory,
                FileKind::Symlink => WorkspaceEntryKind::Symlink,
                FileKind::File | FileKind::Other => WorkspaceEntryKind::File,
            },
            size: (entry.kind != FileKind::Dir).then_some(entry.size),
            modified_at: time(entry.modified_ms),
            ignored: false,
            read_only: false,
        })
        .collect()
}

/// What a folder's entries are now, to tell its changes by.
fn seen(listed: &[slopty_proto::folder::FolderEntry]) -> Vec<Seen> {
    listed
        .iter()
        .filter(|entry| entry.name != GIT_DIR)
        .map(|entry| Seen {
            name: entry.name.clone(),
            dir: entry.kind == FileKind::Dir,
            size: entry.size,
            modified: entry.modified_ms.as_millis(),
        })
        .collect()
}

/// The changes between a folder's entries `was` and `now`, `dir` the folder.
fn changes(dir: &str, was: &[Seen], now: &[Seen]) -> Vec<WorkspaceFileChange> {
    let change = |kind, name: &str| WorkspaceFileChange {
        operation_id: None,
        kind,
        path: join(dir, name),
        old_path: None,
    };
    let mut out = Vec::new();
    for entry in now {
        match was.iter().find(|w| w.name == entry.name) {
            None => out.push(change(WorkspaceFileChangeKind::Created, &entry.name)),
            Some(w) if !entry.dir && (w.size != entry.size || w.modified != entry.modified) => {
                out.push(change(WorkspaceFileChangeKind::Modified, &entry.name));
            }
            Some(_) => {}
        }
    }
    for gone in was.iter().filter(|w| !now.iter().any(|n| n.name == w.name)) {
        out.push(change(WorkspaceFileChangeKind::Removed, &gone.name));
    }
    out
}

/// A call's parameters as its request.
fn request<T: serde::de::DeserializeOwned>(params: serde_json::Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))
}

/// A failed call, answered at once.
fn fail(error: RpcError) -> Task<Result<RpcReply, RpcError>> {
    Task::ready(Err(error))
}

impl WorkspaceView {
    /// `ListWorkspaceDirectory`: a folder of the workspace, its entries relative to it.
    pub(super) fn list_directory(
        &mut self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: ListWorkspaceDirectoryRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        let dir = request.directory.trim_matches('/').to_owned();
        let rx = self.surface_list(key, join(&root, &dir));
        cx.spawn(async move |this, cx| match within(cx, rx).await? {
            Listing::Listed { entries: listed, total, .. } => {
                let kept = seen(&listed);
                let workspace = (key, root.clone());
                let at = dir.clone();
                this.update(cx, |view, _| {
                    let files = &mut view.view_model.surfaces.files;
                    files.listed.entry(workspace).or_default().insert(at, kept);
                })
                .map_err(super::gone)?;
                let shown = u32::try_from(listed.len()).unwrap_or(u32::MAX);
                value(&WorkspaceDirectoryPage {
                    checkout_id: Some(checkout_id(key, &root)),
                    mutation_capabilities: Some(WorkspaceMutationCapabilities {
                        move_entry: true,
                        delete_entry: true,
                    }),
                    directory: dir.clone(),
                    entries: entries(&dir, &listed),
                    next_cursor: None,
                    truncated: total > shown,
                })
            }
            Listing::NotFolder => Err(RpcError::Failed(format!("{dir} is not a folder"))),
            Listing::Missing { error } => Err(RpcError::Failed(error)),
        })
    }

    /// `SearchWorkspaceFiles`: the workspace's paths a query matches, best first.
    pub(super) fn search_files(
        &mut self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: SearchWorkspaceFilesRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        let limit = request.limit.map_or(MATCHES, usize::from);
        let rx = self.surface_find(key, root, request.query);
        cx.spawn(async move |_, cx| {
            let paths = within(cx, rx).await?;
            let count = i64::try_from(paths.len()).unwrap_or(i64::MAX);
            let matches: Vec<WorkspaceFileSearchMatch> = paths
                .iter()
                .take(limit)
                .zip(0_i64..)
                .map(|(path, rank)| {
                    let dir = path.ends_with('/');
                    let path = path.trim_end_matches('/');
                    WorkspaceFileSearchMatch {
                        path: path.to_owned(),
                        name: base_name(path).to_owned(),
                        kind: if dir {
                            WorkspaceEntryKind::Directory
                        } else {
                            WorkspaceEntryKind::File
                        },
                        score: count.saturating_sub(rank),
                    }
                })
                .collect();
            value(&matches)
        })
    }

    /// `SearchWorkspaceContent`: the lines of the workspace's files a text is on, smart case,
    /// as many as the limit or the time budget lets.
    pub(super) fn search_content(
        &mut self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: SearchWorkspaceContentRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        let limit = request.limit.map_or(MATCHES, usize::from);
        let per_file = request.per_file_limit.map_or(usize::MAX, usize::from);
        let query = SearchQuery {
            match_case: request.query.chars().any(char::is_uppercase),
            pattern: request.query,
            ..SearchQuery::default()
        };
        let (id, mut events) = self.surface_search(key, root, query);
        cx.spawn(async move |this, cx| {
            let mut matches = Vec::new();
            let mut truncated = false;
            let budget = cx.background_executor().timer(SEARCH_BUDGET);
            let mut budget = std::pin::pin!(futures::FutureExt::fuse(budget));
            loop {
                let event = futures::select_biased! {
                    event = futures::FutureExt::fuse(events.recv()) => event,
                    () = budget => { truncated = true; None }
                };
                match event {
                    Some(SearchEvent::Hits { files, .. }) => {
                        for file in files {
                            for hit in file.lines.iter().take(per_file) {
                                matches.push(content_match(&file.path, hit));
                            }
                        }
                        if matches.len() >= limit {
                            matches.truncate(limit);
                            truncated = true;
                            break;
                        }
                    }
                    Some(SearchEvent::Done { summary, .. }) => {
                        truncated |= summary.capped;
                        break;
                    }
                    Some(SearchEvent::Failed { error, .. }) => return Err(RpcError::Failed(error)),
                    None => break,
                }
            }
            let _gone = this.update(cx, |view, _| view.surface_stop_search(key, id));
            value(&WorkspaceContentSearchResult { matches, truncated, indexing: false })
        })
    }

    /// `ReadWorkspaceFile`: a file's text with its version, or why it is not editable.
    pub(super) fn read_file(
        &mut self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: ReadWorkspaceFileRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        let outside = absolute(&request.path);
        let path = join(&root, &request.path);
        let rx = self.surface_read(key, path);
        let checkout = checkout_id(key, &root);
        cx.spawn(async move |_, cx| {
            let read = within(cx, rx).await?;
            let mut text = WorkspaceFileText {
                checkout_id: checkout,
                path: request.path,
                text: None,
                content_hash: None,
                size: 0,
                modified_at: None,
                encoding: WorkspaceTextEncoding::Binary,
                line_ending: None,
                read_only_reason: outside.then_some(WorkspaceReadOnlyReason::OutsideWorkspace),
                truncated: false,
            };
            match read {
                FileRead::Text { text: body, size, modified_ms, final_newline, .. } => {
                    let mut body = body;
                    if final_newline {
                        body.push('\n');
                    }
                    let bom = body.starts_with('\u{feff}');
                    if bom {
                        body.remove(0);
                    }
                    let ending = line_ending(&body);
                    if ending == WorkspaceLineEnding::Mixed {
                        text.read_only_reason
                            .get_or_insert(WorkspaceReadOnlyReason::MixedLineEndings);
                    }
                    text.encoding = if bom {
                        WorkspaceTextEncoding::Utf8Bom
                    } else {
                        WorkspaceTextEncoding::Utf8
                    };
                    text.line_ending = Some(ending);
                    text.content_hash = Some(version(size, modified_ms));
                    text.size = size;
                    text.modified_at = time(modified_ms);
                    text.text = Some(body);
                }
                FileRead::Binary { size } => {
                    text.size = size;
                    text.read_only_reason.get_or_insert(WorkspaceReadOnlyReason::Binary);
                }
                FileRead::Media { bytes, modified_ms, .. } => {
                    let size = bytes.len() as u64;
                    text.size = size;
                    text.content_hash = Some(version(size, modified_ms));
                    text.modified_at = time(modified_ms);
                    text.read_only_reason.get_or_insert(WorkspaceReadOnlyReason::Binary);
                }
                FileRead::TooLarge { size } => {
                    text.size = size;
                    text.encoding = WorkspaceTextEncoding::Unsupported;
                    text.truncated = true;
                    text.read_only_reason.get_or_insert(WorkspaceReadOnlyReason::TooLarge);
                }
                FileRead::Missing { error } => return Err(RpcError::Failed(error)),
                FileRead::Absent { .. } => {
                    return Err(RpcError::Failed(format!("{} is not there", text.path)));
                }
                FileRead::Streamed { .. } => {
                    return Err(RpcError::Failed("the file is still on its way".to_owned()));
                }
            }
            value(&text)
        })
    }

    /// `ReadWorkspaceImage`: a picture in chunks, read whole once and kept while it is read.
    pub(super) fn read_image(
        &self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: ReadWorkspaceImageRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        let path = join(&root, &request.path);
        let at = (key, path.clone());
        let kept = (request.offset > 0)
            .then(|| {
                let images = &self.view_model.surfaces.files.images;
                images.iter().find(|(id, _)| *id == at).map(|(_, picture)| picture.clone())
            })
            .flatten();
        cx.spawn(async move |this, cx| {
            let picture = if let Some(picture) = kept {
                picture
            } else {
                let read = ask(&this, cx, |v| v.surface_read(key, path.clone())).await?;
                let picture = match read {
                    FileRead::Media { media_type, bytes, modified_ms } => Picture {
                        version: version(bytes.len() as u64, modified_ms),
                        mime: media_type,
                        bytes,
                    },
                    FileRead::Text { text, size, modified_ms, final_newline, .. }
                        if std::path::Path::new(&path)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("svg")) =>
                    {
                        let mut text = text;
                        if final_newline {
                            text.push('\n');
                        }
                        Picture {
                            version: version(size, modified_ms),
                            mime: "image/svg+xml".to_owned(),
                            bytes: Bytes::from(text),
                        }
                    }
                    FileRead::Missing { error } => return Err(RpcError::Failed(error)),
                    _ => return Err(RpcError::Failed(format!("{path} is not a picture"))),
                };
                let keep = (at, picture.clone());
                this.update(cx, |view, _| {
                    let images = &mut view.view_model.surfaces.files.images;
                    images.retain(|(id, _)| *id != keep.0);
                    if images.len() >= KEPT_IMAGES {
                        images.pop_front();
                    }
                    images.push_back(keep);
                })
                .map_err(super::gone)?;
                picture
            };
            if request.expected_content_hash.as_ref().is_some_and(|hash| *hash != picture.version) {
                return Err(RpcError::Failed("the picture changed while it was read".to_owned()));
            }
            let size = picture.bytes.len();
            let start = request.offset.min(size);
            let end = start.saturating_add(WORKSPACE_IMAGE_CHUNK_BYTES).min(size);
            let data = base64(picture.bytes.get(start..end).unwrap_or_default());
            value(&WorkspaceImageChunk {
                checkout_id: request.expected_checkout_id,
                content_hash: picture.version,
                mime_type: picture.mime,
                data,
                next_offset: end,
                size,
                done: end == size,
            })
        })
    }

    /// `WriteWorkspaceFile`: save a file, refused when it changed on disk since it was read.
    pub(super) fn write_file(
        &mut self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: WriteWorkspaceFileRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        if absolute(&request.path) {
            return fail(RpcError::Failed("a file outside the workspace is not written".into()));
        }
        let mut text = request.text.replace("\r\n", "\n");
        if request.line_ending == WorkspaceWritableLineEnding::Crlf {
            text = text.replace('\n', "\r\n");
        }
        if request.encoding == WorkspaceWritableEncoding::Utf8Bom {
            text.insert(0, '\u{feff}');
        }
        let base = version_time(&request.expected_content_hash);
        let rx = self.surface_write(key, join(&root, &request.path), text, base);
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            WriteResult::Saved { size, modified_ms } => {
                value(&WriteWorkspaceFileOutcome::Written {
                    file: WorkspaceFileWriteResult {
                        path: request.path,
                        content_hash: version(size, modified_ms),
                        size,
                        modified_at: time(modified_ms),
                    },
                })
            }
            WriteResult::Conflict { modified_ms } => value(&WriteWorkspaceFileOutcome::Conflict {
                reason: slopty_view_proto::WorkspaceFileConflictReason::Changed,
                current_content_hash: None,
                current_modified_at: time(modified_ms),
            }),
            WriteResult::Failed { error } => Err(RpcError::Failed(error)),
        })
    }

    /// `MoveWorkspaceEntry`: rename or move an entry, never over another.
    pub(super) fn move_entry(
        &mut self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: MoveWorkspaceEntryRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        let op = FsOp::Move {
            from: join(&root, &request.source_path),
            to: join(&root, &request.destination_path),
        };
        let rx = self.surface_fs(key, op);
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            FsOutcome::Done { .. } => value(&WorkspaceMutationOutcome::Applied {
                operation_id: request.operation_id.clone(),
                checkout_id: request.expected_checkout_id,
                change: WorkspaceFileChange {
                    operation_id: Some(request.operation_id),
                    kind: WorkspaceFileChangeKind::Renamed,
                    path: request.destination_path,
                    old_path: Some(request.source_path),
                },
                entry: None,
            }),
            outcome => value(&rejected(request.operation_id, outcome)),
        })
    }

    /// `DeleteWorkspaceEntry`: move an entry to the worker's Trash, where it can be put back.
    pub(super) fn delete_entry(
        &mut self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(&params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let request: DeleteWorkspaceEntryRequest = match request(params) {
            Ok(request) => request,
            Err(error) => return fail(error),
        };
        let rx = self.surface_fs(key, FsOp::Trash { path: join(&root, &request.path) });
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            FsOutcome::Done { .. } => value(&WorkspaceMutationOutcome::Applied {
                operation_id: request.operation_id.clone(),
                checkout_id: request.expected_checkout_id,
                change: WorkspaceFileChange {
                    operation_id: Some(request.operation_id),
                    kind: WorkspaceFileChangeKind::Removed,
                    path: request.path,
                    old_path: None,
                },
                entry: None,
            }),
            outcome => value(&rejected(request.operation_id, outcome)),
        })
    }

    /// `WatchWorkspaceFiles`: the workspace's changes, as its listed folders show them, looked
    /// at again every [`RELIST`] while the watch is open.
    pub(super) fn watch_files(
        &self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        if let Err(error) = request::<WatchWorkspaceFilesRequest>(params.clone()) {
            return fail(error);
        }
        let (tx, rx) = futures::channel::mpsc::unbounded::<serde_json::Value>();
        let workspace = (key, root.clone());
        cx.spawn(async move |this, cx| {
            let mut sequence = 0_u64;
            while !tx.is_closed() {
                cx.background_executor().timer(RELIST).await;
                let Ok(dirs) = this.update(cx, |view, _| {
                    let listed = view.view_model.surfaces.files.listed.get(&workspace);
                    listed.map(|dirs| dirs.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()
                }) else {
                    return;
                };
                let mut found = Vec::new();
                for dir in dirs {
                    let path = join(&root, &dir);
                    let listing = match ask(&this, cx, |v| v.surface_list(key, path)).await {
                        Ok(listing) => listing,
                        Err(RpcError::Closed) => return,
                        Err(_) => continue,
                    };
                    let now = match listing {
                        Listing::Listed { entries, .. } => Some(seen(&entries)),
                        Listing::NotFolder | Listing::Missing { .. } => None,
                    };
                    let Ok(was) = this.update(cx, |view, _| {
                        let listed = view.view_model.surfaces.files.listed.entry(workspace.clone());
                        let dirs = listed.or_default();
                        match &now {
                            Some(now) => dirs.insert(dir.clone(), now.clone()),
                            None => dirs.remove(&dir),
                        }
                    }) else {
                        return;
                    };
                    if let (Some(was), Some(now)) = (was, now) {
                        found.extend(changes(&dir, &was, &now));
                    }
                }
                if found.is_empty() {
                    continue;
                }
                sequence = sequence.saturating_add(1);
                let frame =
                    WorkspaceFileChanges { sequence, resync_required: false, changes: found };
                let Ok(frame) = serde_json::to_value(&frame) else { return };
                if tx.unbounded_send(frame).is_err() {
                    return;
                }
            }
        })
        .detach();
        Task::ready(Ok(RpcReply::Stream(rx.boxed())))
    }

    /// `WatchWorkspaceGitStatus`: the workspace's repository status, sent when it changes.
    pub(super) fn watch_git_status(
        &self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (key, root) = match self.surface_place(params, cx) {
            Ok(place) => place,
            Err(error) => return fail(error),
        };
        let (tx, rx) = futures::channel::mpsc::unbounded::<serde_json::Value>();
        cx.spawn(async move |this, cx| {
            let mut last: Option<Option<String>> = None;
            while !tx.is_closed() {
                let repo = root.clone();
                let status = match ask(&this, cx, |v| v.surface_git(key, repo, GitOp::Status)).await
                {
                    Ok(outcome) => git_status(key, &root, outcome),
                    Err(RpcError::Closed) => return,
                    Err(_) => None,
                };
                let revision = status.as_ref().map(|status| status.revision.clone());
                if last.as_ref() != Some(&revision) {
                    last = Some(revision);
                    let frame = WorkspaceGitStatusFrame { status };
                    let Ok(frame) = serde_json::to_value(&frame) else { return };
                    if tx.unbounded_send(frame).is_err() {
                        return;
                    }
                }
                cx.background_executor().timer(RESTATUS).await;
            }
        })
        .detach();
        Task::ready(Ok(RpcReply::Stream(rx.boxed())))
    }
}

/// A line a search found as the shell's match: its leading space trimmed, the spans moved with it.
fn content_match(path: &str, hit: &slopty_proto::search::LineHit) -> WorkspaceContentMatch {
    let trimmed = hit.text.trim_start();
    let cut = u32::try_from(hit.text.len().saturating_sub(trimmed.len())).unwrap_or(0);
    let ranges: Vec<(u32, u32)> = hit
        .spans
        .iter()
        .map(|span| (span.start.saturating_sub(cut), span.end.saturating_sub(cut)))
        .collect();
    WorkspaceContentMatch {
        path: path.to_owned(),
        line: u64::from(hit.line),
        column: u64::from(hit.spans.first().map_or(0, |span| span.start)),
        preview: trimmed.to_owned(),
        ranges,
    }
}

#[cfg(test)]
mod tests;
