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
