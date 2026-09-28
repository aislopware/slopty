//! `slopty worker deploy <ssh target>`: put a worker on another machine over the system `ssh`,
//! or update the one there.
//!
//! The system `ssh` carries everything, so `~/.ssh/config`, `ControlMaster` and Tailscale SSH
//! apply as they do to a typed `ssh`. The steps:
//!
//! 1. `uname -sm` there names its OS and CPU, and each binary to upload is read for its own
//!    ([`Platform::of_binary`]): a build for another machine is refused before anything moves.
//! 2. `slopty-ptyd`, `slopty-worker` and `slopty` go to [`STAGE`] under the remote home, each
//!    written beside its name and moved over it once whole.
//! 3. The uploaded `slopty worker install` runs there, which installs the services the way a local
//!    install does, waits for the new worker to answer as itself, and with `--update` puts the
//!    previous worker back when it does not ([`crate::service`]).
//! 4. `slopty worker doctor --json` there says what it can do; a Mac still needs a person at the
//!    desk for Screen Recording and Accessibility.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::Args;
use slopty_platform::service::WORKER_BINARIES;
use slopty_proto::ctl::{Health, Tailscale};
use tokio::process::Command;

/// Where the binaries land on the remote host, relative to its home (where `ssh` starts).
pub const STAGE: &str = ".slopty/deploy";

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

/// An OS and a CPU, as a machine or a binary names them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Platform {
    /// The OS.
    pub os: Os,
    /// The CPU.
    pub arch: Arch,
}

/// The OSes a worker runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    /// macOS (`Darwin`, Mach-O).
    MacOs,
    /// Linux (ELF).
    Linux,
}

/// The CPUs a worker runs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    /// 64-bit ARM.
    Arm64,
    /// `x86_64`.
    X86_64,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let os = match self.os {
            Os::MacOs => "macOS",
            Os::Linux => "Linux",
        };
        let arch = match self.arch {
            Arch::Arm64 => "arm64",
            Arch::X86_64 => "x86_64",
        };
        write!(f, "{os} {arch}")
    }
}

/// `CPU_TYPE_ARM64` and `CPU_TYPE_X86_64` from `<mach/machine.h>`.
const MACHO_ARM64: u32 = 0x0100_000c;
const MACHO_X86_64: u32 = 0x0100_0007;
/// `EM_X86_64` and `EM_AARCH64` from `<elf.h>`.
const ELF_X86_64: u16 = 62;
const ELF_AARCH64: u16 = 183;

impl Platform {
    /// The machine `uname -sm` describes.
    ///
    /// # Errors
    ///
    /// For an OS or a CPU no worker is built for.
    pub fn from_uname(uname: &str) -> Result<Self> {
        let mut words = uname.split_whitespace();
        let os = match words.next() {
            Some("Darwin") => Os::MacOs,
            Some("Linux") => Os::Linux,
            other => bail!("no worker runs on {}", other.unwrap_or("an OS that has no name")),
        };
        let arch = match words.next() {
            Some("arm64" | "aarch64") => Arch::Arm64,
            Some("x86_64" | "amd64") => Arch::X86_64,
            other => bail!("no worker is built for a {} CPU", other.unwrap_or("nameless")),
        };
        Ok(Self { os, arch })
    }

    /// Every platform a binary's header says it runs on: one for a thin binary, each slice of
    /// a universal one, none for what is no executable of ours.
    #[must_use]
    pub fn of_binary(head: &[u8]) -> Vec<Self> {
        let le32 =
            |at: usize| head.get(at..at.checked_add(4)?)?.try_into().ok().map(u32::from_le_bytes);
        let be32 =
            |at: usize| head.get(at..at.checked_add(4)?)?.try_into().ok().map(u32::from_be_bytes);
        let mac = |cpu: u32| {
            let arch = match cpu {
                MACHO_ARM64 => Arch::Arm64,
                MACHO_X86_64 => Arch::X86_64,
                _ => return None,
            };
            Some(Self { os: Os::MacOs, arch })
        };
        match head.get(..4) {
            // MH_MAGIC_64, as a little-endian machine writes it.
            Some([0xcf, 0xfa, 0xed, 0xfe]) => le32(4).and_then(mac).into_iter().collect(),
            // FAT_MAGIC: a count, then 20-byte `fat_arch` records, big-endian.
            Some([0xca, 0xfe, 0xba, 0xbe]) => {
                let count = be32(4).unwrap_or(0).min(16);
                (0..count)
                    .filter_map(|i| {
                        let at = usize::try_from(i).ok()?.checked_mul(20)?.checked_add(8)?;
                        be32(at).and_then(mac)
                    })
                    .collect()
            }
            // ELF, 64-bit, little-endian: `e_machine` at 18.
            Some([0x7f, b'E', b'L', b'F']) if head.get(4..6) == Some(&[2, 1]) => {
                let machine =
                    head.get(18..20).and_then(|b| b.try_into().ok()).map(u16::from_le_bytes);
                let arch = match machine {
                    Some(ELF_AARCH64) => Arch::Arm64,
                    Some(ELF_X86_64) => Arch::X86_64,
                    _ => return Vec::new(),
                };
                vec![Self { os: Os::Linux, arch }]
            }
            _ => Vec::new(),
        }
    }
}

/// The platforms the binary at `path` runs on.
fn platforms_of(path: &Path) -> Result<Vec<Platform>> {
    let mut head = Vec::with_capacity(512);
    std::fs::File::open(path)
        .and_then(|file| file.take(512).read_to_end(&mut head))
        .with_context(|| format!("read {}", path.display()))?;
    Ok(Platform::of_binary(&head))
}

/// The remote host, reached with one `ssh` per step.
struct Remote<'a> {
    ssh: &'a Path,
    target: &'a str,
}

impl Remote<'_> {
    /// `script` run by `sh` there, whatever the login shell (the scripts hold no `'`).
    fn command(&self, script: &str) -> Command {
        let mut ssh = Command::new(self.ssh);
        ssh.arg(self.target).arg(format!("sh -c '{script}'")).kill_on_drop(true);
        ssh
    }

    /// What `script` printed; its error output when it fails.
    async fn output(&self, script: &str) -> Result<String> {
        let out = self
            .command(script)
            .stdin(Stdio::null())
            .output()
            .await
            .with_context(|| format!("run {}", self.ssh.display()))?;
        if !out.status.success() {
            bail!(
                "`{script}` on {} failed ({}): {}",
                self.target,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// `script` with the person watching: its output goes straight to this terminal.
    async fn run(&self, script: &str) -> Result<()> {
        let status = self
            .command(script)
            .stdin(Stdio::null())
            .status()
            .await
            .with_context(|| format!("run {}", self.ssh.display()))?;
        if !status.success() {
            bail!("`{script}` on {} failed ({status}); see above", self.target);
        }
        Ok(())
    }

    /// Copy `from` to `<STAGE>/<name>` there, executable, replacing it only once it is whole.
    async fn upload(&self, from: &Path, name: &str) -> Result<()> {
        let file = std::fs::File::open(from).with_context(|| format!("open {}", from.display()))?;
        let part = format!("{STAGE}/{name}.part");
        let script = format!(
            "mkdir -p {STAGE} && cat > {part} && chmod 755 {part} && mv -f {part} {STAGE}/{name}"
        );
        let out = self
            .command(&script)
            .stdin(Stdio::from(file))
            .stdout(Stdio::null())
            .output()
            .await
            .with_context(|| format!("run {}", self.ssh.display()))?;
        if !out.status.success() {
            bail!(
                "uploading {name} to {} failed ({}): {}",
                self.target,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// What a deploy left running there.
#[derive(Debug)]
pub struct Deployed {
    /// The machine.
    pub platform: Platform,
    /// The worker's own account of itself.
    pub health: Health,
}

/// Deploy (or with `--update`, replace) the worker on `opts.target`, from `source`.
///
/// # Errors
///
/// When the machine or the binaries do not fit, a step there fails, or the new worker did not
/// come up (after an update, the previous one is back then).
pub async fn deploy(opts: &DeployOpts, source: &Path) -> Result<Deployed> {
    if opts.target.starts_with('-') {
        bail!("{:?} is not an ssh target", opts.target);
    }
    let remote = Remote { ssh: &opts.ssh, target: &opts.target };
    let platform = Platform::from_uname(&remote.output("uname -sm").await?)
        .with_context(|| format!("on {}", opts.target))?;
    for name in WORKER_BINARIES {
        let path = source.join(name);
        let runs_on = platforms_of(&path)?;
        if !runs_on.contains(&platform) {
            let built = runs_on
                .first()
                .map_or_else(|| "no machine we know".to_owned(), Platform::to_string);
            bail!(
                "{} is built for {built}, and {} is {platform}; pass --bin-dir with a build for it",
                path.display(),
                opts.target
            );
        }
    }
    for name in WORKER_BINARIES {
        println!("uploading {name} to {}:{STAGE}", opts.target);
        remote.upload(&source.join(name), name).await?;
    }
    let mode = if opts.update { "--update" } else { "--fresh" };
    remote.run(&format!("{STAGE}/slopty worker install --bin-dir {STAGE} {mode}")).await?;
    let doctor = remote.output(&format!("{STAGE}/slopty --json worker doctor")).await?;
    let health: Health = serde_json::from_str(&doctor)
        .map_err(|e| anyhow!("the worker's doctor said {doctor:?}: {e}"))?;
    Ok(Deployed { platform, health })
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
