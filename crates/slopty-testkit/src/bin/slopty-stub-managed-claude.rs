//! A stand-in for a managed launcher in `claude`'s place, for tests of how a worker treats one
//! (`docs/decisions/claude-code.md`, "A managed `claude`"). It keeps the launcher's observable
//! contract and nothing else: no control plane, no account, no network.
//!
//! - `--managed-help` prints the launcher's usage and succeeds, before anything else.
//! - Any other run starts the newest client under
//!   `$HOME/.local/share/claude-managed/artifacts/<version>/<platform>/claude` as its **child**,
//!   with `--bare` dropped (before a `--` only), the credential, endpoint, TLS and loader names and
//!   every `SCC_*` name taken out of its environment, and `DISABLE_TELEMETRY=1` and
//!   `SCC_MANAGED_ARTIFACT` (the client's path) put in. It waits for the client and exits as it
//!   did, so the terminal's foreground process stays the launcher.
//!
//! Every run appends one JSON line to `STUB_MANAGED_LOG`: its pid, its arguments, and the
//! client's pid once it started one. A test reads it to see what a worker asked of `claude`.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a terminal program: its usage and its errors are its screen"
)]

use std::error::Error;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::json;

type Fallible<T> = Result<T, Box<dyn Error>>;

/// What the launcher prints for `--managed-help`.
const USAGE: &str = "Usage: claude managed <COMMAND>\n\nCommands:\n  login   Sign in to the managed fleet\n  status  Show this device's enrolment\n";

/// Names the launcher takes out of the client's environment, beside every `SCC_*` one.
const STRIPPED: [&str; 16] = [
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CLAUDE_CODE_SIMPLE",
    "CLAUDE_CODE_EXTRA_BODY",
    "CLAUDE_CODE_GB_DISK_CACHE_WHEN_TELEMETRY_OFF",
    "NODE_OPTIONS",
    "DYLD_INSERT_LIBRARIES",
    "LD_PRELOAD",
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--managed-help") {
        log(&args, None);
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match launch(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            log(&args, None);
            eprintln!("stub managed claude: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Start the newest client with `args`, wait for it, and say how it exited.
fn launch(args: &[String]) -> Fallible<u8> {
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    let client = newest_client(&Path::new(&home).join(".local/share/claude-managed/artifacts"))?;
    let mut command = Command::new(&client);
    command.args(without_bare(args));
    for (name, _) in std::env::vars_os() {
        let text = name.to_string_lossy();
        if text.starts_with("SCC_") || STRIPPED.contains(&text.as_ref()) {
            command.env_remove(&name);
        }
    }
    command.env("DISABLE_TELEMETRY", "1").env("SCC_MANAGED_ARTIFACT", &client);
    let mut child = command.spawn()?;
    log(args, Some(child.id()));
    let status = child.wait()?;
    Ok(status.code().and_then(|code| u8::try_from(code).ok()).unwrap_or(1))
}

/// `args` without `--bare`, which the launcher drops before a `--`.
fn without_bare(args: &[String]) -> Vec<String> {
    let mut past = false;
    args.iter()
        .filter(|arg| {
            past |= *arg == "--";
            past || *arg != "--bare"
        })
        .cloned()
        .collect()
}

/// The client of the newest version under `artifacts`, by number.
fn newest_client(artifacts: &Path) -> Fallible<PathBuf> {
    let number = |name: &str| -> Option<Vec<u64>> {
        name.split('.').map(|part| part.parse().ok()).collect()
    };
    let newest = std::fs::read_dir(artifacts)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter_map(|name| Some((number(&name)?, name)))
        .max()
        .ok_or("no client under the artifacts")?
        .1;
    let platforms = std::fs::read_dir(artifacts.join(newest))?;
    platforms
        .filter_map(Result::ok)
        .map(|platform| platform.path().join("claude"))
        .find(|client| client.is_file())
        .ok_or_else(|| "no client for any platform".into())
}

/// One line in `STUB_MANAGED_LOG`, when it is set.
fn log(args: &[String], client: Option<u32>) {
    let Some(at) = std::env::var_os("STUB_MANAGED_LOG") else { return };
    let line = json!({ "pid": std::process::id(), "argv": args, "client": client });
    let file = std::fs::OpenOptions::new().create(true).append(true).open(at);
    if let Ok(mut file) = file {
        let _written = writeln!(file, "{line}");
    }
}
