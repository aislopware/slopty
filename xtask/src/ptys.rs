//! Who holds the pseudo-terminals while the tests run: `xtask ptys -- <command>`, and the
//! sampler the gate's tests lane runs beside nextest.
//!
//! macOS allows `kern.tty.ptmx_max` pseudo-terminals at once (511, a hosted runner too). Twice a
//! runner's tests lane had an open refused as if all were in use, and a `lsof` after the lane
//! found nobody holding one. Counted while the tests ran, they held 25 at most: the refusals
//! were XNU's own race as its table of pairs grows (`docs/decisions/testing.md`). The sampler
//! keeps counting in every tests lane, so a test that holds too many, or leaves a process
//! holding one after it ends, fails the lane with its name.
//!
//! A pair is in use while a process holds its master (a descriptor of `/dev/ptmx`'s clones) or
//! its slave (`/dev/ttysN`), one pair to a device minor. Every [`EVERY`] the sampler lists this
//! user's processes and their character devices, and counts the pairs the run's own processes
//! hold: those under the root (the gate) and any that left it, known by the run's workspace in
//! the `NEXTEST_WORKSPACE_ROOT` they inherited or by having been seen under it before. A holder
//! is named by the test whose `NEXTEST_BINARY_ID` and `NEXTEST_TEST_NAME` it inherited. One whose
//! test is no longer running, nextest's child gone, *outlived* it: a leak once it does so for
//! [`GRACE`].
//!
//! It also opens one master itself after each census and closes it at once. The kernel hands
//! out the lowest free minor, so that minor bounds what the census cannot see (another user's
//! processes, a slot the kernel still holds), and an open refused again and again is the moment
//! the system ran out, with the census of that moment beside it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// How often the sampler counts.
pub const EVERY: Duration = Duration::from_millis(200);

/// How long a holder may outlive its test before it is a leak: a shell that is sent its hangup
/// as its test ends exits within milliseconds.
pub const GRACE: Duration = Duration::from_secs(2);

/// The most pairs the gate's tests may hold at once: a quarter of the 511 a Mac allows, which
/// this Mac's other sessions share. The suite held 25 at its peak (`docs/MEASUREMENTS.md`,
/// 2026-10-02).
pub const BUDGET: usize = 128;

/// What a sample saw of the kernel's own count, from the probe's open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    /// The lowest free minor: at least this many pairs below it are in use.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "only the macOS probe reads the lowest free minor")
    )]
    Free(u32),
    /// Every pair the system allows is in use: the open was refused (ENXIO) again and again.
    Exhausted,
    /// The open failed otherwise (the kernel asked for it again, say).
    Unknown,
}

/// A process holding pairs at one sample.
#[derive(Clone, Debug)]
struct Holder {
    pid: u32,
    comm: String,
    /// The test it belongs to, as `binary test` (the `JUnit` report's form).
    test: Option<String>,
    ptys: BTreeSet<u32>,
    /// Its test is no longer running.
    outlived: bool,
}

/// One count.
#[derive(Clone, Debug)]
struct Sample {
    at: Duration,
    /// Pairs the run's processes hold.
    ours: usize,
    /// Pairs any process of this user holds.
    visible: usize,
    probe: Probe,
    holders: Vec<Holder>,
}

/// A leak: pairs a process held after its test ended.
#[derive(Clone, Debug)]
struct Leak {
    ptys: usize,
    first: Duration,
    last: Duration,
}

/// What a run of the sampler found.
#[derive(Debug, Default)]
pub struct Tally {
    limit: Option<u32>,
    samples: u32,
    cost: Duration,
    peak: Option<Sample>,
    /// The first sample whose probe was refused.
    exhausted: Option<Sample>,
    /// The most pairs each test's processes held at once.
    by_test: BTreeMap<String, usize>,
    /// By `(test, process name, pid)`.
    leaks: BTreeMap<(String, String, u32), Leak>,
    /// Why the sampler could not count, when it could not.
    broken: Option<String>,
}

impl Tally {
    /// The most pairs the run held at once.
    pub fn peak(&self) -> usize {
        self.peak.as_ref().map_or(0, |s| s.ours)
    }

    /// The leaks that lasted past [`GRACE`], as `test (process pid)`.
    pub fn leaks(&self) -> Vec<String> {
        self.leaks
            .iter()
            .filter(|(_, leak)| leak.last.saturating_sub(leak.first) >= GRACE)
            .map(|((test, comm, pid), _)| format!("{test} ({comm} {pid})"))
            .collect()
    }

    /// Whether the run ran out of pairs, or held more than [`BUDGET`], or leaked any.
    pub fn verdict(&self) -> Result<()> {
        if let Some(sample) = &self.exhausted {
            anyhow::bail!(
                "the system ran out of pseudo-terminals at +{:.1?}, {} held by the tests: {}",
                sample.at,
                sample.ours,
                most(sample)
            );
        }
        if let Some(peak) = self.peak.as_ref().filter(|peak| peak.ours > BUDGET) {
            anyhow::bail!(
                "the tests held {} pseudo-terminals at once, over the budget of {BUDGET}: {}",
                peak.ours,
                most(peak)
            );
        }
        let leaks = self.leaks();
        anyhow::ensure!(
            leaks.is_empty(),
            "pseudo-terminals held for {GRACE:?} or longer after their test ended: {}",
            leaks.join(", ")
        );
        Ok(())
    }

    /// The account of the run: the peak and its holders, the moment the system ran out if it
    /// did, the most each test held, and the leaks.
    pub fn report(&self) -> String {
        let mut text = String::new();
        if let Some(why) = &self.broken {
            let _written = writeln!(text, "ptys: not counted: {why}");
            return text;
        }
        let limit = self.limit.map_or_else(|| "?".to_owned(), |l| l.to_string());
        let per = self.cost.checked_div(self.samples.max(1)).unwrap_or_default();
        let _written = writeln!(
            text,
            "ptys: peak {} of {limit} held by the tests ({} samples every {EVERY:?}, {per:.1?} each)",
            self.peak(),
            self.samples,
        );
        if let Some(sample) = &self.exhausted {
            let _written = writeln!(text, "  ran out of pseudo-terminals at:");
            describe(&mut text, sample);
        }
        if let Some(sample) = self.peak.as_ref().filter(|s| s.ours > 0) {
            let _written = writeln!(text, "  at the peak:");
            describe(&mut text, sample);
        }
        let mut tests: Vec<(&String, &usize)> = self.by_test.iter().collect();
        tests.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        if !tests.is_empty() {
            let _written = writeln!(text, "  the most one test held at once:");
            for (test, ptys) in tests.into_iter().take(12) {
                let _written = writeln!(text, "    {ptys:>4}  {test}");
            }
        }
        if !self.leaks.is_empty() {
            let _written = writeln!(
                text,
                "  held after their test ended (a leak from {GRACE:?} on, which fails the lane):"
            );
            for ((test, comm, pid), leak) in &self.leaks {
                let lasted = leak.last.saturating_sub(leak.first);
                let seen = if lasted.is_zero() {
                    format!("in one sample at +{:.1?}", leak.first)
                } else {
                    format!("for {lasted:.1?} from +{:.1?}", leak.first)
                };
                let _written =
                    writeln!(text, "    {:>4}  {comm} {pid}, {seen}, of {test}", leak.ptys);
            }
        }
        text
    }
}

/// The pairs each test's processes held at `sample`, most first.
fn by_test(sample: &Sample) -> Vec<(&str, usize)> {
    let mut by_test: HashMap<&str, BTreeSet<u32>> = HashMap::new();
    for holder in &sample.holders {
        let test = holder.test.as_deref().unwrap_or("(no test)");
        by_test.entry(test).or_default().extend(&holder.ptys);
    }
    let mut tests: Vec<(&str, usize)> = by_test.into_iter().map(|(t, p)| (t, p.len())).collect();
    tests.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    tests
}

/// The tests that held the most of `sample`'s pairs, as `test (pairs)`, five at most.
fn most(sample: &Sample) -> String {
    let named: Vec<String> =
        by_test(sample).into_iter().take(5).map(|(t, n)| format!("{t} ({n})")).collect();
    named.join(", ")
}

/// One sample's numbers and holders, most pairs first.
fn describe(text: &mut String, sample: &Sample) {
    let probe = match sample.probe {
        Probe::Free(minor) => format!("lowest free minor {minor}"),
        Probe::Exhausted => "the probe's open refused (ENXIO)".to_owned(),
        Probe::Unknown => "the probe's open failed".to_owned(),
    };
    let _written = writeln!(
        text,
        "    +{:.1?}: {} held by the tests, {} by this user, {probe}",
        sample.at, sample.ours, sample.visible,
    );
    let mut holders: Vec<&Holder> = sample.holders.iter().collect();
    holders.sort_by(|a, b| b.ptys.len().cmp(&a.ptys.len()).then_with(|| a.pid.cmp(&b.pid)));
    for holder in holders.into_iter().take(24) {
        let test = holder.test.as_deref().unwrap_or("(no test)");
        let after = if holder.outlived { ", after its test ended" } else { "" };
        let _written = writeln!(
            text,
            "    {:>4}  {} {}  {test}{after}",
            holder.ptys.len(),
            holder.comm,
            holder.pid,
        );
    }
}

/// A sampler counting on a thread of its own until [`Sampler::finish`].
#[derive(Debug)]
pub struct Sampler {
    stop: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<Tally>,
}

impl Sampler {
    /// Count every [`EVERY`] the pairs held under `root` (a pid) and by the processes of the
    /// tests of `workspace`, writing one line per sample to `log` (`ms ours visible free`).
    pub fn start(root: u32, workspace: &Utf8Path, log: Option<Utf8PathBuf>) -> Self {
        let (stop, stopped) = mpsc::channel();
        let workspace = workspace.to_string();
        let thread = std::thread::spawn(move || run(root, &workspace, log.as_deref(), &stopped));
        Self { stop, thread }
    }

    /// Stop counting and return what was counted.
    pub fn finish(self) -> Tally {
        let _sent = self.stop.send(());
        self.thread.join().unwrap_or_else(|_panic| Tally {
            broken: Some("the sampler panicked".to_owned()),
            ..Tally::default()
        })
    }
}

fn run(root: u32, workspace: &str, log: Option<&Utf8Path>, stopped: &mpsc::Receiver<()>) -> Tally {
    let mut tally = Tally { limit: sys::ptmx_max(), ..Tally::default() };
    let Some(majors) = sys::majors() else {
        tally.broken = Some("no pseudo-terminal could be opened to learn its device".to_owned());
        return tally;
    };
    let mut log = log.and_then(|path| {
        path.parent().map(std::fs::create_dir_all);
        std::fs::File::create(path).ok()
    });
    let mut tracker = Tracker::new(root, workspace);
    let started = Instant::now();
    loop {
        let began = Instant::now();
        let sample = tracker.sample(started.elapsed(), majors);
        tally.cost = tally.cost.saturating_add(began.elapsed());
        tally.samples = tally.samples.saturating_add(1);
        if let Some(log) = &mut log {
            let free = match sample.probe {
                Probe::Free(minor) => minor.to_string(),
                Probe::Exhausted => "full".to_owned(),
                Probe::Unknown => "?".to_owned(),
            };
            // A line a write, unbuffered, so the log reads up to the moment a run is ended.
            let line =
                format!("{} {} {} {free}\n", sample.at.as_millis(), sample.ours, sample.visible);
            let _written = std::io::Write::write_all(log, line.as_bytes());
        }
        tally.add(sample);
        match stopped.recv_timeout(EVERY.saturating_sub(began.elapsed())) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    tally
}

impl Tally {
    fn add(&mut self, sample: Sample) {
        for holder in sample.holders.iter().filter(|h| h.outlived) {
            let test = holder.test.clone().unwrap_or_default();
            let key = (test, holder.comm.clone(), holder.pid);
            let leak = self.leaks.entry(key).or_insert(Leak {
                ptys: 0,
                first: sample.at,
                last: sample.at,
            });
            leak.ptys = leak.ptys.max(holder.ptys.len());
            leak.last = sample.at;
        }
        for (test, ptys) in by_test(&sample) {
            let most = self.by_test.entry(test.to_owned()).or_default();
            *most = (*most).max(ptys);
        }
        if self.exhausted.is_none() && sample.probe == Probe::Exhausted {
            self.exhausted = Some(sample.clone());
        }
        if self.peak.as_ref().is_none_or(|peak| sample.ours > peak.ours) {
            self.peak = Some(sample);
        }
    }
}

/// A process at one census.
#[derive(Clone, Debug)]
struct Proc {
    ppid: u32,
    /// Its start, which with the pid names it across samples.
    start: u64,
    comm: String,
}

/// One of the run's processes as last seen.
struct Known {
    comm: String,
    /// The test it belongs to.
    test: Option<String>,
}

/// What is remembered of the run's processes across samples, .
struct Tracker {
    root: u32,
    workspace: String,
    /// The run's processes seen so far, by pid and start.
    ours: HashMap<(u32, u64), Known>,
    /// Processes seen not to be the run's, by pid, start and name, so their environment is read
    /// once per program.
    theirs: HashSet<(u32, u64, String)>,
}

impl Tracker {
    fn new(root: u32, workspace: &str) -> Self {
        Self { root, workspace: workspace.to_owned(), ours: HashMap::new(), theirs: HashSet::new() }
    }

    fn sample(&mut self, at: Duration, majors: sys::Majors) -> Sample {
        let procs: HashMap<u32, Proc> = sys::processes();
        let mut held: HashMap<u32, BTreeSet<u32>> = HashMap::new();
        for &pid in procs.keys() {
            let ptys = sys::ptys(pid, majors);
            if !ptys.is_empty() {
                held.insert(pid, ptys);
            }
        }
        let mut tests: HashMap<u32, Option<String>> = HashMap::new();
        for &pid in procs.keys() {
            self.classify(pid, &procs, &held, &mut tests, 0);
        }
        // A test runs while nextest's child for it does.
        let running: HashSet<&str> = tests
            .iter()
            .filter(|(pid, _)| {
                procs
                    .get(pid)
                    .and_then(|p| procs.get(&p.ppid))
                    .is_some_and(|parent| parent.comm == "cargo-nextest")
            })
            .filter_map(|(_, test)| test.as_deref())
            .collect();
        let mut ours: BTreeSet<u32> = BTreeSet::new();
        let mut visible: BTreeSet<u32> = BTreeSet::new();
        let mut holders = Vec::new();
        for (pid, ptys) in &held {
            visible.extend(ptys);
            let (Some(test), Some(proc)) = (tests.get(pid), procs.get(pid)) else { continue };
            ours.extend(ptys);
            holders.push(Holder {
                pid: *pid,
                comm: proc.comm.clone(),
                test: test.clone(),
                ptys: ptys.clone(),
                outlived: test.as_deref().is_some_and(|t| !running.contains(t)),
            });
        }
        Sample { at, ours: ours.len(), visible: visible.len(), probe: sys::probe(), holders }
    }

    /// Whether `pid` is the run's, recorded in `tests` with the test it belongs to when it is.
    fn classify(
        &mut self,
        pid: u32,
        procs: &HashMap<u32, Proc>,
        held: &HashMap<u32, BTreeSet<u32>>,
        tests: &mut HashMap<u32, Option<String>>,
        depth: u8,
    ) -> bool {
        if tests.contains_key(&pid) {
            return true;
        }
        let Some(proc) = procs.get(&pid) else { return false };
        let key = (pid, proc.start);
        if let Some(known) = self.ours.get(&key) {
            // An exec keeps the pid and the start; the program may have been given another test.
            let test = if known.comm == proc.comm {
                known.test.clone()
            } else {
                let test =
                    sys::nextest_env(pid).and_then(|env| env.test()).or_else(|| known.test.clone());
                self.ours.insert(key, Known { comm: proc.comm.clone(), test: test.clone() });
                test
            };
            tests.insert(pid, test);
            return true;
        }
        let named = (pid, proc.start, proc.comm.clone());
        if self.theirs.contains(&named) {
            return false;
        }
        // What a process inherited reads as nothing while it execs, and always for Apple's own
        // programs (`/bin/sh`, `sleep`): those take their parent's test. An orphan is asked again
        // at the next sample.
        let (found, settled) = if pid == self.root {
            (Some(None), true)
        } else if depth < 64
            && self.classify(proc.ppid, procs, held, tests, depth.saturating_add(1))
        {
            let parent = tests.get(&proc.ppid).cloned().flatten();
            (Some(sys::nextest_env(pid).and_then(|env| env.test()).or(parent)), true)
        } else if held.contains_key(&pid) || proc.ppid == 1 {
            // Left the tree (its parent gone, so launchd's child): the run's by what it inherited.
            let env = sys::nextest_env(pid);
            let settled = env.as_ref().is_some_and(|env| env.workspace.is_some());
            let ours = env.filter(|env| env.workspace.as_deref() == Some(self.workspace.as_str()));
            (ours.map(|env| env.test()), settled)
        } else {
            (None, true)
        };
        let Some(test) = found else {
            if settled {
                self.theirs.insert(named);
            }
            return false;
        };
        self.ours.insert(key, Known { comm: proc.comm.clone(), test: test.clone() });
        tests.insert(pid, test);
        true
    }
}

/// What a process inherited from nextest.
#[derive(Debug, Default)]
struct NextestEnv {
    binary: Option<String>,
    test: Option<String>,
    workspace: Option<String>,
}

impl NextestEnv {
    /// `binary test`, as the `JUnit` report names it.
    fn test(&self) -> Option<String> {
        Some(format!("{} {}", self.binary.as_deref()?, self.test.as_deref()?))
    }

    /// From a process's `KERN_PROCARGS2`: its argument count, its executable's path, NUL
    /// padding, its arguments (an empty one among them), then its environment up to an empty
    /// string, each NUL-terminated.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "nothing is counted off macOS, where the tests lane runs")
    )]
    fn parse(args: &[u8]) -> Self {
        Self::parse_strings(args).unwrap_or_default()
    }

    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "nothing is counted off macOS, where the tests lane runs")
    )]
    fn parse_strings(args: &[u8]) -> Option<Self> {
        let mut env = Self::default();
        let (argc, rest) = args.split_first_chunk::<4>()?;
        let argc = usize::try_from(i32::from_ne_bytes(*argc)).ok()?;
        let rest = rest.get(rest.iter().position(|&b| b == 0)?..)?;
        let rest = rest.get(rest.iter().position(|&b| b != 0)?..)?;
        let mut strings = rest.split(|&b| b == 0);
        for _ in 0..argc {
            strings.next()?;
        }
        for entry in strings.take_while(|s| !s.is_empty()) {
            let Ok(entry) = std::str::from_utf8(entry) else { continue };
            let Some((key, value)) = entry.split_once('=') else { continue };
            let slot = match key {
                "NEXTEST_BINARY_ID" => &mut env.binary,
                "NEXTEST_TEST_NAME" => &mut env.test,
                "NEXTEST_WORKSPACE_ROOT" => &mut env.workspace,
                _ => continue,
            };
            *slot = Some(value.to_owned());
        }
        Some(env)
    }
}

/// `xtask ptys -- <command>`: run `command`, count while it runs, print the account, and write
/// the samples to `log`. The command's own exit status is returned.
pub fn watch(workspace: &Utf8Path, log: &Utf8Path, command: &[std::ffi::OsString]) -> Result<()> {
    let (program, args) = command.split_first().context("no command to run")?;
    let mut child = std::process::Command::new(program)
        .args(args)
        .spawn()
        .with_context(|| format!("run {}", program.display()))?;
    let sampler = Sampler::start(child.id(), workspace, Some(log.to_owned()));
    let status = child.wait()?;
    let tally = sampler.finish();
    print!("{}", tally.report());
    println!("ptys: samples in {log}");
    let verdict = tally.verdict();
    anyhow::ensure!(status.success(), "{} failed: {status}", program.display());
    verdict
}

#[cfg(target_os = "macos")]
mod sys {
    //! libproc and `sysctl` on macOS.

    use std::collections::{BTreeSet, HashMap};
    use std::mem::MaybeUninit;

    use libc::{
        PROC_PIDLISTFDS, PROC_PIDTBSDINFO, PROX_FDTYPE_VNODE, c_int, c_void, proc_bsdinfo,
        proc_fdinfo, proc_pidfdinfo, proc_pidinfo, vnode_info,
    };

    use super::{NextestEnv, Probe, Proc};

    /// The device majors of the masters (`/dev/ptmx`'s clones) and the slaves (`/dev/ttysN`).
    #[derive(Clone, Copy, Debug)]
    pub(super) struct Majors {
        master: u32,
        slave: u32,
    }

    /// `sys/proc_info.h`'s `struct proc_fileinfo`, which libc does not declare.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct FileInfo {
        openflags: u32,
        status: u32,
        offset: i64,
        kind: i32,
        guardflags: u32,
    }

    /// `sys/proc_info.h`'s `struct vnode_fdinfo`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VnodeFdInfo {
        pfi: FileInfo,
        pvi: vnode_info,
    }

    /// How many refusals in a row [`probe`] takes for exhaustion.
    const REFUSALS: usize = 16;

    /// `PROC_PIDFDVNODEINFO` (`sys/proc_info.h`), which libc does not declare.
    const PROC_PIDFDVNODEINFO: c_int = 1;

    const fn major(rdev: u32) -> u32 {
        (rdev >> 24) & 0xff
    }

    const fn minor(rdev: u32) -> u32 {
        rdev & 0x00ff_ffff
    }

    pub(super) fn ptmx_max() -> Option<u32> {
        let mut value: c_int = 0;
        let mut len = size_of::<c_int>();
        // SAFETY: `sysctlbyname` (sys/sysctl.h) writes at most `len` bytes, one `int` for
        // `kern.tty.ptmx_max`, into `value`, and the name is NUL-terminated.
        let done = unsafe {
            libc::sysctlbyname(
                c"kern.tty.ptmx_max".as_ptr(),
                (&raw mut value).cast::<c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (done == 0).then(|| u32::try_from(value).ok()).flatten()
    }

    /// A master's clone, opened close-on-exec, so no test spawned meanwhile inherits it.
    fn open_master() -> rustix::io::Result<rustix::fd::OwnedFd> {
        use rustix::fs::{Mode, OFlags};
        rustix::fs::open(
            c"/dev/ptmx",
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
    }

    /// The majors, from a pair opened for the purpose and closed at once.
    pub(super) fn majors() -> Option<Majors> {
        let master = open_master().ok()?;
        let name = rustix::pty::ptsname(&master, Vec::new()).ok()?;
        let slave = rustix::fs::stat(name.as_c_str()).ok()?;
        let master = rustix::fs::fstat(&master).ok()?;
        let rdev = |stat: rustix::fs::Stat| u32::try_from(stat.st_rdev).ok();
        Some(Majors { master: major(rdev(master)?), slave: major(rdev(slave)?) })
    }

    /// The lowest free minor, from a master opened and closed at once. XNU refuses an open with
    /// ENXIO far below the limit when its table of pairs is full and must grow while another
    /// pair is closed (`bsd/kern/tty_ptmx.c`), so a refusal is taken for exhaustion only when
    /// it comes [`REFUSALS`] times in a row.
    pub(super) fn probe() -> Probe {
        let mut opened = open_master();
        for _ in 1..REFUSALS {
            if !matches!(opened, Err(rustix::io::Errno::NXIO)) {
                break;
            }
            std::thread::yield_now();
            opened = open_master();
        }
        match opened {
            Ok(master) => rustix::fs::fstat(&master)
                .ok()
                .and_then(|stat| u32::try_from(stat.st_rdev).ok())
                .map_or(Probe::Unknown, |rdev| Probe::Free(minor(rdev))),
            Err(rustix::io::Errno::NXIO) => Probe::Exhausted,
            Err(_) => Probe::Unknown,
        }
    }

    /// This user's processes, by pid.
    pub(super) fn processes() -> HashMap<u32, Proc> {
        let uid = rustix::process::geteuid().as_raw();
        let mut room = 4096_usize;
        let pids = loop {
            let mut pids = vec![0_i32; room];
            let Ok(bytes) = c_int::try_from(room.saturating_mul(size_of::<c_int>())) else {
                return HashMap::new();
            };
            // SAFETY: `proc_listallpids` (libproc.h) writes at most `bytes` bytes of pids into
            // the buffer, which is `room` `int`s long, and returns how many it wrote.
            let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
            let Ok(count) = usize::try_from(count) else { return HashMap::new() };
            if count < room {
                pids.truncate(count);
                break pids;
            }
            room = room.saturating_mul(2);
        };
        pids.into_iter()
            .filter_map(|pid| {
                let info = bsdinfo(pid)?;
                (info.pbi_uid == uid).then(|| {
                    let comm: Vec<u8> = info
                        .pbi_comm
                        .iter()
                        .map_while(|&c| u8::try_from(c).ok().filter(|&b| b != 0))
                        .collect();
                    let proc = Proc {
                        ppid: info.pbi_ppid,
                        start: info
                            .pbi_start_tvsec
                            .saturating_mul(1_000_000)
                            .saturating_add(info.pbi_start_tvusec),
                        comm: String::from_utf8_lossy(&comm).into_owned(),
                    };
                    Some((u32::try_from(pid).ok()?, proc))
                })?
            })
            .collect()
    }

    fn bsdinfo(pid: c_int) -> Option<proc_bsdinfo> {
        let mut info = MaybeUninit::<proc_bsdinfo>::zeroed();
        let bytes = c_int::try_from(size_of::<proc_bsdinfo>()).ok()?;
        // SAFETY: the buffer is one `proc_bsdinfo`, `bytes` long, which is what `proc_pidinfo`
        // (libproc.h) writes for `PROC_PIDTBSDINFO`.
        let wrote = unsafe {
            proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast::<c_void>(), bytes)
        };
        // SAFETY: zeroed is a valid `proc_bsdinfo` (plain integers), and the call wrote it all.
        (wrote == bytes).then(|| unsafe { info.assume_init() })
    }

    /// The minors of the pairs `pid` holds a master or a slave of.
    pub(super) fn ptys(pid: u32, majors: Majors) -> BTreeSet<u32> {
        let Ok(pid) = c_int::try_from(pid) else { return BTreeSet::new() };
        fds(pid)
            .into_iter()
            .filter(|fd| Some(fd.proc_fdtype) == u32::try_from(PROX_FDTYPE_VNODE).ok())
            .filter_map(|fd| {
                let rdev = char_device(pid, fd.proc_fd)?;
                let major = major(rdev);
                (major == majors.master || major == majors.slave).then_some(minor(rdev))
            })
            .collect()
    }

    /// The descriptors `pid` holds: `PROC_PIDLISTFDS` asked with a buffer grown until the list
    /// fits, since a null buffer is answered with the table's size, not the count open.
    fn fds(pid: c_int) -> Vec<proc_fdinfo> {
        let entry = size_of::<proc_fdinfo>();
        let mut room = 256_usize;
        loop {
            let mut buffer = vec![proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; room];
            let Some(bytes) = room.checked_mul(entry).and_then(|b| c_int::try_from(b).ok()) else {
                return Vec::new();
            };
            // SAFETY: the buffer holds `room` entries of `proc_fdinfo`, `bytes` long, which is
            // what `proc_pidinfo` (libproc.h) may write for `PROC_PIDLISTFDS`; it writes whole
            // entries and returns the bytes written.
            let wrote = unsafe {
                proc_pidinfo(pid, PROC_PIDLISTFDS, 0, buffer.as_mut_ptr().cast::<c_void>(), bytes)
            };
            let Some(count) = usize::try_from(wrote).ok().and_then(|w| w.checked_div(entry)) else {
                return Vec::new();
            };
            if count < room {
                buffer.truncate(count);
                return buffer;
            }
            room = room.saturating_mul(4);
        }
    }

    /// The device of `pid`'s descriptor `fd` when it is a character device.
    fn char_device(pid: c_int, fd: i32) -> Option<u32> {
        let mut info = MaybeUninit::<VnodeFdInfo>::zeroed();
        let bytes = c_int::try_from(size_of::<VnodeFdInfo>()).ok()?;
        // SAFETY: the buffer is one `vnode_fdinfo`, `bytes` long, which is what
        // `proc_pidfdinfo` (libproc.h) writes for `PROC_PIDFDVNODEINFO`.
        let wrote = unsafe {
            proc_pidfdinfo(pid, fd, PROC_PIDFDVNODEINFO, info.as_mut_ptr().cast::<c_void>(), bytes)
        };
        if wrote != bytes {
            return None;
        }
        // SAFETY: zeroed is a valid `vnode_fdinfo` (plain integers), and the call wrote it all.
        let info = unsafe { info.assume_init() };
        let stat = info.pvi.vi_stat;
        (stat.vst_mode & libc::S_IFMT == libc::S_IFCHR).then_some(stat.vst_rdev)
    }

    /// What `pid` inherited from nextest, from its `KERN_PROCARGS2`.
    pub(super) fn nextest_env(pid: u32) -> Option<NextestEnv> {
        let pid = c_int::try_from(pid).ok()?;
        let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        let mut len = 0_usize;
        // SAFETY: `sysctl` (sys/sysctl.h) with a null buffer writes only the size it needs to
        // `len`; the name is three `int`s, as `KERN_PROCARGS2` takes.
        let sized = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                std::ptr::null_mut(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if sized != 0 || len == 0 {
            return None;
        }
        let mut args = vec![0_u8; len];
        // SAFETY: `sysctl` writes at most `len` bytes, the buffer's length, and sets `len` to
        // what it wrote.
        let read = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                3,
                args.as_mut_ptr().cast::<c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if read != 0 {
            return None;
        }
        args.truncate(len);
        Some(NextestEnv::parse(&args))
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    //! Nothing is counted off macOS: the tests lane runs on a Mac.

    use std::collections::{BTreeSet, HashMap};

    use super::{NextestEnv, Probe, Proc};

    #[derive(Clone, Copy, Debug)]
    pub(super) struct Majors;

    pub(super) const fn ptmx_max() -> Option<u32> {
        None
    }

    pub(super) const fn majors() -> Option<Majors> {
        None
    }

    pub(super) const fn probe() -> Probe {
        Probe::Unknown
    }

    pub(super) fn processes() -> HashMap<u32, Proc> {
        HashMap::new()
    }

    pub(super) const fn ptys(_pid: u32, _majors: Majors) -> BTreeSet<u32> {
        BTreeSet::new()
    }

    pub(super) const fn nextest_env(_pid: u32) -> Option<NextestEnv> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use super::{BUDGET, GRACE, Holder, NextestEnv, Probe, Sample, Tally};

    fn holding(test: &str, ptys: std::ops::Range<u32>, outlived: bool) -> Holder {
        Holder {
            pid: 7,
            comm: "sh".to_owned(),
            test: Some(test.to_owned()),
            ptys: ptys.collect(),
            outlived,
        }
    }

    fn sample(at: Duration, probe: Probe, holders: Vec<Holder>) -> Sample {
        let ours: BTreeSet<u32> = holders.iter().flat_map(|h| h.ptys.iter().copied()).collect();
        Sample { at, ours: ours.len(), visible: ours.len(), probe, holders }
    }

    /// A child that keeps a pair after its test is gone is found by the census, named by the
    /// test it inherited, and is a leak once it has been so for the grace; killed, it is gone.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pair_a_child_keeps_after_its_test_is_named_as_a_leak() {
        use std::os::unix::process::CommandExt as _;

        use rustix::fs::{Mode, OFlags};

        let master = rustix::fs::open(
            c"/dev/ptmx",
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        rustix::pty::grantpt(&master).unwrap();
        rustix::pty::unlockpt(&master).unwrap();
        let name = rustix::pty::ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(name.as_c_str(), OFlags::RDWR | OFlags::NOCTTY, Mode::empty())
            .unwrap();
        let minor =
            u32::try_from(rustix::fs::fstat(&slave).unwrap().st_rdev).unwrap() & 0x00ff_ffff;
        // Not one of Apple's programs, whose environment no other process may read.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ptys::tests::holds_its_stdin", "--ignored", "-q"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::from(slave))
            .env("NEXTEST_BINARY_ID", "xtask::ptys")
            .env("NEXTEST_TEST_NAME", "gone")
            .process_group(0)
            .spawn()
            .unwrap();

        let majors = super::sys::majors().unwrap();
        let mut tracker = super::Tracker::new(std::process::id(), "/nonexistent");
        let first = tracker.sample(Duration::ZERO, majors);
        let held = first.holders.iter().find(|h| h.pid == child.id()).cloned();
        let mut tally = Tally::default();
        tally.add(first);
        tally.add(tracker.sample(GRACE, majors));
        child.kill().unwrap();
        child.wait().unwrap();
        let after = tracker.sample(GRACE.saturating_mul(2), majors);

        let held = held.expect("the census sees the child's pair");
        assert_eq!(held.test.as_deref(), Some("xtask::ptys gone"));
        assert!(held.ptys.contains(&minor), "{held:?} holds {minor}");
        assert!(held.outlived, "its test is not running");
        assert_eq!(tally.leaks(), [format!("xtask::ptys gone ({} {})", held.comm, held.pid)]);
        let verdict = tally.verdict().unwrap_err().to_string();
        assert!(verdict.contains("after their test ended: xtask::ptys gone"), "{verdict}");
        assert!(after.holders.iter().all(|h| h.pid != held.pid), "the killed child holds nothing");
    }

    /// The child of [`a_pair_a_child_keeps_after_its_test_is_named_as_a_leak`]: it holds the
    /// pair on its stdin until it is killed.
    #[test]
    #[ignore = "run by a_pair_a_child_keeps_after_its_test_is_named_as_a_leak"]
    fn holds_its_stdin() {
        let mut byte = [0_u8; 1];
        let _read = std::io::Read::read(&mut std::io::stdin(), &mut byte);
    }

    /// A holder seen after its test only within the grace is no leak, one seen past it is, and
    /// a peak over the budget or a run out of pairs fails with the tests that held the most.
    #[test]
    fn the_verdict_names_a_leak_a_peak_over_budget_and_exhaustion() {
        let mut tally = Tally::default();
        let brief = holding("a quick", 0..1, true);
        tally.add(sample(Duration::ZERO, Probe::Free(1), vec![brief.clone()]));
        tally.add(sample(
            GRACE.saturating_sub(Duration::from_millis(1)),
            Probe::Free(1),
            vec![brief],
        ));
        tally.verdict().unwrap();

        let budget = u32::try_from(BUDGET).unwrap();
        let mut over = Tally::default();
        let wide = holding("b wide", 0..budget.saturating_add(1), false);
        over.add(sample(Duration::from_secs(3), Probe::Free(budget), vec![wide]));
        let verdict = over.verdict().unwrap_err().to_string();
        assert!(verdict.contains("over the budget of 128: b wide (129)"), "{verdict}");

        let mut out = Tally::default();
        let some = holding("c some", 0..3, false);
        out.add(sample(Duration::from_secs(4), Probe::Exhausted, vec![some]));
        let verdict = out.verdict().unwrap_err().to_string();
        assert!(
            verdict
                .contains("ran out of pseudo-terminals at +4.0s, 3 held by the tests: c some (3)"),
            "{verdict}"
        );
        assert!(out.report().contains("ran out of pseudo-terminals at:"), "{}", out.report());
    }

    /// The environment is read past the executable's path, its padding and the arguments; an
    /// argument that looks like a variable is not taken for one.
    #[test]
    fn the_tests_name_is_read_from_the_environment_not_the_arguments() {
        let mut args = 3_i32.to_ne_bytes().to_vec();
        args.extend_from_slice(b"/bin/test\0\0\0\0test\0\0NEXTEST_TEST_NAME=argv\0");
        args.extend_from_slice(b"PATH=/bin\0NEXTEST_BINARY_ID=slopty-pty::spawn\0");
        args.extend_from_slice(b"NEXTEST_TEST_NAME=spawn::a\0NEXTEST_WORKSPACE_ROOT=/w\0");
        let env = NextestEnv::parse(&args);
        assert_eq!(env.test().as_deref(), Some("slopty-pty::spawn spawn::a"));
        assert_eq!(env.workspace.as_deref(), Some("/w"));
        assert!(NextestEnv::parse(&[1, 0]).test().is_none(), "a short buffer names nothing");
    }
}
