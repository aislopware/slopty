//! `slopty server relay`: whether the server's machine serves as a Tailscale peer relay, and
//! how to make it one (`docs/decisions/transport.md`, "The server's machine as a peer relay").
//!
//! A link that cannot go direct falls back to Tailscale's DERP servers, a TCP detour of tens to
//! hundreds of milliseconds. Tailscale 1.86+ tries a peer relay in the tailnet first, and the
//! server's machine is the one node that is always on. Turning it on is the user's call and
//! takes both a Tailscale pref on that machine and a grant in the tailnet policy, which only a
//! tailnet admin can write, so this reads and explains; it changes nothing.

use std::net::IpAddr;

use slopty_tailnet::{LocalApi, LocalApiError};

/// The port suggested for the relay, as Tailscale's own documentation uses.
const SUGGESTED_PORT: u16 = 40_000;

/// What this machine's Tailscale says of its peer relay.
#[derive(Debug)]
pub enum Relay {
    /// No Tailscale this user can read runs here.
    NoTailscale,
    /// The daemon could not be read.
    Unread(LocalApiError),
    /// It serves as a relay on this UDP port (`0`: one the daemon picks), or does not.
    Read {
        /// The port.
        port: Option<u16>,
        /// This node's tailnet address, for the grant.
        me: Option<IpAddr>,
    },
}

/// Ask this machine's Tailscale.
pub async fn read() -> Relay {
    let Some(api) = LocalApi::find() else { return Relay::NoTailscale };
    match api.relay_server_port().await {
        Ok(port) => {
            let me = api.status().await.ok().and_then(|s| s.me.and_then(|me| me.ipv4()));
            Relay::Read { port, me }
        }
        Err(e) => Relay::Unread(e),
    }
}

impl Relay {
    /// One line for `slopty server status` and `install`.
    pub fn line(&self) -> String {
        match self {
            Self::NoTailscale => "peer relay: no Tailscale this user can read runs here".to_owned(),
            Self::Unread(e) => format!("peer relay: unknown ({e})"),
            Self::Read { port: Some(port), .. } => format!("peer relay: on, {}", udp(*port)),
            Self::Read { port: None, .. } => {
                "peer relay: off; `slopty server relay` says why it helps and how".to_owned()
            }
        }
    }

    /// The whole story, for `slopty server relay`.
    pub fn report(&self) -> String {
        match self {
            Self::NoTailscale | Self::Unread(_) => self.line(),
            Self::Read { port: Some(port), me } => format!(
                "This machine is a Tailscale peer relay on {}. Links that cannot go direct try it \
                 before Tailscale's DERP servers, once the tailnet policy grants it:\n\n{}\n\n\
                 Its UDP port must be open to the rest of the tailnet.\n",
                udp(*port),
                grant(*me)
            ),
            Self::Read { port: None, me } => format!(
                "This machine is no Tailscale peer relay. A worker link that cannot go direct \
                 falls back to Tailscale's DERP servers, a detour that adds tens to hundreds of \
                 milliseconds to every keystroke and frame. The server's machine is always on, \
                 which makes it a good relay (Tailscale 1.86+, Headscale 0.29+):\n\n  \
                 tailscale set --relay-server-port={SUGGESTED_PORT}\n\nthen let UDP \
                 {SUGGESTED_PORT} in through any firewall in front of it, and add this grant to \
                 the tailnet policy (a tailnet admin's job):\n\n{}\n\nTagged workers need their \
                 tag in \"src\" too.\n",
                grant(*me)
            ),
        }
    }
}

fn udp(port: u16) -> String {
    if port == 0 { "a UDP port Tailscale picks".to_owned() } else { format!("UDP {port}") }
}

/// The grant that lets the tailnet's members relay through `me`.
fn grant(me: Option<IpAddr>) -> String {
    let dst = me.map_or_else(|| "<this machine's tailnet IP>".to_owned(), |ip| ip.to_string());
    format!(
        "  {{\n    \"src\": [\"autogroup:member\"],\n    \"dst\": [\"{dst}\"],\n    \"app\": \
         {{ \"tailscale.com/cap/relay\": [] }}\n  }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Off, the report gives the command, the port to open and the grant naming this node; on,
    /// the port it serves and the grant; the one line says which.
    #[test]
    fn the_report_says_how_to_turn_the_relay_on() {
        let me = Some(IpAddr::from([100, 64, 0, 3]));
        let off = Relay::Read { port: None, me };
        let report = off.report();
        assert!(report.contains("tailscale set --relay-server-port=40000"), "{report}");
        assert!(report.contains("\"dst\": [\"100.64.0.3\"]"), "{report}");
        assert!(report.contains("\"tailscale.com/cap/relay\": []"), "{report}");
        assert!(off.line().starts_with("peer relay: off"));

        let on = Relay::Read { port: Some(40_000), me };
        assert_eq!(on.line(), "peer relay: on, UDP 40000");
        assert!(on.report().contains("\"dst\": [\"100.64.0.3\"]"));
        let picked = Relay::Read { port: Some(0), me: None };
        assert_eq!(picked.line(), "peer relay: on, a UDP port Tailscale picks");
        assert!(picked.report().contains("<this machine's tailnet IP>"));
    }

    /// The daemon's prefs are what is read, through the fake `LocalAPI`.
    #[tokio::test]
    async fn the_relay_port_comes_from_the_daemons_prefs() {
        let (api, _seen) = slopty_tailnet::fake::daemon(|path| match path {
            "/localapi/v0/prefs" => (200, r#"{"RelayServerPort":40000}"#.to_owned()),
            _ => (404, String::new()),
        })
        .await
        .unwrap();
        assert_eq!(api.relay_server_port().await.unwrap(), Some(40_000));
    }
}
