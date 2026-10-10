//! A daemon's own tables of the file, read and edited from another device.
//!
//! A worker reads `[worker]` and the server `[server]` of the `settings.toml` on their own
//! machine, and both read `[network]`. A settings page on another device sends the edits it would
//! make to its own file ([`Edit`]), and [`read_and_edit`] makes them here:
//! - only to the daemon's own tables ([`WORKER`], [`SERVER`]);
//! - only to keys of the file's schema ([`crate::schema::fields`]), each value checked as the form
//!   checks it;
//! - only when the file still reads afterwards.
//!
//! The file is replaced whole, through a file beside it, so a daemon following it
//! ([`crate::follow`]) reads it before or after and never half-written.

use std::path::{Path, PathBuf};

use crate::schema::{self, Kind};
use crate::{Settings, edit};

/// The tables a worker reads and takes edits to.
pub const WORKER: [&str; 2] = ["worker", "network"];
/// The tables the server reads and takes edits to.
pub const SERVER: [&str; 2] = ["server", "network"];

/// One edit, as the settings form makes it to its own file.
#[derive(Clone, Copy, Debug)]
pub struct Edit<'a> {
    /// The table, dotted (`worker`, `server.projects`).
    pub table: &'a str,
    /// The key in it.
    pub key: &'a str,
    /// The entry of the key's map this edit is to; `None` for the key itself.
    pub entry: Option<&'a str>,
    /// The new value as TOML text; `None` removes it.
    pub literal: Option<&'a str>,
}

/// The file as it stands after the edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Where it is.
    pub path: PathBuf,
    /// Its text; empty when there is no file yet.
    pub text: String,
    /// What of the daemon's own tables does not read, each in words.
    pub problems: Vec<String>,
}

/// Why the edits were not made.
#[derive(Debug, thiserror::Error)]
pub enum Refused {
    /// An edit is outside the daemon's tables, names no key of the schema, holds a value the
    /// key does not take, or leaves the file unreadable. Nothing was written.
    #[error("{0}")]
    Edit(String),
    /// The file could not be read or written.
    #[error("{0}")]
    Io(String),
}

/// Whether `table` is `root` or one of its own (`server.projects` under `server`).
#[must_use]
pub fn under(table: &str, root: &str) -> bool {
    table == root || table.strip_prefix(root).is_some_and(|rest| rest.starts_with('.'))
}

/// Whether `table` is under one of `roots`.
fn owned(table: &str, roots: &[&str]) -> bool {
    roots.iter().any(|root| under(table, root))
}

/// Make `edits`, in order, to the file at `path`, only under `roots`, then read it.
///
/// No edit is a read. Nothing is written unless every edit holds and the file still reads.
///
/// # Errors
/// [`Refused::Edit`] for an edit that does not hold; [`Refused::Io`] when the file does not
/// read or write.
pub fn read_and_edit(path: &Path, roots: &[&str], edits: &[Edit<'_>]) -> Result<File, Refused> {
    let before = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Refused::Io(format!("{}: {e}", path.display()))),
    };
    let mut text = before.clone();
    for one in edits {
        text = apply(&text, roots, one).map_err(Refused::Edit)?;
    }
    let loaded = Settings::parse(&text);
    if text != before {
        if let Some(error) = &loaded.error {
            return Err(Refused::Edit(format!("the file would not read: {error}")));
        }
        replace(path, &text).map_err(|e| Refused::Io(format!("{}: {e}", path.display())))?;
    }
    let own = |w: &&String| {
        w.strip_prefix("unknown key `").is_some_and(|key| owned(key.trim_end_matches('`'), roots))
    };
    let mut problems: Vec<String> = loaded.warnings.iter().filter(own).cloned().collect();
    problems.extend(loaded.error.map(|e| e.to_string()));
    Ok(File { path: path.to_path_buf(), text, problems })
}

/// `text` with `one` made, checked against the schema.
fn apply(text: &str, roots: &[&str], one: &Edit<'_>) -> Result<String, String> {
    let Edit { table, key, entry, literal } = *one;
    let name = format!("{table}.{key}");
    if !owned(table, roots) {
        let tables = roots.iter().map(|r| format!("[{r}]")).collect::<Vec<_>>().join(" and ");
        return Err(format!("{name} is not this daemon's to change; it changes {tables}"));
    }
    let field = schema::fields()
        .iter()
        .find(|f| f.table == table && f.key == key)
        .ok_or_else(|| format!("{name} is no key of the settings file"))?;
    match (entry, literal) {
        (None, None) => Ok(edit::remove(text, table, key)),
        (None, Some(literal)) => {
            field.check(literal).map_err(|e| format!("{name} = {literal}: {e}"))?;
            Ok(edit::write(text, table, key, literal))
        }
        (Some(_), _) if !matches!(field.kind, Kind::Map(_)) => {
            Err(format!("{name} holds one value, not entries"))
        }
        (Some(entry), None) => Ok(edit::remove_entry(text, table, key, entry)),
        (Some(entry), Some(literal)) => {
            field.check_entry(entry, literal).map_err(|e| format!("{name}.{entry}: {e}"))?;
            Ok(edit::write_entry(text, table, key, entry, literal))
        }
    }
}

/// Replace the file at `path` with `text` whole, through a file beside it.
fn replace(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut beside = path.as_os_str().to_owned();
    beside.push(".editing");
    let beside = PathBuf::from(beside);
    std::fs::write(&beside, text)?;
    std::fs::rename(&beside, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit<'a>(table: &'a str, key: &'a str, literal: Option<&'a str>) -> Edit<'a> {
        Edit { table, key, entry: None, literal }
    }

    /// Edits under the daemon's tables, `[network]` among them, land in its file, the rest of it as
    /// it was; an entry of a map goes in and out on its own; with no edits the file is only
    /// read.
    #[test]
    fn a_daemon_s_own_keys_are_edited_in_place() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "# mine\n[font]\nmono_size = 13.0\n").expect("written");
        let edits = [
            edit("worker", "keep_awake", Some(r#""never""#)),
            edit("network", "server", Some(r#""studio""#)),
            Edit {
                table: "worker",
                key: "acp",
                entry: Some("gemini"),
                literal: Some(r#"["gemini", "--acp"]"#),
            },
        ];
        let file = read_and_edit(&path, &WORKER, &edits).expect("edited");
        let read = Settings::load(&path);
        assert!(read.error.is_none());
        assert_eq!(read.settings.worker.keep_awake, crate::KeepAwake::Never);
        let server = read.settings.network.server.as_ref().map(ToString::to_string);
        assert_eq!(server.as_deref(), Some("studio:45560"), "[network] is the worker's too");
        assert_eq!(
            read.settings.worker.acp.get("gemini"),
            Some(&vec!["gemini".to_owned(), "--acp".to_owned()])
        );
        assert!((read.settings.font.mono_size - 13.0).abs() < f32::EPSILON);
        assert!(file.text.starts_with("# mine\n"), "{}", file.text);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), file.text);

        let gone = Edit { table: "worker", key: "acp", entry: Some("gemini"), literal: None };
        read_and_edit(&path, &WORKER, &[gone]).expect("edited");
        assert!(Settings::load(&path).settings.worker.acp.is_empty());
        let only = read_and_edit(&path, &WORKER, &[]).expect("read");
        assert_eq!(only.text, std::fs::read_to_string(&path).expect("read"));
    }

    /// An edit outside the daemon's tables, to no key, or of a value the key does not take is
    /// refused, and nothing of the edits before it is written.
    #[test]
    fn an_edit_that_does_not_hold_writes_nothing() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "[worker]\nkeep_awake = \"never\"\n").expect("written");
        let first = edit("worker", "keep_awake", Some(r#""working""#));
        for wrong in [
            edit("server.projects", "live_agents", Some("4")),
            edit("font", "mono_size", Some("12.0")),
            edit("worker", "no_such_key", Some("1")),
            edit("worker", "keep_awake", Some(r#""sometimes""#)),
            Edit { table: "worker", key: "keep_awake", entry: Some("x"), literal: Some("1") },
        ] {
            let refused = read_and_edit(&path, &WORKER, &[first, wrong]);
            assert!(matches!(refused, Err(Refused::Edit(_))), "{wrong:?}: {refused:?}");
        }
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "[worker]\nkeep_awake = \"never\"\n"
        );
        assert!(under("server.projects", "server") && !under("serverless", "server"));
    }

    /// A file not there yet reads as empty and is made by the first edit; what in the daemon's
    /// own tables does not read is said.
    #[test]
    fn a_missing_file_is_made_and_problems_are_said() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("deeper").join("settings.toml");
        let read = read_and_edit(&path, &SERVER, &[]).expect("read");
        assert_eq!((read.text.as_str(), read.problems.len()), ("", 0));
        let allow = edit("network", "allow", Some(r#"["10.8.0.0/24"]"#));
        read_and_edit(&path, &SERVER, &[allow]).expect("edited");
        assert_eq!(Settings::load(&path).settings.network.allow, ["10.8.0.0/24"]);

        let text = "[server]\nallowed = []\n[network]\nallows = []\n[font]\nnope = 1\n";
        std::fs::write(&path, text).expect("written");
        let read = read_and_edit(&path, &SERVER, &[]).expect("read");
        assert_eq!(read.problems, ["unknown key `network.allows`", "unknown key `server.allowed`"]);
    }
}
