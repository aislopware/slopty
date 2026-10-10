//! What to tell a person about a worker or server that runs a different build, and what
//! updates it (`docs/decisions/transport.md`, "Each end says its wire first").
//!
//! The link found it in the wire prefix ([`WrongBuild`]) before any message. The client
//! stops its fast redials to that host and says this instead. The app shows it on the worker,
//! and the CLI prints it ([`std::fmt::Display`]).
//!
//! The app's own updates are read from its repository's latest release on GitHub
//! ([`latest_release_feed`], [`newer_release`]): a release is published, never pushed, so the
//! app asks and says what it found, and the person downloads it.

pub use slopty_net::Newer;
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

    /// Which build is the newer: by version, else by when each one's wire last changed (the
    /// stamp [`BUILD`] ends with). `None` when the two cannot be told apart that way: a build
    /// without a stamp, or two on the same version from the same minute.
    #[must_use]
    pub fn newer(&self) -> Option<Newer> {
        WrongBuild { peer: self.peer.clone() }.newer()
    }

    /// Whether this device runs the older build, so the other end is not the one to update.
    #[must_use]
    pub fn this_is_older(&self) -> bool {
        self.newer() == Some(Newer::There)
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

/// The CLI's lines: the host and both builds, then what to update: the command for the other
/// end, or this machine when it is the older.
impl std::fmt::Display for UpdateNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.this_is_older() {
            writeln!(f, "{} runs a newer build. {}", self.host, self.detail())?;
            return write!(f, "Update Slopty on this machine to match it.");
        }
        writeln!(f, "{} runs a different build. {}", self.host, self.detail())?;
        write!(f, "Update it: {}", self.command())
    }
}

/// An error in its own right, so a dial that fails for it can carry it.
impl std::error::Error for UpdateNotice {}

/// A Slopty release newer than this build.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Release {
    /// Its version, without the tag's `v`: `0.2.0`.
    pub version: String,
    /// Its page, which has the notes and the download.
    pub page: String,
}

/// The latest-release feed of `repository`, the `https://github.com/<owner>/<repo>` Cargo knows
/// the package by; `None` for a repository elsewhere, which has no such feed.
#[must_use]
pub fn latest_release_feed(repository: &str) -> Option<String> {
    let path = repository.trim_end_matches('/').strip_prefix("https://github.com/")?;
    let (owner, repo) = path.trim_end_matches(".git").split_once('/')?;
    let plain = |part: &str| {
        !part.is_empty()
            && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (plain(owner) && plain(repo))
        .then(|| format!("https://api.github.com/repos/{owner}/{repo}/releases/latest"))
}

/// The release in `body`, GitHub's latest-release answer, when it is final and newer.
///
/// That is a published release, not a draft or a pre-release, newer than `current`. Anything
/// else is `None`: no release yet (GitHub says "Not Found"), a draft or a pre-release, an
/// answer that does not parse, a page that is not https, or a version that is not newer.
#[must_use]
pub fn newer_release(body: &[u8], current: &str) -> Option<Release> {
    #[derive(serde::Deserialize)]
    struct Latest {
        tag_name: String,
        html_url: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }
    let latest: Latest = serde_json::from_slice(body).ok()?;
    if latest.draft || latest.prerelease || !latest.html_url.starts_with("https://") {
        return None;
    }
    let version = latest.tag_name.strip_prefix('v').unwrap_or(&latest.tag_name);
    (version_parts(version)? > version_parts(current)?)
        .then(|| Release { version: version.to_owned(), page: latest.html_url.clone() })
}

/// `major.minor.patch` as numbers, build metadata (`+…`) aside; `None` for anything else,
/// a pre-release suffix included.
fn version_parts(version: &str) -> Option<[u64; 3]> {
    let core = version.split_once('+').map_or(version, |(core, _build)| core);
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let numbers = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(numbers)
}

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

    /// The feed comes from the repository Cargo names, so a fork reads its own releases.
    #[test]
    fn the_feed_is_the_repository_s_latest_release() {
        assert_eq!(
            latest_release_feed("https://github.com/aislopware/slopty").as_deref(),
            Some("https://api.github.com/repos/aislopware/slopty/releases/latest")
        );
        assert_eq!(
            latest_release_feed("https://github.com/me/slopty.git/").as_deref(),
            Some("https://api.github.com/repos/me/slopty/releases/latest")
        );
        for elsewhere in
            ["https://gitlab.com/a/b", "https://github.com/a", "", "https://github.com/a/b?x"]
        {
            assert_eq!(latest_release_feed(elsewhere), None, "{elsewhere}");
        }
    }

    /// Only a published, final release newer than this build is news.
    #[test]
    fn only_a_newer_final_release_is_news() {
        let answer = |tag: &str, extra: &str| {
            format!(
                r#"{{"tag_name":"{tag}","html_url":"https://github.com/a/b/releases/tag/{tag}",
                "draft":false,"prerelease":false{extra},"assets":[]}}"#
            )
        };
        let newer = newer_release(answer("v0.2.0", "").as_bytes(), "0.1.9").unwrap();
        assert_eq!(newer.version, "0.2.0");
        assert_eq!(newer.page, "https://github.com/a/b/releases/tag/v0.2.0");
        assert!(newer_release(answer("v0.10.0", "").as_bytes(), "0.9.3").is_some(), "by number");
        assert!(newer_release(answer("v0.1.0", "").as_bytes(), "0.1.0").is_none(), "this one");
        assert!(newer_release(answer("v0.1.0", "").as_bytes(), "0.2.0").is_none(), "older");
        assert!(newer_release(answer("v0.3.0-rc.1", "").as_bytes(), "0.2.0").is_none());
        let draft = answer("v1.0.0", "").replace(r#""draft":false"#, r#""draft":true"#);
        assert!(newer_release(draft.as_bytes(), "0.2.0").is_none(), "a draft");
        let pre = answer("v1.0.0", "").replace(r#""prerelease":false"#, r#""prerelease":true"#);
        assert!(newer_release(pre.as_bytes(), "0.2.0").is_none(), "a pre-release");
        let none = br#"{"message":"Not Found","status":"404"}"#;
        assert!(newer_release(none, "0.1.0").is_none(), "no release yet");
        assert!(newer_release(b"<html>", "0.1.0").is_none(), "not an answer");
        let page = answer("v1.0.0", "").replace("https://github.com", "file:///etc");
        assert!(newer_release(page.as_bytes(), "0.1.0").is_none(), "only an https page opens");
        assert!(newer_release(answer("v1.0.0+abc", "").as_bytes(), "0.1.0+wire.1").is_some());
    }

    /// A notice from a newer peer tells this machine to update and offers no command that
    /// would put the older build there.
    #[test]
    fn a_newer_peer_says_to_update_this_machine() {
        let notice = UpdateNotice::worker("mini", &wrong("999.0.0+wire.feedface"));
        assert!(notice.this_is_older());
        let said = notice.to_string();
        assert!(said.starts_with("mini runs a newer build."), "{said}");
        assert!(said.ends_with("Update Slopty on this machine to match it."), "{said}");
        assert!(!said.contains("deploy"), "{said}");
        let older = UpdateNotice::worker("mini", &wrong(""));
        assert_eq!(older.newer(), Some(Newer::Here));
        assert!(older.to_string().ends_with("Update it: slopty worker deploy mini --update"));
    }

    #[test]
    fn only_a_wrong_build_is_a_notice() {
        let wrong = NetError::WrongBuild(wrong("x"));
        assert!(UpdateNotice::for_worker_dial("mini", &wrong).is_some());
        assert_eq!(UpdateNotice::for_worker_dial("mini", &NetError::NotGranted), None);
    }
}
