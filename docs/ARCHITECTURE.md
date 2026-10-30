# Slopty architecture

Slopty is a remote-coding workstation in three roles. **Workers** (macOS; Linux for terminals
and agents) expose shells, agents, windows and displays. One **server** keeps the worker
directory and the orchestration verbs, and is never on the data path. **Clients** (the macOS,
iPhone and iPad app, and the `slopty` CLI that people and AI agents run) show every worker's
agents, terminals, files and screens in one interface, several workers mixed: a terminal's
window (`slopty-shell`, `docs/decisions/layout.md`), fed by Slopty's view model. TUI-first: every agent
(Claude Code, Codex, pi) runs as its own TUI in a worker terminal; a Claude Code session is also
one read-only thread in the agent-neutral model, read from its hooks, its live registry and its
transcript, which the inspector shows; the GUI types nothing into an agent
(`docs/decisions/agents.md`, "TUI-first"). Everything is Rust. Floor: macOS 27.0 / iOS and iPadOS 27.0, Apple silicon.

This file is the map. Rulings and their evidence live under [docs/decisions/](DECISIONS.md), one file per topic.

## 1. Shape

```
                                  ┌──────────── server (slopty-server) ────────────┐
                                  │ worker directory · leases · orchestration verbs │
                                  │ QUIC front end (agents' MCP via `slopty mcp`)   │
                                  └───────▲───────────────────────────────▲─────────┘
                        workers dial in: │ lease + verbs     clients: directory, verbs
                     ┌───────────────────┼──── worker (macOS · Linux) ────┼─────────────────┐
                     │  slopty-ptyd      │     slopty-worker              │                 │
                     │  ┌────────────┐  fd     ┌───────────────────────────────────────┐    │
   shells/agents ◀──▶│  │ PTY keeper │ ──────▶ │ sessions · VT engine (libghostty-vt)  │    │
                     │  │ (tiny,     │ SCM_    │ frame diff · replay · fan-out          │    │
                     │  │  outlives  │ RIGHTS  │ agent threads (observed, read-only)    │    │
                     │  │  worker)   │         │ screen: SCK → VT HEVC → FEC → datagrams│    │
                     │  └────────────┘         │ input: CGEvent injection               │    │
                     │                         └──────────────┬────────────────────────┘    │
                     └────────────────────────────────────────┼─────────────────────────────┘
                                                              │ QUIC, plaintext, direct: streams = control/terminal/files
                                                              │                          datagrams = video/audio/cursor
                     ┌────────────────────────────────────────┼─────────────────────────────┐
                     │ client (macOS app · iOS app · CLI)     ▼                              │
                     │ slopty-client: session state · line cache · prediction · items · layout│
                     │ slopty-ui (GPUI): workspace · view model · surface element · the views│
                     │ slopty-shell (GPUI): the interface, a terminal's window               │
                     │ slopty-terminal: the terminal view · its element · find · copy mode   │
                     │ slopty-codec (decode) → CVPixelBuffer → gpui surface (zero copy)      │
                     └───────────────────────────────────────────────────────────────────────┘
```

Three kinds of traffic, one QUIC connection per (client, worker). The connection is QUIC over
plain UDP (n0's `noq`, standalone) with a null crypto provider: every worker is reached over
Tailscale, WireGuard or a VPN, which encrypts and authenticates already, so Slopty adds no
TLS, no endpoint keys, no relays and no pairing. A worker is named by `host[:port]` (a
`MagicDNS` name, a LAN name or an IP; port 45550 by default) and identified by the `WorkerId`
in its `HelloAck`, which also names the daemon's home directory (a client writes paths under it
as `~`) and carries its `WorkerCaps`; a later change to those comes as `WorkerMsg::Caps`. It admits a connection once, before any handshake state exists: loopback and the
`[network] allow` ranges of its `settings.toml` by address, and a tailnet address
(`100.64.0.0/10`, `fd7a:115c:a1e0::/48`) only as the machine's Tailscale vouches for it through
`whois` and the tailnet's grants (`slopty_net::admission`, `slopty-tailnet`). The server
admits the same way, with the same `[network] allow` ranges.

| Path | QUIC primitive | Payload |
|---|---|---|
| Control | one bidirectional stream, length-prefixed `postcard` messages | hello, open/close, resize, item registry sync, agent events |
| Terminal | one unidirectional stream per session, worker→client; input on the control stream | grid **row diffs** from the worker-side VT engine, scrollback line pages on demand |
| Media | unreliable datagrams (RFC 9221) | HEVC fragments + Reed–Solomon parity, Opus audio, cursor position/shape; client → worker: `Feedback` (NACK, refresh), each datagram standing alone so a lost one never holds the next back |
| Echo copies | one datagram per keystroke and per echo, 2 ms after its stream copy | client → worker: every input request again (`ClientDatagram::Input`, numbered per session in the order the control stream carries them); worker → client: every diff built while input is being answered, when it fits one datagram (`TermDatagram`) |

Every datagram, in both directions, starts with one `datagram::Channel` byte: media, terminal
echo, feedback, input or window input. For media it is the first byte of the `MediaHeader`.

Inside a packet, the control and session streams go first, then media datagrams, then tunnels and
files (`slopty_net::streams::AHEAD_OF_DATAGRAMS`, on a noq patched with
`stream_priority_before_datagrams`). Stock noq writes every datagram first, and an echo waited
behind a keyframe (MEASUREMENTS.md, "an echo ahead of the datagrams").

A keystroke and its echo are one small packet each, and when one is lost nothing behind it
tells QUIC, so it waits for the probe timeout. Each therefore also goes once as a datagram,
2 ms after its stream copy so the two never share a packet (`slopty_net::echo`,
`SLOPTY_ECHO_COPY=off|<ms>|<ms>,<ms>`). Each end takes whichever copy comes first, and only in
order: the worker applies a key's copy only when it is the session's next input, and skips the
stream copy of an input already applied (`conn::InputOrder`); the client's session pump passes
a frame's copy on only when it follows the last frame passed on, and drops the stream copy
after it (`link::FrameOrder`), so an early copy never opens a gap that would ask for every row
again. A copy goes only onto an empty datagram queue: behind queued video it would arrive after
the stream's own retransmission, and noq makes room by dropping the oldest datagram, video's.
The copies pay off only because the vendored noq's pacer holds BBR3's send quantum
(`vendor/noq-proto/SLOPTY.md`, patch 4): at the rates BBR3 paces a lossy, app-limited
connection, a second small packet otherwise waited 15 to 30 ms (MEASUREMENTS.md, "datagram
copies of a keystroke and its echo").

## 2. Terminal: the VT engine lives on the worker

The worker runs `libghostty-vt` against the real PTY and ships **rendered rows**, not bytes
(mosh / zellij / wezterm-mux model). Consequences:

- Reconnect and multi-client are cheap: a joining client receives the current screen plus a scrollback window,
  nothing is replayed. Two clients share one worker and its items at once, each arranging them in its own layout. A terminal opened on one
  is on the other within a round trip; both type into the same session and the actor serialises
  their keys in arrival order (nothing lost or reordered); one client is the size **driver** (§2,
  the "take" pill) while the rest are viewers; an agent's attention badges every client; a client
  that dies leaves the others streaming and reattaches on relaunch (its dead connection's cleanup
  detaches only its own sinks, `Sender::same_channel`). A sink that closes keeps the driver's
  seat until its connection detaches that sink, so a re-attach never hands the size to another
  viewer, and a frame from a replaced stream that arrives after the new stream's is dropped by
  its sequence number (`TermState::superseded`). See DECISIONS "Multi-client" for the
  state-ownership and input-contention rulings.
- A slow link never falls behind a fast program: each viewer has at most two frames in flight
  (a credit on `session::Outbound` that returns when the connection has written the frame), the
  actor builds the next diff only when a viewer has room, the engine coalescing meanwhile, and a
  viewer that missed a diff is sent every row at the others' sequence number. Other events
  (lines, matches, title, bell) are never skipped. A joining viewer's frame is built the same
  way, so nobody else is made to resync. With nobody following the diffs, the whole frame is a
  baseline (`GhosttyEngine::baseline_frame`) that the next diff is taken against, so the first
  key after an attach carries only the rows it changed. Input buys up to two frames outside the 8 ms pace for
  50 ms, so an echo beside a flood is not held to it. A written frame can still wait in QUIC's
  stream buffer, so the worker follows every 16 KiB of frames with a `TermEvent::Marker` that the
  client answers with `TermRequest::Reached` once it has applied everything before it; a viewer
  that answers has at most `FRAMES_UNREACHED_BYTES` (64 KiB) of frames unconfirmed, a quarter of
  a second at 250 kB/s where the 1.25 MB stream window was five. A tool that reads the stream
  raw and never answers is sent frames as before.
- A scrolling screen is not sent again. libghostty rebuilds every row when output moves the
  viewport, and the engine compares each with the line the viewers already hold at that
  absolute index (`ghostty::Shown`), so a frame carries the rows that changed and the lines that
  came in; the client moves the rest up from its line cache (`TermState::readopt`). A row goes
  on the wire up to its last non-blank cell. An Enter at a bottom prompt at 200 × 60 is 0.7 kB,
  not 88.
- The client needs no VT engine at all. iOS never builds Zig.
- The client keeps a **line cache** (absolute line numbers) so scrollback scrolls locally; missing
  ranges are fetched, prefetched around the viewport; the cache indexes its prompt rows, so a
  block's prompt is a range query, not a walk. Mouse selection is client-side too
  (absolute line indices, ⌘C copies from the cache, ⌘V sends `Paste`, which the worker
  sends back as `PasteHeld` for a confirmation strip when it holds a newline and the program,
  as it is when the paste arrives, has not asked for bracketed paste (`Engine::paste_is_safe`);
  drag, ⌥-drag for a
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
(`src/main.rs:12:5`, `url::path_at_col`) opens the same way: ⌘-click opens a file tile for it
at its line (`TerminalViewEvent::ViewFile`, the workspace making the path absolute against the
shell's directory, `WorkspaceView::absolute_in_session`). Nothing is typed at the prompt: every
session's `$EDITOR` is Slopty's own, which would open the same tile and hold the shell until it
closed. A file too large for a tile offers a terminal editor instead, passing over Slopty's own
(`url::editor_command`).
- **Prediction**: the client applies mosh-style speculative echo for printable keys, confidence
  gated on measured RTT, reconciled against the next authoritative diff (see `slopty-predict`).
- Terminal size is owned by one **driver** client (the one that opened the session, else the
  first to attach; anyone can take it with the "take" pill, which also fits the item to their
  viewport); viewers see the driver's grid.

**Absolute line numbering.** Clients cache scrollback by `LineIndex` (absolute, monotonic). The
engine keeps a libghostty *tracked grid ref* pinned to the newest active row and re-derives the
index of screen row 0 after every write, so worker-side eviction never shifts indices. When
numbering cannot survive — reflow on resize, RIS, a single write that scrolls past the whole
scrollback — the frame carries a new `epoch` and clients drop their cache. The alternate screen
has an epoch of its own: the engine parks the primary's anchor with its epoch, prompt marks and
exit statuses, and the client puts its cache aside. Returning restores the primary's epoch, and
the client takes its cache back, so the history, the prompt marks and the command blocks survive
`vim`/`less` round trips. An epoch is never handed out twice.

**Search** is a worker request (`TermRequest::Search` → `TermEvent::Matches`): the engine renders
the retained rows as plain text with libghostty's formatter and maps hits back to cells, so the
whole 50k-line history is searchable without the client ever holding it. A history row is
formatted once and kept; a later search formats only the rows written since, plus the screen,
and starts over after a reflow or on the alternate screen. `regex: true` runs the
needle through the `regex` crate instead of the literal matcher; a bad pattern comes back as
`TermEvent::SearchInvalid`.

**Escapes programs rely on.** OSC 8 links ride on the rows: the engine asks libghostty for the
URI of every linked cell (`ghostty_grid_ref_hyperlink_uri`, only on rows whose page flag says
they may hold one) and folds them into `Line::links`, a list of `Hyperlink { col, len, uri }`
runs, so a link-free row costs one byte and no cell carries an id. The client draws the run
under the pointer underlined while ⌘ is held (the pointer a hand, an I-beam elsewhere, the
arrow while a program reports the mouse), names the target in a chip at the tile's corner
(`TerminalView::link_target`) and opens the URI on ⌘-click; the OSC 8 target
wins over the plain-text URL scan (`slopty_terminal::url`). OSC 52 (and iTerm2 OSC 1337
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
alike, and the shaped-word cache keys on it. OSC 9, OSC 777 `notify` and OSC 99 desktop notifications (libghostty delivers
all three through one callback; OSC 99's chunks are put together in the ghostty fork) become `TermEvent::Notification { title, body }`, each field capped at 512 chars;
the workspace posts them as a notification-centre banner when no window is active, tagged by the
session so a click reveals the tile, and bounces the Dock like an agent's attention. BEL tints
the tile's grid for a flash (`TerminalView::bell_flashing`, `alpha::FAINT`) and, when no window
is active and `[terminal] alert` is "hidden" (the default), or always when it is "always",
plays the alert sound and bounces the Dock.

**Kitty graphics.** libghostty keeps the images (`KITTY_STORAGE_BYTES` per screen; PNG
decoded through the `png` crate by `slopty_engine::graphics::PngDecoder`) and lays the
placements out at the client's cell pixels. An image comes in the program's output, or as a
file, a temporary file or shared memory on the worker (`graphics::allow_media`). Every `Frame` lists the placements on the
viewport (`Frame.images: Vec<Placement>` — cell, offsets, painted size, source rectangle,
z), and a placement's pixels travel once as `TermEvent::Image` (premultiplied BGRA,
the texture's format, made on the session actor; sampled down by a whole factor when over
`IMAGE_WIRE_BYTES`) ahead of the first frame that places them, and
again after a full frame (attach, resync). Worker and client keep the same bounded cache
(`IMAGE_CACHE_BYTES`, least recently placed first: `graphics::Ledger` and
`TermState::keep_image`), so the worker knows what to re-send. A placement change with no cell
change still makes a frame (the storage generation is part of the dirty check). The view
makes one GPUI texture per image generation (`TerminalView::placed_images`, a copy of
the bytes as sent) and the element paints the source rectangle into the placement's cells
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
(`slopty_terminal::metrics`, ported from `vendor/ghostty/src/font/Metrics.zig`): a pure
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
tapped, and after a resize (the size goes to ptyd first, on the same queue, which answers neither a
resize nor a checkpoint, so the worker never waits on it). `Attach` returns
the checkpoint and the ring, and the new worker's engine replays them in
that order, so the screen, the scrollback, the directory and the program's colour changes (the
checkpoint carries them as the OSC sequences that made them, never a palette dump) survive a
worker restart or crash; what the replay reports is kept for the first attach, not broadcast or
answered. A bare
program name the daemon cannot find on its own `PATH` runs through the user's login shell,
interactive (`$SHELL -lic '…'`), so rc-file `PATH`s and aliases apply.
An install whose ptyd keeps custody another way (its fingerprint from the custody goldens
moved) stops the worker, then hands ptyd over to the new build (`slopty-ptyd --succeed`,
`PtydRequest::Succeed`): the old image parks every reader, writes a `Bequest` and each session's
`Heir` to an unlinked file, clears close-on-exec on it and every master, and `execv`s the new
binary with `--inherit <fd>` in one synchronous step (`slopty_pty::succession`). The new image
takes the descriptors back and binds the socket afresh. The children never notice. The
handover's own wire, command line included, is fingerprinted from its goldens too, and a build
whose fingerprint differs is restarted instead, ending the sessions. The worker redials a ptyd
it lost (20 ms doubling to 2 s), checks the custody file names the peer's pid, and takes back
every session it held: `Reclaim` where ptyd still lists it, else `Adopt` with a dup of the
master it kept, whose child ptyd then watches by pid and the kernel's start mark.

**After a reboot.** ptyd's end takes the shells with it, so the worker keeps each session on
disk itself (`slopty_worker::restore::Keeper`: a recipe and the newest checkpoint under
`<data dir>/sessions/`). A worker that finds a recipe ptyd no longer holds reopens the session
under its old id: a new shell in the old directory, under the old screen and a divider, and
nothing it ran started again. An agent's thread is left exited and resumable: a
start with `--resume` takes it up in a terminal again.

**The shell's environment.** Every child gets `TERM`, `COLORTERM=truecolor`, `TERM_PROGRAM`,
`TERM_PROGRAM_VERSION` and the shell integration's `ZDOTDIR`, on top of ptyd's own. `TERM` is
`xterm-ghostty` once ghostty's terminfo is compiled, else `xterm-256color`. ptyd compiles it
itself on start-up — `slopty_pty::terminfo` holds ghostty's entry as const data, renders the
source, and runs `/usr/bin/tic -x -o $HOME/.terminfo -` in a background task — so a fresh
machine gets the full capability set without anyone installing ghostty. The install is
idempotent, nothing waits for it (`TERM` is read per spawn), and `$SLOPTY_TERMINFO_DIR`
redirects the database for tests.

**A shell's browser and editor.** The shell integration also links the `slopty` CLI as `open`
(`xdg-open` on Linux), `slopty-browser` and `slopty-editor`, in a directory whose path has no
space in it. It puts that directory first on `PATH`, again at every prompt, and sets `BROWSER`
and `EDITOR` to the last two by absolute path, as defaults the user's rc overrides. `VISUAL`
is never set. A web page or a file a program hands them goes up the control socket
(`CtlRequest::Open`, `CtlRequest::Edit`) to the daemon's `handoff` module. The daemon offers it
as `WorkerMsg::Handoff`, one after another, to the clients that declared they take it
(`ClientMsg::HandoffCaps`), starting with the client focused on the session. A page opens
without asking only on the client that just typed into the session. Otherwise, and always for
a local or deceptive address, it is offered in a notice. The client answers
`ClientMsg::Handoff`, and `slopty_client::handoff` decides what to do. The link hands each one
on as `LinkEvent::Handoff` with the `Instant` it read it, so whether a page is too late to open
is measured on the client's clock alone. An editor waits on the
socket until the person is done with the file tile, whose saves then go into the file in
place. With no client that takes it, the program falls back at once to this machine's opener
or `vi` (decisions, "A shell's browser and editor are the client's"). In the app,
`slopty-ui::workspace::handoffs` declares the caps first on every link, keeps one
`Handoffs` per worker across its links, opens a page (behind, while a remote window or display
is in front) or holds it in a notice by its host, opens an edit as a file tile beside its
shell (`FileView::set_waiting`, answered by `FileViewEvent::Edited`; a tile that goes any way
but a landed save answers `Cancelled`), and reports the shell in
front of the person to its worker as `TermRequest::Focus` on every change of the workspace.

**Deployment.** `slopty worker install` writes two LaunchAgents (`dev.aislopware.slopty.ptyd`,
`dev.aislopware.slopty.worker`; `KeepAlive`, `RunAtLoad`, `ProcessType Interactive`) with the
sockets under `<data dir>/run/` and logs in `~/Library/Logs/Slopty`, bootstraps them, and
waits for the daemon to answer; `uninstall` and `service` undo and report. The CLI finds the
installed socket by itself, so `slopty worker status` works without launchd's environment.
`slopty worker doctor` asks the running daemon about itself (`CtlRequest::Doctor` →
`Health`): the worker's id and how its link to the server stands (address, then dialling,
linked, redialling or refused, with why), Screen Recording and Accessibility as *that binary*
sees them (TCC grants are per executable, so the report names the path to add), listen
address, admitted ranges, connected clients, sessions;
exit status 1 while a permission is missing, so it can gate a setup script. For an ad-hoc
(`cargo build`) binary "per executable" means per build: the cdhash changes and the grant is gone,
silently. `cargo xtask sign` (and `xtask run worker`, which calls it) signs both daemons under
`dev.aislopware.slopty.worker` and `….ptyd` with a Developer ID certificate, which makes the
requirement the identifier rather than the hash and one approval enough for good.

**Worker link and orchestration.** With a server configured (`--server`, `SLOPTY_SERVER` or
`[network] server`, the one the app on that Mac reads too), the worker registers with it (`apps/slopty-worker/src/server.rs`). It sends
its capabilities (`slopty-worker::caps`), where its listener is bound (the server lists it at
that address, at the loopback address it dialed from when bound to loopback, or at the machine's
tailnet address when it listens everywhere and dialed over loopback) and its sessions, forwards
session and agent events and, as a message of its own, its load average,
and redials with capped backoff when the link drops. Every `SessionSummary` carries the agent
running in the session, its status and the signal that status came from (`SessionAgent`), read
from the daemon's agent table (`Worker::summaries`), and the working tree's changes against
`HEAD` when the session is in a repository (`slopty-worker::changes`, counted by git on a task
of its own, at most one run per repository every 2 s),
and the server's hub keeps each listed terminal's agent current from the `Agent` events, so
`ListTerminals` (and `slopty workers`, which counts the agents waiting on a human from it)
answers without asking each worker. `slopty-worker::orchestrate::Orchestrator`
answers the verbs the server forwards. It opens terminals through the same path a client's
`OpenSession` takes, types through the session's key and paste encoders (never into a terminal a
coding agent runs in, `orchestrate::may_type`, but the one message `agent_tell` submits), and reads text views
of the engine on the session's thread (`GhosttyEngine::screen_text`, `text_lines`,
`text_since`, `commands`). `WaitFor` sleeps on the session's `Activity` watch channel and
matches output from a per-session mark, as `expect` does. `slopty-worker::ports` finds the TCP
listeners in each terminal's process tree through libproc. The server's hub answers two verbs
itself: `Events`, a long poll on a bounded log of what it hears from every worker (liveness,
terminals opened and closed, agent status changes) read from a cursor, which is how one agent
watches the whole fleet (`slopty events`); and `ForgetWorker`. Each event it logs is pushed
as it is, `FromServer::Event(HubEvent)`, to every client and agent link. A link gets the state
(`Directory`, then the attention `Ladder`, then where the person is) on connect and again when it falls behind, and replaces what it
showed with it. Files are read in ranges of at
most 8 MiB with the whole size reported, directories listed with `ListDir`, paths checked with
`Stat`; a terminal opened by a verb takes a size, and `ResizeTerminal` resizes one no client
shows. A verb that changes something may carry an idempotency key on its request; the worker
keeps each key's outcome for ten minutes (`orchestrate::idempotency::Ledger`), so a caller that
lost its answer to a timeout or a dropped link asks again and gets the first outcome, not a second
terminal. The CLI and MCP give every mutating verb a key and resend under it on `Interrupted`.
Rulings in `docs/decisions/topology.md`.

**Agents across machines.** Any agent can start, tell, read, wait for and stop agents on any
worker, and carry a branch between machines (`docs/decisions/projects.md`). A space is a folder
on a machine; an orchestrator is any agent that started others; each agent's row names its
parent (`slopty_proto::agent::PARENT_FACT`), so every device draws one tree. The server keeps no
task, plan or outcome of its own. The hub answers the agents' verbs itself
(`slopty-server::hub::agents`: `AgentStart`, `AgentTell`, `AgentWait`, `AgentStop`, `Agents`,
`BranchBring`, `WorkerFacts`):
- Every link says who it speaks for (`slopty_server::Speaker`): a client is the person, an MCP
  surface an agent, and the CLI inside a Slopty terminal (`Role::Shell`) an agent when an agent
  runs there, when an agent opened or typed into it, or when the server does not know it. A
  terminal's token (`SLOPTY_SESSION_TOKEN`) proves which terminal a program speaks from, and so
  which agent is the parent of what it starts. Only the person sends a composer's message,
  restarts a worker or changes settings; a request is answered in its agent's terminal alone,
  and no verb answers one. An agent tells and stops only agents of its own tree.
- `AgentStart` picks the folder, or has the worker clone the repository named
  (`Verb::CloneRepo`), then forwards a `StartThread` at a seat derived from the caller and its
  idempotency key, so a repeat takes the same seat. The start's mode is the person's
  `[agents] autonomy` in each harness's words (`hub::agents::mode_for`); an agent names no
  harness arguments. The child's facts name its parent, and its system text is one line that
  says so.
- The server delivers nothing to an agent. `AgentTell` is typed into the child's terminal as one
  submitted message, framed as an agent's words; `AgentStop` presses Esc in each session at work
  under the agent, the deepest first. A parent hears of its children by `AgentWait`, which
  watches the agents' rows and answers once one (or each) rests, needs the person or ends.
- Each worker reports open facts (`ToServer::Facts`: agents, toolchains, GPUs, power and its
  clones, from `slopty-worker::facts`), gathered the first time the hub asks and kept current
  from then on. The hub adds what it knows itself (os, cpus, memory, load, live agents, repos)
  and cuts them to bounds; `machines` and `slopty workers --json` show them.
- `BranchBring` has the source worker bundle the branch (`Verb::BundleBranch`), reads it in
  4 MiB parts, uploads it to the target worker and has it fetched there (`Verb::FetchBundle`).

Every Claude Code run the worker starts adds `--mcp-config=` naming `slopty mcp`
(`slopty_agent::mcp`, the CLI from `Host::tools_cli`), which serves Slopty's tools
(`slopty_tools::tools`: `machines`, `agent_start`, `agent_tell`, `agent_read`, `agent_wait`,
`agent_stop`, `agents`, `branch_bring`, each also a `slopty` command, and `show_visual` and
`show_page`); Codex and pi get the same server through their own config doors. Every session gets `SLOPTY_SERVER`, so `slopty mcp` and the CLI inside it find the
server. The attention ladder rolls up by thread, tree, space and fleet
(`slopty-server::hub::ladder`). Tests start `slopty-stub-claude`, never `claude`.

Crates: `slopty-engine` (trait + libghostty-vt backend), `slopty-grid` (frame model, diff, cache),
`slopty-predict`, `slopty-pty` (openpty/spawn, async master, ptyd protocol + client),
`slopty-worker`; app `slopty-ptyd`.

## 3. Remote windows: Parsec-class pipeline

```
SCStream(display | window-as-display-crop | window, 420f BT.709, minimumFrameInterval=0 (the display's beat), queueDepth=3, showsCursor=false)
  → VTCompressionSession(HEVC, EnableLowLatencyRateControl @ creation, RealTime,
                         AllowFrameReordering=false, AllowOpenGOP=false, MaxFrameDelayCount=0,
                         EnableLTR, MaxKeyFrameInterval=∞, AverageBitRate + DataRateLimits)
  → packetize (≤1200 B datagrams, 16 B header; the data sent, then reed-solomon-simd parity per frame)
  → QUIC datagrams                                      ── client: NACK/refresh datagrams; LTR acks + telemetry on the control stream
client: reassemble/recover → VTDecompressionSession(RealTime) → CVPixelBuffer (IOSurface)
  → VideoLayer on the decoder's thread (CVMetalTextureCache, zero copy) → next vsync
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
frame is up the tile hides the client's own pointer (`CursorStyle::None`, a fork addition: on
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
`Idle` immediately, since a window that is not on screen cannot be drawing. Two states outrank
the frames: `Locked`, the Mac's screens locked, and `Away`, another session (the login window, or
another user after a fast user switch) on them, both read from the session dictionary
(`slopty_capture::console`) with each geometry probe. The receiver treats them as `Idle`, and the
tile dims its body under the scrim with a card that says so (DECISIONS.md, "The client is told
when the Mac is locked").

**Capture to glass on any link.** Every stream probes the worker's clock: a
`Feedback::Clock` datagram four times a second (one a report at first), answered at once by the
stream's control with a `Kind::Clock` media datagram carrying the probe's stamp and the capture
clock as it came and left. `slopty_media::ClockSync` keeps 30 s of them, fits a line
through the fastest of each 2 s slice (NTP's offset, with the drift between the two Macs), and
steps with a clock that jumped; the decoder's callback places each picture's capture on the
client's clock through its newest anchor (`FrameStamp::captured`), and the pacer times it to the
layer's glass time. The overlay's plain line leads with capture → glass and says its bound
(DECISIONS.md, "Capture to glass on any link").
A stream's rate, wherever it shows (the overlay), is one number from the pacer:
the pictures painted on this client in the last second, with the frames that missed the display
counted apart and shown as their own flag (`Pacer::rate`, `PaintRate`; DECISIONS.md, "A
stream's rate is one number").

**Worker capture path.** A display target is one `SCContentFilter(display:)`. A window target
is served two ways, and the worker switches between them on the live stream
(`updateContentFilter` + `updateConfiguration`, no restart, no new encoder): while the window
is on screen, sits entirely on one display and no window counts as covering it
(`crop_allowed`; other processes at levels 0–8 and sibling windows of the same app count,
the window's own menus and sheets, the Dock's full-screen hit region, the menu bar and
status items do not: `counts_as_occluder`), the stream is the *display* filter restricted
to the window's application (`display:includingApplications:`) with `sourceRect` at the window's frame (`Target::resolve_crop`,
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
locally over the control socket (`slopty worker screens`; `slopty-probe screen` appends them on
loopback). Nothing of this crosses the wire.

**Several streams, one Mac's encode engines.** Every stream has its own capture, session, encode
thread and packetizer. The hardware engines are the one thing they share, and
`slopty_worker::screen::engines` arbitrates them. A session made for 60 or more declares 120
until its first frame, so VideoToolbox gives streams an engine each (`placement_fps`). A client
says which tile has its keyboard in an active window (`ScreenRequest::Focused`, from
`ScreenView::tell_focus`). When that stream's encoder watch would step its rung down, the
unfocused streams step down a rung instead and hold it (`Engines::contended`), and they are
asked again only after a settling second and two of its windows. Losing the last focus, or
closing the focused stream, ends every hold (`Engines::unfocused`). Captures are not held back
for the focused one (decisions, "A focused stream keeps its rate").

**Presentation path.** The reassembler stamps every complete frame with the arrival of the
datagram that finished it (`FrameOut::arrived`); the stream worker parks that instant under the
frame's presentation timestamp, and the VideoToolbox callback — which is given nothing but that
timestamp — picks it back up as a `Presentable` (`CVPixelBuffer` + arrival + decode instants).
The callback hands it to the view's presenter on its own thread (`ScreenHandle::set_present`)
before the newest-only `watch` channel other readers use. The presenter
(`slopty-ui::screen::glass`) gives the picture straight to the stream's `VideoLayer` (gpui-fast,
`gpui_apple::fast::video_layer`): a `CAMetalLayer` in a native host under GPUI's layer, drawn on
its own thread by GPUI's surface shader, with one picture at most on its way to the glass and
the newest waiting in a one-picture mailbox. No GPUI frame is drawn for a picture. The view
places the layer in its own frame at the picture's fitted (or zoomed) rectangle
(`Window::paint_native`, no hitbox, so the pointer stays GPUI's), clipped by the tile and its
pane, and draws the pointer, the zoom readout and the ⌘⇧I overlay over its hole; it draws again
only when those change or the picture's size or chroma does. A tile that is not drawn (a
background tab, a hidden pane, behind its pane's shown tab) places no layer, which hides it. A `slopty_client::Pacer` beside the layer owns the one decision
left, **present on arrival**: a picture older than the one up is dropped (`late`), one the
layer's mailbox replaced before the glass is `skipped`. The pacer is also the instrument: the
layer's report (`on_presented`, the window server's presentation) stops each picture's clock,
and a ring of the last 240 gives arrival → glass p50/p95/max, the decoder's share, the spacing
and its jitter — read by the overlay (`hud_lines`) and the app self-test's `dump`
(`ScreenInfo`, with the layer's presented placement). The pacer is pure and clock-injected
(`slopty_client::pacing`); the glass's bookkeeping and the placement are unit-tested with the
test platform's native hosts. An e2e render cannot see the layer, so for that frame each stream
draws its picture with GPUI too, over its hole (`screen::capture_pictures`).

**Audio** is one sound per worker on a client, not one per stream (`slopty_worker::screen::sound`,
`slopty_client::screen::sound`). ScreenCaptureKit filters audio by application, so the worker
captures it once for a connection: an audio-only capture (a 2 × 2 picture at 1 fps) of the main
display filtered to the union of the applications behind the client's streams, every app when one
of them is a display, changed in place with `updateContentFilter` as streams come and go
(`Heard`, `CaptureSource::start_sound` / `hear` / `stop_sound`). The video captures capture none.
That sound (48 kHz stereo, this process excluded) → one `AudioConverter` Opus encoder (Apple's,
in the OS; 10 ms packets, 96 kb/s) → one `Audio` datagram per packet on media stream 0
(`slopty_proto::media::SOUND`; a worker numbers its streams from 1), never NACKed (a lost 10 ms
is cheaper than a late one). While
the client's sound reports (`ScreenRequest::SoundReport`, every 50 ms while packets arrive) show
audio loss, each datagram also carries the one or two packets before
it (`slopty_media::AudioCopies`, `audio_datagram`), and the client decodes those copies into a
gap. What no copy covers is concealed by repeating the last pitch period of what played
(`slopty_codec::audio::Conceal`, up to 60 ms).
The worker stops sending 300 ms after the last non-silent sample, so silent apps cost nothing.
The client's router starts one sound for the connection with its first stream and ends it with
the last; it decodes with `AudioConverter` and plays through an output audio unit whose render
callback fills each device I/O buffer straight from a jitter buffer (`slopty-codec::audio`:
`Jitter` sets the target, `Ring` plays) that aims for the lateness it measures (p95 over 5 s plus a device buffer, 20–120 ms),
prefills after a start, a mute or a dry run, and converges by dropping or repeating one 5 ms
slice with a crossfade, which also absorbs clock drift;
iOS puts the app in the `Playback` session category so it plays past the ring switch.
Mute is the worker's sound on this client (the pill, ⌘⇧M, View ▸ Mute Sound), so every tile
of the worker says it at once: packets still arrive and decode, only playback stops, so unmuting
is instant and other clients hear nothing different. A new tile brings the settings' preference
only until a choice is made on the connection.

**Clipboard, files and ports** ride beside the control stream (`slopty-proto::transfer`,
`slopty-net::streams`). Control messages (`ClipMsg`, `XferMsg`, `WorkerMsg::Ports`) go on the
control stream. Bytes go on unidirectional bulk streams that open with `UniHead::Bulk` and are
sent at priority −2, so a file never queues ahead of a keystroke's echo or a video frame. A
forwarded TCP connection is a client-opened bidirectional stream that opens with `TunnelOpen`,
sent at −1: above files, below video.

- **Clipboard: announce, then fetch, lazily.** The worker reads `NSPasteboard.changeCount`
  every 50 ms while some connection has sent `Watch(true)`, and the count alone every 250 ms
  while clients are linked but none watches, to stamp its own copies with when they were made.
  A client sends `Watch(true)` while a remote tile has focus and the app is frontmost. A change
  goes out as an `Offer`: its items, each with its representations, named by `ClipType` (one of
  six formats, or an Apple UTI that only an Apple end writes), plus `age_ms`, the time since
  the copy. A poll reads only the types, the markers, and text or file URLs that fit 64 KiB
  inline. Everything else is listed with no size and read when fetched, while the change count
  still says the same contents (else `Unavailable`). A fetch names one representation by
  `RepRef` and may carry a `max`, answered `TooBig { size }` past it.
  - **Mirror, latest copy wins.** The worker places a client's offer at arrival − `age_ms` on
    its own clock. When that is newer than its own last copy, it writes the offer to its
    pasteboard at once as promises (`NSPasteboardItemDataProvider`), so any app's paste there,
    ⌘V or `pbpaste`, reads the focused client's clipboard. A promise's read sends an urgent
    `Fetch` to the client and waits at most 5 s. ⌘V to a window still holds that client's
    window input in order until any fetch lands (up to 3 s): it fences the offer overtaking the
    chord, and re-writes the paster's own offer when two clients share a worker.
  - **Relay.** The client re-offers a worker's offer to every other worker, origin kept, and
    answers their fetches by fetching from the origin (`slopty_client::clip::relay`, off the
    main thread). A copy on worker A pastes on worker B.
  - **Prefetch.** After an offer the client fetches in the background every representation of
    known size within `min(8 MiB, cwnd ÷ rtt × 250 ms)`. A representation of unknown size is
    never fetched ahead, so a big copy costs a types list until it is pasted. A paste of the
    prefetched ones reads memory. A fetch a paste waits on is
    urgent and its bulk stream goes at −1, ahead of uploads.
  - The macOS client puts promises on the general pasteboard, one data provider per item. A
    paste fetches what prefetch left, inline or over a bulk stream.
  - **Secrets.** Concealed or transient contents are offered by type only (`concealed: true`),
    never prefetched or mirrored. A paste chord fetches them; the worker writes them marked
    concealed and transient and clears them after 60 s or when that client's clipboard moves on.
  - A shell's ⌘V or ⌃V with a picture copied here and no text sends the offer, when the worker
    has not heard it, and then `TermRequest::PastePicture` on the same ordered stream. The worker
    holds that session's input behind it the way it holds a window's, fetches the picture and
    writes it, then applies the chord: ⌃V as the key, ⌘V as an empty paste. Claude Code reads
    the picture off the worker's pasteboard on either. The request has no datagram copy, which
    could overtake the offer.
  - Every Slopty write carries `com.aislopware.slopty.origin` (`transfer::origin_bytes`).
    Together with the `changeCount` of our own write and a same-digest backstop, this stops
    echoes, also when client and worker share a Mac. Writes on both ends are for this Mac only
    (`CurrentHostOnly`), so Universal Clipboard does not pull every promise.
  - The worker's clipboard logic is `slopty_worker::clip`, over the `Board` seam
    (`slopty_input::pasteboard::Board`); the client's is `slopty_client::clip` (`ClipCache` per
    link, `ClipSync` for this device's pasteboard). Tests use named pasteboards
    (`--pasteboard`), never the user's.
  - Reading another app's contents can ask the person: on iOS always, on macOS when
    `accessBehavior` says so (`Pasteboard::reads_ask`). Then the client reads only for a paste
    into a remote tile, never when a tile takes focus, and the worker reads nothing. The change
    count, which never asks, decides whether a paste reads again. On iOS
    (`slopty_platform::pasteboard::IosPasteboard`) writing is free: an offer becomes one
    local-only `NSItemProvider` per item, inline text at once and the rest loaded on paste.
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
  - Copied files paste the way a drop lands (`slopty_client::clip::ClipFiles`). ⌘V in a
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
    names the durable bytes of each partial and each file already landed, each with the version
    (size and modification time) its header said (`transfer::Held`). The worker resumes a held
    file only while it is still that version, and otherwise sends it from 0
    (`slopty_worker::xfer::resume_points`). A download outlives its link: when the link goes it
    waits on its worker's `slopty_client::xfer::Line` for the next link (the app's redial, the
    File Provider domain's own) and goes on over it from what it holds, its progress in Finder
    unbroken and its cancel still answered.
  - Files that apps promise rather than name (Mail, Photos) are received by a view beneath
    the web pages that registers only the promise types (`slopty_platform::file_drop`), and
    on iPad every drop comes through a `UIDropInteraction`. Each drop lands in a temporary
    directory of its own and then reaches the tile under it as an ordinary file drop. A file
    that failed is named in a notice, and what it wrote is deleted.
- **File tiles.** `ReadFile` answers a text file whole, up to 16 MiB (`FILE_BYTES`), and a
  picture or PDF (its first bytes sniffed, `slopty_worker::file::media`) as its bytes, up to
  128 MiB (`MEDIA_BYTES`, `FileRead::Media`). Past `INLINE_FILE_BYTES` either follows on a bulk
  stream (`FileRead::Streamed` with its `Body`, `Purpose::FileBody`). `WatchFiles` follows each
  watched file on kqueue or inotify (`slopty_worker::fswatch`) and sends a changed one again,
  `WatchFolders` does the same for folder tiles' listings (`files::watch_folders`),
  `WatchRepos` follows the repositories behind a git status or a diff and says `RepoMoved` once
  a burst when something git reports on changed (`slopty_worker::repowatch`), so the view model
  asks git again only then, and `WriteFile { path, text, base_modified_ms }` saves an edit
  (`slopty_worker::file::write`). The save writes a temporary file beside the target, fsyncs
  it and renames it over, keeping the mode and writing through a symbolic link. A file whose
  modification time is newer than `base_modified_ms` is a `Conflict` and stays as it was. A
  non-regular file is `Failed`, and so is a text or a file past the 16 MiB cap. The worker
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
  - The palette's "Forwarded ports" lists the forwarded ports: each opens in a browser tile or
    in the default browser. Neither the shell's header nor the window's cluster counts them.
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
`Pace` the gate the worker runs each capture through, keeping the rung's schedule on the display's
beat (decisions/video.md, the cadence ladder and "The capture follows the display's beat");
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
trackpad gestures (a pinch, a rotation, smart zoom, a swipe as one event, and, while the tile
sends its gestures, the gesture each trackpad scroll comes with, which a swipe between pages
follows) built as `NSEventTypeGesture` events with the IOHID subtype AppKit reads, on the same
route (DECISIONS.md, "Trackpad gestures reach the remote app"); each press numbered, its drags and
release under its number ("A press, its drags and its release share one event number");
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
(`hud_lines`) and `slopty-probe screen`, and `slopty-client::pacing` carries the arrival →
present numbers beside them. Transport: ACKs within 2 ms and a 32-packet initial
window (`slopty-net::endpoint`).

**A display sized to the client.** `slopty-vdisplay` makes a virtual display for one client
through CoreGraphics' private `CGVirtualDisplay`, looked up at runtime (decisions/video.md, "A
display sized to the client"). `plan` turns the client's pixels, scale and refresh into a fixed
descriptor and a mode; `VirtualDisplay` is created on the main thread, resized in place with
`applySettings:`, held to its mode by `enforce` and removed when dropped. Its
`display_id` is then an ordinary display target for capture and input.

A client asks with `ScreenRequest::OpenDisplay`: its tile's pixels, the backing scale and
refresh of the screen the tile is on, and a 16-byte key it draws once and keeps beside its
layout (`slopty_client::screen::display`). The worker's main thread serves the run loop, and
the daemon runs on a thread beside it, because the displays live on the main queue
(`slopty_worker::screen::sized`). A stream's task hands create, resize and release over as
jobs, keyed by the client's key, and waits for the answers. A display is enforced every
100 ms until it settles and again on every display reconfiguration. Once ScreenCaptureKit
lists it, it streams through the same `Pipeline` as a physical display. `ScreenEvent::Display`
goes out before `Opened` to name the display, or the physical one streamed instead and why.
A later `Resize` carries size and scale. It resizes in place, or remakes an outgrown display,
and the stream then switches to that display (`Pipeline::switch_display`). The last stream
of a key releases the display, after its input is let go and its capture stopped. Workers
advertise `WorkerCaps::virtual_displays`; Linux says no.

## 4. Workspace

The window is `slopty-shell`, a terminal's window rather than a GUI-first app's
(`docs/decisions/layout.md`), and nothing else draws in it. The
shell knows nothing of workers: it asks one RPC service, Slopty's view model
(`slopty_ui::view_model::ViewModel`), in process through `slopty_view_rpc::memory_client`, and
that service is fed by `slopty_ui::workspace::WorkspaceView`, which keeps every worker this
client reaches and draws nothing (`docs/decisions/ui.md`, "The tiling workspace is deleted; the
shell is the only interface").

**The model in the shell's words** (`view_model::rows`, `view_model::transcript`):

- a **device** is a worker, its row in Settings → Devices carrying the actions the app answers
  (`DeviceAction`: update, remove, wake, forget, show in Finder; on the server's row update all
  and copy grant);
- a **space** is a folder on a worker, and a **chat** is a thread there, its **session** the
  thread's live status; a subagent is a child thread under its parent;
- a chat's **transcript** is its thread's turns, the person's messages as user entries and each
  answer as one assistant entry of text, reasoning, tool calls and notices, followed while the
  shell holds a watch on it (`Ask::Follow`).

A chat is its thread, by the thread's id: a new session starts on its project's machine
(`slopty.StartSession`, an agent's TUI or a plain shell in a terminal of its own) and the sidebar
lists it once that worker's table does. The standing lists (devices, spaces, chats, sessions, the server's presence)
are gathered for a frame and published as watches (`Feed`); live sessions are said again every
15 s, inside the shell's 45 s staleness window. Every other call is answered on the UI thread by
the workspace (`Ask::Call`) or refused as an unknown method.

**Each worker** is one entry in `WorkspaceView`: its link (`slopty_client::WorkerLink`), its
thread hub (`ThreadHub`, the worker's thread table and a mirror per followed thread), its
sessions and its item registry. The worker keeps the registry
(`slopty-worker::items::ItemStore`: terminals, and the windows and displays being streamed,
each with a name and open facts; `Add`/`Remove`/`Rename`/`SetFact`, each op carrying only what
it changes, a refused one answered with a snapshot, a session's item made and removed with it);
`slopty_client::ItemDoc` mirrors it with optimistic local ops and knows its own echo by
`by == me`. A shell opened for a chat carries the fact `chat`, so one still running when the app
comes back is found again by the chat that asks (`surfaces::live::terminals`).

**The right pane and the dock** (`workspace::view_model::surfaces`) answer the shell's calls
through each worker's own verbs. Beside a session's terminal the inspector reads its tray
(`slopty.WatchWork`: the background work at it now) and its dock (`slopty.WatchDock`: the
agent's prompt box as its TUI holds it), and neither asks the agent anything:

- *Changes*: a thread's diff, the working tree's live and the branch's against its base on
  asking, cut by the worker's review (`GitOp::Changes`) and written as the unified patch the
  shell reads, with each file's two sides whole for its highlighting. The worker keeps no turn
  snapshots, so there is no turn's diff.
- *Files*: a space's tree, its search, a file read and saved (its version is its size and
  modification time, which the worker's save checks a conflict by), an entry moved or trashed,
  and the tree's and git's changes watched.
- *Terminal* (⌘J, and a right-pane tab): `OpenTerminal` asks the chat's machine for a shell in
  its folder and answers with the session; the shell then hosts Slopty's own `TerminalView`
  for it (`slopty_shell::live`, a `Host` the app installs that hands the shell a view by id).
- *Remote window*: the machine's windows and displays to pick from (`slopty.ListScreens`), one
  shown (`slopty.OpenScreen`, which puts a window or display item on the worker whose stream
  `reconcile_screens` opens) and let go (`slopty.CloseScreen`); the shell hosts the
  `ScreenView`. Its "…" in the corner opens the stream's controls (`screen::controls`): what
  the view does itself, and what the workspace offers it (`workspace::desktop`: the system's
  shortcuts through the session tap, the curtain held from this device, a display the worker
  makes at the view's size and follows, the clipboard typed in). A window follows its pane's
  size once it rests (`screen::fit`).
- *Browser*: the ports the chat's machine listens on, each forwarded to a loopback port here
  (`slopty_client::tunnel`), so a preview loads the worker's page at a local address.
- *Settings*: the agents each machine offers (from its worker's caps; installed by the person,
  never by Slopty), the one autonomy setting, Devices with Add device (this Mac shared, a
  machine installed over ssh, a server joined or set up; `surfaces::adding`), a phone's code to
  join, and the threads the person archived (`interface/archived.json`, the person's own filing:
  archiving stops nothing). Nothing deletes a session, whose record is its agent's own:
  removing a project archives its sessions, and a folder is a project while an unarchived
  session there, a plain terminal there or the person's adding keeps it.

**Around the shell**, the workspace keeps what makes a worker feel local and what reaches the
person away from it:

- a page a worker's shell hands over (`workspace::handoffs`): opened in the browser, or held
  back in a notice with Open, a page on the worker's loopback at its forward here; a path
  ⌘-clicked in a shell, opened in its chat's Files pane (`slopty_shell::live::open_file`); and the
  keyboard told to each terminal's worker, so the handoff and the program's focus reports go
  to the shell the person is looking at;
- the clipboard shared with the workers, following the keyboard into whichever live view has it
  (`workspace::remote`, `clipboard`), files dropped onto a terminal or a window, uploaded and
  typed or pasted there (`remote::transfers`), and the forwarded ports;
- the notes posted while the app is not in front (`workspace::attention`: an agent that comes
  to need the person, a program's `blocked` status, a long turn or command that ends), each
  opening its session, whose TUI the person answers in; the turns left unread
  (`workspace::turns`, the worker's word, so a turn read on the phone is read on the Mac), and
  where the person is (`workspace::presence`), which the server uses to send a notice only
  where it is wanted.

A worker that drops keeps its rows ("reconnecting"); its next snapshot replaces only what it no
longer has. What an agent is doing comes from its worker's thread table, which a client that
joins late is sent whole, so it names every running agent and its status at once
(`workspace::agents`, `workspace::faces`).

## 5. Agents

**The thread model (being built; decisions, `agents.md`).** Every agent is to speak one
agent-neutral model, `slopty_proto::thread`. A thread's turns and items, its plan, background
tasks and its TUI's prompt box and queue are changed only by `Action`s.
`ThreadState::apply`, the one pure reducer, runs on the worker, which keeps the thread's log,
and on each client, which mirrors it. A followed thread streams a snapshot of its last turns
and then actions numbered by a `Cursor { epoch, seq }`, so a client that comes back gets only
what it missed. Every worker also keeps one small thread table (`ThreadRow`s, `TableFrame`) for
the lists that follow nothing. A start and the intents left (`Seen`, `Name`) carry a
client-made `IntentId` that the worker acts on once. Each agent's adapter maps its native session onto the
model on the worker. What an agent is, how it is reached (`Drive`) and what its tools are stay
open strings. On the worker, `slopty_worker::thread::Host` holds every
thread. Each thread has a log under `threads/<id>/` (a snapshot plus a tail of framed actions,
compacted at turn ends), the last few thousand actions are kept in memory for followers coming
back, and per-thread intent outcomes are deduplicated by id. A `Follower` turns a cursor into
frames, gathering batches within the client's latency budget and merging runs of appends.
Every agent runs as its own TUI in a Slopty terminal (`docs/decisions/agents.md`, "TUI-first"):
a start opens a terminal with the person's own `claude`, `codex` or `pi`
(`slopty_worker::thread::start`), Claude Code on a session Slopty names, with Slopty's tools,
its hooks relayed by `slopty hook`, and Slopty's mod (`--plugin-dir`,
`slopty_agent::claude::draft`), whose two read-only hooks hand the TUI's prompt box to the same
relay after a pause in typing. The worker reads the pictures pasted into the box from Claude
Code's own store, and the thread holds the box (`Action::DraftSet`) and the prompts queued behind
a turn, read from the transcript's queue records (`Action::QueuedSet`), which the inspector
draws under its timeline from the view model's dock (`work::dock`, a `PromptBox`). Any
Claude Code in any terminal is read (`slopty_worker::thread::claude`): the terminal's process
names its session in Claude Code's registry (`slopty_agent::claude::sessions`), and its
transcript, followed on the kernel's word, maps onto the thread model through the transcript
decoder (`slopty_agent::claude::history`), with a linked thread per subagent. One thread per session, named by the session's id; a
`/clear` or `/resume` moves the terminal to the next session's thread. The hooks give the status,
and a session without them takes its phase from its transcript. Codex and pi rows name their terminals and have no inspector. On the link, `ClientMsg::Thread`
carries the requests and the control stream carries the table (`WorkerMsg::Threads`) and each
intent's outcome (`WorkerMsg::IntentDone`). A followed thread streams on a unidirectional
stream of its own (`UniHead::Thread`), one task per thread on the worker
(`apps/slopty-worker/src/threads.rs`), which also carries its pages and expansions. What an
agent sends an agent (`agent_tell`) is typed into its terminal (`SessionHandle::submit`, which
no client link reaches: a paste, then Enter 200 ms later, refused while it needs the person), and
a stop is an Esc. While an
agent's process lives, the worker stamps its thread alive (`Action::Lease`, every
`LEASE_EVERY`), and stops the moment it is gone. A review
is a diff of the repository's states (the unstaged changes, a branch), cut into hunks on the
worker. Each worker publishes its
table to the server too (`ToServer::Threads`), and the server ranks the fleet on one
attention ladder (`slopty_proto::thread::attention`, `slopty_server::hub`'s `ladder`): Needs
you, Failed, To review, Working, Waiting, Idle, a subagent folded into its parent, rolled up
per thread, tile, worker, tree (an agent and every agent it started, across machines), space
(a folder or a repository's clones) and fleet, and sent to every link as
`FromServer::Ladder` whenever a rung moves. A person's client says where they are
(`ToServer::Presence`: desk or handheld, active, the tiles on screen). A thread that comes to
need them, fails or finishes is a `FromServer::Notice` to the clients they are at (a finish
of an agent another started goes to that agent, not the person), none while
its tile is on screen and no handheld while they are at a desk.

Tests drive `slopty-stub-claude` (`slopty_testkit::claude_tui`), a stand-in for Claude Code's
TUI in a real terminal: it keeps the registry record and the transcript Claude Code keeps, runs
the hooks it is given, relays its line as the mod would when it loads one, keeps a pasted
picture where Claude Code does, and records every read of its terminal, never a signed-in
`claude`. The mod itself is checked against the real Claude Code it was recorded with
(`cargo xtask claude-mod check`, `cargo xtask fixtures claude-mod`).

Finding what needs the person: ⌘⇧A (the "Next Thing Needing You" menu item) reveals and
focuses the next rung of the attention ladder on any worker, cycling from the active item: a
waiting agent, then a finish not yet looked at, failed ones first; the bell's badge counts the
agents that wait and the turns left to review, and the bell (⌘⇧U) shows the navigator at them,
under *Needs you* and *To review* (the phone's way in). On macOS the count is also the Dock
badge, an attention event bounces the Dock icon when the app is not active, and (bundled app
only) a notification-centre banner is posted; clicking it activates the app and reveals the
thread (`WorkspaceView::notification_response`).

## 6. UI

GPUI (fork: `aislopware/gpui-fast`, GPUI imported flat out of zed with longbridge's Retained
Mode, with our iOS commits and the zed imports merged in; the bases live in
`xtask/upstream.toml`) plus gpui-kit (fork: `aislopware/gpui-kit`, upstream main plus our
commits, one of them re-pointing deps at gpui-fast). `cargo xtask upstream check` shows the drift,
`cargo xtask upstream sync` merges or rebases, pushes and moves the `Cargo.lock` pins. The iOS
backend is the fork's own `gpui_ios`, grown from zed PR #63068's August draft (zed merged a
rewrite of it that lacks what follows; the fork keeps its own, `docs/decisions/tooling.md`),
extended in the fork for the surface element (zero-copy
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
**Drawing under retention.** gpui-fast builds a view again only when it was notified or when
something it read in its last build changed: an entity updated and notified, a global written,
a list's or a scroll handle's state moved. Every other view is replayed from the last frame, so
a view must hear of everything it shows and must not read what changes more often than it
does. The window itself tells the frame-time probe of every draw it makes, whichever views it
built again (`frames::watch` on gpui-fast's `Window::observe_draws`), so no view is built again
on the probe's account. The shell draws as `docs/decisions/layout.md` rules.
A shell does not read where the pointer is while it draws, since that is news with every move
over the window: its element lets the scrollbar go when the window goes inactive or the grid
moves, and its listeners update the view only when a move changes something in it (a row
reported to a program, the ⌘-hovered cell, the block hovered), keeping the pointer's place in a
cell that no build reads as a change. A value measured while drawing (the workspace's width
inset, a terminal's fitted grid, a
stream's painted bounds) is compared in prepaint and, when it moved, sent as a notify after the
frame (`cx.defer`, or `Window::on_next_frame` on a weak handle), because a notify raised in the
middle of a draw only marks the view. For the same reason what a view shows is a function of
what it holds, never of the clock read while building: a loading grace is a mark its timer
sets (`screen::past_grace`), and the transcript's edge fades read the list's scroll in prepaint
(`gpui::edge_fade` with `hidden_by_list`), asking for the next frame when the list's layout moved
it. What a view keeps but does not show is not
written through the view after a frame: a typed key is timed when its frame reaches the display,
into a record the terminal shares with its element (`TerminalView::latency_record`), since an
update of the view there would count as a change and build it again with the area's next
frame. Reduce Motion is watched rather than polled: on the Mac an observer of
`NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification`, on iOS of
`UIAccessibilityReduceMotionStatusDidChangeNotification` (`slopty_platform::motion::watch_reduce_motion`),
and a change reaches the workspace, the icons' spinners, GPUI's own flag (which gpui-kit's
animations follow) and every window. A
view a sash or a drop moves is built again in its new place: the fork replays a view only where
it was drawn.
`retained::stale` checks all of it: it draws the same state from scratch and diffs the painted
quads and sprites against the frame the window showed (TESTING.md, "Retained frames"). In the
self-test GPUI also checks every frame as it is drawn, against the same frame drawn from scratch
in a shadow that leaves no trace (gpui-fast's `fast::check_retained`), so a view stale for a
frame or two between dumps is named too (TESTING.md, "Every frame, as it is drawn").
**Design system.** Every chrome surface draws from `slopty-theme` and nothing else (ruling
"Design tokens" in DECISIONS): a four-step neutral ladder (`canvas`, `panel`, `raised`,
`overlay`), one hairline (`border`), three text levels, one accent with its fill and the
fill's text (`accent_fill`, `accent_ink`), and
three status tones (`success`, `warn`, `error`) that the chrome uses only where state carries
meaning (worker dot, agent badge and outline, the bell's badge, failed result, failed-command
separator). Geometry comes from `radii` (xs 4 / sm 6 / md 8), the 4/8 pt `spacing` scale
(2 / 4 / 8 / 12 / 16 / 24) and a type scale hanging off `ui_size` (`caption` −3, `small` −1,
`title` +2), so the settings' font size moves every label together; alphas for tints, hover
washes, the scrim and the separators are the `alpha` constants. Hairlines carry the elevation,
shadows are `shadow_sm` on floating layers only (picker, worker switcher, search bar, "↓ latest").
Focus is one accent hairline: the active item's frame and the search bar while
they hold the caret. Pills (tile header, badge) are `small()` text on a `TINT`
fill of their tone with the tone as text; buttons are `radii.sm` with `raised` → `overlay`
hover/pressed, and the one primary action on a surface is `accent_fill` with `accent_ink` text.
gpui-kit's widgets (inputs, Markdown `TextView`) read gpui-kit's own theme, which
`slopty_kit::kit::sync` rewrites from the same tokens on every theme change, and the Markdown in
a file's preview gets `markdown_line_height`, paragraph gaps of one base unit, headings
stepping down from `title()` and code in the terminal mono at `small()` on `raised`. Headless
tests read the tokens back through `painted_quads()`: the accent vs hairline item frame, the
`warn` outline of a blocked agent in both variants, the `error` separator tone, and gpui-kit's
colours after a sync. gpui-kit's code editor (`EditorState`) is the file tile's body, in the
terminal mono at the tile's text size, its highlighter the app's own (§4).
One element is not GPUI's: the browser tile's page is a native `WKWebView` the window composes
under GPUI's layer, where the tile's body is drawn (§4). Its chrome (back, reload, the address)
is GPUI's, from the same tokens. It fades with its tile's opacity as any element does.
`slopty-ui::screen::ScreenView` hands the decoder's IOSurface-backed `CVPixelBuffer` to the
stream's `VideoLayer` off the main thread (zero copy, no GPUI frame per picture), places the
layer under the tile, draws the worker's cursor over it from the cursor channel, forwards mouse,
scroll and keys (including ⌘ chords the workspace does not bind) as `ScreenInput`, and asks the
worker for a stream scale matching its painted width. The pointer mapping (tile point → stream pixel over
the bounds the render recorded, press/release with clicks and modifiers, pixel vs line scroll
with its phase, ⌘-scroll forwarded with its ⌘ and ⌘⌥-scroll left to the workspace, moves off
the picture dropped) is covered headless
by `pointer_and_scroll_reach_the_worker_in_stream_pixels`. When the remote window changes size, the worker notices
within 250 ms, restarts the stream at the new size and sends `Geometry`; the workspace re-aspects
the item. The other way round, a window tile whose pane changes size sends `Resize` with the
size the tile now stands for, at the scale it drew the window at before
(`WorkspaceView::resize_remote_windows`; a display is letterboxed instead), the worker sets `AXSize` on the matched window off the
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
(`separator_color` in `crates/slopty-terminal/src/element.rs`; `crates/slopty-theme/src/lib.rs`), and at the right end of a
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
↩ as a key) and "Select block"; then, on every row, the terminal's own: "Copy"
(only with a selection), "Paste", "Find…" and "Clear screen", each what its shortcut does. ⇧-arrows move
a selection's head (`adjusted_head`: a cell sideways wrapping at the row's ends, a row up or
down), the shell never sees them; without a selection they are the shell's. The
menu is `Menu "Command block"` on a block, `Menu "Terminal"` elsewhere; the armed tap on the
phone opens the same. While the viewport's top row is inside a block whose prompt
rows have all scrolled above, a one-row header over the grid (`block-header`, role Button,
`TerminalView::block_header`, reading `TermState::block_head` — the prompt's rows alone, never
the output) names the command in the mono face on the panel colour, ruled
under with the block's separator colour; a click on it puts the prompt back at the top.
`TermState` also follows the worker's block news (`follow_command`): a block whose output
started is the running command (`Effect::CommandStarted`), and one that ended finishes it with
the status and the time the worker read between its `133;C` and `133;D`
(`Effect::CommandFinished`), even when it ran and ended between two frames; the view takes
that time (its own clock only for a block the worker did not see start) and emits
`TerminalViewEvent::CommandFinished { command, exit, elapsed }`, and the workspace badges the
tile's header ("done 12.3 s", "failed (1) 1 m 04 s" — `took_label`'s clock — in the success or warn tone,
`finished-<uuid>`, role Button) when the command ran at least `SLOW_COMMAND` (5 s) and its
item was not the active one — the shell's answer to the agent attention badge. The badge
goes when the item is activated (a press on it does that).
Headless `#[gpui::test]`s in `terminal/view.rs` read the
separators back from `painted_quads()` and drive the bindings with `simulate_keystrokes`.

**Render path and frame time.** One GPUI frame draws the workspace (`WorkspaceView::render`):
the title bar, then the tab on show from the tiling's `TabFrame`, laying out only the tiles
its panes show, each a header (or its pane's tab row) and a cached view (`Entity::cached`, so a
tile repaints only when its own view is notified), then the overlays
and the `frames::probe()` element last. The terminal element's prepaint resolves the monospace family
once per app (the `ShapeCache` global memoises the theme's list; listing the installed fonts
is a synchronous trip to the font server), reads the view's rows in place — only the rows
inside the window's content mask, so a grid hanging off the viewport builds nothing for the
rest — splits each into words (plain spaces and digits are the boundaries) and looks every word up in
the `ShapeCache` global — an `Rc<Word>` (the `ShapedLine` at the base size plus its per-byte
colours) per FxHash of (text, styles, family, palette, cell width, and the blink phase for a
word with an SGR 5 cell), never per zoom or focus. Nothing is swept per frame: each prepaint
stamps the words it uses, and past a budget of 2¹⁸ glyphs the quarter stamped longest ago
goes, so a scroll back finds what it showed a moment ago still shaped; a word is shaped with no forced width and
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
block); a keystroke pins the phase on for a half. Nothing in the workspace is drawn scaled, so
every glyph paints at its own size. The item chrome's text — title, pills, badge, block
headings — is `slopty_kit::chrome_text::ChromeText`: shaped once at its base size (a per-app
cache), sized by arithmetic rather than a taffy measure callback, painted glyph by glyph with
GPUI's baseline and advance arithmetic, ellipsis included. Worker events reach the workspace from the
link loop in batches (whatever queued while the last one applied, up to 256, in one update),
so twenty streaming sessions cost one update per batch; a session itself never sends more than 125 frames a second
(`MIN_FRAME_INTERVAL`). `slopty_kit::frames` times every draw (`begin` in `Workspace::render`,
`end` in the probe) into a 1024-frame ring with nearest-rank percentiles and a count of the
frame slots long draws swallowed; it is the fourth line of the ⌘⇧I overlay and the `frames`
block of the self-test `dump`, and `terminal::latency` stamps each keystroke so `dump` can say
how long the local echo and the worker's echo took to reach the display: a paint that holds a
waiting key hands `slopty_kit::shown::after_paint` the work, and the key is timed when the
window's presentation report (`Window::on_frame_presented`) says the first frame submitted
after that paint reached the glass. Presentation is vsync-synced (the fork's `CAMetalLayer`
keeps `displaySyncEnabled` at its default) and the compositor adds about a refresh, so a
paint's own clock, or the next display tick, reads a refresh or more early. The video pacer
stops its arrival → present clock the same way. The iOS simulator reports no presentation;
there the next display tick stands in.

**Settings.** `<data dir>/settings.toml` (`slopty settings path|init`) holds what the shell's
own Settings do not: the window's look, its fonts and its shortcuts are the shell's. `slopty-settings` owns the schema: `[terminal] minimum_contrast | copy_on_select |
alert = never | hidden | always | option_as_alt = false | true | left | right |
secure_keyboard_entry` (the ratio rides on `TerminalPalette`, copy-on-select, option-as-alt
and secure entry on `Theme::behaviour`, option-as-alt travelling on every `KeyEvent` to the
worker's encoder; the alert is read by the app's bell and agent handlers, see
decisions/terminal.md), `[clipboard]`, `[keys.terminal]` (below), `[network]`, and the
daemons' `[worker]`, `[server]` and `[agents]`; every key has a default, unknown keys warn,
and a file that does not parse is skipped with the error in a toast. The app polls the file's
stamp once a second. Its theme (`slopty_theme::Theme`, what the terminal and the remote
screens draw with) is the shell's look worn over the file's behaviour
(`slopty_app::interface::wear`): the appearance, the chrome's planes, lines, text and states,
the interface face, and the terminal's face, size and palette all come from the shell's theme,
read through `slopty_shell::live::{Chrome, TerminalLook}` and rebuilt each time the shell's
theme global changes. `WorkspaceView::set_theme` pushes it to every terminal and window and
sends the palette to every shell's worker, so a program asking for its colours (OSC 10/11)
hears the ones it is drawn in; the terminal element re-measures its cell grid on the next
frame and `fitted` resizes the session. iOS reads the same path, in its sandbox.

**Keymap.** The shell's keymap is set in its Settings > Shortcuts. A terminal's own
commands (`slopty_terminal::commands`: copy, paste, find, clear, the prompt jumps, copy mode,
…) each carry their default chords and the terminal key context; `slopty_terminal::bindings`
lays the file's `[keys.terminal]` over them (a chord in GPUI's syntax, `""` unbinding, a
chord GPUI cannot read said in the settings toast). The app hands the result to
`slopty_shell::live::keys`, and the shell's `apply_keymap`, which clears every binding before
it binds its own, binds Slopty's last, so they outlive each rebind the shell makes and a saved
file rebinds at once (`docs/decisions/terminal.md`, "A terminal's own keys are bound inside
the shell's keymap"). The terminal's key encoding is untouched: a chord bound in the terminal
context runs its command, every other key still goes to the program.

**Workers.** The app holds every added worker at once: one `WorkerLink` (on the process's one
client endpoint, with its own reconnect loop and silence check) per worker
(`slopty_app::workers`), all feeding the one `WorkspaceView` under the worker's `WorkerKey`
(its `WorkerId`'s 128 bits). A dropped link, whatever dropped it, keeps the worker's tiles
and what they show, set back under "reconnecting": a shell its rows, a window its last picture.
The next link takes the same views up in place (a shell attaches again, a
window's stream opens again behind its picture), and its snapshot reconciles the tiles. The system says when a link may have died under
it (`slopty_platform::resume`: the Mac woke, its screens woke, the session came back or was
unlocked, the app came to the front, and Network.framework's path monitor on a path change;
UIKit's scene and protected-data notifications on iOS). Each live link is probed at once (a
QUIC PING, alive on the first datagram back, given up after four round trips clamped to
0.25–1 s, `workers::Probe`), after a path change only once every connection has migrated
(`Endpoint::handle_network_change`). A link that answers is kept; one that does not is dialled
again at once while its tiles stay, dimmed (`WorkerStatus::Checking` and `Relinking`), and the
new link's views replace the old ones in one update, so no frame shows the tiles empty. A
worker between links is dialled at once rather than at the end of its backoff. A worker's
capabilities come with its `HelloAck`,
then as `WorkerMsg::Caps`, and from the server's directory while its own link is down; its
navigator header adds a warn line only when something is wrong (Screen Recording or
Accessibility off on a Mac); a worker on another build never links, and says so instead
(`WorkerStatus::NeedsUpdate`: both builds, an "Update" on its tiles' pill that deploys the new
build over SSH and dials it again at once, and the command to copy; otherwise the next dial
comes only after `redial::WRONG_BUILD` or a nudge). Its "…" in the navigator reads its machine
("macOS 26.5 · load 2.1") and each agent installed there with its version, and offers Update,
Connect, Wake, the clipboard and Forget as the app allows (`workspace::machines`). The titlebar
names only the workers that are down; the "…" menu adds a worker. The bell and the Dock badge count
agents across every worker, and ⌘⇧A goes to the next one wherever it is. A banner names only
a session, which the workspace finds on whichever worker runs it. The workers are the server's
directory's, cached as `directory.json` for an outage; a worker is keyed by its id, so one that
moves keeps its row and its tiles, and a dial that reaches another id at the listed address says
so instead of mixing tiles. The client keeps only its own id, in `client.json`
(`slopty_net::known`).

**The first run.** An installation with no server set opens on the first-run page, which is
the whole window (no titlebar, no workspace behind it): "Connect to a server", one line on what
that is, then "Use this Mac", the servers the tailnet answered with, the address field with
"Connect", and "Set up the server over SSH". "Use this Mac" joins the server set or the one the
tailnet found, else starts one here, then installs this Mac's worker against it, and is done when
the directory lists this Mac (`docs/decisions/topology.md`, "The first Mac runs the server").
Later, "Connect to another server" and "Add a machine" (⌘⇧H) show the panel as a dialog over the
workspace with a Cancel; a machine's panel offers this Mac, a machine over SSH and the tailnet's
workers the server does not list yet, and takes no address, since every machine joins through the
server. The phone adds "Paste", since it has no ⌘V. On the Mac the panel looks on the tailnet at
once and says so ("Looking on your tailnet…"); every server that answers is a row to connect to,
best first, and when none does it says "Nothing answered on your tailnet".

**Installing a worker over SSH.** On the Mac the machine panel's "Set up" section has, beside
"Use this Mac", "Install on a machine over SSH", and the palette offers it too. The
form (host, optional user and port) gives way to five step lines while `slopty_deploy::deploy`
runs on the networking runtime through the person's own `ssh`, unattended (`BatchMode`). This
is the plan `slopty worker deploy` runs. A bar under the steps fills while the binaries go up.
Cancel drops the run, which kills its `ssh`. A failure brings the form back, with the failure's
title, what to do and the machine's last lines. When the deploy succeeds, the worker is added
at its tailnet name (or its IP, or the host `ssh` reached) and the panel closes onto it. The
same deploy with `--update` runs from a wrong-build tile's "Update"
(`slopty_app::ssh`, `slopty_ui::add_worker`).

## 7. Crate map

| Crate | Role | Platform |
|---|---|---|
| `slopty-core` | ids, clocks, errors, small shared types | all |
| `slopty-proto` | wire messages, codec (postcard), byte goldens; the worker's local control socket (`ctl`, JSON lines, golden too) | all |
| `slopty-grid` | terminal frame model, row diff, line cache | all |
| `slopty-engine` | libghostty-vt engine: frames, scrollback, input encoders | worker |
| `slopty-pty` | openpty/spawn/resize, ptyd protocol | worker |
| `slopty-predict` | speculative local echo | client |
| `slopty-net` | plaintext QUIC endpoint (noq + null crypto provider), `host[:port]` addresses, admission by source address, known workers, framed channels | all |
| `slopty-media` | packetizer, FEC, reassembly, NACK/refresh policy, redundancy | all |
| `slopty-capture` | the capture seam (`CaptureSource`); ScreenCaptureKit on macOS | worker |
| `slopty-codec` | the encoder seam (`VideoEncoder`, `AudioEncoder`); VideoToolbox encode (worker) / decode (all) | split |
| `slopty-input` | the input and clipboard seams (`InputSink`, `Board`); CGEvent injection for remote-window input (keymap, pointer/scroll/keys, owner activation, a thread per stream, held input let go at the end) and `NSPasteboard` on macOS | worker |
| `slopty-agent` | each agent's TUI start, Claude Code's registry, transcript decoder and thread mapping, its hooks, and what a terminal shows of an agent (`AgentTable`) | worker |
| `slopty-worker` | session manager, mux, fan-out, the orchestration verbs, worker capabilities, listening ports | worker |
| `slopty-tailnet` | the local Tailscale daemon's `LocalAPI`: peers and paths, `whois` and grants, admission policy | all |
| `slopty-tools` | the orchestration verbs as one contract: name resolution, each verb, bulk files, JSON/text views, and the agents' MCP tools | all |
| `slopty-server` | the control plane: worker registry and leases, verb dispatch, the state file, the agents' verbs, the attention ladder and its pushes, the QUIC front end | server |
| `slopty-client` | client session state, the item registry mirror, the layout model, grouping tiles by their facts (`groups`) | client |
| `slopty-settings` | `settings.toml` schema, defaults, loading with fallback, data dir | client |
| `slopty-theme` | design tokens, dark and light variants | client |
| `slopty-ui` | GPUI elements and views; headless `#[gpui::test]` tests drive them through `VisualTestContext` | client |
| `slopty-platform` | process-level platform helpers: keep the process out of App Nap and timer coalescing while a session is live, and raise the user's attention | all |
| `slopty-deploy` | a worker put on another machine over the system `ssh`: the plan, its steps as events, a `Runner` seam, typed failures and how a window says them; the CLI's `worker deploy` and the app's SSH sheet and Update | all |
| `slopty-app` | the app shell shared by macOS and iOS: workspace window, add-worker panel, one link loop per worker, settings | client |
| `slopty-e2e` | app self-test: control-socket wire types, tokio driver, daemon+app harness (the app on the Mac or in the iOS simulator), numeric golden diff, frame-time scenarios (`cargo xtask e2e app\|ios\|smooth\|smooth-ios`), and `slopty-idle-window`, a window the harness owns so a capture target that never draws can be tested without touching anything else on the desktop | dev |
| `slopty-shape` | a UDP relay the client and worker speak QUIC through, with a delay/jitter/loss/rate model below the congestion controller; the degraded link the congestion rulings are measured on | dev |
| `apps/slopty-ptyd` | PTY custodian daemon (LaunchAgent) | worker |
| `apps/slopty-worker` | worker daemon | worker |
| `apps/slopty-server` | server daemon (LaunchAgent, systemd user unit on Linux) | server |
| `apps/slopty` | macOS app: logging, runtime, window options, then `slopty_app::open_workspace` | client |
| `apps/slopty-ios` | iOS static library, called from a UIKit shim: `slopty_ios_did_finish_launching` at launch (logging, the notification delegate), then `slopty_ios_run` once the scene connects; `cargo xtask ios sim [--sim iphone\|ipad]\|device` generates the Xcode project | client |
| `apps/slopty-cli` | `slopty` CLI: worker ctl and install, `add`/`workers`/`forget`, the orchestration verbs (`--json`), `slopty mcp`, raw-mode reference client (`open`/`attach`) | all |
| `xtask` | all scripts (build, gates, bundle, sign, icon from `assets/icon.svg`, `e2e app|ios|worker|screen|input|all` for the self-test and the gated live tests) | dev |

Dependency direction is strictly downward in that table; `slopty-ui` never sees `slopty-worker`.
