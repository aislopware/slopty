//! Trust on first use: a machine whose host key this Mac's `ssh` does not know yet is shown to
//! the person by its fingerprint, and trusted only on their word.
//!
//! The app's `ssh` never asks (`BatchMode`), so an unknown host key ends the first step with
//! "Host key verification failed" ([`DeployError::unknown_host_key`]). [`Ssh::explain`] then
//! connects once more with the person's own config and options, but into a known-hosts file of
//! its own with `StrictHostKeyChecking=accept-new` and no sign-in
//! (`PreferredAuthentications=none`): `ssh` itself writes the key it was shown, as it would to
//! `~/.ssh/known_hosts` (hashed when `HashKnownHosts` says so, `[host]:port` off port 22, under
//! `HostKeyAlias`), and nothing runs there. `ssh-keygen -l` reads each key's fingerprint from
//! that file. [`HostKey::trust`] appends exactly those lines to the first of `ssh`'s
//! `UserKnownHostsFile` for the target (`ssh -G`), so the key the person saw is the key that is
//! trusted, with no second connection in between to swap it. A key that changed is never
//! offered: that is `ssh` warning of someone in the middle, and the failure says so.

use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::process::Command;

use crate::{DeployError, Failure, Ssh};

/// A machine's host key that `ssh` here does not know yet, as `ssh` itself took it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HostKey {
    /// The machine, as the person named it.
    pub target: String,
    /// Each key it showed, for the person to compare with the machine's own.
    pub keys: Vec<Fingerprint>,
    /// The known-hosts lines `ssh` wrote for it.
    pub lines: String,
    /// Where trusting it writes them: the first of `ssh`'s `UserKnownHostsFile` for the target.
    pub file: PathBuf,
}

/// One host key's fingerprint, as `ssh-keygen -l` prints it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fingerprint {
    /// Its type as `ssh` names it: `ED25519`, `ECDSA`, `RSA`.
    pub kind: String,
    /// `SHA256:` and the digest.
    pub sha256: String,
}

impl Fingerprint {
    /// `ssh-keygen -l`'s lines (`256 SHA256:… host (ED25519)`) as fingerprints, a key named
    /// twice (by name and by address) once.
    #[must_use]
    pub fn read(said: &str) -> Vec<Self> {
        let mut keys: Vec<Self> = Vec::new();
        for line in said.lines() {
            let mut words = line.split_whitespace();
            let Some(sha256) = words.nth(1).filter(|w| w.starts_with("SHA256:")) else {
                continue;
            };
            let kind = words.next_back().and_then(|w| w.strip_prefix('(')?.strip_suffix(')'));
            let Some(kind) = kind else { continue };
            if keys.iter().all(|k| k.sha256 != sha256) {
                keys.push(Self { kind: kind.to_owned(), sha256: sha256.to_owned() });
            }
        }
        keys
    }

    /// Where an OpenSSH server keeps this key's public half, on macOS and Linux alike.
    #[must_use]
    pub fn public_file(&self) -> String {
        format!("/etc/ssh/ssh_host_{}_key.pub", self.kind.to_ascii_lowercase())
    }
}

/// Why a host key could not be read or trusted.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    /// A program could not be started.
    #[error("run {program}")]
    Run {
        /// Which.
        program: String,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// `ssh` wrote no key: it did not get as far as the key exchange.
    #[error("ssh showed no host key for {target}: {stderr}")]
    NoKey {
        /// The machine.
        target: String,
        /// What `ssh` printed, trimmed.
        stderr: String,
    },
    /// `ssh-keygen` could not read the key `ssh` wrote.
    #[error("ssh-keygen read no fingerprint: {stderr}")]
    Fingerprint {
        /// What it printed, trimmed.
        stderr: String,
    },
    /// `ssh -G` named no known-hosts file.
    #[error("ssh names no known-hosts file for {target}")]
    NoFile {
        /// The machine.
        target: String,
    },
    /// A file could not be made, read or written.
    #[error("{}", path.display())]
    File {
        /// Which.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
}

impl HostKey {
    /// Trust it: its lines go at the end of [`HostKey::file`], which is made (only its owner may
    /// read it) when there is none.
    ///
    /// # Errors
    ///
    /// When the file or its directory cannot be made or written.
    pub async fn trust(&self) -> Result<(), TrustError> {
        let (file, lines) = (self.file.clone(), self.lines.clone());
        let failed = |source| TrustError::File { path: self.file.clone(), source };
        tokio::task::spawn_blocking(move || append(&file, &lines))
            .await
            .map_err(|e| failed(io::Error::other(e)))?
            .map_err(failed)
    }

    /// The failure that offers it: the machine is new here, and the fingerprints to check.
    #[must_use]
    pub fn offer(self) -> Failure {
        let check = self.keys.first().map_or_else(String::new, |key| {
            format!(" Check it there with ssh-keygen -lf {}.", key.public_file())
        });
        Failure {
            title: format!("{} is new to this Mac", self.target),
            hint: Some(format!("Trust its key only if the fingerprint below is its own.{check}")),
            lines: self.keys.iter().map(|k| format!("{} {}", k.kind, k.sha256)).collect(),
            trust: Some(Box::new(self)),
            password: None,
            ends_sessions: None,
        }
    }
}

/// Append `lines` to `file` on lines of their own, making it and its directory private when
/// they are not there.
fn append(file: &Path, lines: &str) -> io::Result<()> {
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let mut known =
        std::fs::OpenOptions::new().read(true).append(true).create(true).mode(0o600).open(file)?;
    let mut text = String::new();
    // A last line with no end would run into the first one added.
    if known.metadata()?.len() > 0 {
        let mut last = [0_u8];
        known.seek(SeekFrom::End(-1))?;
        known.read_exact(&mut last)?;
        if last != *b"\n" {
            text.push('\n');
        }
    }
    text.push_str(lines.trim_end());
    text.push('\n');
    known.write_all(text.as_bytes())?;
    known.sync_all()
}

impl Ssh {
    /// `failed` as a window says it; when it is a host key `ssh` does not know yet, the failure
    /// carries the key, read as [`Ssh::host_key`] reads it, for the person to trust.
    pub async fn explain(&self, failed: &DeployError) -> Failure {
        if !failed.unknown_host_key() {
            return failed.failure();
        }
        match self.host_key().await {
            Ok(key) => key.offer(),
            Err(error) => {
                tracing::warn!(host = %self.target, %error, "read the unknown host key");
                failed.failure()
            }
        }
    }

    /// The host key the target shows, as `ssh` with this runner's options takes it, without
    /// signing in or trusting anything.
    ///
    /// # Errors
    ///
    /// When `ssh` or `ssh-keygen` cannot be run, `ssh` reaches no key exchange, or names no
    /// known-hosts file.
    pub async fn host_key(&self) -> Result<HostKey, TrustError> {
        let dir = Scratch::new("slopty-host-key")
            .map_err(|(path, source)| TrustError::File { path, source })?;
        let probe = dir.path().join("known_hosts");
        let mut ssh = Command::new(&self.program);
        // `ssh` takes the first value given for each option: these go before the runner's own.
        for option in [
            "BatchMode=yes",
            "StrictHostKeyChecking=accept-new",
            &format!("UserKnownHostsFile={}", probe.display()),
            "GlobalKnownHostsFile=/dev/null",
            "UpdateHostKeys=no",
            "PreferredAuthentications=none",
            "ConnectTimeout=15",
        ] {
            ssh.arg("-o").arg(option);
        }
        ssh.args(&self.options).arg(&self.target).arg("true");
        let out = run(&mut ssh, &self.program.display().to_string()).await?;
        let lines = tokio::fs::read_to_string(&probe).await.unwrap_or_default();
        if lines.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(TrustError::NoKey { target: self.target.clone(), stderr });
        }
        let keys = fingerprints(&probe).await?;
        let file = self.known_hosts().await?;
        Ok(HostKey { target: self.target.clone(), keys, lines, file })
    }

    /// The first known-hosts file `ssh` reads for the target, as `ssh -G` resolves it.
    async fn known_hosts(&self) -> Result<PathBuf, TrustError> {
        let mut ssh = Command::new(&self.program);
        ssh.args(&self.options).arg("-G").arg(&self.target);
        let out = run(&mut ssh, &self.program.display().to_string()).await?;
        let said = String::from_utf8_lossy(&out.stdout);
        let first = said
            .lines()
            .find_map(|line| line.strip_prefix("userknownhostsfile "))
            .and_then(|files| files.split_whitespace().next());
        let file = first.ok_or_else(|| TrustError::NoFile { target: self.target.clone() })?;
        Ok(match file.strip_prefix("~/") {
            Some(rest) => slopty_platform::dirs::home().join(rest),
            None => PathBuf::from(file),
        })
    }
}

/// Run `command` to its end, nothing fed to it; what it printed.
async fn run(command: &mut Command, program: &str) -> Result<std::process::Output, TrustError> {
    command.stdin(Stdio::null()).kill_on_drop(true);
    command.output().await.map_err(|source| TrustError::Run { program: program.to_owned(), source })
}

/// Each key in the known-hosts file `file`, by `ssh-keygen -l`, once each.
async fn fingerprints(file: &Path) -> Result<Vec<Fingerprint>, TrustError> {
    let mut keygen = Command::new("ssh-keygen");
    keygen.arg("-l").arg("-f").arg(file);
    let out = run(&mut keygen, "ssh-keygen").await?;
    let keys = Fingerprint::read(&String::from_utf8_lossy(&out.stdout));
    if !out.status.success() || keys.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(TrustError::Fingerprint { stderr });
    }
    Ok(keys)
}

/// A directory only this user can open, removed with everything in it when dropped.
#[derive(Debug)]
pub struct Scratch(PathBuf);

impl Scratch {
    /// A new one in the temporary directory, named from `prefix`; kept short, since a socket in
    /// it must fit a socket address. The path it could not make, and why.
    pub fn new(prefix: &str) -> Result<Self, (PathBuf, io::Error)> {
        static MADE: AtomicU64 = AtomicU64::new(0);
        let n = MADE.fetch_add(1, Ordering::Relaxed);
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path =
            std::env::temp_dir().join(format!("{prefix}-{:x}-{at:x}-{n}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => Ok(Self(path)),
            Err(e) => Err((path, e)),
        }
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!(path = %self.0.display(), error = %e, "remove a scratch directory");
        }
    }
}
