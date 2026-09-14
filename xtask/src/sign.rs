//! `xtask sign`: give the dev daemons a code identity that outlives a rebuild.
//!
//! `cargo build` ad-hoc signs, so every rebuild is a new executable as far as TCC is concerned and
//! the Screen Recording and Accessibility approvals the last build was given are gone with it. The
//! symptom is ScreenCaptureKit `-3801` from a daemon that worked an hour ago, with no prompt to
//! explain it. Signing with a Developer ID certificate and a fixed identifier makes the designated
//! requirement the identifier plus the certificate rather than the hash, so one approval covers
//! every later build (`docs/decisions/input.md`, the host-daemon signing entry).

use anyhow::{Context as _, Result, bail};
use clap::Args;
use xshell::{Shell, cmd};

use crate::tools::{repo_root, step};

/// Identifier prefix, the same string the `LaunchAgent`s are labelled with.
const IDENTIFIER_PREFIX: &str = "dev.aislopware.slopty";

/// Environment variable naming the identity, for a machine with several certificates.
pub const IDENTITY_ENV: &str = "SLOPTY_SIGN_IDENTITY";

/// The binaries that hold TCC grants, as (file name, identifier suffix).
const SIGNED: [(&str, &str); 2] = [("slopty-hostd", "hostd"), ("slopty-ptyd", "ptyd")];

/// `xtask sign` options.
#[derive(Args, Debug, Clone, Default)]
pub struct SignOpts {
    /// Sign the `--release` binaries instead of the debug ones.
    #[arg(long)]
    pub release: bool,
    /// Signing identity; defaults to `$SLOPTY_SIGN_IDENTITY`, else the keychain's first
    /// `Developer ID Application` certificate.
    #[arg(long)]
    pub identity: Option<String>,
}

/// The identifier a daemon is signed under.
fn identifier(suffix: &str) -> String {
    format!("{IDENTIFIER_PREFIX}.{suffix}")
}

/// The first `Developer ID Application` certificate in `security find-identity` output.
///
/// Only that kind is taken. An Apple Development certificate expires within the year, and a TCC
/// approval tied to one disappears with it, which is the failure this command exists to end.
fn pick_identity(listing: &str) -> Option<&str> {
    listing
        .lines()
        .filter_map(|line| line.split('"').nth(1))
        .find(|name| name.starts_with("Developer ID Application:"))
}

/// The identity to sign with: the flag, then the environment, then the keychain.
fn resolve_identity(sh: &Shell, given: Option<&str>) -> Result<String> {
    if let Some(identity) = given.filter(|id| !id.trim().is_empty()) {
        return Ok(identity.to_owned());
    }
    if let Some(identity) = std::env::var(IDENTITY_ENV).ok().filter(|id| !id.trim().is_empty()) {
        return Ok(identity);
    }
    let listing = cmd!(sh, "security find-identity -v -p codesigning")
        .quiet()
        .read()
        .context("security find-identity")?;
    pick_identity(&listing).map(str::to_owned).with_context(|| {
        format!(
            "no Developer ID Application certificate in the keychain; \
             pass --identity or set {IDENTITY_ENV}"
        )
    })
}

pub fn run(sh: &Shell, opts: &SignOpts) -> Result<()> {
    let identity = resolve_identity(sh, opts.identity.as_deref())?;
    let profile = if opts.release { "release" } else { "debug" };
    let dir = repo_root()?.join("target").join(profile);
    for (bin, suffix) in SIGNED {
        let path = dir.join(bin);
        if !path.exists() {
            bail!("missing {path}: build the host daemons first (`cargo xtask run host`)");
        }
        let id = identifier(suffix);
        step(
            &format!("codesign {bin} as {id}"),
            &cmd!(
                sh,
                "codesign --force --options runtime --timestamp=none
                 --identifier {id} --sign {identity} {path}"
            ),
        )?;
    }
    println!(
        "✔ signed as {IDENTIFIER_PREFIX}.*; approve them once under \
         System Settings → Privacy & Security and the approval survives every rebuild"
    );
    Ok(())
}

/// Sign the daemons if a certificate is there, and say so plainly when there is not.
///
/// `run host` calls this: an unsigned daemon still runs, it just loses its permissions on the next
/// build, so a missing certificate is a warning rather than the end of the run.
pub fn sign_if_possible(sh: &Shell, release: bool) {
    let opts = SignOpts { release, identity: None };
    if let Err(error) = run(sh, &opts) {
        println!("  ! not signing the daemons: {error}");
        println!("    they will lose Screen Recording and Accessibility on the next build");
    }
}

#[cfg(test)]
mod tests {
    use super::{identifier, pick_identity};

    #[test]
    fn a_daemon_is_signed_under_its_launchagent_label() {
        assert_eq!(identifier("hostd"), "dev.aislopware.slopty.hostd");
        assert_eq!(identifier("ptyd"), "dev.aislopware.slopty.ptyd");
    }

    #[test]
    fn the_first_developer_id_certificate_is_the_one_taken() {
        let listing = concat!(
            "  1) 0C4A \"Apple Development: Someone (VGK9Q8GX84)\"\n",
            "  2) BE54 \"Developer ID Application: A Company (AJ4R8GWM7A)\"\n",
            "  3) C0DE \"Developer ID Application: Another (4TCGAUU87K)\"\n",
            "     3 valid identities found\n",
        );
        assert_eq!(
            pick_identity(listing),
            Some("Developer ID Application: A Company (AJ4R8GWM7A)")
        );
    }

    #[test]
    fn an_apple_development_certificate_is_not_taken() {
        let listing = "  1) 0C4A \"Apple Development: Someone (VGK9Q8GX84)\"\n  1 found\n";
        assert_eq!(pick_identity(listing), None);
    }

    #[test]
    fn an_empty_keychain_names_no_identity() {
        assert_eq!(pick_identity("     0 valid identities found\n"), None);
    }
}
