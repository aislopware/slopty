# noq-proto, vendored with seventeen patches

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

5. `fix(proto): Pad an Initial close to the header protection sample`

   `Endpoint::initial_close` (a refused or failed first packet) wrote the CONNECTION_CLOSE and
   the tag and nothing more, counting on a 16-byte AEAD tag to cover the header protection
   sample 4 bytes past the packet number. Slopty's null crypto has no tag and claims a 16-byte
   sample, so every packet is long enough to draw a stateless reset (docs/decisions/transport.md,
   "The stateless reset a restarted host sends now arrives"). The close fell short of it, and
   the client dropped it as too short to decode. It now pads with PADDING frames up to the
   sample, which a real AEAD's tag already covers, so nothing changes for TLS. Test:
   `an_initial_close_holds_the_header_sample_when_the_tag_is_shorter`.

6. `fix(proto): Take BBR3's first round trip from the acknowledged packet`

   BBR3's first rate sample took its RTT from the `RttEstimator` it is handed, but the
   connection calls `on_ack` before it feeds that ACK's sample to the estimator, so on the first
   ACK the estimator still holds the configured initial RTT. That value became `BBR.min_rtt`.
   Upstream's 333 ms default is above any real path and the next sample replaces it, but an
   initial RTT below the path's stands for the ten seconds of `MinRTTFilterLen`, since no later
   sample is lower. Slopty's 5 ms against a 60 ms path sized the window to a twelfth of the
   bandwidth-delay product, and one bulk stream ran at 10 Mbit/s. Every later sample already
   used `now - P.send_time`, the draft's `RS.rtt` (draft-ietf-ccwg-bbr-06 section 4.1.2.3), and
   the first now does too. Test: `the_first_ack_measures_min_rtt_from_its_own_packet`
   (docs/MEASUREMENTS.md, "one bulk stream over a long round trip").

7. `feat(proto): Expose BBR3's min_rtt`

   `Bbr3::min_rtt` returns `BBR.min_rtt`. Slopty's congestion snapshot reports it beside the
   path's own measured minimum, so the gate can check the model against the path whatever the
   machine's load. The rate a bulk stream reaches depends on free cores, and a rate floor
   failed three gates with another job on every performance core (docs/decisions/testing.md).
   This crate's own tests do not run in Slopty's gate, so patch 6's unit test cannot guard it
   there.

8. `feat(proto): Add TransportConfig::stream_priority_unpaced`

   With `Some(t)`, a datagram is started without the pacing check while a stream at priority
   `t` or above has data pending. The congestion window still applies, and the pacer is charged
   for the packet as usual. The pacer's timer runs on the runtime's clock, and tokio's is a
   millisecond wheel that macOS coalesces on top, so a packet the rate earns in half a
   millisecond waited one or two. Slopty sets it to the echo's priority, so an echo behind paced
   video leaves at once: p90 1.4 ms fell to 0.25 ms at 20 Mbit/s (docs/MEASUREMENTS.md, "an
   echo behind paced video"). Tests: `stream_priority_unpaced_skips_the_pacer`,
   `stream_priority_unpaced_below_the_stream_waits`,
   `stream_priority_unpaced_off_waits_for_the_pacer`. Slopty's gate holds it through
   `an_echo_does_not_wait_for_the_pacer` (`crates/slopty-net/tests/pacer_wait.rs`).

9. `feat(proto): Add Bbr3Config::probe_rng_seed`

   BBR3 draws where each bandwidth probe starts from a generator it seeds from the OS, and the
   config's seed field had no setter outside the crate's tests. Slopty's simulated network
   (`slopty_shape::sim`) seeds everything else a run draws, and with the probes left random a
   flooded link's echo times differed between two runs of one seed. `None` stays the default.
   Slopty's gate holds it through `a_seed_repeats_its_run` (`crates/slopty-net/tests/sim.rs`).

10. `fix(proto): Keep a validated path to fall back to when a migration's check fails`

   `Connection::migrate` keeps the path it leaves so that a failed validation of the new one
   can go back to it. Its comment says it keeps that path only if it was validated, but the
   test was inverted (`!prev_path_data.validated`), so a migration away from a working path
   kept nothing. When the one PATH_CHALLENGE to the new address was lost, the retry waited on
   the anti-amplification budget the padded challenge had spent, `PathValidationFailed` fired
   first and cleared the challenge, and with nothing to go back to the path stayed unvalidated
   for good. noq sends no data on an unvalidated path, not even ACKs, while PINGs kept the
   connection from timing out: after a NAT rebinding the worker never sent the client another
   byte. With the test the right way round, the failed check goes back to the old path, and the
   client's next packet from its new address starts the migration again. Upstream `main` still
   has the inverted test (checked 2026-09-29). Slopty's gate holds it through
   `a_nat_rebinding_moves_the_connection_without_a_reconnect` (`crates/slopty-net/tests/sim.rs`,
   seed 10 loses the first challenge).

11. `fix(proto): Pace BBR3's Startup from the first round trip`

   The constructor has no round trip, so `BBRInitPacingRate` uses 1 ms and paces at
   `2.77 * InitialCwnd / 1 ms`, and Startup only ever raises the rate. A flow that stays
   application-limited, as video does, never leaves Startup and kept that placeholder, which
   on any path longer than a millisecond is no pacing at all (noq#800). The first ACK now
   re-derives the rate from its own packet's round trip, as Linux BBR's
   `bbr_init_pacing_rate_from_rtt` does on `has_seen_rtt`. This finishes noq#802 and
   quinn#2481, which take the RTT from the estimator, and the estimator does not yet hold the
   first sample when the controller sees the ACK (patch 6). Test:
   `the_first_round_trip_sets_the_startup_pacing_rate`.

12. `fix(proto): Cap BBR3's ACK-aggregation allowance at 100 ms of bandwidth`

   `BBRUpdateMaxInflight` adds `extra_acked` to the window with no bound. Linux caps it at
   `bbr_extra_acked_max_us`, 100 ms at `BBR.bw`, and so does this. Patch 2 removed the one
   runaway seen so far; the cap keeps any other (a clumping path, a count that outlives its
   interval) from growing the window with it. Test: `ack_aggregation_adds_at_most_100_ms_of_bandwidth`.

13. `fix(proto): Refresh BBR3's ProbeRTT filter from a flow's own quiet round trips`

   Video goes idle between frames, and the first packet of each frame leaves with no more in
   flight than ProbeRTT would allow (`BBRProbeRTTCwnd`, half a BDP). That packet's round trip
   is the measurement ProbeRTT takes by holding the window down for 200 ms, so when the filter
   has expired it refreshes from such a sample and the flow does not dip. A flow that was
   quiet within the last ProbeRTT's length waits for its next quiet sample rather than
   refreshing from a loaded one. A flow never that quiet still enters ProbeRTT as the draft
   says. This goes beyond draft-ietf-ccwg-bbr-06, whose only skip is on a restart from idle.
   Tests: `a_flow_quiet_between_frames_refreshes_min_rtt_without_probe_rtt`,
   `a_flow_never_quiet_still_probes_rtt`; `probe_bw_skips_probe_rtt_on_restart_from_idle` now
   checks that the resume packet's own round trip refreshed the filter. In `VideoSim`, ProbeRTT
   entries fell from 9 to 0 and P-frame p99 from 96 to 48 ms.

14. `feat(proto): Expose BBR3's state`

   `Bbr3::state_name` names the model's state as the draft does (`Startup`, `ProbeBW_UP`,
   `ProbeRTT`, ...). Slopty's congestion snapshot carries it, and its simulated-network test
   asserts that video never meets ProbeRTT.

15. `feat(proto): Bundle a pending ACK into a packet that leaves anyway`

   Port of quinn-rs/quinn#2747 (open). An ACK the delayed-ACK timer holds went out in a packet
   of its own when the timer fired, even when an ack-eliciting packet had left on the same
   path in the meantime. `populate_packet` now adds it, when it fits, to any ack-eliciting
   packet on its own path that carries no ACK yet (`PendingAcks::can_bundle`). An echo now
   carries the ACK of the key it answers: in Slopty's simulated network the worker sends 1.06
   packets per key instead of 2.02. The ACK frame is built by a new `ack_frame`, shared with
   `populate_acks`, and sized without encoding by `AckEncoder::size` and `PathAckEncoder::size`
   (3.5 to 10 ns for one to eight ranges, where encoding into a `Vec` to measure it took 27 to
   99 ns; `ack_size_is_its_encoded_length` holds the two equal). `SentFrames::is_ack_only` now also counts `path_retransmits`, since a
   packet holding OBSERVED_ADDRESS and a bundled ACK is not ACK-only, and the debug assertion
   that relies on it fired in the address-discovery tests. Test: `ack_bundled_with_datagrams`.
   `path_open_challenge_lost` advanced wakeups a fixed number of times to reach the third
   challenge; bundling moves a probe timeout in between, so it now advances until the
   challenge count rises.

16. `fix(proto): Keep a queued datagram exactly at the new limit`

   Part of quinn-rs/quinn#2839 (open). When black-hole detection lowers the MTU, queued
   datagrams that no longer fit are dropped, but the test was `len < max`, so one exactly at
   the new limit went too, although `send` accepts that size. Slopty's minimum MTU sits below
   its initial one, so the drop can happen. The rest of #2839 prunes the queue after a path
   reset or migration; here those only ever raise the MTU back to the initial 1232, so it is not
   taken. Test: `drop_oversized_keeps_datagrams_at_limit`.

17. `fix(proto): Take only a short-header packet for a stateless reset`

   `CryptoState::unprotect_header` compared the last 16 bytes of every packet with the peer's
   reset token, a long-header packet coalesced ahead of others included, and a match ended the
   connection even when the packet decrypted. RFC 9000 compares the datagram's trailing bytes
   (section 10.3.1), and a stateless reset takes the form of a short-header packet (section
   10.3), which always runs to the end of its datagram. Behind an AEAD a packet ends in its tag,
   so the check never went wrong. Slopty's null crypto has no tag. A server's Initial ends in its
   transport parameters, whose order noq shuffles, and one time in about twelve the reset token
   comes last. Sent coalesced, the Initial takes no padding, so a resent copy that the client
   read before it had answered the first ended in the token it had just learnt. The dial then
   failed with "reset by peer" (docs/decisions/transport.md, "A resent Initial is not a stateless
   reset"). Only a short-header packet is compared now. Tests:
   `a_long_header_packet_ending_in_the_token_is_no_stateless_reset` and
   `a_short_header_packet_ending_in_the_token_is_a_stateless_reset`.

A probe-up exit for an app-limited round (leaving `ProbeBW_UP` when a round ends
app-limited) was tried beside patches 11 to 13 and not taken. The draft and Linux keep such a
flow in `ProbeBW_UP`, and with patches 11 to 13 the simulated keyframe p99 already matches
Cubic's, so it had nothing left to win (docs/decisions/transport.md, "BBR3 on app-limited
video").

Commits 2, 3 and 13 share `VideoSim` in the `bbr3` tests: a screen encoder's frames through one
bottleneck, paced by noq's token bucket. `video_through_one_bottleneck` (ignored) prints what
BBR3 does with that traffic (docs/MEASUREMENTS.md, "noq's BBR3 against the draft").

`noq-proto` and `noq-udp` (`vendor/noq-udp/SLOPTY.md`) are patched (`[patch.crates-io]` in
the workspace `Cargo.toml`); `noq` comes from crates.io. The files are the crate as published (its normalised
`Cargo.toml`, `src`, `benches`, licences) plus the patches. To move to a newer noq: take the
new published crate and re-apply each commit (the diff is `git diff` of this directory against
the published files), or drop this directory once upstream has them all.
