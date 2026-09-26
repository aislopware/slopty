//! Which callers Slopty lets in, and in what role, from what the tailnet says of them.
//!
//! * A node of the same user as this one may do anything: it is the user's own machine.
//! * Any other node, a tagged one included, needs a grant carrying the capability [`CAP`] with the
//!   roles it may take, e.g. in the tailnet policy:
//!
//!   ```jsonc
//!   "grants": [{ "src": ["tag:ci"], "dst": ["tag:slopty-worker"], "ip": ["*"],
//!                "app": { "github.com/aislopware/slopty": [{ "roles": ["agent"] }] } }]
//!   ```
//!
//!   Tailscale's control plane, Headscale from 0.29 and slopscale hand such grants to the
//!   destination verbatim. The name is a domain Slopty controls, since `tailscale.com/…` is
//!   reserved.
//! * Everything else is refused.

use serde::Deserialize;

use crate::WhoIs;

/// The application capability a tailnet grant gives Slopty's roles under.
pub const CAP: &str = "github.com/aislopware/slopty";

/// What a peer may be to a Slopty listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// An app or CLI driving workers.
    Client,
    /// An AI agent driving workers through the server.
    Agent,
    /// A worker registering with the server.
    Worker,
}

/// A grant's value under [`CAP`]: `{ "roles": [...] }`.
#[derive(Deserialize)]
struct Value {
    #[serde(default)]
    roles: Vec<Role>,
}

/// The roles a caller may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Grant {
    client: bool,
    agent: bool,
    worker: bool,
}

impl Grant {
    /// Every role: the user's own machines.
    pub const ALL: Self = Self { client: true, agent: true, worker: true };

    /// Whether the caller may take `role`.
    #[must_use]
    pub const fn allows(self, role: Role) -> bool {
        match role {
            Role::Client => self.client,
            Role::Agent => self.agent,
            Role::Worker => self.worker,
        }
    }

    /// Whether the caller may take any role at all.
    #[must_use]
    pub const fn any(self) -> bool {
        self.client || self.agent || self.worker
    }

    const fn with(mut self, role: Role) -> Self {
        match role {
            Role::Client => self.client = true,
            Role::Agent => self.agent = true,
            Role::Worker => self.worker = true,
        }
        self
    }

    /// What the tailnet lets `caller` do on a node owned by `owner` (`None` when this node is
    /// tagged and so owned by nobody).
    #[must_use]
    pub fn of(caller: &WhoIs, owner: Option<i64>) -> Self {
        if !caller.tagged() && owner == Some(caller.user_profile.id) {
            return Self::ALL;
        }
        caller
            .cap_map
            .get(CAP)
            .into_iter()
            .flatten()
            .filter_map(|v| Value::deserialize(v).ok())
            .flat_map(|v| v.roles)
            .fold(Self::default(), Self::with)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whois::tests::{GRANTED, MINE};

    fn whois(json: &str) -> WhoIs {
        serde_json::from_str(json).unwrap()
    }

    /// The user's own machine may do anything; the same machine on someone else's node, or
    /// on a tagged node, may do nothing without a grant.
    #[test]
    fn the_users_own_machines_are_let_in_and_nobody_else_is() {
        assert_eq!(Grant::of(&whois(MINE), Some(2)), Grant::ALL);
        assert!(!Grant::of(&whois(MINE), Some(3)).any(), "another user's node");
        assert!(!Grant::of(&whois(MINE), None).any(), "this node is tagged: no owner to match");
    }

    /// A grant gives the roles it names and no others, whoever owns this node; a value that
    /// is not Slopty's shape is ignored rather than refusing the rest.
    #[test]
    fn a_grant_gives_the_roles_it_names() {
        let ci = Grant::of(&whois(GRANTED), None);
        assert!(ci.allows(Role::Agent));
        assert!(!ci.allows(Role::Client) && !ci.allows(Role::Worker));
        let odd = GRANTED
            .replace(r#"[ { "roles": ["agent"] } ]"#, r#"[ "junk", { "roles": ["worker"] } ]"#);
        let odd = Grant::of(&whois(&odd), Some(2));
        assert!(odd.allows(Role::Worker) && !odd.allows(Role::Agent));
    }
}
