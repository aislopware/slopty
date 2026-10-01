//! The worker's local control protocol: newline-delimited JSON over its Unix socket.
//!
//! One [`CtlRequest`] and one [`CtlReply`] go per connection, with a clipboard's raw bytes after
//! the line that announces them ([`ClipAsk`]).
//!
//! Three kinds of process speak it to `slopty-worker`: the `slopty` CLI (status, doctor, the
//! screen bench), the `slopty hook` relay (hooks and permission requests) and the app, which
//! reads the doctor for "Use this Mac as a worker". Unlike the rest of this crate it is JSON,
//! not postcard: [`Decision::AllowAlways`] carries Claude Code's permission updates as the JSON
//! they arrived as, and the enums are tagged by field.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use slopty_core::{SessionId, WorkerId};

use crate::handoff::{EditOutcome, OfferReason};
use crate::screen::CaptureTarget;
use crate::server::WorkerCaps;
use crate::tailnet::BackendState;
use crate::terminal::SessionSummary;

/// Environment variable naming the session a process runs in (its [`SessionId`]).
///
/// The worker sets it in every shell it spawns, and the `slopty hook` relay reads it to say
/// where a hook fired ([`CtlRequest::Hook`]).
pub const SESSION_ENV: &str = "SLOPTY_SESSION";

/// Environment variable holding the token of the session a process runs in.
///
/// It is a keyed hash of [`SESSION_ENV`] under the worker's key, which only the worker gives
/// out. A program shows it to prove the session it speaks from, to the worker and to the server
/// ([`crate::server::Vouch`]).
pub const SESSION_TOKEN_ENV: &str = "SLOPTY_SESSION_TOKEN";

/// CLI → daemon.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum CtlRequest {
    /// Identity and sessions.
    Status,
    /// Health: permissions, listen address, admitted ranges, connected clients
    /// (`slopty worker doctor`).
    Doctor,
    /// Live screen streams and their worker-side counters (`slopty bench screen` reads the
    /// capture and encode latency through this on loopback).
    Screens,
    /// A coding-agent hook fired inside a session (relayed by `slopty hook`).
    Hook {
        /// The session the hook ran in (`SLOPTY_SESSION`).
        session: SessionId,
        /// The hook's stdin, verbatim JSON.
        payload: String,
    },
    /// A `PermissionRequest` hook, the one the relay waits on: taken in as [`Self::Hook`] is,
    /// then answered with [`CtlReply::Permission`], at once and undecided unless a client
    /// follows the session, and within `wait_ms` whatever happens. The relay sends it instead
    /// of a [`Self::Hook`], never beside one, and keeps its end open while it waits; closing it
    /// withdraws the question.
    Permission(PermissionAsk),
    /// A hook that hands the reports kept for a session over to its agent (`slopty hook
    /// reports`), answered with [`CtlReply::Reports`]. It shows the session's token
    /// ([`SESSION_TOKEN_ENV`]): only the session's own hooks take its reports or say where its
    /// inbox is, so no other program reads them, acknowledges them unread or redirects them.
    Reports(ReportsAsk),
    /// The hook of a [`Self::Reports`] printed batch `batch`: the worker lets it go and tells
    /// the server it was read. Until then it stays, for the next hook to hand over.
    ReportsHanded {
        /// The session (`SLOPTY_SESSION`).
        session: SessionId,
        /// Its token ([`SESSION_TOKEN_ENV`]).
        token: String,
        /// The batch printed.
        batch: u64,
    },
    /// Open a web page for a program in a session (`BROWSER`, the `open` shim) in the browser
    /// of the client in front of that session; answered with [`CtlReply::Handoff`] (`Taken`,
    /// `Offered` or `Nobody`), or an error for an address that is not
    /// [`crate::handoff::is_openable`].
    Open {
        /// The session the program runs in (`SLOPTY_SESSION`), when it runs in one.
        session: Option<SessionId>,
        /// The address.
        url: String,
    },
    /// Show a file in a file tile of the client in front of the session (`EDITOR`, `slopty
    /// edit`), and with `wait`, answer only once the person is done with it. The CLI keeps its
    /// end open while it waits; closing it gives the edit up. Answered with
    /// [`CtlReply::Handoff`]: `Taken` (not waiting), `Edited`, `Lost`, or `Nobody` when no
    /// client could show it.
    Edit(EditAsk),
    /// What keeps the machine awake now.
    Wake,
    /// A program in a session reads or writes the clipboard: the `xclip`, `xsel`, `wl-copy`
    /// and `wl-paste` a Linux session finds first on its `PATH`. Raw bytes travel after the
    /// request line ([`ClipAsk::Write`]) or after the reply line ([`CtlReply::ClipData`]).
    Clip(ClipAsk),
}

/// Which of the worker's clipboards a [`CtlRequest::Clip`] means.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    /// The one synced with the client in front.
    #[default]
    Clipboard,
    /// X11's primary selection: kept on the worker for the programs there, and never synced,
    /// since a Mac has none.
    Primary,
}

/// A program's use of the clipboard ([`CtlRequest::Clip`]). Types are named as X11 and
/// Wayland name them: MIME types, and the X11 names of text (`UTF8_STRING`, `STRING`, `TEXT`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClipAsk {
    /// The types it holds, answered with [`CtlReply::ClipTypes`].
    Types {
        /// Which clipboard.
        selection: Selection,
    },
    /// One type's bytes, answered with [`CtlReply::ClipData`] and then that many bytes, or an
    /// error when it holds no such type. A type the client in front holds may take a moment:
    /// it is fetched from that client.
    Read {
        /// Which clipboard.
        selection: Selection,
        /// The type.
        kind: String,
    },
    /// Replace the contents with `len` bytes of type `kind`, which follow the request line.
    /// `text/uri-list` holds one file or address per line.
    Write {
        /// Which clipboard.
        selection: Selection,
        /// The type.
        kind: String,
        /// How many bytes follow.
        len: u64,
    },
    /// Empty it.
    Clear {
        /// Which clipboard.
        selection: Selection,
    },
}

/// A program's ask to edit a file ([`CtlRequest::Edit`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EditAsk {
    /// The session the program runs in (`SLOPTY_SESSION`), when it runs in one.
    pub session: Option<SessionId>,
    /// The file, as an absolute path.
    pub path: String,
    /// The line to start on, from 1.
    pub line: Option<u32>,
    /// Answer only once the person is done with it.
    pub wait: bool,
}

/// How a handoff went ([`CtlRequest::Open`], [`CtlRequest::Edit`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "handed", rename_all = "snake_case")]
pub enum Handed {
    /// A client took it: the page is open there, or the file shows there.
    Taken {
        /// The client's name.
        client: String,
    },
    /// A client shows the page in a notice, for the person to open or not.
    Offered {
        /// The client's name.
        client: String,
        /// Why it was offered rather than opened.
        why: OfferReason,
    },
    /// No client took it; the program falls back to this machine (the system's opener, `vi`).
    Nobody {
        /// Why.
        why: NoClient,
    },
    /// The person is done with the waiting edit.
    Edited {
        /// How it ended.
        outcome: EditOutcome,
    },
    /// The client showing the waiting edit went away and did not come back.
    Lost,
}

/// Why no client took a handoff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoClient {
    /// No client is connected to the worker.
    NoneConnected,
    /// Clients are connected, and none takes this kind of handoff.
    NoneCapable,
    /// Every client that takes it refused it.
    Refused,
    /// No client that takes it answered in time.
    TimedOut,
}

impl std::fmt::Display for NoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoneConnected => "no client is connected",
            Self::NoneCapable => "no connected client takes it",
            Self::Refused => "every client that takes it refused",
            Self::TimedOut => "no client answered in time",
        })
    }
}

/// What keeps the machine awake ([`CtlRequest::Wake`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Awake {
    /// Clients connected.
    pub clients: usize,
    /// Window and display streams live.
    pub streams: usize,
    /// Agents working or running a tool, whose terminals printed within the silence cap.
    pub agents: usize,
    /// The machine is held out of idle sleep.
    pub system: bool,
    /// The display is held on.
    pub display: bool,
}

/// What `slopty worker doctor` shows: the daemon's own view of its permissions and links.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Health {
    /// Daemon version.
    pub version: String,
    /// Path of the daemon binary (which is what TCC grants permissions to).
    pub exe: String,
    /// What it can do as it last looked, as the server's directory lists it: Screen Recording
    /// is [`WorkerCaps::can_capture`], Accessibility [`WorkerCaps::can_inject`].
    pub caps: WorkerCaps,
    /// Where it listens (`[::]:45550` is every interface, both families).
    pub listen: String,
    /// Address ranges whose peers it admits by address, besides loopback and the tailnet.
    pub allow: Vec<String>,
    /// This machine's Tailscale as the daemon reads it: only while it is up does a tailnet
    /// peer get in.
    pub tailscale: Tailscale,
    /// Reading the pasteboard clipboard sync keeps in step: while reads are not free the
    /// worker announces none of its clipboard's changes.
    pub pasteboard: PasteboardAccess,
    /// Clients connected right now.
    pub clients: usize,
    /// Sessions the worker runs, exited ones kept for their last screen included.
    pub sessions: usize,
    /// Seconds since the daemon started.
    pub uptime_secs: u64,
}

/// This machine's Tailscale, as the daemon reads it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Tailscale {
    /// Logged in and up: tailnet peers reach the worker as this node.
    Up {
        /// Its `MagicDNS` name, without the trailing dot.
        node: String,
        /// Its tailnet IPv4 address.
        ip: Option<IpAddr>,
    },
    /// It answered, and it is not up.
    Down {
        /// Where it stands instead: never [`BackendState::Running`].
        backend: BackendState,
    },
    /// Its `LocalAPI` is there and did not answer.
    Unreachable {
        /// Why.
        error: String,
    },
    /// No Tailscale this daemon can read.
    Absent,
}

/// How reading the worker's pasteboard goes for the daemon, as macOS decides it for a read the
/// person did not make (`slopty_platform::pasteboard_access::Access`, whose `problem` says
/// where to change it).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteboardAccess {
    /// Reads go through without a word.
    Allowed,
    /// The first read will raise the paste alert.
    NotAskedYet,
    /// Every read raises the paste alert.
    Asks,
    /// Every read is refused.
    Denied,
}

/// Daemon → CLI.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum CtlReply {
    /// Status.
    Status {
        /// Worker id: the UUID clients key this worker by.
        id: WorkerId,
        /// Worker name.
        name: String,
        /// Sessions.
        sessions: Vec<SessionSummary>,
    },
    /// Health report.
    Doctor(Box<Health>),
    /// Screen streams.
    Screens {
        /// Open right now.
        live: Vec<ScreenSummary>,
        /// Closed recently, oldest first, with their final counters.
        closed: Vec<ScreenSummary>,
    },
    /// The decision on a [`CtlRequest::Permission`].
    Permission(PermissionAnswer),
    /// How a [`CtlRequest::Open`] or [`CtlRequest::Edit`] went.
    Handoff(Handed),
    /// The answer to [`CtlRequest::Wake`].
    Wake(Awake),
    /// The answer to [`CtlRequest::Reports`]: what the hook prints, when it hands anything
    /// over.
    Reports {
        /// The batch handed over, when one waits.
        batch: Option<u64>,
        /// The hook's output, JSON; none when the agent read the batch already, through its
        /// inbox.
        print: Option<String>,
    },
    /// The types a [`ClipAsk::Types`] found, best first.
    ClipTypes {
        /// The types.
        types: Vec<String>,
    },
    /// A [`ClipAsk::Read`]'s answer: `len` bytes follow the reply line.
    ClipData {
        /// How many bytes follow.
        len: u64,
    },
    /// Done.
    Ok {
        /// Whether anything changed.
        changed: bool,
    },
    /// Failed.
    Error {
        /// Why.
        message: String,
    },
}

/// The `slopty hook` relay's question on a `PermissionRequest` hook (the contract is laid out
/// in `slopty_agent::permission`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAsk {
    /// The terminal session the agent runs in (`SLOPTY_SESSION`).
    pub session: SessionId,
    /// The `PermissionRequest` hook as the relay forwards it (`slopty_agent::Hook::trimmed`):
    /// the tool, its input and the suggested permission updates. The worker reads it once, for
    /// the agent's status and for the prompt.
    pub payload: String,
    /// How long the relay waits; the worker answers before then.
    pub wait_ms: u64,
}

/// The `slopty hook reports` relay's question ([`CtlRequest::Reports`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportsAsk {
    /// The terminal session the agent runs in (`SLOPTY_SESSION`).
    pub session: SessionId,
    /// Its token ([`SESSION_TOKEN_ENV`]).
    pub token: String,
    /// The hook's stdin, verbatim JSON.
    pub payload: String,
    /// The inbox Claude Code takes other programs' messages on in this session, when it has
    /// one (`CLAUDE_CODE_MESSAGING_SOCKET`).
    pub inbox: Option<InboxAt>,
}

/// Where a session's agent takes messages from other programs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxAt {
    /// Its socket.
    pub socket: String,
    /// The token a message shows (`CLAUDE_CODE_MESSAGING_TOKEN`), when the session gave one.
    pub token: Option<String>,
}

/// The worker's answer to a [`PermissionAsk`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAnswer {
    /// What to tell Claude Code.
    pub decision: Decision,
}

/// A decision on one permission request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// No decision: Claude Code shows its own dialog.
    Pass,
    /// Allow this call.
    Allow {
        /// The call's input to use instead of the model's: `AskUserQuestion`'s with the
        /// person's answers, or `ExitPlanMode`'s own. Claude Code takes an allow for a tool that
        /// asks the person only with an input, and shows its own dialog otherwise.
        updated_input: Option<Value>,
    },
    /// Allow this call and apply these permission updates, normally what the request's
    /// `permission_suggestions` offered (an allow rule, a mode, a directory).
    AllowAlways {
        /// The updates, as Claude Code's `updatedPermissions` entries.
        updated_permissions: Vec<Value>,
    },
    /// Refuse the call.
    Deny {
        /// Why, for the model.
        message: String,
        /// Also stop the turn.
        interrupt: bool,
    },
}

/// One screen stream as the control socket lists it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ScreenSummary {
    /// The client that opened it.
    pub client: String,
    /// Stream id on that connection.
    pub stream: u32,
    /// What it captures.
    pub target: CaptureTarget,
    /// Counters (final ones for a closed stream).
    pub stats: ScreenStats,
}

/// A screen stream's worker-side counters, for logs, telemetry and the control socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ScreenStats {
    /// Frames ScreenCaptureKit delivered.
    pub captured: u64,
    /// Frames dropped because the transport already held more than the guard allows
    /// (`slopty_worker::screen::frame_fits`).
    pub dropped: u64,
    /// Frames captured and thrown away because the target was not on screen: the picture was of
    /// whatever is behind it. Zero on a stream whose target never left the screen.
    pub withheld: u64,
    /// Frames held because the accessibility API had just said a window of the target's
    /// application went away and the window list had not yet answered (see
    /// `slopty_worker::screen::SUSPICION_HOLD`). Counted apart from [`Self::withheld`] so a hide
    /// shows where it was caught: here in the first ~260 ms, there once core graphics agrees.
    pub suspected: u64,
    /// Accessibility notifications that raised a suspicion: the target hidden, minimised or
    /// destroyed — or, when the watch could not match the target to its accessibility element,
    /// any window of its application.
    pub suspicions: u64,
    /// Accessibility notifications for a window of the target's application that was not the
    /// target (a sibling window, a pop-up, a tooltip) going away. Never a hold: each one is a
    /// round trip through the window filter, because the capture framework stalls on it
    /// (the worker's `Shared::filter_stalled`).
    pub siblings: u64,
    /// Encoded frames packetized.
    pub encoded: u64,
    /// Datagrams the transport took (data, parity, retransmits, audio, cursor, heartbeats).
    pub datagrams: u64,
    /// Datagrams the transport refused: the connection was gone, or a datagram was larger
    /// than the path carries.
    pub queue_full: u64,
    /// Heartbeats sent while the source was quiet.
    pub heartbeats: u64,
    /// Refresh requests the client sent for this stream (what the receiver's cap bounds).
    pub refreshes: u64,
    /// Times a wanted keyframe was put off because the link could not drain one, counted once
    /// per episode rather than per frame (see `slopty_worker::screen::keyframe_fits`). Each one
    /// sent an LTR refresh in its place and ended either when the link could carry the keyframe
    /// or when the one-second valve opened.
    pub keyframes_deferred: u64,
    /// Worst capture-to-packet latency seen, microseconds.
    pub latency_max_us: u64,
    /// Sum of capture-to-packet latencies, microseconds (divide by `encoded`).
    pub latency_sum_us: u64,
    /// Opus packets sent.
    pub audio_packets: u64,
    /// Bitrate the controller last asked the encoder for.
    pub bitrate_bps: u64,
    /// Capture latency: the window server's display time of a frame → ScreenCaptureKit's
    /// callback (what SCK adds), over the worker's latency window (`LATENCY_WINDOW` frames).
    pub capture: Quantiles,
    /// Encode latency: `VTCompressionSessionEncodeFrame` → the output callback.
    pub encode: Quantiles,
    /// Time between two heartbeats, over the last `LATENCY_WINDOW` of them. The beat is what
    /// tells the receiver the worker is alive while nothing is being drawn, and the receiver calls
    /// a silence of `STALL_GAP` a stall, so this is the number that says whether the worker is
    /// keeping its own promise.
    pub beat_gap: Quantiles,
    /// How long the geometry probe took (its window-server reads, off the runtime), over the
    /// last `LATENCY_WINDOW` of them: the work the beat used to wait behind.
    pub bounds: Quantiles,
    /// The longest gap between two beats since the stream opened, microseconds. The quantiles
    /// above are over a sliding window of `LATENCY_WINDOW` beats — about twenty seconds — so
    /// a single late beat early in a long stream would be gone from them by the end. This is
    /// the one that cannot forget, and it is what a rule about the beat has to be written on.
    pub beat_gap_worst_us: u64,
    /// Frames sent from the display-crop path (a window served as a `sourceRect` of its display
    /// rather than through the window filter). Frames the crop delivered after it stopped holding
    /// the target are not among them — those are [`Self::withheld`].
    pub cropped: u64,
    /// Long-term references: offered, acknowledged, whether one is usable now, and how the
    /// refreshes were answered.
    ///
    /// What it costs to ask for a refresh. With a usable reference the encoder answers
    /// `force_ltr_refresh` with a delta off it — 733 B against a 3 998 B IDR in
    /// `a_forced_ltr_refresh_is_a_delta_not_an_idr` — and without one it falls back to a full
    /// keyframe.
    pub ltr: LtrStats,
    /// Bitrate the encoder was last given: the controller's target less the parity share
    /// (`slopty_worker::screen::encoder_bps`).
    pub encoder_bps: u64,
    /// Frames encoded from the held capture rather than a fresh one: the last capture of a
    /// picture that went still, or a refresh or keyframe answered while nothing changed.
    pub repaired: u64,
    /// Refinement frames of a still picture: the held capture coded again at a finer quality
    /// once it stopped changing. Sent and painted like any frame, and not among
    /// [`Self::encoded`], which counts the source's frames only: painted frames are at most
    /// `encoded + refined`.
    pub refined: u64,
    /// Captures a newer capture replaced in the encoder's mailbox before the encoder took them:
    /// the source drew faster than the encoder took pictures. Never encoded.
    pub superseded: u64,
    /// Video datagrams that waited in the audio lane past their frame's own hand-over.
    pub laned: u64,
    /// Whether the stream is on the display-crop path *right now*. The counter above says how
    /// many frames came that way; this says where the next one will come from, which is what a
    /// test asking "did the crop go away when the window did" has to look at.
    pub on_crop: bool,
}

/// What the long-term reference machinery did on a stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct LtrStats {
    /// Frames the encoder marked as long-term references (tokens offered).
    pub offered: u64,
    /// Tokens the client acknowledged that this encoder session had offered.
    pub acked: u64,
    /// Refreshes the encoder answered with an IDR: no usable reference behind them.
    pub refreshes_idr: u64,
    /// Refreshes the encoder answered with a delta off an acknowledged reference.
    pub refreshes_delta: u64,
    /// Whether an acknowledged reference newer than the latest keyframe is on record now, so a
    /// refresh would be a delta.
    pub usable: bool,
    /// Age of that reference, microseconds; 0 when there is none.
    pub usable_age_us: u64,
}

/// p50 / p95 / max of a latency over the worker's latency window, microseconds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Quantiles {
    /// Samples in the window.
    pub n: u32,
    /// Median.
    pub p50_us: u64,
    /// 95th percentile.
    pub p95_us: u64,
    /// Worst in the window.
    pub max_us: u64,
}

impl Quantiles {
    /// Quantiles of `samples` (any order).
    #[must_use]
    pub fn of(samples: &[u64]) -> Self {
        Self::of_owned(samples.to_vec())
    }

    /// Quantiles of `samples` (any order), sorted in place.
    #[must_use]
    pub fn of_owned(mut sorted: Vec<u64>) -> Self {
        sorted.sort_unstable();
        let last = sorted.len().saturating_sub(1);
        let at = |q: f64| -> u64 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "an index below 2^53"
            )]
            let i = (last as f64 * q).round() as usize;
            sorted.get(i.min(last)).copied().unwrap_or(0)
        };
        Self {
            n: u32::try_from(sorted.len()).unwrap_or(u32::MAX),
            p50_us: at(0.5),
            p95_us: at(0.95),
            max_us: sorted.last().copied().unwrap_or(0),
        }
    }

    /// `p50 / p95 / max ms (n)`.
    #[must_use]
    pub fn describe(&self) -> String {
        #[expect(clippy::cast_precision_loss, reason = "microseconds well below 2^53")]
        let ms = |us: u64| us as f64 / 1e3;
        format!(
            "{:.2} / {:.2} / {:.2} ms (n={})",
            ms(self.p50_us),
            ms(self.p95_us),
            ms(self.max_us),
            self.n
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The relay's lines, as `slopty_agent::permission` documents them, are these variants.
    #[test]
    fn a_permission_request_and_its_decision_are_single_json_lines() {
        let session = SessionId::nil();
        let ask = CtlRequest::Permission(PermissionAsk {
            session,
            payload: "{}".to_owned(),
            wait_ms: 1_000,
        });
        let line =
            json!({ "cmd": "permission", "session": session, "payload": "{}", "wait_ms": 1_000 });
        assert_eq!(serde_json::to_value(&ask).ok(), Some(line.clone()));
        assert_eq!(serde_json::from_value::<CtlRequest>(line).ok(), Some(ask));
        let reply = CtlReply::Permission(PermissionAnswer {
            decision: Decision::Deny { message: "no".to_owned(), interrupt: false },
        });
        assert_eq!(
            serde_json::to_value(&reply).ok(),
            Some(json!({
                "reply": "permission",
                "decision": { "kind": "deny", "message": "no", "interrupt": false },
            }))
        );
    }

    #[test]
    fn quantiles_read_any_order() {
        assert_eq!(Quantiles::of(&[]), Quantiles::default());
        let q = Quantiles::of(&[5, 1, 3, 2, 4]);
        assert_eq!((q.n, q.p50_us, q.p95_us, q.max_us), (5, 3, 5, 5));
        assert_eq!(Quantiles::of(&[7]).describe(), "0.01 / 0.01 / 0.01 ms (n=1)");
    }
}
