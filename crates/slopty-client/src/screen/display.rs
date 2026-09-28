//! A display made on a worker for this device ([`ScreenRequest::OpenDisplay`]): the device's
//! key, kept beside its layout, and the requests that open and resize one.
//!
//! The UI offers it on a worker whose caps say `virtual_displays`, sized to the tile's drawable
//! at the backing scale and refresh of the screen the tile is on.

use std::path::Path;

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

#[cfg(test)]
mod tests {
    use super::{KEY_FILE, key};

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
