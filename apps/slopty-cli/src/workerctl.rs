//! `slopty worker …`: the daemon's local control socket.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use slopty_proto::ctl::{CtlReply, CtlRequest, Tailscale};

use crate::service;

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
    /// Run `slopty-ptyd` and `slopty-worker` as services of this session (`LaunchAgents` on macOS,
    /// systemd user units on Linux): they start now and at every login.
    Install(service::InstallOpts),
    /// Stop the services and remove them (open sessions die with ptyd).
    Uninstall,
    /// Whether the services are installed and running.
    Service,
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

pub async fn run(cmd: WorkerCmd, server: Option<&str>, data_dir: &Path) -> Result<()> {
    let req = match cmd {
        WorkerCmd::Status => CtlRequest::Status,
        WorkerCmd::Doctor => CtlRequest::Doctor,
        WorkerCmd::Screens => CtlRequest::Screens,
        WorkerCmd::Install(opts) => return service::install(&opts, server, data_dir).await,
        WorkerCmd::Uninstall => return service::uninstall().await,
        WorkerCmd::Service => {
            service::status();
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
        CtlReply::Doctor(health) => {
            print!("{}", doctor_report(&health, DESKTOP));
            if DESKTOP && !(health.screen_recording && health.post_events) {
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
        CtlReply::Ok { changed } => println!("{}", if changed { "done" } else { "no change" }),
        CtlReply::Permission(answer) => bail!("a permission decision nobody asked for: {answer:?}"),
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
    let screen = if h.screen_recording {
        String::new()
    } else {
        "  → System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording: add the \
         daemon binary above, then `slopty worker install` (or restart the daemon)"
            .to_owned()
    };
    let post = if h.post_events {
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
    let ranges = if h.allow.is_empty() {
        "admits loopback and the tailnet".to_owned()
    } else {
        format!("admits loopback, the tailnet, and by address {}", h.allow.join(", "))
    };
    let grants = if desktop {
        vec![
            format!("{} Screen Recording{screen}", mark(h.screen_recording)),
            format!("{} Accessibility (remote-window input){post}", mark(h.post_events)),
        ]
    } else {
        vec!["no desktop to stream here: terminals, files and agents only".to_owned()]
    };
    let lines = [
        vec![
            format!("slopty-worker {}  ({})", h.version, h.exe),
            format!("up {} s · listening on {}", h.uptime_secs, h.listen),
            ranges,
            tailscale,
        ],
        grants,
        vec![format!("{} clients connected, {} sessions", h.clients, h.sessions)],
    ]
    .concat();
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use slopty_proto::ctl::NotUp;

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

    #[test]
    fn doctor_report_names_the_binary_and_flags_missing_permissions() {
        let h = slopty_proto::ctl::Health {
            version: "0.3.0".to_owned(),
            exe: "/opt/slopty/bin/slopty-worker".to_owned(),
            screen_recording: true,
            post_events: false,
            listen: "[::]:45550".to_owned(),
            allow: vec!["10.0.0.0/8".to_owned()],
            tailscale: Tailscale::Up {
                node: "studio.tail1234.ts.net".to_owned(),
                ip: Some([100, 64, 0, 3].into()),
            },
            clients: 2,
            sessions: 3,
            uptime_secs: 61,
        };
        let report = doctor_report(&h, true);
        assert!(report.contains("/opt/slopty/bin/slopty-worker"));
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
        let report = with(Tailscale::Down { backend: NotUp::Stopped });
        assert!(report.contains("✘ Tailscale is Stopped:"), "{report}");
        let report = with(Tailscale::Unreachable { error: "timed out".to_owned() });
        assert!(report.contains("✘ Tailscale did not answer (timed out)"), "{report}");
        let linux = doctor_report(&h, false);
        assert!(!linux.contains("Screen Recording") && !linux.contains("Accessibility"), "{linux}");
        assert!(linux.contains("no desktop to stream here"), "{linux}");
    }
}
