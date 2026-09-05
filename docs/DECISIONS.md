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
- ✅ **Mouse selection, ⌘C, ⌘V.** Left-drag selects cells; the selection is stored as
  absolute line indices (`Selection { anchor, head }`, both inclusive) so it survives scrolling
  and is painted as a quad under the text from the same prepaint pass. When the program has
  mouse tracking on, clicks are reported to it instead and ⇧-drag forces a selection (what
  every terminal does). ⌘C copies the selected cells with trailing blanks trimmed per line
  (lines missing from the scrollback cache copy as empty); ⌘V sends the clipboard as
  `TermRequest::Paste`, which the host brackets when the program asked for it. Any key, a
  resize or an epoch change (reflow, alt screen) clears the selection. Bindings live in the
  "Terminal" key context (`slopty_ui::terminal::key_bindings`). Double-click selects the word
  (run of non-blank cells), triple-click the line. On touch a plain drag pans the canvas (GPUI
  recognises it as a scroll), so the terminal element claims GPUI's *long press* instead
  (`window.prevent_default()` at `Started`): hold to select the word under the finger, keep
  holding and move to extend, as iOS text views do.

- ✅ **Input methods in terminals.** In the fork, `prefers_ime_for_printable_keys` follows
  `accepts_text_input` (true for `TerminalView`), so with the input handler installed macOS
  routes printable keys through the active input method. `TerminalView` keeps the marked text
  (`replace_and_mark_text_in_range`/`unmark_text`), the element draws it underlined at the
  cursor over the host's cells and hides the block cursor meanwhile (what Terminal.app does),
  and the commit arrives through `replace_text_in_range` as raw bytes. Covered by the GPUI test
  `composition_is_previewed_then_committed_as_raw_bytes` (gpui `test-support`). A live Telex
  run was not possible from the automation session (see `cargo xtask ime` below).
- ✅ **`cargo xtask ime [id] [--all]`** lists/selects macOS input sources through HIToolbox's
  `TISSelectInputSource` (Carbon), for input-method testing. Caveat found 2026-09-05: the
  agent's shell runs in a `Background` launchd session (`launchctl managername`), where TIS
  refuses to select (`paramErr -50`) and synthetic ⌃Space never reaches the hotkey; the tool
  works from a real Terminal in the Aqua session.
- ✅ **Phone-sized terminals.** The host places every new terminal at `TERMINAL_SIZE`
  (720×440). When the item is one *we* opened, `CanvasView::fit_to_viewport` immediately
  proposes a `Place` that clamps it to the viewport minus `GAP` at zoom 1, so a phone gets a
  phone-wide terminal and, being its driver, a PTY of that width; desktop viewports exceed the
  default and are untouched. Terminals opened elsewhere are still viewed zoomed out; the
  driver/viewer hand-off is the "take" pill below.
  Verified on the iOS 26.5 simulator 2026-09-05: "+ shell" on the phone opens a 43×22 PTY
  shown at 100 % across the phone's width. Landscape (both directions in the spec) rotates the
  GPUI window with the keyboard; nothing else was needed.
- ✅ **The opener drives.** Every client attaches to a new terminal the moment the
  `SessionOpened` broadcast reaches it, so the phone could become driver of a shell the desktop
  just opened (observed 2026-09-05: a fresh desktop shell wearing the "take" pill). hostd calls
  `SessionHandle::reserve_driver(opener)` before broadcasting; the first attach no longer
  decides. Test `reserved_driver_beats_the_first_attach`.
- ✅ **Driver hand-off is explicit: the "take" pill.** A terminal whose PTY follows another
  client's size shows "take" in its title bar (`TerminalState::driving` is false).
  `CanvasView::take_over` sizes the item for this viewport (clamped between the default
  `TERMINAL_SIZE` and the viewport minus `GAP`), sends `TermRequest::Drive`, focuses and
  reveals it. Item geometry is shared canvas state, so the other client sees the terminal
  shrink or grow: that *is* the signal that someone else drives it, and their own "take"
  brings it back. Rejected: tmux-style "latest input drives" (a phone typing one command would
  keep shrinking the desktop's terminal) and per-client geometry (a second document model).

- ✅ **iOS key bar.** The soft keyboard has no esc/tab/ctrl/arrows, so the iOS app
  (`KEY_BAR = cfg!(target_os = "ios")`) draws a 40 pt row above the keyboard with esc, tab, ⌃,
  ←↑↓→, `-`, `/`, `|`, `~`. Keys go through `TerminalView::press`, the same path as hardware
  key events; ⌃ is *sticky*: it arms `set_sticky_control` and the next key (bar or typed
  character, including text arriving through `replace_text_in_range`) is sent with the control
  modifier, then it disarms. Covered by `sticky_control_applies_to_the_next_key_only`; verified
  on the simulator 2026-09-05 (⌃ + `c` interrupts `sleep 30`, arrows recall history). The bar
  only renders while a terminal is the canvas's active item. Its last key is the clipboard:
  "copy" while the terminal has a selection (long-press selects; a plain drag pans the
  canvas), "paste" otherwise, since the phone has no ⌘C/⌘V.
  A remote window gets its own bar (`SCREEN_BAR_KEYS`, `CanvasView::active_key_target`):
  esc, tab, sticky ⌃ and ⌘, arrows, `/`, copy, paste. `ScreenView` implements
  `EntityInputHandler` too, so the soft keyboard rises over a window; each committed character
  becomes a press + release `ScreenInput::Key` carrying the character as `text` (a modified key
  carries none, or the host would insert the letter as well as run the chord). Composition
  (marked text) is held and only the committed string is sent. Windows on a phone were
  view-and-touch only before this (2026-09-05). Found on the way: a press on a window never
  made it the *active* item, because `ScreenView::mouse_down` stops propagation (so the canvas
  does not pan) before the item container's activate handler runs; the view now emits
  `ScreenViewEvent::Pressed` and the canvas activates from that. The workspace also observes
  the canvas entity, since the key bar follows the active item and nothing else re-rendered
  the chrome when it changed. A long press on a window is a right click on the host, which
  exposed a second host bug: a right click posted with `CGEventPostToPid` reaches
  `rightMouseDown` but AppKit's context-menu tracking never opens the menu (Ghostty, macOS
  26.5; a real click opens it). Right-button events now go through the HID tap even for
  window streams — the host pointer moves for those, the price of a menu — and Ghostty's
  menu opens from the Mac app and from a phone long press (verified 2026-09-05).
- ✅ **Stream stats overlay** (2026-09-05). Parsec's overlay is how people judge a remote
  desktop, so ⌘⇧I draws one per window from the client's own counters (`ScreenStats` gained
  `bytes`; rates are deltas over ~1 s, computed in `ScreenView::hud_text` on render since a
  window re-renders on every frame anyway). Global, not per window, and it carries over to
  windows opened later. First reading on loopback: a 450×250 @0.50 idle Ghostty window ran
  54 fps at 0.11 Mb/s, RTT 1.7 ms, zero loss.

- ✅ **Notes are shared text, last writer wins.** `ItemKind::Note { text }` was already in the
  document; the client now edits it in place with gpui-kit's `TextareaState` (`NoteView`).
  Edits go to the host as an `Upsert` of the whole item after a 400 ms typing pause and on
  blur; a remote change is applied only while this client is not editing. No merge: notes are
  short and the canvas has one author at a time in practice. ⌘⇧N / "+ note" places a 320×240
  note in the next free slot and reveals + focuses it at once: the document applies our op
  optimistically, and keying that off the host's echo failed on the phone, where a frame
  rendered (and created the editor) before the echo arrived. Zoomed-out cards show the first
  non-empty line. Verified on macOS and the iOS simulator 2026-09-05 (text persisted in
  `canvas.json`).

- ✅ **Minimap is a painted overlay, not elements.** A 160×100 box in the bottom-right
  corner (`render_minimap`) fits the union of every item rect and the viewport, paints each
  item as a block (`fill`, accent for the active one) and the viewport as an `outline`, all in
  one GPUI `canvas` element: zero layout cost per item, and it scales to hundreds of items.
  Mouse-down/drag inside it centres the camera on the pointer (`Drag::Minimap`); the box's
  mapping from the last prepaint is kept for hit-testing. Hidden while the canvas is empty.
  Verified on macOS 2026-09-05.

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
- ✅ **Congestion controller**: noq default (Cubic); BBR3 measured 2026-09-05 (MEASUREMENTS.md),
  revisit once noq marks it stable. The media path runs its own bitrate controller on top
  (see Video, "Adaptive bitrate").

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
  (`IDLE_TIMEOUT`); the top bar showed a stale RTT until then. ✅ Liveness now comes from
  QUIC itself, no protocol message: the app samples `ConnectionStats.udp_rx.datagrams` once a
  second (`HostLink::received_datagrams`); keep-alive pings make a live host send something
  every 5 s, so a counter that stands still for `SILENCE_WARN` = 8 s turns the RTT readout into
  a yellow "host silent Ns". Verified 2026-09-05 by `SIGSTOP`ping hostd. Since the same day the
  app also *gives up* at `SILENCE_DROP` = 15 s (three missed keep-alives, noq's own path bar):
  `HostLink::abandon` closes the QUIC connection, the control reader reports `Disconnected`,
  and the normal reconnect loop runs, so a restarted host is back ~2 s after it answers instead
  of after the 45 s idle timeout. Measured with a frozen hostd: dropped at 15.2 s, reconnected
  2 s after `SIGCONT`. Reading the logs afterwards: a killed app's *old* connection still shows
  up on hostd as "connection lost: timed out" 45 s later; that is the stale one, not the new.
  Driving note: cliclick `kp:return` never reaches the app (osascript `keystroke return` does).
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

- ✅ **A dying connection detaches only its own sinks** (2026-09-05). Symptom on the simulator:
  ~45 s after relaunching the app, every terminal stopped updating while the host kept running
  the typed commands. Cause: the relaunched app reconnects under the same `ClientId`; the old
  QUIC connection idles out (`IDLE_TIMEOUT`) later and hostd's cleanup called
  `Host::client_gone(client)`, which detached *every* viewer with that id, i.e. the new
  connection's. Rule: viewer eviction is scoped to the connection that attached it:
  `SessionHandle::detach_sink(client, &sink)` removes a viewer only if `Sender::same_channel`
  matches, and `Peer::drop` in hostd uses it; `client_gone` is gone. Regression test
  `stale_connection_detach_keeps_the_reconnected_viewer`.

- ✅ **Terminal search runs on the host** (2026-09-05). The client caches at most 20k lines and
  the host retains 50k, so searching the client cache would miss most of the history or pull
  megabytes on every keystroke. `TermRequest::Search { needle, max }` answers with
  `TermEvent::Matches { needle, total, matches }`: every hit counted, the newest `max` (5 000)
  listed as `(line, col, len)` in cells. The engine renders the grid as plain text with
  libghostty's `Formatter` (plain, trim, selection over every row): one line per row, interior
  blank rows kept, trailing blank rows dropped, 0.36 ms for 904 rows (MEASUREMENTS.md), ~20×
  cheaper than the per-cell `lines()` walk. Columns come from ghostty's `grapheme_width`, so a
  hit paints exactly over its cells (wide characters count two). Smart case (no upper-case
  letter → case-insensitive) with a char-for-char fold so indices stay aligned; hits never span
  a soft-wrapped row. Rejected: libghostty-vt has no text search of its own (its "search"
  functions are word-selection helpers).
  UI: the field is a gpui-kit `Input` inside a `TerminalSearch` key context, so `escape` is
  bound there only and Esc in the grid still reaches the program; `TerminalView::key_down`
  also drops keys while that field is focused. Verified on macOS: 123 hits, stepping into
  history scrolls the hit to the viewport centre, Esc/✕ hand the keys back.
- ✅ **Regex search is a flag on the same request** (2026-09-05, protocol 5).
  `TermRequest::Search { needle, max, regex }` compiles the needle with the `regex` crate
  (1.13.1, linear-time, `size_limit` 1 MiB so a pathological pattern cannot blow up the host);
  smart case becomes a `(?i)` prefix, so plain and regex mode agree on casing. Hits are
  reported in chars and then mapped to cells exactly like plain hits; empty matches (`a*`)
  are skipped rather than painting zero-width highlights. An invalid pattern answers
  `TermEvent::SearchInvalid { needle, message }` instead of an empty `Matches`, so the client
  can tell "no hits" from "bad regex" (the bar shows `bad regex`). The `.*` toggle in the
  search bar restarts the search in the other mode and is lit with the accent when on.
  Verified on macOS: `item[13]` plain → none, regex → 4/4, `item(` → bad regex.
- ✅ **Scrollback is bounded by lines, not libghostty's 10 KB byte default** (2026-09-05).
  `Terminal.zig` defaults `max_scrollback_bytes` to 10_000; the engine had only raised the
  line limit, so every session kept about one page (~900 rows). `set_scrollback_max_bytes(None)`
  in the constructor; tests `the_line_limit_governs_retained_history` (20k of 20k kept) and
  `history_is_pruned_near_the_line_limit` (page-granular, bounded).

- ✅ **NACK and refresh requests are datagrams, not control-stream messages** (2026-09-05).
  Measured on the Wi-Fi → mesh path (MEASUREMENTS.md, "screen stream over the mesh"): loss is
  bursty (a typical gap takes 3–6 of a frame's 5–7 fragments, parity included, so FEC recovered
  2 of 29 gaps), and the frames the receiver gave up on had two NACKs out and *nothing* back
  within the 70–83 ms deadline while the host had the frames in its history. The control
  stream is ordered: once the packet carrying a NACK is lost, every retry queues behind it
  until QUIC's loss timer (a PTO, tens of ms on Wi-Fi) retransmits it, so the receiver's own
  retries cannot help. `slopty_proto::screen::Feedback { Nack, Refresh }` now goes
  client → host as a QUIC datagram (postcard body, no header; the host reads datagrams only
  for this), each one standing alone; a fragment list that would not fit a datagram degrades
  to "whole frame". Reports stay on the control stream (they are periodic and idempotent).
  Protocol 3.
- ✅ **Loss deadlines only run while the link is flowing** (2026-09-05). With NACKs as
  datagrams the give-ups did not go away, and the host side explained why: hostd now logs the
  selected path's `cwnd`/`congestion_events`/`lost_packets` and the datagram send-buffer
  headroom on every NACK — QUIC reported **0 lost packets** for the whole run, cwnd wide
  open, buffer empty, and the client's two NACKs for a "lost" frame reached the host 20 ms
  *after* the client had already asked for a refresh. So the Wi-Fi/mesh path does not drop;
  it **stalls** for 100–300 ms and then releases everything at once (both directions), and a
  wall-clock deadline turned every stall into a refresh (IDR) plus dropped frames. Rule in
  `Reassembler::tick`: a NACK is retried only if something arrived since the last one, and a
  frame past its deadline is lost only if newer datagrams are still coming in
  (`now - arrived_at < deadline`); an outright stall is waited out up to `Config::max_hold`
  (500 ms) — a refresh could not cross it either. When the stall clears (a silence of at
  least `Config::stall_gap`, 50 ms, or one NACK round trip if longer) every pending frame's
  clock restarts: the NACK written into the stall only left with the release, so its answer
  is a round trip away from *now*; the first version without this still lost a frame in the
  very tick the burst arrived. The 50 ms floor matters: at LAN round trips the NACK gap is a
  few ms, and without the floor every 17 ms inter-frame gap would count as a stall. Tests
  `a_stalled_link_holds_the_frame_until_it_moves_again`,
  `a_stall_restarts_the_nack_clock_when_the_link_resumes`,
  `a_stall_longer_than_max_hold_gives_up`. Cost: on a *still* screen a genuinely dropped tail
  waits the full 500 ms before the refresh (nothing newer arrives to prove the drop).
- ✅ **hostd binds a fixed UDP port** (2026-09-05): `slopty-hostd --port` / `SLOPTY_PORT`,
  default `slopty_net::endpoint::HOST_PORT` (45550), IPv4 required, IPv6 best effort. Found
  when a hostd restart stranded the paired MacBook: with `--direct-only` a client on another
  subnet knows the host only by the ticket's `ip:port` and cannot hear mDNS, so a random port
  per launch meant re-pairing after every restart. A port in use is a hard error (a silent
  fallback to a random port would bring the problem back); tests pass `--port 0`.

- ✅ **BBR3 congestion control on the QUIC connection** (2026-09-05). Cubic (noq's default)
  cut the host's window to 13–20 KB after 5–9 real losses in 15 s on the Wi-Fi/mesh path;
  at a 10 ms round trip that caps the stream near 10 Mbit/s, a third of the 30 Mbit/s target,
  and media datagrams sit in the send buffer waiting for the window. noq ships BBR3
  (`noq_proto::congestion::Bbr3Config`, a direct workspace dependency since iroh does not
  re-export the module); `transport_config()` installs it for both roles. See
  MEASUREMENTS.md for the before/after window sizes.
- ✅ **Refresh repeats back off** (2026-09-05). A target that never produces a frame (a hidden
  window) left the receiver in "need refresh", re-asking every `refresh_repeat` + 2 rtt — 79
  requests in 10 s. Each unanswered repeat now doubles the wait up to `refresh_repeat_max`
  (2 s); any video datagram resets it, so a live stream still recovers at the fast cadence.

- ⚠️ **Never await iroh's `Endpoint::close` on GPUI's executor.** It uses `tokio::time::timeout`,
  which panics (`Handle::current`) outside a tokio runtime context; the app aborted on every
  disconnect until the close was spawned onto the runtime and joined (crash report
  2026-09-04). Rule: anything from iroh/tokio-time runs via `runtime.spawn`, GPUI tasks only
  await join handles and channels.
- ⚠️ **One iroh endpoint per process** (found by the app self-test 2026-09-05). The app used to
  bind an endpoint to pair, close it, and bind a second one with the same secret key to
  connect; the second dial hung until the step timeout (the host never saw its packets;
  iroh's discovery/relay state for that key was still the old endpoint's). `slopty-app::net`
  keeps a process-wide `OnceCell<Endpoint>` used by both `pair_host` and `connect_to`, and
  never closes it.

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
- ✅ **Adaptive bitrate** (`slopty_media::RateController`, 2026-09-05). The client's
  `Quality::bitrate_bps` is a ceiling, not a rate: a stream starts at min(ceiling, 12 Mbit/s)
  and the host judges every 10 receiver reports (~0.5 s at the client's 50 ms cadence).
  Overuse — datagram loss > 2 %, present queue ≥ 3, or hold p95 > 60 ms — cuts to 75 % and
  holds 4 decisions; a clean window (loss ≤ 0.5 %, queue ≤ 1, hold p95 ≤ 25 ms) grows by an
  eighth (≥ 0.5 Mbit/s); everything else stays. On top, the selected QUIC path's window caps
  the target at 90 % of `cwnd × 8 / rtt` (`slopty_net::endpoint::selected_path`): datagrams are
  congestion-controlled, so sending past cwnd only fills the host queue (`queue_full`).
  Applied with `VTCompressionSession` `AverageBitRate` in place (no encoder rebuild, no IDR);
  a `SetQuality` or resize re-clamps under the new ceiling and keeps the controller's place.
  Floor 1 Mbit/s. Rejected: GCC's Kalman delay-gradient estimator — the receiver already
  measures hold time and queue depth directly, which is the same signal without the filter;
  and TWCC-style per-packet feedback, which the 50 ms report already approximates.
  `ScreenStats.bitrate_bps` shows the last target in the close log. First mesh run in
  MEASUREMENTS.md: the cap took the start from 12 to 9 Mbit/s and a stalling afternoon link
  drove the target to the floor.
- ⏸ Stall-aware bitrate: a Wi-Fi stall looks like loss to the controller and gets a cut it
  does not deserve. Carry the reassembler's `flowing` flag in `ReceiverReport` (protocol bump)
  and freeze the controller while the path is stalled.
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

## Audio

- ✅ Opus via Apple's `AudioConverter` (`kAudioFormatOpus`), no libopus: the encoder exists on
  macOS 26.5 (verified 2026-09-05: `encodes_and_decodes_a_tone` round-trips a 440 Hz tone;
  the first packet comes back 120 frames short, Opus pre-skip) and the decoder on both
  platforms. One fixed configuration, 48 kHz stereo float interleaved, 960-frame packets at
  96 kb/s, so nothing about the format travels on the wire. Playback is an `AudioQueue` with
  three 20 ms buffers refilled from a mutex-guarded ring (≤200 ms; underrun pads silence and
  counts, overrun drops the oldest). The input-proc pattern: the proc hands its one slice and
  then returns a private status (`'SLOP'`) with zero packets, which `FillComplexBuffer`
  surfaces and the caller treats as "done".
- ✅ Capture rides the video `SCStream` (`capturesAudio`, `sampleRate 48000`, `channelCount 2`,
  `excludesCurrentProcessAudio`) with a second stream output of type `Audio` on the same
  serial queue. `CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer` returns
  `kCMSampleBufferError_ArrayTooSmall` (-12737) unless the list is sized by a first call with
  a null list, so the code asks for the size, allocates 8-byte-aligned words, then fetches;
  ScreenCaptureKit delivers float32 non-interleaved, which `interleave` folds to L/R.
- ✅ **Mute is client-side, decode-but-don't-play** (2026-09-05). `ScreenHandle::set_muted`
  flips an `AtomicBool` the worker reads per packet; the Opus decoder keeps running so its
  state stays continuous and unmuting resumes on the next 20 ms packet. Nothing goes to the
  host: a host-side stop would need a protocol change and would also silence the other
  clients, and the muted stream costs ~12 kB/s, less than one video frame. The pill shows on
  the active window once audio has arrived, and on every muted window regardless, so a
  silenced item is never mistaken for one whose sound stopped.
- ✅ Transport: one `Kind::Audio` datagram per packet (~240 B), sequence in the frame field,
  no parity and no retransmit; the client counts gaps as `audio_lost` and the ring pads. Host
  gate: after 300 ms without a sample above -80 dBFS no packets go out, so the many silent
  windows on a canvas cost nothing; the first loud chunk reopens it (the 300 ms hold keeps
  natural pauses from chattering). Measured 2026-09-05 on loopback (display 6, `afplay`
  Glass.aiff ×3, 5 s): 249 packets sent, 246 received, 0 lost, bench player audible.
- ✅ **Clipboard sync is polled on the host and pushed ahead of ⌘V on the client**
  (2026-09-05, protocol 4). `NSPasteboard` has no change notification, only `changeCount`,
  so hostd reads it every 200 ms (one Mach call to the pasteboard server); a client-side
  watcher was rejected because GPUI's clipboard API has no count and the client would have to
  push on every poll. The client instead pushes exactly when it matters: the paste chord
  goes to the host on the same ordered stream right after `ScreenRequest::Clipboard`, so the
  host's paste already sees the text. Only clients with a window open receive the host's
  clipboard (the connection filters the broadcast) and the client never writes text it
  already holds, which is what stops the loopback echo when app and host share a Mac.
  Text only; files and images stay local. `cargo xtask bundle` builds `Slopty.app` with the
  daemons and CLI beside the app (`Contents/MacOS/slopty host install` works unchanged since
  the installer copies from its own directory), ad-hoc signed unless `--sign` names an
  identity.
- ⏸ A real jitter estimator and PLC (Apple's decoder takes no
  empty packet for concealment as far as the test showed; not tried). No `Quality` field for
  audio on purpose: keeping it off the wire avoided a protocol bump, and the gate makes
  "on" free.

## Input

- ✅ **`slopty-input` = `CGEvent` injection, one `Injector` per screen stream** (verified end to
  end 2026-09-04: keys typed into the Slopty app landed in a streamed Ghostty window). Stream
  pixels map back to global points through the target's current bounds (cached 100 ms) and the
  stream's pixels-per-point scale, which `ScreenStream` updates on every quality change.
  Window streams post with `CGEventPostToPid` to the owner; display streams post to the HID tap.
  Keys carry the client's text via `CGEventKeyboardSetUnicodeString`, so host and client
  layouts need not agree; bare modifier keys post as `FlagsChanged`. Needs post-event
  (Accessibility) access: `slopty-hostd` preflights at start-up, warns, and asks once.
- ✅ **The injector decides, a `Backend` posts** (2026-09-05). `Injector<B: Backend>` maps
  pixels to points, tracks held buttons and flags, picks the route (`Pid` vs `Hid`; right
  clicks always `Hid`) and activation, and hands a `Post { route, flags, event }` to the
  backend. `System` builds the `CGEvent`; `Recorder` keeps the `Post`s. The whole decision
  path is unit-tested against `Recorder` under `cargo gate` with no Accessibility grant and
  no real event. The one live test (`SLOPTY_INPUT_E2E`) checks only that a display-stream
  move lands, and runs via `cargo xtask e2e input`. Hand-driven checks (keys into a pid +
  window screenshots + reading them back) are banned: see `CLAUDE.md` ▸ Live tests.
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

## Testing

- ✅ **Four layers, each answering one question, fastest first** (2026-09-05). Unit tests for
  logic behind traits with fakes (`slopty_input::Recorder`); headless GPUI tests in
  `slopty-ui` (`#[gpui::test]`, `VisualTestContext`) for layout, focus, key bindings and what
  the scene paints; the app self-test (`crates/slopty-e2e`, `cargo xtask e2e app`) for the
  wiring of daemons + app + network + real PTY; the gated live desktop tests for capture
  and event posting. A behaviour is pinned at the lowest layer that can see it; the layers
  above only prove the seams. `CLAUDE.md` ▸ Tests is the operating guide.
- ✅ **The app tests itself over a control socket instead of being driven from outside**
  (2026-09-05). With `SLOPTY_TEST_SOCKET` set (feature `slopty/e2e`) the app serves JSON lines:
  `pair`, `keys`, `type`, `click`, `scroll`, `resize`, `dump`, `render`, `quit`. Input goes
  through `Window::dispatch_keystroke`/`dispatch_event`, so bindings, focus and listeners run
  exactly as for a user, and every command settles on the next frame before it answers.
  `dump` is the app's own model of its window (items with window bounds, focus owner, zoom,
  terminal rows); `render` is `Window::render_to_image` (fork, `test-support`), the app
  painting its own window, diffed numerically against a golden with a 24-value channel slack
  and a 1 % pixel tolerance. No external process ever posts events into the app or captures
  it, and nobody has to look at the images: the numbers and the `.diff.png` path are the
  verdict. Two real bugs surfaced on the first run (below).
- ✅ **Headless UI tests read the scene like a DOM** (2026-09-05). Item roots carry
  `.debug_selector("item-<uuid>")` and the canvas root `"canvas"`; `cx.debug_bounds` gives
  their laid-out bounds, `window.painted_quads()` the borders and colours the frame actually
  produced, and `simulate_click(bounds.center())`/`simulate_keystrokes` drive them. The fake
  host is an `mpsc` pair: the test asserts what the canvas sent and feeds back the
  `CanvasSync` deltas and `Frame`s a host would. GPUI's `TestPlatform` has no accessibility
  tree yet; exposing accesskit for dumps is a later fork change.
- ⚠️ **GPUI drops keystrokes when the focused element is not in the frame** (found by the
  app self-test 2026-09-05: ⌘W dead after ⌘1). Below `CARD_ZOOM` the terminals are drawn as
  cards without their views, so the focused `TerminalView` handle had no node in the
  dispatch tree and nothing, not even canvas bindings, ran. `CanvasView::keep_focus_rendered`
  moves focus to the canvas while in card mode and back to the active terminal when the
  grids return; the headless test `zooming_out_to_cards_hands_the_keyboard_to_the_canvas_and_back`
  pins it.

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
- ✅ **App icon rendered from one SVG at build time** (2026-09-05). `assets/icon.svg` (dark
  full-bleed square, faint dot grid for the canvas, an accent `❯` and a green cursor block) is
  rasterised by `xtask::icon` with `resvg` 0.48.1 + `tiny-skia` 0.12 (MPL-2.0/BSD, already
  allowed) into `Slopty.icns` (16…512 pt at 1× and 2× via `icns` 0.4.0) for `cargo xtask
  bundle` (`CFBundleIconFile`) and a 1024 px PNG in a generated `Assets.xcassets` for `cargo
  xtask ios` (`ASSETCATALOG_COMPILER_APPICON_NAME`). No `iconutil`/`sips`/design tool; `cargo
  xtask icon [dir]` writes the PNG ladder for a look. Full bleed on purpose: macOS 26 and iOS
  apply their own squircle mask, so baked-in rounded corners would double up. Verified: the
  Dock shows the icon for the debug bundle and the iOS 26.5 simulator home screen shows it
  after `cargo xtask ios sim`.
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

## Settings

- ✅ **One TOML file, every key optional** (2026-09-05): `<data dir>/settings.toml`, the
  directory the client identity already uses (`SLOPTY_DATA_DIR`, else
  `~/Library/Application Support/Slopty`; `slopty_settings::data_dir` is now the one copy the
  app and the CLI share). `slopty-settings` is serde + `toml` 1.1.5 only, no GPUI, so the CLI
  and tests use it without a window. `#[serde(default)]` on every struct makes the file and
  any subset of it valid; unknown keys are found by diffing the parsed table against the
  serialised defaults (no hand-kept key list) and reported as warnings; a parse or type error
  yields the defaults plus a one-line error (`Loaded { settings, warnings, error }`), never a
  crash. `slopty settings init` writes a commented file generated from `Settings::default()`
  and covered by a round-trip test. `terminal.scrollback_lines` is host-side and not here.
- ✅ **Hot reload by a 1 s stamp poll, not `notify`** (2026-09-05): a GPUI foreground task
  sleeps on the background executor's timer and compares `(mtime, len, exists)`. Editors save
  atomically (temp file + rename), which orphans a per-file FSEvents/kqueue watch, so `notify`
  would need a directory watch plus filtering and a debounce for half-written files anyway;
  one `stat` a second is free, handles create / replace / delete alike and adds no dependency
  to the iOS triple. Verified: `mono_size` 13 → 20 while the app ran re-fit the shell from
  21×87 to 14×58 (`stty size`) within ~2 s and back again; a `[font` typo showed
  `settings: …: TOML parse error at line 1, column 6` in the bar for 6 s with the defaults in
  force; `ligatures = true` showed the unknown-key notice for `font.ligatures` and loaded the rest.
- ✅ **Theme swap is a push, not a global** (2026-09-05): `Workspace::rebuild_theme` derives
  the `Theme` (`slopty_app::settings::theme_for`: variant, the family ahead of the bundled
  fallbacks, sizes clamped to sane ranges) and calls `CanvasView::set_theme`, which fans out to
  every `TerminalView`, `ScreenView` and the picker; the terminal element measures from the
  view's theme on each frame, so a size change re-fits the grid through the existing `fitted`
  → `TermRequest::Resize` path with no new wire message. Found on the way: the shaped-row
  cache was keyed by text, style, focus and size only, so a swap replayed the old palette's
  default text colour; the key now includes the family and the whole `TerminalPalette`.
- ✅ **Light variant + `system` appearance** (2026-09-05): `Theme::new(Variant::Light)` with a
  GitHub-light terminal palette and near-white surfaces; `appearance = "system"` (the default)
  follows `window.appearance()` through `observe_window_appearance`, so a macOS appearance
  change re-themes without a restart. gpui-kit's own theme mode (the pairing input) is switched
  alongside. Verified on a Mac in light appearance: the app opened light, `appearance = "dark"`
  turned it dark on save.
- ✅ **"Settings…" (⌘,) opens the file in the default editor** via `App::open_with_system`
  (`open <path>` on a background thread in `gpui_macos`), after `Settings::init` so a first
  press has a file to edit. Verified: the menu item opened `settings.toml` in VS Code. iOS has
  no entry: the defaults apply there and the file, if present in the sandbox, is still read.

## Hosts

- ✅ **The pairing store was already a list** (2026-09-05): `slopty_net::identity::Identity`
  keeps `hosts: BTreeMap<EndpointId, KnownHost>` in `client.json`; only the app's
  `connect_host` picked the first entry. So there is no second file and no migration:
  `net::known_hosts` reads the map (sorted by name), `net::forget_host` removes one, and a
  pairing on the panel is `Identity::remember` as before. The stored name is refreshed from
  each `HelloAck` so the switcher does not show a stale one before the link is up.
- ✅ **One link, one canvas, one loop per host; one canvas on show** (2026-09-05):
  `Workspace::spawn_host_loop` is the old connect loop with a host id: its own endpoint,
  backoff, silence check (`SILENCE_DROP` abandons the link after 15 s without a datagram) and
  event pump, writing into a `HostSlot` (status, canvas, zoom, needs-you, RTT). The loop ends
  when its slot is gone (forget). Each host owns its `canvas.json`, so one `CanvasView` per
  host and the workspace shows `active`'s; switching is a field change plus a focus, nothing
  is torn down. `Rejection::NotPaired` maps to `HostStatus::NeedsPairing` (red dot) and keeps
  retrying at the capped backoff so a fresh pairing of the same host resumes by itself.
  Verified with two daemons on this Mac (`SLOPTY_PORT` 45570/45571, `SLOPTY_HOST_NAME`
  "Studio One" / "Studio Two", the new env override in `slopty-hostd::paths`): "Add host…"
  paired the second while the first stayed connected; each host had its own shell (`echo
  two` on one, a new shell on the other); ⌘⌥←/→ and the switcher rows swapped canvases;
  killing one daemon turned its row amber with "disconnected … reconnecting" within ~16 s
  while the other stayed green, and restarting it turned the row green again; "forget"
  removed the row and the `client.json` entry (`slopty hosts` listed one host).
- ✅ **Switcher is a hand-rolled overlay, not gpui-kit's popover** (2026-09-05): a
  full-window backdrop that closes on click with an occluding panel anchored under the host
  name, the same pattern as the window picker. It needs no focus handling and lays out the
  same on a 520-pt window (the host button keeps a 96-pt minimum there and the status label
  yields; verified) as on the desktop. ⌘1…⌘9 are the fit shortcuts, so hosts step with
  ⌘⌥→ / ⌘⌥← (and ⌘⇧H adds one); the Host menu names the same actions.
- ✅ **Cross-host attention** (2026-09-05): the pill and `set_badge` use the sum of every
  slot's `needs_you` (each canvas still reports its own count through `CanvasEvent::NeedsYou`);
  a tap prefers the host on show, else the first with a waiting agent, switching first. A
  banner's tag stays the session UUID (unique across hosts); `on_system_notification_response`
  asks each canvas `has_session` to find the host, activates it, then calls
  `notification_response` as before. Code-verified only: no Claude Code session was driven on
  the second host in the manual run.

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
- ✅ **Dock badge + bounce on macOS** (2026-09-05). `CanvasEvent::NeedsYou(n)` also calls
  `slopty_platform::set_badge(n)` (`NSApplication.dockTile.badgeLabel`, cleared at 0 and on
  every reconnect), and `Attention` adds `slopty_platform::bounce()`
  (`requestUserAttention(NSCriticalRequest)`, skipped when the app `isActive`, so a user looking
  at the canvas gets only the sound). Both need no bundle or entitlement, unlike
  `UNUserNotificationCenter`. iOS: no-ops — the icon badge needs notification authorisation and
  the connection dies in the background anyway, so the in-app "N need you" pill is the signal.
  Verified 2026-09-05 with a synthetic `PermissionRequest` through `slopty hook`: the tile
  shows "1" (bare binary and bundle alike), `PostToolUse` clears it. Gotcha: `cargo build -p
  slopty-app` builds only the library crate; the binary is `cargo build -p slopty --bin
  slopty-app`. GPUI's `show_system_notification` exists in the fork but is disabled outside a
  bundle ("system notifications disabled: not running from an app bundle") — next step for
  banners now that `cargo xtask bundle` exists.
- ✅ **Notification-centre banners for agents** (2026-09-05). `CanvasView::agent_event` posts a
  GPUI `SystemNotification` (tag = session id, title "Claude wants to use Bash" / "has a
  question" / "finished", body = the badge detail, Allow/Deny action buttons for a permission)
  when `attention` is set and `cx.active_window()` is `None` (the app is not frontmost —
  `NSApp.mainWindow` is nil then). Answering from the badge or the agent moving on dismisses it
  by tag. `cx.on_system_notification_response` (registered once in `open_workspace`) parses the
  tag, activates the app and calls `CanvasView::notification_response`: body → reveal the
  session, "allow"/"deny" → the badge's Enter/Esc. GPUI's macOS backend disables all of it
  outside a bundle, so this only works from `cargo xtask bundle` output (the first post raises
  the macOS "Slopty Notifications" prompt). Verified 2026-09-05 with the debug bundle: the
  notification landed in Notification Center ("allow? $ cargo test"), clicking it brought Slopty
  forward with that terminal active. The action buttons are wired through GPUI's category
  registration but were not exercised (macOS only shows them on hover of a live banner).
  iOS: no-op — the connection dies in the background, nothing would post.
- ✅ Answering a permission prompt from the badge (verified 2026-09-05 against Claude Code
  2.1.261 driven under a pty in `default` permission mode): the prompt is a numbered menu with
  the first entry highlighted — `❯ 1. Yes · 2. Yes, and always allow access to <dir> from this
  project · 3. Yes, and switch to auto mode · 4. No — Esc to cancel · Tab to amend`. Enter
  (`\r`) ran the command ("Ran 1 shell command", the file appeared); Esc (`\x1b`) ended the turn
  with "Interrupted · What should Claude do instead?" and nothing ran. So "allow" = Enter and
  "deny" = Esc, sent through `TerminalView::press` like the phone's key bar, with a sticky
  Control disarmed first. Neither key is sent twice: the canvas remembers the answer per session
  until the next `HostMsg::Agent` for it. Two things learned on the way: text and `\r` written
  in one burst are taken as a paste (the newline does not submit), and Esc while the model is
  still thinking interrupts the turn instead of answering — the badge only offers the buttons
  while the status is `Blocked(Permission)`, which the `PermissionRequest` hook raises after
  the menu is up. Question / elicitation badges cannot be answered with one key (the reply is
  text or a pick), so their button reveals and focuses the terminal.
- ✅ Finding the agent that needs you: `CanvasView::needs_you` is the list of terminal items
  whose agent is `Blocked` (not `IdlePrompt`) and not yet answered from the badge, sorted by
  `(rect.y, rect.x)`. ⌘⇧A (`NextAttention`, Canvas key context, so it works with a terminal
  focused since ⌘ chords fall through) picks the entry after the active item, wrapping, else
  the first, and runs the same reveal + focus as the "answer" button. The count travels as
  `CanvasEvent::NeedsYou(n)` to the workspace, which draws the "N need you" pill in the top
  bar on both platforms (a tap on the phone, where there is no ⌘⇧A). Reading order rather than
  z or arrival time because it is the one order the user can predict from what they see; the
  picker (⌘O) uses the same order within its "needs you / other agents / shells" ranking. No
  proto change: the count is derived from the `HostMsg::Agent` table the client already has.
- ✅ What the badge says when the agent waits or stops (verified 2026-09-05, Claude Code 2.1.261
  inside a Slopty session through `slopty open`, payloads captured with a `tee` hook beside
  the relay): every event carries `transcript_path`; `Stop` also carries
  `last_assistant_message` (here "Done. `/tmp/…/opened-enter` was created."), which the docs
  say to prefer because the transcript file "may lag the in-memory conversation". So the
  detail comes from the payload first — the `AskUserQuestion` input's first `question`, the
  `Elicitation` `message`, the `Stop` message's last non-empty line — and only a `Blocked
  (Question|Elicitation)` or `Done` event that still has no detail makes the daemon read the
  transcript's last 256 KiB for the newest non-sidechain `assistant` record with a `text`
  block (`slopty_agent::transcript`, `spawn_blocking`, table lock released meanwhile). The
  transcript is JSONL of `{"type":"assistant","message":{"content":[{"type":"text",…}]}}`
  records; `isSidechain: true` rows are subagent chatter and skipped. No wire change: `detail`
  already existed. Found on the way: the `permission_prompt` Notification that follows a
  `PermissionRequest` ~6 s later says only "Claude needs your permission" (no "to use X"), and
  used to replace `Permission{Bash}` + "$ touch …" with `Permission{""}` + nothing; the tracker
  now keeps the request's tool and detail when the notification names none. After Esc on the
  menu no `Stop` fires (the turn is interrupted; the next hook is whatever the user does), so
  the badge keeps saying "denied" until then — acceptable, since the outline is already off.
- ✅ "+ agent" (⌘⇧T, "New Agent" menu item) sends `OpenSession { command: ["claude"], title:
  "claude" }`; nothing else is special about the session, so the hook relay, badges and
  attention all apply as they do to a `claude` typed into a shell. The daemon's `PATH` is
  launchd's under a LaunchAgent, and `claude` is often a shell alias (`~/.claude/local`), so
  `slopty-pty::resolve_command` runs a bare name it cannot find on `PATH` through
  `$SHELL -lic '<quoted words>'` (interactive login shell: rc files, aliases, job control);
  a path or a name found on `PATH` still execs directly. Covered by
  `unknown_bare_program_goes_through_the_login_shell`.
- ✅ **Links: OSC 8 first, text scan second** (2026-09-05). The engine reads the URI of every
  linked cell with `ghostty_grid_ref_hyperlink_uri`, gated on the row's `has_hyperlink` page
  flag (a false positive costs one extra check per cell, a clean row costs nothing) and on the
  cell's own flag, both on the render path (`Point::Viewport`, the host never scrolls the
  viewport) and on the scrollback fetch path (`Point::Screen`). Runs, not ids: `Line::links`
  is `Vec<Hyperlink { col, len, uri }>` and the per-cell `Option<HyperlinkId>` that
  `slopty-grid` had carried unused is gone. Measured (MEASUREMENTS.md, same date): a
  link-free 80×24 full frame went from 15 477 to 13 581 bytes (−12 %), because postcard
  spends one byte per `None` and the empty run list costs one byte per *row*. A spacer tail
  continues its wide character's run. `PROTOCOL_VERSION` 5 → 6. Client: `url::link_at_col`
  returns the OSC 8 run when there is one, else the plain-text URL with the columns it covers
  (`text_link_at_col`, offsets mapped back to cells, a wide cell's spacer inside the span);
  ⌘-click opens it, and while ⌘ is held the run under the pointer is underlined
  (`TerminalView::link_highlight` → a 1 px quad in `TerminalElement::paint`, so the shaped-line
  cache is untouched; `on_modifiers_changed` plus the modifiers on every move keep the state
  right whether ⌘ goes down before or after the pointer arrives). Covered by
  `osc8_links_become_runs_on_screen_and_in_history`, `plain_rows_carry_no_link_runs`,
  `links_are_found_by_column_and_clipped_on_resize`, `osc8_runs_win_over_the_text_scan`,
  `text_links_come_with_their_columns` and the `host_lines_links` golden. macOS only for now:
  the phone key bar arms ⌘ for remote windows but not for terminals, so a tap has nothing to
  read; long-press stays selection. `cx.open_url` is `gpui_ios`'s `UIApplication.openURL`
  when that arrives.
- ✅ **OSC 52: write only, system clipboard only, ≤ `MAX_CLIPBOARD_BYTES`** (2026-09-05).
  libghostty's `clipboard_write` callback (registered in `install_callbacks` next to the bell)
  hands over a normalised, decoded write; the engine keeps the `text/plain` representation of
  a `Standard` write and answers `Unsupported` for selection/primary (X11 notions with no
  client counterpart), the session drops writes over the pasteboard-sync ceiling (the same
  256 KiB constant from `slopty_proto::screen`; a whole file pasted through OSC 52 would sit
  ahead of every frame on the session stream) and broadcasts `TermEvent::ClipboardWrite` to
  every attached client, whose canvas writes it with `cx.write_to_clipboard`. Reads are not
  implemented, on purpose: libghostty never forwards a `?` request, and the dead
  `TermEvent::ClipboardReadRequest` / `TermRequest::ClipboardRead` pair (the client used to
  answer it with its clipboard, unprompted) is removed from the protocol so a future host
  cannot ask. Covered by `osc52_writes_to_the_system_clipboard_only` (standard, primary,
  selection, `?`) and the `host_term_clipboard_write` golden.
- ✅ Deployment is two LaunchAgents written by `slopty host install` (`plist` crate, XML):
  `KeepAlive` + `RunAtLoad` + `ThrottleInterval 2` so a crash comes back in 2 s and login
  starts both; `ProcessType Interactive` and `LimitLoadToSessionType Aqua` because hostd
  needs the window server and ScreenCaptureKit and must not be App-Napped; sockets under
  `<data dir>/run/` (not `$TMPDIR`, which launchd children may not share) and the CLI's
  socket lookup falls back to that path when it exists. `install` re-bootstraps (bootout
  first, so a stale socket file never wedges the bind) and waits up to 10 s for a ticket.
- 🔬 Attribution without hooks (a `claude` started before `install`, or in a session the host
  did not spawn): no signal today. `SessionSummary.command` could seed an `Idle` badge; the
  transcript tail could recover the rest. Not built.
- ⏸ ACP via `agent-client-protocol` 2.0.0 + `@agentclientprotocol/claude-agent-acp` for structured
  driving — after the PTY path works.
