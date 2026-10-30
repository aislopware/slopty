//! Claude Code's prompt box, read live for the inspector through Slopty's mod
//! (`docs/decisions/claude-code.md`, "The inspector mirrors the TUI's prompt box through a mod
//! of Slopty's").
//!
//! - **The mod** (`assets/claude-mod`): a plugin whose hooks module reads each change of the box's
//!   words and, once typing pauses, hands them to `slopty hook` as a [`crate::hooks::DRAFT_EVENT`]
//!   payload. It writes, draws and answers nothing. The worker writes it under its data folder with
//!   its own CLI as the relay ([`install`]), and each Claude Code it starts loads it
//!   ([`with_mod`]). A Claude Code typed by hand has none, and shows no draft.
//! - **The pictures.** One pasted into the box stands in its words as `[Image #N]`, and Claude Code
//!   writes its bytes at the paste into a store of its own ([`images_dir`]). [`Pictures`] describes
//!   each from there once, while its file stays as it was, and the worker serves its bytes from
//!   there too ([`image_ref`], [`read_picture`]).
//! - **Checked with** [`CHECKED_WITH`]: `cargo xtask fixtures claude-mod` records what the mod
//!   relays with that build (`tests/fixtures/claude-mod`), and the tests hold the embedded module
//!   and the decoding to the recording.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use slopty_core::WallMs;
use slopty_proto::thread::{ContentRef, Draft, DraftImage, Image};

use super::sessions;
use crate::managed::ManagedSettings;

/// The largest picture in the TUI's store a draft carries: a larger one is not shown.
pub const PICTURE_MAX: usize = 20 * 1024 * 1024;

/// The Claude Code the mod's relays were recorded with.
pub const CHECKED_WITH: &str = "2.1.314";

/// The flag a plugin's folder is loaded with, for one run.
pub const PLUGIN_DIR_FLAG: &str = "--plugin-dir";

/// The variable that moves Claude Code's temporary root, where it keeps pasted pictures, from
/// `/tmp`.
pub const TMPDIR_ENV: &str = "CLAUDE_CODE_TMPDIR";

/// Claude Code's temporary root when [`TMPDIR_ENV`] names none.
const TMP_ROOT: &str = "/tmp";

/// The folder under the worker's data folder the mod is written to, one folder per content.
const MOD_DIR: &str = "claude-mod";

/// The module's place in the mod.
const MODULE: &str = "hooks/register.ts";

/// The module as the source spells it, naming a stand-in relay ([`RELAY_LINE`]).
pub const SOURCE: &str = include_str!("../../assets/claude-mod/hooks/register.ts");

/// The mod's other files, by their place in it.
const FILES: [(&str, &str); 2] = [
    (
        ".claude-plugin/plugin.json",
        include_str!("../../assets/claude-mod/.claude-plugin/plugin.json"),
    ),
    ("hooks/hooks.json", include_str!("../../assets/claude-mod/hooks/hooks.json")),
];

/// The line of [`SOURCE`] that names the relay, which the worker writes with its own CLI.
pub const RELAY_LINE: &str = "const RELAY = ['/slopty', 'hook']";

/// How a pasted picture stands in the box's words: `[Image #N]`.
const IMAGE_OPEN: &str = "[Image #";

/// What a draft picture's [`ContentRef`] begins with, before its file's name.
const IMAGE_PREFIX: &str = "draft-image:";

/// The module with `relay` as the program it hands each draft to (`<relay> hook`).
#[must_use]
pub fn module(relay: &str) -> String {
    let named = serde_json::json!([relay, "hook"]);
    SOURCE.replacen(RELAY_LINE, &format!("const RELAY = {named}"), 1)
}

/// Write the mod for `relay` under `data_dir`: the folder to load it from.
///
/// The folder is named by the mod's content, so a Claude Code running keeps the files it loaded.
/// Each file is written only when it differs, to a sibling renamed into place.
///
/// # Errors
///
/// When the folder cannot be written.
pub fn install(data_dir: &Path, relay: &str) -> io::Result<PathBuf> {
    let module = module(relay);
    let files: Vec<(&str, &str)> =
        FILES.iter().copied().chain([(MODULE, module.as_str())]).collect();
    let mut hasher = blake3::Hasher::new();
    for (name, text) in &files {
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        hasher.update(text.as_bytes());
        hasher.update(&[0]);
    }
    let digest: String = hasher.finalize().to_hex().as_str().chars().take(16).collect();
    let dir = data_dir.join(MOD_DIR).join(digest);
    for (name, text) in files {
        let file = dir.join(name);
        if std::fs::read_to_string(&file).is_ok_and(|have| have == text) {
            continue;
        }
        let parent = file.parent().unwrap_or(&dir);
        std::fs::create_dir_all(parent)?;
        let base = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let staging = parent.join(format!(".{base}-{}", std::process::id()));
        std::fs::write(&staging, text)?;
        std::fs::rename(&staging, &file)?;
    }
    Ok(dir)
}

/// `claude` arguments that load the mod at `dir` too, as far as this machine's managed settings
/// let them ([`with_mod_under`]).
#[must_use]
pub fn with_mod(args: Vec<String>, dir: &Path) -> Vec<String> {
    with_mod_under(args, dir, &ManagedSettings::current())
}

/// [`with_mod`] under `managed`: the flag first, once, in its `=` form (the spaced form takes the
/// words after it too), and not at all where sideloading is off, since Claude Code refuses the run
/// for it.
#[must_use]
pub fn with_mod_under(args: Vec<String>, dir: &Path, managed: &ManagedSettings) -> Vec<String> {
    let flag = format!("{PLUGIN_DIR_FLAG}={}", dir.to_string_lossy());
    if !managed.sideloads() || args.contains(&flag) {
        return args;
    }
    std::iter::once(flag).chain(args).collect()
}

/// Where Claude Code `uid`'s session `session`, run in `cwd`, keeps the pictures pasted into its
/// box.
///
/// That is `<root>/claude-<uid>/<cwd's key>/<session>/images`: the root is `tmpdir` (its
/// [`TMPDIR_ENV`]) or `/tmp`, resolved as Claude Code resolves it, and the key is the one its
/// project folder has ([`sessions::escape`]).
#[must_use]
pub fn images_dir(tmpdir: Option<&str>, uid: u32, cwd: &Path, session: &str) -> PathBuf {
    let root = tmpdir.filter(|t| !t.is_empty()).unwrap_or(TMP_ROOT);
    let root = sessions::resolved(Path::new(root));
    let own = sessions::resolved(&root.join(format!("claude-{uid}")));
    own.join(sessions::escape(&sessions::resolved(cwd))).join(session).join("images")
}

/// The `N` of each `[Image #N]` in `text`, each once, in the order they first stand.
#[must_use]
pub fn placeholders(text: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some((_, after)) = rest.split_once(IMAGE_OPEN) {
        rest = after;
        let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
        let (digits, close) = after.split_at(end);
        if close.starts_with(']')
            && let Ok(n) = digits.parse::<u32>()
            && !found.contains(&n)
        {
            found.push(n);
        }
    }
    found
}

/// The number of the picture file `name` holds, when it is one as Claude Code names them
/// (`<N>.<ext>`, the extension in lowercase letters).
#[must_use]
pub fn picture_number(name: &str) -> Option<u32> {
    let (number, ext) = name.split_once('.')?;
    let named = !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && !ext.is_empty()
        && ext.bytes().all(|b| b.is_ascii_lowercase());
    if !named {
        return None;
    }
    number.parse().ok()
}

/// Where a draft's picture in file `name` of its session's store is ([`image_name`]).
#[must_use]
pub fn image_ref(name: &str) -> ContentRef {
    ContentRef(format!("{IMAGE_PREFIX}{name}"))
}

/// The file of its session's store `content` names, when it names a draft's picture.
#[must_use]
pub fn image_name(content: &ContentRef) -> Option<&str> {
    content.0.strip_prefix(IMAGE_PREFIX).filter(|name| picture_number(name).is_some())
}

/// The bytes of picture `name` in the store at `dir`: `None` when it is no picture's name, is
/// gone, or is larger than [`PICTURE_MAX`].
#[must_use]
pub fn read_picture(dir: &Path, name: &str) -> Option<Vec<u8>> {
    picture_number(name)?;
    let file = std::fs::File::open(dir.join(name)).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    let limit = u64::try_from(PICTURE_MAX).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= PICTURE_MAX).then_some(bytes)
}

/// The picture `bytes` described, at `at`: `None` when they are no picture (PNG, JPEG, GIF or
/// WebP, as their first bytes say).
fn describe(bytes: &[u8], at: ContentRef) -> Option<Image> {
    let media_type = crate::conversation::media::sniff(bytes)?;
    let (width, height) = crate::conversation::media::dimensions(bytes).unwrap_or((0, 0));
    Some(Image {
        digest: blake3::hash(bytes).to_hex().to_string(),
        media_type: media_type.to_owned(),
        bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        width,
        height,
        at,
    })
}

/// `text` cut to at most `max` bytes, on a character's boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let end = (0..=max).rev().find(|&at| text.is_char_boundary(at)).unwrap_or_default();
    text.get(..end).unwrap_or_default()
}

/// One session's box as drafts, its pictures described from its store once each while their
/// files stay as they were.
#[derive(Debug)]
pub struct Pictures {
    uid: u32,
    cwd: PathBuf,
    session: String,
    /// Where the last draft's pictures were looked for.
    dir: Option<PathBuf>,
    /// Each picture described, by its file's name.
    known: HashMap<String, Known>,
}

/// A picture described, and its file as it was then.
#[derive(Debug)]
struct Known {
    len: u64,
    modified: Option<SystemTime>,
    image: Image,
}

impl Pictures {
    /// The box of Claude Code `uid`'s session `session`, run in `cwd`.
    #[must_use]
    pub fn new(uid: u32, cwd: &Path, session: &str) -> Self {
        let (cwd, session) = (cwd.to_path_buf(), session.to_owned());
        Self { uid, cwd, session, dir: None, known: HashMap::new() }
    }

    /// Where the last draft's pictures were looked for: the store a [`image_ref`] is read from.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The box holding `text`, heard at `at`, with Claude Code's temporary root at `tmpdir`: the
    /// draft (`None` for an empty box), and whether a picture its words name was not found or
    /// not yet whole, which a later look may find.
    pub fn draft(&mut self, text: &str, tmpdir: Option<&str>, at: WallMs) -> (Option<Draft>, bool) {
        if text.trim().is_empty() {
            self.known.clear();
            return (None, false);
        }
        let text = cut(text, Draft::MAX_BYTES);
        let numbers = placeholders(text);
        let dir = images_dir(tmpdir, self.uid, &self.cwd, &self.session);
        if self.dir.as_ref() != Some(&dir) {
            self.known.clear();
        }
        let files = if numbers.is_empty() { HashMap::new() } else { listed(&dir) };
        let mut images = Vec::new();
        let mut missing = false;
        let mut shown = HashSet::new();
        for n in numbers {
            let image = files.get(&n).and_then(|name| {
                shown.insert(name.clone());
                self.picture(&dir, name)
            });
            match image {
                Some(image) => images.push(DraftImage { n, image }),
                None => missing = true,
            }
        }
        self.known.retain(|name, _| shown.contains(name));
        self.dir = Some(dir);
        (Some(Draft { text: text.to_owned(), images, at_ms: at }), missing)
    }

    /// Picture `name` in the store at `dir`, described anew only when its file changed.
    fn picture(&mut self, dir: &Path, name: &str) -> Option<Image> {
        let meta = std::fs::metadata(dir.join(name)).ok().filter(std::fs::Metadata::is_file)?;
        let (len, modified) = (meta.len(), meta.modified().ok());
        if let Some(known) = self.known.get(name)
            && known.len == len
            && known.modified == modified
        {
            return Some(known.image.clone());
        }
        let bytes = read_picture(dir, name)?;
        let image = describe(&bytes, image_ref(name))?;
        let known = Known { len, modified, image: image.clone() };
        self.known.insert(name.to_owned(), known);
        Some(image)
    }
}

/// The picture files of the store at `dir`, by number; of two with one number, the first by name.
fn listed(dir: &Path) -> HashMap<u32, String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return HashMap::new() };
    let mut names: Vec<String> =
        entries.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
    names.sort();
    let mut files = HashMap::new();
    for name in names {
        if let Some(n) = picture_number(&name) {
            files.entry(n).or_insert(name);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module names the relay the worker writes, a path quoted as TypeScript reads it, and
    /// nothing else of it changes.
    #[test]
    fn the_module_relays_to_the_worker_s_own_cli() {
        assert_eq!(SOURCE.matches(RELAY_LINE).count(), 1, "the relay line, once");
        let module = module("/Users/me/My \"Apps\"/slopty");
        assert!(module.contains(r#"const RELAY = ["/Users/me/My \"Apps\"/slopty","hook"]"#));
        assert_eq!(
            module.replace(r#"["/Users/me/My \"Apps\"/slopty","hook"]"#, "['/slopty', 'hook']"),
            SOURCE
        );
    }

    /// The mod is written once per content, whole, and a second write leaves it as it is; another
    /// relay is another folder, so a running Claude Code keeps what it loaded.
    #[test]
    fn the_mod_is_written_once_per_content() {
        let data = tempfile::tempdir().expect("a folder");
        let dir = install(data.path(), "/opt/slopty").expect("written");
        for name in [".claude-plugin/plugin.json", "hooks/hooks.json", MODULE] {
            assert!(dir.join(name).is_file(), "{name}");
        }
        assert_eq!(
            std::fs::read_to_string(dir.join(MODULE)).expect("module"),
            module("/opt/slopty")
        );
        let modified =
            std::fs::metadata(dir.join(MODULE)).and_then(|m| m.modified()).expect("time");
        assert_eq!(install(data.path(), "/opt/slopty").expect("again"), dir);
        let again = std::fs::metadata(dir.join(MODULE)).and_then(|m| m.modified()).expect("time");
        assert_eq!(modified, again, "an unchanged mod is not written again");
        let other = install(data.path(), "/usr/local/bin/slopty").expect("another");
        assert_ne!(other, dir);
        let left: Vec<_> = std::fs::read_dir(dir.join("hooks")).expect("hooks").collect();
        assert_eq!(left.len(), 2, "no staging file left: {left:?}");
    }

    /// The mod is loaded first, once, in the flag's `=` form, and never where the managed
    /// settings turn sideloading off.
    #[test]
    fn a_started_claude_code_loads_the_mod_where_sideloading_is_on() {
        let dir = Path::new("/data/claude-mod/0123");
        let args =
            vec!["--session-id".to_owned(), "s1".to_owned(), "--".to_owned(), "hi".to_owned()];
        let loaded = with_mod_under(args.clone(), dir, &ManagedSettings::default());
        assert_eq!(loaded.first().map(String::as_str), Some("--plugin-dir=/data/claude-mod/0123"));
        assert_eq!(loaded.get(1..), Some(args.as_slice()));
        assert_eq!(with_mod_under(loaded.clone(), dir, &ManagedSettings::default()), loaded);
        let off = ManagedSettings { disable_sideload_flags: true, ..ManagedSettings::default() };
        assert_eq!(with_mod_under(args.clone(), dir, &off), args);
    }

    /// The store is where the recorded Claude Code wrote its pasted picture (the study's
    /// evidence: `/private/tmp/claude-501/-private-tmp-lp-scratch/<id>/images`), under a moved
    /// root when one is set, whether or not its `claude-<uid>` folder is there yet.
    #[test]
    fn pasted_pictures_are_kept_where_claude_code_keeps_them() {
        let cwd = Path::new("/private/tmp/lp-scratch-missing");
        assert_eq!(
            images_dir(None, 501, cwd, "0feea93c"),
            sessions::resolved(Path::new("/tmp"))
                .join("claude-501/-private-tmp-lp-scratch-missing/0feea93c/images")
        );
        let root = tempfile::tempdir().expect("a root");
        let moved = root.path().to_str().expect("utf-8");
        assert_eq!(
            images_dir(Some(moved), 7, cwd, "s"),
            sessions::resolved(root.path())
                .join("claude-7/-private-tmp-lp-scratch-missing/s/images")
        );
        assert_eq!(images_dir(Some(""), 7, cwd, "s"), images_dir(None, 7, cwd, "s"));
    }

    /// Each picture's number once, in the order its placeholder first stands; anything else in
    /// brackets is no picture.
    #[test]
    fn the_box_names_its_pictures_by_number() {
        let text = "a[Image #2] b [Image #10][Pasted text #1 +5 lines][Image #2][Image #][Image #3";
        assert_eq!(placeholders(text), [2, 10]);
        assert_eq!(placeholders("[Image #4294967296] [Image #-1]"), Vec::<u32>::new());
        assert_eq!(picture_number("12.png"), Some(12));
        for name in ["12", ".png", "12.PNG", "a.png", "12.png.tmp", "../1.png", "1.p_g"] {
            assert_eq!(picture_number(name), None, "{name}");
        }
        assert_eq!(image_name(&image_ref("2.png")), Some("2.png"));
        assert_eq!(image_name(&ContentRef("draft-image:../secret.png".to_owned())), None);
        assert_eq!(image_name(&ContentRef("media:2.png".to_owned())), None);
    }

    /// A draft carries its words, cut on a character's boundary, and each picture its words name
    /// whose file is whole; one not there yet is said to be missing, and comes once it is.
    #[test]
    fn a_draft_holds_its_words_and_the_pictures_whose_files_are_there() {
        let root = tempfile::tempdir().expect("a root");
        let tmpdir = root.path().to_str().expect("utf-8");
        let cwd = Path::new("/work-missing");
        let mut pictures = Pictures::new(501, cwd, "s1");
        assert_eq!(pictures.draft(" \n", Some(tmpdir), WallMs::ZERO), (None, false));
        let (draft, missing) =
            pictures.draft("look [Image #1][Image #2]", Some(tmpdir), WallMs::ZERO);
        assert!(missing, "no file yet");
        assert_eq!(draft.as_ref().map(|d| d.images.len()), Some(0));
        let store = images_dir(Some(tmpdir), 501, cwd, "s1");
        std::fs::create_dir_all(&store).expect("the store");
        std::fs::write(store.join("1.png"), png()).expect("a picture");
        std::fs::write(store.join("2.jpg"), b"not yet whole").expect("a partial file");
        let (draft, missing) =
            pictures.draft("look [Image #1][Image #2]", Some(tmpdir), WallMs::ZERO);
        assert!(missing, "the second is not a picture yet");
        let draft = draft.expect("a draft");
        assert_eq!(draft.text, "look [Image #1][Image #2]");
        let [first] = draft.images.as_slice() else { panic!("{:?}", draft.images) };
        assert_eq!((first.n, first.image.media_type.as_str()), (1, "image/png"));
        assert_eq!((first.image.width, first.image.height), (1, 1));
        assert_eq!(first.image.at, image_ref("1.png"));
        assert_eq!(pictures.dir(), Some(store.as_path()));
        assert_eq!(read_picture(&store, "1.png"), Some(png()));
        assert_eq!(read_picture(&store, "../1.png"), None);
        assert_eq!(read_picture(&store, "3.png"), None);
        let (draft, missing) = pictures.draft("just [Image #1]", Some(tmpdir), WallMs::ZERO);
        assert!(!missing);
        assert_eq!(draft.map(|d| d.images.len()), Some(1));
        let long = "é".repeat(Draft::MAX_BYTES);
        let (draft, _) = pictures.draft(&long, Some(tmpdir), WallMs::ZERO);
        let text = draft.expect("a draft").text;
        assert_eq!(text.len(), Draft::MAX_BYTES, "cut whole");
        assert!(long.starts_with(&text));
    }

    /// The smallest PNG: one pixel.
    fn png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(b"\x08\x06\0\0\0\x1f\x15\xc4\x89");
        bytes
    }
}
