# Decisions

One entry per ruling. **Verified** means checked against a primary source (SDK header on this
machine, upstream source, crates.io, or our own measurement) on the date given. Claims inherited
from slop-desk are *not* trusted until verified here; where slop-desk was wrong it is noted.

Status: ✅ decided · 🔬 measure before relying on it · ⏸ deferred.

## Platform

- ✅ **Floor macOS 26.5 / iOS 26.5, Apple silicon only.** User decision 2026-09-04. No
  availability checks, no fallbacks for older OS.
- ✅ **Pure Rust.** Scripts are `xtask`. The only non-Rust files are the iOS `main.m` shim (a
  UIKit bootstrap that can't be avoided until `UIApplicationMain` is driven from Rust in the GPUI
  fork), Metal shaders, and the XcodeGen spec that xtask generates.

## UI

- ✅ **GPUI from a fork, pinned.** crates.io `gpui` 0.2.2 is 10 months stale (verified crates.io
  2026-09-04). Fork `aislopware/zed`, branch `slopty`, base `801c087` — the commit gpui-kit 0.6.0's
  `gpui-pre 0.3.1` snapshots (verified: crates.io description "snapshot of zed@801c087").
- ✅ **gpui-kit from a fork.** `longbridge/gpui-kit` v0.6.0 (renamed from gpui-component; Apache-2.0,
  LICENSE file verified). It depends on `gpui-pre` from crates.io; Cargo `[patch]` cannot rename
  packages (tested 2026-09-04: a `package =` key in `[patch]` is silently ignored), so a one-commit
  fork re-points its deps at our zed fork. Upstream sync = rebase that commit.
  Done 2026-09-04: `aislopware/zed` branch `slopty` = 801c087; `aislopware/gpui-kit` branch `slopty`
  = v0.6.0 + one commit (`gpui`, `gpui_platform`, `gpui_web`, `gpui_macros`, `reqwest_client`,
  `sum_tree` → git deps on the zed fork; `reqwest` → zed's `zed-reqwest` git fork).
- ✅ **Forks are git dependencies, not submodules.** `gpui = { git = "…/aislopware/zed", rev = … }`
  pinned by rev; cargo caches the fetch. A zed submodule would put a multi-GB checkout in every
  clone and CI run for four crates we build. To hack on the fork: clone it next to this repo and
  add a `[patch."https://github.com/aislopware/zed"]` block locally (never committed).
- ✅ **iOS backend = zed PR #63068 rebased.** Verified: PR open, 36 files, +4436, base `fd82517`,
  author still iterating (comment 2026-08-26). Cherry-pick onto `801c087` conflicts in 5 files
  (`gestures.rs`, `key_dispatch.rs`, `window.rs`, `platform/test/window.rs`, `gpui_apple/build.rs`)
  because upstream merged touch events (#63373) after the PR. Resolve in the iOS phase. Reference
  implementation that ships today: `tanlethanh/zed@feat/gpui-mobile` used by zedra (cloned under
  `.research/`), 8.5k lines, old `objc` crate, Swift shell — use for behaviour, not code.
- ✅ **Zero-copy video in GPUI.** `gpui::surface(CVPixelBuffer)` → `paint_surface` →
  `CVMetalTextureCache` in `gpui_apple/metal_renderer.rs` (verified by reading source). It is
  `cfg(target_os = "macos")`; our fork lifts the gate for iOS.
- ✅ **Continuous redraw model.** GPUI is reactive; video surfaces call
  `Window::request_animation_frame()` every frame, as zed's GIF and LiveKit views do.
- ✅ **Fonts are bundled, never system-resolved.** JetBrains Mono 2.304 (OFL) + Symbols Nerd Font
  Mono (MIT) ship inside `slopty-ui` and are registered with `TextSystem::add_fonts` at startup;
  every run carries a `FontFallbacks` chain (symbols → Menlo → Apple Color Emoji). Evidence
  2026-09-04: "JetBrains Mono" was not installed on the dev Mac and GPUI silently resolved a
  proportional face, producing broken cell alignment. Bundling also makes iOS identical.
- ✅ **Terminal zoom scales paint, not the grid.** `TerminalElement::zoom` derives cols×rows from
  the *unscaled* item rect and paints at `font_size × zoom`; zooming never resizes the PTY. Shaped
  rows are cached per (content, focus, font size).

## Terminal

- ✅ **VT state on the host, rows on the wire.** See ARCHITECTURE §2. Precedents: mosh, zellij,
  wezterm mux. Rejected: raw bytes + client VT (replay on reconnect, iOS needs the engine, slow
  links pile up bytes).
- ✅ **Engine: libghostty-vt via `Uzaaft/libghostty-rs` ≥ `5988a0b7`.** Verified 2026-09-04:
  crates.io 0.2.1 (2026-07-18), repo pushed 2026-09-02, soundness issue #75 fixed by PR #81
  (null-pointer slice in `ClipboardWrite::contents`, `from_utf8_unchecked` on OSC 52 payload).
  Author is an active ghostty contributor (27 upstream contributions). The C API header says
  "not yet stable" — we pin one ghostty commit and vendor the source (`GHOSTTY_SOURCE_DIR`, no
  network at build time). Only the host builds it. Zig 0.16.0 required (installed).
  Alternative kept as a differential-test oracle: `rio-vt` 0.5.26 (pure Rust, extracted 2026-07).
- ✅ **Rendering is our GPUI element**, not sugarloaf or ghostty's renderer. Glyph shaping via
  GPUI's text system with our own cell layout, sprite glyphs for box drawing, per-row dirty
  tracking (zed's terminal element reshapes every frame; we won't).
- 🔬 **Font metrics follow ghostty's `src/font/Metrics.zig`** (slop-desk claim; re-derive from the
  vendored source when implementing).
- ✅ **PTY custody in a tiny separate daemon** (`slopty-ptyd`), masters handed to hostd by
  `SCM_RIGHTS` (`nix` `sendmsg`/`recvmsg`; `sendfd` dropped — one fewer dependency, and macOS
  has no `MSG_CMSG_CLOEXEC` so CLOEXEC is set by hand either way). ptyd drains the master into a
  bounded ring (4 MiB default) while detached. Verified 2026-09-04 by an end-to-end test
  (`apps/slopty-ptyd/tests/roundtrip.rs`): spawn → detached backlog → attach → connection loss →
  resume → reattach → exit status. macOS detail: `TIOCSWINSZ` on a fresh master fails with
  `ENOTTY` until the slave has been opened once; `Pty::open` sizes through the slave.
- ✅ **Absolute line numbering via a tracked grid ref** (see ARCHITECTURE §2). Verified by engine
  tests: 20 lines through a 4-line scrollback keep `epoch` 0 and contiguous indices.
- ✅ **`KeyCode` is generated from ghostty's key list** (`for_each_key_code!` in slopty-proto) so
  the engine mapping is exhaustive at compile time. Our hand-written W3C list had drifted
  (letters named `KeyA` vs `A`, missing numpad/browser keys).
- ✅ **Terminal type**: `xterm-ghostty` when its terminfo is installed, else `xterm-256color`.
  The app installs ghostty's terminfo on first run (later).

## Transport

- ✅ **iroh 1.1.0** (verified from source in the cargo registry 2026-09-04). QUIC via n0's own
  `noq` 1.2.0 underneath (a quinn fork; **not** the `quinn` crate, so quinn docs/types do not
  apply): streams for control/terminal, unreliable datagrams for media, connection migration for
  Wi-Fi↔cellular, hole punching + relay fallback, E2E encryption keyed by endpoint identity.
  Rejected: slop-desk's raw UDP + "the WireGuard mesh is the security boundary" (phone-anywhere
  needs app-level auth); WebRTC (ICE/SDP dead weight); MoQ (relay/pub-sub shape).
- ✅ **Default features off; `fast-apple-datapath` is banned.** `default-features = false,
  features = ["tls-ring"]`. iroh's default set includes `fast-apple-datapath` (dlsym of private
  `sendmsg_x`/`recvmsg_x`, batched UDP). Measured 2026-09-04 on loopback (`slopty ping`, see
  MEASUREMENTS.md): with it on both ends the app round trip is **53 ms** and QUIC's own RTT
  estimate 48 ms; with it off, **0.8 ms**. The batching path waits to fill batches, which is
  exactly wrong for keystrokes. The feature no longer exists in `slopty-net`; do not re-add it.
- ✅ **LAN discovery** is a separate crate, `iroh-mdns-address-lookup` 0.5.0 (there is no
  `discovery-local-network` feature in iroh 1.x). Behind `slopty-net/mdns`; hosts advertise,
  clients only look up. Wide-area lookup is iroh's `presets::N0` (DNS + pkarr on n0's infra).
- ✅ **Pairing**: `PairTicket { addr, token }` (`iroh-tickets` 1.0.0, base32 string, kind
  `sloptypair`), token single-use with a 10-minute TTL. On redeem the host trusts the client's
  endpoint key in `trust.json` (mode 0600); afterwards the QUIC handshake is the whole
  authentication and `Hello.pair_token` stays `None`. Rejections are sent then linger up to 2 s
  so the client reads `Rejected` before the connection closes (QUIC close discards unread data).
- ✅ **One connection per client**: bidi control stream opened by the client; one uni stream per
  attached session opened by the host, first message `StreamHeader { session }`; datagrams for
  media. Transport config: idle 45 s, keep-alive 5 s, 4 MiB datagram buffers.
- 🔬 **Congestion controller**: noq default (Cubic). Media path runs its own delay-gradient
  bitrate controller on top; revisit BBR once noq marks it stable.

## Video

- ✅ **HEVC Main (8-bit 4:2:0) default, Main10/P010 when the source is HDR.** AV1 is decode-only
  on Apple silicon (Apple spec pages, verified 2026-09-04). No 4:4:4 hardware path exists.
- ✅ **Encoder property set** — verified against `VTCompressionProperties.h` in the macOS 26.5 SDK:
  - `kVTVideoEncoderSpecification_EnableLowLatencyRateControl` = true, in the **encoder
    specification at session creation** (enforces infinite GOP, no reordering, temporal layers).
  - `RealTime` = true, `PrioritizeEncodingSpeedOverQuality` = true, `ExpectedFrameRate` = fps,
    `MaximumRealTimeFrameRate` = 120.
  - `AllowFrameReordering` = false. `AllowOpenGOP` = false (**HEVC defaults to true**; header
    verified). `MaxFrameDelayCount` = 0 (default is `kVTUnlimitedFrameDelayCount = -1`).
  - `AverageBitRate` in **bits per second** (header verified — slop-desk said bytes; wrong) plus
    `DataRateLimits` `[bytes, seconds]`. `ConstantBitRate` is incompatible with both and pads
    frames; not used. macOS 26 adds `VariableBitRate` + `VBVMaxBitRate` + `VBVBufferDuration`
    (header verified) — 🔬 evaluate against AverageBitRate+DataRateLimits.
  - `MaxAllowedFrameQP` / `MinAllowedFrameQP` may return `kVTPropertyNotSupportedErr`; probe.
  - `SpatialAdaptiveQPLevel` must be disabled under low-latency RC (header says so).
  - LTR: `EnableLTR` = true; per frame `ForceLTRRefresh` (CFBoolean) and `AcknowledgedLTRTokens`
    (CFArray<CFNumber>); output attachment `RequireLTRAcknowledgementToken`. Header verified.
  - `MaxH264SliceBytes` is H.264-only; no HEVC intra-refresh property exists. LTR is the recovery.
  - Keys are read from the framework's exported statics (objc2 bindings), never string literals.
- ✅ **Capture** — verified against `SCStream.h` (macOS 26.5 SDK): `minimumFrameInterval`,
  `queueDepth`, `pixelFormat`, `showsCursor`, `captureResolution`, `ignoreShadowsSingleWindow`,
  `includeChildWindows` (14.2+), `captureDynamicRange` (15+), `capturesAudio` +
  `excludesCurrentProcessAudio`. 🔬 slop-desk claims default `queueDepth` is 8 and that macOS 15+
  defaults `minimumFrameInterval` to 1/60 — measure.
- ✅ **FEC**: `reed-solomon-simd` 3.1.0 (NEON), systematic RS per frame, redundancy adaptive
  (~20 % default, Sunshine's number). NACK inside the playout window before LTR refresh.
- ✅ **Packet layout** (`slopty-media`, 2026-09-04): body = 16-byte `FramePrefix` (bitstream
  length, capture µs, LTR token) ‖ bitstream ‖ zero pad, cut into *balanced* fragments (all the
  same even size ≤ 1184 B, so padding ≤ 2 B per fragment and the RS shard size is inferred from
  any fragment); parity indexes follow the data indexes in the same header. The prefix rides
  under parity so a recovered frame recovers its metadata. Max 32 768 data + 255 parity
  fragments per frame; every such pair is a supported RS configuration (tested).
- ✅ **Loss policy** (`Reassembler`): deliver strictly in order; NACK after 3 ms of silence on an
  incomplete frame (fragments leave the host back to back), at most 2 tries one RTT apart; a
  frame that never showed up at all is NACKed whole (`fragments = []`); give up after
  `3 ms + 2·(RTT + 3 ms) + 10 ms`, then `RequestRefresh{last_good}` and ignore everything until
  an IDR or LTR-refresh frame. Parity is decoded only when data fragments are missing. Late
  parity for an already complete frame is dropped unread. 🔬 Timings are first guesses; tune
  against `docs/MEASUREMENTS.md` once the capture path exists.
- ✅ **Redundancy control** (`Redundancy`): parity permille = clamp(2 × EWMA(datagram loss) +
  50, 50, 500), ×1.5 bump when a report shows a frame lost outright. 🔬 Heuristic; measure.
- ✅ **Host pipeline** (`slopty-host::screen`, verified on hardware 2026-09-04): one
  `ScreenStream` per open stream: SCK frame → `Encoder::encode` on the SCK queue → packetize in
  the VideoToolbox output callback → bounded datagram queue (4096) → one pump task per
  connection calling `Connection::send_datagram`. Backpressure drops whole *captured* frames
  when fewer than 256 slots are free and flags the next frame as an LTR refresh; a full queue
  mid-frame drops the rest of that frame (the reassembler NACKs). Encoder sessions are
  `Send + Sync` (VideoToolbox is documented thread-safe); the packet sink holds a `Weak` so an
  encoder never keeps its own stream alive.
- ✅ **Datagram budget**: QUIC's datagram limit starts near 1168 B at the 1200-byte initial MTU
  and grows with path-MTU discovery, so the protocol's 1200-byte `MAX_DATAGRAM` is a ceiling,
  not a promise. The pump reads `Connection::max_datagram_size()` into a shared
  `DatagramBudget`; the packetizer cuts the next frame to it (`Packetizer::set_max_datagram`),
  and a datagram cut before the budget shrank is discarded rather than rejected by QUIC.
- ✅ **Stream ids** are allocated per connection by the host, starting at 1; `WindowId` *is*
  the `CGWindowID` (clients open against a listing they just received, so reuse is harmless).
- ✅ **Cursor channel** samples `CGEventGetLocation` at 120 Hz on the host, re-reads the
  target's bounds at 10 Hz (`CGWindowListCreateDescriptionFromArray` / `CGDisplayBounds`), and
  sends a datagram only when the stream-pixel position or visibility changed.
- ⚠️ **`CMTime` → µs must use 128-bit math**: the host clock is nanoseconds since boot, so
  `value × 1e6` overflows `u64` after a few hours of uptime and silently saturates (found when
  every latency read 0). Fixed in `slopty-codec::cf::micros`; unit-tested with a 12-day value.
- ✅ **Present**: `CAMetalDisplayLink`-driven tick; `displaySyncEnabled = false`, drawable count 2;
  present on arrival. 🔬 slop-desk measured vsync-locked = +2 frames at 60 fps; re-measure with
  our harness before ruling.
- ✅ **Cursor** is a separate channel drawn client-side; capture with `showsCursor = false`.
- ✅ **Audio**: SCK `capturesAudio` → `opus` 0.4.0 (verified; `audiopus` is dead) → `objc2-avf-audio`.

## Input

- ✅ `objc2-core-graphics` 0.3.2 `CGEvent` (`post_to_pid`, `new_scroll_wheel_event2`,
  `keyboard_set_unicode_string`, autorepeat field). Host daemon ships non-sandboxed and
  Developer-ID signed (App Sandbox blocks `CGEventPost`; macOS 26 drops modifier combos from
  unsigned processes per slop-desk — 🔬 verify).

## Tooling (versions verified on crates.io / GitHub 2026-09-04)

- ✅ Rust 1.98.1 pinned; edition 2024; resolver 3; `[workspace.lints]` with clippy
  all/pedantic/nursery/cargo + curated restriction lints; `panic = "unwind"` everywhere
  (`panic = "abort"` turns any ObjC exception crossing objc2 into a process abort).
- ✅ nextest 0.9.143 · insta 1.48 · proptest 1.11 · cargo-mutants 27.1 · cargo-llvm-cov 0.9 ·
  cargo-deny 0.20.2 · cargo-shear 1.13.4 · cargo-hack 0.6.45 · cargo-semver-checks 0.50 ·
  typos 1.50.1 · taplo 0.10 · prek 0.5.2 · bacon 3.25 · samply 0.13.1 · tracing-tracy 0.12.
- ✅ **Releases from Conventional Commits**: `committed` 1.1.11 lints every message (commit-msg
  hook via prek, and `cargo gate` over the range since the last tag); `git-cliff` 2.14.1 computes
  the next version (`--bumped-version`, pre-1.0 rules: breaking → minor, feat → patch) and writes
  `CHANGELOG.md`; `cargo xtask release` glues them and tags `vX.Y.Z`. Rejected: cocogitto
  (last release 2026-03, overlaps both), release-plz / cargo-release (crates.io-centric; nothing
  here is published).
- ✅ Miri only for pure crates (cannot cross objc2 FFI); cargo-careful + ASan/TSan nightly lane for
  the rest; `leaks --atExit` for framework wrappers (Valgrind does not exist on Apple silicon).
- ✅ No mold/lld on macOS (Apple's ld-prime is competitive; mold's Mach-O port is commercial).
- ✅ objc2 0.6.4 / objc2-* 0.3.2 / block2 0.6.2 / dispatch2 0.3.1, pinned exact, `CFRetained`
  ownership in the type system; one counted `from_raw`/`retain` admission per wrapper crate.

## Canvas

- ✅ **Infinite canvas is the product.** slop-desk built and retired one; its reasons were
  AppKit-specific (a libghostty surface cannot live under a scaled ancestor) and product-fit
  (undiscoverable, not keyboard-navigable). We own the renderer, so zoom is real, and we add
  keyboard navigation, zoom-to-fit/selection, grid snap, arrange-by-repo and a minimap (kolu).
- ✅ Kind-aware culling: terminals keep state and stop painting off-screen; video pauses decode.
- ✅ **Host-authoritative document, optimistic client.** `slopty-host::CanvasStore` owns the
  document (JSON at `<data>/canvas.json`, atomic rename, serialised writers), validates every
  `CanvasOp` (finite, clamped geometry; unknown ids rejected), bumps a version and broadcasts a
  `CanvasSync::Delta { by }`. Clients apply their own ops immediately and recognise the echo by
  `by == me`. A new session gets a terminal item automatically (right of the rightmost, snapped
  to 16 units) so every client sees it in the same place; a closed session removes its item.
- ✅ Snapshot is pushed right after `HelloAck` on the control stream; no client request needed.

## Claude Code

- ✅ Hooks are the authoritative signal (33 events as of 2026-09; we register the status-bearing
  subset), transcript JSONL is read-only context, screen heuristics corroborate only.
- ⏸ ACP via `agent-client-protocol` 2.0.0 + `@agentclientprotocol/claude-agent-acp` for structured
  driving — after the PTY path works.
