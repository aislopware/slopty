//! Pseudo-terminal open/spawn/resize and async master I/O.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags};
use rustix::termios::Winsize;
use serde::{Deserialize, Serialize};
use slopty_core::shell_quote;
use slopty_proto::terminal::{LineDiscipline, TermSize};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

use crate::PtyError;
use crate::shell_integration::{self, ShellIntegration};
use crate::spawn::{Child, Launch};

/// What to run on the PTY.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// Program and arguments. Empty → the user's login shell as a login shell.
    pub command: Vec<String>,
    /// Working directory. `None` → the user's home ([`home`]).
    pub cwd: Option<PathBuf>,
    /// Extra environment on top of the daemon's, `TERM`, `COLORTERM`, `TERM_PROGRAM`.
    pub env: Vec<(String, String)>,
    /// Initial size.
    pub size: TermSize,
}

/// A child spawned on a PTY, and the terminfo name it was given as `TERM`.
#[derive(Debug)]
pub struct Spawned {
    /// The child.
    pub child: Child,
    /// Its `TERM`: [`default_term`] at the moment it was spawned, unless the spec's own
    /// variables named another. Whoever answers the child's terminal queries answers as this.
    pub term: String,
}

/// An open pseudo-terminal pair. The slave is opened once, handed to the child, and closed in
/// the parent at [`Pty::into_master`].
#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
    slave_path: PathBuf,
}

impl Pty {
    /// Open a new PTY at `size`.
    pub fn open(size: TermSize) -> Result<Self, PtyError> {
        let master = open_master().map_err(|e| PtyError::os("open /dev/ptmx", e))?;
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

    /// Borrow the slave, which the child is given.
    pub(crate) fn slave(&self) -> BorrowedFd<'_> {
        self.slave.as_fd()
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
    pub fn spawn(&self, spec: &SpawnSpec) -> Result<Spawned, PtyError> {
        self.spawn_with(spec, None)
    }

    /// [`Pty::spawn`], with `integration` (see [`crate::shell_integration`]) injected when the
    /// program is a shell it covers.
    ///
    /// Everything the child gets (its program, arguments, environment, directory) is settled
    /// here, before [`crate::spawn`] forks: the child only makes system calls on it.
    pub fn spawn_with(
        &self,
        spec: &SpawnSpec,
        integration: Option<&ShellIntegration>,
    ) -> Result<Spawned, PtyError> {
        let cwd = spec.cwd.clone().unwrap_or_else(home);
        let (program, args, arg0) = resolve_command(&spec.command, &cwd);
        let injection = integration.map(|si| si.apply(&program, &args, arg0.as_deref(), &spec.env));
        let (args, arg0, extra_env) = match injection {
            Some(inj) => (inj.args, inj.arg0, inj.env),
            None => (args, arg0, Vec::new()),
        };
        // Decided once: `default_term` changes when the terminfo is installed, and the child's
        // `TERM` and the name reported with it must be the same. The spec's own variables come
        // last, so a `TERM` among them is the child's.
        let term = spec
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == "TERM")
            .map_or_else(|| default_term().to_owned(), |(_, v)| v.clone());
        if term.len() > MAX_TERM_BYTES {
            return Err(PtyError::os(
                "TERM",
                io::Error::new(io::ErrorKind::InvalidInput, "longer than a terminfo name"),
            ));
        }
        let mut env = Env::inherited();
        env.set("TERM", &term);
        env.set("COLORTERM", "truecolor");
        env.set("TERM_PROGRAM", "slopty");
        env.set("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // `TERM` and the search path have to agree: when the database is ours, point the child
        // at it; otherwise clear whatever we inherited and let ncurses use the usual places.
        match crate::terminfo::child_database() {
            Some(dir) => env.set("TERMINFO", dir),
            None => env.remove("TERMINFO"),
        }
        forget_parent_agent(&mut env);
        // Web pages and editors go to the client, unless the session's own variables say
        // otherwise (they come after).
        let handoff = integration.map(|si| {
            let inherited = std::env::var("PATH").ok();
            (si.handoff_env(&spec.env), si.handoff_path(&spec.env, inherited.as_deref()))
        });
        let (handoff_env, handoff_path) = handoff.unwrap_or_default();
        // One of Slopty's own commands inherited from a daemon started inside a Slopty session
        // is nobody's choice; a stale `VISUAL` would beat the user's `EDITOR`.
        for name in ["BROWSER", "EDITOR", "VISUAL"] {
            if env.get(name).is_some_and(shell_integration::is_handoff_command) {
                env.remove(name);
            }
        }
        for (k, v) in handoff_env.into_iter().chain(extra_env) {
            env.set(k, v);
        }
        for (k, v) in &spec.env {
            env.set(k, v);
        }
        if let Some(path) = handoff_path {
            env.set("PATH", path);
        }

        let child = executable(&program, &cwd, env.0.get(OsStr::new("PATH")))
            .and_then(|executable| {
                let argv = std::iter::once(arg0.unwrap_or(program)).chain(args);
                Launch::new(&executable, argv, env.0, &cwd)
            })
            .and_then(|launch| launch.spawn(self.slave()))
            .map_err(|e| PtyError::os("spawn", e))?;
        Ok(Spawned { child, term })
    }
}

/// XNU's `EREDRIVEOPEN`: an open of a cloning device such as `/dev/ptmx` that raced another
/// and has to be made again. The kernel means to redo it itself, but it reaches the caller
/// (`posix_openpt` too): once in 80 000 opens from four threads at once, 23 times in 32 000
/// from eight, and a soak met it after 1195 cycles. It never came twice in a row there. Linux
/// has no such errno, and rustix cannot even hold a negative one there.
#[cfg(target_os = "macos")]
const REDRIVE_OPEN: i32 = -6;

/// How many times [`redriven`] makes an open the kernel keeps asking to redo.
#[cfg(target_os = "macos")]
const REDRIVES: usize = 64;

/// A new master, close-on-exec from the start: set a moment later, a fork by another thread
/// in between hands the master to that child for its whole life, and closing the tile then
/// never hangs its shell up. `posix_openpt` is this `open` on macOS and Linux, and rustix
/// passes it `O_CLOEXEC` only on Linux.
fn open_ptmx() -> rustix::io::Result<OwnedFd> {
    rustix::fs::open(c"/dev/ptmx", OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC, Mode::empty())
}

/// [`open_ptmx`], made again through XNU's two ways of refusing an open that would take.
#[cfg(target_os = "macos")]
fn open_master() -> io::Result<OwnedFd> {
    regrown(|| redriven(open_ptmx)).map_err(exhausted)
}

/// [`open_ptmx`]. Linux's devpts allocates a pair under a lock and refuses only at its limit.
#[cfg(not(target_os = "macos"))]
fn open_master() -> io::Result<OwnedFd> {
    open_ptmx().map_err(io::Error::from).map_err(exhausted)
}

/// How many times [`regrown`] makes again an open refused with ENXIO.
#[cfg(target_os = "macos")]
const REGROWS: usize = 16;

/// `open`, made again while it fails with ENXIO, giving way to other threads in between, up to
/// [`REGROWS`] times; then the last refusal. XNU refuses an open of the clone device with ENXIO
/// when its table of pairs is full and a pair is closed at that moment, far below the limit
/// (`bsd/kern/tty_ptmx.c`): the clone hands out the minor one past the table, the close frees a
/// slot, so the open does not grow the table, and that minor is out of its range. The table
/// grows 16 at a time and never shrinks, so on a freshly booted Mac (a CI runner) the open that
/// fills each 16 beside closes can be refused: 12 and 14 times in 400 opens beside four threads
/// opening and closing, each with a multiple of 16 in use, and 0 times made again so, on a
/// macOS 26.6 guest (`docs/MEASUREMENTS.md`, 2026-10-02). The open made again takes the freed
/// slot; a system out of pairs refuses every time.
#[cfg(target_os = "macos")]
fn regrown<T>(mut open: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let refused = Some(rustix::io::Errno::NXIO.raw_os_error());
    for _ in 0..REGROWS {
        match open() {
            Err(e) if e.raw_os_error() == refused => std::thread::yield_now(),
            opened => return opened,
        }
    }
    open()
}

/// How the clone device refuses an open when every pseudo-terminal the system allows is in
/// use, and the limit to raise: ENXIO under XNU, after [`regrown`]; ENOSPC from Linux's devpts
/// (`devpts_new_index`), whose limit is `kernel.pty.max`.
#[cfg(target_os = "macos")]
const EXHAUSTED: (rustix::io::Errno, &str) = (rustix::io::Errno::NXIO, "kern.tty.ptmx_max");
#[cfg(not(target_os = "macos"))]
const EXHAUSTED: (rustix::io::Errno, &str) = (rustix::io::Errno::NOSPC, "kernel.pty.max");

/// An open of the clone device refused as [`EXHAUSTED`]: said so, with the limit to raise,
/// instead of "Device not configured" or "No space left on device".
fn exhausted(error: io::Error) -> io::Error {
    let (errno, limit) = EXHAUSTED;
    if error.raw_os_error() == Some(errno.raw_os_error()) {
        io::Error::new(
            io::ErrorKind::ResourceBusy,
            format!("every pseudo-terminal the system allows is in use ({limit})"),
        )
    } else {
        error
    }
}

/// `open`, made again while it fails with [`REDRIVE_OPEN`], giving way to other threads in
/// between, up to [`REDRIVES`] times; then `ResourceBusy`, naming the cause, rather than an
/// errno no `strerror` knows. Any other result is returned as it is.
#[cfg(target_os = "macos")]
fn redriven<T>(mut open: impl FnMut() -> rustix::io::Result<T>) -> io::Result<T> {
    for _ in 0..REDRIVES {
        match open() {
            Err(e) if e.raw_os_error() == REDRIVE_OPEN => std::thread::yield_now(),
            opened => return opened.map_err(io::Error::from),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::ResourceBusy,
        format!("the kernel asked for the open to be made again {REDRIVES} times (EREDRIVEOPEN)"),
    ))
}

/// A child's environment: the daemon's, with each change applied in turn, a later one winning.
#[derive(Debug)]
struct Env(BTreeMap<OsString, OsString>);

impl Env {
    fn inherited() -> Self {
        Self(std::env::vars_os().collect())
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(OsStr::new(name)).and_then(|v| v.to_str())
    }

    fn set(&mut self, name: impl Into<OsString>, value: impl Into<OsString>) {
        self.0.insert(name.into(), value.into());
    }

    fn remove(&mut self, name: &str) {
        self.0.remove(OsStr::new(name));
    }
}

/// The file `execve` runs for `program`: a path as given, a relative one taken from `cwd` (the
/// directory the child starts in, as a shell would read it), a bare name from the child's
/// `PATH` as `execvp` and std's `Command` search it, else from this process's, where
/// [`resolve_command`] found it ([`on_path`] for how). A bare name on neither is an error, as
/// `execvp` gives it: `execve` would take it for a file in `cwd`, which may be anybody's
/// checkout.
fn executable(program: &str, cwd: &Path, child_path: Option<&OsString>) -> io::Result<PathBuf> {
    if program.contains('/') {
        return Ok(cwd.join(program));
    }
    let daemon_path = std::env::var_os("PATH");
    let mut failure = io::Error::new(io::ErrorKind::NotFound, format!("{program} is on no PATH"));
    for path in [child_path, daemon_path.as_ref()].into_iter().flatten() {
        match on_path(program, path, cwd) {
            Ok(found) => return Ok(found),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => failure = e,
            Err(_) => {}
        }
    }
    Err(failure)
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

/// The line discipline of the tty behind `fd`.
///
/// On a master this is the slave's: the two ends share one termios, which is how a program's
/// `stty -echo` shows up here (one `tcgetattr`, no round trip to the program).
pub fn line_discipline(fd: impl AsFd) -> Result<LineDiscipline, PtyError> {
    use rustix::termios::LocalModes;
    let termios = rustix::termios::tcgetattr(fd).map_err(|e| PtyError::os("tcgetattr", e))?;
    Ok(LineDiscipline {
        echo: termios.local_modes.contains(LocalModes::ECHO),
        canonical: termios.local_modes.contains(LocalModes::ICANON),
    })
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

    /// The slave's [`LineDiscipline`] now ([`line_discipline`] on this master).
    pub fn line_discipline(&self) -> Result<LineDiscipline, PtyError> {
        line_discipline(self.as_fd())
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

/// What a Claude Code session sets for the programs it starts, naming itself. A daemon started
/// from inside one (a developer's session, a test run by an agent) would hand them to every
/// shell, and a `claude` there then takes itself for that session's child: it saves no
/// transcript (`CLAUDE_CODE_CHILD_SESSION`) and reports to the parent's inbox.
const PARENT_AGENT_ENV: [&str; 9] = [
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
];

/// Start the child outside any Claude Code session the daemon was started from; a session's own
/// environment (`SpawnSpec::env`) is applied after, so it can still set any of them.
fn forget_parent_agent(env: &mut Env) {
    for name in PARENT_AGENT_ENV {
        env.remove(name);
    }
}

/// `(program, args, arg0)` for a child that starts in `cwd`: an empty command means the login shell
/// run as a login shell (`argv[0] = "-zsh"`), like Terminal.app. A bare program name the daemon
/// cannot find on its own `PATH` (a `LaunchAgent` inherits launchd's `/usr/bin:/bin:…`) runs the
/// way the user's terminal would run it: through the login shell, interactive, so the rc files'
/// `PATH` and aliases apply (`claude` is often an alias).
fn resolve_command(command: &[String], cwd: &Path) -> (String, Vec<String>, Option<String>) {
    let shell = login_shell();
    if let Some((program, args)) = command.split_first() {
        let on_daemon_path =
            || std::env::var_os("PATH").is_some_and(|path| on_path(program, &path, cwd).is_ok());
        if program.contains('/') || on_daemon_path() {
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

/// The shell of a user with neither `$SHELL` nor a passwd entry: the one every POSIX system
/// has, where a guess at the system's default for new accounts may not be installed.
const FALLBACK_SHELL: &str = "/bin/sh";

/// `$SHELL`, else the account's shell from the passwd database (a `LaunchAgent` or a systemd
/// user unit gets no `SHELL`), else [`FALLBACK_SHELL`].
fn login_shell() -> String {
    choose_shell(std::env::var("SHELL").ok(), account_shell)
}

/// [`login_shell`]'s order, an empty value counting as none.
fn choose_shell(env: Option<String>, account: impl FnOnce() -> Option<String>) -> String {
    env.filter(|s| !s.is_empty())
        .or_else(|| account().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| FALLBACK_SHELL.to_owned())
}

/// This user's shell in the passwd database.
fn account_shell() -> Option<String> {
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    // A passwd entry fits in 4 KiB (`_SC_GETPW_R_SIZE_MAX`); a longer one is no entry here.
    let mut buf = [libc::c_char::default(); 4096];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let uid = rustix::process::getuid().as_raw();
    // SAFETY: `getpwuid_r` writes only `pwd`, `buf` (for its given length) and `found`: the entry
    // into `pwd` with its strings inside `buf`, and `found` set to `pwd` on success or to null
    // when the uid has no entry.
    let rc = unsafe {
        libc::getpwuid_r(uid, pwd.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &raw mut found)
    };
    if rc != 0 || found.is_null() {
        return None;
    }
    // SAFETY: success filled `pwd`; its `pw_shell` is null or a NUL-terminated string in `buf`,
    // which lives to the end of this function.
    let shell = unsafe { pwd.assume_init_ref() }.pw_shell;
    if shell.is_null() {
        return None;
    }
    // SAFETY: as above, a NUL-terminated string in `buf`.
    unsafe { std::ffi::CStr::from_ptr(shell) }.to_str().ok().map(str::to_owned)
}

/// Where `program` is on `path` (a `PATH` value), searched as `execvp` searches it in a child
/// standing in `cwd`: an empty or relative entry is taken from `cwd`, the first file there that
/// may be run wins, and one that exists but may not be run makes the answer `PermissionDenied`
/// rather than `NotFound`.
fn on_path(program: &str, path: &OsStr, cwd: &Path) -> io::Result<PathBuf> {
    let mut denied = false;
    for dir in std::env::split_paths(path) {
        let candidate = cwd.join(dir).join(program);
        if !std::fs::metadata(&candidate).is_ok_and(|m| m.is_file()) {
            continue;
        }
        match rustix::fs::access(&candidate, rustix::fs::Access::EXEC_OK) {
            Ok(()) => return Ok(candidate),
            Err(rustix::io::Errno::ACCESS) => denied = true,
            Err(_) => {}
        }
    }
    Err(if denied {
        io::Error::new(io::ErrorKind::PermissionDenied, format!("{program} on PATH may not be run"))
    } else {
        io::Error::new(io::ErrorKind::NotFound, format!("{program} is on no PATH"))
    })
}

/// The user's home directory, by the rule of `slopty_platform::dirs::home`: `$HOME` when set
/// and not empty, else the password-database entry, and `/` when neither is an absolute path.
///
/// Spelled here rather than called there because `slopty-ptyd` links this crate, and linking
/// `slopty-platform` would load `AppKit`, `WebKit`, `UserNotifications` and `AudioToolbox` into
/// the PTY custodian (otool -L), a few milliseconds a launch and their memory for its whole
/// life, for one line.
#[must_use]
pub fn home() -> PathBuf {
    std::env::home_dir().filter(|h| h.is_absolute()).unwrap_or_else(|| PathBuf::from("/"))
}

/// The longest `TERM` a child is given. A terminfo entry is a file named after its terminal, so
/// no usable name is longer than `NAME_MAX`; the bound lets the name ride in ptyd's replies.
pub const MAX_TERM_BYTES: usize = 255;

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
            .unwrap()
            .child;
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

    /// The `TERM` a child is given is the one reported with it: the default, or the spec's own.
    #[tokio::test]
    async fn the_term_reported_is_the_one_the_child_sees() {
        for env in [Vec::new(), vec![("TERM".to_owned(), "vt100".to_owned())]] {
            let pty = Pty::open(size()).unwrap();
            let spawned = pty
                .spawn(&SpawnSpec {
                    command: vec!["/bin/sh".into(), "-c".into(), "echo \"<$TERM>\"".into()],
                    cwd: None,
                    env: env.clone(),
                    size: size(),
                })
                .unwrap();
            let expected = if let [(_, v)] = env.as_slice() { v.as_str() } else { default_term() };
            assert_eq!(spawned.term, expected);
            let master = PtyMaster::new(pty.into_master()).unwrap();
            read_until(&master, format!("<{expected}>").as_bytes()).await;
        }
    }

    /// Read `master` until `needle` has come.
    async fn read_until(master: &PtyMaster, needle: &[u8]) {
        let mut out = Vec::new();
        let mut buf = [0_u8; 1024];
        let deadline =
            tokio::time::Instant::now().checked_add(std::time::Duration::from_secs(10)).unwrap();
        while !out.windows(needle.len()).any(|w| w == needle) {
            let n =
                tokio::time::timeout_at(deadline, master.read(&mut buf)).await.unwrap().unwrap();
            assert_ne!(n, 0, "EOF before {needle:?}");
            out.extend_from_slice(&buf[..n]);
        }
    }

    /// The master reads the slave's termios: a program that turns echo off for a password, or
    /// canonical input off for a TUI, is seen doing so from the master alone.
    #[tokio::test]
    async fn the_master_sees_the_programs_echo_and_canonical_modes() {
        let pty = Pty::open(size()).unwrap();
        let script = "stty -echo; echo password; read x; stty echo -icanon; echo keys; sleep 30";
        let mut child = pty
            .spawn(&SpawnSpec {
                command: vec!["/bin/sh".into(), "-c".into(), script.into()],
                cwd: None,
                env: Vec::new(),
                size: size(),
            })
            .unwrap()
            .child;
        let master = PtyMaster::new(pty.into_master()).unwrap();
        read_until(&master, b"password").await;
        let at_password = master.line_discipline().unwrap();
        assert_eq!(at_password, LineDiscipline { echo: false, canonical: true });
        master.write_all(b"secret\n").await.unwrap();
        read_until(&master, b"keys").await;
        let in_a_tui = master.line_discipline().unwrap();
        assert_eq!(in_a_tui, LineDiscipline { echo: true, canonical: false });
        child.kill().await.unwrap();
    }

    /// What reading the line discipline costs the session actor after each read. Run with
    /// `cargo nextest run -p slopty-pty --release --run-ignored only line_discipline_cost
    /// --no-capture`.
    #[tokio::test]
    #[ignore = "measurement, run by hand"]
    async fn line_discipline_cost() {
        let pty = Pty::open(size()).unwrap();
        let master = PtyMaster::new(pty.into_master()).unwrap();
        let reads = 100_000_u32;
        let started = std::time::Instant::now();
        for _ in 0..reads {
            std::hint::black_box(master.line_discipline().unwrap());
        }
        let each = started.elapsed() / reads;
        eprintln!("line_discipline_cost: {} ns per tcgetattr on a master", each.as_nanos());
    }

    /// The tty is the child's controlling terminal and the child leads its foreground group,
    /// whatever the program: `dd` opens `/dev/tty`, which only a process with a controlling
    /// terminal can, and does nothing to get one itself (bash would, hiding the difference).
    #[tokio::test]
    async fn the_tty_is_the_controlling_terminal_of_the_child() {
        let spec = |command: &[&str]| SpawnSpec {
            command: command.iter().map(|&word| word.to_owned()).collect(),
            cwd: None,
            env: Vec::new(),
            size: size(),
        };
        let pty = Pty::open(size()).unwrap();
        let mut dd = pty.spawn(&spec(&["dd", "if=/dev/tty", "of=/dev/null", "count=1"])).unwrap();
        let master = PtyMaster::new(pty.into_master()).unwrap();
        master.write_all(b"line\n").await.unwrap();
        // Its closing statistics go to the tty, and on macOS it cannot finish exiting until they
        // are all read: the last of its three lines, which may come in a read of its own, and
        // which BSD's dd and GNU's word differently.
        let last_line: &[u8] =
            if cfg!(target_os = "macos") { b"bytes transferred" } else { b" copied, " };
        read_until(&master, last_line).await;
        let status = dd.child.wait().await.unwrap();
        assert_eq!(status.code(), Some(0), "dd could not open /dev/tty: {status:?}");

        let pty = Pty::open(size()).unwrap();
        let mut sleeper = pty.spawn(&spec(&["/bin/sleep", "30"])).unwrap().child;
        let pid = sleeper.id().unwrap();
        let foreground = rustix::termios::tcgetpgrp(pty.master()).unwrap();
        assert_eq!(foreground.as_raw_nonzero().get().unsigned_abs(), pid, "its own group leads");
        sleeper.kill().await.unwrap();
    }

    /// What starting a shell costs, from the call to the program running: all of
    /// [`Pty::spawn`], its preparing alone (the spawn failing just before the fork), its fork
    /// and exec alone, and std's `Command` with the `pre_exec` it used to take.
    /// `SLOPTY_SPAWN_BALLAST_MB` grows this process first, as a daemon holding many sessions'
    /// backlogs is. Run with `cargo nextest run -p slopty-pty --release --run-ignored only
    /// spawn_cost --no-capture`.
    #[tokio::test]
    #[ignore = "measurement, run by hand"]
    async fn spawn_cost() {
        use std::os::unix::process::CommandExt as _;
        use std::time::{Duration, Instant};

        let ballast_mb: usize =
            std::env::var("SLOPTY_SPAWN_BALLAST_MB").map_or(0, |mb| mb.parse().unwrap());
        let ballast = vec![1_u8; ballast_mb << 20];
        let spec = SpawnSpec {
            command: vec!["/usr/bin/true".into()],
            cwd: Some(std::env::temp_dir()),
            env: Vec::new(),
            size: size(),
        };
        let launch = Launch::new(
            Path::new("/usr/bin/true"),
            ["true"],
            std::env::vars_os(),
            &std::env::temp_dir(),
        )
        .unwrap();
        // A NUL in the last variable fails the spawn after all the preparing, before the fork.
        let prepare_only =
            SpawnSpec { env: vec![("~SLOPTY_NUL".to_owned(), "\0".to_owned())], ..spec.clone() };
        let rounds = 300;
        let (mut ours, mut fork_only, mut std_command) = (Vec::new(), Vec::new(), Vec::new());
        let mut preparing = Vec::new();
        for _ in 0..rounds {
            let pty = Pty::open(size()).unwrap();
            let started = Instant::now();
            pty.spawn(&prepare_only).unwrap_err();
            preparing.push(started.elapsed());

            let pty = Pty::open(size()).unwrap();
            let started = Instant::now();
            let mut child = pty.spawn(&spec).unwrap().child;
            ours.push(started.elapsed());
            child.wait().await.unwrap();

            let pty = Pty::open(size()).unwrap();
            let started = Instant::now();
            let mut child = launch.spawn(pty.slave()).unwrap();
            fork_only.push(started.elapsed());
            child.wait().await.unwrap();

            let pty = Pty::open(size()).unwrap();
            let mut command = std::process::Command::new("/usr/bin/true");
            command.current_dir(std::env::temp_dir());
            command.stdin(pty.slave.try_clone().unwrap());
            command.stdout(pty.slave.try_clone().unwrap());
            command.stderr(pty.slave.try_clone().unwrap());
            let controlling_tty = || -> io::Result<()> {
                rustix::process::setsid()?;
                // SAFETY: `Command` has put the slave on fd 0 by now.
                let stdin = unsafe { BorrowedFd::borrow_raw(0) };
                rustix::process::ioctl_tiocsctty(stdin)?;
                Ok(())
            };
            // SAFETY: setsid and TIOCSCTTY, raw system calls, as the old spawn did.
            unsafe {
                command.pre_exec(controlling_tty);
            }
            let started = Instant::now();
            let mut child = command.spawn().unwrap();
            std_command.push(started.elapsed());
            child.wait().unwrap();
        }
        let percentiles = |samples: &mut Vec<Duration>| {
            samples.sort();
            let at = |q: usize| samples[(samples.len() - 1) * q / 100].as_micros();
            format!("p50 {} us p95 {} us p99 {} us", at(50), at(95), at(99))
        };
        eprintln!(
            "spawn_cost ({ballast_mb} MiB ballast, {rounds} rounds): Pty::spawn {}; its preparing \
             alone {}; Launch::spawn alone {}; std Command + pre_exec {}",
            percentiles(&mut ours),
            percentiles(&mut preparing),
            percentiles(&mut fork_only),
            percentiles(&mut std_command),
        );
        std::hint::black_box(ballast);
    }

    #[tokio::test]
    async fn resize_through_the_master_is_visible() {
        let pty = Pty::open(size()).unwrap();
        set_size(pty.master(), TermSize { cols: 100, rows: 30, metrics: CellMetrics::default() })
            .unwrap();
        assert_eq!(get_size(pty.master()).unwrap(), (100, 30));
    }

    /// A shell never inherits the identity of a Claude Code session the daemon ran under, while
    /// the user's own Claude Code settings in the environment pass through.
    #[test]
    fn a_shell_forgets_the_claude_session_the_daemon_ran_in() {
        let names = ["CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDECODE"];
        let mut env = Env(names
            .iter()
            .chain(&["CLAUDE_CODE_USE_BEDROCK"])
            .map(|name| (OsString::from(name), OsString::from("1")))
            .collect());
        forget_parent_agent(&mut env);
        for name in names {
            assert_eq!(env.get(name), None, "{name} kept");
        }
        assert_eq!(env.get("CLAUDE_CODE_USE_BEDROCK"), Some("1"), "settings pass");
    }

    #[test]
    fn login_shell_gets_dash_argv0() {
        let (_, args, arg0) = resolve_command(&[], Path::new("/"));
        assert!(args.is_empty());
        assert!(arg0.unwrap().starts_with('-'));
        let (p, a, explicit_arg0) =
            resolve_command(&["/bin/ls".to_owned(), "-l".to_owned()], Path::new("/"));
        assert_eq!((p.as_str(), a.len(), explicit_arg0), ("/bin/ls", 1, None));
    }

    #[test]
    fn bare_program_on_path_runs_directly() {
        let (p, a, arg0) = resolve_command(&["ls".to_owned(), "-l".to_owned()], Path::new("/"));
        assert_eq!((p.as_str(), a.as_slice(), arg0), ("ls", &["-l".to_owned()][..], None));
    }

    /// `$SHELL` first, then the passwd entry (all a systemd unit has), then the system's own.
    #[test]
    fn the_shell_is_the_environments_then_the_accounts_then_the_systems() {
        let fish = || Some("/usr/bin/fish".to_owned());
        assert_eq!(choose_shell(Some("/bin/sh".to_owned()), fish), "/bin/sh");
        assert_eq!(choose_shell(Some(String::new()), fish), "/usr/bin/fish");
        assert_eq!(choose_shell(None, || Some(String::new())), "/bin/sh");
        assert_eq!(choose_shell(None, || None), "/bin/sh");
    }

    /// A bare name found on no `PATH` is an error, not a file of that name in the directory
    /// the child starts in, which `execve` would run: that directory may be anybody's checkout.
    #[test]
    fn a_bare_name_on_no_path_is_not_run_from_the_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let planted = dir.path().join("slopty-planted-program");
        std::fs::write(&planted, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
        let nowhere = OsString::from("/nonexistent");
        let error = executable("slopty-planted-program", dir.path(), Some(&nowhere)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
        let found = executable("sh", dir.path(), Some(&OsString::from("/bin"))).unwrap();
        assert_eq!(found, Path::new("/bin/sh"));
    }

    /// A name on `PATH` whose file may not be run is `PermissionDenied`, as `execvp` reports
    /// it, not `NotFound`.
    #[test]
    fn a_path_hit_that_may_not_run_is_permission_denied() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("slopty-unrunnable"), "#!/bin/sh\n").unwrap();
        let path = dir.path().as_os_str().to_owned();
        let error = executable("slopty-unrunnable", Path::new("/"), Some(&path)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{error}");
    }

    /// An open refused while the kernel's table of pairs grows is made again until it takes;
    /// one refused every time is refused as exhaustion, named so; any other failure comes back
    /// at once. XNU's alone: Linux's devpts never refuses below its limit.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_open_refused_as_the_table_grows_is_made_again() {
        let refused = || io::Error::from_raw_os_error(rustix::io::Errno::NXIO.raw_os_error());
        let mut tries = 0_usize;
        let opened = regrown(|| {
            tries = tries.saturating_add(1);
            if tries < 3 { Err(refused()) } else { Ok(tries) }
        });
        assert_eq!(opened.unwrap(), 3);

        let mut tries = 0_usize;
        let error = regrown(|| {
            tries = tries.saturating_add(1);
            Err::<(), _>(refused())
        })
        .map_err(exhausted)
        .unwrap_err();
        assert_eq!(tries, REGROWS.saturating_add(1));
        assert_eq!(error.kind(), io::ErrorKind::ResourceBusy, "{error}");
        assert!(error.to_string().contains("kern.tty.ptmx_max"), "{error}");

        let mut tries = 0_usize;
        let error = regrown(|| {
            tries = tries.saturating_add(1);
            Err::<(), _>(io::Error::from_raw_os_error(rustix::io::Errno::ACCESS.raw_os_error()))
        })
        .unwrap_err();
        assert_eq!((error.kind(), tries), (io::ErrorKind::PermissionDenied, 1));
    }

    /// The refusal this kernel gives at its limit names the limit; any other refusal stays as
    /// it came.
    #[test]
    fn running_out_of_pseudo_terminals_says_so() {
        let (errno, limit) = EXHAUSTED;
        let error = exhausted(io::Error::from_raw_os_error(errno.raw_os_error()));
        assert_eq!(error.kind(), io::ErrorKind::ResourceBusy, "{error}");
        assert!(error.to_string().contains(limit), "{error}");
        let other =
            exhausted(io::Error::from_raw_os_error(rustix::io::Errno::ACCESS.raw_os_error()));
        assert_eq!(other.raw_os_error(), Some(rustix::io::Errno::ACCESS.raw_os_error()));
    }

    /// An open the kernel asks to redo is made again until it takes; one it keeps asking for
    /// ends in a `ResourceBusy` that names the cause, never a panic or a bare -6; any other
    /// failure comes back at once. XNU's alone: no other kernel has the errno.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_open_the_kernel_asks_to_redo_is_redone_then_given_up_clearly() {
        let redo = || rustix::io::Errno::from_raw_os_error(REDRIVE_OPEN);
        let mut tries = 0_usize;
        let opened = redriven(|| {
            tries = tries.saturating_add(1);
            if tries < 3 { Err(redo()) } else { Ok(tries) }
        });
        assert_eq!(opened.unwrap(), 3);

        let error = redriven(|| Err::<(), _>(redo())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ResourceBusy, "{error}");
        assert!(error.to_string().contains("EREDRIVEOPEN"), "{error}");
        let error = PtyError::os("open /dev/ptmx", redriven(|| Err::<(), _>(redo())).unwrap_err());
        assert!(error.to_string().starts_with("open /dev/ptmx: the kernel asked"), "{error}");

        let mut tries = 0_usize;
        let error = redriven(|| {
            tries = tries.saturating_add(1);
            Err::<(), _>(rustix::io::Errno::NOENT)
        })
        .unwrap_err();
        assert_eq!((error.kind(), tries), (io::ErrorKind::NotFound, 1));
    }

    #[test]
    fn unknown_bare_program_goes_through_the_login_shell() {
        let (p, a, arg0) =
            resolve_command(&["slopty-no-such-tool".to_owned(), "it's".to_owned()], Path::new("/"));
        assert_eq!(p, login_shell());
        assert_eq!(a, vec!["-lic".to_owned(), "slopty-no-such-tool 'it'\\''s'".to_owned()]);
        assert_eq!(arg0, None);
    }
}
