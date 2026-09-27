//! Which peers a listener lets in, and in what role: decided once per incoming connection.
//!
//! With no encryption and no pairing, the tailnet is the boundary, and Tailscale says who is on
//! the other end. A peer is let in when it is:
//!
//! * on loopback, as anything;
//! * in a range the settings list (`[worker] allow`, for a plain VPN), as anything, since an
//!   address is all such a network says;
//! * a node of the tailnet the local Tailscale vouches for: the user's own machines as anything,
//!   another user's or a tagged node in the roles a tailnet grant gives it
//!   (`slopty_tailnet::policy`). Where no Tailscale this process can read is running, a tailnet
//!   address is let in by address, as before there was a daemon to ask.
//!
//! Anything else is refused before the handshake, so it costs one packet and no state.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use slopty_tailnet::{Grant, LocalApi};

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

/// Tailscale's addresses: its CGNAT block and its IPv6 ULA prefix.
pub const TAILNET: &[&str] = &["100.64.0.0/10", "fd7a:115c:a1e0::/48"];

/// How long the local node's owner is taken as read before it is asked again.
const OWNER_TTL: Duration = Duration::from_secs(60);

/// What the check decided of a peer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Let in, in the roles the grant gives.
    Admit(Grant),
    /// Refused, and why, for the log.
    Refuse(&'static str),
}

/// Who may connect: loopback always, the listed ranges, and the tailnet by its word.
#[derive(Clone, Debug)]
pub struct Admission {
    allow: Vec<Cidr>,
    tailnet: Tailnet,
}

/// The local Tailscale, and the user this node belongs to as it last said.
#[derive(Clone, Debug)]
struct Tailnet {
    source: Source,
    owner: Arc<Mutex<Option<Owner>>>,
}

/// Where the local Tailscale's `LocalAPI` comes from.
#[derive(Clone, Debug)]
enum Source {
    /// Given once: a test's stand-in, or none.
    Fixed(Option<LocalApi>),
    /// This machine's, looked up when first needed, again while none is found, and again
    /// after it fails. A daemon started at login ahead of Tailscale, or an App Store extension
    /// that restarted on a new port and token, is found without a restart.
    Machine(Arc<Machine>),
}

/// How this machine's `LocalAPI` is found, and the last finding.
#[derive(Debug)]
struct Machine {
    find: fn() -> Option<LocalApi>,
    relook: Duration,
    lookup: Mutex<Lookup>,
}

/// The last lookup of this machine's `LocalAPI`.
#[derive(Debug, Default)]
struct Lookup {
    api: Option<LocalApi>,
    looked: Option<Instant>,
}

/// How long a lookup that found no Tailscale stands before the next check looks again.
const RELOOK: Duration = Duration::from_secs(5);

/// The user the local node belongs to, `None` when it is tagged, and when that was read.
#[derive(Clone, Copy, Debug)]
struct Owner {
    read: Instant,
    user: Option<i64>,
}

/// The ranges of an `allow` list that parse; `section` names the list in the warning a range
/// that does not parse leaves in the log.
#[must_use]
pub fn parse_allow(ranges: &[String], section: &str) -> Vec<Cidr> {
    ranges
        .iter()
        .filter_map(|range| match range.parse::<Cidr>() {
            Ok(cidr) => Some(cidr),
            Err(e) => {
                tracing::warn!(%range, error = %e, "{section} allow: skipped");
                None
            }
        })
        .collect()
}

impl Admission {
    /// Loopback, the ranges in `allow`, and the tailnet through this machine's Tailscale,
    /// looked up when a tailnet peer first calls.
    #[must_use]
    pub fn new(allow: Vec<Cidr>) -> Self {
        Self::finding(allow, LocalApi::find, RELOOK)
    }

    /// [`Self::new`] with `find` in place of looking at this machine, looked up again at most
    /// every `relook` while it finds none.
    fn finding(allow: Vec<Cidr>, find: fn() -> Option<LocalApi>, relook: Duration) -> Self {
        let machine = Machine { find, relook, lookup: Mutex::default() };
        Self { allow, tailnet: Tailnet::new(Source::Machine(Arc::new(machine))) }
    }

    /// Loopback, `allow`, and the tailnet through `api`; without one, no tailnet peer.
    #[must_use]
    pub fn with_tailnet(allow: Vec<Cidr>, api: Option<LocalApi>) -> Self {
        Self { allow, tailnet: Tailnet::new(Source::Fixed(api)) }
    }

    /// The local Tailscale this admission asks, when one is running that this process can
    /// read.
    #[must_use]
    pub fn local_api(&self) -> Option<LocalApi> {
        self.tailnet.api()
    }

    /// The ranges let in by address besides loopback and the tailnet.
    #[must_use]
    pub fn ranges(&self) -> &[Cidr] {
        &self.allow
    }

    /// Whether the peer at `peer` may connect, and as what. A tailnet address is let in only
    /// as Tailscale vouches for it: with the wire unencrypted, an address alone is what a host
    /// on the same LAN could forge.
    pub async fn check(&self, peer: SocketAddr) -> Verdict {
        let ip = peer.ip().to_canonical();
        if ip.is_loopback() || self.allow.iter().any(|c| c.contains(ip)) {
            return Verdict::Admit(Grant::ALL);
        }
        if !on_tailnet(ip) {
            return Verdict::Refuse("outside the tailnet and the allowed ranges");
        }
        self.tailnet.check(SocketAddr::new(ip, peer.port())).await
    }
}

impl Default for Admission {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// Whether `ip` is one of Tailscale's addresses.
#[must_use]
pub fn on_tailnet(ip: IpAddr) -> bool {
    TAILNET.iter().filter_map(|c| c.parse::<Cidr>().ok()).any(|c| c.contains(ip))
}

impl Tailnet {
    fn new(source: Source) -> Self {
        Self { source, owner: Arc::default() }
    }

    fn api(&self) -> Option<LocalApi> {
        match &self.source {
            Source::Fixed(api) => api.clone(),
            Source::Machine(machine) => {
                let mut lookup = machine.lookup.lock();
                let due = lookup.looked.is_none_or(|at| at.elapsed() >= machine.relook);
                if lookup.api.is_none() && due {
                    lookup.api = (machine.find)();
                    lookup.looked = Some(Instant::now());
                }
                lookup.api.clone()
            }
        }
    }

    /// The daemon did not answer as found: the next [`Self::api`] looks it up again.
    fn lost(&self) {
        if let Source::Machine(machine) = &self.source {
            *machine.lookup.lock() = Lookup::default();
        }
    }

    async fn check(&self, peer: SocketAddr) -> Verdict {
        let Some(mut api) = self.api() else {
            return Verdict::Refuse("no Tailscale this process can read to say who is calling");
        };
        let mut who = api.whois(peer).await;
        if let Err(e) = &who
            && matches!(self.source, Source::Machine(_))
        {
            tracing::info!(error = %e, "tailscale did not answer; looking it up again");
            self.lost();
            if let Some(again) = self.api() {
                who = again.whois(peer).await;
                api = again;
            }
        }
        match who {
            Ok(Some(who)) => {
                let grant = Grant::of(&who, self.owner(&api).await);
                if grant.any() {
                    Verdict::Admit(grant)
                } else {
                    tracing::info!(%peer, node = %who.node.name, user = %who.user_profile.login_name, "the tailnet grants no role");
                    Verdict::Refuse("the tailnet grants it no role here")
                }
            }
            Ok(None) => Verdict::Refuse("no node of the tailnet has this address"),
            Err(e) => {
                tracing::warn!(%peer, error = %e, "tailscale could not say who is calling");
                Verdict::Refuse("tailscale could not say who is calling")
            }
        }
    }

    /// The user this node belongs to, `None` when it is tagged (a tagged node belongs to
    /// nobody). Read at most once a minute; a daemon that does not answer keeps the last word.
    async fn owner(&self, api: &LocalApi) -> Option<i64> {
        let known = *self.owner.lock();
        if let Some(known) = known
            && known.read.elapsed() < OWNER_TTL
        {
            return known.user;
        }
        match api.status().await {
            Ok(status) => {
                let user = status.me.filter(|me| me.tags.is_empty()).map(|me| me.user);
                *self.owner.lock() = Some(Owner { read: Instant::now(), user });
                user
            }
            Err(e) => {
                tracing::warn!(error = %e, "tailscale did not say who owns this node");
                known.and_then(|known| known.user)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_tailnet::Role;

    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn at(s: &str) -> SocketAddr {
        SocketAddr::new(ip(s), 5000)
    }

    /// With no Tailscale to ask, loopback and a listed range get in and nothing else does: not
    /// the tailnet by its address alone, and not the LAN.
    #[tokio::test]
    async fn without_a_daemon_only_loopback_and_the_listed_ranges_get_in() {
        let a = Admission::with_tailnet(vec!["10.0.0.0/8".parse().unwrap()], None);
        for yes in ["127.0.0.1", "::1", "::ffff:127.0.0.1", "10.1.2.3"] {
            assert_eq!(a.check(at(yes)).await, Verdict::Admit(Grant::ALL), "{yes}");
        }
        for no in [
            "100.64.0.3",
            "::ffff:100.101.102.103",
            "fd7a:115c:a1e0:ab12::1",
            "192.168.1.9",
            "172.16.0.1",
            "fd00::1",
            "fe80::1",
            "8.8.8.8",
            "0.0.0.0",
            "::",
        ] {
            assert!(matches!(a.check(at(no)).await, Verdict::Refuse(_)), "{no}");
        }
    }

    /// A `LocalAPI` answering from a table: the node's own status, and a whois per caller.
    async fn daemon() -> LocalApi {
        const STATUS: &str = r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"mac",
            "DNSName":"mac.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.3"],"UserID":2}}"#;
        fn whois(user: i64, tags: &str, caps: &str) -> String {
            format!(
                r#"{{"Node":{{"Name":"n.ts.net.","User":{user},"Tags":{tags}}},
                "UserProfile":{{"ID":{user},"LoginName":"u{user}"}},"CapMap":{caps}}}"#
            )
        }
        let (api, _seen) = slopty_tailnet::fake::daemon(|q| {
            if q.ends_with("/status") {
                (200, STATUS.to_owned())
            } else if q.contains("100.64.0.4") {
                (200, whois(2, "null", "null"))
            } else if q.contains("100.64.0.5") {
                (200, whois(7, "null", "null"))
            } else if q.contains("100.64.0.6") {
                let caps = r#"{"github.com/aislopware/slopty":[{"roles":["agent"]}]}"#;
                (200, whois(9, r#"["tag:ci"]"#, caps))
            } else if q.contains("100.64.0.7") {
                (500, "stuck".to_owned())
            } else {
                (404, "no match".to_owned())
            }
        })
        .await
        .unwrap();
        api
    }

    /// Through the daemon: the owner's other machine gets every role, another user's machine
    /// none, a tagged node what its grant names, an address no node has nothing, and a daemon
    /// that fails refuses rather than lets in. Loopback never asks.
    #[tokio::test]
    async fn the_tailnet_says_who_is_calling_and_what_they_may_do() {
        let a = Admission::with_tailnet(Vec::new(), Some(daemon().await));
        assert_eq!(
            a.check(at("100.64.0.4")).await,
            Verdict::Admit(Grant::ALL),
            "the owner's laptop"
        );
        assert!(matches!(a.check(at("100.64.0.5")).await, Verdict::Refuse(_)), "another user");
        let Verdict::Admit(ci) = a.check(at("100.64.0.6")).await else {
            panic!("the granted node")
        };
        assert!(ci.allows(Role::Agent) && !ci.allows(Role::Client));
        assert!(matches!(a.check(at("100.64.9.9")).await, Verdict::Refuse(_)), "no such node");
        assert!(matches!(a.check(at("100.64.0.7")).await, Verdict::Refuse(_)), "daemon failed");
        assert_eq!(a.check(at("127.0.0.1")).await, Verdict::Admit(Grant::ALL));
    }

    /// This machine's Tailscale is looked up when a tailnet peer calls, not only at start:
    /// none found refuses, a daemon that comes up later is found on a later call, and one that
    /// stops answering (an App Store extension back on a new port and token) is looked up
    /// again within the same call.
    #[tokio::test]
    async fn tailscale_is_looked_up_again_when_missing_or_failing() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static STAGE: AtomicUsize = AtomicUsize::new(0);
        static FOUND: Mutex<Vec<LocalApi>> = Mutex::new(Vec::new());
        fn find() -> Option<LocalApi> {
            let stage = STAGE.load(Ordering::SeqCst);
            FOUND.lock().get(stage.checked_sub(1)?).cloned()
        }
        let (stale, _) =
            slopty_tailnet::fake::daemon(|_| (401, "bad token".to_owned())).await.unwrap();
        let fresh = daemon().await;
        FOUND.lock().extend([stale, fresh]);
        let a = Admission::finding(Vec::new(), find, Duration::ZERO);
        assert!(matches!(a.check(at("100.64.0.4")).await, Verdict::Refuse(_)), "none running");
        STAGE.store(1, Ordering::SeqCst);
        assert!(a.local_api().is_some(), "found once it runs");
        STAGE.store(2, Ordering::SeqCst);
        assert_eq!(
            a.check(at("100.64.0.4")).await,
            Verdict::Admit(Grant::ALL),
            "the stale daemon failed, and the fresh one answered in the same call"
        );
        assert_eq!(a.check(at("127.0.0.1")).await, Verdict::Admit(Grant::ALL));
        assert!(on_tailnet(ip("100.127.255.254")) && !on_tailnet(ip("100.128.0.0")));
        assert!(on_tailnet(ip("::ffff:100.64.0.1")) && !on_tailnet(ip("100.63.255.255")));
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
