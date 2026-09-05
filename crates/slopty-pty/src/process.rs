//! The foreground process of a pseudo-terminal.
//!
//! A tty always names one process group as its foreground: the one the kernel delivers ⌃C to
//! and the one whose output the human is reading. `tcgetpgrp` on the master returns it, and
//! its leader is the program the shell most recently started — `claude`, `vim`, `cargo`.
//! Slopty reads it on a timer to attribute agent sessions nobody registered hooks for; the
//! decision of what counts as an agent is `slopty_agent::detect`, which is a pure function
//! over the name and `argv` this module produces.
//!
//! Three facts come back, all of them from the process table of a child of this same user:
//! the executable's name and start time (`proc_pidinfo` `PROC_PIDTBSDINFO`), the command line
//! (the `KERN_PROCARGS2` sysctl) and the current working directory (`proc_pidinfo`
//! `PROC_PIDVNODEPATHINFO`). Every one of them is best-effort: a process that exits between
//! two calls simply reports less, never an error.

use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::SystemTime;

/// What the platform can say about the program in the foreground of a tty.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Foreground {
    /// Process id of the group leader.
    pub pid: i32,
    /// Executable name (the last component of its path).
    pub name: String,
    /// The command line, `argv[0]` first; empty when it could not be read.
    pub argv: Vec<String>,
    /// The process's working directory, when it could be read.
    pub cwd: Option<PathBuf>,
    /// When the process started, when it could be read.
    pub started: Option<SystemTime>,
}

/// The program in the foreground of the tty behind `fd` (a PTY master), or `None` when the
/// tty has no foreground group or the platform would not say.
#[must_use]
pub fn foreground(fd: impl AsFd) -> Option<Foreground> {
    let pid = rustix::termios::tcgetpgrp(fd).ok()?;
    imp::describe(pid.as_raw_nonzero().get())
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    use super::Foreground;

    /// `KERN_PROCARGS2` output kept; a command line past this is not one we need to read.
    const ARGS_MAX: usize = 64 * 1024;

    /// Everything the process table will say about `pid`.
    pub fn describe(pid: i32) -> Option<Foreground> {
        let info = bsd_info(pid)?;
        let name = c_string(&info.pbi_name).or_else(|| c_string(&info.pbi_comm))?;
        let started = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(info.pbi_start_tvsec))
            .and_then(|at| at.checked_add(Duration::from_micros(info.pbi_start_tvusec)));
        Some(Foreground { pid, name, argv: argv(pid).unwrap_or_default(), cwd: cwd(pid), started })
    }

    /// `proc_pidinfo(PROC_PIDTBSDINFO)`: name, start time.
    fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = i32::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `proc_pidinfo` writes at most `size` bytes into the buffer, which is exactly
        // one `proc_bsdinfo`; it returns the number of bytes written, and anything short of the
        // whole structure means the call did not fill it in.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast::<libc::c_void>(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: the call above filled the whole structure, and `proc_bsdinfo` is plain data.
        Some(unsafe { info.assume_init() })
    }

    /// `proc_pidinfo(PROC_PIDVNODEPATHINFO)`: the process's working directory.
    fn cwd(pid: i32) -> Option<PathBuf> {
        let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let size = i32::try_from(size_of::<libc::proc_vnodepathinfo>()).ok()?;
        // SAFETY: as `bsd_info`: the buffer is one whole `proc_vnodepathinfo` and the call
        // writes at most `size` bytes into it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast::<libc::c_void>(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: the call filled the whole structure, which is plain data.
        let info = unsafe { info.assume_init() };
        // `vip_path` is a `[c_char; MAXPATHLEN]` libc spells as a square array; it is one
        // contiguous NUL-terminated buffer either way.
        let path = info.pvi_cdir.vip_path;
        // SAFETY: `path` is a contiguous array of `MAXPATHLEN` `c_char`s inside a structure we
        // own, so reading it as a byte slice of the same length borrows only initialised memory.
        let bytes =
            unsafe { std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), size_of_val(&path)) };
        let text = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
        (!text.is_empty()).then(|| PathBuf::from(text))
    }

    /// The `KERN_PROCARGS2` sysctl: `argc`, the executable path, then `argc` NUL-terminated
    /// arguments (with padding NULs between the path and the first one).
    fn argv(pid: i32) -> Option<Vec<String>> {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut len: libc::size_t = 0;
        // SAFETY: a `sysctl` with a null `oldp` only writes the required size into `oldlenp`;
        // `mib` is three `c_int`s long, as its `namelen` says.
        let sized = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if sized != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0_u8; len.min(ARGS_MAX)];
        let mut got: libc::size_t = buf.len();
        // SAFETY: `oldp` points at `got` writable bytes of `buf` and `oldlenp` says so; the
        // call writes at most that many and updates `got` with what it wrote.
        let read = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                &raw mut got,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return None;
        }
        buf.truncate(got);
        Some(parse_procargs(&buf))
    }

    /// Split a `KERN_PROCARGS2` buffer into its arguments.
    fn parse_procargs(buf: &[u8]) -> Vec<String> {
        let Some(argc) =
            buf.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_ne_bytes)
        else {
            return Vec::new();
        };
        let rest = buf.get(4..).unwrap_or_default();
        // The executable path comes first and is not an argument; the arguments start after the
        // NULs that pad it.
        let after_exe = match rest.iter().position(|b| *b == 0) {
            Some(end) => rest.get(end..).unwrap_or_default(),
            None => return Vec::new(),
        };
        let args = after_exe
            .iter()
            .position(|b| *b != 0)
            .map_or(&[][..], |start| after_exe.get(start..).unwrap_or_default());
        args.split(|b| *b == 0)
            .take(usize::try_from(argc).unwrap_or(0))
            .map(|word| String::from_utf8_lossy(word).into_owned())
            .collect()
    }

    /// A NUL-terminated `c_char` field as a `String`, empty ones dropped.
    fn c_string(field: &[libc::c_char]) -> Option<String> {
        // SAFETY: `field` is a contiguous array of `c_char` inside a structure we own; reading
        // the same length as bytes borrows only initialised memory.
        let bytes = unsafe { std::slice::from_raw_parts(field.as_ptr().cast::<u8>(), field.len()) };
        let text = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
        (!text.is_empty()).then(|| text.to_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn procargs_are_split_after_the_executable_path() {
            let mut buf = 3_u32.to_ne_bytes().to_vec();
            buf.extend_from_slice(b"/usr/local/bin/node\0\0\0");
            buf.extend_from_slice(b"node\0/opt/claude-code/cli.js\0--resume\0");
            assert_eq!(parse_procargs(&buf), ["node", "/opt/claude-code/cli.js", "--resume"]);
            // A truncated or empty buffer is no arguments, never a panic.
            assert!(parse_procargs(&[]).is_empty());
            assert!(parse_procargs(&1_u32.to_ne_bytes()).is_empty());
            assert!(parse_procargs(b"\x01\0\0\0nonul").is_empty());
        }

        #[test]
        fn this_process_describes_itself() {
            let me = std::process::id();
            let pid = i32::try_from(me).expect("pid fits");
            let info = describe(pid).expect("the test binary is in the process table");
            assert_eq!(info.pid, pid);
            assert!(!info.name.is_empty(), "{info:?}");
            assert!(!info.argv.is_empty(), "{info:?}");
            assert_eq!(info.cwd.as_deref(), Some(std::env::current_dir().expect("cwd").as_path()));
            assert!(info.started.is_some_and(|at| at < SystemTime::now()), "{info:?}");
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Foreground;

    /// No process table to read here; the caller treats `None` as "the platform would not say".
    pub const fn describe(_pid: i32) -> Option<Foreground> {
        None
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;

    use super::*;
    use crate::Pty;

    #[tokio::test]
    async fn a_ptys_foreground_process_is_the_program_it_runs() {
        let size = TermSize { cols: 40, rows: 10, metrics: CellMetrics::default() };
        let pty = Pty::open(size).expect("openpt");
        let mut child = pty
            .spawn(&crate::SpawnSpec {
                // The trailing `:` keeps the shell from exec'ing the last command over itself,
                // so the tty's foreground leader stays the program we spawned.
                command: vec!["/bin/sh".into(), "-c".into(), "echo ready; sleep 30; :".into()],
                cwd: Some(std::env::temp_dir()),
                env: Vec::new(),
                size,
            })
            .expect("spawn");
        let master = crate::PtyMaster::new(pty.into_master()).expect("master");
        // Wait until the child has run far enough to own the tty.
        let mut buf = [0_u8; 64];
        let n = master.read(&mut buf).await.expect("read");
        assert!(n > 0);

        let fg = foreground(master.as_fd()).expect("a foreground process");
        // The executable's name is not `argv[0]`: `/bin/sh` on macOS is bash.
        assert_eq!(fg.name, "bash", "{fg:?}");
        assert_eq!(fg.argv.first().map(String::as_str), Some("/bin/sh"), "{fg:?}");
        assert!(fg.cwd.is_some(), "{fg:?}");
        child.start_kill().expect("kill");
        let _reaped = child.wait().await;
    }
}
