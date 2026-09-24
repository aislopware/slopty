//! What this worker can do and how it is doing ([`WorkerCaps`]), for the server's directory.
//!
//! Most of it is fixed for the life of the daemon (OS, CPUs, memory, encoders, the agents on
//! `PATH`). A Screen Recording or Accessibility grant, a display attached and the load change
//! without anyone telling us, so [`watch()`] looks at them every 5 s and publishes a new value
//! only when something a caller would act on changed.

use std::ffi::CStr;
use std::time::Duration;

use slopty_proto::agent::AgentKind;
use slopty_proto::screen::VideoCodec;
use slopty_proto::server::{DisplayCap, InstalledAgent, Os, WorkerCaps};
use tokio::sync::watch;

/// How often permissions and displays are looked at: TCC and display changes come with no
/// notification a daemon can take.
const CHECK_PERIOD: Duration = Duration::from_secs(5);
/// How often a load change is reported, and by how much it must have moved.
const LOAD_PERIOD: Duration = Duration::from_secs(30);
const LOAD_STEP: f32 = 0.5;
/// How long `claude --version` may take, login shell included.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// The coding agents installed here, with the versions they report.
///
/// Runs each agent's `--version` once, as a child process (through the login shell when the
/// daemon's own `PATH` does not have it, as ptyd runs such a program).
pub async fn installed_agents() -> Vec<InstalledAgent> {
    let mut out = Vec::new();
    if let Some(version) = version_of("claude").await {
        out.push(InstalledAgent { kind: AgentKind::ClaudeCode, version });
    }
    out
}

async fn version_of(program: &str) -> Option<String> {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()));
    let mut command = if on_path {
        let mut direct = tokio::process::Command::new(program);
        direct.arg("--version");
        direct
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
        let mut login = tokio::process::Command::new(shell);
        login.args(["-l", "-i", "-c", &format!("{program} --version")]);
        login
    };
    command
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output()).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().map(str::trim).rfind(|line| !line.is_empty()).map(str::to_owned)
}

/// Everything about this worker as it is now; `agents` from [`installed_agents`].
pub async fn probe(agents: &[InstalledAgent]) -> WorkerCaps {
    let can_capture = slopty_capture::can_capture();
    WorkerCaps {
        os: Os::MacOs,
        os_version: sysctl_string(c"kern.osproductversion").unwrap_or_default(),
        arch: std::env::consts::ARCH.to_owned(),
        cpus: std::thread::available_parallelism()
            .map_or(1, |n| u16::try_from(n.get()).unwrap_or(u16::MAX)),
        memory: sysctl_u64(c"hw.memsize").unwrap_or(0),
        // Every Apple-silicon Mac encodes both in hardware (VideoToolbox).
        encoders: vec![VideoCodec::Hevc, VideoCodec::H264],
        // Without Screen Recording the enumeration fails anyway, and asking could prompt.
        displays: if can_capture { displays().await } else { Vec::new() },
        agents: agents.to_vec(),
        can_capture,
        can_inject: slopty_input::can_post(),
        load: load(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

async fn displays() -> Vec<DisplayCap> {
    match crate::screen::shareable().await {
        Ok(content) => content
            .displays()
            .into_iter()
            .map(|d| DisplayCap { id: d.id, w: d.w, h: d.h, scale: d.scale, hz: d.hz })
            .collect(),
        Err(e) => {
            tracing::debug!(error = %e, "displays not listed");
            Vec::new()
        }
    }
}

/// Keep `caps` current until every receiver is gone: permissions and displays every 5 s, the
/// load every 30 s when it moved by more than 0.5.
pub async fn watch(caps: watch::Sender<WorkerCaps>, agents: Vec<InstalledAgent>) {
    let mut tick = tokio::time::interval(CHECK_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut load_at = tokio::time::Instant::now();
    loop {
        tick.tick().await;
        if caps.is_closed() {
            return;
        }
        let mut next = probe(&agents).await;
        let load_due = load_at.elapsed() >= LOAD_PERIOD;
        if load_due {
            load_at = tokio::time::Instant::now();
        }
        caps.send_if_modified(|current| {
            if !load_due || (next.load - current.load).abs() <= LOAD_STEP {
                next.load = current.load;
            }
            let changed = *current != next;
            if changed {
                *current = next;
            }
            changed
        });
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

    #[tokio::test]
    async fn the_probe_reads_this_mac() {
        let caps = probe(&[]).await;
        assert!(
            caps.os_version.split('.').next().is_some_and(|major| major.parse::<u32>().is_ok()),
            "{caps:?}"
        );
        assert!(caps.memory >= 1 << 30, "{caps:?}");
        assert!(caps.cpus >= 1);
        assert_eq!(caps.arch, "aarch64");
        assert!(caps.load >= 0.0);
        assert_eq!(caps.encoders, [VideoCodec::Hevc, VideoCodec::H264]);
    }

    #[test]
    fn an_unknown_sysctl_is_none() {
        assert_eq!(sysctl_string(c"slopty.no.such.name"), None);
        assert_eq!(sysctl_u64(c"slopty.no.such.name"), None);
    }
}
