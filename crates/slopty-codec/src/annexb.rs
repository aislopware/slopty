//! Annex B ↔ length-prefixed NAL unit framing, pure and testable.
//!
//! VideoToolbox produces and consumes *length-prefixed* NAL units (4-byte big-endian sizes, as
//! in `hvcC`/`avcC`) with the parameter sets kept out of band in the format description. On the
//! wire Slopty uses Annex B (`00 00 00 01` start codes, parameter sets inline before every
//! keyframe) because it is self-describing: a receiver can (re)build its decoder from any
//! keyframe it manages to reassemble.

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
        nal.first().map(|b| (b >> 1) & 0x3F)
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
        nal.first().map(|b| b & 0x1F)
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
    let mut rest = stream;
    let mut started = false;
    std::iter::from_fn(move || {
        loop {
            if rest.is_empty() {
                return None;
            }
            if !started {
                // Skip to the first start code.
                let current = rest;
                let Some((at, len)) = find_start_code(current) else {
                    rest = &[];
                    return None;
                };
                rest = current.get(at.saturating_add(len)..).unwrap_or(&[]);
                started = true;
                continue;
            }
            let current = rest;
            let (nal, next) = match find_start_code(current) {
                Some((at, len)) => {
                    let nal = current.get(..at).unwrap_or(&[]);
                    (nal, current.get(at.saturating_add(len)..).unwrap_or(&[]))
                }
                None => (current, &[][..]),
            };
            rest = next;
            let nal = trim_trailing_zeros(nal);
            if !nal.is_empty() {
                return Some(nal);
            }
            if rest.is_empty() {
                return None;
            }
        }
    })
}

/// Position and length of the next start code (`00 00 01` or `00 00 00 01`).
fn find_start_code(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0_usize;
    while let Some(window) = buf.get(i..) {
        if window.len() < 3 {
            return None;
        }
        if window.starts_with(&[0, 0, 1]) {
            // Fold a preceding zero into a four-byte code.
            let four = i > 0 && buf.get(i.wrapping_sub(1)) == Some(&0);
            return Some(if four { (i.wrapping_sub(1), 4) } else { (i, 3) });
        }
        i = i.saturating_add(1);
    }
    None
}

fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let end = nal.iter().rposition(|&b| b != 0).map_or(0, |p| p.saturating_add(1));
    nal.get(..end).unwrap_or(&[])
}

/// Convert length-prefixed NAL units (`nal_length_size` big-endian bytes each) to Annex B.
/// Stops at the first malformed length.
#[must_use]
pub fn length_prefixed_to_annexb(buf: &[u8], nal_length_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len().saturating_add(16));
    let mut rest = buf;
    while rest.len() >= nal_length_size && nal_length_size > 0 {
        let (head, tail) = rest.split_at(nal_length_size);
        let len = head.iter().fold(0_usize, |acc, &b| (acc << 8) | usize::from(b));
        let Some(nal) = tail.get(..len) else { break };
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        rest = tail.get(len..).unwrap_or(&[]);
    }
    out
}

/// Convert Annex B to 4-byte length-prefixed NAL units, dropping parameter sets (they travel in
/// the format description). Returns the units kept.
#[must_use]
pub fn annexb_to_length_prefixed(stream: &[u8], is_parameter_set: fn(&[u8]) -> bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(stream.len());
    for nal in nal_units(stream).filter(|nal| !is_parameter_set(nal)) {
        let len = u32::try_from(nal.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
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
    fn length_prefixed_round_trip() {
        let avcc = [0, 0, 0, 2, 0x26, 0xAA, 0, 0, 0, 1, 0x02];
        let annexb = length_prefixed_to_annexb(&avcc, 4);
        assert_eq!(annexb, vec![0, 0, 0, 1, 0x26, 0xAA, 0, 0, 0, 1, 0x02]);
        let back = annexb_to_length_prefixed(&annexb, hevc::is_parameter_set);
        assert_eq!(back, avcc.to_vec());
        // A truncated length stops conversion without panicking.
        assert_eq!(length_prefixed_to_annexb(&[0, 0, 0, 9, 1], 4), Vec::<u8>::new());
        assert_eq!(length_prefixed_to_annexb(&[0, 0], 4), Vec::<u8>::new());
        assert_eq!(length_prefixed_to_annexb(&[1, 2], 0), Vec::<u8>::new());
    }

    #[test]
    fn parameter_sets_are_prepended_and_stripped() {
        let mut out = Vec::new();
        prepend_parameter_sets(&mut out, [&[0x40, 1][..], &[0x42, 2], &[0x44, 3]]);
        out.extend_from_slice(&[0, 0, 0, 1, 0x26, 0xFF]);
        assert_eq!(nal_units(&out).count(), 4);
        let lp = annexb_to_length_prefixed(&out, hevc::is_parameter_set);
        assert_eq!(lp, vec![0, 0, 0, 2, 0x26, 0xFF]);
    }
}
