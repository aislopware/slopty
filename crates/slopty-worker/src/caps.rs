//! What this worker can do ([`WorkerCaps`]) and how loaded it is, for the server's directory.
//!
//! Most of it is fixed for the life of the daemon (OS, CPUs, memory, encoders, the agents on
//! `PATH`). A Screen Recording or Accessibility grant, a display attached and the load change
//! without anyone telling us, so [`watch()`] looks at them every 5 s and publishes a new value
//! only when something a caller would act on changed. The load goes out on its own, so what
//! the worker can do compares equal until it changes.

use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::ffi::CStr;
use std::time::Duration;

use slopty_proto::project::{Fact, Facts};
use slopty_proto::screen::{DisplayInfo, VideoCodec};
use slopty_proto::server::{InstalledAgent, Os, WorkerCaps};
use slopty_proto::thread::AgentId;
use tokio::sync::watch;

/// How often permissions and displays are looked at: TCC and display changes come with no
/// notification a daemon can take.
const CHECK_PERIOD: Duration = Duration::from_secs(5);
/// How often what costs a child process is read ([`Seldom`]): settings people rarely touch.
const SELDOM_PERIOD: Duration = Duration::from_mins(1);
/// How often a load change is reported, and by how much it must have moved.
const LOAD_PERIOD: Duration = Duration::from_secs(30);
const LOAD_STEP: f32 = 0.5;
/// How long `pmset` may take.
#[cfg(target_os = "macos")]
const PMSET_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an agent command's output kept: `claude agents --json` lists every session.
const OUTPUT_CAP: usize = 4 << 20;

/// What the worker could not keep on its own disk, by what it keeps, while each fails: a
/// full or read-only volume costs a restart its terminals' screens and the agents' threads,
/// and says so only in the log otherwise. Process-wide, as the disk is.
static NOT_WRITTEN: std::sync::LazyLock<parking_lot::Mutex<BTreeMap<&'static str, String>>> =
    std::sync::LazyLock::new(parking_lot::Mutex::default);

/// `what` (`"Thread logs"`) was written: it no longer fails.
pub fn wrote(what: &'static str) {
    NOT_WRITTEN.lock().remove(what);
}

/// `what` (`"Thread logs"`) could not be written, for `error`; said in [`WorkerCaps`] until
/// it is written again.
pub fn not_written(what: &'static str, error: &dyn std::fmt::Display) {
    NOT_WRITTEN.lock().insert(what, format!("{what} cannot be written: {error}"));
}

/// The first of what fails to be written, in words, while anything does.
#[must_use]
pub fn writes_failing() -> Option<String> {
    NOT_WRITTEN.lock().values().next().cloned()
}

/// The agents with an adapter of their own, by the program [`crate::facts::agents`] names.
const ADAPTED: [(&str, &str); 3] =
    [("claude", AgentId::CLAUDE_CODE), ("codex", AgentId::CODEX), ("pi", AgentId::PI)];

/// The coding agents a thread can be started of here, with the versions they report.
///
/// Claude Code, Codex and pi, then each agent reached over ACP whose program is here, the
/// person's own (`own_acp`, from `[worker.acp]`) among the known ones. What a client offers to
/// start on this machine is exactly this, with no server needed.
pub async fn installed_agents(own_acp: &BTreeMap<String, Vec<String>>) -> Vec<InstalledAgent> {
    let (agents, acp) = crate::facts::agents(own_acp).await;
    agents_in(&agents, &acp)
}

/// The agents a thread can be started of, from the `agents` and `acp` facts.
///
/// Those with an adapter come in its order, then the ACP ones by name. An agent with no adapter
/// (`aider`) is not startable, and an ACP agent that says no version is listed with none.
#[must_use]
pub fn agents_in(agents: &Facts, acp: &Facts) -> Vec<InstalledAgent> {
    let text = |fact: &Fact| match fact {
        Fact::Text(version) => version.clone(),
        _ => String::new(),
    };
    let adapted = ADAPTED.iter().filter_map(|(program, name)| {
        let version = text(agents.get(*program)?);
        Some(InstalledAgent { agent: AgentId::named(name), version })
    });
    let reached = acp
        .iter()
        .map(|(name, fact)| InstalledAgent { agent: AgentId::acp(name), version: text(fact) });
    adapted.chain(reached).collect()
}

/// What an agent's command line `program args…` prints, when it succeeds within `wait`.
///
/// It runs straight when the daemon's own `PATH` has the program, else through the person's
/// login shell, as ptyd runs such a program.
pub async fn agent_output(program: &str, args: &[&str], wait: Duration) -> Option<Vec<u8>> {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()));
    let shell = (!on_path).then(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned()));
    let ran =
        crate::facts::run(agent_command(program, args, shell.as_deref()), wait, OUTPUT_CAP).await?;
    ran.ok.then(|| ran.out.into_bytes())
}

/// `program args…` run straight, or through `shell` as a login shell.
///
/// The login shell `exec`s the program: an interactive zsh runs a command in a process group
/// of its own (job control), which the group [`crate::facts::run`] kills on a time-out would
/// miss, leaving the program to outlive the wait.
fn agent_command(program: &str, args: &[&str], shell: Option<&str>) -> tokio::process::Command {
    let Some(shell) = shell else {
        let mut direct = tokio::process::Command::new(program);
        direct.args(args);
        return direct;
    };
    let line = std::iter::once(program)
        .chain(args.iter().copied())
        .map(slopty_core::shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let mut login = tokio::process::Command::new(shell);
    login.args(["-l", "-i", "-c", &format!("exec {line}")]);
    login
}

/// What of a worker is read only once a minute, each answer costing a child process.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Seldom {
    /// [`wake_on_lan`].
    pub wake_on_lan: Option<bool>,
    /// What the person runs so the services outlive their last logout, while they do not
    /// (`slopty_platform::service::Session::stops_at_logout`).
    pub stops_at_logout: Option<String>,
}

impl Seldom {
    /// Both, read now.
    pub async fn read() -> Self {
        let stops = tokio::task::spawn_blocking(|| {
            slopty_platform::service::Session::native().stops_at_logout()
        });
        let wake_on_lan = wake_on_lan().await;
        Self { wake_on_lan, stops_at_logout: stops.await.ok().flatten() }
    }
}

/// Everything about this worker as it is now; `agents` from [`installed_agents`] and the rest
/// from [`Seldom::read`].
pub fn probe(agents: &[InstalledAgent], seldom: &Seldom) -> WorkerCaps {
    let desktop = desktop();
    WorkerCaps {
        os: if cfg!(target_os = "linux") { Os::Linux } else { Os::MacOs },
        os_version: os_version(),
        arch: std::env::consts::ARCH.to_owned(),
        cpus: std::thread::available_parallelism()
            .map_or(1, |n| u16::try_from(n.get()).unwrap_or(u16::MAX)),
        memory: memory(),
        encoders: desktop.encoders,
        displays: desktop.displays,
        agents: agents.to_vec(),
        can_capture: desktop.can_capture,
        can_inject: desktop.can_inject,
        virtual_displays: desktop.virtual_displays,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        lan: slopty_tailnet::lan::ports(),
        wake_on_lan: seldom.wake_on_lan,
        writes_failing: writes_failing(),
        stops_at_logout: seldom.stops_at_logout.clone(),
    }
}

/// Whether this Mac wakes for a magic packet: `womp` in `pmset -g`, the current power
/// source's "Wake for network access". `None` where that cannot be read.
#[cfg(target_os = "macos")]
pub async fn wake_on_lan() -> Option<bool> {
    let output = tokio::process::Command::new("/usr/bin/pmset")
        .arg("-g")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(PMSET_TIMEOUT, output).await.ok()?.ok()?;
    womp(&String::from_utf8_lossy(&output.stdout))
}

/// Linux says per card whether it wakes for a magic packet, behind `ethtool`'s ioctl; unread.
#[cfg(not(target_os = "macos"))]
#[expect(clippy::unused_async, reason = "the macOS reader's shape")]
pub async fn wake_on_lan() -> Option<bool> {
    None
}

/// `womp` in `pmset -g`'s settings: ` womp                 1`.
#[cfg(any(target_os = "macos", test))]
fn womp(settings: &str) -> Option<bool> {
    settings.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == "womp").then(|| words.next()).flatten().map(|value| value != "0")
    })
}

/// What this worker offers of its desktop.
struct Desktop {
    encoders: Vec<VideoCodec>,
    displays: Vec<DisplayInfo>,
    can_capture: bool,
    can_inject: bool,
    virtual_displays: bool,
}

/// macOS: both codecs, and the displays and input its grants allow.
#[cfg(target_os = "macos")]
fn desktop() -> Desktop {
    let can_capture = slopty_capture::can_capture();
    Desktop {
        // Every Apple-silicon Mac encodes both in hardware (VideoToolbox).
        encoders: vec![VideoCodec::Hevc, VideoCodec::H264],
        // Without Screen Recording no display can be streamed, so none is offered.
        displays: if can_capture { slopty_capture::active_displays() } else { Vec::new() },
        can_capture,
        can_inject: slopty_input::can_post(),
        // A display made for a client is streamed like any other, so it needs the grant too.
        virtual_displays: can_capture && slopty_vdisplay::available(),
    }
}

/// Linux streams no desktop yet (`docs/decisions/platform.md`, "Linux seams"): no encoder, no
/// display, no capture and no input, so no client offers any.
#[cfg(not(target_os = "macos"))]
const fn desktop() -> Desktop {
    Desktop {
        encoders: Vec::new(),
        displays: Vec::new(),
        can_capture: false,
        can_inject: false,
        virtual_displays: false,
    }
}

/// macOS's product version (`26.5`).
#[cfg(target_os = "macos")]
fn os_version() -> String {
    sysctl_string(c"kern.osproductversion").unwrap_or_default()
}

/// The distribution and its version from `/etc/os-release` (`ubuntu 24.04`).
#[cfg(target_os = "linux")]
fn os_version() -> String {
    std::fs::read_to_string("/etc/os-release").map(|text| os_release(&text)).unwrap_or_default()
}

/// `ID` and `VERSION_ID` of an `os-release` file, unquoted, as one string.
#[cfg(any(target_os = "linux", test))]
fn os_release(text: &str) -> String {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .map(|value| value.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|value| !value.is_empty())
    };
    [field("ID"), field("VERSION_ID")].into_iter().flatten().collect::<Vec<_>>().join(" ")
}

/// Physical memory, bytes.
#[cfg(target_os = "macos")]
fn memory() -> u64 {
    sysctl_u64(c"hw.memsize").unwrap_or(0)
}

/// Physical memory, bytes: `sysinfo`'s total in its memory unit.
#[cfg(target_os = "linux")]
fn memory() -> u64 {
    let info = rustix::system::sysinfo();
    info.totalram.saturating_mul(u64::from(info.mem_unit))
}

/// Keep `caps` and `load` current until every receiver of both is gone, with the agents
/// installed as `agents` last says: a change of them is probed at once.
///
/// Permissions and displays every 5 s (the displays from CoreGraphics, since a ScreenCaptureKit
/// enumeration that often raises the private-window consent prompt again and again), the load
/// every 30 s when it moved by more than 0.5.
pub async fn watch(
    caps: watch::Sender<WorkerCaps>,
    load: watch::Sender<f32>,
    mut agents: watch::Receiver<Vec<InstalledAgent>>,
) {
    let mut tick = tokio::time::interval(CHECK_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut load_at = tokio::time::Instant::now();
    let mut seldom = Seldom::default();
    let mut seldom_at: Option<tokio::time::Instant> = None;
    let mut following = true;
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            changed = agents.changed(), if following => following = changed.is_ok(),
        }
        if caps.is_closed() && load.is_closed() {
            return;
        }
        if seldom_at.is_none_or(|at| at.elapsed() >= SELDOM_PERIOD) {
            let now = Seldom::read().await;
            let was = seldom_at.map(|_| &seldom);
            if now.wake_on_lan == Some(false)
                && was.is_none_or(|was| was.wake_on_lan != now.wake_on_lan)
            {
                tracing::warn!(
                    "Wake for network access is off: a client cannot wake this machine once it \
                     sleeps (`sudo pmset -a womp 1`)"
                );
            }
            if let Some(how) = &now.stops_at_logout
                && was.is_none_or(|was| was.stops_at_logout != now.stops_at_logout)
            {
                tracing::warn!("the worker's services stop at logout: {how}");
            }
            seldom = now;
            seldom_at = Some(tokio::time::Instant::now());
        }
        let installed = agents.borrow_and_update().clone();
        let next = probe(&installed, &seldom);
        caps.send_if_modified(|current| {
            let changed = *current != next;
            if changed {
                *current = next;
            }
            changed
        });
        if load_at.elapsed() >= LOAD_PERIOD {
            load_at = tokio::time::Instant::now();
            let now = self::load();
            load.send_if_modified(|current| {
                let moved = (now - *current).abs() > LOAD_STEP;
                if moved {
                    *current = now;
                }
                moved
            });
        }
    }
}

/// The one-minute load average.
#[must_use]
pub fn load() -> f32 {
    let mut avg = [0.0_f64; 1];
    // SAFETY: `getloadavg` writes at most `nelem` (1) doubles into the buffer, which holds one.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    let one = if n == 1 { avg.first().copied().unwrap_or(0.0) } else { 0.0 };
    #[expect(clippy::cast_possible_truncation, reason = "a load average fits an f32")]
    let load = one as f32;
    load
}

/// A string sysctl, such as `kern.osproductversion`.
#[cfg(target_os = "macos")]
fn sysctl_string(name: &CStr) -> Option<String> {
    let mut buf = [0_u8; 256];
    let mut len = buf.len();
    // SAFETY: `sysctlbyname` writes at most `len` bytes into `buf`, which is that long, and
    // stores the length it wrote back into `len`; no new value is set (null, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let bytes = buf.get(..len.min(buf.len()))?;
    let text = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
    Some(text.to_owned())
}

/// A 64-bit integer sysctl, such as `hw.memsize`.
#[cfg(target_os = "macos")]
fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut value: u64 = 0;
    let mut len = size_of::<u64>();
    // SAFETY: `sysctlbyname` writes at most `len` (8) bytes into `value`, a u64, and stores
    // the length it wrote into `len`; no new value is set (null, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut value).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == size_of::<u64>()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A write that fails is said in the caps until the same kind of write goes through
    /// again; another kind failing beside it is said once the first is fixed.
    #[test]
    fn a_failing_write_is_said_until_one_goes_through() {
        const WHAT: &str = "Test writes";
        const OTHER: &str = "Test writes too";
        not_written(WHAT, &"No space left on device");
        let said = writes_failing().expect("said");
        assert!(said.starts_with("Test writes"), "{said}");
        assert_eq!(probe(&[], &Seldom::default()).writes_failing.as_deref(), Some(said.as_str()));
        not_written(OTHER, &"Read-only file system");
        wrote(WHAT);
        assert_eq!(
            writes_failing().as_deref(),
            Some("Test writes too cannot be written: Read-only file system")
        );
        wrote(OTHER);
        assert_eq!(writes_failing(), None);
    }

    /// What a machine can start is every agent with an adapter its facts found, by the name its
    /// threads carry, then each ACP agent found, the one that says no version listed with none;
    /// a program with no adapter (aider) is not startable.
    #[test]
    fn the_agents_found_are_what_a_thread_can_be_started_of() {
        let text = |v: &str| Fact::Text(v.to_owned());
        let agents: Facts = [
            ("aider".to_owned(), text("0.86.1")),
            ("codex".to_owned(), text("0.157.0")),
            ("claude".to_owned(), text("2.1.286")),
            ("pi".to_owned(), text("0.42.1")),
        ]
        .into();
        let acp: Facts =
            [("gemini".to_owned(), text("0.9.0")), ("goose".to_owned(), Fact::Bool(true))].into();
        let found = agents_in(&agents, &acp);
        let named: Vec<(&str, &str)> =
            found.iter().map(|a| (a.agent.0.as_str(), a.version.as_str())).collect();
        assert_eq!(
            named,
            [
                ("claude-code", "2.1.286"),
                ("codex", "0.157.0"),
                ("pi", "0.42.1"),
                ("acp:gemini", "0.9.0"),
                ("acp:goose", ""),
            ]
        );
        assert_eq!(agents_in(&Facts::new(), &Facts::new()), []);
    }

    /// A Linux worker names its distribution and version, quoted or not.
    #[test]
    fn os_release_names_the_distribution_and_version() {
        let ubuntu = "NAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(os_release(ubuntu), "ubuntu 24.04");
        assert_eq!(os_release("ID='arch'\nBUILD_ID=rolling\n"), "arch", "rolling: no version");
        assert_eq!(os_release(""), "");
    }

    /// `womp` reads from `pmset -g`'s settings; a listing without it says nothing.
    #[test]
    fn wake_for_network_access_reads_from_the_settings() {
        let settings = "System-wide power settings:\nCurrently in use:\n standby              0\n \
                        networkoversleep     0\n womp                 1\n";
        assert_eq!(womp(settings), Some(true));
        assert_eq!(womp(&settings.replace("womp                 1", "womp 0")), Some(false));
        assert_eq!(womp(" standby 0\n"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_probe_reads_this_mac() {
        let caps = probe(&[], &Seldom::default());
        assert!(
            caps.os_version.split('.').next().is_some_and(|major| major.parse::<u32>().is_ok()),
            "{caps:?}"
        );
        assert!(caps.memory >= 1 << 30, "{caps:?}");
        assert!(caps.cpus >= 1);
        assert_eq!(caps.arch, "aarch64");
        assert!(load() >= 0.0);
        assert_eq!(caps.encoders, [VideoCodec::Hevc, VideoCodec::H264]);
        assert!(caps.lan.iter().all(|port| port.mac.is_unicast()), "{:?}", caps.lan);
    }

    /// This Mac says whether it wakes for a magic packet.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn this_mac_says_whether_it_wakes_on_lan() {
        assert!(wake_on_lan().await.is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unknown_sysctl_is_none() {
        assert_eq!(sysctl_string(c"slopty.no.such.name"), None);
        assert_eq!(sysctl_u64(c"slopty.no.such.name"), None);
    }

    /// A fake `claude` that starts a child of its own, says the child's pid in `pidfile`, and
    /// waits on it: hung, as a `claude agents` stuck on the network.
    fn hung_claude(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let program = dir.join("claude");
        let pidfile = dir.join("child.pid");
        let script = format!(
            "#!/bin/sh\nsleep 600 &\necho $! > {}\nwait\n",
            slopty_core::shell_quote(&pidfile.to_string_lossy())
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        (program, pidfile)
    }

    /// The pid a hung `claude` wrote, once it has.
    async fn child_of(pidfile: &std::path::Path) -> rustix::process::Pid {
        let written = async {
            loop {
                let text = tokio::fs::read_to_string(pidfile).await.unwrap_or_default();
                if let Ok(pid) = text.trim().parse::<i32>() {
                    return rustix::process::Pid::from_raw(pid).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), written).await.expect("it started")
    }

    /// Whether `pid` is gone within a few seconds.
    async fn gone(pid: rustix::process::Pid) -> bool {
        for _ in 0..500 {
            if rustix::process::test_kill_process(pid).is_err() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// A `claude` that hangs past the wait is killed with everything it started, run straight
    /// or through a login shell whose job control would put it in a group of its own.
    #[tokio::test]
    async fn a_hung_agent_command_dies_with_its_children_after_the_wait() {
        let dir = tempfile::tempdir().unwrap();
        let (program, pidfile) = hung_claude(dir.path());
        let program = program.to_string_lossy().into_owned();
        let mut shells = vec![None, Some("/bin/sh")];
        if std::path::Path::new("/bin/zsh").exists() {
            shells.push(Some("/bin/zsh"));
        }
        for shell in shells {
            if pidfile.exists() {
                std::fs::remove_file(&pidfile).unwrap();
            }
            let mut command = agent_command(&program, &[], shell);
            // The login shell reads no rc of the person's.
            command.env("HOME", dir.path()).env("ZDOTDIR", dir.path());
            let waited = std::time::Instant::now();
            let ran = tokio::join!(
                crate::facts::run(command, Duration::from_millis(1500), OUTPUT_CAP),
                child_of(&pidfile)
            );
            assert!(ran.0.is_none(), "{shell:?}: no answer past the wait");
            assert!(waited.elapsed() < Duration::from_secs(10), "{shell:?}");
            assert!(gone(ran.1).await, "{shell:?}: its child outlived the wait");
        }
    }

    /// A poll the worker stops waiting on (it shuts down, the task is aborted) takes its
    /// command's whole group with it.
    #[tokio::test]
    async fn an_abandoned_agent_command_dies_with_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let (program, pidfile) = hung_claude(dir.path());
        let command = agent_command(&program.to_string_lossy(), &[], None);
        let polling =
            tokio::spawn(crate::facts::run(command, Duration::from_secs(600), OUTPUT_CAP));
        let child = child_of(&pidfile).await;
        polling.abort();
        assert!(matches!(polling.await, Err(e) if e.is_cancelled()));
        assert!(gone(child).await, "its child outlived the poll");
    }

    /// Through a login shell a program still answers with what it printed.
    #[tokio::test]
    async fn an_agent_command_answers_through_a_login_shell() {
        let ran = crate::facts::run(
            agent_command("printf", &["1.2.3\\n"], Some("/bin/sh")),
            Duration::from_secs(10),
            OUTPUT_CAP,
        )
        .await
        .unwrap();
        assert!(ran.ok);
        assert_eq!(ran.out, "1.2.3");
    }
}
