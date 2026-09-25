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
cargo test -p slopty-capture --release --test geometry -- --ignored geometry_tick_cost --nocapture
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
