//! `cargo xtask symbolicate` and `cargo xtask dsyms`: crash reports resolved away from the
//! machine they happened on.
//!
//! A shipped binary carries no debug info (the `dist` profile strips it into `<bin>.dSYM`, and
//! the dSYMs ship apart from the app, `docs/decisions/crashes.md`). A report taken in the field
//! still has each frame's address, its offset into the executable, the executable's Mach-O UUID
//! and the names from the symbol table. `dsyms` files each dSYM under its binary's UUID;
//! `symbolicate` finds the dSYM with the report's UUID and resolves every frame of the
//! executable to its file, line and inlined frames with `atos`, Apple's own symbolicator.

use std::fmt::Write as _;
use std::io::Read as _;
use std::process::Command;

use anyhow::{Context as _, Result, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use slopty_crash::{Frame, Kind, Report};
use xshell::{Shell, cmd};

use crate::tools::repo_root;

/// `cargo xtask symbolicate` options.
#[derive(Args, Debug, Clone)]
pub struct SymbolicateOpts {
    /// The report: a `.json` file from `<data dir>/crashes`.
    report: Utf8PathBuf,
    /// Where else to look for dSYMs: a directory, or a release's `*-dSYMs.tar.gz`. `target/dist`,
    /// `target/release` and `target/bundle` are always searched.
    #[arg(long = "dsyms")]
    dsyms: Vec<Utf8PathBuf>,
    /// Print the resolved report as JSON instead of its frames.
    #[arg(long)]
    json: bool,
}

/// `cargo xtask dsyms` options.
#[derive(Args, Debug, Clone)]
pub struct DsymsOpts {
    /// Where the binaries and their `<bin>.dSYM` were built.
    #[arg(long, default_value = "target/dist")]
    from: Utf8PathBuf,
    /// Where each dSYM goes, as `<UUID>/<bin>.dSYM`.
    #[arg(long)]
    out: Utf8PathBuf,
    /// The binaries.
    #[arg(required = true)]
    bins: Vec<String>,
}

pub fn run(sh: &Shell, opts: &SymbolicateOpts) -> Result<()> {
    let text =
        std::fs::read_to_string(&opts.report).with_context(|| format!("read {}", opts.report))?;
    let mut report: Report = serde_json::from_str(&text)
        .with_context(|| format!("{} is no crash report", opts.report))?;
    let uuid = report.build.uuid.clone().context("the report names no build UUID")?;
    let root = repo_root()?;
    let mut roots = Vec::new();
    for given in &opts.dsyms {
        roots.push(unpacked(sh, &root, given)?);
    }
    roots.extend(["target/dist", "target/release", "target/bundle"].map(|dir| root.join(dir)));
    let dwarf = find_dsym(&roots, &uuid).with_context(|| {
        format!(
            "no dSYM with UUID {uuid} under {}",
            roots.iter().map(|r| r.as_str()).collect::<Vec<_>>().join(", ")
        )
    })?;
    eprintln!("dSYM {dwarf}");
    resolve(&mut report, &dwarf)?;
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{} ({}, {})", report.headline(), report.process, report.when());
        for frame in &report.frames {
            println!("  {}", frame.describe());
        }
    }
    Ok(())
}

pub fn dsyms(sh: &Shell, opts: &DsymsOpts) -> Result<()> {
    let bins: Vec<&str> = opts.bins.iter().map(String::as_str).collect();
    collect(sh, &opts.from, &bins, &opts.out)?;
    println!("✔ {}", opts.out);
    Ok(())
}

/// Each of `bins`' dSYMs, from beside it in `built`, into `out/<UUID>/<bin>.dSYM`.
pub fn collect(sh: &Shell, built: &Utf8Path, bins: &[&str], out: &Utf8Path) -> Result<()> {
    for bin in bins {
        let uuid = macho(&built.join(bin))?.uuid;
        let dsym = built.join(format!("{bin}.dSYM"));
        let dwarf = dwarf_files(&dsym)?;
        ensure!(
            dwarf.iter().any(|file| macho(file).is_ok_and(|m| m.uuid == uuid)),
            "{dsym} is not the dSYM of {bin} ({uuid})"
        );
        let to = out.join(&uuid).join(format!("{bin}.dSYM"));
        if to.exists() {
            sh.remove_path(&to)?;
        }
        sh.create_dir(out.join(&uuid))?;
        // Cargo leaves `<bin>.dSYM` as a link into `deps`: copy what it points at.
        cmd!(sh, "cp -RL {dsym} {to}").run()?;
    }
    Ok(())
}

/// `given` as a directory: a `.tar.gz` is unpacked under `target/symbolicate` first.
fn unpacked(sh: &Shell, root: &Utf8Path, given: &Utf8Path) -> Result<Utf8PathBuf> {
    if given.is_dir() {
        return Ok(given.to_owned());
    }
    let name = given.file_name().context("an archive without a name")?;
    ensure!(name.ends_with(".tar.gz"), "{given} is neither a directory nor a .tar.gz");
    let dir = root.join("target/symbolicate").join(name.trim_end_matches(".tar.gz"));
    if dir.exists() {
        sh.remove_path(&dir)?;
    }
    sh.create_dir(&dir)?;
    cmd!(sh, "tar -xzf {given} -C {dir}").run()?;
    Ok(dir)
}

/// The DWARF file of the first dSYM under `roots` (three levels down at most, links followed)
/// whose UUID is `uuid`.
fn find_dsym(roots: &[Utf8PathBuf], uuid: &str) -> Option<Utf8PathBuf> {
    fn walk(dir: &Utf8Path, depth: u8, uuid: &str) -> Option<Utf8PathBuf> {
        let entries = dir.read_dir_utf8().ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() == Some("dSYM") {
                let found = dwarf_files(path)
                    .ok()?
                    .into_iter()
                    .find(|file| macho(file).is_ok_and(|m| m.uuid.eq_ignore_ascii_case(uuid)));
                if found.is_some() {
                    return found;
                }
            } else if let Some(below) = depth.checked_sub(1)
                && path.is_dir()
                && let Some(found) = walk(path, below, uuid)
            {
                return Some(found);
            }
        }
        None
    }
    roots.iter().find_map(|root| walk(root, 2, uuid))
}

/// The files in a dSYM's `Contents/Resources/DWARF`.
fn dwarf_files(dsym: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    let dir = dsym.join("Contents/Resources/DWARF");
    let mut files = Vec::new();
    for entry in dir.read_dir_utf8().with_context(|| format!("read {dir}"))? {
        files.push(entry?.path().to_owned());
    }
    Ok(files)
}

/// What a 64-bit Mach-O file says of itself.
#[derive(Debug, PartialEq, Eq)]
struct MachO {
    /// `LC_UUID`, as `dwarfdump --uuid` and the reports spell it.
    uuid: String,
    /// `__TEXT`'s address in the file, where offsets into the image start.
    text: u64,
}

/// Reads `path`'s header and load commands (`<mach-o/loader.h>`): a thin, little-endian 64-bit
/// file, as every Slopty binary and its dSYM is.
fn macho(path: &Utf8Path) -> Result<MachO> {
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const LC_SEGMENT_64: u32 = 0x19;
    const LC_UUID: u32 = 0x1b;
    let mut file = std::fs::File::open(path).with_context(|| format!("open {path}"))?;
    let mut header = [0_u8; 32];
    file.read_exact(&mut header).with_context(|| format!("{path}: no Mach-O header"))?;
    let word = |bytes: &[u8], at: usize| -> Result<u32> {
        let slice = bytes.get(at..at.saturating_add(4)).context("a load command past its end")?;
        Ok(u32::from_le_bytes(slice.try_into()?))
    };
    ensure!(word(&header, 0)? == MH_MAGIC_64, "{path} is not a thin 64-bit Mach-O");
    let size = usize::try_from(word(&header, 20)?)?;
    let mut commands = vec![0_u8; size];
    file.read_exact(&mut commands).with_context(|| format!("{path}: load commands"))?;
    let (mut uuid, mut text) = (None, None);
    let mut at = 0_usize;
    while at.saturating_add(8) <= commands.len() {
        let cmd = word(&commands, at)?;
        let cmdsize = usize::try_from(word(&commands, at.saturating_add(4))?)?;
        let end = at.checked_add(cmdsize).context("a load command past the end")?;
        let body = commands.get(at..end).context("a load command past the end")?;
        match cmd {
            LC_UUID => {
                let bytes: [u8; 16] = body.get(8..24).context("a short LC_UUID")?.try_into()?;
                uuid = Some(uuid_text(&bytes));
            }
            LC_SEGMENT_64 if body.get(8..14) == Some(b"__TEXT") && body.get(14) == Some(&0) => {
                let vmaddr = body.get(24..32).context("a short LC_SEGMENT_64")?;
                text = Some(u64::from_le_bytes(vmaddr.try_into()?));
            }
            _ => {}
        }
        ensure!(cmdsize >= 8, "{path}: a load command of {cmdsize} bytes");
        at = end;
    }
    Ok(MachO {
        uuid: uuid.with_context(|| format!("{path} has no LC_UUID"))?,
        text: text.with_context(|| format!("{path} has no __TEXT"))?,
    })
}

/// `8C63022A-CF30-3538-9024-34156A089D88`.
fn uuid_text(bytes: &[u8; 16]) -> String {
    let mut text = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            text.push('-');
        }
        let _infallible = write!(text, "{byte:02X}");
    }
    text
}

/// Every frame of the report's executable, with the frames inlined at its address, resolved
/// against `dwarf`. A frame of another image (a system library) stays as the report has it.
fn resolve(report: &mut Report, dwarf: &Utf8Path) -> Result<()> {
    let exe =
        report.build.exe.as_deref().map(|exe| {
            Utf8Path::new(exe).file_name().map_or_else(|| exe.to_owned(), str::to_owned)
        });
    let text = macho(dwarf)?.text;
    // Consecutive frames at one address are one frame and those inlined into it.
    let mut groups: Vec<Vec<Frame>> = Vec::new();
    for frame in std::mem::take(&mut report.frames) {
        match groups.last_mut() {
            Some(group) if group.first().is_some_and(|g| g.address == frame.address) => {
                group.push(frame);
            }
            _ => groups.push(vec![frame]),
        }
    }
    // A signal's first frame is the faulting instruction; every other address is a return
    // address, the instruction after its call, so the call is looked up.
    let faulted = matches!(report.kind, Kind::Signal { .. });
    let lookups: Vec<Option<u64>> = groups
        .iter()
        .enumerate()
        .map(|(i, group)| {
            let frame = group.first()?;
            let ours = frame.image.is_some() && frame.image == exe;
            let offset = frame.offset.filter(|_| ours)?;
            let back = u64::from(!(faulted && i == 0));
            text.checked_add(offset.checked_sub(back)?)
        })
        .collect();
    let addresses: Vec<u64> = lookups.iter().flatten().copied().collect();
    let mut resolved = atos(dwarf, text, &addresses)?.into_iter();
    for (group, lookup) in groups.into_iter().zip(lookups) {
        let chain = lookup.and_then(|_| resolved.next()).filter(|chain| !chain.is_empty());
        let Some(chain) = chain else {
            report.frames.extend(group);
            continue;
        };
        let raw = group.into_iter().next().unwrap_or_default();
        report.frames.extend(chain.into_iter().map(|(function, file, line)| Frame {
            function: Some(function),
            file,
            line,
            ..raw.clone()
        }));
    }
    Ok(())
}

/// A function and where in its source: one frame of an address's chain.
type Resolved = (String, Option<String>, Option<u32>);

/// Each of `addresses` (in the file's own address space) resolved by `atos -i`, innermost frame
/// first.
fn atos(dwarf: &Utf8Path, text: u64, addresses: &[u64]) -> Result<Vec<Vec<Resolved>>> {
    if addresses.is_empty() {
        return Ok(Vec::new());
    }
    let run = |addresses: &[u64]| -> Result<String> {
        let out = Command::new("atos")
            .args(["-o", dwarf.as_str(), "-arch", "arm64", "-i", "-fullPath", "-l"])
            .arg(format!("{text:#x}"))
            .args(addresses.iter().map(|a| format!("{a:#x}")))
            .output()
            .context("atos (Xcode's command line tools)")?;
        ensure!(out.status.success(), "atos: {}", String::from_utf8_lossy(&out.stderr));
        Ok(String::from_utf8(out.stdout)?)
    };
    // `atos -i` ends each address's lines with an empty one; an address with no symbol at all
    // prints an empty line of its own, which makes one run ambiguous. Then each goes alone.
    let chains = groups(&run(addresses)?);
    if chains.len() == addresses.len() {
        return Ok(chains);
    }
    addresses
        .iter()
        .map(|a| Ok(groups(&run(&[*a])?).into_iter().next().unwrap_or_default()))
        .collect()
}

/// `atos -i` output split into each address's frames.
fn groups(output: &str) -> Vec<Vec<Resolved>> {
    let mut all = Vec::new();
    let mut current = Vec::new();
    for line in output.lines() {
        if line.is_empty() {
            all.push(std::mem::take(&mut current));
        } else if let Some(frame) = parse(line) {
            current.push(frame);
        }
    }
    if !current.is_empty() {
        all.push(current);
    }
    all
}

/// One `atos` line: `name (in image) (file:line)`, `name (in image) + 12`, or a bare address.
fn parse(line: &str) -> Option<Resolved> {
    let (name, rest) = line.split_once(" (in ")?;
    let place = rest.rsplit_once(") (").map(|(_, place)| place.trim_end_matches(')'));
    let (file, line) = match place.and_then(|p| p.rsplit_once(':')) {
        Some((file, line)) => (Some(file.to_owned()), line.parse().ok().filter(|l| *l > 0)),
        None => (None, None),
    };
    let name = rustc_demangle::try_demangle(name)
        .map_or_else(|_| name.to_owned(), |demangled| format!("{demangled:#}"));
    Some((name, file, line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atos_lines_parse_into_name_file_and_line() {
        let inlined = "_RNvCs1234_12slopty_crash7install (in slopty-worker) (/a/b/lib.rs:135)";
        assert_eq!(
            parse(inlined),
            Some(("slopty_crash::install".to_owned(), Some("/a/b/lib.rs".to_owned()), Some(135)))
        );
        let generic = "call_once<fn() -> core::result::Result<(), E>, ()> (in x) (/f.rs:250)";
        assert_eq!(
            parse(generic).map(|(name, ..)| name),
            Some("call_once<fn() -> core::result::Result<(), E>, ()>".to_owned())
        );
        let compiler_made = "poll (in x) (harness.rs:0)";
        assert_eq!(
            parse(compiler_made),
            Some(("poll".to_owned(), Some("harness.rs".to_owned()), None))
        );
        assert_eq!(parse("0x5"), None, "an address with nothing to say");
        let out = "a (in x) (a.rs:1)\nb (in x) (b.rs:2)\n\nc (in x) (c.rs:3)\n\n";
        let chains = groups(out);
        assert_eq!(chains.len(), 2, "{chains:?}");
        assert_eq!(chains[0].len(), 2, "two frames at the first address");
    }

    /// The pc here, in a function inlined into [`outer`].
    #[expect(clippy::inline_always, reason = "the test needs a frame inlined into another")]
    #[inline(always)]
    fn inner() -> usize {
        let pc: usize;
        // SAFETY: `adr` only writes the address of this instruction into a register; it touches
        // no memory and no flags (AArch64 ARM, "ADR").
        unsafe {
            std::arch::asm!("adr {pc}, .", pc = out(reg) pc, options(nomem, nostack, preserves_flags));
        }
        pc
    }

    #[inline(never)]
    fn outer() -> usize {
        std::hint::black_box(inner())
    }

    /// A dSYM of this very test binary, found by its UUID, resolves an address inside a function
    /// inlined into another to both of them, with their file and line.
    #[test]
    fn a_dsym_found_by_uuid_resolves_inlined_frames() {
        let sh = Shell::new().unwrap();
        let exe = Utf8PathBuf::try_from(std::env::current_exe().unwrap()).unwrap();
        let dir = Utf8PathBuf::try_from(std::env::temp_dir())
            .unwrap()
            .join(format!("xtask-symbolicate-{}", std::process::id()));
        let _fresh: std::io::Result<()> = std::fs::remove_dir_all(&dir);
        let own = macho(&exe).unwrap();
        let dsym = dir.join("elsewhere/test.dSYM");
        cmd!(sh, "dsymutil {exe} -o {dsym}").quiet().ignore_stdout().run().unwrap();
        let roots = std::slice::from_ref(&dir);
        let dwarf = find_dsym(roots, &own.uuid).expect("found by UUID");
        assert_eq!(macho(&dwarf).unwrap(), own, "the dSYM's UUID and __TEXT are the binary's");
        assert!(find_dsym(roots, "00000000-0000-0000-0000-000000000000").is_none(), "no other");

        let pc = outer();
        let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
        // SAFETY: `dladdr` (dlfcn.h) only fills `info` for an address in a loaded image.
        let found =
            unsafe { libc::dladdr(std::ptr::with_exposed_provenance(pc), info.as_mut_ptr()) };
        assert_ne!(found, 0, "dladdr");
        // SAFETY: `dladdr` returned non-zero, so it filled `info`.
        let base = unsafe { info.assume_init() }.dli_fbase.expose_provenance();
        let offset = u64::try_from(pc.checked_sub(base).unwrap()).unwrap();
        let chains = atos(&dwarf, own.text, &[own.text.checked_add(offset).unwrap()]).unwrap();
        let chain = &chains[0];
        assert_eq!(chain.len(), 2, "the inlined function and the one it is in: {chain:?}");
        assert!(chain[0].0.ends_with("inner"), "innermost first: {chain:?}");
        assert!(chain[1].0.ends_with("symbolicate::tests::outer"), "{chain:?}");
        for (_, file, line) in chain {
            assert!(
                file.as_deref().is_some_and(|f| f.ends_with("xtask/src/symbolicate.rs")),
                "{chain:?}"
            );
            assert!(line.is_some(), "{chain:?}");
        }
        let _removed: std::io::Result<()> = std::fs::remove_dir_all(&dir);
    }
}
