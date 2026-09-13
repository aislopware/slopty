# Slopty architecture

Slopty is a remote-coding workstation: a macOS **host** exposes shells and windows; macOS and iOS
**clients** show them on one infinite canvas, mixed freely, with Claude Code agents surfaced as
first-class objects. Everything is Rust. Floor: macOS 26.5 / iOS 26.5, Apple silicon.

This file is the map. Rulings and their evidence live under [docs/decisions/](DECISIONS.md), one file per topic.
How the work itself is organised across agent sessions is in `docs/ORCHESTRATION.md`.

## 1. Shape

```
                     ┌──────────────────────────── host (macOS) ────────────────────────────┐
                     │  slopty-ptyd            slopty-hostd                                  │
                     │  ┌────────────┐  fd     ┌───────────────────────────────────────┐    │
   shells/agents ◀──▶│  │ PTY keeper │ ──────▶ │ sessions · VT engine (libghostty-vt)  │    │
                     │  │ (tiny,     │ SCM_    │ frame diff · replay · fan-out          │    │
                     │  │  outlives  │ RIGHTS  │ agent status (hooks/JSONL)             │    │
                     │  │  hostd)    │         │ screen: SCK → VT HEVC → FEC → datagrams│    │
                     │  └────────────┘         │ input: CGEvent injection               │    │
                     │                         └──────────────┬────────────────────────┘    │
                     └────────────────────────────────────────┼─────────────────────────────┘
                                                              │ iroh (QUIC): streams = control/terminal
                                                              │              datagrams = video/audio/cursor
                     ┌────────────────────────────────────────┼─────────────────────────────┐
                     │ client (macOS app · iOS app)           ▼                              │
                     │ slopty-client: session state · line cache · prediction · canvas doc  │
                     │ slopty-ui (GPUI): canvas · terminal element · surface element · chrome│
                     │ slopty-codec (decode) → CVPixelBuffer → gpui surface (zero copy)      │
                     └───────────────────────────────────────────────────────────────────────┘
```

Three kinds of traffic, one QUIC connection per (client, host):

| Path | QUIC primitive | Payload |
|---|---|---|
| Control | one bidirectional stream, length-prefixed `postcard` messages | hello, auth, open/close, resize, canvas doc sync, agent events |
| Terminal | one unidirectional stream per session, host→client; input on the control stream | grid **row diffs** from the host-side VT engine, scrollback line pages on demand |
| Media | unreliable datagrams (RFC 9221) | HEVC fragments + Reed–Solomon parity, Opus audio, cursor position/shape; client → host: `Feedback` (NACK, refresh), each datagram standing alone so a lost one never holds the next back |

## 2. Terminal: the VT engine lives on the host

The host runs `libghostty-vt` against the real PTY and ships **rendered rows**, not bytes
(mosh / zellij / wezterm-mux model). Consequences:

- Reconnect and multi-client are proven, not just cheap (`crates/slopty-e2e/tests/pair.rs`,
  `cargo xtask e2e pair`): a joining client receives the current screen plus a scrollback window,
  nothing is replayed. Two clients share one host and one canvas at once. A terminal opened on one
  is on the other within a round trip; both type into the same session and the actor serialises
  their keys in arrival order (nothing lost or reordered); one client is the size **driver** (§2,
  the "take" pill) while the rest are viewers; an agent's attention badges every client; a client
  that dies leaves the others streaming and reattaches on relaunch (its dead connection's cleanup
  detaches only its own sinks, `Sender::same_channel`). See DECISIONS "Multi-client" for the
  state-ownership and input-contention rulings.
- A slow link never falls behind a fast program: the host coalesces to one diff per tick.
- The client needs no VT engine at all. iOS never builds Zig.
- The client keeps a **line cache** (absolute line numbers) so scrollback scrolls locally; missing
  ranges are fetched, prefetched around the viewport; the cache indexes its prompt rows, so a
  block's prompt is a range query, not a walk. Mouse selection is client-side too
  (absolute line indices, ⌘C copies from the cache, ⌘V sends `Paste`; drag, double/triple click,
  ⇧-click to move the near end, or long-press on touch; a drag past the grid's top or bottom
  keeps scrolling through the cache at a pace set by the distance); nothing reaches the host.
  A scrollbar thumb over the grid's right edge shows while there is history and the pointer is
  over the card or the viewport is in the history; it drags and its track pages. The wheel
  scrolls the cache in whole lines (fractions carried between events), except for a program
  tracking the mouse or the alternate screen, whose rows go to the host as `Wheel` (button
  presses, or cursor keys under alternate scroll). ⌘-click opens the link under the pointer:
the OSC 8 run the host put on the row, else the URL found in the cached row text
(`slopty-ui::terminal::url`); ⌘-hover underlines it. A file path under the pointer
(`src/main.rs:12:5`, `url::path_at_col`) opens the same way: ⌘-click types
`${EDITOR:-vi} +12 'src/main.rs'` at the prompt, or opens a file card for it while a command
runs and on a tap the phone's key-bar ⌘ armed (`TerminalViewEvent::ViewFile`, the canvas
making the path absolute against the shell's directory, `canvas::absolute_in_session`).
- **Prediction**: the client applies mosh-style speculative echo for printable keys, confidence
  gated on measured RTT, reconciled against the next authoritative diff (see `slopty-predict`).
- Terminal size is owned by one **driver** client (the one that opened the session, else the
  first to attach; anyone can take it with the "take" pill, which also fits the item to their
  viewport); viewers see the driver's grid.

**Absolute line numbering.** Clients cache scrollback by `LineIndex` (absolute, monotonic). The
engine keeps a libghostty *tracked grid ref* pinned to the newest active row and re-derives the
index of screen row 0 after every write, so host-side eviction never shifts indices. When
numbering cannot survive — reflow on resize, RIS, alternate-screen switch, a single write that
scrolls past the whole scrollback — the frame's `epoch` bumps and clients drop their cache.
Returning from the alternate screen restores the parked primary anchor, so the cache survives
`vim`/`less` round trips.

**Search** is a host request (`TermRequest::Search` → `TermEvent::Matches`): the engine renders
the retained rows as plain text with libghostty's formatter and maps hits back to cells, so the
whole 50k-line history is searchable without the client ever holding it. `regex: true` runs the
needle through the `regex` crate instead of the literal matcher; a bad pattern comes back as
`TermEvent::SearchInvalid`.

**Escapes programs rely on.** OSC 8 links ride on the rows: the engine asks libghostty for the
URI of every linked cell (`ghostty_grid_ref_hyperlink_uri`, only on rows whose page flag says
they may hold one) and folds them into `Line::links`, a list of `Hyperlink { col, len, uri }`
runs, so a link-free row costs one byte and no cell carries an id. The client draws the run
under the pointer underlined while ⌘ is held and opens the URI on ⌘-click; the OSC 8 target
wins over the plain-text URL scan (`slopty_ui::terminal::url`). OSC 52 (and iTerm2 OSC 1337
Copy) writes to the *system* clipboard become `TermEvent::ClipboardWrite`, capped at
`MAX_CLIPBOARD_BYTES`, and every attached client puts the text on its own clipboard;
selection/primary targets and every read (`?`) are dropped on the host, and no message exists
for a read reply.

**Command blocks (OSC 133).** `slopty-ptyd` injects shell integration for zsh, bash and fish:
at every start it writes the bundled scripts (`slopty-pty/assets/shell`, compiled in with
`include_str!`, so nothing reads the source tree at runtime) under `<data dir>/shell` and
`ShellIntegration::apply` rewrites the spawn per shell. zsh gets `ZDOTDIR` pointing at the
bootstrap `.zshenv`, which hands `ZDOTDIR` back to the user, sources their own `.zshenv`, and
for interactive shells adds precmd/preexec hooks. bash gets `--rcfile <shell>/bash/slopty.bash`
in front of its arguments (its `-l` and leading-dash arg0 travel as `SLOPTY_BASH_LOGIN=1`,
because bash ignores `--rcfile` for login shells); the rcfile sources the user's profile or
`.bashrc` as bash would have, then adds prompt hooks and a `DEBUG` trap for preexec (or
registers with bash-preexec when the user loads it). fish gets `<shell>/fish` in front of
`XDG_DATA_DIRS` so its `vendor_conf.d/slopty.fish` loads before the user's `config.fish`;
fish ≥ 4 emits the marks itself and the snippet steps aside. All three emit `133;A` (in the
prompt), `B`, `C` and `D;<status>`. `SLOPTY_NO_SHELL_INTEGRATION=1` in the daemon's or the
session's environment opts out (`apply` then changes nothing); other programs run untouched. The engine takes libghostty's per-row prompt flag for the row kind and, because
the library exposes neither which row an `A` landed on nor the status a `D` carries, scans the
PTY bytes for those two marks itself (`slopty_engine::osc133::Scanner`, state kept across
reads so a mark split over two PTY reads is still found) and notes the cursor's absolute line
at each. Every `Line` then carries a `SemanticMark`: `Prompt { exit, input }` on the row a
prompt started (with the previous command's status), `PromptContinuation { input }` for the
rest of a multi-line prompt, `Input`, `Output`; `input` is the column of the row's first
`Input` cell (libghostty's per-cell `CellSemanticContent`), where the typed command begins. The client side is in §6.

**Render metrics.** The cell grid is derived exactly as ghostty derives it
(`slopty_ui::terminal::metrics`, ported from `vendor/ghostty/src/font/Metrics.zig`): a pure
function from a face — advance, ascent, descent, line gap, and the underline/strikethrough
metrics when the font has them — to whole **device** pixels for the cell, the baseline, the
underline, the strikethrough, the overline and the cursor. The face comes from the font's own
tables through the fork's `TextSystem::font_metrics` (`hhea` line gap, `post` underline
position and thickness; Core Text on both platforms), with ghostty's estimates only where a
font says zero; the self-test dump reports it (`terminals[].face`). The element measures the
face at `font_size × scale`, derives once — a zoomed grid is that one scaled, so the columns that fit
still fit — and `Grid` divides back to logical points, so a row is pixel-aligned on the display
it was measured for; `typography.mono_line_height` is ghostty's
`adjust-cell-height` percentage on top (1.0 = the font's own). The element paints underlines
and strikethroughs itself, at those offsets, because GPUI puts them elsewhere — only the curly
underline is left to GPUI. Glyphs are painted on the derived baseline rather than
GPUI's centred one. The same derived cell goes on the wire in `TermSize.metrics`, which becomes
the PTY's `ws_xpixel`/`ws_ypixel`, and pixel mouse reports are measured in it too.

**PTY custody.** `slopty-ptyd` spawns the child (own session, slave as controlling tty), keeps
the master, and drains it into a bounded ring while no host holds it. `Attach` pauses the reader
(handshake through a `watch` pair so the fd is never read by two parties), ships the ring
contents plus the master over `SCM_RIGHTS`, and hostd reads the fd directly from then on —
no relay hop. Losing the hostd connection resumes draining; the child never blocks. While a
host holds the master it feeds the ring itself (`Output`, a copy of every read, on the same
connection, which is the only one ptyd accepts taps from) and replaces the ring with a
`Checkpoint`, the engine's whole state as VT bytes from libghostty-vt's formatter (palette,
modes, every retained row, margins, cursor): right after adopting, then 500 ms after the last
output (deferred while the output stands inside an escape sequence) or once 1 MiB has been
tapped. `Attach` returns the checkpoint and the ring, and the new host's engine replays them in
that order, so the screen, the scrollback and the title survive a hostd restart or crash. A bare
program name the daemon cannot find on its own `PATH` runs through the user's login shell,
interactive (`$SHELL -lic '…'`), so rc-file `PATH`s and aliases apply.

**The shell's environment.** Every child gets `TERM`, `COLORTERM=truecolor`, `TERM_PROGRAM`,
`TERM_PROGRAM_VERSION` and the shell integration's `ZDOTDIR`, on top of ptyd's own. `TERM` is
`xterm-ghostty` once ghostty's terminfo is compiled, else `xterm-256color`. ptyd compiles it
itself on start-up — `slopty_pty::terminfo` holds ghostty's entry as const data, renders the
source, and runs `/usr/bin/tic -x -o $HOME/.terminfo -` in a background task — so a fresh
machine gets the full capability set without anyone installing ghostty. The install is
idempotent, nothing waits for it (`TERM` is read per spawn), and `$SLOPTY_TERMINFO_DIR`
redirects the database for tests.

**Deployment.** `slopty host install` writes two LaunchAgents (`dev.aislopware.slopty.ptyd`,
`dev.aislopware.slopty.hostd`; `KeepAlive`, `RunAtLoad`, `ProcessType Interactive`) with the
sockets under `<data dir>/run/` and logs in `~/Library/Logs/Slopty`, bootstraps them, and
prints a pairing ticket; `uninstall` and `service` undo and report. The CLI finds the
installed socket by itself, so `slopty host ticket` works without launchd's environment.
`slopty host doctor` asks the running daemon about itself (`CtlRequest::Doctor` →
`Health`): Screen Recording and Accessibility as *that binary* sees them (TCC grants are per
executable, so the report names the path to add), reach, port, connected clients, sessions;
exit status 1 while a permission is missing, so it can gate a setup script.

Crates: `slopty-engine` (trait + libghostty-vt backend), `slopty-grid` (frame model, diff, cache),
`slopty-predict`, `slopty-pty` (openpty/spawn, async master, ptyd protocol + client),
`slopty-host`; app `slopty-ptyd`.

## 3. Remote windows: Parsec-class pipeline

```
SCStream(display | window-as-display-crop | window, 420v/P010, minimumFrameInterval, queueDepth=2, showsCursor=false)
  → VTCompressionSession(HEVC, EnableLowLatencyRateControl @ creation, RealTime,
                         AllowFrameReordering=false, AllowOpenGOP=false, MaxFrameDelayCount=0,
                         EnableLTR, MaxKeyFrameInterval=∞, AverageBitRate + DataRateLimits)
  → packetize (≤1200 B datagrams, 16 B header) → reed-solomon-simd parity per frame
  → iroh datagrams                                      ── client: NACK/refresh datagrams; LTR acks + telemetry on the control stream
client: reassemble/recover → VTDecompressionSession(RealTime) → CVPixelBuffer (IOSurface)
  → gpui surface (CVMetalTextureCache, zero copy) → present on arrival (vsync off)
```

Loss recovery order: FEC (free) → NACK inside the playout window → `ForceLTRRefresh` (small
P-frame from an acked LTR) → IDR only when no acked LTR exists. The NACK delay is not a
constant: it is a quarter of the transport's measured round trip, floored at 1 ms and capped at
20 ms (`slopty_media::NackDelay`), so a loopback link repairs in a millisecond and a 40 ms one
waits 10 ms rather than asking again for fragments still in flight. Cursor is a separate
low-rate channel drawn client-side, so pointer latency is one RTT, not one video pipeline.

**How much parity.** The host's `Redundancy` turns the receiver reports into the ratio the
packetizer cuts each frame with: parity tracks twice the smoothed datagram loss over a 5 % floor,
up to a 50 % ceiling (past that a smaller picture beats a better-protected one), and a report
that says a frame was *lost* buys half again on the spot — parity was demonstrably not enough.
The smoothing is asymmetric, half weight on a rising sample and an eighth on a falling one,
because loss arrives in bursts: protection has to be there for the second half of a burst and
must not be given back by one clean report. A change smaller than 2 % does not move the ratio at
all, since every change re-cuts the frame layout. Windows the receiver spent stalled are excluded
from the estimate: a link holding packets and releasing them together is not a link dropping
them, and the fragments such a window reports missing usually arrive with the release. Every
frame gets at least one parity fragment whatever the ratio works out to, which on a still window
(two to five fragments per frame) is 240–440 ‰ of overhead and on a busy one is nothing —
measured, and measured again with the minimum removed, which cost frames at 20 ‰ loss for a
saving of ~150 kbit/s.

**Whose silence is it.** A receiver that sees nothing arrive cannot assume the link is at fault:
a capture with nothing to draw is silent too, and the heartbeat that says so can itself be late.
Every datagram carries `send_ms_lo`, the low byte of the host's send clock, so the difference
between two stamps is how long the host waited between sending them; subtracting that from the
arrival gap leaves the link's share, and only that counts as a stall or is charged to
`stalled_ms`. Past the stamp's 256 ms range, on a retransmission (which carries its original
frame's stamp) and while a gap is still open the receiver keeps the pessimistic reading. This
matters beyond the HUD: the bitrate and parity controllers both throw away windows the receiver
spent stalled, so mislabelling a quiet source as a stall threw away good evidence.

**When to stop asking.** A receiver that needs a refresh repeats the request with a doubling
backoff, which is right for a stream that has stopped and wrong for a target that never started:
a hidden window produces no frames, so no amount of asking helps. The host answers that directly
— `ScreenEvent::Source { state: Idle | Live }` (protocol 13), sent 400 ms after `Opened` if the
encoder has produced nothing and again the moment it does — and the receiver stops asking while
the source is idle (`Reassembler::set_source_live`), with the element's placeholder saying
"waiting for the window to draw" instead of "waiting for the first frame". For a host too silent
to send the hint, `Config::refresh_max_repeats` (12, ≈17 s with the backoff) is the fallback cap.
The two are cleared by different evidence: *any* datagram on the stream — a heartbeat included —
restarts the cap, because it proves the host is still there, while only a video fragment lifts the
idle suppression, because only a picture proves the source is drawing (a hint that has not changed
never overwrites what the stream itself proved). The hint follows the *recent* frames, not the
first one (`SourceTracker`, clock injected so the rule is unit-tested): a new frame is `Live` at
once, `SOURCE_QUIET_AFTER` (2 s) without one is `Idle` again — longer than the start-up grace so a
target drawing once a second does not flap — and a target the geometry tick reports off screen is
`Idle` immediately, since a window that is not on screen cannot be drawing.

**Host capture path.** A display target is one `SCContentFilter(display:)`. A window target
is served two ways, and the host switches between them on the live stream
(`updateContentFilter` + `updateConfiguration`, no restart, no new encoder): while the window
is on screen, sits entirely on one display and no window counts as covering it
(`crop_allowed`; other processes at levels 0–8 and sibling windows of the same app count,
the window's own menus and sheets, the Dock's full-screen hit region, the menu bar and
status items do not: `counts_as_occluder`), the stream is the *display* filter restricted
to the window's application (`display:includingApplications:`, so the audio is that app's
only) with `sourceRect` at the window's frame (`Target::resolve_crop`,
`WindowPath::DisplayCrop`), which ScreenCaptureKit serves from the frame it already
composited; otherwise it is the independent-window filter (`WindowPath::Filter`), which
composites the window on its own and costs a few milliseconds more per frame
(MEASUREMENTS.md, "capture floor"). `check_geometry` (10 Hz) follows moves by updating the
crop, resizes by rebuilding the encoder, and occlusion by swapping the filter (the swap keeps
the stream and its frame counter, so the `Source` hint never flips on the swap itself); each switch
is a `Transition` that becomes the stream's state only when every ScreenCaptureKit completion
callback has succeeded, a failed one is asked again next tick. A window that closes under a
crop ends the stream the way the window filter does (`on_stop` → `Closed`), never showing the
desktop where it was. The tick's verdict on the target also reaches the frame path directly:
`Shared::target_hidden` is set from `window_on_screen` *before* the transition machinery, which
returns early while a swap is in flight, and `on_frame` drops anything captured while it holds
(counted as `ScreenStats::withheld`). That closes the window between deciding the crop is wrong
and ScreenCaptureKit acknowledging it, in which the crop is still live over a rectangle that now
holds the desktop; `ScreenStats::on_crop` says which path a stream is on right now, as against
`cropped`, which counts the frames that came that way, and both readers go through
`Shared::stats` so neither can publish the counters' placeholder. The larger gap before it —
CoreGraphics keeps reporting a window on screen for ~260 ms after it is ordered out, whichever
way it is asked — is closed from the other side: a `slopty_capture::HideWatch` (an `AXObserver`
on the window's application, on its own `CFRunLoop` thread) hears AppKit order a window out the
moment it happens, and its callback opens a `SUSPICION_HOLD` (400 ms) during which `on_frame`
holds every frame (`ScreenStats::suspected`). Accessibility cannot name a window, so the watch
matches the target's element once at registration (frame within 1 pt and title against the
window list) and tells it from the application's other windows by identity: the target going
is the suspicion, and the geometry tick's `target_hidden` is the confirmation that outlives it;
when nothing matched, any window going is the suspicion and costs a ~500 ms freeze, never a
frame of the desktop. During the hold `follow_window` moves the stream to the window filter and
brings the crop back on the first tick after it: ScreenCaptureKit stops delivering for an
application-scoped display filter once a window of that application is ordered out, and only a
change of filter kind wakes it. Another window of the application going (`ScreenStats::siblings`:
a sibling, a pop-up, a tooltip) is therefore no hold but the same round trip, taken with frames
flowing (DECISIONS.md "A suspicion moves the stream to the window filter"). Without accessibility trust the watch is simply absent and the
crop carries black rather than what is behind the window for those ~260 ms, in every case that
has been measured including another application's window: DECISIONS.md "The accessibility API
knows about a hide 260 ms before core graphics does" and "A crop cannot be made to show another
application" have the reasons and the numbers. The pure crop geometry (`slopty_capture::crop_for`: window frame →
display-relative points, pixels at the display's scale, `None` when not entirely on that
display) and the occluder rule are unit-tested; DECISIONS.md "Rulings of the crop path" has
the list. `SLOPTY_WINDOW_CAPTURE=window|crop` on hostd forces a path. Every frame carries the window server's display time
(`SCStreamFrameInfoDisplayTime`, equal to the sample's pts) and the host keeps two latency
rings per stream — display time → SCK callback (`ScreenStats::capture`) and encoder submit →
VideoToolbox callback (`ScreenStats::encode`), p50/p95/max over the last 600 frames — read
locally over the control socket (`slopty host screens`; `slopty bench screen` appends them on
loopback). Nothing of this crosses the wire.

**Presentation path.** The reassembler stamps every complete frame with the arrival of the
datagram that finished it (`FrameOut::arrived`); the stream worker parks that instant under the
frame's presentation timestamp, and the VideoToolbox callback — which is given nothing but that
timestamp — picks it back up, so a decoded picture reaches the UI as a `Presentable`
(`CVPixelBuffer` + arrival + decode instants) on a `watch` channel that keeps only the newest.
In the element a `slopty_client::Pacer` owns the one decision left: **present on arrival**. A
frame goes up on the first paint after the decoder returns it and is never queued for a later
one — a queue would buy smoother spacing at the cost of a whole frame of latency on every frame,
which is the wrong trade for a screen. When the decoder runs ahead of the display the pacer
*replaces* the frame waiting to be painted instead of lining up behind it (`skipped`); when it
runs behind, the paint shows the same picture again (`repeats`); a frame not newer than what is
up is dropped (`late`). The pacer is also the instrument: a ring of the last 240 presented
frames gives arrival → present p50/p95/max, the decoder's share of it, the spacing of the paints
and that spacing's jitter — read by the ⌘⇧I overlay's third line (`hud_lines`) and by the app
self-test's `dump` (`ScreenInfo`). The policy is pure and clock-injected
(`slopty_client::pacing`), so it is unit-tested without a window; the element only feeds it a
frame on one side and a paint on the other.

**Audio** rides the same stream: ScreenCaptureKit captures the target's audio (48 kHz stereo,
this process excluded) → `AudioConverter` Opus (Apple's, in the OS; 20 ms packets, 96 kb/s) →
one `Audio` datagram per packet, no FEC and no NACK (a lost 20 ms is cheaper than a late one).
The host stops sending 300 ms after the last non-silent sample, so silent apps cost nothing.
The client decodes with `AudioConverter` and plays through an `AudioQueue` fed from a 200 ms
ring that pads silence on underrun and drops the oldest on overrun (`slopty-codec::audio`);
iOS puts the app in the `Playback` session category so it plays past the ring switch.
Mute is per item and per client (the "mute" pill, ⌘⇧M, Canvas ▸ Mute Window): packets still
arrive and decode, only playback stops, so unmuting is instant and other clients hear nothing
different.

**Clipboard** follows the window both ways, text only (≤ 256 KiB). Host → client: hostd polls
`NSPasteboard.changeCount` every 200 ms (`slopty-input::Pasteboard`) and broadcasts
`ScreenEvent::Clipboard`, which each connection forwards only while that client has a window
open; the canvas writes it to the local clipboard unless it already holds the same text (with
the host on the same Mac that write would bump the count the host watches and echo forever).
Client → host: a ⌘V into a window first sends `ScreenRequest::Clipboard` on the ordered
control stream, so the host pastes what the client copied; the view remembers what the host
holds and skips the push when nothing changed. Writes the host makes for a client are not
reported back by the poller.

Crates: `slopty-capture` (SCK streams, shareable content, pointer/bounds queries),
`slopty-codec` (encode half is `cfg(macos)`), `slopty-media` (`Packetizer` → datagrams + parity +
retransmit history; `Reassembler` → in-order frames, NACK/refresh `Action`s, `ReceiverReport`;
`Redundancy` → parity ratio; `RateController` → encoder bitrate from the reports and the QUIC
path's cwnd/rtt, `judge` the pure policy over one decision window: a stall the reassembler
reported (`stalled_ms` / `stalls` in the report) freezes the target, loss while flowing cuts
it, a clean window grows it; every decision goes back to the client as `ScreenEvent::Rate`
for the stats overlay and the bench; pure, no clocks, tested; `heartbeat_datagram` → a bare
`Kind::Heartbeat` header the host sends after `HEARTBEAT_AFTER` of silence so a still screen
or a capture gap does not read as a link stall at the receiver), `slopty-host::screen`
(`ScreenStream`: capture → encode → packetize into a bounded queue; `DatagramBudget` tracks the
path's datagram limit and how many bytes QUIC is holding in its send buffer, and a captured
frame is dropped rather than encoded while more than two frames' worth wait there
(`frame_fits`); `warm_up` runs one throwaway capture when hostd comes online and `shareable()`
keeps its enumeration for 2 s; a cursor sampler whose window-server calls all go through the
blocking pool, and a heartbeat on its own task beside it — a promise about time must not share a
task with a call that takes it (DECISIONS.md, "The heartbeat has its own task"); input injection), `slopty-input` (client
`ScreenInput` → `CGEvent`, posted to the owning pid for windows or the HID tap for displays,
right clicks always through the HID tap because AppKit only tracks context menus for those;
activates the owner before clicks and keys because macOS only delivers keyboard events to the
active app). `slopty-hostd` owns one datagram pump
per connection (it also measures the QUIC hold and logs every stretch of it) and maps
`ScreenRequest`s onto the streams it opened for that client. Client side, `HostLink::start`
warms VideoToolbox's decoder up once per process, the router stamps each datagram with its
arrival, and the stream worker drains what is queued before running the reassembler's timers
and pushes whatever the timers released straight into the decoder (a frame freed by a loss can
otherwise wait for the next datagram, which on a still screen is a heartbeat away);
`ScreenStats` carries hold, jitter and the start-up instants for the ⌘⇧I overlay
(`hud_lines`) and `slopty bench screen`, and `slopty-client::pacing` carries the arrival →
present numbers beside them. Transport: ACKs within 2 ms and a 32-packet initial
window (`slopty-net::endpoint`).

## 4. Canvas

One infinite 2D plane per workspace (kolu model). Items: terminal, remote window, remote display,
note, file (`ItemKind` in `crates/slopty-proto/src/canvas.rs`). Camera `{x, y, zoom}`; zoom is real (we own the renderer), with semantic LOD: full terminal
at ≥ 0.6×, summary card (title, last lines, agent state) below. Off-screen terminals keep their
line cache and stop painting; off-screen video pauses decode (kind-aware culling). Layout is a
document synced through the host so every client sees the same canvas — item geometry, z, sleep
and note text are host state; the camera `{x, y, zoom}` is per client, so panning and zoom on one
client do not move another (`slopty-host::canvas` owns the document,
`slopty-client::canvas` mirrors it with optimistic local ops, `slopty-ui::canvas` draws it: two-finger scroll pans, pinch / ⌘-scroll zooms about the pointer, title bar drags, corner
grip resizes, ⌘T/⌘⇧N/⌘O/⌘W/⌘0/⌘1/⌘=/⌘-/⌘⇧A/⌘]/⌘[/⌘⇧F are the keyboard surface, the last
one a find in every card (the palette lists the cards with hits, ↩ opens that card's find bar), the two before it
walking the cards in reading order; a minimap in the corner
shows every item and the viewport and scrubs the camera). Notes (⌘⇧N) are edited
in place (`slopty-ui::note`) and their text lives in the document; a note nobody is editing
draws that text as Markdown in the conversation's own style (`slopty-ui::markdown`), its
fenced blocks as the conversation's block element with "copy" and, given a shell, "run"
(`markdown::code_block`, `NoteViewEvent::Run` → `run_in_shell`), and a
click on it puts the caret back in the editor; its title bar reads the first non-empty
line (`canvas::note_title`, heading and list marks stripped, 40 chars), "note" while empty.
Any card takes a **name** (⌘E, or a double-click on its title bar; `CanvasItem.name`,
protocol 37, host-sanitised to 128 characters): document state every client shows in place
of the derived title until it is cleared, and what the palette's "Go to" line says. A
**file card** (`ItemKind::File { path }`, protocol 34, `slopty-ui::file`) shows a file on the
host read-only: the item names the absolute path and lives in the shared document, the text
does not — each client asks `ClientMsg::ReadFile` when the card appears (`canvas::reconcile_files`,
which also sends the set of paths as `ClientMsg::WatchFiles`, protocol 40: the host looks at
each one's size and modification time every second and re-sends a changed file unasked)
and draws the `HostMsg::File` answer (`slopty-host::file::read`: the first 512 KiB, then the
first 2 000 lines, `FileRead::Text | Binary | Missing`) as line-numbered mono rows in a
`uniform_list`, coloured by the grammar the path names (`slopty-ui::highlight`: syntect's
bundled grammars on the pure-Rust regex engine, reduced to nine tokens painted from the
theme's palette, parsed on a background thread after each read; the same hook colours a
fenced block in an answer through gpui-kit's `TextViewDefaults`, and the changed rows of an
edit's diff in the agent card, parsed once per entry), or one line saying why not; a read that follows one tints the lines that
differ (`file::changed_lines`, `similar` over the lines) and scrolls the first into view; a
card opened from an edit lands on the edit's line (`ToolDetail::Diff.line`, protocol 35: the
host finds `old_string` in the file, or `new_string` once the edit landed,
`transcript::locate`, tinted in the accent tone, `FileView::focus_line`). The
card reads again when any agent's
Edit/Write result arrives on the canvas, on its "reload" pill, and when "view" is pressed on
another call for the same path (one card per path: `canvas::open_file` reveals the existing
one). Titled `name · parent` (`canvas::file_title`); its dump entry is `ItemInfo.file`
(path, summary, lines). The way in is the agent card: "view" on an edit, a write or a read
(`conversation-view-<entry>`, a11y "View <path> on the canvas"), a relative path made
absolute against the agent's `cwd` (`TerminalView::view_file`); the palette lists every card
as "Go to <title>" after the sessions (`PaletteRun::Item`), the last five distinct commands
of the shell a "run" would go to as "Rerun <command>" (`TermState::recent_commands`,
`PaletteRun::Rerun`, typed through `run_text`), and the card's "ask" pill puts
`@<path>` into the agent's composer (a note card's puts the note's Markdown, a blank line
after it). ⌘F with the card active opens a find bar like the
terminal's (`FileView::find`, key context `FileSearch`): a hit is a line holding the text,
case-insensitive (`file::find_hits`), tinted in the warn tone, stepped with ⌘G/↩ and wrapped;
Esc closes it and the canvas takes the keyboard back. An "edit" pill (`edit-<item>`, shown
while a shell exists) types `${EDITOR:-vi} +line 'path'` into the last-used shell, the line
being the current find hit or the one the card opened at (`FileView::reading_line`); with
the card active, ↑/↓/⇞/⇟/Home/End move that reading line (`FileView::move_line`), a click
on a row sets it, and the "ask" pill mentions it (`@path line N`). The picker (⌘O, a field at
the top filtering by every word typed, ↑/↓/↩ choosing) lists the canvas's sessions first — agents waiting on the human, then other agents with their status
line, then plain shells — and a click reveals and focuses that terminal; below them the host's
windows and displays. Remote-window items are
created from the picker; `reconcile_screens` opens a stream for every window/display item
that lacks one and closes streams for items that disappeared, so the document, not the UI, is
the source of truth for what is being streamed. The command palette (⌘⇧P, `slopty-ui::palette`)
lists the canvas's sessions to go to ("Go to <title>", agents waiting on the human first,
their status on the right) and every action by name with its keys read from the binding
tables (`canvas::palette_items` for the canvas's and the terminal's, the app's own appended
with `extend_palette`; the View menu's "Commands…" opens it on the Mac): typing
keeps the lines every word of the text is found in, ↑/↓ choose, ↩ or a click runs one, Esc
closes; a path typed in (`src/lib.rs:7`, `~/notes.md`) is an `Open <path>` line first, which
opens a file card against the active shell's directory (`palette::path_query`); a directory
spelled from the root or home with a slash at the end (`~/proj/`) is instead a "New terminal
in …" and a "New conversation in …" line (`palette::path_items`), the host expanding `~`; and a word is
also asked of the host's files under that directory (`ClientMsg::FindFiles` →
`HostMsg::FoundFiles`, protocol 36, the `@` completion's matcher) whose hits are `Open <path>`
lines after the commands; the top bar's "⋯" button (`commands`, a11y "Commands") opens it too, the phone's way
to every action. The palette remembers where the keyboard was (`window.focused`), puts it
back when it closes and dispatches the choice on the next frame from there, so a terminal's
own actions (find, the prompts) reach the terminal that had the focus.

**Presence.** Each client tells the host where its viewport is on the canvas
(`ClientMsg::Look`, once the camera rests for 100 ms); the host keeps that in an ephemeral
table beside the document and fans it out as `CanvasSync::Presence`, newcomers hearing the
table after the snapshot and a dropped connection announced with no view. The canvas draws
every other client as an outline in its colour with its name at the corner, so two people on
one canvas can see what the other sees; the name is a button that follows that client's
viewport until the camera is moved by hand. The minimap outlines every viewport too, and a
"here" row at the top right names every other client, on screen or not, each pill following
on a click. ⌘⇧O points the others at the active card (`ClientMsg::Point`, relayed as
`CanvasSync::Pointed`): they get a toast naming the pointer and the card that goes there on a
click and leaves by itself after 8 s; the active card carries a "point" pill while
somebody else is here (`docs/decisions/multi-client.md`, 2026-09-13).

## 5. Agents

Claude Code only, for now. The bar's "+ agent" pill opens a menu (`agent-menu`, `Menu`
"Agent") of the three ways to one: "Terminal agent" (⌘⇧T) opens a terminal running
`claude` (a bare name, resolved on the host through the login shell), "Conversation" (⌘⌥T)
the driven card of §5's last paragraphs, "Resume conversation…" (⌘⌥R) the picker of past
ones — on the phone the menu is the only way to the last two. All three, like ⌘N's shell,
start in the active terminal's directory when there is one (`CanvasView::active_cwd`, the
session's OSC 7 cwd as the host last reported it), else the host's default. Four signals, in precedence order: hooks
(delivered to `slopty-hostd` over its control socket by `slopty hook`, the relay Claude Code
runs for each event) → JSONL transcript tail → terminal title/OSC → foreground-process
presence. All four are built, and `AgentEvent.source` (`AgentSource::{Process, Title,
Transcript, Hook}`) says which one a status came from, so a `claude` the human started by
hand — or one running before `slopty hook install` — gets the same pill, badges, attention and
conversation view as a hooked one.

**The three signals below the hooks** are read by `slopty-hostd`'s own tick (`agents::watch`,
every 750 ms) and merged by `slopty_agent::Tracker::observe`, which never lets a weaker signal
overwrite what a stronger one said. The tick broadcasts what it found before it reads any file
and drops anything the table has moved past (`AgentTable::is_current`), so a hook arriving
while it works is never overwritten by the older poll. `slopty_host`'s session actor answers a `Probe` with the
title, the OSC 7 cwd and the tty's foreground process; `slopty_pty::process` names that
process (`tcgetpgrp` for the foreground group, then `proc_pidinfo PROC_PIDTBSDINFO` for the
name and start time, the `KERN_PROCARGS2` sysctl for `argv` and `PROC_PIDVNODEPATHINFO` for
its cwd), and `slopty_agent::detect` decides from the name and `argv` alone whether it is
Claude Code — the native launcher, a `node`/`bun`/`deno` whose *script* is the npm `cli.js`,
or a shell running either (the kernel rewrites `argv` for a `#!` script, and `slopty_pty::pty`
itself starts an unfound `claude` as `zsh -lic claude`). Present → `Idle`; gone → the agent is
cleared; replaced by another process → attributed again from scratch, since a tracker follows
a pid and its start time, not a terminal. `slopty_agent::title` maps the OSC 0/2 title to working-vs-idle from two tables: the
spinning circle Claude Code paints while a turn runs (`◐◑◒◓`) and the sparkle it paints in
front of the conversation's summary between turns (`✳`, or the bare name before there is a
summary) — the frames in the pane itself (`·✢∗✻✽`) never reach the title and mean nothing
there. `slopty_agent::discover` finds the conversation from the session's own working
directory — Claude Code writes `~/.claude/projects/<cwd with every non-alphanumeric character
replaced by `-`>/<session uuid>.jsonl`, and the live file is the newest one modified at or
after the process started — and `transcript::progress` reads `Working` / `Tool` / `Done` out
of its newest record. That lookup is repeated every eighth tick (`AgentTable::discoveries`
carries the file being read): `/clear` and `/resume` start a new file, and the tail moves to
it and starts over rather than freezing on the old conversation's last record. The transcript
can never report `Blocked`: a permission prompt is only written once it has been answered, so
blocking stays a hook-only signal. Once a hook has spoken for a session, the tick fills gaps
only (the transcript path, so ⌘⇧L works either way) and never changes the status; it ends the
agent only when a `claude` the host actually watched in the foreground has been gone for four
probes, which is how a killed agent loses its pill without a `SessionEnd`. A session
attributed without hooks shows an
"install hooks" pill beside its agent pill, once per run: `ClientMsg::InstallHooks` asks the
host to register the relay (`slopty_agent::hooks`, the same code `slopty hook install` runs)
and `HostMsg::HooksInstalled` comes back as a notice, because the human reading the pill may
be on a phone.

The host spawns every session with `SLOPTY_SESSION=<id>` and `SLOPTY_HOSTD_SOCKET=<path>`;
the relay forwards its stdin plus those two to the daemon as `CtlRequest::Hook` and always
exits 0. `slopty-agent` keeps one `Tracker` per session that turns the hook stream into
`AgentStatus` (`Idle`, `Working`, `Tool`, `Blocked{Permission|Question|Elicitation|IdlePrompt}`,
`Done`) and flags `attention` on the transitions worth a sound. `AgentEvent.detail` says what
the agent wants: the tool call awaiting permission, the question it asked, the elicitation's
message, or on `Done` the last line it said (`last_assistant_message` from the `Stop` payload;
when a payload has none of these but names a transcript, the daemon reads the JSONL tail —
`slopty_agent::transcript` — off the blocking pool and fills the detail in). The daemon
broadcasts each change as `HostMsg::Agent` and replays the table to joining clients. The canvas shows the status
as a pill in the terminal's title bar and outlines the item when the agent needs the human.
A permission badge carries "allow" / "deny" buttons that type Enter / Esc into that session
through the terminal view's normal key path (`TermRequest::Key`, nothing new on the wire); a
question or elicitation badge carries "answer", which reveals and focuses the terminal. The
pill shows "allowed" / "denied" until the host reports the agent's next state, so one tap sends
one key. Finding them: ⌘⇧A (the "Next Agent Needing You" menu item) reveals and focuses the
next waiting terminal in reading order, cycling from the active item; the top bar shows an
"N need you" pill with the count that does the same on a click or a tap (the phone's way in).
`slopty hook install|uninstall|status` manage the registration in `~/.claude/settings.json`.
On macOS the count is also the Dock badge, an attention event bounces the Dock icon when the
app is not active, and (bundled app only) a notification-centre banner with Allow/Deny buttons
is posted; clicking it activates the app and reveals the session
(`CanvasView::notification_response`).

**Driven agents (structured control).** "New Agent (Structured)" / ⌘⌥T (`NewDrivenAgent`)
sends `ClientMsg::OpenAgent { cwd, resume, worktree, model, title }` (⌘⌥⇧T sets `worktree`:
Claude Code makes a git worktree under `.claude/worktrees` and runs there; the card's header
shows its name) and the host runs Claude Code
itself, with no PTY, over its own stream-json host protocol (DECISIONS, "Structured driving
is Claude Code's own stream-json protocol"): `apps/slopty-hostd/src/driven.rs` spawns `claude
-p --verbose --input-format stream-json --output-format stream-json --permission-prompts host
--permission-prompt-tool stdio --include-partial-messages --replay-user-messages [--resume id]
[--model m]` through the login shell (or the program named by `SLOPTY_CLAUDE_BIN`, which is
how the self-test substitutes a fake), in the requested directory, with stdio piped and
`CLAUDECODE` unset. The session is a `SessionKind::Agent` `SessionSummary` (protocol 15) in
the same `HelloAck` list and the same canvas item kind as a terminal, so placement, many
clients, sleeping, closing and reattaching are the machinery terminals already have; only the
per-session stream differs: `TermRequest::Attach` answers with the conversation snapshot
instead of a grid, `Close` ends the process. `slopty_agent::stream` is the pure protocol
layer — `parse` turns each NDJSON line into an `Event` (`Init`, transcript-shaped `Record`s,
`TextDelta`, `MessageStop`, `Permission`, `ControlAck`, `Result`, `Denied`), `Fold::apply`
turns events into `TranscriptEntry`s (through the same `transcript::record_entries` the JSONL
tail uses), the streamed `Partial` text, `AgentStatus` (`Working` while a turn runs,
`Blocked(Permission)` on a `can_use_tool`, `Done` on the `result`) and `PermissionRequest`s
(the request id, the tool, its one-line summary and its `ToolDetail`), and
`user_message` / `Fold::answer` / `interrupt` build the lines the host writes back. The
daemon's pump task per agent folds stdout, keeps the last 400 entries, the partial and the
pending requests per session (`driven::Snapshot`, what a joining client gets), and
broadcasts `HostMsg::Transcript` (increments), `HostMsg::AgentPartial` (coalesced to one
every 40 ms), `HostMsg::AgentPermission` and `HostMsg::Agent` with `source: Driven`, the
strongest source. Clients speak `ClientMsg::AgentSay { session, text }` (shown at once as
the user entry; the agent's replay of it is deduplicated), `AgentAnswer { session, request,
allowed, message }` and `AgentInterrupt { session }`. When the process ends, the session
closes like any other (`SessionClosed { Exited }`, the item removed).
The **agent card** is the same `TerminalView` in *driven* mode (`set_driven`, from
`SessionSummary.kind`): the conversation opens on its first frame with the caret in the
composer and cannot be hidden, there is no grid behind it, ↩ in the composer sends
`AgentSay` (mid-turn the prompt is also shown as queued, `Conversation::queued`, until its
entry arrives or the turn ends), ↑ / ↓ on an empty composer recall the prompts sent (`Conversation::recall`; a
click on a sent prompt's bubble does the same, `Conversation::reuse`), Esc
and ⌃C send `AgentInterrupt`, the streamed text shows under the list
(`conversation-partial`) until it becomes an entry, ⌘F opens the terminal's find bar as a
row under the header and searches the entries in the client (`conversation::entry_hits`,
plain or regex; hits are washed entries, the newest first on show, ⌘G / ↩ / ⇧↩ step, the
reveal pauses tail-following), and the attention row names the tool
*and what it would do* (`Attention::Permission.detail` from the `PermissionRequest`), with
Allow / Deny raising `TerminalViewEvent::AgentAnswered { request, allowed }` so the canvas
sends `AgentAnswer` by request id (`answer_driven`) — the title-bar badge and the
notification-centre buttons go the same way for a driven session. An `AskUserQuestion` is a
`can_use_tool` on the wire too (protocol 18, probed on 2.1.269: `requires_user_interaction`),
but the fold reports it as `Blocked(Question)` and the card answers it in place
(`Attention::Question`, `conversation::question_block`): each question with its header chip
and its options as buttons (`question-option-<q>-<o>`, labelled by their label, the picked
ones in the accent tone, the description under the label); one tap on a single-select
question is the whole answer, a multi-select question toggles and an Answer button
(`question-answer`) sends when every question has a pick, and text typed into the composer
is the "Other" answer to the first question without one, not a prompt (`choose`,
`answer_question`, `answer_question_with`). The answer travels as
`AgentAnswer { allowed: true, answers: [QuestionAnswer { question, answer }] }` and the
host files it into the request's input as `updatedInput.answers = { question: answer }`
(labels of a multi-select joined with ", "), which Claude Code turns into the tool's
"Your questions have been answered" result. The badge and the notification treat it as the
question it is (an "answer" pill that reveals the card, no Allow / Deny). A permission row
has a third button when the agent offered a way not to ask again (protocol 19): the
`can_use_tool`'s `permission_suggestions` (a `Write` suggests `setMode acceptEdits` for the
session; a command suggests `addRules`) are kept on the host with the input, worded for the
human as `PermissionRequest::always` (`stream::always_label`: "accept edits for this
session", "always allow Bash(cargo test:*) in this project") and drawn as Always
(`conversation-always`, labelled "Always: …") between Allow and Deny; a tap sends
`AgentAnswer { allowed: true, always: true }` and the host answers with the suggestions
echoed as `updatedPermissions`, after which Claude Code stops asking for that case and, for
a mode change, sends the `system/status` record that moves the mode chip. `AgentAnswer` is
one struct (`session, request, allowed, message, answers, always`) for both rows.
A subagent (`Agent` / `Task`) is followed, not shown (protocol 20): over stream-json its
own records carry `parent_tool_use_id`, and `transcript::is_subagent` skips them the way
it skips a sidechain in the file (no entries, no status, no model), while Claude Code's
`system/task_started` and `task_progress` records (`tool_use_id`, `description`,
`subagent_type`, `usage.tool_uses`, `usage.duration_ms`, `last_tool_name`) fold into one
`AgentTask` per spawning call, marked `done` when that call's `tool_result` arrives, and
travel as `HostMsg::AgentTask` (kept per session for a joining client). Every `ToolUse`
entry now carries its `call` id, and the card draws the task under the call that spawned it
(`conversation::task_line`: "Explore running · 1 tool use · 3 s · Bash", muted once done);
the label reads the same. The header also shows the subscription's windows (protocol 21):
Claude Code's `rate_limit_event` (`rate_limit_info.unifiedWindows.{five_hour,seven_day}`,
`utilization` as a fraction, `resetsAt` in seconds; `status: "rejected"` when refused) folds
into `AgentInfo.usage: Option<Usage { limited, five_hour, seven_day }>` with each window as
whole percent and reset time, drawn as a Status chip (`conversation-usage`, "5h 23% · 7d
74%", or "limited until HH:MM"); API-key runs never send one and show nothing. Next to it
the context chip (`conversation-context`, protocol 24): every assistant record's
`message.usage` (input + cache writes + cache reads = the context the request carried) and
the turn result's widest `modelUsage.*.contextWindow` fold into `AgentInfo.context:
Option<Context { tokens, window }>`, drawn as "ctx 16%" (or "ctx 31k" before a result names
the window), in the warn tone from 80%. A compaction (`system/compact_boundary`, protocol 25)
is a `TranscriptBody::Compacted` entry drawn as a ruled divider ("compacted (auto): 167k →
12k"); the summary record flagged `isCompactSummary` is not an entry, and the boundary's
`post_tokens` lowers the chip at once. Lines from the loop rather than the model
(`system/informational` past the `info` level, `model_fallback`, `permission_retry`;
protocol 26) are `TranscriptBody::Notice { level, text }` entries drawn as one plain line in
the muted, accent or warn tone; `system/commands_changed` replaces the slash list (each a
`SlashCommand { name, description, hint }`, protocol 29; `init` names them alone) and
`system/task_notification` ends a backgrounded task. An agent that exits unasked with a
non-zero status closes its session with `CloseReason::Failed { status, detail }` (its last
stderr line; protocol 27), which the app shows as a top-bar notice. A
`system/status` whose `status` says `requesting` or `compacting` becomes the Working detail
("waiting for the model…", "compacting the conversation…") so the chip moves while there is
nothing yet to draw; it never displaces a permission or a question. So does a streamed
block's opening (`content_block_start` → `stream::Block`): "thinking…" for a thinking block,
"calling Write…" for a tool call being composed, cleared when the text block starts and the
partial takes over. The badge shows the Working detail when there is one. **Pictures** go with a
prompt (protocol 23): ⌘V in the composer with a picture on the clipboard (`composer_paste`
captures the input's `Paste` before it reads text) attaches it — PNG, JPEG, GIF or WebP, the
types the model reads, at most `IMAGES_MAX` of `IMAGE_BYTES_MAX` each — as a chip above the
composer (`composer-attachment-<i>`, "PNG · 70 B", a tap drops it); a picture file dropped
from the desktop onto a driven card (`drop_paths`, GPUI's `ExternalPaths`) is read off the UI
thread and attached the same way, any other file refused by name, and a shell card takes no
files since the path is the client's. Before it lands the
picture is made fit off the UI thread (`terminal::attachment::fit`, a "preparing…" chip
meanwhile): one over 1568 px on the long side or over the cap is decoded, shrunk and
re-encoded — PNG when it has transparency, JPEG otherwise — so a 6 MB phone screenshot goes
as a few hundred KB the model reads at full detail anyway; one already inside both bounds
passes through byte for byte. A refused picture (a TIFF, undecodable bytes, a fifth) says
why in the top bar (`TerminalViewEvent::Notice` → `CanvasEvent::Notice` →
`Workspace::show_notice`). ↩ sends
`AgentSay { text, images }` and the host writes the stream-json user message as blocks, each
picture base64 with its media type before the text (`stream::user_message`), refusing a
prompt over the caps whole. The transcript reader counts image blocks into
`TranscriptBody::User.images`, so the bubble says "1 picture" whether the prompt came from
this client, another, or the file; the bytes never come back over the wire. A host window's
picture never goes over it at all (protocol 33): the "ask" pill on a window or display card
(or the palette's "Ask the agent about this window") puts a `🖥 <title> ×` chip in the agent's
composer (`composer-snapshot-<i>`), ↩ sends the target in `AgentSay::snapshots`, and hostd
takes the picture as it sends — `Target::snapshot` through `SCScreenshotManager`, then
`snapshot::encode` to a PNG at the model's size — and adds it to the prompt's images. On the
phone the key bar's "paste" is the way in: `paste_clipboard` on a driven card attaches the clipboard's
pictures and puts its text into the composer (no session to paste into), and the fork's
`gpui_ios` reads a picture off `UIPasteboard` under its uniform type (PNG first, as a
screenshot is) and writes one the same way (fork `f629234166`). The e2e socket's `attach`
stands in for the clipboard read on the Mac (a test must not touch the shared pasteboard);
on the simulator, whose pasteboard is its own, the socket's `clipboard` puts a picture there
through GPUI and the test taps "paste"; the headless test pastes through the test
platform's. Both kinds coexist: ⌘⇧T
still opens a PTY `claude` with the TUI. Tests: `slopty_agent::stream` on the probe fixtures
(`tests/fixtures/stream_one_turn.jsonl`), the wire shapes in the proto goldens
(`driven_agent`), headless `a_driven_view_speaks_to_the_agent` and `a_driven_view_answers_a_question_in_place`, and the app self-test
`a_driven_agent_talks_over_stream_json`, which runs the host against `slopty-fake-claude`
(`crates/slopty-e2e/src/bin`, a scripted stream-json agent: it streams, lingers for an
interrupt, asks a `Write` permission that the test allows and then denies through the
Allow / Deny buttons in the accessibility tree, edits with a todo list, asks a question
the test answers by tapping "Blue", suggests accept-edits with its `Write` permission,
which the test takes with Always and then writes again unasked, and spawns a subagent whose
own Bash stays off the card while its progress shows under the call; goldens
`conversation-tools`, `conversation-question`) and reads the card through the dump
(`TerminalInfo.kind`, `ConversationInfo.partial`, `ConversationInfo.permission`).

**What the agent says about itself, and retuning it (protocol 16).** The pump folds
`system/init` (`Update::Init`), every `result` (`Update::Turn`), the model each assistant
record names (`Update::Model`) and the `system/status` records that carry a permission
mode (`Update::PermissionMode`) into one `AgentInfo { agent_session, model,
permission_mode, slash_commands, turns, cost_micro_usd }` per session, broadcast whole as
`HostMsg::AgentInfo` whenever any of it changes and sent with the snapshot a joining client
gets. The card shows it as a header over the list (`conversation-header`): the model as a
chip (`conversation-model`, "Model: …") whose click opens a row of the four aliases
(`conversation-models`, `MODELS`: fable, opus, sonnet, haiku) — picking one sends
`ClientMsg::AgentSet { session, model }`; the permission mode as a chip
(`conversation-mode`, "Permission mode: …") that cycles `default → acceptEdits → plan`
through `AgentSet { permission_mode }`; then the turn count and the cost. While the agent
works the composer's Send (`composer-send`) is a Stop (`composer-stop`, one tap =
`AgentInterrupt`, the same as Esc / ⌃C), the finger's way to end a turn. The host turns an
`AgentSet` into Claude Code's `set_model` / `set_permission_mode` control requests (probed on
2.1.269: both acknowledged in place, no restart; `supported_models` / `supported_commands` are
not, so the model list is fixed and the slash commands come from `init`), records the model
on the ack and the mode on the status record that follows, and the chip only changes when
the agent has confirmed. The composer completes the announced slash commands
(`conversation::slash_matches`): a one-word `/…` text lists its prefix matches above the
field (`conversation-completions`, `ListBox` of `ListBoxOption`s, at most `SLASH_MAX` = 8,
one per line: the name in the mono face, the argument hint and the description muted after
it, the a11y label `completion_label`: `/compact [instructions] — …`), Tab takes the selected
one (`CompleteSlash`, bound to Tab in the "Terminal" context ahead of gpui-kit `Root`'s
focus-ring Tab, and propagated when there is nothing to complete), ↑/↓ choose, Esc hides the
list until the text changes. An `@` word the text ends in (`conversation::file_query`) is
asked of the host once per query (`ClientMsg::ListFiles { session, query }` →
`HostMsg::Files { session, query, paths }`, protocol 30: `slopty_agent::files::matching`
walks the agent's working directory with the `ignore` crate — hidden and `.gitignore`d
entries skipped, depth 8, 20 000 entries at most — and answers at most 8 paths, a name
starting with the query first, a directory ending in `/`); the answer whose query is still
the word lists as `@path` completions in the same box and Tab replaces the word alone (a
directory gets no space after it, so typing on descends and the host is asked again).
Those keys are caught in the capture phase (`completion_key`,
and `capture_action` for the input's own `MoveUp` / `MoveDown` / `Escape` / `IndentInline`,
which GPUI dispatches before any key event) so the composer never moves its caret or
interrupts the agent while the list is up. **Resuming** a conversation:
"Resume Agent…" / ⌘⌥R (`ResumeAgent`) sends `ClientMsg::ListAgentSessions { cwd }` for the
active terminal's directory; the host lists `~/.claude/projects/<escaped cwd>/*.jsonl`
(`slopty_agent::discover::sessions`, the directory named by the canonical cwd since Claude
Code names it by `process.cwd()`: newest first, at most 30, each named by the first prompt
the human typed — meta records, sidechains, subagent `agent-*.jsonl` files and
`<command-…>` records are not prompts, and a file with none is not listed) as
`HostMsg::AgentSessions { cwd, sessions }`, which opens the `WindowPicker` in its resume
form (`Dialog` "Resume a conversation", rows `picker-agent-<i>` labelled "title, N min ago ·
cwd"). Without an active terminal (the phone), or from the "Every directory" row
(`picker-everywhere-0`) a directory's list ends with, `cwd` is `None` and the host lists
every project under `~/.claude/projects` (`discover::all_sessions`, protocol 22: all
candidates sorted by mtime first, only the newest opened to be named, so the cost is 30
reads whatever the home holds); that answer carries `cwd: None` and offers nothing wider.
A row sends `OpenAgent { cwd, resume: id, title }`, so the new card is titled by that first prompt
and Claude Code continues the same session (`--resume`); before the agent says anything the
pump reads the transcript's last entries (`discover::conversation`, off the runtime) and
appends them, so the card opens with its past instead of empty. Tests: the fold and the discovery
in `slopty-agent`, the wire shapes in `driven_agent_info`, headless
`a_driven_view_shows_the_agent_and_retunes_it` and `a_past_conversation_is_resumed_from_the_picker`,
the app self-test (the fake acks `set_model`, answers `set_permission_mode` with a status
record, answers `/cost`, honours `--resume`, and writes its prompts and replies to the
transcript under the run's private `HOME` so ⌘⌥R finds them and the resumed card shows them), and `the_driven_agent_card_on_the_simulator`
with the `ios-<device>-agent` goldens.

**Conversation view.** A terminal that runs a Claude Code session can show the conversation
instead of the grid: the "chat" pill in its title bar or ⌘⇧L (`ToggleConversation`) sends
`ClientMsg::Transcript(TranscriptFollow { session, follow: true })`; the daemon opens a
`slopty_agent::transcript::Tail` on the session's `transcript_path` (from the hook payloads),
sends the last 200 entries as a `HostMsg::Transcript` with `reset: true`, then polls the file
every 400 ms off the blocking pool and sends only the appended entries (a truncated or
replaced file resets again). An entry is `TranscriptEntry { at, body }`: the record's
`timestamp` in Unix milliseconds and a `TranscriptBody::{User, Assistant, Thinking, ToolUse,
ToolResult}` built from the JSONL records — `user` text, `assistant` text blocks as markdown,
`thinking` blocks, `tool_use` blocks with a one-line summary (the command / file / pattern /
query the tool was given) and a `ToolDetail` (protocol 17: what the call would do, read on
the host from the input by tool — `Edit` as a `Diff` of `old_string` against `new_string`
line by line through `similar`, `Write` with its content, `Bash` as a `Command`, `Read` with
its slice, `Grep`/`Glob` as a `Search`, `TodoWrite` as `Todos`, `Agent`/`Task` with the
brief, `AskUserQuestion` as `Question`s with their `Choice`s, and any other tool or a known
one with a strange input as its `Json`), `tool_result`
blocks with `is_error` and named after their `tool_use_id` (the `Tail` remembers the last
512 calls). Thinking, tool input, a diff and tool output are `Clipped` on the host: the
first 40 whole lines or 4 000 characters, whichever comes first, and the count of lines
dropped, so the wire never carries a whole file. Sidechain rows and the app's injected texts are skipped. The client
keeps them in `slopty_ui::terminal::conversation::Conversation`: a bottom-aligned gpui `list`
in `FollowMode::Tail` (pinned to the newest entry until the reader scrolls up, then a
"↓ latest" pill re-pins; a reset re-pins) with `TextView::markdown` for assistant turns —
split at their ``` fences first (`conversation::segments`), each fenced block drawn as its own
element with the language and a "copy" button (`conversation-code-copy-<entry>-<segment>`,
a11y "Copy code") over the code, beside a "run" button
(`conversation-code-run-<entry>-<segment>`, a11y "Run in shell") drawn only while the canvas
has a plain shell to run it in, which reveals that shell — the one most recently activated,
else the newest — and types the block into it as a paste and one ↩ —
"HH:MM" local-time stamps on prompts and answers (the answer that closes a turn adds "took
42 s" from the stamps at both ends, `conversation::turn_took`), and folds that open on a click — thinking
starts folded, a result shows its first 4 lines, and a tool call draws its `ToolDetail`
(`conversation::tool_body`): an edit's diff shows unasked as rows tinted in the success tone
(added) and the error tone (removed) with a sign column and "+a −r" in the header
(`tool_badge`), folded past 12 lines until a click; a todo list shows whole with what is
done struck through and the item in progress in the accent tone; a command, a written file,
a subagent's brief and a stranger's JSON open on a click; a read or a search opens to its
slice or filter. A call that named a file (an edit, a write, a read; `conversation::tool_path`)
carries an "open" button in its header (`conversation-open-<entry>`, a11y "Open <path> in the
editor") under the same rule as the answer's "run" button: drawn only while the canvas has a
plain shell, and a click types `url::editor_command` for the path into that shell the way
⌘-click on a path in a terminal does, without folding the call. Every line of a result that
names a file (`url::first_path`: a grep hit `src/a.rs:12:…`, a compiler's ` --> src/b.rs:3:5`)
is a button (`result-path-<entry>-<line>`, a11y "View <path>:<line> on the canvas") that opens
the file card at that line (`TerminalView::view_file`). A screen reader hears the
counts ("Tool Edit: src/a.rs, 2 added, 1 removed", "Tool TodoWrite: 1 of 3 done"); headless
`an_edit_shows_its_diff_and_a_todo_list_its_checklist` and `a_tool_calls_path_opens_in_the_canvas_shell`,
the app self-test's `edit` turn against the fake (golden `conversation-tools`). Under the list sits the
**composer**, a gpui-kit `TextareaState` growing from one to six rows: ↩ sends the text into
the session as `TermRequest::Paste` followed by an Enter key (an empty ↩ sends the bare
Enter, so a prompt can be accepted; slash commands and `@file` go through as typed), ⇧↩
breaks a line. While the composer has the keyboard, `TerminalView::key_down` lets only Esc and
Control keys (⌃C above all) through to the session; ⌘ shortcuts and ⌘⇧L keep their meaning
through the action path. Above the composer the **attention row** shows the agent's
`AgentStatus` when it waits: `Blocked(Permission)` draws Allow / Deny, which raise
`TerminalViewEvent::Answered` so the canvas types the same Enter / Esc the title-bar badge
does (`allow_agent` / `deny_agent`), then reads "allowed" / "denied" until the host's next
report; `Blocked(Question | Elicitation)` moves the caret into the composer. The canvas hands
every `AgentEvent` to its view (`set_agent_status`). ⌘⇧L again drops the view, sends
`follow: false` (which closes the tail on the host) and gives the grid the keyboard back. On
iOS the composer sits inside the item above the key bar and the workspace's safe-area /
keyboard insets, and focusing it raises the soft keyboard through the same input handler as
the grid. Tests: JSONL → entry mapping and clipping in `slopty-agent` (fixtures, never a real
transcript); headless `the_conversation_replaces_the_grid_and_follows_the_transcript`,
`the_composer_types_into_the_session`, `the_attention_row_answers_a_permission`,
`the_list_stays_pinned_until_the_reader_scrolls_up`, `folds_open_on_a_click`; the app
self-test `the_conversation_view_reads_and_answers_the_agent` (a fixture transcript named by
`Stop` and `PermissionRequest` hooks the test hands to hostd over its control socket, never
typed into the shell; goldens `conversation.png` and `conversation-permission.png`) and the
simulator's `the_conversation_view_on_the_simulator` (dump plus the goldens
`ios-phone-conversation.png` / `ios-pad-conversation.png`, rendered by the fork's iOS
`render_to_image`); the dump lists the entries, the composer, its focus, the pin and
the attention row (`ConversationInfo`) and the agent's state and signal
(`TerminalInfo.agent`, `TerminalInfo.agent_source`).

**Attribution tests.** Unit: `slopty_agent::detect` (the executable name, `argv[0]`, runtimes
and shells), `slopty_agent::title` (every frame of both tables, and the in-pane spinner
reading as nothing), `slopty_agent::discover` (the escaped project directory, the newest
recent `.jsonl` in a temp home, and the file moving after a `/clear`),
`transcript::progress` (which record means what), and the precedence merge itself
(`a_hand_started_claude_is_attributed_from_the_process_and_the_title`,
`hooks_outrank_everything_and_decide_when_the_agent_ends`,
`a_transcript_read_for_the_first_time_is_not_an_alert`); `slopty_pty::process` reads its own
process out of the process table and the foreground process of a PTY it spawned. Headless:
`an_agent_seen_without_hooks_gets_the_pill_and_offers_the_hooks`. App self-test:
`an_agent_started_without_hooks_is_attributed_from_what_the_host_can_see`, which puts a fake
`claude` first on ptyd's `PATH` with a `HOME` of its own
(`Stack::launch_with_fake_claude`), opens it with ⌘⇧T and walks it from stage to stage with
marker files — the agent is played by the program in the session and by the harness's
environment, never by typing into the shell under test.

## 6. UI

GPUI (fork: `aislopware/zed` branch `slopty`, our iOS commits rebased onto upstream main; the
base commit and date live in `xtask/upstream.toml`) plus gpui-kit (fork: `aislopware/gpui-kit`,
upstream main plus one commit re-pointing deps at the zed fork). `cargo xtask upstream check`
shows the drift, `cargo xtask upstream sync` rebases, pushes and moves the `Cargo.lock` pins. The iOS backend is zed PR
#63068's `gpui_ios` on top of the pin, extended in the fork for the surface element (zero-copy
video), `Window::insets()` (safe area, keyboard), a native pinch recognizer, hardware keyboards and
pointers, a VoiceOver bridge (`gpui_ios/src/ios/a11y.rs`: GPUI's accesskit tree mirrored as
`UIAccessibilityElement`s on the Metal view) and `Window::a11y_tree()` / `set_a11y_active()` under
`test-support` so tests read the tree on every platform; one finger taps and pans through gpui core's touch recognizer, two fingers
pinch-zoom, both landing in the same canvas handlers the Mac uses. A keyboard attached to an
iPad or iPhone arrives as `pressesBegan`/`pressesEnded` on the metal view (the first responder
whenever no text input is): every key that is not plain text (arrows, escape, function keys,
enter, tab, backspace, ⌃/⌥/⌘ chords) becomes the same `Keystroke` the Mac backend would build,
so the Mac keymaps and the terminal's key encoder work unchanged; plain characters keep going
through the text system (`insertText:`, IME) while a text input has focus. UIKit does not
repeat presses, so the window repeats a held key itself (400 ms, then every 50 ms, delivered
as `is_held`). A trackpad or mouse hovers as `MouseMove` and scrolls as `ScrollWheel` with
phases (a `UIPanGestureRecognizer` limited to indirect scrolls, so direct drags stay with the
touch recognizer); while a keyboard is attached (`GCKeyboard.coalescedKeyboard`, polled once a
second with the settings) the key bar hides, since every key on it is under the fingers; `UIApplicationSupportsIndirectInputEvents` is set so clicks are pointer
events rather than synthesised touches. The bundle targets iPhone and iPad
(`TARGETED_DEVICE_FAMILY 1,2`, every iPad orientation, so Split View and Stage Manager can
resize the window; the canvas re-fits on resize like any window);
`cargo xtask ios sim --sim ipad` boots an iPad Pro 13-inch simulator beside the iPhone one, and
`cargo xtask e2e ios --sim ipad` drives the app there over its test socket, including `render`:
the fork's `gpui_ios` draws the scene offscreen through the shared Metal renderer at the
layer's drawable size, and the tests diff it against per-device goldens (`ios-phone-*.png`,
`ios-pad-*.png` under `crates/slopty-e2e/golden`). The same socket also reaches the UIKit
boundary: `UiKeyPress`, `UiTouch`, `UiPinch`, `UiInsertText` and `UiDeleteBackward` describe a
`UIPress`, a `touches…:withEvent:` set, a pinch report and the text system's calls by the
values the metal view reads out of them, and the fork's `gpui_ios::inject` (`test-support`
only) runs the view's own delivery from that description, so `tests/ios_uikit.rs` proves the
phone's input path (modifiers, key repeat, the text-system hand-off, gpui core's touch
recognizer, the pinch) and not just GPUI's dispatch; the pure mappings (HID usage → key,
`UITouchPhase` → phase, the US layout stand-in) are unit-tested on the host in the fork.
**Design system.** Every chrome surface draws from `slopty-theme` and nothing else (ruling
"Design tokens" in DECISIONS): a four-step neutral ladder (`canvas`, `panel`, `raised`,
`overlay`), one hairline (`border`), three text levels, one accent with its foreground, and
three status tones (`success`, `warn`, `error`) that the chrome uses only where state carries
meaning (host dot, agent badge and outline, "N need you", failed result, failed-command
separator). Geometry comes from `radii` (xs 4 / sm 6 / md 8), the 4/8 pt `spacing` scale
(2 / 4 / 8 / 12 / 16 / 24) and a type scale hanging off `ui_size` (`caption` −3, `small` −1,
`title` +2), so the settings' font size moves every label together; alphas for tints, hover
washes, the scrim and the separators are the `alpha` constants. Hairlines carry the elevation,
shadows are `shadow_sm` on floating layers only (picker, host switcher, search bar, "↓ latest").
Focus is one accent hairline: the active item's frame, the search bar and the composer while
they hold the caret. Pills (title bar, badge, "N need you") are `small()` text on a `TINT`
fill of their tone with the tone as text; buttons are `radii.sm` with `raised` → `overlay`
hover/pressed, and the one primary action on a surface is an accent fill with `accent_fg`.
gpui-kit's widgets (inputs, the composer, Markdown `TextView`) read gpui-kit's own theme, which
`slopty_ui::kit::sync` rewrites from the same tokens on every theme change, and the Markdown in
the conversation gets `markdown_line_height`, paragraph gaps of one base unit, headings
stepping down from `title()` and code in the terminal mono at `small()` on `raised`. Headless
tests read the tokens back through `painted_quads()`: the accent vs hairline item frame, the
`warn` outline of a blocked agent in both variants, the `error` separator tone, the composer's
focus ring and the warn-tinted attention row, and gpui-kit's colours after a sync.
`slopty-ui::screen::ScreenView` paints a remote window as a `gpui::surface` from the decoder's
`CVPixelBuffer` (zero copy), draws the host cursor from the cursor channel, forwards mouse, scroll
and keys (including ⌘ chords the canvas does not bind) as `ScreenInput`, and asks the host for
a stream scale matching its painted width. When the host window changes size, hostd notices
within 250 ms, restarts the stream at the new size and sends `Geometry`; the canvas re-aspects
the item. It is also a text input (`EntityInputHandler`): on iOS a tap raises the soft
keyboard and committed text goes to the host one key per character through
`ScreenView::press` (press + release, armed ⌃/⌘ from the phone key bar applied); the bar over
a window is esc, tab, ⌃, ⌘, arrows, `/`, copy, paste (⌘C/⌘V on the host, so the host's
pasteboard flows back through clipboard sync). ⌘⇧I (Canvas ▸ Stream Stats) overlays every
window with its stream size and scale, fps, Mb/s, link RTT and the FEC/lost/NACK/refresh and
audio counters, re-sampled once a second from `ScreenStats`.

**Canvas navigation.** ⌘1 fits every item, ⌘2 fits the active one, ⌘0 returns to 100 % with the
active item still in the middle, and ⌘⇧R tidies the canvas into one block per repository. Every
one of them is a `Camera` the view flies to rather than jumps to: `slopty_client::canvas::Flight`
interpolates two cameras over 180 ms with a cubic ease-out — zoom geometrically, the viewport's
centre in a straight line — and the canvas element's prepaint advances it by the time since the
last frame. The render loop is the only clock, and a landed flight stops asking for frames. The
arrangement itself is pure (`slopty_client::arrange`): items in, origins and a `Heading` per
block out, grouped by their session's **repository** and ordered by the z the canvas already
keeps. The repository is the host's answer, not a guess from the path: `slopty_host::repo`
walks up from the working directory OSC 7 reported to the nearest `.git` entry (a directory for
a checkout, a file for a worktree, whose `gitdir:` link is not followed, so a worktree is its
own repository), caches it per session and sends it as `SessionSummary.repo` and
`TermEvent::Cwd { path, repo }`. A host that sends none, or a directory in no repository at
all, falls back to the client's containment heuristic over the working directories.
Headings draw in canvas coordinates with role `Heading`, so they pan, zoom and read aloud.

**Terminal element.** `slopty-ui::terminal` draws the cached lines as one element (each
row's words shaped once at the base size and cached by content hash, their glyphs painted one
by one at the zoomed size through `Window::paint_glyph` from the fork's `ShapedLine::layout`,
so a zoom step shapes nothing; background quads, cursor, selection, underlines — the curly one
as GPUI's wave — and strikethroughs at the offsets §2's metrics derive, ⌘-hover link
underline) with a hairline over every prompt-start row but the first line: the
command-block separator, the foreground at 18 % alpha, or the theme's `surfaces.error` token
at `alpha::SEPARATOR_ERROR` (70 %) when the row's `Prompt { exit }` is non-zero
(`separator_color` in `crates/slopty-ui/src/terminal/element.rs`; `crates/slopty-theme/src/lib.rs`), and at the right end of a
prompt row whose command took a second or more, its duration (`took_label`, fg at
`alpha::TINT_STRONG`; `TerminalView::took` by prompt row from `Effect::CommandFinished`,
emptied with the epoch; the sticky block header repeats it at its right end). Terminal-context bindings: ⌘C /
⌘V copy and paste, ⌘F / ⌘G / ⌘⇧G search, ⌘↑ / ⌘↓ scroll the previous / next prompt start to
the top of the viewport (`TermState::prompt_before/after` over the cached lines, uncached
history is not fetched first; ⌘↓ past the newest prompt goes back to following output; in a
conversation card the prompts are the `User` entries, `Conversation::prompt_from_top`, and
⌘⇧C copies the newest answer, each answer's "copy" button copies that one, its "note" button
keeps it as a note card beside the agent (`conversation-note-<ix>`, the block menu's
`NoteBlock` event), and the palette's
"Copy conversation as Markdown" writes the lot as `conversation::as_markdown`),
⌘K clears the screen and the history (`TermRequest::Clear`, protocol 31: the host writes
`CSI 3 J` through its engine and output tap, as if the program had, then ⌃L to the shell so
the prompt repaints at the top; the view drops its scroll offset),
⌘⇧↩ runs the last finished command again (`TermState::last_command`, the block before the
newest prompt, typed as the block menu's rerun types it; also "Rerun last command" in the
palette), ⌘⇧C copies the last finished command's output (`TermState::last_command_output`: the
`Output` rows right above the newest prompt start, blank tail trimmed; nothing without shell
integration). A right click on a block row opens the block menu (`block-menu`, protocol 28:
the marks carry the input column, `TermState::command_block` reads the command and the
output from any row): "Copy command", "Copy output", "Rerun" (a paste of the command, then
↩ as a key), "Ask the agent" (the block as a fence, `block_markdown`, into the composer of the
agent card the human is on, else the topmost, else one the canvas opens in the active
shell's directory — `TerminalViewEvent::AskAgent` → `CanvasView::ask_agent`, the text
waiting in `TerminalView::compose` until the card has its conversation), "Save as note" (the
block as a note card beside the shell: the command as a heading and a runnable `sh` fence,
the output as a plain fence, `block_note`; `TerminalViewEvent::NoteBlock` →
`CanvasView::note_beside`, a free slot when the space beside is taken; the palette's "Keep
last block as a card" does the same for the block before the newest prompt,
`TermState::last_block`) and "Select block". While the viewport's top row is inside a block whose prompt
rows have all scrolled above, a one-row header over the grid (`block-header`, role Button,
`TerminalView::block_header`, reading `TermState::block_head` — the prompt's rows alone, never
the output) names the command in the mono face on the panel colour, ruled
under with the block's separator colour; a click on it puts the prompt back at the top.
`TermState` also follows the blocks frame by frame (`track_command`, the prompt's rows
alone): a command is running once the cursor has left the rows it was typed on
(`Effect::CommandStarted`) and finished when a newer prompt starts, whose `exit` is its
status (`Effect::CommandFinished`); the view times the two and emits
`TerminalViewEvent::CommandFinished { command, exit, elapsed }`, and the canvas badges the
item's title bar ("done 12.3 s", "failed (1) 1 min 4 s" in the success or warn tone,
`finished-<uuid>`, role Button) when the command ran at least `SLOW_COMMAND` (5 s) and its
item was not the active one — the shell's answer to the agent attention badge. The badge
goes when the item is activated (a press on it does that).
Headless `#[gpui::test]`s in `terminal/view.rs` read the
separators back from `painted_quads()` and drive the bindings with `simulate_keystrokes`. ⌘⇧L swaps the element
for the conversation view (§5), whose composer takes the typing.

**Render path and frame time.** One GPUI frame draws the workspace: the top bar, then the
canvas (`CanvasView::render`), which lays out only the items whose screen rectangle meets the
viewport (`draws`; the active and any dragged item always), each as a card below `CARD_ZOOM`
or as its view above it, then the minimap from every item's rectangle, the overlays and the
`frames::probe()` element last. The terminal element's prepaint resolves the monospace family
once per app (the `ShapeCache` global memoises the theme's list; listing the installed fonts
is a synchronous trip to the font server), reads the view's rows in place — only the rows
inside the window's content mask, so a grid hanging off the viewport builds nothing for the
rest — splits each into words (plain spaces and digits are the boundaries) and looks every word up in
the `ShapeCache` global — an `Rc<Word>` (the `ShapedLine` at the base size plus its per-byte
colours) per (text, styles, family, palette, focus), never per zoom, swept once per frame — so
a frame of streaming output shapes only the words it has never seen and a zoom step shapes
nothing; paint puts every glyph of a word at column × cell width plus its shaped position
scaled by the zoom, on the derived baseline, through `Window::paint_glyph` at the zoomed font
size (the fork's `ShapedLine::layout` hands out the runs), over the row's background quads and
under the cursor, the link underline and the prediction overlay. While the canvas zoom is **in
motion** (it differs from the zoom drawn last frame, until an 80 ms settle timer fires) the
canvas tells every terminal view and chrome label so, and they paint from the nearest rung of
an eight-per-octave raster ladder stretched to the painted size (`fonts::raster_rung`, the
fork's `Window::paint_glyph_scaled`) instead of rasterising every glyph at each intermediate
size; the settled frame paints exact. The item chrome's text — title, pills, badge, block
headings — is `slopty_ui::chrome_text::ChromeText`: shaped once at its base size (a per-app
cache), sized by arithmetic rather than a taffy measure callback, painted glyph by glyph at
`base × k` with GPUI's baseline and advance arithmetic, ellipsis included. Host events reach the canvas from the
link loop in one update per frame (the first after a quiet spell at once), so twenty streaming
sessions cost one notify a frame; a session itself never sends more than 125 frames a second
(`MIN_FRAME_INTERVAL`). `slopty_ui::frames` times every draw (`begin` in `Workspace::render`,
`end` in the probe) into a 1024-frame ring with nearest-rank percentiles and a count of the
frame slots long draws swallowed; it is the fourth line of the ⌘⇧I overlay and the `frames`
block of the self-test `dump`, and `terminal::latency` stamps each keystroke so `dump` can say
how long the local echo and the host's echo took to reach a paint. `cargo xtask e2e smooth`
runs the load scenarios (MEASUREMENTS, "canvas frame time").

**Settings.** `<data dir>/settings.toml` (`slopty settings path|init`; the "Settings…" menu
item, ⌘,, opens it in the default editor, writing the commented defaults first when it is
missing). `slopty-settings` owns the schema: `[font] mono_family | mono_size | ui_size`,
`[theme] appearance = dark | light | system`; every key has a default, unknown keys warn, a
file that does not parse is skipped with the error in the top bar for a few seconds. The app
polls the file's stamp once a second and on a change rebuilds the `Theme` (variant from
`appearance`, `system` following `window.appearance()` through `observe_window_appearance`)
and pushes it down `CanvasView::set_theme` → every terminal, window and the picker; the
terminal element re-measures its cell grid from the new size on the next frame and `fitted`
resizes the session. iOS reads the same path (in its sandbox) but has no editor entry.

**Hosts.** The app holds every paired host at once: one `HostLink` (own iroh endpoint,
own reconnect loop, own silence check) and one `CanvasView` per host (`slopty_app::hosts`),
because each host owns its canvas document. A dropped link drops the canvas and the next
connection makes a new one, told where the old one was (`HostSlot::resume` →
`CanvasView::resume_at`: the camera at once, the active card once the snapshot brings it).
One canvas is on show; the host name in the top
bar is the switcher (status dot: green connected, amber connecting / reconnecting, red needs
pairing; rows list every host with "forget", then "Add host…"), ⌘⌥→ / ⌘⌥← and the Host menu
step through them, and on a phone the same tap on the name opens it. The "N need you" pill
and the Dock badge count agents across all hosts; a tap jumps to the next one, switching host
when the one on show has none. A banner names only a session, so its response finds the host
whose canvas holds it, switches, then answers or reveals. The pairing store is the map in
`client.json` (`slopty_net::identity`); the panel appears with no host paired, or on "Add
host…" (with a Cancel), and a fresh pairing joins the switcher without touching the others.

**Pairing.** Unpaired installations show a pairing panel instead of the canvas: paste the
ticket `slopty host ticket` printed on the host (a "Paste & pair" button reads the clipboard,
which is the only practical path on a phone). `slopty_app::net::pair_host` redeems it the same
way the CLI does and the host's connect loop starts.

## 7. Crate map

| Crate | Role | Platform |
|---|---|---|
| `slopty-core` | ids, clocks, errors, small shared types | all |
| `slopty-proto` | wire messages, versioning, codec (postcard) | all |
| `slopty-grid` | terminal frame model, row diff, line cache | all |
| `slopty-engine` | `VtEngine` trait + libghostty-vt backend | host |
| `slopty-pty` | openpty/spawn/resize, ptyd protocol | host |
| `slopty-predict` | speculative local echo | client |
| `slopty-net` | iroh endpoint (`Reach::Anywhere` relays+pkarr, or `DirectOnly`), pairing/auth, channels | all |
| `slopty-media` | packetizer, FEC, reassembly, NACK/refresh policy, redundancy | all |
| `slopty-capture` | ScreenCaptureKit | host |
| `slopty-codec` | VideoToolbox encode (host) / decode (all) | split |
| `slopty-input` | CGEvent injection for remote-window input (keymap, pointer/scroll/keys, owner activation) | host |
| `slopty-agent` | Claude Code hook payloads → per-session `AgentStatus` | host |
| `slopty-host` | session manager, mux, fan-out | host |
| `slopty-client` | client session state, canvas document | client |
| `slopty-settings` | `settings.toml` schema, defaults, loading with fallback, data dir | client |
| `slopty-theme` | design tokens, dark and light variants | client |
| `slopty-ui` | GPUI elements and views; headless `#[gpui::test]` tests drive them through `VisualTestContext` | client |
| `slopty-platform` | process-level platform helpers: keep the process out of App Nap and timer coalescing while a session is live, and raise the user's attention | all |
| `slopty-app` | the app shell shared by macOS and iOS: workspace window, host switcher, pairing panel, one link loop per host, settings | client |
| `slopty-e2e` | app self-test: control-socket wire types, tokio driver, daemon+app harness (the app on the Mac or in the iOS simulator), numeric golden diff, frame-time scenarios (`cargo xtask e2e app\|ios\|smooth\|smooth-ios`), and `slopty-idle-window`, a window the harness owns so a capture target that never draws can be tested without touching anything else on the desktop | dev |
| `apps/slopty-ptyd` | PTY custodian daemon (LaunchAgent) | host |
| `apps/slopty-hostd` | host daemon | host |
| `apps/slopty` | macOS app: logging, runtime, window options, then `slopty_app::open_workspace` | client |
| `apps/slopty-ios` | iOS static library (`slopty_ios_run` called from a UIKit shim); `cargo xtask ios sim [--sim iphone\|ipad]\|device` generates the Xcode project | client |
| `apps/slopty-cli` | `slopty` CLI: host ctl, pairing, raw-mode reference client (`open`/`attach`), hook relay | host |
| `xtask` | all scripts (build, gates, bundle, sign, icon from `assets/icon.svg`, `e2e app|ios|host|screen|input|all` for the self-test and the gated live tests) | dev |

Dependency direction is strictly downward in that table; `slopty-ui` never sees `slopty-host`.
