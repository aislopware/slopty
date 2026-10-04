//! What a composer attaches: a picture pasted into it or files dropped on it, each a chip from
//! the paste until the message goes ([`Attachments`]).
//!
//! An attachment goes the way an agent takes one from a terminal: by its path in the message.
//! The file goes up to a fresh directory of the worker's drop directory (`Dest::Attachment`),
//! never the thread's working tree, where a screenshot would show in `git status` and could be
//! committed. The draft never holds a worker's temporary path: sending puts the landed paths
//! after the text ([`with_paths`]), the same bytes a terminal drop at the end of the draft
//! would have put there.

/// The name a pasted picture lands under, in a directory of its own on the worker.
#[must_use]
pub fn picture_name(extension: &str) -> String {
    format!("pasted-image.{extension}")
}

/// The message `text` with the attachments that landed at `paths`: the text, then each path
/// as a terminal drop types it (quoted where a shell would need it), a space apart.
#[must_use]
pub fn with_paths(text: &str, paths: &[String]) -> String {
    let text = text.trim_end();
    let typed = slopty_client::xfer::paste_paths(paths);
    let typed = typed.trim_end();
    match (text.trim().is_empty(), typed.is_empty()) {
        (_, true) => text.to_owned(),
        (true, false) => typed.to_owned(),
        (false, false) => format!("{text} {typed}"),
    }
}

/// What an attachment is before it goes up.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Attach {
    /// A picture pasted into the composer: its bytes and the name it lands under.
    Picture {
        /// The name, [`picture_name`].
        name: String,
        /// The encoded picture.
        bytes: Vec<u8>,
    },
    /// Files copied here, pasted into the composer.
    Files(Vec<std::path::PathBuf>),
}

impl Attach {
    /// What a paste into a composer attaches rather than pastes as text: files copied here, or
    /// a picture with no text beside it on the clipboard.
    #[must_use]
    pub fn of_paste(item: &gpui::ClipboardItem) -> Option<Self> {
        let mut files = Vec::new();
        let mut picture = None;
        for entry in item.entries() {
            match entry {
                gpui::ClipboardEntry::ExternalPaths(paths) => {
                    files.extend_from_slice(paths.paths());
                }
                gpui::ClipboardEntry::Image(image) => {
                    picture = picture.or_else(|| {
                        picture_extension(image.format).map(|ext| (ext, image.bytes.clone()))
                    });
                }
                gpui::ClipboardEntry::String(_) => {}
            }
        }
        if !files.is_empty() {
            return Some(Self::Files(files));
        }
        let texted = item.text().is_some_and(|t| !t.is_empty());
        picture
            .filter(|_| !texted)
            .map(|(ext, bytes)| Self::Picture { name: picture_name(ext), bytes })
    }

    /// What its chip calls it: the picture's name, the one file's, or how many.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Picture { name, .. } => name.clone(),
            Self::Files(paths) => match paths.as_slice() {
                [one] => one.file_name().map_or_else(
                    || one.to_string_lossy().into_owned(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                many => format!("{} files", many.len()),
            },
        }
    }

    /// The picture its chip draws, for a pasted picture in a format gpui decodes.
    #[must_use]
    pub fn picture(&self) -> Option<std::sync::Arc<gpui::Image>> {
        let Self::Picture { name, bytes } = self else { return None };
        let format = picture_format(name)?;
        Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes.clone())))
    }
}

/// The format of a pasted picture named `name` ([`picture_name`]).
fn picture_format(name: &str) -> Option<gpui::ImageFormat> {
    match name.rsplit_once('.')?.1 {
        "png" => Some(gpui::ImageFormat::Png),
        "jpg" => Some(gpui::ImageFormat::Jpeg),
        "gif" => Some(gpui::ImageFormat::Gif),
        "webp" => Some(gpui::ImageFormat::Webp),
        _ => None,
    }
}

/// The extension of a picture Claude Code reads, for a pasted picture in `format`; `None` for
/// one it does not.
const fn picture_extension(format: gpui::ImageFormat) -> Option<&'static str> {
    match format {
        gpui::ImageFormat::Png => Some("png"),
        gpui::ImageFormat::Jpeg => Some("jpg"),
        gpui::ImageFormat::Gif => Some("gif"),
        gpui::ImageFormat::Webp => Some("webp"),
        _ => None,
    }
}

/// The thread view an upload's chip is in, which hears how it goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target(pub gpui::WeakEntity<super::thread::ThreadView>);

impl Target {
    /// Show the chip of `what`, which the workspace sends up itself; which chip it is, unless
    /// the composer is gone.
    pub fn start(&self, what: &Attach, cx: &mut gpui::App) -> Option<u64> {
        self.0.update(cx, |v, _cx| v.start_attachment(what)).ok()
    }

    /// Put `text` at the end of the draft and give the composer the keyboard, unless the
    /// composer is gone.
    pub fn quote(&self, text: &str, window: &mut gpui::Window, cx: &mut gpui::App) {
        let _gone = self.0.update(cx, |v, cx| v.quote_into_draft(text, window, cx));
    }

    /// Chip `id` is `fraction` of the way up.
    pub fn progress(&self, id: u64, fraction: f32, cx: &mut gpui::App) {
        let _gone = self.0.update(cx, |v, cx| v.attachment_progress(id, fraction, cx));
    }

    /// Chip `id` landed at `paths` on the worker.
    pub fn landed(&self, id: u64, paths: &[String], cx: &mut gpui::App) {
        let _gone = self.0.update(cx, |v, cx| v.attachment_landed(id, paths, cx));
    }

    /// Chip `id`'s upload is over, however it ended.
    pub fn ended(&self, id: u64, cx: &mut gpui::App) {
        let _gone = self.0.update(cx, |v, cx| v.attachment_ended(id, cx));
    }
}

/// One attachment of the draft: its chip in the composer.
#[derive(Clone, PartialEq, Debug)]
pub struct Attachment {
    /// Names it to the workspace, which reports its progress and its landing.
    pub id: u64,
    /// The file's name.
    pub name: String,
    /// How far along, 0 to 1.
    pub fraction: f32,
    /// Where it landed on the worker; empty while it uploads.
    pub paths: Vec<String>,
}

impl Attachment {
    /// It is on the worker, ready to go with the message.
    #[must_use]
    pub const fn landed(&self) -> bool {
        !self.paths.is_empty()
    }

    /// What the chip says after the name while it uploads: `↑ 42%`.
    #[must_use]
    pub fn progress(&self) -> String {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 100")]
        let percent = (self.fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
        format!("\u{2191} {percent}%")
    }
}

/// The draft's attachments, in the order they were attached.
#[derive(Debug, Default)]
pub struct Attachments {
    next: u64,
    chips: Vec<Attachment>,
}

impl Attachments {
    /// A file called `name` starts to upload: its chip shows, and its id is returned.
    pub fn add(&mut self, name: String) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.chips.push(Attachment { id: self.next, name, fraction: 0.0, paths: Vec::new() });
        self.next
    }

    /// Attachment `id` is `fraction` of the way up. Whether its chip changed.
    pub fn progress(&mut self, id: u64, fraction: f32) -> bool {
        let Some(chip) = self.chips.iter_mut().find(|c| c.id == id) else { return false };
        let changed = chip.progress() != Attachment { fraction, ..chip.clone() }.progress();
        chip.fraction = fraction;
        changed
    }

    /// Attachment `id` landed at `paths`: its chip stays, to go with the message. One that
    /// landed nowhere goes. Whether its chip changed.
    pub fn land(&mut self, id: u64, paths: &[String]) -> bool {
        if paths.is_empty() {
            return self.end(id);
        }
        let Some(chip) = self.chips.iter_mut().find(|c| c.id == id) else { return false };
        chip.fraction = 1.0;
        chip.paths = paths.to_vec();
        true
    }

    /// Attachment `id` is off the draft: taken off, or it will not land. Whether it was here.
    pub fn end(&mut self, id: u64) -> bool {
        let before = self.chips.len();
        self.chips.retain(|c| c.id != id);
        self.chips.len() != before
    }

    /// Attachment `id`'s upload is over: a chip that did not land goes, one that landed stays.
    /// Whether its chip went.
    pub fn over(&mut self, id: u64) -> bool {
        let before = self.chips.len();
        self.chips.retain(|c| c.id != id || c.landed());
        self.chips.len() != before
    }

    /// Something is still on its way up.
    #[must_use]
    pub fn uploading(&self) -> bool {
        self.chips.iter().any(|c| !c.landed())
    }

    /// Where the landed attachments are, in the order they were attached.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        self.chips.iter().flat_map(|c| c.paths.iter().cloned()).collect()
    }

    /// The message went: every chip goes with it.
    pub fn clear(&mut self) {
        self.chips.clear();
    }

    /// The chips on show.
    #[must_use]
    pub fn chips(&self) -> &[Attachment] {
        &self.chips
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An attachment is a chip from the paste until the message goes: it says how far it got
    /// while it uploads and stays once it landed; the message then ends with its path as a
    /// terminal drop types it.
    #[test]
    fn an_attachment_stays_a_chip_and_its_path_goes_with_the_message() {
        let mut attached = Attachments::default();
        let shot = attached.add(picture_name("png"));
        let other = attached.add("notes.txt".to_owned());
        assert_eq!(attached.chips()[0].name, "pasted-image.png");
        assert_eq!(attached.chips()[0].progress(), "\u{2191} 0%");
        assert!(attached.progress(shot, 0.42));
        assert!(!attached.progress(shot, 0.421), "the chip says the same");
        assert_eq!(attached.chips()[0].progress(), "\u{2191} 42%");

        let path = "/Users/me/.slopty/drop/x/pasted-image.png".to_owned();
        assert!(attached.land(shot, std::slice::from_ref(&path)));
        assert!(attached.chips()[0].landed(), "the chip stays once it landed");
        assert!(attached.uploading(), "notes.txt is still on its way");
        assert!(!attached.over(shot), "an upload over after landing keeps its chip");
        assert!(attached.land(other, &[]) && !attached.end(other), "landing nowhere ends it");
        assert!(!attached.uploading());
        assert_eq!(attached.paths(), std::slice::from_ref(&path));
        assert_eq!(with_paths("look at", &attached.paths()), format!("look at {path}"));
        assert_eq!(with_paths("look at", &[]), "look at");
        assert_eq!(with_paths("", &attached.paths()), path, "a picture alone is the message");
        assert_eq!(
            with_paths("this", &["/tmp/Screen Shot.png".to_owned()]),
            "this '/tmp/Screen Shot.png'",
            "quoted as a drop on a shell is"
        );
        attached.clear();
        assert_eq!(attached.chips(), []);
    }
}
