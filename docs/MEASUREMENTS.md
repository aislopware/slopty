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

Not yet measured: a moving picture at native scale, a lossy path, and the client's
arrival→present hold; the numbers above only prove the pipeline and its clock plumbing.
