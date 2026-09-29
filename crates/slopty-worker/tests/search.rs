//! Search in files on real files in a temporary directory: the context round each match, and a
//! replace made from what a search found, down to the bytes on disk.

#[cfg(test)]
mod search {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use slopty_proto::search::{
        FileHits, FileReplace, MAX_LINES, MatchAt, Replace, SearchQuery, SkipReason,
    };
    use slopty_worker::search::{collect, replace};

    fn query(pattern: &str) -> SearchQuery {
        SearchQuery { pattern: pattern.to_owned(), ..SearchQuery::default() }
    }

    /// A deadline no search in these tests comes near.
    const FOREVER: Duration = Duration::from_secs(600);

    fn found(root: &Path, query: &SearchQuery) -> Vec<FileHits> {
        collect(root, query, MAX_LINES, &AtomicBool::new(false), FOREVER).unwrap().0
    }

    /// A replace of every match `query` found under `root`, with `with`.
    fn replace_all(query: &SearchQuery, files: &[FileHits], with: &str) -> Replace {
        let files = files
            .iter()
            .map(|f| FileReplace {
                path: f.path.clone(),
                stamp: f.stamp,
                matches: f
                    .lines
                    .iter()
                    .flat_map(|l| {
                        (0..l.spans.len()).map(|index| MatchAt {
                            line: l.line,
                            index: u32::try_from(index).unwrap(),
                        })
                    })
                    .collect(),
            })
            .collect();
        Replace { id: 1, root: String::new(), query: query.clone(), with: with.to_owned(), files }
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

    /// A replace rewrites exactly the matches named: every one in a file, or one on a line
    /// holding two. The rest of the file is kept to the byte, CRLF endings and the missing
    /// final newline too, and so are its permissions. The answer counts the matches and gives
    /// the new stamp, which a second replace in the same file then goes by.
    #[test]
    fn a_replace_rewrites_the_named_matches_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.sh");
        fs::write(&script, "echo old\r\nold and old\r\nkeep\r\nold").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o751)).unwrap();
        let q = query("old");
        let files = found(dir.path(), &q);

        let mut one = replace_all(&q, &files, "new");
        one.files[0].matches = vec![MatchAt { line: 2, index: 1 }];
        let (done, skipped) = replace(dir.path(), &one).unwrap();
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(done[0].matches, 1);
        assert_ne!(done[0].stamp, files[0].stamp, "a write moves the stamp");
        let text = fs::read_to_string(&script).unwrap();
        assert_eq!(text, "echo old\r\nold and new\r\nkeep\r\nold", "the second match alone");

        let mut rest = replace_all(&q, &found(dir.path(), &q), "new");
        rest.files[0].stamp = done[0].stamp;
        let (done, _) = replace(dir.path(), &rest).unwrap();
        assert_eq!(done[0].matches, 3);
        assert_eq!(fs::read_to_string(&script).unwrap(), "echo new\r\nnew and new\r\nkeep\r\nnew");
        let mode = fs::metadata(&script).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o751, "still executable");
        let left: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 1, "no temporary file left beside it");
    }

    /// A file written since it was searched is left alone and said so, while the files beside
    /// it are replaced; so is one gone since. A name that climbs out of the root is refused.
    #[test]
    fn a_file_changed_on_disk_since_the_search_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(dir.path().join(name), "needle\n").unwrap();
        }
        let q = query("needle");
        let files = found(dir.path(), &q);
        assert_eq!(files.len(), 3);
        fs::write(dir.path().join("b.txt"), "needle moved\n").unwrap();
        fs::remove_file(dir.path().join("c.txt")).unwrap();
        let mut request = replace_all(&q, &files, "thread");
        let mut outside = request.files[0].clone();
        outside.path = "../a.txt".to_owned();
        request.files.push(outside);

        let (done, skipped) = replace(dir.path(), &request).unwrap();
        assert_eq!(done.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt"]);
        let why: Vec<(&str, &SkipReason)> =
            skipped.iter().map(|s| (s.path.as_str(), &s.why)).collect();
        assert_eq!(why[0], ("b.txt", &SkipReason::Changed));
        assert_eq!(why[1], ("c.txt", &SkipReason::Changed));
        assert!(matches!(why[2], ("../a.txt", SkipReason::Failed(_))), "{why:?}");
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "thread\n");
        assert_eq!(fs::read_to_string(dir.path().join("b.txt")).unwrap(), "needle moved\n");
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

    /// With a regular expression, `$1` and `${name}` are the match's groups and `$$` a dollar
    /// sign, whole words and either case included; a literal query's `$1` is text.
    #[test]
    fn a_regex_replace_expands_its_groups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        fs::write(&path, "let x = Foo(1) + foo(22) + foobar(3);\n").unwrap();
        let q = SearchQuery { regex: true, whole_word: true, ..query(r"(?P<name>foo)\((\d+)\)") };
        let files = found(dir.path(), &q);
        assert_eq!(files[0].lines[0].spans.len(), 2, "foobar is not a whole word");
        let (done, _) = replace(dir.path(), &replace_all(&q, &files, "${name}_$2[$$]")).unwrap();
        assert_eq!(done[0].matches, 2);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, "let x = Foo_1[$] + foo_22[$] + foobar(3);\n");

        let literal = query("foobar(3)");
        let files = found(dir.path(), &literal);
        replace(dir.path(), &replace_all(&literal, &files, "$1")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "let x = Foo_1[$] + foo_22[$] + $1;\n");

        let bad = Replace {
            query: SearchQuery { regex: true, ..query("(") },
            ..replace_all(&q, &[], "")
        };
        assert!(replace(dir.path(), &bad).is_err(), "a query that does not parse");
    }
    /// On a line too long to show whole, a replace of the match the client picked rewrites that
    /// match, byte for byte, however many matches sit in the indentation before it: the shown
    /// line keeps them, since a match's index counts every match from the line's start.
    #[test]
    fn a_replace_on_a_long_indented_line_rewrites_the_match_shown() {
        for (indent, pattern) in [("        ", " "), ("\t\t\t\t", "\t")] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("wide.txt");
            let words = format!("word{pattern}").repeat(60);
            let text = format!("first\n{indent}{words}\nlast\n");
            fs::write(&path, &text).unwrap();
            let q = query(pattern);
            let files = found(dir.path(), &q);
            let hit = files[0].lines.iter().find(|l| l.line == 2).unwrap();
            assert!(hit.cut_after, "the line is cut: {hit:?}");
            // The first match after the indentation, as the client finds it in the text shown.
            let index = hit
                .spans
                .iter()
                .position(|s| {
                    let before = hit.text.get(..usize::try_from(s.start).unwrap());
                    before.is_some_and(|text| text.ends_with("word"))
                })
                .unwrap();
            let mut one = replace_all(&q, &files, "_");
            one.files[0].matches = vec![MatchAt { line: 2, index: u32::try_from(index).unwrap() }];
            let (done, skipped) = replace(dir.path(), &one).unwrap();
            assert!(skipped.is_empty(), "{skipped:?}");
            assert_eq!(done[0].matches, 1);
            let want = text.replacen(&format!("word{pattern}"), "word_", 1);
            assert_eq!(fs::read_to_string(&path).unwrap(), want, "{pattern:?}");
        }
    }

    /// A replace never writes through a link below the search's folder: not a directory
    /// swapped for a link to a folder outside holding a file of the same name and stamp, nor a
    /// file swapped for a link. Both are refused and the files outside keep their bytes.
    #[test]
    fn a_link_swapped_in_after_the_search_is_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/a.txt"), "needle\n").unwrap();
        fs::write(root.join("b.txt"), "needle\n").unwrap();
        let q = query("needle");
        let files = found(root, &q);
        assert_eq!(files.len(), 2);

        // The same bytes and modification time outside, so a stamp alone cannot tell them apart.
        for (inside, name) in [("sub/a.txt", "a.txt"), ("b.txt", "b.txt")] {
            let modified = fs::metadata(root.join(inside)).unwrap().modified().unwrap();
            fs::write(outside.path().join(name), "needle\n").unwrap();
            let copy = fs::File::options().write(true).open(outside.path().join(name)).unwrap();
            copy.set_modified(modified).unwrap();
        }
        fs::remove_dir_all(root.join("sub")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("sub")).unwrap();
        fs::remove_file(root.join("b.txt")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("b.txt"), root.join("b.txt")).unwrap();

        let (done, skipped) = replace(root, &replace_all(&q, &files, "thread")).unwrap();
        assert!(done.is_empty(), "{done:?}");
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        for s in &skipped {
            assert_eq!(s.why, SkipReason::Failed("Reached through a link".to_owned()), "{s:?}");
        }
        for name in ["a.txt", "b.txt"] {
            assert_eq!(fs::read_to_string(outside.path().join(name)).unwrap(), "needle\n");
        }
        let left: Vec<_> = fs::read_dir(outside.path()).unwrap().flatten().collect();
        assert_eq!(left.len(), 2, "no temporary file left outside");
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
