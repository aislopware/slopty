//! Running a new build of this process in place, under the same pid.
//!
//! This is how ptyd hands every session to its next build
//! ([`crate::protocol::PtydRequest::Succeed`]), with chosen descriptors kept open across the
//! `exec`. Nothing crosses a socket. The old image clears close-on-exec on each master and on the
//! state file and `exec`s; the new one finds them open under the numbers the state file names. A
//! descriptor this process opens itself is close-on-exec from the start (std, tokio and rustix
//! open every one with `O_CLOEXEC`), so one above 2 open without the flag can only be one the
//! image before kept for this one: that is what [`take_inherited`] checks before it owns one. Two
//! kinds are close-on-exec only a moment after they exist on macOS, a PTY `openpty` opens and a
//! descriptor `recvmsg` brings, so the inheritance is taken first, before this process opens
//! or receives any. The checks cannot tell a descriptor the image before kept from one another
//! part of this process owns, so taking one is `unsafe`, on the caller's word.

use std::ffi::CString;
use std::os::fd::{BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::{io, ptr};

use rustix::io::FdFlags;

/// Keep `fd` open across the next `exec` (`keep`), or close it there again (`!keep`).
///
/// # Errors
///
/// When `fcntl` refuses, `fd` being no descriptor of this process's.
pub fn keep_across_exec(fd: BorrowedFd<'_>, keep: bool) -> io::Result<()> {
    let flags = if keep { FdFlags::empty() } else { FdFlags::CLOEXEC };
    rustix::io::fcntl_setfd(fd, flags).map_err(io::Error::from)
}

/// Become `program` run with `args` (after its `argv[0]`, which is `program`), under this pid,
/// with this environment and every descriptor not close-on-exec.
///
/// Unlike std's `Command::exec`, nothing of this process is changed before the `exec`: no
/// descriptor is moved onto standard input, no signal is put back to its default and the mask
/// is left alone. So a failed one leaves the process exactly as it was, and it carries on.
/// Returns only then, with why.
pub fn exec_in_place(program: &Path, args: &[String]) -> io::Error {
    let invalid = |e| io::Error::new(io::ErrorKind::InvalidInput, e);
    let path = match CString::new(program.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(e) => return invalid(e),
    };
    let rest: Result<Vec<CString>, _> = args.iter().map(|a| CString::new(a.as_bytes())).collect();
    let rest = match rest {
        Ok(rest) => rest,
        Err(e) => return invalid(e),
    };
    let argv: Vec<*const libc::c_char> = std::iter::once(path.as_ptr())
        .chain(rest.iter().map(|a| a.as_ptr()))
        .chain(std::iter::once(ptr::null()))
        .collect();
    // SAFETY: `execv` reads the NUL-terminated `path` and the null-terminated `argv`, whose
    // strings `path` and `rest` own past the call; it replaces the image or returns -1 with
    // `errno` set, changing nothing.
    unsafe {
        libc::execv(path.as_ptr(), argv.as_ptr());
    }
    io::Error::last_os_error()
}

/// Own descriptor `raw`, which the image before this one kept open across `exec` for it. It is
/// close-on-exec from here, so nothing this process starts gets it.
///
/// What can be checked is: `raw` must be open, above 2 and not close-on-exec, so a descriptor
/// this process opened itself, or one taken already, is refused.
///
/// # Safety
///
/// `raw` was kept open across `exec` for this image by the image before it, and nothing in
/// this process owns it: it is taken once, before this process could have wrapped it.
/// Descriptors left open by whatever started the process are not covered by the checks, so the
/// numbers must come from the image before, as its handover names them.
///
/// # Errors
///
/// `InvalidInput` for standard input, output or error, which std owns; the error of `fcntl`
/// when `raw` is not open; and `InvalidData` when it is close-on-exec.
pub unsafe fn take_inherited(raw: RawFd) -> io::Result<OwnedFd> {
    if raw <= libc::STDERR_FILENO {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("descriptor {raw} is standard input, output or error"),
        ));
    }
    // SAFETY: `F_GETFD` only reads the descriptor's flags; a number not open is `EBADF`.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::FD_CLOEXEC != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("descriptor {raw} was not kept across exec"),
        ));
    }
    // SAFETY: `raw` is open, and the caller vouches that nothing else in this process owns it
    // (this function's contract).
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    rustix::io::fcntl_setfd(&fd, FdFlags::CLOEXEC)?;
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd as _, AsRawFd as _, IntoRawFd as _};

    use super::*;

    /// A descriptor kept across `exec` is taken, and close-on-exec from then on; one this
    /// process opened, one taken already, and standard input, are refused.
    #[test]
    fn only_a_descriptor_kept_across_exec_is_taken() {
        let file = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: refused, being close-on-exec, before anything could own it twice; the takes
        // below are the same, but the one of `raw`, which `file` gave up first.
        let refused = unsafe { take_inherited(file.as_raw_fd()) }.unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData, "{refused}");
        keep_across_exec(file.as_fd(), true).unwrap();
        let raw = file.into_raw_fd();
        // SAFETY: as above.
        let taken = unsafe { take_inherited(raw) }.unwrap();
        let flags = rustix::io::fcntl_getfd(&taken).unwrap();
        assert!(flags.contains(FdFlags::CLOEXEC), "close-on-exec once taken: {flags:?}");
        // SAFETY: as above: refused, since `taken` made it close-on-exec.
        let twice = unsafe { take_inherited(raw) }.unwrap_err();
        assert_eq!(twice.kind(), io::ErrorKind::InvalidData, "not taken twice: {twice}");
        // SAFETY: as above: refused before anything is owned.
        let stdin = unsafe { take_inherited(0) }.unwrap_err();
        assert_eq!(stdin.kind(), io::ErrorKind::InvalidInput, "{stdin}");
    }

    /// A program that cannot be run leaves this process as it was, and says why.
    #[test]
    fn a_failed_exec_returns() {
        let missing = Path::new("/nonexistent/slopty-ptyd");
        let why = exec_in_place(missing, &["--inherit".to_owned(), "3".to_owned()]);
        assert_eq!(why.kind(), io::ErrorKind::NotFound, "{why}");
        let nul = exec_in_place(Path::new("/bin/sh"), &["a\0b".to_owned()]);
        assert_eq!(nul.kind(), io::ErrorKind::InvalidInput, "{nul}");
    }
}
