//! The LAN beneath the tailnet: this machine's interfaces on it, and the magic packet that
//! wakes a machine asleep there (`docs/decisions/workers.md`, "A sleeping worker is woken from
//! its own LAN").
//!
//! A sleeping Mac is off the tailnet, so Tailscale cannot wake it. Only a machine on its own
//! Ethernet segment can, by broadcasting the magic packet there ([`slopty_proto::lan`]). Each
//! worker reports its [`ports`]; the server, or a worker that shares a subnet with a sleeping
//! one, [`wake`]s it.
//!
//! Interfaces come from `getifaddrs(3)`: the flags and the subnet from each interface's
//! `AF_INET` entries, the hardware address from its `AF_PACKET` entry (`sockaddr_ll`) on Linux.
//! macOS 27 hands a process that is not root `02:00:00:00:00:00` in every `AF_LINK` entry
//! (`sockaddr_dl`), as iOS long has, so there the address comes from the I/O Registry instead,
//! which `ioreg` reads too and which still says it; the `AF_LINK` entry is read all the same
//! and wins wherever it holds a real address.

#![expect(unsafe_code, reason = "getifaddrs(3) is the one interface that names every MAC")]

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use slopty_proto::lan::{LanPort, MacAddr, WAKE_GAP, WAKE_PORT, WAKE_REPEATS, magic_packet};
use tokio::net::UdpSocket;

/// What macOS 27 and iOS put in every `AF_LINK` entry for a process that is not root.
const REDACTED: MacAddr = MacAddr([0x02, 0, 0, 0, 0, 0]);

/// Interfaces reported at most: a machine with more is a router or a VM host, and the first
/// few by name are its real ones.
const MAX_PORTS: usize = 8;

/// One entry of the interface list, as read.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Entry {
    /// The interface's name.
    name: String,
    /// Up, able to broadcast, and neither loopback nor point-to-point (a tunnel).
    broadcasts: bool,
    /// What the entry holds.
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// The interface's Ethernet address.
    Link(MacAddr),
    /// One of its IPv4 addresses and the subnet's prefix length.
    V4(Ipv4Addr, u8),
}

/// This machine's interfaces on a LAN, by name.
///
/// Each is up, broadcasting, Ethernet or Wi-Fi, with an IPv4 subnet. Tunnels (the tailnet's own
/// `utun`), loopback and self-assigned (`169.254`) addresses are left out, as nothing asleep can be
/// reached through them.
#[must_use]
pub fn ports() -> Vec<LanPort> {
    lan_ports(&entries(), &hardware())
}

/// Which of this machine's ports (`own`) sends for which ports of a sleeping machine (`peer`).
///
/// Each sleeping port is sent for by the first of `own` whose subnet holds its address. A port no
/// subnet of `own` holds is left out; none left means this machine cannot wake it.
#[must_use]
pub fn plan(own: &[LanPort], peer: &[LanPort]) -> Vec<(LanPort, Vec<LanPort>)> {
    let mut out: Vec<(LanPort, Vec<LanPort>)> = Vec::new();
    for sleeping in peer {
        let Some(from) = own.iter().find(|p| p.reaches(sleeping.addr) && p.addr != sleeping.addr)
        else {
            continue;
        };
        match out.iter_mut().find(|(sender, _)| sender == from) {
            Some((_, targets)) => targets.push(sleeping.clone()),
            None => out.push((from.clone(), vec![sleeping.clone()])),
        }
    }
    out
}

/// Wake the machine whose ports are `peer` from this machine's ports `own`, by [`plan`]: the
/// names of the sleeping machine's interfaces the packet went out for.
///
/// # Errors
/// When no port of `own` shares a subnet with `peer`, or a send fails.
pub async fn wake_peer(own: &[LanPort], peer: &[LanPort]) -> std::io::Result<Vec<String>> {
    let plan = plan(own, peer);
    if plan.is_empty() {
        let subnets: Vec<String> =
            peer.iter().map(|p| format!("{}/{}", p.addr, p.prefix)).collect();
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no interface here is on the sleeping machine's subnet ({})",
                subnets.join(", ")
            ),
        ));
    }
    let mut sent = Vec::new();
    for (from, targets) in plan {
        let macs: Vec<MacAddr> = targets.iter().map(|t| t.mac).collect();
        wake(&from, &macs).await?;
        sent.extend(targets.into_iter().map(|t| t.interface));
    }
    Ok(sent)
}

/// Send the magic packet for each of `macs` as a broadcast on `from`'s subnet.
///
/// It goes [`WAKE_REPEATS`] times, [`WAKE_GAP`] apart, to UDP [`WAKE_PORT`]. `from` is one of this
/// machine's [`ports`]: bound to its address, the broadcast leaves by that interface.
///
/// # Errors
/// When the socket cannot be bound or a send fails.
pub async fn wake(from: &LanPort, macs: &[MacAddr]) -> std::io::Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(from.addr, 0)).await?;
    socket.set_broadcast(true)?;
    let to = SocketAddr::V4(SocketAddrV4::new(from.broadcast(), WAKE_PORT));
    tracing::info!(interface = %from.interface, %to, ?macs, "sending wake packets");
    send(&socket, to, macs).await
}

/// The sends of [`wake`], on any socket to any address.
///
/// # Errors
/// When a send fails.
pub async fn send(socket: &UdpSocket, to: SocketAddr, macs: &[MacAddr]) -> std::io::Result<()> {
    let packets: Vec<_> = macs.iter().map(|mac| magic_packet(*mac)).collect();
    for round in 0..WAKE_REPEATS {
        if round > 0 {
            tokio::time::sleep(WAKE_GAP).await;
        }
        for packet in &packets {
            socket.send_to(packet, to).await?;
        }
    }
    Ok(())
}

/// The ports the entries describe: each broadcasting interface with an Ethernet address, once
/// for each IPv4 subnet it is on. An address the entries redact is taken from `hardware`, by
/// interface name.
fn lan_ports(entries: &[Entry], hardware: &BTreeMap<String, MacAddr>) -> Vec<LanPort> {
    let mut macs = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.broadcasts) {
        if let Kind::Link(listed) = entry.kind {
            let mac =
                if listed == REDACTED { hardware.get(&entry.name).copied() } else { Some(listed) };
            if let Some(mac) = mac.filter(|m| m.is_unicast() && *m != REDACTED) {
                macs.insert(entry.name.as_str(), mac);
            }
        }
    }
    let mut ports: Vec<LanPort> = entries
        .iter()
        .filter(|e| e.broadcasts)
        .filter_map(|e| match e.kind {
            Kind::V4(addr, prefix) if !addr.is_link_local() && !addr.is_loopback() => {
                let mac = *macs.get(e.name.as_str())?;
                Some(LanPort { interface: e.name.clone(), mac, addr, prefix })
            }
            Kind::V4(..) | Kind::Link(_) => None,
        })
        .collect();
    ports.sort_by(|a, b| a.interface.cmp(&b.interface).then(a.addr.cmp(&b.addr)));
    ports.dedup();
    ports.truncate(MAX_PORTS);
    ports
}

/// Each Ethernet and Wi-Fi interface's hardware address by its BSD name, from the I/O
/// Registry, which does not redact it: every `IOEthernetInterface`'s `BSD Name`, and the
/// `IOMACAddress` of the controller above it, as `ioreg` shows them.
#[cfg(target_os = "macos")]
fn hardware() -> BTreeMap<String, MacAddr> {
    use std::ffi::c_char;
    use std::ptr::NonNull;

    use objc2_core_foundation::{CFAllocator, CFData, CFDictionary, CFRetained, CFString, CFType};

    /// `<mach/port.h>` `mach_port_t`, and the I/O Kit object handles that are one.
    type Port = u32;
    // <IOKit/IOKitLib.h>
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        /// The default main port, for every lookup.
        static kIOMainPortDefault: Port;
        /// A matching dictionary for the class `name`, owned by the caller.
        fn IOServiceMatching(name: *const c_char) -> Option<NonNull<CFDictionary>>;
        /// An iterator over the services `matching` finds; consumes `matching`.
        fn IOServiceGetMatchingServices(
            main: Port,
            matching: NonNull<CFDictionary>,
            existing: *mut Port,
        ) -> i32;
        /// The iterator's next object, owned by the caller, or 0 at the end.
        fn IOIteratorNext(iterator: Port) -> Port;
        /// Release an object handle.
        fn IOObjectRelease(object: Port) -> i32;
        /// A property of `entry`, owned by the caller.
        fn IORegistryEntryCreateCFProperty(
            entry: Port,
            key: &CFString,
            allocator: Option<&CFAllocator>,
            options: u32,
        ) -> Option<NonNull<CFType>>;
        /// A property of `entry` or, with the options, of an entry above it, owned by the
        /// caller.
        fn IORegistryEntrySearchCFProperty(
            entry: Port,
            plane: *const c_char,
            key: &CFString,
            allocator: Option<&CFAllocator>,
            options: u32,
        ) -> Option<NonNull<CFType>>;
    }
    /// `<IOKit/IOKitKeys.h>` `kIORegistryIterateRecursively | kIORegistryIterateParents`.
    const UP_THE_TREE: u32 = 0x1 | 0x2;
    /// `<IOKit/IOKitKeys.h>` `kIOServicePlane`.
    const SERVICE_PLANE: &CStr = c"IOService";
    // `<IOKit/IOKitKeys.h>` `kIOBSDNameKey`.
    let bsd_name = CFString::from_static_str("BSD Name");
    // `<IOKit/network/IONetworkController.h>` `kIOMACAddress`.
    let mac_key = CFString::from_static_str("IOMACAddress");

    let mut out = BTreeMap::new();
    // SAFETY: IOKitLib.h: takes a NUL-terminated class name; returns an owned dictionary.
    let Some(matching) = (unsafe { IOServiceMatching(c"IOEthernetInterface".as_ptr()) }) else {
        return out;
    };
    let mut iterator: Port = 0;
    // SAFETY: IOKitLib.h: consumes `matching` whatever it returns, and on success stores an
    // iterator the caller releases; `kIOMainPortDefault` is a constant the framework exports.
    let found =
        unsafe { IOServiceGetMatchingServices(kIOMainPortDefault, matching, &raw mut iterator) };
    if found != 0 {
        return out;
    }
    loop {
        // SAFETY: IOKitLib.h: `iterator` is the live iterator; returns an owned object or 0.
        let entry = unsafe { IOIteratorNext(iterator) };
        if entry == 0 {
            break;
        }
        let property = |search: bool, key: &CFString| {
            let raw = if search {
                // SAFETY: IOKitLib.h: `entry` is a live registry entry, the plane a
                // NUL-terminated name and the key a CFString; returns an owned value or NULL.
                unsafe {
                    IORegistryEntrySearchCFProperty(
                        entry,
                        SERVICE_PLANE.as_ptr(),
                        key,
                        None,
                        UP_THE_TREE,
                    )
                }
            } else {
                // SAFETY: as above, on `entry` alone.
                unsafe { IORegistryEntryCreateCFProperty(entry, key, None, 0) }
            };
            // SAFETY: the Create Rule: the value is ours to release, once.
            raw.map(|raw| unsafe { CFRetained::from_raw(raw) })
        };
        let name = property(false, &bsd_name).and_then(|v| v.downcast::<CFString>().ok());
        let mac = property(true, &mac_key).and_then(|v| v.downcast::<CFData>().ok());
        if let (Some(name), Some(mac)) = (name, mac)
            && let Ok(mac) = <[u8; 6]>::try_from(mac.to_vec())
        {
            out.insert(name.to_string(), MacAddr(mac));
        }
        // SAFETY: IOKitLib.h: releases the object IOIteratorNext handed over, once.
        unsafe {
            IOObjectRelease(entry);
        }
    }
    // SAFETY: IOKitLib.h: releases the iterator IOServiceGetMatchingServices handed over, once.
    unsafe {
        IOObjectRelease(iterator);
    }
    out
}

/// Where `getifaddrs(3)` names every hardware address, nothing more is asked.
#[cfg(not(target_os = "macos"))]
const fn hardware() -> BTreeMap<String, MacAddr> {
    BTreeMap::new()
}

/// Every entry of `getifaddrs(3)` that is a hardware address or an IPv4 address.
fn entries() -> Vec<Entry> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs(3): on success stores the head of a list the caller frees with
    // freeifaddrs(3); on failure returns -1 and stores nothing.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        tracing::debug!(error = %std::io::Error::last_os_error(), "getifaddrs");
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut at = head.cast_const();
    while !at.is_null() {
        // SAFETY: getifaddrs(3): every node of the list stays valid until freeifaddrs(3).
        let node = unsafe { &*at };
        if let Some(entry) = entry(node) {
            out.push(entry);
        }
        at = node.ifa_next.cast_const();
    }
    // SAFETY: freeifaddrs(3) takes the head getifaddrs(3) returned, once; nothing read from
    // the list outlives this.
    unsafe {
        libc::freeifaddrs(head);
    }
    out
}

/// The entry `node` describes, if it is a hardware or an IPv4 address.
fn entry(node: &libc::ifaddrs) -> Option<Entry> {
    if node.ifa_addr.is_null() || node.ifa_name.is_null() {
        return None;
    }
    // SAFETY: getifaddrs(3): `ifa_name` is a NUL-terminated string that lives with the node.
    let name = unsafe { CStr::from_ptr(node.ifa_name) }.to_str().ok()?.to_owned();
    let flags = i64::from(node.ifa_flags);
    let has = |flag: libc::c_int| flags & i64::from(flag) != 0;
    let broadcasts = has(libc::IFF_UP)
        && has(libc::IFF_BROADCAST)
        && !has(libc::IFF_LOOPBACK)
        && !has(libc::IFF_POINTOPOINT);
    // SAFETY: getifaddrs(3): a non-null `ifa_addr` points at a sockaddr that lives with the
    // node.
    let addr = unsafe { &*node.ifa_addr };
    // SAFETY: getifaddrs(3): the sockaddr is as long as its family says (`sockaddr_bytes`).
    let raw = unsafe { sockaddr_bytes(addr) };
    let kind = if libc::c_int::from(addr.sa_family) == libc::AF_INET {
        let mask = if node.ifa_netmask.is_null() {
            None
        } else {
            // SAFETY: getifaddrs(3): a non-null `ifa_netmask` points at a sockaddr that lives
            // with the node.
            let mask = unsafe { &*node.ifa_netmask };
            // SAFETY: as for the address; BSD may cut a netmask's `sa_len` short, and
            // `ipv4` reads the bytes it is missing as zeros.
            ipv4(unsafe { sockaddr_bytes(mask) })
        };
        Kind::V4(ipv4(raw)?, prefix(mask?)?)
    } else {
        Kind::Link(link_mac(raw)?)
    };
    Some(Entry { name, broadcasts, kind })
}

/// The address of an `AF_INET` sockaddr's bytes: `sin_len` and `sin_family` then `sin_port`
/// on BSD, `sin_family` then `sin_port` on Linux, so it starts at byte 4 either way. Bytes a
/// short netmask leaves out read as zeros.
fn ipv4(raw: &[u8]) -> Option<Ipv4Addr> {
    let mut octets = [0_u8; 4];
    for (octet, byte) in octets.iter_mut().zip(raw.iter().skip(4)) {
        *octet = *byte;
    }
    (!raw.is_empty()).then(|| Ipv4Addr::from(octets))
}

/// A netmask's prefix length, `None` for one whose bits are not contiguous.
fn prefix(mask: Ipv4Addr) -> Option<u8> {
    let bits = u32::from(mask);
    let ones = bits.leading_ones();
    if bits.count_ones() == ones { u8::try_from(ones).ok() } else { None }
}

/// The bytes of `addr`: its own `sa_len` of them on BSD.
///
/// # Safety
/// `addr` is at least `sa_len` bytes long.
#[cfg(target_vendor = "apple")]
unsafe fn sockaddr_bytes(addr: &libc::sockaddr) -> &[u8] {
    let len = usize::from(addr.sa_len);
    // SAFETY: the caller's promise: `len` bytes from `addr` are the sockaddr's.
    unsafe { std::slice::from_raw_parts(std::ptr::from_ref(addr).cast::<u8>(), len) }
}

/// The bytes of `addr`: as many as its family's struct on Linux, which has no `sa_len`, and
/// none for a family not read here.
///
/// # Safety
/// `addr` is as long as its family's struct.
#[cfg(not(target_vendor = "apple"))]
unsafe fn sockaddr_bytes(addr: &libc::sockaddr) -> &[u8] {
    let len = match libc::c_int::from(addr.sa_family) {
        libc::AF_INET => size_of::<libc::sockaddr_in>(),
        libc::AF_PACKET => size_of::<libc::sockaddr_ll>(),
        _ => 0,
    };
    // SAFETY: the caller's promise: a sockaddr of this family is this long.
    unsafe { std::slice::from_raw_parts(std::ptr::from_ref(addr).cast::<u8>(), len) }
}

/// The Ethernet address of a link-layer entry.
#[cfg(target_vendor = "apple")]
fn link_mac(raw: &[u8]) -> Option<MacAddr> {
    sockaddr_dl_mac(raw)
}

/// The Ethernet address of a link-layer entry.
#[cfg(not(target_vendor = "apple"))]
fn link_mac(raw: &[u8]) -> Option<MacAddr> {
    sockaddr_ll_mac(raw)
}

/// `<net/if_dl.h>` `sockaddr_dl`: length, family, index (2), type, name length, address
/// length, selector length, then the name and the address in `sdl_data`.
#[cfg(any(target_vendor = "apple", test))]
fn sockaddr_dl_mac(raw: &[u8]) -> Option<MacAddr> {
    /// `<sys/socket.h>` `AF_LINK`.
    const AF_LINK: u8 = 18;
    /// `<net/if_types.h>` `IFT_ETHER`: the `sdl_type` of Ethernet and Wi-Fi alike.
    const IFT_ETHER: u8 = 0x6;
    let [_len, family, _, _, kind, nlen, alen, _slen, data @ ..] = raw else { return None };
    if (*family, *kind, *alen) != (AF_LINK, IFT_ETHER, 6) {
        return None;
    }
    let name = usize::from(*nlen);
    let bytes = data.get(name..name.checked_add(6)?)?;
    Some(MacAddr(bytes.try_into().ok()?))
}

/// `<linux/if_packet.h>` `sockaddr_ll`: family (2), protocol (2), index (4), hardware type
/// (2), packet type, address length, then the address.
#[cfg(any(not(target_vendor = "apple"), test))]
fn sockaddr_ll_mac(raw: &[u8]) -> Option<MacAddr> {
    /// `<sys/socket.h>` `AF_PACKET`.
    const AF_PACKET: u16 = 17;
    /// `<net/if_arp.h>` `ARPHRD_ETHER`.
    const ARPHRD_ETHER: u16 = 1;
    let [f0, f1, _, _, _, _, _, _, t0, t1, _pkttype, halen, addr @ ..] = raw else { return None };
    let family = u16::from_ne_bytes([*f0, *f1]);
    let hatype = u16::from_ne_bytes([*t0, *t1]);
    if (family, hatype, *halen) != (AF_PACKET, ARPHRD_ETHER, 6) {
        return None;
    }
    Some(MacAddr(addr.get(..6)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_proto::lan::MAGIC_LEN;

    use super::*;

    const STUDIO: MacAddr = MacAddr([0x3c, 0x22, 0xfb, 0x01, 0x02, 0x03]);

    fn link(name: &str, broadcasts: bool, mac: MacAddr) -> Entry {
        Entry { name: name.to_owned(), broadcasts, kind: Kind::Link(mac) }
    }

    fn v4(name: &str, broadcasts: bool, addr: [u8; 4], prefix: u8) -> Entry {
        Entry { name: name.to_owned(), broadcasts, kind: Kind::V4(Ipv4Addr::from(addr), prefix) }
    }

    /// The Ethernet card with a subnet is a port; loopback, the tailnet's tunnel, a card with no
    /// address, a self-assigned address and a group MAC are not.
    #[test]
    fn only_a_broadcasting_card_with_a_subnet_is_a_port() {
        let wifi = MacAddr([0x3e, 0x22, 0xfb, 0x0a, 0x0b, 0x0c]);
        let entries = [
            link("lo0", false, MacAddr([0; 6])),
            v4("lo0", false, [127, 0, 0, 1], 8),
            link("en0", true, STUDIO),
            v4("en0", true, [192, 168, 1, 20], 24),
            link("en1", true, wifi),
            v4("en1", true, [169, 254, 3, 4], 16),
            link("awdl0", true, MacAddr([0x5e, 0, 0, 0, 0, 1])),
            v4("utun4", false, [100, 64, 0, 3], 32),
            link("en7", true, MacAddr([0x01, 0, 0x5e, 0, 0, 1])),
            v4("en7", true, [10, 0, 0, 2], 24),
            link("en8", false, MacAddr([0x3c, 0, 0, 0, 0, 8])),
            v4("en8", false, [10, 1, 0, 2], 24),
        ];
        let ports = lan_ports(&entries, &BTreeMap::new());
        assert_eq!(
            ports,
            [LanPort {
                interface: "en0".to_owned(),
                mac: STUDIO,
                addr: Ipv4Addr::new(192, 168, 1, 20),
                prefix: 24
            }]
        );
    }

    /// A redacted `AF_LINK` address is replaced by the one the I/O Registry names, and an
    /// interface it does not name is no port; a real address in the entry stands.
    #[test]
    fn a_redacted_address_is_taken_from_the_hardware_list() {
        let entries = [
            link("en0", true, REDACTED),
            v4("en0", true, [192, 168, 1, 20], 24),
            link("en1", true, REDACTED),
            v4("en1", true, [192, 168, 1, 21], 24),
            link("en2", true, STUDIO),
            v4("en2", true, [10, 0, 0, 2], 24),
        ];
        let wired = MacAddr([0x9c, 0x76, 0x0e, 0x37, 0x42, 0x4e]);
        let hardware =
            BTreeMap::from([("en0".to_owned(), wired), ("en2".to_owned(), MacAddr([0x9c; 6]))]);
        let ports = lan_ports(&entries, &hardware);
        let named: Vec<_> = ports.iter().map(|p| (p.interface.as_str(), p.mac)).collect();
        assert_eq!(named, [("en0", wired), ("en2", STUDIO)]);
    }

    /// An Apple `sockaddr_dl` as the kernel writes it for `en0`: the name, then the address.
    #[test]
    fn a_link_address_is_read_after_the_interface_name() {
        let mut raw = vec![20, 18, 4, 0, 6, 3, 6, 0];
        raw.extend_from_slice(b"en0");
        raw.extend_from_slice(&STUDIO.0);
        raw.extend_from_slice(&[0, 0, 0]);
        assert_eq!(sockaddr_dl_mac(&raw), Some(STUDIO));
        let mut bridge = raw.clone();
        bridge[4] = 0xd1;
        assert_eq!(sockaddr_dl_mac(&bridge), None, "not Ethernet");
        let mut short = raw.clone();
        short[6] = 0;
        assert_eq!(sockaddr_dl_mac(&short), None, "no address");
        assert_eq!(sockaddr_dl_mac(&raw[..12]), None, "cut short");
    }

    /// A Linux `sockaddr_ll` for `eth0`.
    #[test]
    fn a_packet_address_is_read_after_its_header() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&17_u16.to_ne_bytes());
        raw.extend_from_slice(&[0, 0, 2, 0, 0, 0]);
        raw.extend_from_slice(&1_u16.to_ne_bytes());
        raw.extend_from_slice(&[0, 6]);
        raw.extend_from_slice(&STUDIO.0);
        raw.extend_from_slice(&[0, 0]);
        assert_eq!(sockaddr_ll_mac(&raw), Some(STUDIO));
        let mut loopback = raw.clone();
        loopback[8..10].copy_from_slice(&772_u16.to_ne_bytes());
        assert_eq!(sockaddr_ll_mac(&loopback), None);
    }

    /// Each sleeping port is sent for from the first own port on its subnet; one on no subnet
    /// here is left out, and so is the machine's own address.
    #[test]
    fn a_sleeping_port_is_sent_for_from_the_port_on_its_subnet() {
        let port = |interface: &str, addr: [u8; 4], mac: u8| LanPort {
            interface: interface.to_owned(),
            mac: MacAddr([0x3c, 0, 0, 0, 0, mac]),
            addr: Ipv4Addr::from(addr),
            prefix: 24,
        };
        let own = [port("en0", [192, 168, 1, 2], 1), port("en1", [10, 0, 0, 2], 2)];
        let peer = [
            port("en0", [192, 168, 1, 20], 10),
            port("en1", [192, 168, 1, 21], 11),
            port("en5", [172, 16, 0, 20], 12),
        ];
        let plan = plan(&own, &peer);
        assert_eq!(plan.len(), 1);
        let (from, targets) = &plan[0];
        assert_eq!(from.interface, "en0");
        let names: Vec<_> = targets.iter().map(|t| t.interface.as_str()).collect();
        assert_eq!(names, ["en0", "en1"]);
        assert_eq!(super::plan(&own, &[port("en5", [172, 16, 0, 20], 12)]), []);
        assert!(super::plan(&own, &[port("en0", [192, 168, 1, 2], 1)]).is_empty(), "itself");
    }

    /// A contiguous netmask is a prefix; a mask with holes is none.
    #[test]
    fn a_netmask_is_a_prefix_length() {
        assert_eq!(prefix(Ipv4Addr::new(255, 255, 255, 0)), Some(24));
        assert_eq!(prefix(Ipv4Addr::new(255, 255, 240, 0)), Some(20));
        assert_eq!(prefix(Ipv4Addr::UNSPECIFIED), Some(0));
        assert_eq!(prefix(Ipv4Addr::new(255, 0, 255, 0)), None);
    }

    /// This machine's own list reads: every port has a unicast MAC, a real subnet and an
    /// address that is not loopback or self-assigned.
    #[test]
    fn this_machine_lists_its_ports() {
        let ports = ports();
        assert!(ports.len() <= MAX_PORTS);
        for port in &ports {
            assert!(port.mac.is_unicast() && port.mac != REDACTED, "{port:?}");
            assert!((1..=32).contains(&port.prefix), "{port:?}");
            assert!(!port.addr.is_loopback() && !port.addr.is_link_local(), "{port:?}");
            assert!(port.reaches(port.addr), "{port:?}");
        }
    }

    /// Each MAC's packet goes five times, 100 ms apart, byte for byte. Sent over loopback to a
    /// socket of the test's own: nothing reaches the LAN.
    #[tokio::test]
    async fn each_packet_goes_five_times_a_tenth_of_a_second_apart() {
        let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let to = receiver.local_addr().unwrap();
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let wifi = MacAddr([0x3e, 0x22, 0xfb, 0x0a, 0x0b, 0x0c]);
        let started = Instant::now();
        let sent = tokio::spawn(async move { send(&sender, to, &[STUDIO, wifi]).await });
        let mut heard = Vec::new();
        let mut buf = [0_u8; 256];
        for _ in 0..10 {
            let (n, _) = tokio::time::timeout(Duration::from_secs(5), receiver.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
            heard.push((buf[..n].to_vec(), started.elapsed()));
        }
        sent.await.unwrap().unwrap();
        for (i, (packet, _)) in heard.iter().enumerate() {
            let mac = if i % 2 == 0 { STUDIO } else { wifi };
            assert_eq!(packet.len(), MAGIC_LEN);
            assert_eq!(packet.as_slice(), magic_packet(mac).as_slice(), "packet {i}");
        }
        let last = heard.last().unwrap().1;
        assert!(last >= WAKE_GAP * 4, "five rounds take four gaps: {last:?}");
        assert!(last < Duration::from_secs(2), "{last:?}");
    }
}
