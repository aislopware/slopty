//! `xtask bundle`: the macOS app bundle.
//!
//! `Slopty.app` carries the app and, beside it, the worker daemons, the server and the CLI, so
//! one bundle serves every role: launch it for the workspace, or run
//! `Contents/MacOS/slopty worker install` (or `server install`) to turn the machine into a worker
//! (or the server). It also carries the Linux worker and server for both CPUs under
//! `Contents/Resources/workers/<platform>` (`cargo xtask linux dist`), which is what the app
//! sends to a Linux box it sets up over SSH: a worker or a server must be this very build to be
//! let in, so the app ships every build it can install. `Info.plist` is generated from the
//! workspace version.
//!
//! Signing is with a stable identity whenever there is one: `--sign`, else
//! `$SLOPTY_SIGN_IDENTITY`, else the keychain's Developer ID Application certificate. Each
//! daemon is signed under its `LaunchAgent` label as identifier (`dev.aislopware.slopty.worker`,
//! as `cargo xtask sign` signs the dev ones), so its designated requirement is that identifier and
//! the team, not the binary's hash: a Screen Recording or Accessibility grant made once survives
//! every update, here and on each Mac a worker is deployed to. With no identity, or with
//! `--ad-hoc`, it is signed ad hoc and every update asks again (`docs/decisions/tooling.md`,
//! "Bundles are signed with a stable identity").
//!
//! A shipping bundle is built with the `dist` profile, whose binaries are stripped of their debug
//! info, and ships lean: the dSYMs go beside the bundle, under `dSYMs/<UUID>/<bin>.dSYM`, for a
//! release to publish on their own. `cargo xtask symbolicate` finds them by a crash report's UUID
//! (`docs/decisions/crashes.md`, "The dSYMs ship apart from the app").

use std::fmt::Write as _;

use anyhow::{Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use xshell::{Shell, cmd};

use crate::linux;
use crate::tools::{repo_root, step};

/// Bundle identifier of the macOS app.
const BUNDLE_ID: &str = "dev.aislopware.slopty";
/// Product name (the `.app` name and what the Finder shows).
pub const PRODUCT: &str = "Slopty";
/// Floor, as `LSMinimumSystemVersion`.
const MACOS_VERSION: &str = "26.5";
/// Binaries copied into `Contents/MacOS`, the app first.
pub const BINARIES: [&str; 5] =
    ["slopty-app", "slopty-worker", "slopty-ptyd", "slopty-server", "slopty"];
/// Each helper binary's code identifier, after `dev.aislopware.slopty.`: the daemons' are their
/// `LaunchAgent` labels'. The app's is the bundle's own.
const IDENTIFIERS: [(&str, &str); 4] = [
    ("slopty-worker", "worker"),
    ("slopty-ptyd", "ptyd"),
    ("slopty-server", "server"),
    ("slopty", "cli"),
];
/// Where the other platforms' workers go, under `Contents/Resources`
/// (`slopty_deploy::BUNDLED_DIR`, which the app reads them from).
const WORKERS_DIR: &str = "workers";
/// The File Provider extension (`apps/slopty-files`): its bundle under `Contents/PlugIns`, its
/// executable, and its identifier after `dev.aislopware.slopty.`.
const FILES_APPEX: &str = "SloptyFiles.appex";
const FILES_BIN: &str = "slopty-files";
const FILES_ID: &str = "files";
/// The app group the app and the extension share, prefixed with the signing team, which needs
/// no provisioning profile under a Developer ID (`slopty_platform::files::GROUP`).
const GROUP: &str = "AJ4R8GWM7A.dev.aislopware.slopty";

/// `xtask bundle` options.
#[derive(Args, Debug, Clone, Default)]
pub struct BundleOpts {
    /// Build with the `dist` profile (default), its dSYMs beside the bundle; `--debug` for a
    /// dev bundle, whose debug info stays in this checkout's object files.
    #[arg(long)]
    pub debug: bool,
    /// Code-signing identity (`Developer ID Application: …`); default `$SLOPTY_SIGN_IDENTITY`,
    /// else the keychain's Developer ID Application certificate, else ad hoc.
    #[arg(long, conflicts_with = "ad_hoc")]
    pub sign: Option<String>,
    /// Sign ad hoc even when a Developer ID is at hand: every update then asks again for
    /// Screen Recording and Accessibility.
    #[arg(long)]
    pub ad_hoc: bool,
    /// Leave the Linux workers out (no zig needed): such a bundle installs no Linux worker.
    #[arg(long)]
    pub no_linux: bool,
    /// Output directory (default: `target/bundle`).
    #[arg(long)]
    pub out: Option<Utf8PathBuf>,
}

/// How a bundle is signed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Signing {
    /// With this identity: grants survive updates, and it can be notarised.
    Identity(String),
    /// Ad hoc: every update is a new program to TCC.
    AdHoc,
}

impl Signing {
    /// `codesign`'s arguments for `path` under `identifier` (`None`: the bundle's own). A real
    /// identity takes a secure timestamp, which notarisation requires; ad hoc takes none.
    fn codesign(&self, identifier: Option<&str>, path: &Utf8Path) -> Vec<String> {
        self.codesign_with(identifier, None, path)
    }

    /// [`Self::codesign`], with the entitlements in the file `entitlements`.
    fn codesign_with(
        &self,
        identifier: Option<&str>,
        entitlements: Option<&Utf8Path>,
        path: &Utf8Path,
    ) -> Vec<String> {
        let mut args: Vec<String> = ["--force", "--options", "runtime"].map(str::to_owned).into();
        if let Some(entitlements) = entitlements {
            args.extend(["--entitlements".to_owned(), entitlements.to_string()]);
        }
        let identity = match self {
            Self::Identity(identity) => {
                args.push("--timestamp".to_owned());
                identity.as_str()
            }
            Self::AdHoc => {
                args.push("--timestamp=none".to_owned());
                "-"
            }
        };
        if let Some(identifier) = identifier {
            args.extend(["--identifier".to_owned(), identifier.to_owned()]);
        }
        args.extend(["--sign".to_owned(), identity.to_owned(), path.to_string()]);
        args
    }
}

/// The signing `opts` ask for: ad hoc when told, else the identity named or found; ad hoc, said
/// loudly, when none is at hand.
pub fn signing(sh: &Shell, opts: &BundleOpts) -> Signing {
    if opts.ad_hoc {
        return Signing::AdHoc;
    }
    match crate::sign::resolve_identity(sh, opts.sign.as_deref()) {
        Ok(identity) => Signing::Identity(identity),
        Err(why) => {
            println!("  ! signing ad hoc: {why}");
            println!(
                "    every update of a Mac then asks again for Screen Recording and Accessibility"
            );
            Signing::AdHoc
        }
    }
}

/// What a bundle build made.
#[derive(Debug)]
pub struct Bundle {
    /// `Slopty.app`.
    pub app: Utf8PathBuf,
    /// How it is signed.
    pub signing: Signing,
    /// The Linux builds inside it, with the servers built beside them.
    pub linux: Vec<linux::Built>,
    /// The dSYMs, for a `dist` build.
    pub dsyms: Option<Utf8PathBuf>,
}

pub fn run(sh: &Shell, opts: &BundleOpts) -> Result<Bundle> {
    let root = repo_root()?;
    let profile = if opts.debug { "debug" } else { "dist" };
    let flags: &[&str] = if opts.debug { &[] } else { &["--profile", "dist"] };
    let signing = signing(sh, opts);
    step(
        &format!("cargo build ({profile})"),
        &cmd!(
            sh,
            "cargo build {flags...} -p slopty -p slopty-workerd -p slopty-ptyd -p slopty-serverd -p slopty-cli -p slopty-files"
        ),
    )?;
    let linux = if opts.no_linux {
        Vec::new()
    } else {
        linux::build_shipped(sh, if opts.debug { "dev" } else { "dist" })?
    };
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
    let appex = Appex::under(&contents);
    if let Some(dir) = appex.executable.parent() {
        sh.create_dir(dir)?;
    }
    sh.copy_file(built.join(FILES_BIN), &appex.executable)?;
    for build in &linux {
        let dir = resources.join(WORKERS_DIR).join(build.shipped.name);
        sh.create_dir(&dir)?;
        for bin in linux::WORKER_BINARIES {
            sh.copy_file(build.workers.join(bin), dir.join(bin))?;
        }
        sh.copy_file(build.server.join("slopty-server"), dir.join("slopty-server"))?;
    }
    let dsyms = if opts.debug {
        None
    } else {
        let dsyms = out.join("dSYMs");
        if dsyms.exists() {
            sh.remove_path(&dsyms)?;
        }
        crate::symbolicate::collect(sh, &built, &BINARIES, &dsyms)?;
        crate::symbolicate::collect(sh, &built, &[FILES_BIN], &dsyms)?;
        println!("✔ {dsyms}");
        Some(dsyms)
    };
    let version = crate::release::current_version(sh)?;
    sh.write_file(contents.join("Info.plist"), info_plist(&version))?;
    // The document sits beside the app, not in it: only what actool makes of it ships.
    let icon = crate::icon::Art::load(sh)?.write_document(sh, &out)?;
    crate::icon::compile_macos(sh, &icon, &resources)?;
    sh.write_file(contents.join("PkgInfo"), "APPL????")?;
    sh.write_file(&appex.info_plist, files_info_plist(&version))?;
    // Beside the app, as the icon's document is: only what codesign seals of them ships.
    let entitlements = out.join("entitlements");
    let (app_rights, files_rights) =
        (entitlements.join("app.plist"), entitlements.join("files.plist"));
    sh.write_file(&app_rights, app_entitlements())?;
    sh.write_file(&files_rights, files_entitlements())?;
    // The helpers first, each under its identifier, then the extension, then the bundle, which
    // signs the app's own binary as it; `--deep` is deprecated for a reason.
    for (bin, suffix) in IDENTIFIERS {
        let id = crate::sign::identifier(suffix);
        let args = signing.codesign(Some(&id), &macos.join(bin));
        step(&format!("codesign {bin} as {id}"), &cmd!(sh, "codesign {args...}"))?;
    }
    let id = crate::sign::identifier(FILES_ID);
    let args = signing.codesign_with(Some(&id), Some(&files_rights), &appex.bundle);
    step(&format!("codesign {FILES_APPEX} as {id}"), &cmd!(sh, "codesign {args...}"))?;
    let args = signing.codesign_with(None, Some(&app_rights), &app);
    step("codesign bundle", &cmd!(sh, "codesign {args...}"))?;
    step("codesign verify", &cmd!(sh, "codesign --verify --strict --deep {app}"))?;
    if let Signing::Identity(identity) = &signing {
        let worker = macos.join("slopty-worker");
        let requirement = cmd!(sh, "codesign --display --requirements - {worker}")
            .quiet()
            .ignore_stderr()
            .read()?;
        ensure!(
            stable(&requirement, &crate::sign::identifier("worker")),
            "{worker} is not held to its identifier and team: {requirement}"
        );
        println!("✔ signed by {identity}: a grant made once survives every update");
    }
    println!("✔ {app}");
    Ok(Bundle { app, signing, linux, dsyms })
}

/// Where the File Provider extension sits in the app's `Contents`.
struct Appex {
    /// `PlugIns/SloptyFiles.appex`, which the system finds the extension in.
    bundle: Utf8PathBuf,
    /// Its executable, the one its `Info.plist` names.
    executable: Utf8PathBuf,
    /// Its `Info.plist`.
    info_plist: Utf8PathBuf,
}

impl Appex {
    fn under(contents: &Utf8Path) -> Self {
        let bundle = contents.join("PlugIns").join(FILES_APPEX);
        let inner = bundle.join("Contents");
        Self {
            executable: inner.join("MacOS").join(FILES_BIN),
            info_plist: inner.join("Info.plist"),
            bundle,
        }
    }
}

/// Whether a designated requirement holds a binary to `identifier` and a certificate's team,
/// rather than to its hash: what lets a TCC grant outlive an update.
fn stable(requirement: &str, identifier: &str) -> bool {
    requirement.contains(&format!("identifier \"{identifier}\""))
        && requirement.contains("certificate leaf[subject.OU]")
        && !requirement.contains("cdhash")
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

/// The File Provider extension's `Info.plist`: a replicated extension whose principal class
/// the binary registers before it hands the process to the system.
fn files_info_plist(version: &str) -> String {
    let id = crate::sign::identifier(FILES_ID);
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
	<string>{FILES_BIN}</string>
	<key>CFBundleIdentifier</key>
	<string>{id}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{PRODUCT}</string>
	<key>CFBundlePackageType</key>
	<string>XPC!</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSMinimumSystemVersion</key>
	<string>{MACOS_VERSION}</string>
	<key>NSExtension</key>
	<dict>
		<key>NSExtensionFileProviderDocumentGroup</key>
		<string>{GROUP}</string>
		<key>NSExtensionFileProviderSupportsEnumeration</key>
		<true/>
		<key>NSExtensionPointIdentifier</key>
		<string>com.apple.fileprovider-nonui</string>
		<key>NSExtensionPrincipalClass</key>
		<string>SloptyFilesExtension</string>
	</dict>
</dict>
</plist>
"#
    )
}

/// The app's entitlements: the group it shares with its extension.
fn app_entitlements() -> String {
    entitlements(&[])
}

/// The extension's: the sandbox every File Provider extension runs in, the shared group, and
/// the network both ways, since QUIC over UDP binds a socket of its own.
fn files_entitlements() -> String {
    entitlements(&[
        "com.apple.security.app-sandbox",
        "com.apple.security.network.client",
        "com.apple.security.network.server",
    ])
}

/// An entitlements file with the shared group and each of `switches` on.
fn entitlements(switches: &[&str]) -> String {
    let on = switches.iter().fold(String::new(), |mut on, key| {
        let _infallible = writeln!(on, "\t<key>{key}</key>\n\t<true/>");
        on
    });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
{on}	<key>com.apple.security.application-groups</key>
	<array>
		<string>{GROUP}</string>
	</array>
</dict>
</plist>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extension's plist names the File Provider extension point, the class its binary
    /// registers, and an identifier under the app's; the app and the extension share one group,
    /// and only the extension is sandboxed.
    #[test]
    fn the_extension_is_a_file_provider_under_the_app() {
        let plist = files_info_plist("0.3.1");
        assert!(plist.contains("<string>com.apple.fileprovider-nonui</string>"));
        assert!(plist.contains("<string>SloptyFilesExtension</string>"));
        assert!(plist.contains(&format!("<string>{BUNDLE_ID}.files</string>")));
        assert!(plist.contains("<string>XPC!</string>"));
        let (app, files) = (app_entitlements(), files_entitlements());
        for rights in [&app, &files] {
            assert!(rights.contains(&format!("<string>{GROUP}</string>")));
        }
        assert!(files.contains("com.apple.security.app-sandbox</key>\n\t<true/>"));
        assert!(files.contains("com.apple.security.network.client"));
        assert!(!app.contains("app-sandbox"));
    }

    /// The extension sits where the system looks for an app's extensions, and its executable is
    /// the one its `Info.plist` names, in the place a bundle keeps it.
    #[test]
    fn the_extension_sits_in_the_apps_plugins() {
        let appex = Appex::under(Utf8Path::new("Slopty.app/Contents"));
        assert_eq!(appex.bundle, "Slopty.app/Contents/PlugIns/SloptyFiles.appex");
        assert_eq!(
            appex.executable,
            "Slopty.app/Contents/PlugIns/SloptyFiles.appex/Contents/MacOS/slopty-files"
        );
        assert_eq!(
            appex.info_plist,
            "Slopty.app/Contents/PlugIns/SloptyFiles.appex/Contents/Info.plist"
        );
        let named = format!("<key>CFBundleExecutable</key>\n\t<string>{FILES_BIN}</string>");
        assert!(files_info_plist("0.3.1").contains(&named));
        assert_eq!(appex.executable.file_name(), Some(FILES_BIN));
    }

    /// A real identity signs under the identifier with a secure timestamp (notarisation wants
    /// one), ad hoc with none.
    #[test]
    fn each_binary_is_signed_under_its_identifier() {
        let path = Utf8Path::new("/b/Slopty.app/Contents/MacOS/slopty-worker");
        let id = Signing::Identity("Developer ID Application: A (AJ4R8GWM7A)".to_owned());
        assert_eq!(
            id.codesign(Some("dev.aislopware.slopty.worker"), path).join(" "),
            "--force --options runtime --timestamp --identifier dev.aislopware.slopty.worker \
             --sign Developer ID Application: A (AJ4R8GWM7A) /b/Slopty.app/Contents/MacOS/slopty-worker"
        );
        assert_eq!(
            Signing::AdHoc.codesign(None, path).join(" "),
            "--force --options runtime --timestamp=none --sign - \
             /b/Slopty.app/Contents/MacOS/slopty-worker"
        );
        let helpers: Vec<&str> = IDENTIFIERS.iter().map(|(bin, _)| *bin).collect();
        assert_eq!(helpers, BINARIES[1..], "every binary but the app's own");
    }

    /// A Developer ID requirement holds to the identifier and the team; an ad hoc one to the
    /// hash, which every build changes.
    #[test]
    fn a_stable_requirement_names_the_identifier_and_team() {
        let id = "dev.aislopware.slopty.worker";
        let developer_id = "designated => identifier \"dev.aislopware.slopty.worker\" and anchor \
             apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and \
             certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate \
             leaf[subject.OU] = AJ4R8GWM7A";
        assert!(stable(developer_id, id));
        assert!(!stable("designated => cdhash H\"0123abcd\"", id));
        assert!(!stable(developer_id, "dev.aislopware.slopty.ptyd"));
    }

    #[test]
    fn info_plist_names_the_app_and_version() {
        let plist = info_plist("0.3.1");
        assert!(plist.contains("<string>slopty-app</string>"));
        assert!(plist.contains("<string>0.3.1</string>"));
        assert!(plist.contains(BUNDLE_ID));
    }
}
