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

    /// How an SPS samples its pictures (H.265 7.4.3.2).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct SampleFormat {
        /// 1 for 4:2:0, 2 for 4:2:2, 3 for 4:4:4 (0 is monochrome).
        pub chroma_format_idc: u32,
        /// Bits per luma sample.
        pub bit_depth: u32,
    }

    /// The sample format of an SPS NAL unit (header included); `None` for anything that is not
    /// a well-formed SPS.
    #[must_use]
    pub fn sample_format(nal: &[u8]) -> Option<SampleFormat> {
        if nal_type(nal)? != SPS {
            return None;
        }
        let mut bits = Rbsp::new(nal.get(2..)?);
        // sps_video_parameter_set_id u(4), sps_max_sub_layers_minus1 u(3), nesting flag u(1).
        bits.skip(4)?;
        let sub_layers = usize::try_from(bits.read(3)?).ok()?;
        bits.skip(1)?;
        // profile_tier_level: the general profile and level, 88 + 8 bits (H.265 7.3.3).
        bits.skip(96)?;
        let mut present = [(false, false); 7];
        for layer in present.iter_mut().take(sub_layers) {
            *layer = (bits.flag()?, bits.flag()?);
        }
        if sub_layers > 0 {
            bits.skip(2_usize.checked_mul(8_usize.checked_sub(sub_layers)?)?)?;
        }
        for &(profile, level) in present.iter().take(sub_layers) {
            if profile {
                bits.skip(88)?;
            }
            if level {
                bits.skip(8)?;
            }
        }
        bits.exp_golomb()?; // sps_seq_parameter_set_id
        let chroma_format_idc = bits.exp_golomb()?;
        if chroma_format_idc == 3 {
            bits.skip(1)?; // separate_colour_plane_flag
        }
        bits.exp_golomb()?; // pic_width_in_luma_samples
        bits.exp_golomb()?; // pic_height_in_luma_samples
        if bits.flag()? {
            for _ in 0..4 {
                bits.exp_golomb()?; // conformance window offsets
            }
        }
        let bit_depth = bits.exp_golomb()?.checked_add(8)?;
        Some(SampleFormat { chroma_format_idc, bit_depth })
    }

    /// An RBSP read bit by bit, emulation-prevention bytes (`00 00 03`) dropped.
    struct Rbsp<'a> {
        bytes: &'a [u8],
        byte: usize,
        bit: u32,
        zeros: u8,
        current: u8,
    }

    impl<'a> Rbsp<'a> {
        const fn new(bytes: &'a [u8]) -> Self {
            Self { bytes, byte: 0, bit: 8, zeros: 0, current: 0 }
        }

        fn next_byte(&mut self) -> Option<u8> {
            let mut b = *self.bytes.get(self.byte)?;
            self.byte = self.byte.checked_add(1)?;
            if self.zeros >= 2 && b == 3 {
                self.zeros = 0;
                b = *self.bytes.get(self.byte)?;
                self.byte = self.byte.checked_add(1)?;
            }
            self.zeros = if b == 0 { self.zeros.saturating_add(1) } else { 0 };
            Some(b)
        }

        fn flag(&mut self) -> Option<bool> {
            if self.bit == 8 {
                self.current = self.next_byte()?;
                self.bit = 0;
            }
            let set = self.current.checked_shl(self.bit)? & 0x80 != 0;
            self.bit = self.bit.checked_add(1)?;
            Some(set)
        }

        fn read(&mut self, n: u32) -> Option<u32> {
            (0..n).try_fold(0_u32, |acc, _| Some(acc.checked_shl(1)? | u32::from(self.flag()?)))
        }

        fn skip(&mut self, n: usize) -> Option<()> {
            (0..n).try_for_each(|_| self.flag().map(drop))
        }

        /// An unsigned Exp-Golomb code, H.265 9.2.
        fn exp_golomb(&mut self) -> Option<u32> {
            let mut zeros = 0_u32;
            while !self.flag()? {
                zeros = zeros.checked_add(1).filter(|z| *z < 32)?;
            }
            1_u32.checked_shl(zeros)?.checked_sub(1)?.checked_add(self.read(zeros)?)
        }
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

    /// SPSs the M1 Max's low-latency encoder wrote at 320×180 (2026-09-28) for Main, Main 4:4:4
    /// and Main 4:4:4 10: two temporal sub-layers and emulation-prevention bytes, so the parser
    /// walks both.
    const SPS_420: [u8; 86] = [
        0x42, 0x01, 0x03, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xb0, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x3f, 0x00, 0x00, 0xa0, 0x0a, 0x08, 0x0c, 0x1f, 0x3e, 0x20, 0x10, 0xee, 0x45,
        0x20, 0x82, 0xe7, 0xe1, 0x3d, 0x0b, 0xea, 0x1b, 0xd5, 0x0f, 0xea, 0xa0, 0x8f, 0x55, 0x41,
        0x3e, 0xaa, 0xa0, 0xaf, 0x55, 0x54, 0x17, 0xea, 0xaa, 0xa0, 0xcf, 0x55, 0x55, 0x41, 0xbe,
        0xaa, 0xaa, 0xa0, 0xef, 0x55, 0x55, 0x54, 0x1f, 0xea, 0xaa, 0xaa, 0xa0, 0x43, 0xd5, 0x55,
        0x55, 0x52, 0x9b, 0x81, 0x01, 0x00, 0x81, 0xfc, 0x20, 0x10, 0x40,
    ];
    const SPS_444: [u8; 85] = [
        0x42, 0x01, 0x03, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0xbe, 0x08, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x5d, 0x00, 0x00, 0x90, 0x01, 0x41, 0x01, 0x83, 0xe3, 0x71, 0x00, 0x87, 0x72, 0x29,
        0x04, 0x17, 0x3f, 0x09, 0xe8, 0x5f, 0x50, 0xde, 0xa8, 0x7f, 0x55, 0x04, 0x7a, 0xaa, 0x09,
        0xf5, 0x55, 0x05, 0x7a, 0xaa, 0xa0, 0xbf, 0x55, 0x55, 0x06, 0x7a, 0xaa, 0xaa, 0x0d, 0xf5,
        0x55, 0x55, 0x07, 0x7a, 0xaa, 0xaa, 0xa0, 0xff, 0x55, 0x55, 0x55, 0x02, 0x1e, 0xaa, 0xaa,
        0xaa, 0x94, 0xdc, 0x08, 0x08, 0x04, 0x0f, 0xe1, 0x00, 0x82,
    ];

    const SPS_444_10: [u8; 86] = [
        0x42, 0x01, 0x03, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0xbc, 0x08, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x5d, 0x00, 0x00, 0x90, 0x01, 0x41, 0x01, 0x83, 0xe3, 0x5b, 0x10, 0x08, 0x77, 0x22,
        0x90, 0x41, 0x73, 0xf0, 0x9e, 0x85, 0xf5, 0x0d, 0xea, 0x87, 0xf5, 0x50, 0x47, 0xaa, 0xa0,
        0x9f, 0x55, 0x50, 0x57, 0xaa, 0xaa, 0x0b, 0xf5, 0x55, 0x50, 0x67, 0xaa, 0xaa, 0xa0, 0xdf,
        0x55, 0x55, 0x50, 0x77, 0xaa, 0xaa, 0xaa, 0x0f, 0xf5, 0x55, 0x55, 0x50, 0x21, 0xea, 0xaa,
        0xaa, 0xa9, 0x4d, 0xc0, 0x80, 0x80, 0x40, 0xfe, 0x10, 0x08, 0x20,
    ];

    #[test]
    fn the_sps_says_which_chroma_format_the_stream_is() {
        let format = |chroma_format_idc, bit_depth| {
            Some(hevc::SampleFormat { chroma_format_idc, bit_depth })
        };
        assert_eq!(hevc::sample_format(&SPS_420), format(1, 8), "Main");
        assert_eq!(hevc::sample_format(&SPS_444), format(3, 8), "Main 4:4:4");
        assert_eq!(hevc::sample_format(&SPS_444_10), format(3, 10), "Main 4:4:4 10");
        let pps = [0x44, 0x01, 0xc0, 0x72, 0xf0, 0x5b, 0x24];
        assert_eq!(hevc::sample_format(&pps), None, "not an SPS");
        assert_eq!(hevc::sample_format(&SPS_444[..12]), None, "cut off in the profile");
        assert_eq!(hevc::sample_format(&SPS_444[..24]), None, "cut off before the bit depth");
        assert_eq!(hevc::sample_format(&[]), None);
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
