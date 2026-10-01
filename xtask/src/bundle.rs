//! `xtask bundle`: the macOS app bundle.
//!
//! `Slopty.app` carries the app and, beside it, the worker daemons, the server and the CLI, so
//! one bundle serves every role: launch it for the workspace, or run
//! `Contents/MacOS/slopty worker install` (or `server install`) to turn the machine into a worker
//! (or the server). `Info.plist` is generated from the workspace version; signing is ad hoc unless
//! `--sign` names a Developer ID identity.
//!
//! A shipping bundle is built with the `dist` profile, whose binaries are stripped of their debug
//! info, and ships lean: the dSYMs go beside the bundle, under `dSYMs/<UUID>/<bin>.dSYM`, for a
//! release to publish on their own. `cargo xtask symbolicate` finds them by a crash report's UUID
//! (`docs/decisions/crashes.md`, "The dSYMs ship apart from the app").

use anyhow::{Result, bail};
use camino::Utf8PathBuf;
use clap::Args;
use xshell::{Shell, cmd};

use crate::tools::{repo_root, step};

/// Bundle identifier of the macOS app.
const BUNDLE_ID: &str = "dev.aislopware.slopty";
/// Product name (the `.app` name and what the Finder shows).
const PRODUCT: &str = "Slopty";
/// Floor, as `LSMinimumSystemVersion`.
const MACOS_VERSION: &str = "26.5";
/// Binaries copied into `Contents/MacOS`, the app first.
const BINARIES: [&str; 5] =
    ["slopty-app", "slopty-worker", "slopty-ptyd", "slopty-server", "slopty"];

/// `xtask bundle` options.
#[derive(Args, Debug, Clone)]
pub struct BundleOpts {
    /// Build with the `dist` profile (default), its dSYMs beside the bundle; `--debug` for a
    /// dev bundle, whose debug info stays in this checkout's object files.
    #[arg(long)]
    debug: bool,
    /// Code-signing identity (`Developer ID Application: …`); ad hoc when omitted.
    #[arg(long)]
    sign: Option<String>,
    /// Output directory (default: `target/bundle`).
    #[arg(long)]
    out: Option<Utf8PathBuf>,
}

pub fn run(sh: &Shell, opts: &BundleOpts) -> Result<Utf8PathBuf> {
    let root = repo_root()?;
    let profile = if opts.debug { "debug" } else { "dist" };
    let flags: &[&str] = if opts.debug { &[] } else { &["--profile", "dist"] };
    step(
        &format!("cargo build ({profile})"),
        &cmd!(
            sh,
            "cargo build {flags...} -p slopty -p slopty-workerd -p slopty-ptyd -p slopty-serverd -p slopty-cli"
        ),
    )?;
    let built = root.join("target").join(profile);
    let out = opts.out.clone().unwrap_or_else(|| root.join("target").join("bundle"));
    let app = out.join(format!("{PRODUCT}.app"));
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    if app.exists() {
        sh.remove_path(&app)?;
    }
    sh.create_dir(&macos)?;
    let resources = contents.join("Resources");
    sh.create_dir(&resources)?;
    for bin in BINARIES {
        let from = built.join(bin);
        if !from.exists() {
            bail!("missing {from}");
        }
        sh.copy_file(&from, macos.join(bin))?;
    }
    if !opts.debug {
        let dsyms = out.join("dSYMs");
        if dsyms.exists() {
            sh.remove_path(&dsyms)?;
        }
        crate::symbolicate::collect(sh, &built, &BINARIES, &dsyms)?;
        println!("✔ {dsyms}");
    }
    let version = crate::release::current_version(sh)?;
    sh.write_file(contents.join("Info.plist"), info_plist(&version))?;
    // The document sits beside the app, not in it: only what actool makes of it ships.
    let icon = crate::icon::Art::load(sh)?.write_document(sh, &out)?;
    crate::icon::compile_macos(sh, &icon, &resources)?;
    sh.write_file(contents.join("PkgInfo"), "APPL????")?;
    let identity = opts.sign.as_deref().unwrap_or("-");
    // Sign the nested binaries first, then the bundle; `--deep` is deprecated for a reason.
    for bin in BINARIES.iter().rev() {
        let path = macos.join(bin);
        step(
            &format!("codesign {bin}"),
            &cmd!(
                sh,
                "codesign --force --options runtime --timestamp=none --sign {identity} {path}"
            ),
        )?;
    }
    step(
        "codesign bundle",
        &cmd!(sh, "codesign --force --options runtime --timestamp=none --sign {identity} {app}"),
    )?;
    step("codesign verify", &cmd!(sh, "codesign --verify --strict {app}"))?;
    println!("✔ {app}");
    Ok(app)
}

/// The app's `Info.plist`.
fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleDisplayName</key>
	<string>{PRODUCT}</string>
	<key>CFBundleExecutable</key>
	<string>slopty-app</string>
	<key>CFBundleIconFile</key>
	<string>{icon}</string>
	<key>CFBundleIconName</key>
	<string>{icon}</string>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{PRODUCT}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key>
	<string>{MACOS_VERSION}</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
	<key>NSLocalNetworkUsageDescription</key>
	<string>Slopty connects to your workers on the local network.</string>
</dict>
</plist>
"#,
        icon = crate::icon::NAME,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_plist_names_the_app_and_version() {
        let plist = info_plist("0.3.1");
        assert!(plist.contains("<string>slopty-app</string>"));
        assert!(plist.contains("<string>0.3.1</string>"));
        assert!(plist.contains(BUNDLE_ID));
    }
}
