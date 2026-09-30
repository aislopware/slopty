//! Spawning from a process in a state a daemon can end up in: its standard descriptors closed,
//! at the user's process limit, killed between a fork and the child's report, holding a
//! descriptor another process was handed, or holding hundreds of descriptors. Each changes
//! something process-wide, so they run one after another in one test, the only one this binary
//! runs by default.

#[cfg(test)]
mod process_state {
    use std::os::fd::{AsRawFd as _, BorrowedFd, IntoRawFd as _, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::path::{Path, PathBuf};
    use std::sync::Once;
    use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU8, AtomicU32, Ordering};
    use std::time::Duration;

    use slopty_proto::terminal::TermSize;
    use slopty_pty::{Pty, PtyError, PtyMaster, SpawnSpec};
    use tokio::runtime::Runtime;

    fn spec(command: &[&str], cwd: &Path) -> SpawnSpec {
        SpawnSpec {
            command: command.iter().map(|&word| word.to_owned()).collect(),
            cwd: Some(cwd.to_owned()),
            env: Vec::new(),
            size: TermSize::default(),
        }
    }

    fn missing_dir() -> PathBuf {
        std::env::temp_dir().join("slopty-no-such-dir-for-spawn")
    }

    /// The exit an `exec` that failed ends in, as a shell reports a program it cannot run.
    const CANNOT_START: i32 = 127;

    #[test]
    fn spawns_hold_up_in_a_process_in_a_bad_state() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        failed_spawns_leave_no_child();
        standard_descriptors_closed(&runtime);
        fork_fails();
        parent_gone_before_the_report(&runtime);
        #[cfg(target_vendor = "apple")]
        no_pipe_comes_back_for_another_process_to_hold(&runtime);
        descriptors_past_a_batch_are_closed(&runtime);
    }

    /// A spawn that fails reaps its child: none is left behind, so none sits a zombie. Checked
    /// first, while this process has no other child.
    fn failed_spawns_leave_no_child() {
        let pty = Pty::open(TermSize::default()).unwrap();
        pty.spawn(&spec(&["/bin/sh"], &missing_dir())).map(|spawned| spawned.child).unwrap_err();
        let missing = spec(&["/nonexistent/slopty-missing"], Path::new("/"));
        pty.spawn(&missing).map(|spawned| spawned.child).unwrap_err();
        let left = rustix::process::waitpid(None, rustix::process::WaitOptions::NOHANG);
        assert_eq!(left.unwrap_err(), rustix::io::Errno::CHILD, "every child was reaped");
    }

    /// With fds 0 to 2 closed, a PTY opened then sits on them (master 0, slave 1), and so does
    /// the report pipe of a spawn made then (Linux; macOS reports through memory). The shell still
    /// gets the tty on all three: a `dup2` of the slave onto its own number would keep its
    /// close-on-exec and leave the shell without a stdout. A program that cannot run is still
    /// an error, where the child's `dup2` onto the report pipe would have sent its report into
    /// the tty and the spawn would have looked like a success.
    fn standard_descriptors_closed(runtime: &Runtime) {
        let closed = ClosedStdio::close();
        let pty = Pty::open(TermSize::default()).unwrap();
        let spawned =
            pty.spawn(&spec(&["/bin/sh", "-c", "echo out; echo err >&2"], Path::new("/")));
        let master = rustix::io::fcntl_dupfd_cloexec(pty.into_master(), 3).unwrap();
        drop(closed);
        let mut child = spawned.unwrap().child;
        let said = runtime.block_on(output(master));
        assert_eq!(said.lines().collect::<Vec<_>>(), ["out", "err"], "{said:?}");
        assert!(runtime.block_on(child.wait()).unwrap().success());

        let pty = Pty::open(TermSize::default()).unwrap();
        let closed = ClosedStdio::close();
        let spawned = pty.spawn(&spec(&["/nonexistent/slopty-missing"], Path::new("/")));
        drop(closed);
        let error = spawned.map(|spawned| spawned.child).unwrap_err();
        assert!(error.to_string().contains("exec /nonexistent/slopty-missing"), "{error}");
    }

    /// A `fork` that fails (the user at their process limit) is a spawn error. rustix takes the
    /// -1 it returns for a pid, and a `Child` of -1 would reap any child and `kill(-1)` every
    /// process of the user.
    fn fork_fails() {
        let pty = Pty::open(TermSize::default()).unwrap();
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: reads this process's limit into `limit`.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &raw mut limit) }, 0);
        let one = libc::rlimit { rlim_cur: 1, rlim_max: limit.rlim_max };
        // SAFETY: lowers the soft limit only, which any process may.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &raw const one) }, 0);
        let spawned = pty.spawn(&spec(&["/usr/bin/true"], Path::new("/")));
        // macOS clamps the hard limit it was given to the per-user maximum, and raising a
        // hard limit takes root, so the soft one goes back under the hard one it has now.
        let mut lowered = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: reads this process's limit into `lowered`.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &raw mut lowered) }, 0);
        let back = libc::rlimit {
            rlim_cur: limit.rlim_cur.min(lowered.rlim_max),
            rlim_max: lowered.rlim_max,
        };
        // SAFETY: raises the soft limit, no higher than the hard one.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &raw const back) }, 0);
        let error = spawned.map(|spawned| spawned.child).unwrap_err();
        let PtyError::Os { source, .. } = &error else { panic!("{error}") };
        assert_eq!(source.raw_os_error(), Some(libc::EAGAIN), "{error}");
    }

    /// The crash the fork of our own exists for, made certain rather than timed: every pipe
    /// loses its readers after the fork and before the child reports, as when the daemon is
    /// killed mid-spawn. A child that cannot `chdir` (its directory gone) or `exec` exits 127
    /// every time; std's aborted (`std_command_aborts_when_its_parent_is_gone`). On Linux the
    /// report goes down a pipe, so it is lost and the spawn sees only the exit. On macOS it
    /// is a word in shared memory, which nothing here can take away, so the spawn names the
    /// step (`a_child_that_cannot_start_with_its_parent_gone_exits_quietly` sees that child's
    /// exit).
    fn parent_gone_before_the_report(runtime: &Runtime) {
        let harness = ForkHarness::install();
        for round in 0..300 {
            let pty = Pty::open(TermSize::default()).unwrap();
            let (cwd, program, step) = if round % 2 == 0 {
                (missing_dir(), "/bin/sh", "chdir ")
            } else {
                (PathBuf::from("/"), "/nonexistent/slopty-missing", "exec ")
            };
            let spawned = harness.during(DROP_READERS, || pty.spawn(&spec(&[program], &cwd)));
            match spawned {
                Ok(spawned) => {
                    let mut child = spawned.child;
                    let status = runtime.block_on(child.wait()).unwrap();
                    assert_eq!(status.signal(), None, "round {round}, {program}: {status:?}");
                    assert_eq!(status.code(), Some(CANNOT_START), "round {round}: {status:?}");
                }
                Err(e) => assert!(e.to_string().contains(step), "round {round}: {e}"),
            }
        }
    }

    /// A guard against a report pipe coming back on macOS. std and tokio start commands with
    /// `posix_spawn`, which copies every descriptor not yet close-on-exec, and a macOS pipe is
    /// that for a moment after it exists: a child started then (a long `ssh` the worker runs)
    /// held a report pipe's write end, the pipe never read as closed, and the spawn waited as
    /// long as that child lived. The spawn now makes no pipe on macOS, so the copies held here
    /// at the fork are none, and this passes by construction. With a pipe back it failed.
    #[cfg(target_vendor = "apple")]
    fn no_pipe_comes_back_for_another_process_to_hold(runtime: &Runtime) {
        let harness = ForkHarness::install();
        let (done, spawned) = std::sync::mpsc::channel();
        let spawner = std::thread::spawn(move || {
            let pty = Pty::open(TermSize::default()).unwrap();
            let spawned = harness
                .during(HOLD_WRITERS, || pty.spawn(&spec(&["/bin/sleep", "30"], Path::new("/"))));
            done.send(spawned.map(|spawned| spawned.child)).unwrap();
        });
        let returned = spawned.recv_timeout(Duration::from_secs(5));
        release_held_writers();
        let mut child = returned.expect("the spawn returned while the write end was held").unwrap();
        spawner.join().unwrap();
        runtime.block_on(child.kill()).unwrap();
    }

    /// Every descriptor the daemon holds without close-on-exec stays out of the shell, however
    /// many: past the batch of 256 the child lists at a time, and far above it (fd 5000).
    fn descriptors_past_a_batch_are_closed(runtime: &Runtime) {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: reads this process's limit into `limit`.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) }, 0);
        let room = libc::rlimit { rlim_cur: limit.rlim_max.min(8192), rlim_max: limit.rlim_max };
        // SAFETY: sets the soft limit no higher than the hard one.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const room) }, 0);
        let null = std::fs::File::open("/dev/null").unwrap();
        // `dup` makes a descriptor without close-on-exec.
        let inheritable: Vec<OwnedFd> =
            std::iter::repeat_with(|| rustix::io::dup(&null).unwrap()).take(300).collect();
        // SAFETY: `dup2` of an open descriptor onto a number nothing here uses.
        assert_eq!(unsafe { libc::dup2(null.as_raw_fd(), 5000) }, 5000);

        let pty = Pty::open(TermSize::default()).unwrap();
        let mut child = pty.spawn(&spec(&["/bin/cat"], Path::new("/"))).unwrap().child;
        let held = runtime.block_on(async {
            let master = PtyMaster::new(pty.into_master()).unwrap();
            master.write_all(b"ping\n").await.unwrap();
            // The echo of the tty, then `cat`'s own: it runs, past the loader's own files.
            let mut out = String::new();
            let mut buf = [0_u8; 1024];
            while out.matches("ping").count() < 2 {
                let read = tokio::time::timeout(Duration::from_secs(10), master.read(&mut buf));
                let n = read.await.expect("cat echoed").unwrap();
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
            descriptors(child.id().unwrap())
        });
        runtime.block_on(child.kill()).unwrap();
        // SAFETY: closes the number `dup2`'d above.
        let _closed = unsafe { libc::close(5000) };
        drop(inheritable);
        assert_eq!(held, [0, 1, 2]);
    }

    /// The descriptors process `pid` holds, in order.
    #[cfg(target_vendor = "apple")]
    fn descriptors(pid: u32) -> Vec<i32> {
        let mut fds = [libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; 256];
        let room = i32::try_from(size_of_val(&fds)).unwrap();
        // SAFETY: `proc_pidinfo` writes at most `room` bytes into `fds` and returns how many.
        let filled = unsafe {
            libc::proc_pidinfo(
                i32::try_from(pid).unwrap(),
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                room,
            )
        };
        let count =
            usize::try_from(filled).unwrap().checked_div(size_of::<libc::proc_fdinfo>()).unwrap();
        fds[..count].iter().map(|fd| fd.proc_fd).collect()
    }

    #[cfg(not(target_vendor = "apple"))]
    fn descriptors(pid: u32) -> Vec<i32> {
        let mut fds: Vec<i32> = std::fs::read_dir(format!("/proc/{pid}/fd"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_str().unwrap().parse().unwrap())
            .collect();
        fds.sort_unstable();
        fds
    }

    /// The control for [`parent_gone_before_the_report`]: the same harness around std's
    /// `Command` with the `pre_exec` the spawn used to take, whose child aborts. Run with
    /// `cargo nextest run -p slopty-pty --test spawn_process_state --run-ignored only`.
    #[test]
    #[ignore = "a control: std's child aborts, and macOS writes a crash report each run"]
    fn std_command_aborts_when_its_parent_is_gone() {
        let harness = ForkHarness::install();
        let pty = Pty::open(TermSize::default()).unwrap();
        let slave = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOCTTY)
                .open(pty.slave_path())
                .unwrap()
        };
        let mut command = std::process::Command::new("/bin/sh");
        command.current_dir(missing_dir()).stdin(slave()).stdout(slave()).stderr(slave());
        let controlling_tty = || -> std::io::Result<()> {
            rustix::process::setsid()?;
            // SAFETY: `Command` has put the slave on fd 0 by now.
            rustix::process::ioctl_tiocsctty(unsafe { BorrowedFd::borrow_raw(0) })?;
            Ok(())
        };
        // SAFETY: setsid and TIOCSCTTY, raw system calls, as the old spawn did.
        unsafe {
            command.pre_exec(controlling_tty);
        }
        let mut child = harness.during(DROP_READERS, || command.spawn()).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGABRT), "{status:?}");
    }

    /// Everything the program on `master` writes, until it closes the tty.
    async fn output(master: OwnedFd) -> String {
        let master = PtyMaster::new(master).unwrap();
        let mut out = Vec::new();
        let mut buf = [0_u8; 4096];
        loop {
            let read = tokio::time::timeout(Duration::from_secs(10), master.read(&mut buf));
            match read.await.expect("the program closed its tty").unwrap() {
                0 => return String::from_utf8_lossy(&out).into_owned(),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    /// This process's fds 0 to 2, closed, and put back on drop.
    struct ClosedStdio([OwnedFd; 3]);

    impl ClosedStdio {
        fn close() -> Self {
            let saved = [0, 1, 2].map(|fd| {
                // SAFETY: 0 to 2 are open in a test process.
                let fd = unsafe { BorrowedFd::borrow_raw(fd) };
                rustix::io::fcntl_dupfd_cloexec(fd, 3).unwrap()
            });
            for fd in 0..3 {
                // SAFETY: closes a number nothing owns; std writes to 0 to 2 without owning them.
                let _closed = unsafe { libc::close(fd) };
            }
            Self(saved)
        }
    }

    impl Drop for ClosedStdio {
        fn drop(&mut self) {
            for (fd, saved) in (0..).zip(&self.0) {
                // SAFETY: `dup2` of an open descriptor onto a standard number.
                let _restored = unsafe { libc::dup2(saved.as_raw_fd(), fd) };
            }
        }
    }

    /// Fork handlers that, while armed, stand in for what can happen to a spawn's pipes: with
    /// [`DROP_READERS`] the read end of every pipe becomes `/dev/null` in the parent right after
    /// a fork and then in the child before it goes on, so a pipe the child reports on has no
    /// reader left when it writes; with [`HOLD_WRITERS`] the parent keeps a copy of every
    /// pipe's write end, as a process forked in the same moment would.
    #[derive(Clone, Copy)]
    struct ForkHarness(&'static AtomicU32);

    const DROP_READERS: u8 = 1;
    const HOLD_WRITERS: u8 = 2;

    static MODE: AtomicU8 = AtomicU8::new(0);
    static DEV_NULL: AtomicI32 = AtomicI32::new(-1);
    /// A word in memory shared across the fork: 1 once the parent has let go of its readers.
    static RELEASED: AtomicPtr<AtomicU32> = AtomicPtr::new(std::ptr::null_mut());
    /// The write ends [`HOLD_WRITERS`] keeps, -1 for none.
    static HELD: [AtomicI32; 16] = [const { AtomicI32::new(-1) }; 16];

    impl ForkHarness {
        fn install() -> Self {
            static INSTALL: Once = Once::new();
            INSTALL.call_once(|| {
                let null = std::fs::File::open("/dev/null").unwrap();
                DEV_NULL.store(null.into_raw_fd(), Ordering::Relaxed);
                // SAFETY: a fresh anonymous shared page, never unmapped, which a fork shares
                // rather than copies.
                let page = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        4096,
                        libc::PROT_READ | libc::PROT_WRITE,
                        libc::MAP_SHARED | libc::MAP_ANON,
                        -1,
                        0,
                    )
                };
                assert_ne!(page, libc::MAP_FAILED);
                RELEASED.store(page.cast(), Ordering::Relaxed);
                // SAFETY: registers two handlers that only make async-signal-safe calls.
                let rc = unsafe {
                    libc::pthread_atfork(None, Some(parent_after_fork), Some(child_after_fork))
                };
                assert_eq!(rc, 0);
            });
            Self(released())
        }

        fn during<T>(self, mode: u8, spawn: impl FnOnce() -> T) -> T {
            self.0.store(0, Ordering::SeqCst);
            MODE.store(mode, Ordering::SeqCst);
            let spawned = spawn();
            MODE.store(0, Ordering::SeqCst);
            spawned
        }
    }

    fn released() -> &'static AtomicU32 {
        // SAFETY: set once by `install` to a page that is never unmapped.
        unsafe { &*RELEASED.load(Ordering::Relaxed) }
    }

    /// Close what [`HOLD_WRITERS`] kept.
    #[cfg(target_vendor = "apple")]
    fn release_held_writers() {
        for slot in &HELD {
            let fd = slot.swap(-1, Ordering::SeqCst);
            if fd >= 0 {
                // SAFETY: closes a copy the fork handler made and nothing else owns.
                let _closed = unsafe { libc::close(fd) };
            }
        }
    }

    extern "C" fn parent_after_fork() {
        match MODE.load(Ordering::SeqCst) {
            DROP_READERS => {
                for_each_pipe(libc::O_RDONLY, drop_reader);
                released().store(1, Ordering::SeqCst);
            }
            HOLD_WRITERS => for_each_pipe(libc::O_WRONLY, hold_writer),
            _ => {}
        }
    }

    extern "C" fn child_after_fork() {
        if MODE.load(Ordering::SeqCst) == DROP_READERS {
            while released().load(Ordering::SeqCst) == 0 {
                // SAFETY: a system call with no arguments.
                let _yielded = unsafe { libc::sched_yield() };
            }
            for_each_pipe(libc::O_RDONLY, drop_reader);
        }
    }

    /// The read end at `fd` becomes `/dev/null`, which reads as end of file.
    fn drop_reader(fd: i32) {
        // SAFETY: `dup2` of an open descriptor onto another.
        let _replaced = unsafe { libc::dup2(DEV_NULL.load(Ordering::Relaxed), fd) };
    }

    /// A copy of the write end at `fd` is kept in [`HELD`].
    fn hold_writer(fd: i32) {
        // The scan goes on upwards and meets the copies made here.
        if HELD.iter().any(|slot| slot.load(Ordering::SeqCst) == fd) {
            return;
        }
        let Some(slot) = HELD.iter().find(|slot| slot.load(Ordering::SeqCst) < 0) else {
            return;
        };
        // SAFETY: `F_DUPFD_CLOEXEC` on an open descriptor makes a new one.
        slot.store(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) }, Ordering::SeqCst);
    }

    /// `each` for every end of a pipe above fd 2 open for `access` (`O_RDONLY` or `O_WRONLY`).
    fn for_each_pipe(access: i32, each: fn(i32)) {
        for fd in 3..1024 {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
            // SAFETY: `fstat` fills `stat` or fails for a number that is not open.
            if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
                continue;
            }
            // SAFETY: zeroed, then filled by `fstat`.
            let mode = unsafe { stat.assume_init_ref() }.st_mode;
            // SAFETY: `F_GETFL` on an open descriptor.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if mode & libc::S_IFMT == libc::S_IFIFO && flags & libc::O_ACCMODE == access {
                each(fd);
            }
        }
    }
}
