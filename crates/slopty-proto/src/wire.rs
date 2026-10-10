//! What each end of a link says before its first message: that it is Slopty, and which wire it
//! speaks.
//!
//! A [`Prefix`] opens the control stream both ways, ahead of any postcard: [`MAGIC`], the wire
//! [`FINGERPRINT`] and the build a person reads ([`this_build`]). Its layout never changes, so two
//! builds whose messages no longer decode alike still read each other's prefix. The two ends
//! compare fingerprints before anything else. On a mismatch the one that sees it closes the
//! connection with a code of its own, and a person is told which side to update. No message ever
//! fails to decode over it.
//!
//! Nobody bumps the fingerprint. The build script hashes the goldens that pin the wire
//! (`tests/snapshots`, all but the control socket's), so it moves exactly when a golden does
//! (`docs/decisions/transport.md`, "Each end says its wire first").

use bytes::{BufMut as _, Bytes, BytesMut};

include!(concat!(env!("OUT_DIR"), "/wire.rs"));

#[cfg(test)]
mod fingerprint;

/// The first bytes of every control stream, both ways.
pub const MAGIC: [u8; 6] = *b"SLOPTY";

/// Bytes of a prefix before its build text: the magic, the fingerprint (`u64` little-endian)
/// and the text's length (one byte).
pub const HEAD_BYTES: usize = MAGIC.len() + size_of::<u64>() + 1;

/// Longest build text a prefix carries; a longer one is cut at a character boundary.
pub const BUILD_MAX: usize = u8::MAX as usize;

/// The commit this binary was made from, when the build was told it.
///
/// It is `<hash>.<YYYYMMDDTHHMMZ>` (its commit time, UTC): `cargo xtask bundle` and `dist` set
/// `SLOPTY_COMMIT`. A plain `cargo build` has none, and is told apart from another build of its
/// version only by its wire.
///
/// It is read here and not by a build script, so a commit rebuilds nothing: only a build given
/// another commit compiles this crate again.
pub const COMMIT: Option<&str> = option_env!("SLOPTY_COMMIT");

/// This build in full, as a daemon tells its clients.
///
/// [`BUILD`], then `.commit.` and [`COMMIT`] when it has one
/// (`0.4.0+wire.0badf00d.20261009T2307Z.commit.1a2b3c4d5e6f.20261010T0930Z`), as the wire
/// prefix, [`crate::server::WorkerCaps::build`] and [`crate::server::FromServer::Welcome`]
/// carry it.
#[must_use]
pub fn this_build() -> String {
    COMMIT.map_or_else(|| BUILD.to_owned(), |commit| format!("{BUILD}.commit.{commit}"))
}

/// Which of two builds is the newer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Newer {
    /// This build: the other end is the one to update.
    Here,
    /// The other end's: this machine is the one to update, and putting this build there would
    /// take it back.
    There,
}

/// Which of `here` and `there`, two builds as [`this_build`] spells them, is the newer.
///
/// By version first; on one version, by the commit each was made from when both say one, and
/// else by when each one's wire last changed (the stamp [`BUILD`] ends with). A build that says
/// nothing (`there` empty: older than the wire prefix) is older than any. `None` when the two
/// cannot be told apart: the same commit, a version that does not parse (a pre-release), or two
/// on one version with no commit whose wires changed in the same minute or say no date.
#[must_use]
pub fn newer(here: &str, there: &str) -> Option<Newer> {
    use std::cmp::Ordering;
    if there.is_empty() {
        return Some(Newer::Here);
    }
    let (here, there) = (Parts::of(here), Parts::of(there));
    let same_length = |a: &str, b: &str| (a.len() == b.len()).then(|| a.cmp(b));
    let order = match here.version?.cmp(&there.version?) {
        Ordering::Equal => match (here.commit, there.commit) {
            (Some((here_hash, _)), Some((there_hash, _))) if here_hash == there_hash => {
                Ordering::Equal
            }
            (Some((_, here_made)), Some((_, there_made))) => same_length(here_made, there_made)?,
            _ => same_length(here.wire_changed?, there.wire_changed?)?,
        },
        order => order,
    };
    match order {
        Ordering::Greater => Some(Newer::Here),
        Ordering::Less => Some(Newer::There),
        Ordering::Equal => None,
    }
}

/// A build's text read back ([`this_build`]).
struct Parts<'a> {
    /// `major.minor.patch` as numbers; `None` for anything else, a pre-release among them.
    version: Option<[u64; 3]>,
    /// When its wire last changed, when it says.
    wire_changed: Option<&'a str>,
    /// The commit it was made from and that commit's time, when it says.
    commit: Option<(&'a str, &'a str)>,
}

impl<'a> Parts<'a> {
    fn of(build: &'a str) -> Self {
        let (version, metadata) = build.split_once('+').unwrap_or((build, ""));
        let (wire, commit) = metadata.split_once(".commit.").unwrap_or((metadata, ""));
        let wire_changed = wire.strip_prefix("wire.").and_then(|wire| wire.split('.').nth(1));
        let commit =
            commit.split_once('.').filter(|(hash, made)| !hash.is_empty() && !made.is_empty());
        let mut parts = version.split('.').map(|part| part.parse::<u64>().ok());
        let version = (|| {
            let numbers = [parts.next()??, parts.next()??, parts.next()??];
            parts.next().is_none().then_some(numbers)
        })();
        Self { version, wire_changed: wire_changed.filter(|s| !s.is_empty()), commit }
    }
}

/// One end's word on its wire.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prefix {
    /// Its [`FINGERPRINT`].
    pub fingerprint: u64,
    /// Its build ([`this_build`]), for a person and for telling which side is the newer.
    pub build: String,
}

/// A stream that does not open with a prefix.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("the peer did not open with Slopty's wire prefix: an older build, or not Slopty")]
pub struct NotSlopty;

impl Prefix {
    /// This build's.
    #[must_use]
    pub fn this() -> Self {
        Self { fingerprint: FINGERPRINT, build: this_build() }
    }

    /// Whether it speaks this build's wire.
    #[must_use]
    pub const fn is_this_wire(&self) -> bool {
        self.fingerprint == FINGERPRINT
    }

    /// The bytes on the stream.
    #[must_use]
    pub fn encode(&self) -> Bytes {
        let end = self.build.floor_char_boundary(BUILD_MAX);
        let build = self.build.get(..end).unwrap_or_default().as_bytes();
        let mut out = BytesMut::with_capacity(HEAD_BYTES.saturating_add(build.len()));
        out.put_slice(&MAGIC);
        out.put_u64_le(self.fingerprint);
        out.put_u8(u8::try_from(build.len()).unwrap_or(u8::MAX));
        out.put_slice(build);
        out.freeze()
    }

    /// The prefix at the start of `buf` and how many bytes it took; `None` until all of it
    /// is there.
    ///
    /// # Errors
    ///
    /// [`NotSlopty`] as soon as the bytes there differ from [`MAGIC`].
    pub fn decode(buf: &[u8]) -> Result<Option<(Self, usize)>, NotSlopty> {
        let magic = buf.get(..MAGIC.len()).unwrap_or(buf);
        if !MAGIC.starts_with(magic) {
            return Err(NotSlopty);
        }
        let Some(head) = buf.get(..HEAD_BYTES) else { return Ok(None) };
        let Some((fingerprint, len)) = head
            .get(MAGIC.len()..)
            .and_then(|rest| rest.split_first_chunk::<8>())
            .and_then(|(fingerprint, len)| Some((u64::from_le_bytes(*fingerprint), *len.first()?)))
        else {
            return Ok(None);
        };
        let end = HEAD_BYTES.saturating_add(usize::from(len));
        let Some(build) = buf.get(HEAD_BYTES..end) else { return Ok(None) };
        let build = String::from_utf8_lossy(build).into_owned();
        Ok(Some((Self { fingerprint, build }, end)))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn snapshots() -> Vec<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
        fingerprint::read(&dir).unwrap()
    }

    /// Deriving it again from the same tree gives what was baked in, every time.
    #[test]
    fn the_fingerprint_is_the_goldens_and_stable() {
        let first = fingerprint::of(snapshots());
        let mut reversed = snapshots();
        reversed.reverse();
        assert_eq!(first, FINGERPRINT, "the baked fingerprint is this tree's");
        assert_eq!(fingerprint::of(reversed), first, "the order files are read in does not count");
        assert!(BUILD.starts_with(concat!(env!("CARGO_PKG_VERSION"), "+wire.")), "{BUILD}");
        let hex = format!("{FINGERPRINT:016x}");
        let shown = BUILD.split_once("+wire.").map(|(_, wire)| wire.split('.').next());
        assert_eq!(shown.flatten(), hex.get(..8), "{BUILD}");
    }

    /// A changed, added or renamed wire golden moves it; the insta header and the control
    /// socket's goldens do not.
    #[test]
    fn the_fingerprint_moves_exactly_with_a_wire_golden() {
        let base = snapshots();
        let was = fingerprint::of(base.clone());
        let hello = base.iter().position(|(n, _)| n == "golden__golden__client_hello.snap");
        let hello = hello.expect("the hello golden");

        let mut changed = base.clone();
        changed[hello].1.push_str(" 00");
        assert_ne!(fingerprint::of(changed), was, "a changed body is a wire change");

        let mut renamed = base.clone();
        renamed[hello].0 = "golden__golden__client_hi.snap".to_owned();
        assert_ne!(fingerprint::of(renamed), was, "a renamed golden is a wire change");

        let mut added = base.clone();
        added.push(("golden__golden__client_new.snap".to_owned(), "---\n---\n00".to_owned()));
        assert_ne!(fingerprint::of(added), was, "a new golden is a wire change");

        let mut moved = base.clone();
        moved[hello].1 = moved[hello].1.replace("expression: ", "expression:  ");
        assert_ne!(moved[hello].1, base[hello].1);
        assert_eq!(fingerprint::of(moved), was, "the insta header is not the wire");

        let mut local = base;
        local.push(("golden__ctl__new.snap".to_owned(), "---\n---\n{}".to_owned()));
        local.push(("golden__golden__pending.snap.new".to_owned(), "---\n---\n00".to_owned()));
        assert_eq!(fingerprint::of(local), was, "the control socket and pending goldens are not");
    }

    #[test]
    fn a_prefix_reads_back_whole_and_waits_for_the_rest() {
        let prefix = Prefix { fingerprint: 0x0123_4567_89ab_cdef, build: "0.1.0+wire.x".into() };
        let mut bytes = prefix.encode().to_vec();
        bytes.extend_from_slice(b"postcard after it");
        let whole = HEAD_BYTES + prefix.build.len();
        assert_eq!(Prefix::decode(&bytes), Ok(Some((prefix, whole))));
        for cut in 0..whole {
            assert_eq!(Prefix::decode(&bytes[..cut]), Ok(None), "{cut} bytes are not all of it");
        }
        assert!(Prefix::this().is_this_wire());
        assert!(!Prefix { fingerprint: FINGERPRINT ^ 1, ..Prefix::this() }.is_this_wire());
    }

    /// A pre-prefix peer's first bytes are a frame length: refused at the first byte that
    /// differs, without waiting for a whole head.
    #[test]
    fn anything_else_is_not_slopty_at_once() {
        assert_eq!(Prefix::decode(&[0x15, 0, 0, 0]), Err(NotSlopty));
        assert_eq!(Prefix::decode(b"SLX"), Err(NotSlopty));
        assert_eq!(Prefix::decode(b"SLO"), Ok(None));
    }

    /// The newer build is told by version first, then by its commit, then by when its wire
    /// last changed; a build older than the prefix is older than any, and two that cannot be
    /// told apart are neither.
    #[test]
    fn the_newer_build_is_told_by_version_then_commit_then_wire_date() {
        let order = [
            ("0.2.0+wire.0badf00d.20261009T2307Z", "0.1.9+wire.feedface.20261101T0000Z"),
            ("0.1.0+wire.0badf00d.20261011T0812Z", "0.1.0+wire.feedface.20261009T2307Z"),
            ("0.1.0+wire.0badf00d.20261011T0812Z", ""),
            ("0.10.0+wire.0badf00d", "0.9.0+wire.feedface"),
            // One wire, two commits: the later commit is the newer, whatever the wire says.
            (
                "0.1.0+wire.0badf00d.20261009T2307Z.commit.bbbbbbbbbbbb.20261012T1000Z",
                "0.1.0+wire.0badf00d.20261009T2307Z.commit.aaaaaaaaaaaa.20261011T0900Z",
            ),
            (
                "0.1.0+wire.0badf00d.commit.bbbbbbbbbbbb.20261012T1000Z",
                "0.1.0+wire.0badf00d.commit.aaaaaaaaaaaa.20261011T0900Z",
            ),
            // One side without a commit falls back to the wire's date.
            (
                "0.1.0+wire.0badf00d.20261011T0812Z.commit.aaaaaaaaaaaa.20261011T0900Z",
                "0.1.0+wire.feedface.20261009T2307Z",
            ),
        ];
        for (new, old) in order {
            assert_eq!(newer(new, old), Some(Newer::Here), "{new} over {old}");
            if !old.is_empty() {
                assert_eq!(newer(old, new), Some(Newer::There), "{old} under {new}");
            }
        }
        let same = "0.1.0+wire.0badf00d.20261009T2307Z.commit.aaaaaaaaaaaa.20261011T0900Z";
        let unknown = [
            ("0.1.0+wire.0badf00d", "0.1.0+wire.feedface"),
            ("0.1.0+wire.0badf00d.20261011T0812Z", "0.1.0+wire.feedface"),
            ("0.1.0+wire.0badf00d.20261011T0812Z", "0.1.0+wire.feedface.20261011T0812Z"),
            ("0.1.0+wire.0badf00d", "not a build"),
            ("0.1.0-rc.1+wire.0badf00d", "0.1.0+wire.feedface"),
            (same, same),
            (
                "0.1.0+wire.0badf00d.commit.aaaaaaaaaaaa.20261011T0900Z",
                "0.1.0+wire.0badf00d.commit.bbbbbbbbbbbb.20261011T0900Z",
            ),
        ];
        for (a, b) in unknown {
            assert_eq!(newer(a, b), None, "{a} beside {b}");
        }
    }

    #[test]
    fn this_build_is_the_wire_build_and_its_commit() {
        let build = this_build();
        assert!(build.starts_with(BUILD), "{build}");
        match COMMIT {
            Some(commit) => assert_eq!(build, format!("{BUILD}.commit.{commit}")),
            None => assert_eq!(build, BUILD),
        }
        assert_eq!(newer(&build, &build), None, "a build is not newer than itself");
    }

    #[test]
    fn a_long_build_is_cut_at_a_character() {
        let build = "é".repeat(200);
        let (read, _) =
            Prefix::decode(&Prefix { fingerprint: 1, build }.encode()).unwrap().unwrap();
        assert_eq!(read.build, "é".repeat(BUILD_MAX / 2));
    }
}
