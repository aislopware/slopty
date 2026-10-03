//! A process as the kernel counts it: retired instructions, footprint, descriptors, threads.
//!
//! Everything here reads `libproc` for a pid of the same user, which needs no root and no
//! entitlement. On Linux the descriptors and threads come from `/proc` and the counters are
//! `None`; elsewhere each reader returns `None`.
//!
//! - Instructions and cycles (`ri_instructions`, `ri_cycles` of `rusage_info_v4`) are the
//!   process's, all threads together. On a loaded machine they move by well under a percent where
//!   wall time moves by tens, so they are what a measurement's budget holds.
//! - The footprint (`ri_phys_footprint`) is the dirty memory Activity Monitor and jetsam count, not
//!   the resident set; `ri_lifetime_max_phys_footprint` is its peak.

/// One reading of a process's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Instructions retired since the process started.
    pub instructions: u64,
    /// Cycles since the process started.
    pub cycles: u64,
    /// Physical footprint, in bytes.
    pub footprint: u64,
    /// The largest footprint the process has had, in bytes.
    pub peak_footprint: u64,
}

/// The counters of `pid`; `None` when it is gone or not ours to read.
#[cfg_attr(
    not(target_vendor = "apple"),
    expect(clippy::missing_const_for_fn, reason = "const only where the reader is a stub")
)]
#[must_use]
pub fn usage(pid: i32) -> Option<Usage> {
    imp::usage(pid)
}

/// The counters of this process.
#[must_use]
pub fn own() -> Option<Usage> {
    usage(i32::try_from(std::process::id()).ok()?)
}

/// Instructions this process has retired, all threads together.
#[must_use]
pub fn instructions() -> Option<u64> {
    own().map(|u| u.instructions)
}

/// The descriptors `pid` has open.
#[must_use]
pub fn open_fds(pid: i32) -> Option<u32> {
    imp::open_fds(pid)
}

/// The threads `pid` runs.
#[must_use]
pub fn threads(pid: i32) -> Option<u32> {
    imp::threads(pid)
}

#[cfg(target_vendor = "apple")]
mod imp {
    use std::mem::MaybeUninit;

    use libc::{
        PROC_PIDLISTFDS, PROC_PIDTASKINFO, RUSAGE_INFO_V4, c_int, c_void, proc_fdinfo,
        proc_pid_rusage, proc_pidinfo, proc_taskinfo, rusage_info_v4,
    };

    use super::Usage;

    pub(super) fn usage(pid: i32) -> Option<Usage> {
        let mut info = MaybeUninit::<rusage_info_v4>::zeroed();
        // SAFETY: `proc_pid_rusage` (libproc.h) writes one `rusage_info_v4` for the flavor
        // `RUSAGE_INFO_V4` into the buffer it is given, which is one of exactly that type. The
        // SDK declares the parameter `rusage_info_t *`, a pointer to the struct passed as that.
        let done = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V4, info.as_mut_ptr().cast()) };
        if done != 0 {
            return None;
        }
        // SAFETY: zeroed is a valid `rusage_info_v4` (plain integers), and the call succeeded.
        let info = unsafe { info.assume_init() };
        Some(Usage {
            instructions: info.ri_instructions,
            cycles: info.ri_cycles,
            footprint: info.ri_phys_footprint,
            peak_footprint: info.ri_lifetime_max_phys_footprint,
        })
    }

    /// `proc_pidinfo(PROC_PIDLISTFDS)` answers a null buffer with the size of the descriptor
    /// table, not the count open; so it is asked with a buffer, grown until the list fits, and
    /// the count is what it wrote.
    pub(super) fn open_fds(pid: i32) -> Option<u32> {
        let entry = size_of::<proc_fdinfo>();
        let mut room = 256_usize;
        loop {
            let mut buffer: Vec<MaybeUninit<proc_fdinfo>> = Vec::with_capacity(room);
            let bytes = c_int::try_from(room.checked_mul(entry)?).ok()?;
            // SAFETY: the buffer holds `room` entries of `proc_fdinfo`, `bytes` long, which is
            // what `proc_pidinfo` (libproc.h) may write for `PROC_PIDLISTFDS`; it writes whole
            // entries and returns the bytes written.
            let wrote = unsafe {
                proc_pidinfo(pid, PROC_PIDLISTFDS, 0, buffer.as_mut_ptr().cast::<c_void>(), bytes)
            };
            let wrote = usize::try_from(wrote).ok().filter(|&w| w > 0)?;
            let count = wrote.checked_div(entry)?;
            if count < room {
                return u32::try_from(count).ok();
            }
            room = room.checked_mul(4)?;
        }
    }

    pub(super) fn threads(pid: i32) -> Option<u32> {
        let mut info = MaybeUninit::<proc_taskinfo>::zeroed();
        let bytes = c_int::try_from(size_of::<proc_taskinfo>()).ok()?;
        // SAFETY: the buffer is one `proc_taskinfo`, `bytes` long, which is what
        // `proc_pidinfo` (libproc.h) writes for `PROC_PIDTASKINFO`.
        let wrote = unsafe {
            proc_pidinfo(pid, PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast::<c_void>(), bytes)
        };
        if wrote != bytes {
            return None;
        }
        // SAFETY: zeroed is a valid `proc_taskinfo` (plain integers), and the call wrote it all.
        let info = unsafe { info.assume_init() };
        u32::try_from(info.pti_threadnum).ok()
    }
}

#[cfg(not(target_vendor = "apple"))]
mod imp {
    use super::Usage;

    pub(super) const fn usage(_pid: i32) -> Option<Usage> {
        None
    }

    pub(super) fn open_fds(pid: i32) -> Option<u32> {
        entries(pid, "fd")
    }

    pub(super) fn threads(pid: i32) -> Option<u32> {
        entries(pid, "task")
    }

    /// How many entries `/proc/<pid>/<dir>` lists: one per descriptor in `fd`, one per thread
    /// in `task`.
    fn entries(pid: i32, dir: &str) -> Option<u32> {
        let listed = std::fs::read_dir(format!("/proc/{pid}/{dir}")).ok()?;
        u32::try_from(listed.count()).ok()
    }
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    #[test]
    fn this_process_reads_back() {
        let me = i32::try_from(std::process::id()).unwrap();
        let before = own().unwrap();
        let mut x = 0_u64;
        for i in 0..1_000_000_u64 {
            x = std::hint::black_box(x.wrapping_add(i));
        }
        let after = own().unwrap();
        assert!(
            after.instructions.saturating_sub(before.instructions) > 1_000_000,
            "a million additions retire at least a million instructions: {before:?} {after:?}"
        );
        assert!(after.footprint > 0 && after.peak_footprint >= after.footprint, "{after:?}");
        let fds = open_fds(me).unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(open_fds(me).unwrap(), fds + 1, "one more file, one more descriptor");
        drop(file);
        assert!(threads(me).unwrap() >= 1, "at least the one running this");
    }

    #[test]
    fn a_gone_process_reads_none() {
        assert_eq!(usage(-1), None, "no such pid");
        assert_eq!(open_fds(-1), None, "no such pid");
        assert_eq!(threads(-1), None, "no such pid");
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod linux_tests {
    use super::*;

    /// The descriptors and threads come from `/proc`; the counters it has none of.
    #[test]
    fn this_process_reads_back_from_proc() {
        let me = i32::try_from(std::process::id()).unwrap();
        let fds = open_fds(me).unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(open_fds(me).unwrap(), fds + 1, "one more file, one more descriptor");
        drop(file);
        assert!(threads(me).unwrap() >= 1, "at least the one running this");
        assert_eq!(open_fds(-1), None, "no such pid");
        assert_eq!(usage(me), None, "no counters read here");
    }
}
