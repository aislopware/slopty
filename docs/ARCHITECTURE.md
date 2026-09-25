# Slopty architecture

Slopty is a remote-coding workstation: macOS **workers** expose shells and windows; macOS
and iOS **clients** show them as tiles in one scrollable tiling workspace, several workers mixed, with Claude Code agents surfaced as
first-class objects. Everything is Rust. Floor: macOS 26.5 / iOS 26.5, Apple silicon.

This file is the map. Rulings and their evidence live under [docs/decisions/](DECISIONS.md), one file per topic.

## 1. Shape

```
                     ┌─────────────────────────── worker (macOS) ───────────────────────────┐
                     │  slopty-ptyd            slopty-worker                                 │
                     │  ┌────────────┐  fd     ┌───────────────────────────────────────┐    │
   shells/agents ◀──▶│  │ PTY keeper │ ──────▶ │ sessions · VT engine (libghostty-vt)  │    │
                     │  │ (tiny,     │ SCM_    │ frame diff · replay · fan-out          │    │
                     │  │  outlives  │ RIGHTS  │ agent status (hooks/JSONL)             │    │
                     │  │  worker)   │         │ screen: SCK → VT HEVC → FEC → datagrams│    │
                     │  └────────────┘         │ input: CGEvent injection               │    │
                     │                         └──────────────┬────────────────────────┘    │
                     └────────────────────────────────────────┼─────────────────────────────┘
                                                              │ QUIC, plaintext: streams = control/terminal
                                                              │              datagrams = video/audio/cursor
                     ┌────────────────────────────────────────┼─────────────────────────────┐
                     │ client (macOS app · iOS app)           ▼                              │
                     │ slopty-client: session state · line cache · prediction · items · layout│
                     │ slopty-ui (GPUI): workspace · terminal element · surface element · chrome│
                     │ slopty-codec (decode) → CVPixelBuffer → gpui surface (zero copy)      │
                     └───────────────────────────────────────────────────────────────────────┘
```

Three kinds of traffic, one QUIC connection per (client, worker). The connection is QUIC over
plain UDP (n0's `noq`, standalone) with a null crypto provider: every worker is reached over
Tailscale, WireGuard or a VPN, which encrypts and authenticates already, so Slopty adds no
TLS, no endpoint keys, no relays and no pairing. A worker is named by `host[:port]` (a
`MagicDNS` name, a LAN name or an IP; port 45550 by default) and identified by the `WorkerId`
in its `HelloAck`; it admits a connection by source address alone — loopback, the tailnet
(`100.64.0.0/10`, `fd7a:115c:a1e0::/48`), private LANs, or the `[worker] allow` ranges of its
`settings.toml` — once per connection, before any handshake state exists.

| Path | QUIC primitive | Payload |
|---|---|---|
| Control | one bidirectional stream, length-prefixed `postcard` messages | hello, open/close, resize, item registry sync, agent events |
| Terminal | one unidirectional stream per session, worker→client; input on the control stream | grid **row diffs** from the worker-side VT engine, scrollback line pages on demand |
| Media | unreliable datagrams (RFC 9221) | HEVC fragments + Reed–Solomon parity, Opus audio, cursor position/shape; client → worker: `Feedback` (NACK, refresh), each datagram standing alone so a lost one never holds the next back |

## 2. Terminal: the VT engine lives on the worker

The worker runs `libghostty-vt` against the real PTY and ships **rendered rows**, not bytes
(mosh / zellij / wezterm-mux model). Consequences:

- Reconnect and multi-client are proven, not just cheap (`crates/slopty-e2e/tests/pair.rs`,
  `cargo xtask e2e pair`): a joining client receives the current screen plus a scrollback window,
  nothing is replayed. Two clients share one worker and its items at once, each arranging them in its own layout. A terminal opened on one
  is on the other within a round trip; both type into the same session and the actor serialises
  their keys in arrival order (nothing lost or reordered); one client is the size **driver** (§2,
  the "take" pill) while the rest are viewers; an agent's attention badges every client; a client
  that dies leaves the others streaming and reattaches on relaunch (its dead connection's cleanup
  detaches only its own sinks, `Sender::same_channel`). See DECISIONS "Multi-client" for the
  state-ownership and input-contention rulings.
- A slow link never falls behind a fast program: each viewer has at most two frames in flight
  (a credit on `session::Outbound` that returns when the connection has written the frame), the
  actor builds the next diff only when a viewer has room, the engine coalescing meanwhile, and a
  viewer that missed a diff is sent every row at the others' sequence number. Other events
  (lines, matches, title, bell) are never skipped. A joining viewer's frame is built the same
  way, so nobody else is made to resync. Input buys up to two frames outside the 8 ms pace for
  50 ms, so an echo beside a flood is not held to it.
- The client needs no VT engine at all. iOS never builds Zig.
- The client keeps a **line cache** (absolute line numbers) so scrollback scrolls locally; missing
  ranges are fetched, prefetched around the viewport; the cache indexes its prompt rows, so a
  block's prompt is a range query, not a walk. Mouse selection is client-side too
  (absolute line indices, ⌘C copies from the cache, ⌘V sends `Paste`, held for a
  confirmation strip first when it holds a newline and the program has not asked for
  bracketed paste (`paste_is_safe`, `[terminal] paste_protection`); drag, ⌥-drag for a
  rectangle, double/triple click, ⇧-click to move the near end, or long-press on touch; a drag past the grid's top or bottom
  keeps scrolling through the cache at a pace set by the distance); nothing reaches the worker,
  except a plain click on the shell's input line, which becomes the arrow keys that put the
  cursor there (`TermState::cursor_path_to`), and, with `copy_on_select` set, a selection
  goes to the clipboard as it ends.
  A scrollbar thumb over the grid's right edge is an overlay, as on macOS: hidden at rest, it
  shows while the viewport scrolls, while the pointer is near the right edge and while the thumb
  is held, then stays a second and fades out (at once under Reduce Motion); never without
  history (`terminal::scrollbar`). It drags and its track pages. The wheel
  scrolls the cache in whole lines (fractions carried between events), except for a program
  tracking the mouse or the alternate screen, whose rows go to the worker as `Wheel` (button
  presses, or cursor keys under alternate scroll). ⌘-click opens the link under the pointer:
the OSC 8 run the worker put on the row, else the URL found in the cached row text
(`slopty-ui::terminal::url`); ⌘-hover underlines it. A file path under the pointer
(`src/main.rs:12:5`, `url::path_at_col`) opens the same way: ⌘-click types
`${EDITOR:-vi} +12 'src/main.rs'` at the prompt, or opens a file tile for it while a command
runs and on a tap the phone's key-bar ⌘ armed (`TerminalViewEvent::ViewFile`, the workspace
making the path absolute against the shell's directory, `WorkspaceView::absolute_in_session`).
- **Prediction**: the client applies mosh-style speculative echo for printable keys, confidence
  gated on measured RTT, reconciled against the next authoritative diff (see `slopty-predict`).
- Terminal size is owned by one **driver** client (the one that opened the session, else the
  first to attach; anyone can take it with the "take" pill, which also fits the item to their
  viewport); viewers see the driver's grid.

**Absolute line numbering.** Clients cache scrollback by `LineIndex` (absolute, monotonic). The
engine keeps a libghostty *tracked grid ref* pinned to the newest active row and re-derives the
index of screen row 0 after every write, so worker-side eviction never shifts indices. When
numbering cannot survive — reflow on resize, RIS, alternate-screen switch, a single write that
scrolls past the whole scrollback — the frame's `epoch` bumps and clients drop their cache.
Returning from the alternate screen restores the parked primary anchor, so the cache survives
`vim`/`less` round trips.

**Search** is a worker request (`TermRequest::Search` → `TermEvent::Matches`): the engine renders
the retained rows as plain text with libghostty's formatter and maps hits back to cells, so the
whole 50k-line history is searchable without the client ever holding it. `regex: true` runs the
needle through the `regex` crate instead of the literal matcher; a bad pattern comes back as
`TermEvent::SearchInvalid`.

**Escapes programs rely on.** OSC 8 links ride on the rows: the engine asks libghostty for the
URI of every linked cell (`ghostty_grid_ref_hyperlink_uri`, only on rows whose page flag says
they may hold one) and folds them into `Line::links`, a list of `Hyperlink { col, len, uri }`
runs, so a link-free row costs one byte and no cell carries an id. The client draws the run
under the pointer underlined while ⌘ is held (the pointer a hand, an I-beam elsewhere, the
arrow while a program reports the mouse), names the target in a chip at the card's corner
(`TerminalView::link_target`) and opens the URI on ⌘-click; the OSC 8 target
wins over the plain-text URL scan (`slopty_ui::terminal::url`). OSC 52 (and iTerm2 OSC 1337
Copy) writes to the *system* clipboard become `TermEvent::ClipboardWrite`, capped at
`MAX_OSC52_BYTES`, and every attached client puts the text on its own clipboard;
selection/primary targets and every read (`?`) are dropped on the worker, and no message exists
for a read reply. Colour queries (OSC 10/11/12 `?`, OSC 4) are answered by libghostty from
the defaults the engine sets: the dark theme's foreground, background, cursor and ANSI 0–15
at start (`ghostty::set_colors`, from `slopty_theme::TerminalPalette::DARK.wire()`), then
whatever the driver paints with — every client sends `TermRequest::Colors(TermColors)` after
its attach and on a theme change, the session keeps each viewer's, and the driver's (on
attach, on claiming the wheel, on a change) reach `GhosttyEngine::set_colors`. The driver's
background also decides the colour scheme: `CSI ? 996 n` is answered light or dark from its
luma, and a program that set mode 2031 hears a change of scheme unprompted. Cell colours still
travel symbolically (`Color::Default`/`Palette`/`Rgb`), so each client's own theme paints
them — under the program's own changes: libghostty has no colour-change callback, so after
each chunk the engine reads the current fg/bg/cursor/palette against the defaults, and a
difference is `TermEvent::Colors(ColorOverrides)` with the whole set (an OSC 104/110/111/112
reset sends it again without the entry; RIS keeps them, as xterm does). A late attach gets the set ahead of its first
frame; `slopty_theme::Colors` paints it over the client's theme, ANSI 0–15 and the cube
alike, and the shaped-word cache keys on it. OSC 9, OSC 777 `notify` and OSC 99 desktop notifications (libghostty parses
all three) become `TermEvent::Notification { title, body }`, each field capped at 512 chars;
the workspace posts them as a notification-centre banner when no window is active, tagged by the
session so a click reveals the card, and bounces the Dock like an agent's attention. BEL tints
the card's grid for a flash (`TerminalView::bell_flashing`, `alpha::FAINT`) and, when no window
is active and `[terminal] bell_alert` holds (the default), plays the alert sound and bounces
the Dock.

**Kitty graphics.** libghostty keeps the images (`KITTY_STORAGE_BYTES` per screen; PNG
decoded through the `png` crate by `slopty_engine::graphics::PngDecoder`) and lays the
placements out at the client's cell pixels. Every `Frame` lists the placements on the
viewport (`Frame.images: Vec<Placement>` — cell, offsets, painted size, source rectangle,
z), and a placement's pixels travel once as `TermEvent::Image` (RGBA, sampled down by a
whole factor when over `IMAGE_WIRE_BYTES`) ahead of the first frame that places them, and
again after a full frame (attach, resync). Worker and client keep the same bounded cache
(`IMAGE_CACHE_BYTES`, least recently placed first: `graphics::Ledger` and
`TermState::keep_image`), so the worker knows what to re-send. A placement change with no cell
change still makes a frame (the storage generation is part of the dirty check). The view
makes one GPUI texture per image generation (`TerminalView::placed_images`, BGRA
premultiplied) and the element paints the source rectangle into the placement's cells
(`placement_bounds`), under the glyphs for `z < 0` and over them otherwise, shifted by the
rows a scrolled view shows. A virtual placement (`U=1`) is shown through Unicode
placeholders: the worker reads every U+10EEEE cell (image id in the foreground colour,
placement id in the underline colour, tile row and column in the combining diacritics —
`engine::placeholder`), joins consecutive cells into runs by kitty's rules, sends the cell
blank, and emits one ordinary `Placement` per run with ghostty's geometry (the image scaled
to fit the placement's grid and centred, the run showing its own strip), so the client
paints it like any other and never sees the placeholder.

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
fish ≥ 4 emits the marks itself and the snippet steps aside. zsh's line-init widget marks a prompt whose PS1 lost the marks in place (`133;P;k=i` + `B`). All three emit `133;A` (in the
prompt), `B`, `C` and `D;<status>`. `SLOPTY_NO_SHELL_INTEGRATION=1` in the daemon's or the
session's environment opts out (`apply` then changes nothing); other programs run untouched. The snippets also wrap `sudo` to keep `TERMINFO` (our compiled `xterm-ghostty` entry) when it is set, and zsh's zle hooks shape the cursor by keymap (bar to insert, block in vi command mode, the program's shape again before a command runs). The engine takes libghostty's per-row prompt flag for the row kind and, because
the library exposes neither which row an `A` landed on nor the status a `D` carries, scans the
PTY bytes for those two marks itself (`slopty_engine::osc133::Scanner`, state kept across
reads so a mark split over two PTY reads is still found) and notes the cursor's absolute line
at each. Every `Line` then carries a `SemanticMark`: `Prompt { exit, input }` on the row a
prompt started (with the previous command's status), `PromptContinuation { input }` for the
rest of a multi-line prompt, `Input`, `Output`; `input` is the column of the row's first
`Input` cell (libghostty's per-cell `CellSemanticContent`), where the typed command begins. The scanner also reports `C`: libghostty takes that row out of the prompt without writing a cell, so the engine keeps the line in `forced_rows` and the next frame carries it (prompt flag read from the live grid, since the render state only copies dirty rows); without this a `sleep` typed at a fresh prompt showed as running only when it ended. `slopty_worker::session=trace` logs every PTY read's bytes (`pty read`), the evidence behind the rulings in `docs/decisions/terminal.md`. The client side is in §6.

**Render metrics.** The cell grid is derived exactly as ghostty derives it
(`slopty_ui::terminal::metrics`, ported from `vendor/ghostty/src/font/Metrics.zig`): a pure
function from a face — advance, ascent, descent, line gap, and the underline/strikethrough
metrics when the font has them — to whole **device** pixels for the cell, the baseline, the
underline, the strikethrough, the overline and the cursor (which spans both columns of a
wide character). Underlines paint under the glyphs, strikethroughs over them. Box drawing,
block elements, sextants, octants, the legacy computing symbols (wedges, eighth bars, shaded
halves, checkerboards, hatching, corner diagonals), Braille and Powerline cells are not
shaped at all: `terminal::sprite` turns each into rectangles, polygons, arcs and strokes from
the cell size and the underline thickness, snapped to device pixels, so borders never seam
between rows. As in ghostty, each is rasterised once per (character, cell in device pixels,
line thickness) into GPUI's atlas, as an SVG mask the element writes and `Window::paint_svg`
tints with the cell's colour; only while the zoom is in motion is the geometry painted
directly. The face comes from the font's own
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
the master, and drains it into a bounded ring while no worker holds it. `Attach` pauses the reader
(handshake through a `watch` pair so the fd is never read by two parties), ships the ring
contents plus the master over `SCM_RIGHTS`, and the worker reads the fd directly from then on —
no relay hop. Losing the worker's connection resumes draining; the child never blocks. While a
worker holds the master it feeds the ring itself (`Output`, a copy of every read, on the same
connection, which is the only one ptyd accepts taps from) and replaces the ring with a
`Checkpoint`, the engine's whole state as VT bytes from libghostty-vt's formatter (palette,
modes, every retained row, margins, cursor): right after adopting, then 500 ms after the last
output (deferred while the output stands inside an escape sequence) or once 1 MiB has been
tapped, and after a resize (the size goes to ptyd first, on the same queue). `Attach` returns
the checkpoint and the ring, and the new worker's engine replays them in
that order, so the screen, the scrollback, the directory and the program's colour changes (the
checkpoint carries them as the OSC sequences that made them, never a palette dump) survive a
worker restart or crash; what the replay reports is kept for the first attach, not broadcast or
answered. A bare
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

**Deployment.** `slopty worker install` writes two LaunchAgents (`dev.aislopware.slopty.ptyd`,
`dev.aislopware.slopty.worker`; `KeepAlive`, `RunAtLoad`, `ProcessType Interactive`) with the
sockets under `<data dir>/run/` and logs in `~/Library/Logs/Slopty`, bootstraps them, and
waits for the daemon to answer; `uninstall` and `service` undo and report. The CLI finds the
installed socket by itself, so `slopty worker status` works without launchd's environment.
`slopty worker doctor` asks the running daemon about itself (`CtlRequest::Doctor` →
`Health`): Screen Recording and Accessibility as *that binary* sees them (TCC grants are per
executable, so the report names the path to add), listen address, admitted ranges, connected
clients, sessions;
exit status 1 while a permission is missing, so it can gate a setup script. For an ad-hoc
(`cargo build`) binary "per executable" means per build: the cdhash changes and the grant is gone,
silently. `cargo xtask sign` (and `xtask run worker`, which calls it) signs both daemons under
`dev.aislopware.slopty.worker` and `….ptyd` with a Developer ID certificate, which makes the
requirement the identifier rather than the hash and one approval enough for good.

**Worker link and orchestration.** With a server configured (`--server`, `SLOPTY_SERVER` or
`[worker] server`), the worker registers with it (`apps/slopty-worker/src/server.rs`). It sends
its capabilities (`slopty-worker::caps`) and its sessions, forwards session and agent events,
and redials with capped backoff when the link drops. Every `SessionSummary` carries the agent
running in the session and its status, read from the daemon's agent table (`Worker::summaries`),
and the server's hub keeps each listed terminal's agent current from the `Agent` events, so
`ListTerminals` (and `slopty workers`, which counts the agents waiting on a human from it)
answers without asking each worker. `slopty-worker::orchestrate::Orchestrator`
answers the verbs the server forwards. It opens terminals through the same path a client's
`OpenSession` takes, types through the session's key and paste encoders, and reads text views
of the engine on the session's thread (`GhosttyEngine::screen_text`, `text_lines`,
`text_since`, `commands`). `WaitFor` sleeps on the session's `Activity` watch channel and
matches output from a per-session mark, as `expect` does. `slopty-worker::ports` finds the TCP
listeners in each terminal's process tree through libproc. The server's hub answers two verbs
itself: `Events`, a long poll on a bounded log of what it hears from every worker (liveness,
terminals opened and closed, agent status changes) read from a cursor, which is how one agent
watches the whole fleet over stateless HTTP; and `ForgetWorker`. Files are read in ranges of at
most 8 MiB with the whole size reported, directories listed with `ListDir`, paths checked with
`Stat`; a terminal opened by a verb takes a size, and `ResizeTerminal` resizes one no client
shows. Rulings in `docs/decisions/topology.md`.

Crates: `slopty-engine` (trait + libghostty-vt backend), `slopty-grid` (frame model, diff, cache),
`slopty-predict`, `slopty-pty` (openpty/spawn, async master, ptyd protocol + client),
`slopty-worker`; app `slopty-ptyd`.

## 3. Remote windows: Parsec-class pipeline

```
SCStream(display | window-as-display-crop | window, 420f BT.709, minimumFrameInterval, queueDepth=2, showsCursor=false)
  → VTCompressionSession(HEVC, EnableLowLatencyRateControl @ creation, RealTime,
                         AllowFrameReordering=false, AllowOpenGOP=false, MaxFrameDelayCount=0,
                         EnableLTR, MaxKeyFrameInterval=∞, AverageBitRate + DataRateLimits)
  → packetize (≤1200 B datagrams, 16 B header) → reed-solomon-simd parity per frame
  → QUIC datagrams                                      ── client: NACK/refresh datagrams; LTR acks + telemetry on the control stream
client: reassemble/recover → VTDecompressionSession(RealTime) → CVPixelBuffer (IOSurface)
  → gpui surface (CVMetalTextureCache, zero copy) → present on arrival (next vsync)
```

Loss recovery order: FEC (free) → NACK inside the playout window → `ForceLTRRefresh` (small
P-frame from an acked LTR) → IDR only when no acked LTR exists. The NACK delay is not a
constant: it is a quarter of the transport's measured round trip, floored at 1 ms and capped at
20 ms (`slopty_media::NackDelay`), so a loopback link repairs in a millisecond and a 40 ms one
waits 10 ms rather than asking again for fragments still in flight. Cursor is a separate
low-rate channel drawn client-side, so pointer latency is one RTT, not one video pipeline:
the position rides in `Cursor` datagrams (120 Hz, sent on change) and the picture on the
control stream (`ScreenEvent::Cursor`, a `CursorShape` of premultiplied BGRA with its hotspot
and backing scale), read by `slopty_capture::cursor_shape` from `NSCursor.currentSystemCursor`
at 30 Hz while the worker's pointer is over the target and sent only when it changed
(`slopty_worker::screen::ShapeDedup`); the client draws that picture with its hotspot on the
position (`ScreenView`'s `Pointer`), and its own arrow until the first one arrives. The position
scales by the stream size input maps with, the one last asked for, not the frame in flight. While a
frame is up the card hides the client's own pointer (`CursorStyle::None`, a fork addition: on
macOS a cursor rect with a one-pixel clear `NSCursor`, restored by AppKit when the pointer
leaves), so only the worker's pointer shows on it.

**How much parity.** The worker's `Redundancy` turns the receiver reports into the ratio the
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
Every datagram carries `send_ms_lo`, the low byte of the worker's send clock, so the difference
between two stamps is how long the worker waited between sending them; subtracting that from the
arrival gap leaves the link's share, and only that counts as a stall or is charged to
`stalled_ms`. Past the stamp's 256 ms range, on a retransmission (which carries its original
frame's stamp) and while a gap is still open the receiver keeps the pessimistic reading. This
matters beyond the HUD: the bitrate and parity controllers both throw away windows the receiver
spent stalled, so mislabelling a quiet source as a stall threw away good evidence.

**When to stop asking.** A receiver that needs a refresh repeats the request with a doubling
backoff, which is right for a stream that has stopped and wrong for a target that never started:
a hidden window produces no frames, so no amount of asking helps. The worker answers that directly
— `ScreenEvent::Source { state: Idle | Live }` (protocol 13), sent 400 ms after `Opened` if the
encoder has produced nothing and again the moment it does — and the receiver stops asking while
the source is idle (`Reassembler::set_source_live`), with the element's placeholder saying
"waiting for the window to draw" instead of "waiting for the first frame". For a worker too silent
to send the hint, `Config::refresh_max_repeats` (12, ≈17 s with the backoff) is the fallback cap.
The two are cleared by different evidence: *any* datagram on the stream — a heartbeat included —
restarts the cap, because it proves the worker is still there, while only a video fragment lifts the
idle suppression, because only a picture proves the source is drawing (a hint that has not changed
never overwrites what the stream itself proved). The hint follows the *recent* frames, not the
first one (`SourceTracker`, clock injected so the rule is unit-tested): a new frame is `Live` at
once, `SOURCE_QUIET_AFTER` (2 s) without one is `Idle` again — longer than the start-up grace so a
target drawing once a second does not flap — and a target the geometry tick reports off screen is
`Idle` immediately, since a window that is not on screen cannot be drawing.

**Worker capture path.** A display target is one `SCContentFilter(display:)`. A window target
is served two ways, and the worker switches between them on the live stream
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
the list. `SLOPTY_WINDOW_CAPTURE=window|crop` on the worker forces a path. Every frame carries the window server's display time
(`SCStreamFrameInfoDisplayTime`, equal to the sample's pts) and the worker keeps two latency
rings per stream — display time → SCK callback (`ScreenStats::capture`) and encoder submit →
VideoToolbox callback (`ScreenStats::encode`), p50/p95/max over the last 600 frames — read
locally over the control socket (`slopty worker screens`; `slopty bench screen` appends them on
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
The worker stops sending 300 ms after the last non-silent sample, so silent apps cost nothing.
The client decodes with `AudioConverter` and plays through an `AudioQueue` of three 10 ms
buffers fed from a jitter buffer (`slopty-codec::audio::Playout`) that aims for the lateness it
measures (p95 over 5 s plus a device buffer, 20–120 ms), prefills after a start, a mute or a
dry run, and converges by dropping or repeating one 5 ms slice with a crossfade, which also
absorbs clock drift;
iOS puts the app in the `Playback` session category so it plays past the ring switch.
Mute is per item and per client (the "mute" pill, ⌘⇧M, View ▸ Mute Window): packets still
arrive and decode, only playback stops, so unmuting is instant and other clients hear nothing
different.

**Clipboard, files and ports** ride beside the control stream (`slopty-proto::transfer`,
`slopty-net::streams`). Control messages (`ClipMsg`, `XferMsg`, `WorkerMsg::Ports`) go on the
control stream. Bytes go on unidirectional bulk streams that open with `UniHead::Bulk` and are
sent at priority −1, so a file never queues ahead of a keystroke's echo or a video frame. A
forwarded TCP connection is a client-opened bidirectional stream that opens with `TunnelOpen`.

- **Clipboard: announce, then fetch.** The worker reads `NSPasteboard.changeCount` every 200 ms,
  but only while some connection has sent `Watch(true)`. A client sends that while a remote
  tile has focus and the app is frontmost. A change goes out as an `Offer`: every
  representation (text, PNG, TIFF, RTF, HTML, file URLs) with its size and BLAKE3 digest.
  Plain text of 64 KiB or less rides inline. Concealed and transient contents are never
  offered.
  - The macOS client puts promises on the general pasteboard (`NSPasteboardItem` data
    providers). A paste fetches the representation, inline or over a bulk stream.
  - The worker writes a client's offer to its pasteboard when that client's ⌘V reaches a window.
    The window's input is held in order until any fetch lands, for up to 3 s.
  - Every Slopty write carries `com.aislopware.slopty.origin` (`transfer::origin_bytes`).
    Together with the `changeCount` of our own write and a same-digest backstop, this stops
    echoes, also when client and worker share a Mac.
  - The pasteboard sits behind `slopty_input::pasteboard::Board`, and tests use named
    pasteboards (`--pasteboard`), never the user's.
  - On iOS (`slopty_platform::pasteboard::IosPasteboard`) writing is free: an offer becomes
    one local-only `NSItemProvider`, inline text at once and the rest loaded on paste. Reading
    another app's contents can prompt, so the client reads only for a paste into a remote tile
    (`Pasteboard::reads_ask`), never when a tile takes focus. The change count, which never
    prompts, decides whether a paste reads again.
- **Files.** A drop on a terminal tile uploads with `Begin { dest: SessionCwd }`, one bulk
  stream per file.
  - The worker writes `name.partial`, fsyncs, renames, then fsyncs the directory, and sends `Done`
    with the digest. `Finished { paths }` follows once every file is in, and the client types
    the shell-quoted paths as a bracketed paste.
  - The destination is the OSC 7 directory, else the shell's cwd from the process table. A
    name already taken there lands in `~/.slopty/drop/<xfer>/` (`--drop-dir` in tests).
  - `Resume` answers the durable length of a partial, so an interrupted upload continues.
  - A drop on a streamed window goes to staging, and the files go on the worker's pasteboard
    as file URLs.
  - Copied files paste the way a drop lands (`slopty_ui::clipboard::ClipFiles`). ⌘V in a
    terminal with files copied here uploads them to the shell's directory and types their
    paths. ⌘V in a streamed window stages them, and the window's input, the chord first, waits
    on the client until the staging is done (`ScreenView::release_paste`). The offer for copied
    files carries only their URLs. The worker never writes those, but the offer replaces the
    client's earlier one, whose text would otherwise land over the staged files. Files a worker
    copied are remembered by the client that received its offer. Pasted into a shell of that
    worker, their paths are typed as they are. Pasted into another worker's tile, they come
    down to a scratch directory here, go up as above, and the scratch directory is removed.
    Both ends resolve a Finder file reference URL to its path before it travels.
  - ⌘-drag on a path in a terminal drags the file out as an `NSFilePromiseProvider` that
    fetches it (`XferMsg::Fetch`, `Purpose::Download`). The workspace hands the promises to a
    drag sink (`WorkspaceView::set_drag_sink`). In the app that is a system drag. The self-test
    parks the promises and keeps them on request (`Command::KeepDragged`). The client writes `name.partial`,
    syncs it every 8 MiB, checks it against the digest in the worker's `Done` and renames it.
    A cut attempt is called off (`Cancel`) and fetched again under a new transfer whose `held`
    names the durable bytes of each partial and each file already landed. The worker resumes a
    held file only when it sent that same version (size and modification time) before, and
    otherwise sends it from 0 (`Transfers::resume_points`).
  - Files that apps promise rather than name (Mail, Photos) are received by a view beneath
    the web pages that registers only the promise types (`slopty_platform::file_drop`), and
    on iPad every drop comes through a `UIDropInteraction`. Each drop lands in a temporary
    directory of its own and then reaches the tile under it as an ordinary file drop. A file
    that failed is named in a notice, and what it wrote is deleted.
- **File cards.** `ReadFile` answers the first 512 KiB and 2 000 lines of a text file,
  `WatchFiles` looks at each watched file's size and modification time every second and sends
  a changed one again, and `WriteFile { path, text, base_modified_ms }` saves an edit
  (`slopty_worker::file::write`). The save writes a temporary file beside the target, fsyncs
  it and renames it over, keeping the mode and writing through a symbolic link. A file whose
  modification time is newer than `base_modified_ms` is a `Conflict` and stays as it was. A
  non-regular file is `Failed`, and so is a text or a file past the 512 KiB cap. The worker
  answers with `WorkerMsg::Written`, and the watchers, the writer's own included, hear the new
  text on their next look.
- **Ports.** A session's listening TCP ports come from its process tree (libproc,
  `slopty_worker::ports`). They are scanned 250 ms after its output names a local address or an
  OSC 8 web link, and every 2 s while it runs a foreground program or listens, never when
  idle. A changed set goes to every client as `WorkerMsg::Ports`.
  - The client listens on `127.0.0.1` at the same port (the next free one when that is taken,
    with a notice), so origins, cookies and OAuth redirects keep working.
  - Each accepted connection becomes a tunnel, which the worker joins to `127.0.0.1` (then `::1`)
    with a half-close.
  - The tile shows each port as two quiet pills: the number ("5173", or "8080 → 8081" when
    served on another port here) opens a browser tile, and "↗" opens the default browser. The
    palette's port list offers both.
  - A browser tile names the worker's port, and a client serves any port it names on demand
    (`Remote::forward`, a pin in `tunnel::Forwards` kept while the link lives), so a page opens
    on every client even when no session lists its port there any more (§4).

**Platform seams.** The pipeline is `slopty_worker::screen::Pipeline<P: Platform>`, and
`ScreenStream` is `Pipeline<Native>`, macOS today (`slopty_worker::platform`). A `Platform`
names one implementation of each seam, and each seam is a trait in its platform crate that
compiles on every target: `CaptureSource` (displays, windows, frames, the window list and the
pointer; `slopty_capture::ScreenCaptureKit`), `VideoEncoder` and `AudioEncoder`
(`slopty_codec::VideoToolbox`, `slopty_codec::Opus`) and `InputSink`
(`slopty_input::CgEvents`). The clipboard seam is `slopty_input::pasteboard::Board`. The
stream is generic, so the frame path makes direct calls; the one `dyn` is the registry's
handle on each stream's counters.

Crates: `slopty-capture` (SCK streams, shareable content, pointer/bounds queries, the cursor's picture),
`slopty-codec` (encode half is `cfg(macos)`), `slopty-media` (`Packetizer` → datagrams + parity +
retransmit history; `Reassembler` → in-order frames, NACK/refresh `Action`s, `ReceiverReport`;
`Redundancy` → parity ratio; `RateController` → encoder bitrate from the reports and the QUIC
path's cwnd/rtt, `judge` the pure policy over one decision window: a stall the reassembler
reported (`stalled_ms` / `stalls` in the report) freezes the target, loss while flowing cuts
it, a clean window grows it; every decision goes back to the client as `ScreenEvent::Rate`
for the stats overlay and the bench; pure, no clocks, tested; `Cadence` → the frame rate that
target affords, a ladder of the client's ceiling then 60/30/15 chosen on bytes per frame, with
`frame_due` the gate the worker runs each capture through (decisions/video.md, the cadence ladder);
`heartbeat_datagram` → a bare
`Kind::Heartbeat` header the worker sends after `HEARTBEAT_AFTER` of silence so a still screen
or a capture gap does not read as a link stall at the receiver), `slopty-worker::screen`
(`ScreenStream`: capture → encode → packetize → a `DatagramSink`, one call per frame from the
thread that produced it; the sink answers the path's datagram limit, how many bytes QUIC is
holding in its send buffer and how wide the congestion window is, and a captured frame is
dropped rather than encoded while more than two
frames' worth at the rate in force wait there, or one window, whichever is larger
(`frame_fits`); `warm_up` runs one throwaway capture when the worker comes online and `shareable()`
keeps its enumeration for 2 s; the geometry split in two, `prober()` for the window-server
reads off the runtime and `check_geometry(&Probe)` for the decision, which also hands the probe's
bounds to the input sink; a cursor sampler that reads the bounds the last probe left and, for a
display, asks for the real pointer on the blocking pool only when it moved, while a window's
pointer is where its input last put it (`PointerWatch`, hidden before the first event), since
input posted to the window's owner leaves the real pointer alone; `release_input` lets go of what
the client held before the close waits on ScreenCaptureKit; and a heartbeat on its own task beside it — a promise about time must not share a
task with a call that takes it (DECISIONS.md, "The heartbeat has its own task"); input injection), `slopty-input` (client
`ScreenInput` → `CGEvent`, posted to the owning pid for windows or the HID tap for displays,
right clicks always through the HID tap because AppKit only tracks context menus for those;
activates the owner before clicks and keys because macOS only delivers keyboard events to the
active app, taking an owner found active as active for 250 ms; each stream's `Injector` runs on
an `InputThread` of its own, fed in order through a channel, so the stream's task never waits on
the window server; it maps through the bounds the geometry probe hands over, reading them itself
only when none newer than 250 ms came; it tracks held buttons and keys and lets go of them on
`release_all` and on drop). `slopty-worker` gives each stream a task of its own (`screens.rs`: the open,
input, quality, resize, the geometry tick and the close, in the order asked) and a `QuicSink`
over the connection; its connection loop only routes, with loss feedback and reports going
straight to the stream's `StreamControl` and everything slow (session open and close, file
reads and quick open, the pasteboard write behind a paste) on tasks that report back, so no
request waits in front of a terminal's echo. Client side, `WorkerLink::start`
warms VideoToolbox's decoder up once per process, the router stamps each datagram with its
arrival, and the stream worker drains what is queued before running the reassembler's timers
and pushes whatever the timers released straight into the decoder (a frame freed by a loss can
otherwise wait for the next datagram, which on a still screen is a heartbeat away);
`ScreenStats` carries hold, jitter and the start-up instants for the ⌘⇧I overlay
(`hud_lines`) and `slopty bench screen`, and `slopty-client::pacing` carries the arrival →
present numbers beside them. Transport: ACKs within 2 ms and a 32-packet initial
window (`slopty-net::endpoint`).

## 4. Workspace

Every worker's items in one scrollable tiling workspace, niri's model
(`docs/decisions/workspace.md`). Items: terminal, remote window, remote display, note, file, browser
(`ItemKind` in `crates/slopty-proto/src/items.rs`). The worker keeps only the registry
(`slopty-worker::items::ItemStore`: the items, their names and sleep; `Upsert`/`Remove`/`Sleep`,
a session's item made and removed with it); `slopty-client::items::ItemDoc` mirrors it with
optimistic local ops and recognises its own echo by `by == me`. Where each item sits is this
device's alone: `slopty-client::layout` is a pure niri port (workspaces stacked vertically, one
empty at the end, each an endless strip of columns of tiles; preset widths 1/3, 1/2, 2/3;
springs, swipe tracker, overview), saved as `layout.json` in the client's data directory. A
tile is `(WorkerKey, ItemId)`, so one `slopty-ui::workspace::WorkspaceView` shows every worker
at once: its own echo opens right of the focus and takes it, anything from elsewhere joins the
end of the workspace that last held that worker's tiles. A worker that drops keeps its tiles
("reconnecting"); its next snapshot removes only what it no longer has. Each terminal's agent
badge starts from its `SessionSummary.agent` when the worker connects or the session opens
(`WorkspaceView::seed_agents`), so a client that joins late names the running agents and
their status before any event arrives; an event always wins over the seed. The view draws only
tiles near the view (a far terminal prepares no rows), sizes a terminal's grid from the
tile's resting rect so a spring never resizes a PTY, lets a remote tile's stream go after 5 s
off screen, and asks for frames only while something moves. Two-finger swipes drag the strip
(axis locked after 16 pt, vertical scrolls the content under it, momentum after a snap
swallowed), ⌘⌥ and the wheel step columns, a header drag moves a tile, the gap right of a
column resizes it, a pinch opens the overview. The keys are niri's on ⌘⌥ (the table is in the
decision), bound in `Workspace && !Screen`: a focused remote window gets them all, ⌃Tab is the
way back. The titlebar is the workspace name and any worker that is down, a dot per column (none for one column),
the round trip, the agents that need you, "+" and "…". ⌘W on any tile takes it off and offers it
back for `UNDO_CLOSE` (5 s): ⌘Z, the palette's "Undo close" or the toast's "Undo" (toasts sit at the foot of the strip) put the
item back as it was (`remember_closed`, `take_back`), else `forget_closed` lets go. An idle
shell keeps its session and its attached view through the wait, so its rows come back
untouched and `forget_closed` sends the worker `Close`; every other card is its item, so the
document restores it (a note's editor commits on a timer, and the close reads the field
directly so the last keystrokes come back too). An ended shell is the one exception: it has
no session to keep and no rows the worker could replay, so it goes at once. Notes (⌘⇧N) are edited
in place (`slopty-ui::note`) and their text lives in the document; a note nobody is editing
draws that text as Markdown (`slopty-ui::markdown`), its
fenced blocks as a block element with "copy" and, given a shell, "run"
(`markdown::code_block`, `NoteViewEvent::Run` → `run_in_shell`), its task lines
(`- [ ] …`, `markdown::task_row`) as rows whose box ticks the line in the text on a click
without opening the editor (`NoteView::toggle_task`, committed at once), and a
click on it puts the caret back in the editor; its title bar reads the first non-empty
line (`workspace::note_title`, heading, list and task marks stripped, 40 chars, a checklist's
ticks counted after it as `· 1/3`), "note" while empty.
Any card takes a **name** (⌘E, or a double-click on its title bar; `Item.name`,
protocol 37, worker-sanitised to 128 characters): document state every client shows in place
of the derived title until it is cleared, and what the palette's "Go to" line says. A
**file tile** (`ItemKind::File { path }`, `slopty-ui::file`) is an editor on a file on the
worker: the item names the absolute path and lives in the shared document, the text does
not. Each client asks `ClientMsg::ReadFile` when the tile appears (`workspace
reconcile_notes_and_files`, which also sends the set of paths as `ClientMsg::WatchFiles`: the
worker looks at each one's size and modification time every second and re-sends a changed
file unasked) and puts the `WorkerMsg::File` answer (`slopty-worker::file::read`: the first
512 KiB, then the first 2 000 lines, `FileRead::Text | Binary | Missing`) into gpui-kit's code
editor (`EditorState`, line numbers, no folding or wrap) in the theme's mono font, or one line
saying why not. Colour comes from the grammar the path names (`slopty-ui::highlight`: syntect's
bundled grammars on the pure-Rust regex engine, reduced to nine tokens painted from the
theme's palette); `highlight::editor::EditorHighlighter` is the editor's highlighter, which
moves the last parse's colours with each edit (`editor::splice`) and parses the whole text
again on a background thread 60 ms after the typing stops. The same hook colours a fenced
block in an answer through gpui-kit's `TextViewDefaults`. ⌘S (`SaveFile`, key context
`FileEditor`) sends `ClientMsg::WriteFile { path, text, base_modified_ms }`, the text
carrying the file's final newline when the read had one (`FileRead::Text.final_newline`); the header shows a quiet dot while the text differs from the read, and the answer
(`WorkerMsg::Written`, `FileView::written`) clears it, or keeps it with a one-line reason.
What the worker sends next is taken by the state the tile is in: its own save's echo changes
nothing; a clean tile takes a change on disk silently, tints the lines that differ
(`file::changed_lines`, `similar` over the lines) and puts the caret on the first; a dirty tile
keeps the edit and shows "Changed on disk" with "Reload" and "Overwrite" inline, and a
`Conflict` answer to a save does the same. Reload asks for the file again and takes it;
Overwrite saves with no base. A clipped read (past 512 KiB or 2 000 lines) opens read-only
with the reason in that line (`file::clipped_reason`), since a save would cut the file. A
tile opened from an edit lands on the edit's line (`ToolDetail::Diff.line`: the worker finds
`old_string` in the file, or `new_string` once the edit landed, `transcript::locate`,
tinted in the accent tone, `FileView::focus_line`). The tile reads again when any agent's
Edit/Write result arrives in the workspace and when "view" is pressed on another call for the
same path (one tile per path: `WorkspaceView::open_file` reveals the existing one). Titled
`name · parent` (`workspace::file_title`), after a small page glyph that drags the file out
of the app (macOS, `WorkspaceView::file_proxy`); its dump entry is `ItemInfo.file` (path, summary,
lines, edited, trouble, read-only). The way in is the palette: a path typed in opens as an
`Open <path>` line against the active shell's directory, and a word asked of the worker's
files does the same for its hits; either opens the tile with the keyboard in the editor. The
palette lists every tile as "Go to <title>" after the sessions (`PaletteRun::Item`) and the last
five distinct commands of the shell a "run" would go to as "Rerun <command>"
(`TermState::recent_commands`, `PaletteRun::Rerun`, typed through `run_text`). ⌘F with the
tile active opens a find bar like the terminal's (`FileView::find`, key context `FileSearch`):
a hit is a line holding the text, smart-case (`file::find_hits`), tinted in the warn tone,
stepped with ⌘G/↩ and wrapped; Esc closes it and the editor takes the keyboard back. Inside
the editor, ⌘F is the tile's find, not gpui-kit's, and ⌘⌥↑/↓ move the focus between tiles, not add
carets (bindings in `FileEditor > Input`).

A **browser tile** (`ItemKind::Browser { url }`, `slopty-ui::browser`) shows a web page,
usually a port on the worker, in the platform's `WKWebView` (`slopty_platform::web`: an `NSView`
on macOS, a `UIView` on iOS, each a subview of the GPUI window's view from its
`raw_window_handle`). A native view always draws over GPUI, so the page follows the tile
rather than being drawn by it: each frame the tile's body records where it was drawn
(`BrowserView::drawn_in`), and a final element's prepaint (`WorkspaceView::browser_sync`)
asks `browser::placement` where each page goes, over the body and clipped to the strip, or
hidden while anything GPUI draws covers the strip (the palette, the picker, a menu, the
overview, an app dialog via `set_covered`), while the tile fades or is off the strip; a
toast cuts the clip short at its top edge (`Cover::toast`, measured where the toast was drawn
that frame). Hidden,
the body shows the page's last snapshot (`takeSnapshot` → PNG → `RenderImage`), so covering it
leaves no hole, and the renders the tests diff show it too. A click on the page gives it the
keyboard and focuses its tile; on the Mac a local event monitor takes the keyboard back on a
click elsewhere, ⌃Tab or Esc twice, and hands ⌘C, ⌘X, ⌘V, ⌘A, ⌘Z and ⇧⌘Z to the page
(`web::edit_for`), which the GPUI view's key equivalents would otherwise take; on iOS the
page's own fields take the keyboard when tapped and hiding the page ends their editing. The
item's address is the worker's (`http://localhost:5173/`, the same on every client): each
client asks its link to serve a loopback port the address names (`Remote::forward`) and loads
the page from the local port it got (`browser::local_url`), asking again when the link comes
back; what the header and the dump show is put back on the worker's port
(`browser::worker_url`). The header shows the page's title and its
address as text, "←" while there is history and "↻"; the title and address are read after
each navigation and every second while the page is open. The ways in: a port chip's number
opens the worker's port in a tile (its "↗" opens the default browser, as before), "Open URL…" in the
palette starts at `http://localhost:`, and an address typed into the palette is an "Open
<address> in a tile" line. Only http and https open (`browser::is_web_url`). The picker (⌘O, a field at
the top filtering by every word typed, ↑/↓/↩ choosing) lists the workspace's sessions first — agents waiting on the human, then other agents with their status
line, then plain shells — and a click reveals and focuses that terminal; below them the worker's
windows and displays. Remote-window items are
created from the picker; `reconcile_screens` opens a stream for every window/display item
that lacks one and closes streams for items that disappeared, so the document, not the UI, is
the source of truth for what is being streamed. The command palette (⌘⇧P, `slopty-ui::palette`)
lists the workspace's sessions to go to ("Go to <title>", agents waiting on the human first,
their status on the right) and every action by name with its keys read from the binding
tables (`workspace::palette_items` for the workspace's and the terminal's, the app's own appended
with `extend_palette`; the View menu's "Commands…" opens it on the Mac): typing
keeps the lines every word of the text is found in, ↑/↓ choose, ↩ or a click runs one, Esc
closes; a path typed in (`src/lib.rs:7`, `~/notes.md`) is an `Open <path>` line first, which
opens a file tile against the active shell's directory (`palette::path_query`); a directory
spelled from the root or home with a slash at the end (`~/proj/`) is instead a "New terminal
in …" and a "New agent in …" line (`palette::path_items`), the worker expanding `~`; and a word is
also asked of the worker's files under that directory (`ClientMsg::FindFiles` →
`WorkerMsg::FoundFiles`, protocol 36) whose hits are `Open <path>`
lines after the commands; the top bar's "⋯" button (`commands`, a11y "Commands") opens it too, the phone's way
to every action. The palette remembers where the keyboard was (`window.focused`), puts it
back when it closes and dispatches the choice on the next frame from there, so a terminal's
own actions (find, the prompts) reach the terminal that had the focus.

**Pointing.** ⌘⇧O points the other clients of the focused tile's worker at it
(`ClientMsg::Point`, relayed as `ItemSync::Pointed`): they get a toast naming the pointer and
the tile that goes there on a click and leaves by itself after 8 s; on a phone the palette's
"Point other devices at this tile" does the same (the header's "point" pill went in the
2026-09-25 de-slop pass). Nothing tells a client where another one is looking any more: each
device's layout is its own.

## 5. Agents

Claude Code only, for now, and only through its own TUI: Slopty never draws a GUI of its
own over the agent (decisions, "The agent is used through its TUI; Slopty only reads its
status"), it reads the agent's state and points the human at the terminal that needs them.
The bar's "+ agent" pill and ⌘⇧T (`NewAgent`) open a terminal running `claude` (a bare name,
resolved on the worker through the login shell), which, like ⌘N's shell, starts in the active
terminal's directory when there is one (`WorkspaceView::active_cwd`, the session's OSC 7 cwd as
the worker last reported it), else the worker's default; the palette's "New agent in <dir>" line
does the same for a directory typed in. Four signals, in precedence order: hooks
(delivered to `slopty-worker` over its control socket by `slopty hook`, the relay Claude Code
runs for each event) → JSONL transcript tail → terminal title/OSC → foreground-process
presence. All four are built, and `AgentEvent.source` (`AgentSource::{Process, Title,
Transcript, Hook}`) says which one a status came from, so a `claude` the human started by
hand — or one running before `slopty hook install` — gets the same pill, badges and
attention as a hooked one.

**The three signals below the hooks** are read by `slopty-worker`'s own tick (`agents::watch`,
every 750 ms) and merged by `slopty_agent::Tracker::observe`, which never lets a weaker signal
overwrite what a stronger one said. The tick broadcasts what it found before it reads any file
and drops anything the table has moved past (`AgentTable::is_current`), so a hook arriving
while it works is never overwritten by the older poll. `slopty_worker`'s session actor answers a `Probe` with the
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
only (the transcript path) and never changes the status; it ends the
agent only when a `claude` the worker actually watched in the foreground has been gone for four
probes, which is how a killed agent loses its pill without a `SessionEnd`. A session
attributed without hooks shows an
"install hooks" pill beside its agent pill, once per run: `ClientMsg::InstallHooks` asks the
worker to register the relay (`slopty_agent::hooks`, the same code `slopty hook install` runs)
and `WorkerMsg::HooksInstalled` comes back as a notice, because the human reading the pill may
be on a phone.

The worker spawns every session with `SLOPTY_SESSION=<id>` and `SLOPTY_WORKER_SOCKET=<path>`;
the relay forwards its stdin plus those two to the daemon as `CtlRequest::Hook` and always
exits 0. `slopty-agent` keeps one `Tracker` per session that turns the hook stream into
`AgentStatus` (`Idle`, `Working`, `Tool`, `Blocked{Permission|Question|Elicitation|IdlePrompt}`,
`Done`) and flags `attention` on the transitions worth a sound. A block is a ledger of the
calls waiting on the human (`tool_use_id`s), so a concurrent call finishing beside a pending
permission does not release it; and a hook naming another agent session (a nested `claude -p`
inherits the terminal's session) is dropped while the tracker is busy. An Esc interrupt fires
no hook; the transcript's `[Request interrupted by user]` record takes the agent to `Idle`
instead. `AgentEvent.detail` says what
the agent wants: the tool call awaiting permission, the question it asked, the elicitation's
message, or on `Done` the last line it said (`last_assistant_message` from the `Stop` payload;
when a payload has none of these but names a transcript, the daemon reads the JSONL tail —
`slopty_agent::transcript` — off the blocking pool and fills the detail in). The daemon
broadcasts each change as `WorkerMsg::Agent` and replays the table to joining clients. The workspace shows the status
as a pill in the terminal's title bar and outlines the item when the agent needs the human.
A blocked badge (permission, question or elicitation) is itself the button: a click reveals
and focuses the terminal so the human answers Claude Code's own prompt there; Slopty never
answers for them. Finding them: ⌘⇧A (the "Next Agent Needing You" menu item) reveals and focuses the
next waiting terminal in reading order, cycling from the active item; the top bar shows an
"N need you" pill with the count that does the same on a click or a tap (the phone's way in).
`slopty hook install|uninstall|status` manage the registration in `~/.claude/settings.json`;
`slopty hook report working|blocked|done|idle|gone [message]`, run from inside a session by any
program (a wrapper around another agent), is the same relay with the agent's own word, and
gets the same pill, badge, attention and banner.
On macOS the count is also the Dock badge, an attention event bounces the Dock icon when the
app is not active, and (bundled app only) a notification-centre banner
is posted; clicking it activates the app and reveals the session
(`WorkspaceView::notification_response`).

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
`an_agent_started_without_hooks_is_attributed_from_what_the_worker_can_see`, which puts a fake
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
pinch-zoom, both landing in the same workspace handlers the Mac uses. A keyboard attached to an
iPad or iPhone arrives as `pressesBegan`/`pressesEnded` on the metal view (the first responder
whenever no text input is): every key that is not plain text (arrows, escape, function keys,
enter, tab, backspace, ⌃/⌥/⌘ chords) becomes the same `Keystroke` the Mac backend would build,
so the Mac keymaps and the terminal's key encoder work unchanged; plain characters keep going
through the text system (`insertText:`, IME) while a text input has focus. UIKit does not
repeat presses, so the window repeats a held key itself (400 ms, then every 50 ms, delivered
as `is_held`). A modifier pressed on its own reaches a remote window as its key: the screen
view diffs each `ModifiersChanged` against the last and forwards what moved (fn stays local);
when the view loses focus or the window goes inactive it releases every key and modifier it
had pressed on the worker, so nothing stays down there.
A trackpad or mouse hovers as `MouseMove` and scrolls as `ScrollWheel` with
phases (a `UIPanGestureRecognizer` limited to indirect scrolls, so direct drags stay with the
touch recognizer); while a keyboard is attached (`GCKeyboard.coalescedKeyboard`, polled once a
second with the settings) the key bar hides, since every key on it is under the fingers; `UIApplicationSupportsIndirectInputEvents` is set so clicks are pointer
events rather than synthesised touches. The bundle targets iPhone and iPad
(`TARGETED_DEVICE_FAMILY 1,2`, every iPad orientation, so Split View and Stage Manager can
resize the window; the workspace re-fits on resize like any window);
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
meaning (worker dot, agent badge and outline, "N need you", failed result, failed-command
separator). Geometry comes from `radii` (xs 4 / sm 6 / md 8), the 4/8 pt `spacing` scale
(2 / 4 / 8 / 12 / 16 / 24) and a type scale hanging off `ui_size` (`caption` −3, `small` −1,
`title` +2), so the settings' font size moves every label together; alphas for tints, hover
washes, the scrim and the separators are the `alpha` constants. Hairlines carry the elevation,
shadows are `shadow_sm` on floating layers only (picker, worker switcher, search bar, "↓ latest").
Focus is one accent hairline: the active item's frame and the search bar while
they hold the caret. Pills (title bar, badge, "N need you") are `small()` text on a `TINT`
fill of their tone with the tone as text; buttons are `radii.sm` with `raised` → `overlay`
hover/pressed, and the one primary action on a surface is an accent fill with `accent_fg`.
gpui-kit's widgets (inputs, Markdown `TextView`) read gpui-kit's own theme, which
`slopty_ui::kit::sync` rewrites from the same tokens on every theme change, and the Markdown in
a note gets `markdown_line_height`, paragraph gaps of one base unit, headings
stepping down from `title()` and code in the terminal mono at `small()` on `raised`. Headless
tests read the tokens back through `painted_quads()`: the accent vs hairline item frame, the
`warn` outline of a blocked agent in both variants, the `error` separator tone, and gpui-kit's
colours after a sync. gpui-kit's code editor (`EditorState`) is the file tile's body, in the
terminal mono at the tile's text size, its highlighter the app's own (§4).
One element is not GPUI's: the browser tile's page is a native `WKWebView` over the Metal
view, placed each frame from where the tile's body was drawn and hidden whenever GPUI draws
over the strip (§4). Its chrome (back, reload, the address as text) is GPUI's, from the same
tokens. The page takes its tile's opacity and hides below `browser::MIN_ALPHA`, so a tile
fading in or out leaves no page behind; under Reduce Motion there is no fade to follow.
`slopty-ui::screen::ScreenView` paints a remote window as a `gpui::surface` from the decoder's
`CVPixelBuffer` (zero copy), draws the worker's cursor from the cursor channel, forwards mouse,
scroll and keys (including ⌘ chords the workspace does not bind) as `ScreenInput`, and asks the
worker for a stream scale matching its painted width. The pointer mapping (card point → stream pixel over
the bounds the render recorded, press/release with clicks and modifiers, pixel vs line scroll
with its phase, ⌘⌥-scroll left to the workspace, moves off the picture dropped) is covered headless
by `pointer_and_scroll_reach_the_worker_in_stream_pixels`. When the remote window changes size, the worker notices
within 250 ms, restarts the stream at the new size and sends `Geometry`; the workspace re-aspects
the item. The other way round, letting go of a window card's grip sends `Resize` with the size
the card now stands for (native pixels), the worker sets `AXSize` on the matched window off the
runtime, and the same poll reports what the window took. It is also a text input (`EntityInputHandler`): on iOS a tap raises the soft
keyboard and committed text goes to the worker one key per character through
`ScreenView::press` (press + release, armed ⌃/⌘ from the phone key bar applied); the bar over
a window is esc, tab, ⌃, ⌘, arrows, `/`, copy, paste (⌘C/⌘V on the worker, so the worker's
pasteboard flows back through clipboard sync). ⌘⇧I (View ▸ Stream Stats) overlays every
window with its stream size and scale, fps, Mb/s, link RTT and the FEC/lost/NACK/refresh and
audio counters, re-sampled once a second from `ScreenStats`.

**Terminal element.** `slopty-ui::terminal` draws the cached lines as one element (each
row's words shaped once at the base size and cached by content hash, their glyphs painted one
by one at the zoomed size through `Window::paint_glyph` from the fork's `ShapedLine::layout`,
so a zoom step shapes nothing; background quads, cursor, selection, underlines — the curly one
as GPUI's wave — and strikethroughs at the offsets §2's metrics derive, ⌘-hover link
underline) with a hairline over every prompt-start row but one with only blank lines above it
since the first line, and on the viewport's top row only when the command failed (a neutral
rule there would double the tile header's hairline): the
command-block separator, the foreground at `alpha::FAINT` (12 %), or the theme's `surfaces.error` token
at `alpha::STRONG` (70 %) when the row's `Prompt { exit }` is non-zero
(`separator_color` in `crates/slopty-ui/src/terminal/element.rs`; `crates/slopty-theme/src/lib.rs`), and at the right end of a
prompt row whose command took a second or more, its duration (`took_label`, fg at
`alpha::TINT`; `TerminalView::took` by prompt row from `Effect::CommandFinished`,
emptied with the epoch; the sticky block header repeats it at its right end). Terminal-context bindings: ⌘C /
⌘V copy and paste, ⌘A selects every line the worker keeps and fetches the history the cache
lacks so the ⌘C after it has it all, ⌘F / ⌘G / ⌘⇧G search, ⇧⇞ / ⇧⇟ page through history and
⇧⇱ / ⌘⇱ / ⇧⇲ / ⌘⇲ go to the oldest line / back to the output (ghostty's keys plus
Terminal.app's ⌘ pair), ⌘↑ / ⌘↓ scroll the previous / next prompt start to
the top of the viewport (`TermState::prompt_before/after` over the cached lines, uncached
history is not fetched first; ⌘↓ past the newest prompt goes back to following output),
⌘K clears the screen and the history (`TermRequest::Clear`, protocol 31: the worker writes
`CSI 3 J` through its engine and output tap, as if the program had, then ⌃L to the shell so
the prompt repaints at the top; the view drops its scroll offset),
⌘⇧↩ runs the last finished command again (`TermState::last_command`, the block before the
newest prompt, typed as the block menu's rerun types it; also "Rerun last command" in the
palette), ⌘⇧C copies the last finished command's output (`TermState::last_command_output`: the
`Output` rows right above the newest prompt start, blank tail trimmed; nothing without shell
integration). A right click on any row opens the context menu (`block-menu`; on a block row
the block's items come first, protocol 28:
the marks carry the input column, `TermState::command_block` reads the command and the
output from any row): "Copy command", "Copy output", "Rerun" (a paste of the command, then
↩ as a key), "Save as note" (the
block as a note card beside the shell: the command as a heading and a runnable `sh` fence,
the output as a plain fence, `block_note`; `TerminalViewEvent::NoteBlock` →
`WorkspaceView::note_beside`, a free slot when the space beside is taken; the palette's "Keep
last block as a card" does the same for the block before the newest prompt,
`TermState::last_block`) and "Select block"; then, on every row, the terminal's own: "Copy"
(only with a selection; off a block a selection also offers "Save as note", fenced), "Paste", "Find…" and "Clear screen", each what its shortcut does. ⇧-arrows move
a selection's head (`adjusted_head`: a cell sideways wrapping at the row's ends, a row up or
down), the shell never sees them; without a selection they are the shell's. The
menu is `Menu "Command block"` on a block, `Menu "Terminal"` elsewhere; the armed tap on the
phone opens the same. While the viewport's top row is inside a block whose prompt
rows have all scrolled above, a one-row header over the grid (`block-header`, role Button,
`TerminalView::block_header`, reading `TermState::block_head` — the prompt's rows alone, never
the output) names the command in the mono face on the panel colour, ruled
under with the block's separator colour; a click on it puts the prompt back at the top.
`TermState` also follows the blocks frame by frame (`track_command`, the prompt's rows
alone): a command is running once the cursor has left the rows it was typed on
(`Effect::CommandStarted`) and finished when a newer prompt starts, whose `exit` is its
status (`Effect::CommandFinished`); the view times the two and emits
`TerminalViewEvent::CommandFinished { command, exit, elapsed }`, and the workspace badges the
item's title bar ("done 12.3 s", "failed (1) 1 m 04 s" — `took_label`'s clock — in the success or warn tone,
`finished-<uuid>`, role Button) when the command ran at least `SLOW_COMMAND` (5 s) and its
item was not the active one — the shell's answer to the agent attention badge. The badge
goes when the item is activated (a press on it does that).
Headless `#[gpui::test]`s in `terminal/view.rs` read the
separators back from `painted_quads()` and drive the bindings with `simulate_keystrokes`.

**Render path and frame time.** One GPUI frame draws the workspace (`WorkspaceView::render`):
the titlebar, then the strip from the layout's `Frame`, laying out only the tiles that are
`near` the view (the focused one and any dragged one always), each a header and a cached view
(`Entity::cached`, so a tile repaints only when its own view is notified), then the overlays
and the `frames::probe()` element last. The terminal element's prepaint resolves the monospace family
once per app (the `ShapeCache` global memoises the theme's list; listing the installed fonts
is a synchronous trip to the font server), reads the view's rows in place — only the rows
inside the window's content mask, so a grid hanging off the viewport builds nothing for the
rest — splits each into words (plain spaces and digits are the boundaries) and looks every word up in
the `ShapeCache` global — an `Rc<Word>` (the `ShapedLine` at the base size plus its per-byte
colours) per FxHash of (text, styles, family, palette, cell width, and the blink phase for a
word with an SGR 5 cell), never per zoom or focus. Nothing is swept per frame: each prepaint
stamps the words it uses, and past a budget of 2¹⁸ glyphs the quarter stamped longest ago
goes, so a pan or a scroll back finds what it showed a moment ago still shaped; a word is shaped with no forced width and
each glyph placed at the column of the cell its byte came from, so a wide cluster spans two
cells however many glyphs it shaped to — so
a frame of streaming output shapes only the words it has never seen and a zoom step shapes
nothing; paint puts every glyph of a word at column × cell width plus its shaped position
scaled by the zoom, on the derived baseline, through `Window::paint_glyph` at the zoomed font
size (the fork's `ShapedLine::layout` hands out the runs), over the row's background quads and
under the cursor and the link underline. The predictor's guesses are cells of their row
(faint, underlined) shaped and painted as the worker's text is, and a block cursor's cell has
its text drawn over it in the theme's `cursor_text`. The cursor's blink flag and
SGR 5 text share one 600 ms clock on the view, started by the first prepared frame that holds
either and stopped by the first that holds neither (an unfocused cursor is a steady hollow
block); a keystroke pins the phase on for a half. While the overview's zoom is **in
motion** the workspace tells every terminal view and chrome label so, and they paint from the nearest rung of
an eight-per-octave raster ladder stretched to the painted size (`fonts::raster_rung`, the
fork's `Window::paint_glyph_scaled`) instead of rasterising every glyph at each intermediate
size; the settled frame paints exact. The item chrome's text — title, pills, badge, block
headings — is `slopty_ui::chrome_text::ChromeText`: shaped once at its base size (a per-app
cache), sized by arithmetic rather than a taffy measure callback, painted glyph by glyph at
`base × k` with GPUI's baseline and advance arithmetic, ellipsis included. Worker events reach the workspace from the
link loop in batches (whatever queued while the last one applied, up to 256, in one update),
so twenty streaming sessions cost one update per batch; a session itself never sends more than 125 frames a second
(`MIN_FRAME_INTERVAL`). `slopty_ui::frames` times every draw (`begin` in `Workspace::render`,
`end` in the probe) into a 1024-frame ring with nearest-rank percentiles and a count of the
frame slots long draws swallowed; it is the fourth line of the ⌘⇧I overlay and the `frames`
block of the self-test `dump`, and `terminal::latency` stamps each keystroke so `dump` can say
how long the local echo and the worker's echo took to reach the display: a paint that holds a
waiting key hands `slopty_ui::shown::after_paint` the work, and the key is timed when the
window's presentation report (`Window::on_frame_presented`) says the first frame submitted
after that paint reached the glass. Presentation is vsync-synced (the fork's `CAMetalLayer`
keeps `displaySyncEnabled` at its default) and the compositor adds about a refresh, so a
paint's own clock, or the next display tick, reads a refresh or more early. The video pacer
stops its arrival → present clock the same way. The iOS simulator reports no presentation;
there the next display tick stands in. `cargo xtask e2e smooth`
runs the load scenarios (MEASUREMENTS, "canvas frame time", measured before the workspace).

**Settings.** `<data dir>/settings.toml` (`slopty settings path|init`; the "Settings…" menu
item, ⌘, and the palette's "Open settings" open it in the in-app editor, `SettingsEditor`
in slopty-ui: a dialog with the text in a monospace field, ⌘↩ parses then writes and
applies it, a parse error stays under the field; the Mac's dialog also offers the default
`.toml` editor, writing the commented defaults first when the file is missing).
`slopty-settings` owns the schema: `[font] mono_family | mono_size | mono_line_height |
ligatures | ui_size` (the line height is `Typography::mono_line_height`, ghostty's
`adjust-cell-height`; ligatures toggle `calt`), `[theme] appearance = dark | light | system`,
`[terminal] minimum_contrast | copy_on_select | bell_alert | cursor_blink = program | always |
never | cursor_style = program | block | bar | underline | paste_protection | bold_is_bright |
hide_pointer_while_typing | scroll_multiplier | option_as_alt = false | true | left | right |
confirm_close | natural_editing` (the ratio and bold-is-bright ride on `TerminalPalette`,
copy-on-select, the blink override, paste protection, the pointer hide, the multiplier,
option-as-alt, the close confirmation and the natural editing keys on `Theme::behaviour`,
the last travelling on every `KeyEvent` to the worker's encoder, the bell flag is
read by the app's bell handler, see decisions/terminal.md), `[remote] fps | max_bitrate_mbps | hdr | muted` (`Theme::behaviour.stream`;
a live stream re-asks its quality on change and takes a changed `muted`, see
decisions/video.md and decisions/settings.md), `[colors] foreground |
background | cursor | cursor_text | selection | ansi` (`"#rrggbb"` strings laid over
`TerminalPalette` in both appearances, see decisions/settings.md); every key has a
default, unknown keys warn, a
file that does not parse is skipped with the error in the top bar for a few seconds. The app
polls the file's stamp once a second and on a change rebuilds the `Theme` (variant from
`appearance`, `system` following `window.appearance()` through `observe_window_appearance`)
and pushes it down `WorkspaceView::set_theme` → every terminal, window and the picker; the
terminal element re-measures its cell grid from the new size on the next frame and `fitted`
resizes the session. iOS reads the same path (in its sandbox); the in-app editor is its
only way to change it.

**Workers.** The app holds every added worker at once: one `WorkerLink` (on the process's one
client endpoint, with its own reconnect loop and silence check) per worker
(`slopty_app::workers`), all feeding the one `WorkspaceView` under the worker's `WorkerKey`
(its `WorkerId`'s 128 bits). A dropped link keeps the worker's tiles ("reconnecting"); the
next connection's snapshot reconciles them. The titlebar names only the workers that are down;
the "…" menu adds a worker and forgets one. The "N need you" pill and the Dock badge count
agents across every worker, and a tap goes to the next one wherever it is. A banner names only
a session, which the workspace finds on whichever worker runs it. The known workers are
`workers.json` in the client's data dir (`slopty_net::known`): a list of `{ address, name,
worker_id }` keyed by the id, so a worker that moves keeps its row and its tiles; a dial that
reaches another id at a stored address says so instead of mixing tiles.

**Adding a worker.** An installation with no worker and no server opens on the first-run
page, which is the whole window: "Connect to a server", one line on what that is, the address
field, "Connect", and "Add a worker by address instead" as a quiet link (no titlebar, no
workspace behind it). Later, "Connect to a server" and "Add a worker" (⌘⇧H) show the same panel
as a dialog over the workspace with a Cancel. The phone adds "Paste", since it has no ⌘V.
`slopty_app::net::add_worker` connects, says `Hello`, and stores the worker under the id its
`HelloAck` carries, the same as `slopty add` on the CLI.

## 7. Crate map

| Crate | Role | Platform |
|---|---|---|
| `slopty-core` | ids, clocks, errors, small shared types | all |
| `slopty-proto` | wire messages, versioning, codec (postcard) | all |
| `slopty-grid` | terminal frame model, row diff, line cache | all |
| `slopty-engine` | libghostty-vt engine: frames, scrollback, input encoders | worker |
| `slopty-pty` | openpty/spawn/resize, ptyd protocol | worker |
| `slopty-predict` | speculative local echo | client |
| `slopty-net` | plaintext QUIC endpoint (noq + null crypto provider), `host[:port]` addresses, admission by source address, known workers, framed channels | all |
| `slopty-media` | packetizer, FEC, reassembly, NACK/refresh policy, redundancy | all |
| `slopty-capture` | the capture seam (`CaptureSource`); ScreenCaptureKit on macOS | worker |
| `slopty-codec` | the encoder seam (`VideoEncoder`, `AudioEncoder`); VideoToolbox encode (worker) / decode (all) | split |
| `slopty-input` | the input and clipboard seams (`InputSink`, `Board`); CGEvent injection for remote-window input (keymap, pointer/scroll/keys, owner activation, a thread per stream, held input let go at the end) and `NSPasteboard` on macOS | worker |
| `slopty-agent` | Claude Code hook payloads → per-session `AgentStatus` | worker |
| `slopty-worker` | session manager, mux, fan-out, the orchestration verbs, worker capabilities, listening ports | worker |
| `slopty-client` | client session state, the item registry mirror, the layout model | client |
| `slopty-settings` | `settings.toml` schema, defaults, loading with fallback, data dir | client |
| `slopty-theme` | design tokens, dark and light variants | client |
| `slopty-ui` | GPUI elements and views; headless `#[gpui::test]` tests drive them through `VisualTestContext` | client |
| `slopty-platform` | process-level platform helpers: keep the process out of App Nap and timer coalescing while a session is live, and raise the user's attention | all |
| `slopty-app` | the app shell shared by macOS and iOS: workspace window, add-worker panel, one link loop per worker, settings | client |
| `slopty-e2e` | app self-test: control-socket wire types, tokio driver, daemon+app harness (the app on the Mac or in the iOS simulator), numeric golden diff, frame-time scenarios (`cargo xtask e2e app\|ios\|smooth\|smooth-ios`), and `slopty-idle-window`, a window the harness owns so a capture target that never draws can be tested without touching anything else on the desktop | dev |
| `slopty-shape` | a UDP relay the client and worker speak QUIC through, with a delay/jitter/loss/rate model below the congestion controller; the degraded link the congestion rulings are measured on | dev |
| `apps/slopty-ptyd` | PTY custodian daemon (LaunchAgent) | worker |
| `apps/slopty-worker` | worker daemon | worker |
| `apps/slopty` | macOS app: logging, runtime, window options, then `slopty_app::open_workspace` | client |
| `apps/slopty-ios` | iOS static library (`slopty_ios_run` called from a UIKit shim); `cargo xtask ios sim [--sim iphone\|ipad]\|device` generates the Xcode project | client |
| `apps/slopty-cli` | `slopty` CLI: worker ctl, `add`/`workers`/`forget`, raw-mode reference client (`open`/`attach`), hook relay | worker |
| `xtask` | all scripts (build, gates, bundle, sign, icon from `assets/icon.svg`, `e2e app|ios|worker|screen|input|all` for the self-test and the gated live tests) | dev |

Dependency direction is strictly downward in that table; `slopty-ui` never sees `slopty-worker`.
