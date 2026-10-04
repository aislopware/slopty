//! `slopty-server` — the control plane daemon.
//!
//! Workers register over QUIC on `--port` and hold their lease there; clients, the CLI and
//! agents get the worker directory and send verbs on the same port; AI agents also reach the
//! verbs over MCP (Streamable HTTP) on `--mcp-port`. Both listeners admit loopback, the tailnet
//! and the `[server] allow` ranges of `settings.toml` (a VPN Tailscale does not vouch for). The
//! worker list survives restarts in `workers.json` in the data directory.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_net::admission::{Admission, parse_allow};
use slopty_server::project::{Bounds, Policy, ProjectId};
use slopty_server::{Config, Server};

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-server", version, about)]
struct Args {
    /// UDP port for worker, client and agent links; 0 picks a free one. Also
    /// `SLOPTY_SERVER_PORT`.
    #[arg(long, env = "SLOPTY_SERVER_PORT", default_value_t = slopty_net::endpoint::SERVER_PORT)]
    port: u16,
    /// TCP port for the MCP endpoint (`http://<host>:<port>/mcp`); 0 picks a free one. Also
    /// `SLOPTY_MCP_PORT`.
    #[arg(long, env = "SLOPTY_MCP_PORT", default_value_t = slopty_net::endpoint::MCP_PORT)]
    mcp_port: u16,
    /// Where `workers.json` lives (default: `server` in `$SLOPTY_DATA_DIR`, else in
    /// `~/Library/Application Support/Slopty` on macOS and `$XDG_DATA_HOME/slopty` on Linux).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// The name clients show for this server (default: `$SLOPTY_SERVER_NAME`, else the
    /// machine's computer name).
    #[arg(long, env = "SLOPTY_SERVER_NAME")]
    name: Option<String>,
    /// Once both listeners are bound, print where on stdout as one JSON line,
    /// `{"quic":"[::]:45560","mcp":"[::]:45561"}` (a harness reads the ports `0` picked).
    #[arg(long)]
    print_addr: bool,
}

/// The `[server]` table of the `settings.toml` beside `data_dir` (the Slopty data directory
/// the server's own lives in, which the worker and the app read too); the defaults when it
/// does not read.
fn settings(data_dir: &std::path::Path) -> slopty_settings::ServerSettings {
    let root = data_dir.parent().unwrap_or(data_dir);
    let loaded = slopty_settings::Settings::load(&slopty_settings::path_in(root));
    if let Some(e) = &loaded.error {
        tracing::warn!(error = %e, "settings.toml ignored; no extra ranges, default project bounds");
    }
    loaded.settings.server
}

/// The `settings.toml` beside `data_dir`, which [`settings`] reads.
fn settings_path(data_dir: &std::path::Path) -> PathBuf {
    slopty_settings::path_in(data_dir.parent().unwrap_or(data_dir))
}

/// Which parts of `[server]` a change of the file touched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Changed {
    allow: bool,
    projects: bool,
}

/// What changed from `before` to `now`, named field by field so a new key is decided here.
fn changed(
    before: &slopty_settings::ServerSettings,
    now: &slopty_settings::ServerSettings,
) -> Changed {
    let slopty_settings::ServerSettings { allow, projects } = now;
    Changed { allow: *allow != before.allow, projects: *projects != before.projects }
}

/// Follow the file at `path` for as long as the server runs, looking `every` so often, and
/// hand `apply` each `[server]` that differs from the one applied before (`applied` at first)
/// with what in it changed: every key takes effect as the file changes. A file that does not
/// parse changes nothing.
async fn follow_settings(
    path: PathBuf,
    every: std::time::Duration,
    mut applied: slopty_settings::ServerSettings,
    apply: impl Fn(&slopty_settings::ServerSettings, Changed),
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
        let now = loaded.settings.server;
        let what = changed(&applied, &now);
        if what != Changed::default() {
            apply(&now, what);
            applied = now;
        }
    }
}

/// Who may connect: loopback, the tailnet as this machine's Tailscale vouches for it, and the
/// `[server] allow` ranges.
fn admission(settings: &slopty_settings::ServerSettings) -> Admission {
    Admission::new(parse_allow(&settings.allow, "[server]"))
}

/// The person's bounds on projects and the fleet (`[server.projects]`). A project name there
/// that is no name is skipped, saying so; bounds past their ceiling leave the defaults.
fn policy(settings: &slopty_settings::ProjectBounds) -> Policy {
    let bounds = Bounds {
        live_agents: settings.live_agents,
        live_per_worker: settings.live_per_worker,
        live_per_project: settings.live_per_project,
        timeline_kept: settings.timeline_kept,
        permission_flags: false,
        projects: settings.projects,
        tasks_per_project: settings.tasks_per_project,
        title_max: settings.title_max,
        brief_max: settings.brief_max,
        owns_max: settings.owns_max,
        comprehension_depth: settings.comprehension_depth,
    };
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
    let admission = admission(&settings);
    let followed = admission.clone();
    let settings_path = settings_path(&data_dir);
    let config = Config {
        name: args.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            slopty_platform::computer_name().unwrap_or_else(|| "server".to_owned())
        }),
        quic: slopty_net::endpoint::any(args.port),
        mcp: slopty_net::endpoint::any(args.mcp_port),
        data_dir,
        admission,
    };
    let server = Server::start(config).await.context("start (is another server running?)")?;
    server.hub().set_policy(policy(&settings.projects));
    let hub = server.hub().clone();
    let every = slopty_settings::follow::POLL;
    tokio::spawn(follow_settings(settings_path, every, settings, move |now, changed| {
        if changed.allow {
            tracing::info!(ranges = ?now.allow, "[server] allow changed: applied");
            followed.set_ranges(parse_allow(&now.allow, "[server]"));
        }
        if changed.projects {
            tracing::info!("[server.projects] changed: applied");
            hub.set_policy(policy(&now.projects));
        }
    }));
    if args.print_addr {
        let bound = serde_json::json!({
            "quic": server.quic_addr().to_string(),
            "mcp": server.mcp_addr().to_string(),
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
    use super::{Changed, ProjectId, admission, follow_settings, policy, settings, settings_path};

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
        std::fs::write(&path, "[server]\nallow = [\"10.8.0.0/24\"]\n").unwrap();
        after_a_poll().await;
        let allow = Changed { allow: true, projects: false };
        assert_eq!(heard.try_recv().ok(), Some((vec!["10.8.0.0/24".to_owned()], allow)));
        std::fs::write(&path, "[server\n").unwrap();
        after_a_poll().await;
        assert!(heard.try_recv().is_err(), "a file that does not parse changes nothing");
        let text = "[server]\nallow = [\"10.8.0.0/24\"]\n[server.projects]\nlive_agents = 3\n";
        std::fs::write(&path, text).unwrap();
        after_a_poll().await;
        let projects = Changed { allow: false, projects: true };
        assert_eq!(heard.try_recv().ok().map(|(_, what)| what), Some(projects));
        follow.abort();
    }

    /// The ranges come from the settings beside the server's own directory, and a range that
    /// does not parse is skipped; with none, only loopback and the tailnet get in.
    #[test]
    fn the_allow_list_comes_from_the_shared_settings() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("server");
        assert!(admission(&settings(&data_dir)).ranges().is_empty(), "no range by default");
        let text = "[server]\nallow = [\"10.8.0.0/24\", \"bogus\"]\n";
        std::fs::write(root.path().join("settings.toml"), text).unwrap();
        let ranges: Vec<String> =
            admission(&settings(&data_dir)).ranges().iter().map(ToString::to_string).collect();
        assert_eq!(ranges, ["10.8.0.0/24"]);
    }

    /// The bounds on projects and the fleet are the person's, from the same settings; a
    /// project allowed looser permissions by a name that is no name is skipped, and bounds past
    /// their ceiling leave the defaults.
    #[test]
    fn the_project_bounds_come_from_the_shared_settings() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("server");
        let default = policy(&settings(&data_dir).projects);
        assert_eq!(
            default.bounds,
            super::Bounds::default(),
            "the file's defaults are the server's"
        );
        assert!(default.permission_flags.is_empty(), "no project may loosen by default");
        let text = "[server.projects]\nlive_agents = 3\n\
                    permission_flags = [\"nightly\", \"Not A Name\"]\n\
                    tasks_per_project = 50\nowns_max = 4\ncomprehension_depth = 2\n";
        std::fs::write(root.path().join("settings.toml"), text).unwrap();
        let set = policy(&settings(&data_dir).projects);
        let b = set.bounds;
        assert_eq!((b.live_agents, b.tasks_per_project, b.owns_max), (3, 50, 4));
        assert_eq!(b.comprehension_depth, 2);
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
        let over = policy(&settings(&data_dir).projects);
        assert_eq!(over.bounds, super::Bounds::default(), "past a ceiling, the defaults hold");
        std::fs::write(
            root.path().join("settings.toml"),
            "[server.projects]\ncomprehension_depth = 3\n",
        )
        .unwrap();
        let deep = policy(&settings(&data_dir).projects);
        assert_eq!(deep.bounds, super::Bounds::default(), "a rule's cost stays bounded");
    }
}
