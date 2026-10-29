//! The zig libghostty-vt builds with: which one, where it is, and ghostty's patches to it.
//!
//! The pinned ghostty's `build.zig.zon` names it (`minimum_zig_version`), and is the one pin:
//! CI's jobs read it too (`.github/actions/zig`), so a ghostty bump moves every job's zig and
//! every check's here at once (`docs/decisions/tooling.md`, "zig is the one the pinned ghostty
//! names").

use anyhow::{Context as _, Result, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

/// The zig version the ghostty checkout `ghostty` builds with: its `build.zig.zon`'s
/// `minimum_zig_version`, which its `build.zig` holds every build to.
pub fn ghostty_zig(ghostty: &Utf8Path) -> Result<String> {
    let manifest = ghostty.join("build.zig.zon");
    let zon = std::fs::read_to_string(&manifest).with_context(|| format!("read {manifest}"))?;
    minimum_zig(&zon)
        .map(str::to_owned)
        .with_context(|| format!("{manifest} names no minimum_zig_version"))
}

/// The `minimum_zig_version` a `build.zig.zon` names.
fn minimum_zig(zon: &str) -> Option<&str> {
    zon.lines().find_map(|line| {
        let value = line.trim().strip_prefix(".minimum_zig_version")?.trim_start();
        let (version, _) =
            value.strip_prefix('=')?.trim_start().strip_prefix('"')?.split_once('"')?;
        Some(version)
    })
}

/// A zig version's release series, `0.17` of `0.17.0`: zig's language and library change from
/// one series to the next, and a patch release only fixes bugs.
pub fn series(version: &str) -> &str {
    version.match_indices('.').nth(1).and_then(|(at, _)| version.get(..at)).unwrap_or(version)
}

/// A zig that builds a ghostty checkout.
#[derive(Debug, PartialEq, Eq)]
pub struct Zig {
    /// What `zig version` prints.
    pub version: String,
    /// The directory it is in, for `PATH`; `None` for the one on `PATH` already.
    pub dir: Option<Utf8PathBuf>,
}

impl Zig {
    /// The command that runs it.
    fn bin(&self) -> Utf8PathBuf {
        self.dir.as_ref().map_or_else(|| "zig".into(), |dir| dir.join("zig"))
    }
}

/// The zig that builds the ghostty checkout `ghostty`, or what to do to have one: the zig on
/// `PATH` when it is of the series ghostty names, else Homebrew's of that series, which
/// Homebrew keeps installed beside the one it links (`zig@0.16` beside `zig`). Two trees at two
/// ghostty pins, as two lanes' checks are across a bump, each build with their own.
pub fn zig_for(sh: &Shell, ghostty: &Utf8Path) -> Result<Zig, String> {
    let wanted = ghostty_zig(ghostty).map_err(|error| format!("{error:#}"))?;
    let series = series(&wanted);
    let prefix = std::env::var("HOMEBREW_PREFIX").unwrap_or_else(|_| "/opt/homebrew".to_owned());
    let kegs = [format!("zig@{series}"), "zig".to_owned()]
        .map(|formula| Utf8Path::new(&prefix).join("opt").join(formula).join("bin"));
    let candidates = [None, Some(kegs[0].as_path()), Some(kegs[1].as_path())];
    first_of_series(sh, series, &candidates).ok_or_else(|| {
        let on_path = version_of(sh, Utf8Path::new("zig"))
            .map_or_else(|| "no zig is on PATH".to_owned(), |version| format!("zig {version} is"));
        format!("{on_path}, and {ghostty} builds with zig {series}: {}", how(series))
    })
}

/// The first of `candidates` (directories, `None` for `PATH`'s) whose zig is of `series`.
fn first_of_series(sh: &Shell, series: &str, candidates: &[Option<&Utf8Path>]) -> Option<Zig> {
    candidates.iter().find_map(|dir| {
        let bin = dir.map_or_else(|| "zig".into(), |dir| dir.join("zig"));
        let version = version_of(sh, &bin).filter(|version| self::series(version) == series)?;
        Some(Zig { version, dir: dir.map(Utf8Path::to_path_buf) })
    })
}

/// What zig `bin` says its version is, when it runs.
fn version_of(sh: &Shell, bin: &Utf8Path) -> Option<String> {
    let version = cmd!(sh, "{bin} version").quiet().ignore_stderr().read().ok()?;
    Some(version.trim().to_owned())
}

/// Homebrew's way to zig `series`: linked for the builds outside a check, and pinned, since an
/// unrelated `brew upgrade` moved 0.16 to 0.17 under ghostty's `build.zig` once; then setup
/// puts ghostty's patches in.
fn how(series: &str) -> String {
    format!(
        "`brew install zig && brew link --overwrite zig && brew pin zig` (`zig@{series}` once \
         Homebrew's `zig` is past {series}), then `cargo xtask setup`"
    )
}

/// What is wrong with the zig on `PATH` for building the checkout's ghostty, if anything: the
/// builds outside a check (`cargo xtask linux`, a bundle) run the one there.
pub fn zig_problem(sh: &Shell) -> Option<String> {
    let ghostty = match super::repo_root() {
        Ok(root) => root.join("vendor/ghostty"),
        Err(error) => return Some(format!("{error:#}")),
    };
    match zig_for(sh, &ghostty) {
        Ok(Zig { dir: None, .. }) => None,
        Ok(Zig { version, dir: Some(dir) }) => {
            let formula = dir.parent().and_then(Utf8Path::file_name).unwrap_or("zig");
            Some(format!(
                "the zig on PATH is not the {version} {ghostty} builds with, which is \
                 Homebrew's `{formula}`: `brew link --overwrite --force {formula}`"
            ))
        }
        Err(problem) => Some(problem),
    }
}

/// Put the zig that builds `tree`'s ghostty first on `sh`'s `PATH` when it is not the one there
/// already. libghostty-vt-sys's build runs the `zig` on `PATH` and keys its prebuilt library by
/// that zig's version, and cargo-zigbuild runs it too. Without one `PATH` stays as it is: a
/// build that reuses its library needs no zig, and one that does not fails naming zig. A zig of
/// another series on `PATH` is noted, as its build would fail on ghostty's `build.zig`; a lane
/// with no zig at all (CI's tools lane) builds nothing that needs one.
pub fn use_zig_for(sh: &Shell, tree: &Utf8Path) -> Result<()> {
    match zig_for(sh, &tree.join("vendor/ghostty")) {
        Ok(Zig { dir: Some(dir), .. }) => {
            let path = sh.var_os("PATH").unwrap_or_default();
            let path = std::env::split_paths(&path);
            let path = std::env::join_paths(std::iter::once(dir.into_std_path_buf()).chain(path))
                .context("zig's directory on PATH")?;
            sh.set_var("PATH", path);
        }
        Ok(Zig { dir: None, .. }) => {}
        Err(problem) => {
            if version_of(sh, Utf8Path::new("zig")).is_some() {
                println!("  note: {problem}");
            }
        }
    }
    Ok(())
}

/// ghostty's patches to the standard library of zig `series`, which ghostty's own build puts
/// into that zig too (`flake.nix`): its `nix/patches/zig-<series>-*.patch`, in name order. A
/// ghostty with none has none.
pub fn zig_patches(ghostty: &Utf8Path, series: &str) -> Result<Vec<Utf8PathBuf>> {
    let dir = ghostty.join("nix/patches");
    let entries = match dir.read_dir_utf8() {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {dir}")),
    };
    let prefix = format!("zig-{series}-");
    let mut patches = Vec::new();
    for entry in entries {
        let path = entry.with_context(|| format!("read {dir}"))?.into_path();
        if path.file_name().is_some_and(|name| name.starts_with(&prefix))
            && path.extension() == Some("patch")
        {
            patches.push(path);
        }
    }
    patches.sort();
    Ok(patches)
}

/// Put ghostty's patches ([`zig_patches`]), from the ghostty checkout `ghostty`, into the
/// standard library of `zig`. A hunk already in is left as it is, so this runs on every setup;
/// a hunk whose lines are there in neither form is an error, as that zig is not the one the
/// patch was made for. Returns the files it rewrote.
///
/// zig 0.17.0 analyses `spawnDarwin` for iOS, whose `NullFile` has no `fd`, so without ghostty's
/// `zig-0.17-ios-process-replace.patch` (zig `0bfd34342b`, in no release yet) every iOS build of
/// libghostty-vt in `Debug` or `ReleaseSafe` fails; `ReleaseFast`, which Slopty's builds pin,
/// compiles that path out.
pub fn patch_zig(sh: &Shell, ghostty: &Utf8Path, zig: &Zig) -> Result<Vec<Utf8PathBuf>> {
    let patches = zig_patches(ghostty, series(&zig.version))?;
    if patches.is_empty() {
        return Ok(Vec::new());
    }
    let bin = zig.bin();
    let env = cmd!(sh, "{bin} env").quiet().read().with_context(|| format!("{bin} env"))?;
    let lib = crate::zig_cc::lib_dir(&env).context("`zig env` names no lib_dir")?;
    let mut rewritten = Vec::new();
    for path in patches {
        let diff = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
        for (file, hunks) in unified_diff(&diff).with_context(|| format!("read {path}"))? {
            // The patch names files in zig's source tree, whose `lib` is `zig env`'s `lib_dir`.
            let in_lib = file
                .strip_prefix("lib/")
                .with_context(|| format!("{path} changes {file}, outside zig's lib"))?;
            let target = Utf8Path::new(lib).join(in_lib);
            let before =
                std::fs::read_to_string(&target).with_context(|| format!("read {target}"))?;
            let version = &zig.version;
            let after = with_hunks(&before, &hunks)
                .with_context(|| format!("{path} on {target}: not made for zig {version}"))?;
            if after != before {
                // Renamed into place, so a build that reads the file meanwhile sees one whole,
                // with the file's own mode: Homebrew's are read-only.
                let mode = std::fs::metadata(&target)
                    .with_context(|| format!("read {target}"))?
                    .permissions();
                let staged = target.with_extension("zig.patching");
                std::fs::write(&staged, &after).with_context(|| format!("write {staged}"))?;
                std::fs::set_permissions(&staged, mode)
                    .with_context(|| format!("set the mode of {staged}"))?;
                std::fs::rename(&staged, &target).with_context(|| format!("replace {target}"))?;
                rewritten.push(target);
            }
        }
    }
    Ok(rewritten)
}

/// One file's hunks in a unified diff: the lines each replaces, and the lines it puts there.
type FileHunks = (String, Vec<(String, String)>);

/// The files a unified diff changes, by their new name (`+++ b/…`), each with its hunks. A hunk
/// is read by the line counts its `@@` header gives, so a header after it is never taken for one
/// of its lines.
fn unified_diff(diff: &str) -> Result<Vec<FileHunks>> {
    let mut files: Vec<FileHunks> = Vec::new();
    let mut lines = diff.lines();
    while let Some(line) = lines.next() {
        if let Some(name) = line.strip_prefix("+++ ") {
            files.push((name.strip_prefix("b/").unwrap_or(name).to_owned(), Vec::new()));
        } else if let Some(header) = line.strip_prefix("@@ -") {
            let (_, hunks) = files.last_mut().context("a hunk before its file's `+++` line")?;
            hunks.push(hunk(header, &mut lines).with_context(|| format!("the hunk at {line}"))?);
        }
    }
    Ok(files)
}

/// The hunk whose header, after its `@@ -`, is `header`, from the diff's `lines` after it.
fn hunk<'d>(header: &str, lines: &mut impl Iterator<Item = &'d str>) -> Result<(String, String)> {
    // `-12,7 +12,8 @@ …`: where each side starts, and how many lines it has (one when unsaid).
    let length = |range: &str| range.split_once(',').map_or(Some(1_usize), |(_, n)| n.parse().ok());
    let (old_range, rest) = header.split_once(" +").context("no `+` range")?;
    let (new_range, _) = rest.split_once(" @@").context("no closing `@@`")?;
    let (Some(mut old_left), Some(mut new_left)) = (length(old_range), length(new_range)) else {
        anyhow::bail!("a range that counts no lines");
    };
    let (mut old, mut new) = (String::new(), String::new());
    while old_left > 0 || new_left > 0 {
        let line = lines.next().context("the diff ends inside it")?;
        // An empty line is a context line whose text is empty; `\` notes a missing newline.
        let (mark, text) = line.split_at_checked(1).unwrap_or((" ", ""));
        let (in_old, in_new) = match mark {
            " " => (true, true),
            "-" => (true, false),
            "+" => (false, true),
            "\\" => (false, false),
            _ => anyhow::bail!("a line that is no context, removal or addition: {line}"),
        };
        if in_old {
            old_left = old_left.checked_sub(1).context("more lines than its range says")?;
            old.push_str(text);
            old.push('\n');
        }
        if in_new {
            new_left = new_left.checked_sub(1).context("more lines than its range says")?;
            new.push_str(text);
            new.push('\n');
        }
    }
    Ok((old, new))
}

/// `text` with every hunk in: one there once already is left, and one found once unpatched is
/// put in. Where one form holds the other (a hunk that only adds lines, or only removes them),
/// the shorter is inside the longer too, so only the longer one's count tells.
fn with_hunks(text: &str, hunks: &[(String, String)]) -> Result<String> {
    let mut text = text.to_owned();
    for (old, new) in hunks {
        let olds = text.matches(old.as_str()).count();
        let news = text.matches(new.as_str()).count();
        if news == 1 && (olds == 0 || new.contains(old.as_str())) {
            continue;
        }
        ensure!(
            olds == 1 && (news == 0 || old.contains(new.as_str())),
            "a hunk's lines are there neither as it finds them nor as it leaves them"
        );
        text = text.replacen(old.as_str(), new, 1);
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use camino::{Utf8Path, Utf8PathBuf};
    use xshell::Shell;

    use super::{
        Zig, first_of_series, ghostty_zig, minimum_zig, patch_zig, series, unified_diff,
        with_hunks, zig_patches,
    };
    use crate::tools::repo_root;

    /// A directory of its own for test `name`, empty.
    fn scratch(name: &str) -> Utf8PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-zig-{name}-{}", std::process::id()));
        let dir = Utf8PathBuf::from_path_buf(dir).expect("a UTF-8 temp dir");
        let _gone = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch dir");
        dir
    }

    /// A `zig` in `dir` that says it is `version`, with its library at `dir/lib`.
    fn fake_zig(dir: &Utf8Path, version: &str) {
        std::fs::create_dir_all(dir).expect("its dir");
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\nversion) echo {version} ;;\nenv) echo '.{{ .lib_dir = \
             \"{dir}/lib\", .std_dir = \"{dir}/lib/std\" }}' ;;\n*) exit 1 ;;\nesac\n"
        );
        let bin = dir.join("zig");
        std::fs::write(&bin, script).expect("the script");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("its mode");
    }

    /// The pinned ghostty's, and ghostty's form of the field among others; a manifest without
    /// it names no zig.
    #[test]
    fn the_zig_is_the_one_ghostty_s_manifest_names() {
        let ghostty = repo_root().expect("repo root").join("vendor/ghostty");
        let pinned = ghostty_zig(&ghostty).expect("the pinned ghostty names its zig");
        assert_eq!(series(&pinned).matches('.').count(), 1, "a release's version: {pinned}");
        let zon = ".{\n    .name = .ghostty,\n    .version = \"1.3.0\",\n    \
                   .minimum_zig_version = \"0.17.0\",\n}\n";
        assert_eq!(minimum_zig(zon), Some("0.17.0"));
        assert_eq!(minimum_zig(".{\n    .version = \"1.3.0\",\n}\n"), None);
        assert_eq!(minimum_zig(".minimum_zig_version_x = \"1\""), None);
    }

    /// Patch releases and dev builds are of their release's series.
    #[test]
    fn a_series_is_the_version_to_its_minor() {
        assert_eq!(series("0.17.0"), "0.17");
        assert_eq!(series("0.17.1"), "0.17");
        assert_eq!(series("0.18.0-dev.123+abcdef0"), "0.18");
        assert_eq!(series("0.17"), "0.17");
        assert_eq!(series("1"), "1");
    }

    /// The first zig of the series is taken, a zig of another one never, and none of it is
    /// none: a check of a tree at the old pin and one at the new each get their own.
    #[test]
    fn each_tree_gets_a_zig_of_its_ghostty_s_series() {
        let dir = scratch("series");
        let (old, new, newer) = (dir.join("old"), dir.join("new"), dir.join("newer"));
        fake_zig(&old, "0.16.0");
        fake_zig(&new, "0.17.0");
        fake_zig(&newer, "0.17.1");
        let missing = dir.join("missing");
        let sh = Shell::new().expect("a shell");
        let candidates = [Some(missing.as_path()), Some(&*old), Some(&*new), Some(&*newer)];
        let found = |series| first_of_series(&sh, series, &candidates);
        assert_eq!(found("0.16"), Some(Zig { version: "0.16.0".into(), dir: Some(old.clone()) }));
        assert_eq!(found("0.17"), Some(Zig { version: "0.17.0".into(), dir: Some(new.clone()) }));
        assert_eq!(found("0.18"), None);
        let _gone = std::fs::remove_dir_all(&dir);
    }

    /// ghostty's patch goes into a zig whose library is as 0.17.0 ships it, keeps the file's
    /// read-only mode, and a second setup finds it in and rewrites nothing.
    #[test]
    fn ghostty_s_patch_goes_into_its_zig_once() {
        let ghostty = repo_root().expect("repo root").join("vendor/ghostty");
        let dir = scratch("patch");
        fake_zig(&dir, "0.17.0");
        let zig = Zig { version: "0.17.0".into(), dir: Some(dir.clone()) };
        let patches = zig_patches(&ghostty, "0.17").expect("listed");
        if patches.is_empty() {
            // A ghostty at a pin with no patch for 0.17 has nothing to put in.
            return;
        }
        // The library as its patches find it: each hunk's old lines, between lines of its own.
        let mut files = Vec::new();
        for patch in &patches {
            let diff = std::fs::read_to_string(patch).expect("the patch");
            for (file, hunks) in unified_diff(&diff).expect("parsed") {
                let target = dir.join(&file);
                let olds: String = hunks.iter().flat_map(|(old, _)| ["// …\n", old]).collect();
                std::fs::create_dir_all(target.parent().expect("a dir")).expect("its dir");
                std::fs::write(&target, olds).expect("the file");
                let read_only = std::fs::Permissions::from_mode(0o444);
                std::fs::set_permissions(&target, read_only).expect("read-only");
                files.push((target, hunks));
            }
        }
        let sh = Shell::new().expect("a shell");
        let rewritten = patch_zig(&sh, &ghostty, &zig).expect("patched");
        assert_eq!(rewritten.len(), files.len(), "{rewritten:?}");
        for (target, hunks) in &files {
            let text = std::fs::read_to_string(target).expect("patched file");
            assert!(hunks.iter().all(|(_, new)| text.contains(new.as_str())), "{text}");
            let mode = std::fs::metadata(target).expect("its mode").permissions().mode();
            assert_eq!(mode & 0o777, 0o444, "{target} keeps its mode");
        }
        assert_eq!(patch_zig(&sh, &ghostty, &zig).expect("in already"), Vec::<Utf8PathBuf>::new());
        let _gone = std::fs::remove_dir_all(&dir);
    }

    /// A zig whose library has the lines in neither form, or the old ones twice, is not the one
    /// the patch was made for: refused, the file left alone.
    #[test]
    fn a_zig_the_patch_was_not_made_for_is_refused() {
        let diff = "--- a/lib/x.zig\n+++ b/lib/x.zig\n@@ -1,3 +1,3 @@\n a\n-    if (is_darwin) if\n\
                    +    if (is_darwin and process.can_spawn) if\n b\n";
        let files = unified_diff(diff).expect("parsed");
        let hunks = &files[0].1;
        let unpatched = "a\n    if (is_darwin) if\nb\n";
        let patched = with_hunks(unpatched, hunks).expect("patched");
        assert_eq!(patched, "a\n    if (is_darwin and process.can_spawn) if\nb\n");
        let other = unpatched.replace("is_darwin", "native_os.isDarwin()");
        let _refused = with_hunks(&other, hunks).expect_err("another zig's file");
        let twice = format!("{unpatched}{unpatched}");
        let _refused = with_hunks(&twice, hunks).expect_err("the old lines twice");
    }

    /// A hunk that only adds lines leaves its context in the patched file: it is found in by the
    /// added lines, not put in a second time.
    #[test]
    fn a_hunk_that_only_adds_lines_goes_in_once() {
        let diff = "--- a/lib/x.zig\n+++ b/lib/x.zig\n@@ -1,2 +1,3 @@\n a\n+x\n b\n";
        let files = unified_diff(diff).expect("parsed");
        let once = with_hunks("a\nb\n", &files[0].1).expect("patched");
        assert_eq!(once, "a\nx\nb\n");
        assert_eq!(with_hunks(&once, &files[0].1).expect("in already"), once);
    }

    /// A hunk ends where its header's counts say, so the next file's `---` line is a header and
    /// not a removed line; a diff cut inside a hunk is refused.
    #[test]
    fn a_hunk_is_read_by_its_line_counts() {
        let diff = "--- a/lib/a.zig\n+++ b/lib/a.zig\n@@ -1 +1 @@\n-a\n+b\n\
                    --- a/lib/b.zig\n+++ b/lib/b.zig\n@@ -3,2 +3,1 @@ fn f()\n c\n-d\n";
        let files = unified_diff(diff).expect("parsed");
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["lib/a.zig", "lib/b.zig"]);
        assert_eq!(files[0].1, [("a\n".to_owned(), "b\n".to_owned())]);
        assert_eq!(files[1].1, [("c\nd\n".to_owned(), "c\n".to_owned())]);
        let _cut = unified_diff("+++ b/lib/a.zig\n@@ -1,2 +1,2 @@\n a\n").expect_err("cut");
    }

    /// The patches setup finds for the pinned ghostty's zig are the ones ghostty's own build
    /// puts into it (`flake.nix`), each changing only zig's library: a ghostty bump that adds,
    /// drops or renames one fails here, not in a Mac's iOS build.
    #[test]
    fn setup_puts_in_the_patches_ghostty_s_build_does() {
        let ghostty = repo_root().expect("repo root").join("vendor/ghostty");
        let series = series(&ghostty_zig(&ghostty).expect("its zig")).to_owned();
        let patches = zig_patches(&ghostty, &series).expect("listed");
        let flake = std::fs::read_to_string(ghostty.join("flake.nix")).unwrap_or_default();
        let named = format!("./nix/patches/zig-{series}-");
        assert_eq!(flake.matches(&named).count(), patches.len(), "{patches:?}");
        for patch in patches {
            let name = patch.file_name().expect("a name");
            assert!(flake.contains(&format!("./nix/patches/{name}")), "flake.nix applies {name}");
            let diff = std::fs::read_to_string(&patch).expect("the patch");
            let files = unified_diff(&diff).expect("parsed");
            assert!(!files.is_empty(), "{name} changes nothing");
            for (file, hunks) in files {
                assert!(file.starts_with("lib/") && !hunks.is_empty(), "{name}: {file}");
            }
        }
    }
}
