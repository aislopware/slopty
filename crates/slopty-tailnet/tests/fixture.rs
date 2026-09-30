//! Against a real tailnet on loopback (`cargo xtask tailnet up`): Headscale and two userspace
//! `tailscaled` nodes, `worker` tagged `tag:slopty-worker` and `ci` tagged `tag:ci`, where the
//! policy grants `ci` Slopty's capability on `worker`. The test reads the fixture the xtask writes,
//! named by `SLOPTY_TAILNET_FIXTURE`, and skips, saying so, when there is none.

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::path::PathBuf;

    use serde::Deserialize;
    use slopty_tailnet::policy::CAP;
    use slopty_tailnet::{Grant, LocalApi, Location, Role};

    /// The variable `cargo xtask tailnet status` prints.
    const FIXTURE: &str = "SLOPTY_TAILNET_FIXTURE";

    /// What `cargo xtask tailnet up` writes, the fields read here.
    #[derive(Deserialize)]
    struct Fixture {
        cap: String,
        nodes: Vec<Node>,
    }

    #[derive(Deserialize)]
    struct Node {
        name: String,
        socket: PathBuf,
        ips: Vec<IpAddr>,
        tags: Vec<String>,
    }

    impl Fixture {
        fn node(&self, name: &str) -> &Node {
            self.nodes.iter().find(|n| n.name == name).expect("the fixture has the node")
        }
    }

    impl Node {
        fn api(&self) -> LocalApi {
            LocalApi::at(Location::Unix(self.socket.clone()))
        }

        fn ipv4(&self) -> IpAddr {
            self.ips.iter().copied().find(IpAddr::is_ipv4).expect("every node has an IPv4")
        }
    }

    fn fixture() -> Option<Fixture> {
        let Some(path) = std::env::var_os(FIXTURE) else {
            slopty_testkit::live::skip(&format!("{FIXTURE} is unset: `cargo xtask tailnet up`"));
            return None;
        };
        let path = PathBuf::from(path);
        let Ok(text) = std::fs::read_to_string(&path) else {
            slopty_testkit::live::skip(&format!("no tailnet fixture at {}", path.display()));
            return None;
        };
        Some(serde_json::from_str(&text).expect("the fixture reads"))
    }

    /// The worker's daemon lists ci as an online peer with its tag, and whois from either of
    /// ci's addresses names ci with exactly the roles the policy grants it. Nothing is granted
    /// the other way.
    #[tokio::test]
    async fn a_granted_peer_is_seen_with_its_roles() {
        let Some(fixture) = fixture() else { return };
        assert_eq!(fixture.cap, CAP, "the fixture grants the capability Slopty reads");
        let (worker, ci) = (fixture.node("worker"), fixture.node("ci"));

        let api = worker.api();
        let status = api.status().await.expect("the worker's daemon answers");
        assert!(status.running(), "the worker is {}", status.backend_state);
        let me = status.me.as_ref().expect("a running node has itself");
        assert_eq!(me.ips, worker.ips);
        assert_eq!(me.tags, worker.tags);
        let peer = status.node_at(ci.ipv4()).expect("the worker sees ci");
        assert!(peer.online, "ci is online");
        assert_eq!(peer.tags, ci.tags);
        assert_eq!(peer.ips, ci.ips);

        // A tagged node belongs to nobody, so only a grant lets anyone in.
        let owner = me.tags.is_empty().then_some(me.user);
        assert_eq!(owner, None, "the worker is tagged");
        for &ip in &ci.ips {
            let who = api
                .whois((ip, 41641).into())
                .await
                .expect("the daemon answers")
                .expect("the daemon knows ci's address");
            assert_eq!(who.node.name, peer.dns_name);
            assert!(who.tagged(), "ci is tagged");
            let grant = Grant::of(&who, owner);
            assert!(grant.allows(Role::Agent), "{ip}: {grant:?} {:?}", who.cap_map);
            assert!(!grant.allows(Role::Client) && !grant.allows(Role::Worker), "{grant:?}");
        }

        let back = ci
            .api()
            .whois((worker.ipv4(), 1).into())
            .await
            .expect("ci's daemon answers")
            .expect("ci knows the worker's address");
        assert!(!Grant::of(&back, None).any(), "no grant runs from the worker to ci");
    }
}
