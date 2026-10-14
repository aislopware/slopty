//! Bakes ptyd's custody fingerprint into the binary (`src/custody.rs`): a hash of the
//! protocol's goldens, the shell integration scripts and ptyd's own source, so it moves exactly
//! when one of them does and nobody bumps it by hand.

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
    let (custody, succession, read) = custody::derive(&var("CARGO_MANIFEST_DIR")?)?;
    let generated = format!(
        "/// What a running ptyd hands the worker: a hash of the protocol's goldens, the shell\n\
         /// integration scripts and ptyd's own source. Two builds that say the same share one\n\
         /// running ptyd.\n\
         pub const CUSTODY: &str = \"{custody}\";\n\
         /// How a running ptyd hands its sessions to the build it runs next: a hash of the\n\
         /// handover's goldens. Two builds that say the same hand over to each other.\n\
         pub const SUCCESSION: &str = \"{succession}\";\n"
    );
    std::fs::write(var("OUT_DIR")?.join("custody.rs"), generated)?;
    for path in read {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    Ok(())
}
