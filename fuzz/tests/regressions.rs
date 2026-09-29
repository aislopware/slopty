//! Every input a fuzz run found (and every one kept for a fixed crash) replays here on an
//! ordinary build: `regressions/<target>/<file>` through the target of that name, under the
//! counting allocator the targets bound a decode's heap with.

/// As in `fuzz_targets/`: a decode that takes more heap than its bytes allow fails here too.
#[global_allocator]
static ALLOC: slopty_testkit::alloc::Counting = slopty_testkit::alloc::Counting;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};

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

    #[test]
    fn every_regression_input_replays_clean() {
        assert!(slopty_testkit::alloc::installed(), "the heap bound reads zeros without it");
        let dir = root().join("regressions");
        let mut failed: Vec<PathBuf> = Vec::new();
        let mut replayed = 0_usize;
        for target in names_in(&dir) {
            let run = slopty_fuzz::target(&target)
                .unwrap_or_else(|| panic!("regressions/{target} names no fuzz target"));
            for file in names_in(&dir.join(&target)) {
                let path = dir.join(&target).join(file);
                let input = fs::read(&path).unwrap();
                replayed += 1;
                if catch_unwind(AssertUnwindSafe(|| run(&input))).is_err() {
                    failed.push(path);
                }
            }
        }
        assert!(failed.is_empty(), "{} of {replayed} inputs still fail: {failed:#?}", failed.len());
    }

    #[test]
    fn the_seeds_run_through_every_target() {
        // The shapes a target meets first: empty, one byte, and an input past every length check.
        for (name, run) in slopty_fuzz::TARGETS {
            for input in [&[][..], &[0], &[0xff; 64], &[1; 4096]] {
                assert!(catch_unwind(|| run(input)).is_ok(), "{name} on {} bytes", input.len());
            }
        }
    }
}
