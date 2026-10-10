//! Bakes the wire fingerprint into the crate (`src/wire.rs`): a hash of the goldens that pin
//! the wire, so it moves exactly when a golden does and nobody bumps it by hand.
//!
//! [`BUILD`] also says when the wire last changed, so two builds on different wires tell which
//! is the newer: the last commit to touch the goldens, or now while they are being edited. It
//! is read only when the goldens change, as the fingerprint is, so a commit rebuilds nothing.

use std::path::PathBuf;

#[expect(
    clippy::redundant_pub_crate,
    reason = "the library compiles the same file, and there `pub(crate)` is what it needs"
)]
#[path = "src/wire/fingerprint.rs"]
mod fingerprint;

/// Digits of the fingerprint in [`BUILD`]'s text: enough to tell two builds apart by eye.
const SHOWN_DIGITS: usize = 8;

fn main() -> std::io::Result<()> {
    let var = |name: &str| {
        std::env::var_os(name).map(PathBuf::from).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("cargo did not set {name}"))
        })
    };
    let snapshots = var("CARGO_MANIFEST_DIR")?.join("tests").join("snapshots");
    let fingerprint = fingerprint::of(fingerprint::read(&snapshots)?);
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let hex = format!("{fingerprint:016x}");
    let shown = hex.get(..SHOWN_DIGITS).unwrap_or(&hex);
    let grouped: Vec<&str> =
        hex.as_bytes().chunks(4).filter_map(|g| std::str::from_utf8(g).ok()).collect();
    let literal = grouped.join("_");
    let changed = wire_changed(&var("CARGO_MANIFEST_DIR")?)
        .map(|seconds| format!(".{}", utc_stamp(seconds)))
        .unwrap_or_default();
    let generated = format!(
        "/// A hash of the goldens that pin the wire: two builds link only when theirs are equal.\n\
         pub const FINGERPRINT: u64 = 0x{literal};\n\
         /// This build as a person reads it: the version, the fingerprint's first digits, and \
         when the wire last changed (UTC), when git could say.\n\
         pub const BUILD: &str = \"{version}+wire.{shown}{changed}\";\n"
    );
    std::fs::write(var("OUT_DIR")?.join("wire.rs"), generated)?;
    rerun_if_changed(&["tests/snapshots", "src/wire/fingerprint.rs"]);
    Ok(())
}

/// Run again when anything under `paths` changes; a directory counts every file in it.
fn rerun_if_changed(paths: &[&str]) {
    for path in paths {
        println!("cargo::rerun-if-changed={path}");
    }
}

/// When the goldens in `crate_dir` last changed, in seconds since the Unix epoch: now while
/// they differ from the last commit, else that commit's time. `None` without git.
fn wire_changed(crate_dir: &std::path::Path) -> Option<u64> {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(crate_dir)
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())?;
        String::from_utf8(out.stdout).ok()
    };
    let edited = git(&["status", "--porcelain", "--", "tests/snapshots"])?;
    if !edited.trim().is_empty() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?;
        return Some(now.as_secs());
    }
    git(&["log", "-1", "--format=%ct", "--", "tests/snapshots"])?.trim().parse().ok()
}

/// `seconds` since the Unix epoch as `YYYYMMDDTHHMMZ`, which sorts as it reads and fits a
/// version's build metadata (Hinnant's civil-from-days).
fn utc_stamp(seconds: u64) -> String {
    let (days, of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let z = days.wrapping_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.wrapping_sub(era.wrapping_mul(146_097));
    let yoe = doe
        .wrapping_sub(doe.div_euclid(1_460))
        .wrapping_add(doe.div_euclid(36_524))
        .wrapping_sub(doe.div_euclid(146_096))
        .div_euclid(365);
    let doy = doe.wrapping_sub(
        yoe.wrapping_mul(365).wrapping_add(yoe.div_euclid(4)).wrapping_sub(yoe.div_euclid(100)),
    );
    let mp = doy.wrapping_mul(5).wrapping_add(2).div_euclid(153);
    let day = doy.wrapping_sub(mp.wrapping_mul(153).wrapping_add(2).div_euclid(5)).wrapping_add(1);
    let month = if mp < 10 { mp.wrapping_add(3) } else { mp.wrapping_sub(9) };
    let year = yoe.wrapping_add(era.wrapping_mul(400)).wrapping_add(u64::from(month <= 2));
    let (hour, minute) = (of_day.div_euclid(3_600), of_day.rem_euclid(3_600).div_euclid(60));
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}Z")
}
