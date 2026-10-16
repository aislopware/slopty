//! Pictures an agent returned whose bytes its own record does not keep where the worker can read
//! them again: Codex's viewed and generated images and its MCP results, ACP image blocks, pi's
//! tool images.
//!
//! Claude Code's pictures stay in its transcript and are read from there
//! ([`crate::conversation::media`]). The other agents either hold them where only the agent reads
//! them (an ACP agent's own store, Codex's rollout) or name a file that may change later (Codex's
//! `imageView`). So each picture is kept here once, by the BLAKE3 of its bytes, and described as
//! an [`Image`] at `media:<digest>` ([`keep`]); the worker answers a client's `Expand` of one from
//! here ([`read`]).
//!
//! - **Where.** A folder the worker names at its start ([`set_dir`]), one file per picture named by
//!   its digest. Until one is named (a decoder's own tests) pictures are kept in memory, at most
//!   [`MEMORY_MAX`] bytes.
//! - **How much.** At most [`STORE_MAX`] bytes on disk; past that the pictures used longest ago go
//!   first. A thread read again (Codex's `thread/resume`, ACP's `session/load`, pi's session file)
//!   keeps its pictures again, so what was let go comes back when it is shown.
//! - **Not the decoders' IO.** The decoders stay sans-IO in everything but this: a picture's bytes
//!   are written once, when first described, and a Codex `imageView` reads the file it names
//!   ([`keep_file`]), since Codex holds no bytes of it.

use std::collections::VecDeque;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use data_encoding::BASE64;
use parking_lot::{Mutex, RwLock};
use slopty_proto::thread::{ContentRef, Image};

/// How a kept picture is named: `media:` and its digest.
pub const PREFIX: &str = "media:";

/// The most bytes kept on disk.
pub const STORE_MAX: u64 = 512 * 1024 * 1024;

/// The most bytes kept in memory while no folder is named.
pub const MEMORY_MAX: usize = 64 * 1024 * 1024;

/// The largest picture kept: a larger one is not shown inline.
pub const PICTURE_MAX: usize = 20 * 1024 * 1024;

static DIR: RwLock<Option<PathBuf>> = RwLock::new(None);
static MEMORY: Mutex<VecDeque<(String, Vec<u8>)>> = Mutex::new(VecDeque::new());
/// Serialises writes and the trimming after them.
static WRITING: Mutex<()> = Mutex::new(());

/// Keep pictures in `dir` from now on, made when it is not there.
///
/// # Errors
///
/// When the folder cannot be made.
pub fn set_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    *DIR.write() = Some(dir.to_owned());
    Ok(())
}

fn dir() -> Option<PathBuf> {
    DIR.read().clone()
}

/// The picture `bytes` described, its bytes kept.
///
/// `None` when they are no picture (PNG, JPEG, GIF, WebP, as their first bytes say, or an SVG)
/// or are larger than [`PICTURE_MAX`]. A `media_type` the agent gave is taken when it names an
/// image.
#[must_use]
pub fn keep(bytes: &[u8], media_type: Option<&str>) -> Option<Image> {
    if bytes.is_empty() || bytes.len() > PICTURE_MAX {
        return None;
    }
    let sniffed = sniff(bytes);
    let media_type = sniffed.or_else(|| media_type.filter(|t| t.starts_with("image/")))?;
    let digest = blake3::hash(bytes).to_hex().to_string();
    store(&digest, bytes);
    let (width, height) = crate::conversation::media::dimensions(bytes).unwrap_or((0, 0));
    Some(Image {
        digest: digest.clone(),
        media_type: media_type.to_owned(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        width,
        height,
        at: ContentRef(format!("{PREFIX}{digest}")),
    })
}

/// The picture base64 `data` holds, kept ([`keep`]); `data` may be a `data:` URL, whose media
/// type is then taken.
#[must_use]
pub fn keep_base64(data: &str, media_type: Option<&str>) -> Option<Image> {
    let (said, encoded) = match data.strip_prefix("data:") {
        Some(url) => {
            let (head, body) = url.split_once(',')?;
            let said = head.strip_suffix(";base64")?;
            (Some(said), body)
        }
        None => (None, data),
    };
    let bytes = decode(encoded)?;
    keep(&bytes, said.or(media_type))
}

/// The picture at `path` on this machine, kept ([`keep`]); `None` when it cannot be read or is
/// no picture.
#[must_use]
pub fn keep_file(path: &str) -> Option<Image> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(PICTURE_MAX).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit).read_to_end(&mut bytes).ok()?;
    keep(&bytes, None)
}

/// The bytes of the picture `content` names, when it names one kept here and it is still kept.
#[must_use]
pub fn read(content: &ContentRef) -> Option<Vec<u8>> {
    let digest = content.0.strip_prefix(PREFIX)?;
    if !is_digest(digest) {
        return None;
    }
    if let Some(dir) = dir() {
        let path = dir.join(digest);
        if let Ok(bytes) = fs::read(&path) {
            // Read again: it is the last to go.
            let _touched = fs::File::options()
                .append(true)
                .open(&path)
                .and_then(|f| f.set_modified(std::time::SystemTime::now()));
            return Some(bytes);
        }
    }
    let memory = MEMORY.lock();
    memory.iter().find(|(d, _)| d == digest).map(|(_, bytes)| bytes.clone())
}

/// Base64 as agents write it: standard, padded or not, with no line breaks.
#[must_use]
pub fn decode(data: &str) -> Option<Vec<u8>> {
    let data = data.trim();
    BASE64.decode(data.as_bytes()).ok().or_else(|| {
        let short = match data.len().checked_rem(4) {
            Some(2) => 2,
            Some(3) => 1,
            _ => 0,
        };
        let padded = format!("{data}{}", "=".repeat(short));
        BASE64.decode(padded.as_bytes()).ok()
    })
}

fn is_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
}

fn store(digest: &str, bytes: &[u8]) {
    let Some(dir) = dir() else {
        let mut memory = MEMORY.lock();
        if memory.iter().any(|(d, _)| d == digest) {
            return;
        }
        memory.push_back((digest.to_owned(), bytes.to_vec()));
        let mut total: usize = memory.iter().map(|(_, b)| b.len()).sum();
        while total > MEMORY_MAX {
            let Some((_, gone)) = memory.pop_front() else { break };
            total = total.saturating_sub(gone.len());
        }
        drop(memory);
        return;
    };
    let _writing = WRITING.lock();
    let path = dir.join(digest);
    if path.is_file() {
        return;
    }
    // Written aside and renamed, so a reader never sees half a picture.
    let aside = dir.join(format!(".{digest}.part"));
    // A picture that cannot be kept is only not shown when asked for.
    if fs::write(&aside, bytes).and_then(|()| fs::rename(&aside, &path)).is_err() {
        let _gone = fs::remove_file(&aside);
        return;
    }
    trim(&dir, STORE_MAX);
}

/// Let the pictures used longest ago go until the folder holds at most `max` bytes.
fn trim(dir: &Path, max: u64) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut kept: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_str().is_some_and(is_digest))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            Some((meta.modified().ok()?, meta.len(), e.path()))
        })
        .collect();
    let mut total: u64 = kept.iter().map(|(_, len, _)| len).sum();
    if total <= max {
        return;
    }
    kept.sort_by_key(|(at, ..)| *at);
    for (_, len, path) in kept {
        if total <= max {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// The media type a picture's first bytes say.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if is_svg(bytes) {
        Some("image/svg+xml")
    } else {
        None
    }
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(512)).unwrap_or_default();
    let Ok(text) = std::str::from_utf8(head)
        .or_else(|e| std::str::from_utf8(head.get(..e.valid_up_to()).unwrap_or_default()))
    else {
        return false;
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 × 3 PNG's header.
    fn png(width: u32, height: u32, salt: u8) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        bytes.extend(width.to_be_bytes());
        bytes.extend(height.to_be_bytes());
        bytes.extend([8, 6, 0, 0, salt]);
        bytes
    }

    /// A picture is described by the digest of its bytes and comes back whole from its ref;
    /// what is no picture is not kept, nor is a ref that names no digest read.
    #[test]
    fn a_picture_is_kept_by_its_digest_and_read_back() {
        let bytes = png(2, 3, 1);
        let image = keep(&bytes, None).unwrap();
        assert_eq!(image.digest, blake3::hash(&bytes).to_hex().to_string());
        assert_eq!((image.width, image.height), (2, 3));
        assert_eq!(image.bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(image.media_type, "image/png");
        assert_eq!(read(&image.at), Some(bytes.clone()));
        assert_eq!(keep(b"plain words", Some("text/plain")), None);
        assert_eq!(keep(b"", None), None);
        assert_eq!(read(&ContentRef("media:../../etc/passwd".to_owned())), None);
        assert_eq!(read(&ContentRef(format!("{PREFIX}{}", "0".repeat(64)))), None);
        let encoded = BASE64.encode(&bytes);
        let url = format!("data:image/png;base64,{encoded}");
        assert_eq!(keep_base64(&url, None).map(|i| i.digest), Some(image.digest.clone()));
        let unpadded = encoded.trim_end_matches('=');
        assert_eq!(keep_base64(unpadded, None).map(|i| i.digest), Some(image.digest));
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#;
        assert_eq!(keep(svg, None).map(|i| i.media_type).as_deref(), Some("image/svg+xml"));
    }

    /// On disk, the store holds at most its cap: the picture used longest ago goes first.
    #[test]
    fn the_store_lets_the_oldest_go_past_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("a".repeat(64));
        let new = dir.path().join("b".repeat(64));
        fs::write(&old, [0_u8; 600]).unwrap();
        let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
        fs::File::options().append(true).open(&old).unwrap().set_modified(an_hour_ago).unwrap();
        fs::write(&new, [0_u8; 600]).unwrap();
        trim(dir.path(), 1_000);
        assert!(!old.exists(), "the oldest went");
        assert!(new.exists(), "the newest stayed");
    }
}
