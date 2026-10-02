//! A program may hand the terminal an image in POSIX shared memory (Kitty graphics, `t=s`): the
//! engine reads the object and unlinks it.

#[cfg(test)]
#[cfg(unix)]
mod shared_memory {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

    use slopty_engine::{EngineConfig, GhosttyEngine};
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;

    /// A shared memory object named `name` holding `bytes`, as a program makes one.
    fn share(name: &CString, bytes: &[u8]) -> OwnedFd {
        // SAFETY: shm_open(2) reads `name` as a NUL-terminated string, which a `CString` is.
        let fd = unsafe {
            libc::shm_open(name.as_ptr(), libc::O_CREAT | libc::O_EXCL | libc::O_RDWR, 0o600)
        };
        assert!(fd >= 0, "shm_open: {}", std::io::Error::last_os_error());
        // SAFETY: shm_open returned this descriptor just now; nothing else owns it.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let len = libc::off_t::try_from(bytes.len()).unwrap();
        // SAFETY: ftruncate(2) on a descriptor this function owns, sizing a new object once.
        let sized = unsafe { libc::ftruncate(fd.as_raw_fd(), len) };
        assert_eq!(sized, 0, "ftruncate: {}", std::io::Error::last_os_error());
        // A shared memory object on macOS takes no write(2), only a mapping.
        // SAFETY: mmap(2) of `bytes.len()` bytes of an object just sized to that, shared,
        // with no address asked for.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes.len(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        assert_ne!(map, libc::MAP_FAILED, "mmap: {}", std::io::Error::last_os_error());
        // SAFETY: the mapping is `bytes.len()` writable bytes that alias nothing of Rust's.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), map.cast::<u8>(), bytes.len());
        }
        // SAFETY: unmapping exactly the mapping made above, used no more.
        let unmapped = unsafe { libc::munmap(map, bytes.len()) };
        assert_eq!(unmapped, 0, "munmap: {}", std::io::Error::last_os_error());
        fd
    }

    fn base64(bytes: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let at = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
            let n = at(0).wrapping_shl(16) | at(1).wrapping_shl(8) | at(2);
            let digits = [18, 12, 6, 0].map(|shift: u32| {
                char::from(
                    T.get(usize::try_from(n.wrapping_shr(shift) & 63).unwrap()).copied().unwrap(),
                )
            });
            let keep = chunk.len().saturating_add(1);
            out.extend(digits.iter().take(keep));
            out.extend(std::iter::repeat_n('=', 4_usize.saturating_sub(keep)));
        }
        out
    }

    #[test]
    fn an_image_may_come_in_shared_memory() {
        let pixels: Vec<u8> = (0..16).collect();
        // Short: macOS holds a shared memory name to 31 bytes.
        let name = format!("/slopty-{}", std::process::id());
        let cname = CString::new(name.clone()).unwrap();
        let _fd = share(&cname, &pixels);
        let mut e = GhosttyEngine::new(EngineConfig {
            size: TermSize {
                cols: 20,
                rows: 5,
                metrics: CellMetrics { cell_width: 8, cell_height: 16 },
            },
            scrollback_lines: 100,
        })
        .unwrap();
        let shm = base64(name.as_bytes());
        e.write(format!("\x1b_Ga=T,t=s,f=32,s=2,v=2,i=5,q=2;{shm}\x1b\\").as_bytes());
        let _frame = e.take_frame(0).unwrap();
        let got: Vec<(u32, Vec<u8>)> =
            e.drain_images().into_iter().map(|u| (u.id, u.rgba)).collect();
        // SAFETY: shm_unlink(2) reads `cname` as a NUL-terminated string; it fails, as it
        // should, when the engine unlinked the object already.
        let unlinked = unsafe { libc::shm_unlink(cname.as_ptr()) } != 0;
        assert_eq!(got, vec![(5, pixels)]);
        assert!(unlinked, "the object is unlinked once read");
    }
}
