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

Not yet measured here: a real lossy/jittery path (Wi-Fi, LTE) and a release build. Arrival →
present is measured in "start-up over iroh on a quiet machine, and arrival → present" below.

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
profile after clippy checked it in `dev`, and sccache does not cache incremental workspace crates.

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
