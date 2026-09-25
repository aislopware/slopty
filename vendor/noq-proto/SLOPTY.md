# noq-proto, vendored with four patches

The published `noq-proto` 1.3.0 (crates.io, upstream tag `noq-proto-v1.3.0`, commit
`c1f411562` of <https://github.com/n0-computer/noq>) with these commits on top:

1. `feat(proto): Add TransportConfig::stream_priority_before_datagrams`

   With `Some(t)`, a packet takes the STREAM frames of streams at priority `t` or above before
   its DATAGRAM frames, then the rest; `None` (the default) keeps noq's order, datagrams first.
   Slopty sets it so control and session streams go ahead of video, and tunnels and file
   transfers stay behind it (docs/decisions/transport.md, docs/MEASUREMENTS.md "an echo ahead
   of the datagrams").

2. `fix(proto): Clear BBR3's ACK-aggregation count on a restart from idle`

   `handle_restart_from_idle` moved `extra_acked_interval_start` to now and kept
   `extra_acked_delivered`, so after every idle gap each byte delivered counted as
   aggregation, and `extra_acked` and the window grew each other without bound. It now zeroes
   the count with the start, as Linux `tcp_bbr.c` does on `CA_EVENT_TX_START` and as the
   draft's own `OnInit` pairs them. The draft's `HandleRestartFromIdle` (draft-ietf-ccwg-bbr-06
   and the editor's copy) resets only the start. Test:
   `restart_from_idle_starts_an_empty_ack_aggregation_interval`.

3. `fix(proto): Mark the connection app-limited in BBR3's ProbeRTT`

   The draft's `HandleProbeRTT` begins with `MarkConnectionAppLimited()`, so the low
   delivery rates of a flow held to half a BDP do not read as the path's. noq left it out. Test:
   `probe_rtt_marks_its_samples_app_limited`.

4. `fix(proto): Let the pacer hold the controller's send quantum`

   A send asks the pacer for a whole MTU of credit, since the packet is not built yet, and at a
   pacing rate below 125 kB/s the bucket held one MTU. So every packet waited for the bucket to
   fill completely, and a small packet right after another (an ACK, a datagram) waited for the
   first one's bytes to be earned back. BBR3 on a lossy, app-limited connection paces at tens
   of kB/s, and that wait was 15 to 30 ms. The bucket is now never smaller than the
   controller's `send_quantum` (BBR's `C.send_quantum`, at least `2 * SMSS`,
   draft-ietf-ccwg-bbr-06 section 5.6.3); above the lowest rates the rate's own budget is
   larger and nothing changes. Test: `a_low_rate_lets_the_send_quantum_out_together`
   (docs/MEASUREMENTS.md, "datagram copies of a keystroke and its echo").

Commits 2 and 3 share `VideoSim` in the `bbr3` tests: a screen encoder's frames through one
bottleneck, paced by noq's token bucket. `video_through_one_bottleneck` (ignored) prints what
BBR3 does with that traffic (docs/MEASUREMENTS.md, "noq's BBR3 against the draft").

Only `noq-proto` is patched (`[patch.crates-io]` in the workspace `Cargo.toml`); `noq` and
`noq-udp` come from crates.io. The files are the crate as published (its normalised
`Cargo.toml`, `src`, `benches`, licences) plus the patches. To move to a newer noq: take the
new published crate and re-apply each commit (the diff is `git diff` of this directory against
the published files), or drop this directory once upstream has them all.
