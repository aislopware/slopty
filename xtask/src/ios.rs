//! `xtask ios`: build the Rust static library, generate the Xcode project with `XcodeGen`, build
//! the app bundle, and run it on the simulator or a connected device.
//!
//! The app has no source outside Rust: the static library holds `main` and UIKit's delegates,
//! and Xcode only links it and packages the bundle. The notification service extension
//! (`apps/slopty-notify`), which opens a pushed note, is a second static library linked the same
//! way into `SloptyNotify.appex` inside the app. The `XcodeGen` spec, the `Info.plist`s and the
//! entitlements are generated under `target/ios/<sdk>/` on every run, so nothing in the tree is
//! Xcode-specific.

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand};
use xshell::{Shell, cmd};

use crate::tools::step;

/// Bundle identifier of the iOS app.
pub const BUNDLE_ID: &str = "dev.aislopware.slopty";
/// The notification service extension's target and product name.
const NOTIFY: &str = "SloptyNotify";
/// Its bundle identifier, under the app's.
const NOTIFY_ID: &str = "dev.aislopware.slopty.notifications";
/// The Keychain access group the app and the extension share, where the phone's push key and
/// device token live (`slopty_platform::notify::pushed::keychain`): first in both, so an item
/// named in no group goes there.
const SHARED_KEYCHAIN: &str = "$(AppIdentifierPrefix)dev.aislopware.slopty.shared";
/// Product / scheme name.
const PRODUCT: &str = "Slopty";
/// Deployment target (the project floor). Simulators run an installed iOS runtime at or above
/// it ([`choose_simulator`]): the floor is what the app needs, not what the tests run on.
const IOS_VERSION: &str = "27.0";

/// Which simulator `sim` boots; each is created on first use, as the device of its line that the
/// installed runtimes take ([`choose_simulator`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SimKind {
    /// The newest standard-size Pro iPhone ("iPhone 18 Pro").
    #[default]
    Iphone,
    /// The newest 13-inch iPad Pro: the tablet layout, pointer and hardware keyboard.
    Ipad,
}

impl SimKind {
    /// Simulator device name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Iphone => "Slopty iPhone",
            Self::Ipad => "Slopty iPad",
        }
    }

    /// The product family `simctl` files the kind's device types under.
    pub const fn family(self) -> &'static str {
        match self {
            Self::Iphone => "iPhone",
            Self::Ipad => "iPad",
        }
    }

    /// Whether device type `name` is of the line the kind's frames are drawn on, so a new
    /// generation keeps the screen its goldens were taken at: the standard-size Pro phone
    /// ("iPhone 18 Pro", not its Max), the 13-inch Pro tablet ("iPad Pro 13-inch (M5)").
    fn of_line(self, name: &str) -> bool {
        match self {
            Self::Iphone => name.starts_with("iPhone ") && name.ends_with(" Pro"),
            Self::Ipad => name.starts_with("iPad Pro 13-inch"),
        }
    }
}

/// The device type and runtime a simulator for one [`SimKind`] is made of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Simulator {
    /// The `simctl` device type's identifier.
    pub device_type: String,
    /// The runtime's identifier.
    pub runtime: String,
    /// Both, in words: "iPhone 18 Pro on iOS 27.0".
    pub described: String,
}

/// The simulator to make for `kind`, from `simctl list runtimes -j` and `simctl list devicetypes
/// -j`: the newest device type of the kind's line ([`SimKind::of_line`]) that an available iOS
/// runtime at or above [`IOS_VERSION`] takes, else the newest of its family, on the newest such
/// runtime that takes it. A device type is newer for the newer runtime it needs, then its model
/// ("iPhone19,2" after "iPhone18,1"). A runtime made for one device (iOS 27.1, which takes only
/// the iPhone Duo) or a device the runtime does not take is never paired, so a runner image that
/// adds either moves the choice rather than failing `simctl create`. `None` when no runtime
/// takes any device of the family.
///
/// # Errors
///
/// When either list does not parse.
pub fn choose_simulator(
    kind: SimKind,
    runtimes: &str,
    devicetypes: &str,
) -> Result<Option<Simulator>> {
    #[derive(serde::Deserialize)]
    struct List {
        devicetypes: Vec<DeviceType>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DeviceType {
        identifier: String,
        name: String,
        #[serde(default)]
        product_family: String,
        #[serde(default)]
        min_runtime_version: u64,
        #[serde(default)]
        model_identifier: String,
    }
    /// "iPhone19,2" as (19, 2); (0, 0) for one with no numbers.
    fn model(id: &str) -> (u32, u32) {
        let digits = id.trim_start_matches(|c: char| !c.is_ascii_digit());
        let mut parts = digits.split(',').map(|n| n.parse().unwrap_or(0));
        (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
    }
    let runtimes = ios_runtimes(runtimes)?;
    let list: List = serde_json::from_str(devicetypes).context("simctl's device type list")?;
    let rank =
        |t: &DeviceType| (kind.of_line(&t.name), t.min_runtime_version, model(&t.model_identifier));
    let taken =
        list.devicetypes.iter().filter(|t| t.product_family == kind.family()).filter_map(|t| {
            // Newest first: the first that takes it is the newest.
            let runtime = runtimes.iter().find(|r| r.takes(&t.identifier))?;
            Some((t, runtime))
        });
    // On a tie the lower identifier ("…-M5-12GB" before "…-M5-16GB"): the same device each run.
    let best = taken.max_by(|(a, _), (b, _)| {
        rank(a).cmp(&rank(b)).then_with(|| b.identifier.cmp(&a.identifier))
    });
    Ok(best.map(|(t, r)| Simulator {
        device_type: t.identifier.clone(),
        runtime: r.identifier.clone(),
        described: format!("{} on iOS {}", t.name, r.version),
    }))
}

/// An iOS simulator runtime, as `simctl list runtimes -j` lists it.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    identifier: String,
    version: String,
    platform: String,
    is_available: bool,
    /// The device types it runs.
    #[serde(default)]
    supported_device_types: Vec<Supported>,
}

/// One device type a runtime runs.
#[derive(serde::Deserialize)]
struct Supported {
    identifier: String,
}

impl Runtime {
    /// Whether it runs device type `identifier`.
    fn takes(&self, identifier: &str) -> bool {
        self.supported_device_types.iter().any(|s| s.identifier == identifier)
    }
}

/// `version` as numbers: "27.10" after "27.2".
fn version_of(version: &str) -> Vec<u32> {
    version.split('.').map_while(|n| n.parse().ok()).collect()
}

/// The available iOS runtimes at or above [`IOS_VERSION`] in `simctl list runtimes -j`, newest
/// first.
fn ios_runtimes(json: &str) -> Result<Vec<Runtime>> {
    #[derive(serde::Deserialize)]
    struct List {
        runtimes: Vec<Runtime>,
    }
    let floor = version_of(IOS_VERSION);
    let list: List = serde_json::from_str(json).context("simctl's runtime list")?;
    let mut ios: Vec<Runtime> = list
        .runtimes
        .into_iter()
        .filter(|r| r.platform == "iOS" && r.is_available && version_of(&r.version) >= floor)
        .collect();
    ios.sort_by_key(|r| std::cmp::Reverse(version_of(&r.version)));
    Ok(ios)
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
    preflight(sh, matches!(cmd, IosCmd::Sim(_)))?;
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

/// What building and running the app needs besides cargo, checked before anything compiles: a
/// missing `xcodegen` failed a runner's iOS e2e after 30 minutes of build (run 38110033656).
/// For the `simulator`, an iOS runtime at or above [`IOS_VERSION`] too.
///
/// # Errors
///
/// Naming what is missing and how to get it.
pub fn preflight(sh: &Shell, simulator: bool) -> Result<()> {
    if !crate::tools::has(sh, "xcodegen") {
        bail!(
            "xcodegen writes the app's Xcode project and is not on PATH: `brew install xcodegen` \
             (CI installs the pinned release, check.yml)"
        );
    }
    cmd!(sh, "xcodebuild -version").quiet().ignore_stdout().run().context(
        "xcodebuild builds the app: is Xcode installed and selected (`xcode-select -p`)?",
    )?;
    if simulator {
        let runtimes = cmd!(sh, "xcrun simctl list runtimes -j").read()?;
        if ios_runtimes(&runtimes)?.is_empty() {
            bail!(
                "no iOS simulator runtime {IOS_VERSION} or newer is installed \
                 (`xcodebuild -downloadPlatform iOS`)"
            );
        }
    }
    Ok(())
}

/// Build the static library and the app bundle; returns the `.app` path.
fn build(sh: &Shell, sdk: Sdk, opts: &IosOpts) -> Result<Utf8PathBuf> {
    // Wherever cargo builds for `sh` (the checkout's `target`, or a lane's own dir), so a tree
    // built elsewhere finds its libraries where cargo put them.
    let target = crate::tools::target_dir(sh)?;
    let profile = if opts.release { "release" } else { "debug" };
    let triple = sdk.triple();
    let mut cargo_flags: Vec<&str> = Vec::new();
    if opts.release {
        cargo_flags.push("--release");
    }
    if opts.e2e {
        cargo_flags.extend(["--features", "slopty-ios/e2e"]);
    }
    let out = target.join("ios").join(sdk.dir());
    sh.create_dir(&out)?;
    let link_object = out.join("link.o");
    let (links, notify_links) = {
        let _env = sh.push_env("IPHONEOS_DEPLOYMENT_TARGET", IOS_VERSION);
        let what = format!("{triple}, {profile}");
        let links = build_static_lib(sh, "slopty-ios", &what, triple, &cargo_flags)?;
        let release: &[&str] = if opts.release { &["--release"] } else { &[] };
        let notify_links = build_static_lib(sh, "slopty-notify", &what, triple, release)?;
        // Xcode links a target only when it has an object file of its own, and every line of
        // the app is in the static library: an empty crate compiled to an object is that file.
        step(
            "rustc link.o",
            &cmd!(
                sh,
                "rustc --crate-name slopty_ios_link --crate-type lib --edition 2024 --emit obj --target {triple} -o {link_object} -"
            )
            .stdin(""),
        )?;
        (links, notify_links)
    };
    let lib = target.join(triple).join(profile).join("libslopty_ios.a");
    let notify_lib = target.join(triple).join(profile).join("libslopty_notify.a");
    for built in [&lib, &notify_lib] {
        if !built.exists() {
            bail!("static library missing: {built}");
        }
    }

    // Xcode compiles the Icon Composer document (`wrapper.icon`) with the target's resources.
    crate::icon::Art::load(sh)?.write_document(sh, &out)?;
    let app = Linked { lib: &lib, links: &links };
    let notify = Linked { lib: &notify_lib, links: &notify_links };
    let spec = project_spec(sdk, opts.release, &link_object, &app, &notify);
    sh.write_file(out.join("project.yml"), spec)?;
    step(
        "xcodegen generate",
        &cmd!(sh, "xcodegen generate --quiet --spec {out}/project.yml --project {out}"),
    )?;

    let configuration = if opts.release { "Release" } else { "Debug" };
    let derived = out.join("derived");
    let xcode_sdk = sdk.xcode_sdk();
    let project = out.join(format!("{PRODUCT}.xcodeproj"));
    let xcodebuild = cmd!(
        sh,
        "xcodebuild -quiet -project {project} -scheme {PRODUCT} -sdk {xcode_sdk} -configuration {configuration} -derivedDataPath {derived}"
    );
    let team = match sdk {
        Sdk::Simulator => None,
        Sdk::Device => Some(
            std::env::var("SLOPTY_TEAM_ID")
                .context("set SLOPTY_TEAM_ID to your Apple Developer team id for device builds")?,
        ),
    };
    step("xcodebuild", &xcodebuild.args(signing_args(team.as_deref())).arg("build"))?;
    let app = derived
        .join("Build")
        .join("Products")
        .join(format!("{configuration}-{xcode_sdk}"))
        .join(format!("{PRODUCT}.app"));
    if !app.join(PRODUCT).exists() {
        bail!("app bundle missing or without its executable: {app}");
    }
    println!("✔ {app}");
    Ok(app)
}

/// Build `package`'s static library; returns the linker flags for the system libraries and
/// frameworks its crates link.
///
/// A static library keeps no record of those (a `#[link]` puts no load command in its objects),
/// so the app's link line is rustc's own list (`--print native-static-libs`) rather than one kept
/// by hand, and a crate that starts calling a new framework links it here without an edit.
fn build_static_lib(
    sh: &Shell,
    package: &str,
    what: &str,
    triple: &str,
    cargo_flags: &[&str],
) -> Result<Vec<String>> {
    let title = format!("cargo build {package} ({what})");
    println!("▶ {title}");
    let started = std::time::Instant::now();
    let output = cmd!(
        sh,
        "cargo rustc -p {package} --lib --crate-type staticlib --target {triple} {cargo_flags...} --message-format json-diagnostic-rendered-ansi -- --print native-static-libs"
    )
    .ignore_status()
    .output()
    .with_context(|| format!("step failed to start: {title}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (links, rendered) = native_static_libs(&stdout);
    eprint!("{rendered}{}", String::from_utf8_lossy(&output.stderr));
    let ok = output.status.success();
    println!("  {} {title} ({:.1?})", if ok { "✓" } else { "✘" }, started.elapsed());
    if !ok {
        bail!("step failed: {title}");
    }
    links.with_context(|| format!("{title}: rustc printed no native-static-libs"))
}

/// From cargo's JSON messages: the flags rustc's `native-static-libs` note lists, and every other
/// diagnostic as rustc rendered it.
fn native_static_libs(messages: &str) -> (Option<Vec<String>>, String) {
    let mut links = None;
    let mut rendered = String::new();
    for line in messages.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if message.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-message") {
            continue;
        }
        let Some(diagnostic) = message.get("message") else { continue };
        let text =
            diagnostic.get("message").and_then(serde_json::Value::as_str).unwrap_or_default();
        if let Some(flags) = text.strip_prefix("native-static-libs:") {
            links = Some(flags.split_whitespace().map(str::to_owned).collect());
        } else if text.starts_with("link against the following native artifacts") {
            // The note's preamble, which the list above answers.
        } else if let Some(shown) = diagnostic.get("rendered").and_then(serde_json::Value::as_str) {
            rendered.push_str(shown);
        }
    }
    (links, rendered)
}

/// A static library a target links whole, with the system libraries and frameworks its crates
/// need.
struct Linked<'a> {
    lib: &'a Utf8Path,
    links: &'a [String],
}

impl Linked<'_> {
    /// The target's `OTHER_LDFLAGS` entries: the library whole, then `links`, each on its own
    /// line at `indent`.
    fn ldflags(&self, indent: &str) -> String {
        use std::fmt::Write as _;
        let mut flags = format!("{indent}- \"-Wl,-force_load,{}\"\n", self.lib);
        for flag in self.links {
            let _infallible = writeln!(flags, "{indent}- \"{flag}\"");
        }
        flags
    }
}

/// What `xcodebuild` is told about signing: a device's build is signed by `team`; the
/// simulator's is told nothing, so the spec's ad hoc identity signs it and Xcode embeds the
/// entitlements (the shared Keychain group, the push environment) in each binary's
/// `__entitlements` section. Turning signing off there (`CODE_SIGNING_ALLOWED=NO`) dropped them,
/// and the extension could not read the key the app kept.
fn signing_args(team: Option<&str>) -> Vec<String> {
    team.map_or_else(Vec::new, |team| {
        vec![
            format!("DEVELOPMENT_TEAM={team}"),
            "CODE_SIGN_STYLE=Automatic".to_owned(),
            "-allowProvisioningUpdates".to_owned(),
        ]
    })
}

/// The `XcodeGen` spec: the application target wrapping `app`'s static library, and the
/// notification service extension wrapping `notify`'s, embedded in it; each linked beside
/// `link_object` (see [`build`]).
///
/// Both carry the shared Keychain group, and the app the push entitlement: the sandbox APNs for
/// a debug build, production for a release. The simulator's build is signed ad hoc, which is
/// what applies entitlements there; a device's is signed by its team.
fn project_spec(
    sdk: Sdk,
    release: bool,
    link_object: &Utf8Path,
    app: &Linked<'_>,
    notify: &Linked<'_>,
) -> String {
    let lib_dir = app.lib.parent().map_or(".", Utf8Path::as_str);
    let link_flags = app.ldflags("          ");
    let notify_flags = notify.ldflags("          ");
    let signing = match sdk {
        Sdk::Simulator => "    CODE_SIGN_IDENTITY: \"-\"\n    CODE_SIGN_STYLE: Manual\n",
        Sdk::Device => "",
    };
    let aps = if release { "production" } else { "development" };
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
      - path: {icon}.icon
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
        UIStatusBarHidden: false
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
        NSLocalNetworkUsageDescription: Slopty connects to your workers on the local network.
        CADisableMinimumFrameDurationOnPhone: true
        # The code a Mac shows for its server is a `slopty://connect` link: the Camera app opens
        # it here, with the address filled in (`slopty_app::invite`).
        CFBundleURLTypes:
          - CFBundleURLName: {BUNDLE_ID}.connect
            CFBundleURLSchemes:
              - slopty
        UIApplicationSupportsIndirectInputEvents: true
        # A page's downloads land in Documents; these show them in the Files app.
        UIFileSharingEnabled: true
        LSSupportsOpeningDocumentsInPlace: true
        # A transfer asks to go on off screen as a continued-processing task, one per transfer.
        BGTaskSchedulerPermittedIdentifiers:
          - "{BUNDLE_ID}.transfer.*"
        # `remote-notification`: a background push from the server takes back a note answered on
        # another device.
        UIBackgroundModes:
          - processing
          - remote-notification
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: {BUNDLE_ID}
        PRODUCT_NAME: {PRODUCT}
        TARGETED_DEVICE_FAMILY: "1,2"
        ASSETCATALOG_COMPILER_APPICON_NAME: {icon}
        DEAD_CODE_STRIPPING: YES
        LIBRARY_SEARCH_PATHS:
          - "{lib_dir}"
        OTHER_LDFLAGS:
{link_flags}    entitlements:
      path: {PRODUCT}.entitlements
      properties:
        aps-environment: {aps}
        keychain-access-groups:
          - "{SHARED_KEYCHAIN}"
    dependencies:
      - framework: {link_object}
        embed: false
      - target: {NOTIFY}
  {NOTIFY}:
    type: app-extension
    platform: iOS
    info:
      path: {NOTIFY}-Info.plist
      properties:
        CFBundleDisplayName: {PRODUCT}
        NSExtension:
          NSExtensionPointIdentifier: com.apple.usernotifications.service
          NSExtensionPrincipalClass: SloptyNotificationService
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: {NOTIFY_ID}
        PRODUCT_NAME: {NOTIFY}
        TARGETED_DEVICE_FAMILY: "1,2"
        DEAD_CODE_STRIPPING: YES
        LIBRARY_SEARCH_PATHS:
          - "{lib_dir}"
        # The extension needs few of the frameworks its crates name: a framework nothing calls
        # is not loaded, under the extension's tight memory limit.
        OTHER_LDFLAGS:
          - "-Wl,-dead_strip_dylibs"
{notify_flags}    entitlements:
      path: {NOTIFY}.entitlements
      properties:
        keychain-access-groups:
          - "{SHARED_KEYCHAIN}"
    dependencies:
      - framework: {link_object}
        embed: false
"#,
        icon = crate::icon::NAME,
    )
}

/// Build for the simulator, boot it (creating it on first use) and install the app; returns
/// the simulator's UDID. `cargo xtask e2e ios` launches the app itself, with its own environment.
///
/// # Errors
///
/// When the build, the boot or the install fails.
/// The e2e build of the app for the simulator, in `sh`'s directory and target dir; its `.app`.
pub fn build_for_simulator(sh: &Shell, sim: SimKind, log: &str) -> Result<Utf8PathBuf> {
    build(sh, Sdk::Simulator, &IosOpts::for_e2e(sim, log))
}

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
    // Headless: simctl boots and drives it, and nothing needs Simulator.app's window.
    step("simctl bootstatus", &cmd!(sh, "xcrun simctl bootstatus {udid} -b"))?;
    let keyboard = no_hardware_keyboard(&udid);
    step("simctl: no hardware keyboard", &cmd!(sh, "xcrun simctl {keyboard...}"))?;
    step("simctl install", &cmd!(sh, "xcrun simctl install {udid} {app}"))?;
    Ok(udid)
}

/// The Darwin notification whose state says `CoreDevice`'s `dtuhidd` carries the host's keys,
/// buttons and touches into a simulator.
const HID_FROM_HOST: &str = "com.apple.coredevice.dtuhidd.active";

/// `simctl`'s arguments that leave the booted simulator `udid` with no hardware keyboard, as a
/// phone with none (`docs/decisions/testing.md`, "A simulator the e2e boots has no hardware
/// keyboard"). Its UIKit already has none: only a key from the host (`Simulator.app`'s)
/// attaches one, and the soft keyboard comes up. But the simulator's HID plugin
/// (`CoreSimulator`'s `SimulatorHID`) connects a virtual external keyboard at boot, which
/// `GameController` counts, so `GCKeyboard.coalescedKeyboard` was there in every simulator and
/// the phone's key row stayed away over the soft keyboard. While `HID_FROM_HOST` is set the
/// plugin disconnects it, with the screen's buttons and digitizer, which carry only the host's
/// events (the tests' touches and keys go in through the app). The state lives in the
/// simulator's own `notifyd`, so every boot sets it, before the app starts.
pub const fn no_hardware_keyboard(udid: &str) -> [&str; 8] {
    ["spawn", udid, "notifyutil", "-s", HID_FROM_HOST, "1", "-p", HID_FROM_HOST]
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
    let runtimes = cmd!(sh, "xcrun simctl list runtimes -j").read()?;
    let devicetypes = cmd!(sh, "xcrun simctl list devicetypes -j").read()?;
    let chosen = choose_simulator(kind, &runtimes, &devicetypes)?.with_context(|| {
        format!(
            "no installed iOS simulator runtime {IOS_VERSION} or newer takes an {} \
             (`xcodebuild -downloadPlatform iOS`)",
            kind.family()
        )
    })?;
    println!("▶ creating simulator {name:?}: {}", chosen.described);
    let Simulator { device_type, runtime, .. } = chosen;
    let udid = cmd!(sh, "xcrun simctl create {name} {device_type} {runtime}").read()?;
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

#[cfg(test)]
mod tests {
    use super::{
        Linked, Sdk, SimKind, choose_simulator, native_static_libs, project_spec, signing_args,
    };

    #[test]
    fn the_link_line_is_the_frameworks_rustc_lists_and_other_diagnostics_are_shown() {
        let message = |level: &str, text: &str| {
            serde_json::json!({
                "reason": "compiler-message",
                "message": { "level": level, "message": text, "rendered": format!("{level}: {text}\n") },
            })
            .to_string()
        };
        let messages = [
            serde_json::json!({ "reason": "compiler-artifact" }).to_string(),
            message("warning", "unused import"),
            message("note", "link against the following native artifacts when linking against this static library"),
            message("note", "native-static-libs: -framework ImageIO -lSystem -lc"),
            "not json".to_owned(),
        ]
        .join("\n");
        let (links, rendered) = native_static_libs(&messages);
        assert_eq!(links.unwrap(), ["-framework", "ImageIO", "-lSystem", "-lc"]);
        assert_eq!(rendered, "warning: unused import\n");
        assert_eq!(native_static_libs(&message("warning", "x")).0, None, "no list, no link line");
    }

    /// The app embeds the notification service extension, whose bundle names the principal
    /// class the extension registers; each links its own library whole, the extension dropping
    /// the frameworks it does not call. Both share the Keychain group, the app alone asks for
    /// pushes (the sandbox for a debug build), and the simulator's build is signed ad hoc so
    /// the entitlements apply there.
    #[test]
    fn the_app_embeds_the_extension_that_opens_a_push() {
        let flags = ["-framework".to_owned(), "UserNotifications".to_owned()];
        let app = Linked { lib: "/t/libslopty_ios.a".into(), links: &flags };
        let notify = Linked { lib: "/t/libslopty_notify.a".into(), links: &flags };
        let spec = project_spec(Sdk::Simulator, false, "/t/link.o".into(), &app, &notify);
        // The two targets, the app's first.
        let (slopty, ext) = spec.split_once("\n  SloptyNotify:\n").unwrap();
        assert!(slopty.ends_with("\n      - target: SloptyNotify"), "the app embeds it");
        assert!(ext.contains("type: app-extension"));
        assert!(ext.contains("NSExtensionPointIdentifier: com.apple.usernotifications.service"));
        assert!(ext.contains("NSExtensionPrincipalClass: SloptyNotificationService"));
        assert!(ext.contains("PRODUCT_BUNDLE_IDENTIFIER: dev.aislopware.slopty.notifications"));
        assert!(slopty.contains("- \"-Wl,-force_load,/t/libslopty_ios.a\""));
        assert!(ext.contains("- \"-Wl,-force_load,/t/libslopty_notify.a\""));
        assert!(ext.contains("- \"-Wl,-dead_strip_dylibs\""), "the frameworks it never calls");
        assert!(!slopty.contains("libslopty_notify") && !ext.contains("libslopty_ios"));
        let group = "keychain-access-groups:\n          - \"$(AppIdentifierPrefix)dev.aislopware.slopty.shared\"";
        assert!(slopty.contains(group) && ext.contains(group), "one Keychain group for both");
        assert!(slopty.contains("aps-environment: development"), "a debug build's sandbox");
        assert!(!ext.contains("aps-environment"), "the app alone is pushed to");
        assert!(spec.contains("CODE_SIGN_IDENTITY: \"-\""), "ad hoc, so the entitlements apply");
        let release = project_spec(Sdk::Device, true, "/t/link.o".into(), &app, &notify);
        assert!(release.contains("aps-environment: production"));
        assert!(!release.contains("CODE_SIGN_IDENTITY"), "a team signs a device's build");
        assert!(signing_args(None).is_empty(), "the simulator's build is signed, ad hoc");
        assert!(
            signing_args(Some("ABCDE12345")).contains(&"DEVELOPMENT_TEAM=ABCDE12345".to_owned())
        );
    }

    /// The simulators made from what the runtimes take. On the runner image of 2026-10-29, whose
    /// newest runtime (iOS 27.1) takes the iPhone Duo alone, so that the iPhone 17 Pro made on it
    /// failed ("Incompatible device"): the iPhone 18 Pro and the 13-inch iPad Pro, on 27.0. On a
    /// runtime that no longer takes the old device, its line's next generation; with the line
    /// gone, the newest of the family; nothing below the floor or unavailable.
    #[test]
    fn the_simulator_is_the_newest_of_its_line_a_runtime_takes() {
        let id = |name: &str| {
            format!("com.apple.CoreSimulator.SimDeviceType.{}", name.replace(' ', "-"))
        };
        let ty = |name: &str, family: &str, min: u64, model: &str| {
            serde_json::json!({
                "identifier": id(name), "name": name, "productFamily": family,
                "minRuntimeVersion": min, "modelIdentifier": model,
            })
        };
        // As simctl writes a version, its major, minor and patch a byte each: 27.0 is 0x1B_0000.
        let (v26, v26_3, v27, v27_1, v28) = (0x1a_0000, 0x1a_0300, 0x1b_0000, 0x1b_0100, 0x1c_0000);
        let types = serde_json::json!({ "devicetypes": [
            ty("iPhone 17 Pro", "iPhone", v26, "iPhone18,1"),
            ty("iPhone Air", "iPhone", v26, "iPhone18,4"),
            ty("iPhone 18 Pro", "iPhone", v27, "iPhone19,2"),
            ty("iPhone 18 Pro Max", "iPhone", v27, "iPhone19,3"),
            ty("iPhone Duo", "iPhone", v27_1, "iPhone19,4"),
            ty("iPhone 19 Pro", "iPhone", v28, "iPhone20,1"),
            ty("iPad Pro 13-inch (M5) (16GB)", "iPad", v26, "iPad17,4"),
            ty("iPad Pro 13-inch (M5)", "iPad", v26, "iPad17,4"),
            ty("iPad Air 13-inch (M4)", "iPad", v26_3, "iPad16,11"),
            ty("Apple Watch Ultra 3 (49mm)", "Apple Watch", v26, "Watch7,12"),
        ]})
        .to_string();
        let runtime = |version: &str, available: bool, takes: &[&str]| {
            let takes: Vec<_> =
                takes.iter().map(|name| serde_json::json!({ "identifier": id(name) })).collect();
            serde_json::json!({
                "identifier": format!("com.apple.CoreSimulator.SimRuntime.iOS-{}", version.replace('.', "-")),
                "version": version, "platform": "iOS", "isAvailable": available,
                "supportedDeviceTypes": takes,
            })
        };
        let list = |runtimes: &[serde_json::Value]| {
            serde_json::json!({ "runtimes": runtimes }).to_string()
        };
        let chosen = |kind: SimKind, runtimes: &str| {
            choose_simulator(kind, runtimes, &types).unwrap().map(|s| s.described)
        };
        let ipads =
            ["iPad Pro 13-inch (M5) (16GB)", "iPad Pro 13-inch (M5)", "iPad Air 13-inch (M4)"];
        let mut on_27 = vec!["iPhone 17 Pro", "iPhone Air", "iPhone 18 Pro", "iPhone 18 Pro Max"];
        on_27.extend(ipads);
        let runner = list(&[
            runtime("26.5", true, &["iPhone 17 Pro", "iPhone Air"]),
            runtime("27.0", true, &on_27),
            runtime("27.1", true, &["iPhone Duo"]),
            runtime("27.2", false, &on_27),
        ]);
        assert_eq!(chosen(SimKind::Iphone, &runner).as_deref(), Some("iPhone 18 Pro on iOS 27.0"));
        assert_eq!(
            chosen(SimKind::Ipad, &runner).as_deref(),
            Some("iPad Pro 13-inch (M5) on iOS 27.0"),
            "the base model of a tied pair, the same each run"
        );
        let made = choose_simulator(SimKind::Iphone, &runner, &types).unwrap().unwrap();
        assert_eq!(made.device_type, "com.apple.CoreSimulator.SimDeviceType.iPhone-18-Pro");
        assert_eq!(made.runtime, "com.apple.CoreSimulator.SimRuntime.iOS-27-0");

        // A runtime that no longer takes the old device.
        let next = list(&[runtime("28.0", true, &["iPhone 19 Pro", "iPhone Air"])]);
        assert_eq!(chosen(SimKind::Iphone, &next).as_deref(), Some("iPhone 19 Pro on iOS 28.0"));
        // The newest runtime that takes it, by number: 27.10 after 27.2.
        let both = list(&[runtime("27.10", true, &on_27), runtime("27.2", true, &on_27)]);
        assert_eq!(chosen(SimKind::Iphone, &both).as_deref(), Some("iPhone 18 Pro on iOS 27.10"));
        // No device of the line: the newest of the family.
        let no_pro = list(&[runtime("27.1", true, &["iPhone Duo", "iPhone Air"])]);
        assert_eq!(chosen(SimKind::Iphone, &no_pro).as_deref(), Some("iPhone Duo on iOS 27.1"));
        assert_eq!(chosen(SimKind::Ipad, &no_pro), None, "no runtime takes an iPad");
        // Below the floor, or unavailable.
        let old =
            list(&[runtime("26.5", true, &["iPhone 17 Pro"]), runtime("27.2", false, &on_27)]);
        assert_eq!(chosen(SimKind::Iphone, &old), None);
        choose_simulator(SimKind::Iphone, "not json", &types).unwrap_err();
        choose_simulator(SimKind::Iphone, &runner, "not json").unwrap_err();
    }
}
