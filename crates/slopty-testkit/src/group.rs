//! A test's daemons in a process group of their own, ended whole before the test ends.
//!
//! A daemon a test kills and does not wait for can still be alive when the test returns, under
//! load for long enough that nextest finds the test's output pipes held open and reports a
//! leak. So can anything the daemon started, which a kill of the daemon alone never reaches.
//! Spawned as the leader of a group of its own (`process_group(0)`), the daemon and everything
//! it started are ended with one signal to the group ([`kill`]). The test then reaps its own
//! children and waits until no process of the group is left ([`gone`]).

use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

/// How long [`wait`] waits for a group to be gone before it gives up.
pub const GONE_WITHIN: Duration = Duration::from_secs(10);

/// Send `SIGKILL` to every process in group `group`, the pid of the child that leads it.
pub fn kill(group: u32) {
    if let Some(pid) = pid(group) {
        let _gone = kill_process_group(pid, Signal::KILL);
    }
}

/// Whether no process of group `group` is left. A child the test has not reaped yet still
/// counts: reap it first.
#[must_use]
pub fn gone(group: u32) -> bool {
    pid(group).is_none_or(|pid| test_kill_process_group(pid).is_err())
}

/// Wait until `reaped` says the test's own child is reaped and no process of group `group` is
/// left, for at most [`GONE_WITHIN`]; whether it got there.
///
/// It looks again every [`LOOK_EVERY`]. A process reparented to `launchd` sends nothing this
/// process can wait on, and a test's teardown has no runtime to wait with a timer.
#[expect(
    clippy::disallowed_methods,
    reason = "a test's teardown waiting out killed processes, with nothing to be woken by"
)]
pub fn wait(group: u32, mut reaped: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if reaped() && gone(group) {
            return true;
        }
        if start.elapsed() > GONE_WITHIN {
            return false;
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

/// How often [`wait`] looks for the group to be gone.
pub const LOOK_EVERY: Duration = Duration::from_millis(2);

/// End each of `children`, every one the leader of a group of its own, and wait for each group
/// to be gone ([`kill`], then [`wait`]).
///
/// What a test's daemons hold of its output is closed once this returns. `id` says a child's
/// pid while it is unreaped, and `reaped` reaps it if it has exited. It says whether every
/// group was gone in time.
pub fn end<T>(
    children: &mut [T],
    id: impl Fn(&T) -> Option<u32>,
    mut reaped: impl FnMut(&mut T) -> bool,
) -> bool {
    let groups: Vec<Option<u32>> = children.iter().map(&id).collect();
    for group in groups.iter().flatten() {
        kill(*group);
    }
    children
        .iter_mut()
        .zip(groups)
        .all(|(child, group)| group.is_none_or(|group| wait(group, || reaped(child))))
}

fn pid(group: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(group).ok()?)
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    use super::*;

    /// A group leader and the child it started are both ended by one kill, and the wait sees
    /// them gone once the leader is reaped.
    #[test]
    fn a_group_and_what_it_started_are_ended_together() {
        let mut leader = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 600 & wait"])
            .stdout(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = leader.id();
        assert!(!gone(group));
        kill(group);
        assert!(wait(group, || leader.try_wait().is_ok_and(|s| s.is_some())), "all gone");
    }
}
