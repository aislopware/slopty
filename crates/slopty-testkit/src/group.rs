//! A test's daemons in a process group of their own, ended whole before the test ends.
//!
//! A daemon a test kills and does not wait for can still be alive when the test returns, under
//! load for long enough that nextest finds the test's output pipes held open and reports a
//! leak. So can anything the daemon started, which a kill of the daemon alone never reaches.
//! Spawned as the leader of a group of its own (`process_group(0)`), the daemon and everything
//! it started are ended with one signal to the group ([`kill`]). The test then reaps its own
//! children and waits until no process of the group is left ([`gone`]).
//!
//! A test that never gets that far still ends its daemons: nextest ending a test past its
//! timeout kills the test's own process group, which a daemon leading a group of its own is
//! outside of, and the test's teardown never runs. So each group is also put in the care of
//! the test process's warden ([`ward`]): a `sh` in a group of its own, reading `+<group>` and
//! `-<group>` lines from a pipe only the test process holds. The pipe closes when the test
//! process ends, however it ends, and the warden then kills every group still listed.

use std::io::Write as _;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

/// The warden's script: it keeps the groups said begun and not yet ended, and kills them once
/// its standard input closes.
const WARDEN: &str = r#"groups=" "
while IFS= read -r line; do
  group=${line#?}
  case $group in ''|*[!0-9]*) continue ;; esac
  case $line in
    +*) groups="$groups$group " ;;
    -*) kept=" "; for g in $groups; do [ "$g" = "$group" ] || kept="$kept$g "; done; groups=$kept ;;
  esac
done
for g in $groups; do kill -9 "-$g" 2>/dev/null; done
"#;

/// The pipe to this test process's warden, started on the first [`ward`]; `None` when it could
/// not be started.
fn warden() -> Option<&'static std::process::ChildStdin> {
    /// The warden, which outlives this process by design, and the pipe to it.
    struct Warden {
        _child: std::process::Child,
        pipe: std::process::ChildStdin,
    }
    static WARDEN_PIPE: OnceLock<Option<Warden>> = OnceLock::new();
    WARDEN_PIPE
        .get_or_init(|| {
            use std::os::unix::process::CommandExt as _;
            let mut warden = std::process::Command::new("/bin/sh")
                .args(["-c", WARDEN])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .process_group(0)
                .spawn()
                .ok()?;
            let pipe = warden.stdin.take()?;
            Some(Warden { _child: warden, pipe })
        })
        .as_ref()
        .map(|warden| &warden.pipe)
}

/// Say `line` to the warden.
fn tell(line: &str) {
    if let Some(mut pipe) = warden() {
        let _told = pipe.write_all(line.as_bytes());
    }
}

/// Put group `group`, the pid of a child that leads it, in the warden's care: killed whole once
/// this test process ends, however it ends, unless [`end`] ended it first. Call it right after
/// the spawn.
pub fn ward(group: Option<u32>) {
    if let Some(group) = group {
        tell(&format!("+{group}\n"));
    }
}

/// Take group `group` out of the warden's care: it is gone, and its id may name another.
fn unward(group: u32) {
    tell(&format!("-{group}\n"));
}

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
/// to be gone ([`kill`], then [`wait`], killing again while any of it is left), then out of the
/// warden's care ([`ward`]).
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
    let mut all = true;
    for (child, group) in children.iter_mut().zip(groups) {
        let Some(group) = group else { continue };
        let ended = wait(group, || {
            let reaped = reaped(child);
            // A member forking as the kill came can leave a child the kill missed, still in the
            // group; it goes with the next.
            if !gone(group) {
                kill(group);
            }
            reaped
        });
        if ended {
            unward(group);
        }
        all &= ended;
    }
    all
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
    ///
    /// The kill comes only once `sh` says the `sleep`'s pid, after its fork returned: a group
    /// killed while a member is still forking can miss the child the fork is making, which
    /// then lives on in the group, orphaned.
    #[test]
    fn a_group_and_what_it_started_are_ended_together() {
        use std::io::BufRead as _;
        let mut leader = Command::new("/bin/sh")
            .args(["-c", "/bin/sleep 600 >/dev/null & echo $!; exec >&-; wait"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut said = String::new();
        let out = leader.stdout.take().unwrap();
        std::io::BufReader::new(out).read_line(&mut said).unwrap();
        assert!(said.trim().parse::<u32>().is_ok(), "the sleep's pid, not {said:?}");
        let group = leader.id();
        assert!(!gone(group));
        let killed = kill_process_group(pid(group).unwrap(), Signal::KILL);
        assert_eq!(killed, Ok(()), "the group is killed");
        assert!(wait(group, || leader.try_wait().is_ok_and(|s| s.is_some())), "all gone");
    }

    /// Set for the test process [`a_killed_test_s_warded_daemons_die_with_it`] starts.
    const CHILD: &str = "SLOPTY_TESTKIT_WARDED_CHILD";

    /// The test process [`a_killed_test_s_warded_daemons_die_with_it`] starts, when [`CHILD`]
    /// is set: a daemon leading a group of its own put in the warden's care, its group said,
    /// and then a wait to be killed.
    #[test]
    fn a_warded_child() {
        use std::io::BufRead as _;
        if std::env::var_os(CHILD).is_none() {
            return;
        }
        let mut daemon = Command::new("/bin/sleep")
            .arg("600")
            .stdout(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        ward(Some(daemon.id()));
        println!("group {}", daemon.id());
        let mut line = String::new();
        let _closed = std::io::stdin().lock().read_line(&mut line);
        let _killed = daemon.kill();
        let _reaped = daemon.wait();
    }

    /// A test process killed outright, as nextest ends one past its timeout, takes the daemons
    /// it put in the warden's care with it, though each leads a group of its own.
    #[test]
    fn a_killed_test_s_warded_daemons_die_with_it() {
        use std::io::BufRead as _;
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "group::tests::a_warded_child", "--nocapture"])
            .env(CHILD, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let said = std::io::BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok)
            .find_map(|line| {
                line.strip_prefix("group ").and_then(|g| g.trim().parse::<u32>().ok())
            });
        let group = said.expect("the child said its daemon's group");
        assert!(!gone(group), "the daemon runs");
        child.kill().unwrap();
        child.wait().unwrap();
        let died = wait(group, || true);
        if !died {
            kill(group);
        }
        assert!(died, "the warded daemon outlived its test");
    }
}
