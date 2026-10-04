# Measurements

Numbers that drove or confirmed a decision, with the exact command that produced them. Re-run
before trusting; hardware and network are stated per entry.

## Reading old entries

Entries before 2026-09-24 ran over iroh, and their commands are kept as they were run. Some of
what they call no longer exists. The plaintext QUIC entry of 2026-09-24 has both forms side by
side.

- `--direct-only` (the worker, `slopty worker install`) and `SLOPTY_DIRECT_ONLY=1` (clients and
  benches) chose iroh's direct path. Every connection is direct now, so leave them out.
- `--print-ticket`, `slopty worker ticket` and `slopty pair` exchanged an iroh ticket. Today the
  worker prints the address it listens on with `--print-addr`. A client dials it with
  `--worker <ip>:<port>` on `ping`, `bench`, `sessions` and `attach`, or adds it once with
  `slopty add`.
- `shell_round_trip_over_iroh`, `screen_stream_over_iroh` and `screen_start_up_over_iroh` in
  `apps/slopty-worker/tests/e2e.rs` are now `shell_round_trip_over_quic`,
  `screen_stream_over_quic` and `screen_start_up_over_quic`.

## 2026-09-04 — control-stream round trip, loopback: iroh `fast-apple-datapath` costs 50 ms

Setup: mac-studio, `slopty-ptyd` + `slopty-worker` + `slopty ping` on the same machine, debug
build, direct path selected. `slopty ping` sends `ClientMsg::Ping` on the control stream and
times the `Pong` (application level, includes both daemons' channel hops).

| build of worker + cli                    | app rtt min | median | max   | QUIC rtt (direct path) |
| --------------------------------------- | ----------- | ------ | ----- | ---------------------- |
| `slopty-net/apple-fast-datapath` on     | 52.8 ms     | 53.8   | 56.9  | 48.3 ms                |
| feature off (plain `sendmsg`/`recvmsg`) | 0.41 ms     | 0.82   | 1.41  | 1.6 ms                 |

Command:

```sh
SLOPTY_DATA_DIR=/tmp/slopty-manual/client target/debug/slopty ping --count 15
```

Ruling: the feature is removed from the workspace (DECISIONS.md, Transport).

## 2026-09-04 — keystroke echo round trip, loopback, debug build

Setup: mac-studio (Apple silicon), `slopty-ptyd` + `slopty-worker` + `slopty open -- /bin/sh`
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

Setup: mac-studio (Apple silicon), `crates/slopty-worker/tests/screen.rs` opens the main display
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
SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-worker --no-capture display_stream
```

End to end through the worker and iroh (`apps/slopty-worker/tests/e2e.rs`,
`screen_stream_over_iroh`): 86 frames in 3 s reassembled and hardware-decoded on the client, 0
lost, 0 NACKs, 0 FEC recoveries on the loopback path.

```sh
SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-workerd --no-capture screen_stream
```

## 2026-09-04 — screen stream end to end, native scale, loopback, debug build

Setup: mac-studio, `slopty-worker --direct-only` and `slopty bench screen` on the same machine
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
# iroh era: see "Reading old entries" at the top for today's flags
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
terminal), launched by the test or by hand. Loopback: `slopty-ptyd` + `slopty-worker` in
`/tmp/slopty-glass`, `--direct-only --port 45599`, the CLI paired with `SLOPTY_DIRECT_ONLY=1`.

**Per-frame host instrumentation.** `SCStreamFrameInfoDisplayTime` (mach absolute time, through
`CMClockMakeHostTimeFromSystemUnits`) equals the sample's presentation timestamp on every
frame of every path (offset p50 0.00 ms, n=285 per row), so *capture latency* below is display
time → ScreenCaptureKit callback and *encode* is `VTCompressionSessionEncodeFrame` →
output callback (`ScreenStats::capture` / `::encode`, `slopty worker screens`).

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
# iroh era: see "Reading old entries" at the top for today's flags
SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-capture --test latency --no-capture
# the worker on the crop / window path, then the bench (release: target/release/…):
SLOPTY_WINDOW_CAPTURE=crop target/debug/slopty-worker --direct-only --ptyd-socket /tmp/slopty-glass/ptyd.sock \
  --ctl-socket /tmp/slopty-glass/worker.sock --data-dir /tmp/slopty-glass/data --port 45599
SLOPTY_DATA_DIR=/tmp/slopty-glass/client SLOPTY_DIRECT_ONLY=1 SLOPTY_WORKER_SOCKET=/tmp/slopty-glass/worker.sock \
  target/debug/slopty bench screen --window 3411 --seconds 5
SLOPTY_DATA_DIR=/tmp/slopty-glass/client SLOPTY_DIRECT_ONLY=1 SLOPTY_WORKER_SOCKET=/tmp/slopty-glass/worker.sock \
  target/debug/slopty bench screen --display 6 --seconds 5
```

## 2026-09-05 — encoder rate control: low-latency vs VBV keys, and three variants (debug build)

Command: `SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-worker --test screen low_latency_versus_vbv --no-capture`
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

Setup: `slopty-worker --direct-only` on mac-studio (Ethernet LAN), `slopty bench screen` on
macbook-pro over Wi-Fi. The LAN is not reachable from the MacBook, so iroh picked the direct path
over the WireGuard mesh (`100.107.14.250`, `SLOPTY_DIRECT_ONLY=1`); idle `slopty ping` on that
path: app rtt min/median/max 6.4 / 9.1 / 12.5 ms, QUIC rtt 10.9 ms, `ping` 0 % loss. Same
targets as the loopback run above (main display 1920×1080, Ghostty window 900×500 scrolling
`seq`). The capture→decoded column is meaningless here — two clocks ~903 s apart — so only
fps, arrival gap, loss and QUIC rtt are recorded; "encoded" is the host's `ScreenStats` line in
the worker log, "decoded" the client's count.

| run                                  | encoded → decoded | fps  | arrival gap p50 / p90 / max | datagrams | FEC / lost / NACK / refresh | QUIC rtt while streaming |
| ------------------------------------ | ----------------- | ---- | --------------------------- | --------- | --------------------------- | ------------------------ |
| main display, mostly still           | 254 → 254         | 26.0 | 20.0 / 119.4 / 175.0 ms     | 625       | 0 / 0 / 1 / 0               | 17.3 ms                  |
| Ghostty window 900×500, scrolling    | 586 → 567         | 57.7 | 15.9 / 22.8 / 290.9 ms      | 3598      | 2 / 5 / 29 / 5              | 44.3 ms                  |
| main display, Ghostty scrolling on it| 557 → 547         | 55.6 | 16.9 / 23.2 / 191.0 ms      | 3296      | 0 / 1 / 13 / 1              | 14.1 ms                  |

Commands (on macbook-pro, binary copied with `gzip -1 -c target/debug/slopty | ssh macbook-pro
'gunzip -c > /tmp/slopty-bench/slopty'`, paired once with `slopty pair <ticket>`):

```sh
# iroh era: see "Reading old entries" at the top for today's flags
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
The worker with `slopty_worker=debug` logs each NACK it receives with the selected path's QUIC
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
# iroh era: see "Reading old entries" at the top for today's flags
RUST_LOG=warn,slopty_media=debug,slopty_client=debug SLOPTY_DATA_DIR=/tmp/slopty-bench/data SLOPTY_DIRECT_ONLY=1 \
  /tmp/slopty-bench/slopty bench screen --window 927 --seconds 15
grep 'frame lost\|nack' <client log>
grep 'nack\|screen closing' <worker log>      # path=rtt … cwnd … congestion … lost … space …
```

## 2026-09-05 — BBR3 vs Cubic on the mesh path (host window), debug build

Same bench as above (window 927 scrolling, 15 s, macbook-pro → mac-studio over the mesh),
host path stats from the worker's `screen closing` line. Two runs each.

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
controller back to back (`SLOPTY_CC=bbr3|cubic` on the worker):

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
| loopback (mac-studio → its own worker)           | 3.6  | 4.9  | 5.9   | 11.7  | 2.6 ms   |
| macbook-pro over Wi-Fi + mesh, quiet link       | 12.1 | 13.8 | 15.6  | 106.0 | 12 ms    |
| same, taken during a bad Wi-Fi phase            | 12.5 | 78.9 | 132.2 | 451.5 | 86 ms    |

```sh
# iroh era: see "Reading old entries" at the top for today's flags
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

`cargo build --release -p slopty-workerd -p slopty-ptyd -p slopty-cli`, the worker restarted from
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
the trajectory below is what `slopty bench screen` printed, not a grep of worker's log on the
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
# iroh era: see "Reading old entries" at the top for today's flags
# on mac-studio: private daemons, own data dir and port
export SLOPTY_DATA_DIR=/Volumes/Lacie/Workspace/oss/slopty-wt/escapes/target/e2e-data/stall
target/debug/slopty-ptyd --socket $SLOPTY_DATA_DIR/ptyd.sock &
RUST_LOG=info,slopty_worker=debug SLOPTY_PORT=45560 target/debug/slopty-worker --direct-only \
  --ptyd-socket $SLOPTY_DATA_DIR/ptyd.sock --ctl-socket $SLOPTY_DATA_DIR/worker.sock --print-ticket
open -na Ghostty --args -e seq 1 300000000          # motion on display 6, no synthetic keys
# on macbook-pro (binary copied with gzip -1 -c target/debug/slopty | ssh macbook-pro 'gunzip -c > /tmp/slopty-bench/stall/slopty')
export SLOPTY_DATA_DIR=/tmp/slopty-bench/stall/data SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug
/tmp/slopty-bench/stall/slopty pair <ticket>
/tmp/slopty-bench/stall/slopty bench screen --display 6 --seconds 20   # prints stalls and the target trajectory
# worker side: grep 'rate decision\|screen closing' $SLOPTY_DATA_DIR/worker.log
# loopback control: the same bench on mac-studio with SLOPTY_DATA_DIR=$SLOPTY_DATA_DIR/client
```

A window that does not change on screen (`--window 1880`, an idle Ghostty) produced
`captured 0` for 20 s: ScreenCaptureKit delivers nothing while the content is static, which
the client reports as refresh requests. Not a regression; bench a moving window or the display.

```sh
# iroh era: see "Reading old entries" at the top for today's flags
# on macbook-pro, paired with the Mac Studio's manual worker (SLOPTY_PORT 45550)
export SLOPTY_DATA_DIR=/tmp/slopty-bench/data SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug
/tmp/slopty-bench/slopty bench screen --display 6 --seconds 20
# on the worker: RUST_LOG=info,slopty_worker=debug slopty-worker → grep 'bitrate\|stream closed'
```

## 2026-09-05 — capture heartbeat, loopback, debug build

The host now sends a bare `Kind::Heartbeat` header whenever nothing left its queue for 25 ms
(`HEARTBEAT_AFTER`, half the receiver's 50 ms stall gap), so a still screen or a capture gap
no longer reads as a link stall at the receiver. Measured on loopback against the control
above (display 6, a Ghostty window scrolling `seq`, 20 s, private daemons on port 45560,
`slopty_worker=trace` so every beat is logged with the silence that triggered it).

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
# iroh era: see "Reading old entries" at the top for today's flags
# on mac-studio: private daemons, own data dir and port (same as the stall section)
export SLOPTY_DATA_DIR=/Volumes/Lacie/Workspace/oss/slopty-wt/escapes/target/e2e-data/stall
target/debug/slopty-ptyd --socket $SLOPTY_DATA_DIR/ptyd.sock &
RUST_LOG=info,slopty_worker=trace SLOPTY_PORT=45560 target/debug/slopty-worker --direct-only \
  --ptyd-socket $SLOPTY_DATA_DIR/ptyd.sock --ctl-socket $SLOPTY_DATA_DIR/worker.sock > $SLOPTY_DATA_DIR/worker6.log 2>&1 &
open -na Ghostty --args -e seq 1 300000000
SLOPTY_DATA_DIR=$SLOPTY_DATA_DIR/client SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug \
  target/debug/slopty bench screen --display 6 --seconds 20
grep -E 'heartbeat|rate decision|stream closed' $SLOPTY_DATA_DIR/worker6.log   # beats, verdicts, ScreenStats { heartbeats }
```

## 2026-09-05 — start-up on a cold connection, loopback, debug build

`screen_start_up_over_iroh` (`apps/slopty-worker/tests/e2e.rs`, gate `SLOPTY_SCREEN_E2E`):
one private ptyd + worker (`--direct-only`, any free port) in a temp dir, then five samples,
each a fresh pairing ticket over the worker's control socket, a fresh endpoint and key, a cold QUIC
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
had blamed). The worker's first `capture started` took 351 ms (`enumerate 60`), later ones 175–190
(`enumerate 62–75`). Sample 1's six NACKs were frame tails held by a 5808-byte congestion
window (BBR3's `ProbeRTT` floor) whose ACK waited on the receiver's 25 ms `max_ack_delay`;
the pump saw every frame end with ~1.5 KB held for 5–7 ms and one 68 KB frame held 105 ms at
a 38 KB window.

After (this branch: decoder warm-up at launch, encoder + capture warm-up when the worker comes
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
  alone, → 184 ms once the worker also warms the encoder (the first `Encoder::new` in a process is
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
SLOPTY_SCREEN_E2E=1 SLOPTY_E2E_SAMPLES=5 RUST_LOG=info,slopty_worker=debug,slopty_codec=debug,slopty_client=debug \
  cargo nextest run -p slopty-workerd --no-capture screen_start_up 2>&1 | tee /tmp/startup.log
grep -E '^\| [0-9]' /tmp/startup.log                         # the table
grep -E 'capture started|keyframe encoded|decoder session|warmed up' /tmp/startup.log
grep -o 'held_ms=[0-9]* max_bytes=[0-9]*' /tmp/startup.log   # the pump's hold episodes
SLOPTY_CC=cubic … / SLOPTY_QUIC_IW=10 …                     # controller / initial window overrides
```

## 2026-09-05 — start-up over the mesh (Wi-Fi MacBook Pro → Mac Studio), debug build

Same private worker on mac-studio (port 45560, own data dir under `target/e2e-data/startup`),
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
# iroh era: see "Reading old entries" at the top for today's flags
# mac-studio: private daemons (this worktree's target/e2e-data/startup)
export SLOPTY_DATA_DIR=$PWD/target/e2e-data/startup
target/debug/slopty-ptyd --socket $SLOPTY_DATA_DIR/ptyd.sock &
RUST_LOG=info,slopty_worker=debug SLOPTY_PORT=45560 target/debug/slopty-worker --direct-only \
  --ptyd-socket $SLOPTY_DATA_DIR/ptyd.sock --ctl-socket $SLOPTY_DATA_DIR/worker.sock --data-dir $SLOPTY_DATA_DIR/data --port 45560 > $SLOPTY_DATA_DIR/worker.log 2>&1 &
SLOPTY_WORKER_SOCKET=$SLOPTY_DATA_DIR/worker.sock target/debug/slopty worker ticket
open -na Ghostty --args -e sh -c 'seq 1 400000000'          # motion on display 6
gzip -1 -c target/debug/slopty | ssh macbook-pro 'mkdir -p /tmp/slopty-bench/startup && gunzip -c > /tmp/slopty-bench/startup/slopty && chmod +x /tmp/slopty-bench/startup/slopty'
# macbook-pro
export SLOPTY_DATA_DIR=/tmp/slopty-bench/startup/data SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug
/tmp/slopty-bench/startup/slopty pair <ticket>
/tmp/slopty-bench/startup/slopty bench screen --display 6 --seconds 20   # "after Open: …", "keyframe spread", stalls, trajectory
# worker side: grep -E 'keyframe encoded|held_ms=[0-9]{3,}|screen closing|stream closed' $SLOPTY_DATA_DIR/worker.log
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
sibling build). Each sample is a fresh QUIC connection to the same private worker, native scale
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
  RUST_LOG=info,slopty_worker=debug,slopty_codec=debug,slopty_client=debug \
  cargo nextest run -p slopty-workerd --no-capture screen_start_up > /tmp/startup-quiet.log 2>&1
grep -E '^\| ' /tmp/startup-quiet.log        # both tables: start-up, then arrival → present
grep -E 'capture started|keyframe encoded|decoder session|warmed up' /tmp/startup-quiet.log
```

## 2026-09-05 — parity, NACK and refresh under injected loss (loopback), debug build

Command (both tables from the same test; the "before" run is the same test with
`crates/slopty-media/src/redundancy.rs` reverted to its previous controller):

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data \
  cargo nextest run -p slopty-workerd --test e2e screen_under_injected_loss --no-capture
grep -E '^\| ' /tmp/loss-after.log
```

Setup: mac-studio, `screen_under_injected_loss` in `apps/slopty-worker/tests/e2e.rs` — the
worker and client in one process over iroh's direct path, main display 1920×1080, 5 s per rate,
four rates back to back. Loss is injected on the client's receive path
(`ScreenRouter::set_loss`) from a fixed seed, so a rate always drops the same datagrams of the
sequence and two builds compare on the same losses. "Parity seen" is parity fragments per thousand
data fragments *on the wire*, not the controller's ratio: the packetizer always sends at least one
parity fragment, which on a mostly-still desktop (4–8 fragments per frame) dominates whatever
ratio is asked for.

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
  cargo nextest run -p slopty-workerd --test e2e screen_under_injected_loss --no-capture
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data \
  cargo nextest run --release -p slopty-workerd --test e2e screen_under_injected_loss --no-capture
```

Setup as in the section above: the worker and client in one process over iroh's direct path, main
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
beside 5 shells, pan; **(d)** 60 letters typed at 15/s into the focused shell; **(e)**/**(f)**
(2026-09-12) a file card holding the 2 000-line cap beside 5 shells, pan and zoom cycle.

How to read the harness: the `e2e` feature is GPUI `test-support`, under which a dirty window
is drawn synchronously in `flush_effects` — a "frame" is an update, not a vsync, so `every`
(the interval between draws) is command cadence, not display cadence, and the product draws
at most once per display period. Draw percentiles are the numbers that transfer. Debug
profile (`opt-level = 1`, deps 3). The machine ran other sessions' builds throughout
(`pgrep -fl "cargo|rustc" | wc -l` was 8–173 between runs), which is where the max columns come
from; p50/p95 moved little between quiet and busy runs.

### Baseline (main 9d01682 + the probe)

Scenarios (a) and (b) did not complete: the host sent a frame every 2 ms per flooding session
(500/s × 20), the per-client sink (256) overran and the worker detached the client 19 times in the
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
  cargo nextest run -p slopty-workerd --test e2e path_flap_under_cpu_load --no-capture
# … and path_flap_under_user_initiated_cpu_load, path_flap_under_memory_io_load
```

The worker and a client on this machine over iroh with **relays enabled**, so the connection holds a
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
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
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
  `slopty-worker` [bin + test], `slopty-app` [lib + test], `slopty` [bin]).

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
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
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
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
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
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
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
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
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
SLOPTY_FLAP_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
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

Two `slopty-app` processes paired with one ptyd + worker, each driven through its own test socket
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
| (c) permission badge (hook played to the worker) | on both clients within one poll (≤ 250 ms); "1 needs you" on both |
| (f) B's flood while A is killed | longest pause 150-176 ms, against a 131-133 ms baseline with A alive (dump-poll floor); guard 600 ms |
| (f) A's abandoned connection idles out | 44.1 s after relaunch (QUIC `IDLE_TIMEOUT` 45 s); both live clients keep their viewers |

Display fanned out to both clients (first display, native 1920×1080, `SLOPTY_SCREEN_E2E=1`):

| viewer | presented | arrival → present p50 / p95 | skipped | late |
|---|---|---|---|---|
| A | 35 | 4.3 / 57.1 ms | 0 | 0 |
| B | 45 | 4.3 / 21.5 ms | 0 | 0 |

Host-side fan-out (from `slopty worker screens` and `ps` CPU time over 4 s windows):

| viewers of the one display | live streams | worker CPU |
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
# iroh era: see "Reading old entries" at the top for today's flags
# mac-studio, private daemons, own data dir and port; an idle desktop as the target.
export D=target/e2e-data/stalls && mkdir -p $D/client
target/debug/slopty-ptyd --socket $D/ptyd.sock &
RUST_LOG=info,slopty_worker=debug SLOPTY_PORT=45571 target/debug/slopty-worker --direct-only \
  --ptyd-socket $D/ptyd.sock --ctl-socket $D/worker.sock > $D/worker.log 2>&1 &
TICKET=$(SLOPTY_WORKER_SOCKET=$D/worker.sock target/debug/slopty worker ticket | tail -1)
SLOPTY_DIRECT_ONLY=1 target/debug/slopty --data-dir $D/client pair "$TICKET"
SLOPTY_DATA_DIR=$D/client SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug \
  target/debug/slopty bench screen --display 6 --seconds 90 --max-stalls 0
```

`--max-stalls` is the self-check the guard lives in: the run exits non-zero if the receiver
counted more. The `silences past the gap:` and `ended by:` lines are new
(`ReassemblerStats::silences`), and `reader lag` is how far behind the arrival stamps the
client's own stream worker was reading (`ScreenStats::reader_lag_max`).

### Before — every silence past the stall gap, 3 × 90 s

| run | fps  | stalls (stalled) | worker-quiet | in-flight | stamp overshot (worst) | wrapped | absent | none pending | worst gap | reader lag max / ≥ 50 ms |
| --- | ---- | ---------------- | ------------ | --------- | ---------------------- | ------- | ------ | ------------ | --------- | ------------------------ |
| 1   | 17.8 | 2 (289 ms)       | 2            | 1         | 1 (7.6 ms)             | 0       | 0      | 4 of 4       | 245 ms    | 123.3 ms / 76            |
| 2   | 21.8 | 0 (102 ms)       | 6            | 0         | 0                      | 0       | 0      | 6 of 6       | 54 ms     | 148.7 ms / 80            |
| 3   | 26.8 | 2 (169 ms)       | 1            | 1         | 1 (7.3 ms)             | 0       | 0      | 3 of 3       | 117 ms    | 113.3 ms / 64            |

13 silences past the gap in 270 s, 4 of them charged. The three buckets the brief asked for:

* **(a) the sender was silent and the stamps said so — 11 of 13.** Nine read cleanly
  (`worker-quiet`); two were thrown away because the host's own interval read *longer* than the
  arrival gap by 7.3 and 7.6 ms, and the old rule discarded any such reading whole and charged
  the entire silence to the link. Both became stalls. The overshoot is structural: `send_ms_lo`
  is written when the host *builds* a datagram, not when QUIC sends it, so two datagrams'
  build→wire delays differ by a few milliseconds and the difference lands on the subtraction.
* **(b) the receiver was not scheduled — the other 2.** `gap=245.6 ms worker_gap=5 ms
  ended_by=Audio pending=0` and `gap=117.9 ms worker_gap=0 ms ended_by=VideoParity pending=0`.
  Both read as the link having held datagrams, and on loopback with `lost 0 pkts`, `congestion 0`
  and cwnd at the floor that is not credible. The same runs read the client's stream worker **64–80 datagrams
  behind by a stall gap or more, worst 113–149 ms**: this machine descheduled the receiver for
  longer than a stall while the arrival stamps kept coming from the connection's reader task.
* **(c) genuinely in flight — 0.** No silence survived once (a) and (b) were named. Every one of
  the 13 had **no frame pending**: there was nothing for the link to be holding.

The 31 stalls / 90 s of the cropsafe run did not reproduce here; the same shape on this machine
gives 0–2 per 90 s. The mechanism is the same either way, and the counters now say which.

### After — the same command, five samples

| run | fps  | audio pkts | stalls (stalled) | worker-quiet | receiver-dozed | in-flight | stamp wrapped / backwards / absent | none pending | worst gap | reader lag max / ≥ 50 ms |
| --- | ---- | ---------- | ---------------- | ------------ | -------------- | --------- | ---------------------------------- | ------------ | --------- | ------------------------ |
| 1   | 18.1 | 3 385      | **0** (0 ms)     | 0            | 0              | 0         | 0 / 0 / 0                          | —            | —         | 235.2 ms / 15            |
| 2   | 22.2 | 3 396      | **0** (0 ms)     | 1            | 0              | 0         | 0 / 0 / 0                          | 1 of 1       | 52 ms     | 40.0 ms / 0              |
| 3   | 11.6 | 0          | 25 (2001 ms)     | 1            | 0              | 25        | 0 / 0 / 0                          | 4 of 26      | 123 ms    | 19.7 ms / 0              |
| 4   | 23.4 | 3 207      | 14 (1118 ms)     | 0            | 0              | 14        | 0 / 0 / 0                          | 0 of 14      | 114 ms    | 90.4 ms / 7              |
| 5   | 29.0 | 0          | 1 (63 ms)        | 0            | 0              | 1         | 0 / 0 / 0                          | 1 of 1       | 63 ms     | 60.4 ms / 109            |

The invariant is the result, not the count: **across all five samples not one stall was charged
for want of a reading** — `stamp_wrapped`, `stamp_backwards`, `stamp_absent` and `receiver_dozed`
are zero everywhere, where before two of four stalls were `stamp_overshot` and the other two were
the receiver being descheduled. Every stall that remains is `in_flight`, and the debug line says
why —

```
gap=93.9ms worker_gap=0ns dozed=0ns link_gap=93.9ms stamp=Worker(0ns) ended_by=VideoData pending=1
```

— the host built those fragments together (`Worker(0ns)`), the receiver was awake throughout
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
(`crates/slopty-e2e/tests/pair.rs::the_mac_and_the_phone_share_a_worker_with_the_simulator`). Run
once on each device; the app is built and installed on the simulator by the recipe:

```sh
cargo xtask e2e pair-ios --sim iphone
cargo xtask e2e pair-ios --sim ipad
```

| behaviour | iPhone simulator (26.5) | iPad simulator (26.5) |
|---|---|---|
| (a) shell opened on the Mac, shown on the phone | 0 ms after the Mac | 0 ms after the Mac |
| (b) typed on the phone into a shell, read on the Mac | `phone-42` echoed back | `phone-42` echoed back |
| (c) permission badge (hook played to the worker) | on both, "1 needs you" | on both, "1 needs you" |
| (g) ⌘W on the Mac closes the shell on both | closed on both | closed on both |

The phone fits two desktop-sized items at a card zoom (0.49), so a shell it types into is first
revealed — that zooms one item up to a live grid (`reveal_session`, clamped to `CARD_ZOOM` 0.6),
which a click cannot, and the soft keyboard then routes to it. The harness gained a `Reveal`
test-socket command for exactly this; over the relay the run is ~51 s each.

## 2026-09-06 — a scrolling terminal under injected loss (mac-studio, e2e app build, loopback)

The run MEASUREMENTS "the path under load" said could not be driven from the worker loss test,
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
  under one frame in a hundred, as in the worker test.
* **The capture is the display the app's flood fills, not the app's own window**: the window
  picker does not offer the app its own windows, so a self-test cannot target its own window. The
  display target still carries the app-driven scrolling content — the point the worker test could
  not reach — plus whatever else is on the desktop. A cleaner isolation (a dedicated scrolling
  helper window, or a second app viewing the first's window) is left for a follow-up.

## 2026-09-06 — what actually holds a frame on a quiet loopback stream: BBR3's ProbeRTT

Ran on mac-studio, debug binaries, `Display(6)` 1920×1080 HEVC, 60 fps cap, 30 Mbit/s target,
direct-only on port 45571, private data dir, own ptyd + worker killed after every sample:

```sh
# iroh era: see "Reading old entries" at the top for today's flags
D=target/e2e-data/cadence; rm -rf $D; mkdir -p $D/client
target/debug/slopty-ptyd --socket $D/ptyd.sock &
RUST_LOG=info,slopty_net=debug,slopty_worker=debug \
  SLOPTY_PORT=45571 SLOPTY_PATH_TRACE_MS=100 SLOPTY_CC=bbr3 \
  target/debug/slopty-worker --direct-only --ptyd-socket $D/ptyd.sock --ctl-socket $D/worker.sock \
  > $D/worker.log 2>&1 &
TICKET=$(SLOPTY_WORKER_SOCKET=$D/worker.sock target/debug/slopty worker ticket | tail -1)
SLOPTY_DIRECT_ONLY=1 target/debug/slopty --data-dir $D/client pair "$TICKET"
SLOPTY_CC=bbr3 SLOPTY_WORKER_SOCKET=$D/worker.sock SLOPTY_DATA_DIR=$D/client \
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
`quic path (client→worker, feedback only)` and the host's own window is sampled on a timer
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
for run. Nothing else does: `worker captured N, dropped 0, encoded N, queue full 0` in every run,
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
| `worker_quiet` silences past the gap | 0–4 | **0** |
| samples with cwnd at 5 808 | 1.4–8.1 % | **23.2 %** (211 / 909) |
| holds ≥ 25 ms | 1–19 | 28, **27 of them at 5 808** |
| stalls (stalled) | 0–15 (0–1359 ms) | 14 (1064 ms), all in-flight |

Audio does what a keep-cadence datagram is supposed to do — it removes sender-side silence
outright, `worker_quiet` goes to 0 — and the stalls get *worse*, because the window is not on the
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
`send_ms_lo`'s 256 ms range — 2.016 s in c2, `worker_gap=0ns`, `stamp=Absent`, the whole 2.016 s
charged to the link. That is the right party: the host had a 1 979 ms send backlog in the same
run. Every other gap is under 245 ms and reads as `Worker(n)` with the link taking the remainder.

No widening is proposed, and the reason is structural rather than luck. A wrap needs a datagram
to *arrive* carrying a stamp more than 256 ms old; when the link holds everything for two seconds
nothing arrives to carry a stamp, so the case lands on the `Absent` path, which is already
pessimistic. Wrapping would need a link that delays past 256 ms without reordering *and* keeps
delivering — not what this path does.

Commands (mac-studio side; the client side is the recipe in "start-up over the mesh" with
`/tmp/slopty-bench/mesh`):

```sh
# iroh era: see "Reading old entries" at the top for today's flags
# mac-studio, private daemons under this worktree
D=$PWD/target/e2e-data/mesh; mkdir -p $D/data
target/debug/slopty-ptyd --socket $D/ptyd.sock &
RUST_LOG=info,slopty_net=debug,slopty_worker=debug \
  SLOPTY_PORT=45550 SLOPTY_PATH_TRACE_MS=100 SLOPTY_CC=bbr3|cubic \
  target/debug/slopty-worker --direct-only --ptyd-socket $D/ptyd.sock \
  --ctl-socket $D/worker.sock --data-dir $D/data --port 45550 > $D/worker-<tag>.log 2>&1 &
gzip -1 -c target/debug/slopty | ssh macbook-pro 'mkdir -p /tmp/slopty-bench/mesh && gunzip -c > /tmp/slopty-bench/mesh/slopty && chmod +x /tmp/slopty-bench/mesh/slopty'
open -na Ghostty --args -e sh -c 'seq 1 400000000'          # busy rows only
# macbook-pro, once per run (the worker restarted between runs so SLOPTY_CC takes)
ssh macbook-pro 'export SLOPTY_DATA_DIR=/tmp/slopty-bench/mesh/data SLOPTY_DIRECT_ONLY=1 \
  RUST_LOG=warn,slopty_media=debug,slopty_client=debug; \
  /tmp/slopty-bench/mesh/slopty bench screen --display 6 --seconds 90 --max-stalls 0'
# worker-side series, per run
grep 'path health' $D/worker-<tag>.log   | grep -o 'cwnd=[0-9]*'     # the window as a series
grep 'quic released' $D/worker-<tag>.log | grep -o 'held_ms=[0-9]* max_bytes=[0-9]* cwnd=[0-9]*'
grep -c 'nack not answered' $D/worker-<tag>.log
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

One client, two hosts on two machines: ptyd + worker + the app on mac-studio, and a second
ptyd + worker on macbook-pro started over ssh under `/tmp/slopty-e2e/worker2-<pid>` with a private
`HOME` (torn down after, `pgrep -f <root>` empty). The app pairs with both, opens a shell on the
MacBook host that round-trips over the mesh, then the cross-host attention path is driven: a
permission hook is played to a MacBook session **through the real `slopty hook` relay over ssh**
(never a Claude Code session, never a keystroke into a shell), the pill badges the cross-host
sum, a tap switches host and focuses the session, and a `notification_response` carrying only the
session UUID routes back to the MacBook host. Finally the MacBook worker is killed mid-stream (row
goes amber, mac-studio host keeps streaming) and restarted (green, shell reattaches via live
I/O). This is the evidence behind the DECISIONS "Cross-host attention" ruling.

```
# built here for arm64 (same triple as macbook-pro) and copied by the harness:
#   gzip -1 -c target/debug/<bin> | ssh macbook-pro 'gunzip -c > /tmp/slopty-e2e/worker2-*/bin/<bin>'
SLOPTY_WORKER2=macbook-pro cargo xtask e2e workers   # ×3
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

## 2026-09-12 — cross-host attention, second run (LAN address, ticket retry, teardown fix)

The same suite re-run from main cfb03d8 with three harness changes and no product change.
`ssh macbook-pro` resolves to the overlay address (`100.64.0.2`), which on this day could not
move 5 MB in 40 s (the LAN address `192.168.100.240` moved it in 0.2 s), so the 60 MB of
binaries the harness ships never arrived and the run hit nextest's 120 s cap. Over the LAN
address the ticket mint then raced the daemon's start-up (`Socket is not connected`) and the
teardown's `pkill -f <root>` killed the ssh shell running it (its own command line holds the
pattern): `mint_ticket` retries for `STARTUP` and the pattern is `<root>/[b]in/`.

```
SLOPTY_WORKER2=congtran@192.168.100.240 cargo xtask e2e workers
```

| host B up | mesh RTT (path) | hook → pill | full run |
| --- | --- | --- | --- |
| 2.8 s | 0.8 ms (direct) | 6 ms | 34.5 s |

Host B comes up in **2.8 s** over the LAN against 31–51 s on 2026-09-06 (the copy is the whole
of it), and the mesh chose the direct LAN path (0.8 ms). Hook → pill stays in the single-digit
milliseconds. Two daemons before teardown, none after.

## 2026-09-06 — checkpoint cost (the number behind the 500 ms / 1 MiB policy)

`GhosttyEngine::checkpoint` is libghostty-vt's VT formatter over the whole terminal, run by the
session actor 500 ms after the last output or once 1 MiB has been tapped since the last one
(`slopty_worker::session::{CHECKPOINT_AFTER, CHECKPOINT_EVERY_BYTES}`). Release, mac-studio, an
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

## 2026-09-06 — libghostty-vt built ReleaseFast: the same checkpoint_cost run

Same command, same machine, after `.cargo/config.toml` pins `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`
(the zig library was `-Doptimize=Debug` under every profile before, see the entry above):

| step | Debug zig (before) | ReleaseFast zig (after) |
| --- | --- | --- |
| fill 10 024 lines (651 560 bytes) through the engine | 12.5 s | **3.6 ms** |
| raw `Terminal::vt_write`, same bytes | 13.2 s | 1.6 ms |
| format the checkpoint (694 142 bytes) | 22–25 ms | 2.7 ms |
| replay the checkpoint into a fresh engine | 11.9–12.1 s | 3.4–3.9 ms |

Roughly 3 500× on the VT write path (≈ 180 MB/s now), 8× on the formatter. Every earlier
latency number that included libghostty-vt parsing (keystroke echo, frame build, scrollback
replay on attach, the checkpoint replay) was measured with the Debug library and is an upper
bound; nothing in this repository ever ran the optimised parser until this pin. The engine's own
overhead over the raw write (3.6 ms vs 1.6 ms for 10k lines: the OSC 133 scan, the anchor
follow, the alternate-screen scan) is now the larger half and is where the next write-path
measurement goes.

## 2026-09-06 — `slopty bench echo` after the ReleaseFast pin: the keystroke path was never parser-bound

Release daemons (13b95f0) on the paired `/tmp/slopty-manual` dirs, direct-only, mac-studio to its
own worker (the client dialled the LAN address, not loopback), three runs of 30 bytes into
`/bin/cat`:

```sh
# iroh era: see "Reading old entries" at the top for today's flags
SLOPTY_DIRECT_ONLY=1 target/release/slopty-ptyd --socket /tmp/slopty-manual/ptyd.sock &
SLOPTY_DIRECT_ONLY=1 target/release/slopty-worker --ptyd-socket /tmp/slopty-manual/ptyd.sock \
  --ctl-socket /tmp/slopty-manual/worker.sock --data-dir /tmp/slopty-manual/data &
SLOPTY_DATA_DIR=/tmp/slopty-manual/client SLOPTY_DIRECT_ONLY=1 target/release/slopty bench echo --count 30
```

| run | min | p50 | p90 | max | QUIC rtt |
| --- | --- | --- | --- | --- | --- |
| 1 | 4.3 | 5.9 | 7.3 | 11.9 | 2.7 ms |
| 2 | 4.4 | 5.6 | 7.3 | 12.8 | 2.6 ms |
| 3 | 4.4 | 6.1 | 10.2 | 31.0 | 3.3 ms |

Same as the 2026-09-05 release row (p50 4.4–5.1, p90 4.8–8.1): one byte through the parser was
never where the time went, so the 3 500× on bulk parsing leaves the echo where it was. The
round trip is still the QUIC rtt plus about 3 ms of engine, coalescing and channel hops; that
3 ms is the next thing to take apart on the keystroke path, not the parser.

## 2026-09-06 — leading-edge frame: the 2 ms coalescing window off the keystroke path

Same setup and command as the entry above, the worker rebuilt with `frame_due_after` framing output
after a quiet spell at once (`MIN_FRAME_INTERVAL` still paces floods; the read arm of the
actor's select still folds already-readable bytes into the frame):

| run | min | p50 | p90 | max | QUIC rtt (smoothed) |
| --- | --- | --- | --- | --- | --- |
| before 1–3 | 4.3–4.4 | 5.6–6.1 | 7.3–10.2 | 11.9–31.0 | 2.6–3.3 ms |
| after 1 | 2.1 | 4.4 | 6.0 | 6.4 | 5.2 ms |
| after 2 | 1.9 | 3.9 | 5.3 | 14.8 | 5.8 ms |
| after 3 | 2.0 | 2.9 | 5.2 | 5.4 | 2.3 ms |

The floor moved by the size of the window (4.3 → 1.9 ms) and the median by 1.5–3 ms; the QUIC
rtt estimate wandered between runs (2.3–5.8 ms, it is iroh's smoothed value, not a per-run
ping), so the medians are the honest comparison. What is left under the floor is one rtt plus
the engine and the channel hops; the flood cap is unchanged (a `yes` still sends one frame per
8 ms).


## 2026-09-06 — the accessibility hide watch: the crop-path gap closed from the other side

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
  --test e2e --test-threads 1 --no-capture -E 'test(a_hidden_window_stops) | \
  test(what_a_crop_shows_of_an_empty_rectangle) | test(what_a_crop_shows_of_another_application)'
```

The same three hides as "a hidden window on the crop path" and "how late a hide is", with
`slopty_capture::HideWatch` on the helper's process (the host is accessibility-trusted; the
`how_late` measurement in the same run still reads accessibility 0 ms, core graphics 265 ms).
Two runs of each, mac-studio, debug build:

| behind the target  | crop frames after the order | after the hide was asked | frames held on suspicion | pictures of the gap decoded by the client | suspicions |
| ------------------ | --------------------------- | ------------------------ | ------------------------ | ----------------------------------------- | ---------- |
| nothing            | **0**, 0                    | 2, 2                     | 15, 11                   | 0, 0                                      | 1, 1       |
| same application   | **0**, 0                    | 1, 3                     | 11, 11                   | 1, 0                                      | 1, 1       |
| another application| **0**, 0                    | 0, 1                     | 9, 12                    | 0, 0                                      | 1, 1       |

Before the watch the second column was 13–28 and the client decoded 4 or more pictures of the
gap (black ones, but pictures). Now the frames of those ~260 ms are held on the host
(`ScreenStats::suspected`, 9–15 per hide at 60 Hz ≈ the 150–250 ms until the window list
agrees and `withheld` takes over) and nothing of the gap goes out; the one picture in the
same-application run is a frame already in flight when the order happened. `swap_ms` (order →
off the crop path) is unchanged at 373–473 ms: the watch changes what is *sent* during the
lag, not the lag. One notification per hide, every time, so nothing else in the helper's
window set was mistaken for it; the false-suspicion rate of a real multi-window application is
still to be read from `suspicions` against `withheld` on a long session.

## 2026-09-06 — what a datagram waits in the pump's channel

```sh
RUST_LOG="info,iroh::_events::path=debug,slopty_worker::conn=debug" SLOPTY_FLAP_E2E=1 \
  SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd --test e2e \
  path_flap_with_no_load --no-capture
# then read the "the datagram pump caught up" lines: waited = p50 / p95 / max over the last
# 1024 datagrams, worst_wait_ms = the worst single wait since the previous line
```

The no-load flap shape (90 s, loopback, mac-studio, debug build), now with every datagram
stamped when it is queued and the wait read by the pump on the way out — the number the
backlog accounting could only approximate, and could not see at all for a datagram that
arrived in an empty queue and waited alone.

| over 1654 caught-up lines, 1656 frames         | value        |
| ---------------------------------------------- | ------------ |
| queue wait p50 (worst line)                    | 0.11 ms      |
| queue wait p95 (worst line)                    | 0.66 ms      |
| queue wait max in any window                   | **10.8 ms**  |
| lines whose worst single wait was ≥ 5 ms       | 8 of 1654    |
| lone waits past 20 ms with no backlog          | 0            |
| turn accounting on the same run (`late_ms`)    | 0 everywhere |
| max queued when the pump looked                | 31           |

The pump is not where the stalls of this shape come from: the typical datagram waits a tenth
of a millisecond for it and the worst waited 11 ms, against the 4 stalls (355 ms) the receiver
counted in the same run and QUIC holds of 96–100 ms measured before ("stalls are not made by
load"). What the stamp adds over the turn accounting is the certainty: the earlier `late_ms`
of 0 was consistent with a lone descheduled datagram it could not see, and now it is not.

## 2026-09-06 — the keystroke path, stage by stage

```sh
# iroh era: see "Reading old entries" at the top for today's flags
# release daemons on the manual sockets, both with the keystroke trace on
SLOPTY_DIRECT_ONLY=1 target/release/slopty-ptyd --socket /tmp/slopty-manual/ptyd.sock &
SLOPTY_DIRECT_ONLY=1 RUST_LOG="info,slopty_worker::session=trace,slopty_worker::conn=trace" \
  target/release/slopty-worker --ptyd-socket /tmp/slopty-manual/ptyd.sock \
  --ctl-socket /tmp/slopty-manual/worker.sock --data-dir /tmp/slopty-manual/data > worker.log 2>&1 &
SLOPTY_DATA_DIR=/tmp/slopty-manual/client SLOPTY_DIRECT_ONLY=1 \
  RUST_LOG="warn,slopty=trace,slopty_cli=trace,slopty_client::link=trace" \
  target/release/slopty bench echo --count 30 > bench.log 2>&1
# both logs carry microsecond timestamps from one clock; match each "bench send" to the first
# line of each later stage after it: term input received → pty input written → echo read →
# frame flushed → frame sent (worker) → frame received → bench frame (client)
cargo nextest run -p slopty-engine --release --run-ignored only frame_cost --no-capture
```

The echo round trip had settled at p50 2.9–4.4 ms with a QUIC rtt estimate of 2–3 ms, and the
working theory was "one rtt plus about 3 ms of engine and channel hops". Trace stamps at every
stage (permanent, `trace` level, two `Option` writes when off) say where it actually went.
Three runs of 30 bytes into `/bin/cat`, release, mac-studio, before any change:

| stage                                  | p50 (3 runs)      | p90         |
| -------------------------------------- | ----------------- | ----------- |
| client → worker, control stream         | 0.94–1.22 ms      | 1.7–2.1     |
| conn → actor channel + master write    | 0.12–0.17 ms      | 0.2–0.4     |
| kernel echo → actor read               | 0.05–0.06 ms      | 0.1         |
| **actor read → frame flushed**         | **1.38–1.41 ms**  | **1.6–2.0** |
| sink → forwarder + `stream.send`       | 0.04–0.05 ms      | 0.1–1.9     |
| worker → client, session stream         | 0.39–0.59 ms      | 1.0         |
| link events channel → bench task       | 0.06–0.07 ms      | 0.1–0.4     |
| total (the bench's own number)         | 3.12–3.92 ms      | 4.9–8.5     |

The engine is not the 1.4 ms: `frame_cost` (one byte in, `take_frame` for the bench's 60×12
screen, 1 000 times, release) is **write 0 µs, take_frame p50 2 µs, max 27 µs**. The stage is
`sleep_until(now)`: the read arm set `frame_due = now` and let the select's timer arm flush,
and a tokio timer with a deadline of "now" is rounded up to the wheel's next millisecond tick
and waited for by the driver's park — 1.4 ms with a spread of 0.3 ms, the signature of a
timer, not of work. The read arm now flushes the frame inline when nothing paces it and sets
the timer only when the previous frame is younger than `MIN_FRAME_INTERVAL`.

After, same command. The first run had the machine to itself; the others did not (load
average 70, another user's `golangci-lint` at 5.7 cores) and every stage on both sides shows
scheduling tails of 1–15 ms — noise this trace can now attribute rather than guess at:

| run               | read → frame p50 / max | total p50 | total p90 | total max |
| ----------------- | ---------------------- | --------- | --------- | --------- |
| after 1 (quiet)   | **0.02 / 0.08 ms**     | **0.65 ms** | 1.14 ms | 1.68 ms   |
| after 2 (loaded)  | 0.03 / 0.16 ms         | 0.87 ms   | 9.5 ms    | 27.8 ms   |
| after 3 (loaded)  | 0.03 / 0.06 ms         | 3.23 ms   | 12.9 ms   | 14.1 ms   |
| after 4 (loaded)  | 0.05 / 1.54 ms         | 4.67 ms   | 11.9 ms   | 19.7 ms   |
| after 5 (loaded)  | 0.05 / 0.08 ms         | 4.09 ms   | 7.0 ms    | 9.8 ms    |
| after 6 (loaded)  | 0.03 / 0.73 ms         | 0.69 ms   | 8.3 ms    | 15.0 ms   |

The stage that was changed is 0.02–0.05 ms in every run, loaded or not; the quiet run's round
trip is **0.65 ms p50, 0.56 ms min** against 3.1–3.9 ms before. The loaded rows are kept
because they are what a shared host looks like, and the trace is how to tell that from a
regression.

Re-measured once the other job had finished (load average 3.2–3.6), three runs:

| stage                              | p50 (3 quiet runs) | p90       |
| ---------------------------------- | ------------------ | --------- |
| client → worker, control stream     | 0.42 ms            | 0.45–0.49 |
| conn → actor + master write        | 0.03–0.04 ms       | 0.04–0.07 |
| kernel echo → actor read           | 0.01 ms            | 0.01–0.03 |
| actor read → frame flushed         | 0.02–0.03 ms       | 0.03–0.05 |
| sink → forwarder + `stream.send`   | 0.02 ms            | 0.03      |
| worker → client, session stream     | 0.18–0.21 ms       | 0.25–0.27 |
| link events channel → bench task   | 0.02 ms            | 0.02–0.04 |
| **total**                          | **0.73–0.75 ms**   | 0.80–1.00 |

Everything that is ours is 0.1 ms; the two QUIC legs are 0.6 of the 0.73.

### The two QUIC legs against the runtime's worker count

Same setup, `TOKIO_WORKER_THREADS=1` for the worker and the bench client against the default (one
worker per core), interleaved within two minutes on the loaded machine (load average 22–31):

| runtime            | client → worker p50 | worker → client p50 | total p50 | total min |
| ------------------ | ------------------ | ------------------ | --------- | --------- |
| one thread, run 1  | 0.36 ms            | 0.18 ms            | 0.7 ms    | 0.4 ms    |
| one thread, run 2  | 0.58 ms            | 0.31 ms            | 1.1 ms    | 0.7 ms    |
| default, run 1     | 0.62 ms            | 0.30 ms            | 1.3 ms    | 0.9 ms    |
| default, run 2     | 0.71 ms            | 0.35 ms            | 1.4 ms    | 1.0 ms    |

On the loaded machine both legs looked 0.1–0.3 ms shorter with one worker. Quiet (load 3.2,
interleaved, two runs each) the difference is gone from the medians and lives only in the
tails:

| runtime, quiet     | client → worker p50 / p90 | worker → client p50 / p90 | total p50 / p90 / max |
| ------------------ | ------------------------ | ------------------------ | --------------------- |
| one thread, run 1  | 0.41 / 0.47 ms           | 0.20 / 0.26 ms           | 0.8 / 0.8 / 0.9 ms    |
| one thread, run 2  | 0.40 / 0.45 ms           | 0.19 / 0.25 ms           | 0.7 / 0.8 / 0.9 ms    |
| default, run 1     | 0.43 / 1.72 ms           | 0.21 / 0.57 ms           | 0.8 / 2.8 / 6.8 ms    |
| default, run 2     | 0.49 / 0.86 ms           | 0.21 / 0.37 ms           | 0.8 / 1.5 / 6.7 ms    |

So the 0.6 ms of QUIC legs is not the thread count: it is the path through iroh and quinn
itself (their socket task, endpoint and connection drivers, and one wake at each end), and
the same on one thread. What the thread count buys is the tail — p90 0.8 against 1.5–2.8 ms,
max 0.9 against 6.8 — which is worth knowing and not worth a runtime change on the strength
of two runs (DECISIONS "The QUIC legs are quinn's and iroh's own").

## 2026-09-06 — a sibling window closing stalls the crop

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
  --test e2e --test-threads 1 --no-capture -E 'test(a_sibling_window_closing) | \
  test(a_hidden_window_stops) | test(what_a_crop_shows_of_an_empty_rectangle) | \
  test(what_a_crop_shows_of_another_application)'
```

The test written to price a false suspicion — the idle-window helper opens a second window of
its own process next to the target and orders it out while the target streams on the crop path
— found something else first. With the hold as it was (frames held for 400 ms, the filter left
alone) the suspicion came and went and **no frame was ever captured again**: `encoded` stood
still for the rest of the run, and the `capture frame status` log added for this (a debug line
on every change of `SCFrameStatus`) showed the framework's last word was a `Complete` frame
before the order-out and nothing after it. An application-scoped display filter
(`initWithDisplay:includingApplications:exceptingWindows:`) stops delivering sample buffers once
a window of that application is ordered out, with no error and no status change. What wakes it,
tried in that order on the same run:

| after the sibling went                                                           | frames again |
| -------------------------------------------------------------------------------- | ------------ |
| nothing (the hold lapses, the filter stays)                                      | never        |
| `updateContentFilter` with the same filter rebuilt from the cached content        | never        |
| `updateContentFilter` with the same kind of filter from a fresh `SCShareableContent` | never     |
| swap to the window filter, then back to the display crop                         | yes          |

Only a change of filter *kind* wakes it. The swap back to the crop is rejected once with
`-3812` when it is asked in the same tick as the retarget (the crop update reaches the framework
while the filter is still the window filter); the retry on the next tick lands, as for any
other path change.

So a suspicion now moves the stream to the window filter for the hold (`follow_window` treats
"suspected" like "covered"), frames held throughout, and the crop returns on the first tick
after the hold. Read from the same tests, debug build, mac-studio, one run each:

| scenario                    | suspicions | frames held | withheld | crop frames after the order | flowing again after | back on the crop |
| --------------------------- | ---------- | ----------- | -------- | --------------------------- | ------------------- | ---------------- |
| sibling closes (false)      | 1          | 16          | 0        | —                           | **520 ms**          | yes              |
| target hides, nothing behind| 1          | 7           | 0        | 0                           | —                   | no (correct)     |
| target hides, same app      | 1          | 5           | 0        | 0                           | —                   | no               |
| target hides, another app   | 1          | 6           | 0        | 0                           | —                   | no               |

The false suspicion costs a 520 ms outage (the 400 ms hold plus a tick and the swap back) and
the stream is whole afterwards; before this it was dead. The true hides gain from it as well:
`swap_ms` (order → off the crop path) is **158–206 ms** against 373–473 before, because the
swap is now asked on the suspicion instead of on the confirmation, and `withheld` is 0 because
the crop is already gone by the time the window list agrees.

One variant was tried and rejected on this evidence: holding only the *crop's* frames during a
suspicion and letting the window filter's through, so a false suspicion would not freeze at
all. Two of the hide runs then encoded 1–2 frames after the swap had settled, and the client's
tail pictures were the backdrop (luma level 0.5 behind a same-application window and 28.5
behind nothing, against 136 for the target): the framework delivers a frame or two of the old
filter after the swap's completion handler, and with the swap landing before the window list
knows, those are the desktop. The hold covers every frame for its duration, whichever path.

## 2026-09-06 — pop-ups and siblings against the targeted watch

```sh
SLOPTY_SCREEN_E2E=1 SLOPTY_DATA_DIR=target/e2e-data cargo nextest run -p slopty-workerd \
  --test e2e --test-threads 1 --no-capture -E 'test(a_popup_of_the_application) | \
  test(a_sibling_window_closing) | test(a_hidden_window_stops) | \
  test(what_a_crop_shows_of_an_empty_rectangle) | test(what_a_crop_shows_of_another_application)'
```

The idle-window helper gained `popup`/`unpopup`: a borderless, non-activating `NSPanel` at the
pop-up menu level below the target, the shape of an autocomplete list or a tooltip. With the
watch counting every window of the application as the target, the panel's order-out was a
suspicion like any other: **suspicions 1, 10 frames held**, a 400 ms freeze per pop-up, which an
editor would pay on every completion list. The watch now matches the target's element once at
registration (`AXPosition`/`AXSize` within 1 pt and `AXTitle` against `kCGWindowBounds` and
`kCGWindowName`; `targeted=true` in the log for every stream of this run) and another window's
going is `siblings`, a wake through the window filter with no hold. Debug build, mac-studio,
load average 5, one run each:

| what went                    | suspicions | siblings | held | withheld | ten more frames after the order-out | on the crop after |
| ---------------------------- | ---------- | -------- | ---- | -------- | ----------------------------------- | ----------------- |
| a pop-up panel               | 0          | 1        | 0    | 0        | 367 ms                              | yes               |
| a titled sibling window      | 0          | 1        | 0    | 0        | 207 ms                              | yes               |
| the target (nothing behind)  | 1          | 0        | 7    | 0        | —                                   | no (correct)      |
| the target (same app behind) | 1          | 0        | 7    | 0        | —                                   | no                |
| the target (other app behind)| 1          | 0        | 9    | 0        | —                                   | no                |

The three hides are unchanged from "a sibling window closing stalls the crop" (0 crop frames
after the order, off the crop path 154–209 ms after it), so matching the target lost nothing
on the true case. One of the five runs decoded 30 pictures *of the target* after the order —
the host's counters showed nothing sent after it and 7 held, so those were frames already sent,
let out of the client's decoder late on the loaded machine (0–2 in the eight other runs of the
day). The client-side assertion now counts pictures *unlike* the target (`foreign_after_order`,
more than 8 luma levels from every picture decoded while it was visible), which is the leak it
was there to catch; pictures of the target after the order are never one.

## 2026-09-12 — search over a full history: the columns were the cost, not the text

`cargo nextest run -p slopty-engine search_cost --run-ignored all --no-capture --cargo-profile
release` (release build of the host engine; ten searches each over a history of N lines of
"line i the quick brown fox jumps over the lazy dog"; "plain" is the needle `lazy dog`, which
hits every row; "regex" is `line [0-9]+7 `, which hits one row in ten; `max` = 100).

| lines  | format (plain text) p50 | plain before | plain after | regex before | regex after |
|--------|-------------------------|--------------|-------------|--------------|-------------|
| 1 001  | 0.2 ms                  | 2.4 ms       | 0.4 ms      | 0.6 ms       | 0.4 ms      |
| 10 001 | 2.0 ms                  | 21.3 ms      | 3.4 ms      | 5.0 ms       | 3.3 ms      |
| 50 001 | 9.9 ms                  | 104 ms       | 16 ms       | 23 ms        | 16 ms       |

"Before" is main at `b2d5ca3`: the plain path lower-cased every row into a fresh `String`
(an allocation per row), and both paths laid out the cell columns of every hit — a call into
libghostty's `grapheme_width` per character of every row that hit, so a needle that hits every
row paid it 50 000 times for the 100 hits it reports. "After": plain text is an escaped regex
(memchr literal search, Unicode case folding), hits are counted as they are found and only the
newest `max` get their columns, ASCII rows skip the layout altogether. What remains at 50 k
lines is the formatter (10 ms, libghostty writing the history as plain text) plus ~6 ms of
per-row regex scanning; ghostty's native `ghostty_search_*` API would skip the formatter but
knows no regex (see DECISIONS "Terminal").

## 2026-09-12 — the smooth probe after the fork sync and ghostty bump (main `becd27e`)

`SLOPTY_SMOOTH_E2E=1 cargo xtask e2e smooth` on an idle Mac Studio, the display-stream
scenario skipped (no screen-recording grant in this shell). Same shape as the 2026-09-06 runs:
nothing in zed `d12e456`, gpui-kit `84f57fd` or ghostty `44f2a44` moved the frame times.

```
MEASURE (a) mac: 20 streaming shells, pan at zoom 1: draw 0.8 / 1.5 / 3.8 / 12.3 ms · every 3.4 / 16.2 ms · 845 frames, 0 over 16.7 ms, 0 dropped
MEASURE (b) mac: 20 streaming shells, zoom fit → 200 % → fit: draw 0.8 / 2.5 / 6.0 / 25.3 ms · every 3.4 / 16.0 ms · 838 frames, 1 over 16.7 ms, 1 dropped
MEASURE frames while typing: draw 1.2 / 2.8 / 8.7 / 9.1 ms · every 9.6 / 63.5 ms · 189 frames, 0 over 16.7 ms, 0 dropped
MEASURE (d) mac, SLOPTY_PREDICT=never: echo 13.8 / 21.6 / 27.8 ms (60 keys) · predicted 0.0 / 0.0 / 0.0 ms (0 keys)
MEASURE frames while typing: draw 1.4 / 5.8 / 10.9 / 11.0 ms · every 11.3 / 59.7 ms · 189 frames, 0 over 16.7 ms, 0 dropped
MEASURE (d) mac, SLOPTY_PREDICT=always: echo 16.2 / 24.8 / 58.2 ms (60 keys) · predicted 2.6 / 6.6 / 11.2 ms (60 keys)
```

## 2026-09-12 — a full file card in the smooth probe (main `72e7754`)

`SLOPTY_SMOOTH_E2E=1 cargo xtask e2e smooth` on an idle Mac Studio, the new scenarios **(e)**
and **(f)**: one file card holding the 2 000-line cap (`FILE_LINES`, a source-like 70-column
body, landed on line 1 000) beside five flooding shells, panned and zoom-cycled at the same
rates as (a)/(b). The card's rows are a `uniform_list`, so the question was whether the file's
size or the rows on screen set the frame cost.

```
MEASURE (e) mac: 1 file card of 2 000 lines + 5 streaming shells, pan at zoom 1: draw 2.5 / 3.9 / 4.2 / 4.7 ms · every 4.2 / 12.2 ms · 878 frames, 0 over 16.7 ms, 0 dropped
MEASURE (f) mac: 1 file card of 2 000 lines + 5 streaming shells, zoom fit → 200 % → fit: draw 3.7 / 9.4 / 11.4 / 12.0 ms · every 6.3 / 13.2 ms · 794 frames, 0 over 16.7 ms, 0 dropped
MEASURE (a) mac: 20 streaming shells, pan at zoom 1: draw 1.7 / 3.0 / 8.9 / 53.3 ms · every 3.1 / 15.5 ms · 733 frames, 4 over 16.7 ms, 6 dropped
MEASURE (b) mac: 20 streaming shells, zoom fit → 200 % → fit: draw 1.4 / 6.0 / 29.7 / 60.7 ms · every 4.4 / 29.0 ms · 611 frames, 16 over 16.7 ms, 26 dropped
```

Reading: the rows on screen set the cost. A pan with the card draws in 4 ms flat (p50 3.9,
max 4.7 — the flattest profile of any scenario, the 5 shells beside it being the whole
variance), nothing over budget; the zoom cycle peaks at 12 ms where the card at 200 % shows
its widest rows at the largest text, still under 16.7 ms with no drop. (a)/(b) in this run were
noisier than the same-day run above (max 53/61 ms against 12/25 ms) — the 20-shell scenario
ran right after the file scenario's daemons shut down in the same `cargo test` process, and its
tail is the known content-keyed-cache worst case, not a regression the file card introduced
(the card is not on that canvas). No optimisation follows from this: nothing to fix.

A second run the same evening (main `0fa1bee`) repeated the shape — (e) 3.9 / 4.1 / 4.4 ms,
0 dropped; (f) 9.5 / 11.6 / 16.7 ms, 1 dropped; (b) p90 29.7 ms, 16 dropped — but the
machine was not idle (load average 12 on ten cores: a VM, a booted simulator left by the
iOS self-test, other sessions), so the (a)/(b) tails of both evening runs are the
environment's until a run on an idle machine says otherwise; the file-card numbers held
regardless.

## 2026-09-12 — gate wall time, third look: a snapshot and parallel lanes

`cargo gate` before and after the rewrite in `xtask/src/gate.rs` (snapshot of the tree under
`target/gate/tree`, lanes on `target/gate/{tools,clippy-host,clippy-ios,tests,rustdoc}`,
`CARGO_BUILD_JOBS` 1/4/4/6/3, sccache warm). Mac Studio, ten cores, nothing else building.

Old serial gate, warm (`cargo gate > /tmp/gate.log; grep -n '✓' /tmp/gate.log`), the last run
before the rewrite: fmt 1 s, clippy host 23 s, clippy ios 8 s, nextest 129 s (build 7 s, then
73 s between `Finished` and `Starting 563 tests`, run 48 s), doctests 9 s, rustdoc 12 s —
**183 s** end to end, and the working tree locked for all of it.

New gate:

```
cold (four empty target dirs, sccache warm)        725 s   clippy host 584 · ios 670 · nextest 673 · rustdoc 724
incremental, slopty-proto changed (every crate
  downstream rebuilds in all four lanes)            54 s   clippy host 17 · ios 18 · nextest 50 (build 16, run 13) · rustdoc 22 · tools 4
incremental, slopty-ui + slopty-e2e + docs          40 s   clippy host 12 · ios 12 · nextest 36 · rustdoc 17
warm, nothing changed                               19 s   clippy host 3 · ios 3 · nextest 15 (run 13) · rustdoc 3 · tools 4
```

(from `/tmp/gate-new*.log`, the `✓ <lane> (<time>)` lines; a lane's time includes waiting
for cargo's package-cache lock, which the four lanes take in turn for a few seconds each).

What changed the number: the lanes overlap, so the wall time is the slowest lane (nextest)
rather than the sum; the unexplained 29–73 s listing gap of the old gate did not reappear
(the tests lane goes from `Finished` to `Starting` in about 20 s here, most of it the
package-cache lock while the other lanes start). The cold run is paid once per target dir
and again after a toolchain or dependency bump.

## 2026-09-12 — syntax colouring: what a parse costs

`AWS_LC_SYS_CMAKE_BUILDER=1 cargo nextest run -p slopty-ui --target aarch64-apple-darwin
--run-ignored ignored-only -E 'test(timing_of_a_full_card)' --no-capture`, Mac Studio, the
dev profile (optimised) and `--release` (one run each; both numbers alike, the regex engine
dominates):

```
grammar set on first use        1.5 ms  (dev)   0.8 ms  (release)
canvas.rs, 6 582 lines, parse   639 ms          541 ms      ≈ 80 µs a line, 50 149 spans
the same text again             590 ms          500 ms      (no per-text cache to warm)
a 4 000-byte fenced block       7.4 ms          6.3 ms
```

What follows: a full file card (2 000 lines, the host's cap) is ≈ 165 ms of a background
thread, never the UI thread, and the card shows plain text meanwhile; a fenced block is
coloured at layout on the UI thread, once per block (gpui-kit caches the ranges against the
highlighter), so a long answer pays a few ms on its first frame. `regex-fancy` is the price of
pure Rust: syntect's onig engine is reported 2–3× faster, which would be a C dependency.

## 2026-09-12 — deep checks: what each costs on this machine

First runs of `cargo xtask deep …` (cold `target/deep/<check>` dirs, sccache warm, a Miri
build sharing the machine), Mac Studio:

```
deep features   cargo hack check --each-feature, 7 crates with features    517 s cold
deep miri       slopty-proto + slopty-core, PROPTEST_CASES=8              147 s  (codec_props 79 s of it; golden failed)
deep miri       slopty-proto alone, rerun beside a running gate            416 s  (all green)
deep sanitize   thread: pty, net, worker, codec, -Zbuild-std, 79 tests       263 s  (build ≈ 190 s, run 66 s; no race)
deep coverage   llvm-cov nextest, 553 tests, workspace minus e2e/xtask     359 s  (73.1 % of 75 832 lines)
```

Coverage by crate (lines, unit and headless tests only — the app, workerd, capture and the
CLI are exercised by the live layers this report cannot see): agent 96.6 %, media 95.8 %,
theme 95.6 %, predict 95.4 %, grid 94.4 %, engine 89.3 %, settings 89.0 %, pty 88.9 %,
ui 86.4 % (16 587 lines), codec 84.5 %, proto 79.3 %, input 78.4 %, net 73.5 %, client
73.4 %, core 73.0 %, worker 50.7 %, cli 21.2 %, capture 10.9 %, workerd 7.0 %, app 4.0 %.
The files with real logic and the least cover: `slopty-worker/src/manager.rs` 0 %,
`slopty-client/src/screen.rs` 23 %, `slopty-worker/src/screen.rs` 34 %, `slopty-ui/src/screen.rs`
36 %, `slopty-net/src/endpoint.rs` 46 % — the next test passes go there.

Under ThreadSanitizer six tests (the four `session_actor` ones and the two that spawn a pty
program) each took 60–61 s and passed: a 60 s wait somewhere in the pty path that the
sanitizer's slowdown turns from an early return into a full timeout. Worth a look if it
recurs — it is the run's whole cost — but not a race.

What follows: neither belongs in the gate (its budget is 5–10 minutes for everything); both
are the weekly `Deep` workflow's, one runner each, and run by hand before a release. The
first Miri run failed on `tests/golden.rs` — insta shells out to `cargo metadata` and Miri
cannot `fork` — so `deep miri` sets `INSTA_WORKSPACE_ROOT`; the rerun is the number kept.

## 2026-09-13 — a coloured file card's zoom: the shaper split runs on colour

The smooth probe's (f) after the colouring landed (main `e931d97`, 2026-09-12) against the
run above at `72e7754`: the card at 200 % dropped 116 of 565 frames where it had dropped none.
Command, on an idle Mac Studio, dev profile (optimised), the file scenario alone:

```
cargo build -p slopty-ptyd -p slopty-workerd -p slopty-cli -p slopty -p slopty-e2e --bins --features slopty/e2e
SLOPTY_SMOOTH_E2E=1 cargo nextest run -p slopty-e2e --test smooth --no-capture --no-fail-fast -E 'test(a_full_file_card)'
```

```
main e931d97   (f) draw 4.0 / 22.5 / 24.4 / 25.5 ms · every 6.1 / 22.8 ms · 563 frames, 116 dropped
```

`git bisect run` over `2b8e9cb..e931d97` (`/tmp/bisect-f.sh`: build the bins, run (f), good
under 40 dropped), each step a fresh build and one run:

```
1a7fc04  (f) 3.8 / 9.4 / 11.6 / 12.4 ms · 791 frames · 0 dropped   good
2b8e9cb  (f) 3.9 / 9.4 / 11.3 / 13.1 ms · 795 frames · 0 dropped   good
ab6be77  (f) 3.8 / 9.3 / 11.3 / 11.7 ms · 796 frames · 0 dropped   good
e931d97  (f) 4.0 / 22.5 / 24.4 / 25.5 ms · 563 frames · 116 dropped bad   ← first bad
```

The experiment that named the cost: the same build with the card's spans dropped at draw
(`SLOPTY_NO_COLOUR`, a temporary switch in `FileView`'s row closure, not kept):

```
e931d97, no colour   (f) 3.8 / 9.2 / 11.3 / 12.3 ms · 796 frames · 0 dropped
```

So the coloured rows cost 13 ms a frame at 200 %, and the parse was not it (it runs once, on
a background thread). The reason is in GPUI's `text_system.rs`: `shape_line` and the three
`layout_line` variants started a new `FontRun` on every decoration change, so a row of code
with a span per token shaped as ten or twenty CoreText runs instead of one, at every zoom
step (the zoom changes the font size, so the shaped-line cache misses on every frame of the
cycle). The painter (`paint_line`) applies the decoration runs by glyph byte index and never
looks at the font runs, so the split bought nothing but the ligature boundary.

The fix, in the fork (`aislopware/zed` `06c345cf`, "gpui: shape a line's font runs by font, not
by colour"): merge font runs by font id only, at all four sites. The same command after
`cargo update -p gpui`, the two scenarios of the file test:

```
main + fix   (e) draw 2.4 / 3.9 / 4.1 / 5.6 ms · every 4.3 / 11.9 ms · 877 frames, 0 dropped
main + fix   (f) draw 3.7 / 9.6 / 11.7 / 12.7 ms · every 6.5 / 13.5 ms · 787 frames, 0 dropped
```

(f) is back on the `72e7754` line (9.4 / 11.4 / 12.0 ms). A per-row plain-text fallback in
`FileView` (a `SharedString` child instead of a one-run `StyledText` for a row with no spans)
was tried first and did not move the coloured card; it stays only because a plain file's row
saves a run vector for nothing lost.

## 2026-09-13 — the 20-shell zoom: a prompt walk on every frame

Scenario (b) had read 26 and 16 dropped frames in the two evening runs above and was written
off as the machine's load; on an idle machine it stayed bad — 23 dropped beside the file
scenario, 26 run alone (`-E 'test(twenty_streaming)'`), against 1 at `becd27e` — so it was a
regression. `git bisect run` over `c1e679c..72e7754` with `/tmp/bisect-b.sh` (build the bins,
run the 20-shell test, good under 8 dropped), six steps:

```
cb15efe  (b) 1.0 / 5.2 / 20.1 / 97.6 ms · 603 frames · 18 dropped   bad   ← first bad
c4e863d  (b) 0.7 / 1.7 /  3.1 / 29.9 ms · 866 frames ·  1 dropped   good
af3340c  (b) 0.7 / 1.8 /  4.6 / 22.1 ms · 863 frames ·  1 dropped   good
cc4b36a  (b) 0.7 / 1.7 /  3.4 / 12.8 ms · 865 frames ·  0 dropped   good
f9e9150  (b) 0.7 / 1.9 /  5.5 / 36.5 ms · 852 frames ·  3 dropped   good
58854f3  (b) 0.7 / 1.7 /  2.9 / 14.4 ms · 854 frames ·  0 dropped   good
```

`cb15efe` is the sticky block header: `TerminalView::block_header` runs on every render and
asks `TermState::prompt_before(top)`, which walked the cache line by line, a `BTreeMap`
lookup each, from the top row down to the oldest cached line. A flooding shell holds
`CACHE_LINES` (20 000) with its prompt long evicted, so each of the 20 shells walked 20 000
lookups on every frame the zoom redrew: ≈ 10–15 ms a frame, the whole excess. `9554fb4`
(read only the block head) had cut the block's *output* join out of that path, not the walk.
Scenario (a) did not show it because the pan comes first, before the floods fill the cache.

The fix: `Scrollback` keeps a `BTreeSet` of the cached indices that start a prompt (kept on
insert, replacement, eviction and the host's drop), and `prompt_before`/`prompt_after` are
range queries. Both fixes in, all four scenarios, the same command as the file section:

```
main + both  (e) draw 1.1 / 1.4 / 1.6 / 5.0 ms · every 3.7 / 14.1 ms · 883 frames, 0 dropped
main + both  (f) draw 1.2 / 4.7 / 6.1 / 7.8 ms · every 4.8 / 13.3 ms · 884 frames, 0 dropped
main + both  (a) draw 0.8 / 1.1 / 2.0 / 15.7 ms · every 8.1 / 17.5 ms · 603 frames, 0 dropped
main + both  (b) draw 0.7 / 1.8 / 3.9 / 16.7 ms · every 3.1 / 15.9 ms · 857 frames, 1 dropped
```

(b) is back on the `becd27e` line (2.5 / 6.0 / 25.3, 1 dropped) and better; (e) and (f) fell
well under their `72e7754` numbers (3.9 → 1.4 ms and 9.4 → 4.7 ms at p50) because the five
flooding shells beside the card were paying the same walk. The unit is now bounded by the
prompts cached, not the lines.

## 2026-09-13 — the first audio packet: the player's creation blocked the screen worker

A unit test that drives `slopty_client::screen`'s worker end to end (`worker_tests` in
`crates/slopty-client/src/screen.rs`: cursor, a NACKed keyframe, reports, audio, shutdown)
timed out on its audio step with the video counters frozen. `sample` on the test process:

```
Worker::ingest → Player::new → AudioQueueNewOutput → AQ::Server::global
  → AT::MixServer::macOSImpl → AudioObjectAddPropertyListener
  → HALSystem::CheckOutInstance → HALSystem::InitializeDevices → mach_msg
```

The first AudioToolbox client in a process initialises the HAL through coreaudiod, and on the
mac-studio that takes seconds:

```
cargo nextest run -p slopty-codec player_starts_and_drains     (a 100 ms test)
  7.467 s, 7.4 s on a second run in a fresh process
```

The worker called `Player::new` inline on the first `Kind::Audio` datagram, so for that long
it read no datagrams, ran no NACK or refresh timers, decoded nothing and sent no report — a
remote window's picture froze the moment sound started, and the host's rate controller saw a
receiver gone quiet. Fixed in the same change: the player opens on `spawn_blocking`, packets
that land meanwhile count as `audio_lost`, and the test asserts a second video frame is
delivered while the player is still opening (the whole test runs in ~7.6 s, nearly all of it
that HAL initialisation).

## 2026-09-14 — the smooth probe after the week's element work (main `c689d06`)

`SLOPTY_SMOOTH_E2E=1 cargo xtask e2e smooth` on an idle Mac Studio (no mutants, no build), the
display-stream scenario skipped (no screen-recording grant in this shell). Since the 2026-09-12
run at `becd27e` the element gained the sticky block header, the ⌘-hover link, the "took"
caption overlay and the coloured file card's run split; the file card scenarios (e)/(f) now
drop nothing where (f) had dropped 116 of 565 at `e931d97`, and (b)'s one drop is gone.

```
MEASURE (e) mac: 1 file card of 2 000 lines + 5 streaming shells, pan at zoom 1: draw 1.1 / 1.5 / 1.6 / 5.2 ms · every 3.9 / 14.1 ms · 882 frames, 0 over 16.7 ms, 0 dropped
MEASURE (f) mac: 1 file card of 2 000 lines + 5 streaming shells, zoom fit → 200 % → fit: draw 1.2 / 4.7 / 6.1 / 8.0 ms · every 4.8 / 12.7 ms · 883 frames, 0 over 16.7 ms, 0 dropped
MEASURE (a) mac: 20 streaming shells, pan at zoom 1: draw 0.8 / 1.1 / 3.3 / 9.3 ms · every 7.9 / 17.4 ms · 594 frames, 0 over 16.7 ms, 0 dropped
MEASURE (b) mac: 20 streaming shells, zoom fit → 200 % → fit: draw 0.7 / 1.7 / 2.8 / 10.2 ms · every 1.9 / 16.7 ms · 812 frames, 0 over 16.7 ms, 0 dropped
MEASURE frames while typing: draw 1.3 / 2.8 / 5.0 / 12.4 ms · every 7.4 / 64.1 ms · 190 frames, 0 over 16.7 ms, 0 dropped
MEASURE (d) mac, SLOPTY_PREDICT=never: echo 11.9 / 19.9 / 27.0 ms (60 keys) · predicted 0.0 / 0.0 / 0.0 ms (0 keys)
MEASURE frames while typing: draw 1.4 / 3.3 / 4.0 / 4.2 ms · every 6.9 / 63.0 ms · 190 frames, 0 over 16.7 ms, 0 dropped
MEASURE (d) mac, SLOPTY_PREDICT=always: echo 11.4 / 18.3 / 22.8 ms (60 keys) · predicted 2.4 / 3.8 / 4.3 ms (60 keys)
```

Nothing to optimise from this run: the p90 draw stays under 7 ms everywhere and the
prediction path still answers a key in under 5 ms at p90.

## 2026-09-14 — the capture guard on a healthy mesh link: the change is a no-op, as predicted

Setup: mac-studio hosting `display 6` (1920×1080 @1x 60 Hz) with a Ghostty window scrolling `seq`,
`slopty bench screen --display 6 --seconds 20` from macbook-pro over the WireGuard mesh, debug
build, direct path, MTU 1230. **The host ran under launchd** (`slopty worker install`), not from a
shell: a shell-spawned daemon is attributed to whatever launched it for TCC purposes, so it never
gets Screen Recording however the binary is signed. The link was in its best state yet — rtt 9.5 ms,
0 lost packets, 0 congestion events, the host's window reaching 12.5 MB.

Arm: `HELD_FLOOR` still in (the parent of `d58dfa0`).

| metric | value |
| --------------------------------- | ----------------------------- |
| frames decoded / fps              | 1155 in 19.96 s = 57.9 fps    |
| stalls                            | 0 (0 ms stalled)              |
| arrival gap p50 / p90 / max       | 17.1 / 21.0 / 45.9 ms         |
| worst gap, worst doze             | 0 ms, 0 ms                    |
| hold (client report) p50 / p95    | 2.1 / 3.9 ms                  |
| datagrams / lost / nacks / refresh| 11757 / 0 / 84 / 0            |
| host target                       | 13.5 → 30.0 Mbit/s by 10.1 s  |
| keyframe spread                   | 15.3 ms                       |

Host side, 1236 holds logged: `held_ms` p50 2, p90 3, worst 15; `max_bytes` worst 74 340 (the
keyframe), second 43 216; `cwnd` ranged 23 734 B to 12 465 747 B.

**What this settles.** The guard's limit is `max(per_frame × 2, cwnd)`, and the old rule was
`max(per_frame × 2, 32 768)`. On this link `per_frame × 2` dominates both floors at every sampled
moment: 56 250 B at the 13.5 Mbit/s the run opened with, 125 000 B at the 30 Mbit/s it reached,
against a `cwnd` that never fell below 23.7 kB and a fixed floor of 32 kB. Neither floor is ever the
larger term, so the two builds admit exactly the same frames and the arms are identical by
construction — which is what was predicted, and why the second arm adds nothing here. It is a
negative result worth the run: it shows the change is inert on a good path, so nothing was traded
away for the collapsed-path win.

**Still owed: the collapsed path.** The case the change exists for — `per_frame × 2` under one
window, which needs `rtt × fps` past 1.8 — did not occur, and cannot be conjured on a 9.5 ms link.
It needs a day like 2026-09-06 (25–30 % loss, the window pinned at its floor, the target cut to
1 Mbit/s) or a deliberately conditioned link. The prediction stands: `max_bytes` peaking near
`cwnd + one frame` instead of 32 kB + one frame, bought with more frequent short drops. Interleave
the arms when it happens — the mesh drifted monotonically over two hours on 09-06, so A-then-B
would read drift as effect. The second arm of this run was lost to macbook-pro leaving the mesh
mid-measurement, which is the other reason to interleave.

```sh
# iroh era: see "Reading old entries" at the top for today's flags
# mac-studio: the worker must be the launchd one, or TCC denies capture with -3801
cargo xtask sign && slopty worker install --direct-only --port 45560
# per arm: swap the binary under test into place, keep the identity, restart
cp <worker-under-test> ~/Library/Application\ Support/Slopty/bin/slopty-worker
cargo xtask sign            # re-sign the copy under dev.aislopware.slopty.worker
launchctl kickstart -k gui/$(id -u)/dev.aislopware.slopty.worker
slopty worker doctor          # must show two ticks before the arm counts
open -na Ghostty --args -e sh -c 'seq 1 400000000'          # motion on display 6
# macbook-pro
slopty bench screen --worker <id> --display 6 --seconds 20
# worker side
grep -E 'held_ms|keyframe encoded' ~/Library/Logs/Slopty/slopty-worker.log
```


## 2026-09-15 — the shaped ladder: a collapsed link on demand, and what it costs the stream

The run three rulings were waiting on. Setup: mac-studio alone, hosting `display 6`
(1920×1080 @1x 60 Hz, an idle desktop), debug build. The host is the installed one
(`slopty worker install --direct-only --port 45560 --bind 192.168.100.240`); the client is the
in-process one in `screen_over_a_shaped_link`, dialling through a `slopty-shape` relay per rung.
Both ends are pinned so the flow cannot leave the relay — the `selected` column of each row is
asserted, not hoped for. 8 s per rung, seed 1, `Quality::default()`.

`direct` has no relay at all and `clear` has one that shapes nothing: together they separate the
harness and the extra hop from the link. They agree to within 2 ms of first-decoded and 5 frames,
so the relay itself costs nothing readable.

| link      | shaper rate | loss  | first decoded | client hold max | decoded (8 s) | gap p50 / p90 / max | stalls | nacks | shaper down: sent / lost / overflowed | host target (Mbit/s, first → last) |
| --------- | ----------- | ----- | ------------- | --------------- | ------------- | ------------------- | ------ | ----- | ------------------------------------- | ---------------------------------- |
| direct    | —           | —     | 134 ms        | 12 ms           | 184           | 48.4 / 69.0 / 84.3 ms  | 0   | 0     | —                                     | 13.5 → 30.0, every step Grow       |
| clear     | unlimited   | 0 %   | 136 ms        | 6 ms            | 187           | 48.9 / 67.4 / 90.5 ms  | 0   | 1     | 1165 / 0 / 0                          | 13.5 → 30.0, every step Grow       |
| wifi      | 4 000 kB/s  | 0.2 % | 241 ms        | 56 ms           | 182           | 49.6 / 67.3 / 84.9 ms  | 0   | 2     | 1441 / 1 / 0                          | 12.0 → 30.0 by 6.8 s               |
| lte       | 1 500 kB/s  | 1.0 % | 531 ms        | 278 ms          | 158           | 54.6 / 74.0 / 282.0 ms | 0   | 4     | 1526 / 15 / 0                         | 9.0 Cut → 6.8 → regrew to 9.1      |
| collapsed | 600 kB/s    | 3.0 % | 645 ms        | 248 ms          | 52            | 141.7 / 277.3 / 341.4 ms | 1 | 10    | 2651 / 78 / 1                         | 7.3 → cut to the 1.0 floor by 5 s  |

Host side, 738 hold episodes across the five rungs: `held_ms` p50 2, p90 3, **worst 4 796 ms**
holding **435 274 B** with `cwnd` at 260 799 B; second worst 771 ms holding 306 108 B at a
`cwnd` of 259 925 B. `cwnd` ranged 6 127 B to 441 214 B over the run. Capture guard per rung
(captured / dropped / encoded): direct 184/0/184, clear 188/0/188, wifi 188/4/184, lte
188/24/164, collapsed 202/99/58.

**What this settles.** The collapsed rung is the day the capture-guard ruling said it needed and
could not conjure on a 9.5 ms link: the rate controller walks 7.3 → 1.0 Mbit/s in seven `Cut`
verdicts and stays pinned at the floor, the guard drops 99 of 202 captured frames, and the client
gets 52 frames in 8 s with a p90 arrival gap of 277 ms. The ⏸ keyframe rule asked one question —
whether the hold after a keyframe is still hundreds of milliseconds once the budget is
`max(per_frame × 2, cwnd)` — and the answer is worse than the question assumed: **4.8 seconds**,
with 435 kB held.

**Read `max_bytes` as what it is.** It is peak bytes held in QUIC's send buffer — the standing
queue, the admitted frame, and that frame's parity together — not one frame's size. The host's own
log says so: the encoder produced 34 keyframes across the whole ladder and the largest was
**133 960 B**, the range 87–134 kB. So 261 kB of window plus a 134 kB keyframe plus parity (and at
3 % loss the redundancy controller is not cheap) accounts for the 435 kB without any frame being
anywhere near that big. An earlier reading of this paragraph took the 435 kB for the keyframe and
concluded no budget could have reached it; that inference is withdrawn in
`docs/decisions/transport.md`. The rule still stands on the real number — a 134 kB keyframe is
1.07 s of a 1 Mbit/s link in one frame, and `frame_fits` runs before the encode so it never sees
it.

**Two harness defects found on the way, and both had the same cause.** The first rungs read zero
decoded frames, and the decoder warm-up took 28.4 / 28.7 / 31.0 s where 2026-09-05 measured
150–400 ms. That is not the decoder: the same test binary takes 118 s run from this repo's
external volume and 0.56 s run from `/tmp`, unmodified. The cause is the mount flag: a disk image
*created on that same external disk* and attached with `-owners on` runs it in 0.50 s, while
`/Volumes/Lacie` is mounted `noowners` and so misses the cached code-signature path
(`decisions/testing.md`). Every VideoToolbox test in the suite pays it, and the fix is one command:
`sudo diskutil enableOwnership /Volumes/Lacie`.

The harness keeps two changes anyway, both right on their own terms: it waits for
`slopty_codec::warm_up()` before the first rung rather than racing it, and throws the first
sample away. On the boot volume they cost about a second.

Also worth knowing: `slopty worker install` copies the binary, and the copy loses its Screen
Recording grant unless `cargo xtask sign` runs immediately before each install.

```sh
# iroh era: see "Reading old entries" at the top for today's flags
# the worker must be the installed one: TCC attributes a shell-spawned daemon to whatever launched
# it, so a test that spawns its own is refused capture with -3801 however the binary is signed
cargo xtask sign && slopty worker install --direct-only --port 45560 --bind <lan-ip> \
  --log 'info,slopty_worker=debug'
slopty worker doctor          # two ticks before the run counts
SLOPTY_E2E_WORKER_SOCKET="$HOME/Library/Application Support/Slopty/run/worker.sock" \
  SLOPTY_E2E_SECONDS=8 RUST_LOG=info,slopty_client=debug,slopty_codec=debug \
  cargo nextest run -p slopty-workerd --test e2e -E 'test(screen_over_a_shaped_link)' --no-capture
grep -E '^\| ' /tmp/ladder.log                                   # the table
perl -pe 's/\e\[[0-9;]*m//g' ~/Library/Logs/Slopty/slopty-worker.log \
  | perl -ne 'print "$1 $2 $3\n" if /held_ms=(\d+) max_bytes=(\d+) cwnd=(\d+)/'
cargo xtask sign && slopty worker install --direct-only --port 45560   # put the worker back
```

## 2026-09-15 — the shaped ladder again, against the keyframe rule

Same rig, same shaper seed, same 8 s per rung as the run above; the host is the installed one
pinned to the LAN address. The point of the run was to put a number on the keyframe-admission rule
(`docs/decisions/transport.md`). It produced a better one — and the rule had nothing to do with it.

| link      | rate       | loss  | first decoded | hold max | decoded | gap p50 / p90 / max      | nacks | shaper down sent/lost/ovf |
| --------- | ---------- | ----- | ------------- | -------- | ------- | ------------------------ | ----- | ------------------------- |
| direct    | —          | —     | 136 ms        | 10 ms    | 189     | 48.6 / 66.5 / 83.5 ms    | 0     | —                         |
| clear     | ∞          | 0 %   | 135 ms        | 13 ms    | 190     | 47.7 / 67.1 / 87.0 ms    | 0     | 1139 / 0 / 0              |
| wifi      | 4000 kB/s  | 0.2 % | 239 ms        | 57 ms    | 185     | 49.4 / 67.3 / 84.5 ms    | 1     | 1370 / 1 / 0              |
| lte       | 1500 kB/s  | 1.0 % | 523 ms        | 186 ms   | 164     | 49.6 / 71.4 / 213.3 ms   | 4     | 1500 / 15 / 0             |
| collapsed | 600 kB/s   | 3.0 % | 662 ms        | 276 ms   | 70      | 97.1 / 242.2 / 281.3 ms  | 7     | 2290 / 68 / 0             |

**`keyframes_deferred` was 0 on every rung.** The host logged no deferral at all, so the gated path
never executed. The collapsed rung nevertheless decoded 70 frames against the previous run's 52,
with p50 gap 97 ms against 142 ms and max 281 ms against 341 ms. **None of that is the rule** — the
code it added did not run. It is variance between two runs over content this harness does not
control, and it is recorded here so nobody later reads the pair of tables as a before/after.

**Why it never fired, from the host's own log.** The encoder runs an infinite GOP
(`MaxKeyFrameInterval` = `i32::MAX`), so every keyframe is requested rather than periodic — and the
run still produced 28 of them, 20 524 B to 123 990 B. They cannot have come from the gate's input:
`pending.keyframe` is set only when a stream opens and when a quality change rebuilds the encoder.
They came from `force_ltr_refresh`, which the guard sets on every dropped frame (99 of 202 captures
on the collapsed rung) and which VideoToolbox answers with an IDR when it has no acknowledged
long-term reference to work from.

**Keyframes track the target bitrate.** Sizes fell with the rate controller through the collapsed
rung — 123 990, 114 368, 105 439, 80 705, 67 566, 46 935, 42 522, 20 524 B — ending at a sixth of
where they started once the controller pinned at the 1 Mbit/s floor. The earlier claim that the
encoder sizes a keyframe without regard for what the path can carry is wrong in that half.

```sh
# as the run above, then:
grep -c "keyframe deferred" ~/Library/Logs/Slopty/slopty-worker.log     # the rule's own log line
perl -pe 's/\e\[[0-9;]*m//g' ~/Library/Logs/Slopty/slopty-worker.log \
  | perl -ne 'print "$1 $2\n" if /^(\S+).*bytes=(\d+) keyframe=true/'  # sizes with timestamps
```

## 2026-09-15 — why a refresh costs a keyframe: the LTR reference is starved

Third shaped-ladder run, same rig, with `ltr_acked` reported per stream and the first
acknowledgement logged. It was run to decide between two explanations for the 28 IDRs of the run
above: the client never acknowledges a long-term reference, or it does and the encoder ignores it.
Neither is right.

| link      | rate      | loss  | first decoded | hold max | decoded | gap p50 / p90 / max     | nacks |
| --------- | --------- | ----- | ------------- | -------- | ------- | ----------------------- | ----- |
| direct    | —         | —     | 134 ms        | 4 ms     | 196     | 47.9 / 61.2 / 78.6 ms   | 1     |
| clear     | ∞         | 0 %   | 139 ms        | 12 ms    | 193     | 47.6 / 63.7 / 82.0 ms   | 0     |
| wifi      | 4000 kB/s | 0.2 % | 248 ms        | 55 ms    | 187     | 48.4 / 64.7 / 81.9 ms   | 12    |
| lte       | 1500 kB/s | 1.0 % | 517 ms        | 175 ms   | 155     | 50.6 / 73.0 / 154.3 ms  | 3     |
| collapsed | 600 kB/s  | 3.0 % | 663 ms        | 256 ms   | 54      | 128.4 / 278.7 / 343.5 ms | 11    |

**Every stream acknowledged a reference, and every one acknowledged exactly one.** Six streams
(five rungs plus the discarded start-up sample), six `first LTR ack` lines, `tokens=1` on all of
them, each within the first second. So the acknowledgement path works end to end — the client's
`ack_ltr` reaches the host and sets `ltr_acked`.

**And the run still produced 33 keyframes.** Only two places set `pending.keyframe` (a stream
opening, a quality change rebuilding the encoder), which accounts for at most one or two per
stream. The rest can only be `force_ltr_refresh` falling back to an IDR — with an acknowledged
reference already on record.

**So the mechanism is starved, not broken.** VideoToolbox offers tokens sparsely: one per fifteen
frames in `a_forced_ltr_refresh_is_a_delta_not_an_idr`, and one per eight-second stream here. A
refresh taken immediately off a fresh reference is 733 B against a 3 998 B IDR; a refresh asked for
seconds later, with that single reference long overtaken, is answered with a keyframe. The guard
asks on every dropped frame, so the collapsed rung — 99 of 202 captures dropped — spends its whole
budget re-requesting keyframes from a link that cannot carry one.

That reframes the rule. It is not "defer a keyframe for a refresh": a refresh *is* a keyframe
whenever the reference behind it has gone stale, and `ltr_acked` does not distinguish the two
because it only records that an acknowledgement once happened. What the guard needs is either more
live references, or a gate on the refresh request itself.

```sh
# as the runs above, then:
perl -pe 's/\e\[[0-9;]*m//g' ~/Library/Logs/Slopty/slopty-worker.log | grep "first LTR ack"
perl -pe 's/\e\[[0-9;]*m//g' ~/Library/Logs/Slopty/slopty-worker.log \
  | grep -c "bytes=[0-9]* keyframe=true"
```

Harness note: the first attempt at this run failed at `e2e.rs:826`, a timeout waiting for the
discarded sample's `Closed` event, and passed unchanged on the next run. Not investigated; if it
recurs, the log line to read first is noq's `failed closing path err=LastOpenPath`, which appeared
17 s into the stalled stream.

## 2026-09-15 — the drain budget on the refresh request: three runs against one

Same rig, same shaper seed, same 8 s per rung, same installed host pinned to the LAN address as
the two ladders above. The change under test is `90cf9b9`: `Shared::dropped` puts the refresh a
dropped frame asks for through `keyframe_admitted` instead of setting it unconditionally. The
baseline is the run directly above, which is the last one before the change and uses the same
harness.

Three runs, because the collapsed rung is the noisy one — its own baseline notes record it moving
52 → 70 decoded with no code change at all, so a single run proves nothing here.

| collapsed rung        | baseline | run 1 | run 2 | run 3 |
| --------------------- | -------- | ----- | ----- | ----- |
| decoded in 8 s        | 70       | 109   | 116   | 95    |
| arrival gap p50       | 97.1 ms  | 65.0  | 57.6  | 81.9  |
| arrival gap p90       | 242.2 ms | 118.5 | 120.7 | 129.2 |
| arrival gap max       | 281.3 ms | 412.5 | 308.5 | 272.8 |
| guard drops / capture | 99 / 202 | 47 / 195 | 45 / — | 48 / — |
| client hold max       | 276 ms   | 276   | 238   | 268   |
| shaper down sent      | 2290     | 1499  | 1453  | 1472  |
| shaper down lost      | 68       | 36    | 35    | 35    |
| nacks                 | 7        | 4     | 8     | 6     |

Host side, whole ladder: keyframes **28 → 20, 21, 20**; `keyframes_deferred` **0 → 4, 1, 3**,
landing on `lte` and `collapsed` and nowhere else. Hold episodes over runs 2 and 3 together
(1 577 of them): `held_ms` p50 2, p90 6, **max 1 708 ms** holding 379 444 B, against the
baseline's p50 2, p90 3, **max 4 796 ms** holding 435 274 B. Run 1 alone read p90 20 ms, which
the next two runs do not reproduce.

**What holds across all three.** p90 arrival gap roughly halves and stays inside 118–129 ms where
the baseline was 242. The guard drops about half as many frames (45–48 against 99) because the
queue it tests is no longer full of keyframes. About 35% fewer packets are pushed into a 600 kB/s
pipe and about half as many are lost. The worst hold falls 2.8×. And `keyframes_deferred` is
non-zero for the first time: the keyframe rule read 0 on every rung of three earlier ladders, so
this is the new path executing rather than variance.

**What does not hold, and is not claimed.** `first decoded` on the collapsed rung reads 977, 647
and 928 ms against a 662 ms baseline — it straddles the baseline and is not a regression, but
neither is it evidence of anything. Run 1's decoder warm-up took 37.5 s (the `noowners` tax,
situational as the ruling above says) and its two worst rungs were the two measured under that
contention. `gap max` is likewise noisy in both directions. The claim is the p90 and the drop
count, not the tail of the tail.

**A harness race, found and fixed.** Two runs in three died at `e2e.rs:255` with `ENOTCONN` while
minting the pairing ticket for the fifth rung. The worker was never down — `up 471 s`, no panic. It
answers a control request and closes without waiting to be half-closed, so the test's
`wr.shutdown()` races that close and macOS reports `ENOTCONN` when it loses. The reply is already
buffered, so the shutdown now tolerates that one error kind and `read_line` decides whether a
reply arrived. Distinct from the `LastOpenPath` flake noted above.

```sh
# as the ladder runs above; then, per run:
perl -pe 's/\e\[[0-9;]*m//g' ~/Library/Logs/Slopty/slopty-worker.log \
  | grep -c "stands on its own is deferred"          # deferral episodes
perl -pe 's/\e\[[0-9;]*m//g' ~/Library/Logs/Slopty/slopty-worker.log \
  | grep -oE "keyframes_deferred: [0-9]+|dropped: [0-9]+"   # per-rung counters
```

## 2026-09-24 — the word cache on a return, and the hash behind its keys

**Words shaped when a screen comes back.** `terminal::view::tests::a_screen_shown_again_shapes_nothing`
draws a 100 × 40 grid of prose (A), another (B) for three frames, then A again, and counts the
words shaped on the return (the element's `cfg(test)` shape counter). Headless GPUI runs on
`NoopTextSystem`, so its draw times say nothing about CoreText; the count is the number that
transfers.

| tree | words shaped on the return | cache after |
| --- | --- | --- |
| main `a88e2e3` (forget after two frames unused) | **574** (all of A) | 1144 |
| LRU under a 2¹⁸-glyph budget | **0** | 1144 |

The same test printed draw times of 0.90 ms (return, before) and 0.25 ms (return, after) on the
no-op shaper. That shows the reshaping was the return frame's whole extra cost, but it is not a
CoreText figure. The end-to-end `cargo xtask e2e smooth` rerun (the 44–65 ms zoom p99s) is still
owed. This session could not build the app, because other crates in the shared checkout were
mid-change.

**Key hash.** A throwaway release-build loop hashed 20 000 words the way `segment_hash` does:
per cell, the text, a 13-byte style and the width, 200 passes, five rounds, on mac-studio. The
table gives ns per word, taking the last three rounds after warm-up.

| hasher | ns / word |
| --- | --- |
| `std` SipHash (`DefaultHasher`, the old key) | 176–192 |
| `foldhash::fast` 0.2.0 | 57–68 |
| `foldhash::quality` 0.2.0 | 67–77 |
| `rustc-hash` 2.1.3 `FxHasher` | **46–56** |

FxHash wins on this pattern of many small writes, so the keys use `rustc-hash`. The map holds
the key's own 64-bit hash, so a collision paints the wrong word; that needs 2⁶⁴-scale luck,
exactly as it did with the fixed-key SipHash before.

```sh
cargo nextest run -p slopty-ui --no-capture -E 'test(a_screen_shown_again_shapes_nothing)' | grep MEASURE
# /tmp/hashbench (Cargo.toml: foldhash =0.2.0, rustc-hash =2.1.3; src/main.rs hashes the words
# as segment_hash does, per hasher, and prints ns/word)
cd /tmp/hashbench && cargo run --release --offline -q
```

## 2026-09-24 — the host hot paths: ptyd transfer, repeated search, the write path

Release, mac-studio, a busy machine (load average 33–48, other agents building). "Before" is
main `a88e2e3` exported with `git archive` to `/tmp/slopty-base`, with `vendor/ghostty` at
`5252b193c` and the same ptyd measurement test added. "After" is the working tree: ghostty
`7c40388b2`, fdpass reading into a scratch buffer kept per connection, 1 MiB socket buffers,
the search text cached per terminal generation, the colour check only after an OSC or RIS, the
anchor kept while nothing scrolls, and the memchr scanners. Each tree ran the same command
three times, alternating:

```
cargo nextest run -p slopty-engine -p slopty-ptyd --release --run-ignored only \
  -E 'test(frame_cost) | test(checkpoint_cost) | test(search_cost) | test(checkpoint_transfer_cost)' \
  --no-capture
```

| measurement | before (3 runs) | after (3 runs) |
| --- | --- | --- |
| 4 MiB `Checkpoint` to ptyd, then `List`, p50 | 137 / 115 / 122 ms | **14 / 11 / 18 ms** |
| same, max of 20 | 248 / 275 / 249 ms | 87 / 38 / 138 ms |
| search a 50 001-line history again, nothing written between, plain p50 | 70 / 16 / 37 ms | **11 / 6.1 / 6.0 ms** |
| same, regex p50 | 55 / 16 / 16 ms | 5.6 / 5.6 / 5.6 ms |
| format the plain text (the first search's extra cost) p50 | 55 / 10.3 / 10.4 ms | 10.5 / 10.3 / 10.4 ms |
| one typed byte: `write` p50 / p90 / max | 1/2/38, 0/0/1, 1/2/59 µs | 0/0/0, 0/0/0, 0/0/2 µs |
| one typed byte: `take_frame` p50 / max | 5/66, 2/25, 5/75 µs | 2/26, 2/25, 2/25 µs |
| 10 024 coloured lines through the engine (raw `vt_write` 1.6 ms) | 3.2 / 3.6 / 3.3 ms | 2.8 / 3.6 / 3.6 ms |

The ptyd transfer is the fdpass fix. Each `recvmsg` used to zero a scratch `Vec` as large as
the frame still owed (`try_decode` reserves the whole frame), through an 8 KiB socket buffer:
about 430 reads, each zeroing up to 4 MiB. Repeated search is the cache: a find bar searches
on every keystroke, and each of those searches formatted the whole history again. The single
byte's `write` lost the colour check (eight FFI reads and two 256-entry palette copies) and
the tracked-pin churn. The 10k-line fill is inside this machine's noise in both trees. It has
no OSC and ten chunks, so the per-chunk savings do not show. The number that says whether the
scanners' memchr path matters on escape-dense output is still owed, from a quiet machine.

## 2026-09-24 — the media path's copies, the audio backlog, the still pointer

Release, mac-studio. Each "before" was run on the unchanged code with the same test before the
change went in. `docs/decisions/video.md` and `audio.md` (2026-09-24) hold the rulings.

```
cargo nextest run -p slopty-codec --release --run-ignored only -E 'test(cost)' --no-capture
cargo nextest run -p slopty-media --release --run-ignored only packetize_cost --no-capture
cargo nextest run -p slopty-codec --release a_stall_burst --no-capture
cargo nextest run -p slopty-capture --release --run-ignored only pointer_read_cost --no-capture
```

**Host, VideoToolbox's output to an Annex B packet** (`packet_conversion_cost`, four slices per
access unit, on the encoder's callback thread):

| access unit | before | after |
| --- | --- | --- |
| 62 KB P-frame | 10.1 µs | 1.2 µs |
| 300 KB keyframe | 24.2 µs | 5.3 µs |

**Host, packetize** (`packetize_cost`, 2 000 frames each; the history's eviction included):

| frame | parity | before | after |
| --- | --- | --- | --- |
| 62 KB P-frame, 64 datagrams | 200‰ | 23.5 µs | 17.4 µs |
| 62 KB P-frame, 53 datagrams | 0 | 8.1 µs | 2.5 µs |
| 300 KB keyframe, 305 datagrams | 200‰ | 131.5 µs | 131.6 µs |
| 300 KB keyframe, 254 datagrams | 0 | 35.0 µs | 12.7 µs |

Reed–Solomon dominates a parity keyframe either way; what went is the per-fragment allocation
and the second copy.

**Client, a received access unit to a sample buffer** (`access_unit_conversion_cost`, a 1 MB
keyframe with its parameter sets): 4.84 ms before (two byte-by-byte scans, a copy into a vector,
a copy into the block buffer), **60 µs** after (one `memchr` scan, one write into the block).

**Audio, how far behind the host the listener is** (`a_stall_burst_does_not_leave_lasting_delay`,
a model driven through the ring's own `push`/`pull` on a 1 ms clock: steady 20 ms packets, a
250 ms stall, the held packets in one burst; the ring plus the device buffers queued ahead of
it):

| | after the burst | 1.25 s later |
| --- | --- | --- |
| before: 200 ms cap, 3 × 20 ms buffers | 260 ms | 260 ms |
| after: trim to 40 ms past 50 ms, 3 × 10 ms buffers | 70 ms | 74 ms mean over 1.5–3 s |

This is a model of the ring, not a reading at the DAC; the device's own output latency comes on
top of both rows alike.

**Host, one look at the pointer** (`pointer_read_cost`, 2 000 calls): `CGEventCreate` +
location 13.8 µs, which the cursor loop paid through a `spawn_blocking` hop 120 times a second
per open window; the four move/drag counters (`CGEventSourceCounterForEventType`) 43 ns, read
inline. The pointer itself is now asked for only when the counters moved.

## 2026-09-24 — plaintext QUIC on noq against iroh: connect time and control-stream round trip

Setup: mac-studio, debug builds, `slopty-ptyd` + `slopty-worker` on port 45597 with private
sockets and data dirs, `slopty ping --count 20` five times per row. The machine was busy (load
average ~20: other sessions compiling) for both builds alike, so the tails are noisy; the
medians are the claim. "Connected in" is new in `slopty ping`: from before the client endpoint
is bound to the host's `HelloAck`, so it includes the bind. The iroh rows dialled the ticket's
addresses, which iroh resolved to the LAN address (`192.168.100.240`); the noq rows dial the
address given.

| build, path                              | connected in (5 runs)            | app rtt median (5 runs)        | QUIC rtt   |
| ---------------------------------------- | -------------------------------- | ------------------------------ | ---------- |
| iroh 1.2.0, `Anywhere` (relays on)       | 44 208 (cold), 29, 33, 34, 33 ms | 1.49, 1.04, 0.87, 1.16, 0.93 ms | 2.2–2.9 ms |
| iroh 1.2.0, `DirectOnly`                 | 22, 31, 28, 32, 27 ms            | 1.15, 1.23, 1.29, 1.27, 1.16 ms | 1.7–2.5 ms |
| noq, plaintext, `127.0.0.1`              | 42, 44, 6.9, 6.6, 8.6 ms         | 0.50, 0.48, 0.34, 0.35, 0.47 ms | 1.1–2.5 ms |
| noq, plaintext, `192.168.100.240` (LAN)  | 12, 13, 14, 10, 11 ms            | 0.60, 0.53, 0.46, 0.50, 0.63 ms | 1.0–2.4 ms |
| noq, plaintext, `100.64.0.3` (tailnet IP) | 14, 15, 7.2, 7.1, 7.8 ms         | 1.13, 0.68, 0.63, 0.66, 0.67 ms | 1.7–4.3 ms |

Reading: on the same LAN address the median round trip halves (≈1.2 ms → ≈0.55 ms) and a
connection is up in a third of the time (≈30 ms → ≈11 ms); on loopback the steady connect is
7–9 ms. The first iroh `Anywhere` connect took 44 s (the relay and discovery warming while the
ticket's direct address waited); nothing in the noq path can wait on a third party. The two
40 ms loopback connects are the first two runs after the worker started, not a steady cost.

```sh
# common: R=/tmp/slopty-meas; SLOPTY_PTYD_SOCKET=$R/ptyd.sock SLOPTY_WORKER_SOCKET=$R/worker.sock
#         SLOPTY_PORT=45597 SLOPTY_NO_SHELL_INTEGRATION=1; slopty-ptyd running
# before (main a88e2e3 plus the "connected in" line; add SLOPTY_DIRECT_ONLY=1 for that row):
SLOPTY_DATA_DIR=$R/worker slopty-worker --print-ticket > $R/ticket &
SLOPTY_DATA_DIR=$R/client slopty pair "$(head -1 $R/ticket)"
SLOPTY_DATA_DIR=$R/client slopty ping --count 20          # ×5
# after (this branch):
SLOPTY_DATA_DIR=$R/worker slopty-worker --print-addr &
SLOPTY_DATA_DIR=$R/client slopty ping --worker 127.0.0.1:45597 --count 20   # ×5, then the LAN and tailnet IPs
```

Not measured: the shaped screen ladder (`screen_over_a_shaped_link`). It runs only against a
launchd-installed host (TCC grants capture to nothing else), and installing this branch's
daemons would have replaced the host the user runs. BBR3 against Cubic, and a 2 MB Cubic
initial window, stay unmeasured on this transport; the tuning carried over unchanged.

## 2026-09-25 — the port hint on the PTY read path

The session actor now asks every PTY read whether it names a local server
(`slopty_worker::ports::mentions_local_server`: `localhost:`, `127.0.0.1:`, `0.0.0.0:` or `[::1]:`
and a port, or an OSC 8 link to a web address), so the worker can scan that session's listening
ports. After a hit the session skips the check for a second, so the cost that matters is the
miss, which every read pays. Release, mac-studio, three runs of 2 000 checks each:

```
cargo test -p slopty-worker --release --lib -- --ignored local_server_scan_cost --nocapture
```

| read | p50 (3 runs) | p99 (3 runs) |
| --- | --- | --- |
| one echoed keystroke (1 byte) | 0 / 0 / 0 ns | 42 / 42 / 42 ns |
| 64 KiB of coloured `cargo build` lines | 7.2 / 7.0 / 7.2 µs | 7.8 / 7.5 / 7.8 µs |
| 64 KiB of plain text with digits and colons | 11.1 / 11.1 / 11.1 µs | 11.8 / 12.0 / 11.9 µs |

A keystroke's echo pays nothing measurable. A full 64 KiB read in a flood pays 7 to 11 µs,
a few percent of what the engine spends on the same read (10 024 coloured lines, about 600 KiB,
take 2.8 ms, see "the host hot paths" above).

## 2026-09-25 — submit to glass: the drawable queue a vsync-driven window keeps (zed fork)

`Window::on_frame_presented` (zed fork bd43a00) reports when each frame reached the display.
With it, a window that animates continuously measured about 3 refreshes from submit to glass.
This display runs at 75 Hz, 13.3 ms per refresh. `CAMetalLayer` presents drawables in order,
and a window that draws once per vsync keeps any queue it ever builds: one late tick, a slow
first frame or a compositor hiccup adds a frame of queue, and every later frame waits a
refresh behind it. The fork's fix (22e4c6a) is a pacer in `gpui_apple`. It skips one display
link tick when frames reach the glass a refresh later than the pipeline's recent floor, and it
never starts two frames within half a refresh. An idle macOS window marked dirty now draws on
the next main-queue turn instead of the next vsync, as iOS already did.

The rows below are release-fast, mac-studio, with a load average of 7–10 from other sessions,
900 refreshes a run, in milliseconds:

```
cd .research/zed-main && MACOSX_DEPLOYMENT_TARGET=26.5 IPHONEOS_DEPLOYMENT_TARGET= \
  cargo run -p gpui --example frame_latency --profile release-fast -- continuous|heavy|idle
```

| case | before p50 / p99 | after p50 / p99 | repeated refreshes before → after |
| --- | --- | --- | --- |
| continuous | 31.1 / 33.1 | 18.1 / 32.3 | 0–4 → 8–9 (the pacer's skips) |
| heavy (≈ 9 ms of work per frame) | 22.6 / 25.0 | 22.7 / 25.1 | 0 → 0 |
| idle, notify → glass | 26 / 32–44 | 20–22 / 28–29 | — |

No frame was dropped in any run. The floor that remains, about 17.8 ms (1.3 refreshes), is the
macOS compositor. Measured and rejected:
- `maximumDrawableCount = 2`: p50 24.7, still queued, and able to block the main thread.
- `CAMetalDisplayLink` with frame latency 1 or 2: p50 39.6.
- `displaySyncEnabled = NO`: it reported 5.7 ms, but the timestamp no longer means the glass,
  and motion repeated 153 refreshes.

The iOS side runs the same pacer and only builds so far; it has no number yet.

## 2026-09-25 — echo behind slow requests

Keystroke echo on a `/bin/cat` session while the same connection keeps the worker busy: quick open
walking a 20 000-file tree and a window stream opening, back to back, for as long as the typing
lasts. Each key goes out after the last one's echo came back. Release, mac-studio. Screen
Recording is not granted to a worker this test spawns (`-3801`, docs/DEV.md "TCC"), so each
window open fails at ScreenCaptureKit (≈ 9 ms on its own) instead of opening the idle window;
where the grant exists the same test opens and closes the `slopty-idle-window` window.

```
cargo test -p slopty-workerd --release --test e2e echo_is_not_held_behind_slow_requests -- --nocapture
```

| | echo idle p50 / p99 / max | echo under load p50 / p99 / max | quick open p50 | load avg |
| --- | --- | --- | --- | --- |
| before (one run) | 3.01 / 4.36 / 4.42 ms | **28.68 / 33.31 / 69.60 ms** | 27.4 ms | ≈ 6 |
| after, run 1 | 2.78 / 4.11 / 4.31 ms | **2.81 / 4.79 / 7.18 ms** | 28.9 ms | ≈ 16 |
| after, run 2 | 2.91 / 5.09 / 6.06 ms | 2.77 / 5.25 / 8.12 ms | 28.1 ms | ≈ 16 |

Before, a key typed during a quick open waited out the walk: the echo's median under load was
the walk's median. After, the load leaves the echo where it is idle. The test fails when the
loaded p99 reaches half a quick open, which the old loop does on every run.

## 2026-09-25 — the geometry tick off the connection

What one geometry tick of a window stream costs in window-server calls, against the idle window
(`slopty-idle-window`, found by owner pid; the window list needs no Screen Recording). Before, a
tick read bounds, on-screen state and owner as three window-list descriptions, and the cursor
loop read the bounds again at the same 10 Hz. After, one description answers all three and the
cursor loop reads what the probe left. Both include the occlusion list and the display lookup.
Release, mac-studio, 2 000 ticks a run, three runs:

```
cargo test -p slopty-capture --release --test geometry -- --ignored geometry_tick_reads --nocapture
```

| | p50 | p99 | max |
| --- | --- | --- | --- |
| separate reads | 551 / 513 / 513 µs | 3 270 / 2 805 / 2 782 µs | 15.2 / 6.1 / 6.8 ms |
| one description | **264 / 242 / 244 µs** | **541 / 537 / 552 µs** | 3.3 / 3.6 / 3.2 ms |

At 10 Hz that is about 5 ms of window-server time a second per open window before and 2.5 ms
after. It also no longer runs on the connection's task: the probe runs on the blocking pool from
the stream's own task, beside that stream's input. Not measured: the worker's CPU per second with one
idle window stream open. It needs a capture, and a worker this session can start has no Screen
Recording grant; installing one under launchd would replace the host the user runs.

## 2026-09-25 — datagrams from the encoder's thread

64 datagrams of 1 150 B per frame (a 62 KB P-frame at 30 Mbit/s), 300 frames at 60 fps,
produced on a plain thread as VideoToolbox's callback produces them, sent over loopback QUIC to
a client in the same process. "Pump" is the path this replaced, kept as the test's twin: a
4 096-slot channel into a task that reads the send buffer and the datagram size beside every
`send_datagram` and polls at 1 kHz while QUIC holds bytes. "Direct" is `QuicSink`, one
`send_many_datagrams` per frame. CPU is the whole process (both ends of the connection) per
frame; the arrival is from a frame's first stamp to its last datagram at the client. Release,
mac-studio, load average 8–19 from other sessions, five alternating pairs:

```
cargo test -p slopty-workerd --release --bin slopty-worker datagram_send_cost -- --ignored --nocapture
```

| pair | pump cpu / frame | direct cpu / frame | pump arrival p50 / p99 | direct arrival p50 / p99 |
| --- | --- | --- | --- | --- |
| 1 | 3.43 ms | 2.47 ms | 2.06 / 7.5 ms | 1.60 / 9.9 ms |
| 2 | 3.33 ms | 2.17 ms | 0.86 / 1.5 ms | 0.81 / 1.2 ms |
| 3 | 2.77 ms | 2.77 ms | 0.84 / 0.95 ms | 0.84 / 6.9 ms |
| 4 | 4.63 ms | 2.30 ms | 1.51 / 13.7 ms | 0.96 / 40.0 ms |
| 5 | 3.50 ms | 3.47 ms | 0.82 / 18.3 ms | 1.59 / 10.3 ms |

Median CPU per frame 3.43 → 2.47 ms. The arrival does not move measurably on loopback, where
the pump was seldom behind (median p50 0.86 against 0.96 ms); the tails in both columns are this
machine's load, not either path. What the change removes that no loopback run shows is the
pump's own failure: any send error, `TooLarge` included, ended the connection's media, where a
datagram cut for a stale size now costs only itself (`a_too_large_datagram_costs_itself_and_a_closed_link_takes_nothing`).

## 2026-09-25 — the editor's highlighter: a frame and a keystroke

The file tile's body is gpui-kit's code editor with our syntect highlighter
(`slopty_ui::highlight::editor`). What it costs on the UI thread, on a 2 000-line Rust file
(the worker's read cap): styling the 60 rows a tile shows, which the editor asks for every
frame it paints, and moving the colours below a typed newline (`editor::splice`), which runs
on every edit. The full parse stays off the UI thread (2026-09-12: ≈ 165 ms for 2 000 lines)
and starts 60 ms after the typing stops. Release, mac-studio, two runs (the second at a load
average of 8.7 from other sessions):

```
cargo test -p slopty-ui --release --lib timing_of_a_frame_and_a_keystroke -- --ignored --nocapture
```

| run | 60 rows styled, per frame | a newline spliced, per keystroke |
| --- | --- | --- |
| 1 | 16.2 µs | 10.6 µs |
| 2 | 18.4 µs | 11.0 µs |

Both are two to three orders under a 16.7 ms frame. Most of the splice is cloning the
2 000 per-line handles (`Arc<[Span]>`); a keystroke on a line recolours that line only when the
next background parse lands.

## 2026-09-25 — the view cache under streaming shells, and keystrokes timed at the glass

The e2e app build (debug profile, `opt-level = 1`), mac-studio, window 1280 × 800, with a load
average of 4–9 from other sessions. The new smooth scenario
`six_streaming_shells_in_view_on_the_mac` measures three things. **(g)** six shells flooding
in the overview while nothing moves, 5 s. **(h)** 60 keys typed into a seventh shell with the
six flooding beside it. **(i)** one flooding shell beside five still ones (a screen of `seq`,
then `sleep`), in the overview, 5 s. Draw is the frame probe's `begin` → `end` (ms, p50 / p95 /
p99 / max). Echo is the key → display meter (ms, p50 / p95 / p99). With 60 keys the nearest-rank
p99 is the worst key.

```
cargo build -p slopty-ptyd -p slopty-workerd -p slopty-serverd -p slopty-cli -p slopty -p slopty-e2e --bins --features slopty/e2e
D=$PWD/target/e2e-perf; mkdir -p $D/run $D/artifacts
SLOPTY_DATA_DIR=$D SLOPTY_PTYD_SOCKET=$D/run/ptyd.sock SLOPTY_WORKER_SOCKET=$D/run/worker.sock \
  SLOPTY_E2E_BIN_DIR=$PWD/target/debug SLOPTY_E2E_ARTIFACTS=$D/artifacts SLOPTY_SMOOTH_E2E=1 \
  cargo nextest run -p slopty-e2e --test smooth --no-capture -E 'test(six_streaming) | test(typing_is_timed)'
# the "cache off" arm: `WorkspaceView::cacheable` (workspace/tile.rs) returning false
```

**The view cache.** Tile bodies are `Entity::cached` views, so a tile whose view was not
notified is replayed and not rendered. The cache off and on, alternating builds, one row per
run:

| arm | (g) 6 flooding, draw | (i) 1 flooding + 5 still, draw | (h) echo beside 6 flooding (next display tick) |
| --- | --- | --- | --- |
| off | 1.6 / 2.4 / 5.8 / 114.4 | — | 29.8 / 64.6 / 111.3 |
| off | 1.6 / 2.4 / 3.0 / 3.3 | — | 29.8 / 40.1 / 40.9 |
| off | 1.6 / 2.4 / 3.8 / 4.5 | **0.9 / 1.1 / 1.2 / 1.3** | 29.0 / 40.4 / 41.7 |
| off | 1.6 / 2.3 / 3.3 / 3.5 | **0.9 / 1.1 / 1.3 / 1.3** | 29.7 / 42.2 / 43.0 |
| on | 1.7 / 2.5 / 3.3 / 3.4 | — | 29.4 / 42.9 / 52.8 |
| on | 1.6 / 2.5 / 3.0 / 3.5 | — | 29.6 / 39.1 / 41.5 |
| on | 1.6 / 2.7 / 3.4 / 9.1 | **0.7 / 0.9 / 1.0 / 1.0** | 29.2 / 42.6 / 55.0 |
| on, with the file tile cached and the glass meter | 1.6 / 2.5 / 3.2 / 3.4 | **0.7 / 0.9 / 1.1 / 1.1** | 44.3 / 56.5 / 59.7 (glass) |

With six shells flooding, all six are dirty in every frame (the self-test link applies one
batch per nominal frame, and each shell sends a frame every 8 ms), so the cache has nothing to
replay: p50 is 1.6 ms in both arms. With one shell flooding beside five still ones, it saves
0.2 ms of a 0.9 ms draw. A still terminal that is rendered anyway redraws from the word cache,
which is already cheap. The draw is not where a keystroke's time goes: the echo beside six
floods (29 ms) is 6 ms over the echo into a lone shell (23 ms, below), and that difference is on
the worker and the link, not the frame.

**Keystrokes timed at the glass.** The meter used to stop at the next display tick after the
paint. It now stops at `presented_at` of the first frame submitted after the paint
(`slopty_ui::shown`, from `Window::on_frame_presented`). Scenario (d), 60 keys at 15/s into one
shell:

| meter | never: echo | always: echo | always: predicted |
| --- | --- | --- | --- |
| next display tick | 23.3 / 32.4 / 40.5 | 21.9 / 28.6 / 39.2 | 7.1 / 13.2 / 14.3 |
| glass, run 1 | 38.3 / 46.4 / 54.9 | 38.3 / 45.1 / 47.8 | 23.9 / 30.2 / 31.8 |
| glass, run 2 | 38.9 / 45.6 / 46.7 | 35.0 / 46.3 / 59.2 | 20.9 / 29.2 / 39.4 |

The 15 ms added is the meter reading what it always should have. The app got no slower. A key
the predictor echoes locally reaches the glass 21–24 ms after it is typed. The fork's
`frame_latency` example measured 20–22 ms from notify to glass for an idle window on this
display ("submit to glass", above), so nearly all of that is the compositor. The video pacer's
arrival → present clock now stops at the glass the same way. It has no number here: the
display scenarios need Screen Recording.

## 2026-09-25 — keystroke to glass, hop by hop

The meter now splits every key into hops on one clock (`slopty_ui::terminal::latency`,
`LatencyInfo::echo_hops_us` / `predicted_hops_us` in the self-test `dump`). An echoed key runs
key → its echo's batch left the link (the worker's round trip plus the hand-over to the UI
thread) → applied to the grid → painted → submitted to the GPU → on the glass. A predicted key
runs key → guess painted → submitted → glass. "Submitted" and "glass" come from the fork's
`PresentedFrame`. Scenario (d): 60 keys at 15/s into one shell, window 1280 × 800, 75 Hz display
(13.3 ms a refresh), mac-studio. Values are p50 (p95) in ms.

```
# binaries: cargo build [--release] -p slopty-ptyd -p slopty-workerd -p slopty-serverd \
#   -p slopty-cli -p slopty -p slopty-e2e --bins --features slopty/e2e
D=$PWD/target/e2e-perf; mkdir -p $D/run $D/artifacts
SLOPTY_DATA_DIR=$D SLOPTY_PTYD_SOCKET=$D/run/ptyd.sock SLOPTY_WORKER_SOCKET=$D/run/worker.sock \
  SLOPTY_E2E_BIN_DIR=$PWD/target/release SLOPTY_E2E_ARTIFACTS=$D/artifacts SLOPTY_SMOOTH_E2E=1 \
  cargo nextest run -p slopty-e2e --test smooth --no-capture -E 'test(typing_is_timed)'
# prints "MEASURE (d) mac" (totals) and "MEASURE (d) mac hops" per policy
cd .research/zed-main && MACOSX_DEPLOYMENT_TARGET=26.5 IPHONEOS_DEPLOYMENT_TARGET= \
  cargo run -p gpui --example frame_latency --profile release-fast -- idle|echo
```

**The shell first.** The typed shell used to be the user's own login zsh. Here that zsh runs a
syntax highlighter on every key, and the worker's trace (`slopty_worker::session=trace`) put
PTY write → echo read at p50 8.0, p90 13.5 ms. Everything else in the round trip took about
3 ms. With the same binaries (release, before the fixes below) and an empty `ZDOTDIR`, which
the scenario now uses so the number is Slopty's:

| typed shell | key → arrived | echo, key → glass |
| --- | --- | --- |
| the user's zsh (load 1.8) | 12.0 (19.9) | 39.1 (45.0) |
| empty `ZDOTDIR` (load 1.2) | 1.9 (4.6) | 32.9 (41.4) |

**Where the rest went.** Every key drew two frames (124 frames for 60 keys). The key itself
drew one, even with nothing to show: the view notified on every key, and the self-test key
command also forced a whole-window refresh. The echo arrived 2 ms later and waited for the next
vsync tick (applied → painted 7.2), and then waited a whole refresh more behind the key's frame
(submitted → glass 28.5, against 19 for a lone frame). A `CAMetalLayer` shows each drawable for
at least a refresh, so a second frame inside one refresh is shown a refresh after the first.
The fork's `frame_latency echo` shows the same thing with no terminal in it: a notify, then a
second one 3 ms later. Two runs, load 4–5:

| frame_latency | notify → glass p50 (p99) |
| --- | --- |
| `idle`: a lone frame | 18.9 (27.4), 20.5 (27.5) |
| `echo`: the key's frame | 18.9 (27.2), 21.9 (27.3) |
| `echo`: key → the second frame on glass | 31.9 (40.6), 35.2 (40.7) |

Presenting through the Core Animation transaction (`presentsWithTransaction`) was tried for
the same case. It was worse: idle 22.7, echo 33.8. It was not kept.

**The changes.** (1) A key notifies the view only when the tile shows something new before the
worker answers: a guess, the bottom of the history, a cursor that had blinked off, a selection
cleared, a sticky modifier used. Committed input-method text follows the same rule. (2) The
self-test `type` and `keys` commands no longer force a whole-window refresh. They settle on the
next frame and draw what the views asked for, as a keyboard does. (3) Fork, measured with a
local patch and pinned since as 6fbee65: the immediate frame an idle window gets on the next main-queue turn is kept for later when the wake that took it
drew nothing. The self-test's own next-frame wait is such a wake, and it used to push the
echo's frame to the next tick. (4) The self-test build's link pacer is gone (see
decisions/ui.md, "The link applies host events once per frame").

Before and after, same test binary, one row per run:

| build | policy | echo, key → glass | predicted, key → glass | frames |
| --- | --- | --- | --- | --- |
| release, before (load 1.2) | never | 32.9 (41.4) | — | 124 |
| release, before | always | 34.3 (42.4) | 21.0 (29.3) | 124 |
| release, after (load 1.5) | never | **22.3** (30.7) | — | 65 |
| release, after | always | 34.7 (43.3) | 21.4 (29.9) | 122 |
| release, after (load 1.9) | never | **22.6** (30.9) | — | 65 |
| release, after | always | 34.0 (42.8) | 21.8 (29.5) | 122 |
| debug, before (load 1.2) | never | 35.0 (41.8) | — | 122 |
| debug, before | always | 34.8 (43.3) | 21.5 (30.7) | 123 |
| debug, after (load 11) | never | 25.7 (32.5) | — | 65 |
| debug, after (load 8) | never | 24.5 (30.9) | — | 65 |
| debug, after without (3) (load 2) | never | 26.5 (**43.7**) | — | 65 |

The hops of an unpredicted key (`never`, the path a LAN takes, where the adaptive policy draws
no guess), release, p50:

| | key → arrived | arrived → applied | applied → painted | painted → submitted | submitted → glass |
| --- | --- | --- | --- | --- | --- |
| before | 1.9 | 0.0 | 7.2 | 0.4 | 28.5 |
| after, run 1 | 1.3 | 0.0 | 1.4 | 0.5 | 19.1 |
| after, run 2 | 1.1 | 0.0 | 1.2 | 0.5 | 19.4 |

A predicted key, release, after: key → painted 1.4–1.5, painted → submitted 0.5,
submitted → glass 19.2–19.3.

**What is left.** An echoed key now reaches the glass at the compositor floor plus the round
trip plus one draw: 19.1–19.4 ms from submission to the glass (the `idle` probe's floor, about
1.4 refreshes), 1.1–1.3 ms from the key to the echo arriving on loopback, and 1.9 ms from
applying the frame to submitting it (a main-queue turn, then about 1 ms of layout, prepaint and
paint for the whole window). A guess reaches the glass at the floor plus that same draw. The
echo of a guessed key still shows a refresh after the guess (34–35 ms) whenever the round trip
is shorter than a refresh, because it is the second frame inside one refresh. A correct guess
already shows the same cells, so this is only visible where the guess was wrong. On a link slow
enough for the adaptive policy to draw guesses (25 ms or more), the echo lands more than a
refresh after the guess, and its frame is an idle window's immediate frame again. The p95 of
about 30 ms is the floor's own spread: a lone frame's p99 in the `idle` probe is 27–28 ms.

The whole smooth suite on the same two release builds, back to back (load 4–9 from other
sessions). Draw is p50 / p95 / p99 / max in ms; "every" is the median interval between draws.
The self-test link pacer held the app to one link update per nominal 60 Hz frame. With floods
running, that capped drawing at 60 a second on this 75 Hz display, and a key typed beside six
floods waited up to a frame for its echo to be applied (arrived → applied 16.2 ms):

| scenario | before: draw · every | after: draw · every |
| --- | --- | --- |
| (a) 20 shells, strip | 1.3 / 2.5 / 4.1 / 4.3 · 17.4 | 1.2 / 3.6 / 5.1 / 7.8 · 13.3 |
| (b) 20 shells, overview | 1.6 / 3.6 / 6.8 / 8.5 · 18.0 | 1.3 / 2.3 / 3.9 / 7.2 · 13.3 |
| (e) file tile + 5 shells, strip | 1.1 / 1.7 / 6.4 / 7.4 · 16.2 | 1.1 / 1.7 / 3.5 / 6.6 · 13.3 |
| (f) file tile + 5 shells, overview | 2.0 / 2.6 / 7.4 / 9.7 · 17.6 | 2.0 / 2.6 / 7.2 / 10.0 · 13.3 |
| (g) 6 shells flooding, still | 1.3 / 1.9 / 3.0 / 3.3 · 17.9 | 1.4 / 2.7 / 3.6 / 3.8 · 13.3 |
| (i) 1 flooding + 5 still | 0.6 / 0.8 / 0.9 / 1.0 · 17.9 | 0.6 / 0.8 / 0.9 / 0.9 · 13.3 |

| (h) typing beside 6 floods | echo p50 (p95) | key → arrived | arrived → applied | applied → painted | submitted → glass |
| --- | --- | --- | --- | --- | --- |
| before | 42.0 (53.1) | 0.0 | 16.2 | 4.6 | 26.6 |
| after | **28.4** (34.2) | 1.8 | 0.0 | 9.3 | 16.5 |

Before, "arrived" is when the batch's first event left the channel, ahead of the pacer's wait,
and the echo usually joined that batch during the wait, hence 0.0 and 16.2. Beside floods the
window draws every refresh, so an echo waits for the next tick (applied →
painted up to a refresh) and is never an idle window's immediate frame. The draws stay under
a quarter of the refresh at 75 frames a second. In the same run (d) read never 23.3 (32.5), and
always 34.1 (42.5) for the echo and 20.8 (29.1) for the guess. The (d) test's own check that 58
of 60 guesses were drawn failed on both builds at this load (57 of 60). On loopback the echo
sometimes lands before the guess's frame is painted, and then no frame ever shows that guess.

## 2026-09-25 — guesses against echoes that win the race

The keystroke meter now counts, for each key the predictor guessed at, whether its echo was on
the first frame painted after it (`echo_first`) and whether a frame after it showed neither its
guess nor its echo (`guess_late`). Same scenario (d) and command as "keystroke to glass, hop by
hop", release build, mac-studio under other sessions' builds. With `SLOPTY_PREDICT=always`: 58
of 60 keys drawn as guesses, 1 echoed first, 0 late; guess p50 21.6 ms (p95 30.8), echo p50
34.9 ms (p95 44.1). With `never`: echo p50 26.4 ms (p95 32.2), 60 keys.


## 2026-09-25 — an echo beside floods waits for the tick, and nothing drawn sooner shows sooner

Scenario (h) puts the echo p50 at 28.4 ms, against about 22.5 ms in an idle window. Most of
the gap is the hop from applied to painted. Six flooding shells make the window draw on every
display-link tick, so an echo waits for the next tick and never gets an idle window's
immediate frame. The question was whether a frame drawn for the echo at once, off the tick,
could reach the glass sooner. The fork's `frame_latency` has a `flood` mode for this (fork
7b2e5e9). One thread notifies every 2 ms, so the window draws every refresh. Another notifies
every 60 ms or so, at a phase that walks across the refresh, the way a keystroke's echo
arrives. `wake_to_glass` runs from that notify to the glass of the first frame submitted
after it. Two other schedules ran as local patches to `gpui_macos`, not kept. *At once* lets
the echo's notify take an immediate frame even while the window draws every refresh.
*Deferred* draws each tick's frame about 2 ms after the tick, closer to the compositor's
deadline. It asks the main queue's `dispatch_after` for 1 or 1.5 ms, and the timer's leeway
stretches that to 1.9–2.5 ms. 75 Hz display (13.3 ms a refresh), mac-studio,
release-fast, 200 samples a run (40 for `idle`), ms:

```
cd .research/zed-main && MACOSX_DEPLOYMENT_TARGET=26.5 IPHONEOS_DEPLOYMENT_TARGET= \
  cargo run -p gpui --example frame_latency --profile release-fast -- flood|idle
```

| schedule | wake → glass p50, one run each (load 6–14) | submit → glass p50 |
| --- | --- | --- |
| on the tick (the fork today) | 24.4, 26.2, 23.8; earlier 23.7–26.1 in 8 runs | 17.9–18.2 |
| at once, off the tick | 27.7, 27.2, 26.8; earlier 26.2–30.1 in 4 runs | **27.8–31.2** (queued) |
| deferred about 2 ms | 27.7, 24.5, 24.2; earlier 23.2–29.9 in 5 runs | 16.3–17.9 |
| a lone frame in an idle window (`idle`) | 20.5, 19.7, 18.4 | 18.2–20.3 |

Two runs, one on the tick and one deferred, stalled for 5–8 s with a few hundred frames
dropped, as if the window had been hidden. Their p50s are listed but say little.

**Drawn at once, it shows later.** A `CAMetalLayer` presents drawables in order and shows each
one for at least a refresh. While the window draws every refresh, the tick's frame is still
in flight when the echo arrives (submit → glass is about 18 ms, longer than a refresh). A frame
drawn for the echo then reaches the glass a refresh after that frame, the same refresh the next
tick's frame would have reached anyway. The tick's frame that follows then queues behind it,
which is what the 28–31 ms submit → glass shows, until the pacer holds a tick to drain it.

**The tick is already close to the deadline.** In `continuous`, a busy-wait of 3 ms before
each render still made the same refresh. At 7 and 8 ms every frame missed it by exactly one
refresh (tick → glass 31.3 against 19.0). The runs at 2, 4, 5 and 6 ms mixed hits and misses
and queued (submit → glass p50 25–29), so the edge is not sharp. The deferred runs agree: a
draw about 2.5 ms after the tick made its refresh, one at 3.8 ms missed it. The deadline sits
2–4 ms after the tick and moves with the machine's load. The app's draw beside six floods
takes 1.1–1.4 ms of that (its submit → glass is 16.8 against the probe's
17.9). At best a deferred draw would gain about a millisecond. Each miss costs a repeated
frame and a queue that a later hold has to drain.

```
FRAME_LATENCY_SPIN_US=0|2000|3000|4000|5000|6000|7000|8000 \
  target/release-fast/examples/frame_latency continuous   # (in .research/zed-main)
```

The app, same session, release build, (h) and (d) with the commands of "keystroke to glass, hop
by hop" (`--test-threads 1`), load 3–9. p50 (p95) in ms:

| run | echo, key → glass | key → arrived | applied → painted | painted → submitted | submitted → glass |
| --- | --- | --- | --- | --- | --- |
| (h) beside 6 floods | 28.3 (45.0) | 1.5 | 8.1 | 0.6 | 16.8 |
| (d) `never`, idle window | 21.3 (28.2) | 0.4 | 0.4 | 0.1 | 20.3 |

The gap between them is the wait for the next tick, about half a refresh plus the time from
the tick to the paint, less the 3.5 ms by which a tick's frame reaches the glass sooner than a
frame at a random phase. With this compositor, a window that must draw every refresh cannot
avoid it. Nothing was changed in the app or in the fork's frame scheduling. The scenario
labels in smooth.rs ((a) to (i)) match the latest sections above. The older, dated sections
keep the labels of their own day.

## 2026-09-25 — the held capture, the audio lane, and what is still owed on hardware

Unit models in `slopty-worker`, run on mac-studio under load from other sessions. None of these
touches a capture or a real link; the hardware runs are listed at the end as owed.

```sh
cargo nextest run -p slopty-worker --lib -E 'test(/screen::/)' --no-capture
```

**Audio behind a keyframe** (`audio_waits_behind_a_slice_of_a_keyframe_not_all_of_it`). The
model is QUIC's datagram queue drained a millisecond at a time in whole datagrams. A 130 kB
keyframe (113 × 1 150 B) goes in at 0 ms and a 160 B audio packet at 5 ms. The 20 Mbit/s rate
comes out at 2.3 kB a millisecond once whole datagrams are drained.

| link | audio wait, FIFO | audio wait, lane | keyframe's last byte, FIFO | with lane |
| --- | --- | --- | --- | --- |
| 20 Mbit/s | 52 ms | 4 ms | 57 ms | 57 ms |
| 800 Mbit/s | 1 ms | 1 ms | 2 ms | 3 ms |

At 20 Mbit/s the lane keeps QUIC one slice ahead and the link never idles, so the keyframe is not
later. At 800 Mbit/s the lane starts from the target's rate and learns the link in two ticks;
that one extra millisecond is paid on the first large frame after audio starts, and only while
audio flows.

**A still picture's owed frame** (`a_still_picture_sends_its_held_capture_when_owed_or_asked`,
`a_moving_picture_is_never_repaired_ahead_of_its_next_capture`). At a 30 fps rung on a 60 Hz
capture, the scroll's last capture (16.7 ms after the last encoded one) was never sent before.
Now it goes out 25 ms after it was captured, the quiet threshold, and a refresh on the still
picture goes out one cadence period after the last frame. While the picture keeps moving the
repair waits past the next capture, so it never sends an older frame in place of a newer one.

**Owed, on hardware.** Each needs the installed, TCC-granted worker, and the machine was shared
with five other sessions, so none ran here:

```sh
cargo xtask sign && slopty worker install --port 45560 --bind <lan-ip> --log 'info,slopty_worker=debug'
slopty worker doctor
# shaped ladder: collapsed-rung gaps and drops, keyframes_deferred, and now ScreenStats::ltr
# (offered / acked / refreshes_idr / refreshes_delta / usable) and repaired, per rung
SLOPTY_E2E_WORKER_SOCKET="$HOME/Library/Application Support/Slopty/run/worker.sock" \
  SLOPTY_E2E_SECONDS=8 RUST_LOG=info,slopty_client=debug,slopty_codec=debug \
  cargo nextest run -p slopty-workerd --test e2e -E 'test(screen_over_a_shaped_link)' --no-capture
# capture floor at queueDepth 3 with a held surface and the user-interactive video queue
slopty bench screen --worker <lan-ip>:45560 --window <id> --seconds 20   # host capture / encode p50 / p95 against the 2026-09-05 table
```

The ladder should also run once with audio playing in the target, to read the lane on a real
link (`laned` in `slopty worker screens`).

## 2026-09-25 — input injection off the runtime

```sh
cargo nextest run -p slopty-input --test cost --run-ignored only --no-capture
```

`injection_cost_on_the_callers_thread` drives 1500 moves at 500 Hz and then 200 key presses at
200 Hz into an injector aimed at a real on-screen window. The window-server reads are real (the
target's bounds, the owner's `NSRunningApplication`); nothing is posted and nothing is activated.
The time is what one event costs the thread that hands it over, which on the worker is the
stream's tokio task. Before is the injector as it stood at `5e7e85d`, called inline by the task.
After is the stream's `InputThread`. mac-studio, three runs each, with other builds running
(load average about 6).

| per event, on the stream's task | before | after |
| --- | --- | --- |
| move, p50 | 0.4–0.5 µs | 3.0–3.9 µs |
| move, p99 | 1.1–4.0 ms | 33–48 µs |
| move, max | 7.2–11.9 ms | 70–296 µs |
| key press, p50 | 1.3–2.1 ms | 4.0–5.7 µs |
| key press, p99 | 5.8–15.1 ms | 12–28 µs |
| key press, max | 12.0–17.2 ms | 20–42 µs |
| bounds reads in front of a move, of 1500 | 43 | 0–1 (the first move, when it beats the reader) |
| owner lookups, of 200 presses | 200 | 5–6 |

Before, a bounds read (`CGWindowListCreateDescriptionFromArray`) landed on the task every
100 ms of pointer input, and every key-down and button-down paid an `NSRunningApplication`
lookup. The lookup cost 1–2 ms at the median and up to 17 ms, more than the bounds read. On
the worker the geometry tick measured that read at p95 2–6 ms and 93 ms at most (the heartbeat
section above). After, the task only queues. The input thread maps each move with bounds that a
second thread re-reads every 80 ms while pointer input flows, and stops reading a second after
it stops. The owner is looked up once per 250 ms of keys rather than on every one. The move
median rises by about 3 µs, the cost of a channel send, in exchange for a tail three orders of
magnitude shorter.

Queued to posted on the input thread, all 1900 events: p50 9–12 µs, p99 0.12–0.74 ms, max
0.9–8.4 ms. The tail there is the owner lookups and the one inline read, which no longer hold
up the stream's other work.

## 2026-09-25 — the audio jitter buffer on synthetic traces, and a reassembled frame without its copy

Release, mac-studio, shared with five other sessions. `docs/decisions/audio.md` (2026-09-25,
"Playback is a jitter buffer") holds the ruling.

```
cargo nextest run -p slopty-codec --release latency_tests --no-capture
cargo nextest run -p slopty-media --release --run-ignored only reassemble_cost --no-capture
```

**Audio, the playout on arrival traces** (`slopty_codec::audio` `latency_tests`). The traces are
built from arrival times, with 3 ms of link delay; the device pulls a 10 ms buffer every 10 ms of
its own clock, and each packet is a 440 Hz tone continuous across packets. "Heard" is the ring
plus the three device buffers queued ahead of it, the same model as the 2026-09-24 entry. It is
not a reading at the DAC. The largest step between neighbouring output samples is set against
the tone's own, 0.0288, and a hard cut in this tone can step by up to 1.0.

| trace | heard | underruns | trimmed / stretched | largest step |
| --- | --- | --- | --- | --- |
| 250 ms stall, then its backlog in one burst | 63 ms at most after the burst, 58 ms mean over 1.5–3 s | 1 | 227 ms / 0 | 0.0288 |
| worker clock +100 ppm, 2 min, 1 ms scatter | 60 ms at 5–15 s, 60 ms at 110–120 s | 0 | 25 ms / 0 | 0.0325 |
| worker clock −100 ppm, 2 min, 1 ms scatter | 58 ms, then 49 ms | 0 | 15 ms / 10 ms | 0.0325 |
| a 40 ms keyframe burst every 2 s, 3 ms scatter, 20 s | 77 ms mean after 3 s, 110 ms at most | 3 | 140 ms / 0 | 0.0327 |

The 2026-09-24 ring on the stall trace read 70 ms after the burst and 74 ms mean afterwards,
and cut the backlog with a hard edge. The target settles at 20 ms, the floor, on every trace.
Trimmed audio on the drift traces includes the first second's default target of 40 ms coming
down to 20. A clock 100 ppm off moves about 12 ms in two minutes, and the part the slices
absorbed is the rest of the trimmed figure. On the keyframe trace the percentile calls the
bursts noise, since they make up two packets in a hundred. So a burst starves the ring once a
window, and the lateness that starved it holds the depth until the window forgets it.

**Client, reassembling one frame** (`reassemble_cost`: every data fragment arrives, no parity,
200 frames, ingest to `next_frame`). The frame's bitstream is now a `Bytes::slice` of the
reassembled fragments, where it was a copy of them.

| frame | before | after |
| --- | --- | --- |
| 62 KB P-frame, 53 datagrams | 5.06 µs | 3.9 µs |
| 300 KB keyframe, 254 datagrams | 23.4 µs | 17.8 µs |

Three runs each. The before runs were taken earlier in the same session, at a load that was not
recorded; the after runs ran at a load average of 4 to 5.

## 2026-09-25 — echo pacing and frames in flight

Three session-actor tests, debug build, mac-studio with other sessions building (load 10–25).
The echo test types 40 keys into `cat` while a background loop prints a dot every 2 ms or so,
and times each key to the first frame that acknowledges it. The throttled test floods an
80 × 24 shell (120 bursts of 15 lines, 10 ms apart) into one viewer whose link carries
250 kB/s. The link holds each event until its last byte would have gone, as `send_raw` does.
The test reads how long after the engine's screen showed `DONE` the viewer's screen did, and
the most events it found waiting in the 256-deep sink. The reply test stops reading for 400 ms
of a flood with a `FetchLines` sent in the middle.

```
cargo nextest run -p slopty-worker --test session_actor --no-capture \
  -E 'test(/echo_beside|throttled|reply_reaches/)'
```

| | key → acking frame p50 / p90 / max | throttled viewer behind the program | events queued | `Lines` reply |
| --- | --- | --- | --- | --- |
| before (load 18) | 4.69 / 10.27 / 10.58 ms | **9 463 ms** | 157 | lost |
| after, run 1 (load 12) | 0.07 / 0.24 / 0.68 ms | 147 ms | 1 | arrives |
| after, run 2 (load 25) | 0.17 / 1.74 / 12.95 ms | 173 ms | 0 | arrives |
| after, run 3 (load 20) | 0.06 / 0.10 / 0.27 ms | 162 ms | 1 | arrives |

The flood beside the typing stayed paced: 40–42 frames in 400 ms before and after, against
50 at the 8 ms pace. A frame of that flood is about 14.6 kB. Before, 157 of them (2.3 MB)
waited in the sink, which a 250 kB/s link takes 9.4 s to drain. After, one frame waits behind
the one on the link: 29 kB, a tenth of a second. The p90 above 8 ms before is the pace plus
tokio rounding the pace timer up to its next millisecond.

The same queue sits one layer down as well, and this change does not bound it. A frame the
connection has written is in noq's stream buffer until the link carries it, up to the 1.25 MB
stream window. On a real link that is about 5 s at 250 kB/s. The credit is given back when the
connection drops the event after `send_raw`, so it bounds the channel and not QUIC.

The smooth suite's (h) and (d), release, with the commands of "keystroke to glass, hop by hop"
(`--test-threads 1`). The binaries were built from this tree with and without the change
(`target/e2e-perf/bin-before`, `bin-after`, `SLOPTY_E2E_BIN_DIR` pointed at each). Two pairs,
the second in reverse order, load 18–45. Both scenarios type into a quiet shell, which is not
the path this changes, so they check that nothing regressed. p50 (p95) in ms:

| run | (h) echo beside 6 floods | (d) `never` echo | (d) `always` echo | (d) `always` guess |
| --- | --- | --- | --- | --- |
| before, pair 1 (load 39→18) | 27.1 (43.6) | 21.8 (28.6) | 33.8 (41.0) | 20.8 (27.5) |
| after, pair 1 (load 18→34) | 28.4 (42.4) | 23.6 (45.5) | 37.0 (44.2) | 23.6 (30.9) |
| after, pair 2 (load 45→27) | 26.7 (41.7) | 22.2 (28.8) | 34.5 (40.7) | 21.5 (27.6) |
| before, pair 2 (load 27→30) | 31.1 (96.4) | 24.4 (65.8) | 35.5 (42.4) | 22.2 (28.6) |

The spread between runs of the same binary is larger than any difference between the arms.
The two wide p95s (after pair 1's `never`, before pair 2's (h) and `never`) are key → arrived
p95 of 17–68 ms, when the load peaked mid-run.

## 2026-09-25 — nothing on the connection waits behind anything slow

**The connection's loop.** Two e2e tests make the worker wait on the client, the way a stuck
or slow client does, and type into a `/bin/cat` session on the same connection meanwhile. In
the first the client allows one worker-opened stream at a time and attaches a second terminal
while the first holds it. In the second the client stops reading its control stream while
another client sends 300 000 pointings. Each key goes out after the last one's echo, and the
echo counts only when the frame shows that key at the end of the line. Debug build,
mac-studio, other sessions running (load average 4 to 18). "Before" is `conn.rs` as of
`4f48f26` with a 24 h wait given to `open_session`, which had none then.

```
cargo nextest run -p slopty-workerd --test e2e -E 'test(/a_terminal_waiting|stops_reading/)' --no-capture
```

| 30 keys | before | after |
| --- | --- | --- |
| an attach waiting for a stream | no echo within the test's 20 s step | p50 0.26 / p99 0.95 ms |
| the control stream unread | no echo within the test's 20 s step | p50 0.20 / p99 1.67 ms |

Before, the loop awaited the stream open and the event send inline, so the first key never
reached its session. Not measured here: a receiver report's encoder calls, which now run on
the blocking pool from a task of their own. That needs a live stream and Screen Recording.

**Datagrams ahead of an echo.** noq writes queued datagrams into each packet before any stream
data. `echo_beside_a_video_flood` runs `/bin/cat` behind a real connection's control and
session streams through `slopty-shape` at 20 Mbit/s, 2 ms each way. The worker side floods
datagrams at 60 fps: 25 kB frames cut to the path's datagram size, and a 130 kB keyframe every
second. Four arms on fresh connections: no video, video in one burst per frame, video through
the audio lane's rule (QUIC holds at most 5 ms of the link) while someone typed in the last
second, and that lane also metered to 2 MB/s, 80% of the link. The echo event is 240 B, about a
one-row frame. 200 keys an arm, release, three runs:

```
cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only --no-capture
```

| echo p50 / p99 ms | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| no video | 8.6 / 33.9 | 8.1 / 8.4 | 7.9 / 8.3 |
| bursts | 9.4 / 38.6 | 8.8 / 35.8 | 8.5 / 24.8 |
| lane, 5 ms slice | 8.8 / 48.6 | 8.5 / 31.4 | 8.9 / 39.3 |
| lane, metered | 8.5 / 34.2 | 8.6 / 14.6 | 9.0 / 19.3 |

| datagrams QUIC held when the echo was written, p99 (max) kB | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| bursts | 78.4 (112.0) | 98.8 (110.8) | 96.4 (118.0) |
| lane, 5 ms slice | 12.0 (12.4) | 12.0 (12.4) | 12.0 (12.4) |
| lane, metered | 12.0 (52.0) | 3.6 (3.6) | 2.4 (10.8) |

The clear link with no video answers in 0.45 to 0.76 ms at p50. The shaper's own timers put
the shaped baseline near 8 ms, twice its round trip, and run 1 was noisy from the start.

The slice does what it says. What QUIC holds in front of an echo drops from most of a keyframe
to 12 kB. The echo gets no faster, though. BBR3's window on this path read 0.9 to 7.4 MB at the
median against a bandwidth-delay product near 20 kB, so QUIC never holds a frame for the window.
The pacer lets it into the bottleneck queue, and the echo waits there instead. Only the metered
lane, which keeps video below the link rate, pulls the echo's p99 down (14.6 and 19.3 ms against
35.8 and 24.8 on the two quiet runs). The max stays near 52 ms in every arm with video, one
keyframe at this rate: a keyframe already on the wire when the key arrives still has to drain.

## 2026-09-25 — a scroll ships the rows it moved

Bytes a viewer that follows the diffs is sent: the encoded `TermEvent::Frame`, length prefix
included. The engine's screen is full of `ls -l`-like output with a zsh-style marked prompt on
the bottom row. "Echo" is the frame after the shell echoes one typed key. "Enter" is the frame
after `ls` ran: three lines of output and the next prompt, so the screen scrolled by four lines.
The one-line scroll is 500 lines written one at a time at 200 × 60, each frame built and encoded
(release). "Trimmed only" is the old engine with the new `Line` encoding: the engine file
checked out from `HEAD` for the run, then put back.

```
cargo nextest run -p slopty-engine --test wire_bytes --no-capture            # bytes
cargo nextest run --release -p slopty-engine --test wire_bytes --no-capture  # the scroll's time
```

| | 80 × 24 echo | 80 × 24 Enter | 200 × 60 echo | 200 × 60 Enter | 200 × 60 one-line scroll |
| --- | --- | --- | --- | --- | --- |
| before | 619 B | 15 008 B (24 rows) | 1 461 B | 88 333 B (60 rows) | 87 951 B, 486–519 µs |
| trimmed only | 228 B | 9 873 B | 230 B | 27 178 B | 27 883 B, 393 µs |
| rows compared by hash (rejected) | 228 B | 653 B | 230 B | 658 B | 505 B, 885 µs |
| after | 228 B | 653 B (4 rows) | 230 B | 658 B (4 rows) | 505 B, 343–369 µs |

A 200-column echo fits one 1252-byte packet again. The scroll still reads every row from
libghostty, and that read is most of the time. Hashing every cell with `DefaultHasher` cost
more than it saved. Comparing each row with the line kept costs less than encoding it did.
Keeping the lines costs the size of one screen per session, about 0.6 MB at 200 × 60.

Staleness on a throttled link, debug build, mac-studio at load 4–9. The flood is the one in
"echo pacing and frames in flight": 120 bursts of 15 lines into 80 × 24, the viewer's link
250 kB/s. The stream-window test models noq's send buffer: a write returns as soon as the
1.25 MB window has room, and the frame's credit goes back then, as `send_raw` does. The link
delivers each event when its bytes have crossed. The client applies it to a `TermState` and
sends back the requests that come out of it. The older test holds each event until its bytes
have crossed, a link with no buffer at all.

```
cargo nextest run -p slopty-worker --test session_actor --no-capture \
  -E 'test(/throttled|echo_beside/)'
```

| | behind the stream window: flood's end shown after | most in the stream buffer | no-buffer link: shown after | a frame |
| --- | --- | --- | --- | --- |
| before | 5 096 ms | 1 241 050 B | 137 ms | 14 530 B |
| after, run 1 | 222 ms | 64 282 B | 91 ms | 9 417 B |
| after, run 2 | 225 ms | 64 732 B | 95 ms | 9 328 B |
| after, run 3 | 258 ms | 63 422 B | 86 ms | 9 787 B |

The buffer now holds what the markers allow, 64 KiB and the frame in hand. At 250 kB/s that
is a quarter of a second. The echo beside a flood did not move: key to acking frame p50
0.06–0.10 ms, p90 0.10–0.14 ms, 39–43 flood frames in 400 ms.

## 2026-09-25 — BBR3's window under bursty video

The question from "nothing on the connection waits behind anything slow": why noq's BBR3 held
a 0.9 to 7.4 MB window on a path whose bandwidth-delay product is about 20 kB, and which
controller keeps a keystroke's echo quickest beside video.

`echo_beside_a_video_flood` gained what it needed to answer that. A sampler reads the worker's
path every 10 ms: the window in force and noq's own, bytes in flight (the new
`slopty_net::congestion` wrapper keeps noq's count), BBR3's pacing rate, the wrapper's delivery
rate and round-trip minimum, loss, and the shaper's standing queue (`Relay::queue_delay_down`).
With `SLOPTY_ECHO_TRACE` it also writes BBR3's model out of its `Debug` print. Each datagram now
carries its frame number and send time, so the client times whole frames. A fifth arm halves the
link's rate as the typing starts (`Relay::set_rate`), the way Wi-Fi or LTE drops under a sender.
`SLOPTY_ECHO_QUEUE_MS` sizes the queue and `SLOPTY_ECHO_LOSS` adds random loss. 320 keys an arm,
release, mac-studio with other sessions building (load average 2 to 21 across the runs).

```
SLOPTY_CC=<bbr3|bbr3-unbounded|cubic|cubic-unbounded> SLOPTY_ECHO_QUEUE_MS=<100|20> \
  SLOPTY_ECHO_LOSS=<0|0.002> SLOPTY_ECHO_TRACE=$PWD/target/cc-trace \
  cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only --no-capture
```

### Why the window grows (noq-proto 1.3.0, `src/congestion/bbr3/mod.rs`)

The window is `2 × max_bw × min_rtt + extra_acked` (`update_max_inflight`, line 1350). In the
trace `extra_acked` is the whole window less a kilobyte, and it climbs by about 380 kB every
250 ms, which is the video's own 1.5 MB/s. Every frame leaves the connection idle, and each
restart from idle moves `extra_acked_interval_start` to now (`handle_restart_from_idle`, line
1266) without clearing `extra_acked_delivered`. `update_ack_aggregation` (line 819) clears that
count only once it falls under `bw × interval`, which a fresh interval never allows, so every
byte delivered counts as aggregation. `min(extra, cwnd)` (line 839) and `cwnd + newly_acked` in
`set_cwnd` (line 1331) let each feed the other. Linux clears the count on the same event
(`tcp_bbr.c`, `CA_EVENT_TX_START` resets `ack_epoch_acked`) and caps the allowance at 100 ms of
bandwidth. noq does neither.

Two more things keep the pacing rate above the link. `check_full_bw_reached` (line 849) skips
app-limited samples, and `maybe_go_down` (line 1089) leaves `ProbeBW_UP` only on
`full_bw_now` or loss, so video that never fills the link sits in `UP` at gain 1.25: 61 to 79 %
of samples in the four traces. `max_bw` read 2.55 to 3.14 MB/s against the shaper's 2.5, since
the relay releases packets on 1 ms timer ticks and the ACKs come back in clumps. The same guard
can hold a flow in `Startup` for good (noq has a test for it, `startup_never_exits_when_app_limited_without_loss`,
line 3085), and `Startup` paces at `2.77 × initial_cwnd / 1 ms` (line 611), 106 MB/s with our
38 400 B initial window. In one traced run a quarter of the samples were still in `Startup`.

Datagrams count as bytes in flight like anything else: a DATAGRAM frame is ack-eliciting
(`frame.rs:246`), and `packet_builder.rs:284-318` charges the packet to the controller and the
pacer. What the window never did was bind. Bytes in flight peaked at 131 kB, one keyframe, so
the pacer alone decided when a burst left, and at 1.25 × an overestimate a keyframe reaches the
bottleneck faster than it drains.

`app_limited` is set when a `poll_transmit` finds nothing to send and nothing blocked it
(`connection/mod.rs:1424`). For one burst per frame that is right. The trouble is what BBR3 does
with a flow that is app-limited most of the time, above.

The shaper behaves as a drop-tail router does. Each direction is one FIFO bounded in bytes
(`slopty-shape/src/lib.rs:125` drops an arrival that would wait longer than the queue's size at
the link's rate). Delay applies after serialisation, and nothing reorders. It has no AQM, where
many current home routers run fq_codel, and it releases packets on tokio's 1 ms timer
(`relay.rs:163`), which reads to BBR as ACK aggregation, much as Wi-Fi's frame aggregation does.
The runs use 100 ms of buffer (250 kB), a home router's size, and 20 ms (50 kB) for a shallow one.

### The comparison

`bbr3` and `cubic` are held by the new bound (twice the measured bandwidth-delay product, below);
`-unbounded` is noq's controller as it ships. Medians over 2 to 5 runs per cell, interleaved.
"queue" is the shaper's standing queue in front of the worker's packets, sampled while the keys
went; nothing on either host can reorder what waits there. Echo p99 takes the median of the runs'
p99s, and the range across runs follows in brackets. The shaped link with no video already
spread p99 over 9 to 38 ms under this load, so echo differences under about 10 ms are noise, and
the queue columns are the load-independent reading.

100 ms queue, no loss:

| arm | controller | echo p50 / p95 / p99 [runs] ms | queue p99 / max ms | cwnd p50 | lost | keyframe p50 / p99 ms | Mbit/s |
| --- | --- | --- | --- | --- | --- | --- | --- |
| bursts | bbr3-unbounded | 10.2 / 22.0 / 30.1 [26.4–55.7] | 16.7 / 26.5 | 3.7 MB | 0 | 56 / 170 | 12.5 |
| bursts | **bbr3** | 9.4 / 19.4 / 23.2 [18.5–30.8] | **10.9 / 12.1** | 42 kB | 0 | 56 / 169 | 12.5 |
| bursts | cubic-unbounded | 9.4 / 31.4 / 52.0 [51.3–52.8] | 42.5 / 51.7 | 1.1 MB | 0 | 56 / 62 | 12.9 |
| bursts | cubic | 9.8 / 23.1 / 27.8 [18.4–70.0] | 15.2 / 16.2 | 48 kB | 0 | 56 / 60 | 12.7 |
| laned | bbr3-unbounded | 9.7 / 22.5 / 28.5 [27.2–56.3] | 17.8 / 39.3 | 4.8 MB | 0 | 56 / 152 | 12.6 |
| laned | **bbr3** | 9.8 / 19.5 / 23.7 [19.9–26.4] | **11.9 / 13.6** | 40 kB | 0 | 56 / 133 | 12.5 |
| laned | cubic-unbounded | 9.6 / 30.5 / 46.6 [45.4–47.9] | 40.5 / 42.7 | 200 kB | 0 | 56 / 59 | 12.9 |
| laned | cubic | 10.0 / 26.1 / 45.3 [28.4–61.9] | 15.4 / 18.2 | 45 kB | 0 | 56 / 68 | 12.6 |
| metered | bbr3-unbounded | 9.5 / 16.1 / 28.1 [22.6–60.0] | 5.8 / 10.4 | 2.0 MB | 0 | 63 / 164 | 12.4 |
| metered | **bbr3** | 9.3 / 14.7 / 23.6 [20.5–30.5] | 5.5 / 8.2 | 37 kB | 0 | 63 / 165 | 12.4 |
| metered | cubic | 9.7 / 16.3 / 28.9 [19.2–77.6] | 7.2 / 9.9 | 50 kB | 0 | 64 / 78 | 12.5 |
| halved | bbr3-unbounded | 26.7 / 40.9 / 112 [68.5–155] | 118 / 128 | 26 kB | 114 | 158 / 296 | 9.6 |
| halved | **bbr3** | 24.1 / 29.6 / **37.4** [32.2–42.5] | **17.4 / 19.8** | 20 kB | **0** | 151 / 275 | 9.6 |
| halved | cubic | 66.7 / 209 / 315 [215–415] | 199 / 200 | 138 kB | 312 | 240 / 345 | 9.9 |

The same with 0.2 % random loss each way (two runs each; `cubic-unbounded` not run):

| arm | controller | echo p50 / p95 / p99 [runs] ms | queue p99 / max ms | lost | keyframe p50 / p99 ms | Mbit/s |
| --- | --- | --- | --- | --- | --- | --- |
| bursts | bbr3-unbounded | 9.1 / 19.4 / 27.1 [24.4–29.8] | 15.7 / 38.1 | 55 | 56 / 164 | 12.4 |
| bursts | bbr3 | 8.7 / 19.1 / 28.5 [23.6–33.4] | 8.2 / 9.7 | 54 | 59 / 201 | 12.4 |
| bursts | cubic | 9.2 / 20.3 / 46.7 [26.5–66.9] | 9.3 / 10.3 | 58 | 56 / 61 | 12.4 |
| halved | bbr3-unbounded | 24.9 / 35.7 / 94.1 [53.2–135] | 127 / 132 | 114 | 159 / 288 | 9.6 |
| halved | bbr3 | 23.7 / 31.0 / 38.6 [38.2–39.1] | 18.8 / 21.2 | 54 | 151 / 268 | 9.6 |
| halved | cubic | 28.1 / 35.9 / 54.7 [50.1–59.2] | 20.7 / 21.6 | 64 | 156 / 168 | 9.8 |

20 ms queue, no loss (two runs each):

| arm | controller | echo p99 [runs] ms | queue p99 / max ms | lost | keyframe p50 / p99 ms | frames whole |
| --- | --- | --- | --- | --- | --- | --- |
| bursts | bbr3-unbounded | 42.0 [33.8–50.3] | 15.4 / 20.1 | 75 | 56 / 176 | 2031 / 2042 |
| bursts | bbr3 | 30.1 [29.7–30.5] | 10.8 / 13.7 | 0 | 56 / 164 | 2042 / 2047 |
| bursts | cubic-unbounded | 39.0 [38.7–39.4] | 16.4 / 20.4 | 952 | **none whole** | 1994 / 2063 |
| bursts | cubic | 25.2 [20.8–29.6] | 10.8 / 12.6 | 0 | 56 / 69 | 2033 / 2038 |
| laned | bbr3-unbounded | 37.0 [25.4–48.6] | 16.6 / 19.5 | 90 | 56 / 184 | 2023 / 2040 |
| laned | bbr3 | 31.8 [25.8–37.8] | 12.2 / 14.9 | 0 | 56 / 193 | 2050 / 2054 |

A sweep of the bound's size on the 100 ms queue (single runs, an earlier version that sized it
from BBR3's pacing rate) found one round trip too tight: the laned echo's p50 rose to 16 ms on
the shallow queue and the idle connection's smoothed round trip to 24 ms. Two matches BBR's own
`cwnd_gain` and is what shipped.

What the numbers say:

* **The bound is the fix for the queue, and it costs no video.** On a steady link it cuts the
  bottleneck queue's p99 by a third and its maximum by half or more in the bursts and laned
  arms, with the same 12.5 Mbit/s and the same frames delivered whole. Metered video already
  stays under the link rate and barely changes. When the link halves, noq's BBR3 kept pacing at
  the old rate with a window nothing bound, and in two of four runs it filled the whole buffer
  (195 ms, up to 228 packets dropped at the queue). Bounded, the queue stayed under 22 ms in all
  four with no overflow, and echo p99 was 32 to 43 ms against 53 to 155. On the shallow queue the
  bound took BBR3's overflow losses from 75 to 0.
* **No controller makes the bursts arm's echo fast.** A keyframe is 52 ms of this link. Paced
  at or below the link rate it waits in QUIC, where noq writes datagrams before stream data
  (`connection/mod.rs:6504` before `:6562`); paced above, it waits in the bottleneck. Either way
  an echo written behind it waits for it. The bound only stops the controller adding queue of
  its own on top. The halved arm shows the sender's half: with the queue held at the bottleneck,
  the capture guard's 50 kB held in QUIC put the echo's p50 at 24 ms. The QUIC half is fixed
  since: see "an echo ahead of the datagrams" below.
* **Cubic is not the answer, bounded or not.** Unbounded, it has no rate to pace by, so a burst
  leaves at once: on the shallow queue 950 packets overflowed and not one keyframe arrived
  whole. Bounded, it fills the buffer when the link halves (199 ms, 312 dropped), because
  nothing in Cubic drains a queue, the round-trip minimum ages into the queued round trip after
  ten seconds, and the bound follows it up. Its one win is keyframe p99, next.
* **BBR3's keyframe p99 of 130 to 200 ms is `ProbeRTT`, bound or not.** Every 5 s (line 82)
  BBR3 drops to `max(0.5 × BDP, 4 packets)` for 200 ms, and a keyframe that lands then drains
  through 7.5 kB a round trip. Cubic's keyframe p99 is 56 to 78 ms. The bound does not touch it.

The final run with nothing set, load 10 to 21 (`target/cc-logs/final.log`): bursts echo p50 9.3
/ p99 24.3 ms, queue p99 7.8 ms, cwnd 36 kB, 787 of 789 frames whole at 12.4 Mbit/s; halved
echo p99 39.4 ms, queue p99 22.8 ms, no overflow, 9.6 Mbit/s.

## 2026-09-25 — an echo ahead of the datagrams

noq-proto 1.3.0 writes every queued DATAGRAM frame that fits into a packet before any STREAM
frame, so an echo written behind a keyframe waited for the keyframe's datagrams to be paced out
("datagrams ahead of an echo", above). Slopty's noq now has
`TransportConfig::stream_priority_before_datagrams(Some(threshold))`: streams at or above the
threshold write first, then datagrams, then the rest. Slopty sets it to 0, so the control and
session streams go first and tunnels (−1) and files (−2) stay behind video.
`SLOPTY_DATAGRAMS_FIRST=1` restores noq's order, and that is the "off" arm.

The harness is `echo_beside_a_video_flood` as described above: 20 Mbit/s, 2 ms each way, 100 ms
of bottleneck queue, BBR3 with the bound. Release build, mac-studio. On and off ran interleaved,
alternating which went first, ten runs each for bursts and halved and three for the other arms.
The load average fell from 159 to 8 over the half hour. Runs 8 to 10 saw 8 to 13. Logs are in
`target/logs/prio/`.

```
cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only --no-capture
SLOPTY_DATAGRAMS_FIRST=1 cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only --no-capture
SLOPTY_ECHO_ARMS=bursts,halved …                                       # runs 4 to 10
```

Medians over the runs, with each statistic's range across runs in brackets:

| arm | order | runs | echo p50 ms | echo p99 ms | echo max ms | frames whole | Mbit/s |
| --- | --- | --- | --- | --- | --- | --- | --- |
| bursts | streams first | 10 | 9.4 [8.8–10.7] | **19.0** [13.5–30.2] | **22.9** [14.3–71.9] | all but 2 | 12.4–12.6 |
| bursts | datagrams first | 10 | 9.8 [8.1–10.1] | 24.6 [18.4–50.9] | 31.8 [19.7–87.1] | all but 1–2 | 12.4–12.7 |
| halved | streams first | 10 | **21.0** [19.2–24.4] | **34.4** [27.3–43.4] | **39.6** [28.3–55.5] | all but 1–5 | 9.6–9.7 |
| halved | datagrams first | 10 | 25.3 [23.0–30.7] | 39.0 [33.7–83.3] | 51.0 [33.7–120.6] | all but 3–5 | 9.0–9.7 |
| laned | streams first | 3 | 9.8 [9.1–10.4] | 17.9 [14.2–23.8] | 25.1 [23.0–30.6] | all but 2 | 12.5–12.6 |
| laned | datagrams first | 3 | 10.2 [9.1–10.7] | 36.3 [27.9–73.9] | 46.4 [36.9–118.8] | all but 2 | 12.4–12.6 |
| metered | streams first | 3 | 9.2 [8.6–9.5] | 21.6 [13.8–22.7] | 36.7 [28.3–48.0] | all but 2 | 12.4–12.5 |
| metered | datagrams first | 3 | 9.0 [8.4–9.5] | 20.7 [20.7–41.7] | 31.3 [21.7–95.7] | all but 2–3 | 12.3–12.4 |

Paired run by run, streams first had the lower or equal bursts p99 in nine of ten runs and the
lower halved p50 in all ten. The halved arm holds the capture guard's 50 kB in QUIC for most of
its keys, and its p50 dropped by about 4 ms. Bursts only catches an echo behind a keyframe now
and then, so its p50 is unchanged and its tail moved. Keyframe p50 stayed at 55 to 56 ms
(bursts) and 149 to 165 ms (halved), and the same frames arrived whole in both orders. Video
gave up nothing. The "datagrams ahead of it" column the harness prints reads the same in both
orders. It counts what QUIC held when the echo was written, and streams first now pass it.

What is left is the bottleneck. The halved arm's echo p50 of 21 ms is the 7 ms baseline plus a
queue of 10 to 15 ms at the shaper, and a keyframe already on the wire when a key arrives still
drains ahead of the echo. Single maxima of 50 to 120 ms turn up in both orders, and at this load
they say little.

`a_session_frame_overtakes_queued_datagrams` (`crates/slopty-net/tests/loopback.rs`) queues
1 200 full datagrams, 1.4 s of a 1 MB/s link, ahead of a session frame. With streams first the
frame arrived after 1 or 2 of them. With `SLOPTY_DATAGRAMS_FIRST=1` it arrived after all 1 200,
and the test fails. Early in a connection it came after 16 either way. A packet that carries
an ACK has no room left for a full datagram, and noq then fills it with stream data, a file's
included. So noq's order was never strict, and the test waits out start-up before it floods.

## 2026-09-25 — a working mark that steps: frames drawn a second with one agent at work

The headless workspace (`slopty-ui` tests, the GPUI test platform), 1200 × 800, one worker, one
shell whose agent reports `Working`. Its mark shows in the navigator's tile row and in the
status bar's agent summary. The harness stands in for a 120 Hz display. Each of 120 ticks
moves the executor's clock by 1/120 s, delivers whatever frame was asked for
(`Window::simulate_next_frame`) and runs what is due. The count is the workspace's renders
(`frames_drawn`) in that second. Each arm ran five consecutive seconds.

```
cargo nextest run -p slopty-ui -E 'test(a_working_mark_draws_twelve_frames_a_second_and_none_at_rest)'
# the arms: `icons::status_icon` returning the static LoaderCircle (before), or the icon under
# `with_animation(Animation::new(1 s).repeat())` (a spinner turned on every frame), in place of
# `Spinner`; a temporary eprintln of `frames_in_a_second` printed the counts
```

| arm | frames drawn, seconds 1–5 |
| --- | --- |
| before: the mark stands still | 0, 0, 0, … |
| a spinner turned every frame (`with_animation`) | 120, 120, 120, 120, 120 |
| **the stepped spinner (`icons::Spinner`)** | **11, 12, 12, 12, 12** |
| the stepped spinner, the agent idle again | 1 (the step already due), then 0 |
| the stepped spinner under Reduce Motion | 0 |

A spinner that turns every frame keeps the window at the display's rate for as long as any
agent works, so it would be 120 frames a second on this display for hours. The stepped one
draws a frame when its twelfth of a second is up and not otherwise. It draws nothing once no
mark is painted. The first second reads 11 because the first step's timer runs from the frame
that painted the mark. The test asserts 11 to 13 while working and 0 at rest, so a return to
per-frame drawing fails it. The app itself was not measured here. Its display link and view cache
change the cost of each frame, not the count asked for.

## 2026-09-25 — the lines-below pill, and reading Reduce Motion

mac-studio, release build on main `1292763` plus the uncommitted UI work, load average 6–8
from other sessions.

**The lines-below pill.** Headless GPUI, one 100 × 40 terminal at 1000 × 900 pt flooding one
line a frame (each frame moves the screen down one line and sends the one new row). Three arms
in alternating blocks of 50 frames, ten rounds, so 500 frames each: following the output;
scrolled up 20 lines with the pill hidden (`set_covered(true)`); scrolled up with the pill
shown. A sample is the frame applied (`TerminalView::apply`, which now also holds the view
still while scrolled up) and drawn. Five runs, p50 / p95 / p99 / max in µs:

| run | following | scrolled, pill hidden | scrolled, pill shown |
| --- | --- | --- | --- |
| 1 | 177 / 210 / 235 / 319 | 161 / 178 / 223 / 263 | 177 / 216 / 271 / 389 |
| 2 | 176 / 228 / 290 / 320 | 162 / 196 / 245 / 272 | 177 / 211 / 277 / 380 |
| 3 | 175 / 202 / 245 / 393 | 162 / 199 / 239 / 262 | 177 / 211 / 259 / 319 |
| 4 | 174 / 201 / 252 / 317 | 162 / 187 / 238 / 327 | 175 / 199 / 255 / 302 |
| 5 | 176 / 209 / 259 / 309 | 162 / 189 / 231 / 285 | 178 / 212 / 244 / 306 |

```
cargo test -p slopty-ui --release --lib lines_below_cost -- --ignored --nocapture
```

The pill costs 15 µs at p50 and 15–25 µs at p95: a few boxes, an icon from the atlas and two
labels. That is 0.2 % of a 120 Hz frame, and a frame with the pill up costs what a frame
following the output costs. The count is `view_offset`, one read. Holding the view still is
two reads of the state and no rows, and a scrolled frame with the pill hidden is the cheapest
of the three.

**Reduce Motion.** `slopty_platform::reduce_motion()` was asked of AppKit on every frame, by
the canvas and by every terminal view's scrollbar. One read of
`NSWorkspace.accessibilityDisplayShouldReduceMotion` costs 110–240 ns, averaged over 100 000
reads in four runs, against 25–37 ns for the kept answer (a clock read and two atomics). It
is well under a microsecond, so a once-a-second refresh is enough. The crate watches no change
notification, and a change in System Settings shows within a second.

```
cargo test -p slopty-platform --release --lib reduce_motion -- --nocapture
```

## 2026-09-25 — noq's BBR3 against the draft: aggregation, ProbeBW_UP, ProbeRTT

"BBR3's window under bursty video" left three suspects in noq-proto 1.3.0's BBR3. This entry
checks each against draft-ietf-ccwg-bbr-06 (July 2026) and the editor's copy of 3 August,
Linux BBRv3 (`google/bbr` branch `v3`, `net/ipv4/tcp_bbr.c`) and Google's QUIC BBRv2
(`quiche`, `congestion_control/bbr2_*`). noq has had no BBR change since 1.3.0. Quinn's
BBRv3 is the PR noq's came from (quinn-rs/quinn#2481).

**The aggregation count.** The draft's `HandleRestartFromIdle` moves
`extra_acked_interval_start` to now and leaves `extra_acked_delivered` alone, and noq does the
same. Every byte delivered after an idle gap then counts as aggregation. Linux clears
`ack_epoch_acked` together with `ack_epoch_mstamp` on `CA_EVENT_TX_START`, and the draft's own
`OnInit` sets both. Slopty's noq now clears the count too (`vendor/noq-proto/SLOPTY.md`, patch
2). Linux also caps the allowance at 100 ms of bandwidth. The draft has no such cap, and with
the count cleared it does not bind here, so noq still has none.

**ProbeBW_UP.** The draft leaves `ProbeBW_UP` on loss, or on `full_bw_now`.
`CheckFullBWReached` counts only rounds that are not app-limited, and Linux does the same
(`bbr_check_full_bw_reached`, `case BBR_BW_PROBE_UP`). Neither has any other way out. noq's
pacer releases ten packets at once, so a 25 kB P-frame is out in about 4 ms, under one round
trip. The ACK that comes back finds nothing left to send and marks the next round app-limited,
as the draft's `CheckIfApplicationLimited` would. Only a keyframe gives non-app-limited rounds.
So video like Slopty's stays in `UP`, as the draft specifies. "BBR3's window under bursty
video" planned an exit in `UP` for an app-limited flow and called it Linux's. Linux has no such
exit. The check it had in mind is BBRv1's gain cycling, which does not leave the 1.25 phase for
an app-limited flow either. Nothing changed.

**ProbeRTT.** Every 5 s the draft and Linux cap the window at `max(0.5 × BDP, 4 packets)` for
200 ms and a round trip. The draft makes one exception, and noq has it. It skips ProbeRTT when
the expiry is found on the first ACK after a restart from idle (`idle_restart`). The draft also
expects quiet spells to refresh the round-trip estimate first. In its pseudocode only a sample
strictly below the minimum does that, and a clean round trip that only matches the minimum does
not. At 60 fps the connection is idle for only part of each frame period, and the expiry often
falls inside a burst. noq was faithful to the draft here but for one line. `HandleProbeRTT` starts
with `MarkConnectionAppLimited()`, so the low rates of a window held to half a BDP are not taken
for the path's, and noq left it out. Slopty's noq has it now (patch 3). quiche is not the draft.
It pushes the round-trip timestamp forward by each quiet spell (`avoid_unnecessary_probe_rtt`,
on by default) and checks for expiry only as `ProbeBW_DOWN` ends. That stretches the interval
by the share of time spent idle, and ProbeRTT still comes.

### The model, in noq-proto's unit tests

`VideoSim` (`vendor/noq-proto/src/congestion/bbr3/mod.rs`, tests) drives BBR3 through the calls
the connection makes. It sends the harness's traffic: 25 kB P-frames at 60 fps, a 130 kB
keyframe each second, a 20 Mbit/s link with 4 ms of round trip, and an ACK every second packet
or 2 ms after an odd one. A token bucket shaped like noq's pacer does the pacing. With
"1 ms ticks" the bottleneck releases packets on a 1 ms grid, as `slopty-shape` does, so ACKs
arrive in clumps. Sixty seconds after two of start-up, fixed seed:

| link | noq | Up / Cruise / ProbeRTT | ProbeRTT entries | window max | extra_acked max | P-frame p50 / p99 ms | P-frames handed over in ProbeRTT, p50 / max ms |
| --- | --- | --- | --- | --- | --- | --- | --- |
| ACKs as served | 1.3.0 | 37 / 42 / 3.2 % | 9 | 390 kB | 363 kB | 12.1 / 87.6 | 76.6 / 149 (113 frames) |
| ACKs as served | patched | 38 / 40 / 3.1 % | 9 | 76 kB | 9.0 kB | 12.1 / 96.4 | 86.2 / 128 (113 frames) |
| 1 ms ticks | 1.3.0 | 64 / 24 / 1.4 % | 4 | 359 kB | 330 kB | 12.7 / 63.0 | 72.0 / 109 (51 frames) |
| 1 ms ticks | patched | 64 / 22 / 2.5 % | 7 | 76 kB | 10.3 kB | 12.7 / 85.3 | 81.7 / 116 (85 frames) |

```
cd vendor/noq-proto && CARGO_TARGET_DIR=../../target/noq-vendor cargo test --lib \
  video_through_one_bottleneck -- --ignored --nocapture
rm Cargo.lock    # cargo writes one here; the vendored crate carries none
```

The "1.3.0" rows are the patched file with the two fixed lines taken out. The patch keeps the
window within a few times the 10 kB bandwidth-delay product. It does not touch the time in `UP`.
A P-frame handed over during ProbeRTT takes 70 to 90 ms against 12, because 60 % of the link is
more than half a BDP per round trip carries. That sets P-frame p99, and it moves with how many
ProbeRTTs a run happens to take. One keyframe landed in ProbeRTT in these four runs, and it took
156 ms. `restart_from_idle_starts_an_empty_ack_aggregation_interval` fails on 1.3.0 (extra_acked
155 kB against a 40 kB limit). `probe_rtt_marks_its_samples_app_limited` fails on 1.3.0 (ten of
ten packets sent in ProbeRTT unmarked).

### The harness

`echo_beside_a_video_flood`, release, 100 ms of queue, no loss. The unpatched arm used a
temporary `SLOPTY_BBR3_UNFIXED=1` switch in the vendored crate, since removed. Eight rounds of
four runs, patched against unpatched, bounded (`bbr3`, the default) against noq's window
(`bbr3-unbounded`), alternating which went first. The load average ran from 13 to 415, since
other sessions were building. An arm-run whose goodput fell below 12 Mbit/s (9 when halved) was
starved of CPU and is left out: 10 of 128. That leaves 6 to 8 runs a cell. Medians over runs,
echo p99's range across runs in brackets, "missing" is frames not delivered whole per run. Logs
are in `target/logs/bbr3fix/`.

```
SLOPTY_CC=<bbr3|bbr3-unbounded> \
  cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only --no-capture
```

| arm | controller | noq | echo p50 / p99 [runs] / max ms | queue p99 / max ms | lost | window p50 | keyframe p50 / p99 ms | missing | Mbit/s |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| bursts | bbr3 | patched | 9.7 / 19.1 [16.4–37.3] / 36.9 | 9.9 / 12.4 | 0 | 39 kB | 55.9 / 167 | 2–5 | 12.4–12.5 |
| bursts | bbr3 | 1.3.0 | 9.8 / 19.6 [14.9–51.2] / 24.2 | 9.5 / 11.1 | 0 | 37 kB | 55.7 / 76 | 2–4 | 12.5–12.6 |
| bursts | unbounded | patched | 10.2 / 23.9 [18.5–59.3] / 39.2 | **11.8 / 26.9** | 0 | **45 kB** | 56.0 / 131 | 2–4 | 12.1–12.6 |
| bursts | unbounded | 1.3.0 | 10.0 / 26.1 [20.3–54.4] / 48.8 | 17.4 / 48.6 | 0 | 5.4 MB | 55.8 / 162 | 2–3 | 12.4–12.6 |
| laned | bbr3 | patched | 9.8 / 22.8 [13.8–43.8] / 33.0 | 10.1 / 13.6 | 0 | 40 kB | 55.9 / 85 | 2–4 | 12.5–12.6 |
| laned | bbr3 | 1.3.0 | 10.2 / 27.6 [16.4–57.8] / 48.0 | 11.7 / 16.1 | 0 | 45 kB | 55.9 / 160 | 2–4 | 12.2–12.6 |
| laned | unbounded | patched | 9.9 / 25.5 [18.5–67.1] / 36.6 | **12.6 / 24.5** | 0 | **42 kB** | 55.5 / 168 | 2 | 12.3–12.6 |
| laned | unbounded | 1.3.0 | 9.7 / 26.4 [23.0–62.7] / 48.5 | 17.1 / 39.5 | 0 | 5.0 MB | 55.8 / 139 | 2 | 12.3–12.6 |
| metered | bbr3 | patched | 9.4 / 31.6 [12.1–48.6] / 49.9 | 5.9 / 9.4 | 0 | 38 kB | 63.3 / 104 | 2 | 12.2–12.5 |
| metered | bbr3 | 1.3.0 | 9.3 / 21.6 [11.8–49.7] / 40.7 | 5.3 / 8.8 | 0 | 44 kB | 62.7 / 101 | 1–2 | 12.4–12.6 |
| metered | unbounded | patched | 9.3 / 18.4 [13.3–31.1] / 28.2 | 5.3 / 8.7 | 0 | 41 kB | 63.0 / 110 | 2–3 | 12.4–12.5 |
| metered | unbounded | 1.3.0 | 9.3 / 22.9 [13.0–34.6] / 34.0 | 5.4 / 9.1 | 0 | 2.4 MB | 63.4 / 149 | 1–2 | 12.3–12.4 |
| halved | bbr3 | patched | 20.3 / 38.7 [26.8–45.6] / 52.7 | 28.4 / 30.4 | 0 | 25 kB | 157 / 281 | 1–5 | 9.3–9.7 |
| halved | bbr3 | 1.3.0 | 21.4 / 45.5 [32.2–80.1] / 57.4 | 25.7 / 27.6 | 0 | 30 kB | 159 / 280 | 3–4 | 9.0–9.7 |
| halved | unbounded | patched | 19.9 / **47.2** [32.0–72.8] / 67.0 | **27.0 / 57.6** | **0 in 8 runs** | 24 kB | 156 / 280 | 1–4 | 9.3–9.7 |
| halved | unbounded | 1.3.0 | 19.1 / 168 [27.5–201] / 211 | 195 / 201 | 107–241 in 6 of 8 | 24 kB | 156 / 254 | 3–41 | 9.5–9.6 |

What the numbers say:

* **The aggregation fix does what the bound did.** noq's own window went from 2.4 to 5.4 MB to
  41 to 45 kB, beside the bound's 37 to 45 kB. Unbounded, the bottleneck queue's p99 fell by
  a third in the bursts and laned arms and its maximum by half. When the link halved, noq's
  window as shipped filled the 100 ms buffer in six of eight runs and dropped 107 to 241
  packets. Patched, it filled none, and the queue peaked at 58 ms. Video gave nothing up:
  the same goodput, and the same frames delivered whole. The halved arm lost fewer frames
  (1 to 4 against up to 41).
* **Bounded, the patch changes nothing measurable.** The bound was already holding the window
  under noq's. Every bounded difference above sits inside the run-to-run spread at this load.
* **Keyframe p99 did not move, as expected.** It is two-valued. A run whose keyframes all miss
  ProbeRTT reads 55 to 77 ms, and one that catches ProbeRTT reads mostly 100 to 250 ms. About two
  arm-runs in three caught it in all four columns (16, 16, 17 and 18 of 24). The ProbeRTT mark
  changes what BBR3 learns from those rounds, not how long they hold the window.
* **Why the harness catches ProbeRTT so often.** The first round trip is sampled at the
  handshake, the first keyframe follows at once, and keyframes come every second. ProbeRTT falls
  due 5 s after the minimum was last refreshed, and it often lands on a keyframe. An encoder
  whose keyframe interval divides 5 s would do the same on a real link.

## 2026-09-25 — BBR3's bound on the Tailscale mesh

The mesh run the "noq's BBR3 follows the draft" decision left owed: keystroke echo while bulk
data leaves the same host over the same path, with the bound on (`bbr3`, the default) and off
(`bbr3-unbounded`, noq's patched window alone).

Setup. mac-studio drove macbook-pro (arm64, macOS 27.0) over Tailscale, direct to the
MacBook's public endpoint, not a LAN: 100.64.0.3 to 100.64.0.2, ICMP round trip 7 to 16 ms. On the
MacBook, ptyd, a worker (port 45650) and a server (port 45660) ran from `/tmp/slopty-meshbbr`
with a private `HOME`, release builds of main `79fb113`. Each arm-run started the worker and
server fresh with its `SLOPTY_CC` and then ran two things from the Studio.

* **Idle.** `slopty bench echo`, 100 keys, with nothing else flowing.
* **Loaded.** `slopty cat` pulled a 1 GiB file of random bytes on the MacBook through its
  server, restarting when it ended. After 3 s, `slopty bench echo` sent 300 keys, and then
  the script stopped the pull.

Eight rounds, the two arms interleaved, alternating which went first. Studio load average
11 to 22, as other sessions built. MacBook 2 to 5.

Two limits of what the CLI can do. `bench echo` opens its own connection, and `cat` goes
through the server, so the echo and the bulk ride two QUIC connections, both sent from the
MacBook under the same `SLOPTY_CC`. They are one WireGuard UDP flow on the wire, so they share
every queue on the path. The run measures the queue the bulk flow's controller builds, not
scheduling inside one connection. No verb puts a bulk transfer beside an echo on one
connection. And only the worker's client connections carry `SLOPTY_PATH_TRACE_MS`, so the bulk
connection's window and bytes in flight could not be read. The trace below is the echo
connection's.

```sh
# mac-studio: build and ship (the daemons' packages are slopty-workerd and slopty-serverd)
cargo build --release -p slopty-ptyd -p slopty-workerd -p slopty-serverd -p slopty-cli
R=/tmp/slopty-meshbbr
ssh macbook-pro "mkdir -p $R/bin $R/home $R/data $R/sdata $R/terminfo $R/drops"
for b in slopty-ptyd slopty-worker slopty-server slopty; do
  gzip -1 -c target/release/$b | ssh macbook-pro "gunzip -c > $R/bin/$b && chmod +x $R/bin/$b"; done
ssh macbook-pro "dd if=/dev/urandom of=$R/bulk.bin bs=1m count=1024"
# macbook-pro, per arm-run (ptyd once): CC is bbr3 or bbr3-unbounded
E="HOME=$R/home SLOPTY_TERMINFO_DIR=$R/terminfo TERMINFO_DIRS=$R/terminfo: \
  SLOPTY_PASTEBOARD=dev.aislopware.slopty.meshbbr SLOPTY_DROP_DIR=$R/drops"
env $E nohup $R/bin/slopty-ptyd --socket $R/ptyd.sock >$R/ptyd.log 2>&1 </dev/null &
env $E SLOPTY_CC=$CC nohup $R/bin/slopty-server --port 45660 --mcp-port 45661 \
  --data-dir $R/sdata >$R/server.log 2>&1 </dev/null &
env $E SLOPTY_CC=$CC SLOPTY_SERVER=127.0.0.1:45660 SLOPTY_WORKER_NAME=meshbbr \
  SLOPTY_PATH_TRACE_MS=50 RUST_LOG=info,slopty_net=debug nohup $R/bin/slopty-worker \
  --ptyd-socket $R/ptyd.sock --ctl-socket $R/worker.sock --data-dir $R/data --port 45650 \
  >$R/worker-$TAG.log 2>&1 </dev/null &
# mac-studio, per arm-run; per-key samples are the trace's `bench frame … us=`
S="target/release/slopty --data-dir /tmp/meshbbr/cli"
RUST_LOG=warn,slopty::bench=trace $S bench echo --worker 100.64.0.2:45650 --count 100
( while :; do $S --server 100.64.0.2:45660 cat --worker meshbbr $R/bulk.bin; done ) | wc -c &
sleep 3
RUST_LOG=warn,slopty::bench=trace $S bench echo --worker 100.64.0.2:45650 --count 300
# stop the loop, then its `slopty cat`; goodput = bytes / (end − start)
# macbook-pro, end of an arm-run: kill the worker and server; at the end ptyd too, and rm -rf $R
```

Medians over the eight runs of each arm, the range across runs in brackets. "Pooled" puts the
2400 loaded keys of an arm together. "srtt" is the echo connection's smoothed round trip from
the worker's trace while the load ran.

| | bbr3 (bound) | bbr3-unbounded |
| --- | --- | --- |
| idle echo p50 ms | 10.2 [9.6–11.1] | 10.1 [9.7–10.6] |
| idle echo p99 ms | 168 [106–224] | 120 [97–162] |
| idle keys over 50 ms, pooled | 12.4 % | 12.4 % |
| loaded echo p50 ms | 10.7 [10.2–12.9] | 10.5 [10.2–11.1] |
| loaded echo p90 ms | 85 [68–93] | 76 [43–97] |
| loaded echo p99 ms | 197 [136–293] | 176 [134–278] |
| loaded echo max ms | 318 [197–402] | 246 [197–787] |
| loaded, pooled p50 / p90 / p99 ms | 10.8 / 84 / 214 | 10.5 / 76 / 181 |
| loaded keys over 50 / 100 ms, pooled | 16.3 / 6.9 % | 13.5 / 5.4 % |
| bulk goodput Mbit/s | 77 [55–120] | 80 [69–119] |
| echo srtt p50 / p99 ms | 19.8 / 43 [32–76] | 20.2 / 38 [25–51] |
| keys lost to the 2 s timeout | 0 | 0 |

What the numbers say:

* **On bulk traffic the bound makes no difference that this path can show.** Loaded echo p50 is
  the same in both arms, and so is goodput. Unbounded reads 10 to 30 ms better in p90 and p99,
  but the idle runs differ by more, 168 against 120 ms at p99. Nothing flows there but the echo
  itself, whose window a single 240-byte frame cannot fill, so that gap is the path drifting
  between runs, not the controller. Bounded had the higher loaded p99 in six rounds of eight,
  and the higher idle p99 in six of eight as well. A single-host bulk pull did not make the
  bound cost anything or save anything.
* **The load adds to the tail, not the median.** Under load, echo p50 rose by half a millisecond
  and pooled p90 by 14 to 21 ms. Keys over 50 ms went from 12 % to 14 to 16 %. BBR3 keeps no
  standing queue here that the median key would wait behind, with or without the bound.
* **The largest term is the path's idle tail.** With nothing else flowing, one key in eight took
  over 50 ms and one in twenty-five over 100 ms, against a 10 ms median. The worker's QUIC
  counted no lost packets in 15 of 16 runs and two in the other. ICMP ping lost 13 to 21 %
  each way, which says more about how ICMP is treated than about the UDP path. This run does not
  find the cause. Keys lost on the way to the MacBook would not show in the worker's counters.
* **What this run cannot reach.** The case the bound still guards is noq#800 (open): an
  app-limited flow that never leaves `Startup` and paces at `2.77 × initial window / 1 ms`.
  A bulk pull is not app-limited. Video is, but a worker started over ssh has no Screen
  Recording, so this mesh run carried no video. The echo connection is app-limited. Its window
  sat at 5 kB and never bound, and one frame in flight cannot tell whether it would.

Logs, per arm-run (`.idle.log`, `.load.log`, `.load.meta`, `.worker.log`, `.uptime`), are in
`target/logs/meshbbr/`, with the scripts that ran the rounds (`all.sh`, `round.sh`,
`remote-up.sh`, `remote-down.sh`) and the one that tabulates them (`summ.pl`).

## 2026-09-25 — datagram copies of a keystroke and its echo

The mesh run above left the idle tail unexplained: one key in eight over 50 ms against a 10 ms
median, with the worker counting no lost packets. `slopty bench echo` now also prints the
client's own counters (`keys: … lost N pkts`), and they answer it. On the way to the MacBook
the Studio lost 20 to 93 packets per 100 idle keys, and 60 to 250 per 300 loaded keys, while
the worker lost at most 4 in any run on the way back. The lossy direction is the one into the
MacBook, most likely its Wi-Fi. A key is one sparse packet, and when it is lost nothing behind
it tells QUIC, so it waits for the probe timeout.

**What was built** (`docs/decisions/transport.md`). Each input request also goes once as a
datagram, and each diff the worker builds while it is answering input goes once as a datagram
too when it fits one. A copy leaves 2 ms after its stream copy, in a packet of its own. Each end
takes whichever copy comes first, in order only. `SLOPTY_ECHO_COPY` (`off`, a delay in ms,
or two delays such as `2,10`) switches it; the client reads it for the keys and the worker for
the echoes. The arms below are named by it: `off`, `0` (the copy written with its stream copy,
so the two share a packet), `2`, `2,10` (a second copy at 10 ms).

**Setup.** As in "BBR3's bound on the Tailscale mesh": mac-studio drove macbook-pro over a
direct Tailscale path (ICMP 7 to 16 ms), release builds of this change, ptyd, worker (port
45650) and server (45660) under `/tmp/slopty-echocopy` with a private `HOME`, the worker and
server restarted for each arm-run. Each arm-run was 100 idle keys, then 300 keys while `slopty
cat` pulled a 1 GiB file through the MacBook's server. Arms interleaved, the order rotating each
round. Load averages: Studio 3 to 21 (other sessions building), MacBook 2 to 6. The scripts
(`round.sh`, `remote-up.sh`, `remote-down.sh`, `reverse.sh`, `local.sh`, `all*.sh`) ran from
`/tmp/echocopy` and are kept, with every log, in `target/logs/echocopy/`; `summ.py` and
`summ_local.py` tabulate them.

```sh
# The scripts expect to live in /tmp/echocopy, the tree's release builds (slopty, slopty-ptyd,
# slopty-worker, slopty-server, slopty-shape) in target/release, and the same builds with
# pacing.rs.orig in place of the patched pacer in /tmp/echocopy/bin-a. On macbook-pro:
# /tmp/slopty-echocopy/{bin,bin-a} with the same binaries, remote-up.sh, remote-down.sh, and a
# 1 GiB bulk.bin.
cp target/logs/echocopy/*.sh target/logs/echocopy/*.py /tmp/echocopy/
/tmp/echocopy/round.sh 2 2 r1-2 full p     # one mesh arm-run: worker's copies, client's, tag
/tmp/echocopy/reverse.sh 2 off r1-w2-coff 150   # roles reversed: worker here, client there
SEED=1 /tmp/echocopy/local.sh q1-p2 2 2 yes 200   # loopback through the shaper (no: clean)
/tmp/echocopy/all.sh       # run 1; all2.sh run 2, all3.sh run 3, all4.sh run 4, all8.sh run 5
ROUNDS="1 2 3 4 5 6" /tmp/echocopy/all7.sh      # the shaped loopback
ROUNDS="$(seq -s ' ' 1 12)" /tmp/echocopy/all9.sh   # the clean loopback
python3 /tmp/echocopy/summ.py /tmp/echocopy/mesh
python3 /tmp/echocopy/summ_local.py q aoff a2 poff p2
/tmp/echocopy/stop-local.sh; ssh macbook-pro '/tmp/slopty-echocopy/remote-down.sh all'
```

Medians over runs with the range across them in brackets; "pooled" puts every key of an arm
together.

**Run 1, the delay** (six rounds):

| | off | 0 | 2 | 2,10 |
| --- | --- | --- | --- | --- |
| idle p50 ms | 11.2 [10.9–11.7] | 11.7 [10.9–12.5] | 11.9 [11.6–12.1] | 11.6 [11.1–12.0] |
| idle, pooled p50 / p90 / p99 ms | 11.3 / 66.9 / 189 | 11.6 / 62.5 / 159 | 11.8 / 30.7 / 159 | 11.5 / 30.6 / 115 |
| idle keys over 50 / 100 ms | 12.7 / 4.8 % | 12.0 / 3.8 % | 4.5 / 2.8 % | 4.7 / 2.3 % |
| loaded p50 ms | 11.6 [11.5–12.0] | 11.7 [11.3–11.8] | 12.6 [12.2–13.8] | 13.5 [12.7–14.5] |
| loaded, pooled p50 / p90 / p99 ms | 11.6 / 106 / 354 | 11.5 / 88.0 / 254 | 12.6 / 43.9 / 149 | 13.5 / 43.8 / 164 |
| loaded keys over 50 / 100 ms | 19.1 / 10.9 % | 16.3 / 8.0 % | 8.2 / 3.6 % | 8.3 / 3.5 % |
| key copies taken / late | 0 / 0 | 1 / 2013 | 312 / 1652 | 350 / 3517 |

A copy written with its stream copy shares its packet and is lost with it: of 2014 such copies
the worker took one. Sent two milliseconds later it goes alone, and the worker took 16 % of
them, each one a key whose stream packet was lost or late. A second copy at 10 ms bought nothing past
50 ms. Both separate-packet arms cost the loaded median a millisecond or two; that is taken up
below.

**Run 2, which direction** (six rounds, both at 2 ms):

| | off | echoes only | keys only | both |
| --- | --- | --- | --- | --- |
| idle, pooled p50 / p90 / p99 ms | 11.2 / 70.3 / 169 | 11.2 / 65.5 / 158 | 10.7 / 32.6 / 174 | 11.5 / 35.5 / 151 |
| idle keys over 50 ms | 14.5 % | 13.8 % | 5.5 % | 5.3 % |
| loaded p50 ms | 11.5 [11.2–12.2] | 11.6 [11.3–12.2] | 12.6 [12.3–13.4] | 12.2 [12.1–13.2] |
| loaded, pooled p50 / p90 / p99 ms | 11.5 / 95.5 / 240 | 11.6 / 76.8 / 184 | 12.7 / 43.1 / 172 | 12.4 / 40.1 / 178 |
| loaded keys over 50 ms | 18.6 % | 17.7 % | 7.8 % | 6.7 % |

The keys' copies carry the whole gain here, as they should when nothing is lost on the way
back. The echoes' copies cost nothing measurable, and the loaded median's millisecond comes
with the keys' copies.

**Run 3, roles reversed** (worker on the Studio at port 45750, client on the MacBook, 150 idle
keys a run, six rounds less the connects that failed): now the echoes cross the lossy direction. The worker lost 54 to 118
packets a run and the client none.

| | off | keys only | echoes only | both |
| --- | --- | --- | --- | --- |
| pooled p50 / p90 / p99 ms | 11.4 / 58.7 / 131 | 10.6 / 49.1 / 133 | 11.3 / 31.9 / 110 | 11.0 / 27.5 / 108 |
| keys over 50 / 100 ms | 12.3 / 2.2 % | 9.9 / 2.3 % | 4.1 / 1.3 % | 3.0 / 1.2 % |
| echoes shown from their copy | 0 | 0 | 118 | 114 |
| runs | 4 | 5 | 5 | 6 |

Four connects of 24 failed QUIC's 5 s handshake timeout on this direction (`no answer`) and are
left out; that is outside this change. One run in each copy arm had a single key of 0.96 and
1.66 s, a spell in which nothing at all reached the MacBook, stream or copy.

**Run 4, the delay again** (six rounds, both ends):

| | off | 2 | 5 | 10 |
| --- | --- | --- | --- | --- |
| idle, pooled p50 / p90 / p99 ms | 10.9 / 71.3 / 165 | 10.7 / 30.3 / 113 | 11.1 / 31.5 / 118 | 10.9 / 36.9 / 133 |
| idle keys over 50 ms | 14.0 % | 3.2 % | 4.0 % | 5.5 % |
| loaded p50 ms | 11.7 [10.7–12.2] | 12.7 [11.6–14.1] | 13.0 [12.1–13.6] | 12.5 [11.4–14.1] |
| loaded, pooled p50 / p90 / p99 ms | 11.6 / 85.2 / 232 | 12.5 / 36.6 / 138 | 12.8 / 38.9 / 145 | 12.6 / 38.6 / 146 |
| loaded keys over 50 ms | 16.9 % | 4.9 % | 6.3 % | 5.2 % |

Two milliseconds is best in the tail, and a later copy does not remove the loaded median's cost,
so the copy is not colliding with its own echo on the air.

**Where the millisecond went: noq's pacer.** On `slopty-shape` (4 ms each way, 13 % loss
each way, independent, so a round trip like the mesh's) the cost was larger: copies raised the
median from 14 to 18 ms while they cut p90 (the table below, `a`). Traced on one clock,
the copies of slow keys reached the worker 20 to 40 ms after they were sent. The worker's noq trace counted 170
`blocked by pacing` with copies and 80 without, each up to 33 ms. A send asks noq's pacer for a
whole MTU of credit, since the packet is not built yet, and below 125 kB/s the bucket held one
MTU, so every packet waited for the bucket to fill completely. BBR3 on a lossy connection that
sends a key now and then paces at tens of kB/s, so a small packet right after another waited for
the first one's bytes: 300 B at 20 kB/s is 15 ms. The copies added such second packets, and the
ACKs of copies did too. BBR3 reports `send_quantum`, the aggregate it means to let out together,
at least two packets; the vendored pacer now holds it (`vendor/noq-proto/SLOPTY.md`, patch 4;
`a_low_rate_lets_the_send_quantum_out_together` fails without it). Below 125 kB/s the bucket
held one MTU and now holds two; up to about 250 kB/s it holds the larger of the two; above that
nothing changes.

Loopback through the shaper, the pacer as shipped (`a`) or holding the quantum (`p`), copies off
and on, six rounds of 200 keys:

| | a, off (before) | a, copies | p, off | p, copies (after) |
| --- | --- | --- | --- | --- |
| p50 ms | 13.7 [13.5–15.0] | 18.0 [13.2–23.5] | 13.2 [12.1–13.6] | 12.4 [11.7–13.5] |
| p90 ms | 117 [111–143] | 73.9 [41.3–92.5] | 59.9 [47.3–79.2] | 18.1 [16.0–22.2] |
| pooled p50 / p90 / p99 / max ms | 14.0 / 124 / 363 / 654 | 16.6 / 67.7 / 238 / 590 | 13.1 / 58.6 / 148 / 806 | 12.3 / 18.5 / 77.0 / 169 |
| keys over 50 / 100 ms | 24.7 / 15.2 % | 16.7 / 6.6 % | 15.4 / 3.4 % | 2.2 / 0.7 % |

**Run 5, the mesh with both** (the same four arms; the MacBook went off the network during round
4, so four rounds idle and four loaded, three for the last arm, whose fourth pull broke):

| | a, off (before) | a, copies | p, off | p, copies (after) |
| --- | --- | --- | --- | --- |
| idle p50 ms | 12.3 [9.9–14.9] | 10.2 [9.8–10.6] | 10.6 [10.1–10.7] | 10.4 [10.0–12.8] |
| idle, pooled p50 / p90 / p99 ms | 11.4 / 66.7 / 134 | 10.2 / 19.5 / 57.9 | 10.4 / 36.9 / 65.7 | 10.5 / 16.4 / 40.6 |
| idle keys over 50 / 100 ms | 13.5 / 2.2 % | 1.5 / 0.2 % | 2.5 / 0.2 % | 0.5 / 0.0 % |
| loaded p50 ms | 10.9 [10.5–11.1] | 11.8 [10.9–12.5] | 13.1 [12.3–14.2] | 12.2 [11.5–12.6] |
| loaded, pooled p50 / p90 / p99 ms | 10.8 / 64.7 / 200 | 11.6 / 32.0 / 111 | 13.1 / 49.6 / 128 | 12.2 / 19.6 / 36.7 |
| loaded keys over 50 / 100 ms | 11.8 / 5.2 % | 2.9 / 1.3 % | 9.8 / 2.0 % | 0.7 / 0.1 % |
| bulk goodput Mbit/s | 82 [76–96] | 73 [67–92] | 178 [147–203] | 184 [168–197] |

The loaded medians are not comparable across the pacers: with the quantum held, the bulk pull
beside the echo ran at 2.3 times the rate, and the echo queued behind a pull that fast. Why the
bulk connection gained is not measured here. The patch changes the pacer only below 250 kB/s,
so it must spend time at such rates, perhaps after its losses. The run was cut short, and a
full six rounds with the bulk's pacing traced is owed (below).

**Clean loopback** (no shaper, 12 rounds of 200 keys): nothing lost, nothing copied too late to
matter, and no regression.

| | a, off (before) | p, off | p, copies (after) |
| --- | --- | --- | --- |
| p50 ms | 0.90 [0.68–1.08] | 0.87 [0.69–1.05] | 0.82 [0.58–1.09] |
| p90 ms | 1.94 [1.46–2.44] | 1.86 [1.38–2.28] | 1.84 [1.20–2.39] |
| pooled p50 / p90 / p99 / max ms | 0.90 / 1.93 / 10.4 / 38 | 0.83 / 1.90 / 7.5 / 56 | 0.80 / 1.85 / 9.6 / 48 |

`typing_through_a_lossy_link_lands_once_in_order` (`cargo test -p slopty-workerd --test e2e`)
types 150 keys 4 ms apart through the shaper at 20 % loss each way into a raw `cat`. The screen
shows each key once and in order, no frame asks for a resync, and both ends take copies (the
worker took 14 and 48 key copies in two runs, the client showed 8 echoes from theirs in the
first).

**Of record.** Run 5 stopped after four rounds when the MacBook left the network. It is not
rerun: measurements run on this Mac alone (2026-09-25 ruling), so the shaped loopback above is
the number of record, and its lossy link models the tailnet path these runs measured.

The bulk connection's pacing is worth tracing beside it, to explain its goodput.

## 2026-09-26 — a dead worker shown down and a restarted one found again

`cargo xtask e2e workers`: one app and two workers on this Mac, with worker B behind the
`slopty-shape` relay shaped like the tailnet (`harness::TAILNET`: 8 to 12 ms round trip, 3 %
loss). Worker B is killed with SIGKILL mid-stream and started again on its port. The test
restarts it as soon as the app shows it down. The new last scenario kills it again, waits for
the app to give the link up, keeps it away 3 s more and then restarts it. Three runs each, on
the tree before this change and after it (`docs/decisions/workers.md`, "A worker that dies
shows down in seconds and one that comes back is found in one").

```sh
cargo xtask e2e workers            # the MEASURE lines; RUST_LOG via --log
cargo xtask e2e workers --log "info,noq_proto::endpoint=debug"   # shows the stateless resets
cargo nextest run -p slopty-net --test loopback restarted ping --no-capture
```

| | before (8 s / 15 s bars, 5 s keep-alive, 1 s → 10 s backoff) | after |
| --- | --- | --- |
| shown down after the kill | 9.6, 8.7, 8.8 s | 3.3, 3.5, 3.4 s |
| connected again after a restart while shown down | 8.4, 8.2, 8.2 s | 0.8, 0.6, 0.8 s |
| link given up after the kill | (15 s bar) | 5.7, 5.6, 5.7 s |
| connected again after a restart 3 s past the drop | (not run) | 0.6, 0.6, 0.5 s |
| whole run | 22.8, 20.7, 20.8 s | 17.6, 16.6, 16.6 s |

Logs: `target/logs/workers-before-{1,2,3}.log`, `target/logs/workers-after2-{1,2,3}.log`. Two
more runs on the final tree, with the refused-close patch and the tick policy factored out,
read 3.7 / 3.4 s down, 0.8 / 0.7 s back, 5.6 / 5.7 s given up and 0.6 / 0.5 s back past the
drop (`target/logs/reconnect-e2e-workers.log`, `target/logs/reconnect-e2e2-workers.log`).

**Where the 8 s went.** The worker's keep-alive was 5 s and the app's silence bars 8 s (shown
down) and 15 s (given up), so a restarted worker was found only after the 15 s bar and the 1 s
redial. The diagnostic run (`workers-diag.log`) showed the restarted worker dropping the old
connection's packets as `dropping packet with invalid CID`: noq's connection-ID generator takes
a random key per process, so the stateless reset `docs/decisions/transport.md` counted on never
went out. With a fixed key, a PING under the null crypto was still only 13 to about 20 bytes,
and noq answers nothing shorter than 22 bytes with a reset. The loopback test fails the same way with the
header sample set back to 0 (no reset within 3 s) and passes with it: a bare keep-alive draws
the reset about 0.5 s after the restart, and a ping draws it at once.

**Intermediate run.** With the 5 s handshake timeout still in place, the scenario 3 s past the
drop reconnected in 1.3, 1.4 and 1.3 s (`workers-after-{1,2,3}.log`), while the other numbers
matched the table. The dial that was running when the worker came back had backed its Initial
probes off to 2 s apart. A 2 s timeout ends that dial and the next one asks at once. How much
this gains depends on where in the dial the restart lands, so the run measures one point of
it. The bound is the part that holds: at most about 2 s between two probes, where it was about
3 s.

**Cost on a healthy link.** One keep-alive PING a second from whichever side's timer fires
first, and its ACK, each at least 29 bytes of UDP payload, on a link with nothing else to send.
A link that carries anything sends no keep-alives at all.

## 2026-09-26 — the app through a server

`cargo xtask e2e through-server`: a `slopty-server`, worker `studio` on loopback and worker
`remote` behind the `slopty-shape` relay shaped like the tailnet (`harness::TAILNET`: 8 to 12 ms
round trip, 3 % loss), both registered with it, and the app at its first run, connected by
typing the server's address into the panel (`docs/decisions/workers.md`, "The app is tested
the way it is used: through a server"). `remote` is killed with SIGKILL and restarted twice:
first as soon as the app shows it down, then after the server has called it unreachable and
the app holds it. Five runs on this Mac. The first two ran before the directory cache got its
one writer and the far worker's `HOME` was canonicalised, and neither change is on the paths
measured here.

```sh
cargo xtask e2e through-server     # the MEASURE lines
```

| | runs 1 to 5 | bound |
| --- | --- | --- |
| both workers connected after the server's address is entered | 0.03, 0.18, 0.08, 0.03, 0.04 s | |
| far's hook badges the pill | 33, 20, 22, 20, 19 ms | |
| far shown down after the kill | 3.1, 2.7, 2.4, 2.7, 2.9 s | 5 s |
| connected again after a restart while shown down (the app's own link) | 0.88, 0.78, 0.84, 0.76, 0.77 s | 2 s |
| held as unreachable after the kill (the server's 5 s lease, then its word) | 6.2, 6.2, 6.1, 6.2, 6.1 s | |
| connected again after a restart while held (the server lists it online) | 0.12, 0.13, 0.13, 0.13, 0.12 s | 2 s |
| whole run | 14.6, 19.4, 19.6, 18.4, 18.9 s | |

Logs: `target/logs/through-server-{1,2,3,4,5}.log`. Runs 1 to 3 wrote the golden
(`--accept`); 4 and 5 compared against it.

Shown down comes in under the 3 s silence bar because the bar counts from the last datagram
heard, which came up to a keep-alive before the kill. A restart the server announces is found
faster than one the app's own link finds, 0.13 s against 0.8 s. The online event wakes the held
connect loop at once, and the dial is a single handshake over the shaped link. The link's own
path waits for a ping to draw the reset and then for the 250 ms redial. The first row, 0.03 s
from Enter to both workers connected, is short because the link the panel dials to prove the
server is kept as the directory's link, and each worker's dial is one handshake.

## 2026-09-26 — a workspace frame over a large registry

A frame of the headless workspace (GPUI test window, 1200 × 800, debug profile) with one worker
holding 120 notes of 4 KB, 60 file cards and 12 shells, few of them near the view: the work a
frame does that grows with the registry rather than with what is drawn. The test notifies the
workspace and times the draw that follows, 20 frames of warm-up then 400 (nearest-rank
percentiles). Both builds ran from a scratch export of `HEAD` with only this change's files on
top (`/tmp/slopty-measure`, its own target dir), so other sessions' edits in the checkout
did not enter either number. Other builds were running on the machine (162 to 211
`cargo`/`rustc` processes).

| build | mean | p50 | p95 | max |
| --- | --- | --- | --- | --- |
| before, run 1 | 17.5 ms | 15.5 ms | 31.1 ms | 93.7 ms |
| before, run 2 | 15.4 ms | 15.0 ms | 16.0 ms | 57.6 ms |
| after, run 1 | 9.2 ms | 9.2 ms | 10.5 ms | 12.5 ms |
| after, run 2 | 9.3 ms | 9.2 ms | 10.7 ms | 11.5 ms |

The change (`docs/decisions/workspace.md`, "A frame does only the work its drawing needs"):
notes, file cards and pages are matched to the items only on the frame after a registry or a
link changed, instead of every frame cloning every note's text and rebuilding the file lists;
the layout's frame is built once per draw, not twice; the waiting agents are counted once, not
four times; note bodies are drawn from GPUI's view cache. What is left of the 9 ms is the
drawing itself (the navigator lists every tile) and the layout's frame over 192 tiles.

```sh
# the measurement test; prints one MEASURE line
cargo test -p slopty-ui --lib measure_a_frame -- --ignored --nocapture
```

Logs: `target/logs/agent-ui-measure-before.log`, `target/logs/agent-ui-measure-after.log`.

## 2026-09-26 — serving a history fetch, and scanning the output for prompt marks

Two worker paths the output and the scrollback go through, measured before and after
trimming them (`docs/decisions/terminal.md`, "The history cache keeps what the view is
near"). Mac Studio, release, other sessions' builds running on the machine throughout, so each
figure is given with its spread over three runs.

**`FetchLines`**: one 4096-row chunk (the worker's cap) of 80-column rows read from the oldest
history of a 50 000-line scrollback, half the rows plain and half with bold, colour and
underline runs, ten reads per run.

| build | p50 per chunk (3 runs) | per row |
| --- | --- | --- |
| before | 11.95–12.2 ms | 2.9 µs |
| after | 8.1–9.4 ms | 2.0 µs |

A side-by-side run of both `read_line`s in one binary put the fastest of ten reads at 11.5–11.8
ms before and 8.2–9.6 ms after. `read_line` now skips the per-cell lookups the row's flags say
are empty (no styling, no multi-codepoint cluster, no link), resolves a style once per run of
cells sharing its id, asks for the semantic content past the first cell only on a prompt's
rows, and builds a cell's text on the stack instead of a `String`. What is left is mostly the
grid lookup itself: `Terminal::grid_ref` per cell plus `cell()` alone cost 3.0–4.2 ms of the
chunk. Stepping a `GridRef` along a row would need an API in the libghostty-rs fork, which
this change did not touch.

**OSC 133 scanner**: 30 000 OSCs that are not marks (an OSC 8 link opened and closed around a
file name and a title, as `ls --hyperlink` and prompt themes write them), twenty scans.

| build | p50 per OSC (3 runs) |
| --- | --- |
| before | 54–55 ns |
| after | 18–19 ns |

The scanner allocated a `Vec` for every OSC and stepped through its payload byte by byte. It
now keeps the payload on the stack, and an OSC whose first bytes are not `133;` is skipped to
its terminator with `memchr2`.

```sh
cargo nextest run -p slopty-engine --release --run-ignored only \
  -E 'test(fetch_lines_cost) | test(osc_scan_cost)' --no-capture
```

Logs: `target/logs/term-bench-before.log`, `target/logs/term-bench-after.log` (one run each).

## 2026-09-26 — the ptyd tap, framed once

mac-studio (M1 Max), release build, while other builds ran on the machine (the spread below is
theirs). Each read the session actor takes from a master is copied to ptyd (the tap), and now
and then the whole terminal state follows (the checkpoint). Before: the actor copied the read
into a `Vec`, the tap task copied it into the frame, and serde wrote a `Vec<u8>` one call per
byte, both ways. After: the actor builds the frame where it read (`OutputFrame::new`), and
every byte field of the ptyd protocol is a byte string, the same bytes on the wire
(`byte_strings_keep_the_wire_of_a_sequence_of_bytes`, `the_borrowed_output_frame_is_the_requests_frame`).

```sh
cargo nextest run -p slopty-pty -p slopty-ptyd --release --run-ignored only \
  -E 'test(tap_copy_cost) | test(checkpoint_transfer_cost) | test(line_discipline_cost)' --no-capture
```

| | before | after (3 runs) |
| --- | --- | --- |
| one 64 KiB read made a tap frame, on the actor's thread | 213, 83, 72 µs (copied, then per byte) | **5.4, 3.7, 1.2 µs** (framed once) |
| 4 MiB `Checkpoint` to ptyd, then `List`, p50 | 14 / 11 / 18 ms (2026-09-06 entry above) | **0.97 / 0.81 / 0.97 ms** |
| same, max of 20 | 87 / 38 / 138 ms | 2.6 / 2.6 / 2.7 ms |
| `tcgetattr` on a master, per read (new) | — | 282, 283, 284 ns |

The per-byte encoding was the cost, not the copies: about 1.5 ms per MiB. A flood of 64 KiB
reads spent more time building taps than feeding the engine. The last row is the price of the
line discipline the actor now reads after every read (`docs/decisions/terminal.md`, "The frames
say when the tty stops echoing"): one syscall, well under a microsecond.

## 2026-09-26 — encoder sessions off the runtime

mac-studio (M1 Max), release build, busy machine. A window stream builds a VideoToolbox session
when it opens, on every quality change that is not a bitrate alone, and when its window is
resized; the old session is invalidated in the swap. All three ran on the stream's task, on a
runtime worker thread, and held that thread for the whole build. They now run on the blocking
pool (`build_encoder`, `retire` in `crates/slopty-worker/src/screen.rs`). One build and drop of
a 3024 × 1964 HEVC session, twenty times each way:

```sh
cargo nextest run -p slopty-worker --release --test screen --run-ignored only \
  -E 'test(encoder_build_stall)' --no-capture
```

| runtime thread held per build | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| built on it, p50 / max | 3.5 / 42 ms | 4.3 / 5.1 ms | 3.7 / 4.3 ms |
| built on the blocking pool, p50 / max | **3 / 24 µs** | **3 / 38 µs** | **2 / 40 µs** |

A worker thread held for 4 ms is every task queued behind it held too: another client's session
pump, a heartbeat, a receiver report.

## 2026-09-26 — an echo copy too large for the path is not built

A datagram copy of an echo frame (`slopty_net::echo`) was built for every echo frame: the frame
copied into a new datagram, a task spawned, a 2 ms timer, and only then the check that the path
carries a datagram that large, which a screenful redraw never passes. The worker now checks the
least the copy can be (media header plus the frame) against `max_datagram_size` before it builds
anything (`copy_frame` in `apps/slopty-worker/src/conn.rs`). Per echo frame over the path's
limit (about 1.2 kB): one allocation and copy of the frame, one task and one timer before; none
after. Nothing changes for a frame that fits. No bench: the saving is the work removed, counted
above.

## 2026-09-26 — the branch at each prompt

The session actor now finds a shell's repository **and** its branch (`HEAD` read as a file) when
the shell reports its directory and when a command ends (`docs/decisions/workers.md`, "A
session's summary carries its branch and its start"). The shell integrations send OSC 7 at every
prompt, so this runs once per prompt. mac-studio (M1 Max), release, while other builds ran
(load average 14–16). Each figure is the mean of 20 000 calls on a temp tree on the internal
disk, with the directory four levels below the root:

```sh
cargo nextest run -p slopty-worker --release --lib --run-ignored only place_cost --no-capture
```

| | 3 runs |
| --- | --- |
| repository root (canonicalise, walk up for `.git`), which ran before the prompt's frame until now | 21.7, 21.4, 21.3 µs |
| branch (find `HEAD`, read it) | 15.1, 15.0, 15.1 µs |
| both, what a prompt now costs | 41.2, 39.7, 38.2 µs |

The file system is slower than the frame. Until now the root walk ran inside the OSC 7 handler,
so every prompt's frame waited about 21 µs for it. The actor now only notes that the place is
due and resolves it after the read's frame has gone out. The prompt's frame waits for none of
the 40 µs, and the viewers hear a `Cwd` event only when the directory, the repository or the
branch has changed, where before they heard one at every prompt.

## 2026-09-26 — failed blocks in the paint

A frame of the terminal view with command blocks on screen: `terminal::view::tests::failed_blocks_cost`
(headless GPUI, release, mac-studio) draws a 100 × 40 screen of ten four-row blocks, every
other one failed, and times a notify and the draw it brings, 2 000 frames with the pointer off
the window and 2 000 with it over a failed block's output, in alternating blocks of 200 after a
warm-up round. "Before" is the index as it stood (the red separator, no hover); "after" adds the
error bar and wash on the failed blocks' rows and the hovered block's facts. Both binaries were
built from one scratch export, with only this change's files differing, and run four times
each, alternating, while other sessions built on the machine. Headless GPUI shapes with a no-op
text system, so the numbers are the element's and the layout's own work, not CoreText's.

| build | pointer away, p50 | pointer over a failed block, p50 |
| --- | --- | --- |
| before (four runs) | 135.7–136.3 µs | 135.8–136.5 µs |
| after (four runs) | 136.2–137.3 µs | 151.1–152.1 µs |

The bars and washes cost about 1 µs a frame: `TermState::failed_runs` reads the marks of the
rows once, with two indexed prompt lookups, and each failed block adds two quads. The facts a
hovered block shows cost about 15 µs a frame, and only while the pointer rests on a block: the
status, the duration and the "…" button are GPUI elements laid out over the grid. The p95 and
p99 moved with the load on the machine, not with the build (before: 142–509 µs p95; after:
144–521 µs).

```sh
# the measurement test; prints one MEASURE line
cargo test -p slopty-ui --release --lib failed_blocks_cost -- --ignored --nocapture
```

Logs: `target/logs/terminal-agent-measure-interleaved.log`.

## 2026-09-26 — what the navigator adds to a frame

The navigator's rows became two lines with a leading slot, a second line and an age, its
worker headers gained a count and a fixed trailing slot, and it now draws every frame the
workspace does (`docs/decisions/ui.md`, "The navigator runs the window's height, and the
workspaces are tabs"). The same workspace (one worker, 60 shells with a directory, a branch and
a start, and 60 notes) drawn 400 times after 20 of warm-up, first with the navigator docked
and then hidden by ⌘B, in one debug test build. The difference is the navigator: its listing
(status, title, second line and age of every tile) and its rows' elements. Mac Studio, other
sessions' builds and an e2e run on the machine throughout (load average about 20).

| run | docked p50 | docked p95 | hidden p50 | hidden p95 |
| --- | --- | --- | --- | --- |
| 1 | 7.70 ms | 30.05 ms | 1.24 ms | 1.48 ms |
| 2 | 7.58 ms | 12.52 ms | 1.28 ms | 1.51 ms |
| 3 | 9.50 ms | 10.45 ms | 1.69 ms | 1.87 ms |

About 6.5 ms of a debug frame at p50 for 120 tiles, all of them laid out whether or not the
list shows them. The next step, not taken here, is a virtualised list that lays out only the
rows in view; this number is the baseline it has to beat.

```sh
cargo test -p slopty-ui --lib measure_the_navigator -- --ignored --nocapture
```

Log: `target/logs/navigator-measure.log`.

## 2026-09-26 — the navigator as a virtual list

The navigator's rows became GPUI's `list` over a `ListState`, which lays out and draws only
the rows in view and two tile rows' height past each edge (`workspace/navigator.rs`). Each
frame still works out what every row says (the filter, the attention order and a folded
worker's rollup need all of them), but builds elements for about 20 rows instead of 120. The
same test and workspace as the entry above, in one test build (the `dev` profile). The
baseline was taken the same afternoon, before the change, at a load average of about 120; the
runs after at about 70.

| run | docked p50 | docked p95 | hidden p50 | hidden p95 |
| --- | --- | --- | --- | --- |
| before | 7.66 ms | 33.61 ms | 1.24 ms | 1.50 ms |
| after 1 | 2.44 ms | 15.86 ms | 1.23 ms | 12.78 ms |
| after 2 | 2.50 ms | 18.33 ms | 1.24 ms | 4.64 ms |
| after 3 | 2.46 ms | 4.55 ms | 1.88 ms | 5.83 ms |

What the navigator adds at p50 fell from about 6.4 ms to about 1.2 ms. The p95s are the
machine's load, not the list: the hidden runs spread as widely. What is left is the listing
of all 120 tiles and the rows in view. The list measures every row once, when it first shows
or its width changes, so dragging the navigator's edge still lays out every row in each frame
of the drag.

```sh
cargo test -p slopty-ui --lib measure_the_navigator -- --ignored --nocapture
```

Log: `target/logs/navigator-measure.log`.

## 2026-09-26 — target/ growth under varied check sets

What one `cargo clippy --all-targets --target aarch64-apple-darwin` adds to a fresh target dir
per crate set, run in order, before and after `cargo xtask check` named `workspace-hack` beside
the crates (`docs/decisions/tooling.md`, "target/ stays bounded"). Units are `.fingerprint`
entries (one per crate build: a library, a test, a build script). Mac Studio, sccache warm,
other sessions building throughout (load average 20–30), so the seconds are rough; the unit
counts are not.

| crate set, in order | before: s | units compiled | units in dir | size | after: s | units compiled | units in dir | size |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `slopty-proto` | 54 | 57 | 89 | 215 MB | 687 | 436 | 634 | 938 MB |
| `slopty-proto slopty-net` | 49 | 88 | 196 | 509 MB | 54 | 22 | 663 | 1.0 GB |
| `slopty-net` | 12 | 10 | 211 | 618 MB | 3 | 0 | 663 | 1.0 GB |
| `slopty-client` | 19 | 32 | 254 | 772 MB | 71 | 13 | 685 | 1.1 GB |

Before, `slopty-proto` was checked three times (once per set) and every set added units for
the same crates; after, it was checked once and a set only adds its own crates. The first
check pays for the whole feature union (GPUI included) once per target dir.

The same once-only cost in the shared `target/debug`: the first `cargo xtask check -p xtask`
took 1 900 s (clippy 226 s, nextest 1 251 s building the union's libraries, rustdoc 420 s) at a
load average of 110–150; the next one took 90 s. Afterwards `cargo clippy -p slopty-proto -p
workspace-hack` compiled nothing after a `-p slopty-grid -p slopty-proto` run.

The sweep (`cargo xtask prune`), first passes on 2026-09-26: `target/` was 212 GB. The first
automatic passes deleted the gate lanes' incremental caches older than a day (16.4 GB by the
dry run). `cargo xtask prune --idle-hours 3` then deleted 1 230 caches untouched for three
hours, 48.5 GB (22.7 GB in `debug`, 21.1 GB in the gate lanes). `target/` was then 163 GB.
Units go only after a full idle window, and the first pass only opened one, so `debug/deps`
(85 GB, 113 builds of `slopty-client`, 41 of `gpui`) shrinks from the next day on.

```sh
# before: plain sets; after: add `-p workspace-hack`, each in a fresh CARGO_TARGET_DIR
for set in "-p slopty-proto" "-p slopty-proto -p slopty-net" "-p slopty-net" "-p slopty-client"; do
  cargo clippy ${=set} --all-targets --target aarch64-apple-darwin
  ls target/tmp/probe/{debug,aarch64-apple-darwin/debug}/.fingerprint | wc -l
done
```

## 2026-09-26 — an echo owed for want of room, framed when room returns

mac-studio, the test profile (optimised), while other sessions built (load average 120–160). A
viewer's connection holds both frames in flight (the echoes of two keys, each framed as it was
read), a third key is typed, and 2 ms later the connection is done with one frame. The echo read
meanwhile was owed, because the viewer had no room. The number is the wait from the freed
place to the frame that acknowledges the third key, 12 rounds a run.

```sh
cargo nextest run -p slopty-worker --test session_actor \
  -E 'test(an_echo_owed_for_want_of_room_goes_when_room_returns)' --no-capture
```

| | p50 | p90 | max |
| --- | --- | --- | --- |
| before, run 1 | 4.74 | 7.75 | 10.53 ms |
| before, run 2 | 6.48 | 7.12 | 7.77 ms |
| before, run 3 | 5.76 | 6.49 | 8.48 ms |
| after, run 1 | **0.08** | 0.13 | 0.16 ms |
| after, run 2 | 0.11 | 0.41 | 2.10 ms |
| after, run 3 | 0.07 | 0.09 | 0.15 ms |

Before, `frame_room` built the owed diff only on the 8 ms pace, counted from the frame that had
just gone, and the timer added tokio's millisecond rounding. The input exemption
(`EchoBurst`) applied only to output as it was read. Now the room a frame gives back spends
the same budget, and the read that found no room keeps its frame of the budget instead of
spending it on a flush that could not send (`Actor::echo_frame`).

## 2026-09-26 — capture on a 75 Hz display, data before parity, the reassembler's tick, UDP drops

mac-studio (M1 Max), main display 1920×1080 at **75 Hz** (`system_profiler SPDisplaysDataType`
and the worker's own listing agree), load average 107–196 from other sessions throughout, so
tails here are the machine's. Source: a Ghostty window running `yes` (1120×852, mostly on the
window filter because other windows overlap it), launched by hand and killed after.

### Capture against encode, before (the installed worker, release `d8139d6`)

The installed launchd worker is the only process here with Screen Recording, so the "before"
is it, driven by the release CLI on loopback, 10 s a run:

```sh
target/release/slopty bench screen --worker 127.0.0.1:45550 --list          # display 3, the window
target/release/slopty bench screen --worker 127.0.0.1:45550 --window <id> --seconds 10 [--fps 75|120]
target/release/slopty bench screen --worker 127.0.0.1:45550 --display 3 --seconds 10 [--fps 75|120]
```

| target | ceiling | captured / s | encoded / s | decoded fps | arrival gap p50 | capture→decoded p50 / p90 |
| --- | --- | --- | --- | --- | --- | --- |
| display | 60 (default) | 52.8 | 34.9 | 34.7 | 30.5 ms | 8.4 / 11.8 ms |
| display | 60 (default) | 48.3 | 32.9 | 32.9 | 31.0 ms | 8.5 / 15.4 ms |
| display | 75 | — | — | 67.9 | 13.8 ms | 7.4 / 12.1 ms |
| display | 120 | 54.4 | 54.4 | 55.2 | 13.6 ms | 5.2 / 10.7 ms |
| window | 60 (default) | 47.8 | 47.4 | 47.4 | 17.9 ms | 9.9 / 15.2 ms |
| window | 60 (default) | 54.9 | 54.6 | 54.5 | 17.6 ms | 9.5 / 11.8 ms |
| window | 75 | 65.1 | 64.9 | 64.8 | 14.0 ms | 8.8 / 11.0 ms |
| window | 120 | 66.2 | 65.8 | 65.8 | 13.4 ms | 10.2 / 12.2 ms |

On the display path at the default ceiling the cadence gate threw away a third of what was
captured: `minimumFrameInterval` 1/60 on a 75 Hz beat delivers a mix of 13.3 and 26.7 ms gaps,
and a gate measured from the last encoded frame rejects every 13.3 ms one (13.3 + 2.1 < 16.7),
so 33–35 fps went out of a 60 fps rung with 30 ms between frames. The window filter composites
off the display's beat, so its captures rarely met the gate (474 of 478 encoded); what capped it
was the 1/60 interval. With a ceiling at or above the panel's rate every capture passes, 65–68
fps, with Ghostty's own drawing under this load the limit. (The worker's counters are missing
from the 75 row: that run did not print them.)

### After: the capture on the display's beat, the gate on the rung's schedule

Capture now asks for `minimumFrameInterval` = `kCMTimeZero` (SCStream.h: "capture at display's
native refresh rate") whenever the target's display refresh is known, the frame rate is the
ceiling or the display's rate, whichever is lower, and `slopty_media::Pace` keeps the rung's
schedule with the lateness carried as credit. The gate on a synthetic beat (the unit tests
`a_rung_under_the_display_rate_keeps_its_rate_on_the_displays_beat` and
`a_pause_restarts_the_schedule_from_the_capture_that_ends_it`):

| beat | rung | old gate (from the last encoded) | `Pace` |
| --- | --- | --- | --- |
| 75 Hz | 60 | 37.5 fps, every gap 26.7 ms | **60.0 fps**, four in five, widest gap 26.7 ms |
| 75 Hz | 75 | 75 | 75, with ±0.4 ms jitter too |
| 60 Hz | 30 / 15 | 30 / 15 | 30 / 15 |
| 120 Hz | 60 | 60 | 60 |

**Owed, on hardware:** the same bench against a worker built from this change. It needs the
launchd worker reinstalled (`cargo xtask sign`, `slopty worker install`), which this session was
not to restart; a worker started from a shell here has no Screen Recording (`slopty worker
doctor`: ✘). Expected: display at the default ceiling 60 fps encoded (from 33–35), arrival gap
p50 13.3 ms.

2026-09-26, the worker reinstalled from `503ac1b` (doctor: both ticks). The display bench at the
default ceiling read 318 captured in 10 s, 0 dropped, 31.7 fps decoded, capture→decoded p50 5.0 /
p90 7.2 ms. That is not the "after" row: the display was mostly still, and ScreenCaptureKit only
delivers a frame when something on it changes, so 32 captures a second is the content's rate,
not the pace's. The moving source did not come up (a Ghostty `-e yes` window launched from the
shell was listed but never delivered a frame), and the other windows on screen belong to the
user, so none was used. The row stays owed: it needs a window this session starts that keeps
drawing at the panel's rate.

### Capture to the glass

Unreachable when this entry was written: a worker started from a shell cannot capture. Measured
from drawn pictures on 2026-09-28 ("capture to the glass and input to the glass, from drawn
pictures").

### Data before parity

```sh
cargo nextest run -p slopty-media --release --run-ignored only -E 'test(packetize_cost)' --no-capture
```

`packetize` hands the data datagrams on before Reed–Solomon runs over them, then the parity. The
"whole frame" column is when the first datagram left before (nothing went until the frame was
cut), the next when it leaves now. Three runs, µs:

| frame | whole frame cut | first datagram handed on |
| --- | --- | --- |
| 300 KB keyframe, 200‰ (305 datagrams) | 596 / 273 / 268 | **80 / 34 / 29** |
| 62 KB P-frame, 200‰ (64 datagrams) | 30.1 / 31.5 / 29.4 | **5.9 / 5.3 / 5.1** |
| 300 KB keyframe, no parity | 104 / 31 / 41 | 95 / 29 / 35 |

The 131 µs of the 2026-09-24 table is this machine quiet; the ratio is what carries. The worker
sends in two calls a frame (data, then parity) under the packetizer's lock, so a NACK's answer
never overtakes the frame.

### The reassembler's 2 ms tick stays

The client's stream loop sleeps a fixed 2 ms while frames are pending. The candidate slept until
the reassembler's next deadline instead (the next NACK, retry, loss deadline or refresh repeat,
at most 25 ms). A lost tail fragment is silence, so its NACK waits on this timer and on nothing
else. The number is the NACK's send past its due time (the last fragment's arrival plus the
2.5 ms delay at a 10 ms RTT), 200 frames a run. Both builds came from one target dir and ran
alternately:

```sh
cargo test -p slopty-client --lib tail_loss_nack_lateness -- --ignored --nocapture
```

| run | load | 2 ms tick p50 / p90 / p99 / max | next deadline p50 / p90 / p99 / max |
| --- | --- | --- | --- |
| 1 | 180–196 | 2.35 / 5.21 / 10.37 / 15.62 ms | 1.99 / 4.71 / 7.97 / 11.49 ms |
| 2 | 180–196 | 1.38 / 3.69 / 7.97 / 10.96 ms | 1.65 / 3.04 / 8.63 / 14.48 ms |
| 3 | 180–196 | 1.30 / 3.16 / 9.16 / 11.37 ms | 1.64 / 3.03 / 8.07 / 19.62 ms |
| 4 | 40–60 | 1.48 / 3.90 / 7.01 / 8.03 ms | 1.82 / 3.64 / 6.57 / 14.92 ms |
| 5 | 40–60 | 1.29 / 1.63 / 5.73 / 6.04 ms | 1.86 / 3.11 / 6.85 / 8.33 ms |
| 6 | 40–60 | 1.30 / 2.57 / 6.38 / 9.11 ms | 1.48 / 3.06 / 8.06 / 11.02 ms |

Rejected: the median was no better in any pair, and worse in five of six. The tick restarts after
every arrival, so its phase is the last fragment's, and at a 2.5 ms delay its second tick already
lands near the due time. A deadline is rounded up to tokio's millisecond anyway, so whatever the
tick lost comes back in the timer's own rounding and wake-up. Precision below that would need a
timer other than tokio's. The measurement test stays for the next attempt.

### UDP receive buffer: no drops of ours

`netstat -s -p udp` counts 41 007 datagrams "dropped due to full socket buffers" machine-wide
(Tailscale shares the counter). Across a 20 s display stream at 75 fps and 60 Mbit/s, and the
same with 5 % injected loss (`SLOPTY_DROP_PERMILLE=50`, refreshes and retransmissions), the
counter did not move: 0 drops each. A 1080p keyframe (≤ ~300 KB) fits the 768 KiB default many
times over, so `SO_RCVBUF` stays as it is until a larger picture shows drops.

## 2026-09-26 — a key after a pause and the checkpoint

mac-studio, the test profile (optimised, libghostty-vt ReleaseFast), while other sessions built
(load average 90–250). The session actor formats its whole terminal for ptyd (the checkpoint)
500 ms after the last output, on its own thread. A `cat` session holds a full default history
(50 000 coloured lines, as `checkpoint_cost` writes them, at 100 × 30; a 3.65 MB state). Ten
keys are typed, each 500–504 ms after the previous echo arrived, which is when that echo's
quiet-spell checkpoint comes due.

```sh
cargo nextest run -p slopty-worker --test session_actor \
  -E 'test(a_key_after_a_pause_does_not_wait_for_the_checkpoint)' --no-capture
```

| | key → acking frame p50 | p90 | max | checkpoints while typing |
| --- | --- | --- | --- | --- |
| before, run 1 | 13.42 | 17.10 | 19.09 ms | 9 |
| before, run 2 | 15.18 | 16.07 | 17.85 ms | 9 |
| after, run 1 | 0.93 | 5.10 | 12.57 ms | 0 |
| after, run 2 | 0.27 | 2.09 | 16.37 ms | 0 |
| after, run 3 | **0.20** | 0.74 | 3.00 ms | 0 |

Before, each key waited for the formatter: 13–19 ms on a full history, about five times the
2026-09-06 figure for 10k lines. After, the quiet-spell checkpoint waits two seconds past a
viewer's last input as well, so none runs while someone types, and the key waits for nothing.
The after-runs shared the machine with four other actor tests and with other sessions' builds,
and their tails are that. The checkpoint forced every 1 MiB of output now runs after the frame
of the read that made it due, not before it.

Not done: formatting only what changed since the last checkpoint. History rows do not change
once scrolled off, so each run could be sub-millisecond, but that is a change to
`slopty-engine`'s formatter.

## 2026-09-26 — the keystroke path under an all-core spin

mac-studio (10 cores), release daemons on a private port, data dir and sockets, `slopty bench
echo` from this Mac over loopback, 150 bytes into `/bin/cat` a run. Load: every core spun at
`QOS_CLASS_USER_INITIATED` (the class a build's threads ask for) by `tests/load.rs`, on top of
other sessions' builds (load average 110 before the spin, 270–300 by the end). The worker arm was
chosen per run by a temporary `SLOPTY_QOS=off` in `slopty_worker::qos::user_interactive`, since
removed. The bench client (`slopty`, the CLI) is unclassed in both arms. The stages come from
the trace on both sides (as in "the keystroke path, stage by stage"): each `control send` is
matched to the worker's next `term input received`, that to its next `frame sent`, and that to
the client's next `frame received`.

```sh
cargo build --release -p slopty-ptyd -p slopty-workerd -p slopty-cli
cargo test -p slopty-worker --test load --no-run      # the spinner's binary
R=/tmp/slopty-qos; export SLOPTY_NO_SHELL_INTEGRATION=1
SLOPTY_DATA_DIR=$R/ptyd target/release/slopty-ptyd --socket $R/ptyd.sock &
# per run: a fresh worker, the spinner for 18 s, then the bench
RUST_LOG="info,slopty_worker::session=trace,slopty_worker::conn=trace" \
  SLOPTY_DATA_DIR=$R/w SLOPTY_DROP_DIR=$R/drop target/release/slopty-worker \
  --ptyd-socket $R/ptyd.sock --ctl-socket $R/worker.sock --data-dir $R/w \
  --port 45613 --bind 127.0.0.1 --pasteboard slopty-qos-bench > $R/worker.log 2>&1 &
SLOPTY_LOAD_SECS=18 target/debug/deps/load-<hash> --ignored --exact spin::every_core_at_user_initiated &
RUST_LOG="warn,slopty=trace,slopty_cli=trace,slopty_client::link=trace" SLOPTY_DATA_DIR=$R/c \
  target/release/slopty bench echo --worker 127.0.0.1:45613 --count 150 2> $R/bench.log
```

Inside the worker, `term input received` → `frame sent` (the connection's task, the session
thread and back), ms, four interleaved pairs of runs:

| run | unclassed p50 / p90 / p99 / max | user-interactive p50 / p90 / p99 / max |
| --- | --- | --- |
| 1 | 0.64 / 5.06 / 11.1 / 13.1 | **0.14 / 0.19 / 0.67 / 3.6** |
| 2 | 0.53 / 5.49 / 20.4 / 54.3 | 0.15 / 0.36 / 3.78 / 25.3 |
| 3 | 0.44 / 6.43 / 569 / 603 | 0.14 / 0.37 / 9.16 / 32.6 |
| 4 | 0.37 / 5.09 / 212 / 533 | 0.14 / 0.47 / 11.2 / 20.5 |

Client `control send` → worker `term input received` (the key's packet, the worker's noq driver
and the connection's loop): p90 2.7–4.1 ms unclassed, 0.37–0.76 ms classed. Worker → client did
not change, and could not: its receiving half is the unclassed CLI. So the bench's own totals
still carry the client's tail. Without the trace, the same comparison read p50 1.2–1.7 against
0.8–0.9 ms loaded and 1.0–1.3 against 0.6–0.7 ms before the spin. The loaded maxima of 0.7–1.4 s
in both arms are the CLI: in the worst sample the key waited 446 ms in the client before it was
sent, and the echo 827 ms between the client's link and the bench task, while the worker
turned it round in 0.1 ms.

So user-interactive QoS on the session threads and the worker's runtime (every runtime thread,
through `on_thread_start`, and the main thread) takes the worker's share of a loaded echo back to
what it is on a quiet machine. The client needs the same, which this run shows from the other
side.

The client side, the same day: one release worker (classed) and ptyd from one build, and two
builds of the CLI, `before` unclassed and `after` with `slopty_platform::user_interactive_thread`
on its main thread, every runtime thread (`on_thread_start`) and its stdin thread. Four
interleaved pairs, each under a fresh 25 s all-core `USER_INITIATED` spin at load average
100–150, 150 keys a run through `slopty bench echo`. Per pair, a throwaway `run.sh` started a
fresh worker on port 45631, the spin, then the bench from each build:

```sh
# before/ and after/ hold each build's `slopty`; before/ also the worker and ptyd
cargo test -p slopty-worker --test load --no-run
SLOPTY_LOAD_SECS=25 target/debug/deps/load-<hash> --ignored --exact spin::every_core_at_user_initiated &
RUST_LOG="warn,slopty_cli=trace,slopty_client::link=trace" SLOPTY_DATA_DIR=$R/c \
  $R/$arm/slopty bench echo --worker 127.0.0.1:45631 --count 150
```

| pair | unclassed p50 / p90 / max | user-interactive p50 / p90 / max |
| --- | --- | --- |
| 1 | 0.9 / 170.8 / 1 497 ms | 0.6 / 21.3 / 44.8 ms |
| 2 | 9.6 / 186.3 / 1 098 ms | 1.0 / 21.4 / 38.2 ms |
| 3 | 0.5 / 165.7 / 1 130 ms | 0.7 / 20.1 / 32.3 ms |
| 4 | 0.5 / 270.1 / 1 110 ms | 1.2 / 23.4 / 57.6 ms |

The whole loaded echo's p90 falls eightfold and the second-long maxima go. What is left, about
20 ms at p90, is the spin sharing the P-cores with every user-interactive thread on the machine.
The app's and the iOS app's runtimes, and the worker's input threads, take the same class.

## 2026-09-26 — a dense screen's frame: UTF-8 checks, background runs and the font per run

`terminal::view::tests::dense_screen_cost` (headless GPUI, release, mac-studio) draws a 200 × 60
screen of dense coloured text: every word in one of the ANSI colours or a true colour, a third of
them bold, a fifth on a background. Each sample is a frame applied and drawn, 600 after 60 of
warm-up, four ways: the screen redrawn unchanged, scrolled by one line a frame, replaced whole
every frame (1 800 new words a frame, past the word cache's budget), and a second screen with
every cell on a background (a full-screen program's theme) redrawn unchanged. Headless GPUI
shapes with a no-op text system and rasterises nothing, so these are the element's and GPUI's
per-frame work, not CoreText's.

A `sample` of the unchanged screen put 37 % of the draw in the element's prepaint, and 14 % of
it in `core::str::from_utf8`: `CellText::as_str` validates the cell's bytes on every call, and
the prepaint called it three times a cell a frame (the blank test that splits words, the sprite
test and the word's hash key). A full screen of new text spent a further fifth building a
`Font` per style run (copying the family, allocating features and fallbacks). Changes, each
measured against the build before it:

1. Blank and sprite tests read the cell's bytes (a sprite is three or four bytes of UTF-8, so
   ASCII never reaches `as_str`), and a word's key hashes an ASCII cell by its byte and any
   other by `CellText`'s own content hash.
2. The four faces (regular, bold, italic, both) are built once a frame and cloned into runs.
3. A run of cells on one background resolves and converts its colour once, not per cell.

p50 in µs; each cell is the range over interleaved runs of the builds side by side (four runs
for 1 and 2, at load average 15 to 30; five for 3, at 50 to 100). The tails moved with the
load on the machine, not with the build.

| scenario | before | after 1 | after 1 + 2 | after 1 + 2 + 3 |
| --- | --- | --- | --- | --- |
| unchanged | 585–604 | 458–475 | 456–472 | 473–491 |
| a line a frame | 635–655 | 500–527 | 493–518 | 519–548 |
| a screen a frame | 2 194–2 264 | 2 056–2 152 | 1 776–1 834 | 1 812–1 917 |
| every cell on a background, unchanged | | | 724–778 | 670–729 |

The last column ran later and under more load than the others; its own comparison is the row it
changes, against the column before in the same interleaved runs. Together: an unchanged dense
screen and a flood cost about a fifth less a frame, a screen of new text about a fifth less, and
a screen painted edge to edge another 7 %. What is left of the unchanged frame is about half
GPUI's per glyph (`TextSystem::raster_bounds`, a lock and a map lookup per glyph, and the
bounds tree), and half the element's walk over the cells and words.

```sh
cargo test -p slopty-ui --release --lib dense_screen_cost -- --ignored --nocapture
# attribution: run the binary it builds by hand and `sample <pid> 6 1` while it draws
```

Logs: `/tmp/dense-ab2.log` (before, 1, 1 + 2), `/tmp/dense-ab3.log` (3 against 1 + 2),
`/tmp/sample-dense-u.txt`, `/tmp/sample-dense-r.txt`.

## 2026-09-26 — a state too large to keep

mac-studio, the test profile (optimised, libghostty-vt ReleaseFast), load average 8–15. ptyd
takes a checkpoint of at most `MAX_CHECKPOINT_BYTES` (12 578 816 B). The formatted state of a full
50 000-line history, `slopty-engine`'s `checkpoint` at 50 rows (a throwaway probe, not kept):

| rows | state | format |
| --- | --- | --- |
| 80 columns of digits | 4.06 MB | 10.6 ms |
| 200 columns of digits | 10.06 MB | 23.1 ms |
| 250 columns of digits | 12.56 MB, just under | 28.1 ms |
| 120 columns, a 256-colour run every 20 | 10.60 MB | 44.3 ms |
| 200 columns, a 256-colour run every 20 | **17.52 MB, too large** | 71.5 ms |

So plain text at 200 columns fits and full coloured rows at 200 columns do not. A state that did
not fit was formatted and dropped, and `tap_lost` was never cleared. A session whose tap had
missed the queue (ptyd behind while the history filled) therefore owed a checkpoint on every
read, and every read formatted the whole state on the actor's thread after its frame. The test
fills that history at 200 × 50 with the tap queue unread, so a tap is lost, then types 20 keys
30 ms apart. After each acking frame it asks the actor for a snapshot, and times how long the
actor stays busy after the frame:

```sh
cargo nextest run -p slopty-worker --test session_actor \
  -E 'test(a_state_too_large_to_keep_is_not_formatted_for_every_read)' --no-capture
```

| run | load | busy after the frame p50 / p90 / max | output taps after the fill |
| --- | --- | --- | --- |
| before 1 | 9 | 77.75 / 82.49 / 84.66 ms | 0 |
| after 1 | 8 | **0.05** / 0.20 / 4.85 ms | 20 |
| before 2 | 9 | 70.37 / 72.78 / 74.84 ms | 0 |
| after 2 | 10 | **0.04** / 0.05 / 0.07 ms | 20 |
| before 3 | 15 | 70.46 / 70.84 / 71.55 ms | 0 |
| after 3 | 8 | **0.11** / 0.40 / 12.83 ms | 20 |

Before, each read cost a 70–78 ms format, and a key typed inside it waited for all of it. The
taps had stopped for good too, so ptyd's ring stayed as it was when the tap was lost. After, a
state found too large clears `tap_lost`, and no checkpoint is forced (neither by a lost tap nor
every 1 MiB) until one fits. The taps go on, so the ring keeps the newest output, as it does
after any overflow. Each quiet spell still tries once, which costs what a fitting session's
checkpoint costs, only larger (71 ms here). Formatting only what changed would remove that too,
and it needs `slopty-engine`'s formatter.

## 2026-09-26 — an echo beside other busy sessions

mac-studio, release, load average 7–15. Every session stream sits at one priority and noq
round-robins equal priorities a packet each. An echo written while other sessions' frames wait
therefore goes out after one packet from each of them. The worker's pump now raises the typed
session's stream to `ECHO_PRIORITY` before an echo frame and lowers it again at the next frame
that is not one (`slopty_net::streams::EchoLift`).

On a 1 MB/s shaped link, 32 other session streams each hold 64 frames of about a packet, then the
typed stream writes its echo. The number is how many of their bytes reach the client first
(`a_lifted_echo_overtakes_other_sessions_queued_frames`, three runs, identical each time):

| stream | other sessions' bytes ahead of the echo |
| --- | --- |
| default priority | 29 700 (33 frames: a turn for each stream, and one) |
| lifted | **900** (the frame already on its way) |

Six busy sessions beside one echoing session, each writing a frame every 8 ms (the actor's
pace), through `slopty-shape` at 20 Mbit/s with 2 ms each way and a 100 ms queue. There are
200 keys a run, and the arms alternate on fresh connections, three rounds each:

```sh
SLOPTY_FLOOD_BYTES=4000 cargo nextest run -p slopty-net --release --test echo_beside_sessions \
  --run-ignored only --no-capture
```

| floods | arm | echo p50 per round | all: p50 / p90 / p99 / max |
| --- | --- | --- | --- |
| 3 000 B (90 % of the link), run 1 | default | 7.04, 7.61, 7.01 ms | 7.15 / 10.60 / 15.77 / 59.31 ms |
| | lifted | 5.56, 5.69, 7.57 ms | 5.78 / 9.23 / 23.40 / 63.05 ms |
| 3 000 B, run 2 | default | 5.61, 5.59, 6.86 ms | 5.67 / 11.63 / 26.05 / 99.43 ms |
| | lifted | 5.62, 5.89, 8.46 ms | 6.07 / 10.76 / 38.52 / 81.58 ms |
| 4 000 B (120 %), run 1 | default | 17.45, 10.87, 11.73 ms | 12.16 / 21.61 / 73.31 / 232.94 ms |
| | lifted | 7.75, 8.20, 7.70 ms | **7.81** / 9.89 / 11.12 / 11.51 ms |
| 4 000 B, run 2 | default | 10.94, 18.18, 10.24 ms | 11.95 / 20.59 / 30.07 / 41.02 ms |
| | lifted | 7.66, 13.00, 7.59 ms | **8.33** / 16.66 / 24.62 / 54.50 ms |

With the floods under the link, the queues rarely hold several sessions at once, and the two arms
are within noise of each other. With them over it, every session always has data waiting, and
the lifted echo was 2.7–9.7 ms sooner at the median in all six pairs. That is the case the change
is for: typing into one terminal while others stream to a slow link.

## 2026-09-26 — the prediction threshold over a shaped link

Scenario (d) of the smooth suite through `slopty-shape`'s relay (`Stack::launch_shaped`): the
app and a debug worker on this Mac, the relay adding half the round trip each way, 60 keys typed
into zsh a run, with `SLOPTY_PREDICT` never, always and adaptive. Echo and predicted are key to
glass, p50 / p90 ms; "link" is the round trip the connection measured and the predictor was
told. Load average 9–19.

```sh
D=$PWD/target/e2e-predict SLOPTY_DATA_DIR=$D SLOPTY_PTYD_SOCKET=$D/run/ptyd.sock \
  SLOPTY_WORKER_SOCKET=$D/run/worker.sock SLOPTY_E2E_BIN_DIR=$PWD/target/debug \
  SLOPTY_E2E_ARTIFACTS=$D/artifacts SLOPTY_SMOOTH_E2E=1 \
  RUST_LOG=info,slopty_ui::terminal::view=debug \
  cargo nextest run -p slopty-e2e --test smooth --no-capture -E 'test(typing_over_a_shaped)'
```

First, with the ruling of the day before, no key was predicted even with `always`: every zsh
prompt carried `ECHO_OFF` (decisions/terminal.md, "A line editor's prompt is guessed at").
With that fixed, the old 25 ms threshold against half a 60 Hz refresh, adaptive:

| shaped rtt | link | never: echo | 25 ms: guesses | ½ refresh: echo | ½ refresh: guess | keys guessed |
| --- | --- | --- | --- | --- | --- | --- |
| 5 ms | 6.4–14.6 ms | 31.9 / 73.9 | none | 37.5 / 53.8 | 25.8 / 30.4 | 41 of 60 |
| 10 ms | 11.0–11.6 ms | 34.8 / 56.2 | none | 34.7 / 44.0 | **20.6 / 28.5** | 55 of 60 |
| 15 ms | 16.6–16.9 ms | 38.3 / 44.8 | none | 36.7 / 44.7 | **21.1 / 28.7** | 54 of 60 |
| 20 ms | 22.4 ms | 42.6 / 52.4 | none | | | |

No run had a miss or a late guess. The five or six keys not guessed at 10 and 15 ms are the
warm-up hits. With `always`, the guess reached the glass 21.5–23.6 ms p50 at every round
trip; the floor is the compositor's 19–21 ms. At 5 ms the loaded runs measured a link of
14.6 ms and so guessed; on a quiet 5 ms link half a 60 Hz refresh (8.3 ms) is not reached.

The relay itself held its delays late at first: on this Mac a tokio timer fires 25–50 % past
its interval (timer coalescing by the process's latency tier, plus tokio's millisecond), so a
configured 10 ms round trip read key → arrived 13.9 / 24.0 ms. Its `Timer` now asks for the
wait less the lateness it has seen and yields through the last stretch: 10.7 / 14.9 ms
(`a_short_wait_ends_when_it_was_asked_to`: a 5 ms wait never early, its median overshoot
under 1 ms; a round trip also carries loopback's own hops, which on 2026-09-26 evening cost 1 ms
clear and 3–4 ms more after each hold, so the test reads the timer, not the trip).

## 2026-09-26 — timers fire late by the thread's latency tier

A throwaway binary slept 300 times each on tokio's timer (a multi-thread runtime, the sleep in
a spawned task) at 1, 2, 5 and 8 ms, run as a launchd agent with `ProcessType Interactive` as
the daemons are, twice per arm, arms back to back, load average 9–40. Lateness past the
interval asked for, p50 / p90 ms:

| runtime threads | 1 ms | 2 ms | 5 ms | 8 ms |
| --- | --- | --- | --- | --- |
| unclassed | 2.02 / 2.18 | 2.30 / 2.60 | 2.99 / 4.03 | 4.07 / 5.48 |
| `QOS_CLASS_USER_INTERACTIVE` | 1.88 / 2.08 | 2.22 / 2.56 | 2.74 / 4.00 | 3.86 / 5.40 |
| `THREAD_LATENCY_QOS_POLICY` tier 0 | 1.53 / 1.66 | 1.78 / 1.88 | 2.16 / 2.59 | 2.49 / 3.05 |

Run from a shell instead, a `std::thread::sleep` of 5 ms came 1.26 ms late either way, and 0.65
ms at tier 0. `kern.timer_coalesce_tier{0,1,2}_scale` read 3, 2 and 1 here: the kernel may fire a
timer late by an eighth, a quarter or a half of its interval, by the thread's tier, up to 1 ms
at tier 0. What is left at tier 0 is tokio's rounding to its millisecond wheel.

Tier 0 is not taken. `thread_policy_set` with the latency policy takes the thread out of the
QoS system: its `pthread_get_qos_class_np` then reads `QOS_CLASS_UNSPECIFIED`, in either order
of the two calls, which undoes the user-interactive class that took a loaded echo's p90 from
166–270 ms to 20–23 ms ("the keystroke path under an all-core spin"). The class is worth
tenfold more than the 1–1.6 ms a tier-0 timer saves. What the lateness costs, and where:
the worker's 8 ms output pace (echoes are exempt), noq's 2 ms ACK delay and its pacer, and the
2 ms echo copies. A timer that must fire on time asks for less and yields through the last
stretch, as `slopty-shape`'s relay now does; none on the keystroke path has needed it.

## 2026-09-27 — loading placeholders after a grace

What a round trip to the worker takes, from the entries above: the control stream's app round
trip is 0.34–0.50 ms on loopback, 0.46–0.63 ms on the LAN and 0.63–1.13 ms over the tailnet IP
(2026-09-24, "plaintext QUIC on noq against iroh"); the shaped tailnet profile the e2e runs
through (`harness::TAILNET`, 4 ms each way plus up to 2 ms of jitter) is 8–12 ms. A small
file's read on the worker is well under a millisecond beside that. A stream's first frame is
another matter: about 250 ms of capture start and encoder warm-up (2026-09-04, "screen stream
end to end").

`screen::LOADING_GRACE` is 32 ms, two 60 Hz frames: the frame the answer lands in, plus a
round trip of up to one frame budget, which covers the shaped profile's worst 12 ms. A read
or an attach that answers in time draws its content into a blank body; a stream always takes
longer, so "Opening … on …" shows after 32 ms, with the header's working mark from the start.

The same change touched the terminal's prepaint (an unfocused cursor's colour) and paint (the
failed wash's width, one product a frame). `dense_screen_cost` after it, release, mac-studio,
load average about 13, one run:

| scenario | p50 / p95 µs |
| --- | --- |
| unchanged | 441 / 481 |
| a line a frame | 483 / 581 |
| a screen a frame | 1 695 / 3 385 |
| every cell on a background, unchanged | 637 / 783 |

Every p50 is at or under the previous entry's range for the same scenario, so nothing was
added per frame that the bench can see.

```sh
cargo test -p slopty-ui --release --lib dense_screen_cost -- --ignored --nocapture
```

Log: `target/logs/b3-dense.log`.

## 2026-09-27 — an echo's frame beside the chrome

The navigator, the title bar and the status bar became cached views of their own
(`docs/decisions/ui.md`, "The chrome is three cached views"). One worker with 60 shells (a
directory, a branch and a start each) and 60 notes, the navigator docked; 400 frames after
20 of warm-up, each either a notify of the last shell's terminal alone (an echo) or of the
workspace itself. One test build (the `dev` profile) per arm, Mac Studio, load average 9–13,
other sessions building throughout. The baseline is the tree just before the change.

| run | echo p50 | echo p95 | workspace p50 | workspace p95 |
| --- | --- | --- | --- | --- |
| before 1 | 2.19 ms | 2.97 ms | 2.26 ms | 2.98 ms |
| before 2 | 2.41 ms | 2.83 ms | 2.29 ms | 2.69 ms |
| before 3 | 2.27 ms | 2.76 ms | 2.23 ms | 2.63 ms |
| after 1 | 0.44 ms | 0.51 ms | 1.82 ms | 2.21 ms |
| after 2 | 0.44 ms | 0.49 ms | 1.86 ms | 2.22 ms |
| after 3 | 0.45 ms | 0.55 ms | 1.86 ms | 2.17 ms |

An echo's frame no longer draws the chrome: it costs a fifth of what it did, about 1.8 ms
less at p50 in a debug build. The workspace's own frame still draws all three; other wave-2
edits landed in the tree between the runs, so its change is not this one's number.

```sh
cargo test -p slopty-ui --lib measure_an_echo_frame_beside_the_chrome -- --ignored --nocapture
```

## 2026-09-27 — uploading many small files

An upload waited for the worker to acknowledge each file's stream before opening the next, so
a tree of small files paid a round trip per file. 100 files of 4 KiB through the shaping relay
at a 20 ms round trip (no rate limit, no loss), timed at the worker from `Begin` to the last
byte of the last file:

| client | time | per file |
|---|---|---|
| a round trip per file (before) | 2.154 s, 2.166 s | 21.6 ms |
| up to 16 finished streams awaiting acknowledgement at once (after) | 150 ms, 156 ms | 1.5 ms |

The files' bytes now follow each other; a stream the worker stops or loses is sent again from
what it holds, as before. The walk of a dropped tree also moved off the async runtime
(`spawn_blocking`), which it held for as long as a large directory took to stat.

```sh
cargo nextest run -p slopty-client -E 'test(many_small_files)' --no-capture
```

## 2026-09-27 — syncing a landed file

What each landed file cost in syncs, 100 files of 4 KiB written, synced and renamed on this
Mac's internal SSD (`/tmp`, release build of a scratch program), per file:

| after the write | ms per file |
|---|---|
| nothing | 0.15–0.17 |
| `sync_all` (`F_FULLFSYNC` on Apple platforms) | 3.77–4.11 |
| `sync_all` and a directory `sync_all` (before, both sides) | 7.59–7.70 |
| plain `fsync` (after) | 0.20 |

A download also ran `sync_data` every 8 MiB, which a resume never needed: what a resume claims
to hold is synced when it is asked for. A landed file, whose digest has matched, now gets a plain
`fsync` before its rename and no directory sync, on the client and the worker. Ten thousand
small files went from about 76 s of syncing to 2 s. A cut stream's bytes are still fully synced
for its resume.

## 2026-09-27 — one bulk stream over a long round trip

One 16 MiB bulk stream, client to worker, through `slopty-shape` adding a one-way delay and
nothing else (no rate limit, no loss), timed at the receiver from the stream's opening to its
last byte. Default controller (bounded BBR3), `INITIAL_RTT` 5 ms, test build, Mac Studio with
other sessions building (load average 15 to 20). Two runs a cell, Mbit/s:

| one way (round trip) | before | after |
| --- | --- | --- |
| 0 ms | 476, 440 | 597, 524 |
| 5 ms (10 ms) | 425, 313 | 429, 475 |
| 10 ms (20 ms) | 147, 113 | 310, 314 |
| 15 ms (30 ms) | 93, 96 | 213, 213 |
| 20 ms (40 ms) | 10, 12 | 160, 161 |
| 30 ms (60 ms) | 10, 10 | 111, 107 |

Before, at 60 ms the sender ended with no loss, a 2.3 MB window and 10 Mbit/s. BBR3's own state
showed why: `min_rtt` 5 ms against real samples of 60 ms, from the first ACK on. noq calls the
controller's `on_ack` before it feeds that ACK's round trip to the `RttEstimator`, and BBR3 took
its first rate sample's RTT from the estimator, which still held the initial RTT. No later sample
was lower, so the 5 ms minimum stood for the 10 s filter, and the window
(`2 × bw × min_rtt + extra_acked`) was a twelfth of the path's product. The flow reached its rate
only when the minimum expired, which is why a 16 MiB transfer took 13 s. At 10 and 15 ms each way
extra_acked covered part of the shortfall. The vendored noq-proto now takes the packet's own round
trip, as every later sample did (`vendor/noq-proto/SLOPTY.md`, patch 6).

After, the fastest delivery rate each run measured sits on `1.25 MB / RTT`: 60.6 MB/s at 20 ms,
41.7 at 30, 30.6 at 40 and 20.2 at 60. That is noq's default stream receive window (sized for
100 Mbit/s at 100 ms), the next ceiling for one stream. An 8 MiB window, one run each, reached
191 Mbit/s at 60 ms and 186 at 20 ms, against 310 at 20 ms with the default, so it is not taken
here. `SLOPTY_CC=cubic` had reached 102 to 115 Mbit/s at 60 ms before the fix, the same window
limit. The file upload the bug was found in went from 10 to 114 Mbit/s at 60 ms; `slopty-client`'s
`a_large_file_fills_a_long_round_trip` now holds 16 MiB there under 3 s.

`echo_beside_a_video_flood`, release, one run after the fix: every arm inside the ranges of
"noq's BBR3 against the draft" (bursts: echo p99 16.0 ms, queue p99 8.0 / max 11.6 ms, window
p50 32 kB; halved: echo p99 17.6 ms, queue max 14.4 ms, nothing lost). Its link has a 4 ms round
trip, below the 5 ms initial RTT, so the first real sample always replaced the stale one there.

```sh
UP_MS=<one-way ms> cargo nextest run -p slopty-net --test bulk_over_delay --run-ignored only --no-capture
cargo nextest run -p slopty-net --test bulk_over_delay    # the 80 Mbit/s floor at 0, 5, 10 and 30 ms
cd vendor/noq-proto && CARGO_TARGET_DIR=../../target/noq-vendor cargo test --lib \
  the_first_ack_measures_min_rtt_from_its_own_packet && rm Cargo.lock
cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only --no-capture
```

"Before" is the patched tree with the one line put back (`rtt: rtt.get()`).

## 2026-09-27 — describing a terminal's foreground process

The agent watcher probes every session every 750 ms, and the probe runs on the session's actor,
the thread that also carries its output and echo: `tcgetpgrp` on the master, then
`proc_pidinfo` (`PROC_PIDTBSDINFO` and `PROC_PIDVNODEPATHINFO`) and the `KERN_PROCARGS2` sysctl
for the group leader. An audit asked whether that should move off the actor. Describing the test
process 2,000 times, test build, Mac Studio under other sessions' builds: p50 7.6 µs, p99 50.5 µs,
max 112 µs. One echo waits at most that long, once per 750 ms, so the probe stays where it is.

```sh
cargo nextest run -p slopty-pty describing_a_process_costs --run-ignored only --no-capture
```

## 2026-09-27 — the stream window against echo

A quiet machine (load under 10, no builds), release. One 16 MiB bulk stream through
`slopty-shape` (delay only), Mbit/s, three runs a cell, all within 2%:

| one way | 1.25 MB (noq default) | 4 MB | 8 MB | 16 MB |
| --- | --- | --- | --- | --- |
| 5 ms | 480–490 | 475–487 | 481–485 | 475–484 |
| 15 ms | 212–213 | 304–307 | 303–312 | 303–306 |
| 30 ms | 105–107 | 181–182 | 191–192 | 191–192 |

`echo_beside_session_floods` (six sessions flooding, one echoing, 20 Mbit/s, 2 ms each way, lifted
stream), three alternating passes per window, echo in ms:

| window | p90 | p99 | max |
| --- | --- | --- | --- |
| 1.25 MB | 5.81, 6.96, 5.82 | 8.98, 9.23, 8.11 | 10.7, 9.8, 9.4 |
| 4 MB | 7.17, 8.97, 6.65 | 8.85, 12.22, 8.98 | 15.7, 77.6, 16.1 |

The window stays at noq's default (`docs/decisions/transport.md`).

```sh
UP_MS=30 SLOPTY_STREAM_WINDOW=4194304 cargo nextest run -p slopty-net --release \
  --test bulk_over_delay --run-ignored only --no-capture
SLOPTY_STREAM_WINDOW=4194304 cargo nextest run -p slopty-net --release \
  --test echo_beside_sessions --run-ignored only --no-capture
```


## 2026-09-27 — counting a working tree's changes

What one `slopty_worker::changes::count` costs, `git diff --numstat HEAD` and `git ls-files
--others --exclude-standard` run side by side, release, five counts after a warm-up, the
repositories on the external volume, load 7–11:

| repository | tracked files | changes found | fastest | slowest |
| --- | --- | --- | --- | --- |
| slopty | 616 | 37 files, +289 −38 | 32 ms | 40 ms |
| zed | 4351 | clean | 34 ms | 36 ms |
| ghostty | 5890 | clean (large ignored build caches) | 168 ms | 172 ms |

A count runs on a Tokio task, never on a session actor, at most one per repository at a time and
no sooner than 2 s after the last began (`changes::MIN_INTERVAL`), so the worst of these costs a
background core a twelfth of the time at the fastest cadence.

```sh
SLOPTY_COUNT_REPO=/path/to/repo cargo test -p slopty-worker --release --lib \
  counting_a_repository_costs -- --ignored --nocapture
```

## 2026-09-27 — an echo beside a followed conversation

What following a busy agent's conversation costs a key's echo on the same connection
(`echo_beside_a_followed_conversation`, apps/slopty-worker e2e, release, loopback, mac-studio,
load 6–7, working tree on 0439797 plus phase 1b). One client connection types into `/bin/cat`,
200 keys an arm, each timed to the frame that shows it; three arms alternate for five rounds:
nothing else going on; an agent session whose transcript grows by a 2 KB answer every 5 ms
(400 KB/s, a hook every 50 ms) that nobody follows; and the same session followed on the same
connection, its conversation stream drained. That is about twenty times the rate of a real turn.
Each follow re-sends the backlog first (730 to 3 800 changes by the fifth round).

| run | arm | p50 / p90 / p99 / max, 1 000 keys |
| --- | --- | --- |
| 1 | quiet | 0.90 / 2.38 / 7.43 / 14.23 ms |
| | busy, not followed | 1.00 / 2.74 / 12.74 / 34.94 ms |
| | busy, followed | 0.94 / 3.98 / 12.11 / 41.80 ms |
| 2 | quiet | 0.41 / 1.93 / 4.54 / 10.64 ms |
| | busy, not followed | 0.54 / 2.14 / 6.08 / 12.78 ms |
| | busy, followed | 0.57 / 3.33 / 11.14 / 29.29 ms |

The median does not move. At that rate the followed arm's p90 is 1.2–1.6 ms higher than the
unfollowed one, and did not grow with the backlog from round to round (per-round p90s 2.1–4.7 ms
against 2.1–3.0), so it is the steady decode and send, not the snapshot. An answer appended to
the transcript reached the follower in p50 26 ms / p90 57–82 ms / p99 420–500 ms (live changes
only, after `Current`): the 250 ms tick and the hooks between ticks.

```sh
cargo build --release -p slopty-ptyd -p slopty-cli
cargo test -p slopty-workerd --release --test e2e echo_beside_a_followed_conversation \
  -- --ignored --nocapture
```

The conversation stream is gone (2026-10-04): a session is followed as its thread now. The
same measurement follows the session's thread on a thread stream
(`echo_beside_a_followed_thread`, the lag read from the answers' items as they start); its
first run is the 2026-10-04 entry "an echo beside a followed thread".

```sh
cargo test -p slopty-workerd --release --test e2e echo_beside_a_followed_thread \
  -- --ignored --nocapture
```

## 2026-09-27 — typing over a shaped link: guesses that look final, and the round trip at link-up

Scenario (d) of the smooth suite through `slopty-shape`'s relay (`typing_over_a_shaped_round_trip_on_the_mac`),
the same rig as "the prediction threshold over a shaped link": the e2e app build (debug), 60 keys
at 15/s into zsh with an empty `ZDOTDIR`, `SLOPTY_PREDICT` never, always and adaptive, a 75 Hz
display. Key → glass, p50 / p95 ms; "link" is the round trip the connection measured.

The earlier note that this test drew no guesses and echoed in 38 ms at 10 ms no longer holds on
this tree. A diagnostic run at load 60–170 drew 58–59 of 60 guesses with `always` at every round
trip, with no misses and no late guesses. At 10 ms the unguessed echo was 32.1 ms: 10.8 ms from the
key to the echo arriving, 0.8 ms in the app (applied → painted → submitted), and 20.4 ms from
submission to the glass. That last hop is the compositor's floor (about 1.4 refreshes, "keystroke to
glass, hop by hop"). So the echo is the round trip plus that floor, and no hop of Slopty's own is
left to cut. What was left:

1. **Adaptive drew 55 of 60 keys, `always` 59.** One key is the warm-up. The other three came
   before the predictor knew any round trip: the app first read the link's RTT 500 ms after the link
   came up (`HEARING_TICK`), so `visible` said no. The app now passes the handshake's RTT to the
   predictors at link-up.
2. **Every guess was faint and underlined**, so a key looked final only when its echo came, a
   refresh or more after the guess, and every key changed its look once on the way. A guess now
   looks like the text it continues. It is marked only on a link of 80 ms or more, while it waits
   past 250 ms, or after a slow echo or a miss (decisions/terminal.md, "A guess looks like the text
   it continues"). "Key → final look" below is when the key first shows as it will stay: the echo
   while guesses were marked, and the guess once they are not. Nothing here marks a guess: every
   link is under 50 ms, the slowest guess waited 39 ms, and no run had a miss.

| shaped rtt | link | never: echo | adaptive: guesses drawn, before → after | adaptive: guess | key → final look, before → after |
| --- | --- | --- | --- | --- | --- |
| 5 ms | 6.0 ms | 27.8 / 35.6 | 31, then 11: the link sits on half a refresh (6.7 ms) either way | 22.9 / 28.3 | — (mostly unguessed) |
| 10 ms | 11.0 ms | 32.0 / 38.8 | 55 → **58** | 20.8 / 26.3 | 35.3 → **20.8** |
| 15 ms | 16.3 ms | 37.5 / 43.2 | 55 → **58** | 20.9 / 27.8 | 37.1 → **20.9** |
| 20 ms | 21.1 ms | 42.3 / 48.3 | 55 → **58** | 20.6 / 28.2 | 42.3 → **20.6** |

The "before" counts come from the diagnostic run (load 60–170). Everything else comes from one run
at load 7–9. Both final-look columns are from that run: the adaptive echo (35.3, 37.1, 42.3) is when a
marked guess's key looked final, and the adaptive guess is when an unmarked one does. A
second run after the change, with the load climbing from 10 to 44 during it, also drew 58 of 60 at
10, 15 and 20 ms. Its relay's round trips read 12–19 ms at the 5 and 10 ms settings. On loopback
(`typing_is_timed_with_and_without_the_local_echo_on_the_mac`, load 10) the echo was 23.2 / 29.2
and `always` drew 59 guesses at 22.2 / 28.5, in line with "keystroke to glass, hop by hop".

The shaped test now also fails when the adaptive policy draws fewer than half the keys, on a
measured link past half a 60 Hz refresh.

```sh
cargo build -p slopty-ptyd -p slopty-workerd -p slopty-serverd -p slopty-cli -p slopty -p slopty-e2e --bins --features slopty/e2e
D=$PWD/target/e2e-typing; mkdir -p $D/run $D/artifacts
SLOPTY_DATA_DIR=$D SLOPTY_PTYD_SOCKET=$D/run/ptyd.sock SLOPTY_WORKER_SOCKET=$D/run/worker.sock \
  SLOPTY_E2E_BIN_DIR=$PWD/target/debug SLOPTY_E2E_ARTIFACTS=$D/artifacts SLOPTY_SMOOTH_E2E=1 \
  RUST_LOG=info,slopty_ui::terminal::view=debug \
  cargo nextest run -p slopty-e2e --test smooth --no-capture --test-threads 1 \
  -E 'test(typing_over_a_shaped) | test(typing_is_timed)'
```

Logs: `target/logs/typing-shaped-diag.log` (before), `target/logs/typing-shaped-after1.log` and
`target/logs/typing-shaped-after2.log`.

## 2026-09-27 — the conversation face under a streaming answer

How long the face takes to draw a frame while the model writes (`the_face_draws_a_streaming_answer_within_a_frame`, slopty-e2e app, debug build, mac-studio, 1000 × 720 window).
- The session is 80 made-up turns. Each has a prompt, a Markdown answer with a list and a fenced block, a 30-line command output, an edit with its diff and a read, and every assistant record carries a model and usage.
- On top of that, the mod streams a text block one word every 16 ms for 5 s.
- (h) follows the tail. (i) pans ±40 points twice per word.
- Before is HEAD 294ed05 with 0 cargo or rustc processes running. After is the redesigned face: turn figures, changed files, copy and time, the floating composer. The load average was about 11 for the two quiet runs and 110 to 190 for the loaded one.

| face | scenario | draw p50 / p95 / p99 / max | frames, over 16.7 ms |
| --- | --- | --- | --- |
| before | (h) following | 2.6 / 2.9 / 3.2 / 3.5 ms | 288, 0 |
| | (i) panning | 2.5 / 4.6 / 6.1 / 6.8 ms | 272, 0 |
| after, run 1 | (h) following | 2.5 / 2.9 / 3.1 / 3.8 ms | 338, 0 |
| | (i) panning | 2.4 / 4.6 / 5.6 / 6.3 ms | 249, 0 |
| after, run 2 | (h) following | 2.5 / 2.9 / 3.0 / 3.3 ms | 332, 0 |
| | (i) panning | 2.4 / 4.4 / 5.4 / 6.8 ms | 254, 0 |
| after, loaded | (h) following | 2.4 / 2.8 / 3.9 / 4.7 ms | 329, 0 |
| | (i) panning | 2.5 / 5.0 / 6.7 / 7.2 ms | 239, 0 |

The quiet runs hold p99 where it was, or a little under it, while each row now draws more. The row no longer clones its `Row` or its entry. The code blocks' corner and the fold's hint share the view's theme through an `Arc` / `Rc`, so a frame copies none of it. The list stays virtualized: only the rows in view and the 2048-point overdraw are laid out.

```sh
# the app suite, with the frame probe let in; its lines start with MEASURE
SLOPTY_SMOOTH_E2E=1 cargo xtask e2e app
```

## 2026-09-27 — the conversation face with its work tray, pictures and plan card

The same probe as the entry below (`the_face_draws_a_streaming_answer_within_a_frame`: 80 made-up turns, a word every 16 ms for 5 s, debug build, mac-studio, 1000 × 720), after the face gained the background tray, the task card over the composer, thumbnails, the thinking line, the plan card and the top fade.
- Each frame now also works out the tray and the task card and reads the list's scroll top for the fade.
- A row in view checks whether it still has pictures to fetch. The check is a lookup that allocates nothing unless one is missing, and a fold's settle is looked up only while a fold is settling.
- The machine was shared with other builds throughout: load average 27 to 68, no quiet run was possible. Run 1 is inside the app suite; runs 2 and 3 are the probe alone.

| face | scenario | draw p50 / p95 / p99 / max | frames, over 16.7 ms |
| --- | --- | --- | --- |
| run 1 (suite, load ~35–68) | (h) following | 2.4 / 2.8 / 3.0 / 4.1 ms | 344, 0 |
| | (i) panning | 2.4 / 4.7 / 5.8 / 6.2 ms | 271, 0 |
| run 2 (alone, load ~34) | (h) following | 2.5 / 2.9 / 3.1 / 3.2 ms | 338, 0 |
| | (i) panning | 2.6 / 4.9 / 5.5 / 11.2 ms | 268, 0 |
| run 3 (alone, load ~27) | (h) following | 2.5 / 3.0 / 3.1 / 3.2 ms | 338, 0 |
| | (i) panning | 2.5 / 5.1 / 6.5 / 6.9 ms | 265, 0 |

Following holds at 3.0–3.1 ms p99, the quiet figure of the entry below. Panning reads 5.5–6.5 ms p99, between that entry's quiet 5.4 and its loaded 6.7, as its loaded run did. No frame went over 16.7 ms. The session has no background work or pictures, so these numbers are the added per-frame cost of the new surfaces when there is nothing to show. A quiet re-run is still owed.

```sh
# the app suite, with the frame probe let in; its lines start with MEASURE
SLOPTY_SMOOTH_E2E=1 cargo xtask e2e app
```

## 2026-09-27 — a prompt set back, and blocks edge to edge

The terminal paint now sets a prompt back (its default-coloured cells before the typed command
take the faint attribute, on a copy of the row made only when one of them changes) and draws
the block rule and the failed wash across the whole element rather than to the grid's last
column. Both benches below are headless GPUI in release on mac-studio, so the numbers are the
element's and the layout's own work, not CoreText's. "Before" is the test binary built from
the tree as it stood (`target/bench-tiles/before`), "after" the same tree with this change
(`target/bench-tiles/after`), run alternately while other sessions built on the machine (load
average 15 to 19).

`failed_blocks_cost`: 100 × 40, ten four-row blocks, every prompt row `$ run N` with its
command at column 2, so every frame copies and flags ten prompt rows. p50 in µs, four runs
each:

| build | pointer away | pointer over a failed block |
| --- | --- | --- |
| before | 103.3–103.5 | 117.4–117.6 |
| after | 105.6–106.1 | 119.6–120.3 |

About 2.4 µs a frame for ten prompts on screen, a quarter of a microsecond a prompt: the row
copy and the words keyed with the faint style. The rule and the wash cost nothing more; they
are the same quads, wider. `dense_screen_cost` (200 × 60, no prompt marks, three runs each)
moved within its noise: unchanged screen 453–473 µs before, 466–471 after; a line a frame
491–517 before, 505–518 after. The p95 and p99 moved with the load, not with the build.

```sh
cargo test -p slopty-ui --release --lib --no-run   # then copy the binary it names
target/bench-tiles/before failed_blocks_cost --ignored --nocapture --test-threads 1
target/bench-tiles/after failed_blocks_cost --ignored --nocapture --test-threads 1
```

Logs: `target/logs/tiles-blocks-ab.log`, `target/logs/tiles-dense-ab.log`.

## 2026-09-27 — a block's head on its own surface

The terminal paint now lays a band of `surfaces.panel` under every block's head (its prompt
rows and the `Input` rows continuing its command), edge to edge, with the rule over a prompt as
the band's top edge (`docs/decisions/terminal.md`, "A block's head is a surface"). No row
moves, so scrolling and hit testing are untouched: the cost is in the paint alone. Both benches
are headless GPUI in release on mac-studio, so they measure the element's and the layout's own
work, not CoreText's. "Before" and "after" are test binaries built from one scratch copy of
the tree (`/tmp`, own target dir), which differ only in `terminal/element.rs` and
`terminal/view.rs`. They ran alternately, four runs each, while other sessions built on the
machine (load average 11 to 14).

`failed_blocks_cost`: 100 × 40, ten four-row blocks, so ten one-row heads a frame. p50 / p99
in µs:

| build | pointer away | pointer over a failed block |
| --- | --- | --- |
| before | 99.3–105.2 / 156–161 | 112.9–119.6 / 175–192 |
| after | 100.7–107.0 / 139–159 | 113.6–118.9 / 174–187 |

That comes to about 1 µs a frame for ten heads: one pass over the view's marks and one quad a
band. The p99 moved with the load, not with the build. `dense_screen_cost` (200 × 60, no
prompt marks), p50 in µs: unchanged screen 437.7–456.6 before, 438.3–469.5 after; a line a
frame 472.1–516.2 before, 473.8–509.7 after; a screen a frame 1 566–1 832 before, 1 596–1 834
after. All three are the same within the load. A first version computed the heads inside the
per-row loop. In six alternating runs it moved the dense screen's screen-a-frame case from
1 554–1 678 µs to 1 681–1 806 µs, with nothing to band. That is 1–2 µs a row for a comparison,
which points to the loop's code generation rather than the work, so the heads are now a pass of
their own.

```sh
# in a scratch copy of the tree, before and after the change
CARGO_TARGET_DIR=/tmp/slopty-band-target cargo test -p slopty-ui --release --lib --no-run
target/bench-band/before failed_blocks_cost --ignored --nocapture --test-threads 1
target/bench-band/after failed_blocks_cost --ignored --nocapture --test-threads 1
target/bench-band/before dense_screen_cost --ignored --nocapture --test-threads 1
target/bench-band/after dense_screen_cost --ignored --nocapture --test-threads 1
```

Logs: `target/logs/band-blocks-ab.log`, `target/logs/band-dense-ab.log`.

## 2026-09-27 — an echo behind paced video

mac-studio, release, load average 11–18. noq's pacer holds a packet until the pacing rate has
earned its bytes and sets a timer for the shortfall. That timer is tokio's (a millisecond wheel,
coalesced by macOS on top: "timers fire late by the thread's latency tier"), so a packet the rate
earns in 0.5 ms waited 1–2 ms. An echo written while a frame's datagrams are paced out goes in
the next packet (streams before datagrams), and waited for that timer. The measurement is one
connection over loopback with a controller that paces at a fixed rate and never binds its
window, so only the pacer decides when a packet leaves. Video goes as the encoder hands it
over: one burst a frame at 60 fps, 25 kB P-frames and a 130 kB keyframe a second. A 240-byte
echo goes on a stream at `ECHO_PRIORITY` at a random phase of each frame, 400 a run. The
number is each echo's one-way time, from write to read. "Paced" is noq's pacer as it was.
"Unpaced" is `stream_priority_unpaced(Some(ECHO_PRIORITY))` (`vendor/noq-proto/SLOPTY.md`,
patch 8): a datagram starts without the pacing check while a stream at that priority has data
pending.

```sh
cargo nextest run -p slopty-net --release --test pacer_wait --run-ignored only --no-capture
```

| rate | arm | round 1 p50 / p90 / p99 | round 2 | round 3 |
| --- | --- | --- | --- | --- |
| 20 Mbit/s | paced | 0.26 / 1.98 / 6.83 ms | 0.22 / 1.45 / 2.39 | 0.22 / 1.47 / 2.80 |
| 20 Mbit/s | unpaced | 0.16 / **0.24** / 0.32 ms | 0.17 / **0.26** / 0.51 | 0.18 / **0.30** / 0.91 |
| 100 Mbit/s | paced | 0.18 / 0.30 / 2.33 ms | 0.18 / 0.28 / 0.52 | 0.19 / 0.31 / 0.58 |
| 100 Mbit/s | unpaced | 0.18 / 0.29 / 0.41 ms | 0.18 / 0.29 / 0.58 | 0.19 / 0.31 / 0.57 |

At 20 Mbit/s the paced echo's p90 was 1.4–2.0 ms, three times the 0.5 ms one packet takes at the
rate: the timer, not the rate. Unpaced, it is the loopback floor. At 100 Mbit/s a frame's burst
drains in a few pacing intervals and the arms are within noise. An earlier run with the pacer
lending a millisecond of the rate ahead (Chromium's `kAlarmGranularity`) changed nothing, p90
1.42–1.59 ms against 1.44–1.66 ms across three alternated pairs. Video, always queued, spent
the lent credit before any echo arrived, so the echo still waited for the timer. The lending was
removed.

The unpaced packet fills the room the echo leaves with datagrams, and the pacer is charged for it
as for any other; a bucket already empty stays empty, so each echo costs the rate one packet.
The gate test holds the order of magnitude. At 50 kB/s behind a backlog, a packet leaves every
25 ms. Twenty echoes there measured 10.2 ms at the median paced and well under the test's
5 ms unpaced (`an_echo_does_not_wait_for_the_pacer`).

## 2026-09-27 — the shaped flood after the echo left the pacer

One run of `echo_beside_a_video_flood` after noq-proto patch 8 (an echo stream skips the pacing
check) and the client's typed input lifted the same way. Same harness as "echo priority ahead of
datagrams": 20 Mbit/s, 2 ms each way, 100 ms of bottleneck queue, BBR3 with the bound. Release
build, mac-studio, load average 40 to 60 from other work, so one run, not a ranking.

```
cargo nextest run -p slopty-net --release --test echo_beside_flood --run-ignored only --no-capture
```

| arm | echo p50 ms | echo p99 ms | echo max ms | frames whole | Mbit/s | before (median p50 / p99) |
| --- | --- | --- | --- | --- | --- | --- |
| bursts | 8.57 | 11.74 | 18.40 | 735 of 737 | 12.6 | 9.4 / 19.0 |
| laned | 8.94 | 33.32 | 97.38 | 764 of 766 | 12.5 | 9.8 / 17.9 |
| metered | 7.38 | 17.34 | 59.30 | 730 of 732 | 12.5 | 9.2 / 21.6 |
| halved | 13.93 | 20.86 | 21.82 | 649 of 653 | 9.7 | 21.0 / 34.4 |

No loss, no congestion events, and the same frames whole as before: video gave nothing up. The
halved arm, where an echo queues behind the capture guard's 50 kB, gained most (p50 21 → 14 ms).
Laned's p99 sits above its earlier range in this one loaded run; a quiet ten-run pass owes the
ranking.

## 2026-09-27 — the prompt rail off the face's frame

The app's frame probe (`the_face_draws_a_streaming_answer_within_a_frame`) could not run
tonight. Every app e2e test, `shell_typed_from_the_app_echoes_back_into_its_rows` included,
timed out on its first `Ping`. The reply waits for the window's next frame, and the display
drew none: its link is idle while the display sleeps. So the face was measured headless, in a
twin of the probe (`measure_a_face_frame_while_an_answer_streams`, `slopty-ui`, release, GPUI's
test platform, so no CoreText and no GPU). It builds the same made-up 80 turns in a 1000 × 720
window through the worker's own decoder (`fixtures::long`) and starts a live answer. (h)
appends a word per frame. (i) appends a word and pans ±40 points twice per word. Each frame is
the event's apply plus the draw. The runs alternated binaries, four each, at load average 10
to 15.

A profile of (i) put about a third of the frame in the prompt rail. It rebuilt eighty ticks
every frame, each with its prompt's text cloned, cut and formatted, and then laid them out and
painted them. Dropping only its layout and paint saved 0.28 ms at p50; dropping it altogether
saved 0.41 ms. The rail is now a cached view that draws only when its prompts change
(`docs/decisions/claude-code.md`, "The prompt rail is a cached view of its own").

| build | (h) following, p50 / p95 / p99 | (i) panning, p50 / p95 / p99 |
| --- | --- | --- |
| before | 1.66–1.72 / 2.03–2.72 / 2.15–8.03 ms | 1.16–1.22 / 2.75–2.82 / 3.01–3.47 ms |
| **after** | **1.34–1.39 / 1.69–1.91 / 1.82–2.50 ms** | **0.82–0.86 / 2.35–2.48 / 2.68–2.76 ms** |

The 8.03 ms before is one run hit by the load. Everything else in the table is steady.

- A pan frame now costs about 0.35 ms less and a word frame about 0.3 ms less.
- The app's panning p99 of 5.5–6.5 ms, from the entry before last, should fall by about that
  much, but that is owed a run of the app probe with the display awake.
- What is left of a word frame is GPUI laying out the rows in view and gpui-kit's `TextView`
  parsing the live block's whole Markdown again (about a quarter of the frame).
- Neither is in `slopty-ui`, and the rest of the face's own code is about 6 % of the frame.

```sh
cargo test -p slopty-ui --release --lib --no-run   # then copy the binary it names
target/bench-face/before measure_a_face_frame_while_an_answer_streams --ignored --nocapture
target/bench-face/after measure_a_face_frame_while_an_answer_streams --ignored --nocapture
```

## 2026-09-28 — a streaming answer's Markdown parsed from its last item

The same headless twin as the entry above (`measure_a_face_frame_while_an_answer_streams`,
`slopty-ui`, release, GPUI's test platform, 80 turns in a 1000 × 720 window). Both binaries
were built from one tree and differ only in the gpui-kit pin: `a3dc7ffb` before, `577b935d`
after (the fork's commits `dbd18ca4` and `577b935d`). The runs alternated binaries, four each,
at load average 8 to 10.

The face hands `TextView` the live block's whole text every frame. `set_text` used to parse
all of it again on the UI thread, and the background parser parsed it once more to keep its
copy in step. Now a Markdown text that extends the last one is appended: the append parses on
the UI thread when what it parses is small, and it parses only the last block. A list, the
last block of this answer, is parsed from its last item.

| build | (h) following, p50 / p95 / p99 | (i) panning, p50 / p95 / p99 |
| --- | --- | --- |
| before | 1.33–1.38 / 1.74–1.76 / 1.85–1.98 ms | 0.83–0.87 / 2.42–2.50 / 2.80–2.84 ms |
| **after** | **1.10–1.16 / 1.48–1.50 / 1.60–1.68 ms** | **0.83–0.84 / 2.28–2.38 / 2.52–2.74 ms** |

One (i) run after hit a 15.7 ms max from the load; its percentiles are in the table.

- A word frame costs about 0.23 ms less at p50 and 0.25 ms less at p95 and p99. A pan frame
  parses nothing, so its p50 stays; the word frames in its tail are cheaper.
- The fork's first commit alone (the last block parsed again, not the last item) changed
  nothing here. The answer's words put a paragraph line right after a list item, which
  continues the item, so after its first paragraph the whole answer is one list that never
  closes. Its last block was nearly all of it.
- Parsing alone, on the same words (a gpui-base test, release, dropped after): the whole text
  took 137 µs a word on average over 320 words, and the tail 20 µs.

```sh
cargo test -p slopty-ui --release --lib --no-run   # once per pin; copy the binary it names
target/bench-face/md-before measure_a_face_frame_while_an_answer_streams --ignored --nocapture
target/bench-face/md-after measure_a_face_frame_while_an_answer_streams --ignored --nocapture
```

## 2026-09-28 — the head band on its own step, rules only between heads

The terminal paint now lays each block's head on `surfaces.band` rather than `panel`, and it
draws the 1 px rule only where two heads touch (a command that printed nothing). A head that
follows output has no rule, because the band's top edge is the boundary
(`docs/decisions/terminal.md`, "The head band is seen and the rule parts only heads"). The
colour costs nothing. The rules used to be a field of every prepared row, set in the row loop.
They are now a list built from the head runs, the same pass that yields the bands, and are
painted after them. Headless GPUI in release on mac-studio. "Before" and "after" are test
binaries built from one scratch copy of the tree (`/tmp/slopty-band3`, own target dir), which
differ only in `terminal/element.rs`. They ran alternately, six runs each, while other
sessions built on the machine (load average 12 to 14). Runs 5 and 6 caught a heavier build,
which slowed both binaries by about 40 %. The ranges below are runs 1 to 4, and the logs hold
all six.

`failed_blocks_cost` (100 × 40, ten four-row blocks, ten heads that each follow output: ten
rules before, none after), p50 in µs:

| build | pointer away | pointer over a failed block |
| --- | --- | --- |
| before | 106.9–109.6 | 121.0–126.3 |
| after | 104.9–117.0 | 119.1–134.0 |

`dense_screen_cost` (200 × 60, no prompt marks), p50:

| build | unchanged | a line a frame | a screen a frame |
| --- | --- | --- | --- |
| before | 465–510 µs | 501–650 µs | 1 772–2 241 µs |
| after | 452–494 µs | 497–554 µs | 1 759–2 094 µs |

Both are the same within the load. With ten rules gone, three of the four block runs came out a
few µs faster. A first version kept the rule on the row and decided it in the row loop with a
lookup in the head runs, reached only on a prompt row. In four alternating runs it moved the
dense screen's screen-a-frame case from 1 793–1 857 µs to 1 902–1 993 µs, with no prompt on
screen (`target/logs/band3-dense-ab.log`). That is the loop's code generation again, as on
2026-09-27. The rules now come from their own pass.

```sh
# in a scratch copy of the tree, once with each element.rs
CARGO_TARGET_DIR=/tmp/slopty-band3/target cargo test -p slopty-ui --release --lib --no-run
/tmp/slopty-band3/bench/before failed_blocks_cost --ignored --nocapture --test-threads 1
/tmp/slopty-band3/bench/after2 failed_blocks_cost --ignored --nocapture --test-threads 1
/tmp/slopty-band3/bench/before dense_screen_cost --ignored --nocapture --test-threads 1
/tmp/slopty-band3/bench/after2 dense_screen_cost --ignored --nocapture --test-threads 1
```

Logs: `target/logs/band3-blocks-ab2.log`, `target/logs/band3-dense-ab2.log` (the first version:
`band3-blocks-ab.log`, `band3-dense-ab.log`).

## 2026-09-28 — a zoomed stream's frame

A remote picture now zooms inside its tile (`docs/decisions/input.md`, "A remote picture zooms
inside its tile, and fingers can be a trackpad"). This times the frame of the view alone in a
headless GPUI window (release, the phone's 1170 × 2532 body, a 5120 × 2880 frame as the
surface) in three cases: at fit, zoomed 4× and still, and zoomed with a pinch step every frame,
which re-clamps the zoom and draws the readout. Each run alternates six blocks of each case (50
frames of warm-up, then 100 timed), so the machine's load weighs on all three alike. One test
binary, six runs back to back, load average about 34 on mac-studio (other sessions' builds).

p50 / p95 per run, µs:

| run | fit | zoomed | pinching |
| --- | --- | --- | --- |
| 1 | 16.9 / 17.8 | 18.0 / 19.8 | 20.5 / 23.3 |
| 2 | 17.6 / 18.0 | 18.1 / 19.3 | 20.9 / 21.9 |
| 3 | 17.8 / 19.6 | 17.6 / 18.9 | 20.6 / 21.5 |
| 4 | 17.2 / 18.3 | 17.5 / 19.0 | 20.5 / 22.2 |
| 5 | 17.6 / 18.5 | 17.7 / 18.6 | 20.6 / 22.5 |
| 6 | 17.3 / 19.1 | 17.6 / 18.9 | 20.2 / 22.5 |

Spread of the p50s: fit 16.9–17.8, zoomed 17.5–18.1, pinching 20.2–20.9. A still zoomed picture
costs the same as fit within the spread (it is one surface with relative insets in place of
`size_full`). A pinching frame costs about 3 µs more, the readout's pill and its text. Maxima
(22–162 µs) are the load, and fall on all three cases. What this does not see is the GPU: the
surface is the same one quad either way, and the Metal surface shader clips it to the body's
content mask with clip distances, so the fragments shaded are the body's pixels at any zoom.
A zoomed picture does ask the worker for a larger scale, which costs encode, bandwidth and
decode; that is the stream's cost, not the frame's, and the region design in
`docs/decisions/video.md` ("A zoomed picture's region at native resolution") is its follow-up.

```sh
cargo test -p slopty-ui --release --lib --no-run
# the binary it names, copied so other sessions' builds cannot replace it mid-run
cp target/release/deps/slopty_ui-<hash> /tmp/slopty-zoom-bench
for r in 1 2 3 4 5 6; do
  /tmp/slopty-zoom-bench measure_a_zoomed_stream_frame --ignored --nocapture --test-threads 1
done
```

Log: `target/logs/zoom-measure.log`.

## 2026-09-28 — keystroke echo on a Linux worker, container on Docker Desktop's VM, debug build

The first numbers from a worker running on Linux (`docs/decisions/platform.md`, "Linux proven
in a container"). The worker, its ptyd and the relay are the `aarch64-unknown-linux-gnu` dev
builds, cross-linked here. They run in a Debian 13 container on Docker Desktop, capped at two
CPUs. The client is `tests/linux.rs` on this Mac: the client core's link and a `TermState`,
with no GPUI. It sends 300 key presses (`TermRequest::Key`, as the app's keyboard sends them)
into `/bin/cat`, with a return every 60 columns, untimed. Each sample runs from the send to the
applied frame whose cursor has moved past the echo. The path is loopback only: the published
UDP port goes through Docker Desktop's port forwarder into the VM and the container's bridge.
It says nothing about a network. mac-studio, load average about 11 (other sessions' builds).

| run | min | p50 | p90 | p99 | max | QUIC rtt |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 0.44 ms | 0.69 ms | 0.89 ms | 1.41 ms | 1.56 ms | 0.47 ms |
| 2 | 0.41 ms | 0.72 ms | 0.94 ms | 2.08 ms | 5.57 ms | 0.51 ms |
| 3 | 0.47 ms | 0.72 ms | 0.94 ms | 1.63 ms | 1.84 ms | 0.44 ms |

The echo costs about a quarter of a millisecond over the QUIC round trip. That covers the key
encoder, the PTY's line discipline, the engine's frame and the client's apply, all on Linux.
All 300 keys came back in every run. The test fails only when a key does not.

```sh
nice -n 10 cargo xtask linux e2e > target/logs/linux-e2e.log 2>&1
grep 'linux echo' target/logs/linux-e2e.log
```

Logs: `target/logs/linux-e2e.log`, `linux-e2e-2.log`, `linux-e2e-3.log`; the daemons' under
`target/logs/linux/<container>/`.

## 2026-09-28 — a file tile of any size: frame time at 2 000, 20 000 and 200 000 lines

The file tile now edits whole files up to 16 MiB (`docs/decisions/workspace.md`, "A file tile
edits any file up to 16 MiB"). The smooth probe's file scenario opens a tile of source-like
lines (`fn line_N() -> u32 { … } // padding…`, 67–77 bytes a line) beside five flooding
shells, 1280 × 800. The caret starts ten lines from the end. It then measures four things, 5 s
or 60 keys each: **(k)** typing 60 letters at 15/s into the file, **(j)** paging it (⇞ twenty
times, then ⇟ twenty times, every 60 ms; the caret's line must move), **(e)** stepping the
strip column to column, and **(f)** opening and closing the overview. The draw is the frame
probe's `begin` → `end`, in ms (p50 / p95 / p99), with the frames dropped. The e2e app build
(dev profile), mac-studio, load average 16–25 from other sessions. The arms alternate, three
rounds of five.

- **main** is this change at its tree.
- **fork** is the same tree with two gpui-kit fixes (below), built in a copy of the workspace
  with `[patch]` pointing at a patched copy of the fork (`target/files-fork/`). The fixes are
  not landed.
- The 2 000- and 20 000-line files are coloured. The 200 000-line one (14.5 MiB) is past the
  2 MiB colour limit, so it is plain text.

| arm | lines | (k) typing | (j) paging | (e) strip | (f) overview | (f) dropped |
| --- | --- | --- | --- | --- | --- | --- |
| main | 2 000 | 1.6 / 2.0 / 2.3–2.5 | 1.9 / 6.1–6.2 / 6.3 | 1.6–1.7 / 2.5–2.6 / 3.1–3.6 | 4.6–4.7 / 5.0–5.3 / 10.5–10.8 | 0, 0, 0 |
| main | 20 000 | 1.7–1.9 / 2.1–2.4 / 2.9–3.5 | 1.9 / 5.4–5.6 / 5.9–6.4 | 1.6–1.7 / 2.7–2.8 / 3.8–7.0 | 4.1–4.9 / 5.4–5.6 / 21.7–22.0 | 14, 14, 14 |
| fork | 20 000 | 1.5–1.6 / 2.0–2.1 / 2.5–3.8 | 1.6 / 5.1–5.2 / 5.3–5.6 | 1.5–1.6 / 2.4–2.5 / 3.8–7.2 | 4.6–4.7 / 5.1–5.2 / 9.4–9.6 | 0, 0, 0 |
| main | 200 000 | 5.7–6.0 / 6.2–6.5 / 6.6–7.0 | 5.8–6.0 / 9.4–9.7 / 9.7–10.2 | 1.6–1.7 / 6.5–6.8 / 7.0–7.7 | 8.5–8.7 / 133.3–133.6 / 136.8–137.1 | 112, 112, 112 |
| fork | 200 000 | 1.5–1.7 / 1.9–2.2 / 2.3–2.5 | 1.5–1.6 / 5.1–5.2 / 5.3–5.5 | 1.6 / 2.3–2.4 / 3.0–3.2 | 4.5 / 4.8–5.1 / 8.7–8.8 | 0, 0, 0 |

Typing and paging stay within a frame at every size in both arms: no draw in (k) or (j) went
over 16.7 ms. Two costs grow with the file in gpui-kit as it stands.

- **The overview.** Zooming changes the editor's font size every frame, and
  `TextWrapper::set_font` then rewraps every line, although with soft wrap off no line can
  wrap. That is about 130 ms per frame at 200 000 lines (112 of 270 frames dropped) and 20 ms at
  20 000 (14 dropped). The fix returns early when there is no wrap width.
- **Every frame of the tile.** While an accessibility client is attached, and in every e2e
  build, `Input` copies the whole rope into its accessibility value on each render. That is
  the 4 ms added to the p50 of (k) and (j) at 200 000 lines. A `sample` of the app during (k)
  put `Rope::to_string` under `Input::render` and `TextWrapper::update` under `set_font` as the
  tile's main-thread time. The fix exposes a text over 64 KiB as the 100 lines around the caret,
  as a screen reader's page of a long document.

With both fixes, 200 000 lines draw as 2 000 do, within the spread. The patch
(`target/files-fork/gpui-kit-large-files.patch`, three files) passes the fork's `gpui-base`
and `gpui-component` input tests, plus a new unit test for the page.

The background parse behind the colours costs 80–160 µs per line (the headless timing below:
328 ms for 2 000 lines, 3.1 s for 20 000, 31 s for 200 000, release, at load 15–20). A parse
that has started runs to its end. Before the colour limit, a `sample` of typing into a
coloured 200 000-line file found five background threads in `highlight::spans` for the whole
10 s. Hence `file::COLOURED_BYTES` (2 MiB). In the coloured 20 000-line arms, (k)'s frame
interval p95 is 29–71 ms against 14 ms at 2 000. No draw was long, and the main thread was
mostly idle in a `sample`. This fits the shells drawing less while the parses take the cores,
but that is not proven.

Headless, the tile's own UI-thread cost (no glyphs shaped, so a floor rather than a frame):
putting the text in, a keystroke, a scroll step and a find.

| lines | load | keystroke | scroll | find | background parse |
| --- | --- | --- | --- | --- | --- |
| 2 000 | 5.1 ms | 0.99 ms | 0.33 ms | 1.6 ms | 328 ms |
| 20 000 | 1.3 ms | 0.97 ms | 0.33 ms | 1.2 ms | 3 137 ms |
| 200 000 | 3.0 ms | 0.91 ms | 0.33 ms | 1.1 ms | 31 493 ms |

```sh
# the bins, once per arm (the fork arm: in the patched copy, CARGO_TARGET_DIR=<copy>/target)
cargo build -p slopty-ptyd -p slopty-workerd -p slopty-serverd -p slopty-cli -p slopty -p slopty-e2e --bins --features slopty/e2e
D=$PWD/target/e2e-files-perf; mkdir -p $D/run $D/artifacts
SLOPTY_SMOOTH_FILE_LINES=200000 SLOPTY_DATA_DIR=$D SLOPTY_PTYD_SOCKET=$D/run/ptyd.sock \
  SLOPTY_WORKER_SOCKET=$D/run/worker.sock SLOPTY_E2E_BIN_DIR=$PWD/target/debug \
  SLOPTY_E2E_ARTIFACTS=$D/artifacts SLOPTY_SMOOTH_E2E=1 RUST_LOG=warn \
  nice -n 10 cargo nextest run -p slopty-e2e --test smooth --no-capture -E 'test(a_large_file_tile)'
# SLOPTY_SMOOTH_FILE_NAME=big.txt: the same text uncoloured
cargo test -p slopty-ui --release --lib timing_of_a_large_file -- --ignored --nocapture
```

Logs: `target/logs/files-ab.log` (the table), `files-smooth.log` (the first series, before the
colour limit), `files-sample-typing.txt` and `files-sample-typing2.txt` (the samples).

## 2026-09-28 — the overview's miniatures against its word covers

Below zoom 0.5 the overview laid a word cover over each shell, file and note tile. It now
shows the tile's own body at the zoom, with a one-line label under it
(`docs/decisions/workspace.md`, "The overview draws miniatures"). Scenario (m) is new in the
smooth probe (`twenty_mixed_tiles_open_hold_and_close_the_overview_on_the_mac`). It opens 20
tiles: twelve shells (five flooding, six still after a screen of `seq`, one idle), four
400-line file tiles and four notes. The overview then springs open and closed six times each
way, with the frames of each spring read 500 ms after its ⌘⌥O. Then the overview is held
open for 5 s with the floods running. Each spring gives about 40 frames, so a row gives the
median of the six springs' p50s, the worst of their p95, p99 and max, and every frame
counted. Draw is the frame probe's `begin` → `end` in ms (p50 / p95 / p99 / max).

The e2e app build (dev profile) ran on mac-studio with a 1280 × 800 window and a load average
of 13–17 from other sessions. The display ran at 75 Hz (frames every 13.3 ms), and no frame
went over its period in any run. At 120 Hz a draw has 8.3 ms. Both arms are one binary. The
cover arm was a temporary `SLOPTY_MEASURE_COVERS` switch in `render_body` that drew the old
cover, and it was deleted after these runs. The arms alternated A B A B A B A B.

| arm | run | opening | closing | held open |
| --- | --- | --- | --- | --- |
| covers | 1 | 4.3 / 6.1 / 9.2 / 9.2 | 1.1 / 4.4 / 5.8 / 5.8 | 4.3 / 4.6 / 5.8 / 7.4 |
| miniatures | 1 | 1.7 / 3.1 / 5.7 / 5.7 | 1.1 / 3.0 / 4.2 / 4.2 | 1.7 / 1.9 / 2.2 / 2.9 |
| covers | 2 | 4.3 / 5.8 / 9.3 / 9.3 | 1.1 / 4.4 / 5.9 / 5.9 | 4.3 / 4.6 / 4.8 / 6.9 |
| miniatures | 2 | 1.8 / 3.2 / 5.8 / 5.8 | 1.2 / 2.6 / 3.2 / 3.2 | 1.7 / 1.9 / 2.0 / 2.9 |
| covers | 3 | 4.3 / 5.5 / 9.5 / 9.5 | 1.1 / 4.2 / 6.0 / 6.0 | 4.3 / 4.6 / 5.5 / 5.9 |
| miniatures | 3 | 1.7 / 3.0 / 6.2 / 6.2 | 1.1 / 1.8 / 3.1 / 3.1 | 1.7 / 2.1 / 2.9 / 3.3 |
| covers | 4 | 4.3 / 5.5 / 8.9 / 8.9 | 1.1 / 5.3 / 5.8 / 5.8 | 4.3 / 4.7 / 5.3 / 7.6 |
| miniatures | 4 | 1.7 / 3.0 / 5.9 / 5.9 | 1.1 / 2.4 / 3.5 / 3.5 | 1.7 / 2.0 / 2.4 / 3.1 |

- **The covers cost 2.6 ms on every frame of an open overview.** The bodies were drawn under
  the covers anyway, because the focused one keeps the keyboard and the others are cached
  views. The covers came on top: each frame laid out every cover's title, meta and last lines
  again (`tail`, `head`, a note's lines), since none of them was cached. With the floods
  dirtying the window every frame, that was the held-open p50 of 4.3 ms, against 1.7 ms now.
- **The opening spring fits 120 Hz now.** With covers its worst frame was 8.9–9.5 ms in every
  run, over the 8.3 ms a 120 Hz frame has. With miniatures the worst is 5.7–6.2 ms. Closing
  was already inside the budget and is a little lower (worst 3.1–4.2 ms against 5.8–6.0).
- **Not measured: a screen tile.** A display needs Screen Recording for the worker
  (`SLOPTY_SCREEN_E2E`), and its miniature is its last decoded frame. That frame was already
  drawn at the zoom with no cover, so this change adds nothing to it.

```
cargo build -p slopty-ptyd -p slopty-workerd -p slopty-serverd -p slopty-cli -p slopty -p slopty-e2e --bins --features slopty/e2e
D=$PWD/target/e2e-mini; mkdir -p $D/bin $D/run $D/artifacts
# one binary for every arm, safe from other sessions' builds
for f in target/debug/*; do [ -f "$f" ] && [ -x "$f" ] && cp "$f" $D/bin/; done
SLOPTY_DATA_DIR=$D SLOPTY_PTYD_SOCKET=$D/run/ptyd.sock SLOPTY_WORKER_SOCKET=$D/run/worker.sock \
  SLOPTY_E2E_BIN_DIR=$D/bin SLOPTY_E2E_ARTIFACTS=$D/artifacts SLOPTY_SMOOTH_E2E=1 RUST_LOG=warn \
  cargo nextest run -p slopty-e2e --test smooth --no-capture -E 'test(twenty_mixed)'
```

## 2026-09-28 — the focused tile cached, titles worked out when they change, notes counted once

Three changes to what a workspace frame redraws when something other than the workspace
changed (`docs/decisions/workspace.md`, "The focused tile is replayed too"):

- **F1.** The focused tile's body is drawn from the view cache like any other while the
  keyboard is on the body's own view (a shell's grid, a window, a folder, a file's editor). It
  was always drawn afresh, so a stream, a flood or an animation step beside it drew it again.
- **F3.** The twins' numbers ("Terminal 2") are worked out when a title may have changed (the
  workspace's own notify, a command starting or ending, a window named), not every frame. That
  pass read every shell's view, and GPUI tracks what a frame read, so any shell's output woke
  the window even with that shell off screen. A program's new title notifies the chrome only
  when its tile's title follows it (F6).
- **F4.** A note's title and task count are kept per text (`NoteFacts`, refreshed by
  `item_changed`), counted without building segments (`markdown::task_counts`), and a note tile
  is no longer cloned (64 KiB at most) to draw it.

Headless workspace, test profile (`dev`, deps at `opt-level = 3`), mac-studio, load average
8.5–15 from other sessions. Three test binaries built from one tree: **B0** before, **B1** with
F1 alone, **B2** with all three. The builds ran in turn, B0 → B1 → B2, three rounds. p50 in ms,
one cell per round; each arm is 400 frames after 20 of warm-up. Round 2 of B0 and B1 and
round 1 of B2 fell on load spikes (every arm of those runs moved together): read the other two.

| arm | B0 | B1 | B2 |
| --- | --- | --- | --- |
| an off-screen shell's echo, 60 shells + 60 notes | 0.495, 0.925, 0.513 | 0.560, 0.980, 0.520 | 0.001, 0.001, 0.001 (no frame) |
| the workspace's own frame, same crowd | 2.469, 3.713, 2.394 | 2.358, 3.922, 2.435 | 2.408, 2.314, 2.405 |
| a neighbour's echo beside a focused 80 × 40 grid of words | 0.543, 1.031, 0.559 | 0.494, 0.986, 0.515 | 0.507, 0.445, 0.463 |
| the focused grid rendered in those 420 frames | 420 | 1 | 1 |
| the workspace's own frame, 120 notes × 4 KiB, 60 files, 12 shells | 8.712, 11.831, 8.633 | 8.671, 13.615, 8.817 | 7.727, 7.270, 7.505 |
| the focused shell's echo in that registry | 0.592, 1.044, 0.603 | 0.601, 1.069, 0.603 | 0.563, 0.512, 0.530 |
| a shell's echo beside two drawn 64 KiB notes of task lines | 32.26, 46.60, 31.73 | 31.89, 32.61, 31.65 | 46.15, 30.59, 30.69 |

- **An off-screen shell no longer costs a frame.** Its output was a 0.5 ms frame of the
  workspace, since the twins' pass had read its view. Now nothing is drawn. Sixty idle shells
  and one busy one off screen is the ordinary case this fixes.
- **The focused grid is replayed.** It rendered once in 420 neighbour frames against every
  frame before. Here that is 0.04–0.05 ms a frame, since an 80 × 40 screen of one style
  redraws from the word cache. A dense coloured 200 × 60 screen costs about 0.47 ms a frame
  (the 2026-09-26 entry above), which is the saving a focused dense shell beside a stream now
  gets.
- **The registry's frame is 1.1–1.4 ms cheaper.** That is the notes: the navigator's rows and
  the headers counted every note's tasks by building its segments, and the twins derived all
  192 titles. An echo in that registry is 0.07 ms cheaper.
- **The long notes' frame is about 1 ms cheaper and still 31 ms.** The header's clone and two
  parses of 64 KiB per note are gone. The rest is the cached note view's replay: a `sample`
  put 56 % of the frame in GPUI's `TabStopMap::replay`, which re-inserts every one of the note's
  tab stops (a checkbox per task line, about 2 100 a note here) into a sum tree on every frame. That is
  the fork's and `note.rs`'s to fix, not this change's.

```sh
# one test binary per arm (B0: the files before; B1: B0 with `cacheable` alone), then in turn:
cargo test -p slopty-ui --lib --no-run   # copy target/debug/deps/slopty_ui-<hash> per arm
for r in 1 2 3; do for b in b0 b1 b2; do for t in measure_an_echo_frame_beside_the_chrome \
  measure_a_frame_over_a_large_registry measure_an_echo_frame_beside_long_notes; do
  (cd crates/slopty-ui && ../../target/ab/bin-$b $t --ignored --nocapture); done; done; done
# attribution: run measure_an_echo_frame_beside_long_notes and `sample <pid> 5 1`
```

Log: `target/logs/hotpath-ab.log`; the sample: `/tmp/ln-sample.txt`.

## 2026-09-28 — a note drawn as a list, a focused face and note replayed

Two changes on the frame another tile causes (`docs/decisions/workspace.md`, "The focused tile
is replayed too", amended):

- **N1.** A read note draws its segments in a gpui `list` (`NoteView::render_segment`), so a
  frame lays out and paints only the rows in view. The entry above put 56 % of the long notes'
  frame in `TabStopMap::replay` and blamed a checkbox per task line. The boxes are not focusable.
  The nodes are the segments' gpui-kit `TextView`s: each wraps itself in a `track_focus` div
  (gpui-kit `base/src/text/text_view.rs`, `request_layout`), GPUI's div paint inserts every
  tracked handle into `next_frame.tab_stops`, tab stop or not (`elements/div.rs`), and a
  replayed view replays that whole insertion history (`window.rs`, `TabStopMap::replay`). The
  test's note is 2 114 tasks and 1 057 prose runs, about 3 200 `TextView`s a note.
- **F2.** A focused face and a focused note are replayed while their fields have the keys
  (`cacheable(placed, true)` for both), and both views observe their fields.

Headless workspace, test profile (`dev`, deps at `opt-level = 3`), mac-studio, load average
13.7–15.8 from other sessions. **B0** is the tree before (with the face's render counter and
the new face arm), **B1** the tree after. The builds ran in turn, B0 → B1, three rounds. p50 in
ms, one cell per round; each arm is 400 frames after 20 of warm-up. Round 1 of B0 fell on a
load spike (every B0 arm moved together). Other sessions edited the tree between the two builds,
so B1 differs from B0 by more than this change; the notes' and the face's arms move 40× and 3×,
which that cannot account for.

| arm | B0 | B1 |
| --- | --- | --- |
| a shell's echo beside two drawn 64 KiB notes of task lines | 43.18, 33.31, 30.72 | 0.817, 0.514, 0.491 |
| a shell's echo beside a focused face (composer has the keys), `tools` transcript | 1.284, 0.769, 0.715 | 0.500, 0.282, 0.268 |
| the face rendered in those 420 frames | 420 | 0 |
| the workspace's own frame, 120 notes × 4 KiB, 60 files, 12 shells | 11.17, 8.07, 7.62 | 4.96, 3.44, 3.38 |
| the focused shell's echo in that registry | 0.845, 0.558, 0.531 | 1.025, 0.543, 0.536 |

- **The long notes' frame is 60× cheaper.** 30.7 ms was two notes replaying the paint of
  about 6 400 segments, nearly all clipped out of sight. Now each note paints the rows its tile
  shows, and the frame is the shell's echo plus the chrome.
- **The registry's own frame is 4 ms cheaper.** Its notes are 4 KiB of task lines, about 80
  segments each, and the notes on screen were replayed whole in each of those frames.
- **A focused face costs nothing in a neighbour's frame.** It was drawn afresh with every
  frame, 0.45–0.78 ms of each. Its composer's keys, caret blink and selection still draw it
  (`the_focused_face_is_replayed_while_a_neighbour_draws` checks the send button lights for a
  typed draft).
- The echo in the registry did not move: that shell is focused and the notes were already
  replayed there, at 4 KiB each.

```sh
# one test binary per arm, copied after `cargo nextest run -p slopty-ui --lib --no-run`
# (`cargo nextest list -p slopty-ui --lib --message-format json` names it), then in turn:
for r in 1 2 3; do for b in b0 b1; do for t in measure_an_echo_frame_beside_long_notes \
  measure_an_echo_frame_beside_a_focused_face measure_a_frame_over_a_large_registry; do
  (cd crates/slopty-ui && nice -n 10 ../../target/ab-notes/$b $t --ignored --nocapture); done; done; done
```

Log: `target/logs/notes-ab.log`.

## 2026-09-28 — rows kept across frames, pixels in the texture's format

mac-studio, release, load average 9–22 (other sessions building). Each comparison alternates
the two arms in one sitting, so the load falls on both; ranges are over the rounds.

**The terminal element's row cache** (`terminal::view::tests::dense_screen_cost`, 200 × 60 of
dense coloured text, 600 frames after 60 of warm-up; p50 in µs; two sittings, seven rounds of
each build, the before binary built from the tree just before the element change). A row is
kept by its `Arc<Line>` while nothing that restyles it changes (docs/decisions/terminal.md, "a
row the element built is kept while its line is"). The first after round of the first sitting
ran without the map's capacity and fell on a load spike (unchanged 375 µs, a screen a frame
3.2 ms); it is left out.

| scenario | before | after |
| --- | --- | --- |
| unchanged | 450–471 (one round at 734) | 247–258 |
| a line a frame | 486–516 (one round at 830) | 279–294 |
| a screen a frame | 1 677–1 877 (one round at 3 054) | 1 763–1 972 |
| every cell on a background, unchanged | 641–753 | 359–383 |

An unchanged dense frame and a flood cost about 45 % less: the per-cell walk was about half of
them, as the 2026-09-26 entry measured. A screen replaced whole every frame builds every row
and is unchanged within the spread (the rounds overlap; the first sitting without the map's
capacity read 3–5 % slower, which pre-sizing the map removed). p99 moved the same way:
unchanged 612–757 → 365–455 µs.

**A 12 MiB kitty image, encoded and decoded** (`codec::tests::image_event_cost`, 20 alternating
rounds, four runs). The pixels as a sequence of `u8`s against one byte string
(`serde_bytes`), the same wire bytes: median 15.3–16.5 ms → 0.75–0.91 ms (min 14.7–15.2 →
0.56–0.63 ms).

**Its texture on the UI thread** (`terminal::view::tests::image_texture_cost`, 12 rounds, three
runs): the RGBA → premultiplied BGRA conversion the view made (a `flat_map` collect) against
`placed_images` making the texture of the bytes as sent: median 1.60–1.79 ms → 0.39–0.48 ms
(a copy into the texture's buffer). The conversion moved to the worker's session actor, where
it runs once per image sent, in place: median 1.89–2.45 ms
(`session::tests::image_premultiply_cost`).

**A stream frame encoded and handed to the stream** (`codec::tests::frame_encode_cost`,
alternating, four runs): the body allocated, copied behind the prefix and copied again by
noq's slice write, against one serialisation into the prefixed buffer handed over shared.

| frame | before, median µs | after, median µs |
| --- | --- | --- |
| an echo, 1 row, 1 631 B | 3.33 | 3.25–3.29 |
| a screen, 60 rows, 96 503 B | 175.8–178.4 | 160.4–163.1 |

The echo's saving is within the noise of its 20 000 samples; a screenful saves two 96 KB copies,
about 15 µs.

```sh
# the row cache: one test binary per build, then in turn
cargo test -p slopty-ui --release --lib --no-run   # copy target/release/deps/slopty_ui-<hash>
for r in 1 2 3 4; do for b in before after; do
  target/bench-f5/$b dense_screen_cost --ignored --nocapture --test-threads 1; done; done
cargo test -p slopty-proto --release --lib image_event_cost -- --ignored --nocapture
cargo test -p slopty-proto --release --lib frame_encode_cost -- --ignored --nocapture
cargo test -p slopty-ui --release --lib image_texture_cost -- --ignored --nocapture
cargo test -p slopty-worker --release --lib image_premultiply_cost -- --ignored --nocapture
```

Logs: `target/logs/dense-f5.log`, `target/logs/dense-f5b.log`, `target/logs/image-cost.log`.

## 2026-09-28 — one transcript read for every follower, one pool trip for every agent

**The conversation face.** Each follower of a session used to own the session's transcripts and
decode them itself every 250 ms and at each hook, so N followers made N decodes of one file.
Now one `slopty_worker::conversation::Reader` per session reads for all of them and broadcasts
each read's changes. The probe follows one session with 4 clients, on the captured `tools`
session (half of its 82 records at the start, with its subagent file). It times a tick that
finds nothing new, 2 000 of them, and the second half appended one record per tick, 20
replays. The follower's copy of each shared read is counted in. Release, mac-studio, load
average 12–19 from other sessions, three rounds in one run:

```
cargo test -p slopty-worker --release --lib follower_read_cost -- --ignored --nocapture
```

| 4 followers | own decode each (before) | one shared read (after) |
| --- | --- | --- |
| a tick with nothing new | 211.7 / 208.7 / 246.3 µs | **55.7 / 52.5 / 51.9 µs** |
| a record appended | 335.7 / 329.5 / 357.8 µs | **123.9 / 133.2 / 141.3 µs** |

The idle tick scales with the followers, as expected: it was a `read_dir` of the subagents
directory and a look at every file per follower, and now it is one per session. An appended
record costs one decode plus a copy of its changes per follower, where it used to cost a
decode per follower.

**Agents nobody hooked.** Each 750 ms tick used to take every unhooked agent's transcript tail
to the blocking pool on its own, one after the other. Now one trip reads them all. The probe
uses 16 idle agents (nothing new in any file) and 2 000 ticks. Release, mac-studio, load average
7–12, three runs of three rounds:

```
cargo test -p slopty-workerd --release --bin slopty-worker transcript_tick_cost -- --ignored --nocapture
```

| 16 idle agents | a trip each (before) | one trip (after) |
| --- | --- | --- |
| run 1 | 391.9 / 432.4 / 369.7 µs | 248.1 / 290.1 / 504.3 µs |
| run 2 | 478.1 / 484.1 / 501.3 µs | 278.0 / 275.2 / 239.4 µs |
| run 3 | 363.3 / 367.4 / 366.1 µs | 232.0 / 233.4 / 232.0 µs |

The median tick went from 392 to 248 µs. The one inverted round (504 µs) stands alone among
nine, on a machine shared with other builds. What is left is the reads themselves, a stat and an open
per file. The same tick no longer copies each probe's program name, command line, title and
directory into the agent table's observation: it moves them.

The pump twin of `datagram_send_cost` (2026-09-25, "datagrams from the encoder's thread") is
deleted. Its comparison is recorded there, and the path it measured is gone.

## 2026-09-28 — a remote pointer drawn where the client put it, and cursor samples on frames

Three changes to `ScreenView` (`docs/decisions/input.md`, "A window stream's pointer is where
its input put it", amended; `docs/decisions/video.md`, "A cursor sample draws only what it
moves, and rides the frames").

**The pointer on a window stream.** Before, a move drew nothing: the pointer went up when the
worker's sample of it came back, one round trip, up to a cursor tick (8.3 ms) and a frame later.
On a 10 to 40 ms mesh, with the 19 ms a lone frame takes from notify to glass ("submit to glass",
above), that is about 30 to 67 ms. Now the move redraws the view and the pointer is in that
frame, about 19 ms. The headless test holds it: one render per move, the pointer's element at
the new point in the worker's picture, and no cursor sample in between
(`a_window_streams_pointer_is_drawn_where_this_client_put_it_in_the_same_frame`). The same
holds on a display while this client moves the pointer.

**Draws per second of video with the worker moving the pointer.** Headless GPUI draws a window
at the end of every update that dirtied it, which is what an idle macOS window on the fork
does (it draws on the next main-queue turn). So the count is how many draws the view asks for.
Sixty frames at 60 Hz and 120 cursor samples at 120 Hz over one second, samples 4 ms off the
frames' phase, then 30 ms of quiet:

| rule | draws |
| --- | --- |
| before: every sample notifies, and so does every frame | 180 |
| after: a sample draws only what it moves, and rides the next frame | 61 |

The 61st draw is the last two samples, which came after the last frame and were drawn on their
own once the next frame was more than half a refresh late. With frames at 60 Hz on a 60 Hz
display, the old rule put a cursor-only draw inside most refreshes a frame also landed in. That
frame then went up a refresh late: 31.9 against 18.9 ms to glass ("echo, key → glass", above).
The "before" row is the same test with the pump's cursor arm notifying on every sample, the old
rule.

**The view's own frame** (`measure_a_zoomed_stream_frame`, the fallback arrow drawn at the
worker's sample). The arrow used to be tessellated twice on every paint and the accessible label
formatted on every render. Now the arrow is tessellated once per thread and moved into place,
and the label is kept. The HUD's text is kept as a shared string (not in this case: the HUD is
off). One test binary per arm (the `dev` profile), alternated, three runs, load average 13–19,
p50 in µs:

| case | before | after |
| --- | --- | --- |
| fit | 22.3 / 22.6 / 23.5 | 20.1 / 19.3 / 19.8 |
| zoomed | 23.5 / 23.2 / 23.8 | 20.2 / 19.2 / 19.9 |
| pinching | 25.7 / 26.6 / 27.4 | 24.4 / 22.7 / 22.7 |

About 3 µs a frame. The p95s (23–85 µs) followed the load, not the arm. The "before" binary
rebuilt the old overlay (a full-size canvas tessellating the arrow in its paint) and the old
label, with everything else the same.

**A stream frame beside the chrome** (`measure_a_stream_frame_beside_the_chrome`, new): 60
shells and 60 notes, the navigator docked, a remote window focused with a 2560 × 1600 picture up
and the last shell drawn beside it. Each timed frame is a `notify` of the `ScreenView`, which is
what the pump sends per frame. 400 frames after 20, three runs, load average 15–19, ms:

| run | stream frame p50 / p95 | shell echo p50 / p95 | workspace p50 / p95 |
| --- | --- | --- | --- |
| 1 | 0.463 / 0.628 | 0.477 / 0.697 | 2.172 / 2.515 |
| 2 | 0.450 / 0.562 | 0.458 / 0.576 | 2.234 / 3.305 |
| 3 | 0.453 / 0.554 | 0.462 / 0.524 | 2.264 / 2.723 |

The workspace root rendered 420 times for the 420 stream frames, as did the view. At the pinned
GPUI (`Window::mark_view_dirty`), a view's notify marks every ancestor view dirty, and the draw
starts at the root. So a frame of video costs what an echo does, about 0.45 ms, and the view's
own share is the 20 µs above. Nothing in `ScreenView` can stop this: `notify` is the only way to
put a new picture up. This is the baseline for the next step, an element in the fork that
presents a new surface without dirtying any view.

```sh
# the draw count, and the pointer tests
cargo nextest run -p slopty-ui --no-capture -E 'test(cursor_samples_ride_the_frames_while_they_flow)'
cargo nextest run -p slopty-ui -E 'test(a_window_streams_pointer) | test(a_displays_pointer)'
# the view's frame: one test binary per arm, copied, then in turn
cargo test -p slopty-ui --lib --no-run   # copy target/debug/deps/slopty_ui-<hash> per arm
for r in 1 2 3; do for b in before after; do
  (cd crates/slopty-ui && nice -n 10 ../../target/ab-pointer/$b measure_a_zoomed_stream_frame \
    --ignored --nocapture --test-threads 1); done; done
# the stream frame beside the chrome
cargo test -p slopty-ui --lib measure_a_stream_frame_beside_the_chrome -- --ignored --nocapture
```

Logs: `target/logs/pointer-ab.log`, `target/logs/stream-frame-chrome.log`.

## 2026-09-28 — audio behind the picture: the output device, the queue, and a render callback

Release, mac-studio, default output "Mac Studio Speakers". Nothing played: the device's terms are
read from the HAL, the `AudioQueue` rendered offline into memory, and the playout runs on its
synthetic traces. `docs/decisions/audio.md` (2026-09-28, "Playback renders straight from the
ring") holds the ruling.

```sh
cargo nextest run -p slopty-codec --release --run-ignored only -E 'binary(audio_latency)' --no-capture
cargo nextest run -p slopty-codec --release latency_tests --no-capture
```

**The output devices** (`output_device_latency`, frames at 48 kHz). What a sample written by an
output callback waits before the DAC is the I/O buffer it went into, the safety offset the HAL
keeps ahead of the hardware's read position, and the device's and its stream's latency:

| device | I/O buffer (range) | safety offset | device latency | stream latency | total |
| --- | --- | --- | --- | --- | --- |
| Mac Studio Speakers (default) | 512 (15–4 096) | 76 | 72 | 169 | 829 frames, 17.3 ms |
| display audio (24B2W1G5) | 512 (15–4 096) | 320 | 88 | 0 | 920 frames, 19.2 ms |

**What an `AudioQueue` holds** (`audio_queue_holds_what_is_enqueued`). Three 10 ms buffers whose
samples count frames from 1, rendered offline 512 frames at a time: all 1 440 frames come out in
order, the first at render frame 0, and three buffers come back. The queue adds no delay of its
own; it holds what is enqueued. The player kept three 10 ms buffers enqueued and refilled one as
soon as the device had taken it, so 20 to 30 ms always sat between the ring and the I/O buffer.

**The whole path, at the DAC.** Before: the ring, then 20–30 ms in the queue, then 17.3 ms of
device. After: the ring, then 17.3 ms. The ring's own depth, from the synthetic traces, with the
device now rendering 512 frames at a time (the 2026-09-25 runs pulled 480):

| trace | ring (heard) | at the DAC, before | at the DAC, after | underruns |
| --- | --- | --- | --- | --- |
| 250 ms stall, then its backlog in one burst | 31 ms at most after the burst, 21 ms mean over 1.5–3 s | 58–68 ms (mean) | 38 ms (mean) | 1 |
| worker clock +100 ppm, 4 min | 30 ms at 5–15 s, 28 ms at 230–240 s | 65–77 ms | 45–47 ms | 0 |
| worker clock −100 ppm, 4 min | 28 ms, then 21 ms | 58–75 ms | 38–45 ms | 0 |
| a 40 ms keyframe burst every 2 s | 49 ms mean after 3 s, 78 ms at most | 86–96 ms | 66 ms | 3 |
| muted 2–3 s, a 400 ms gate inside | 30 ms mean from 4 s | 67–77 ms | 47 ms | 0 |
| renders of 1 024 frames (iOS's default), 1 ms scatter, 20 s | 18 ms mean after 3 s | — | — | 0 |

The before column is the same ring with the queue's 20–30 ms added; the queue did not change
what the ring held. Video is presented on arrival, so this is how far sound trailed the picture
on top of the network, less the decode and present time of a frame: 20 to 30 ms less now. The
drift traces run four minutes instead of two. Two minutes of a clock 100 ppm slow moved the ring
12 ms, which stayed inside the ±7.5 ms deadband from where it started, so no slice was needed to
show. Four minutes move it 24 ms.

**The render callback at the device's size**
(`the_render_callback_plays_the_ring_in_order_at_the_devices_size`): twelve on-time packets
rendered 512 frames at a time through the `AURenderCallback` itself, with a buffer one frame
larger than asked. 11 264 of 11 520 frames played, sample for sample after the start's 2.5 ms
fade-in, with no underrun or slice and nothing written past the frames asked for. The rest is
the last partial render, left in the ring when the run ends.

Not measured: the output unit running on a device. That plays through this machine's output,
which the run above avoids. `player_starts_and_drains` starts it on silence in the gate's run.

## 2026-09-28 — the render callback's cost, behind a lock and wait-free

Release, mac-studio. `render_cost` calls the `AURenderCallback` itself on 512-frame buffers,
200 000 times per scenario, and times each call with `Instant` (about 20 ns of that is the
clock). Nothing plays: the output unit is never opened. "Same thread" pushes the decoder's
packets between renders. "Decoder beside" pushes them from a second thread that keeps the ring
three renders ahead, so the two sides contend the way the stream task and the device's I/O
thread do. The bench threads are not real-time, so the maxima include preemption on both
sides. `docs/decisions/audio.md` (2026-09-28, "The device's thread never waits on the
decoder's") holds the ruling.

```sh
cargo nextest run -p slopty-codec --release --run-ignored only -E 'test(render_cost)' --no-capture
```

Three runs each, ns:

| ring | scenario | p50 | p99 | p99.9 | max |
| --- | --- | --- | --- | --- | --- |
| mutex (before) | same thread | 417 | 542 | 1 792–2 000 | 56 917–57 500 |
| mutex (before) | decoder beside | 666–667 | 3 250–4 125 | 21 917–31 333 | 132 084–812 792 |
| SPSC atomics (after) | same thread | 500 | 625 | 917–1 875 | 70 500–100 459 |
| SPSC atomics (after) | decoder beside | 625 | 1 000–1 041 | 1 625–2 417 | 55 750–70 000 |

The median rises 83 ns alone because the atomic copy is not vectorised. That is under 0.001%
of the 10.7 ms buffer. With the decoder beside it, the lock's tail is gone: p99 is 3–4× lower,
p99.9 9–19× lower, and the worst render dropped from 813 µs, 8% of the buffer, to the same
preemption floor the uncontended runs show. On a real-time thread the before case is worse than
this, since a preempted lock holder at normal priority keeps the device parked.

The synthetic traces (`cargo nextest run -p slopty-codec --release latency_tests --no-capture`)
read the same after the change as in the entry above: 31 ms at most after a stall's burst with
one underrun, 30 → 28 ms at +100 ppm and 28 → 21 ms at −100 ppm with none, 49 ms mean and 3
underruns under keyframe bursts, 30 ms after a gate while muted, 18 ms at 1 024-frame renders.

## 2026-09-28 — moves queued behind a stall, the client's datagram path, the cursor's picture

mac-studio, debug test profile (optimized), shared with other sessions' builds (load average
6–24 across the runs). Logs: `target/logs/input-coalesce-*.log`,
`target/logs/client-datagram-*.log`.

```sh
cargo nextest run -p slopty-input --test cost --run-ignored only --no-capture
# the client: one test binary per build, alternated
cargo test -p slopty-client --lib --no-run   # copy target/debug/deps/slopty_client-<hash>
for r in 1 2 3 4 5; do for b in before after; do
  target/bench-client/$b frame_path_cost --ignored --nocapture
  target/bench-client/$b datagram_reader_cost --ignored --nocapture; done; done
cargo nextest run -p slopty-client --lib --run-ignored only --no-capture \
  -E 'test(wake_timer_cost) | test(datagram_reader_cost)'
cargo nextest run -p slopty-capture --lib --run-ignored only --no-capture \
  -E 'test(cursor_shape_cost) | test(pointer_read_cost)'
```

**Moves queued behind a stall** (`moves_queued_behind_a_stall`, new). A window stream gets moves
at 500 Hz and 40 clicks. The owner lookup in front of each press holds the input thread for
17 ms, the worst lookup measured in "input injection off the runtime", and the release comes
10 ms after the press. The backend builds the `CGEvent` it would post and posts nothing. Before,
the input thread posted every queued move in turn. Now a move with another move queued behind it
(new bounds or a new scale between them do not count) is passed over. Three runs before, two of
the stall case, three after:

| | before | after |
| --- | --- | --- |
| moves posted, of 6 000 | all | 257–264 passed over |
| handed → posted, every post, p99 | 18.8–18.9 ms | 10.1–10.7 ms |
| handed → posted, the release, p50 | 7.6–7.9 ms | 7.3–8.1 ms |
| handed → posted, the release, max | 10.0–10.5 ms | 9.4–10.7 ms |

The p99 drop is the stale moves themselves: they no longer wait out the stall and then post.
The release still waits for the rest of the stall. With posts this cheap, six stale moves
cost it microseconds. A real `CGEventPostToPid` costs more than building the event, but
posting can't be measured here without Accessibility and without input reaching a window,
so the release's saving on the worker is six posts' worth, unmeasured. On a display stream
each stale move also moved the real pointer along a path the client had already left.
`injection_cost_on_the_callers_thread` is unchanged within its spread (thread move p50
2.4–3.2 → 2.8–4.5 µs, handed → posted p50 8.5–10.5 → 8.8–17.5 µs). It passed over 0 and 3
moves in the three runs after.

**The client's datagram path.** The connection's reader takes everything the connection
holds with one `read_many_datagrams` (noq 1.3, one lock of the connection per read, up to 64)
and routes the screen datagrams among them under one lock of the router
(`ScreenRouter::route_many`). Before, it made one `read_datagram` and one routing lock per
datagram. The stream's worker keeps one pinned `Sleep` and moves its deadline, where it
used to make a new one each wake. It reads the path's round trip once a report, not on
every wake. It takes the decoder's leftovers only when the callback flagged some.

What each wake paid, measured alone:

| per wake | before | after |
| --- | --- | --- |
| the timer (`wake_timer_cost`, 200 000 wakes, three runs of three rounds) | 335–403 ns (a sleep made, registered, dropped unfired) | 52–56 ns (the deadline pushed later) |
| the round trip (`endpoint::rtt` on the client's end while a frame arrives) | p50 667–834 ns, p99 4.0–4.8 µs, max 15–84 µs | not read |

The whole path under the same load did not move measurably. `frame_path_cost` routes
62 KB keyframes (51 datagrams) at 60 a second through the existing screen `Harness`; the
decoder rejects each, which keeps a refresh pending and the loop on its 2 ms tick, so about
13 wakes a frame. A cursor datagram routed behind each frame marks when the worker has handed
the frame to the decoder. `datagram_reader_cost` sends the same frames over loopback QUIC to
a client runtime of its own. Five alternated pairs (busy time is the runtime's workers'
`worker_total_busy_duration`; "quietest" is the least disturbed of 20 one-second rounds):

| | before | after |
| --- | --- | --- |
| stream worker busy per frame, quietest round | 48.8–74.5 µs | 55.7–82.9 µs |
| stream worker busy per frame, median round | 58.4–163.4 µs | 58.1–167.8 µs |
| last datagram routed → at the decoder, p50 | 42–70 µs | 41–75 µs |
| client runtime busy per datagram, quietest round | 10.4–18.0 µs | 10.4–20.9 µs |
| sent → last datagram routed, p50 | 884–1 619 µs | 894–1 211 µs |

The spread between runs of one binary is several times the few microseconds a frame saves
(13 wakes × about 0.3 µs of timer, plus a round-trip read that costs 0.7 µs at the median
and up to tens of microseconds when the connection's driver holds the lock). Sent → last
routed is mostly noq's pacer spreading the 51 datagrams. The change is kept for the per-wake
figures, which are direct, and for the round-trip read's tail, which lands on the stream's
task when the driver holds the lock.

**The cursor's picture** (`cursor_shape_cost`, new, three runs). It reads the cursor, never
the screen, so it runs without a Screen Recording grant. `NSCursor.currentSystemCursor` alone
took p50 151–157 µs, p99 375–431 µs. The whole `cursor_shape(2)` read took p50 188–196 µs,
p99 399–432 µs, so picking the representation, `CGImageForProposedRect`, the copy and the
conversion come to about 35 µs of it. In 199 consecutive reads of an unchanged cursor the
system never returned the same `NSCursor` or `NSImage` object twice. A throwaway probe also
showed fresh representations and a fresh `CGImage` on every read. There is no identity to
dedupe on. A digest of the pixels would still pay `currentSystemCursor` and
`CGImageForProposedRect` first, so it could save at most the copy and conversion, a few
microseconds of 190. The shape loop is left as it is (`docs/decisions/video.md`, 2026-09-28).
`pointer_read_cost` in the same runs: `pointer_location` 0.73–1.68 ms, `pointer_moves` 14 ns.

## 2026-09-28 — input behind a quality change

mac-studio, release test binary copied to `/tmp` (the repo volume is mounted `noowners`, which
makes each VideoToolbox session revalidate its code signature and build about 200× slower),
machine shared with other sessions' builds. A window stream's task handled `SetQuality` inline:
it waited for any resize build under way, then built the new session and put it in, and the
input queued behind the change waited for all of it. Zoom and tile resizes send a quality change
as often as every 400 ms. Now the change maps input and the pointer at the new scale at once and
its session is built beside the commands, in the arm a resize's build already had.

`input_behind_a_quality_change` (new, `apps/slopty-worker/src/screens.rs`) serves a stream on a
test platform: a capture that never delivers, VideoToolbox building 1600 × 1000 ↔ 800 × 500 HEVC
sessions, and an input sink that notes when the stream hands it an event, which is where the
real one queues it for its input thread. Each of 40 rounds sends a quality change that flips the
scale, then a move, and times the move from its send to the hand-over.

```sh
cargo build -p slopty-workerd --tests --release   # copy target/release/deps/slopty_worker-<hash>
/tmp/slopty-a/after --ignored --exact screens::serving::input_behind_a_quality_change --nocapture
```

| move sent → handed to the input thread | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| before, p50 / p90 / max | 3.1 / 5.8 / 7.4 ms | 2.9 / 5.5 / 8.7 ms | 3.8 / 6.0 / 7.7 ms |
| after, p50 / p90 / max | **12 / 33 / 246 µs** | **11 / 18 / 84 µs** | **11 / 31 / 85 µs** |

The before column is the session build itself, the 3.5–4.3 ms of "encoder sessions off the
runtime". The worst build measured there, 42 ms, would have held input as long.

The first after-run found a bug the change had introduced. A geometry read started before the
quality change and landing while its session was built replaced the build with its own answer
(no resize), so the new session was dropped and never went in. A read that lands while a build
is pending is now let go; the next tick after the build reads again. Test:
`a_read_landing_during_a_quality_build_does_not_drop_it`, which fails without the fix.

## 2026-09-28 — window input through a lossy link

mac-studio, test profile, machine shared with other sessions (load average 14–36 across the
runs). A window stream's input rode only the ordered control stream, at the paced priority,
while a terminal's has had datagram copies and the unpaced echo priority since 2026-09-25. Now
each window input also goes once as a `ClientDatagram::ScreenInput` copy 2 ms after its stream
copy, and the client's writer raises the control stream to the echo priority for it as it does
for typed input.

`window_input_through_a_lossy_link` (new, `apps/slopty-worker/src/conn.rs`) runs the real client
link, a `slopty-shape` relay with 4 ms each way and 13 % of packets lost each way (the
keystroke copies' shaped loopback), real QUIC, and the worker's datagram reader and ordering,
into a stream's task on the test platform of "input behind a quality change". Each input is
timed from the client handing it to its link to the stream handing it to the input thread. Two
patterns. A drag: 1 500 inputs 6 ms apart, a press or a release every tenth, moves otherwise.
Clicks: 200 lone presses and releases 50 ms apart, as keys typed into a window come.

```sh
SLOPTY_ECHO_COPY=off cargo nextest run -p slopty-workerd --bin slopty-worker --run-ignored only \
  -E 'test(=conn::lossy::window_input_through_a_lossy_link)' --no-capture
cargo nextest run -p slopty-workerd --bin slopty-worker --run-ignored only \
  -E 'test(=conn::lossy::window_input_through_a_lossy_link)' --no-capture
# the same with SLOPTY_E2E_PATTERN=clicks, and SLOPTY_E2E_LOSS=0 for the clean link
```

Before is the code as it was: no copies, window input paced. After is copies on and window
input unpaced. The middle row is the lift alone, copies off. Ranges over the runs:

| drag, 13 % loss | runs | clicks p50 / p90 / p99 / max | moves p50 / p90 / p99 / max | moves over 50 ms |
| --- | --- | --- | --- | --- |
| before | 7 | 4.4–10.0 / 22–36 / 52–214 / 64–280 ms | 5.0–9.1 / 22–40 / 49–226 / 69–309 ms | 0.9–7.8 % |
| lift, no copies | 3 | 4.3 / 22–23 / 40–51 / 45–59 ms | 4.3 / 22 / 39–43 / 56–78 ms | 0.3–0.5 % |
| after | 3 | **4.2–4.3 / 8.0–11 / 15–32 / 21–58 ms** | **4.2–4.3 / 7.8–10 / 12–19 / 17–52 ms** | **0–0.1 %** |

| lone clicks, 13 % loss | runs | p50 / p90 / p99 / max | over 50 ms |
| --- | --- | --- | --- |
| before | 2 | 4.3–4.4 / 27–28 / 30–54 / 52–80 ms | 1.0–2.5 % |
| lift, no copies | 3 | 4.4–6.4 / 27–43 / 52–126 / 65–176 ms | 1.5–8.0 % |
| after | 5 | **4.3–4.4 / 6.8–8.3 / 30–54 / 55–72 ms** | 1.0–1.5 % |

A lost click waited for QUIC to learn of the loss and retransmit, about 27 ms on this 9 ms
path, and every move behind it waited with it. Its copy arrives 2 ms after the lost packet
would have. The tail that is left is a click whose packet and copy were both lost: then the
moves behind it wait for the retransmission, since they may not overtake it. In a drag the
copies took 294–382 inputs of 1 500 ahead of their stream copy, and 24–81 moves were passed over
because a newer one had already been applied.

The copies first went out without the lift, and made the drag worse: p99 up to 634 ms and max
up to 723 ms in some runs, against 309 ms before. The client's congestion window sat at its
floor, and the pacer, which charges a whole packet's credit however small the packet, held
twice the packets it held before. Unpaced, as a keystroke is, the drag lost those stalls, and so
did the stream copies alone (the middle row).

Clean link (loss 0, 4 ms each way), drag, two pairs at load average 35: before p90 4.3–7.3 /
max 8.2–42 ms, after p90 5.5–8.9 / max 30–78 ms. The copies go nowhere there (every one late),
yet each costs a spawned task and a 2 ms timer on the client and a decode on the worker. The
tails of both rows come and go with the load, and this run cannot separate that cost from it.

## 2026-09-28 — capture to the glass and input to the glass, from drawn pictures

A worker started from a shell has no Screen Recording, so the stream's source is
`slopty_capture::synthetic::Canvas`. It stands in for the main display (1920×1080 at 75 Hz here)
and draws a picture on the display's beat: a dark desktop with a text window scrolling 3 rows a
frame, and a strip of 16 blocks that spells how many inputs the process has taken. Each picture
is stamped on the host time clock, as ScreenCaptureKit stamps. `slopty_worker::screen::synthetic::Drawn`
is the canvas with the real VideoToolbox encoder, so everything after the capture call is the
product's `Pipeline`: the cadence gate, encode, packetize and send. Its input sink counts each
event, and the next picture shows the count.

The client side is the product's too: `ScreenRouter`, `spawn_screen` (reassembly, NACK and
refresh, VideoToolbox decode) and the element's `Pacer`. The pacer now also times capture →
shown through a `ClockAnchor`, which is exact here because both ends read mach time. It times
input sent → shown from `input_sent` / `input_visible`, and the test reads the strip off each
decoded picture to know which frame first shows a click. Between the two ends, datagrams go
straight into the router on loopback. The tailnet-shaped run sends them down a delay line of
5 ms each way with 3 % of datagrams dropped (`ScreenRouter::with_loss`), which is
`harness::TAILNET`. Reports and loss feedback go back to the stream's `StreamControl` 5 ms
later. A click (`ScreenInput::Button`) is sent every 80–150 ms and reaches `Pipeline::inject`
one way later. Paints come on a timer at the display's rate standing in for the display link,
the same stand-in as `screen_start_up_over_quic`'s pacing. The runtime is user-interactive,
as the app's is. The first second is dropped as warm-up, and then each case runs 20 s.

```sh
cargo test -p slopty-worker --lib --no-run      # prints target/debug/deps/slopty_worker-<hash>
mkdir -p /tmp/slopty-glass && cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-glass/slopty_worker
cd /tmp/slopty-glass && ./slopty_worker --ignored --exact \
  screen::synthetic::tests::capture_and_input_to_glass --nocapture   # SLOPTY_GLASS_SECONDS=20
```

The binary runs from `/tmp` because `/Volumes/Lacie` is mounted `noowners`, and every VideoToolbox
session revalidates the binary's signature from there. There were three runs, with the load
average between 35 and 92 (other sessions' builds). Each value is p50 / p95 / max in ms, one run
per cell:

| loopback | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| **capture → painted** | 14.0 / 15.8 / 29.5 | 14.4 / 31.2 / 62.8 | 14.2 / 30.4 / 56.8 |
| **input sent → painted** (pacer, n=174) | 23.1 / 38.0 / 42.0 | 28.6 / 48.8 / 70.8 | 28.6 / 48.3 / 60.0 |
| worker encode (submit → VideoToolbox callback) | 7.6 / 10.2 / 13.3 | 10.1 / 15.8 / 33.1 | 8.4 / 13.0 / 28.6 |
| capture → arrival (encode, packetize, route) | 7.7 / 10.1 / 15.7 | 9.4 / 16.1 / 33.5 | 8.7 / 13.1 / 17.7 |
| arrival → decoded | 1.1 / 2.2 / 12.6 | 1.4 / 7.5 / 32.8 | 1.9 / 8.2 / 17.2 |
| capture → decoded | 8.8 / 11.5 / 20.4 | 11.3 / 22.2 / 43.2 | 11.1 / 20.4 / 30.3 |
| decoded → painted | 4.7 / 6.9 / 15.6 | 4.7 / 15.9 / 47.8 | 3.9 / 15.6 / 36.2 |
| input at the worker → the capture showing it | 7.8 / 23.2 / 26.5 | 8.9 / 23.8 / 52.7 | 9.3 / 23.9 / 28.2 |
| frames encoded / s (75 Hz beat, 60 ceiling) | 60.0 | 59.3 | 59.8 |

| tailnet-shaped: 5 ms each way, 3 % loss | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| **capture → painted** | 25.7 / 27.2 / 40.0 | 26.5 / 39.9 / 52.4 | 26.9 / 37.6 / 59.5 |
| **input sent → painted** (pacer) | 52.4 / 93.1 / 96.9 | 59.7 / 97.1 / 104.3 | 62.4 / 100.9 / 129.1 |
| worker encode | 7.8 / 9.2 / 25.6 | 9.4 / 16.5 / 45.7 | 7.9 / 12.8 / 27.9 |
| capture → arrival | 14.0 / 16.0 / 30.1 | 16.5 / 24.7 / 69.0 | 14.5 / 21.5 / 35.6 |
| arrival → decoded | 1.3 / 2.4 / 5.9 | 2.4 / 8.2 / 23.4 | 1.8 / 5.9 / 16.7 |
| decoded → painted | 10.4 / 12.2 / 13.8 | 7.8 / 13.8 / 22.3 | 10.6 / 13.4 / 23.1 |
| input at the worker → the capture showing it | 21.0 / 61.6 / 65.7 | 23.1 / 61.9 / 66.3 | 26.4 / 62.9 / 74.7 |
| frames encoded / s | 27.1 | 26.7 | 26.0 |

Drawing a picture takes 0.07 ms p50 and 0.2–0.3 ms p95 on the canvas's queue. No frame was lost
and none failed to decode. The shaped runs recovered 34–59 frames through parity and sent 2–5
NACKs.

Where the time goes on loopback, capture → painted p50 14 ms:

- **The encoder is 8–10 ms, 60–70 % of the total.** That is its floor on this machine. HEVC and
  H.264 read 6.4–8.0 ms p50 at 1080p in every earlier row on 2026-09-05, whatever the rate
  control or content. Nothing on this side of VideoToolbox is left to cut.
- Packetize and the hand-over to the client are what remains of capture → arrival once the
  encode is taken out: 0.1–0.3 ms at the median.
- Decode is 1.1–1.9 ms.
- The wait for the paint is 4–5 ms. That is the timer's phase and not a cost of the path. The
  timer and the canvas run at 75 Hz off one clock, so a run holds one phase: anywhere from 0 to
  13.3 ms, 6.7 ms on average against an unrelated display.
- Input adds the wait for the next beat that the cadence gate lets through: 8–9 ms p50, which
  is about half a beat plus the one beat in five the 60 ceiling skips at 75 Hz.

**The one hop clearly above its floor is on the lossy path: input at the worker → the capture
that shows it, 21–26 ms p50 and 62 ms p95, against 8–9 and 24 on loopback.** 3 % random loss is over
`OVERUSE_LOSS_PERMILLE`, 20 ‰, so the rate controller reads it as overuse. It cuts to its
1 Mbit/s floor within a few seconds, `9.0(Cut) 6.8(Cut) … 1.0(Cut)` in a 12 s run. The
cadence ladder then drops to about 27 fps, so a click waits up to three beats for a frame the
rung lets through. Parity recovered every lost datagram, so the cut bought nothing the picture
needed. That is also why input → painted doubles from 23–29 to 52–62 ms p50, while capture →
painted only rises by the 5 ms flight and the phase. It matches
`docs/decisions/transport.md`, where a 3 % loss link pins the controller at its floor. Deciding
loss by what parity could not recover is a policy change with its own measurement, not a small
fix, so nothing was changed here.

What these numbers leave out:

- **ScreenCaptureKit's own hop.** On 2026-09-05 it read 0.00 ms on the display path, where the
  frame comes before its display time, and 0.42 ms p50 on the window filter. The real capture →
  decoded on loopback is the 2026-09-26 bench's 5.2–9.9 ms p50, which agrees with the 8.8–11.3
  ms here once the display path's early hand-over is allowed for.
- **QUIC.** Datagrams skip the connection: the loopback column has no transport in it, and the
  shaped one has a delay line and drops in place of the relay. The rate controller gets no path
  sample.
- **The glass.** Painted is a timer's tick. A GPUI window shows a paint one compositor frame or
  more later, and `Pacer::shown` gets that moment from the window's presentation report in the
  app. No window presented in this session. The app self-test cannot use the canvas either: its
  worker is the product binary, which never builds a `Pipeline<Drawn>`. Closing this needs a
  test switch in `apps/slopty-worker` to serve the canvas, and capture and input → shown in the
  app's test dump, and then `cargo xtask e2e app`/`pair`.
- **The application's redraw.** The canvas shows a click on its next beat, and a real
  application adds its own render.

This is also the moving source at the panel's rate that the 75 Hz row above was owed. At the
default 60 ceiling on a 75 Hz beat, 59.3–60.0 frames/s were encoded, four beats in five.

`slopty bench screen` now runs the same pacer on a 60 Hz timer and prints capture → painted and
arrival → painted, loopback only, for a worker that can capture. It was not run: the only such
worker here is the installed one, and it would capture this Mac's screen.

## 2026-09-28 — the session loop's catch-up, find and ptyd costs

Release builds on mac-studio, other sessions building (load average 15 to 30). "Before" is
the tree as it was, "after" is the change, each run from the same test. One before run each,
three after runs for ptyd.

```
cargo nextest run -p slopty-worker --release --test session_actor --run-ignored only \
  -E 'test(stale_viewers_catch_up_cost)' --no-capture
cargo nextest run -p slopty-engine --release --run-ignored only \
  -E 'test(search_after_output_cost) | test(search_cost)' --no-capture
cargo nextest run -p slopty-ptyd --release --run-ignored only \
  -E 'test(resize_cost) | test(checkpoint_transfer_cost)' --no-capture
```

| measurement | before | after |
| --- | --- | --- |
| eight viewers owed every row of a full 200 × 60 screen, the last one's whole frame after they get room, p50 / p90 / max of 10 | 7.81 / 8.40 / 8.64 ms | **2.85 / 3.13 / 3.30 ms** |
| a find bar's refresh: the same needle over 50 621 lines after 30 more were written, p50 / max of 20 | 19 983 / 22 554 µs | **144 / 265 µs** |
| search again, nothing written, 50 001 lines, plain p50 (the needle alternates, so each is scanned) | 7.6 ms | 6.1 ms |
| a resize then an output tap on the worker's ptyd connection, p50 / p99 / max of 500 | 41 / 132 / 306 µs | **1 / 13–16 / 17–44 µs** |
| a 4 MiB checkpoint to ptyd, then `List`, p50 / max of 20 | 1 130 / 2 661 µs | 513–597 / 1 338–1 377 µs |

Each stale viewer used to cost the session loop its own `join_frame`: every row read through
libghostty and encoded, once per viewer, in one flush. Now one whole frame is built and encoded
per flush and the same buffer goes to each of them (`stale_viewers_catching_up_together_share_one_whole_frame`).
What is left of the 2.85 ms is the one build at 200 × 60 and the eight deliveries.

Search used to format the whole history as plain text whenever anything had been written, and
during output something always had: 10 ms of formatting and 6 to 10 ms of scanning at 50 k
lines, on the loop that frames the echo. A row that scrolled into history never changes within
its numbering, so each is now formatted once and scanned once per needle; a refresh formats and
scans the rows written since and the screen. Rows the terminal evicts leave the count, and a
new numbering (a reflow, the alternate screen) starts over
(`a_search_after_output_formats_and_scans_only_the_new_rows`, `evicted_rows_and_a_reflow_leave_the_search`).

A resize waited for ptyd's reply while the worker's one connection to ptyd, and so every
session's output taps and checkpoints, queued behind it. ptyd no longer answers a resize, and
the worker sends it like a tap (`a_resize_and_a_checkpoint_wait_for_no_reply`). A checkpoint
used to be encoded into a buffer that grew by doubling through the megabytes; its frame is now
a small head written beside the state, and both go to the socket in one `sendmsg`.

## 2026-09-28 — motion that is no news, a palette drawn from its matches, a streamed word that builds nothing

Four changes on the frame path (`.research/code-audit-2026-09-28.md`, findings 2 and 12–15):

- **Motion is no news (12, 13).** The strip is drawn as a view of its own, and a frame of its
  motion (a spring's step, a working mark's turn, a fade) notifies that view instead of the
  workspace. The workspace's own notify is still a change: it redraws the chrome and works out
  the titles, who needs the human and which clipboard is wanted. The faces follow in the next
  frame. The titles and each tile's place are kept together (finding 3).
- **The palette (15)** works out its matches as places in its lists when the field, the path
  lines or the found files change. It draws them in a virtual `gpui::list` (the headings are
  not a row tall, so not `uniform_list`), so a frame of the plate's glide draws the lines in
  view. It used to filter, clone and draw all 300.
- **The face (14)** measures only the live row again when a live block grows. Rows are keyed
  by a hashed `RowKey`, and outputs by thread and then call, so a lookup allocates nothing.
- **A typed key (2)** waits in order for room in a full outbound queue instead of being
  dropped. The fast path is unchanged.

Headless, one test binary per arm, both from the index tree of the day. **A0** is before.
**A1** is A0 with this change's `crates/slopty-ui/src`. Test profile, mac-studio, load
average 18–42 from other sessions. The arms ran in turn, three rounds. p50 / p95 in ms, one
cell per round. Round 2 fell on a load spike: every arm moved together.

| probe | A0 | A1 |
| --- | --- | --- |
| a frame of motion (the overview opening and closing), 60 shells + 60 notes, docked navigator, 400 frames | 2.46 / 7.61, 2.20 / 2.31, 2.70 / 5.49 | 0.57 / 0.60, 0.88 / 1.18, 0.59 / 0.69 |
| chrome renders in those 400 frames (navigator / title bar / status bar), and the moves they took | 405 / 405 / 405 over 5, 3, 5 moves | 1 / 1 / 2 over 1 move, 2 / 2 / 4 over 2, 1 / 1 / 2 over 1 |
| a frame of the palette's plate glide, 300 lines | 4.32 / 11.72, 3.86 / 4.40, 4.17 / 4.65 | 0.38 / 0.43, 0.63 / 0.91, 0.40 / 0.46 |
| the face, (h) a word a frame on the tail, 80 turns | 1.36 / 1.72, 1.27 / 1.55, 1.25 / 1.51 | 1.13 / 1.45, 1.97 / 2.49, 1.19 / 1.46 |
| the face, (i) panning ±40 points twice a word | 1.06 / 2.69, 1.66 / 4.52, 0.97 / 2.62 | 0.90 / 2.39, 1.37 / 4.68, 0.95 / 2.54 |
| a key into the outbound queue, room left (ns) | 29.1, 43.8, 29.1 | 30.8, 33.5, 32.7 |

- **A frame of motion is 4× cheaper.** Every such frame used to redraw the three chrome
  regions and derive every title. Now the chrome draws once for each change that starts a
  move, and the status bar once more when the tiles on screen change.
- **The glide frame is 10× cheaper.** Rows drawn per glide frame fell from 300 to 28
  (`a_glide_frame_draws_the_lines_in_view_and_filters_nothing`).
- **A streamed word builds no row** (`a_streamed_word_builds_no_row`: three words, zero
  rebuilds, three growths). The face's frame in this probe moves by 0.1–0.2 ms at p50, because
  the list already drew only the rows in view.
- **The key's fast path holds at about 30 ns.**

The smooth probe (`cargo xtask e2e smooth`) was not run for this change. It needs two app
builds and a window on the shared screen, and the load average was 30–40 from other sessions.

```sh
# one test binary per arm: the index tree exported (git checkout-index -a --prefix=target/ab/ui0/),
# the measurement tests added to it, built; then this change's crates/slopty-ui/src over it, built
CARGO_TARGET_DIR=$PWD/target cargo test -p slopty-ui -p workspace-hack --lib --no-run  # in target/ab/ui0
for r in 1 2 3; do for a in ui0 ui1; do for t in measure_a_frame_of_motion_beside_the_chrome \
  measure_a_palette_glide_frame measure_a_face_frame_while_an_answer_streams \
  measure_a_key_into_the_outbound_queue; do
  (cd target/ab/ui0/crates/slopty-ui && ../../../bin-$a $t --ignored --nocapture); done; done; done
```

Log: `target/logs/ui-ab.log`.

## 2026-09-28 — repaired loss holds the rate: capture and input to the glass, before and after

The same measurement as "capture to the glass and input to the glass, from drawn pictures", run
before and after the rate controller stopped reading loss that parity repairs as overuse
(`docs/decisions/video.md`, "Repaired loss holds the rate"). Each case is one binary built from
the tree and copied off the repository volume, run three times, 20 s after a 1 s warm-up, with
the loopback case and the tailnet-shaped case (5 ms each way, 3 % i.i.d. datagram loss) in each
run. The rate controller gets no path sample here, so no congestion window caps it.

```sh
cargo test -p slopty-worker --lib --no-run      # target/debug/deps/slopty_worker-<hash>
mkdir -p /tmp/slopty-glass/after && cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-glass/after/slopty_worker
cd /tmp/slopty-glass/after && nice -n 10 ./slopty_worker --ignored --exact \
  screen::synthetic::tests::capture_and_input_to_glass --nocapture
```

"Before" is the binary built just ahead of the change, rerun after it at the same load as the
"after" runs: load average 10 to 14 before, 9 to 19 after. A first set of before-runs at load 42
to 50 read the same on the shaped link, input sent → painted at 57–60 ms p50. Each cell is
p50 / p95 / max in ms for runs 1, 2 and 3.

| loopback | before | after |
| --- | --- | --- |
| capture → painted | 11.9 / 15.2 / 87.5, 12.3 / 23.6 / 36.9, 12.4 / 15.1 / 28.0 | 12.7 / 15.1 / 26.8, 11.5 / 15.0 / 24.4, 11.4 / 14.7 / 22.6 |
| input sent → painted | 21.5 / 37.6 / 58.6, 22.5 / 39.2 / 55.3, 21.6 / 36.9 / 40.3 | 21.9 / 37.3 / 40.2, 21.3 / 37.3 / 43.3, 21.3 / 37.7 / 51.9 |
| input at the worker → the capture showing it | 8.0 / 22.5 / 27.8, 8.0 / 24.9 / 26.8, 8.5 / 23.2 / 28.8 | 8.3 / 22.9 / 27.1, 7.5 / 23.9 / 26.3, 8.3 / 25.2 / 29.3 |
| frames encoded / s | 59.6, 60.0, 60.0 | 60.0, 60.0, 60.0 |
| rate at the end | 30.0 Mbit/s (the ceiling) in each | 30.0 Mbit/s in each |

| tailnet-shaped: 5 ms each way, 3 % loss | before | after |
| --- | --- | --- |
| capture → painted | 25.1 / 28.6 / 120.5, 25.9 / 28.1 / 39.4, 25.8 / 27.9 / 39.8 | 25.3 / 28.3 / 40.7, 25.1 / 28.3 / 36.5, 25.1 / 27.8 / 39.8 |
| **input sent → painted** | 54.4 / 96.2 / 147.1, 54.1 / 95.5 / 100.6, 56.1 / 97.1 / 104.5 | **40.1 / 55.9 / 65.2, 40.3 / 53.9 / 57.8, 39.8 / 55.6 / 60.0** |
| **input at the worker → the capture showing it** | 22.0 / 63.0 / 68.2, 21.4 / 61.9 / 70.2, 24.1 / 63.9 / 68.7 | **8.4 / 23.2 / 28.6, 8.4 / 21.5 / 26.7, 8.2 / 24.0 / 27.6** |
| frames encoded / s | 24.5, 26.4, 25.0 | 60.0, 60.0, 60.0 |
| rate at the end | 1.0 Mbit/s (the floor) in each | 15.2, 15.2, 21.6 Mbit/s |
| frames lost / repaired by parity or NACK / NACKs | 0 / 57, 54, 55 / 3, 2, 5 | 0 / 94, 99, 87 / 10, 7, 8 |

Before, the first decision of every shaped run was a cut (`9.0(Cut)`), and the stream reached
the 1 Mbit/s floor within about 8 s and stayed there, cutting again every cooldown. After, no
shaped run cut once. The verdicts were `Steady` from the 12 Mbit/s start, with a `Grow` in the
odd window that read under 0.5 % loss (two to five per run; why a window of 3 % i.i.d. loss
reads that low was not looked into). No frame was lost in any run, before or after. Parity and
NACK repaired every frame the loss hit, and at 60 fps there are more frames to hit.

The hop that moved is input at the worker → the capture showing it: 21–24 ms p50 before,
8.2–8.4 after, which is the loopback figure. The cadence stays at 60, so a click waits for the
next beat the gate lets through and not for one of every three. Input sent → painted falls by
the same 14–16 ms at the median and by 40 ms at p95. What is left over loopback, about 18 ms, is
the 10 ms round trip plus the paint timer's phase, which a run holds at 10 ms on the shaped
link against 3–4 on loopback. Capture → painted did not move, because the cut never touched
the encode: it only thinned the cadence. Loopback is within run-to-run noise of before, and its
controller path is the same one (clean windows grow).

What this leaves out: the shaped link has no QUIC, and so no congestion controller of its own.
Over a real connection with 3 % random loss, BBRv3 lowers its long-term inflight bound when a
probing round loses more than 2 %, and the controller's cap at 90 % of `cwnd × 8 / rtt` then
bounds the target however the policy reads the loss. How far that holds a stream down on such
a path needs the same run over QUIC.

## 2026-09-28 — 4:4:4 HEVC on the low-latency encoder: what VideoToolbox offers and what it costs

Mac Studio M1 Max, macOS 27.0 (26A428), SDK MacOSX27.0, load average 7–9. The probe is
`crates/slopty-codec/tests/chroma444.rs`: three ignored tests that open no window and capture
nothing. They ask the framework (`VTCopyVideoEncoderList`, `VTCopySupportedPropertyDictionaryForEncoder`),
open sessions fed synthetic pictures, parse the SPS each one writes, decode it with
`RequireHardwareAcceleratedVideoDecoder`, and time and score a synthetic code-editor picture:
8×16 cells of one-pixel-stroke glyphs in the Monokai palette on its dark ground, scrolled a
line (16 px) a frame over six pictures. Sessions are set up like the worker's (real time, no
reordering, infinite GOP, LTR asked for), one frame in flight, pts at 60 fps.

```sh
cargo test -p slopty-codec --test chroma444 --no-run   # target/debug/deps/chroma444-<hash>
cp target/debug/deps/chroma444-<hash> /tmp/chroma444 && cd /tmp
nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 chroma_444_capabilities
nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 chroma_444_encode_time
nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 chroma_444_rate_quality
```

**What the framework lists.** Two HEVC encoders: `com.apple.videotoolbox.videoencoder.ave.hevc`
(hardware) and `…hevc.vcp` (software, profiles Main, Main10, MainStill and Monochrome only).
Asking for `EnableLowLatencyRateControl` + `RequireHardwareAcceleratedVideoEncoder` selects a
third, `…videoencoder.hevc.rtvc`, absent from the list. Its `ProfileLevel` supported values are
`HEVC_Main_AutoLevel`, `HEVC_Main10_AutoLevel`, `HEVC_MainStill_AutoLevel`,
`HEVC_Main444_AutoLevel`, `HEVC_Main42210_AutoLevel`, `HEVC_Main44410_AutoLevel`,
`HEVC_Monochrome_AutoLevel` and `HEVC_Monochrome10_AutoLevel`. The SDK's
`VTCompressionProperties.h` exports only Main, Main10, Main42210 and the two Monochromes. The
low-latency encoder lists `EnableLTR`. The plain hardware encoder does not, and refuses it
(`-12900`). `VTIsHardwareDecodeSupported(HEVC)` is true.

**What the sessions write** (SPS `profile_idc`, `chroma_format_idc`, bit depth; `ltr` is
`EnableLTR`'s status):

| encoder | profile | fed | SPS | LTR |
| --- | --- | --- | --- | --- |
| low-latency | Main (shipped) | `420f` | 1, 4:2:0, 8 | 0 |
| low-latency | none, Main, Main42210 | any, `444f` and `xf44` included | 1, 4:2:0, 8 | 0 |
| low-latency | Main444 (from its list) | `444f` | **4 (RExt), 4:4:4, 8** | 0 |
| low-latency | Main444 | `420f`, `xf22`, `xf44`, `BGRA` | 1, 4:2:0, 8 (silently) | 0 |
| low-latency | Main44410 (from its list) | `xf44`, `420f`, `xf22`, `BGRA` | **4, 4:4:4, 10** | 0 |
| low-latency | Main44410 | `444f` | 4, 4:4:4, 8 | 0 |
| hardware, no low-latency | none | `444f` / `xf44` | 4, 4:4:4, 8 / 10 | −12900 |
| hardware, no low-latency | Main444 / Main44410 / Main42210 | any | 4:4:4 8 / 4:4:4 10 / 4:2:2 10 | −12900 |

The low-latency encoder's Main42210 is accepted (status 0) and ignored: it writes Main 4:2:0.
Its Main444 writes 4:4:4 only when fed `444f`; anything else comes out 4:2:0 with no error.
Every 4:4:4 and 4:2:2 stream decodes with the hardware decoder required
(`UsingHardwareAcceleratedVideoDecoder` true) straight to `444f` / `xf44` / `xf22`.

**Encode time**, submit → callback, 180 frames, one in flight (p50 / p95 / max, ms; the
spent rate is what the rate controller used of the target on this picture):

| mode | 1920×1080 at 16 Mbit/s | spent | 5120×2880 at 60 Mbit/s | spent |
| --- | --- | --- | --- | --- |
| low-latency Main 4:2:0 (shipped) | 7.72 / 8.01 / 8.71 | 6.9 | 35.29 / 47.66 / 127.2 | 25.3 |
| low-latency Main444, `444f` | 7.88 / 8.93 / 29.3 | 11.4 | 35.79 / 46.73 / 113.1 | 42.6 |
| low-latency Main44410, `xf44` | 7.73 / 8.14 / 9.86 | 10.8 | 35.88 / 47.65 / 91.7 | 36.2 |
| low-latency Main44410, `BGRA` | 8.01 / 8.50 / 12.85 | 10.7 | 41.55 / 43.05 / 72.7 | 36.7 |
| low-latency Main444, `BGRA` (writes 4:2:0) | 9.56 / 11.78 / 25.9 | 6.7 | 47.03 / 53.85 / 80.6 | 21.8 |
| hardware Main 4:2:0, no low-latency | 9.66 / 10.22 / 11.27 | 7.8 | 18.44 / 30.87 / 65.5 | 29.5 |
| hardware Main444, no low-latency | 10.21 / 13.92 / 21.36 | 12.5 | 18.85 / 29.30 / 67.9 | 41.5 |

An earlier run at lower load read the same p50s (1080p 7.64–7.76 ms for every low-latency mode,
5K 35.1–35.4 ms) with 5K maxima of 36–40 ms; the 5K tails above are load. 4:4:4 costs the
low-latency encoder nothing in time at either size. `BGRA` costs a conversion inside the
session: 0.3 ms at 1080p and 6 ms at 5K. Separate finding: the low-latency encoder takes
35 ms a 5K frame on this chip, about 28 fps, where the plain hardware encoder takes 18.

**Rate against quality**, 1080p, 90 frames, PSNR of the last decoded frame against the 4:4:4
source (luma, and Cb and Cr together at full resolution; a 4:2:0 frame's chroma is replicated
over its 2×2 block). "Source" is the picture in the input format before encoding: box-filtered
4:2:0 already loses the chroma to 29.05 dB.

| mode | source | 4 Mbit/s target | 8 | 16 | 32 |
| --- | --- | --- | --- | --- | --- |
| low-latency 4:2:0 (shipped) | 57.33 / 29.05 | 3.45 → 44.48 / 28.48 | 7.27 → 52.93 / 28.88 | 6.94 → 53.22 / 28.89 | 6.85 → 53.80 / 28.89 |
| low-latency Main444, `444f` | 57.33 / 62.56 | 3.34 → 37.95 / 35.57 | 6.92 → 49.36 / 44.65 | 11.63 → 53.14 / 51.42 | 11.17 → 53.84 / 53.12 |
| low-latency Main44410, `xf44` | 67.95 / 69.97 | 3.34 → 35.14 / 35.13 | 6.92 → 47.47 / 43.95 | 11.12 → 55.29 / 52.77 | 10.63 → 56.42 / 54.95 |
| low-latency Main44410, `BGRA` | — | 3.41 → 35.32 / 35.37 | 7.00 → 46.84 / 43.88 | 11.11 → 51.83 / 51.42 | 10.55 → 52.20 / 52.94 |
| hardware Main444, no low-latency | 57.33 / 62.56 | 2.98 → 34.27 / 36.44 | 5.86 → 40.26 / 41.04 | 13.80 → 50.64 / 51.25 | 22.06 → 55.90 / 58.58 |

Cells are spent Mbit/s → luma / chroma dB. The `BGRA` rows are scored against the BT.709
reference and the session's own RGB → YCbCr conversion, so part of their luma gap is the
conversion, not the codec. Read at equal luma: the shipped 4:2:0 stream saturates at 6.9 Mbit/s
with 53.2 dB luma and 28.9 dB chroma; the 10-bit 4:4:4 stream reaches 55.3 dB luma and 52.8 dB
chroma at 11.1 Mbit/s, about 1.6× the bits for 24 dB more chroma. Below the 4:2:0 saturation
rate 4:4:4 loses: at 3.4 Mbit/s it is 35 dB luma against 44.5, at 7 Mbit/s 47.5 against 52.9.
At no rate does 4:2:0 get its chroma above 29 dB, the subsampling ceiling.

## 2026-09-28 — full chroma on the wire: a chroma switch, rebuild to picture

Mac Studio M1 Max, macOS 27.0. `a_full_chroma_stream_arrives_as_444_and_follows_the_rate` in
`crates/slopty-worker/src/screen/synthetic.rs` streams drawn pictures (no capture) at a quarter
of the first display, 480×270 here, through the real VideoToolbox encoder, packetizer,
reassembler and decoder on loopback. It times each chroma switch from the geometry tick that
starts the rebuild to the first decoded picture in the new format. That span covers the
session build, the capture's format change, the keyframe and the decoder's rebuild on the new
SPS.

```sh
cargo test -p slopty-worker --lib --no-run   # target/debug/deps/slopty_worker-<hash>
cp target/debug/deps/slopty_worker-<hash> /tmp/worker && cd /tmp
nice -n 10 ./worker --exact screen::synthetic::tests::a_full_chroma_stream_arrives_as_444_and_follows_the_rate --nocapture
```

| run | 4:4:4 → 4:2:0 | 4:2:0 → 4:4:4 |
| --- | --- | --- |
| 1 | 54.7 ms | 43.1 ms |
| 2 | 45.4 | 42.4 |
| 3 | 38.4 | 31.5 |
| 4 | 97.8 | 35.6 |
| 5 | 47.5 | 42.6 |

A switch costs 30–100 ms of the old picture held, and a keyframe, once. The hold in
`ChromaGate` keeps that to about one switch every half a minute on a link that sits at the
line. Nothing was measured at 1080p or on a real link.

## 2026-09-28 — an idle stream's wakeups: the cursor loop and the geometry probe

Mac Studio M1 Max, macOS 27.0, debug build. Each stream ran a cursor loop that woke every
8.3 ms and a geometry probe every 100 ms, whether anything moved or not. Now a window stream's
cursor loop waits on its input's pointer (`PointerWatch::changes`), the target's bounds, its
visibility and the zoom. A stream with nothing to follow (its source told idle, not a window on
the crop path, nothing under way) is probed when the accessibility API says the target moved,
was resized or went, when the first frame after idle is encoded, or after a 1 s backstop. The
counts are loop rounds over ten seconds with nobody touching anything:

```sh
cargo test -p slopty-worker --lib -- --ignored idle_cursor_wakes --nocapture
cargo test -p slopty-workerd --bin slopty-worker -- --ignored idle_stream_probes --nocapture
```

| loop | before | after |
| --- | --- | --- |
| cursor, window stream (placed pointer) | 118.3/s | **1.0/s** (the backstop) |
| cursor, display stream (real pointer) | 118.3/s | 118.3/s |
| geometry probe, idle display or covered window | 10/s | **1.0/s** (the backstop) |
| geometry probe, window on the crop path, or a source that draws | 10/s | 10/s |
| tailnet status reads, no client on the tailnet | 0 reads, 0.5 wakes/s | 0 reads, 0 wakes |

The "before" cursor figure is measured: it is the real-pointer loop, which is still the one
every stream used to run. The "before" probe figure is the old `interval(GEOMETRY_PERIOD)`.
`a_quiet_stream_is_probed_when_woken_not_every_period` checks both sides of it: two probes at
most in 1.5 s against the period's fifteen, and one probe within 50 ms of a wake. A display
stream still reads the pointer's move counters every period, because nothing announces the
worker's own user moving it. A window on the crop path is still probed every period, because
another application's window moving over it announces nothing to this worker.

Not measured:
- The worker process's wakeups with a real capture open. That needs a launchd worker with
  Screen Recording (TCC per build), and installing one replaces the host the user runs.
- How closely the crop follows a drag now that each accessibility move wakes the probe. The
  drag would have to be synthetic input.
- The frame-path cost of the idle check in `Shared::on_packet`. It is one relaxed load of a
  flag per encoded frame, against an encode of about 7 ms.

## 2026-09-29 — allocation budgets and retired-instruction budgets

Mac Studio M1 Max, macOS 27.0. Other sessions were building in the same checkout throughout.

### Allocations on the hot paths

Counted on the test's own thread by `slopty_testkit::alloc::Counting`, in steady state (buffers
warmed first). The counts are exact and repeat run to run:

```sh
cargo nextest run -p slopty-media -p slopty-engine -p slopty-grid --test allocs --no-capture
```

| path | before | after |
| --- | --- | --- |
| packetize a 62 KB P-frame, parity 200 ‰ | 5 blocks, 81 576 B | **3 blocks, 78 184 B** |
| packetize a 300 KB keyframe, parity 200 ‰ | 5 blocks, 391 751 B | **3 blocks, 375 495 B** |
| packetize, no parity (62 KB / 300 KB) | 3 blocks, 64 753 / 312 714 B | 3 blocks, same |
| reassemble 64 P-frames of 62 KB | 204 blocks, 4 086 400 B | same (3 a frame) |
| a key's byte into the engine | 0 | 0 |
| its diff frame, 80×24 / 200×60 | 4 blocks, 9 536 / 23 072 B | same |
| an Enter at a bottom prompt, 80×24 | 30 blocks, 109 376 B | **10 blocks, 32 576 B** |
| an Enter at a bottom prompt, 200×60 | 66 blocks, 618 272 B | **10 blocks, 80 672 B** |
| the echo frame encoded (228 B on the wire) | 8 blocks, 532 B | 8 blocks (see below) |
| a row applied on the client (screen and scrollback) | 1 block, 72 B | 1 block |
| 1 000 lines into a full 10 000-line history | 6 166 blocks, 1 450 080 B | **1 166 blocks, 106 080 B** |
| a P-frame or an echo to 8 viewers against 1 | equal | equal |

- The packetizer's list of datagrams was sized for the data and grew for the parity, and the
  parity was first collected into a list of its own. It is now sized for both.
- On a scroll, libghostty dirties every row and the engine reads each one again. Every row went
  into a new line, dropped at once when it matched what the viewers held. The row is now read
  into the allocation of the last one that matched.
- The client's scrollback cut its index with `split_off` each time the worker's oldest line
  moved, which is every frame of a flood at the history's limit. It now pops the few lines
  below the new oldest one.
- The echo's 8 blocks are `slopty_proto::codec::encode` growing a `Vec` from its 4-byte prefix
  (4, 8, 16, … 256 bytes). Sizing it first would make that 1; that is `slopty-proto`'s change.

What the fixes cost or saved in instructions, `cargo xtask bench --filter <name>` with the
change reverted and then restored:

| series | before | after |
| --- | --- | --- |
| `engine.scroll_frame_cost.80x24` | 914 438 | 894 562 (−2.2 %) |
| `engine.scroll_frame_cost.200x60` | 5 430 577 | 5 315 576 (−2.1 %) |
| `media.packetize_cost.p_frame.parity_200` | 211 956 | 210 037 (−0.9 %) |
| `media.packetize_cost.keyframe.parity_200` | 1 242 086 | 1 238 330 (−0.3 %) |

The no-parity series, whose code did not change, moved by 0.5-0.6 % between those builds, so
the packetizer's difference is within the noise of a rebuild. The scroll's is not.

### Instructions per operation, two runs

`cargo xtask bench --update-budgets` recorded `xtask/budgets.toml`, then `cargo xtask bench`
ran again at once. Instructions are the median per operation, from `ri_instructions` less the
cost of reading it (calibrated on 201 empty samples). Wall times are from the second run and are
for reading, not for passing:

| series | run 1 | run 2 | Δ | wall p50 | p99 |
| --- | --- | --- | --- | --- | --- |
| `codec.render_cost.same_thread` | 9 317 | 9 242 | −0.8 % | 542 ns | 1 208 ns |
| `codec.access_unit_conversion_cost.keyframe` | 1 008 616 | 1 008 638 | 0.0 % | 54 us | 71 us |
| `codec.packet_conversion_cost.p_frame` | 14 237 | 14 312 | +0.5 % | 1 250 ns | 1 708 ns |
| `codec.packet_conversion_cost.keyframe` | 58 883 | 58 908 | 0.0 % | 5 250 ns | 6 541 ns |
| `engine.frame_cost.write` | 861 | 886 | +2.9 % | 125 ns | 292 ns |
| `engine.frame_cost.take_frame` | 38 209 | 38 184 | −0.1 % | 2 500 ns | 24 us |
| `engine.scroll_frame_cost.80x24` | 894 562 | 895 166 | +0.1 % | 55 us | 58 us |
| `engine.scroll_frame_cost.200x60` | 5 315 576 | 5 317 539 | 0.0 % | 328 us | 546 us |
| `engine.osc_scan_cost.per_osc` | 172 | 171 | −0.6 % | 19 ns | 38 ns |
| `engine.fetch_lines_cost.4096_rows` | 124 790 604 | 124 825 381 | 0.0 % | 8 663 us | 9 775 us |
| `engine.search_after_output_cost.plain` | 1 877 298 | 1 843 946 | −1.8 % | 135 us | 230 us |
| `engine.search_cost.50000_lines.plain` | 101 218 349 | 101 110 559 | −0.1 % | 6 351 us | 17 ms |
| `engine.checkpoint_cost.format` | 45 649 442 | 45 342 517 | −0.7 % | 2 896 us | (one sample) |
| `engine.checkpoint_cost.fill_engine` | 32 850 996 | 32 295 928 | −1.7 % | 2 558 us | (one sample) |
| `media.packetize_cost.p_frame.parity_200` | 210 037 | 210 104 | 0.0 % | 15 us | 33 us |
| `media.packetize_cost.keyframe.parity_200` | 1 238 330 | 1 238 553 | 0.0 % | 88 us | 131 us |
| `media.reassemble_cost.p_frame` | 46 778 | 46 763 | 0.0 % | 4 125 ns | 10 us |
| `media.reassemble_cost.keyframe` | 209 982 | 210 029 | 0.0 % | 18 us | 33 us |

All 31 budgeted series held within 5 % on the second run; most moved by 0.1 % or less. The
wall times of the same runs moved by up to 40× at p95 (`engine.search_cost.50000_lines.format`
went from 432 ms to 11 ms at p95 between the runs), which is why they are not the budget.
Printed by the table at the end of `cargo xtask bench`; the full run is in `xtask/budgets.toml`.

**2026-10-01, the budgets rerecorded.** A run beside a gate put `media.reassemble_cost` 37 %
(p_frame, 64 132) and 24 % (keyframe, 260 102) over; the next run, on a quieter machine, 9.0 %
and 9.7 % (51 011, 230 383). The commit the budgets were recorded at (bd015ef8), rebuilt on the
same toolchain and measured with `cargo nextest run --release -p slopty-media --run-ignored only
-E 'test(reassemble_cost)'`, gave 51 075 and 230 440. The code did not get dearer; the 9 % came
with the toolchain and dependencies that moved since. The 37 % is the machine. The likeliest
cause (not yet isolated) is that `ri_instructions` includes what the kernel retires on the
process's behalf, and this series takes its 300 KB of frames in fresh pages, so its faults
would be counted with it. A series that allocates is only as
steady as the machine under it; an over-budget verdict on one is rerun before it is believed.

## 2026-09-29 — the transport on a simulated network: handshake, outages, rebinding, a flood

`crates/slopty-net/tests/sim.rs` runs a real client and worker (the shipped endpoint, transport
config, listener and dial) over `slopty_shape::sim`, the in-memory network, on tokio's paused
clock (docs/decisions/transport.md, "The transport is tested on a simulated network"). Times
are simulated, so they do not move with the machine's load. They land on whole milliseconds,
tokio's timer quantum. Real time is what the run cost on this Mac, with other sessions building
beside it (load average above 100).

```sh
cargo test -p slopty-net --test sim                                  # the gate: 8 seeds + 2 pinned
cargo test -p slopty-net --test sim -- --ignored --nocapture         # 200 seeds of each, prints this table
```

200 seeds of each scenario. "Hop" is 5 ms each way.

| scenario | link | measured | p50 | p99 | max | real |
| --- | --- | --- | --- | --- | --- | --- |
| dial | hop, 20 % lost each way | dial to `HelloAck` | 50 ms | 1.008 s | 1.866 s | 0.09 s |
| 30 s outage | hop, 1 % | path back to last echo | 138 ms | 1.673 s | 1.673 s | 1.4 s |
| 60 s outage | hop, 1 % | outage start to both ends closed | 45.515 s | 45.546 s | 45.546 s | 0.7 s |
| dial after it | hop, 1 % | dial to `HelloAck` | 20 ms | 50 ms | 50 ms | (with the row above) |
| NAT rebinding | hop, 1 % | slowest key's echo | 34 ms | 80 ms | 80 ms | 3.9 s |
| flood | 2 Mbit/s, 100 ms queue, 10 + 0–2 ms, 5 % | each seed's echo p50 | 49 ms | 59 ms | 60 ms | 22.6 s |
| flood | same | each seed's echo p99 | 156 ms | 238 ms | 273 ms | (same run) |

- *Dial:* at a fifth lost each way, every seed now connects inside the client's 2 s handshake
  timeout. Before the client waited for the worker's FINISHED, 29 of the 200 seeds failed at
  2 s with "no answer". The 171 that connected had p50 40 ms, p90 265 ms, p99 1.482 s and max
  1.866 s, but the before and after are not the same draws, since noq's own generator was not
  yet seeded. A dial on a clean hop is two round trips (20 ms), as before.
- *30 s outage:* 100 keys 20 ms apart, with the outage starting 0.5 s in. Every key reaches the
  worker once and in order, on the connection it was typed on. The last echo comes within
  noq's 2 s cap on its probe interval of the path's return.
- *60 s outage:* both ends end with `TimedOut` 45.5 s after the path goes dark: the 45 s idle
  timeout from the last packet heard. The next dial after the outage takes one or two round trips.
- *NAT rebinding:* the client moves 1 s into 2 s of typing. Before noq-proto patch 10, seed 10,
  whose first PATH_CHALLENGE is lost, never recovered: the worker sent nothing but PINGs until
  the sweep's 600 s guard. Now it falls back and migrates again, and its slowest key takes
  under 80 ms like the rest.
- *Flood:* a session writes a 3,000-byte frame every 8 ms (375 kB/s, 1.5 times the link)
  beside 200 keys 30 ms apart, echoed on a lifted session stream. Every key and echo arrives
  once and in order.
- *Repeatability:* two runs of one seed deliver the same packets at the same simulated times
  (`a_seed_repeats_its_run`, seeds 3 and 11, all four timed scenarios). Before BBR3's probes
  were seeded, the flood's echo p50/p99 for seed 3 came out 46/139 ms in one run and 44/124 ms
  in the next.
- The gate's six tests take 0.6 s of real time together.

## 2026-09-29 — the first soak: ptyd keeps a descriptor and ~145 KiB per closed session

Mac Studio M1 Max, macOS 27.0, release daemons copied and signed ad hoc for `leaks`, other
sessions building beside it. `cargo xtask soak` (60 s of load after two warm-up cycles and a
12 s settle, a sample every 2 s): 156 cycles of open a quiet bash, `seq 1 20000`, two
`slopty hook report` calls, `output --max 200`, close. A cycle took 382 ms at p50 and 440 ms at
most.

```sh
cargo xtask soak            # samples, logs, leaks reports: target/deep/soak/last/
```

| daemon | footprint, baseline → end | slope over the load | peak | descriptors | threads | `leaks` |
| --- | --- | --- | --- | --- | --- | --- |
| server | 4 304 → 4 896 KiB | 173 KiB/min | 4 MiB | 11 → 11 | 11 → 11 | 0 |
| ptyd | 2 720 → 25 200 KiB | 21 632 KiB/min | 24 MiB | **13 → 169** | 1 → 1 | 0 |
| worker | 8 000 → 9 248 KiB | 1 096 KiB/min | 28 MiB | 14 → 14 | 14 → 13 | 0 |

- **ptyd** keeps one descriptor for every session closed: 156 cycles, 156 more descriptors,
  still open after the settle. Its footprint grows by about 145 KiB a session in a straight line
  (a sample every 6 s: 4 688, 6 960, 9 216, 11 328, 13 648 KiB …). `leaks` finds nothing
  unreferenced (6 658 blocks, 46 830 KB malloced), so the sessions are still held, not lost.
  The soak fails on it, as it should.
- **worker** grows about 8 KiB a session, steadily, with no descriptor or thread left behind.
  A minute cannot tell a cache still filling from a record kept per session. The nightly's
  20-minute soak can.
- **server** grows by less each sample (+96, +64, +48, then +16 KiB every 6 s), which looks
  like a cache settling. It is still over the 64 KiB/min budget in a 60 s run.
- An earlier run stopped at one `slopty hook report` that failed with "Socket is not connected
  (os error 57)". `slopty_platform::service::ask` shuts down its write half after sending, and
  on macOS `shutdown` fails with `ENOTCONN` once the worker has answered and closed first. The
  reply is still there to read. The soak now counts failed hook reports as a failure and goes
  on; none failed in the run above.

## 2026-09-29 — ptyd gives back a closed session; the soak again

The first soak's ptyd growth had one cause. A session closed while a worker held its master
kept its reader task: the reader was parked waiting for its pause flag to change, the flag's
sender lived in the session itself, so nothing ever woke it, and the task held the session.
That kept the master's descriptor, the 64 KiB read buffer and the ring (the ~145 KiB a
session). A close now stops the reader outright (`Reader::Stop` in
`apps/slopty-ptyd/src/session.rs`); once the child is reaped nothing holds the session.
`roundtrip::a_closed_session_gives_back_its_descriptor` opens and closes 16 sessions, half of
them attached, and reads ptyd's descriptors through `proc_pidinfo`: 12 → 20 before the fix
(one per attached close), 12 → 12 after.

```sh
cargo nextest run -p slopty-ptyd a_closed_session
cargo xtask soak --out target/deep/soak/ptyd-fix
```

| daemon | footprint, baseline → end | slope over the load | peak | descriptors | threads | `leaks` |
| --- | --- | --- | --- | --- | --- | --- |
| server | 4 400 → 4 928 KiB | 228 KiB/min | 4 MiB | 11 → 11 | 11 → 11 | 0 |
| ptyd | 2 576 → 3 376 KiB | −159 KiB/min | 3 MiB | 11 → 11 | 1 → 1 | 0 |
| worker | 8 176 → 8 512 KiB | 939 KiB/min | 27 MiB | 14 → 14 | 14 → 14 | 0 |

158 cycles; a cycle took 380 ms at p50 and 435 ms at most. ptyd was 13 → 169 descriptors and
2 720 → 25 200 KiB (21 632 KiB/min) in the first soak. No hook report failed:
`slopty_platform::service::ask` now reads the answer when the daemon answered and closed before
the asker's write or shutdown (`EPIPE`, `ENOTCONN`), which
`service::tests::an_answer_from_a_peer_that_already_closed_is_read` plays deterministically on a
socket pair.

The worker's growth, about 6 KiB a cycle over the load (8 KiB in the first soak), is not the
same class. Read from the code, not yet measured apart: nothing keyed by session outlives the
close verb, and two capped stores are still filling at 158 cycles: the
idempotency ledger (4 keyed verbs a cycle, up to 4 096 keys, and lapsed keys are swept only
once it is full) and the 1 024-message event broadcast, whose payloads the never-read `_keep`
receiver pins until the ring wraps. Both should level off, the ledger at an estimated
2.5–3 MiB; together they account for an estimated 3 KiB of a cycle. The server's
slope still reads like a cache settling. The soak holds both to its budget, so it still fails on
them.

## 2026-09-29 — Encoding a frame

Mac Studio M1 Max, macOS 27.0, other sessions building in the same checkout.
`slopty_proto::codec::encode` grew a fresh `Vec` from its 4-byte prefix as postcard wrote, and
handed it to `Bytes` with spare capacity, which shares it through a block of its own. It now
serialises into a scratch buffer the thread keeps (at most 64 KiB) and copies the frame out at its
exact size. A frame too big for that buffer leaves as the buffer it grew, uncopied.

```sh
cargo nextest run -p slopty-engine --test allocs --no-capture
cargo xtask bench --filter encode_cost
```

Allocations, on the test's own thread with its buffers warmed:

| path | before | after |
| --- | --- | --- |
| the echo frame encoded, 80×24 (228 B on the wire) | 8 blocks, 532 B | **1 block, 228 B** |
| the echo frame encoded, 200×60 (230 B) | 8 blocks, 532 B | **1 block, 230 B** |
| an echo's take, encode and hand-off, 1 viewer or 8 | 12 blocks, 10 068 B | **6 blocks, 9 788 B** |

Retired instructions per encode, the old `encode` restored for the before column:

| series | before | after |
| --- | --- | --- |
| `engine.encode_cost.echo` | 6 747 | 4 148 (−38.5 %) |
| `engine.encode_cost.full_200x60` (28 133 B) | 1 095 518 | 1 084 187 (−1.0 %) |

Sizing the message first, with postcard's `Size` flavour, and then serialising it into an exact
`Vec` also makes one block, but it walks the message twice. On a one-off harness with the same
frames it cost 6 768 instructions for the echo and 1 729 833 for the full screen (+58 %), so it
was not taken.

## 2026-09-29 — Keeping a session's screen on disk

Mac Studio M1 Max, macOS 27.0, other sessions building in the same checkout. The worker now
writes each session's newest checkpoint to `<data dir>/sessions/<id>.vt` so a session can come
back after its shell is lost (`docs/decisions/terminal.md`, "Sessions come back after a
reboot"). The write runs on a blocking thread of the keeper's own task. The session thread,
the frame path and the tap to ptyd do not change: the tap loop hands the state it already
sent to ptyd to the keeper, a move with no copy.

```sh
cargo nextest run -p slopty-worker --release --run-ignored only -E 'test(keep_cost)' --no-capture
```

A 200-column session holding the full 50 000-line scrollback, checkpointed, then written with
`slopty_platform::fs::replace` into a temporary directory on the internal SSD, 20 rounds, three
runs:

| state | mean | worst |
| --- | --- | --- |
| 5 957 KiB | 2.7–3.0 ms | 4.2–5.8 ms |

A session is written at most once every 10 s (`restore::KEEP_EVERY`), and the newest state
replaces one still waiting. Twenty busy sessions at full scrollback cost the writer about 60 ms
of I/O every 10 s, on no thread a key or a frame waits on.

## 2026-09-29 — 120 fps against 60

Mac Studio M1 Max, macOS 27.0, a 75 Hz panel. Other sessions were building in the same
checkout, with a load average of 8–21, which is printed per row. Two instruments open no
window and capture nothing.

**The encoder, fed in real time.** `frame_rate_120_against_60` in
`crates/slopty-codec/tests/chroma444.rs` sets the shipped session up as the worker does
(low-latency hardware HEVC Main 4:2:0, real time, no reordering, LTR) and sets
`ExpectedFrameRate`. It then submits the synthetic code-editor picture on a real-time beat
without waiting for the last frame, as the capture callback does, for 3 s. The scroll is
960 rows a second at both rates: 16 rows a frame at 60 and 8 at 120. Each row gives submit →
callback per frame, frames the encoder dropped, the rate spent after the first second, and
the mean luma PSNR of the last 10 decoded pictures against their sources (hardware decoder).

```sh
cargo test -p slopty-codec --test chroma444 --no-run      # target/debug/deps/chroma444-<hash>
cp target/debug/deps/chroma444-<hash> /tmp/chroma444 && cd /tmp
nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 --exact tests::frame_rate_120_against_60
```

Second run (the first agreed within its noise; p50 / p95 ms):

| size | fps | target | encode p50 / p95 | dropped | spent | a frame | PSNR y |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1920×1080 | 60 | 4 Mbit/s | 9.1 / 16.2 | 16/180 | 3.98 | 8.1 KiB | 47.40 |
| 1920×1080 | 120 | 4 Mbit/s | 8.1 / 17.5 | 4/360 | 3.79 | 3.9 KiB | 44.58 |
| 1920×1080 | 60 | 8 Mbit/s | 9.9 / 14.6 | 0 | 7.07 | 14.4 KiB | 52.83 |
| 1920×1080 | 120 | 8 Mbit/s | 8.0 / 9.2 | 1/360 | 7.89 | 8.0 KiB | 53.28 |
| 1920×1080 | 60 | 16 Mbit/s | 8.4 / 16.0 | 0 | 6.91 | 14.1 KiB | 53.18 |
| 1920×1080 | 120 | 16 Mbit/s | 8.1 / 12.7 | 0 | 8.12 | 8.3 KiB | 53.46 |
| 1920×1080 | 60 | 32 Mbit/s | 10.1 / 17.7 | 0 | 6.86 | 13.9 KiB | 53.73 |
| 1920×1080 | 120 | 32 Mbit/s | 8.1 / 13.0 | 0 | 8.15 | 8.3 KiB | 53.47 |
| 2560×1440 | 60 | 32 Mbit/s | 11.3 / 16.4 | 0 | 8.52 | 17.3 KiB | 53.39 |
| 2560×1440 | 120 | 32 Mbit/s | 11.2 / 19.2 | 0 | 11.30 | 11.5 KiB | 53.26 |
| 3024×1964 | 60 | 32 Mbit/s | **82.8 / 99.0** | 0 | 13.39 | 27.2 KiB | 53.33 |
| 3024×1964 | 120 | 32 Mbit/s | **77.0 / 78.3** | 0 | 17.52 | 17.8 KiB | 53.23 |
| 3840×2160 | 60 | 32 Mbit/s | 22.0 / 30.6 | 0 | 18.04 | 36.7 KiB | 53.23 |
| 3840×2160 | 120 | 32 Mbit/s | 21.8 / 28.8 | 1/360 | 21.90 | 22.3 KiB | 53.45 |

What it says:

- **120 costs 1.2–1.3× the bits of 60 for the same motion, at the same PSNR.** Above about
  8 Mbit/s the picture, not the target, sets the rate: 8.1 against 6.9 Mbit/s at 1080p, 11.3
  against 8.5 at 1440p, 21.9 against 18.0 at 4K. A frame at 120 changes half as much as one
  at 60.
- **At 8 Mbit/s, 120 is as sharp as 60.** Its 8.0 KiB frames score 53.3 dB against 60's
  14.4 KiB at 52.8. At 4 Mbit/s both drop frames, and 120's 3.9 KiB frames lose 2.8 dB. The
  cadence ladder's 8 KB bar falls between the two rows, which is where 120 should give way to
  60.
- **The encoder's latency does not grow with the rate while it keeps up**: 8 ms at 1080p and
  22 ms at 4K, at either rate.
- **When it does not keep up, frames queue.** At 3024 × 1964 every frame was 77 ms late at
  120, a steady queue with p95 78 ms. In this run 60 was 83 ms late too. In the first run,
  60 at that size was 20 ms and only 120 queued (79 ms). 3840 × 2160 has 40 % more pixels and
  held 22 ms, so the slow size is the odd one: 1964 is not a multiple of 16. This is what the
  worker's encoder watch reacts to.

**The frame path at a 120 Hz beat.** `capture_and_input_to_glass_at_120` in
`crates/slopty-worker/src/screen/synthetic.rs` is the glass measurement of 2026-09-28, with
the canvas drawing at 120 Hz (`slopty_capture::synthetic::set_beat`) and the stream asked for
60 or 120. The canvas now scrolls 180 rows a second, set in time, where it used to scroll 3
rows a beat. That makes it lighter at 75 Hz than in the rows of 2026-09-28. The wire rate
counts every datagram after the 1 s warm-up. Each case runs 20 s:

```sh
cargo test -p slopty-worker --lib --no-run      # target/debug/deps/slopty_worker-<hash>
cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-fps/slopty_worker && cd /tmp/slopty-fps
./slopty_worker --ignored --exact screen::synthetic::tests::capture_and_input_to_glass_at_120 --nocapture
```

| 1920×1080, 120 Hz beat (p50 / p95 ms) | loopback 60 | loopback 120 | shaped 60 | shaped 120 |
| --- | --- | --- | --- | --- |
| **input sent → shown** (pacer) | 26.0 / 35.3 | 24.4 / 36.0 | 38.6 / 53.7 | **28.3 / 38.7** |
| **input at the worker → the capture showing it** | 8.9 / 15.9 | **4.9 / 10.2** | 9.6 / 17.1 | **3.6 / 7.8** |
| capture → painted | 17.5 / 20.4 | 17.3 / 26.7 | 20.1 / 34.0 | 17.3 / 24.1 |
| worker encode | 7.7 / 10.6 | 8.4 / 13.3 | 7.7 / 10.3 | 7.6 / 8.6 |
| capture → decoded | 9.7 / 14.8 | 11.4 / 20.4 | 17.1 / 26.4 | 16.2 / 17.6 |
| frames encoded / s | 60.0 | 108.2 | 60.0 | 119.7 |
| on the wire | 1.40 Mbit/s | 1.87 Mbit/s | 1.40 Mbit/s | 2.00 Mbit/s |

Shaped is 5 ms each way with 3 % of datagrams lost. No frame was lost, and none failed to
decode. The shaped runs repaired 87 and 125 frames through parity. At 120 the wait for the
next frame the gate lets through halves, and that is the whole gain in input → capture. The
loopback 120 run encoded 108 frames a second because the canvas's beat skipped under load
(2376 captures in 21 s). Capture → painted is the encoder's floor plus the paint timer's
phase at either rate.

Not measured: a physical display above 60 Hz (this panel runs at 75), and a `CGVirtualDisplay`
at 120 Hz (making one is gated on `SLOPTY_VDISPLAY_E2E`). Rulings: `docs/decisions/video.md`,
"The stream follows the screen's refresh".

## 2026-09-29 — ghostty `12752b2ac`

The engine's `*_cost` series before and after moving libghostty-vt from ghostty `6301810a4` to
`12752b2ac` (fork `801866f` → `d1a57e4`), in instructions per operation. mac-studio, release,
libghostty-vt ReleaseFast, other sessions building. `--filter` matches test names, so the
engine's series take three runs:

```sh
for f in ghostty osc_scan encode_cost; do cargo xtask bench --filter $f; done
cargo xtask bench --filter checkpoint_cost    # the checkpoint series again, twice
```

| series | budget | before | after | after, reruns |
| --- | --- | --- | --- | --- |
| checkpoint `fill_engine` | 32850996 | 32800596 | 32959469 | 32488025, 32403227 |
| checkpoint `fill_raw_vt` | 27067881 | 27051037 | 27657895 | 27037350, 27059814 |
| checkpoint `format` | 45649442 | 45674146 | 46534571 | 45800235, 45268418 |
| checkpoint `replay_one_chunk` | 35249023 | 35263439 | 35712401 | 35412170, 35568184 |
| checkpoint `replay_64k_chunks` | 35270833 | 35330829 | 35672509 | 35356749, 35819244 |
| `frame_cost.take_frame` | 38209 | 37975 | 37959 | |
| `scroll_frame_cost.200x60` | 5315576 | 5317737 | 5315858 | |
| `fetch_lines_cost.4096_rows` | 124790604 | 125543551 | 124924229 | |
| `search_after_output_cost.plain` | 1877298 | 1880992 | 1840799 | |
| `search_cost.50000_lines.plain` | 101218349 | 101149605 | 101141637 | |
| `osc_scan_cost.per_osc` | 172 | 171 | 171 | |
| `encode_cost.full_200x60` | 1084187 | 1084187 | 1084187 | |

Every series held within the 5 % slack, and the eighteen search, frame, OSC and encode series
moved less than 2 %. The checkpoint series are one sample each: the first run after the bump
read 1–2 % higher, and two reruns landed on either side of the budget, so that was noise. The
budgets stay as they are. None of the 31 commits touches the parser's print path or the page
list. The OSC integer parser and the wuffs zlib decoder are off these paths.

## 2026-09-29 — Rows shared between the frame and the record

Audit finding 17: each changed row was read into a new line on the worker, then copied into the
record of what the viewers hold (`Shown`), and the client wrapped each arriving row in a new
`Arc`. `RowUpdate.line` is now an `Arc<Line>` that both sides share, and the worker reads a
changed row into the allocation of the line it replaces. mac-studio, other sessions building.

Allocation counts are exact, from the counting allocator on the test's own thread:

```sh
cargo test -p slopty-engine --test allocs -p slopty-grid --test allocs -p workspace-hack -- --nocapture
```

| path | before | after |
| --- | --- | --- |
| echo `take_frame`, 80×24 | 4 blocks, 9536 B | 1 block, 128 B |
| echo `take_frame`, 200×60 | 4 blocks, 23072 B | 1 block, 128 B |
| Enter at the bottom `take_frame`, 80×24 | 10 blocks, 32576 B | 8 blocks, 11936 B |
| Enter at the bottom `take_frame`, 200×60 | 10 blocks, 80672 B | 8 blocks, 29216 B |
| echo to one or eight viewers (take, encode, share) | 6 blocks, 9788 B | 3 blocks, 380 B |
| an echo row applied on the client | 1 block, 72 B | 0 blocks |

Instructions per operation, in release:

```sh
cargo xtask bench --filter frame_cost
```

| series | before | after |
| --- | --- | --- |
| `engine.frame_cost.take_frame` (60×12) | 37951 | 34324 (−10.1 %) |
| `engine.frame_cost.write` | 861 | 861 |
| `engine.scroll_frame_cost.80x24` | 894380 | 881550 (−1.4 %) |
| `engine.scroll_frame_cost.200x60` | 5315825 | 5301643 (−0.2 %) |

The flood (one line scrolled in per frame at 200×60, built and encoded, 500 frames:
`cargo test --release -p slopty-engine --test wire_bytes -- --nocapture`) took 348 µs before
and 350 µs after, which is within run-to-run noise. The rows are still read cell by cell, and
that read is most of the cost, so the flood moved by less than the noise. The wire bytes did
not change: 228 B for an echo, 653 B for an Enter, and 505 B per scrolled frame.
`xtask/budgets.toml` records the three cheaper series. Ruling: `docs/decisions/terminal.md`,
"A frame's rows are shared, not copied".

## 2026-09-29 — The server's growth was its event log filling, with twice the room it needs

Mac Studio M1 Max, macOS 27.0, release daemons copied and signed ad hoc for `leaks`, other
sessions building beside it. The server grew 173 KiB/min in the first soak and 228 KiB/min in
the second. That was the hub's event log filling. The log is bounded (`EVENT_LOG`, 4 096
`HubEvent`s), but it pushed each event before it dropped the oldest, so the 4 097th push
doubled the `VecDeque` to 8 192 slots of 256 B. A ring writes its way through the whole
buffer, so the footprint kept rising until 8 192 events. At four events a soak cycle (opened,
working, idle, closed) and 158 cycles a minute, that is about 2 048 cycles, or 13 minutes.
It then held about 1 MiB more than the bound calls for. Each cycle costs about 1.2 KiB (four
slots and the opened terminal's strings), which is the slope both soaks measured. The log
now drops the oldest event before it pushes and reserves its 4 096 slots up front
(`Hub::happen`, `crates/slopty-server/src/hub.rs`).

Nothing else in the server grows. A one-off harness in one process ran a real `Server`, a fake
worker link and a live-heap allocator. With the log already full, it made 600 soak-shaped
cycles of five fresh client links, each asking one forwarded verb. The heap went from 2 898
to 3 028 KiB over the first 300 cycles as noq's and tokio's tables sized themselves, then
stayed at 3 028 KiB through cycle 600. Per-link state goes when the link does: the requests'
`JoinSet`, the broadcast receiver and the pending-reply entry all leave with it.

```sh
cargo nextest run -p slopty-server --test heap --no-capture
cargo xtask soak --out target/deep/soak/server-after
```

`heap::the_hub_holds_its_event_log_and_no_more` drives a hub on the test's own thread through
2 × 1 024 cycles and then 2 × 1 024 more, counting the bytes the thread holds (allocated minus
freed). It checks the growth against the log's bound: 4 096 slots, plus the strings of the
1 024 terminals the log still holds, plus 16 KiB. The old `happen` was restored for the
"before" column.

| | hub growth over 2 048 cycles | over the next 2 048 | bound |
| --- | --- | --- | --- |
| before | 2 206 KiB | +0 B | 1 199 KiB (fails) |
| after | 159 KiB | +0 B | 1 199 KiB |

### The soak fills the bounded stores before it takes the baseline

In a store that is still filling, the footprint climbs as steadily as it does under a leak,
and a minute of load cannot tell the two apart. The soak now runs 1 536 cycles, four at a
time, between the warm-up and the baseline. The largest stores fill by cycle 1 024: the
server's log (4 096 events at four a cycle) and the worker's idempotency ledger (4 096 keys at
four keyed verbs a cycle: open, send, wait, close). The slope is then taken over the load
alone. Each daemon's line now shows its footprint before the fill, after it and at the end,
and the samples carry the fill's own curve, so what a store costs is on the record and the
peak budget still bounds it. The fill took 148 s.

Fill cycles run the full flood. When eight ran at once, the worker peaked at 173 MiB, over its
128 MiB budget, so four run at once (peak 91 MiB). A fill of one-line cycles kept the peaks
low, but it left the worker's flood path to warm up during the load, where it read as
204 KiB/min.

One soak each way, 60 s of load after the fill. The "before" column is the old `happen` under
the new soak:

| daemon | before the fill → filled → end | slope over the load | peak |
| --- | --- | --- | --- |
| server, before | 4 192 → 6 976 → 7 120 KiB | **149 KiB/min** (fails) | 6 MiB |
| server, after | 4 336 → 6 848 → 6 720 KiB | −50 KiB/min | 6 MiB |
| ptyd, after | 2 592 → 3 856 → 3 856 KiB | 0 KiB/min | 5 MiB |
| worker, after | 8 032 → 15 984 → 16 000 KiB | 0 KiB/min | 91 MiB |

The server's footprint during the fill, one sample every 6 s:

- before: 5 088, 5 200, 5 376 … 6 784, 6 864, 6 912, 6 976, and through the load 6 976, 6 992,
  7 008 … 7 104, 7 120 (+16 KiB every 6 s, still filling the doubled buffer);
- after: 5 200, 5 264, 5 408 … 6 656, 6 720, then 6 720, 6 736, 6 736, 6 736, 6 816, 6 832,
  6 848, and through the load 6 848, then 6 720 to the end.

After the fix the server levels off about 100 s into the fill, roughly 1 040 cycles in,
which is where 4 096 events fill the log. It goes on growing about 1.3 MiB beyond the log's
own heap. The harness above puts that down to the tables of four links at once and the
allocator's zones. No run had descriptors, threads or leaks left over: server 11 → 11
descriptors and 11 → 11 threads, `leaks` 0.

Once its stores have filled, the worker's slope is 0 KiB/min. The 939 KiB/min it showed
before (the second soak) was the ledger and the event broadcast filling, as that entry
estimated.

### A finding the fill brought out: a dial to the server sometimes gets no answer

1 to 2 of about 11 870 CLI calls a run failed with `cannot reach the server at
127.0.0.1:<port>: connect: …: no answer`, meaning the QUIC handshake timed out
(`slopty_net::client::HANDSHAKE_TIMEOUT`, 2 s). It also stopped an earlier 600 s soak at an
`output` call. The soak now sends such a call again once (a failed dial sent no verb), counts
it as a failure, and goes on with the load. The one-off harness hit it with a fresh client
endpoint per link, while the server was pushing events to its clients. The server logged
`dialer dropped … unsent packet acked`, then `bad transport parameters: parameter had illegal
value` at each Initial the client resent (at 0.02, 0.06, 0.3 and 1.3 s), all from one client
port. Links from one reused endpoint never hit it, and neither did 4 000 fresh endpoints with
no traffic, 437 of them on a port used before. The likely cause is a packet from another
connection crossing into this one on a reused port. The null crypto (`slopty_net::crypto`)
has no AEAD tag to reject such a packet, and the reset-token lookup in `noq-proto`'s
`ConnectionIndex::get` routes by remote address and the datagram's last 16 bytes. This is in
`slopty-net` and `vendor/noq-proto`, and is not measured further here.

## 2026-09-29 — prediction over the line editor's keys

What the local echo draws while a line is edited, before and after ← → and ⌫ are guessed inside a
shell's input (decisions/prediction-editing.md). The rig is `crates/slopty-predict/tests/editing.rs`.
It replays a zle-like line editor on row 0 of an 80-column screen: insert mode, OSC 133 input from
column 2, a right prompt `~/src/slopty` drawn while the line leaves room, and in the second session
a history suggestion in bright black that → at the end of the buffer takes. Each key reaches the
worker half a round trip after it is pressed. The frame answering it (the line after that key,
`input_ack` = the key) arrives half a round trip later. The predictor is `Policy::Adaptive`, told
the round trip, with a 60 Hz display. Keys come at 15/s (66 ms) typed and 30/s (33 ms) held, with a
400 ms pause between steps.

"Right as pressed" means the row the client shows right after the key (the last frame, with the
guesses drawn over it when `visible`) reads as the shell's line after that key, cursor included;
a suggestion's grey and the right prompt are skipped by the reader. "Key → right" is from the
press to the first moment the client shows that line or a later one. "Wrong overlays" counts the
moments, at a key or a frame, when the client showed a line the shell never had. Before is the
predictor at `acc89259`, after is this change, on the same rig.

| session | rtt | keys right as pressed | key → right, mean ms | p50 / p95 ms | wrong overlays | misses |
| --- | --- | --- | --- | --- | --- | --- |
| fixing a typo, before | 20 ms | 17 of 47 | 12.8 | 20 / 20 | 0 | 0 |
| fixing a typo, after | 20 ms | **45 of 47** | **0.9** | 0 / 0 | 0 | 0 |
| taking a suggestion, before | 20 ms | 4 of 31 | 17.4 | 20 / 20 | 4 | 0 |
| taking a suggestion, after | 20 ms | **27 of 31** | **2.6** | 0 / 20 | **0** | 0 |
| fixing a typo, before | 40 ms | 17 of 47 | 25.5 | 40 / 40 | 0 | 0 |
| fixing a typo, after | 40 ms | **45 of 47** | **1.7** | 0 / 0 | 0 | 0 |
| taking a suggestion, before | 40 ms | 4 of 31 | 34.8 | 40 / 40 | 4 | 0 |
| taking a suggestion, after | 40 ms | **26 of 31** | **5.4** | 0 / 40 | **0** | 0 |

"Fixing a typo" types `echo halo world`, holds ← 7 times, types `l`, holds → 7 times, erases 5,
types `there`, holds ← 5 times, erases 1 and types `-`. "Taking a suggestion" types `git co`,
presses → (the suggestion `git commit -m 'fix the build'` is taken), holds ← 7 times, erases 3,
types `fixes`, holds → 7 times and erases 2. Before, every arrow and every ⌫ past the predictor's
own guesses waited for the echo, and so did the next key. The four wrong overlays were keys typed
in the middle of the text, drawn over the character under the cursor where the shell pushed it
right. After, the keys not drawn as pressed are the two warm-up keys, the → that takes the
suggestion (the shell's to draw), and one or two keys of the tentative epoch after it: one at
20 ms, two at 40 ms, where a held key repeats twice within a round trip.

Random sessions (`random_editing_never_shows_a_wrong_line`): 2 000 sessions of 10–119 keys, 60 %
typing from `git cm-'fxhb`, 20 % ←, 10 % →, 10 % ⌫, 10–150 ms apart, round trips of 5–85 ms, half
with the suggestion and half with a right prompt `~/src main` that the line grows into, a session
ending before its line would wrap:

| predictor | keys right as pressed | wrong overlays | misses | sessions with either |
| --- | --- | --- | --- | --- |
| before | 11 565 of 131 867 | 31 642 | 979 | 1 825 |
| after | **117 907 of 131 867** | **0** | **0** | **0** |

The same property held over 100 000 sessions of that kind (6 447 066 keys, 76 s).

Scenario (d) of the smooth suite (`typing_over_a_shaped_round_trip_on_the_mac`) types 60 letters
and no arrow or ⌫, at the fixed round trips 5, 10, 15 and 20 ms (`SHAPED_RTTS` in
`crates/slopty-e2e/tests/smooth.rs`; 40 ms is not among them, and that crate was not this
change's to edit). It measures the printable path, which this change leaves as it was at the
end of the text; it was not rerun here (see the report).

```sh
cargo test -p slopty-predict --test editing -- --nocapture
```

## 2026-09-29 — A dual-stack port no IPv4 socket holds

The fill's dial that got no answer ("A finding the fill brought out", above), found and fixed
(decisions/transport.md, "A dual-stack port no IPv4 socket holds"). The rig is
`fresh_endpoints_dial_a_loopback_server` in `crates/slopty-net/tests/dual_stack_port.rs`. A
server links listener on `127.0.0.1:0` sends every link a directory every 2 ms. Eight tasks
each bind a fresh client endpoint (`bind_client`, `[::]:0`), dial, say hello, read one push
and close, 100 000 links in all. "Before" is `bind_udp` with the IPv4 bind taken out. Release
build, this Mac:

| bind | dials that got no answer | client port of each | run |
| --- | --- | --- | --- |
| before | 10 of 100 000 | 60 294, the server's own | 58.5 s |
| before | 2 of 100 000 | 65 461, the server's own | 45.2 s |
| after | **0 of 100 000** | | 43.1 s |
| after | **0 of 100 000** | | 40.9 s |

One server port in the 16 384 of the ephemeral range predicts 6 in 100 000. For the fill's
11 870 CLI calls it predicts 0.7, and any other IPv4 socket on an ephemeral port adds as much;
the fill saw 1 to 2. A client forced onto the
server's port reproduced the server's log from the fill: one `unsent packet acked`, then
`bad transport parameters: parameter had illegal value` at each Initial, and the dial timed out
with the client having received nothing.

What the kernel does, 20 000 binds against 2 000 sockets held (a one-off probe, run 2026-09-29):

| bind | held by 2 000 sockets on | binds that took a held port |
| --- | --- | --- |
| `[::]:0` dual-stack | `127.0.0.1:0` | 2 360 |
| `[::]:0` dual-stack | `0.0.0.0:0` | 3 074 |
| `0.0.0.0:0` | `127.0.0.1:0` | 0 |
| `0.0.0.0:0` or `127.0.0.1:0` | `[::]:0` dual-stack | 0 |

An explicit `[::]:p` bound beside a socket on `0.0.0.0:p` and was refused beside `127.0.0.1:p`.

The fix costs one IPv4 bind and close per endpoint: 16 to 21 µs over four runs of 10 000. A
whole `bind_client` took 29 to 63 µs in the same runs, before and after alike, which is the
machine's noise. No packet pays anything. The proving tests:
`a_fresh_endpoint_takes_no_port_an_ipv4_socket_holds` (14 of 1 000 endpoints beside 200 IPv4
sockets took one of their ports before, 0 after) and
`an_endpoint_on_a_port_an_ipv4_socket_holds_is_refused`.

```sh
ROUNDS=100000 TASKS=8 cargo test -p slopty-net --release --test dual_stack_port -- --ignored --nocapture
cargo test -p slopty-net --test dual_stack_port
```

## 2026-09-29 — Decoding a frame

Mac Studio M1 Max, macOS 27.0, other sessions building in the same checkout (load average
6–7). Every decode now runs under a cell budget. A thread-local is set per message and read
and written per decoded line, and a line is checked against `MAX_COLS`
(`docs/decisions/terminal.md`, "Decoded lines are bounded"). The wire bytes did not change.

```sh
cargo test -p slopty-proto --release --lib frame_decode_cost -- --ignored --nocapture
```

`codec::decode_body::<TermEvent>` on 200-column frames, µs, min / median:

| frame | bytes | before | after (two runs) |
| --- | --- | --- | --- |
| an echo, 1 row (20 000 rounds) | 1 627 | 3.42 / 3.79 | 3.46 / 3.79–3.88 |
| a screen of code, 60 rows (2 000) | 96 499 | 209.4 / 226.2 | 208.3–208.8 / 227.1–227.5 |
| a blank screen, 60 rows (20 000) | 439 | 25.00 / 25.92 | 24.96–25.38 / 25.6–26.9 |

That is the machine's noise. The allocation budgets (`slopty-grid` and `slopty-engine`
`tests/allocs.rs`) count the same. The blank screen shows what putting the blanks back costs,
0.4 µs a 200-column line, with or without the budget. The 703 bytes that decoded into 314 MB now fail at the first line's width, having allocated
no cells (`decode_bounds.rs`).

## 2026-09-29 — Quick terminal: chord to glass

The quick terminal was removed on 2026-09-30 (decisions/ui.md); this section is its record.

Mac Studio M1 Max, macOS 27.0, 60 Hz display, other sessions building in the same checkout.
The real app under the self-test, one worker on loopback, the quick terminal shown nine times
from the palette's "Toggle quick terminal" (the first show opens its shell and the panel; each
later show follows a hide that has finished sliding). The app logs each show under
`slopty::quick_terminal`: from the command to the panel in front (`asked_to_front_ms`) and to the
first frame painted there (`asked_to_paint_ms`). The chord adds only its hop from the Carbon
handler to the toggle on the same run loop; the self-test registers no chord, so that hop is not
in these numbers.

```sh
cargo xtask e2e app -E 'test(the_quick_terminal_keeps_one_shell_across_shows)' 2>&1 | grep 'quick terminal'
```

| | to the panel in front | to the first frame painted in it |
| --- | --- | --- |
| first show (panel made and dressed) | 24.4 ms | 25.6 ms |
| later shows, median of 8 (min–max) | 1.0 ms (0.4–1.2) | 16.5 ms (0.5–18.0) |

The panel is in front within a millisecond, and its first frame is painted at the next display
tick: one 60 Hz period (16.7 ms), once 0.5 ms when a tick was due. That first frame starts the
slide, so the sheet is just above the window's top; it lands 240 ms later. No presentation
report came back for the panel's frames (`presented_at` stayed empty on every frame of the run),
so the glass is not in the table; by "submit to glass" above, it is a refresh or so after the
paint.

Since the review fixes of the same day, `asked_to_paint_ms` ends at the frame handed to the GPU
(the window's presentation report) rather than in a paint callback, which the view no longer
has. That point is a little later than the one measured above; the table was not measured again.

## 2026-09-29 — Drawn frames to the glass

Mac Studio M1 Max, macOS 27.0, debug build, other sessions building in the same checkout (load
average 6–15). A worker under `SLOPTY_SYNTHETIC_SCREEN=1` serves its drawn display, 1512×982 pt
at 2×, with a page of glyphs scrolling at 180 px/s (`docs/decisions/testing.md`, "A worker under
test serves a drawn screen"). The `slopty-glass` client (`crates/slopty-e2e/src/bin/`) opens it
over loopback QUIC, decodes with VideoToolbox and paints on a 60 Hz pacer that shares the
worker's clock. Capture stamps are mach time, so the host clock is the same at both ends. Each
run is 20 s after a 1 s warm-up. The harness runs every binary from a copy under `$TMPDIR`,
because a VideoToolbox session from the noowners volume took 28–31 s to open.

```sh
cargo xtask e2e smooth --filter 'test(drawn_frames_reach_the_glass_on_loopback)'
```

"Painted" is the pacer's paint of the newest decoded frame: the client surface up to
submission, not the panel's scan-out, which adds up to a refresh. Three runs, ms, p50 / p95 /
p99 / max:

| scale | stream | capture → arrival | capture → decoded | capture → painted |
| --- | --- | --- | --- | --- |
| 0.5 | 1512×982 HEVC | 6.9 / 10.4 / 14.2 / 17.4 | 7.9 / 11.9 / 15.9 / 28.2 | 17.0 / 19.7 / 21.4 / 35.2 |
| 0.5 | | | | 16.2 / 19.5 / 20.2 / 20.6 |
| 0.5 | | | | 15.8 / 19.7 / 20.6 / 31.7 |
| 1 | 3024×1964 HEVC | 23.1 / 30.1 / 64.2 / 72.8 | 25.3 / 33.7 / 66.0 / 74.5 | 33.8 / 48.8 / 81.8 / 85.9 |
| 1 | | | | 33.0 / 36.3 / 65.4 / 69.1 |
| 1 | | | | 32.8 / 36.0 / 49.1 / 84.4 |

At 0.5 about 1 200 frames were painted per run, with none lost, NACKed, repaired or refreshed.
The worker encoded 587 of 588 captured by halfway, encode p50 6.4 / p95 7.7–8.1 ms, capture →
packetized mean 7.1 ms. Decode adds about 1 ms, and the rest is waiting for the paint tick. At
native scale the encoder cannot keep up. It encoded 327–329 of 600 captured (about 30 fps),
encode p50 18 / p95 49–51 ms, capture → packetized mean 24–26 ms, so the pacer repainted each
frame about twice. Again nothing was lost.

The same test then opens the display in the real app for 20 s. The app's tile shows 1329
frames at 756×492, and its UI draws at p50 0.4 ms per 16.7 ms frame. Encode p50 was 3.7 /
p95 7.5 ms and capture → packetized mean 5.2 ms. No presentation report came back, because
the window was covered while the user worked in Parsec. That leaves `presented` at 0, so the
app's own arrival → present has no sample, and the glass numbers above come from
`slopty-glass`. In the same run the app's stream NACKed once and asked for two refreshes with
nothing lost.

The app stream cases (`cargo xtask e2e app --filter 'test(/stream::/) - test(/frame_time/)'`)
assert that a loopback stream loses nothing. Over repeated runs they failed about half the
time. Window runs lost 1 frame and 116 datagrams, then 1 frame. Display runs had refresh
storms of 2 to 266 with no loss, and recovery frames (219) outnumbered the frames shown (121).
That points at the client's decoder after a quality or size change, not at the wire.

## 2026-09-29 — encode time against frame size

Mac Studio M1 Max, macOS 27.0, other sessions building in the same checkout (load average 6
at the start, 24 at the end; printed per row in the log). `encode_time_by_size` in
`crates/slopty-codec/tests/chroma444.rs` opens the worker's session (low-latency hardware HEVC
Main 4:2:0 fed `420f`, with every property `Encoder::configure` sets, 32 Mbit/s) at each size,
times 30 frames one at a time, then submits the scrolling code-editor picture on a real-time
beat for 3 s without waiting for the last frame, as the capture callback does, and reads submit
→ callback after the first second. Nothing is captured and no window opens.

```sh
cargo test -p slopty-codec --release --test chroma444 --no-run   # target/release/deps/chroma444-<hash>
cp target/release/deps/chroma444-<hash> /tmp/slopty-probe/chroma444 && cd /tmp/slopty-probe
nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 --exact tests::encode_time_by_size
SLOPTY_PROBE_SIZES=3024x1964,3024x1968 nice -n 10 ./chroma444 --ignored --nocapture --exact tests::encode_time_by_size
```

| size | w % 16, h % 16 | one at a time p50 | on a 60 fps beat p50 / p95 | on a 120 fps beat p50 / p95 |
| --- | --- | --- | --- | --- |
| 1920 × 1080 | 0, 8 | 8.0 | 8.0 / 10.8 | 7.9 / 10.6 |
| 2560 × 1440 | 0, 0 | 10.7 | 10.3 / 13.3 | 13.0 / 13.3 |
| 3840 × 2160 | 0, 0 | 23.5 | 23.5 / 23.8 | 23.6 / 29.5 |
| 3024 × 1964 | 0, 12 | 18.4 | **87.6 / 91.0** | **88.3 / 105.0** |
| 3024 × 1962 | 0, 10 | 18.3 | **81.1 / 84.7** | **87.6 / 111.8** |
| 3024 × 1960 | 0, 8 | 18.2 | **62.3 / 91.4** | **85.9 / 90.7** |
| 3024 × 1968 | 0, 0 | 18.2 | 18.4 / 25.7 | 18.2 / 20.2 |
| 3024 × 1952 | 0, 0 | 18.4 | 18.3 / 19.1 | 18.2 / 25.5 |
| 3008 × 1964 | 0, 12 | 18.2 | 30.3 / 37.4 | **85.1 / 90.6** |
| 3020 × 1968 | 12, 0 | 18.3 | **84.8 / 88.1** | **87.8 / 91.2** |
| 3456 × 2234 | 0, 10 | 22.6 | **108.9 / 124.2** | **106.9 / 110.2** |
| 3456 × 2240 | 0, 0 | 22.0 | 22.3 / 35.4 | 22.2 / 30.4 |
| 2880 × 1800 | 0, 8 | 16.7 | 17.8 / 25.9 | **77.1 / 82.2** |
| 1728 × 1118 | 0, 14 | 7.8 | 8.8 / 47.9 | 12.7 / 35.8 |
| 1500 × 946 | 12, 2 | 6.6 | 6.5 / 12.1 | 6.4 / 17.9 |
| 1282 × 802 | 2, 2 | 5.2 | 5.4 / 43.1 | 5.5 / 23.6 |
| 2000 × 1234 | 0, 2 | 9.1 | 9.5 / 28.1 | **43.4 / 52.2** |

No row dropped a frame. What it says:

- **The time a frame takes follows its pixels, and alignment does not change it**: 18.2–18.4 ms
  one at a time for every 3024-wide size, 22.0–22.6 ms for both 3456-wide ones.
- **With both sides multiples of 16 the encoder overlaps frames; with either side off 16 it
  does not.** 3840 × 2160 takes 23.5 ms a frame and still keeps a 120 fps beat at 23.6 ms, so
  at least two frames are in the encoder at once. 3024 × 1968 and 3024 × 1952 keep both beats
  at their 18 ms. Every size off 16 whose frame takes longer than the beat's period queues, to
  a steady 77–109 ms: 3024 × 1964/1962/1960 at 60 (18 ms > 16.7) and at 120, 3020 × 1968 with
  its width off, 3456 × 2234 against 22 ms at 3456 × 2240, 2880 × 1800 at 120 only (16.7 ms is
  not over 60's period, is over 120's), 2000 × 1234 at 120 only (9.1 > 8.3). 3008 × 1964 at 60
  sat on the edge (30 ms). Sizes off 16 that fit the period (1920 × 1080 at 120 with 8.0 ms,
  1500 × 946, 1282 × 802) do not queue; their p95 tails are the load.
- **Sixteen, not eight**: 3024 × 1960 and 2880 × 1800 are multiples of 8 and queue.
- The queue stops growing at 77–109 ms rather than without bound, and nothing is dropped, so
  VideoToolbox holds a fixed number of frames (about five at 60 fps) and the beat is held back
  behind it.

A second run the same afternoon (load 15–19), with an aligned twin for each size off 16 (p50
ms, on a 60 / 120 fps beat):

| size | one at a time | 60 fps beat | 120 fps beat |
| --- | --- | --- | --- |
| 3024 × 1964 | 18.4 | 86.1 | 85.8 |
| 3024 × 1968 | 18.3 | 18.3 | 18.1 |
| 3456 × 2234 | 22.6 | 110.9 | 107.2 |
| 3456 × 2240 | 22.2 | 22.2 | 22.1 |
| 2880 × 1800 | 16.6 | 16.4 | 77.4 |
| 2880 × 1808 | 16.4 | 14.2 | 14.8 |
| 2000 × 1234 | 9.3 | 9.4 | 41.5 |
| 2000 × 1232 | 8.5 | 7.9 | 7.5 |

Ruling: `docs/decisions/video.md`, "Stream sides padded to 16, cropped by the SPS conformance
window". The same day's follow-up ("Stream sides padded to 16", below) found that an aligned
session does not overlap frames: it encodes inside the submit call, which held the beat loop's
next submit back, so the aligned rows' "120 fps beat" ran at the encoder's own rate.

## 2026-09-29 — compression presets and the low-latency encoder's keys

Same machine and harness. `compression_presets` reads `SupportedPresetDictionaries` from a
low-latency and a plain hardware session at 1080p, then times the worker's session with and
without `VideoConferencing`'s settings applied on top, 180 frames on a 60 fps beat (the first
second left out) and 120 one at a time, and scores the last ten decoded pictures.

```sh
nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 --exact tests::compression_presets
```

What the encoders offer:

| encoder | preset | settings |
| --- | --- | --- |
| low-latency (`…hevc.rtvc`) | VideoConferencing | AverageBitRate 11 118 750, EnableLTR false, RealTime true |
| hardware (`…ave.hevc`) | Balanced | AllowFrameReordering, LookAheadFrames 4, VariableBitRate 8.0 M, VBVMaxBitRate 12.0 M, VBVBufferDuration 2.5 |
| hardware | HighSpeed | AllowFrameReordering, AverageBitRate 8.0 M, PrioritizeEncodingSpeedOverQuality |
| hardware | HighQuality | AllowFrameReordering, LookAheadFrames 16, VBV as Balanced but 4 s |
| hardware | ConsistentQuality | ConstantQualityFactor 0.5, LookAheadFrames 8, VBVMaxBitRate 16.0 M |

The worker's session against the same with `VideoConferencing` (ms; spent Mbit/s; luma PSNR):

| size | config | beat p50 / p95 / max | one at a time p50 / p95 | dropped | spent | PSNR |
| --- | --- | --- | --- | --- | --- | --- |
| 1920 × 1080, 16 Mbit/s | worker | 7.43 / 7.87 / 8.68 | 7.36 / 9.48 | 0/180 | 6.91 | 53.18 |
| | + VideoConferencing | 7.57 / 9.65 / 11.19 | 8.00 / 10.12 | 0/180 | 6.88 | 53.24 |
| 3840 × 2160, 40 Mbit/s | worker | 23.38 / 23.72 / 24.07 | 23.39 / 23.71 | 0/180 | 18.42 | 53.54 |
| | + VideoConferencing | 23.49 / 23.84 / 24.18 | 23.43 / 23.73 | 16/180 | 11.03 | 48.47 |
| 5120 × 2880, 60 Mbit/s | worker | 38.10 / 38.33 / 38.72 | 38.12 / 38.41 | 0/180 | 23.53 | 53.24 |
| | + VideoConferencing | 38.01 / 38.22 / 38.59 | 38.07 / 38.27 | 54/180 | 9.95 | 41.88 |

The preset changes no time the encoder takes. Its fixed 11.1 Mbit/s rate drops frames and
PSNR at 4K and 5K, and it turns LTR off. The low-latency encoder's supported-property list also
names keys no SDK header exports, `RequestedMaxEncoderLatency`, `NumberOfSubFrameSections`,
`NumberOfSlices`, `ReferenceBufferCount`, `NumberOfTemporalLayers` and `ThroughputMode` among
them. Ruling: `docs/decisions/video.md`, "The `VideoConferencing` compression preset is not
adopted".

## 2026-09-29 — temporal layers on the low-latency encoder

Same machine and harness. `temporal_layers` runs the worker's session at 1080p, 16 Mbit/s, on a
60 fps beat for 120 frames, as it is and with `BaseLayerFrameRateFraction` 0.5, and reads every
sample's NAL headers (`nal_unit_type`, `TemporalId`) and attachments.

```sh
nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 --exact tests::temporal_layers
```

| session | frames | NAL, TemporalId | IsDependedOnByOthers | mean size |
| --- | --- | --- | --- | --- |
| worker (fraction reads back -1) | keyframe | IDR_N_LP, 0 | true | 465 KiB |
| | 119 P | TRAIL_R, 0 | true | 14.2 KiB |
| fraction 0.5 (status 0, reads back 0.5) | keyframe | IDR_N_LP, 0 | true | 465 KiB |
| | 59 P, base | TRAIL_R, 0 | true | 17.9 KiB |
| | 60 P, layer 1 | TSA_R, 1 | false | 13.3 KiB |

Encode time on the beat: 7.43 / 8.91 ms p50 / p95 as it is, 7.77 / 10.01 with the fraction. The
keyframe also carries `FECGroupID` 0, `FECLastFrameInGroup` true and `FECLevelOfProtection` 3;
every frame carries `EncodedFrameAvgQP`. Ruling: `docs/decisions/video.md`, "The encoder
writes every frame as a reference; half of them need not be".

## 2026-09-29 — two parity fragments on small frames, under clumped loss

A simulation, so any machine gives the same numbers. `parity_under_burst_loss` in
`crates/slopty-media/tests/burst_loss.rs` streams 60 fps for a minute over a link that loses
datagrams in runs (a Gilbert channel: `loss` is the share lost, `burst` the mean run length),
three seeds a case. The packetizer and `Redundancy` sit on one end and the reassembler on the
other, with NACKs, refresh requests and 50 ms reports going back over a 10 ms one-way delay and
retransmissions crossing the same lossy link. A still window's frames are 0.6–5.5 kB (one to
five fragments), a scrolling 1080p screen's 8–20 kB (seven to seventeen). "Waited" is frames
that needed a retransmission, "still" the time the picture stood past one frame interval.

```sh
cargo nextest run -p slopty-media --release --run-ignored only -E 'test(parity_under_burst_loss)' --no-capture
```

Before is a floor of one parity fragment; after is two once the ratio is above 50 ‰. Cells are
before → after, over 10 800 frames:

| scene | loss, run | waited a round trip | refreshes | still | wire Mbit/s |
| --- | --- | --- | --- | --- | --- |
| still | 0 % | 0 → 0 | 0 → 0 | 0 → 0 s | 1.98 → 1.98 |
| still | 1 %, 1 | 3 → 0 | 0 → 0 | 0.07 → 0 s | 1.99 → 2.37 |
| still | 1 %, 2 | 111 → 78 | 0 → 0 | 2.6 → 1.8 s | 1.99 → 2.26 |
| still | 1 %, 4 | 111 → 102 | 1 → 0 | 2.7 → 2.3 s | 2.00 → 2.14 |
| still | 3 %, 1 | 32 → 3 | 0 → 0 | 0.8 → 0.07 s | 2.00 → 2.46 |
| still | 3 %, 2 | 325 → 166 | 0 → 0 | 7.6 → 4.3 s | 2.02 → 2.46 |
| still | 3 %, 4 | 321 → 254 | 8 → 3 | 7.9 → 6.7 s | 2.02 → 2.39 |
| still | 5 %, 1 | 93 → 5 | 0 → 0 | 2.3 → 0.1 s | 2.03 → 2.46 |
| still | 5 %, 2 | 528 → 245 | 1 → 1 | 12.2 → 6.4 s | 2.04 → 2.48 |
| still | 5 %, 4 | 554 → 439 | 12 → 7 | 13.7 → 11.2 s | 2.05 → 2.49 |
| scroll | 0 % | 0 → 0 | 0 → 0 | 0 → 0 s | 7.40 → 7.40 |
| scroll | 1 %, 1 | 19 → 6 | 0 → 0 | 0.47 → 0.15 s | 7.70 → 7.95 |
| scroll | 1 %, 2 | 246 → 180 | 0 → 0 | 5.8 → 4.2 s | 7.66 → 7.92 |
| scroll | 1 %, 4 | 288 → 255 | 0 → 0 | 5.9 → 5.5 s | 7.59 → 7.83 |
| scroll | 3 %, 1 | 72 → 50 | 0 → 0 | 1.7 → 1.2 s | 8.03 → 8.08 |
| scroll | 3 %, 2 | 593 → 519 | 0 → 1 | 13.1 → 11.6 s | 8.04 → 8.13 |
| scroll | 3 %, 4 | 731 → 649 | 2 → 0 | 14.9 → 13.7 s | 7.93 → 8.12 |
| scroll | 5 %, 1 | 110 → 103 | 0 → 0 | 2.7 → 2.5 s | 8.29 → 8.29 |
| scroll | 5 %, 2 | 830 → 794 | 1 → 1 | 17.7 → 17.5 s | 8.34 → 8.36 |
| scroll | 5 %, 4 | 1 154 → 1 073 | 3 → 4 | 23.1 → 21.8 s | 8.22 → 8.32 |

A floor of two on every link was run too: the same repairs, but the scrolling screen at 0 %
paid 7.40 → 7.95 Mbit/s and the still window 1.98 → 2.46 for nothing. Ruling:
`docs/decisions/video.md`, "Small frames carry two parity fragments on a lossy link".

## 2026-09-29 — audio: a smaller device buffer and 10 ms packets

Mac Studio M1 Max, macOS 27.0, default output "Mac Studio Speakers", other sessions building in
the same checkout (load average 5–20). Nothing audible played: the device runs carry silence.
`docs/decisions/audio.md` (2026-09-29, "The Mac's device renders 128 frames and packets are
10 ms") holds the ruling.

```sh
cargo test -p slopty-codec --release --lib --no-run      # target/release/deps/slopty_codec-<hash>
cp target/release/deps/slopty_codec-<hash> /tmp/slopty-probe/codec_lib && cd /tmp/slopty-probe
nice -n 10 ./codec_lib --ignored --nocapture --exact audio::tests::device_io_buffer
cargo nextest run -p slopty-codec --release latency_tests --no-capture
```

**Apple's Opus converter takes 10 ms packets.** With `mFramesPerPacket` 480 the 100 ms tone of
`encodes_and_decodes_a_tone` came out as 10 packets, 213 bytes for the first and 88–106 for the
rest at the 96 kb/s target, and decoded to 9 360 of 9 600 samples (the 120-frame pre-skip, as at
960).

**The device takes 128 frames.** `device_io_buffer` opens the player's output unit, asks the
device for a size through the unit (`kAudioDevicePropertyBufferFrameSize`, clamped to
`kAudioDevicePropertyBufferFrameSizeRange`, 15–4 096 here), listens for
`kAudioDeviceProcessorOverload` (a missed I/O deadline, the glitch) and feeds silent packets in
real time from a thread that sleeps to each packet's time, for a minute a size. A run that
starts more than once is a ring that ran dry and refilled; with sound each would be a gap.

| ring floor | asked | in effect | largest render | overloads | ring ran dry |
| --- | --- | --- | --- | --- | --- |
| 15 ms | unasked | 512 | 512 | 0 | 1, 0 |
| 15 ms | 128 | 128 | 128 | 0 | 1, 3 (and 1, 2, 2 in three 30 s runs) |
| 20 ms | unasked | 512 | 512 | 0 | 0, 0 |
| 20 ms | 128 | 128 | 128 | 0 | 0, 0 |

Two rounds each, alternated, 10 ms packets. The feeding thread's own lateness (4–5 ms at p95 on
this loaded machine) is what drains the ring; the device never missed a deadline at either
size. A 15 ms floor was tried because 10 ms packets arrive twice as often, and was given up:
it ran dry at 128 frames where 20 ms did not.

**What the device adds, before the DAC.** The speakers' safety offset, device latency and stream
latency are 317 frames (6.6 ms, 2026-09-28) at either size; the I/O buffer goes from 512 frames
(10.7 ms) to 128 (2.7 ms). The device's share goes from 17.3 to 9.3 ms.

**The ring's traces** (depth heard, ms; "before" is 20 ms packets and 512-frame renders, as of
2026-09-28 and rerun today, "after" 10 ms packets and 128-frame renders with the ring's device
term taken from the renders it sees). A 128-frame render takes a smaller bite, so the depth read
after each one sits up to 4 ms higher than after a 512-frame one for the same ring; the column is
the ring alone.

| trace | before | after | underruns before → after |
| --- | --- | --- | --- |
| 250 ms stall, then its backlog in one burst | 21 mean over 1.5–3 s, 31 at most after | 24 mean, 59 at most after | 1 → 1 |
| worker clock +100 ppm, 4 min | 30 → 28 | 25 → 22 | 0 → 0 |
| worker clock −100 ppm, 4 min | 28 → 21 | 23 → 20 | 0 → 0 |
| a 40 ms keyframe burst every 2 s | 49 mean | 39 mean | 3 → 3 |
| muted 2–3 s, a 400 ms gate inside | 30 mean | 23 mean | 0 → 0 |
| capture handed over 1 024 frames at a time (new) | 27 mean | 16 mean | 0 → 0 |
| renders of 512 frames (a device that refuses 128) | — | 23 mean | — → 0 |
| renders of 1 024 frames (iOS's default) | 18 mean, target 20 | 23 mean, target 22.3 | 0 → 0 |

At the DAC that is ring + 17.3 ms before and ring + 9.3 ms after: 45 → 31 ms on the +100 ppm
trace's last ten seconds, 66 → 48 ms under keyframe bursts. The stall's backlog now takes longer
to cut (59 ms at most after the burst against 31), since a 10 ms packet gives at most 7.5 ms to a
cut where a 20 ms one gave 17.5. On iOS the ring now counts the 21.3 ms render it really takes
and holds 5 ms more than before; asking the audio session for a 10 ms I/O buffer
(`setPreferredIOBufferDuration`, in `slopty-platform`) would take that and more off.

Not measured here: what ScreenCaptureKit hands the worker per audio sample buffer. A packet now
leaves once 10 ms of audio is in, not 20, which is 10 ms off the first sample of each packet
when the capture's buffers are that short. The chunked trace is the case where they are 1 024
frames; the ring covers the clumping it causes. Measuring it needs a Screen Recording-signed
worker. On the wire a stream sends 100 packets a second instead of 50, about 90–106 bytes each
instead of about 240, so the per-datagram headers cost roughly 25 kbit/s more (an estimate).

## 2026-09-29 — refresh storms on loopback

On the M1 Max (macOS 27.0), over loopback with no loss injected, the drawn display failed its
e2e case in 6 of 10 runs and the drawn window in 0 of 10. The runs were logged at debug. A
failing run showed decode errors, a storm of refreshes, and "1 lost" out of about 116
datagrams. The worker-side reproducer streams the drawn display through the real pipeline and
reassembler with the reports and feedback answered. It changes the quality six times, and every
change builds a new encoder session. Before the fixes, 3 of 4 runs had decode errors (3, 15 and
3) and refreshes (2, 10 and 2). After them, 0 errors in 14 runs.

```sh
cargo xtask e2e app --filter 'test(/stream::/) - test(/frame_time/)'
TMPDIR=/tmp nice cargo test -p slopty-worker --lib quality_changes_decode_without_a_refresh -- --nocapture
TMPDIR=/tmp nice cargo test -p slopty-worker --lib a_refresh_with_nothing_acknowledged_breaks -- --ignored --nocapture
cargo test -p slopty-media --test pipeline given_up
```

There were four causes, each found in the logs and then pinned by a test that failed before
its fix:

- **A refresh with nothing acknowledged.** VideoToolbox, asked for `ForceLTRRefresh` before any
  token is acknowledged, writes a sync frame. At 3024 × 1964, where the encoder drops frames
  under real-time pressure, the frames after it fail to decode (-12909) until the next keyframe.
  In each line, `K` is a sync frame, `r` a refresh, `t` a token and `!` a failed decode:
  - The LTR refresh: `6 failed: 0:Kt0 2:P 4:Kr 6:Pt6! 8:P! 10:Pt10! 12:P! 14:P! 16:P!`
  - A keyframe in its place: `0 failed`.

  Dropping the stale acknowledged tokens from the request did not help, so the cause is the
  missing acknowledgement, not an old one. Each failure brought a refresh, which caused the next
  failure. With nothing acknowledged, the worker now sends a keyframe.
- **An old session's frame after the new keyframe.** A retired encoder session finishes the
  frame it holds after the new session's first keyframe is out. The decoder took that P-frame
  from another session and failed. Packets now carry their session's generation, and a stale
  one is dropped.
- **A refresh asked for while the keyframe is encoded.** A session's first keyframe takes
  60–130 ms at 3024 × 1964. The client repeats its refresh after 100 ms and two round trips, so
  every stream that opened at that size made two keyframes of about 160 kB each. Now a keyframe
  in flight answers requests for 400 ms, and the client waits 400 ms before asking for a new
  stream's first keyframe again. A client that asks while part of the keyframe has arrived
  also made a second keyframe: in one run the repeat went out 50 ms into a 136 kB keyframe that
  QUIC was still draining (142 kB held). The reassembler now holds its repeat while a frame
  that would restart delivery is still arriving and has not been given up on.
- **The first keyframe given up on while it arrives.** The reassembler lost frame 0 at an age
  of 15–20 ms with 130 of its 138 fragments missing and `flowing` true. It was not lost on the
  wire: the connection's send window was still opening, and QUIC held about 120 kB of the
  keyframe for 15–85 ms. Its later fragments re-created the frame, which was lost again. In one
  run, frame 0 was lost four times in 60 ms. The deadline now runs from the frame's latest
  fragment, and a frame given up on stays given up on until the keyframe it waits for arrives.
  This was the "1 lost with 116 datagrams" on a lossless link.

Still open: the congestion guard (`Shared::dropped`) asks for a refresh when it skips a capture
that was never encoded. At start-up, with the first keyframe still held by QUIC, that refresh
finds no acknowledged reference and goes out as a second keyframe of 165 kB, 75 ms behind the
first. The client loses nothing, so it does not fail the case, but those bytes are wasted. The
guard's rule is a transport ruling (`docs/decisions/transport.md`) and is left as it is here.

After the fixes, pass counts, logged at debug, with other sessions building alongside (load
average 9–19):

| build | drawn window | drawn display |
| --- | --- | --- |
| before | 10 / 10 | 4 / 10 |
| every fix but the hold while a keyframe arrives | 9 / 10 | 10 / 10 |
| every fix | 10 / 11 | 11 / 11 |

The one window failure left is a stall with nothing lost and no refresh. The first keyframe
(136 kB) waited in the worker's datagram queue for 133 ms (142 kB held) and was released in one
piece ("the link held datagrams the worker had already sent", `worker_gap` 0). With a 38 kB
initial window and a 2 ms ACK delay, it should leave in a few round trips. Within the next
second the same worker logged late heartbeats of 150–250 ms, so its runtime was starved around
then. This is the transport under load, not the screen path, and is left to its owner.

Ruling: `docs/decisions/video.md`, "A stream recovers with a keyframe until a reference is
acknowledged".

## 2026-09-29 — Stream sides padded to 16

Mac Studio M1 Max, macOS 27.0, other sessions building in the same checkout (load average
printed per row, 5–23). A stream's capture surface and encoder session are now its picture's
sides rounded up to 16, the picture at the top-left and black around it, and the worker rewrites
each keyframe's SPS conformance window so decoders output the picture's own size
(`docs/decisions/video.md`, "Stream sides padded to 16"). Every probe ran from a copy of its
binary under `/tmp`.

```sh
cargo test -p slopty-codec --release --test chroma444 --no-run   # target/release/deps/chroma444-<hash>
cp target/release/deps/chroma444-<hash> /tmp/slopty-probe/chroma444 && cd /tmp/slopty-probe
nice -n 10 ./chroma444 --ignored --nocapture --exact tests::conformance_window
SLOPTY_PROBE_ALIGN=16 nice -n 10 ./chroma444 --ignored --nocapture --test-threads=1 --exact tests::encode_time_by_size
SLOPTY_PROBE_SIZES=3024x1964,3456x2234,2880x1800,2000x1234 nice -n 10 ./chroma444 --ignored --nocapture --exact tests::encode_time_by_size
nice -n 10 ./chroma444 --ignored --nocapture --exact tests::submit_blocking_and_pictures_held
cargo test -p slopty-codec --release --lib --no-run && ./slopty_codec-<hash> --ignored --nocapture --exact conformance::tests::keyframe_crop_time
cargo test -p slopty-worker --lib --no-run      # target/debug/deps/slopty_worker-<hash>
cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-pad/slopty_worker && cd /tmp/slopty-pad
TMPDIR=/tmp nice ./slopty_worker --ignored --exact screen::synthetic::tests::capture_to_glass_padded_against_even --nocapture
```

**Probe P1: the rewritten window is the encoder's own.** `conformance_window` encodes the
scrolling code-editor picture on the worker's session (`slopty_codec::Encoder`) twice: at its
true size, and drawn at the top-left of the size padded to 16 with the SPS rewritten by
`slopty_codec::conformance::crop_access_unit`. Both go through the client's decoder
(`slopty_codec::Decoder`, hardware).

| stream | SPS rewritten from the padded session = the encoder's own at the true size | decoded | luma PSNR | chroma PSNR | edge error |
| --- | --- | --- | --- | --- | --- |
| 3024 × 1964 Main 4:2:0, native | (reference) | 3024 × 1964 | 41.39 dB | 28.39 dB | 2.48 |
| 3024 × 1964 coded 3024 × 1968 | bit for bit | 3024 × 1964 | 42.07 dB | 28.40 dB | 2.48 |
| 3456 × 2234 Main 4:4:4 10, native | (reference) | 3456 × 2234 | 36.20 dB | 38.96 dB | 5.36 |
| 3456 × 2234 coded 3456 × 2240 | bit for bit | 3456 × 2234 | 36.22 dB | 39.45 dB | 5.28 |

"Edge error" is the mean absolute luma difference, 0–255, between the decoded picture's last
row and last column and the source's; a band of padding that showed would be the black
against the picture's own level there. VideoToolbox's decoder crops by the window, so the
client's buffers, the GPUI surface and pointer mapping see the true size with no change. The
encoder itself already codes 3024 × 1964 as a 3024 × 1968 picture with a two-unit bottom window;
the rewrite writes the same window over the padded session's SPS. `CleanAperture` on a 3024 ×
1968 session is accepted (status 0) and writes no window: the SPS still shows 3024 × 1968.

**Encode time, padded, against the same sizes as they were.** The size sweep with
`SLOPTY_PROBE_ALIGN=16` (load 5–8), then the sizes off 16 as they were (load 6–7), p50 ms:

| size | coded | as it was: one at a time / 60 beat / 120 beat | padded: one at a time / 60 beat / 120 beat |
| --- | --- | --- | --- |
| 3024 × 1964 | 3024 × 1968 | 17.6 / 17.7 / **76.0** | 15.2 / 15.2 / 15.2 |
| 3456 × 2234 | 3456 × 2240 | 22.3 / **95.6** / **95.6** | 19.2 / 19.1 / 19.1 |
| 2880 × 1800 | 2880 × 1808 | 15.9 / 15.8 / **68.1** | 13.6 / 14.9 / 13.8 |
| 2000 × 1234 | 2000 × 1248 | 8.6 / 8.8 / 8.9 (p95 14.6) | 7.3 / 7.4 / 7.3 |
| 1920 × 1080 | 1920 × 1088 | | 6.2 / 6.4 / 6.2 |
| 1728 × 1118 | 1728 × 1120 | | 6.3 / 6.4 / 6.0 |
| 1500 × 946 | 1504 × 960 | | 4.9 / 5.2 / 4.9 |
| 1282 × 802 | 1296 × 816 | | 3.9 / 4.2 / 4.1 |
| 3840 × 2160 | (aligned) | | 20.5 / 20.5 / 20.5 |

No row dropped a frame. Every padded size runs at its one-at-a-time time on either beat, and
that time is itself 12–15 % shorter than the same picture's off 16: the encoder's own padding
step was costing it on every frame. The 3024 × 1964 row at 60 did not queue this time (17.7 ms,
just over the period at a lower load than the table above it) and did at 120.

**The submit encodes the frame when the sides are multiples of 16.**
`submit_blocking_and_pictures_held` times `VTCompressionSessionEncodeFrame` itself on a
real-time beat and counts the source pictures the session retains before each submit (load
16–23):

| session | fps | submit call p50 / p95 / max | pictures held p50 / max |
| --- | --- | --- | --- |
| 3024 × 1964 | 60 | 0.16 / 15.55 / 24.0 | 1 / 2 |
| 3024 × 1964 | 120 | 15.4 / 18.6 / 37.2 | 2 / 2 |
| 3024 × 1968 | 60 | 15.3 / 15.4 / 15.6 | 0 / 0 |
| 3024 × 1968 | 120 | 15.3 / 15.4 / 15.6 | 0 / 0 |
| 1920 × 1088 | 60 | 6.7 / 12.1 / 21.4 | 0 / 0 |
| 1920 × 1080 | 60 | 0.04 / 0.06 / 0.38 | 0 / 0 |

This corrects the reading of "encode time against frame size" above: an aligned session does
not overlap frames. It encodes inside the submit call (the header allows it: "The
kVTEncodeInfo_Asynchronous bit may be set if the encode ran asynchronously") and keeps no
source picture after it returns. A session off 16 returns at once, copies the picture into its
own padded buffer, and queues it; that queue is the 77–111 ms. The aligned rows "kept a 120 beat"
only because the beat loop's submits were themselves held back by the call. The rate an aligned
session sustains is one frame per encode time. The sweep now prints the rate it managed to
submit at: 3024 × 1968 60.0 frames a second on the 60 beat and 65.2 on the 120 beat, and
3840 × 2160 48.5 on either, where the table above read "keeps a 120 fps beat at 23.6 ms". A 4K
stream on this encoder is a 48 fps stream.

So the worker no longer submits on the capture's queue. The capture leaves each frame in a
one-frame mailbox for the stream's encode thread (`start_encode_thread` in
`crates/slopty-worker/src/screen.rs`), and a capture that arrives while the encoder is busy
replaces the one waiting. The repair loop submits from the blocking pool. Without that, the
first glass run below captured the drawn display at 30 frames a second: 1.6–2.4 ms of drawing
plus a 15.5 ms submit is more than a 60 Hz period.

**Rewriting a keyframe's SPS** (`keyframe_crop_time`, release): 12.5 µs for a 160 kB 3024 ×
1964 keyframe, of which 2.7 µs is copying the unit; once per keyframe.

**Capture to the glass, in one binary.** `capture_to_glass_padded_against_even` streams
`Synthetic`'s 3024 × 1964 display at native scale through the real encoder, packetizer, QUIC-less
router, reassembler and decoder, painted on a 60 Hz pacer, 20 s a run, alternating the stream as
it was (the picture's even size) with the padded one (each run's own `pad_to`). Two rounds with the
encode thread, load 12–16, p50 / p95 ms:

| run | frames encoded / s | worker encode | capture → decoded | decoded → painted | capture → painted |
| --- | --- | --- | --- | --- | --- |
| even, round 1 | 54.1 | 19.5 / 33.4 | 24.9 / 47.9 | 8.9 / 16.5 | 32.5 / 57.8 |
| padded, round 1 | 53.7 | 15.5 / 21.6 | 21.2 / 41.5 | 11.8 / 16.5 | 32.7 / 52.9 |
| even, round 2 | 58.0 | 17.7 / 20.9 | 21.8 / 37.0 | 10.0 / 13.0 | 32.4 / 47.7 |
| padded, round 2 | 59.7 | 15.2 / 15.3 | 19.0 / 20.4 | 13.0 / 15.1 | 32.1 / 34.5 |

No frame was lost, NACKed, refreshed or failed to decode in any run. Padded, the encoder is
2–4 ms faster at p50 and its p95 stays at its p50 (15.3 against 20.9 in the quieter round), and
capture → decoded p95 falls from 37 to 20 ms. At this load the unpadded stream did not queue at
60 (its 17.7 ms is just over the period), so both sides encoded 54–60 frames a second. Capture
→ painted p50 is the same 32 ms on both: the pacer paints on its own 60 Hz tick, and a frame
decoded 19–22 ms after its capture waits for the second tick after the capture either way
(decoded → painted takes up the difference). What padding bought on this path is the tail and
the rate headroom; the p50 needs capture → decoded under one period, or a paint on decode (the
VideoLayer, rank 2 of the plan).

**Drawn frames to the glass, over loopback QUIC.** The app-level instrument of "Drawn frames to
the glass" above, same command, one run after the change (load 10–14), against that section's
three runs from the same morning:

```sh
cargo xtask e2e smooth --filter 'test(drawn_frames_reach_the_glass_on_loopback)'
```

| scale 1, 3024×1964 HEVC | capture → arrival | capture → decoded | capture → painted p50 / p95 / p99 / max | frames timed in 20 s | worker encode p50 / p95 |
| --- | --- | --- | --- | --- | --- |
| before (3 runs) | 23.1 | 25.3 | 32.8–33.8 / 36.0–48.8 / 49.1–81.8 / 69.1–85.9 | about 30 a second encoded (327–329 of 600 captured) | 18 / 49–51 |
| padded, encode thread | 17.1 | 18.9 | 32.7 / 34.4 / 35.3 / 36.0 | 1180 (59 a second) | 15.2 / 15.4 |

At 0.5 (1512×982, coded 1520×992) the same run gave capture → painted 16.5 / 18.2 / 19.1 /
22.3 ms, against 15.8–17.0 p50 before, with encode p50 5.3 ms against 6.4. Nothing was lost,
refreshed or failed to decode. The acceptance the design set (at least 58 frames painted a
second, capture → painted p50 at most 27 and p95 at most 34 ms) holds for the rate and the p95;
the p50 does not move on a 60 Hz paint for the reason given in the in-process runs.

## 2026-09-29 — a taken key's hop and the input-source answer

Mac Studio M1 Max, macOS 27.0.1, release build under `nice`, other sessions building in the same
checkout (load average 4–6). Two runs of one opt-in binary: a real `NSApplication` on its main
thread with no window and no Dock icon, the real key monitor (`slopty_platform::keyboard::take_keys`),
and the real input sources (`slopty_input::sources::Sources::system`). Keys are made in the
process and posted into its own event queue (`-[NSApplication postEvent:atStart:]`), nothing
through `CGEventPost`; the monitor's wake hops to the main queue as GPUI's foreground executor
does when the view's task wakes, and the drain there is where the view sends the key
(`docs/decisions/input.md`, "Keys go by position").

```sh
SLOPTY_MEASURE=1 nice cargo test -p slopty-input --release --test key_path
```

| | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- |
| monitor to drain, 2 × 2000 keys: what the taken path adds | 124–141 µs | 284–327 µs | 0.95–2.0 ms | 2.7–13.1 ms |
| the answer for the source the worker already has, 2 × 300 | 6.5–23.7 µs | 8.7–36.8 µs | 13–102 µs | 27–379 µs |
| `TISCopyCurrentKeyboardInputSource` and its id, 2 × 300 | 0.3–0.7 µs | 0.3–0.8 µs | 0.4–0.8 µs | 0.5–1.7 µs |

A key GPUI dispatched went to the worker inside `sendEvent:`; a taken key goes one main-queue
turn later, a tenth of a millisecond at the median and under 2 ms at p99 on a loaded machine,
against a frame of 8.3 ms at 120 Hz. The binary also prints the time from `postEvent:` to the
monitor (p50 8 ms); that is AppKit handing an event posted in-process back to `nextEvent`, which
real keys from the WindowServer do not take, and is not in either path.

The answer to an ask for the source the worker already has goes at once, and the input behind
it waits for nothing: a round trip through the main queue, tens of microseconds. A switch is no
longer answered after a fixed 50 ms that was chosen and never measured: the answer goes as the
worker hears the switch (`kTISNotifySelectedKeyboardInputSourceChanged`, the notification the
target app reads it by), bounded at 100 ms, and the stream holds the keys behind it at most
150 ms. How long that takes was not measured here: timing it switches the input source of
whoever uses this Mac, and the user was on it. The same binary times it on a Mac nobody is
typing on, 20 switches each way:

```sh
SLOPTY_MEASURE=1 SLOPTY_MEASURE_SWITCH=com.apple.keylayout.French cargo test -p slopty-input --release --test key_path
```

## 2026-09-29 — taking the keyboard back: what reclaiming the input source costs

Same machine and binary as the entry above, rerun after claims became per-stream tokens and
the worker's input holding was narrowed to keys and text (`docs/decisions/input.md`, "Keys go
by position", "How long a claim holds"). Load average about 8, other sessions building.

```sh
SLOPTY_MEASURE=1 nice cargo test -p slopty-input --release --test key_path
```

| | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- |
| monitor to drain, 2000 keys | 168 µs | 373 µs | 1.2 ms | 5.4 ms |
| the answer for the source the worker already has, 300 | 26 µs | 50 µs | 110 µs | 507 µs |
| `TISCopyCurrentKeyboardInputSource` and its id, 300 | 0.7 µs | 0.7 µs | 0.8 µs | 1.9 µs |

A tile that takes the keyboard back within 10 s (`RELEASE_AFTER`) still holds its claim, so its
ask is the second row: a main-queue round trip, and nothing typed waits on it. After a longer
absence the claim was released and the worker went back to its own source, so the ask is a real
switch, and the keys and text typed meanwhile wait until the worker hears it, at most 150 ms
(`HOLD_MOST`); the pointer no longer waits unless a key is held ahead of it. The switch itself
is not measured here: timing it switches the input source of whoever is using this Mac, so it
stays opt-in for a Mac nobody is typing on, `SLOPTY_MEASURE=1
SLOPTY_MEASURE_SWITCH=com.apple.keylayout.French cargo test -p slopty-input --release --test
key_path`.

## 2026-09-29 — the encoder watch behind the mailbox

Mac Studio M1 Max, macOS 27.0, other sessions building in the same checkout (load average per
row). A follow-up to "Stream sides padded to 16" above: an aligned session codes the frame
inside the submit, and the mailbox in front of it replaces a capture the encoder is busy for, so
frames are lost rather than late and the late-run rule saw nothing. `EncoderWatch` now weighs
windows of 30 due captures: the share of the rung's slots the encoder took, capped at one frame
per mean encode time (`docs/decisions/video.md`, "Stream sides padded to 16"). "Before" is the
padded tree with the mailbox and the watch blind to it, one binary each, both run from `/tmp`.

```sh
cargo test -p slopty-worker --lib --no-run      # target/debug/deps/slopty_worker-<hash>
cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-video/after/slopty_worker && cd /tmp/slopty-video/after
TMPDIR=/tmp ./slopty_worker --ignored --exact screen::synthetic::tests::capture_to_glass_at_120_past_the_encoder --nocapture
TMPDIR=/tmp ./slopty_worker --ignored --exact screen::synthetic::tests::capture_and_input_to_glass_at_120 --nocapture
cargo test -p slopty-codec --test chroma444 --no-run && cp target/debug/deps/chroma444-<hash> /tmp/slopty-video/run/chroma444
cd /tmp/slopty-video/run && ./chroma444 --ignored --nocapture --exact tests::the_scaler_is_opened_only_off_16
```

**3024 × 1964 on a 120 Hz display, 120 asked, 20 s a run** (`capture_to_glass_at_120_past_the_encoder`,
the drawn display at native scale coded as 3024 × 1968, painted on a 120 Hz pacer). The middle
rows are the two rules tried on the way, kept for why the rule is what it is. p50 / p95 ms:

| loopback | load | rung / ceiling | encoded / s | worker encode | capture → decoded | capture → painted p50 / p95 / max | captures replaced |
| --- | --- | --- | --- | --- | --- | --- | --- |
| before | 10–7 | 120 / 120 | 65.8 | 15.10 / 15.24 | 22.41 / 26.51 | 27.06 / 28.39 / 36.76 | not counted |
| every replaced due capture a lost slot | 7–5 | 36 / 36 | 36.0 | 15.56 / 19.63 | 20.31 / 28.62 | 27.13 / 36.36 / 51.79 | 253 |
| a slot lost only to the next slot's capture | 16–10 | 55 / 55 | 54.7 | 16.33 / 16.97 | 23.11 / 32.04 | 26.40 / 37.36 / 59.66 | 629 |
| after: the ceiling also rises | 7–6 | 65 / 65 | 65.0 | 15.15 / 15.27 | 21.54 / 25.62 | 26.12 / 28.18 / 35.49 | 896 |

| tailnet-shaped (5 ms, 3 %) | rung / ceiling | encoded / s | worker encode | capture → decoded | capture → painted p50 / p95 / max |
| --- | --- | --- | --- | --- | --- |
| before | 120 / 120 | 66.2 | 15.03 / 15.18 | 28.80 / 33.23 | 33.88 / 35.33 / 43.51 |
| every replaced due capture a lost slot | 32 / 32 | 32.6 | 16.77 / 19.11 | 29.96 / 34.42 | 34.35 / 42.63 / 59.46 |
| a slot lost only to the next slot's capture | 53 / 53 | 52.9 | 16.51 / 17.48 | 29.63 / 37.51 | 34.36 / 42.90 / 56.99 |
| after | 65 / 65 | 65.0 | 15.07 / 15.27 | 27.60 / 32.24 | 33.72 / 35.11 / 42.93 |

No run lost a frame, refreshed or failed to decode. Counting every replaced due capture as a
lost slot was wrong below the capture rate: at a 66 rung on 120 Hz captures, a due capture
replaced by one 8.3 ms newer still has its slot filled by the newer one, and the ratchet took
the rung to 36 and then 32 over a run. Counted only when the newer capture falls in the next
slot, one slow window under load (encode p95 17–20 ms) still took the ceiling to 55, and the
ceiling only ever fell. It now rises again when the mean encode time allows over eight sevenths
of the rung. After the change, the encoder is told 65 frames a second where it was told 120
and fed 66. On this light synthetic picture rate control never reached its cap (3.9 KiB a
frame, where a 120th of 30 Mbit/s is 31 KiB), so latency and rate match the before run within
noise. What changes is what each frame is budgeted from, in the encoder and in the congestion
guard, once the rate is what limits the picture.

**1080p on a 120 Hz display** (`capture_and_input_to_glass_at_120`, 60 and 120 asked, before and
after alternated twice at load 8–12, then once more): capture → painted p50 moved within 1 ms
either way in every pairing (loopback 60 asked 10.1–10.6 before against 9.3–10.1 after, 120
asked 8.5–8.8 against 8.3–9.1; tailnet 17.2–17.3 against 16.8–19.8), and the p95 and max
followed the load, not the build. An encoder that keeps its rung loses no slot, the watch
changes nothing there, and every window read "fed 60" or "fed 120".

**The deadlock, before the fix.** `a_rebuild_beside_a_cadence_change_finishes` on the tree
before (four rebuilds lined up on the frame whose output callback moved the rung, real
1920 × 1088 sessions, from `/tmp`) never finished: five runs in five hit the test's 10 s bound.
After the fix it takes 0.4–0.9 s from `/tmp` and 12–20 s from the repo volume, which is
mounted `noowners`, so each VideoToolbox session revalidates the binary's signature.

**What the hosted runner's refresh was.** `quality_changes_decode_without_a_refresh` failed on
the GitHub macOS runner (tree before the padding) with one refresh, nothing lost and no decode
error. The worker counted the refresh and answered it with neither a keyframe nor a refresh
frame, which is what it does for an ask that arrives while a keyframe is in flight. With
nothing lost, the only ask the client makes is the new stream's repeat after
`FIRST_KEYFRAME_WAIT` (400 ms) with no datagram yet, so that keyframe came late. The guest
logs `IOServiceMatching failed for: AppleM2ScalerParavirtDriver`. Here, a session opens the M2
scaler for a picture off 16 and never for one on it (`the_scaler_is_opened_only_off_16`, five
frames each, user clients of `AppleM2ScalerCSCDriver` counted by `ioreg` after them):

| size | scaler user clients | five frames |
| --- | --- | --- |
| 3024 × 1964 | 2 | 197.8 ms |
| 3024 × 1968 | 0 | 121.4 ms |
| 1512 × 982 | 2 | 68.1 ms |
| 1520 × 992 | 0 | 62.3 ms |
| 756 × 492 | 2 | 51.6 ms |
| 768 × 496 | 0 | 45.4 ms |

These are the sizes that test codes, as the stream was and padded. A padded stream never asks
for the scaler that the guest lacks. Whether the guest's first padded keyframe then comes
within 400 ms can only be read on the runner itself; this Mac has the scaler.

## 2026-09-29 — temporal layers on the worker's session

Mac Studio M1 Max, macOS 27.0, load average 3–5, run from `/tmp` under `nice` (the repo volume
is mounted `noowners`, and each VideoToolbox session there revalidates the binary's
signature). `temporal_layers_skip_and_toggle` runs the worker's low-latency session at
1920 × 1088 on scrolling text, first with `BaseLayerFrameRateFraction` 0.5 and LTR acknowledged,
then decodes the stream with frames left out and compares every decoded picture's hash against
the whole stream's. The same session then prices the layers on a 60 fps beat.

```sh
cargo test -p slopty-codec --test chroma444 --no-run        # target/debug/deps/chroma444-<hash>
mkdir -p /tmp/slopty-p5 && cp target/debug/deps/chroma444-<hash> /tmp/slopty-p5/chroma444 && cd /tmp/slopty-p5
nice ./chroma444 --ignored --nocapture --exact tests::temporal_layers_skip_and_toggle
SLOPTY_PROBE_LAYERS_COST_ONLY=1 nice ./chroma444 --ignored --nocapture --exact tests::temporal_layers_skip_and_toggle
```

**What may be skipped.** A frame is "layer 1" when its sample says `IsDependedOnByOthers` false
(`TSA_R`, `TemporalId` 1). Decoded, failed and differing are counted over the frames kept:

| left out | decoded | failed | differ from the whole stream |
| --- | --- | --- | --- |
| every layer-1 frame | 45 | 0 | 0 |
| every other layer-1 frame | 67 | 0 | 0 |
| the layer-1 frames either side of an LTR refresh | 87 | 0 | 0 |
| one base frame (control) | 20 | 69 | — |
| a base frame at 30, refresh on a base slot (36) | 84 | 0 | 0 |
| a base frame at 30, refresh on a layer-1 slot (37) | 83 | 0 | 0 |
| a layer-1 frame at 31, no refresh | 89 | 0 | 0 |

A refresh asked on a layer-1 slot comes back as a base frame (`TRAIL_R`, depended on), and LTR
tokens ride base frames only. `BaseLayerFrameRateFraction` is taken live: unset it reads -1
and every frame is base; 0.5 gives `BLBL…` from the next frame; 1.0 gives all base; 0.5 again
gives `BLBL…`, every status 0, no keyframe. 0.67 and 0.75 are accepted and then every frame
of 240 comes back dropped, so 0.5 is the only fraction this encoder codes.

**What layers cost on a session opened with them** (120 frames on the beat after a settling
second; `+ 0.8` is `BaseLayerBitRateFraction` 0.8, the cheapest of Apple's suggested 0.6–0.8):

| size, target | layers | encode p50 | dropped | base / layer-1 frame | spent | luma PSNR |
| --- | --- | --- | --- | --- | --- | --- |
| 1920 × 1088, 16 Mbit/s | none | 6.41 ms | 0 | 12.3 KB | 5.89 Mbit/s | 53.27 dB |
| | 0.5 | 6.38 ms | 0 | 17.5 / 12.9 KB | 7.31 Mbit/s | 53.24 dB |
| | 0.5 + 0.8 | 6.31 ms | 0 | 17.3 / 11.7 KB | 6.96 Mbit/s (+18 %) | 53.23 dB |
| 3024 × 1968, 32 Mbit/s | none | 15.27 ms | 0 | 23.9 KB | 11.48 Mbit/s | 53.30 dB |
| | 0.5 | 16.87 ms | 0 | 38.2 / 24.3 KB | 14.98 Mbit/s | 53.05 dB |
| | 0.5 + 0.8 | 15.25 ms | 0 | 37.7 / 20.5 KB | 13.97 Mbit/s (+22 %) | 53.04 dB |
| 1920 × 1088, 3 Mbit/s | none | 6.38 ms | 18 | 6.3 KB | 3.00 Mbit/s | 45.30 dB |
| | 0.5 + 0.8 | 6.42 ms | 36 | 7.9 / 3.5 KB | 2.68 Mbit/s | 42.83 dB |
| 3024 × 1968, 6 Mbit/s | none | 15.29 ms | 50 | 12.6 KB | 5.99 Mbit/s | 46.94 dB |
| | 0.5 + 0.8 | 15.33 ms | 68 | 15.9 / 7.5 KB | 5.10 Mbit/s | 41.35 dB |

Encode time does not move. On a session opened with layers, they cost 18–22 % more bytes at
the same picture where the rate leaves room, and 2.5–5.6 dB of luma and twice the dropped
frames where the rate is the limit. That is not how the worker uses them: it switches them on a
session that has run a while, where they cost next to nothing ("temporal layers switched on a
live session" below). Ruling: `docs/decisions/video.md`, "Frames nothing refers to are skipped,
not refreshed".

## 2026-09-29 — temporal layers switched on a live session

Mac Studio M1 Max, macOS 27.0, run from `/tmp` under `nice`, load average per row in the log
(3–17: other sessions building). The first cut above priced sessions opened with layers on.
The worker switches them on and off on a session that is already running, which is a
different encoder. Three probes in `crates/slopty-codec/tests/chroma444.rs` price that. Each
runs the worker's session on scrolling text on a 60 fps beat, and every phase starts with a
settling second.

```sh
cargo test -p slopty-codec --test chroma444 --no-run      # target/debug/deps/chroma444-<hash>
mkdir -p /tmp/slopty-rev && cp target/debug/deps/chroma444-<hash> /tmp/slopty-rev/chroma444 && cd /tmp/slopty-rev
nice ./chroma444 --ignored --nocapture --exact tests::temporal_layers_live_switch
nice ./chroma444 --ignored --nocapture --exact tests::temporal_layers_when_switched_on
nice ./chroma444 --ignored --nocapture --exact tests::temporal_layers_by_codec
nice ./chroma444 --ignored --nocapture --exact tests::mean_squared_error_report
```

**Switched on a running session** (`temporal_layers_live_switch`). One session runs five phases of
600 frames each: without layers, then on, off, on and off. "On" is 0.5 + 0.8, and "off" puts
both fractions back to 1.0. A second session is opened with layers on, for comparison. Cells
give the spend in Mbit/s, the dropped frames of 600 and the luma PSNR of the phase's end:

| size, target | as opened | on | off | on | off | opened layered |
| --- | --- | --- | --- | --- | --- | --- |
| 1920 × 1088, 16 Mbit/s | 5.88, 0, 53.27 | 5.37, 0, 53.31 | 5.87, 0, 53.29 | 5.36, 0, 53.31 | 5.87, 0, 53.29 | 6.95, 0, 53.23 |
| 1920 × 1088, 3 Mbit/s | 3.00, 18, 45.53 | 2.83, 0, 46.45 | 3.00, 0, 46.48 | 2.82, 0, 47.38 | 3.00, 0, 47.07 | 2.79, 36, 44.38 |
| 3024 × 1968, 32 Mbit/s | 11.48, 0, 53.30 | 12.69, 0, 53.29 | 11.47, 0, 53.30 | 12.68, 0, 53.29 | 11.47, 0, 53.30 | 13.96, 0, 53.04 |
| 3024 × 1968, 6 Mbit/s | 6.11, 50, 48.19 | 5.75, 0, 47.77 | 6.07, 0, 48.16 | 5.61, 0, 47.94 | 6.10, 0, 48.54 | 5.46, 68, 42.27 |

Switched on a running session, layers cost −9 % to +11 % of the bytes where the rate has room,
at the same picture. Where the rate binds, the luma moved −0.6 to +0.9 dB against the phase on
either side, and no layered phase dropped a frame. The drops in the first column are the
session's start. Layer-1 frames are small on a running session (5.5 KB against 16.9 KB base
frames at 1080p 16 Mbit/s), where the opened-layered session makes them 11.7 KB. Leaving out
all 600 marked frames of the toggled 1080p streams decoded every other frame to the same
picture as the whole stream (2400 and 2382 frames, none failed, none differed).

**How long a session must run first** (`temporal_layers_when_switched_on`, 1920 × 1088, layers
on after N frames, then 600 frames measured):

| on after | 16 Mbit/s: spent, dropped, luma | 3 Mbit/s: spent, dropped, luma |
| --- | --- | --- |
| 0 (before the keyframe) | 6.95, 0, 53.23 | 2.79, 36, 44.38 |
| 1 | 16.03, 0, 45.63 | 2.94, 41, 29.08 |
| 10 | 6.95, 0, 53.14 | 2.93, 20, 30.06 |
| 60 | 5.39, 0, 53.30 | 2.85, 0, 44.72 |
| 300 | 5.37, 0, 53.30 | 2.83, 0, 46.46 |

Switched on within the first ten frames, the session behaves as one opened layered, or far
worse: after the keyframe alone it lost 7.6 dB with room and 17 dB where the rate binds, for all
600 frames. After 60 frames it has room again but has not recovered from its start where the
rate binds. After 300 it matches the running session above. The worker waits for 300 frames
(`LAYERS_AFTER_FRAMES`).

**Each codec the worker opens** (`temporal_layers_by_codec`, 1920 × 1088, one session per row
through five phases of 150 frames: as opened, on, the frame fraction off with the bit fraction
left at 0.8, both off, and on again; spent in Mbit/s and luma PSNR, every layered phase marked
75 of 150 frames and dropped none):

| session, target | as opened | on | off, bits at 0.8 | off, both 1.0 | on again |
| --- | --- | --- | --- | --- | --- |
| HEVC 4:2:0, 16 Mbit/s | 5.90, 53.27 | 5.38, 53.31 | 5.88, 53.29 | 5.88, 53.29 | 5.37, 53.31 |
| HEVC 4:2:0, 4 Mbit/s | 3.76, 50.41 (16 dropped) | 3.76, 50.94 | 4.01, 51.29 | 4.00, 51.29 | 3.72, 51.39 |
| HEVC 4:4:4 10-bit, 16 Mbit/s | 9.18, 55.34 | 8.01, 55.42 | 8.94, 55.41 | 8.93, 55.42 | 7.99, 55.47 |
| HEVC 4:4:4 10-bit, 4 Mbit/s | 3.69, 39.08 (20 dropped) | 3.64, 39.95 | 4.00, 40.05 | 4.00, 40.67 | 3.66, 40.86 |
| H.264 4:2:0, 16 Mbit/s | 5.97, 49.37 | 5.26, 49.35 | 5.73, 49.37 | 5.73, 49.37 | 5.26, 49.35 |
| H.264 4:2:0, 4 Mbit/s | 3.72, 38.90 (15 dropped) | 3.60, 41.71 | 4.01, 43.38 | 4.00, 43.63 | 3.88, 42.37 |

All three sessions code 0.5. HEVC in either chroma costs at most 0.35 dB where the rate binds
(0.6 dB in the table above).
H.264 costs 1.3–1.7 dB there, and nothing where the rate has room. Layered sessions spend
under a binding target (3.60–3.88 of 4 Mbit/s), so spend reads as room exactly when layers are
on: it cannot say whether they cost picture. Putting the bit fraction back to 1.0 as well as the
frame fraction was worth up to 0.6 dB on 4:4:4 at 4 Mbit/s and 0.25 dB on H.264, and nothing
with room.

**The encoder's error report** (`mean_squared_error_report`, one session per row, 16 Mbit/s,
alternated twice). `CalculateMeanSquaredError` costs nothing measurable. One frame in flight
at 1080p took 6.05–6.17 ms without it and 6.09–6.19 ms with it; on the beat, 6.23–6.30 against
6.28–6.29. At 3024 × 1968 it took 15.13–15.29 against 15.21–15.23 ms. Set before the first
frame, every sample carried the error (240 of 240). Toggled on a running session, the set
returns 0 and no sample carries it (0 of 60 in every phase), so it is set at open and stays on.

## 2026-09-29 — temporal layers under clumped loss

A simulation, so any machine gives the same numbers. `layers_under_burst_loss` in
`crates/slopty-media/tests/burst_loss.rs` runs the link of "two parity fragments on small
frames" above (Gilbert loss, 10 ms each way, NACKs, parity, refreshes), three seeds × 60 s,
10 800 frames a case. With layers, base frames are 1.378× and layer-1 frames 0.450× the mean
frame without layers: the 1080p sizes of layers switched on a running session, as the worker
switches them ("temporal layers switched on a live session" above). The first cut used the
sizes of a session opened layered (1.407× and 0.958×). Off is the stream as it is; unflagged
has layers but the receiver does not know which frames it may skip; flagged sets `DISCARDABLE` and
`PREV_DISCARDABLE` and the reassembler skips a layer-1 frame it cannot repair. Cells are
off / unflagged / flagged; the count in brackets is the unflagged refreshes that a layer-1
frame's loss started.

```sh
cargo nextest run -p slopty-media --release --run-ignored only -E 'test(layers_under_burst_loss)' --no-capture
```

| scene | loss, run | waited a round trip | refresh episodes (a layer-1 loss) | still | skipped | wire Mbit/s |
| --- | --- | --- | --- | --- | --- | --- |
| still | 0 % | 0 / 0 / 0 | 0 / 0 (0) / 0 | 0.0 / 0.0 / 0.0 s | 0 | 1.98 / 1.83 / 1.83 |
| still | 1 %, 1 | 0 / 1 / 1 | 0 / 0 (0) / 0 | 0.0 / 0.0 / 0.0 s | 0 | 2.37 / 2.19 / 2.19 |
| still | 1 %, 2 | 78 / 64 / 53 | 0 / 0 (0) / 0 | 1.8 / 1.7 / 1.4 s | 16 | 2.26 / 2.09 / 2.11 |
| still | 1 %, 4 | 102 / 97 / 64 | 0 / 2 (0) / 0 | 2.3 / 2.7 / 2.0 s | 36 | 2.14 / 1.97 / 2.00 |
| still | 3 %, 1 | 3 / 1 / 1 | 0 / 0 (0) / 0 | 0.1 / 0.0 / 0.0 s | 0 | 2.46 / 2.28 / 2.28 |
| still | 3 %, 2 | 166 / 159 / 114 | 0 / 0 (0) / 0 | 4.3 / 4.2 / 3.3 s | 47 | 2.46 / 2.27 / 2.27 |
| still | 3 %, 4 | 254 / 249 / 152 | 3 / 5 (1) / 1 | 6.7 / 6.7 / 5.1 s | 99 | 2.39 / 2.21 / 2.23 |
| still | 5 %, 1 | 5 / 5 / 5 | 0 / 0 (0) / 0 | 0.1 / 0.1 / 0.1 s | 0 | 2.46 / 2.28 / 2.28 |
| still | 5 %, 2 | 245 / 236 / 176 | 1 / 1 (0) / 0 | 6.4 / 6.2 / 4.7 s | 62 | 2.48 / 2.31 / 2.31 |
| still | 5 %, 4 | 439 / 423 / 292 | 6 / 6 (2) / 5 | 11.2 / 11.3 / 9.6 s | 152 | 2.49 / 2.28 / 2.30 |
| still | 10 %, 1 | 34 / 39 / 39 | 0 / 0 (0) / 0 | 0.8 / 1.0 / 1.0 s | 0 | 2.46 / 2.29 / 2.29 |
| still | 10 %, 2 | 607 / 546 / 368 | 1 / 4 (1) / 2 | 15.4 / 14.6 / 11.2 s | 188 | 2.52 / 2.35 / 2.34 |
| still | 10 %, 4 | 867 / 871 / 561 | 21 / 13 (5) / 13 | 22.6 / 22.4 / 18.7 s | 281 | 2.57 / 2.39 / 2.38 |
| scroll | 0 % | 0 / 0 / 0 | 0 / 0 (0) / 0 | 0.0 / 0.0 / 0.0 s | 0 | 7.40 / 6.88 / 6.88 |
| scroll | 1 %, 1 | 6 / 1 / 1 | 0 / 0 (0) / 0 | 0.1 / 0.0 / 0.0 s | 0 | 7.95 / 7.39 / 7.39 |
| scroll | 1 %, 2 | 180 / 153 / 117 | 0 / 0 (0) / 0 | 4.2 / 3.5 / 2.9 s | 32 | 7.92 / 7.37 / 7.38 |
| scroll | 1 %, 4 | 255 / 229 / 162 | 0 / 0 (0) / 0 | 5.5 / 5.0 / 4.2 s | 68 | 7.83 / 7.27 / 7.29 |
| scroll | 3 %, 1 | 50 / 26 / 23 | 0 / 0 (0) / 0 | 1.2 / 0.6 / 0.6 s | 4 | 8.08 / 7.58 / 7.58 |
| scroll | 3 %, 2 | 518 / 400 / 242 | 1 / 0 (0) / 0 | 11.6 / 9.2 / 6.3 s | 141 | 8.13 / 7.63 / 7.67 |
| scroll | 3 %, 4 | 649 / 575 / 391 | 0 / 0 (0) / 0 | 13.7 / 12.5 / 9.5 s | 148 | 8.12 / 7.60 / 7.64 |
| scroll | 5 %, 1 | 103 / 54 / 35 | 0 / 0 (0) / 0 | 2.5 / 1.3 / 1.0 s | 15 | 8.29 / 7.78 / 7.79 |
| scroll | 5 %, 2 | 785 / 594 / 355 | 2 / 0 (0) / 0 | 17.5 / 13.4 / 9.5 s | 227 | 8.36 / 7.85 / 7.90 |
| scroll | 5 %, 4 | 1072 / 933 / 563 | 3 / 0 (0) / 0 | 21.5 / 19.4 / 14.1 s | 277 | 8.32 / 7.81 / 7.91 |
| scroll | 10 %, 1 | 245 / 149 / 70 | 0 / 0 (0) / 0 | 5.8 / 3.5 / 2.2 s | 83 | 8.87 / 8.29 / 8.30 |
| scroll | 10 %, 2 | 1313 / 1060 / 504 | 7 / 4 (4) / 0 | 27.6 / 22.6 / 13.4 s | 470 | 8.97 / 8.40 / 8.47 |
| scroll | 10 %, 4 | 1950 / 1591 / 943 | 24 / 15 (7) / 7 | 38.8 / 33.0 / 23.2 s | 545 | 8.94 / 8.43 / 8.53 |

Over every case: 69 refresh episodes off, 50 unflagged (20 of them, 40 %, a layer-1 frame's
loss that a receiver that knew could have skipped), 28 flagged. Flagged, the picture stands
still 13–51 % less at 1–10 % loss in runs of 2 and 4, and the scrolling screen at 10 % in runs
of 4 goes from 24 refreshes to 7. The wire carries 7–9 % less with layers, since layer-1 frames
of a running session are small; the first cut, with the opened session's sizes, paid 15–18 %
more. A stream without the bits is untouched: `parity_under_burst_loss` prints the same
numbers, digit for digit, before and after the reassembler learned to skip (the simulation
draws its random frame sizes only with layers on).

## 2026-09-29 — a still picture refined

Mac Studio M1 Max, macOS 27.0, load average 8.4–8.9 (other sessions building), run from `/tmp`
under `nice`. `still_picture_refinement` codes 30 frames of text scrolling 16 rows a frame, then
12 more of the last picture unchanged, on the worker's session at 1920 × 1088 with
`CalculateMeanSquaredError` on, and decodes each frame for its PSNR against the source. The
still frames are stamped as the worker stamps refinements: two periods apart, each a period
before it is sent (`Refine::stamp`). The encoder's own error comes back on each sample
(`kVTSampleAttachmentKey_QualityMetrics`). The session is deterministic: a rerun at load 4
gave the same bytes and PSNR to the digit.

```sh
cargo test -p slopty-codec --test chroma444 --no-run
mkdir -p /tmp/slopty-rev && cp target/debug/deps/chroma444-<hash> /tmp/slopty-rev/chroma444 && cd /tmp/slopty-rev
nice ./chroma444 --ignored --nocapture --exact tests::still_picture_refinement
nice ./chroma444 --ignored --nocapture --exact tests::a_change_after_a_refinement
```

Luma PSNR at the last moving frame, then after 1, 2, 4 and 8 frames of the still picture, and
the best of the 12 (4:4:4 rows are luma of the 10-bit stream read at 8 bits). "Policy" is where
`Refine`'s plateau rule stops on the encoder's error series, and what it sent until then; the
probe codes 12, so "12" means it would have gone on:

| stream | target | at the stop | + 1 / 2 / 4 / 8 | best, reached at | over the frame's share | policy | refinement encode p50 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 4:2:0 | 4 Mbit/s | 33.0 dB | 33.6 / 33.6 / 34.4 / 37.9 | 42.6 at 12 | 5 of 12 | 12, 131 KB, 42.6 dB | 6.26 ms |
| 4:2:0 | 8 Mbit/s | 48.5 dB | 49.1 / 50.5 / 51.6 / 52.9 | 53.3, within 0.3 dB at 9 | 4 of 12 | 12, 153 KB, 53.3 dB | 6.57 ms |
| 4:2:0 | 16 Mbit/s | 53.4 dB | 53.5 / 53.5 / 53.6 / 53.6 | 53.6 at 1 | 0 | 4, 5.8 KB, 53.6 dB | 6.42 ms |
| 4:4:4 | 8 Mbit/s | 35.8 dB | 37.7 / 38.1 / 40.3 / 44.2 | 47.7 at 12 | 7 of 12 | 12, 249 KB, 47.7 dB | 6.59 ms |
| 4:4:4 | 16 Mbit/s | 52.2 dB | 52.6 / 53.4 / 54.3 / 55.0 | 55.3, within 0.3 dB at 8 | 0 | 11, 63 KB, 55.3 dB | 6.56 ms |
| 4:4:4 | 32 Mbit/s | 56.3 dB | 56.5 / 56.5 / 56.7 / 56.7 | 56.7 at 1 | 0 | 4, 2.9 KB, 56.7 dB | 6.33 ms |

4:4:4 chroma at 8 Mbit/s went from 36.7 to 42.8 dB in eight frames. At 4 Mbit/s the encoder
dropped the last moving frame and 1 of the 12 still frames; the policy counts a dropped frame
as sent and goes on blind. A still frame costs the encoder a whole frame's time (6.3–6.6 ms
against 6.5–7.2 moving). Refinement frames are small where the rate had room: 0.2–2 KB at 16
Mbit/s 4:2:0 and 32 Mbit/s 4:4:4, and 1.4–17 KB at 16 Mbit/s 4:4:4, which still gained 3 dB.
Where the rate binds, about every other one spends more than a frame's share (17–42 KB at 8 Mbit/s, where a 60th of the rate is
16.7 KB). The encoder's error, as PSNR, tracks the decoded luma within 1–2.7 dB on 4:2:0 and
runs 11–13 dB low on 4:4:4, where it is in 10-bit units, so the policy reads only its ratios.
With the first cut's stamps (one period apart, stamped when sent) the same streams gained
less: 4:4:4 at 8 Mbit/s reached 41.4 dB after eight frames, where two periods apart it reached
44.2.

**Input to glass while refining** (`input_to_glass_on_a_still_screen_while_refining` in
`crates/slopty-worker/src/screen/synthetic.rs`, ignored; load 2–7). The worker's whole path on
the drawn 1920 × 1080 display, at the real cadence: the canvas stands still and changes only on
a click, every 80–150 ms, so each click lands in the quiet where refinement runs. 20 s a run,
refinement off and on alternated twice, on loopback and on a 20 Mbit/s link with 5 ms each way
(`Wire` holds what the rate has not yet let leave, so a refinement's bytes are still queued
when a change comes).

```sh
cargo test -p slopty-worker --lib --no-run                # target/debug/deps/slopty_worker-<hash>
cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-rev/slopty_worker && cd /tmp/slopty-rev
TMPDIR=/tmp nice ./slopty_worker --ignored --exact screen::synthetic::tests::input_to_glass_on_a_still_screen_while_refining --nocapture
```

| link | refinement | input sent → present p50 / p95 / max | refinements in 20 s |
| --- | --- | --- | --- |
| loopback | off | 26.79 / 45.41 / 50.86 ms, 27.45 / 47.95 / 58.38 | 0 |
| loopback | on | 26.18 / 46.55 / 54.90 ms, 28.26 / 48.81 / 60.37 | 190, 190 |
| 20 Mbit/s, 5 ms | off | 44.23 / 57.08 / 67.29 ms, 47.06 / 56.91 / 68.53 | 0 |
| 20 Mbit/s, 5 ms | on | 45.83 / 54.36 / 68.16 ms, 47.01 / 54.90 / 57.18 | 189, 189 |

Refinement moves nothing past the spread between two runs of the same case. Every click was
seen (174 of 174), and no frame was dropped. The encoder was at 30 Mbit/s with room, so the
refinements were small (1.2 KiB a frame on the wire, against 0.8 without); a large refinement
ahead of a change on a slow link is the guard's case, tested in the worker
(`a_still_picture_is_refined_until_the_encoder_stops_gaining`).

**The change right after a refinement** (`a_change_after_a_refinement`). The text scrolls,
stops, is refined twice, and scrolls on; the first change is timed 1 ms after the second
refinement. Bytes and luma of the change, and the mean of the four frames after it:

| stream, 8 Mbit/s | before the change | the change | the next four |
| --- | --- | --- | --- |
| 4:2:0 | no refinement (a 100 ms pause) | 9.4 KB, 48.97 dB | 25.0 KB, 51.45 dB |
| | two refinements stamped when sent | 14.4 KB, 50.94 dB | 11.7 KB, 51.67 dB |
| | two refinements stamped a period early | 10.3 KB, 50.84 dB | 12.4 KB, 51.52 dB |
| 4:4:4 | no refinement | 59.7 KB, 38.42 dB | 21.9 KB, 41.13 dB |
| | stamped when sent | 15.8 KB, 39.73 dB | 9.3 KB, 40.87 dB |
| | stamped a period early | 24.3 KB, 38.91 dB | 10.5 KB, 40.12 dB |

A refinement leaves the change better and cheaper than no refinement: 1.9 dB better on 4:2:0,
0.5–1.3 dB and a third to a half of the bytes on 4:4:4. The encoder did not squeeze the change
after a refinement stamped 1 ms before it: it gave it more bytes than after one a period
before. The two stampings differ by 0.1 dB on 4:2:0 and 0.8 dB on 4:4:4, over one sequence.
The worker stamps a period early for a reason of its own: stamps only go forward, so a
refinement stamped when sent would push a capture taken just before it to a stamp past its
capture time.

## 2026-09-29 — stripes across the two encode engines

Mac Studio M1 Max (two `ave2` encode engines), macOS 27.0, run from `/tmp` under `nice`. The
load average is printed per row: 4.8–10.7 for the first table (other sessions building), 2.8–5.1
for the rest. `stripes_across_engines` in `crates/slopty-codec/tests/chroma444.rs` opens one
low-latency session per horizontal stripe, each set up as the worker's and fed its rows of the
same scrolling text. Each stripe runs on its own thread, and every stripe of a frame is
submitted at the same instant. The probe runs 40 frames one at a time, then 4 s on a 60 or 120
beat. A frame counts when its last stripe comes back. "One in flight" is submit → last stripe
back, one frame at a time. "Skew" is first → last stripe back in that run: near 0 when the
stripes ran side by side, near a stripe's encode when they took turns. "Late" is due → last
stripe back on the beat; "queues" means the beat outran the encoder and the lateness grew for
the whole run. Stripes are the nearest multiple of 64 rows to an even share. Each gets its share
of the target by the rows it shows, 32 Mbit/s (40 at 4K and 5K) for the whole unless stated.
PSNR is the luma of the last 8 decoded frames. "Seam" is the 8 rows each side of it, striped
against the same rows of the whole picture.

```sh
cargo test -p slopty-codec --test chroma444 --no-run      # target/debug/deps/chroma444-<hash>
mkdir -p /tmp/slopty-rev && cp target/debug/deps/chroma444-<hash> /tmp/slopty-rev/chroma444 && cd /tmp/slopty-rev
nice ./chroma444 --ignored --nocapture --exact tests::stripes_across_engines
SLOPTY_PROBE_OVERLAP=64 SLOPTY_PROBE_STRIPES=1,2 SLOPTY_PROBE_SIZES=3024x1968,3840x2160,5120x2880 nice ./chroma444 --ignored --nocapture --exact tests::stripes_across_engines
SLOPTY_PROBE_EXPECT=120 SLOPTY_PROBE_OVERLAP=64 SLOPTY_PROBE_STRIPES=1,2 SLOPTY_PROBE_SIZES=1920x1088,3024x1968 nice ./chroma444 --ignored --nocapture --exact tests::stripes_across_engines
SLOPTY_PROBE_RATE=6000000 SLOPTY_PROBE_OVERLAP=64 SLOPTY_PROBE_STRIPES=1,2 SLOPTY_PROBE_SIZES=3024x1968 nice ./chroma444 --ignored --nocapture --exact tests::stripes_across_engines
SLOPTY_PROBE_EXPECT=120 SLOPTY_PROBE_RATE=6000000 SLOPTY_PROBE_OVERLAP=64 SLOPTY_PROBE_STRIPES=1,2 SLOPTY_PROBE_SIZES=3024x1968 nice ./chroma444 --ignored --nocapture --exact tests::stripes_across_engines
SLOPTY_PROBE_RATE=8000000 SLOPTY_PROBE_OVERLAP=64 SLOPTY_PROBE_STRIPES=1,2 SLOPTY_PROBE_SIZES=3840x2160 nice ./chroma444 --ignored --nocapture --exact tests::stripes_across_engines
SLOPTY_PROBE_ENCODER=plain SLOPTY_PROBE_STRIPES=1,2 SLOPTY_PROBE_SIZES=3024x1968,3840x2160 nice ./chroma444 --ignored --nocapture --exact tests::stripes_across_engines
```

**One picture against two stripes** (no overlap; ms):

| size | beat | one in flight: 1 → 2 stripes | skew with 2 | late p50 on the beat: 1 → 2 | submitted a second: 1 → 2 | spent Mbit/s: 1 → 2 | PSNR: 1 → 2 | seam: 2 / whole |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1920 × 1088 | 60 | 6.43 → 7.86 | 3.27 | 8.80 → 10.99 | 60 → 60 | 5.75 → 15.21 | 53.95 → 53.62 | 50.12 / 53.26 |
| 1920 × 1088 | 120 | 6.48 → 7.51 | 3.30 | 11.32 → 7.28 | 118.5 → 120 | 7.98 → 19.06 | 53.55 → 53.35 | 50.28 / 52.63 |
| 3024 × 1968 | 60 | 15.34 → 15.96 | 7.70 | queues → 16.02 | 57.8 → 60.1 | 11.13 → 20.41 | 53.36 → 53.30 | 51.55 / 53.68 |
| 3024 × 1968 | 120 | 15.34 → 8.54 | 0.25 | queues → 19.09 | 65.2 → 118.3 | 13.90 → 25.63 | 53.23 → 53.24 | 51.68 / 53.29 |
| 3840 × 2160 | 60 | 20.55 → 10.99 | 0.03 | queues → 12.05 | 48.9 → 60.0 | 18.08 → 28.89 | 53.49 → 53.37 | 51.61 / 53.69 |
| 3840 × 2160 | 120 | 20.52 → 11.07 | 0.01 | queues → queues | 49.0 → 90.5 | 21.80 → 35.00 | 53.50 → 53.41 | 52.35 / 53.86 |
| 5120 × 2880 | 60 | 35.04 → 18.57 | 0.58 | queues → queues | 27.7 → 53.4 | 23.69 → 36.90 | 53.11 → 52.52 | 50.81 / 53.11 |
| 5120 × 2880 | 120 | 34.85 → 18.50 | 0.62 | queues → queues | 26.9 → 48.8 | 30.14 → 38.27 | 53.28 → 51.73 | 48.88 / 53.22 |

The 3024 × 1968 row at 120 keeps the beat (118.3 a second), 19 ms late at the median. On the
beat its stripes come back 11 ms apart at the median, where one at a time they come back 0.25
ms apart: on a 120 beat an engine is still busy with one stripe of the previous frame when the
next arrives. Four stripes are never better than two: 16.49 ms at 4K 60 (skew 10.5, two stripes
on each engine taking turns), 28.54 at 5K, 13.6–13.7 at 3024 × 1968, and 1–15 dB lost at the
seams.

**Where the driver puts a session.** At 1080p, and at 3024 × 1968 on a 60 beat, the two
stripes take turns: the skew is half the whole picture's encode, and the pair takes longer than
the whole. At 3024 × 1968 on a 120 beat, and at 4K and 5K on either beat, they run side by side.
The switch follows the load the sessions declare. With `ExpectedFrameRate` 120 on a 60 beat
(`SLOPTY_PROBE_EXPECT=120`, overlap 64), 3024 × 1968 went side by side: 8.70 ms one in flight
(skew 0.29) against 18.13 declared at 60 (skew 8.16), late p50 11.96 against 24.53, and the
same spend (23.26 against 23.24 Mbit/s). 1080p still took turns (7.62 ms, skew 3.45). The point
where it switches lies between 1080p halves at 120 and 3024 × 1968 halves at 60. The picture
that fits: the driver packs sessions onto one engine while one engine's pixel rate covers them
(about 400 megapixels a second, from 20.5 ms for a 4K frame). Neither
`RecommendedParallelizationLimit` nor `UsingHardwareAcceleratedVideoEncoder` answers on the
low-latency session (-12900 for both). The plain hardware session reads 2 and true.

**Rows past each seam** (`SLOPTY_PROBE_OVERLAP=64`: each stripe also codes the 64 rows beyond
each seam, only its own rows count, and its target is its share of the rows shown). Scrolled
content keeps its reference inside the stripe. The seam is then within 0.4 dB of the whole
picture's same rows: 53.45 / 53.68 dB at 3024 × 1968 60, 53.61 / 53.29 at 120, 53.52 / 53.69 at
4K 60, 54.20 / 53.86 at 4K 120, 52.90 / 53.11 and 52.85 / 53.22 at 5K, 52.43 / 52.79 and
52.67 / 52.63 at 1080p. Two stripes with the overlap took 11.5–12.8 ms at 4K, 21.2 at 5K and
8.7–9.5 at 3024 × 1968 declared at 120. The last is over a 120 period: on a 120 beat they fell
behind (88–100 a second), where without the overlap they kept it.

**Where the rate is the limit** (overlap 64; the whole picture 8 Mbit/s at 4K, 6 at 3024 ×
1968; the 3024 × 1968 rows are the control for the declared rate):

| size, declared | beat | one in flight: 1 → 2 | submitted: 1 → 2 | spent Mbit/s: 1 → 2 | PSNR: 1 → 2 |
| --- | --- | --- | --- | --- | --- |
| 3840 × 2160 | 60 | 20.62 → 11.51 | 43.5 → 60.0 | 8.03 → 8.02 | 43.52 → 36.65 |
| 3840 × 2160 | 120 | 21.58 → 11.46 | 44.4 → 74.4 | 8.02 → 8.02 | 41.32 → 34.72 |
| 3024 × 1968, declared 60 | 60 | 14.95 → 16.55 (skew 8.14) | 60.1 → 60.0 | 6.09 → 6.01 | 47.37 → 37.69 |
| 3024 × 1968, declared 120 | 60 | 15.28 → 8.91 (skew 0.30) | 60.1 → 60.1 | 6.26 → 5.99 | 47.60 → 37.76 |
| 3024 × 1968 | 120 | 15.40–15.49 → 8.74–8.79 | 57.0–62.9 → 91.9–104.3 | 6.01 → 6.00 | 44.98 → 36.45 |

Declaring 120 on a 60 beat changes neither the spend nor the picture (37.76 against 37.69 dB
striped, 47.60 against 47.37 whole) and halves the stripes' encode.

Stripes cost scrolling dearly. Scrolled text enters every coded region at its bottom edge,
where nothing can predict it. The whole picture has one such edge, while two stripes have
two, and that band is nearly all a scroll's bits. With room under the target they spent
1.3–2.6× the whole picture's bits for the same PSNR, from 5K down to 1080p. At a binding rate,
at the same spend, the stripes lost 7–10 dB.

**The plain hardware encoder** (no low-latency rate control, same properties otherwise):
whole-picture 4K took 17.7 ms at 60, against 20.5 on the low-latency session. It overspent its
target (79.6 Mbit/s against 40 with two stripes at 4K 120), and its two stripes at 4K 60 queued
to 145 ms. Not a candidate.

Ruling: `docs/decisions/video.md`, "Two stripes halve the encode at 3K and above, and cost a
scroll twice the bits".

## 2026-09-29 — UI frames on gpui-fast, retention on and off

Mac Studio M1 Max, macOS 27.0, the dev profile (optimized, with debuginfo), window 1280×800 at
1×, other sessions building in the same checkout (load average 6–13 in every run). Draw is the
frame-time probe's span in ms, p50 / p95 / p99 / max; each scenario runs 5 s. **(a)** twenty
flooding shells, the strip stepping column to column; **(b)** the same, the overview in and
out; **(j)** one shell typed into at 15 keys a second with nothing else on screen changing,
each echo drawn.

```sh
cargo xtask e2e smooth --filter 'test(twenty_streaming_shells_scroll_the_strip) | test(typing_draws_only_the_echo)'
GPUI_VIEW_RETENTION=0 cargo xtask e2e smooth --no-build --filter '<the same>'   # retention off
```

"Before" is the tree before the switch: the zed fork, the workspace rendering the strip and the
chrome through its own `Context`. "After" is gpui-fast `6d80f2d` with the strip and the chrome
built from a read of the workspace and the bodies' facts (`docs/ARCHITECTURE.md` §6, "Drawing
under retention"); the retention-off row is the same binary with the fork's view retention
turned off.

| run | (a) strip | (b) overview | (j) echo |
|---|---|---|---|
| before, zed fork | 1.8 / 3.0 / 3.7 / 3.8 | 1.8 / 3.2 / 5.0 / 16.7, 1 dropped | 1.6 / 3.7 / 4.2 / 4.2 |
| after, retention on | 1.1 / 2.1 / 2.3 / 2.3 | 1.3 / 3.0 / 4.0 / 18.4, 1 dropped | 0.3 / 0.4 / 0.5 / 0.5 |
| after, retention on, again | 1.1 / 2.1 / 2.3 / 2.5 | 1.4 / 3.3 / 5.1 / 16.4 | 0.4 / 1.1 / 1.5 / 1.5 |
| after, retention off | 1.3 / 3.1 / 5.6 / 6.5 | 1.6 / 3.7 / 5.6 / 17.3, 1 dropped | 0.4 / 0.5 / 0.7 / 0.7 |

- The echo costs a fifth of what it did: p50 1.6 → 0.3–0.4 ms, p95 3.7 → 0.4–1.1 ms. Most of
  that is not view retention, since the retention-off row keeps it too. It comes from the
  fork's other changes and from what an echo frame now builds; these runs do not tell the two
  apart.
- Retention itself shows on the strip: with it on, a frame of motion builds the strip and
  replays the chrome and every tile body it did not hand anything new. Retention off, p95 goes
  from 2.1 to 3.1 ms and p99 from 2.3 to 5.6 ms.
- The overview's single slow frame (16–18 ms, one per run, before and after, retention on or
  off) was not looked into here.
- Frames arrive every 16.6 ms in (a) and (b) and every 68–70 ms in (j) (one per key) in every
  run: nothing draws that did not change, and nothing that changed waited.
- `typing_is_timed_with_and_without_the_local_echo_on_the_mac` times out waiting for its
  echoes, before and after alike: the window is not the key window when the run starts.

Logs: `target/logs/frames-before-zed.log`, `target/logs/smooth-after-fast.log`,
`target/logs/smooth-after-fast-ab.log`.

**Frame CPU, headless, retention on and off** (2026-09-30, gpui-fast `d5216f9`, the same test
binary, load average 15–23). The window is GPUI's test window, so a number is the main thread's
work for one frame (build, layout, prepaint, paint into the scene) with no GPU and no display.
Three runs each, alternated; p50 / p95 in ms. The overview's springs run on the workspace's
clock, held and moved 16.7 ms a frame, so both modes draw the same 400 frames.

```sh
cargo test -p slopty-ui --lib --no-run      # target/debug/deps/slopty_ui-<hash>
for r in 1 2 3; do for ret in 1 0; do GPUI_VIEW_RETENTION=$ret target/debug/deps/slopty_ui-<hash> \
  --ignored --exact --nocapture --test-threads 1 \
  workspace::tests::chrome::measure_an_echo_frame_beside_the_chrome \
  workspace::tests::chrome::measure_a_frame_of_motion_beside_the_chrome; done; done
```

| frame | retention on | retention off |
|---|---|---|
| a neighbour's echo beside a focused 80 × 40 grid | 0.105 / 0.167, 0.105 / 0.210, 0.106 / 0.327 | 0.271 / 0.653, 0.276 / 0.543, 0.280 / 2.349 |
| the workspace notified, 60 shells + 60 notes | 0.900 / 1.388, 0.925 / 1.695, 0.947 / 5.263 | 0.853 / 1.458, 0.888 / 1.461, 0.922 / 5.467 |
| a frame of the overview's spring, the same crowd | 0.756 / 1.233, 0.886 / 1.956, 0.815 / 3.210 | 0.854 / 1.456, 0.872 / 1.500, 0.915 / 7.610 |

- An echo frame costs 0.105 ms with retention against 0.27–0.28 ms without: the terminal that
  echoed is built, and the focused grid beside it and the chrome are replayed.
- A frame of the spring saves 0.03–0.1 ms at p50. The fork replays a view only where it was
  drawn, so every tile the spring moves is built again at its new place; what retention keeps
  is the chrome and the tiles off the strip's moving part.
- The workspace's own notify is a wash (−0.05 to +0.07 ms): it builds the chrome and the strip
  either way.
- The two modes differ only in `GPUI_VIEW_RETENTION`. While the spring's measurement ran on
  the wall clock, its retention-off run did not finish in 30 minutes under this load; with the
  clock held both modes finish in under a second.

Log: `target/logs/ui-retention-ab.log`.

## 2026-09-30 — the fork owner's five retention changes, A/B

Mac Studio M1 Max, macOS 27.0, the dev profile, headless (GPUI's test window: build, layout,
prepaint and paint on the main thread, no GPU). Load average 28–46: a gate and other sessions
were building, so the p95 column is noise and only p50 is compared. 60 shells and 60 notes with
the navigator docked. "Before" is the same tree with only the change under test undone, built
as its own binary; the runs alternate, three of each.

```sh
cargo test -p slopty-ui --lib --no-run          # target/debug/deps/slopty_ui-<hash>, copied per variant
for r in 1 2 3; do for b in before after; do target/ab/$b --ignored --exact --nocapture \
  --test-threads 1 workspace::tests::chrome::measure_a_pointer_frame_beside_the_chrome \
  workspace::tests::chrome::measure_a_frame_of_motion_beside_the_chrome \
  workspace::tests::chrome::measure_the_keyboard_moving_beside_the_chrome \
  workspace::tests::chrome::measure_an_echo_frame_beside_the_chrome; done; done
```

| change | frame | before, p50 ms | after, p50 ms |
|---|---|---|---|
| a shell reads no pointer and updates only on news | the pointer crossing a dense shell, the strip built each frame | 0.323, 0.323, 0.317 (the shell built 420 times) | 0.236, 0.236, 0.237 (built once, on entering) |
| a spring's frame builds a shell once | a frame of the overview's spring | 0.932, 0.848, 0.830 | 0.509, 0.500, 0.502 |
| the focus moved in a build notifies the views it touches, not a refresh | the keyboard moving between two shells, both frames | 1.860, 1.860, 2.065 | 2.777, 1.840, 1.735 |
| the row cache shared with the element, not lent through the view | a frame of motion; the pointer frame; a neighbour's echo | 0.456 / 0.226 / 0.101, 0.449 / 0.222 / 0.102, 0.431 / 0.220 / 0.101 | 0.456 / 0.224 / 0.102, 0.441 / 0.222 / 0.102, 0.429 / 0.221 / 0.101 |

- The shell under a moving pointer was built with every frame: its element read the pointer in
  prepaint, and its move listener updated the view on every move. Both are gone, and the frame
  costs a quarter less.
- A frame of a spring costs 40 % less: each moving shell was built twice, the second time for
  the grid its element had measured moving or zooming.
- The two others are within the noise. The focus change still builds a shell the focus never
  touched: something in GPUI rebuilds it in prepaint, whether or not Slopty notifies it, so
  targeted notifies save nothing until the fork treats focus as a read it can track. The row
  cache lent through two view updates a frame costs nothing measurable. Neither landed.
- The other two recommendations (tile headers and readouts as views of their own, plain inner
  divs for element retention) can save at most the strip's own build when nothing moves:
  0.22 ms, the pointer frame above, and only on a header's change. In motion every tile is built
  again at its new place anyway. Not done.

Logs: `target/logs/ui-retention-ab2.log`, `target/logs/ui-rowcache-ab.log`.

## 2026-09-30 — a keystroke's parse in the file tile

Mac Studio M1 Max, macOS 27.0, load average 23–29 (other sessions building). The file tile's
editor colours a 2 000-line Rust file (`workspace.rs` repeated); one character is typed on row
1 000. Before, every pause in typing parsed the whole text again off the UI thread. Now each
line keeps the parser's state it starts in, and a parse runs from the edited line down to the
first line that starts in the state it started in before (`highlight::editor::reparse`). Three
runs:

```sh
cargo test -p slopty-ui --lib --no-run
target/debug/deps/slopty_ui-<hash> --ignored --nocapture --exact \
  highlight::editor::tests::timing_of_a_keystrokes_parse highlight::editor::tests::timing_of_a_frame_and_a_keystroke
```

| | run 1 | run 2 | run 3 |
|---|---|---|---|
| the whole text parsed (before) | 184 ms | 179 ms | 212 ms |
| the lines the edit reaches (1 row), lists copied and spliced | 0.31 ms | 0.81 ms | 0.28 ms |
| the first parse, a state kept per line | 236 ms | 218 ms | 268 ms |
| 60 rows styled for a frame | 43 µs | 43 µs | 44 µs |

A keystroke's parse is 220–650× cheaper. The first parse pays 20–30 % more than a parse that
keeps no states, once per file. It now starts as the text arrives: the editor hands a text set
whole over as an edit too, so the first colours used to wait out the 60 ms typing pause.

What the kept parse holds for the largest file still coloured (`COLOURED_BYTES`, 2 MiB of
Rust, 51 125 lines): the process's resident size after the parse less before it, the grammars
loaded first. A state is kept apart only when none of the last 64 kept is equal to it
(`RECENT_STATES`), so most lines share one.

```sh
target/debug/deps/slopty_ui-<hash> --ignored --nocapture --exact \
  highlight::editor::tests::memory_of_a_parse_kept
```

| states kept | resident | states held apart |
|---|---|---|
| one per line | 81.5 MB | 51 124 |
| shared with the last 16 | 30.4 MB | 4 873 |
| shared with the last 64 | 27.4 MB | 2 332 |

What remains is mostly the spans, one list per line. The first parse of the 2 000-line file
took 204–257 ms with sharing, as without it, and a keystroke's parse 0.23 ms.


## 2026-09-30 — ghostty `0538f7535`

The engine's `*_cost` series before and after moving libghostty-vt from ghostty `12752b2ac` to
`0538f7535` (fork `d1a57e4` → `29bbc6a`), then after the prompt-mark scanner rewrite that the
bump called for. Instructions per operation. mac-studio, release, libghostty-vt ReleaseFast,
other sessions building, one run each except the scanner column (three runs, which agreed to
within 12 instructions). The new `osc_write_cost` pushes output dense with OSCs (an `ls
--hyperlink` of 40 names per command, a title and the four prompt marks per command) through
the engine and through libghostty alone. It was added for this bump, and it has no budget
until `cargo xtask bench --filter osc_write_cost --update-budgets` records one.

```sh
SLOPTY_BENCH_OUT=$PWD/target/bench/ghostty-0538-after.jsonl \
  nice -n 10 cargo nextest run --release -p slopty-engine -p workspace-hack \
  --run-ignored only -E 'test(/_cost$/)' --no-capture --no-fail-fast
```

| series | before | after the bump | after the scanner |
| --- | --- | --- | --- |
| checkpoint `fill_engine` | 32812730 | 32873519 | |
| checkpoint `fill_raw_vt` | 27045924 | 27058352 | |
| checkpoint `format` | 45491682 | 45582781 | |
| checkpoint `replay_one_chunk` | 35257816 | 35300407 | |
| `frame_cost.take_frame` | 34349 | 34347 | |
| `scroll_frame_cost.200x60` | 5301887 | 5301863 | |
| `fetch_lines_cost.4096_rows` | 124857051 | 125036983 | |
| `search_cost.50000_lines.plain` | 101123221 | 101394370 | |
| `encode_cost.full_200x60` | 1084247 | 1084247 | |
| `osc_write_cost.per_osc_raw_vt` | 6872 | 6926 | 6932–6936 |
| `osc_write_cost.per_osc` | 7222 | 7267 | 7222–7234 |
| `osc_scan_cost.per_osc` | 172 | 171 | 113 |

The bump moved every series by less than 0.4 %, except the OSC path through libghostty. That
path grew by 54 instructions per OSC (0.8 %), which is the new unknown-OSC plumbing and the
CAN/SUB check at the end of every OSC. It is too small to patch in the fork. The scanner now
drops back to waiting for the next ESC as soon as an OSC cannot be a mark, where it used to
skip that OSC to its terminator with `memchr2`. That cut the scan from 171 to 113 instructions
per OSC (p50 19 → 15 ns). The engine's overhead over libghostty on OSC-dense output fell from
341 to about 290 instructions per OSC. Ruling: `docs/decisions/terminal.md`, "The prompt-mark
scanner reads OSCs as libghostty does".

Logs: `target/logs/ghostty-0538-before.log`, `target/logs/ghostty-0538-after.log`,
`target/logs/ghostty-0538-scanner.log`. Numbers: `target/bench/ghostty-0538-*.jsonl`.

## 2026-09-30 — Clipboard v2: copy to offer, and the two pastes

Mac Studio M1 Max, macOS 27.0, debug build, a worker and its client on loopback QUIC, a named
pasteboard (never the general one), other sessions building in the same checkout. Targets from
`.research/design-dragdrop-clipboard.md` §7.

```sh
cargo nextest run -p slopty-workerd --no-capture -E 'test(/reaches_a_watching|pbpaste_on_the_worker/)'
cargo nextest run -p slopty-client --no-capture -E 'test(/fetched_ahead_and_pastes/)'
cargo nextest run -p slopty-input --no-capture -E 'test(/unchanged_board/)'
```

| What | Result | Target |
| --- | --- | --- |
| Copy on the worker → offer at a watching client, 40 copies after one warm-up | p50 30.7 ms, p95 51.6 ms, max 52.7 ms (earlier runs of the day: p95 52–55, max 52–77) | p95 ≤ 50 ms poll + RTT/2 + 5 ms |
| Paste on the worker of the focused client's 100 kB copy, not held there: a second process reads the promise, which fetches it from the client, urgent | 2.1–4.3 ms over four runs | ≤ RTT + size ÷ goodput + 10 ms |
| Paste on the client of a 300 kB picture the client fetched ahead, 20 pastes | p50 5.75 µs, max 6 µs | ≤ 5 ms |
| One look at an unchanged pasteboard (`changeCount`), mean of 2000 | 1.5–4.5 µs over three runs, 0.003–0.009 % of a core at 20 looks a second | < 0.1 % of a core |
| Read of another process's 200 MB copy, capped at 1 MiB (the first read), then whole | capped 39–44 ms, whole 19–21 ms, over two runs | |

- Copy to offer sits where a 50 ms poll puts it: a copy lands at a uniform point in the period,
  so the median wait is half of it and the worst is the whole of it, plus a few milliseconds
  of read and send. The first copy on a cold pasteboard took about 1 s once; the test warms up
  before sampling, and that first read is not in the table.
- The lazy paste is the whole round trip a real app's paste makes on the worker: the reader's
  `dataForType:` to the pasteboard server, the server to the worker's data provider, an urgent
  `Fetch` to the client, the answer inline, and back.
- The look's cost is the calling thread's wall time, which bounds its CPU. The pasteboard
  server's share of a look and the poller's timer wake are not in it.
- The capped read (`a_capped_read_of_a_big_copy_copies_nothing`, `cargo nextest run -p
  slopty-input --no-capture -E 'test(/big_copy_copies_nothing/)'`) shows `dataForType:` brings
  the bytes over from the pasteboard server whatever is done with them: the first read of the
  copy, capped, took twice the second, whole one. So the cap saves the copy into this process
  and keeping it, not the transfer, which is why a poll reads no type but text and file URLs.
- After the review fixes of 2026-09-30 (the worker's board holding a copy from before it
  started, the client's copy 30 s old): copy to offer p50 30.0 ms, p95 51.1 ms, max 52.6 ms;
  the lazy paste 2.7 ms.

## 2026-09-30 — target/ under a budget; a test binary's directory, not the volume

Mac Studio M1 Max, macOS 27.0. Other sessions were building throughout (load average 5–20).
`/Volumes/Lacie` (external APFS, `noowners`) holds the repository, and the internal disk is APFS
with owners on. Rulings: `docs/decisions/tooling.md`, "target/ stays under a byte budget…" and
"A test binary's directory, not the volume…".

**What `target/` holds, by last use** (`cargo xtask prune --dry-run`, after the one-off
`--idle-hours 6` pass of 2026-09-29):

| used within | 1 h | 1–6 h | 6–24 h | 1–3 d | older |
|---|---|---|---|---|---|
| units and incremental caches | 37 GB | 66 GB | 26 GB | 0.9 GB | 1.2 GB |

`target/` was 117.5 GB: `debug` 85.5 GB and `gate` 36.4 GB (tests 14, clippy-ios 8.8,
clippy-host 4.9, rustdoc 2.4). A day's work is therefore about 130 GB, of which the gate's lanes
are 36 GB. The budget is 160 GB and the floor 50 GB.

**The first pass of the new prune**: 134 510 object files of earlier compiles deleted, 19.7 GB.
`debug/deps` went from 152 737 entries to 64 263, and the tests lane's `deps/` from 102 568 to
62 296. No unit was deleted, because the first pass starts the ledger. A pass takes 4.8 s to
sweep and 4.5 s to size `target/` when no lock is held. A pass that has to look at every unit's
objects (the first, or one after a ledger is lost) takes 26–53 s to sweep.

**A busy `debug`, later the same day.** The gate refused on the floor with `debug: busy,
skipped`. `target/debug/incremental` held 70 GB of caches untouched for over three hours, and
deleting them by hand freed 62 GB. The pass now deletes a busy directory's idle caches one
session at a time under rustc's session locks. A dry run with `target/debug/.cargo-lock` held
shared from another process, which is how cargo 1.98 holds it while it builds, and with the
pass not waiting (as the gate runs it), after the hand deletion:

| idle window | `debug` line of the report | sweep | size walk |
|---|---|---|---|
| 24 h | `debug: busy, units kept; 0 caches would be swept, 0.0 GB` | 2.1 s | 6.0 s |
| 3 h | `debug: busy, units kept; 94 caches would be swept, 3.4 GB` | 1.6 s | 3.4 s |

No session was held by a compile in either run. `target/` was 90–93 GB, of which `debug` was
76 GB, with 57 GB used in the last hour and 41–44 GB in the last six.

**One test binary, `slopty-codec`'s `hevc_encode_then_decode`**, wall time of the process:

| where the binary is | run 1 | run 2 | run 3 |
|---|---|---|---|
| gate's `deps/`, 102 568 entries (Lacie) | 5.74 s | 5.68 s | 5.10–6.50 s |
| a copy on the internal disk, small directory | 0.69 s | 0.55 s | 0.43 s |
| a copy on Lacie, small directory | 0.50 s | 0.50 s | 0.37 s |
| a hard link (same inode) on Lacie, small directory | 0.40 s | | |
| a copy on the internal disk beside 100 000 empty files | 6.17 s | 2.08 s | 2.04 s |
| gate's `deps/` after the prune, 62 296 entries | 4.76 s | 5.51 s | 3.42 s |
| the same binary through a hard link in a small directory | 0.55 s | 0.64 s | 0.65 s |

**The codec's suite under nextest** (48 tests, `Summary` time), fresh target dirs that differ
only in where they are and in how many extra files `deps/` holds:

| target dir | entries in `deps/` | runner | runs |
|---|---|---|---|
| Lacie | 1 092 | — | 2.9 s, 3.3 s |
| Lacie | 101 092 | — | 33.3 s, 25.3 s |
| internal | 1 092 | — | 3.1 s, 3.2 s, 2.7 s, 2.8 s |
| internal | 11 092 | — | 3.6 s |
| internal | 31 092 | — | 6.3 s |
| internal | 101 092 | — | 36.2 s, 25.1 s, 33.2 s |
| internal | 101 092 | `xtask test-runner` | 4.8 s, 5.4 s |
| internal | 1 092 | shell stand-in for the runner | 3.2 s, 3.3 s |

The slowest single test with 101 092 entries and no runner was 4.5–8.0 s, against 0.74 s with
1 092.

```sh
# one binary, from where it is and from a copy or link elsewhere (cwd: crates/slopty-codec)
/usr/bin/time -p <dir>/roundtrip-<hash> --exact tests::hevc_encode_then_decode
# the suite, in a fresh target dir, then with 100 000 extra entries in deps/
CARGO_TARGET_DIR=<dir> cargo nextest run -p slopty-codec -p workspace-hack --no-run
CARGO_TARGET_DIR=<dir> cargo nextest run -p slopty-codec
(cd <dir>/debug/deps && seq 1 100000 | sed 's/^/zzdummy-/' | xargs touch)
CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER="$PWD/target/xtask/debug/xtask-runner test-runner" \
  CARGO_TARGET_DIR=<dir> cargo nextest run -p slopty-codec
cargo xtask prune --dry-run
# the busy rows: hold target/debug/.cargo-lock shared (flock, LOCK_SH) from another process,
# then call prune::prune on target/ with wait: false, dry_run: true and the idle window, and
# print its report (a scratch #[ignore] test in xtask/src/prune/tests.rs, not kept)
```

## 2026-09-30 — first launch of a new binary; RealtimeSanitizer on the render callback

`cargo xtask doctor` from herdr (the chain was zsh ← Claude Code ← zsh ← herdr, and herdr was not
listed under Developer Tools). Each of three fresh binaries was timed on its first launch
against its second. Median cost of the first launch: **261 ms** a binary. The research note of
2026-09-29 had measured 0.24 s on an idle Mac, and 0.8–12 s while `syspolicyd` ran at 238 %
CPU under other sessions' builds. After the switch is on, `cargo xtask doctor` records the other
side of this number.

`cargo xtask deep sanitize realtime` (nightly 1.101.0 c1070d693, 2026-09-28): 49 tests passed,
one of them the child process that allocates in `render` and dies of RealtimeSanitizer's
report. The run took 37.7 s cold and 9.8 s warm. Without the probe the real callback trips
nothing, both in the ring's tests, which call it directly, and in `player_starts_and_drains` on
the device's I/O thread.

## 2026-09-30 — PNG decode in place, and ghostty `7d0734aa8`

A kitty `f=100` PNG of 1024 × 768 decoded through `PngDecoder`, before and after it began
decoding into libghostty's buffer and widening in place. Both columns run with the
libghostty-rs `Bytes` that zeroes its allocation. mac-studio, release, other sessions
building. Three runs each, twenty images a run. The samples cycle through 0–250.

```sh
SLOPTY_BENCH_OUT=$PWD/target/bench/png.jsonl nice -n 10 cargo nextest run --release \
  -p slopty-engine -p workspace-hack --run-ignored only -E 'test(png_decode_cost)' --no-capture
```

| series | instructions before | after | p50 before | after |
| --- | --- | --- | --- | --- |
| `png_decode_cost.rgb_1024x768` | 43973434–44028952 | 42615904–42683161 | 3.86–4.04 ms | 3.63–3.66 ms |
| `png_decode_cost.rgba_1024x768` | 11835831–11838166 | 10456912–10465515 | 1.14–1.19 ms | 0.95–0.96 ms |

One of the "after" RGB runs read 5.22 ms, one sample on a busy machine. The RGBA image gains
most because its old path copied every byte twice for nothing. Zeroing the allocation cost
about 190 000 instructions on the RGBA image (12.48 M → 12.68 M on the earlier sample
pattern), with no change in wall time. `png_decode_cost` has no budget until
`cargo xtask bench --filter png_decode_cost --update-budgets` records one.

After the carried ghostty commits (`0538f7535` → `7d0734aa8`, the alternate screen modes),
every engine `*_cost` series stayed within 1 % of the "after the bump" column of the entry
above, for example `checkpoint fill_raw_vt` 27003718, `osc_write_cost.per_osc_raw_vt` 6926
and `scroll_frame_cost.200x60` 5302025. The change adds one switch arm on a mode set, which is
not on the print path.

Logs: `target/logs/png-final-{old,new}.log`, `target/logs/ghostty-7d0734-after.log`.

## 2026-09-30 — BBR3 on app-limited video, the batched Apple datapath, 1232-byte packets

mac-studio, other sessions building throughout (load averages below). Rulings in
`docs/decisions/transport.md`, "BBR3 on app-limited video" and the entries after it. Scripts and
logs: `target/logs/bbr3-app/` (`run2.sh` → `all2.log`, `run3.sh` → `all3.log`).

### BBR3 against Cubic, simulated

`crates/slopty-net/tests/sim.rs` section (f): a 20 Mbit/s link, 2 ms each way, 100 ms of
queue. The worker streams 60 fps video (25 kB P-frames, a 130 kB keyframe every 60 frames) and
answers keys on the session stream. Eight seeds per controller, paused clock, so a run repeats
exactly. "Unpatched" is noq-proto without patches 11 to 13 (`vendor/noq-proto/SLOPTY.md`),
measured with a build switch that has since been removed.

```sh
SLOPTY_CC=bbr3 cargo nextest run -p slopty-net --release --test sim --run-ignored only \
  -E 'test(video_report)' --no-capture      # also bbr3-unbounded, cubic
```

| controller | keyframe p99 | P-frame p99 | echo p50 / p99 | ProbeRTT / Startup samples |
| --- | --- | --- | --- | --- |
| bbr3, unpatched | 139 ms | — | — | ProbeRTT met by keyframes |
| bbr3, patched | 55 ms | 32 ms | 9 / 10 ms | 0 / 0 |
| bbr3-unbounded, patched | 55 ms | 32 ms | 9 / 31 ms | 0 / 32 |
| cubic | 55 ms | 32 ms | 10 / 12 ms | — |

55 ms is the keyframe's own serialisation at 20 Mbit/s, so every patched arm is at the floor.
Unbounded, noq's window reached 50 to 75 kB against the bound's 22 kB, and the echo waited
behind it. With 1 and 3 ms of jitter (`SLOPTY_SIM_JITTER_US`) the echo p99 read 15 against 31
and 20 against 27 ms, bound against unbounded. In `VideoSim` (noq-proto's own test,
`video_through_one_bottleneck`), ProbeRTT entries fell from 9 to 0 and P-frame p99 from 96 to
48 ms.

### BBR3 against Cubic, real time

`echo_beside_flood` on the shaper (20 Mbit/s, 100 ms queue), bursts arm only, 8 interleaved
rounds of four configurations, every keyframe and echo pooled. Load averages 12 to 41.

```sh
target/logs/bbr3-app/run2.sh    # echo_bin2 is the echo_beside_flood test binary of this change
```

| config | keyframes | keyframe p50 / p90 / p99 | over 80 ms | echo p50 / p99 | echoes over 50 ms | lost |
| --- | --- | --- | --- | --- | --- | --- |
| bbr3, patched | 114 | 54.9 / 75.0 / 170.6 ms | 7.0 % | 9.0 / 47.8 ms | 0.82 % | 0 |
| bbr3, unpatched | 114 | 55.1 / 65.9 / 156.3 ms | 5.3 % | 9.0 / 51.9 ms | 1.05 % | 0 |
| bbr3-unbounded | 111 | 54.9 / 64.9 / 163.5 ms | 6.3 % | 8.9 / 49.5 ms | 0.98 % | 0 |
| cubic | 116 | 54.9 / 70.0 / 139.2 ms | 6.9 % | 9.4 / 52.3 ms | 1.09 % | 0 |

At this load the four do not differ: the tail is the machine's scheduling, not the
controller. The traces (`SLOPTY_PATH_TRACE_MS`) showed no ProbeRTT in the patched runs, and a
first pacing sample of 23.8 MB/s from the measured round trip. Unbounded spent about 2 s in
Startup. The simulation is the number of record for the controller.

### ACK bundling

`an_echo_carries_the_ack_of_its_key` (sim section (g)) counts the worker's packets per key: 2.02
without noq-proto patch 15, 1.06 with it. The client's stays 2.02, since a key has no reply
packet to ride.

### The batched Apple datapath

`datagram_send_cost` (worker, 300 frames of 64 datagrams of 1150 bytes over loopback), three
interleaved pairs at load 31 to 37:

```sh
cargo test -p slopty-workerd --release --bin slopty-worker --no-run
for i in 1 2 3; do for b in 0 1; do SLOPTY_BATCHED_UDP=$b <that binary> datagram_send_cost \
  --ignored --nocapture --test-threads 1; done; done
```

| run | plain: cpu per frame, one-way p50 | batched: cpu per frame, one-way p50 |
| --- | --- | --- |
| 1 | 2033 µs, 1144 µs | 1466 µs, 918 µs |
| 2 | 1900 µs, 1176 µs | 1633 µs, 1014 µs |
| 3 | 2200 µs, 1106 µs | 1600 µs, 1249 µs |

The batched path costs 23 % less CPU (2044 against 1566 µs per frame, means), and every
datagram arrived on both. One-way p99 (16 to 39 ms) followed the load on both paths.

The 53 ms round trip of 2026-09-04 is gone. `echo_beside_flood`'s clear-link echo over loopback,
four interleaved pairs at load 26 to 37 (`target/logs/bbr3-app/run3.sh`):

| | plain | batched |
| --- | --- | --- |
| clear-link echo p50, per run | 0.67, 0.69, 0.50, 1.06 ms | 0.64, 0.99, 0.57, 0.42 ms |
| bursts echo p50, per run | 9.39, 9.67, 8.96, 8.66 ms | 13.32, 8.98, 8.92, 8.96 ms |

That 50 ms belonged to iroh's socket layer, which is gone, not to `sendmsg_x`.
`cargo nextest run -p slopty-net -p slopty-client` passed on both paths.

### The receive buffer

`keyframe_report` in `crates/slopty-net/src/endpoint.rs`: 1.5 MB of 1232-byte datagrams (a 5K
keyframe at 4:4:4) sent over loopback to a socket nobody reads, the case of a receiver task
that is late.

```sh
SLOPTY_UDP_RCVBUF=0 cargo test -p slopty-net --lib keyframe_report -- --ignored --nocapture
cargo test -p slopty-net --lib keyframe_report -- --ignored --nocapture
```

| `SO_RCVBUF` | held |
| --- | --- |
| 786 896 B (macOS default) | 623 of 1 217 |
| 4 MiB | 1 217 of 1 217 |

### 1232-byte packets

The largest datagram at the handshake, and the MTU reached, over loopback
(`a_full_media_datagram_fits_from_the_first_packet`, with the old configuration put back for
the "before" row):

| | first `max_datagram_size` | MTU |
| --- | --- | --- |
| before: 1200 initial, discovery to 1252 | 1178 B | 1252 after 2.4 ms of probing |
| after: 1232 initial and ceiling, 1200 minimum | 1210 B | 1232 from the first packet |

A media datagram is up to 1200 bytes. Before, it did not fit until discovery finished, and it
never fitted on an IPv6 tailnet path, where the 1252 probe (1300 bytes of IPv6 packet) cannot
cross a 1280 TUN.

A path narrower than 1232 (an IPv4 MTU of 1240, 1212 bytes of payload), simulated, eight seeds:
256 kB from the worker on a uni stream.

```sh
cargo nextest run -p slopty-net --test sim -E 'test(narrower)' --no-capture
```

| minimum MTU | 256 kB took | MTU after | black holes detected |
| --- | --- | --- | --- |
| 1232 (the minimum at the ceiling) | 6.4 s | 1232 | 0 |
| 1200 | 136 ms, every seed | 1200 | 1 |

### The shipped socket and partial sends

`a_round_trip_over_the_shipped_socket_takes_a_millisecond_not_fifty` (`tests/loopback.rs`): 200
one-byte keys and echoes on one stream through `endpoint::bind`, batched path and 4 MiB receive
buffer. Median 0.12 ms, worst 0.33 ms (load about 10).

`loopback_short_sends` (`src/udp.rs`, ignored): 100 000 ten-datagram transmits of 1200 bytes
through Slopty's sender to a socket nobody reads, three runs. Only the first transmit waited
for room, while tokio learned the socket was writable, and none went out in part. The same
flood straight through noq-udp's `try_send_partial` (1 000 000 datagrams, `SO_SNDBUF` default,
70 kB and 300 kB) gave no short send and no `EWOULDBLOCK`.

```sh
cargo test -p slopty-net --release --lib loopback_short_sends -- --ignored --nocapture
```

`no_datagram_size_blocks_for_good_at_the_send_buffer_edge` (`vendor/noq-udp`) sends every size
from `SO_SNDBUF` − 64 to `SO_SNDBUF` + 1 with an ECN `cmsg`. With the size check removed it fails
at 65 616 bytes against a 65 631-byte buffer with `EWOULDBLOCK`, which is the kernel bug.

### Sizing a bundled ACK

What noq-proto patch 15 spends per ack-eliciting packet while an ACK is pending, to decide
whether the ACK fits: a release loop of 2 000 000 ACK frames per row, load about 9.

| ranges | encode into a `Vec` and take its length | `AckEncoder::size` |
| --- | --- | --- |
| 1 | 26.7 ns | 3.5 ns |
| 3 | 51.7 ns | 5.5 ns |
| 8 | 99.1 ns | 10.2 ns |

The loop was a scratch test and is not kept. `ack_size_is_its_encoded_length` (a proptest)
holds the computed size equal to the encoded one.

## 2026-09-30 — frames read a row's cells at once (libghostty-rs PR stack #83–#95)

Mac Studio M1 Max, macOS 27.0, `cargo xtask bench` (release, retired instructions per
operation, median), other sessions building throughout. The engine's frame build read every
cell of a dirty row through the render state's cell iterator, one call into libghostty per
field (`next`, `raw_cell`, `semantic_content`, `has_styling`, `wide`, `has_text`,
`graphemes_utf8`). Now it takes the row's raw cells in one read (`RowIteration::cells_raw`,
upstream #85) and decodes each packed cell in Rust from the layout libghostty publishes in
its type manifest (`CellLayout`, fork `32963c5`..`fe69e05`). The cell iterator is positioned
(`select`) only for a styled cell's style and a cluster's codepoints. `read_line`, which
serves `FetchLines`, decodes its cells the same way.

```sh
cargo xtask bench --filter frame_cost
cargo xtask bench --filter fetch_lines
cargo xtask bench --filter search
```

| series | before | raw cells, one getter per field | raw cells, decoded |
| --- | --- | --- | --- |
| `engine.frame_cost.take_frame` (a typed byte, 60×12) | 34 387 | 24 969 | **21 197 (−38 %)** |
| `engine.scroll_frame_cost.80x24` (an Enter at a bottom prompt) | 881 613 | 611 314 | **493 884 (−44 %)** |
| `engine.scroll_frame_cost.200x60` | 5 301 725 | 3 659 592 | **2 942 736 (−44 %)** |
| `engine.fetch_lines_cost.4096_rows` | 124 790 604 | — | **103 177 530 (−17 %)** |

Wall p50 of the 200×60 scroll frame went from 333 µs to 158 µs. The frame's bulk cursor read
(`Snapshot::cursor`, one call for four) is in the last column too.

- Each getter costs about 65–80 instructions, the call and the Rust `Result` around it. A
  release loop over 80 cells (a scratch example in the fork, not kept) read two fields in
  10.8–15.3 ns a cell through the getters and all seven in 3.0 ns decoded.
- The decoded cell went through `CellLayout::linked()` per cell at first (a `OnceLock` read and
  seven layout loads, 23 % of the scroll frame in a samply profile at 20 kHz); the layout is
  now looked up once per frame and per fetched row, and the shifts carry no overflow check
  (`wrapping_shr`, every `lsb` is below 64 once the layout is read). That step is
  597 603 → 493 884 on the 80×24 scroll.
- What the profile leaves: `build_frame` itself 35 %, `Line::eq` (comparing the row with the
  one the viewers hold, `slopty-grid`) 16 %, `blank_line` 5 %, `Runs::finish` 3 %.

**A scrolled row that reads as its line is kept, not rebuilt.** libghostty marks every row
dirty when the viewport's pin moves, which is every line of output at the bottom, and clears
the page's own row dirty bits as it does, so the engine built every row of the screen again
and compared each with the line the viewers held (`Line::eq` above). Now the record keeps,
beside each held line, the raw cells and the two row flags it was built from (`Prints`, one
allocation for the whole screen, overwritten in place while nothing scrolls). A dirty row whose
raw cells and flags equal its print, and whose styles still resolve to the held line's (a
freed style id can be taken by another style, leaving a cell's bits as they were), keeps the
held line; rows with links, clusters, placeholders or a mark the engine forced are always
built.

| series | before | after |
| --- | --- | --- |
| `engine.scroll_frame_cost.80x24` | 493 884 | **123 681 (−75 %)** |
| `engine.scroll_frame_cost.200x60` | 2 942 736 | **364 303 (−88 %)** |
| `engine.frame_cost.take_frame` | 21 197 | 21 877 (+3 %) |

Against the start of the day the scroll frame is −86 % and −93 % (881 613 and 5 301 725). The
keystroke's frame pays for comparing and printing its one dirty row. Wall p50 of the 200×60
scroll: 26 µs. The allocation budgets hold (`tests/allocs.rs`): the spare print buffer is sized
during typing frames, not in the frame that scrolls.

**Search's formatter streamed** (upstream #91, `Formatter::format` into a `Vec`): the plain
text of the history is written into the string's own buffer instead of into a libghostty
allocation copied out, and blank rows no longer need the out-of-memory workaround.

| series | before | after |
| --- | --- | --- |
| `engine.search_cost.1000_lines.format` | 3 000 073 | 2 697 389 (−10 %) |
| `engine.search_cost.10000_lines.format` | 30 086 990 | 27 124 250 (−10 %) |
| `engine.search_cost.50000_lines.format` | 152 761 532 | 137 358 987 (−10 %) |

The plain and regex series, whose code did not change, moved by 1.5 % or less.

**libghostty's binary snapshot against the VT checkpoint** (not adopted, see
docs/decisions/terminal.md): 80×24 with 10 000 lines of history filled as `checkpoint_cost`
fills it, best of five in a release example in the fork (not kept):

| | VT formatter / replay | binary snapshot |
| --- | --- | --- |
| write | 1 902 µs, 688 611 bytes | 580 µs, 2 069 729 bytes |
| restore into a fresh terminal | 2 259 µs | 2 628 µs |

## 2026-09-30 — Annex B against length-prefixed on the wire

Release, mac-studio, instructions per access unit (`slopty_testkit::bench`, 500 samples; wall
p50 in brackets). `access_unit_framing_cost` (`crates/slopty-codec/src/decoder.rs`) splits the
client's path from a reassembled access unit to the sample buffer VideoToolbox takes, on the
synthetic 1 MB keyframe of `access_unit_conversion_cost` and on the worker's own session's
output for scrolling text: its keyframe and its median P-frame at 1080p (16 Mbit/s) and 4K (40
Mbit/s). Today's path is `annexb_parse` (the `memchr` scan for start codes) then `annexb_block`
(the units written length-prefixed into a block CoreMedia allocates). Carried length-prefixed,
as VideoToolbox writes it, the path is `lp_parse` (a walk over the lengths) and either `lp_copy`
(the picture copied once into a CoreMedia block) or `lp_wrap` (the received bytes wrapped in a
block with a custom block source that holds them, no copy).

```sh
cargo test -p slopty-codec --lib --release --no-run    # target/release/deps/slopty_codec-<hash>
cp target/release/deps/slopty_codec-<hash> /tmp/codec_lib && cd /tmp && nice ./codec_lib --ignored --nocapture framing_cost
```

| access unit | bytes | annexb_parse | annexb_block | lp_parse | lp_copy | lp_wrap |
| --- | --- | --- | --- | --- | --- | --- |
| synthetic keyframe | 1 000 151 | 814 429 (34.5 µs) | 193 509 (21.4 µs) | 601 | 193 832 (23.6 µs) | 5 101 (0.3 µs) |
| 1080p keyframe | 442 781 | 361 316 (15.4 µs) | 88 890 (8.3 µs) | 587 | 88 874 (8.0 µs) | 5 029 (0.3 µs) |
| 1080p P-frame | 8 708 | 7 669 (0.3 µs) | 6 612 (0.5 µs) | 0 | 6 596 (0.5 µs) | 5 029 (0.3 µs) |
| 4K keyframe | 1 383 552 | 1 125 758 (48.5 µs) | 265 531 (29.3 µs) | 371 | 265 130 (31.0 µs) | 4 500 (0.3 µs) |
| 4K P-frame | 22 632 | 18 475 (0.8 µs) | 8 667 (0.7 µs) | 0 | 8 601 (0.7 µs) | 4 475 (0.3 µs) |

The scan is 80 % of today's cost on a real keyframe (0.8 instructions a byte; real HEVC has no
more start-code lookalikes than the synthetic bytes), the copy the other 20 %. A length walk
costs nothing measurable, and wrapping the received bytes costs the same 4.5–5 k instructions
whatever the size: CoreMedia's two objects. A 4K keyframe goes from 1.39 M instructions (78 µs)
to 4.5 k (0.3 µs) on the client's stream task, ahead of the decode that restarts a stream after
loss; a P-frame from 14–27 k to 4.5 k. Ruling: `docs/decisions/video.md`, "HEVC travels
length-prefixed"; this bench went with the Annex B code.

## 2026-09-30 — Opus concealment: ropus against fade-replay, and repetition

Mac Studio M1 Max, macOS 27.0.1, load average 3.0–3.6 (an earlier run at 9.5–10.5 gave the same
picture). A scratch crate outside the repository (`/tmp/slopty-opus-eval`, not kept in the tree:
adopting `ropus` would change `Cargo.lock`) pulls in `slopty-codec` by path. It encodes with the
shipped `OpusEncoder` (48 kHz stereo, 10 ms, 96 kbit/s; every packet came out CELT-only, TOC
config 30), drops packets on a seeded trace, and builds the played timeline as the client's
`play_audio` does: the stand-in goes in before the packet that arrived, and a gap over
`MAX_CONCEALED` packets stays silent. Each method is scored against a clean decode by the same
decoder. Signals: 30.1 s of `say` speech (Daniel and Samantha) and 20 s of synthetic music
(three harmonic voices with vibrato and noise).

```sh
cd /tmp/slopty-opus-eval/data && say -v Daniel -o d.aiff "<text>" && say -v Samantha -o s.aiff "<text>"
for f in d s; do afconvert -f WAVE -d LEF32@48000 -c 2 $f.aiff $f.wav; done
cd /tmp && CARGO_TARGET_DIR=/tmp/slopty-opus-eval/target nice cargo build --release --manifest-path /tmp/slopty-opus-eval/Cargo.toml
nice /tmp/slopty-opus-eval/target/release/slopty-opus-eval   # EVAL_ONLY=speech|music for one signal
```

At 50‰ random loss and at bursty loss of about 50‰ (Gilbert–Elliott, mean burst 2.3). SNR and
segmental SNR over the lost segments (segLost) in dB; LSD is the log-spectral distance over the
lost segments, lower is better; E is their energy against the clean decode; the jumps are the
mean |sample step| entering and leaving a gap, where the clean signal's step is 0.0096 (speech)
and 0.0053 (music).

| signal, loss | method | SNR | segLost | LSD | E | jump in / out |
| --- | --- | --- | --- | --- | --- | --- |
| speech 50‰ | Apple decode + fade-replay (today) | 9.35 | -1.11 | 6.25 | -5.2 | 0.102 / 0.078 |
| | ropus decode + ropus PLC | 11.05 | 2.03 | 5.63 | -1.1 | 0.0096 / 0.0086 |
| | ropus decode + fade-replay | 9.35 | -1.11 | 6.25 | -5.2 | 0.102 / 0.078 |
| | repetition + Apple + fade-replay | 22.73 | 33.01 | 0.26 | -0.3 | 0.013 / 0.0095 |
| speech bursty | today | 9.66 | -1.41 | 7.99 | -5.5 | 0.158 / 0.116 |
| | ropus + PLC | 11.00 | 0.13 | 7.41 | -2.9 | 0.026 / 0.022 |
| | repetition + Apple + fade-replay | 11.60 | 6.66 | 5.08 | | |
| music 50‰ | today | 10.04 | -1.22 | 9.97 | -4.9 | 0.052 / 0.035 |
| | ropus + PLC | 8.92 | -2.50 | 9.41 | -1.1 | 0.0049 / 0.0042 |
| | repetition + Apple + fade-replay | 27.43 | 33.53 | 0.26 | | |
| music bursty | today | 11.60 | -1.25 | 12.48 | -4.7 | 0.051 / 0.037 |
| | ropus + PLC | 11.04 | -1.96 | 11.19 | -1.7 | 0.0040 / 0.0031 |

At 20‰ and 100‰ the pattern holds: speech SNR 13.62 → 17.66 and 6.61 → 8.51 with ropus PLC,
music 14.82 → 13.87 and 7.37 → 6.76.

- The decoder alone changes nothing: ropus's clean decode of Apple's packets is 74.98 dB
  (speech) and 70.94 dB (music) from Apple's at a 120-frame lag (Apple drops Opus's pre-skip, its
  first packet is 720 samples), and ropus with fade-replay scores exactly as today.
- Fade-replay steps 8–15× the clean step at both edges of every gap: it restarts the old packet
  and fades to zero before the next begins at full level. ropus PLC stays at the clean step and
  keeps the level (-1 dB against -5). Waveform SNR, which rewards the quieter fade when the phase
  is wrong, favours PLC on speech by 1.3–4 dB and fade-replay on the music by 0.6–1.1 dB.
- Decode per 10 ms packet, p50 / p95: Apple 26.9 / 31.0 µs, ropus 22.3–22.9 / 25.7–28.1 µs
  (0.83×), ropus PLC 53.3–59.9 / 62.0–69.8 µs (2.1–2.2× Apple's decode, paid only on a lost
  packet; about 26 µs deeper into a burst).
- Allocations per packet after warm-up (a counting global allocator): Apple 0; ropus float decode
  35.1 (speech) and 16.2 (music), PLC 24.7. `decode_float` allocates a fresh `vec![0i16; …]` on
  every call (`ropus-0.12.18/src/opus/decoder.rs`, near line 1402).
- In-band FEC does not exist at this operating point: LBRR is SILK's, and the encoder turns it off
  in CELT-only mode (`ropus/src/opus/encoder.rs:1027`, `if use_in_band_fec == 0 ||
  packet_loss_perc == 0 || mode == MODE_CELT_ONLY { return 0; }`); the decoder falls back to PLC
  on a CELT-only packet (`opus/decoder.rs:1252–1257`).
- Repetition (each audio datagram also carrying the previous packet) recovers 97–98 % of the
  losses at 20–50‰ random, 91–92 % at 100‰, 44–46 % in bursts. It costs +119 B (speech) and
  +110 B (music) a datagram with a 2-byte length, about +89 % of the audio datagram and +88–95
  kbit/s.

Ruling: `docs/decisions/audio.md`, "Opus loss concealment stays fade-replay until a decoder
conceals without allocating".

## 2026-09-30 — several streams on the encode engines

Mac Studio M1 Max (two `ave2` encode engines), macOS 27.0, from `/tmp` under `nice`. The load
average is printed per row: other sessions were building, and some ran encoders of their own,
which share the engines. `concurrent_sessions` in `crates/slopty-codec/tests/chroma444.rs` opens
N of the worker's own sessions (`slopty_codec::Encoder`, 1080p unless stated, 16 Mbit/s), each on
its own thread feeding the scrolling text picture on a 60 beat behind the worker's one-frame
mailbox. Every beat falls on the same instants, as windows on one display are captured on its
refresh. Stream 0 is "the focused one". Encode is submit → packet, in ms, p50 / p95 / max, over
the frames after the first 10. `SLOPTY_PROBE_OPEN=sequential` opens the sessions one after
another, as a client opens tiles.

```sh
cargo test -p slopty-codec --test chroma444 --no-run      # target/debug/deps/chroma444-<hash>
cp target/debug/deps/chroma444-<hash> /tmp/slopty-conc/chroma444 && cd /tmp/slopty-conc
SLOPTY_PROBE_PLACE_OWN=1 SLOPTY_PROBE_SIZES=1920x1088 SLOPTY_PROBE_STREAMS=1,2,3,4,5,6,8 nice ./chroma444 --ignored --nocapture --exact tests::concurrent_sessions
SLOPTY_PROBE_PLACE_OWN=1 SLOPTY_PROBE_EXPECT=120 SLOPTY_PROBE_SIZES=1920x1088 SLOPTY_PROBE_STREAMS=1,2,3,4,5,6,8 nice ./chroma444 --ignored --nocapture --exact tests::concurrent_sessions
SLOPTY_PROBE_OPEN=sequential SLOPTY_PROBE_SIZES=1920x1088 SLOPTY_PROBE_STREAMS=1,2,3,4,5,6,8 nice ./chroma444 --ignored --nocapture --exact tests::concurrent_sessions
SLOPTY_PROBE_PLACE_OWN=1 SLOPTY_PROBE_SIZES=896x512,2560x1440,3840x2160 SLOPTY_PROBE_STREAMS=1,2,4,8 nice ./chroma444 --ignored --nocapture --exact tests::concurrent_sessions
cargo test -p slopty-worker --lib --no-run && cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-conc/worker_lib
cd /tmp/slopty-conc && SLOPTY_STREAMS=2,4 nice ./worker_lib --ignored --nocapture --exact screen::synthetic::tests::measure_concurrent_streams
```

The first two tables ran on the encoder as it was before this change: every session placed at
its own rate. `SLOPTY_PROBE_PLACE_OWN=1` gives that now: sessions made for 59, which
`placement_fps` leaves alone, and the declare knobs then act at once. The other knobs
(`SLOPTY_PROBE_EXPECT_AT`, `_THEN`, `_FOCUS_EXPECT`, `_BACKGROUND_FPS`, `_YIELD`, `_RATE`,
`_FPS`, `_PHASE`) are in the test's doc.

**Each session declaring its own rate (before), load 2.3–3.1:**

| streams | focused | all streams | frames a second each |
| --- | --- | --- | --- |
| 1 | 6.38 / 6.66 / 8.34 | 6.38 / 6.66 / 8.34 | 60 |
| 2 | 8.18 / 11.79 / 12.07 | 6.92 / 11.84 / 12.39 | 60 |
| 3 | 7.44 / 11.85 / 12.12 | 6.35 / 11.76 / 12.19 | 60 |
| 4 | 6.14 / 6.51 / 6.77 | 10.86 / 16.61 / 17.68 | 60 |
| 5 | 6.58 / 11.87 / 12.18 | 10.78 / 16.58 / 18.00 | 60 |
| 6 | 21.95 / 22.25 / 31.86 | 21.78 / 22.18 / 31.86 | 45.7 (others 51.3) |
| 8 | 22.26 / 22.44 / 25.28 | 22.25 / 22.44 / 51.66 | 45.1 (others 44.9) |

Two sessions at 60 take turns: the second back 5–6 ms after the first, one frame's encode, as
the stripes did at 1080p. Together they declare less than one engine's pixel rate, so the driver
puts them on one. From six on, both engines are full (six 6.4 ms frames a period is 38 ms of
work for 33 ms of two engines), and every stream falls to 45 frames a second.

**Every session declaring 120** (`SLOPTY_PROBE_EXPECT=120`, load 3.1–4.5):

| streams | focused | all streams | frames a second each |
| --- | --- | --- | --- |
| 1 | 6.53 / 6.72 / 7.47 | 6.53 / 6.72 / 7.47 | 60 |
| 2 | 6.53 / 7.32 / 16.92 | 6.53 / 7.25 / 17.54 | 60 |
| 3 | 10.37 / 12.03 / 12.47 | 6.48 / 11.99 / 12.47 | 60 |
| 4 | 9.96 / 12.06 / 12.21 | 7.74 / 12.05 / 12.28 | 60 |
| 5 | 12.34 / 16.52 / 16.73 | 12.34 / 16.74 / 19.96 | 60 |
| 6 | 16.67 / 17.46 / 25.78 | 16.67 / 17.47 / 77.37 | 59.5 |
| 8 | 28.43 / 31.11 / 35.33 | 21.24 / 30.12 / 79.46 | 34.4 (others 44.7) |

Two streams run side by side, one on each engine, and six keep 60 frames a second.

**When the rate places a session.** Two 1080p sessions opened one after the other, told their
rates at different moments (`SLOPTY_PROBE_EXPECT_AT`, `_THEN`, `_FOCUS_EXPECT`), focused p95:

| what the sessions were told | focused p95 | engines |
| --- | --- | --- |
| both 60 | 11.8 | shared |
| the second 120, set only before `PrepareToEncodeFrames` | 11.9 (2 runs) | shared |
| both 120 after the prepare, through the first frame | 6.6–7.3 (9 runs) | one each |
| both 120 after the prepare, 60 just before the first frame | 11.7 | shared |
| both 120 after the prepare, 60 after the first or the 12th frame | 6.7 | one each |
| both 60, then 120 from the 12th frame | 11.9 | shared |
| the first 60, the second 120 or 240 after the prepare | 6.6–7.8 in 7 runs, 10.2–12.2 in 9 | either |

The driver places a session, and fixes its set-up, from the rate last set between the prepare
and the first frame. The picture follows the same rate: a session placed at 120 codes the same
text at 55.43 dB where one placed at 60 codes it at 55.70, whatever it is told later. Only
sessions that all declare 120 were split every time.

**What declaring 120 costs a stream alone** (one session, 6 s, luma PSNR from the encoder's own
error, rate spent; own rate → 120):

| size, target | 60 a second | 30 | 15 |
| --- | --- | --- | --- |
| 1080p, 3 Mbit/s | 48.18 → 48.47 dB, 2.77 → 2.74 Mbit/s | 51.49 → 50.92, 2.00 → 2.04 | 52.12 → 50.29, 1.15 → 1.35 |
| 1080p, 8 Mbit/s | 54.86 → 54.26, 4.17 → 4.20 | 55.45 → 54.37, 2.14 → 2.38 | 56.79 → 54.66, 1.06 → 1.34 |
| 1080p, 16 Mbit/s | 55.70 → 55.43, 4.25 → 4.39 | | |
| 1440p, 16 Mbit/s | 56.08 → 55.68, 5.91 → 6.20 | | |
| 4K, 40 Mbit/s | 55.51 → 55.07, 11.39 → 12.24 | | |

Up to 0.6 dB and 4–7 % more bits at 60, all above 54 dB, and +0.3 dB where 3 Mbit/s binds;
0.6–2.1 dB at 30 and 15. So a session made for 60 or more declares 120, and one made for less
keeps its own rate.

**Other sizes** (each declaring its own rate; p95 of all streams): 896 × 512 windows pack onto one
engine even when declared at 120, and queue there: 2.87, 4.62, 8.27 and 17.54 ms for 1, 2, 4 and 8
(declared 120: 3.20, 4.73, 8.67, 13.32, and 8 keep 60 where they made 58.9). At 2560 × 1440, two
at 60 take turns (18.5 ms each, 54 frames a second); declared 120, 10.7 / 18.0 and 59. A 4K
session alone already declares more than one engine: one or two ran at 20.6 ms p50 and 46–48
frames a second either way.

**Favouring the focused stream** (every session declaring 120 unless stated, focused p95):
background streams that wait while the focused one has a frame in the encoder
(`SLOPTY_PROBE_YIELD=wait`) held it at 6.57 ms with two others and 11.60 with four; at 30 frames a
second the four others still gave 11.30–11.56. With four others at 60, two engines are full and
some frame is always ahead of the focused one's. These runs were at load 7–10, and their spread
between repeats was as wide as the differences.

**As shipped** (`placement_fps` in `crates/slopty-codec/src/encoder.rs`: a session made for 60
or more declares 120 from the prepare until its first frame, then its own rate). The probe with
sessions opened one after another, before (`SLOPTY_PROBE_PLACE_OWN=1`) against after, p95 of all
streams and frames a second:

| streams | before | after | load |
| --- | --- | --- | --- |
| 4 | 16.75 and 16.72 ms, 59 | 12.67 and 12.04 ms, 60 | 7–24 |
| 6 | 22.3–22.5 ms; focused 44–45, others 50–51 | 16.9 ms, 59.3–59.7 each | 39–53 |
| 7 | 28.0 ms; focused 35–36, others 43–44 | 22.5–22.6 ms; focused 44–59, others 47–52 | 39–53 |
| 8 | 28.2–29.7 ms; focused 50–59, others 42 | 22.6–22.9 ms, 44–45 each | 39–53 |

Eight 1080p streams at 60 ask for 51 ms of encoding each 16.7 ms period, more than two engines
have, so both ways fall to about 44 frames a second; after the change they share it evenly. Two
streams alone could not be settled in the probe at load 20–30: other sessions' encoders were on
the engines, and even one stream read 12–20 ms p95 in some runs.

The worker end to end (`measure_concurrent_streams` in
`crates/slopty-worker/src/screen/synthetic.rs`: N synthetic display streams through the real
`Pipeline`, about 1920 wide, 4 s each, two rounds each way, load 20–29), encode p95 per stream:

| streams | before | after |
| --- | --- | --- |
| 1 | 7.37; 6.58 | 7.51; 6.86 |
| 2 | 10.68, 12.55; 9.20, 10.91 | 6.54, 6.58; 6.70, 6.74 |
| 4 | 18.72, 18.63, 17.14, 8.90; 16.53, 16.53, 6.47, 16.84 | 11.55, 12.78, 9.21, 12.93; 10.81, 8.40, 6.78, 6.82 |
| 8 | three at 22.3 and 58 fps, five at 28.2 and 36; all 22.9 and 42–45 fps | one at 22.4 and 58.5 fps, seven at 41.3 and 23; all 22.7 and 31–49 fps |

The first eight-stream round after the change put seven streams on one engine; the probe's three
rounds above never did, so it reads as the other encoders on the machine at the time, but it is
recorded as seen. What a stream costs the worker's CPU, capture drawing included, is about 0.05
of a core: 0.06–0.10, 0.11–0.15, 0.21–0.28 and 0.35–0.40 cores for 1, 2, 4 and 8. Nothing else
is shared or duplicated at a cost that shows: each stream owns its session, its packetizer and
its pacer, and the engines are the one shared resource.

## 2026-09-30 — HEVC travels length-prefixed

Release, mac-studio, load 8–15, instructions per call (`slopty_testkit::bench`; wall p50 in
brackets, noisy with other sessions' encoders on the machine). Before is the Annex B build, after
the length-prefixed one; both binaries were built from the same tree around the change and run
twice, interleaved, with the same tests. `decode_cost` (`crates/slopty-codec/src/decoder.rs`)
hands `Decoder::decode` a live session and the worker's own session's output for scrolling text:
its keyframe, which is what restarts a stream after loss, and a P-frame. The submit is timed and
the decode awaited outside it, so the figure is what the client's stream task spends.

```sh
cargo test -p slopty-codec --lib --release --no-run    # target/release/deps/slopty_codec-<hash>
cp target/release/deps/slopty_codec-<hash> /tmp/codec_lib && cd /tmp
nice ./codec_lib --ignored --nocapture decode_cost
nice ./codec_lib --ignored --nocapture packet_conversion_cost access_unit_conversion_cost
```

| call | before | after |
| --- | --- | --- |
| decode, 1080p keyframe (442 781 B) | 724 860–736 655 | 284 543–285 886 |
| decode, 4K keyframe (1 383 552 B) | 1 838 080–1 850 817 | 445 382–446 699 |
| decode, 1080p P-frame (2 654 B) | 159 495–160 677 | 156 487–158 033 |
| decode, 4K P-frame (16 472 B) | 205 110–207 813 | 185 816–188 749 |
| to a sample buffer, 1 MB synthetic keyframe | 1 510 550 (56 µs) | 12 300 (0.4 µs) |
| host, VideoToolbox's output to a packet, 300 KB keyframe | 58 847 (5.5 µs) | 58 449 (5.6 µs) |
| host, the same, 62 KB P-frame | 14 214 (1.2 µs) | 13 762 (1.2 µs) |

A 4K keyframe after loss costs the client a quarter of what it did: 1.4 M instructions less, the
scan for start codes and the copy into CoreMedia's block. What is left, about 150 k for any
P-frame and 280–450 k for a keyframe, is VideoToolbox's own submit. The host is unchanged: it
used to rewrite each length to a start code in place, and now walks the same lengths to check
them. `access_unit_framing_cost`, which compared the two framings on one build (the section
above), went with the Annex B code; `decode_cost` measures the end result.

## 2026-09-30 — the focused stream when the engines are full (first cut)

Release, mac-studio, load 15–30, with other sessions' encoders on the machine.
`measure_concurrent_streams` (`crates/slopty-worker/src/screen/synthetic.rs`) opens 7 and 8
drawn display streams at 1920 × 1080, 60 frames a second, with the canvases now beating on a
shared grid (`first_beat` in `slopty-capture`), and waits 3 s for the encoder watches to settle.
It measures for 6 s. `SLOPTY_FOCUSED=1` has a client focus stream 0 (`screen/engines.rs`).
There are two rounds each way. Encode p50 / p95 in ms and frames a second:

| streams | nobody focused (stream 0; the rest) | stream 0 focused (stream 0; the rest) |
| --- | --- | --- |
| 7 | 16.6 / 16.9, 59.7; three at 59.7, four at 43–45 — and 22.1 / 22.6, 44.8 | 13.5 / 23.7, 60.0; 30 each (one 15) |
| 8 | 21.0 / 22.6, 46.8; 37–46 — and 22.0 / 22.5, 45.2; 44–46 | 6.7 / 17.9, 60.0; 15 each — and 12.1 / 17.9, 60.0; 30 each |

The focused stream keeps 60 frames a second where it had 45–47, and its median encode falls
back towards its time alone. Two things are left, and neither is shipped yet. First, the
background drops a second rung when the next window still reads short (15 frames a second at 8
streams), so the give-way needs a settling period. Second, the focused p95 stays at 18–24 ms
because background frames go into the engines on the same refresh; the fix is to hold them while
a focused frame is in the submit.

## 2026-09-30 — keeping an unsaved edit

What hot exit (`docs/decisions/ui.md`, "An unsaved edit survives a quit or a crash") costs.
Release, mac-studio's internal SSD, other sessions building (load 15–30). Medians of 21 rounds
on the UI thread and of 7–40 rounds for the writes.

```sh
cargo nextest run -p slopty-ui --release --run-ignored only timing_of_a_backup --no-capture
cargo nextest run -p slopty-client --release --run-ignored only put_cost --no-capture
```

**On the UI thread**, a 16 MiB file tile with an unsaved edit (two runs):

| what a pass does per tile | time |
|---|---|
| `FileView::text()`, the whole text copied out (the first version read this) | 5047 / 4820 µs |
| `FileView::backup_mark()`, when its backup is current | 0.042 µs |
| `FileView::backup()`, the editor's `ropey::Rope` shared, when it is behind | 0.042 µs |
| off the UI thread: the rope read out to a string (`Backup::unsaved`) | 3052 / 2757 µs |

Sharing the rope keeps the copy off the frame: 5 ms is a third of a 60 Hz frame, paid on every
pass while typing. The pass now reads nothing per byte on the UI thread.

**Writing one backup**, ms. The first run measured the store as it was (`File::sync_all` before
the rename, which is `F_FULLFSYNC` on Apple platforms). The second measured it on
`slopty_platform::fs::replace` (`F_BARRIERFSYNC`, then a plain `fsync` of the directory). The
three middle columns write the same JSON three ways, whatever the store does:

| size | JSON | + `F_FULLFSYNC` | + barrier, directory `fsync` | unsynced | `Store::put` before → after |
|---|---|---|---|---|---|
| 4 KiB | 0.00 | 5.06 / 4.91 | 1.48 / 1.03 | 0.16 / 0.25 | 5.05 → 1.06 |
| 1 MiB | 0.46 / 0.47 | 5.47 / 5.11 | 2.87 / 1.54 | 1.01 / 0.52 | → 2.11 |
| 16 MiB | 8.48 / 27.12 | 9.85 / 12.29 | 6.58 / 4.86 | 3.92 / 4.10 | → 14.66 |

(The first run's `Store::put` at 1 MiB and 16 MiB is missing: the harness numbered its changes
from 1 for each size, so the store took them as overtaken and wrote nothing. Fixed for the
second run. The 16 MiB JSON's 8 → 27 ms is the machine's load between runs.)

What changed:
- The store writes through `slopty_platform::fs::replace`, the codebase's one way to replace a
  file (`docs/decisions/platform.md`, "One way to replace a file, one home"). A small backup
  costs 1.1 ms where it cost 5.1, and no longer flushes the drive's cache up to five times a
  second while someone types.
- A tile brings on a pass only when its `backup_mark` moves. Before, every notify of a file
  tile did, and the caret's blink notifies twice a second: a headless test counted 121 calls
  to start a pass in 60 s of a tile doing nothing.
- The passes space out for a large edit, so the backups write at most 8 MiB a second
  (`KEEP_BYTES_PER_SEC`). A 16 MiB file typed into was rewritten every 200 ms, 80 MiB a second
  of writes and about 20 ms of encoding and writing a pass; it is now kept every 2 s. A file
  up to 1.6 MiB is still kept every 200 ms.

## 2026-09-30 — spawning a shell: the fork against std's

What a new tile's shell costs from `Pty::spawn` to the program running, after the review of
`crates/slopty-pty/src/spawn.rs` (`docs/decisions/terminal.md`, "A shell starts from our own
fork"). Four legs, interleaved in each of 300 rounds and run on `/usr/bin/true`: all of
`Pty::spawn`; its preparing alone (the same spawn failing on a NUL in the last variable, just
before the fork); `Launch::spawn` alone (fork, child steps, `execve`); and std's `Command` with
the `pre_exec` the spawn used to take. Measured on mac-studio, main `c08d4c14` plus the
uncommitted tree, release, under `nice -n 10`, with a load average of 26 to 35 from other
builds:

```
cargo nextest run -p slopty-pty --release --run-ignored only spawn_cost --no-capture
SLOPTY_SPAWN_BALLAST_MB=2048 cargo nextest run -p slopty-pty --release --run-ignored only spawn_cost --no-capture
```

| run | `Pty::spawn` p50 / p95 / p99 | preparing | `Launch::spawn` | std `Command` |
|---|---|---|---|---|
| 0 MiB | 1492 / 12876 / 27381 µs | 149 / 300 / 429 | 1382 / 9456 / 29128 | 1356 / 11988 / 19002 |
| 0 MiB | 1906 / 15275 / 27742 | 154 / 331 / 491 | 1889 / 14987 / 25272 | 1831 / 12739 / 27075 |
| 0 MiB (before the prep leg) | 2125 / 14623 / 27104 | | 1763 / 16528 / 37744 | 1690 / 15509 / 36624 |
| 2 GiB ballast | 1879 / 13286 / 26003 | | 1683 / 14289 / 32745 | 1639 / 11672 / 31474 |

- The fork is std's within 2 to 4 % at p50 in every run, and the p95s swap places between
  runs. The load sets the tails, not the code. Closing inherited descriptors
  (`proc_pidinfo` and a `close` each) is not visible at this resolution.
- With 2 GiB of touched heap the fork was no slower, so a daemon holding many sessions'
  backlogs pays nothing extra to spawn.
- Preparing is 150 µs p50 in the loop. Warm, one step at a time (2000 calls each, a probe that
  was not kept), it is 42 µs: 28 µs for `Launch::new` turning 182 variables into C strings,
  12 µs to copy the environment, 1.8 µs for `default_term`'s `stat`s, under 1 µs for the
  rest. The rest of the 150 is the first touches after the previous fork (copy-on-write
  faults and cold caches). Caching the prepared environment would save at most about 40 µs
  of a 1.4 to 1.9 ms spawn, under the noise here, and would need invalidating whenever the
  daemon's environment changes. Not taken.
- Faster ways to fork, weighed: `posix_spawn` (388 µs against 919 µs p50 when measured
  before) cannot make the tty a controlling terminal on macOS. `vfork` in Rust has no
  `returns_twice`, and libc marks it deprecated on Linux for the memory corruption that
  follows (rust-lang/libc#1596). A shell's own start-up (tens of ms for zsh with rc files)
  dwarfs the spawn anyway.

After the second review, the parent learns of the `exec` through kqueue (`EVFILT_PROC`, a
shared page for the report) instead of a pipe's end of file. The same command, twice, at load
averages of 21 to 23:

| run | `Pty::spawn` p50 / p95 / p99 | preparing | `Launch::spawn` | std `Command` |
|---|---|---|---|---|
| 0 MiB | 1646 / 9185 / 38600 µs | 141 / 311 / 402 | 1595 / 8541 / 30495 | 1581 / 8871 / 26896 |
| 0 MiB, quieter | 1174 / 1426 / 2068 | 120 / 147 / 168 | 1072 / 1303 / 1570 | 1058 / 1329 / 4859 |

The quieter run is the cleanest pair so far: the fork and exec are std's within 1.3 % at p50
and 2 % faster at p95, so a `kqueue`, one `kevent` to watch, one `proc_pidinfo` and one to
wait cost what a `pipe` and two `fcntl`s did.

## 2026-09-30 — Opus concealment: pitch repeat

Mac Studio M1 Max, macOS 27.0, load 15–35 (other sessions building; the scores do not depend
on it, the timings do). The harness of "Opus concealment: ropus against fade-replay, and
repetition" above, same signals, traces and scores, with two rows added: (f) Apple's decode with
the new `Conceal` (the last pitch period repeated, and the next packet faded in from its
continuation) and (g) the same with repetition. Row (a) is the fade-replay `Conceal` did before,
kept in the harness as a copy. The harness calls `Conceal::fill` and `Conceal::take` as the
client's `play_audio` does, and counts allocations with a counting global allocator.

```sh
cd /tmp && CARGO_TARGET_DIR=/tmp/slopty-opus-eval/target nice cargo build --release --manifest-path /tmp/slopty-opus-eval/Cargo.toml
nice /tmp/slopty-opus-eval/target/release/slopty-opus-eval
cargo nextest run -p slopty-codec --lib --run-ignored only --no-capture concealment_against_the_clean_decode
```

| signal, loss | method | SNR | segLost | LSD | E | jump in / out |
| --- | --- | --- | --- | --- | --- | --- |
| speech 20‰ | (a) fade-replay | 13.62 | -0.92 | 6.32 | -4.78 | 0.109 / 0.080 |
| | (b) ropus + PLC | 17.66 | 3.40 | 4.74 | -0.83 | 0.0074 / 0.0082 |
| | (f) pitch repeat | 17.56 | 5.10 | 5.21 | 0.04 | 0.029 / 0.0079 |
| | (g) repetition + pitch | 47.83 | 33.83 | 0.16 | 0.01 | 0.0077 / 0.0095 |
| speech 50‰ | (a) | 9.35 | -1.11 | 6.25 | -5.23 | 0.102 / 0.078 |
| | (b) | 11.05 | 2.03 | 5.63 | -1.09 | 0.0096 / 0.0086 |
| | (f) | 12.73 | 2.93 | 5.12 | -0.28 | 0.031 / 0.011 |
| | (g) | 26.45 | 33.23 | 0.21 | -0.16 | 0.012 / 0.0095 |
| speech bursty | (a) | 9.66 | -1.41 | 7.99 | -5.49 | 0.158 / 0.116 |
| | (b) | 11.00 | 0.13 | 7.41 | -2.92 | 0.026 / 0.022 |
| | (f) | 11.54 | 0.69 | 6.98 | -1.87 | 0.050 / 0.025 |
| | (g) | 13.62 | 7.91 | 4.42 | -1.32 | 0.025 / 0.011 |
| speech 100‰ | (a) | 6.61 | -1.16 | 6.49 | -5.26 | 0.094 / 0.072 |
| | (b) | 8.51 | 2.22 | 5.73 | -1.46 | 0.0081 / 0.0081 |
| | (f) | 9.53 | 2.67 | 5.56 | -0.47 | 0.034 / 0.0096 |
| | (g) | 19.89 | 30.20 | 0.81 | -0.17 | 0.011 / 0.0088 |
| music 20‰ | (a) | 14.82 | -0.84 | 9.89 | -5.10 | 0.043 / 0.030 |
| | (b) | 13.87 | -2.07 | 8.77 | -1.53 | 0.0057 / 0.0044 |
| | (f) | 13.62 | -2.89 | 8.63 | -0.71 | 0.046 / 0.0055 |
| | (g) | 27.38 | 33.20 | 0.28 | -0.10 | 0.0072 / 0.0071 |
| music 50‰ | (a) | 10.04 | -1.22 | 9.97 | -4.86 | 0.052 / 0.035 |
| | (b) | 8.92 | -2.50 | 9.41 | -1.08 | 0.0049 / 0.0042 |
| | (f) | 8.82 | -3.04 | 8.57 | -0.25 | 0.040 / 0.0054 |

The clean signal's own step is 0.0095 (speech) and 0.0053–0.0070 (music).

- On speech the pitch repeat matches ropus's concealment at 20‰ and beats it at 50‰, 100‰ and
  in bursts, and keeps the level better (-0.3 dB against -1.1 at 50‰). On the music both lose
  about 1.2 dB of waveform SNR to fade-replay while their spectra and levels are closer.
- Leaving a gap steps as the clean signal does, thanks to the fade into the next packet.
  Entering one still steps 3–5× the clean step (fade-replay: 8–15×): the repeat starts a period
  back and cannot touch what was already played.
- Per concealed packet: 22.5 µs p50, 23–26 µs p95 (Apple's decode is 27 µs), and no allocation
  after the first gap (0.00–0.03 a packet, the first gap's buffers).
- Repetition on top (g) is the best row on every speech trace. On the music it is 0.1–2 dB of
  waveform SNR under repetition over fade-replay (d), the same trade as without repetition, with
  a closer spectrum and level.
- The in-crate measurement on a 140 Hz voice-like tone and a chord, one packet in 20 lost plus
  pairs: over the gaps and the packet after them -1.3 → 3.4 dB and 0.0 → 2.9 dB; over the whole
  6.1 → 9.7 dB and 7.8 → 10.5 dB.

## 2026-09-30 — the focused stream when the engines are full

Release, mac-studio, load 18–56, with other sessions' encoders on the machine. Same harness as
the first cut above. After the others give way, the focused stream asks again only after 1 s and
two of its own windows (`SETTLE_US`, `SETTLE_WINDOWS`). This was set against a variant that also
held each background capture on its encode thread for up to 8 ms, until the focused capture of
the same refresh had gone into its encoder. The three modes ran interleaved, three rounds each,
4, 7 and 8 streams per mode per round. Stream 0's encode p95 in ms, its frames a second, and the
rest's frames a second:

```sh
cargo test -p slopty-worker --release --lib --no-run   # copy the test binary to /tmp/slopty-conc
SLOPTY_FOCUSED=1 SLOPTY_STREAMS=4,7,8 SLOPTY_GLASS_SECONDS=6 nice /tmp/slopty-conc/worker_lib \
  --ignored --nocapture --exact screen::synthetic::tests::measure_concurrent_streams
```

| streams | settle, and held captures | settle only (shipped) | nobody focused |
| --- | --- | --- | --- |
| 4 | 13.0, 14.9, 12.1; 58–59; the rest 30 once, else 57–59 | 13.2, 12.8, 12.2; 58–60; the rest 59–60 | 13.3, 13.7, 12.3; 59; the rest 58–60 |
| 7 | 18.0, 24.7, 28.7; 56–58; the rest 30, 19, 15 | 32.9, 20.1, 12.7; 57–59; the rest 15, 30, 30 | 29.8, 28.3, 22.8; 35–44; the rest 34–59 |
| 8 | 25.8, 17.9, 28.2; 56–59; the rest 15–30, 15–30, 15 | 18.2, 18.3, 40.0; 56–59; the rest 30, 30, 15 | 26.6, 17.3, 22.8; 44–60; the rest 35–59 |

- Focus keeps stream 0 at 56–60 frames a second with a p50 of 7–12 ms, where nobody focused
  gets it 35–44 and 22–28 ms at 7 streams.
- The p95 spread between rounds of one mode (12.7–32.9) is wider than the difference between
  the modes. Held captures waited 1.1–3.2 ms a frame and left the rest at 15–20 frames a second
  in five of six runs, against two of six with the settle alone. So only the settle ships.
- Under this load the focused p95 is not below 16.7 ms at 7–8 streams. The frames that queue in
  front of it are the rest's, coded at full size.

## 2026-09-30 — Deep checks widened, a stream soak, a loom model

mac-studio, load 20–30 from other sessions' builds and tests, all heavy work under `nice`. The
wall times are this loaded machine's and are kept as a scale, not a budget.

```
cargo xtask deep sanitize address          # the widened crate set, two runs (see below)
cargo xtask deep sanitize thread
cargo xtask deep loom                      # needs target/quality/codec-loom.patch applied
cargo xtask deep metal [--filter …]        # the app self-test under Metal validation
cargo xtask deep leaks                     # daemon and wire test binaries under leaks --atExit
cargo xtask soak --seconds 60 [--no-build] # now with one stream lane by default
```

**Sanitizers over every crate with `unsafe`.** `slopty-vdisplay`, `slopty-crash`,
`slopty-tailnet`, `slopty-input` and `slopty-ptyd` joined the sanitized set. Neither
sanitizer reported a memory error or a data race in any of them.
- The four new crates alone: 139 tests, 117 s under ASan and 96 s under TSan, builds included.
- The whole set under ASan: 680 tests in 72 s, plus 33 in `slopty-crash`'s own run. The lane
  took 272 s with a warm build.
- The whole set under TSan: 681 tests in 709 s, and 996 s for the lane.

What failed, and why:
- `slopty-crash`'s crash tests saw the child die of `SIGABRT` rather than the signal it raised.
  The crate chains to the handler it finds, and under a sanitizer that is the runtime's, which
  reports and aborts. The crate now runs apart with the runtime's handlers off
  (`handle_segv=0:…`), and the two signal assertions pass.
- Four of its tests still fail on frame names. With `debug = "line-tables-only"`, an inlined
  frame is named from DWARF alone, without its path (`child`, `{closure#0}`). The sanitizer
  builds inline `child` into its `FnOnce` shim, and the test's search for
  `crash::crash::child` then finds the shim at `function.rs:250`. The gate passes only because
  the test profile does not inline it. A release report names every inlined frame the same bare
  way.
- Five tests wait forever under TSan and pass under ASan, and are left out of the TSan run by
  name (`deep.rs`, `TSAN_CANNOT_RUN`). Each waits on a child through `tokio::process`. TSan
  holds the `SIGCHLD` handler until the thread calls a function it intercepts, and the runtime
  is parked in `kevent`, which it does not intercept. The child was seen `<defunct>`, never
  reaped, in all five.
- A few worker and pty tests missed their deadlines under TSan's slowdown and passed alone.
  One more fails alone too: `actor::a_viewer_whose_queue_was_dropped_gets_frames_again_once_it_reads`,
  "never shown" after 11.5 s. It is not classified.

**The loom model of the audio ring** (`audio::ring_model`, three scenarios, exhaustive with no
preemption bound): 3 passed in 61 s, 142 s with the build. Three planted bugs were each caught
within 10 ms: `write` published relaxed (a torn frame), the run word stored relaxed (the front
played unfaded), and the device's second look at `run` removed (the next run played unfaded after
a mute).

**Metal validation** (`deep metal`, the quick terminal, since removed, and a folder tile, a build
each time):

| run | wall of the two tests |
| --- | --- |
| plain | 13.6 s, 11.4 s |
| API and shader validation | 11.4 s |

The cost is within the noise. No API error asserted and no shader fault was reported. Warnings
per run, from gpui-fast's Metal renderer: 16 redundant buffer bindings (the same 48-byte
buffer bound again), 2 redundant texture bindings (the 1024 × 1024 A8 atlas), and 4 unused
fragment bindings at buffer index 1. On the longer run (seven tile tests) there were 1 750 and
449 of them.

**The stream soak** (`soak --seconds 60`: 1 536 fill cycles and a minute of load, beside one
lane streaming the drawn display and both windows in turn at scales 1.0, 0.75 and 0.5 for 3 s
each):

| | run 1 | run 2 |
| --- | --- | --- |
| streams / failed | 71 / 0 | 85 / 0 |
| frames decoded (per stream p50, of 180) | 11 663 (175) | 14 444 (173) |
| lost / decode errors / stalls | 0 / 0 / 0 | 0 / 0 / 0 |
| worker dropped / captured | 52 / 12 295 (4.2 ‰) | 49 / 15 054 (3.3 ‰) |
| first frame p50 / p95 / max | 111 / 148 / 346 ms | 127 / 324 / 444 ms |
| worker peak footprint | 146 MiB | 149 MiB |
| worker descriptors, threads before → after | 14 → 14, 18 → 17 | 14 → 14, 17 → 17 |
| `leaks` (server, ptyd, worker) | 0, 0, 0 | 0, 0, 0 |
| the whole soak | 275 s | 330 s |

Without streams the worker peaked at 104 MiB. A stream lane adds about 42 MiB of capture
surfaces and encoder pool, so the worker's peak budget grows by 64 MiB a lane
(`STREAM_PEAK_MIB`). Its footprint was back at 36–40 MiB after the load, with a falling slope.
A soak run earlier the same day stopped after 1 195 fill cycles on ptyd's
`open /dev/ptmx: Unknown error: -6`. Another session was churning PTYs at the time, and the
machine allows 511 (`kern.tty.ptmx_max`). A cycle that fails now ends the load but keeps the
evidence: samples, counts, `leaks` and the summary.

**Miri** (`deep miri`, unchanged): `slopty-predict`'s 23 unit tests pass in 385 s.
`editing::random_editing_never_shows_a_wrong_line` ran past 20 minutes under the interpreter
without finishing, so the nightly's Miri lane runs for hours as it stands.

**`leaks --atExit` on test binaries** (`deep leaks`): `slopty-net`'s 49 tests take 13 s under it,
against 5 s bare, with 0 leaks. The lane as shipped runs 35 test binaries of `slopty-net`,
`slopty-ptyd`, `slopty-media`, `slopty-engine`, `slopty-grid`, `slopty-server` and
`slopty-worker` in 224 s, with 0 leaks. Tests that drive a shell on a PTY cannot run under it
(`LEAKS_CANNOT_RUN`, and `slopty-pty` as a whole), for the reason `docs/TESTING.md` gives.

## 2026-09-30 — a stream's pictures on a layer of their own

Debug build (the e2e's), mac-studio, macOS 27.0, a 60 Hz Parsec virtual display that reports no
scan-out, other sessions building and testing (load 15–32). The app's tile shows the worker's
drawn display (756×492 HEVC at the tile's width) for 20 s. The presented handler is taken as
the glass (`GPUI_PRESENTED_AT_CALLBACK=1`, which the test passes on to the app). Two paths in one
build, interleaved: the picture on the stream's `VideoLayer`, put up from the decoder's thread,
against the picture drawn by GPUI's surface element in the view's frame and timed after that
frame's paint (the path before, kept behind a switch for the runs and then removed).

```sh
GPUI_PRESENTED_AT_CALLBACK=1 nice cargo xtask e2e smooth --filter 'test(drawn_frames_reach_the_glass_on_loopback)'
```

| run | arrival → glass p50 / p95 / max ms | present spacing | GPUI frames in 20 s |
| --- | --- | --- | --- |
| layer, round 1 | 0.97 / 1.19 / 9.77 | 16.51 ± 1.94 | 160 |
| surface, round 1 | 13.92 / 16.29 / 17.04 | 16.66 ± 0.68 | 1411 |
| layer, round 2 | 1.19 / 3.29 / 8.56 | 16.52 ± 1.96 | 159 |
| surface, round 2 | 12.18 / 14.18 / 14.45 | 16.66 ± 0.24 | 1426 |

- The layer takes 11–13 ms off p50 and 11–15 ms off p95. The window draws no frame for a
  picture: 160 frames in 20 s against about 1 420, each at 0.4 ms p50.
- The drawn display beats on this Mac's own display clock, so every picture arrives at one
  phase of the refresh, just before it. The layer puts it up on that refresh. GPUI's frame
  waits for the next display tick and misses it. A source on another Mac arrives at any phase,
  where the gain is the wait for the tick (half a refresh on average) and the frame itself.
- Presents are a little less evenly spaced on the layer (±1.9 ms against ±0.2–0.7): each goes
  up as it is decoded, not on the display's tick.
- Every picture decoded was put up (1300–1322), 0–7 were skipped, and nothing was lost.

## 2026-09-30 — a browser tile's page composed by the window

Debug build (the e2e's), mac-studio, macOS 27.0, a 60 Hz Parsec virtual display, other sessions
building and testing. One page tile (a static page the test serves on 127.0.0.1) beside five
shells printing as fast as they can, for 5 s per scenario: the palette opened and closed every
400 ms over the page, the strip moved column to column every 150 ms, and the overview opened and
closed every 400 ms. Before: the build from just before the change, where the page was an
`NSView` laid over the window, placed in a prepaint each frame and hidden (with a snapshot) under
the palette. After: the page on a gpui-fast native host. Three interleaved rounds each. CPU is
the `ps` time of the app, and of the WebKit processes that started with the run, over each 5 s
scenario.

```sh
nice cargo xtask e2e smooth --filter 'test(a_page_tile_beside_five_shells)'
# the before: the same test binary, SLOPTY_E2E_BIN_DIR at the earlier build's binaries
```

| scenario | draw p50 / p95 / p99 ms, before | after | app CPU s / 5 s, before | after | WebKit CPU s / 5 s, before | after |
| --- | --- | --- | --- | --- | --- | --- |
| palette over the page | 0.8 / 2.0–2.1 / 2.4–2.5 | 0.8 / 1.8 / 2.2–2.3 | 1.27–1.48 | 1.20–1.38 | 0.04–0.05 | 0.01–0.02 |
| strip column to column | 0.8–1.0 / 1.9 / 2.0–2.1 | 0.8–1.0 / 1.8–1.9 / 2.0 | 1.08–1.60 | 1.20–1.54 | 0.03 | 0.03 |
| overview in and out | 0.8–1.2 / 1.8–1.9 / 2.3 | 0.8–1.0 / 1.5–1.7 / 2.1–2.2 | 1.18–1.51 | 1.19–1.34 | 0.03–0.05 | 0.03–0.05 |

- No frame went over 16.7 ms either way (312–341 frames per scenario).
- The prepaint that placed the page cost nothing measurable: moving the strip draws the same.
- What goes is the work around a cover. Each palette opening hid the page and took a snapshot
  (WebKit renders it, then PNG, then a decode on the app's side). The page now stays under the
  palette, so WebKit spends a third to a half of what it did, and the palette's frames are
  0.2–0.3 ms shorter at p95.
- The overview still shows the page's picture, taken once as the tile goes scaled, so it costs
  what it did.

## 2026-09-30 — capture to glass on any link, the Mac's lock state

mac-studio, M1 Max, macOS 27.0, test profile (optimized), shared with other sessions' builds
(load average 26–32 during the glass runs). Logs: `target/logs/wire-clock-glass.log`.

```sh
# the estimate through the real encoder and decoder, loopback and 5 ms each way (a test)
cargo nextest run -p slopty-worker --lib --no-capture -E 'test(the_clock_probes_time_captures)'
# the whole glass measurement, now with the estimated clock beside the shared one
SLOPTY_GLASS_SECONDS=15 cargo nextest run -p slopty-worker --lib --run-ignored only --no-capture \
  -E 'test(=screen::synthetic::tests::capture_and_input_to_glass)'
# the estimator alone, simulated
cargo nextest run -p slopty-client --lib -E 'test(pacing::tests)'
# what the probes add to the frame path, and what a lock-state read costs
cargo nextest run -p slopty-client --lib --run-ignored only --no-capture -E 'test(clock_path_cost)'
cargo nextest run -p slopty-capture --lib --run-ignored only --no-capture -E 'test(console_read_cost)'
```

**The clock estimate against the shared clock.** Both clocks are this Mac's, so the shared
clock is the truth and every frame's estimated capture can be held to it. The drawn 1920 × 1080
screen at 60 fps through VideoToolbox, the stream's probes answered by its control:

| link | estimate off the shared clock, p50 / p95 / max | stated bound | capture → painted, shared | capture → painted, estimated |
| --- | --- | --- | --- | --- |
| loopback | 0.01 / 0.57 / 0.58 ms | 0.009 ms (round trip 17 µs) | p50 15.35, p95 16.65 ms | p50 15.45, p95 16.84 ms |
| 5 ms each way, 3 % loss | 0.26 / 0.37 / 0.37 ms | 6.07 ms (round trip 11.4 ms) | p50 18.64, p95 36.72 ms | p50 18.78, p95 36.97 ms |
| loopback, the bound that holds | 0.06 / 0.06 / 0.06 ms | 1.33 ms (round trip 120 µs) | p50 20.49, p95 28.04 ms | p50 20.55, p95 28.10 ms |
| 5 ms each way, the bound that holds | 0.10 / 0.35 / 0.37 ms | 6.92 ms (round trip 11.4 ms) | p50 24.92, p95 32.05 ms | p50 25.05, p95 32.11 ms |

The first two rows stated half the slowest fitted round trip, which is no bound (111 µs off on
a simulated loopback while claiming 101 µs); the last two, from the rerun after the worker
stamped a probe's arrival in its connection reader and the bound became what the fit can stand
behind (`docs/decisions/video.md`, "Capture to glass on any link"), on a far busier machine
(the loopback round trip was 120 µs, not 17). Log `target/logs/wire-clock-glass2.log`.

The loopback maximum comes from the first probes, which met a runtime busy starting the stream
and so carried a wider bound. The in-process test (60 frames, a 2-thread runtime) read 0.28 ms
at p50 and 0.62 at worst on loopback, where the probe's leg waits on a task and the echo's does
not: that half round trip is the asymmetry no estimate can see, inside the bound it states.
Behind 5 ms it read 0.32 ms p50, 0.61 ms at worst. Rerun with the bound that holds, under the
heavier load: 0.53 ms p50 and 0.92 ms at worst on loopback (a final bound of 0.64 ms, the worst
frames placed while the first probes' wider one stood), 0.07 ms behind 5 ms.

**The estimator, simulated** (`the_estimate_finds_a_drifting_clock_through_a_jittery_link`).
5 ms each way, each leg queued by up to 30 ms (a uniform draw cubed, so most probes meet a
nearly empty path), four probes a second for a minute. Six queueing sequences times four drifts
(−80, 0, 40, 150 ppm): the anchor is 14–66 µs from the true clock, the drift is found within
11 ppm, and the stated bound is 5.0–7.2 ms (5.3–5.6 ms as half the slowest round trip fitted,
which `the_stated_bound_holds_after_every_probe` shows is no bound). A path 2 ms one
way and 8 ms the other reads 3 ms off, half the difference, as it must, inside a bound of 5 ms.
A clock that steps 3 s back is followed on the second probe that disagrees, not the first.

**What the probes cost the frame path** (`clock_path_cost`, 2 000 000 each, three rounds):
the look at each datagram for an echo 2.9–3.0 ns, the anchor read in the decoder's callback per
picture 9.3–9.5 ns. Against the stream worker's 50–160 µs a frame ("the client's datagram path"
above), 51 datagrams and a picture add about 0.16 µs. On the wire, a probe is 6 to 12 bytes (postcard
varints) and its echo 41, four of each a second, before QUIC's own.

**The Mac's lock state** (`console_read_cost`, 2 000 reads of `CGSessionCopyCurrentDictionary`
and three keys): p50 71 µs, p99 472 µs, max 4.8 ms, reading `Shown` on this Mac. It runs with
each geometry probe, after the probe's bounds are timed, on the probe's blocking thread: every
250 ms while a stream is followed (and while it is locked), once a second while it is quiet.

**A locked Mac, end to end** (`a_locked_mac_reaches_the_client_and_so_does_its_return`): the
drawn Mac locked while its stream is served as the worker serves one, `Locked` reaches the
client's link over loopback QUIC 102 ms later (the geometry probe's 100 ms period, then the
source check and the control stream).

## 2026-09-30 — a trackpad scroll's gesture, one swipe, a press's number

mac-studio, M1 Max, macOS 27.0, test profile (optimized), with other sessions' builds running.
Every event is posted to an application the test starts itself (`tests/support/gesture_app.rs`)
and to nothing else; "Swipe between pages" is set to two fingers on this Mac.

```sh
SLOPTY_INPUT_E2E=1 cargo nextest run -p slopty-input --test inject gestures:: --no-capture
```

**What a trackpad scroll costs to post** (`a_trackpad_scroll_s_post_cost`): 600 scrolls in
their gesture at 120 a second through the real injector and `CGEventPostToPid`, alone and with
the gesture of subtype 6 after each, five runs:

| | p50 | p99 | max |
| --- | --- | --- | --- |
| scroll alone | 25.5–29.5 µs | 0.43–1.33 ms | 0.67–17.2 ms |
| with its gesture | 27.8–37.9 µs | 0.49–1.14 ms | 2.2–11.4 ms |

The gesture adds 2–8 µs at the median, a second `CGEventCreate` and post on the stream's input
thread; the tails are this machine's, alike either way. Every scroll's travel reached the app
(AppKit coalesced up to 3 of the 1 204 events into their neighbours).

**What the gesture is for** (`a_swipe_between_pages_follows_the_fingers_only_with_their_gesture`,
each case in an app of its own, swipe tracking on for that app alone through its argument
domain): a sideways swipe of twelve reports of 10 points then a lift, at a trackpad's 120 a
second, followed as Safari follows one (`trackSwipeEventWithOptions:`). The reports arrive
steadily (every 8.3 ms), jittered (each 0 to 12 ms late, in order, seeded) or in pairs (two at
once every 16.7 ms). Three runs, 9 swipes each way:

| arrivals | scroll alone | with its gesture, at the lift | then |
| --- | --- | --- | --- |
| steady | stays at −0.010, cancelled | −0.127 to −0.144, ended | turned (−1.0) |
| jittered | stays at −0.010, cancelled | −0.160, ended | turned |
| pairs | stays at −0.010, cancelled | −0.110 to −0.144, ended | turned |

Alone, the tracking springs back to 0 after the cancel (jittered and paired arrivals bounce to
−0.04 to −0.055 first); with the gesture it runs on to −1.0 every time.

*Why an earlier 120 failed.* The first cut's swipe was 8, 20, 30 and 40 points, then a lift.
At 60 a second it turned 5 of 5; at 120 it turned about one in three. Scratch runs (posting to
the test's app only) found the cause in that swipe's shape, an accelerating flick of 25 to
33 ms that AppKit's tracking cancels whatever the gesture carries. At 120 a second that flick
turned:

| change | turned |
| --- | --- |
| as posted, steady / jittered | 0 of 4 / 2 of 4 |
| lift held back 25, 34, 50 ms | 1 of 4 each |
| gesture travel ×0, ×1, ×1.67, ×3 | 0, 1, 0, 0 of 4 |
| half-size steps | 0 of 4 |
| merged down to 60 a second | 1 of 6 |
| at 60 a second | 4 of 4 |

One timestamp for both events or the scroll's own, the fixed-point deltas in lines, −0 for no
travel, the first report's gap, and all of those at once: 21 of 21 turned with six-step swipes,
so none of them is needed. With AppKit's mouse coalescing off or on, ten −10 steps turned 12 of
12 at 60 and at 120; those small steps turned 4 of 4 steady, jittered (0 to 25 ms apart, mean
8.1 ms) and in pairs, and merged to 60 a second 3 of 3. Constant-speed swipes of 150 ms at 1.0,
1.4, 1.8, 2.2 and 3.0 points a millisecond turned 30 of 30 at 60 and at 120. A gesture posted
8 000 points off every display cancels at once.

**A swipe is one event** (`each_gesture_reaches_the_app_as_a_trackpad_s_does`): the began
(no direction) and ended pair the injector posted reached the app as two `NSEventTypeSwipe`
events, `deltaX` 0 and then 1. One event with its direction reaches it as one, whatever its
phase field (began, ended or none were each tried).

**A press's number** (`a_drag_reaches_the_app_under_its_press_number`): posted as the injector
built them before, a press, two drags and a release read back with event number 0 each; now
press, drag and release read 1, the next press 2.

## 2026-09-30 — the live lane's macOS guest: install, disk, clone, boot, deploy
`cargo xtask vm` (`docs/DEV.md` ▸ "Live lane in a VM") on this Mac Studio (M1 Max, 10 cores,
32 GB, macOS 27.0.1), with guests on the external disk (`/Volumes/Lacie`, APFS over PCIe). The
guest is macOS 26.6.2 from the pinned `macos-tahoe-base` image, 4 cores and 8 GB. Other
sessions were building on this Mac throughout, so boots and volume figures carry their load.

| what | number | how |
| --- | --- | --- |
| tart install (`brew install openai/tools/tart`) | 75 MB, and 14 MB for Softnet | `du -sh` of the Cellar dirs |
| image pull, 27.3 GB compressed, 96 layers | 1 047 s (≈ 26 MB/s) | `tart pull …@sha256:1b09…` |
| image on disk | 31 GB of a 50 GB sparse disk (47 GB apparent) | `du -sh`, `du -A` of `TART_HOME` |
| base made from it (`vm create`) | ≈ 0 GB more: an APFS clone (`du` counts it twice, 61 GB) | free space 92 → 96 GB across it |
| base's first boot, to a command in the logged-in session | 63.8 s | `vm create` |
| clone per run | 26–40 ms | `tart clone`, four runs |
| boot of a fresh clone, to a command in the logged-in session | 38.1, 38.6, 48.6, 91.0 s (the 91 s under a loaded host); 36.0 s for the dev guest | `tart run --no-graphics` → first `tart exec true` |
| `slopty worker deploy` into it (3 binaries, install, doctor) | 3.5, 5.8, 8.1, 9.1 s | `vm live` / `vm e2e` timings |
| volume growth over clone + boot + deploy | ≤ 0.37 GB at the quietest run (an upper bound, the volume is shared) | `statvfs` before and after |
| `slopty-input --test inject`, 5 live tests, in the guest | 40.1 s, 36.9 s of it the post-cost test | `vm live -p slopty-input --test inject` |

Rerun: `cargo xtask vm create`, then `cargo xtask vm e2e` and
`cargo xtask vm live -p slopty-input --test inject`. Each run writes its timings to
`target/vm/<guest>/timings.txt` and its report to `target/vm/<guest>/target/nextest/default/junit.xml`.

In the guest, the gesture tests read what AppKit made of each posted event as they do on a Mac of
its own. A swipe between pages followed its gesture to −1.000 and the scroll alone sprang back from
−0.036. 1 204 of 1 204 scroll events arrived. The trackpad scroll's post took p50 / p99 74.5 /
1 022.8 µs alone and 91.9 / 2 483.7 µs with its gesture. That is two to three times this Mac's
27.8–37.9 µs medians (2026-09-30, "a trackpad scroll's gesture"): a guest's numbers are for
behaviour, never for a budget. The HID pointer move landed within 2 points of its target.

What costs the most is the boot: 40–50 s from a clean clone, most of it macOS starting and
logging in. A suspended base (`tart suspend`) would resume in seconds, but a suspendable guest
has no audio device, so it waits for a live test that does not need sound.

## 2026-09-30 — projects on the server at a large fleet's state, placement in CEL (release)

What the server's project state costs the hub (review item: deltas, not whole tasks; debounced,
compact writes; the lock only for the change), and what one placement over CEL rules costs. The
state is 10 projects of 200 tasks, 2 live agents per project with 128 subagents each, and every
timeline full at its default 4 096 entries. Release build on mac-studio (M1 Max, 10 cores), 100 samples each, while
other sessions built on this Mac. Instructions are the gating number (`xtask/budgets.toml`); wall
time carries the load.

| series (`server.…`) | what | instructions/op | wall p50 / p99 |
| --- | --- | --- | --- |
| `projects_cost.file_clone` | the clone the keeper takes under the hub's lock | 13.1 M | 695 / 952 µs |
| `projects_cost.file_write_compact` | serializing it, off the lock | 83.8 M | 4.94 / 5.11 ms |
| `projects_cost.task_change` | one task's status change, under the lock | 7 130 | 0.46 / 0.88 µs |
| `projects_cost.agent_report` | one subagent's start landing on its task | 15 815 | 1.6 / 1.9 µs |
| `projects_cost.snapshot` | the projects a new or lagging client link is sent | 4.96 M | 290 / 367 µs |
| `placement_cost.rank_32_workers` | 2 `require` + 2 `prefer` rules over 32 workers' facts, compile included, outside the lock | 5.36 M | 353 / 627 µs |

Sizes:

| what | bytes |
| --- | --- |
| `projects.json`, compact (what the keeper writes) | 5 062 834 |
| the same, pretty (what it wrote before) | 11 089 412 |
| a task's status change as pushed (`ProjectUpdate`: task + timeline entry) | 713 |
| the task alone | 641 |
| its project's whole status | 506 442 |
| the largest node's natives (what a whole-task push carried before) | 24 400 |
| a subagent's start as pushed (the one native) | 212 |
| a new link's snapshot, all ten projects | 1 828 606 |

Reading: a change costs microseconds under the lock and pushes under a kilobyte. Before, every
change pushed the whole task with its natives (up to 25 KB here) and rewrote the file pretty
(11 MB). Now a burst of changes costs one compact write per 250 ms settle. The only real lock
time left is the keeper's clone, 0.7 ms at most four times a second. That is the control plane,
never the terminal, input or frame path, so it stays. An append log becomes the path if the
clone passes a millisecond or two. Placement's 0.35 ms runs before the reservation and outside
the lock.

Rerun: `SLOPTY_BENCH_OUT=target/logs/projects-bench.jsonl cargo nextest run --release -p
slopty-server --run-ignored only -E 'test(/_cost$/)' --no-capture`, or
`cargo xtask bench --filter projects_cost` (and `--filter placement_cost`), from
`crates/slopty-server/src/project/cost.rs`.

## 2026-09-30 — projects, round 2: an append log, a snapshot of cards, CEL at master (release)

The same state and bench after review round 2. The keeper no longer clones the state under the
hub's lock. It keeps its own replica, built from the changes the hub sends it, and appends each
change to `projects.log` as one JSON line. It writes the snapshot only when the log passes 8 MiB
and at shutdown. A client's snapshot carries each task as its card (no brief, no natives), in
parts of at most 8 MiB. Placement now counts a rule's worst case before it runs, and runs on the
blocking pool on the `cel` fork at upstream master (for its absorbed comprehension errors and
its maps iterated in key order). Same Mac, 100 samples each.

| series (`server.…`) | what | instructions/op | wall p50 / p99 |
| --- | --- | --- | --- |
| `projects_cost.log_append` | the keeper's work for one change, off the lock: its replica takes it, and its log line | 15 020 | 0.9 / 2.4 µs |
| `projects_cost.file_write_compact` | a compaction's write of the whole file, off the lock | 84.3 M | 4.92 / 5.07 ms |
| `projects_cost.task_change` | one task's status change, under the lock | 7 579 | 0.5 / 1.0 µs |
| `projects_cost.agent_report` | one subagent's start landing on its task | 14 049 | 1.6 / 2.5 µs |
| `projects_cost.snapshot` | the projects a new or lagging client link is sent | 1.50 M | 101 / 164 µs |
| `placement_cost.compile_4_rules` | compiling and costing the 4 rules | 676 k | 42 / 134 µs |
| `placement_cost.rank_32_workers` | the 4 rules over 32 workers' facts, compile included | 7.89 M | 537 / 694 µs |

Sizes: a task change's log line is 725 bytes. The change as pushed is 417 bytes (the task as a
card, 345). A new link's snapshot is 742 KB, down from 1.83 MB.

Reading: nothing the store does holds the hub's lock now. The 0.7 ms clone four times a second
is gone, and a change costs the keeper about a microsecond. The snapshot costs a third of what
it did, as cards. Placement costs 47 % more, in judging rather than compiling (0.68 M of 7.89
M). The fork's own change sorts a map's keys only when a comprehension walks one, which these
rules do not. The cause turned out to be `Context::default` building the standard library
per context; the next section measures the fix.

Rerun as above.

## 2026-09-30 — placement: the standard library built once, not per worker (release)

Round 2 left ranking 47 % slower on `cel` master than on 0.14.5. The cause is not the value
model. Placement builds one `Context::default()` per worker per ranking, and on master that
call builds `Env::stdlib()`, declaring every standard overload into fresh maps. The `Env` is
read-only once built, so our fork (`aislopware/cel-rust` `69202be`, drafted upstream in
`.research/cel-rust-upstream-pr.md`) builds it once per process and shares it with every
default context. Same Mac, same bench, 100 samples each, three runs each way:

| series (`server.placement_cost.…`) | before (`4316180`) | after (`69202be`) |
| --- | --- | --- |
| `compile_4_rules` | 675–684 k instructions, 39–42 µs p50 | 674–680 k, 39–44 µs |
| `rank_32_workers` (compile included) | 8.03–8.07 M instructions, 527–531 µs p50, 760–786 µs p99 | 2.97–3.03 M, 208–220 µs p50, 290–347 µs p99 |

Inside `cel` alone, one default context, two variables bound and one rule executed costs
25.73 µs on `4316180` and 0.40 µs on `69202be` (release, 20 000 rounds).

Reading: a ranking now costs 63 % less than before and 45 % less than on 0.14.5 (5.36 M). A
quarter of what is left is compiling the four rules (0.68 M), which happens once per ranking.
Placement's structure was already right: rules compiled once per ranking, facts bound once per
worker. The fix belongs in `cel`, where every caller that builds a context per evaluation
gains the same.

Rerun: `SLOPTY_BENCH_OUT=$PWD/target/logs/placement.jsonl cargo nextest run --release -p
slopty-server --run-ignored only -E 'test(placement_cost)' --no-capture`. The output path must
be absolute, since nextest runs the test in the crate's directory.

## 2026-09-30 — hang reports: what the foreground journal costs a runnable

The app builds gpui-fast `0dd9e96` with the `profiler` feature for its hang monitor
(`docs/decisions/crashes.md`). With the feature on, gpui installs a foreground journal when the
App is created. The macOS dispatcher's two hooks around every main-thread poll
(`gpui::profiler::update_running_task` before it, `save_task_timing` after it) then record the
poll too. The journal records whether or not the monitor runs; the monitor only adds its own
thread, which reads the journal every 2 s.

The A/B compares the same crate built twice, without and with the feature. The workspace cannot
build gpui both ways, because hakari unifies the feature, so the bench is a one-file crate
outside it. It depends on gpui, gpui_platform and scheduler at that rev, with a `profiler`
feature forwarding to `gpui/profiler` and release with `debug = 1`. It opens
`gpui_platform::headless()` and calls the hook pair in a loop on the main thread: 2 M
empty polls, then 2 000 polls that spin 1.1 ms each (past the journal's 100 µs floor, so each
is an event of its own). It reports the median of nine rounds, and was run three times each
way on the Studio under other lanes' builds:

| per runnable | without `profiler` | with `profiler` | added |
|---|---|---|---|
| a short poll (folded into the small-poll summary) | 43.6–46.6 ns | 151–157 ns | ~110 ns |
| a poll recorded as an event (best round) | 435–8 700 ns | 910–994 ns | ≲ 0.5 µs, noise-bound |
| a whole dispatch round trip (yield → main queue → poll) | 1.96–2.05 µs | 1.96–2.14 µs | inside the noise |

A frame that runs 100 main-thread runnables pays about 11 µs, 0.13 % of a 120 Hz frame's
8.3 ms. A recorded poll is already over 100 µs, so its half microsecond is under 0.5 % of it.
Nothing of this shows in a frame. Once the first frame is up, the monitor's thread polls every
2 s and takes no main-thread time.

## 2026-09-30 — a caret changed alone is a frame

libghostty dirties no row when a program changes the caret in place (DECSCUSR's `ESC[5 q`,
DECTCEM), so `GhosttyEngine::build_frame` found nothing to send. zsh's bar at a fresh prompt,
written apart from the prompt, never reached the app. The engine now reads the caret before
deciding that nothing changed, and compares it with the caret of the last frame every viewer
took. That read costs a `take_frame` that finds nothing new:

| `engine.frame_cost` (60×12), instructions per op, two runs each | before | after |
|---|---|---|
| `take_frame_unchanged` (new series: asked again, nothing new) | 716–741 | 816–891 |
| `take_frame` after a typed byte | 21 875–21 926 | 21 803–21 828 |
| `write` of one byte | 824–849 | 774–824 |

About 125 instructions, under 0.1 µs (wall p50 0.0–0.1 µs both ways), and only when nothing
changed. A typed byte's frame costs the same as before. Rerun:
`cargo nextest run --release -p slopty-engine --run-ignored only -E 'test(=ghostty::checkpoint_tests::frame_cost)' --no-capture`.

## 2026-09-30 — streams from one worker: what they could share

Mac Studio M1 Max (two `ave2` encode engines, one decode engine), macOS 27.0, release, run from
`/tmp` under `nice`, load 7–27 with other sessions building. The question: of what every stream
owns today (a capture, an encoder session, a packetizer, a pacer and its own sound on the worker; a
decoder, an Opus decoder, a CoreAudio player and a `VideoLayer` on the client), what would be
cheaper shared across the streams one client watches from one worker? CPU is `getrusage`; CPU power
is the process's `ri_energy_nj` (`proc_pid_rusage`, `RUSAGE_INFO_V6`), which counts its threads on
the CPU and not the media engines or the GPU. Those take `powermetrics`, which needs root, and this
Mac has no passwordless `sudo`, so engine energy is not measured.

```sh
cargo test -p slopty-codec --release --test streams --no-run   # target/release/deps/streams-<hash>
cp target/release/deps/streams-<hash> /tmp/slopty-conc/streams && cd /tmp/slopty-conc
nice ./streams --ignored --nocapture --exact tests::concurrent_decode
nice ./streams --ignored --nocapture --exact tests::concurrent_audio
```

**The client decoding N streams** (`concurrent_decode` in `crates/slopty-codec/tests/streams.rs`:
the worker's own encoder makes a 2 s loop of scrolling text once, HEVC 4:2:0, then N of the
client's `Decoder`s, each on its own thread, submit it on one 60 Hz beat as tiles on one display
do). Submit → picture p50 / p95 in ms for stream 0, pictures a second for every stream, the
process's CPU cores and CPU watts; two rounds:

| size | streams | stream 0 p50 / p95 | pictures a second | CPU cores | CPU W |
| --- | --- | --- | --- | --- | --- |
| 1080p, 4.3 Mbit/s | 1 | 1.15 / 5.73; 1.01 / 1.14 | 60 | 0.014; 0.010 | 0.016; 0.014 |
| | 2 | 1.19 / 4.36; 1.00 / 1.30 | 60 each | 0.022; 0.017 | 0.030; 0.027 |
| | 4 | 1.55 / 5.92; 1.04 / 1.67 | 60 each | 0.045; 0.030 | 0.054; 0.049 |
| | 8 | 1.46 / 3.09; 1.34 / 2.55 | 60 each | 0.069; 0.053 | 0.101; 0.089 |
| 4K, 14.7 Mbit/s | 1 | 2.16 / 2.49; 2.09 / 2.29 | 60 | 0.012; 0.011 | 0.016; 0.016 |
| | 2 | 2.19 / 3.52; 2.78 / 11.76 | 60 each | 0.020; 0.025 | 0.030; 0.033 |
| | 4 | 3.33 / 10.56; 3.74 / 17.99 | 60 each | 0.039; 0.046 | 0.059; 0.060 |
| | 8 | 6.37 / 33.31; 6.81 / 32.32 | 60 each | 0.079; 0.085 | 0.115; 0.119 |

A decoded stream costs the client about 0.007 of a core and 0.011 W of CPU. Eight 1080p streams
decode within 2.6 ms p95. Eight 4K streams fill the one decode engine (p95 32–33 ms) and still keep
60 pictures a second each. There is nothing to share here: each picture is its own decode, and a
session costs nothing that shows (the footprint grew 0.1–0.6 MB for 2–8 sessions after the first).

**Each stream's own sound** (`concurrent_audio`: per stream, the worker's Opus encoder and the
client's Opus decoder on one thread, fed 10 ms of two tones and a little noise every 10 ms):

| streams | encode p50 / p95 µs a packet | decode p50 / p95 µs | CPU cores | CPU W |
| --- | --- | --- | --- | --- |
| 1 | 118.5 / 152.0 | 36.8 / 43.8 | 0.017 | 0.043 |
| 2 | 114.3 / 141.7 | 36.2 / 42.8 | 0.032 | 0.085 |
| 4 | 111.4 / 180.7 | 35.2 / 64.5 | 0.063 | 0.167 |
| 8 | 127.9 / 214.8 | 34.8 / 77.6 | 0.126 | 0.327 |

A stream's sound costs about 0.016 of a core and 0.041 W of CPU, more than decoding its video,
before the CoreAudio player each stream opens on the client and the audio tap each capture holds
in ScreenCaptureKit (neither measured here). The worker end to end of "several streams on the
encode engines" (about 0.05 of a core a stream) is without sound, since the drawn canvas has none
(`audio: false` in `slopty-capture/src/synthetic.rs`), so a real stream's encode adds about 0.012.

**The worker's video.** Unchanged from "several streams on the encode engines" and "the focused
stream when the engines are full" above: about 0.05 of a core a 1080p60 stream, and the two
engines are the one resource the streams share, which `placement_fps` and the focus give-way
already arbitrate.

What this says (docs/decisions/audio.md, "One sound per worker on a client, not one per stream"):
the decoders and the encoder sessions have nothing to gain from sharing, and the sound does, since
ScreenCaptureKit filters audio by application, so tiles of one app, or a desktop and any window,
carry the same sound and the client plays it once per tile.

## 2026-09-30 — large streams on the encode engines: a session each, stripes, or one shared session

Mac Studio M1 Max (two `ave2` encode engines), macOS 27.0, release, run from `/tmp` under `nice`,
load 7–20 (other sessions building). The question (backlog #14): N concurrent 4K and 5K streams
from one worker, coded as today (one low-latency session per stream), as two stripe sessions per
stream (`docs/decisions/video.md`, "Two stripes halve the encode at 3K and above"), or all in one
shared session (the streams' pictures side by side in one grid). `concurrent_encode` in
`crates/slopty-codec/tests/streams.rs` gives every session a thread submitting on one 60 Hz beat,
the beats of all streams on the same instants, and a thread back late from a submit takes the
newest beat, as the worker's one-frame mailbox does. The pictures are `IOSurface`-backed scrolling
text, so an aligned session codes inside the submit as it does on a capture (a plain buffer is
copied and queued, which read 116 ms for one 4K stream in the first run and was thrown out). A
striped stream's two sessions take each capture together. The grid of the shared session is drawn
ahead of time, so the copy it would need is not counted. Encode is a capture's first submit →
its last packet back. Per run: p50 / p95 / p99 in ms, captures coded a second per stream, the
process's CPU (`getrusage`, cores), its footprint past the drawn pictures, and from IOReport
(`slopty_testkit::soc`, no root) each engine's interrupts and DRAM traffic and the GPU's active
share and watts, less one idle second just before each run.

```sh
cargo test -p slopty-codec --release --test streams --no-run   # target/release/deps/streams-<hash>
/bin/rm -f /tmp/slopty-conc/streams && cp target/release/deps/streams-<hash> /tmp/slopty-conc/streams && cd /tmp/slopty-conc
nice ./streams --ignored --nocapture --exact tests::concurrent_encode
SLOPTY_ENCODE_CHROMA=444 SLOPTY_ENCODE_SIZES=3840x2160 nice ./streams --ignored --nocapture --exact tests::concurrent_encode
SLOPTY_ENCODE_BACKGROUND=1920x1088:2 SLOPTY_ENCODE_MODES=sessions,stripes SLOPTY_ENCODE_SIZES=3840x2160 SLOPTY_ENCODE_STREAMS=1 nice ./streams --ignored --nocapture --exact tests::concurrent_encode
cargo test -p slopty-codec --lib --release --no-run && cp target/release/deps/slopty_codec-<hash> /tmp/slopty-conc/codec_lib
nice /tmp/slopty-conc/codec_lib --ignored --nocapture --exact stripes::imp::tests::stripes_copy_cost
```

(Replace a copied test binary with `rm` then `cp`: `cp` over a binary that has run keeps the
kernel's cached signature of the old one, and the new one is killed at launch with no output.)

**4:2:0, 3840 × 2160, 40 Mbit/s a stream.** Encode p50 / p95 / p99 (every stream), captures a
second each:

| streams | a session each | two stripes each | one shared session |
| --- | --- | --- | --- |
| 1 | 20.5 / 20.7 / 20.8, 48.5 | **12.0 / 12.1 / 12.2, 60** | 20.6 / 20.8 / 20.9, 48.3 |
| 2 | 20.7 / 26.0 / 31.1, 46.3 each | 22.6 / 22.8 / 22.9, 44.3 each | 39.5 / 40.9 / 41.2, 25.3 |
| 3 | 38.4 / 39.3 / 43.1; 47.5, 25.3, 25.5 | 32.5 / 32.6 / 34.6, 30.8 each | 77.3 / 82.1 / 89.8, 12.8 |
| 4 | 39.2 / 41.5 / 44.8, 25.5 each | 43.4 / 43.6 / 43.7, 23.0 each | 77.2 / 80.4 / 80.8, 13.0 |

**4:2:0, 5120 × 2880, 60 Mbit/s a stream:**

| streams | a session each | two stripes each | one shared session |
| --- | --- | --- | --- |
| 1 | 35.0 / 36.2 / 36.6, 28.3 | **19.4 / 19.8 / 25.6, 50.3** | 35.0 / 36.2 / 36.7, 28.3 |
| 2 | 35.2 / 38.4 / 43.1, 27.5 each | 37.0 / 37.5 / 40.5, 27.0 each | 68.4 / 71.6 / 71.8, 14.5 |
| 3 | 60.0 / 70.7 / 74.6; 14.8, 26.0, 14.8 | 55.0 / 55.3 / 56.6, 18.2 each | fails: `VTCompressionSessionCompleteFrames` -17691 |
| 4 | 68.0 / 70.7 / 78.2, 14.8 each | 73.6 / 73.9 / 85.0, 13.8 each | not run |

**4:4:4 10-bit, 3840 × 2160** (the session costs the engine what 4:2:0 does; its DRAM traffic is
twice): a session each 20.7 ms and 48 a second alone, 20.9 and 47.4 each for two, 25.5 each for
four; two stripes each **12.0 / 12.3 / 12.5 ms and 60** alone, 32.3 ms and 31.8 each for two
(this run's stripes took beats each on their own, before they took captures together; 4:2:0
went from 32.3 to 22.6 ms and from 32.6 to 44.3 a second when they did); one shared session
20.8 ms and 45.5 alone, 25.0 for two, 12.4 for three or four.

**A 4K stream beside 1080p ones** (16 Mbit/s each, a session each). The 4K stream's encode p50 /
p95 and rate; the others' rates:

| beside it | the 4K stream as one session | the 4K stream as two stripes |
| --- | --- | --- |
| one 1080p | 20.7 / 23.3 ms, 46.8; the other 59.5 (6.7 ms p50) | 16.0 / 17.1 ms, 60; the other 60 (16.0 ms p50) |
| two | 25.0 / 25.2 ms, 40.0; 39.8 and 60 | 13.4 / 17.0 ms, 58.8; 59.3 and 59.0 |
| four | 30.6 / 30.8 ms, 32.8; 32.8, 60, 32.8, 60 | 27.2 / 27.4 ms, 36.8; 36.5, 60, 36.8, 36.8 |

And the 1080p baseline in the same harness: 6.50 / 6.69 ms and 60 alone, two at 60 (6.54 p50),
four at 59.5–59.8 (9.9 / 12.2 ms).

**Where the frames went** (IOReport, interrupts a second above idle, `ave0`, `ave1`). One session
runs on one engine only: one 4K stream 0 and 194, one 5K 0 and 112, whatever the load. Two
streams got one engine each (184 and 184). Three got two on one engine and one alone, the lone
one at 47.5 a second and the pair at 25 (4K), 26 against 14.8 (5K), in every run. The shared
session never left one engine: 101, 51 and 52 interrupts a second on `ave1` alone for two,
three and four 4K pictures in its grid, and `ave0` idle. Two stripes of one stream ran side by
side, 240 and 240. In the first runs of the day another process was coding on `ave1` (60–123
interrupts a second with none of ours running); every table here is from runs where both engines
read idle beforehand.

**What else a stream costs.** A 4K session's footprint is 3.2 MB when the process opens its
first, 0.1 MB for each after, and 3 MB more once it has coded; stripes the same. The process's
CPU is 0.006–0.026 of a core for every row above and 0.02–0.04 with stripes (two threads a
stream, and the barrier), 0.009–0.05 W. The GPU is not in the path: active +0.000–0.03 of the
time and +0.000–0.06 W over idle, noise. The DRAM traffic of an engine at 4K is 4.0–4.4 GB/s
(4:2:0) and 8.6–9.3 GB/s (4:4:4) whatever the arrangement, the engine's own reads of its
references.

**A stripe's picture** (`stripes_copy_cost`, `slopty_codec::stripes::StripeCopy`: the stripe's
coded rows of both planes copied from the capture into an `IOSurface`-backed pool buffer, both
stripes one after the other; p50 / p95, 200 rounds), and the gate (`side_by_side`: 9 frames
whole, then 9 as stripes on two threads with their copies, medians):

| size | 4:2:0 both copies | 4:4:4 both copies | gate 4:2:0 whole → striped | gate 4:4:4 |
| --- | --- | --- | --- | --- |
| 3024 × 1968 | 0.29 / 0.45 ms | 1.04 / 1.60 | 15.6 → 9.7 ms, pays | 15.6 → 10.0 |
| 3840 × 2160 | 0.30 / 0.41 | 1.15 / 1.57 | 20.7 → 12.1 | 20.8 → 13.4 |
| 5120 × 2880 | 0.65 / 0.96 | 2.26 / 3.23 | 35.2 → 20.4 | 35.4 → 22.5 |

The copy of one stripe, on its own thread as the stripes run, is 0.15 ms at 4K 4:2:0 and 0.6 ms
at 4:4:4 (26.5 MB, about 46 GB/s: the memory, not the loop). The gate's striped time includes
it and still pays by 35–42 %.

What this says (`docs/decisions/video.md`, "A large stream takes both encode engines while it
has them"):
- One shared session is never better. A session runs on one engine, so every stream in a
  shared one gets 1/N of an engine: two 4K pictures in one session are coded at 25 a second
  where two sessions code them at 46, and three 5K pictures in one fail.
- A session per stream stays, and a stream larger than one engine's pixel rate (about 400
  megapixels a second, 20.5 ms a 4K frame) is coded as two stripes. Alone it goes from 48 to 60
  a second at 4K (20.5 → 12.0 ms) and from 28 to 50 at 5K (35 → 19.4 ms), and beside 1080p
  streams it keeps 59–60 until the engines are full.
- Once several large streams fill both engines, stripes and sessions code about as many frames
  (4K: 88.6 against 92.6 a second in all at two streams, 92.4 against 98.3 at three, 92 against
  102 at four), and the stripes share them evenly where the driver's placement gives one of
  three streams an engine of its own and the other two half an engine each.
- Nothing else is shared or worth sharing: the sessions cost 3 MB and a hundredth of a core, and
  the GPU nothing.

## 2026-09-30 — the drag helper's roles: a slow promise, spring loading, a drag out

macOS 26.6.2 in a tart guest (4 cores, 8 GB, `--no-graphics`). The runs are
`cargo xtask vm live -p slopty-dnd --test roles`, logs under `target/logs/media/roles{,2,3,4,5}.log`.
`slopty_dnd`'s source and catcher run in the test's own `slopty-dnd-helper`, and every drag ends
on the test's own drop target (`crates/slopty-dnd/tests/support/`).

**A promise the target waits on.** The helper's source drags one item that promises
`public.file-url`, whose provider waits 5000 ms (an upload still arriving) and then writes a
1 MiB file. The drop target reads the URL inside `performDragOperation:`.

| run | release → the target has the whole file | provider's wait | the drag ends |
| --- | ---: | ---: | --- |
| 1 | 5054 ms | 5032 ms | copy |
| 2 | 5045 ms | — | copy |
| 3 | 5093 ms | — | copy |

A target blocks inside its read for as long as the provider takes, 5 s at least, and then
takes the file whole. Nothing times out, and the drag still ends as a copy. The target's app is
frozen for that time, so a drop must never be held for long. With text and a 4096-byte file
promised beside a whole file, the target asked for the late file first (339 ms, its 300 ms wait)
and then for the text (0 ms).

**Spring loading.** A drag rests over a spring-loaded target (`com.apple.springing.enabled` 1,
delay 0.5 s), and the test posts moves through the worker's injector, a fresh drag per pattern,
for up to 4 s. The target's highlight is already on when the glide there ends.

| nudges | run 4 | run 5 |
| --- | --- | --- |
| a point either side every 100 ms, from the rest | no spring in 3 s (27 moves) | — |
| a point out and back every 100 ms | no update reaches the target | — |
| a point either side every 100 ms after 2.5 s still (the P0 spike) | 3273 ms | 3268 ms |
| a point either side every 500 / 600 ms | no spring | no spring |
| a point either side every 650 ms | no spring | 1432 ms |
| two points out and back every 400 / 500 / 600 ms | no spring | no spring |
| two points out and back every 700 ms | 1399 ms | no spring |
| two points out and back every 800 ms | 1567 ms | 1492 ms |
| `slopty_dnd::nudge` (800 ms rest, two points out and back) | — | 1550 ms |

A one-point move never reaches the target as an update. Moves 600 ms apart or less never spring
it, however long they go on. Between 650 and 700 ms it springs on some runs and not on others,
and at 800 ms it sprang in every run. So `nudge::rest_us` is the user's spring delay plus
300 ms, 800 ms at the default, and the target springs about 1.5 s after the pointer comes to
rest.

**A drag out, seen and caught.** A test app begins a drag of a file and a file promise (2048
bytes, written after 200 ms) from a press, and the drag watch reads the drag pasteboard every 8
ms of the moves.

- The count moved by the first move, 3 points and 8 ms after the press.
- The file item named its file (and also offers `com.apple.pasteboard.promised-file-url`). The
  promise's item offered `com.apple.pasteboard.promised-file-content-type` (`public.data`) and no
  file.
- Let go over the helper's catcher, the drag caught the file as a reference, left where it was,
  and one promise. The promise's file arrived whole in the catcher's folder, and the app saw a
  copy.

## 2026-10-01 — the drop in, end to end

Mac Studio M1 Max, macOS 27.0.1, other sessions building beside it. The app against a worker
that records its drops, on the drawn display, two runs:

```sh
cargo xtask e2e app --filter 'test(~dnd::)'
```

The app's self-test hands the workspace each step of the drag as the platform's destination
would, 20 ms apart, and the test reads the worker's record and its drop directory every 20 ms,
so each figure is up to 20 ms late. A note (20 bytes) and 24 MiB of bytes that do not repeat
are dragged.

| | ms |
| --- | ---: |
| refusing target: the first step → the badge reads none | 33.7, 35.7 |
| drop → the release, both files whole in the landing | 269.4, 278.0 (1156.1 in a run before the fixes) |
| refused drop → its landing gone on the worker | 21.5, 21.8 |
| left drag → its landing gone | 20.9, 22.1 |

- The digests the release found in the landing are the sources' own.
- The drop was let go right after the badge read copy, so the 24 MiB went up mostly after the
  drop: the release waited for it, as it should.
- Before the fixes the refusing target's drag read copy for the whole 20 s, and a left drag's
  landing still held `note.txt` and `frames.bin.partial` 20 s on.

## 2026-10-01 — the badge, timed, and a drag out of a window

macOS 26 in a tart guest (4 cores, 8 GB, `--no-graphics`), each test once, ten rounds where a
test has rounds:

```sh
cargo xtask vm live -p slopty-dnd --test roles
```

Every stamp is on the uptime clock (`NSProcessInfo.systemUptime`): the test's drop targets stamp
their `entered`, `updated` and `exited` lines with it, a thread of the test's own reads the
system cursor every 500 µs and stamps each change, and the helper's messages are stamped as the
test reads them off its pipe. A drag is carried in 24 moves 8 ms apart.

**Where the badge's 50–85 ms went** (`the_workers_helper_lands_a_drop_at_the_point`, from the
move that crossed onto the target, before the change):

| | ms |
| --- | --- |
| the target's `draggingEntered:` | 24–59 |
| the copy cursor shows | within 0.3–2 of the entry |
| the helper says copy (a timer on its main thread every 8 ms) | 42–82, so 4–39 after the cursor (10, 10, 11, 12, 17, 18, 29, 31, 39 and 4) |
| the helper says copy (its own thread, the seed every 1 ms) | 0–2 after the cursor |

- The helper's timer ran on the main thread, where AppKit tracks the drag, and fired late. On
  a thread of its own the copy follows the cursor within 2 ms.
- The targets' `draggingUpdated:` come 16–33 ms apart while the moves come every 8 ms: the drag
  manager steps the drag at that pace, and enters a target on a later step than the one that
  left the last.

**Across touching targets** (`the_badge_follows_a_drag_across_touching_targets`, two that take a
copy side by side and one below the second that refuses; two runs, twenty crossings each way):

| | ms |
| --- | --- |
| copy onto copy: the next's `entered` after the first's `exited` | 19–47, median 30 |
| copy onto copy: a none said between them | never (the arrow showed for that gap before the hold) |
| onto the refusing one: its `entered` → the helper says none | 4 before to 37 after, median 18 |
| back onto one that takes: its `entered` → the helper says copy | 0.9–16, median 3 |

- The none waits for two more steps of the drag manager (`draggingSession:movedToPoint:`) or 50
  ms, whichever is first, so the arrow between two targets is never said. Once the gap ran past
  the hold, the none came 4 ms before the refusing target's entry.

**A drag out of a window** (`a_window_streams_press_drags_out_and_the_catch_takes_it`, one run):
the injector presses on a 1:1 stream of the test app's window, which is on top, through the HID
tap, and moves 3 points every 8 ms; the app begins its drag of a file and a promise.

| | ms |
| --- | ---: |
| press → the drag pasteboard's count moved (read after each move) | 174.5 |
| catch → the catcher up under the pointer (`Ready`) | 30.6 |
| catch → the catcher says the drag is over it | 170.3 |
| release → the catch says what it took | 22.0 |

- The file is taken where it is, the promise is called into the drag's folder whole (2048
  bytes), and the app sees a copy.
- One run, so a sample, not a spread. The catcher needed more than one carry out and back
  (each waits 250 ms) before the drag manager entered it. The drag was seen only after the
  app's own drag threshold and the moves past it.

## 2026-10-01 — the drop in, carried: the helper's source, the badge, the release

macOS 26 in a tart guest (4 cores, 8 GB, `--no-graphics`), ten drops in one run:

```sh
cargo xtask vm live -p slopty-dnd --test roles -- the_workers_helper_lands_a_drop_at_the_point
```

The worker's drag helper (`slopty_dnd::helper`, the test's `slopty-dnd-wire-helper`) is spoken to
over its pipes as the daemon speaks to it. The injector's drag mode (`DragStep`) maps the entry,
presses into the helper's source and carries the drag onto the test's drop target in 24 moves 8
ms apart, and lets go 100 ms after the last move, with the file whole and the text given. Each
time is on the test's clock, at the moment it read the message or the line. The log is under
`target/vm/<guest>/nextest.log`.

| | 1st | 2nd–10th |
| --- | ---: | --- |
| `SourceAt` → `Ready` (the window server shows the source at the point) | 112.1 ms | 4.8–10.1 ms |
| the drag crosses onto the target → the copy cursor's `Operation` | 42.2 ms | 50.0–84.7 ms (one drag never left copy) |
| release → the target's `performDragOperation:` | 8.5 ms | 4.5–7.0 ms, and 374.0 ms once |
| release → the helper's `Ended` (a copy every time) | 11.7 ms | 7.3–10.4 ms, and 396.0 ms once |

- The first `Ready` includes the helper starting: AppKit, the window server's connection and
  AppKit's cursors read once. Later ones are the wait for the window list to show the source's
  window at the point. A press sent at once on `Ready` before that wait missed the window and
  began no session (the run before this one).
- Over the guest's desktop the drag already shows copy, since Finder's desktop takes files. On
  crossing onto the target the cursor goes to the arrow 4–37 ms after the crossing and to copy
  50–85 ms after it, and the client's badge flickers to none in between. That this was the
  drag manager's alone was wrong: the next entry times each part and finds the helper's
  watch 10–39 ms late.
- A drop with its files whole reaches the target 5 ms after the release (median), so the drop
  lands RTT/2 plus about 5 ms after the client's drop. One release in ten took 374 ms; the
  guest's other work is the likely cause, not yet shown.

## 2026-09-30 — file tiles on kernel events: kqueue against FSEvents, and the follower's report

Mac Studio (M1 Max, 10 cores), macOS 26, release build. Other sessions were building, with a
load average of 15 to 30, which the tails show. A round changes one file four ways, and the
changes are 3 ms apart for the bare backends and at least 100 ms apart for the follower (its
`HOLD` between two reports of one file). Each row is 200 samples, in µs.
"Event" is the change → the event reaching the process. "Report" is the change → the path
coming out of `Changes::next`, after the 2 ms quiet window, the stamp and the hand-off.

```sh
cargo nextest run -p slopty-worker --release --test fswatch --run-ignored only --no-capture
```

| path | change | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| follower (report) | write in place | 3681 | 7532 | 12744 | 17635 |
| follower (report) | rename over | 3980 | 9940 | 23941 | 31095 |
| follower (report) | delete | 3655 | 7275 | 12874 | 29546 |
| follower (report) | create | 3719 | 8115 | 16550 | 26140 |
| bare kqueue (event) | write in place | 111 | 180 | 339 | 394 |
| bare kqueue (event) | rename over | 105 | 149 | 503 | 690 |
| bare kqueue (event) | delete | 89 | 138 | 368 | 772 |
| bare kqueue (event) | create | 104 | 147 | 414 | 1161 |
| FSEvents via `notify` 8.2 (event) | write in place | 11489 | 14689 | 17307 | 19214 |
| FSEvents via `notify` 8.2 (event) | rename over | 11680 | 14168 | 15114 | 24454 |
| FSEvents via `notify` 8.2 (event) | delete | 11510 | 13973 | 15283 | 18388 |
| FSEvents via `notify` 8.2 (event) | create | 11622 | 14215 | 17616 | 21873 |

The watching process's own CPU, while another process writes 5 000 times into a folder beside
the watched file: the follower 0.68 ms (the child's spawn, and no event at all), `FSEvents` on
the file's folder, non-recursive, 19.7 ms. An `FSEvents` stream carries its subtree and filters
afterwards. A first comparison, a throwaway program with the same two watches, on the external
APFS volume (`/Volumes/Lacie`) gave FSEvents 9.4 to 11.2 ms p50 and kqueue 0.18 to 0.26 ms p50.

The same test on Linux: Debian bookworm, aarch64, in Docker Desktop's VM capped at two CPUs, run
as an unprivileged user. Only the follower is measured here. `IN_CLOSE_WRITE` and `IN_MOVED_TO`
end a change without the quiet window, and a delete waits it out.

```sh
cargo zigbuild -p slopty-worker --release --tests --target aarch64-unknown-linux-gnu
docker run --rm --platform linux/arm64 --cpus 2 -u 1000:1000 \
  -v "$PWD/target/aarch64-unknown-linux-gnu/release/deps":/t:ro debian:bookworm-slim \
  sh -c '/t/fswatch-* --ignored --nocapture'
```

| path | change | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| follower (report) | write in place | 671 | 3261 | 4865 | 8065 |
| follower (report) | rename over | 820 | 4274 | 5179 | 5301 |
| follower (report) | delete | 4488 | 7905 | 10305 | 13176 |
| follower (report) | create | 550 | 3355 | 4637 | 6273 |

Both tables came from the module's own sources built as a crate of their own. At the time,
another session's half-finished protocol change kept `slopty-worker` from compiling, and the
test and module files are the same ones.

What this says (docs/decisions/workspace.md, "File tiles follow the disk on the kernel's
events"): the old poll showed a change 500 ms late on average and 1 s at worst. The follower
shows it in about 4 ms on macOS and under 1 ms on Linux, and most of the macOS figure is the
quiet window that makes one save one send. `FSEvents` would add 11 ms before any window of our
own. Its subtree also costs CPU in proportion to unrelated writes nearby.

## 2026-09-30 — gpui-fast: number shaping (#21), the in-motion replay budget, and the pins on Slopty's frames

Mac Studio M1 Max, macOS 27.0, load average 15–38 (other sessions building), so wall times
are noisy and **instructions per frame** decide. `gpui_perf --headless` draws through GPUI's
test window (build, layout, prepaint, paint; no GPU).

```sh
cd .research/gpui-fast
IPHONEOS_DEPLOYMENT_TARGET= cargo build -p gpui_perf --release     # once per variant, binary copied
for r in 1 2 3; do for b in main with21; do ./$b --headless --retention on --frames 200 --json $b-$r.json; done; done
```

**longbridge #21 (numbers put together from their glyphs)**, fork `a5704c5` against `ee5c2ff`
(the merge), three alternating runs, median instructions (M) per frame and allocations:

| scenario | before | after | change | allocations |
| --- | --- | --- | --- | --- |
| workspace-quotes | 23.24 | 22.17 | −4.6% | 9020 → 8258 |
| workspace-hover | 22.41 | 21.42 | −4.4% | 8688 → 8007 |
| workspace-rowviews-quotes | 19.33 | 18.26 | −5.5% | 7359 → 6597 |
| table-ticks-many | 108.38 | 103.35 | −4.6% | 32030 → 28328 |
| table-virtual-scroll | 16.51 | 16.20 | −1.9% | 4868 → 4671 |
| strip-scroll, strip-output, strip-readout | 7.81, 2.31, 2.62 | 7.80, 2.31, 2.62 | ±0.1% | unchanged |
| every other scenario | | | −0.5 to +0.6% | unchanged |

Where numbers change the saving is 4–5.5%; a line that is not a number costs a byte scan
and nothing measurable, the terminal strip included. `1cc6b5c` checks it against CoreText
in the system faces (610 numbers × every UI weight, Menlo, Monaco, Helvetica Neue, 9–22 pt).

**The replay budget in motion** (`bc80d7a`). A profile of `strip-scroll` (`sample`, 8 s) put
5–6.5% of `Window::draw` in the bounds tree's replay searching changed bounds: a frame that
moves everything spends the whole 32,768-comparison budget and then builds the grid anyway.
The frame after one that ran out now replays on a smaller budget; a frame at rest spends
none and hands the next the whole budget again. One binary with the smaller budget read from
the environment, two runs each, instructions against the full budget:

| scenario | 2,048 | 4,096 | 8,192 (taken) |
| --- | --- | --- | --- |
| list-uniform-scroll | −14.4% | −13.6% | −11.6% |
| list-select | −4.8% | −4.6% | −3.9% |
| strip-scroll / strip-spring | −3.0% | −2.8% / −2.9% | −2.2% |
| strip-scroll-keyed | −3.3% | −3.1% | −2.5% |
| table-virtual-scroll | −2.7% | −2.6% | −2.2% |
| workspace scrolls and quotes | −0.9 to −2.1% | −1.0 to −2.2% | −0.9 to −1.9% |
| workspace hovers | +0.5 to +1.2% | −0.1 to +0.5% | −0.7 to −1.0% |
| every other scenario | −1.3 to +0.1% | −0.9 to +0.2% | −0.8 to +0.3% |

8,192 is the only setting that costs no scenario anything. `gpui_perf --headless --verify`
paints identical frames retained and from scratch in all 45 scenarios.

**Slopty's own frames across the pins**, `slopty-ui`'s ignored measure tests built from `HEAD`
(`e7146b59`) in a scratch copy of the tree, once per gpui-fast pin, five alternating rounds,
median p50 in ms. `f994c34` is what `HEAD` pins; `0dd9e96` added focus-read tracking, element
moves and keyed paint; `6db1629` adds #19's follow-ups, #21, trackpad gestures and zed
`39b53293`; `bc80d7a` is `6db1629` with the in-motion budget.

```sh
cargo test -p slopty-ui --lib --no-run    # per pin (cargo update -p gpui --precise <rev>), binary copied
<bin> --ignored --exact --nocapture --test-threads 1 <the 12 measure_* tests>
```

| frame | f994c34 | 0dd9e96 | 6db1629 | bc80d7a |
| --- | --- | --- | --- | --- |
| face following an answer, 80 turns | 1.022 | 0.953 | 0.948 | 0.954 |
| face panning, 80 turns | 0.782 | 0.736 | 0.707 | 0.716 |
| navigator docked, 60 shells + 60 notes | 0.880 | 0.757 | 0.783 | 0.816 |
| navigator hidden | 0.344 | 0.327 | 0.327 | 0.333 |
| palette glide, 300 lines | 0.276 | 0.263 | 0.266 | 0.270 |
| a frame of motion beside the chrome | 0.468 | 0.476 | 0.485 | 0.468 |
| echo beside the chrome (workspace) | 0.776 | 0.826 | 0.791 | 0.782 |
| the keyboard moving beside the chrome | 1.747 | 1.735 | 1.819 | 1.752 |
| pointer frame beside the chrome | 0.223 | 0.232 | 0.227 | 0.227 |
| neighbour echo beside a dense grid | 0.101 | 0.100 | 0.104 | 0.103 |
| stream frame beside the chrome | 0.123 | 0.122 | 0.122 | 0.120 |
| workspace frame over a large registry | 0.608 | 0.625 | 0.678 | 0.623 |

- The conversation face got 7–10% cheaper and the docked navigator 7–14% since `f994c34`;
  the rest moves within this load's noise (±4%, one outlier at +11.7% that the next variant
  does not repeat).
- The keyboard moving between shells does not get cheaper with focus-read tracking
  (`0dd9e96`): Slopty still refreshes the window on a focus change, so every view is built
  anyway. Dropping that refresh is the Slopty half of the change (the UI lane's WIP).
- `strip-*` in `gpui_perf` paints the grids of five offscreen tiles every frame (culled
  glyph by glyph); Slopty renders only the tiles near the viewport, so the strip scenarios
  overstate `paint_line` for Slopty. Slopty's terminal element does not use
  `Window::paint_keyed` yet, which `gpui_perf` puts at −58% to −88% of a grid's instructions
  per frame (SLOPTY.md in the fork).

**The tab order, sorted when read** (fork `4e47901`). A `samply` profile of Slopty's measure
tests put GPUI's tab-stop sum tree at 6–11% of the workspace frames and 25–35% of the echo
frames beside 60 shells and 60 notes: every tracked focus handle is inserted into the tree
as it is painted, and every view drawn from last frame replays its inserts. The fork now
records what was painted and sorts it only when the focus is moved by the keyboard or
accessibility counts the tab stops. Slopty's measure tests, fork main `fdb39c6` against the
change, two sets of five alternating rounds (the second under load 16–24), median p50 ms:

| frame | before | after | before | after |
| --- | --- | --- | --- | --- |
| neighbour echo beside a dense grid | 0.107 | 0.061 (−43%) | 0.167 | 0.075 (−55%) |
| echo beside a focused face | 0.086 | 0.052 (−40%) | 0.105 | 0.060 (−43%) |
| echo beside 2 drawn notes of 64 KB | 0.271 | 0.120 (−56%) | 0.411 | 0.184 (−55%) |
| stream frame beside the chrome | 0.139 | 0.079 (−43%) | 0.250 | 0.129 (−48%) |
| workspace frame over a large registry | 0.727 | 0.553 (−24%) | 1.060 | 0.592 (−44%) |
| pointer frame beside the chrome | 0.237 | 0.195 (−18%) | 0.434 | 0.338 (−22%) |
| echo beside the chrome (workspace) | 1.040 | 0.967 (−7%) | 1.352 | 1.218 (−10%) |
| navigator docked / hidden | 0.877 / 0.359 | 0.785 / 0.381 | 1.395 / 0.588 | 1.188 / 0.512 |
| a frame of motion; the keyboard moving | 0.516; 1.971 | 0.491; 1.813 | 0.781; 3.024 | 0.755; 2.887 |
| face following; palette glide | 1.190; 0.287 | 1.088; 0.344 | 1.422; 0.432 | 1.575; 0.437 |

The face and the palette move both ways between the two sets: noise. Profiled again, the tab
stops are 0.3–0.8% of the echo frames. In `gpui_perf`, the strip scenarios save 2–4.5% of
their instructions and 39 allocations a frame (8 tiles), and every other scenario is within
±0.6%.

Logs: `target/logs/gpui-fast-2026-09-30/` (the `gpui_perf` JSON per run, `sab/`, `sab2/`, `sab3/` for the Slopty
rounds, and the `sample` profiles).

## 2026-09-30 — a gesture's events keep the client's spacing; a window stream's pointer reaches the view

macOS 26.6.2 in a tart guest (4 cores, 8 GB, `--no-graphics`), from the slopty-input `inject`
archive; nothing posted on this Mac. Every event goes to applications the test starts itself
(`tests/support/gesture_app.rs`). The injector's own cost is on this Mac (mac-studio, M1 Max,
macOS 27.0, test profile, other sessions' builds running), with nothing built or posted.

```sh
cargo xtask vm live -p slopty-input --test inject          # the live tests below
cargo nextest run -p slopty-input --test cost --run-ignored only \
  -E 'test(the_injector_s_own_cost_per_input)' --no-capture
```

**A swipe between pages, by its events' timestamps.** Twelve 10-point reports at 120 a
second then a lift, through the injector with the scroll's gesture, into an app following it as
Safari does; 12 swipes per cell, each in an app of its own. The guest's scheduler held the posts
up to 44.6 ms at worst on top of the arrivals' own pattern.

| arrivals | stamped when posted: followed / turned | at the client's spacing: followed / turned |
| --- | --- | --- |
| steady, every 8.3 ms | 12 / 8 | 12 / 12 |
| jittered, 0–12 ms late | 10 / 4 | 11 / 12 |
| in pairs every 16.7 ms | 12 / 6 | 10 / 10 |
| all | 34 / 18 of 36 | 33 / 34 of 36 |

"Followed" is the tracking past −0.05 before the lift, which the guest's display reports once
a frame and unevenly; "turned" is the lift's verdict, ended and run on to −1.0. A second pairs
run at the client's spacing turned 9 of 12, jittered 12 of 12. Setting the events' timestamps to
an even 8.3 ms timeline instead turned 15 of 15 against 8 of 15 as posted, in the first
look (raw posts, window-bound). With the kept test stamped by its client, the whole `inject`
suite passed its last 6 runs in the guest.

Mac Mouse Fix's fields on the scroll's gesture (41 = 33231 and 134 = the phase), 20 swipes each
at 120 a second, raw posts: 14 turned without, 8 with; window-bound, 14 without. Noise either
way, and the fields are its pre-27 dock swipe's.

**The injector's own work per input** (`the_injector_s_own_cost_per_input`, 200 000 inputs,
three rounds, ns):

| input | ns |
| --- | --- |
| trackpad scroll and its gesture, left to the system's stamp | 125–129 |
| trackpad scroll and its gesture, at the client's spacing | 192–207 |
| pointer move | 145–157 |
| pointer move in a drag | 68–97 |

The stamp is one read of the host clock, about 70 ns; posting a scroll costs 61–91 µs at the
median in the guest (`a_trackpad_scroll_s_post_cost`: alone 61.2 / 75.1 / 91.0 µs p50 over three
runs, with its gesture 62.6 / 91.6 / 91.9), 25–38 µs on this Mac (the entry above).

**What of a pid-posted event reaches a regular app's view** (the probe behind
`a_window_stream_s_pointer_reaches_a_regular_app_s_view_bound_to_its_window`): a click, a
press-drag-release, a scroll and a pinch, into the view of a regular app at the normal level.

| the app | posted plain (window 0) | bound to its window (field 51 + `CGEventSetWindowLocation`) |
| --- | --- | --- |
| not active (none asked, or `NSRunningApplication` asked) | nothing | scroll; each press is only `acceptsFirstMouse:` |
| switched to by `_SLPSSetFrontProcessWithOptions`, window not key | nothing | the first press makes it key; then everything |
| switched to, and its make-key records | nothing | everything |
| its title bar clicked through the HID tap | nothing | everything |

Uncovered or covered by another window at the same place, the same. `NSRunningApplication`
from the test process never activated the app while another was active (1.5 s watched, every
run); the window server's switch made it active and its window key in 11 ms.

Logs: `target/logs/wire-probe-1.log` (the pid-post matrix), `target/logs/wire-live-5.log` (the
suite with the switch), `target/logs/wire-mmf*.log`.

## 2026-09-30 — OSC 133 marks from libghostty's semantic prompt effect instead of our scanner

The engine used to scan every PTY read for OSC 133 itself and feed the terminal up to each
mark. It now takes the marks from libghostty's semantic prompt and reset effects (ghostty
#14479, carried on `aislopware/ghostty` `f96ae0c96`) and resolves their rows once the write
has settled. Before: `vendor/ghostty` `61eea99c7`, the scanner. After: `f96ae0c96`, the effects.
Release build, retired instructions per op (the numbers `xtask/budgets.toml` holds), one run
each:

```sh
nice -n 10 cargo nextest run --release -p slopty-engine --run-ignored only \
  -E 'test(/_cost$/)' --no-capture --no-fail-fast
```

| series | before | after | change |
| --- | --- | --- | --- |
| `checkpoint_cost.fill_engine` (10 024 coloured lines, 651 560 bytes in 64 KiB writes) | 32 659 427 | 31 254 406 | −4.3 % |
| `checkpoint_cost.fill_raw_vt` (the same bytes, bare `Terminal::vt_write`) | 26 992 830 | 27 003 263 | +0.04 % |
| engine overhead over the bare write | 5 666 597 | 4 251 143 | −25 % |
| `checkpoint_cost.replay_64k_chunks` | 35 012 040 | 33 034 201 | −5.6 % |
| `osc_write_cost.per_osc` (OSC 8 links, titles and marks) | 7 224 | 7 108 | −1.6 % |
| `osc_write_cost.per_osc_raw_vt` | 6 932 | 6 933 | 0 |
| engine overhead per OSC | 292 | 175 | −40 % |
| `frame_cost.write` (one typed byte into the terminal: bytes to state) | 849 | 804 | −5.3 % |
| `osc_scan_cost.per_osc` | 113 | — | the scanner is gone |

Wall time for a 651 560-byte fill: 2.41 ms before, 2.33 ms after (n = 1 each; the bare write
took 1.52 and 1.61 ms in the same runs, so wall time is inside this machine's noise). Per
64 KiB write, the time from the bytes to the terminal's state is the fill over its ten
writes, about 0.23 ms either way. Every other `_cost` series moved by less than 0.3 %.

The saving is the scan's pass over every byte (a `memchr` for ESC and a state step per
escape), which the terminal's own parser already makes. A mark costs more than it did: the
effect builds its event, and the engine tracks the cursor row with a grid ref (one small
allocation in libghostty per mark) to resolve after the write. Marks come a few per command,
so that does not show in any series. `xtask/budgets.toml` still lists
`engine.osc_scan_cost.per_osc`, which no longer exists.

Logs: `target/logs/osc133-before.log`, `target/logs/osc133-after.log`.

## 2026-09-30 — the keyboard moving builds neither the workspace nor the strip

gpui-fast (`beb580e`, pinned at `867b4d4`) builds again only the views whose answer to a focus
question changed, but Slopty read the focus as a whole in the workspace's frame
(`apply_pending_focus`) and the strip's (`Drawn::keys` for `cacheable`), so any focus move built
both, as the refresh the fork dropped did (`docs/decisions/ui.md`, "The workspace reads no focus
as a whole"). Headless workspace, test profile (`dev`, deps at `opt-level = 3`), mac-studio, load
average 19–22 from other sessions. Two test binaries from one tree, **before** with the two
reads put back and **after**, run in turn for three rounds. p50 and p95 in ms, one cell per
round.

| arm | before | after |
| --- | --- | --- |
| the keyboard moved by a view of its own, 60 shells + 60 notes, 400 moves (p50) | 0.325, 0.311, 0.284 | 0.131, 0.133, 0.143 |
| the same (p95) | 0.674, 0.396, 0.347 | 0.190, 0.152, 0.162 |
| the workspace and the strip built in those 420 moves | 420 and 420 | 1 and 0 |
| a move the workspace asks for (`pending_focus`), 200 moves (p50) | 1.115, 1.128, 1.067 | 1.070, 1.024, 1.045 |

- **A focus move costs its two views.** The move a view makes itself (a click in a body, a find
  bar giving the keys back) fell by 55–60 %: what is left is the two shells built again for
  their carets and rings. The workspace's single build is the first move after the crowd opened.
- **A move the workspace asks for is unchanged,** inside the noise: the layout's focus moves
  with it, which builds the workspace and the strip anyway, and the strip is still notified
  once for the frame after.

```sh
# the two arms: `after` is the tree; `before` puts back `window.focused(cx)` in
# `apply_pending_focus` and `Drawn::keys` in the strip and `cacheable`
cargo test -p slopty-ui --lib --no-run   # copy target/debug/deps/slopty_ui-<hash> per arm
cd crates/slopty-ui && for r in 1 2 3; do for b in before after; do
  ../../target/ab-focus/$b measure_the_keyboard_moving --ignored --nocapture | grep MEASURE
done; done
```

## 2026-09-30 — a dead link found by a resume, against its silence (mac-studio, e2e app build)

The real app and a real worker on loopback, the worker behind a UDP proxy the test cuts
(`slopty_e2e::cut`: every datagram dropped both ways until the next QUIC Initial, as a sleeping
Mac's dead path drops them). Timed from the cut, or from the resume handed over the test socket,
to the first frame whose dump shows the worker's shell drawn from a newer link
(`docs/decisions/transport.md`, "A resume probes every link at once").

| a dead link found by | deaths | p50 | p95 |
| --- | --- | --- | --- |
| its silence (five missed keep-alives, then the redial) | 5 | 6600 ms | 7651 ms |
| a resume's probe (`woke` and `path-changed` in turn) | 20 | 267 ms | 268 ms |

An earlier run the same evening, before the review's fix to a resume lost between links,
read 6583 / 7634 and 267 / 269 ms.

- **The probe's floor is the time.** Loopback's round trip is well under a millisecond, so the
  probe waits its 250 ms floor (`workers::PROBE_FLOOR`) and the new link lands in the next
  17 ms. The floor is there for a radio just back from sleep. A link that answers is kept, and
  the test's first resume, over the live link, dialled nothing.
- **The tiles were dimmed on every one of the twenty.** That no frame shows them empty is
  `a_link_in_doubt_sets_its_tiles_back_until_it_is_live`'s to hold (`slopty-ui`).

```sh
cargo xtask e2e app --filter 'test(a_resume_brings_a_dead_link_back_at_once)' 2>&1 | grep MEASURE
```

## 2026-09-30 — ghostty on 76895d97b, libghostty-rs fixes, and a frame's dirty flags in one call

Before: `vendor/ghostty` `9f8e1b28a` (the fork on ghostty `acf1209ee`), libghostty-rs
`eb3a963`. After: `89c9624f0` (the fork on ghostty `76895d97b`, plus the render state's row
dirty view) and libghostty-rs `2f8b499` (the view as `Snapshot::dirty_rows`, and the fixes in
decisions/terminal.md, "ghostty on 76895d97b"). The engine reads each row's dirty flag from the
view instead of one call per row. Release build, retired instructions per op, one run each
(mac-studio, other lanes building):

```sh
SLOPTY_BENCH_OUT=target/bench/engine-after.jsonl nice -n 10 cargo nextest run --release \
  -p slopty-engine --run-ignored only -E 'test(/_cost$/)' --no-capture --no-fail-fast
```

| series | before | after | change |
| --- | --- | --- | --- |
| `frame_cost.write` (one typed byte into the terminal) | 829 | 779 | −6.0 % |
| `frame_cost.take_frame` (that byte's frame, 200×60) | 45 807 | 45 119 | −1.5 % |
| `frame_cost.take_frame_unchanged` | 1 013 | 983 | −3.0 % |
| `scroll_frame_cost.80x24` (an Enter at a bottom prompt) | 125 123 | 122 850 | −1.8 % |
| `scroll_frame_cost.200x60` | 365 921 | 361 927 | −1.1 % |
| `osc_write_cost.per_osc` (links, titles, marks) | 7 108 | 7 108 | 0 |
| `osc_write_cost.per_osc_raw_vt` | 6 930 | 6 941 | +0.2 % |
| `checkpoint_cost.fill_engine` (651 560 bytes of coloured lines, n = 1) | 31 108 672 | 31 919 661 | +2.6 % |
| `checkpoint_cost.fill_raw_vt` (the same bytes, bare `vt_write`, n = 1) | 26 970 583 | 27 242 697 | +1.0 % |

Plain output and OSC-heavy output cost what they did. The two fill series are one sample
each: three more runs of the after build gave `fill_engine` 31 743 537, 30 969 022 and
31 425 283, and `fill_raw_vt` 27 058 712, 27 292 761 and 27 017 066, so a single run moves by
±1.5 % and the before number sits inside that spread. The scroll frames gain from the
dirty view: they visit every row, and each clean row cost a call to learn it was clean. Every
other `_cost` series moved by less than 0.4 %.

**`frame_cost.take_frame` against its budget of 21 899: a bigger screen, not a regression.**
The uncommitted change that added the sparse frame path (only the rows libghostty marks dirty
are visited) also moved `frame_cost` from 60×12 to 200×60 and kept the series name, so the
new number was compared with a budget taken at the old size. The engine was bisected from
`e7146b59` in a throwaway worktree (`target/bisect-engine`, its own target dir):

| build | 60×12 | 200×60 |
| --- | --- | --- |
| `e7146b59`: engine, libghostty-rs `8a45222`, ghostty `741a800e8` | 21 899 | 61 366 |
| the same with ghostty `89c9624f0` | 21 899 | — |
| `e7146b59` engine on libghostty-rs `eb3a963` and ghostty `9f8e1b28a` | 21 876 | — |
| this tree | 18 887 (−13.8 %) | 45 061 (−26.6 %) |

`take_frame` after a typed byte, instructions per op. The ghostty and libghostty-rs moves
leave the frame's cost where it was. The engine's changes (the sparse path, one flag read per
row) make it cheaper at either size. `frame_cost` now measures both sizes and names them:
`take_frame.60x12`, `take_frame.200x60`, `take_frame_unchanged.60x12` (901),
`take_frame_unchanged.200x60` (1 047), and `write` (795, timed at 60×12). The budgets need
`cargo xtask bench --update-budgets` for the new names.

The two sets of `frame_cost`, `scroll_frame_cost` and `fetch_lines_cost` lines in the
`engine-before.jsonl` and `engine-after.jsonl` files above were one run each, appended to
files that an earlier session's measurement of those three series had already written:
`slopty_testkit::bench` appends to `SLOPTY_BENCH_OUT`. `cargo xtask bench` writes a fresh
`target/bench/measured.jsonl` and reads only that. A fresh file (`dup-probe.jsonl`) holds
one line per series. Only the logs' `BENCH` lines are quoted in this entry.

Logs: `target/logs/engine-bench-before.log`, `target/logs/engine-bench-after.log`.

## 2026-09-30 — the remote pointer as the system cursor, and the cursor seed

Gap-audit items 1 and 7 (`docs/decisions/input.md`, "The remote pointer is the system cursor;
its shape follows the window server's seed"). Mac Studio M1 Max, macOS 27.0.1 (26A434), one
1920 × 1080 display at 1×, load average 28–39 from other sessions, debug builds.

**Pointer motion.** Before, the view drew the worker's picture at the pointer, so every move
over a remote tile was one app frame, 19 ms notify → glass ("a remote pointer drawn where the
client put it", above). As the system cursor, the picture moves on the window server's cursor
path with the hand, as a local pointer does, and the app draws nothing. The glass time of the
cursor plane cannot be read without a camera, so the claim is structural and the test counts
frames: in the fork, a tile styled `CursorStyle::Image` takes the picture as the entering move
is dispatched, and 21 moves over it build the tile 0 times
(`the_pointer_takes_the_picture_as_it_enters_and_moving_over_it_draws_nothing`). In Slopty's
view, a window stream's moves, new pictures and late echoes draw 0 frames, where each move was
1 (`a_window_streams_pointer_is_the_system_pointer_in_the_workers_picture`); on a display, a
move or a sample inside the hold draws 0, and the hold starting or ending draws 1
(`a_displays_pointer_follows_the_worker_unless_this_client_moves_it`).

**A shape change on the client** (fork, `a_shape_change_is_built_once_and_set_in_microseconds`,
32 pictures of 64 × 64 at 2×, twenty rounds; four runs, p50 / p99):

| step | p50 | p99 |
| --- | --- | --- |
| build the `NSCursor` (first time a picture is seen) | 12–40 µs | 35–372 µs |
| point the id at a picture built before | 0.63–0.83 µs | 2.6–10.8 µs |
| `-[NSCursor set]` | 56–129 µs | 0.2–7.7 ms |

`NSCursor.currentCursor` is the cursor set as soon as `set` returns: no frame and no run-loop
turn stand between the picture arriving and the system cursor taking it.

After the review (2026-10-01, load 27–32), a cache hit compares the pixels behind the key: the
same run gives build 15.8 µs / 126 µs, a picture built before 2.1 µs / 94 µs (the 16 KB compare
of a 64 × 64 picture), `set` 337 µs / 10.4 ms (p50 / p99). The picture changes only when the
worker's cursor does, so the compare costs nothing per move.

**Seeing the change on the worker** (`slopty-capture`,
`the_global_cursor_is_the_system_cursors_picture_and_its_seed_costs_nanoseconds`, 200 reads
each; four runs, p50 / p99):

| read | p50 | p99 |
| --- | --- | --- |
| `CGSCurrentCursorSeed` | 83–417 ns | 0.7–1.3 µs |
| `CursorWatch::poll`, cursor unchanged (10 000 polls, 0 reads) | 0–42 ns | 42–84 ns |
| `CGSGetGlobalCursorData`, the picture | 42–219 µs | 0.2–14.9 ms |
| before: `currentSystemCursor` and its picture, every tick | 193–474 µs | 0.5–13.5 ms |

- **Latency.** The worker looked at the picture every 33 ms, so a new shape waited up to 33 ms
  (16.7 ms on average) before it was even read. With the seed polled every 8.3 ms, it waits up
  to 8.3 ms (4.2 ms on average), and the one round trip happens only on a change.
- **Cost.** A tick with the cursor unchanged was a 0.2–0.5 ms window-server round trip plus a
  picture copy; it is now a seed read of tens of nanoseconds. The first read in a process
  connects to the window server: 0.7–0.9 s here, against the 11 s AppKit's first
  `currentSystemCursor` took.
- **Wakeups** (with `worker.patch`). The shape loop ticked 30 times a second whether or not
  the pointer was over the target. It now waits for the pointer to come over the target (1/s
  backstop) and ticks 120 times a second only while it is there.
- **Layout pinned.** On this macOS the global data is the same picture, pixel for pixel, as the
  1× representation of `currentSystemCursor`: 28 × 40 pixels at 1×, hotspot (5, 5).
- **Colour order** (2026-10-01,
  `a_coloured_cursor_reads_back_in_bgra_with_its_hotspot_at_its_scale`). A test app's 16-point cursor of red, green, blue and half-covered white reads back as BGRA
  `[0, 0, 255, 255]`, `[0, 255, 0, 255]`, `[255, 0, 0, 255]` and `[128, 128, 128, 128]`,
  16 × 16 at 1×, hotspot (3, 5): blue first, premultiplied, exact on this display. The test
  holds the cursor for 0.47–0.56 s. A Retina display is still to be checked.
- **Wakeups, tested** (`the_shape_loop_reads_the_picture_only_when_the_seed_moves`). Off the
  target: 0 seed reads in 200 ms. Over it: 10–40 seed reads in 250 ms (120 Hz with the test's
  scheduling), 1 picture read, then 1 more when the seed moves; off again, 0.

```sh
# fork (.research/gpui-fast, main at 132dbc1)
cargo test -p gpui --lib fast::tests::cursor
cargo test -p gpui_macos --lib fast::cursor -- --nocapture | grep MEASURE
# worker: reads the cursor, never the screen; the colour test shows its own cursor for ~0.5 s
SLOPTY_SCREEN_E2E=1 cargo nextest run -p slopty-capture --no-capture -E 'test(/cursor/)' | grep -E 'MEASURE|quadrants'
cargo nextest run -p slopty-ui -E 'test(/pointer/)'
cargo nextest run -p slopty-worker -E 'test(the_shape_loop)'
```

## 2026-09-30 — two stripes, capture to glass

Mac Studio M1 Max (two `ave2` encode engines), macOS 27.0, the `dev` profile (optimized), run
from `/tmp` under `nice`. The machine was busy with other sessions' builds and tests throughout:
load 42–121, and their tests code on the same engines, so these read as a loaded machine, not a
quiet one. The question: what the built stripes (`docs/decisions/video.md`, "Two stripes, as
built") do to a stream's encode and to capture → glass, against the same stream coded whole.
`capture_to_glass_striped_against_whole` streams the drawn display through the worker's real
pipeline (capture stand-in, `VideoToolbox` sessions, packetizer, loopback) into the client's
reassemblers, decoders, stitch and pacer, 15 s a run after a second's warm-up, a click every
80–150 ms, 60 frames a second asked, stripes forced with `Knob::On` and off with `Knob::Off`,
two interleaved rounds. "Encode" is submit → the capture's last stripe back, as the worker's
`ScreenStats::encode` counts it (the slower stripe's). `stripes_copy_cost` times the gate
(`slopty_codec::stripes::side_by_side`) and both stripes' copies alone.

```sh
cargo test -p slopty-worker --lib --no-run      # target/debug/deps/slopty_worker-<hash>
mkdir -p /tmp/slopty-stripes && /bin/rm -f /tmp/slopty-stripes/slopty_worker
cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-stripes/slopty_worker && cd /tmp/slopty-stripes
SLOPTY_GLASS_SECONDS=15 SLOPTY_GLASS_ROUNDS=2 TMPDIR=/tmp nice ./slopty_worker --ignored --exact screen::synthetic::tests::capture_to_glass_striped_against_whole --nocapture
cargo test -p slopty-codec --lib --no-run       # target/debug/deps/slopty_codec-<hash>
nice ./codec_lib --ignored --nocapture --exact stripes::imp::tests::stripes_copy_cost
```

**Encode per frame, one picture → two stripes** (p50 / p95 ms, rounds 1 and 2), and the frames
a second the encoder made:

| size | one picture | two stripes | frames a second: one → two |
| --- | --- | --- | --- |
| Retina 3024 × 1964 | 15.5 / 23.8, 15.8 / 25.1 | **9.5 / 13.8, 9.8 / 23.5** | 49.3 → 56.1, 43.6 → 49.2 |
| 4K 3840 × 2160 | 20.6 / 26.3, 21.0 / 30.4 | **12.3 / 15.2, 12.7 / 23.3** | 42.6 → 56.9, 36.7 → 42.3 |
| 5K 5120 × 2880 | 35.3 / 38.8, 35.8 / 51.2 | 38.0 / 43.2, **23.1 / 55.9** | 25.4 → 24.3, 21.4 → 19.6 |

**Capture → painted** (p50 / p95 ms), the same runs: Retina 35.8 / 56.0 → 17.6 / 40.0 and
36.7 / 75.6 → 23.2 / 56.4; 4K 38.7 / 62.2 → 29.3 / 46.3 and 49.5 / 77.0 → 35.1 / 67.6; 5K
67.7 / 75.6 → 61.8 / 82.3 and 75.3 / 137.4 → 63.3 / 127.5. No frame was lost, NACKed or
refreshed in any run, and no decode failed.

**Seam tears** (a capture that went up with one stripe's previous picture, the late one past the
stitch's 16.7 ms wait): Retina 6 and 10, 4K 4 and 21, in 750–880 captures a run; 5K 205 and 22
in 300–375.

**The gate and the copies alone** (`stripes_copy_cost`, load 100): whole against striped with
both copies, 3024 × 1968 15.8 → 9.8 ms (4:4:4 15.9 → 10.6), 3840 × 2160 20.9 → 12.5 (21.1 →
13.7), 5120 × 2880 35.2 → 21.3 (35.9 → 23.0); both copies at p50 0.42, 0.55 and 0.96 ms at
4:2:0 and 1.7, 2.1 and 3.2 ms at 4:4:4, about twice what the quiet machine measured.

What it says:

- **Retina and 4K get the ruling's halving on the real pipeline.** The encode falls 38–40 % at
  both sizes, the encoder keeps up with 56–57 captures a second where the whole picture made
  37–49, and capture → glass falls 7–19 ms at the median. The 4K stripes are inside the
  ruling's 14 ms bar at p50 in both rounds; their p95 is not, on this load.
- **5K is where the load shows.** In the first round the two stripes took longer than the
  whole picture (38.0 against 35.3 ms), the tears went to 205, and the encoder made fewer
  frames. The second round's stripes were back to 23.1 ms at the median, with a p95 of 56. Two
  stripes only pay while both engines are free for them: other processes coding on the same
  engines, as the other sessions' tests were, put the stripes behind their frames. The gate's
  timing is taken once per size and process, so a machine that becomes busy later keeps
  stripes it no longer gains from; the spend half of the gate does not see it.
- **The copies cost more under load** but stay under a millisecond for both 4:2:0 stripes up to
  5K, on the stripes' own threads, beside a 9–23 ms encode.

## 2026-09-30 — the grid's rows under keys: frame cost with `paint_keyed` and without

The terminal element paints each row's backgrounds, underlines, sprites, words and
strikethroughs as stretches under keys (`Window::paint_keyed`, gpui-fast), so a row that did not
change is drawn again from the last frame's scene, in place or moved by whole device pixels
(`docs/decisions/terminal.md`, "The grid's rows are painted under keys"). The two arms are one
binary: `TerminalView::set_keyed_paint(false)` paints every row afresh, as before. A focused
200 × 60 screen of dense coloured text under a blinking block cursor; each sample is a frame
applied and drawn, 600 frames a case after 60. "Bare" is GPUI's test text system (a glyph is a
lookup, no sprite); "sprites" is the paint oracle's (`terminal/view/paint_oracle.rs`), where every
glyph is a sprite in the scene as on the glass. Release, mac-studio, gpui-fast `132dbc1`, two
runs started at a load average of 18.5 and 17.3 (a first run at 48–90 gave the same medians):

```sh
cargo test -p slopty-ui --release --lib terminal_frames_cost -- --ignored --nocapture
```

p50 / p95 in µs, runs 1 and 2:

| case | bare, afresh | bare, keyed | sprites, afresh | sprites, keyed | stretches a keyed frame: painted / again (moved) |
| --- | --- | --- | --- | --- | --- |
| unchanged | 202 / 258, 210 / 283 | **22 / 26, 24 / 39** | 758 / 1592, 745 / 940 | **140 / 306, 134 / 187** | 0 / 120 (0) |
| cursor blinking | 200 / 238, 210 / 298 | **26 / 35, 28 / 29** | 755 / 1627, 744 / 971 | **149 / 307, 148 / 212** | 1 / 119 (0) |
| a key echoed at the prompt | 206 / 251, 215 / 324 | **32 / 46, 31 / 52** | 752 / 1499, 728 / 852 | **150 / 221, 148 / 215** | 1 / 118 (0) |
| a line of output a frame | 286 / 386, 299 / 442 | **114 / 186, 119 / 178** | 852 / 1699, 830 / 998 | **434 / 933, 419 / 588** | 3 / 117 (117) |
| `yes`, a screen of one line a frame | 212 / 319, 166 / 238 | 206 / 292, 210 / 288 | 214 / 390, 186 / 255 | 182 / 356, 192 / 273 | 0 / 60 (58) |
| a screen of new text a frame | 2245 / 4005, 2202 / 4201 | 2543 / 9780, 2250 / 4228 | 3037 / 7744, 2765 / 4633 | 2879 / 8413, 2769 / 4453 | 120 / 0 |
| scrolling the scrollback a line a frame | 275 / 365, 290 / 439 | **109 / 423, 99 / 134** | 850 / 1983, 832 / 1031 | **396 / 499, 398 / 517** | 2 / 118 (118) |

What it says:

- **Typing echo costs a fifth of what it did.** With every glyph a sprite, a key echoed at the
  prompt went from 728–752 to 148–150 µs at p50: one row's two stretches are painted and the
  other 118 are copied. The bare arm says the same, 206–215 to 31–32 µs.
- **Scrolling costs half.** A line a frame through the scrollback went from 832–850 to 396–398
  µs, and a line of output from 830–852 to 419–434 µs: 117–118 stretches move by a row's height
  instead of being painted, and the new row is painted. What is left is the prepaint's rows and
  GPUI's copy of the moved operations.
- **A screen of new text costs the same.** Every row's key is new, so all 120 stretches are
  painted; keying them costs nothing that shows against the 2–3 ms of building the rows.
- **`yes` neither gains nor loses.** Its rows are all one line, already built and shared by the
  row cache, and only its glyph pass keys: 58 of 60 stretches move, the medians are within
  the runs' spread of each other in both text systems.

## 2026-10-01 — debug info for shipped builds: `limited` against `line-tables-only`

Mac Studio M1 Max (10 cores), macOS 27.0, rustc 1.98.1. The `dist` profile (fat LTO, one codegen
unit, `split-debuginfo = "packed"`, `strip = "debuginfo"`) with its `debug` set per arm by
`CARGO_PROFILE_DIST_DEBUG`, sccache off, one target directory per arm and binary, the arms one
after the other. Other sessions were building throughout (load average 11–95), so wall time is
noise; the build columns compare CPU time (user + sys of cargo and every rustc and linker under
it).

```sh
export RUSTC_WRAPPER=
for arm in line-tables-only limited; do
  CARGO_PROFILE_DIST_DEBUG=$arm cargo build --profile dist --target-dir target/dbg-measure/$arm-app \
    -p slopty --bin slopty-app
  CARGO_PROFILE_DIST_DEBUG=$arm cargo build --profile dist --target-dir target/dbg-measure/$arm-worker \
    -p slopty-workerd --bin slopty-worker
done
# incremental: both arms caught up to one tree, then `touch crates/slopty-app/src/lib.rs`
# (`apps/slopty-worker/src/main.rs`) and each arm rebuilt
# sizes: `stat -f %z <bin>`, `du -skL <bin>.dSYM`, `tar -h -czf - <bin>.dSYM | wc -c`
```

Inlined frames: one address every 4 KiB of `__text`, symbolized with its inlined frames by `atos
-i` against the arm's dSYM, and each inlined frame (every one but the outermost) counted as named
with a path (`a::b`, or a mangled `_R` name) or bare (`install`, `{closure#0}`, `poll<…>`).

```sh
otool -l <bin>     # __TEXT,__text: addr, size
awk -v s=<addr> -v n=<size> 'BEGIN{for(a=s;a<s+n;a+=4096) printf "0x%x\n", a}' |
  xargs atos -o <bin>.dSYM/Contents/Resources/DWARF/<name> -arch arm64 -l 0x100000000 -i
```

| | slopty-app, line-tables-only | slopty-app, limited | slopty-worker, line-tables-only | slopty-worker, limited |
|---|---|---|---|---|
| shipped binary | 40 217 120 B | 40 237 872 B (+0.05 %) | 16 194 720 B | 16 193 520 B (−0.01 %) |
| binary, gzip | 17.41 MB | 17.42 MB | 6.39 MB | 6.40 MB |
| dSYM | 191 MiB | 269 MiB (+41 %) | 81 MiB | 119 MiB (+48 %) |
| dSYM, tar + gzip | 41.0 MB | 61.1 MB | 19.8 MB | 28.6 MB |
| clean build, CPU | 1108 + 105 s | 1062 + 57 s | 476 + 28 s | 481 + 33 s |
| rebuild after touching one file, CPU | 441 + 14 s | 453 + 15 s (+3 %) | 143 + 3 s | 152 + 4 s (+7 %) |
| inlined frames named with a path | 6 991 of 31 352 (22.3 %) | 31 141 of 31 147 (100.0 %) | 3 340 of 15 265 (21.9 %) | 14 752 of 15 452 (95.5 %) |
| local symbols left after the strip | 30 976 | 30 994 | 14 563 | 14 564 |

The report a panic leaves (`SLOPTY_CRASH_TEST=panic`, the dSYM beside the binary), the same
in both binaries:

| line-tables-only | limited |
|---|---|
| `slopty_crash::probe::panic` (probe.rs:85) | `slopty_crash::probe::panic` (probe.rs:85) |
| `slopty_crash::probe::fire` (probe.rs:75) | `slopty_crash::probe::fire` (probe.rs:75) |
| `install` (lib.rs:135) | `slopty_crash::install` (lib.rs:135) |
| `slopty_app::main` (main.rs:113) | `slopty_app::main` (main.rs:113) |
| `call_once<fn() -> …, ()>` (function.rs:250) | `<fn() -> … as core::ops::function::FnOnce<()>>::call_once` (function.rs:250) |

What it says:

- **The shipped binary does not change.** Packed split debug info leaves every byte of DWARF in
  the dSYM, and the strip leaves the symbol table, so a binary without its dSYM names its
  outermost frames the same way in both arms.
- **Inlined frames get their paths.** Under fat LTO most frames of a crash are inlined: an
  address resolves to its own function and about six inlined into it (5.8 in the app over 5 410
  addresses, 6.4 in the worker over 2 401). With `line-tables-only` four in five of those are
  bare names; the fifth has a path only because std's own objects were built with linkage
  names. With `limited` all of the app's have one, and 95.5 % of the worker's (the rest
  are C and Zig code linked in, whose debug info is their own).
- **The dSYM grows by two fifths**, 20 MB compressed for the app and 9 MB for the worker.
- **Build time barely moves.** The clean builds' CPU time is within the noise of a loaded
  machine (the app's came out lower with `limited`). A rebuild after one file, which is fat
  LTO's link of the whole binary, costs 3 % more CPU in the app and 7 % in the worker.
- Decided: `debug = "limited"` for `dist` (`docs/decisions/crashes.md`, "Shipped builds have
  limited debug info").

## 2026-10-01 — dSYMs apart from the bundle

Mac Studio M1 Max, macOS 27.0, rustc 1.98.1, the `dist` profile with `debug = "limited"`. One
`cargo xtask bundle`, whose five binaries are signed into `Slopty.app` and whose dSYMs land beside
it in `dSYMs/<UUID>/<bin>.dSYM`. The "in the bundle" column puts the same five dSYMs into
`Contents/MacOS`, as the bundle shipped them before.

```sh
cargo xtask bundle --out target/bundle-lean
ditto -c -k --keepParent Slopty.app app.zip          # the download
tar -C dSYMs -czf dsyms.tar.gz .                      # the release artifact
du -skL Slopty.app dSYMs
```

| | lean bundle | dSYMs, their own artifact | bundle with the dSYMs in it |
|---|---|---|---|
| on disk | 83 MiB | 595 MiB | 678 MiB |
| compressed (zip, or tar + gzip for the dSYMs) | 36.6 MB | 141.0 MB | 177.7 MB |

Per binary, gzip of the binary against tar + gzip of its dSYM: `slopty-app` 17.3 MB and 62.4 MB,
`slopty-worker` 6.4 and 29.0, `slopty-server` 4.9 and 22.4, `slopty` 4.9 and 22.0, `slopty-ptyd`
1.2 and 5.4.

A field crash, end to end. The lean bundle's app and worker are crashed in place, with no dSYM
anywhere near them (`SLOPTY_CRASH_TEST=panic` for the app, `segv` for the worker, then one more
worker run to resolve its record), and the reports are resolved against the artifact alone:

```sh
M=target/bundle-lean/Slopty.app/Contents/MacOS; export SLOPTY_DATA_DIR=/tmp/fdata2
SLOPTY_CRASH_TEST=panic $M/slopty-app; SLOPTY_CRASH_TEST=segv $M/slopty-worker
$M/slopty-worker --port 0 --ctl-socket /tmp/fdata2/w.sock --ptyd-socket /tmp/fdata2/p.sock
cargo xtask symbolicate /tmp/fdata2/crashes/<report>.json --dsyms dsyms.tar.gz
```

| frame of the worker's `SIGSEGV` | in the field | after `symbolicate` |
|---|---|---|
| 0 | `slopty_crash::probe::segv` | `slopty_crash::probe::segv` (probe.rs:96) |
| 1 | `slopty_crash::probe::fire` | `slopty_crash::probe::fire` (probe.rs:77) |
| 2 (inlined) | — | `slopty_crash::install` (lib.rs:135) |
| 3 | `slopty_worker::main` | `slopty_worker::main` (main.rs:384) |
| 4 (inlined) | — | `<fn() -> … as core::ops::function::FnOnce<()>>::call_once` (function.rs:250) |
| 5 | `std::sys::backtrace::__rust_begin_short_backtrace::<…>` | the same (backtrace.rs:166) |

The app's panic resolves the same way, `slopty_crash::probe::panic` to probe.rs:85 included (the
`.llvm.` gap of a debug map does not reach a dSYM). Each report kept its image, offsets and the
build's UUID; `symbolicate` took 1.8–2.0 s including unpacking the 141 MB archive.

What it says:

- **In the bundle the dSYMs were 80 % of the download**: 177.7 MB against 36.6 MB lean, 4.9 times
  the size, for every user, to serve the few reports that come back.
- **Apart, nothing is lost.** The field report names every outermost frame from the symbol table,
  and the dSYM of its UUID brings back the files, the lines and the inlined frames exactly as a
  dSYM beside the binary did.
- Decided: the dSYMs ship apart (`docs/decisions/crashes.md`, "The dSYMs ship apart from the
  app").

## 2026-10-01 — the tests lane on a hosted runner, minute by minute

CI run 36778678106 on 369d9e83, `gate (tests)` on `macos-26` (3 cores, 7 GB): 36:40 from
21:21:18 to 21:57:58. The phases come from the job log's timestamps and each `quiet_step`'s own
time, the compile cache's share from `sccache --show-stats` at the end of the job, and the tests
from the `junit.xml` artifact.

```sh
gh api repos/{owner}/{repo}/actions/jobs/110103610895/logs    # phases, sccache stats
gh run download 36778678106 -n junit                           # per-test times
```

| phase | time |
|---|---|
| setup: checkout, toolchain, rust-cache restore, `brew install zig` (52 s), `cargo xtask setup` (69 s, most of it compiling xtask) | 2:56 |
| `nextest build` | 26:16 |
| `spawned binaries` | 3:11 |
| `nextest` (3 098 tests, 215 s) beside the doctests (42 s) | 3:44 |
| artifact upload, sccache stats, cache save | 0:29 |

The build, as the compile cache saw it:

| sccache | count |
|---|---|
| compile requests | 958 |
| hits (Rust) | 641, 0.18 s each to read |
| misses (Rust) | 19, 53.1 s each to compile |
| not cacheable: `crate-type` (test harnesses, binaries, proc macros, build scripts) | 266 |
| not cacheable: other (`-` probes, missing input) | 20 |

What it says:

- **Not cache misses.** 97 % of what sccache can cache it served; every dependency was a hit.
  The 19 misses are the workspace libraries downstream of the commit's changes, 17 core-minutes
  that any build of that commit pays.
- **The build is the units sccache cannot cache.** A runner has 79 core-minutes in 26:16; after
  the misses and the hits, some 60 are left, and they go to what sccache never stores: 125 test
  harnesses (every library's unit tests and each `tests/*.rs`, each compiled and linked into its
  own binary), 13 binaries, 42 proc macros and 67 build scripts, one of which builds libghostty
  with zig. Built the same way here (`CARGO_INCREMENTAL=0 cargo test --workspace --no-run
  --timings`), those take 2 144 s (test harnesses), 416 (proc macros), 286 (binaries) and 300
  (build scripts, compile and run) of unit time: the test harnesses are two thirds of it. This
  Mac was under heavy load from other sessions, so these are proportions, not times. The next
  run's `timings` artifact (`cargo-timing.html` from the gate's `--timings`) gives the
  runner's own per-unit times.
- **The spawned binaries built a dozen workspace crates twice.** `cargo build --bins -p …`
  resolves features without the dev-dependencies the test build saw (`slopty-tailnet/fake`,
  `slopty-codec/experiments`, `slopty-client/headless`), so `slopty-tailnet` and everything above
  it compiled a second time, differently: 191 s here after `workspace-hack` joined the list, 354 s
  before. `cargo build --workspace --tests --bin …` resolves them as the test build did. Here,
  after `cargo nextest run --workspace --no-run --cargo-message-format json`, it takes 0.8 s,
  and its `--message-format json` lists 826 artifacts, all fresh: 825 the test build's own
  (same package, target, features and files, the four daemons among them), and the stand-in
  `claude`, which no test build links.
- **The run is the tests.** Their times add up to 581 s over three threads, 194 s of the 215;
  the longest is the icon test (68 s, started first by its priority).

Three follow-ups, measured on this Mac (M1 Max, load average 40–90 from other sessions, so CPU
time, user + sys of cargo and everything under it, is the comparable number):

- **`slopty-e2e`'s live targets leave the test build.** Every test in its ten integration
  targets (`app`, `ios`, `ios_uikit`, `linux`, `pair`, `server`, `smooth`, `through_server`,
  `vm`, `workers`) is `#[ignore]`d, and CI ran none of them; they now need the crate's `live`
  feature, which `cargo xtask e2e`, `vm e2e`, `linux e2e` and host clippy pass. A rebuild of
  `slopty-e2e`'s tests after its library changed, as every commit below it causes, five rounds
  each:

  | `cargo nextest run -p slopty-e2e --no-run` | units | built | CPU | wall |
  |---|---|---|---|---|
  | with `--features slopty-e2e/live` (as before) | 250 | 16 | 96.5–97.1 + 4.6–4.7 s | 20.5–43.1 s |
  | without (the gate now) | 238 | 4 | 8.5–8.6 + 1.0 s | 6.6–13.4 s |

  Twelve units fewer (the ten test binaries and the two helper binaries only they needed) and
  about 92 s of CPU per build: some half a minute of the runner's three cores.
  `cargo nextest list -p slopty-e2e` now lists the library and the two helpers' harnesses, and
  with `live` all thirteen; `cargo xtask e2e app -E 'test(the_settings_form_edits_the_file)'`
  built the `app` target with it and passed (3.96 s); clippy with `--all-targets --features
  slopty-e2e/live` checks all thirteen targets and reports nothing in `slopty-e2e`.

- **One integration binary per crate would save little.** Touching every `tests/*.rs` of a crate
  and rebuilding its test binaries, then touching only the smallest (two rounds each; its cost
  is what one more binary adds: compile, the dependencies' generics again, and the link):

  | crate | binaries | all of them, CPU | the smallest alone, CPU |
  |---|---|---|---|
  | `slopty-net` | 8 | 48.8–71.3 s | 4.4–4.5 s (`wrong_build`) |
  | `slopty-worker` | 7 | 32.3–32.9 s | 1.5 s (`load`) |
  | `slopty-proto` | 5 | 29.6–31.9 s | 5.8–6.0 s (`codec_props`) |
  | `slopty-client` | 5 | 21.7–21.8 s | 3.6 s (`wrong_build`) |
  | `slopty-cli` | 5 | 30.4–30.8 s | 1.4–1.5 s (`crash`) |

  Merging a crate's N binaries into one saves about N − 1 of those: 1.5–6 s each. The workspace
  has 74 integration binaries in 23 crates outside `slopty-e2e`, so 51 fewer links and 1.5–5
  CPU-minutes, a minute or so of a runner's build. The larger share of the test harnesses is
  each library compiled a second time for its unit tests, which merging does not touch.

- **The Actions cache.** `gh api repos/{owner}/{repo}/actions/caches` on 2026-10-01: 10.54 GB in
  4 379 entries, over the 10 GB quota. sccache 2.88 GB in 4 351 objects; rust-cache 7.66 GB in 28
  entries, 270 MB each (registry, git checkouts and `~/.cargo/bin`), one per lane and lockfile:
  a lockfile change saved five more. A restore took 22–23 s of a lane; a cold `cargo fetch` of
  the host's packages took 38 s here. One shared entry saved by the tools lane on main keeps the
  restore at about 270 MB for every lockfile, leaving the quota to sccache.

## 2026-10-01 — the fill's dials after the port fix, and a reset by peer

Mac Studio M1 Max, macOS 27.0, other sessions building beside it (load average 26 to 38). It
reruns the fill that found the dial with no answer ("A finding the fill brought out", fixed in
"A dual-stack port no IPv4 socket holds") and records a second failure it turned up
(decisions/transport.md, "A resent Initial is not a stateless reset").

The soak, terminals only (`--stream-lanes 0`, so nothing captures the screen), counts the CLI
calls that could not reach the server at the first try. A cycle makes seven calls, so a run is
about 11 800 calls, where the fill that found the bug made 11 870 and saw 1 to 2:

| soak | cycles (warm-up + fill + load) | calls that got no answer |
| --- | --- | --- |
| 1 | 2 + 1 536 + 152 | **0** |
| 2 | 2 + 1 536 + 149 | **0** |
| 3 | 2 + 1 536 + 123 | **0** |
| 4, with noq-proto patch 17 | 2 + 1 536 + 146 | **0** |

Soak 3 failed its own check on the worker's thread count (18 before the load, 19 after),
which is not the transport's. None of the four leaked, and each server's slope was 0 KiB/min.

The in-process rig (`fresh_endpoints_dial_a_loopback_server`, 100 000 fresh endpoints dialing
a server that pushes to every link every 2 ms) got no answer 0 times. It failed one dial in
100 000 another way, with `stream: reset by peer` at the client's first stream, while two
soaks ran beside it. Three rigs at once with 32 dialers each load the machine enough to show
it. "Before" ran with a tracing probe that kept each thread's last 40 noq events and printed
them at every stateless reset (a one-off, not kept, which slowed the dials); "after" ran once
with the same probe and once without:

| noq-proto | dials, 3 × 200 000 at once | reset by peer | no answer | run |
| --- | --- | --- | --- | --- |
| before patch 17, probe | 600 000 | **7** (2, 2, 3) | 0 | 522 s |
| after, probe | 600 000 | **0** | 0 | 465 s |
| after, no probe | 600 000 | **0** | 0 | 424 s |

The server sent no stateless reset to any of the seven client ports. Each failing client had
read the server's first flight (a 182-byte Initial, its Handshake packet and a 973-byte 1-RTT
packet), then 10 µs later a 173-byte Initial that the server had resent unpadded, and took it
for a reset. The stateless resets the probe saw otherwise, 41 to 57 a run before and after
alike, all went to links that had closed.

On the simulated network (`crates/slopty-net/tests/sim.rs`), a dial over a path that delivers
every datagram twice failed with the same `stream: reset by peer` for 25 of seeds 1 to 300
before the patch (seed 7 the first) and for 0 of seeds 1 to 1 000 after it. That is one seed in
12, the share of shuffles that write the reset token last among the server's transport
parameters. Each doubled dial took 10 ms of simulated time.

```sh
cargo xtask soak --stream-lanes 0 --out target/deep/soak/<name>   # summary.json: calls_unreached
for i in 1 2 3; do ROUNDS=200000 TASKS=32 cargo test -p slopty-net --release --test dual_stack_port -- --ignored --nocapture & done; wait
cargo test -p slopty-net --test sim a_path_that_delivers_every_datagram_twice_still_connects
cargo test --manifest-path vendor/noq-proto/Cargo.toml --target-dir target/noq-proto --lib packet_crypto
```

## 2026-10-01 — file tile pictures and PDFs: decode and draw costs

Mac Studio (M1 Max, 10 cores), macOS 26, the test profile (optimised, with debug info). Other
sessions were building, with a load average of 28 to 33; an earlier run at 40 to 45 took up to
twice as long, so the figures are loaded, not idle. Each is the median of five. "Decoded to" is
the size ImageIO is asked for (`CGImageSourceCreateThumbnailAtIndex`), the pixels a tile covers;
the last row of each picture decodes every pixel. The cost includes the draw into BGRA and, for
a picture with alpha, the unpremultiply pass. The pictures are made by the test: a noisy
gradient saved as a JPEG at quality 90, the same converted to HEIC by `sips`, and a two-colour
PNG the size of a Retina screenshot.

| picture | bytes | decoded to | ms |
| --- | --- | --- | --- |
| JPEG 4032 × 3024 | 2 797 880 | 400 × 300 | 24.5 |
| | | 1 600 × 1 200 | 39.8 |
| | | 4 032 × 3 024 | 52.9 |
| HEIC 4032 × 3024 | 2 166 644 | 400 × 300 | 135.4 |
| | | 1 600 × 1 200 | 100.0 |
| | | 4 032 × 3 024 | 182.2 |
| PNG 2880 × 1800 | 104 973 | 400 × 250 | 28.1 |
| | | 1 600 × 1 000 | 45.5 |
| | | 2 880 × 1 800 | 56.3 |

A tile of 800 points on a 2× screen decodes a 12 MP JPEG at 1 600 pixels for about three
quarters of the full cost, and holds 7.7 MB of pixels instead of 48.8 MB. HEIC was slower at 400
than at 1 600 in every run; "HEIC thumbnails in two steps" below explains it and the fix. A 12 MP picture drawn whole spent about 20 ms
painting and 12 ms unpremultiplying in a probe before an opaque picture skipped the second
pass. Every decode runs off the UI thread.

| PDF, 20 pages of text, US letter, 81 283 bytes | ms |
| --- | --- |
| open (`CGPDFDocument`) | 0.02 |
| one page drawn 800 pixels wide | 0.4 |
| 1 600 pixels (an 800-point tile on 2×) | 0.7 |
| 3 200 pixels | 3.0 |

```sh
cargo nextest run -p slopty-ui decode_and_draw_costs --run-ignored only --no-capture
```

## 2026-10-01 — folder tiles on kernel events

Same machine, release build, load average about 20. A round changes a folder's entries five
ways, at least 100 ms apart (the follower's `HOLD`). Each row is 200 samples, in µs, from the
change to the path coming out of `Changes::next` (`fswatch::follow_folders`, kqueue).

| change | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- |
| a file made | 3 835 | 9 489 | 23 032 | 35 493 |
| a file renamed | 3 856 | 9 487 | 17 936 | 42 408 |
| a file deleted | 3 880 | 11 129 | 23 385 | 98 589 |
| a folder made | 3 805 | 8 945 | 30 193 | 45 011 |
| a folder deleted | 3 822 | 9 689 | 17 755 | 47 106 |

Through the worker over a loopback link (a file made → the client's `WorkerMsg::Folder`),
20 rounds: p50 4.24 ms, p90 4.39 ms, max 13.95 ms. In the app (`cargo xtask e2e app`, the
folder scenario), a file written → its row in the tile's dump: 15.8 to 19.7 ms in five of seven
runs and 125 to 131 ms in two, the dump being polled. Before, a folder tile listed again only
when it took the focus, however long that was.

```sh
cargo nextest run -p slopty-worker --release --test fswatch folder_change_to_report --run-ignored only --no-capture
cargo nextest run -p slopty-worker --test e2e a_followed_folder_is_listed_again --no-capture
cargo xtask e2e app --filter 'test(/a_folder_tile_browses/)'
```

## 2026-10-01 — quick open's worktree index

Same machine, release build, load average 30 to 42 from other sessions. A worktree is indexed
on its first query (`find::Index`), then each keystroke is a match over memory. "First
keystroke" is 10 rounds of six queries that do not follow each other (`view`, `item 4`,
`mod12 item`, `srcmodvie`, `main`, `rs`), each scoring the whole tree; "typed on" is
`srcmodvi` typed a letter at a time, 10 times, each query after the first scoring only the
last one's matches. "Before" is the walk each keystroke made until now, bounded at 20 000
entries and 8 levels, with the new ranking. "A file made" is a file written into the worktree
→ the first query that returns it, polled every 5 ms, through the `FSEvents` stream.

| worktree | paths | first walk | first keystroke p50 / p99 | typed on p50 / p99 | before p50 / p99 | a file made p50 / p99 |
| --- | --- | --- | --- | --- | --- | --- |
| this repository | 7 910 | 41 ms | 0.67 / 5.19 ms | 2.98 / 4.97 ms | 40.6 / 43.3 ms | 14.0 / 19.1 ms |
| synthetic, 4 000 folders × 50 files | 204 160 | 166 ms | 4.15 / 15.3 ms | 8.88 / 15.0 ms | 39.0 / 42.3 ms | 25.5 / 27.2 ms |

The synthetic tree is the worst case for narrowing: every path holds `src`, `module` and
`view`, so `srcmodvi` typed on keeps all 204 160 as candidates and only adds the cost of
keeping them. In the repository, typed on is slower than a first keystroke because its short
prefixes (`sr`, `src`) match nearly everything, where the first-keystroke set includes words
that match little. Before, the walk also stopped at 20 000 entries, so in the synthetic tree
it could never find 90 % of the files. A first run, before the tree's own creation had drained
from fseventsd, saw keystrokes at p99 363 ms while the index relisted the directories those
late events named; the measurement now waits 10 s after making the tree. That first run
also scored on one thread, at 40.7 ms p50 for a keystroke; the two changes (the wait and
scoring across the cores) were not measured apart.

```sh
cargo nextest run -p slopty-worker --release quick_open_costs --run-ignored only --no-capture
SLOPTY_FIND_TREE=$PWD cargo nextest run -p slopty-worker --release quick_open_costs --run-ignored only --no-capture
```

## 2026-10-01 — quick open's index on Linux, and the watch beside the walk

The synthetic tree of the section above (4 000 folders of 50 files, 204 160 paths), the release
build. Linux is aarch64 Debian trixie in Docker Desktop's VM on the same Mac (kernel 7.0.14,
`max_user_watches` 1 048 576), the binaries cross-built with `cargo zigbuild`. Load average 20
to 58 from other sessions throughout, so the p99s are the load's. "A file made" is a file
written under a probe folder → the first query under that folder that returns it, polled every
millisecond, so it is the event and the catch-up, not the scoring. "Watch up" is when the watch
started beside the first walk was up.

| system | first walk, no events / with | watch up | watches | first keystroke p50 / p99 | typed on p50 / p99 | before p50 | a file made p50 / p99 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Linux, inotify | 129 / 162 ms | 162 ms | 4 161 | 11.6 / 51.7 ms | 18.5 / 63.2 ms | 29.3 ms | 2.04 / 2.25 ms |
| macOS, `FSEvents` | 271 / 222 ms | 222 ms | 0 | 9.10 / 28.5 ms | 14.5 / 28.6 ms | 99.1 ms | 18.3 / 66.5 ms |
| macOS, this repository (7 943 paths) | 43 / 21 ms | 21 ms | 0 | 0.48 / 4.92 ms | 2.98 / 4.64 ms | 42.4 ms | 12.3 / 13.4 ms |

- **inotify's cost.** 4 161 watches for 4 161 directories, 33 ms more on the first walk for
  adding them. The kernel's slab could not be read in the container (`/proc/slabinfo` is
  root's on the host), so the memory is from the kernel's own sizing of the limit, about 1 KiB
  a watch on 64-bit: some 4 MiB for this tree, and 0.4 % of this machine's limit.
- **A file made, before and after the merge.** Catching up one file pushed it on the sorted
  paths and sorted all 204 160 again. On Linux that took a file made to visible at p50
  17.5 ms (p99 47.7). Merging the new paths in takes it to 2.36 ms (p99 2.59) in the same
  container, and 2.04 ms with the watch started beside the walk. On macOS the same row is
  `FSEvents`' delivery, about 11 ms, plus the catch-up; it was 25.5 ms with the sort.
- **The watch's start.** One debug run under a load of about 50, on a tree made 10 s before,
  spent 882 ms in `FSEventStreamStart` before the walk began, which the first answer waited
  for (1 882 ms against 400 ms without events). In the runs above the stream was up within
  the walk. Either way the first answer now waits for the walk alone.
- **Out of watches.** `past_the_kernels_watches_the_index_looks_at_times_and_stays_right`
  runs with the limit lowered to 20 in a user namespace of its own: `ENOSPC` at the 21st
  directory, the watch let go, and a file made then is found by the times.

```sh
cargo nextest run -p slopty-worker --release quick_open_costs --run-ignored only --no-capture
CARGO_FEATURE_NO_NEON=1 cargo zigbuild --target aarch64-unknown-linux-gnu --target-dir target/linux \
  -p slopty-worker --lib --tests --release
docker --context desktop-linux run --rm --label dev.slopty.linux \
  -v $PWD/target/linux/aarch64-unknown-linux-gnu/release/deps:/t:ro debian:trixie-slim \
  sh -c '/t/slopty_worker-* --ignored --exact find::index::tests::quick_open_costs --nocapture'
docker --context desktop-linux run --rm --privileged --label dev.slopty.linux \
  -v $PWD/target/linux/aarch64-unknown-linux-gnu/release/deps:/t:ro debian:trixie-slim \
  unshare -Ur sh -c 'echo 20 > /proc/sys/user/max_inotify_watches && /t/slopty_worker-* --ignored past_the_kernels'
```

## 2026-10-01 — HEIC thumbnails in two steps

Same machine and pictures as "file tile pictures and PDFs" above, load average 17 to 44. CPU is
`clock(3)` across the call, which another session's load does not inflate the way it does
wall time. Median of five, two runs.

| HEIC 4032 × 3024 to | one step, wall / CPU ms | two steps, wall / CPU ms |
| --- | --- | --- |
| 100 × 75 | 257.9 / 94.2, 128.2 / 68.4 | 131.4 / 66.0, 87.8 / 46.5 |
| 400 × 300 | 128.2 / 71.7, 107.6 / 53.5 | 102.3 / 61.7, 75.1 / 44.3 |
| 1 600 × 1 201 | 117.9 / 69.5, 81.6 / 56.7 | (one step) |

ImageIO decodes a HEIC whole and then scales the image to the thumbnail; a JPEG it decodes
at a reduced scale instead, which is why the JPEG rows fall with the size. The scaling's cost
grows as the ratio does: in a probe, thumbnails straight from the source took 65 to 73 ms CPU
at 100 pixels, 56 to 66 at 400 and 47 to 55 at 1 600. So a HEIC asked for at less than two
fifths of its longest side is decoded at two fifths (1 612 pixels here) and drawn down to the
size by Core Graphics, the draw the decode makes anyway. That takes 14 to 32 % less CPU at
400 and 100 pixels. A JPEG, PNG or any other type keeps its one step.

```sh
cargo nextest run -p slopty-ui decode_and_draw_costs --run-ignored only --no-capture
```

## 2026-10-01 — a PDF's page flip and drag

Same machine, the test profile, load average 20 to 48. A file tile 1 920 × 1 080 in GPUI's test
window, showing a PDF of 200 US-letter pages of text. A flip is → from the key to the frame laid
out, every page in view drawn (the window is headless, so no GPU work is in it). A drag step
is the pointer moved with the button down → the selection asked of `PDFKit` again and its
frame. 60 of each, three runs.

| | p50 | p90 |
| --- | --- | --- |
| page flip | 3.03 to 3.63 ms | 9.27 to 9.95 ms |
| drag step | 0.01 to 0.02 ms | 0.04 to 0.05 ms |

A flip lands a page that is not drawn yet, so its p90 carries drawing one page 1 902 pixels
wide, which "file tile pictures and PDFs" puts at about 1 ms, and the next page the list's
overdraw reaches. `PDFKit` answers a selection between two points of one line in tens of
microseconds, so a drag can ask it on every move.

Paging by key and the text selection were cut on 2026-10-05 (prune #12), and the timing test
with them; this stays as the record. It was run with:

```sh
cargo nextest run -p slopty-ui --run-ignored only timing_of_a_page_flip --no-capture
```

## 2026-10-01 — the worker's blocking pool after a soak

Mac Studio M1 Max, macOS 27.0, other sessions building beside it. It explains a soak failing
with `worker: 19 threads after the load, 18 before` (decisions/testing.md, "A soak's extra
worker thread was the blocking pool, kept warm by an idle tick"). Each run is `cargo xtask
soak --stream-lanes 0`. Beside it a one-off probe (Python over libproc, not kept) read the
worker's threads by name four times a second, and `sample <pid> 1` took their stacks at the
fill, the baseline and the end.

Before the fix the worker's threads were, by name and stack: 10 tokio workers, the main thread,
`slopty-worker` (the daemon thread), 1 to 3 unnamed system dispatch threads, a
`session-<uuid>` thread per live session, and 3 to 5 idle threads of tokio's blocking pool
(also named `tokio-rt-worker`, parked in the pool's `wait_timeout`). The pool grew during the
fill, 3 → 4 at 46 s and → 5 at 68 s, and never shrank, through 60 s of load, the 12 s settle
and `leaks`. In an idle worker an lldb breakpoint on `spawn_blocking` caught 32 calls in 8 s,
every one from `agents::keep_awake` on the 750 ms agents tick, with no agent working.

| soaks | worker threads baseline → end | failed the thread check |
| --- | --- | --- |
| before the fix, 11 | 17 to 19 → 15 to 19 | 2 (18 → 19 both times) |
| after the fix, 2 | 14 → 14, 13 → 13 | 0 |

After the fix the pool empties about 10 s after its last task (tokio's keep-alive): the
`tokio-rt-worker` count fell to 10 at 12 s (after the warm-up), at 209 s and 182 s (after the
fill) and at the end, so the baseline and the end both hold 10 tokio workers, the main thread,
the daemon thread and 1 or 2 dispatch threads. Three of the pre-fix soaks were also the ones
with lldb attached, and in those the server held one thread more after the load (11 → 12).
The server is not changed here.

```sh
cargo test -p slopty-workerd --bin slopty-worker only_a_paused_turn_takes_the_blocking_pool
cargo test -p xtask --bin xtask soak::
cargo xtask soak --stream-lanes 0   # summary.json: daemons.<name>.thread_names
```

## 2026-10-01 — a nightly that shares the Mac

Mac Studio M1 Max, macOS 27.0. Other sessions were building, and the load average stood at
about 45. One cold build of `slopty-proto` and its dependencies into a fresh target dir, at the
priority each mode gives a check (`xtask/src/nightly.rs`, `Share`):

| mode | wall | CPU (user + sys) | cores used |
| --- | --- | --- | --- |
| a runner's: `nice -n 10`, every core | 67.3 s | 130.2 + 6.9 s | 2.0 |
| this Mac's: `nice -n 19`, `CARGO_BUILD_JOBS=4` | 92.6 s | 134.2 + 7.7 s | 1.5 |

The build does the same work either way. Here it takes 38 % longer and leaves a quarter of
the cores it would have used to whoever else is on the machine.

What the checks cost here, from the one local run (`target/nightly/2026-10-01/<check>.json`).
That run is what moved them to GitHub Actions (decisions/testing.md, "The deep checks run on
GitHub Actions"):

| check | wall | result |
| --- | --- | --- |
| `sanitize realtime` | 59 s | 59 passed |
| `sanitize address`, 10 crates (then `slopty-crash` apart, 19 s) | 619 s | 802 passed; 5 of 35 crash tests failed |
| `sanitize thread`, 12 crates (then `slopty-crash` apart, 21 s) | 870 s | 809 passed, 1 failed, 1 timed out; 5 of 35 crash tests failed |
| `miri`, before nextest and its timeout | stopped at 3 300 s | held 55 min by `slopty-grid`'s `tests/allocs.rs` |
| `miri -p slopty-core` through nextest | 4.4 s of tests | 10 passed |

```sh
/usr/bin/time -l nice -n 10 cargo build -p slopty-proto --target-dir target/nice-a
CARGO_BUILD_JOBS=4 /usr/bin/time -l nice -n 19 cargo build -p slopty-proto --target-dir target/nice-b
cargo xtask nightly run --only sanitize-address    # one check at a time, here
cargo xtask deep miri -p slopty-core
```

## 2026-10-01 — the terminal fuzz target

Mac Studio M1 Max, macOS 27.0, nightly Rust and libFuzzer through cargo-fuzz, AddressSanitizer,
with libghostty built `ReleaseSafe`. The corpus started from the nine captures in
`fuzz/seeds/terminal`, with `fuzz/dicts/terminal.dict`.

| run | executions/s | coverage | features | corpus | found |
| --- | --- | --- | --- | --- | --- |
| 300 s, one job | 19 | 4 750 | 21 126 | 831 | 2 checkpoint replay bugs, no trap, no panic, no viewer divergence |
| 300 s, two jobs, every line held to the oracle (marks, links, wraps) | 37 | 5 276 | 27 713 | 1 186 | 6 (50 before that round's fixes) |
| 300 s, two jobs, after aislopware/ghostty#3's first fixes | 28 | 5 407 | 29 097 | 1 348 | 6: coloured blanks, an empty flagged row, a link on an empty cell, the alternate screen's region, a stale prompt flag |
| 300 s, two jobs, after all of them | 15 | 5 489 | 30 175 | 1 522 | nothing |

The corpus carries over from run to run, so later runs start deeper. Every artifact the runs
found now passes, and one of each kind is kept under `fuzz/regressions/terminal`, which
`cargo xtask fuzz --replay` replays (decisions/testing.md, "What it found next").

Resident memory per execution, on the same inputs replayed:

| build | resident growth |
| --- | --- |
| AddressSanitizer (as fuzzed) | 0.40 to 0.55 MB per execution, until libFuzzer's 2 GiB limit stopped it |
| `ReleaseFast` / `ReleaseSafe`, no sanitizer | flat, 8 to 12 MB |
| AddressSanitizer with `detect_leaks=1` | no leak reported |

The growth is AddressSanitizer's (its quarantine and shadow memory), not the engine's, so the
terminal target runs with an 8 GiB RSS limit and every target with `-malloc_limit_mb=2048`.

```sh
cargo xtask fuzz terminal --time 300
cargo xtask fuzz --replay           # the kept regressions, on a plain build
```

## 2026-10-01 — terminal checkpoints held to every line, and what a frame pays for it

Mac Studio M1 Max, release build, retired instructions per op, one run each (other lanes
building, so wall times are only indicative). Before: the tree at `640f9477` with ghostty
`89c9624f0` and libghostty-rs `2f8b499`, built from `git archive` in `target/bench-before`.
After: this tree with ghostty `874ada99a` (aislopware/ghostty#1 to #4) and libghostty-rs
`24b9ceff` (aislopware/libghostty-rs#1 to #3).

```sh
SLOPTY_BENCH_OUT=$PWD/target/bench/terminal-after.jsonl nice -n 10 cargo nextest run --release \
  -p slopty-engine --run-ignored only -E 'test(/_cost$/)' --no-capture --no-fail-fast
```

| series | before | after | change | p50 / p95 before | p50 / p95 after |
| --- | --- | --- | --- | --- | --- |
| `frame_cost.take_frame.60x12` (a typed byte's frame) | 18 922 | 19 011 | +0.5 % | 1.13 / 1.38 µs | 1.13 / 1.33 µs |
| `frame_cost.take_frame.200x60` | 45 009 | 44 911 | −0.2 % | 2.42 / 2.63 µs | 2.46 / 2.71 µs |
| `frame_cost.take_frame_unchanged` (both sizes) | 901 / 997 | 901 / 997 | 0 | | |
| `scroll_frame_cost.80x24` (an Enter at a bottom prompt) | 123 543 | 123 508 | 0 | 7.79 / 9.17 µs | 7.67 / 8.54 µs |
| `scroll_frame_cost.200x60` | 361 877 | 362 843 | +0.3 % | 25.0 / 26.5 µs | 25.3 / 28.8 µs |
| `fetch_lines_cost.4096_rows` | 103 166 442 | 104 251 230 | +1.1 % | 7.72 / 9.69 ms | 7.72 / 9.22 ms |
| `checkpoint_cost.format` (10 024 coloured lines) | 45 654 778 | 50 644 004 | +10.9 % | 2.58 ms | 2.67 ms |
| `checkpoint_cost.replay_one_chunk` | 33 245 557 | 34 380 073 | +3.4 % | 2.26 ms | 2.38 ms |
| `checkpoint_cost.replay_64k_chunks` | 33 146 737 | 33 076 974 | −0.2 % | | |

`frame_cost.write` moved between 765 and 865 over runs of the same build, so its +2.5 % is
noise. Every other `_cost` series moved by less than 1 %, except the search series, which
moved by up to 2.3 % in a run whose untouched series (`png_decode_cost`) moved by 1.2 % too.

**What the frame path paid on the way, and how it went.** The first working version of these
fixes cost a typed byte's frame +13.8 % (60×12) and +17.9 % (200×60), and a scroll frame
+14 to +16 %. Taken apart one change at a time:

- A test of every cell for one holding only a background colour, about 7 instructions per cell
  even with a row-level guard in front of it (it changed how the whole cell loop compiled).
  libghostty had no row flag for such a cell, so the fork now has one (`Row.background`,
  aislopware/ghostty#4), and the engine reads the colours after the row's loop, on a flagged
  row only. Reading the flag before the loop cost `fetch_lines_cost` +22 % the same way;
  read after the loop it costs nothing measurable.
- `row_above_wraps` was placed between `next_row`'s `#[inline(never)]` and the function, which
  moved the attribute onto it and let `next_row` inline into the frame loop.
- A walk over every row before the frame to find a row whose wrap changed above a clean row,
  with a grid lookup for each, and a grid lookup for the row above each changed row. A frame
  now notes a changed wrap while it visits the changed row and sends the clean row below
  again with only its flag changed. A changed row whose row above is clean takes its wrap from
  its own line as last sent.

`fetch_lines_cost` keeps +1.1 %: one lookup of the row above per line served, for `WRAPPED`.
Passing the row read before instead measured +1.7 %, so the lookup stays.

**What a checkpoint pays.** Formatting it costs +10.9 %. It now writes every row's prompt
flag and every cell's content (OSC 133), hyperlinks, and the steps that set prompt flags right
across a wrap. A replay costs +3.4 % in one chunk and nothing in 64 KiB chunks. A shell
session's history (the probe below, 10 000 lines with OSC 133 marks and OSC 8 links) replays
in 792.7 M instructions, against 52.8 M when the checkpoint dropped the marks and the links:
about what the original output cost (799.4 M). libghostty takes about 25 000 instructions per
OSC 8 link, the bulk of it.

**The binary snapshot, measured again for size.** libghostty's own snapshot (`GHOSTSNP`, decided
in decisions/terminal.md, "#90") keeps both screens, but a coloured cell takes 8 bytes in it:

| history | snapshot | encode | compressed |
| --- | --- | --- | --- |
| 10 000 lines, coloured | 2.07 MB | 0.55 ms (decode 2.68 ms) | |
| 10 000 lines, the shell probe | 1.41 MB | 0.82 ms (decode 2.27 ms) | |
| 50 000 lines at 80 columns, digits | 4.11 MB | 2.5 ms | zlib level 1: 0.14 MB in 1.9 ms |
| 50 000 lines at 200 columns, digits | 10.12 MB | | |
| 50 000 lines at 120 columns, coloured | 47.20 MB | 12.3 ms | zlib level 1: 10.96 MB in 184 ms; fdeflate: 27.6 MB |
| 50 000 lines at 200 columns, coloured | 79.82 MB | | |
| 50 000 lines, the shell probe | 4.94 MB | | |

A full coloured history is past `MAX_CHECKPOINT_BYTES` (12 MiB) raw, and compressing it costs
more than the VT replay, so checkpoints stay VT.

## 2026-10-01 — the soak's footprint slope against its window

Mac Studio M1 Max, macOS 27.0, other sessions building beside it. It explains slopes that short
soaks read as growth (ptyd up to 1 980 KiB/min, the server 204, the worker 930 once) and sets
how the soak judges them (decisions/testing.md, "The soak judges footprint growth by Theil–Sen,
over 900 s of load after its first 300"). Every run is `cargo xtask soak --no-build`, sampled
every 2 s. "At once" means two or three soaks ran side by side.

**No daemon grew underneath.** `leaks` found nothing of ours in any daemon, in any run. In
`long-1` (900 s of load) `heap` every 60 s held ptyd's live heap at about 630 blocks
(0.67–0.97 MB) and the worker's at about 51 000 (7.5–7.9 MB). Under `--stacks`,
`malloc_history -callTree` 60 s and 620 s into a load found:

- ptyd: 443 blocks both times, 1.03 → 1.17 MB, the difference a live session's `Ring::push`
  (bounded by its capacity);
- worker: 27.2 → 20.2 MB, all of it the ghostty pages of the terminals open at that moment.
  Past those, the largest growth was 8 KiB (`Activity::begin`, a held sleep assertion).

The server was not snapshotted under load, because a `malloc_history` of it stalled it into a
`no answer` dial. Its slope never passed 16 over any window of 600 s.

**Least squares against Theil–Sen, worst slope of any window** (KiB/min; `long-1`, `ten-1`,
`ten-2`, no stream lane):

| window | 40 s | 120 s | 200 s | 300 s | 400 s | 600 s |
| --- | --- | --- | --- | --- | --- | --- |
| least squares | 489 | 1 757 | 848 | 467 | 331 | 189 |
| Theil–Sen | 398 | 1 160 | 439 | 263 | 199 | 40 |

**Each soak's slope** over the last two thirds of its load (least squares / Theil–Sen):

| run | load | lanes | at once | server | ptyd | worker |
| --- | --- | --- | --- | --- | --- | --- |
| long-1 | 900 s | 0 | no | 11 / 3 | 6 / 0 | 6 / 3 |
| ten-1 | 600 s | 0 | 2 | 14 / 8 | 142 / 0 | −33 / 23 |
| ten-2 | 600 s | 0 | 2 | 14 / 0 | 88 / 0 | −316 / 0 |
| ts-2 | 900 s | 1 | 3 | 2 / 0 | 0 / 0 | 92 / 33 |
| ts-3 | 900 s | 1 | 3 | 2 / 0 | 0 / 0 | 100 / 25 |
| j-2 | 960 s | 0 | 3 | 1 / 0 | −110 / −103 | −302 / 4 |
| j-3 | 960 s | 1 | 3 | 0 / 0 | −105 / −75 | 116 / 108 |
| v-1 | 1 800 s | 1 | 3 | — / 0 | — / −13 | — / 19 |
| v-2 | 1 800 s | 1 | 3 | — / 0 | — / −5 | — / 15 |
| v-3 | 960 s | 0 | 3 | — / 4 | — / 58 | — / 0 |
| threads-1 | 960 s | 0 | no | — / 0 | — / −19 | — / 39 |

**With a stream lane the worker's floor climbs, then holds.** `footprint <pid>` every 120 s in
v-1 and v-2 put the worker's `Malloc Small` dirty pages at 15 and 10 MB as the fill began, 21
two minutes in, 24 to 25 by about 400 s into the load, and 24 to 26 MB from there to the end of
30 minutes. `IOSurface` ran from 0 to 35 MB with the streams open at each sample. Without a lane
(v-3) the worker held 18.6 → 18.8 MB for the whole load. The worst Theil–Sen slope of any window
starting 300 s or more into the load (900 s and 1 200 s windows fit only v-1 and v-2):

| window | 600 s | 900 s | 1 200 s |
| --- | --- | --- | --- |
| worker (stream lane) | 179 | 47 | 32 |
| ptyd | 72 | 39 | 17 |
| server | 10 | 4 | 3 |

So the slope leaves out the first 300 s of load and is judged once the rest spans 900 s, with
the 64 KiB/min budget unchanged, and the default load is 1 200 s.

**Threads at the end.** With a 12 s settle, two soaks of seven ended with one unnamed thread
more than the baseline. A C probe calling `dispatch_async` eight times and counting
`task_threads` every 0.5 s read 1 → 9 threads, then 2 from 5.5 s to 90 s: idle dispatch threads
end after about 5 s, and one stays. In threads-1, `sample` at the baseline and every two minutes
showed the only unnamed thread of a settled worker to be that one (`start_wqthread`), and with
a 20 s settle the run held 13 → 13. Two of three soaks run at once with the 20 s settle (v-2,
v-3) still ended one over.

**Soaks killed by another soak.** ts-1 and j-1 died in the fill with their CLI killed
(`signal: 9`) 20 s after a second soak started. That second soak had rewritten
`target/deep/soak/bin` in place. Staging by rename fixed it. Probes that pause a daemon
(`heap`, `malloc_history`) caused `no answer` dials and hook timeouts of their own in four
runs, and those are not counted above.

```sh
cargo test -p xtask --bin xtask soak::
cargo xtask soak                      # 1 200 s; summary.json: daemons.<name>.slope_*
cargo xtask soak --stacks --seconds 720   # then malloc_history <pid> -callTree -invert
footprint <worker pid>                # Malloc Small, IOSurface
```

## 2026-10-01 — frame costs on gpui-fast 4afc87b, and the navigator's rows on their own

gpui-fast moved from `ed16b231` to `4afc87b`: longbridge main `92a9b0f` (bounded scroll-layer
rebuilds, the zed `0bdc70c` sync) and zed `f8c2cc84`, which shares the Apple dispatcher and Metal
renderer with iOS. The grid's frame costs, rerun as in "the grid's rows under keys" (release,
mac-studio, one run at a load average of about 20), p50 / p95 in µs:

| case | bare, afresh | bare, keyed | sprites, afresh | sprites, keyed |
| --- | --- | --- | --- | --- |
| unchanged | 208 / 297 | 24 / 25 | 746 / 782 | 139 / 145 |
| cursor blinking | 210 / 1 367 | 27 / 28 | 749 / 811 | 150 / 159 |
| a key echoed at the prompt | 217 / 1 946 | 31 / 39 | 749 / 826 | 149 / 183 |
| a line of output a frame | 300 / 1 699 | 120 / 155 | 832 / 914 | 425 / 483 |
| `yes`, a screen of one line a frame | 178 / 637 | 158 / 192 | 174 / 208 | 172 / 204 |
| a screen of new text a frame | 2 196 / 4 117 | 2 271 / 56 095 | 2 718 / 4 550 | 2 845 / 4 859 |
| scrolling the scrollback a line a frame | 283 / 339 | 104 / 289 | 835 / 935 | 409 / 484 |

Every median is within the spread of the two runs on `132dbc1`, so the sync costs a frame nothing.
The long p95 tails in the bare columns came with the load: the run started a minute after a load
average of 128.

The navigator's rows are a view of their own (`Region::NavigatorRows`), so a wheel over the list
builds that view and not the panel with its filter field, whose input writes its own state each
time it is built. 120 notes scrolled 12 points a frame, 600 frames after 20: p50 0.341 ms, p95
0.419 ms, 600 views built (one a frame, the rows'). Scroll layers are compiled out on Apple, so
none composited.

```sh
cargo test -p slopty-ui --release --lib -- --ignored --nocapture terminal_frames_cost measure_the_navigator_scrolling
```

## 2026-10-01 — a keyframe charged to the rung

Mac Studio M1 Max, macOS 27.0. The drawn screen at a quarter of its 3024 × 1964 (756 × 492),
streamed to a client in the same process, each session's first frame held back 500 ms
(`SLOPTY_COLD_MS`), as a cold encoder or a busy machine holds it. `EncoderWatch` charged the
captures the mailbox replaced during that keyframe to the rung: one capture of a 60 fps window
taken, so the ceiling fell to what the window was fed. Five runs each way, interleaved, from
copies of the test binary:

| | frames coded, first second | second second | lowest ceiling | pictures decoded in 2 s |
| --- | --- | --- | --- | --- |
| charged | 4–10 | 2–10 | 2–10 fps | 5–20 |
| excused (`EncoderWatch::fed`) | 24–28 | 57–60 | 60 fps | 82–84 |

Without the injected delay both builds code 53–59 frames in each second. At the whole
3024 × 1964 the ceiling falls to about 52 either way, which is that encoder's own rate. Before
the change the clock test sometimes ran at a ceiling of 4 and took about 13 s; after it, 3.9 to
5.5 s, with one keyframe a session.

```sh
SLOPTY_COLD_MS=500 cargo test -p slopty-worker --lib -- --ignored --exact --nocapture \
  screen::synthetic::tests::measure_a_new_streams_first_seconds
```

## 2026-10-01 — an encoder session that malfunctions

Mac Studio M1 Max, macOS 27.0. Eight copies of the worker's test binary at background QoS
(`taskpolicy -b`, the efficiency cores), each running three or four of the drawn-screen stream
tests, the load average between 27 and 83 with other sessions building. Each run lasted under
four minutes.

- With a debug log on, one copy's clock test stream had a frame come back with
  `kVTVideoEncoderMalfunctionErr` (−12912), then every one of the next 264 with
  `kVTVideoEncoderNotAvailableNowErr` (−12915), while the other tests' sessions in that process
  went on coding. The client had no video for 10 s and asked for a refresh 24 times. The
  worker treated each failure as a dropped frame.
- A striped stream in a run without the log stopped the same way on its top stripe: over 50 s
  the client decoded lower stripes only, asked 100 times, and the worker counted no capture
  coded.
- The CI failure this was found for (run 36813467628) has that shape: 33 frames handed to the
  decoder, 32 pictures, one decode error, then 22 refreshes and no picture for 10 s.

With the codec naming the loss (`CodecError::EncoderLost`) and the geometry tick rebuilding, the
deterministic test (`a_session_taken_away_is_replaced_and_the_stream_goes_on`, the first session
refusing every frame from its twentieth) shows 20 pictures, one new build and 100 more. With its
replacement taking 200 ms to build, a bare "lost" flag built three sessions and the check that the
lost session is still in force built two.

What still stopped those copies after the fix was the machine itself: the worker captured two
frames in over 10 s, each keyframe spent 7.7 to 8.8 s inside VideoToolbox, and the client task
went 10 s without a report. The stream tests now wait through that (`next_or_stopped`) and fail
only when both sides ran.

```sh
cargo test -p slopty-worker --lib --no-run   # then, from a copy of the binary off this volume:
taskpolicy -b ./slopty_worker-<hash> --exact --nocapture \
  screen::synthetic::tests::two_stripes_meet_at_the_seam_row_for_row \
  screen::synthetic::tests::the_clock_probes_time_captures_as_the_shared_clock_does \
  screen::synthetic::tests::a_frame_that_does_not_decode_is_refreshed_and_the_stream_goes_on \
  screen::synthetic::tests::a_session_taken_away_is_replaced_and_the_stream_goes_on
```

## 2026-10-01 — one sound per worker on a client

Mac Studio M1 Max, macOS 27.0, release, under `nice`, load 13–14 with other sessions building.
The worker's sound is now one capture and one Opus encoder per client, whatever the number of
streams hearing through it (docs/decisions/audio.md, "One sound per worker on a client, not one
per stream").

```sh
cargo nextest run --release -p slopty-worker --lib --run-ignored only --no-capture \
  -E 'test(=screen::synthetic::tests::one_sound_costs_the_same_for_any_number_of_streams)'
cargo nextest run -p slopty-worker --lib --no-capture \
  -E 'test(=screen::synthetic::tests::a_display_and_two_windows_of_one_app_are_one_sound_on_the_wire)'
cargo nextest run --release -p slopty-client --features headless --lib --run-ignored only --no-capture \
  -E 'test(=screen::worker_tests::clock_path_cost)'
```

**What the sound costs the worker** (`one_sound_costs_the_same_for_any_number_of_streams`: one
`Sound` on the canvas, its 440 Hz tone captured in 10 ms chunks, Opus-encoded and sent into a
transport that only counts, with N streams listening, a display among them and windows of two
apps; the process's CPU over 4 s after a second to settle):

| streams | CPU cores | packets a second | captures |
| --- | --- | --- | --- |
| 1 | 0.0193 | 99.9 | 1 |
| 2 | 0.0197 | 100.0 | 1 |
| 4 | 0.0209 | 100.2 | 1 |
| 8 | 0.0241 | 100.2 | 1 |

Before, each stream had its own capture tap and its own encoder: about 0.012 of a core a stream
for the encode alone (118 µs a 10 ms packet, "streams from one worker: what they could share"),
0.095 at eight, and the client decoded and played each stream's copy (37 µs a packet and a
CoreAudio player each). Now eight streams cost the worker 0.024 of a core in all, the tone's
drawing included, and the client one decoder and one player. Nothing a packet goes through
depends on how many streams listen, and the packet rate and the capture count do not move. The
0.005 between one stream and eight was not traced on this loaded machine.

**One sound with tiles of one app** (`a_display_and_two_windows_of_one_app_are_one_sound_on_the_wire`:
the drawn display and both drawn windows, owned by the test's own process, each a real stream
that joins the sound through `Pipeline::listen`): 122 packets in 1.2 s, 100.4 a second, one
sequence, all on the sound's media stream and none on the streams'. One capture heard every app
throughout. The windows' two streams then closed, and the longest wait between packets after
that was 14.5 ms against 10 ms packets. The worker unit test
`a_display_and_a_window_are_one_sound_with_no_gap_when_one_goes` holds the same change under
60 ms, with the capture's filter narrowed in place when the display goes.

**Each frame carries the bound of the estimate that timed it** (`FrameStamp::captured` is now a
`Captured`: the moment and how far off it may be, the estimate's bound as it stood plus the
drift from its anchor). `the_clock_probes_time_captures_as_the_shared_clock_does` held every
frame to the bound read at the end of the run. On loopback that run's last bound was 1.040 ms,
but the widest a frame had been placed within was 1.251 ms, and 6.243 ms against 8.021 ms
behind 5 ms each way. Early frames were timed by looser estimates than the last, so the old
check could fail a frame that was inside its own bound. Each frame is now judged by its own:
off the shared clock p50 0.038 / max 0.038 ms on loopback, and p50 0.319 / max 0.539 ms behind
5 ms each way. Placing a capture with its bound costs the decoder's callback 8.2–11.0 ns a
picture against 8.0–9.4 ns for the anchor alone, in the same run (`clock_path_cost`, 2 000 000
each, three rounds).

## 2026-10-01 — a page through the worker's proxy

A browser tile's page on any host but the loopback now goes through a SOCKS5 proxy on the
client, each connection a tunnel the worker dials (`docs/decisions/workspace.md`, "A browser
tile reaches every host the worker reaches"). A loopback page still goes through a forwarded
port, which is how every page loaded before. `tunnel_cost` times a fresh connection's GET of a
32 KiB page from a server on the real worker, to the answer's first byte and to its last. It
runs three ways, interleaved 300 times after 20 to warm up: a forward, the proxy given an
address, and the proxy given a name (`only-the-worker.localhost`, which the worker's resolver
answers with `::1` and `127.0.0.1`, while the server listens on IPv4 only). Client and worker
are on one Mac over loopback QUIC, release build, mac-studio, load 10 to 20. A path that crosses
a network adds its round trip to every way alike.

```
cargo test -p slopty-workerd --release --test e2e tunnel_cost -- --ignored --nocapture
```

| way | first byte p50 / p95 / p99 | last byte p50 / p95 / p99 |
| --- | --- | --- |
| forward | 274 / 475 / 613 µs | 579 / 815 / 1 031 µs |
| proxy, by address | 330 / 548 / 705 µs | 634 / 916 / 1 062 µs |
| proxy, by name | 326 / 604 / 1 549 µs | 638 / 901 / 1 854 µs |

The proxy costs a connection 56 µs at the median over a forward: the greeting and the request,
two exchanges with a process on the same machine. A CONNECT is answered before the worker
dials, so none of them waits on the link.

**The name cost 0.8 ms more until the worker kept its lookups.** In the first run, at the same
load, the forward's first byte was 443 µs at p50, the address's 529 µs and the name's 1 345 µs
(p99 992 / 1 142 / 5 319 µs). A probe in the worker's `connect` put the gap in the lookup:
`tokio::net::lookup_host` took 1 214 µs at p50 in the worker. The same lookup takes 172 µs in a
quiet process of its own, so the rest is the hop to the blocking pool on a loaded worker. Dialling
the refused `::1` first cost 28 µs. A page opens many connections to its host at once, so the
worker now keeps a name's addresses for each client for a minute, as Chromium keeps a system
lookup (`HostCache`), with the address that answered last first. An address that stops
answering is looked up again at once, so a host that moved costs one dial. The table above is
that run: the name now costs what the address does.

## 2026-10-01 — frame costs on gpui-fast 37b6e9a and gpui-kit 1cc5914d, and scroll layers on Apple, measured in Slopty

gpui-kit moved from `d6e51664` to `1cc5914d`: upstream `3a142844` (#3343 single-line Input
height, #3330 Textarea rows, #3329 TextView colour, #3322 capped code blocks, #3318 table
columns, #3281 Markdown source ranges) under our 37 commits. gpui-fast is at fork main `37b6e9a`.
That is `4afc87b` with formatting CI added, so the GPUI code is the code measured in "frame costs
on gpui-fast 4afc87b". The gpui-pre compat crates, still locked at `4afc87b`, moved with it.
Release, mac-studio, three runs at a load average of 6 to 12, median p50 / p95 in µs:

| case | bare, afresh | bare, keyed | sprites, afresh | sprites, keyed |
| --- | --- | --- | --- | --- |
| unchanged | 206 / 211 | 23 / 24 | 721 / 756 | 136 / 143 |
| cursor blinking | 204 / 211 | 27 / 28 | 723 / 763 | 145 / 152 |
| a key echoed at the prompt | 208 / 218 | 31 / 38 | 733 / 781 | 145 / 155 |
| a line of output a frame | 289 / 321 | 116 / 128 | 797 / 835 | 417 / 455 |
| `yes`, a screen of one line a frame | 176 / 211 | 175 / 212 | 173 / 205 | 177 / 209 |
| a screen of new text a frame | 2 081 / 3 755 | 2 107 / 3 913 | 2 575 / 4 225 | 2 530 / 4 106 |
| scrolling the scrollback a line a frame | 279 / 324 | 104 / 126 | 834 / 900 | 399 / 419 |

Every median is within a few percent of the `4afc87b` run. The p95 tails are shorter at this
load. The grid draws no gpui-kit component, so the kit moving should not show here, and it does
not. The navigator scrolling: p50 0.309, 0.325 and 0.324 ms, p95 0.375 to 0.431 ms (0.341 /
0.419 before), with 600 views built and no layer frames, since layers are compiled out on Apple.

```sh
cargo test -p slopty-ui --release --lib -- --ignored --nocapture terminal_frames_cost measure_the_navigator_scrolling
```

**Scroll layers compiled in on macOS.** A scratch tree, `git archive` of `HEAD` (`5a4357ab`)
into `target/scratch-layers/tree`, patches `[patch."https://github.com/aislopware/gpui-fast.git"]`
(and the `gpui-pre*` crates.io patches) over to a local clone of the fork at `37b6e9a`. In that
clone, `fast::layers::COMPILED` also names `target_os = "macos"`. One binary runs every measure
test in alternating rounds, with layers on and with `GPUI_SCROLL_LAYERS=0`. In that tree,
`NavList::set_rows` reads the list's offset only when it splices rows (docs/decisions/tooling.md,
"Scroll layers are on for macOS and iOS: the navigator and the face composite"). Medians of 8
rounds, p50 in ms:

| frame | layers off | layers on | change |
| --- | --- | --- | --- |
| navigator scrolling, 120 notes | 0.341 | 0.229 | −33% (500 of 600 frames composited) |
| face following an answer, 80 turns | 0.794 | 0.815 | +2.6% |
| face panning while it streams | 0.569 | 0.603 | +6.0% |
| navigator docked / hidden | 0.609 / 0.260 | 0.623 / 0.258 | +2.3% / −0.8% |
| palette glide, 300 lines | 0.214 | 0.217 | +1.4% |
| workspace frame over a large registry | 0.853 | 0.819 | −4.0% |
| a frame of motion; the keyboard moving | 0.334; 0.833 | 0.334; 0.829 | 0; −0.5% |
| echo, pointer, stream, notes, neighbour frames | | | −1.0% to +0.5% |
| the terminal grid, all 28 cases | | | −4.3% to +2.4%, none past the noise |

Without the `set_rows` change, the navigator with layers on composited 3 frames, repainted 3 and
was demoted once (p50 0.336 ms against 0.326 ms off): its render read the offset, so every
scroll counted as a change of the content. The first 60 frames of each run are bypassed because
of the selection plate under the list (`defer_unbaked`).

The face, 6 more rounds with a test added only to the scratch tree that pans a finished 80-turn
session ±40 points a frame. "Skip" is layers on with the demoted layer's per-frame
`changed_without_layer` check turned off:

| face frame | off | on | on, skip |
| --- | --- | --- | --- |
| following an answer | 0.843 | 0.804 | 0.862 |
| panning while it streams | 0.575 | 0.615 | 0.641 |
| panned at rest | 0.470 | 0.505 | 0.498 |

Following an answer moves both ways between the two sets: that is noise. Panning costs 7% with
layers on, whether or not the check runs. At rest, the trace shows 7 frames composited before a
content dependency changes about every 8 frames and the layer is demoted, plus 185 frames of a
row's settle animation, during which the layer is not promoted.

```sh
S=target/scratch-layers
(cd $S/tree && CARGO_TARGET_DIR=../target cargo test -p slopty-ui --release --lib --no-run)
$S/suite.sh $S/target/release/deps/slopty_ui-<hash> $S/runs-b 5   # on/off rounds of the measure tests
$S/compare.sh $S/runs-a $S/runs-b                                  # medians, off against on
# the decision trace: git -C $S/gpui-fast apply ../trace.patch, rebuild, GPUI_LAYER_TRACE=1
```

Logs: `target/logs/upstream-measure-{1,2,3}.log` and `target/scratch-layers/runs-{a,b,c}/`.

## 2026-10-01 — scroll layers on Apple: gpui-fast f71b3fe, gpui-kit 25b62b08

The steps of "Scroll layers are on for macOS and iOS: the navigator and the face composite"
(docs/decisions/tooling.md), each measured in the scratch tree of the section above. The setup
is the same: the measure tests alternate layers on and `GPUI_SCROLL_LAYERS=0`, 8 rounds, release,
mac-studio, other sessions building (load 12 to 21). Medians of p50 in ms:

| frame | today (off) | step 1 on | step 2 on | step 3 on | final off | final on |
| --- | --- | --- | --- | --- | --- | --- |
| navigator scrolling, 120 notes | 0.3455 | 0.2405 | 0.228 | 0.234 | 0.3435 | 0.221 |
| frames of 600 composited | 0 | 500 | 600 | 600 | 0 | 600 |
| finished face panned at rest | 0.508 | 0.5195 | 0.4995 | 0.4325 | 0.479 | 0.4215 |
| frames of 600 composited | 0 | 3.5 | 5 | 393 | 0 | 385.5 |
| face panning while it streams | 0.5855 | 0.602 | 0.575 | 0.5955 | 0.561 | 0.5405 |
| face following an answer | 0.832 | 0.8485 | 0.8045 | 0.8485 | 0.7595 | 0.7635 |

- **Step 1.** `NavList::set_rows` reads the offset only when it splices rows.
- **Step 2.** The selection plate is seated inside the selected row (`Plate::seat`).
- **Step 3.** gpui-kit's text view no longer changes its state when it is first built, or on
  the parser's acknowledgement (aislopware/gpui-kit#3).
- **Final.** The fork with #8 (the overflow below) and #9 (`COMPILED` names macOS and iOS),
  and the kit at `25b62b08`. That is `6d32ce6f`, the merge of #3, plus a CI-only commit.
- Columns are from two suites: "today" to "step 3" from `runs-d`, "final" from `runs-e`.
  Compare within a suite.

Every other frame stays within noise between final on and off: the chrome's echo, pointer,
motion and keyboard; the stream; the palette; the registry; long notes; the docked and hidden
navigator; and the 28 terminal cases (−5.9% to +0.7%, and −1.7% to +1.6% against today's off).

- **Where the 215 frames at rest that do not composite go.** The fixture ends on a settled
  prompt, so the composer's morph runs on the wall clock for `Pace::Settle`, 160 ms. A test
  frame takes about 0.75 ms, so the morph spans about 215 of them. While the face asks for
  animation frames, its list is not promoted (gpui-fast `fcc4533`). In the app that is 160 ms
  of frames, once per prompt. `a_face_panned_at_rest_composites_its_layer` runs under Reduce
  Motion and composites 60 of 60.
- **Step 4 found nothing to fix in the fork.** A `sample` profile of the face panning with
  layers on, after step 3, puts the layer bookkeeping at 77 of 9 482 samples on the test thread
  (0.8%). The largest parts are `policy::decide`, `lists::list_id` and `owner_scrolled_only`.
  Panning moves +1.7% and following +0.8% on against off, while one round differs from the
  next by about ±15%.
- **The overflow.** Step 2 made `measure_an_echo_frame_beside_long_notes` panic with layers on:
  "attempt to subtract with overflow" in `PaintIndex::shifted`. When the view holding the
  navigator was copied from the last frame, the shift subtracted the window's scene index from
  a list row's own scene index, then threw the result away. Fixed in the fork's `e831224` (#8),
  with a test that panicked before the fix.

```sh
S=target/scratch-layers
$S/refresh.sh                                       # tree from HEAD, patched to the local clones
$S/build.sh final                                   # release test binary into $S/bins/final
$S/suite2.sh $PWD/$S/runs-e 8 head final            # alternating on/off rounds, OUT absolute
$S/table.sh $S/runs-e head-off head-on final-off final-on
cargo nextest run -p slopty-ui composites_its_layer # the two tests that see layers composite
```

## 2026-10-01 — a finished task to its merge, across two workers

Mac Studio M1 Max, macOS 27.0, debug build, under `nice`, load 10–15 with other sessions
building; git 2.56.0.

```sh
cargo test -p slopty-cli --test projects -- verified_and_merged --nocapture 2>&1 | grep MEASURE
```

`a_finished_task_is_verified_and_merged_into_the_orchestrator_s_clone` uses two real workers on
one machine and a local forge reached through `insteadOf`. The clock starts when the stub agent
is let go to report done, and stops when the task reads merged. In between:
- the agent's MCP `task_report`;
- the branch's bundle from one worker, sent through the server and fetched into the other;
- the project's checkout made with `git worktree add`;
- the verifier as a terminal under a login shell (`cat work.txt && grep -qx done work.txt`);
- the rebase check;
- the fast-forward with `merge --ff-only` in the person's checkout.

| run | done to merged |
| --- | --- |
| 1 | 679 ms |
| 2 | 620 ms |
| 3 | 755 ms |

The verifier itself is a few milliseconds here. What remains is the queue's own cost: the git
calls, the terminal's login shell, and the 2 s progress read, which a short run never waits for
because the exit wakes the lane. Nothing on this path is a hot path for input or frames, so no
optimisation follows. A real verifier (`cargo gate`, about a minute warm) dwarfs it.


## 2026-10-01 — a finished task to a reviewer's word

Mac Studio M1 Max, macOS 27.0, debug build, under `nice`, other sessions building.

```sh
cargo test -p slopty-cli --test projects -- a_reviewer_s_block --nocapture 2>&1 | grep MEASURE
```

`a_reviewer_s_block_goes_back_to_the_agent_and_the_person_s_word_merges_it` uses one real
worker and the stub claude as both the task's agent and its reviewer. The clock starts when the
agent is spawned and stops when the task's card carries the reviewer's verdict. In between:
- the agent's `task_report`;
- the verifier as a terminal;
- the review checkout with `git worktree add` and the diff written beside it;
- the reviewer's spawn through ptyd;
- its MCP `review_report`.

| run | spawn to the reviewer's word |
| --- | --- |
| 1 | 883 ms |
| 2 | 466 ms |
| 3 | 573 ms |

This is the server's own path. A real reviewer reads for minutes, and nothing here is on an
input or frame path, so no optimisation follows.

## 2026-10-01 — a reload's diff

A file tile tints the lines a reload changed. The diff ran under an 8 ms wall-clock timeout and
fell back to tinting more lines when it passed it, so a loaded machine tinted differently: CI
run 36861354571 tinted an unchanged line in
`a_clean_tile_takes_a_change_on_disk_and_tints_the_lines`. It is now bounded by size instead:
the common head and tail are trimmed, and a middle of up to `DIFF_EXACT_LINES` (2 000) lines is
compared exactly, with no clock. Larger middles tint every line between head and tail.

Mac Studio M1 Max, release, the worst case at the bound (two 1 000-line middles with nothing in
common), 21 runs: p50 0.22 ms, max 0.82 ms.

```sh
cargo test -p slopty-ui --release --lib measure_the_reload_diff_at_its_bound -- --ignored --nocapture
```

## 2026-10-01 — the editor's helpers

What a file tile's editing helpers cost on the UI thread at their bounds (`file::edit`). The
bracket pair is looked for again on every caret move and keystroke, so it is on the input path;
the others run once per read or per command.

Mac Studio M1 Max, release, load average 26 from other sessions' builds, medians of 21 runs:

| What | Median |
| --- | --- |
| Bracket at the caret, its pair 64 KiB away (the scan's bound) | 218 µs |
| Bracket at the caret, unmatched (the whole 64 KiB scanned) | 212 µs |
| No bracket next to the caret (the usual keystroke) | 0.08 µs |
| Indentation guess over its 10 000-line sample | 184 µs |
| Comment toggled over 1 000 lines | 50 µs |

The worst case is under 3 % of a 120 Hz frame and only arises with the caret next to a bracket
whose pair is hundreds of lines off; `BRACKET_SCAN_BYTES` bounds it by size, never by the clock.

Round two, same machine and method, load average 23 to 32. A find runs again on every edit while
its field is open, so it is on the input path; the symbol list and the word candidates run off
the UI thread.

| What | Median |
| --- | --- |
| Find over 1 MiB, a word on every line (10 000 matches kept) | 708 µs |
| Find over 1 MiB, a word not in it | 119 µs |
| Find over 1 MiB, the pattern `\w+\(` | 494 µs |
| Find over 16 MiB (the largest text a worker sends), a word on every line | 707 µs |
| Find over 16 MiB, a word not in it | 1.9 ms |
| The editor's rope made one string, per MiB (each find starts so) | 269 µs |
| Symbols over 1 MiB of Rust (off the UI thread) | 635 ms |
| Word candidates for `va` over 1 MiB (off the UI thread) | 1.8 ms |
| Word candidates for `va` over 16 MiB (off the UI thread) | 30 ms |

A find stops at `MATCHES_MAX`, which is why a common word costs the same at 1 and at 16 MiB.
The cost an edit pays is the string copy plus the scan: about 0.4 ms at 1 MiB, and some 6 ms in a
16 MiB file, the one size where it nears a 120 Hz frame. The symbol list parses as a colouring
does, so it reads a file only up to `COLOURED_BYTES` (2 MiB, about 1.3 s, "Reading symbols…"
shown meanwhile); past it the tile is plain text and lists none.
The word candidates are read from the 1 MiB round the caret (`complete::SCAN_BYTES`), so a
keystroke costs the 1 MiB row above at any file size rather than the 16 MiB one.
Word completion was cut on 2026-10-05 (prune #10), and its rows with it; the test no longer
times it.

```sh
cargo nextest run -p slopty-ui --release --run-ignored only timing_of_the_editor_helpers --no-capture
```

## 2026-10-02 — pseudo-terminals in the tests lane, and a refusal far below the limit

CI's tests lane twice had `/dev/ptmx` refuse an open with ENXIO, which `slopty-pty` reports as
"every pseudo-terminal the system allows is in use (kern.tty.ptmx_max)" (runs 36817494749 and
36892141848). Both times the test was `slopty-pty::spawn`
`shells_start_while_other_threads_allocate_and_hold_locks`, and in the first run its neighbour
`no_descriptor_of_the_daemon_leaks_into_a_shell` failed too. A `lsof -n /dev/ptmx`
after the lane found nobody holding one.

**What the tests hold.** The tests lane as CI runs it (`cargo xtask gate --ci --lane tests`) on a
`git archive` of 9f34d7cb, on mac-studio (Darwin 27.0.1) under `nice -n 10`, counted every
200 ms by `cargo xtask ptys`. A pair counts once whether its master, its slave or both are held,
by any process under the gate or any that left it with the run's `NEXTEST_WORKSPACE_ROOT`.

| Run | Test threads | Wall | Peak held by the tests | Lowest free minor at the peak | Held after their test |
| --- | --- | --- | --- | --- | --- |
| HEAD | 10 (this Mac) | 77 s | 25 | 17 | none |
| HEAD | 3 (a runner's cores) | 171 s | 18 | 23 | none |
| HEAD, `regrown`, the sampler in the lane | 3 | 327 s (load 24 to 58 from other sessions) | 16 | 18 | one `bash`, in one sample (under the grace) |

The third run's lane failed on one test, which the count did not cause: `slopty-pty`
`the_tty_is_the_controlling_terminal_of_the_child` read `dd`'s statistics up to "records out", so
when the last line came in a read of its own nobody read it, and `dd`, a session leader whose
exit waits for its tty to drain, never exited (180 s, the timeout).

At the peak of the first run, `shells_start_while_other_threads_allocate_and_hold_locks` held 16,
`no_descriptor_of_the_daemon_leaks_into_a_shell` 2, and seven processes of the `slopty-pty`
shell integration tests (`fish`, `bash`, `sh`, `git`, `xcodebuild`) one each. No other test held
more than 4 at once: `slopty-worker::agent_open` 4, the `slopty-cli::projects` tests 3 each.
Over the whole run the probe's lowest free minor never passed 30, the most this user held, so no
pair was held out of the census's sight. CI's `JUnit` reports agree: at both failures three tests
ran (the runner has three cores), the two spawn tests and `slopty-predict::editing`, which opens
none, so at most about 20 were held, and `slopty-ptyd` tests opening pairs 1.4 s later passed.

**What refuses the open.** XNU's table of pairs (`bsd/kern/tty_ptmx.c`, xnu-12377.121.6) grows
16 slots at a time and never shrinks. `ptmx_clone` hands an open the first empty slot, or the
slot one past the table when it is full. `ptmx_get_ioctl` grows the table only when no slot is
free, so when a pair is closed between the two, the table stays as it is, the minor is out of its
range, and the open fails with ENXIO ("minor number %d was out of range"). It takes a full table
and a close at that moment, so it shows on a freshly booted Mac whose table has never grown past
what the tests hold: a hosted runner is one, and this Mac, after days of sessions, is not.

`slopty-pty --test ptmx_churn` (ignored) opens 400 pairs and holds them while four threads open
and close one each. On the macOS 26.6.2 guest (`slopty-26-dev`, tart, 4 cores, limit 511), each
run on a fresh boot:

| `open_master` | Refused while 400 were opened | Held at each refusal |
| --- | --- | --- |
| as at 9f34d7cb | 12; 14 | 14, 46, 62, 158, ... 396; 30, 46, 62, 94, ... 397 |
| made again on ENXIO (`regrown`, up to 16 times) | 0; 0; 0 | — |

Each refusal came with the held pairs, the churners and the guest's own two or three summing to
a multiple of 16: the table was full. A second run on the same boot refused none either way,
since the table had already grown past 400. CI's two failing tests, run unchanged but with 120
shells in flight, passed on two fresh boots: the race is rare at a dozen pairs and certain over
a few hundred.

**The sampler's cost.** 17.6 to 18.9 ms a sample here, with some 640 processes of this user, and
10.5 ms in the guest: under a tenth of a core at one sample every 200 ms.

```sh
# the tests lane, counted (here, or on a runner, which runs it in every tests lane)
cargo xtask ptys --workspace "$PWD" -- nice -n 10 cargo xtask gate --ci --lane tests
# three test threads, as a runner has
NEXTEST_TEST_THREADS=3 cargo xtask ptys --workspace "$PWD" -- cargo xtask gate --ci --lane tests
# the refusal, in a freshly booted guest
cargo xtask vm start
cargo nextest run -p slopty-pty --test ptmx_churn --no-run   # then copy the binary in
cargo xtask vm ssh -- '/tmp/ptmx_churn --ignored --nocapture'
cargo xtask vm stop
```

## 2026-10-02 — scrollback memory and idle compression

Release, mac-studio, other sessions building beside it. `memory_cost`
(`crates/slopty-engine/src/ghostty/memory/tests.rs`) fills an 80×24 terminal with 10 000 lines,
reads libghostty's own count of its pages (`GHOSTTY_TERMINAL_DATA_MEMORY_USAGE`, ghostty #14499),
compresses the history a step at a time as an idle session does (`GhosttyEngine::compress_history`),
and formats the screen before and after. Plain is a line of text; coloured is a 256-colour style
per word; shell is a marked prompt, a command and a 20-line listing whose names are coloured and
linked (OSC 133, OSC 8). An empty terminal holds 409 600 bytes (one page and the screen).

```sh
cargo nextest run --release -p slopty-engine --run-ignored only -E 'test(memory_cost)' --no-capture
```

| history | resident | a line | compressed | a line | pages compressed | steps |
| --- | --- | --- | --- | --- | --- | --- |
| plain | 6.96 MB | 657 B | 0.55 MB | 14 B | 16 of 17 (141 kB) | 18 |
| coloured | 9.19 MB | 880 B | 2.81 MB | 241 B | 16 of 17 (2.27 MB) | 18 |
| shell | 12.81 MB | 1 241 B | 2.22 MB | 181 B | 16 of 17 (1.47 MB) | 18 |

A step compresses about a page: 0.79 M instructions (59 µs) plain, 6.9 M (0.64 ms, p95 0.70 ms)
coloured, 4.7 M (0.59 ms, p95 0.66 ms) shell. The page the screen is on stays resident. Address
space is not given back (7.4, 10.8 and 14.5 MB reserved).

**A checkpoint used to undo it.** ghostty's formatter read each page through `Node.page()`, which
restores a compressed page for good: one checkpoint took the plain history from 0.55 MB back to
6.96 MB. The fork's formatter now decodes a compressed page into a buffer of its own
(aislopware/ghostty #10, as ghostty's snapshot encoder and search already did) and the history
stays compressed. Decoding costs the format of 10 000 lines:

| history | format, resident | format, compressed |
| --- | --- | --- |
| plain | 43.7 M (2.30 ms) | 46.7 M (2.43 ms), +7 % |
| coloured | 128.5 M (7.58 ms) | 165.6 M (9.56 ms), +29 % |
| shell | 90.5 M (5.13 ms) | 115.7 M (7.14 ms), +28 % |

**The scrollback limit bounds it.** At a 2 000-line limit, 10 000 coloured lines hold 1.62 MB, no
more than 2 000 lines did (2.16 MB): pruning is by whole pages (`scrollback_memory_stays_within_its_bound`).

No frame or fetch series moved against a copy of the tree without the change: `take_frame_unchanged`
901 and 997, `write` 813, `fetch_lines_cost.4096_rows` 104.21 M.

## 2026-10-02 — ghostty sync to 83edd491e, Kitty drag and drop, and a frame series that moves with the heap

Release, mac-studio, other sessions building beside it; each pass ran only with the 1-minute load
under 20, and the load is given beside it. The same engine and binding code were built against
three ghostty trees: the old pin (5439e2b), the sync onto upstream 83edd491e with #14508 and
#14509 (3e0c377), and that plus Kitty drag and drop (#14511 at a36d3f5, 497a316). Each build
went to its own target directory with `GHOSTTY_SOURCE_DIR` pointing at its tree, and the
prebuilt `*_cost` binaries ran back to back, interleaved.

```sh
GHOSTTY_SOURCE_DIR=<tree> CARGO_TARGET_DIR=target/bench-<name> cargo test --release -p slopty-engine --tests --no-run
cd crates/slopty-engine && SLOPTY_BENCH_OUT=<out>.jsonl <binary> --ignored _cost --test-threads=1
```

**The sync moves nothing.** At a load of 15 to 17, `frame_cost.write` was 813 in all three trees,
`take_frame_unchanged` 901 (60×12) and 997 (200×60), `take_frame` 18 963 to 18 985 (60×12) and
44 910 to 44 934 (200×60). `encode_cost.echo` was 4 148 and `full_200x60` 1 084 187 in all
three. The checkpoint series agree within 0.6 % (`format` 50.95 M to 51.01 M). A first pass at a
load of 19 to 33 showed moves of about 50 instructions either way, which reruns did not repeat.

**The engine's drag and drop costs the write path nothing measurable.** Against the 497a316 tree
without the engine's wiring, at a load of 8 to 19, over eight interleaved rounds: `write` median
813 unwired and 830 wired, with both spreading from about 690 to 920 round to round. A write
the program did no drag and drop in reads one flag and branches (`after_dnd` is inlined, the
work behind it is not). Every other series stays within its own spread.

**One frame series moves with the heap, not with the code.** `scroll_frame_cost.80x24` is
123 460 to 123 530 in every unwired run (30 of 30). With the wiring it is either that or
124 200 to 124 330 (+0.6 %), about 40 % of the time (6 of 16, then 6 of 14). `take_frame` runs
no drag and drop code. A build that registers a callback capturing nothing (no allocation) is
never high (0 of 14), and one that makes the wiring's allocations without registering is never
high (0 of 14). So what moves it is the callback's 16-byte box shifting the heap. `MallocNanoZone=0`
moves both builds' counts (unwired 123 290 to 123 360) and makes the high level more common (10 of
12 wired), so `take_frame` at 80×24 has a cost that depends on where its buffers fall. That is
`take_frame`'s own, and finding it is a follow-up.

## 2026-10-02 — grapheme clustering on by default

Release, mac-studio, other sessions building beside it; each pass ran with the 1-minute load
under 20 (18.7), before and after back to back. The engine with mode 2027 on from the start
(`set_default_mode(Mode::GRAPHEME_CLUSTER, true)`) against the same tree without that line.
`unicode_write_cost` writes a 76-column line and its line break into an 80×24 screen, 500
times: plain ASCII; CJK (wide); and emoji sequences (a ZWJ family, a flag, a skin tone, a
combining mark, a variation selector, five times over).

```sh
cargo nextest run --release -p slopty-engine --run-ignored only -E 'test(unicode_write_cost)' --no-capture
```

| line | instructions, off | on | wall p50, off | on |
| --- | --- | --- | --- | --- |
| ascii | 3 143 | 3 147 to 3 172 | 208 ns | 208 ns |
| cjk | 4 450 | 4 990 (+12 %) | 292 ns | 292 ns |
| emoji | 15 715 | 32 981 to 32 995 (×2.1) | 1.0 µs | 2.3 to 2.5 µs |

The whole `_cost` suite, interleaved twice at a load of 17 to 19, moved nothing else:
`frame_cost.write` 842 off and 821 on, `fill_engine` 31.14 M and 31.11 M, `encode_cost.echo`
4 149 both. `search_after_output_cost` (only the search is timed) read 1.17 M to 1.21 M off
and 1.20 M to 1.22 M on over five runs each, overlapping.

## 2026-10-02 — search over soft wraps, and Kitty notifications in the ghostty fork

Release, mac-studio, other sessions building beside it; every pass ran with the 1-minute load
under 20 (17.7 to 19.9), before and after interleaved. Before is the engine with
row-by-row search against ghostty `497a316`. After is the engine searching soft-wrapped lines
as one against ghostty `9cb1c04`, which adds Kitty notifications (OSC 99).

```sh
cargo nextest run --release -p slopty-engine --run-ignored only -E 'test(search_cost) | test(search_after_output_cost) | test(osc_write_cost) | test(frame_cost)' --no-capture
```

| series | before | after |
| --- | --- | --- |
| `search_cost.50000_lines.plain` (every row hits) | 103.8 M, 6.3 ms | 108.9 M (+4.8 %), 6.7 ms |
| `search_cost.50000_lines.regex` | 84.0 M | 86.5 M (+2.9 %), 6.1 ms |
| `search_cost.10000_lines.plain` | 21.6 M | 22.7 M (+4.8 %) |
| `search_after_output_cost.plain` | 1.21 M | 1.24 M to 1.27 M (+2 to 5 %) |

The rescans above reuse the history's text. A first search also reads each history row's
soft-wrap flag, once: at a load of 13.5, `search_cost.50000_lines.wraps` is 21.5 M
instructions and 2.4 ms, against 139 M and 9.9 ms for the format of the same rows (1 000 rows:
0.19 M; 10 000: 2.3 M). Three changes took the rescan from +40 % to this: no allocation for a
line of one row, no reference count per hit, and no second pass over the text to find where a
wrapped tail starts.

Nothing on the write or frame path moved with ghostty `9cb1c04`, over three passes each:
`osc_write_cost.per_osc` 7 086 to 7 089 before and 7 091 to 7 096 after,
`scroll_frame_cost.80x24` 123 476 to 123 519 and 123 457 to 123 513. `frame_cost.write` read
771 to 796 and 823 to 848, inside the 690 to 920 spread it shows run to run.

## 2026-10-02 — a turn snapshot of this repository

What a turn's snapshot costs the worker (`repo::snapshot::Repo::take`: `git add -A` and
`git write-tree` through the thread's own index), on a local clone of this repository (1 683
tracked files) in the temporary directory. M1 Max, git 2.56.0, three runs while other lanes
built under `nice`:

```
cargo test -p slopty-worker --test review -- --ignored --nocapture snapshot_cost
```

| snapshot | run 1 | run 2 | run 3 |
| --- | --- | --- | --- |
| first (from `HEAD`, every file hashed) | 216 ms | 322 ms | 245 ms |
| nothing changed | 27 ms | 108 ms | 99 ms |
| nothing changed | 25 ms | 96 ms | 102 ms |
| one file changed | 30 ms | 96 ms | 90 ms |
| nothing changed | 30 ms | 95 ms | 101 ms |

Keeping the index between snapshots is what makes the later ones cheap: git's stat cache
skips every file not touched since, where a fresh `read-tree HEAD` each time would hash the
whole tree again, as the first one does. The spread between runs is the machine's load: both
are two git processes. None of it is on the turn's path: the edge goes out at once and the
snapshot follows as its own action.

## 2026-10-02 — the thread view's path on the UI thread

What the client pays per change for an agent's thread, over a snapshot's worth of a long one:
`SNAPSHOT_TURNS` (20) turns of 40 commands each, every command with a 2 KiB output, 840 items in
all, 1.8 MB as cached. A cached thread is read on the UI thread so it draws in the view's first
frame; everything else here runs on every frame the thread moves.

Mac Studio M1 Max, release, load average 22 to 50 from other sessions' builds, medians of 21
runs:

| What | Median |
| --- | --- |
| The cache read and decoded (the first frame of a cached thread) | 498 µs |
| The cache encoded and written (off the UI thread) | 2.3 ms |
| A streamed word applied to the mirror, its outbox settled | 0.17 µs |
| The rows built again, settled turns folded | 3.4 µs |
| The rows built again, every turn open | 17 µs |
| The activity bar's stack | 0.08 µs |

A streamed word costs well under a microsecond to apply and a few to lay out again; the rows
are drawn from where they were built, so a row never searches the thread for its item. The
first frame of a cached thread is the only read on the UI thread, at about 6 % of a 120 Hz
frame for a long thread.

```sh
cargo nextest run -p slopty-ui --release --run-ignored only timing_of_the_thread_path --no-capture
```

## 2026-10-02 — a decode submission that never returned

The clock test failed on CI (run 36940096068, log `target/logs/ci-tests-36940096068.log`) with
"no picture for 10s after 24 … both sides running and none inside VideoToolbox". The figures
both sides gave at the panic:

| | client | worker |
| --- | --- | --- |
| frames | 25 | 244 encoded |
| datagrams | 211 | 1335 |
| refreshes | 9 | 9 |
| decode errors | 1, status -19092 | |
| `datagrams_lost` | 0 | |
| `reader_lag_max` | 1.17 s | |
| longest gap, stalls | 68 ms, 0, not stalled | |
| LTR | | 3 offered, 0 acknowledged |

The client's figures stop partway through the stream. It had 211 datagrams of the worker's
1335, yet it saw no stall and no gap over 68 ms across 10 s without a picture. The clock
estimate had last moved 2.66 s after the first datagram. The process took 170 s past the panic
to exit, until the runner's 180 s limit. A runtime thread was held for good. The stream's task
made every VideoToolbox call itself (`Decoder::decode` in `deliver`), so a call that does not
return stops the task. It then reads, reports and refreshes nothing more. `next_or_stopped`
counted the task as running anyway, because a report published just after the last picture
moved its datagram count.

**Reproduced here** in a copy of the tree (`target/scratch-layers/tree`). The copy forced a
-19092 on the 24th frame and then held the stream's task for 40 s, standing in for the call
that does not return. Under `taskpolicy -b`, the 5 ms case panicked with the CI message
(`target/scratch-screen/block40.log`). The client's counters were frozen at 106 datagrams,
`decoding` 0, not stalled, no gap, while the worker went on to 2083 datagrams. The test then
took 81.8 s to finish. A one-off failure with no hang recovered every time, in the submit or
in the callback, at frames 10, 24 or 25. So did a failure followed by a 1.2 s or 3 s hold.
60 of 60 plain runs passed under 6 concurrent instances. Recovering from the failure was never
the problem.

**After**: each lane decodes on a thread of its own (`DecodeThread`). A submission held up
past `DECODE_STUCK` loses its decoder, as `docs/decisions/video.md` records ("The client
decodes on a thread per coded picture"). Time is charged only while the worker runs on time.
The same scenario, a -19092 on submission 24 and then submission 25 held for 40 s inside the
decode thread (`SCRATCH_HANG_AT`):

| run | result | time | client after the 0 ms case |
| --- | --- | --- | --- |
| hold at 25 (0 ms case) | passed | 4.3 s | 1 decoder replaced, 2 refreshes, 0 lost |
| hold at 100 (5 ms case) | passed | 4.4 s | 1 decoder replaced, 2 refreshes, 0 lost |
| 4 instances × 5, `taskpolicy -b`, hold at 25 | 20 of 20 passed | 9.4–60 s | |

The process exits at once, with the held thread still asleep in it. Before a full decode
queue went quiet, every refresh answered by an IDR met the same full queue and was asked for
again: 108 refreshes and 108 refusals in the 2 s. Now the frames that don't fit are dropped and
counted, 83–89 of them, and the replacement's keyframe is the one refresh.

**Under load, base against fix**: 4 instances of each binary ran at once under
`taskpolicy -b`, 6 runs each, load average 50–57 (`target/scratch-screen/ab-*.log`).

| | passed | failed |
| --- | --- | --- |
| before | 17 | 7, every one the p50 clock accuracy (1.2–20.5 ms against 1 ms) |
| after | 20 | 4, every one the p50 clock accuracy (2.2–3.3 ms) |

Neither build stalled. The p50 assertion fails on both when the machine starves the test, a
failure of its own. An earlier batch that charged the wall clock replaced a decoder twice in
one run at load 85–110 while the worker's encoder was 24 s behind. That run then panicked
"both sides running" waiting on the keyframe, which is why only time the worker ran is charged
now. In the batch above, four runs had one replacement each, all in the first round at the
highest load. Their streams went on to 60 pictures and then failed the p50 assertion, as three
base runs of the same round did.

**The hop's cost.** `decode_hop_cost` takes a frame onto the decode thread at 60 frames a
second, beside the wake every frame already paid: a datagram handed to a stream task parked
on a user-interactive runtime. Three runs, load average 27–51, µs:

| | p50 | p90 | p99 | max |
| --- | --- | --- | --- | --- |
| decode thread hop | 7.6–15.8 | 86–730 | 1302–7636 | 5906–36059 |
| stream task wake | 17.5–21.5 | 43–270 | 760–4448 | 1792–29540 |

A `parking_lot` mutex and condvar in place of the bounded `std::sync::mpsc` channel was no
better (p50 17.8–21.0, p99 1.9–8.6 ms), so the channel stays. End to end, from the glass test
(`capture_and_input_to_glass`, both builds alternated, 6 runs each with 2 cases a run, load
20–111), arrival → decoded:

| | p50, median of 12 | p50 range | p95, median of 12 | p95 range |
| --- | --- | --- | --- | --- |
| before | 1.30 ms | 1.16–1.43 | 6.9 ms | 1.9–11.9 |
| after | 1.35 ms | 1.20–1.48 | 7.6 ms | 2.3–11.5 |

The 0.05 ms is within either build's spread. Decoded → painted swings between about 4 and
about 13 ms from run to run in both builds, with the paint timer's phase. It is not the
decoder's.

```sh
cargo nextest run -p slopty-client -E 'test(stuck_in_a_submission) | test(full_queue_drops)'
cargo nextest run -p slopty-client --run-ignored only -E 'test(decode_hop_cost)' --no-capture
cargo test -p slopty-worker --lib --no-run   # copy the binary to /tmp, as for the glass test
./slopty_worker --ignored --exact screen::synthetic::tests::capture_and_input_to_glass --nocapture
```

## 2026-10-02 — the thread view against its latency budgets

The GUI-first plan's interaction and wire budgets (`.research/gui-first-2026-10-01/plan.md`
§4.4), held against the thread view: a cached thread paints in the first frame, a send shows as
its pending bubble in the frame of its ↵, a keystroke reacts in under 50 ms, and an agent's word
reaches its glyph within one round trip plus 20 ms at p50.

Mac Studio M1 Max, macOS 26, load average 16 to 94 from other sessions' builds.

**The view's frames, headless** (`timing_of_the_thread_s_frames`, release, medians of 11 views
and 21 keystrokes). The thread is a snapshot's worth of a long one: `SNAPSHOT_TURNS` (20) turns
of 40 commands each, 840 items, 1.8 MB cached. The headless window lays out and paints but
rasterizes nothing, so these are the UI thread's share of a frame.

| What | Median | Budget |
| --- | --- | --- |
| The view made, its cache read and decoded | 1.0 ms | |
| The first frame of a cached thread: view made, laid out and painted | 2.8 ms | the first frame |
| A send drawn as its pending bubble in the frame of its ↵ | 1.3 ms | the same frame |
| Every step opened or folded by ⌃O | 0.79 ms | 50 ms a keystroke |

The path under those frames is unchanged from this morning's section (the cache read 516 µs, a
streamed word applied in 0.17 µs, the rows built in 3.4 µs folded and 17 µs open).

**The app's frames while an answer streams** (`frame_time::the_thread_draws_a_streaming_answer_within_a_frame`,
debug build with optimisation, a 1000×720 window at 60 Hz, 5 s each, draw time p50 / p95 / p99
/ max). The answer grows by a word every 16 ms, as the Claude Code mod reports it.

| Scenario | Draw | Frame interval p50 / p99 | Over 16.7 ms | Dropped |
| --- | --- | --- | --- | --- |
| (h) following a streaming answer | 1.5 / 1.8 / 2.7 / 5.4 ms | 16.7 / 17.6 ms | 0 of 298 | 0 |
| (i) panning at 120 events/s while it streams | 2.1 / 3.8 / 5.5 / 5.8 ms | 16.7 / 17.5 ms | 0 of 300 | 0 |
| (j) every step open, panning, nothing streaming | 1.7 / 3.3 / 3.5 / 4.7 ms | 16.7 / 17.5 ms | 0 of 300 | 0 |

**A streamed word to the frame that shows it** ((k), the same test, 60 words). Each word is a
token of its own, posted to the worker's mod socket and timed until the first dump whose
accessibility tree holds it. A dump waits for the next frame, so the dump's own round trip is
in every sample, and is timed alone beside it.

| What | p50 | p95 | max |
| --- | --- | --- | --- |
| Post to a frame that shows the word | 32.3 ms | 48.6 ms | 50.1 ms |
| A dump alone (the next frame and the tree) | 16.6 ms | 18.1 ms | |

Over loopback the round trip is near nothing, so the word costs about 16 ms past the dump's own
frame at p50: the worker's 16 ms delta coalescing, then the frame. That meets one round trip plus
20 ms at p50; the p95 adds a second frame, where the word landed just after a frame began.

Every figure is inside its budget, and with a wide margin: the first frame of a long cached
thread costs a third of a 120 Hz frame, and no streaming or panning frame came near 16.7 ms.

```sh
cargo nextest run -p slopty-ui --release --run-ignored only -E 'test(/timing_of_the_thread/)' --no-capture
cargo xtask e2e smooth --filter 'test(/frame_time::the_thread/)'
```

## 2026-10-02 — renderer: text drawn antialiased (gpui-fast 9072710)

The workspace draws text with gpui-fast's `TextSmoothing::Antialiased` (fork PR #11, `2d3df6a`)
in place of `Native`. Under `Native`, macOS dilates a glyph by the luminance of its colour, as Core
Graphics' font smoothing does. The dark theme's `#e6e6e6` text is drawn at level 3 and
`#1d1d1f` at 0, so the same weight inks heavier on dark. Antialiased draws every colour at
dilation 0.

**Glyph ink.** Printable ASCII (94 glyphs) rasterised by Core Text at 2×, in release, on
mac-studio. Ink is the sum of the coverage bytes, against dilation 0. The semibold face inks
+30.0 % over the regular (10 710 px against 8 236 px), so +13 % is about four tenths of the step
from 400 to 600, nearly a whole CSS weight.

| Face | d2 | d3 | d4 | raster per glyph, p50 (d0 … d4) |
| --- | --- | --- | --- | --- |
| system 13 regular | +6.5 % | +13.0 % | +14.9 % | 6.95 to 7.19 µs |
| system 13 semibold | +5.0 % | +10.0 % | +11.4 % | 6.94 to 7.04 µs |
| system 15 regular | +6.5 % | +10.9 % | +10.9 % | 6.86 to 7.00 µs |
| Menlo 13 | +6.1 % | +12.1 % | +13.9 % | 3.88 to 3.95 µs |

A raster costs the same at every level. Under `Native` one glyph in four tones of text can
hold up to four rasters in the atlas. Antialiased, it holds one.

**Frame cost.** The same tree was built twice. A calls `set_text_smoothing(Native)` and B
`Antialiased`: the binaries differ only in that argument (`mov w1, #0x0` against `#0x1` before
the call). The two smooth scenarios with the most text ran in alternating rounds, A then B,
three rounds each, one test at a time, at a load average of 12 to 24 (another session was
building). Draw time p50 / p95 / p99 in ms, the median of three rounds, then the worst max:

| Scenario | A, Native | B, Antialiased | Over 16.7 ms (A / B, all rounds) |
| --- | --- | --- | --- |
| (a) 20 streaming shells, strip column to column | 0.9 / 2.3 / 2.6, max 3.0 | 0.8 / 2.2 / 2.4, max 5.6 | 0 / 0 of about 950 |
| (b) 20 streaming shells, overview in and out | 1.1 / 2.3 / 3.4, max 18.1 | 1.0 / 2.1 / 3.1, max 16.6 | 1 / 0 of about 940 |
| (j) typing into one shell, its echo drawn | 0.3 / 0.4 / 0.6 | 0.3 / 0.4 / 0.6 | 0 / 0 of 183 |

The frame cost is unchanged: every B median is equal to or a tenth below A's, within the noise
at this load. B's 5.6 ms max in (a) came in its last round, while the load average reached 38.

```sh
# glyph ink and raster cost, in the gpui-fast checkout
IPHONEOS_DEPLOYMENT_TARGET= cargo test -p gpui_macos --features font-kit --release --lib \
  measure_dilation -- --ignored --nocapture
# frame cost: build the e2e binaries once per argument, copy each into target/r-ab/{A,B}, then
# alternate rounds (target/r-ab/run.sh: SLOPTY_E2E_BIN_DIR=target/r-ab/$v SLOPTY_BINS_FRESH=1 and
# a data dir of its own)
cargo nextest run --binaries-metadata target/r-ab/smooth.json --cargo-metadata \
  target/r-ab/metadata.json --run-ignored only --test-threads 1 \
  -E 'test(~on_the_mac) & (test(twenty_streaming_shells) | test(typing_draws_only_the_echo))'
```

## 2026-10-02 — the tests lane's build, and the test profile at opt-level 0

What bounds a CI run, and the first change against it (`docs/decisions/tooling.md`, "Tests
build the workspace's own crates at opt-level 0"). The CI figures are from the gate runs on the
`gate` branch; the local ones from mac-studio (10 cores) while five other sessions built and
tested, at a load average of 20 to 30, every command under `nice`.

**CI, before.** Over the last 22 gate runs the tests lane took 27–48 min, clippy-ios 12–25,
clippy-host 7–16, rustdoc 7–12 and tools about 3. In the tests lane of run 36963072630:

| Step | Time |
| --- | --- |
| `cargo xtask setup` (mostly compiling xtask) | 2 min |
| `nextest build` | 1 626.7 s |
| nextest's run (JUnit `time`, 3 629 tests) | 435.9 s |
| doctests, beside the run | 158.6 s |

Its `cargo-timing.html`: 899 units whose durations sum to 4 830 s over 1 613.7 s on 3 cores,
so the build is CPU-bound with no chain to shorten. sccache hit 99.4 % of what it could cache,
and 298 compiles it could not (274 by crate type: binaries, test harnesses, proc macros).
Workspace test harnesses took 2 902 s, workspace libraries and binaries 767, build scripts 354
and dependencies 807. The largest units were `slopty-ui`'s test harness (653 s) and
`slopty-workerd`'s binary (332 s) and its test harness (219 s). Of the last 41 runs, 14 passed,
16 failed and 10 were cancelled by a newer push, two of them at 47.6 and 48.3 min. The run
after (36969382434) ran 3 629 tests in 444.3 s; the most per package: xtask 213 s (the icon
test 157), `slopty-engine` 183, `slopty-ui` 149, `slopty-worker` 139, `slopty-workerd` 137,
`slopty-client` 115.

**Build CPU, opt-level 1 against 0.** One crate's library and test harness, rebuilt alone in a
target dir of its own after both levels had built everything under it, A then B, twice.
`RUSTC_WRAPPER` was empty so that rustc's CPU shows in `time`.

| `slopty-agent`, `nextest run --no-run` | User CPU | Wall |
| --- | --- | --- |
| A, opt-level 1 | 109.7 s, 115.8 s | 31.3 s, 104.9 s |
| B, opt-level 0 (the compute crates at 1) | 30.4 s, 28.5 s | 25.8 s, 20.3 s |

3.8 times less CPU. The wall time of one crate barely moves, since its two units compile one
after the other; a build of the workspace is throughput-bound, where CPU is what counts.
`cargo build -p slopty-agent` after the A build found `slopty-core`, `-grid`, `-proto` and
`-platform` fresh: `dev` and `test` shared their units while their settings matched, which is
why the spawned binaries are now built with `--profile test`.

**Test time, opt-level 1 against 0.** The suites of `slopty-ui`, `slopty-client`,
`slopty-worker`, `slopty-workerd` and `slopty-shape` (about 1 900 tests), run twice at each
level, nextest's `gate` profile. The sum over the tests of each one's faster run:

| Package | Opt-level 1 | Opt-level 0 |
| --- | --- | --- |
| `slopty-ui` | 139.9 s | 74.7 s |
| `slopty-worker` | 225.0 s | 131.0 s |
| `slopty-workerd` | 113.7 s | 85.5 s |
| `slopty-client` | 27.5 s | 21.8 s |
| `slopty-shape` | 1.1 s | 1.0 s |

The level-1 runs came later, under more load, so the table says only that opt-level 0 was not
slower; the load decides the rest. The test slowed most by 0 was
`screen::synthetic::tests::two_stripes_meet_at_the_seam_row_for_row` (3.8 s to 7.2 s). At
level 0 every timing-sensitive test named in `.config/nextest.toml` passed in both runs (the
session actor's echo, checkpoint and slow-viewer tests, the daemon's echo behind slow requests,
the relay's short wait), while one level-1 run failed the actor's checkpoint test.
The failures in both columns were other sessions' unfinished work in the UI and the ACP
adapter.

**What the next land should show.** In its tests shards' `timings-<shard>` artifacts: each
shard's `nextest build` and the sum of its `cargo-timing.html` unit times against the 1 613.7 s
and 4 830 s above (accept: at least 35 % less), nextest's JUnit `time` against 435.9 s (less
than 15 % more), and `slopty-workerd`'s and `slopty-cli`'s binaries as links of about a second
once unchanged. In `gh run view <id> --json jobs`: the `cargo xtask setup` step against its 3.2
min average, and the slowest job, which should be clippy-ios at about 19 min.

**What the lands showed** (wall minutes per job, from `gh run view <id> --json jobs`):

| Run | Cache | tools | clippy-host + rustdoc | clippy-ios | tests ui | tests worker | tests rest | push → promote |
|---|---|---|---|---|---|---|---|---|
| 36992664501 (4c330979) | cold, first run of the new profile | 4.6 | 16.2 | 20.8 | 25.2 | 27.8 | 30.4 | 30.6 |
| 37008187045 (6272fec2) | warm | 1.5 | 18.4 | 21.2 | 11.5 | 15.2 | 21.0 | 21.5 |

Before the change the tests lane alone took 27–48 min and bounded every run (39 min typical). A
warm run now promotes main in 21.5 min, bounded by clippy-ios and the `rest` shard together, so
the next cut is the clippy-ios lane's Linux clippy step and the `rest` shard's split.

```sh
# CI
gh run list --branch gate -L 22 --json databaseId,conclusion
gh run view <id> --json jobs
gh run download <id> -R aislopware/slopty -n timings-<shard>   # `timings` before the shards
# build CPU, in a target dir of its own: B is the profile in Cargo.toml; A adds
# `--config profile.test.opt-level=1` to each cargo command. Build everything once, then:
export CARGO_TARGET_DIR=target/v-probe RUSTC_WRAPPER= CARGO_INCREMENTAL=0
cargo clean -p slopty-agent
/usr/bin/time -l nice cargo nextest run -p slopty-agent --no-run
# test time: build the daemons the tests spawn, then run with them marked fresh
CARGO_TARGET_DIR=target/v-probe nice cargo build --profile test -p workspace-hack \
  -p slopty-ptyd -p slopty-workerd -p slopty-serverd -p slopty-cli -p slopty-testkit --examples \
  --bin slopty-ptyd --bin slopty-worker --bin slopty-server --bin slopty \
  --bin slopty-stub-claude --bin slopty-stub-pi
CARGO_TARGET_DIR=target/v-probe SLOPTY_BINS_FRESH=1 nice cargo nextest run -p workspace-hack \
  -p slopty-ui -p slopty-client -p slopty-worker -p slopty-workerd -p slopty-shape \
  --profile gate --no-fail-fast --status-level all
```

## 2026-10-02 — images in the history and block marks on the frame path

Release, mac-studio, other sessions building beside it (`nice -n 19`, 4 jobs). The engine now
lays out the placements above the screen when a frame is due to list them, keeps the placeholder
runs that scrolled up, and queues command-block news. One run, after both changes:

```sh
SLOPTY_BENCH_OUT=target/bench/laneT.jsonl \
  cargo test -p slopty-engine --release --lib frame_cost -- --ignored --nocapture --test-threads 1
```

| series | budget | measured |
| --- | --- | --- |
| `engine.frame_cost.take_frame.60x12` | 18 922 | 19 043 (+0.6 %) |
| `engine.frame_cost.take_frame.200x60` | 45 011 | 44 949 (−0.1 %) |
| `engine.frame_cost.take_frame_unchanged.60x12` | 901 | 908 (+0.8 %) |
| `engine.frame_cost.take_frame_unchanged.200x60` | 1 022 | 1 004 (−1.8 %) |
| `engine.frame_cost.write` | 795 | 825 (+3.8 %) |
| `engine.scroll_frame_cost.80x24` | 123 537 | 123 574 (0.0 %) |
| `engine.scroll_frame_cost.200x60` | 361 833 | 363 012 (+0.3 %) |
| `engine.history_image_frame_cost.scroll` | new | 100 890 (p50 6.6 µs) |
| `engine.history_image_frame_cost.graphics_changed` | new | 67 240 (p50 4.3 µs) |

- **A frame with nothing new is where the checks sit.** A first build tested the two new "is a
  list due" flags before the no-frame return and read 945 at 60×12 (+4.9 %). They are only
  needed once a frame is being built, and moved past the return it reads 908. The one check
  left before it, whether any block news waits, is a length read.
- **The write path is untouched.** Block news is queued only when an `OSC 133` mark lands.
  `write` read 850 and then 825 across two builds of the same write path; the entry above
  ("ghostty sync to 83edd491e") found it spreading 690 to 920 round to round.
- **The history's images.** `history_image_frame_cost` puts fifty one-cell images in the
  history of an 80×24 screen. A frame of plain output over them (`scroll`) lists nothing
  above, as before, since the clients move placements up themselves. A frame after the
  graphics storage changed (`graphics_changed`, one image sent again) lays out and lists all
  fifty above the screen, with no rows to diff. Both are new series, recorded as budgets in
  `xtask/budgets.toml` at these numbers.
- **The lists ride on their frame.** As first landed, the two lists went out as events of their
  own after the frame, and every whole frame carried a block list. CI (run 36998497615, a
  hosted Mac) then found 4 events waiting in the throttled viewer's sink, over the bound of 3:
  each resync the slow viewer was owed came as frame, marker and list. With the lists made
  fields of `Frame` (two bytes when empty), the same test, run 30 times here under other
  sessions' builds (`nice -n 19`, 4 jobs):

  ```sh
  for i in $(seq 1 30); do
    cargo test -q -p slopty-worker --test session_actor a_throttled_viewer -- --nocapture
  done
  ```

  | | runs | events queued at most | shown after the program ended |
  | --- | --- | --- | --- |
  | lists as fields of the frame | 30 / 30 pass | 1 (16 runs), 2 (14 runs) | 73–104 ms |

## 2026-10-02 — an encode that never returned

The clock test timed out on CI (run 36989451177, a hosted virtual Mac) at 180 s. After 36
pictures its wait printed "a frame being coded in VideoToolbox, waiting on it" 17 times, every
10 s: the held capture's lock stayed taken for 170 s. Only an encode takes it, across its calls
into VideoToolbox, so one call into the encoder never returned, and every encode after it, the
repair loop's included, waited behind it.

Mac Studio M1 Max, macOS 27.0.1, load average 8–10 from other sessions. The fix and its ruling
are in `docs/decisions/video.md` ("The worker gives up an encode that does not come back from
VideoToolbox").

**Reproduced and recovered.** `a_submit_that_never_comes_back_is_given_up_and_the_pictures_go_on`
streams the drawn screen through the real encoder and decoder, with the first session's 20th
submit waiting until the test lets it go. Before the fix nothing could take the stream past
that submit, since the lock it held is the one every encode takes. After it, over 6 runs in a
row and 3 more at once under `taskpolicy -b`:

| | pictures before | first picture after the stuck submit went in | after | sessions built | encodes given up on |
| --- | --- | --- | --- | --- | --- |
| 6 runs in a row | 18–20 | 2 038–2 108 ms | 100 | 2 | 1 |
| 3 at once, background QoS | 20 | 2 088–2 111 ms | 100 | 2 | 1 |

The 2 s is `ENCODE_STUCK`, charged by the beat. The rest is the build and the keyframe. The
clock test ran beside it every time, 9 runs, and gave no encode up (`0 encodes given up on` in
its `MEASURE` line), background QoS included.

**The path around the encoder, before and after.** `measure_the_encode_path_around_the_encoder`
takes a fresh capture through `on_frame` to its datagrams on the wire, 4 000 times, with a
session that hands a 900-byte frame on inside the submit, as an aligned VideoToolbox session
does. So it times everything an encode does except VideoToolbox. Release build, retired
instructions per capture (median), runs on one build each:

| | instructions | wall p50 / p95 / p99 |
| --- | --- | --- |
| before (held lock across the submit) | 11 815, 11 846, 11 852, 11 915 | 0.8 / 1.0 / 1.1–1.9 µs |
| after (the turn, locks let go before the submit) | 12 315, 12 340, 12 479, 12 490 | 0.8–0.9 / 1.0 / 1.1–2.0 µs |

About 500 instructions more a capture (+4 %): the turn's three uncontended locks, a retain and
release of the image and the sessions' references. The wall time does not move. A frame
spends 5–23 ms inside the encoder.

```sh
cargo test -p slopty-worker --release --lib measure_the_encode_path -- --ignored --nocapture
cargo test -p slopty-worker --lib -- screen::synthetic::tests::a_submit_that_never_comes_back \
  screen::synthetic::tests::the_clock_probes --nocapture
cargo test -p slopty-worker --lib screen::tests::an_encode_that_never_comes_back
```

## 2026-10-02 — the first key after an attach

The session actor built an attaching viewer's frame with `join_frame`, which leaves the record of
what the viewers following the diffs hold as it was, and the dirty state with it, so that viewers
already following do not lose their next diff. With nobody else following, that record was empty
or out of date, and the first key typed after an attach, reattach or catch-up was sent every row
in a full frame. A full frame never rides as an echo datagram. Now, when no viewer follows the
diffs, the whole frame is a baseline (`GhosttyEngine::baseline_frame`): it becomes what the next
diff is computed from. A viewer joining beside followers still gets `join_frame`.

The engine's screen is the one from "a scroll ships the rows it moved": `ls -l`-like output and a
marked zsh-style prompt on the bottom row, written while nobody watched. After the attach frame,
one typed key is written, and its frame is built and encoded (the encoded `TermEvent::Frame`, length
prefix included). "Before" is the attach as the actor made it until now (`join_frame`). Both
attach styles are measured in the same run. Release build, mac-studio M1 Max at load average 15–17
with other lanes building, median of 51 rounds, three runs:

```sh
cargo nextest run --release -p slopty-engine --test wire_bytes --no-capture \
  -E 'test(first_key)'
```

| first key after an attach | bytes | rows | full | build and encode |
| --- | --- | --- | --- | --- |
| 80 × 24 before | 11 134 B | 24 | yes | 51.1–52.8 µs |
| 80 × 24 after | **230 B** | 1 | no | **2.3–2.4 µs** |
| 200 × 60 before | 28 439 B | 60 | yes | 209.4–212.4 µs |
| 200 × 60 after | **232 B** | 1 | no | **3.9–4.0 µs** |

The key's frame now fits one datagram and also goes out as an echo copy, so a lost packet no
longer leaves it waiting for a retransmit. `only_the_diffs_that_answer_input_are_echoes`
(`crates/slopty-worker/tests/session_actor.rs`) checks this in the actor against a real shell.
It types Enter as the attach frame arrives and requires the frame that answers to be an echo and
not full. With the actor switched back to `join_frame`, it fails: that frame is full and not an
echo.

A resize with nobody following the diffs no longer builds a frame that nobody receives. The
viewers waiting are sent the baseline when they have room. The echo test attaches both at the
session's size and at a size the attach resizes it to.

## 2026-10-02 — the thread's frames on gpui-fast bb5991c (zed 23d10a47), and a two-state Mac

gpui-fast bb5991c imports zed 23d10a47, whose frame requests carry when the platform asked for
them (zed#64958). The thread's frame test (`frame_time::the_thread_draws_a_streaming_answer_within_a_frame`,
the scenarios of "hold the thread view to its latency budgets" above) ran four times on the same
tree, alternating the lock file before the sync (A, gpui-fast 6a4dd6f) and after it (B), with
load averages of 8 to 11 from other work on this Mac. Draw time p50 / p95 / p99 / max:

| Scenario | A1 | B1 | A2 | B2 |
| --- | --- | --- | --- | --- |
| (h) following | 3.4 / 7.0 / 8.2 / 9.0 ms | 2.5 / 5.5 / 7.3 / 9.3 ms | 3.4 / 6.5 / 8.9 / 13.9 ms | 3.1 / 5.8 / 12.5 / 16.6 ms |
| (i) panning, streaming | 3.4 / 7.4 / 9.8 / 10.3 ms | 2.8 / 6.2 / 7.7 / 8.1 ms | 3.3 / 7.0 / 10.3 / 12.9 ms | 3.1 / 6.3 / 8.1 / 8.6 ms |
| (j) every step open | 2.4 / 5.7 / 6.9 / 8.9 ms | 3.3 / 7.3 / 9.1 / 20.8 ms | 2.1 / 5.9 / 8.4 / 11.4 ms | 2.8 / 6.5 / 9.8 / 15.0 ms |
| (k) word to its frame p50 / p95 | 32.6 / 49.3 ms | 32.6 / 49.4 ms | 32.7 / 44.9 ms | 32.4 / 49.9 ms |

The sync moves nothing: A and B overlap in every scenario.

Every run sat about twice above that section's figures, and panning drew 257 to 283 frames in its
5 s with a p99 interval of 33 ms where that section had 300 and 17.5 ms. The commit that recorded
those figures (2adfd2af), measured twice more now, gave both states back to back: 3.3 / 5.8 /
8.1 ms with 271 and 255 panning frames, then 1.6 / 1.9 / 2.4 ms with 298 and 300, the second at
the higher load average (13). So the code since then is not the cause: this Mac draws in one of
two states, independent of the load average, most likely the GPU's or WindowServer's. A frame
comparison on this Mac is made A/B in alternation, as here, never against an older table.

```sh
# A: git show 08cbf9a4:Cargo.lock > Cargo.lock; B: the lock file at 767e0502
cargo xtask e2e smooth --filter 'test(/frame_time::the_thread/)'
```

## 2026-10-03 — the stream goldens and the late beats

`stream::a_drawn_window_streams_into_its_tile` failed in full `cargo xtask e2e app` runs and
passed alone. Mac Studio M1 Max, macOS 27.0, other lanes building beside it (load average 7–43).
Two separate things were behind it.

**The stats golden compared live figures.** The overlay's line ("60 fps · – to glass · 0.5
Mb/s · RTT 1.0 ms") had no accessibility node, so no mask found it, and one changed digit is
about 0.15 % of the frame. Every stream golden also still had the icons from before the
Hugeicons change, 1 698 px (0.338 %) that the 0.4 % tolerance let through. "59 fps" against the
golden's "60 fps" made 0.485 % and 0.589 %, alone or in the suite. The line is now a `Label`
node that the golden masks, the four stream goldens were retaken, and their tolerance is the
chrome's 0.2 %:

| golden | before | after (12 runs: 2 full suites, 10 of the stream tests) |
| --- | --- | --- |
| `stream-window` | 0.338 % | 0.000 % |
| `stream-window-stats` | 0.343–0.589 % | 0.000–0.033 % |
| `stream-window-popped` | 0.021 % | 0.000 % |
| `stream-display` | — | 0.000 % |

A run whose overlay said "59 fps" matched the "60 fps" golden at 0.000 %.

**The late heartbeat was measured wrong.** `beat_loop` logged a beat as late when the time
since the previous *beat* passed four promises (100 ms). On a moving picture no beat goes out
while video flows, so the time between two beats spans the video between them. The same runs
logged "late heartbeats" of 116 ms, 268 ms, 857 ms, 2.2 s and 4.0 s with 0 stalls on the client.
The beat (log, `ScreenStats::beat_gap`, `beat_gap_worst_us`) is now held to the silence it ends,
from the last datagram of any kind. Measured the same way, the longest silence any beat ended
was 26.9–35.1 ms in every run (the 25 ms promise plus the loop's tick), at load averages of
10–23, and no beat was late. On a quiet stream the two measures are the same.
`video_between_two_beats_is_not_a_late_beat` fails on the old measure.

**The one real stall did not come back.** One full run (before these changes) had a 66 ms
stall that the client charged to the link ("the link held datagrams the worker had already
sent"), 2 NACKs, ~450 ms into the stream, as the stripe timer's sessions at 2560 × 1600 finished
beside the first keyframe. The app suite runs its tests one at a time (`--no-capture`), so no
other test was beside it. A temporary probe polled the worker's datagram queue every 0.5 ms for
the first 3 s of each stream. Over 8 runs of the test (5 at load averages of 10–23) and a full
suite, the queue never held bytes for more than 11 ms (cwnd 19 712 B, the 16-packet floor), and
those runs and 2 more full suites had 0 stalls. This matches the 2026-09-29 note above (a 136 kB keyframe held 133 ms in QUIC on
a starved Mac), so it stays the transport's open question, not the screen path's. The test now
prints the worker's line before its verdict, so a stall's next appearance shows whether the
worker's own silence covers it.

**A synthetic 4:4:4 test waited on the clock alone.** CI run 37049458626 (a hosted virtual
Mac) failed `a_full_chroma_stream_arrives_as_444_and_follows_the_rate` with no 4:4:4 picture in
30 s after the switch back. Its waits (`next_picture`) never ran the geometry tick. When
VideoToolbox keeps an encode past `ENCODE_STUCK` ("an encode that never returned", above), the
beat gives the sessions up and only that tick builds new ones. The other synthetic tests already
waited through `next_or_stopped`, and the three that used `next_picture` now do too. With the
binaries run from `deps/` (no runner, so every VideoToolbox call was slow) and the screen tests
10 at a time, all three had failed with `None`. After the change they passed. The 4:4:4 test had
1 encode given up on, its replacement built at 4:4:4, and the switches took 196 and 166 ms. A
replacement is built at the chroma in force (`Pipeline::follow_lost`). Through the runner the
113 screen tests pass in 5.2 s.

```sh
cargo xtask e2e app                                   # twice; 55 passed each
cargo xtask e2e app --filter 'test(/^stream::/)'
cargo nextest run -p slopty-worker --lib -E 'test(/^screen::/)'
```

## 2026-10-03 — per-pixel edge fades on the thread's frames (gpui-fast decee29f)

The transcript, the palette, the project board, the settings page and the key bar now fade their
content per pixel (`gpui::edge_fade`, aislopware/gpui-fast#18) where they laid a gradient quad
over it in the surface's colour (`kit::edge_fade`). A primitive carries its fade in padding it
already had, so a fade adds no byte to the scene; the renderer reads one small table a frame. The
thread's frame test (`frame_time::the_thread_draws_a_streaming_answer_within_a_frame`, scenarios
(h) to (k) as above) scrolls a transcript that fades at its top and, while panning, its foot. It ran
four times, alternating the tree before the change (A: gpui-fast 448d3dac with the gradient
overlays) and after it (B), at load averages of 11 to 16 from other work on this Mac. Draw time
p50 / p95 / p99 / max:

| Scenario | A1 | B1 | A2 | B2 |
| --- | --- | --- | --- | --- |
| (h) following | 2.1 / 4.7 / 5.2 / 5.3 ms | 1.7 / 2.0 / 2.7 / 4.4 ms | 2.5 / 5.2 / 7.7 / 8.6 ms | 2.4 / 4.5 / 6.4 / 7.4 ms |
| (i) panning, streaming | 2.6 / 5.6 / 8.7 / 8.8 ms | 1.6 / 2.2 / 3.3 / 3.5 ms | 2.2 / 6.3 / 8.3 / 8.7 ms | 2.8 / 5.5 / 6.9 / 7.9 ms |
| (j) every step open | 2.5 / 5.5 / 6.4 / 7.4 ms | 1.3 / 2.1 / 2.6 / 4.9 ms | 2.5 / 5.1 / 8.5 / 11.1 ms | 2.1 / 4.7 / 7.5 / 8.2 ms |
| (k) word to its frame p50 / p95 | 35.1 / 47.3 ms | 37.1 / 51.7 ms | 32.6 / 49.7 ms | 32.8 / 49.7 ms |

No frame ran over 16.7 ms or was dropped in any run. B1 drew in this Mac's faster state (the
section above); A2 and B2, in the same state, overlap in every scenario. The fades cost nothing
measurable on the frame path.

```sh
# A: the tree at 9434a068 (Cargo.lock pinning gpui-fast 448d3dac); B: this change
cargo xtask e2e smooth --filter 'test(/frame_time::the_thread/)'
```

## 2026-10-03 — a typed claude's wiring

A `claude` typed in a Slopty shell first asks `slopty hook wire` for its words and environment
(`docs/decisions/claude-code.md`, "A `claude` typed in a Slopty shell is wired as one Slopty
starts"). That call is the whole cost the wiring adds to a launch. A release `slopty` on this Mac
(M1 Max), with a session, a server and the mod named in its environment and `HOME` an empty
directory, 100 calls in a row: 10.6 ms a call, against 2.4 ms for `/usr/bin/true` started the same
way, so about 8 ms of its own (p50 9.6 ms over 100 calls timed one by one). Claude Code itself
takes hundreds of milliseconds to start, so the wiring is not felt.

```sh
cargo build --release -p slopty-cli
export HOME=$(mktemp -d) SLOPTY_SESSION=$(uuidgen | tr A-Z a-z) SLOPTY_SERVER=127.0.0.1:1 \
  SLOPTY_CLAUDE_MOD=$HOME/mod SLOPTY_MOD_SOCKET=$HOME/mod.sock
time (for i in {1..100}; do
  target/release/slopty hook wire -- --model opus 'fix it' >/dev/null; done)
time (for i in {1..100}; do /usr/bin/true; done)
```

## 2026-10-03 — a download across a relink

A download whose link goes waits on its worker's line for the next link and fetches again over
it, naming the bytes it holds (`docs/decisions/transport.md`, "A download outlives its link").
From the moment the next link is on the line to the worker reading that `Fetch`, in the scripted
worker's test (the link closed 1.5 MB into a 4 MB file), three runs: 6.0, 7.3 and 4.7 ms (5.1 to
7.1 ms in three earlier runs). Most of it is the fsync of the partial, so its claim names only
bytes on disk; the rest is opening the control stream on the new link. The bytes already held
are not sent again.

```sh
cargo test -p slopty-client --test remote_link -- \
  a_download_cut_by_a_lost_link_goes_on_over_the_next_link --nocapture
```

## 2026-10-03 — an upload across a relink

An upload whose link goes waits on its worker's line for the next link, begins again there
under the same transfer, asks what the worker holds of the file it was sending and sends the rest
(`docs/decisions/transport.md`, "An upload outlives its link"). From the next link being up to
the worker's side accepting the stream of the rest, in the scripted worker's test (the link
closed 1.5 MB into a 4 MB file), three runs: 0.50, 3.5 and 0.62 ms: one `Begin`, one
`Resume` round trip and the stream's open. Nothing is synced on this side, unlike a download's
claim; the worker syncs the partial it answers for.

Against the real worker, the first link through a 4 MB/s shaper and cut 1.04 MB into an 8 MB
file without the worker being told, the upload finished 88 and 85 ms (two runs) after the next
link was up: the rest of the file over loopback, the earlier stream taken over, the landing and
the worker's `Finished`.

```sh
cargo test -p slopty-client --test remote_link -- \
  an_upload_cut_by_a_lost_link_goes_on_over_the_next_link --nocapture
cargo test -p slopty-workerd --test e2e -- \
  an_upload_goes_on_over_the_next_link_from_what_the_worker_holds --nocapture
```

## 2026-10-03 — the paste chord and the worker's caret on the input path

The paste goes as `ScreenInput::PasteChord` (`docs/decisions/input.md`, "A paste is the
layout's V, on both sides"): 10 bytes framed (golden `client_screen_paste_chord`:
`06 00 00 00 04 04 07 0f 2f 08`), against a `Key`'s 12. The view knows it by the event's own
`key_char`, so no work is added per key, and the worker holds it behind the clipboard offer as
it held the V position before.

The caret (`ScreenEvent::Field`, 26 bytes framed with a caret, golden `worker_screen_field`)
is read off the key path by construction. The stream's loop only sets a deadline when a command
can move the caret (a `const fn` match). The read starts once 60 ms (`FIELD_AFTER`) pass with
no such command, on a blocking thread, and never while one is still running. Each
accessibility call waits at most 100 ms (`FIELD_TIMEOUT_SECS`, set per element, since the
system-wide element's timeout is global). A burst of typing costs one read after it, and
nothing reaches the client unless the field changed. Not measured: the read's own time
against a real app. A test can only read the app in front of this Mac, and that is the
person's, so the bound above stands in for it.

```sh
cargo test -p slopty-proto --test golden -- screen_keyboard
cargo test -p slopty-workerd --lib -- \
  screens::fields::the_field_with_the_keyboard_is_told_after_typing_and_only_as_it_changes
```

## 2026-10-03 — the app's deploy into a fresh guest, the Linux e2e's new cases, a Linux lane in CI

On mac-studio (M1 Max, 10 cores, 32 GB) while other sessions built, every cargo command under
`nice -n 19` with four jobs.

**The app's deploy into a fresh macOS guest** (`docs/decisions/workers.md`, "The app's deploy
ran live into a fresh macOS guest"). Guest macOS 26.6.2, 4 cores, 8 GB, on the external disk.

| step | run 1 (fresh clone) | run 2 (kept clone, worker there) | run 3 (fresh clone) |
| --- | --- | --- | --- |
| clone, boot to a command in the session | 0.03 s, 31.2 s | — | 0.02 s, 36.3 s |
| first step stopped on the unknown host key | — | 59 ms | 103 ms |
| the key read again and fingerprinted (`Ssh::explain`) | — | 69 ms | 66 ms |
| the deploy once trusted: 76.8 MB up, install, doctor | 1.62 s | 1.82 s | 1.61 s |
| `slopty ping` ×3 in the guest, its own worker over QUIC | — | — | rtt 1.98, 0.59, 0.69 ms |

Run 1 failed at the dial from this Mac (it had no guest-side ping yet), which found the Local
Network denial of this session's process tree (`EHOSTUNREACH` on every UDP send to the guest,
system binaries excepted); runs 2 and 3 stop there by design until that grant is given.

```sh
cargo xtask vm deploy --app
cargo xtask vm deploy --app --keep     # leaves the guest; `cargo xtask vm prune --kept`
```

**The Linux e2e's new cases** (`docs/decisions/platform.md`, "A Linux worker's files, search,
uploads and tunnels"). Debian trixie, aarch64, in Docker Desktop capped at two CPUs; the client
on this Mac over the published loopback port. All five cases passed in 3.3 s of tests.

| what | number |
| --- | --- |
| a file written by `docker exec` there, to its `WorkerMsg::File` here | 47.7 ms (an upper bound: most of it is `docker exec` starting) |
| an upload of 2.5 MB into the shell's directory, to its `Done` | 62.4 ms |
| 300 keys echoed by `cat` | p50 0.66 ms, p90 1.12 ms, p99 3.01 ms (QUIC rtt 0.43 ms) |
| `cargo xtask linux e2e`, warm, from cross-build to the end | 63.1 s cross-build, 112.8 s the e2e step |

```sh
cargo xtask linux e2e > target/logs/linux-e2e.log 2>&1
```

**A Linux lane in CI** (`docs/decisions/tooling.md`, "The Linux worker is built and tested on a
Linux runner"). The repository's Actions cache on 2026-10-03: 10.78 GB in 8 309 entries, against
a 10 GB quota with least-recently-used eviction (`gh api repos/aislopware/slopty/actions/cache/usage`).
So the Linux job compiles cold, with no cache entry of its own; its wall time is to be read from
its first runs (`gh run view <id> --json jobs`) before it joins what `promote` needs.

**A release built here** (`cargo xtask dist`, ad hoc signed, cold for the dist profile). Every
artifact the release job uploads came out and matched its `SHA256SUMS`.

| step | time |
| --- | --- |
| `cargo build` (dist), this Mac | 937.1 s |
| cross-build the Linux workers (glibc 2.28), arm64 and x86_64 | 563.9 s |
| cross-build the Linux servers (static musl), arm64 and x86_64 | 445.5 s |
| zip the app; the dSYMs archived | 8.9 s; 14.7 s |

| artifact | size |
| --- | --- |
| `Slopty-0.1.0-macos-arm64.zip` | 87 MB |
| `slopty-0.1.0-macos-arm64.tar.gz` (worker, ptyd, server, CLI) | 22 MB |
| `slopty-worker-0.1.0-linux-{arm64,x86_64}.tar.gz` | 15 MB, 16 MB |
| `slopty-server-0.1.0-linux-{arm64,x86_64}.tar.gz` | 5.1 MB, 5.4 MB |
| `slopty-0.1.0-dSYMs.tar.gz` | 177 MB |

```sh
cargo xtask dist --ad-hoc --out target/dist-out > target/logs/dist.log 2>&1
(cd target/dist-out && shasum -a 256 -c SHA256SUMS)
```

## 2026-10-03 — a zoomed picture's region

On mac-studio (M1 Max) while other sessions built (load 5–18), from a copy of a `dev`-profile
test binary off the repository volume. The display is drawn, never captured. HEVC at 60 asked,
the encoder's ceiling 30 Mbit/s, loopback, no loss. Two alternated rounds of 15 s each; the
two figures in a cell are the two rounds (`docs/decisions/video.md`, "A zoomed picture's region
at native resolution").

**Before the build: a 5K display at native against the regions a phone streams of it.** A
phone at one to one shows 2080 × 1170 device pixels of the display in landscape and 1170 × 658
in portrait. With a quarter of its view on every side, the region is 3120 × 1756 or
1756 × 988. Each region is drawn as a display of its own size with the 5K display's glyphs, so
the encoder sees the same density.

| stream | encode p50 / p95 | frames a second | wire Mbit/s (KiB a frame) | arrival → decoded p50 | capture → painted p50 | input sent → present p50 |
| --- | --- | --- | --- | --- | --- | --- |
| 5K whole, 5120 × 2880 | 35.1 / 36.2, 35.1 / 35.3 ms | 27.9 | 1.62 (7.1) | 3.32 ms | 65.5, 55.2 ms | 82.9, 83.5 ms |
| 5K in two stripes | 19.7 / 20.0, 19.8 / 20.0 ms | 48.3, 48.6 | 3.54 (8.9), 3.57 (9.0) | 1.35, 1.40 ms | 40.2, 51.6 ms | 55.4, 57.1 ms |
| landscape region, 3120 × 1756 | 14.2 / 14.4, 14.3 / 14.5 ms | 59.5, 58.6 | 1.73 (3.6), 1.74 (3.6) | 1.69, 1.72 ms | 31.9, 30.8 ms | 40.7, 39.6 ms |
| portrait region, 1756 × 988 | 5.84 / 5.99, 5.84 / 6.02 ms | 60.0, 59.8 | 1.03 (2.1), 1.01 (2.1) | 1.05 ms | 22.9, 22.6 ms | 33.2, 32.1 ms |

The whole 5K display cannot be encoded at 60: one picture holds 28 a second, and two stripes
hold 48. The portrait region encodes in a sixth of the whole display's time and holds the full
60, with the picture painted about 40 ms sooner. The landscape region is 2.5 times quicker to
encode and holds the full 60. The bits a frame follow the area, but bits a second do not
fall as much, because the regions are sent twice as often.

```sh
cargo test --profile dev -p slopty-worker --lib --no-run
cp target/debug/deps/slopty_worker-<hash> /tmp/slopty-region/slopty_worker
(cd /tmp/slopty-region && SLOPTY_GLASS_SECONDS=15 SLOPTY_GLASS_ROUNDS=2 TMPDIR=/tmp nice \
  ./slopty_worker --ignored --exact \
  screen::synthetic::tests::a_region_against_the_whole_5k_display --nocapture) \
  > target/logs/region-before.log 2>&1
```

**Built: from the ask for a region to the client's first decoded picture of it.** The same
drawn 5K display at native, loopback, with a 3120 × 1756 region. `ask` is `set_quality`, with
the sessions it builds waited for and put in, as the stream's loop does. Wire KiB is what the
worker sent between the ask and that picture. Two runs, of 8 and 12 rounds.

| change | ask → first picture p50 / p95 | wire KiB in between, p50 |
| --- | --- | --- |
| the whole display → a region (new sessions, a keyframe) | 119.5 / 176.8, 120.1 / 143.2 ms | 212, 241 |
| the region moved at its size (no new session) | 36.4 / 58.1, 41.1 / 56.7 ms | 295, 294 |
| the region → the whole display (new sessions, a keyframe) | 164.9 / 232.3, 148.2 / 183.9 ms | 421, 416 |

A move at the region's size costs about two frames and no session. The move here is 600 × 338
pixels, past what the encoder's motion search reaches, so its first frame is close to a
keyframe in bytes. No capture was dropped as "between two regions" (`between_regions` 0): the
drawn capture takes a configuration before its next beat. That a real ScreenCaptureKit stream
delivers a frame promptly after a same-size `sourceRect` move on a still screen is a check
still owed on hardware.

```sh
(cd /tmp/slopty-region && SLOPTY_REGION_ROUNDS=8 TMPDIR=/tmp nice ./slopty_worker --ignored \
  --exact screen::synthetic::tests::measure_a_region_change --nocapture) \
  > target/logs/region-change.log 2>&1
```

## 2026-10-03 — a Linux socket's receive buffer

`keyframe_report` (`crates/slopty-net/src/endpoint.rs`) on Linux: 1.5 MB of 1232-byte datagrams
over loopback to a socket nobody reads. Docker Desktop's VM (arm64, kernel 7.0.14-linuxkit,
`net.core.rmem_max` 4 MiB) in an Ubuntu 24.04 container; `SO_RCVBUF` is what the socket was set
to, half what Linux reports. Ubuntu's own default cap is 212 992 B, which the first row stands
for (the socket's default).

| asked | as | `SO_RCVBUF` | held |
| --- | --- | --- | --- |
| nothing (`SLOPTY_UDP_RCVBUF=0`) | a user | 106 496 B | 92 of 1 217 |
| 16 MiB | a user | 4 MiB (the cap) | 1 217 of 1 217 |
| 16 MiB | root, no `CAP_NET_ADMIN` (Docker's default) | 4 MiB (the cap) | 1 217 of 1 217 |
| 16 MiB | root with `CAP_NET_ADMIN` | 16 MiB (forced) | 1 217 of 1 217 |

```sh
SLOPTY_UDP_RCVBUF=16777216 target/debug/deps/slopty_net-<hash> --exact \
  endpoint::tests::keyframe_report --ignored --nocapture
docker run --rm --cap-add NET_ADMIN -u root -e SLOPTY_UDP_RCVBUF=16777216 … (the same)
```

## 2026-10-03 — the navigator by project

The navigator groups by project (`docs/decisions/ui.md`, "The navigator groups by project; the
machine is a facet"): every tile's facts are assembled and grouped once a frame
(`grouping::FrameProjects`, shared by the navigator, its rail, the breadcrumb and every
workspace's name). `measure_the_navigator_over_many_tiles` (one worker, 60 shells each in a
folder of its own and 60 notes, 400 frames) and `measure_the_navigator_scrolling` (120 notes,
600 frames), three runs each, on this Mac under other lanes' builds.

The workload means more now: each of the 60 shells is a project of its own, so the navigator
lists 60 project headers, 60 rows and a *Workers* block of 60 notes (181 rows where it had 121),
the rail draws 60 project glyphs where it drew one worker's, and the snapshot that brings them
lands in 61 workspaces (a project per workspace, the notes with the first). The test now hides
the navigator through its action and checks it by the layout's state, since a snapshot's tiles
leave nothing focused for ⌘B to reach the workspace through and a release build keeps no debug
selectors; before, it panicked at that step.

| build | docked p50 / p95 | hidden p50 / p95 | scrolling p50 / p95 |
| --- | --- | --- | --- |
| release, load ~19 | 0.912 / 1.383, 0.919 / 1.305, 0.883 / 1.296 ms | 0.563 / 0.829, 0.581 / 0.848, 0.559 / 0.840 ms | 0.344 / 0.555, 0.342 / 0.501, 0.344 / 0.515 ms |
| dev, load ~7–9 | 4.119 / 4.613, 3.989 / 4.476, 4.066 / 4.431 ms | 2.878 / 3.174, 2.647 / 2.973, 2.752 / 3.009 ms | 1.269 / 1.544, 1.261 / 1.455, 1.279 / 1.465 ms |

What the navigator adds at p50 (docked less hidden) is 0.34 ms in release (0.43 to 0.54 ms
across the four pins measured on 2026-09-30) and 1.3 ms in dev (about 1.2 ms on 2026-09-26):
the bar holds. The
hidden frame grew, 0.33 → 0.57 ms in release: it now groups every tile for the rail and draws a
glyph per project. In dev, one grouping of the 120 tiles takes 0.6 ms (0.3 ms the facts, 0.25 ms
the grouping); with the grouping stubbed out the same frames take 1.41 ms hidden and 1.66 ms
docked, so the grouping and the rail are what the hidden frame added. Two fixes came out of the
measuring, which together took the hidden frame from 4.0 to 2.7 ms in dev: the grouping is
worked out once a frame where five callers each worked it out, and `reading_order` no longer
sorts every tile by a linear position lookup (277 µs → 12 µs for 120 tiles), since the layout
lists its tiles in reading order already. Scrolling the list
groups once a frame too, since the rows region builds every row's words each frame as before;
its p50 is the 2026-09-30 figure (0.341 ms) rather than the 0.229 ms of a quieter run, at a load
of 19, so it is noted and not claimed.

```sh
cargo test -p slopty-ui --release --lib --no-run
target/release/deps/slopty_ui-<hash> --ignored --nocapture --test-threads 1 measure_the_navigator
cargo test -p slopty-ui --lib measure_the_navigator -- --ignored --nocapture
```

Logs: `target/logs/laneO-navmeasure-release.log`, `target/logs/laneO-navmeasure.log`.

**With boards and threads with no tile (same day, later).** The navigator now also heads each
project with its board's row and lists the threads at work that have no tile here under their
project, and every attention look names each tile's project, so a notification of one project
groups with the others. `measure_the_navigator_over_many_tiles` adds a second phase on top of
the same 120 tiles: 40 threads with no tile, in 40 folders of their own, and 5 boards with
members, then times 400 docked frames and the attention look (`WorkspaceView::attention_look`,
which now carries the projects map). Release, three runs, load ~7–8:

| | docked p50 / p95 | hidden p50 / p95 | scrolling p50 / p95 |
| --- | --- | --- | --- |
| 120 tiles | 0.962 / 1.465, 0.939 / 1.374, 0.900 / 1.388 ms | 0.586 / 0.867, 0.594 / 0.850, 0.580 / 0.887 ms | 0.354 / 0.548, 0.341 / 0.485, 0.327 / 0.430 ms |
| + 40 threads, 5 boards | 0.977 / 1.401, 0.921 / 1.303, 0.885 / 1.422 ms | | |

| attention look p50 / p95 |
| --- |
| 0.210 / 0.259, 0.199 / 0.213, 0.209 / 0.261 ms |

The 40 thread rows and 5 boards add nothing measurable to a docked frame (0.93 against
0.93 ms p50, run to run inside the noise), since the claims are worked out once per listing and
idle or tiled threads are skipped before any grouping. The figures without them match the
morning's within noise. The app takes the attention look each time the workspace view notifies
(`slopty-app`'s observer of the view), not on each drawn frame; with the projects map in it,
it stays at a fifth of a millisecond.

Log: `target/logs/laneO-measure.log`.

## 2026-10-03 — a folder tile's contents stream start

This Mac, macOS 27.0.1 (26A434), debug builds, other lanes building. The decision is in
`docs/decisions/workspace.md`, "A folder made again is reported at once, whatever its stream
costs to start".

| what | macOS 27.0.1, this Mac | macOS 26.6.2, CI runner |
| --- | --- | --- |
| `FSEventStreamStart`, asked → up, 8 runs of the unit test | 0.31 s seven times, 0.80 s once | — |
| the same, throwaway probes under heavier load (3 runs each of 6 flag and `sinceWhen` sets, over 4 folders) | 0.6 to 2.7 s | — |
| `FSEventsGetCurrentEventId`, alone / while a start is in flight | 0.08 to 0.6 ms / 0.27 to 0.6 s | — |
| `FSEventStreamStop` while a start is in flight | 0.04 to 0.3 ms | — |
| `a_folder_deleted_and_made_again_is_followed`, whole test | — | 0.134 s (gate run 37089756772, junit) |
| folder made again → its report, before | 0.6 to 0.9 s, or past 3 s (1 run in 5 failed) | — |
| folder made again → its report, after, 8 runs | 51.7 to 52.5 ms (`HOLD` after "gone") | — |

```sh
cargo test -p slopty-worker --lib fsevents::tests::a_stream_says -- --nocapture
cargo test -p slopty-worker --test fswatch a_folder_deleted -- --nocapture
```

## 2026-10-04 — the terminal seat and the server link after a lid close

This Mac, debug builds, other lanes building. A driver whose Mac closed its lid holds a
session's seat until its link times out; a server link dead after a sleep held the app until the
same timeout. Decisions: `docs/decisions/terminal.md` (the seat) and
`docs/decisions/workers.md`, "A wake dials a held machine, and a resume probes the server link".

| what | before | after (3 runs) |
| --- | --- | --- |
| second viewer's first key → its `Driver { you: true }`, the driver silent | 45 s (`IDLE_TIMEOUT`) | 3.002 / 3.003 / 3.002 s |
| resume with the server path muted → the link given up | 45 s | 1.002 / 1.002 / 1.003 s |

The input path gains one `Option` check per request for the marker the driver is asked to answer.

```sh
cargo test -p slopty-worker --test session_actor -- silent_driver --nocapture
cargo test -p slopty-client --test server_link -- resume --nocapture
```

## 2026-10-04 — the stripe timing beside a new stream

The 66 ms stall about 450 ms into a new stream ("the stream goldens and the late beats", above)
was left as the transport's. One thing beside it was the worker's own: a stream's open asked
`stripes::pays` for its size, and for a size not yet timed (from 3024 × 1968 × 2/3 pixels up,
so 2560 × 1600 too) that started `VideoEncoder::side_by_side` at once. That is 13 frames on a
whole-picture session and 13 on two stripe sessions of its own, coded beside the stream's first
keyframe and first frames. Now the timing starts only from the geometry tick, once no session of
the worker has coded anything for a second (`engines::Engines::quiet`), and the open takes a
size's verdict only when it is already known.

Measured on the M1 Max (macOS 27.0, load average 5–6), release, the drawn 2560 × 1600 display
at its own size, each run in a fresh process, the two alternated 8 times. "Before" runs the
timing at the open as the open did (`SLOPTY_TIME_AT_OPEN=1`); "after" is the stream alone, as it
now opens. The longest gap between two frames' first datagrams leaving, over the stream's first
2 s:

| | longest gap, median (range) | when, after the open began | frames in 2 s |
| --- | --- | --- | --- |
| before | 48.6 ms (33.4–54.5) | 42–55 ms at 491–543 ms in 6 of 8 | 108–112 |
| after | 36.0 ms (33.4–36.1) | the 33–36 ms gaps every run has | 109–111 |

The timing beside the stream took 670–710 ms, against about 310 ms of encodes for its 26
frames at its own medians, and timed the size as whole 10.8–11.6 ms and striped 12.5–12.7 ms,
so it never paid. The extra gap falls at
the point the stall was seen. On a loaded Mac, with the first keyframe still in QUIC, a 50 ms
hole in the worker's sending is enough to read as a link stall at the client. The 33 ms gaps
left in both are the first frames at 30 a second while the encoder warms up and the encoder
watch's window boundaries (36 ms at 1.06 s and 2.06 s); they are not the timing's.

```sh
cargo test --release -p slopty-worker --lib --no-run
# the binary it names, each run on its own:
SLOPTY_WIDE=1 SLOPTY_TIME_AT_OPEN=1 target/release/deps/slopty_worker-<hash> --ignored --exact \
  --nocapture screen::synthetic::tests::measure_a_new_streams_first_seconds   # before
SLOPTY_WIDE=1 target/release/deps/slopty_worker-<hash> --ignored --exact \
  --nocapture screen::synthetic::tests::measure_a_new_streams_first_seconds   # after
cargo test -p slopty-worker --lib -- engines:: stripes:: a_new_stream_codes_its_first_frames
```

**Through the real path.** The same question end to end: a fresh `slopty-worker` (release) on
the drawn screen with its own `slopty-ptyd`, and `slopty bench screen` streaming the drawn
editor window at its own 2560 × 1600 over QUIC on loopback, decoding with VideoToolbox and
painting through the app's pacer on a 60 Hz timer. "Before" sets `SLOPTY_TIME_AT_OPEN=1` on the
worker, which times the stripes at the open as the open did (the worker's log says "stripes
timed … whole 11.0–11.7 ms striped 12.5–12.9 ms pays=false" each time); "after" is the worker as
it is. Each run has new processes, the two are alternated, and every daemon is killed at the run's
end. The bench now also says when each arrival gap over 40 ms ended.

| batch (load average) | runs | longest arrival gap, before | after |
| --- | --- | --- | --- |
| 1 (14–20), 5 s | 8 + 8 | median 60.1 ms (48.0–68.2) | median 36.4 ms (33.3–52.8, one run 156.7) |
| 2 (33–50), 6 s, in the first second | 10 + 10 | over 40 ms in 8 runs (40–63, 400–950 ms in) | over 40 ms in 5 runs (41–61) |
| 3 (41–52), 6 s, workers' own figures | 6 + 6 | 34–54 ms | 35–65 ms |

- The client's stall counter read 0 (0 ms stalled) in all 48 runs, before and after. The 66 ms
  stall did not come back on the old schedule either, so the old one adds a hole, not a stall
  by itself. No datagram was lost, no frame refreshed, and nothing failed to decode. QUIC lost
  0 packets. Ten runs had 1–9 NACKs for datagrams that came late under load.
- At moderate load the old schedule's gap is the longest of the run, 400–600 ms in. With
  the timing waiting for idle engines it is gone, and the longest gap is about two frame
  periods.
- At load averages of 40–52 both schedules have 40–65 ms gaps anywhere in the run. The
  worker's own figures put them in the encoder's turns, not on the wire. Capture to callback
  stays under 3.4 ms (8.5 once), encode p95 rises from 11 to 13–16 ms, and 2–16 captures a run are
  superseded before encoding. That is this Mac running other lanes' builds, and nothing the
  link or the client does.
- The one 156.7 ms gap (batch 1, after) came with a 160 ms capture-to-decoded figure for the
  same frame. It did not recur in the 16 runs after it.

```sh
cargo build --release -p slopty-workerd --bin slopty-worker -p slopty-cli --bin slopty -p slopty-ptyd
# per run, in a fresh directory $root:
target/release/slopty-ptyd --socket $root/ptyd.sock --shell-dir $root/shell &
SLOPTY_SYNTHETIC_SCREEN=1 [SLOPTY_TIME_AT_OPEN=1] target/release/slopty-worker \
  --ptyd-socket $root/ptyd.sock --ctl-socket $root/worker.sock --data-dir $root/worker \
  --drop-dir $root/drops --print-addr --port 0 > $root/addr &
SLOPTY_WORKER_SOCKET=$root/worker.sock target/release/slopty bench screen --data-dir $root/cli \
  --worker 127.0.0.1:<port from $root/addr> --window 7001 --scale 1 --seconds 6
# then kill the worker and ptyd
```

## 2026-10-04 — what rested Codex threads keep, followed and let go

Two `codex app-server` daemons (codex-cli 0.156.1, each with its own signed-out `CODEX_HOME`)
ran side by side on this Mac. Each had Slopty's MCP relay (`slopty mcp`) as the one MCP server
every thread loads, and `thread_unload_delay_secs = 10` so the run is short (Codex's own default
is longer). The worker started 10 threads on each, with no turn, so no model was asked. On one
side the worker kept every thread followed, as it did before. On the other it let a thread go
(`thread/unsubscribe`) after 3 s at rest. Counts are `pgrep -f` of each side's relays and `ps
-o rss=`, every 5 s for 45 s.

| | MCP relays | relays' RSS | daemon RSS |
|---|---|---|---|
| followed, 5–46 s | 10 | 290–292 MiB | 142 MiB |
| let go, 5–10 s (before Codex's unload delay) | 10 | 289 MiB | 141 MiB |
| let go, 15–46 s | 0 | 0 | 142 MiB |

- Each loaded thread kept one relay of about 29 MiB, for as long as anything stayed subscribed.
  Let go, a thread's relay was gone within Codex's unload delay, and the 10 never came back.
- The daemon's own resident size did not fall: its allocator keeps what it freed. What
  letting go saves is the MCP servers' processes, one per server per thread, and for the
  person's own MCP servers the same again for each.
- In the shipped worker a thread is let go after 10 minutes at rest, with nobody following it
  and no TUI on it ([`REST`](../crates/slopty-worker/src/thread/codex.rs)). Codex unloads it after
  its own delay.
- The same run checks that Codex takes the per-thread `config` Slopty now sends
  (`shell_environment_policy.set.SLOPTY_*`): every thread started with it.

```sh
cargo nextest run -p slopty-worker --test codex_rest --run-ignored only --no-capture
# or, with the binaries already built:
cargo test -p slopty-worker --test codex_rest -- --ignored --nocapture
```

## 2026-10-04 — Prompt search

The worker's search of the person's past prompts (`slopty_worker::thread::history`) over this
Mac's own records: Claude Code's prompt history (4.2 MB, 13,936 lines, with its paste cache),
Codex's (0.2 MB) and the first lines of its 143 rollouts, and pi's 78 session files (18 MB). A
fresh index for each query reads every record (cold); five more searches on the same index read
only what was appended (warm). Release build, Apple silicon, the files in the page cache. Only
times and counts are printed; no prompt is.

| query | cold | warm median | warm max | sessions |
|---|---|---|---|---|
| none (prompted last first) | 150–192 ms | 3.4 ms | 4.7 ms | 50 (the limit) |
| `the` | 82–107 ms | 19.3 ms | 20.2 ms | 50 |
| `login test` | 85–90 ms | 4.8 ms | 5.5 ms | 14 |
| `zzqxj` (no match) | 87–107 ms | 2.5 ms | 2.6 ms | 0 |

- The cold read is far inside the search's bounds (3 s, 512 MiB, 64 MiB a file), so nothing was
  cut. It happens once per worker; every later search reads only new lines.
- Scoring every prompt with `nucleo-matcher` took 45 ms a warm search whatever the words. Each
  prompt now keeps its text folded as the matcher reads it (by grapheme, accents folded, lower
  case), and a prompt that lacks one of the words is passed over before scoring. A rare word
  went from 45 ms to 3–5 ms. A word in nearly every prompt (`the`) still scores most of them,
  at 19 ms.

```sh
cargo test --release -p slopty-worker --test history -- --ignored --nocapture measure
```

## 2026-10-04 — companions on the step clock

The headless workspace (`slopty-ui` tests, the GPUI test platform) with eight agents on one
worker, each in its own tile: three working, one waiting on the person (a question), one done
and three idle, then all eight idle. The person counts as present (the yard is awake). Each
arm warms up for two seconds, then counts the workspace's renders over five simulated seconds
of 120 ticks, as the working mark's entry above does, and the wall time those five seconds
took. The test build is unoptimised (`profile.test`, opt-level 0) and ran under `nice -n 19`
beside other builds on an M1 Max, so the times are an upper bound and wander by about 3 ms
between runs; the frame counts do not wander.

```sh
CARGO_BUILD_JOBS=4 nice -n 19 cargo nextest run -p slopty-ui --lib \
  -E 'test(companions_cost)' --run-ignored only --no-capture
```

| `[theme] companions` | crowd | frames a second | time drawing a second (3 runs) |
| --- | --- | --- | --- |
| off | mixed | 12 | 10.7, 10.0, 12.8 ms |
| quiet | mixed | 12 | 12.2, 11.6, 13.9 ms |
| lively | mixed | 12 | 12.7, 12.2, 13.8 ms |
| off | at rest | 0 | 53, 50, 60 µs |
| quiet | at rest | 0 | 50, 52, 50 µs |
| lively | at rest | 0 | 53, 50, 54 µs |

- Companions draw no frame of their own: a crowd at work draws the working mark's twelve a
  second with them off, quiet or lively, and nothing at rest. The lively yard's play rides
  only frames a working companion already draws.
- A frame with companions costs about 1–2 ms more per second (0.1 ms a frame, unoptimised)
  than one with the marks: the 16 × 16 sprites are up to 96 quads each. At rest all three cost
  the same, since nothing is drawn.
- The first probe found the needs-you wave at 15 renders a second beside a working
  companion, not 12. Each wave beat fell on the clock's step grid, but its own timer and the
  clock's notified the view separately, and the test platform builds a dirty window at every
  effect flush. The wave's clock now leaves a view to the spin clock when it will wake it at
  that step anyway (`icons::steps_wake`), and the beats ride the mark's frames: 12.
- The lively one-shots that do draw at rest are bounded: a finished turn's hop is six steps
  (the second "at rest" second of `companions_draw_no_frame_the_working_mark_does_not` reads
  7 lively, 1 off or quiet), a needs-you wave is six beats over two seconds.
- So lively stays the default (`docs/decisions/brand.md`, "Companions"). Not measured yet: the
  app's own process CPU idle with a lively yard on a real display, release build.

## 2026-10-04 — a virtual Mac's encoder and its clients

The worker shard's hangs on CI (`docs/decisions/video.md`, "A virtual Mac's encoder stops for
good past its 1020th client"), reproduced in macOS 26.6.2 guests (25G83, tart 2.40.1 on the
M1 Max, macOS 27.0), 4 cores and 8 GB or 3 cores and 7 GB. The count is the guest kernel's
`AppleVideoToolboxParavirtualizationUserClient` objects; a fresh guest boots with 2.

```sh
# in the guest: the clients, then one process that codes and ends cleanly, then the clients
ioreg -l -w0 -r -c AppleVideoToolboxParavirtualizationDriver \
  | grep -c 'o AppleVideoToolboxParavirtualizationUserClient'
cargo-nextest nextest run --archive-file vt.tar.zst --workspace-remap ws \
  -E 'package(slopty-codec) & test(=tests::hevc_encode_then_decode)'
```

| what one process does before it ends | clients it leaves |
| --- | --- |
| one HEVC session: 2 frames, `CompleteFrames`, `Invalidate`, release | 2 |
| 10 such sessions one after another | 2 |
| 50 such sessions one after another | 2 |
| one decoder session, or 20 one after another | 2 |
| `hevc_encode_then_decode` (`slopty-codec`), three runs | 2 each |
| a session left alive at exit, or the process killed mid-encode | 2 |

None of them is ever given back before the guest restarts. A loop of processes that each code
two frames at 1280 × 800 and end cleanly, counting every 50:

| guest | other guest | clients at the start | the loop's run that failed | clients then |
| --- | --- | --- | --- | --- |
| 4 cores, 8 GB | running the same loop | 32 | 496th | 1020 |
| 3 cores, 7 GB | running the same loop | 2 | 510th | 1020 |
| 4 cores, 8 GB, fresh | idle, 8–14 clients | 2 | 509th | 1020 |

Past it no session codes: frames come back `kVTVideoEncoderNotAvailableNowErr`
(-12915), and the system log reads "VTVideoEncoderSelection signalled err=-12908",
"(VCPRealtimeEncoder) PT: No real codec!!" and, as processes end, "stalling for detach from
AppleVideoToolboxParavirtualizationDriver". The idle guest beside the third went on coding,
and the host's own VideoToolbox coded throughout. Thirty-two 2560 × 1600 sessions alive in one
process on a fresh guest got -12915 or -12912 for 12 of 96 frames, and the next process coded
again: busy, not stopped.

What the tests leave: one run of the `slopty-codec`, `slopty-capture` and `slopty-worker`
library tests under nextest adds 60 clients (10 runs: 2 → 602); the whole worker shard added
61 (4 → 65). So one shard on a fresh guest is a sixteenth of the way.

The worker on a stopped encoder. The test
`sessions_lost_one_after_another_are_replaced_ever_more_slowly` runs a stream at scale 0.25 for
3 s on a real session that answers every frame -12915:

| | sessions built in 3 s |
| --- | --- |
| rebuilt at once on every loss (before) | 181 |
| 250 ms after a second loss in a row, doubling (`LostRetry`) | 5 |

A stopped run of the old worker in a guest logged "the geometry tick builds new sessions"
13 301 times.

The fix under load. On the M1 Max (other lanes' builds and the guest's loop below running beside
it), the worker library's VideoToolbox tests in one process, the commit before and the fix
alternated 10 times: 15 of 15 and 16 of 16 passed every time, 4.4–5.8 s a run. In a fresh 4-core
guest with two `yes` processes beside it, the three packages' library tests under nextest (three
at a time, VideoToolbox two at a time) 10 times: every VideoToolbox test passed in every run, no
test process lived past 100 s, 26–43 s a run. The failures there were the guest's own every run
(`this_mac_says_whether_it_wakes_on_lan`,
`this_mac_reports_its_toolchains_and_what_its_person_said`, and twice
`player_starts_and_drains`, which CI skips for want of audio hardware).

```sh
CARGO_BUILD_JOBS=4 nice -n 19 cargo nextest archive -p slopty-codec -p slopty-capture \
  -p slopty-worker --archive-file vt.tar.zst
# in the guest, 10 times:
cargo-nextest nextest run --archive-file vt.tar.zst --workspace-remap ws --test-threads 3 \
  --no-fail-fast \
  -E 'package(slopty-codec) | package(slopty-capture) | (package(slopty-worker) & kind(lib))'
# on the host, 10 times each:
target/debug/deps/slopty_worker-<hash> screen::synthetic:: screen::tests::a_rebuild_beside
```

## 2026-10-04 — an echo beside a followed thread

The 2026-09-27 measurement again, on the thread path that replaced the conversation stream
(`echo_beside_a_followed_thread`, apps/slopty-worker e2e, release, loopback, mac-studio (M1 Max),
load average 9–12 from other lanes' builds, tree on 88d9f458). The same rig as before: 200 keys an
arm into `/bin/cat`, three arms alternating for five rounds (quiet; an agent whose transcript grows
by a 2 KB answer every 5 ms with a hook every 50 ms, nobody following; the same, its thread
followed on the same connection). Each follow starts with no `have`, so the server sends the
thread's last turn first: 340–371 actions per round.

| arm | p50 / p90 / p99 / max, 1 000 keys |
| --- | --- |
| quiet | 0.43 / 1.10 / 5.63 / 16.62 ms |
| busy, not followed | 0.58 / 1.67 / 7.22 / 13.96 ms |
| busy, followed | 0.57 / 1.90 / 8.74 / 38.80 ms |

Following the thread costs the echo nothing at the median. The p90 is 0.2 ms over the unfollowed
arm, against 1.2–1.6 ms on the conversation stream. The p99 is 1.5 ms over. The 38.8 ms max is one
key in round 4, and that round's p99 is 8.7 ms. Per-round p90s, followed, are 1.29–3.14 ms, and
the busy unfollowed arm reads 0.47–2.47 ms. An answer appended to the transcript reached the
follower in p50 25.5 / p90 48.3 / p99 218.6 / max 272.1 ms over 1 791 answers. That is the 250 ms
tick and the hooks between ticks, as before, now at a lower tail (p99 was 420–500 ms).

```sh
cargo build --release -p slopty-ptyd -p slopty-cli
cargo test -p slopty-workerd --release --test e2e echo_beside_a_followed_thread \
  -- --ignored --nocapture
```

## 2026-10-04 — A stack with its own server

Every e2e app stack now starts its own `slopty-server`; the worker registers with it, and the app
follows it and dials the worker the directory lists (`docs/decisions/topology.md`, "The first Mac
runs the server"). Before, the app added the worker by its address. Measured with
`a_stack_comes_up_in_measured_time` (slopty-e2e app, debug, mac-studio (M1 Max), load average
9–11 from other lanes' builds): seven stacks in a row, from the launch call to the app's link up
and to its first shell's prompt; then seven servers started alone, to the line where they print
their listeners. The "before" row is the same test on the add-by-address harness, earlier the
same day on the same machine.

| stack | linked, median | first prompt, median |
| --- | --- | --- |
| add by address (before) | 430 ms | 450 ms |
| own server, through the directory | 449 ms | 465 ms |

The server alone starts in 7 ms (median). A stack costs about 15–20 ms more: the server's start
plus the app's link to it and the directory's first listing before the worker is dialled. That is
under 5 % of a stack's start, and no test step waits on it beyond the launch.

```sh
cargo xtask e2e app --filter 'test(~a_stack_comes_up)'
```

## 2026-10-05 — ghostty #14536 taken into the fork: the engine's cost series (M4)

Release, mac-studio, the 1-minute load 6.0 to 6.9. The same engine and binding code (libghostty-rs
695e7f7, whose bindings the merge left unchanged) built against two ghostty trees: the fork before
the merge (72c13dd) and after it (5b92fd2, upstream #14536 at 5617b8ad7 with our drag effect).
Four interleaved rounds of every `*_cost` series and `encode_cost`; instructions per operation,
median of four, with each tree's range.

```sh
git -C .research/ghostty archive <commit> | tar -x -C /tmp/ghostty-<commit>
GHOSTTY_SOURCE_DIR=/tmp/ghostty-<commit> CARGO_TARGET_DIR=target/bench-<commit> cargo test --release -p slopty-engine --tests --no-run
cd crates/slopty-engine && SLOPTY_BENCH_OUT=<out>.jsonl <lib test binary> --ignored _cost --test-threads=1
```

**The write path still pays one flag check.** `frame_cost.write` is 821 on both trees, as are
`take_frame` (19 453 at 60×12, 44 910 at 200×60), `take_frame_unchanged` (913, 1 009),
`encode_cost.echo` (4 169) and `unicode_write_cost` (3 144, 4 987, 32 952). A write the program did
no drag and drop in still reads one flag (`after_dnd`): the drop now reaches the engine through
the `drop` trampoline, which only a program's OSC 72 calls.

**Two series moved, in code the merge does not run.** `search_after_output_cost.plain` went from
1 224 555 to 1 195 198 (−2.4 %; old 1 223 986 to 1 224 717, new 1 194 418 to 1 196 992), and
`memory_cost.plain.compress_step` from 784 090 to 790 230 (+0.8 %, the ranges touching). Neither
calls drag and drop code, so they are the library's layout moving under a 130-line change. Every
other series is within 0.15 %.

## 2026-10-05 — the workspace measurements without notes

Notes became Markdown files (`docs/decisions/ui.md`, "A note is a Markdown file"), so the
measurements that filled the workspace with notes changed with them. They were not run again
here, since the change takes no frame work away from what they still measure.
`measure_an_echo_frame_beside_long_notes` is deleted, because its subject is gone; a Markdown
file's preview draws its rows from a gpui `list` the way the notes did. In
`measure_a_frame_over_a_large_registry` (120 notes × 4 KiB) and
`measure_a_stream_frame_beside_the_chrome` (60 notes), folder tiles now stand where the notes
did. Figures from either before this entry do not compare with a run after it.

## 2026-10-05 — drops read lazily: the hybrid wire (M1–M3)

A drop on a program that asks for drops (Kitty drag and drop) now sends at once what the program
accepted during the hover, uploads its files from that moment, and fetches anything else when
the program asks (`docs/decisions/terminal.md`, "Drops are read lazily"). These are the plan's
M1–M3; M4 is the entry above.

```sh
cargo xtask e2e app --filter 'test(files_dropped_on_a_program_asking_for_drops_reach_it_as_the_workers_copies)'
cargo nextest run -p slopty-worker --test session_actor --run-ignored only drop_footprint --no-capture
cargo test -p slopty-client --lib only_what_the_program_accepts_goes_up -- --nocapture
```

**M1, drop to data, files.** The e2e drags a 20-byte text file and a 24 MiB file over a bash
stand-in that asks for drops, through a link shaped as a tailnet's (`harness::TAILNET`: 4 ms
each way, up to 2 ms jitter, 3 % loss). The stand-in answers a move, so the step that reads
move is the one that heard its acceptance. It asks for the file list on the drop and copies
every file it names. Six runs:

| | median | range |
|---|---|---|
| first step → acceptance heard | 47 ms | 45–49 ms |
| upload of the 24 MiB, begun on the acceptance | 770 ms | 692–840 ms |
| drop → program has the files, after that upload | 43 ms | 42–45 ms |
| drop → program has the files, dropped at once | 5.5 s | 0.71–10.2 s |

Dropped once the hover has carried the upload, the program has its copies in 43 ms: the drop,
its request, the list and the copy of 24 MiB on the worker. Dropped at once, it waits for the
upload, which the pre-upload takes off the drop, up to the whole upload. The at-once drop is
always the connection's second 24 MiB transfer here, and that second transfer took 4.4 to 10.2 s
in four of six runs, against 0.7 to 0.8 s for every first one. That spread is the transport's
under this shape (QUIC under 3 % loss after a long transfer), not the drop's, and is noted for
its own look. Before this change the upload began only at the drop, so every drop paid the
at-once figure.

Not measured end to end: a small text fetched lazily. The e2e's drag carries files only. A
type the program accepted is pushed during the hover and waits on the worker, so it costs
nothing at the drop. One it did not accept costs one round trip after its request (8 to 12 ms
on this shape), the fetch and its answer. The worker test
`a_dropped_text_is_fetched_when_the_program_asks` shows the one fetch and the pushed text
answered with none.

**M2, bytes sent up.** A drag of a 1 KiB text, its 10 KiB HTML and a 1 MiB PNG onto a program
that accepts only `text/plain` sends 1 024 bytes, against 1 059 840 for sending every type.
This is the push policy counted (`TermDrag::accepted`), not a capture of the wire; the types
the program reads later go up only when fetched, one at a time.

**M3, the worker's peak footprint for an 8 MiB type** (the cap), through a real session into
a stand-in that hands its input straight to a file. Three runs each, in a process of its own:

| | peak footprint growth |
|---|---|
| given whole, as the worker held a type before | +64.4 MiB (64.4, 64.4, 64.4) |
| streamed in 64 KiB chunks into the program's answer | +39.1 MiB (38.7, 39.1, 39.2) |

Streaming takes 25 MiB off: the whole copy, and the engine's copy for the answer. What is left
is the program's input queue holding the answer in base64 while the program reads it, which
`INPUT_MAX_BYTES` (16 MiB) bounds. Pacing a stream by that queue would take most of the rest,
and is the next step if drops past 8 MiB are wanted.
