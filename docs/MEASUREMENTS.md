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
release build. This ad-hoc Python driver is a stopgap; the harness will be `cargo xtask bench echo`
per the pure-Rust rule.

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
frame. Window capture has a floor of ~8 ms where display capture goes down to <1 ms — SCK's
window path composites separately; worth a look if the budget ever needs it. The 60 fps cap
yields ~50–53 delivered fps on a 60 Hz display.

Not yet measured: the client's arrival→present hold in the GPUI app, a real lossy/jittery path
(Wi-Fi, LTE), and a release build.

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

Not yet measured: LTE from the phone, a release build, and the arrival→present hold in the GPUI app.

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

A window that does not change on screen (`--window 1880`, an idle Ghostty) produced
`captured 0` for 20 s: ScreenCaptureKit delivers nothing while the content is static, which
the client reports as refresh requests. Not a regression; bench a moving window or the display.

```sh
# on macbook-pro, paired with the Mac Studio's manual hostd (SLOPTY_PORT 45550)
export SLOPTY_DATA_DIR=/tmp/slopty-bench/data SLOPTY_DIRECT_ONLY=1 RUST_LOG=warn,slopty_media=debug,slopty_client=debug
/tmp/slopty-bench/slopty bench screen --display 6 --seconds 20
# on the host: RUST_LOG=info,slopty_host=debug hostd → grep 'bitrate\|stream closed'
```
