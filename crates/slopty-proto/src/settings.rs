//! A daemon's settings, read and edited from another device
//! ([`crate::orchestration::Verb::Settings`]).
//!
//! A worker reads `[worker]` of the `settings.toml` on its own machine, the server `[server]` of
//! its own. A settings page on another device edits those keys there: it sends the same edits it
//! makes to its own file ([`SettingEdit`]), and the daemon makes them to its file and takes the
//! file up again, as it does after a hand edit.

use serde::{Deserialize, Serialize};

/// One edit to a settings file, as `slopty_settings::edit` makes it: a key set or removed in a
/// table, or an entry of a key's inline table set or removed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SettingEdit {
    /// The table, dotted (`worker`, `server.projects`).
    pub table: String,
    /// The key in it.
    pub key: String,
    /// The entry of the key's inline table this edit is to (an ACP agent's name under
    /// `worker.acp`); `None` for the key itself.
    pub entry: Option<String>,
    /// The new value as TOML text (`"ssh"`, `8`, `["a", "b"]`); `None` removes it.
    pub literal: Option<String>,
}

impl SettingEdit {
    /// Whether it edits a key under `root` (`worker`, `server`): the table is `root` or one
    /// of its own.
    #[must_use]
    pub fn under(&self, root: &str) -> bool {
        self.table == root
            || self.table.strip_prefix(root).is_some_and(|rest| rest.starts_with('.'))
    }
}

/// A daemon's settings file as it stands ([`crate::orchestration::Outcome::Settings`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DaemonSettings {
    /// Where the file is on the daemon's machine, `~` spelled out.
    pub path: String,
    /// Its text; empty when there is no file yet.
    pub text: String,
    /// The tables the daemon reads and takes edits to (`worker`, `server`).
    pub tables: Vec<String>,
    /// What in the file the daemon could not read, each in words; it runs on its defaults
    /// for those keys.
    pub problems: Vec<String>,
}
