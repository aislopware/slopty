//! Shell integration: bundled scripts that make the shell emit OSC 133 prompt marks.
//!
//! Injected the way terminals do it, one hook per shell: a `ZDOTDIR` bootstrap for zsh, an
//! `--rcfile` for bash, a vendor snippet on `XDG_DATA_DIRS` for fish; every bootstrap hands
//! control straight back to the user's own files. The daemon writes the scripts under its data
//! dir on every start so a running install never depends on the source tree.

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
    Ok(ShellIntegration {
        zdotdir: zsh,
        bash_rcfile,
        fish_data_dir: fish,
        original_zdotdir: var("ZDOTDIR"),
        original_xdg_data_dirs: var("XDG_DATA_DIRS"),
        enabled: !std::env::var(OPT_OUT).is_ok_and(|v| opted_out(&v)),
    })
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
        match base.as_deref() {
            Some("zsh") => {
                let mut env = vec![("ZDOTDIR".to_owned(), lossy(&self.zdotdir))];
                if let Some(original) = &self.original_zdotdir {
                    env.push((ZSH_ORIGINAL_ZDOTDIR.to_owned(), original.clone()));
                }
                Injection { env, ..untouched() }
            }
            Some("bash") => self.bash(args, arg0).unwrap_or_else(untouched),
            Some("fish") => {
                let session = extra.iter().find(|(k, _)| k == "XDG_DATA_DIRS").map(|(_, v)| v);
                let behind = session
                    .or(self.original_xdg_data_dirs.as_ref())
                    .filter(|v| !v.is_empty())
                    .map_or(XDG_DATA_DIRS_DEFAULT, String::as_str);
                let dirs = format!("{}:{behind}", lossy(&self.fish_data_dir));
                Injection { env: vec![("XDG_DATA_DIRS".to_owned(), dirs)], ..untouched() }
            }
            _ => untouched(),
        }
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
        let tmp = std::env::temp_dir().join(format!("slopty-shell-{tag}-{}", std::process::id()));
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
        let si = install(&tmp.join("shell")).unwrap();
        let si = ShellIntegration {
            original_zdotdir: None,
            original_xdg_data_dirs: None,
            enabled: true,
            ..si
        };
        let home = tmp.join("home");
        fs::create_dir_all(&home).unwrap();
        for (name, content) in rc_files {
            let path = home.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        let size = TermSize { cols: 60, rows: 10, metrics: CellMetrics::default() };
        let pty = Pty::open(size).unwrap();
        let injection = si.apply(program, &strings(args), arg0, &[]);
        // A daemon's PATH: none of the developer's Homebrew hooks (mise, direnv) in the way.
        let mut env = vec![
            pair("HOME", &home.to_string_lossy()),
            pair("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        ];
        env.extend(injection.env);
        // Spawn exactly the way ptyd does, through `spawn_with`, so the rewrite is the real one.
        let spec = SpawnSpec {
            command: [vec![program.to_owned()], strings(args)].concat(),
            cwd: Some(home.clone()),
            env,
            size,
        };
        let mut child = pty.spawn_with(&spec, Some(&si)).unwrap();
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
            "echo zdotdir=$ZDOTDIR env=$SLOPTY_TEST_ZSHENV; false\nexit\n",
        )
        .await;
        assert_marks(&text, "zsh");
        assert!(
            text.contains("zdotdir= env=ran"),
            "ZDOTDIR handed back, user .zshenv ran: {text:?}"
        );
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
                "echo rc=$SLOPTY_TEST_BASHRC profile=$SLOPTY_TEST_PROFILE login=$SLOPTY_BASH_LOGIN; false\nexit\n",
            )
            .await;
            assert_marks(&text, bash);
            assert!(
                text.contains("rc=ran profile= login="),
                "{bash}: .bashrc only, env clean: {text:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_login_bash_reads_its_profile_and_still_marks() {
        for bash in bashes() {
            let text = run_shell(
                "bash-login",
                bash,
                &[],
                Some("-bash"),
                &[(".bashrc", "export SLOPTY_TEST_BASHRC=ran\n"), (".bash_profile", "export SLOPTY_TEST_PROFILE=ran\n")],
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
            eprintln!("SKIP: fish is not installed (brew install fish)");
            return;
        };
        let text = run_shell(
            "fish",
            fish,
            &["-i"],
            None,
            &[(".config/fish/config.fish", "set -gx SLOPTY_TEST_FISH ran\n")],
            "echo cfg=$SLOPTY_TEST_FISH loaded=$__slopty_integrated; false\nexit\n",
        )
        .await;
        assert_marks(&text, "fish");
        // fish 4 marks on its own and the snippet only records that it loaded; on fish 3 it
        // wraps the prompt.
        assert!(
            text.contains("cfg=ran loaded=1") || text.contains("cfg=ran loaded=wrapped"),
            "user config.fish ran, the vendor snippet loaded: {text:?}"
        );
    }
}
