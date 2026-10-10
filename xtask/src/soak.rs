//! `cargo xtask soak`: the real daemons under a synthetic workload, watched for growth.
//!
//! It starts `slopty-server`, `slopty-ptyd` and `slopty-worker` from a temporary HOME, with the
//! worker registered with the server, and drives them through the `slopty` CLI the way an agent
//! does, cycle after cycle for `--seconds`:
//! - open a quiet bash, flood it (`seq 1 20000`) and wait for the end of the output;
//! - play a hook through `slopty hook report` (working, then idle), as a wrapped agent would;
//! - read the terminal's output as a second reader;
//! - close it.
//!
//! Before the load, [`FILL_CYCLES`] cycles, [`FILL_LANES`] at a time, fill the daemons' bounded
//! stores, so the load's slope sees only what grows without bound. A store still filling grows
//! as steadily as a leak, and a minute of load cannot tell them apart. What the fill cost each
//! daemon is reported beside the slope, and the peak budget still bounds it.
//!
//! Every `--interval` it samples each daemon's physical footprint (`ri_phys_footprint`), open
//! descriptors and threads (`slopty_testkit::process`). It fails when
//! - a daemon's footprint grows faster than [`SLOPE_KIB_PER_MIN`] by the Theil–Sen slope over the
//!   load after its first [`UNJUDGED_SECONDS`], judged once that spans [`SLOPE_WINDOW_SECONDS`] (a
//!   shorter load prints its slope as not judged);
//! - a daemon's peak footprint passes its [`PEAK_MIB`] budget;
//! - a daemon holds more descriptors or threads after the load has settled than before it;
//! - `leaks <pid>` finds a leak in a daemon at the end (exit 1; `man leaks`);
//! - a hook report fails, or a CLI call cannot reach the server (it is sent again once, so the load
//!   goes on).
//!
//! Beside the terminals, `--stream-lanes` lanes (one by default) stream the worker's drawn screen
//! (`SLOPTY_SYNTHETIC_SCREEN`: one display and two windows, drawn and encoded by VideoToolbox as
//! a captured one would be, with no Screen Recording grant) through the fill and the load. Each
//! stream is a `slopty-probe screen` run straight to the worker for [`STREAM_SECONDS`]: open,
//! decode, close. They take the display and the windows in turn, at each of [`STREAM_SCALES`] in
//! turn, so every open builds the worker's capture and encoder and the client's decoder at
//! another size, and the fill's sessions and the load's streams overlap. The soak also fails when
//! - a stream fails to open or run, or decodes no frame;
//! - a stream reports a decode error, or loses more than [`LOST_PERMILLE`] of its frames on
//!   loopback, where nothing drops them;
//! - the worker's own counters show it dropped more than [`DROPPED_PERMILLE`] of what it captured.
//!
//! Nothing is captured from the Mac, played or shown.
//!
//! `leaks` cannot read a hardened-runtime binary, and `cargo xtask sign` signs the dev daemons
//! with the hardened runtime. So the daemons are copied under `target/deep/soak/bin` and signed
//! there ad hoc, without the runtime and with `get-task-allow`, which the build tree never sees.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::Utf8PathBuf;
use clap::Args;
use serde_json::{Value, json};
use slopty_testkit::process;
use xshell::{Shell, cmd};

use crate::tools::{repo_root, step};

/// Growth a daemon's footprint may show over the load, in KiB a minute.
const SLOPE_KIB_PER_MIN: f64 = 64.0;

/// The first stretch of load, in seconds, that the slope leaves out.
///
/// With a stream lane, the worker's small-allocation pages climb 5 to 6 MiB to their high-water
/// mark over its first five to seven minutes of streaming, the fill's three included, and hold
/// there for the rest of a 30-minute load. Its live heap did not grow over the same stretch
/// (`malloc_history`): the allocator keeps the pages the most streams and sessions at once
/// needed. The climb ends about 400 s into the load.
const UNJUDGED_SECONDS: f64 = 300.0;

/// The shortest stretch of load, in seconds, whose footprint slope is judged against
/// [`SLOPE_KIB_PER_MIN`].
///
/// The allocator takes and gives back its regions in steps of 16 KiB to 2.5 MiB, ptyd's
/// footprint wanders by 1 MiB, and the worker's by 18 to 35 MiB of `IOSurface` as streams open
/// and close. So a footprint with no growth under it still moves. Over seven soaks, most of them
/// run three at once, the largest Theil–Sen slope of any window starting [`UNJUDGED_SECONDS`] or
/// more into the load was 179 KiB/min over 600 s, and 47 over 900 s and 32 over 1 200 s in the
/// two 30-minute ones, all of it the worker's with a stream lane (MEASUREMENTS, "the soak's
/// footprint slope against its window"). From 900 s it stays within the budget.
const SLOPE_WINDOW_SECONDS: f64 = 900.0;

/// The largest footprint each daemon may reach, in MiB: a few times the first soak's peaks
/// (server 4, ptyd 24, worker 28; MEASUREMENTS, "the first soak").
const PEAK_MIB: [(&str, u64); 3] = [("server", 32), ("ptyd", 64), ("worker", 128)];

/// What each stream lane may add to the worker's peak, in MiB: a stream's capture surfaces and its
/// encoder's pool. The first stream soak put the worker's peak 42 MiB over the terminals' alone
/// (104 to 146 MiB; MEASUREMENTS, "Deep checks widened, a stream soak, a loom model").
const STREAM_PEAK_MIB: u64 = 64;

/// How long the daemons are left alone before a count is compared. Tokio's blocking pool ends a
/// thread idle for 10 s, and those threads' exits can hand the system's dispatch threads work,
/// which end once idle for about 5 s (of eight started at once, seven ended by 5.5 s and one
/// stayed, as one does after any dispatch work; measured with `task_threads`).
/// At 12 s two soaks of seven caught one such dispatch thread still alive (MEASUREMENTS, "the
/// soak's footprint slope against its window").
const SETTLE: Duration = Duration::from_secs(20);

/// How long one CLI call may take.
const CALL: Duration = Duration::from_secs(60);

/// What the CLI says when its dial to the server failed (`apps/slopty-cli/src/link.rs`).
const UNREACHED: &str = "cannot reach the server";

/// Lines each cycle's flood prints.
const FLOOD_LINES: u32 = 20_000;

/// Cycles that fill the daemons' bounded stores before the baseline. The largest fill at 1 024
/// cycles: the server's event log keeps 4 096 events (`slopty_server::hub::EVENT_LOG`) and a
/// cycle logs four (opened, working, idle, closed); the worker's idempotency ledger keeps 4 096
/// keys and a cycle sends four keyed verbs (open, send, wait, close). The rest is margin, so the
/// samples show the footprint level off before the baseline is taken.
const FILL_CYCLES: u32 = 1_536;

/// Fill cycles run at once: few enough that the floods running together stay inside the
/// worker's peak budget (eight at once peaked at 173 MiB).
const FILL_LANES: usize = 4;

/// Seconds each stream of a stream lane runs before it is closed and the next one opened.
const STREAM_SECONDS: u64 = 3;

/// The capture scales a stream lane opens at, in turn: each builds the capture, the encoder and
/// the decoder at another size.
const STREAM_SCALES: [&str; 3] = ["1.0", "0.75", "0.5"];

/// Frames a stream may lose, per thousand it decoded, over the soak: loopback drops nothing, so
/// a loss is the worker or the client falling behind.
const LOST_PERMILLE: u64 = 5;

/// Frames the worker may drop, per thousand it captured, over the soak.
const DROPPED_PERMILLE: u64 = 50;

#[derive(Args, Debug, Clone)]
pub struct SoakOpts {
    /// How long the load runs, in seconds. The slope is judged over the load after its first
    /// [`UNJUDGED_SECONDS`], and only once that spans [`SLOPE_WINDOW_SECONDS`].
    #[arg(long, default_value_t = 1200)]
    pub seconds: u64,
    /// Seconds between samples.
    #[arg(long, default_value_t = 2)]
    pub interval: u64,
    /// Soak the debug build instead of the release one.
    #[arg(long)]
    pub debug: bool,
    /// Soak what the last build left in the profile's directory, without calling cargo: a rerun
    /// that does not wait on the other sessions' builds, or on a crate someone is halfway
    /// through.
    #[arg(long)]
    pub no_build: bool,
    /// Give the daemons `MallocStackLogging`, so `leaks` shows where a leak was allocated (the
    /// footprint then includes the log, so its slope and peak read high).
    #[arg(long)]
    pub stacks: bool,
    /// Lanes streaming the worker's drawn screen through the fill and the load, one stream after
    /// another; 0 soaks the terminals alone.
    #[arg(long, default_value_t = 1)]
    pub stream_lanes: usize,
    /// Where the samples, logs and summary go (default `target/deep/soak/last`).
    #[arg(long)]
    pub out: Option<Utf8PathBuf>,
}

/// The binaries, as the build names them.
const BINARIES: [&str; 5] =
    ["slopty-server", "slopty-ptyd", "slopty-worker", "slopty", "slopty-probe"];

pub fn run(sh: &Shell, opts: &SoakOpts) -> Result<()> {
    let root = repo_root()?;
    let out = opts.out.clone().unwrap_or_else(|| root.join("target/deep/soak/last"));
    if out.exists() {
        std::fs::remove_dir_all(&out).with_context(|| format!("clearing {out}"))?;
    }
    std::fs::create_dir_all(&out).with_context(|| format!("creating {out}"))?;
    let bin = build(sh, opts.debug, opts.no_build)?;
    let mut stack = Stack::start(&bin, out.as_std_path(), opts.stacks)?;
    let result = soak(&stack, opts, out.as_std_path());
    stack.stop();
    let summary = result?;
    let failures: Vec<String> = summary
        .get("failures")
        .and_then(Value::as_array)
        .map(|f| f.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    ensure!(failures.is_empty(), "soak failed:\n  {}", failures.join("\n  "));
    println!("✔ soak held for {} s ({out}/summary.json)", opts.seconds);
    Ok(())
}

/// Build the daemons and the CLI, and copy them where they are signed to be read by `leaks`.
fn build(sh: &Shell, debug: bool, no_build: bool) -> Result<Utf8PathBuf> {
    let (flags, profile): (&[&str], &str) =
        if debug { (&[], "debug") } else { (&["--release"], "release") };
    if !no_build {
        step(
            "build the daemons and the CLI",
            &cmd!(
                sh,
                "nice -n 10 cargo build {flags...} -p slopty-serverd -p slopty-ptyd -p slopty-workerd -p slopty-cli"
            ),
        )?;
    }
    let root = repo_root()?;
    let dir = root.join("target/deep/soak/bin");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {dir}"))?;
    let entitlements = root.join("target/deep/soak/debuggable.entitlements");
    std::fs::write(
        &entitlements,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\
         <key>com.apple.security.get-task-allow</key><true/></dict></plist>\n",
    )?;
    for name in BINARIES {
        let built = root.join("target").join(profile).join(name);
        replace_signed(built.as_std_path(), dir.join(name).as_std_path(), |staged| {
            cmd!(sh, "codesign --force --sign - --entitlements {entitlements} {staged}")
                .quiet()
                .ignore_stderr()
                .run()
                .with_context(|| format!("signing {} for leaks", staged.display()))
        })?;
    }
    Ok(dir)
}

/// Put a copy of `built`, signed by `sign`, at `copy` by renaming it over the old one.
///
/// Another soak of this tree may be running these files. A file written in place keeps its
/// inode, so a running daemon's code pages change under it and a CLI started between the write
/// and the signature runs unsigned code, and macOS kills both (`SIGKILL`). A rename leaves the
/// old file whole for whoever runs it.
fn replace_signed(built: &Path, copy: &Path, sign: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let name = copy.file_name().context("a binary's file name")?.to_string_lossy();
    let staged = copy.with_file_name(format!(".{name}.{}", std::process::id()));
    let placed = std::fs::copy(built, &staged)
        .with_context(|| format!("copying {}", built.display()))
        .and_then(|_bytes| sign(&staged))
        .and_then(|()| {
            std::fs::rename(&staged, copy).with_context(|| format!("placing {}", copy.display()))
        });
    if placed.is_err() {
        let _gone = std::fs::remove_file(&staged);
    }
    placed
}

/// The three daemons on a temporary root.
struct Stack {
    root: PathBuf,
    /// Numbers each call's output files, so calls can run at once.
    calls: AtomicU64,
    /// Calls that could not reach the server at the first try, and the first one's error.
    unreached: AtomicU64,
    first_unreached: OnceLock<String>,
    /// Numbers the streams, so the lanes take the targets and scales in turn.
    streams: AtomicUsize,
    slopty_bin: PathBuf,
    /// `slopty-probe`, which streams.
    probe_bin: PathBuf,
    cli: PathBuf,
    server: String,
    /// The worker's own address, which a stream dials without the server.
    worker: String,
    daemons: Vec<(&'static str, Child)>,
}

impl Stack {
    fn start(bin: &Utf8PathBuf, out: &Path, stacks: bool) -> Result<Self> {
        // Short: the sockets live under it, and a Unix socket path holds 104 bytes.
        let root = std::env::temp_dir().join(format!("slopty-soak-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let home = root.join("home");
        let zsh = root.join("zsh");
        for dir in [&home, &zsh, &root.join("server"), &root.join("drops")] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(zsh.join(".zshrc"), "PROMPT='%~ %# '\n")?;
        let terminfo = root.join("terminfo");
        let mut env: Vec<(&str, std::ffi::OsString)> = vec![
            ("HOME", home.into_os_string()),
            ("RUST_LOG", "warn".into()),
            ("ZDOTDIR", zsh.into_os_string()),
            ("SLOPTY_TERMINFO_DIR", terminfo.clone().into_os_string()),
            ("TERMINFO_DIRS", format!("{}:", terminfo.display()).into()),
            // Never the person's clipboard.
            (
                "SLOPTY_PASTEBOARD",
                format!("com.aislopware.slopty.soak.{}", std::process::id()).into(),
            ),
            ("SLOPTY_DROP_DIR", root.join("drops").into_os_string()),
            ("SLOPTY_WORKER_NAME", "soak".into()),
            ("BASH_SILENCE_DEPRECATION_WARNING", "1".into()),
            // The worker streams its drawn screen, never the Mac's.
            ("SLOPTY_SYNTHETIC_SCREEN", "1".into()),
        ];
        if stacks {
            env.push(("MallocStackLogging", "1".into()));
        }
        let mut stack = Self {
            slopty_bin: bin.join("slopty").into_std_path_buf(),
            probe_bin: bin.join("slopty-probe").into_std_path_buf(),
            cli: root.join("cli"),
            root,
            calls: AtomicU64::new(0),
            unreached: AtomicU64::new(0),
            first_unreached: OnceLock::new(),
            streams: AtomicUsize::new(0),
            server: String::new(),
            worker: String::new(),
            daemons: Vec::new(),
        };
        let spawn = |name: &'static str, program: &str, args: &[&str]| -> Result<Child> {
            let log = File::create(out.join(format!("{name}.log")))?;
            Command::new(bin.join(program).as_std_path())
                .args(args)
                .envs(env.iter().map(|(k, v)| (*k, v)))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(log)
                .spawn()
                .with_context(|| format!("spawn {program}"))
        };
        let server_dir = stack.root.join("server");
        let server_dir = server_dir.to_string_lossy();
        let mut server = spawn(
            "server",
            "slopty-server",
            &["--port", "0", "--print-addr", "--data-dir", &server_dir, "--name", "soak-server"],
        )?;
        let printed = first_line(&mut server, out, "server");
        // Held before anything can fail, so the stack's drop kills it.
        stack.daemons.push(("server", server));
        let bound: Value =
            serde_json::from_str(&printed?).context("slopty-server printed no addresses")?;
        let quic: std::net::SocketAddr =
            text(&bound, "quic").parse().context("the server's address")?;
        stack.server = format!("127.0.0.1:{}", quic.port());

        let ptyd_sock = stack.root.join("ptyd.sock");
        let ptyd = spawn("ptyd", "slopty-ptyd", &["--socket", &ptyd_sock.to_string_lossy()])?;
        stack.daemons.push(("ptyd", ptyd));
        wait_for(|| ptyd_sock.exists(), Duration::from_secs(10)).context("slopty-ptyd's socket")?;

        let worker_data = stack.root.join("worker").to_string_lossy().into_owned();
        let ctl = stack.root.join("worker.sock").to_string_lossy().into_owned();
        let server_addr = stack.server.clone();
        let mut worker = spawn(
            "worker",
            "slopty-worker",
            &[
                "--ptyd-socket",
                &ptyd_sock.to_string_lossy(),
                "--ctl-socket",
                &ctl,
                "--data-dir",
                &worker_data,
                "--print-addr",
                "--port",
                "0",
                "--server",
                &server_addr,
            ],
        )?;
        let listening = first_line(&mut worker, out, "worker");
        stack.daemons.push(("worker", worker));
        let listening: std::net::SocketAddr =
            listening?.parse().context("the worker printed no address")?;
        stack.worker = format!("127.0.0.1:{}", listening.port());
        let online = wait_for(
            || {
                stack.slopty(&["workers"]).is_ok_and(|w| {
                    w.as_array().is_some_and(|all| {
                        all.iter()
                            .any(|w| text(w, "name") == "soak" && text(w, "liveness") == "online")
                    })
                })
            },
            Duration::from_secs(30),
        );
        online.context("the worker never came online at the server")?;
        Ok(stack)
    }

    /// `slopty <args> --server <server> --json`, parsed.
    fn slopty(&self, args: &[&str]) -> Result<Value> {
        let out = self.call(args, &[])?;
        serde_json::from_slice(&out)
            .with_context(|| format!("slopty {args:?} printed {:?}", String::from_utf8_lossy(&out)))
    }

    /// [`Self::call_once`], sent again once when it could not reach the server. A dial that
    /// failed sent no verb, so nothing is done twice; the miss is counted as a finding, and the
    /// load is still worth watching.
    fn call(&self, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>> {
        match self.call_once(&self.slopty_bin, args, env) {
            Err(e) if format!("{e:#}").contains(UNREACHED) => {
                self.unreached.fetch_add(1, Ordering::Relaxed);
                let _first = self.first_unreached.set(format!("{e:#}"));
                self.call_once(&self.slopty_bin, args, env)
            }
            answered => answered,
        }
    }

    /// `slopty-probe` with `args` and `env`, its stdout, as [`Self::call_once`].
    fn probe(&self, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>> {
        self.call_once(&self.probe_bin, args, env)
    }

    /// `program` (`slopty`, `--json`, or `slopty-probe`) with `args` and `env`, its stdout;
    /// fails on an exit status or after [`CALL`].
    fn call_once(&self, program: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let stdout = self.root.join(format!("call-{call}.out"));
        let stderr = self.root.join(format!("call-{call}.err"));
        let json: &[&str] = if program == self.slopty_bin { &["--json"] } else { &[] };
        let mut child = Command::new(program)
            .arg("--data-dir")
            .arg(&self.cli)
            .args(["--server", &self.server])
            .args(json)
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(File::create(&stdout)?)
            .stderr(File::create(&stderr)?)
            .spawn()
            .context("spawn slopty")?;
        let status = wait_child(&mut child, CALL).with_context(|| format!("slopty {args:?}"));
        let err = std::fs::read_to_string(&stderr).unwrap_or_default();
        let out = std::fs::read(&stdout);
        let _removed = (std::fs::remove_file(&stdout), std::fs::remove_file(&stderr));
        let status = status?;
        ensure!(status.success(), "slopty {args:?}: {status}: {}", err.trim());
        Ok(out?)
    }

    fn pids(&self) -> Vec<(&'static str, i32)> {
        self.daemons
            .iter()
            .filter_map(|(name, child)| Some((*name, i32::try_from(child.id()).ok()?)))
            .collect()
    }

    /// Kill the daemons, worker first, and remove the root.
    fn stop(&mut self) {
        for (_name, child) in self.daemons.iter_mut().rev() {
            let _killed = child.kill();
            let _reaped = child.wait();
        }
        self.daemons.clear();
        let _removed = std::fs::remove_dir_all(&self.root);
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The string at `key` of `value`, or empty.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The first line `child` prints, within 30 s; the rest of its output goes to `<name>.stdout`.
fn first_line(child: &mut Child, out: &Path, name: &str) -> Result<String> {
    let stdout = child.stdout.take().context("a piped stdout")?;
    let mut rest = File::create(out.join(format!("{name}.stdout")))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let read = reader.read_line(&mut line).map(|_| line);
        let _sent = tx.send(read);
        let _copied = std::io::copy(&mut reader, &mut rest);
    });
    let line = rx
        .recv_timeout(Duration::from_secs(30))
        .with_context(|| format!("{name} printed nothing in 30 s"))??;
    ensure!(!line.trim().is_empty(), "{name} exited before it printed where it listens");
    Ok(line.trim().to_owned())
}

/// Poll `ready` every 50 ms until it holds, for at most `bound`.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling")]
fn wait_for(mut ready: impl FnMut() -> bool, bound: Duration) -> Result<()> {
    let started = Instant::now();
    while !ready() {
        ensure!(started.elapsed() < bound, "not within {bound:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Reap `child` within `bound`, killing it after.
fn wait_child(child: &mut Child, bound: Duration) -> Result<std::process::ExitStatus> {
    let mut status = None;
    let waited = wait_for(
        || {
            status = child.try_wait().ok().flatten();
            status.is_some()
        },
        bound,
    );
    if waited.is_err() {
        let _killed = child.kill();
        let _reaped = child.wait();
        bail!("did not finish within {bound:?}");
    }
    status.context("reaped")
}

/// Open a quiet bash, flood it, play a hook to it, read it back and close it. A hook the relay
/// failed to deliver goes into `missed` and the cycle goes on: it is a finding, and the load is
/// still worth watching.
fn cycle(stack: &Stack, n: u32, missed: &mut Vec<String>) -> Result<()> {
    let cwd = stack.root.to_string_lossy().into_owned();
    let name = format!("soak-{n}");
    let opened = stack.slopty(&[
        "open",
        "--worker",
        "soak",
        "--cwd",
        &cwd,
        "--name",
        &name,
        "--",
        "/bin/bash",
        "--noprofile",
        "--norc",
        "-i",
    ])?;
    let term =
        opened.get("term").and_then(Value::as_str).context("`open` printed no term")?.to_owned();
    let done = format!("soak-done-{n}");
    let typed = format!("seq 1 {FLOOD_LINES}; echo {done}\n");
    stack.slopty(&["send", &term, "--text", &typed])?;
    stack.slopty(&["wait", &term, "--output", &format!("^{done}$"), "--timeout", "30000"])?;
    let session = term.rsplit('/').next().context("a term is worker/session")?;
    let socket = stack.root.join("worker.sock").to_string_lossy().into_owned();
    let env = [("SLOPTY_SESSION", session), ("SLOPTY_WORKER_SOCKET", socket.as_str())];
    for status in ["working", "idle"] {
        if let Err(e) = stack.call(&["hook", "report", status, "soak"], &env) {
            missed.push(format!("{e:#}"));
        }
    }
    stack.slopty(&["output", &term, "--max", "200"])?;
    stack.slopty(&["close", &term])?;
    Ok(())
}

/// A stream's target as `slopty-probe screen` takes it: `--display <id>` or `--window <id>`.
type Target = [String; 2];

/// What the worker's drawn screen offers to stream.
fn stream_targets(stack: &Stack) -> Result<Vec<Target>> {
    let listed = stack.probe(&["screen", "--worker", &stack.worker, "--list"], &[])?;
    let listed = String::from_utf8_lossy(&listed);
    let targets = parse_targets(&listed);
    ensure!(!targets.is_empty(), "the worker listed nothing to stream:\n{listed}");
    Ok(targets)
}

/// The targets in `slopty-probe screen --list`'s lines: `display display#<id> …` (a `DisplayId`
/// prints itself so) and `window <id> …`.
fn parse_targets(listed: &str) -> Vec<Target> {
    listed
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let flag = match words.next()? {
                "display" => "--display",
                "window" => "--window",
                _ => return None,
            };
            let id = words.next()?.rsplit('#').next()?;
            id.parse::<u32>().ok()?;
            Some([flag.to_owned(), id.to_owned()])
        })
        .collect()
}

/// One stream as `slopty-probe screen` reports it, with the worker's own counters for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StreamRun {
    target: String,
    first_frame_ms: u64,
    decoded: u64,
    lost: u64,
    decode_errors: u64,
    stalls: u64,
    captured: u64,
    dropped: u64,
}

impl StreamRun {
    /// Read the probe's report (`apps/slopty-cli/src/probe/screen.rs`). A line it no longer
    /// prints is an error, so a changed report cannot pass as a clean stream.
    fn parse(report: &str) -> Result<Self> {
        let line = |start: &str| {
            report
                .lines()
                .map(str::trim)
                .find(|l| l.starts_with(start))
                .with_context(|| format!("no line starting {start:?}"))
        };
        let first = line("first frame after")?;
        let datagrams = line("datagrams")?;
        let stalls = line("stalls")?;
        let worker = line("worker captured")?;
        Ok(Self {
            target: String::new(),
            first_frame_ms: number_after(first, "first frame after")?,
            decoded: number_after(first, ";")?,
            lost: number_after(datagrams, "lost")?,
            decode_errors: number_after(datagrams, "decode errors")?,
            stalls: number_after(stalls, "stalls")?,
            captured: number_after(worker, "worker captured")?,
            dropped: number_after(worker, "dropped")?,
        })
    }
}

/// The whole number that follows `key` in `line`, its fraction dropped.
fn number_after(line: &str, key: &str) -> Result<u64> {
    let (_, rest) = line.split_once(key).with_context(|| format!("no {key:?} in {line:?}"))?;
    let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
    digits.parse().with_context(|| format!("no number after {key:?} in {line:?}"))
}

/// Open the next stream in turn (the targets, then the scales), run it for
/// [`STREAM_SECONDS`] and close it.
fn stream(stack: &Stack, targets: &[Target]) -> Result<StreamRun> {
    let n = stack.streams.fetch_add(1, Ordering::Relaxed);
    let [flag, id] = targets.get(n.checked_rem(targets.len()).unwrap_or(0)).context("a target")?;
    let turn = n.checked_div(targets.len()).unwrap_or(0);
    let scale =
        STREAM_SCALES.get(turn.checked_rem(STREAM_SCALES.len()).unwrap_or(0)).context("a scale")?;
    let seconds = STREAM_SECONDS.to_string();
    let socket = stack.root.join("worker.sock").to_string_lossy().into_owned();
    let args =
        ["screen", "--worker", &stack.worker, flag, id, "--seconds", &seconds, "--scale", scale];
    // The worker's counters for the stream come from its control socket.
    let out = stack.probe(&args, &[("SLOPTY_WORKER_SOCKET", &socket)])?;
    let report = String::from_utf8_lossy(&out);
    let mut run = StreamRun::parse(&report)
        .with_context(|| format!("`slopty-probe screen` printed:\n{report}"))?;
    run.target = format!("{flag} {id} at {scale}");
    Ok(run)
}

/// Stream one after another until `stop` is set. A stream that fails ends the lane: it is a
/// finding, and the terminals' load goes on without it.
fn stream_lane(stack: &Stack, targets: &[Target], stop: &AtomicBool) -> Vec<Result<StreamRun>> {
    let mut runs = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let run = stream(stack, targets);
        let failed = run.is_err();
        runs.push(run);
        if failed {
            break;
        }
    }
    runs
}

/// Run `load` beside `lanes` stream lanes, which stop once it returns.
fn with_streams<T>(
    stack: &Stack,
    targets: &[Target],
    lanes: usize,
    streams: &mut Vec<Result<StreamRun>>,
    load: impl FnOnce() -> T,
) -> T {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let running: Vec<_> =
            std::iter::repeat_with(|| scope.spawn(|| stream_lane(stack, targets, &stop)))
                .take(lanes)
                .collect();
        let loaded = load();
        stop.store(true, Ordering::Relaxed);
        for lane in running {
            match lane.join() {
                Ok(runs) => streams.extend(runs),
                Err(_panic) => streams.push(Err(anyhow::anyhow!("a stream lane panicked"))),
            }
        }
        loaded
    })
}

/// The streams' totals, and a failure for each budget they broke.
fn judge_streams(runs: &[Result<StreamRun>], failures: &mut Vec<String>) -> Value {
    let ran: Vec<&StreamRun> = runs.iter().filter_map(|r| r.as_ref().ok()).collect();
    let failed: Vec<String> =
        runs.iter().filter_map(|r| r.as_ref().err()).map(|e| format!("{e:#}")).collect();
    let sum = |f: fn(&StreamRun) -> u64| ran.iter().map(|r| f(r)).fold(0_u64, u64::saturating_add);
    let (decoded, lost, errors) = (sum(|r| r.decoded), sum(|r| r.lost), sum(|r| r.decode_errors));
    let (captured, dropped, stalls) = (sum(|r| r.captured), sum(|r| r.dropped), sum(|r| r.stalls));
    if let Some(first) = failed.first() {
        failures.push(format!("{} streams failed; the first: {first}", failed.len()));
    }
    let blank: Vec<&str> =
        ran.iter().filter(|r| r.decoded == 0).map(|r| r.target.as_str()).collect();
    if !blank.is_empty() {
        failures.push(format!("{} streams decoded no frame: {}", blank.len(), blank.join(", ")));
    }
    if errors > 0 {
        failures.push(format!("streams: {errors} decode errors"));
    }
    if lost.saturating_mul(1000) > decoded.saturating_mul(LOST_PERMILLE) {
        failures.push(format!(
            "streams: {lost} frames lost of {decoded} decoded, over {LOST_PERMILLE}‰"
        ));
    }
    if dropped.saturating_mul(1000) > captured.saturating_mul(DROPPED_PERMILLE) {
        failures.push(format!(
            "streams: the worker dropped {dropped} of {captured} captured, over {DROPPED_PERMILLE}‰"
        ));
    }
    let mut first_ms: Vec<u64> = ran.iter().map(|r| r.first_frame_ms).collect();
    let mut decoded_each: Vec<u64> = ran.iter().map(|r| r.decoded).collect();
    let first = slopty_testkit::stats::Spread::of(&mut first_ms);
    let each = slopty_testkit::stats::Spread::of(&mut decoded_each);
    println!(
        "  streams: {} run, {} failed; {decoded} frames decoded ({}), {lost} lost, {errors} decode errors, {stalls} stalls; worker captured {captured}, dropped {dropped}; first frame ms {}",
        ran.len(),
        failed.len(),
        each.map_or_else(String::new, |s| format!("per stream {s}")),
        first.map_or_else(String::new, |s| s.to_string()),
    );
    json!({
        "streams": ran.len(),
        "failed": failed.len(),
        "seconds_each": STREAM_SECONDS,
        "decoded": decoded,
        "decoded_each": each.map(|s| json!({"min": s.min, "p50": s.p50, "max": s.max})),
        "lost": lost,
        "decode_errors": errors,
        "stalls": stalls,
        "worker_captured": captured,
        "worker_dropped": dropped,
        "first_frame_ms": first.map(|s| json!({"p50": s.p50, "p95": s.p95, "max": s.max})),
    })
}

/// One reading of one daemon.
#[derive(Debug, Clone, Copy)]
struct Sample {
    at: f64,
    phase: &'static str,
    daemon: &'static str,
    footprint: u64,
    peak: u64,
    fds: u32,
    threads: u32,
}

/// Each daemon's threads by name, with how many carry each.
fn named_threads(stack: &Stack) -> BTreeMap<&'static str, BTreeMap<String, u32>> {
    stack
        .pids()
        .into_iter()
        .filter_map(|(daemon, pid)| Some((daemon, thread_names(pid)?)))
        .collect()
}

/// The names that `after` holds more of than `before`, as `+1 tokio-rt-worker`.
///
/// The worker's tokio workers are a fixed number, so more `tokio-rt-worker` threads once the
/// daemons have settled for longer than tokio's 10 s keep-alive means its blocking pool is still
/// being handed work.
fn grown(before: &BTreeMap<String, u32>, after: &BTreeMap<String, u32>) -> String {
    after
        .iter()
        .filter_map(|(name, &n)| {
            let more = n.saturating_sub(before.get(name).copied().unwrap_or(0));
            (more > 0).then(|| format!("+{more} {name}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `pid`'s threads by name, with how many carry each. A name that ends in a UUID (a session's
/// thread) is counted under its stem, and a thread without a name as `(unnamed)`: the system's
/// dispatch threads, which the kernel starts and ends as work comes.
#[cfg(target_vendor = "apple")]
fn thread_names(pid: i32) -> Option<BTreeMap<String, u32>> {
    /// `PROC_PIDLISTTHREADS` (`<sys/proc_info.h>`): the process's thread handles, as `u64`s.
    const PROC_PIDLISTTHREADS: libc::c_int = 6;
    let mut handles = vec![0_u64; 4096];
    let bytes = libc::c_int::try_from(size_of_val(handles.as_slice())).ok()?;
    // SAFETY: `proc_pidinfo` (libproc.h) writes at most `buffersize` bytes into `buffer`, which
    // is the `handles` allocation of exactly that many bytes, and returns how many it wrote.
    let written = unsafe {
        libc::proc_pidinfo(pid, PROC_PIDLISTTHREADS, 0, handles.as_mut_ptr().cast(), bytes)
    };
    let listed = usize::try_from(written).ok().filter(|&n| n > 0)?.checked_div(size_of::<u64>())?;
    let mut names = BTreeMap::new();
    for &handle in handles.get(..listed)? {
        let mut info = std::mem::MaybeUninit::<libc::proc_threadinfo>::zeroed();
        let size = libc::c_int::try_from(size_of::<libc::proc_threadinfo>()).ok()?;
        // SAFETY: for `PROC_PIDTHREADINFO` `proc_pidinfo` writes one `proc_threadinfo` for the
        // thread handle `arg` into a buffer of that size, which `info` is.
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTHREADINFO,
                handle,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if got != size {
            // The thread ended between the listing and this read.
            continue;
        }
        // SAFETY: zeroed is a valid `proc_threadinfo` (integers and a byte array), and the call
        // filled it.
        let info = unsafe { info.assume_init() };
        let bytes: Vec<u8> =
            info.pth_name.iter().map(|&c| c.to_ne_bytes()[0]).take_while(|&b| b != 0).collect();
        let name = String::from_utf8_lossy(&bytes).into_owned();
        let count = names.entry(stem(&name)).or_insert(0_u32);
        *count = count.saturating_add(1);
    }
    Some(names)
}

/// Each of `pid`'s threads by its name's [`stem`], counted, from `/proc/<pid>/task/*/comm`
/// (proc(5)). `None` once the process is gone.
#[cfg(not(target_vendor = "apple"))]
fn thread_names(pid: i32) -> Option<BTreeMap<String, u32>> {
    let mut names = BTreeMap::new();
    for task in std::fs::read_dir(format!("/proc/{pid}/task")).ok()?.flatten() {
        // A thread that ended between the listing and this read has no name to count.
        let Ok(name) = std::fs::read_to_string(task.path().join("comm")) else { continue };
        let count = names.entry(stem(name.trim_end_matches('\n'))).or_insert(0_u32);
        *count = count.saturating_add(1);
    }
    Some(names)
}

/// `name` with a trailing UUID replaced by `*`, and `(unnamed)` for no name.
fn stem(name: &str) -> String {
    if name.is_empty() {
        return "(unnamed)".to_owned();
    }
    match name.len().checked_sub(36).and_then(|at| name.split_at_checked(at)) {
        Some((head, uuid))
            if uuid.len() == 36
                && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
                && uuid.matches('-').count() == 4 =>
        {
            format!("{head}*")
        }
        _ => name.to_owned(),
    }
}

fn sample(stack: &Stack, started: Instant, phase: &'static str) -> Vec<Sample> {
    let at = started.elapsed().as_secs_f64();
    stack
        .pids()
        .into_iter()
        .filter_map(|(daemon, pid)| {
            let usage = process::usage(pid)?;
            Some(Sample {
                at,
                phase,
                daemon,
                footprint: usage.footprint,
                peak: usage.peak_footprint,
                fds: process::open_fds(pid)?,
                threads: process::threads(pid)?,
            })
        })
        .collect()
}

/// Leave the daemons alone for [`SETTLE`], sampling as usual.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; pacing samples")]
fn settle(
    stack: &Stack,
    samples: &mut Vec<Sample>,
    started: Instant,
    interval: Duration,
    phase: &'static str,
) {
    let now = Instant::now();
    let until = now.checked_add(SETTLE).unwrap_or(now);
    while Instant::now() < until {
        samples.extend(sample(stack, started, phase));
        std::thread::sleep(interval.min(until.saturating_duration_since(Instant::now())));
    }
}

/// Run [`FILL_CYCLES`] cycles, [`FILL_LANES`] at a time, sampling as usual.
/// Cycles are numbered on from `last`; returns the last number taken.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; pacing samples")]
fn fill(
    stack: &Stack,
    samples: &mut Vec<Sample>,
    missed: &mut Vec<String>,
    started: Instant,
    interval: Duration,
    last: u32,
) -> Result<u32> {
    let next = AtomicU32::new(last.saturating_add(1));
    let end = next.load(Ordering::Relaxed).saturating_add(FILL_CYCLES);
    std::thread::scope(|scope| {
        let lanes: Vec<_> = std::iter::repeat_with(|| {
            scope.spawn(|| {
                let mut lost = Vec::new();
                let ran = loop {
                    let n = next.fetch_add(1, Ordering::Relaxed);
                    if n >= end {
                        break Ok(());
                    }
                    if let Err(e) = cycle(stack, n, &mut lost) {
                        // The other lanes stop at their next cycle.
                        next.store(end, Ordering::Relaxed);
                        break Err(e);
                    }
                };
                (ran, lost)
            })
        })
        .take(FILL_LANES)
        .collect();
        while !lanes.iter().all(std::thread::ScopedJoinHandle::is_finished) {
            samples.extend(sample(stack, started, "fill"));
            std::thread::sleep(interval);
        }
        for lane in lanes {
            let (ran, lost) =
                lane.join().map_err(|_panic| anyhow::anyhow!("a fill lane panicked"))?;
            missed.extend(lost);
            ran?;
        }
        Ok(end.saturating_sub(1))
    })
}

/// The part of the load's `points` (seconds, bytes) whose slope is judged: all but its first
/// [`UNJUDGED_SECONDS`].
fn judged_part(points: &[(f64, f64)]) -> &[(f64, f64)] {
    let Some(&(start, _)) = points.first() else { return points };
    let from = points.partition_point(|&(at, _)| at - start < UNJUDGED_SECONDS);
    points.get(from..).unwrap_or_default()
}

/// The seconds `points` (seconds, bytes) span.
fn window_seconds(points: &[(f64, f64)]) -> f64 {
    match (points.first(), points.last()) {
        (Some(first), Some(last)) => last.0 - first.0,
        _ => 0.0,
    }
}

/// The Theil–Sen slope of `points` (seconds, bytes), in KiB a minute: the median of the slopes
/// between every pair of points.
///
/// The footprint moves in allocator steps and in spikes a few seconds long, and a least-squares
/// fit follows each of them. The median moves only when most pairs rise. Growth spread over the
/// window makes them rise; a spike a few samples wide spans almost no pairs, and a single held
/// step spans more than half only when it falls within `1/(2√n)` of the window's middle.
fn slope_kib_per_min(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 3 {
        return None;
    }
    let mut slopes: Vec<f64> = points
        .iter()
        .enumerate()
        .flat_map(|(i, a)| points.iter().skip(i.saturating_add(1)).map(move |b| (a, b)))
        .filter(|(a, b)| b.0 > a.0)
        .map(|(a, b)| (b.1 - a.1) / (b.0 - a.0))
        .collect();
    if slopes.is_empty() {
        return None;
    }
    let middle = slopes.len() / 2;
    let (_, median, _) = slopes.select_nth_unstable_by(middle, f64::total_cmp);
    Some(*median * 60.0 / 1024.0)
}

/// `leaks <pid>`: the verdict line and whether it found none.
fn leaks(sh: &Shell, pid: i32, report: &Path) -> Result<(bool, String)> {
    let pid = pid.to_string();
    let output = cmd!(sh, "leaks {pid}").quiet().ignore_status().ignore_stderr().output()?;
    std::fs::write(report, &output.stdout)?;
    let verdict = verdict(&String::from_utf8_lossy(&output.stdout));
    match output.status.code() {
        Some(0) => Ok((true, verdict)),
        Some(1) => Ok((false, verdict)),
        _ => bail!("leaks {pid} failed ({}): {verdict}", output.status),
    }
}

/// The verdict of a `leaks` report, `Process 7: 1 leak for 128 total leaked bytes.`, with its
/// first root leak after it.
fn verdict(report: &str) -> String {
    let Some(line) =
        report.lines().find(|l| l.starts_with("Process ") && l.contains("total leaked bytes"))
    else {
        return "no verdict line".to_owned();
    };
    let root = report
        .lines()
        .filter(|l| l.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .find_map(|l| l.split_once("ROOT LEAK: "));
    match root {
        Some((_, root)) => format!("{} (first: {})", line.trim(), root.trim()),
        None => line.trim().to_owned(),
    }
}

#[expect(clippy::cast_precision_loss, reason = "byte counts and seconds, far below 2^52")]
fn soak(stack: &Stack, opts: &SoakOpts, out: &Path) -> Result<Value> {
    let sh = Shell::new()?;
    let started = Instant::now();
    let interval = Duration::from_secs(opts.interval.max(1));
    let mut samples = Vec::new();
    let mut missed = Vec::new();
    let mut n = 0_u32;
    let lanes = opts.stream_lanes;
    let mut streams = Vec::new();
    let targets = if lanes == 0 { Vec::new() } else { stream_targets(stack)? };
    println!("▶ warm up: two cycles and a stream of each target, then {SETTLE:?} alone");
    for _ in 0..2 {
        n = n.wrapping_add(1);
        cycle(stack, n, &mut missed)?;
    }
    for _ in &targets {
        streams.push(stream(stack, &targets));
    }
    settle(stack, &mut samples, started, interval, "warm");
    let unfilled = sample(stack, started, "unfilled");
    samples.extend(unfilled.iter().copied());
    println!(
        "▶ fill the bounded stores: {FILL_CYCLES} cycles, {FILL_LANES} at a time, beside {lanes} stream lanes"
    );
    // A cycle that fails ends the load, not the soak: the samples up to it, the counts once the
    // daemons have settled and `leaks` are the evidence of why it failed.
    let mut aborted: Option<String> = None;
    let fill_started = Instant::now();
    match with_streams(stack, &targets, lanes, &mut streams, || {
        fill(stack, &mut samples, &mut missed, started, interval, n)
    }) {
        Ok(last) => n = last,
        Err(e) => aborted = Some(format!("the fill stopped: {e:#}")),
    }
    let fill_took = fill_started.elapsed();
    settle(stack, &mut samples, started, interval, "filled");
    let baseline = sample(stack, started, "baseline");
    let named_before = named_threads(stack);
    samples.extend(baseline.iter().copied());
    if aborted.is_none() {
        println!("▶ load for {} s beside {lanes} stream lanes", opts.seconds);
    }
    let load_started = Instant::now();
    let deadline =
        load_started.checked_add(Duration::from_secs(opts.seconds)).unwrap_or(load_started);
    let mut cycles = Vec::new();
    let load_lanes = if aborted.is_some() { 0 } else { lanes };
    let loaded = with_streams(stack, &targets, load_lanes, &mut streams, || -> Result<()> {
        let mut next = Instant::now();
        while aborted.is_none() && Instant::now() < deadline {
            n = n.wrapping_add(1);
            let began = Instant::now();
            cycle(stack, n, &mut missed)?;
            cycles.push(began.elapsed());
            if Instant::now() >= next {
                samples.extend(sample(stack, started, "load"));
                next = next.checked_add(interval).unwrap_or(next);
            }
        }
        Ok(())
    });
    if let Err(e) = loaded {
        aborted = Some(format!("the load stopped: {e:#}"));
    }
    println!("▶ settle for {SETTLE:?}, then leaks");
    settle(stack, &mut samples, started, interval, "settle");
    let end = sample(stack, started, "end");
    let named_after = named_threads(stack);
    samples.extend(end.iter().copied());

    let mut failures = Vec::new();
    failures.extend(aborted);
    if let Some(first) = missed.first() {
        failures.push(format!(
            "{} of {} hook reports failed; the first: {first}",
            missed.len(),
            u64::from(n).saturating_mul(2)
        ));
    }
    let unreached = stack.unreached.load(Ordering::Relaxed);
    if let Some(first) = stack.first_unreached.get() {
        failures.push(format!(
            "{unreached} of {} calls could not reach the server at the first try; the first: {first}",
            stack.calls.load(Ordering::Relaxed)
        ));
    }
    let streamed = if lanes == 0 { Value::Null } else { judge_streams(&streams, &mut failures) };
    let mut daemons = serde_json::Map::new();
    for (name, pid) in stack.pids() {
        let empty = unfilled.iter().find(|s| s.daemon == name).context("an unfilled sample")?;
        let before = baseline.iter().find(|s| s.daemon == name).context("a baseline sample")?;
        let after = end.iter().find(|s| s.daemon == name).context("an end sample")?;
        let load: Vec<(f64, f64)> = samples
            .iter()
            .filter(|s| s.daemon == name && s.phase == "load")
            .map(|s| (s.at, s.footprint as f64))
            .collect();
        let tail = judged_part(&load);
        let slope = slope_kib_per_min(tail);
        let window = window_seconds(tail);
        let judged = window >= SLOPE_WINDOW_SECONDS;
        let peak = samples.iter().filter(|s| s.daemon == name).map(|s| s.peak).max().unwrap_or(0);
        let streaming =
            if name == "worker" { STREAM_PEAK_MIB.saturating_mul(lanes as u64) } else { 0 };
        let peak_budget = PEAK_MIB
            .iter()
            .find(|(d, _)| *d == name)
            .map_or(u64::MAX, |(_, m)| m.saturating_add(streaming) << 20);
        let (clean, verdict) = leaks(&sh, pid, &out.join(format!("leaks-{name}.txt")))?;
        if judged && slope.is_some_and(|s| s > SLOPE_KIB_PER_MIN) {
            failures.push(format!(
                "{name}: footprint grows {:.0} KiB/min, over {SLOPE_KIB_PER_MIN}",
                slope.unwrap_or(0.0)
            ));
        }
        if peak > peak_budget {
            failures.push(format!(
                "{name}: peak footprint {} MiB, over {} MiB",
                peak >> 20,
                peak_budget >> 20
            ));
        }
        if after.fds > before.fds {
            failures.push(format!(
                "{name}: {} descriptors open after the load, {} before",
                after.fds, before.fds
            ));
        }
        let named = (named_before.get(name), named_after.get(name));
        if after.threads > before.threads {
            let grew = match named {
                (Some(before), Some(after)) => format!(" ({})", grown(before, after)),
                _ => String::new(),
            };
            failures.push(format!(
                "{name}: {} threads after the load, {} before{grew}",
                after.threads, before.threads
            ));
        }
        if !clean {
            failures.push(format!("{name}: {verdict} (leaks-{name}.txt)"));
        }
        println!(
            "  {name}: footprint {} → {} filled → {} KiB, slope {} KiB/min{}, peak {} MiB; fds {} → {}; threads {} → {}; {verdict}",
            empty.footprint >> 10,
            before.footprint >> 10,
            after.footprint >> 10,
            slope.map_or_else(|| "—".to_owned(), |s| format!("{s:.1}")),
            if judged {
                String::new()
            } else {
                format!(
                    " (not judged: {window:.0} s of load after the first {UNJUDGED_SECONDS:.0}, \
                     {SLOPE_WINDOW_SECONDS:.0} s needed)"
                )
            },
            peak >> 20,
            before.fds,
            after.fds,
            before.threads,
            after.threads,
        );
        daemons.insert(
            name.to_owned(),
            json!({
                "footprint_unfilled": empty.footprint,
                "footprint_baseline": before.footprint,
                "footprint_end": after.footprint,
                "slope_kib_per_min": slope,
                "slope_window_s": window,
                "slope_judged": judged,
                "peak": peak,
                "peak_budget": peak_budget,
                "fds": [before.fds, after.fds],
                "threads": [before.threads, after.threads],
                "thread_names": [named.0, named.1],
                "leaks_clean": clean,
                "leaks": verdict,
            }),
        );
    }
    let mut lines = String::new();
    for s in &samples {
        let row = json!({
            "at": s.at, "phase": s.phase, "daemon": s.daemon, "footprint": s.footprint,
            "peak": s.peak, "fds": s.fds, "threads": s.threads,
        });
        lines.push_str(&row.to_string());
        lines.push('\n');
    }
    std::fs::write(out.join("samples.jsonl"), lines)?;
    let mut cycle_ms: Vec<u64> =
        cycles.iter().map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)).collect();
    let spread = slopty_testkit::stats::Spread::of(&mut cycle_ms);
    let summary = json!({
        "seconds": opts.seconds,
        "fill_cycles": FILL_CYCLES,
        "fill_seconds": fill_took.as_secs_f64(),
        "cycles": cycles.len(),
        "calls_unreached": unreached,
        "cycle_ms": spread.map(|s| json!({"p50": s.p50, "p95": s.p95, "max": s.max})),
        "slope_budget_kib_per_min": SLOPE_KIB_PER_MIN,
        "streams": streamed,
        "daemons": daemons,
        "failures": failures,
    });
    std::fs::write(out.join("summary.json"), serde_json::to_string_pretty(&summary)?)?;
    println!(
        "  {} cycles, {}",
        cycles.len(),
        spread.map_or_else(String::new, |s| format!("cycle ms {s}"))
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This process's threads, read by name: one started here under its own name is there.
    #[test]
    fn a_process_s_threads_read_by_name() {
        // A thread names itself as it starts, so it answers once its name is set.
        let (ready, started) = std::sync::mpsc::channel::<()>();
        let (hold, held) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("soak-names-probe".to_owned())
            .spawn(move || {
                let _sent = ready.send(());
                held.recv()
            })
            .unwrap();
        started.recv().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        let names = thread_names(pid).unwrap();
        drop(hold);
        let _ended = thread.join();
        assert_eq!(names.get("soak-names-probe"), Some(&1), "{names:?}");
    }

    #[test]
    fn a_thread_named_for_a_session_counts_under_its_stem() {
        assert_eq!(stem("session-01a0f413-4c57-7409-b598-8612456179f6"), "session-*");
        assert_eq!(stem("tokio-rt-worker"), "tokio-rt-worker");
        assert_eq!(stem(""), "(unnamed)");
        let before = BTreeMap::from([("tokio-rt-worker".to_owned(), 14), ("main".to_owned(), 1)]);
        let after = BTreeMap::from([("tokio-rt-worker".to_owned(), 15), ("main".to_owned(), 1)]);
        assert_eq!(grown(&before, &after), "+1 tokio-rt-worker");
    }

    /// Twenty minutes of load sampled every 2 s at 4 MiB in 16 KiB steps: 6 MiB taken over the
    /// first five minutes (the allocator reaching its high-water mark), then three 17 MiB spikes
    /// two samples wide and 1 MiB taken at 900 s and held. The judged part leaves the climb out
    /// and reads no growth, and the same with 128 KiB a minute of growth under it reads that.
    #[test]
    fn a_climb_spikes_and_a_held_step_read_as_no_growth_and_growth_reads_its_rate() {
        let footprint = |kib_per_min: f64| -> Vec<(f64, f64)> {
            (0..=600_u32)
                .map(|i| {
                    let at = f64::from(i * 2);
                    let climbed = (at.min(300.0) / 300.0 * 6144.0 / 16.0).floor() * 16.0;
                    let spike = [450.0, 750.0, 1050.0].iter().any(|s| (*s..*s + 4.0).contains(&at));
                    let grown = (at / 60.0 * kib_per_min / 16.0).floor() * 16.0;
                    let held = if at >= 900.0 { 1024.0 } else { 0.0 };
                    let spiked = if spike { 17.0 * 1024.0 } else { 0.0 };
                    let kib = 4096.0 + climbed + grown + held + spiked;
                    (at, kib * 1024.0)
                })
                .collect()
        };
        let still = footprint(0.0);
        let judged = judged_part(&still);
        assert!(window_seconds(judged) >= SLOPE_WINDOW_SECONDS);
        let slope = slope_kib_per_min(judged).unwrap_or(f64::NAN);
        assert!(slope.abs() < 1.0, "no growth read as {slope} KiB/min");
        let whole = slope_kib_per_min(&still).unwrap_or(f64::NAN);
        assert!(whole > SLOPE_KIB_PER_MIN, "the climb, judged, reads {whole} KiB/min");
        let growing = slope_kib_per_min(judged_part(&footprint(128.0))).unwrap_or(f64::NAN);
        assert!((growing - 128.0).abs() < 128.0 * 0.1, "128 KiB/min read as {growing}");
    }

    /// A soak staging its daemons leaves the file another soak is running whole: what was open
    /// still reads the old bytes, the path reads the new ones, and nothing is left beside them.
    #[test]
    fn a_staged_binary_replaces_the_path_and_leaves_the_running_file_whole() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("soak-stage-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let built = dir.join("built");
        let copy = dir.join("slopty-worker");
        std::fs::write(&built, "new")?;
        std::fs::write(&copy, "old")?;
        let mut running = File::open(&copy)?;
        replace_signed(&built, &copy, |staged| {
            ensure!(staged != copy, "signed in place");
            ensure!(std::fs::read_to_string(&copy)? == "old", "the path changed before the rename");
            Ok(())
        })?;
        let mut was = String::new();
        std::io::Read::read_to_string(&mut running, &mut was)?;
        assert_eq!(was, "old", "the running file was written in place");
        assert_eq!(std::fs::read_to_string(&copy)?, "new");
        assert_eq!(std::fs::read_dir(&dir)?.count(), 2, "a staged copy was left behind");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// `leaks` says `1 leak for` and `2 leaks for`, and names each root leak, after its stack under
    /// `MallocStackLogging`.
    #[test]
    fn a_leaks_verdict_reads_in_the_singular_and_names_the_root() {
        let one = "Process 83979: 56282 nodes malloced for 16716 KB\n\
                   Process 83979: 1 leak for 128 total leaked bytes.\n\n\
                   STACK OF 1 INSTANCE OF 'ROOT LEAK: <NSPasteboard>':\n\
                   0   libsystem_malloc.dylib  0x1873d0820 _malloc_zone_calloc + 132\n====\n    \
                   1 (128 bytes) ROOT LEAK: <NSPasteboard 0x78beeeff80> [128]\n";
        assert_eq!(
            verdict(one),
            "Process 83979: 1 leak for 128 total leaked bytes. (first: <NSPasteboard 0x78beeeff80> \
             [128])"
        );
        let none = "Process 7: 0 leaks for 0 total leaked bytes.\n";
        assert_eq!(verdict(none), "Process 7: 0 leaks for 0 total leaked bytes.");
        assert_eq!(verdict("leaks: cannot examine process"), "no verdict line");
    }

    #[test]
    fn the_slope_is_the_median_of_the_pairwise_slopes() {
        let rising: Vec<(f64, f64)> =
            (0..10).map(|s| (f64::from(s), f64::from(s) * 1024.0)).collect();
        let slope = slope_kib_per_min(&rising).unwrap_or(f64::NAN);
        assert!((slope - 60.0).abs() < 1e-9, "1 KiB a second is 60 KiB a minute: {slope}");
        let flat: Vec<(f64, f64)> = (0..10).map(|s| (f64::from(s), 5.0)).collect();
        assert!(slope_kib_per_min(&flat).is_some_and(|s| s.abs() < 1e-9), "flat");
        assert_eq!(slope_kib_per_min(&rising[..2]), None, "two points fit anything");
    }

    /// The lines of `slopty-probe screen` the soak reads, as `probe/screen.rs` prints them.
    const REPORT: &str = "\
soak: Display(DisplayId(1)) → 1920×1080 Hevc 60 fps 30 Mbit/s scale 0.75
  first frame after 142 ms; 171 frames decoded in 2.86 s = 59.8 fps
  capture→decoded (worker clock; loopback only): p50 9.1 ms
  datagrams 2210  fec-recovered 0  lost 3  nacks 1  refreshes 0  decode errors 2
  stalls 1 (40 ms stalled)
  audio packets 0  lost 9
  worker captured 180 (display-crop path 0), dropped 4, encoded 176, refused 0
";

    #[test]
    fn a_stream_report_reads_back() {
        let run = StreamRun::parse(REPORT).unwrap();
        assert_eq!(
            run,
            StreamRun {
                target: String::new(),
                first_frame_ms: 142,
                decoded: 171,
                lost: 3,
                decode_errors: 2,
                stalls: 1,
                captured: 180,
                dropped: 4,
            },
            "the video's losses, not the audio's"
        );
    }

    #[test]
    fn a_report_without_a_counter_is_an_error_not_a_clean_stream() {
        let no_worker: String = REPORT
            .lines()
            .filter(|l| !l.contains("worker captured"))
            .collect::<Vec<_>>()
            .join("\n");
        let err = StreamRun::parse(&no_worker).unwrap_err();
        assert!(format!("{err:#}").contains("worker captured"), "{err:#}");
        let renamed = REPORT.replace("decode errors 2", "errors 2");
        assert!(StreamRun::parse(&renamed).is_err(), "a renamed counter");
    }

    #[test]
    fn the_listing_gives_the_display_and_the_windows() {
        let listed = "display display#1    1920×1080 @2x 60 Hz\n\
                      window  7            800×600  Canvas — page\n\
                      window  8            640×480  Canvas — blocks\n";
        let targets = parse_targets(listed);
        let flat: Vec<String> = targets.iter().map(|t| t.join(" ")).collect();
        assert_eq!(flat, ["--display 1", "--window 7", "--window 8"]);
    }

    #[test]
    fn streams_are_judged_on_every_budget() {
        let clean = StreamRun { decoded: 1000, captured: 1000, ..StreamRun::default() };
        let mut failures = Vec::new();
        judge_streams(&[Ok(clean.clone())], &mut failures);
        assert!(failures.is_empty(), "{failures:?}");
        let broken = [
            Ok(StreamRun { lost: 6, decode_errors: 1, dropped: 51, ..clean }),
            Ok(StreamRun { target: "--window 7 at 0.5".to_owned(), ..StreamRun::default() }),
            Err(anyhow::anyhow!("the open was refused")),
        ];
        judge_streams(&broken, &mut failures);
        let all = failures.join("\n");
        for expected in [
            "1 streams failed; the first: the open was refused",
            "decoded no frame: --window 7 at 0.5",
            "1 decode errors",
            "6 frames lost of 1000 decoded",
            "dropped 51 of 1000 captured",
        ] {
            assert!(all.contains(expected), "{expected:?} in:\n{all}");
        }
    }
}
