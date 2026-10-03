//! Files sent with a message (`Intent::Send`'s `attachments`), as each agent takes them.
//!
//! A client uploads a file to the worker first, with the transfer it uses for any file
//! (`transfer::Dest::Attachment`, a fresh folder under `~/.slopty/drop`, which needs no terminal
//! and is never inside the thread's working tree, so no review shows it as a change), and sends
//! the paths it landed at with the message. The worker reads each one ([`Attached`]); an adapter
//! gives a picture to its agent as a picture where the agent takes one (Codex `localImage`, pi's
//! `images`, an ACP image block), and any other file by its path, as Claude Code's own TUI takes a
//! path typed or pasted into it ([`with_paths`]).

use std::fmt;

/// The most attachments one message carries.
pub const MAX: usize = 32;

/// The largest picture sent as one, in bytes: a larger one goes by its path.
pub const PICTURE_MAX: u64 = 20 * 1024 * 1024;

/// The bytes a picture's media type is read from.
pub const HEAD: usize = 16;

/// A file sent with a message, as the worker read it.
#[derive(Clone, PartialEq, Eq)]
pub enum Attached {
    /// A picture, read whole: its media type is from its first bytes, not its name.
    Picture {
        /// Where it is on the worker.
        path: String,
        /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
        media_type: &'static str,
        /// Its bytes.
        bytes: Vec<u8>,
    },
    /// Any other file, and a picture too large to send as one: by its path.
    File {
        /// Where it is on the worker.
        path: String,
    },
}

impl fmt::Debug for Attached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Picture { path, media_type, bytes } => f
                .debug_struct("Picture")
                .field("path", path)
                .field("media_type", media_type)
                .field("bytes", &bytes.len())
                .finish(),
            Self::File { path } => f.debug_struct("File").field("path", path).finish(),
        }
    }
}

impl Attached {
    /// Where it is on the worker.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Picture { path, .. } | Self::File { path } => path,
        }
    }

    /// The file name, for a block that names it.
    #[must_use]
    pub fn name(&self) -> &str {
        let path = self.path();
        path.rsplit('/').find(|part| !part.is_empty()).unwrap_or(path)
    }

    /// Its bytes in base64, for a picture.
    #[must_use]
    pub fn base64(&self) -> Option<String> {
        match self {
            Self::Picture { bytes, .. } => Some(data_encoding::BASE64.encode(bytes)),
            Self::File { .. } => None,
        }
    }
}

/// The media type of a picture that starts with `head`, by its signature; `None` for anything
/// else.
#[must_use]
pub const fn picture_type(head: &[u8]) -> Option<&'static str> {
    match head {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// `text` with `paths` after it, each quoted as a shell word, as the person would paste them: the
/// form a terminal agent reads a file by, and what a message carries of a file its agent takes no
/// other way.
#[must_use]
pub fn with_paths<'a>(text: &str, paths: impl IntoIterator<Item = &'a str>) -> String {
    let quoted: Vec<String> = paths.into_iter().map(slopty_core::shell_quote).collect();
    let text = text.trim_end();
    match (text.trim().is_empty(), quoted.is_empty()) {
        (_, true) => text.to_owned(),
        (true, false) => quoted.join(" "),
        (false, false) => format!("{text} {}", quoted.join(" ")),
    }
}

/// `text` with the paths of the files in `attached` that go by their path after it.
#[must_use]
pub fn with_files(text: &str, attached: &[Attached]) -> String {
    with_paths(
        text,
        attached.iter().filter(|a| matches!(a, Attached::File { .. })).map(Attached::path),
    )
}

/// The pictures in `attached`.
pub fn pictures(attached: &[Attached]) -> impl Iterator<Item = &Attached> {
    attached.iter().filter(|a| matches!(a, Attached::Picture { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture is told by its first bytes, whatever its name; anything else is a file.
    #[test]
    fn pictures_are_told_by_their_signature() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert_eq!(picture_type(&png), Some("image/png"));
        assert_eq!(picture_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(picture_type(b"GIF89a.."), Some("image/gif"));
        assert_eq!(picture_type(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(picture_type(b"RIFF\x10\0\0\0WAVEfmt "), None, "a RIFF that is no picture");
        assert_eq!(picture_type(b"# notes\n"), None);
        assert_eq!(picture_type(&png[..4]), None, "too short to tell");
        assert_eq!(picture_type(&[]), None);
    }

    /// Paths follow the words, quoted as a shell takes them; with no words they stand alone.
    #[test]
    fn paths_follow_the_words_quoted() {
        assert_eq!(with_paths("look", ["/tmp/a.png"]), "look /tmp/a.png");
        assert_eq!(with_paths("look  ", ["/tmp/my shot.png"]), "look '/tmp/my shot.png'");
        assert_eq!(with_paths("  ", ["/a", "/b"]), "/a /b");
        assert_eq!(with_paths("just words", []), "just words");
    }

    /// Only what goes by its path joins the words; a picture goes as itself.
    #[test]
    fn only_files_join_the_words() {
        let attached = [
            Attached::Picture {
                path: "/d/shot.png".into(),
                media_type: "image/png",
                bytes: vec![1],
            },
            Attached::File { path: "/d/notes.md".into() },
        ];
        assert_eq!(with_files("see", &attached), "see /d/notes.md");
        assert_eq!(pictures(&attached).map(Attached::name).collect::<Vec<_>>(), ["shot.png"]);
        assert_eq!(attached[0].base64().as_deref(), Some("AQ=="));
        assert_eq!(attached[1].base64(), None);
    }
}
