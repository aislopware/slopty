//! `xtask e2e`: the live tests, run on purpose and in isolation.
//!
//! Every test that touches real hardware or real permissions (posting events, capturing the
//! screen, running the daemons) is gated behind an environment variable and skips itself
//! otherwise, so `cargo gate` never touches the desktop. This command is the one sanctioned
//! way to run them: it picks the case, sets the gate variable, gives the daemons their own
//! `SLOPTY_DATA_DIR` under `target/e2e/` so nothing installed is touched, runs nextest with
//! output visible, and prints what ran. No ad-hoc key presses, screenshots or window
//! probing outside these tests: what they need is asserted inside them.

use anyhow::{Result, bail};
use clap::{Args, ValueEnum};
use xshell::{Shell, cmd};

use crate::tools::step;

/// Which live tests to run.
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    /// ptyd + hostd + the real app, driven through its test socket: pair, open a shell, type,
    /// read the rows back, render frames with the app's own renderer and compare them with
    /// the goldens. No permissions needed.
    App,
    /// ptyd + hostd + a paired client over loopback iroh: open a shell, read its output. No
    /// permissions needed.
    Host,
    /// Window geometry, a display stream and the stream through hostd. Needs Screen Recording
    /// for the test binaries (System Settings ▸ Privacy ▸ Screen Recording).
    Screen,
    /// One pointer move on the main display and back. Needs Accessibility for the test binary.
    Input,
    /// Frame-time budget: 20 streaming shells panned and zoomed, a display stream beside
    /// shells (only when `SLOPTY_SCREEN_E2E` is also set), typing with and without the
    /// local echo; prints the percentiles and fails when panning is over budget. No
    /// permissions needed for the shell scenarios.
    Smooth,
    /// The same frame-time scenarios with the app in the simulator (`--sim iphone|ipad`);
    /// indicative only, the simulator has no GPU-backed display link.
    SmoothIos,
    /// Two clients on one host: two app processes on this Mac, each on its own socket, paired
    /// with the same daemons; a terminal opened on one appears on the other, typing on both is
    /// serialised, attention badges both, a client dying leaves the other streaming, closing
    /// and notes propagate. No permissions needed (the display scenario also needs
    /// `SLOPTY_SCREEN_E2E`).
    Pair,
    /// The same with the second client in the simulator (`--sim iphone|ipad`): the Mac and
    /// the phone on one host.
    PairIos,
    /// One client, two hosts on two machines: ptyd + hostd + the app here, ptyd + hostd on a
    /// second machine over ssh (`SLOPTY_HOST2=<ssh name>`), under a temp root there with a
    /// private HOME. Proves cross-host attention against a real remote daemon: two hosts pair,
    /// a shell on the second round-trips over the mesh, a permission hook played to it through
    /// `slopty hook` badges the cross-host pill and routes a banner back to it, and killing it
    /// mid-stream turns its row amber then green on restart. No permissions needed.
    Hosts,
    /// All of the above (not `ios`, which needs a simulator, nor `hosts`, which needs a second
    /// machine).
    All,
    /// ptyd + hostd on the Mac and the iOS app in the simulator (`--sim iphone|ipad`), driven
    /// through its test socket: pair, open a shell, type, read the rows back, render frames
    /// against the per-device goldens (`ios-phone-*`, `ios-pad-*`).
    Ios,
}

/// Options.
#[derive(Args, Debug)]
pub struct E2eOpts {
    /// Which tests.
    #[arg(value_enum, default_value_t = Case::App)]
    case: Case,
    /// Write missing and failing render goldens under `crates/slopty-e2e/golden/`.
    #[arg(long)]
    accept: bool,
    /// Rewrite every render golden under `crates/slopty-e2e/golden/`.
    #[arg(long)]
    accept_all: bool,
    /// Data directory for the daemons the tests spawn (default `target/e2e`).
    #[arg(long)]
    data_dir: Option<String>,
    /// `RUST_LOG` for the daemons and tests.
    #[arg(long, default_value = "info")]
    log: String,
    /// Which simulator the `ios` case uses.
    #[arg(long, value_enum, default_value_t)]
    sim: crate::ios::SimKind,
}

/// One nextest invocation.
struct Suite {
    /// Gate variable set to `1`; `None` for a suite that also runs under `cargo gate`.
    gate: Option<&'static str>,
    /// Package.
    package: &'static str,
    /// Integration test target.
    test: &'static str,
    /// Test name filter, empty for the whole target.
    filter: &'static str,
    /// One test at a time: the frame-time scenarios measure a quiet machine.
    serial: bool,
}

const APP: &[Suite] = &[Suite {
    gate: Some("SLOPTY_APP_E2E"),
    package: "slopty-e2e",
    test: "app",
    filter: "",
    serial: false,
}];

const HOST: &[Suite] = &[Suite {
    gate: None,
    package: "slopty-hostd",
    test: "e2e",
    filter: "shell_round_trip",
    serial: false,
}];

const SCREEN: &[Suite] = &[
    Suite {
        gate: Some("SLOPTY_SCREEN_E2E"),
        package: "slopty-capture",
        test: "geometry",
        filter: "",
        serial: false,
    },
    Suite {
        gate: Some("SLOPTY_SCREEN_E2E"),
        package: "slopty-host",
        test: "screen",
        filter: "",
        serial: false,
    },
    Suite {
        gate: Some("SLOPTY_SCREEN_E2E"),
        package: "slopty-capture",
        test: "latency",
        filter: "",
        serial: false,
    },
    Suite {
        gate: Some("SLOPTY_SCREEN_E2E"),
        package: "slopty-hostd",
        test: "e2e",
        filter: "screen_stream",
        serial: false,
    },
];

const INPUT: &[Suite] = &[Suite {
    gate: Some("SLOPTY_INPUT_E2E"),
    package: "slopty-input",
    test: "inject",
    filter: "",
    serial: false,
}];

const IOS: &[Suite] = &[
    Suite {
        gate: Some("SLOPTY_IOS_E2E"),
        package: "slopty-e2e",
        test: "ios",
        filter: "",
        serial: false,
    },
    // The UIKit-boundary scenarios share the one simulator: one app at a time.
    Suite {
        gate: Some("SLOPTY_IOS_E2E"),
        package: "slopty-e2e",
        test: "ios_uikit",
        filter: "",
        serial: true,
    },
];

const SMOOTH: &[Suite] = &[Suite {
    gate: Some("SLOPTY_SMOOTH_E2E"),
    package: "slopty-e2e",
    test: "smooth",
    filter: "on_the_mac",
    serial: true,
}];

const PAIR: &[Suite] = &[Suite {
    gate: Some("SLOPTY_PAIR_E2E"),
    package: "slopty-e2e",
    test: "pair",
    filter: "on_the_mac",
    serial: true,
}];

const PAIR_IOS: &[Suite] = &[Suite {
    gate: Some("SLOPTY_PAIR_IOS_E2E"),
    package: "slopty-e2e",
    test: "pair",
    filter: "with_the_simulator",
    serial: true,
}];

const HOSTS: &[Suite] = &[Suite {
    gate: Some("SLOPTY_HOST2_E2E"),
    package: "slopty-e2e",
    test: "hosts",
    filter: "",
    serial: true,
}];

const SMOOTH_IOS: &[Suite] = &[Suite {
    gate: Some("SLOPTY_SMOOTH_IOS_E2E"),
    package: "slopty-e2e",
    test: "smooth",
    filter: "on_the_simulator",
    serial: true,
}];

pub fn run(sh: &Shell, opts: &E2eOpts) -> Result<()> {
    let suites: Vec<&Suite> = match opts.case {
        Case::App => APP.iter().collect(),
        Case::Host => HOST.iter().collect(),
        Case::Screen => SCREEN.iter().collect(),
        Case::Input => INPUT.iter().collect(),
        Case::Smooth => SMOOTH.iter().collect(),
        Case::SmoothIos => SMOOTH_IOS.iter().collect(),
        Case::Pair => PAIR.iter().collect(),
        Case::PairIos => PAIR_IOS.iter().collect(),
        Case::Hosts => HOSTS.iter().collect(),
        Case::All => {
            APP.iter().chain(HOST).chain(SCREEN).chain(INPUT).chain(SMOOTH).chain(PAIR).collect()
        }
        Case::Ios => IOS.iter().collect(),
    };
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| sh.current_dir().join("target/e2e").to_string_lossy().into_owned());
    std::fs::create_dir_all(format!("{data_dir}/run"))?;
    println!("▶ e2e {:?} (data dir {data_dir})", opts.case);

    let artifacts = format!("{data_dir}/artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let bin_dir = sh.current_dir().join("target/debug");
    // Daemons the tests spawn get their own sockets and state; nothing installed is touched.
    let _env = [
        sh.push_env("SLOPTY_DATA_DIR", &data_dir),
        sh.push_env("SLOPTY_PTYD_SOCKET", format!("{data_dir}/run/ptyd.sock")),
        sh.push_env("SLOPTY_HOSTD_SOCKET", format!("{data_dir}/run/hostd.sock")),
        sh.push_env("SLOPTY_E2E_BIN_DIR", &bin_dir),
        sh.push_env("SLOPTY_E2E_ARTIFACTS", &artifacts),
        sh.push_env("RUST_LOG", &opts.log),
    ];
    let _accept = if opts.accept_all {
        Some(sh.push_env("SLOPTY_E2E_ACCEPT", "all"))
    } else if opts.accept {
        Some(sh.push_env("SLOPTY_E2E_ACCEPT", "changed"))
    } else {
        None
    };

    // Build every binary a suite may spawn up front, so a test never shells out to cargo.
    // `slopty-e2e` is in there for its own helper binaries (the idle window).
    // The app carries the `e2e` feature (renderer access for `Render`). `--bins`, not
    // `--bin slopty-app`: a `--bin` filter applies to every selected package and would leave
    // the daemons stale.
    step(
        "build daemons and app",
        &cmd!(
            sh,
            "cargo build -p slopty-ptyd -p slopty-hostd -p slopty-cli -p slopty -p slopty-e2e --bins --features slopty/e2e"
        ),
    )?;

    // The iOS case also needs the app in a booted simulator; the test launches it there.
    let _simulator_env = matches!(opts.case, Case::Ios | Case::SmoothIos | Case::PairIos)
        .then(|| -> Result<_> {
            let ios_opts = crate::ios::IosOpts::for_e2e(opts.sim, &opts.log);
            let udid = crate::ios::install_on_simulator(sh, &ios_opts)?;
            Ok((
                sh.push_env("SLOPTY_SIM_UDID", udid),
                sh.push_env("SLOPTY_SIM_BUNDLE_ID", crate::ios::BUNDLE_ID),
            ))
        })
        .transpose()?;

    let mut failed = Vec::new();
    for suite in &suites {
        let _gate = suite.gate.map(|gate| sh.push_env(gate, "1"));
        let (package, test) = (suite.package, suite.test);
        let filter: &[&str] = if suite.filter.is_empty() { &[] } else { &[suite.filter] };
        let threads: &[&str] = if suite.serial { &["--test-threads", "1"] } else { &[] };
        let title = format!("{package} {test} {}", suite.filter);
        let command = cmd!(
            sh,
            "cargo nextest run -p {package} --test {test} --no-capture --no-fail-fast {threads...} {filter...}"
        );
        if step(title.trim(), &command).is_err() {
            failed.push(title);
        }
    }
    if failed.is_empty() {
        println!(
            "✔ e2e {:?}: {} suite(s) passed; renders under {artifacts}",
            opts.case,
            suites.len()
        );
        Ok(())
    } else {
        bail!("e2e {:?}: failed {}", opts.case, failed.join(", "))
    }
}
