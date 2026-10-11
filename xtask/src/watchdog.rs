//! What a run that outlives its bound is doing, said before the runner cancels it.
//!
//! CI ends a job at its `timeout-minutes` with nothing but "The operation was canceled": the
//! hung test, and why it hangs, go with it. A nextest run whose test is stuck where a signal
//! can't reach it (in the kernel, `U` in `ps`) outlives nextest's own `terminate-after`, so even
//! its SLOW lines stop. So every [`POLL`] it looks under the run: a test process that has run
//! [`EARLY`] is sampled once while it still lives (nextest kills at 180 s, and a process killed
//! in the kernel can no longer be sampled), and past [`Watchdog::start`]'s bound it prints every
//! process under the run with its state and age, sampling each leaf, then again every [`AGAIN`]
//! until the run ends.
//!
//! A run handed a process group to end ([`Watchdog::start`]'s `end`) is ended when every test
//! under it has been stuck exiting ([`STUCK_POLLS`] looks in a row): a process that opened
//! VideoToolbox on a virtual Mac whose codec wedged cannot finish its exit, no signal ends it,
//! and once such processes hold every test slot nextest waits for the job's cancel
//! (`.research/ci-vt-hang-2026-10-11.md`). The run's tests are named and their kernel stacks
//! printed first, and the run fails in minutes instead.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// How long after a first report the next one comes, while the run still holds.
const AGAIN: Duration = Duration::from_mins(5);

/// How often the watchdog looks under the run.
const POLL: Duration = Duration::from_secs(30);

/// How long a test process runs before it is sampled: past the profile's SLOW lines, before its
/// `terminate-after` (3 × 60 s in CI).
const EARLY: Duration = Duration::from_secs(150);

/// How many leaves are sampled per report: the stuck ones are among a few at most.
const SAMPLED: usize = 6;

/// How many looks in a row find every test stuck exiting before the run is ended: two minutes
/// at [`POLL`], past any exit that only takes long. Nextest's own kill (180 s on CI) has then
/// already failed to end them, or none of them is a test it still times.
const STUCK_POLLS: u32 = 4;

/// How long a kernel stack may take to read before the watchdog goes on without it, so the
/// watchdog never becomes the hang itself.
#[cfg(target_os = "macos")]
const STACK_CAP: Duration = Duration::from_secs(20);

/// How many lines of each sample's call graph are kept.
#[cfg(target_os = "macos")]
const SAMPLE_LINES: usize = 60;

/// Reports on its own thread until [`Watchdog::finish`].
#[derive(Debug)]
pub struct Watchdog {
    stop: mpsc::Sender<()>,
    /// The tests the run was ended for, when it was.
    thread: std::thread::JoinHandle<Option<Vec<String>>>,
}

impl Watchdog {
    /// Watch the processes under `root` (a pid), reporting on them once the run named `title`
    /// has gone on for `after`. With `end`, the run's process group, the run is killed once
    /// every test under it is stuck exiting ([`STUCK_POLLS`]).
    pub fn start(title: &str, root: u32, after: Duration, end: Option<u32>) -> Self {
        let (stop, stopped) = mpsc::channel();
        let title = title.to_owned();
        let thread = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let mut next = after;
            let mut sampled = HashSet::new();
            let mut stuck = Stuck::default();
            while stopped.recv_timeout(POLL) == Err(mpsc::RecvTimeoutError::Timeout) {
                let Some(procs) = listed() else { continue };
                let under = descendants(&procs, root);
                print!("{}", early(&under, &mut sampled));
                if let Some(group) = end {
                    let exiting = stuck_exiting(&under);
                    if stuck.look(exiting.is_some())
                        && let Some(tests) = exiting
                    {
                        let names: Vec<String> = tests.iter().map(|t| named(&t.command)).collect();
                        print!("{}", ending(&title, &tests, &names));
                        let _killed = Command::new("/bin/kill")
                            .args(["-KILL", "--", &format!("-{group}")])
                            .status();
                        return Some(names);
                    }
                }
                if started.elapsed() >= next {
                    print!("{}", report(&title, &under, next));
                    next = next.saturating_add(AGAIN);
                }
            }
            None
        });
        Self { stop, thread }
    }

    /// Stop watching: the tests the run was ended for, when it was.
    #[must_use]
    pub fn finish(self) -> Option<Vec<String>> {
        let _sent = self.stop.send(());
        self.thread.join().ok().flatten()
    }
}

/// How many looks in a row found every test stuck exiting.
#[derive(Debug, Default)]
struct Stuck {
    looks: u32,
}

impl Stuck {
    /// Count a look that found every test stuck exiting (`all`), or none that did: whether the
    /// run has now been stuck for [`STUCK_POLLS`] looks in a row.
    const fn look(&mut self, all: bool) -> bool {
        self.looks = if all { self.looks.saturating_add(1) } else { 0 };
        self.looks >= STUCK_POLLS
    }
}

/// The run's tests, when there is one and every one is stuck exiting (`E` in `ps`): the
/// children of nextest, each a test binary (the runner execs it). What a test started is left
/// out: a daemon can live on while the test that started it is stuck.
fn stuck_exiting(under: &[Proc]) -> Option<Vec<&Proc>> {
    let nextest: HashSet<u32> = under
        .iter()
        .filter(|proc| proc.command.contains("cargo-nextest"))
        .map(|proc| proc.pid)
        .collect();
    let tests: Vec<&Proc> = under
        .iter()
        .filter(|proc| nextest.contains(&proc.ppid) && !nextest.contains(&proc.pid))
        .collect();
    let all = !tests.is_empty() && tests.iter().all(|test| test.stat.contains('E'));
    all.then_some(tests)
}

/// A test process's name, from its command line: the test binary's file name and the test
/// nextest asked it to run.
fn named(command: &str) -> String {
    let mut words = command.split_whitespace();
    let binary = words.next().unwrap_or_default();
    let binary = binary.rsplit('/').next().unwrap_or(binary);
    match words.find(|word| !word.starts_with('-')) {
        Some(test) => format!("{binary} {test}"),
        None => binary.to_owned(),
    }
}

/// What is said as the run is ended: the error line first, so CI keeps it whatever the
/// teardown does, then each test's kernel stack.
fn ending(title: &str, tests: &[&Proc], names: &[String]) -> String {
    let mut text = format!(
        "\n::error title={title}::tests stuck exiting in the kernel: {}\n⏱ {title}: every test \
         has been stuck exiting for {} s, so the run is ended\n",
        names.join(", "),
        POLL.as_secs().saturating_mul(u64::from(STUCK_POLLS)),
    );
    for (test, name) in tests.iter().zip(names) {
        let _written = writeln!(text, "  {} {} {} {name}", test.pid, test.stat, test.etime);
        text.push_str(&sample(test));
    }
    text.push_str(&host());
    text
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

/// Every process on the machine, or `None` when `ps` can't be run.
fn listed() -> Option<Vec<Proc>> {
    let listed = Command::new("ps").args(["-axo", "pid=,ppid=,stat=,etime=,command="]).output();
    listed.ok().map(|listed| parse(&String::from_utf8_lossy(&listed.stdout)))
}

/// A sample of each test process under the run that has gone on for [`EARLY`] and wasn't
/// sampled yet, and the host's state after them.
fn early(under: &[Proc], sampled: &mut HashSet<u32>) -> String {
    let mut text = String::new();
    for proc in due(under, sampled) {
        let command: String = proc.command.chars().take(200).collect();
        let _written = writeln!(text, "\n⏱ {} has run {}: {command}", proc.pid, proc.etime);
        text.push_str(&sample(proc));
    }
    if !text.is_empty() {
        text.push_str(&host());
    }
    text
}

/// The test processes under the run that have gone on for [`EARLY`] and weren't sampled yet, at
/// most [`SAMPLED`], marked sampled. Cargo and nextest themselves are left out: they run as long
/// as the run.
fn due<'a>(under: &'a [Proc], sampled: &mut HashSet<u32>) -> Vec<&'a Proc> {
    under
        .iter()
        .filter(|proc| {
            let ours = proc.command.contains("nextest")
                || proc.command.starts_with("cargo")
                || proc.command.starts_with("ps ");
            !ours && seconds(&proc.etime).is_some_and(|age| age >= EARLY.as_secs())
        })
        .take(SAMPLED)
        .filter(|proc| sampled.insert(proc.pid))
        .collect()
}

fn report(title: &str, under: &[Proc], held: Duration) -> String {
    let mut text = format!(
        "\n⏱ {title} has run {} min: the processes under it (pid ppid state age command)\n",
        held.as_secs() / 60
    );
    for proc in under {
        let command: String = proc.command.chars().take(200).collect();
        let _written =
            writeln!(text, "  {} {} {} {} {command}", proc.pid, proc.ppid, proc.stat, proc.etime);
    }
    for leaf in leaves(under).into_iter().take(SAMPLED) {
        text.push_str(&sample(leaf));
    }
    text.push_str(&host());
    text
}

/// `ps`'s elapsed time, `[[dd-]hh:]mm:ss`, in seconds.
fn seconds(etime: &str) -> Option<u64> {
    let (days, clock) =
        etime.split_once('-').map_or((Some(0_u64), etime), |(d, c)| (d.parse().ok(), c));
    let mut total: u64 = 0;
    for part in clock.split(':') {
        total = total.checked_mul(60)?.checked_add(part.parse().ok()?)?;
    }
    days?.checked_mul(86_400)?.checked_add(total)
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

/// What `leaf` is doing: on macOS, the head of a one-second `sample` call graph; for one stuck
/// exiting, which `sample` cannot read, its kernel stack where it can be read.
#[cfg(target_os = "macos")]
fn sample(leaf: &Proc) -> String {
    if leaf.stat.contains('E')
        && let Some(stack) = kernel_stack(leaf.pid)
    {
        return stack;
    }
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

/// The head of `pid`'s stacks, kernel frames included, as `spindump` reads them: it needs root,
/// which a hosted runner grants without a password (`sudo -n`), so here it is `None` and the
/// caller samples as before. A read past [`STACK_CAP`] is gone on from, left to finish alone.
#[cfg(target_os = "macos")]
fn kernel_stack(pid: u32) -> Option<String> {
    let mut command = Command::new("sudo");
    command
        .args(["-n", "spindump", &pid.to_string(), "1", "-onlyTarget", "-stdout"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let child = command.spawn().ok()?;
    let (done, read) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done.send(child.wait_with_output());
    });
    let output = match read.recv_timeout(STACK_CAP) {
        Ok(Ok(output)) if output.status.success() => output,
        Ok(_) => return None,
        Err(_) => {
            return Some(format!(
                "  ── spindump of {pid} took over {} s; gone on without it\n",
                STACK_CAP.as_secs()
            ));
        }
    };
    let report = String::from_utf8_lossy(&output.stdout);
    let mut text = format!("  ── kernel stack of {pid} (spindump)\n");
    // From the first thread's stack, or the whole report should its layout ever change.
    let threads = report.lines().position(|line| line.trim_start().starts_with("Thread "));
    for line in report.lines().skip(threads.unwrap_or(0)).take(SAMPLE_LINES) {
        let _written = writeln!(text, "    {line}");
    }
    Some(text)
}

/// What the machine says of its video encoder. On a virtual Mac, each process that opened
/// VideoToolbox leaves two clients of the paravirtual driver until the guest reboots, and the
/// encoder stops for good at 1020 of them (docs/decisions/video.md), hanging every VideoToolbox
/// test: the count, and the driver's own words in the log, say whether that is what a hung run
/// hit. A Mac on its own hardware has no such driver and says nothing.
#[cfg(target_os = "macos")]
fn host() -> String {
    let listed = Command::new("ioreg")
        .args(["-l", "-w0", "-r", "-c", "AppleVideoToolboxParavirtualizationDriver"])
        .output();
    let clients = listed.map_or(0, |listed| {
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .filter(|line| line.contains("o AppleVideoToolboxParavirtualizationUserClient"))
            .count()
    });
    if clients == 0 {
        return String::new();
    }
    let mut text = format!("  ── the paravirtual video driver holds {clients} clients\n");
    let said = Command::new("log")
        .args([
            "show",
            "--last",
            "15m",
            "--style",
            "compact",
            "--predicate",
            "process != \"log\" AND (eventMessage CONTAINS \"No real codec\" OR eventMessage \
             CONTAINS \"stalling for detach\" OR eventMessage CONTAINS \"err=-12908\")",
        ])
        .output();
    if let Ok(said) = said {
        let said = String::from_utf8_lossy(&said.stdout);
        let lines: Vec<&str> = said.lines().filter(|line| !line.is_empty()).collect();
        for line in lines.iter().skip(lines.len().saturating_sub(SAMPLE_LINES / 6)) {
            let _written = writeln!(text, "    {line}");
        }
    }
    text
}

/// Whether this Mac's video encoder said in the last half hour that it has stopped: a virtual
/// Mac's paravirtual encoder service logs "No real codec" once it can no longer reach its host's
/// (`docs/decisions/video.md`). A Mac on its own hardware never does.
#[cfg(target_os = "macos")]
pub fn encoder_stopped() -> bool {
    Command::new("log")
        .args([
            "show",
            "--last",
            "30m",
            "--style",
            "compact",
            "--predicate",
            "process == \"VTEncoderXPCService\" AND eventMessage CONTAINS \"No real codec\"",
        ])
        .output()
        .is_ok_and(|said| String::from_utf8_lossy(&said.stdout).contains("No real codec"))
}

/// Whether this machine's video encoder stopped: off macOS there is none to ask.
#[cfg(not(target_os = "macos"))]
pub const fn encoder_stopped() -> bool {
    false
}

/// What the machine says of its video encoder: nothing off macOS.
#[cfg(not(target_os = "macos"))]
const fn host() -> String {
    String::new()
}

/// What `leaf` is doing: on Linux, the kernel function it sleeps in.
#[cfg(not(target_os = "macos"))]
fn sample(leaf: &Proc) -> String {
    let wchan = std::fs::read_to_string(format!("/proc/{}/wchan", leaf.pid)).unwrap_or_default();
    format!("  ── {} ({}) waits in {}\n", leaf.pid, leaf.stat, wchan.trim())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        Proc, STUCK_POLLS, Stuck, descendants, due, leaves, named, parse, seconds, stuck_exiting,
    };

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

    /// `ps`'s ages read as seconds, with and without hours and days.
    #[test]
    fn an_age_reads_in_seconds() {
        assert_eq!(seconds("02:30"), Some(150));
        assert_eq!(seconds("01:02:03"), Some(3_723));
        assert_eq!(seconds("2-00:00:01"), Some(172_801));
        assert_eq!(seconds("bad"), None);
    }

    /// A test process past [`EARLY`] is sampled once; cargo and nextest never are.
    #[test]
    fn a_long_test_is_sampled_once() {
        let mut long = proc(12, 11, "worker-tests screen::synthetic");
        long.etime = "02:40".to_owned();
        let mut runner = proc(11, 10, "cargo-nextest nextest run");
        runner.etime = "20:00".to_owned();
        let young = proc(13, 11, "worker-tests fsevents");
        let under = vec![runner, long, young];
        let mut sampled = HashSet::new();
        // Which are due, not their samples: `sample` and the host's log read the real machine,
        // which took 3–43 s beside the other tests.
        let pids = |due: Vec<&Proc>| due.iter().map(|p| p.pid).collect::<Vec<_>>();
        assert_eq!(pids(due(&under, &mut sampled)), [12]);
        assert_eq!(pids(due(&under, &mut sampled)), [0_u32; 0], "sampled once");
    }

    /// The run's tests are found stuck only when there is one and every one is exiting; what a
    /// test started does not count, and each is named by its binary and its test.
    #[test]
    fn a_run_whose_every_test_is_stuck_exiting_is_found() {
        let exiting =
            |pid, ppid, command: &str| Proc { stat: "UE".to_owned(), ..proc(pid, ppid, command) };
        let nextest =
            proc(11, 10, "/Users/runner/.cargo/bin/cargo-nextest nextest run --profile ci");
        let a = exiting(12, 11, "/t/ci/run/domain-1f tests::a_drop_lands --exact --nocapture");
        let b = exiting(13, 11, "/t/ci/run/threads-2e tests::an_ask_waits --exact --nocapture");
        let daemon = proc(14, 12, "slopty-workerd --data-dir /tmp/x");
        let under = vec![proc(10, 1, "xtask gate"), nextest.clone(), a.clone(), b, daemon];
        let stuck = stuck_exiting(&under).expect("every test exiting");
        let names: Vec<String> = stuck.iter().map(|t| named(&t.command)).collect();
        assert_eq!(names, ["domain-1f tests::a_drop_lands", "threads-2e tests::an_ask_waits"]);

        let running = proc(15, 11, "/t/ci/run/media-3d tests::paced --exact");
        assert!(stuck_exiting(&[nextest.clone(), a, running]).is_none(), "one still runs");
        assert!(stuck_exiting(&[nextest]).is_none(), "no test at all");
    }

    /// The run is ended only once every test has been stuck exiting for [`STUCK_POLLS`] looks in
    /// a row: a look that finds one moving starts the count again.
    #[test]
    fn the_run_is_ended_only_after_its_looks_stuck_in_a_row() {
        let mut stuck = Stuck::default();
        for _ in 1..STUCK_POLLS {
            assert!(!stuck.look(true));
        }
        assert!(!stuck.look(false), "one moved");
        for _ in 1..STUCK_POLLS {
            assert!(!stuck.look(true));
        }
        assert!(stuck.look(true), "stuck for every look in a row");
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
