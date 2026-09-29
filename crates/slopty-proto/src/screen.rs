//! Remote windows and displays: enumeration, stream setup, input, telemetry.

use serde::{Deserialize, Serialize};
use slopty_core::{DisplayId, Duration, StreamId, WindowId};

use crate::input::{KeyAction, KeyCode, Mods, MouseButton};

/// A window on the worker that can be streamed.
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
    /// Bounds in worker points.
    pub x: f32,
    /// Bounds.
    pub y: f32,
    /// Bounds.
    pub w: f32,
    /// Bounds.
    pub h: f32,
    /// Display the window is on.
    pub display: DisplayId,
    /// On screen (not minimised or on another Space).
    pub on_screen: bool,
}

/// A display on the worker.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// Identity.
    pub id: DisplayId,
    /// Bounds in points.
    pub w: f32,
    /// Bounds.
    pub h: f32,
    /// Backing scale.
    pub scale: f32,
    /// Refresh rate.
    pub hz: f32,
}

/// What to stream.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum CaptureTarget {
    /// One window (with its child windows).
    Window(WindowId),
    /// A whole display.
    Display(DisplayId),
}

/// A client's own key for the virtual display a worker makes for it.
///
/// Sixteen random bytes the client draws once and keeps on the device, so the display has the
/// same identity every time and macOS finds the arrangement and mode it stored for it last time.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DisplayKey(pub [u8; 16]);

/// The display a client wants a worker to make for it: the tile's drawable in pixels, the
/// backing scale of the screen the tile is on, and that screen's fastest refresh.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DisplayShape {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Backing scale (1 for a standard screen, 2 for Retina, 3 for an iPhone).
    pub scale: f32,
    /// The screen's maximum refresh, hertz; 0 when it is not known.
    pub refresh_hz: u16,
}

/// What a stream opened with [`ScreenRequest::OpenDisplay`] shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum VirtualDisplay {
    /// A display made for this client, at its shape.
    Made(DisplayId),
    /// A physical display, because no virtual one could be had.
    Physical {
        /// The display streamed instead.
        display: DisplayId,
        /// Why.
        why: NoVirtualDisplay,
    },
}

/// Why a worker streams a physical display where a client asked for one of its own.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NoVirtualDisplay {
    /// This worker cannot make one (not macOS, or macOS without the classes it needs).
    Unavailable,
    /// macOS declined to create it or to take its mode.
    Refused,
    /// It never settled in its mode.
    Unsettled,
    /// ScreenCaptureKit never listed it.
    Unlisted,
}

/// Video codec.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum VideoCodec {
    /// HEVC: Main, 8-bit 4:2:0, or Main 4:4:4 10 for a [`Chroma::Full`] stream.
    Hevc,
    /// H.264 High (fallback only).
    H264,
}

/// How much colour a video stream carries (`docs/decisions/video.md`, "4:4:4 on the
/// low-latency encoder").
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Chroma {
    /// 4:2:0, one colour sample per 2×2 pixels: HEVC Main or H.264 High, fed full-range NV12.
    #[default]
    Subsampled,
    /// 4:4:4, colour at every pixel: HEVC Main 4:4:4 10, HEVC only, captured as full-range
    /// 10-bit bi-planar 4:4:4 (`xf44`), the one 4:4:4 format ScreenCaptureKit delivers.
    Full,
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
    /// The most colour the client wants. [`Chroma::Full`] is an ask, not an order: the worker
    /// streams 4:4:4 only while its bitrate target is high enough for 4:4:4 to beat 4:2:0, and
    /// 4:2:0 below that (`docs/decisions/video.md`, "Full chroma follows the rate"). The client
    /// learns which from the stream itself: its decoder follows the SPS.
    pub chroma: Chroma,
}

impl Default for Quality {
    fn default() -> Self {
        Self {
            fps: 60,
            bitrate_bps: 30_000_000,
            scale: 1.0,
            codec: VideoCodec::Hevc,
            chroma: Chroma::Subsampled,
        }
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
    /// Scroll with trackpad phases so the worker can synthesise momentum-faithful events.
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
    /// A key by its position on the keyboard, never by its character: the worker posts the
    /// position and its own keyboard layout, the client's when it took it
    /// ([`Self::KeyboardSource`]), makes the character, composing dead keys and input methods
    /// in the remote app (`docs/decisions/input.md`, "Keys go by position").
    Key {
        /// Physical key.
        code: KeyCode,
        /// Action.
        action: KeyAction,
        /// Modifiers, with their side when the client knows it.
        mods: Mods,
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
    /// Text the client composed and committed: an input method's or a dead key's result while
    /// the worker has not taken the client's input source, dictation, "Type the clipboard".
    /// The worker types it as it stands, whatever its own layout.
    Text {
        /// The text; any length, the worker splits it.
        text: String,
    },
    /// Caps Lock's state on the client: the worker sets its own lock to it. Sent when the tile
    /// takes the keyboard and whenever it changes, never as a key.
    Lock {
        /// Caps Lock is on.
        caps: bool,
    },
    /// A media key: the worker's Now Playing app takes it. Volume keys stay on the client,
    /// where the stream's sound plays.
    Media {
        /// Which.
        key: MediaKey,
        /// Pressed, or let go.
        down: bool,
    },
    /// The client's keyboard input source (a TIS id, `com.apple.keylayout.French`): the worker
    /// selects it, so positions mean what they mean on the client. The stream's claim on it
    /// lasts until [`Self::KeyboardReleased`] or the stream's end; the worker is under the
    /// newest claim still held, by any stream of any client, and the source goes back to the
    /// worker's own when none is, unless the person at the worker picked one by hand since.
    /// Answered with [`ScreenEvent::KeyboardSource`]. Sent in the input's order, and the stream
    /// holds the keys and text after it, and whatever follows a held one, until the switch is
    /// answered (at once for the source the worker already has, at most 150 ms otherwise), so
    /// they are read under it.
    KeyboardSource {
        /// The input source id.
        source: String,
    },
    /// The client's tile has gone a while without the keyboard: this stream's claim from
    /// [`Self::KeyboardSource`] goes, as at the stream's end. A later `KeyboardSource` claims
    /// anew.
    KeyboardReleased,
}

/// A media key the worker takes (`NX_KEYTYPE_*`, `<IOKit/hidsystem/ev_keymap.h>`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MediaKey {
    /// Play or pause.
    PlayPause,
    /// Next track.
    Next,
    /// Previous track.
    Previous,
}

impl ScreenInput {
    /// Whether this input applies only in its turn. A move sets where the pointer is, so a
    /// newer one may overtake an older one that has not arrived; everything else is an event
    /// the target sees once, in order (a key, a button, a scroll's delta, a pinch's, text, the
    /// input source the keys after it are read under).
    #[must_use]
    pub const fn in_order(&self) -> bool {
        !matches!(self, Self::Move { .. })
    }

    /// Whether this is ⌘V, the chord a paste into a streamed window is: it must find the
    /// client's clipboard on the worker's pasteboard, so nothing may carry it ahead of the offer
    /// the control stream sent before it.
    #[must_use]
    pub fn is_paste_chord(&self) -> bool {
        matches!(
            self,
            Self::Key { code: KeyCode::V, action: KeyAction::Press, mods, .. }
                if mods.contains(Mods::SUPER) && !mods.intersects(Mods::CTRL | Mods::ALT)
        )
    }
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

/// Client → worker.
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
    /// Raise/focus the window on the worker.
    Focus(StreamId),
    /// Give the streamed window, or the display made for the client, this size on the worker,
    /// in the stream's native pixels (what `Opened` / `Geometry` report). A physical display
    /// stream ignores it; the worker answers, when the target did change, with a `Geometry`
    /// event from its geometry poll.
    Resize {
        /// Stream.
        stream: StreamId,
        /// Wanted width in native pixels.
        width: u32,
        /// Wanted height in native pixels.
        height: u32,
        /// The backing scale of the screen the tile is on now, for a display made for the
        /// client; `None` keeps the one it has. A window stream ignores it.
        scale: Option<f32>,
    },
    /// Make a display sized to the client and stream it; the worker answers with
    /// [`ScreenEvent::Display`] and then `Opened`. Later sizes and scales come as
    /// [`ScreenRequest::Resize`].
    OpenDisplay {
        /// The client's key: one display per key.
        key: DisplayKey,
        /// Its size, scale and refresh.
        shape: DisplayShape,
        /// How.
        quality: Quality,
    },
}

impl ScreenRequest {
    /// The stream this request is numbered for, and whether it applies only in its turn, when
    /// it is one both ends number ([`crate::datagram::ClientDatagram::ScreenInput`]): input, and
    /// a quality change, which sets the scale the input after it is mapped at.
    #[must_use]
    pub const fn numbered(&self) -> Option<(StreamId, bool)> {
        match self {
            Self::Input { stream, input } => Some((*stream, input.in_order())),
            Self::SetQuality { stream, .. } => Some((*stream, true)),
            Self::List
            | Self::Open { .. }
            | Self::OpenDisplay { .. }
            | Self::Close(_)
            | Self::Report { .. }
            | Self::Focus(_)
            | Self::Resize { .. } => None,
        }
    }
}

/// Loss feedback, client → worker, sent as a QUIC **datagram** rather than on the control stream.
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
    /// The client lost a frame it could not recover; the worker should refresh from an acked LTR.
    Refresh {
        /// Stream.
        stream: StreamId,
        /// Highest frame fully decoded.
        last_good_frame: u32,
        /// The client holds no reference to predict from (its decoder session was lost, or the
        /// stream has not started): only an IDR can be decoded, whatever it acknowledged before.
        keyframe: bool,
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
    /// Highest worker send timestamp seen (worker clock, echoed).
    pub last_worker_send_ts_us: u32,
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

/// What the worker's bitrate controller made of its last decision window.
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
/// answer. The worker says which it is.
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
    /// Premultiplied BGRA pixels, written as one byte string.
    #[serde(with = "serde_bytes")]
    pub bgra: Vec<u8>,
    /// Backing scale of the pixels.
    pub scale: u8,
}

/// Worker → client.
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
    /// What a stream opened with [`ScreenRequest::OpenDisplay`] shows: sent before its `Opened`
    /// (whose target is then that display), and again whenever it changes.
    Display {
        /// Stream.
        stream: StreamId,
        /// The key the client asked with.
        key: DisplayKey,
        /// The display it got.
        display: VirtualDisplay,
    },
    /// The answer to [`ScreenInput::KeyboardSource`]: whether the worker now types under the
    /// client's input source. Until it has, the client composes text itself and sends it as
    /// [`ScreenInput::Text`]. Sent again unasked whenever that changes: another client's
    /// stream took the worker's source (`applied: false`), or gave it back.
    KeyboardSource {
        /// Stream.
        stream: StreamId,
        /// The input source asked for.
        source: String,
        /// The worker selected it.
        applied: bool,
    },
}
