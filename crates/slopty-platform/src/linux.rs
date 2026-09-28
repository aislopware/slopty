//! The Linux half of the crate's process-level calls: what a worker or a server asks of the
//! system, answered for Linux or refused aloud.

/// What a process holds while its work is latency-critical or a person is attached.
///
/// Linux has no App Nap: nothing coalesces a background daemon's timers, so
/// [`Activity::latency_critical`] has nothing to ask for and holds nothing. Keeping the machine
/// or the display awake is a logind inhibitor lock, taken by `systemd-inhibit` (logind's own
/// D-Bus client) running `cat` on a pipe this process holds the write end of. Dropping the hold
/// closes the pipe; so does this process dying, so no lock outlives its holder.
#[derive(Debug)]
pub struct Activity {
    _held: Option<Inhibitor>,
}

impl Activity {
    /// Nothing to hold: Linux does not throttle a background process's timers.
    #[must_use]
    pub const fn latency_critical(_reason: &str) -> Self {
        Self { _held: None }
    }

    /// Keep the machine out of sleep, idle or asked for, with a blocking `sleep:idle` lock.
    #[must_use]
    pub fn system_awake(reason: &str) -> Self {
        Self { _held: Inhibitor::take("sleep:idle", reason) }
    }

    /// Keep the session from going idle, with a blocking `idle` lock.
    #[must_use]
    pub fn display_awake(reason: &str) -> Self {
        Self { _held: Inhibitor::take("idle", reason) }
    }
}

/// A `systemd-inhibit … cat` whose standard input is ours: the lock lasts until `cat` reads the
/// end of it.
#[derive(Debug)]
struct Inhibitor {
    _input: std::process::ChildStdin,
}

impl Inhibitor {
    /// The command that holds a blocking logind lock on `what` for as long as its input is open.
    fn command(what: &str, reason: &str) -> std::process::Command {
        let mut command = std::process::Command::new("systemd-inhibit");
        command
            .arg(format!("--what={what}"))
            .args(["--who=Slopty", "--mode=block"])
            .arg(format!("--why={reason}"))
            .arg("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command
    }

    /// The lock, or `None` and a warning that nothing is held. logind's refusal comes after the
    /// spawn, as a failed exit the reaper reports.
    fn take(what: &'static str, reason: &str) -> Option<Self> {
        let mut child = match Self::command(what, reason).spawn() {
            Ok(child) => child,
            Err(e) => {
                tracing::warn!(what, reason, error = %e, "no systemd-inhibit; sleep not held");
                return None;
            }
        };
        let input = child.stdin.take();
        reap("systemd-inhibit", child, move |status| {
            tracing::warn!(what, %status, "logind refused the inhibitor; sleep not held");
        });
        tracing::info!(what, reason, "logind inhibitor taken");
        Some(Self { _input: input? })
    }
}

/// Wait for `child` on a thread of its own, so it does not linger as a zombie, and hand a
/// failed exit to `failed`.
fn reap(
    name: &'static str,
    mut child: std::process::Child,
    failed: impl FnOnce(std::process::ExitStatus) + Send + 'static,
) {
    let reaper =
        std::thread::Builder::new().name(name.to_owned()).spawn(move || match child.wait() {
            Ok(status) if !status.success() => failed(status),
            Ok(_) => {}
            Err(e) => tracing::warn!(name, error = %e, "waiting for a child"),
        });
    if let Err(e) = reaper {
        tracing::warn!(name, error = %e, "reaper thread");
    }
}

/// Leave the calling thread's scheduling as it is.
///
/// Linux has no `QoS` class to ask for. Its nearest equivalents (a lower nice value, a realtime
/// policy, a raised `uclamp` minimum) need `CAP_SYS_NICE`, which a user's daemon does not
/// have, so the keystroke path runs at the default class.
pub const fn user_interactive_thread() {}

/// Open `url` with the desktop's handler (`xdg-open`), off the calling thread.
///
/// A URL that begins with `-` is refused: `xdg-open` would read it as an option.
pub fn open_url(url: &str) {
    if url.starts_with('-') {
        tracing::warn!(url, "not a URL");
        return;
    }
    let spawned = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => reap("xdg-open", child, |status| tracing::warn!(%status, "xdg-open")),
        Err(e) => tracing::warn!(url, error = %e, "xdg-open"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blocking lock named for Slopty, held by a `cat` on the pipe the hold owns.
    #[test]
    fn an_inhibitor_blocks_for_as_long_as_its_pipe_is_open() {
        let command = Inhibitor::command("sleep:idle", "Slopty client attached");
        assert_eq!(command.get_program(), "systemd-inhibit");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "--what=sleep:idle",
                "--who=Slopty",
                "--mode=block",
                "--why=Slopty client attached",
                "cat"
            ]
        );
    }
}
