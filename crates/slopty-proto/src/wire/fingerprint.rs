//! How the wire fingerprint is derived, shared by the build script, which bakes it into
//! [`FINGERPRINT`](crate::wire::FINGERPRINT), and the test that derives it again.
//!
//! It hashes the goldens that pin the wire: each `.snap` under `tests/snapshots` but the
//! control socket's, by name and body. The insta header (the test's source line, the
//! expression) is not the wire and is left out, so moving a test changes nothing.

use std::path::Path;

/// The goldens of the worker's local control socket, which no link carries.
const NOT_ON_THE_WIRE: &str = "golden__ctl__";

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
pub(crate) fn body(snapshot: &str) -> &str {
    snapshot
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(snapshot, |(_header, body)| body)
}

/// Whether the snapshot file `name` pins the wire.
pub(crate) fn on_the_wire(name: &str) -> bool {
    let snap = Path::new(name).extension().is_some_and(|ext| ext == "snap");
    snap && !name.starts_with(NOT_ON_THE_WIRE)
}

/// The fingerprint of `snapshots`, each a file name and its text; their order does not count.
pub(crate) fn of(mut snapshots: Vec<(String, String)>) -> u64 {
    snapshots.retain(|(name, _text)| on_the_wire(name));
    snapshots.sort_unstable();
    let mut hash = Fnv(Fnv::BASIS);
    for (name, text) in &snapshots {
        hash.write(name.as_bytes());
        hash.write(&[0]);
        hash.write(body(text).as_bytes());
        hash.write(&[0]);
    }
    hash.0
}

/// Every file in `dir`, by name, with its text: what [`of`] takes.
pub(crate) fn read(dir: &Path) -> std::io::Result<Vec<(String, String)>> {
    let mut snapshots = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if on_the_wire(&name) {
            snapshots.push((name, std::fs::read_to_string(entry.path())?));
        }
    }
    Ok(snapshots)
}
