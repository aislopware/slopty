//! `slopty` — the command-line face of Slopty.
//!
//! * `slopty workers|terminals|open|send|wait|…` drive workers through the server, one verb each
//!   (`slopty_proto::orchestration`), as text or `--json`.
//! * `slopty mcp` is the same verbs as an MCP server on stdio, for an AI agent.
//! * `slopty project …` and `slopty task …` make and follow projects: a goal split into tasks whose
//!   agents the server places across the workers.
//! * `slopty server …` runs `slopty-server` as a `LaunchAgent` or a systemd user unit, and `slopty
//!   server relay` says whether its machine is a Tailscale peer relay.
//! * `slopty wake <worker>` wakes a sleeping worker from its own LAN.
//! * `slopty worker …` talks to the local `slopty-worker` over its control socket.
//! * `slopty worker deploy <ssh target>` puts a worker on another machine over `ssh`.
//! * `slopty hook` is the Claude Code hook relay (`slopty hook install` registers it).
//! * `slopty ssh` is `ssh` with a terminal the far side knows; a Slopty shell's `ssh` runs it.
//! * `slopty browse` and `slopty edit` hand a shell's web pages and files to the client in front of
//!   it; a Slopty session's `BROWSER`, `EDITOR` and `open` are this binary under other names.
//! * `slopty add <host[:port]>` remembers a worker (today's `slopty-worker`) by its address.
//! * `slopty sessions|attach` are a real client over QUIC straight to a worker: a raw-mode terminal
//!   that renders frames locally. It is the reference client for latency measurements and works
//!   before (and without) the GPUI apps.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "a CLI; stdout is its UI")]
#![forbid(unsafe_code)]

mod attach;
mod bench;
mod client;
mod clipboard;
mod deploy;
mod handoff;
mod hook;
mod link;
mod mcp;
mod projects;
mod relay;
mod service;
mod statusline;
mod verbs;
mod workerctl;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty", version, about)]
struct Cli {
    /// Data directory (default: `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty` or
    /// `$XDG_DATA_HOME/slopty`).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// The server, `host[:port]` (default: `$SLOPTY_SERVER`, else `server` under `[client]` in
    /// settings.toml, else the first that answers on the tailnet). `worker install` saves it as
    /// the server this Mac registers with.
    #[arg(long, global = true)]
    server: Option<String>,
    /// Print the answer as JSON.
    #[arg(long, global = true)]
    json: bool,
    /// For a verb that changes something: a name for its effect, such as a UUID. Run again
    /// with the same key, the command prints what the first run did instead of doing it twice.
    /// A fresh key covers this run's own retries when omitted.
    #[arg(long, global = true, value_name = "KEY")]
    idempotency_key: Option<slopty_proto::orchestration::IdempotencyKey>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    #[command(flatten)]
    Verb(verbs::VerbCmd),
    /// Serve the verbs to an AI agent over MCP on stdio.
    #[command(after_help = mcp::REGISTER_HELP)]
    Mcp,
    /// Run the server as a `LaunchAgent`.
    Server {
        #[command(subcommand)]
        cmd: service::ServerCmd,
    },
    /// Control the local worker daemon.
    Worker {
        #[command(subcommand)]
        cmd: workerctl::WorkerCmd,
    },
    /// `ssh` with a terminal the far side knows: Slopty's terminfo entry is installed on the
    /// host once, else the session gets xterm-256color. A Slopty shell's `ssh` runs this.
    Ssh {
        /// The `ssh` to run.
        #[arg(long, default_value = "ssh")]
        ssh: PathBuf,
        /// `ssh`'s own arguments, as typed (after `--`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Open web pages in the browser of the client in front of this shell (a Slopty session's
    /// `BROWSER`); anything but a web address goes to this machine's opener.
    Browse {
        /// `http` or `https` addresses.
        #[arg(required = true)]
        targets: Vec<String>,
    },
    /// Show a file in a file tile of the client in front of this shell; with `--wait`, return
    /// once you are done with it (a Slopty session's `EDITOR` is this with `--wait`).
    Edit {
        /// Return once the tile is done with: 0, or 1 when the edit was given up.
        #[arg(long)]
        wait: bool,
        /// `[+line] <file>`.
        #[arg(required = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Claude Code hook relay: forwards the hook on stdin to the worker daemon (exits 0 always).
    Hook {
        #[command(subcommand)]
        cmd: Option<hook::HookCmd>,
    },
    /// The app's `settings.toml`.
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// This machine's latest crash reports, newest first: every Slopty process's panics and
    /// fatal signals, with macOS's own reports of them.
    Crashes {
        /// How many.
        #[arg(long, default_value_t = 10)]
        last: usize,
        /// Every frame, not only the first few.
        #[arg(long)]
        all_frames: bool,
    },
    /// Add a worker by its address: a Tailscale `MagicDNS` name, a LAN name or an IP, with an
    /// optional `:port`.
    Add {
        /// `host[:port]`.
        address: String,
    },
    /// Forget a worker.
    Forget {
        /// Worker name, address or id prefix.
        worker: String,
    },
    /// List sessions on a worker.
    Sessions {
        /// Worker name, address or id prefix, or any `host[:port]` (the only worker when
        /// omitted).
        #[arg(long)]
        worker: Option<String>,
    },
    /// Measure application round-trip time to a worker (control-stream ping).
    Ping {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Number of probes.
        #[arg(long, default_value_t = 20)]
        count: u32,
    },
    /// Measure a screen stream (fps, latency on loopback, loss/FEC/NACK counters).
    Bench {
        #[command(subcommand)]
        cmd: BenchCmd,
    },
    /// Attach to a session straight on its worker, or open one and attach when no session is
    /// given. Detach with `^]`.
    Attach {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Session id prefix.
        session: Option<String>,
        /// Working directory for a new session.
        #[arg(long, conflicts_with = "session")]
        cwd: Option<String>,
        /// Program and arguments for a new session, after `--` (the login shell when omitted).
        #[arg(last = true, conflicts_with = "session")]
        command: Vec<String>,
    },
}

/// `slopty settings …`.
#[derive(Subcommand, Debug)]
enum SettingsCmd {
    /// Print where the app reads its settings from.
    Path,
    /// Write a commented default file there unless one exists.
    Init,
}

/// Benchmarks.
#[derive(Subcommand, Debug)]
enum BenchCmd {
    /// Keystroke round trip: a byte to a `cat` session on the worker, timed to the first frame
    /// back.
    Echo {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Samples.
        #[arg(long, default_value_t = 30)]
        count: u32,
    },
    /// Stream a window or display and report what arrived.
    // Apple only: this side decodes with VideoToolbox (`docs/decisions/platform.md`).
    #[cfg(target_vendor = "apple")]
    Screen {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Print the worker's windows and displays instead of streaming.
        #[arg(long)]
        list: bool,
        /// Window id (see `--list`).
        #[arg(long)]
        window: Option<u32>,
        /// Display id (see `--list`); the main display when neither is given.
        #[arg(long)]
        display: Option<u32>,
        /// Run length in seconds.
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Capture scale, 1.0 = native.
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Frame rate cap.
        #[arg(long, default_value_t = 60)]
        fps: u16,
        /// Bitrate in Mbit/s.
        #[arg(long, default_value_t = 30)]
        mbit: u32,
        /// Fail the run when the receiver counted more stalls than this. The self-check for a
        /// quiet loopback stream, where the answer is zero.
        #[arg(long)]
        max_stalls: Option<u64>,
    },
}

fn main() -> Result<ExitCode> {
    slopty_crash::install(slopty_crash::Process::Cli, &slopty_platform::dirs::data_dir());
    // `attach` and the benches carry keys and echoes through every runtime thread; unclassed,
    // a loaded Mac held one for hundreds of milliseconds (MEASUREMENTS.md, "the keystroke path
    // under an all-core spin").
    slopty_platform::user_interactive_thread();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(slopty_platform::user_interactive_thread)
        .build()?
        .block_on(run())
}

async fn run() -> Result<ExitCode> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Some(done) = handoff::by_name().await {
        return done;
    }
    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(slopty_platform::dirs::data_dir);
    let done = match cli.cmd {
        Cmd::Worker { cmd } => {
            workerctl::run(cmd, cli.server.as_deref(), &data_dir, cli.json).await
        }
        Cmd::Ssh { ssh, args } => {
            let opts =
                slopty_pty::ssh::Options { ssh, ..slopty_pty::ssh::Options::from_env(&data_dir) };
            let (mut ssh, _decision) =
                slopty_pty::ssh::prepare(&opts, &args, &mut |note| eprintln!("slopty: {note}"))
                    .await;
            let e = std::os::unix::process::CommandExt::exec(&mut ssh);
            return Err(anyhow::anyhow!("run {}: {e}", opts.ssh.display()));
        }
        Cmd::Browse { targets } => return handoff::browse(&data_dir, &targets).await,
        Cmd::Edit { wait, args } => return handoff::edit(&data_dir, wait, args).await,
        Cmd::Hook { cmd: None } => {
            hook::relay(&data_dir).await;
            Ok(())
        }
        Cmd::Hook { cmd: Some(cmd) } => hook::run(cmd, &data_dir).await,
        Cmd::Crashes { last, all_frames } => crashes(&data_dir, last, all_frames, cli.json),
        Cmd::Settings { cmd } => {
            let path = slopty_settings::path_in(&data_dir);
            match cmd {
                SettingsCmd::Path => println!("{}", path.display()),
                SettingsCmd::Init => {
                    if slopty_settings::Settings::init(&path)? {
                        println!("wrote {}", path.display());
                    } else {
                        println!("{} already exists; left as is", path.display());
                    }
                }
            }
            Ok(())
        }
        Cmd::Verb(cmd) => {
            let key = cli.idempotency_key;
            verbs::run(cmd, cli.server.as_deref(), &data_dir, cli.json, key).await
        }
        Cmd::Mcp => Box::pin(mcp::run(cli.server.as_deref(), &data_dir)).await,
        Cmd::Server { cmd } => service::server(cmd, &data_dir).await,
        Cmd::Add { address } => client::add(&data_dir, &address).await,
        Cmd::Forget { worker } => client::forget(&data_dir, &worker),
        Cmd::Sessions { worker } => client::sessions(&data_dir, worker.as_deref()).await,
        Cmd::Attach { worker, session: Some(session), .. } => {
            return attach::attach(&data_dir, worker.as_deref(), &session).await;
        }
        Cmd::Attach { worker, session: None, cwd, command } => {
            return attach::open(&data_dir, worker.as_deref(), cwd, command).await;
        }
        Cmd::Ping { worker, count } => client::ping(&data_dir, worker.as_deref(), count).await,
        Cmd::Bench { cmd: BenchCmd::Echo { worker, count } } => {
            bench::echo(&data_dir, worker.as_deref(), count).await
        }
        #[cfg(target_vendor = "apple")]
        Cmd::Bench { cmd: BenchCmd::Screen { worker, list, .. } } if list => {
            bench::screen::list(&data_dir, worker.as_deref()).await
        }
        #[cfg(target_vendor = "apple")]
        Cmd::Bench {
            cmd:
                BenchCmd::Screen {
                    worker, window, display, seconds, scale, fps, mbit, max_stalls, ..
                },
        } => {
            let spec = bench::screen::ScreenBench {
                window,
                display,
                seconds,
                scale,
                fps,
                mbit,
                max_stalls,
            };
            bench::screen::screen(&data_dir, worker.as_deref(), spec).await
        }
    };
    done.map(|()| ExitCode::SUCCESS)
}

/// `slopty crashes`: the latest reports, each with its first frames.
fn crashes(data_dir: &std::path::Path, last: usize, all_frames: bool, json: bool) -> Result<()> {
    let reports = slopty_crash::reports(data_dir);
    let shown = reports.get(..last.min(reports.len())).unwrap_or_default();
    if json {
        println!("{}", serde_json::to_string_pretty(shown)?);
        return Ok(());
    }
    if shown.is_empty() {
        println!("no crash reports in {}", slopty_crash::crash_dir(data_dir).display());
    }
    for report in shown {
        println!(
            "{}  {} (pid {})  {}",
            report.when(),
            report.process,
            report.pid,
            report.headline()
        );
        if let Some(thread) = &report.thread {
            println!("  thread {thread}");
        }
        let frames = if all_frames { report.frames.len() } else { 8 };
        for frame in report.frames.iter().take(frames) {
            println!("    {}", frame.describe());
        }
        println!("  {}", report.path.display());
        if let Some(ips) = report.ips.as_ref().filter(|ips| **ips != report.path) {
            println!("  macOS's report: {}", ips.display());
        }
        println!();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    #[test]
    fn the_command_line_is_well_formed() {
        super::Cli::command().debug_assert();
    }

    /// The one `--data-dir` reaches every command, before or after its name.
    #[test]
    fn the_data_dir_is_global() {
        use clap::Parser as _;
        for args in [
            &["slopty", "--data-dir", "/d", "worker", "status"][..],
            &["slopty", "worker", "uninstall", "--data-dir", "/d"],
            &["slopty", "hook", "--data-dir", "/d"],
        ] {
            let cli = super::Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.data_dir.as_deref(), Some(std::path::Path::new("/d")), "{args:?}");
        }
    }

    /// `ssh`'s own flags reach `slopty ssh` as typed, with or without the `--` the shells put.
    #[test]
    fn ssh_takes_its_arguments_as_typed() {
        use clap::Parser as _;
        for args in [
            &["slopty", "ssh", "--", "-p", "2222", "box", "-t", "htop"][..],
            &["slopty", "ssh", "box", "-t", "htop"],
        ] {
            let cli = super::Cli::try_parse_from(args).unwrap();
            let super::Cmd::Ssh { args: got, .. } = cli.cmd else { panic!("{args:?}") };
            let typed: Vec<&str> = args.iter().skip(2).filter(|a| **a != "--").copied().collect();
            assert_eq!(got, typed, "{args:?}");
        }
    }
}
