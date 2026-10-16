// slopty: Slopty's own file (not in zeron).
//! slopty: a draft that holds attachments, kept on this client (its words alone go to the chat's
//! machine, `SaveDraft`), so a draft that survives a restart keeps its images and files and the
//! chips that name them, as Claude desktop keeps its drafts. Each chat's is a folder under
//! `{data_dir}/drafts/`: each attachment's bytes in a file named by its id, written once, and
//! `index.json` with the draft's words and each attachment's name and chip. A draft with none has
//! no folder.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::attachments::{self, StagedAttachment};

/// The folder every chat's kept drafts go under, in the client's data folder.
const DIR: &str = "drafts";
/// What a chat's folder lists.
const INDEX: &str = "index.json";

/// A kept draft: its words and its attachments.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    text: String,
    attachments: Vec<Kept>,
}

/// One kept attachment: its file in the folder, its name, the chip that mentions it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    file: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mention: Option<u32>,
}

/// A draft as it came back: its words and its attachments, staged again.
pub(super) struct Restored {
    pub text: String,
    pub staged: Vec<StagedAttachment>,
}

/// The folder chat `chat`'s draft is kept in: its id as hex, so any id is one plain name.
fn folder(data_dir: &Path, chat: &str) -> PathBuf {
    let name: String = chat.bytes().map(|b| format!("{b:02x}")).collect();
    data_dir.join(DIR).join(name)
}

/// The file an attachment's bytes go in: its id, then its name's extension, which says what an
/// image is when it is read back.
fn file_of(att: &StagedAttachment) -> String {
    let id: String = att.id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    match Path::new(&att.name).extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.chars().all(|c| c.is_ascii_alphanumeric()) => format!("{id}.{ext}"),
        _ => id,
    }
}

/// What a draft is, for telling whether it changed since it was kept: its words and each
/// attachment's id and chip; none with no attachments, whose words its machine keeps alone.
pub(super) fn fingerprint(text: &str, staged: &[StagedAttachment]) -> Vec<String> {
    if staged.is_empty() {
        return Vec::new();
    }
    std::iter::once(text.to_owned())
        .chain(staged.iter().map(|att| format!("{}:{:?}", att.id, att.mention)))
        .collect()
}

/// Keep chat `chat`'s draft, `text` with `staged`, in place of what was kept: an attachment's
/// bytes are written once, and one no longer held is let go. A draft with no attachments
/// removes the folder.
pub(super) fn save(data_dir: &Path, chat: &str, text: &str, staged: &[StagedAttachment]) -> io::Result<()> {
    let dir = folder(data_dir, chat);
    if staged.is_empty() {
        return match std::fs::remove_dir_all(&dir) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(&dir)?;
    let mut index = Index { text: text.to_owned(), attachments: Vec::with_capacity(staged.len()) };
    for att in staged {
        let file = file_of(att);
        let path = dir.join(&file);
        if !path.exists() {
            std::fs::write(&path, att.bytes())?;
        }
        index.attachments.push(Kept { file, name: att.name.clone(), mention: att.mention });
    }
    let held: HashSet<&str> = index.attachments.iter().map(|kept| kept.file.as_str()).collect();
    for entry in std::fs::read_dir(&dir)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name != INDEX && !held.contains(name.as_ref()) {
            std::fs::remove_file(dir.join(name.as_ref()))?;
        }
    }
    let json = serde_json::to_vec(&index).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    // The index goes in whole or not at all.
    let partial = dir.join(format!("{INDEX}.partial"));
    std::fs::write(&partial, json)?;
    std::fs::rename(partial, dir.join(INDEX))
}

/// Chat `chat`'s kept draft, its attachments staged again; none when nothing was kept. An
/// attachment that no longer reads is left out, and the rest come back.
pub(super) fn load(data_dir: &Path, chat: &str) -> io::Result<Option<Restored>> {
    let dir = folder(data_dir, chat);
    let index = match std::fs::read(dir.join(INDEX)) {
        Ok(index) => index,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let index: Index =
        serde_json::from_slice(&index).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let staged = index
        .attachments
        .into_iter()
        .filter_map(|kept| {
            let mut att = attachments::stage_file(&dir.join(&kept.file))
                .inspect_err(|why| tracing::warn!(%why, "a kept draft attachment did not read"))
                .ok()?;
            att.name = kept.name;
            att.mention = kept.mention;
            Some(att)
        })
        .collect();
    Ok(Some(Restored { text: index.text, staged }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A draft comes back as kept: its words, an image an image and a file its bytes, each with
    /// its chip; kept again, a bytes file is written once and one let go is removed; none kept
    /// removes the chat's folder; a chat never kept has none.
    #[test]
    fn a_draft_comes_back_as_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(2, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let image_path = dir.path().join("shot.png");
        std::fs::write(&image_path, png.into_inner()).unwrap();
        let mut image = attachments::stage_file(&image_path).unwrap();
        image.name = "Image 1.png".into();
        image.mention = Some(1);
        let notes_path = dir.path().join("notes.txt");
        std::fs::write(&notes_path, b"line one\n").unwrap();
        let notes = attachments::stage_file(&notes_path).unwrap();

        save(dir.path(), "chat/1", "look at this", &[image.clone(), notes]).unwrap();
        let back = load(dir.path(), "chat/1").unwrap().expect("kept");
        assert_eq!(back.text, "look at this");
        assert_eq!(back.staged.len(), 2);
        assert_eq!((back.staged[0].name.as_str(), back.staged[0].mention), ("Image 1.png", Some(1)));
        assert!(back.staged[0].image().is_some(), "an image comes back an image");
        assert_eq!((back.staged[1].name.as_str(), back.staged[1].bytes()), ("notes.txt", &b"line one\n"[..]));

        save(dir.path(), "chat/1", "just the image", &[image]).unwrap();
        let files = std::fs::read_dir(folder(dir.path(), "chat/1")).unwrap().count();
        assert_eq!(files, 2, "the image's bytes and the index; the note let go");
        assert_eq!(load(dir.path(), "chat/1").unwrap().map(|d| d.text).as_deref(), Some("just the image"));

        save(dir.path(), "chat/1", "words alone", &[]).unwrap();
        assert!(!folder(dir.path(), "chat/1").exists(), "no attachments, no folder");
        assert!(load(dir.path(), "chat/2").unwrap().is_none());
    }
}
