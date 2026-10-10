//! What each end of a link says before its first message: that it is Slopty, and which wire it
//! speaks.
//!
//! A [`Prefix`] opens the control stream both ways, ahead of any postcard: [`MAGIC`], the wire
//! [`FINGERPRINT`] and the [`BUILD`] a person reads. Its layout never changes, so two builds
//! whose messages no longer decode alike still read each other's prefix. The two ends compare
//! fingerprints before anything else. On a mismatch the one that sees it closes the connection
//! with a code of its own, and a person is told which side to update. No message ever fails
//! to decode over it.
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

/// One end's word on its wire.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prefix {
    /// Its [`FINGERPRINT`].
    pub fingerprint: u64,
    /// Its [`BUILD`], for a person.
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
        Self { fingerprint: FINGERPRINT, build: BUILD.to_owned() }
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

    #[test]
    fn a_long_build_is_cut_at_a_character() {
        let build = "é".repeat(200);
        let (read, _) =
            Prefix::decode(&Prefix { fingerprint: 1, build }.encode()).unwrap().unwrap();
        assert_eq!(read.build, "é".repeat(BUILD_MAX / 2));
    }
}
