//! `slopty host …`: the daemon's local control socket.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use slopty_host::ctl::{CtlReply, CtlRequest};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

use crate::service;

#[derive(Subcommand, Debug)]
pub enum HostCmd {
    /// Print a pairing ticket (valid 10 minutes, single use).
    Ticket,
    /// Identity and sessions.
    Status,
    /// Paired clients.
    Paired,
    /// Check the daemon's permissions (Screen Recording, Accessibility), reach, port and links.
    Doctor,
    /// Revoke a paired client.
    Revoke {
        /// Endpoint id (hex).
        endpoint: String,
    },
    /// Run `slopty-ptyd` and `slopty-hostd` as `LaunchAgents` (starts now and at every login).
    Install(service::InstallOpts),
    /// Stop the `LaunchAgents` and remove them (open sessions die with ptyd).
    Uninstall {
        /// Data directory the agents were installed with.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Whether the `LaunchAgents` are installed and running.
    Service {
        /// Data directory the agents were installed with.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
}

/// `$SLOPTY_HOSTD_SOCKET`, else the installed agents' socket under the data dir when it
/// exists, else the dev default `$TMPDIR/slopty/hostd.sock`.
fn socket() -> PathBuf {
    if let Some(p) = std::env::var_os("SLOPTY_HOSTD_SOCKET") {
        return PathBuf::from(p);
    }
    let installed = crate::client::data_dir().join("run").join("hostd.sock");
    if installed.exists() {
        return installed;
    }
    std::env::temp_dir().join("slopty").join("hostd.sock")
}

pub async fn run(cmd: HostCmd) -> Result<()> {
    let req = match cmd {
        HostCmd::Ticket => CtlRequest::Ticket,
        HostCmd::Status => CtlRequest::Status,
        HostCmd::Paired => CtlRequest::Paired,
        HostCmd::Doctor => CtlRequest::Doctor,
        HostCmd::Revoke { endpoint } => CtlRequest::Revoke { endpoint },
        HostCmd::Install(opts) => return service::install(&opts).await,
        HostCmd::Uninstall { data_dir } => return service::uninstall(data_dir.as_deref()),
        HostCmd::Service { data_dir } => {
            service::status(data_dir.as_deref());
            return Ok(());
        }
    };
    match call(req).await? {
        CtlReply::Ticket { ticket } => println!("{ticket}"),
        CtlReply::Status { id, name, sessions } => {
            println!("{name}  {id}");
            for s in sessions {
                println!(
                    "  {}  {}x{}  {:?}  {} viewer(s)  {}",
                    s.id, s.cols, s.rows, s.state, s.viewers, s.title
                );
            }
        }
        CtlReply::Paired { paired } => {
            for p in paired {
                println!("{}  {}  {}", p.endpoint, p.name, p.client);
            }
        }
        CtlReply::Doctor(health) => {
            print!("{}", doctor_report(&health));
            if !(health.screen_recording && health.post_events) {
                bail!("permissions missing; see above");
            }
        }
        CtlReply::Ok { changed } => println!("{}", if changed { "done" } else { "no change" }),
        CtlReply::Error { message } => bail!("{message}"),
    }
    Ok(())
}

pub async fn call(req: CtlRequest) -> Result<CtlReply> {
    call_at(&socket(), req).await
}

/// One request over the daemon's control socket at `path`.
pub async fn call_at(path: &std::path::Path, req: CtlRequest) -> Result<CtlReply> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("is slopty-hostd running? ({})", path.display()))?;
    let (rd, mut wr) = stream.into_split();
    let mut line = serde_json::to_vec(&req)?;
    line.push(b'\n');
    wr.write_all(&line).await?;
    wr.shutdown().await?;
    let mut reply = String::new();
    BufReader::new(rd).read_line(&mut reply).await?;
    Ok(serde_json::from_str(reply.trim())?)
}

/// The doctor's checklist. Permissions are granted to the daemon *binary*, so the report
/// names the path to add in System Settings.
fn doctor_report(h: &slopty_host::ctl::Health) -> String {
    let mark = |ok: bool| if ok { "✔" } else { "✘" };
    let screen = if h.screen_recording {
        String::new()
    } else {
        "  → System Settings ▸ Privacy & Security ▸ Screen & System Audio Recording: add the \
         daemon binary above, then `slopty host install` (or restart the daemon)"
            .to_owned()
    };
    let post = if h.post_events {
        String::new()
    } else {
        "  → System Settings ▸ Privacy & Security ▸ Accessibility: add the daemon binary above"
            .to_owned()
    };
    let lines = [
        format!("slopty-hostd {}  ({})", h.version, h.exe),
        format!("up {} s · reach {} · udp {}", h.uptime_secs, h.reach, h.port),
        format!("{} Screen Recording{screen}", mark(h.screen_recording)),
        format!("{} Accessibility (remote-window input){post}", mark(h.post_events)),
        format!("{} clients connected, {} sessions", h.clients, h.sessions),
    ];
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_report_names_the_binary_and_flags_missing_permissions() {
        let h = slopty_host::ctl::Health {
            version: "0.3.0".to_owned(),
            exe: "/opt/slopty/bin/slopty-hostd".to_owned(),
            screen_recording: true,
            post_events: false,
            reach: "DirectOnly".to_owned(),
            port: 45550,
            clients: 2,
            sessions: 3,
            uptime_secs: 61,
        };
        let report = doctor_report(&h);
        assert!(report.contains("/opt/slopty/bin/slopty-hostd"));
        assert!(report.contains("✔ Screen Recording"));
        assert!(report.contains("✘ Accessibility"));
        assert!(report.contains("2 clients connected, 3 sessions"));
    }
}
