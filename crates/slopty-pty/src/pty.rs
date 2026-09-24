//! Pseudo-terminal open/spawn/resize and async master I/O.

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use rustix::fs::{Mode, OFlags};
use rustix::io::FdFlags;
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;
use serde::{Deserialize, Serialize};
use slopty_proto::terminal::TermSize;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

use crate::PtyError;
use crate::shell_integration::ShellIntegration;

/// What to run on the PTY.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// Program and arguments. Empty → the user's login shell as a login shell.
    pub command: Vec<String>,
    /// Working directory. `None` → `$HOME`.
    pub cwd: Option<PathBuf>,
    /// Extra environment on top of the daemon's, `TERM`, `COLORTERM`, `TERM_PROGRAM`.
    pub env: Vec<(String, String)>,
    /// Initial size.
    pub size: TermSize,
}

/// An open pseudo-terminal pair. The slave is opened once for the child and closed in the parent
/// right after spawn.
#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
    slave_path: PathBuf,
}

impl Pty {
    /// Open a new PTY at `size`.
    pub fn open(size: TermSize) -> Result<Self, PtyError> {
        let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)
            .map_err(|e| PtyError::os("posix_openpt", e))?;
        // macOS posix_openpt has no O_CLOEXEC; set it so the child never inherits the master.
        rustix::io::fcntl_setfd(&master, FdFlags::CLOEXEC).map_err(|e| PtyError::os("fcntl", e))?;
        rustix::pty::grantpt(&master).map_err(|e| PtyError::os("grantpt", e))?;
        rustix::pty::unlockpt(&master).map_err(|e| PtyError::os("unlockpt", e))?;
        let name =
            rustix::pty::ptsname(&master, Vec::new()).map_err(|e| PtyError::os("ptsname", e))?;
        let slave_path = PathBuf::from(OsString::from(name.to_string_lossy().into_owned()));
        // The tty state only exists once the slave is open; size it through the slave so the
        // child sees the right geometry from its first syscall.
        let slave = rustix::fs::open(
            &slave_path,
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| PtyError::os("open slave", e))?;
        set_size(&slave, size)?;
        Ok(Self { master, slave, slave_path })
    }

    /// Path of the slave device (`/dev/ttys00N`).
    #[must_use]
    pub fn slave_path(&self) -> &Path {
        &self.slave_path
    }

    /// Borrow the master.
    #[must_use]
    pub fn master(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// Take the master out (for handing to another process or wrapping in [`PtyMaster`]). The
    /// parent's slave handle closes here; only the child keeps one.
    #[must_use]
    pub fn into_master(self) -> OwnedFd {
        self.master
    }

    /// Spawn `spec` on this PTY as a new session with the slave as its controlling terminal.
    pub fn spawn(&self, spec: &SpawnSpec) -> Result<tokio::process::Child, PtyError> {
        self.spawn_with(spec, None)
    }

    /// [`Pty::spawn`], with `integration` (see [`crate::shell_integration`]) injected when the
    /// program is a shell it covers.
    pub fn spawn_with(
        &self,
        spec: &SpawnSpec,
        integration: Option<&ShellIntegration>,
    ) -> Result<tokio::process::Child, PtyError> {
        let slave = self.slave.try_clone().map_err(|e| PtyError::os("dup slave", e))?;
        let (program, args, arg0) = resolve_command(&spec.command);
        let injection = integration.map(|si| si.apply(&program, &args, arg0.as_deref(), &spec.env));
        let (args, arg0, extra_env) = match injection {
            Some(inj) => (inj.args, inj.arg0, inj.env),
            None => (args, arg0, Vec::new()),
        };

        let mut cmd = std::process::Command::new(&program);
        cmd.args(&args);
        if let Some(arg0) = arg0 {
            cmd.arg0(arg0);
        }
        cmd.current_dir(spec.cwd.clone().unwrap_or_else(home_dir));
        cmd.env("TERM", default_term());
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "slopty");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // `TERM` and the search path have to agree: when the database is ours, point the child
        // at it; otherwise clear whatever we inherited and let ncurses use the usual places.
        match crate::terminfo::child_database() {
            Some(dir) => cmd.env("TERMINFO", dir),
            None => cmd.env_remove("TERMINFO"),
        };
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        let dup = |what: &'static str| slave.try_clone().map_err(|e| PtyError::os(what, e));
        cmd.stdin(Stdio::from(dup("dup slave for stdin")?));
        cmd.stdout(Stdio::from(dup("dup slave for stdout")?));
        cmd.stderr(Stdio::from(slave));

        // SAFETY: `make_controlling_tty` only issues async-signal-safe raw syscalls (setsid,
        // ioctl) between fork and exec.
        unsafe {
            cmd.pre_exec(make_controlling_tty);
        }

        let child = tokio::process::Command::from(cmd)
            .kill_on_drop(false)
            .spawn()
            .map_err(|e| PtyError::os("spawn", e))?;
        Ok(child)
    }
}

/// Runs in the forked child before exec: new session, slave (on fd 0) as controlling tty.
fn make_controlling_tty() -> io::Result<()> {
    rustix::process::setsid()?;
    // SAFETY: `Command` dup2'd the slave onto fd 0 before running pre_exec, and fd 0 stays open
    // for the child's lifetime, so the borrow is valid here.
    let stdin = unsafe { BorrowedFd::borrow_raw(0) };
    rustix::process::ioctl_tiocsctty(stdin)?;
    Ok(())
}

/// Apply `TIOCSWINSZ` to a PTY fd.
pub fn set_size(fd: impl AsFd, size: TermSize) -> Result<(), PtyError> {
    let ws = Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: u16::try_from(size.width_px()).unwrap_or(u16::MAX),
        ws_ypixel: u16::try_from(size.height_px()).unwrap_or(u16::MAX),
    };
    rustix::termios::tcsetwinsize(fd, ws).map_err(|e| PtyError::os("TIOCSWINSZ", e))
}

/// Read back the kernel's idea of the size.
pub fn get_size(fd: impl AsFd) -> Result<(u16, u16), PtyError> {
    let ws = rustix::termios::tcgetwinsize(fd).map_err(|e| PtyError::os("TIOCGWINSZ", e))?;
    Ok((ws.ws_col, ws.ws_row))
}

/// Async master end.
#[derive(Debug)]
pub struct PtyMaster {
    fd: AsyncFd<OwnedFd>,
}

impl PtyMaster {
    /// Wrap a master fd for use on the tokio runtime; switches it to non-blocking.
    pub fn new(fd: OwnedFd) -> Result<Self, PtyError> {
        rustix::io::ioctl_fionbio(&fd, true).map_err(|e| PtyError::os("FIONBIO", e))?;
        let fd = AsyncFd::with_interest(fd, Interest::READABLE | Interest::WRITABLE)
            .map_err(|e| PtyError::os("AsyncFd", e))?;
        Ok(Self { fd })
    }

    /// Read available output. `Ok(0)` means the slave side is gone (child exited).
    pub async fn read(&self, buf: &mut [u8]) -> Result<usize, PtyError> {
        loop {
            let mut guard = self.fd.readable().await.map_err(|e| PtyError::os("readable", e))?;
            match guard.try_io(|inner| read_fd(inner.get_ref(), buf)) {
                Ok(Ok(n)) => return Ok(n),
                Ok(Err(e)) => return Err(PtyError::os("read", e)),
                Err(_would_block) => {}
            }
        }
    }

    /// Write all of `data`, waiting on the tty as long as it takes.
    pub async fn write_all(&self, mut data: &[u8]) -> Result<(), PtyError> {
        while !data.is_empty() {
            let mut guard = self.fd.writable().await.map_err(|e| PtyError::os("writable", e))?;
            match guard.try_io(|inner| write_fd(inner.get_ref(), data)) {
                Ok(Ok(n)) => data = data.get(n..).unwrap_or_default(),
                Ok(Err(e)) => return Err(PtyError::os("write", e)),
                Err(_would_block) => {}
            }
        }
        Ok(())
    }

    /// Write what the tty takes right now, without waiting: the number of bytes written, 0 when
    /// its input queue is full. A writer that must also keep reading (a program that echoes
    /// its input fills the output while the input queue waits on it) writes with this and
    /// [`Self::writable`] rather than [`Self::write_all`].
    pub fn try_write(&self, data: &[u8]) -> Result<usize, PtyError> {
        match self.fd.try_io(Interest::WRITABLE, |inner| write_fd(inner, data)) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(PtyError::os("write", e)),
        }
    }

    /// Wait until the tty may take input again.
    pub async fn writable(&self) -> Result<(), PtyError> {
        self.fd.writable().await.map(drop).map_err(|e| PtyError::os("writable", e))
    }

    /// The raw fd (for `TIOCSWINSZ`, termios queries).
    #[must_use]
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.get_ref().as_fd()
    }
}

fn read_fd(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    match rustix::io::read(fd, buf) {
        Ok(n) => Ok(n),
        // macOS reports the slave hangup as EIO on read; treat it as EOF.
        Err(rustix::io::Errno::IO) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

fn write_fd(fd: &OwnedFd, data: &[u8]) -> io::Result<usize> {
    rustix::io::write(fd, data).map_err(Into::into)
}

/// `(program, args, arg0)`: an empty command means the login shell run as a login shell
/// (`argv[0] = "-zsh"`), like Terminal.app. A bare program name the daemon cannot find on its
/// own `PATH` (a `LaunchAgent` inherits launchd's `/usr/bin:/bin:…`) runs the way the user's
/// terminal would run it: through the login shell, interactive, so the rc files' `PATH` and
/// aliases apply (`claude` is often an alias).
fn resolve_command(command: &[String]) -> (String, Vec<String>, Option<String>) {
    let shell = login_shell();
    if let Some((program, args)) = command.split_first() {
        if program.contains('/') || on_path(program) {
            return (program.clone(), args.to_vec(), None);
        }
        let line = command.iter().map(|word| shell_quote(word)).collect::<Vec<_>>().join(" ");
        return (shell, vec!["-lic".to_owned(), line], None);
    }
    let base = Path::new(&shell)
        .file_name()
        .map_or_else(|| "sh".to_owned(), |n| n.to_string_lossy().into_owned());
    (shell, Vec::new(), Some(format!("-{base}")))
}

/// `$SHELL`, else the account's shell from the passwd database (a `LaunchAgent` gets no
/// `SHELL`), else zsh.
fn login_shell() -> String {
    if let Some(shell) = std::env::var("SHELL").ok().filter(|s| !s.is_empty()) {
        return shell;
    }
    nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|user| user.shell.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_owned())
}

/// Whether `program` is an executable file on this process's `PATH`.
fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            std::fs::metadata(dir.join(program))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Single-quote a word for a POSIX or fish shell.
fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b))
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `xterm-ghostty` when its terminfo is installed (ghostty's entry is the most complete), else
/// `xterm-256color`.
///
/// Read per spawn, not cached: [`crate::terminfo::install`] runs alongside the first shells, so
/// a shell that starts a moment later gets the better answer.
#[must_use]
pub fn default_term() -> &'static str {
    if crate::terminfo::installed() { crate::terminfo::NAMES[0] } else { "xterm-256color" }
}

impl AsRawFd for PtyMaster {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.get_ref().as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::input::CellMetrics;

    use super::*;

    fn size() -> TermSize {
        TermSize { cols: 40, rows: 10, metrics: CellMetrics { cell_width: 7, cell_height: 14 } }
    }

    #[tokio::test]
    async fn spawn_echo_and_read_output() {
        let pty = Pty::open(size()).unwrap();
        assert_eq!(get_size(pty.master()).unwrap(), (40, 10));
        let mut child = pty
            .spawn(&SpawnSpec {
                command: vec!["/bin/sh".into(), "-c".into(), "stty size; echo done".into()],
                cwd: None,
                env: Vec::new(),
                size: size(),
            })
            .unwrap();
        let master = PtyMaster::new(pty.into_master()).unwrap();
        let mut out = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let n = master.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            if out.windows(4).any(|w| w == b"done") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("10 40"), "stty should see our size: {text}");
        let status = child.wait().await.unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn resize_through_the_master_is_visible() {
        let pty = Pty::open(size()).unwrap();
        set_size(pty.master(), TermSize { cols: 100, rows: 30, metrics: CellMetrics::default() })
            .unwrap();
        assert_eq!(get_size(pty.master()).unwrap(), (100, 30));
    }

    #[test]
    fn login_shell_gets_dash_argv0() {
        let (_, args, arg0) = resolve_command(&[]);
        assert!(args.is_empty());
        assert!(arg0.unwrap().starts_with('-'));
        let (p, a, explicit_arg0) = resolve_command(&["/bin/ls".to_owned(), "-l".to_owned()]);
        assert_eq!((p.as_str(), a.len(), explicit_arg0), ("/bin/ls", 1, None));
    }

    #[test]
    fn bare_program_on_path_runs_directly() {
        let (p, a, arg0) = resolve_command(&["ls".to_owned(), "-l".to_owned()]);
        assert_eq!((p.as_str(), a.as_slice(), arg0), ("ls", &["-l".to_owned()][..], None));
    }

    #[test]
    fn unknown_bare_program_goes_through_the_login_shell() {
        let (p, a, arg0) = resolve_command(&["slopty-no-such-tool".to_owned(), "it's".to_owned()]);
        assert_eq!(p, login_shell());
        assert_eq!(a, vec!["-lic".to_owned(), "slopty-no-such-tool 'it'\\''s'".to_owned()]);
        assert_eq!(arg0, None);
    }

    #[test]
    fn shell_quote_leaves_plain_words_alone() {
        assert_eq!(shell_quote("--flag=1"), "--flag=1");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
    }
}
