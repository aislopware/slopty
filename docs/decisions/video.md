# Decisions — Video

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **HEVC Main (8-bit 4:2:0) default, Main10/P010 when the source is HDR.** AV1 is decode-only
  on Apple silicon (Apple spec pages, verified 2026-09-04). No 4:4:4 hardware path exists.
  (Main10/P010 superseded 2026-09-24: HEVC Main only, see "420f and BT.709 end to end".)

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
    (header verified) — ✅ evaluated 2026-09-05, rejected; see "Encoder rate control" below.
  - `MaxAllowedFrameQP` / `MinAllowedFrameQP`: ✅ probed 2026-09-05, both accepted (status 0)
    by the low-latency and the VBV encoder; unused (see below).
  - `SpatialAdaptiveQPLevel` must be disabled under low-latency RC (header says so).
  - LTR: `EnableLTR` = true; per frame `ForceLTRRefresh` (CFBoolean) and `AcknowledgedLTRTokens`
    (CFArray<CFNumber>); output attachment `RequireLTRAcknowledgementToken`. Header verified.
  - `MaxH264SliceBytes` is H.264-only; no HEVC intra-refresh property exists. LTR is the recovery.
  - Keys are read from the framework's exported statics (objc2 bindings), never string literals.

- ✅ **Capture** — verified against `SCStream.h` (macOS 26.5 SDK): `minimumFrameInterval`,
  `queueDepth`, `pixelFormat`, `showsCursor`, `captureResolution`, `ignoreShadowsSingleWindow`,
  `includeChildWindows` (14.2+), `captureDynamicRange` (15+), `capturesAudio` +
  `excludesCurrentProcessAudio`. ✅ Defaults read back from a fresh `SCStreamConfiguration`
  (`slopty_capture::sck_defaults`, macOS 26.5, 2026-09-05): `queueDepth` **8**,
  `minimumFrameInterval` **1/60 s** — slop-desk was right on both. Ruling on the depth below.

- ✅ **Capture latency is measured per frame, host side, and the window's "8 ms floor" was the
  encoder** (2026-09-05). Every ScreenCaptureKit sample carries `SCStreamFrameInfoDisplayTime`
  (mach absolute time when the window server displayed the frame); read through
  `CMClockMakeHostTimeFromSystemUnits` it equals the sample's presentation timestamp to the
  microsecond on both the window and the display path (offset p50 0.00 ms over 285 frames each),
  so `CapturedFrame::latency_us` (display time → our callback) is what SCK adds. Measured
  (MEASUREMENTS.md, "capture floor"): the window filter delivers a scrolling 900×500 terminal
  at **0.46 ms p50 / 0.95–2.35 ms p95**, the display filter at 0.00 / 0.46 ms
  — SCK's own cost is under a millisecond either way, and the "window floor of ~8 ms" recorded
  earlier was the *encode* step: submit → VideoToolbox callback is **6.7 ms p50** for that
  window whatever the capture path and whatever the content (a scrolling and a static window
  encode in the same time), i.e. the hardware encoder's fixed pipeline, not rate control. The
  host keeps both as p50/p95/max rings per stream (`ScreenStats::capture` / `::encode`, 600
  frames) and exposes them on the local control socket (`CtlRequest::Screens`, `slopty worker
  screens`); `slopty bench screen` appends them on loopback. Not on the wire: the client has no
  use for the host's internal split, and the protocol number stays with the tracks that need it.

- ✅ **Windows are served as a crop of their display when nothing covers them** (2026-09-05).
  `SCContentFilter(display:excludingWindows:)` + `sourceRect` at the window's frame (points in
  the display's space; `crop_for` is the pure geometry, `Target::resolve_crop` the SCK side)
  delivers the same picture as the window filter (interior mean |Δluma| 0.05/255 on a static
  window; only the corners differ, the window filter leaves them transparent) at the display
  path's latency: end to end (capture → decoded on loopback, `slopty bench screen`)
  **1.7 ms p50 against 9.0 ms** for the window filter in a debug build
  (release: 2.6 ms vs 9.0 ms), the ≥ 3 ms the brief asked for. Rules, in
  `slopty_worker::screen::resolve` / `check_geometry`: crop only while the window is entirely on
  one display (`CGGetDisplaysWithRect` + `encloses`) and no on-screen window of another process
  at levels 0–8 overlaps it (`occluded`, from `kCGWindowListOptionOnScreenAboveWindow`); the
  Dock's window is a transparent full-screen hit region at level 20, the menu bar is 24 and
  status items 25, none of which counts. A move is one `updateConfiguration` with the new
  `sourceRect` (~20 ms to complete, frames keep flowing, measured), a resize rebuilds the
  encoder as before, and a window that gets covered or dragged off its display swaps to the
  window filter on the live stream (`updateContentFilter`) and back when clear; the crop is
  cleared *before* the filter swap because a `sourceRect` on a window stream is read in the
  window's own space. Known cost: on the crop path a drag lags by the geometry poll
  (100 ms, was 250) plus the update, during which one edge shows the desktop the window left.
  `SLOPTY_WINDOW_CAPTURE=window|crop` forces a path. Guard: `slopty-capture/tests/latency.rs`
  (gated) fails when the window filter's capture p95 exceeds 1 ms + 3 ms margin.

- ✅ **Rulings of the crop path: when it is allowed, what it must never show** (2026-09-06,
  after the Codex review of the first cut). The crop path is an optimisation of the window
  filter and must be indistinguishable from it to the viewer; wherever it cannot be, the
  window filter serves. `slopty_worker::screen::crop_allowed(on_screen, crop, occluded)` is
  the rule, unit-tested; `wanted_crop` / `resolve` feed it from the window server.
  1. *Allowed only for an on-screen window* (`kCGWindowIsOnscreen`, `window_on_screen`): a
     minimised window or one on another Space has a frame but nothing at it; the crop would
     show whatever sits there now. The window filter shows the window wherever it is.
  2. *Allowed only when nothing counts as covering it*, and "covering" is per window, not
     per process: a sibling window of the same app in front is an occluder like any other
     (`counts_as_occluder`, unit-tested with two siblings); only the target's own
     child, sheet, popup and menu windows (same pid, level above normal) are not, since they
     belong to the picture the window filter shows too. Other processes count at levels 0–8;
     the Dock's full-screen hit region (20), the menu bar (24), status items (25) and the
     cursor do not. First cut had excluded the whole pid; a second Ghostty window dragged over
     the target went unnoticed.
  3. *It must never show the desktop after the window closed.* Through the window filter
     ScreenCaptureKit ends the stream itself and the connection reports `Closed`; a display
     stream never ends by itself, so `check_geometry` treats "no bounds for the window" on the
     crop path as the window gone: stops the capture and fires `on_stop`
     (`CaptureError::Stopped("window closed")`) once, the same event the filter path raises.
     Gated test `closing_the_window_under_a_display_crop_ends_the_stream` (`slopty-worker`)
     kills the Ghostty window and waits for it. First cut returned "no change" and kept
     streaming the old rectangle.
  4. *Audio is the window's application's only.* `sourceRect` scopes the picture, not the
     sound: a `display:excludingWindows:` filter carries every app's audio. The crop filter is
     `display:includingApplications:exceptingWindows:` with the window's owning app
     (`Target::resolve_crop`; `included_applications()` reads it back), which is what the
     window filter carries. Gated test `display_crop_scopes_audio_to_the_windows_app`
     asserts the filter's app list is exactly Ghostty's bundle id on the crop path, empty for
     a display and for the window filter (no measuring: the configuration is the contract).
  5. *A switch is committed only when ScreenCaptureKit has committed it.* `updateContentFilter`
     and `updateConfiguration` complete asynchronously and can fail; the first cut wrote
     `path` / crop as soon as it had asked. Now `follow_window` opens a `Transition` (path,
     crop, number of outstanding callbacks) shared with the completion blocks and does nothing
     while one is in flight; the next tick settles it: every callback ok → path and crop
     become the stream's state; any failure → nothing changes and the tick asks again, since
     the window server still says the same thing. Unit-tested with a failing result. Order
     is unchanged: crop cleared before the swap to the window filter, filter swapped before
     the crop is set.
  6. *Host-side stats are matched on (client, stream)*: stream ids are per connection, so
     `slopty bench screen` looks its own stream up by its client id too, not the first
     connection that happens to have a stream of that number.

- ✅ **`queueDepth` stays 2** (2026-09-05; superseded 2026-09-25 by 3, see "A still picture keeps
  its newest capture"). Measured 2 / 3 / 5 / 8 on the window filter
  (MEASUREMENTS.md, "capture floor"): p50 is flat (0.46 → 0.49 ms) and p95/max grow with the
  depth; no drops at any depth on a 60 Hz source. The default 8 buys nothing here since the
  callback hands the surface straight to the encoder.

- ✅ **Encoder rate control: keep `EnableLowLatencyRateControl` + `AverageBitRate` +
  `DataRateLimits`; the macOS 26 VBV keys are out** (2026-09-05). The VBV keys
  (`VariableBitRate`, `VBVMaxBitRate`, `VBVBufferDuration`) are documented incompatible with
  low-latency rate control and the probe agrees: the low-latency session is a *different*
  encoder whose `VTSessionCopySupportedPropertyDictionary` does not list them (it lists
  `EnableLTR`, `DataRateLimits`, `AverageBitRate`, no `MaxFrameDelayCount`, no
  `PrioritizeEncodingSpeedOverQuality`), while a session created without the specification
  lists them plus `LookAheadFrames`, `MCTFLatencyMode`, `AllowOpenGOP` — and loses LTR
  (`EnableLTR` rejected, `ltr false`), which is Slopty's loss recovery. Measured head to head on
  the same 900×500 Ghostty window at 8 Mbit/s (MEASUREMENTS.md, "encoder rate control"):
  encode p50 6.70 vs 6.59 ms — identical; frame-size spikes max/p50 **2.0×
  vs 5.6×** and 64 vs 163 kbit/s for the same picture — the VBV encoder
  spends more and spikes more. Nothing to adopt. Also tried on the low-latency encoder
  (`Encoder::set_property`, public for benches): `MaximizePowerEfficiency=false` (6.69 ms
  p50), `ExpectedFrameRate=120` (6.73 ms), H.264 instead of HEVC (6.45 ms): all within run-to-run noise of the 6.70 ms baseline (H.264 buys 0.25 ms p50 and loses 0.3 ms at p95 on the static window), so none is adopted.
  `MaxAllowedFrameQP` / `MinAllowedFrameQP` are accepted by both encoders (status 0) but stay
  unset: they trade frame drops for a QP bound, and a dropped frame is a hole the client asks
  a refresh for. The ~7 ms is the encoder's pipeline; the remaining glass-to-wire budget is
  there, behind keys the SDK does not export (`RequestedMaxEncoderLatency` shows in the
  low-latency encoder's supported list but is not in the 26.5 headers, so it is not used).

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

- ✅ **Stall-aware bitrate** (2026-09-05). A Wi-Fi stall (packets held 100–300 ms, then
  released together; the host dropped nothing, see "stalls, not drops" in MEASUREMENTS.md)
  looked like loss to the controller and got a cut it did not deserve: sending less does not
  clear a stall. *Wire:* `ReceiverReport` carries `stalled_ms` (silence past the reassembler's
  stall gap charged to the report window, a stall still on at report time included up to now)
  and `stalls` (releases in the window). Two fields, not one `flowing` flag, because a report
  is a 50 ms point sample and a stall of 150 ms can start and release between two of them;
  the count alone would miss a stall still on, the time alone cannot show two short ones. A
  third `stalled_ms` per stall was not needed: the policy only asks "was there a stall in this
  window". `ScreenEvent::Rate { target_bps, verdict }` goes back to the client on every
  decision so the ⌘⇧I overlay and `slopty bench screen` show the host's target and whether it
  is holding; before, the target lived only in hostd's log on another machine.
  `PROTOCOL_VERSION` 7 → 8, goldens `client_screen_report` and `worker_screen_rate`.
  *Policy:* `slopty_media::judge` is a pure function of the decision window
  (`RateWindow`): any stall in the window → `Stall`, freeze (no cut, no grow, the cooldown
  neither restarts nor ticks, the window's loss is discarded — it is the receiver giving up on
  frames the link still holds); else the overuse / clean rules as before → `Cut` / `Grow` /
  `Steady`. The two reports after a stall (`SETTLE_REPORTS`) contribute loss but not queue or
  hold: the release fills the present queue for a moment and that burst must not be judged.
  Loss in those reports still counts, so a stall followed by real loss cuts once. The cwnd cap
  applies in every state, a stall included. Table of report sequences → target trajectory in
  `report_sequences_and_their_target_trajectories` (clean → grow; loss → cut then cooldown;
  stall → hold then grow; stall then loss → one cut; a stall inside a cooldown neither cuts
  nor ends it), the burst rule in `the_release_burst_after_a_stall_is_not_judged`, the
  reassembler's charging in `reports_carry_the_stall_time_and_count`. Evidence from the mesh
  in MEASUREMENTS.md ("stall-aware bitrate").

- ✅ **Capture heartbeat** (2026-09-05). The reassembler's stall clock counts silence on the
  link, but a screen that does not change is silent too: ScreenCaptureKit delivers no frame
  for a still display, and its warm-up after the first frame is a 300 ms hole. Every such
  gap read as a stall and froze the controller for a window (0.5 s of growth lost at every
  start, see "stall-aware bitrate" in MEASUREMENTS.md). *Wire:* `Kind::Heartbeat` (4), a bare
  16-byte `MediaHeader` with no body, `frame` reused as a beat counter; `PROTOCOL_VERSION`
  8 → 9, golden `media_heartbeat`. A header-only datagram over a new control message
  because it has to travel the same datagram path the stall clock watches (a reliable-stream
  message would arrive through a different queue and prove nothing about the datagram path)
  and because the reassembler already resets `arrived_at` for any datagram of its stream
  before looking at the kind: the beat resets the stall clock and nothing else — not
  `any_arrived`, not the frame or loss counters — so a report window full of beats is clean
  and the controller grows (`heartbeats_keep_a_quiet_source_from_reading_as_a_stall`: 400 ms
  of beats → `(stalled_ms, stalls) = (0, 0)` and `Grow`; the same 400 ms without → `(400,
  1)`). *Host:* `ScreenStream` records the time of its last successful push; the cursor
  sampler (already ticking at 120 Hz) pushes a beat whenever that is `HEARTBEAT_AFTER` =
  25 ms old, half the receiver's 50 ms `STALL_GAP`, so one lost beat still does not read as a
  stall. Counted in `ScreenStats::heartbeats`. The client ignores `Ingest::Heartbeat`.

- ✅ **The start-up stall was the client, not QUIC** (2026-09-05, corrects the reading in
  the capture-heartbeat entry above and its MEASUREMENTS.md section). The new e2e test
  `screen_start_up_over_iroh` (five cold connections to one hostd, direct-only loopback,
  each opening the first display at the app's default quality) stamps four instants on the
  client: `Open` sent, first datagram, first frame complete, first decoded picture. Before
  the fixes: opened at 353 ms on a fresh hostd (176–191 ms after), first frame complete
  5 ms later, first picture **168 ms** after that, and a 145–151 ms "stall" in the first
  decision window. The 168 ms is `VTDecompressionSessionCreate` for the first session in a
  process (3 ms for every later one); it ran inside the stream worker, which read no
  datagram meanwhile, so when it caught up the reassembler saw 150 ms of silence and charged
  it as a link stall, freezing the first rate window (the "QUIC send-side hold" the heartbeat
  section blamed). Rules that follow, each measured in the same test:
  * `slopty_codec::warm_up` builds one HEVC session from canned 64×64 parameter sets;
    `slopty_client::warm_up_decoder` runs it once per process on its own thread and is
    called at app launch (`open_workspace`), on the CLI's connect, by `WorkerLink::start`
    (`crates/slopty-client/src/link.rs`), and by the test. First picture 526 → 319 ms on
    the first open in a process.
  * `slopty_worker::screen::warm_up` builds and drops a 64×64 HEVC encoder, then starts and
    stops a 64×64 capture of the first display, when hostd comes online. The capture-only
    version did not move the first `Opened` (270–295 ms); the encoder did (→ 127–140 ms):
    VideoToolbox's first compression session in a process is ~170 ms, the later ones ~10.
    `shareable()` keeps its last enumeration for 2 s (`SHAREABLE_TTL`) because a client
    lists, picks and opens within seconds and each enumeration is 60–75 ms.
    `Opened` 176–191 → 113–138 ms, first open included.
  * The datagram pump published how many bytes QUIC is holding (then `DatagramBudget::held`,
    now `DatagramSink::held`: 4 MiB buffer minus `datagram_send_buffer_space`); the capture
    callback drops a frame, flagged for an LTR refresh, while more than two frames' worth at
    the current target (32 KiB floor) is held (`frame_fits`), and a NACK is answered only
    under the same condition. Over the mesh at a 4800-byte window the pump had held 275–365 KB
    for 4–7 s — every frame delivered stale — and 851 NACK answers piled 64 221 datagrams
    behind 12 frames. Stale frames and stale retransmits are worth nothing; a fresh keyframe
    after the drop is. (`a_frame_fits_unless_the_queue_or_quic_holds_too_much`)
  * The client router stamps every datagram with the instant the connection handed it over
    and the worker ingests with that instant; the worker drains everything already queued
    (a burst, or the backlog from before the stream was attached) before the timers look at
    frame ages. A slow worker can no longer read as a stalled link or as a lost tail.
  * The bitrate controller's cwnd cap is taken from the widest path sample in the decision
    window, not the last: BBR's `ProbeRTT` shrinks the window to four packets for 200 ms
    every 5 s and a cap read then cut a loopback stream to 6.4 Mbit/s for a window
    (`a_probe_rtt_dip_inside_the_window_does_not_cap_the_target`).
  * The ⌘⇧I overlay is two lines built by the pure `hud_lines`: picture (size, fps, Mb/s,
    rtt, age of the frame on screen) and path (jitter, hold p50/p95, queue, fec/lost/nack/
    refresh, stalls, the host's verdict, audio). `ScreenStats` carries the report's hold
    and jitter figures and the four start-up instants; `slopty bench screen` prints them.
  Loopback after all of the above, machine quiet: 0 NACKs, 0 stalls, 0 holds ≥ 5 ms in
  5 × 3 s under BBR3 and under Cubic (tables in MEASUREMENTS.md). Settled on a quiet machine
  (MEASUREMENTS.md, "start-up over iroh on a quiet machine"): 0 stalls and 0 NACKs across all
  samples, confirming the loaded-run stalls were the scheduler; the mesh run's held-frame drop
  is the one ruling from it.

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
  parity for an already complete frame is dropped unread. Settled by the rulings below: initial
  NACK delay derives from round trip ("NACK delay from the round trip"), deadlines pause during
  stalls ("Loss deadlines only run while the link is flowing"), and the give-up deadline is kept
  ("The NACK give-up deadline stays where it is").

- ✅ **Redundancy control** (`Redundancy`): parity permille = clamp(2 × EWMA(datagram loss) +
  50, 50, 500), ×1.5 bump when a report shows a frame lost outright. Settled by the ruling
  below: "Parity tracks loss asymmetrically, with a deadband" (asymmetric rise ½ / fall ⅛,
  2 % deadband, stalled windows excluded from the estimate).

- ✅ **Host pipeline** (`slopty-worker::screen`, verified on hardware 2026-09-04): one
  `ScreenStream` per open stream: SCK frame → `Encoder::encode` on the SCK queue → packetize in
  the VideoToolbox output callback → bounded datagram queue (4096) → one pump task per
  connection calling `Connection::send_datagram`. Backpressure drops whole *captured* frames
  when fewer than 256 slots are free and flags the next frame as an LTR refresh; a full queue
  mid-frame drops the rest of that frame (the reassembler NACKs). Encoder sessions are
  `Send + Sync` (VideoToolbox is documented thread-safe); the packet sink holds a `Weak` so an
  encoder never keeps its own stream alive.

- ✅ **Datagram budget**: QUIC's datagram limit starts near 1168 B at the 1200-byte initial MTU
  and grows with path-MTU discovery, so the protocol's 1200-byte `MAX_DATAGRAM` is a ceiling,
  not a promise. The packetizer cuts each frame to `Connection::max_datagram_size()` as the
  stream's `DatagramSink` reads it just before the cut (`Packetizer::set_max_datagram`).
  Amended 2026-09-25 (transport.md, **Media datagrams go to QUIC from the thread that made
  them**): a datagram cut before the limit shrank is refused by QUIC with `TooLarge`; the rest of
  its batch still goes, the refusal is counted and logged once, and the stream carries on.

- ✅ **Stream ids** are allocated per connection by the host, starting at 1; `WindowId` *is*
  the `CGWindowID` (clients open against a listing they just received, so reuse is harmless).

- ✅ **Cursor channel** samples `CGEventGetLocation` at 120 Hz on the host, re-reads the
  target's bounds at 10 Hz (`CGWindowListCreateDescriptionFromArray` / `CGDisplayBounds`), and
  sends a datagram only when the stream-pixel position or visibility changed.

- ✅ **Client receive path** (`slopty-client::screen`, verified end to end 2026-09-04):
  `WorkerLink` reads every datagram and a `ScreenRouter` fans out by stream id, keeping a
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

- ✅ **Present**: a `CVDisplayLink`-driven tick; the layer keeps `displaySyncEnabled` at its
  default (on), `maximumDrawableCount` 3; present on arrival (superseded 2026-09-05: slop-desk
  vsync-locked hypothesis settled by present-on-arrival measurement without buffering; see ruling
  below). Corrected 2026-09-29: this entry used to say `CAMetalDisplayLink`, `displaySyncEnabled
  = false` and two drawables, which the GPUI fork never did. At the pinned fork (`b2befefba5`),
  `gpui_macos/src/display_link.rs` drives frames from one `CVDisplayLink` per display,
  `gpui_apple/src/metal_renderer.rs` `configure_layer` sets `maximumDrawableCount(3)` and never
  touches display sync, and `present_drawable` goes on the command buffer, synchronously through
  `presentsWithTransaction` only while a window re-activates.

- ✅ **Present on arrival, and the presentation path is measured rather than assumed**
  (2026-09-05). A decoded frame goes up on the first paint after the decoder returns it and is
  never queued for a later one. The alternative — a one-frame playout buffer, which is what a
  video player does — buys evenly spaced pictures at the price of a whole frame of latency on
  *every* frame, and a remote desktop is judged on how long after a keystroke the screen moves,
  not on how evenly it moves. So the only decision left is what to do when the decoder is ahead
  of the display, and the answer is drop, not queue: `Pacer::offer` replaces the frame waiting
  to be painted and counts it (`skipped`) rather than lining up behind it, because by the paint
  after next that picture would already be wrong. The mechanism was mostly there — the `watch`
  channel between the worker and the element keeps only the newest frame — but nothing measured
  it and one path did buffer: a frame the reassembler released inside `tick` (freed when the
  frame ahead of it was given up on) sat in `ready` until the *next datagram* arrived, which on
  a still screen is a heartbeat away. The worker now drains after every tick.
  *Instrument:* the reassembler stamps each complete frame with the arrival of the datagram that
  finished it (`FrameOut::arrived`), the worker parks that instant under the frame's
  presentation timestamp (VideoToolbox's callback is handed nothing else to correlate on) and
  the callback picks it up, so the element receives a `Presentable` and its `Pacer` can close
  the clock when the display shows the frame. The paint only says which frame went up
  (`Pacer::painted`); the window's presentation report says when it reached the glass
  (`Pacer::shown`, through `slopty_ui::shown`, since 2026-09-25; before, the clock stopped at
  the paint and read a refresh or more early). A ring of the last 240 presented frames gives arrival → present
  p50/p95/max, the decoder's share, the paint spacing and its jitter, plus `skipped` /
  `repeats` / `late` — the third line of the ⌘⇧I overlay and `ScreenInfo` in the app self-test's
  dump. The policy is pure with an injected clock (`slopty_client::pacing`), so a steady source,
  a source faster than the display, a source slower than it, a straggler and the ring's own
  forgetting are all unit tests; the element only feeds it a frame on one side and a paint on
  the other. Over a live iroh stream (MEASUREMENTS.md, "arrival → present"): p50 9.0–12.6 ms,
  p95 17.1–20.4 ms, decode 2.6–2.9 ms, `late` 0. A paint interval is 16.7 ms, so
  present-on-arrival predicts p50 ≈ decode + half an interval and p95 ≈ decode + a whole one;
  both land there, and a single frame of playout buffering would have added 16.7 ms to each.
  There is no room in the numbers for a buffer, which is the ruling's evidence.
  *Two things the ordering rests on, both found in review.* The wire carries the low 32 bits of
  the host's microsecond capture clock, which wraps every ~71.6 minutes; widening it with
  `u64::from` would make the first frame of the new turn compare older than the last of the old
  one and freeze the picture until the raw value climbed back past it — up to another 71
  minutes. `pacing::CaptureClock` counts the wraps instead (a step back of more than half the
  range is a wrap forward, a step forward of more than half is a straggler from before one) and
  is applied at the single point where the stamp becomes a `u64`, so the decoder echoes the
  widened value back and the parked arrivals inherit it. And `skipped` has to be counted on the
  decoder's side of the channel: the `watch` between the worker and the element keeps only the
  newest frame, so a burst finishing between two paints reaches `offer` as its last member
  alone and the counter would read zero while the display missed three. Each callback stamps a
  `decode_seq`, and the pacer reads the gaps — the only trace those frames leave.
  Not built: a jitter-adaptive delay. It would only earn its latency on a link whose delivery
  jitter exceeds a frame interval, and the same overlay now says whether that is happening.

- ✅ **NACK delay from the round trip, floor 1 ms, ceiling 20 ms** (2026-09-05). The delay was a
  fixed 3 ms. It exists only to outlast the spread of one frame's fragments on the wire — they
  leave the host back to back, so silence after them means loss, not pacing — and that spread
  scales with the path's delay variation, which scales with its round trip. A constant is
  therefore wrong at both ends: on loopback (RTT ≈ 0.6 ms) 3 ms is two extra milliseconds
  before every repair, and on a 40 ms link it fires while the fragments are still in flight and
  answers a NACK storm with retransmissions nobody was missing.
  `NackDelay { min: 1 ms, max: 20 ms, divisor: 4 }` makes it `rtt / 4` between the bounds — the
  reordering tolerance TCP RACK uses (`min_rtt / 4`), for the same reason. The bounds carry more
  weight than the fraction: without the floor, scheduling noise on a loopback link reads as
  loss; without the ceiling, a satellite path would hold a repairable frame past the point where
  the retransmission could still be shown (`max_hold`, 500 ms, still bounds it). The whole loss
  deadline follows, since it is `delay + retries × (rtt + delay) + grace`: loopback 20.2 → 14.2
  ms, a 40 ms link 99 → 120 ms. The storm gate from the start-up track is untouched — a retry
  still needs a datagram to have arrived since the last NACK, the deadline still only counts
  while newer datagrams are coming in, and an outright stall is still waited out. `tick` derives
  the delay from the RTT it is handed and caches it, so `stalled` and the resume path (which
  have no round trip to hand) use the same number. Tested against a table of fake RTTs
  (loopback / LAN / Wi-Fi / mesh / satellite → 1 / 1 / 3 / 10 / 20 ms), for monotonicity and
  bounds across a thousand round trips, and end to end in the pipeline tests, which now derive
  their own advances from the policy.

- ✅ **Cursor** is a separate channel drawn client-side; capture with `showsCursor = false`.

- ✅ **Audio**: SCK `capturesAudio` → `opus` 0.4.0 (verified; `audiopus` is dead) → `objc2-avf-audio`.

- ✅ **Conversation view from the transcript, not from hooks** (2026-09-05). The hook stream
  says what state the agent is in; it never carries what was said. The JSONL transcript does
  (every record the CLI writes, tool calls included), and its path arrives with the first hook
  payload, so the daemon tails that file for the sessions a client asked about and nothing
  else. Wire: `ClientMsg::Transcript(TranscriptFollow)` to start/stop following,
  `WorkerMsg::Transcript(TranscriptUpdate { reset, entries })` with a 200-entry snapshot on
  reset and appended slices afterwards; entries are already reduced to
  `User{text}` / `Assistant{markdown}` / `ToolUse{name, summary}` on the host so the phone
  never parses JSONL and the wire stays small. `PROTOCOL_VERSION` 9 → 10, goldens
  `client_transcript_follow`, `host_transcript`. Polling at 400 ms rather than FSEvents: the
  file grows in bursts of whole lines, a 400 ms lag is invisible next to the model's own
  latency, and one `Tail` per followed session (byte offset + the partial last line) costs a
  `metadata` call per tick. The view swaps the grid rather than splitting the card because
  the card is already the unit the canvas lays out and zooms; the grid is one ⌘⇧L away.
  Verified by `slopty_agent::transcript` unit tests (13: tail across appends, truncation,
  partial lines, every tool summary) and the headless
  `the_conversation_replaces_the_grid_and_follows_the_transcript`.
  **Correction (2026-09-06):** an earlier version of this ruling said `tool_result` bodies,
  thinking blocks and typing into the conversation were not done. All three are:
  `crates/slopty-ui/src/terminal/conversation.rs` renders `TranscriptBody::Thinking` and
  `TranscriptBody::ToolResult` as folds — a header with the line count, opening on a click, a
  tool result carrying its error state — and the composer under the list types its text into
  the session followed by Enter (↩ sends, ⇧↩ is a newline). Covered by `folds_open_on_a_click`,
  `the_composer_types_into_the_session`, `the_composer_wears_a_focus_ring_only_while_focused`
  and `the_attention_row_and_the_composer_are_read_and_tabbed` headlessly, and by
  `the_conversation_view_reads_and_answers_the_agent` in the app self-test.

- ✅ **Structured driving is Claude Code's own stream-json protocol over stdio, not ACP**
  (2026-09-12, replaces the ⏸ ACP plan). Verified against CLI 2.1.268 with live probes
  (`claude -p --verbose --input-format stream-json --output-format stream-json
  --permission-prompts host --permission-prompt-tool stdio --include-partial-messages
  --replay-user-messages`, working directory the repo, no PTY): stdout is NDJSON of
  `system/init` (session id, model, tools, permission mode, slash commands), `system/status`,
  `system/thinking_tokens`, `system/permission_denied`, hook lifecycle records,
  `rate_limit_event`, `stream_event` (the API's own `content_block_delta`s, so text streams
  token by token), `assistant` and `user` records **in the same shape as the JSONL transcript**
  (so `slopty_agent::transcript` maps them unchanged), `result` (`subtype` `success` |
  `error_during_execution`, `is_error`, `num_turns`, `total_cost_usd`), and
  `control_request {request_id, request: {subtype: "can_use_tool", tool_name, display_name,
  input, description, permission_suggestions, tool_use_id}}` whenever a tool needs the human.
  The host writes `{"type":"user","message":{"role":"user","content":…}}` to prompt,
  `{"type":"control_response","response":{"subtype":"success","request_id",
  "response":{"behavior":"allow","updatedInput":…}}}` or `{"behavior":"deny","message":…}` to
  answer (a denial comes back as an error `tool_result` carrying the message), and
  `{"type":"control_request","request_id","request":{"subtype":"interrupt"}}` for Esc (acked
  with `control_response {still_queued: []}`, then a user "[Request interrupted by user]" and a
  `result`). The process lives across turns until stdin closes; `--resume <session id>` reopens
  a conversation. Prompts fire only for tools the user's settings do not already allow
  (`--restricted` ignores settings; the probes needed it on this machine, whose settings allow
  `Write`). Rejected: ACP (`@agentclientprotocol/claude-agent-acp` is a Node adapter around this
  very protocol, so it would add a runtime and a translation for nothing), and the docs' inferred
  `control_type` shape (wrong on the wire; the shapes above are what 2.1.268 emits).
  **What this buys over observing the TUI:** the conversation streams live instead of a 400 ms
  file tail; a permission arrives with the tool's full input and is answered in-protocol instead
  of by typing into a menu; the composer sends a message instead of keystrokes; Esc is an
  `interrupt`; the agent's session id and cost are known. What it costs: no Claude Code TUI in
  that card (its slash commands still work as messages — `init` lists them). Both kinds
  coexist: a `claude` typed into a shell stays the observed PTY agent it is today.
  Plan, landing in commits: (1) `slopty_agent::stream` — the pure protocol layer (parse every
  record into an `Event`, fold events into `TranscriptEntry`s, `AgentStatus` and
  `PermissionRequest`s, build the outbound lines), fixtures from the probes; (2) hostd runs the
  process per agent session and the wire carries `AgentRequest`/`AgentUpdate` (protocol bump), a
  fake `claude` in `slopty-e2e` replays the fixtures for the self-test; (3) an agent card on the
  canvas hosting the conversation view without a grid, on the Mac and the phone.
  Landed (2026-09-12, protocol 15): (1) as `slopty_agent::stream`; (2) as `hostd::driven` with
  `ClientMsg::{OpenAgent, AgentSay, AgentAnswer, AgentInterrupt}` and
  `WorkerMsg::{AgentPartial, AgentPermission}`, the session a `SessionKind::Agent` in the
  ordinary session list so the canvas, many-clients and reattach machinery is reused unchanged;
  (3) as `TerminalView`'s driven mode (ARCHITECTURE, "Driven agents"). Two rulings made on the
  way: the fake `claude` for the self-test is a Rust binary (`slopty-fake-claude`) handed to the
  host as `SLOPTY_CLAUDE_BIN`, not a shell script on `PATH`, because the host launches the real
  one through the login shell and a test must not depend on the tester's rc files; and the
  driven session keeps ⌘⇧T's PTY `claude` beside it (⌘⌥T / "New Agent (Structured)" opens the
  driven one) until the structured card has proven itself day to day — the TUI's own rendering
  of diffs, todo lists and slash-command output has no equal in the card yet.

- ✅ **The host says when its capture target is idle; the receiver stops asking, protocol 13**
  (2026-09-05). A stream whose target has never drawn (a hidden window) left the client in "need
  refresh", re-sending `RequestRefresh` on a doubling backoff for as long as it stayed hidden —
  79 requests in 10 s on the mesh run (MEASUREMENTS, "screen stream over the mesh"). A
  client-side cap alone is the wrong answer: the client cannot tell "nothing drawn yet" from
  "the link ate my keyframe", and the two want opposite behaviour. So the host answers it —
  `ScreenEvent::Source { stream, state: Idle | Live }`, sent 400 ms after `Opened` when the
  encoder has produced no frame (`ScreenStream::check_source`, polled from hostd's geometry
  loop) and again the instant it produces one. The receiver stops asking while the source is
  idle (`Reassembler::set_source_live`) and the placeholder says "waiting for the window to
  draw…" rather than "waiting for the first frame…", which is also the honest thing to show —
  as a `Role::Status` labelled with that text, since it is the only thing a screen reader has to
  read while the surface is empty and it changes when the host reports the source. A
  cap stays as the fallback for a host too silent to send the hint — `refresh_max_repeats` 12,
  ≈17 s of asking with the backoff. Different evidence clears each: any datagram, heartbeat
  included, restarts the cap (the host is there); only a video fragment lifts the suppression
  (the source drew). Wire:
  `SourceState` + the new `ScreenEvent` variant, goldens `worker_screen_source` (new) and
  `worker_screen_rate` / `client_hello` re-accepted, PROTOCOL_VERSION 12 → 13. Tests:
  `an_idle_source_stops_the_refresh_requests`, `refresh_requests_give_up_after_the_cap` and
  `a_heartbeat_restarts_the_cap_and_a_frame_lifts_the_idle_hint`
  (`crates/slopty-media/tests/pipeline.rs`), `the_placeholder_says_which_end_is_waiting`.

- ✅ **The NACK give-up deadline stays where it is** (2026-09-05). With the loss hook in place,
  loopback at 0/20/50/100 ‰ loses no frame and needs no refresh (MEASUREMENTS, "parity, NACK and
  refresh under injected loss"): there is nothing to tune away, and moving a deadline no
  measurement moves would be churn. The give-up path that does fire in practice is the stalled
  one, and that is already handled by the flow-aware deadline and the stall-restart rule
  measured on the mesh path.

- ✅ **Loss injection is a router hook, not an env var flip** (2026-09-05).
  `ScreenRouter::set_loss(permille)` drops that many datagrams per thousand from a fixed-seed
  LCG before routing, so a rate always drops the same datagrams of the sequence and two builds
  compare on the same losses; `SLOPTY_E2E_DROP_PERMILLE` (read once at construction) is the
  entry point for `slopty bench screen`. Setting it per stream rather than through the
  environment is what lets one test process run three rates back to back — mutating the
  environment in a multi-threaded process is `unsafe` in Rust 2024, and the drop rate is state
  the test owns anyway.

- ❌ **Dropping parity on small frames** (2026-09-06, measured and reverted). The packetizer
  rounds any non-zero parity ratio up to one whole fragment, so a still window — where frames are
  two to five fragments — puts 240–440 ‰ of its data fragment count on the wire as parity the
  controller never asked for. Rounding to the ratio instead (nearest, then a quarter, with a
  floor kept for IDR and LTR-refresh frames) did what it promised: parity seen fell to 47–80 ‰
  against a one-per-frame counterfactual of 285–309 ‰ for the same frames. It also lost frames.
  At 20 ‰ datagram loss the run lost 2 and took 2 refreshes where the minimum loses none, and at
  100 ‰ it lost up to 9: a two- or three-fragment frame with no parity has nothing between a
  single drop and a NACK round trip, and when the retransmission is dropped too the frame is
  gone. The minimum stays, because the trade it makes is the right way round — it is a large
  *percentage* of a stream that is already small (a still window is ~0.5 Mbit/s, so 30 % of it is
  ~150 kbit/s) and costs nothing on the busy screens where bytes are worth something, since those
  frames run to tens of fragments. Group parity across consecutive small frames was not built:
  it needs a wire change and it can only repair once the whole group has arrived, which on a
  still window is hundreds of milliseconds — all of that to save a fraction of 150 kbit/s.

- ✅ **The gated loss test asserts, and says when it cannot** (2026-09-06). Its verdicts used to
  be skipped whenever any row stalled, which before the stall fix meant nearly every run. The
  skip still keys on the stall count, but the count now means something: host and client share
  the test's process, so nothing except the scheduler can hold a loopback datagram for a stall
  gap, and an idle machine reports none. Two guards that look reasonable and are not: the frame
  *count* (a still desktop draws 8 fps because nothing is changing) and the gaps between
  *decoded* frames — a recovery bug that starves decode while datagrams keep arriving would then
  skip every verdict instead of failing one, which is exactly the failure the test exists to
  catch. The evidence has to be at the datagram level, which is where the stall counter is. The
  loss verdict is a rate (under one frame in a hundred), not zero: a two-fragment frame plus one
  parity is beyond repair when two of its three datagrams go, and at 50 ‰ that is about one
  frame in three hundred whatever the policy does.

- ✅ **The refresh guard is checked against a window the test owns** (2026-09-06). The 2026-09-05
  ruling ("an idle source stops the refresh requests") was pinned by media unit tests and by one
  hand-run against a hidden Ghostty window; nothing checked the whole path. It does now, and the
  awkward part was the target: the guard needs a capture source that produces *no* frame at all,
  the app's picker lists on-screen windows only, and no window this repo does not own may be
  touched. So the harness owns one — `slopty-idle-window` (`crates/slopty-e2e/src/bin`), a
  240×160 AppKit window with no content that orders itself in and out on marker files. On screen
  it is listed and pickable; ordered out it is still in the host's window list (the host
  enumerates with `onScreenWindowsOnly: false`) and ScreenCaptureKit has nothing to deliver for
  it, which is exactly the state the guard is about. Two tests use it:
  `a_window_that_never_draws_is_reported_idle_and_stops_the_asking`
  (`apps/slopty-worker/tests/e2e.rs`, gate `SLOPTY_SCREEN_E2E`) for the host and the receiver, and
  `a_remote_window_that_never_draws_waits_instead_of_asking_forever`
  (`crates/slopty-e2e/tests/app.rs`) for the app: ⌘O, pick the row, hide, and then the item's
  `Role::Status` reads "waiting for the window to draw…" while the host counts what it was
  actually asked for. **4 refreshes against the cap of 12**, then silence, and the picture
  returns on its own when the window draws again (MEASUREMENTS.md, "the refresh guard end to
  end"). The host now counts them: `ScreenStats::refreshes`, over the control socket as
  `slopty worker screens` — a client-side counter would only say what the client believes it sent.
  Known limit the tests make visible rather than fix: `check_source` latches on `encoded > 0`, so
  a window that draws once and *then* hides stays `Live` forever and only the cap protects the
  receiver (superseded 2026-09-06: `SourceTracker` follows recent frames, resetting to `Idle` after
  2 s quiet or immediately on off-screen; see "The source state follows the frames, not the first
  one" below). The app case gates on `SLOPTY_SCREEN_E2E` as well as `SLOPTY_APP_E2E`: it is the only
  case in that suite that asks the machine for anything. (This entry first also claimed that a
  hidden window on the display-crop path keeps streaming the desktop behind it. It does not; see
  the crop-path ruling below.)

- ✅ **What a cropped stream must never show** (2026-09-06). The claim in the entry above — that a
  window hidden while on the display-crop path keeps streaming the patch of desktop behind it —
  **was wrong**, and the correction is worth more than the alarm was: it came from a run whose
  helper binary was stale, so the window under test never actually hid (that is also why
  `bin_of` now rebuilds every run). Measured properly, hiding a window on the crop path takes the
  stream off the crop in **0.4–1.4 s** and it encodes nothing at all while the window is away; it
  does not go back onto the crop, and the picture returns when the window does.
  What *is* true is that the swap is bookkeeping with a gap in it: the geometry tick decides, then
  two ScreenCaptureKit callbacks settle, and between the decision and the settle the crop is still
  live. So the rule is now enforced on the frame path rather than by the transition: `Shared`
  carries `target_hidden`, set from `window_on_screen` at the **top** of every geometry tick —
  before `Transitions::settle`, which returns early while a swap is in flight and would otherwise
  leave the guard stale exactly when it is needed — and `on_frame` drops anything captured while
  it is set, counting it as `ScreenStats::withheld`. `ScreenStats` also gained `on_crop` (which
  path the stream is on right now, as against `cropped`, which counts frames that came that way),
  because a test cannot otherwise tell "left the crop" from "the desktop happens to be still",
  and it is filled in one place (`Shared::stats`) so no reader can publish the counters' `false`
  placeholder and report the window filter for a stream on the crop.
  Not changed: `window_on_screen` still reads `kCGWindowIsOnscreen` — see the entry below for the
  measurement that finally settles it.
  Tests: `a_frame_captured_while_the_target_is_hidden_is_withheld` (unit, `slopty-worker`) is the
  statement — a frame handed to `on_frame` while the guard is set is counted as withheld and goes
  no further, and it fails the moment the guard is removed. `a_hidden_window_stops_being_served_
  from_its_crop` (`apps/slopty-worker/tests/e2e.rs`, gate `SLOPTY_SCREEN_E2E`) drives visible →
  crop path → hide → show for real, with a **second window repainting directly behind the
  target**: without that backdrop the framework has no new frame for the rectangle once the
  window goes, so every assertion about what is not sent passes for free. The client's own frame
  count is *not* the evidence either: frames sent legitimately while the window was still up are
  still being decoded seconds later, and asserting on them fails for the wrong reason.

- ✅ **A hide is ~270 ms late, and the crop keeps that latency anyway** (2026-09-06).
  With something repainting behind the target, the same test measures **12–15 crop frames sent
  after AppKit ordered the window out** — not the zero the entry above reports, which was measured
  against an empty rectangle. The cause is not this code: every way of asking CoreGraphics whether
  a window is on screen keeps saying yes for **267–279 ms** after `orderOut` returns, measured by
  a 2 ms probe as well as by the 100 ms geometry tick, and the window's own `kCGWindowIsOnscreen`
  and membership of the on-screen list flip in the same millisecond as each other. So no polling
  predicate can meet "within one geometry tick", and the guard above does not cover this gap
  either — by the time the host knows, the framework has stopped on its own.
  What bounds it today is the crop filter itself: it is
  `initWithDisplay:includingApplications:exceptingWindows:` restricted to the target's **owning
  application**, so in principle only that application's other windows can appear. The test does
  not confirm that — its backdrop is a bare executable with no bundle identifier, and copying it
  to a second path did not make ScreenCaptureKit treat it as a second application — so whether a
  genuinely different app can appear in the crop is unresolved.
  **The display-crop path stays on by default.** This is one ruled measurement against another,
  not a mistake to fix: `CROP_WINDOWS` buys ~6 ms of capture→decoded latency (2026-09-05,
  "capture floor", 9.0 → 1.7/2.6 ms p50), which is the point of the product on that axis, and
  turning it off would spend that everywhere to close a window that is ~270 ms long, bounded by a
  CoreGraphics flip no amount of polling beats, and scoped by the filter's construction to the
  target's own application. Against it stand the guard, which closes the part this side owns, and
  the e2e's ceiling of 30 frames, which makes a regression in detection visible. Recorded so the
  trade is re-openable rather than rediscovered.
  Both of the questions this entry left open are answered in the two entries below.

- ✅ **A crop cannot be made to show another application** (2026-09-06). The open question from
  the entry above, answered with a fixture that is a real second application: the idle-window
  helper inside a minimal `.app` with its own `CFBundleIdentifier`, ad-hoc signed, because a copy
  of a bare executable at another path is still the same application to ScreenCaptureKit.
  It also replaces the instrument. Frame *counts* cannot answer this — the framework delivers the
  crop rectangle at the frame rate whether anything in it changed or not, so an empty rectangle
  produces as many frames as a busy one, which is what the control run shows. The pictures are
  read instead, as one number: the mean brightness of the client's decoded frames. With the
  target up and flashing in the crop that measure swings by 136; once the target is ordered out
  it goes to **zero in all three cases** — nothing behind, a window of the same executable, and a
  window of another application. The crop carries black, and none of the window behind reaches
  the client (MEASUREMENTS.md, "what the crop shows"). So the ~270 ms gap costs the client a
  black rectangle, not somebody's content, and the default in the entry above stands on firmer
  ground than when it was taken.
  Tests: `what_a_crop_shows_of_another_application`, `what_a_crop_shows_of_an_empty_rectangle`
  and `a_hidden_window_stops_being_served_from_its_crop` share one driver and differ only in what
  is behind the target; the empty rectangle is kept as the control that makes the other two mean
  anything.

- ✅ **The accessibility API knows about a hide 260 ms before core graphics does, and the
  stream holds frames on its word** (2026-09-06, measured; wired the same day). Polled every
  2 ms from one thread against the same order-out: accessibility answers in **0 ms**,
  `kCGWindowIsOnscreen` and the on-screen list in **256–266 ms**. It is now the first guard on
  the crop path: `slopty_capture::HideWatch` puts an `AXObserver` on the window's application
  (its own `CFRunLoop` thread; `AXWindowMiniaturized` and `AXApplicationHidden` on the
  application element, `AXUIElementDestroyed` on every window element — that is what AppKit
  posts for a window it orders out — and `AXWindowCreated` to put new windows under the same
  watch). Its callback opens a `SUSPICION_HOLD` of 400 ms on the stream, in which `on_frame`
  holds every frame (`ScreenStats::suspected`, apart from `withheld` so a hide shows where it
  was caught); the geometry tick's `target_hidden` is the confirmation that outlives it. With it
  on, the three crop-hide tests send **0 crop frames after the order** and **0–2 after the
  hide was asked** (13–28 before), and 0–1 pictures of the gap reach the client
  (MEASUREMENTS.md, "the accessibility hide watch"). Two things were worth writing down rather
  than rediscovering:
  * **It cannot name the window, but it can match it once.** The public accessibility API
    exposes no window number for an `AXUIElement`; the call that does, `_AXUIElementGetWindow`,
    is private and out under the no-private-API rule. So accessibility can say "a window of that
    application went" but not "*the* window went" — except that at registration the watch reads
    every window's `AXPosition`, `AXSize` and `AXTitle` and keeps the element whose frame (within
    1 pt) and title are the target's from the window list (`kCGWindowBounds`, `kCGWindowName`),
    and from then on tells that element from every other by identity (`CFEqual`), since an
    element's identity is stable however the window moves. Its going, its minimising and the
    application being hidden are the *suspicion*: hold frames at once, let core graphics confirm
    or deny inside the hold. Any other window of the application going is `Went::Other`
    (`ScreenStats::siblings`): no hold, only the wake the next ruling describes. Amended
    2026-09-06 after `a_popup_of_the_application_closing_raises_no_suspicion` showed the
    unmatched watch treating a borderless pop-up panel's going — an autocomplete list, a
    tooltip — as the target's, a 400 ms freeze on every one; measured after the change, a
    pop-up or sibling closing is no suspicion and the stream flows straight through
    (MEASUREMENTS.md, "pop-ups and siblings against the targeted watch"). When nothing matches —
    the application does not list the window, or it moved between the two reads — every window
    of the application counts as the target, as before (`HideWatch::targeted` says which; the
    watch logs it). Focus and main-window changes are *not* suspicions: they fire on every
    switch between an application's windows and would freeze a multi-window target constantly.
    The hold is 400 ms because the confirmation is the 256–266 ms lag plus one geometry tick
    (≈100 ms).
  * **The constants are string literals.** Ruled in "Constants the SDK defines as `CFSTR`
    macros" below; the spelling lives once, in `crates/slopty-capture/src/ax.rs`.
  * **Without accessibility trust there is no watch** (`AxError::NotTrusted`, logged once per
    stream) and the crop carries black for the ~260 ms, as measured before: the tests assert the
    hold when `suspicions > 0` and fall back to "dark and still" otherwise, so they say something
    on either kind of host.
  Also considered and not measured: `SCShareableContent` and the stream delegate are the same
  window-server data core graphics reads, so they cannot be earlier than it; window-server
  notifications are private API and out for that reason; and deciding a hide from the frames
  themselves (the crop going black) is a guess about content that a dark window would trip.

- ✅ **The host's cursor picture rides on the control stream** (2026-09-14). The client drew
  the host's pointer as one drawn arrow whatever the host showed: an I-beam over a text field,
  a resize arrow on a window edge, a busy pointer — all arrows, and the wire type for the
  picture (`ScreenEvent::Cursor`, `CursorShape`) had no sender and no reader. Rulings:
  (1) the picture comes from `NSCursor.currentSystemCursor`, the one public reading of a
  cursor another process set; Apple has deprecated it in favour of `showsCursor` on the
  ScreenCaptureKit stream, which would bake the pointer into the video and tie its latency
  to the pipeline the cursor channel exists to avoid, so the deprecated reading is used
  (`#[expect(deprecated)]` with that reason) and a client keeps drawing its own arrow when it
  answers nothing — the live test `the_system_cursor_reads_as_a_small_picture_with_its_hotspot_inside`
  (`SLOPTY_SCREEN_E2E=1`) is the check that it still answers on the floor OS; (2) the
  picture is read on its own task (`shape_loop`, 30 Hz while the position loop says the
  pointer is over the target), never in the position loop: the first read in a process
  takes 11 s (AppKit's connection to the window server; the second takes 200 µs), and hostd
  pays it at start-up (`slopty_capture::warm_cursor`) beside the capture warm-up; (3) it is
  sent only when it changed (`ShapeDedup`, a byte comparison — 18 KB for a 2× arrow, at
  most every 33 ms, in practice on a cursor change), on the control stream because a
  picture is bigger than a datagram and must arrive whole and in order, while the position
  stays on the datagram channel; a read of `None` (hidden, or unsupported) sends nothing,
  since the position channel already says whether to draw; (4) the representation read is
  the smallest at or above the display's backing scale: a cursor image carries 1×, 2×, 5×
  and 10× reps for the pointer-size setting, and the 10× one is 448 KB; (5) the client draws
  the picture at its point size (pixels over the backing scale) with the hotspot on the
  position, the same fixed size the arrow had, not scaled with the card — a pointer is not
  part of the picture; (6) the pixel reader (`Layout`, `bgra_premultiplied`) takes every
  32-bit CoreGraphics layout (both byte orders, alpha first or last, straight or
  premultiplied, padding alpha) and turns it into the wire's premultiplied BGRA, and refuses
  anything else rather than misread it. Tests: `slopty-capture::cursor::tests` (each layout,
  straight alpha, padded rows, short data, rounding), the host's `shape_tests` (one send per
  change, a blank read changes nothing), the proto golden `worker_screen_cursor`, and headless
  `the_workers_cursor_picture_is_drawn_at_its_hotspot` (size and hotspot in points, short bytes
  and `None` put the arrow back).

- ✅ **The client's own pointer hides over a card that shows a frame** (2026-09-15). With the
  host's pointer drawn on the card, the client's arrow sat a round trip ahead of it, and the
  pair read as lag. GPUI had no cursor style that hides the pointer, so the fork gained
  `CursorStyle::None` (fc045ebc): on macOS it registers a cursor rect whose `NSCursor` is one
  clear pixel, so AppKit brings the arrow back on its own when the pointer leaves the view —
  a balanced `NSCursor hide`/`unhide` pair could leave it hidden over another app if the
  window lost the pointer without a style change; Linux and web map to the CSS `none`,
  Wayland's shape protocol and Windows have no hidden shape and fall back to the default
  arrow. The card sets it while a frame is up (`local_pointer`): the host's pointer is drawn
  there, its picture or an arrow, or nothing when the host hides it (a hidden host pointer
  is then mirrored rather than replaced by ours), and before the first frame the arrow stays
  since there is nothing to point at. Test: `the_local_pointer_hides_once_a_frame_is_up`.
  - Amended 2026-09-28: the pointer drawn over the frame is no longer always the host's
    sample. While this client drives it (always on a window stream, and for a hold after its
    last move on a display), it is drawn at the client's own point in the host's cursor
    picture, so the hidden arrow and the drawn one no longer sit a round trip apart
    (`docs/decisions/input.md`, "A window stream's pointer is where its input put it",
    amended).

- ✅ **The source state follows the frames, not the first one** (2026-09-06). `check_source`
  decided `Live` from `encoded > 0`, a latch: a window that drew once and was then hidden, or
  closed and left up, stayed `Live` for the rest of the stream, and the receiver — which stops
  asking for refreshes only while the source is idle — had nothing but its cap of 12 to protect
  it. It now follows recent history, in a `SourceTracker` that takes the clock as an argument so
  the rule is unit-tested rather than slept through:
  * a stream that has produced nothing says nothing for `SOURCE_IDLE_AFTER` (400 ms), then `Idle`
    — unchanged, and still the answer to "is it just starting up";
  * any new frame is `Live` at once;
  * `SOURCE_QUIET_AFTER` (2 s) without one is `Idle` again. Deliberately longer than the 400 ms:
    a target that draws once a second would otherwise flap and spend a control message on each
    change, and the receiver only needs to know before it starts asking;
  * a target the geometry tick reports off screen is `Idle` immediately, with no quiet period —
    a window that is not on screen cannot be drawing, whatever the counter says, and that is the
    same evidence the crop guard acts on.
  Tests: five in `crates/slopty-worker/src/screen.rs` (never drew, first frame, drew-then-stopped,
  a slow target that must not flap, hidden), and the `slopty-worker` e2e now drives hide → show →
  hide and asserts the host reports `Idle` the second time — which under the latch it never did. No
  wire change: the states are the ones `SourceState` already had.

- ❌ **The pacer is not clumping frames under load** (2026-09-06), retiring the leftover the flap
  track raised. The claim was that 19–38 datagram stalls per 90 s under every load shape, against
  "none on an idle machine", meant frames were being released in bursts. Both halves were wrong.
  The comparison was a 90 s window against the loss test's 5 s ones, and the flap harness with
  **no load at all** produces 31 stalls in 90 s — as many as the loaded shapes, two of which
  produce fewer (3 and 4). The harness now records both ends of the run, and the host is clean
  under all five shapes: the datagram queue never fills, next to nothing is dropped, and `hold`
  p50/p95 are 0 ms, so frames arrive whole rather than dribbling. Running the same all-core burn
  in other processes instead of in the receiver's own (`Shape::CpuExternal`, `/usr/bin/yes` per
  core) changes nothing either, which rules out the harness starving itself.
  So nothing changed: no thread QoS for the capture path, no pacer burst cap, no send spreading.
  What the numbers do leave is a different question — a quiet loopback stream charges the link
  about a third of a stall a second whatever the machine is doing — and that is about the stall
  detector's pessimism rules (a gap it cannot attribute is charged to the link on purpose,
  2026-09-05), not about pacing. `Shape::None` is kept as the baseline row precisely so the next
  reading of these numbers starts from it.

- ✅ **The stream's rate, ceiling and depth are settings** (2026-09-15). `[remote] fps |
  max_bitrate_mbps | hdr` in `settings.toml` (defaults 60, 30, off: what every stream asked
  for before). They ride on `Theme::behaviour.stream`, so the settings poll delivers them
  the way it delivers colours: a new stream opens at them (`quality_of`, the scale still
  from the canvas zoom), and a live one hears a `SetQuality` from `ScreenView::set_theme`
  at the scale it holds — a bitrate change is applied in place on the host, a rate or
  depth change rebuilds its encoder, as `set_quality` already ruled. `hdr` selects HEVC
  Main 10 (P010 capture); it is the client's choice, since the host cannot know what the
  client's display shows. (`hdr` superseded 2026-09-24 and removed from the settings
  2026-09-25, see "420f and BT.709 end to end" and "HDR is not carried".) Out-of-range values (fps outside 15–120, a ceiling outside 1–200
  Mbit/s) read as the defaults, as the font sizes do. Tests: `remote_keys`,
  `remote_settings_ride_on_the_theme`, `new_stream_settings_are_asked_of_a_live_stream`.

- ✅ **The frame rate follows the bitrate: a cadence ladder of ceiling → 60 → 30 → 15** (2026-09-15).
  A bitrate is a budget per second and the encoder spends it on however many frames it is handed,
  so a path that has collapsed to the 1 Mbit/s floor was being asked for sixty frames of 2 KB each.
  Every one of them is smeared and none is worth the bandwidth it cost. The collapsed-window run
  (MEASUREMENTS.md, "QUIC held frames for seconds") already shows the shape of it: the congestion
  guard dropped 1016 of 1149 captures, an effective 4.5 fps, but it dropped them wherever the send
  buffer happened to be full, and the encoder went on sizing every frame for a 60 fps stream.
  `slopty_media::Cadence` now picks the rung — the client's `fps` ceiling, then 60, 30 and 15 — as
  the fastest whose frames each get 8 KB at the target in force, and the cadence gate admits a
  capture only when the rung's schedule has a slot for it (`slopty_media::Pace`; it was a period
  past the last encoded capture until 2026-09-26, see "The capture follows the display's beat"). Climbing back costs 12 KB a frame rather than
  8, so the ladder does not flap around one threshold. Three consequences, all deliberate:
  capture keeps running at the ceiling, so a change on screen is still seen within one display
  beat and only the *sending* slows; `frame_fits` reads the rung, so its two-frame budget is two
  frames of the new cadence and QUIC may hold 133 ms at 15 fps against 33 ms at 60 — worse than
  the old floor sounds, but the alternative at that rate is refusing nearly every frame; and the
  67 ms gaps would read as stalls to the receiver were the host not already heartbeating every
  25 ms of silence, which it is (`HEARTBEAT_AFTER`, half of `STALL_GAP`).
  The rungs and the ladder's shape are ruled. 🔬 The 8 KB and 12 KB thresholds are arithmetic, not
  a measurement: they are where a 1080p HEVC screen frame stops holding text, judged from the
  58 KB keyframes and ~16 KB P-frames this encoder produces at 8 Mbit/s. The collapsed-link arm
  that would tune them needs a degraded path, which the 9.5 ms mesh cannot be made into without
  `dnctl`. Tests: `the_cadence_drops_a_rung_when_a_frame_can_no_longer_hold_8_kb`,
  `climbing_back_costs_more_than_leaving_so_the_ladder_does_not_flap`,
  `the_ladder_never_passes_the_ceiling_the_client_asked_for`,
  `a_rung_under_the_display_rate_keeps_its_rate_on_the_displays_beat`,
  `a_collapsed_target_takes_the_stream_down_the_cadence_ladder`,
  `the_cadence_gate_hands_the_encoder_one_capture_a_period`.


- ✅ **The decoder has not regressed; a slow first session is the volume the binary runs from**
  (2026-09-15). The shaped ladder read five rungs of zero decoded frames and a warm-up that took
  28–31 s where 2026-09-05 measured 150–400 ms, which looked like the worst latency defect in the
  project: the app calls `warm_up_decoder` at launch, so half a minute of blank first window. It is
  not in this code. The same test binary takes 118 s from the repo's external volume and 0.56 s
  from `/tmp`, unmodified, and 0.50 s from a disk image on that same external disk attached with
  `-owners on`. That comparison also moved the binary to a small directory, and the directory size
  is the real cause (`tooling.md`, "A test binary's directory, not the volume", 2026-09-30). Nothing in `slopty-codec` or `slopty-client` is owed a change, and the daemons
  never showed it because `bench screen` runs the installed host off the boot volume.

  What the ladder's harness keeps from the episode: it waits for `slopty_codec::warm_up` before the
  first rung instead of firing it off, and throws the first sample away. Both are right regardless
  of the cause — a measurement should not race its own warm-up — and on the boot volume they cost
  a second rather than half a minute.

- ✅ **420f and BT.709 end to end, one stream format, no HDR option** (2026-09-24, protocol 49).
  The host asked ScreenCaptureKit for `420v` while the client's decoder asked for `420f`, so
  VideoToolbox converted the range into a second pool on every frame (~12 MB read and written
  per 4K frame), and the fork's shader hard-coded BT.601 on a matrix nobody had set. Now:
  capture is `PixelFormat::Nv12Full` with `colorMatrix = kCGDisplayStreamYCbCrMatrix_ITU_R_709_2`
  (SCStream.h leaves the default unsaid, so it is set rather than assumed); the encoder sets
  `kVTCompressionPropertyKey_YCbCrMatrix = ITU_R_709_2`, and VideoToolbox writes the full-range
  flag from the `420f` source by itself; the decoder's output attributes ask for that same
  `420f`, so it writes its output directly. The fork's shader reads the matrix tag off each
  decoded buffer (`ui.md`, the fork entry). Colour primaries and transfer stay untagged: the
  capture is in the host display's colour space, and GPUI's layer does no colour matching.
  The `hdr` setting and `VideoCodec::HevcMain10` are gone. The client forced 8-bit output into
  an 8-bit layer, so Main 10 cost bandwidth and showed nothing. `PixelFormat::{Nv12, P010}` went
  with them. Tests: `the_stream_is_full_range_bt709_end_to_end` (the keyframe's parameter sets
  say full range and BT.709, and the decoded buffer is `420f` tagged BT.709),
  `the_capture_is_full_range_bt709`, `configs_clamp_the_quality_and_keep_even_sides`.
  (What this left behind, the always-false wire fields and the unread setting, went
  2026-09-25: "HDR is not carried".)

- ✅ **The LTR-refresh flag rides with its own frame, in `sourceFrameRefcon`** (2026-09-24). The
  encoder kept one `pending_refresh` atomic, set before `VTCompressionSessionEncodeFrame` and
  taken by the next output callback. With frames in flight, or a submit that failed, the flag
  landed on whichever frame came back next. The receiver restarts decoding on an `LTR_REFRESH`
  frame (`reassemble.rs`), so a mislabelled P-frame there is a corrupted picture. The refcon
  VideoToolbox hands back with each frame (VTCompressionSession.h: "sourceFrameRefcon: Your
  reference value for the frame") now carries a tag for refresh frames, never a pointer. A
  refresh frame the encoder drops is not carried forward: the receiver repeats its request until
  a picture it can decode arrives. Test: `the_refresh_flag_rides_with_its_own_frame` (12 frames
  in flight, the refresh on the 8th; only its pts comes back flagged).

- ✅ **An encoded frame is copied twice on the host and once on the client** (2026-09-24). Host:
  the sample's bytes go once into a vector sized for the parameter sets plus the body. The
  4-byte length prefixes are rewritten to start codes in place (both are four bytes), and a
  length that runs past the end is an error that drops the frame, where it used to be silently
  truncated. The packetizer then writes every datagram, parity included, into one buffer laid
  out as the wire wants it and hands out `Bytes` slices of it: one allocation per frame, not one
  per fragment. Client: one `memchr` scan splits the access unit into parameter sets and units,
  and the units are written length-prefixed straight into the `CMBlockBuffer` CoreMedia
  allocates. The old path scanned byte by byte twice and copied into a vector and then into the
  block. Numbers in `docs/MEASUREMENTS.md` (2026-09-24, "the media path's copies"). Tests:
  `length_prefixes_become_start_codes_in_place_and_back`, `a_malformed_length_is_an_error`,
  `the_datagrams_share_one_buffer_and_rebuild_the_frame`.

- ✅ **The host's per-stream timers sleep instead of polling** (2026-09-24). `beat_loop` woke at
  120 Hz to ask whether 25 ms of silence had passed. It now sleeps until the moment a heartbeat
  would be due, which every datagram moves on, so a streaming window wakes it about once per
  `HEARTBEAT_AFTER`. A beat the full queue refused still counts as traffic, so a full queue is
  retried a period later and not spun on. `cursor_loop` still ticks at 120 Hz, but a tick with
  the pointer still costs one read of `CGEventSourceCounterForEventType` over the move and drag
  types (43 ns) instead of a `spawn_blocking` hop and a `CGEventCreate` (13.8 µs plus the hop).
  The pointer is read only when those counters moved, and the target's bounds are still read
  at 10 Hz, since a window moving under a still pointer moves the cursor across the picture.
  Tests: `a_beat_is_due_when_the_silence_reaches_the_promise`,
  `a_refused_beat_is_retried_a_period_later`, `a_slow_geometry_call_does_not_make_the_beat_late`.

- ✅ **The stream worker never waits on the control channel** (2026-09-24). Its receiver report
  went out with `send().await` on the bounded control channel, so a full channel stopped
  reassembly, NACKs and decode behind a report. It uses `try_send` now: a full channel drops that
  report (the next follows in 50 ms), and the counters are published either way. Test:
  `a_full_control_channel_does_not_stall_the_worker`.

- ✅ **A still picture keeps its newest capture, and the stream sends it when nothing newer
  will** (2026-09-25). ScreenCaptureKit hands over only frames whose content changed
  (`SCFrameStatus::Complete`), and the capture callback used to drop anything the cadence gate,
  the congestion guard or a keyframe deferral held back. So the last frame of a scroll that
  landed inside the rung's period never went out, and a refresh asked for on a still window had
  no frame to ride on: the client stayed on a wrong picture until something moved. Now
  `Shared::held` keeps the newest capture of the target, and `owed` says it has not reached the
  encoder. `repair_loop` sleeps until `repair_at`: the later of the cadence's next due time and
  the moment the picture counts as quiet (1.5 capture periods after the held capture, never
  under 25 ms, so a ceiling above the panel's rate cannot call the gap between two ordinary
  frames a stop). A keyframe waits only for the quiet. The repair runs the same gates as a
  fresh capture, stamps the frame with the time it goes (the encoder wants presentation times
  that only go forward, and every encode goes through the held frame's lock), and is counted in
  `ScreenStats::repaired`. A hidden or suspected target drops the held capture rather than send
  a picture from before the hide, and a rebuild drops it because it is the old size.
  Holding a surface costs a slot in the pool, so `queueDepth` goes from 2 to 3: one held, one in
  the encoder for its ~7 ms, one for ScreenCaptureKit to render into. The 2026-09-05 survey
  already has 3 on the window filter at 0.49 ms p50 against 0.46 at 2, inside its own
  run-to-run spread. This supersedes "`queueDepth` stays 2". Tests:
  `a_still_picture_sends_its_held_capture_when_owed_or_asked`,
  `a_moving_picture_is_never_repaired_ahead_of_its_next_capture`. Owed: the capture floor and
  `bench screen` at depth 3 against the installed worker (MEASUREMENTS.md, 2026-09-25, "the
  held capture, the audio lane").

- ✅ **A long-term reference is usable only until the next keyframe or rebuild, and the keyframe
  deferral episode ends when the link recovers** (2026-09-25). `ltr_acked` latched on the
  stream's first acknowledgement and stayed true through every IDR after it, though an IDR
  empties the reference list and a refresh asked for then comes back as another IDR (the
  "starved reference" measurement of 2026-09-15). `LtrBook` now records each token the session
  offers with the keyframe epoch it came in, and a token is usable when it was acknowledged and
  no keyframe has been encoded since. Acknowledgements for tokens this session never offered no
  longer reach the encoder, and a rebuild (resize, codec, rate) clears the book, the queued
  acknowledgements, the keyframe estimate and the valve's clock. `keyframe_admitted` defers a
  keyframe only while a reference is usable. The drop path no longer looks at references at
  all: a refresh is budgeted as the IDR it may turn out to be, admitted while no keyframe has
  been measured, then by the drain budget and the valve (`standalone_fits`). The control socket
  carries `ScreenStats::ltr`: tokens offered and acknowledged, refreshes answered as IDR and as
  delta, and whether a reference is usable now with its age. Those counters are what decides
  whether the supply of references is the problem, and they need a shaped-ladder run to read.
  Separately, the one-second valve only reset when a keyframe was encoded. An episode that ended
  because the link could carry a keyframe again left its start standing, and the next collapse
  found the valve a second past and let its first keyframe straight through.
  `standalone_fits` now clears the clock whenever a keyframe fits. Tests:
  `a_reference_is_usable_until_a_keyframe_or_a_rebuild`,
  `a_link_that_recovers_ends_the_deferral_without_the_valve` (now with a second collapse),
  `a_keyframe_is_deferred_only_when_a_refresh_can_go_out_instead`,
  `a_dropped_frame_asks_for_a_refresh_only_when_the_link_could_carry_one`.

- ✅ **One desired capture configuration, sent whole through one transition; a resize waits for
  the size to hold** (2026-09-25). A resize used to rebuild the encoder and call
  `updateConfiguration` with the new size and the *committed* crop, outside the transition
  machinery, while a crop move for the same tick was still in flight with the old size. The
  later call could leave a stale `sourceRect` in force. Now the pipeline keeps `desired` (size,
  rate, format and crop together) and `desired_path`. `follow_window` only records the wish,
  and `apply_desired` sends the whole configuration through `Transitions`, one at a time, with
  the old call order for a path change. A transition commits the configuration it carried, so
  what the stream records is what ScreenCaptureKit was sent. A live corner drag changed the size
  on every 100 ms tick and rebuilt a VideoToolbox session each time, 10 IDRs a second.
  `ResizeDebounce` rebuilds only for a size seen on two ticks in a row, so a drag costs one
  keyframe at its end and at most 100 ms on a settled resize. Tests:
  `a_transition_carries_the_size_and_the_crop_as_one_configuration`,
  `a_resize_rebuilds_only_once_the_size_holds_for_a_tick`, plus the two transition tests.

- ✅ **Small rate and cadence fixes on the worker** (2026-09-25). A bitrate-only `SetQuality`
  moved the encoder's rate but not the cadence rung; it now calls `apply_cadence` like a rate
  decision does. `force_ltr_refresh` on a session without long-term references was a plain
  P-frame off the picture the receiver lost, so a client waiting on a refresh waited for good;
  `slopty_codec::encoder::submission` makes it a keyframe then, and a frame that is already a
  forced keyframe is not also flagged as a refresh (test
  `a_refresh_without_long_term_references_is_a_keyframe`). The encoder now gets
  `target × 1000 / (1000 + parity)` (`encoder_bps`, test
  `the_encoder_gets_the_target_less_the_parity_share`), re-applied when the parity ratio moves,
  since the parity rides the link the target describes. 🔬 That is proportional only; whether
  less parity at a higher encoder rate reads better is an A/B still to run. The guards keep
  reading the full target, because the bytes they weigh include parity. The video capture queue
  is `UserInteractive`, like the audio queue: its callback gates the frame and submits it to the
  encoder. Not measured; owed with the capture floor above. The cursor loop reads the stream's
  zoom from `Shared` on every sample, so a quality change rescales the cursor as it already
  rescaled the injector. `Pipeline::zoom` / `StreamControl::zoom` expose it for mapping client
  input.

- 🔬 **Audio goes ahead of queued video, without a protocol change** (2026-09-25). QUIC's
  datagram queue is first in, first out and noq has no priority for datagrams, so audio sent
  behind a keyframe waited for all of it: 130 kB is 52 ms of a 20 Mbit/s link, more than the
  player's jitter slack. While audio flows (a packet in the last 200 ms), video goes through
  `Lane`, and `lane_loop` tops QUIC up every millisecond to 5 ms of the rate QUIC has been seen
  to drain at. That rate comes from QUIC's held bytes between two looks, never lower than the
  controller's target, because a budget sized from the target would pace a LAN down to it.
  Audio and cursor datagrams go straight in and wait behind at most that slice. Video keeps its
  order whichever thread tops it up, and the guard and the NACK gate count lane bytes as held.
  With no audio flowing the lane is empty and video goes straight in as before. This is the
  first queue of the worker's own since "media datagrams go to QUIC from the thread that made
  them", and only while audio flows. Measured on a model of the link, not yet on one
  (MEASUREMENTS.md, 2026-09-25): audio behind a 130 kB keyframe waits 52 ms → 4 ms at 20 Mbit/s
  with the keyframe done at the same millisecond, and on an 800 Mbit/s link the first keyframe
  finishes 1 ms later while the rate is learned. Test:
  `audio_waits_behind_a_slice_of_a_keyframe_not_all_of_it`. Owed: the shaped ladder with audio
  playing.
- ✅ **A decoder failure asks for a refresh, a lost session rebuilds, and only a decoded picture
  acknowledges its long-term reference** (2026-09-25, no protocol change). The VideoToolbox
  callback reports every bad status, dropped frame and missing image instead of returning
  silently; the stream worker then sends one immediate `Feedback::Refresh` through
  `Reassembler::force_refresh` and drops frames until the restart. On -12903/-12911 (sleep,
  background, media-server reset) the decoder drops its session and parameter sets, so the next
  keyframe rebuilds even with identical sets. A token is acknowledged when its picture comes
  back, never on submit, and each report repeats the newest acknowledged token while frames
  flow, so a lost report heals; a report refused by a full channel is handed back into the next.
  After a lost session only an IDR helps, but the worker answered `Refresh` with a
  long-term-reference refresh once any token was acknowledged, which a fresh session cannot
  decode (-17694). Fixed by "A refresh says when only a keyframe will do".
- ✅ **A refresh says when only a keyframe will do** (2026-09-25, protocol 57).
  `Feedback::Refresh` carries `keyframe`, set while the reassembler waits in `Need::Keyframe`:
  before the stream's first picture, and after `force_refresh(_, true)` (a lost decoder
  session). A reassembler already waiting on an LTR refresh when the session goes asks again at
  once, since the refresh it asked for is no longer decodable, and its repeats keep the flag.
  The worker answers a keyframe refresh by retiring its usable reference (`LtrBook::client_lost`:
  a new epoch, so a late acknowledgement from the lost session names nothing) and setting
  `pending.keyframe`. The IDR then goes through the usual keyframe gates: the cadence rung
  never holds a keyframe back, the congestion guard still does, and with no usable reference
  `keyframe_admitted` has nothing to defer it for. A new flag, not a second variant, because
  both ask for the same thing (a picture that stands on its own) and differ only in what the
  client can still predict from. Tests: `a_keyframe_refresh_is_an_idr_even_with_a_usable_reference`
  (worker), `a_lost_session_while_waiting_on_a_refresh_asks_for_a_keyframe` and
  `a_decoder_failure_forces_a_refresh_and_a_lost_session_a_keyframe` (reassembler), the
  `client_refresh` golden.
- ✅ **HDR is not carried** (2026-09-25, protocol 57). `ScreenEvent::Opened.hdr` and
  `DisplayInfo.hdr` were always false after the 2026-09-24 ruling, and the `[remote] hdr`
  setting (`RemoteSettings`, `StreamPrefs`) no longer reached a stream. All three are gone,
  with no compatibility: a settings file that still names `hdr` gets the usual unknown-key
  warning. Every stream is 8-bit HEVC Main, 420f, BT.709. HDR comes back only with a pipeline that
  shows it (a 10-bit capture, an EDR layer on the client, tagged primaries and transfer), and
  that change adds its own fields.
- ✅ **A refresh that fails to decode is answered with a keyframe** (2026-09-25, no protocol
  change). The client answered a decode failure at or after the last restart with an LTR
  refresh unless the session was gone. When the failed frame was that refresh, the next one
  predicts from the same references and can fail the same way, so the stream could ask
  forever. Each parked frame now records whether it restarts decoding (a keyframe or an LTR
  refresh), and the folded failure keeps that. `refresh_for` asks for a keyframe when a
  restart frame was among the failures, and a synchronous `decode` error on a restart frame
  does the same. Test: `a_failed_refresh_asks_for_a_keyframe`.
- ✅ **A rebuild swaps the encoder and resets its session state in one step** (2026-09-25).
  `reconfigure` installed the new encoder and only then reset the LTR book and the pending
  requests. A frame encoded in between reached the new session without the rebuild's keyframe,
  so that keyframe went out later as a second IDR. The new session's tokens also landed in the
  old book, the reset wiped them, and the worker rejected the client's first acknowledgements.
  `Shared::install` now swaps and calls `rebuilt` under the encoder's write lock, and
  `try_encode` holds the read lock from reading the requests to submitting the frame. The swap
  drops the old session, and its invalidation ends its callbacks before the reset. A report
  queues acknowledged tokens while it still holds the book that vouched for them, and
  `rebuilt` clears the queue under the same lock, so a report lands wholly before or after a
  rebuild. The held capture goes after the write lock is released, because an encode takes
  that lock before the encoder's. Test: `a_rebuild_is_one_step_for_an_encode_beside_it`, which
  uses a recording encoder on a test platform, holds the rebuild at the book and tries an
  encode beside it. The report ordering holds by construction; no test can force that
  interleaving.
- ✅ **The audio lane counts only what QUIC took** (2026-09-25). `Lane::take` added every
  datagram it handed out to the bytes QUIC should hold, before `send`. A datagram the transport
  refused (too large for the path, or a closed connection) then read as drained at the next look
  and raised the rate the slice is sized from. `Shared::send` now returns what the transport
  took (`Taken`), and both the lane and the straight path add only those bytes. Test:
  `a_refused_datagram_is_not_counted_as_sent`.

- ✅ **Encoder sessions are built and dropped off the runtime** (2026-09-26). A stream built its
  VideoToolbox session on its own task, on a runtime worker thread: at open, at a quality
  change that is more than a bitrate, and when its window is resized. The swap also
  invalidated the old session there, which waits for its callbacks. Each held the thread for
  3.5 to 4.3 ms at the median, 42 ms at worst, and everything queued behind it waited too. Both
  now run on the blocking pool (`build_encoder`, `retire`), so `set_quality` is async, and the
  thread is held for 2 to 3 µs (MEASUREMENTS, "encoder sessions off the runtime").
  The thread was free, but the stream's own task still awaited a resize's build, so the
  window's input waited those 3.5 to 42 ms behind it. `check_geometry` now only starts the
  build and hands back a `Rebuild`. The task waits for it in its `select!` beside its commands,
  and `finish_rebuild` puts it in. The stream goes on at its old size until then, and no
  geometry is probed while a build is out. A `SetQuality` that arrives meanwhile waits for the
  rebuild, so it applies on top of the new size. Test:
  `a_rebuild_starts_at_once_and_survives_a_dropped_wait`.

- ✅ **The capture follows the display's beat, and the cadence gate keeps the rung's schedule**
  (2026-09-26). This machine's panel runs at 75 Hz. With `minimumFrameInterval` at 1/60 its
  captures came 13.3 or 26.7 ms apart, and a gate that measured from the last encoded frame
  rejected every 13.3 ms one: a display stream at the default 60 fps sent 33–35 fps with 30 ms
  between frames (MEASUREMENTS, "capture on a 75 Hz display"). Three changes:
  - The worker reads the refresh of the target's display (`CaptureSource::refresh_hz`, the
    window's display for a window). The frame rate is the client's ceiling or that refresh,
    whichever is lower, since a display draws no faster, and the encoder, the cadence ladder and
    the frame guard all use it.
  - The capture asks for `kCMTimeZero` (`CaptureConfig::fps` 0), which SCStream.h documents as
    the display's native rate. A capture throttled to the rung's interval on a beat that does not
    divide it cannot be picked evenly from. When the refresh cannot be read, the capture keeps
    the ceiling's interval as before.
  - `slopty_media::Pace` replaces `frame_due`. Each encoded frame claims the rung's next slot,
    and a capture late in its slot carries the lateness to the next, so 60 fps on a 75 Hz beat
    is four captures in five. The credit is at most one period. A capture more than a period
    past its slot ends a pause and restarts the schedule, so no burst follows a still picture.
    The eighth of a period of early slack stays.
  Capturing every beat costs a ScreenCaptureKit callback per beat, even when a low rung encodes
  only some of them. That is the price of seeing a change within one beat. 🔬 Hardware "after"
  is owed; it needs the launchd worker rebuilt. Tests: `configs_follow_the_displays_refresh`,
  `a_rung_under_the_display_rate_keeps_its_rate_on_the_displays_beat`,
  `a_pause_restarts_the_schedule_from_the_capture_that_ends_it`,
  `the_cadence_gate_hands_the_encoder_one_capture_a_period`.

- ✅ **A frame's data leaves before its parity is computed** (2026-09-26). `Packetizer::packetize`
  hands the data datagrams to its `ship` callback, then runs Reed–Solomon over them and hands
  on the parity. The worker sends both from the callback under the packetizer's lock, so a
  NACK's answer never overtakes its frame. The first fragment of a 300 KB keyframe is handed on
  after about 30 µs rather than about 270 µs under load (MEASUREMENTS, "data before parity").
  Test: `an_encoded_packet_is_sent_and_a_nack_answers_from_history` (two calls: data, then
  parity).

- ✅ **The client's reassembler keeps its 2 ms tick** (2026-09-26). Sleeping until the
  reassembler's next deadline (NACK, retry, loss deadline, refresh repeat) instead of a fixed 2 ms
  while frames are pending was built and measured on a lost tail fragment, the case only the
  timer serves. Its median NACK lateness was no better in six alternated pairs and worse in five
  (MEASUREMENTS, "the reassembler's 2 ms tick stays"). The tick restarts after each arrival, so
  it is already in phase with the silence that matters, and tokio rounds a deadline up to its
  millisecond. Rejected. `tail_loss_nack_lateness` stays as the instrument.

- ✅ **The UDP receive buffer stays at the macOS default** (2026-09-26). No datagram of ours was
  dropped for a full socket buffer across keyframe-heavy and lossy loopback streams, and the
  machine-wide counter did not move (MEASUREMENTS, "UDP receive buffer"). `SO_RCVBUF` is set
  when a measurement shows drops, and not before.

- ✅ **A zoomed picture's region at native resolution** (designed 2026-09-28, built 2026-10-03).
  A zoomed picture already asked for the scale it is drawn at, which tops out at the whole
  target at native size. A 5K display at 1:1 on a phone then cost a 14.7 MP encode, send and
  decode for the 3 MP the phone shows. Now a zoomed picture streams only its region, at the
  scale it is drawn at. Measured before building, on a drawn 5K display at native: the whole
  display encodes in 35 ms and holds 28 frames a second (48 in two stripes). A phone's portrait
  region with its margin (1756 × 988) encodes in 5.8 ms and holds 60. Capture to painted drops
  from 55–65 ms to 23 ms, and the landscape region (3120 × 1756) sits between the two
  (MEASUREMENTS, "a zoomed picture's region"). Built as designed, except as follows:
  - **The region rides in `Quality`, not a `SetRegion` request.** `Quality::region:
    Option<Region>` (`Region { x, y, w, h: u16 }`, the target's native pixels, `None` for all
    of it). A zoom changes the scale and the region together, so one numbered `SetQuality`
    asks for both and builds sessions at most once. Two requests would have built twice, or
    needed an ordering between them. `Region::within` holds a region to its target: edges on
    even pixels, sides of at least `Region::MIN_SIDE` (64), moved in from an edge it would cross
    and cut at one it runs past. A region wholly off the target, or covering all of it, is the
    whole target.
  - **`FramePrefix` grows from 20 to 28 bytes** (the 2026-09-28 figure was from before the stripe
    fields): `region: [U16; 4]`, all zero for the whole target, under the frame's parity. The
    reassembler hands it on as `FrameInfo::region`, and the client's `Presentable::region`
    says where each picture goes. The client's stitch pairs two stripes only when both their
    build and their region match.
  - **A region moved at the size in force builds nothing.** The capture samples elsewhere
    (`sourceRect`, through `CaptureConfig::region` composed inside any crop), the encoder goes
    on predicting from what it coded, and each frame says where it goes. Only a region of
    another size builds new sessions and a keyframe. The client keeps a zoom's region the same
    size while it pans, so only a new zoom changes the size. Measured: a move shows its first
    picture in 36–41 ms (p50), against 120 ms for a new size and 148–165 ms back to the whole
    display.
  - **Each capture is labelled by its display time** (`screen::region::RegionClock`). A
    configuration update takes ScreenCaptureKit some milliseconds after it is asked, so a
    capture whose display time falls between the ask and the completion handler could show
    either region. Such a capture is not sent (`between_regions` counts them), because placed
    by the wrong region it would draw the old picture in the new place for a frame. A refused
    update leaves the old region on every capture. The capture's configuration is now weighed
    against the transition that has completed, not the one the last geometry tick settled.
    Otherwise a region asked for and then given back between two ticks was taken for the one
    in force, and never asked for.
  - **Where the client asks.** The region is the view plus a quarter of it on every side. At
    the picture's edges the region is moved inside the picture rather than cut, so its size
    holds (`zoom::streamed`). It is asked `QUALITY_COOLDOWN` (400 ms) after the zoom settles.
    A pan that leaves part of the view outside the region last asked for asks at once, so
    the margin is never a blank edge for longer than a round trip and a frame.
  - **The view.** The region's picture goes to its own native layer at its place in the zoomed
    whole (`region_bounds`). Under it, GPUI paints the newest picture of the whole target
    (`Glass::base`), which fills the margin while a new region is on its way. A picture of
    another region waits until the view has placed the layers for it (`glass::Placed` carries
    the region beside the seam), so it never shows at the old region's place. Input, the
    pointer, `Opened` and `Geometry` stay in the whole target's stream pixels, so `ScreenInput`,
    the cursor channel and `Zoom::to_picture` did not change.
  Rejected: a `SetRegion` request beside `SetQuality` (two builds per zoom, or an ordering
  between them); a rebuild on every move (a keyframe per pan, for no gain, since the encoder
  predicts across the shift); and a region cut at the picture's edge (a new size, and so a
  rebuild, every time a pan reached an edge). Owed on hardware: that a real ScreenCaptureKit
  stream delivers promptly after a same-size `sourceRect` move on a still screen (the drawn
  capture does). Also owed: the one refresh when a zoom back out to fit may show the last
  region's picture before the whole one comes. Tests: `a_region_is_held_to_its_target`, the
  goldens `screen_quality_region` and `frame_prefix_region` (proto);
  `the_datagrams_share_one_buffer_and_rebuild_the_frame` (media);
  `the_region_is_sampled_inside_the_crop` (capture); `screen::region::tests`,
  `a_region_is_captured_at_the_scale_and_held_to_the_target` and
  `a_region_streams_its_part_at_native_resolution` (worker: region frames at the region's size
  with it in the prefix, a move without a rebuild, a region past the edge, one off the target,
  `None` back to the whole, a window shrunk under its region, a stream closed while a region's
  sessions build); `stripes_of_two_regions_never_go_up_together` (client);
  `a_region_picture_waits_for_its_place_and_the_whole_one_stays_the_base`,
  `screen::zoom::tests`, `a_zoom_asks_for_its_region_once_it_settles`,
  `a_region_picture_goes_to_its_place_over_the_whole` (ui). Measurements:
  `a_region_against_the_whole_5k_display`, `measure_a_region_change`.

- ✅ **A cursor sample draws only what it moves, and rides the frames** (2026-09-28; the riding
  superseded 2026-09-30 by **Pictures go to a layer of their own, not through a GPUI frame**:
  frames no longer draw the view, so a sample that moves the pointer draws at once). The view
  drew on every cursor sample, up to 120 a second. It did so in trackpad mode, where the
  trackpad's pointer is drawn and not the sample, and when the worker's pointer was off the
  target and nothing was drawn. Frames drew separately. A cursor-only draw just before a frame
  made that frame the second inside one refresh, and a `CAMetalLayer` shows it a refresh late
  (31.9 against 18.9 ms to glass). Now the pump hands each sample to
  `ScreenView::pointer_changed`, which compares the pointer the view would draw (a point of
  the picture, or none) with the one the last render drew, and does nothing when they match.
  While frames flow (the last came within two periods of the rate asked for), a change waits
  for the next frame, which draws it anyway. It draws on its own only when that frame is more
  than `FOLD` (half a 60 Hz refresh) past its due time. With frames stopped it draws at once.
  Every render, whatever asked for it, records the pointer it drew and ends any wait. At 60
  frames and 120 samples a second, the view draws 61 times instead of 180 (MEASUREMENTS, "a
  remote pointer drawn where the client put it"). The price is a pointer the worker moves on
  its own drawn up to one frame period plus `FOLD` late, about 25 ms at 60 fps. The pointer
  this client moves is never drawn from samples: it is drawn at once, in the move's own frame
  (`docs/decisions/input.md`, "A window stream's pointer is where its input put it",
  amended). So the wait adds nothing a hand here can feel. Test:
  `cursor_samples_ride_the_frames_while_they_flow`.

- ✅ **The client reads datagrams in batches and its stream worker keeps one timer** (2026-09-28).
  The link's reader takes a read's worth with `read_many_datagrams` (one lock of the
  connection) and routes the screen datagrams under one lock of the router
  (`ScreenRouter::route_many`). Each stream worker moves the deadline of one pinned `Sleep`,
  which is an atomic update when it moves later. Before, it made a sleep each wake and dropped
  it unfired, taking the timer wheel's lock twice (52–56 against 335–403 ns a wake). The worker
  reads the path's round trip once a report rather than on every wake, because that read takes
  the connection's lock and waits on its driver (p50 0.7 µs, tens of µs at worst). The
  reassembler's timers go by a figure up to 50 ms old. The tick still restarts after each
  arrival, as the 2026-09-26 ruling needs. End to end the saving is below what this machine's
  load lets the frame benchmarks resolve (MEASUREMENTS, "the client's datagram path"). Tests:
  `a_batch_fans_out_in_order_and_skips_what_does_not_parse`,
  `the_round_trip_is_read_once_a_report`, `the_reader_sorts_a_burst_between_sessions_and_screens`.

- ❌ **The cursor's picture is not deduped on the cursor's identity** (2026-09-28). The shape loop
  reads the picture 30 times a second while the pointer is over the target. An unchanged cursor
  was meant to cost one pointer compare. AppKit returns a fresh `NSCursor`, `NSImage`,
  representations and `CGImage` on every `currentSystemCursor`, so there is nothing to compare.
  The read's cost is that call, about 155 of 190 µs at the median. A pixel digest would still
  make the call and save at most the copy and conversion. Rejected (MEASUREMENTS, "the cursor's
  picture"). What would cut the cost is reading less often, for instance only after the move
  counters change plus a slow fallback. That would delay a shape change under a still pointer
  (a scroll, a click, an app going busy), so it waits for a measurement of those cases.

- ✅ **A quality change builds its session beside the stream's input, and the stream's news
  waits for room on its own** (2026-09-28, MEASUREMENTS "input behind a quality change").
  `Pipeline::set_quality` no longer awaits anything. A bitrate alone still applies in place. A
  size, rate or codec change maps input and the pointer at the new scale at once and returns a
  `Rebuild`, which the stream's task waits for in the same select arm as a resize's. The client
  maps its pointer at the scale it asked for from the moment it asks, so this is also the scale
  the input behind the change was sent at. A change that comes while a build is under way
  replaces that build and keeps its size, so a quality asked for after a resize still applies on
  top of it. A replaced session is dropped on the blocking pool. Only a resize's build tells the
  client `Geometry`; a quality change tells nothing, as before, because a late `Geometry` for an
  older ask would move the size the client maps with. A move sent right behind a scale change
  reached the input thread after 2.9–3.8 ms at the median, the session build, and now after
  11–12 µs.

  The `Geometry` and `Source` events the task sends used to wait for room in the connection's
  1024-slot queue in front of the next command. They now wait in a queue of their own, drained
  by a select arm, where a newer event of a kind replaces the one waiting, since each says
  where the stream is now. Dropping them was rejected: a lost `Geometry` leaves the client
  mapping input at the wrong size. Tests: `input_behind_a_quality_change_does_not_wait_for_the_encoder`,
  `a_quality_change_replaces_the_build_of_the_one_before`,
  `news_the_client_has_no_room_for_does_not_hold_input`,
  `a_read_landing_during_a_quality_build_does_not_drop_it`.

- ✅ **Capture and input to the glass are timed from drawn pictures, not from a switch in the
  product** (2026-09-28, MEASUREMENTS "capture to the glass and input to the glass, from drawn
  pictures"). A worker a test starts cannot capture, and nothing may ask for Screen Recording,
  so the capture seam gets a second implementation: `slopty_capture::synthetic::Canvas`. It
  stands in for each display and draws on the display's beat into `IOSurface`-backed NV12, the
  same pictures ScreenCaptureKit hands over, stamped on the same host clock. It never draws over
  a picture the stream or the encoder still holds. The worker platform `screen::synthetic::Drawn`
  pairs it with the real encoders, and its input sink changes the next picture. The strip of
  blocks that spells the input count is read back off the decoded picture, so input → glass is
  timed from pixels that really went through the codec. A platform type rather than an
  environment switch keeps it out of the product: `ScreenStream` is `Pipeline<Native>`, and only
  a test names `Pipeline<Drawn>`. The pacer keeps capture → shown beside arrival → shown when it
  is given a `ClockAnchor`, which is exact only where both ends read mach time (loopback), and
  input sent → shown for inputs the caller marks. `PacingStats` is unchanged; the new numbers
  come from `Pacer::glass`. What was found: the encoder is 60–70 % of capture → painted on
  loopback and sits at its floor. On a tailnet-shaped link, the rate controller's loss rule
  halves the cadence under 3 % random loss that parity fully repairs, and that doubles input →
  glass. That is a policy question left open.

- ✅ **Repaired loss holds the rate; congestion, unrepaired frames and heavy loss cut it**
  (2026-09-28, MEASUREMENTS "repaired loss holds the rate"). `judge` used to cut on datagram
  loss above 2 %. Parity is sized from that same loss and repairs it without a round trip, so on
  a path that drops at random the cut bought nothing. Over 5 ms each way with 3 % i.i.d. loss it
  took the stream to the 1 Mbit/s floor and the cadence to 25 fps, with no frame lost. Input at
  the worker → the capture showing it was 21–24 ms p50 against 8 on loopback. The overuse lines
  are now the present queue (≥ 3) and the hold p95 (> 60 ms) as before, more than 2 % of the
  window's frames given up after parity and NACK (`frames_lost` over `frames_ok + frames_lost`,
  fields the report already carried), and datagram loss above 10 %. Clean now also asks for no
  lost frame. Loss between the clean line (0.5 %) and 10 % neither cuts nor grows. After the
  change the shaped runs never cut, held 60 fps at 15–22 Mbit/s, and input at the worker → the
  capture showing it came down to 8.2–8.4 ms p50, the loopback figure. Loopback did not move.

  The sources agree on the shape. GCC's loss-based controller holds between 2 and 10 % and
  decreases only above 10 %, because "if the packet loss ratio does not increase, the losses are
  probably not related to self-inflicted congestion"
  (<https://www.ietf.org/archive/id/draft-ietf-rmcat-gcc-02.txt>, §6). libwebrtc's
  `LossBasedBweV2` fits observed loss to an inherent loss plus the loss explained by sending
  above a limit. It lowers the estimate only for the second part and freezes increases while
  the average loss is above the inherent one
  (<https://webrtc.googlesource.com/src/+/656517acd38c2621edc4905caf0efe389843bb8c/modules/congestion_controller/goog_cc/loss_based_bwe_v2.cc>).
  BBRv3 tolerates loss up to `BBR.LossThresh` (2 %) per round while it probes, and names random
  loss as a case where it does better than loss-based controllers
  (<https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.txt>). The RMCAT evaluation RFCs
  require independent random loss in the test cases, as loss that is not congestion
  (<https://www.rfc-editor.org/rfc/rfc8867.txt>, <https://www.rfc-editor.org/rfc/rfc8868.txt>).
  SCReAM backs off on every loss event (<https://www.rfc-editor.org/rfc/rfc8298.txt>, §4.1.2.1),
  but it has no parity to pay for the loss it ignores.

  The 2 % line moved from datagrams to frames because a frame given up is what congestion does
  that repaired random loss does not: a bottleneck that overflows drops in bursts, and a burst
  outruns parity sized at twice the mean. It is also what the viewer sees, as a freeze and a
  refresh. 10 % is GCC's decrease line. Past it parity is 250 ‰ of the rate and more
  (`Redundancy::HEADROOM`), and a smaller picture buys more than more parity does. Delay still
  cuts on its own at any loss. The QUIC path is a further guard: BBRv3 reacts to loss with its
  own bounds, and the 90 % cwnd cap takes the target down with it. Growth stays at loss ≤ 0.5 %
  and was not moved to GCC's 2 %: parity takes a share of every rate and growth under loss was
  not measured. `Redundancy` is unchanged, because it repaired every frame in the shaped runs.
  A delay trend (GCC's over-use detector) was not added. The report carries `owd_jitter` but no
  delay gradient, and adding one is a wire change that no measurement asked for. Tests:
  `repaired_random_loss_holds_the_rate`, `repaired_loss_with_a_growing_hold_or_queue_cuts`,
  `unrepaired_or_heavy_loss_cuts`, `the_overuse_lines_are_exclusive`.

- 🔬 **A display sized to the client comes from `CGVirtualDisplay`, found at runtime**
  (2026-09-28, `.research/virtual-display-2026-09-28.md`). A headless Mac, or a client whose
  screen is not the worker's (an iPad, a 5K display), should get a desktop at its own size and
  scale rather than a crop of a physical display. macOS 26 has no public API for that; the
  private `CGVirtualDisplay`, `CGVirtualDisplayDescriptor`, `CGVirtualDisplayMode` and
  `CGVirtualDisplaySettings` classes are what DeskPad, Chromium's test and remoting code,
  BetterDisplay and opendisplay use, with the same selectors in the 26.4 header dump and on
  this Mac's 27.0 runtime. `slopty-vdisplay` looks the classes up by name and checks every
  selector before it sends one, so a macOS that drops them yields `Unavailable` and the worker
  streams a physical display. The sizing policy (`plan`) is pure. A client scale of 2 or more
  gets a hiDPI mode given in points, since macOS only backs at 2×. Sides are clamped to a
  480-point floor and, keeping the aspect, to 8K. A 1× mode's sides are rounded down to even for
  the 4:2:0 encoder. Refresh is clamped to 30–120 Hz and defaults to 60. The panel is sized at
  109 or 218 PPI, Apple's desktop densities, so macOS takes the one mode offered as native. The
  descriptor's maximum is fixed at creation, so it is the longest side plus a quarter, on both
  axes: the tile can grow and the client rotate through `applySettings:` without recreating
  the display, which would redistribute every window. Vendor is "SLP" in EDID letters, and
  product and serial come from an FNV-1a hash of a stable client identity. macOS files
  arrangement, mode and mirroring under that triple, so a returning client finds its own. The
  hash is pinned by a test. The mode is enforced, not set once: `enforce` picks the exact mode
  from `CGDisplayCopyAllDisplayModes` with the duplicate low-resolution modes listed, where a 2×
  mode and its 1× twin share a point size. It sets that mode and breaks any mirror in one
  `kCGConfigureForSession` transaction, and it is called again on every reconfiguration,
  because macOS assigns its own default and may restore a saved mode later. Creation needs the
  main thread, so the one test that makes a real display runs a probe binary as its own child.
  It is ignored and gated on `SLOPTY_VDISPLAY_E2E=1`, because a new display rearranges the
  screens of whoever is using the Mac. It has not been run: the create, rotate and teardown
  path, refresh above 60 Hz and the lock screen are unproven. Tests:
  `every_plan_is_even_consistent_and_within_its_maximum` and its siblings in `plan/tests.rs`,
  `a_missing_class_or_selector_is_unavailable`, `the_descriptor_carries_the_plan`,
  `the_settings_offer_the_mode_in_points_at_2x`,
  `creating_off_the_main_thread_is_refused_before_any_display_exists`, and the ignored
  `a_virtual_display_takes_its_mode_rotates_and_goes_away`.

- ✅ **4:4:4 exists on the low-latency hardware encoder: HEVC Main 4:4:4 10 fed `xf44`, as a
  per-stream chroma option** (2026-09-28, M1 Max, macOS 27.0; MEASUREMENTS.md, "4:4:4 HEVC on
  the low-latency encoder"). This supersedes "No 4:4:4 hardware path exists" in the first entry.
  The low-latency encoder (`…videoencoder.hevc.rtvc`, the one `EnableLowLatencyRateControl`
  selects) advertises `HEVC_Main444_AutoLevel` and `HEVC_Main44410_AutoLevel` in its own
  `ProfileLevel` supported values although no SDK header exports them. With Main44410 it writes
  an RExt SPS (`chroma_format_idc` 3, 10-bit) and keeps `EnableLTR`, so LTR recovery holds. The
  plain hardware encoder can do 4:4:4 but refuses LTR, as before. The hardware decoder takes the
  stream straight to `xf44`. Encode time is unchanged: 7.73 against 7.72 ms p50 at 1080p and
  35.9 against 35.3 at 5K. On coloured text it buys 24 dB of chroma PSNR (52.8 against 28.9 dB,
  the 4:2:0 ceiling at every rate) and 2 dB of luma. It costs about 1.6× the bits at
  saturation, 11.1 against 6.9 Mbit/s at 1080p. Below the 4:2:0 saturation rate it is worse,
  47.5 against 52.9 dB luma at 7 Mbit/s. So it is an option a stream asks for when its link
  carries the rate, not a new default. Rulings:
  1. *Main44410 fed `xf44`, not Main444 fed `444f`.* ScreenCaptureKit's `pixelFormat` lists
     `xf44` (full-range 10-bit 4:4:4, `SCStream.h`, macOS 27.0 SDK) and no 8-bit 4:4:4, and
     Main444 writes 4:2:0 for any input but `444f`. Against `444f`, `xf44` trails by 1–3 dB of
     luma below saturation and leads by 2 dB at it. `BGRA` into Main44410 also stays 4:4:4, but
     the conversion inside the session costs 0.3 ms at 1080p and 6 ms at 5K, so the capture
     delivers `xf44`.
  2. *The profile string is the session's own.* `slopty_codec::Encoder::with_chroma(config,
     Chroma::Full, sink)` looks `HEVC_Main44410_AutoLevel` up in the session's
     `VTSessionCopySupportedPropertyDictionary` and sets the `CFString` the framework handed
     back. It fails with `NoFullChroma` when the encoder does not list the value, and H.264 fails
     the same way.
  3. *A 4:4:4 session refuses a picture that is not `xf44`* (`NotFullChroma`). The hardware
     would otherwise write 4:2:0 under a 4:4:4 profile without a word, as the probe shows for
     Main444.
  4. *The decoder follows the SPS.* `Decoder` parses `chroma_format_idc` and the bit depth
     (`annexb::hevc::sample_format`) and asks for `xf44` (or `444f` for an 8-bit 4:4:4 stream),
     rebuilding its session when that changes, so a 4:4:4 stream is never subsampled on the way
     out. 4:2:0 streams decode to `420f` as before.
  Also found: the low-latency encoder takes 35 ms per 5K frame on this chip (about 28 fps), where
  the plain hardware encoder takes 18. A 5K stream at 60 fps is out of reach on an M1 Max
  whatever the chroma.
  Not yet wired, in dependency order:
  (a) *GPUI fork* (`crates/gpui_apple/src/metal_renderer.rs`, `draw_surfaces`). It asserts
      `420f` and makes `R8Unorm` + `RG8Unorm` textures, so an `xf44` picture would panic the
      client. It needs `xf44` accepted, with `R16Unorm` + `RG16Unorm` from planes 0 and 1. The
      shader is unchanged, since it samples normalised coordinates and the chroma plane is
      simply full size; the scale is 65535/65472 off, 0.1 %.
  (b) *Wire* (`slopty-proto`, `screen::Quality`). A `chroma: Chroma` field, 4:2:0 by default,
      with its golden, and `slopty_codec::EncoderConfig` gains the same field so that
      `VideoToolbox::new` passes it on and `Encoder::with_chroma` folds into `new`. A client
      asks for 4:4:4 only when its decoder takes it: the macOS client yes, iOS/iPadOS unproven,
      because the probe's decode half has not run on a device.
  (c) *Capture* (`slopty-capture`, `PixelFormat`). A `Yuv444Full10` variant mapped to
      `kCVPixelFormatType_444YpCbCr10BiPlanarFullRange` with the BT.709 matrix already set. The
      worker (`screen.rs`, the `EncoderConfig` built from `Quality`) picks it when
      `quality.chroma` is full.
  (d) *Rate.* A 4:4:4 stream should start its adaptive ceiling about 1.6× higher, or stay 4:2:0
      below roughly 10 Mbit/s at 1080p. That needs a measurement on a real link first.
  Tests: `a_full_chroma_stream_is_444_end_to_end`,
  `a_full_chroma_session_refuses_a_subsampled_picture`, `full_chroma_is_hevc_only`,
  `the_sps_says_which_chroma_format_the_stream_is`, and the ignored probes in
  `tests/chroma444.rs`.

- 🔬 **A display sized to the client is wired: the main thread owns the displays, a stream
  falls back to a physical display, typed** (2026-09-28). `CGVirtualDisplay` only initialises
  on the main thread, and the descriptor hands it the main queue. The worker's main thread
  therefore serves the run loop (`slopty_vdisplay::park_main`), and the tokio daemon runs on a
  thread beside it that ends the process when it ends. Before this the main thread sat in
  `block_on`, so nothing queued to it ever ran. The displays live there in a registry keyed
  by the client's `DisplayKey`. Stream tasks hand create, resize, settle and release over as
  jobs (`screen::sized::Main`) and await the answers, so no display object leaves the main
  thread and nothing there blocks a task. One key has one display: a second tile of the same
  device shares it and the last lease releases it. A new or changed display is enforced
  every 100 ms until `Settled`, for at most 5 s, and on every `CGDisplayRegisterReconfigurationCallback`
  notice after that. Once settled, ScreenCaptureKit must list it within 2 s, asked with a fresh
  enumeration every 100 ms because the shared one may predate the display. The client is told
  what it got before `Opened` (`ScreenEvent::Display`). That is the display made, or the
  physical one streamed instead with a typed reason: `Unavailable` (no classes, not macOS),
  `Refused` (nil, rejected settings, a failed configuration), `Unsettled` or `Unlisted`.
  `Resize` gained an optional scale. A resize that fits stays in place and the geometry poll
  follows it. An outgrown display is released first, since the new one reuses its identity,
  then made anew. A change of backing scale moves the stream to the display
  (`Pipeline::switch_display`: a filter update, input and the cursor remapped). A display lost
  on the way hands the stream a physical one. Teardown lets input go, stops the capture and
  then drops the lease, which releases the display on the main queue. `WorkerCaps` carries
  `virtual_displays` (Screen Recording granted and the classes present), so a client hides the
  option elsewhere; Linux says no. The lifecycle is tested over a fake factory and a task
  standing in for the main queue: `a_display_is_enforced_until_it_settles_shared_by_key_and_released_with_its_last_lease`,
  `a_worker_that_cannot_make_one_says_unavailable_and_a_refusal_says_refused`,
  `a_display_that_never_settles_is_given_up_on_and_released_with_its_lease`,
  `a_resize_that_fits_stays_in_place_and_one_that_outgrows_remakes_the_display` and
  `a_reconfiguration_enforces_every_display_again`. The stream is tested over the test
  platform: `a_made_display_is_streamed_and_released_with_the_stream`,
  `without_displays_a_physical_display_is_streamed_and_the_client_told_why`,
  `a_display_never_listed_falls_back_and_is_released` and
  `an_outgrown_display_is_remade_and_the_stream_follows_it`. Still unproven live: a real display
  made by the worker (the opt-in probe has not run), capture and input through its bounds, and
  the latency of a switch.

- ✅ **Full chroma follows the rate: 4:4:4 is a client's ask, granted above a line that scales
  with the picture and taken back below a lower one, with a hold** (2026-09-28; MEASUREMENTS.md,
  "4:4:4 HEVC on the low-latency encoder" and "full chroma on the wire"). This wires (b) and (c)
  of the 4:4:4 entry above and answers (d) from the numbers already measured. Rulings:
  1. *The wire says what the client wants, not what it gets.* `screen::Chroma` (`Subsampled`,
     `Full`) moved to `slopty-proto`, and `Quality` gained `chroma`, 4:2:0 by default (golden
     `client_screen_set_quality_full_chroma`; `client_screen_open_display` grew its byte).
     `slopty_codec::EncoderConfig` carries the same field and `Encoder::with_chroma` folded into
     `Encoder::new`. The client is not told which chroma it gets: its decoder already follows
     the SPS, and the GPUI fork draws `xf44` as it comes, so the picture says so.
  2. *4:4:4 only where it is the sharper picture.* On scrolling text at 1080p60, 4:2:0
     saturates at 6.9 Mbit/s. At that rate 4:4:4 has 5 dB less luma, and it passes 4:2:0's luma
     only near 10 Mbit/s, where its chroma is 24 dB better. So `slopty_media::rate::ChromaGate`
     grants 4:4:4 when the client asked, the codec is HEVC and the rate target is at least
     10 Mbit/s. It takes 4:4:4 back under 8 Mbit/s, where the luma loss outweighs the chroma.
     Inside the band a stream keeps what it has. One cut (to 75 %) from the enter line lands
     under the leave line, and the band is wider than a clean window's growth of an eighth.
  3. *The lines scale with the picture as the pixels to the ⅔.* Between 1080p and 5K the
     saturation rate grew as the pixels to the 0.62 (4:4:4) and 0.66 (4:2:0), so ⅔ errs towards
     4:2:0. That puts the enter line near 25 Mbit/s at 4K and 37 at 5K: a 5K stream under the
     default 30 Mbit/s ceiling never takes 4:4:4, which the 5K numbers (36 against
     25 Mbit/s spent) support. Frame rate is not in the line; the cadence ladder's rungs sit far
     below it.
  4. *A switch is a new session, so it is rationed.* A stream that fell back waits ten decisions
     (about five seconds) before it may take 4:4:4 again. A link whose capacity sits just over
     the line then switches about every 33 s, not every decision (test
     `a_swinging_rate_does_not_flap_the_chroma`). The client asking again, or the stream
     changing size, decides at once and drops the hold, since a new session is being built
     either way. A stream that asks for 4:4:4 opens with it when the 12 Mbit/s start clears its
     line (1080p and smaller).
  5. *The capture leads on the way up and trails on the way down.* A 4:4:4 session refuses
     anything but `xf44`, and a 4:2:0 session takes `xf44` as well. So a switch to 4:4:4 asks
     ScreenCaptureKit for `xf44` while the session builds, and a switch back keeps `xf44` until
     the 4:2:0 session is in. A `420f` picture that still reaches a 4:4:4 session in the race is
     logged at debug and retried a frame later. The worker switches on its 100 ms geometry tick,
     which reads the gate the rate decision moved. A 4:4:4 session that cannot be built
     (`NoFullChroma`) makes the stream 4:2:0 until the client asks again, at open or on a
     rebuild.
  6. *Every stream from a Mac asks.* `[remote] sharp_text`, an opt-in, was cut on 2026-10-05:
     the ask costs the encoder no time, and the gate above already weighs the 1.6× bits against
     the rate (`docs/decisions/settings.md`, "`[remote]` is the bitrate ceiling alone"). An
     iPhone or iPad still asks for 4:2:0 until its decoder is proven to take 4:4:4.
  Tests: `the_full_chroma_band_is_measured_at_1080p_and_scales_with_the_picture`,
  `full_chroma_is_asked_for_and_earned`, `full_chroma_has_hysteresis_and_a_hold`,
  `a_swinging_rate_does_not_flap_the_chroma` and
  `an_ask_decides_at_once_and_a_refusal_holds` (policy);
  `full_chroma_captures_xf44_for_a_444_session` and `the_capture_is_full_range_bt709`
  (configuration); `the_strip_reads_back_what_was_drawn` (the canvas draws `xf44`); and
  `a_full_chroma_stream_arrives_as_444_and_follows_the_rate`. That last one runs drawn pictures
  through the real encoder, packetizer, reassembler and decoder: 4:4:4 at open, 4:2:0 after
  loss, and 4:4:4 again after clean windows and the hold. Not yet proven: the line on a
  real link, and iOS/iPadOS decoding 4:4:4.

- ✅ **The stream follows the screen's refresh; a link or an encoder short of 120 drops the
  rate to 60 before the picture thins** (2026-09-29, MEASUREMENTS "120 fps against 60").
  1. *The client asks for its screen's rate.* `[remote] fps` is now a ceiling, 120 by default,
     which follows any screen. A stream asks for `min(ceiling, refresh)`
     (`slopty_ui::screen::stream_fps`), with the refresh read from the screen the view's window
     is on: `NSScreen.maximumFramesPerSecond`, matched by its `NSScreenNumber` to the display
     GPUI puts the window on (`slopty_platform::display_refresh_of`), so a ProMotion panel
     reads 120 whatever it idles at. On iOS it is the scene's screen. A screen that does not
     say counts as 60. A stream opens at the main screen's rate. Once its view is drawn in a
     window, it asks again whenever that window lands on a screen of another rate, through
     the window's bounds observer. It sends the ordinary `SetQuality` with the new `fps`, so
     there is no wire change. The worker already capped the rate at the target display's
     refresh and captured at the display's own beat.
  2. *A new rate alone is taken in place.* A `SetQuality` that moves only `fps` used to
     rebuild the encoder, which meant a keyframe on every move between screens. The capture
     already runs at the display's beat, so the worker now sets the ceiling, resets the
     cadence, and sends `ExpectedFrameRate` to the session it has. There is no rebuild and no
     keyframe (`Pipeline::set_rate_in_place`).
  3. *The link: the ladder's thresholds stand, and its first step from 120 is 60.* Measured
     on the shipped encoder with a 1080p text scroll at the same speed at both rates, 120 fps
     spent 1.2× the bits of 60 at the same PSNR: 8.1 against 6.9 Mbit/s, 53.4 dB against
     53.1–53.7. At 1440p the ratio was 1.3× and at 4K 1.2×. At an 8 Mbit/s target, 120 gives
     8.0 KiB a frame and 53.3 dB, as good as 60's 14 KiB frames. At 4 Mbit/s, 120 drops frames
     and loses 2.8 dB against 60. The ladder's 8 KB bar already sits at that line. 120 stands
     while the target is at least 7.9 Mbit/s, and below that the stream goes to 60, where each
     frame gets twice the bytes, before anything reaches 30. Climbing back to 120 takes 12 KB
     a frame, 11.8 Mbit/s. On the tailnet-shaped synthetic link (5 ms each way, 3 % loss),
     120 cut input sent → shown from 38.6 to 28.3 ms p50 and input at the worker → the capture
     showing it from 9.6 to 3.6 ms, for 2.0 against 1.4 Mbit/s on the wire. Test:
     `a_link_short_of_120_drops_to_60_first`.
  4. *The encoder: a rung it cannot keep costs a queue, so it is given up.* The hardware
     encoder's own latency is about 8 ms at 1080p at either rate, a 120 fps period. When
     frames come faster than it turns them out, they queue inside VideoToolbox. At
     3024 × 1964, a MacBook Pro's native size and a height that is not a multiple of 16, every
     frame came back 77–87 ms late at 120 fps, and in one run 70–86 ms at 60. At 3840 × 2160
     it held 22 ms at 120. A fixed pixel-rate budget cannot describe that, and it would differ
     on every chip, so the worker watches instead (`slopty_media::EncoderWatch`). A frame
     back more than three periods of the rung in force after it went in is late. Twelve late
     in a row (a tenth of a second at 120) take the stream's ceiling down one rung, 120 → 60 →
     30 → 15, and the cadence with it. One slow frame, such as the first keyframe at 40–97 ms,
     is not a run. The ceiling stays down until the next encoder session (a resize or a new
     quality). Probing back up would pay the queue again to learn what was already seen.
     Tests: `an_encoder_that_falls_behind_takes_the_ceiling_down_a_rung`,
     `an_encoder_behind_its_rung_takes_the_ceiling_down`.
  5. *What was not measured.* A physical display above 60 Hz, since this Mac's panel runs at
     75 Hz. A `CGVirtualDisplay` at 120 Hz, since making one is gated on
     `SLOPTY_VDISPLAY_E2E` and was not run. The virtual display is already planned at the
     client's refresh (30–120 Hz). Whether macOS paces its frames at 120 is still open. The
     synthetic canvas drew at a 120 Hz beat (`synthetic::set_beat`). Its scroll is now set in
     time (180 rows a second) rather than per beat, so a faster beat draws the same motion in
     smaller steps.
  Tests: `the_rate_follows_the_screen_up_to_the_ceiling`,
  `a_view_on_a_screen_of_another_rate_asks_for_it`, `a_new_frame_rate_alone_is_taken_in_place`,
  `remote_settings_ride_on_the_theme` (default 120), `the_page_moves_every_frame` (the scroll in
  time). Follow-up the numbers point at: 3024 × 1964 was the slow size at both rates, while
  3840 × 2160 was not. Rounding a stream's sides to a multiple of 16 rather than 2 may be worth
  more than any rate policy at a MacBook's native size. That needs its own measurement.

- ✅ **Small frames carry two parity fragments on a lossy link** (2026-09-29, no wire change;
  `slopty_media::layout`, `MIN_PARITY_FRAGMENTS`). A frame got `ceil(data × ratio)` parity
  fragments, at least one. Loss comes in clumps, and a clump of two in a frame of two to seven
  fragments beat that one fragment, so the frame waited a NACK round trip or was refreshed.
  Moonlight asks Sunshine for a floor of two for this reason (`minRequiredFecPackets`, "Require
  at least 2 FEC packets for small frames", `SdpGenerator.c`). Now the floor is two once the
  ratio is above `Redundancy::MIN` (50 ‰), which is to say once the receiver's reports have
  shown loss. At 50 ‰ it stays one.
  - Measured on a simulated link that loses datagrams in runs (a Gilbert channel), with the
    packetizer, `Redundancy` and the reassembler in the loop and NACKs, refreshes and reports
    travelling back over 10 ms (`tests/burst_loss.rs`, MEASUREMENTS "two parity fragments on
    small frames"). On a still window, frames that waited a round trip fell from 111 to 78 at 1 %
    loss in runs of two, 32 to 3 at 3 % single losses and 325 to 166 at 3 % in runs of two, and
    refreshes from 8 to 3 at 3 % in runs of four. The picture stood still 4.3 s where it stood
    7.6 s. That costs the still window 0.2 to 0.5 Mbit/s of parity on a 2 Mbit/s stream. On a
    scrolling 1080p screen the frames are 7 to 17 fragments, the ratio already buys two or
    more at 3 % loss, and the gain is smaller: 19 to 6 round trips at 1 % (for 0.25 Mbit/s more), 72 to 50 at 3 %.
  - A floor of two on every link was measured first and rejected: on a clean link it adds a
    fragment to every busy frame for no repair, 7.40 to 7.95 Mbit/s (+7 %) on the scrolling
    screen at 0 % loss. The 2026-09-06 ruling ("Dropping parity on small frames") still holds;
    this is its other side, more parity where the frames are small and the link has lost some.
  - Tests: `the_parity_floor_is_two_on_a_lossy_link_and_one_on_a_clean_one` (packetize),
    `a_small_frame_survives_two_losses_in_a_row_without_a_nack` (pipeline), and the ignored
    measurement `parity_under_burst_loss`.

- ❌ **The `VideoConferencing` compression preset is not adopted** (2026-09-29, M1 Max, macOS
  27.0; MEASUREMENTS "compression presets and the low-latency encoder's keys"). macOS 26 added
  `kVTCompressionPropertyKey_SupportedPresetDictionaries` and five presets; the header says
  `VideoConferencing` needs `EnableLowLatencyRateControl`. The low-latency encoder offers only
  that one, and all it sets is `{AverageBitRate = 11 118 750, EnableLTR = false, RealTime =
  true}`. Applied on top of the worker's session it moved nothing in the encoder's time (1080p
  7.4 against 7.6 ms p50 on a real-time beat, 4K 23.5 against 23.4, 5K 38.0 against 38.1). It
  turns off LTR, which is Slopty's loss recovery, and its fixed 11.1 Mbit/s rate dropped 16 of
  180 frames at 4K and 54 at 5K and cost 5 and 11 dB of PSNR. The hardware encoder without
  low latency offers `Balanced`, `HighQuality`, `HighSpeed` and `ConsistentQuality`, all with
  frame reordering or look-ahead, which are out for the same reasons as before.
  - The low-latency encoder's supported-property list still names keys no SDK header exports:
    `RequestedMaxEncoderLatency`, `NumberOfSubFrameSections`, `NumberOfSlices`,
    `ReferenceBufferCount`, `MinNumberOfTemporalLayers`, `NumberOfTemporalLayers`,
    `EnableFrameDropping`, `PeriodicRefreshMode`, `MaxRefreshFrameIntervalDuration`,
    `ThroughputMode` and `SupportedThroughputModes`. The 2026-09-05 ruling that left
    `RequestedMaxEncoderLatency` alone stands: private keys, set by name, are not used.
    `NumberOfSubFrameSections` is the one that could matter (output before the whole frame is
    encoded); it would need a public key first.
  - Test: `compression_presets` in `crates/slopty-codec/tests/chroma444.rs`, ignored.

- ✅ **Stream sides padded to 16, cropped by the SPS conformance window** (2026-09-29, M1 Max,
  macOS 27.0; MEASUREMENTS "encode time against frame size" and "Stream sides padded to 16";
  settles the follow-up in "The stream follows the screen's refresh"). The low-latency encoder
  handles a picture whose sides are both multiples of 16 inside the submit call, one at a time,
  and keeps nothing after it. Given any other size it returns at once, copies the picture into a
  padded buffer of its own and queues it, and every size whose frame takes longer than the
  period queues to a steady 68–111 ms: 3024 × 1964 (a 14-inch MacBook Pro's panel) 76–88 ms,
  3456 × 2234 96–111 ms, 2880 × 1800 68–77 ms at 120. Sixteen, not eight: 1960 and 1800 queue
  too. The copy also costs every frame: 3024 × 1964 takes 17.6 ms one at a time, the same
  picture coded as 3024 × 1968 15.2 ms.
  - HEVC streams are coded padded. `configs` keeps the picture at the quality's scale with even
    sides, as before, and sets `CaptureConfig::align` to 16 (`HEVC_ALIGN`). The capture surface
    and the encoder session are `CaptureConfig::surface()`, each side rounded up to 16, and
    ScreenCaptureKit draws the picture at its top-left at its own size
    (`SCStreamConfiguration.destinationRect`, "specified in pixels", `SCStream.h`) over a black
    `backgroundColor` (the header's default is clear, which a Y′CbCr surface has no value for).
    The drawn canvas pads its pictures the same way, so the glass tests take this path.
  - The worker rewrites each keyframe's SPS (`slopty_codec::conformance::crop_access_unit`, on
    the encoder's output thread in `start_encoder`): `conformance_window_flag` and a right and
    bottom offset in chroma units (2 for 4:2:0, 1 for 4:4:4), every other bit copied, trailing
    bits and emulation prevention redone. The result is bit for bit the SPS the encoder writes
    when given the true size itself, which already carries such a window, for Main at 3024 ×
    1964 and Main 4:4:4 10 at 3456 × 2234 (probe P1). VideoToolbox's decoder crops by it, so the
    client's buffers are the true size with PSNR and edges equal to the unpadded stream's, and
    nothing past the worker changes: no wire field, no client crop, pointer mapping as it was.
    12.5 µs a keyframe. Each new encoder session (resize, quality, chroma) is built with the
    picture size it shows, so the rewrite follows the parameter sets through rebuilds and their
    session generations.
  - `kVTCompressionPropertyKey_CleanAperture` is not the fix: it is accepted and writes no
    window into the SPS (the header puts it on the output's format description), and an Annex B
    stream carries only the SPS.
  - H.264 streams keep the even size until its encoder is measured for the same.
  - Because an aligned submit encodes the frame (15.3 ms at 3024 × 1968), captures are no
    longer submitted on the capture's queue: each stream has an encode thread fed through a
    one-frame mailbox, where a newer capture replaces one the encoder has not taken, and the
    repair loop submits from the blocking pool. On the capture's queue, drawing plus the submit
    ran past a 60 Hz period and the display was captured at 30.
  - An aligned submit runs the encoder's output callback on the encoding thread before it
    returns, inside whatever the encode holds. The callback used to take the session's lock to
    tell it a new frame rate, while a rebuild waited to write that lock. `parking_lot` lets no
    reader past a waiting writer, so the encode, the callback and the rebuild waited on each
    other for good (five runs in five). The callback now touches the session not at all:
    `apply_bitrate` and `apply_cadence` only store what is wanted, and the next encode tells the
    session (`Shared::tell`) under the lock it already holds. A rebuild no longer writes the
    session either: `install` leaves it staged, and the next encode puts it in (`put_in`). The
    lock order is written in the module doc of `crates/slopty-worker/src/screen.rs` ("Locks").
  - Nothing but an encode waits on one. The encode holds the held capture and the session for
    15 ms at 3024 × 1968 and 38 ms at 5K, and a runtime worker waiting that long answers no
    input. The repair loop's clock reads the held capture's time from an atomic, `install` and
    the mailbox take only their own short locks, and the one path that drops the held capture
    (`forget_held`) is called only from an encode.
  - The encoder's watch sees behind the mailbox. An aligned session never queues, so no frame
    comes back late for the late-run rule to count; frames are replaced in the mailbox instead,
    and a 120 rung was fed 66 a second while the gate, the congestion guard and the encoder
    each budgeted a frame at a 120th of the rate. Each window of 30 due captures is now weighed
    (`EncoderWatch::fed` in `slopty-media`). The delivered rate is the rung times the share of
    its slots the encoder took; a slot is lost only when a due capture was replaced by one that
    falls in the next slot, since a newer capture within the same slot fills it. That rate is
    capped at one frame per mean encode time, keyframes aside. Under seven eighths of the rung,
    the ceiling comes down to it. Over eight sevenths, the ceiling rises back towards what the
    client asked for, so one slow stretch does not cost the rest of the session. The guard
    budgets a frame at the lower of the rung and the fed rate (`frame_fits`). Replacements are
    counted (`superseded`) and logged at close. `ScreenStats` has no field for them, so showing
    them to the client is a proto change.
  - A capture of the old size never reaches a rebuilt session. `Encoder::encode` refuses a
    picture whose size is not the session's (`CodecError::WrongSize`). The worker then drops
    that capture and keeps the session's keyframe pending for the first capture of its own
    size, and `install` empties the mailbox.
  - Each pipeline carries its own padding (`Pipeline::open_padded`'s `pad_to`), so a
    measurement of the even size no longer changes every other stream in the process.
  - On a hosted runner's virtual Mac the stream as it was asked VideoToolbox for the M2 scaler,
    which the guest does not have (`IOServiceMatching failed for: AppleM2ScalerParavirtDriver`).
    Locally, a session opens two scaler user clients on its first frames at 3024 × 1964,
    1512 × 982 and 756 × 492, and none at the padded 3024 × 1968, 1520 × 992 and 768 × 496
    (`the_scaler_is_opened_only_off_16`). On
    that VM the first keyframe came after the client's 400 ms first wait, and the client asked
    again (the one refresh in `quality_changes_decode_without_a_refresh`). A padded stream
    never takes that path.
  - Measured over loopback QUIC (`drawn_frames_reach_the_glass_on_loopback`), 3024 × 1964 at
    60 Hz: 59 frames a second reach the glass where 30 did, encode p50 / p95 15.2 / 15.4 ms
    against 18 / 49–51, capture → decoded 18.9 ms against 25.3, capture → painted p95 / p99
    34.4 / 35.3 ms against 36–49 / 49–82. In one binary against the stream as it was: encode
    15.2 / 15.3 against 17.7 / 20.9, capture → decoded p95 20 against 37. Capture → painted p50
    stays 32–33 ms on a 60 Hz paint: a frame decoded 19 ms after its capture waits for the
    second tick either way. That p50 needs a paint on decode (the VideoLayer) or an encode
    under about 12 ms (stripes).
  - Tests: `conformance::tests` in `slopty-codec` (exp-Golomb and emulation prevention round
    trips, the encoder's own SPSs bit for bit, refusals), `a_padded_surface_holds_the_picture_at_its_top_left`
    and `a_picture_off_16_is_drawn_into_a_padded_surface` in `slopty-capture`,
    `hevc_codes_a_surface_padded_to_16_around_the_even_picture`, and
    `a_drawn_window_streams_at_its_own_size` (854 × 534 coded as 864 × 544, decoded at 854 × 534
    with the desktop at its edges) and `quality_changes_decode_without_a_refresh` in
    `slopty-worker`. The locks: `a_rebuild_beside_a_cadence_change_finishes` (real sessions)
    and `nothing_but_an_encode_waits_on_one`. The watch:
    `a_rung_the_encoder_cannot_feed_follows_what_it_was_fed`,
    `a_seldom_fed_encoder_is_capped_only_by_its_encode_time` and
    `a_ceiling_the_encoder_outgrew_rises_to_what_it_codes` in `slopty-media`, and
    `a_rung_the_encoder_cannot_feed_comes_down_to_what_it_was_fed`,
    `a_capture_replaced_within_its_slot_costs_the_rung_nothing`,
    `a_ceiling_the_encoder_outgrew_rises_back` and
    `the_guard_budgets_a_frame_at_the_rate_the_encoder_is_fed` in the worker. The size:
    `a_picture_of_another_size_is_refused` in `slopty-codec` and
    `a_capture_of_the_old_size_never_reaches_a_rebuilt_session` in the worker. The padding
    parameter: `a_streams_padding_is_its_own`. On the live window server,
    `a_live_padded_capture_is_the_bare_picture_over_black` in
    `crates/slopty-capture/tests/latency.rs` (opt-in, `SLOPTY_SCREEN_E2E=1`). The ignored
    probes `conformance_window`, `encode_time_by_size` (`SLOPTY_PROBE_ALIGN=16`) and
    `submit_blocking_and_pictures_held` in `crates/slopty-codec/tests/chroma444.rs`, and
    `capture_to_glass_padded_against_even` and `capture_to_glass_at_120_past_the_encoder` in
    the worker.

- ⏸ **The encoder writes every frame as a reference; half of them need not be** (2026-09-29,
  M1 Max, macOS 27.0; MEASUREMENTS "temporal layers on the low-latency encoder"). The SPS
  declares two temporal sub-layers (`annexb.rs` tests), but on the worker's session every
  P-frame is `TRAIL_R` with `TemporalId` 0 and the attachment `IsDependedOnByOthers = true`:
  nothing is marked non-reference, and `BaseLayerFrameRateFraction` reads back unset (-1).
  Setting it to 0.5 (a public key, accepted with status 0) makes every other frame `TSA_R` at
  `TemporalId` 1 with `IsDependedOnByOthers = false`.
  - What a non-reference flag on the wire would save: a frame nothing refers to can be skipped
    when parity and a NACK cannot repair it, with no refresh and no wait. Today every lost frame
    costs a `RequestRefresh`, a round trip and an LTR refresh frame, and the picture stands
    still meanwhile. With half the frames in layer 1, half of those refreshes go. It costs
    bits: on the scrolling 1080p text at 16 Mbit/s the mean P-frame went from 14.2 KiB to 15.6
    (17.9 KiB for base frames, 13.3 for layer-1 frames), about 10 %, and the encoder's time
    was the same (7.8 against 7.4 ms p50, within noise).
  - Deferred: it needs a media flag (the attachment is read in `encoder.rs` and would ride the
    `MediaHeader`'s flags) and the reassembler skipping such a frame rather than giving up the
    stream. That is a wire change, and the 10 % is worth measuring against the refreshes it
    saves on the burst-loss simulation first. The keyframe's attachments also carry
    `FECGroupID`, `FECLastFrameInGroup` and `FECLevelOfProtection` (3), hints VideoToolbox makes
    for FaceTime's own FEC; nothing reads them.
  - Test: `temporal_layers` in `crates/slopty-codec/tests/chroma444.rs`, ignored.
  - Taken up the same day: "Frames nothing refers to are skipped, not refreshed" below. The
    10 % measured here was 18–22 % on a session opened layered, and −9 % to +11 % switched on
    a session that has settled, as the worker switches them.

- ✅ **A stream recovers with a keyframe until a reference is acknowledged, and one keyframe
  answers the refreshes that cross it** (2026-09-29, M1 Max, macOS 27.0; MEASUREMENTS "refresh
  storms on loopback"). On loopback with no loss, the drawn display at 3024 × 1964 failed its e2e
  run half the time with decode errors, refreshes and a frame lost. There were four causes, and
  each one fed the next:
  - VideoToolbox answers `ForceLTRRefresh` with a sync frame when no token has been
    acknowledged, and at that size (while the encoder drops frames) every frame after it fails
    to decode with -12909 until the next keyframe. The client's refresh then caused the next
    failure. `Shared::try_encode` now sends such a refresh as a keyframe (`refreshes_idr`), and
    an LTR refresh only goes out when `ltr_usable()` holds.
  - A quality or size change rebuilt the encoder, and the retired session's last frame came out
    after the new session's keyframe. A P-frame from another session reached the decoder. Every
    session now carries a generation, and `Shared::on_session_packet` drops a packet from a
    session that is no longer in force.
  - The first keyframe (about 160 kB) takes 60–130 ms to encode, longer than the client's first
    refresh repeat, so a second keyframe was made behind the first. A keyframe handed to the
    encoder now answers refreshes for `KEYFRAME_IN_FLIGHT_US` (400 ms). The client gives a new
    stream's first keyframe `FIRST_KEYFRAME_WAIT` (400 ms, `Config::first_repeat_after`) before
    it asks again, and no repeat goes out while a keyframe (or a refresh frame, when that is
    awaited) is still arriving.
  - The reassembler counted a frame's deadline from its first fragment. A keyframe crossing a
    connection whose send window was still opening took longer than that, so it was given up on
    half-way (the "one frame lost with 116 datagrams" on a lossless link). Its stragglers then
    re-created the frame, which was lost again. The deadline now runs from the latest fragment
    (fragments still arriving are in flight), and a frame given up on stays given up on.
  - These fixes are ours, not the transport's: QUIC delivered every datagram.

- ✅ **The remote picture keeps its aspect: letterboxed on the tile's surface, with the pointer
  mapped through the same rectangle** (2026-09-29). The surface was stretched to the tile, so a
  window between its resize and the worker's answer, a window that refuses the size, and any
  display whose aspect is not the tile's all drew distorted, and a click landed off the point
  under it.
  - The picture is fitted with `ObjectFit::Contain`. `zoom::fit` computes the same rectangle,
    and zoom, pan, pointer and cursor all work in fractions of that rectangle.
  - A press on the bars goes nowhere. A release or scroll that ends on them is clamped to the
    picture's edge.
  - Window tiles still ask the worker for the tile's size (`resize_remote_windows`), so the bars
    last only until the window follows.
  - Tests: `a_picture_of_another_aspect_is_letterboxed_and_the_pointer_follows` and
    `fit_keeps_the_pictures_aspect_and_centres_it` in `slopty-ui`. The stream goldens check
    that the bars are bare surface. (The bars' colour is superseded 2026-10-04: "Remote
    pictures sit on a dark stage".)

- ✅ **Frames nothing refers to are skipped, not refreshed; the encoder writes them while the
  link loses, on a session that has settled** (2026-09-29, M1 Max, macOS 27.0; MEASUREMENTS
  "temporal layers on the worker's session", "temporal layers switched on a live session" and
  "temporal layers under clumped loss").
  - The mechanism. `BaseLayerFrameRateFraction` 0.5 with `BaseLayerBitRateFraction` 0.8 makes
    every other frame one nothing later predicts from (`IsDependedOnByOthers` false). The
    encoder reads that attachment onto `EncodedPacket::discardable`. The packetizer sets
    `slopty_proto::media::flags::DISCARDABLE` on every datagram of such a frame, and
    `PREV_DISCARDABLE` on every datagram of the frame after it, so a receiver that lost the
    whole frame still knows. The flag is never set on a keyframe, a refresh, or a frame that
    carries a long-term reference.
  - The reassembler NACKs such a frame once, and only while no later frame is complete. It
    never waits for it behind a complete one. Once parity and that NACK cannot repair it, it
    skips the frame: no refresh and no give-up of the stream. A partial frame learns it may be
    skipped from the next frame's `PREV_DISCARDABLE` too. The frame counts in
    `frames_skipped`, not as lost; its missing datagrams still count in the loss rate that
    parity follows. On the client, a decode error on such a frame with the decoder still
    whole asks for nothing either.
  - Measured on the encoder: leaving out every such frame, every other one, or those around a
    refresh decodes every kept frame to the same picture as the whole stream, with no error,
    on sessions opened layered and on sessions switched live. A lost base frame recovers with
    a refresh on either kind of slot.
  - Measured on the link (a simulation with the frame sizes of layers switched on a running
    session): over the clumped-loss cases, refresh episodes went from 69 without layers to 28
    with them. The picture stood still 13–51 % less at 1–10 % loss in runs of 2 and 4, and the
    wire carried 7–9 % less. With layers but a receiver that did not know, 40 % of the
    refreshes came from a frame it could have skipped.
  - What they cost depends on when they are switched on, and the worker switches them only on
    a session that has coded `LAYERS_AFTER_FRAMES` (300, five seconds at 60):
    - switched on such a session they cost −9 % to +11 % of the bytes where the rate has room,
      at the same picture; where it binds, HEVC moved −0.6 to +0.9 dB and H.264 lost 1.3–1.7 dB,
      and no layered phase dropped a frame;
    - opened with them, or switched on within the first ten frames, they cost 18–22 % more
      bytes, 2–6 dB and more dropped frames; switched on after the keyframe alone, 8–17 dB.
  - The gate (`LayerGate` in `slopty-media`) follows the link and nothing the layers move. On
    after two report windows in a row whose parity ratio is above `Redundancy::MIN` (about
    100 ms), off after 40 clean windows (two seconds). The encoder's spend cannot judge room:
    layered sessions spend under a binding target, so spend reads as room exactly while layers
    are on. H.264's 1.3–1.7 dB where the rate binds is paid only while the link loses, against
    13–51 % less still picture; smoothness ranks above sharpness.
  - A safety valve for what the probes did not see. A layered session never dropped a frame in
    any measured phase, and a fraction the encoder cannot code drops every one. The encoder
    counts frames it dropped (`Encoder::frames_dropped`), and if a layered session drops any
    in a report window, layers go off and stay off for 200 windows (ten seconds).
  - Only measured pairs layer (`layers_measured` in the encoder): HEVC 4:2:0, HEVC 4:4:4
    10-bit and H.264 4:2:0, each coding every other frame as layer 1. On anything else
    `set_temporal_layers(true)` leaves the session as it is.
  - Off puts both fractions back to 1.0. Leaving the bit fraction at 0.8 cost up to 0.6 dB
    where the rate binds. If setting either fails, both are reset to 1.0.
  - 0.5 is the only fraction this encoder codes. 0.67 and 0.75 are accepted, and then every
    frame comes back dropped.
  - Latency: the encoder's time does not move (6.3 against 6.4 ms at 1080p, 15.25 against
    15.27 at 3024 × 1968). A skipped frame shows its successor one frame interval later, where
    the same loss used to cost a round trip and a refresh.
  - The client's reports do not yet say whether a stream is layered; `ScreenStats` gains
    `layered` with the stripes wire change below.
  - Tests:
    - `layers_follow_the_link_with_hysteresis`,
      `a_layered_session_that_drops_frames_loses_them_for_the_hold` and
      `drops_without_layers_change_nothing` in `slopty-media`;
    - `a_frame_nothing_refers_to_that_never_arrives_is_skipped_without_a_refresh`,
      `a_frame_nothing_refers_to_is_nacked_once_and_shown_when_repaired_in_time`,
      `a_frame_nothing_refers_to_is_not_waited_for_behind_a_complete_one`,
      `at_its_deadline_a_frame_nothing_refers_to_is_skipped_and_any_other_refreshed`,
      `a_partial_frame_learns_it_may_be_skipped_from_the_next_frame` and
      `the_layer_bits_ride_the_frame_and_the_one_after_it` in
      `crates/slopty-media/tests/pipeline.rs`, and
      `a_frame_carrying_a_reference_is_never_skippable` in the packetizer;
    - `a_refused_frame_asks_by_what_it_was_and_what_is_left` and
      `a_failed_frame_nothing_refers_to_asks_for_nothing_unless_the_session_went` in
      `slopty-client`;
    - `layers_wait_for_a_settled_session_and_follow_the_gate` in the worker;
    - `layers_mark_every_other_frame_and_turn_off_live` and
      `layers_on_the_full_chroma_and_h264_sessions` in `slopty-codec`, on the real encoder:
      every other frame marked, both fractions read back 1.0 after off, every frame carries
      its error, none dropped.
  - Measurements (ignored): `temporal_layers_skip_and_toggle`, `temporal_layers_live_switch`,
    `temporal_layers_when_switched_on`, `temporal_layers_by_codec` in `chroma444.rs`, and
    `layers_under_burst_loss` in `burst_loss.rs`.

- ✅ **A still picture is refined until the encoder stops gaining on it** (2026-09-29, M1 Max,
  macOS 27.0; MEASUREMENTS "a still picture refined").
  - When the screen stops, ScreenCaptureKit sends nothing more, and the last frame stays as
    it was coded. Handed the same picture again, the low-latency encoder spends the next
    frames on what it missed. At 8 Mbit/s, 1080p 4:2:0 went from 48.5 to 52.9 dB of luma in
    eight frames, and 4:4:4 from 35.8 to 44.2. Where the moving picture already had its rate
    there was little to gain: 53.4 to 53.6 dB at 16 Mbit/s.
  - Once the picture is quiet (the repair loop's quiet mark), the worker codes the held
    capture again, one refinement frame at a time. The policy is `Refine` in `slopty-media`.
    It uses the encoder's own error per frame (`CalculateMeanSquaredError`, read back as
    `EncodedPacket::mse`; it costs no encode time measured, and a session takes it only
    before its first frame, so it is set at open). A refinement goes on while the error falls
    by at least 0.2 dB over two frames. Only ratios of the error are used, so 10-bit 4:4:4,
    whose error is in 10-bit units, reads the same.
  - It stops at a plateau, on a lossless frame, after 32 frames, after 4 when the encoder
    reports no error, when its frames have taken half a second of the encoder's rate
    (`REFINE_BUDGET_MS`), and as soon as a new picture is coded. Only a fresh capture starts
    a picture over. A keyframe or refresh answered while the picture is still is a frame of
    the same picture: its error restarts the record, but not the count or the budget, so
    refreshes cannot keep refinement going. A forgotten capture and a rebuilt session end it.
  - It never competes with a change for the link. A refinement goes only onto an empty link
    (nothing held in QUIC's send buffer; otherwise `NoRoom`). A change that follows is not
    held back by the refinement's own bytes: the congestion guard (`frame_fits`) leaves out
    the last refinement's wire bytes, and any other frame ends that exemption.
  - It never competes with a change for the encoder for long. The low-latency encoder codes
    inside the submit, so a change that lands on a refinement waits for it. Refinement frames
    are therefore at least two periods and four mean encode times apart, and take no slot of
    the cadence, so the change behind one goes at once. A change can wait for at most one
    refinement's encode, and on a slow link for the rest of that refinement's bytes ahead of
    it on the wire. Measured input to glass on a still screen that changes on a click every
    80–150 ms, about one refinement between clicks: p50 26.2–28.3 ms with refinement against
    26.8–27.5 without on loopback, 45.8–47.0 against 44.2–47.1 on a 20 Mbit/s link with 5 ms
    each way, and p95 within 2.5 ms either way. Those refinements were small (about 1 KB); a
    large one on a slow link is covered by the guard's test, not by this measurement.
  - Its stamp for the encoder is a period before it is sent, and after the last stamp.
    Stamps only go forward, so one stamped when sent would push a capture taken just before
    it to a stamp past its capture time. Its capture time on the wire is when it was sent.
    The encoder was not seen to squeeze the change after a refinement either way, and a
    refinement leaves the change better and cheaper than no refinement (1.9 dB on 4:2:0 at 8
    Mbit/s; 0.5–1.3 dB and a third to a half of the bytes on 4:4:4).
  - Refinements are not frames of the source. They count apart from repairs (`refined` in
    the stream's close log), and not in `encoded`, the source's idle tracking, the encode
    figures, the capture-to-packet latency or the encoder watch. `ScreenStats` has no field
    for them yet; it gains `refined` with the stripes wire change below.
  - Tests:
    - the `refine::tests` in `slopty-media` (the gains from the measured series, every bound
      and the budget, refreshes that do not restart it, refinements known by their stamps,
      the spacing, the stamp and a rebuild);
    - in the worker, `a_still_picture_is_refined_until_the_encoder_stops_gaining` (the budget,
      only onto an empty link, the stamp, two periods apart, kept out of the stats, and a
      change taken just before a large refinement sent at once with its own stamp and not
      dropped by the guard) and `refreshes_forgetting_and_a_rebuild_end_refinement`;
    - the ignored probes `still_picture_refinement` and `a_change_after_a_refinement` in
      `chroma444.rs`, and the measurement `input_to_glass_on_a_still_screen_while_refining`
      in the worker.

- ✅ **Two stripes halve the encode at 3K and above, and cost a scroll twice the bits**
  (2026-09-29, M1 Max with two `ave2` engines, macOS 27.0; MEASUREMENTS "stripes across the two
  encode engines"). The question was whether two low-latency sessions, each coding its own
  horizontal stripe of the same capture at the same instant, run on the two engines at once.
  - They do, once the sessions ask for more than one engine's pixel rate. Measured one frame
    in flight, the whole picture against two stripes, with the stripes back within 0.6 ms of
    each other:
    - 4K: 20.6 → 11.0 ms without the overlap below, 11.5–12.8 with it; 60 fps kept where the
      whole picture made 42–49;
    - 5K: 35.0 → 18.5 ms (21.2 with the overlap), about 50 fps where the whole picture made 27;
    - 3024 × 1968 declared at 120: 15.3 → 8.5 ms, 8.7–9.5 with the overlap.
  - Below that load the driver puts both sessions on one engine, and they take turns: the
    stripes come back 3.3 ms apart at 1080p and 8.2 ms apart at 3024 × 1968 declared at 60,
    and the pair is slower than the whole picture. Declaring `ExpectedFrameRate` 120 moves
    3024 × 1968 onto both engines on a 60 beat (18.1 → 8.7 ms with the overlap). It costs
    nothing measured: at a binding 6 Mbit/s, declared 60 and declared 120 spent 6.01 and 5.99
    Mbit/s and gave 37.69 and 37.76 dB. 1080p stays on one engine either way. Four stripes are
    never better than two on two engines.
  - The design's bar was 4K at no more than 14 ms: met at 11.5–12.8 ms with the overlap. A 120
    beat stays out of reach: at 4K 11.5 ms is more than a period, and the mailbox carried
    72–90 frames a second, not 120. At 3024 × 1968 two stripes without the overlap kept 118 of
    120 (8.5 ms, 19 ms late at the median), and with the overlap they take longer than the 8.3
    ms period and fall behind (88–100 a second).
  - Seams: coding 64 rows past each seam in each stripe, with each stripe showing only its
    own rows, keeps the rows at the seam within 0.4 dB of the whole picture's. The cost is
    about 6 % more rows coded. Without the overlap, the seam was 1.5–4.3 dB worse.
  - The cost is scrolling. Text scrolled into a coded region enters at its bottom edge, where
    nothing predicts it, and that edge is nearly all a scroll's bits. Two stripes have two such
    edges. With room under the target they spent 1.3–2.6× the bits for the same picture. Where
    the rate is the limit they lost 7–10 dB at the same spend.
  - So stripes are a mode for a link with room, not a default. They are on only when all of
    these hold:
    - this machine's two engines are shown to run side by side (timed once per process at the
      stream's stripe size, while the engines are idle: "The stripe timing waits for idle
      engines", below);
    - the whole picture's encode is over 12 ms (3024 × 1968 and up);
    - the encoder spends well under half its target, so twice a scroll still fits.
    Switching mode rebuilds the sessions, so the gate holds for seconds, not report windows.
  - Not taken:
    - the plain hardware encoder, which overspent its target 2× and queued its stripes;
    - slices in one bitstream (`NumberOfSlices` fails every frame, design §3.1);
    - two capture streams, one per half, which would not share a display time.
  - Measurement: `stripes_across_engines` in `crates/slopty-codec/tests/chroma444.rs`
    (ignored), with the knobs for stripes, overlap, declared rate, target and encoder in its
    doc. The build plan is the next entry.

- ✅ **Two stripes, built: the wire, the worker and the client** (designed 2026-09-29, built
  2026-09-30; the ruling above). The plan as it was written; "Two stripes, as built" below says
  where the build departs from it.
  - Build only behind the gate in the ruling. Two stripes, never more.
  - Geometry, one function both ends call (built: `slopty_codec::stripes::layout`, see "A
    large stream takes both encode engines while it has them"). The seam is the multiple of 64
    (the HEVC CTU) nearest half, the lower one on a tie, and each stripe also codes `OVERLAP` =
    64 rows past it. Stripe 0 shows `[0, seam)` and codes `[0, seam + 64)`; stripe 1 shows
    `[seam, H)` and codes `[seam − 64, H)`. For 3024 × 1968 that is 960 + 1008 shown and
    1024 + 1072 coded; for 3840 × 2160, 1088 + 1072 and 1152 + 1136; for 5120 × 2880,
    1408 + 1472 and 1472 + 1536. Every coded height is a multiple of 16 when H is, which the
    padding already guarantees.
  - Wire (`slopty-proto`):
    - a `Stripe { media: StreamId, coded_top, coded_rows, shown_top }` in `screen.rs`: each
      stripe is its own media stream, with its own reassembly, NACKs, refreshes and reports;
    - `ScreenEvent::Opened` and `ScreenEvent::Geometry` gain `stripes: Vec<Stripe>`, empty for
      one picture. With stripes, `stream` stays the stream the client asked for (control,
      audio, cursor), and video arrives only on each `Stripe::media`. A resize can turn stripes
      on or off and moves the seam;
    - `FramePrefix` gains a `stripes: u8` mask of the stripes coded from this capture (0 when
      unstriped) and 3 reserved bytes, so `FRAME_PREFIX_BYTES` goes from 16 to 20. The capture
      is already named by `capture_ts_us`, which every stripe of one capture shares;
    - `ScreenStats` gains `refined` (refinement frames), `superseded` (captures replaced in the
      encode mailbox) and `layered` (whether the encoder writes temporal layers now), which the
      worker counts today and logs only at close;
    - goldens: `Opened` with and without stripes (it has none today), `Geometry` with stripes,
      the prefix round trip with the mask, and the stats literal. `ClientMsg` needs nothing:
      NACKs, refreshes, reports and LTR acks are already per `StreamId`.
  - Capture → stripes (worker). One ScreenCaptureKit stream as today. Each stripe's session
    has its own IOSurface-backed `CVPixelBufferPool` of `coded_rows` rows. Per capture, the
    stripe's coded rows of both planes are copied into a pool buffer (12.4 MB a frame at 4K):
    a CPU copy first, a Metal blit only if the copy measures over 0.5 ms, and never
    `VTPixelTransferSession`, which goes through the scaler the padding work removed. Built as
    `slopty_codec::stripes::StripeCopy`; `stripes_copy_cost` measured 0.15 ms a stripe at 4K
    4:2:0 and 0.6 ms at 4:4:4.
  - Encode (worker). `Live` holds one or two stripe sessions, each with its own `Encoder`,
    encode thread and one-frame mailbox (an aligned submit encodes inside the call, and both
    stripes must submit at the same instant), and its own packetizer, redundancy, LTR book,
    pending keyframe or refresh and generation. The capture goes to every stripe's mailbox
    with one `capture_ts_us`. Every stripe session declares `ExpectedFrameRate` = max(rung,
    120). One `RateController` per stream: each stripe gets `encoder_bps` × its shown rows /
    the picture's rows, and `frame_fits` budgets each its share. `EncoderWatch` is fed by the
    slowest stripe. A refresh or NACK on a stripe's `media` is answered by that stripe alone,
    and the next frame's mask names only the stripes coded from that capture. `LayerGate` and
    `Refine` run per stripe.
  - The gate: `slopty_codec::stripes::side_by_side` (built) times one session against the two
    stripes with their copies at once, and `SideBySide::pays` says whether two beat one by 25 %;
    the worker runs it once per boot and size class and keeps the answer. Stripes go on when that
    holds, the whole picture's mean encode is over 12 ms and the smoothed spend is under 45 %
    of target, and off at 70 %, with a few seconds' hold each way. Switching rebuilds the
    sessions (keyframes) through `install`/`put_in` and sends `Geometry` with the new stripes.
  - Client (`slopty-client`, then the view). The router sends each `Stripe::media` to the
    stream's worker task, which holds one reassembler and decoder per stripe. A `Stitch`
    groups decoded stripes by `capture_ts_us`, presents a capture when every stripe in its
    mask has decoded or one display refresh after the first did (then with the other stripe's
    previous picture, counted as a `seam_tear`). A stripe waiting for its refresh keeps its
    last picture while the other goes on. The view draws each stripe's shown rows in one
    layer, two textured quads with source rects; cursor, zoom, letterbox and pointer mapping
    stay in whole-picture coordinates.
  - Tests to write with it: `layout` at the three sizes; a capture reaching both mailboxes
    with one stamp; a refresh on one stripe coding only that stripe, with the mask saying so;
    the gate's hysteresis; a real session at 3840 × 2160 whose stripes come back within 2 ms
    of each other and under 14 ms, from `/tmp`; `Stitch` with both stripes, one late past a
    refresh, and one stripe's refresh; an e2e on the drawn 4K display with stripes forced on,
    the seam rows' PSNR in the golden.

- ✅ **Two stripes, as built** (2026-09-30, M1 Max, macOS 27.0; MEASUREMENTS "two stripes,
  capture to glass"). Wire changes: `Stripe`, `Opened.stripes`, `Geometry.stripes`, and a frame
  prefix of 20 bytes carrying the stripes' mask and the sessions' build.
  - *Numbers, on a loaded machine.* The encode falls from 15.5 to 9.5 ms at 3024 × 1964 and
    from 20.6 to 12.3 ms at 4K (p50), the encoder keeps 56–57 captures a second where the whole
    picture made 37–49, and capture → glass falls 7–19 ms at the median. At 5K one round's
    stripes came back slower than the whole picture while other processes coded on the same
    engines: stripes pay only while both engines are theirs.
  - *Media streams.* The top stripe's media stream is the stream's own id; the lower one's is
    that id with its top bit set (`Stripe::media_of`, `Stripe::stream_of`). The client's router
    and the worker's connection route a lower stripe's datagrams and feedback by the id alone,
    before `Opened` has named it; a stream's first datagrams often beat that event.
  - *The prefix* carries `stripes`, the mask of the stripes coded from the capture, and `build`,
    the low byte of the number of the sessions' build (`Shared::put_in` stamps it on both
    stripes' packetizers). The build was not in the plan. Without it the client could not tell a
    stripe of the sessions a resize or a chroma switch replaced from one of the new sessions,
    when one stripe's keyframe comes late: it put the new top stripe beside the old lower one,
    a picture of neither size, for as long as the late keyframe took.
  - *Worker.* One encode thread and one mailbox, as for one picture, and a helper thread
    (`Helper`) that submits the lower stripe while the encode thread submits the top one; the
    encode waits for both before it lets its locks go. Each stripe is a `Coder` with its own
    packetizer, redundancy, LTR book, pending requests, refinement and keyframe estimate, and
    a refresh or NACK on a stripe's media stream is answered by that stripe alone
    (`StreamControl::of_media`). A capture counts once, when its last stripe is back, with the
    slower stripe's encode (`Join`); a code of one stripe alone (a refresh) counts on its own.
    The rate controller, the audio copies and the layer gate take the top stripe's reports only:
    the lower one's carry the same link, and counted twice they would cut twice. Each stripe gets
    the encoder's rate times its shown rows over the picture's.
  - *The stripe's picture.* `VideoToolbox::stripe` makes a session whose submit copies its rows
    of the capture into an `IOSurface`-backed picture of its own (`StripeCopy`), on the thread
    that submits it, so the two copies run at once. A capture of any other size than the one the
    stripes were laid out for is refused (`WrongSize`), a taller one too, which the first cut
    copied from; the worker drops it as it drops a whole picture of the old size. A 4:2:0
    stripe takes a 4:4:4 capture, as the whole picture's session does: ScreenCaptureKit goes on
    delivering 4:4:4 for a few frames after a switch back, and the first cut refused every one
    of them.
  - *The gate.* `SLOPTY_STRIPES=on|off` forces it (the tests and the measurements); otherwise
    `stripes::pays` times `side_by_side` once per size and process off the runtime and requires
    it to pay and the whole picture to take over 12 ms, and the spend gate (`stripes::Gate`)
    turns them on under 45 % of the target and off over 70 %, each after 3 s. A size too small
    to take over 12 ms is never timed.
  - *Client.* The stream's task keeps a lane (reassembler and decoder) per stripe, the lower one
    opened by its first datagram and closed by the first frame of one picture; a lower
    datagram after that opens nothing unless it is a keyframe's. The `Stitch` puts a capture up
    once every stripe its mask names has decoded it. Where the plan left it open:
    - the wait of one display refresh runs from the first stripe that came out and was not
      shown. A newer capture replaces the one waited on but keeps that clock, and a stripe of a
      capture the other stripe has already gone past completes nothing and is kept only as its
      stripe's newest picture. The first cut restarted the wait on every stripe that came out,
      so a stripe running a capture behind the other (behind a retransmission, on a 120 Hz beat)
      kept any picture from going up;
    - a stripe waiting for its refresh holds nothing up, and a capture that goes up with a
      stripe's previous picture counts as a seam tear (`ScreenStats::seam_tears`);
    - stripes of two builds never go up together, not even past the wait.
  - *The view.* Each stripe goes to a `VideoLayer` of its own, not two quads in one layer: the
    glass presents a capture's two pictures back to back, and the view places the two layers
    each clipped to its rows (`stripe_places`). The glass counts captures whose stripes the two
    layers showed more than 4 ms apart (`Seams`); one drawable for both would be the only way to
    make that impossible, and that is `VideoLayer`'s to offer. A picture whose stripes meet
    elsewhere than the layers were last placed for (stripes turned on or off, a resize that
    moved the seam) waits for the view's next paint to place them (`Glass::place`); shown
    before, the top stripe filled the whole picture's place for a frame.
  - Open: `ScreenStats` does not carry `refined`, `superseded` or `layered` yet (the plan's
    wire list); the gate's timing is not taken again when the engines become busy later; the
    e2e on the drawn 4K display with stripes forced on, with the seam's PSNR in its golden, is
    not written.
  - Tests: `the_seam_falls_on_a_coding_tree_unit_near_half`, `a_stripe_holds_its_rows_of_the_capture`,
    `a_capture_that_does_not_fit_is_refused`, `a_subsampled_stripe_codes_a_full_chroma_capture`
    and `the_gate_times_both_ways` (`slopty-codec`); `screen_stripes` (goldens) and the units in
    `slopty-proto`; `a_frame_s_stripes_and_build_survive_its_recovery` (`slopty-media`);
    `a_striped_capture_codes_both_and_a_refresh_only_its_stripe`,
    `both_stripes_frames_name_the_build_they_came_from`,
    `a_striped_capture_counts_when_its_last_stripe_is_back`,
    `the_spend_moves_the_stripes_only_after_the_hold` and, through the real sessions,
    `two_stripes_meet_at_the_seam_row_for_row` (`slopty-worker`); the `stitch_tests`
    (`slopty-client`); `a_striped_pictures_shape_is_its_shown_rows`,
    `a_picture_waits_for_layers_placed_for_its_seam`,
    `a_capture_counts_its_stripes_together_or_split` and
    `a_striped_pictures_layers_meet_at_the_seam` (`slopty-ui`). Measurements:
    `capture_to_glass_striped_against_whole` (worker) and `stripes_copy_cost` (codec).

- ✅ **Every interactive session declares 120 until its first frame, so streams get an encode
  engine each** (2026-09-30, M1 Max with two `ave2` engines, macOS 27.0; MEASUREMENTS "several
  streams on the encode engines").
  - Every stream has its own ScreenCaptureKit stream, low-latency session, encode thread,
    packetizer and timers. They share the engines, and that is where a second stream's cost
    went. The worker's own CPU is about 0.05 of a core per 1080p stream at 60, the drawn
    stand-in for capture included.
  - VideoToolbox packs two 1080p sessions declaring 60 onto one engine. Captured on the same
    refresh, as windows on one display are, they take turns: either stream's encode p95 goes
    from 6.6 ms alone to 11.8, while the other engine sits idle. Six such streams fall to 45
    frames a second.
  - The engine a session gets, and its internal set-up, follow the `ExpectedFrameRate` last set
    between `PrepareToEncodeFrames` and its first frame. A rate set only before the prepare
    places nothing, and nothing after the first frame moves the session.
  - Two sessions declaring 120 ran on both engines in every run, at 6.6–6.7 ms p95 each, and
    six kept 60 frames a second. Eight ask for more than two engines hold and fall to about
    44 frames a second either way, but now every stream gets the same share. One declaring 60 beside one declaring 120 (or 240) got an
    engine of its own in about half the runs, so the second stream alone cannot ask for it.
  - So every session made for 60 frames a second or more (`placement_fps`) declares 120 after
    the prepare. It is told its own rate once its first frame is in, and a `set_frame_rate`
    before then is held until after it.
  - The cost falls on a stream alone: up to 0.6 dB at 60 frames a second, with 4–7 % more bits,
    and only where the picture is above 54 dB. At a rate that binds it was +0.3 dB. A session
    made for less than 60 keeps its own rate, because there declaring 120 cost 0.6–2 dB.
  - A 4K session already declares more than one engine, and two ran side by side before too.
    Small windows (896 × 512) pack onto one engine even at 120, and their frames queue there:
    2.9, 4.6 and 8.2 ms p95 for 1, 2 and 4 streams.
  - Favouring the focused stream came later, once the client said which one it is ("A focused
    stream keeps its rate").
  - Test: `a_session_declares_the_placement_rate_until_its_first_frame` (slopty-codec).
    Measurements: `concurrent_sessions` in `crates/slopty-codec/tests/chroma444.rs` and
    `measure_concurrent_streams` in the worker's `screen/synthetic.rs`, both ignored.

- ✅ **HEVC travels length-prefixed** (2026-09-30; MEASUREMENTS "Annex B against length-prefixed
  on the wire" and "HEVC travels length-prefixed"). A wire change: the video payload's framing.
  - VideoToolbox writes each NAL unit behind a 4-byte length and wants them back that way. The
    worker used to rewrite the lengths to start codes, and the client scanned for the start
    codes and wrote the lengths back into a block CoreMedia allocated. On a real 4K keyframe that
    cost 1.39 M instructions on the client's stream task, 80 % of it the scan, right before the
    decode that restarts a stream after loss.
  - An access unit now travels as VideoToolbox writes it (`slopty_codec::nal`). A keyframe
    carries its parameter sets in front as units of their own, so it still describes itself and
    a receiver can build its decoder from any keyframe it reassembles. The host copies the
    sample's bytes once, as before, and walks the lengths to check them where it used to rewrite
    them; a length that runs past the end still drops the frame. The SPS rewrite
    (`conformance::crop_access_unit`) replaces the unit and its length together.
  - `Decoder::decode` takes the reassembled frame's `Bytes`. A walk over the lengths finds the
    parameter sets; the picture's units behind them become the sample's block as they are, a
    block whose custom source (`CMBlockBufferCustomBlockSource`) holds a reference to the bytes
    until CoreMedia frees it. A 4K keyframe's submit went from 1.84 M instructions to 0.45 M,
    and what is left is VideoToolbox's own.
  - Annex B is gone from the code, not kept beside it: nothing on the wire is versioned, and
    every binary is rebuilt together.
  - The walk refuses what a peer could send to trip it: a length that runs past the end, a
    tail too short for a length, a length a `usize` cannot hold, and a unit of no bytes, which
    has no header and which no encoder writes. The 4-byte prefix is the one the decoder's format
    description declares (`NALUnitHeaderLength` 4), and the host refuses a session whose
    parameter sets say otherwise. The fuzz target `nal` holds the three walks (`check`, `units`,
    `AccessUnit::parse`) to one answer on any bytes, and runs the SPS reads and the SPS rewrite
    on them. On its first run it found the rewrite (`crop_sps`) writing an SPS that no longer
    parsed: on a malformed SPS whose last set bit was syntax, the rewrite took that bit for the
    stop bit. The rewrite now reads itself back and is refused unless it shows the size asked
    for. Only the host's own encoder output reaches it.
  - Tests: `units_come_back_without_their_lengths`, `a_malformed_length_is_an_error`,
    `an_empty_unit_is_malformed`, `an_sps_without_its_stop_bit_is_not_rewritten_into_a_broken_one`
    (with its input under `fuzz/regressions/nal/`),
    `the_parameter_sets_in_front_are_split_from_the_picture` (`nal`),
    `a_sample_holds_the_received_bytes_until_it_is_released` (the block is the received bytes,
    released with the sample), `an_access_unit_has_its_sps_rewritten_in_place`, and the round
    trip, which checks every packet's lengths end to end. Measurement: `decode_cost`.

- ✅ **A focused stream keeps its rate; the others give way a rung** (2026-09-30, M1 Max,
  macOS 27.0; MEASUREMENTS "the focused stream when the engines are full", and its first cut).
  A wire change: `ScreenRequest::Focused`.
  - The client says whether a stream's tile has the keyboard in an active window
    (`ScreenRequest::Focused(bool)`, sent from `ScreenView::tell_focus` once per change). A
    press (`ScreenRequest::Focus`) could not say that a tile lost focus.
  - Past what the two engines hold (seven 1080p streams at 60), every stream fell to about 44
    frames a second, the one being worked in with the rest. Now, when the focused stream's
    encoder watch would step down, the streams nobody focuses step down a rung instead and hold
    it for 10 s (`Engines::contended`). The focused stream keeps 56–60 frames a second with a
    p50 of 7–12 ms, where it had 35–44 and 22–28 ms.
  - The first cut asked again at the next window that still read short, while that window still
    counted frames queued before the give-way, and the rest fell a second rung to 15. The next
    give-way now waits 1 s and two of the focused stream's own windows (`SETTLE_US`,
    `SETTLE_WINDOWS`).
  - Losing the last focus ends every hold at once, and so does closing a focused stream. Before,
    a stream that closed while focused left the others held for the rest of the 10 s.
  - Not taken: holding each background capture for up to 8 ms, until the focused capture of
    the same refresh has gone into its encoder. An engine codes its sessions' frames in the
    order they arrive, so this should have put the focused frame first. Over three interleaved
    rounds its p95 moved less than the rounds differed from each other. It cost the rest 1–3 ms
    a frame and left them at 15–20 frames a second more often.
  - Not available: a priority between sessions. VideoToolbox has no such key. `RealTime`,
    `MaximumRealTimeFrameRate`, `PrioritizeEncodingSpeedOverQuality` and `MaxFrameDelayCount`
    act on a session's own pipeline, not its place on the engine. Every session already asks
    for all four (a delay count of 0, at most 120 frames a second), where the encoder takes
    them.
  - Open: under a machine load of 20–55 the focused p95 is 13–40 ms at 7–8 streams, not under
    16.7. What queues in front of it on the engine is the rest's frames, coded at full size.
    The next idea to measure is giving way by scale rather than rate, so each background frame
    holds the engine for less time.
  - Tests: `the_unfocused_streams_give_way_until_they_have_no_rung_left`,
    `the_hold_ends_when_nothing_is_focused` (worker `screen/engines.rs`),
    `the_worker_hears_the_tiles_focus_once_per_change`,
    `a_tile_drawn_focused_in_an_active_window_is_focused_at_once` (slopty-ui `screen.rs`), and the
    `ScreenRequest::Focused` golden. Measurement: `measure_concurrent_streams`, ignored.

- ✅ **Pictures go to a layer of their own, not through a GPUI frame** (2026-09-30, gpui-fast's
  `VideoLayer`; MEASUREMENTS "a stream's pictures on a layer of their own").
  - Each picture drew the whole window again: the pump took it on the main thread, the view
    drew it with GPUI's surface element, and it reached the glass with that frame on the
    display's next tick. Now the decoder's callback hands it to the stream's presenter on its
    own thread (`ScreenHandle::set_present`). The presenter gives it to a `VideoLayer`, a
    `CAMetalLayer` in a native host under GPUI's layer, drawn on its own thread with GPUI's
    surface shader, one picture at most on its way to the glass, and the newest waiting
    (`slopty-ui::screen::glass`).
  - The view places the layer in its own frame (`Window::paint_native`) at the picture's
    fitted or zoomed rectangle, worked out from the body as that frame lays it out. The body and
    the strip clip it. It has no hitbox, so GPUI keeps the pointer and the view forwards it. A
    tile not drawn places nothing, which hides the layer. A tile moved to another window gets a
    host there, and its layer moves with it.
  - The view draws again only for what is GPUI's: the pointer, the zoom readout, the overlay,
    and a picture of a new size or chroma. A cursor sample that moves the pointer draws it at
    once. The fold that held it for the next frame is gone, since no frame is coming.
  - On loopback, arrival → glass fell from 12.2–13.9 to 1.0–1.2 ms p50 and from 14.2–16.3 to
    1.2–3.3 ms p95, and the window drew 160 frames in 20 s instead of about 1 420. The drawn
    display beats in phase with this Mac's refresh, which is the layer's best case. A remote
    source's gain is the wait for the display tick plus the frame itself.
  - The pacer's clock now stops at the layer's report (`on_presented`). A picture the layer's
    mailbox replaced counts as skipped. A picture drawn that the display did not time is left
    untimed, as GPUI's own frames were. A remote desktop's virtual display reports no scan-out,
    and counting those as skipped would have marked every stream "Frames late".
  - A render of the window cannot see a layer. For the e2e's `Render`, each stream draws its
    newest picture with GPUI too, over its hole, for that frame (`capture_pictures`). The
    shader is the layer's, so the goldens keep checking the picture, and a check holds the
    layer's presented placement to the picture drawn there (`assert_layer_on`).
  - Tests: `a_picture_costs_the_view_no_frame`, `the_layer_goes_where_the_picture_is_drawn`,
    `the_layer_follows_its_tile_through_the_strip`,
    `samples_draw_at_once_and_pictures_draw_nothing` (`screen`),
    `a_report_times_its_picture_or_counts_it_skipped`,
    `an_untimed_report_is_skipped_only_when_the_mailbox_replaced_it`,
    `pictures_wait_for_a_layer` (`screen::glass`),
    `a_picture_replaced_before_the_glass_is_skipped` (`pacing`), and the stream goldens.
  - Open: the test platform presents no frames, so a test sees where the view placed the layer
    and not what the window did with it. A test-mode present in the fork would let tests read
    the hosts. The iOS simulator's layer reports nothing, so its stream shows no timings.

- ⏸ **Giving way by scale, not only by rate** (designed 2026-09-30, not built; follows "A
  focused stream keeps its rate; the others give way a rung"). Under a machine load of 20–55,
  the focused stream's p95 stays at 13–40 ms with 7–8 streams, because the engine codes the
  rest's frames at full size in front of it. The idea is to make each background frame
  smaller, so it holds the engine for less time.
  - What it would change. When `Engines::contended` would give way a rate rung, it halves each
    side of the unfocused streams' capture instead: at most one step, and it holds for the
    same 10 s. The capture's output size follows through `SCStream`'s configuration update,
    with no restart. The encoder session cannot change size, so each step rebuilds the session
    and sends a keyframe. Steps back up after the hold cost the same.
  - What it touches beyond the worker. The client maps its pointer at the scale it asked for
    (`ScreenRequest::Scale`), so a scale the worker imposes needs `Geometry`, or a scale field
    beside it, on every step. Otherwise a background tile clicked during the hold lands at
    twice the distance. The layer stretches any picture to its tile, so nothing else on the
    client moves. A tile already painted at half its stream's width or less (the client asks
    that scale itself) gains nothing, so the step applies only to tiles painted larger:
    half-width columns of a large display, and pop-outs.
  - What to measure, all under `measure_concurrent_streams` with a knob choosing rate, scale,
    or both, in three interleaved rounds each on the same load (a 20–55 machine load, 7 and 8
    streams):
    - the focused stream's encode wait and arrival → glass, at p50 and p95;
    - the background streams' frames a second, and their p95;
    - the keyframe bytes and the count of keyframes per give-way and per step back;
    - the rebuild's time on the worker, from the step to the first frame out;
    - how long the pointer mapping is wrong: a background tile's click during a step, with no
      `Geometry` sent, as the negative control.
  - What decides it. Take it if the focused p95 falls under 16.7 ms at 7–8 streams, the
    background keeps 30 frames a second or more, and a step's keyframe costs less than one
    second of the stream's own rate. Otherwise keep rate alone. If the rebuild alone costs the
    focused stream a refresh, the steps must stay fewer than one a second.

- ✅ **Capture to glass on any link: the stream probes the worker's clock** (2026-09-30, M1 Max,
  macOS 27.0; MEASUREMENTS "capture to glass on any link"). The pacer could time a frame from its
  capture only where the worker's clock was the client's (loopback, the bench, the e2e glass
  tool), so the one number a remote desktop is judged on was missing on every real link.
  - *The probe.* Every stream's worker on the client sends `Feedback::Clock { stream, sent_us }`
    as a datagram, one per report for its first eight and then one every fifth report (four a
    second). The worker's connection answers at once through the stream's control, ahead of
    the stream task's own queue (not of what the transport already holds, whose wait lands in
    the down leg and is what the fastest-round-trip filter drops): a `Kind::Clock` media datagram whose `ClockEcho` carries `sent_us` back
    beside the stream's capture clock as the probe came and as the echo left. Both legs are
    datagrams, so a probe never waits behind a lost packet on the control stream, and on both
    ends the arrival is the connection reader's stamp, not the stream task's: the worker's
    `received` is when its reader took the probe off the connection, carried to the stream as
    an `Instant` and read on the capture clock, so the time the probe waited for the
    connection's loop is no part of the round trip. The `Pong` was the other
    candidate: the app sends it only when a link goes quiet and handles it outside the stream,
    and one estimate per stream costs a probe of 6 to 12 bytes and an echo of 41, four a second.
  - *The estimate* (`slopty_media::ClockSync`, pure, so the client and the fuzzer share it) is NTP's: each echo gives the offset
    `((received − sent) + (echoed − arrived)) / 2`, off by at most half its round trip. Probes
    are kept for 30 s. Each 2 s slice gives its fastest probe, and those within 200 µs (or an
    eighth of it on a slower path) of the fastest round trip in the window are fitted by
    weighted least squares. The slope, the drift between the two Macs' quartz, is fitted once
    they span 8 s, clamped to ±500 ppm; two Macs drift tens of ppm, a millisecond a minute at
    worst, which a fixed offset over a 30 s window would smear. Each probe pins the worker's
    clock to within half its round trip of the offset it saw (nothing can do better without
    assuming the path symmetric), and while the clocks drift at a steady rate the line's error
    is a line too. So the stated bound is how far the line strays from what the newest probe
    pins, or from what the fitted probes at either end pin carried to the anchor, whichever is
    less: it holds after every probe, where half the slowest fitted round trip, the first bound
    written, was 111 µs off on loopback while claiming 101 µs, and could not see the fit miss a
    clock drifting past ±500 ppm. A frame captured after the anchor is off by the drift since
    as well, tens of µs between probes. A probe the line cannot explain, even at the full width
    of its own round trip plus half the slowest fitted one and 1 ms, is a clock that stepped (a Mac that slept stops its host clock); two in
    a row that agree with each other replace the window.
  - *Where it lands.* The decoder's callback places each picture's capture on the client's
    clock through the newest anchor (`FrameStamp::captured`), and the pacer times it to the
    layer's glass time. An exact shared clock (`Pacer::share_clock`) still wins where one exists,
    which is how the bench and the e2e tools check the estimate. `ScreenStats::clock` carries the
    estimate; the stats overlay leads its plain line with capture → glass once there is one
    (flagged past three display periods and half the round trip), and its presentation line
    shows `capture p50 / p95 / max ±bound`.
  - *Numbers.* In process on loopback the estimate sits 0.01 ms off the shared clock at p50
    (0.58 ms at worst, while the first probes met a busy runtime); behind a 5 ms-each-way, 3 %
    loss link, 0.26 ms at p50, 0.37 ms at worst; rerun with the bound that holds, 0.06 ms on
    loopback within a stated 1.33 ms and 0.10 ms at p50 (0.37 at worst) behind 5 ms within
    6.9 ms.
    Capture → painted read 15.35 against 15.45 ms and 18.64 against 18.78 ms. Simulated, over
    six queueing sequences with up to 30 ms on each leg and drift from −80 to +150 ppm, it is
    within 66 µs after a minute, finds the drift within 11 ppm, and states 5.0–7.2 ms. On the frame path it costs
    3 ns a datagram (the look for an echo) and 9.4 ns a picture (the anchor read).
  - Wire: `Feedback::Clock`, `Kind::Clock` and `ClockEcho`, goldens `client_clock_probe` and
    `media_clock_echo`. The worker's own time per frame in `FramePrefix` (Moonlight's
    `frameHostProcessingLatency`), which would split the figure into worker, network and client
    even without a clock, is not taken here.
  - Tests: `on_loopback_the_estimate_is_the_shared_clock`,
    `the_estimate_finds_a_drifting_clock_through_a_jittery_link`,
    `the_stated_bound_holds_after_every_probe` (loopback, queueing, a 40 ms path, one leg
    slower, and an 800 ppm clock, the bound checked after each of 240 probes),
    `an_asymmetric_path_stays_inside_the_bound`, `a_clock_that_steps_is_followed_after_two_probes`,
    `a_slow_probe_or_an_impossible_echo_changes_nothing` and
    `the_anchor_reads_the_nearest_moment_across_a_wrap` (`slopty-media` `clock`);
    `a_shared_clock_times_frames_from_their_capture` (`slopty-client` `pacing`);
    `clock_probes_go_out_and_an_echo_places_the_worker_clock` (the stream's worker);
    `a_clock_probe_is_echoed_with_the_capture_clock` and
    `the_clock_probes_time_captures_as_the_shared_clock_does` (the drawn screen through the real
    encoder and decoder, on loopback and behind 5 ms) in `slopty-worker`;
    `to_glass_is_from_the_capture_once_the_clock_is_placed` and the overlay's
    `hud_shows_age_jitter_hold_present_cadence_and_the_verdict` in `slopty-ui`. The `feedback`
    fuzz target (`fuzz/src/feedback.rs`) takes probes the worker echoes and reads back, any
    bytes as a media datagram with a clock echo in it, and any readings an echo may carry, and
    holds the estimate to a drift within ±500 ppm and a bound no tighter than half the fastest
    round trip.

- ✅ **The client is told when the Mac is locked, or its screens have another session**
  (2026-09-30, M1 Max, macOS 27.0). A stream of a locked Mac showed whatever the capture made of
  it with nothing to say why the windows had gone, and nothing the client does can change it:
  someone has to unlock the Mac, or switch back to the session, at the Mac itself.
  - *Detection.* The session dictionary CoreGraphics keeps for the worker's login session
    (`CGSessionCopyCurrentDictionary`, read with the geometry probe off the stream's task).
    `kCGSessionOnConsoleKey` false or `kCGSessionLoginDoneKey` false is another session on the
    screens, the login window or another user after a fast user switch, as Chromium's remoting
    host reads them for its curtain mode. `CGSSessionScreenIsLocked`, the key xnu's
    `IOKitKeysPrivate.h` names `kIOConsoleSessionScreenIsLockedKey`, is present and true only
    while the screens are locked; AltTab and RustDesk read the same key. The
    `com.apple.screenIsLocked` distributed notifications were the other way: they need the main
    thread's run loop, AppKit suspends them for an inactive app, and Apple says they may be
    dropped, so they would still need this read beside them. A read costs 71 µs at the median
    (472 µs at p99) and runs after the probe's bounds are timed, every 250 ms while the stream is
    locked (a locked stream is never quiet, so its probe keeps its period) and at least once a
    second otherwise.
  - *Wire.* `SourceState` gains `Locked` and `Away`, which outrank what the target draws: the
    lock screen may still produce frames, and they do not make the stream `Live`. The receiver
    treats both as it treats `Idle` and stops asking for refreshes no refresh can answer.
    Goldens `worker_screen_source_locked` and `worker_screen_source_away`; nothing else moved.
  - *The tile.* Over the body, the modal scrim (`kit::scrim`) with whatever picture it last
    showed kept under it, and in its middle a lifted card (`kit::elevate`, `radii.lg`) holding a
    `kit::notice`: the Lock or MonitorOff mark, "The Mac is locked" or "The Mac is at the login
    window", and one line saying when the picture returns. It is a status, so a screen reader
    hears it, and it comes and goes with the worker's word, with no motion. It asks for nothing:
    unlocking from the client is not built. Pointer and keys still reach the worker through it,
    as they would reach the lock screen at the Mac, so nothing stops a person from typing their
    password into a display stream either.
  - What ScreenCaptureKit delivers while the Mac is locked (frames of the lock screen, blank
    frames or none) has no primary source and has not been observed here: the state is read
    from the session, so either way the tile says the same.
  - Tests: `the_session_flags_say_what_the_screens_show` (`slopty-capture`),
    `a_locked_mac_or_a_session_away_outranks_the_frames` (the worker's tracker),
    `a_locked_mac_is_said_over_the_picture` and `the_console_notice_says_what_is_so`
    (`slopty-ui`), the ignored `console_read_cost`, and end to end
    `a_locked_mac_reaches_the_client_and_so_does_its_return` (`slopty-workerd` `screens`): the
    drawn Mac locked, its stream served as the worker serves one, and the client's link hearing
    `Locked` 102 ms later, then `Away`, then `Live`. It found the drawn screen the app
    self-test streams (`StudioAt`) never said what its session showed, so a locked drawn Mac
    streamed on as `Live`; it now says what the canvas does.

- ✅ **A large stream takes both encode engines while it has them; no session is shared**
  (2026-09-30, M1 Max with two `ave2` engines, macOS 27.0; MEASUREMENTS "large streams on the
  encode engines"; backlog #14). The question was what several 4K and 5K streams from one worker
  should share: a session each (today), two stripe sessions each, or one session for all of
  them, their pictures side by side.
  - *No shared session.* A session runs on one engine, however large its picture: IOReport's
    per-engine interrupts put every frame of a 4K, 5K or two-4K-wide session on one engine and
    none on the other. So N streams in one session get 1/N of an engine: two 4K pictures in one
    session were coded at 25 a second where two sessions coded them at 46, three at 12.8, and
    three 5K pictures in one failed (`VTCompressionSessionCompleteFrames` -17691).
  - *A session each stays, and a stream larger than one engine is striped.* One engine codes
    about 400 megapixels a second, a 4K frame in 20.5 ms, so a 4K stream alone is a 48 fps
    stream and a 5K one 28. Striped, the same stream alone kept 60 at 12.0 ms (p99 12.2) and
    5K 50 at 19.4 ms. Beside one or two 1080p streams the striped 4K stream still kept 59–60
    (the 1080p ones too, their encode 6.7 → 16 ms p50 as the stripes share the engines), where
    as one session it made 47 and 40.
  - *Under contention stripes cost little and share fairly.* Once large streams fill both
    engines, stripes code 4–10 % fewer frames in all than a session each (4K: 88.6 against 92.6
    a second at two streams, 92 against 102 at four) at the same encode time or better at p95
    (22.8 against 26.0 ms at two). The driver's placement of three sessions is unfair: one gets
    an engine to itself (47.5 a second) and two share the other (25 each), in every run and at
    both sizes, while three striped streams got 30.8 each. So the stripe gate stays per stream,
    as the ruling above has it, and the number of other streams does not enter it; the worker's
    `Engines` give-way works on a striped stream's rung as on any other's.
  - *Nothing else is shared.* A 4K session costs 3.2 MB when the process opens its first and
    0.1 MB each after (3 MB more once it has coded), 0.01 of a core, and nothing on the GPU,
    which is not in the path. The earlier finding stands: what is worth sharing across a
    client's streams is the sound (`docs/decisions/audio.md`, "One sound per worker on a client,
    not one per stream").
  - *Built here* (`slopty-codec`, `stripes.rs`): `layout`, the geometry both ends call (moved
    from the `slopty-media` of the build plan below, so the gate and the copy use the one
    function without a dependency on the wire crate; the client already links the codec);
    `StripeCopy`, a stripe's coded rows of a capture in an `IOSurface`-backed pool buffer of
    its own, 0.15 ms per stripe on its own thread at 4K 4:2:0 and 0.6 ms at 4:4:4; and
    `side_by_side`, the gate's timing of the whole picture against both stripes with their
    copies at once (4K 20.7 → 12.1 ms, 5K 35.2 → 20.4, 3024 × 1968 15.6 → 9.7). Tests:
    `the_seam_falls_on_a_coding_tree_unit_near_half`, `a_stripe_holds_its_rows_of_the_capture`
    (both planes, both chromas), `a_capture_that_does_not_fit_is_refused`,
    `the_gate_times_both_ways`; the measurements `concurrent_encode` (`tests/streams.rs`) and
    `stripes_copy_cost`.
  - *Left for the build plan above* and built since ("Two stripes, as built"): the wire (a
    stripe's media stream, the frame prefix's mask), the worker's two submitting threads,
    packetizers and watch, and the client's stitch.
    At 4:4:4 the copy is over the 0.5 ms the plan set for trying a Metal blit; it is
    26.5 MB a stripe at about 46 GB/s, the memory's rate, so a blit saves CPU (0.04 of a core
    a stripe at 60) rather than time, and waits for a measurement on the worker's path.
  - Not taken: capturing each stripe as a stream of its own (ruled out above: two captures
    share no display time).

- ✅ **A session the system takes away is rebuilt at the size in force** (2026-10-01). On a
  starved machine VideoToolbox malfunctioned in one session (`kVTVideoEncoderMalfunctionErr`)
  and answered every frame after it with `kVTVideoEncoderNotAvailableNowErr` for as long as the
  session lived, while sessions beside it in the same process went on coding. The worker
  logged each as a dropped frame and kept feeding the dead session, so the stream sent nothing
  more and the client's refreshes went unanswered (MEASUREMENTS.md, "an encoder session that
  malfunctions"). That is the shape of the clock test's failure on CI (run 36813467628): one
  frame that did not decode, then 22 refreshes and no picture.
  - *The codec names it.* A frame back with `kVTInvalidSessionErr`, `kVTSessionMalfunctionErr`,
    `kVTVideoEncoderMalfunctionErr` or `kVTVideoEncoderNotAvailableNowErr`, from the output
    callback or the submit, marks the session lost, and from then on `encode` refuses every
    frame as `CodecError::EncoderLost`, the one that found it out included when the callback
    ran inside the submit. The decoder already did the same on its side (`session_lost`).
  - *The geometry tick replaces it.* The first refusal from a session records its number and
    wakes the tick, which builds new sessions at the size, quality and chroma in force, as a
    chroma switch does: keyframes, and nothing for the client to hear but them. Only while that
    session is still in force and nothing is staged in its place, so the captures it refuses
    while its replacements build ask for no second build. A build that fails leaves it in, and
    the next tick tries again.
  - *A keyframe's slots are not the rung's.* The captures the mailbox replaced while a keyframe
    was in the encoder count as neither taken nor lost in `EncoderWatch`'s window. Charged, a
    cold 494 ms first keyframe cut the ceiling to 4 frames a second for 7.5 s
    (MEASUREMENTS.md, "a keyframe charged to the rung").
  - *A keyframe is marked in flight before its submit.* An aligned session runs the callback
    that clears the mark inside the submit, so a mark set afterwards outlived the keyframe and
    left the refreshes of the next 400 ms unanswered.
  - *The stream tests wait on the stream, not the clock* (`next_or_stopped`). They follow the
    geometry when the stream wakes it, as the app's loop does, and fail after 10 s without a
    picture only when both sides ran (the worker captured, the client task reported) and no
    frame was inside VideoToolbox. A machine that did not run the stream is the runner's
    timeout to call; a stream that ran and sent nothing has stopped.
  - Tests: `a_frame_back_from_a_malfunction_loses_the_session`,
    `an_invalidated_session_is_lost` (codec);
    `a_session_taken_away_is_replaced_and_the_stream_goes_on`,
    `a_keyframe_coded_inside_the_submit_is_not_left_in_flight`,
    `a_frame_that_does_not_decode_is_refreshed_and_the_stream_goes_on` (worker);
    `a_slow_keyframe_costs_the_rung_nothing` (media).

- ✅ **A stream's rate is one number: the pictures painted on this client in the last second**
  (2026-10-01). The `stream-window-stats` golden had the overlay say "59 fps" and the status
  bar "0 fps" for the same stream.
  - *Two counters, two clocks.* The overlay divided the frames reassembled off the link by its
    sample period: decoded frames, the ones the pacer then dropped as late or replaced included.
    The status bar divided the pictures put up on the layer by a sample it kept per view, which
    read 0 for the view's first second. It then held that first reading for good. Its clock
    was a timer each draw of the bar set going again, so under a bar drawn more often than
    once a second it never fired. A GPUI test that draws the bar every 400 ms saw "1 fps"
    beside 30 painted.
  - *One source.* The pacer counts the pictures it lets a paint put up, and the frames that
    miss the display, late or replaced, apart from them, over `RATE_WINDOW` (1 s) on its own
    clock (`Pacer::rate`, `PaintRate`). The overlay's figure, the status bar's and
    `fps_label`, which words both, read it through `ScreenView::paint_rate`. Frames that
    missed the display are a figure of their own after the rate in the overlay ("8 late", in
    the warning tone, only while there are any), as the header's "Frames late" mark already
    is. They are never in the rate.
  - *The bar's clock fires.* A draw sets it going only when it is not already waiting, and
    its firing reads the rate again and draws the bar.
  - Tests: `the_rate_is_the_pictures_painted_in_the_last_second_and_misses_apart` (client),
    `a_streams_rate_is_one_number_in_the_overlay_and_the_status_bar` (workspace),
    `the_plain_line_leads_with_the_human_numbers_and_flags_trouble` (health).

- ✅ **The client decodes on a thread per lane, and gives up a decoder that does not return**
  (2026-10-02). The clock test failed on CI (run 36940096068) with "both sides running"
  after a frame failed to decode with status -19092 (not named in the SDK's headers). The
  client's counters had stopped mid-stream: 211 datagrams to the worker's 1335, no stall and a
  68 ms longest gap across 10 s without a picture. The process then took 170 s to exit after
  the panic. The stream's task had stopped inside a VideoToolbox call it made itself,
  and taken its runtime thread with it, so it read, reported and refreshed nothing more. A
  report sent just before it stopped still counted as the task running. Holding the task the
  same way here reproduced the panic and its frozen counters (MEASUREMENTS.md, "a decode
  submission that never returned").
  - *A decode thread per lane.* The stream's task parks a complete frame and hands it over
    a bounded queue (`DECODE_QUEUE`, 32). It never calls VideoToolbox, as the worker's encoders
    never did on the runtime. Refusals come back through `Inflight` the way the callback's
    failures do. A dropped lane ends its thread, which invalidates the session there, since
    that waits for the session's callbacks.
  - *Stuck is given up on.* A submission still inside VideoToolbox after `DECODE_STUCK` (2 s)
    loses its decoder. A new one is started and asks for a keyframe, and anything the old one
    returns afterwards is not shown. A decoder given up on without a picture doubles the wait
    for the next, up to 32 s, so a machine that blocks every submission does not leave a
    thread a second. The count is `ScreenStats::decoders_replaced`.
  - *Only time the stream task ran counts.* Each report charges the submission inside VideoToolbox
    the time since the last report, at most `STUCK_CREDIT` (100 ms). A starved machine holds
    the whole process for seconds: a stress run under background QoS stalled the worker's
    capture beat for 24 s. A submission that waited out the same hold has not stopped. Charged
    by the wall clock, it lost its decoder twice in one run, and each keyframe asked for went
    to an encoder already 24 s behind.
  - *A full queue asks for nothing.* What does not fit is dropped and counted, and so is what
    is predicted from it. One refresh goes out once the queue has room again, or the keyframe
    of the replacement goes out. Asked for at once, every refresh answered went into the same
    full queue and was asked for again: 108 refreshes in 2 s.
  - *Drops are counted where they happen.* The router's full stream queue and the backlog
    bound used to drop silently. They now count into `ScreenStats::datagrams_dropped`, apart
    from `datagrams_lost`, which counts only the fragments of frames the reassembler saw.
  - *A task runs if it reported lately.* `next_or_stopped` takes the client to have run only
    if its counters moved and `ScreenStats::reported_at` is under a second old.
  - Tests: `a_decoder_stuck_in_a_submission_is_replaced_and_the_stream_runs_on`,
    `what_a_full_queue_drops_is_counted`,
    `a_stream_backlogs_until_attached_and_the_backlog_keeps_the_newest` (client).

- ✅ **The worker gives up an encode that does not come back from VideoToolbox** (2026-10-02).
  The clock test timed out on CI (run 36989451177, a hosted virtual Mac) after 36 pictures: for
  170 s it reported a frame being coded in VideoToolbox, until the runner's 180 s limit. A call
  into the encoder had not returned, and the encode held the held capture's lock and the
  sessions' lock across it, so every other encode, the repair loop's included, waited behind it
  for good. In a product that is a remote desktop frozen until the client reconnects. The
  client had the same failure on the decode side and was fixed first ("The client decodes on a
  thread per lane"); this is the worker's half (MEASUREMENTS.md, "an encode that never
  returned").
  - *No lock across a call into the encoder.* Encodes still go one at a time, which keeps the
    presentation times in order and a rebuild wholly before or after a frame, but what they
    hold is a turn (`Gate`), not a lock. An encode reads and writes the held capture and the
    sessions under their locks, decides what each session is to be told, takes out what the
    submits need (the sessions, the image, the lower stripe's helper), lets both locks go, and
    only then tells the sessions and submits. The callback inside an aligned submit took none
    of those locks before either.
  - *The beat watches the turn.* Each look of the stream's beat (every 25 ms or sooner) charges
    the encode inside the encoder the time since the last look, at most `STUCK_CREDIT`
    (100 ms), the same rule the client's decode thread follows: a starved machine holds the
    whole process for seconds, and an encode that waited out the same hold has not stopped.
    The beat was chosen because it already wakes on that period while video flows and while it
    does not, so the watch adds no wakeup.
  - *Two seconds, doubling.* `ENCODE_STUCK` is 2 s of that charged time. A frame takes 5–23 ms
    one at a time on the M1 Max, and the deepest queue an unaligned session built held a submit
    110 ms (MEASUREMENTS.md, "encode time against frame size"), so 2 s is eighteen times the
    worst wait measured and 120 frame periods at 60 fps. It is the client's `DECODE_STUCK`, so
    a frozen picture is bounded alike on both ends. Each session given up on that coded nothing
    since the last doubles the patience, up to 32 s, as every give-up leaves a thread inside
    the encoder.
  - *Giving up.* The beat numbers the sessions out of force, so what they return later is
    dropped as a replaced session's (`replaced_dropped`), takes the turn from the stuck encode
    and frees it, starts a new encode thread in case the stuck one was it, wakes the repair loop
    out of waiting on a stuck repair, and wakes the geometry tick, which builds new sessions at
    the size in force as it does for a lost session. The next encode takes the old sessions out
    of force, and until the new ones are put in the stream codes nothing; their first frame is
    a keyframe from whatever is held. The stuck thread is left where it is, holding nothing
    anyone waits on. If its call returns, it finds its turn gone and changes nothing.
  - *Counted on the wire.* `ScreenStats::encoders_replaced` (control socket, `ctl_reply_screens`
    golden).
  - *The stream tests tell the machine from the stream.* `next_or_stopped` still takes a frame
    inside VideoToolbox as the machine's doing, but only until the beat has charged it its
    patience. Past that the beat should have given it up, so the test fails at once instead of
    waiting for the runner.
  - Cost: about 500 more instructions an encode (11 850 → 12 400, the gate, a retain of the
    image and the sessions' references), no change in wall time at 0.8 µs for the whole path
    around the encoder, against 5–23 ms inside it.
  - Not taken: a submit thread per session, with the encode waiting on it with a timeout. It is
    simpler, but every frame would pay a hop between threads (the decode thread's hop measured
    7.6–15.8 µs at the median, with tails of milliseconds under load) for a failure seen once.
  - Tests: `the_watch_charges_the_beats_own_time_and_waits_longer_on_sessions_that_coded_nothing`,
    `an_encode_that_never_comes_back_is_given_up_and_the_stream_goes_on`,
    `nothing_but_an_encode_waits_on_one`,
    `a_submit_that_never_comes_back_is_given_up_and_the_pictures_go_on` (worker).

- ✅ **Remote pictures sit on a dark stage** (2026-10-04, `.research/rulings-2026-10-04.md` §4a).
  The bars round a picture of another aspect were the tile's content colour, so in the light
  appearance a dark desktop sat between wide white bands and its edge was lost.
  - The body behind a picture is `surfaces.stage`: a neutral near-black (`STAGE`, #0a0a0a) in
    both appearances (black under Increase Contrast until that variant was deleted on
    2026-10-06, `ui.md` "Light and dark only"). Every remote-desktop client checked
    letterboxes on black (RustDesk, Moonlight, Jump Desktop), as the HIG's video guidance does.
  - Only the body is the stage. The header stays on the theme, and a tile still waiting for its
    first picture is the page its words are on, since muted text does not read on near-black
    in the light appearance.
  - It is one token and no extra quad. No setting.
  - Test: `a_letterbox_is_the_stage_in_both_appearances` (`slopty-ui`), reading the body's fill
    from the painted quads before and after the first picture, in both variants; the theme's
    `the_stage_is_the_same_near_black_in_both_variants_and_black_at_more_contrast`. The stream
    goldens moved with it.

- ✅ **A locked Mac can be unlocked from its stream** (2026-10-04, rulings §1, readiness A23).
  The notice over a locked Mac's picture asked for nothing, since unlocking was not built,
  though the lock screen already took the keys a stream sends.
  - The card offers "Unlock here". It lifts the scrim to a slim line at the top, "Type the
    Mac's password, then Return", and gives the stream the keyboard. The person types their own
    password, which goes as keystrokes, as any other key would. Slopty never keeps, fills or
    types a password.
  - The worker's word ends it: any change of `SourceState` takes the line away, so an unlock
    brings the picture back and a lock again brings the card back.
  - The login window, or another user's session, offers nothing of the kind. The worker's
    session is not the one on the screens then, and the keys it posts do not reach them.
  - Test: `unlock_here_hands_the_keys_to_the_lock_screen` (`slopty-ui`), replacing the
    assertion that nothing was offered.

- ✅ **An open the worker refuses is told, not left opening** (2026-10-04, readiness N7). On a
  failed open the worker sent `Closed` for a stream id the client had never heard, so the
  pending open was never cleared and the tile said "Opening…" for good. Screen Recording off,
  a window closed mid-open and a restored window id that did not survive all ended there, with
  the reason only in the worker's log. A failed listing was only logged too, so ⌘O waited on a
  picker that never came.
  - *Wire.* `ScreenEvent::OpenFailed { asked, why }` names what was asked (`OpenAsk::Target`
    for an `Open`, `OpenAsk::Made` with the key for an `OpenDisplay`), since the client learns a
    stream's id only from `Opened`. `ScreenEvent::ListFailed { why }` answers a `List` that
    could not be. `ScreenFailure` is `NotPermitted`, `Gone`, `Unsupported` or the worker's own
    words, so the client can say what to do. `ScreenError::failure` maps the worker's errors:
    ScreenCaptureKit's -3801 and the preflight are `NotPermitted`, a window gone or a target
    not listed is `Gone`. Goldens `worker_screen_open_failed`,
    `worker_screen_open_failed_made` and `worker_screen_list_failed`; nothing else moved.
  - *The client.* The pending open is dropped and the tile is not asked for again until the
    next link (`failed_opens`), so a refused open does not loop. A made display that could not
    be made goes back to the physical one, as one that closes does. The person hears why in a
    notice ("Window 7 did not open. studio may not record its screen. Turn on Screen Recording
    for slopty-worker in its System Settings."), worded by `screen::failure_text`. The tile's
    own body with Retry and Close is the workspace's to draw from `failed_opens`.

- ✅ **The stripe timing waits for idle engines** (2026-10-04, MEASUREMENTS "the stripe timing
  beside a new stream"). A stream's open asked whether stripes pay at its size, and a size not
  yet timed was timed at once. The timing codes 26 frames on three sessions of its own, so it
  ran beside the stream's first keyframe and first frames. At 2560 × 1600 it put a 42–55 ms
  gap in the stream's sending about 500 ms in (6 runs of 8), where the 66 ms stall was seen.
  The timing took 670–710 ms beside a stream, against about 310 ms of encodes at its own
  medians, and its verdict at that size was false anyway.
  - The open takes a size's verdict only when it is known. A size not yet known opens as one
    picture, which is what it opened as while the timing ran.
  - The geometry tick starts the timing, once per size and process, only once no session of
    the worker has put a frame in or taken one out for a second (`engines::Engines::quiet`).
    That is after a still picture's last refinements and past any frame a session holds at
    60 frames a second. Stripes are wanted when the spend is low, which a still or slow
    picture gives, so the verdict comes when it can be used.
  - A picture that starts moving while the timing runs shares the engines with it for those
    300 ms, once per size. Before the worker's first frame nothing counts as idle.
  - Not taken: timing every display size at the worker's start. Windows come in any size,
    the scale moves the size too, and a client often opens its first stream as the worker
    starts.
  - Not taken: deriving the verdict from the stream's own encode time. The whole picture's
    half could come from it, but whether two stripes run side by side cannot be seen without
    coding two stripes.
  - Through the real path (worker, QUIC on loopback, VideoToolbox decode, the app's pacer),
    48 alternated runs at 2560 × 1600: the old schedule's 48–68 ms opening gap is gone at
    moderate load (the longest gap's median 60.1 → 36.4 ms), and the client counted no stall
    on either schedule. The gaps left under heavy load are the encoder's turns on a busy Mac,
    on both schedules alike. `SLOPTY_TIME_AT_OPEN=1` brings the old schedule back for a
    measurement.
  - Tests: `the_engines_are_timed_only_where_they_are_idle` (`stripes`),
    `the_engines_are_quiet_a_second_after_the_last_frame` (`engines`), and
    `a_new_stream_codes_its_first_frames_with_the_engines_to_itself` (`synthetic`, a 2560 ×
    1600 stream on an encoder that counts its timings: one with the old open, none now).

- ✅ **A virtual Mac's encoder stops for good past its 1020th client** (2026-10-04, MEASUREMENTS
  "a virtual Mac's encoder and its clients"). From 2026-10-03 the worker shard on CI hung
  in five of seventeen runs: the VideoToolbox tests timed out one after another, each in a
  process of its own, the killed ones stayed in `?<E`, and the job was cancelled at 45 min
  (runs 37134085979, 37156683749, 37171599737, 37173900555, 37177919762). Reproduced in a
  macOS 26.6.2 guest (tart, Virtualization.framework, as the hosted runners are):
  - *The guest's VideoToolbox is forwarded.* An encode in the guest goes through the kernel's
    `AppleVideoToolboxParavirtualizationDriver` to an encoder in the host's virtual machine
    process. Each guest process that opens a session, encoder or decoder, leaves two of the
    driver's user clients behind (its own and its `VTEncoderXPCService`'s) until the guest
    restarts. That holds after `VTCompressionSessionCompleteFrames`,
    `VTCompressionSessionInvalidate` and the release, which is all Apple asks of a session's
    end, and it is per process: one session or fifty in a row leave two.
  - *At 1020 clients the encoder stops.* In three runs out of three, fresh guests of 4 and 3
    cores, alone or side by side, about the 510th process past boot failed: the system log reads
    "VTVideoEncoderSelection signalled err=-12908" and "No real codec!!", frames answer
    `kVTVideoEncoderNotAvailableNowErr`, calls into a session stop returning, and a process
    that exits stalls in the kernel "for detach from AppleVideoToolboxParavirtualizationDriver",
    which is the `?<E` CI showed. Only a restart of the guest brings it back; another guest
    on the same Mac goes on coding. Thirty-two 2560 × 1600 sessions alive at once got the same
    status for some frames but recovered at once: busy is not stopped.
  - *What the worker did then.* Every encode answered `EncoderLost`, so the geometry tick
    built new sessions at once, every time: 13 301 builds in one stopped run, each one more
    call into an encoder that no longer answered. A replacement now waits when the one before
    it was lost too without coding a frame: 250 ms, doubling to `ENCODE_STUCK_MAX` (32 s), and
    a coded frame brings it back to at once (`LostRetry`). The beat wakes the tick when the
    wait is over, so a lost session is still rebuilt with nothing else moving. A session the
    system takes away once, as the 2026-10-01 entry above has it, is rebuilt at once as
    before.
  - *What CI does then.* The VideoToolbox tests are bounded at a minute on CI (they take
    9.2 s at most on a green runner), so a stopped encoder fails the shard in minutes,
    naming its tests, instead of cancelling the job.
  - *Told apart from a hang of ours* (2026-10-06). The log check ("No real codec") missed two
    of four stopped encoders (`.research/dev-speed-2026-10-06.md` item 7), and three reds on
    2026-10-06 were synthetic-stream tests that passed here in seconds. The gate now asks the
    encoder itself: one keyframe through a real session, under 30 s, before the group runs
    (an encoder that does not answer skips it) and again after a failed run (one that stopped
    answering makes the failure a warning). A failure while the encoder still codes fails the
    lane. The step's deadline is 5 minutes, over a green p90 of 198 s. Tests:
    `a_run_past_its_deadline_is_killed_with_its_group` and
    `the_encoder_probe_is_a_test_of_the_codec` (`xtask` gate), and the probe itself,
    `the_encoder_codes_one_frame` (`slopty-codec`).
  - *Not proven: how a runner gets there.* The whole worker shard on a fresh guest adds 61
    clients (4 → 65), far from 1020, and a hosted runner is a fresh virtual Mac per job. The
    hangs match the stopped encoder in every sign, but either the runner's guest starts a job
    with clients in use or its limit is lower. The CI watchdog is to log the guest's client
    count (`ioreg -r -c AppleVideoToolboxParavirtualizationDriver`) and the driver's lines from
    the system log when a test hangs, which will tell them apart. The first hang fell on
    eabfe413 and none in the thirty runs before it; that commit's stripe timing never ran in
    the hung tests (their sizes are under `TIMED_FROM`, or the timing is a stub), and it
    leaves no client a process does not already leave.
  - Not taken: fewer VideoToolbox tests side by side. The limit is in processes, not in
    sessions at once, so it would only slow the shard.
  - Tests: `replacements_lost_without_a_frame_are_rebuilt_ever_more_slowly` (`screen`),
    `sessions_lost_one_after_another_are_replaced_ever_more_slowly` (`synthetic`, a real
    session that answers every frame `kVTVideoEncoderNotAvailableNowErr`: 181 builds in 3 s
    before, 5 now).

- ✅ **The stripe timing waits for the encoder to answer** (2026-10-04). The engines counted
  as idle once no frame had gone in or come out for a second (`Engines::quiet`), so a call
  that had not returned for a second read as a second of nothing. On a hosted virtual Mac an
  encode has stayed inside VideoToolbox for 170 s (run 36989451177), and the geometry tick
  would then have timed stripes onto it: three more sessions and 26 encodes on an encoder that
  was not answering.
  - Each call into a session holds a count while it is under way (`Engines::enter`): the
    submit, a session's build and a retired session's release. The engines are quiet only
    when no call is, so one that never returns keeps them busy for good. The timing's own
    calls count too, so no stream's engines read as idle while it codes.
  - The timing runs on a thread of its own, not the runtime's blocking pool. A runtime waits
    for its blocking tasks as it shuts down, so a timing call that never returned would hold
    the worker's exit, or a test's end, until something killed it.
  - Tests: `a_call_that_has_not_returned_keeps_the_engines_busy` (`engines`),
    `the_engines_are_timed_only_where_they_are_idle` (`stripes`).

- ✅ **A session build that never comes back is given up** (2026-10-09). CI run 37929905806
  timed out `a_frame_that_does_not_decode_is_refreshed_and_the_stream_goes_on` at 60 s after
  two replacement builds and no stall line. The tests' geometry tick awaited its build inside
  the select arm, so the 10 s stall clock never ran (and every wake restarted it); the second
  build never answered, and the run was cut off without a word. The worker had the same wait:
  its stream task skips the geometry probe while a build is in flight, so a build that never
  came back froze window following and the input's bounds for good, and the build sat on the
  runtime's blocking pool, which holds the runtime's shutdown until it returns.
  - A build runs on a thread of its own (`slopty-build-encoder`), and its wait is charged on
    the worker's own time as an encode's is: a look every beat, each charged at most
    `STUCK_CREDIT`. Past `BUILD_STUCK` it answers `ScreenError::BuildStuck`, which the stream
    takes as any failed build, and the next geometry tick builds again. The thread is left
    inside VideoToolbox and drops what it made if it ever comes back.
  - `BUILD_STUCK` is a minute. A warm build takes 3.5–42 ms and a fresh process codes its
    first keyframe in 0.28–0.34 s through the test runner's directory, but with other
    processes coding on this Mac's engines a first build passed 10 s (a 10 s patience failed
    an open in one of three runs of the group) and the one-keyframe codec probe took 20.7 s. A
    minute is past the slowest seen, and past the bound every CI test is cut at, so a test
    never meets the give-up; its waits name the build instead (below).
  - A process's first low-latency session does more than it looks: `VTCompressionSessionCreate`
    starts a reaction observer (`VCPReactionObserverCreate`), which lists the audio and camera
    devices through CoreAudio's HAL and CoreMediaIO, besides Metal's device list. It is once per
    process, and in the 0.3 s above.
  - The stream tests run the stall clock from the last picture, whatever woke the tick, and a
    build they wait on is named each 10 s with how long it has been inside. The first time, a
    thread of the test's own prints the build thread's stack as `/usr/bin/sample` reads it,
    given up after 10 s, so the next such failure names the call. Read inside the wait, the
    stack held the words back: on this loaded Mac a run of the group was cut off at 120 s with
    the build started and nothing after it.
  - Not proven: which VideoToolbox call that build was in on the runner, and why the stream's
    sessions were being replaced at all on a runner whose other 75 tests ran at speed. The probe
    after the run found the runner's encoder coding in a fresh process.
  - Tests: `a_build_that_never_comes_back_is_given_up_and_holds_nothing` and
    `a_build_wait_charges_only_the_time_it_ran_on_time` (`screen`).

- ✅ **The seam test compares only the seam** (2026-10-09). CI run 37931699857 timed out
  `two_stripes_meet_at_the_seam_row_for_row` at 60 s with 9 of its 12 pairs compared and the
  pictures still coming. The cost was the test's own: unoptimised, a pair took 195 ms of its
  thread (80 ms copying both stripes' whole luma planes, 57 ms for each of its two shift
  curves), and that runner ran CPU-bound work 15 times slower (`clock_drift_is_absorbed_by_slices`
  47.6 s against 3.1 s). It now copies the 128 rows both stripes code and the lower picture's
  rows a shift reaches, and sums each row pairing once for both curves, which are the same sums.
  The test took 4.5–6.6 s of CPU here and takes 1.4–2.2 s; on background QoS, throttled to the
  efficiency cores as a stand-in for that runner, 25–69 s and now 6–10 s. It still compares 12
  pairs, from the first frames after the keyframe on.
