//! Search in files on real files in a temporary directory: the context round each match, the
//! globs, and a search stopped.

#[cfg(test)]
mod search {
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use slopty_proto::search::{FileHits, MAX_LINES, SearchQuery};
    use slopty_worker::search::collect;

    fn query(pattern: &str) -> SearchQuery {
        SearchQuery { pattern: pattern.to_owned(), ..SearchQuery::default() }
    }

    /// A deadline no search in these tests comes near.
    const FOREVER: Duration = Duration::from_secs(600);

    fn found(root: &Path, query: &SearchQuery) -> Vec<FileHits> {
        collect(root, query, MAX_LINES, &AtomicBool::new(false), FOREVER).unwrap().0
    }

    /// Context comes from the lines round each match, each line once: two matches close
    /// together share theirs, a matching line is never context, and a line cut at the cap
    /// takes its context with it.
    #[test]
    fn context_runs_round_matches_once_and_goes_with_a_capped_line() {
        let dir = tempfile::tempdir().unwrap();
        let text = "a\nb\nhit 1\nc\nhit 2\nd\ne\nf\ng\nhit 3\n\tindented\n";
        fs::write(dir.path().join("f.txt"), text).unwrap();
        let q = SearchQuery { context: 1, ..query("hit") };
        let files = found(dir.path(), &q);
        let [f] = files.as_slice() else { panic!("{files:#?}") };
        let lines: Vec<u32> = f.lines.iter().map(|l| l.line).collect();
        assert_eq!(lines, [3, 5, 10]);
        let context: Vec<(u32, &str)> =
            f.context.iter().map(|c| (c.line, c.text.as_str())).collect();
        assert_eq!(
            context,
            [(2, "b"), (4, "c"), (6, "d"), (9, "g"), (11, "indented")],
            "line 4 is shared, once; the indentation is dropped"
        );

        let wide = SearchQuery { context: 99, ..query("hit") };
        let lines = found(dir.path(), &wide).remove(0).context.len();
        assert_eq!(lines, 8, "capped at five either side: every other line of the file");

        let (files, summary) =
            collect(dir.path(), &q, 2, &AtomicBool::new(false), FOREVER).unwrap();
        assert!(summary.capped);
        let f = files.first().unwrap();
        assert_eq!(f.lines.iter().map(|l| l.line).collect::<Vec<_>>(), [3, 5]);
        let last = f.context.iter().map(|c| c.line).max();
        assert_eq!(last, Some(6), "the third match's context went with it");
    }

    /// A glob narrows what `.gitignore` lets through and never brings an ignored file back,
    /// as a search panel's "files to include" does (`rg -g` would).
    #[test]
    fn a_glob_narrows_within_the_ignore_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join(".gitignore"), "gen.rs\n").unwrap();
        for name in ["gen.rs", "src/a.rs", "src/b.md"] {
            fs::write(dir.path().join(name), "needle\n").unwrap();
        }
        let q = SearchQuery { globs: vec!["*.rs".to_owned()], ..query("needle") };
        let paths: Vec<String> = found(dir.path(), &q).into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["src/a.rs"]);
    }

    /// A collected search stops when its flag is set or its time is up, and answers with what
    /// it found by then, capped.
    #[test]
    fn a_collected_search_stops_when_told_or_late() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..50 {
            fs::write(dir.path().join(format!("f{n}.txt")), "needle\n").unwrap();
        }
        let q = query("needle");
        let (files, summary) =
            collect(dir.path(), &q, MAX_LINES, &AtomicBool::new(true), FOREVER).unwrap();
        assert!(files.is_empty() && summary.searched == 0, "{summary:?}");

        let (files, summary) =
            collect(dir.path(), &q, MAX_LINES, &AtomicBool::new(false), Duration::ZERO).unwrap();
        assert!(files.is_empty() && summary.searched == 0, "{summary:?}");
        assert!(summary.capped, "a search cut short says so: {summary:?}");

        let (files, summary) =
            collect(dir.path(), &q, MAX_LINES, &AtomicBool::new(false), FOREVER).unwrap();
        assert_eq!((files.len(), summary.capped), (50, false));
    }
}
