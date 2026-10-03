//! What to tell a person about a worker or server that runs a different build, and what
//! updates it (`docs/decisions/transport.md`, "Each end says its wire first").
//!
//! The link found it in the wire prefix ([`WrongBuild`]) before any message. The client
//! stops its fast redials to that host and says this instead. The app shows it on the worker,
//! and the CLI prints it ([`std::fmt::Display`]).

use slopty_net::{NetError, WrongBuild};
use slopty_proto::wire::BUILD;

/// Which end runs the other build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Of {
    /// A worker, which a deploy from this machine replaces.
    Worker,
    /// The server, which is installed on its own machine.
    Server,
}

/// A worker or server that runs a different build.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UpdateNotice {
    /// Which end it is.
    pub of: Of,
    /// The host it is about, as it was dialled.
    pub host: String,
    /// Its build as it said it; empty for a build older than the wire prefix.
    pub peer: String,
}

impl UpdateNotice {
    /// For the worker at `host` (the address it was dialled at, which `ssh` usually takes
    /// too).
    #[must_use]
    pub fn worker(host: &str, wrong: &WrongBuild) -> Self {
        Self { of: Of::Worker, host: host.to_owned(), peer: wrong.peer.clone() }
    }

    /// For the server at `host`.
    #[must_use]
    pub fn server(host: &str, wrong: &WrongBuild) -> Self {
        Self { of: Of::Server, host: host.to_owned(), peer: wrong.peer.clone() }
    }

    /// The worker notice for a dial to `host` that failed with `e`, when that is why.
    #[must_use]
    pub fn for_worker_dial(host: &str, e: &NetError) -> Option<Self> {
        match e {
            NetError::WrongBuild(wrong) => Some(Self::worker(host, wrong)),
            _ => None,
        }
    }

    /// The line: "This machine runs a different build".
    #[must_use]
    pub const fn title(&self) -> &'static str {
        match self.of {
            Of::Worker => "This machine runs a different build",
            Of::Server => "The server runs a different build",
        }
    }

    /// What each side runs.
    #[must_use]
    pub fn detail(&self) -> String {
        let peer = WrongBuild { peer: self.peer.clone() };
        format!("It runs {}; this build is {BUILD}.", peer.peer_build())
    }

    /// What updates it, to copy and run on this machine, which has this build: a deploy over
    /// `ssh` for a worker or a server elsewhere, and an install in place for a server on this
    /// machine. An install typed on the server's own machine would put back the build already
    /// there, beside its own CLI.
    #[must_use]
    pub fn command(&self) -> String {
        match self.of {
            Of::Worker => format!("slopty worker deploy {} --update", self.host),
            Of::Server if self.here() => "slopty server install".to_owned(),
            Of::Server => format!("slopty server deploy {}", self.host),
        }
    }

    /// Whether the host is this machine, dialled on loopback.
    #[must_use]
    pub fn here(&self) -> bool {
        let host = self.host.trim_start_matches('[').trim_end_matches(']');
        host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }
}

/// The CLI's lines: the host and both builds, then the command.
impl std::fmt::Display for UpdateNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{} runs a different build. {}", self.host, self.detail())?;
        write!(f, "Update it: {}", self.command())
    }
}

/// An error in its own right, so a dial that fails for it can carry it.
impl std::error::Error for UpdateNotice {}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrong(peer: &str) -> WrongBuild {
        WrongBuild { peer: peer.to_owned() }
    }

    #[test]
    fn a_worker_notice_names_both_builds_and_the_deploy() {
        let notice = UpdateNotice::worker("mini", &wrong("0.0.9+wire.0badf00d"));
        assert_eq!(notice.title(), "This machine runs a different build");
        assert_eq!(notice.detail(), format!("It runs 0.0.9+wire.0badf00d; this build is {BUILD}."));
        assert_eq!(notice.command(), "slopty worker deploy mini --update");
        assert_eq!(
            notice.to_string(),
            format!(
                "mini runs a different build. It runs 0.0.9+wire.0badf00d; this build is \
                 {BUILD}.\nUpdate it: slopty worker deploy mini --update"
            )
        );
    }

    /// A server elsewhere is brought to this build from here, over `ssh`, never by an install
    /// on its own machine, which would put its own build back; one on this machine is installed
    /// in place. An older build is said so.
    #[test]
    fn a_server_notice_deploys_this_build_and_an_older_build_is_said_so() {
        let notice = UpdateNotice::server("hub", &wrong(""));
        assert_eq!(notice.title(), "The server runs a different build");
        assert_eq!(notice.detail(), format!("It runs an older one; this build is {BUILD}."));
        assert_eq!(notice.command(), "slopty server deploy hub");
        assert!(notice.to_string().ends_with("Update it: slopty server deploy hub"));
        for here in ["127.0.0.1", "::1", "[::1]", "localhost"] {
            let notice = UpdateNotice::server(here, &wrong("0.0.9+wire.0badf00d"));
            assert!(notice.here(), "{here}");
            assert_eq!(notice.command(), "slopty server install", "{here}");
        }
        assert!(!notice.here(), "hub is another machine");
    }

    #[test]
    fn only_a_wrong_build_is_a_notice() {
        let wrong = NetError::WrongBuild(wrong("x"));
        assert!(UpdateNotice::for_worker_dial("mini", &wrong).is_some());
        assert_eq!(UpdateNotice::for_worker_dial("mini", &NetError::NotGranted), None);
    }
}
