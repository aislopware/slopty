# noq-udp, vendored with three patches

The published `noq-udp` 1.3.0 (crates.io, upstream tag `noq-udp-v1.3.0`, commit `c1f411562` of
<https://github.com/n0-computer/noq>) with these commits on top. Each is a port of a quinn-udp
change noq has not taken, finished where quinn's is still open.

1. `fix(udp): Keep SO_SNDBUF clear of macOS's permanent EWOULDBLOCK`

   Port of quinn-rs/quinn#2727 (merged 2026-07-14). A non-blocking `sendmsg` or `sendmsg_x`
   with a `cmsg` whose payload sits at or just under `SO_SNDBUF` returns `EWOULDBLOCK` for good
   on macOS (Apple FB23671230), and the socket never becomes writable for it. The socket state
   raises `SO_SNDBUF` to `65535 + cmsg::LEN` when it is made and whenever it is resized, and a
   send that would still land in the bug's region fails with `EMSGSIZE` instead, which the
   caller treats as a lost datagram. Test: `no_datagram_size_blocks_for_good_at_the_send_buffer_edge`
   sends every size across the edge of the effective buffer, with an ECN `cmsg`, on both paths.
   Without the check it fails with `EWOULDBLOCK` at 65 616 bytes against a 65 631-byte buffer.
   quinn's own test sends 60 kB, which the kernel refuses with `EMSGSIZE` whatever the buffer, so
   it cannot fail.

2. `feat(udp): Report how many datagrams a send took`

   Port of the noq-udp half of quinn-rs/quinn#2748 (approved, open), kept additive so noq
   itself builds unchanged. `sendmsg_x` can take fewer messages than it is given, and returns
   how many it took; 1.3.0 ignored the count, so the rest of a batch was dropped without a
   word. `UdpSocketState::try_send_partial` returns that count, `Transmit::datagram_count` and
   `Transmit::advance` give what is left without copying, and the old `send` and `try_send` go
   on after a partial send until the socket has no room, when the rest is dropped as a full
   buffer would drop it. A transmit longer than one batch now goes a batch at a time instead of
   tripping a `debug_assert`, and the fallback when `sendmsg_x` cannot be resolved sends one
   datagram, not the whole buffer as one. Slopty's socket (`slopty_net::udp`) keeps the part
   not yet sent across a wait for room, which is quinn's connection-side half. Tests:
   `apple_fast_datapath_reports_a_partial_send`, `a_whole_send_reports_every_datagram`.

3. `feat(udp): Enable the Apple fast path only where the OS has it`

   `UdpSocketState::try_enable_apple_fast_path` resolves `sendmsg_x` and `recvmsg_x` first and
   enables the path only when both exist, which is the one condition the `unsafe`
   `set_apple_fast_path` leaves to its caller. noq's own runtime never enables the path, and
   Slopty's transport crate forbids `unsafe`.

The fast path is compiled in only on macOS, by `slopty-net`'s target-specific `noq` feature,
and is on unless `SLOPTY_BATCHED_UDP=0` (docs/decisions/transport.md, "The batched Apple
datapath"). iOS never builds it: the calls are private and reached by `dlsym`.

Wired in with `[patch.crates-io]` in the workspace `Cargo.toml`. The files are the crate as
published (its normalised `Cargo.toml`, `build.rs`, `src`, `tests`, `benches`, licences) plus
the patches; `diff -ru` against `~/.cargo/registry/src/*/noq-udp-1.3.0` shows them. To move to a
newer noq: take the new published crate and re-apply what upstream has not taken. Drop patch 1
once noq has #2727, and patch 2 once noq has #2748 in any form; patch 3 goes with the last.

Tests: `cargo test --features fast-apple-datapath` in this directory (with
`CARGO_TARGET_DIR=../../target/noq-vendor`, then `rm Cargo.lock`).
