# noq-proto, vendored with one patch

The published `noq-proto` 1.3.0 (crates.io, upstream tag `noq-proto-v1.3.0`, commit
`c1f411562` of <https://github.com/n0-computer/noq>) with one commit on top:

    feat(proto): Add `TransportConfig::stream_priority_before_datagrams`

With `Some(t)`, a packet takes the STREAM frames of streams at priority `t` or above before
its DATAGRAM frames, then the rest; `None` (the default) keeps noq's order, datagrams first.
Slopty sets it so control and session streams go ahead of video, and tunnels and file
transfers stay behind it (docs/decisions/transport.md, docs/MEASUREMENTS.md "an echo ahead of
the datagrams").

Only `noq-proto` is patched (`[patch.crates-io]` in the workspace `Cargo.toml`); `noq` and
`noq-udp` come from crates.io. The files are the crate as published (its normalised
`Cargo.toml`, `src`, `benches`, licences) plus the patch. To move to a newer noq: take the new
published crate, re-apply the commit (the diff is `git diff` of this directory against the
published files), or drop this directory once upstream takes the knob.
