//! `cargo xtask tailnet up|down|status`: a real tailnet on loopback, for the live tests of what
//! reads Tailscale (`slopty-tailnet`'s `LocalApi`, grants, paths, the IPN bus).
//!
//! - **Control.** Headscale [`HEADSCALE`] on loopback over HTTP, with its embedded DERP and STUN on
//!   loopback too and no other DERP map, so nothing leaves this Mac. The release binary is fetched
//!   from GitHub once and checked against [`HEADSCALE_SHA256`].
//! - **Nodes.** One userspace `tailscaled` per entry of [`NODES`], each with its own state dir, log
//!   dir and `LocalAPI` socket under `target/tailnet/run/<name>`, logged in with a tagged pre-auth
//!   key from Headscale's CLI. `tailscale` and `tailscaled` [`TAILSCALE`] are built once with the
//!   local Go (`go install`, which checks every module against Go's checksum database) and cached
//!   under `target/tailnet/tools`. No Go is written in this repository.
//! - **Policy.** The tailnet is open between its nodes, and `tag:ci` holds Slopty's capability
//!   ([`CAP`]) on `tag:slopty-worker` with the roles [`CI_ROLES`], as an admin would grant it.
//! - **Life.** `up` stays in the foreground holding everything. Once every node runs, sees the
//!   other, and the worker sees the grant, it writes `fixture.json` (what a test reads through
//!   [`FIXTURE_ENV`]) and prints [`status`]. Ctrl-C, SIGTERM, SIGHUP or `down` take it all down.
//!   Each daemon runs under a guard (this binary, `tailnet guard`) in its own process group, which
//!   stops the daemon when `up`'s end of a pipe closes: however `up` ends, even killed outright, no
//!   daemon outlives it. `down` also removes any process whose command line names the run dir, and
//!   nothing else.
//! - **Ports.** Headscale's four are picked free on loopback and written to `fixture.json`; each
//!   node's `WireGuard` port is the kernel's pick (`--port=0`). Two checkouts never share anything.
//! - **The user's own Tailscale is never reached.** Every CLI call names its node's socket
//!   (`--socket`, which also turns off the CLI's search for the macOS app's port), a node must
//!   answer as a fresh daemon before it is logged in, and its control URL must be this Headscale's
//!   after. `TS_LOGS_DIR` keeps `tailscaled`'s log state out of `/Library/Tailscale`, and the build
//!   leaves out DNS ([`TAILSCALE_TAGS`]), whose start-up clean-up would otherwise reset the system
//!   DNS a root `tailscaled` sets.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{ErrorKind, Read as _};
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream, UdpSocket};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::repo_root;

#[derive(Subcommand)]
pub enum TailnetCmd {
    /// Start Headscale and the nodes, write `fixture.json` once the tailnet is ready, and hold it
    /// until Ctrl-C or `down`.
    Up,
    /// Stop a running `up` and remove everything it started.
    Down,
    /// Print the fixture: the path a test reads, Headscale's address, each node's socket and IPs.
    Status,
    /// Bring the fixture up, run `slopty-tailnet`'s live tests against it, and take it down.
    Test {
        /// More arguments for `cargo nextest run` (filters, `--no-capture`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        nextest: Vec<String>,
    },
    /// Run a daemon until stdin closes, then stop it. Not for people.
    #[command(hide = true)]
    Guard {
        /// The daemon and its arguments.
        #[arg(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
}

/// The Headscale release the fixture runs.
const HEADSCALE: &str = "0.29.4";
/// Its macOS arm64 binary's SHA-256, from the release's `checksums.txt`.
const HEADSCALE_SHA256: &str = "b5cfd0f81caaa1e8f71f830fd89fdf86a8719bb6e9f9a2ec5b47d9426c96986e";
/// The `tailscale.com` module version the nodes are built from.
const TAILSCALE: &str = "1.102.5";
/// Build tags for `tailscale` and `tailscaled`. Without DNS support, a `tailscaled` on macOS
/// skips its start-up clean-up of the system DNS (an `scutil` key and `/etc/resolver` files).
const TAILSCALE_TAGS: &str = "ts_omit_dns";
/// The application capability Slopty's policy reads (`slopty_tailnet::policy::CAP`; the live
/// test holds the two equal).
const CAP: &str = "github.com/aislopware/slopty";
/// The roles the policy grants `tag:ci` on `tag:slopty-worker`.
const CI_ROLES: [&str; 1] = ["agent"];
/// The variable a live test finds `fixture.json` through.
const FIXTURE_ENV: &str = "SLOPTY_TAILNET_FIXTURE";
/// How long `up` waits for each step.
const STEP: Duration = Duration::from_secs(90);
/// How long a daemon has to stop after it is asked.
const GRACE: Duration = Duration::from_secs(5);
/// The longest `sun_path` macOS takes, less its terminating NUL.
const SOCKET_PATH_MAX: usize = 103;

/// A node of the fixture.
struct NodeSpec {
    name: &'static str,
    tag: &'static str,
}

const NODES: [NodeSpec; 2] =
    [NodeSpec { name: "worker", tag: "tag:slopty-worker" }, NodeSpec { name: "ci", tag: "tag:ci" }];

/// What `up` writes for tests and `status` to read.
#[derive(Serialize, Deserialize)]
struct Fixture {
    /// The `up` process holding the daemons.
    owner: u32,
    /// The run dir: every daemon's state, logs and sockets.
    dir: Utf8PathBuf,
    /// The capability the policy grants under.
    cap: String,
    headscale: Headscale,
    nodes: Vec<Node>,
    /// Who holds what on whom: `{ src, dst, cap, value }` by node name.
    grants: Vec<Value>,
}

#[derive(Serialize, Deserialize)]
struct Headscale {
    version: String,
    url: String,
    binary: Utf8PathBuf,
    config: Utf8PathBuf,
    /// Its CLI's unix socket.
    socket: Utf8PathBuf,
    policy: Utf8PathBuf,
    ports: Ports,
}

#[derive(Serialize, Deserialize)]
struct Ports {
    http: u16,
    metrics: u16,
    grpc: u16,
    stun: u16,
}

#[derive(Serialize, Deserialize)]
struct Node {
    name: String,
    tags: Vec<String>,
    /// Its `LocalAPI`.
    socket: Utf8PathBuf,
    /// Its tailnet addresses, IPv4 first.
    ips: Vec<IpAddr>,
    dns_name: String,
    tailscale: Utf8PathBuf,
    version: String,
}

pub fn run(cmd: &TailnetCmd) -> Result<()> {
    match cmd {
        TailnetCmd::Up => up(),
        TailnetCmd::Down => down(),
        TailnetCmd::Status => status(),
        TailnetCmd::Test { nextest } => test(nextest),
        TailnetCmd::Guard { command } => guard(command),
    }
}

fn tailnet_dir() -> Result<Utf8PathBuf> {
    Ok(repo_root()?.join("target/tailnet"))
}

fn run_dir() -> Result<Utf8PathBuf> {
    Ok(tailnet_dir()?.join("run"))
}

/// The pid of the `up` that holds `run`, written before it starts anything.
fn owner_file(run: &Utf8Path) -> Utf8PathBuf {
    run.join("owner")
}

fn fixture_file(run: &Utf8Path) -> Utf8PathBuf {
    run.join("fixture.json")
}

// ---------------------------------------------------------------------------------------------
// test

/// The live tests that need the fixture.
const LIVE_TESTS: [&str; 4] = ["-p", "slopty-tailnet", "--test", "fixture"];

fn test(nextest: &[String]) -> Result<()> {
    let run = run_dir()?;
    ensure!(
        live_owner(&run).is_none(),
        "the tailnet fixture is already up; run the tests against it, or `cargo xtask tailnet down`"
    );
    fs::create_dir_all(tailnet_dir()?)?;
    let log = tailnet_dir()?.join("up.log");
    let out = File::create(&log).with_context(|| format!("creating {log}"))?;
    let mut up = Command::new(std::env::current_exe()?)
        .args(["tailnet", "up"])
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .spawn()
        .context("starting `tailnet up`")?;
    let result = ready(&mut up, &run, &log).and_then(|fixture| {
        let status = Command::new("cargo")
            .args(["nextest", "run"])
            .args(LIVE_TESTS)
            .args(nextest)
            .env(FIXTURE_ENV, fixture.as_str())
            .status()
            .context("running cargo nextest")?;
        ensure!(
            status.success(),
            "the tailnet live tests failed ({status}); the fixture's log is {log}"
        );
        Ok(())
    });
    // `up` takes everything down on SIGTERM, as on Ctrl-C.
    signal(up.id(), rustix::process::Signal::TERM);
    let _stopped: std::io::Result<ExitStatus> = up.wait();
    sweep(&run)?;
    result
}

/// Wait for `up` to write the fixture; fail when it exits first or takes too long.
fn ready(up: &mut Child, run: &Utf8Path, log: &Utf8Path) -> Result<Utf8PathBuf> {
    let fixture = fixture_file(run);
    // A first `up` also builds `tailscaled`: give it the steps and the build.
    let deadline = Instant::now().checked_add(STEP.saturating_mul(6)).context("a deadline")?;
    loop {
        if let Some(status) = up.try_wait()? {
            bail!("`tailnet up` exited ({status}) before the tailnet was ready; its log is {log}");
        }
        if fixture.exists() {
            return Ok(fixture);
        }
        ensure!(Instant::now() < deadline, "the tailnet was not ready in time; its log is {log}");
        pause(Duration::from_millis(200));
    }
}

// ---------------------------------------------------------------------------------------------
// up

fn up() -> Result<()> {
    let run = run_dir()?;
    if let Some(pid) = live_owner(&run) {
        bail!("the tailnet fixture is already up (pid {pid}); `cargo xtask tailnet down` first");
    }
    sweep(&run)?;
    if run.exists() {
        fs::remove_dir_all(&run).with_context(|| format!("removing the last run at {run}"))?;
    }
    let tools = tailnet_dir()?.join("tools");
    let headscale = headscale_binary(&tools)?;
    let (tailscale, tailscaled) = tailscale_binaries(&tools)?;
    watch_signals()?;
    fs::create_dir_all(&run)?;
    fs::write(owner_file(&run), std::process::id().to_string())?;
    let mut held = Held { run: run.clone(), daemons: Vec::new() };
    let result =
        start(&mut held, &headscale, &tailscale, &tailscaled).and_then(|()| hold(&mut held));
    held.stop();
    let result = match result {
        Err(e) if e.downcast_ref::<Stop>().is_some() => {
            println!("▶ {e}: the tailnet fixture stopped");
            Ok(())
        }
        other => other,
    };
    if result.is_ok() {
        fs::remove_dir_all(&run).with_context(|| format!("removing {run}"))?;
        println!("✓ the tailnet fixture is down");
    } else {
        // The logs stay for a look; the next `up` or `down` removes them.
        let _gone: std::io::Result<()> = fs::remove_file(fixture_file(&run));
        let _gone: std::io::Result<()> = fs::remove_file(owner_file(&run));
        eprintln!("✘ the tailnet fixture is down; its logs are under {run}");
    }
    result
}

/// The daemons `up` holds, each under its guard.
struct Held {
    run: Utf8PathBuf,
    daemons: Vec<Guarded>,
}

struct Guarded {
    name: String,
    guard: Child,
    /// `up`'s end of the guard's stdin: dropping it stops the daemon.
    pipe: Option<ChildStdin>,
    log: Utf8PathBuf,
}

impl Held {
    /// Fail when a signal asked `up` to stop or a daemon is gone.
    fn check(&mut self) -> Result<()> {
        let signal = SIGNALLED.load(Ordering::SeqCst);
        ensure!(signal == 0, Stop(signal));
        for daemon in &mut self.daemons {
            if let Some(status) = daemon.guard.try_wait()? {
                bail!("{} exited ({status}); its log is {}", daemon.name, daemon.log);
            }
        }
        Ok(())
    }

    /// Wait for `ready` to say yes, checking the daemons between tries.
    fn wait<T>(&mut self, what: &str, mut ready: impl FnMut() -> Result<Option<T>>) -> Result<T> {
        let deadline = Instant::now().checked_add(STEP).context("a deadline")?;
        let mut last = None;
        loop {
            self.check()?;
            match ready() {
                Ok(Some(value)) => return Ok(value),
                Ok(None) => {}
                Err(e) => last = Some(e),
            }
            if Instant::now() >= deadline {
                let why = last.map_or_else(String::new, |e| format!(": {e:#}"));
                bail!("{what} within {STEP:?}{why}");
            }
            pause(Duration::from_millis(200));
        }
    }

    fn spawn(
        &mut self,
        name: &str,
        program: &Utf8Path,
        args: &[String],
        env: &[(&str, &str)],
    ) -> Result<()> {
        let log = self.run.join(format!("{name}.log"));
        let out = File::create(&log).with_context(|| format!("creating {log}"))?;
        let mut guard = Command::new(std::env::current_exe()?)
            .args(["tailnet", "guard", "--"])
            .arg(program)
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(out.try_clone()?)
            .stderr(out)
            // Out of the terminal's group: Ctrl-C reaches `up` alone, which stops them in turn.
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {name}"))?;
        let pipe = guard.stdin.take();
        self.daemons.push(Guarded { name: name.to_owned(), guard, pipe, log });
        Ok(())
    }

    /// Close every guard's pipe, wait for them, then kill what is left of the run.
    fn stop(&mut self) {
        for daemon in &mut self.daemons {
            drop(daemon.pipe.take());
        }
        let deadline = Instant::now().checked_add(GRACE.saturating_mul(2));
        for daemon in &mut self.daemons {
            loop {
                match daemon.guard.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) if deadline.is_some_and(|d| Instant::now() < d) => {
                        pause(Duration::from_millis(50));
                    }
                    Ok(None) => {
                        let _killed: std::io::Result<()> = daemon.guard.kill();
                        let _reaped: std::io::Result<ExitStatus> = daemon.guard.wait();
                        break;
                    }
                }
            }
        }
        if let Err(e) = sweep(&self.run) {
            eprintln!("✘ sweeping {}: {e:#}", self.run);
        }
    }
}

/// A signal asked `up` to stop.
#[derive(Debug)]
struct Stop(i32);

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped by signal {}", self.0)
    }
}

fn start(
    held: &mut Held,
    headscale: &Utf8Path,
    tailscale: &Utf8Path,
    tailscaled: &Utf8Path,
) -> Result<()> {
    let run = held.run.clone();
    let hs_dir = run.join("headscale");
    fs::create_dir_all(&hs_dir)?;
    let ports = free_ports()?;
    let url = format!("http://127.0.0.1:{}", ports.http);
    let hs = Headscale {
        version: HEADSCALE.to_owned(),
        url: url.clone(),
        binary: headscale.to_owned(),
        config: hs_dir.join("config.yaml"),
        socket: hs_dir.join("headscale.sock"),
        policy: hs_dir.join("policy.json"),
        ports,
    };
    fits_sun_path(&hs.socket)?;
    fs::write(&hs.policy, serde_json::to_string_pretty(&policy())?)?;
    fs::write(&hs.config, headscale_config(&hs, &hs_dir)?)?;
    let config = hs.config.to_string();
    held.spawn("headscale", headscale, &["-c".to_owned(), config, "serve".to_owned()], &[])?;
    let http = hs.ports.http;
    held.wait("Headscale to listen", || {
        let listening = TcpStream::connect((Ipv4Addr::LOCALHOST, http)).is_ok();
        Ok((listening && hs.socket.exists()).then_some(()))
    })?;
    println!("✓ Headscale {HEADSCALE} at {url}");

    let mut nodes = Vec::new();
    for spec in &NODES {
        let dir = run.join(spec.name);
        fs::create_dir_all(&dir)?;
        let socket = dir.join("tailscaled.sock");
        fits_sun_path(&socket)?;
        let args = [
            "--tun=userspace-networking".to_owned(),
            format!("--statedir={dir}"),
            format!("--state={dir}/tailscaled.state"),
            format!("--socket={socket}"),
            "--port=0".to_owned(),
            "--no-logs-no-support".to_owned(),
        ];
        let logs = dir.to_string();
        held.spawn(
            spec.name,
            tailscaled,
            &args,
            &[("TS_LOGS_DIR", logs.as_str()), ("TS_DEBUG_USE_DERP_HTTP", "1")],
        )?;
        let fresh = held.wait(&format!("{} to answer on its socket", spec.name), || {
            if !socket.exists() {
                return Ok(None);
            }
            Ok(Some(cli_json(tailscale, &socket, &["status", "--json"])?))
        })?;
        let state = fresh.get("BackendState").and_then(Value::as_str).unwrap_or_default();
        ensure!(
            state != "Running",
            "{socket} answers as a node already running: not this run's fresh daemon"
        );
        let key_file = dir.join("authkey");
        fs::write(&key_file, preauth_key(&hs, spec.tag)?)?;
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))?;
        cli(
            tailscale,
            &socket,
            &[
                "up",
                &format!("--login-server={url}"),
                &format!("--auth-key=file:{key_file}"),
                &format!("--hostname={}", spec.name),
                "--accept-dns=false",
                "--timeout=60s",
            ],
        )
        .with_context(|| format!("logging {} in", spec.name))?;
        let prefs = cli_json(tailscale, &socket, &["debug", "prefs"])?;
        let control = prefs.get("ControlURL").and_then(Value::as_str).unwrap_or_default();
        ensure!(control == url, "{} logged in to {control}, not {url}", spec.name);
        nodes.push(Node {
            name: spec.name.to_owned(),
            tags: vec![spec.tag.to_owned()],
            socket,
            ips: Vec::new(),
            dns_name: String::new(),
            tailscale: tailscale.to_owned(),
            version: TAILSCALE.to_owned(),
        });
        println!("✓ {} logged in as {}", spec.name, spec.tag);
    }

    // Every node runs and has its addresses, and each sees every other.
    let statuses = held.wait("every node to see every other", || {
        let statuses = nodes
            .iter()
            .map(|n| cli_json(tailscale, &n.socket, &["status", "--json"]))
            .collect::<Result<Vec<_>>>()?;
        let selves: Vec<Vec<String>> = statuses.iter().map(self_ips).collect();
        let all_running = statuses
            .iter()
            .all(|s| s.get("BackendState").and_then(Value::as_str) == Some("Running"));
        let all_seen = statuses.iter().zip(&selves).all(|(status, own)| {
            let peers: Vec<String> = status
                .get("Peer")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter(|(_, p)| p.get("Online").and_then(Value::as_bool) == Some(true))
                .flat_map(|(_, p)| ips_at(p.get("TailscaleIPs")))
                .collect();
            selves
                .iter()
                .filter(|ips| *ips != own)
                .all(|ips| ips.first().is_some_and(|ip| peers.contains(ip)))
        });
        let ready = all_running && all_seen && selves.iter().all(|ips| !ips.is_empty());
        Ok(ready.then_some(statuses))
    })?;
    for (node, status) in nodes.iter_mut().zip(&statuses) {
        node.ips = self_ips(status)
            .iter()
            .map(|ip| ip.parse().with_context(|| format!("{ip} is an address")))
            .collect::<Result<_>>()?;
        status
            .pointer("/Self/DNSName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .clone_into(&mut node.dns_name);
    }

    // The worker sees ci's grant: the policy reached the nodes.
    let [worker, ci] = nodes.as_slice() else { bail!("the fixture has a worker and ci") };
    let ci_ip = ci.ips.first().context("ci has an address")?.to_string();
    held.wait("the worker to see ci's grant", || {
        let who = cli_json(tailscale, &worker.socket, &["whois", "--json", &ci_ip])?;
        Ok(who.get("CapMap").and_then(|caps| caps.get(CAP)).is_some().then_some(()))
    })?;

    let fixture = Fixture {
        owner: std::process::id(),
        dir: run.clone(),
        cap: CAP.to_owned(),
        headscale: hs,
        grants: vec![json!({ "src": "ci", "dst": "worker", "cap": CAP,
                             "value": { "roles": CI_ROLES } })],
        nodes,
    };
    let path = fixture_file(&run);
    let partial = run.join("fixture.json.partial");
    fs::write(&partial, serde_json::to_string_pretty(&fixture)?)?;
    fs::rename(&partial, &path)?;
    print_status(&path, &fixture);
    println!("✓ the tailnet fixture is up; Ctrl-C or `cargo xtask tailnet down` stops it");
    Ok(())
}

/// Hold the daemons until a signal ([`Stop`]), or until one of them dies.
fn hold(held: &mut Held) -> Result<()> {
    loop {
        held.check()?;
        pause(Duration::from_millis(200));
    }
}

/// The tailnet's policy: open between its nodes, and Slopty's capability from `tag:ci` to
/// `tag:slopty-worker`.
fn policy() -> Value {
    let tag_owners: serde_json::Map<String, Value> =
        NODES.iter().map(|n| (n.tag.to_owned(), json!([]))).collect();
    json!({
        "tagOwners": tag_owners,
        "grants": [
            { "src": ["*"], "dst": ["*"], "ip": ["*"] },
            { "src": ["tag:ci"], "dst": ["tag:slopty-worker"],
              "app": { CAP: [{ "roles": CI_ROLES }] } },
        ],
    })
}

/// Headscale's configuration: everything on loopback and under `dir`, its own DERP the only one.
fn headscale_config(hs: &Headscale, dir: &Utf8Path) -> Result<String> {
    // A JSON string is a YAML double-quoted scalar.
    let q = |s: &str| serde_json::to_string(s);
    let Ports { http, metrics, grpc, stun } = hs.ports;
    Ok(format!(
        r#"server_url: {url}
listen_addr: "127.0.0.1:{http}"
metrics_listen_addr: "127.0.0.1:{metrics}"
grpc_listen_addr: "127.0.0.1:{grpc}"
grpc_allow_insecure: false
noise:
  private_key_path: {noise}
prefixes:
  v4: 100.64.0.0/10
  v6: fd7a:115c:a1e0::/48
  allocation: sequential
derp:
  server:
    enabled: true
    region_id: 999
    region_code: "fixture"
    region_name: "Slopty fixture on loopback"
    verify_clients: true
    stun_listen_addr: "127.0.0.1:{stun}"
    private_key_path: {derp}
    automatically_add_embedded_derp_region: true
    ipv4: "127.0.0.1"
    ipv6: ""
  urls: []
  paths: []
  auto_update_enabled: false
  update_frequency: 24h
disable_check_updates: true
database:
  type: sqlite
  sqlite:
    path: {db}
    write_ahead_log: true
policy:
  mode: file
  path: {policy}
dns:
  magic_dns: true
  base_domain: fixture.slopty.test
  override_local_dns: false
  nameservers:
    global: []
unix_socket: {socket}
unix_socket_permission: "0700"
log:
  level: info
logtail:
  enabled: false
"#,
        url = q(&hs.url)?,
        noise = q(dir.join("noise_private.key").as_str())?,
        derp = q(dir.join("derp_server_private.key").as_str())?,
        db = q(dir.join("db.sqlite").as_str())?,
        policy = q(hs.policy.as_str())?,
        socket = q(hs.socket.as_str())?,
    ))
}

/// A single-use pre-auth key that tags its node with `tag`.
fn preauth_key(hs: &Headscale, tag: &str) -> Result<String> {
    let out =
        checked(
            Command::new(&hs.binary)
                .args(["-c", hs.config.as_str(), "preauthkeys", "create"])
                .args(["--tags", tag, "--expiration", "1h", "--output", "json"]),
        )?;
    let key: Value = serde_json::from_str(&out).context("Headscale's pre-auth key")?;
    key.get("key").and_then(Value::as_str).map(ToOwned::to_owned).context("a key in the answer")
}

/// Four distinct free loopback ports, held together while they are picked.
fn free_ports() -> Result<Ports> {
    let tcp: Vec<TcpListener> =
        std::iter::repeat_with(|| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
            .take(3)
            .collect::<std::io::Result<_>>()?;
    let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port =
        |i: usize| -> Result<u16> { Ok(tcp.get(i).context("a listener")?.local_addr()?.port()) };
    Ok(Ports { http: port(0)?, metrics: port(1)?, grpc: port(2)?, stun: udp.local_addr()?.port() })
}

fn fits_sun_path(socket: &Utf8Path) -> Result<()> {
    ensure!(
        socket.as_str().len() <= SOCKET_PATH_MAX,
        "{socket} is longer than a unix socket path may be ({SOCKET_PATH_MAX} bytes)"
    );
    Ok(())
}

/// A status's own addresses.
fn self_ips(status: &Value) -> Vec<String> {
    ips_at(status.pointer("/Self/TailscaleIPs"))
}

/// A node's `TailscaleIPs` as its status writes them.
fn ips_at(ips: Option<&Value>) -> Vec<String> {
    ips.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

/// `tailscale --socket <socket> <args>`: never the machine's own daemon.
fn cli(tailscale: &Utf8Path, socket: &Utf8Path, args: &[&str]) -> Result<String> {
    checked(Command::new(tailscale).arg(format!("--socket={socket}")).args(args))
}

fn cli_json(tailscale: &Utf8Path, socket: &Utf8Path, args: &[&str]) -> Result<Value> {
    let out = cli(tailscale, socket, args)?;
    serde_json::from_str(&out)
        .with_context(|| format!("`tailscale {}` wrote no JSON", args.join(" ")))
}

/// Run `command` and return its standard output, or fail with what it said.
fn checked(command: &mut Command) -> Result<String> {
    let out =
        command.stdin(Stdio::null()).output().with_context(|| format!("running {command:?}"))?;
    ensure!(
        out.status.success(),
        "{} failed ({}): {}",
        command.get_program().display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------------------------
// tools

/// Headscale's release binary, fetched once and checked against its pinned SHA-256.
fn headscale_binary(tools: &Utf8Path) -> Result<Utf8PathBuf> {
    let dir = tools.join(format!("headscale-{HEADSCALE}"));
    let binary = dir.join("headscale");
    if binary.is_file() {
        return Ok(binary);
    }
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "the tailnet fixture pins Headscale's macOS arm64 binary only"
    );
    fs::create_dir_all(&dir)?;
    let partial = dir.join("headscale.partial");
    let url = format!(
        "https://github.com/juanfont/headscale/releases/download/v{HEADSCALE}/headscale_{HEADSCALE}_darwin_arm64"
    );
    println!("▶ fetching Headscale {HEADSCALE} from {url}");
    checked(Command::new("curl").args(["-fsSL", "-o", partial.as_str(), &url]))?;
    let sum = checked(Command::new("shasum").args(["-a", "256", "-b", partial.as_str()]))?;
    let got = sum.split_whitespace().next().unwrap_or_default();
    if got != HEADSCALE_SHA256 {
        fs::remove_file(&partial)?;
        bail!("{url}: SHA-256 {got}, pinned {HEADSCALE_SHA256}");
    }
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o755))?;
    fs::rename(&partial, &binary)?;
    Ok(binary)
}

/// `tailscale` and `tailscaled` at [`TAILSCALE`], built once with the local Go.
fn tailscale_binaries(tools: &Utf8Path) -> Result<(Utf8PathBuf, Utf8PathBuf)> {
    let dir = tools.join(format!("tailscale-{TAILSCALE}-{TAILSCALE_TAGS}"));
    let (cli, daemon) = (dir.join("tailscale"), dir.join("tailscaled"));
    if !(cli.is_file() && daemon.is_file()) {
        let go = go()?;
        let partial = tools.join(format!("tailscale-{TAILSCALE}.partial"));
        if partial.exists() {
            fs::remove_dir_all(&partial)?;
        }
        fs::create_dir_all(&partial)?;
        println!("▶ building tailscale and tailscaled {TAILSCALE} with {go}");
        let status = Command::new(&go)
            .args(["install", "-tags", TAILSCALE_TAGS])
            .arg(format!("tailscale.com/cmd/tailscale@v{TAILSCALE}"))
            .arg(format!("tailscale.com/cmd/tailscaled@v{TAILSCALE}"))
            .env("GOBIN", &partial)
            // Cargo's `[env]` sets these for Slopty's own builds: with the iOS one, clang links
            // cgo for iOS, and the macOS one disagrees with the objects Go builds for this SDK.
            .env_remove("IPHONEOS_DEPLOYMENT_TARGET")
            .env_remove("MACOSX_DEPLOYMENT_TARGET")
            .status()
            .with_context(|| format!("running {go}"))?;
        ensure!(status.success(), "`go install` of tailscale {TAILSCALE} failed ({status})");
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::rename(&partial, &dir)?;
    }
    let said = checked(Command::new(&daemon).arg("--version"))?;
    ensure!(
        said.lines().next().is_some_and(|l| l.starts_with(TAILSCALE)),
        "{daemon} is tailscaled {}, not {TAILSCALE}",
        said.lines().next().unwrap_or_default()
    );
    Ok((cli, daemon))
}

/// Go, from `PATH` or where Homebrew and the official installer put it.
fn go() -> Result<Utf8PathBuf> {
    let on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| Utf8Path::new(d).join("go"))
        .find(|p| p.is_file());
    on_path
        .or_else(|| {
            ["/opt/homebrew/bin/go", "/usr/local/go/bin/go"]
                .into_iter()
                .map(Utf8PathBuf::from)
                .find(|p| p.is_file())
        })
        .context(
            "the tailnet fixture builds tailscaled with Go, and none is installed: `brew install go`",
        )
}

// ---------------------------------------------------------------------------------------------
// down and status

fn down() -> Result<()> {
    let run = run_dir()?;
    if let Some(pid) = live_owner(&run) {
        println!("▶ stopping `tailnet up` (pid {pid})");
        signal(pid, rustix::process::Signal::TERM);
        let deadline = Instant::now().checked_add(GRACE.saturating_mul(4)).context("a deadline")?;
        while live_owner(&run).is_some() && Instant::now() < deadline {
            pause(Duration::from_millis(100));
        }
        if live_owner(&run).is_some() {
            println!("  it did not stop in time; killing it");
            signal(pid, rustix::process::Signal::KILL);
        }
    }
    sweep(&run)?;
    if run.exists() {
        fs::remove_dir_all(&run).with_context(|| format!("removing {run}"))?;
    }
    ensure!(ours(&run)?.is_empty(), "processes of {run} are still running");
    println!("✓ the tailnet fixture is down: no process of {run} runs");
    Ok(())
}

fn status() -> Result<()> {
    let run = run_dir()?;
    let path = fixture_file(&run);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            bail!("the tailnet fixture is not up: `cargo xtask tailnet up`")
        }
        Err(e) => return Err(e).with_context(|| format!("reading {path}")),
    };
    let fixture: Fixture =
        serde_json::from_str(&text).with_context(|| format!("reading {path}"))?;
    ensure!(
        live_owner(&run) == Some(fixture.owner),
        "the tailnet fixture's `up` (pid {}) is gone: `cargo xtask tailnet down`",
        fixture.owner
    );
    print_status(&path, &fixture);
    Ok(())
}

/// One `key=value` line for the fixture's path, one for Headscale, one per node.
fn print_status(path: &Utf8Path, fixture: &Fixture) {
    println!("{FIXTURE_ENV}={path}");
    let hs = &fixture.headscale;
    println!("headscale url={} socket={} config={}", hs.url, hs.socket, hs.config);
    for node in &fixture.nodes {
        let ips: Vec<String> = node.ips.iter().map(ToString::to_string).collect();
        println!(
            "node name={} socket={} ips={} tags={} dns={}",
            node.name,
            node.socket,
            ips.join(","),
            node.tags.join(","),
            node.dns_name
        );
    }
}

/// The pid of the `tailnet up` holding `run`, if it still runs.
fn live_owner(run: &Utf8Path) -> Option<u32> {
    let pid: u32 = fs::read_to_string(owner_file(run)).ok()?.trim().parse().ok()?;
    let command =
        checked(Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()])).ok()?;
    (command.contains("xtask") && command.contains("tailnet up")).then_some(pid)
}

/// Every process whose command line names `run`: this run's guards and daemons, and nothing
/// else (the user's own Tailscale never names it).
fn ours(run: &Utf8Path) -> Result<Vec<u32>> {
    let listing = checked(Command::new("ps").args(["-A", "-ww", "-o", "pid=,command="]))?;
    let me = std::process::id();
    Ok(listing
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(' ')?;
            let pid: u32 = pid.parse().ok()?;
            (pid != me && command.contains(run.as_str())).then_some(pid)
        })
        .collect())
}

/// Stop every process of `run`: SIGTERM, then SIGKILL after [`GRACE`].
fn sweep(run: &Utf8Path) -> Result<()> {
    let left = ours(run)?;
    if left.is_empty() {
        return Ok(());
    }
    println!("▶ stopping {} process(es) left from {run}", left.len());
    for &pid in &left {
        signal(pid, rustix::process::Signal::TERM);
    }
    let deadline = Instant::now().checked_add(GRACE).context("a deadline")?;
    while !ours(run)?.is_empty() && Instant::now() < deadline {
        pause(Duration::from_millis(100));
    }
    for pid in ours(run)? {
        signal(pid, rustix::process::Signal::KILL);
    }
    Ok(())
}

fn signal(pid: u32, signal: rustix::process::Signal) {
    if let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) {
        let _sent: rustix::io::Result<()> = rustix::process::kill_process(pid, signal);
    }
}

// ---------------------------------------------------------------------------------------------
// the guard

/// Run `command` until stdin reaches its end (`up` closed its pipe, or ended), then stop it:
/// SIGTERM, and SIGKILL after [`GRACE`]. A daemon that exits on its own ends the guard with it.
fn guard(command: &[OsString]) -> Result<()> {
    let (program, args) = command.split_first().context("a command to guard")?;
    let mut daemon = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", program.display()))?;
    let (closed, on_close) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut sink = [0_u8; 64];
        let mut stdin = std::io::stdin().lock();
        while stdin.read(&mut sink).is_ok_and(|n| n > 0) {}
        drop(closed);
    });
    loop {
        if let Some(status) = daemon.try_wait()? {
            bail!("{} exited on its own ({status})", program.display());
        }
        match on_close.recv_timeout(Duration::from_millis(200)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    signal(daemon.id(), rustix::process::Signal::TERM);
    let deadline = Instant::now().checked_add(GRACE).context("a deadline")?;
    while daemon.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            daemon.kill()?;
            daemon.wait()?;
            return Err(anyhow!("{} did not stop within {GRACE:?}; killed", program.display()));
        }
        pause(Duration::from_millis(50));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// signals

/// The signal that asked `up` to stop, 0 until one did.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNALLED.store(signal, Ordering::SeqCst);
}

/// On SIGINT, SIGTERM or SIGHUP, record the signal; `up` sees it at its next check and stops.
fn watch_signals() -> Result<()> {
    let handler: extern "C" fn(libc::c_int) = on_signal;
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        #[expect(
            clippy::fn_to_numeric_cast_any,
            reason = "signal(3) takes the handler's address as a sighandler_t"
        )]
        let address = handler as libc::sighandler_t;
        // SAFETY: `signal(3)` installs a handler; `on_signal` only stores to a lock-free atomic,
        // which POSIX allows a signal handler to do (async-signal-safe).
        let previous = unsafe { libc::signal(signal, address) };
        ensure!(previous != libc::SIG_ERR, "install a handler for signal {signal}");
    }
    Ok(())
}

#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling")]
fn pause(duration: Duration) {
    std::thread::sleep(duration);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy opens the tailnet and grants Slopty's roles from ci to the worker alone, and
    /// declares an owner entry for every tag a node takes.
    #[test]
    fn the_policy_grants_ci_its_roles_on_the_worker() {
        let policy = policy();
        let grants = policy["grants"].as_array().unwrap();
        let app: Vec<&Value> = grants.iter().filter(|g| g.get("app").is_some()).collect();
        assert_eq!(app.len(), 1, "{policy}");
        let grant = app[0];
        assert_eq!(grant["src"], json!(["tag:ci"]));
        assert_eq!(grant["dst"], json!(["tag:slopty-worker"]));
        assert_eq!(grant["app"][CAP], json!([{ "roles": ["agent"] }]));
        for node in &NODES {
            assert!(policy["tagOwners"].get(node.tag).is_some(), "{} has an owner entry", node.tag);
        }
    }

    /// The config quotes every path, keeps every listener on loopback and has no DERP but its
    /// own.
    #[test]
    fn headscale_listens_on_loopback_only() {
        let dir = Utf8Path::new("/tmp/a dir: with \"quotes\"");
        let hs = Headscale {
            version: HEADSCALE.to_owned(),
            url: "http://127.0.0.1:1".to_owned(),
            binary: dir.join("headscale"),
            config: dir.join("config.yaml"),
            socket: dir.join("headscale.sock"),
            policy: dir.join("policy.json"),
            ports: Ports { http: 1, metrics: 2, grpc: 3, stun: 4 },
        };
        let config = headscale_config(&hs, dir).unwrap();
        assert!(config.contains(r#"policy.json""#) && config.contains(r#"\"quotes\""#), "{config}");
        for line in config.lines().filter(|l| l.contains("_addr:")) {
            assert!(line.contains("127.0.0.1:"), "{line}");
        }
        assert!(config.contains("  urls: []\n"), "no DERP map from outside");
    }

    /// Ports picked together are distinct.
    #[test]
    fn free_ports_are_distinct() {
        let Ports { http, metrics, grpc, .. } = free_ports().unwrap();
        assert!(http != metrics && metrics != grpc && http != grpc);
    }
}
