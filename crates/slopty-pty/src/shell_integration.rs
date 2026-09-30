//! Shell integration: bundled scripts that make the shell emit OSC 133 prompt marks.
//!
//! Injected the way terminals do it, one hook per shell: a `ZDOTDIR` bootstrap for zsh, an
//! `--rcfile` for bash, a vendor snippet on `XDG_DATA_DIRS` for fish; every bootstrap hands
//! control straight back to the user's own files. The daemon writes the scripts under its data
//! dir on every start so a running install never depends on the source tree.
//!
//! **Web pages and editors go to the client** ([`ShellIntegration::handoff_env`]). Every session,
//! shell or not, gets `BROWSER` and `EDITOR` naming Slopty's handoff commands, and a directory
//! ([`BIN`]) first on its `PATH` that holds them and an `open` (`xdg-open` on Linux) that
//! forwards web addresses and hands anything else to the system's. All of them are the `slopty`
//! CLI under another name, which it reads from `argv[0]`.
//!
//! Both variables are defaults. `VISUAL` is never set, since git, `crontab`, `less` and Claude
//! Code read it ahead of `EDITOR`, and it would beat an `EDITOR` the user's shell files export.
//! Neither is set when the daemon's own environment already names a browser or an editor.
//! Each value is the command's absolute path, one word with no space in it: Claude Code runs
//! `$BROWSER <url>` as a single program, `git` hands `$EDITOR` to `sh -c`, and a program run
//! with a `PATH` of its own (`env PATH=/usr/bin:/bin git commit`) still finds it. When the
//! scripts' directory has a space in it (macOS's Application Support), the commands live in a
//! private directory under the user's temporary directory instead ([`bin_dir`]). Before each
//! prompt the scripts put [`BIN`] back in front of the path, where macOS's `path_helper` and
//! the user's files may have moved it.

use std::path::{Path, PathBuf};
use std::{fs, io};

/// zsh bootstrap, read by zsh because `ZDOTDIR` points at its directory.
pub const ZSH_ZSHENV: &str = include_str!("../assets/shell/zsh/.zshenv");
/// The zsh hooks, sourced by the bootstrap for interactive shells.
pub const ZSH_INTEGRATION: &str = include_str!("../assets/shell/zsh/slopty-integration.zsh");
/// bash bootstrap, passed with `--rcfile`: the user's files, then the hooks.
pub const BASH_RCFILE: &str = include_str!("../assets/shell/bash/slopty.bash");
/// fish hooks, loaded from `<XDG_DATA_DIRS entry>/fish/vendor_conf.d`.
pub const FISH_INTEGRATION: &str =
    include_str!("../assets/shell/fish/fish/vendor_conf.d/slopty.fish");
/// Set (to anything but `0` or empty) to run shells untouched.
pub const OPT_OUT: &str = "SLOPTY_NO_SHELL_INTEGRATION";
/// The `slopty` CLI, which the hooks' `ssh` runs as `slopty ssh` ([`crate::ssh`]).
pub const CLI: &str = "SLOPTY_CLI";
/// The directory of Slopty's `open`, `BROWSER` and `EDITOR`, first on every session's `PATH`.
pub const BIN: &str = "SLOPTY_BIN";
/// Every session's `BROWSER`: the CLI opening a web page on the client (`slopty browse`).
pub const BROWSER_SHIM: &str = "slopty-browser";
/// Every session's `EDITOR`: the CLI editing a file on the client and waiting for the person
/// (`slopty edit --wait`).
pub const EDITOR_SHIM: &str = "slopty-editor";
/// The system's opener that [`BIN`] shadows: web addresses go to the client, anything else to
/// the system's own.
#[cfg(target_os = "macos")]
pub const OPENER: &str = "open";
/// The system's opener that [`BIN`] shadows.
#[cfg(not(target_os = "macos"))]
pub const OPENER: &str = "xdg-open";
/// Where the zsh bootstrap finds the user's original `ZDOTDIR`, when there was one.
const ZSH_ORIGINAL_ZDOTDIR: &str = "SLOPTY_ZSH_ZDOTDIR";
/// Tells the bash bootstrap the shell was asked to be a login shell (bash ignores `--rcfile`
/// for real login shells, so the daemon takes the flag and the bootstrap reads the profiles).
const BASH_LOGIN: &str = "SLOPTY_BASH_LOGIN";
/// `--noprofile` travelled to the bootstrap.
const BASH_NOPROFILE: &str = "SLOPTY_BASH_NOPROFILE";
/// `--norc` travelled to the bootstrap.
const BASH_NORC: &str = "SLOPTY_BASH_NORC";
/// fish's default when `XDG_DATA_DIRS` is unset; kept behind our entry so vendor snippets
/// installed there still load.
const XDG_DATA_DIRS_DEFAULT: &str = "/usr/local/share:/usr/share";

/// The installed scripts plus the daemon-side decisions taken once at start.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShellIntegration {
    /// The directory zsh is pointed at (holds `.zshenv` and the hooks).
    pub zdotdir: PathBuf,
    /// The bash bootstrap file.
    pub bash_rcfile: PathBuf,
    /// The `XDG_DATA_DIRS` entry holding `fish/vendor_conf.d/slopty.fish`.
    pub fish_data_dir: PathBuf,
    /// The daemon's own `ZDOTDIR`, handed back to the shell by the bootstrap.
    pub original_zdotdir: Option<String>,
    /// The daemon's own `XDG_DATA_DIRS`, kept behind ours for fish.
    pub original_xdg_data_dirs: Option<String>,
    /// `false` when the daemon's environment carries [`OPT_OUT`]: no shell is touched.
    pub enabled: bool,
    /// The `slopty` CLI beside the daemon, handed to the shell as [`CLI`]; without it `ssh`
    /// stays the plain one.
    pub cli: Option<PathBuf>,
    /// The directory of the handoff commands ([`BIN`]), when the CLI is there to be them.
    pub bin: Option<PathBuf>,
    /// The daemon's own environment names a browser (`BROWSER`) that is not Slopty's: the
    /// sessions keep it.
    pub own_browser: bool,
    /// The daemon's own environment names an editor (`EDITOR` or `VISUAL`) that is not
    /// Slopty's: the sessions keep it.
    pub own_editor: bool,
}

/// What to change about a spawn for the integration to load.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Injection {
    /// Arguments after the program, possibly rewritten (bash).
    pub args: Vec<String>,
    /// `argv[0]`, possibly rewritten (a leading dash means a login shell to bash).
    pub arg0: Option<String>,
    /// Variables to add.
    pub env: Vec<(String, String)>,
}

/// Write the scripts under `dir` (idempotent) and read the daemon's environment.
pub fn install(dir: &Path) -> io::Result<ShellIntegration> {
    let zsh = dir.join("zsh");
    fs::create_dir_all(&zsh)?;
    write_if_changed(&zsh.join(".zshenv"), ZSH_ZSHENV)?;
    write_if_changed(&zsh.join("slopty-integration.zsh"), ZSH_INTEGRATION)?;
    let bash = dir.join("bash");
    fs::create_dir_all(&bash)?;
    let bash_rcfile = bash.join("slopty.bash");
    write_if_changed(&bash_rcfile, BASH_RCFILE)?;
    let fish = dir.join("fish");
    let vendor = fish.join("fish").join("vendor_conf.d");
    fs::create_dir_all(&vendor)?;
    write_if_changed(&vendor.join("slopty.fish"), FISH_INTEGRATION)?;
    let var = |name: &str| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
    let cli = sibling_cli();
    let bin = cli.as_deref().and_then(|cli| {
        let bin = bin_dir(dir);
        link_shims(&bin, cli)
            .map_err(|e| tracing::warn!(error = %e, "handoff commands not linked"))
            .ok()?;
        Some(bin)
    });
    Ok(ShellIntegration {
        zdotdir: zsh,
        bash_rcfile,
        fish_data_dir: fish,
        original_zdotdir: var("ZDOTDIR"),
        original_xdg_data_dirs: var("XDG_DATA_DIRS"),
        enabled: !std::env::var(OPT_OUT).is_ok_and(|v| opted_out(&v)),
        cli,
        bin,
        own_browser: names_own("BROWSER"),
        own_editor: names_own("EDITOR") || names_own("VISUAL"),
    })
}

/// Whether the daemon's environment names `var` as something other than Slopty's own command.
fn names_own(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| !v.is_empty() && !is_handoff_command(&v))
}

/// Link the handoff commands in `bin` to `cli`, replacing links to anything else.
pub fn link_shims(bin: &Path, cli: &Path) -> io::Result<()> {
    fs::create_dir_all(bin)?;
    for name in [OPENER, BROWSER_SHIM, EDITOR_SHIM] {
        let link = bin.join(name);
        if fs::read_link(&link).is_ok_and(|to| to == cli) {
            continue;
        }
        match fs::remove_file(&link) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        std::os::unix::fs::symlink(cli, &link)?;
    }
    Ok(())
}

/// Where the handoff commands go for scripts under `dir`.
///
/// That is `dir/bin` when its path holds only letters, digits and `/._-`, else `slopty-<uid>/bin`
/// under the user's temporary directory (per user on macOS), made private to the user; `dir/bin`
/// again when that cannot be made safely, and the commands then go by bare name, found on `PATH`.
#[must_use]
pub fn bin_dir(dir: &Path) -> PathBuf {
    let own = dir.join("bin");
    if plain(&own) {
        return own;
    }
    let uid = rustix::process::getuid().as_raw();
    let private = std::env::temp_dir().join(format!("slopty-{uid}"));
    match make_private(&private, uid) {
        Ok(()) if plain(&private) => private.join("bin"),
        Ok(()) => own,
        Err(e) => {
            tracing::warn!(dir = %private.display(), error = %e, "no private place for the handoff commands");
            own
        }
    }
}

/// Make `dir` a directory only `uid` can reach, or check that it is one: not a link, owned by
/// `uid`, with no access for group or others. `/tmp` is everyone's, so what is found there may
/// have been planted.
fn make_private(dir: &Path, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other("not a directory private to this user"));
    }
    Ok(())
}

/// Whether `path` can stand as a command word in any shell or `PATH` unquoted: letters, digits
/// and `/._-` only.
fn plain(path: &Path) -> bool {
    path.to_str().is_some_and(|p| {
        p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
    })
}

/// Whether `value` (a `BROWSER`, `EDITOR` or `VISUAL`) names one of Slopty's handoff commands:
/// one a daemon started from a Slopty session inherited, which is no choice of the user's.
#[must_use]
pub fn is_handoff_command(value: &str) -> bool {
    Path::new(value).file_name().is_some_and(|n| n == BROWSER_SHIM || n == EDITOR_SHIM)
}

/// `slopty` beside this binary: an installed worker's bin dir carries all three
/// (`slopty_platform::service::WORKER_BINARIES`), and so does a build's target dir.
fn sibling_cli() -> Option<PathBuf> {
    let cli = std::env::current_exe().ok()?.parent()?.join("slopty");
    cli.is_file().then_some(cli)
}

fn write_if_changed(path: &Path, content: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|have| have == content) {
        return Ok(());
    }
    fs::write(path, content)
}

/// `SLOPTY_NO_SHELL_INTEGRATION=<value>` means opt out unless the value is empty or `0`.
fn opted_out(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl ShellIntegration {
    /// How to spawn `program` (a path or a bare name) with `args` and `arg0` so the marks load.
    ///
    /// Untouched (args and arg0 as given, no variables) when it is not a shell we integrate,
    /// when the shell would not be interactive (`-c`, a script), when the daemon opted out, or
    /// when `extra` (the session's own variables) carries [`OPT_OUT`].
    #[must_use]
    pub fn apply(
        &self,
        program: &str,
        args: &[String],
        arg0: Option<&str>,
        extra: &[(String, String)],
    ) -> Injection {
        let untouched =
            || Injection { args: args.to_vec(), arg0: arg0.map(str::to_owned), env: Vec::new() };
        if !self.enabled || extra.iter().any(|(k, v)| k == OPT_OUT && opted_out(v)) {
            return untouched();
        }
        let base = Path::new(program).file_name().map(|n| n.to_string_lossy().into_owned());
        let injection = match base.as_deref() {
            Some("zsh") => {
                let mut env = vec![("ZDOTDIR".to_owned(), lossy(&self.zdotdir))];
                if let Some(original) = &self.original_zdotdir {
                    env.push((ZSH_ORIGINAL_ZDOTDIR.to_owned(), original.clone()));
                }
                Some(Injection { env, ..untouched() })
            }
            Some("bash") => self.bash(args, arg0),
            Some("fish") => {
                let session = extra.iter().find(|(k, _)| k == "XDG_DATA_DIRS").map(|(_, v)| v);
                let behind = session
                    .or(self.original_xdg_data_dirs.as_ref())
                    .filter(|v| !v.is_empty())
                    .map_or(XDG_DATA_DIRS_DEFAULT, String::as_str);
                let dirs = format!("{}:{behind}", lossy(&self.fish_data_dir));
                Some(Injection { env: vec![("XDG_DATA_DIRS".to_owned(), dirs)], ..untouched() })
            }
            _ => None,
        };
        let Some(mut injection) = injection else { return untouched() };
        if let Some(cli) = &self.cli {
            injection.env.push((CLI.to_owned(), lossy(cli)));
        }
        injection
    }

    /// What every session is spawned with so its web pages and editors go to the client:
    /// [`BIN`], and `BROWSER` and `EDITOR` as the commands' absolute paths (bare names when
    /// [`BIN`] is not plain). `BROWSER` is left out when the daemon's environment
    /// ([`Self::own_browser`]) or `extra` (the session's own variables) names a browser, and
    /// `EDITOR` when either names an `EDITOR` or a `VISUAL`. Nothing without the handoff
    /// commands, when the daemon opted out, or when `extra` carries [`OPT_OUT`]. `PATH` is
    /// [`Self::handoff_path`]'s.
    #[must_use]
    pub fn handoff_env(&self, extra: &[(String, String)]) -> Vec<(String, String)> {
        let Some(bin) = self.handoff_bin(extra) else { return Vec::new() };
        let named = |name: &str| extra.iter().any(|(k, v)| k == name && !v.is_empty());
        let command =
            |name: &str| if plain(bin) { lossy(&bin.join(name)) } else { name.to_owned() };
        let mut env = vec![(BIN.to_owned(), lossy(bin))];
        if !self.own_browser && !named("BROWSER") {
            env.push(("BROWSER".to_owned(), command(BROWSER_SHIM)));
        }
        if !self.own_editor && !named("EDITOR") && !named("VISUAL") {
            env.push(("EDITOR".to_owned(), command(EDITOR_SHIM)));
        }
        env
    }

    /// The session's `PATH` with [`BIN`] in front: `extra`'s `PATH`, else `inherited` (the
    /// daemon's own). `None` when [`Self::handoff_env`] gives nothing.
    #[must_use]
    pub fn handoff_path(
        &self,
        extra: &[(String, String)],
        inherited: Option<&str>,
    ) -> Option<String> {
        let bin = lossy(self.handoff_bin(extra)?);
        let path =
            extra.iter().rev().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).or(inherited);
        let rest = path
            .into_iter()
            .flat_map(|p| p.split(':'))
            .filter(|dir| !dir.is_empty() && *dir != bin);
        Some(std::iter::once(bin.as_str()).chain(rest).collect::<Vec<_>>().join(":"))
    }

    fn handoff_bin(&self, extra: &[(String, String)]) -> Option<&Path> {
        let opted = extra.iter().any(|(k, v)| k == OPT_OUT && opted_out(v));
        self.bin.as_deref().filter(|_| self.enabled && !opted)
    }

    /// The bash rewrite: `--rcfile` replaces the user's `~/.bashrc` for interactive shells
    /// only, and bash ignores it for login shells, so `-l` / `--login` / a leading dash in
    /// `argv[0]` are taken out and handed to the bootstrap as [`BASH_LOGIN`]; `--noprofile`
    /// and `--norc` travel the same way. `None` when the shell would not be interactive.
    fn bash(&self, args: &[String], arg0: Option<&str>) -> Option<Injection> {
        // bash reads its GNU long options only at the front of argv, before the short ones.
        let mut out: Vec<String> = vec!["--rcfile".to_owned(), lossy(&self.bash_rcfile)];
        let mut login = arg0.is_some_and(|a| a.starts_with('-'));
        let mut noprofile = false;
        let mut norc = false;
        let mut options_done = false;
        for arg in args {
            if options_done || !arg.starts_with('-') || arg == "-" {
                // A script (or stdin with `-`): not an interactive shell with a prompt.
                return None;
            }
            match arg.as_str() {
                "--" => options_done = true,
                "--login" => login = true,
                "--noprofile" => noprofile = true,
                "--norc" => norc = true,
                "--rcfile" | "--init-file" | "--posix" => return None,
                long if long.starts_with("--") => out.insert(0, long.to_owned()),
                short => {
                    // A cluster such as `-lic` or `-il`; `-c` (and `-s` with a script) mean no
                    // prompt, `-l` is the login flag, the rest stays.
                    let flags: String = short.chars().skip(1).collect();
                    if flags.contains('c') || flags.contains('o') {
                        return None;
                    }
                    if flags.contains('l') {
                        login = true;
                    }
                    let kept: String = flags.chars().filter(|&f| f != 'l').collect();
                    if !kept.is_empty() {
                        out.push(format!("-{kept}"));
                    }
                }
            }
        }
        let mut env = Vec::new();
        if login {
            env.push((BASH_LOGIN.to_owned(), "1".to_owned()));
        }
        if noprofile {
            env.push((BASH_NOPROFILE.to_owned(), "1".to_owned()));
        }
        if norc {
            env.push((BASH_NORC.to_owned(), "1".to_owned()));
        }
        let arg0 = arg0.map(|a| a.trim_start_matches('-').to_owned()).filter(|a| !a.is_empty());
        Some(Injection { args: out, arg0, env })
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;

    use super::*;
    use crate::{Pty, PtyMaster, SpawnSpec};

    fn integration() -> ShellIntegration {
        ShellIntegration {
            zdotdir: PathBuf::from("/tmp/x/zsh"),
            bash_rcfile: PathBuf::from("/tmp/x/bash/slopty.bash"),
            fish_data_dir: PathBuf::from("/tmp/x/fish"),
            original_zdotdir: None,
            original_xdg_data_dirs: None,
            enabled: true,
            cli: None,
            bin: None,
            own_browser: false,
            own_editor: false,
        }
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn pair(k: &str, v: &str) -> (String, String) {
        (k.to_owned(), v.to_owned())
    }

    #[test]
    fn zsh_gets_a_zdotdir_and_other_programs_nothing() {
        let si = integration();
        let inj = si.apply("/bin/zsh", &strings(&["-i"]), None, &[]);
        assert_eq!(inj.env, vec![pair("ZDOTDIR", "/tmp/x/zsh")]);
        assert_eq!(inj.args, strings(&["-i"]), "zsh keeps its arguments");
        assert_eq!(si.apply("zsh", &[], None, &[]).env, si.apply("/bin/zsh", &[], None, &[]).env);
        let with_original =
            ShellIntegration { original_zdotdir: Some("/home/me/cfg".to_owned()), ..integration() };
        assert_eq!(
            with_original.apply("zsh", &[], None, &[]).env.get(1),
            Some(&pair("SLOPTY_ZSH_ZDOTDIR", "/home/me/cfg")),
            "the user's ZDOTDIR travels to the bootstrap"
        );
        let untouched = si.apply("claude", &strings(&["--resume"]), Some("claude"), &[]);
        assert_eq!(
            untouched,
            Injection { args: strings(&["--resume"]), arg0: Some("claude".into()), env: vec![] }
        );
    }

    #[test]
    fn bash_gets_an_rcfile_and_its_login_flag_moves_to_the_environment() {
        let si = integration();
        let rc = strings(&["--rcfile", "/tmp/x/bash/slopty.bash"]);
        // The account's login shell: `-bash` as argv[0], no arguments.
        let inj = si.apply("/bin/bash", &[], Some("-bash"), &[]);
        assert_eq!((inj.args.clone(), inj.arg0.as_deref()), (rc.clone(), Some("bash")));
        assert_eq!(inj.env, vec![pair("SLOPTY_BASH_LOGIN", "1")]);
        // An explicit interactive login shell with clustered flags.
        let inj = si.apply("bash", &strings(&["-il", "--noprofile"]), None, &[]);
        assert_eq!(inj.args, [rc.clone(), strings(&["-i"])].concat(), "long options first");
        assert_eq!(
            inj.env,
            vec![pair("SLOPTY_BASH_LOGIN", "1"), pair("SLOPTY_BASH_NOPROFILE", "1")]
        );
        let inj = si.apply("bash", &strings(&["--login", "--norc"]), None, &[]);
        assert_eq!(inj.args, rc);
        assert_eq!(inj.env, vec![pair("SLOPTY_BASH_LOGIN", "1"), pair("SLOPTY_BASH_NORC", "1")]);
        // Plain interactive: no login variable, just the rcfile.
        let inj = si.apply("/opt/homebrew/bin/bash", &strings(&["--noediting", "-i"]), None, &[]);
        assert_eq!(
            (inj.args, inj.env),
            ([strings(&["--noediting"]), rc, strings(&["-i"])].concat(), vec![])
        );
        // No prompt, no integration: `-c`, `-lic`, a script, `--posix`, a user rcfile.
        for args in [
            strings(&["-c", "true"]),
            strings(&["-lic", "claude"]),
            strings(&["script.sh"]),
            strings(&["--", "script.sh"]),
            strings(&["--posix"]),
            strings(&["--rcfile", "mine"]),
            strings(&["-"]),
        ] {
            let inj = si.apply("bash", &args, Some("-bash"), &[]);
            assert_eq!(
                (inj.args, inj.arg0.as_deref(), inj.env),
                (args.clone(), Some("-bash"), vec![]),
                "{args:?}"
            );
        }
    }

    /// Every shell the hooks load in is told where the CLI is, for its `ssh`; nothing else is.
    #[test]
    fn the_cli_travels_to_integrated_shells_only() {
        let si = ShellIntegration { cli: Some(PathBuf::from("/opt/s/slopty")), ..integration() };
        for (shell, args) in [("zsh", vec![]), ("bash", strings(&["-i"])), ("fish", vec![])] {
            let env = si.apply(shell, &args, None, &[]).env;
            assert_eq!(env.last(), Some(&pair(CLI, "/opt/s/slopty")), "{shell}: {env:?}");
        }
        assert!(si.apply("bash", &strings(&["-c", "true"]), None, &[]).env.is_empty(), "no prompt");
        assert!(si.apply("vim", &[], None, &[]).env.is_empty());
        assert!(si.apply("zsh", &[], None, &[pair(OPT_OUT, "1")]).env.is_empty(), "opted out");
    }

    #[test]
    fn fish_gets_its_vendor_dir_in_front_of_xdg_data_dirs() {
        let si = integration();
        let inj = si.apply("/opt/homebrew/bin/fish", &strings(&["-i"]), None, &[]);
        assert_eq!(inj.env, vec![pair("XDG_DATA_DIRS", "/tmp/x/fish:/usr/local/share:/usr/share")]);
        assert_eq!(inj.args, strings(&["-i"]));
        let daemon =
            ShellIntegration { original_xdg_data_dirs: Some("/opt/share".into()), ..integration() };
        assert_eq!(daemon.apply("fish", &[], None, &[]).env[0].1, "/tmp/x/fish:/opt/share");
        let session = [pair("XDG_DATA_DIRS", "/sess/share")];
        assert_eq!(
            daemon.apply("fish", &[], None, &session).env[0].1,
            "/tmp/x/fish:/sess/share",
            "the session's own wins"
        );
    }

    #[test]
    fn opt_out_from_the_daemon_or_the_session() {
        let si = integration();
        let out = [pair(OPT_OUT, "1")];
        for shell in ["/bin/zsh", "/bin/bash", "fish"] {
            assert!(si.apply(shell, &[], None, &out).env.is_empty(), "session opt-out for {shell}");
            let daemon_out = ShellIntegration { enabled: false, ..integration() };
            assert!(
                daemon_out.apply(shell, &[], None, &[]).env.is_empty(),
                "daemon opt-out for {shell}"
            );
        }
        assert_eq!(
            si.apply("bash", &[], Some("-bash"), &out).arg0.as_deref(),
            Some("-bash"),
            "argv untouched too"
        );
        let not_out = [pair(OPT_OUT, "0")];
        assert!(!si.apply("/bin/zsh", &[], None, &not_out).env.is_empty(), "`0` is not an opt-out");
        let empty = [pair(OPT_OUT, "")];
        assert!(!si.apply("/bin/zsh", &[], None, &empty).env.is_empty(), "nor is an empty value");
        assert!(opted_out("1") && opted_out("yes") && !opted_out("0") && !opted_out(""));
    }

    #[test]
    fn install_writes_the_bundled_scripts_and_is_idempotent() {
        let tmp = std::env::temp_dir().join(format!("slopty-shell-install-{}", std::process::id()));
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
        let si = install(&tmp).unwrap();
        assert_eq!(si.zdotdir, tmp.join("zsh"));
        assert_eq!(fs::read_to_string(si.zdotdir.join(".zshenv")).unwrap(), ZSH_ZSHENV);
        let hooks = si.zdotdir.join("slopty-integration.zsh");
        assert_eq!(fs::read_to_string(&hooks).unwrap(), ZSH_INTEGRATION);
        assert_eq!(fs::read_to_string(&si.bash_rcfile).unwrap(), BASH_RCFILE);
        let fish = si.fish_data_dir.join("fish").join("vendor_conf.d").join("slopty.fish");
        assert_eq!(fs::read_to_string(&fish).unwrap(), FISH_INTEGRATION);
        // A stale or edited file is rewritten on the next start.
        fs::write(&hooks, "# edited\n").unwrap();
        fs::write(&fish, "# edited\n").unwrap();
        assert_eq!(install(&tmp).unwrap(), si);
        assert_eq!(fs::read_to_string(&hooks).unwrap(), ZSH_INTEGRATION);
        assert_eq!(fs::read_to_string(&fish).unwrap(), FISH_INTEGRATION);
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
    }

    /// Run `program args` on a PTY with `HOME` set to a fresh directory holding `rc_files`,
    /// type `input`, and return everything the shell wrote.
    async fn run_shell(
        tag: &str,
        program: &str,
        args: &[&str],
        arg0: Option<&str>,
        rc_files: &[(&str, &str)],
        input: &str,
    ) -> String {
        run_shell_with(tag, Shell { program, args, arg0 }, rc_files, &[], input).await
    }

    /// A shell to start: the program, its arguments and its `argv[0]`.
    #[derive(Clone, Copy)]
    struct Shell<'a> {
        program: &'a str,
        args: &'a [&'a str],
        arg0: Option<&'a str>,
    }

    /// [`run_shell`], with `extra` variables in the session's environment, last.
    async fn run_shell_with(
        tag: &str,
        shell: Shell<'_>,
        rc_files: &[(&str, &str)],
        extra: &[(&str, &str)],
        input: &str,
    ) -> String {
        run_shell_in(tag, shell, rc_files, extra, input, None).await
    }

    /// [`run_shell_with`], with the handoff commands in `bin`.
    async fn run_shell_in(
        tag: &str,
        shell: Shell<'_>,
        rc_files: &[(&str, &str)],
        extra: &[(&str, &str)],
        input: &str,
        bin: Option<&Path>,
    ) -> String {
        let Shell { program, args, arg0 } = shell;
        let tmp = std::env::temp_dir().join(format!("slopty-shell-{tag}-{}", std::process::id()));
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
        let si = install(&tmp.join("shell")).unwrap();
        let si = ShellIntegration {
            original_zdotdir: None,
            original_xdg_data_dirs: None,
            enabled: true,
            cli: None,
            bin: bin.map(Path::to_path_buf),
            own_browser: false,
            own_editor: false,
            ..si
        };
        let home = tmp.join("home");
        fs::create_dir_all(&home).unwrap();
        for (name, content) in rc_files {
            let path = home.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        let terminfo = tmp.join("terminfo");
        compile_terminfo(&terminfo).await;
        let size = TermSize { cols: 60, rows: 10, metrics: CellMetrics::default() };
        let pty = Pty::open(size).unwrap();
        let injection = si.apply(program, &strings(args), arg0, &[]);
        // A daemon's PATH: none of the developer's Homebrew hooks (mise, direnv) in the way.
        // The terminal ptyd gives a shell once its database is compiled, whether or not this
        // machine has the entry: a TERM the shell cannot look up leaves readline on a dumb
        // terminal, which drops the prompt's marks from a prompt wider than the tile. The
        // `sudo` wrapper is defined only with a TERMINFO.
        let mut env = vec![
            pair("HOME", &home.to_string_lossy()),
            pair("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            pair("TERM", crate::terminfo::NAMES[0]),
            pair("TERMINFO", &terminfo.to_string_lossy()),
        ];
        env.extend(injection.env);
        env.extend(extra.iter().map(|(k, v)| pair(k, v)));
        // Spawn exactly the way ptyd does, through `spawn_with`, so the rewrite is the real one.
        let spec = SpawnSpec {
            command: [vec![program.to_owned()], strings(args)].concat(),
            cwd: Some(home.clone()),
            env,
            size,
        };
        let mut child = pty.spawn_with(&spec, Some(&si)).unwrap().child;
        let master = PtyMaster::new(pty.into_master()).unwrap();
        master.write_all(input.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let mut buf = [0_u8; 4096];
        let mut answered = 0;
        let deadline =
            tokio::time::Instant::now().checked_add(std::time::Duration::from_secs(20)).unwrap();
        loop {
            let read = tokio::time::timeout_at(deadline, master.read(&mut buf)).await;
            let Ok(Ok(n)) = read else {
                // EOF, an error, or a shell that never exited: what it wrote is the evidence.
                let _killed: io::Result<()> = child.start_kill();
                break;
            };
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            // fish 4 asks the terminal who it is (DA1, cursor position) and waits up to 10 s
            // for the answer before drawing a prompt; a real terminal replies at once.
            let asked = out.windows(4).filter(|w| *w == b"\x1b[0c" || *w == b"\x1b[6n").count();
            for _ in answered..asked {
                master.write_all(b"\x1b[?62;22c\x1b[1;1R").await.unwrap();
            }
            answered = asked;
        }
        let _status: io::Result<std::process::ExitStatus> = child.wait().await;
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Our terminfo entry, compiled into `database` by the `tic` ptyd runs.
    async fn compile_terminfo(database: &Path) {
        use tokio::io::AsyncWriteExt as _;
        let mut tic = tokio::process::Command::new("/usr/bin/tic")
            .args(["-x", "-o"])
            .arg(database)
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = tic.stdin.take().unwrap();
        stdin.write_all(crate::terminfo::source().as_bytes()).await.unwrap();
        drop(stdin);
        let out = tic.wait_with_output().await.unwrap();
        assert!(out.status.success(), "tic: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// `OSC 133;<mark>` with either terminator, `mark` possibly followed by parameters.
    fn has_mark(text: &str, mark: &str) -> bool {
        text.split("\x1b]133;").skip(1).any(|rest| {
            rest.strip_prefix(mark).is_some_and(|after| after.starts_with(['\x07', '\x1b', ';']))
        })
    }

    fn assert_marks(text: &str, shell: &str) {
        assert!(has_mark(text, "A"), "{shell} prompt mark: {text:?}");
        assert!(has_mark(text, "B"), "{shell} input mark: {text:?}");
        assert!(has_mark(text, "C"), "{shell} output mark: {text:?}");
        assert!(has_mark(text, "D;1"), "{shell} status of `false`: {text:?}");
    }

    /// The shell reported the directory it moved into, `a b%` under its `HOME`, as OSC 7 with
    /// the space and the percent sign encoded.
    fn assert_cwd(text: &str, shell: &str) {
        let reported = text
            .split("\x1b]7;file://")
            .skip(1)
            .any(|rest| rest.split_once('\x07').is_some_and(|(url, _)| url.ends_with("/a%20b%25")));
        assert!(reported, "{shell} reports its directory: {text:?}");
    }

    /// `sudo` is the wrapper that keeps TERMINFO (the shells print `function` for it).
    fn assert_sudo_wrapped(text: &str, shell: &str) {
        assert!(text.contains("sudo=function"), "{shell} wraps sudo: {text:?}");
    }

    #[tokio::test]
    async fn an_interactive_zsh_emits_prompt_marks() {
        // The user's own .zshenv still runs, from the real ZDOTDIR (here: HOME).
        let text = run_shell(
            "zsh",
            "/bin/zsh",
            &["-i"],
            None,
            &[(".zshenv", "export SLOPTY_TEST_ZSHENV=ran\n")],
            // Two lines: the status of the first is reported by the precmd before the second.
            "mkdir -p ~/'a b%' && cd ~/'a b%'\necho zdotdir=$ZDOTDIR env=$SLOPTY_TEST_ZSHENV sudo=$(whence -w sudo); false\nexit\n",
        )
        .await;
        assert_marks(&text, "zsh");
        assert!(has_mark(&text, "A;redraw=1"), "zle redraws the whole prompt: {text:?}");
        assert_cwd(&text, "zsh");
        assert!(
            text.contains("zdotdir= env=ran sudo=sudo: function"),
            "ZDOTDIR handed back, user .zshenv ran, sudo wrapped: {text:?}"
        );
        // The cursor is a blinking bar while zle reads a line (the `main` keymap) and the
        // program's shape again just before the command runs, ahead of its output mark.
        assert!(text.contains("\x1b[5 q"), "a bar at the prompt: {text:?}");
        assert!(text.contains("\x1b[0 q\x1b]133;C"), "reset before the command: {text:?}");
    }

    /// A theme whose precmd rebuilds PS1 after ours has wrapped it (the first prompt, before
    /// our hook has moved itself last) loses the marks in PS1; zle's line-init then marks the
    /// prompt in place (`133;P;k=i` and `133;B`). The next prompt has them in PS1 again.
    #[tokio::test]
    async fn a_theme_that_rebuilds_ps1_still_gets_its_prompt_marked() {
        let text = run_shell(
            "zsh-theme",
            "/bin/zsh",
            &["-i"],
            None,
            &[(".zshrc", "_theme_precmd() { PS1='%% ' }\nprecmd_functions+=(_theme_precmd)\n")],
            "false\nexit\n",
        )
        .await;
        assert!(has_mark(&text, "P;k=i"), "the first prompt marked in place: {text:?}");
        assert!(has_mark(&text, "B"), "and its input: {text:?}");
        assert!(has_mark(&text, "A"), "the second prompt marked in PS1: {text:?}");
        assert!(has_mark(&text, "D;1"), "status of `false`: {text:?}");
        let (first, rest) = text.split_once("\x1b]133;P;k=i").unwrap();
        assert!(!first.contains("\x1b]133;A"), "PS1 had no marks before the fallback: {first:?}");
        assert!(rest.contains("\x1b]133;A"), "PS1 has them after: {rest:?}");
    }

    /// Every bash on this Mac: Apple's 3.2 and Homebrew's, when installed.
    fn bashes() -> Vec<&'static str> {
        ["/bin/bash", "/opt/homebrew/bin/bash"]
            .into_iter()
            .filter(|p| Path::new(p).is_file())
            .collect()
    }

    #[tokio::test]
    async fn an_interactive_bash_emits_prompt_marks_and_runs_the_users_bashrc() {
        for bash in bashes() {
            let text = run_shell(
                "bash",
                bash,
                &["-i"],
                None,
                &[(".bashrc", "export SLOPTY_TEST_BASHRC=ran\n"), (".bash_profile", "export SLOPTY_TEST_PROFILE=ran\n")],
                "mkdir -p ~/'a b%' && cd ~/'a b%'\necho rc=$SLOPTY_TEST_BASHRC profile=$SLOPTY_TEST_PROFILE login=$SLOPTY_BASH_LOGIN sudo=$(type -t sudo); false\nexit\n",
            )
            .await;
            assert_marks(&text, bash);
            assert!(has_mark(&text, "A;redraw=last"), "{bash} redraws a row: {text:?}");
            assert_cwd(&text, bash);
            assert!(
                text.contains("rc=ran profile= login= sudo=function"),
                "{bash}: .bashrc only, env clean, sudo wrapped: {text:?}"
            );
            assert_sudo_wrapped(&text, bash);
        }
    }

    /// The profile's prompt is wider than the tile, as macOS's `\h:\W \u\$ ` is on a Mac with
    /// a long host name (a hosted runner's is 60 characters): it wraps and keeps its marks.
    #[tokio::test]
    async fn a_login_bash_reads_its_profile_and_still_marks() {
        let profile =
            format!("export SLOPTY_TEST_PROFILE=ran\nPS1='{}:\\W \\$ '\n", "h".repeat(70));
        for bash in bashes() {
            let text = run_shell(
                "bash-login",
                bash,
                &[],
                Some("-bash"),
                &[(".bashrc", "export SLOPTY_TEST_BASHRC=ran\n"), (".bash_profile", &profile)],
                "echo rc=$SLOPTY_TEST_BASHRC profile=$SLOPTY_TEST_PROFILE login=$SLOPTY_BASH_LOGIN; false\nexit\n",
            )
            .await;
            assert_marks(&text, bash);
            assert!(
                text.contains("rc= profile=ran login="),
                "{bash}: profile only, env clean: {text:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_interactive_fish_emits_prompt_marks_and_runs_the_users_config() {
        let Some(fish) = ["/opt/homebrew/bin/fish", "/usr/local/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
        else {
            slopty_testkit::live::skip("fish is not installed (brew install fish)");
            return;
        };
        let text = run_shell(
            "fish",
            fish,
            &["-i"],
            None,
            &[(".config/fish/config.fish", "set -gx SLOPTY_TEST_FISH ran\n")],
            "mkdir -p ~/'a b%'; and cd ~/'a b%'\necho cfg=$SLOPTY_TEST_FISH loaded=$__slopty_integrated sudo=(type -t sudo); false\nexit\n",
        )
        .await;
        assert_marks(&text, "fish");
        assert!(has_mark(&text, "A;redraw=1"), "fish redraws the whole prompt: {text:?}");
        assert_cwd(&text, "fish");
        // fish 4 marks on its own and the snippet only records that it loaded; on fish 3 it
        // wraps the prompt.
        assert!(
            text.contains("cfg=ran loaded=1 sudo=function")
                || text.contains("cfg=ran loaded=wrapped sudo=function"),
            "user config.fish ran, the vendor snippet loaded, sudo wrapped: {text:?}"
        );
        assert_sudo_wrapped(&text, "fish");
    }

    /// A `claude` typed in a Slopty shell loads the mod the worker named, in the flag's `=`
    /// form and with function hooks on; not twice, not when opted out, and not over the user's
    /// own `claude`.
    #[tokio::test]
    async fn a_typed_claude_loads_the_mod_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (bin, claude_mod) = (tmp.path().join("bin"), tmp.path().join("mod"));
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&claude_mod).unwrap();
        let fake = bin.join("claude");
        let script = "#!/bin/sh\nprintf 'got[%s]hooks[%s]\\n' \"$*\" \"${CLAUDE_CODE_ENABLE_FUNCTION_HOOKS-}\"\n";
        fs::write(&fake, script).unwrap();
        fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let module = claude_mod.to_string_lossy().into_owned();
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let env = [("SLOPTY_CLAUDE_MOD", module.as_str()), ("PATH", path.as_str())];
        let input = format!(
            "claude -p hi\nclaude --plugin-dir={module} x\nSLOPTY_NO_CLAUDE_MOD=1 claude y\nexit\n"
        );
        let mut shells: Vec<(&str, &str, &str)> =
            vec![("/bin/zsh", ".zshrc", "alias claude='command claude --mine'\n")];
        shells.extend(
            bashes().into_iter().map(|b| (b, ".bashrc", "alias claude='command claude --mine'\n")),
        );
        let fish = ["/opt/homebrew/bin/fish", "/usr/local/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file());
        shells.extend(
            fish.map(|f| (f, ".config/fish/config.fish", "alias claude 'command claude --mine'\n")),
        );
        for (n, (shell, rc, alias)) in shells.into_iter().enumerate() {
            let tag = format!("claude-{n}");
            let interactive = || Shell { program: shell, args: &["-i"], arg0: None };
            let text = run_shell_with(&tag, interactive(), &[], &env, &input).await;
            assert!(
                text.contains(&format!("got[--plugin-dir={module} -p hi]hooks[1]")),
                "{shell}: {text:?}"
            );
            assert!(
                text.contains(&format!("got[--plugin-dir={module} x]hooks[]")),
                "{shell}: {text:?}"
            );
            assert!(text.contains("got[y]hooks[]"), "{shell}, opted out: {text:?}");
            let tag = format!("claude-own-{n}");
            let own =
                run_shell_with(&tag, interactive(), &[(rc, alias)], &env, "claude z\nexit\n").await;
            assert!(own.contains("got[--mine z]hooks[]"), "{shell}, the user's alias: {own:?}");
        }
    }

    /// A typed `ssh` goes through `slopty ssh --` with its arguments as typed, in every shell;
    /// the user's own `ssh` is left alone.
    #[tokio::test]
    async fn a_typed_ssh_goes_through_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let cli = tmp.path().join("slopty");
        fs::write(&cli, "#!/bin/sh\nprintf 'cli[%s]\\n' \"$*\"\n").unwrap();
        fs::set_permissions(&cli, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let cli = cli.to_string_lossy().into_owned();
        let env = [(CLI, cli.as_str())];
        let mut shells: Vec<(&str, &str, &str)> =
            vec![("/bin/zsh", ".zshrc", "alias ssh='echo mine'\n")];
        shells.extend(bashes().into_iter().map(|b| (b, ".bashrc", "alias ssh='echo mine'\n")));
        let fish = ["/opt/homebrew/bin/fish", "/usr/local/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file());
        shells.extend(fish.map(|f| (f, ".config/fish/config.fish", "alias ssh 'echo mine'\n")));
        for (n, (shell, rc, alias)) in shells.into_iter().enumerate() {
            let interactive = || Shell { program: shell, args: &["-i"], arg0: None };
            let input = "ssh -p 2222 'a b' -t htop\nexit\n";
            let text = run_shell_with(&format!("ssh-{n}"), interactive(), &[], &env, input).await;
            assert!(text.contains("cli[ssh -- -p 2222 a b -t htop]"), "{shell}: {text:?}");
            let own =
                run_shell_with(&format!("ssh-own-{n}"), interactive(), &[(rc, alias)], &env, input)
                    .await;
            assert!(own.contains("mine -p 2222 a b -t htop"), "{shell}, the user's alias: {own:?}");
            assert!(!own.contains("cli["), "{shell}: {own:?}");
        }
    }

    /// Every session, shell or not, gets the handoff commands as its browser and editor and
    /// their directory first on its path, once; its own variables win, and opting out leaves
    /// everything alone.
    #[test]
    fn every_session_gets_the_handoff_commands() {
        let si = ShellIntegration { bin: Some(PathBuf::from("/d/bin")), ..integration() };
        assert_eq!(
            si.handoff_env(&[]),
            vec![
                pair(BIN, "/d/bin"),
                pair("BROWSER", "/d/bin/slopty-browser"),
                pair("EDITOR", "/d/bin/slopty-editor"),
            ],
            "absolute, and no VISUAL"
        );
        let names =
            |env: Vec<(String, String)>| env.into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        let editor = ShellIntegration { own_editor: true, ..si.clone() };
        assert_eq!(names(editor.handoff_env(&[])), [BIN, "BROWSER"], "the daemon's editor");
        let browser = ShellIntegration { own_browser: true, ..si.clone() };
        assert_eq!(names(browser.handoff_env(&[])), [BIN, "EDITOR"], "the daemon's browser");
        assert_eq!(
            names(si.handoff_env(&[pair("VISUAL", "code -w")])),
            [BIN, "BROWSER"],
            "the session's own"
        );
        assert!(is_handoff_command("/x/bin/slopty-editor") && is_handoff_command("slopty-browser"));
        assert!(!is_handoff_command("nvim"));
        let spaced = ShellIntegration { bin: Some(PathBuf::from("/A S/bin")), ..integration() };
        assert_eq!(
            spaced.handoff_env(&[]).get(2),
            Some(&pair("EDITOR", EDITOR_SHIM)),
            "a bare name rather than a path the shell would split"
        );
        assert_eq!(
            si.handoff_path(&[], Some("/usr/bin:/d/bin:/bin")).as_deref(),
            Some("/d/bin:/usr/bin:/bin")
        );
        let own = [pair("PATH", "/opt/x/bin")];
        assert_eq!(si.handoff_path(&own, Some("/usr/bin")).as_deref(), Some("/d/bin:/opt/x/bin"));
        assert_eq!(si.handoff_path(&[], None).as_deref(), Some("/d/bin"));
        assert!(si.handoff_env(&[pair(OPT_OUT, "1")]).is_empty());
        assert_eq!(si.handoff_path(&[pair(OPT_OUT, "1")], Some("/usr/bin")), None);
        assert!(ShellIntegration { enabled: false, ..si }.handoff_env(&[]).is_empty());
        assert!(integration().handoff_env(&[]).is_empty(), "no CLI, no commands");
    }

    /// The commands live where their path needs no quoting: beside the scripts when that is
    /// plain, else in a directory private to the user under the temporary directory.
    #[test]
    fn the_handoff_commands_live_where_no_space_splits_them() {
        let tmp = tempfile::tempdir().unwrap();
        let plain_dir = tmp.path().join("shell");
        assert_eq!(bin_dir(&plain_dir), plain_dir.join("bin"));
        let spaced = tmp.path().join("Application Support").join("shell");
        let bin = bin_dir(&spaced);
        assert!(plain(&bin), "{}", bin.display());
        let uid = rustix::process::getuid().as_raw();
        assert!(bin.ends_with(format!("slopty-{uid}/bin")), "{}", bin.display());
        let private = bin.parent().unwrap();
        let mode = std::os::unix::fs::MetadataExt::mode(&fs::metadata(private).unwrap());
        assert_eq!(mode & 0o077, 0, "only the user reaches it");

        let planted = tmp.path().join("planted");
        fs::create_dir_all(&planted).unwrap();
        fs::set_permissions(&planted, std::os::unix::fs::PermissionsExt::from_mode(0o777)).unwrap();
        assert!(make_private(&planted, uid).is_err(), "a directory others can write is refused");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(private, &link).unwrap();
        assert!(make_private(&link, uid).is_err(), "and so is a link");
    }

    /// The commands are links to the CLI, made once and remade when they point elsewhere.
    #[test]
    fn the_handoff_commands_link_to_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let (cli, other, bin) =
            (tmp.path().join("slopty"), tmp.path().join("old"), tmp.path().join("bin"));
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&other, bin.join(OPENER)).unwrap();
        link_shims(&bin, &cli).unwrap();
        link_shims(&bin, &cli).unwrap();
        for name in [OPENER, BROWSER_SHIM, EDITOR_SHIM] {
            assert_eq!(fs::read_link(bin.join(name)).unwrap(), cli, "{name}");
        }
    }

    /// A shell whose files put the system's directories first (macOS's `path_helper` does, and
    /// many `.zshrc` files) finds the handoff `open` first again by its prompt, in zsh, bash
    /// and fish. The session's browser and editor are the handoff commands by absolute path,
    /// `VISUAL` is not set, and an `EDITOR` or a `VISUAL` the user's files export is the one
    /// every program reads (`${VISUAL:-$EDITOR}`, as git and crontab do).
    #[tokio::test]
    async fn the_handoff_commands_come_first_and_yield_to_the_users_editor() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        link_shims(&bin, Path::new("/bin/echo")).unwrap();
        let at = |name: &str| bin.join(name).to_string_lossy().into_owned();
        let posix =
            "command -v open; echo \"e=${VISUAL:-$EDITOR} b=$BROWSER v=${VISUAL-}\"\nexit\n";
        let fish_input = "command -v open; if set -q VISUAL; echo \"e=$VISUAL b=$BROWSER v=$VISUAL\"; else; echo \"e=$EDITOR b=$BROWSER v=\"; end\nexit\n";
        let mut shells: Vec<(&str, &str, &str, &str)> =
            vec![("/bin/zsh", ".zshrc", "export", posix)];
        shells.extend(bashes().into_iter().map(|b| (b, ".bashrc", "export", posix)));
        if let Some(fish) = ["/opt/homebrew/bin/fish", "/usr/local/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
        {
            shells.push((fish, ".config/fish/config.fish", "set -gx", fish_input));
        } else {
            slopty_testkit::live::skip("fish is not installed (brew install fish)");
        }
        for (n, (shell, rc, set, input)) in shells.into_iter().enumerate() {
            let (assign, path) = if set == "export" {
                ("export {}={}", "export PATH=/usr/bin:/bin")
            } else {
                ("set -gx {} {}", "set -gx PATH /usr/bin /bin")
            };
            let exported = |name: &str| assign.replacen("{}", name, 1).replacen("{}", "nvim", 1);
            for (case, line, want) in [
                ("none", String::new(), format!("e={} b={} v=", at(EDITOR_SHIM), at(BROWSER_SHIM))),
                ("editor", exported("EDITOR"), format!("e=nvim b={} v=", at(BROWSER_SHIM))),
                ("visual", exported("VISUAL"), format!("e=nvim b={} v=nvim", at(BROWSER_SHIM))),
            ] {
                let rc_text = format!("{path}\n{line}\necho rc-ran\n");
                let rc_files = [(rc, rc_text.as_str())];
                let interactive = Shell { program: shell, args: &["-i"], arg0: None };
                let tag = format!("bin-{n}-{case}");
                let text = run_shell_in(&tag, interactive, &rc_files, &[], input, Some(&bin)).await;
                assert!(text.contains("rc-ran"), "{shell} {case}: {text:?}");
                assert!(text.contains(&at(OPENER)), "{shell} {case}: open first: {text:?}");
                assert!(text.contains(&want), "{shell} {case}: want {want:?} in {text:?}");
            }
        }
    }

    /// A shell in a tmux pane drops the presence file it inherited, since the tmux server may
    /// have started in another session; outside tmux it keeps it.
    #[tokio::test]
    async fn a_tmux_pane_drops_the_inherited_presence_file() {
        let presence = ("CLAUDE_CLIENT_PRESENCE_FILE", "/p/session");
        let input = "echo \"p=[$CLAUDE_CLIENT_PRESENCE_FILE]\"\nexit\n";
        let mut shells: Vec<&str> = vec!["/bin/zsh"];
        shells.extend(bashes());
        if let Some(fish) = ["/opt/homebrew/bin/fish", "/usr/local/bin/fish"]
            .into_iter()
            .find(|p| Path::new(p).is_file())
        {
            shells.push(fish);
        }
        for (n, shell) in shells.into_iter().enumerate() {
            let interactive = Shell { program: shell, args: &["-i"], arg0: None };
            let tmux = [presence, ("TMUX", "/tmp/tmux-501/default,1,0")];
            let text = run_shell_with(&format!("tmux-{n}"), interactive, &[], &tmux, input).await;
            assert!(text.contains("p=[]"), "{shell} in tmux: {text:?}");
            let text =
                run_shell_with(&format!("tile-{n}"), interactive, &[], &[presence], input).await;
            assert!(text.contains("p=[/p/session]"), "{shell} in a tile: {text:?}");
        }
    }
}
