//! `xtask upstream adopt zeron`: take zeron's interface changes into Slopty's copy of it.
//!
//! Slopty's interface was copied from zeron (MIT) and renamed as Slopty's own: zeron is neither a
//! fork nor a dependency. `xtask/zeron.toml` says where each zeron path went (`[paths]`, the
//! longest prefix wins), the text rewrites that turn a zeron file into ours (`[[rename]]`, in
//! order), and what was deliberately not taken (`[skip]`). Applying the rewrites to a zeron file at
//! `base` gives our file apart from the edits marked `// slopty:`, so taking zeron from `base` to a
//! newer commit is a three-way merge into our file of `base` and the new commit, both rewritten.
//!
//! A file zeron added under a mapped path is written, one it deleted is deleted when ours has no
//! edits of our own, and one zeron changed is merged with `git merge-file`, which leaves conflict
//! markers for a person or a lane to settle. Skipped and unmapped paths are listed, never taken.
//! `base` in `xtask/upstream.toml` moves only when every file merged cleanly.

use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use xshell::{Shell, cmd};

use super::{Head, commit_date, ensure_checkout, fetch, short, write_base};

/// zeron's entry in `xtask/upstream.toml`.
#[derive(Debug, Deserialize)]
pub(super) struct Copied {
    upstream: String,
    upstream_branch: String,
    /// The zeron clone, relative to the main checkout.
    checkout: Utf8PathBuf,
    /// The map, relative to this repository's root.
    map: Utf8PathBuf,
    /// The zeron commit our copy stands on.
    base: String,
}

impl Copied {
    /// What `upstream watch` follows: the repository, its branch and the commit last taken.
    pub(super) fn watched(&self) -> (&str, &str, &str) {
        (&self.upstream, &self.upstream_branch, &self.base)
    }
}

/// `xtask/zeron.toml`.
#[derive(Debug, Default, Deserialize)]
struct Map {
    /// The first line of every Rust file taken; `{base}` is the short commit it was taken at.
    #[serde(default)]
    provenance: Option<String>,
    /// zeron path prefix → our path prefix.
    #[serde(default)]
    paths: BTreeMap<String, String>,
    /// Text rewrites, applied in order.
    #[serde(default)]
    rename: Vec<Rename>,
    #[serde(default)]
    skip: Skip,
}

#[derive(Debug, Deserialize)]
struct Rename {
    from: String,
    to: String,
}

#[derive(Debug, Default, Deserialize)]
struct Skip {
    /// zeron path prefixes deliberately not taken.
    #[serde(default)]
    paths: Vec<String>,
}

/// Whether `path` is `prefix` or lies under it.
fn under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    path == prefix || path.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))
}

impl Map {
    fn load(path: &Utf8Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        toml::from_str(&text).with_context(|| format!("parsing {path}"))
    }

    /// Whether `path` is not taken: a skipped prefix covers it and no longer mapped one does.
    fn skipped(&self, path: &str) -> bool {
        let longest = |prefixes: &mut dyn Iterator<Item = &String>| {
            prefixes.filter(|p| under(path, p)).map(|p| p.trim_end_matches('/').len()).max()
        };
        let skip = longest(&mut self.skip.paths.iter());
        let take = longest(&mut self.paths.keys());
        skip.is_some_and(|skip| take.is_none_or(|take| take <= skip))
    }

    /// Where zeron's `path` lives in this repository, by the longest mapped prefix.
    fn target(&self, path: &str) -> Option<String> {
        let (from, to) = self
            .paths
            .iter()
            .filter(|(from, _)| under(path, from))
            .max_by_key(|(from, _)| from.trim_end_matches('/').len())?;
        let rest = path.strip_prefix(from.trim_end_matches('/')).unwrap_or_default();
        Some(format!("{}{rest}", to.trim_end_matches('/')))
    }

    /// zeron's text as ours: every rewrite, in order.
    fn renamed(&self, text: &str) -> String {
        self.rename.iter().fold(text.to_owned(), |text, r| text.replace(&r.from, &r.to))
    }

    /// The provenance line for a Rust file taken at `commit`, if the map gives one.
    fn provenance_line(&self, path: &str, commit: &str) -> Option<String> {
        let line = self.provenance.as_ref()?;
        Utf8Path::new(path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
            .then(|| line.replace("{base}", commit.get(..7).unwrap_or(commit)))
    }

    /// `bytes` without a provenance line on top, so files taken at different commits compare and
    /// merge by their bodies.
    fn without_provenance(&self, bytes: &[u8]) -> Vec<u8> {
        let Some(line) = &self.provenance else { return bytes.to_vec() };
        let head = line.split("{base}").next().unwrap_or_default().as_bytes();
        if head.is_empty() || !bytes.starts_with(head) {
            return bytes.to_vec();
        }
        bytes
            .iter()
            .position(|&b| b == b'\n')
            .and_then(|end| bytes.get(end.saturating_add(1)..))
            .map_or_else(Vec::new, <[u8]>::to_vec)
    }

    /// `body` with the provenance line for a file taken at `commit` on top.
    fn with_provenance(&self, path: &str, commit: &str, body: Vec<u8>) -> Vec<u8> {
        match self.provenance_line(path, commit) {
            Some(line) => [line.into_bytes(), b"\n".to_vec(), body].concat(),
            None => body,
        }
    }

    /// zeron's bytes as ours: text is rewritten, anything else (fonts, pictures) is taken as is.
    fn renamed_bytes(&self, bytes: Vec<u8>) -> Vec<u8> {
        match String::from_utf8(bytes) {
            Ok(text) => self.renamed(&text).into_bytes(),
            Err(e) => e.into_bytes(),
        }
    }
}

/// What happened to one zeron path.
#[derive(Debug, Default)]
struct Report {
    merged: Vec<String>,
    added: Vec<String>,
    deleted: Vec<String>,
    conflicts: Vec<String>,
    /// zeron deleted it, but ours has edits of our own: left for a person.
    kept: Vec<String>,
    skipped: Vec<String>,
    unmapped: Vec<String>,
}

/// One zeron file at a commit, or none where it does not exist there.
fn blob(sh: &Shell, rev: &str, path: &str) -> Option<Vec<u8>> {
    let spec = format!("{rev}:{path}");
    cmd!(sh, "git cat-file blob {spec}")
        .quiet()
        .ignore_stderr()
        .output()
        .ok()
        .and_then(|out| out.status.success().then_some(out.stdout))
}

/// The zeron commits to take between: `from` (default `base`) and `to` (default the branch head).
pub(super) struct Range<'a> {
    pub(super) from: Option<&'a str>,
    pub(super) to: Option<&'a str>,
}

/// Take zeron from `copied.base` (or `range.from`) to `range.to` (default: its branch's head) into
/// this repository. `base` moves only when the range starts at it.
pub(super) fn adopt(
    sh: &Shell,
    root: &Utf8Path,
    main: &Utf8Path,
    copied: &Copied,
    range: Range<'_>,
    dry_run: bool,
) -> Result<()> {
    let to = range.to;
    let map = Map::load(&root.join(&copied.map))?;
    let dir = ensure_checkout(sh, main, &copied.checkout, &copied.upstream, None)?;
    let head = {
        let _in = sh.push_dir(&dir);
        let fetched = fetch(sh, &copied.upstream, &copied.upstream_branch)?;
        match to {
            Some(rev) => {
                let spec = format!("{rev}^{{commit}}");
                let sha = cmd!(sh, "git rev-parse {spec}").read()?;
                let date = commit_date(sh, &sha)?;
                Head { sha, date }
            }
            None => fetched,
        }
    };
    let from = {
        let _in = sh.push_dir(&dir);
        match range.from {
            Some(rev) => {
                let spec = format!("{rev}^{{commit}}");
                cmd!(sh, "git rev-parse {spec}").read()?
            }
            None => copied.base.clone(),
        }
    };
    let base = from.as_str();
    let new = head.sha.as_str();
    if base == new {
        println!("zeron: at {}, nothing to take", short(base));
        return Ok(());
    }
    let report = {
        let _in = sh.push_dir(&dir);
        let log = cmd!(sh, "git log --oneline --no-decorate {base}..{new}").read()?;
        println!("zeron: {} → {} ({} commits)", short(base), short(new), log.lines().count());
        let changed = cmd!(sh, "git diff --name-status --no-renames {base} {new}").read()?;
        take(sh, root, &map, (base, new), &changed, dry_run)?
    };
    print_report(&report);
    if dry_run {
        println!("dry run: nothing written");
    } else if base != copied.base {
        println!("zeron: taken from {}, not from base; base left as it is", short(base));
    } else if report.conflicts.is_empty() && report.kept.is_empty() {
        let today = super::civil_from_days(super::today_days()?);
        write_base(root, "zeron", &head, &today)?;
        println!("zeron: base moved to {} in xtask/upstream.toml", short(new));
    } else {
        println!(
            "zeron: base left at {}; settle the conflicts and kept files above, then move `base` \
             in xtask/upstream.toml to {new}",
            short(base)
        );
    }
    Ok(())
}

/// Apply each changed zeron path, `git diff --name-status` lines in `changed`, to `root`.
fn take(
    sh: &Shell,
    root: &Utf8Path,
    map: &Map,
    (base, new): (&str, &str),
    changed: &str,
    dry_run: bool,
) -> Result<Report> {
    let mut report = Report::default();
    for line in changed.lines() {
        let Some((status, path)) = line.split_once('\t') else { continue };
        if map.skipped(path) {
            report.skipped.push(path.to_owned());
            continue;
        }
        let Some(ours) = map.target(path) else {
            report.unmapped.push(path.to_owned());
            continue;
        };
        let ours_path = root.join(&ours);
        match status.chars().next() {
            Some('A') => {
                let bytes = blob(sh, new, path).with_context(|| format!("{path} at {new}"))?;
                if ours_path.exists() {
                    report.conflicts.push(format!("{ours} (zeron added it; ours already exists)"));
                } else {
                    if !dry_run {
                        let body = map.renamed_bytes(bytes);
                        write(&ours_path, &map.with_provenance(&ours, new, body))?;
                    }
                    report.added.push(ours);
                }
            }
            Some('D') => {
                let at_base = blob(sh, base, path).map(|b| map.renamed_bytes(b));
                match std::fs::read(&ours_path) {
                    Ok(current) if Some(map.without_provenance(&current)) == at_base => {
                        if !dry_run {
                            std::fs::remove_file(&ours_path)
                                .with_context(|| format!("deleting {ours_path}"))?;
                        }
                        report.deleted.push(ours);
                    }
                    Ok(_) => report.kept.push(ours),
                    Err(_) => {}
                }
            }
            Some('M' | 'T') => {
                let Ok(current) = std::fs::read(&ours_path) else {
                    // Ours no longer has it: a cut made after the copy. Say so once.
                    report.kept.push(format!("{ours} (missing here; zeron changed it)"));
                    continue;
                };
                let old = blob(sh, base, path).map(|b| map.renamed_bytes(b)).unwrap_or_default();
                let theirs = blob(sh, new, path).map(|b| map.renamed_bytes(b)).unwrap_or_default();
                let body = map.without_provenance(&current);
                match merge(sh, &body, &old, &theirs)? {
                    Merged::Clean(bytes) => {
                        let bytes = map.with_provenance(&ours, new, bytes);
                        if bytes != current {
                            if !dry_run {
                                write(&ours_path, &bytes)?;
                            }
                            report.merged.push(ours);
                        }
                    }
                    Merged::Conflicts(bytes, count) => {
                        let bytes = map.with_provenance(&ours, new, bytes);
                        if !dry_run {
                            write(&ours_path, &bytes)?;
                        }
                        report.conflicts.push(format!("{ours} ({count} conflicts)"));
                    }
                }
            }
            _ => report.unmapped.push(format!("{path} (status {status})")),
        }
    }
    Ok(report)
}

fn write(path: &Utf8Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent}"))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("writing {path}"))
}

enum Merged {
    Clean(Vec<u8>),
    Conflicts(Vec<u8>, i32),
}

/// `git merge-file` of `theirs` into `ours` over `base`, all in memory.
fn merge(sh: &Shell, ours: &[u8], base: &[u8], theirs: &[u8]) -> Result<Merged> {
    if ours == base {
        return Ok(Merged::Clean(theirs.to_vec()));
    }
    if theirs == base || ours == theirs {
        return Ok(Merged::Clean(ours.to_vec()));
    }
    let tmp = std::env::temp_dir().join(format!("slopty-adopt-{}", std::process::id()));
    let tmp = Utf8PathBuf::from_path_buf(tmp)
        .map_err(|p| anyhow::anyhow!("a non-UTF-8 temporary path {}", p.display()))?;
    std::fs::create_dir_all(&tmp).with_context(|| format!("creating {tmp}"))?;
    let (o, b, t) = (tmp.join("ours"), tmp.join("base"), tmp.join("zeron"));
    std::fs::write(&o, ours)?;
    std::fs::write(&b, base)?;
    std::fs::write(&t, theirs)?;
    let out = cmd!(sh, "git merge-file -p -L ours -L base -L zeron {o} {b} {t}")
        .quiet()
        .ignore_status()
        .output();
    let _gone = std::fs::remove_dir_all(&tmp);
    let out = out?;
    match out.status.code() {
        Some(0) => Ok(Merged::Clean(out.stdout)),
        Some(n) if n > 0 => Ok(Merged::Conflicts(out.stdout, n)),
        _ => bail!("git merge-file failed: {}", String::from_utf8_lossy(&out.stderr)),
    }
}

fn print_report(report: &Report) {
    let sections: [(&str, &Vec<String>); 7] = [
        ("merged", &report.merged),
        ("added", &report.added),
        ("deleted", &report.deleted),
        ("CONFLICTS", &report.conflicts),
        ("kept (zeron deleted or changed it; ours has its own edits)", &report.kept),
        ("skipped (not taken, xtask/zeron.toml [skip])", &report.skipped),
        ("unmapped (no [paths] entry: map or skip it)", &report.unmapped),
    ];
    for (name, paths) in sections {
        if paths.is_empty() {
            continue;
        }
        println!("  {name}: {}", paths.len());
        for path in paths.iter().take(40) {
            println!("    {path}");
        }
        if paths.len() > 40 {
            println!("    …");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Map, Merged, Rename, Skip, merge, under};

    fn map() -> Map {
        Map {
            provenance: Some("// Derived from zeron @ {base}".to_owned()),
            paths: [
                ("crates/ui/src".to_owned(), "crates/slopty-shell/src".to_owned()),
                ("crates/ui".to_owned(), "crates/slopty-shell".to_owned()),
                (
                    "crates/engine/src/registry.rs".to_owned(),
                    "crates/slopty-shell/src/catalog/registry.rs".to_owned(),
                ),
            ]
            .into_iter()
            .collect(),
            rename: vec![
                Rename { from: "zeron_ui".to_owned(), to: "slopty_shell".to_owned() },
                Rename { from: "Zeron".to_owned(), to: "Slopty".to_owned() },
            ],
            skip: Skip {
                paths: vec![
                    "crates/ui/src/voice".to_owned(),
                    "crates/orb/".to_owned(),
                    "crates/engine/".to_owned(),
                ],
            },
        }
    }

    #[test]
    fn paths_map_by_their_longest_prefix_and_skips_win() {
        let map = map();
        assert_eq!(
            map.target("crates/ui/src/shell.rs").as_deref(),
            Some("crates/slopty-shell/src/shell.rs")
        );
        assert_eq!(
            map.target("crates/ui/Cargo.toml").as_deref(),
            Some("crates/slopty-shell/Cargo.toml")
        );
        assert_eq!(map.target("crates/uix/a.rs"), None, "a prefix is a whole path segment");
        assert!(map.skipped("crates/ui/src/voice/stage.rs"));
        assert!(map.skipped("crates/orb/src/lib.rs"));
        assert!(!map.skipped("crates/ui/src/voiceover.rs"));
        assert!(under("a/b", "a/b/") && !under("a/bc", "a/b"));
        assert!(map.skipped("crates/engine/src/lib.rs"));
        assert!(!map.skipped("crates/engine/src/registry.rs"), "a longer mapped path is taken");
    }

    #[test]
    fn provenance_is_set_aside_for_the_merge_and_written_for_the_new_commit() {
        let map = map();
        let ours = b"// Derived from zeron @ 053ce2e\nfn a() {}\n";
        assert_eq!(map.without_provenance(ours), b"fn a() {}\n");
        assert_eq!(map.without_provenance(b"fn a() {}\n"), b"fn a() {}\n");
        assert_eq!(
            map.with_provenance("x.rs", "0123456789abcdef", b"fn a() {}\n".to_vec()),
            b"// Derived from zeron @ 0123456\nfn a() {}\n"
        );
        assert_eq!(map.with_provenance("x.svg", "0123456789", b"<svg/>".to_vec()), b"<svg/>");
    }

    #[test]
    fn text_is_renamed_in_order_and_binaries_pass_through() {
        let map = map();
        assert_eq!(
            map.renamed("use zeron_ui::Shell; // Zeron"),
            "use slopty_shell::Shell; // Slopty"
        );
        let binary = vec![0xff, 0xfe, 0x00, b'Z'];
        assert_eq!(map.renamed_bytes(binary.clone()), binary);
    }

    #[test]
    fn a_merge_keeps_our_marked_edit_and_takes_zerons_change() {
        let sh = xshell::Shell::new().expect("shell");
        let base = b"a\nb\nc\nd\ne\nf\n";
        let ours = b"a\nb // slopty: ours\nc\nd\ne\nf\n";
        let theirs = b"a\nb\nc\nd\ne\nf changed\n";
        match merge(&sh, ours, base, theirs).expect("merge") {
            Merged::Clean(out) => assert_eq!(out, b"a\nb // slopty: ours\nc\nd\ne\nf changed\n"),
            Merged::Conflicts(..) => panic!("disjoint edits conflicted"),
        }
        let clash = b"a\nb zeron\nc\nd\ne\nf\n";
        assert!(matches!(merge(&sh, ours, base, clash).expect("merge"), Merged::Conflicts(_, 1)));
    }
}
