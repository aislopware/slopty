//! How hard a step of a CI test lane pressed the macOS runner's memory: the VM's counters from
//! `vm_stat` before and after it, as the step's differences.
//!
//! A hosted Mac has three cores and 7 GB, no swap in use, and its test shards' times swing by
//! ±100 s. At a job's end the worker and rest shards had paged out 90 000–155 000 pages against
//! 3 000–8 000 for the UI shard, and decompressed 400 000–1 100 000 (gate runs 38144514113 to
//! 38151148784). Which step does it, the test build's links or the tests, decides what to change.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xshell::{Shell, cmd};

/// The counters a step is judged by, in pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Pages the compressor took in.
    compressions: u64,
    /// Pages it gave back, each a fault on a page the system had squeezed out.
    decompressions: u64,
    /// Pages written out to their files.
    pageouts: u64,
    /// Pages written to swap.
    swapouts: u64,
    /// Pages free when read.
    free: u64,
    /// The page size, in bytes.
    page: u64,
}

/// `vm_stat`'s counters from its `text`; `None` when it lacks one.
fn parse(text: &str) -> Option<Counters> {
    let field = |name: &str| -> Option<u64> {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim().trim_matches('"') == name)
                .then(|| value.trim().trim_end_matches('.').parse().ok())
                .flatten()
        })
    };
    let page = text
        .split_once("page size of ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())?;
    Some(Counters {
        compressions: field("Compressions")?,
        decompressions: field("Decompressions")?,
        pageouts: field("Pageouts")?,
        swapouts: field("Swapouts")?,
        free: field("Pages free")?,
        page,
    })
}

/// The counters now, on a CI runner's Mac; `None` anywhere else, or when `vm_stat` fails.
pub fn read(sh: &Shell) -> Option<Counters> {
    if !cfg!(target_os = "macos") || std::env::var_os("CI").is_none() {
        return None;
    }
    parse(&cmd!(sh, "vm_stat").quiet().read().ok()?)
}

/// The line that says what happened to memory from `before` to `after`, a step of the lane.
fn line(step: &str, before: Counters, after: Counters) -> String {
    let mb = |pages: u64| pages.saturating_mul(after.page) / (1 << 20);
    format!(
        "  memory, {step}: {} MB compressed, {} MB decompressed, {} MB paged out, {} MB swapped \
         out; {} MB free after",
        mb(after.compressions.saturating_sub(before.compressions)),
        mb(after.decompressions.saturating_sub(before.decompressions)),
        mb(after.pageouts.saturating_sub(before.pageouts)),
        mb(after.swapouts.saturating_sub(before.swapouts)),
        mb(after.free),
    )
}

/// Prints what `step` did to memory since `before` was read, and the counters now, for the
/// next step.
pub fn report(sh: &Shell, step: &str, before: Option<Counters>) -> Option<Counters> {
    let after = read(sh)?;
    if let Some(before) = before {
        println!("{}", line(step, before, after));
    }
    Some(after)
}

/// How often [`Watch`] lists the processes.
const EVERY: Duration = Duration::from_secs(10);

/// A process as `ps -axo pid=,rss=,comm=` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    pid: u32,
    /// Resident memory, in KB.
    rss: u64,
    /// The command's file name, without its directory.
    name: String,
}

/// The processes in `ps -axo pid=,rss=,comm=`'s `text`; a line it cannot read is left out.
fn processes(text: &str) -> Vec<Process> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let rss = words.next()?.parse().ok()?;
            let command = words.collect::<Vec<_>>().join(" ");
            let name = command.rsplit('/').next().unwrap_or(&command).to_owned();
            (!name.is_empty()).then_some(Process { pid, rss, name })
        })
        .collect()
}

/// The processes now; none when `ps` fails.
fn list() -> Vec<Process> {
    std::process::Command::new("ps")
        .args(["-axo", "pid=,rss=,comm="])
        .output()
        .ok()
        .map(|out| processes(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or_default()
}

/// Resident memory by command, in MB, largest first.
fn by_command(processes: &[Process]) -> Vec<(String, u64)> {
    let mut sums: BTreeMap<&str, u64> = BTreeMap::new();
    for p in processes {
        let sum = sums.entry(p.name.as_str()).or_default();
        *sum = sum.saturating_add(p.rss);
    }
    let mut sums: Vec<(String, u64)> =
        sums.into_iter().map(|(name, kb)| (name.to_owned(), kb / 1024)).collect();
    sums.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sums
}

/// A process of the workspace's own, or a stand-in a test starts: one alive once the tests are
/// done outlived the test that started it.
fn ours(name: &str) -> bool {
    name.starts_with("slopty") || name.contains("stub")
}

/// Lists the processes every [`EVERY`] while the tests run, on a CI runner's Mac: what holds the
/// memory as the run goes, and what of ours outlives it.
pub struct Watch {
    stop: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<Vec<String>>,
}

impl Watch {
    /// Starts listing; `None` anywhere but a CI runner's Mac.
    pub fn start() -> Option<Self> {
        if !cfg!(target_os = "macos") || std::env::var_os("CI").is_none() {
            return None;
        }
        let (stop, stopped) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let started = Instant::now();
            let mut lines = Vec::new();
            loop {
                let all = list();
                let total: u64 = all.iter().map(|p| p.rss).sum::<u64>() / 1024;
                let top: Vec<String> = by_command(&all)
                    .into_iter()
                    .take(5)
                    .map(|(name, mb)| format!("{name} {mb}"))
                    .collect();
                lines.push(format!(
                    "  memory at {:>4} s: {total} MB resident; MB by command: {}",
                    started.elapsed().as_secs(),
                    top.join(", ")
                ));
                if !matches!(stopped.recv_timeout(EVERY), Err(mpsc::RecvTimeoutError::Timeout)) {
                    return lines;
                }
            }
        });
        Some(Self { stop, thread })
    }

    /// Stops listing and prints what was seen, and every process of ours still alive.
    pub fn finish(self) {
        let _sent = self.stop.send(());
        for line in self.thread.join().unwrap_or_default() {
            println!("{line}");
        }
        let left: Vec<String> = list()
            .into_iter()
            .filter(|p| ours(&p.name))
            .map(|p| format!("{} (pid {}, {} MB)", p.name, p.pid, p.rss / 1024))
            .collect();
        if left.is_empty() {
            println!("  memory: no process of ours outlived the tests");
        } else {
            println!("  memory: alive after the tests: {}", left.join(", "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Counters, Process, by_command, line, ours, parse, processes};

    const VM_STAT: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)
Pages free:                                   327412.
\"Translation faults\":                    56301218876.
Decompressions:                           3164540394.
Compressions:                             4108267130.
Pageouts:                                     623237.
Swapouts:                                   60804356.
";

    #[test]
    fn vm_stat_reads_as_its_counters_and_a_step_as_their_differences() {
        let now = parse(VM_STAT).unwrap();
        assert_eq!(
            now,
            Counters {
                compressions: 4_108_267_130,
                decompressions: 3_164_540_394,
                pageouts: 623_237,
                swapouts: 60_804_356,
                free: 327_412,
                page: 16_384,
            }
        );
        let before = Counters { compressions: now.compressions - 64, pageouts: 600_000, ..now };
        let said = line("test build", before, now);
        assert!(said.contains("1 MB compressed, 0 MB decompressed, 363 MB paged out"), "{said}");
        assert!(said.ends_with("5115 MB free after"), "{said}");
        assert_eq!(parse("Pages free: 1."), None, "no page size, no reading");
    }

    #[test]
    fn ps_reads_as_processes_and_memory_sums_by_command() {
        let text = "    1  21024 /sbin/launchd\n 9001 204800 /Users/runner/work/t/debug/slopty-worker\n 9002 102400 /Users/runner/work/t/debug/slopty-worker\n 9003 51200 /Applications/A B.app/Contents/MacOS/A B\n garbage\n";
        let all = processes(text);
        assert_eq!(all.len(), 4, "{all:?}");
        assert_eq!(all[3], Process { pid: 9003, rss: 51_200, name: "A B".to_owned() });
        assert_eq!(
            by_command(&all),
            [("slopty-worker".to_owned(), 300), ("A B".to_owned(), 50), ("launchd".to_owned(), 20)]
        );
        assert!(ours("slopty-ptyd") && ours("slopty-stub-claude") && !ours("launchd"));
    }
}
