//! Where Slopty keeps its files and its sockets.
//!
//! - macOS: `~/Library/Application Support/Slopty`, and sockets under the per-user `$TMPDIR`
//!   (`$TMPDIR/slopty`, which only its user can enter).
//! - Linux: the XDG base directories. Data in `$XDG_DATA_HOME/slopty`, else
//!   `~/.local/share/slopty`; sockets in `$XDG_RUNTIME_DIR/slopty`, else `/tmp/slopty-<uid>`, since
//!   `/tmp` is shared and a bare `/tmp/slopty` would be every user's.
//!
//! `$SLOPTY_DATA_DIR` overrides the data directory on both. [`Layout`] holds both rules and
//! takes the environment as an argument, so each is tested on any host. [`home`] is the one answer
//! to where `~` is.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A platform's directory rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// macOS and iOS.
    Apple,
    /// The XDG base directories (Linux).
    Xdg,
}

impl Layout {
    /// The rules of the platform this build serves.
    pub const NATIVE: Self = if cfg!(target_os = "linux") { Self::Xdg } else { Self::Apple };

    /// The data directory, reading the environment through `env`; a `$HOME` that is not an
    /// absolute path defers to [`home`].
    pub fn data_dir(self, env: impl Fn(&str) -> Option<OsString>) -> PathBuf {
        if let Some(dir) = env("SLOPTY_DATA_DIR").filter(|d| !d.is_empty()) {
            return PathBuf::from(dir);
        }
        let user_home = || absolute(env("HOME")).unwrap_or_else(home);
        match self {
            Self::Apple => user_home().join("Library").join("Application Support").join("Slopty"),
            Self::Xdg => absolute(env("XDG_DATA_HOME"))
                .unwrap_or_else(|| user_home().join(".local").join("share"))
                .join("slopty"),
        }
    }

    /// The directory the daemons' sockets go in, given the system's temporary directory and
    /// this user's uid. Whoever binds a socket there creates it with mode 0700.
    pub fn runtime_dir(
        self,
        env: impl Fn(&str) -> Option<OsString>,
        tmp: &Path,
        uid: u32,
    ) -> PathBuf {
        match self {
            Self::Apple => tmp.join("slopty"),
            Self::Xdg => absolute(env("XDG_RUNTIME_DIR"))
                .map_or_else(|| tmp.join(format!("slopty-{uid}")), |run| run.join("slopty")),
        }
    }
}

/// An XDG variable's value, if set to an absolute path; the specification says a relative
/// one is invalid and to be ignored.
fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|p| p.is_absolute())
}

/// The user's home directory: `$HOME` when it is set and not empty, else the home in the user's
/// password-database entry (`std::env::home_dir`), and `/` when neither is an absolute path.
///
/// `/` rather than `/tmp`: `/tmp` is every user's, so a settings file, a terminfo database or a
/// `LaunchAgent` put there could be planted or read by another user, while under `/` the write
/// fails and says so.
#[must_use]
pub fn home() -> PathBuf {
    std::env::home_dir().filter(|h| h.is_absolute()).unwrap_or_else(|| PathBuf::from("/"))
}

/// This platform's data directory, as the environment sets it now.
#[must_use]
pub fn data_dir() -> PathBuf {
    Layout::NATIVE.data_dir(|name| std::env::var_os(name))
}

/// This platform's socket directory, as the environment sets it now.
#[must_use]
pub fn runtime_dir() -> PathBuf {
    let uid = rustix::process::getuid().as_raw();
    Layout::NATIVE.runtime_dir(|name| std::env::var_os(name), &std::env::temp_dir(), uid)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::{Layout, home};

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, OsString)> =
            pairs.iter().map(|(k, v)| ((*k).to_owned(), OsString::from(v))).collect();
        move |name| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    /// XDG's data home when it is absolute, `~/.local/share` when it is unset or relative, and
    /// `$SLOPTY_DATA_DIR` over both; macOS keeps Application Support.
    #[test]
    fn the_data_directory_follows_each_platforms_rules() {
        let data = |layout: Layout, pairs: &[(&str, &str)]| layout.data_dir(env(pairs));
        let home = ("HOME", "/home/me");
        assert_eq!(
            data(Layout::Xdg, &[home, ("XDG_DATA_HOME", "/data")]),
            PathBuf::from("/data/slopty")
        );
        assert_eq!(data(Layout::Xdg, &[home]), PathBuf::from("/home/me/.local/share/slopty"));
        assert_eq!(
            data(Layout::Xdg, &[home, ("XDG_DATA_HOME", "rel/data")]),
            PathBuf::from("/home/me/.local/share/slopty"),
            "a relative XDG path is ignored"
        );
        assert_eq!(
            data(Layout::Xdg, &[home, ("XDG_DATA_HOME", "/data"), ("SLOPTY_DATA_DIR", "/x")]),
            PathBuf::from("/x")
        );
        assert_eq!(
            data(Layout::Apple, &[("HOME", "/Users/me"), ("XDG_DATA_HOME", "/data")]),
            PathBuf::from("/Users/me/Library/Application Support/Slopty")
        );
        assert_eq!(data(Layout::Apple, &[("SLOPTY_DATA_DIR", "/x")]), PathBuf::from("/x"));
    }

    /// The home is an absolute path, and `$HOME` when that is one.
    #[test]
    fn the_home_is_absolute_and_the_environments() {
        assert!(home().is_absolute());
        if let Some(set) = std::env::var_os("HOME").map(PathBuf::from).filter(|h| h.is_absolute()) {
            assert_eq!(home(), set);
        }
    }

    /// Sockets go under `$XDG_RUNTIME_DIR` on Linux, else a `/tmp` name of this user's own;
    /// macOS's `$TMPDIR` is already per-user.
    #[test]
    fn sockets_go_in_a_directory_of_the_users_own() {
        let tmp = Path::new("/tmp");
        let run = |layout: Layout, pairs: &[(&str, &str)]| layout.runtime_dir(env(pairs), tmp, 501);
        assert_eq!(
            run(Layout::Xdg, &[("XDG_RUNTIME_DIR", "/run/user/501")]),
            PathBuf::from("/run/user/501/slopty")
        );
        assert_eq!(run(Layout::Xdg, &[]), PathBuf::from("/tmp/slopty-501"));
        assert_eq!(
            run(Layout::Xdg, &[("XDG_RUNTIME_DIR", "run")]),
            PathBuf::from("/tmp/slopty-501")
        );
        assert_eq!(
            run(Layout::Apple, &[("XDG_RUNTIME_DIR", "/run")]),
            PathBuf::from("/tmp/slopty")
        );
    }
}
