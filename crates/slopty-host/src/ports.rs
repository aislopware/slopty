//! TCP ports listening in the terminals' process trees.
//!
//! They are read from libproc: each session's program and its descendants (`proc_listchildpids`),
//! their descriptors (`proc_pidinfo(PROC_PIDLISTFDS)`), and each socket's state
//! (`proc_pidfdinfo(PROC_PIDFDSOCKETINFO)`).
//!
//! libc binds the calls and `struct proc_fdinfo` but not `struct socket_fdinfo`, a 792-byte
//! struct of nested unions. Only three of its fields are needed, so it is read as bytes at the
//! offsets `<sys/proc_info.h>` lays them out at; the kernel writes exactly
//! `PROC_PIDFDSOCKETINFO_SIZE` bytes or fails, so a size that changed would show as no ports,
//! never as garbage.

use std::collections::{HashSet, VecDeque};

use slopty_core::SessionId;
use slopty_proto::orchestration::Port;

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
/// Processes walked per session at most: a fork bomb in a terminal must not take the worker
/// with it.
const MAX_PROCESSES: usize = 4096;

/// `struct socket_fdinfo` as bytes, aligned as the struct is (it holds `uint64_t`s).
#[repr(C, align(8))]
struct SocketFdInfo([u8; SOCKET_FDINFO_SIZE]);

impl SocketFdInfo {
    fn i32_at(&self, at: usize) -> Option<i32> {
        let bytes = self.0.get(at..at.checked_add(4)?)?;
        Some(i32::from_ne_bytes(bytes.try_into().ok()?))
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

/// The live children of `pid`.
fn children(pid: u32) -> Vec<u32> {
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
fn tcp_listeners(pid: u32) -> Vec<u16> {
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
    let wrote =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), room) };
    let count = usize::try_from(wrote).unwrap_or(0).checked_div(entry).unwrap_or(0);
    // SAFETY: the kernel initialised `count` whole records, and `count` is within capacity
    // (it wrote at most `room` bytes).
    unsafe {
        fds.set_len(count.min(fds.capacity()));
    }
    fds.iter()
        .filter(|fd| fd.proc_fdtype == u32::try_from(libc::PROX_FDTYPE_SOCKET).unwrap_or(u32::MAX))
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

/// The command name of `pid`, empty when it is gone.
fn name(pid: u32) -> String {
    let Ok(pid) = libc::c_int::try_from(pid) else { return String::new() };
    let mut buf = [0_u8; 256];
    let Ok(size) = u32::try_from(buf.len()) else { return String::new() };
    // SAFETY: libproc writes at most `size` bytes of the name into `buf`, which is that long,
    // and returns how many it wrote.
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), size) };
    let n = usize::try_from(n).unwrap_or(0).min(buf.len());
    String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_tree_holds_the_children() {
        let mut child = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pids = tree(std::process::id());
        assert!(pids.contains(&child.id()), "{pids:?}");
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
