# Slopty architecture

Slopty is a remote-coding workstation: a macOS **host** exposes shells and windows; macOS and iOS
**clients** show them on one infinite canvas, mixed freely, with Claude Code agents surfaced as
first-class objects. Everything is Rust. Floor: macOS 26.5 / iOS 26.5, Apple silicon.

This file is the map. Rulings and their evidence live in [DECISIONS.md](DECISIONS.md).
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

- Reconnect and multi-client are cheap: a joining client receives the current screen plus a
  scrollback window; nothing is replayed.
- A slow link never falls behind a fast program: the host coalesces to one diff per tick.
- The client needs no VT engine at all. iOS never builds Zig.
- The client keeps a **line cache** (absolute line numbers) so scrollback scrolls locally; missing
  ranges are fetched, prefetched around the viewport. Mouse selection is client-side too
  (absolute line indices, ⌘C copies from the cache, ⌘V sends `Paste`; drag, double/triple click,
  or long-press on touch); nothing reaches the host. ⌘-click opens the link under the pointer:
the OSC 8 run the host put on the row, else the URL found in the cached row text
(`slopty-ui::terminal::url`); ⌘-hover underlines it.
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
at each. Every `Line` then carries a `SemanticMark`: `Prompt { exit }` on the row a prompt
started (with the previous command's status), `PromptContinuation` for the rest of a
multi-line prompt, `Input`, `Output`. The client side is in §6.

**Render metrics.** The cell grid is derived exactly as ghostty derives it
(`slopty_ui::terminal::metrics`, ported from `vendor/ghostty/src/font/Metrics.zig`): a pure
function from a face — advance, ascent, descent, line gap, and the underline/strikethrough
metrics when the font has them — to whole **device** pixels for the cell, the baseline, the
underline, the strikethrough, the overline and the cursor. The element measures the face at
`font_size × scale`, derives once — a zoomed grid is that one scaled, so the columns that fit
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
no relay hop. Losing the hostd connection resumes draining; the child never blocks. A bare
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
never overwrites what the stream itself proved).

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
the stream and its frame counter, so the `Source` hint above never flips on it); each switch
is a `Transition` that becomes the stream's state only when every ScreenCaptureKit completion
callback has succeeded, a failed one is asked again next tick. A window that closes under a
crop ends the stream the way the window filter does (`on_stop` → `Closed`), never showing the
desktop where it was. The pure crop geometry (`slopty_capture::crop_for`: window frame →
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
keeps its enumeration for 2 s; cursor sampler, which also sends the heartbeats; input injection), `slopty-input` (client
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
note (`ItemKind` in `crates/slopty-proto/src/canvas.rs`). Camera `{x, y, zoom}`; zoom is real (we own the renderer), with semantic LOD: full terminal
at ≥ 0.6×, summary card (title, last lines, agent state) below. Off-screen terminals keep their
line cache and stop painting; off-screen video pauses decode (kind-aware culling). Layout is a
document synced through the host so every client sees the same canvas (`slopty-host::canvas`
owns it, `slopty-client::canvas` mirrors it with optimistic local ops, `slopty-ui::canvas` draws
it: two-finger scroll pans, pinch / ⌘-scroll zooms about the pointer, title bar drags, corner
grip resizes, ⌘T/⌘⇧N/⌘O/⌘W/⌘0/⌘1/⌘=/⌘-/⌘⇧A are the keyboard surface; a minimap in the corner
shows every item and the viewport and scrubs the camera). Notes (⌘⇧N) are edited
in place (`slopty-ui::note`) and their text lives in the document. The picker (⌘O) lists the
canvas's sessions first — agents waiting on the human, then other agents with their status
line, then plain shells — and a click reveals and focuses that terminal; below them the host's
windows and displays. Remote-window items are
created from the picker; `reconcile_screens` opens a stream for every window/display item
that lacks one and closes streams for items that disappeared, so the document, not the UI, is
the source of truth for what is being streamed.

## 5. Agents

Claude Code only, for now. "+ agent" / ⌘⇧T opens a terminal running `claude` (a bare name,
resolved on the host through the login shell). Four signals, in precedence order: hooks
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
(`CanvasView::notification_response`). Later: ACP (`agent-client-protocol`) for structured
control.

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
query the tool was given) and the whole input as pretty JSON, `tool_result` blocks with
`is_error` and named after their `tool_use_id` (the `Tail` remembers the last 512 calls).
Thinking, tool input and tool output are `Clipped` on the host: the first 40 whole lines or
4 000 characters, whichever comes first, and the count of lines dropped, so the wire never
carries a whole file. Sidechain rows and the app's injected texts are skipped. The client
keeps them in `slopty_ui::terminal::conversation::Conversation`: a bottom-aligned gpui `list`
in `FollowMode::Tail` (pinned to the newest entry until the reader scrolls up, then a
"↓ latest" pill re-pins; a reset re-pins) with `TextView::markdown` for assistant turns,
"HH:MM" local-time stamps on prompts and answers, and folds that open on a click — thinking
and tool input start folded, a result shows its first 4 lines. Under the list sits the
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
`ios-pad-*.png` under `crates/slopty-e2e/golden`).
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

**Terminal element.** `slopty-ui::terminal` draws the cached lines as one element (glyph
runs shaped per row and cached by content hash, background quads, cursor, selection,
underlines and strikethroughs at the offsets §2's metrics derive, ⌘-hover link underline) with a hairline over every prompt-start row but the first line: the
command-block separator, the foreground at 18 % alpha, or the theme's `surfaces.error` token
at `alpha::SEPARATOR_ERROR` (70 %) when the row's `Prompt { exit }` is non-zero
(`separator_color` in `crates/slopty-ui/src/terminal/element.rs`; `crates/slopty-theme/src/lib.rs`). Terminal-context bindings: ⌘C /
⌘V copy and paste, ⌘F / ⌘G / ⌘⇧G search, ⌘↑ / ⌘↓ scroll the previous / next prompt start to
the top of the viewport (`TermState::prompt_before/after` over the cached lines, uncached
history is not fetched first; ⌘↓ past the newest prompt goes back to following output),
⌘⇧C copies the last finished command's output (`TermState::last_command_output`: the
`Output` rows right above the newest prompt start, blank tail trimmed; nothing without shell
integration). Headless `#[gpui::test]`s in `terminal/view.rs` read the separators back from
`painted_quads()` and drive the bindings with `simulate_keystrokes`. ⌘⇧L swaps the element
for the conversation view (§5), whose composer takes the typing.

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
because each host owns its canvas document. One canvas is on show; the host name in the top
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
| `slopty-e2e` | app self-test: control-socket wire types, tokio driver, daemon+app harness (the app on the Mac or in the iOS simulator), numeric golden diff (`cargo xtask e2e app\|ios`) | dev |
| `apps/slopty-ptyd` | PTY custodian daemon (LaunchAgent) | host |
| `apps/slopty-hostd` | host daemon | host |
| `apps/slopty` | macOS app: logging, runtime, window options, then `slopty_app::open_workspace` | client |
| `apps/slopty-ios` | iOS static library (`slopty_ios_run` called from a UIKit shim); `cargo xtask ios sim [--sim iphone\|ipad]\|device` generates the Xcode project | client |
| `apps/slopty-cli` | `slopty` CLI: host ctl, pairing, raw-mode reference client (`open`/`attach`), hook relay | host |
| `xtask` | all scripts (build, gates, bundle, sign, icon from `assets/icon.svg`, `e2e app|ios|host|screen|input|all` for the self-test and the gated live tests) | dev |

Dependency direction is strictly downward in that table; `slopty-ui` never sees `slopty-host`.
