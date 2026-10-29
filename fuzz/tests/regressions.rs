//! Every input a fuzz run found (and every one kept for a fixed crash) replays here on an
//! ordinary build, in the gate: `regressions/<target>/<file>` and the committed corpus
//! `corpus/<target>/<file>` through the target of that name, under the counting allocator the
//! targets bound a decode's heap with. The inputs are shared out over every core, the largest
//! first, so the slowest target's do not keep one thread busy after the rest are done.

/// As in `fuzz_targets/`: a decode that takes more heap than its bytes allow fails here too.
#[global_allocator]
static ALLOC: slopty_testkit::alloc::Counting = slopty_testkit::alloc::Counting;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::num::NonZeroUsize;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn every_target_has_its_entry_point() {
        let mut files: Vec<String> = names_in(&root().join("fuzz_targets"))
            .into_iter()
            .filter_map(|n| n.strip_suffix(".rs").map(str::to_owned))
            .collect();
        files.sort();
        let mut targets: Vec<String> =
            slopty_fuzz::TARGETS.iter().map(|(n, _)| (*n).to_owned()).collect();
        targets.sort();
        assert_eq!(files, targets, "fuzz_targets/*.rs and slopty_fuzz::TARGETS differ");
        let manifest = fs::read_to_string(root().join("Cargo.toml")).unwrap();
        for name in &targets {
            assert!(
                manifest.contains(&format!("path = \"fuzz_targets/{name}.rs\"")),
                "Cargo.toml has no [[bin]] for {name}"
            );
        }
    }

    /// The most bytes one target's committed corpus may hold: what `cargo xtask fuzz --cmin`
    /// spends at most, on the fewest bytes that reach every edge the fuzzers reached.
    const CORPUS_BYTES: u64 = 2 << 20;

    /// Every job through `run` on as many threads as there are cores, each thread taking the
    /// next job not yet taken; the jobs whose run panicked.
    fn failing<J: Sync>(jobs: &[J], run: impl Fn(&J) + Sync) -> Vec<&J> {
        let next = AtomicUsize::new(0);
        let threads = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        std::thread::scope(|scope| {
            // Every thread starts before the first is waited for.
            let mut workers = Vec::with_capacity(threads);
            for _ in 0..threads {
                workers.push(scope.spawn(|| {
                    let mut failed = Vec::new();
                    while let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if catch_unwind(AssertUnwindSafe(|| run(job))).is_err() {
                            failed.push(job);
                        }
                    }
                    failed
                }));
            }
            workers.into_iter().flat_map(|worker| worker.join().unwrap()).collect()
        })
    }

    /// One kept input, and the target it goes through.
    struct Input {
        run: fn(&[u8]),
        path: PathBuf,
        bytes: Vec<u8>,
    }

    /// Every input under `dir/<target>/` through its target, the largest first; the inputs that
    /// fail, and how many replayed.
    fn replay(dir: &Path) -> (Vec<PathBuf>, usize) {
        let mut inputs = Vec::new();
        for target in names_in(dir) {
            let run = slopty_fuzz::target(&target)
                .unwrap_or_else(|| panic!("{} names no fuzz target", dir.join(&target).display()));
            for file in names_in(&dir.join(&target)) {
                let path = dir.join(&target).join(file);
                let bytes = fs::read(&path).unwrap();
                inputs.push(Input { run, path, bytes });
            }
        }
        inputs.sort_by_key(|input| std::cmp::Reverse(input.bytes.len()));
        let failed = failing(&inputs, |input| (input.run)(&input.bytes));
        (failed.into_iter().map(|input| input.path.clone()).collect(), inputs.len())
    }

    #[test]
    fn every_regression_input_replays_clean() {
        assert!(slopty_testkit::alloc::installed(), "the heap bound reads zeros without it");
        let (failed, replayed) = replay(&root().join("regressions"));
        assert!(failed.is_empty(), "{} of {replayed} inputs still fail: {failed:#?}", failed.len());
    }

    /// The committed corpora replay clean: what the fuzzers covered stays held at every landing.
    #[test]
    fn every_corpus_input_replays_clean() {
        assert!(slopty_testkit::alloc::installed(), "the heap bound reads zeros without it");
        let (failed, replayed) = replay(&root().join("corpus"));
        assert!(failed.is_empty(), "{} of {replayed} inputs fail: {failed:#?}", failed.len());
    }

    /// Every target has a committed corpus, and none outgrows [`CORPUS_BYTES`].
    #[test]
    fn every_target_keeps_a_small_corpus() {
        let dir = root().join("corpus");
        for (name, _) in slopty_fuzz::TARGETS {
            let files = names_in(&dir.join(name));
            assert!(
                !files.is_empty(),
                "corpus/{name} holds nothing: `cargo xtask fuzz --cmin {name}`"
            );
            let bytes: u64 =
                files.iter().map(|f| fs::metadata(dir.join(name).join(f)).unwrap().len()).sum();
            assert!(bytes <= CORPUS_BYTES, "corpus/{name} holds {bytes} bytes");
        }
    }

    /// The captures `cargo xtask fuzz` seeds the `terminal` target with hold its invariants.
    #[test]
    fn every_terminal_capture_runs_clean() {
        let dir = root().join("seeds/terminal");
        let captures = names_in(&dir);
        assert!(captures.len() >= 5, "{captures:?}");
        let mut all = Vec::new();
        for name in &captures {
            let capture = fs::read(dir.join(name)).unwrap();
            let script = slopty_fuzz::terminal::script(&capture);
            assert!(catch_unwind(|| slopty_fuzz::terminal::run(&script)).is_ok(), "{name}");
            all.extend(capture);
        }
        slopty_fuzz::terminal::run(&slopty_fuzz::terminal::script(&all));
    }

    #[test]
    fn the_seeds_run_through_every_target() {
        // The shapes a target meets first: empty, one byte, and an input past every length check.
        let shapes: [&[u8]; 4] = [&[], &[0], &[0xff; 64], &[1; 4096]];
        let runs: Vec<(slopty_fuzz::Target, &[u8])> = slopty_fuzz::TARGETS
            .iter()
            .flat_map(|&target| shapes.map(|input| (target, input)))
            .collect();
        let failed: Vec<String> = failing(&runs, |((_, run), input)| run(input))
            .into_iter()
            .map(|((name, _), input)| format!("{name} on {} bytes", input.len()))
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
    }
}
