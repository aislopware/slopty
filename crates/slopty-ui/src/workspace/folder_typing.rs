//! A path typed into the folder step completes from the machine's own folders (readiness R14).
//!
//! Typing `~/w` or `/Users/c/work/sl` in the step that asks where an agent starts lists the
//! folders of the typed path's parent that its last part begins, after the line for the path as
//! typed: `~/work`, `~/www`. The parent is asked of the machine once per step
//! (`ClientMsg::ListFolder`, as a folder tile asks), and the lines follow its answer. A part that
//! begins with a dot lists hidden folders too; nothing else does. A typed `/` at the end lists
//! the folder's own folders.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{Context, Entity};
use slopty_client::layout::WorkerKey;
use slopty_proto::ClientMsg;
use slopty_proto::folder::Listing;
use slopty_proto::orchestration::FileKind;

use super::WorkspaceView;
use crate::palette::CommandPalette;

/// The most completions listed under a typed path.
const COMPLETIONS: usize = 12;

/// The folders listed for the paths typed into one folder step, by the folder as typed (`~`,
/// `/Users/c/work`), each with whether it is hidden; shared with the step's line builder.
#[derive(Clone, Debug, Default)]
pub(super) struct Listed(Rc<RefCell<Known>>);

#[derive(Debug, Default)]
struct Known {
    /// Each folder's own folders, as listed.
    folders: HashMap<String, Vec<(String, bool)>>,
    /// The folders asked for, answered or not.
    asked: HashSet<String>,
}

/// A typed path as the folder it is in and the start of its last part: `~/wo` is `~` and
/// `wo`, `/a/b/` is `/a/b` and nothing, `~` is `~` and nothing. `None` for anything that is not
/// a path from a root (`/`, `~/`, `~`).
pub(super) fn split(typed: &str) -> Option<(String, String)> {
    if typed == "~" {
        return Some(("~".to_owned(), String::new()));
    }
    if !(typed.starts_with('/') || typed.starts_with("~/")) {
        return None;
    }
    let (dir, part) = typed.rsplit_once('/')?;
    let dir = if dir.is_empty() { "/" } else { dir };
    Some((dir.to_owned(), part.to_owned()))
}

/// `name` in `dir`, as a path.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

impl Listed {
    /// The folders that complete `typed`, in the machine's order: those in its folder whose name
    /// begins with its last part, any case, other than that part itself; hidden ones only for a
    /// part that begins with a dot.
    pub(super) fn completing(&self, typed: &str) -> Vec<String> {
        let Some((dir, part)) = split(typed) else { return Vec::new() };
        let known = self.0.borrow();
        let Some(folders) = known.folders.get(&dir) else { return Vec::new() };
        let lower = part.to_lowercase();
        folders
            .iter()
            .filter(|(name, hidden)| {
                (!hidden || part.starts_with('.'))
                    && name.to_lowercase().starts_with(&lower)
                    && *name != part
            })
            .take(COMPLETIONS)
            .map(|(name, _)| join(&dir, name))
            .collect()
    }

    /// Whether `dir` is to be asked for: the first time it is typed.
    fn ask(&self, dir: &str) -> bool {
        self.0.borrow_mut().asked.insert(dir.to_owned())
    }

    /// Whether `dir` was asked for here.
    fn asked(&self, dir: &str) -> bool {
        self.0.borrow().asked.contains(dir)
    }

    /// The machine listed `dir`.
    fn keep(&self, dir: &str, listing: &Listing) {
        let folders = match listing {
            Listing::Listed { entries, .. } => entries
                .iter()
                .filter(|e| e.kind == FileKind::Dir)
                .map(|e| (e.name.clone(), e.hidden))
                .collect(),
            Listing::NotFolder | Listing::Missing { .. } => Vec::new(),
        };
        self.0.borrow_mut().folders.insert(dir.to_owned(), folders);
    }
}

impl WorkspaceView {
    /// The folder step's field says `text`: the folder a typed path is in is asked of its
    /// machine, once per step.
    pub(super) fn ask_typed_folder(&self, palette: &Entity<CommandPalette>, text: &str) {
        let Some(step) = self.folder_step.as_ref().filter(|s| s.step() == palette.entity_id())
        else {
            return;
        };
        let Some((dir, _)) = split(text.trim()) else { return };
        if self.workers.get(&step.worker()).is_some_and(super::Worker::is_linked)
            && step.typed().ask(&dir)
        {
            self.send(step.worker(), ClientMsg::ListFolder { path: dir });
        }
    }

    /// `key` listed `path`: when the folder step there asked for it, its typed lines follow.
    pub(super) fn typed_folder_listed(
        &self,
        key: WorkerKey,
        path: &str,
        listing: &Listing,
        cx: &mut Context<Self>,
    ) {
        let Some(step) = self.folder_step.as_ref().filter(|s| s.worker() == key) else { return };
        if !step.typed().asked(path) {
            return;
        }
        step.typed().keep(path, listing);
        if let Some(palette) = self.palette.clone().filter(|p| p.entity_id() == step.step()) {
            palette.update(cx, CommandPalette::retype);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A typed path splits into its folder and the start of its last part, from either root.
    #[test]
    fn a_typed_path_splits_at_its_last_slash() {
        let pair = |a: &str, b: &str| Some((a.to_owned(), b.to_owned()));
        assert_eq!(split("~"), pair("~", ""));
        assert_eq!(split("~/wo"), pair("~", "wo"));
        assert_eq!(split("/Users/c/work/"), pair("/Users/c/work", ""));
        assert_eq!(split("/us"), pair("/", "us"));
        assert_eq!(split("/"), pair("/", ""));
        assert_eq!(split("work"), None);
        assert_eq!(split("~work"), None);
    }

    /// The listed folders complete the last part in any case, never the part itself, and a
    /// hidden one only for a dot.
    #[test]
    fn the_listed_folders_complete_the_last_part() {
        let listed = Listed::default();
        listed.0.borrow_mut().folders.insert(
            "~".to_owned(),
            vec![
                ("Work".to_owned(), false),
                ("www".to_owned(), false),
                ("notes".to_owned(), false),
                (".config".to_owned(), true),
            ],
        );
        assert_eq!(listed.completing("~/w"), ["~/Work", "~/www"]);
        assert_eq!(listed.completing("~/"), ["~/Work", "~/www", "~/notes"]);
        assert_eq!(listed.completing("~/www"), Vec::<String>::new(), "the part itself");
        assert_eq!(listed.completing("~/.c"), ["~/.config"]);
        assert!(listed.completing("/elsewhere/x").is_empty(), "not listed");
    }
}
