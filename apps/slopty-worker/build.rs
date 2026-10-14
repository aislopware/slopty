//! Bakes the custody fingerprint of the ptyd this build is installed with into the worker
//! (`CUSTODY`), by the same derivation `slopty-ptyd`'s build script runs (`custody::derive`):
//! the worker dials only a ptyd that keeps it (`slopty_worker::Worker::connect`).

use std::path::PathBuf;

#[expect(
    clippy::redundant_pub_crate,
    reason = "ptyd's own file, compiled here too, where `pub(crate)` is what ptyd's test needs"
)]
#[path = "../slopty-ptyd/src/custody.rs"]
mod custody;

fn main() -> std::io::Result<()> {
    let var = |name: &str| {
        std::env::var_os(name).map(PathBuf::from).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("cargo did not set {name}"))
        })
    };
    let ptyd = var("CARGO_MANIFEST_DIR")?.join("../slopty-ptyd");
    let (custody, _succession, read) = custody::derive(&ptyd)?;
    let generated = format!(
        "/// The custody of the ptyd this build ships with: the worker dials no other.\n\
         pub const CUSTODY: &str = \"{custody}\";\n"
    );
    std::fs::write(var("OUT_DIR")?.join("custody.rs"), generated)?;
    for path in read {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    Ok(())
}
