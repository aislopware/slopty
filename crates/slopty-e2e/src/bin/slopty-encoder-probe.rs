//! Whether this Mac's video encoder answers: one keyframe through a real VideoToolbox session,
//! the smallest coding there is.
//!
//! `cargo xtask e2e app` runs it before the app's tests that code video, as the gate runs its
//! own probe before the VideoToolbox tests (`xtask/src/gate.rs`, `videotoolbox_step`). A hosted
//! runner's virtual Mac forwards its encoder to the host, and at times that encoder stops for
//! good; a process that coded through it then stalls in its exit, where no signal ends it
//! (`docs/decisions/video.md`, "A virtual Mac's encoder stops for good past its 1020th client").
//! It exits 0 once the keyframe came, 1 with why it did not. An encoder that stopped may never
//! return from the call, so its caller bounds it.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    probe::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod probe {
    use std::process::ExitCode;
    use std::ptr::NonNull;
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, Result, bail};
    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferCreate, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    };
    use slopty_codec::{Chroma, EncoderConfig, FrameOptions};
    use slopty_proto::screen::VideoCodec;

    /// The picture's size: small, so the coding itself takes milliseconds.
    const WIDTH: usize = 320;
    const HEIGHT: usize = 180;

    /// How long the keyframe may take once handed over: an encoder that answers codes it in
    /// milliseconds, and a cold one in well under a second.
    const ANSWER: Duration = Duration::from_secs(20);

    #[expect(
        clippy::print_stdout,
        clippy::print_stderr,
        reason = "what it found is what it prints, for the run's log"
    )]
    pub fn run() -> ExitCode {
        match coded() {
            Ok(took) => {
                println!("encoder probe: one keyframe in {took:.1?}");
                ExitCode::SUCCESS
            }
            Err(why) => {
                eprintln!("encoder probe: {why:#}");
                ExitCode::FAILURE
            }
        }
    }

    /// One keyframe coded, and how long it took from the session's making.
    fn coded() -> Result<Duration> {
        let started = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        let config = EncoderConfig {
            width: u32::try_from(WIDTH)?,
            height: u32::try_from(HEIGHT)?,
            codec: VideoCodec::Hevc,
            fps: 60,
            bitrate_bps: 2_000_000,
            chroma: Chroma::Subsampled,
        };
        let encoder = slopty_codec::Encoder::new(config, move |packet| {
            let _gone = tx.send(packet.keyframe);
        })
        .context("make a session")?;
        let picture = blank()?;
        let keyframe = FrameOptions { force_keyframe: true, ..FrameOptions::default() };
        encoder.encode(&picture, 0, &keyframe).context("hand it a picture")?;
        encoder.flush().context("finish the picture")?;
        match rx.recv_timeout(ANSWER) {
            Ok(true) => Ok(started.elapsed()),
            Ok(false) => bail!("the first frame out was not a keyframe"),
            Err(_) => bail!("no frame came out within {ANSWER:?}"),
        }
    }

    /// A picture in the format the worker captures; what it shows does not matter.
    fn blank() -> Result<CFRetained<CVPixelBuffer>> {
        let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: CoreVideo rule for `CVPixelBufferCreate`: a valid out-pointer, and no
        // allocator or attributes (the defaults).
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                WIDTH,
                HEIGHT,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        if status != 0 {
            bail!("CVPixelBufferCreate: {status}");
        }
        let raw = NonNull::new(raw).context("CVPixelBufferCreate made no buffer")?;
        // SAFETY: `CVPixelBufferCreate` returned a +1 reference, which this takes over.
        Ok(unsafe { CFRetained::from_raw(raw) })
    }
}
