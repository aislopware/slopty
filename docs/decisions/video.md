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

- ✅ **Present**: `CAMetalDisplayLink`-driven tick; `displaySyncEnabled = false`, drawable count 2;
  present on arrival (superseded 2026-09-05: slop-desk vsync-locked hypothesis settled by
  present-on-arrival measurement without buffering; see ruling below).

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
  the fastest whose frames each get 8 KB at the target in force, and `frame_due` admits a capture
  only when its timestamp is a period past the last encoded one (an eighth of a period of slack,
  or SCK's jitter halves the cadence that goes out). Climbing back costs 12 KB a frame rather than
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
  `a_capture_is_due_at_the_cadences_period_give_or_take_an_eighth`,
  `a_collapsed_target_takes_the_stream_down_the_cadence_ladder`,
  `the_cadence_gate_hands_the_encoder_one_capture_a_period`.


- ✅ **The decoder has not regressed; a slow first session is the volume the binary runs from**
  (2026-09-15). The shaped ladder read five rungs of zero decoded frames and a warm-up that took
  28–31 s where 2026-09-05 measured 150–400 ms, which looked like the worst latency defect in the
  project: the app calls `warm_up_decoder` at launch, so half a minute of blank first window. It is
  not in this code. The same test binary takes 118 s from the repo's external volume and 0.56 s
  from `/tmp`, unmodified, and 0.50 s from a disk image on that same external disk attached with
  `-owners on` — see the `testing.md` entry for the isolation and the one-command fix. Nothing in `slopty-codec` or `slopty-client` is owed a change, and the daemons
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
  now run on the blocking pool (`build_encoder`, `retire`), so `set_quality` and
  `check_geometry` are async, and the thread is held for 2 to 3 µs (MEASUREMENTS, "encoder
  sessions off the runtime").
