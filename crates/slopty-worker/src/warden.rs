//! The process groups the worker runs commands in, ended with the worker however it ends.
//!
//! A command the worker runs for itself (a fact's probe, a worktree's setup) runs in a process
//! group of its own, so that a timeout kills whatever it started as well (`Group`). Such a
//! group is not the worker's, so a worker killed outright (a crash, `SIGKILL`, a test's
//! teardown) left its groups running, adopted by launchd. On a CI Mac the `rustup` and
//! `xcodebuild` probes of killed workers held gigabytes for minutes.
//!
//! So a warden outlives the worker: a process of the worker's own binary ([`serve`]), in a group
//! of its own, told of every group as it begins and ends over a pipe ([`ward`]). The pipe's
//! write end is the worker's alone, so it closes exactly when the worker is gone, however it
//! went. The warden then kills every group still listed and exits.

use std::collections::BTreeSet;
use std::io::Write as _;

use parking_lot::Mutex;
use rustix::process::{Pid, Signal, kill_process_group};

/// What tells the warden, once the worker has one.
static WARDEN: Mutex<Option<std::process::ChildStdin>> = Mutex::new(None);

/// Tell the warden on `pipe` (its standard input) of every group from now on.
pub fn ward(pipe: std::process::ChildStdin) {
    *WARDEN.lock() = Some(pipe);
}

/// Say `line` to the warden; a warden gone is told nothing more.
fn tell(line: &str) {
    let mut warden = WARDEN.lock();
    if let Some(pipe) = warden.as_mut()
        && pipe.write_all(line.as_bytes()).is_err()
    {
        tracing::warn!("the warden is gone: what the worker starts may outlive it");
        *warden = None;
    }
}

/// The warden's whole life: it reads `+<group>` for a group begun and `-<group>` for one ended
/// from `told` until the worker closes it, then kills every group still listed.
pub fn serve(told: impl std::io::BufRead) {
    let mut listed = BTreeSet::new();
    for line in told.lines() {
        let Ok(line) = line else { break };
        let (sign, group) = line.split_at_checked(1).unwrap_or_default();
        let Ok(group) = group.parse::<i32>() else { continue };
        if sign == "+" {
            listed.insert(group);
        } else {
            listed.remove(&group);
        }
    }
    for group in listed.into_iter().filter_map(Pid::from_raw) {
        let _gone = kill_process_group(group, Signal::KILL);
    }
}

/// A child's process group, killed whole when dropped unless the child was reaped, and on the
/// warden's list while it may run.
pub(crate) struct Group(Option<Pid>);

impl Group {
    /// The group `child` leads, which it was started to (`process_group(0)`).
    pub(crate) fn of(child: &tokio::process::Child) -> Self {
        let group = child.id().and_then(|id| i32::try_from(id).ok()).and_then(Pid::from_raw);
        if let Some(group) = group {
            tell(&format!("+{}\n", group.as_raw_nonzero()));
        }
        Self(group)
    }

    /// Its leader was reaped: its id may name another group from now on, so it is not killed.
    pub(crate) fn reaped(&mut self) {
        if let Some(group) = self.0.take() {
            tell(&format!("-{}\n", group.as_raw_nonzero()));
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(group) = self.0.take() {
            let _gone = kill_process_group(group, Signal::KILL);
            tell(&format!("-{}\n", group.as_raw_nonzero()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// `sh` waiting on a long `sleep`, leading a group of its own.
    fn sleeping() -> std::process::Child {
        use std::os::unix::process::CommandExt as _;
        std::process::Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 600 & wait"])
            .stdout(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap()
    }

    fn group_of(child: &std::process::Child) -> Pid {
        Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap()
    }

    /// Whether `child`'s group is gone within a few seconds, `child` reaped on the way.
    async fn gone(child: &mut std::process::Child) -> bool {
        for _ in 0..500 {
            let _reaped = child.try_wait();
            if rustix::process::test_kill_process_group(group_of(child)).is_err() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Once the worker's end of the pipe closes, the groups still listed are killed whole and
    /// one said ended is left alone; a line it cannot read changes nothing.
    #[tokio::test]
    async fn the_groups_still_listed_die_when_the_worker_goes() {
        let (mut listed, mut ended) = (sleeping(), sleeping());
        let said = format!("+{}\nnonsense\n+{}\n-{}\n", listed.id(), ended.id(), ended.id());
        serve(said.as_bytes());
        assert!(gone(&mut listed).await, "the listed group was killed with what it started");
        let left = rustix::process::test_kill_process_group(group_of(&ended)).is_ok();
        let _killed = kill_process_group(group_of(&ended), Signal::KILL);
        assert!(gone(&mut ended).await);
        assert!(left, "the group said ended was left alone");
    }
}
