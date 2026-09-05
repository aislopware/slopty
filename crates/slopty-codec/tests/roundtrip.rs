//! Hardware encode → decode round trip on the machine's VideoToolbox (macOS only).

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    #![expect(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        reason = "test fixture arithmetic on small, bounded values"
    )]

    use std::ptr::{self, NonNull};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
        CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    };
    use slopty_codec::annexb::{hevc, nal_units};
    use slopty_codec::{Decoder, Encoder, EncoderConfig, FrameOptions};
    use slopty_proto::screen::VideoCodec;

    const W: usize = 640;
    const H: usize = 360;

    /// An NV12 frame with a moving gradient so consecutive frames differ.
    fn frame(index: usize) -> CFRetained<CVPixelBuffer> {
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: the out-pointer is valid; no attributes dictionary is passed.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                W,
                H,
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0, "CVPixelBufferCreate");
        // SAFETY: `CVPixelBufferCreate` returned a +1 reference.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        // SAFETY: the buffer is valid and not locked yet.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0);
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            let rows = if plane == 0 { H } else { H / 2 };
            for y in 0..rows {
                for x in 0..W {
                    let v = if plane == 0 { ((x + y + index * 7) % 220 + 16) as u8 } else { 128 };
                    // SAFETY: the plane is locked, `y < rows` and `x < W <= stride`, so the
                    // offset is inside the plane's mapped bytes.
                    let cell = unsafe { base.add(y * stride + x) };
                    // SAFETY: `cell` points at one writable byte of the locked plane.
                    unsafe { cell.write(v) }
                }
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0);
        buffer
    }

    #[test]
    fn hevc_encode_then_decode() {
        let (ptx, prx) = mpsc::channel();
        let encoder = Encoder::new(
            EncoderConfig {
                width: W as u32,
                height: H as u32,
                codec: VideoCodec::Hevc,
                fps: 60,
                bitrate_bps: 4_000_000,
                rate_control: slopty_codec::RateControl::LowLatency,
            },
            move |packet| {
                let _sent = ptx.send((Instant::now(), packet));
            },
        )
        .expect("hardware HEVC encoder");
        let frames = 30;
        let mut submitted = Vec::with_capacity(frames);
        for i in 0..frames {
            let opts = FrameOptions { force_keyframe: i == 0, ..FrameOptions::default() };
            submitted.push(Instant::now());
            encoder.encode(&frame(i), (i as u64) * 16_667, &opts).expect("encode");
        }
        encoder.flush().expect("flush");
        let mut packets = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while packets.len() < frames {
            let left = deadline.saturating_duration_since(Instant::now());
            match prx.recv_timeout(left) {
                Ok(p) => packets.push(p),
                Err(_) => break,
            }
        }
        assert!(packets.len() >= frames - 1, "got {} packets", packets.len());
        let first = &packets[0].1;
        assert!(first.keyframe, "first packet is an IDR");
        let types: Vec<u8> = nal_units(&first.data).filter_map(hevc::nal_type).collect();
        assert!(types.contains(&hevc::VPS) && types.contains(&hevc::SPS), "{types:?}");
        assert!(types.contains(&hevc::PPS), "{types:?}");
        assert!(types.iter().any(|t| matches!(*t, hevc::IDR_W_RADL | hevc::IDR_N_LP)), "{types:?}");
        assert!(packets[1..].iter().all(|(_, p)| !p.keyframe), "no unrequested keyframes");
        assert!(packets.windows(2).all(|w| w[0].1.pts_us < w[1].1.pts_us), "pts monotonic");
        if encoder.ltr_enabled() {
            assert!(packets.iter().any(|(_, p)| p.ltr_token.is_some()), "LTR tokens flow");
        }
        let latencies: Vec<Duration> = packets
            .iter()
            .enumerate()
            .map(|(i, (at, _))| at.saturating_duration_since(submitted[i.min(frames - 1)]))
            .collect();
        let max = latencies.iter().max().copied().unwrap_or_default();
        eprintln!("encode latency max {max:?}, first packet {} bytes", first.data.len());

        let (dtx, drx) = mpsc::channel();
        let mut decoder = Decoder::new(VideoCodec::Hevc, move |f| {
            let _sent = dtx.send((f.image.width(), f.image.height(), f.pts_us));
        });
        for (_, p) in &packets {
            decoder.decode(&p.data, p.pts_us).expect("decode");
        }
        let mut decoded = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while decoded.len() < packets.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            match drx.recv_timeout(left) {
                Ok(f) => decoded.push(f),
                Err(_) => break,
            }
        }
        assert!(
            decoded.len() + 2 >= packets.len(),
            "decoded {} of {}",
            decoded.len(),
            packets.len()
        );
        assert!(decoded.iter().all(|&(w, h, _)| (w, h) == (W, H)), "{decoded:?}");
        assert!(decoder.ready());
    }

    #[test]
    fn decoder_rejects_p_frames_before_parameter_sets() {
        let mut decoder = Decoder::new(VideoCodec::Hevc, |_f| {});
        let err = decoder.decode(&[0, 0, 0, 1, 0x02, 0x01, 0xAA], 0).unwrap_err();
        assert!(matches!(err, slopty_codec::CodecError::NoParameterSets), "{err}");
    }
}
