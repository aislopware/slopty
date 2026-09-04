//! `xtask ios`: the iOS build pipeline (filled in during the iOS phase).

use anyhow::{Result, bail};
use clap::Subcommand;
use xshell::Shell;

/// iOS subcommands.
#[derive(Subcommand, Debug)]
pub enum IosCmd {
    /// Build the Rust static library for the simulator and run it.
    Sim,
    /// Build for a connected device and run it.
    Device,
}

pub fn run(_sh: &Shell, cmd: &IosCmd) -> Result<()> {
    bail!("iOS pipeline is not wired yet ({cmd:?}); see docs/ARCHITECTURE.md §6")
}
