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
//! Every `--interval` it samples each daemon's physical footprint (`ri_phys_footprint`), open
//! descriptors and threads (`slopty_testkit::process`). It fails when
//! - a daemon's footprint grows, by least squares over the last two thirds of the load, faster than
//!   [`SLOPE_KIB_PER_MIN`];
//! - a daemon's peak footprint passes its [`PEAK_MIB`] budget;
//! - a daemon holds more descriptors or threads after the load has settled than before it;
//! - `leaks <pid>` finds a leak in a daemon at the end (exit 1; `man leaks`).
//!
//! No display stream runs: the worker has no switch that puts its synthetic capture behind a
//! stream, and a real one needs Screen Recording. Nothing is drawn, captured or played.
//!
//! `leaks` cannot read a hardened-runtime binary, and `cargo xtask sign` signs the dev daemons
//! with the hardened runtime. So the daemons are copied under `target/deep/soak/bin` and signed
//! there ad hoc, without the runtime and with `get-task-allow`, which the build tree never sees.

use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

/// The largest footprint each daemon may reach, in MiB: a few times the first soak's peaks
/// (server 4, ptyd 24, worker 28; MEASUREMENTS, "the first soak").
const PEAK_MIB: [(&str, u64); 3] = [("server", 32), ("ptyd", 64), ("worker", 128)];

/// How long the daemons are left alone before a count is compared: past tokio's blocking-pool
/// keep-alive (10 s), so a thread that is only idle has gone.
const SETTLE: Duration = Duration::from_secs(12);

/// How long one CLI call may take.
const CALL: Duration = Duration::from_secs(60);

/// Lines each cycle's flood prints.
const FLOOD_LINES: u32 = 20_000;

#[derive(Args, Debug, Clone)]
pub struct SoakOpts {
    /// How long the load runs, in seconds (the nightly run gives 1200).
    #[arg(long, default_value_t = 60)]
    pub seconds: u64,
    /// Seconds between samples.
    #[arg(long, default_value_t = 2)]
    pub interval: u64,
    /// Soak the debug build instead of the release one.
    #[arg(long)]
    pub debug: bool,
    /// Give the daemons `MallocStackLogging`, so `leaks` shows where a leak was allocated (the
    /// footprint then includes the log, so its slope and peak read high).
    #[arg(long)]
    pub stacks: bool,
    /// Where the samples, logs and summary go (default `target/deep/soak/last`).
    #[arg(long)]
    pub out: Option<Utf8PathBuf>,
}

/// The binaries, as the build names them.
const BINARIES: [&str; 4] = ["slopty-server", "slopty-ptyd", "slopty-worker", "slopty"];

pub fn run(sh: &Shell, opts: &SoakOpts) -> Result<()> {
    let root = repo_root()?;
    let out = opts.out.clone().unwrap_or_else(|| root.join("target/deep/soak/last"));
    if out.exists() {
        std::fs::remove_dir_all(&out).with_context(|| format!("clearing {out}"))?;
    }
    std::fs::create_dir_all(&out).with_context(|| format!("creating {out}"))?;
    let bin = build(sh, opts.debug)?;
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
fn build(sh: &Shell, debug: bool) -> Result<Utf8PathBuf> {
    let (flags, profile): (&[&str], &str) =
        if debug { (&[], "debug") } else { (&["--release"], "release") };
    step(
        "build the daemons and the CLI",
        &cmd!(
            sh,
            "nice -n 10 cargo build {flags...} -p slopty-serverd -p slopty-ptyd -p slopty-workerd -p slopty-cli"
        ),
    )?;
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
        let copy = dir.join(name);
        std::fs::copy(&built, &copy).with_context(|| format!("copying {built}"))?;
        cmd!(sh, "codesign --force --sign - --entitlements {entitlements} {copy}")
            .quiet()
            .ignore_stderr()
            .run()
            .with_context(|| format!("signing {copy} for leaks"))?;
    }
    Ok(dir)
}

/// The three daemons on a temporary root.
struct Stack {
    root: PathBuf,
    slopty_bin: PathBuf,
    cli: PathBuf,
    server: String,
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
        ];
        if stacks {
            env.push(("MallocStackLogging", "1".into()));
        }
        let mut stack = Self {
            slopty_bin: bin.join("slopty").into_std_path_buf(),
            cli: root.join("cli"),
            root,
            server: String::new(),
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
            &[
                "--port",
                "0",
                "--mcp-port",
                "0",
                "--print-addr",
                "--data-dir",
                &server_dir,
                "--name",
                "soak-server",
            ],
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
        listening?;
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

    /// `slopty` with `args` and `env`, its stdout; fails on an exit status or after [`CALL`].
    fn call(&self, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>> {
        let stdout = self.root.join("call.out");
        let stderr = self.root.join("call.err");
        let mut child = Command::new(&self.slopty_bin)
            .arg("--data-dir")
            .arg(&self.cli)
            .args(["--server", &self.server, "--json"])
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(File::create(&stdout)?)
            .stderr(File::create(&stderr)?)
            .spawn()
            .context("spawn slopty")?;
        let status = wait_child(&mut child, CALL).with_context(|| format!("slopty {args:?}"))?;
        let err = std::fs::read_to_string(&stderr).unwrap_or_default();
        ensure!(status.success(), "slopty {args:?}: {status}: {}", err.trim());
        Ok(std::fs::read(&stdout)?)
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

/// The least-squares slope of `points` (seconds, bytes), in KiB a minute.
#[expect(clippy::cast_precision_loss, reason = "a count of samples, far below 2^52")]
fn slope_kib_per_min(points: &[(f64, f64)]) -> Option<f64> {
    let n = points.len() as f64;
    if points.len() < 3 {
        return None;
    }
    let mean_x = points.iter().map(|p| p.0).sum::<f64>() / n;
    let mean_y = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxy: f64 = points.iter().map(|p| (p.0 - mean_x) * (p.1 - mean_y)).sum();
    let sxx: f64 = points.iter().map(|p| (p.0 - mean_x).powi(2)).sum();
    (sxx > 0.0).then(|| sxy / sxx * 60.0 / 1024.0)
}

/// `leaks <pid>`: the verdict line and whether it found none.
fn leaks(sh: &Shell, pid: i32, report: &Path) -> Result<(bool, String)> {
    let pid = pid.to_string();
    let output = cmd!(sh, "leaks {pid}").quiet().ignore_status().ignore_stderr().output()?;
    std::fs::write(report, &output.stdout)?;
    let text = String::from_utf8_lossy(&output.stdout);
    let verdict = text
        .lines()
        .find(|l| l.starts_with("Process ") && l.contains("leaks for"))
        .unwrap_or("no verdict line")
        .trim()
        .to_owned();
    match output.status.code() {
        Some(0) => Ok((true, verdict)),
        Some(1) => Ok((false, verdict)),
        _ => bail!("leaks {pid} failed ({}): {verdict}", output.status),
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
    println!("▶ warm up: two cycles, then {SETTLE:?} alone");
    for _ in 0..2 {
        n = n.wrapping_add(1);
        cycle(stack, n, &mut missed)?;
    }
    settle(stack, &mut samples, started, interval, "warm");
    let baseline = sample(stack, started, "baseline");
    samples.extend(baseline.iter().copied());
    println!("▶ load for {} s", opts.seconds);
    let load_started = Instant::now();
    let deadline =
        load_started.checked_add(Duration::from_secs(opts.seconds)).unwrap_or(load_started);
    let mut next = Instant::now();
    let mut cycles = Vec::new();
    while Instant::now() < deadline {
        n = n.wrapping_add(1);
        let began = Instant::now();
        cycle(stack, n, &mut missed)?;
        cycles.push(began.elapsed());
        if Instant::now() >= next {
            samples.extend(sample(stack, started, "load"));
            next = next.checked_add(interval).unwrap_or(next);
        }
    }
    println!("▶ settle for {SETTLE:?}, then leaks");
    settle(stack, &mut samples, started, interval, "settle");
    let end = sample(stack, started, "end");
    samples.extend(end.iter().copied());

    let mut failures = Vec::new();
    if let Some(first) = missed.first() {
        failures.push(format!(
            "{} of {} hook reports failed; the first: {first}",
            missed.len(),
            u64::from(n).saturating_mul(2)
        ));
    }
    let mut daemons = serde_json::Map::new();
    for (name, pid) in stack.pids() {
        let before = baseline.iter().find(|s| s.daemon == name).context("a baseline sample")?;
        let after = end.iter().find(|s| s.daemon == name).context("an end sample")?;
        let load: Vec<(f64, f64)> = samples
            .iter()
            .filter(|s| s.daemon == name && s.phase == "load")
            .map(|s| (s.at, s.footprint as f64))
            .collect();
        let tail = load.get(load.len() / 3..).unwrap_or_default();
        let slope = slope_kib_per_min(tail);
        let peak = samples.iter().filter(|s| s.daemon == name).map(|s| s.peak).max().unwrap_or(0);
        let peak_budget =
            PEAK_MIB.iter().find(|(d, _)| *d == name).map_or(u64::MAX, |(_, m)| m << 20);
        let (clean, verdict) = leaks(&sh, pid, &out.join(format!("leaks-{name}.txt")))?;
        if slope.is_some_and(|s| s > SLOPE_KIB_PER_MIN) {
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
        if after.threads > before.threads {
            failures.push(format!(
                "{name}: {} threads after the load, {} before",
                after.threads, before.threads
            ));
        }
        if !clean {
            failures.push(format!("{name}: {verdict} (leaks-{name}.txt)"));
        }
        println!(
            "  {name}: footprint {} → {} KiB, slope {} KiB/min, peak {} MiB; fds {} → {}; threads {} → {}; {verdict}",
            before.footprint >> 10,
            after.footprint >> 10,
            slope.map_or_else(|| "—".to_owned(), |s| format!("{s:.1}")),
            peak >> 20,
            before.fds,
            after.fds,
            before.threads,
            after.threads,
        );
        daemons.insert(
            name.to_owned(),
            json!({
                "footprint_baseline": before.footprint,
                "footprint_end": after.footprint,
                "slope_kib_per_min": slope,
                "peak": peak,
                "peak_budget": peak_budget,
                "fds": [before.fds, after.fds],
                "threads": [before.threads, after.threads],
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
        "cycles": cycles.len(),
        "cycle_ms": spread.map(|s| json!({"p50": s.p50, "p95": s.p95, "max": s.max})),
        "slope_budget_kib_per_min": SLOPE_KIB_PER_MIN,
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

    #[test]
    fn the_slope_is_the_least_squares_fit() {
        let rising: Vec<(f64, f64)> =
            (0..10).map(|s| (f64::from(s), f64::from(s) * 1024.0)).collect();
        let slope = slope_kib_per_min(&rising).unwrap();
        assert!((slope - 60.0).abs() < 1e-9, "1 KiB a second is 60 KiB a minute: {slope}");
        let flat: Vec<(f64, f64)> = (0..10).map(|s| (f64::from(s), 5.0)).collect();
        assert!(slope_kib_per_min(&flat).unwrap().abs() < 1e-9, "flat");
        assert_eq!(slope_kib_per_min(&rising[..2]), None, "two points fit anything");
    }
}
