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
- ✅ **iOS backend = zed PR #63068's `gpui_ios` crate on top of upstream `801c087`, with the
  PR's gpui-core edits dropped** (2026-09-04, fork branch `slopty`). The cherry-pick conflicted
  in 5 files because upstream had meanwhile absorbed everything the PR changed in `gpui` itself
  (`WindowInsets`/`on_insets_changed`, soft keyboard + `TextInputStateChange`, app lifecycle,
  memory warnings, `PlatformGestures`) and replaced its `TouchGestureArena` with
  `TouchGestureRecognizer` (#63373), which synthesises mouse events from taps so ordinary
  `on_click` handlers work under touch. Ruling: `git checkout HEAD` for every `crates/gpui/src`
  file, keep `gpui_ios`, `gpui_apple` (iOS SDK selection in `build.rs`, iOS-safe atlas and
  renderer) and `gpui_platform` (`cfg(target_os = "ios")` → `gpui_ios::current_platform`). Two
  fixes were needed for the newer API: `Platform::on_quit` takes `FnMut() -> bool`, and
  `TouchEvent` gained `predicted_position`, filled from
  `UIEvent.predictedTouchesForTouch:` for moved touches. Shaders: iOS builds use
  `gpui_apple/runtime_shaders` (compiled by `MTLDevice` at launch) so the pipeline has no
  Metal-toolchain dependency; the toolchain is installed on the build Mac anyway
  (`xcodebuild -downloadComponent MetalToolchain`). Reference implementation for behaviour
  only: `tanlethanh/zed@feat/gpui-mobile` (zedra), cloned under `.research/`.
- ✅ **Zero-copy video in GPUI.** `gpui::surface(CVPixelBuffer)` → `paint_surface` →
  `CVMetalTextureCache` in `gpui_apple/metal_renderer.rs` (verified by reading source). Upstream
  gates it to macOS and stubs `draw_surfaces` on iOS; fork commit `a7cc3f4` lifts every gate to
  `any(macos, ios)` (`core-video` 0.5 builds for both), so the phone paints the decoder's
  `CVPixelBuffer` the same zero-copy way. Checked on macOS, `aarch64-apple-ios-sim` and
  `aarch64-apple-ios`.
- ✅ **iOS link and launch quirks (verified on the iOS 26.5 simulator, 2026-09-04).** Two extra
  things beyond the framework list: `Network.framework` (iroh's `netdev` uses `nw_path_monitor`
  on iOS), and stand-ins for three CGL symbols (`CGLErrorString`, `CGLGetCurrentContext`,
  `CGLTexImageIOSurface2D`) that the `io-surface` crate references from `bind_to_gl_texture`.
  `-Wl,-U,<sym>` lets the link pass but dyld's chained fixups bind at load and the app dies
  with "symbol not found in flat namespace", so `apps/slopty-ios` defines them as `extern "C"`
  functions that `unreachable!()`; nothing on iOS calls them. Dead-code stripping does not
  remove the references. `gpui-kit` widgets default to the light theme: the app forces
  `ThemeMode::Dark`. Phones have no CLI, so `slopty-app` shows a pairing panel (ticket field,
  "Paste & pair" reading `UIPasteboard`, which iOS gates behind a paste-permission alert) and
  the connect loop waits on it; the same panel serves macOS first runs. A phone joining a
  desktop-made canvas would open onto empty space (items sit at desktop coordinates), so the
  first snapshot schedules `fit_when_painted`. Safe area / keyboard come from the fork's new
  `Window::insets()` (`a140f32`). Touch: gpui core's portable recognizer turns one finger into
  taps / pans (scroll events) but has no pinch and ignores a second finger, so a pinch on the
  simulator moved the item instead of zooming; the fork adds a native
  `UIPinchGestureRecognizer` on the Metal view that emits GPUI `PinchEvent`s with incremental
  deltas, the same event the macOS trackpad produces, so the canvas's `capture_pinch` handler
  serves both. Soft keyboard: iOS raises it only for a focused element that registered a text
  input handler, so `TerminalView` implements `EntityInputHandler` (empty ranges, no
  composition preview; committed text goes to the PTY as `TermRequest::Raw`) and
  `TerminalElement::paint` registers it; keys still arrive as key events on both platforms
  (gpui_ios turns `insertText:` into `KeyDown`). Items created from this client are
  `Camera::reveal`ed on the next frame (pan if they fit, else zoom out no further than
  `CARD_ZOOM` and anchor top-left), because the host's `free_slot` places them to the right of
  everything, off a phone's screen.
- 🔬 **Input methods in terminals.** With `prefers_ime_for_printable_keys` left false, macOS
  sends printable keys straight to `key_down`, so Telex/Japanese/… composition never starts.
  Turning it on requires drawing marked text at the cursor (`replace_and_mark_text_in_range`)
  the way Terminal.app and Ghostty do. Not done yet.
- 🔬 **Phone-sized terminals.** A terminal opened from a desktop is wider than a phone; today
  the phone sees it at `CARD_ZOOM`–1× and pans. The architecture's driver/viewer sizing
  (one client owns the PTY size) is the real answer: a phone that becomes the driver resizes
  the PTY to what fits.
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
- ✅ **Remote windows are `surface` elements, verified 2026-09-04.** `slopty-ui::screen::ScreenView`
  wraps the decoder's `CVPixelBuffer` with `core_video::CVPixelBuffer::wrap_under_get_rule` (own
  retain, so the frame `Arc` may drop first) and paints `gpui::surface(buffer).object_fit(Fill)`.
  A Ghostty window streamed from the host rendered pixel-exact through the Metal path with no
  copy. The view keeps only the newest frame; a pump task awaits the client `watch` channels for
  frames and cursor and calls `cx.notify()`.
- ✅ **Stream quality follows painted size.** `ScreenView::set_painted_width` (called from the
  canvas at paint time with the item's on-screen width in device pixels) quantises the wanted
  scale to quarter steps and sends `SetQuality` at most every 400 ms, so zooming the canvas
  re-encodes at a matching resolution instead of downscaling a full-size stream. Video paints
  at every zoom (a thumbnail costs a thumbnail-sized stream); only terminals collapse to cards
  below `CARD_ZOOM`. Changed 2026-09-04 when the phone fitted a desktop layout at 28 % and
  showed "2151 frames" instead of the picture.
- ✅ **Picker is a modal overlay, ⌘O.** `slopty-ui::picker::WindowPicker` lists on-screen windows
  (sorted app → title) and displays from a `Listing`; Escape or a backdrop click dismisses, a row
  click picks. Bound to ⌘O because Raycast owns ⌘⇧N system-wide on the dev Mac (evidence: the
  first binding opened Raycast's clipboard history instead).

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

- ✅ **QUIC datagrams, not raw UDP over a WireGuard mesh** (re-examined 2026-09-04 when the
  status bar showed ~100 ms). A QUIC datagram is one UDP packet plus ~30 bytes of header and one
  AEAD (hardware AES-GCM); WireGuard spends the same per packet on ChaCha20, so "UDP + WG" moves
  the crypto, it does not remove it. Measured: app RTT 0.8 ms on loopback, 0.9–2 ms on the LAN
  direct path; the ~100 ms readings were the *relay* path (n0's aps1 server) while iroh had
  dropped the direct path (see below). slop-desk's transport doc admits its numbers were
  loopback-only and the WireGuard case was never measured. What QUIC buys: reliable streams and
  unreliable datagrams on one connection, migration, NAT traversal and relay fallback for a
  phone off the mesh, and app-level auth instead of "the mesh is the boundary".
- ✅ **Direct-only mode** (`slopty_net::Reach::DirectOnly`; hostd `--direct-only`, every binary
  honours `SLOPTY_DIRECT_ONLY=1`) for hosts and clients on a private mesh (NetBird/Tailscale)
  or one LAN: `RelayMode::Disabled` + `clear_address_lookup()`, mDNS kept. The ticket then
  carries only IP addresses (the mesh address among them) and a relay can never be selected: a
  dropped direct path drops the connection, which the app's reconnect loop turns into a
  ~1 s blip instead of a silent ×50 latency step. Gotchas: `Endpoint::online()` waits for a
  home relay and hangs with relays off, so `endpoint::online` watches `watch_addr()` for the
  first `TransportAddr::Ip` instead; a client that paired in `Anywhere` mode has a stored
  address with a relay URL, so `client::connect` strips relay entries when direct-only (else
  the dial waits on a relay it cannot use). Loopback test `direct_only_pairs_without_a_relay`
  asserts the ticket is relay-free and the selected path is direct. Roaming caveat: with no
  relay and no pkarr the client only knows the addresses in its stored ticket plus mDNS, so a
  host whose mesh IP changes needs re-pairing. Observed 2026-09-04: when the host process is
  killed, the client's lone direct path times out at 15 s, noq logs `failed closing path
  err=LastOpenPath` and keeps it, and the connection only drops at the 45 s idle timeout
  (`IDLE_TIMEOUT`); the top bar shows a stale RTT until then. 🔬 A liveness signal from the
  control stream (host heartbeat message) would let the app show "host unreachable" at ~5 s.
- 🔬 **Path flap under investigation** (2026-09-04): one connection went direct → relay-only for
  43 s → direct while the machine was compiling. iroh's default `BiasedRttPathSelector` always
  prefers a live direct path over relay, so the direct path must have been *closed*, not
  deselected. `slopty_net::endpoint::log_path_events` now logs opened/closed/selected with the
  closed path's final stats on both ends, and both binaries default their log filter to
  `iroh::_events::path=debug`, which carries noq's abandon reason (`TimedOut` = 15 s path idle
  with 5 s heartbeats, `UnusableAfterNetworkChange`, `RemoteAbandoned`). Not reproduced by a
  full `cargo xtask gate` on all cores nor by covering the app window for 70 s. Both processes
  now hold an `NSProcessInfo` latency-critical activity (`slopty-platform::Activity`) as a
  precaution against App Nap / timer coalescing. Next occurrence: read the reason, then rule.

- ⚠️ **Never await iroh's `Endpoint::close` on GPUI's executor.** It uses `tokio::time::timeout`,
  which panics (`Handle::current`) outside a tokio runtime context; the app aborted on every
  disconnect until the close was spawned onto the runtime and joined (crash report
  2026-09-04). Rule: anything from iroh/tokio-time runs via `runtime.spawn`, GPUI tasks only
  await join handles and channels.

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
- ✅ **Client receive path** (`slopty-client::screen`, verified end to end 2026-09-04):
  `HostLink` reads every datagram and a `ScreenRouter` fans out by stream id, keeping a
  512-datagram backlog for streams whose `Opened` has not been processed yet (datagrams beat the
  control stream). One task per stream: reassemble → decode; the reassembler's timers run at
  2 ms while frames are pending and 50 ms when idle; a receiver report goes out every 50 ms.
  The newest decoded frame and the cursor position are `watch` channels: the UI paints the
  latest and never queues video. Dropping the handle unroutes the stream; the caller still
  sends `Close` so the host stops capturing.
- ⚠️ **Unsupported encoder properties on Apple silicon** (M-series, macOS 26.5, observed
  2026-09-04): `AllowOpenGOP`, `MaxFrameDelayCount` and `PrioritizeEncodingSpeedOverQuality`
  return `kVTPropertyNotSupportedErr` under low-latency rate control. They are optional in
  `Encoder::new` and logged at debug; low-latency mode already implies no reordering and no
  frame delay, so nothing is lost.
- ⚠️ **`CMTime` → µs must use 128-bit math**: the host clock is nanoseconds since boot, so
  `value × 1e6` overflows `u64` after a few hours of uptime and silently saturates (found when
  every latency read 0). Fixed in `slopty-codec::cf::micros`; unit-tested with a 12-day value.
- ✅ **420f end to end.** Capture asks SCK for `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`
  (`PixelFormat::Nv12Full`), and the decoder's output attributes pin the same format with
  `kCVPixelBufferMetalCompatibilityKey`, because GPUI's `metal_renderer` asserts that exact
  format (`assert_eq!` in `draw_surfaces`) and samples the two planes as R8/RG8. Any other
  format would either abort the app or need a colour-space conversion on the client.
- ✅ **Present**: `CAMetalDisplayLink`-driven tick; `displaySyncEnabled = false`, drawable count 2;
  present on arrival. 🔬 slop-desk measured vsync-locked = +2 frames at 60 fps; re-measure with
  our harness before ruling.
- ✅ **Cursor** is a separate channel drawn client-side; capture with `showsCursor = false`.
- ✅ **Audio**: SCK `capturesAudio` → `opus` 0.4.0 (verified; `audiopus` is dead) → `objc2-avf-audio`.

## Input

- ✅ **`slopty-input` = `CGEvent` injection, one `Injector` per screen stream** (verified end to
  end 2026-09-04: keys typed into the Slopty app landed in a streamed Ghostty window). Stream
  pixels map back to global points through the target's current bounds (cached 100 ms) and the
  stream's pixels-per-point scale, which `ScreenStream` updates on every quality change.
  Window streams post with `CGEventPostToPid` to the owner; display streams post to the HID tap.
  Keys carry the client's text via `CGEventKeyboardSetUnicodeString`, so host and client
  layouts need not agree; bare modifier keys post as `FlagsChanged`. Needs post-event
  (Accessibility) access: `slopty-hostd` preflights at start-up, warns, and asks once.
- ⚠️ **macOS delivers keyboard events only to the active app.** Events posted to an inactive
  pid queue up and all land the moment the app activates (observed macOS 26.5: four probe keys
  arrived as `echoecho` after activation). So the injector activates the owner
  (`NSRunningApplication::activateWithOptions`) before a button-down or key press when it is
  not active, and the client sends `Focus` when a screen view gains focus. The host's own
  desktop sees that app come to the front; there is no public API around this.
- ⚠️ **`CGWindowListCreateDescriptionFromArray` takes raw `CGWindowID`s as array values**, in a
  `CFArray` built with NULL callbacks, not boxed `CFNumber`s: with numbers it returns an empty
  array (observed macOS 26.5; the docs say "array of window IDs"). `window_bounds` returned
  `None` for every window until this was fixed; the gated `slopty-capture` geometry test pins it.
- ⚠️ **Initialise CoreGraphics before ScreenCaptureKit in a daemon.** `SCContentFilter
  initWithDesktopIndependentWindow:` calls SkyLight, which aborts with
  `Assertion failed: (did_initialize), function CGS_REQUIRE_INIT` when nothing has connected the
  process to the WindowServer yet (a hostd whose first SCK call is a window filter, e.g. a
  client reconnecting with a persisted window item). `slopty-capture` calls `CGMainDisplayID`
  once before any enumerate/resolve.
- ✅ **⌘ chords go to the remote window unless the canvas binds them** (2026-09-04). GPUI runs
  key bindings before key listeners, so ⌘T/⌘N/⌘O/⌘W/⌘0/⌘1/⌘=/⌘- never reach `ScreenView`;
  every other chord (⌘C/⌘V/⌘Z/⌘S/⌘K…) is forwarded with `Mods::SUPER` and no text (GPUI gives
  no `key_char` for ⌘ chords; the injector's virtual keycode carries it). Verified: ⌘K from the
  app reached hostd as `Key { K, Press, SUPER }` and cleared the streamed Ghostty. The view
  tracks pressed keys so a release whose press was eaten by a canvas binding is not forwarded.
  ⌘Q/⌘H/⌘M stay with the app (menu bar). Modifier-only presses are not forwarded (GPUI has no
  key-down for them); the injector sets flags per event instead.
- ⚠️ **An absolute GPUI element without insets sits at its static position.** `ScreenView`
  recorded its bounds from a `canvas().absolute().size_full()` placed *after* the picture, so
  Taffy put it one body-height below the real top: every pointer event mapped ~834 px too high
  and the injector clamped it to the window's top edge (clicks "worked" only by landing on the
  title bar). Fixed with `inset_0()`; the canvas viewport recorder got the same for safety.
- ✅ **Host window resizes are polled, not observed** (2026-09-04). ScreenCaptureKit keeps
  scaling a window into the old output size (a 600×830 window in a 1264×834 stream looked
  2× wide), so `ScreenStream::check_geometry` (one `CGWindowListCreateDescriptionFromArray`
  per stream, every 250 ms from the hostd connection loop) compares the target's bounds with
  the stream's native size, rebuilds encoder + capture at the new size with a keyframe, and
  emits `ScreenEvent::Geometry`. The client's `ScreenView` updates its native size and the
  canvas gives the item the new aspect (width kept, `Place`). Verified: 600×830 → 900×500 changed
  the item to 1264×736 and the picture painted unstretched. No public resize notification
  exists for another app's window short of AX observers, which need the same polling fallback.
- ✅ **Magnify is ignored** for now: there is no public constructor for gesture `CGEvent`s.
- 🔬 Host daemon ships non-sandboxed and Developer-ID signed (App Sandbox blocks
  `CGEventPost`; slop-desk claims macOS 26 drops modifier combos from unsigned processes —
  unverified; verify with a ⌘ chord once the daemon is signed).

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
- ✅ Relay path (verified 2026-09-04 with Claude Code 2.1.260 running `-p` inside a Slopty
  shell): `slopty hook install` registers `{"command": "<abs slopty>", "args": ["hook"],
  "async": true, "timeout": 5}` for 12 events (`SessionStart/End`, `UserPromptSubmit`,
  `Pre/PostToolUse`, `PostToolUseFailure`, `PermissionRequest/Denied`, `Notification`,
  `Elicitation/Result`, `Stop`) in `~/.claude/settings.json` (or `--settings`). Exec form
  (no shell) and `async` so the agent never waits on us. The relay reads `SLOPTY_SESSION` and
  `SLOPTY_HOSTD_SOCKET`, both injected by the host into every session's environment, posts
  `CtlRequest::Hook` and exits 0 whatever happens. `slopty-agent::Tracker` maps hooks to
  `AgentStatus`; `AgentEvent.attention` is true only on entering a blocked state or `Done`,
  so the permission `Notification` that follows a `PermissionRequest` does not alert twice.
  Joining clients get the current table after the canvas snapshot (no proto change: `HostMsg::Agent`
  already existed). Observed cycle: Idle → Working → Tool{Bash} → Working → Done → None.
- ✅ Attention signal: `AgentEvent.attention` → `CanvasEvent::Attention` → `slopty_platform::attention()`,
  which is `AudioServicesPlayAlertSound(kSystemSoundID_UserPreferredAlert)` on macOS (the alert the
  user picked in System Settings, respects their volume) and `AudioServicesPlaySystemSound(kSystemSoundID_Vibrate)`
  on iOS (`objc2-audio-toolbox`, `AudioServices` feature; AudioToolbox.framework linked in the
  iOS spec). No `UNUserNotificationCenter`: it needs a signed bundle with the notification
  entitlement, which the bare macOS binary is not; revisit when the Mac app ships as a bundle.
- 🔬 Attribution without hooks (a `claude` started before `install`, or in a session the host
  did not spawn): no signal today. `SessionSummary.command` could seed an `Idle` badge; the
  transcript tail could recover the rest. Not built.
- ⏸ ACP via `agent-client-protocol` 2.0.0 + `@agentclientprotocol/claude-agent-acp` for structured
  driving — after the PTY path works.
