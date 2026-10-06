//! A display made on a worker for this device ([`ScreenRequest::OpenDisplay`]): the device's
//! key, kept beside its layout, and the requests that open and resize one.
//!
//! The UI offers it on a worker whose caps say `virtual_displays`, sized to the tile's drawable
//! at the backing scale and refresh of the screen the tile is on, and the display follows the
//! tile ([`Follow`]) once it has held still for [`SETTLE`].

use std::path::Path;
use std::time::{Duration, Instant};

use slopty_core::StreamId;
use slopty_proto::ClientMsg;
use slopty_proto::screen::{DisplayKey, DisplayShape, Quality, ScreenRequest};

/// The file in the client's data directory, beside `layout.json`, that holds this device's key.
pub const KEY_FILE: &str = "display-key";

/// This device's display key, from `data_dir`, drawn and written there when it holds none.
///
/// The same key makes a worker give this device the same display identity every time, so macOS
/// puts it back where it was.
///
/// # Errors
///
/// When a new key cannot be written.
pub fn key(data_dir: &Path) -> std::io::Result<DisplayKey> {
    let path = data_dir.join(KEY_FILE);
    if let Some(key) = std::fs::read_to_string(&path).ok().as_deref().and_then(parse) {
        return Ok(key);
    }
    let key = DisplayKey(uuid::Uuid::new_v4().into_bytes());
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(&path, format!("{}\n", uuid::Uuid::from_bytes(key.0).simple()))?;
    Ok(key)
}

fn parse(text: &str) -> Option<DisplayKey> {
    uuid::Uuid::try_parse(text.trim()).ok().map(|uuid| DisplayKey(uuid.into_bytes()))
}

/// The shape of a display for a tile `pixels` in size on a screen of `scale` and `refresh_hz`
/// (0 when not known).
#[must_use]
pub const fn shape(pixels: (u32, u32), scale: f32, refresh_hz: u16) -> DisplayShape {
    DisplayShape { width: pixels.0, height: pixels.1, scale, refresh_hz }
}

/// Open a stream of a display made for this device, at `shape`. The worker answers with
/// `ScreenEvent::Display` (the display made, or the physical one streamed instead and why)
/// and then `Opened`.
#[must_use]
pub const fn open(key: DisplayKey, shape: DisplayShape, quality: Quality) -> ClientMsg {
    ClientMsg::Screen(ScreenRequest::OpenDisplay { key, shape, quality })
}

/// The tile of `stream` is now `pixels` in size, on a screen of `scale` (`None`: the screen did
/// not change).
#[must_use]
pub const fn resize(stream: StreamId, pixels: (u32, u32), scale: Option<f32>) -> ClientMsg {
    ClientMsg::Screen(ScreenRequest::Resize { stream, width: pixels.0, height: pixels.1, scale })
}

/// How long a tile holds one size before its display is asked to take it.
///
/// A sash drag passes through a size a frame, and each mode change reconfigures the worker's
/// displays, which takes it a few hundred milliseconds and blanks the picture.
pub const SETTLE: Duration = Duration::from_millis(300);

/// A made display following its tile: the size and scale last asked for, and the one the tile
/// has now, sent once it has held for [`SETTLE`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Follow {
    asked: ((u32, u32), f32),
    wanted: Option<((u32, u32), f32, Instant)>,
}

impl Follow {
    /// Following a display opened at `shape`.
    #[must_use]
    pub const fn new(shape: DisplayShape) -> Self {
        Self { asked: ((shape.width, shape.height), shape.scale), wanted: None }
    }

    /// The tile is `pixels` in size on a screen of `scale`, as drawn at `now`. The wait starts
    /// again only when the size changes: a frame that draws it the same leaves it running.
    pub fn tile(&mut self, pixels: (u32, u32), scale: f32, now: Instant) {
        if (pixels, scale) == self.asked {
            self.wanted = None;
            return;
        }
        if self.wanted.is_some_and(|(p, s, _)| (p, s) == (pixels, scale)) {
            return;
        }
        self.wanted = Some((pixels, scale, now));
    }

    /// When the waiting size is due, if one waits.
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
        self.wanted.and_then(|(_, _, since)| since.checked_add(SETTLE))
    }

    /// The resize for `stream`, once the tile has held its new size for [`SETTLE`] at `now`;
    /// the scale goes only when it changed.
    pub fn take(&mut self, stream: StreamId, now: Instant) -> Option<ClientMsg> {
        let (pixels, scale, since) = self.wanted?;
        if now.saturating_duration_since(since) < SETTLE {
            return None;
        }
        self.wanted = None;
        let moved = (scale - self.asked.1).abs() > f32::EPSILON;
        self.asked = (pixels, scale);
        Some(resize(stream, pixels, moved.then_some(scale)))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_core::StreamId;
    use slopty_proto::ClientMsg;
    use slopty_proto::screen::ScreenRequest;

    use super::{Follow, KEY_FILE, SETTLE, key, shape};

    fn asked(msg: Option<ClientMsg>) -> Option<(u32, u32, Option<f32>)> {
        match msg? {
            ClientMsg::Screen(ScreenRequest::Resize {
                stream: StreamId(7),
                width,
                height,
                scale,
            }) => Some((width, height, scale)),
            other => panic!("not a resize of the stream: {other:?}"),
        }
    }

    /// A tile dragged through many sizes asks for the last one only, once it has held still
    /// for the settle time; a size back where the display is asks for nothing; a new screen's
    /// scale goes with the size, and an unchanged one does not.
    #[test]
    fn the_display_follows_the_tile_once_it_holds_still() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let stream = StreamId(7);
        let mut follow = Follow::new(shape((1600, 1000), 2.0, 120));
        follow.tile((1600, 1000), 2.0, at(0));
        assert_eq!(follow.due(), None, "the size it has");

        for (n, width) in (1610..1700).step_by(10).enumerate() {
            follow.tile((width, 1000), 2.0, at(16 * n as u64));
            assert_eq!(asked(follow.take(stream, at(16 * n as u64))), None, "still moving");
        }
        let last = at(16 * 8);
        assert_eq!(follow.due(), Some(last + SETTLE));
        follow.tile((1690, 1000), 2.0, at(200));
        assert_eq!(follow.due(), Some(last + SETTLE), "a frame drawn the same keeps the wait");
        assert_eq!(asked(follow.take(stream, last + SETTLE / 2)), None);
        assert_eq!(asked(follow.take(stream, last + SETTLE)), Some((1690, 1000, None)));
        assert_eq!(asked(follow.take(stream, at(2_000))), None, "asked once");

        follow.tile((1800, 1000), 2.0, at(2_000));
        follow.tile((1690, 1000), 2.0, at(2_010));
        assert_eq!(follow.due(), None, "back where it is");

        follow.tile((845, 500), 1.0, at(3_000));
        assert_eq!(asked(follow.take(stream, at(3_000) + SETTLE)), Some((845, 500, Some(1.0))));
    }

    /// The key is drawn once per data directory and read back after; another directory (another
    /// device) draws its own, and a file that holds no key is replaced.
    #[test]
    fn the_key_is_drawn_once_and_kept_beside_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let first = key(dir.path()).unwrap();
        assert_eq!(key(dir.path()).unwrap(), first, "kept");
        let other = tempfile::tempdir().unwrap();
        assert_ne!(key(other.path()).unwrap(), first, "another device, another key");
        std::fs::write(dir.path().join(KEY_FILE), "not a key").unwrap();
        let redrawn = key(dir.path()).unwrap();
        assert_ne!(redrawn, first);
        assert_eq!(key(dir.path()).unwrap(), redrawn);
    }

    /// A key directory that cannot be written is an error, not a key that changes every launch.
    #[test]
    fn an_unwritable_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a file");
        std::fs::write(&file, "").unwrap();
        key(&file).unwrap_err();
    }
}
