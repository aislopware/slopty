//! `slopty host …`: the daemon's local control socket.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use slopty_host::ctl::{CtlReply, CtlRequest};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

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
}

fn socket() -> PathBuf {
    std::env::var_os("SLOPTY_HOSTD_SOCKET")
        .map_or_else(|| std::env::temp_dir().join("slopty").join("hostd.sock"), PathBuf::from)
}

pub async fn run(cmd: HostCmd) -> Result<()> {
    let req = match cmd {
        HostCmd::Ticket => CtlRequest::Ticket,
        HostCmd::Status => CtlRequest::Status,
        HostCmd::Paired => CtlRequest::Paired,
        HostCmd::Revoke { endpoint } => CtlRequest::Revoke { endpoint },
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
    let path = socket();
    let stream = UnixStream::connect(&path)
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
