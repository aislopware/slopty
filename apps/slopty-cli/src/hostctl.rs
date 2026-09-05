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
