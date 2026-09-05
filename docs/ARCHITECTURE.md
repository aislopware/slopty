# Slopty architecture

Slopty is a remote-coding workstation: a macOS **host** exposes shells and windows; macOS and iOS
**clients** show them on one infinite canvas, mixed freely, with Claude Code agents surfaced as
first-class objects. Everything is Rust. Floor: macOS 26.5 / iOS 26.5, Apple silicon.

This file is the map. Rulings and their evidence live in [DECISIONS.md](DECISIONS.md).

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
| Media | unreliable datagrams (RFC 9221) | HEVC fragments + Reed–Solomon parity, Opus audio, cursor position/shape |

## 2. Terminal: the VT engine lives on the host

The host runs `libghostty-vt` against the real PTY and ships **rendered rows**, not bytes
(mosh / zellij / wezterm-mux model). Consequences:

- Reconnect and multi-client are cheap: a joining client receives the current screen plus a
  scrollback window; nothing is replayed.
- A slow link never falls behind a fast program: the host coalesces to one diff per tick.
- The client needs no VT engine at all. iOS never builds Zig.
- The client keeps a **line cache** (absolute line numbers) so scrollback scrolls locally; missing
  ranges are fetched, prefetched around the viewport.
- **Prediction**: the client applies mosh-style speculative echo for printable keys, confidence
  gated on measured RTT, reconciled against the next authoritative diff (see `slopty-predict`).
- Terminal size is owned by one **driver** client (first to attach; anyone can take it with the
  "take" pill, which also fits the item to their viewport); viewers see the driver's grid.

**Absolute line numbering.** Clients cache scrollback by `LineIndex` (absolute, monotonic). The
engine keeps a libghostty *tracked grid ref* pinned to the newest active row and re-derives the
index of screen row 0 after every write, so host-side eviction never shifts indices. When
numbering cannot survive — reflow on resize, RIS, alternate-screen switch, a single write that
scrolls past the whole scrollback — the frame's `epoch` bumps and clients drop their cache.
Returning from the alternate screen restores the parked primary anchor, so the cache survives
`vim`/`less` round trips.

**PTY custody.** `slopty-ptyd` spawns the child (own session, slave as controlling tty), keeps
the master, and drains it into a bounded ring while no host holds it. `Attach` pauses the reader
(handshake through a `watch` pair so the fd is never read by two parties), ships the ring
contents plus the master over `SCM_RIGHTS`, and hostd reads the fd directly from then on —
no relay hop. Losing the hostd connection resumes draining; the child never blocks.

Crates: `slopty-engine` (trait + libghostty-vt backend), `slopty-grid` (frame model, diff, cache),
`slopty-predict`, `slopty-pty` (openpty/spawn, async master, ptyd protocol + client),
`slopty-host`; app `slopty-ptyd`.

## 3. Remote windows: Parsec-class pipeline

```
SCStream(window | display, 420v/P010, minimumFrameInterval, queueDepth=2, showsCursor=false)
  → VTCompressionSession(HEVC, EnableLowLatencyRateControl @ creation, RealTime,
                         AllowFrameReordering=false, AllowOpenGOP=false, MaxFrameDelayCount=0,
                         EnableLTR, MaxKeyFrameInterval=∞, AverageBitRate + DataRateLimits)
  → packetize (≤1200 B datagrams, 16 B header) → reed-solomon-simd parity per frame
  → iroh datagrams                                      ── client acks LTR tokens, NACKs, telemetry
client: reassemble/recover → VTDecompressionSession(RealTime) → CVPixelBuffer (IOSurface)
  → gpui surface (CVMetalTextureCache, zero copy) → present on arrival (vsync off)
```

Loss recovery order: FEC (free) → NACK inside the playout window → `ForceLTRRefresh` (small
P-frame from an acked LTR) → IDR only when no acked LTR exists. Cursor is a separate low-rate
channel drawn client-side, so pointer latency is one RTT, not one video pipeline.

Crates: `slopty-capture` (SCK streams, shareable content, pointer/bounds queries),
`slopty-codec` (encode half is `cfg(macos)`), `slopty-media` (`Packetizer` → datagrams + parity +
retransmit history; `Reassembler` → in-order frames, NACK/refresh `Action`s, `ReceiverReport`;
`Redundancy` → parity ratio; pure, no clocks, property tested), `slopty-host::screen`
(`ScreenStream`: capture → encode → packetize into a bounded queue; `DatagramBudget` tracks the
path's datagram limit; cursor sampler; input injection), `slopty-input` (client
`ScreenInput` → `CGEvent`, posted to the owning pid for windows or the HID tap for displays;
activates the owner before clicks and keys because macOS only delivers keyboard events to the
active app). `slopty-hostd` owns one datagram pump
per connection and maps `ScreenRequest`s onto the streams it opened for that client.

## 4. Canvas

One infinite 2D plane per workspace (kolu model). Items: terminal, remote window, agent card,
note. Camera `{x, y, zoom}`; zoom is real (we own the renderer), with semantic LOD: full terminal
at ≥ 0.6×, summary card (title, last lines, agent state) below. Off-screen terminals keep their
line cache and stop painting; off-screen video pauses decode (kind-aware culling). Layout is a
document synced through the host so every client sees the same canvas (`slopty-host::canvas`
owns it, `slopty-client::canvas` mirrors it with optimistic local ops, `slopty-ui::canvas` draws
it: two-finger scroll pans, pinch / ⌘-scroll zooms about the pointer, title bar drags, corner
grip resizes, ⌘T/⌘⇧N/⌘O/⌘W/⌘0/⌘1/⌘=/⌘- are the keyboard surface). Notes (⌘⇧N) are edited
in place (`slopty-ui::note`) and their text lives in the document. Remote-window items are
created from the picker (⌘O); `reconcile_screens` opens a stream for every window/display item
that lacks one and closes streams for items that disappeared, so the document, not the UI, is
the source of truth for what is being streamed.

## 5. Agents

Claude Code only, for now. Signals in precedence order: hooks (delivered to `slopty-hostd` over
its control socket by `slopty hook`, the relay Claude Code runs for each event) → JSONL
transcript tail → terminal title/OSC → foreground-process presence. Only the first is built.

The host spawns every session with `SLOPTY_SESSION=<id>` and `SLOPTY_HOSTD_SOCKET=<path>`;
the relay forwards its stdin plus those two to the daemon as `CtlRequest::Hook` and always
exits 0. `slopty-agent` keeps one `Tracker` per session that turns the hook stream into
`AgentStatus` (`Idle`, `Working`, `Tool`, `Blocked{Permission|Question|Elicitation|IdlePrompt}`,
`Done`) and flags `attention` on the transitions worth a sound. The daemon broadcasts each
change as `HostMsg::Agent` and replays the table to joining clients. The canvas shows the status
as a pill in the terminal's title bar and outlines the item when the agent needs the human.
`slopty hook install|uninstall|status` manage the registration in `~/.claude/settings.json`.
Later: ACP (`agent-client-protocol`) for structured control.

## 6. UI

GPUI (fork: `aislopware/zed` branch `slopty`, pinned to the zed commit gpui-kit tracks) plus
gpui-kit (fork: `aislopware/gpui-kit`, one commit re-pointing deps). The iOS backend is zed PR
#63068's `gpui_ios` on top of the pin, extended in the fork for the surface element (zero-copy
video), `Window::insets()` (safe area, keyboard) and a native pinch recognizer; one finger
taps and pans through gpui core's touch recognizer, two fingers pinch-zoom, both landing in the
same canvas handlers the Mac uses.
Design tokens in `slopty-theme` (Warp-like: surface ladder, hairline borders, one accent).
`slopty-ui::screen::ScreenView` paints a remote window as a `gpui::surface` from the decoder's
`CVPixelBuffer` (zero copy), draws the host cursor from the cursor channel, forwards mouse, scroll
and keys (including ⌘ chords the canvas does not bind) as `ScreenInput`, and asks the host for
a stream scale matching its painted width. When the host window changes size, hostd notices
within 250 ms, restarts the stream at the new size and sends `Geometry`; the canvas re-aspects
the item.

**Pairing.** Unpaired installations show a pairing panel instead of the canvas: paste the
ticket `slopty host ticket` printed on the host (a "Paste & pair" button reads the clipboard,
which is the only practical path on a phone). `slopty_app::net::pair_host` redeems it the same
way the CLI does and the connect loop resumes.

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
| `slopty-theme` | design tokens | client |
| `slopty-ui` | GPUI elements and views | client |
| `slopty-app` | the app shell shared by macOS and iOS: workspace window, pairing panel, host link loop | client |
| `apps/slopty-ptyd` | PTY custodian daemon (LaunchAgent) | host |
| `apps/slopty-hostd` | host daemon | host |
| `apps/slopty` | macOS app: logging, runtime, window options, then `slopty_app::open_workspace` | client |
| `apps/slopty-ios` | iOS static library (`slopty_ios_run` called from a UIKit shim); `cargo xtask ios sim\|device` generates the Xcode project | client |
| `apps/slopty-cli` | `slopty` CLI: host ctl, pairing, raw-mode reference client (`open`/`attach`), hook relay | host |
| `xtask` | all scripts (build, gates, bundle, sign) | dev |

Dependency direction is strictly downward in that table; `slopty-ui` never sees `slopty-host`.
