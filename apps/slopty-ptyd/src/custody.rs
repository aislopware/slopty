//! How ptyd's custody fingerprint is derived, shared by the build script, which bakes it into
//! the binary (`slopty-ptyd --custody`), and the test that derives it again.
//!
//! A running ptyd hands a new worker three things: the protocol they speak over its socket, the
//! shell integration scripts it wrote when it started, which every shell it spawns runs, and its
//! own behaviour with the sessions it holds. The fingerprint hashes what pins each: the
//! protocol's goldens (`tests/snapshots`, each by name and body; the insta header is not the
//! protocol), every file of `slopty-pty`'s `assets/shell`, and the source of ptyd and of
//! `slopty-pty` ([`code`]). Two builds whose fingerprints are equal can share one running ptyd,
//! so a worker update leaves it, and every session it holds, alone; a fix to ptyd itself is
//! handed over to, never left waiting for a restart that ends every session.
//!
//! Its succession fingerprint hashes the goldens of the handover alone: how a ptyd hands its
//! sessions to the new build it runs in place. Two builds whose custody differs and whose
//! succession is equal hand over, so an update ends no session either.

use std::path::Path;

/// FNV-1a, 64 bits: a fixed function of the bytes, the same on every host and toolchain.
struct Fnv(u64);

impl Fnv {
    const BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(Self::PRIME);
        }
    }
}

/// What an insta snapshot pins: the text after its `---` header.
fn body(snapshot: &str) -> &str {
    snapshot
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(snapshot, |(_header, body)| body)
}

/// The fingerprint of `goldens` (snapshot file names and their text) and `scripts` (paths under
/// the shell assets and their bytes), as 16 hex digits; their order does not count.
pub(crate) fn of(goldens: Vec<(String, String)>, scripts: Vec<(String, Vec<u8>)>) -> String {
    let mut goldens: Vec<(String, String)> = goldens
        .into_iter()
        .filter(|(name, _)| Path::new(name).extension().is_some_and(|ext| ext == "snap"))
        .collect();
    goldens.sort_unstable();
    let mut scripts = scripts;
    scripts.sort_unstable();
    let mut hash = Fnv(Fnv::BASIS);
    for (name, text) in &goldens {
        hash.write(b"golden/");
        hash.write(name.as_bytes());
        hash.write(&[0]);
        hash.write(body(text).as_bytes());
        hash.write(&[0]);
    }
    for (path, bytes) in &scripts {
        hash.write(b"shell/");
        hash.write(path.as_bytes());
        hash.write(&[0]);
        hash.write(bytes);
        hash.write(&[0]);
    }
    format!("{:016x}", hash.0)
}

/// The succession fingerprint of `goldens`: of the goldens of how one ptyd hands its sessions
/// to the build it runs next (`succession_*`) alone, as 16 hex digits. Two builds that say the
/// same can hand over to each other, whatever else they changed.
pub(crate) fn succession(goldens: &[(String, String)]) -> String {
    let handover =
        goldens.iter().filter(|(name, _)| name.contains("golden__succession_")).cloned().collect();
    of(handover, Vec::new())
}

/// Every file directly in `dir`, by name, with its text; nothing when `dir` is not there yet
/// (before the first golden is written).
pub(crate) fn goldens(dir: &Path) -> std::io::Result<Vec<(String, String)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            let name = entry.file_name().to_string_lossy().into_owned();
            files.push((name, std::fs::read_to_string(entry.path())?));
        }
    }
    Ok(files)
}

/// Every file under `dir`, at any depth, by its path below `dir` (`/`-separated), with its
/// bytes.
pub(crate) fn scripts(dir: &Path) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((at, below)) = pending.pop() {
        for entry in std::fs::read_dir(&at)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if below.is_empty() { name } else { format!("{below}/{name}") };
            if entry.file_type()?.is_dir() {
                pending.push((entry.path(), path));
            } else {
                files.push((path, std::fs::read(entry.path())?));
            }
        }
    }
    Ok(files)
}

/// The custody and succession fingerprints of the ptyd whose package is at `manifest_dir`, as
/// its binary says them: the one derivation both ptyd's and the worker's build scripts bake,
/// so a worker and the ptyd of its own build never disagree. With the paths read, for
/// `rerun-if-changed`.
pub(crate) fn derive(
    manifest_dir: &Path,
) -> std::io::Result<(String, String, Vec<std::path::PathBuf>)> {
    let (goldens_dir, shell) = sources(manifest_dir);
    let read = goldens(&goldens_dir)?;
    let succession = succession(&read);
    let mut held = scripts(&shell)?;
    held.extend(code(manifest_dir)?);
    let custody = of(read, held);
    let mut paths = vec![goldens_dir, shell, manifest_dir.join("src/custody.rs")];
    paths.extend(code_dirs(manifest_dir).map(|(_, dir)| dir));
    Ok((custody, succession, paths))
}

/// The source of ptyd and of the `slopty-pty` it is built on, from the package's own
/// directory: each file by `code/<crate>/<path>`, with its bytes.
pub(crate) fn code(manifest_dir: &Path) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for (name, dir) in code_dirs(manifest_dir) {
        files.extend(
            scripts(&dir)?.into_iter().map(|(path, bytes)| (format!("code/{name}/{path}"), bytes)),
        );
    }
    Ok(files)
}

/// The source directories [`code`] reads, by crate.
pub(crate) fn code_dirs(manifest_dir: &Path) -> [(&'static str, std::path::PathBuf); 2] {
    [
        ("slopty-ptyd", manifest_dir.join("src")),
        ("slopty-pty", manifest_dir.join("../../crates/slopty-pty/src")),
    ]
}

/// Where the goldens and the shell assets are, from the package's own directory.
pub(crate) fn sources(manifest_dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let goldens = manifest_dir.join("tests").join("snapshots");
    let shell = manifest_dir.join("../../crates/slopty-pty/assets/shell");
    (goldens, shell)
}
