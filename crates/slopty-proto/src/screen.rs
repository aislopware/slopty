//! Remote windows and displays: enumeration, stream setup, input, telemetry.

use serde::{Deserialize, Serialize};
use slopty_core::{DisplayId, Duration, StreamId, WindowId};

use crate::drag::{DragEvent, DragInput};
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

/// One horizontal stripe of a striped stream: a session of its own on the worker, coding its
/// rows of every capture (`docs/decisions/video.md`, "Two stripes halve the encode at 3K and
/// above").
///
/// A stream larger than one encode engine's pixel rate is coded as two stripes on the worker's
/// two engines at once. Each stripe is its own media stream, with its own frames, reassembly,
/// NACKs, refreshes and reports, on [`Self::media`]. It codes [`Self::coded_rows`] rows of the
/// picture from [`Self::coded_top`], past the seam into its neighbour's rows so the seam's rows
/// keep their prediction, and shows only [`Self::shown_rows`] rows from [`Self::shown_top`].
/// Every stripe of one capture carries that capture's `capture_ts_us`, and its frame prefix names
/// the stripes coded from it ([`crate::media::FramePrefix::stripes`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Stripe {
    /// The media stream the stripe's datagrams come on ([`Self::media_of`]).
    pub media: StreamId,
    /// The first row of the picture the stripe's session codes.
    pub coded_top: u32,
    /// Rows the stripe's session codes: the height of the pictures it decodes to.
    pub coded_rows: u32,
    /// The first row of the picture the stripe shows.
    pub shown_top: u32,
    /// Rows of the picture the stripe shows.
    pub shown_rows: u32,
}

impl Stripe {
    /// The most stripes a stream is coded as: one per encode engine of an Apple silicon Mac,
    /// and four were never better than two (MEASUREMENTS.md, "stripes across the two encode
    /// engines").
    pub const MAX: usize = 2;

    /// The media stream of stripe `index` of `stream`: the stream's own id for the top stripe,
    /// and the id with its top bit set for the one under it. A receiver routes a stripe's
    /// datagrams to its stream by the id alone ([`Self::stream_of`]), before it has read the
    /// event that names the stripe: a stream's first datagrams often beat its `Opened`.
    #[must_use]
    pub const fn media_of(stream: StreamId, index: usize) -> StreamId {
        if index == 0 { stream } else { StreamId(stream.0 | STRIPE_BIT) }
    }

    /// The stream a stripe's media stream belongs to, and the stripe's index in it.
    #[must_use]
    pub const fn stream_of(media: StreamId) -> (StreamId, usize) {
        if media.0 & STRIPE_BIT == 0 { (media, 0) } else { (StreamId(media.0 & !STRIPE_BIT), 1) }
    }

    /// Rows of the stripe's own picture above the ones it shows: the rows it codes past the
    /// seam above it, 0 for the top stripe.
    #[must_use]
    pub const fn shown_from(&self) -> u32 {
        self.shown_top.saturating_sub(self.coded_top)
    }
}

/// The bit of a stripe's media stream id that says it is the lower stripe of the stream the
/// other bits name. A worker numbers its streams upwards from 1 and never reaches it.
const STRIPE_BIT: u32 = 1 << 31;

/// A part of a stream's target, in the target's native pixels from its top-left corner.
///
/// What a zoomed picture streams ([`Quality::region`]) and what each of its frames shows
/// ([`crate::media::FramePrefix::region`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Region {
    /// Left edge.
    pub x: u16,
    /// Top edge.
    pub y: u16,
    /// Width.
    pub w: u16,
    /// Height.
    pub h: u16,
}

impl Region {
    /// The smallest side a region keeps: a capture and an encoder session have floors of their
    /// own, and a region this small is a zoom past anything a screen shows.
    pub const MIN_SIDE: u16 = 64;

    /// The part of a target `native` pixels in size this region can stream: held inside the
    /// target, at least [`Self::MIN_SIDE`] a side where the target has it, its edges on even
    /// pixels (a 4:2:0 picture's chroma is sampled per two). `None` when nothing of it is on
    /// the target, or when it is the whole target: the stream then shows all of it.
    #[must_use]
    pub fn within(self, native: (u32, u32)) -> Option<Self> {
        let side = |at: u16, len: u16, of: u32| -> Option<(u16, u16)> {
            let of = u16::try_from(of.min(u32::from(u16::MAX))).ok()?;
            let end = at.saturating_add(len).min(of);
            if at >= end {
                return None;
            }
            let start = at & !1;
            let end = end.saturating_add(end & 1).min(of);
            let min = Self::MIN_SIDE.min(of);
            let len = end.saturating_sub(start).max(min);
            let start = start.min(of.saturating_sub(len)) & !1;
            Some((start, len.min(of.saturating_sub(start))))
        };
        let (x, w) = side(self.x, self.w, native.0)?;
        let (y, h) = side(self.y, self.h, native.1)?;
        let whole = u32::from(w) >= native.0 && u32::from(h) >= native.1;
        (!whole).then_some(Self { x, y, w, h })
    }
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
    /// The part of the target to stream, at `scale`; `None` for all of it
    /// (`docs/decisions/video.md`, "A zoomed picture streams its region"). A zoomed picture
    /// asks for what it shows and a margin round it. Input, the cursor and every size the
    /// worker reports stay in the whole target's stream pixels, so nothing but the frames
    /// changes: each says which region it shows ([`crate::media::FramePrefix::region`]). The
    /// worker holds it to the target ([`Region::within`]), and again whenever the target
    /// changes size.
    pub region: Option<Region>,
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
            region: None,
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
        /// When the client read it: see [`Self::time_us`].
        time_us: u32,
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
        /// For a ⌘ or ⌃ chord while the worker is not under the client's input source: the
        /// character the client's layout puts on the key, ignoring every modifier but Shift,
        /// lowercased. The worker presses the key that types it under its current layout, else
        /// `code`, so ⌘Z means undo whichever layouts the two run (`docs/decisions/input.md`,
        /// "A shortcut goes by its character").
        chord: Option<String>,
    },
    /// A trackpad pinch, for the app under the pointer (`NSEventTypeMagnify`): the worker posts
    /// it as the trackpad would (`docs/decisions/input.md`, "Trackpad gestures reach the remote
    /// app").
    Magnify {
        /// The change in magnification since the last event of the pinch, as
        /// `NSEvent.magnification` reads it (0.1 is ten percent larger).
        delta: f32,
        /// Where the pinch is in its gesture.
        phase: ScrollPhase,
        /// Where the fingers are, in stream pixels.
        x: f32,
        /// Where the fingers are.
        y: f32,
        /// When the client read it: see [`Self::time_us`].
        time_us: u32,
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
    /// A trackpad rotation, for the app under the pointer (`NSEventTypeRotate`).
    Rotate {
        /// The change in angle since the last event of the rotation, degrees, anticlockwise
        /// positive, as `NSEvent.rotation` reads it.
        degrees: f32,
        /// Where the rotation is in its gesture.
        phase: ScrollPhase,
        /// Where the fingers are, in stream pixels.
        x: f32,
        /// Where the fingers are.
        y: f32,
        /// When the client read it: see [`Self::time_us`].
        time_us: u32,
    },
    /// Smart zoom, a two-finger double tap, for the app under the pointer
    /// (`NSEventTypeSmartMagnify`): the app zooms to what is under it, or back.
    SmartMagnify {
        /// Where the fingers tapped, in stream pixels.
        x: f32,
        /// Where the fingers tapped.
        y: f32,
    },
    /// A discrete swipe, for the app under the pointer (`NSEventTypeSwipe`, what
    /// `-swipeWithEvent:` takes: three fingers, or two where the Mac is set to swipe pages with
    /// two and the app does not track the scroll itself). A two-finger swipe that an app tracks
    /// as it moves, as Safari's back and forward do, is a [`Self::Scroll`] with its phases.
    Swipe {
        /// Which way.
        direction: SwipeDirection,
        /// Where the pointer is, in stream pixels.
        x: f32,
        /// Where the pointer is.
        y: f32,
    },
    /// Whether the tile sends its trackpad gestures to the remote app (`true`) or keeps them
    /// (`false`, how a stream starts). While it sends them, a trackpad scroll comes with the
    /// gesture a trackpad's does, which is what lets an app follow a two-finger swipe between
    /// pages as it moves (`docs/decisions/input.md`, "Trackpad gestures reach the remote app").
    Gestures {
        /// Sent to the remote app.
        remote: bool,
    },
    /// A drag from the client over the tile, or the client taking the worker's own drag out of
    /// it (`docs/decisions/audio.md`, "Drag and drop lands at the point, both ways").
    Drag(DragInput),
    /// The press of the chord the client reads as paste (⌘V or ⇧⌘V under the person's own
    /// layout), as a [`Self::Key`] press of `code` with `mods`. The client decides it by the
    /// character it typed, so a Dvorak ⌘V is one and a Dvorak ⌘K at the US V is not; the
    /// worker, which cannot know the client's layout, posts it only once the client's
    /// clipboard, offered on the control stream just ahead of it, is on its pasteboard. Its
    /// release goes as a [`Self::Key`].
    PasteChord {
        /// Physical key, as a [`Self::Key`]'s.
        code: KeyCode,
        /// Modifiers, as a [`Self::Key`]'s.
        mods: Mods,
    },
}

/// Which way a [`ScreenInput::Swipe`] went.
///
/// Named as the trackpad's swipe mask names it (`kIOHIDSwipeLeft` and its siblings,
/// `IOKit/hid/IOHIDEventTypes.h`) and read by the app as `NSEvent.deltaX` / `deltaY`: a left
/// swipe reads `deltaX` 1, a right one −1, an up swipe `deltaY` 1, a down one −1.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SwipeDirection {
    /// `deltaX` 1.
    Left,
    /// `deltaX` −1.
    Right,
    /// `deltaY` 1.
    Up,
    /// `deltaY` −1.
    Down,
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
    /// the target sees once, in order (a key, a button, a scroll's delta, a pinch's or a
    /// rotation's, a tap, a swipe, text, the input source the keys after it are read under, a
    /// drag's entry, drop and leaving). A drag's move is a move.
    #[must_use]
    pub const fn in_order(&self) -> bool {
        !matches!(self, Self::Move { .. } | Self::Drag(DragInput::Move { .. }))
    }

    /// When the client read a scroll, a pinch or a rotation, in microseconds on the client's
    /// own monotonic clock, the low 32 bits (they wrap every 71 minutes, and only the spacing
    /// between two of one gesture counts); `None` for the rest.
    ///
    /// The app the gesture reaches judges it by its events' timestamps: whether a swipe between
    /// pages turns the page on lifting, and how fast a pinch was going. Stamped as it is posted,
    /// an event carries the path's delay with it, so a report held up by the network or the
    /// worker's scheduler and then posted with the next turns a steady swipe into a stop and a
    /// jerk; the worker instead posts each at the client's spacing (`docs/decisions/input.md`,
    /// "A gesture's events keep the client's spacing").
    #[must_use]
    pub const fn time_us(&self) -> Option<u32> {
        match self {
            Self::Scroll { time_us, .. }
            | Self::Magnify { time_us, .. }
            | Self::Rotate { time_us, .. } => Some(*time_us),
            _ => None,
        }
    }

    /// Whether this is the paste chord ([`Self::PasteChord`]): it must find the client's
    /// clipboard on the worker's pasteboard, so nothing may carry it ahead of the offer the
    /// control stream sent before it.
    #[must_use]
    pub const fn is_paste_chord(&self) -> bool {
        matches!(self, Self::PasteChord { .. })
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
    /// Whether the stream's tile has this client's keyboard, in a key window of an active app.
    /// Sent on each change, and again for the stream a reconnect opens; a stream starts
    /// unfocused. The worker favours a focused stream when its encode engines are full.
    Focused {
        /// Stream.
        stream: StreamId,
        /// Focused.
        focused: bool,
    },
    /// What the client's player of the worker's sound heard, at the receiver reports' cadence
    /// while it plays (`docs/decisions/audio.md`, "One sound per worker on a client, not one
    /// per stream").
    SoundReport(SoundReport),
    /// Draw the curtain over the worker's Mac, or let it go (`docs/decisions/video.md`, "The
    /// curtain"): its own screens show a shield, and its own keyboard and pointer are held off,
    /// for as long as any client linked holds it. A client that goes lets go of it, and the Mac
    /// locks when the last one goes that way; letting go on the person's word does not lock.
    /// Answered with [`ScreenEvent::Curtain`].
    Curtain {
        /// Hold it, or let it go.
        on: bool,
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
            | Self::Focused { .. }
            | Self::SoundReport(_)
            | Self::Curtain { .. }
            | Self::Resize { .. } => None,
        }
    }
}

/// Loss feedback and clock probes, client → worker, sent as a QUIC **datagram** rather than on
/// the control stream.
///
/// The control stream is ordered: one lost packet carrying a NACK would hold every later NACK
/// back until QUIC's loss timer retransmits it (a whole PTO, tens of milliseconds on Wi-Fi), by
/// which time the receiver has given up and asked for a refresh. As datagrams each NACK stands
/// alone; the receiver's own retries provide the reliability. A clock probe goes the same way
/// for the opposite reason: a probe held behind a lost packet is a round trip the clock filter
/// has to throw away. Encoded with [`codec::encode_body`](crate::codec::encode_body); a datagram
/// that fails to decode is ignored.
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
    /// What time the worker's capture clock reads: answered at once with a
    /// [`crate::media::Kind::Clock`] datagram on the stream, carrying `sent_us` back beside the
    /// worker's own readings, so the client can place the worker's capture timestamps on its own
    /// clock (`docs/decisions/video.md`, "Capture to glass on any link").
    Clock {
        /// Stream.
        stream: StreamId,
        /// When the probe left, on the client's clock, microseconds from an epoch of its own;
        /// the worker only echoes it.
        sent_us: u64,
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

/// What the client's one player of a worker's sound ([`crate::media::SOUND`]) heard since its
/// last report.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct SoundReport {
    /// Audio packets that arrived in the window.
    pub received: u16,
    /// Audio packets missing from the sequence in the window, whether or not a copy an audio
    /// datagram carried recovered them: the loss the worker sizes those copies by.
    pub lost: u16,
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

/// Whether a stream's capture target is producing pictures, and when it cannot, why.
///
/// A window that is hidden, minimised, or has simply not drawn since the stream opened yields no
/// frames at all, and there is no way for the client to tell that apart from a stream whose
/// frames are being lost. Without the distinction the receiver sits in "need refresh" and asks
/// for one every backoff period for as long as the item is open, which no amount of asking can
/// answer. The worker says which it is. A locked Mac, or one whose screens another session has,
/// shows nothing of the target however it draws, and only someone at the Mac can change that:
/// those two outrank the others (`docs/decisions/video.md`, "The client is told when the Mac is
/// locked").
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum SourceState {
    /// Open and capturing, but the target has produced no frame yet. Nothing to refresh from.
    Idle,
    /// The target has produced a frame; pictures are on the way.
    Live,
    /// The worker's Mac is locked: its screens show the lock screen until someone unlocks it
    /// there. Nothing to refresh from.
    Locked,
    /// The worker's session is off the Mac's screens: they show the login window, or another
    /// user's session after a fast user switch. Nothing to refresh from until it is back.
    Away,
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
        /// How the video is coded: empty for one picture on `stream`, else its stripes, top
        /// first, each on a media stream of its own ([`Stripe`]).
        stripes: Vec<Stripe>,
    },
    /// Stream ended.
    Closed {
        /// Stream.
        stream: StreamId,
        /// Why.
        reason: String,
    },
    /// The target was resized, or the stream turned its stripes on or off: the video that
    /// follows is this size, coded this way.
    Geometry {
        /// Stream.
        stream: StreamId,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// How the video is coded from now on, as in `Opened`.
        stripes: Vec<Stripe>,
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
    /// What a drag over the stream's target does, and drags that begin in it.
    Drag {
        /// Stream.
        stream: StreamId,
        /// What happened.
        event: DragEvent,
    },
    /// The text field that has the keyboard in the stream's target, read after the client's
    /// typing and clicks; sent when it changes. `None` when no text field has it, or the
    /// worker cannot read one.
    Field {
        /// Stream.
        stream: StreamId,
        /// The field.
        field: Option<TextField>,
    },
    /// An [`ScreenRequest::Open`] or [`ScreenRequest::OpenDisplay`] that made no stream. It
    /// names what was asked, since the client learns a stream's id only from its `Opened`.
    OpenFailed {
        /// What was asked for.
        asked: OpenAsk,
        /// Why it did not open.
        why: ScreenFailure,
    },
    /// A [`ScreenRequest::List`] the worker could not answer with a [`Self::Listing`].
    ListFailed {
        /// Why.
        why: ScreenFailure,
    },
    /// Where the curtain over the worker's Mac stands: to every client linked whenever it
    /// changes, and to one that asks ([`ScreenRequest::Curtain`]).
    Curtain(CurtainState),
}

/// Where the curtain over a worker's Mac stands ([`ScreenEvent::Curtain`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CurtainState {
    /// Not drawn: the Mac's screens and input are its own.
    Down,
    /// Drawn, held by `holders` clients.
    Up {
        /// The clients holding it.
        holders: u32,
        /// Its own keyboard and pointer are held off; `false` when the worker could not hold
        /// them (no Accessibility), while its screens are still covered.
        input_held: bool,
    },
    /// The worker could not draw it, in its words: no Mac, or no way to cover its screens.
    Refused {
        /// Why.
        why: String,
    },
}

/// What an open asked for, as [`ScreenEvent::OpenFailed`] names it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum OpenAsk {
    /// A window or a display, by its id ([`ScreenRequest::Open`]).
    Target(CaptureTarget),
    /// A display made for the client, by its key ([`ScreenRequest::OpenDisplay`]).
    Made(DisplayKey),
}

/// Why the worker could not open a stream or list what it can stream. Each kind is one the
/// client answers differently; anything else is the worker's own words.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ScreenFailure {
    /// The worker may not record its screen: its Screen Recording grant is off, and only
    /// someone at that Mac can turn it on.
    NotPermitted,
    /// What was asked for is not there any more: the window closed, the display went.
    Gone,
    /// The worker has no screen to stream (a Linux machine).
    Unsupported,
    /// Anything else, in the worker's words.
    Failed(String),
}

/// A text field that has the keyboard on the worker ([`ScreenEvent::Field`]).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct TextField {
    /// Where its caret is, in stream pixels: what a client's input method hangs its candidate
    /// window under while the client composes. `None` when the field does not say.
    pub caret: Option<Caret>,
    /// It is a password field (`AXSecureTextField`): a client keeps what is typed from other
    /// programs on its own machine (secure keyboard entry) while it has the keyboard.
    pub secure: bool,
}

/// A caret's box in stream pixels: its top left corner and its size (a thin bar as tall as the
/// line).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Caret {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width; 0 for an insertion point.
    pub width: f32,
    /// Height, the line's.
    pub height: f32,
}
