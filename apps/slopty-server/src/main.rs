//! `slopty-server` — the control plane daemon.
//!
//! Workers register over QUIC on `--port` and hold their lease there; clients, the CLI and
//! agents get the worker directory and send verbs on the same port, an agent's tools through
//! `slopty mcp` among them. It admits loopback, the tailnet and the `[network] allow` ranges of
//! `settings.toml` (a VPN Tailscale does not vouch for). The worker list survives restarts in
//! `workers.json` in the data directory. Notes reach a pocketed phone once `[server.push]` names an
//! APNs key.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_net::admission::{Admission, parse_allow};
use slopty_server::project::{Bounds, Policy, ProjectId};
use slopty_server::{Config, PushConfig, Server};

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-server", version, about)]
struct Args {
    /// UDP port for worker, client and agent links; 0 picks a free one. Also
    /// `SLOPTY_SERVER_PORT`.
    #[arg(long, env = "SLOPTY_SERVER_PORT", default_value_t = slopty_net::endpoint::SERVER_PORT)]
    port: u16,
    /// Where `workers.json` lives (default: `server` in `$SLOPTY_DATA_DIR`, else in
    /// `~/Library/Application Support/Slopty` on macOS and `$XDG_DATA_HOME/slopty` on Linux).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// The name clients show for this server (default: `$SLOPTY_SERVER_NAME`, else the
    /// machine's computer name).
    #[arg(long, env = "SLOPTY_SERVER_NAME")]
    name: Option<String>,
    /// Once the listener is bound, print where on stdout as one JSON line,
    /// `{"quic":"[::]:45560"}` (a harness reads the port `0` picked).
    #[arg(long)]
    print_addr: bool,
}

/// What the server reads of the `settings.toml` beside its data directory.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct Read {
    /// `[server]`.
    server: slopty_settings::ServerSettings,
    /// `[network] allow`: the ranges let in besides loopback and the tailnet, the worker's too.
    allow: Vec<String>,
    /// `[worker] keep_awake`: what keeps this machine awake, its worker and its server alike.
    keep_awake: slopty_settings::KeepAwake,
}

impl Read {
    fn of(settings: slopty_settings::Settings) -> Self {
        Self {
            server: settings.server,
            allow: settings.network.allow,
            keep_awake: settings.worker.keep_awake,
        }
    }
}

/// What the server reads of the `settings.toml` beside `data_dir` (the Slopty data directory
/// the server's own lives in, which the worker and the app read too); the defaults when it
/// does not read.
fn settings(data_dir: &std::path::Path) -> Read {
    let root = data_dir.parent().unwrap_or(data_dir);
    let loaded = slopty_settings::Settings::load(&slopty_settings::path_in(root));
    if let Some(e) = &loaded.error {
        tracing::warn!(error = %e, "settings.toml ignored; no extra ranges, default project bounds");
    }
    Read::of(loaded.settings)
}

/// The `settings.toml` beside `data_dir`, which [`settings`] reads.
fn settings_path(data_dir: &std::path::Path) -> PathBuf {
    slopty_settings::path_in(data_dir.parent().unwrap_or(data_dir))
}

/// Which parts of what the server reads a change of the file touched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Changed {
    allow: bool,
    projects: bool,
    push: bool,
    keep_awake: bool,
}

/// What changed from `before` to `now`, named field by field so a new key is decided here.
fn changed(before: &Read, now: &Read) -> Changed {
    let Read { server: slopty_settings::ServerSettings { projects, push }, allow, keep_awake } =
        now;
    Changed {
        allow: *allow != before.allow,
        projects: *projects != before.server.projects,
        push: *push != before.server.push,
        keep_awake: *keep_awake != before.keep_awake,
    }
}

/// How `[server.push]` has notes reach a phone: straight to APNs when it names a key, its ID
/// and the team's, else not at all. A key that does not read, or comes without its IDs, is
/// said and not used.
fn push(settings: &slopty_settings::PushSettings) -> PushConfig {
    let slopty_settings::PushSettings { apns_key, key_id, team_id } = settings;
    let (apns_key, key_id, team_id) = (apns_key.trim(), key_id.trim(), team_id.trim());
    if apns_key.is_empty() {
        return PushConfig::Off;
    }
    let direct = std::fs::read_to_string(apns_key)
        .map_err(|e| e.to_string())
        .and_then(|pem| PushConfig::direct(&pem, key_id, team_id).map_err(|e| e.to_string()));
    match direct {
        Ok(direct) if !key_id.is_empty() && !team_id.is_empty() => return direct,
        Ok(_) => tracing::warn!("[server.push] apns_key needs key_id and team_id; not used"),
        Err(e) => tracing::warn!(error = %e, "[server.push] apns_key does not read; not used"),
    }
    PushConfig::Off
}

/// What `[worker] keep_awake` lets keep the server's machine awake.
const fn keep_awake(keep: slopty_settings::KeepAwake) -> slopty_server::KeepAwake {
    match keep {
        slopty_settings::KeepAwake::Working => slopty_server::KeepAwake::Working,
        slopty_settings::KeepAwake::Attached => slopty_server::KeepAwake::Attached,
        slopty_settings::KeepAwake::Never => slopty_server::KeepAwake::Never,
    }
}

/// The machine's idle-sleep assertion, taken while the hub holds it.
#[derive(Debug, Default)]
struct Assertion(Option<slopty_platform::Activity>);

impl slopty_server::Hold for Assertion {
    fn system(&mut self, hold: bool) {
        self.0 = hold.then(|| slopty_platform::Activity::system_awake("Slopty fleet at work"));
        tracing::info!(hold, "system sleep hold");
    }
}

/// Follow the file at `path` for as long as the server runs, looking `every` so often, and
/// hand `apply` each [`Read`] that differs from the one applied before (`applied` at first)
/// with what in it changed: every key takes effect as the file changes. A file that does not
/// parse changes nothing.
async fn follow_settings(
    path: PathBuf,
    every: std::time::Duration,
    mut applied: Read,
    apply: impl Fn(&Read, Changed),
) -> ! {
    let mut seen = slopty_settings::follow::Seen::of(&path);
    loop {
        tokio::time::sleep(every).await;
        if !seen.changed(&path) {
            continue;
        }
        let loaded = slopty_settings::Settings::load(&path);
        if let Some(e) = &loaded.error {
            tracing::warn!(error = %e, "settings.toml ignored; what was applied stays");
            continue;
        }
        let now = Read::of(loaded.settings);
        let what = changed(&applied, &now);
        if what != Changed::default() {
            apply(&now, what);
            applied = now;
        }
    }
}

/// Who may connect: loopback, the tailnet as this machine's Tailscale vouches for it, and the
/// `[network] allow` ranges.
fn admission(allow: &[String]) -> Admission {
    Admission::new(parse_allow(allow, "[network]"))
}

/// The person's bounds on projects and the fleet (`[server.projects]`). A project name there
/// that is no name is skipped, saying so; bounds past their ceiling leave the defaults.
fn policy(settings: &slopty_settings::ProjectBounds) -> Policy {
    let bounds = Bounds { live_agents: settings.live_agents, permission_flags: false };
    let bounds = match bounds.check() {
        Ok(()) => bounds,
        Err(e) => {
            tracing::warn!(error = %e, "[server.projects] ignored; the default bounds hold");
            Bounds::default()
        }
    };
    let permission_flags = settings
        .permission_flags
        .iter()
        .filter_map(|name| match ProjectId::new(name) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(error = %e, "[server.projects] permission_flags: skipped");
                None
            }
        })
        .collect();
    Policy { bounds, permission_flags }
}

#[tokio::main]
async fn main() -> Result<()> {
    slopty_crash::install(slopty_crash::Process::Server, &slopty_platform::dirs::data_dir());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let data_dir = args.data_dir.unwrap_or_else(slopty_server::store::default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let settings = settings(&data_dir);
    let admission = admission(&settings.allow);
    let followed = admission.clone();
    let settings_path = settings_path(&data_dir);
    let config = Config {
        name: args.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            slopty_platform::computer_name().unwrap_or_else(|| "server".to_owned())
        }),
        quic: slopty_net::endpoint::any(args.port),
        data_dir: data_dir.clone(),
        admission,
        push: push(&settings.server.push),
    };
    let server = Server::start(config).await.context("start (is another server running?)")?;
    server.hub().set_policy(policy(&settings.server.projects));
    server.hub().set_settings_file(settings_path.clone());
    server.hub().keep_awake(Box::new(Assertion::default()), keep_awake(settings.keep_awake));
    let hub = server.hub().clone();
    let every = slopty_settings::follow::POLL;
    tokio::spawn(follow_settings(settings_path, every, settings, move |now, changed| {
        if changed.allow {
            tracing::info!(ranges = ?now.allow, "[network] allow changed: applied");
            followed.set_ranges(parse_allow(&now.allow, "[network]"));
        }
        if changed.projects {
            tracing::info!("[server.projects] changed: applied");
            hub.set_policy(policy(&now.server.projects));
        }
        if changed.push {
            tracing::info!("[server.push] changed: applied");
            if let Err(e) = slopty_server::push_as(&hub, push(&now.server.push)) {
                tracing::warn!(error = %e, "[server.push] not applied; what was applied stays");
            }
        }
        if changed.keep_awake {
            tracing::info!(keep_awake = ?now.keep_awake, "[worker] keep_awake changed: applied");
            hub.set_keep_awake(keep_awake(now.keep_awake));
        }
    }));
    if args.print_addr {
        let bound = serde_json::json!({
            "quic": server.quic_addr().to_string(),
        });
        #[expect(clippy::print_stdout, reason = "the addresses are what a harness waits for")]
        {
            println!("{bound}");
        }
    }

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listen for SIGTERM")?;
    tokio::select! {
        _signal = terminate.recv() => {}
        _signal = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
    server.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Changed, ProjectId, PushConfig, admission, follow_settings, policy, push, settings,
        settings_path,
    };

    /// An edit of the file reaches the running server within a poll: the ranges and the
    /// project bounds it changed, each said by name, and nothing for an edit elsewhere in the
    /// file or one that does not parse.
    #[tokio::test]
    async fn an_edit_of_the_file_is_applied_as_it_is_read() {
        let every = std::time::Duration::from_millis(20);
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("server");
        let path = settings_path(&data_dir);
        let (told, mut heard) = tokio::sync::mpsc::unbounded_channel();
        let applied = settings(&data_dir);
        let follow =
            tokio::spawn(follow_settings(path.clone(), every, applied, move |now, what| {
                let _heard = told.send((now.allow.clone(), what));
            }));
        let after_a_poll = || tokio::time::sleep(every * 10);
        std::fs::write(&path, "[font]\nmono_size = 15.0\n").unwrap();
        after_a_poll().await;
        assert!(heard.try_recv().is_err(), "nothing of the server's changed");
        std::fs::write(&path, "[network]\nallow = [\"10.8.0.0/24\"]\n").unwrap();
        after_a_poll().await;
        let allow = Changed { allow: true, ..Changed::default() };
        assert_eq!(heard.try_recv().ok(), Some((vec!["10.8.0.0/24".to_owned()], allow)));
        std::fs::write(&path, "[server\n").unwrap();
        after_a_poll().await;
        assert!(heard.try_recv().is_err(), "a file that does not parse changes nothing");
        let text = "[network]\nallow = [\"10.8.0.0/24\"]\n[server.projects]\nlive_agents = 3\n";
        std::fs::write(&path, text).unwrap();
        after_a_poll().await;
        let projects = Changed { projects: true, ..Changed::default() };
        assert_eq!(heard.try_recv().ok().map(|(_, what)| what), Some(projects));
        std::fs::write(&path, format!("{text}[worker]\nkeep_awake = \"never\"\n")).unwrap();
        after_a_poll().await;
        let keep = Changed { keep_awake: true, ..Changed::default() };
        assert_eq!(heard.try_recv().ok().map(|(_, what)| what), Some(keep), "the machine's own");
        let pushing = "[server.push]\nkey_id = \"ABC123DEFG\"\n";
        std::fs::write(&path, format!("{text}[worker]\nkeep_awake = \"never\"\n{pushing}"))
            .unwrap();
        after_a_poll().await;
        let pushed = Changed { push: true, ..Changed::default() };
        assert_eq!(heard.try_recv().ok().map(|(_, what)| what), Some(pushed));
        follow.abort();
    }

    /// Notes reach a phone only once `[server.push]` names an APNs key with its ID and the
    /// team's. A key that does not read, or comes without its IDs, leaves them off.
    #[test]
    fn notes_reach_a_phone_as_server_push_says() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("server");
        let read = |text: &str| {
            std::fs::write(root.path().join("settings.toml"), text).unwrap();
            push(&settings(&data_dir).server.push)
        };
        assert!(matches!(read(""), PushConfig::Off), "off until set up");
        let missing = root.path().join("AuthKey.p8");
        let ids = "key_id = \"ABC123DEFG\"\nteam_id = \"DEF456GHIJ\"\n";
        let named = format!("[server.push]\napns_key = {missing:?}\n{ids}");
        assert!(matches!(read(&named), PushConfig::Off), "a key that is not there");
        std::fs::write(&missing, "not a key").unwrap();
        assert!(matches!(read(&named), PushConfig::Off), "a key that is not one");
        let bare = format!("[server.push]\napns_key = {missing:?}\n");
        assert!(matches!(read(&bare), PushConfig::Off), "a key without its IDs");
    }

    /// The ranges come from the settings beside the server's own directory, and a range that
    /// does not parse is skipped; with none, only loopback and the tailnet get in.
    #[test]
    fn the_allow_list_comes_from_the_shared_settings() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("server");
        assert!(admission(&settings(&data_dir).allow).ranges().is_empty(), "no range by default");
        let text = "[network]\nallow = [\"10.8.0.0/24\", \"bogus\"]\n";
        std::fs::write(root.path().join("settings.toml"), text).unwrap();
        let ranges: Vec<String> = admission(&settings(&data_dir).allow)
            .ranges()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(ranges, ["10.8.0.0/24"]);
    }

    /// The bounds on projects and the fleet are the person's, from the same settings; a
    /// project allowed looser permissions by a name that is no name is skipped, and bounds past
    /// their ceiling leave the defaults.
    #[test]
    fn the_project_bounds_come_from_the_shared_settings() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("server");
        let default = policy(&settings(&data_dir).server.projects);
        assert_eq!(
            default.bounds,
            super::Bounds::default(),
            "the file's defaults are the server's"
        );
        assert!(default.permission_flags.is_empty(), "no project may loosen by default");
        let text = "[server.projects]\nlive_agents = 3\n\
                    permission_flags = [\"nightly\", \"Not A Name\"]\n";
        std::fs::write(root.path().join("settings.toml"), text).unwrap();
        let set = policy(&settings(&data_dir).server.projects);
        assert_eq!(set.bounds.live_agents, 3);
        assert_eq!(
            set.permission_flags.into_iter().collect::<Vec<_>>(),
            [ProjectId::new("nightly").unwrap()]
        );
        assert!(!set.bounds.permission_flags, "only per project, never fleet-wide");
        std::fs::write(
            root.path().join("settings.toml"),
            "[server.projects]\nlive_agents = 5000\n",
        )
        .unwrap();
        let over = policy(&settings(&data_dir).server.projects);
        assert_eq!(over.bounds, super::Bounds::default(), "past a ceiling, the defaults hold");
        std::fs::write(
            root.path().join("settings.toml"),
            "[server.projects]\ncomprehension_depth = 3\n",
        )
        .unwrap();
        let deep = policy(&settings(&data_dir).server.projects);
        assert_eq!(deep.bounds, super::Bounds::default(), "a rule's cost stays bounded");
    }
}
