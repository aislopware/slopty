//! What a run that outlives its bound is doing, said before the runner cancels it.
//!
//! CI ends a job at its `timeout-minutes` with nothing but "The operation was canceled": the
//! hung test, and why it hangs, go with it. A nextest run whose test is stuck where a signal
//! can't reach it (in the kernel, `U` in `ps`) outlives nextest's own `terminate-after`, so even
//! its SLOW lines stop. Past [`Watchdog::start`]'s bound this prints every process under the
//! run, with its state and age, and on macOS a short `sample` of each leaf, then again every
//! [`AGAIN`] until the run ends.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// How long after a first report the next one comes, while the run still holds.
const AGAIN: Duration = Duration::from_mins(5);

/// How many leaves are sampled per report: the stuck ones are among a few at most.
const SAMPLED: usize = 6;

/// How many lines of each sample's call graph are kept.
const SAMPLE_LINES: usize = 60;

/// Reports on its own thread until [`Watchdog::finish`].
#[derive(Debug)]
pub struct Watchdog {
    stop: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Watchdog {
    /// Watch the processes under `root` (a pid), reporting on them once the run named `title`
    /// has gone on for `after`.
    pub fn start(title: &str, root: u32, after: Duration) -> Self {
        let (stop, stopped) = mpsc::channel();
        let title = title.to_owned();
        let thread = std::thread::spawn(move || {
            let mut wait = after;
            let mut held = after;
            while stopped.recv_timeout(wait) == Err(mpsc::RecvTimeoutError::Timeout) {
                print!("{}", report(&title, root, held));
                wait = AGAIN;
                held = held.saturating_add(AGAIN);
            }
        });
        Self { stop, thread }
    }

    /// Stop watching.
    pub fn finish(self) {
        let _sent = self.stop.send(());
        let _joined = self.thread.join();
    }
}

/// A process as `ps` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Proc {
    pid: u32,
    ppid: u32,
    stat: String,
    etime: String,
    command: String,
}

fn report(title: &str, root: u32, held: Duration) -> String {
    let mut text = format!(
        "\n⏱ {title} has run {} min: the processes under it (pid ppid state age command)\n",
        held.as_secs() / 60
    );
    let listed = Command::new("ps").args(["-axo", "pid=,ppid=,stat=,etime=,command="]).output();
    let Ok(listed) = listed else {
        text.push_str("  ps could not be run\n");
        return text;
    };
    let procs = parse(&String::from_utf8_lossy(&listed.stdout));
    let under = descendants(&procs, root);
    for proc in &under {
        let command: String = proc.command.chars().take(200).collect();
        let _written =
            writeln!(text, "  {} {} {} {} {command}", proc.pid, proc.ppid, proc.stat, proc.etime);
    }
    for leaf in leaves(&under).into_iter().take(SAMPLED) {
        text.push_str(&sample(leaf));
    }
    text
}

/// Every process `ps` printed, in its order.
fn parse(listing: &str) -> Vec<Proc> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let stat = fields.next()?.to_owned();
            let etime = fields.next()?.to_owned();
            let command = fields.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, ppid, stat, etime, command })
        })
        .collect()
}

/// The processes whose parent chain reaches `root`, in `ps`'s order.
fn descendants(procs: &[Proc], root: u32) -> Vec<Proc> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for proc in procs {
        children.entry(proc.ppid).or_default().push(proc.pid);
    }
    let mut under = HashSet::new();
    let mut todo = vec![root];
    while let Some(pid) = todo.pop() {
        for &child in children.get(&pid).into_iter().flatten() {
            if under.insert(child) {
                todo.push(child);
            }
        }
    }
    procs.iter().filter(|proc| under.contains(&proc.pid)).cloned().collect()
}

/// Those of `under` that are nobody's parent, but `ps` itself: where a stuck run waits.
fn leaves(under: &[Proc]) -> Vec<&Proc> {
    let parents: HashSet<u32> = under.iter().map(|proc| proc.ppid).collect();
    under
        .iter()
        .filter(|proc| !parents.contains(&proc.pid) && !proc.command.starts_with("ps "))
        .collect()
}

/// What `leaf` is doing: on macOS, the head of a one-second `sample` call graph.
#[cfg(target_os = "macos")]
fn sample(leaf: &Proc) -> String {
    let file = std::env::temp_dir().join(format!("xtask-watchdog-{}.txt", leaf.pid));
    let ran = Command::new("sample")
        .args([&leaf.pid.to_string(), "1", "-mayDie", "-file"])
        .arg(&file)
        .output();
    let mut text = format!("  ── sample of {} ({})\n", leaf.pid, leaf.stat);
    match (ran, std::fs::read_to_string(&file)) {
        (Ok(_), Ok(report)) => {
            let graph = report.lines().skip_while(|line| !line.starts_with("Call graph:"));
            for line in graph.take(SAMPLE_LINES) {
                let _written = writeln!(text, "    {line}");
            }
        }
        _ => text.push_str("    sample could not read it\n"),
    }
    let _removed = std::fs::remove_file(&file);
    text
}

/// What `leaf` is doing: on Linux, the kernel function it sleeps in.
#[cfg(not(target_os = "macos"))]
fn sample(leaf: &Proc) -> String {
    let wchan = std::fs::read_to_string(format!("/proc/{}/wchan", leaf.pid)).unwrap_or_default();
    format!("  ── {} ({}) waits in {}\n", leaf.pid, leaf.stat, wchan.trim())
}

#[cfg(test)]
mod tests {
    use super::{Proc, descendants, leaves, parse};

    fn proc(pid: u32, ppid: u32, command: &str) -> Proc {
        Proc {
            pid,
            ppid,
            stat: "S".to_owned(),
            etime: "01:00".to_owned(),
            command: command.to_owned(),
        }
    }

    /// `ps`'s columns read back, the command with its spaces.
    #[test]
    fn a_listing_reads_back_with_its_command_whole() {
        let procs = parse("  10     1 Ss    05:00 cargo nextest run --profile ci\n   bad line\n");
        assert_eq!(
            procs,
            vec![Proc {
                pid: 10,
                ppid: 1,
                stat: "Ss".to_owned(),
                etime: "05:00".to_owned(),
                command: "cargo nextest run --profile ci".to_owned(),
            }]
        );
    }

    /// Only the run's own tree is reported, and its leaves are where it waits.
    #[test]
    fn the_tree_under_the_run_and_its_leaves_are_found() {
        let procs = vec![
            proc(1, 0, "launchd"),
            proc(10, 1, "xtask"),
            proc(11, 10, "cargo-nextest"),
            proc(12, 11, "worker-tests screen::synthetic"),
            proc(13, 11, "worker-tests fsevents"),
            proc(14, 13, "sh -c sleep"),
            proc(20, 1, "unrelated"),
            proc(15, 10, "ps -axo pid="),
        ];
        let under = descendants(&procs, 10);
        let pids: Vec<u32> = under.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![11, 12, 13, 14, 15]);
        let leaves: Vec<u32> = leaves(&under).iter().map(|p| p.pid).collect();
        assert_eq!(leaves, vec![12, 14]);
    }
}
