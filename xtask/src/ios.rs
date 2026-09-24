//! `xtask ios`: build the Rust static library, generate the Xcode project with `XcodeGen`, build
//! the app bundle, and run it on the simulator or a connected device.
//!
//! The only non-Rust source is `apps/slopty-ios/app/main.m` (the UIKit bootstrap). The `XcodeGen`
//! spec and `Info.plist` are generated under `target/ios/<sdk>/` on every run, so nothing in
//! the tree is Xcode-specific.

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};
use xshell::{Shell, cmd};

use crate::tools::step;

/// Bundle identifier of the iOS app.
pub const BUNDLE_ID: &str = "dev.aislopware.slopty";
/// Product / scheme name.
const PRODUCT: &str = "Slopty";
/// Deployment target (the project floor).
const IOS_VERSION: &str = "26.5";
/// Runtime for the simulator.
const SIM_RUNTIME: &str = "com.apple.CoreSimulator.SimRuntime.iOS-26-5";

/// Which simulator `sim` boots; each is created on first use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SimKind {
    /// iPhone 17 Pro.
    #[default]
    Iphone,
    /// iPad Pro 13-inch (M5): the tablet layout, pointer and hardware keyboard.
    Ipad,
}

impl SimKind {
    /// Simulator device name.
    const fn name(self) -> &'static str {
        match self {
            Self::Iphone => "Slopty iPhone",
            Self::Ipad => "Slopty iPad",
        }
    }

    /// `simctl` device type.
    const fn device_type(self) -> &'static str {
        match self {
            Self::Iphone => "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro",
            Self::Ipad => "com.apple.CoreSimulator.SimDeviceType.iPad-Pro-13-inch-M5-12GB",
        }
    }
}

/// iOS subcommands.
#[derive(Subcommand, Debug)]
pub enum IosCmd {
    /// Build for the simulator, install and launch it (streams the app's stderr).
    Sim(IosOpts),
    /// Build for a connected device (signed with `SLOPTY_TEAM_ID`), install and launch it.
    Device(IosOpts),
}

/// Shared options.
#[derive(Args, Debug, Clone)]
pub struct IosOpts {
    /// Build with `--release`.
    #[arg(long)]
    release: bool,
    /// `RUST_LOG` filter for the app.
    #[arg(long, default_value = "info")]
    log: String,
    /// Only build; do not install or launch.
    #[arg(long)]
    no_run: bool,
    /// Device name or identifier for `device` (default: the first connected iPhone/iPad).
    #[arg(long)]
    device: Option<String>,
    /// Which simulator `sim` uses (`SLOPTY_SIM_UDID` overrides both).
    #[arg(long, value_enum, default_value_t)]
    sim: SimKind,
    /// Build with the `e2e` feature (test-socket renderer access; `cargo xtask e2e ios` sets it).
    #[arg(long)]
    e2e: bool,
}

impl IosOpts {
    /// Options for the simulator self-test: debug build with the `e2e` feature.
    #[must_use]
    pub fn for_e2e(sim: SimKind, log: &str) -> Self {
        Self { release: false, log: log.to_owned(), no_run: false, device: None, sim, e2e: true }
    }
}

/// One SDK flavour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sdk {
    Simulator,
    Device,
}

impl Sdk {
    const fn triple(self) -> &'static str {
        match self {
            Self::Simulator => "aarch64-apple-ios-sim",
            Self::Device => "aarch64-apple-ios",
        }
    }

    const fn xcode_sdk(self) -> &'static str {
        match self {
            Self::Simulator => "iphonesimulator",
            Self::Device => "iphoneos",
        }
    }

    const fn dir(self) -> &'static str {
        match self {
            Self::Simulator => "sim",
            Self::Device => "device",
        }
    }
}

pub fn run(sh: &Shell, cmd: &IosCmd) -> Result<()> {
    match cmd {
        IosCmd::Sim(opts) => {
            let app = build(sh, Sdk::Simulator, opts)?;
            if !opts.no_run {
                run_simulator(sh, &app, opts)?;
            }
        }
        IosCmd::Device(opts) => {
            let app = build(sh, Sdk::Device, opts)?;
            if !opts.no_run {
                run_device(sh, &app, opts)?;
            }
        }
    }
    Ok(())
}

/// Build the static library and the app bundle; returns the `.app` path.
/// One 1024 px universal icon; Xcode derives every other size from it.
const APPICON_CONTENTS: &str = r#"{
  "images" : [
    {
      "filename" : "AppIcon.png",
      "idiom" : "universal",
      "platform" : "ios",
      "size" : "1024x1024"
    }
  ],
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}
"#;

fn build(sh: &Shell, sdk: Sdk, opts: &IosOpts) -> Result<Utf8PathBuf> {
    let root = crate::tools::repo_root()?;
    let profile = if opts.release { "release" } else { "debug" };
    let triple = sdk.triple();
    let mut cargo_flags: Vec<&str> = Vec::new();
    if opts.release {
        cargo_flags.push("--release");
    }
    if opts.e2e {
        cargo_flags.extend(["--features", "slopty-ios/e2e"]);
    }
    {
        let _env = sh.push_env("IPHONEOS_DEPLOYMENT_TARGET", IOS_VERSION);
        step(
            &format!("cargo build slopty-ios ({triple}, {profile})"),
            &cmd!(sh, "cargo build -p slopty-ios --target {triple} {cargo_flags...}"),
        )?;
    }
    let lib = root.join("target").join(triple).join(profile).join("libslopty_ios.a");
    if !lib.exists() {
        bail!("static library missing: {lib}");
    }

    let out = root.join("target").join("ios").join(sdk.dir());
    sh.create_dir(&out)?;
    let shim = root.join("apps").join("slopty-ios").join("app").join("main.m");
    let icon = crate::icon::Icon::load(sh)?;
    let iconset = out.join("Assets.xcassets").join("AppIcon.appiconset");
    sh.create_dir(&iconset)?;
    sh.write_file(iconset.join("Contents.json"), APPICON_CONTENTS)?;
    sh.write_file(iconset.join("AppIcon.png"), icon.png(1024)?)?;
    sh.write_file(out.join("project.yml"), project_spec(sdk, &shim, &lib))?;
    step(
        "xcodegen generate",
        &cmd!(sh, "xcodegen generate --quiet --spec {out}/project.yml --project {out}"),
    )?;

    let configuration = if opts.release { "Release" } else { "Debug" };
    let derived = out.join("derived");
    let xcode_sdk = sdk.xcode_sdk();
    let project = out.join(format!("{PRODUCT}.xcodeproj"));
    let mut xcodebuild = cmd!(
        sh,
        "xcodebuild -quiet -project {project} -scheme {PRODUCT} -sdk {xcode_sdk} -configuration {configuration} -derivedDataPath {derived}"
    );
    match sdk {
        Sdk::Simulator => xcodebuild = xcodebuild.arg("CODE_SIGNING_ALLOWED=NO"),
        Sdk::Device => {
            let team = std::env::var("SLOPTY_TEAM_ID")
                .context("set SLOPTY_TEAM_ID to your Apple Developer team id for device builds")?;
            xcodebuild = xcodebuild
                .arg(format!("DEVELOPMENT_TEAM={team}"))
                .arg("CODE_SIGN_STYLE=Automatic")
                .arg("-allowProvisioningUpdates");
        }
    }
    step("xcodebuild", &xcodebuild.arg("build"))?;
    let app = derived
        .join("Build")
        .join("Products")
        .join(format!("{configuration}-{xcode_sdk}"))
        .join(format!("{PRODUCT}.app"));
    if !app.exists() {
        bail!("app bundle missing: {app}");
    }
    println!("✔ {app}");
    Ok(app)
}

/// The `XcodeGen` spec: one application target wrapping the static library.
fn project_spec(sdk: Sdk, shim: &Utf8Path, lib: &Utf8Path) -> String {
    let lib_dir = lib.parent().map_or(".", Utf8Path::as_str);
    let signing = match sdk {
        Sdk::Simulator => "    CODE_SIGNING_ALLOWED: NO\n",
        Sdk::Device => "",
    };
    format!(
        r#"name: {PRODUCT}
options:
  deploymentTarget:
    iOS: "{IOS_VERSION}"
  bundleIdPrefix: dev.aislopware
settings:
  base:
    ARCHS: arm64
    ONLY_ACTIVE_ARCH: YES
    SWIFT_VERSION: "6.0"
{signing}targets:
  {PRODUCT}:
    type: application
    platform: iOS
    sources:
      - path: {shim}
      - path: Assets.xcassets
    info:
      path: Info.plist
      properties:
        CFBundleDisplayName: {PRODUCT}
        UIApplicationSceneManifest:
          UIApplicationSupportsMultipleScenes: false
          UISceneConfigurations:
            UIWindowSceneSessionRoleApplication:
              - UISceneConfigurationName: Default Configuration
                UISceneDelegateClassName: SloptySceneDelegate
        UILaunchScreen: {{}}
        UIStatusBarHidden: true
        UIViewControllerBasedStatusBarAppearance: false
        UISupportedInterfaceOrientations:
          - UIInterfaceOrientationPortrait
          - UIInterfaceOrientationLandscapeLeft
          - UIInterfaceOrientationLandscapeRight
        UISupportedInterfaceOrientations~ipad:
          - UIInterfaceOrientationPortrait
          - UIInterfaceOrientationPortraitUpsideDown
          - UIInterfaceOrientationLandscapeLeft
          - UIInterfaceOrientationLandscapeRight
        NSLocalNetworkUsageDescription: Slopty finds your host on the local network.
        NSBonjourServices:
          - _slopty._udp
        CADisableMinimumFrameDurationOnPhone: true
        UIApplicationSupportsIndirectInputEvents: true
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: {BUNDLE_ID}
        PRODUCT_NAME: {PRODUCT}
        TARGETED_DEVICE_FAMILY: "1,2"
        ASSETCATALOG_COMPILER_APPICON_NAME: AppIcon
        DEAD_CODE_STRIPPING: YES
        LIBRARY_SEARCH_PATHS:
          - "{lib_dir}"
        OTHER_LDFLAGS:
          - "-Wl,-force_load,{lib}"
          - "-lc++"
    dependencies:
      - sdk: AVFoundation.framework
      - sdk: AudioToolbox.framework
      - sdk: CoreFoundation.framework
      - sdk: CoreGraphics.framework
      - sdk: CoreMedia.framework
      - sdk: CoreText.framework
      - sdk: CoreVideo.framework
      - sdk: Foundation.framework
      - sdk: GameController.framework
      - sdk: Metal.framework
      - sdk: Network.framework
      - sdk: QuartzCore.framework
      - sdk: Security.framework
      - sdk: SystemConfiguration.framework
      - sdk: UIKit.framework
      - sdk: VideoToolbox.framework
"#
    )
}

/// Build for the simulator, boot it (creating it on first use) and install the app; returns
/// the simulator's UDID. `cargo xtask e2e ios` launches the app itself, with its own environment.
///
/// # Errors
///
/// When the build, the boot or the install fails.
pub fn install_on_simulator(sh: &Shell, opts: &IosOpts) -> Result<String> {
    let app = build(sh, Sdk::Simulator, opts)?;
    let udid = boot_and_install(sh, &app, opts.sim)?;
    Ok(udid)
}

/// Boot the simulator for `kind` (creating it on first use) and install `app`.
fn boot_and_install(sh: &Shell, app: &Utf8Path, kind: SimKind) -> Result<String> {
    let udid = simulator_udid(sh, kind)?;
    let boot = cmd!(sh, "xcrun simctl boot {udid}").ignore_status().ignore_stderr().quiet();
    boot.run()?;
    // A device created a moment ago is still booting; `launch` on it blocks for minutes.
    step("simctl bootstatus", &cmd!(sh, "xcrun simctl bootstatus {udid} -b"))?;
    step("open Simulator.app", &cmd!(sh, "open -a Simulator"))?;
    step("simctl install", &cmd!(sh, "xcrun simctl install {udid} {app}"))?;
    Ok(udid)
}

/// Boot the simulator, install and launch with the console attached.
fn run_simulator(sh: &Shell, app: &Utf8Path, opts: &IosOpts) -> Result<()> {
    let udid = boot_and_install(sh, app, opts.sim)?;
    let _log = sh.push_env("SIMCTL_CHILD_RUST_LOG", &opts.log);
    step(
        "simctl launch (Ctrl-C to detach; the app keeps running)",
        &cmd!(sh, "xcrun simctl launch --console-pty {udid} {BUNDLE_ID}"),
    )
}

/// The simulator to use: `SLOPTY_SIM_UDID`, else the device named for `kind`, created if needed.
fn simulator_udid(sh: &Shell, kind: SimKind) -> Result<String> {
    if let Ok(udid) = std::env::var("SLOPTY_SIM_UDID") {
        return Ok(udid);
    }
    let name = kind.name();
    let list = cmd!(sh, "xcrun simctl list devices available").read()?;
    if let Some(udid) = simulator_named(&list, name) {
        return Ok(udid.to_owned());
    }
    println!("▶ creating simulator {name:?}");
    let device_type = kind.device_type();
    let udid = cmd!(sh, "xcrun simctl create {name} {device_type} {SIM_RUNTIME}").read()?;
    Ok(udid.trim().to_owned())
}

/// The UDID of the device called exactly `name` in `simctl list devices` output.
fn simulator_named<'a>(list: &'a str, name: &str) -> Option<&'a str> {
    list.lines().map(str::trim).find_map(|line| {
        let rest = line.strip_prefix(name)?;
        let (_, after) = rest.trim_start().split_once('(')?;
        // "Slopty iPhone (UDID) (Booted)": the name must end here, not run on ("Slopty iPhone 2").
        rest.starts_with(' ').then_some(())?;
        after.split_once(')').map(|(udid, _)| udid)
    })
}

/// Install on a connected device via `devicectl` and launch with the console attached.
fn run_device(sh: &Shell, app: &Utf8Path, opts: &IosOpts) -> Result<()> {
    let device = match &opts.device {
        Some(d) => d.clone(),
        None => first_device(sh)?,
    };
    step(
        "devicectl install",
        &cmd!(sh, "xcrun devicectl device install app --device {device} {app}"),
    )?;
    let env_json = format!("{{\"RUST_LOG\":\"{}\"}}", opts.log);
    step(
        "devicectl launch",
        &cmd!(
            sh,
            "xcrun devicectl device process launch --console --device {device} --environment-variables {env_json} {BUNDLE_ID}"
        ),
    )
}

/// The first connected (available) device from `devicectl`.
fn first_device(sh: &Shell) -> Result<String> {
    let json = cmd!(sh, "xcrun devicectl list devices --json-output /dev/stdout --quiet").read()?;
    // Minimal parse: the identifier of the first device whose connection is not "unavailable".
    let mut identifier: Option<String> = None;
    let mut available = false;
    for line in json.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("\"identifier\" : \"") {
            identifier =
                rest.strip_suffix("\",").or_else(|| rest.strip_suffix('"')).map(str::to_owned);
        }
        if line.contains("\"transportType\"") && !line.contains("unavailable") {
            available = true;
        }
        if line.starts_with('}') {
            if available && let Some(id) = identifier.take() {
                return Ok(id);
            }
            identifier = None;
            available = false;
        }
    }
    bail!("no connected device; pass --device <name-or-udid> (see `xcrun devicectl list devices`)")
}
