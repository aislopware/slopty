//! The LAN a worker sits on, beneath the tailnet: its interfaces, and the magic packet a machine
//! beside it sends to wake it (`docs/decisions/workers.md`, "A sleeping worker is woken from
//! its own LAN").
//!
//! A Mac asleep drops off the tailnet, so nothing reaches it over Tailscale. What still reaches
//! it is an Ethernet frame on its own segment: its network card listens for six `0xFF` bytes and
//! then its MAC address sixteen times, and wakes the machine ("Wake for network access",
//! `pmset womp`). A worker reports its interfaces in its [`crate::server::WorkerCaps`]; the
//! server, or another worker on the same subnet, broadcasts the packet there.

use std::net::Ipv4Addr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The UDP port the magic packet goes to: discard, as Jump and most senders use. The card reads
/// the frame whatever the port; the port only has to be one nothing answers on.
pub const WAKE_PORT: u16 = 9;
/// Times the packet is sent: a switch that has aged the sleeping port out of its table floods
/// the first ones, and a packet lost on Wi-Fi is not missed.
pub const WAKE_REPEATS: u32 = 5;
/// Between two sends of the packet, as Jump spaces them.
pub const WAKE_GAP: Duration = Duration::from_millis(100);
/// Bytes of a magic packet: six of `0xFF`, then the MAC sixteen times.
pub const MAGIC_LEN: usize = 102;

/// A 48-bit hardware address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    /// Whether the address is usable as a wake target: not all zeros, not a group address.
    #[must_use]
    pub fn is_unicast(self) -> bool {
        let [first, ..] = self.0;
        self.0 != [0; 6] && first & 1 == 0
    }
}

impl std::fmt::Display for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut sep = "";
        for byte in self.0 {
            write!(f, "{sep}{byte:02x}")?;
            sep = ":";
        }
        Ok(())
    }
}

impl std::fmt::Debug for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

/// Text that is not a MAC address.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("a MAC address is six hex bytes, as aa:bb:cc:dd:ee:ff")]
pub struct BadMac;

impl std::str::FromStr for MacAddr {
    type Err = BadMac;

    /// `aa:bb:cc:dd:ee:ff`, or with `-` between the bytes.
    fn from_str(s: &str) -> Result<Self, BadMac> {
        let mut out = [0_u8; 6];
        let mut parts = s.trim().split([':', '-']);
        for byte in &mut out {
            let part = parts.next().ok_or(BadMac)?;
            if part.len() != 2 {
                return Err(BadMac);
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_not_hex| BadMac)?;
        }
        if parts.next().is_some() {
            return Err(BadMac);
        }
        Ok(Self(out))
    }
}

/// One interface of a worker on a LAN: its hardware address and its IPv4 subnet.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct LanPort {
    /// The interface's name on its machine (`en0`, `eth0`), for people to read.
    pub interface: String,
    /// Its hardware address, the one the magic packet names.
    pub mac: MacAddr,
    /// Its IPv4 address on the LAN.
    pub addr: Ipv4Addr,
    /// The subnet's prefix length (24 for a /24).
    pub prefix: u8,
}

impl LanPort {
    /// The subnet mask.
    #[must_use]
    pub fn netmask(&self) -> Ipv4Addr {
        let bits = u32::MAX.checked_shl(32_u32.saturating_sub(u32::from(self.prefix))).unwrap_or(0);
        Ipv4Addr::from(bits)
    }

    /// The subnet's broadcast address, where the magic packet goes.
    #[must_use]
    pub fn broadcast(&self) -> Ipv4Addr {
        self.addr | !self.netmask()
    }

    /// Whether `addr` is on this port's subnet: a broadcast from here reaches it.
    #[must_use]
    pub fn reaches(&self, addr: Ipv4Addr) -> bool {
        let mask = self.netmask();
        self.prefix > 0 && self.addr & mask == addr & mask
    }
}

/// The magic packet that wakes the machine whose card has `mac`.
#[must_use]
pub fn magic_packet(mac: MacAddr) -> [u8; MAGIC_LEN] {
    let mut packet = [0xff; MAGIC_LEN];
    for copy in packet.as_chunks_mut::<6>().0.iter_mut().skip(1) {
        copy.copy_from_slice(&mac.0);
    }
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(addr: [u8; 4], prefix: u8) -> LanPort {
        LanPort {
            interface: "en0".to_owned(),
            mac: MacAddr([0x3c, 0x22, 0xfb, 0x01, 0x02, 0x03]),
            addr: Ipv4Addr::from(addr),
            prefix,
        }
    }

    /// Six `0xFF`, then the MAC sixteen times, and nothing else.
    #[test]
    fn the_magic_packet_is_the_mac_sixteen_times_after_a_sync() {
        let mac = MacAddr([0x3c, 0x22, 0xfb, 0x01, 0x02, 0x03]);
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 102);
        assert_eq!(packet[..6], [0xff; 6]);
        for i in 0..16 {
            assert_eq!(packet[6 + i * 6..12 + i * 6], mac.0, "copy {i}");
        }
    }

    /// The broadcast and the subnet follow the prefix; a /0 reaches nothing it did not mean to.
    #[test]
    fn a_port_knows_its_subnet() {
        let lan = port([192, 168, 1, 20], 24);
        assert_eq!(lan.netmask(), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(lan.broadcast(), Ipv4Addr::new(192, 168, 1, 255));
        assert!(lan.reaches(Ipv4Addr::new(192, 168, 1, 7)));
        assert!(!lan.reaches(Ipv4Addr::new(192, 168, 2, 7)));
        let wide = port([10, 0, 3, 4], 20);
        assert_eq!(wide.broadcast(), Ipv4Addr::new(10, 0, 15, 255));
        assert!(wide.reaches(Ipv4Addr::new(10, 0, 0, 1)));
        let host = port([10, 0, 3, 4], 32);
        assert_eq!(host.broadcast(), Ipv4Addr::new(10, 0, 3, 4));
        assert!(!port([10, 0, 3, 4], 0).reaches(Ipv4Addr::new(1, 1, 1, 1)));
    }

    /// A MAC reads and prints as colon-separated hex; a group or zero address is no target.
    #[test]
    fn a_mac_reads_and_prints_as_hex() {
        let mac: MacAddr = "3c:22:FB:01:02:03".parse().unwrap();
        assert_eq!(mac.to_string(), "3c:22:fb:01:02:03");
        assert_eq!("3c-22-fb-01-02-03".parse::<MacAddr>(), Ok(mac));
        for bad in
            ["3c:22:fb:01:02", "3c:22:fb:01:02:03:04", "3c:22:fb:1:02:03", "zz:22:fb:01:02:03"]
        {
            assert_eq!(bad.parse::<MacAddr>(), Err(BadMac), "{bad}");
        }
        assert!(mac.is_unicast());
        assert!(!MacAddr([0; 6]).is_unicast());
        assert!(!MacAddr([0x01, 0, 0x5e, 0, 0, 1]).is_unicast(), "multicast");
    }
}
