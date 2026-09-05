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
    /// All of the above.
    All,
}

/// Options.
#[derive(Args, Debug)]
pub struct E2eOpts {
    /// Which tests.
    #[arg(value_enum, default_value_t = Case::App)]
    case: Case,
    /// Rewrite the render goldens under `crates/slopty-e2e/golden/` from this run.
    #[arg(long)]
    accept: bool,
    /// Data directory for the daemons the tests spawn (default `target/e2e`).
    #[arg(long)]
    data_dir: Option<String>,
    /// `RUST_LOG` for the daemons and tests.
    #[arg(long, default_value = "info")]
    log: String,
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
}

const APP: &[Suite] =
    &[Suite { gate: Some("SLOPTY_APP_E2E"), package: "slopty-e2e", test: "app", filter: "" }];

const HOST: &[Suite] =
    &[Suite { gate: None, package: "slopty-hostd", test: "e2e", filter: "shell_round_trip" }];

const SCREEN: &[Suite] = &[
    Suite {
        gate: Some("SLOPTY_SCREEN_E2E"),
        package: "slopty-capture",
        test: "geometry",
        filter: "",
    },
    Suite { gate: Some("SLOPTY_SCREEN_E2E"), package: "slopty-host", test: "screen", filter: "" },
    Suite {
        gate: Some("SLOPTY_SCREEN_E2E"),
        package: "slopty-hostd",
        test: "e2e",
        filter: "screen_stream",
    },
];

const INPUT: &[Suite] = &[Suite {
    gate: Some("SLOPTY_INPUT_E2E"),
    package: "slopty-input",
    test: "inject",
    filter: "",
}];

pub fn run(sh: &Shell, opts: &E2eOpts) -> Result<()> {
    let suites: Vec<&Suite> = match opts.case {
        Case::App => APP.iter().collect(),
        Case::Host => HOST.iter().collect(),
        Case::Screen => SCREEN.iter().collect(),
        Case::Input => INPUT.iter().collect(),
        Case::All => APP.iter().chain(HOST).chain(SCREEN).chain(INPUT).collect(),
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
    let _accept = opts.accept.then(|| sh.push_env("SLOPTY_E2E_ACCEPT", "1"));

    // Build every binary a suite may spawn up front, so a test never shells out to cargo.
    // The app carries the `e2e` feature (renderer access for `Render`).
    step(
        "build daemons and app",
        &cmd!(
            sh,
            "cargo build -p slopty-ptyd -p slopty-hostd -p slopty --bin slopty-app --features slopty/e2e"
        ),
    )?;

    let mut failed = Vec::new();
    for suite in &suites {
        let _gate = suite.gate.map(|gate| sh.push_env(gate, "1"));
        let (package, test) = (suite.package, suite.test);
        let filter: &[&str] = if suite.filter.is_empty() { &[] } else { &[suite.filter] };
        let title = format!("{package} {test} {}", suite.filter);
        let command = cmd!(
            sh,
            "cargo nextest run -p {package} --test {test} --no-capture --no-fail-fast {filter...}"
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
