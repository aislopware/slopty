//! `slopty worker deploy <ssh target>`: put a worker on another machine over the system `ssh`,
//! or update the one there. The steps are [`slopty_deploy`]'s, which the app runs too; this
//! prints each upload as it starts, lets the remote install write to this terminal, and says
//! what the person does next.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use slopty_deploy::{Deployed, Event, Os, Plan, STAGE, Ssh, Step};
use slopty_proto::ctl::Tailscale;

/// `slopty worker deploy` options.
#[derive(Args, Debug, Clone)]
pub struct DeployOpts {
    /// The machine, as `ssh` takes it: a `~/.ssh/config` host, `user@host`, a tailnet name.
    target: String,
    /// Replace the worker installed there, keeping its port and address; the previous one is
    /// put back if the new one does not come up.
    #[arg(long)]
    update: bool,
    /// Where the binaries built for the target are (default: this binary's directory).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// The `ssh` to run.
    #[arg(long, default_value = "ssh")]
    ssh: PathBuf,
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

/// Deploy (or with `--update`, replace) the worker on `opts.target`, from `source`.
///
/// # Errors
///
/// When the machine or the binaries do not fit, a step there fails, or the new worker did not
/// come up (after an update, the previous one is back then).
pub async fn deploy(opts: &DeployOpts, source: &Path) -> Result<Deployed> {
    let ssh = Ssh::new(opts.ssh.clone(), opts.target.clone());
    let plan = Plan { source: source.to_path_buf(), update: opts.update };
    let target = &opts.target;
    let mut say = |event: Event| {
        if let Event::Step(Step::Upload { name }) = event {
            println!("uploading {name} to {target}:{STAGE}");
        }
    };
    Ok(slopty_deploy::deploy(&ssh, &plan, &mut say).await?)
}

/// What the person reads once a deploy is done.
#[must_use]
pub fn report(target: &str, deployed: &Deployed) -> String {
    let Deployed { platform, health } = deployed;
    let mut out = vec![format!(
        "slopty-worker {} is up on {target} ({platform}), running {}",
        health.version, health.exe
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
    out.push(format!("add it from a client with `slopty add {address}`"));
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests;
