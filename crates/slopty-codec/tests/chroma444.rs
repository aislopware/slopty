//! What VideoToolbox on this Mac offers past 4:2:0 HEVC, and what it costs (macOS only).
//!
//! Streams are 4:2:0, so coloured text on a streamed desktop is softer than a 4:4:4 stream
//! makes it. This probe asks the framework directly: the encoder list, each HEVC encoder's
//! supported `ProfileLevel` values, sessions fed 4:2:2 and 4:4:4 pictures under every profile,
//! the chroma format the resulting SPS actually signals, and whether the decoder takes the
//! stream in hardware. Then it times encodes at 1080p and 5K and prices a synthetic coloured
//! text frame in bits and PSNR. It opens no window and captures nothing. `MEASURE` lines go to
//! stderr; `docs/MEASUREMENTS.md` ("4:4:4 HEVC on the low-latency encoder") records a run.

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::cast_possible_wrap,
        clippy::cast_lossless,
        clippy::too_many_lines,
        clippy::many_single_char_names,
        reason = "measurement fixture: pixel arithmetic on small, bounded values"
    )]

    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::{
        CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
    };
    use objc2_core_media::{
        CMFormatDescription, CMSampleBuffer, CMTime, CMTimeFlags,
        CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMTimeInvalid, kCMVideoCodecType_HEVC,
    };
    use objc2_core_video::{
        CVImageBuffer, CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress,
        CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRow,
        CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetPixelFormatType,
        CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey,
        kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
        kCVPixelFormatType_422YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_422YpCbCr10BiPlanarFullRange,
        kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
    };
    use objc2_video_toolbox::{
        VTCompressionSession, VTCopySupportedPropertyDictionaryForEncoder, VTCopyVideoEncoderList,
        VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
        VTDecompressionSession, VTEncodeInfoFlags, VTIsHardwareDecodeSupported, VTSession,
        VTSessionCopyProperty, VTSessionSetProperty,
        kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
        kVTCompressionPropertyKey_EnableLTR, kVTCompressionPropertyKey_ExpectedFrameRate,
        kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
        kVTCompressionPropertyKey_RealTime,
        kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
        kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
        kVTProfileLevel_HEVC_Main_AutoLevel, kVTPropertySupportedValueListKey,
        kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
        kVTVideoEncoderList_CodecType, kVTVideoEncoderList_EncoderID,
        kVTVideoEncoderList_IsHardwareAccelerated,
        kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
        kVTVideoEncoderSpecification_EncoderID,
        kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
    };
    use slopty_codec::annexb::hevc;
    use slopty_testkit::stats::Spread;

    // ---- CoreFoundation plumbing -------------------------------------------------------------

    type Dict = CFDictionary<CFString, CFType>;

    fn dict(pairs: &[(&CFString, &CFType)]) -> CFRetained<Dict> {
        let keys: Vec<&CFString> = pairs.iter().map(|p| p.0).collect();
        let values: Vec<&CFType> = pairs.iter().map(|p| p.1).collect();
        Dict::from_slices(&keys, &values)
    }

    fn yes() -> &'static CFType {
        CFBoolean::new(true)
    }

    fn fourcc(format: u32) -> String {
        format.to_be_bytes().iter().map(|&b| char::from(b)).collect()
    }

    fn as_session(object: &CFType) -> &VTSession {
        // SAFETY: VTSession.h: compression and decompression sessions are `VTSessionRef`s;
        // the reference is only read and lives as long as `object`.
        unsafe { NonNull::from(object).cast::<VTSession>().as_ref() }
    }

    fn set(session: &CFType, key: &CFString, value: &CFType) -> i32 {
        // SAFETY: valid session, key and value for the duration of the call.
        unsafe { VTSessionSetProperty(as_session(session), key, Some(value)) }
    }

    fn copy_bool(session: &CFType, key: &CFString) -> Option<bool> {
        let mut out: *const CFType = ptr::null();
        // SAFETY: VTSession.h: the value comes back +1 through a `CFTypeRef *` out pointer.
        let status = unsafe {
            VTSessionCopyProperty(as_session(session), key, None, (&raw mut out).cast::<c_void>())
        };
        let raw = NonNull::new(out.cast_mut()).filter(|_| status == 0)?;
        // SAFETY: +1 reference from the copy call (Copy rule).
        let value: CFRetained<CFType> = unsafe { CFRetained::from_raw(raw) };
        value.downcast::<CFBoolean>().ok().map(|b| b.as_bool())
    }

    fn text(value: &CFType) -> String {
        value.downcast_ref::<CFString>().map_or_else(|| format!("{value:?}"), ToString::to_string)
    }

    /// The `CFString`s of a `CFArray` value, as the framework handed them over.
    fn strings(value: &CFType) -> Vec<CFRetained<CFString>> {
        value
            .downcast_ref::<CFArray>()
            .map(|array| {
                // SAFETY: CFArray of CFTypes; elements are read, not mutated.
                let array: &CFArray<CFType> = unsafe { array.cast_unchecked() };
                array.iter().filter_map(|v| v.downcast::<CFString>().ok()).collect()
            })
            .unwrap_or_default()
    }

    // ---- The encoder list and the per-encoder property dictionaries --------------------------

    fn encoder_list() -> Vec<(String, bool)> {
        let mut raw: *const CFArray = ptr::null();
        // SAFETY: valid out pointer; the list comes back +1 (Copy rule).
        let status = unsafe { VTCopyVideoEncoderList(None, NonNull::from(&mut raw)) };
        assert_eq!(status, 0, "VTCopyVideoEncoderList");
        // SAFETY: +1 reference; VTVideoEncoderList.h: an array of CFDictionaries keyed by CFString.
        let list: CFRetained<CFArray<Dict>> =
            unsafe { CFRetained::from_raw(NonNull::new(raw.cast_mut()).unwrap().cast()) };
        let mut out = Vec::new();
        for entry in list.iter() {
            // SAFETY: framework-provided constant strings.
            let (codec_key, id_key, hw_key) = unsafe {
                (
                    kVTVideoEncoderList_CodecType,
                    kVTVideoEncoderList_EncoderID,
                    kVTVideoEncoderList_IsHardwareAccelerated,
                )
            };
            let codec = entry
                .get(codec_key)
                .and_then(|v| v.downcast::<CFNumber>().ok())
                .and_then(|n| n.as_i64())
                .unwrap_or(0) as u32;
            if codec != kCMVideoCodecType_HEVC {
                continue;
            }
            let id = entry.get(id_key).map(|v| text(&v)).unwrap_or_default();
            let hw = entry
                .get(hw_key)
                .and_then(|v| v.downcast::<CFBoolean>().ok())
                .is_some_and(|b| b.as_bool());
            out.push((id, hw));
        }
        out
    }

    /// `(status, encoder id, ProfileLevel's supported values, every supported key)`.
    fn supported_for(
        width: i32,
        height: i32,
        spec: &Dict,
    ) -> (i32, String, Vec<CFRetained<CFString>>, Vec<String>) {
        let mut id: *const CFString = ptr::null();
        let mut props: *const CFDictionary = ptr::null();
        // SAFETY: valid out pointers; both results come back +1 (Copy rule).
        let status = unsafe {
            VTCopySupportedPropertyDictionaryForEncoder(
                width,
                height,
                kCMVideoCodecType_HEVC,
                Some(spec.as_opaque()),
                &raw mut id,
                &raw mut props,
            )
        };
        // SAFETY: +1 references from the copy call when non-null.
        let id = NonNull::new(id.cast_mut())
            .map(|p| unsafe { CFRetained::from_raw(p) }.to_string())
            .unwrap_or_default();
        let Some(props) = NonNull::new(props.cast_mut()) else {
            return (status, id, Vec::new(), Vec::new());
        };
        // SAFETY: +1 reference; VTSession.h: keyed by CFString, values are CFDictionaries.
        let props: CFRetained<Dict> = unsafe { CFRetained::from_raw(props.cast()) };
        // SAFETY: framework-provided constant strings.
        let (profile_key, list_key) =
            unsafe { (kVTCompressionPropertyKey_ProfileLevel, kVTPropertySupportedValueListKey) };
        let profiles = props
            .get(profile_key)
            .and_then(|v| v.downcast::<CFDictionary>().ok())
            .and_then(|d| {
                // SAFETY: the per-property dictionary is keyed by CFString (VTSession.h).
                let d: &Dict = unsafe { d.cast_unchecked() };
                d.get(list_key).map(|list| strings(&list))
            })
            .unwrap_or_default();
        let (keys, _values) = props.to_vecs();
        let mut keys: Vec<String> = keys.iter().map(ToString::to_string).collect();
        keys.sort_unstable();
        (status, id, profiles, keys)
    }

    // ---- Synthetic coloured text -------------------------------------------------------------

    /// Monokai on its background: saturated hues on a dark ground, the worst case for 4:2:0.
    const PALETTE: [[u8; 3]; 7] = [
        [249, 38, 114],
        [166, 226, 46],
        [102, 217, 239],
        [253, 151, 31],
        [174, 129, 255],
        [230, 219, 116],
        [248, 248, 242],
    ];
    const BACKGROUND: [u8; 3] = [39, 40, 34];

    fn hash(mut x: u64) -> u64 {
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^ (x >> 33)
    }

    /// A full-range BT.709 4:4:4 picture of a code editor: 8×16 cells, glyphs of one-pixel
    /// strokes, tokens coloured from the palette, ragged line ends. `scroll` shifts it up by
    /// that many pixels so consecutive frames differ the way a scrolling terminal does.
    struct Picture {
        w: usize,
        h: usize,
        y: Vec<f32>,
        cb: Vec<f32>,
        cr: Vec<f32>,
        rgb: Vec<[u8; 3]>,
    }

    fn ycbcr(rgb: [u8; 3]) -> [f32; 3] {
        let [r, g, b] = rgb.map(|c| f32::from(c) / 255.0);
        let y = 0.0722_f32.mul_add(b, 0.2126_f32.mul_add(r, 0.7152 * g));
        let cb = (b - y) / 1.8556;
        let cr = (r - y) / 1.5748;
        [y * 255.0, cb.mul_add(255.0, 128.0), cr.mul_add(255.0, 128.0)]
    }

    fn picture(w: usize, h: usize, scroll: usize) -> Picture {
        let bg = ycbcr(BACKGROUND);
        let colours: Vec<([f32; 3], [u8; 3])> = PALETTE.iter().map(|&c| (ycbcr(c), c)).collect();
        let mut p = Picture {
            w,
            h,
            y: vec![bg[0]; w * h],
            cb: vec![bg[1]; w * h],
            cr: vec![bg[2]; w * h],
            rgb: vec![BACKGROUND; w * h],
        };
        let cols = w / 8;
        for py in 0..h {
            let sy = py + scroll;
            let (line, gy) = (sy / 16, sy % 16);
            let line_len = (hash(line as u64) % (cols as u64)) as usize;
            if !(2..14).contains(&gy) {
                continue;
            }
            for col in 0..cols.min(line_len) {
                let token = hash(((line as u64) << 20) | (col as u64 / 5));
                if token.is_multiple_of(7) {
                    continue; // a space between tokens
                }
                let glyph = hash(((line as u64) << 32) | col as u64 | (1 << 60));
                let colour = colours[(token % colours.len() as u64) as usize];
                for gx in 1..7 {
                    // A 6×12 glyph of strokes: two verticals, three horizontals, one diagonal.
                    let v1 = gx == 1 + (glyph % 3) as usize;
                    let v2 = gx == 4 + (glyph >> 2) as usize % 3;
                    let row = gy - 2;
                    let h1 = row == (glyph >> 4) as usize % 4;
                    let h2 = row == 5 + (glyph >> 6) as usize % 2;
                    let h3 = row == 11;
                    let d = (glyph >> 8).is_multiple_of(2) && row / 2 == gx - 1;
                    let on = (v1 && (glyph >> 9).is_multiple_of(2))
                        || (v2 && !(glyph >> 10).is_multiple_of(3))
                        || (h1 && (glyph >> 11).is_multiple_of(2))
                        || (h2 && !(glyph >> 12).is_multiple_of(3))
                        || (h3 && (glyph >> 13).is_multiple_of(2))
                        || d;
                    if on {
                        let i = py * w + col * 8 + gx;
                        [p.y[i], p.cb[i], p.cr[i]] = colour.0;
                        p.rgb[i] = colour.1;
                    }
                }
            }
        }
        p
    }

    /// `(horizontal, vertical)` chroma subsampling and bit depth of a bi-planar format.
    fn layout(format: u32) -> (usize, usize, u32) {
        match format {
            f if f == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange => (2, 2, 8),
            f if f == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange => (2, 2, 10),
            f if f == kCVPixelFormatType_422YpCbCr8BiPlanarFullRange => (2, 1, 8),
            f if f == kCVPixelFormatType_422YpCbCr10BiPlanarFullRange => (2, 1, 10),
            f if f == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange => (1, 1, 8),
            f if f == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange => (1, 1, 10),
            other => panic!("no layout for {}", fourcc(other)),
        }
    }

    fn buffer(w: usize, h: usize, format: u32) -> CFRetained<CVPixelBuffer> {
        let attrs = dict(&[(
            // SAFETY: framework-provided constant string.
            unsafe { kCVPixelBufferIOSurfacePropertiesKey },
            &Dict::empty(),
        )]);
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo rule: a valid out-pointer and a CFString-keyed attributes dictionary.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                w,
                h,
                format,
                Some(attrs.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0, "CVPixelBufferCreate {}", fourcc(format));
        // SAFETY: +1 reference from the create call.
        unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) }
    }

    /// Run `f(plane, base, stride)` over each plane of a locked buffer.
    fn with_planes(
        image: &CVPixelBuffer,
        read_only: bool,
        mut f: impl FnMut(usize, *mut u8, usize),
    ) {
        let flags = if read_only {
            CVPixelBufferLockFlags::ReadOnly
        } else {
            CVPixelBufferLockFlags::empty()
        };
        // SAFETY: CoreVideo rule: lock before touching the planes.
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(image, flags) }, 0, "lock");
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(image, plane).cast::<u8>();
            f(plane, base, CVPixelBufferGetBytesPerRowOfPlane(image, plane));
        }
        // SAFETY: matches the lock above.
        assert_eq!(unsafe { CVPixelBufferUnlockBaseAddress(image, flags) }, 0, "unlock");
    }

    /// The picture in `format`, chroma box-filtered to the format's subsampling the way a
    /// capture that delivers that format would.
    /// The picture as packed BGRA, what ScreenCaptureKit delivers by default.
    fn fill_bgra(p: &Picture) -> CFRetained<CVPixelBuffer> {
        let image = buffer(p.w, p.h, kCVPixelFormatType_32BGRA);
        // SAFETY: CoreVideo rule: lock before touching the pixels.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0, "lock");
        let base = CVPixelBufferGetBaseAddress(&image).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(&image);
        for y in 0..p.h {
            // SAFETY: the buffer is locked; row `y < h` starts inside it.
            let start = unsafe { base.add(y * stride) };
            // SAFETY: the row spans `4 * w <= stride` writable bytes.
            let row = unsafe { std::slice::from_raw_parts_mut(start, 4 * p.w) };
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let [r, g, b] = p.rgb[y * p.w + x];
                *px = [b, g, r, 255];
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0, "unlock");
        image
    }

    fn fill(p: &Picture, format: u32) -> CFRetained<CVPixelBuffer> {
        if format == kCVPixelFormatType_32BGRA {
            return fill_bgra(p);
        }
        let (sx, sy, bits) = layout(format);
        let image = buffer(p.w, p.h, format);
        let scale = if bits == 8 { 1.0 } else { 4.0 };
        with_planes(&image, false, |plane, base, stride| {
            let (cw, ch) = if plane == 0 { (p.w, p.h) } else { (p.w / sx, p.h / sy) };
            for y in 0..ch {
                for x in 0..cw {
                    let values: [f32; 2] = if plane == 0 {
                        [p.y[y * p.w + x], 0.0]
                    } else {
                        let mut acc = [0.0_f32; 2];
                        for dy in 0..sy {
                            for dx in 0..sx {
                                let i = (y * sy + dy) * p.w + x * sx + dx;
                                acc[0] += p.cb[i];
                                acc[1] += p.cr[i];
                            }
                        }
                        acc.map(|a| a / (sx * sy) as f32)
                    };
                    let count = if plane == 0 { 1 } else { 2 };
                    for (k, v) in values.iter().take(count).enumerate() {
                        let q = (v * scale).round().clamp(0.0, 255.0 * scale) as u16;
                        let at = if plane == 0 { x } else { x * 2 + k };
                        if bits == 8 {
                            // SAFETY: the plane is locked; `y < ch` rows of `stride` bytes and
                            // `at < stride` stay inside it.
                            let cell = unsafe { base.add(y * stride + at) };
                            // SAFETY: one writable byte of the locked plane.
                            unsafe { cell.write(q as u8) }
                        } else {
                            // 10 bits in the high bits of a little-endian u16 (CVPixelBuffer.h).
                            let bytes = (q << 6).to_le_bytes();
                            // SAFETY: as above, two bytes per sample.
                            let cell = unsafe { base.add(y * stride + at * 2) };
                            // SAFETY: two writable bytes of the locked plane.
                            unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), cell, 2) }
                        }
                    }
                }
            }
        });
        image
    }

    /// PSNR of the decoded picture against the 4:4:4 source, `(luma, chroma)` in dB. Chroma is
    /// read at the decoded buffer's own subsampling and replicated to full resolution, the way
    /// a nearest sampler would show it.
    fn psnr(p: &Picture, image: &CVPixelBuffer) -> (f64, f64) {
        let format = CVPixelBufferGetPixelFormatType(image);
        let (sx, sy, bits) = layout(format);
        let (mut ey, mut ec) = (0.0_f64, 0.0_f64);
        with_planes(image, true, |plane, base, stride| {
            let read = |x: usize, y: usize, k: usize, cx: usize| -> f64 {
                if bits == 8 {
                    // SAFETY: the plane is locked and the offset is inside it.
                    let cell = unsafe { base.add(y * stride + x * cx + k) };
                    // SAFETY: one readable byte of the locked plane.
                    f64::from(unsafe { cell.read() })
                } else {
                    let mut b = [0_u8; 2];
                    // SAFETY: as above, two bytes per sample.
                    let cell = unsafe { base.add(y * stride + (x * cx + k) * 2) };
                    // SAFETY: two readable bytes of the locked plane.
                    unsafe { ptr::copy_nonoverlapping(cell, b.as_mut_ptr(), 2) }
                    f64::from(u16::from_le_bytes(b) >> 6) / 4.0
                }
            };
            for y in 0..p.h {
                for x in 0..p.w {
                    let i = y * p.w + x;
                    if plane == 0 {
                        let d = read(x, y, 0, 1) - f64::from(p.y[i]);
                        ey += d * d;
                    } else {
                        let (cx, cy) = (x / sx, y / sy);
                        let db = read(cx, cy, 0, 2) - f64::from(p.cb[i]);
                        let dr = read(cx, cy, 1, 2) - f64::from(p.cr[i]);
                        ec += db * db + dr * dr;
                    }
                }
            }
        });
        let n = (p.w * p.h) as f64;
        let db = |mse: f64| 10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10();
        (db(ey / n), db(ec / (2.0 * n)))
    }

    // ---- Encode and decode sessions ----------------------------------------------------------

    struct Sample(CFRetained<CMSampleBuffer>);
    #[expect(
        clippy::non_send_fields_in_send_ty,
        reason = "CMSampleBuffer is immutable once emitted"
    )]
    // SAFETY: a CMSampleBuffer is immutable once VideoToolbox hands it over and CF reference
    // counting is thread-safe; the test only reads it on another thread.
    unsafe impl Send for Sample {}

    unsafe extern "C-unwind" fn on_encoded(
        refcon: *mut c_void,
        _source: *mut c_void,
        status: i32,
        flags: VTEncodeInfoFlags,
        sample: *mut CMSampleBuffer,
    ) {
        // SAFETY: the refcon is the `Sender` boxed by `Session::new`, alive until the session
        // is invalidated in `Drop`.
        let tx = unsafe { &*refcon.cast::<mpsc::Sender<(Instant, Option<Sample>)>>() };
        let sample = NonNull::new(sample)
            .filter(|_| status == 0 && !flags.contains(VTEncodeInfoFlags::FrameDropped))
            // SAFETY: the callback borrows a valid sample buffer; retaining it keeps it.
            .map(|s| Sample(unsafe { CFRetained::retain(s) }));
        let _gone = tx.send((Instant::now(), sample));
    }

    struct Session {
        vt: CFRetained<VTCompressionSession>,
        rx: mpsc::Receiver<(Instant, Option<Sample>)>,
        _tx: Box<mpsc::Sender<(Instant, Option<Sample>)>>,
        /// `EnableLTR`'s status: 0 when the encoder took long-term references.
        ltr: i32,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: invalidation stops callbacks before the boxed sender is freed.
            unsafe { self.vt.invalidate() }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Spec {
        LowLatencyHardware,
        Hardware,
        Encoder(&'static str),
    }

    impl Session {
        /// A session like the worker's (real time, no reordering, infinite GOP, LTR), with
        /// `profile` and `format` as its declared source; the status on failure.
        fn new(
            w: usize,
            h: usize,
            spec: Spec,
            profile: Option<&CFString>,
            format: u32,
            bitrate: i64,
        ) -> Result<Self, (&'static str, i32)> {
            // SAFETY: framework-provided constant strings.
            let (ll, hw, id_key) = unsafe {
                (
                    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                    kVTVideoEncoderSpecification_EncoderID,
                )
            };
            let id;
            let spec = match spec {
                Spec::LowLatencyHardware => dict(&[(ll, yes()), (hw, yes())]),
                Spec::Hardware => dict(&[(hw, yes())]),
                Spec::Encoder(name) => {
                    id = CFString::from_str(name);
                    dict(&[(id_key, &id)])
                }
            };
            let format_number = CFNumber::new_i64(i64::from(format));
            // SAFETY: framework-provided constant string.
            let source = dict(&[(unsafe { kCVPixelBufferPixelFormatTypeKey }, &format_number)]);
            let (tx, rx) = mpsc::channel();
            let tx = Box::new(tx);
            let mut raw: *mut VTCompressionSession = ptr::null_mut();
            // SAFETY: valid pointers; the refcon is the boxed sender this `Session` owns.
            let status = unsafe {
                VTCompressionSession::create(
                    None,
                    w as i32,
                    h as i32,
                    kCMVideoCodecType_HEVC,
                    Some(spec.as_opaque()),
                    Some(source.as_opaque()),
                    None,
                    Some(on_encoded),
                    ptr::from_ref(&*tx).cast_mut().cast::<c_void>(),
                    NonNull::from(&mut raw),
                )
            };
            let raw = NonNull::new(raw).filter(|_| status == 0).ok_or(("create", status))?;
            // SAFETY: +1 reference from the create call.
            let session = unsafe { CFRetained::from_raw(raw) };
            let mut this = Self { vt: session, rx, _tx: tx, ltr: 0 };
            let s: &CFType = &this.vt;
            // SAFETY: framework-provided constant strings.
            unsafe {
                // Best effort, as the worker sets them: not every encoder takes every key.
                set(s, kVTCompressionPropertyKey_RealTime, yes());
                set(s, kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::new(false));
                set(s, kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i64(60));
                set(
                    s,
                    kVTCompressionPropertyKey_MaxKeyFrameInterval,
                    &CFNumber::new_i64(i64::from(i32::MAX)),
                );
                this.ltr = set(s, kVTCompressionPropertyKey_EnableLTR, yes());
                let status =
                    set(s, kVTCompressionPropertyKey_AverageBitRate, &CFNumber::new_i64(bitrate));
                if status != 0 {
                    return Err(("AverageBitRate", status));
                }
                if let Some(profile) = profile {
                    let status = set(s, kVTCompressionPropertyKey_ProfileLevel, profile);
                    if status != 0 {
                        return Err(("ProfileLevel", status));
                    }
                }
                let status = this.vt.prepare_to_encode_frames();
                if status != 0 {
                    return Err(("prepare", status));
                }
            }
            Ok(this)
        }

        fn hardware(&self) -> Option<bool> {
            // SAFETY: framework-provided constant string.
            copy_bool(&self.vt, unsafe {
                kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder
            })
        }

        /// Encode one picture and wait for it: the submit → callback time and the sample.
        fn encode(&self, image: &CVPixelBuffer, index: i64) -> (Duration, Option<Sample>) {
            let pts = CMTime { value: index, timescale: 60, flags: CMTimeFlags::Valid, epoch: 0 };
            let submitted = Instant::now();
            // SAFETY: a valid image and session; no per-frame properties or refcon.
            let status = unsafe {
                self.vt.encode_frame(
                    image,
                    pts,
                    kCMTimeInvalid,
                    None,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if status != 0 {
                return (Duration::ZERO, None);
            }
            match self.rx.recv_timeout(Duration::from_secs(10)) {
                Ok((at, sample)) => (at.duration_since(submitted), sample),
                Err(_) => (Duration::ZERO, None),
            }
        }
    }

    fn sample_bytes(sample: &CMSampleBuffer) -> usize {
        // SAFETY: a valid sample buffer.
        unsafe { sample.total_sample_size() }
    }

    /// `(profile_idc, chroma_format_idc, bit depth)` from the stream's SPS.
    fn sps_of(format: &CMFormatDescription) -> Option<(u32, u32, u32)> {
        let mut count = 0_usize;
        let mut index = 0_usize;
        loop {
            let mut ptr: *const u8 = ptr::null();
            let mut size = 0_usize;
            // SAFETY: valid out pointers; the bytes are owned by `format`.
            let status = unsafe {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    index,
                    &raw mut ptr,
                    &raw mut size,
                    &raw mut count,
                    ptr::null_mut(),
                )
            };
            if status != 0 || ptr.is_null() {
                return None;
            }
            // SAFETY: CoreMedia returned `size` readable bytes at `ptr`.
            let nal = unsafe { std::slice::from_raw_parts(ptr, size) };
            if (nal[0] >> 1) & 0x3f == 33 {
                return parse_sps(nal);
            }
            index += 1;
            if index >= count {
                return None;
            }
        }
    }

    /// `(profile_idc, chroma_format_idc, bit depth)`: the profile from the SPS's fixed-length
    /// head (no emulation bytes can precede it), the rest from the codec's own parser.
    fn parse_sps(nal: &[u8]) -> Option<(u32, u32, u32)> {
        let profile_idc = u32::from(nal.get(3)? & 0x1f);
        let format = hevc::sample_format(nal)?;
        Some((profile_idc, format.chroma_format_idc, format.bit_depth))
    }

    unsafe extern "C-unwind" fn on_decoded(
        refcon: *mut c_void,
        _source: *mut c_void,
        status: i32,
        _flags: VTDecodeInfoFlags,
        image: *mut CVImageBuffer,
        _pts: CMTime,
        _duration: CMTime,
    ) {
        // SAFETY: the refcon is the `Sender` owned by `decode`, which outlives the session.
        let tx = unsafe { &*refcon.cast::<mpsc::Sender<Result<Pixels, i32>>>() };
        let image = NonNull::new(image).filter(|_| status == 0);
        // SAFETY: the callback borrows a valid image buffer; retaining it keeps it.
        let _gone = tx.send(image.map(|i| Pixels(unsafe { CFRetained::retain(i) })).ok_or(status));
    }

    struct Pixels(CFRetained<CVImageBuffer>);
    #[expect(clippy::non_send_fields_in_send_ty, reason = "CVBuffer is documented thread-safe")]
    // SAFETY: the decoded buffer is not written again once handed over; CF retain is thread-safe.
    unsafe impl Send for Pixels {}

    /// Decode `samples` into `output` format; `(hardware in use, last picture)`, or the
    /// failing status.
    fn decode(
        samples: &[Sample],
        output: u32,
        require_hardware: bool,
    ) -> Result<(Option<bool>, Pixels), (&'static str, i32)> {
        let first = samples.first().ok_or(("no samples", 0))?;
        // SAFETY: a valid sample buffer.
        let format = unsafe { first.0.format_description() }.ok_or(("format", 0))?;
        let (tx, rx) = mpsc::channel::<Result<Pixels, i32>>();
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_decoded),
            decompressionOutputRefCon: ptr::from_ref(&tx).cast_mut().cast::<c_void>(),
        };
        let format_number = CFNumber::new_i64(i64::from(output));
        // SAFETY: framework-provided constant strings.
        let attrs = dict(&[(unsafe { kCVPixelBufferPixelFormatTypeKey }, &format_number)]);
        let spec = dict(&[(
            // SAFETY: framework-provided constant string.
            unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder },
            CFBoolean::new(require_hardware),
        )]);
        let mut raw: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY: valid pointers; the refcon is `tx`, which outlives the session.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                Some(spec.as_opaque()),
                Some(attrs.as_opaque()),
                &raw const record,
                NonNull::from(&mut raw),
            )
        };
        let raw = NonNull::new(raw).filter(|_| status == 0).ok_or(("create", status))?;
        // SAFETY: +1 reference from the create call.
        let session = unsafe { CFRetained::from_raw(raw) };
        // SAFETY: framework-provided constant string.
        let hardware = copy_bool(&session, unsafe {
            kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder
        });
        let mut last = Err(("no picture", 0));
        for sample in samples {
            // SAFETY: a valid sample and session; synchronous decode, no refcon.
            let status = unsafe {
                session.decode_frame(
                    &sample.0,
                    VTDecodeFrameFlags(0),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if status != 0 {
                last = Err(("decode", status));
                break;
            }
            last = match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(p)) => Ok(p),
                Ok(Err(s)) => Err(("output", s)),
                Err(_) => Err(("timeout", 0)),
            };
        }
        // SAFETY: invalidation stops callbacks before `tx` is dropped.
        unsafe { session.invalidate() }
        last.map(|p| (hardware, p))
    }

    /// Nanoseconds as milliseconds.
    fn ms(ns: u64) -> f64 {
        ns as f64 / 1e6
    }

    // ---- The probe -----------------------------------------------------------------------------

    fn low_latency_spec() -> CFRetained<Dict> {
        // SAFETY: framework-provided constant strings.
        let (ll, hw) = unsafe {
            (
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
            )
        };
        dict(&[(ll, yes()), (hw, yes())])
    }

    fn hardware_spec() -> CFRetained<Dict> {
        // SAFETY: framework-provided constant string.
        let hw = unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder };
        dict(&[(hw, yes())])
    }

    /// The profile named `suffix` from an encoder's own supported-value list: the 4:4:4
    /// profiles are advertised there but the SDK exports no constant for them.
    fn advertised(list: &[CFRetained<CFString>], name: &str) -> Option<CFRetained<CFString>> {
        list.iter().find(|p| p.to_string() == name).cloned()
    }

    /// The first picture's SPS `(profile_idc, chroma_format_idc, bits, bits)` from a session.
    fn emitted(session: &Session, image: &CVPixelBuffer) -> Option<(u32, u32, u32)> {
        let (_, sample) = session.encode(image, 0);
        // SAFETY: a valid sample buffer.
        sample.and_then(|s| unsafe { s.0.format_description() }).and_then(|f| sps_of(&f))
    }

    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn chroma_444_capabilities() {
        // 1. What the framework lists.
        // SAFETY: plain query.
        let hw_decode = unsafe { VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) };
        eprintln!("MEASURE vt hevc_hw_decode={hw_decode}");
        let encoders = encoder_list();
        for (id, hw) in &encoders {
            eprintln!("MEASURE vt encoder id={id} hardware={hw}");
        }
        // SAFETY: framework-provided constant strings.
        let (ll, id_key) = unsafe {
            (
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                kVTVideoEncoderSpecification_EncoderID,
            )
        };
        let mut specs: Vec<(String, CFRetained<Dict>)> = vec![
            ("low-latency+hw".to_owned(), low_latency_spec()),
            ("hw".to_owned(), hardware_spec()),
        ];
        for (id, _) in &encoders {
            let name = CFString::from_str(id);
            specs.push((format!("id={id}"), dict(&[(id_key, &name)])));
            specs.push((format!("id={id}+low-latency"), dict(&[(id_key, &name), (ll, yes())])));
        }
        let mut advertised_profiles: Vec<CFRetained<CFString>> = Vec::new();
        for (name, spec) in &specs {
            let (status, id, profiles, keys) = supported_for(1920, 1080, spec);
            let names: Vec<String> = profiles.iter().map(ToString::to_string).collect();
            eprintln!(
                "MEASURE vt supported spec={name} status={status} encoder={id} profiles={names:?}"
            );
            let wanted = ["EnableLTR", "InputPixelFormat", "Quality", "ConstantQualityFactor"];
            let has: Vec<&str> =
                wanted.iter().copied().filter(|k| keys.iter().any(|key| key == k)).collect();
            eprintln!("MEASURE vt keys spec={name} of {wanted:?} lists {has:?}");
            for p in profiles {
                if !advertised_profiles.iter().any(|q| q.to_string() == p.to_string()) {
                    advertised_profiles.push(p);
                }
            }
        }

        // 2. What a session actually emits for each profile the encoders advertise, fed 4:2:0,
        //    4:2:2 and 4:4:4 pictures.
        let formats = [
            kCVPixelFormatType_32BGRA,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_422YpCbCr10BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
        ];
        let mut session_specs = vec![Spec::LowLatencyHardware, Spec::Hardware];
        for (id, hw) in &encoders {
            if !hw {
                session_specs.push(Spec::Encoder(Box::leak(id.clone().into_boxed_str())));
            }
        }
        let (w, h) = (1920, 1080);
        let source = picture(w, h, 0);
        let mut beyond_420 = Vec::new();
        for spec in &session_specs {
            let profiles = std::iter::once(None).chain(advertised_profiles.iter().map(Some));
            for profile in profiles {
                let pname = profile.map_or_else(|| "none".to_owned(), ToString::to_string);
                for &format in &formats {
                    let tag = format!("spec={spec:?} profile={pname} input={}", fourcc(format));
                    let session = match Session::new(
                        w,
                        h,
                        *spec,
                        profile.map(|p| &**p),
                        format,
                        20_000_000,
                    ) {
                        Ok(s) => s,
                        Err((call, status)) => {
                            eprintln!("MEASURE vt session {tag} refused {call} status={status}");
                            continue;
                        }
                    };
                    let image = fill(&source, format);
                    let (_, sample) = session.encode(&image, 0);
                    let sps = sample.as_ref().and_then(|s| {
                        // SAFETY: a valid sample buffer.
                        unsafe { s.0.format_description() }.and_then(|f| sps_of(&f))
                    });
                    eprintln!(
                        "MEASURE vt session {tag} hardware={:?} ltr_status={} \
                         sps(profile_idc,chroma_format_idc,bits)={sps:?}",
                        session.hardware(),
                        session.ltr,
                    );
                    if let (Some((_, chroma, ..)), Some(sample)) = (sps, sample)
                        && chroma > 1
                        && !beyond_420.iter().any(|(_, f, c, _): &(String, u32, u32, Sample)| {
                            *f == format && *c == chroma
                        })
                    {
                        beyond_420.push((tag, format, chroma, sample));
                    }
                }
            }
        }

        // 3. Whether the decoder takes what came out beyond 4:2:0, in hardware.
        for (tag, format, chroma, sample) in &beyond_420 {
            let output = if *format == kCVPixelFormatType_32BGRA {
                kCVPixelFormatType_444YpCbCr8BiPlanarFullRange
            } else {
                *format
            };
            match decode(std::slice::from_ref(sample), output, true) {
                Ok((hardware, pixels)) => eprintln!(
                    "MEASURE vt decode {tag} chroma_format_idc={chroma} require_hw=true \
                     hardware={hardware:?} output={}",
                    fourcc(CVPixelBufferGetPixelFormatType(&pixels.0))
                ),
                Err((call, status)) => eprintln!(
                    "MEASURE vt decode {tag} chroma_format_idc={chroma} require_hw=true \
                     failed {call} status={status}"
                ),
            }
        }
    }

    /// One stream configuration the cost measurement compares.
    struct Mode {
        name: &'static str,
        spec: Spec,
        profile: Option<CFRetained<CFString>>,
        format: u32,
    }

    impl Mode {
        /// What the decoder is asked for: the source's own format, or 4:4:4 for BGRA.
        fn decoded(&self) -> u32 {
            if self.format == kCVPixelFormatType_32BGRA {
                kCVPixelFormatType_444YpCbCr8BiPlanarFullRange
            } else {
                self.format
            }
        }
    }

    fn modes() -> Vec<Mode> {
        let (_, _, ll_profiles, _) = supported_for(1920, 1080, &low_latency_spec());
        let (_, _, hw_profiles, _) = supported_for(1920, 1080, &hardware_spec());
        // SAFETY: framework-provided constant string.
        let main: CFRetained<CFString> =
            CFRetained::from(unsafe { kVTProfileLevel_HEVC_Main_AutoLevel });
        let mut modes = vec![
            Mode {
                name: "low-latency Main 4:2:0 (shipped)",
                spec: Spec::LowLatencyHardware,
                profile: Some(main.clone()),
                format: kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            },
            Mode {
                name: "hw Main 4:2:0",
                spec: Spec::Hardware,
                profile: Some(main),
                format: kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            },
            Mode {
                name: "hw Main444 4:4:4",
                spec: Spec::Hardware,
                profile: advertised(&hw_profiles, "HEVC_Main444_AutoLevel"),
                format: kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
            },
        ];
        if let Some(profile) = advertised(&ll_profiles, "HEVC_Main444_AutoLevel") {
            modes.push(Mode {
                name: "low-latency Main444 4:4:4",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile.clone()),
                format: kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
            });
            modes.push(Mode {
                name: "low-latency Main444 fed BGRA",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile),
                format: kCVPixelFormatType_32BGRA,
            });
        }
        if let Some(profile) = advertised(&ll_profiles, "HEVC_Main44410_AutoLevel") {
            modes.push(Mode {
                name: "low-latency Main44410 fed xf44",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile.clone()),
                format: kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
            });
            modes.push(Mode {
                name: "low-latency Main44410 fed BGRA",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile),
                format: kCVPixelFormatType_32BGRA,
            });
        }
        modes
    }

    /// Encode time per frame at 1080p and 5K, one frame in flight like the worker, over a
    /// scrolling text picture.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn chroma_444_encode_time() {
        const FRAMES: usize = 180;
        for (w, h, bitrate) in [(1920_usize, 1080_usize, 16_000_000_i64), (5120, 2880, 60_000_000)]
        {
            let pictures: Vec<Picture> = (0..6).map(|i| picture(w, h, i * 16)).collect();
            for mode in modes() {
                let Ok(session) =
                    Session::new(w, h, mode.spec, mode.profile.as_deref(), mode.format, bitrate)
                else {
                    eprintln!("MEASURE encode {w}x{h} mode={:?} refused", mode.name);
                    continue;
                };
                let images: Vec<_> = pictures.iter().map(|p| fill(p, mode.format)).collect();
                let chroma = emitted(&session, &images[0]).map(|s| s.1);
                let mut times = Vec::with_capacity(FRAMES);
                let mut bytes = 0_usize;
                for i in 0..FRAMES {
                    let (took, sample) = session.encode(&images[i % images.len()], i as i64 + 1);
                    if let Some(sample) = sample {
                        times.push(took);
                        if i >= 30 {
                            bytes += sample_bytes(&sample.0);
                        }
                    }
                }
                let spread = Spread::of_durations(&times).unwrap_or_default();
                let mbps = (bytes * 8) as f64 / ((FRAMES - 30) as f64 / 60.0) / 1e6;
                eprintln!(
                    "MEASURE encode {w}x{h} mode={:?} chroma_format_idc={chroma:?} frames={} \
                     p50={:.2}ms p95={:.2}ms max={:.2}ms target={}Mbit/s actual={mbps:.1}Mbit/s",
                    mode.name,
                    times.len(),
                    ms(spread.p50),
                    ms(spread.p95),
                    ms(spread.max),
                    bitrate / 1_000_000,
                );
            }
        }
    }

    /// Bits against quality on the text picture at 1080p: each mode at several target rates,
    /// the actual rate it spent, and the PSNR of what the hardware decoder gives back against
    /// the 4:4:4 source.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn chroma_444_rate_quality() {
        const FRAMES: usize = 90;
        let (w, h) = (1920, 1080);
        let pictures: Vec<Picture> = (0..6).map(|i| picture(w, h, i * 16)).collect();
        for mode in modes() {
            let images: Vec<_> = pictures.iter().map(|p| fill(p, mode.format)).collect();
            // The picture as the source format carries it, before any encoding.
            let (sy, sc) = if mode.format == kCVPixelFormatType_32BGRA {
                (f64::NAN, f64::NAN)
            } else {
                psnr(&pictures[0], &images[0])
            };
            eprintln!(
                "MEASURE quality mode={:?} source_only psnr_y={sy:.2} psnr_c={sc:.2}",
                mode.name
            );
            for mbps in [4_i64, 8, 16, 32] {
                let Ok(session) = Session::new(
                    w,
                    h,
                    mode.spec,
                    mode.profile.as_deref(),
                    mode.format,
                    mbps * 1_000_000,
                ) else {
                    eprintln!("MEASURE quality mode={:?} refused", mode.name);
                    break;
                };
                let mut samples = Vec::with_capacity(FRAMES);
                let mut bytes = 0_usize;
                for i in 0..FRAMES {
                    let (_, sample) = session.encode(&images[i % images.len()], i as i64);
                    if let Some(sample) = sample {
                        if i >= 30 {
                            bytes += sample_bytes(&sample.0);
                        }
                        samples.push(sample);
                    }
                }
                let actual = (bytes * 8) as f64 / ((FRAMES - 30) as f64 / 60.0) / 1e6;
                let last = &pictures[(FRAMES - 1) % pictures.len()];
                match decode(&samples, mode.decoded(), true) {
                    Ok((hardware, pixels)) => {
                        let (py, pc) = psnr(last, &pixels.0);
                        eprintln!(
                            "MEASURE quality mode={:?} target={mbps}Mbit/s actual={actual:.2}Mbit/s \
                             psnr_y={py:.2}dB psnr_c={pc:.2}dB hw_decode={hardware:?}",
                            mode.name
                        );
                    }
                    Err((call, status)) => eprintln!(
                        "MEASURE quality mode={:?} target={mbps}Mbit/s decode failed {call} {status}",
                        mode.name
                    ),
                }
            }
        }
    }
}
