//! `slopty worker deploy <ssh target>`: put a worker on another machine over the system `ssh`,
//! or update the one there. The steps are [`slopty_deploy`]'s, which the app runs too; this
//! finds the server the new worker registers with (as every verb finds it), prints each upload
//! as it starts, lets the remote install write to this terminal, and says what the person does
//! next.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use slopty_deploy::{Deployed, Event, Os, Plan, STAGE, Served, Server, Ssh, Step};
use slopty_proto::ctl::Tailscale;

/// `slopty worker deploy` options.
#[derive(Args, Debug, Clone)]
pub struct DeployOpts {
    /// The machine, as `ssh` takes it: a `~/.ssh/config` host, `user@host`, a tailnet name.
    target: String,
    /// Replace the worker installed there, keeping its port and address; the previous one is
    /// put back if the new one does not come up. With none there, one is installed.
    #[arg(long)]
    update: bool,
    /// Where the binaries built for the target are (default: this binary's directory, and the
    /// Linux builds an app bundle carries beside it).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// The `ssh` to run.
    #[arg(long, default_value = "ssh")]
    ssh: PathBuf,
    /// Go on when the update restarts `slopty-ptyd` there, ending every shell and agent turn
    /// it holds. Without it such an update stops before changing anything, and says so.
    #[arg(long)]
    end_sessions: bool,
}

impl DeployOpts {
    /// The machine.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// `--bin-dir`.
    #[must_use]
    pub fn bin_dir(&self) -> Option<&Path> {
        self.bin_dir.as_deref()
    }
}

/// Deploy (or with `--update`, replace) the worker on `opts.target`, from `source`, registered
/// with the server `--server` names, else the one every verb would reach from `data_dir`.
///
/// # Errors
///
/// When no server is named or answers, the machine or the binaries do not fit, a step there
/// fails, or the new worker did not come up (after an update, the previous one is back then).
pub async fn deploy(
    opts: &DeployOpts,
    server: Option<&str>,
    data_dir: &Path,
    source: &Path,
) -> Result<Deployed> {
    let server = register_with(server, data_dir).await?;
    let ssh = Ssh::new(opts.ssh.clone(), opts.target.clone());
    let plan = Plan {
        sources: sources(opts.bin_dir(), source),
        update: opts.update,
        server,
        end_sessions: opts.end_sessions,
        password: None,
        add_key: false,
    };
    let target = &opts.target;
    let mut say = |event: Event| {
        if let Event::Step(Step::Upload { name }) = event {
            println!("uploading {name} to {target}:{STAGE}");
        }
    };
    Ok(slopty_deploy::deploy(&ssh, &plan, &mut say).await?)
}

/// The server a deployed worker registers with: `--server`, `$SLOPTY_SERVER` or `[client]
/// server`, else the first that answers on the tailnet ([`crate::link::locate`]). With none, the
/// deploy stops before it starts: a worker registered nowhere is listed by no client.
async fn register_with(flag: Option<&str>, data_dir: &Path) -> Result<Server> {
    let endpoint = slopty_net::client::bind_client()?;
    let located = crate::link::locate(flag, data_dir, &endpoint).await;
    crate::client::close_endpoint(&endpoint).await;
    let address = located.map_err(|e| {
        e.context(
            "a worker registers with a server: start one with `slopty server install` here or \
             `slopty server deploy <ssh target>`",
        )
    })?;
    Ok(Server { host: address.host().to_owned(), port: address.port() })
}

/// `slopty server deploy` options.
#[derive(Args, Debug, Clone)]
pub struct ServerDeployOpts {
    /// The machine, as `ssh` takes it: a `~/.ssh/config` host, `user@host`, a tailnet name.
    target: String,
    /// Where the binaries built for the target are (default: this binary's directory, and the
    /// Linux builds an app bundle carries beside it).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// The `ssh` to run.
    #[arg(long, default_value = "ssh")]
    ssh: PathBuf,
}

impl ServerDeployOpts {
    /// The machine.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// `--bin-dir`.
    #[must_use]
    pub fn bin_dir(&self) -> Option<&Path> {
        self.bin_dir.as_deref()
    }
}

/// The binaries to send: `--bin-dir` alone, else `source`'s and the Linux builds beside it.
fn sources(bin_dir: Option<&Path>, source: &Path) -> Vec<PathBuf> {
    if bin_dir.is_some() { vec![source.to_path_buf()] } else { slopty_deploy::bundled(source) }
}

/// Put the server on `opts.target` from `source`, or replace the one there.
///
/// # Errors
///
/// When the machine or the binaries do not fit, a step there fails, or the server did not come
/// up there.
pub async fn serve(opts: &ServerDeployOpts, source: &Path) -> Result<Served> {
    let ssh = Ssh::new(opts.ssh.clone(), opts.target.clone());
    let target = &opts.target;
    let mut say = |event: Event| {
        if let Event::Step(Step::Upload { name }) = event {
            println!("uploading {name} to {target}:{STAGE}");
        }
    };
    let sources = sources(opts.bin_dir(), source);
    Ok(slopty_deploy::serve(&ssh, &sources, &mut say).await?)
}

/// What the person reads once a server is up.
#[must_use]
pub fn served(target: &str, served: &Served) -> String {
    let at = served.addresses.first().map_or(target, String::as_str);
    format!(
        "slopty-server is up on {target} ({}); point clients at it with `slopty --server {at}` or \
         the app's \"Connect to a server\"\n",
        served.platform
    )
}

/// What the person reads once a deploy is done.
#[must_use]
pub fn report(target: &str, deployed: &Deployed) -> String {
    let Deployed { platform, health, server, ptyd, stops_at_logout, console, .. } = deployed;
    let mut out = vec![format!(
        "slopty-worker {} is up on {target} ({platform}), running {}",
        health.caps.build, health.exe
    )];
    if platform.os == Os::MacOs && !(health.caps.can_capture && health.caps.can_inject) {
        let missing: Vec<&str> = [
            (!health.caps.can_capture).then_some("Screen & System Audio Recording"),
            (!health.caps.can_inject).then_some("Accessibility"),
        ]
        .into_iter()
        .flatten()
        .collect();
        out.push(format!(
            "terminals and agents work now; its screen waits for someone at {target} to allow \
             {} for {} in System Settings ▸ Privacy & Security",
            missing.join(" and "),
            health.exe
        ));
    }
    let address = match &health.tailscale {
        Tailscale::Up { node, .. } => node.as_str(),
        Tailscale::Down { .. } | Tailscale::Unreachable { .. } | Tailscale::Absent => target,
    };
    out.push(format!(
        "it registers with the server at {server}, so every client of it lists {address}"
    ));
    out.extend(ptyd.map(|ptyd| ptyd.to_string()));
    out.extend(stops_at_logout.clone());
    if console.logged_in == Some(false) {
        let way = if console.filevault == Some(true) {
            "FileVault is on there, so after a restart unlock its disk with `ssh` first, then log \
             in through Screen Sharing"
        } else {
            "for a Mac nobody sits at, turn on automatic login in System Settings ▸ Users & \
             Groups (FileVault must be off)"
        };
        out.push(format!(
            "nobody is logged in at {target}, and Slopty runs once someone is: {way}"
        ));
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests;
