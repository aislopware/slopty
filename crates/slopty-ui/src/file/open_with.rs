//! "Open in `<editor>`": a file, folder or review tile hands its path to the person's own editor
//! (`[client] editor`, `docs/decisions/settings.md`), for the heavy editing a light editor
//! leaves to it.
//!
//! The link is the editor's own remote form, filled in with the machine's SSH name, the path
//! and the line ([`slopty_settings::EditorLink::open`]). With no link set, a Mac opens the file
//! with the system's handler for its type: this Mac's own path for a tile of this Mac, or the
//! machine's place in Finder (`slopty_platform::files`) for one of a worker elsewhere. An iPhone
//! or iPad opens links only, so with no link set it offers nothing.
//!
//! What the app knows of each machine (its SSH name, its home, its place in Finder, whether it
//! is this Mac) is told to [`Editors`], a global, as the app learns it; each tile asks it for
//! its own path when the action runs, and offers the action only while it would open something.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::App;
use slopty_client::layout::WorkerKey;
use slopty_settings::EditorLink;

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        file,
        [
            /// Open the focused file, folder or review in the person's own editor.
            OpenInEditor,
        ]
    );
}
pub use actions::OpenInEditor;

/// What the app knows of one machine, for a link or a path to open.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    /// Its name, as its link and the directory say.
    pub name: String,
    /// How SSH reaches it (`[user@]host[:port]`), when it was installed from here; its name
    /// stands in otherwise.
    pub ssh: Option<String>,
    /// Its home, as its link said, so a `~/…` path is written out whole.
    pub home: Option<String>,
    /// Where its home is in Finder on this Mac, once its place there has been made.
    pub finder: Option<PathBuf>,
    /// Whether it is this Mac, whose paths open as they are.
    pub here: bool,
}

/// What opens a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opening {
    /// The editor's link.
    Link(String),
    /// A file or folder on this Mac, opened with the system's handler for its type.
    File(PathBuf),
}

/// The person's editor and what the app knows of each machine: a global the app keeps.
#[derive(Clone, Debug, Default)]
pub struct Editors {
    link: EditorLink,
    machines: HashMap<WorkerKey, Machine>,
}

impl gpui::Global for Editors {}

impl Editors {
    /// The editor link the app last took from the settings.
    #[must_use]
    pub const fn link(&self) -> &EditorLink {
        &self.link
    }

    /// What opens `path` on `worker` at `line`; `None` when nothing would.
    #[must_use]
    pub fn opening(&self, worker: WorkerKey, path: &str, line: Option<u32>) -> Option<Opening> {
        let machine = self.machines.get(&worker)?;
        let path = whole(path, machine.home.as_deref());
        if !self.link.is_empty() {
            let host = machine.ssh.as_deref().unwrap_or(&machine.name);
            return self.link.open(host, &path, line).map(Opening::Link);
        }
        if !cfg!(target_os = "macos") {
            return None;
        }
        if machine.here {
            return Some(Opening::File(PathBuf::from(path)));
        }
        let under = under_home(&path, machine.home.as_deref()?)?;
        machine.finder.as_ref().map(|root| Opening::File(root.join(under)))
    }

    /// Whether [`Self::opening`] would open `path` on `worker`, asked as a tile draws, so
    /// nothing is written out.
    #[must_use]
    pub fn offers(&self, worker: WorkerKey, path: &str) -> bool {
        let Some(machine) = self.machines.get(&worker) else { return false };
        if !self.link.is_empty() {
            return true;
        }
        if !cfg!(target_os = "macos") {
            return false;
        }
        machine.here
            || machine.finder.is_some()
                && machine.home.as_deref().is_some_and(|home| {
                    path == "~" || path.starts_with("~/") || under_home(path, home).is_some()
                })
    }

    /// The machine called `name`.
    fn named(&self, name: &str) -> Option<WorkerKey> {
        self.machines.iter().find(|(_, m)| m.name == name).map(|(key, _)| *key)
    }

    /// [`Self::opening`] for the machine called `name`: a review knows its machine by name.
    #[must_use]
    pub fn opening_named(&self, name: &str, path: &str, line: Option<u32>) -> Option<Opening> {
        self.opening(self.named(name)?, path, line)
    }

    /// The palette's line: "Open in Zed" for a link whose editor it knows, "Open in your editor"
    /// for another, "Open with default app" on a Mac with no link; `None` where nothing would
    /// open.
    #[must_use]
    pub fn label(&self) -> Option<String> {
        if self.link.is_empty() {
            return cfg!(target_os = "macos").then(|| OPEN_WITH_DEFAULT.to_owned());
        }
        Some(
            self.link
                .editor_name()
                .map_or_else(|| OPEN_IN_SOME_EDITOR.to_owned(), |name| format!("Open in {name}")),
        )
    }
}

/// The line for a link whose editor has no name Slopty knows.
pub const OPEN_IN_SOME_EDITOR: &str = "Open in your editor";
/// The line on a Mac with no editor link set.
pub const OPEN_WITH_DEFAULT: &str = "Open with default app";

/// Take the editor link from the settings.
pub fn set_link(link: EditorLink, cx: &mut App) {
    cx.default_global::<Editors>().link = link;
}

/// What the app now knows of `worker`.
pub fn set_machine(worker: WorkerKey, machine: Machine, cx: &mut App) {
    cx.default_global::<Editors>().machines.insert(worker, machine);
}

/// Change what the app knows of `worker`, from what it knew (the default before anything).
pub fn update_machine(worker: WorkerKey, change: impl FnOnce(&mut Machine), cx: &mut App) {
    change(cx.default_global::<Editors>().machines.entry(worker).or_default());
}

/// Whether a tile of `path` on `worker` offers the action ([`Editors::offers`]).
#[must_use]
pub fn offers(worker: WorkerKey, path: &str, cx: &App) -> bool {
    cx.try_global::<Editors>().is_some_and(|e| e.offers(worker, path))
}

/// Whether a tile of `path` on the machine called `name` offers the action.
#[must_use]
pub fn offers_named(name: &str, path: &str, cx: &App) -> bool {
    cx.try_global::<Editors>().is_some_and(|e| e.named(name).is_some_and(|w| e.offers(w, path)))
}

/// What opens `path` on `worker` at `line`, as [`Editors`] has it now.
#[must_use]
pub fn opening(worker: WorkerKey, path: &str, line: Option<u32>, cx: &App) -> Option<Opening> {
    cx.try_global::<Editors>()?.opening(worker, path, line)
}

/// What opens `path` on the machine called `name` at `line`.
#[must_use]
pub fn opening_named(name: &str, path: &str, line: Option<u32>, cx: &App) -> Option<Opening> {
    cx.try_global::<Editors>()?.opening_named(name, path, line)
}

/// Open what `opening` names with the system: the link, or the file as a `file://` link, which
/// the system opens with the handler for its type.
pub fn open(opening: &Opening, cx: &App) {
    cx.open_url(&opening.url());
}

impl Opening {
    /// What the system is asked to open.
    #[must_use]
    pub fn url(&self) -> String {
        match self {
            Self::Link(link) => link.clone(),
            Self::File(path) => {
                format!("file://{}", slopty_settings::link_path(&path.to_string_lossy()))
            }
        }
    }
}

/// `path` written out whole: `~` and `~/…` under `home`, when it is known.
fn whole(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.map(|h| h.trim_end_matches('/')) else { return path.to_owned() };
    match path.strip_prefix('~') {
        Some("") => home.to_owned(),
        Some(rest) if rest.starts_with('/') => format!("{home}{rest}"),
        _ => path.to_owned(),
    }
}

/// `path` relative to `home`, when it is in it: `""` for the home itself.
fn under_home<'a>(path: &'a str, home: &str) -> Option<&'a str> {
    let home = home.trim_end_matches('/');
    let rest = path.strip_prefix(home)?;
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(template: &str) -> EditorLink {
        let file = format!("[client]\neditor = \"{template}\"\n");
        slopty_settings::Settings::parse(&file).settings.client.editor
    }

    fn studio() -> Machine {
        Machine {
            name: "studio".to_owned(),
            ssh: None,
            home: Some("/Users/me".to_owned()),
            finder: Some(PathBuf::from("/Users/here/Library/CloudStorage/Slopty-studio")),
            here: false,
        }
    }

    /// [`Editors::opening`] for `path`, which [`Editors::offers`] agrees on.
    fn agreed(editors: &Editors, key: WorkerKey, path: &str) -> Option<Opening> {
        let opening = editors.opening(key, path, None);
        assert_eq!(editors.offers(key, path), opening.is_some(), "{path}");
        opening
    }

    /// A link is filled with the machine's SSH name (its name when it was not installed from
    /// here), the path written out whole, and the line; a machine nobody told of opens nothing.
    #[test]
    fn the_link_names_the_machine_the_path_and_the_line() {
        let key = WorkerKey::new(7);
        let mut editors =
            Editors { link: link("zed://ssh/{host}{path}:{line}"), ..Editors::default() };
        assert_eq!(editors.opening(key, "/a", None), None, "an unknown machine");
        editors.machines.insert(key, studio());
        assert_eq!(
            editors.opening(key, "~/src/main.rs", Some(42)),
            Some(Opening::Link("zed://ssh/studio/Users/me/src/main.rs:42".to_owned()))
        );
        editors
            .machines
            .insert(key, Machine { ssh: Some("me@studio.tail:2222".to_owned()), ..studio() });
        assert_eq!(
            editors.opening_named("studio", "/srv/app", None),
            Some(Opening::Link("zed://ssh/me@studio.tail:2222/srv/app:1".to_owned())),
            "a folder or a review opens at its first line"
        );
        assert_eq!(editors.label().as_deref(), Some("Open in Zed"));
    }

    /// With no link a Mac opens the file with the system's handler: this Mac's own path, or the
    /// machine's place in Finder for a path in its home; nothing outside the home, or before the
    /// place is made. An iPhone or iPad opens nothing without a link.
    #[test]
    fn with_no_link_the_system_handler_opens_the_file_in_finder_s_place() {
        let key = WorkerKey::new(7);
        let mut editors = Editors::default();
        editors.machines.insert(key, studio());
        let opened = editors.opening(key, "/Users/me/src/main.rs", Some(3));
        assert!(editors.offers(key, "/Users/me/src/main.rs"));
        let outside = agreed(&editors, key, "/etc/hosts");
        let home = agreed(&editors, key, "~");
        let sibling = agreed(&editors, key, "/Users/meg/a");
        editors.machines.insert(key, Machine { finder: None, ..studio() });
        let no_place = agreed(&editors, key, "~/a");
        editors.machines.insert(key, Machine { here: true, finder: None, ..studio() });
        let here = agreed(&editors, key, "/etc/hosts");
        if cfg!(target_os = "macos") {
            let place = PathBuf::from("/Users/here/Library/CloudStorage/Slopty-studio");
            assert_eq!(opened, Some(Opening::File(place.join("src/main.rs"))));
            assert_eq!(home, Some(Opening::File(place.join(""))));
            assert_eq!(outside, None, "Finder holds only the home");
            assert_eq!(sibling, None, "a name that starts as the home's is not in it");
            assert_eq!(no_place, None, "its place in Finder is not made yet");
            assert_eq!(here, Some(Opening::File(PathBuf::from("/etc/hosts"))));
            assert_eq!(editors.label().as_deref(), Some(OPEN_WITH_DEFAULT));
        } else {
            assert!([opened, outside, home, sibling, no_place, here].iter().all(Option::is_none));
            assert_eq!(editors.label(), None, "no line without a link");
        }
    }

    /// A link Slopty does not know the editor of is still opened, under a plain line.
    #[test]
    fn another_editor_s_link_opens_under_a_plain_line() {
        let editors = Editors { link: link("myeditor://open?file={path}"), ..Editors::default() };
        assert_eq!(editors.label().as_deref(), Some(OPEN_IN_SOME_EDITOR));
        let vscode = Editors {
            link: link("vscode://vscode-remote/ssh-remote+{host}{path}"),
            ..Editors::default()
        };
        assert_eq!(vscode.label().as_deref(), Some("Open in VS Code"));
    }
}
