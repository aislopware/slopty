//! `slopty worker …`: the daemon's local control socket.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use slopty_proto::ctl::{CtlReply, CtlRequest, LinkState, PasteboardAccess, Tailscale};

use crate::{deploy, service};

#[derive(Subcommand, Debug)]
pub enum WorkerCmd {
    /// Identity and sessions.
    Status,
    /// Check the daemon's permissions (Screen Recording, Accessibility), listen address,
    /// admitted ranges and links.
    Doctor,
    /// Screen streams open right now (and the last few closed) with the worker-side counters:
    /// capture and encode latency, capture path, drops.
    Screens,
    /// What keeps this machine awake: connected clients, working agents, live streams.
    Wake,
    /// Run `slopty-ptyd` and `slopty-worker` as services of this session (`LaunchAgents` on macOS,
    /// systemd user units on Linux): they start now and at every login.
    Install(service::InstallOpts),
    /// Stop the services and remove them (open sessions die with ptyd); with `--purge`,
    /// everything else of the worker's on this machine too.
    Uninstall(service::UninstallOpts),
    /// Whether the services are installed and running.
    Service,
    /// Put a worker on another machine over `ssh`, or with `--update` replace the one there:
    /// upload the binaries built for it, install its services and check it answers.
    Deploy(deploy::DeployOpts),
}

/// The worker's control socket, for every verb and relay that talks to it:
/// `$SLOPTY_WORKER_SOCKET`, else the installed agents' socket under `data_dir` when it
/// exists, else the dev default in the platform's socket directory
/// (`slopty_platform::dirs::runtime_dir`: `$TMPDIR/slopty` on macOS).
pub fn socket(data_dir: &Path) -> PathBuf {
    socket_in(std::env::var_os("SLOPTY_WORKER_SOCKET"), data_dir)
}

/// [`socket`] with the environment's say given.
fn socket_in(env: Option<std::ffi::OsString>, data_dir: &Path) -> PathBuf {
    if let Some(p) = env {
        return PathBuf::from(p);
    }
    let installed = data_dir.join("run").join("worker.sock");
    if installed.exists() {
        return installed;
    }
    slopty_platform::dirs::runtime_dir().join("worker.sock")
}

pub async fn run(cmd: WorkerCmd, server: Option<&str>, data_dir: &Path, json: bool) -> Result<()> {
    let req = match cmd {
        WorkerCmd::Status => CtlRequest::Status,
        WorkerCmd::Doctor => CtlRequest::Doctor,
        WorkerCmd::Screens => CtlRequest::Screens,
        WorkerCmd::Wake => CtlRequest::Wake,
        WorkerCmd::Install(opts) => return service::install(&opts, server, data_dir, json).await,
        WorkerCmd::Uninstall(opts) => return service::uninstall(opts, data_dir, json).await,
        WorkerCmd::Service => return service::status(json),
        WorkerCmd::Deploy(opts) => {
            let source = service::binaries_source(opts.bin_dir())?;
            let deployed = deploy::deploy(&opts, server, data_dir, &source).await?;
            print!("{}", deploy::report(opts.target(), &deployed));
            return Ok(());
        }
    };
    match call(data_dir, req).await? {
        CtlReply::Status { id, name, sessions } => {
            println!("{name}  {id}");
            for s in sessions {
                println!(
                    "  {}  {}x{}  {:?}  {} viewer(s)  {}",
                    s.id, s.cols, s.rows, s.state, s.viewers, s.title
                );
            }
        }
        CtlReply::Doctor(health) if json => println!("{}", serde_json::to_string(&health)?),
        CtlReply::Doctor(health) => {
            print!("{}", doctor_report(&health, DESKTOP));
            let quiet = std::env::var(slopty_agent::claude_mod::NONESSENTIAL_TRAFFIC_ENV).ok();
            let managed = slopty_agent::managed::ManagedSettings::current();
            println!("{}", mod_line(managed.mod_off(quiet.as_deref())));
            if DESKTOP && !(health.caps.can_capture && health.caps.can_inject) {
                bail!("permissions missing; see above");
            }
        }
        CtlReply::Screens { live, closed } => {
            for (state, list) in [("live", live), ("closed", closed)] {
                for s in list {
                    println!(
                        "{state}  client {}  stream {}  {:?}\n  capture {}  encode {}\n  captured {} (crop path {})  dropped {}  encoded {}  refused {}  bitrate {} bps",
                        s.client,
                        s.stream,
                        s.target,
                        s.stats.capture.describe(),
                        s.stats.encode.describe(),
                        s.stats.captured,
                        s.stats.cropped,
                        s.stats.dropped,
                        s.stats.encoded,
                        s.stats.queue_full,
                        s.stats.bitrate_bps
                    );
                }
            }
        }
        CtlReply::Wake(awake) if json => println!("{}", serde_json::to_string(&awake)?),
        CtlReply::Wake(awake) => {
            let held = |on: bool| if on { "held awake" } else { "free to sleep" };
            println!(
                "machine {}: {} client(s), {} working agent(s)\ndisplay {}: {} stream(s)",
                held(awake.system),
                awake.clients,
                awake.agents,
                held(awake.display),
                awake.streams
            );
        }
        CtlReply::Ok { changed } => println!("{}", if changed { "done" } else { "no change" }),
        CtlReply::Permission(answer) => bail!("a permission decision nobody asked for: {answer:?}"),
        CtlReply::Handoff(handed) => bail!("a handoff nobody asked for: {handed:?}"),
        CtlReply::Reports { batch, .. } => bail!("reports nobody asked for: {batch:?}"),
        reply @ (CtlReply::ClipTypes { .. } | CtlReply::ClipData { .. }) => {
            bail!("a clipboard answer nobody asked for: {reply:?}")
        }
        CtlReply::Error { message } => bail!("{message}"),
    }
    Ok(())
}

/// One request to the local daemon, found as [`socket`] says.
pub async fn call(data_dir: &Path, req: CtlRequest) -> Result<CtlReply> {
    call_at(&socket(data_dir), req).await
}

/// One request over the daemon's control socket at `path`.
pub async fn call_at(path: &Path, req: CtlRequest) -> Result<CtlReply> {
    let mut line = serde_json::to_vec(&req)?;
    line.push(b'\n');
    let reply = slopty_platform::service::ask(path, &line)
        .await
        .with_context(|| format!("is slopty-worker running? ({})", path.display()))?;
    Ok(serde_json::from_str(&reply)?)
}

/// Whether this machine's worker streams its desktop, so the doctor checks the grants that
/// takes. A Mac's does; a Linux worker runs terminals, files and agents only
/// (`docs/decisions/platform.md`, "Linux seams"). The daemon is on the machine the CLI runs on.
const DESKTOP: bool = cfg!(target_os = "macos");

/// The doctor's checklist. Permissions are granted to the daemon *binary*, so the report
/// names the path to add in System Settings; a worker with no `desktop` to stream needs none.
fn doctor_report(h: &slopty_proto::ctl::Health, desktop: bool) -> String {
    let mark = |ok: bool| if ok { "✔" } else { "✘" };
    let screen = if h.caps.can_capture {
        String::new()
    } else {
        "  → System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording: add the \
         daemon binary above, then `slopty worker install` (or restart the daemon)"
            .to_owned()
    };
    let post = if h.caps.can_inject {
        String::new()
    } else {
        "  → System Settings ▸ Privacy & Security ▸ Accessibility: add the daemon binary above"
            .to_owned()
    };
    let tailscale = match &h.tailscale {
        Tailscale::Up { node, ip } => {
            let ip = ip.map_or_else(|| "no IPv4".to_owned(), |ip| ip.to_string());
            format!(
                "{} Tailscale: {node} ({ip}); its nodes are let in as its grants say",
                mark(true)
            )
        }
        Tailscale::Down { backend } => {
            format!("{} Tailscale is {backend}: no tailnet peer reaches this worker", mark(false))
        }
        Tailscale::Unreachable { error } => {
            format!("{} Tailscale did not answer ({error}): no tailnet peer gets in", mark(false))
        }
        Tailscale::Absent => {
            format!("{} no Tailscale this daemon can read: no tailnet peer gets in", mark(false))
        }
    };
    let server = match &h.server {
        None => format!("{} no server set: this worker runs on its own", mark(false)),
        Some(s) => match &s.link {
            LinkState::Linked => {
                format!("{} registered with the server at {}", mark(true), s.address)
            }
            LinkState::Dialling => format!("… dialling the server at {}", s.address),
            LinkState::Redialling { why } => {
                format!(
                    "{} the server at {} is not linked ({why}); dialling again",
                    mark(false),
                    s.address
                )
            }
            LinkState::Refused { why } => {
                format!(
                    "{} the server at {} does not take this worker: {why}",
                    mark(false),
                    s.address
                )
            }
        },
    };
    let ranges = if h.allow.is_empty() {
        "admits loopback and the tailnet".to_owned()
    } else {
        format!("admits loopback, the tailnet, and by address {}", h.allow.join(", "))
    };
    let clipboard = match pasteboard_problem(h.pasteboard) {
        None => format!("{} Clipboard reads", mark(true)),
        Some(problem) => format!("{} {}{}", mark(false), sentence(problem), CLIPBOARD_WAITS),
    };
    let grants = if desktop {
        vec![
            format!("{} Screen Recording{screen}", mark(h.caps.can_capture)),
            format!("{} Accessibility (remote-window input){post}", mark(h.caps.can_inject)),
            clipboard,
        ]
    } else {
        vec!["no desktop to stream here: terminals, files and agents only".to_owned()]
    };
    let lines = [
        vec![
            format!("slopty-worker {}  ({})", h.caps.build, h.exe),
            format!("worker {}", h.worker),
            server,
            format!("up {} s · listening on {}", h.uptime_secs, h.listen),
            ranges,
            tailscale,
        ],
        grants,
        wake_line(h.caps.wake_on_lan).into_iter().collect(),
        h.caps.writes_failing.iter().map(|why| format!("✘ {why}")).collect(),
        h.caps
            .stops_at_logout
            .iter()
            .map(|how| format!("✘ the worker's services: {how}"))
            .collect(),
        vec![format!("{} clients connected, {} sessions", h.clients, h.sessions)],
    ]
    .concat();
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Whether Claude Code here can run Slopty's mod, whose live stream (text as it streams, the
/// command list, the model aliases) the threads add to the hooks and the transcript: under this
/// machine's managed settings, and for a `claude` typed in this shell, its environment.
fn mod_line(off: Option<slopty_agent::managed::ModOff>) -> String {
    match off {
        None => "✔ Claude Code's live stream (Slopty's mod) can be heard".to_owned(),
        Some(why) => format!(
            "✘ Claude Code's live stream is off: {why}\n  → its threads follow the hooks and the \
             transcript"
        ),
    }
}

/// What follows a clipboard read that is not free: what the worker does meanwhile.
const CLIPBOARD_WAITS: &str =
    "\n  → until then the worker's copies stay on it; pastes from clients still land";

/// Why reading the worker's pasteboard is not free, naming where to change it; `None` when it is.
const fn pasteboard_problem(access: PasteboardAccess) -> Option<&'static str> {
    use slopty_platform::pasteboard_access::Access;
    let access = match access {
        PasteboardAccess::Allowed => Access::Allowed,
        PasteboardAccess::NotAskedYet => Access::NotAskedYet,
        PasteboardAccess::Asks => Access::Asks,
        PasteboardAccess::Denied => Access::Denied,
    };
    access.problem()
}

/// `text` with its first letter capitalised, for a line of its own.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Whether the machine wakes for a magic packet, when it could tell.
fn wake_line(wake_on_lan: Option<bool>) -> Option<String> {
    let line = if wake_on_lan? {
        "✔ Wake for network access: a client can wake this machine from sleep"
    } else {
        "✘ Wake for network access is off: a client cannot wake this machine from sleep\n  \
         → turn on Wake for network access in System Settings ▸ Energy (Battery ▸ Options on a \
         laptop), or `sudo pmset -a womp 1`"
    };
    Some(line.to_owned())
}

#[cfg(test)]
mod tests {
    use slopty_proto::server::{Os, WorkerCaps};
    use slopty_proto::tailnet::BackendState;

    use super::*;

    /// `--data-dir` names the installed daemon's socket, unless the environment names one.
    #[test]
    fn the_socket_is_the_data_dirs_when_its_daemon_is_installed() {
        let data = tempfile::tempdir().unwrap();
        let dev = slopty_platform::dirs::runtime_dir().join("worker.sock");
        assert_eq!(socket_in(None, data.path()), dev, "nothing installed there");
        let installed = data.path().join("run").join("worker.sock");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"").unwrap();
        assert_eq!(socket_in(None, data.path()), installed);
        assert_eq!(socket_in(Some("/x.sock".into()), data.path()), PathBuf::from("/x.sock"));
    }

    /// The doctor says whether Claude Code's live stream can be heard, and if not, why.
    #[test]
    fn the_doctor_says_why_the_mod_is_off() {
        use slopty_agent::managed::ModOff;
        assert!(mod_line(None).starts_with('✔'));
        let quiet = mod_line(Some(ModOff::QuietByEnvironment));
        assert!(quiet.starts_with('✘') && quiet.contains("NONESSENTIAL_TRAFFIC"), "{quiet}");
        assert!(mod_line(Some(ModOff::SideloadingOff)).contains("disableSideloadFlags"));
    }

    #[test]
    fn doctor_report_names_the_binary_and_flags_missing_permissions() {
        let h = slopty_proto::ctl::Health {
            worker: slopty_core::WorkerId::nil(),
            server: Some(slopty_proto::ctl::ServerHealth {
                address: "studio:45560".to_owned(),
                link: LinkState::Linked,
            }),
            exe: "/opt/slopty/bin/slopty-worker".to_owned(),
            caps: WorkerCaps {
                build: "0.3.0".to_owned(),
                can_capture: true,
                can_inject: false,
                ..WorkerCaps::bare(Os::MacOs)
            },
            listen: "[::]:45550".to_owned(),
            allow: vec!["10.0.0.0/8".to_owned()],
            tailscale: Tailscale::Up {
                node: "studio.tail1234.ts.net".to_owned(),
                ip: Some([100, 64, 0, 3].into()),
            },
            pasteboard: PasteboardAccess::Allowed,
            clients: 2,
            sessions: 3,
            uptime_secs: 61,
        };
        let report = doctor_report(&h, true);
        assert!(report.contains("/opt/slopty/bin/slopty-worker"));
        assert!(report.contains(&format!("worker {}\n", h.worker)), "{report}");
        assert!(report.contains("✔ registered with the server at studio:45560"), "{report}");
        assert!(report.contains("✔ Screen Recording"));
        assert!(report.contains("✘ Accessibility"));
        assert!(report.contains("2 clients connected, 3 sessions"));
        assert!(report.contains("listening on [::]:45550"), "{report}");
        assert!(
            report.contains("admits loopback, the tailnet, and by address 10.0.0.0/8"),
            "{report}"
        );
        assert!(report.contains("✔ Tailscale: studio.tail1234.ts.net (100.64.0.3)"), "{report}");
        let alone = slopty_proto::ctl::Health {
            allow: Vec::new(),
            tailscale: Tailscale::Absent,
            ..h.clone()
        };
        let report = doctor_report(&alone, true);
        assert!(report.contains("admits loopback and the tailnet\n"), "{report}");
        assert!(report.contains("✘ no Tailscale this daemon can read"), "{report}");
        let with =
            |tailscale| doctor_report(&slopty_proto::ctl::Health { tailscale, ..h.clone() }, true);
        let report = with(Tailscale::Down { backend: BackendState::Stopped });
        assert!(report.contains("✘ Tailscale is Stopped:"), "{report}");
        let report = with(Tailscale::Unreachable { error: "timed out".to_owned() });
        assert!(report.contains("✘ Tailscale did not answer (timed out)"), "{report}");
        assert!(report.contains("✔ Clipboard reads\n"), "{report}");
        assert!(!report.contains("Wake for network access"), "{report}");
        let asks = slopty_proto::ctl::Health {
            pasteboard: PasteboardAccess::NotAskedYet,
            caps: WorkerCaps { wake_on_lan: Some(false), ..h.caps.clone() },
            ..h.clone()
        };
        let report = doctor_report(&asks, true);
        assert!(
            report.contains("✘ Clipboard reads need permission: allow pasting from other apps"),
            "{report}"
        );
        assert!(report.contains("✘ Wake for network access is off"), "{report}");
        assert!(report.contains("sudo pmset -a womp 1"), "{report}");
        let wakes = slopty_proto::ctl::Health {
            caps: WorkerCaps { wake_on_lan: Some(true), ..h.caps.clone() },
            ..h.clone()
        };
        assert!(doctor_report(&wakes, true).contains("✔ Wake for network access"));
        let full = slopty_proto::ctl::Health {
            caps: WorkerCaps {
                writes_failing: Some("Thread logs cannot be written: disk full".to_owned()),
                stops_at_logout: Some("it stops when you log out: run `x` there".to_owned()),
                ..h.caps.clone()
            },
            ..h.clone()
        };
        let said = doctor_report(&full, true);
        assert!(said.contains("✘ Thread logs cannot be written: disk full"), "{said}");
        assert!(said.contains("✘ the worker's services: it stops when you log out"), "{said}");
        assert!(!doctor_report(&wakes, true).contains("cannot be written"));
        let link = |link| {
            let server =
                Some(slopty_proto::ctl::ServerHealth { address: "studio:45560".to_owned(), link });
            doctor_report(&slopty_proto::ctl::Health { server, ..h.clone() }, true)
        };
        let down = link(LinkState::Redialling { why: "connection refused".to_owned() });
        assert!(
            down.contains(
                "✘ the server at studio:45560 is not linked (connection refused); dialling again"
            ),
            "{down}"
        );
        let refused = link(LinkState::Refused { why: "not granted".to_owned() });
        assert!(
            refused.contains("✘ the server at studio:45560 does not take this worker: not granted"),
            "{refused}"
        );
        let own = doctor_report(&slopty_proto::ctl::Health { server: None, ..h.clone() }, true);
        assert!(own.contains("✘ no server set: this worker runs on its own"), "{own}");
        let linux = doctor_report(&h, false);
        assert!(!linux.contains("Screen Recording") && !linux.contains("Accessibility"), "{linux}");
        assert!(linux.contains("no desktop to stream here"), "{linux}");
    }
}
