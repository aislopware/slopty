//! The client's first look at a reassembled video frame, on bytes a peer chose.
//!
//! That is the walk over its 4-byte lengths ([`slopty_codec::nal`]), the split of the parameter
//! sets in front from the picture behind them, and the SPS reads a decoder is rebuilt from.
//!
//! The first 4 bytes are a picture size for the host's SPS rewrite
//! ([`slopty_codec::conformance::crop_access_unit`]); the rest is the access unit.
//!
//! The three walks must agree on where the bytes stop being units, a well-formed unit must
//! come back byte for byte from its units, the picture must start on a unit's boundary, and
//! the rewrite must leave a well-formed unit well formed (or the unit as it was).

use slopty_codec::CodecError;
use slopty_codec::conformance::{crop_access_unit, crop_sps};
use slopty_codec::nal::{self, AccessUnit, h264, hevc};

/// Run one input.
pub fn run(data: &[u8]) {
    let Some((size, unit)) = data.split_first_chunk::<4>() else { return };
    let shown = (
        u32::from(u16::from_be_bytes([size[0], size[1]])),
        u32::from(u16::from_be_bytes([size[2], size[3]])),
    );
    let units: Vec<&[u8]> = nal::units(unit).collect();
    let walked = units.iter().fold(0_usize, |sum, u| sum.saturating_add(4).saturating_add(u.len()));
    assert!(units.iter().all(|u| !u.is_empty()), "an empty unit was walked");
    match nal::check(unit) {
        Ok(()) => {
            assert_eq!(walked, unit.len(), "check passed what the walk did not reach");
            let mut again = Vec::with_capacity(unit.len());
            for u in &units {
                nal::push(&mut again, u).expect("a walked unit is pushed back");
            }
            assert_eq!(again, unit, "the units came back other than they went");
        }
        Err(CodecError::MalformedNal { offset }) => {
            assert_eq!(walked, offset, "check and the walk stop in different places");
        }
        Err(other) => panic!("check: {other}"),
    }
    let well_formed = nal::check(unit).is_ok();
    for is_parameter_set in [hevc::is_parameter_set as fn(&[u8]) -> bool, h264::is_parameter_set] {
        match AccessUnit::parse(unit, is_parameter_set) {
            Ok(parsed) => {
                assert!(well_formed, "parse took what check refused");
                let at = parsed.picture_at();
                let picture = unit.get(at..).expect("the picture starts inside the unit");
                assert!(nal::check(picture).is_ok(), "the picture starts off a boundary");
                let sets = parsed.parameter_sets();
                let front = units.get(..sets.len()).expect("no more sets than units");
                assert_eq!(sets, front, "the sets are not the units in front");
                assert!(sets.iter().all(|s| is_parameter_set(s)), "a picture unit among the sets");
            }
            Err(_) => assert!(!well_formed, "parse refused what check took"),
        }
    }
    for u in &units {
        let format = hevc::sample_format(u);
        let size = hevc::shown_size(u);
        if hevc::nal_type(u) != Some(hevc::SPS) {
            assert!(format.is_none() && size.is_none(), "an SPS read from another unit");
        }
        if let Some(cropped) = crop_sps(u, shown) {
            assert_eq!(hevc::shown_size(&cropped), Some(shown), "the crop shows another size");
        }
    }
    let mut rewritten = unit.to_vec();
    match crop_access_unit(&mut rewritten, shown) {
        Ok(()) if well_formed => {
            assert!(nal::check(&rewritten).is_ok(), "the rewrite broke the lengths");
        }
        Ok(()) => {}
        Err(_) => assert_eq!(rewritten, unit, "a failed rewrite changed the unit"),
    }
}
