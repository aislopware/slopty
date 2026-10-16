//! The files a visual or a page uses, read for a client by their item and name.
//!
//! [`ContentRef::item_asset`] names them: a visual's assets by the names its call gave them, a
//! page file's sub-resources by their paths under its folder.
//!
//! Only what the item names is read. A visual's asset is the path its call put under that
//! name; a page's sub-resource is a relative path that stays under the page file's folder once
//! every link in it is followed: an absolute path, a `..` step, or a link out of the folder is
//! refused. A file larger than [`MAX_ASSET_BYTES`] is never read, as the client never draws one.

use std::path::{Component, Path, PathBuf};

use slopty_proto::thread::wire::Expanded;
use slopty_proto::thread::{ContentRef, ItemBody, ThreadState, ToolDetail};
use slopty_proto::visual::PageSource;

/// The largest file a visual or a page may use, as the client draws them
/// (`slopty_view_proto::agent::VISUAL_MAX_ASSET_BYTES`).
pub const MAX_ASSET_BYTES: u64 = 8 * 1024 * 1024;

/// Where the file `content` names lives, in `state`'s items: `None` when `content` names no
/// item's asset, or the item or the name is unknown or refused.
#[must_use]
pub fn path_of(state: &ThreadState, content: &ContentRef) -> Option<PathBuf> {
    let (item, name) = content.item_asset_of()?;
    let call = state.items.iter().rev().find_map(|i| match &i.body {
        ItemBody::Tool(call) if i.id.0 == item => Some(call),
        _ => None,
    })?;
    match call.detail.as_ref()? {
        ToolDetail::Visual(visual) => visual.assets.get(name).map(PathBuf::from),
        ToolDetail::Page(page) => match &page.source {
            PageSource::File(file) => under(Path::new(file).parent()?, name),
            PageSource::Html(_) | PageSource::App(_) => None,
        },
        _ => None,
    }
}

/// `name` under `dir`, every link followed, when it stays there.
fn under(dir: &Path, name: &str) -> Option<PathBuf> {
    let relative = Path::new(name);
    if name.is_empty() || !relative.components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    let dir = dir.canonicalize().ok()?;
    let found = dir.join(relative).canonicalize().ok()?;
    found.starts_with(&dir).then_some(found)
}

/// The bytes of the file `content` names in `state`, on the blocking pool: gone when it names
/// none, is missing, or is larger than [`MAX_ASSET_BYTES`].
pub async fn read(state: &ThreadState, content: &ContentRef) -> Expanded {
    let Some(path) = path_of(state, content) else { return Expanded::Gone };
    tokio::task::spawn_blocking(move || read_file(&path)).await.unwrap_or(Expanded::Gone)
}

fn read_file(path: &Path) -> Expanded {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else { return Expanded::Gone };
    let fits = file.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_ASSET_BYTES);
    if !fits {
        return Expanded::Gone;
    }
    // Read no further than the cap, should the file grow meanwhile.
    let mut bytes = Vec::new();
    match file.take(MAX_ASSET_BYTES.saturating_add(1)).read_to_end(&mut bytes) {
        Ok(_) if u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_ASSET_BYTES) => {
            Expanded::Bytes(bytes)
        }
        Ok(_) | Err(_) => Expanded::Gone,
    }
}

#[cfg(test)]
mod tests;
