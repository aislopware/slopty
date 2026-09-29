//! Bakes the wire fingerprint into the crate (`src/wire.rs`): a hash of the goldens that pin
//! the wire, so it moves exactly when a golden does and nobody bumps it by hand.

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
    let generated = format!(
        "/// A hash of the goldens that pin the wire: two builds link only when theirs are equal.\n\
         pub const FINGERPRINT: u64 = 0x{literal};\n\
         /// This build as a person reads it: the version, and the fingerprint's first digits.\n\
         pub const BUILD: &str = \"{version}+wire.{shown}\";\n"
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
