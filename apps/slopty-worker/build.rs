//! Bakes the custody fingerprint of the ptyd this build is installed with into the worker
//! (`CUSTODY`), from the same sources `slopty-ptyd`'s build script hashes: the worker dials only
//! a ptyd that keeps it (`slopty_worker::Worker::connect`).

use std::path::PathBuf;

#[expect(
    clippy::redundant_pub_crate,
    dead_code,
    reason = "ptyd's own file, compiled here too; the worker needs its custody and no more"
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
    let (goldens, shell) = custody::sources(&ptyd);
    let custody = custody::of(custody::goldens(&goldens)?, custody::scripts(&shell)?);
    let generated = format!(
        "/// The custody of the ptyd this build ships with: the worker dials no other.\n\
         pub const CUSTODY: &str = \"{custody}\";\n"
    );
    std::fs::write(var("OUT_DIR")?.join("custody.rs"), generated)?;
    for path in [goldens, shell, ptyd.join("src/custody.rs")] {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    Ok(())
}
