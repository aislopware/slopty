//! TCP ports listening in the terminals' process trees, and when to look for them.
//!
//! [`Trigger`] decides when a session is scanned: soon after its output names a local server
//! ([`mentions_local_server`]), every [`RESCAN`] while its shell runs a program in the
//! foreground or while it has listeners (a background job's), and once more when that program
//! ends. An idle shell is never scanned.
//!
//! On macOS they are read from libproc: each session's program and its descendants
//! (`proc_listchildpids`), their descriptors (`proc_pidinfo(PROC_PIDLISTFDS)`), and each socket's
//! state (`proc_pidfdinfo(PROC_PIDFDSOCKETINFO)`).
//!
//! libc binds the calls and `struct proc_fdinfo` but not `struct socket_fdinfo`, a 792-byte
//! struct of nested unions. Only three of its fields are needed, so it is read as bytes at the
//! offsets `<sys/proc_info.h>` lays them out at; the kernel writes exactly
//! `PROC_PIDFDSOCKETINFO_SIZE` bytes or fails, so a size that changed would show as no ports,
//! never as garbage.
//!
//! On Linux they are read from `/proc`: the parent of every process (`stat`), the socket inodes
//! each descriptor names (`fd/*`), and the listening sockets of its network namespace
//! (`net/tcp`, `net/tcp6`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use imp::{children, cpu, name, tcp_listeners};
use slopty_core::SessionId;
use slopty_proto::orchestration::Port;

/// Processes walked per session at most: a fork bomb in a terminal must not take the worker
/// with it.
const MAX_PROCESSES: usize = 4096;

/// How often a busy session is scanned.
pub const RESCAN: Duration = Duration::from_secs(2);
/// How long after output names a local server its session is scanned: a server prints its
/// address as it binds, and the scan should find it listening.
pub const SETTLE: Duration = Duration::from_millis(250);

/// A local server's address (`localhost:5173`, `127.0.0.1:8000`, `0.0.0.0:3000`,
/// `[::1]:8080`) or an OSC 8 hyperlink to a web address.
static LOCAL_SERVER: LazyLock<Option<regex::bytes::Regex>> = LazyLock::new(|| {
    regex::bytes::Regex::new(
        r"(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]):[0-9]{2,5}|\x1b\]8;[^;\x07\x1b]*;https?://",
    )
    .ok()
});

/// Whether terminal output names a local server, so its session is worth a scan.
#[must_use]
pub fn mentions_local_server(output: &[u8]) -> bool {
    LOCAL_SERVER.as_ref().is_some_and(|re| re.is_match(output))
}

/// When each session is next scanned, and what it listened on last.
#[derive(Debug, Default)]
pub struct Trigger {
    sessions: HashMap<SessionId, Watch>,
}

#[derive(Debug, Default)]
struct Watch {
    due: Option<Instant>,
    /// Its shell ran a program in the foreground at the last look.
    busy: bool,
    ports: Vec<Port>,
}

impl Watch {
    fn due_by(&mut self, at: Instant) {
        self.due = Some(self.due.map_or(at, |due| due.min(at)));
    }
}

impl Trigger {
    /// `session`'s output named a local server: scan it after [`SETTLE`].
    pub fn hint(&mut self, session: SessionId, now: Instant) {
        self.sessions.entry(session).or_default().due_by(now.checked_add(SETTLE).unwrap_or(now));
    }

    /// The periodic look at `session`: `busy` when its shell runs a program in the foreground.
    /// A busy session, one that just stopped being busy, and one with listeners are due now.
    pub fn look(&mut self, session: SessionId, busy: bool, now: Instant) {
        let watch = self.sessions.entry(session).or_default();
        let ended = std::mem::replace(&mut watch.busy, busy) && !busy;
        if busy || ended || !watch.ports.is_empty() {
            watch.due_by(now);
        }
    }

    /// Forget sessions `live` says are gone.
    pub fn retain(&mut self, live: impl Fn(SessionId) -> bool) {
        self.sessions.retain(|session, _watch| live(*session));
    }

    /// The earliest scan owed.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        self.sessions.values().filter_map(|w| w.due).min()
    }

    /// Sessions to scan now; each is owed nothing more until a hint or a look.
    pub fn take_due(&mut self, now: Instant) -> Vec<SessionId> {
        let mut due = Vec::new();
        for (session, watch) in &mut self.sessions {
            if watch.due.is_some_and(|at| at <= now) {
                watch.due = None;
                due.push(*session);
            }
        }
        due
    }

    /// A scan of `session` found `ports`: the new set when it changed.
    pub fn scanned(&mut self, session: SessionId, ports: Vec<Port>) -> Option<Vec<Port>> {
        let watch = self.sessions.entry(session).or_default();
        (watch.ports != ports).then(|| {
            watch.ports.clone_from(&ports);
            ports
        })
    }

    /// Every session with listeners, and them: what a client that just connected is told.
    #[must_use]
    pub fn known(&self) -> Vec<(SessionId, Vec<Port>)> {
        let listening = self.sessions.iter().filter(|(_s, w)| !w.ports.is_empty());
        listening.map(|(s, w)| (*s, w.ports.clone())).collect()
    }
}

/// Every TCP listener in the process trees rooted at `roots` (a session and the pid of the
/// program ptyd spawned for it), ordered by port.
#[must_use]
pub fn listening(roots: &[(SessionId, u32)]) -> Vec<Port> {
    let mut out: Vec<Port> = Vec::new();
    for &(session, root) in roots {
        for pid in tree(root) {
            for number in tcp_listeners(pid) {
                // One socket per family (IPv4 and IPv6) is one port to a caller.
                if !out.iter().any(|p| p.number == number && p.pid == pid) {
                    let process = name(pid);
                    out.push(Port { number, pid, process, session: Some(session) });
                }
            }
        }
    }
    out.sort_by_key(|p| (p.number, p.pid));
    out
}

/// The processor time `root`'s live descendants have used, summed, in the kernel's own units.
///
/// The units are Mach ticks on macOS, clock ticks on Linux. It says whether a background command
/// still works: it moves while any of them computes, and when one starts or ends. `root` itself
/// (an agent's own process, which keeps a timer going) is left out.
#[must_use]
pub fn descendants_cpu(root: u32) -> u64 {
    tree(root).into_iter().skip(1).map(cpu).fold(0, u64::wrapping_add)
}

/// `root` and its descendants, breadth first.
fn tree(root: u32) -> Vec<u32> {
    let mut seen = HashSet::from([root]);
    let mut queue = VecDeque::from([root]);
    let mut out = Vec::new();
    while let Some(pid) = queue.pop_front() {
        out.push(pid);
        if out.len() >= MAX_PROCESSES {
            break;
        }
        for child in children(pid) {
            if seen.insert(child) {
                queue.push_back(child);
            }
        }
    }
    out
}

/// libproc (`docs` above).
#[cfg(target_os = "macos")]
mod imp {
    /// `PROC_PIDFDSOCKETINFO` (`<sys/proc_info.h>`).
    const PROC_PIDFDSOCKETINFO: libc::c_int = 3;
    /// `PROC_PIDFDSOCKETINFO_SIZE`: `sizeof(struct socket_fdinfo)`.
    const SOCKET_FDINFO_SIZE: usize = 792;
    /// `socket_fdinfo.psi` (after `struct proc_fileinfo`, 24 bytes) `.soi_kind`, at 232 in it.
    const SOI_KIND: usize = 24 + 232;
    /// `socket_fdinfo.psi.soi_proto.pri_tcp.tcpsi_ini.insi_lport`: `soi_proto` at 240, the port
    /// at 4 in `struct in_sockinfo`, which starts `struct tcp_sockinfo`.
    const INSI_LPORT: usize = 24 + 240 + 4;
    /// `…pri_tcp.tcpsi_state`, at 80 in `struct tcp_sockinfo`.
    const TCPSI_STATE: usize = 24 + 240 + 80;
    /// `SOCKINFO_TCP`: `soi_proto` holds a `tcp_sockinfo`.
    const SOCKINFO_TCP: i32 = 2;
    /// `TSI_S_LISTEN`: the TCP state of a listening socket.
    const TSI_S_LISTEN: i32 = 1;
    /// `struct socket_fdinfo` as bytes, aligned as the struct is (it holds `uint64_t`s).
    #[repr(C, align(8))]
    struct SocketFdInfo([u8; SOCKET_FDINFO_SIZE]);

    impl SocketFdInfo {
        fn i32_at(&self, at: usize) -> Option<i32> {
            let bytes = self.0.get(at..at.checked_add(4)?)?;
            Some(i32::from_ne_bytes(bytes.try_into().ok()?))
        }
    }

    /// The live children of `pid`.
    pub(super) fn children(pid: u32) -> Vec<u32> {
        let Ok(ppid) = libc::pid_t::try_from(pid) else { return Vec::new() };
        // SAFETY: libproc's size query: with a null buffer and size 0 `proc_listchildpids` writes
        // nothing and returns how many pids it would.
        let estimate = unsafe { libc::proc_listchildpids(ppid, std::ptr::null_mut(), 0) };
        let Ok(estimate) = usize::try_from(estimate) else { return Vec::new() };
        // Room for children forked between the two calls.
        let mut pids: Vec<libc::pid_t> = vec![0; estimate.saturating_add(16)];
        let Ok(bytes) = libc::c_int::try_from(pids.len().saturating_mul(size_of::<libc::pid_t>()))
        else {
            return Vec::new();
        };
        // SAFETY: libproc writes at most `bytes` bytes of pids into the buffer, which is that
        // long, and returns how many it wrote.
        let n = unsafe { libc::proc_listchildpids(ppid, pids.as_mut_ptr().cast(), bytes) };
        let n = usize::try_from(n).unwrap_or(0).min(pids.len());
        pids.truncate(n);
        pids.into_iter().filter_map(|p| u32::try_from(p).ok().filter(|&p| p > 0)).collect()
    }

    /// Ports of the TCP sockets `pid` holds in the listening state.
    pub(super) fn tcp_listeners(pid: u32) -> Vec<u16> {
        let Ok(pid) = libc::c_int::try_from(pid) else { return Vec::new() };
        // SAFETY: libproc's size query: with a null buffer and size 0 `proc_pidinfo` writes
        // nothing and returns the bytes the descriptor list would take.
        let needed =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        let Ok(needed) = usize::try_from(needed) else { return Vec::new() };
        let entry = size_of::<libc::proc_fdinfo>();
        // Room for descriptors opened between the two calls.
        let records = needed.checked_div(entry).unwrap_or(0);
        let mut fds: Vec<libc::proc_fdinfo> = Vec::with_capacity(records.saturating_add(16));
        let Ok(room) = libc::c_int::try_from(fds.capacity().saturating_mul(entry)) else {
            return Vec::new();
        };
        // SAFETY: libproc writes at most `room` bytes of `proc_fdinfo` records into the buffer,
        // whose capacity is that many bytes, and returns the bytes it wrote.
        let wrote = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), room)
        };
        let count = usize::try_from(wrote).unwrap_or(0).checked_div(entry).unwrap_or(0);
        // SAFETY: the kernel initialised `count` whole records, and `count` is within capacity
        // (it wrote at most `room` bytes).
        unsafe {
            fds.set_len(count.min(fds.capacity()));
        }
        fds.iter()
            .filter(|fd| {
                fd.proc_fdtype == u32::try_from(libc::PROX_FDTYPE_SOCKET).unwrap_or(u32::MAX)
            })
            .filter_map(|fd| listening_port(pid, fd.proc_fd))
            .collect()
    }

    /// The port of socket `fd` in `pid` when it is a listening TCP socket.
    fn listening_port(pid: libc::c_int, fd: i32) -> Option<u16> {
        let mut info = SocketFdInfo([0; SOCKET_FDINFO_SIZE]);
        let size = libc::c_int::try_from(SOCKET_FDINFO_SIZE).ok()?;
        // SAFETY: libproc writes at most `size` bytes of `struct socket_fdinfo` into `info`,
        // which is that long and aligned as the struct; it returns the bytes it wrote.
        let wrote = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd,
                PROC_PIDFDSOCKETINFO,
                std::ptr::from_mut(&mut info).cast(),
                size,
            )
        };
        if usize::try_from(wrote).ok()? != SOCKET_FDINFO_SIZE {
            return None;
        }
        if info.i32_at(SOI_KIND)? != SOCKINFO_TCP || info.i32_at(TCPSI_STATE)? != TSI_S_LISTEN {
            return None;
        }
        // `insi_lport` is an int holding the port in network byte order (`ntohs` of its low half).
        let lport = u16::try_from(info.i32_at(INSI_LPORT)? & 0xffff).ok()?;
        Some(u16::from_be(lport)).filter(|&p| p != 0)
    }

    /// The user and system time `pid` has used, in Mach ticks; 0 when it is gone.
    pub(super) fn cpu(pid: u32) -> u64 {
        let Ok(pid) = libc::c_int::try_from(pid) else { return 0 };
        let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::zeroed();
        let Ok(bytes) = libc::c_int::try_from(size_of::<libc::proc_taskinfo>()) else { return 0 };
        // SAFETY: the buffer is one `proc_taskinfo`, `bytes` long, which is what `proc_pidinfo`
        // (libproc.h) writes for `PROC_PIDTASKINFO`; it returns the bytes it wrote.
        let wrote = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), bytes)
        };
        if wrote != bytes {
            return 0;
        }
        // SAFETY: zeroed is a valid `proc_taskinfo` (plain integers), and the call wrote it all.
        let info = unsafe { info.assume_init() };
        info.pti_total_user.wrapping_add(info.pti_total_system)
    }

    /// The command name of `pid`, empty when it is gone.
    pub(super) fn name(pid: u32) -> String {
        let Ok(pid) = libc::c_int::try_from(pid) else { return String::new() };
        let mut buf = [0_u8; 256];
        let Ok(size) = u32::try_from(buf.len()) else { return String::new() };
        // SAFETY: libproc writes at most `size` bytes of the name into `buf`, which is that long,
        // and returns how many it wrote.
        let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), size) };
        let n = usize::try_from(n).unwrap_or(0).min(buf.len());
        String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned()
    }
}

/// `/proc` (`docs` above).
#[cfg(target_os = "linux")]
mod imp {
    use std::path::PathBuf;

    use super::procfs;

    /// The live children of `pid`, from each of its threads' `children` list; where the kernel
    /// keeps none (`CONFIG_PROC_CHILDREN` off), every process whose `stat` names it as parent.
    pub(super) fn children(pid: u32) -> Vec<u32> {
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else { return Vec::new() };
        let listed: Option<Vec<u32>> = tasks
            .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("children")).ok())
            .map(|list| list.split_whitespace().filter_map(|c| c.parse().ok()).collect::<Vec<_>>())
            .reduce(|mut all, more| {
                all.extend(more);
                all
            });
        listed.unwrap_or_else(|| by_parent(pid))
    }

    /// Every process whose `stat` names `pid` as its parent: one read of each in `/proc`.
    fn by_parent(pid: u32) -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
        entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&child| {
                std::fs::read_to_string(format!("/proc/{child}/stat"))
                    .ok()
                    .and_then(|stat| procfs::parent(&stat))
                    == Some(pid)
            })
            .collect()
    }

    /// Ports of the TCP sockets `pid` holds in the listening state.
    pub(super) fn tcp_listeners(pid: u32) -> Vec<u16> {
        let dir = PathBuf::from(format!("/proc/{pid}"));
        let Ok(fds) = std::fs::read_dir(dir.join("fd")) else { return Vec::new() };
        let inodes: Vec<u64> = fds
            .filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
            .filter_map(|target| procfs::socket_inode(&target.to_string_lossy()))
            .collect();
        if inodes.is_empty() {
            return Vec::new();
        }
        let mut ports: Vec<u16> = ["tcp", "tcp6"]
            .into_iter()
            .filter_map(|table| std::fs::read_to_string(dir.join("net").join(table)).ok())
            .flat_map(|table| procfs::listeners(&table))
            .filter(|(_port, inode)| inodes.contains(inode))
            .map(|(port, _inode)| port)
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// The user and system time `pid` has used, in clock ticks; 0 when it is gone.
    pub(super) fn cpu(pid: u32) -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| procfs::cpu(&stat))
            .unwrap_or(0)
    }

    /// The command name of `pid`, empty when it is gone.
    pub(super) fn name(pid: u32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|comm| comm.trim_end().to_owned())
            .unwrap_or_default()
    }
}

/// The `/proc` text the Linux scan reads, parsed. Pure, so it is tested on any host.
#[cfg(any(target_os = "linux", test))]
mod procfs {
    /// `TCP_LISTEN` as `/proc/net/tcp` writes a socket's state.
    const LISTEN: &str = "0A";

    /// The parent pid in a `/proc/<pid>/stat` line: the second field after the parenthesised
    /// name, which may itself hold spaces and parentheses.
    pub(super) fn parent(stat: &str) -> Option<u32> {
        let after = stat.get(stat.rfind(')')?.checked_add(1)?..)?;
        after.split_whitespace().nth(1)?.parse().ok()
    }

    /// `utime` plus `stime` in a `/proc/<pid>/stat` line: the 12th and 13th fields after the
    /// parenthesised name.
    pub(super) fn cpu(stat: &str) -> Option<u64> {
        let after = stat.get(stat.rfind(')')?.checked_add(1)?..)?;
        let mut fields = after.split_whitespace().skip(11);
        let user: u64 = fields.next()?.parse().ok()?;
        let system: u64 = fields.next()?.parse().ok()?;
        Some(user.wrapping_add(system))
    }

    /// The inode a descriptor's link names when it is a socket (`socket:[12345]`).
    pub(super) fn socket_inode(link: &str) -> Option<u64> {
        link.strip_prefix("socket:[")?.strip_suffix(']')?.parse().ok()
    }

    /// Every listening socket in a `/proc/net/tcp` or `tcp6` table, as (port, inode).
    pub(super) fn listeners(table: &str) -> Vec<(u16, u64)> {
        table
            .lines()
            .skip(1)
            .filter_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if *fields.get(3)? != LISTEN {
                    return None;
                }
                let port = fields.get(1)?.rsplit_once(':')?.1;
                let port = u16::from_str_radix(port, 16).ok().filter(|&p| p != 0)?;
                Some((port, fields.get(9)?.parse().ok()?))
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Listeners, parents and socket links read off `/proc` text as Linux writes it: only
        /// the sockets in state `0A` of either table, a name with spaces and parentheses.
        #[test]
        fn listeners_and_parents_are_read_off_procfs() {
            let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
                0: 0100007F:1435 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 41231 1 0000000000000000 100 0 0 10 0\n   \
                1: 0100007F:1435 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 41299 1 0000000000000000 20 4 30 10 -1\n";
            let tcp6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
                0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 52000 1 0000000000000000 100 0 0 10 0\n";
            assert_eq!(listeners(tcp), [(5173, 41231)], "the established one is not listening");
            assert_eq!(listeners(tcp6), [(8080, 52000)]);
            assert_eq!(parent("4242 (tmux: (srv) x) S 17 4242 4242 0 -1"), Some(17));
            assert_eq!(socket_inode("socket:[41231]"), Some(41231));
            assert_eq!(socket_inode("/dev/pts/3"), None);
            let stat = "4242 (cargo (build)) R 17 4242 4242 0 -1 4194304 100 0 0 0 250 31 0 0 20";
            assert_eq!(cpu(stat), Some(281), "utime 250 + stime 31");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_naming_a_local_server_is_noticed() {
        for yes in [
            &b"  Local:   http://localhost:5173/"[..],
            b"Serving HTTP on 0.0.0.0:8000 (http://0.0.0.0:8000/) ...",
            b"listening on 127.0.0.1:3000",
            b"http://[::1]:8080",
            b"\x1b]8;id=1;https://example.com\x1b\\link\x1b]8;;\x1b\\",
        ] {
            assert!(mentions_local_server(yes), "{}", String::from_utf8_lossy(yes));
        }
        for no in [
            &b"12:34:56 build finished"[..],
            b"see localhost for details",
            b"\x1b]8;;file:///Users/me/a.txt\x1b\\a.txt\x1b]8;;\x1b\\",
        ] {
            assert!(!mentions_local_server(no), "{}", String::from_utf8_lossy(no));
        }
    }

    /// What the port hint costs a PTY read that names no server, the case that is checked on
    /// every read (a hit is followed by a second without checks): full 64 KiB reads of
    /// coloured build output and of escape-free text. Printed; recorded in MEASUREMENTS.md.
    #[test]
    #[ignore = "a measurement: run with --run-ignored only --release --no-capture"]
    fn local_server_scan_cost() {
        let coloured = "\x1b[1m\x1b[32m   Compiling\x1b[0m slopty-worker v0.1.0 (/Users/me/src/slopty/crates/slopty-worker) 12:34:56\r\n";
        let plain =
            "test ports::tests::a_tree_holds_the_children ... ok, took 0.012 s on 10.0.0.12:x\n";
        let fill = |line: &str| {
            let mut read = Vec::with_capacity(64 << 10);
            while read.len() + line.len() <= 64 << 10 {
                read.extend_from_slice(line.as_bytes());
            }
            read
        };
        let reads = [
            ("one echoed keystroke", b"a".to_vec()),
            ("a 64 KiB read of coloured build output", fill(coloured)),
            ("a 64 KiB read of plain text", fill(plain)),
        ];
        for (what, read) in reads {
            let mut took: Vec<u128> = std::iter::repeat_n((), 2_000)
                .map(|()| {
                    let at = Instant::now();
                    assert!(!mentions_local_server(std::hint::black_box(&read)));
                    at.elapsed().as_nanos()
                })
                .collect();
            took.sort_unstable();
            let (p50, p99) = (took[took.len() / 2], took[took.len() * 99 / 100]);
            println!("{what}: p50 {p50} ns, p99 {p99} ns");
        }
    }

    fn port(number: u16, session: SessionId) -> Port {
        Port { number, pid: 1, process: "node".to_owned(), session: Some(session) }
    }

    /// Idle sessions are never due; a hint is due after the settle; a busy one on every look;
    /// one that stops being busy once more; one with listeners until they go.
    #[test]
    fn scans_follow_hints_and_busy_shells_and_never_idle_ones() {
        let mut t = Trigger::default();
        let (idle, busy, hinted) = (SessionId::new(), SessionId::new(), SessionId::new());
        let now = Instant::now();
        t.look(idle, false, now);
        t.look(busy, true, now);
        t.hint(hinted, now);
        assert_eq!(t.take_due(now), [busy]);
        assert_eq!(t.next_due(), Some(now.checked_add(SETTLE).unwrap()));
        let later = now.checked_add(SETTLE).unwrap();
        assert_eq!(t.take_due(later), [hinted]);
        assert_eq!(t.next_due(), None, "nothing owed until the next look");

        let next = later.checked_add(RESCAN).unwrap();
        t.look(idle, false, next);
        t.look(busy, false, next);
        assert_eq!(t.take_due(next), [busy], "one more scan when the program ends");
        assert_eq!(t.scanned(hinted, vec![port(5173, hinted)]), Some(vec![port(5173, hinted)]));
        assert_eq!(t.scanned(hinted, vec![port(5173, hinted)]), None, "unchanged");
        t.look(hinted, false, next);
        assert_eq!(t.take_due(next), [hinted], "a background server is watched");
        assert_eq!(t.known(), [(hinted, vec![port(5173, hinted)])]);
        assert_eq!(t.scanned(hinted, Vec::new()), Some(Vec::new()), "it went");
        t.look(hinted, false, next);
        assert!(t.take_due(next).is_empty());
        t.retain(|s| s != hinted);
        assert!(t.known().is_empty());
    }

    /// This test process's own listener is found through the same calls, with its port, its
    /// pid and its name; a closed one is gone.
    #[test]
    fn a_listener_of_this_process_is_found() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let me = std::process::id();
        let session = SessionId::new();
        let found = listening(&[(session, me)]);
        let mine = found.iter().find(|p| p.number == port).expect("the listener");
        assert_eq!((mine.pid, mine.session), (me, Some(session)));
        assert!(!mine.process.is_empty());
        drop(listener);
        assert!(!listening(&[(session, me)]).iter().any(|p| p.number == port));
    }

    #[test]
    fn a_connected_socket_is_not_a_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(addr).unwrap();
        let local = client.local_addr().unwrap().port();
        let ports = tcp_listeners(std::process::id());
        assert!(ports.contains(&addr.port()));
        assert!(!ports.contains(&local), "the client end is established, not listening");
    }

    /// A descendant that computes moves the sum; one that sleeps does not; the root's own
    /// time is not in it.
    #[tokio::test]
    async fn a_busy_descendant_moves_the_processor_time_and_a_sleeping_one_does_not() {
        use std::time::Duration;
        let me = std::process::id();
        let mut sleeper = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let before = descendants_cpu(me);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(descendants_cpu(me), before, "a sleeping child uses nothing");
        let mut busy = std::process::Command::new("/usr/bin/yes")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let first = descendants_cpu(me);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let second = descendants_cpu(me);
        assert!(second > first, "a busy child moves it: {first} then {second}");
        for child in [&mut sleeper, &mut busy] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }

    #[test]
    fn a_tree_holds_the_children() {
        let mut child = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pids = tree(std::process::id());
        assert!(pids.contains(&child.id()), "{pids:?}");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
