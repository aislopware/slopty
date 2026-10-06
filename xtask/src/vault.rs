//! The release's Developer ID and notary key, from the Better Update vault.
//!
//! Neither the certificate nor the App Store Connect key lives in CI or in this checkout: the
//! vault of the organisation's Better Update holds both, bound to the Slopty project, and
//! `better-update` reaches it as the person signed in on this Mac (`better-update login`, the
//! vault unlocked) or as the project's CI robot (`BETTER_UPDATE_ROBOT`, the release job's only
//! secret). The certificate is fetched for one build into a keychain of its own, which `codesign`
//! is pointed at and which is deleted when the build is done; the notary key never leaves the
//! vault, since `better-update macos notarize` submits with it (`docs/decisions/tooling.md`,
//! "A release signs and notarises from the Better Update vault").

use std::io::Read as _;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use crate::tools::step;

/// The Slopty project on Better Update, which the credentials are bound to.
const PROJECT_ID: &str = "b435f9a9-bb53-4464-ac42-8da9d8e982e5";
/// The vault's Developer ID Application certificate (team `UK58J62H8L`).
const DEVELOPER_ID: &str = "e9752244-44cc-4a8a-bfbb-8206b66046bc";
/// The vault's App Store Connect API key of the same team (Apple's key id `L2HX58MWYR`),
/// which notarises.
const NOTARY_KEY: &str = "6a08f315-e27f-4a77-a467-b6ece092527b";
/// The program.
const PROGRAM: &str = "better-update";

/// The Developer ID, unlocked in a keychain of its own until this is dropped.
#[derive(Debug)]
pub struct Keychain {
    /// The keychain file, which `codesign --keychain` reads the identity from.
    pub path: Utf8PathBuf,
    /// The identity's SHA-1, unambiguous where the person's own keychain holds it too.
    pub identity: String,
}

impl Drop for Keychain {
    fn drop(&mut self) {
        let deleted =
            std::process::Command::new("security").arg("delete-keychain").arg(&self.path).status();
        if !deleted.is_ok_and(|s| s.success()) {
            eprintln!("  ! the signing keychain {} stayed; delete it by hand", self.path);
        }
    }
}

/// Whether `better-update` is on `PATH`.
pub fn available(sh: &Shell) -> bool {
    cmd!(sh, "{PROGRAM} --version").quiet().ignore_stdout().ignore_stderr().run().is_ok()
}

/// Fetch the Developer ID into a new keychain under `dir`, unlocked for `codesign` alone.
///
/// # Errors
///
/// When `better-update` cannot reach the vault (not signed in, the vault locked, no robot) or
/// the keychain cannot be made.
pub fn developer_id(sh: &Shell, dir: &Utf8Path) -> Result<Keychain> {
    sh.create_dir(dir)?;
    let p12 = dir.join("developer-id.p12");
    let answer = cmd!(
        sh,
        "{PROGRAM} credentials download {DEVELOPER_ID} --type macos-certificate --output {p12} --json --non-interactive"
    )
    .env("BETTER_UPDATE_PROJECT_ID", PROJECT_ID)
    .quiet()
    .ignore_stderr()
    .read()
    .context("better-update credentials download (signed in, the vault unlocked?)")?;
    let fetched = p12_password(&answer);
    let made = fetched.and_then(|password| import(sh, dir, &p12, &password));
    // The certificate's file goes whatever happened: the keychain holds it now, or nothing does.
    if p12.exists() {
        sh.remove_path(&p12)?;
    }
    made
}

/// The `.p12`'s password in `better-update credentials download --json`'s answer.
fn p12_password(answer: &str) -> Result<String> {
    let doc: serde_json::Value = serde_json::from_str(answer).context("the download's answer")?;
    doc.pointer("/data/p12Password")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .context("the download named no .p12 password")
}

/// `p12` imported into a fresh keychain under `dir`, readable by `codesign` without a prompt.
fn import(sh: &Shell, dir: &Utf8Path, p12: &Utf8Path, password: &str) -> Result<Keychain> {
    let path = dir.join("signing.keychain-db");
    if path.exists() {
        let _stale = cmd!(sh, "security delete-keychain {path}").quiet().ignore_stderr().run();
    }
    let secret = random_hex()?;
    cmd!(sh, "security create-keychain -p {secret} {path}").quiet().run()?;
    // From here the keychain is deleted on any failure, by the guard's drop.
    let mut keychain = Keychain { path, identity: String::new() };
    let path = &keychain.path;
    cmd!(sh, "security set-keychain-settings -lut 21600 {path}").quiet().run()?;
    cmd!(sh, "security unlock-keychain -p {secret} {path}").quiet().run()?;
    cmd!(sh, "security import {p12} -k {path} -f pkcs12 -P {password} -T /usr/bin/codesign")
        .quiet()
        .ignore_stdout()
        .run()
        .context("security import")?;
    cmd!(sh, "security set-key-partition-list -S apple-tool:,apple: -s -k {secret} {path}")
        .quiet()
        .ignore_stdout()
        .run()
        .context("security set-key-partition-list")?;
    // `codesign --keychain` finds the identity there alone, but builds its chain from the search
    // list, so the keychain joins it, ahead of the rest, until `delete-keychain` takes it out.
    let listed = cmd!(sh, "security list-keychains -d user").quiet().read()?;
    let search = std::iter::once(path.to_string()).chain(keychains(&listed)).collect::<Vec<_>>();
    cmd!(sh, "security list-keychains -d user -s {search...}").quiet().run()?;
    let listing = cmd!(sh, "security find-identity -v -p codesigning {path}").quiet().read()?;
    identity_hash(&listing)
        .context("the vault's certificate is no Developer ID Application identity")?
        .clone_into(&mut keychain.identity);
    println!("✔ the Developer ID from the vault, in a keychain of this build's own");
    Ok(keychain)
}

/// The SHA-1 of the Developer ID Application identity in `security find-identity`'s `listing`.
fn identity_hash(listing: &str) -> Option<&str> {
    listing
        .lines()
        .find(|line| line.contains("\"Developer ID Application: "))
        .and_then(|line| line.split_whitespace().nth(1).filter(|hash| hash.len() == 40))
}

/// The keychains in `security list-keychains`' `listing`, one quoted path a line.
fn keychains(listing: &str) -> impl Iterator<Item = String> + '_ {
    listing.lines().map(|line| line.trim().trim_matches('"').to_owned()).filter(|k| !k.is_empty())
}

/// Sixteen random bytes in hex: a password for a keychain nothing else opens.
fn random_hex() -> Result<String> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        use std::fmt::Write as _;
        let _infallible = write!(hex, "{byte:02x}");
        hex
    }))
}

/// Submit `app` to the notary service with the vault's key, wait for the verdict, and staple
/// the ticket to it.
///
/// # Errors
///
/// When the notary refuses it or `better-update` cannot reach the vault.
pub fn notarise(sh: &Shell, app: &Utf8Path) -> Result<()> {
    step(
        "better-update macos notarize --wait --staple",
        &cmd!(
            sh,
            "{PROGRAM} macos notarize {app} --asc-key-id {NOTARY_KEY} --wait --staple --non-interactive"
        )
        .env("BETTER_UPDATE_PROJECT_ID", PROJECT_ID),
    )
}

/// Why the vault cannot sign this build, when it cannot: no `better-update` on `PATH`.
pub fn unavailable(sh: &Shell) -> Option<String> {
    (!available(sh)).then(|| {
        format!(
            "no `{PROGRAM}` on PATH: install it (`curl -fsSL \
             https://raw.githubusercontent.com/aislopware/better-update/main/install.sh | sh`)"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity is the Developer ID Application line's hash, among others and the count.
    #[test]
    fn the_identity_is_the_developer_id_line_s_hash() {
        let listing = "  1) 0123456789ABCDEF0123456789ABCDEF01234567 \"Apple Development: A (X1)\"\n  \
                       2) FEDCBA9876543210FEDCBA9876543210FEDCBA98 \"Developer ID Application: A (UK58J62H8L)\"\n     \
                       2 valid identities found\n";
        assert_eq!(identity_hash(listing), Some("FEDCBA9876543210FEDCBA9876543210FEDCBA98"));
        assert_eq!(identity_hash("     0 valid identities found\n"), None);
    }

    /// The search list is read back path by path, quotes and indent gone.
    #[test]
    fn the_search_list_is_read_path_by_path() {
        let listing =
            "    \"/Users/a/Library/Keychains/login.keychain-db\"\n    \"/t/x.keychain-db\"\n";
        assert_eq!(
            keychains(listing).collect::<Vec<_>>(),
            ["/Users/a/Library/Keychains/login.keychain-db", "/t/x.keychain-db"]
        );
    }

    /// The password is read from the download's answer, and an answer without one says so.
    #[test]
    fn the_p12_password_is_read_from_the_download_s_answer() {
        let answer = r#"{"ok":true,"data":{"path":"/t/d.p12","p12Password":"pw"}}"#;
        assert_eq!(p12_password(answer).unwrap(), "pw");
        p12_password(r#"{"ok":true,"data":{}}"#).unwrap_err();
    }

    /// The vault's Developer ID signs a binary with the hardened runtime and a secure timestamp,
    /// and the vault's key has the notary accept it: the release's whole chain, short of a
    /// release. `cargo test -p xtask --lib -- --ignored the_vault_signs`, signed in to
    /// Better Update with the vault unlocked, or with `BETTER_UPDATE_ROBOT`.
    #[test]
    #[ignore = "live: the Better Update vault and Apple's notary"]
    fn the_vault_signs_and_the_notary_accepts() {
        let sh = Shell::new().unwrap();
        let dir = crate::tools::repo_root().unwrap().join("target").join("vault-check");
        if dir.exists() {
            sh.remove_path(&dir).unwrap();
        }
        let keychain = developer_id(&sh, &dir.join("keychain")).unwrap();
        let probe = dir.join("probe");
        sh.copy_file("/usr/bin/true", &probe).unwrap();
        let (path, identity) = (&keychain.path, &keychain.identity);
        cmd!(sh, "codesign --force --options runtime --timestamp --keychain {path} --identifier dev.aislopware.slopty.probe --sign {identity} {probe}").run().unwrap();
        cmd!(sh, "codesign --verify --strict --verbose=2 {probe}").run().unwrap();
        let zip = dir.join("probe.zip");
        cmd!(sh, "ditto -c -k --keepParent {probe} {zip}").run().unwrap();
        notarise(&sh, &zip).unwrap();
    }
}
