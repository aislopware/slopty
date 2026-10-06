//! A machine's or a project's own colour, worn by its glyph alone (`docs/decisions/ui.md`,
//! "A machine and a project wear their own colour").
//!
//! The colour is one of the theme's eight identity hues ([`slopty_theme::Surfaces::identity`]),
//! picked by the FNV-1a hash of the thing's group key as [`GroupKey::as_str`] spells it:
//! `machine:<worker id>` for a machine, `project:<project id>` for a project. Every client, the
//! phone too, spells the same key for the same thing, so each shows the same colour without a
//! word on the wire. The hues sit at the status fills' lightness and keep 20 degrees clear of
//! every status hue, so a machine's glyph never reads as a state.

use slopty_client::groups::GroupKey;
use slopty_client::layout::WorkerKey;
use slopty_theme::{Rgb, Theme};

/// The colour of the thing `key` names: its identity hue, for its glyph and nothing else,
/// never its name, its row, a chip or a wash.
#[must_use]
pub fn identity_ink(theme: &Theme, key: &GroupKey) -> Rgb {
    let hues = &theme.surfaces.identity;
    let at = usize::try_from(fnv1a(key.as_str().as_bytes())).unwrap_or_default();
    at.checked_rem(hues.len())
        .and_then(|at| hues.get(at))
        .copied()
        .unwrap_or(theme.surfaces.text_secondary)
}

/// The colour of `worker`'s machine glyph: its identity hue, or muted while it is `away`, so a
/// lost machine greys out.
#[must_use]
pub fn machine_ink(theme: &Theme, worker: WorkerKey, away: bool) -> Rgb {
    if away { theme.surfaces.text_muted } else { identity_ink(theme, &GroupKey::machine(worker)) }
}

/// The 32-bit FNV-1a hash of `bytes`: short, well spread over short keys, and the same on
/// every platform and in every build.
fn fnv1a(bytes: &[u8]) -> u32 {
    const OFFSET: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    bytes.iter().fold(OFFSET, |hash, &byte| (hash ^ u32::from(byte)).wrapping_mul(PRIME))
}

#[cfg(test)]
mod tests {
    use slopty_theme::Variant;

    use super::*;

    /// A machine wears the same colour on every client: the hash is FNV-1a of its key's
    /// spelling and nothing local, so a known worker id maps to a pinned hue, here and on the
    /// phone alike. A change of the hash or the key's spelling is a change every client would
    /// show at once, and this pin says so.
    #[test]
    fn a_machine_wears_the_same_colour_on_every_client() {
        assert_eq!(fnv1a(b""), 0x811c_9dc5, "the offset basis");
        assert_eq!(fnv1a(b"a"), 0xe40c_292c, "FNV-1a's own test vector");
        let worker = WorkerKey::new(0x01a1_0fde_3c66_7300_af88_c958_7de4_7c02);
        let key = GroupKey::machine(worker);
        assert_eq!(key.as_str(), "machine:01a10fde3c667300af88c9587de47c02");
        for theme in [Theme::new(Variant::Dark), Theme::new(Variant::Light)] {
            let hues = theme.surfaces.identity;
            assert_eq!(machine_ink(&theme, worker, false), hues[4], "the pinned hue");
            assert_eq!(identity_ink(&theme, &GroupKey::machine(WorkerKey::new(2))), hues[2]);
            assert_eq!(identity_ink(&theme, &GroupKey::new("project", "parser")), hues[3]);
            assert_eq!(machine_ink(&theme, worker, true), theme.surfaces.text_muted, "away");
        }
    }
}
