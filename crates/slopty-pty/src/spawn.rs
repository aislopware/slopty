//! Starting a program on a PTY, and the [`Child`] that comes back.
//!
//! Between a `fork` and its `exec` the child may run only async-signal-safe code: another
//! thread may have held the allocator's or some other lock at the moment of the fork, and the
//! child has a copy of it held forever. std's `Command` runs its own code there, and when the
//! child cannot `chdir` or `exec` it writes the errno to a pipe and aborts if that write fails,
//! which it does once the parent has died (a PTY custodian killed mid-spawn, the session's
//! directory already gone). So the fork here is our own, and the child side is system calls on
//! data built before the fork, nothing else: no allocation, no lock, no panic, and a report that
//! may fail without consequence. `posix_spawn` would need no child side at all, but on macOS it
//! cannot give the child a controlling terminal (`docs/decisions/terminal.md`, "A shell starts
//! from our own fork").
//!
//! The parent learns how the child fared through a `Handshake`: on macOS a shared page for
//! the report and the kernel's word on the `exec`, on Linux a close-on-exec pipe.

use std::ffi::{CString, OsStr, OsString};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;
use std::{fmt, io};

use libc::{c_char, c_int};
use rustix::process::{Pid, Signal, WaitOptions};
use tokio::signal::unix::{SignalKind, signal};

/// One past the highest signal number (`NSIG`); the `libc` crate does not name it for either.
#[cfg(target_vendor = "apple")]
const SIGNALS: c_int = 32;
#[cfg(not(target_vendor = "apple"))]
const SIGNALS: c_int = 65;

/// The exit status of a child that could not become its program, as a shell reports one it
/// cannot run.
const CANNOT_START: c_int = 127;

/// The shell `execvp` hands a file the kernel cannot run (`ENOEXEC`: no `#!`, not a binary).
const SHELL: &std::ffi::CStr = c"/bin/sh";

/// A program to start on a tty, every string already in C form: the child builds nothing.
#[derive(Debug)]
pub(crate) struct Launch {
    executable: CString,
    argv: Vec<CString>,
    env: Vec<CString>,
    cwd: CString,
    /// The open-descriptor limit the program starts with ([`program_descriptors`]).
    descriptors: libc::rlimit,
}

/// The soft limit on open descriptors a program started here gets at most: what a login gives,
/// whatever this process raised its own to. A program that `select`s fails on a descriptor past
/// `FD_SETSIZE` (1024), so a limit above it only hides that until the day it bites.
const PROGRAM_DESCRIPTORS: u64 = 1024;

/// This process's limit on open descriptors with the soft one at most
/// [`PROGRAM_DESCRIPTORS`], for the program to start with.
fn program_descriptors() -> libc::rlimit {
    let now = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    let max = now.maximum.unwrap_or(libc::RLIM_INFINITY);
    let current = now.current.unwrap_or(libc::RLIM_INFINITY).min(PROGRAM_DESCRIPTORS).min(max);
    libc::rlimit { rlim_cur: current, rlim_max: max }
}

impl Launch {
    /// `executable` run with `argv` (its `argv[0]` first) and exactly `env`, in `cwd`.
    /// `InvalidInput` when any of them holds a NUL.
    pub(crate) fn new(
        executable: &Path,
        argv: impl IntoIterator<Item = impl AsRef<OsStr>>,
        env: impl IntoIterator<Item = (OsString, OsString)>,
        cwd: &Path,
    ) -> io::Result<Self> {
        let env = env
            .into_iter()
            .map(|(name, value)| {
                let mut line = name.into_encoded_bytes();
                line.push(b'=');
                line.extend_from_slice(value.as_encoded_bytes());
                c_string(&line)
            })
            .collect::<io::Result<_>>()?;
        Ok(Self {
            executable: c_string(executable.as_os_str().as_bytes())?,
            argv: argv
                .into_iter()
                .map(|arg| c_string(arg.as_ref().as_bytes()))
                .collect::<io::Result<_>>()?,
            env,
            cwd: c_string(cwd.as_os_str().as_bytes())?,
            descriptors: program_descriptors(),
        })
    }

    /// Start it on `tty` (a PTY's slave): a new session whose controlling terminal the tty is,
    /// on fds 0, 1 and 2. Returns once the program runs, or with the step that failed and why.
    pub(crate) fn spawn(&self, tty: BorrowedFd<'_>) -> io::Result<Child> {
        let pointers = Pointers::new(self);
        let moved_tty = above_stdio(tty)?;
        let tty = moved_tty.as_ref().map_or(tty, |moved| moved.as_fd());
        let handshake = Handshake::new()?;
        let pid = self.fork(&pointers, tty, handshake.child_end())?;
        match handshake.outcome(pid) {
            Ok(None) => Ok(Child { pid, status: None }),
            Ok(Some((step, errno))) => {
                // It `_exit`s right after its report.
                let _reaped = reap(pid, WaitOptions::empty());
                let cause = io::Error::from_raw_os_error(errno);
                let what = match step {
                    Step::Directory => format!("{step} {}", self.cwd.to_string_lossy()),
                    Step::Exec => format!("{step} {}", self.executable.to_string_lossy()),
                    Step::Session | Step::Terminal => step.to_string(),
                };
                Err(io::Error::new(cause.kind(), format!("{what}: {cause}")))
            }
            Err(e) => {
                let _killed = rustix::process::kill_process(pid, Signal::KILL);
                let _reaped = reap(pid, WaitOptions::empty());
                Err(e)
            }
        }
    }

    /// Fork with every signal blocked, so no handler of this process runs in the child before
    /// it has put them back to their defaults; the child goes on in [`Self::become_program`].
    fn fork(&self, pointers: &Pointers, tty: BorrowedFd<'_>, report: Report) -> io::Result<Pid> {
        let all = signal_set(libc::sigfillset)?;
        let mut before = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: `all` is an initialised set; the thread's mask is written into `before`.
        check_errno(unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &raw const all, before.as_mut_ptr())
        })?;
        // SAFETY: the child runs only `become_program`, which never returns and is
        // async-signal-safe throughout; the parent carries on as if it had called a function.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: this is the child of the fork above, with every signal blocked, and
            // `pointers` point into `self`, which the copied address space still holds.
            unsafe { self.become_program(pointers, tty.as_raw_fd(), report) }
        }
        // Checked before `Pid::from_raw`, which takes -1 for a pid: a `Child` of -1 would reap
        // any child and `kill(-1)` every process of the user.
        let forked = if pid < 0 {
            Err(io::Error::last_os_error())
        } else {
            Pid::from_raw(pid).ok_or_else(|| io::Error::other("fork returned no pid"))
        };
        // SAFETY: `pthread_sigmask` filled `before` with the mask this thread had.
        let before = unsafe { before.assume_init() };
        // SAFETY: `before` is an initialised set.
        let restored = check_errno(unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &raw const before, std::ptr::null_mut())
        });
        if let (Err(e), Ok(pid)) = (restored, &forked) {
            // Nobody would own the child otherwise.
            let _killed = rustix::process::kill_process(*pid, Signal::KILL);
            let _reaped = reap(*pid, WaitOptions::empty());
            return Err(e);
        }
        forked
    }

    /// The child's side of the fork: the working directory, a new session, the tty on fds 0
    /// to 2 as its controlling terminal, nothing else open, the descriptor limit and the signal
    /// state a new program expects, and `execve`, with `execvp`'s fallback to the shell for a file
    /// the kernel cannot run. System calls on memory built before the fork and nothing else. A
    /// step that fails is reported on `report` and ends the child with [`CANNOT_START`].
    ///
    /// # Safety
    ///
    /// Only in the child of a `fork` made with every signal blocked, with `pointers` built
    /// from `self`, and `tty` and a descriptor of `report` above fd 2 ([`above_stdio`]).
    unsafe fn become_program(&self, pointers: &Pointers, tty: c_int, report: Report) -> ! {
        // First, so a directory that is gone leaves the tty as it was.
        // SAFETY: `cwd` is a NUL-terminated string in memory the fork copied.
        if unsafe { libc::chdir(self.cwd.as_ptr()) } == -1 {
            fail(report, Step::Directory, errno());
        }
        if let Err(e) = rustix::process::setsid() {
            fail(report, Step::Session, e.raw_os_error());
        }
        // `tty` is above 2, so each `dup2` makes a new descriptor, which `exec` keeps, and
        // overwrites neither `tty` nor a report pipe.
        for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            // SAFETY: `dup2` on two descriptor numbers; `tty` is open in this process.
            if unsafe { libc::dup2(tty, fd) } == -1 {
                fail(report, Step::Terminal, errno());
            }
        }
        // SAFETY: fd 0 is the tty now, open for the rest of this process.
        let stdin = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
        if let Err(e) = rustix::process::ioctl_tiocsctty(stdin) {
            fail(report, Step::Terminal, e.raw_os_error());
        }
        close_inherited(report.descriptor());
        // SAFETY: `setrlimit` reads the limit, built before the fork, and changes this process's
        // alone; a refusal leaves the limit as it was, which the program can live with.
        let _limited = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const self.descriptors) };
        default_signals();
        // SAFETY: `execve` reads the NUL-terminated path and the null-terminated `argv` and
        // `envp` arrays, all in memory the fork copied; it returns only on failure.
        unsafe {
            libc::execve(self.executable.as_ptr(), pointers.argv.as_ptr(), pointers.envp.as_ptr());
        }
        let cannot = errno();
        if cannot == libc::ENOEXEC {
            // SAFETY: as above, with the shell's own `argv` built before the fork.
            unsafe {
                libc::execve(SHELL.as_ptr(), pointers.shell_argv.as_ptr(), pointers.envp.as_ptr());
            }
        }
        fail(report, Step::Exec, cannot)
    }
}

/// `argv` and `envp` as `execve` takes them, null-terminated, pointing into a [`Launch`], and
/// the `argv` of the shell that runs a file the kernel cannot (`sh file args…`, as `execvp`).
struct Pointers {
    argv: Vec<*const c_char>,
    envp: Vec<*const c_char>,
    shell_argv: Vec<*const c_char>,
}

impl Pointers {
    fn new(launch: &Launch) -> Self {
        let terminated = |strings: &mut dyn Iterator<Item = *const c_char>| {
            strings.chain(std::iter::once(std::ptr::null())).collect()
        };
        let args = || launch.argv.iter().skip(1).map(|s| s.as_ptr());
        Self {
            argv: terminated(&mut launch.argv.iter().map(|s| s.as_ptr())),
            envp: terminated(&mut launch.env.iter().map(|s| s.as_ptr())),
            shell_argv: terminated(
                &mut [c"sh".as_ptr(), launch.executable.as_ptr()].into_iter().chain(args()),
            ),
        }
    }
}

/// How the child fared, told to the parent. The report of a failed step must survive the
/// parent's death (the crash this module exists for), and nothing of it may reach another
/// process: a pipe that is close-on-exec only a moment after it exists, as any pipe on macOS
/// (no `pipe2`), goes to a child that std or tokio starts in that moment, and the pipe never
/// reads as closed while that child lives, so the spawn waits on it (a long `ssh`: forever).
///
/// So on macOS the report is a word in a page shared with the child, and whether it reached
/// its program comes from the kernel: `EVFILT_PROC` on its pid for `NOTE_EXEC` and
/// `NOTE_EXIT`, with `PROC_FLAG_EXEC` and a look at its exit covering an `exec` or exit before
/// the watch began. No descriptor is made, so none can leak. Linux makes a pipe close-on-exec
/// from the start (`pipe2`), and the pipe's end of file is the `exec`.
#[cfg(target_vendor = "apple")]
struct Handshake {
    page: std::ptr::NonNull<std::sync::atomic::AtomicU64>,
}

/// The child's end of a [`Handshake`].
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy)]
struct Report(*const std::sync::atomic::AtomicU64);

/// `PROC_FLAG_EXEC` in `<sys/proc_info.h>`: the process has called `exec`. A child just forked
/// does not have it, whatever its parent has.
#[cfg(target_vendor = "apple")]
const PROC_FLAG_EXEC: u32 = 0x4000;

#[cfg(target_vendor = "apple")]
impl Handshake {
    const PAGE: usize = 4096;

    fn new() -> io::Result<Self> {
        // SAFETY: a fresh anonymous mapping, shared with a child so its report reaches us;
        // zero-filled, so no report is 0.
        let page = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                Self::PAGE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if page == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        std::ptr::NonNull::new(page.cast())
            .map(|page| Self { page })
            .ok_or_else(|| io::Error::other("mmap returned null"))
    }

    const fn child_end(&self) -> Report {
        Report(self.page.as_ptr().cast_const())
    }

    fn report(&self) -> Option<(Step, c_int)> {
        // SAFETY: the page is mapped until `self` drops; a child wrote it before exiting.
        let word = unsafe { self.page.as_ref() }.load(std::sync::atomic::Ordering::Acquire);
        let [_, _, _, step, a, b, c, d] = word.to_be_bytes();
        Step::from_byte(step).map(|step| (step, c_int::from_be_bytes([a, b, c, d])))
    }

    /// `None` once the child runs its program (or ended without a report, as a program may
    /// that exits at once), else the step it reported.
    fn outcome(self, pid: Pid) -> io::Result<Option<(Step, c_int)>> {
        use rustix::event::kqueue::{
            Event, EventFilter, EventFlags, ProcessEvents, kevent, kqueue,
        };
        let watched = ProcessEvents::EXEC | ProcessEvents::EXIT;
        let queue = kqueue()?;
        let watch = Event::new(
            EventFilter::Proc { pid, flags: watched },
            EventFlags::ADD | EventFlags::ONESHOT,
            std::ptr::null_mut(),
        );
        #[cfg(test)]
        hook::at(hook::Moment::Forked, pid);
        // SAFETY: the event names a process, not a descriptor, and carries no user data.
        match unsafe { kevent(&queue, &[watch], &mut Vec::<Event>::new(), None) } {
            Ok(_) => {}
            // Gone before the watch: it has exited.
            Err(rustix::io::Errno::SRCH) => {
                #[cfg(test)]
                hook::caught_up(hook::CaughtUp::Gone);
                return Ok(self.report());
            }
            Err(e) => return Err(e.into()),
        }
        #[cfg(test)]
        hook::at(hook::Moment::Watching, pid);
        if has_exec(pid) {
            #[cfg(test)]
            hook::caught_up(hook::CaughtUp::Exec);
            return Ok(self.report());
        }
        if exited(pid)? {
            #[cfg(test)]
            hook::caught_up(hook::CaughtUp::Exited);
            return Ok(self.report());
        }
        let mut events: Vec<Event> = Vec::with_capacity(1);
        loop {
            // SAFETY: as above; the one event comes back into `events`.
            match unsafe { kevent(&queue, &[], rustix::buffer::spare_capacity(&mut events), None) }
            {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
            let fired = events.iter().any(|event| {
                matches!(event.filter(), EventFilter::Proc { flags, .. } if flags.intersects(watched))
            });
            events.clear();
            if fired {
                #[cfg(test)]
                hook::caught_up(hook::CaughtUp::Event);
                return Ok(self.report());
            }
        }
    }
}

#[cfg(target_vendor = "apple")]
impl Drop for Handshake {
    fn drop(&mut self) {
        // SAFETY: unmaps the page `new` mapped, which nothing here uses after this.
        let _unmapped = unsafe { libc::munmap(self.page.as_ptr().cast(), Self::PAGE) };
    }
}

/// What a test puts into this thread's spawns: a call at two moments of the macOS handshake,
/// to let the child get ahead of the watch, and which way the handshake caught up with it.
#[cfg(all(test, target_vendor = "apple"))]
mod hook {
    use std::cell::{Cell, RefCell};

    use rustix::process::Pid;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Moment {
        /// Forked, the watch not yet made.
        Forked,
        /// Watched, before the looks for an `exec` or an exit that came first.
        Watching,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum CaughtUp {
        /// Gone before the watch.
        Gone,
        /// Had called `exec` before the watch.
        Exec,
        /// Had exited by the look after the watch.
        Exited,
        /// Through the watch's event.
        Event,
    }

    type Hook = Box<dyn FnMut(Moment, Pid)>;

    thread_local! {
        static AT: RefCell<Option<Hook>> = const { RefCell::new(None) };
        static CAUGHT_UP: Cell<Option<CaughtUp>> = const { Cell::new(None) };
    }

    /// Run `hook` at each moment of this thread's spawns from now on.
    pub(super) fn set(hook: impl FnMut(Moment, Pid) + 'static) {
        AT.set(Some(Box::new(hook)));
        CAUGHT_UP.set(None);
    }

    pub(super) fn at(moment: Moment, pid: Pid) {
        AT.with_borrow_mut(|hook| {
            if let Some(hook) = hook {
                hook(moment, pid);
            }
        });
    }

    pub(super) fn caught_up(how: CaughtUp) {
        CAUGHT_UP.set(Some(how));
    }

    /// How this thread's last spawn caught up with its child.
    pub(super) fn last() -> Option<CaughtUp> {
        CAUGHT_UP.get()
    }
}

/// Whether `pid` has called `exec` by now. The short view, since the full one
/// (`PROC_PIDTBSDINFO`) is refused with `EPERM` for a process whose effective user is not
/// ours, which a setuid program (`sudo`, `login`) is right after its `exec`; the short one
/// has no such check.
#[cfg(target_vendor = "apple")]
fn has_exec(pid: Pid) -> bool {
    let mut info = MaybeUninit::<libc::proc_bsdshortinfo>::zeroed();
    let room = c_int::try_from(size_of::<libc::proc_bsdshortinfo>()).unwrap_or(0);
    // SAFETY: `proc_pidinfo` writes at most `room` bytes of the process's details into `info`.
    let written = unsafe {
        libc::proc_pidinfo(
            pid.as_raw_nonzero().get(),
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            room,
        )
    };
    // SAFETY: zeroed, then filled by `proc_pidinfo` when it wrote the whole of it.
    written == room && unsafe { info.assume_init_ref() }.pbsi_flags & PROC_FLAG_EXEC != 0
}

/// Whether `pid`, a child of this process, has exited, leaving it to be reaped.
#[cfg(target_vendor = "apple")]
fn exited(pid: Pid) -> io::Result<bool> {
    use rustix::process::{WaitId, WaitIdOptions};
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    loop {
        match rustix::process::waitid(WaitId::Pid(pid), options) {
            Ok(status) => return Ok(status.is_some()),
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(target_vendor = "apple")]
impl Report {
    /// Tell the parent that `step` failed with `errno`: one store into the shared page.
    fn send(self, step: Step, errno: c_int) {
        let [a, b, c, d] = errno.to_be_bytes();
        let word = u64::from_be_bytes([0, 0, 0, step as u8, a, b, c, d]);
        // SAFETY: the page stays mapped in this child, which only stores into it.
        unsafe { &*self.0 }.store(word, std::sync::atomic::Ordering::Release);
    }

    /// No descriptor to keep open for it.
    #[expect(
        clippy::unused_self,
        reason = "the same call as on Linux, whose report is a descriptor"
    )]
    const fn descriptor(self) -> c_int {
        -1
    }
}

#[cfg(target_os = "linux")]
struct Handshake {
    reader: OwnedFd,
    writer: OwnedFd,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Report(c_int);

#[cfg(target_os = "linux")]
impl Handshake {
    fn new() -> io::Result<Self> {
        let (reader, writer) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        let writer = above_stdio(writer.as_fd())?.unwrap_or(writer);
        Ok(Self { reader, writer })
    }

    fn child_end(&self) -> Report {
        Report(self.writer.as_raw_fd())
    }

    /// `None` once the child runs its program (the pipe closed at its `exec`), or ended
    /// without a report, else the step it reported.
    fn outcome(self, _pid: Pid) -> io::Result<Option<(Step, c_int)>> {
        let Self { reader, writer } = self;
        drop(writer);
        let mut message = [0_u8; 5];
        let read = loop {
            match rustix::io::read(&reader, &mut message) {
                Err(rustix::io::Errno::INTR) => {}
                read => break read?,
            }
        };
        let [step, a, b, c, d] = message;
        match (read, Step::from_byte(step)) {
            (0, _) => Ok(None),
            (5, Some(step)) => Ok(Some((step, c_int::from_be_bytes([a, b, c, d])))),
            _ => Err(io::Error::other("the child's report is garbled")),
        }
    }
}

#[cfg(target_os = "linux")]
impl Report {
    /// Tell the parent that `step` failed with `errno`, best-effort: the parent may be gone,
    /// and with `SIGPIPE` ignored the write then fails quietly.
    fn send(self, step: Step, errno: c_int) {
        // SAFETY: an all-zero `sigaction` is valid: `SIG_DFL`, no flags, an empty mask.
        let mut ignore: libc::sigaction = unsafe { MaybeUninit::zeroed().assume_init() };
        ignore.sa_sigaction = libc::SIG_IGN;
        // SAFETY: `sigaction` is async-signal-safe and changes one disposition of this process.
        let _previous =
            unsafe { libc::sigaction(libc::SIGPIPE, &raw const ignore, std::ptr::null_mut()) };
        let [a, b, c, d] = errno.to_be_bytes();
        let message = [step as u8, a, b, c, d];
        // SAFETY: writes the five bytes of `message`, a local, to a descriptor number; a pipe
        // takes up to `PIPE_BUF` bytes in one piece or not at all.
        let _unsent = unsafe { libc::write(self.0, message.as_ptr().cast(), message.len()) };
    }

    const fn descriptor(self) -> c_int {
        self.0
    }
}

/// The child's steps that can fail, as it reports them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Step {
    Session = 1,
    Terminal = 2,
    Directory = 3,
    Exec = 4,
}

impl Step {
    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Session),
            2 => Some(Self::Terminal),
            3 => Some(Self::Directory),
            4 => Some(Self::Exec),
            _ => None,
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Session => "setsid",
            Self::Terminal => "controlling terminal",
            Self::Directory => "chdir",
            Self::Exec => "exec",
        })
    }
}

/// End the child: `step` failed with `errno`, which it reports as well as it can.
fn fail(report: Report, step: Step, errno: c_int) -> ! {
    report.send(step, errno);
    // SAFETY: `_exit` ends the process at once, running nothing of this one's.
    unsafe { libc::_exit(CANNOT_START) }
}

/// The calling thread's `errno`, read without allocating.
fn errno() -> c_int {
    io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}

/// Close every descriptor above 2 but `keep` (-1 for none), so a program gets the tty and
/// nothing else of this process's. Close-on-exec covers what this process opens itself, but
/// not a descriptor that gets it only a moment after it exists, as macOS gives no atomic way
/// for one received over a socket, accepted (no `accept4`) or piped (no `pipe2`): one that
/// reached a shell in that moment would stay there for good, and a PTY master there keeps its
/// own tile's hangup away.
///
/// macOS has no `closefrom`, and walking up to `OPEN_MAX` (a million here) would cost more
/// than the spawn, so the process lists its own descriptors, a batch at a time on the stack.
#[cfg(target_vendor = "apple")]
fn close_inherited(keep: c_int) {
    let mut listed = [libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; 256];
    let room = c_int::try_from(size_of_val(&listed)).unwrap_or(0);
    loop {
        // SAFETY: `proc_pidinfo` is the `proc_info` system call; it writes at most `room`
        // bytes of this process's descriptors into `listed` and returns how many it wrote.
        let written = unsafe {
            libc::proc_pidinfo(
                rustix::process::getpid().as_raw_nonzero().get(),
                libc::PROC_PIDLISTFDS,
                0,
                listed.as_mut_ptr().cast(),
                room,
            )
        };
        let count = usize::try_from(written)
            .unwrap_or(0)
            .checked_div(size_of::<libc::proc_fdinfo>())
            .unwrap_or(0);
        let mut closed_any = false;
        for fd in listed.iter().take(count).map(|entry| entry.proc_fd) {
            if fd > libc::STDERR_FILENO && fd != keep {
                // SAFETY: closes a descriptor of this process, which nothing here uses again.
                let _closed = unsafe { libc::close(fd) };
                closed_any = true;
            }
        }
        // A full batch may have left more behind; the ones closed are gone from the next.
        if count < listed.len() || !closed_any {
            break;
        }
    }
}
/// Close every descriptor above 2 but `keep`. `close_range` (Linux 5.9) does it in two calls;
/// on an older kernel they fail and close-on-exec is what remains.
#[cfg(target_os = "linux")]
fn close_inherited(keep: c_int) {
    let keep = libc::c_uint::try_from(keep).unwrap_or(libc::c_uint::MAX);
    let ranges = [(3, keep.saturating_sub(1)), (keep.saturating_add(1), libc::c_uint::MAX)];
    for (first, last) in ranges.into_iter().filter(|(first, last)| first <= last) {
        // SAFETY: `close_range` closes this process's descriptors from `first` to `last`,
        // none of which anything here uses again.
        let _closed = unsafe { libc::syscall(libc::SYS_close_range, first, last, 0_u32) };
    }
}

/// Every signal back to its default, then the mask emptied: a shell starts as from a login,
/// whatever this process was started with. Rust ignores `SIGPIPE`, `nohup` ignores `SIGHUP`
/// and a script's `&` ignores `SIGINT` and `SIGQUIT`, and `exec` keeps an ignored signal
/// ignored, so a shell would inherit them: one whose tile closed would outlive its hangup.
/// Caught ones go back first too, so none arriving before the `exec` runs a handler of ours.
fn default_signals() {
    let default = MaybeUninit::<libc::sigaction>::zeroed();
    for signal in 1..SIGNALS {
        // SAFETY: a zeroed `sigaction` is `SIG_DFL` with no flags and an empty mask; for
        // `SIGKILL`, `SIGSTOP` and numbers the C library keeps, it fails and changes nothing.
        let _set = unsafe { libc::sigaction(signal, default.as_ptr(), std::ptr::null_mut()) };
    }
    let mut none = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: `sigemptyset` initialises the set it is pointed at.
    if unsafe { libc::sigemptyset(none.as_mut_ptr()) } == 0 {
        // SAFETY: the set was initialised just above; the child has one thread.
        let _unblocked =
            unsafe { libc::sigprocmask(libc::SIG_SETMASK, none.as_ptr(), std::ptr::null_mut()) };
    }
}

fn c_string(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

/// A close-on-exec copy of `fd` above 2 when it sits on 0, 1 or 2, else `None`. The child
/// `dup2`s the tty onto those three: a report pipe there would be overwritten, and a tty there
/// would keep its close-on-exec through a `dup2` onto itself and close at the `exec`. A process
/// whose standard descriptors were closed hands those numbers out first.
fn above_stdio(fd: BorrowedFd<'_>) -> io::Result<Option<OwnedFd>> {
    if fd.as_raw_fd() > libc::STDERR_FILENO {
        return Ok(None);
    }
    Ok(Some(rustix::io::fcntl_dupfd_cloexec(fd, libc::STDERR_FILENO + 1)?))
}

/// A `pthread_*` result: 0, or the errno itself.
fn check_errno(rc: c_int) -> io::Result<()> {
    if rc == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(rc)) }
}

/// A `sigset_t` initialised by `init` (`sigemptyset` or `sigfillset`).
fn signal_set(
    init: unsafe extern "C" fn(*mut libc::sigset_t) -> c_int,
) -> io::Result<libc::sigset_t> {
    let mut set = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: both initialisers write a whole set into the memory they are pointed at.
    if unsafe { init(set.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: initialised just above.
    Ok(unsafe { set.assume_init() })
}

/// A process started on a PTY. The methods are tokio's `Child`'s, which this replaces.
///
/// A child dropped before it is reaped is reaped in the background.
#[derive(Debug)]
pub struct Child {
    pid: Pid,
    status: Option<ExitStatus>,
}

impl Child {
    /// A child this process image did not start: the image before it did, and handed it on by
    /// running a new build in place (an exec keeps the pid, and with it every child). `None`
    /// for a number no process can have.
    #[must_use]
    pub fn inherited(pid: u32) -> Option<Self> {
        let pid = i32::try_from(pid).ok().and_then(Pid::from_raw)?;
        Some(Self { pid, status: None })
    }

    /// Its pid; `None` once it has been reaped and the number may belong to someone else.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.status.is_none().then(|| self.pid.as_raw_nonzero().get().unsigned_abs())
    }

    /// Its exit status if it has exited, reaping it; `None` while it runs.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = reap(self.pid, WaitOptions::NOHANG)?;
        }
        Ok(self.status)
    }

    /// Wait for it to exit and reap it.
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status = exit_of(self.pid).await?;
        self.status = Some(status);
        Ok(status)
    }

    /// Send it `SIGKILL`, without waiting; nothing once it has been reaped.
    pub fn start_kill(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        // Not reaped, so the pid is still this child's, a zombie at worst.
        rustix::process::kill_process(self.pid, Signal::KILL).map_err(io::Error::from)
    }

    /// Send it `SIGKILL` and wait for it to exit.
    pub async fn kill(&mut self) -> io::Result<()> {
        self.start_kill()?;
        self.wait().await.map(drop)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !matches!(self.try_wait(), Ok(None)) {
            return;
        }
        let pid = self.pid;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(e) = exit_of(pid).await {
                    tracing::warn!(pid = pid.as_raw_nonzero().get(), error = %e, "reaping a child");
                }
            });
        } else {
            let _reaper = std::thread::Builder::new()
                .name("slopty-reap".to_owned())
                .spawn(move || reap(pid, WaitOptions::empty()));
        }
    }
}

/// Wait for `pid`, a child of this process, to exit, and reap it. `SIGCHLD` says some child
/// changed; the listener is in place before the first look, so an exit between the two still
/// wakes it. The kernel signals once the child is a zombie, so the look after it finds it.
async fn exit_of(pid: Pid) -> io::Result<ExitStatus> {
    let mut exits = signal(SignalKind::child())?;
    loop {
        if let Some(status) = reap(pid, WaitOptions::NOHANG)? {
            return Ok(status);
        }
        if exits.recv().await.is_none() {
            return Err(io::Error::other("SIGCHLD is no longer delivered"));
        }
    }
}

/// `waitpid(pid)`, retried on `EINTR`.
fn reap(pid: Pid, options: WaitOptions) -> io::Result<Option<ExitStatus>> {
    loop {
        match rustix::process::waitpid(Some(pid), options) {
            Ok(reaped) => {
                return Ok(reaped.map(|(_, status)| ExitStatus::from_raw(status.as_raw())));
            }
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::terminal::TermSize;

    use super::*;
    use crate::Pty;

    /// The crash this module exists for: a child that cannot start while its parent is gone
    /// (here the parent's side of the handshake is gone, as when the parent died; on Linux the
    /// report pipe then has no reader). std's child aborts on the failed report; this one exits
    /// with 127, before the signal reset (a missing directory) and after it (a missing program).
    #[test]
    fn a_child_that_cannot_start_with_its_parent_gone_exits_quietly() {
        let missing = std::env::temp_dir().join("slopty-no-such-dir-for-spawn");
        let cases = [
            ("missing directory", Path::new("/bin/sh"), missing.as_path()),
            ("missing program", missing.as_path(), Path::new("/")),
        ];
        for (case, executable, cwd) in cases {
            let pty = Pty::open(TermSize::default()).unwrap();
            let launch = Launch::new(executable, ["x"], std::env::vars_os(), cwd).unwrap();
            let handshake = Handshake::new().unwrap();
            let report = handshake.child_end();
            let pid = launch.fork(&Pointers::new(&launch), pty.slave(), report).unwrap();
            drop(handshake);
            let status = reap(pid, WaitOptions::empty()).unwrap().unwrap();
            assert_eq!(status.signal(), None, "{case}: died of a signal: {status:?}");
            assert_eq!(status.code(), Some(CANNOT_START), "{case}: {status:?}");
        }
    }

    /// A child that has exited before the watch is made (the watch then finds no such process)
    /// is caught up with, and its report read.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_child_gone_before_the_watch_is_caught_up_with() {
        let missing = std::env::temp_dir().join("slopty-no-such-dir-for-spawn");
        let pty = Pty::open(TermSize::default()).unwrap();
        let launch = Launch::new(Path::new("/bin/sh"), ["sh"], [], &missing).unwrap();
        hook::set(|moment, pid| {
            if moment == hook::Moment::Forked {
                wait_for_exit(pid);
            }
        });
        let error = launch.spawn(pty.slave()).unwrap_err();
        assert!(error.to_string().starts_with("chdir "), "{error}");
        assert_eq!(hook::last(), Some(hook::CaughtUp::Gone));
    }

    /// A child that exits after the watch is made but before the looks is found by the look
    /// for an exit.
    ///
    /// The hooks run in the parent, so nothing holds the child: one that fails its `chdir`
    /// before the parent adds the watch is `Gone` instead (a loaded runner did, 2026-10-01).
    /// Both are right; the spawn is repeated until the look after the watch has been seen.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_child_that_exits_while_the_watch_is_made_is_caught_up_with() {
        let missing = std::env::temp_dir().join("slopty-no-such-dir-for-spawn");
        let pty = Pty::open(TermSize::default()).unwrap();
        let launch = Launch::new(Path::new("/bin/sh"), ["sh"], [], &missing).unwrap();
        let mut seen = Vec::new();
        while seen.len() < 200 && !seen.contains(&Some(hook::CaughtUp::Exited)) {
            hook::set(|moment, pid| {
                if moment == hook::Moment::Watching {
                    wait_for_exit(pid);
                }
            });
            let error = launch.spawn(pty.slave()).unwrap_err();
            assert!(error.to_string().starts_with("chdir "), "{error}");
            seen.push(hook::last());
        }
        assert!(
            seen.iter()
                .all(|how| matches!(how, Some(hook::CaughtUp::Gone | hook::CaughtUp::Exited))),
            "{seen:?}"
        );
        assert_eq!(seen.last(), Some(&Some(hook::CaughtUp::Exited)), "{seen:?}");
    }

    /// A setuid program that has already run by the time the watch is made (`login`, which
    /// waits on the tty for a name) is seen to have run. The kernel's full view of a process
    /// whose effective user is not ours is refused; a spawn that took the refusal for "not
    /// yet" would wait on an `exec` already past, for as long as `login` or `sudo` waits for
    /// someone to type.
    #[cfg(target_vendor = "apple")]
    #[tokio::test]
    async fn a_setuid_program_that_ran_before_the_watch_is_seen_running() {
        let (done, spawned) = std::sync::mpsc::channel();
        let pty = Pty::open(TermSize::default()).unwrap();
        let master = pty.master().try_clone_to_owned().unwrap();
        let spawner = std::thread::spawn(move || {
            hook::set(move |moment, _| {
                if moment == hook::Moment::Forked {
                    wait_for_output(&master, b"login:");
                }
            });
            let launch =
                Launch::new(Path::new("/usr/bin/login"), ["login"], [], Path::new("/")).unwrap();
            let spawned = launch.spawn(pty.slave());
            done.send((spawned, hook::last())).unwrap();
            pty
        });
        let (spawned, caught_up) = spawned
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the spawn returned while login waited for a name");
        let _pty = spawner.join().unwrap();
        let mut child = spawned.unwrap();
        child.kill().await.unwrap();
        assert_eq!(caught_up, Some(hook::CaughtUp::Exec));
    }

    /// Block until `pid`, a child of this process, has exited, leaving it to be reaped.
    #[cfg(target_vendor = "apple")]
    fn wait_for_exit(pid: Pid) {
        use rustix::process::{WaitId, WaitIdOptions};
        rustix::process::waitid(WaitId::Pid(pid), WaitIdOptions::EXITED | WaitIdOptions::NOWAIT)
            .unwrap()
            .unwrap();
    }

    /// Read `master` until `needle` has come, for at most 10 s.
    #[cfg(target_vendor = "apple")]
    fn wait_for_output(master: &OwnedFd, needle: &[u8]) {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut out = Vec::new();
        let mut buf = [0_u8; 1024];
        let started = std::time::Instant::now();
        while !out.windows(needle.len()).any(|w| w == needle) {
            let waited = started.elapsed();
            assert!(waited < std::time::Duration::from_secs(10), "no {needle:?} in {out:?}");
            let mut fds = [PollFd::new(master, PollFlags::IN)];
            let tick = Timespec { tv_sec: 0, tv_nsec: 100_000_000 };
            if poll(&mut fds, Some(&tick)).unwrap() > 0 {
                let n = rustix::io::read(master, &mut buf).unwrap();
                out.extend_from_slice(&buf[..n]);
            }
        }
    }

    /// With its parent there, the same failures come back from the spawn as errors that name
    /// the step. A missing directory leaves the tty usable. That no child is left behind is
    /// `tests/spawn_process_state.rs`'s to check, in a process with no other children.
    #[tokio::test]
    async fn a_child_that_cannot_start_is_a_spawn_error() {
        let pty = Pty::open(TermSize::default()).unwrap();
        let missing = std::env::temp_dir().join("slopty-no-such-dir-for-spawn");
        let launch = Launch::new(Path::new("/bin/sh"), ["sh"], [], &missing).unwrap();
        let error = launch.spawn(pty.slave()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
        assert!(error.to_string().starts_with("chdir "), "{error}");
        let launch = Launch::new(&missing, ["x"], [], Path::new("/")).unwrap();
        let error = launch.spawn(pty.slave()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{error}");
        assert!(error.to_string().starts_with("exec "), "{error}");
    }
}
