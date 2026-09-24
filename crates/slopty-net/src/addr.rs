//! How a host is named: `host[:port]`.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::NetError;
use crate::endpoint::WORKER_PORT;

/// A host as a person types it, with an optional port ([`WORKER_PORT`] when absent).
///
/// The host is a Tailscale `MagicDNS` name, a LAN name or an IP address. IPv6 literals take
/// brackets when they carry a port (`[fd7a:115c:a1e0::1]:45550`) and may go without when they
/// do not.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostAddr {
    host: String,
    port: u16,
}

impl HostAddr {
    /// A host and port.
    #[must_use]
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self { host: host.into(), port }
    }

    /// The name or IP address, without brackets.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The UDP port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// The addresses the name stands for, IP literals without a lookup.
    pub async fn resolve(&self) -> Result<Vec<SocketAddr>, NetError> {
        if let Ok(ip) = self.host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, self.port)]);
        }
        let found = tokio::net::lookup_host((self.host.as_str(), self.port))
            .await
            .map_err(|e| NetError::Resolve(format!("{self}: {e}")))?
            .collect::<Vec<_>>();
        if found.is_empty() {
            return Err(NetError::Resolve(format!("{self}: no address")));
        }
        Ok(found)
    }
}

impl From<SocketAddr> for HostAddr {
    fn from(addr: SocketAddr) -> Self {
        Self::new(addr.ip().to_canonical().to_string(), addr.port())
    }
}

impl fmt::Display for HostAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

impl HostAddr {
    /// Parse `host[:port]` with `default_port` when it names none ([`WORKER_PORT`] is what
    /// [`str::parse`] assumes; a server address wants [`crate::endpoint::SERVER_PORT`]).
    pub fn parse_with_port(s: &str, default_port: u16) -> Result<Self, NetError> {
        let s = s.trim();
        let bad = |why: &str| NetError::Address(format!("{s:?}: {why}"));
        let port =
            |p: &str| p.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| bad("bad port"));
        let (host, port) = if let Some(rest) = s.strip_prefix('[') {
            let (host, after) = rest.split_once(']').ok_or_else(|| bad("no closing ]"))?;
            match after {
                "" => (host, default_port),
                _ => (host, port(after.strip_prefix(':').ok_or_else(|| bad("junk after ]"))?)?),
            }
        } else if s.parse::<std::net::Ipv6Addr>().is_ok() {
            (s, default_port)
        } else {
            match s.rsplit_once(':') {
                Some((host, p)) => (host, port(p)?),
                None => (s, default_port),
            }
        };
        if host.is_empty() {
            return Err(bad("no host"));
        }
        if host.contains(':') && host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(bad("not an IPv6 address"));
        }
        if host.chars().any(|c| c.is_whitespace() || c == '/') {
            return Err(bad("not a host name"));
        }
        Ok(Self::new(host, port))
    }
}

impl std::str::FromStr for HostAddr {
    type Err = NetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_with_port(s, WORKER_PORT)
    }
}

impl TryFrom<String> for HostAddr {
    type Error = NetError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<HostAddr> for String {
    fn from(addr: HostAddr) -> Self {
        addr.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_reads_with_and_without_its_port() {
        let cases = [
            ("mac-studio", "mac-studio", WORKER_PORT),
            ("mac-studio.tail1234.ts.net:4000", "mac-studio.tail1234.ts.net", 4000),
            (" 100.64.0.3 ", "100.64.0.3", WORKER_PORT),
            ("100.64.0.3:45551", "100.64.0.3", 45551),
            ("fd7a:115c:a1e0::1", "fd7a:115c:a1e0::1", WORKER_PORT),
            ("[fd7a:115c:a1e0::1]", "fd7a:115c:a1e0::1", WORKER_PORT),
            ("[fd7a:115c:a1e0::1]:45552", "fd7a:115c:a1e0::1", 45552),
            ("[::1]:9", "::1", 9),
        ];
        for (text, host, port) in cases {
            let addr: HostAddr = text.parse().unwrap();
            assert_eq!((addr.host(), addr.port()), (host, port), "{text:?}");
            assert_eq!(addr.to_string().parse::<HostAddr>().unwrap(), addr, "{text:?} round trips");
        }
        assert_eq!("[::1]:9".parse::<HostAddr>().unwrap().to_string(), "[::1]:9");
    }

    #[test]
    fn a_server_address_defaults_to_the_server_port() {
        use crate::endpoint::SERVER_PORT;
        let named = HostAddr::parse_with_port("studio", SERVER_PORT).unwrap();
        assert_eq!((named.host(), named.port()), ("studio", SERVER_PORT));
        let v6 = HostAddr::parse_with_port("[::1]", SERVER_PORT).unwrap();
        assert_eq!(v6.port(), SERVER_PORT);
        let explicit = HostAddr::parse_with_port("studio:7", SERVER_PORT).unwrap();
        assert_eq!(explicit.port(), 7, "an explicit port wins");
        HostAddr::parse_with_port("a b", SERVER_PORT).unwrap_err();
    }

    #[test]
    fn nonsense_is_an_error() {
        for bad in
            ["", ":45550", "host:", "host:0", "host:99999", "[::1", "[::1]x", "a:b:c", "a b", "h/x"]
        {
            assert!(bad.parse::<HostAddr>().is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn it_serialises_as_its_text() {
        let addr: HostAddr = "studio:45551".parse().unwrap();
        let json = serde_json::to_string(&addr).unwrap();
        assert_eq!(json, "\"studio:45551\"");
        assert_eq!(serde_json::from_str::<HostAddr>(&json).unwrap(), addr);
        serde_json::from_str::<HostAddr>("\"a b\"").unwrap_err();
    }

    #[tokio::test]
    async fn an_ip_resolves_without_a_lookup_and_a_name_through_one() {
        let ip: HostAddr = "127.0.0.1:7".parse().unwrap();
        assert_eq!(ip.resolve().await.unwrap(), vec![SocketAddr::from(([127, 0, 0, 1], 7))]);
        let local: HostAddr = "localhost:7".parse().unwrap();
        let found = local.resolve().await.unwrap();
        assert!(found.iter().all(|a| a.ip().is_loopback() && a.port() == 7), "{found:?}");
        let nowhere: HostAddr = "nowhere.invalid".parse().unwrap();
        nowhere.resolve().await.unwrap_err();
    }
}
