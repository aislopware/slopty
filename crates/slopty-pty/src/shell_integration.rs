//! Shell integration: bundled scripts that make the shell emit OSC 133 prompt marks.
//!
//! Injected the way terminals do it: a `ZDOTDIR` bootstrap for zsh that hands control straight
//! back to the user's own files. zsh only for now; the daemon writes the scripts under its data
//! dir on every start so a running install never depends on the source tree.

use std::path::{Path, PathBuf};
use std::{fs, io};

/// zsh bootstrap, read by zsh because `ZDOTDIR` points at its directory.
pub const ZSH_ZSHENV: &str = include_str!("../assets/shell/zsh/.zshenv");
/// The hooks, sourced by the bootstrap for interactive shells.
pub const ZSH_INTEGRATION: &str = include_str!("../assets/shell/zsh/slopty-integration.zsh");
/// Set (to anything but `0` or empty) to run shells untouched.
pub const OPT_OUT: &str = "SLOPTY_NO_SHELL_INTEGRATION";
/// Where the bootstrap finds the user's original `ZDOTDIR`, when there was one.
const ZSH_ORIGINAL_ZDOTDIR: &str = "SLOPTY_ZSH_ZDOTDIR";

/// The installed scripts plus the daemon-side decisions taken once at start.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShellIntegration {
    /// The directory zsh is pointed at (holds `.zshenv` and the hooks).
    pub zdotdir: PathBuf,
    /// The daemon's own `ZDOTDIR`, handed back to the shell by the bootstrap.
    pub original_zdotdir: Option<String>,
    /// `false` when the daemon's environment carries [`OPT_OUT`]: no shell is touched.
    pub enabled: bool,
}

/// Write the scripts under `dir` (idempotent) and read the daemon's environment.
pub fn install(dir: &Path) -> io::Result<ShellIntegration> {
    let zsh = dir.join("zsh");
    fs::create_dir_all(&zsh)?;
    write_if_changed(&zsh.join(".zshenv"), ZSH_ZSHENV)?;
    write_if_changed(&zsh.join("slopty-integration.zsh"), ZSH_INTEGRATION)?;
    Ok(ShellIntegration {
        zdotdir: zsh,
        original_zdotdir: std::env::var_os("ZDOTDIR").map(|v| v.to_string_lossy().into_owned()),
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

impl ShellIntegration {
    /// The variables that switch integration on for `program` (a path or a bare name).
    ///
    /// Empty when it is not a shell we integrate, when the daemon opted out, or when `extra`
    /// (the session's own variables) carries [`OPT_OUT`].
    #[must_use]
    pub fn env_for(&self, program: &str, extra: &[(String, String)]) -> Vec<(String, String)> {
        if !self.enabled || extra.iter().any(|(k, v)| k == OPT_OUT && opted_out(v)) {
            return Vec::new();
        }
        let base = Path::new(program).file_name().map(|n| n.to_string_lossy().into_owned());
        match base.as_deref() {
            Some("zsh") => {
                let mut out =
                    vec![("ZDOTDIR".to_owned(), self.zdotdir.to_string_lossy().into_owned())];
                if let Some(original) = &self.original_zdotdir {
                    out.push((ZSH_ORIGINAL_ZDOTDIR.to_owned(), original.clone()));
                }
                out
            }
            _ => Vec::new(),
        }
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
            original_zdotdir: None,
            enabled: true,
        }
    }

    #[test]
    fn only_zsh_gets_a_zdotdir() {
        let si = integration();
        assert_eq!(
            si.env_for("/bin/zsh", &[]),
            vec![("ZDOTDIR".to_owned(), "/tmp/x/zsh".to_owned())]
        );
        assert_eq!(si.env_for("zsh", &[]), si.env_for("/bin/zsh", &[]));
        assert!(si.env_for("/bin/bash", &[]).is_empty());
        assert!(si.env_for("claude", &[]).is_empty());
        let with_original =
            ShellIntegration { original_zdotdir: Some("/home/me/cfg".to_owned()), ..integration() };
        assert_eq!(
            with_original.env_for("zsh", &[]).get(1),
            Some(&("SLOPTY_ZSH_ZDOTDIR".to_owned(), "/home/me/cfg".to_owned())),
            "the user's ZDOTDIR travels to the bootstrap"
        );
    }

    #[test]
    fn opt_out_from_the_daemon_or_the_session() {
        let si = integration();
        let out = [(OPT_OUT.to_owned(), "1".to_owned())];
        assert!(si.env_for("/bin/zsh", &out).is_empty(), "session opt-out");
        let not_out = [(OPT_OUT.to_owned(), "0".to_owned())];
        assert!(!si.env_for("/bin/zsh", &not_out).is_empty(), "`0` is not an opt-out");
        let empty = [(OPT_OUT.to_owned(), String::new())];
        assert!(!si.env_for("/bin/zsh", &empty).is_empty(), "nor is an empty value");
        let daemon_out = ShellIntegration { enabled: false, ..integration() };
        assert!(daemon_out.env_for("/bin/zsh", &[]).is_empty(), "daemon opt-out");
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
        // A stale or edited file is rewritten on the next start.
        fs::write(&hooks, "# edited\n").unwrap();
        assert_eq!(install(&tmp).unwrap(), si);
        assert_eq!(fs::read_to_string(&hooks).unwrap(), ZSH_INTEGRATION);
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn an_interactive_zsh_emits_prompt_marks() {
        let tmp = std::env::temp_dir().join(format!("slopty-shell-{}", std::process::id()));
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
        let si = install(&tmp.join("shell")).unwrap();
        let home = tmp.join("home");
        fs::create_dir_all(&home).unwrap();
        // The user's own .zshenv still runs, from the real ZDOTDIR (here: HOME).
        fs::write(home.join(".zshenv"), "export SLOPTY_TEST_ZSHENV=ran\n").unwrap();
        let size = TermSize { cols: 60, rows: 10, metrics: CellMetrics::default() };
        let pty = Pty::open(size).unwrap();
        let mut env = vec![("HOME".to_owned(), home.to_string_lossy().into_owned())];
        env.extend(
            ShellIntegration { original_zdotdir: None, enabled: true, ..si }
                .env_for("/bin/zsh", &[]),
        );
        let mut child = pty
            .spawn(&SpawnSpec {
                command: vec!["/bin/zsh".into(), "-i".into()],
                cwd: None,
                env,
                size,
            })
            .unwrap();
        let master = PtyMaster::new(pty.into_master()).unwrap();
        // Two lines: the status of the first is reported by the precmd before the second.
        master
            .write_all(b"echo zdotdir=$ZDOTDIR env=$SLOPTY_TEST_ZSHENV; false\nexit\n")
            .await
            .unwrap();
        let mut out = Vec::new();
        let mut buf = [0_u8; 4096];
        while let Ok(n) = master.read(&mut buf).await {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        let _status: io::Result<std::process::ExitStatus> = child.wait().await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("\x1b]133;A\x07"), "prompt mark: {text:?}");
        assert!(text.contains("\x1b]133;B\x07"), "input mark: {text:?}");
        assert!(text.contains("\x1b]133;C\x07"), "output mark: {text:?}");
        assert!(text.contains("\x1b]133;D;1\x07"), "status of `false`: {text:?}");
        assert!(
            text.contains("zdotdir= env=ran"),
            "ZDOTDIR handed back, user .zshenv ran: {text:?}"
        );
        let _removed: io::Result<()> = fs::remove_dir_all(&tmp);
    }
}
