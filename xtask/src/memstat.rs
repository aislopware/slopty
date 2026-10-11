//! How hard a step of a CI test lane pressed the macOS runner's memory: the VM's counters from
//! `vm_stat` before and after it, as the step's differences.
//!
//! A hosted Mac has three cores and 7 GB, no swap in use, and its test shards' times swing by
//! ±100 s. At a job's end the worker and rest shards had paged out 90 000–155 000 pages against
//! 3 000–8 000 for the UI shard, and decompressed 400 000–1 100 000 (gate runs 38144514113 to
//! 38151148784). Which step does it, the test build's links or the tests, decides what to change.

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

#[cfg(test)]
mod tests {
    use super::{Counters, line, parse};

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
}
