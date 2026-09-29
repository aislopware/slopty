//! Replacing the matches a search showed, file by file.
//!
//! A replace names each match by its line and its place on the line, as the search reported
//! it, with the stamp the file had then. A file whose stamp moved since is left alone: its
//! lines may have shifted, and a match found again by number could be another one. A file
//! still as it was is read whole, its matches found again with the search's matcher, the named
//! ones replaced (with the groups of a regular expression expanded), and the result written beside
//! it and renamed over it, with its permissions. A file tile showing it sees the new stamp on
//! its next look and reads it again.
//!
//! No link is followed below the search's folder, at any step: each directory on the way is
//! opened from the one before with `O_NOFOLLOW`, the file too, and the write goes into the
//! directory held open, so a link swapped in while a replace runs cannot send the write out of
//! the folder or onto another file.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::Metadata;
use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use grep_matcher::{Captures as _, Matcher as _};
use grep_regex::RegexMatcher;
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;
use slopty_proto::search::{
    FileReplace, FileReplaced, FileStamp, MatchAt, Replace, SkipReason, Skipped,
};

use super::{MAX_SPANS, content, stamp_of};
use crate::file::os_word;

/// The largest file a replace rewrites, in bytes: it is read and written whole.
pub const REPLACE_BYTES: u64 = 64 << 20;

/// Replace `request`'s matches in the files under `root`: the files rewritten, and the ones
/// left alone with why. Each file stands alone; one refused does not stop the others.
///
/// # Errors
///
/// For a person: the query is empty or does not parse.
pub fn replace(
    root: &Path,
    request: &Replace,
) -> Result<(Vec<FileReplaced>, Vec<Skipped>), String> {
    if request.query.pattern.is_empty() {
        return Err("Nothing to replace: the query is empty".to_owned());
    }
    let matcher = super::matcher(&request.query)?;
    let with = Replacement { text: request.with.as_bytes(), expand: request.query.regex };
    let mut replaced = Vec::with_capacity(request.files.len());
    let mut skipped = Vec::new();
    for file in &request.files {
        match replace_file(root, file, &matcher, with) {
            Ok(done) => replaced.push(done),
            Err(why) => {
                tracing::info!(path = file.path, ?why, "replace skipped a file");
                skipped.push(Skipped { path: file.path.clone(), why });
            }
        }
    }
    Ok((replaced, skipped))
}

/// What a match becomes: the text, with `$1` and `${name}` expanded when it is a regular
/// expression's.
#[derive(Clone, Copy, Debug)]
struct Replacement<'a> {
    text: &'a [u8],
    expand: bool,
}

fn replace_file(
    root: &Path,
    file: &FileReplace,
    matcher: &RegexMatcher,
    with: Replacement<'_>,
) -> Result<FileReplaced, SkipReason> {
    let failed = |e: &io::Error| match e.kind() {
        io::ErrorKind::NotFound => SkipReason::Changed,
        _ => SkipReason::Failed(os_word(e)),
    };
    let (dir, name) = parent_beneath(root, &file.path)?;
    let opened = open_beneath(&dir, name, OFlags::RDONLY | OFlags::NONBLOCK)?;
    let mut opened = std::fs::File::from(opened);
    let meta = opened.metadata().map_err(|e| failed(&e))?;
    #[expect(clippy::filetype_is_file, reason = "a FIFO or a device is not rewritten")]
    let regular = meta.file_type().is_file();
    if !regular {
        return Err(SkipReason::Failed("Not a regular file".to_owned()));
    }
    if stamp_of(&meta) != file.stamp {
        return Err(SkipReason::Changed);
    }
    if meta.len() > REPLACE_BYTES {
        let (size, cap) = (meta.len() >> 20, REPLACE_BYTES >> 20);
        return Err(SkipReason::Failed(format!(
            "{size} MB is past the {cap} MB a replace rewrites"
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    let _read = (&mut opened)
        .take(REPLACE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| failed(&e))?;
    // A write between the look and the read moves the stamp: what was read is not what was
    // searched.
    let now = opened.metadata().map_err(|e| failed(&e))?;
    if stamp_of(&now) != file.stamp || bytes.len() as u64 != meta.len() {
        return Err(SkipReason::Changed);
    }
    if file.matches.is_empty() {
        return Ok(FileReplaced { path: file.path.clone(), matches: 0, stamp: file.stamp });
    }
    let (text, count) = rewrite(&bytes, &file.matches, matcher, with).ok_or(SkipReason::Changed)?;
    let stamp = write_beneath(&dir, name, &text, &meta).map_err(|e| failed(&e))?;
    Ok(FileReplaced { path: file.path.clone(), matches: count, stamp })
}

/// The directory `relative` is in under `root`, open, and the file's name in it: each directory
/// on the way opened from the last without following a link, so a link swapped in anywhere
/// below the root, before or during the replace, is refused rather than followed out of it.
/// The root itself may be a link: it is the folder the person searched.
fn parent_beneath<'a>(root: &Path, relative: &'a str) -> Result<(OwnedFd, &'a OsStr), SkipReason> {
    let outside = || SkipReason::Failed("Not a path in the search's folder".to_owned());
    let path = Path::new(relative);
    let mut names = Vec::new();
    for part in path.components() {
        let Component::Normal(name) = part else { return Err(outside()) };
        names.push(name);
    }
    let name = names.pop().ok_or_else(outside)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut dir = rustix::fs::open(root, flags, Mode::empty()).map_err(gone)?;
    for part in names {
        dir = open_beneath(&dir, part, OFlags::RDONLY | OFlags::DIRECTORY)?;
    }
    Ok((dir, name))
}

/// `name` in `dir`, opened with `flags` and never through a link.
fn open_beneath(dir: &OwnedFd, name: &OsStr, flags: OFlags) -> Result<OwnedFd, SkipReason> {
    let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty()).map_err(gone)
}

/// Why a file could not be reached: gone since the search, or reached only through a link.
fn gone(e: Errno) -> SkipReason {
    match e {
        Errno::NOENT => SkipReason::Changed,
        Errno::LOOP | Errno::NOTDIR => SkipReason::Failed("Reached through a link".to_owned()),
        _ => SkipReason::Failed(os_word(&e.into())),
    }
}

/// Put `bytes` in place of `name` in `dir`, whose contents were `was`: written beside it, given
/// its permissions, and renamed over it, so a reader sees the old file or the new one whole.
/// The rename replaces whatever is at `name` and follows nothing. The new file's stamp.
fn write_beneath(
    dir: &OwnedFd,
    name: &OsStr,
    bytes: &[u8],
    was: &Metadata,
) -> io::Result<FileStamp> {
    let temp = temp_name(name);
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mode = Mode::from_raw_mode(0o600);
    let mut out = std::fs::File::from(rustix::fs::openat(dir, &temp, flags, mode)?);
    let written = (|| {
        out.write_all(bytes)?;
        out.set_permissions(was.permissions())?;
        slopty_platform::fs::order_before_later_writes(&out)?;
        // The file a replace read is still the one at `name`: a replace of another file put
        // there since would lose that file's contents.
        let there = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
        #[cfg_attr(
            target_os = "linux",
            expect(clippy::useless_conversion, reason = "`st_dev` is u64 on Linux, i32 on macOS")
        )]
        let same_dev = u64::try_from(there.st_dev).is_ok_and(|dev| dev == was.dev());
        if !same_dev || there.st_ino != was.ino() {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        rustix::fs::renameat(dir, &temp, dir, name)?;
        out.metadata()
    })();
    match written {
        Ok(meta) => {
            rustix::fs::fsync(dir)?;
            Ok(stamp_of(&meta))
        }
        Err(e) => {
            let _removed = rustix::fs::unlinkat(dir, &temp, AtFlags::empty());
            Err(e)
        }
    }
}

/// A name beside `name` that no other replace, in this process or another, is writing.
fn temp_name(name: &OsStr) -> OsString {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut temp = OsString::from(".");
    temp.push(name);
    temp.push(format!(".{}-{nanos}-{n}.slopty-tmp", std::process::id()));
    temp
}

/// `bytes` with the matches at `wanted` replaced, and how many were: `None` when one of them
/// is not there, which in a file unchanged since it was searched it always is.
///
/// A line's matches are counted as a search reports them (`super::spans_in`): those that are
/// not empty, in order, up to the cap on a line.
fn rewrite(
    bytes: &[u8],
    wanted: &[MatchAt],
    matcher: &RegexMatcher,
    with: Replacement<'_>,
) -> Option<(Vec<u8>, u32)> {
    let mut lines: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for at in wanted {
        lines.entry(at.line).or_default().insert(at.index);
    }
    let mut caps = matcher.new_captures().ok()?;
    let mut out = Vec::with_capacity(bytes.len());
    let mut count = 0_u32;
    let mut number = 0_u32;
    for raw in bytes.split_inclusive(|b| *b == b'\n') {
        number = number.saturating_add(1);
        let Some(indices) = lines.remove(&number) else {
            out.extend_from_slice(raw);
            continue;
        };
        let line = content(raw);
        let (mut at, mut index, mut here) = (0_usize, 0_u32, 0_usize);
        matcher
            .captures_iter(line, &mut caps, |caps| {
                let Some(m) = caps.get(0).filter(|m| m.start() < m.end()) else { return true };
                if usize::try_from(index).unwrap_or(usize::MAX) >= MAX_SPANS {
                    return false;
                }
                if indices.contains(&index) {
                    out.extend_from_slice(line.get(at..m.start()).unwrap_or_default());
                    if with.expand {
                        caps.interpolate(
                            |name| matcher.capture_index(name),
                            line,
                            with.text,
                            &mut out,
                        );
                    } else {
                        out.extend_from_slice(with.text);
                    }
                    at = m.end();
                    here = here.saturating_add(1);
                }
                index = index.saturating_add(1);
                true
            })
            .ok()?;
        if here != indices.len() {
            return None;
        }
        out.extend_from_slice(line.get(at..).unwrap_or_default());
        out.extend_from_slice(raw.get(line.len()..).unwrap_or_default());
        count = count.saturating_add(u32::try_from(here).unwrap_or(u32::MAX));
    }
    lines.is_empty().then_some((out, count))
}
