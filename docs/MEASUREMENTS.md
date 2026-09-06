# Measurements

Numbers that drove or confirmed a decision, with the exact command that produced them. Re-run
before trusting; hardware and network are stated per entry.

## 2026-09-04 — control-stream round trip, loopback: iroh `fast-apple-datapath` costs 50 ms

Setup: mac-studio, `slopty-ptyd` + `slopty-hostd` + `slopty ping` on the same machine, debug
build, direct path selected. `slopty ping` sends `ClientMsg::Ping` on the control stream and
times the `Pong` (application level, includes both daemons' channel hops).

| build of hostd + cli                    | app rtt min | median | max   | QUIC rtt (direct path) |
| --------------------------------------- | ----------- | ------ | ----- | ---------------------- |
| `slopty-net/apple-fast-datapath` on     | 52.8 ms     | 53.8   | 56.9  | 48.3 ms                |
| feature off (plain `sendmsg`/`recvmsg`) | 0.41 ms     | 0.82   | 1.41  | 1.6 ms                 |

Command:

```sh
SLOPTY_DATA_DIR=/tmp/slopty-manual/client target/debug/slopty ping --count 15
```

Ruling: the feature is removed from the workspace (DECISIONS.md, Transport).

## 2026-09-04 — keystroke echo round trip, loopback, debug build

Setup: mac-studio (Apple silicon), `slopty-ptyd` + `slopty-hostd` + `slopty open -- /bin/sh`
all on the same machine, iroh direct path selected (`slopty sessions` showed
`*direct ip:192.168.100.240 rtt ~0.6 ms`). Driver: a Python `pty.fork()` wrapper writing one byte
and timing until the first byte of the echoed frame arrives. Includes Python overhead, the host's
2 ms frame coalescing window, and the CLI's ANSI repaint.

| metric | ms  |
| ------ | --- |
| min    | 4.8 |
| p50    | 6.1 |
| p90    | 6.9 |
| max    | 7.2 |

Command (from the repo root, daemons already running with `SLOPTY_*` env pointing at a temp dir):

```sh
SLOPTY_DATA_DIR=/tmp/slopty-manual/client python3 - <<'PY'
import os, pty, time, select, fcntl, termios, struct
pid, fd = pty.fork()
if pid == 0:
    os.execv("target/debug/slopty", ["slopty","open","--","/bin/sh"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 60, 0, 0))
def drain(t):
    end=time.time()+t
    while time.time()<end:
        r,_,_=select.select([fd],[],[],0.05)
        if r: os.read(fd,65536)
drain(3.0)
samples=[]
for i in range(20):
    t0=time.perf_counter(); os.write(fd, b"x")
    r,_,_=select.select([fd],[],[],2.0)
    if r:
        os.read(fd,65536); samples.append((time.perf_counter()-t0)*1000)
    drain(0.15)
samples.sort()
print("min %.1f p50 %.1f p90 %.1f max %.1f" % (samples[0], samples[len(samples)//2], samples[int(len(samples)*0.9)], samples[-1]))
os.write(fd, b"\x1d"); drain(1.0)
PY
```

Takeaways: the transport adds well under a frame at 120 Hz; the budget is dominated by the
coalescing window. Next: the same probe over Wi-Fi from macbook-pro and from a phone on LTE, and a
release build. This ad-hoc Python driver is a stopgap; the harness is `slopty bench echo`
(`apps/slopty-cli/src/bench.rs`), a CLI subcommand, not an xtask.

## 2026-09-04 — client connect setup time

`slopty sessions` end to end (bind endpoint, connect, Hello/HelloAck, close): about 2 s wall in
a debug build, dominated by endpoint bind (relay connection + discovery). The apps keep one
endpoint alive for the process lifetime, so this is paid once, not per session.

## 2026-09-04 — screen pipeline, first display at 0.5×, loopback, debug build

Setup: mac-studio (Apple silicon), `crates/slopty-host/tests/screen.rs` opens the main display
(native 1920×1080 pt at 1× → 960×540 stream at `scale: 0.5`, 60 fps cap, 8 Mbit/s HEVC) for 2 s
with a mostly static desktop, counting datagrams from the pipeline's queue. Latency is
capture timestamp (SCK, host clock) → packet leaves the VideoToolbox callback, i.e. capture
queue delay + encode + packetize, measured inside the process.

| metric                              | value            |
| ----------------------------------- | ---------------- |
| frames captured / encoded / dropped | 67 / 67 / 0      |
| datagrams (data + parity + cursor)  | 83 + 70 + 1      |
| capture→packet latency, mean        | 8.8 ms           |
| capture→packet latency, max         | 113.7 ms (frame 0, encoder warm-up) |

Command:

```sh
SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-host --no-capture display_stream
```

End to end through hostd and iroh (`apps/slopty-hostd/tests/e2e.rs`, `screen_stream_over_iroh`):
86 frames in 3 s reassembled and hardware-decoded on the client, 0 lost, 0 NACKs, 0 FEC
recoveries on the loopback path.

```sh
SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-hostd --no-capture screen_stream
```

## 2026-09-04 — screen stream end to end, native scale, loopback, debug build

Setup: mac-studio, `slopty-hostd --direct-only` and `slopty bench screen` on the same machine
(`SLOPTY_DIRECT_ONLY=1`), iroh direct path (QUIC rtt 1–6 ms as reported by the client while
streaming). Latency is ScreenCaptureKit's frame timestamp (host clock) → decoded frame handed to
the client, i.e. capture queue + HEVC encode + packetize + QUIC + reassembly + VideoToolbox
decode, measured with `slopty_capture::host_now_us()` on the receiving side (same clock, same
machine). "Moving" = `seq 1 6000000` scrolling in a 900×500 Ghostty window; 60 fps cap,
30 Mbit/s target. Loss injection is `SLOPTY_DROP_PERMILLE=20` on the client (2 % of media
datagrams dropped before the router).

| run                                  | fps  | capture→decoded p50 / p90 / max | arrival gap p50 / p90 | datagrams | FEC / lost / NACK |
| ------------------------------------ | ---- | ------------------------------- | --------------------- | --------- | ----------------- |
| main display 1920×1080, mostly still | 49.0 | 9.5 / 17.9 / (≈0 min) ms        | 17.7 / 35.9 ms        | 521       | 0 / 0 / 0         |
| Ghostty window 900×500, scrolling    | 53.1 | 10.9 / 15.8 / 113.8 ms          | 17.5 / 27.5 ms        | 521       | 0 / 0 / 0         |
| same, 2 % datagram loss injected     | 51.1 | 11.5 / 16.6 / 124.3 ms          | 17.7 / 33.5 ms        | 490       | 9 / 0 / 0         |

Commands:

```sh
SLOPTY_DATA_DIR=/tmp/slopty-manual/client SLOPTY_DIRECT_ONLY=1 target/debug/slopty bench screen --list
SLOPTY_DATA_DIR=/tmp/slopty-manual/client SLOPTY_DIRECT_ONLY=1 target/debug/slopty bench screen --display 6 --seconds 5
SLOPTY_DATA_DIR=/tmp/slopty-manual/client SLOPTY_DIRECT_ONLY=1 target/debug/slopty bench screen --window 927 --seconds 5
SLOPTY_DROP_PERMILLE=20 SLOPTY_DATA_DIR=/tmp/slopty-manual/client SLOPTY_DIRECT_ONLY=1 target/debug/slopty bench screen --window 927 --seconds 5
```

Takeaways: ~10 ms from capture to a decoded frame on the client at native scale, one frame of
arrival jitter; the first frame after `Open` takes ~250 ms (SCK start + encoder warm-up, also the
max latency outlier). 2 % loss is absorbed entirely by parity: no NACK, no refresh, no lost
frame. Window capture has a floor of ~8 ms where display capture goes down to <1 ms — *this
turned out to be the encoder, not SCK's window path; see "capture floor" below (2026-09-05),
which also moves windows onto the display-crop path.* The 60 fps cap yields ~50–53 delivered
fps on a 60 Hz display.

Not yet measured here: a real lossy/jittery path (Wi-Fi, LTE) (release build measured 2026-09-05 in
"capture floor" and 2026-09-06 in "stall attribution, parity overhead and a release row" below).
Arrival → present is measured in "start-up over iroh on a quiet machine, and arrival → present" below.

## 2026-09-05 — capture floor: display vs window vs display-crop, queueDepth, encode (debug + release)

Setup: mac-studio, macOS 26.5, main display 1920×1080 @1x 60 Hz, quiet machine (0 `cargo`/`rustc`
processes during every run below; the first survey run, on a machine with 200+ rustc processes,
is quoted where it differs). Source: a Ghostty window 900×500 running `yes` (a scrolling
terminal), launched by the test or by hand. Loopback: `slopty-ptyd` + `slopty-hostd` in
`/tmp/slopty-glass`, `--direct-only --port 45599`, the CLI paired with `SLOPTY_DIRECT_ONLY=1`.

**Per-frame host instrumentation.** `SCStreamFrameInfoDisplayTime` (mach absolute time, through
`CMClockMakeHostTimeFromSystemUnits`) equals the sample's presentation timestamp on every
frame of every path (offset p50 0.00 ms, n=285 per row), so *capture latency* below is display
time → ScreenCaptureKit callback and *encode* is `VTCompressionSessionEncodeFrame` →
output callback (`ScreenStats::capture` / `::encode`, `slopty host screens`).

### ScreenCaptureKit alone (`slopty-capture/tests/latency.rs`, 5 s per row)

`SCStreamConfiguration` defaults read back: `queueDepth` 8, `minimumFrameInterval` 1/60 s.

| path           | queueDepth | frames (fps) | capture p50 / p95 / max | gaps > 25 ms |
| -------------- | ---------- | ------------ | ----------------------- | ------------ |
| window filter  | 2          | 259 (51.8)   | 0.46 / 2.35 / 27.07 ms  | 18           |
| window filter  | 3          | 257 (51.4)   | 0.49 / 1.48 / 16.07 ms  | 25           |
| window filter  | 5          | 287 (57.4)   | 0.50 / 4.42 / 10.55 ms  | 4            |
| window filter  | 8          | 286 (57.2)   | 0.49 / 3.52 / 12.77 ms  | 2            |
| display        | 2          | 284 (56.8)   | 0.00 / 0.46 / 20.92 ms  | 4            |
| display        | 8          | 284 (56.8)   | 0.00 / 0.57 / 1.92 ms   | 3            |
| display-crop   | 2          | 287 (57.4)   | 0.00 / 0.00 / 0.46 ms   | 0            |
| display-crop   | 8          | 287 (57.4)   | 0.00 / 0.00 / 0.48 ms   | 0            |

The loaded-machine run of the same survey: window 2 / 3 / 5 / 8 → 0.48 / 0.49 / 0.52 / 0.51 ms
p50, 0.85 / 0.94 / 1.05 / 1.10 ms p95, 288 / 288 / 286 / 285 frames, 0–1 gaps; display 2 → 0.00 /
0.11 ms; display-crop 2 → 0.00 / 0.57 ms. p50 is flat across depths in both runs; the p95 and the
gap count move between runs more than between depths. `0.00` on the display paths is a
clamp: ScreenCaptureKit hands the composited display frame *before* the window server shows
it (its display time is the coming scan-out), which the end-to-end rows below confirm.

Crop correctness (`display_crop_shows_the_window_filter_picture`): window filter vs display
crop of a static window, mean |Δluma| 0.366/255 whole frame, **0.054/255 interior** (the
corners: the window filter leaves them transparent, the crop shows what is behind). Crop move
(`moving_the_crop_is_one_configuration_update`): `updateConfiguration` with a shifted
`sourceRect` completes in 17.8–22.5 ms (5 moves), the next frame arrives with the completion.

### End to end on loopback (`slopty bench screen`, 5 s, 60 fps cap, 30 Mbit/s ceiling)

| build   | target                     | capture→decoded p50 / p90 | host capture p50 / p95 | host encode p50 / p95 | fps  |
| ------- | -------------------------- | ------------------------- | ---------------------- | --------------------- | ---- |
| debug   | display 1920×1080          | 4.1 / 10.0 ms             | 0.00 / 0.00 ms         | 7.99 / 8.95 ms        | 57.4 |
| debug   | window, window filter      | 9.0 / 9.5 ms              | 0.42 / 0.51 ms         | 6.71 / 7.96 ms        | 58.4 |
| debug   | window, display-crop       | **1.7 / 7.9 ms**          | 0.00 / 0.00 ms         | 6.76 / 7.84 ms        | 58.6 |
| release | display 1920×1080          | 4.1 / 9.8 ms              | 0.00 / 0.00 ms         | 8.02 / 9.07 ms        | 58.2 |
| release | window, window filter      | 9.0 / 9.3 ms              | 0.42 / 0.49 ms         | 6.70 / 7.08 ms        | 58.4 |
| release | window, display-crop       | **2.6 / 7.8 ms**          | 0.00 / 0.00 ms         | 6.73 / 7.92 ms        | 58.0 |

No drops, no queue-full, no loss in any row (n = 287–293). Release changes nothing: the
pipeline is framework time. The window filter's "8 ms floor" is the encoder: 6.7 ms p50 whatever
the capture path; the crop path wins the ≥ 3 ms the ruling asked for because the display
frame reaches us ~5 ms *before* its display time (encode 6.7 ms, yet capture→decoded 1.7 ms
measured from that time) where the window filter hands the window ~0.4 ms *after* its own
composite. Both rows show the same picture (above).

Commands:

```sh
SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-capture --test latency --no-capture
# hostd on the crop / window path, then the bench (release: target/release/…):
SLOPTY_WINDOW_CAPTURE=crop target/debug/slopty-hostd --direct-only --ptyd-socket /tmp/slopty-glass/ptyd.sock \
  --ctl-socket /tmp/slopty-glass/hostd.sock --data-dir /tmp/slopty-glass/data --port 45599
SLOPTY_DATA_DIR=/tmp/slopty-glass/client SLOPTY_DIRECT_ONLY=1 SLOPTY_HOSTD_SOCKET=/tmp/slopty-glass/hostd.sock \
  target/debug/slopty bench screen --window 3411 --seconds 5
SLOPTY_DATA_DIR=/tmp/slopty-glass/client SLOPTY_DIRECT_ONLY=1 SLOPTY_HOSTD_SOCKET=/tmp/slopty-glass/hostd.sock \
  target/debug/slopty bench screen --display 6 --seconds 5
```

## 2026-09-05 — encoder rate control: low-latency vs VBV keys, and three variants (debug build)

Command: `SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-host --test screen low_latency_versus_vbv --no-capture`
(the test launches Ghostty running `yes`, then `top -s 1`; 900×500, HEVC unless stated, 60 fps,
8 Mbit/s, 5 s per row, quiet machine). `keyframe` is the first IDR; `spike` is max/p50 of the
frames after it.

Probe: `MaxAllowedFrameQP` and `MinAllowedFrameQP` → status 0 on both sessions. The
low-latency session's supported-property list has no `VariableBitRate` / `VBVMaxBitRate` /
`VBVBufferDuration`, no `MaxFrameDelayCount`, no `PrioritizeEncodingSpeedOverQuality`; the
plain session lists all of those plus `LookAheadFrames`, `MCTFLatencyMode`, `AllowOpenGOP`,
and rejects `EnableLTR`.

| variant                          | content   | frames | encode p50 / p95 / max | keyframe | p50 frame | max frame (spike) | rate        | LTR |
| -------------------------------- | --------- | ------ | ---------------------- | -------- | --------- | ----------------- | ----------- | --- |
| LowLatency (the host's)          | scrolling | 289    | 6.70 / 7.15 / 55.3 ms  | 2995 B   | 127 B     | 248 B (2.0×)      | 64 kbit/s   | yes |
| Vbv                              | scrolling | 288    | 6.59 / 6.87 / 36.0 ms  | 2861 B   | 312 B     | 1742 B (5.6×)     | 163 kbit/s  | no  |
| LL + MaximizePowerEfficiency=off | scrolling | 289    | 6.69 / 6.93 / 39.2 ms  | 2995 B   | 127 B     | 257 B (2.0×)      | 65 kbit/s   | yes |
| LL + ExpectedFrameRate=120       | scrolling | 290    | 6.73 / 7.09 / 38.5 ms  | 2377 B   | 121 B     | 520 B (4.3×)      | 62 kbit/s   | yes |
| LL, H.264                        | scrolling | 287    | 6.45 / 7.41 / 31.3 ms  | 2835 B   | 52 B      | 219 B (4.2×)      | 29 kbit/s   | yes |
| LowLatency (the host's)          | top 1 Hz  | 287    | 6.66 / 6.92 / 36.3 ms  | 1233 B   | 327 B     | 81683 B (250×)    | 358 kbit/s  | yes |
| Vbv                              | top 1 Hz  | 288    | 6.62 / 8.03 / 36.0 ms  | 68179 B  | 632 B     | 47314 B (75×)     | 1423 kbit/s | no  |
| LL + MaximizePowerEfficiency=off | top 1 Hz  | 277    | 6.96 / 14.21 / 48.8 ms | 86759 B  | 327 B     | 9492 B (29×)      | 337 kbit/s  | yes |
| LL + ExpectedFrameRate=120       | top 1 Hz  | 280    | 6.77 / 8.08 / 61.7 ms  | 65703 B  | 331 B     | 23495 B (71×)     | 376 kbit/s  | yes |
| LL, H.264                        | top 1 Hz  | 273    | 6.39 / 10.23 / 39.5 ms | 94817 B  | 170 B     | 15366 B (90×)     | 323 kbit/s  | yes |

Takeaways: encode latency is 6.4–7.0 ms p50 in every row — the hardware pipeline, independent
of rate control, content and codec. VBV spends 2.5–4× the bytes for the same picture, spikes
more on the scroller and loses LTR; nothing to adopt. The `top` rows' keyframe sizes vary with
where in `top`'s redraw the IDR landed; the 81 KB "spike" of the host's mode is one full
`top` redraw as a P-frame, the price of 64 kbit/s the rest of the time. A "static" Ghostty
window still delivers ~57 frames/s to the window filter (its blinking cursor redraws), each
one a 120–330 B P-frame. Also: `top` runs 1 Hz, so the 5 s rows saw 5 redraws.

## 2026-09-05 — libghostty plain-text formatter for terminal search, debug build

Command: exploration test in `crates/slopty-engine/src/ghostty.rs` (since replaced by
`scrollback_tests`), `cargo nextest run -p slopty-engine --no-capture`, Mac Studio.

| what | result |
| --- | --- |
| `Formatter` (plain, trim, selection over every row), 904 rows × 80 cols | 0.36 ms, 59.6 KB |
| `str::matches("fox")` over that text | 32 µs, 903 hits |
| `VtEngine::lines(0, 10_000)` (per-cell FFI) over the same 904 rows | 8.1 ms |

The formatter is ~20× cheaper per row than the cell walk, one line per row (interior blank
rows kept, trailing blank rows dropped), so search runs on the host over the whole retained
history per keystroke. Same run found the 10 KB byte cap on scrollback (60k rows written,
904 retained), fixed in a1b8798.

## 2026-09-05 — screen stream over the mesh (Wi-Fi MacBook Pro → Mac Studio), debug build

Setup: `slopty-hostd --direct-only` on mac-studio (Ethernet LAN), `slopty bench screen` on
macbook-pro over Wi-Fi. The LAN is not reachable from the MacBook, so iroh picked the direct path
over the WireGuard mesh (`100.107.14.250`, `SLOPTY_DIRECT_ONLY=1`); idle `slopty ping` on that
path: app rtt min/median/max 6.4 / 9.1 / 12.5 ms, QUIC rtt 10.9 ms, `ping` 0 % loss. Same
targets as the loopback run above (main display 1920×1080, Ghostty window 900×500 scrolling
`seq`). The capture→decoded column is meaningless here — two clocks ~903 s apart — so only
fps, arrival gap, loss and QUIC rtt are recorded; "encoded" is the host's `ScreenStats` line in
the hostd log, "decoded" the client's count.

| run                                  | encoded → decoded | fps  | arrival gap p50 / p90 / max | datagrams | FEC / lost / NACK / refresh | QUIC rtt while streaming |
| ------------------------------------ | ----------------- | ---- | --------------------------- | --------- | --------------------------- | ------------------------ |
| main display, mostly still           | 254 → 254         | 26.0 | 20.0 / 119.4 / 175.0 ms     | 625       | 0 / 0 / 1 / 0               | 17.3 ms                  |
| Ghostty window 900×500, scrolling    | 586 → 567         | 57.7 | 15.9 / 22.8 / 290.9 ms      | 3598      | 2 / 5 / 29 / 5              | 44.3 ms                  |
| main display, Ghostty scrolling on it| 557 → 547         | 55.6 | 16.9 / 23.2 / 191.0 ms      | 3296      | 0 / 1 / 13 / 1              | 14.1 ms                  |

Commands (on macbook-pro, binary copied with `gzip -1 -c target/debug/slopty | ssh macbook-pro
'gunzip -c > /tmp/slopty-bench/slopty'`, paired once with `slopty pair <ticket>`):

```sh
export SLOPTY_DATA_DIR=/tmp/slopty-bench/data SLOPTY_DIRECT_ONLY=1
/tmp/slopty-bench/slopty ping --count 15
/tmp/slopty-bench/slopty bench screen --list
/tmp/slopty-bench/slopty bench screen --display 6 --seconds 10
/tmp/slopty-bench/slopty bench screen --window 927 --seconds 10   # seq 1 20000000 running in it
```

Takeaways:

* The still-display row is capture-bound, not network-bound: SCK delivered 254 frames and all
  254 arrived; the 119 ms p90 gap is the desktop not changing.
* Under load the path carries ~55–58 fps at ~4 Mbit/s (≈3.5k datagrams of ≤1.2 KB in 10 s) —
  the encoder is nowhere near the 30 Mbit/s target, so every loss here is Wi-Fi loss, not
  congestion. About 2–3 % of frames never reach the decoder (19 of 586, 10 of 557); NACKs
  recover most datagrams, parity recovered 2, and 5 refreshes were needed on the window run,
  each costing a ~200–300 ms arrival gap (the max column). Frame-level delivery is what a
  Parsec-class stream needs to be tuned for next: more parity per frame on a lossy path and/or
  a shorter NACK deadline before giving up.
* QUIC rtt rises from 11 ms idle to 44 ms while the window stream runs (Wi-Fi queueing under
  the mesh's userspace WireGuard); the display run stayed at 14 ms. Worth re-checking with
  pacing once the bitrate goes up.
* A target that never produces a frame (a hidden Ghostty window, id 924) leaves the client in
  "need refresh": it re-sends `RequestRefresh` every `refresh_repeat` + 2 rtt (79 in 10 s). Cheap
  (one datagram each) but pointless; a capped retry or a host-side "no frames yet" hint would
  stop it.

Not yet measured: LTE from the phone (release build measured 2026-09-05/2026-09-06 below; arrival→present
hold measured in "start-up over iroh on a quiet machine, and arrival → present" below).

## 2026-09-05 — stalls, not drops: feedback datagrams and stall-aware deadlines on the mesh path

Same setup as the previous section (Ghostty window 900×500 scrolling `seq`, 15 s runs, debug
build, macbook-pro over Wi-Fi → WireGuard mesh → mac-studio). Client run with
`RUST_LOG=warn,slopty_media=debug,slopty_client=debug` so every NACK and give-up is logged;
hostd with `slopty_hostd=debug` logs each NACK it receives with the selected path's QUIC
stats (`slopty_net::endpoint::describe_health`).

What the traces showed before any change:

* A typical gap is the **tail** of a frame: 3–6 of 5–7 fragments missing, parity included
  (parity trails the data). FEC recovered 2 of 29 gaps; NACKs the rest.
* Every give-up had 2 NACKs out and nothing back, at exactly the computed deadline
  (70–83 ms at the smoothed rtt).
* Host side, over a whole run: `lost 0 pkts`, `congestion 0`, cwnd ~100 KB, datagram send
  buffer empty — and the client's two NACKs for the given-up frame arrived at the host 20 ms
  **after** the client had already asked for a refresh, 136 µs apart. The path holds packets
  for 100–300 ms and releases them together; it rarely drops.

| client build                                              | run | fps  | gap p50 / p90 / max        | FEC / lost / NACK / refresh |
| --------------------------------------------------------- | --- | ---- | -------------------------- | --------------------------- |
| NACK on the control stream (before)                       | 1   | 57.7 | 15.9 / 22.8 / 290.9 ms     | 2 / 5 / 29 / 5              |
|                                                           | 2   | 57.8 | 16.7 / 20.8 / 156.9 ms     | 0 / 1 / 20 / 1              |
|                                                           | 3   | 58.1 | 16.9 / 20.5 / 158.0 ms     | 0 / 1 / 10 / 1              |
| NACK/refresh as datagrams (`Feedback`, protocol 3)        | 1   | 56.6 | 15.9 / 23.2 / 183.8 ms     | 0 / 2 / 23 / 2              |
|                                                           | 2   | 57.6 | 15.4 / 22.7 / 159.1 ms     | 0 / 1 / 33 / 1              |
|                                                           | 3   | 58.1 | 15.5 / 22.5 / 151.9 ms     | 0 / 0 / 24 / 0              |
| + flow-aware deadline, `max_hold` 500 ms                  | 1   | 57.5 | 16.1 / 23.1 / 156.0 ms     | 6 / 1 / 35 / 1              |
| + NACK clock restarts when a stall clears (`stall_gap`)   | 1   | 58.2 | 15.4 / 21.9 / 165.6 ms     | 3 / 0 / 23 / 0              |
|                                                           | 2   | 58.4 | 15.6 / 22.3 / 158.2 ms     | 4 / 0 / 17 / 0              |
|                                                           | 3   | 58.1 | 15.5 / 22.6 / 161.7 ms     | 1 / 0 / 23 / 0              |

The one give-up left after the flow-aware deadline was declared in the very tick the stall
released (`flowing=true`, one NACK out, its answer one round trip away); restarting the NACK
clock on resume removed it. The arrival-gap max stays at ~160 ms: that is the stall itself,
which no receiver policy can hide — but it no longer costs an IDR and a dropped run of frames.

Host path stats at the end of the later runs also showed a few *real* losses (5–9 QUIC packets
per 15 s, cwnd cut to 13–20 KB by Cubic). At 4 Mbit/s that is harmless; at the 30 Mbit/s target
a 13 KB window over 10 ms rtt would cap the stream near 10 Mbit/s. Congestion control for the
media datagrams (noq ships BBR3) is the next thing to measure.

Commands (as in the previous section; the bench loop was
`/tmp/slopty-manual/fbbench.sh`, typing `seq 1 20000000` into Ghostty before each run):

```sh
RUST_LOG=warn,slopty_media=debug,slopty_client=debug SLOPTY_DATA_DIR=/tmp/slopty-bench/data SLOPTY_DIRECT_ONLY=1 \
  /tmp/slopty-bench/slopty bench screen --window 927 --seconds 15
grep 'frame lost\|nack' <client log>
grep 'nack\|screen closing' <hostd log>      # path=rtt … cwnd … congestion … lost … space …
```

## 2026-09-05 — BBR3 vs Cubic on the mesh path (host window), debug build

Same bench as above (window 927 scrolling, 15 s, macbook-pro → mac-studio over the mesh),
host path stats from hostd's `screen closing` line. Two runs each.

| congestion control | cwnd at close        | QUIC lost pkts | client: lost / NACK / refresh | gap max        |
| ------------------ | -------------------- | -------------- | ----------------------------- | -------------- |
| Cubic (default)    | 13 660 / 20 326 B    | 9 / 6          | 0 / 23 / 0, 0 / 23 / 0        | 166 / 162 ms   |
| BBR3               | 2 000 610 / 22 010 B | 5 / 5          | 1 / 16 / 1, 0 / 45 / 0        | 525 / 241 ms   |

BBR3 keeps probing (2 MB window mid-run, back to ~22 KB in its probe-RTT phase at the end)
instead of halving on every loss, so the 30 Mbit/s target is no longer window-bound. The one
give-up in the BBR3 run was a 525 ms stall, past `max_hold` (500 ms) — the receiver policy,
not the window. Rtt and delivered fps are unchanged at this bitrate; the real test is a higher
bitrate, still to be measured.

Follow-up, main display 1920×1080 with the same scrolling window on it, 15 s, two runs per
controller back to back (`SLOPTY_CC=bbr3|cubic` on hostd):

| controller | fps         | gap max          | client lost / NACK / refresh | host QUIC lost pkts | host cwnd at close |
| ---------- | ----------- | ---------------- | ---------------------------- | ------------------- | ------------------ |
| BBR3       | 58.0 / 49.7 | 165 / 1584 ms    | 0/48/0, 25/150/32            | 9 / 74              | 15.6 / 9.5 KB      |
| Cubic      | 50.4 / 53.0 | 2044 / 1101 ms   | 19/175/26, 7/61/9            | 74 / 32             | 22.5 / 35.3 KB     |

Three of the four runs hit one- to two-second outages with dozens of real losses; the link,
not the controller, decided them (the earlier clean BBR3 window runs and the perfect BBR3
display run are the same code). Neither controller shows an advantage at this bitrate, so
BBR3 stays the default for its behaviour under loss and the override remains for the next
round on a better-behaved link (or a wired client). An outage longer than `max_hold` costs
one refresh per frame in flight — the 25–32 refreshes in the bad runs — which is the intended
floor: the stream restarts from a keyframe the moment the link is back.

## 2026-09-05 — `slopty bench echo` (pure-Rust keystroke round trip), debug build

Replaces the Python `pty.fork()` driver above. The CLI opens a `/bin/cat` session over the
normal client link, sends one byte at a time and times it to the first `TermEvent::Frame`
back, so the number is transport + host engine + the host's coalescing window, with no
terminal emulation or repaint on the measuring side.

| path                                            | min  | p50  | p90   | max   | QUIC rtt |
| ----------------------------------------------- | ---- | ---- | ----- | ----- | -------- |
| loopback (mac-studio → its own hostd)           | 3.6  | 4.9  | 5.9   | 11.7  | 2.6 ms   |
| macbook-pro over Wi-Fi + mesh, quiet link       | 12.1 | 13.8 | 15.6  | 106.0 | 12 ms    |
| same, taken during a bad Wi-Fi phase            | 12.5 | 78.9 | 132.2 | 451.5 | 86 ms    |

```sh
SLOPTY_DATA_DIR=/tmp/slopty-manual/client SLOPTY_DIRECT_ONLY=1 target/debug/slopty bench echo --count 30
```

On a quiet link the round trip is one QUIC rtt plus ~2 ms (engine + coalescing); the bad-phase
row is the same binary minutes earlier while the radio was stalling, kept to show the spread
a Wi-Fi client sees.

## 2026-09-05 — OSC 8 on the wire: link runs per row vs. an id per cell, link-free screens

Command (both numbers from the same test, run before and after the change):

```sh
cargo nextest run -p slopty-proto size_report --no-capture
```

Encoded `TermEvent::Frame` (postcard, codec framing included), full frame, no links anywhere,
default style; "text" rows are all `x`.

| frame           | `Option<HyperlinkId>` per cell (before) | `Vec<Hyperlink>` per row (after) | saved           |
| --------------- | --------------------------------------- | -------------------------------- | --------------- |
| 80×24 blank     | 15 477 B                                | 13 581 B                         | 1 896 B (12 %)  |
| 80×24 text      | 17 397 B                                | 15 501 B                         | 1 896 B (11 %)  |
| 200×60 blank    | 96 322 B                                | 84 382 B                         | 11 940 B (12 %) |
| 200×60 text     | 108 322 B                               | 96 382 B                         | 11 940 B (11 %) |

postcard encodes `None` as one byte, so an id per cell costs `cols × rows` bytes on a screen
with no links; an empty run list costs one byte per row. A linked row costs its URI once per
run plus 3 bytes of header. Ruling in DECISIONS.md (Terminal, "Links: OSC 8 first").

## 2026-09-05 — OSC 133 marks on the wire: `Prompt { exit }` per row

```
cargo nextest run -p slopty-proto size_report --no-capture
```

Same test as the OSC 8 entry above, now with an all-prompt frame, after the
OSC 8 change.

| frame                              | bytes    | vs. blank |
| ---------------------------------- | -------- | --------- |
| 80×24 blank (`Output` rows)        | 13 581 B | —         |
| 80×24 every row `Prompt{Some(1)}`  | 13 629 B | +48 B     |
| 200×60 blank                       | 84 382 B | —         |
| 200×60 every row `Prompt{Some(1)}` | 84 502 B | +120 B    |

A non-prompt row's mark is still the one-byte variant index; a prompt row with a status costs
three (variant, `Some`, the code). Real screens have one prompt row per command, so the cost is
a few bytes per frame.

## 2026-09-05 — release build, loopback

`cargo build --release -p slopty-hostd -p slopty-ptyd -p slopty-cli`, hostd restarted from
`target/release`, same benches as above on mac-studio.

| bench                                        | debug (earlier today)        | release                      |
| -------------------------------------------- | ---------------------------- | ---------------------------- |
| `bench echo` p50 / p90 / max                 | 4.9 / 5.9 / 11.7 ms          | 4.4 / 4.8 / 5.1 ms           |
| `bench screen` window 927 capture→decoded p50 / p90 | 10.9 / 15.8 ms        | 11.5 / 16.5 ms               |
| `bench screen` fps                           | 53–58                        | 57.5                         |

The pipeline is bound by ScreenCaptureKit, VideoToolbox and the coalescing window, not by
optimisation level; release mostly trims the tail of the echo distribution. Development stays
on debug builds (the `dev` profile already runs with `opt-level` tuned for the codecs).

## 2026-09-05 — adaptive bitrate on the mesh path (Wi-Fi MacBook Pro → Mac Studio), debug build

First run of `slopty_media::RateController` (12 Mbit/s start, cwnd cap, cut/grow on the
receiver reports). The mesh was in a bad state this afternoon (control rtt 14–39 ms against
~10 ms in the morning runs, arrival-gap max 5.7 s), so the numbers are about the controller's
behaviour, not the ceiling of the link.

| stream (20 s)                | fps  | gap p50 / p90 / max     | FEC / lost / NACK / refresh | host target over time (Mbit/s)                          |
| ---------------------------- | ---- | ----------------------- | --------------------------- | ------------------------------------------------------- |
| Display 6, 1920×1080, 60 fps | 16.2 | 32.5 / 65.7 / 5680.2 ms | 19 / 50 / 269 / 83          | 12 → 9.0 (cwnd 22 KB / 14 ms) → 7.8 → 3.2 → 1.0 → 1.5 → 1.1 → 1.0 |

Host side: captured 1146, encoded 1146, datagrams 4643, `queue_full` 0 — nothing dropped on
the host; the client's path stats still said `lost 0 pkts`. So the losses the client counted
were stalls again (packets held, then released), and the controller answered a stall the only
way it can, by cutting to the floor. That is the right move for a congested link and the
wrong one for a stalled one (sending less does not clear a Wi-Fi stall). Follow-up: let the
reassembler's `flowing` state ride in the `ReceiverReport` so a stall freezes the controller
instead of cutting it (protocol change; not done here).

## 2026-09-05 — stall-aware bitrate on the mesh path (Wi-Fi MacBook Pro → Mac Studio), debug build

The follow-up above: `ReceiverReport` now carries `stalled_ms` / `stalls`, a window with a
stall freezes the controller (`RateVerdict::Stall`), the policy's value is separate from the
cwnd-capped target, and every decision comes back to the client as `ScreenEvent::Rate`, so
the trajectory below is what `slopty bench screen` printed, not a grep of hostd's log on the
other machine. Display 6 (1920×1080, a Ghostty window scrolling `seq` on it), 20 s, two runs
per build, private daemons on port 45560 with their own data dir.

The mesh was in a worse state than the afternoon run: this time the host's QUIC path *did*
lose packets (230–335 per 20 s, cwnd 4.8 KB at close, `congestion 229–264`), so the cuts in
the first five seconds are real overuse and the controller took them; and the link stalled
40–50 % of the time (57–104 stalls, 6–10 s of silence per 20 s run), so once at the floor
every window held. The premise of the follow-up (stalls with nothing lost) did not hold
tonight; what the runs show is the two verdicts telling loss from stalls per window.

| run | build                   | fps  | gap p50 / p90 / max     | FEC / lost / NACK / refresh | stalls (stalled) | host QUIC lost / cwnd | host target over time (Mbit/s)                                                                        |
| --- | ----------------------- | ---- | ----------------------- | --------------------------- | ---------------- | --------------------- | ----------------------------------------------------------------------------------------------------- |
| 1   | stall verdict           | 24.7 | 16.5 / 34.3 / 6616.8 ms | 116 / 47 / 206 / 89         | 91 (8.1 s)       | — / —                 | 12 hold → 9.0 cut → 4.8 → 3.6 → 2.7 → 2.0 → 1.5 → 1.1 (cuts, no stall in those windows) → 1.0 hold ×25 |
| 2   | stall verdict           | 26.3 | 9.9 / 81.2 / 1735.9 ms  | 98 / 47 / 206 / 71          | 94 (9.9 s)       | 230 / 4 800 B         | 12 hold → 9.0 cut → 6.8 → 5.1 → 4.8 hold → 2.8 hold ×2 → 1.0 hold ×3 → 1.0 cut → 1.0 hold ×27          |
| 3   | + wanted/target split   | 29.2 | 17.0 / 27.1 / 622.9 ms  | 133 / 42 / 183 / 77         | 57 (6.0 s)       | 335 / 4 920 B         | 9.1 hold/cwnd → 6.8 cut → 3.3 hold/cwnd → 2.4 → 1.8 → 1.4 → 1.0 (cuts) → 1.0 hold ×19                  |
| 4   | + wanted/target split   | 16.6 | 17.0 / 29.8 / 187.8 ms  | 122 / 50 / 140 / 115        | 104 (10.2 s)     | 313 / 4 800 B         | 12 hold → 7.9 cut/cwnd → 6.0 → 4.5 → 3.4 → 2.5 → 1.9 → 1.4 → 1.1 → 1.0 (cuts) → 1.0 hold ×28           |
| —   | loopback control        | 57.3 | 17.3 / 19.4 / 33.4 ms   | 0 / 0 / 4 / 0               | 2 (0.14 s)       | 0 / —                 | 12 hold → 13.5 → 15.2 → 17.1 → 19.2 → 21.6 → 24.3 → 27.4 → 30.0 (grow ×8, 4 s) → 30.0                  |

Host-side decision lines (`rate decision`, window sums) from run 3, the first seven:

```
Stall  target 9.1  capped  loss 53‰ queue 2 hold 35 ms  stalled 100 ms stalls 1
Cut    target 6.8          loss 37‰ queue 1 hold 29 ms  stalled 0
Stall  target 3.3  capped  loss 77‰ queue 2 hold 15 ms  stalled 80 ms  stalls 1
Cut    target 2.4          loss 39‰ queue 1 hold 34 ms  stalled 0
Cut    target 1.8          loss 48‰ …
Cut    target 1.4          loss 38‰ …
Cut    target 1.0          loss 49‰ …
```

What the runs say:

* A window with a stall holds and discards its loss (`Stall`, 5–8 % counted loss thrown
  away); a window without one and with 4–5 % loss cuts. Both verdicts fire on this link,
  which is why the target still reaches the floor: the loss is real (host QUIC counters),
  not a stall artefact, and the floor is where a link dropping 300 packets per 20 s belongs.
* In runs 1–2 the cwnd cap dragged the target down *inside* held windows (4.8 → 2.8 → 1.0,
  all `hold`) and it would have had to grow back an eighth at a time. Since run 3 the cap only
  shadows the policy's value (`hold/cwnd` in the trajectory, `(cwnd)` in the ⌘⇧I overlay):
  the value survives the stall and the target springs back when the window recovers. On this
  link the window never recovered, so the split changed the bookkeeping, not the outcome.
* The loopback control grows 12 → 30 Mbit/s in eight clean windows (4 s) at 57 fps, as
  before. Its two "stalls" were read here as the host's own capture gaps (ScreenCaptureKit's
  warm-up after the first frame, one 55 ms hole mid-run) and judged not worth a host-side
  heartbeat yet. The heartbeat section below measured that reading and found it wrong: the
  host was pushing the whole time, the silence was QUIC's send side.
* Delivered fps on the mesh (17–29) is the link, not the controller: the same display
  streams at 57 fps on loopback and at 58 fps over the mesh on a good morning (BBR3 table
  above).

Not looped: two runs per build were taken back to back and the link was visibly in the
same bad state for all four; a re-measurement belongs to a day when the mesh does not lose
packets, which is the case the stall verdict was built for.

```sh
# on mac-studio: private daemons, own data dir and port
export SLOPTY_DATA_DIR=/Volumes/Lacie/Workspace/oss/slopty-wt/escapes/target/e2e-data/stall
target/debug/slopty-ptyd --socket $SLOPTY_DATA_DIR/ptyd.sock &
RUST_LOG=info,slopty_host=debug SLOPTY_PORT=45560 target/debug/slopty-hostd --direct-only \
  --ptyd-socket $SLOPTY_DATA_DIR/ptyd.sock --ctl-socket $SLOPTY_DATA_DIR/hostd.sock --print-ticket
open -na Ghostty --args -e seq 1 300000000          # motion on display 6, no synthetic keys
# on macbook-pro (binary copied with gzip -1 -c target/debug/slopty | ssh macbook-pro 'gunzip -c > /tmp/slopty-bench/stall/slopty')
export SLOPTY_DATA_DIR=/tmp/slopty-bench/stall/data SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug
/tmp/slopty-bench/stall/slopty pair <ticket>
/tmp/slopty-bench/stall/slopty bench screen --display 6 --seconds 20   # prints stalls and the target trajectory
# host side: grep 'rate decision\|screen closing' $SLOPTY_DATA_DIR/hostd.log
# loopback control: the same bench on mac-studio with SLOPTY_DATA_DIR=$SLOPTY_DATA_DIR/client
```

A window that does not change on screen (`--window 1880`, an idle Ghostty) produced
`captured 0` for 20 s: ScreenCaptureKit delivers nothing while the content is static, which
the client reports as refresh requests. Not a regression; bench a moving window or the display.

```sh
# on macbook-pro, paired with the Mac Studio's manual hostd (SLOPTY_PORT 45550)
export SLOPTY_DATA_DIR=/tmp/slopty-bench/data SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug
/tmp/slopty-bench/slopty bench screen --display 6 --seconds 20
# on the host: RUST_LOG=info,slopty_host=debug hostd → grep 'bitrate\|stream closed'
```

## 2026-09-05 — capture heartbeat, loopback, debug build

The host now sends a bare `Kind::Heartbeat` header whenever nothing left its queue for 25 ms
(`HEARTBEAT_AFTER`, half the receiver's 50 ms stall gap), so a still screen or a capture gap
no longer reads as a link stall at the receiver. Measured on loopback against the control
above (display 6, a Ghostty window scrolling `seq`, 20 s, private daemons on port 45560,
`slopty_host=trace` so every beat is logged with the silence that triggered it).

| run | fps  | first frame | gap p50 / p90 / max     | stalls (stalled) | beats | host target over time (Mbit/s)                                     |
| --- | ---- | ----------- | ----------------------- | ---------------- | ----- | ------------------------------------------------------------------ |
| 1   | 57.9 | 650 ms      | 17.2 / 20.7 / 424.8 ms  | 2 (486 ms)       | 12    | 12 hold → 13.5 … 27.4 (grow ×7) → 14.3 grow/cwnd → 30 hold → 30 grow |
| 2   | 57.3 | 503 ms      | 17.3 / 20.4 / 262.7 ms  | 3 (254 ms)       | 14    | 12 hold → 13.5 … 30 (grow ×8) → 30 hold → 30 grow                  |

What the beat log says:

* (Read on 2026-09-05 evening, "start-up on a cold connection" below: the hold was the
  client's first decoder session, not QUIC; the reading in this bullet is superseded.)
* The start-up hold did not disappear, and the heartbeat is not the reason. In run 2 the
  stream opened at 30.333 s, the first decision at 30.879 s reported `stalled 81 ms, stalls
  1`, and the first beat went out at 31.087 s: the host's queue was never quiet for 25 ms in
  between. The silence the receiver saw was on QUIC's send side, not the capture: a fresh
  connection paces the first keyframe out of its initial window (`space 4147282` of the
  4 MiB datagram buffer still held at 35.18 s in the same run, with the target just raised
  to 30 Mbit/s). That is a link hold and the stall verdict is right to freeze on it.
* The mid-run stalls are the same thing: two beats at 34.79–34.82 s (a 50 ms capture gap,
  covered), then a receiver-side `link resumed gap=58.7 ms` at 35.03 s with the host pushing
  throughout, and the host's cwnd at the 5808 B floor with NACKs at 43.6 s. Loopback QUIC
  holds bursts after a bitrate step; the receiver cannot tell that from Wi-Fi.
* What the heartbeat does cover, it covers: 12–14 beats per run, each after 25–43 ms of
  source silence, none of which reached the receiver as a stall. The still-screen case
  (`captured 0` for 20 s, above) is the one it was built for and is verified by the
  pipeline test `heartbeats_keep_a_quiet_source_from_reading_as_a_stall` rather than here.

Not looped: two runs, back to back; the numbers above are within the earlier control's
spread except the stall attribution, which the beat log settles.

```sh
# on mac-studio: private daemons, own data dir and port (same as the stall section)
export SLOPTY_DATA_DIR=/Volumes/Lacie/Workspace/oss/slopty-wt/escapes/target/e2e-data/stall
target/debug/slopty-ptyd --socket $SLOPTY_DATA_DIR/ptyd.sock &
RUST_LOG=info,slopty_host=trace SLOPTY_PORT=45560 target/debug/slopty-hostd --direct-only \
  --ptyd-socket $SLOPTY_DATA_DIR/ptyd.sock --ctl-socket $SLOPTY_DATA_DIR/hostd.sock > $SLOPTY_DATA_DIR/hostd6.log 2>&1 &
open -na Ghostty --args -e seq 1 300000000
SLOPTY_DATA_DIR=$SLOPTY_DATA_DIR/client SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug \
  target/debug/slopty bench screen --display 6 --seconds 20
grep -E 'heartbeat|rate decision|stream closed' $SLOPTY_DATA_DIR/hostd6.log   # beats, verdicts, ScreenStats { heartbeats }
```

## 2026-09-05 — start-up on a cold connection, loopback, debug build

`screen_start_up_over_iroh` (`apps/slopty-hostd/tests/e2e.rs`, gate `SLOPTY_SCREEN_E2E`):
one private ptyd + hostd (`--direct-only`, any free port) in a temp dir, then five samples,
each a fresh pairing ticket over hostd's control socket, a fresh endpoint and key, a cold QUIC
connection, and the first display opened at the app's default quality (native scale, 60 fps,
30 Mbit/s ceiling) for 3 s. The client stamps `Open` sent → first datagram → first frame
complete → first decoded picture; the host logs `capture started` (enumeration and encoder
build inside), `keyframe encoded` (bytes) and, from the datagram pump, every stretch in which
QUIC's send buffer was not empty (`held_ms`, `max_bytes`, `cwnd`). mac-studio, the desktop
mostly still (the gap columns are the desktop, not the transport). The machine was shared with
another agent's builds during the later runs; the start-up columns did not move with that,
the stall and NACK columns did.

Before (main at df59235; the first sample is the first stream in both processes):

| sample | opened | first datagram | first frame | first decoded | hold max | stalls (stalled) | nack / refresh / lost | host target |
| ------ | ------ | -------------- | ----------- | ------------- | -------- | ---------------- | --------------------- | ----------- |
| 0      | 353 ms | 353 ms         | 358 ms      | **526 ms**    | 2 ms     | 1 (151 ms)       | 0 / 0 / 0             | 12.0 hold → 13.5 … 19.2 grow |
| 1      | 176 ms | 176 ms         | 176 ms      | 184 ms        | 12 ms    | 0                | **6** / 0 / 0         | 13.5 … 24.3 grow |
| 2–4    | 182–191 ms | 182–191 ms | 182–191 ms  | 190–200 ms    | 0 ms     | 0                | 0 / 0 / 0             | 13.5 … 24.3 grow |

What the logs said: `decoder session configured ms=150` on the first stream in the client
process (3 ms on every later one), inside the stream worker; the reassembler then charged
the 150 ms it had not read datagrams for as a link stall (`stalled 145–151 ms, stalls 1`,
verdict `Stall` for the first window — the "QUIC send-side hold" the heartbeat section above
had blamed). hostd's first `capture started` took 351 ms (`enumerate 60`), later ones 175–190
(`enumerate 62–75`). Sample 1's six NACKs were frame tails held by a 5808-byte congestion
window (BBR3's `ProbeRTT` floor) whose ACK waited on the receiver's 25 ms `max_ack_delay`;
the pump saw every frame end with ~1.5 KB held for 5–7 ms and one 68 KB frame held 105 ms at
a 38 KB window.

After (this branch: decoder warm-up at launch, encoder + capture warm-up when hostd comes
online, 2 s enumeration cache, arrival stamps and backlog drain on the client, 2 ms
`max_ack_delay`, 32-packet initial window, widest-sample cwnd cap; one run per row, five
samples each):

| run                              | opened      | first datagram | first frame  | first decoded | hold max | stalls (stalled)   | nack / refresh / lost | holds ≥ 5 ms (host) |
| -------------------------------- | ----------- | -------------- | ------------ | ------------- | -------- | ------------------ | --------------------- | ------------------- |
| BBR3, machine quiet, sample 0    | 295 ms      | 290 ms         | 295 ms       | 319 ms        | 1 ms     | 0                  | 0 / 0 / 0             | 0                   |
| BBR3, machine quiet, samples 1–4 | 115–121 ms  | 106–114 ms     | 115–121 ms   | 123–129 ms    | 1–2 ms   | 1 of 4 (59 ms)     | 0 / 0 / 0             | 0                   |
| Cubic (`SLOPTY_CC=cubic`), quiet | 112–298 ms  | 100–299 ms     | 112–305 ms   | 120–329 ms    | 2–3 ms   | 0                  | 0 / 0 / 0             | 0                   |
| + encoder warm-up, machine loaded, sample 0 | 129 ms | 134 ms   | 154 ms       | 184 ms        | 3 ms     | 1 (139 ms)         | 3 / 0 / 0             | 0                   |
| same run, samples 1–4            | 127–138 ms  | 123–131 ms     | 127–138 ms   | 140–149 ms    | 3–42 ms  | 0–2 (0–186 ms)     | 0–5 / 0 / 0           | 0                   |

* First picture on the first stream in both processes: 526 → 319 ms with the decoder warm-up
  alone, → 184 ms once hostd also warms the encoder (the first `Encoder::new` in a process is
  ~170 ms; the capture-only warm-up did not move `opened` — 270–295 ms with it — the encoder
  was the cold part). Later streams: 184–200 → 123–149 ms.
* `opened` 176–191 → 115–138 ms: the enumeration the client had just done for its listing is
  reused (`enumerate_ms=0`).
* With ACKs within 2 ms the host never held a datagram for 5 ms or more in any run (0 of ~350
  hold episodes per run, all ≤ 3 ms), and the quiet-machine runs had no NACK and no stall
  under either controller. The stalls and NACKs in the loaded runs did not coincide with a
  QUIC hold; the warm-ups took 630–680 ms in those runs instead of 170–190, i.e. the
  processes were not being scheduled, and the arrival stamps put the silence at the client's
  reader. **Settled on a quiet machine** (rerun below, `frame pacing` branch): 5 samples of
  3 s, nothing else building, **0 stalls, 0 NACKs, 0 refreshes, 0 lost frames** across every
  sample. The loaded runs' stalls and NACKs were the scheduler, as the arrival stamps said;
  nothing on the transport side is owed a change.

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_E2E_SAMPLES=5 RUST_LOG=info,slopty_host=debug,slopty_hostd=debug,slopty_codec=debug,slopty_client=debug \
  cargo nextest run -p slopty-hostd --no-capture screen_start_up 2>&1 | tee /tmp/startup.log
grep -E '^\| [0-9]' /tmp/startup.log                         # the table
grep -E 'capture started|keyframe encoded|decoder session|warmed up' /tmp/startup.log
grep -o 'held_ms=[0-9]* max_bytes=[0-9]*' /tmp/startup.log   # the pump's hold episodes
SLOPTY_CC=cubic … / SLOPTY_QUIC_IW=10 …                     # controller / initial window overrides
```

## 2026-09-05 — start-up over the mesh (Wi-Fi MacBook Pro → Mac Studio), debug build

Same private hostd on mac-studio (port 45560, own data dir under `target/e2e-data/startup`),
`slopty bench screen --display 6 --seconds 20` from macbook-pro over the WireGuard mesh, a
Ghostty window scrolling `seq` on the display. The link was in its worst state yet: idle
`slopty ping` 7.7 / 9.2 / 32 ms before the first run, 24 / 41 / 107 ms before the third; the
host's QUIC path lost 640–1571 packets per 20 s (25–30 % of what it sent) with 248–1205
congestion events and the window pinned at its 4800-byte floor. Every run cut to the 1 Mbit/s
floor within 5 s. The numbers describe that link; no start-up before/after can be read from
them and none is claimed. Two runs per build.

| build                         | first datagram / frame / decoded | keyframe (host) | keyframe spread | fps        | stalls (stalled)      | nack / refresh / lost | host QUIC lost / held at close |
| ----------------------------- | -------------------------------- | --------------- | --------------- | ---------- | --------------------- | --------------------- | ------------------------------ |
| IW 32 (this branch)           | 498 / 534 / 580 ms, 213 / 370 / 404 ms | 58.5 KB   | 72 / 88 ms      | 28.6 / 17.4 | 39 (5.0 s) / 52 (5.8 s) | 490/104/97, 357/115/90 | 1314 pkts / 2.6 KB, — |
| IW 10 (`SLOPTY_QUIC_IW=10`)   | 376 / 449 / 493 ms, 211 / 244 / 277 ms | 58.8 KB   | 75 / 57 ms      | 4.9 / 9.9  | 85 (9.4 s) / 56 (6.1 s) | 341/145/105, 475/194/155 | 640 pkts / 2.0 MB, 1016 pkts / 1.3 MB |
| IW 32 + held-frame drop       | 507 / 552 / 585 ms, 329 / – / –        | —         | 38 ms / —       | 4.5 / 0    | 88 (10.3 s) / 31 (4.1 s) | 334/171/155, 851/444/428 | 405 pkts / 1.1 MB, 1571 pkts / 4.2 MB |

What the runs did show, and what changed because of them:

* **QUIC held frames for seconds.** With the window at 4800 bytes the pump logged
  `held_ms=4149 max_bytes=365258` and `held_ms=7120 max_bytes=275586`: 4–7 s of frames sat
  in the 4 MiB datagram send buffer and were delivered stale, which the receiver counted as
  one stall after another. The pump now publishes the held byte count (`DatagramBudget::held`)
  and the capture callback drops a frame instead of encoding it while more than two frames'
  worth at the current target waits there (`frame_fits`, 32 KB floor; the datagram queue's
  low-water rule still applies). On this link that dropped 1016 of 1149 captured frames and
  delivered the rest fresh, against every frame delivered late before.
* **NACK answers stacked up.** The second drop run: 12 frames encoded, 64 221 datagrams sent
  — the receiver's 851 NACKs were each answered with a retransmit into a buffer that was
  already full (`space 9890` of 4 MiB). A NACK is now answered only when `frame_fits`; the
  stale answers were never going to arrive in time.
* **Initial window:** the 58 KB keyframe's spread was 57–88 ms under both windows, inside the
  noise of a path losing a quarter of its packets. The 32-packet ruling stands on arithmetic
  (one round trip fewer for a keyframe up to ~40 KB, two up to ~110 KB); the mesh
  measurement of it is still owed a day with a working link.

```sh
# mac-studio: private daemons (this worktree's target/e2e-data/startup)
export SLOPTY_DATA_DIR=$PWD/target/e2e-data/startup
target/debug/slopty-ptyd --socket $SLOPTY_DATA_DIR/ptyd.sock &
RUST_LOG=info,slopty_host=debug,slopty_hostd=debug SLOPTY_PORT=45560 target/debug/slopty-hostd --direct-only \
  --ptyd-socket $SLOPTY_DATA_DIR/ptyd.sock --ctl-socket $SLOPTY_DATA_DIR/hostd.sock --data-dir $SLOPTY_DATA_DIR/data --port 45560 > $SLOPTY_DATA_DIR/hostd.log 2>&1 &
SLOPTY_HOSTD_SOCKET=$SLOPTY_DATA_DIR/hostd.sock target/debug/slopty host ticket
open -na Ghostty --args -e sh -c 'seq 1 400000000'          # motion on display 6
gzip -1 -c target/debug/slopty | ssh macbook-pro 'mkdir -p /tmp/slopty-bench/startup && gunzip -c > /tmp/slopty-bench/startup/slopty && chmod +x /tmp/slopty-bench/startup/slopty'
# macbook-pro
export SLOPTY_DATA_DIR=/tmp/slopty-bench/startup/data SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug
/tmp/slopty-bench/startup/slopty pair <ticket>
/tmp/slopty-bench/startup/slopty bench screen --display 6 --seconds 20   # "after Open: …", "keyframe spread", stalls, trajectory
# host side: grep -E 'keyframe encoded|held_ms=[0-9]{3,}|screen closing|stream closed' $SLOPTY_DATA_DIR/hostd.log
```

## 2026-09-05 — gate wall time after the speed-up (mac-studio, 10 cores, warm caches)

`cargo xtask gate > /tmp/gate.log 2>&1` on a tree touching xtask, slopty-e2e and docs; every step
prints its own wall time since this change. Before it, the same gate ran ~8–9 min with the three
clippy passes in sequence (32 + 25 + 26 s) and deny/shear/typos/taplo/committed after the cargo
steps.

| step                          | time     | note                                                         |
| ----------------------------- | -------- | ------------------------------------------------------------ |
| fmt + taplo                   | 0.4 s    |                                                              |
| deny, shear, typos, taplo, committed | 4 s | one thread beside the cargo steps; output printed whole  |
| clippy aarch64-apple-darwin   | 54.4 s   | `--all-targets`                                              |
| clippy ios + ios-sim          | 43.9 s   | one invocation, two `--target`s, lib + bins                  |
| nextest                       | 148.6 s  | 245 tests run in 8 s; the rest is building the test binaries |
| doctests                      | 9.7 s    |                                                              |
| rustdoc                       | 25.3 s   |                                                              |
| **total**                     | **282 s** | budget 5–10 min |

nextest's build is the next target if the gate grows: it recompiles the workspace in the `test`
profile after clippy checked it in `dev`, and sccache does not cache incremental workspace crates
(second look 2026-09-06: check is not build; clippy produces rmeta only, so test codegen cannot
be shared; no tested variant beats the baseline by ≥ 10 %, see below).

## 2026-09-05 — start-up over iroh on a quiet machine, and arrival → present (mac-studio, debug)

The rerun the previous section was owed: same `screen_start_up_over_iroh`, same loopback iroh
path, but nothing else on the machine (`pgrep -fl 'cargo|rustc'` = 2, `top` 71 % idle, no
sibling build). Each sample is a fresh QUIC connection to the same private hostd, native scale
at the default quality, streaming display 1 of an otherwise idle desktop. Two runs: 5 × 3 s
(like-for-like with the "machine quiet" rows above) and 8 × 5 s (more frames, for the
presentation numbers).

### Start-up, 5 samples × 3 s

| sample | opened | first datagram | first frame | first decoded | hold max | decoded | stalls (stalled) | nack / refresh / lost |
| ------ | ------ | -------------- | ----------- | ------------- | -------- | ------- | ---------------- | --------------------- |
| 0      | 130 ms | 131 ms         | 146 ms      | 170 ms        | 1 ms     | 39      | 0 (0 ms)         | 0 / 0 / 0             |
| 1      | 132 ms | 121 ms         | 132 ms      | 141 ms        | 3 ms     | 78      | 0 (0 ms)         | 0 / 0 / 0             |
| 2      | 127 ms | 113 ms         | 127 ms      | 136 ms        | 1 ms     | 83      | 0 (0 ms)         | 0 / 0 / 0             |
| 3      | 123 ms | 113 ms         | 123 ms      | 132 ms        | 2 ms     | 84      | 0 (0 ms)         | 0 / 0 / 0             |
| 4      | 123 ms | 114 ms         | 123 ms      | 133 ms        | 1 ms     | 83      | 0 (0 ms)         | 0 / 0 / 0             |

The 8 × 5 s run agreed on shape and sat 15–30 ms higher throughout (opened 116–160 ms, first
decoded 151–170 ms), all of it in host-side `capture started total_ms` (141–159 vs ~115); it
also picked up one NACK in one sample and a 57–86 ms stall in three. Run-to-run spread on a
shared desktop, not a trend: the client-side stamps put no silence at the reader in either run.

* **Zero stalls, zero NACKs, zero refreshes, zero lost frames** in every sample of the 3 s run.
  That is what the previous section was waiting for, and it retires the "machine loaded by
  another agent's builds" caveat: the stalls and NACKs there were the scheduler, not the link.
* `opened` 123–132 ms and first decoded 132–141 ms (sample 0: 170 ms, its first keyframe encode
  is 61 ms against 26–27 ms later) — within a dozen milliseconds of the 115–121 / 123–129 ms
  measured on the previous branch, i.e. unchanged by this branch's client-side work.
* Warm-ups in this run: decoder 456 ms, capture 410 ms, both finished long before the first
  `Open`; `decoder session configured ms=3–4` on every stream, `enumerate_ms=0` on every one.

### Arrival → present

The same runs, driving the app's real presentation path: the element's `slopty_client::Pacer`,
offered every frame off the same `watch` channel the GPUI element reads, asked to present on a
60 Hz tokio timer standing in for the display link. Arrival is the datagram that completed the
frame; present is the paint. A timer is not a display link, so the *interval* jitter carries the
timer's own and the source's — a still desktop is not a steady 60 fps source, it delivered
17–24 fps here — but arrival → present does not depend on the timer's regularity.

| run       | samples | arrival → present p50 | p95         | max         | decode p50 | present every | skip | repeat | late |
| --------- | ------- | --------------------- | ----------- | ----------- | ---------- | ------------- | ---- | ------ | ---- |
| 5 × 3 s   | 1–4     | 9.0–12.4 ms           | 17.5–19.2 ms | 17.9–21.2 ms | 2.6–2.7 ms | 17.2–17.7 ms  | 3–4  | 100–105 | 0    |
| 8 × 5 s   | 0–7     | 9.0–12.6 ms           | 17.1–20.4 ms | 18.7–63.5 ms | 2.7–2.9 ms | 17.5–18.9 ms  | 3–11 | 184–213 | 0    |

* **No extra frame of buffering.** A paint interval is 16.7 ms and the decoder takes 2.7 ms, so
  present-on-arrival predicts p50 ≈ decode + half an interval ≈ 11 ms and p95 ≈ decode + a whole
  interval ≈ 19 ms. Measured: p50 9.0–12.6, p95 17.1–20.4. A single frame of playout buffering
  would have added 16.7 ms to both; there is no room for it in these numbers.
* **`late` is 0 everywhere**: nothing arrives out of order behind `AllowFrameReordering=false`,
  so the ordering guard costs nothing and catches nothing on this path.
* `skip` 3–11 per sample: bursts where two frames left the decoder inside one 16.7 ms tick and
  the older was replaced rather than queued. `repeat` 100–213 is the timer ticking with no new
  frame — expected at 17–24 source fps against a 60 Hz paint, and the reason `repeat` alone is
  not a fault signal. The one `max` outlier (63.5 ms, 8 × 5 s sample 7) is the sample that also
  reported a stall; every other sample's worst frame is inside p95 + 3 ms.
* `interval_jitter` is ±21–54 ms and says nothing about this branch: it is the *source's*
  cadence, a desktop that only produces a frame when something changes. It is in the overlay
  because on a steady source it is the first place a double- or skipped-present shows up.

```sh
# quiet check first: nothing else building
pgrep -fl "cargo|rustc" | wc -l && top -l 2 -n 0 -s 1 | grep "CPU usage" | tail -1
SLOPTY_SCREEN_E2E=1 SLOPTY_E2E_SAMPLES=5 SLOPTY_E2E_SECONDS=3 \
  RUST_LOG=info,slopty_host=debug,slopty_hostd=debug,slopty_codec=debug,slopty_client=debug \
  cargo nextest run -p slopty-hostd --no-capture screen_start_up > /tmp/startup-quiet.log 2>&1
grep -E '^\| ' /tmp/startup-quiet.log        # both tables: start-up, then arrival → present
grep -E 'capture started|keyframe encoded|decoder session|warmed up' /tmp/startup-quiet.log
```

## 2026-09-05 — parity, NACK and refresh under injected loss (loopback), debug build

Command (both tables from the same test; the "before" run is the same test with
`crates/slopty-media/src/redundancy.rs` reverted to its previous controller):

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data \
  cargo nextest run -p slopty-hostd --test e2e screen_under_injected_loss --no-capture
grep -E '^\| ' /tmp/loss-after.log
```

Setup: mac-studio, `screen_under_injected_loss` in `apps/slopty-hostd/tests/e2e.rs` — hostd and
client in one process over iroh's direct path, main display 1920×1080, 5 s per rate, four rates
back to back. Loss is injected on the client's receive path (`ScreenRouter::set_loss`) from a
fixed seed, so a rate always drops the same datagrams of the sequence and two builds compare on
the same losses. "Parity seen" is parity fragments per thousand data fragments *on the wire*, not
the controller's ratio: the packetizer always sends at least one parity fragment, which on a
mostly-still desktop (4–8 fragments per frame) dominates whatever ratio is asked for.

Before — previous controller (symmetric quarter-weight EWMA, no deadband, stalls counted as
loss):

| drop | frames | by parity | by NACK | lost | NACK / refresh | datagrams (lost) | parity seen | gap p50 / p90 / max |
| ---- | ------ | --------- | ------- | ---- | -------------- | ---------------- | ----------- | ------------------- |
| 0 ‰   | 89    | 0         | 0       | 0    | 0 / 0          | 521 (0)          | 310 ‰       | 61.9 / 96.4 / 136.1 ms |
| 20 ‰  | 129   | 9         | 0       | 0    | 0 / 0          | 608 (11)         | 381 ‰       | 40.3 / 69.0 / 101.2 ms |
| 50 ‰  | 133   | 16        | 1       | 0    | 1 / 0          | 603 (18)         | 376 ‰       | 25.1 / 72.5 / 95.8 ms  |
| 100 ‰ | 133   | 28        | 4       | 0    | 4 / 0          | 579 (37)         | 380 ‰       | 39.3 / 68.9 / 107.6 ms |

After — asymmetric controller (rise ½ / fall ⅛, 2 % deadband, stalled windows excluded), same
NACK and refresh policy:

| drop | frames | by parity | by NACK | lost | NACK / refresh | datagrams (lost) | parity seen | gap p50 / p90 / max |
| ---- | ------ | --------- | ------- | ---- | -------------- | ---------------- | ----------- | ------------------- |
| 0 ‰   | 87    | 0         | 0       | 0    | 0 / 0          | 519 (0)          | 331 ‰       | 70.6 / 86.1 / 106.4 ms |
| 20 ‰  | 120   | 9         | 0       | 0    | 0 / 0          | 579 (11)         | 378 ‰       | 19.6 / 83.4 / 112.6 ms |
| 50 ‰  | 120   | 13        | 2       | 0    | 2 / 0          | 563 (15)         | 373 ‰       | 19.4 / 83.6 / 100.0 ms |
| 100 ‰ | 117   | 23        | 3       | 0    | 3 / 0          | 540 (31)         | 385 ‰       | 25.3 / 83.8 / 98.2 ms  |

Both tables above were taken while another session was building (~94 % CPU, 38 cargo/rustc
processes), which is why the frame counts are ~17 fps: they compare two builds under the same
load, not the machine's best. Re-run of the after build on a quiet machine (5 processes), on
protocol 13 after the rebase, with the decoded and stall columns the test grew since:

| drop | frames (decoded) | by parity | by NACK | lost | NACK / refresh | datagrams (lost) | parity seen | stalls | gap p50 / p90 / max |
| ---- | ---------------- | --------- | ------- | ---- | -------------- | ---------------- | ----------- | ------ | ------------------- |
| 0 ‰   | 271 (273) | 0  | 0  | 0 | 1 / 0  | 992 (0)  | 431 ‰ | 3 | 15.7 / 46.3 / 102.8 ms |
| 20 ‰  | 270 (269) | 12 | 0  | 0 | 1 / 0  | 949 (13) | 422 ‰ | 2 | 13.8 / 49.9 / 105.6 ms |
| 50 ‰  | 274 (273) | 29 | 6  | 0 | 7 / 0  | 977 (32) | 435 ‰ | 0 | 14.1 / 45.5 / 99.3 ms  |
| 100 ‰ | 269 (273) | 45 | 12 | 0 | 15 / 0 | 885 (49) | 422 ‰ | 2 | 15.3 / 39.5 / 84.4 ms  |

At ~54 fps the picture is the same one three times over: nothing lost, nothing refreshed, parity
carrying most of the repairs and NACK the rest (12 at 100 ‰, up from 3 at a third of the frame
rate). Decoded can exceed frames by one because the two counters are read a moment apart. Two or
three windows still register as stalls even on a quiet machine — the capture's own 100 ms gaps —
so the test skipped its per-rate verdicts; the frame, decode and decode-error assertions ran.

Takeaways:

* **Nothing is lost and nothing is refreshed up to 100 ‰ datagram loss on this path**, before or
  after, loaded or quiet: parity repairs 8–24 % of the frames, a handful of NACKs cover the rest,
  and the arrival gap is the desktop's, not the link's (the 0 ‰ row's 70 ms p50 is a still screen
  on a busy machine, 16 ms on a quiet one). The NACK
  give-up deadline is therefore left alone — there are no refreshes to remove at 20–50 ‰, and
  changing a deadline that no measurement moves would be churn. What this path cannot show is
  the deadline's RTT-dependent half: loopback answers a NACK in ~1 ms, so a give-up needs a
  stall, which is what the mesh sections above measure.
* **The two controllers are indistinguishable here, and that is a property of the source, not
  of the controllers**: a still desktop at ~5 Mbit/s encodes 4–8 fragments per frame, so the
  packetizer's one-parity-fragment minimum (310–435 ‰ observed) is always above the ratio either
  controller asks for (50–200 ‰). The controller only binds on frames of tens of fragments —
  a busy screen on a lossy path — which this test cannot produce without driving the desktop.
  Its behaviour is pinned by unit tests (`crates/slopty-media/src/redundancy.rs`: where the
  ratio settles, how many times it re-cuts the layout, rise/fall asymmetry, a stalled window,
  degenerate reports) instead.
* Frame counts differ between rows because the desktop drew different amounts, not because of
  the loss; only the recovery columns compare across rows.

Not measured here: the loss path over the mesh with the new controller (needs a second machine
and a busy screen) (refresh storm guard end-to-end verified 2026-09-06; see "The refresh guard end to end" below).

## 2026-09-06 — stall attribution, parity overhead and a release row (loopback)

Commands (same test, both profiles; the machine has to be idle — `pgrep -fl "cargo|rustc"`):

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data \
  cargo nextest run -p slopty-hostd --test e2e screen_under_injected_loss --no-capture
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data \
  cargo nextest run --release -p slopty-hostd --test e2e screen_under_injected_loss --no-capture
```

Setup as in the section above: hostd and client in one process over iroh's direct path, main
display, 5 s per rate, loss injected on the client's receive path from a fixed seed. Two columns
are new. *Fragments/frame* is `data_shards / frames`, and *one-per-frame would be* is
`1000 × frames / data_shards` — what the packetizer's one-parity-fragment minimum costs for
exactly the frames this run saw, which is the only fair comparison when the capture target is the
machine's own display and its content is not under the test's control.

Release, a still desktop (the four rows are one run, verdicts asserted):

| drop | frames | by parity | by NACK | lost | NACK / refresh | datagrams (lost) | kB (B/frame) | fragments/frame | parity seen (one-per-frame) | stalls | gap p50 / p90 / max |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 0 ‰   | 96 | 0  | 0 | 0 | 0 / 0 | 598 (0)  | 494 (5149) | 3 | 295 ‰ (258 ‰) | 0 | 53.9 / 94.0 / 118.4 ms |
| 20 ‰  | 88 | 4  | 0 | 0 | 0 / 0 | 514 (6)  | 397 (4521) | 3 | 350 ‰ (296 ‰) | 0 | 69.6 / 94.2 / 112.6 ms |
| 50 ‰  | 95 | 20 | 1 | 0 | 1 / 0 | 568 (22) | 474 (4996) | 3 | 327 ‰ (261 ‰) | 0 | 67.0 / 84.2 / 95.6 ms  |
| 100 ‰ | 73 | 12 | 1 | 0 | 1 / 0 | 465 (24) | 359 (4918) | 3 | 343 ‰ (258 ‰) | 0 | 82.8 / 97.6 / 119.7 ms |

Debug, a busy desktop (58 fps, 17 ms between frames — the same test minutes earlier, while a
build was drawing to a terminal on the captured display):

| drop | frames | by parity | by NACK | lost | NACK / refresh | datagrams (lost) | kB (B/frame) | fragments/frame | parity seen (one-per-frame) | stalls | gap p50 / p90 / max |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 0 ‰   | 288 | 0  | 0 | 0 | 0 / 0 | 1000 (0) | 834 (2896) | 2 | 432 ‰ (413 ‰) | 0 | 17.3 / 18.9 / 41.0 ms |
| 20 ‰  | 288 | 18 | 0 | 0 | 0 / 0 | 977 (20) | 836 (2904) | 2 | 436 ‰ (414 ‰) | 0 | 17.3 / 18.2 / 54.3 ms |
| 50 ‰  | 285 | 25 | 3 | 1 | 4 / 1 | 948 (28) | 805 (2826) | 2 | 436 ‰ (410 ‰) | 0 | 17.3 / 19.9 / 49.0 ms |
| 100 ‰ | 285 | 58 | 7 | 2 | 9 / 2 | 910 (69) | 773 (2713) | 2 | 441 ‰ (408 ‰) | 0 | 17.2 / 19.1 / 54.0 ms |

Takeaways:

* **Stalls: 3 / 2 / 0 / 2 before, 0 / 0 / 0 / 0 after**, on an idle machine at the same four
  rates (the "before" is the table above this one, taken with the same test the night before).
  Every one of them was the receiver reading its own capture's quiet gap as a held link. Now the
  gap is compared with the host's send stamps and only the link's share is charged. The gated
  test still skips its per-rate verdicts on a run that stalled — on loopback that is the
  scheduler holding datagrams — but the skip fires on the rare run rather than on all of them.
* **A release build is 3× the frame rate of a debug one and changes no verdict.** Nothing is lost
  or refreshed at any rate in release; the numbers that move are throughput (release reached
  ~19 fps on a still desktop against debug's ~8 on the same content) and the arrival gap. Loss
  recovery is not CPU-bound at these rates: the debug run repairs the same fraction of frames.
* **The one-parity-fragment minimum costs 240–440 ‰ of the data fragments on a still window and
  ~0 on a busy one**, because a still window's frames are two to five fragments and a busy one's
  are tens. Removing it is measured and rejected in DECISIONS: parity fell to ~50 ‰ but the run
  lost 2 frames and took 2 refreshes at 20 ‰ where the minimum loses none. The absolute cost is
  small where the ratio is large — a still window streams ~0.5 Mbit/s, so 30 % of it is
  ~150 kbit/s.
* Frames beyond parity's reach are not zero at 50–100 ‰ and cannot be: a two-fragment frame plus
  one parity is gone when two of its three datagrams are, which at 50 ‰ is about one frame in
  three hundred. The gated test's verdict is a rate (under one frame in a hundred), not zero.
* Rows are only comparable within a run. The capture target is the machine's own display, so a
  row's frame count and frame size are whatever the desktop drew during those 5 s; the
  *one-per-frame* column exists so the parity comparison survives that.

A scrolling terminal as the capture target is measured in the app self-test now (the app drives a
flooding shell and captures the display it fills; see "a scrolling terminal under injected loss"
below). Still not measured here: the loss path over the mesh, and group parity shared across
consecutive small frames (not implemented — the minimum measured well enough not to need a wire
change).

## 2026-09-06 — the apply path under a 20-shell flood (mac-studio, e2e app build)

Applying host frames was 31 % of the main thread in the table below, 16 % of it
`Vec<Cell>::clone`: the client copied every row it received so the same line could sit in both
the screen and the scrollback. It no longer copies — both hold `Arc<Line>` and applying a row
makes one allocation (DECISIONS, Canvas → performance).

```
cargo build -p slopty --bin slopty-app          # the harness launches this binary; nextest does not rebuild it
SLOPTY_SMOOTH_E2E=1 SLOPTY_SMOOTH_SHELLS=20 cargo nextest run -p slopty-e2e --test smooth \
  --test-threads 1 --no-capture -E 'test(twenty_streaming)'
sample $(pgrep -n -x slopty-app) 10 -file /tmp/sample-<tag>.raw    # 4 s after the run starts
```

Share of main-thread samples, paired runs on a quiet machine (`pgrep -f rustc | wc -l` ≤ 6,
`/tmp/sample-before10.raw`, `/tmp/sample-after10.raw`; each 10 s, ~5 000 samples):

| stage | draw | shaping | applying host frames | of which `Vec<Cell>::clone` | of which scrollback |
| --- | --- | --- | --- | --- | --- |
| before (line copied into both) | 20.3 % | 4.2 % | **14.8 %** | 6.8 % | 5.9 % |
| after (`Arc<Line>` shared) | 10.4 % | 1.7 % | **4.2 %** | 0.0 % | 3.8 % |

Only the apply column is attributable: draw and shaping move with the phase the sample lands in
and are quoted for context, not as a result. Frame times over the same runs
(`draw p50 / p95 / p99 / max ms · frames, over 16.7 ms`) are noise-dominated at this machine's
load — pan before 2.4 / 13.1 / 25.5 / 82.9 · 233, 5 over; after 2.3 / 10.0 / 21.9 / 272.6 · 271,
4 over — six alternating runs of each build overlapped completely, so the sample is the evidence
and the frame numbers are not.

**The scrollback container was measured and left alone.** A `VecDeque` ring over
`[base, base + capacity)` with holes replaced the `BTreeMap` in a working build; two more paired
samples gave apply 4.2 % / 6.2 % (map) against 7.3 % / 5.0 % (ring) — the same number twice over.
The profile says why: what `insert_shared` spends its time on is `Arc::drop_slow` freeing the
line that fell out of the cache, which both containers pay identically. The "B-tree churn" of the
2026-09-05 note was that same free, seen under the `pop_first` frame it happened in.

One copy nobody had measured went with the same commit: `Scrollback::set_extent` ran `split_off`
on **every frame**, rebuilding the map whether or not the host had dropped a line. It now splits
only when `oldest` advances.

## 2026-09-05 — canvas frame time under streaming load (mac-studio, e2e app build)

The app times its own draws (`slopty_ui::frames`: `begin` at the top of `Workspace::render`,
`end` in the last-painted element; ring of 1024, nearest-rank percentiles) and the self-test
reads them back (`dump.frames`). Scenarios in `crates/slopty-e2e/tests/smooth.rs`
(`cargo xtask e2e smooth`), 5 s each, window 1280×800, N shells running
`while :; do printf '%06d the quick brown fox … %06x\n' …; done` (every row new every frame,
the worst case for a content-keyed cache): **(a)** pan at zoom 1, 120 scroll events/s;
**(b)** ⌘-scroll zoom fit → 200 % → fit, 120 steps/s; **(c)** the first display streaming
beside 5 shells, pan; **(d)** 60 letters typed at 15/s into the focused shell.

How to read the harness: the `e2e` feature is GPUI `test-support`, under which a dirty window
is drawn synchronously in `flush_effects` — a "frame" is an update, not a vsync, so `every`
(the interval between draws) is command cadence, not display cadence, and the product draws
at most once per display period. Draw percentiles are the numbers that transfer. Debug
profile (`opt-level = 1`, deps 3). The machine ran other sessions' builds throughout
(`pgrep -fl "cargo|rustc" | wc -l` was 8–173 between runs), which is where the max columns come
from; p50/p95 moved little between quiet and busy runs.

### Baseline (main 9d01682 + the probe)

Scenarios (a) and (b) did not complete: the host sent a frame every 2 ms per flooding session
(500/s × 20), the per-client sink (256) overran and hostd detached the client 19 times in the
first minute ("client cannot keep up; detaching"); the app answered no `dump` for 30 s. With the
first two fixes in (host 8 ms pace, link batching) but the link still applying per event, the
harness drew 13 010 frames in the 5 s zoom (every event a draw), p95 15.4 ms, max 305 ms.

### Draw time per scenario, 20 shells, by fix (ms; p50 / p95 / p99 / max · frames over 16.7 ms)

| state | (a) pan | (b) zoom | log |
| --- | --- | --- | --- |
| link paced to one update per frame, input at 120 Hz | 3.2 / 72.2 / 132.1 / 152.9 · 25 | 0.9 / 95.4 / 196.5 / 948.9 · 14 | /tmp/e2e-smooth-paced.log |
| + words shaped once (row split at spaces, cache swept once per frame) | 1.9 / 34.9 / 57.5 / 78.9 · 26 | 2.9 / 102.1 / 163.2 / 1146 · 21 | /tmp/e2e-smooth-words.log |
| + digits shaped alone, cell text a copy | **1.1 / 3.9 / 9.7 / 72.7 · 2** | **1.1 / 13.5 / 51.7 / 348.8 · 17** | /tmp/e2e-smooth-digits.log |

Fewer shells, final state: 5 shells (a) 1.1 / 4.0 / 7.1 / 33.1 · 2, (b) 1.8 / 4.9 / 7.2 / 11.2 · 0;
10 shells (a) 1.1 / 4.0 / 8.3 / 31.4 · 1, (b) 1.2 / 5.0 / 10.5 / 126.8 · 2. Before the word cache
the same 5-shell pan was 2.3 / 10.3 / 20.0 / 35.0 and 10 shells 2.1 / 10.9 / 47.3 / 81.8.

What the main thread was doing (`sample <pid> 4`, 20 shells, share of main-thread samples;
`/tmp/sample-app20.raw`, `/tmp/sample-app20b.raw`):

| stage | draw | shaping (`shape_line`) | applying host frames | of which `Vec<Cell>::clone` | scrollback B-tree |
| --- | --- | --- | --- | --- | --- |
| paced link, row cache | 67 % | 48 % | 23 % | 11 % | 9 % |
| word cache | 61 % | 41 % | 31 % | 16 % | 9 % |

Whole-row shaping never hit under a flood (every row is new); words hit for the prose but the
two counters per row still cost a `shape_line` each, and CoreText's per-call cost dominates a
short string, so two calls per row were no cheaper than one. Digits shaped alone leave the
steady state with no shaping at all. The remaining (b) p99/max is the card → grid transition
at `CARD_ZOOM` (twenty grids appear in one frame, every word at a new size) and the machine.

### Keystroke → paint (scenario d, ms; p50 / p95 / max over 60 keys)

| build | `SLOPTY_PREDICT=never`: host echo | `always`: local echo | `always`: host echo |
| --- | --- | --- | --- |
| baseline | 7.4 / 14.3 / 17.1 | 0.8 / 1.6 / 1.9 | 7.6 / 12.7 / 28.3 |
| final (frames while typing: draw 0.4–0.5 ms p50, 0 over) | 6.2 / 6.9 / 8.5 | 0.6 / 1.9 / 2.8 | 6.5 / 16.4 / 19.8 |

The default policy is `Adaptive`, which draws predictions only once the measured RTT is over
`SLOW_LINK` (25 ms), so on loopback the default shows the host's echo (7–8 ms p50, one frame)
and a slow link gets the ~1 ms local echo. Nothing to fix: the local echo path is under 2 ms
and the host echo is a host round trip plus the next draw.

### Display beside five shells (scenario c)

`/tmp/e2e-smooth-final.log`, the first display captured at native scale and streamed over
loopback beside 5 flooding shells, pan at 120 events/s: draw 1.1 / 7.7 / 18.4 / 40.9 ms, 9 of
722 draws over 16.7 ms. The stream's frames mark the window dirty outside the link loop's
pacing, so the harness drew every 5.9 ms here (the product would still draw once per vsync).

### The iPad simulator (indicative only: software rendering, 60 Hz nominal set by the harness)

`cargo xtask e2e smooth-ios --sim ipad`, iPad Pro 13-inch simulator, 20 flooding shells, `SLOPTY_FRAME_HZ=60`
(`/tmp/e2e-smooth-ios.log`):

| scenario | draw p50 / p95 / p99 / max (ms) | over 16.7 ms |
| --- | --- | --- |
| (a) pan at zoom 1 | 1.0 / 1.3 / 1.5 / 20.5 | 1 of 566 |
| (b) zoom fit → 200 % → fit | 0.7 / 1.3 / 1.6 / 2.0 | 0 of 547 |
| (d) frames while typing | 1.0–1.5 / 2.3–2.5 / 2.6–3.5 / 4.1 | 0 |

Keystroke → paint on the simulator: `never` host echo 7.7 / 12.8 / 25.7 ms; `always` local echo 1.6 / 2.4 / 2.7 ms,
host echo 7.8 / 9.9 / 14.9 ms (p50 / p95 / max, 60 keys). The simulator's window is the device's points at 1× on
software rendering, so its draws are cheaper than the Mac window's and say nothing about a real iPad's 8.3 ms budget;
the row is here so a regression in the shared code shows up on both platforms.

```sh
pgrep -fl "cargo|rustc" | wc -l                         # machine noise, goes in the log
SLOPTY_SCREEN_E2E=1 cargo xtask e2e smooth > /tmp/e2e-smooth-final.log 2>&1   # (a) (b) (c) (d)
cargo xtask e2e smooth-ios --sim ipad > /tmp/e2e-smooth-ios.log 2>&1
grep MEASURE /tmp/e2e-smooth-final.log /tmp/e2e-smooth-ios.log
# one scenario at another shell count, e.g. 5:
SLOPTY_SMOOTH_E2E=1 SLOPTY_SMOOTH_SHELLS=5 cargo nextest run -p slopty-e2e --test smooth \
  --test-threads 1 --no-capture -E 'test(twenty_streaming)'
# where the main thread is while it runs:
sample $(pgrep -n -f target/debug/slopty-app) 4 -file /tmp/sample-app.raw
```

## 2026-09-06 — the path under load, and the refresh guard end to end

Two things this repo had been arguing about without evidence: whether a busy machine is what
pushed a connection onto a relay for 43 s (DECISIONS, 2026-09-04), and whether the refresh-storm
guard actually holds outside the media unit tests.

### The path under load

```sh
SLOPTY_FLAP_E2E=1 SLOPTY_DATA_DIR=target/e2e-data \
  cargo nextest run -p slopty-hostd --test e2e path_flap_under_cpu_load --no-capture
# … and path_flap_under_user_initiated_cpu_load, path_flap_under_memory_io_load
```

hostd and a client on this machine over iroh with **relays enabled**, so the connection holds a
relay path (`aps1-1.relay.n0.iroh.link`, rtt 55 ms) as well as the loopback direct one and has
somewhere to flap to. One shell and one display stream; the selected path, its rtt and noq's path
log sampled every 250 ms for 90 s while a load shape owns the machine. Run one at a time (each
saturates every core), mac-studio, debug build, 2026-09-06 00:45–00:50 local.

| shape                                              | ran           | to relay | rtt worst / last | paths closed | stalls | frames |
| -------------------------------------------------- | ------------- | -------- | ---------------- | ------------ | ------ | ------ |
| all-core spin, default QoS                         | 00:45:27–47:02 | none     | 8.4 / 2.8 ms     | none         | 34     | 889    |
| all-core spin, `QOS_CLASS_USER_INITIATED`          | 00:47:05–48:38 | none     | 6.3 / 2.2 ms     | none         | 19     | 1219   |
| memory + I/O (GB writes/reads, 2 000 small files)  | 00:48:38–50:16 | none     | 10.0 / 6.4 ms    | none         | 38     | 974    |

Idle rtt on the same direct path before each load: 1.4–1.9 ms. So the whole machine at full tilt
costs the path a few milliseconds and nothing else — no abandon, no relay sample, not one path
event in 90 s × 3. The load hypothesis is dead (DECISIONS, "Machine load alone does not flap the
path"); what is left untested is the network half, which needs the second machine.

The stalls looked like the interesting leftover — 19–38 datagram stalls per 90 s under every
shape — and were read here as the pacer clumping frames. That reading was wrong; see
"stalls are not made by load" below, where the same harness with **no load at all** produces just
as many. The loss table's 0 stalls came from 5 s windows, not from an idle machine being clean.

### The refresh guard end to end

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-hostd \
  --test e2e a_window_that_never_draws --no-capture
SLOPTY_SCREEN_E2E=1 cargo xtask e2e app   # the app case needs the grant too, and says so
```

The target is a window the test owns (`slopty-idle-window`, a 240×160 AppKit window with no
content): on screen while the host lists it, ordered out before the stream opens, so
ScreenCaptureKit has nothing at all to deliver. Nothing on the desktop is touched.

* Host → `SourceState::Idle` within its 400 ms grace, every time.
* Client → **4 refresh requests** over the following 3 s and then silence, against the
  `refresh_max_repeats` cap of 12 and the 79-in-10-s storm this guard was built for
  (2026-09-05, "screen stream over the mesh"). The count is read from the host's own
  `ScreenStats::refreshes`, i.e. what actually arrived, not what the client believes it sent.
* Item → the `Role::Status` placeholder reads "waiting for the window to draw…", not "waiting
  for the first frame…".
* Recovery → the helper orders the window back in and repaints; frames arrive with no refresh,
  no reopen and no help from the client, and the placeholder goes.

Seen while writing the test and worth its own look: once the window is visible and unobstructed
the host switches that stream to the **display-crop** path. (An earlier version of this paragraph
said that hiding the window then does not stop the stream. That was measured on a stale helper
binary whose window never hid; see the next section for what actually happens.)

The same guard with the phone as the viewer is measured now: the AppKit helper is on the host's
Mac (the simulator shares that machine), the phone captures it and the host counts what the phone
asks for. Over the relay the phone asked for **0 refreshes** (the host's idle hint, sent inside
its 400 ms grace, reached the phone before its first refresh interval — well inside the cap of
12); the host then flips the stream back to `Live` when the window draws again, without a reopen.
An idle source sends no frames, so this needs no video decode on the simulator, only the picker
and the refresh loop (`crates/slopty-e2e/tests/pair.rs`,
`the_refresh_guard_holds_with_the_phone_asking_with_the_simulator`,
`SLOPTY_SCREEN_E2E=1 cargo xtask e2e pair-ios --sim iphone`). Still not measured: a target that
draws once and then hides — `check_source` latches on `encoded > 0`, so that case reports `Live`
forever and only the cap protects the client (superseded 2026-09-06: `SourceTracker` follows
recent frames, reporting `Idle` after 2 s quiet or when off-screen; see DECISIONS, "The source
state follows the frames, not the first one").

## 2026-09-06 — font truth: the cell from the font's own tables (macOS app self-test, debug)

`cargo xtask e2e app` on the tree where `measure` fills the line gap and the `post` underline
from the fork's `TextSystem::font_metrics` (JetBrains Mono 13 pt, the e2e window at 2×).
`dump.terminals[0].face` in device pixels per em (26): ascent 1.020, descent −0.300, line
gap 0, underline −0.155 / 0.050 (asserted by `check_jetbrains_mono_face`, macOS and both
simulators). Goldens (`snapshot <name>: differing of total`):

| golden | differing / total | note |
| --- | --- | --- |
| note | 0 / 540000 | |
| terminal | 1540 / 540000 (0.285 %) | rows 122–188, x 33–270: the echoed prompt's glyphs, the same 1540 as the pre-track run (`/tmp/e2e-terminal.log`); no underline in the scene |
| conversation | 0 / 540000 | |
| conversation-permission | 0 / 540000 | |

No golden accepted: nothing in the scenes is underlined or struck through, the line gap is
zero, so the cell, baseline and glyph positions are the ones before. The change shows only
under an underline (one pixel lower, 1 device pixel thick at 1×, 2 at 2×; DECISIONS "The
font's own line gap and underline").

## 2026-09-06 — the zoom hitch: words shaped once at the base size, glyphs painted at the zoom

Scenarios (a) and (b) of `cargo xtask e2e smooth` (20 flooding shells; pan at zoom 1, and
⌘-scroll zoom fit → 200 % → fit at 120 steps/s; 5 s each) on three trees: **A** `ae91c0d`
(smooth's tree plus font truth), **B0** `4b9c619` (the element paints each word's base-size
glyph runs at the zoomed size, one `paint_glyph` per glyph, no layer) and **B** `b1b77b2` (the
same inside one `paint_layer`). Draw ms p50 / p95 / p99 / max · frames over 16.7 ms; `procs`
is `pgrep -fl "cargo|rustc" | wc -l` when the run started. Other sessions were building the
whole night (`/tmp/glyphs-ab-quiet.sh` waited for a quiet moment and alternated A, B, A, B; the
quiet moment lasted one run each time), so **every column past p50 is load**: the same tree
swings 4.1 → 36.0 ms p95 between runs. p50 is the column that transfers.

| run | tree | procs | (a) pan | (b) zoom |
| --- | --- | --- | --- | --- |
| A | ae91c0d | 24 | **1.2** / 4.4 / 12.2 / 50.8 · 3 | **1.1** / 5.3 / 27.7 / 223.4 · 5 |
| A1 | ae91c0d | 18 | 1.4 / 36.0 / 106.5 / 127.9 · 24 | 1.7 / 44.7 / 162.6 / 643.0 · 15 |
| A2 | ae91c0d | 0 | 1.2 / 4.1 / 11.1 / 52.2 · 3 | 1.2 / 8.0 / 35.0 / 335.2 · 13 |
| A3 | ae91c0d | 8 | 1.2 / 3.2 / 6.4 / 56.3 · 1 | 1.1 / 6.1 / 17.5 / 268.7 · 5 |
| A4 | ae91c0d | 5 | 1.3 / 9.4 / 38.6 / 60.6 · 16 | 1.4 / 16.4 / 43.5 / 376.8 · 15 |
| A5 | ae91c0d | 18 | 1.3 / 12.1 / 79.7 / 94.1 · 13 | 1.5 / 58.6 / 139.2 / 621.5 · 22 |
| B0 | 4b9c619 | 15 | **1.7** / 8.9 / 25.7 / 79.9 · 6 | 1.4 / 7.9 / 36.2 / 295.8 · 7 |
| B0-1 | 4b9c619 | 22 | 1.7 / 24.7 / 57.9 / 133.8 · 22 | 1.2 / 16.1 / 108.4 / 687.3 · 12 |
| B0-2 | 4b9c619 | 107 | 1.7 / 5.8 / 31.4 / 87.2 · 5 | 1.3 / 12.7 / 46.3 / 289.8 · 14 |
| B0-3 | 4b9c619 | 33 | 1.7 / 9.6 / 24.0 / 56.4 · 11 | 1.6 / 9.1 / 42.3 / 315.6 · 7 |
| B4 | b1b77b2 | 17 | **1.0** / 3.8 / 8.3 / 31.6 · 2 | **1.3** / 10.6 / 44.2 / 307.9 · 13 |
| B5 | b1b77b2 | 38 | 1.1 / 10.6 / 49.4 / 70.9 · 10 | 1.1 / 12.2 / 65.6 / 750.6 · 10 |

Read across p50: B0 cost 0.5 ms a frame at zoom 1 in all four runs (1.7 against 1.2–1.4), the
bounds-tree insert per bare primitive; B gives it back (1.0–1.1 against 1.2–1.4, the cheapest
pan of the night) and the zoom p50 is the pan p50. The zoom p99 / max did not reach the
brief's 16.7 ms on any tree in any run — including A3 at 8 processes (17.5 / 268.7) — and the
sample below says why: with shaping gone, the zoom frame is prepaint's per-cell loops over all
twenty visible grids (a pan at zoom 1 sees three) plus the flood's apply, not text. **No zoom
guard**: a limit that the same tree fails and passes by load would only flap. Re-run on an idle
machine before ruling further: `pgrep -fl "cargo|rustc" | wc -l` must be 0 for the whole run.

What the main thread does during the zoom cycle on B0 (`sample <pid> 4` while (b) runs, 23
build processes alongside; `/tmp/glyphs-sample-zoom.raw`, 2176 samples on the main thread;
inclusive share of the highest frame naming the symbol; on B the `paint_glyph` row loses its
`BoundsTree::insert`, 268 samples = 12 %):

| stage | share |
| --- | --- |
| drawing (`flush_effects`) | 36 % |
| of which prepaint (per-cell loops: quads, decorations, word keys) | 26 % |
| of which `paint_glyph` (all of it scene insertion, `insert_primitive` 11 %) | 12 % |
| of which shaping (`shape_line`, only words never seen) | **2.5 %** (41–48 % in the samples above) |
| of which glyph rasterisation (`rasterize_glyph` + `raster_bounds`) | 1.1 % |
| applying host frames (`TermState` 15.5 %, `Vec<Cell>` copies 15 %, `slopty_grid` drops 10 %) | 33 % |

Shaping is gone from the zoom; the draw that remains is per-cell bookkeeping and the scene,
and a third of the thread is the flood itself (the `Arc<Line>` item in DECISIONS). The
rasteriser is not worth quantising the size for.

```sh
pgrep -fl "cargo|rustc" | wc -l
cargo xtask e2e smooth > /tmp/glyphs-smooth-after.log 2>&1; grep MEASURE /tmp/glyphs-smooth-after.log
sh /tmp/glyphs-sample-zoom.sh      # scenario (b) alone with `sample` on the app's main thread
```

## 2026-09-06 — gate wall time, second look (mac-studio, 10 cores, warm caches)

Re-evaluating gate wall time and investigating whether nextest test binary codegen can share
artifacts with clippy or run concurrently. Tested on `pi/gatetime` worktree against baseline
(e811039).

### Methodology & Commands
* Warm: touch nothing, `time cargo xtask gate`.
* Incremental: touch `crates/slopty-client/src/lib.rs` (revert after), `time cargo xtask gate`.
  Note: appending a trailing newline to EOF triggers `cargo fmt --check` failure (`Diff in ...: - \n`),
  so the incremental touch inserts a blank line between module declarations to pass rustfmt while
  invalidating `slopty-client` and all downstream dependents.
* Nextest alone: `time cargo nextest run --workspace --no-run` alone warm (~1.8–2.0 s) vs after touch
  (12.7 s real, 7.5 s build).
* Nextest rebuilds after touch: `cargo build --tests --workspace -v 2>&1 | grep -c Running` = 0 warm,
  10 units after touch (6 packages: `slopty-client`, `slopty-ui` [lib + test], `slopty-cli` [bin],
  `slopty-hostd` [bin + test], `slopty-app` [lib + test], `slopty` [bin]).

### Results (baseline vs variants)

| Variant | Warm gate (real / step sum) | Incremental gate (real / step sum) | Clippy host (inc) | Clippy iOS (inc) | Nextest (inc) | Doctests (inc) | Rustdoc (inc) | Notes |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| **Baseline** | 14.1 s / 13.3 s | 50.6–66.1 s / 50.0–64.1 s (~57 s avg) | 4.9–12.2 s | 4.6–9.2 s | 14.3–25.6 s | 5.6–18.5 s | 7.1–9.0 s | Clippy host `--target <host>`, nextest default `target/debug` |
| **(i) Nextest `--target <host>`** | 23.4 s / 22.3 s | 55.1 s / 54.2 s | 6.6 s | 5.8 s | 23.4 s | 11.0 s | 6.7 s | Shares `target/<triple>`, but clippy emits rmeta only; doctests/doc still use `target/debug` |
| **(ii) Drop `--target` from host clippy** | 20.8 s / 20.0 s | 46.3–72.6 s / 45.6–72.0 s (~59 s avg) | 5.1–14.5 s | 6.2–12.4 s | 15.9–27.1 s | 5.1–8.5 s | 8.7–10.9 s | Clippy moves to `target/debug`; rmeta cannot link test bins; within noise |
| **(iii) Concurrent nextest build + iOS clippy** | 21.9 s / 19.5 s | 59.6 s / 58.9 s | 6.6 s | 7.9 s | 30.7 s (build+run) | 6.8 s | 14.1 s | Cargo serialises on `target/.cargo-lock` ("Blocking waiting for file lock on build directory") |
| **(iv) Profile test codegen share** | — | — | — | — | — | — | — | Ruled out: clippy emits `.rmeta` only ("check is not build"); test runners require full codegen |

### Outcome
Kept change: **none** (negative result). No variant beats the baseline incremental gate by ≥ 10 %.
The workspace crates recompile during nextest because `cargo check` does not emit object code or `.rlib`s,
sccache cannot cache incremental workspace crates, and Cargo serialises concurrent workspace builds on
the target directory lock.



## 2026-09-06 — the zoom hitch, second look: the font walk at the flip, rows outside the clip

`sample <pid> 4` on the app's main thread while scenario (b) runs, demangled with `rustfilt`
this time (`/tmp/sample-prepaint-before.txt`, main 24b9e67, 1 cargo process alongside, 2283
samples; `/tmp/sample-prepaint-after.txt`, `f0e7007`, 8 alongside, 2088 samples). Inclusive
share of the main thread. The `sample` stops the process at every tick, so the MEASURE rows of
a sampled run are not numbers (its own pan p95 was 58 ms on a tree that pans at 3 ms).

| what | before (main) | after (f0e7007) |
| --- | --- | --- |
| `TerminalElement::prepaint` | **37.8 %** | 4.3 % |
| of which `TextSystem::all_font_names` (`pick_family`, a view's first frame) | 31.9 % | 0 |
| of which the words (segments → cache; `shape_cells` for the flood's new text 3.4 %) | 5.1 % | 3.0 % |
| of which the per-cell loop (quads, decorations, keys) | ≈ 0.4 % | ≈ 0.4 % |
| `TerminalElement::paint` (all `paint_glyph`) | 10.6 % | 12.4 % |
| GPUI layout (`Div::request_layout`, taffy, `compute_layout`) | ≈ 12 % | ≈ 13 % of the thread, 29 % of a draw |
| `TermState::apply` (the flood; `Vec<Cell>::clone` 11 % → 8 %) | 18.8 % | 18.1 % |

The "prepaint per-cell loops 26 %" of the previous section was this font walk read through
mangled names. It is a per-view startup cost that the zoom scenario pays twenty times in one
frame: the canvas culls off-screen items, so at zoom 1 only the viewport's grids ever drew;
⌘1 puts everything below `CARD_ZOOM`; the first step back over it draws twenty first frames,
each listing the installed fonts (a synchronous XPC round trip to fontd, ~36 ms).

Draw ms p50 / p95 / p99 / max · frames over 16.7 ms · dropped, `cargo xtask e2e smooth`, main
24b9e67 (A) against `f0e7007` (B), alternated, `/tmp/prepaint-ab.log`; `noise` is
`pgrep -fl "cargo|rustc|xcodebuild|clang" | wc -l` when the run started (other sessions built
throughout; A2 failed the pan guard at 5 processes, B1 passed at 120):

| run | tree | noise | (a) pan | (b) zoom |
| --- | --- | --- | --- | --- |
| A1 | 24b9e67 | 6 | 0.9 / 3.0 / 19.3 / 39.2 · 5 · 7 | 1.3 / 14.3 / 75.2 / **356.1** · 12 · 48 |
| A2 | 24b9e67 | 5 | 1.3 / 38.7 / 72.8 / 293.8 · 18 · 53 | 1.8 / 51.3 / 280.3 / **1006.2** · 20 · 119 |
| B1 | f0e7007 | 120 | 0.9 / 4.5 / 14.3 / 34.0 · 2 · 3 | 1.4 / 21.9 / 86.4 / **108.6** · 16 · 43 |
| B2 | f0e7007 | 42 | 1.0 / 4.6 / 11.5 / 26.0 · 2 · 2 | 1.4 / 14.1 / 33.7 / **91.3** · 14 · 22 |

The max is the column that moved: the flip's frame went from 356–1006 ms to 91–109 ms on a
busier machine, and the zoom's dropped frames from 48–119 to 22–43. p50 is unchanged (the
element's per-frame work was never the cost); p99 is 34–86 ms and is now layout + paint of
twenty items plus the apply between frames (DECISIONS "What the zoom p99 is now").

```sh
pgrep -fl "cargo|rustc|xcodebuild|clang" | wc -l
zsh /tmp/prepaint-ab.sh                 # A, B, A, B then the after-sample
python3 /tmp/sample-attrib.py /tmp/sample-prepaint-after.txt "TerminalElement as gpui::element::Element>::prepaint" "all_font_names"
python3 /tmp/sample-children.py /tmp/sample-prepaint-after.txt "Window>::draw"
```

## 2026-09-06 — a hidden window on the crop path

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-hostd \
  --test e2e a_hidden_window_stops --no-capture
```

The helper's window, visible and unobstructed so the host serves it as a crop of its display,
then ordered out. Seven runs, mac-studio, debug build:

| what                                                   | measured                  |
| ------------------------------------------------------ | ------------------------- |
| hide → the host is off the crop path (`on_crop` false) | 415 / 466–594 / 1380 ms   |
| crop frames sent between asking it to hide and that    | 13–28 (mostly of the window, which is still up for the first ~100 ms) |
| frames encoded while the window is away                | **0**                     |
| frames withheld by the guard (`ScreenStats::withheld`) | 0–1 per hide              |
| goes back onto the crop while hidden                   | never                     |
| show → frames again                                    | every run                 |

So the alarm raised in the previous section was false as it was written. **These numbers are a
measurement of an empty rectangle, though, and the next section is what they look like once
there is something behind the window to see.** With nothing but the desktop behind it,
ScreenCaptureKit has no new frame to deliver for the crop once the window goes, so "frames
encoded while the window is away: 0" says more about the wallpaper than about the host.

The client's frame count is not usable as evidence here and the test does not assert on it: at
the moment the host leaves the crop the client is still decoding the frames sent while the window
was legitimately up, so its count keeps climbing for seconds afterwards for entirely honest
reasons. Only the host's counters can distinguish the two.

## 2026-09-06 — how late a hide is, and what the crop sends meanwhile

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-hostd \
  --test e2e a_hidden_window_stops --no-capture
```

The same test with a **second window of the same kind directly behind the target, repainting**,
so the rectangle the crop covers holds something that changes the moment the target is ordered
out. That is the difference between the section above and this one, and it changes the answer.

Timed from the helper's own report that AppKit ordered the window out (`isVisible()` false),
five runs, mac-studio, debug build:

| what                                                            | measured           |
| ---------------------------------------------------------------- | ------------------ |
| order out → the host sees the window off screen                 | **267–279 ms**     |
| the host sees it → it has left the crop path                    | 1–20 ms            |
| crop frames sent after the order and before the swap            | **0, 12, 13, 14, 15** |
| frames encoded while the window is away (after the swap)        | 0                  |
| frames withheld by the guard                                    | 0                  |

The ~270 ms is not the geometry tick and not this code: the tick runs every 100 ms on the dot
(traced), and a probe polling every 2 ms straight from the test measures the same. Both ways of
asking CoreGraphics flip together — `kCGWindowIsOnscreen` on the window's own description and
membership of the on-screen window list gave **269 / 269** and **279 / 279** ms in the same
runs — so the lag is the WindowServer's, and asking harder does not help. (An earlier track
changed this predicate from one to the other and reverted it for want of a measurement; this is
that measurement, and it says the two are the same thing.)

For that ~270 ms the display crop keeps sending the rectangle, which now holds the window
behind. The guard withholds nothing, because by the time the host knows, the framework has
already stopped: the guard covers the tick→acknowledgement gap, which is 1–20 ms here, and not
this one. What the crop filter *does* bound is whose windows can be in it — it is
`initWithDisplay:includingApplications:exceptingWindows:` restricted to the target's owning
application — but that bound was not confirmed here: both helpers are bare executables with no
bundle identifier, and the leak measured above says ScreenCaptureKit did not treat the copy at a
second path as a second application. Whether a genuinely different bundled app can appear in the
crop is still open.

## 2026-09-06 — the beat behind the geometry call

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-hostd \
  --test e2e -E 'test(the_heartbeat_keeps_its_cadence)' --no-capture
```

A minute on a stream whose target draws nothing, so the stream carries heartbeats and nothing
else. The window is ordered out *before* the stream opens, so the minute is the steady state
with no path swap in it. Read from the host's own counters (`ScreenStats::beat_gap`,
`beat_gap_worst_us`, `bounds`), mac-studio, debug build:

| gap between beats     | before  | after      |
| ----------------------- | ------- | ---------- |
| p50                   | 31.4 ms | 31.1 ms    |
| p95                   | 34.7 ms | 34.6 ms    |
| max, sliding window   | 75.5 ms | 36.9 ms    |
| **worst since open**  | **242.1 ms** | **44.5 / 51.8 / 71.3 / 74.6 ms** |
| beats in the minute   | 1997    | 1993–2004  |
| receiver: stalls, stalled | 0, 83 ms | 0, 0 ms (all four runs) |

The geometry call in the same loop, over the same runs: p50 0.4–0.7 ms, p95 2.1–5.7 ms, **max
10.8–92.7 ms**. That maximum is three beats' worth of the promise, and before the split the beat
waited behind it.

The median does not move and was never the problem: the loop checks a 25 ms promise every
8.3 ms, so beats land on tick boundaries at ~31 ms whatever else is happening. The tail is the
whole story, and **the quantiles cannot see it** — they slide over the last 600 beats, about
twenty seconds of a sixty-second stream, so the run with a 242 ms hole in it reported p95 34.7 ms
and max 75.5 ms. `beat_gap_worst_us` is an all-time maximum for exactly that reason.

Before the split the late beats appeared mid-stream as well as at the swap (gaps of 165, 108 and
109 ms in one run, the last at 28 s with nothing else going on). After it, the mid-stream ones
are gone; what remains is ~75 ms at worst once a minute, and ~108 ms around a path swap. Neither
is this loop's own geometry call, whose maximum in the same runs is 12 ms: the remaining
blocking is elsewhere on the runtime, and a blocked worker delays every timer on it.

## 2026-09-06 — what the crop shows, and which signal knows a hide first

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-hostd \
  --test e2e -E 'test(what_a_crop_shows) or test(a_hidden_window_stops) \
  or test(how_late_each_way)' --no-capture
```

### What reaches the client while nobody knows the window has gone

The section above counted *frames*. That number cannot answer what the crop shows, and the
control here says why: ScreenCaptureKit delivers the rectangle at the frame rate whether or not
anything in it changed, so an empty rectangle produces as many frames as a busy one. The
pictures themselves are read instead — the client's decoded frames, averaged to one number,
brightness 0–255 — over the same hide, driven three ways:

| behind the target                              | frames after the order | brightness swing, last frames | brightness level, last frames |
| ----------------------------------------------- | ---------------------: | ----------------------------: | ----------------------------: |
| nothing (the control)                          |                  11–15 |                     0.0 – 2.0 |                   0.00 – 0.02 |
| a second window of the same executable          |                  13–16 |                     0.0 – 0.9 |                   0.00 – 0.24 |
| the same binary in its own signed `.app` bundle |                  13–16 |                     0.0 – 0.2 |                   0.00 – 0.02 |

The backdrop is a window repainting between two colours for the whole of the gap: while the
target is up, that same measure reads **136** (the target flashing in the crop). After the
target is ordered out it reads **zero, in all three cases** — the crop carries black that stops
changing. Nothing of the window behind reaches the client, and that holds whether the framework
counts it as the target's own application or another one.

So the ⏳ from the crop ruling is answered: the crop cannot be made to show another application,
and the frames sent during the gap are black, not the desktop. The fixture that answers it is a
real second application — the helper binary inside a minimal `.app` with its own
`CFBundleIdentifier`, ad-hoc signed — because a copy of a bare executable at a second path is
still the same application to ScreenCaptureKit.

### Which signal knows first

Both polled every 2 ms from one thread, timed from AppKit's own report that the window was
ordered out, four runs:

| signal                                                            | knows after |
| ------------------------------------------------------------------ | ----------- |
| accessibility (`AXUIElementCopyAttributeValue`, the app's windows) | **0 ms**    |
| core graphics (`kCGWindowIsOnscreen` / the on-screen list)         | 256–266 ms  |

`AXIsProcessTrusted` was true for the test process. The accessibility API knows the moment
AppKit does; core graphics is a quarter of a second behind, which is the whole of the gap.

## 2026-09-06 — stalls are not made by load

```sh
SLOPTY_FLAP_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-hostd \
  --test e2e path_flap_with_no_load --no-capture
# … and path_flap_under_{cpu,user_initiated_cpu,memory_io,external_cpu}_load
```

The flap harness now reports both ends of the same 90 s: the receiver's stalls and holds, and the
host's own capture/encode quantiles and what it had to throw away, read off the control socket
before the daemons go. Five shapes, one at a time, mac-studio, debug, 2026-09-06 02:06–02:16.

| shape                    | stalls | stalled | hold p50/p95/max | jitter | encoded | dropped | queue-full | encode p95 / max |
| ------------------------ | ------ | ------- | ---------------- | ------ | ------- | ------- | ---------- | ---------------- |
| **none (baseline)**      | **31** | 3741 ms | 0 / 0 / 45 ms    | 15 ms  | 2249    | 0       | 0          | 10.5 / 15.7 ms   |
| all-core spin, in-process| 49     | 4031 ms | 0 / 0 / 45 ms    | 34 ms  | 664     | 0       | 0          | 13.7 / 23.8 ms   |
| the same at `USER_INITIATED` | 3  | 1189 ms | 0 / 0 / 3 ms     | 7 ms   | 430     | 21      | 0          | 19.7 / 57.7 ms   |
| memory + I/O             | 4      | 337 ms  | 0 / 0 / 27 ms    | 6 ms   | 808     | 0       | 0          | 14.3 / 32.7 ms   |
| all-core spin, other processes | 38 | 2722 ms | 0 / 0 / 42 ms | 5 ms   | 913     | 0       | 0          | 10.9 / 22.6 ms   |

Read down the first column: **no load produces as many stalls as full load**, and the two heavier
shapes produce fewer. There is nothing load-shaped in this number.

Read across, and the host is not the one clumping under any of them: the datagram queue never
filled (`queue-full` 0 everywhere), nothing was dropped except 21 frames in one run, and `hold`
p50 and p95 are 0 ms — frames arrive complete, not dribbling in fragments. Encode does stretch to
24–58 ms under load, but the receiver already forgives the host's share of a gap from the
`send_ms_lo` stamps, so that is not what these count.

The out-of-process row exists to rule out the harness competing with itself: the load threads for
the other shapes live in the receiver's own process, which would starve it by construction.
Moving the same all-core burn into other processes changes nothing (38 against 49 and 31).

So the pacer is not clumping and there is nothing here to fix at that layer. What is left is a
receiver that charges the link ~0.3 stalls a second on a quiet loopback stream whatever the
machine is doing, which is a question about the stall detector's own pessimism rules — a gap it
cannot attribute is charged to the link by design — and not about pacing. That is its own track.

## 2026-09-06 — two clients on one host (mac-studio, e2e pair build, loopback)

Two `slopty-app` processes paired with one ptyd + hostd, each driven through its own test socket
(`crates/slopty-e2e/tests/pair.rs`, `cargo xtask e2e pair`; the display row also needs
`SLOPTY_SCREEN_E2E=1`). Debug build, loopback iroh, machine shared with other sessions' builds.
The `dump` round trip is one socket message gated on the app's next frame, so it resolves time to
about one frame (~130 ms under a full-screen flood); the propagation and stall numbers are read at
that granularity and the guards sit well clear of it.

Behaviours (each a `MEASURE` line the test prints):

| behaviour | number |
|---|---|
| (a) shell opened on A, shown on B | 0 ms after A (both from the one `SessionOpened`/`Canvas` broadcast) |
| (b) both clients type into one session | keys interleaved and complete: `a0b1c2d3e4f5g6h7i8j9`, none lost or reordered |
| (b) opener drives | A drives, B wears the "take" pill; B's "take" hands the PTY size over |
| (c) permission badge (hook played to hostd) | on both clients within one poll (≤ 250 ms); "1 needs you" on both |
| (f) B's flood while A is killed | longest pause 150-176 ms, against a 131-133 ms baseline with A alive (dump-poll floor); guard 600 ms |
| (f) A's abandoned connection idles out | 44.1 s after relaunch (QUIC `IDLE_TIMEOUT` 45 s); both live clients keep their viewers |

Display fanned out to both clients (first display, native 1920×1080, `SLOPTY_SCREEN_E2E=1`):

| viewer | presented | arrival → present p50 / p95 | skipped | late |
|---|---|---|---|---|
| A | 35 | 4.3 / 57.1 ms | 0 | 0 |
| B | 45 | 4.3 / 21.5 ms | 0 | 0 |

Host-side fan-out (from `slopty host screens` and `ps` CPU time over 4 s windows):

| viewers of the one display | live streams | hostd CPU |
|---|---|---|
| two | 2 | 0.15 cores |
| one | 1 | 0.09 cores |

The host encodes once per viewer: the second viewer of the display costs about 0.06 of a core.
See DECISIONS "Multi-client" for why per-viewer encode is kept.

## 2026-09-06 — a quiet loopback stream's stalls, attributed and removed

`claude/cropsafe` measured a 90 s display stream on loopback with nothing else running and got
as many stalls as under full CPU load (31 stalls / 3741 ms, host hold p95 0 ms — the host never
held a frame). If the host held nothing and the link is loopback, the stalls are the receiver's
reading, not the wire. This section takes every silence past the stall gap apart.

```sh
# mac-studio, private daemons, own data dir and port; an idle desktop as the target.
export D=target/e2e-data/stalls && mkdir -p $D/client
target/debug/slopty-ptyd --socket $D/ptyd.sock &
RUST_LOG=info,slopty_host=debug SLOPTY_PORT=45571 target/debug/slopty-hostd --direct-only \
  --ptyd-socket $D/ptyd.sock --ctl-socket $D/hostd.sock > $D/hostd.log 2>&1 &
TICKET=$(SLOPTY_HOSTD_SOCKET=$D/hostd.sock target/debug/slopty host ticket | tail -1)
SLOPTY_DIRECT_ONLY=1 target/debug/slopty --data-dir $D/client pair "$TICKET"
SLOPTY_DATA_DIR=$D/client SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug \
  target/debug/slopty bench screen --display 6 --seconds 90 --max-stalls 0
```

`--max-stalls` is the self-check the guard lives in: the run exits non-zero if the receiver
counted more. The `silences past the gap:` and `ended by:` lines are new
(`ReassemblerStats::silences`), and `reader lag` is how far behind the arrival stamps the
client's own stream worker was reading (`ScreenStats::reader_lag_max`).

### Before — every silence past the stall gap, 3 × 90 s

| run | fps  | stalls (stalled) | host-quiet | in-flight | stamp overshot (worst) | wrapped | absent | none pending | worst gap | reader lag max / ≥ 50 ms |
| --- | ---- | ---------------- | ---------- | --------- | ---------------------- | ------- | ------ | ------------ | --------- | ------------------------ |
| 1   | 17.8 | 2 (289 ms)       | 2          | 1         | 1 (7.6 ms)             | 0       | 0      | 4 of 4       | 245 ms    | 123.3 ms / 76            |
| 2   | 21.8 | 0 (102 ms)       | 6          | 0         | 0                      | 0       | 0      | 6 of 6       | 54 ms     | 148.7 ms / 80            |
| 3   | 26.8 | 2 (169 ms)       | 1          | 1         | 1 (7.3 ms)             | 0       | 0      | 3 of 3       | 117 ms    | 113.3 ms / 64            |

13 silences past the gap in 270 s, 4 of them charged. The three buckets the brief asked for:

* **(a) the sender was silent and the stamps said so — 11 of 13.** Nine read cleanly
  (`host-quiet`); two were thrown away because the host's own interval read *longer* than the
  arrival gap by 7.3 and 7.6 ms, and the old rule discarded any such reading whole and charged
  the entire silence to the link. Both became stalls. The overshoot is structural: `send_ms_lo`
  is written when the host *builds* a datagram, not when QUIC sends it, so two datagrams'
  build→wire delays differ by a few milliseconds and the difference lands on the subtraction.
* **(b) the receiver was not scheduled — the other 2.** `gap=245.6 ms host_gap=5 ms ended_by=Audio
  pending=0` and `gap=117.9 ms host_gap=0 ms ended_by=VideoParity pending=0`. Both read as the
  link having held datagrams, and on loopback with `lost 0 pkts`, `congestion 0` and cwnd at the
  floor that is not credible. The same runs read the client's stream worker **64–80 datagrams
  behind by a stall gap or more, worst 113–149 ms**: this machine descheduled the receiver for
  longer than a stall while the arrival stamps kept coming from the connection's reader task.
* **(c) genuinely in flight — 0.** No silence survived once (a) and (b) were named. Every one of
  the 13 had **no frame pending**: there was nothing for the link to be holding.

The 31 stalls / 90 s of the cropsafe run did not reproduce here; the same shape on this machine
gives 0–2 per 90 s. The mechanism is the same either way, and the counters now say which.

### After — the same command, five samples

| run | fps  | audio pkts | stalls (stalled) | host-quiet | receiver-dozed | in-flight | stamp wrapped / backwards / absent | none pending | worst gap | reader lag max / ≥ 50 ms |
| --- | ---- | ---------- | ---------------- | ---------- | -------------- | --------- | ---------------------------------- | ------------ | --------- | ------------------------ |
| 1   | 18.1 | 3 385      | **0** (0 ms)     | 0          | 0              | 0         | 0 / 0 / 0                          | —            | —         | 235.2 ms / 15            |
| 2   | 22.2 | 3 396      | **0** (0 ms)     | 1          | 0              | 0         | 0 / 0 / 0                          | 1 of 1       | 52 ms     | 40.0 ms / 0              |
| 3   | 11.6 | 0          | 25 (2001 ms)     | 1          | 0              | 25        | 0 / 0 / 0                          | 4 of 26      | 123 ms    | 19.7 ms / 0              |
| 4   | 23.4 | 3 207      | 14 (1118 ms)     | 0          | 0              | 14        | 0 / 0 / 0                          | 0 of 14      | 114 ms    | 90.4 ms / 7              |
| 5   | 29.0 | 0          | 1 (63 ms)        | 0          | 0              | 1         | 0 / 0 / 0                          | 1 of 1       | 63 ms     | 60.4 ms / 109            |

The invariant is the result, not the count: **across all five samples not one stall was charged
for want of a reading** — `stamp_wrapped`, `stamp_backwards`, `stamp_absent` and `receiver_dozed`
are zero everywhere, where before two of four stalls were `stamp_overshot` and the other two were
the receiver being descheduled. Every stall that remains is `in_flight`, and the debug line says
why —

```
gap=93.9ms host_gap=0ns dozed=0ns link_gap=93.9ms stamp=Host(0ns) ended_by=VideoData pending=1
```

— the host built those fragments together (`Host(0ns)`), the receiver was awake throughout
(`dozed=0ns`) and **a fragment of the frame was still pending**. That is a hold, and
`RateVerdict::Stall` freezing on it is correct. What holds them is the host's send side, not the
network: `cwnd 5808` — QUIC's floor — is about 16 Mbit/s on a 2.9 ms path against a 30 Mbit/s
target, and runs 3 and 4 spend their whole trajectory in `grow/cwnd` and `hold/cwnd`. Runs 3 and
5 also carried **no audio at all** (0 packets against ~3 300), so nothing kept the stream's
datagram cadence under the gap between video frames. Both belong to the rate/transport track; the
point here is that the counter now says which.

The before and after runs are not paired — the machine and the desktop moved between them, and
the stall count follows that more than anything else. What does not move is the attribution, and
that is what the ruling rests on.

Two changes, both in the receiver:

1. **The stamp is read as a signed offset from the arrival gap.** Within the byte's 256 ms range
   a difference of `d` means either `d` or `d − 256 ms`; the reading nearer the gap is the one
   meant. Below the midpoint the host accounts for the whole silence (the 7 ms overshoots), above
   it the datagram overtook its predecessor and buys nothing (the existing reordering guard,
   unchanged: a stamp 10 ms backwards still reads as 246 ms and is still refused). Gaps past
   256 ms are still charged — there the ambiguity is real and the pessimistic reading stands.
2. **Silence the receiver slept through is not charged.** The reassembler is told how often its
   owner promises to `tick` (`Config::tick_period`) and subtracts the stretch since the last one
   beyond that promise. A task that never ran cannot tell a held link from an unread socket, and
   `IDLE_TICK` moved 50 ms → 25 ms so the receiver looks twice per stall gap — the same reason
   the host beats twice per gap. A gap it slept through entirely used to leave exactly one stall
   gap charged, which is a stall.

Not measured: the same attribution over the mesh, where a ≥ 256 ms hold is plausible and the
stamp genuinely cannot resolve it (widening `send_ms_lo` is the fix if that ever shows up);
whether the host's heartbeat is late because `cursor_loop` calls `target_bounds` on its own task
— the stamps say the beats *were* late, but that file belongs to another branch; and why QUIC
sits at the 5808-byte cwnd floor on loopback while the controller asks for 30 Mbit/s, which is
what run 3's stalls are and is the next thing worth chasing.

## 2026-09-06 — the zoom hitch, third look: the chrome's share, the raster per step, text in motion

`sample <pid> 4` on the main thread during scenario (b), demangled (`rustfilt`); inclusive
share of a **draw** (`Window::draw`'s children) unless said otherwise. Before: main 7556564
(`/tmp/sample-chrome-before.txt`, 27 build processes, 2363 samples, draw 1059). After:
`8582c08` (`/tmp/sample-chrome-after3.txt`, idle machine, 2110 samples, draw 853). A sampled
run's MEASURE rows are not numbers.

| stage | before | after |
| --- | --- | --- |
| paint | 42 % | 33 % |
| of which terminal glyphs | 32 % (`rasterize_glyph` **14 %**, scene insertion the rest) | 18 % (all `paint_glyph_scaled`; `rasterize_glyph` 3.9 % of the thread, was 7.5 %) |
| of which chrome text | 5.5 % (its rasters 3 %) | `ChromeText::paint` ≈ 5 % |
| prepaint (the flood's words) | 22 % | 23 % |
| `request_layout` (tree build; chrome `shape_text` 6 % before) | 16 % | 19 % (no shaping in it) |
| taffy `compute_layout` (the whole window) | 11 % | 9 % |
| Metal | 5 % | 6 % |

Of the "29 % layout" the chrome's own share was the 6 % text re-shaped per step plus its
measure leaves; GPUI rebuilds the element and taffy trees every frame (`layout_engine.clear()`
in `Window::draw`), so nothing is laid out *because* the bounds change — the tree walk is per
frame, about ten taffy nodes per item, and a zoom step only adds the text shaping. What the
look found instead was the raster per step.

Draw ms p50 / p95 / p99 / max · frames over 16.7 ms · dropped, `cargo xtask e2e smooth`,
alternated A B A B, `noise` = `pgrep -fl "cargo|rustc|xcodebuild|clang" | wc -l` at the start
of the run. A = main, B = the tree named. Round 1 (`/tmp/chrome-ab.log`) is the first cut,
whose settle frame followed every frame in motion; round 2 (`/tmp/chrome-ab2.log`) settles on
an 80 ms timer; round 3 (`/tmp/chrome-ab3.log`) also keeps the frames between two reports on
the ladder.

| round · run | tree | noise | (a) pan | (b) zoom |
| --- | --- | --- | --- | --- |
| 1 · A1 | main 1960c99 | 0 | 0.8 / 3.4 / 5.8 / 6.8 · 0 · 0 | 1.0 / 4.2 / **9.1** / 42.0 · 2 · 4 |
| 1 · B1 | a0c59dd | 14 | 0.8 / 3.6 / 7.7 / 60.9 · 2 · 4 | 1.0 / 7.1 / 32.0 / 76.2 · 11 · 18 (every 4.9 ms: a settle frame per step) |
| 1 · A2 | main 1960c99 | 3 | 0.8 / 3.7 / 6.9 / 51.2 · 1 · 3 | 0.9 / 5.0 / **10.7** / 25.7 · 2 · 2 |
| 1 · B2 | a0c59dd | 2 | 0.8 / 4.0 / 8.3 / 13.5 · 0 · 0 | 1.0 / 6.4 / 13.9 / 58.9 · 6 · 11 |
| 2 · A1 | main 2f82f2d | 22 | 0.9 / 4.4 / 12.0 / 47.0 · 2 · 3 | 1.2 / 9.5 / 46.6 / 67.9 · 11 · 19 |
| 2 · B1 | 0045325 | 14 | 0.8 / 3.5 / 14.1 / 29.0 · 3 · 3 | 0.9 / 3.9 / **7.9** / 43.5 · 1 · 2 |
| 2 · A2 | main 2f82f2d | 26 | 0.9 / 3.0 / 4.9 / 8.8 · 0 · 0 | 1.0 / 4.7 / 13.2 / 19.7 · 3 · 3 |
| 2 · B2 | 0045325 | 36 | 0.9 / 4.1 / 12.8 / 26.3 · 4 · 4 | 0.9 / 4.1 / **7.3** / 27.0 · 1 · 1 |
| 3 · A1 | main 2f82f2d | 7 | 0.9 / 13.3 / 46.5 / 96.1 · 11 · 22 | 1.3 / 19.5 / 43.8 / 96.4 · 20 · 33 |
| 3 · B1 | 8582c08 | 5 | 0.8 / 2.5 / 4.2 / 7.9 · 0 · 0 | 0.9 / 4.5 / **9.8** / 59.0 · 3 · 5 |
| 3 · A2 | main 2f82f2d | 14 | 0.9 / 3.1 / 7.7 / 17.4 · 1 · 1 | 1.5 / 16.0 / 56.7 / 108.5 · 17 · 34 |
| 3 · B2 | 8582c08 | 0 | 0.8 / 3.4 / 9.1 / 18.8 · 1 · 1 | 0.8 / 4.0 / **8.6** / 34.0 · 2 · 3 |

Read the zoom column: on an idle machine main already sits at 9–11 ms p99 (round 1) — the
34–86 ms of the earlier sections was load — and the final tree holds 7.3–9.8 ms p99 in all
four of its runs at 0–36 build processes, where main swings 13–57; dropped frames 1–5 against
3–34, the pan unchanged. The max (27–59 ms) is the apply landing on a frame and moves with
load on both trees, so the smooth test still has no zoom limit.

```sh
pgrep -fl "cargo|rustc|xcodebuild|clang" | wc -l
zsh /tmp/chrome-ab.sh            # A, B, A, B, then the after-sample of scenario (b)
rustfilt < /tmp/sample-chrome-after3.raw > /tmp/sample-chrome-after3.txt
python3 /tmp/sample-children.py /tmp/sample-chrome-after3.txt "Window>::draw"
python3 /tmp/sample-attrib.py /tmp/sample-chrome-after3.txt rasterize_glyph paint_glyph_scaled shape_text
```

### The Mac and the phone on one host (`cargo xtask e2e pair-ios`)

The same host with the second client in the simulator, the a/b/c/g subset the phone supports
(`crates/slopty-e2e/tests/pair.rs::the_mac_and_the_phone_share_a_host_with_the_simulator`). Run
once on each device; the app is built and installed on the simulator by the recipe:

```sh
cargo xtask e2e pair-ios --sim iphone
cargo xtask e2e pair-ios --sim ipad
```

| behaviour | iPhone simulator (26.5) | iPad simulator (26.5) |
|---|---|---|
| (a) shell opened on the Mac, shown on the phone | 0 ms after the Mac | 0 ms after the Mac |
| (b) typed on the phone into a shell, read on the Mac | `phone-42` echoed back | `phone-42` echoed back |
| (c) permission badge (hook played to hostd) | on both, "1 needs you" | on both, "1 needs you" |
| (g) ⌘W on the Mac closes the shell on both | closed on both | closed on both |

The phone fits two desktop-sized items at a card zoom (0.49), so a shell it types into is first
revealed — that zooms one item up to a live grid (`reveal_session`, clamped to `CARD_ZOOM` 0.6),
which a click cannot, and the soft keyboard then routes to it. The harness gained a `Reveal`
test-socket command for exactly this; over the relay the run is ~51 s each.

## 2026-09-06 — a scrolling terminal under injected loss (mac-studio, e2e app build, loopback)

The run MEASUREMENTS "the path under load" said could not be driven from the hostd loss test,
because that test captures the machine's own still desktop and cannot change it. The app self-test
can: it floods a shell (`/bin/sh -c 'i=0; while :; do printf …; done'`) so its window is a
scrolling terminal, then captures the display that window fills. Loss is injected on the app's own
receive path from a fixed seed (`SLOPTY_E2E_DROP_PERMILLE`), one app process per rate, 5 s each;
the recovery counters are the client's own `ScreenStats`, surfaced in `dump.screens[].recovery`.

```sh
SLOPTY_SCREEN_E2E=1 cargo xtask e2e app   # or the case alone:
SLOPTY_APP_E2E=1 SLOPTY_SCREEN_E2E=1 SLOPTY_E2E_BIN_DIR=$PWD/target/debug \
  cargo nextest run -p slopty-e2e --test app --no-capture -E 'test(a_scrolling_window_recovers)'
```

Debug, ~58 fps (busy frames: ~1600–1800 data fragments over the 5 s, so frames are tens of
fragments — the regime where the one-parity-fragment minimum costs ~0):

| drop | frames | by parity | by NACK | lost | datagrams (lost) | kB | parity ‰ | NACK / refresh | stalls | gap p50 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 0 ‰   | 277 | 0  | 0 | 0 | 1580 (0)   | 1707 | 236 | 1 / 1 | 0 | 17.2 ms |
| 20 ‰  | 292 | 29 | 0 | 0 | 1778 (33)  | 1951 | 254 | 2 / 1 | 0 | 17.2 ms |
| 50 ‰  | 288 | 62 | 8 | 0 | 1789 (75)  | 1881 | 294 | 8 / 1 | 1 | 17.3 ms |
| 100 ‰ | 288 | 95 | 5 | 1 | 1625 (127) | 1696 | 392 | 6 / 2 | 0 | 17.3 ms |

Takeaways, against the still-desktop table above:

* **Busy frames are recovered the same way, and lose almost nothing.** Parity repairs the bulk
  (29 / 62 / 95 frames at 20 / 50 / 100 ‰), a retransmission mops up the rest; one frame was lost
  at 100 ‰, the two-fragment tail a frame plus one parity cannot cover. The verdict is a rate
  under one frame in a hundred, as in the hostd test.
* **The capture is the display the app's flood fills, not the app's own window**: the window
  picker does not offer the app its own windows, so a self-test cannot target its own window. The
  display target still carries the app-driven scrolling content — the point the hostd test could
  not reach — plus whatever else is on the desktop. A cleaner isolation (a dedicated scrolling
  helper window, or a second app viewing the first's window) is left for a follow-up.

## 2026-09-06 — what actually holds a frame on a quiet loopback stream: BBR3's ProbeRTT

Ran on mac-studio, debug binaries, `Display(6)` 1920×1080 HEVC, 60 fps cap, 30 Mbit/s target,
direct-only on port 45571, private data dir, own ptyd + hostd killed after every sample:

```sh
D=target/e2e-data/cadence; rm -rf $D; mkdir -p $D/client
target/debug/slopty-ptyd --socket $D/ptyd.sock &
RUST_LOG=info,slopty_net=debug,slopty_hostd=debug,slopty_host=debug \
  SLOPTY_PORT=45571 SLOPTY_PATH_TRACE_MS=100 SLOPTY_CC=bbr3 \
  target/debug/slopty-hostd --direct-only --ptyd-socket $D/ptyd.sock --ctl-socket $D/hostd.sock \
  > $D/hostd.log 2>&1 &
TICKET=$(SLOPTY_HOSTD_SOCKET=$D/hostd.sock target/debug/slopty host ticket | tail -1)
SLOPTY_DIRECT_ONLY=1 target/debug/slopty --data-dir $D/client pair "$TICKET"
SLOPTY_CC=bbr3 SLOPTY_HOSTD_SOCKET=$D/hostd.sock SLOPTY_DATA_DIR=$D/client \
  SLOPTY_DIRECT_ONLY=1 target/debug/slopty bench screen \
  --display 6 --seconds 90 --mbit 30 --max-stalls 0
```

### First, a correction

The 2026-09-06 stalls section above blames the remaining stalls on `cwnd 5808` "on the host's
send side". The number is real but it was read off the wrong connection. `slopty bench screen`
prints `quic path (client side)`: the **client→host** connection, which carries receiver reports
and NACKs and nothing else. BBR never gets a delivery-rate sample worth the name on it, so it
parks that window at `min_pipe_cwnd` (4 × 1452 = 5808 B) for the whole run, every run, whatever
the media is doing. It never described the stream's send window. The line is now labelled
`quic path (client→host, feedback only)` and the host's own window is sampled on a timer
(`SLOPTY_PATH_TRACE_MS`, off by default) because a congestion window is only legible as a series.

The conclusion survives the correction, for a different reason than the one recorded.

### BBR3 — 5 × 90 s, quiet machine

| run | fps | stalls (stalled) | in-flight | QUIC holds ≥ 25 ms | worst hold | host cwnd min / p50 / max | samples at 5808 | pump behind |
| --- | --- | ---------------- | --------- | ------------------ | ---------- | ------------------------- | --------------- | ----------- |
| 1 | 19.9 | 4 (396 ms) | 4 | 6 | 94 ms | 5 808 / 13 261 / 131 079 | 47 / 903 | ≤ 3 ms |
| 2 | 28.9 | 1 (77 ms) | 1 | 4 | 98 ms | 5 808 / 16 722 / 76 101 | — | 0 ms |
| 3 | 13.0 | **0** | 0 | 1 | 49 ms | 5 808 / 12 794 / 76 048 | — | 0 ms |
| 4 | 8.1 | 15 (1359 ms) | 15 | 19 | 129 ms | 5 808 / 11 032 / 76 092 | 73 / 902 | ≤ 1 ms |
| 5 | 14.2 | 1 (662 ms) | 0 | 2 | 668 ms | 5 808 / 15 087 / 76 092 | 13 / 898 | ≤ 1 ms |

Over the five samples: 7 547 holds in all, mean 2.4 ms — and **32 holds of 25 ms or more, 24 of
them (75 %) with the window at exactly 5 808 bytes.** The stall count follows the long holds run
for run. Nothing else does: `host captured N, dropped 0, encoded N, queue full 0` in every run,
`receiver_dozed` 0 everywhere, and the datagram pump — newly instrumented — never late by more
than 12 ms. The frames are built, they reach the pump at once, and then QUIC sits on them.

That last number was measured wrong the first time and is corrected here. The pump's backlog
timer started when `recv` handed over the first datagram, so it timed the *drain* and could not
see the sleep before it: a pump descheduled for 200 ms while datagrams piled up would wake, drain
in a millisecond and report one millisecond behind. It now sums the turns it owed to datagrams
already queued, and records the worst single turn against the 1 ms deadline the loop asks for
while anything is outstanding — which is the scheduling delay the claim is about, stated
directly. Re-measured over 3 × 90 s with the corrected instrument: worst turn **12 ms**, **no turn
past 25 ms in any run**, against QUIC holds of 96–100 ms at `cwnd 5808` in the same logs. The
conclusion stands; the evidence for it did not, until now. What the instrument still cannot see
is time a datagram spent queued while the pump had found the queue empty at its previous look —
that needs a stamp at enqueue, which is a change to the sender's side of the channel.

5 808 B is `BBR.MinPipeCwnd`, `4 * smss`, and the window reaches it in `ProbeRTT`: every
`probe_rtt_interval` (5 s) without a lower RTT sample, BBR3 clamps the window to
`max(0.5 × BDP, MinPipeCwnd)` for 200 ms. On this path the BDP is ~11 kB, so half of it is below
the floor and the floor is what applies. The floor is reached 1.4–8 % of the time and a frame that
lands in that window waits out the rest of the 200 ms: 25–129 ms typically, 668 ms once — which
is also the only `Stamp::Wrapped` silence any run has produced, so the 256 ms limit on
`send_ms_lo` is no longer hypothetical.

None of it is configurable. `noq_proto::congestion::Bbr3Config` has the fields
(`probe_rtt_cwnd_gain`, `probe_bw_up_cwnd_gain`, `default_cwnd_gain`, …) but `impl Bbr3Config`
exposes **exactly one setter, `initial_window`** (`bbr3/mod.rs:2015`), and `min_pipe_cwnd` is
assigned `4 * self.smss` outright at `bbr3/mod.rs:1841`. 1.2.0 is the only version published, so
there is no bump to reach for either.

### Cubic — 3 × 90 s, same command with `SLOPTY_CC=cubic`

| run | fps | stalls (stalled) | QUIC holds ≥ 25 ms | worst hold | host cwnd min / p50 / max |
| --- | --- | ---------------- | ------------------ | ---------- | ------------------------- |
| 1 | 18.4 | **0** (66 ms) | 0 | 13 ms | 38 967 / 76 818 / 76 818 |
| 2 | 16.2 | 1 (294 ms) | 2 | 48 ms | 38 960 / 76 813 / 116 963 |
| 3 | 18.0 | 1 (185 ms) | 2 | 37 ms | 38 971 / 86 668 / 94 096 |

Cubic has no ProbeRTT, and it shows in the one place it should: **the window never goes below
38 960 bytes**, against BBR3's 5 808. Long holds fall from 4.3 per minute to 0.9, and the worst
hold from 668 ms to 48 ms. Stalls fall with them but not to zero, and with three samples against
five on a machine whose own frame rate wandered between 8 and 29 fps the stall counts are the
noisiest number on this page; the hold distribution is a property of the controller and is not.

### Audio on — the cadence question, answered backwards

The hypothesis this track started from was that a stream with no audio has nothing keeping its
datagram cadence under the inter-frame gap, and that the window falls to the floor for want of
traffic. One more 90 s sample with sound playing (`afplay` at volume 0.02 in a loop, so
ScreenCaptureKit has an audio stream to carry) says the opposite:

| | quiet (5 runs) | audio (1 run) |
| --- | --- | --- |
| audio packets | 0, 0, 0, 0, 18 | 2 367 |
| fps | 8.1–28.9 | 31.3 |
| `host_quiet` silences past the gap | 0–4 | **0** |
| samples with cwnd at 5 808 | 1.4–8.1 % | **23.2 %** (211 / 909) |
| holds ≥ 25 ms | 1–19 | 28, **27 of them at 5 808** |
| stalls (stalled) | 0–15 (0–1359 ms) | 14 (1064 ms), all in-flight |

Audio does what a keep-cadence datagram is supposed to do — it removes sender-side silence
outright, `host_quiet` goes to 0 — and the stalls get *worse*, because the window is not on the
floor for want of datagrams. It is on the floor because the flow is application-limited: a still
desktop carries about 1 Mbit/s, BBR sizes the window from `bw × min_rtt`, and 1–3 Mbit/s over a
1.5 ms path is a BDP of well under one packet, so `max(…, MinPipeCwnd)` is the whole answer. More
datagrams means more bursts arriving into a four-packet window, not a bigger window. The only
filler that would raise the estimate is filler at the target rate — 30 Mbit/s of nothing on a link
carrying 1 Mbit/s of content, which is a cost, not a fix.

Confounded, and worth saying: the audio run is also the busiest sample on the page (31.3 fps,
reader lag max 194 ms against 82–119 ms) and it is one run against five. What it rules out is the
mechanism, not the size of the effect.

### Echo round trip — unchanged

`slopty bench echo` (30 bytes into `/bin/cat`, timed to the first frame back), BBR3, same host:
min 4.1 / p50 5.1 / p90 8.1 / max 10.0 ms, 0 timeouts, quic rtt 2.7 ms. Nothing here touches the
keystroke path, and nothing was traded for throughput.

### The self-check, tightened

`slopty bench screen --max-stalls` counted only stalls that *released*. A stall still on when the
run ends never releases, so it never reaches the counter — `stalled_ms` is its only trace, and the
worst case there is, a link that stops and stays stopped, passed. Cubic run 1 above reports
`stalls 0 (66 ms stalled)` and passed a `--max-stalls 0` run on the old check. It now counts an
unreleased stall and requires zero stalled time when zero stalls are allowed. Every sample on this
page re-scored against the stricter check: **BBR3 1 of 5 pass (run 3), Cubic 0 of 3.** The
acceptance run the cadence brief asked for does not pass on either controller on this machine, and
the reason is the `ProbeRTT` floor above, not the check.

### Not measured

The same comparison over the mesh, which is the path the BBR3 ruling rests on and the one where
Cubic was measured to collapse — until that exists the default stays where it is. Whether the
host's heartbeat is late because `cursor_loop` calls the blocking `slopty_capture::target_bounds`
on its own task every 100 ms (that file belongs to another branch; the beats did go out here —
`ended by: heartbeat` is non-zero in four of eight runs). And a moving screen: every run on this
page is a still desktop, where ScreenCaptureKit delivers 8–29 fps of near-empty frames and the
stream carries about 1 Mbit/s against its 30 Mbit/s target, so nothing here says what the send
side does when there is actually 30 Mbit/s to send.

## 2026-09-06 — BBR3 vs Cubic over the mesh, still desktop and busy source (debug build)

The comparison the cadence ruling left open, now that macbook-pro is on. Path: MacBook Pro over
Wi-Fi → WireGuard mesh (`100.107.14.250`) → mac-studio, `--direct-only` both ends, idle
`slopty ping` before the first run app rtt 8.3 / 10.3 / 38.3 ms and QUIC rtt 12.4 ms. Debug build
both ends, the same profile as every mesh row above. Target display 6 (1920×1080), default
30 Mbit/s target, 90 s per run, **interleaved** BBR3/Cubic so the link's drift is shared. The
link did drift: QUIC lost packets per run climbed 23 → 240 across the twelve still-desktop runs,
and Cubic drew the worse half of it. Also on the machine and its network throughout: Cloudflare
WARP, Slack, Parsec and an iOS Simulator.

**The mesh MTU is 1230, not loopback's 1452**, so BBR3's `MinPipeCwnd` floor here is
**4 920 B**, not 5 808. Same four packets; the two tables cannot be compared on the raw byte.

### A still desktop (application-limited, ~1 Mbit/s of content)

| run | cc | fps | cwnd min / p50 / max | at floor | holds >25 ms | worst hold | stalls (stalled) | gap max | lost / cong | FEC / lost / NACK / refresh |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| b1 | BBR3  | 17.6 | 4 920 / 10 529 / 121 774 | 6.7 %  | 7.3/min   | 113 ms    | 145 (16 302 ms) | 224 ms  | 23 / 17  | 7 / 0 / 8 / 0        |
| c1 | Cubic | 18.8 | 4 246 / 25 468 / 74 255  | 0.1 %  | 8.7/min   | 82 ms     | 171 (20 022 ms) | 291 ms  | 64 / 31  | 15 / 0 / 21 / 0      |
| b2 | BBR3  | 16.2 | 4 920 / 4 920 / 244 948  | 65.7 % | 43.3/min  | 43 757 ms | 268 (29 891 ms) | 1 426 ms| 53 / 41  | 13 / 11 / 1 063 / 21 |
| c2 | Cubic | 18.4 | 5 994 / 15 277 / 71 367  | 0.1 %  | 4.7/min   | 1 979 ms  | 177 (22 063 ms) | 2 223 ms| 124 / 27 | 5 / 4 / 28 / 8       |
| b3 | BBR3  | 18.6 | 4 920 / 7 821 / 115 719  | 37.2 % | 18.0/min  | 135 ms    | 175 (19 948 ms) | 315 ms  | 160 / 88 | 28 / 1 / 42 / 1      |
| c3 | Cubic | 17.2 | 3 273 / 12 306 / 123 721 | 0.1 %  | 18.7/min  | 1 681 ms  | 170 (19 357 ms) | 1 953 ms| 240 / 94 | 18 / 53 / 120 / 60   |
| b4 | BBR3  | 15.9 | 4 920 / 4 920 / 404 972  | 69.2 % | 161.3/min | 1 897 ms  | 184 (22 457 ms) | 1 670 ms| 77 / 53  | 25 / 15 / 1 164 / 22 |
| c4 | Cubic | 18.6 | 2 460 / 9 548 / 238 470  | 0.1 %  | 6.0/min   | 174 ms    | 162 (18 623 ms) | 373 ms  | 136 / 77 | 26 / 2 / 27 / 3      |
| b5 | BBR3  | 18.6 | 4 920 / 7 971 / 169 132  | 34.4 % | 10.7/min  | 139 ms    | 156 (17 469 ms) | 203 ms  | 93 / 62  | 17 / 0 / 22 / 1      |
| c5 | Cubic | 18.6 | 3 808 / 13 034 / 43 543  | 1.9 %  | 10.0/min  | 1 904 ms  | 154 (17 822 ms) | 189 ms  | 114 / 96 | 8 / 95 / 205 / 96    |
| b6 | BBR3  | 18.6 | 4 920 / 10 697 / 120 525 | 4.5 %  | 9.3/min   | 104 ms    | 145 (16 407 ms) | 181 ms  | 9 / 6    | 2 / 0 / 3 / 0        |
| c6 | Cubic | 18.6 | 3 091 / 20 312 / 120 864 | 0.3 %  | 3.3/min   | 39 ms     | 149 (16 807 ms) | 184 ms  | 30 / 24  | 7 / 0 / 10 / 1       |

Medians: fps **18.1 / 18.6**, cwnd p50 **7 971 / 13 034 B**, samples at the floor **35.8 % /
0.1 %**, holds past 25 ms **14.4 / 7.4 per minute**, worst hold **137 / 928 ms**, stalls
**165.5 / 166**, stalled **18 708 / 18 990 ms**, lost packets absorbed **65 / 119** (BBR3 /
Cubic).

**On everything the receiver sees the two are indistinguishable** — same stall count, same
stalled milliseconds, the same gap-max distribution (each has two runs past 1.4 s) — although
Cubic's window runs 3× larger and BBR3 spends a third of its samples on four packets, and
although Cubic absorbed roughly twice the loss. Neither window binds here: a still desktop offers
~1 Mbit/s against a 30 Mbit/s ask, so the controller is choosing a window nothing needs.

### A busy source (a Ghostty window scrolling `seq` on the captured display)

| run | cc | fps | cwnd min / p50 / max | at floor | holds >25 ms | worst hold | stalls (stalled) | gap max | host target, end | datagrams | lost / cong | FEC / lost / NACK / refresh |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| bs1 | BBR3  | 56.2 | 4 920 / 40 524 / 156 295 | 1.2 % | 54.7/min | 377 ms   | 180 (20 015 ms) | 189 ms | **30.0 Mbit/s** | 33 122 | 334 / 262 | 166 / 3 / 259 / 3   |
| cs1 | Cubic | 55.5 | 4 885 / 15 410 / 94 013  | 0.1 % | 44.0/min | 1 861 ms | 182 (20 713 ms) | 281 ms | 7.9 Mbit/s      | 28 052 | 310 / 244 | 153 / 12 / 219 / 14 |
| bs2 | BBR3  | 56.0 | 4 920 / 43 322 / 263 914 | 1.3 % | 36.0/min | 1 413 ms | 176 (19 701 ms) | 447 ms | 13.9 Mbit/s     | 27 780 | 373 / 283 | 189 / 11 / 204 / 11 |
| cs2 | Cubic | 54.5 | 3 218 / 13 077 / 203 716 | 0.1 % | 54.7/min | 2 070 ms | 179 (19 889 ms) | 488 ms | 1.6 Mbit/s      | 24 397 | 533 / 415 | 250 / 28 / 312 / 28 |

Here the controllers separate, and in the direction the 2026-09-05 ruling claimed. **Cubic's
window collapses to 13–15 kB under real loss and its target falls to 1.6–7.9 Mbit/s; BBR3 holds
40–43 kB and one run reached the full 30 Mbit/s.** BBR3 also moved more datagrams in both pairs.
Delivered fps is the same (54–56) because ScreenCaptureKit caps at 60 and both controllers keep
up at this frame size — the difference is the bitrate the encoder was allowed to ask for, which
is picture quality, not smoothness.

The two failure modes are different things and the "worst hold" column mixes them. `held_ms` is
how long QUIC's datagram send buffer went without emptying, not one datagram's delay:

* **BBR3 starves.** b2's 43 757 ms and 33 795 ms holds peaked at **37 kB** with cwnd at 4 920 —
  ~2 Mbit/s through a four-packet window at a 20 ms rtt, so the buffer never drained for 78 s of
  the 90. Not one freeze: 269 released stalls whose worst gap is 186 ms, a drizzle. The 37 kB
  peak is `frame_fits`' `HELD_FLOOR` (32 kB) plus a frame in flight — the guard was dropping
  captures the whole time.
* **Cubic overshoots.** c3's 1 681 ms hold peaked at **267 kB**, c5's 1 904 ms at **347 kB**:
  one refresh keyframe, admitted while the buffer was under the 32 kB floor and then draining
  through a 10–17 kB window. `frame_fits` gates a frame *before* it is encoded, so it bounds
  what is queued ahead of a keyframe, never the keyframe itself. c5's 96 refreshes and 95 lost
  frames are the same event.

### The loss path over the mesh (the item that needed a second machine and a busy screen)

The busy rows above are that measurement. Against the loopback injected-loss rows: parity
recovers far more than is lost at every sample (166 / 3, 153 / 12, 189 / 11, 250 / 28 —
FEC-recovered against frames lost outright), which the loopback rows never showed because
loopback loses nothing without injection. Frames are tens of fragments here, so the packetizer's
one-parity-fragment minimum no longer dominates and the redundancy controller's ratio is what
binds — the regime the loopback note said this test could not reach. The NACK-answer guard is
live and load-bearing: it refused 85 / 76 / 54 retransmits in b2 / b4 / c3, the collapsed-link
case it was added for.

### Stall attribution over the mesh, and whether `send_ms_lo` wraps

Across all sixteen 90 s runs (~24 minutes of streaming, ~2 800 released stalls): **`stamp
wrapped 0` and `stamp backwards 0` everywhere**, and exactly **one** stall gap past
`send_ms_lo`'s 256 ms range — 2.016 s in c2, `host_gap=0ns`, `stamp=Absent`, the whole 2.016 s
charged to the link. That is the right party: the host had a 1 979 ms send backlog in the same
run. Every other gap is under 245 ms and reads as `Host(n)` with the link taking the remainder.

No widening is proposed, and the reason is structural rather than luck. A wrap needs a datagram
to *arrive* carrying a stamp more than 256 ms old; when the link holds everything for two seconds
nothing arrives to carry a stamp, so the case lands on the `Absent` path, which is already
pessimistic. Wrapping would need a link that delays past 256 ms without reordering *and* keeps
delivering — not what this path does.

Commands (mac-studio side; the client side is the recipe in "start-up over the mesh" with
`/tmp/slopty-bench/mesh`):

```sh
# mac-studio, private daemons under this worktree
D=$PWD/target/e2e-data/mesh; mkdir -p $D/data
target/debug/slopty-ptyd --socket $D/ptyd.sock &
RUST_LOG=info,slopty_net=debug,slopty_hostd=debug,slopty_host=debug \
  SLOPTY_PORT=45550 SLOPTY_PATH_TRACE_MS=100 SLOPTY_CC=bbr3|cubic \
  target/debug/slopty-hostd --direct-only --ptyd-socket $D/ptyd.sock \
  --ctl-socket $D/hostd.sock --data-dir $D/data --port 45550 > $D/hostd-<tag>.log 2>&1 &
gzip -1 -c target/debug/slopty | ssh macbook-pro 'mkdir -p /tmp/slopty-bench/mesh && gunzip -c > /tmp/slopty-bench/mesh/slopty && chmod +x /tmp/slopty-bench/mesh/slopty'
open -na Ghostty --args -e sh -c 'seq 1 400000000'          # busy rows only
# macbook-pro, once per run (hostd restarted between runs so SLOPTY_CC takes)
ssh macbook-pro 'export SLOPTY_DATA_DIR=/tmp/slopty-bench/mesh/data SLOPTY_DIRECT_ONLY=1 \
  RUST_LOG=warn,slopty_media=debug,slopty_client=debug; \
  /tmp/slopty-bench/mesh/slopty bench screen --display 6 --seconds 90 --max-stalls 0'
# host-side series, per run
grep 'path health' $D/hostd-<tag>.log   | grep -o 'cwnd=[0-9]*'     # the window as a series
grep 'quic released' $D/hostd-<tag>.log | grep -o 'held_ms=[0-9]* max_bytes=[0-9]* cwnd=[0-9]*'
grep -c 'nack not answered' $D/hostd-<tag>.log
```

### QUIC initial window 32 vs 10 over the mesh (the measurement the IW ruling was owed)

`slopty bench screen --display 6 --seconds 20`, five starts per window, interleaved, BBR3, the
same scrolling window on the display. The keyframe was 58.6–58.8 kB in all ten runs (49 packets
of 1200 B) and encode took 37–44 ms.

| window | first decoded, 5 starts | median | keyframe spread |
| --- | --- | --- | --- |
| 32 (default) | 587 / 567 / 1 024 / 597 / 526 ms | 587 ms | 12–43 ms |
| 10 (`SLOPTY_QUIC_IW=10`) | 555 / 581 / 1 144 / 576 / 782 ms | 581 ms | 20–54 ms |

**No measurable difference: 6 ms between the medians against a 526–1 144 ms spread within each
condition.** The arithmetic behind the 32-packet ruling is not refuted — 49 packets is 5 windows
at IW 10 and 2 at IW 32, which at this path's 11 ms rtt predicts ~33 ms — but that is an order of
magnitude below what start-up on this path actually costs. First datagram alone is 217–304 ms
after `Open` and the encode is 40 ms; the congestion window is not what makes a stream slow to
start here. Run 3 of each pair (1 024 / 1 144 ms) is the same bad minute on the link, which is
what interleaving is for.

The mesh row this ruling was owed since 2026-09-05 therefore exists and says: keep 32 for the
arithmetic, expect nothing visible from it on a Wi-Fi path.

### Bitrate sweep with a busy source, and where the comparison stops being clean

Same busy source, 90 s, interleaved per rate, **on a link that had degraded further** — 502–633
lost packets per run against 310–373 in the 30 Mbit/s rows above, and an idle-ish `ping` during a
run of 7.9 / 65.3 / 138.9 ms (stddev 45 ms) against 8.3 / 10.3 / 38.3 ms at the session's start.
Absolute numbers here are therefore **not** comparable with the 30 Mbit/s rows taken 40 minutes
earlier; only the two runs within each rate are.

| target | cc | fps | cwnd p50 | datagrams | host target, end | worst hold | gap max | lost / cong |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 10 Mbit/s | BBR3  | 52.9 | 18 446 B | 30 154 | 3.6 Mbit/s | 2 564 ms | 479 ms   | 628 / 464 |
| 10 Mbit/s | Cubic | 53.5 | 10 012 B | 21 764 | 1.0 Mbit/s | 1 383 ms | 547 ms   | 633 / 472 |
| 20 Mbit/s | BBR3  | 49.9 | 11 462 B | 21 533 | 1.0 Mbit/s | 9 508 ms | 5 775 ms | 502 / 397 |
| 20 Mbit/s | Cubic | 51.6 | 13 869 B | 29 299 | 5.1 Mbit/s | 2 764 ms | 692 ms   | 611 / 494 |

**This does not reproduce the clean BBR3 win of the 30 Mbit/s pairs.** At 10 Mbit/s BBR3 keeps
the larger window and 39 % more datagrams; at 20 Mbit/s it is BBR3 that cuts to the 1 Mbit/s
floor, with a 9.5 s backlog at cwnd 4 920 and a **5.8 s** arrival gap, while Cubic holds 5.1
Mbit/s and moves 36 % more datagrams. On a link losing ~600 packets per 90 s **both controllers
collapse, and which one collapses in a given run is not predictable from the controller.**

Counting every busy-source pair on this page: BBR3 delivers more datagrams and a higher end
target in **three of four** interleaved pairs, Cubic in one, and the one Cubic win is large. The
two cleanest pairs — 30 Mbit/s, the better link, two runs each — favour BBR3 unambiguously. That
is the strength of the evidence: a real advantage where the window is the binding constraint on a
merely-lossy link, and no demonstrated advantage once the link is bad enough that the rate
controller is cutting to its floor regardless.

What would settle the remaining question is the same sweep with ≥ 3 runs per cell on a link in
one state — which this session could not provide, because the link drifted monotonically worse
across the two hours it took to measure everything above.

## 2026-09-06 — cross-host attention against a real second host

One client, two hosts on two machines: ptyd + hostd + the app on mac-studio, and a second
ptyd + hostd on macbook-pro started over ssh under `/tmp/slopty-e2e/host2-<pid>` with a private
`HOME` (torn down after, `pgrep -f <root>` empty). The app pairs with both, opens a shell on the
MacBook host that round-trips over the mesh, then the cross-host attention path is driven: a
permission hook is played to a MacBook session **through the real `slopty hook` relay over ssh**
(never a Claude Code session, never a keystroke into a shell), the pill badges the cross-host
sum, a tap switches host and focuses the session, and a `notification_response` carrying only the
session UUID routes back to the MacBook host. Finally the MacBook hostd is killed mid-stream (row
goes amber, mac-studio host keeps streaming) and restarted (green, shell reattaches via live
I/O). This is the evidence behind the DECISIONS "Cross-host attention" ruling.

```
# built here for arm64 (same triple as macbook-pro) and copied by the harness:
#   gzip -1 -c target/debug/<bin> | ssh macbook-pro 'gunzip -c > /tmp/slopty-e2e/host2-*/bin/<bin>'
SLOPTY_HOST2=macbook-pro cargo xtask e2e hosts   # ×3
```

| run | host B up | mesh RTT (path) | hook → pill | full run |
| --- | --- | --- | --- | --- |
| 1   | 31.1 s | 8.2 ms (direct) |  4 ms | 54.8 s |
| 2   | 37.7 s | 8.3 ms (direct) | 11 ms | 61.5 s |
| 3   | 50.6 s | 8.3 ms (direct) | 14 ms | 75.7 s |

The mesh link came up **direct over WireGuard every run** (~8.3 ms RTT, `relayed = false` in the
app's dump), matching the earlier bench page's macbook-pro → mac-studio path. Cross-host
attention lands the pill within **4–14 ms** of the relayed hook — the badge is a canvas event
already flowing on the control stream, so it costs a frame, not a round trip. "Host B up" is the
one-time ssh setup (copy three binaries, start two daemons, mint a ticket); it dominates the run
and is not on any latency-critical path. The remote left exactly the two daemons it started
(`strays before teardown: 2`), both reaped, `rm -rf` clean.

## 2026-09-06 — checkpoint cost (the number behind the 500 ms / 1 MiB policy)

`GhosttyEngine::checkpoint` is libghostty-vt's VT formatter over the whole terminal, run by the
session actor 500 ms after the last output or once 1 MiB has been tapped since the last one
(`slopty_host::session::{CHECKPOINT_AFTER, CHECKPOINT_EVERY_BYTES}`). Release, mac-studio, an
80×24 engine with `scrollback_lines = 10_000`, filled with 10 024 coloured 70-column lines
(651 560 bytes) in 64 KiB chunks like PTY reads:

```
cargo nextest run -p slopty-engine --release --run-ignored only checkpoint_cost --no-capture
```

| step | result |
| --- | --- |
| history retained | 10 001 lines |
| checkpoint size | 694 142 bytes (palette ≈ 7 KB, the rest rows + SGR) |
| format | 21.9 ms, 22.6 ms (two runs) |
| replay into a fresh engine, one chunk | 12.1 s |
| replay in 64 KiB chunks | 11.8 s |
| **filling the original engine with the same lines** | **12.6 s** |
| raw `libghostty_vt::Terminal::vt_write` of the same bytes | 13.2 s |

Formatting is cheap enough to run after every quiet spell: 22 ms for a full 10k-line history, once
per 500 ms at most, off the read loop's hot path (the actor formats, the tap task sends). The
replay is not the checkpoint's cost: filling the engine with the same output takes as long, so a
host that restarts with a 10k-line session pays what the session paid to draw it the first time.
That write-path throughput (≈ 50 KB/s, ≈ 1.2 ms per line) is not the engine's either: the raw
binding is as slow, and the cause is the build. Every profile in `Cargo.toml` keeps
`debug = "line-tables-only"`, cargo therefore sets `DEBUG=true` for build scripts, and
`libghostty-vt-sys` takes that as "compile the zig library with `-Doptimize=Debug`", release
included. `.cargo/config.toml` now pins `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`; the same test
after the change is the next entry.

