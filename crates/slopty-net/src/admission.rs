//! Which peers a worker lets in: decided once per incoming connection, by source address.
//!
//! With no encryption and no pairing, the network is the boundary: a worker answers loopback,
//! its tailnet and the private LAN it sits on, and nothing else. A peer outside those ranges
//! is refused before the handshake, so it costs one packet and no state.

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::NetError;

/// An address range in CIDR notation (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`); a bare address
/// is a range of one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// `net/prefix`, with the host bits of `net` cleared.
    pub fn new(net: IpAddr, prefix: u8) -> Result<Self, NetError> {
        let bits = if net.is_ipv4() { 32 } else { 128 };
        if prefix > bits {
            return Err(NetError::Address(format!("{net}/{prefix}: prefix past {bits}")));
        }
        let net = match net {
            IpAddr::V4(v4) => IpAddr::V4((u32::from(v4) & mask32(prefix)).into()),
            IpAddr::V6(v6) => IpAddr::V6((u128::from(v6) & mask128(prefix)).into()),
        };
        Ok(Self { net, prefix })
    }

    /// Whether `ip` falls in the range. An IPv4-mapped IPv6 address counts as its IPv4 self.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                u32::from(ip) & mask32(self.prefix) == u32::from(net)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                u128::from(ip) & mask128(self.prefix) == u128::from(net)
            }
            (IpAddr::V4(_), IpAddr::V6(_)) | (IpAddr::V6(_), IpAddr::V4(_)) => false,
        }
    }
}

fn mask32(prefix: u8) -> u32 {
    u32::MAX.checked_shl(32_u32.saturating_sub(u32::from(prefix))).unwrap_or(0)
}

fn mask128(prefix: u8) -> u128 {
    u128::MAX.checked_shl(128_u32.saturating_sub(u32::from(prefix))).unwrap_or(0)
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.net, self.prefix)
    }
}

impl std::str::FromStr for Cidr {
    type Err = NetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let bad = || NetError::Address(format!("{s:?}: not an address range"));
        let (net, prefix) = if let Some((net, prefix)) = s.split_once('/') {
            let net: IpAddr = net.parse().map_err(|_ip| bad())?;
            (net, prefix.parse::<u8>().map_err(|_n| bad())?)
        } else {
            let net: IpAddr = s.parse().map_err(|_ip| bad())?;
            (net, if net.is_ipv4() { 32 } else { 128 })
        };
        Self::new(net, prefix)
    }
}

impl TryFrom<String> for Cidr {
    type Error = NetError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Cidr> for String {
    fn from(c: Cidr) -> Self {
        c.to_string()
    }
}

/// The ranges admitted when the worker settings name none: private networks only.
pub const DEFAULT_ALLOW: &[&str] = &[
    // Tailscale: its CGNAT block and its IPv6 ULA prefix.
    "100.64.0.0/10",
    "fd7a:115c:a1e0::/48",
    // RFC 1918 LANs.
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    // Unique local IPv6 (other VPNs, some LANs).
    "fc00::/7",
    // Link-local, both families.
    "169.254.0.0/16",
    "fe80::/10",
];

/// Who may connect: loopback always, then the configured ranges.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Admission {
    allow: Vec<Cidr>,
}

impl Admission {
    /// Loopback plus `allow`; an empty `allow` means [`DEFAULT_ALLOW`]. A list replaces the
    /// defaults rather than adding to them, so it can narrow as well as widen.
    #[must_use]
    pub fn new(allow: Vec<Cidr>) -> Self {
        if allow.is_empty() { Self::default() } else { Self { allow } }
    }

    /// Whether a peer at `ip` may connect.
    #[must_use]
    pub fn admits(&self, ip: IpAddr) -> bool {
        ip.to_canonical().is_loopback() || self.allow.iter().any(|c| c.contains(ip))
    }

    /// The ranges besides loopback.
    #[must_use]
    pub fn ranges(&self) -> &[Cidr] {
        &self.allow
    }
}

impl Default for Admission {
    fn default() -> Self {
        Self { allow: DEFAULT_ALLOW.iter().filter_map(|c| c.parse().ok()).collect() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn the_defaults_let_in_loopback_the_tailnet_and_the_lan_only() {
        let a = Admission::default();
        assert_eq!(a.ranges().len(), DEFAULT_ALLOW.len(), "every default parses");
        for yes in [
            "127.0.0.1",
            "127.9.9.9",
            "::1",
            "::ffff:127.0.0.1",
            "100.64.0.3",
            "100.127.255.254",
            "::ffff:100.101.102.103",
            "fd7a:115c:a1e0:ab12::1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.100.240",
            "::ffff:192.168.1.1",
            "fd00::1",
            "fe80::1",
            "169.254.3.4",
        ] {
            assert!(a.admits(ip(yes)), "{yes} is private and should be admitted");
        }
        for no in [
            "8.8.8.8",
            "100.63.255.255",
            "100.128.0.0",
            "172.32.0.1",
            "192.169.0.1",
            "2001:4860:4860::8888",
            "::ffff:8.8.8.8",
            "0.0.0.0",
            "::",
        ] {
            assert!(!a.admits(ip(no)), "{no} is public and should be refused");
        }
    }

    #[test]
    fn an_allow_list_replaces_the_defaults_but_never_loopback() {
        let a =
            Admission::new(vec!["100.64.0.3".parse().unwrap(), "2001:db8::/32".parse().unwrap()]);
        assert!(a.admits(ip("100.64.0.3")));
        assert!(!a.admits(ip("100.64.0.4")), "narrowed to one tailnet peer");
        assert!(!a.admits(ip("192.168.1.1")), "the LAN default is gone");
        assert!(a.admits(ip("2001:db8:1::5")), "widened to a public range");
        assert!(a.admits(ip("127.0.0.1")) && a.admits(ip("::1")), "loopback always");
        assert_eq!(Admission::new(Vec::new()), Admission::default());
    }

    #[test]
    fn ranges_parse_normalise_and_refuse_nonsense() {
        let c: Cidr = "10.1.2.3/8".parse().unwrap();
        assert_eq!(c.to_string(), "10.0.0.0/8", "host bits cleared");
        assert_eq!(
            "fd7a:115c:a1e0::5/48".parse::<Cidr>().unwrap().to_string(),
            "fd7a:115c:a1e0::/48"
        );
        assert_eq!("1.2.3.4".parse::<Cidr>().unwrap().to_string(), "1.2.3.4/32");
        let everything: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(everything.contains(ip("8.8.8.8")) && !everything.contains(ip("::1")));
        assert!("::/0".parse::<Cidr>().unwrap().contains(ip("2001::1")));
        for bad in ["", "10.0.0.0/33", "::/129", "10.0.0/8", "worker", "10.0.0.0/x", "/8"] {
            assert!(bad.parse::<Cidr>().is_err(), "{bad:?}");
        }
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Cidr>(&json).unwrap(), c);
    }
}
