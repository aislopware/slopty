//! What a shell starts with, through the public spawn: from several threads at once while
//! others allocate, hold locks and open PTYs, as in a busy daemon, it gets exactly the tty on
//! fds 0 to 2, its own `PATH`, default signals, and no hang or abort between fork and exec.

#[cfg(test)]
mod spawn {
    use std::os::unix::process::ExitStatusExt as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use slopty_proto::terminal::TermSize;
    use slopty_pty::{Pty, PtyError, PtyMaster, SpawnSpec};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shells_start_while_other_threads_allocate_and_hold_locks() {
        let stop = Arc::new(AtomicBool::new(false));
        let held = Arc::new(parking_lot::Mutex::new(Vec::<Vec<u8>>::new()));
        let churn: Vec<_> = (0..2_usize)
            .map(|seed| {
                let (stop, held) = (Arc::clone(&stop), Arc::clone(&held));
                std::thread::spawn(move || {
                    let mut size = seed + 1;
                    while !stop.load(Ordering::Relaxed) {
                        size = size * 31 % 65_521 + 1;
                        let mut kept = held.lock();
                        kept.push(vec![0_u8; size]);
                        if kept.len() > 64 {
                            kept.clear();
                        }
                    }
                })
            })
            .collect();

        let missing = std::env::temp_dir().join("slopty-no-such-dir-for-spawn");
        // The race under test is spawns against threads that allocate and hold locks, which 16
        // in flight on four workers reach as 120 would. More at once only holds more
        // pseudo-terminals, which the gate's tests lane counts against its budget
        // (`xtask/src/ptys.rs`).
        let in_flight = Arc::new(tokio::sync::Semaphore::new(16));
        let spawns: Vec<_> = (0..120_usize)
            .map(|i| {
                let (missing, in_flight) = (missing.clone(), Arc::clone(&in_flight));
                tokio::spawn(async move {
                    let _slot = in_flight.acquire_owned().await.unwrap();
                    let pty = Pty::open(TermSize::default()).unwrap();
                    let spec = SpawnSpec {
                        command: vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
                        cwd: Some(if i % 4 == 0 { missing } else { std::env::temp_dir() }),
                        env: Vec::new(),
                        size: TermSize::default(),
                    };
                    match pty.spawn(&spec) {
                        Ok(spawned) => {
                            let mut child = spawned.child;
                            let status = child.wait().await.unwrap();
                            assert_eq!(status.code(), Some(7), "shell {i}: {status:?}");
                            assert_ne!(i % 4, 0, "shell {i} started in a missing directory");
                        }
                        Err(e) => {
                            assert_eq!(i % 4, 0, "shell {i}: {e}");
                            assert!(e.to_string().contains("chdir"), "shell {i}: {e}");
                        }
                    }
                })
            })
            .collect();
        let all = async {
            for spawn in spawns {
                spawn.await.unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(60), all).await.expect("no child hung");
        stop.store(true, Ordering::Relaxed);
        for thread in churn {
            thread.join().unwrap();
        }
    }

    fn spec(command: &[&str], env: &[(&str, &str)]) -> SpawnSpec {
        SpawnSpec {
            command: command.iter().map(|&word| word.to_owned()).collect(),
            cwd: Some(std::env::temp_dir()),
            env: env.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect(),
            size: TermSize::default(),
        }
    }

    /// Everything the program on `pty` writes, until it closes the tty.
    async fn output(pty: Pty) -> String {
        let master = PtyMaster::new(pty.into_master()).unwrap();
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

    /// A shell holds fds 0, 1 and 2 and nothing else of the daemon's: not a descriptor the
    /// daemon holds without close-on-exec (as a socket accepted on macOS, which has no
    /// `accept4`, is for a moment), and not the master of a PTY another thread opens at the
    /// same time. A master that reached a shell so would keep its own tile's hangup away.
    /// The descriptors are read from outside once `cat` has echoed a line, past the loader's
    /// own start-up files, since a program listing its own would open more.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_descriptor_of_the_daemon_leaks_into_a_shell() {
        let inheritable = rustix::io::dup(std::fs::File::open("/dev/null").unwrap()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let openers: Vec<_> = std::iter::repeat_with(|| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    drop(Pty::open(TermSize::default()).unwrap());
                }
            })
        })
        .take(3)
        .collect();
        for round in 0..100 {
            let pty = Pty::open(TermSize::default()).unwrap();
            let mut child = pty.spawn(&spec(&["/bin/cat"], &[])).unwrap().child;
            let master = PtyMaster::new(pty.into_master()).unwrap();
            master.write_all(b"ping\n").await.unwrap();
            read_until(&master, |out| out.matches("ping").count() == 2).await;
            let held = descriptors(child.id().unwrap());
            // Reaped by looking, not on `SIGCHLD`, which ThreadSanitizer holds back while the
            // runtime waits in `kevent`: this test also runs under it.
            child.start_kill().unwrap();
            while child.try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(held, [0, 1, 2], "round {round}, {inheritable:?} inheritable");
        }
        stop.store(true, Ordering::Relaxed);
        for opener in openers {
            opener.join().unwrap();
        }
    }

    /// Read `master` until what came satisfies `enough`.
    async fn read_until(master: &PtyMaster, enough: impl Fn(&str) -> bool) {
        let mut out = String::new();
        let mut buf = [0_u8; 1024];
        while !enough(&out) {
            let read = tokio::time::timeout(Duration::from_secs(10), master.read(&mut buf));
            let n = read.await.expect("output came").unwrap();
            assert_ne!(n, 0, "the program closed its tty after {out:?}");
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
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

    /// A bare program name is looked up on the child's own `PATH`, as `execvp`, std's
    /// `Command` and `env PATH=… program` all do: a session that puts a directory first gets
    /// that directory's program.
    #[tokio::test]
    async fn a_bare_program_is_found_on_the_path_of_the_child() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let shadow = dir.path().join("true");
        std::fs::write(&shadow, "#!/bin/sh\necho shadowed-true\n").unwrap();
        std::fs::set_permissions(&shadow, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:/usr/bin:/bin", dir.path().display());

        let pty = Pty::open(TermSize::default()).unwrap();
        let mut child = pty.spawn(&spec(&["true"], &[("PATH", &path)])).unwrap().child;
        let said = output(pty).await;
        assert!(said.contains("shadowed-true"), "ran the daemon's `true`: {said:?}");
        assert!(child.wait().await.unwrap().success());
    }

    /// A relative `PATH` entry, or an empty one (the current directory), is taken from the
    /// directory the child starts in, as its own `execvp` would take it, not from the daemon's.
    #[tokio::test]
    async fn a_relative_path_entry_is_taken_from_the_directory_of_the_child() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("relbin")).unwrap();
        for (file, says) in [("relbin/true", "relative-true"), ("true", "cwd-true")] {
            let script = dir.path().join(file);
            std::fs::write(&script, format!("#!/bin/sh\necho {says}\n")).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        for (path, says) in
            [("relbin:/usr/bin:/bin", "relative-true"), (":/usr/bin:/bin", "cwd-true")]
        {
            let pty = Pty::open(TermSize::default()).unwrap();
            let spec = SpawnSpec {
                cwd: Some(dir.path().to_owned()),
                ..spec(&["true"], &[("PATH", path)])
            };
            let mut child = pty.spawn(&spec).unwrap().child;
            let said = output(pty).await;
            assert!(said.contains(says), "PATH={path} ran another `true`: {said:?}");
            assert!(child.wait().await.unwrap().success());
        }
    }

    /// A file the kernel cannot run (no `#!` line, not a binary) runs in `/bin/sh`, as
    /// `execvp` runs it.
    #[tokio::test]
    async fn a_script_without_an_interpreter_line_runs_in_the_shell() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("plain-script");
        std::fs::write(&script, "echo no-interpreter-line\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pty = Pty::open(TermSize::default()).unwrap();
        let mut child = pty.spawn(&spec(&[script.to_str().unwrap()], &[])).unwrap().child;
        let said = output(pty).await;
        assert!(said.contains("no-interpreter-line"), "{said:?}");
        assert!(child.wait().await.unwrap().success());
    }

    /// A shell starts with every signal at its default, whatever the daemon was started with:
    /// under `nohup` it ignores `SIGHUP`, and a shell that inherited that would outlive the
    /// hangup of its tile closing.
    #[tokio::test]
    async fn a_shell_starts_with_every_signal_at_its_default() {
        // SAFETY: sets one disposition of this test process, as `nohup` would have.
        let previous = unsafe { libc::signal(libc::SIGHUP, libc::SIG_IGN) };
        let pty = Pty::open(TermSize::default()).unwrap();
        let spawned = pty.spawn(&spec(&["/bin/sh", "-c", "kill -HUP $$; echo survived"], &[]));
        // SAFETY: puts back the disposition read above.
        let _ignoring = unsafe { libc::signal(libc::SIGHUP, previous) };
        let mut child = spawned.unwrap().child;
        // Read, since macOS holds an exiting session leader until its tty output is read.
        let said = output(pty).await;
        let status = child.wait().await.unwrap();
        assert_eq!(status.signal(), Some(libc::SIGHUP), "the hangup was ignored: {said:?}");
    }

    /// A variable that cannot be passed to `execve` (a NUL inside) fails the spawn, before
    /// any fork.
    #[test]
    fn an_environment_with_a_nul_is_refused() {
        let pty = Pty::open(TermSize::default()).unwrap();
        let error = pty.spawn(&spec(&["/usr/bin/true"], &[("SLOPTY_NUL", "a\0b")])).unwrap_err();
        let PtyError::Os { source, .. } = &error else { panic!("{error}") };
        assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput, "{error}");
    }
}
