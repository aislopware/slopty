//! Remote windows and displays: enumeration, stream setup, input, telemetry.

use serde::{Deserialize, Serialize};
use slopty_core::{Duration, StreamId, WindowId};

use crate::input::{KeyAction, KeyCode, Mods, MouseButton};

/// A window on the host that can be streamed.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Identity.
    pub id: WindowId,
    /// Owning application name.
    pub app: String,
    /// Bundle identifier.
    pub bundle_id: Option<String>,
    /// Window title.
    pub title: String,
    /// Bounds in host points.
    pub x: f32,
    /// Bounds.
    pub y: f32,
    /// Bounds.
    pub w: f32,
    /// Bounds.
    pub h: f32,
    /// Display the window is on.
    pub display: u32,
    /// On screen (not minimised or on another Space).
    pub on_screen: bool,
}

/// A display on the host.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// CoreGraphics display id.
    pub id: u32,
    /// Bounds in points.
    pub w: f32,
    /// Bounds.
    pub h: f32,
    /// Backing scale.
    pub scale: f32,
    /// Refresh rate.
    pub hz: f32,
    /// Supports HDR.
    pub hdr: bool,
}

/// What to stream.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum CaptureTarget {
    /// One window (with its child windows).
    Window(WindowId),
    /// A whole display.
    Display(u32),
}

/// Video codec.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum VideoCodec {
    /// HEVC Main, 8-bit 4:2:0.
    Hevc,
    /// HEVC Main10, 10-bit 4:2:0 (HDR).
    HevcMain10,
    /// H.264 High (fallback only).
    H264,
}

/// Stream quality request.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Quality {
    /// Target frames per second.
    pub fps: u16,
    /// Target average bitrate, bits per second.
    pub bitrate_bps: u32,
    /// Scale factor applied to the capture (1.0 = native points × scale).
    pub scale: f32,
    /// Preferred codec.
    pub codec: VideoCodec,
    /// Capture HDR when the source is HDR.
    pub hdr: bool,
}

impl Default for Quality {
    fn default() -> Self {
        Self { fps: 60, bitrate_bps: 30_000_000, scale: 1.0, codec: VideoCodec::Hevc, hdr: false }
    }
}

/// Input aimed at a streamed window, in the stream's pixel coordinates.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ScreenInput {
    /// Pointer moved.
    Move {
        /// X in stream pixels.
        x: f32,
        /// Y.
        y: f32,
    },
    /// Button.
    Button {
        /// Which.
        button: MouseButton,
        /// Down or up.
        down: bool,
        /// Position.
        x: f32,
        /// Position.
        y: f32,
        /// Click count (double-click detection is client-side, like a real mouse).
        clicks: u8,
        /// Modifiers.
        mods: Mods,
    },
    /// Scroll with trackpad phases so the host can synthesise momentum-faithful events.
    Scroll {
        /// Pixel delta x.
        dx: f32,
        /// Pixel delta y.
        dy: f32,
        /// Precise (trackpad) vs. line (wheel).
        precise: bool,
        /// Gesture phase.
        phase: ScrollPhase,
        /// Momentum phase.
        momentum: ScrollPhase,
        /// Position.
        x: f32,
        /// Position.
        y: f32,
        /// Modifiers.
        mods: Mods,
    },
    /// Key.
    Key {
        /// Physical key.
        code: KeyCode,
        /// Action.
        action: KeyAction,
        /// Modifiers.
        mods: Mods,
        /// Text, for keys the host cannot reproduce from the code alone (IME, dead keys).
        text: Option<String>,
    },
    /// Pinch (magnify) gesture.
    Magnify {
        /// Delta.
        delta: f32,
        /// Phase.
        phase: ScrollPhase,
        /// Position.
        x: f32,
        /// Position.
        y: f32,
    },
}

/// Trackpad gesture phase, mirroring `NSEventPhase`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum ScrollPhase {
    /// Not in a gesture.
    #[default]
    None,
    /// Fingers touched.
    Began,
    /// Moving.
    Changed,
    /// Fingers lifted.
    Ended,
    /// Cancelled.
    Cancelled,
    /// May begin.
    MayBegin,
}

/// Client → host.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ScreenRequest {
    /// Enumerate windows and displays.
    List,
    /// Start streaming.
    Open {
        /// What.
        target: CaptureTarget,
        /// How.
        quality: Quality,
    },
    /// Stop.
    Close(StreamId),
    /// Change quality on a live stream.
    SetQuality {
        /// Stream.
        stream: StreamId,
        /// New quality.
        quality: Quality,
    },
    /// Input.
    Input {
        /// Stream.
        stream: StreamId,
        /// Event.
        input: ScreenInput,
    },
    /// Periodic receiver report (every ~50 ms while a stream is open).
    Report {
        /// Stream.
        stream: StreamId,
        /// Report.
        report: ReceiverReport,
    },
    /// Raise/focus the window on the host.
    Focus(StreamId),
    /// Put `text` on the host's pasteboard (sent ahead of a paste chord so the host pastes
    /// what the client copied). Text only, at most `MAX_CLIPBOARD_BYTES`.
    Clipboard {
        /// The client's clipboard text.
        text: String,
    },
}

/// Largest clipboard text carried in either direction; a pasteboard can hold a whole file,
/// and pushing that on every change would starve the video stream.
pub const MAX_CLIPBOARD_BYTES: usize = 256 * 1024;

/// Loss feedback, client → host, sent as a QUIC **datagram** rather than on the control stream.
///
/// The control stream is ordered: one lost packet carrying a NACK would hold every later NACK
/// back until QUIC's loss timer retransmits it (a whole PTO, tens of milliseconds on Wi-Fi), by
/// which time the receiver has given up and asked for a refresh. As datagrams each NACK stands
/// alone; the receiver's own retries provide the reliability. Encoded with
/// [`codec::encode_body`](crate::codec::encode_body); a datagram that fails to decode is
/// ignored.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Feedback {
    /// Ask for retransmission of specific fragments (inside the playout window).
    Nack {
        /// Stream.
        stream: StreamId,
        /// Frame.
        frame: u32,
        /// Missing data fragment indices; empty means "every fragment" (nothing of the frame
        /// arrived, so the client does not know how many there are).
        fragments: Vec<u16>,
    },
    /// The client lost a frame it could not recover; the host should refresh from an acked LTR.
    Refresh {
        /// Stream.
        stream: StreamId,
        /// Highest frame fully decoded.
        last_good_frame: u32,
    },
}

/// Receiver-side telemetry, all relative so no clock sync is assumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct ReceiverReport {
    /// Frames received complete or recovered in the window.
    pub frames_ok: u32,
    /// Frames recovered by FEC.
    pub frames_fec: u32,
    /// Frames lost.
    pub frames_lost: u32,
    /// Datagrams lost (by sequence gaps).
    pub datagrams_lost: u32,
    /// Highest host send timestamp seen (host clock, echoed).
    pub last_host_send_ts_us: u32,
    /// Client-side hold time between arrival and present, p50.
    pub hold_p50: Duration,
    /// Client-side hold time, p95.
    pub hold_p95: Duration,
    /// One-way-delay jitter estimate.
    pub owd_jitter: Duration,
    /// Present queue depth.
    pub queue_depth: u8,
    /// Frames presented late (missed their intended vsync).
    pub late_frames: u32,
    /// Acknowledged LTR tokens since the last report.
    pub acked_ltr: [u64; 4],
    /// Number of valid entries in `acked_ltr`.
    pub acked_ltr_len: u8,
    /// Milliseconds of the window during which nothing at all arrived on the stream past the
    /// reassembler's stall gap (a stall in progress at report time counts up to now). Loss
    /// counted while this is non-zero is the link holding packets, not dropping them.
    pub stalled_ms: u16,
    /// Stalls that released in the window (packets held, then delivered together).
    pub stalls: u16,
}

/// What the host's bitrate controller made of its last decision window.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RateVerdict {
    /// Loss, queueing or hold time: the target was cut.
    Cut,
    /// The window held a stall: the target is frozen, the window's loss discarded.
    Stall,
    /// Nothing wrong, nothing to grow into yet (cooldown after a cut, or the ceiling).
    Steady,
    /// Clean: the target grew.
    Grow,
}

/// Whether a stream's capture target is producing pictures.
///
/// A window that is hidden, minimised, or has simply not drawn since the stream opened yields no
/// frames at all, and there is no way for the client to tell that apart from a stream whose
/// frames are being lost. Without the distinction the receiver sits in "need refresh" and asks
/// for one every backoff period for as long as the item is open, which no amount of asking can
/// answer. The host says which it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SourceState {
    /// Open and capturing, but the target has produced no frame yet. Nothing to refresh from.
    Idle,
    /// The target has produced a frame; pictures are on the way.
    Live,
}

/// Cursor appearance, sent when it changes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CursorShape {
    /// Width in pixels.
    pub w: u16,
    /// Height in pixels.
    pub h: u16,
    /// Hotspot x.
    pub hot_x: u16,
    /// Hotspot y.
    pub hot_y: u16,
    /// Premultiplied BGRA pixels.
    pub bgra: Vec<u8>,
    /// Backing scale of the pixels.
    pub scale: u8,
}

/// Host → client.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ScreenEvent {
    /// Reply to `List`.
    Listing {
        /// Windows.
        windows: Vec<WindowInfo>,
        /// Displays.
        displays: Vec<DisplayInfo>,
    },
    /// Streaming started.
    Opened {
        /// Stream.
        stream: StreamId,
        /// Target.
        target: CaptureTarget,
        /// Codec in use.
        codec: VideoCodec,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Points-to-pixels scale.
        scale: f32,
        /// HDR stream (PQ, BT.2020).
        hdr: bool,
    },
    /// Stream ended.
    Closed {
        /// Stream.
        stream: StreamId,
        /// Why.
        reason: String,
    },
    /// The window moved or resized; the stream will follow.
    Geometry {
        /// Stream.
        stream: StreamId,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
    },
    /// The cursor image changed.
    Cursor {
        /// Stream.
        stream: StreamId,
        /// Shape, or `None` when hidden.
        shape: Option<CursorShape>,
    },
    /// Window list changed (a window appeared or vanished).
    ListingChanged,
    /// The host's pasteboard changed to `text` (only sent to clients with a stream open).
    Clipboard {
        /// The host's clipboard text.
        text: String,
    },
    /// The capture target started or stopped producing pictures. Sent when the state changes,
    /// so a client that never gets one treats the stream as `Live` (the old behaviour).
    Source {
        /// Stream.
        stream: StreamId,
        /// What the target is doing.
        state: SourceState,
    },
    /// The bitrate controller decided (about twice a second per stream).
    Rate {
        /// Stream.
        stream: StreamId,
        /// Encoder target after the decision, bits per second.
        target_bps: u32,
        /// What the decision was.
        verdict: RateVerdict,
        /// The QUIC congestion window, not the verdict, is what holds the target down.
        capped: bool,
    },
}
