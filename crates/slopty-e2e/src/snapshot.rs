//! Golden renders of the app's self-test: an app frame held to the PNG and the words under
//! `golden/`, by [`slopty_testkit::golden`]'s comparator, which the shell's in-process tests
//! share.
//!
//! A golden is held in words too: what the frame says (its accessibility tree's roles, labels
//! and values, [`crate::frame_text`]) sits beside the PNG as `golden/<name>.txt` and must match
//! it exactly. The pixels cannot see a word change that moves few of them; its text fails it,
//! with the lines that changed. A frame fails on its worst window
//! ([`slopty_testkit::golden::WINDOW_NOISE`]), a seam, or a share of its pixels past
//! [`MAC_SHARE`] ([`SIMULATOR_SHARE`] on the simulator). A golden that is not there fails. Pass
//! `--accept` to write missing and failing goldens, or `--accept-all` to rewrite every golden (via
//! `SLOPTY_E2E_ACCEPT=changed` and `SLOPTY_E2E_ACCEPT=all`); neither writes a golden the shell's
//! in-process tests draw ([`assert_matches_shells`]).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use image::RgbaImage;
pub use slopty_testkit::golden::{
    Accept, CHANNEL_SLACK, Diff, EDGE_SLACK, LUMA_BUCKETS, MAC_SHARE, PixelRect, SEAM_RUN,
    SIMULATOR_SHARE, WINDOW, WINDOW_NOISE, compare, compare_masked, foreground_fraction,
    luma_distance, luma_histogram, pixels_near,
};
use slopty_testkit::golden::{Goldens, holds};

/// A frame the app drew, and what it says in words.
#[derive(Clone, Debug)]
pub struct Frame {
    /// The picture.
    pub image: RgbaImage,
    /// Its accessibility tree, in reading order.
    pub a11y: Vec<crate::A11yNode>,
    /// Device pixels per point: the tree is in points, the picture in pixels.
    pub scale: f32,
}

impl Frame {
    /// What the frame says ([`crate::frame_text`]), leaving out the nodes centred inside
    /// `masks`: what a masked region says moves as its picture does. A path in the run's scratch
    /// directory starts `$SCRATCH`, a port on loopback is `$PORT`, a new note's moment is
    /// `$MOMENT` and a machine's system release is `$OS`, because each machine and each run has
    /// its own.
    #[must_use]
    pub fn text(&self, masks: &[PixelRect]) -> String {
        let kept: Vec<crate::A11yNode> = self
            .a11y
            .iter()
            .filter(|node| {
                let [x, y, w, h] = node.bounds;
                let at = |v: f32| {
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "pixels"
                    )]
                    let px = (v * self.scale).max(0.0).round() as u32;
                    px
                };
                let (cx, cy) = (at(w.mul_add(0.5, x)), at(h.mul_add(0.5, y)));
                !masks.iter().any(|m| holds(m, cx, cy))
            })
            .cloned()
            .collect();
        let text = scrub_ports(&scrub_scratch(&crate::frame_text(&kept)));
        scrub_releases(&scrub_moments(&text))
    }
}

/// `text` with the system release a machine's record names (`"os_version":"27.0.1"`, in a
/// tool's answer the inspector shows) spelled `$OS`, and the rest of its line cut to `…`: the
/// release is the machine's, and a label cut at a fixed length ends elsewhere when the release
/// is longer or shorter.
fn scrub_releases(text: &str) -> String {
    const RELEASE: &str = "\"os_version\":\"";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(RELEASE) {
        let (before, found) = rest.split_at(at.saturating_add(RELEASE.len()));
        out.push_str(before);
        out.push_str("$OS\u{2026}");
        rest = found.find('\n').map_or("", |end| found.get(end..).unwrap_or_default());
    }
    out.push_str(rest);
    out
}

/// `text` with the moment in each new note's name (`note-2026-10-05-143210`) spelled
/// `note-$MOMENT`: a note is named for when it was made.
fn scrub_moments(text: &str) -> String {
    const NOTE: &str = "note-";
    const MOMENT: &[u8] = b"dddd-dd-dd-dddddd";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(NOTE) {
        let (before, found) = rest.split_at(at.saturating_add(NOTE.len()));
        out.push_str(before);
        let moment = found.as_bytes().get(..MOMENT.len()).is_some_and(|b| {
            b.iter().zip(MOMENT).all(|(c, m)| if *m == b'd' { c.is_ascii_digit() } else { c == m })
        });
        rest = if moment {
            out.push_str("$MOMENT");
            found.get(MOMENT.len()..).unwrap_or_default()
        } else {
            found
        };
    }
    out.push_str(rest);
    out
}

/// `text` with the port after each loopback host spelled `$PORT`: the tests' own servers bind
/// whatever port the system gives them.
fn scrub_ports(text: &str) -> String {
    ["127.0.0.1:", "[::1]:", "localhost:"].iter().fold(text.to_owned(), |text, host| {
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.find(host) {
            let (before, after) = rest.split_at(at.saturating_add(host.len()));
            out.push_str(before);
            let port = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            if port > 0 {
                out.push_str("$PORT");
            }
            rest = after.get(port..).unwrap_or_default();
        }
        out.push_str(rest);
        out
    })
}

/// `text` with each run's scratch directory spelled `$SCRATCH`: the system temporary
/// directory and the stacks' roots' parent ([`crate::harness::roots_parent`]), as the app saw
/// each raw or resolved, and the directory the run made in it, whose name is random. Resolved
/// first, since the raw root is a part of it (`/private/var/…`, `/private/tmp`).
fn scrub_scratch(text: &str) -> String {
    let resolved = |raw: PathBuf| std::fs::canonicalize(&raw).unwrap_or_else(|_| raw.clone());
    let (temp, roots) = (std::env::temp_dir(), crate::harness::roots_parent());
    // `/tmp` is a link to `/private/tmp`; a shell's prompt shows the spelling it was given.
    let tmp = PathBuf::from("/tmp");
    [resolved(temp.clone()), temp, resolved(roots.clone()), roots, tmp].iter().fold(
        text.to_owned(),
        |text, root| {
            let root = root.to_string_lossy();
            let root = root.trim_end_matches('/');
            if root.is_empty() { text } else { scrub_root(&text, root) }
        },
    )
}

/// `text` with every `root/<name>` spelled `$SCRATCH`, `<name>` being the run's directory.
fn scrub_root(text: &str, root: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(root) {
        let (before, found) = rest.split_at(at);
        out.push_str(before);
        out.push_str("$SCRATCH");
        let after = found.get(root.len()..).unwrap_or_default();
        rest = after.strip_prefix('/').map_or(after, |name| {
            let end = name
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
                .unwrap_or(name.len());
            name.get(end..).unwrap_or_default()
        });
    }
    out.push_str(rest);
    out
}

impl std::ops::Deref for Frame {
    type Target = RgbaImage;

    fn deref(&self) -> &RgbaImage {
        &self.image
    }
}

/// Where the goldens live.
#[must_use]
pub fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("golden")
}

/// The self-test's goldens: [`golden_dir`], the run's artifacts, `accept` and the share of a
/// frame that may differ.
fn goldens(artifacts: &Path, accept: Accept, share: f64) -> Goldens<'_> {
    // The directory lives as long as the process: a `PathBuf` made once.
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    Goldens { dir: DIR.get_or_init(golden_dir), artifacts, accept, share }
}

/// Compare `actual` with `golden/<name>.png`, and its text with `golden/<name>.txt`.
///
/// When accepting, `--accept` writes every golden the frame differs from by a pixel or a word,
/// while `--accept-all` rewrites every golden. Otherwise the frame is written to
/// `<artifacts>/<name>.actual.png` and, when it differs, the diff to `<artifacts>/<name>.diff.png`,
/// and a failure names both files. Its text is written to `<artifacts>/<name>.actual.txt`, and a
/// line changed fails with the lines around it.
///
/// # Errors
///
/// When the sizes differ, a window or a run differs past noise ([`Diff::passes`]), a word
/// differs, or a file cannot be written.
pub fn assert_matches(name: &str, actual: &Frame, artifacts: &Path) -> Result<Diff> {
    assert_matches_masked(name, actual, artifacts, &[])
}

/// [`assert_matches`] outside `masks` ([`compare_masked`]): the chrome around a streamed
/// picture, held to a golden while the picture moves.
///
/// # Errors
///
/// As [`assert_matches`].
pub fn assert_matches_masked(
    name: &str,
    actual: &Frame,
    artifacts: &Path,
    masks: &[PixelRect],
) -> Result<Diff> {
    assert_matches_apart(name, actual, artifacts, (masks, masks))
}

/// [`assert_matches`] with the pixels outside `pixels` and the words of the nodes outside
/// `words`.
///
/// A region masked in pixels alone says words that hold still, or that the text already scrubs
/// (a test server's port in an address), while its glyphs or its edge move from run to run.
///
/// # Errors
///
/// As [`assert_matches`].
pub fn assert_matches_apart(
    name: &str,
    actual: &Frame,
    artifacts: &Path,
    masks: (&[PixelRect], &[PixelRect]),
) -> Result<Diff> {
    held(goldens(artifacts, Accept::from_env(), MAC_SHARE), name, actual, masks)
}

/// [`assert_matches_apart`] for a simulator's frame, held to [`SIMULATOR_SHARE`].
///
/// # Errors
///
/// As [`assert_matches`].
pub fn assert_matches_simulator(
    name: &str,
    actual: &Frame,
    artifacts: &Path,
    masks: (&[PixelRect], &[PixelRect]),
) -> Result<Diff> {
    held(goldens(artifacts, Accept::from_env(), SIMULATOR_SHARE), name, actual, masks)
}

/// [`assert_matches`] for a golden the shell's in-process tests draw and write.
///
/// Compared whatever the accept and never written (`slopty_shell::shell::goldens`), so the app's
/// frame on a real stack that parts from the one drawn in the test process fails here, loudly.
///
/// # Errors
///
/// As [`assert_matches`].
pub fn assert_matches_shells(name: &str, actual: &Frame, artifacts: &Path) -> Result<Diff> {
    let accept = Accept::from_env().without_writing();
    held(goldens(artifacts, accept, MAC_SHARE), name, actual, (&[], &[]))
}

/// `actual` held to `goldens`' picture and words of `name`, outside the masks.
fn held(
    goldens: Goldens<'_>,
    name: &str,
    actual: &Frame,
    (pixels, words): (&[PixelRect], &[PixelRect]),
) -> Result<Diff> {
    let pixels = goldens.pixels(name, &actual.image, pixels);
    let words = goldens.text(name, &actual.text(words));
    match (pixels, words) {
        (Ok(diff), Ok(())) => Ok(diff),
        (Err(e), Ok(())) | (Ok(_), Err(e)) => Err(e),
        (Err(pixels), Err(words)) => bail!("{pixels:#}\n{words:#}"),
    }
}

#[cfg(test)]
mod tests {
    use image::Rgba;
    use slopty_testkit::golden::changed_lines;

    use super::*;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([rgb[0], rgb[1], rgb[2], 255]))
    }

    fn node(role: &str, label: &str, bounds: [f32; 4]) -> crate::A11yNode {
        crate::A11yNode {
            role: role.to_owned(),
            label: Some(label.to_owned()),
            value: None,
            focused: false,
            bounds,
        }
    }

    /// A frame's words are a line a node; one changed word fails with that line, the golden's
    /// and the frame's, where every pixel but a word's few would have passed.
    #[test]
    fn a_changed_word_is_a_changed_line() {
        let golden = crate::frame_text(&[
            node("Tab", "Workspace 1", [0.0, 0.0, 80.0, 20.0]),
            node("Button", "New", [90.0, 0.0, 20.0, 20.0]),
        ]);
        assert_eq!(golden, "Tab \u{2502} Workspace 1\nButton \u{2502} New\n");
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Tab", "e2e-worker", [0.0, 0.0, 80.0, 20.0]),
                node("Button", "New", [90.0, 0.0, 20.0, 20.0]),
            ],
            scale: 2.0,
        };
        let diff = changed_lines(Some(&golden), &frame.text(&[]));
        assert!(diff.contains("-Tab \u{2502} Workspace 1"), "{diff}");
        assert!(diff.contains("+Tab \u{2502} e2e-worker"), "{diff}");
        assert!(!diff.contains("-Button"), "the line that stayed is context: {diff}");
    }

    /// What a masked region says moves with its picture: its nodes leave the text.
    #[test]
    fn a_masked_node_says_nothing() {
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Status", "Frames late", [10.0, 10.0, 20.0, 10.0]),
                node("Button", "Close tile", [100.0, 10.0, 20.0, 10.0]),
            ],
            scale: 2.0,
        };
        assert_eq!(frame.text(&[[0, 0, 80, 60]]), "Button \u{2502} Close tile\n");
    }

    /// A path in a run's scratch directory, under this machine's temporary root raw or
    /// resolved, reads the same on every machine and in every run; the words round it stay.
    #[test]
    fn a_scratch_path_reads_the_same_everywhere() {
        let raw = std::env::temp_dir().join("slopty-e2e-47c78742/project/main.rs");
        let resolved = std::fs::canonicalize(std::env::temp_dir())
            .unwrap_or_else(|_| std::env::temp_dir())
            .join("slopty-e2e-projects-8103d87d");
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Label", &format!("File {}", raw.display()), [0.0, 0.0, 1.0, 1.0]),
                node("Label", &format!("In {}. Done", resolved.display()), [0.0, 0.0, 1.0, 1.0]),
                node("Label", "At /private/tmp/slopty-e2e-ios-3ac00c2c/a", [0.0, 0.0, 1.0, 1.0]),
                node("Label", "At /tmp/slopty-e2e-folder-1f2e3d4c", [0.0, 0.0, 1.0, 1.0]),
            ],
            scale: 1.0,
        };
        assert_eq!(
            frame.text(&[]),
            "Label \u{2502} File $SCRATCH/project/main.rs\nLabel \u{2502} In $SCRATCH. Done\n\
             Label \u{2502} At $SCRATCH/a\nLabel \u{2502} At $SCRATCH\n"
        );
    }

    /// A new note's name, which is the moment it was made, reads the same in every run; any
    /// other name with "note-" in it stays.
    #[test]
    fn a_new_notes_moment_reads_the_same_in_every_run() {
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Document", "File ~/note-2026-10-05-034645.md", [0.0, 0.0, 1.0, 1.0]),
                node("Label", "note-taking, note-2026-10 and note-", [0.0, 0.0, 1.0, 1.0]),
            ],
            scale: 1.0,
        };
        assert_eq!(
            frame.text(&[]),
            "Document \u{2502} File ~/note-$MOMENT.md\n\
             Label \u{2502} note-taking, note-2026-10 and note-\n"
        );
    }

    /// A test server's port, which the system picks, reads the same in every run; a port
    /// anywhere else stays.
    #[test]
    fn a_loopback_port_reads_the_same_in_every_run() {
        let frame = Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node("Button", "http://127.0.0.1:58423/", [0.0, 0.0, 1.0, 1.0]),
                node("Label", "localhost:3000 and [::1]:61 and studio:22", [0.0, 0.0, 1.0, 1.0]),
            ],
            scale: 1.0,
        };
        assert_eq!(
            frame.text(&[]),
            "Button \u{2502} http://127.0.0.1:$PORT/\n\
             Label \u{2502} localhost:$PORT and [::1]:$PORT and studio:22\n"
        );
    }

    /// A machine's release reads the same on every machine, however long it is, and the
    /// label it ends is cut after it; the next line stays.
    #[test]
    fn a_machine_s_release_reads_the_same_on_every_machine() {
        let record = |release: &str| Frame {
            image: solid(1, 1, [0, 0, 0]),
            a11y: vec![
                node(
                    "Code",
                    &format!(r#"[{{"os":"macos","os_version":"{release}","arch":"aarch64"}}]"#),
                    [0.0, 0.0, 1.0, 1.0],
                ),
                node("Button", "os_version stays in prose", [0.0, 0.0, 1.0, 1.0]),
            ],
            scale: 1.0,
        };
        let said = record("27.0.1").text(&[]);
        assert_eq!(
            said,
            "Code \u{2502} [{\"os\":\"macos\",\"os_version\":\"$OS\u{2026}\n\
             Button \u{2502} os_version stays in prose\n"
        );
        assert_eq!(record("27.0").text(&[]), said);
    }
}
