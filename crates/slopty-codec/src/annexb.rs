//! Annex B ↔ length-prefixed NAL unit framing, pure and testable.
//!
//! VideoToolbox produces and consumes *length-prefixed* NAL units (4-byte big-endian sizes, as
//! in `hvcC`/`avcC`) with the parameter sets kept out of band in the format description. On the
//! wire Slopty uses Annex B (`00 00 00 01` start codes, parameter sets inline before every
//! keyframe) because it is self-describing: a receiver can (re)build its decoder from any
//! keyframe it manages to reassemble.

use crate::CodecError;

/// A four-byte start code.
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// HEVC NAL unit types that carry parameter sets.
pub mod hevc {
    /// Video parameter set.
    pub const VPS: u8 = 32;
    /// Sequence parameter set.
    pub const SPS: u8 = 33;
    /// Picture parameter set.
    pub const PPS: u8 = 34;
    /// IDR with RADL pictures.
    pub const IDR_W_RADL: u8 = 19;
    /// IDR without leading pictures.
    pub const IDR_N_LP: u8 = 20;

    /// The type of an HEVC NAL unit: bits 1..7 of the first header byte.
    #[must_use]
    pub fn nal_type(nal: &[u8]) -> Option<u8> {
        nal.first().map(|b| (b >> 1) & 0x3f)
    }

    /// True for VPS/SPS/PPS.
    #[must_use]
    pub fn is_parameter_set(nal: &[u8]) -> bool {
        matches!(nal_type(nal), Some(VPS | SPS | PPS))
    }
}

/// H.264 NAL unit types that carry parameter sets.
pub mod h264 {
    /// Sequence parameter set.
    pub const SPS: u8 = 7;
    /// Picture parameter set.
    pub const PPS: u8 = 8;
    /// IDR slice.
    pub const IDR: u8 = 5;

    /// The type of an H.264 NAL unit: low five bits of the first header byte.
    #[must_use]
    pub fn nal_type(nal: &[u8]) -> Option<u8> {
        nal.first().map(|b| b & 0x1f)
    }

    /// True for SPS/PPS.
    #[must_use]
    pub fn is_parameter_set(nal: &[u8]) -> bool {
        matches!(nal_type(nal), Some(SPS | PPS))
    }
}

/// Iterate the NAL units of an Annex B stream (3- or 4-byte start codes), without the start
/// codes. Trailing zero bytes before a start code belong to the start code, not the NAL.
pub fn nal_units(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = find_start_code(stream)
        .and_then(|(at, len)| stream.get(at.saturating_add(len)..))
        .unwrap_or(&[]);
    std::iter::from_fn(move || {
        while !rest.is_empty() {
            let current = rest;
            let (nal, next) = match find_start_code(current) {
                Some((at, len)) => {
                    (current.get(..at).unwrap_or(&[]), current.get(at.saturating_add(len)..))
                }
                None => (current, None),
            };
            rest = next.unwrap_or(&[]);
            let nal = trim_trailing_zeros(nal);
            if !nal.is_empty() {
                return Some(nal);
            }
        }
        None
    })
}

/// Position and length of the next start code (`00 00 01` or `00 00 00 01`).
fn find_start_code(buf: &[u8]) -> Option<(usize, usize)> {
    static THREE: std::sync::LazyLock<memchr::memmem::Finder<'static>> =
        std::sync::LazyLock::new(|| memchr::memmem::Finder::new(&[0, 0, 1]));
    let i = THREE.find(buf)?;
    // Fold a preceding zero into a four-byte code.
    let four = i > 0 && buf.get(i.wrapping_sub(1)) == Some(&0);
    Some(if four { (i.wrapping_sub(1), 4) } else { (i, 3) })
}

fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let end = nal.iter().rposition(|&b| b != 0).map_or(0, |p| p.saturating_add(1));
    nal.get(..end).unwrap_or(&[])
}

/// Rewrite the 4-byte big-endian length prefixes of an access unit into start codes, in place:
/// both are four bytes, so VideoToolbox's output becomes Annex B without moving a byte.
///
/// A length that runs past the end, or a tail too short for a prefix, is an error: the bytes
/// are not an access unit and nothing of them should reach a decoder.
pub fn length_prefixed_to_annexb_in_place(buf: &mut [u8]) -> Result<(), CodecError> {
    let mut at = 0_usize;
    while at < buf.len() {
        let malformed = CodecError::MalformedNal { offset: at };
        let prefix = buf.get_mut(at..at.saturating_add(4)).ok_or(malformed)?;
        let len = prefix.iter().fold(0_usize, |acc, &b| (acc << 8) | usize::from(b));
        prefix.copy_from_slice(&START_CODE);
        let next = at.saturating_add(4).checked_add(len).ok_or(malformed)?;
        if next > buf.len() {
            return Err(malformed);
        }
        at = next;
    }
    Ok(())
}

/// One Annex B access unit split in a single scan: the parameter sets it carries, and the
/// units the decoder is given as 4-byte length-prefixed NAL units.
#[derive(Debug)]
pub struct AccessUnit<'a> {
    parameter_sets: Vec<&'a [u8]>,
    units: Vec<&'a [u8]>,
}

impl<'a> AccessUnit<'a> {
    /// Scan `stream` once.
    #[must_use]
    pub fn parse(stream: &'a [u8], is_parameter_set: fn(&[u8]) -> bool) -> Self {
        let (parameter_sets, units) = nal_units(stream).partition(|nal| is_parameter_set(nal));
        Self { parameter_sets, units }
    }

    /// The parameter sets, in stream order.
    #[must_use]
    pub fn parameter_sets(&self) -> &[&'a [u8]] {
        &self.parameter_sets
    }

    /// Bytes [`Self::write_length_prefixed`] writes; zero when the unit carries no picture.
    #[must_use]
    pub fn length_prefixed_len(&self) -> usize {
        self.units.iter().fold(0_usize, |sum, nal| sum.saturating_add(4).saturating_add(nal.len()))
    }

    /// Write the non-parameter-set units into `out`, each behind its 4-byte length. `out` is
    /// exactly [`Self::length_prefixed_len`] bytes long.
    pub fn write_length_prefixed(&self, out: &mut [u8]) -> Result<(), CodecError> {
        let mut rest = out;
        for nal in &self.units {
            let len = u32::try_from(nal.len())
                .map_err(|_too_long| CodecError::MalformedNal { offset: 0 })?;
            let (head, tail) =
                rest.split_at_mut_checked(4).ok_or(CodecError::MalformedNal { offset: 0 })?;
            head.copy_from_slice(&len.to_be_bytes());
            let (body, tail) = tail
                .split_at_mut_checked(nal.len())
                .ok_or(CodecError::MalformedNal { offset: 0 })?;
            body.copy_from_slice(nal);
            rest = tail;
        }
        Ok(())
    }
}

/// Prepend parameter sets (each one NAL unit) as Annex B units.
pub fn prepend_parameter_sets<'a>(
    out: &mut Vec<u8>,
    parameter_sets: impl IntoIterator<Item = &'a [u8]>,
) {
    for ps in parameter_sets {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(ps);
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let stream = [0, 0, 0, 1, 0x40, 1, 2, 0, 0, 1, 0x42, 9, 0, 0, 0, 0, 1, 0x44, 0, 0];
        let units: Vec<&[u8]> = nal_units(&stream).collect();
        assert_eq!(units, vec![&[0x40, 1, 2][..], &[0x42, 9], &[0x44]]);
    }

    #[test]
    fn ignores_garbage_before_the_first_start_code_and_empty_streams() {
        assert_eq!(nal_units(&[7, 7, 0, 0, 1, 0x26, 5]).collect::<Vec<_>>(), vec![&[0x26, 5][..]]);
        assert_eq!(nal_units(&[]).count(), 0);
        assert_eq!(nal_units(&[0, 0, 1]).count(), 0);
        assert_eq!(nal_units(&[1, 2, 3]).count(), 0);
    }

    #[test]
    fn hevc_and_h264_types() {
        assert_eq!(hevc::nal_type(&[0x40, 1]), Some(hevc::VPS));
        assert_eq!(hevc::nal_type(&[0x42, 1]), Some(hevc::SPS));
        assert_eq!(hevc::nal_type(&[0x44, 1]), Some(hevc::PPS));
        assert_eq!(hevc::nal_type(&[0x26, 1]), Some(hevc::IDR_W_RADL));
        assert!(hevc::is_parameter_set(&[0x44]));
        assert!(!hevc::is_parameter_set(&[0x26]));
        assert_eq!(h264::nal_type(&[0x67]), Some(h264::SPS));
        assert_eq!(h264::nal_type(&[0x68]), Some(h264::PPS));
        assert_eq!(h264::nal_type(&[0x65]), Some(h264::IDR));
        assert!(h264::is_parameter_set(&[0x68]));
        assert_eq!(hevc::nal_type(&[]), None);
    }

    #[test]
    fn length_prefixes_become_start_codes_in_place_and_back() {
        let avcc = [0, 0, 0, 2, 0x26, 0xaa, 0, 0, 0, 1, 0x02];
        let mut annexb = avcc;
        length_prefixed_to_annexb_in_place(&mut annexb).unwrap();
        assert_eq!(annexb, [0, 0, 0, 1, 0x26, 0xaa, 0, 0, 0, 1, 0x02]);
        let unit = AccessUnit::parse(&annexb, hevc::is_parameter_set);
        let mut back = vec![0; unit.length_prefixed_len()];
        unit.write_length_prefixed(&mut back).unwrap();
        assert_eq!(back, avcc.to_vec());
        let mut empty: [u8; 0] = [];
        length_prefixed_to_annexb_in_place(&mut empty).unwrap();
    }

    /// A length past the end or a tail shorter than a prefix is refused, never truncated.
    #[test]
    fn a_malformed_length_is_an_error() {
        let offset = |buf: &mut [u8]| match length_prefixed_to_annexb_in_place(buf) {
            Err(CodecError::MalformedNal { offset }) => Some(offset),
            _other => None,
        };
        assert_eq!(offset(&mut [0, 0, 0, 9, 1]), Some(0), "runs past the end");
        assert_eq!(offset(&mut [0, 0]), Some(0), "shorter than a prefix");
        assert_eq!(offset(&mut [0, 0, 0, 1, 0x26, 0, 0]), Some(5), "a stub after a good unit");
        assert_eq!(offset(&mut [0xff, 0xff, 0xff, 0xff, 1]), Some(0), "a huge length");
    }

    #[test]
    fn parameter_sets_are_prepended_and_stripped() {
        let mut out = Vec::new();
        prepend_parameter_sets(&mut out, [&[0x40, 1][..], &[0x42, 2], &[0x44, 3]]);
        out.extend_from_slice(&[0, 0, 0, 1, 0x26, 0xff]);
        assert_eq!(nal_units(&out).count(), 4);
        let unit = AccessUnit::parse(&out, hevc::is_parameter_set);
        assert_eq!(unit.parameter_sets(), &[&[0x40, 1][..], &[0x42, 2], &[0x44, 3]]);
        let mut lp = vec![0; unit.length_prefixed_len()];
        unit.write_length_prefixed(&mut lp).unwrap();
        assert_eq!(lp, vec![0, 0, 0, 2, 0x26, 0xff]);
        assert!(unit.write_length_prefixed(&mut [0; 3]).is_err(), "a short buffer is refused");
    }
}
