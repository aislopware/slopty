//! Bakes ptyd's custody fingerprint into the binary (`src/custody.rs`): a hash of the
//! protocol's goldens and the shell integration scripts, so it moves exactly when one of them
//! does and nobody bumps it by hand.

use std::path::PathBuf;

#[expect(
    clippy::redundant_pub_crate,
    reason = "the test compiles the same file, and there `pub(crate)` is what it needs"
)]
#[path = "src/custody.rs"]
mod custody;

fn main() -> std::io::Result<()> {
    let var = |name: &str| {
        std::env::var_os(name).map(PathBuf::from).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("cargo did not set {name}"))
        })
    };
    let (goldens, shell) = custody::sources(&var("CARGO_MANIFEST_DIR")?);
    let custody = custody::of(custody::goldens(&goldens)?, custody::scripts(&shell)?);
    let generated = format!(
        "/// What a running ptyd hands the worker: a hash of the protocol's goldens and the shell\n\
         /// integration scripts. Two builds that say the same share one running ptyd.\n\
         pub const CUSTODY: &str = \"{custody}\";\n"
    );
    std::fs::write(var("OUT_DIR")?.join("custody.rs"), generated)?;
    for path in [goldens, shell, PathBuf::from("src/custody.rs")] {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    Ok(())
}
