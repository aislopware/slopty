//! `xtask dist`: everything a release publishes, built, signed, notarised when it can be, and
//! checked.
//!
//! - `Slopty-<v>-macos-arm64.zip`: the app bundle ([`crate::bundle`]), which carries the daemons,
//!   the server and the CLI for the Mac, and the Linux workers it installs over SSH.
//! - `slopty-<v>-macos-arm64.tar.gz`: the CLI, both worker daemons and the server, signed as they
//!   are in the bundle, for a Mac that runs them headless.
//! - `slopty-worker-<v>-linux-<cpu>.tar.gz`: the Linux worker (`slopty-ptyd`, `slopty-worker`,
//!   `slopty`), glibc [`crate::linux::GLIBC`] and later, for arm64 and `x86_64`.
//! - `slopty-server-<v>-linux-<cpu>.tar.gz`: the server, static on musl, for both.
//! - `slopty-<v>-dSYMs.tar.gz`: the Mac binaries' dSYMs, under their UUIDs.
//! - `SHA256SUMS` over all of them.
//!
//! Notarisation runs when there is an identity to sign with and credentials for `notarytool`
//! ([`Notary::from_env`]); otherwise it is skipped with a note of why, and Gatekeeper then asks
//! the person to confirm the first open. Publishing is not this command's: the release job of
//! CI uploads what it leaves in `--out` for a tag.

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
    /// Code-signing identity; default `$SLOPTY_SIGN_IDENTITY`, else the keychain's Developer ID
    /// Application certificate, else ad hoc.
    #[arg(long, conflicts_with = "ad_hoc")]
    sign: Option<String>,
    /// Sign ad hoc, which also means no notarisation.
    #[arg(long)]
    ad_hoc: bool,
    /// Do not notarise, even with credentials at hand.
    #[arg(long)]
    no_notarize: bool,
}

/// How `notarytool` signs in: a keychain profile (`notarytool store-credentials`), or an App
/// Store Connect API key, which is what CI holds.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Notary {
    /// `--keychain-profile <name>`, from `SLOPTY_NOTARY_PROFILE`.
    Profile(String),
    /// `--key <p8> --key-id <id> --issuer <uuid>`, from `APPLE_API_KEY_PATH`,
    /// `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`.
    ApiKey {
        /// The `.p8` file.
        key: String,
        /// Its id.
        id: String,
        /// The issuer.
        issuer: String,
    },
}

/// The environment variable naming a `notarytool` keychain profile.
const PROFILE_ENV: &str = "SLOPTY_NOTARY_PROFILE";
/// The environment variables of an App Store Connect API key.
const KEY_ENV: [&str; 3] = ["APPLE_API_KEY_PATH", "APPLE_API_KEY_ID", "APPLE_API_ISSUER"];

impl Notary {
    /// The credentials `var` holds, if any: a keychain profile first, then an API key whose
    /// three parts are all set.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let set = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        if let Some(profile) = set(PROFILE_ENV) {
            return Some(Self::Profile(profile));
        }
        let [key, id, issuer] = KEY_ENV.map(set);
        Some(Self::ApiKey { key: key?, id: id?, issuer: issuer? })
    }

    /// `notarytool`'s arguments that sign in.
    fn args(&self) -> Vec<String> {
        match self {
            Self::Profile(name) => vec!["--keychain-profile".to_owned(), name.clone()],
            Self::ApiKey { key, id, issuer } => {
                ["--key", key, "--key-id", id, "--issuer", issuer].map(str::to_owned).into()
            }
        }
    }
}

/// Why a build was or was not notarised.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Notarised {
    /// Notarised and stapled.
    Done,
    /// Skipped, for this reason.
    Skipped(String),
}

/// Whether to notarise a bundle signed so, with these credentials: only a real identity can be,
/// and only with credentials; the reason when not.
pub fn plan_notary(
    signing: &Signing,
    notary: Option<&Notary>,
    refused: bool,
) -> Result<(), String> {
    if refused {
        return Err("--no-notarize".to_owned());
    }
    if *signing == Signing::AdHoc {
        return Err("an ad hoc signature cannot be notarised".to_owned());
    }
    if notary.is_none() {
        return Err(format!(
            "no notary credentials: set {PROFILE_ENV} (a `notarytool store-credentials` profile) \
             or {}",
            KEY_ENV.join(", ")
        ));
    }
    Ok(())
}

pub fn run(sh: &Shell, opts: &DistOpts) -> Result<()> {
    preflight(sh)?;
    let root = repo_root()?;
    let version = crate::release::current_version(sh)?;
    let out = opts.out.clone().unwrap_or_else(|| root.join("target").join("dist-out"));
    if out.exists() {
        sh.remove_path(&out)?;
    }
    sh.create_dir(&out)?;
    let bundle_opts = BundleOpts {
        debug: false,
        sign: opts.sign.clone(),
        ad_hoc: opts.ad_hoc,
        no_linux: false,
        out: Some(root.join("target").join("dist-bundle")),
    };
    let built = bundle::run(sh, &bundle_opts)?;
    let notary = Notary::from_env(|name| std::env::var(name).ok());
    let notarised = match plan_notary(&built.signing, notary.as_ref(), opts.no_notarize) {
        Ok(()) => {
            let notary = notary.context("notary credentials")?;
            notarise(sh, &built.app, &notary, &out)?;
            Notarised::Done
        }
        Err(why) => Notarised::Skipped(why),
    };

    let mac = |name: &str| format!("{name}-{version}-macos-arm64");
    let zip = out.join(format!("{}.zip", mac(PRODUCT)));
    let app = &built.app;
    step("zip the app", &cmd!(sh, "ditto -c -k --sequesterRsrc --keepParent {app} {zip}"))?;
    let macos = app.join("Contents").join("MacOS");
    let helpers = &bundle::BINARIES[1..];
    tar(sh, &macos, helpers, &out.join(format!("{}.tar.gz", mac("slopty"))))?;
    for build in &built.linux {
        let name = build.shipped.name;
        let workers = out.join(format!("slopty-worker-{version}-{name}.tar.gz"));
        tar(sh, &build.workers, &linux::WORKER_BINARIES, &workers)?;
        let server = out.join(format!("slopty-server-{version}-{name}.tar.gz"));
        tar(sh, &build.server, &["slopty-server"], &server)?;
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
        Signing::Identity(identity) => println!("  signed by {identity}"),
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

/// Submit the app to the notary service, wait for its verdict, and staple the ticket.
fn notarise(sh: &Shell, app: &Utf8Path, notary: &Notary, out: &Utf8Path) -> Result<()> {
    let upload = out.join("notarize.zip");
    step("zip for the notary", &cmd!(sh, "ditto -c -k --keepParent {app} {upload}"))?;
    let creds = notary.args();
    step(
        "notarytool submit --wait",
        &cmd!(sh, "xcrun notarytool submit {upload} {creds...} --wait"),
    )?;
    sh.remove_path(&upload)?;
    step("staple", &cmd!(sh, "xcrun stapler staple {app}"))?;
    step("gatekeeper", &cmd!(sh, "spctl --assess --type execute --verbose {app}"))?;
    Ok(())
}

/// `names` from `dir` into the gzipped tarball `archive`, at its top level.
fn tar(sh: &Shell, dir: &Utf8Path, names: &[&str], archive: &Utf8Path) -> Result<()> {
    let file = archive.file_name().unwrap_or_default();
    step(&format!("archive {file}"), &cmd!(sh, "tar -C {dir} -czf {archive} {names...}"))
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

/// What each archive must hold, read back from it: the app's signature, every Mac binary
/// arm64, every Linux one for its CPU and, for the glibc workers, no symbol newer than
/// [`linux::GLIBC`].
fn verify(sh: &Shell, built: &bundle::Bundle, out: &Utf8Path) -> Result<()> {
    println!("▶ verify the archives");
    let app = &built.app;
    cmd!(sh, "codesign --verify --strict --deep {app}").quiet().run()?;
    let macos = app.join("Contents").join("MacOS");
    for bin in bundle::BINARIES {
        let path = macos.join(bin);
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

    /// Notarisation runs only for a real identity with credentials, and says why not otherwise.
    #[test]
    fn notarisation_needs_an_identity_and_credentials() {
        let profile = Notary::Profile("slopty".to_owned());
        let id = Signing::Identity("Developer ID Application: A (AJ4R8GWM7A)".to_owned());
        assert_eq!(plan_notary(&id, Some(&profile), false), Ok(()));
        assert!(plan_notary(&id, None, false).unwrap_err().contains(PROFILE_ENV));
        assert!(
            plan_notary(&Signing::AdHoc, Some(&profile), false).unwrap_err().contains("ad hoc")
        );
        assert_eq!(plan_notary(&id, Some(&profile), true), Err("--no-notarize".to_owned()));
    }

    /// An API key's three parts, as the environment would hold them.
    static KEY: [(&str, &str); 3] =
        [("APPLE_API_KEY_PATH", "/k.p8"), ("APPLE_API_KEY_ID", "K"), ("APPLE_API_ISSUER", "I")];

    /// A keychain profile wins; an API key needs all three of its parts.
    #[test]
    fn credentials_come_from_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned())
        };
        assert_eq!(Notary::from_env(env(&[])), None);
        assert_eq!(
            Notary::from_env(env(&[(PROFILE_ENV, "slopty"), ("APPLE_API_KEY_ID", "K")])),
            Some(Notary::Profile("slopty".to_owned()))
        );
        let key = &KEY;
        let notary = Notary::from_env(env(key)).unwrap();
        assert_eq!(notary.args().join(" "), "--key /k.p8 --key-id K --issuer I");
        assert_eq!(Notary::from_env(env(&key[..2])), None, "a key with no issuer");
        assert_eq!(Notary::from_env(env(&[(PROFILE_ENV, " ")])), None, "blank is unset");
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
