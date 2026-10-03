//! A folder tile's changes to the worker's files: a new folder, a move or rename, and a trip to
//! the OS's own trash ([`slopty_platform::trash`]).
//!
//! Each op is checked before anything is touched and refused, as a [`FsRefusal`], when it would
//! do what it must not: a path that is relative or climbs with `..`, a name that is not one
//! plain name, a move or trash of a place that holds others' work (the file system's root, a
//! volume's, the home or a folder holding it), or anything already at the destination. Nothing
//! is ever replaced: a move is a rename that fails when its destination is taken, in one step
//! where the file system allows it ([`slopty_platform::fs::rename_new`]). Nothing is ever
//! unlinked: a trashed entry can be put back from the OS's trash.

use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use slopty_platform::trash::TrashError;
use slopty_proto::folder::{FsOp, FsOutcome, FsRefusal};

/// Do `op` as this worker's user, its home the place `~` names.
#[must_use]
pub fn apply(op: &FsOp) -> FsOutcome {
    apply_in(op, &slopty_platform::dirs::home(), slopty_platform::trash::trash)
}

/// [`apply`] with the home and the trash given.
fn apply_in(
    op: &FsOp,
    home: &Path,
    trash: impl FnOnce(&Path) -> Result<PathBuf, TrashError>,
) -> FsOutcome {
    let done = match op {
        FsOp::MakeDir { parent, name } => make_dir(home, parent, name),
        FsOp::Move { from, to } => move_to(home, from, to),
        FsOp::Trash { path } => throw_away(home, path, trash),
    };
    done.unwrap_or_else(|refused| refused)
}

/// An op's end, refused or failed, before it reaches the end of its function.
type Step<T> = Result<T, FsOutcome>;

fn make_dir(home: &Path, parent: &str, name: &str) -> Step<FsOutcome> {
    let dir = place(home, parent)?;
    if !plain(name) {
        return Err(refused(FsRefusal::BadName { name: name.to_owned() }));
    }
    let at = dir.join(name);
    #[expect(
        clippy::create_dir,
        reason = "one folder in one that is there: a missing parent is said, never made"
    )]
    let made = std::fs::create_dir(&at);
    match made {
        Ok(()) => Ok(done(&at)),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(clash(&at)),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(missing(&dir)),
        Err(e) => Err(failed(&e)),
    }
}

fn move_to(home: &Path, from: &str, to: &str) -> Step<FsOutcome> {
    let (from, to) = (place(home, from)?, place(home, to)?);
    guard(home, &from)?;
    let into = to.parent().ok_or_else(|| protected(&to))?;
    if from == to {
        return Ok(done(&to));
    }
    let source = there(&from)?;
    if to.starts_with(&from)
        || real(into).is_some_and(|i| real(&from).is_some_and(|f| i.starts_with(f)))
    {
        return Err(refused(FsRefusal::IntoItself));
    }
    match std::fs::metadata(into) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(failed(&std::io::Error::from(ErrorKind::NotADirectory))),
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(missing(into)),
        Err(e) => return Err(failed(&e)),
    }
    // A name's case changed on a volume that ignores case: the destination is the source.
    let itself = std::fs::symlink_metadata(&to)
        .is_ok_and(|at| (at.dev(), at.ino()) == (source.dev(), source.ino()));
    let moved = if itself {
        std::fs::rename(&from, &to)
    } else {
        slopty_platform::fs::rename_new(&from, &to)
    };
    match moved {
        Ok(()) => Ok(done(&to)),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(clash(&to)),
        Err(e) if e.kind() == ErrorKind::CrossesDevices => Err(refused(FsRefusal::OtherVolume)),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(missing(&from)),
        Err(e) => Err(failed(&e)),
    }
}

fn throw_away(
    home: &Path,
    path: &str,
    trash: impl FnOnce(&Path) -> Result<PathBuf, TrashError>,
) -> Step<FsOutcome> {
    let path = place(home, path)?;
    guard(home, &path)?;
    there(&path)?;
    match trash(&path) {
        Ok(landed) => Ok(done(&landed)),
        Err(TrashError::NoTrash) => Err(refused(FsRefusal::NoTrash)),
        Err(TrashError::Os(e)) if e.kind() == ErrorKind::NotFound => Err(missing(&path)),
        Err(TrashError::Os(e)) => Err(failed(&e)),
    }
}

/// `path` as the worker names it: `~` spelled out, lexically clean (`a//b/./` is `a/b`), not
/// resolved, so a link in it stays one. Refused when it is relative or climbs.
fn place(home: &Path, path: &str) -> Step<PathBuf> {
    let spelled = match path.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(path),
    };
    let climbs = spelled.components().any(|c| c == Component::ParentDir);
    if !spelled.is_absolute() || climbs {
        return Err(refused(FsRefusal::NotAbsolute { path: path.to_owned() }));
    }
    Ok(spelled.components().collect())
}

/// Refuse to move or trash a place that holds others' work: the root, a volume's root (a mount
/// point), the home, or a folder the home is in. Looked at as named and as resolved, so a link
/// to the home is refused too.
fn guard(home: &Path, path: &Path) -> Step<()> {
    let holds_home = |p: &Path| home.starts_with(p) || real(home).is_some_and(|h| h.starts_with(p));
    let mount = std::fs::symlink_metadata(path).is_ok_and(|own| {
        !own.file_type().is_symlink()
            && path
                .parent()
                .and_then(|up| std::fs::metadata(up).ok())
                .is_some_and(|up| up.dev() != own.dev())
    });
    let resolved_holds = real(path).is_some_and(|r| holds_home(&r) || r.parent().is_none());
    if path.parent().is_none() || holds_home(path) || resolved_holds || mount {
        return Err(protected(path));
    }
    Ok(())
}

/// What is at `path` itself (a link, not what it points to); refused as missing when nothing.
fn there(path: &Path) -> Step<std::fs::Metadata> {
    std::fs::symlink_metadata(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound => missing(path),
        _other => failed(&e),
    })
}

/// `path` with every link resolved, when it is there.
fn real(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// One plain name: not empty, not `.` or `..`, no `/` and no NUL.
fn plain(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']))
}

fn shown(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn done(path: &Path) -> FsOutcome {
    FsOutcome::Done { path: shown(path) }
}

const fn refused(why: FsRefusal) -> FsOutcome {
    FsOutcome::Refused(why)
}

fn protected(path: &Path) -> FsOutcome {
    refused(FsRefusal::Protected { path: shown(path) })
}

fn clash(path: &Path) -> FsOutcome {
    refused(FsRefusal::Clash { path: shown(path) })
}

fn missing(path: &Path) -> FsOutcome {
    refused(FsRefusal::Missing { path: shown(path) })
}

fn failed(e: &std::io::Error) -> FsOutcome {
    FsOutcome::Failed { error: crate::file::os_word(e) }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A home and a trash can in a temporary directory: the trash moves an entry into `bin`,
    /// as the OS's would, so the tests never touch the person's own.
    struct World {
        root: tempfile::TempDir,
    }

    impl World {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            for dir in ["home/work", "bin"] {
                std::fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            Self { root }
        }

        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }

        fn at(&self, rel: &str) -> PathBuf {
            self.home().join(rel)
        }

        fn s(&self, rel: &str) -> String {
            shown(&self.at(rel))
        }

        fn apply(&self, op: &FsOp) -> FsOutcome {
            let bin = self.root.path().join("bin");
            apply_in(op, &self.home(), |path: &Path| {
                let to = bin.join(path.file_name().unwrap());
                std::fs::rename(path, &to)?;
                Ok(to)
            })
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A folder is made in an existing one, under `~` too; one already there is a clash and is
    /// left as it was, a parent that is not there is missing, and a name that is not one plain
    /// name is refused before anything is touched.
    #[test]
    fn a_folder_is_made_once_and_only_by_a_plain_name() {
        let w = World::new();
        let make = |parent: &str, name: &str| FsOp::MakeDir {
            parent: parent.to_owned(),
            name: name.to_owned(),
        };
        assert_eq!(w.apply(&make(&w.s("work"), "new")), FsOutcome::Done { path: w.s("work/new") });
        assert!(w.at("work/new").is_dir());
        write(&w.at("work/new/kept.txt"), "kept");
        assert_eq!(
            w.apply(&make("~/work", "new")),
            refused(FsRefusal::Clash { path: w.s("work/new") })
        );
        assert!(w.at("work/new/kept.txt").exists(), "a clash leaves what is there");
        assert_eq!(
            w.apply(&make(&w.s("gone"), "new")),
            refused(FsRefusal::Missing { path: w.s("gone") })
        );
        for bad in ["", ".", "..", "a/b", "nul\0"] {
            assert_eq!(
                w.apply(&make(&w.s("work"), bad)),
                refused(FsRefusal::BadName { name: bad.to_owned() }),
                "{bad:?}"
            );
        }
        assert_eq!(w.apply(&make("~", "top")), FsOutcome::Done { path: w.s("top") });
    }

    /// A move renames in a folder or moves to another; it never replaces what is at the
    /// destination, never puts a folder inside itself, and says a source that is not there.
    #[test]
    fn a_move_never_replaces_and_never_goes_into_itself() {
        let w = World::new();
        let mv = |from: &Path, to: &Path| FsOp::Move { from: shown(from), to: shown(to) };
        write(&w.at("work/a.txt"), "a");
        write(&w.at("work/b.txt"), "b");
        write(&w.at("work/dir/inner.txt"), "inner");

        let renamed = w.apply(&mv(&w.at("work/a.txt"), &w.at("work/c.txt")));
        assert_eq!(renamed, FsOutcome::Done { path: w.s("work/c.txt") });
        assert_eq!(std::fs::read_to_string(w.at("work/c.txt")).unwrap(), "a");

        let onto = w.apply(&mv(&w.at("work/c.txt"), &w.at("work/b.txt")));
        assert_eq!(onto, refused(FsRefusal::Clash { path: w.s("work/b.txt") }));
        assert_eq!(std::fs::read_to_string(w.at("work/b.txt")).unwrap(), "b", "not replaced");
        assert!(w.at("work/c.txt").exists(), "and the source stays");

        let moved = w.apply(&mv(&w.at("work/dir"), &w.at("dir")));
        assert_eq!(moved, FsOutcome::Done { path: w.s("dir") });
        assert_eq!(std::fs::read_to_string(w.at("dir/inner.txt")).unwrap(), "inner");

        let inside = w.apply(&mv(&w.at("dir"), &w.at("dir/deeper/dir")));
        assert_eq!(inside, refused(FsRefusal::IntoItself));
        let gone = w.apply(&mv(&w.at("nope"), &w.at("work/nope")));
        assert_eq!(gone, refused(FsRefusal::Missing { path: w.s("nope") }));
        let nowhere = w.apply(&mv(&w.at("work/b.txt"), &w.at("absent/b.txt")));
        assert_eq!(nowhere, refused(FsRefusal::Missing { path: w.s("absent") }));
        let same = w.apply(&mv(&w.at("work/b.txt"), &w.at("work/b.txt")));
        assert_eq!(same, FsOutcome::Done { path: w.s("work/b.txt") });
    }

    /// Only a name's case changed: on a volume that ignores case the destination is the source
    /// itself, which is a rename, not a clash.
    #[test]
    fn a_name_can_change_only_its_case() {
        let w = World::new();
        write(&w.at("work/readme.md"), "r");
        let op = FsOp::Move { from: w.s("work/readme.md"), to: w.s("work/README.md") };
        assert_eq!(w.apply(&op), FsOutcome::Done { path: w.s("work/README.md") });
        let names: Vec<String> = std::fs::read_dir(w.at("work"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["README.md"]);
    }

    /// The trash takes an entry and says where it landed; nothing there is missing, and the
    /// trash is never asked for it.
    #[test]
    fn the_trash_takes_an_entry_and_says_where() {
        let w = World::new();
        write(&w.at("work/old.txt"), "old");
        let op = FsOp::Trash { path: "~/work/old.txt".to_owned() };
        let landed = shown(&w.root.path().join("bin/old.txt"));
        assert_eq!(w.apply(&op), FsOutcome::Done { path: landed });
        assert!(!w.at("work/old.txt").exists());

        let asked = Cell::new(false);
        let gone = apply_in(&op, &w.home(), |_: &Path| {
            asked.set(true);
            Err(TrashError::NoTrash)
        });
        assert_eq!(gone, refused(FsRefusal::Missing { path: w.s("work/old.txt") }));
        assert!(!asked.get());

        write(&w.at("work/share.txt"), "s");
        let none = apply_in(&FsOp::Trash { path: w.s("work/share.txt") }, &w.home(), |_: &Path| {
            Err(TrashError::NoTrash)
        });
        assert_eq!(none, refused(FsRefusal::NoTrash));
    }

    /// The root, the home, a folder holding the home, and a link to the home are never moved
    /// or trashed; a relative path or one that climbs is refused before anything is looked at.
    #[test]
    fn the_places_that_hold_everything_are_refused() {
        let w = World::new();
        std::os::unix::fs::symlink(w.home(), w.at("work/home-link")).unwrap();
        let elsewhere = shown(&w.root.path().join("moved"));
        for path in
            ["/".to_owned(), "~".to_owned(), w.s(""), shown(w.root.path()), w.s("work/home-link")]
        {
            let trash = w.apply(&FsOp::Trash { path: path.clone() });
            assert!(
                matches!(trash, FsOutcome::Refused(FsRefusal::Protected { .. })),
                "{path}: {trash:?}"
            );
            let mv = w.apply(&FsOp::Move { from: path.clone(), to: elsewhere.clone() });
            assert!(
                matches!(mv, FsOutcome::Refused(FsRefusal::Protected { .. })),
                "{path}: {mv:?}"
            );
        }
        assert!(w.home().is_dir());
        for path in ["work/a", "~/../x", "/w/../etc"] {
            let trash = w.apply(&FsOp::Trash { path: path.to_owned() });
            assert_eq!(trash, refused(FsRefusal::NotAbsolute { path: path.to_owned() }));
        }
    }

    /// A volume's root is a mount point (`/dev`, a file system of its own on a Mac and on
    /// Linux), which a move or trash never takes.
    #[test]
    fn a_volume_root_is_refused() {
        let w = World::new();
        let trash = w.apply(&FsOp::Trash { path: "/dev".to_owned() });
        assert_eq!(trash, refused(FsRefusal::Protected { path: "/dev".to_owned() }));
    }
}
