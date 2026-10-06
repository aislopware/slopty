//! `xtask dist`: everything a release publishes, built, signed, notarised when it can be, and
//! checked.
//!
//! - `Slopty-<v>-macos-arm64.zip`: the app bundle ([`crate::bundle`]), which carries the daemons,
//!   the server and the CLI for the Mac, and the Linux workers it installs over SSH.
//! - `slopty-<v>-macos-arm64.tar.gz`: the CLI, both worker daemons and the server, signed as they
//!   are in the bundle, for a Mac that runs them headless.
//! - `slopty-worker-<v>-linux-<cpu>.tar.gz`: the Linux worker (`slopty-ptyd`, `slopty-worker`,
//!   `slopty`), glibc [`crate::linux::GLIBC`] and later, for arm64 and `x86_64`, with the static
//!   `slopty-server` beside them, so `slopty server install` there needs no `--bin-dir`.
//! - `slopty-server-<v>-linux-<cpu>.tar.gz`: the server, static on musl, for both.
//! - `slopty-<v>-dSYMs.tar.gz`: the Mac binaries' dSYMs, under their UUIDs.
//! - `SHA256SUMS` over all of them.
//!
//! It signs with the Developer ID and notarises with the App Store Connect key that the Better
//! Update vault holds ([`crate::vault`]), whenever `better-update` can reach it: as the person
//! signed in here, or as the CI robot. A tag's build must; any other is signed with the
//! keychain's identity or ad hoc and left unnotarised when the vault is out of reach, with a
//! note of why, and Gatekeeper then asks the person to confirm the first open. Publishing is
//! not this command's: the release job of CI uploads what it leaves in `--out` for a tag.

use std::io::Read as _;

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use xshell::{Shell, cmd};

use crate::bundle::{self, BundleOpts, PRODUCT, Signing};
use crate::linux;
use crate::tools::{repo_root, step};

/// `xtask dist` options.
#[derive(Args, Debug, Clone)]
pub struct DistOpts {
    /// Where the archives go (default: `target/dist-out`).
    #[arg(long)]
    out: Option<Utf8PathBuf>,
    /// Code-signing identity in the person's keychain, in place of the vault's; default
    /// `$SLOPTY_SIGN_IDENTITY`, else the keychain's Developer ID Application certificate, else
    /// ad hoc. Such a build is not notarised.
    #[arg(long, conflicts_with = "ad_hoc")]
    sign: Option<String>,
    /// Sign ad hoc, which also means no notarisation.
    #[arg(long)]
    ad_hoc: bool,
    /// Do not notarise, even with the vault at hand.
    #[arg(long)]
    no_notarize: bool,
}

/// Why a build was or was not notarised.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Notarised {
    /// Notarised and stapled.
    Done,
    /// Skipped, for this reason.
    Skipped(String),
}

/// Whether a build may be published: one for a tag (`GITHUB_REF_TYPE=tag`, the release job's)
/// must be signed with a Developer ID and notarised, since an ad hoc app asks again for every
/// grant on each update and Gatekeeper stops its first open. A build that is not for a tag may
/// be either, as a local `dist` is.
///
/// # Errors
///
/// For a tag, what it lacks.
pub fn publishable(tag: bool, signing: &Signing, notarised: &Notarised) -> Result<()> {
    if !tag {
        return Ok(());
    }
    ensure!(
        matches!(signing, Signing::Identity { keychain: Some(_), .. }),
        "a release is signed with the vault's Developer ID: install better-update and set \
         BETTER_UPDATE_ROBOT, or sign in here with the vault unlocked"
    );
    if let Notarised::Skipped(why) = notarised {
        bail!("a release is notarised, and this one was not: {why}");
    }
    Ok(())
}

/// Whether to notarise a bundle signed so: only one signed from the vault is, whose key the
/// notary is reached with; the reason when not.
pub fn plan_notary(signing: &Signing, refused: bool) -> Result<(), String> {
    if refused {
        return Err("--no-notarize".to_owned());
    }
    match signing {
        Signing::AdHoc => Err("an ad hoc signature cannot be notarised".to_owned()),
        Signing::Identity { keychain: None, .. } => {
            Err("signed from the keychain, not the vault, whose key notarises".to_owned())
        }
        Signing::Identity { keychain: Some(_), .. } => Ok(()),
    }
}

/// The vault's Developer ID in a keychain of this build's own, unless the build signs otherwise
/// (`--sign`, `--ad-hoc`); `None`, said, when the vault is out of reach of a build that may be
/// unsigned by it.
///
/// # Errors
///
/// For a tag, when the vault is out of reach.
fn vault_identity(
    sh: &Shell,
    opts: &DistOpts,
    tag: bool,
    dir: &Utf8Path,
) -> Result<Option<crate::vault::Keychain>> {
    if opts.ad_hoc || opts.sign.is_some() {
        ensure!(!tag, "a release signs with the vault's Developer ID, not --sign or --ad-hoc");
        return Ok(None);
    }
    let fetched = match crate::vault::unavailable(sh) {
        Some(why) => Err(anyhow::anyhow!(why)),
        None => crate::vault::developer_id(sh, dir),
    };
    match fetched {
        Ok(keychain) => Ok(Some(keychain)),
        Err(why) if tag => Err(why.context("a release signs with the vault's Developer ID")),
        Err(why) => {
            println!("  ! not signing from the vault: {why:#}");
            Ok(None)
        }
    }
}

pub fn run(sh: &Shell, opts: &DistOpts) -> Result<()> {
    preflight(sh)?;
    let tag = std::env::var("GITHUB_REF_TYPE").is_ok_and(|kind| kind == "tag");
    let root = repo_root()?;
    let version = crate::release::current_version(sh)?;
    let out = opts.out.clone().unwrap_or_else(|| root.join("target").join("dist-out"));
    if out.exists() {
        sh.remove_path(&out)?;
    }
    sh.create_dir(&out)?;
    // Before the long build: a tag the vault cannot sign fails now, not after it. The keychain
    // goes when the build is done, whatever happened.
    let keychain = vault_identity(sh, opts, tag, &root.join("target").join("dist-keychain"))?;
    let bundle_opts = BundleOpts {
        debug: false,
        sign: keychain.as_ref().map(|k| k.identity.clone()).or_else(|| opts.sign.clone()),
        ad_hoc: opts.ad_hoc,
        no_linux: false,
        out: Some(root.join("target").join("dist-bundle")),
        keychain: keychain.as_ref().map(|k| k.path.clone()),
    };
    let built = bundle::run(sh, &bundle_opts)?;
    drop(keychain);
    let notarised = match plan_notary(&built.signing, opts.no_notarize) {
        Ok(()) => {
            notarise(sh, &built.app)?;
            Notarised::Done
        }
        Err(why) => Notarised::Skipped(why),
    };
    publishable(tag, &built.signing, &notarised)?;

    let mac = |name: &str| format!("{name}-{version}-macos-arm64");
    let zip = out.join(format!("{}.zip", mac(PRODUCT)));
    let app = &built.app;
    step("zip the app", &cmd!(sh, "ditto -c -k --sequesterRsrc --keepParent {app} {zip}"))?;
    let macos = app.join("Contents").join("MacOS");
    let helpers = &bundle::BINARIES[1..];
    tar(sh, &[(&macos, helpers)], &out.join(format!("{}.tar.gz", mac("slopty"))))?;
    for build in &built.linux {
        let name = build.shipped.name;
        let workers = out.join(format!("slopty-worker-{version}-{name}.tar.gz"));
        let parts = [(&build.workers, &linux::WORKER_BINARIES[..]), (&build.server, SERVER)];
        tar(sh, &parts, &workers)?;
        let server = out.join(format!("slopty-server-{version}-{name}.tar.gz"));
        tar(sh, &[(&build.server, SERVER)], &server)?;
    }
    if let Some(dsyms) = &built.dsyms {
        let archive = out.join(format!("slopty-{version}-dSYMs.tar.gz"));
        step("archive the dSYMs", &cmd!(sh, "tar -C {dsyms} -czf {archive} ."))?;
    }
    verify(sh, &built, &out)?;
    let sums = {
        let _in = sh.push_dir(&out);
        cmd!(sh, "shasum -a 256").args(archives(&out)?).read()?
    };
    sh.write_file(out.join("SHA256SUMS"), format!("{sums}\n"))?;

    println!("✔ {out}");
    for archive in archives(&out)? {
        println!("    {archive}");
    }
    match &built.signing {
        Signing::Identity { name, keychain: Some(_) } => {
            println!("  signed by {name}, the vault's Developer ID");
        }
        Signing::Identity { name, keychain: None } => println!("  signed by {name}"),
        Signing::AdHoc => {
            println!(
                "  ! signed ad hoc: every update asks again for Screen Recording and Accessibility"
            );
        }
    }
    match notarised {
        Notarised::Done => println!("  notarised and stapled"),
        Notarised::Skipped(why) => println!(
            "  ! not notarised ({why}): Gatekeeper asks the person to confirm its first open"
        ),
    }
    Ok(())
}

/// Submit the app to the notary service with the vault's key, wait for its verdict, staple
/// the ticket, and have Gatekeeper judge it.
fn notarise(sh: &Shell, app: &Utf8Path) -> Result<()> {
    crate::vault::notarise(sh, app)?;
    step("gatekeeper", &cmd!(sh, "spctl --assess --type execute --verbose {app}"))?;
    Ok(())
}

/// The server's binary, in its own archive and beside each Linux worker.
const SERVER: &[&str] = &["slopty-server"];

/// Each part's names from its directory into the gzipped tarball `archive`, at its top level.
fn tar(sh: &Shell, parts: &[(&Utf8PathBuf, &[&str])], archive: &Utf8Path) -> Result<()> {
    let file = archive.file_name().unwrap_or_default();
    let args = parts.iter().flat_map(|(dir, names)| {
        ["-C".to_owned(), dir.to_string()].into_iter().chain(names.iter().map(|n| (*n).to_owned()))
    });
    step(&format!("archive {file}"), &cmd!(sh, "tar -czf {archive}").args(args))
}

/// The archives in `out`, by name.
fn archives(out: &Utf8Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = out
        .read_dir_utf8()?
        .filter_map(|entry| entry.ok().map(|e| e.file_name().to_owned()))
        .filter(|name| matches!(Utf8Path::new(name).extension(), Some("zip" | "gz")))
        .collect();
    names.sort();
    Ok(names)
}

/// What each archive must hold, read back from it: the app's signature, every Mac binary (the
/// File Provider extension's too) arm64, every Linux one for its CPU and, for the glibc
/// workers, no symbol newer than [`linux::GLIBC`].
fn verify(sh: &Shell, built: &bundle::Bundle, out: &Utf8Path) -> Result<()> {
    println!("▶ verify the archives");
    let app = &built.app;
    cmd!(sh, "codesign --verify --strict --deep {app}").quiet().run()?;
    let macos = app.join("Contents").join("MacOS");
    let appex = bundle::appex_executable(app);
    for path in bundle::BINARIES.iter().map(|bin| macos.join(bin)).chain([appex]) {
        let archs = cmd!(sh, "lipo -archs {path}").quiet().read()?;
        ensure!(archs.trim() == "arm64", "{path} is built for {archs}");
    }
    ensure!(built.linux.len() == linux::SHIPPED.len(), "a Linux build is missing");
    for build in &built.linux {
        let bundled = app.join("Contents/Resources/workers").join(build.shipped.name);
        for bin in linux::WORKER_BINARIES {
            for dir in [&build.workers, &bundled] {
                let path = dir.join(bin);
                let head = head(&path)?;
                ensure!(
                    elf_cpu(&head) == Some(build.shipped.name),
                    "{path} is not a {} binary",
                    build.shipped.name
                );
                let newest = newest_glibc(&std::fs::read(&path)?);
                ensure!(
                    newest.is_none_or(|v| v <= glibc(linux::GLIBC)),
                    "{path} needs glibc {newest:?}, past {}",
                    linux::GLIBC
                );
            }
        }
        let server = build.server.join("slopty-server");
        ensure!(elf_cpu(&head(&server)?) == Some(build.shipped.name), "{server} is not its CPU's");
        let symbols = std::fs::read(&server)?;
        ensure!(newest_glibc(&symbols).is_none(), "{server} links glibc; it ships static");
    }
    for archive in archives(out)? {
        let path = out.join(&archive);
        let listing = if Utf8Path::new(&archive).extension() == Some("zip") {
            cmd!(sh, "unzip -Z1 {path}").quiet().read()?
        } else {
            cmd!(sh, "tar -tzf {path}").quiet().read()?
        };
        ensure!(!listing.trim().is_empty(), "{archive} is empty");
        if archive.starts_with("slopty-worker-") {
            let names: Vec<&str> = listing.lines().map(|l| l.trim_start_matches("./")).collect();
            for bin in linux::WORKER_BINARIES.iter().chain(SERVER) {
                ensure!(names.contains(bin), "{archive} lacks {bin}");
            }
        }
    }
    println!("  ✓ verify the archives");
    Ok(())
}

/// The first bytes of `path`, where its header is.
fn head(path: &Utf8Path) -> Result<Vec<u8>> {
    let mut head = Vec::with_capacity(64);
    std::fs::File::open(path)
        .and_then(|f| f.take(64).read_to_end(&mut head))
        .with_context(|| format!("read {path}"))?;
    Ok(head)
}

/// The Linux CPU a 64-bit little-endian ELF header names, as [`linux::SHIPPED`] calls it.
fn elf_cpu(head: &[u8]) -> Option<&'static str> {
    if head.get(..6) != Some(&[0x7f, b'E', b'L', b'F', 2, 1]) {
        return None;
    }
    // `EM_AARCH64` and `EM_X86_64` from `<elf.h>`, at `e_machine`.
    match head.get(18..20)? {
        [183, 0] => Some("linux-arm64"),
        [62, 0] => Some("linux-x86_64"),
        _ => None,
    }
}

/// A glibc version as `(major, minor)`.
fn glibc(version: &str) -> (u32, u32) {
    let mut parts = version.split('.').map(|p| p.parse().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// The newest `GLIBC_x.y` symbol version a binary asks for, read from its version strings.
fn newest_glibc(binary: &[u8]) -> Option<(u32, u32)> {
    const TAG: &[u8] = b"GLIBC_";
    let mut newest = None;
    let mut at = 0;
    while let Some(found) = binary.get(at..)?.windows(TAG.len()).position(|w| w == TAG) {
        let start = at.saturating_add(found).saturating_add(TAG.len());
        let digits: Vec<u8> = binary
            .get(start..)?
            .iter()
            .take_while(|b| b.is_ascii_digit() || **b == b'.')
            .copied()
            .collect();
        if let Ok(text) = std::str::from_utf8(&digits)
            && text.contains('.')
        {
            let version = glibc(text);
            newest = newest.max(Some(version));
        }
        at = start;
    }
    newest
}

/// Fail with what is missing when this Mac cannot build a release at all.
fn preflight(sh: &Shell) -> Result<()> {
    for tool in ["codesign", "ditto", "lipo", "shasum", "tar", "unzip"] {
        if !crate::tools::has(sh, tool) {
            bail!("{tool} is needed to build a release");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vault's Developer ID and the person's.
    fn identities() -> (Signing, Signing) {
        let name = "Developer ID Application: A (UK58J62H8L)".to_owned();
        let vault = Signing::Identity { name: name.clone(), keychain: Some("/t/k".into()) };
        (vault, Signing::Identity { name, keychain: None })
    }

    /// Only a build signed from the vault is notarised, whose key reaches the notary; any other
    /// says why not.
    #[test]
    fn only_a_build_signed_from_the_vault_is_notarised() {
        let (vault, own) = identities();
        assert_eq!(plan_notary(&vault, false), Ok(()));
        assert!(plan_notary(&own, false).unwrap_err().contains("vault"));
        assert!(plan_notary(&Signing::AdHoc, false).unwrap_err().contains("ad hoc"));
        assert_eq!(plan_notary(&vault, true), Err("--no-notarize".to_owned()));
    }

    /// A tag's build fails unless the vault's Developer ID signed it and it was notarised; any
    /// other build passes as it is.
    #[test]
    fn a_tag_is_published_only_signed_from_the_vault_and_notarised() {
        let (vault, own) = identities();
        let skipped = Notarised::Skipped("no credentials".to_owned());
        publishable(true, &vault, &Notarised::Done).unwrap();
        let ad_hoc = publishable(true, &Signing::AdHoc, &skipped).unwrap_err().to_string();
        assert!(ad_hoc.contains("vault"), "{ad_hoc}");
        let keychain = publishable(true, &own, &Notarised::Done).unwrap_err().to_string();
        assert!(keychain.contains("vault"), "{keychain}");
        let unnotarised = publishable(true, &vault, &skipped).unwrap_err().to_string();
        assert!(unnotarised.contains("no credentials"), "{unnotarised}");
        publishable(false, &Signing::AdHoc, &skipped).unwrap();
    }

    #[test]
    fn a_binary_says_its_cpu_and_its_newest_glibc() {
        let mut elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0];
        elf.resize(18, 0);
        elf.extend([62, 0]);
        assert_eq!(elf_cpu(&elf), Some("linux-x86_64"));
        assert_eq!(elf_cpu(b"\xcf\xfa\xed\xfe"), None, "Mach-O is no Linux binary");
        let symbols = b"\0GLIBC_2.17\0GLIBC_2.28\0GLIBC_PRIVATE\0GLIBC_2.3.4\0";
        assert_eq!(newest_glibc(symbols), Some((2, 28)));
        assert_eq!(newest_glibc(b"static, no versions"), None);
        assert!(glibc("2.34") > glibc(linux::GLIBC));
    }
}
