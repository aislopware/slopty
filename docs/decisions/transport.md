# Decisions — Transport

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **iroh 1.2.0** — superseded 2026-09-24 by **Plaintext QUIC on noq, standalone; iroh removed** (end of file). (1.1.0 verified from source in the cargo registry 2026-09-04; 1.2.0 adopted
  2026-09-12 the day after its release: `n0-dns-resolver` replaces hickory, nothing in our API
  surface moved, gate + app/iOS e2e green). QUIC via n0's own `noq` 1.3.0 underneath (a quinn fork; **not** the `quinn` crate, so quinn docs/types do not
  apply): streams for control/terminal, unreliable datagrams for media, connection migration for
  Wi-Fi↔cellular, hole punching + relay fallback, E2E encryption keyed by endpoint identity.
  Rejected: slop-desk's raw UDP + "the WireGuard mesh is the security boundary" (phone-anywhere
  needs app-level auth); WebRTC (ICE/SDP dead weight); MoQ (relay/pub-sub shape).

- ✅ **Default features off; `fast-apple-datapath` is banned.** Superseded 2026-09-30 by **The
  batched Apple datapath is on** (end of file): the 53 ms did not come back without iroh. (2026-09-24: iroh is gone; the ban
  carries over to `noq`, whose feature of the same name is off in the workspace.) `default-features = false,
  features = ["tls-ring"]`. iroh's default set includes `fast-apple-datapath` (dlsym of private
  `sendmsg_x`/`recvmsg_x`, batched UDP). Measured 2026-09-04 on loopback (`slopty ping`, see
  MEASUREMENTS.md): with it on both ends the app round trip is **53 ms** and QUIC's own RTT
  estimate 48 ms; with it off, **0.8 ms**. The batching path waits to fill batches, which is
  exactly wrong for keystrokes. The feature no longer exists in `slopty-net`; do not re-add it.

- ✅ **LAN discovery** — superseded 2026-09-24 by **Plaintext QUIC on noq, standalone; iroh removed**: no mDNS; hosts are added by address.
  Was: a separate crate, `iroh-mdns-address-lookup` 0.5.0 (there is no
  `discovery-local-network` feature in iroh 1.x). Behind `slopty-net/mdns`; hosts advertise,
  clients only look up. Wide-area lookup is iroh's `presets::N0` (DNS + pkarr on n0's infra).

- ✅ **Pairing** — superseded 2026-09-24 by **Plaintext QUIC on noq, standalone; iroh removed** and workers.md **Admission by source
  address**: no tickets, tokens or trust store. Was: `PairTicket { addr, token }` (`iroh-tickets` 1.0.0, base32 string, kind
  `sloptypair`), token single-use with a 10-minute TTL. On redeem the host trusts the client's
  endpoint key in `trust.json` (mode 0600); afterwards the QUIC handshake is the whole
  authentication and `Hello.pair_token` stays `None`. Rejections are sent then linger up to 2 s
  so the client reads `Rejected` before the connection closes (QUIC close discards unread data).

- ✅ **One connection per client**: bidi control stream opened by the client; one uni stream per
  attached session opened by the host, first message `StreamHeader { session }`; datagrams for
  media. Transport config: idle 45 s, keep-alive 5 s, 4 MiB datagram buffers.

- ✅ **1024 streams each way, and a stream that never comes is a reset** (2026-09-25). Every
  forwarded TCP connection is a bidirectional stream, and the limit was 16 with the control
  stream taking one. A browser keeps six sockets per origin plus its hot-reload websockets, so
  the sixteenth connection waited for a stream with no deadline and the page hung. Both
  directions now allow `slopty_net::endpoint::MAX_STREAMS` (1024). Flow control stays at
  noq's defaults: 1.25 MB per stream bounds what one stalled stream holds, and the
  connection window stays unbounded. A shared bound would let a few stalled streams block the
  control stream. The client gives a tunnel stream 10 s to open. After that it resets the
  accepted socket with a zero linger, so the browser sees a refused request rather than an
  empty answer. The accepted socket also gets `TCP_NODELAY`, as the worker's end already
  had. Tests: `sixty_four_forwarded_connections_at_once_are_all_served` (`slopty-workerd`
  e2e, 64 connections held open together through a real worker) and
  `a_connection_with_no_stream_to_be_had_is_reset` (`slopty-client`).

- ✅ **A session stream ends with its session, whoever closed it** (2026-09-25). The
  connection kept a strong clone of each attached session's sink, so the pump never saw the
  channel close. A session closed by another client or by an agent left its pump task and its
  unidirectional stream open for the life of the connection. At the old limit of 256 the next
  attach then waited in the connection's loop, and every keystroke waited behind it. The
  connection now keeps a weak sender, and `SessionClosed` lets go of the pump rather than
  aborting it. The pump sends what the actor left in the sink and finishes the stream. Test:
  `a_session_another_client_closes_ends_the_watchers_stream`.

- ✅ **A client that falls behind the worker's events gets the state again** (2026-09-25). The
  daemon's broadcast held 64 events, and a connection that lagged only logged it, so the
  client silently missed sessions, item changes and agent states. The broadcast now holds
  1024. On a lag the connection resubscribes and sends what the broadcast carries as it is
  now: `SessionClosed` for the sessions it had told of that are gone (with `Exited`, since
  the real reason was among what was missed), `SessionOpened` for the new ones, the item
  `Snapshot` the protocol already sends "after a gap", every agent's state, and the ports.
  Item deltas at or below that snapshot's version are skipped. Clipboard offers are not
  repeated, because the next copy offers again. The server link keeps its rule: it registers
  again. Test: `a_client_that_falls_behind_gets_the_items_again`, where one client floods
  pointings while the other stops reading.

- ✅ **Congestion controller**: noq default (Cubic); BBR3 measured 2026-09-05 (MEASUREMENTS.md),
  revisit once noq marks it stable (superseded 2026-09-05: BBR3 installed as default for both
  roles with `SLOPTY_CC=cubic` override; see line 683). The media path runs its own bitrate
  controller on top (see Video, "Adaptive bitrate").

- ✅ **ACKs within 2 ms, not QUIC's 25** (2026-09-05, `slopty_net::endpoint::MAX_ACK_DELAY`
  via `AckFrequencyConfig`, both roles). Media leaves the host as one burst per frame; BBR
  sizes the congestion window from bandwidth × min RTT, which on loopback is one or two
  frames (9–43 KB seen, 5.8 KB in `ProbeRTT`), so the tail of a frame waits for the ACK of
  its head. The receiver ACKs every second ack-eliciting packet at once and an odd last
  packet only when `max_ack_delay` expires: 25 ms by default, longer than a frame interval.
  Measured on loopback ("start-up on a cold connection" in MEASUREMENTS.md): the hostd pump
  logs every stretch in which the QUIC send buffer is not empty, and every frame ended with
  1.5 KB held for 5–7 ms, a 68 KB frame was held 105 ms at a 38 KB window, and the receiver
  NACKed the tails (6 NACKs in 3 s at the 5.8 KB window). With 2 ms the same run shows no
  hold over 5 ms and no NACK. Cost: an ACK per 2 ms of traffic at most, ~50 bytes each.

- ✅ **Initial congestion window 32 packets** (2026-09-05,
  `slopty_net::endpoint::INITIAL_WINDOW_PACKETS`, `SLOPTY_QUIC_IW` overrides it for
  experiments). RFC 9002's 10 packets is for an unknown peer on the open Internet; a paired
  host and client on a LAN or a mesh can start with the burst Chromium's QUIC uses. The
  first keyframe is tens to hundreds of KB and each window's worth costs a round trip, so
  the start-up cost of the default is 1–2 extra round trips per stream on a 10 ms path
  (numbers in "start-up over the mesh", MEASUREMENTS.md). Invisible on loopback.

- ✅ **QUIC datagrams, not raw UDP over a WireGuard mesh** (2026-09-24: still QUIC, but the
  "app-level auth" half is superseded by **Plaintext QUIC on noq, standalone; iroh removed**: the mesh is the boundary after all, and the
  per-packet AEAD is gone.) (re-examined 2026-09-04 when the
  status bar showed ~100 ms). A QUIC datagram is one UDP packet plus ~30 bytes of header and one
  AEAD (hardware AES-GCM); WireGuard spends the same per packet on ChaCha20, so "UDP + WG" moves
  the crypto, it does not remove it. Measured: app RTT 0.8 ms on loopback, 0.9–2 ms on the LAN
  direct path; the ~100 ms readings were the *relay* path (n0's aps1 server) while iroh had
  dropped the direct path (see below). slop-desk's transport doc admits its numbers were
  loopback-only and the WireGuard case was never measured. What QUIC buys: reliable streams and
  unreliable datagrams on one connection, migration, NAT traversal and relay fallback for a
  phone off the mesh, and app-level auth instead of "the mesh is the boundary".

- ✅ **Direct-only mode** — superseded 2026-09-24 by **Plaintext QUIC on noq, standalone; iroh removed**: there is no relay to be direct
  about; every connection is one UDP path. Was: (`slopty_net::Reach::DirectOnly`; hostd `--direct-only`, every binary
  honours `SLOPTY_DIRECT_ONLY=1`) for hosts and clients on a private mesh (NetBird/Tailscale)
  or one LAN: `RelayMode::Disabled` + `clear_address_lookup()`, mDNS kept. The ticket then
  carries only IP addresses (the mesh address among them) and a relay can never be selected: a
  dropped direct path drops the connection, which the app's reconnect loop turns into a
  ~1 s blip instead of a silent ×50 latency step. Gotchas: `Endpoint::online()` waits for a
  home relay and hangs with relays off, so `endpoint::online` watches `watch_addr()` for the
  first `TransportAddr::Ip` instead; a client that paired in `Anywhere` mode has a stored
  address with a relay URL, so `client::connect` strips relay entries when direct-only (else
  the dial waits on a relay it cannot use). Loopback test `direct_only_pairs_without_a_relay`
  asserts the ticket is relay-free and the selected path is direct. Roaming caveat: with no
  relay and no pkarr the client only knows the addresses in its stored ticket plus mDNS, so a
  host whose mesh IP changes needs re-pairing. Observed 2026-09-04: when the host process is
  killed, the client's lone direct path times out at 15 s, noq logs `failed closing path
  err=LastOpenPath` and keeps it, and the connection only drops at the 45 s idle timeout
  (`IDLE_TIMEOUT`); the top bar showed a stale RTT until then. Seen once (2026-09-12, the
  first gate after the zed sync, cold build then 516 tests at once): the loopback test
  `pair_then_reconnect_then_reject_stranger` failed with the stranger's *dial* timing out at
  45 s (`Connect("timed out")`, the idle timeout again) instead of the host's `NotPaired`;
  alone it takes 0.24 s and the whole suite passed on the next run in 12.7 s. Not acted on:
  one occurrence, and a retry policy would only hide the rate. If it recurs, the number to
  read first is whether the host's accept loop ever saw the stranger. ✅ Liveness now comes from
  QUIC itself, no protocol message: the app samples `ConnectionStats.udp_rx.datagrams` once a
  second (`WorkerLink::received_datagrams`); keep-alive pings make a live host send something
  every 5 s, so a counter that stands still for `SILENCE_WARN` = 8 s turns the RTT readout into
  a yellow "host silent Ns". Verified 2026-09-05 by `SIGSTOP`ping hostd. Since the same day the
  app also *gives up* at `SILENCE_DROP` = 15 s (three missed keep-alives, noq's own path bar):
  `WorkerLink::abandon` closes the QUIC connection, the control reader reports `Disconnected`,
  and the normal reconnect loop runs, so a restarted host is back ~2 s after it answers instead
  of after the 45 s idle timeout. Measured with a frozen hostd: dropped at 15.2 s, reconnected
  2 s after `SIGCONT`. Reading the logs afterwards: a killed app's *old* connection still shows
  up on hostd as "connection lost: timed out" 45 s later; that is the stale one, not the new.
  Driving note: cliclick `kp:return` never reaches the app (osascript `keystroke return` does).

- ❌ **Machine load alone does not flap the path** (2026-09-06; the harness was deleted
  2026-09-24 with iroh: a single-path connection has no relay to flap to; amended 2026-09-26
  by **The keystroke path's threads are user-interactive**, which sets the QoS this entry
  declined on the strength of the echo's tail, not of path flaps), closing the 2026-09-04
  investigation of one connection that went direct → relay-only for 43 s → direct while the
  machine was compiling. iroh's `BiasedRttPathSelector` always prefers a live direct path, so
  the direct path had to have been *closed* (noq abandon reasons `TimedOut` = 15 s path idle
  with 5 s heartbeats, `UnusableAfterNetworkChange`, `RemoteAbandoned`), and the standing
  hypothesis was that a busy machine starves the QUIC driver task until the heartbeats miss.
  A harness now says otherwise: `path_flap_under_{cpu,user_initiated_cpu,memory_io}_load`
  (`apps/slopty-worker/tests/e2e.rs`, gate `SLOPTY_FLAP_E2E`) runs hostd and a client over iroh
  with relays enabled — both ends hold a relay path *and* a direct path, so there is somewhere
  to flap to — attaches a shell and a display stream, and hammers the machine for 90 s while
  sampling the selected path four times a second and keeping noq's own path log. Three shapes,
  each the whole machine: all-core spinning at default QoS, the same at
  `QOS_CLASS_USER_INITIATED` (what a build's workers ask for), and memory + I/O (GB-scale
  writes and reads plus thousands of small files). **None of them closed a path or spent a
  single sample on the relay**; the worst rtt on the direct path was 6–10 ms against 1.4–1.9 ms
  idle (MEASUREMENTS.md, "the path under load"). So load is ruled out on its own and nothing in
  the runtime or thread QoS is changed on the strength of it: no dedicated QUIC runtime, no
  `pthread_set_qos_class_self_np` on the driver threads, no send-queue priority.
  What the harness cannot see, and what the ruling therefore does *not* cover: its direct path
  is loopback on one machine, so it has no NAT rebinding, no Wi-Fi, no WireGuard and no black
  hole — the 2026-09-04 flap was on the mesh between two machines. The load half of the
  hypothesis is dead; the network half is untested and needs the second machine. Load is not
  free either: 19–38 datagram stalls per 90 s appeared under every shape where an idle machine
  has none, which is the pacer's problem, not the path's.

- ✅ **A dying connection detaches only its own sinks** (2026-09-05). Symptom on the simulator:
  ~45 s after relaunching the app, every terminal stopped updating while the host kept running
  the typed commands. Cause: the relaunched app reconnects under the same `ClientId`; the old
  QUIC connection idles out (`IDLE_TIMEOUT`) later and hostd's cleanup called
  `Host::client_gone(client)`, which detached *every* viewer with that id, i.e. the new
  connection's. Rule: viewer eviction is scoped to the connection that attached it:
  `SessionHandle::detach_sink(client, &sink)` removes a viewer only if `Sender::same_channel`
  matches, and `Peer::drop` in `slopty-worker` uses it; `client_gone` is gone. Regression test
  `stale_connection_detach_keeps_the_reconnected_viewer`.

- ✅ **Terminal search runs on the host** (2026-09-05). The client caches at most 20k lines and
  the host retains 50k, so searching the client cache would miss most of the history or pull
  megabytes on every keystroke. `TermRequest::Search { needle, max }` answers with
  `TermEvent::Matches { needle, total, matches }`: every hit counted, the newest `max` (5 000)
  listed as `(line, col, len)` in cells. The engine renders the grid as plain text with
  libghostty's `Formatter` (plain, trim, selection over every row): one line per row, interior
  blank rows kept, trailing blank rows dropped, 0.36 ms for 904 rows (MEASUREMENTS.md), ~20×
  cheaper than the per-cell `lines()` walk. Columns come from ghostty's `grapheme_width`, so a
  hit paints exactly over its cells (wide characters count two). Smart case (no upper-case
  letter → case-insensitive) with a char-for-char fold so indices stay aligned; hits never span
  a soft-wrapped row. Rejected: libghostty-vt has no text search of its own (its "search"
  functions are word-selection helpers).
  UI: the field is a gpui-kit `Input` inside a `TerminalSearch` key context, so `escape` is
  bound there only and Esc in the grid still reaches the program; `TerminalView::key_down`
  also drops keys while that field is focused. Verified on macOS: 123 hits, stepping into
  history scrolls the hit to the viewport centre, Esc/✕ hand the keys back.

- ✅ **Regex search is a flag on the same request** (2026-09-05, protocol 5).
  `TermRequest::Search { needle, max, regex }` compiles the needle with the `regex` crate
  (1.13.1, linear-time, `size_limit` 1 MiB so a pathological pattern cannot blow up the host);
  smart case becomes a `(?i)` prefix, so plain and regex mode agree on casing. Hits are
  reported in chars and then mapped to cells exactly like plain hits; empty matches (`a*`)
  are skipped rather than painting zero-width highlights. An invalid pattern answers
  `TermEvent::SearchInvalid { needle, message }` instead of an empty `Matches`, so the client
  can tell "no hits" from "bad regex" (the bar shows `bad regex`). The `.*` toggle in the
  search bar restarts the search in the other mode and is lit with the accent when on.
  Verified on macOS: `item[13]` plain → none, regex → 4/4, `item(` → bad regex.

- ✅ **Scrollback is bounded by lines, not libghostty's 10 KB byte default** (2026-09-05).
  `Terminal.zig` defaults `max_scrollback_bytes` to 10_000; the engine had only raised the
  line limit, so every session kept about one page (~900 rows). `set_scrollback_max_bytes(None)`
  in the constructor; tests `the_line_limit_governs_retained_history` (20k of 20k kept) and
  `history_is_pruned_near_the_line_limit` (page-granular, bounded).

- ✅ **NACK and refresh requests are datagrams, not control-stream messages** (2026-09-05).
  Measured on the Wi-Fi → mesh path (MEASUREMENTS.md, "screen stream over the mesh"): loss is
  bursty (a typical gap takes 3–6 of a frame's 5–7 fragments, parity included, so FEC recovered
  2 of 29 gaps), and the frames the receiver gave up on had two NACKs out and *nothing* back
  within the 70–83 ms deadline while the host had the frames in its history. The control
  stream is ordered: once the packet carrying a NACK is lost, every retry queues behind it
  until QUIC's loss timer (a PTO, tens of ms on Wi-Fi) retransmits it, so the receiver's own
  retries cannot help. `slopty_proto::screen::Feedback { Nack, Refresh }` now goes
  client → host as a QUIC datagram (postcard body, no header; the host reads datagrams only
  for this), each one standing alone; a fragment list that would not fit a datagram degrades
  to "whole frame". Reports stay on the control stream (they are periodic and idempotent).
  Protocol 3.

- ✅ **Loss deadlines only run while the link is flowing** (2026-09-05). With NACKs as
  datagrams the give-ups did not go away, and the host side explained why: hostd now logs the
  selected path's `cwnd`/`congestion_events`/`lost_packets` and the datagram send-buffer
  headroom on every NACK — QUIC reported **0 lost packets** for the whole run, cwnd wide
  open, buffer empty, and the client's two NACKs for a "lost" frame reached the host 20 ms
  *after* the client had already asked for a refresh. So the Wi-Fi/mesh path does not drop;
  it **stalls** for 100–300 ms and then releases everything at once (both directions), and a
  wall-clock deadline turned every stall into a refresh (IDR) plus dropped frames. Rule in
  `Reassembler::tick`: a NACK is retried only if something arrived since the last one, and a
  frame past its deadline is lost only if newer datagrams are still coming in
  (`now - arrived_at < deadline`); an outright stall is waited out up to `Config::max_hold`
  (500 ms) — a refresh could not cross it either. When the stall clears (a silence of at
  least `Config::stall_gap`, 50 ms, or one NACK round trip if longer) every pending frame's
  clock restarts: the NACK written into the stall only left with the release, so its answer
  is a round trip away from *now*; the first version without this still lost a frame in the
  very tick the burst arrived. The 50 ms floor matters: at LAN round trips the NACK gap is a
  few ms, and without the floor every 17 ms inter-frame gap would count as a stall. Tests
  `a_stalled_link_holds_the_frame_until_it_moves_again`,
  `a_stall_restarts_the_nack_clock_when_the_link_resumes`,
  `a_stall_longer_than_max_hold_gives_up`. Cost: on a *still* screen a genuinely dropped tail
  waits the full 500 ms before the refresh (nothing newer arrives to prove the drop).

- ✅ **hostd binds a fixed UDP port** (2026-09-05): `slopty-worker --port` / `SLOPTY_PORT`,
  default `slopty_net::endpoint::WORKER_PORT` (45550), IPv4 required, IPv6 best effort. Found
  when a hostd restart stranded the paired MacBook: with `--direct-only` a client on another
  subnet knows the host only by the ticket's `ip:port` and cannot hear mDNS, so a random port
  per launch meant re-pairing after every restart. A port in use is a hard error (a silent
  fallback to a random port would bring the problem back); tests pass `--port 0`.

- ✅ **BBR3 congestion control on the QUIC connection** (2026-09-05). Cubic (noq's default)
  cut the host's window to 13–20 KB after 5–9 real losses in 15 s on the Wi-Fi/mesh path;
  at a 10 ms round trip that caps the stream near 10 Mbit/s, a third of the 30 Mbit/s target,
  and media datagrams sit in the send buffer waiting for the window. noq ships BBR3
  (`noq_proto::congestion::Bbr3Config`, a direct workspace dependency since iroh does not
  re-export the module); `transport_config()` installs it for both roles. See
  MEASUREMENTS.md for the before/after window sizes.

- ✅ **Refresh repeats back off** (2026-09-05). A target that never produces a frame (a hidden
  window) left the receiver in "need refresh", re-asking every `refresh_repeat` + 2 rtt — 79
  requests in 10 s. Each unanswered repeat now doubles the wait up to `refresh_repeat_max`
  (2 s); any video datagram resets it, so a live stream still recovers at the fast cadence.
  Settled by the ruling below: backoff is retained as a fallback cap, but an idle target is
  silenced by the host source hint ("The host says when its capture target is idle; the
  receiver stops asking, protocol 13").

- ⚠️ **Never await iroh's `Endpoint::close` on GPUI's executor.** (iroh removed 2026-09-24; the
  rule — tokio-timed work runs on the runtime — still holds for noq's `wait_idle`.) It uses `tokio::time::timeout`,
  which panics (`Handle::current`) outside a tokio runtime context; the app aborted on every
  disconnect until the close was spawned onto the runtime and joined (crash report
  2026-09-04). Rule: anything from iroh/tokio-time runs via `runtime.spawn`, GPUI tasks only
  await join handles and channels.

- ⚠️ **One iroh endpoint per process** (found by the app self-test 2026-09-05; iroh removed
  2026-09-24, the app still binds one client endpoint per process, now a `OnceLock`). The app used to
  bind an endpoint to pair, close it, and bind a second one with the same secret key to
  connect; the second dial hung until the step timeout (the host never saw its packets;
  iroh's discovery/relay state for that key was still the old endpoint's). `slopty-app::net`
  keeps a process-wide `OnceCell<Endpoint>` used by both `pair_host` and `connect_to`, and
  never closes it.

- ✅ Deployment is two LaunchAgents written by `slopty worker install` (`plist` crate, XML):
  `KeepAlive` + `RunAtLoad` + `ThrottleInterval 2` so a crash comes back in 2 s and login
  starts both; `ProcessType Interactive` and `LimitLoadToSessionType Aqua` because the worker
  needs the window server and ScreenCaptureKit and must not be App-Napped; sockets under
  `<data dir>/run/` (not `$TMPDIR`, which launchd children may not share) and the CLI's
  socket lookup falls back to that path when it exists. `install` re-bootstraps (bootout
  first, so a stale socket file never wedges the bind) and waits up to 10 s for a ticket.

- ✅ **"install hooks" is a pill on the title bar of the first unhooked agent, and hostd
  writes the settings** (2026-09-05). The human whose agent is being guessed at may be on a
  phone, so `ClientMsg::InstallHooks` asks the host to do it and `WorkerMsg::HooksInstalled`
  comes back as a notice. The installer moved from `slopty-cli` into `slopty_agent::hooks` so
  the CLI and the daemon run the same code; hostd registers the `slopty` binary beside itself
  (`Contents/MacOS` in a bundle, `target/<profile>` in a build tree). The offer shows once per
  run and comes back only if the host reports it failed. Wire, with the `AgentSource` of the
  attribution ruling above: `AgentEvent.source`, `ClientMsg::InstallHooks` and
  `WorkerMsg::HooksInstalled`, goldens `worker_agent_process`, `worker_agent_hook`,
  `client_install_hooks` and `worker_hooks_installed` (all new) with `client_hello` re-accepted,
  PROTOCOL_VERSION 11 → 12. Superseded 2026-10-05 by **No app offer to install the hooks**
  (claude-code.md): the pill, `ClientMsg::InstallHooks` and `WorkerMsg::HooksInstalled` are gone.

- ✅ **A stall is the link's silence, not the source's** (2026-09-06, measured). The receiver
  counted a stall whenever nothing arrived for a stall gap, which read the capture's own quiet
  gaps as a held link: 3 / 2 / 0 / 2 stalls per 5 s on an idle machine at 0 / 20 / 50 / 100 ‰
  (MEASUREMENTS, "parity, NACK and refresh under injected loss"), enough that the gated test
  skipped its per-rate verdicts on every run. The heartbeat was supposed to prevent this — it
  says "the link is up, the source is quiet" — but a beat that is itself late leaves the same
  hole. The answer was already on the wire: every datagram carries `send_ms_lo`, the low byte of
  the host's millisecond clock, so the difference between two stamps is how long the host waited
  between sending them. `link_gap = arrival_gap − worker_gap` is the link's share, and only that is
  compared with the stall threshold and charged to `stalled_ms` — RFC 3550's interarrival
  arithmetic, used for attribution rather than jitter. Three cases keep the old pessimistic
  reading, because they are cases where the receiver genuinely cannot tell: a gap past the
  stamp's 256 ms range, a retransmission (it carries the original frame's stamp), and no previous
  stamp at all. `Reassembler::stalled` also returns false while the host reports the source idle,
  which is the in-progress half — a gap that has not ended yet has no stamp to settle it. Result:
  0 stalls at every rate, and the gated test asserts its verdicts again. Tests:
  `a_quiet_source_whose_heartbeat_was_late_is_not_a_stall` and
  `only_the_workers_share_of_a_gap_is_forgiven` (`crates/slopty-media/tests/pipeline.rs`), with the
  existing stall tests rewritten to produce their frames *before* the silence they are released
  after, which is what a held link actually looks like.

- ✅ **A suspicion moves the stream to the window filter, because ScreenCaptureKit stalls an
  application-scoped display filter when a window of that application is ordered out**
  (2026-09-06, found by the false-suspicion test). With the hold alone, a sibling window of the
  target's process closing left the stream dead: no sample buffer ever again, no error, no
  status change (MEASUREMENTS.md, "a sibling window closing stalls the crop"). Re-applying the
  same filter, from the cached content or a fresh `SCShareableContent`, does not wake it; a
  change of filter kind does. So `follow_window` treats a live suspicion like a covered window
  — the crop is not allowed, the stream goes to the window filter on the next tick — and comes
  back to the crop on the first tick after the hold; a crop update asked in the same tick as
  the retarget is rejected once with `-3812` and lands on the retry, like any other path change.
  Frames stay held for the whole hold on either path: the variant that let the window filter's
  frames through encoded 1–2 pictures of the backdrop after the swap had settled, because the
  framework still delivers a frame or two of the old filter after the completion handler.
  Measured cost of a false suspicion, while every window of the application still counted as
  the target: 520 ms without frames and 16 held, nothing withheld, the crop back by itself.
  Side effect on true hides: off the crop path 158–206 ms after the order (373–473 before),
  since the swap is asked on the suspicion rather than the confirmation. The `capture frame
  status` debug line (one per change of `SCFrameStatus`) stays in `slopty_capture::stream`: it
  is how a silent stall is told from an idle source. Amended the same day, once the watch
  matched the target (the ruling above): another window going is no longer a suspicion but
  still the event the framework stalls on, so it sets `filter_stalled` and the next geometry
  tick takes a stream on the crop through the window filter and back with no hold — frames flow
  throughout, ten more of them 207–367 ms after the order-out, none held or withheld
  (MEASUREMENTS.md, "pop-ups and siblings against the targeted watch"). The 520 ms freeze is now
  only the unmatched watch's price.

- ✅ **A stall means the link held datagrams while the receiver was awake to notice** (2026-09-06,
  measured). "A stall is the link's silence, not the source's" (above) put the send stamps to
  work, and a quiet loopback stream still reported stalls the host could not have caused: it
  held no frame, QUIC lost no packet, and every charged silence had **no frame pending** — there
  was nothing for the link to hold (MEASUREMENTS, "a quiet loopback stream's stalls"). Taking
  all 13 silences past the gap apart over 270 s named two readings, both the receiver's:
  * The stamp difference was *discarded* whenever it read longer than the arrival gap it
    explained. It reads longer routinely: `send_ms_lo` is written when the host builds a
    datagram, not when QUIC sends it, and the two datagrams either side of a silence wait
    different amounts in the send queue — 7.3 and 7.6 ms in the runs. Discarding charged the
    whole silence to the link. The difference is congruent to the host's real interval modulo
    the byte's 256 ms range, so it is now read as a **signed offset from the arrival gap**: below
    the midpoint the host covers the silence, above it the datagram overtook its predecessor and
    is refused exactly as before (`STAMP_SLACK`, `Stamp::{Worker, Covered, Backwards}`). Past
    256 ms the ambiguity is real and the pessimistic reading stands (`Stamp::Wrapped`).
  * **A receiver that was not scheduled charged its own load to the link.** Nothing distinguishes
    a held link from an unread socket from inside a task that never ran, and the same runs had
    the client's stream worker 64–80 datagrams behind by a stall gap or more, worst 149 ms.
    `Config::tick_period` is now the cadence the owner promises, `Reassembler::tick` records when
    it kept the promise, and the stretch of a silence beyond it is subtracted before anything is
    charged (`dozed`). `slopty-client`'s `IDLE_TICK` moved 50 ms → 25 ms so the receiver looks
    twice per stall gap, for the same reason the host beats twice per gap: at 50 ms a gap slept
    through entirely still left one whole stall gap charged.
  What a stall no longer means: "nothing arrived for 50 ms". What it means now: "for a stall gap,
  the host's own stamps say it was sending and this receiver was awake and saw nothing". Deadlines
  still restart on any silence nothing could have crossed, receiver-side included, so a NACK
  written before one still gets its round trip. Result over five 90 s samples: **not one stall
  charged for want of a reading** — `stamp_wrapped`, `stamp_backwards`, `stamp_absent` and
  `receiver_dozed` are zero in every sample, where before two of four stalls were the discarded
  stamp and the other two the descheduled receiver. Two samples report 0 stalls outright (against
  2 / 0 / 2 before) and the rest report holds the counters stand behind: `stamp=Host(0ns)` with
  **a fragment of the frame still pending**, on the host's send side (`cwnd 5808`, QUIC's floor,
  against a 30 Mbit/s target on a 2.9 ms path). Freezing growth on those is right. The self-check
  is `slopty bench screen --max-stalls 0`. Tests (`crates/slopty-media/tests/pipeline.rs`):
  `a_stamp_reading_just_past_the_silence_still_belongs_to_the_worker`,
  `heartbeats_late_by_three_beats_are_charged_to_the_sender`,
  `two_hundred_milliseconds_in_flight_is_one_stall` (which must still be one stall of 200 ms, and
  still freeze the bitrate controller) and `a_silence_the_receiver_slept_through_is_not_a_stall`;
  the harness now separates `awake` (time passes, the receiver ticks) from `advance` (it does
  not), which is the distinction the rule turns on. Not done: widening `send_ms_lo` past its
  256 ms range — no run has produced a `Stamp::Wrapped` silence, so the protocol bump would be
  paying for a case nothing has shown yet.
  Amended 2026-09-06: the sentence above about the silences the stamps charged to the host was
  right, and the entry below is what was behind them — the host really was quiet, for up to
  242 ms, because its beat was sharing a task with a blocking call.

- ✅ **The heartbeat has its own task** (2026-09-06, measured). The beat is a promise about time:
  one every `HEARTBEAT_AFTER` (25 ms), so that a receiver counting a silence of `STALL_GAP`
  (50 ms) never has to. It was being sent from `cursor_loop`, which every 100 ms also asks the
  window server where the target is — a round trip this project has measured at up to 90 ms, and
  which the accessibility work showed is not cheap in general. A beat behind that call is late by
  several times its own period, which is exactly the failure the beat exists to prevent.
  Split in two: `beat_loop` touches nothing but atomics and the datagram queue, and `cursor_loop`
  makes its geometry and pointer calls through `spawn_blocking` so it does not hold a runtime
  worker either — a blocked worker delays every timer on it, which is the mechanism, not the
  syscall itself.
  Measured over a quiet minute, in `ScreenStats`: the median gap is 31 ms either way (the loop
  checks a 25 ms promise every 8.3 ms, so beats land on tick boundaries) and p95 is 35 ms. What
  changed is the tail — **242 ms worst before, 45–75 ms after over four runs**, with the late beats that used to
  appear mid-stream gone. The receiver counts 0 stalls and 0 ms stalled.
  The counter that found it is worth keeping in mind: the quantiles slide over the last 600 beats,
  about twenty seconds, so a single 242 ms gap in a sixty-second stream is *not in them* — p95
  read 35 ms while the stream had a quarter-second hole in it. `beat_gap_worst_us` is an all-time
  maximum for that reason, and a rule about a promise like this has to be written on the worst
  case, not on a quantile.
  Not fixed here: what remains is about 75 ms once a minute, and around a path swap about 108 ms.
  Neither is this loop — its own geometry call reads p95 5 ms, max 12 ms in the same runs — so
  the remaining blocking is elsewhere on the runtime, hostd's own `check_geometry` being the
  candidate, and moving that off a worker means turning a synchronous ScreenCaptureKit path
  async, which is not a change to make while chasing a tail. The e2e asserts the p95 against
  `STALL_GAP` and 0 stalls, and deliberately does not assert the worst gap.
  Tests: `a_slow_geometry_call_does_not_make_the_beat_late` (unit: 300 ms of blocking work beside
  the beat, cadence kept) and `the_heartbeat_keeps_its_cadence_on_a_quiet_stream` (`slopty-worker`
  e2e, `SLOPTY_SCREEN_E2E`, a minute on a stream whose target draws nothing).

- ❌ **Raising BBR3's minimum congestion window, or softening `ProbeRTT`** (2026-09-06). The long
  holds on a quiet loopback stream are `ProbeRTT`: every 5 s without a lower RTT sample BBR3
  clamps the window to `max(0.5 × BDP, MinPipeCwnd)` for 200 ms, and on a 2 ms path the BDP is
  ~11 kB so the floor is what applies — 24 of the 32 holds past 25 ms in 7.5 minutes of samples
  had the window at exactly 5 808 B = `4 × smss` (MEASUREMENTS.md, "what actually holds a frame").
  Not reachable from here: `noq_proto::congestion::Bbr3Config` carries the fields
  (`probe_rtt_cwnd_gain`, `default_cwnd_gain`, …) as private members and `impl Bbr3Config` exposes
  **one setter, `initial_window`** (`bbr3/mod.rs:2015`); `min_pipe_cwnd` is `4 * self.smss`
  outright (`bbr3/mod.rs:1841`), asserted by a noq test. noq-proto 1.2.0 is the only version
  published, so there is no bump either. The ask upstream is either a setter for
  `probe_rtt_cwnd_gain` or skipping `ProbeRTT` for a flow that is application-limited — such a
  flow never fills the pipe, so its minimum RTT is already an unqueued one and the 200 ms buys
  nothing. Until then the cost is measured and named rather than worked around.

- ❌ **Switching the default congestion controller to Cubic** (2026-09-06), ✅ **`SLOPTY_CC=cubic`
  as the measured short-path escape hatch.** Cubic has no `ProbeRTT` and it shows exactly where it
  should: over 3 × 90 s its window never fell below 38 960 B against BBR3's 5 808, holds past
  25 ms fell from 4.3 per minute to 0.9 and the worst from 668 ms to 48 ms. That is not enough to
  move the default. The BBR3 ruling above (line 634) rests on the Wi-Fi/mesh path, where Cubic cut
  the window to 13–20 KB after a handful of real losses and capped the stream near 10 Mbit/s for
  the rest of the session; a 200 ms hitch every few seconds is worse than nothing but it is not
  worse than that, and no measurement of Cubic over the mesh exists to weigh against it. Two
  controllers, two paths, one knob: the numbers for both are on the MEASUREMENTS page and
  `SLOPTY_CC` selects either. Revisit when the mesh comparison exists, or when noq exposes the
  `ProbeRTT` knobs. **Settled 2026-09-06 by the mesh comparison below: the default stays BBR3,
  and the loopback numbers in this entry are true but were taken in a regime where no window
  binds.**

- ❌ **The client's `pacing.rs` is not the ACK path** (2026-09-06, checked while looking for a
  receiver-side limiter). It is the display pacer — present-on-arrival, replace rather than queue,
  and the percentiles that go with it. What governs ACK cadence is `slopty_net::endpoint`'s
  `MAX_ACK_DELAY`, already at 2 ms since 2026-09-05, and the eight 90 s samples here confirm it is
  not limiting: 0 NACKs, 0 lost packets, 0 congestion events in every run.

- ✅ **A congestion window is read as a series, not as a final sample** (2026-09-06,
  `slopty_net::endpoint::trace_path_health`, `SLOPTY_PATH_TRACE_MS` in milliseconds, off by
  default). One reading at the end of a run cannot tell a window that sat on BBR's four-packet
  floor the whole time from one that dipped there for 200 ms — and a 15 s sample missed the dips
  entirely. (The pump half is superseded 2026-09-25 by **Media datagrams go to QUIC from the
  thread that made them**: with no pump there is no channel for a datagram to wait in.)
  Paired with a **backlog** line in hostd's datagram pump, the host-side twin of
  `receiver_dozed`: a datagram waits either in the pump's channel, because the task did not run,
  or in QUIC's send buffer, because the window holds it, and a receiver sees the same silence for
  both. The pump is never late by more than 12 ms in any sample, which is what makes the
  window the answer. That measurement was wrong when first taken — the timer started when `recv`
  handed over a datagram, so it timed the drain and not the sleep before it, and a pump
  descheduled for 200 ms could have reported 1 ms. It now sums the turns owed to datagrams
  already queued and keeps the worst turn against the 1 ms deadline the loop asks for while work
  is outstanding; re-measured, no turn past 25 ms in 3 × 90 s against QUIC holds of 96–100 ms in
  the same logs. Unit test: `a_pump_descheduled_before_it_drains_reports_the_sleep_not_the_drain`.
  Amended 2026-09-06: the turn accounting still could not see a datagram that arrived in an
  empty queue and then waited out a deschedule alone — `queued_before` was zero, so no turn was
  owed. Every datagram now carries its enqueue time (`slopty_worker::screen::Queued`, stamped in
  `Shared::push` with `host_now_us()`) and the pump reads the wait on the way out: the caught-up
  line reports `waited` p50/p95/max over the last 1024 datagrams and the worst since the last
  line, and a lone wait past 20 ms with no backlog to charge it to is logged on its own, at most
  once a second. Host-internal; the stamp never goes on the wire. Unit test:
  `the_wait_ring_reports_the_last_window_and_the_worst_once`; numbers in MEASUREMENTS.md
  "what a datagram waits in the pump's channel". The bench's `quic path (client side)` line — the feedback connection, permanently at
  `min_pipe_cwnd` because nothing measurable flows on it — is relabelled
  `quic path (client→worker, feedback only)`; reading it as the media window is the mistake this
  entry exists to stop.

- ✅ **With no audio, the heartbeat is what keeps the cadence — and that is all a keep-cadence
  datagram can do** (2026-09-06). ❌ **Beating at the inter-frame gap instead of
  `HEARTBEAT_AFTER`.** The question was whether a stream carrying no audio loses its datagram
  cadence and lets the congestion window fall to the floor. Measured both ways on the same host
  (MEASUREMENTS.md, "Audio on"): with sound playing, `worker_quiet` silences go to 0 — the sender is
  never quiet — and the window is at 5 808 B for **23 % of samples against 1.4–8 % on the quiet
  runs**, with 27 of its 28 long holds there and 14 stalls. Traffic does not lift the window; the
  window is low because the flow is application-limited and BBR sizes it from `bw × min_rtt`, which
  on a still desktop (~1 Mbit/s over 1.5 ms) is under a packet. More datagrams are more bursts into
  a four-packet window. The heartbeat's job is the receiver's stall clock, not the congestion
  controller's estimate, and at half the stall gap it already does that job: `worker_quiet` silences
  run 0–4 per 90 s with `stamp_absent` zero throughout. Raising its rate would buy nothing and cost
  a datagram every 16.7 ms per stream. The only filler that would move the estimate is filler at
  the target rate, which is 30 Mbit/s of nothing on a link carrying 1 Mbit/s of content.

- ✅ **A stall still on when the run ends counts against `--max-stalls`** (2026-09-06,
  `apps/slopty-cli/src/bench.rs`, from Codex's review of the stalls track). The check read
  `stats.stalls`, which only counts stalls that *released* — so the worst case there is, a link
  that stops and stays stopped, passed with zero. A run reporting `stalls 0 (66 ms stalled)` did
  pass (MEASUREMENTS.md, 2026-09-06). It now counts an unreleased stall and requires
  `stalled_ms == 0` when zero stalls are allowed.

- ✅ **The receiver's doze credit survives the tick that observes it** (2026-09-06,
  `crates/slopty-media/src/reassemble.rs`, same review). A receiver waking from a long sleep runs
  whatever its executor polls first, and a ready timer is as likely as the socket. `tick` moved
  `last_tick_at` to now, so an ingest a moment later found nothing slept through and charged the
  whole gap to the link — undoing the stalls-track fix in exactly the interleaving that fix was
  for. `tick` now banks the slept stretch in `dozed_since_arrival` and the arrival that ends the
  silence spends it. The same edit closed a second hole the first did not name: `last_tick_at` was
  only moved by `tick`, so a receiver that was ingesting steadily but not ticking accumulated a
  doze it never took. An arrival is proof the loop ran, so it moves the mark too. Test:
  `a_tick_that_wakes_first_does_not_hand_the_silence_to_the_link` — the timer fires before the
  datagrams and the 200 ms is still not a stall, and the next silence, watched, still is one.

- ✅ **A sleep is forgiven once, and the two shares of a silence are not added** (2026-09-06,
  `crates/slopty-media/src/reassemble.rs`, from Codex's review of the cadence track). Two ways the
  doze credit was wrong. **It was spent again in every report**: the bank belongs to the whole
  silence but a charge only covers the stretch since the last one, so a receiver that slept 200 ms
  and then sat awake in a stall reported the silence once and zero thereafter — and a rate
  controller reads a window with no stalled time as healthy and grows into a link that is still
  holding everything. `charge_stall` now spends what it forgives. **And the host's share and the
  receiver's were added**, though they are not disjoint: the host's silence runs from the last
  arrival, and the receiver may have slept through exactly that stretch. A host quiet for 100 ms,
  then a link holding the next datagram for 100 ms with 75 ms of sleep inside the host's half,
  read as 25 ms of link time instead of 100 and the stall was missed. Only sleep past the end of
  the host's silence is credited now, which needs the doze's start as well as its length. Tests:
  `a_sleep_is_forgiven_once_and_a_stall_that_stays_on_keeps_reporting`,
  `sleep_inside_the_workers_own_silence_is_not_forgiven_twice`; both were checked against a mutation
  of the fix they cover.

- ✅ **BBR3 stays the default congestion controller; the choice is decided by the busy case, and
  a still desktop cannot decide it at all** (2026-09-06, sixteen 90 s runs over the Wi-Fi/mesh
  path, MEASUREMENTS.md "BBR3 vs Cubic over the mesh"). This is the comparison the entry above
  said it was waiting for, and it splits by regime rather than by controller.

  On a **still desktop** the two are indistinguishable on everything the receiver sees — stalls
  165.5 against 166, stalled 18.7 s against 19.0 s, the same arrival-gap-max distribution (each
  has two of six runs past 1.4 s) — even though Cubic's window runs 3× larger at p50 and BBR3
  sits on its four-packet floor for 35.8 % of samples against Cubic's 0.1 %, and even though
  Cubic absorbed roughly twice the packet loss. Nothing binds: a still desktop offers ~1 Mbit/s
  against a 30 Mbit/s ask, so the window the controller picks is a window nothing needs. **The
  loopback ruling above was measured entirely in this regime.** Its numbers stand and its
  mechanism (`ProbeRTT`) is real; what it could not see is that the quantity it optimised does
  not reach the user.

  Put a scrolling terminal on the captured display and they separate at once, in the direction
  the 2026-09-05 ruling claimed and on the path it claimed it for: **Cubic's window collapses to
  13–15 kB under real loss and its target falls to 1.6–7.9 Mbit/s, while BBR3 holds 40–43 kB and
  one run reached the full 30 Mbit/s**, moving more datagrams in both interleaved pairs. That is
  picture quality, not smoothness — delivered fps is 54–56 either way because ScreenCaptureKit
  caps at 60 — and it is the whole reason a 30 Mbit/s target exists. `SLOPTY_CC=cubic` remains
  the escape hatch for a short path with no loss, where the `ProbeRTT` hitch is the only thing
  happening.

  **The strength of this evidence, stated plainly, because a later sweep partly cuts against it.**
  Counting every busy-source pair, BBR3 delivers more datagrams and a higher end target in three
  of four interleaved pairs. The exception is the 20 Mbit/s sweep run, taken after the link had
  degraded to ~600 lost packets per 90 s: there **BBR3** cut to the 1 Mbit/s floor with a 9.5 s
  backlog at cwnd 4 920 and a 5.8 s arrival gap, while Cubic held 5.1 Mbit/s and moved 36 % more
  datagrams. So the claim this ruling supports is narrow: BBR3 is better where the window is the
  binding constraint on a merely-lossy link. Once the link is bad enough that the rate controller
  is cutting to its floor anyway, **both controllers collapse and which one does so in a given
  run is not predictable from the controller** — neither is defensible there on this evidence.
  Settling that needs ≥ 3 runs per cell on a link in one state, which this session could not
  provide: it drifted monotonically worse across the two hours of measurement.

  Method notes worth keeping: runs were **interleaved** because the link drifted hard (23 → 240
  lost packets per run across the twelve still runs, with Cubic drawing the worse half); and the
  **mesh MTU is 1230**, so BBR3's `MinPipeCwnd` floor is 4 920 B here against loopback's 5 808 —
  the same four packets, so the two pages' floor figures must not be compared as bytes.

- 🔬 **The two controllers fail in opposite ways, and `held_ms` is one instrument reading both**
  (2026-09-06). `held_ms` in hostd's pump is how long QUIC's datagram send buffer went without
  emptying — not one datagram's delay — so a long hold means two different things. **BBR3
  starves**: 43.8 s and 33.8 s holds peaking at 37 kB with cwnd at 4 920, which is ~2 Mbit/s
  through four packets at a 20 ms rtt, delivered to the user as 269 stalls whose worst gap is
  186 ms — a drizzle, not a freeze. The 37 kB peak is `frame_fits`' 32 kB `HELD_FLOOR` plus a
  frame in flight, so the capture guard was dropping frames throughout. **Cubic overshoots**:
  1.7 s and 1.9 s holds peaking at 267 kB and 347 kB, each one refresh keyframe admitted while
  the buffer was under the 32 kB floor and then draining through a 10–17 kB window. `frame_fits`
  gates a frame *before* it is encoded, so it bounds what is queued ahead of a keyframe and never
  the keyframe itself. Read a hold's `max_bytes` beside its `held_ms` or the number means
  nothing.

- ✅ **`send_ms_lo` stays one byte; no protocol widening** (2026-09-06, the mesh case the stalls
  ruling left open). Across ~24 minutes of mesh streaming and ~2 800 released stalls: `stamp
  wrapped 0` and `stamp backwards 0` everywhere, and exactly one gap past the 256 ms range —
  2.016 s, `host_gap=0ns`, `stamp=Absent`, charged whole to the link, which is the right party
  (the host had a 1 979 ms send backlog in that run). The reason is structural rather than luck:
  a wrap needs a datagram to *arrive* carrying a stamp more than 256 ms old, and when the link
  holds everything for two seconds nothing arrives to carry one, so the case lands on the
  already-pessimistic `Absent` path. Wrapping would need a link that delays past 256 ms without
  reordering and keeps delivering; this path does not do that. PROTOCOL_VERSION 15 stays free.

- ✅ **Initial congestion window 32 packets stands, and buys nothing visible on Wi-Fi**
  (2026-09-06, five interleaved starts per window over the mesh). Median first-decoded 587 ms at
  IW 32 against 581 ms at IW 10, inside a 526–1 144 ms spread within each condition. The
  arithmetic is unrefuted — a 58.7 kB keyframe is 49 packets, 5 windows at IW 10 and 2 at IW 32,
  ~33 ms at this path's 11 ms rtt — but start-up here is dominated by the 217–304 ms to the first
  datagram and 40 ms of encode, not by the window. Keep it for LAN and loopback, where it is
  cheap and the arithmetic is the same; do not claim a Wi-Fi start-up win for it.

- ✅ **The capture guard's held-bytes budget is two frames at the rate in force, floored at one
  congestion window; the fixed 32 kB floor is gone** (2026-09-14, from the 2026-09-06 mesh
  measurement above). `HELD_FRAMES` was always a *time* budget written in bytes: at 30 Mbit/s and
  60 fps it is 125 kB, at the 1 Mbit/s floor 4 kB. `HELD_FLOOR` pinned it at 32 kB from below, so
  as the path slowed the budget became a queue measured in seconds — 33 ms of standing latency at
  8 Mbit/s, 145 ms at the 1.8 Mbit/s the mesh's collapsed window sustains, charged to every frame
  behind it. The 🔬 entry above measured exactly that: BBR3 holding 37 kB, which is the floor plus
  a frame in flight, with the guard dropping frames the whole time. The floor's stated reason —
  that a low target must not drop every frame behind a beat — is the wrong medicine: at 1.8 Mbit/s
  a link cannot carry 60 fps, and turning a drop into a 145 ms delay is the worse of the two
  answers for a remote desktop.

  **This is the BBR3 half of that entry only.** The Cubic half is barely touched: a 267–347 kB peak
  under a 32 kB floor means the keyframe itself was 235–315 kB, and at Cubic's collapsed
  1.6–7.9 Mbit/s target with a 10–17 kB window the new limit is 13–33 kB — within a few kB of the
  floor it replaces. Sizing the budget correctly does not answer a frame that is ten times the
  budget whatever the budget is; only the keyframe rule below does.

  One congestion window replaces it as the floor. Bytes inside the window are not a standing queue
  — QUIC sends them on the next acknowledgement — so refusing a frame while `held` is under `cwnd`
  drops a frame the link would have carried. That case is reachable rather than theoretical: two
  frames fall under one window once `rtt × fps` passes 1.8, which at 60 fps is any path past a
  30 ms round trip. (Amended 2026-09-25: the stream now asks its `DatagramSink` for the window on
  each captured frame, and only while something is held; the pump below is gone.)
  `DatagramBudget` carried the window beside the held bytes and hostd's pump
  samples the selected path when a hold starts, when it deepens, and on a poll turn — one that woke
  on `HOLD_POLL` with no datagram to send, which is exactly a hold that is long and quiet, the only
  case where the reading would otherwise go stale. Not on every turn: reading the path takes the
  connection lock the QUIC driver wants, and one 30 Mbit/s frame is ~50 datagrams.

  The NACK path shares the budget and so refuses retransmits earlier than it did. That is the
  intent rather than a side effect: a fragment pushed into a queue that is already two frames deep
  arrives after the frame it would have completed is stale, and the client's own refresh request
  comes back through the capture guard, which is the path that can actually answer it.

  Sampling during BBR3's `ProbeRTT` cannot hurt: `cwnd` only ever raises the limit, and the
  bitrate the other term comes from is capped by the *widest* path sample of its window, so a
  200 ms dip to four packets narrows neither term.

  **Measured on a good path, still owed on a bad one.** The 2026-09-06 mesh run is what licenses
  this. The 2026-09-14 run (MEASUREMENTS.md) got a real 20 s stream at last — the host has to run
  under launchd for TCC to grant it capture at all — and read 57.9 fps, 0 stalls, 0 lost, 1236
  holds at p50 2 ms with a worst of 15 ms on the 74 kB keyframe. It settles the cheap half: on a
  9.5 ms link `per_frame × 2` is 56–125 kB while `cwnd` never fell below 23.7 kB and the old floor
  was 32 kB, so neither floor is ever the larger term and the two builds admit the same frames.
  The change is inert on a good path; nothing was traded away for the collapsed-path win.

  The expensive half is untouched: `per_frame × 2` only falls under one window once `rtt × fps`
  passes 1.8, which a 9.5 ms link cannot reach. That needs a day like 09-06 (25–30 % loss, the
  window pinned at its floor, the target cut to 1 Mbit/s) or a conditioned link. Prediction
  unchanged: `max_bytes` peaking near `cwnd + one frame` rather than 32 kB + one frame, bought with
  more frequent short drops. Interleave the arms — the mesh drifted monotonically over two hours on
  09-06, and on 09-14 the second machine left the mesh mid-run, which costs a whole arm either way.

- 🔬 **Gating a keyframe on its own headroom** (2026-09-14, measured 2026-09-15). The Cubic half
  of the 🔬 entry — `frame_fits` runs before the frame is encoded, so it bounds what is queued
  *ahead* of a keyframe and never the keyframe itself — is only partly answered by the budget
  above, which shrinks the peak without removing it. A keyframe rule needs a safety valve (admit
  it after N refusals or T ms) or a permanently congested link never gets one and the client sits
  on a hole for good, which is worse than the stall it would prevent. Out of scope either way: at
  2 Mbit/s 60 fps HEVC is not viable whatever the guard does, and the answer there is frame rate
  adaptation.

  **The collapsed link has now been measured** (MEASUREMENTS.md, the shaped ladder), and it
  answers the question this was deferred on. The hold after a keyframe is not hundreds of
  milliseconds: the worst episode held **435 kB for 4.8 seconds**, with `cwnd` at 261 kB. The rung
  it came from is a 600 kB/s link at 3 % loss where the rate controller is pinned at its 1 Mbit/s
  floor and the guard is already dropping 99 of 202 captured frames; 4.8 s of one frame's fragments
  on a link in that state is worse than the hole the client would have sat on. Implement the rule
  with the valve; the number to beat is that 4 796 ms.

  **Correction (2026-09-15): 435 kB was never one frame, and the sentence that said so is
  withdrawn.** This entry read the hold's `max_bytes` as a keyframe's size and concluded "the frame
  overshot the window by 175 kB, so no sizing of `max(per_frame × 2, cwnd)` reaches a frame that
  large. It is the keyframe itself." The host's own log refutes it: across the whole ladder the
  encoder produced 34 keyframes and the largest was **133 960 B**, the range 87–134 kB. `max_bytes`
  is peak bytes *held* in QUIC's send buffer, which is the standing queue plus the admitted frame
  plus its parity — at 3 % loss the redundancy controller is not cheap — so 261 kB of window plus a
  134 kB keyframe and its parity lands within a few tens of kB of the 435 kB measured. The budget
  was doing exactly what it was sized to do.

  What survives is the reason for the rule, on better numbers than the wrong ones. A 134 kB
  keyframe at the 1 Mbit/s floor is **1.07 seconds of link** by itself, and `frame_fits` still
  cannot see it: that gate runs before the encode and bounds the queue ahead of a frame, never the
  frame. The rule is right; the arithmetic that motivated it was not. (Second time this session a
  conclusion was drawn from a number that measured something adjacent to what it was read as; the
  first is in `docs/decisions/testing.md`.)

- 🔬 **A keyframe the link cannot drain is deferred for an LTR refresh** (2026-09-15,
  `keyframe_fits`). What makes the deferral affordable is that there is something to send instead.
  Refusing a keyframe and sending nothing does not help: the frame does not get smaller while the
  host waits, and a client with a hole and no frames is exactly the failure the valve is there to
  prevent. An LTR refresh is a picture decodable from a reference the client has already
  acknowledged, at a fraction of a keyframe's bytes, and the encoder already takes it as
  `force_ltr_refresh` — the drop path has been asking for one since the frame budget landed. So the
  rule is a substitution rather than a refusal: while the estimate says a keyframe will not drain
  inside 400 ms at the rate in force, the request stays pending and each due frame goes out as a
  refresh.

  Three things keep it honest. A stream that has never had an LTR acknowledged is never deferred —
  a refresh would decode into nothing, and that client is the one the first keyframe exists for.
  The estimate rises to an observed keyframe at once and decays by an eighth, so one cheap keyframe
  (a blank screen, a window scrolled off) cannot license the next expensive one. And the valve is a
  wall-clock second from the start of a run, not a refusal count: a count is read at capture rate,
  so the same second would be 60 refusals at 60 fps and 8 at 8 fps, which is a valve that opens
  sooner exactly where the link is worst. One second against the measured 4 796 ms, with a picture
  going out throughout.

  400 ms is the drain budget because it separates the rungs cleanly on the sizes the encoder
  actually produced (87–134 kB, from the host log of the ladder run). 30 Mbit/s carries 1.5 MB in
  400 ms and the `lte` rung's 9 Mbit/s carries 450 kB, so neither ever defers; the 1 Mbit/s floor
  carries 50 kB, so the collapsed rung refuses a 134 kB keyframe — 1.07 s of that link in one
  frame — by a factor of nearly three. The rule fires on exactly the one rung that earned it.
  `keyframes_deferred` counts episodes, not frames, and is in `ScreenStats` for the ladder to read.

  **Measured, and it never fires** (2026-09-15, second shaped-ladder run). Across five rungs the
  host logged **zero** deferrals. The rule is guarded correctly and it is not dead code in general,
  but in the scenario it was written for it does not run, so it is not the fix for the 4 796 ms
  hold and must not be recorded as one. Two things explain it, and both were assumptions rather
  than measurements when the rule was written.

  *Nothing requests a keyframe on a collapsed link.* `pending.keyframe` is set in exactly two
  places: when a stream opens, and when a quality change rebuilds the encoder. Neither happens
  while a link is collapsing. The drop path sets `pending.refresh`, never `pending.keyframe` — so
  the gate sits on the one request type that rung never produces.

  *The expensive frames are refreshes answered as keyframes.* The encoder runs an infinite GOP
  (`MaxKeyFrameInterval` is `i32::MAX`, "keyframes only on request"), so every keyframe is one
  somebody asked for — and the run still produced 28 of them, 20–124 kB. They came from
  `force_ltr_refresh`, which VideoToolbox satisfies with an IDR when it has no acknowledged
  long-term reference to refresh from. The guard sets a refresh on *every* dropped frame, and the
  collapsed rung drops 99 of 202 captures, so that rung asks for refreshes continuously and is
  handed keyframes for them.

  One more premise was wrong: keyframes *do* shrink with the target bitrate. This run's ran 124 kB
  near the top and 20 kB once the controller pinned at the 1 Mbit/s floor, tracking the rate down.
  The entry above says the encoder sizes a keyframe "never from what the path can carry right now";
  that half is withdrawn. It sizes them from the average rate, which the controller is already
  lowering.

  The collapsed rung did read better this run (70 frames decoded against 52, p50 gap 97 ms against
  142 ms). **That is not this change.** The gated path provably never executed, so the difference is
  run-to-run variance in desktop content — worth stating plainly, because a green number next to a
  new feature is exactly how a rule with no effect gets believed.

  **Where the real rule goes.** At the refresh request, not the keyframe request. Measured
  2026-09-15 (MEASUREMENTS.md, "the LTR reference is starved"), and the answer was neither of the
  two candidates. The acknowledgement path works: all six streams of a ladder run logged a `first
  LTR ack` within their first second. The encoder honours it: with a fresh acknowledged reference a
  forced refresh is 733 B against a 3 998 B IDR (`a_forced_ltr_refresh_is_a_delta_not_an_idr`). And
  the same run still produced 33 keyframes, far more than the two `pending.keyframe` sites can
  account for.

  What is wrong is the *supply*. VideoToolbox offers acknowledgement tokens sparsely — one per
  fifteen frames in the codec test, one per eight-second stream live — so a stream holds a single
  reference, and a refresh asked for seconds after that reference was taken is answered with an
  IDR. A refresh is therefore cheap or ruinous depending on how stale the reference behind it is,
  and nothing in the host currently knows which. `ltr_acked` does not: it records only that an
  acknowledgement once happened, which is why it is the wrong guard to have built the rule on.

  Two ways out, and the first is the root cause. Either keep more references live and acknowledged
  so a refresh always has a recent one to work from, or accept that a refresh is a keyframe in
  disguise and put the drain budget and valve on the refresh request. The first needs an answer to
  whether the token rate can be influenced at all — `EnableLTR` is a plain bool and the encoder
  chooses its own LTR frames, so that may not be ours to set.

- ✅ **The bad link is a UDP relay the two ends speak through** (2026-09-15, `slopty-shape`;
  2026-09-24: the pinning below is gone with iroh — plain QUIC dials the shaper's address and
  stays on it). Three
  rulings wait on a link that collapses on demand: the cadence ladder's 8/12 kB rungs, the keyframe
  rule above, and the audio jitter estimator. The kernel's shapers (`dnctl`, `pfctl`) want a
  password this process does not have, and a measurement that cannot run unattended does not run.
  So the impairment goes in a process of our own: the client's pair ticket carries the host's node
  id and the relay's address, every datagram passes through `Shaper::admit`, and QUIC sees a queue,
  a delay and a loss rate it cannot tell from a real bottleneck.

  Two alternatives were rejected. `SLOPTY_E2E_DROP_PERMILLE` drops frames *above* QUIC, so the
  congestion controller never learns anything was lost and the arm measures the wrong layer
  entirely. `iroh`'s `add_custom_transport` sits at the right layer but takes a whole
  `CustomEndpoint`/`CustomSender`/`CustomAddr` implementation — a new address type in the product
  so that a test can be slow, when a 400-line process outside it does the same job.

  Two things the model gets right on purpose. Packets leave in the order they arrived, one sender
  task per direction: a task per packet lets a small jitter draw overtake a large one, and quinn
  declares loss after three out-of-order packets, so a reordering shaper would report congestion on
  a clear link. And the queue drains forward only (`drains_at.max(now)`) — a link left idle does not
  owe the next packet the transmission time it did not use.

  Pinning the path is what makes the numbers mean anything: if hostd's real address ever reaches
  the client, iroh migrates off the relay and the run is silently unshaped. Two tries at this were
  wrong, and the test is the only reason that is known. Turning relays off does nothing — the first
  run moved to `192.168.100.240` in about a second, with `PathId(2)` and `PathId(3)` probing a
  Tailscale address on the way. Skipping mDNS does nothing either: the addresses travel over the
  connection itself. Holepunching is the mechanism, and iroh 1.2 refuses to be told to stop.
  `quic.rs:472-480` clamps `max_concurrent_multipath_paths` to a minimum of 13 and `:537-540`
  clamps `max_remote_nat_traversal_addresses` to 8, each ignoring a smaller value with a warning.
  There is no configuration that pins a path.

  What works is giving each end nowhere else to go. `Local::Pinned` binds one address with the
  prefix set to the address itself and the default route off, and `poll_send` blackholes a datagram
  once no transport matches and none is default. So hostd binds its LAN address (`--bind`), the
  client binds loopback (`bind_pinned_client`), and each one's probe at the other's real address is
  dropped before it leaves: the path never validates and the relay stays selected. It is also a
  feature worth having outside a measurement — a host reachable only through a local tunnel. The
  run checks itself as well as pinning: `selected_addr` reads the address the selected path is
  sending to, `a_shaped_link_keeps_the_client_on_the_shaper` asserts it over a real session on a
  30 ms link, and each rung of the ladder asserts it again.

  The ladder runs against the *installed* host, which is not a preference: TCC attributes a
  shell-spawned daemon to whatever launched it, so a test that spawns its own is refused capture
  with -3801 however the binary is signed. `SLOPTY_E2E_WORKER_SOCKET` points it at the launchd
  host's control socket, and it checks that host is pinned before streaming rather than after.
  Every other `SLOPTY_SCREEN_E2E` test still spawns its own daemons and so cannot capture either;
  that is older than this change and not fixed here.

- ✅ **A dropped frame asks for a refresh only when the link could carry one.**
  The keyframe drain budget landed in the wrong place. It guards `pending.keyframe`, which is
  set at stream open and on a quality change and almost nowhere else — `keyframes_deferred` read
  0 on all five rungs of the shaped ladder, so the rule never once fired. The path that runs on
  a collapsed link is the *drop* path, and it asked for a refresh on every frame it dropped.

  That ask is not cheap here. A forced LTR refresh comes back from VideoToolbox as a full IDR
  (133 960 B measured), not the 733 B delta it produces with a fresh long-term reference on hand,
  because the encoder offers about one LTR token per stream and the single reference goes stale
  — the finding recorded above. So a link already too slow for ordinary frames was being asked
  for keyframes twenty times their size, each of which filled the queue and caused the next drop.
  That is the shape a multi-second stall would take: not one bad frame, a loop.

  `Shared::dropped` now counts the drop and puts the refresh request through
  `keyframe_admitted`, the same drain estimate and one-second valve the keyframe rule uses. Where
  the link cannot carry a picture that stands on its own, the client holds a stale one for a beat
  instead. Before the first acknowledged reference nothing else decodes, so the ask goes out
  regardless; the valve keeps a badly-estimating link from holding the picture forever.

  The client's own `request_refresh` — sent when it reports a loss it cannot recover — is *not*
  gated. It is the same IDR and arguably the same loop, but there the client has said it is stuck
  rather than the host having guessed, and one change measured beats two changed together.

  The valve resets, which is what makes the rule bite rather than merely delay: `keyframe` on a
  packet is `!NotSync`, so the IDR a starved refresh produces is flagged as the keyframe it is,
  and `on_packet` zeroes `keyframe_deferred_us` on it. A collapsed link therefore carries at most
  one of these a second instead of one per dropped frame.

  Test: `a_dropped_frame_asks_for_a_refresh_only_when_the_link_could_carry_one`, over the wiring
  the keyframe path never reached.

  **Measured over three runs against one baseline** (`docs/MEASUREMENTS.md`, 2026-09-15, the
  drain budget on the refresh request). On the collapsed rung the p90 arrival gap halves, 242 ms
  to 118–129 ms across all three; the guard drops 45–48 frames of ~195 where it dropped 99 of
  202; about 35% fewer packets go into the 600 kB/s pipe and about half as many are lost; the
  worst host hold falls 4 796 ms to 1 708 ms. Keyframes over the whole ladder fall 28 to 20, and
  `keyframes_deferred` reads 4, 1 and 3 where three earlier ladders read 0 on every rung — the
  rule executing, not variance.

  Three runs and not one because this rung is noisy: its own baseline records it moving 52 to 70
  decoded with no code change. `first decoded` and `gap max` stay noisy here and no claim rests
  on them.

- ✅ **Plaintext QUIC on noq, standalone; iroh removed** (2026-09-24). Every host is now reached
  over Tailscale, WireGuard or a VPN, which already encrypt and authenticate the path, so the
  transport drops what iroh brought for the open Internet: TLS, endpoint keys, relays,
  holepunching, multipath (whose clamps `Local::Pinned` fought), pkarr/DNS discovery and pairing
  tickets. What stays is QUIC itself: reliable streams, unreliable datagrams, congestion control
  and migration on one UDP flow.

  *Which QUIC.* noq 1.3.0 (n0's quinn fork, already compiled in under iroh) used standalone,
  over upstream quinn 0.11. Read from source in the registry: noq-proto's `crypto` traits have
  quinn's shape (plus a `PathId` argument) and take a provider that does nothing, and
  `EndpointConfig::new(reset_key)` / `ServerConfig::new(crypto, token_key)` avoid the ring
  feature. The decider is congestion control: BBR3 is noq's, it has been the default since
  2026-09-05 on measurements (the entries above), and quinn 0.11 ships only an experimental BBR,
  so moving would have changed the controller every video ruling was measured under. Multipath
  and NAT traversal are off in a standalone noq by default; nothing had to be turned off.

  *The provider* (`slopty_net::crypto`). Its own QUIC version, `SLP1`, so a standard QUIC stack
  gets a version negotiation rather than plaintext it would read as garbage. The handshake is
  one round trip: each side's `HELLO` (a magic, a length, the transport parameters as QUIC
  encodes them) in Initial, then a one-byte `FINISHED` in Handshake each way. The FINISHED
  messages are not decoration: noq moves to the next packet space only when `write_handshake`
  hands out keys, and marks a connection established when a Handshake packet arrives after the
  session stops handshaking, so each side must put bytes in Handshake. Packet keys copy (tag 0,
  limits `u64::MAX`, `next_1rtt_keys` always `Some`), header keys leave the header (sample 0),
  the retry tag is 16 zero bytes that `is_valid_retry` accepts (the server never retries and
  sends no address-validation tokens), `export_keying_material` is an error. `read_handshake`
  returns a transport error for anything that is not ours — a TLS ClientHello, a bad parameter
  block, an over-long block, trailing bytes — and never panics (unit tests, one per case). The
  reset and token keys are a keyed SipHash with one constant key for every Slopty process: a
  restarted host answers the old connection's packets with a stateless reset the client
  recognises, instead of leaving it to the 15 s silence bar.

  *Tuning carried over, and what is new.* `MAX_ACK_DELAY` 2 ms through ack frequency, the
  32-packet initial window, BBR3 by default with `SLOPTY_CC` / `SLOPTY_QUIC_IW` overrides,
  keep-alive 5 s, idle 45 s, migration on (the server default), `fast-apple-datapath` off. New:
  `initial_rtt` 5 ms (RFC 9002's 333 ms is for an unknown path and set the first handshake
  retransmission), and an MTU discovery ceiling of 1252 (Tailscale's TUN MTU of 1280 less 28
  bytes of IPv4 and UDP; superseded 2026-09-30 by **Every packet is 1232 bytes**). One ask was not taken: a datagram send buffer of about one frame. noq
  drops the *oldest* datagram when that buffer is full and a frame is pushed in one burst, so a
  buffer smaller than a keyframe would cut the head off every keyframe; the standing queue is
  already held to two frames by the capture guard (`frame_fits`), which drops whole frames.
  The buffer stays 4 MiB, a ceiling for a burst rather than a queue.

  A host binds one dual-stack socket on `[::]` (IPv4 peers arrive as `::ffff:a.b.c.d` and are
  canonicalised before admission) or the address `--bind` names. The endpoint and provider know
  nothing of roles, so the worker ↔ server link can reuse them. Measured (MEASUREMENTS.md
  2026-09-24): on the same LAN address the control-stream round trip median goes ≈1.2 ms →
  ≈0.55 ms and connect-to-`HelloAck` ≈30 ms → ≈11 ms; loopback connects in 7–9 ms.
  `PROTOCOL_VERSION` 50 (`Hello.pair_token` and `Rejection::NotPaired` gone, `HelloAck.worker`).

  🔬 Still owed on this transport: BBR3 against Cubic, and a ~2 MB Cubic initial window that
  would leave a per-frame HEVC burst unpaced under the 256-MTU pacer cap. The shaped ladder is the
  instrument and it runs only against a launchd-installed host; installing this branch would have
  replaced the host in use, so it was not run.

- ✅ **Media datagrams go to QUIC from the thread that made them** (2026-09-25). A stream hands
  its datagrams to a `DatagramSink` (`slopty-worker`'s `QuicSink` over the connection) from wherever
  they were produced: a frame's worth in one `send_many_datagrams` call from VideoToolbox's
  callback, audio from the capture queue, heartbeats and cursor samples from their tasks. The pump
  it replaces put every datagram through a 4096-slot channel into a task that read the send buffer
  and the datagram size beside each `send_datagram`, three connection locks per datagram, and
  polled at 1 kHz while QUIC held bytes. The capture guard now reads the held bytes from the sink
  on each captured frame and the window only while something is held, so there is no second
  queue for a datagram to wait in and nothing to account for it. A datagram larger than the path
  carries is refused by QUIC as `TooLarge`: that is the packetizer having cut for a size the path
  had a moment ago, so the batch's datagrams that fit still go, the rest are counted in
  `ScreenStats::queue_full`, it is logged once per stream, and the stream goes on. The pump
  ended the connection's media on any send error, `TooLarge` included. Only a closed connection
  stops a stream's loops now. Measured (MEASUREMENTS.md, "datagrams from the encoder's thread"):
  median process CPU per 64-datagram frame on loopback 3.43 → 2.47 ms over five alternating
  pairs; a frame's arrival did not measurably change on loopback (p50 medians 0.86 and 0.96 ms),
  where the pump was seldom behind. `datagram_send_cost` stays as the instrument;
  its pump twin went once this comparison was recorded (2026-09-28).

- ✅ **A connection's loop only routes; anything slow runs beside it** (2026-09-25). Everything
  a client sent used to be handled inline in one `select!`: a window stream's open (115–300 ms),
  its close, an encoder rebuild, the 100 ms geometry tick with its window-server reads, a session
  open waiting on ptyd, quick open walking 20 000 files, file reads and polls, and the pasteboard
  write behind a paste. A terminal's keys waited behind all of it. Now each screen stream has a
  task of its own that takes its commands in order (open, input, focus, quality, resize, close)
  and runs its geometry probe on the blocking pool beside them; loss feedback and reports reach
  the stream's `StreamControl` straight from the loop; session open and close, quick open, file
  reads, hook installs and transfer resumes are spawned and report back; the watched files have
  their own task. A paste chord holds input only for the windows it went to, in order, while the
  clipboard is asked, fetched and written off the runtime; other windows and every terminal carry
  on. `echo_is_not_held_behind_slow_requests` (`slopty-worker` e2e) keeps it so: keystroke echo
  under a stream of quick opens and window opens on the same connection, p99 against half a quick
  open. Measured (MEASUREMENTS.md, "echo behind slow requests"): echo p99 33.3 → 4.8–5.3 ms under
  that load, p50 28.7 → 2.8 ms. `slopty_worker::file::read` also refuses what is not a regular file
  before opening it: a named pipe would wait for a writer on a blocking thread for good.

- ✅ **The connection's loop never awaits; slow work and slow clients get tasks of their own**
  (2026-09-25). After the earlier split the loop still awaited five things inline: the uni
  stream an attach opens, which waits for as long as the client withholds stream credit; every
  send into the control queue (pongs, errors, rates, clipboard fetches, and each broadcast
  event), which waits on a client that reads slowly; a receiver report's rate change, which
  calls VideoToolbox and can wait on the encoder's lock during a rebuild; an upload's session
  directory, an actor snapshot and then a ptyd probe; and the clipboard fetch. A key for any
  terminal on the connection waited behind each. Now the loop only routes. The actor gets the
  attach's sink at once, and a pump task opens the stream within
  `slopty_net::streams::SESSION_STREAM_WAIT` (10 s, `NetError::TimedOut` past it) and then
  detaches the sink and tells the client. The worker's events go to the client from a relay
  task. It waits on the client alone and resyncs when it lags. Reports go through a bounded
  queue (64; a full one drops the report, and each report stands alone) to a task that applies
  them on the blocking pool. The upload's directory is asked on a task with a 2 s limit, since
  the files already wait for `Begin`. What the loop itself answers goes in with `try_send`,
  and a client that has left a thousand messages unread loses that answer, not the other
  terminals. The connection also subscribes to the events before it reads the greeting, so an
  event between the two is no longer lost, and drops what the greeting already told (a
  `SessionOpened` it listed, an item delta its snapshot holds). Tests:
  `a_terminal_waiting_for_its_stream_holds_no_other_terminal` and
  `a_client_that_stops_reading_its_control_stream_still_types` (`slopty-workerd` e2e; both hang
  on the old loop), `a_session_stream_the_client_never_allows_times_out` (`slopty-net`),
  `an_event_the_greeting_carried_is_not_told_again`. Measured (MEASUREMENTS.md, "nothing on the
  connection waits behind anything slow"): echo p99 under 1.7 ms in both, where the old loop
  gave no echo in 20 s.

  🔬 Not taken: widening the audio lane to "while someone types". noq fills packets with
  datagrams before stream data, so an echo waits behind whatever datagrams QUIC holds. Over a
  20 Mbit/s shaper the lane's 5 ms slice cuts that from 78 to 99 kB at p99 to 12 kB, and the
  echo is no faster. BBR3's window there ran into megabytes, so the burst moved into the
  bottleneck queue and the echo waited in that queue instead. Metering the lane below the link
  rate while someone types did cut the echo's p99 (15 to 19 ms against 25 to 36 on quiet runs).
  That costs a keyframe its delivery time, so it needs the rate controller's number and a
  shaped-ladder run on hardware before it lands. BBR3's window on the shaper is part of the
  BBR3 against Cubic comparison still owed above.

- ✅ **A tunnel ends both ways when either side fails; a failed accept keeps the port**
  (2026-09-25). When nothing listened on a pinned port, the worker reset the stream, but the
  client kept waiting on the browser's socket, so the browser hung. When the browser reset a
  long poll or a websocket, the other direction waited on the worker for good and leaked a
  task and a stream. `splice` now runs both directions under `try_join!`. A failure on the
  stream side closes the browser's socket with a zero linger, which the browser sees as a
  reset. A failure on the browser's side resets and stops the stream. A clean end of one
  direction still half-closes it and the other goes on. The accept loop used to return on
  any error and never bind the port again. It now retries a connection-level failure at once
  and pauses on anything else, 10 ms doubling to 1 s while the failures last. That covers
  EMFILE, ENFILE and ENOBUFS. The loop never ends by itself; it stops when the forward is
  dropped, as hyper and axum do. Tests: `a_stream_the_worker_resets_resets_the_browser`,
  `a_browser_that_resets_resets_the_stream`, `a_failed_accept_keeps_the_port_served` (each
  failed on the old code) and `clean_ends_half_close_each_way` (`slopty-client`).

- ✅ **BBR3 stays the default, with its window held to twice the measured bandwidth-delay
  product** (2026-09-25, MEASUREMENTS.md "BBR3's window under bursty video"). This settles the
  shaper half of the 🔬 BBR3-against-Cubic item above and the window question in the "loop never
  awaits" entry. The mesh half, and the ladder on hardware, are still owed.

  *Why the window ran to megabytes.* noq-proto 1.3.0's BBR3 counts every delivered byte as ACK
  aggregation once a flow restarts from idle each frame: `handle_restart_from_idle` moves the
  aggregation interval to now without clearing the bytes counted in it, and the window
  (`2 × max_bw × min_rtt + extra_acked`) grows by the video's own rate. Video that never fills
  the link also stays in `ProbeBW_UP` at pacing gain 1.25 (the exit waits for `full_bw_now`,
  which app-limited samples never set), and it can stay in `Startup`, pacing at 106 MB/s. With
  a window nothing binds, the pacer alone sends each burst, and above the link rate it lands in
  the bottleneck's queue.

  *The bound* (`slopty_net::congestion::Bounded`, wrapping whatever `SLOPTY_CC` picks). The
  window is `min(noq's, 2 × delivery rate × min RTT)`, never below 16 packets. Both inputs are
  measured in the wrapper, since BBR3's model is private and is what went wrong: the fastest the
  peer acknowledged data over at least one round trip, and the smallest per-packet round trip,
  each the best of the last five to ten seconds. On the 20 Mbit/s shaper it cut the bottleneck
  queue's p99 from 16.7 to 10.9 ms (maximum 26.5 to 12.1) with bursts and from 17.8 to 11.9 ms
  with the lane, at the same 12.5 Mbit/s and the same frames delivered whole. When the link
  halves under the sender, noq's BBR3 filled the 100 ms buffer in two of four runs and echo p99
  reached 94 to 155 ms; bounded, the queue stayed under 22 ms with nothing dropped and echo p99
  was 32 to 43 ms. On a 20 ms buffer the bound took BBR3's overflow losses from 75 to 0.
  `SLOPTY_CC=bbr3-unbounded` (any choice with `-unbounded`) is noq's controller as it ships, for
  measuring against. The window also feeds the media rate controller's cap (`0.9 × cwnd / srtt`),
  which now reads about 1.8 × the measured delivery rate instead of nothing.

  ❌ *Cubic, bounded or not.* Unbounded Cubic reports no pacing rate, so a burst leaves at once:
  on the 20 ms buffer 950 packets overflowed a run and no keyframe arrived whole. Bounded, it
  filled the buffer when the link halved (199 ms, 312 dropped), because Cubic never drains a
  queue, the minimum round trip ages into the queued one, and the bound rises with it. Its keyframe
  p99 is better (56 to 78 ms against BBR3's 130 to 200, which is `ProbeRTT`), and that is the
  one thing it wins.

  🔬 *What no controller can do here.* A keyframe is 52 ms of a 20 Mbit/s link, and an echo
  written behind it waits for it: at the bottleneck if it was paced above the link rate, in QUIC
  if below, since noq writes datagrams before stream data (`connection/mod.rs:6504` before
  `:6562`). The bound removes the controller's own queue, not that one. The rest of the fix
  sits in noq:
  1. Write stream frames before datagrams (`populate_packet`, the `DATAGRAM` block ahead of
     `STREAM`). Done in Slopty's noq, see "Control and session streams go ahead of datagrams"
     at the end of this file.
  2. In `handle_restart_from_idle` (`bbr3/mod.rs:1266`) also zero `extra_acked_delivered`, as
     Linux does on `CA_EVENT_TX_START`, and cap `extra_acked` at some milliseconds of `max_bw`
     in `update_max_inflight` (line 1350; Linux uses 100 ms).
  3. Let an app-limited flow leave `ProbeBW_UP` after a round trip at `inflight ≤ 1.25 × BDP`
     (Linux BBRv3's probe-up exit), and skip `ProbeRTT` for a flow that was app-limited through
     the interval, whose round trips are unqueued already. That takes BBR3's keyframe p99 back
     toward Cubic's.
  With 2 and 3 upstream the bound should stop binding, and it can go.

  Owed on real links: the shaped ladder on hardware and a mesh session with this default, bound
  against `bbr3-unbounded`, watching the rate controller's cap and keyframe delivery. The shaper
  has no AQM and releases packets on 1 ms ticks, so a Wi-Fi link's aggregation or an fq_codel
  router may move the numbers.

- ✅ **Control and session streams go ahead of datagrams; tunnels and files stay behind them**
  (2026-09-25, MEASUREMENTS.md "an echo ahead of the datagrams"). noq-proto 1.3.0 writes every
  queued DATAGRAM frame that fits before any STREAM frame, so an echo written behind a keyframe
  left after it. Slopty patches noq with `TransportConfig::stream_priority_before_datagrams`.
  With `Some(threshold)`, `populate_packet` writes STREAM frames for streams at or above the
  threshold first, then datagrams, then the rest. Priority order and `send_fairness` hold inside
  each group, and `None` is noq's order. Neither noq nor quinn had an issue or PR on the
  ordering. The nearest is noq#816, on hierarchical stream scheduling. The patch lives
  in `vendor/noq-proto`: the published 1.3.0 crate plus the one commit, wired in with
  `[patch.crates-io]`. Only `noq-proto` changes, and `noq` and `noq-udp` stay on crates.io.
  It is vendored rather than forked because this change is one crate and 178 lines, and
  `vendor/noq-proto/SLOPTY.md` says how to carry it to the next release. It is small and
  opt-in, so it is worth offering upstream.

  Slopty's threshold is `streams::AHEAD_OF_DATAGRAMS`, 0, which is where the control and session
  streams sit. Tunnels moved from 0 to `TUNNEL_PRIORITY`, −1, and files from −1 to
  `BULK_PRIORITY`, −2. A download through a forwarded port has no bound but the link, and ahead
  of the datagrams it would starve video. A session stream is bounded by the terminal's own
  credit, and the control stream carries small messages. On the 20 Mbit/s shaper, ten
  interleaved runs a side, the bursts arm's echo p99 went from 24.6 to 19.0 ms (median of runs)
  and the halved arm's p50 from 25.3 to 21.0 ms. Keyframe times, goodput and frames delivered
  whole did not move. `SLOPTY_DATAGRAMS_FIRST=1` restores noq's order, to measure against.
  `a_session_frame_overtakes_queued_datagrams` (`slopty-net`) fails without the change.

  noq's old order was never strict. When the head datagram does not fit, as in a packet that
  carries an ACK, the rest of the packet takes stream data of any priority. So files could
  already slip ahead of video now and then, and still can.

  Worth proposing upstream: it is opt-in, keeps the default, and is a few lines. Upstream may
  prefer to give datagrams a priority of their own in the stream order, which says the same
  thing.

- ✅ **noq's BBR3 follows the draft: clear the aggregation count on idle restart, mark ProbeRTT
  app-limited, and leave ProbeBW_UP and the ProbeRTT dip as specified** (partly superseded
  2026-09-30 by **BBR3 on app-limited video**: Linux's aggregation cap is taken, and ProbeRTT
  is skipped for a flow that goes quiet on its own) (2026-09-25,
  MEASUREMENTS.md "noq's BBR3 against the draft"). This settles items 2 and 3 of the
  "BBR3 stays the default" entry. Checked against draft-ietf-ccwg-bbr-06 and its editor's copy,
  Linux BBRv3 (`google/bbr` `v3`) and quiche's BBRv2.

  *Taken.* Two patches to `vendor/noq-proto`, each with a unit test that fails without it
  (`SLOPTY.md` lists them). `handle_restart_from_idle` zeroes `extra_acked_delivered` when it
  moves `extra_acked_interval_start`, as Linux does on `CA_EVENT_TX_START`. The draft's
  pseudocode resets only the start, which reads as an omission, since its `OnInit` sets the two
  together. On the 20 Mbit/s shaper noq's own window fell from 2.4 to 5.4 MB to 41 to 45 kB. When
  the link halved it no longer overflowed the 100 ms buffer, which it had done in six of eight
  runs. Goodput and frames delivered whole did not change. `handle_probe_rtt` now marks the
  connection app-limited, the first line of the draft's `HandleProbeRTT`. That is a
  correctness fix with no measurable effect on this harness, and none was expected. Linux's cap
  of 100 ms of bandwidth on the aggregation allowance is not in the draft, and it does not bind
  once the count is cleared, so it was not taken.

  *Left as specified.* Item 3 of the earlier entry rested on a misreading. The draft keeps
  an app-limited flow in `ProbeBW_UP`: `full_bw_now` counts only rounds that are not
  app-limited, and loss is the only other way out. Linux BBRv3 does the same, and no probe-up
  exit exists there for such a flow. The draft's only concession to app-limited flows in ProbeRTT
  is the skip when a restart from idle finds the interval expired, and noq has it. Skipping or
  shortening ProbeRTT for video that is idle part of each frame goes beyond the draft. quiche's
  `avoid_unnecessary_probe_rtt`, which pushes the interval out by each quiet spell, is one such
  rule, and it only spreads ProbeRTT out. So BBR3's keyframe p99 stays two-valued. It reads 55 to
  77 ms when no keyframe meets ProbeRTT and mostly 100 to 250 ms when one does, about two
  arm-runs in three on this harness.

  *The bound stays.* Patched, noq's window no longer needs it. Unbounded, patched BBR3 matched
  the bounded one on every arm. It still guards the case the patches do not reach, an
  app-limited flow that never leaves `Startup` and paces at `2.77 × initial window / 1 ms`
  (noq#800). Removing it waits for that fix and a mesh run.

  *Upstream.* Propose both patches to noq (and quinn#2481, where the code came from), each
  with its test. Propose the `HandleRestartFromIdle` reset to the draft
  (`ietf-wg-ccwg/draft-ietf-ccwg-bbr`), with Linux as the reference. The ProbeRTT cost for
  60 fps video could go to the same tracker beside issue 109, as a report with this harness's
  numbers rather than a patch.

- ✅ **The bound stays: on the Tailscale mesh, bulk traffic cannot tell it from noq's patched
  window, and noq#800 is still open** (2026-09-25, MEASUREMENTS.md "BBR3's bound on the
  Tailscale mesh"). This is the mesh run the previous entry asked for. Eight interleaved rounds
  ran from mac-studio to macbook-pro over the internet on a direct Tailscale path. Each round
  timed keystroke echoes while a 1 GiB pull left the MacBook on the same path, `bbr3`
  against `bbr3-unbounded`. Echo p50 was 10.7 against 10.5 ms and goodput 77 against 80 Mbit/s.
  Unbounded read 10 to 30 ms lower at p90 and p99. The idle runs, where the controller has
  nothing to hold, differed by more (p99 168 against 120 ms), so that is the path drifting.
  Removing the bound would buy nothing measurable here, and keeping it costs nothing.

  What the run could not reach is the one case the bound is still for. An app-limited flow
  that never leaves `Startup` paces at `2.77 × initial window / 1 ms` (noq#800, open). A bulk
  pull is not app-limited, and a worker started over ssh cannot capture the screen, so no video
  crossed the mesh. The CLI also puts the echo and the bulk on two connections, and only the
  echo's window is traced. Reopen when noq#800 is fixed. Then stream video over the mesh
  from a launchd-installed worker (the 2026-09-14 recipe), bound on and off, and read the
  worker's bytes in flight against the window.

  The larger finding is not the controller's. With nothing else flowing, one key in eight took
  over 50 ms on this path against a 10 ms median, and the worker counted no lost packets.
  Finding that tail's cause matters more to typing over the mesh than further work on the bound.

- ✅ **A keystroke and its echo each also go once as a datagram, 2 ms after the stream copy, and
  noq's pacer holds the controller's send quantum** (2026-09-25, MEASUREMENTS.md "datagram
  copies of a keystroke and its echo"). The mesh's echo tail had one cause: packets lost on the
  way into the MacBook, one at a time, about one in eight. A key or an echo is a lone small
  packet, so when it is lost nothing behind it tells QUIC, and the stream waits for the probe
  timeout. Retransmitting sooner would not help, because the loss is only learned late. A copy
  that races the stream copy does.

  *Numbering.* Nothing new goes on the control stream. Each end counts the requests that are
  input (`TermRequest::is_input`: keys, mouse, paste, raw bytes, clear, focus) per session in
  control-stream order, from 1, and a copy names its input by that count
  (`ClientDatagram::Input`). The worker applies a copy only when it is the next input it has
  not applied, and then skips that input's stream copy. A copy that arrives early or late is
  dropped, so every input lands once and in order. Worker to client, a copy is a `Kind::Term`
  media datagram holding the session and the frame, built from the bytes already written to the
  session stream. The client's pump takes it only when it is a diff whose seq follows the last
  frame applied, and then drops the stream copy. A copy never reaches `TermState` out of order,
  so it can never ask for a resync. Only diffs built within 50 ms of input are copied, and not
  when an event the frame depends on (an image, colours, a resize, scrolled-off lines) went
  since the last frame.

  *When a copy goes.* A copy written with its stream copy shares its packet and is lost with
  it: the worker took 1 of 2014. Two milliseconds later it goes in a packet of its own. A second
  copy at 10 ms bought nothing, and delays of 5 and 10 ms did worse than 2. A copy goes only
  when it fits one datagram and at most two packets of datagrams are queued. Behind queued
  video it would arrive after the stream's own retransmission, and noq would drop video's
  oldest datagram to make room for it. So with video flowing the copies mostly stay home, and
  that is intended. Both directions are on, because the lossy direction depends on where the
  client sits: with roles reversed on the same mesh, the echo copies cut keys over 50 ms from
  12.3 to 4.1 %. `SLOPTY_ECHO_COPY=off` turns them off per process, or `<ms>` or `<ms>,<ms>`
  sets the delays.

  *The pacer.* Copies alone cost the loaded median a millisecond on the mesh and 4 ms on the
  shaped loopback. The cause was noq's pacer. A send asks for a whole MTU of credit, and below
  125 kB/s the bucket held one MTU, so a second small packet waited for the first one's bytes
  to drain at the pacing rate. BBR3 on a lossy connection that sends now and then paces at tens
  of kB/s, so a copy or its ACK waited 15 to 30 ms. The vendored noq-proto now lets the bucket
  hold the controller's send quantum, which BBR3 keeps at two packets or more
  (`vendor/noq-proto/SLOPTY.md`, patch 4). With both, keys over 50 ms fell from 24.7 to 2.2 % on
  the shaped loopback (8 ms round trip, 13 % loss each way) and from 13.5 to 0.5 % idle and
  11.8 to 0.7 % loaded on the mesh. Clean loopback is unchanged: p50 0.90 ms before, 0.80 after.

  *Open.* The mesh after-run stopped at four of six rounds when the MacBook left the network.
  It is not rerun: measurements run on this Mac alone, and the shaped loopback, which models
  the tailnet path measured here, is the number of record. With the pacer patch the bulk pull beside the echo
  ran at 180 instead of 80 Mbit/s, which is not explained yet; the patch changes nothing above
  about 250 kB/s, so the bulk connection must spend time below that. A connect over the lossy
  direction sometimes misses the 5 s handshake timeout (4 of 24 with roles reversed). BBR3's
  low pacing rate on an app-limited lossy connection is the deeper cause the pacer patch works
  around. *Upstream:* propose patch 4 to noq with its test.

- ✅ **No protocol version while in development** (2026-09-25). The user wants speed over
  negotiation: when the wire changes, every binary is rebuilt from the same tree, and a
  mismatched build is rebuilt rather than refused politely. Removed: `PROTOCOL_VERSION`, the
  `protocol` field of `Hello`, `HelloAck`, `ToServer::Hello` and `FromServer::Welcome`,
  `Rejection::ProtocolVersion` and `Refusal::ProtocolVersion`, the worker's and server's checks,
  and the CLI's "update the older one" message. A changed golden is now just a wire change and
  is accepted with the change. A stale peer now fails to decode or sends a wrong first message,
  and the link drops with a codec or protocol error. Older entries that give a version number
  are history. Tests: the `client_hello` and `server_worker_hello` goldens re-accepted, each one
  byte shorter. The ptyd socket follows the same rule: `PTYD_PROTOCOL` and
  `PtyError::ProtocolMismatch` are gone, and `Hello` carries nothing.

- ✅ **The stateless reset a restarted host sends now arrives** (2026-09-26). The null crypto
  ruling above claimed that one reset key in every process makes a restarted host reset the
  old connection. No reset ever went out. noq drops a short-header packet whose connection ID
  its generator does not recognise, and the generator's key was random per process. A packet
  shorter than 22 bytes also draws no reset, and a PING with a zero-byte tag is 13 to about 20.
  `crypto::endpoint_config` now gives every process one connection-ID key (`CID_KEY`), and the
  plain header key claims a 16-byte sample that nothing reads (`SAMPLE`), so noq pads every
  packet to at least 29 bytes. That costs up to 16 bytes on a bare ACK or PING and nothing on
  a packet that is already that long. noq padded every packet but one to the sample: the close
  it sends for a refused first packet counted on a 16-byte tag, so a refused client dropped it
  as too short and waited out its dial. That is the fifth vendored patch
  (`vendor/noq-proto/SLOPTY.md`). `docs/decisions/workers.md`, "A worker that dies shows down
  in seconds and one that comes back is found in one", has what this does for a restart. Tests:
  `a_restarted_worker_resets_the_old_connection_within_a_keep_alive` (no reset within 3 s with
  the sample at 0), `a_peer_outside_the_admitted_ranges_is_refused_before_the_handshake`, and
  `an_initial_close_holds_the_header_sample_when_the_tag_is_shorter` in noq-proto.

- ✅ **The greetings carry only what a peer reads** (2026-09-26). A client's `Hello` is its
  identity and its name; the worker's `HelloAck` is its identity, its name and its sessions.
  The client kind, both app versions and the capability flags are gone: every binary is built
  together, so a version told nothing, and no peer read a kind or a flag. `WorkerMsg::Rejected`
  went with them. The worker never refused a `Hello`, and the one place that sent `Rejected`
  was its shutdown, which closes the endpoint anyway. A client dialling the server names
  itself and nothing more (`Role::Client { name }`), since the server reads only the name.
  ptyd lost its hello round trip: its reply carried a pid nobody read, so a worker now
  connects and sends its first request. One redial rule serves every link,
  `slopty_net::redial`, where the client used to keep a verbatim copy.

- ✅ **A worker's reset reaches the browser as a reset, never as a clean end first**
  (2026-09-26). `splice` split the browser's socket into halves, and on a stream failure set
  a zero linger and dropped them. Dropping the write half on its own sends a FIN, so the
  browser could read a clean end of file before the reset landed; the test caught it as a
  read of 0 bytes about one run in fifty. The halves are now joined back into the socket
  before it is dropped, so the only thing the browser sees is the reset. Test:
  `a_stream_the_worker_resets_resets_the_browser`, 200 runs in a row under
  `--stress-count`.

- ✅ **A client reads each stream's header on that stream's own task** (2026-09-26). The link
  accepted a unidirectional stream and read its header inside the accept loop. A header whose
  packet was lost therefore held up every stream behind it until the retransmission arrived.
  The loop now only accepts, and each stream's task reads its own header with
  `streams::read_uni`, as the worker already did. Test:
  `a_stream_waiting_on_its_header_does_not_hold_up_the_next` (`slopty-client`), where a stream
  with half a header stays open while a session stream opened after it is delivered.

- ✅ **The keystroke path's threads are user-interactive** (2026-09-26, MEASUREMENTS "the
  keystroke path under an all-core spin"), amending **Machine load alone does not flap the
  path**. That ruling declined `pthread_set_qos_class_self_np` because load did not flap the
  path; it never looked at the echo. Under an all-core `USER_INITIATED` spin (what a build asks
  for) the worker's own share of an echo, input received to frame sent, was p90 5–6 ms and p99
  11–569 ms with its threads unclassed, against p90 0.2–0.5 ms and p99 0.7–11 ms at
  `QOS_CLASS_USER_INTERACTIVE`. The key's leg into the worker fell from p90 2.7–4.1 to
  0.4–0.8 ms. The process's `LatencyCritical` activity keeps timers sharp but classes no
  thread. So `slopty_platform::user_interactive_thread` classes every session actor's thread,
  each remote window's input thread and, through `on_thread_start`, every thread of the worker
  daemon's runtime (the connections' loops, noq's drivers) and its main thread. The runtime's
  blocking pool goes with it, which is short file work. ptyd is left alone: it only takes the
  tap and is off the keystroke path. The video lanes were user-interactive dispatch queues
  already. The clients take the same class on every runtime thread (the macOS and iOS apps: the
  link writer, the pumps, noq's drivers; the CLI, plus its stdin thread): under the same spin the
  CLI's whole echo went from p90 166–270 ms and maxima over a second to p90 20–23 ms and maxima
  under 60 ms.

- ✅ **An echo lifts its session stream above the other sessions** (2026-09-26). The control
  stream and every session stream sit at `AHEAD_OF_DATAGRAMS`, and noq round-robins equal
  priorities a packet each. An echo written beside other busy sessions therefore waited a
  packet from each. The pump sets the stream to `ECHO_PRIORITY` (1) before a frame the actor
  marked as an echo, and back to `AHEAD_OF_DATAGRAMS` at the next frame that is not one
  (`slopty_net::streams::EchoLift`). noq files a stream by the priority it had when data was
  queued, so the priority is set before the write. Six sessions flooding a 20 Mbit/s link beside
  the typed one: the echo's p50 fell from 10.2–18.2 ms to 7.6–13.0 ms in all six alternated
  pairs, and p99 from 30–73 ms to 11–25 ms. Below the link's rate the two are within noise
  (MEASUREMENTS, "an echo beside other busy sessions"). The echo also goes ahead of the control
  stream; QUIC orders nothing across streams anyway, so nothing relied on the old order. Test:
  `a_lifted_echo_overtakes_other_sessions_queued_frames` (loopback: 900 B of the other sessions
  ahead of it instead of 29 700).

- ❌ **No latency tier 0 on the keystroke threads** (2026-09-26). A tokio timer in a launchd
  job fires 2–4 ms late at 1–8 ms (timer coalescing by the thread's latency tier, plus tokio's
  millisecond wheel), and `THREAD_LATENCY_QOS_POLICY` tier 0 saves 0.4–1.6 ms of that. But
  setting any Mach thread policy takes the thread out of the QoS system, dropping the
  user-interactive class that cut a loaded echo's p90 eightfold. Kept the class (MEASUREMENTS,
  "timers fire late by the thread's latency tier").

- ✅ **BBR3 takes its first round trip from the acknowledged packet** (2026-09-27,
  MEASUREMENTS "one bulk stream over a long round trip"). One upload through the shaping relay
  with nothing but delay reached 10 Mbit/s at a 60 ms round trip, with no loss and a 2.3 MB
  window at the end. noq's connection hands an ACK to the controller before it feeds the ACK's
  round trip to the `RttEstimator`, and BBR3 built its first rate sample from the estimator,
  which on the first ACK still holds the configured initial RTT. That became `BBR.min_rtt`.
  Slopty's `INITIAL_RTT` is 5 ms, so on any path longer than that the minimum stood at 5 ms
  until the 10 s filter expired, the window was sized to `2 × bw × 5 ms` and the flow could not
  fill even that fraction of the pipe fast enough to raise `bw`. The vendored noq-proto now takes
  the packet's own `now - send_time`, as every later sample already did and as the draft's
  `RS.rtt` is defined (`vendor/noq-proto/SLOPTY.md`, patch 6). The 2 × BDP bound is untouched;
  its round trip was always measured per packet in `slopty_net::congestion`.

  *Kept.* `INITIAL_RTT` stays 5 ms: it sets the handshake's first retransmission, and the bug was
  the controller reading it as a measurement. Paths shorter than 5 ms never showed it, because
  their first real sample was lower and replaced it; that is also why the 4 ms shaped link of
  `echo_beside_flood` saw nothing.

  🔬 *Next ceiling.* With the minimum right, one stream's rate is `stream_receive_window /
  RTT`: noq's default 1.25 MB (sized for 100 Mbit/s at 100 ms) gives about 166 Mbit/s at 60 ms,
  and the delivery rates measured sit on that line at 20, 40 and 60 ms. An 8 MiB window reached
  191 Mbit/s at 60 ms but only 186 at 20 ms, against 310 with the default, so it is not simply
  taken; it needs its own look at start-up overshoot on loopback. *Upstream:* propose patch 6
  to noq with its test.

  ✅ *Ruled 2026-09-27: the connection keeps noq's 1.25 MB stream window.* On a quiet machine
  (release, three runs a cell, runs within 2%) one 16 MiB stream went from 213 to 305 Mbit/s at
  30 ms and from 106 to 182 at 60 ms with a 4 MB window, and 8 or 16 MB added nothing (start-up
  within 16 MiB is the next limit). But the same 4 MB window made a terminal's echo worse beside
  six busy sessions on a 20 Mbit/s link, in all three alternating passes: p90 7.2, 9.0 and
  6.7 ms against 5.8, 7.0 and 5.8, max 16 to 78 ms against 9 to 11. That link's product is
  ~10 KB, so the window is not what the network sees; it is the terminal pump's backpressure.
  A small window stalls a busy session's writes and its actor folds frames together; a large
  one lets stale frames queue in noq's send buffer ahead of the echo. Latency comes first, so
  the window stays, and a larger one belongs on bulk streams alone. noq sets it per connection
  only, so that takes either a per-stream window in the vendored noq-proto and noq, or a large
  file sent as parallel ranges; neither is done. `SLOPTY_STREAM_WINDOW` (bytes) sets it for
  measurement (MEASUREMENTS, "the stream window against echo").

- ✅ **An echo does not wait for the pacer** (2026-09-27, MEASUREMENTS "an echo behind paced
  video"). noq's pacer holds a packet until the rate has earned it and wakes the connection on a
  tokio timer, which rounds up to its millisecond and fires later still under macOS coalescing.
  An echo written while video was paced out went in the next packet and waited for that timer:
  p90 1.4–2.0 ms at 20 Mbit/s, where the rate alone asks 0.5 ms. The vendored noq-proto gained
  `TransportConfig::stream_priority_unpaced` (`vendor/noq-proto/SLOPTY.md`, patch 8), and the
  endpoint sets it to `ECHO_PRIORITY`. A datagram then starts without the pacing check while a
  lifted session stream has data pending, and the echo's p90 is 0.24–0.30 ms. The congestion
  window still applies, and the pacer is charged for the packet. What escapes pacing is bounded
  by what `EchoLift` lifts, the echo frames `EchoBurst` exempts, and a lifted stream falls back
  at its next frame that is not an echo. The key going up is lifted the same way: the client's
  control stream rises to `ECHO_PRIORITY` for typed input that fits a datagram and falls back
  for anything else, so a key typed during an upload, a pasted picture or a file save is not
  held behind their pacing either, while a long paste is still paced.
  Tests: `an_echo_does_not_wait_for_the_pacer` (slopty-net, loopback at 50 kB/s: 10 ms at the
  median paced, under 5 ms unpaced) and noq-proto's three `stream_priority_unpaced_*` tests.

  ❌ *Not the timer slack.* Lending the pacer a millisecond of the rate, as Chromium's pacing
  sender does, changed nothing (p90 1.42–1.59 against 1.44–1.66 ms). The video queued behind
  the pacer spends any credit before an echo arrives.

- ✅ **A stream message is serialised once and handed to noq shared** (2026-09-28,
  MEASUREMENTS "rows kept across frames, pixels in the texture's format"). `codec::encode`
  serialises straight after four reserved prefix bytes (`postcard::to_extend`), patches the
  length and freezes that `Vec` into `Bytes`; `FramedSend::send_raw` takes the `Bytes` and
  writes it with `SendStream::write_chunk`, so noq keeps the buffer itself instead of copying a
  slice (`Bytes::copy_from_slice` in its `write`). The worker's `Outbound::wire` hands out the
  shared `Bytes`. A frame's credit still returns when its `Outbound` drops, after the write
  has handed the bytes over, as before. A 96 KB screen frame's encode and hand-off went from
  165–178 µs to 160–164 µs; an echo's one row is within noise (3.3 µs).
- ✅ **A frame that came twice is dropped before it is decoded** (2026-09-28). With datagram
  copies on, the second copy of an echo frame (the stream's, or the datagram's) was decoded and
  then dropped by the frame order. The client now reads the frame's number from the front of
  its postcard body (`terminal::frame_head`) and skips the stream's copy of a frame already
  passed on (`FramedRecv::recv_unless`), and the datagram reader drops a copy at or below the
  last number its pump passed on (an atomic the pump updates). The frame order stays the
  authority; the check before decoding only drops what it would drop. Tests: client
  `a_copy_already_passed_on_is_dropped_undecoded`, proto
  `a_copy_is_read_to_its_frame_number_without_decoding_it`.

- ✅ **A window stream's input also goes once as a datagram, and leaves unpaced** (2026-09-28,
  MEASUREMENTS "window input through a lossy link"). Window input rode only the ordered control
  stream, so a lost packet held every later move, click and key for every stream on the
  connection until QUIC recovered it, and a paste or a clipboard offer ahead of it on the stream
  held it too. It now gets the terminal's treatment: a `ClientDatagram::ScreenInput` copy 2 ms
  after the stream copy, through the same `slopty_net::echo::Copies` (so `SLOPTY_ECHO_COPY`
  governs both), and the echo priority on the control stream while it is written.

  *Numbering.* Nothing new goes on the control stream. Both ends number each stream's input
  and quality changes (`ScreenRequest::numbered`) in control-stream order, from 1, and count
  apart those that apply only in their turn: everything but a move. A copy carries both
  numbers. A click, key, scroll or pinch copy applies only when it is the next in-order request;
  a move's copy applies when every in-order request before it has and nothing newer has, so a
  move may overtake older moves and nothing else. From the stream, an in-order request whose
  copy was applied is skipped, and so is a move older than anything applied. Each click and key
  lands once and in order, and the pointer never goes back (`ScreenOrder` in
  `apps/slopty-worker/src/conn.rs`). A quality change is in-order and has no copy, because the
  client maps its pointer at the scale it asked for: a click's copy must not reach the window
  before the scale it was mapped at. A ⌘V has no copy either, since its clipboard offer rides the
  control stream just ahead of it. The two numbers were needed over one: with a single count the
  worker cannot tell, for a copy that arrives early, whether the missing requests before it were
  moves it may pass or a click it must wait for.

  *Unpaced.* The copies first went out with window input still paced, and made a lossy drag
  worse: p99 up to 634 ms against 309 ms before. The client's window sat at its floor and the
  pacer charged each small packet a whole packet's credit, now for twice the packets. With the
  control stream raised to `ECHO_PRIORITY` for window input, as for typed input, the drag's p90
  fell from 22–40 to 7.8–11 ms and its p99 from 49–226 to 12–32 ms at 13 % loss each way. Lone
  clicks, which is how keys typed into a window come, fell from a 27–28 ms p90 to 6.8–8.3 ms.

  *Open.* On a clean link every copy is late and costs a spawned task and a timer on the client
  and a decode on the worker; at load average 35 that cost could not be separated from the
  load's own tails. A copy of every move at 160 Hz is a lot of redundancy for the moves, whose
  worth is only that a newer one may pass a lost one; copying only moves that follow a gap is
  the next thing to measure.
- ✅ **Every datagram starts with its channel** (2026-09-28, audit finding 24). A worker's echo
  copy used to carry a 16-byte `MediaHeader` with nothing set but `kind = Term`. `Kind::Term`
  sat among the media kinds, and the reassembler had to reject it. Datagrams from the client
  were a postcard enum instead. Now the first byte of every datagram, in both directions, is a
  `datagram::Channel`: `Media` 0, `Term` 1, `Feedback` 2, `Input` 3 or `ScreenInput` 4.
  - `MediaHeader.channel` is the media header's first byte (17 bytes now), so a media packet
    keeps its zerocopy parse. `MAX_PAYLOAD` is rounded down to even, for the parity shards.
  - An echo copy is `[Term][session][event]`, 15 bytes less than before (`TERM_OVERHEAD`, 18).
  - `ClientDatagram::encode`/`decode` put the channel byte in front of the postcard body.
    `Kind::Term` is gone.
  - A new kind of datagram is a new channel, not a fake media kind.
  - Test: `a_datagram_of_each_channel_round_trips` (`slopty-proto`) and the header offsets in
    `slopty-media`'s pipeline test. Goldens: `media_heartbeat`, `worker_frame_copy`,
    `client_input_copy`, `client_screen_input_copy`, `client_nack` and `client_refresh`.
- ✅ **Byte payloads serialize as bytes** (2026-09-28, audit finding 1). Every `Vec<u8>` payload
  on the wire has `#[serde(with = "serde_bytes")]`: `TermRequest::Raw`, clipboard items and
  data, conversation blobs, orchestration writes, upload parts, file outcomes and stills.
  Postcard writes both forms the same way, so no golden moved. A format that tells the two
  apart would get the compact form.
- ✅ **A transfer retries only what a retry can fix** (2026-09-28, audit finding 18).
  - **`slopty_client::xfer::XferError`** separates `Local` (a file here that cannot be read or
    written, with its `io::Error` as the source) from `Cut`, `Mismatch`, `Worker`,
    `Unanswered`, `Cancelled` and `LinkClosed`. An upload resumes only after `Cut`. A download
    is fetched again after `Cut`, `Mismatch`, `Worker` or `Unanswered`, never after `Local`.
    `send_file` opens and seeks the local file before it opens a stream, so a file it cannot
    read never makes the worker wait. Before, a local `File::open` failure was treated as a
    cut: two `Resume` round trips, then a failure.
  - **`NetError::Io { context, source }`** is the variant for disk errors (transfers, saves,
    the known-workers store). `Bind` keeps its `io::Error` as the source, and
    `Store(serde_json::Error)` is kept for a store that does not parse. A stream's write error
    goes through `NetError::stream`, which keeps its cause chain.
  - Test: `an_unreadable_file_fails_as_local_and_is_not_resumed` (`slopty-client`).
- ✅ **A download outlives its link** (2026-10-03, readiness audit A9; wire change). A drag
  into Finder, a saved copy or a File Provider read stopped with `LinkClosed` the moment the
  link went, and a relink started it again from nothing, though the partial file was on disk.
  - **A line per worker.** `slopty_client::xfer::Line` is one worker's links in this process,
    one after another: every `WorkerLink` puts its uplink on its worker's line as it starts
    (`Line::of(ack.worker)`). A download whose link goes (its connection closed, or its
    control stream gone) abandons the attempt, waits for its streams to stop, and waits on the
    line for a link that is up and is not the one it lost. Then it fetches again over that one,
    naming what it holds. A relink costs no attempt; the three attempts are for cuts, digests
    and the worker's failures.
  - **Process-wide, not passed in.** The next link is dialed by whoever dials (the app's
    redial loop, the File Provider domain), which knows nothing of the transfers the last one
    carried, and a drag's promise holds the remote it was made with. So the line is found by
    the worker's id, kept weakly: it lives while a link or a download holds it.
  - **How long.** Up to `RELINK_WAIT`, five minutes: long enough for a network change, a Mac
    waking or the worker restarting for an update, and Finder's progress and its cancel stay up
    meanwhile. Past it the download ends `LinkClosed`.
  - **Cancel.** A cancel reaches a download on whichever link it is, or while it waits: the
    line keeps each download's stop by the transfer it began as, and `Remote::cancel`, Finder's
    progress and the File Provider's cancel all go through `Line::cancel`. The download then
    calls off the attempt in flight and tells the worker if the link is up.
  - **The version travels with the claim.** A retried `XferMsg::Fetch` names each held file as
    a `transfer::Held`: its bytes and the size and modification time its header said. The
    worker resumes it while the file is still that version and sends it from 0 otherwise
    (`slopty_worker::xfer::resume_points`). Before, the worker kept the versions it had sent in
    memory, so a worker that restarted (the common way a worker comes back) knew none and sent
    every file from its first byte. A client claiming bytes it does not hold only hurts itself:
    the whole file is checked against the worker's digest.
  - **The File Provider dials while it waits.** The extension opens its links only when the
    system asks something, so a fetch waiting on the line would wait for nothing. While a
    fetch runs, its domain dials the worker again each time the link goes, backing off from 1 s
    to 10 s (`Domain::keep_linked`). A link the extension holds counts as gone once its
    connection is closed, not only once its events say so.
  - Number: the next link's fetch leaves 5 to 7 ms after the link is up, most of it the sync
    of the partial it claims (`docs/MEASUREMENTS.md`, "a download across a relink").
  - Uploads go on the same way: the next entry.
  - Tests: `a_download_cut_by_a_lost_link_goes_on_over_the_next_link` (the scripted worker
    closes the link 1.5 MB into 4 MB; the next link's fetch claims the durable bytes of that
    version, and the file lands with the same BLAKE3 digest),
    `a_download_waiting_for_its_worker_is_cancelled` (a cancel during the outage ends it at
    once), `a_download_resumes_only_the_version_held` (slopty-worker),
    `a_download_resumes_what_the_client_holds_unless_the_file_changed` (the worker daemon),
    `a_fetch_goes_on_once_the_worker_is_back` (the File Provider domain against a real worker
    restarted on its port). Golden: `client_xfer_fetch_resumed`.
- ✅ **An upload outlives its link** (2026-10-03, lane U2). A drop on a remote tile, files
  pasted into a shell or a window, and the Files picker's uploads ended with `LinkClosed` when
  the link went, and the UI dropped them from their tiles at once (`reset_remote`), though the
  worker kept the partial file and its transfer.
  - **On the line, like a download.** An upload follows its worker's `Line` from the first
    link. When the link goes (its connection closed, or its control stream gone), it waits for
    the next link, up to `RELINK_WAIT`, and goes on over it: it sends `Begin` again under the
    same transfer, counting the files the worker has not said landed, and sends each of them.
    A file a stream was opened for asks the worker what it holds (`Resume` → `Offset`) and
    goes from there; one never started goes from 0. A relink costs no attempt.
  - **The upload ends on the worker's word.** It used to end once its last stream was
    acknowledged, with the `Finished` that the UI waits on still to come. A link lost in
    between left the UI waiting with nobody to ask again. Now the task waits for `Finished`,
    hearing the worker's `Done`s on whichever link it is on (`Table::track_upload`), and
    skips a file the worker said landed. Its failure is said on the link it ended on
    (`Uplink::events`), whose events the UI still reads.
  - **The worker's transfer is the daemon's, not a connection's.** A `Begin` of a transfer in
    flight keeps what it has (`Begun::Again`). One that finished is remembered (the last
    `FINISHED_KEPT`, 64), so a client whose link went before the end reached it is told
    `Finished` again (`Begun::Finished`), and a stream of a file that landed already is
    drained and answered with its `Done` again rather than written twice. A restarted
    worker that forgot a transfer finds where its entries went in the ledger of unfinished
    entries (`Transfers::ledger_roots`), so a partial file resumes where it is, not again
    in the drop directory because the entry's directory now exists. A `Resume` waits for its
    transfer's `Begin`, which on the next link may still be asking the session for its
    directory.
  - **One writer per file.** The first link's stream can still be open on the worker when
    the next link's stream of the same file arrives: the worker learns of a lost link only
    at its idle timeout, and a shaper or a network change tells it nothing. The later stream
    claims the file (`Transfers::claim`). The earlier one is told to stop, keeps what it
    wrote, and lets go, and only then does the later one open the partial file at its
    offset. The claim is held until a landing is recorded, so a stream that waited finds
    the file landed rather than an empty partial. The latest claim wins.
  - **The version travels with the claim, from this side.** Before sending a started file
    again, the upload reads its size and modification time. A file that changed here while
    the link was down goes again from its start, as the new version.
  - **Only the transfer's failure ends it.** The worker reports a file it could not write as
    `Failed` with the file's name, and it also cuts the stream. The upload sends a cut
    stream again from what the worker holds and says it failed only once it gives up, so
    neither the upload nor the UI ends on a named `Failed`. A `Failed` in answer to a
    `Resume` answers that resume at once.
  - **The UI keeps the upload.** While the worker is away the tile keeps its progress and
    its cancel. The cancel reaches the upload through the remote it went by
    (`Upload::via`): the line stops it, waiting or not, and the worker hears the cancel if a
    link is up. On the next link the worker's `Finished` types the paths into the shell as
    before. A shell that closed meanwhile is said in a notice rather than dropped silently.
    An upload whose worker stays away for `RELINK_WAIT` ends with "the worker went away",
    as its task has. A paste into a window waiting on files lets its keys go when the link
    goes, rather than minutes later, and the files still reach the worker's pasteboard. The
    drop of a drag ends with its link, since the drag on the worker ends with it.
  - **The system's progress cancels through the line** too, so Finder's and the Live
    Activity's cancel reach an upload waiting for its worker.
  - Rejected: a fresh transfer per link, as a download does. A download names what it holds
    in each fetch, so a new id costs nothing. An upload's id is where the worker keeps its
    roots, its landed files and its count, and a new id would land a second copy in the
    drop directory.
  - Number: `docs/MEASUREMENTS.md`, "an upload across a relink".
  - Tests: `an_upload_cut_by_a_lost_link_goes_on_over_the_next_link` (scripted worker: the
    same transfer begun again, the resume asked, the rest from the worker's offset, the
    same BLAKE3, and the end on `Finished`), `an_upload_waiting_for_its_worker_is_cancelled`
    (`slopty-client`);
    `a_transfer_begun_again_keeps_what_it_has_and_one_finished_is_told_again`,
    `a_restart_finds_where_a_transfers_entries_went` and
    `a_later_stream_of_a_file_takes_it_over` (`slopty-worker`);
    `an_upload_goes_on_over_the_next_link_from_what_the_worker_holds` (the real client and
    worker, the first link through a 4 MB/s shaper cut mid-file, the worker not told);
    `an_upload_outlives_its_workers_link` (`slopty-ui` workspace: progress and cancel kept,
    paths typed over the next link, the end after `RELINK_WAIT`).
- ✅ **Wall-clock times are `WallMs`** (2026-09-28, audit finding 10). A newtype in
  `slopty-core` over milliseconds since the Unix epoch, serialized transparently so no golden
  moved. It covers every wall time on the wire: session starts, file modification times, agent
  and conversation stamps, prompts and their deadlines, `HubEvent.at_ms` and a worker's last
  sighting. Zero means unknown. `WallMs::to_system` and `since` replace the hand-rolled epoch
  arithmetic, and `slopty_core::shell_quote` replaces the three copies of shell quoting.
  Test: `wall_milliseconds_round_trip_and_zero_is_unknown`.
- ✅ **The transport's knobs are read once, into a typed `Tuning`, and logged** (2026-09-28,
  audit finding 57). `SLOPTY_CC`, `SLOPTY_QUIC_IW`, `SLOPTY_STREAM_WINDOW`,
  `SLOPTY_DATAGRAMS_FIRST`, `SLOPTY_ECHO_COPY` and `SLOPTY_PATH_TRACE_MS` are parsed by
  `slopty_net::endpoint::tuning()` on first use, into `Tuning` with a `Controller` enum.
  The values are logged at `info` when any knob is set and at `debug` otherwise. Every
  connection, endpoint and echo copy reads that one value; before, each endpoint re-read the
  environment and an unknown controller warned once per endpoint.
  - They stay in release builds rather than behind an `experiments` feature. They exist so a
    measurement can compare alternatives on the binaries a person runs, and the MEASUREMENTS
    entries behind BBR3, the bound, the initial window and the echo copies were all taken that
    way. The only `experiments` feature is `slopty-codec`'s, and `slopty-worker` turns it on
    unconditionally, so it would gate nothing. The risk the finding names is a run on a knob
    nobody sees; the startup line removes it.
  - `Copies::from_env` and `path_trace_period` read the same `Tuning`.
  - Tests: `unset_knobs_are_the_shipped_transport`, `each_knob_reads_its_variable` and
    `the_initial_window_reads_its_flag` (`slopty-net`).
- ✅ **A link that stays on DERP says so** (2026-09-29, product gaps #5). Tailscale v1.102.4
  (the release in use) fills a peer's `Relay` with its home DERP region whatever the path.
  `CurAddr` is set only for a trusted direct UDP path and `PeerRelay` (`ip:port:vni:N`) only
  for a peer relay. `tailscale status` prints `relay "<region>"` when the peer is `Active`
  (a packet in the last 45 s) and both are empty (`cmd/tailscale/cli/status.go`,
  `wgengine/magicsock/endpoint.go`). `slopty_tailnet::Node::path` already read it this way,
  and `WorkerMsg::Path` already carries the result to the client, so nothing on the wire
  changed.
  - **Ten seconds before a word** (`slopty_proto::tailnet::DERP_NOTICE_AFTER`). A path starts
    on DERP while a direct one is found. A direct path whose pongs stopped also reads as DERP
    for a few seconds (`trustUDPAddrDuration`, 6.5 s), because magicsock sends on both. Only
    a DERP path that holds is news.
  - **The worker logs it** once per stretch on DERP, at `warn`, with the client's address
    and the region (`tailnet::Relayed`).
  - **The client decides when to show it.** `slopty_client::relay::RelayWatch` takes each
    `WorkerMsg::Path` with the time it arrived. `notice(now)` gives
    "Relayed via fra — adds latency" and the fix, but only after ten seconds on DERP.
    `due(now)` says when to look again. `LinkPath::relay_note` and `LinkPath::relay_fix`
    hold the wording. The status bar and the screen header, in `slopty-ui`, are the next
    step.
  - Tests: `a_link_that_stays_on_derp_is_logged_once` (worker),
    `derp_is_said_once_it_has_held` and `a_peer_relay_says_nothing` (client),
    `only_derp_says_it_is_relayed` (proto). The status fixture now spells `PeerRelay` as the
    daemon does.
- ✅ **The server's machine as a peer relay: explained, not switched on** (2026-09-29, product
  gaps #5). Tailscale 1.86+ tries a peer relay in the tailnet before DERP (KB 1591). A relay
  can run on any OS but iOS, tvOS and Android, and Headscale supports it from 0.29.0. The
  Slopty server's machine is the one node that is always on, which makes it the natural
  relay.
  - Turning it on takes three things:
    - `tailscale set --relay-server-port=40000` on that machine. This writes
      `ipn.Prefs.RelayServerPort` and needs `LocalAPI` write access: root, the operator, or
      the `admin` group on a Mac.
    - That UDP port open to the tailnet.
    - A grant in the tailnet policy, `{"src": [...], "dst": ["<relay>"], "app":
      {"tailscale.com/cap/relay": []}}`, which only a tailnet admin can write.
  - Slopty cannot write the grant. Setting a pref on the user's Tailscale behind their back
    is not ours to do either. So Slopty reads and explains:
    - `LocalApi::relay_server_port` reads `GET /localapi/v0/prefs`, which needs read access
      only. A missing key means off, since Go's `omitempty` drops a nil `*uint16`, and `0`
      means a port the daemon picks.
    - `slopty server relay` prints whether the machine is a relay, the command, the port to
      open and the grant, filled in with this node's tailnet address.
    - `slopty server status` and `slopty server install` print one line on it.
    - `LinkPath::relay_fix` points a DERP notice at `slopty server relay`.
  - Not verified: whether the App Store and standalone Tailscale apps on macOS will serve as
    a relay. Their `LocalAPI` sits behind closed code, and the KB names no variant.
  - Tests: `the_peer_relay_port_reads_from_the_prefs` (`slopty-tailnet`, fake `LocalAPI`),
    `the_report_says_how_to_turn_the_relay_on` and
    `the_relay_port_comes_from_the_daemons_prefs` (`slopty-cli`).
- ✅ **The transport is tested on a simulated network, on tokio's paused clock** (2026-09-29,
  MEASUREMENTS "the transport on a simulated network").
  - `slopty_shape::sim::Net` is an in-memory datagram network whose `Socket` is noq's
    `AsyncUdpSocket`. Each direction between two sockets runs the relay's seeded `Link`
    (delay, jitter, loss, rate, queue). On top of that it adds three faults the relay cannot
    give: blackouts, which lose what is sent and what is in flight; NAT rebinding, where a
    socket's peers see it at a new address and the old mapping is gone; and reordering, off by
    default. `slopty_net::endpoint::bind_on(socket, server, seed)` builds the shipped endpoint
    and transport config on such a socket, and `WorkerListener::on` listens on it.
  - noq's `TokioRuntime` reads tokio's clock, so under `start_paused` the whole connection runs
    in virtual time. A 60 s outage costs milliseconds, and a load on the machine changes what
    a run costs, never what it measures. `bind_on`'s seed also seeds noq's endpoint generator
    and BBR3's probe generator (noq-proto patch 9). With the network's seed, a run then repeats
    to the packet: `a_seed_repeats_its_run` compares two runs' delivery digests. Before the
    probe seed, a flooded link's echo times differed between two runs of one seed.
  - `crates/slopty-net/tests/sim.rs` runs a real dial, control stream and session stream. It
    covers five cases: the handshake at 20 % loss each way; keys typed into a 30 s outage; an
    outage past the 45 s idle timeout, then a new dial; a NAT rebinding mid-typing; and keys
    beside a flood on a 2 Mbit/s lossy link. Each checks every key and echo once and in order,
    on one connection. The gate runs 8 seeds of each plus two pinned seeds (0.6 s). The
    ignored `every_scenario_holds_across_the_seed_sweep` runs 200 seeds of each at night
    (about 30 s) and prints the numbers. A scenario still running after 600 simulated seconds
    fails with its seed, since keep-alives keep a stuck run's clock moving.
  - Not the relay for this. The relay runs on the real clock between real sockets, so an
    outage costs its full length, the results move with the machine's load, and it cannot
    rebind or reorder. It stays the tool for real-time measurements and the e2e lanes.
  - **The clock.** `slopty-net` and `slopty-shape` time the network on `tokio::time::Instant`:
    admission's lookup and owner TTLs, the relay's run epoch and timer, and the tests. Each
    crate has its own `clippy.toml` that disallows `std::time::Instant::now` and `::elapsed`.
    Clippy reads only the nearest `clippy.toml`, so each file is the root's plus those two
    entries, and `clippy_config_is_the_workspaces_plus_the_clock` fails when a copy drifts from
    the root. A read that must be real, the sweep's own cost, carries
    `#[expect(clippy::disallowed_methods, reason = …)]`. noq still hands congestion
    controllers `std::time::Instant`, which it derives from tokio's clock. `Redial` still takes
    its callers' `std::time::Instant` (slopty-cli, slopty-client, slopty-app, slopty-worker),
    and the rest of the workspace still reads the std clock; each owner moves when it wants its
    own paused tests.
  - **Found by it: a lost FINISHED from the worker stranded the client.** The null crypto
    provider's client sent its `FINISHED` as soon as it read the worker's `HELLO`. The worker
    finishes on that `FINISHED` and discards its Handshake keys at once, so when its own
    `FINISHED` (in the same datagram as its `HELLO`) was lost, it could never resend it. The
    client is connected only by a Handshake packet from the worker, so it waited out its 2 s
    dial and failed with "no answer": 29 of 200 seeds at 20 % loss. The client now sends its
    `FINISHED` only after reading the worker's, which is TLS's order. On a clean path both
    arrive together, so a dial is still two round trips. Tests:
    `the_client_finishes_only_after_the_server_has` (unit) and
    `a_handshake_at_a_fifth_lost_each_way_completes` (seed 31 loses that first flight).
  - **Found by it: a lost PATH_CHALLENGE after a NAT rebinding wedged the worker.** noq kept
    no validated path to fall back to (an inverted test in `Connection::migrate`). When the
    worker's one challenge to the client's new address was lost, the path stayed unvalidated
    for good. noq sends no data on such a path, and its PINGs kept the connection alive, so
    the worker never sent the client another byte. Fixed as noq-proto patch 10
    (`vendor/noq-proto/SLOPTY.md`). Test: `a_nat_rebinding_moves_the_connection_without_a_reconnect`
    (seed 10 loses the first challenge).
  - **The one-off gate failure is not reproduced.** `typing_through_a_lossy_link_lands_once_in_order`
    once failed at connect with "connection lost: closed by peer: 0". No seed of the simulation
    closes a dial that way. Its dials failed only as "no answer", the stranded client above. A
    relay run can reach that too, since load changes which retransmissions draw the fixed
    seed's losses. A
    code-0 close with no reason is noq's implicit close when the worker's last handle to a
    connection drops. In slopty-net that happens only when `listen::greet` gives up on a peer
    (10 s without a control stream or a hello, or a failed read). Past the greeting, it
    happens on the worker app's own error paths in `conn::run`. Those need the worker app on
    the simulated network to test, and that crate has another owner.
- ✅ **A dual-stack port no IPv4 socket holds** (2026-09-29, the fill's "no answer"). About
  one fresh dial in 10 000 to a server on a loopback port timed out. The cause was the port,
  not the crypto. The client endpoint binds `[::]:0` dual-stack, and XNU's allocator for it
  checks the IPv6 sockets but not the IPv4 ones: with 2 000 IPv4 sockets open, one such bind
  in eight took a port one of them held. An explicit `[::]:p` also binds beside a socket on
  `0.0.0.0:p`. IPv4 datagrams to that port go to the IPv4 socket. A client that drew the
  server's own port (the server on `127.0.0.1:<ephemeral>`) sent its Initial from
  `127.0.0.1:p` to `127.0.0.1:p`. The server answered to that address, which is its own
  socket, and took its own Initial for a new client's. Its ACK acknowledged a packet the new
  connection never sent ("unsent packet acked"), and its HELLO carried the server-only
  parameters ("bad transport parameters: parameter had illegal value", once per
  retransmission). The client heard nothing. Every failure in a run of 100 000 dials had the
  client's port equal to the server's.
  - **Rejected: the hypothesis of a stray packet from an earlier connection on a reused
    port.** Stale traffic to a reused port is answered as designed: the new endpoint sends
    the old connection a stateless reset (the reset key is the same in every process). In a
    one-off harness, 5 000 dials from one port, each endpoint killed without a close while
    the server pushed to it and the next bound on the same port, failed none. `ConnectionIndex::get` routes by the connection ID before any reset token, and
    nothing here reached the reset-token lookup. A tag on the null crypto would not have
    helped either: the server's own packets would carry valid tags, and the client would
    still hear nothing.
  - **The fix is in `slopty_net::endpoint::bind_udp`.** A socket that takes IPv4 (on `[::]`
    or a v4-mapped address) gets its port from an IPv4 bind first. IPv4's checks cover both
    families: in 20 000 binds, `0.0.0.0:0` never took a port an IPv4 or dual-stack socket
    held. The probe is closed and the dual-stack socket binds that port explicitly. If
    something took the port in between, the next port is tried, up to 16 times. An explicit
    port an IPv4 socket holds is now `AddrInUse`, as it is on Linux. The cost is one IPv4
    bind and close per endpoint (about 17 µs) and nothing per packet.
  - **It also covers the worker's listener on `--port 0`**, which the e2e binds, and a
    test's client beside other test processes' IPv4 sockets. The previously flaky dial in
    `echo_is_not_held_behind_slow_requests` (a fresh client dialing a fresh worker on
    loopback) fits the same cause, but no run caught that case.
  - The MCP listener (`slopty_server::mcp::bind`, dual-stack TCP) half shares it. TCP draws a
    free port clear of IPv4 sockets (0 of 1 000 beside 200 held), but a fixed `[::]:p` bound
    beside a listener on `0.0.0.0:p`. A fixed port is now bound on `0.0.0.0` alone first, and
    is `AddrInUse` when held there (`crates/slopty-server/tests/mcp_bind.rs`).
  - Tests: `a_fresh_endpoint_takes_no_port_an_ipv4_socket_holds` (14 of 1 000 before, 0
    after) and `an_endpoint_on_a_port_an_ipv4_socket_holds_is_refused` (bound beside
    `0.0.0.0:p` before), in `crates/slopty-net/tests/dual_stack_port.rs`.
    `fresh_endpoints_dial_a_loopback_server` (ignored) is the measurement
    (docs/MEASUREMENTS.md, "A dual-stack port no IPv4 socket holds").
- ✅ **Each end says its wire first** (2026-09-29). `Hello` and `HelloAck` carried nothing that
  named a build, and `WorkerCaps.version` sat inside the `HelloAck`. When two builds' postcard
  layouts differed, that message did not decode, so it could not be read. The decode error
  dropped the link and the redial dialled it again forever. `slopty worker deploy` puts
  workers on other machines, so they drift from the app. The product is pre-release and every
  binary is rebuilt together, so there is no protocol version for a person to bump.
  - **The fingerprint is derived from the goldens.** `crates/slopty-proto/build.rs` hashes
    every `.snap` under `tests/snapshots` (FNV-1a 64, by file name and body) into
    `slopty_proto::wire::FINGERPRINT`. The control socket's goldens (`golden__ctl__*`) are
    left out because no link carries them. The insta header is left out because it names a
    test's source line and not the wire. A changed golden is already a wire change, so the
    fingerprint moves exactly when one is accepted. Nobody bumps it, and cargo reruns the
    script when the directory changes. `BUILD` is the version plus the fingerprint's first
    eight hex digits (`0.1.0+wire.f6acd634`), for a person to read. `git describe` was
    rejected: tracking the git state would rebuild `slopty-proto`, and everything that
    depends on it, after every commit.
  - **A prefix that never changes opens the control stream both ways**, before any postcard:
    `SLOPTY`, the fingerprint (`u64` little-endian), one length byte and the build text
    (`wire::Prefix`, pinned by `golden__wire__prefix` with fixed values so that golden does
    not feed the fingerprint). Each dialer writes its prefix, then its hello, then reads the
    peer's prefix. Each listener writes its prefix, then reads the dialer's before the hello
    (`slopty_net::prefix`). That covers client ↔ worker, client ↔ server and worker ↔ server,
    because every control stream goes through `slopty_net::{client, server, listen}`.
  - **On a mismatch the end that sees it closes with `close_code::WRONG_BUILD` (4)**, with
    its own build as the reason. The other end reads the prefix or the close, whichever comes
    first, as `NetError::WrongBuild { peer }`. A stream that opens with anything but the magic
    is a build from before the prefix. It is closed the same way, with an empty `peer`
    ("an older one"). A listener never hands such a peer on, and logs it at `warn` with both
    builds. A new dialer against a pre-prefix listener only sees that listener's decode
    error. That happens once, on the way over.
  - **Nobody redials into it.** `slopty_net::redial::WRONG_BUILD` (60 s) replaces the fast
    backoff for a peer that says it runs another build. A dial before then can only be
    refused, and the peer changes only when someone updates it. The client's server link,
    the worker's server link and the CLI's persistent link wait that long. The app's worker
    loop is meant to wait for a wake instead: the Connect action, or the directory saying the
    worker came back, which a redeployed worker does when it re-registers.
  - **What a person reads** is `slopty_client::update::UpdateNotice`: "This worker runs a
    different build", both builds, and the command that updates it. For a worker that is
    `slopty worker deploy <host> --update`. For the server it is `slopty server install`, run
    on its machine. The CLI prints the notice's `Display`, and the app shows the same words
    on the worker. The deploy lives in the `slopty` binary (`apps/slopty-cli/src/deploy.rs`).
    The app cannot run it until it moves into a library, so the notice gives the command to
    copy.
  - Tests: `the_fingerprint_is_the_goldens_and_stable` and
    `the_fingerprint_moves_exactly_with_a_wire_golden` (proto) cover the derivation. Prefix
    reading is in `a_prefix_reads_back_whole_and_waits_for_the_rest` and
    `anything_else_is_not_slopty_at_once`. `crates/slopty-net/tests/wrong_build.rs` runs both
    directions and a pre-prefix peer over loopback. `a_wrong_build_close_names_the_peers_build`
    covers the error mapping. `crates/slopty-client/tests/wrong_build.rs` checks that a worker
    dial becomes the notice, and that a server on another build is reported once with no
    second dial within the fast backoff's first three steps.

- ✅ **BBR3 on app-limited video: pace from the first round trip, cap the aggregation
  allowance, and let a flow's own quiet round trips stand for ProbeRTT; the bound stays**
  (2026-09-30, MEASUREMENTS.md "BBR3 on app-limited video, the batched Apple datapath,
  1232-byte packets"). Video is application-limited: it sends a frame and waits for the next.
  Three things in noq's BBR3 cost it, and `vendor/noq-proto/SLOPTY.md` patches 11 to 13 take
  them, each with a unit test that fails without it.
  - *First-RTT pacing (noq#800, finishing noq#802 and quinn#2481).* The constructor paces at
    `2.77 × initial window / 1 ms`, Startup only raises it, and an app-limited flow never
    leaves Startup. The first ACK now sets the rate from its packet's own round trip, as Linux
    does on `has_seen_rtt`. The open PRs read the estimator, which does not yet hold that sample
    when the controller sees the ACK (patch 6).
  - *The `extra_acked` cap.* 100 ms at `BBR.bw`, Linux's `bbr_extra_acked_max_us`. The
    2026-09-25 entry left it out because it did not bind once patch 2 cleared the count. It is
    taken now as a guard, since any other runaway would grow the window with it.
  - *The ProbeRTT skip.* The first packet of each frame leaves with no more in flight than
    ProbeRTT allows. Its round trip is the sample ProbeRTT takes by holding the window at half
    a BDP for 200 ms, so an expired filter refreshes from it and the flow never dips. A flow
    never that quiet still probes. This goes beyond the draft, whose only skip is a restart from
    idle. quiche's `avoid_unnecessary_probe_rtt` only spreads the dips out.
  - *The probe-up exit was not taken.* The draft and Linux keep an app-limited flow in
    `ProbeBW_UP`, and with the three above the simulated keyframe p99 already equals Cubic's.

  Keyframe p99 against Cubic, simulated over eight seeds: 139 ms unpatched, 55 ms patched and
  55 ms for Cubic, which is the keyframe's own serialisation. `video_beside_keys_keeps_its_keyframes_out_of_probe_rtt`
  (`crates/slopty-net/tests/sim.rs`) holds keyframe p99 under 80 ms with no ProbeRTT and no
  Startup sample, reading the model's state from `Snapshot::model_state` (patch 14). In real
  time on this machine, at load 12 to 41, the four configurations could not be told apart
  (keyframe p99 139 to 171 ms, 5 to 7 % over 80 ms, for all of them): the machine's scheduling
  is the tail there, and the simulation is the number of record.

  *The bound stays, for a new reason.* noq#800 was the case it guarded, and it is fixed. But
  unbounded, noq's window sat at 50 to 75 kB against the bound's 22 kB, and an echo behind it
  waited: echo p99 31 against 10 ms simulated, 31 against 15 and 27 against 20 ms with 1 and
  3 ms of jitter. The unbounded flow also opened in Startup for about 2 s of real time. Keyframe
  times were equal. The bound costs video nothing and keeps a key's echo out of video's queue.

- ✅ **An ACK waiting on the delayed-ACK timer rides the next packet that leaves on its path**
  (2026-09-30). Port of quinn#2747 (open) as noq-proto patch 15. An echo now carries the ACK of
  the key it answers, so the worker sends 1.06 packets per key instead of 2.02
  (`an_echo_carries_the_ack_of_its_key`, sim section (g)). The ACK goes only on its own path
  and only when it fits, so a packet never grows. noq's `is_ack_only` did not count
  OBSERVED_ADDRESS, so a debug assertion took such a packet with a bundled ACK for an ACK-only
  one; that is fixed in the same patch.

- ✅ **The batched Apple datapath is on** (2026-09-30, supersedes the 2026-09-04 ban). The ban
  rested on a 53 ms loopback round trip measured through iroh. Without iroh, over noq with
  quinn's two fixes vendored (`vendor/noq-udp/SLOPTY.md`: quinn#2727, the `SO_SNDBUF` floor
  against macOS's permanent `EWOULDBLOCK`; quinn#2748's udp half, a partial `sendmsg_x`
  reported rather than dropped), the clear-link echo reads the same on both paths (p50 0.42 to
  0.99 ms against 0.50 to 1.06), and the worker's datagram send costs 23 % less CPU (1566
  against 2044 µs per 64-datagram frame). The connection half of #2748 is Slopty's own socket
  (`crates/slopty-net/src/udp.rs`): a transmit that went out in part is finished on the next
  try, even across a wait for room, and only the same transmit resumes. noq's runtime never
  enables the path, and `slopty-net` forbids `unsafe`, so noq-udp gained a safe
  `try_enable_apple_fast_path` that checks both symbols resolve. macOS only; iOS never builds
  the private calls. `SLOPTY_BATCHED_UDP=0` turns it off to measure against. Tests:
  `a_long_transmit_arrives_whole_on_the_batched_path`, `a_partial_send_resumes_only_its_own_transmit`,
  the three in `vendor/noq-udp/tests/tests.rs`, and
  `a_round_trip_over_the_shipped_socket_takes_a_millisecond_not_fifty` (`tests/loopback.rs`,
  through `endpoint::bind`: median 0.12 ms against the 5 ms bound).

  *What no test here reaches.* A kernel short send that is then resumed after a wait for room.
  Over loopback the kernel hands each datagram to the receiver at once and drops it there when
  that buffer is full, so the send buffer never fills: 1 000 000 datagrams in ten-datagram
  `sendmsg_x` calls, at the default, 70 kB and 300 kB of `SO_SNDBUF`, gave no short send and no
  `EWOULDBLOCK` (`loopback_short_sends`, ignored; the one wait it prints is the first poll, while
  tokio learns the socket is writable). A real interface's full queue answers `ENOBUFS`, which
  the socket counts as a lost datagram. The resume logic is tested against a batch longer than
  `sendmsg_x` takes, which noq never builds.

  *Distribution.* `sendmsg_x` and `recvmsg_x` are private and reached by `dlsym`. Mac App Store
  review rejects private calls, so a Store build would have to leave the feature out
  (`slopty-net`'s macOS dependency line). Slopty is not distributed there today.

- ✅ **Every packet is 1232 bytes, and the socket holds 4 MiB** (2026-09-30). 1232 is 1280,
  the least any IPv6 link carries and Tailscale's TUN MTU, less 48 bytes of IPv6 and UDP. It is
  noq's initial MTU and ceiling, and discovery is off. The minimum stays QUIC's 1200: an IPv4
  path with an MTU of 1228 to 1259 (L2TP or IP security VPNs, some cellular links) carries the
  handshake and path challenges, which are padded to 1200 only, and then loses every full
  packet. With the minimum at 1232 black-hole detection had nowhere to fall back to, and 256 kB
  took 6.4 s over such a path in the simulation; at 1200 it detects the black hole and takes
  136 ms (`a_path_narrower_than_the_packets_falls_back_to_the_minimum`, a 1240-byte MTU, eight
  seeds). A media datagram of 1200 bytes does not fit such a path at all (1178 bytes of room),
  which is the video sender's to handle. When the local interface itself is narrower, the OS
  refuses the send with `EMSGSIZE`, which no loss detection sees, so the socket says it once
  at `warn` (`a_datagram_too_large_for_the_interface_is_lost_not_fatal`). The 1252 ceiling fitted IPv4
  inside a tunnel only: its probe was lost on every IPv6 tailnet path, which left the MTU at
  noq's 1200, and a media datagram (up to 1200 bytes) needs 1210 of room. Before, the first
  `max_datagram_size` was 1178 bytes; now it is 1210 from the handshake
  (`a_full_media_datagram_fits_from_the_first_packet`). `SO_RCVBUF` goes from
  macOS's 786 896 bytes to 4 MiB (`kern.ipc.maxsockbuf` allows 8): 1.5 MB of datagrams sent to
  a socket nobody reads, a 5K keyframe while the receiver task is late, kept 623 of 1 217 before
  and all after (`the_socket_holds_a_large_keyframe_while_nobody_reads`). The 2026-09-26
  finding that a 1080p keyframe fits the default still holds; a 5K one does not.
  `SLOPTY_UDP_RCVBUF` overrides it, `0` for the OS's. The OS clamps a buffer to
  `kern.ipc.maxsockbuf` without a word, so the endpoint warns when it got less than it asked,
  and logs both buffers' effective sizes at `debug`; iOS's limits are not measured.

- ✅ **quinn#2794, #2839 and #2735 are not ported** (2026-09-30). #2794 changes the ACK of a
  loss probe when the peer lacks ACK frequency; Slopty's peers all have it, so the probe carries
  IMMEDIATE_ACK already. #2839 prunes queued datagrams when a path reset or migration shrinks the
  MTU; here a reset only ever returns to the initial 1232, so only its off-by-one is taken
  (noq-proto patch 16: a datagram exactly at the new limit is kept). #2735 cuts allocations when a frame's datagrams are queued: about 64 a frame, some
  2 µs of the 2.47 ms of CPU a frame costs, and it would mean vendoring `noq` itself.

- ✅ **One key in eight over 50 ms on the mesh was loss on the way in** (2026-09-30). The
  2026-09-30 library audit asked for a per-hop trace of the 2026-09-25 mesh finding. That trace
  was already taken the same day (MEASUREMENTS.md, "datagram copies of a keystroke and its
  echo"): the client lost 20 to 93 packets per 100 idle keys on the way to the MacBook while the
  worker lost at most 4, and a lost lone key waits for the probe timeout. The worker "counted no
  loss" because the loss was the client's. Datagram copies and the pacer patch took keys over
  50 ms from 13.5 to 0.5 %. What is new is the instrument: `describe_path` now ends with the
  connection's own `lost N of M sent`, so either end's log names the lossy direction.
- ✅ **A resume probes every link at once** (2026-09-30, MEASUREMENTS "a dead link found by a
  resume"). Until now only a link's silence said it was dead: "silent" after three missed
  keep-alives and given up after five (`SILENCE_DROP`), then dialled again, so a Mac that woke
  with its links dead showed stale tiles for about 6.6 s. The system knows sooner.
  `slopty_platform::resume` hands the app a `Resume` when the Mac wakes, its screens wake, its
  session comes back or is unlocked, the app comes to the front, or Network.framework's path
  monitor sees another path (another interface or gateway; the monitor repeating the same path
  is no change, `PathLog`). On iOS the same comes from the scene entering the foreground, the
  app becoming active and protected data becoming available. Linux has no source yet: its seam
  is logind's `PrepareForSleep` and a netlink route watch.
  - **On a path change the connections migrate first** (noq's `handle_network_change`, RFC 9000
    §9), so a link that survives the move is probed on the new path and never dialled again.
  - **Every live link is probed at once**: a QUIC PING, and the link is alive on the first
    datagram back. The probe waits four round trips, clamped to 0.25–1 s (`workers::Probe`).
    Past the ceiling a new handshake costs less than more waiting.
  - **The tiles are dimmed while the link is in doubt, never emptied**: at once when the device
    was away or the path moved, and for a return to the front only once the probe outlasts two
    round trips (one frame at the least), so switching apps never flickers a healthy tile. The
    dimming has no word or spinner (`WorkerStatus::Checking`, `Relinking`, `in_doubt`).
  - **A probe with no answer relinks at once, make before break.** The old link's views stay,
    dimmed, until the new link's views replace them in one update, so no frame shows the tile
    empty or the worker away.
  - **A worker between links is dialled at once** rather than at the end of its backoff. The
    2026-09-30 review found two ways a resume could be lost in the half second before a link's
    check notices its link has ended: the resume reached a check whose link was already gone,
    or a probe outlived its link. Either way the connect loop sat out its backoff (up to 2 s).
    Both now wake the connect loop. A wake left over when the relink already ran costs at
    most one dial without backoff.
  - **Many workers resume together, unstaggered.** A probe is one PING and a relink one
    handshake of a few datagrams on the process's one socket. A stagger would add its step to
    every worker after the first, and the aim is every tile live together.
  - **A transfer rides its link.** A link that answers keeps its transfers, and a path change
    migrates them with the connection. A transfer on a link found dead fails as it did when
    the silence found it, only sooner. Transfers are per link (`xfer::Table`), so none carries
    over to the new one.
  - **Repeats collapse.** Resumes that arrive while a probe runs are answered by it (iOS posts
    two on every return to the foreground). A return to the front on macOS, which comes with
    every ⌘⇥, costs one PING per link.
  - Measured on the real app and worker, behind a UDP proxy the test cuts (`slopty_e2e::cut`):
    twenty deaths, each followed by the resume the system would send, against five deaths left
    to the silence. Numbers and command in MEASUREMENTS.
  - Tests: `a_resume_brings_a_dead_link_back_at_once` (`slopty-e2e`, app),
    `a_link_in_doubt_sets_its_tiles_back_until_it_is_live` (`slopty-ui`),
    `a_probe_is_sized_from_the_round_trip` and `a_resume_probes_a_live_link_and_dials_a_dead_one`
    (`slopty-app`), and `a_path_changes_only_when_what_a_link_rides_on_does`,
    `an_injected_resume_reaches_its_watch_while_it_lives` and `the_system_watch_starts_and_stops`
    (`slopty-platform`).
  - *What no test here reaches.* The system posting its notifications, because no test sleeps
    the Mac or moves its network, and the two lost-resume races, which need a resume to land
    within one tick of a link ending.
- ✅ **A dropped link keeps every tile and takes it up in place** (2026-10-01, readiness audit
  A1). Only the resume path kept its tiles: a link given up after its silence, a worker that
  restarted, or one the server said went away dropped every shell, window and agent view of
  that worker, so its tiles went blank under "Reconnecting" until the next link replayed.
  Every cause now goes through the same keep (`WorkspaceView::disconnect_worker`).
  - **Shells keep their view.** The tile shows its last rows, set back under the away pill. The
    next link hands the same view its sender and clipboard (`TerminalView::relink`), and the view
    attaches again at the size it is laid out at. A restarted worker starts its frame numbers
    over and may number the lines afresh under the same epoch, since it replays its checkpoint
    into a new engine. So the new stream's first frame is taken whatever its number, and the
    lines held are put aside then, never shown under the new numbering
    (`TermState::relinked`). What waited for room on the old link and the local-echo guesses
    made over it are dropped: no worker heard them. Scroll place does not survive a relink.
  - **Windows keep their last picture**, set back. The next link opens the stream again behind
    it. The stale view hears none of the new link's streams, whose ids start over, and gives
    the tile up in one step once the new stream has a picture, or once the worker says how its
    target stands (`Worker::fresh_screens`, `stale_screens`). No frame shows the tile empty.
  - **Faces stay** as before, draft and all, and follow again on the new link. The worker's own
    word on an agent still goes with the link, so the server's stands in while it is away.
  - Tests: `a_dropped_link_keeps_each_shell_and_the_next_one_takes_it_up_in_place`,
    `a_dropped_window_keeps_its_picture_until_the_new_stream_has_one`,
    `a_face_stays_through_a_dropped_link_with_its_draft` (`slopty-ui`), and
    `a_relinked_stream_starts_over_and_trusts_no_held_line` (`slopty-client`).
- ✅ **A resent Initial is not a stateless reset** (2026-10-01, MEASUREMENTS "the fill's dials
  after the port fix, and a reset by peer"). The fill's dials that got no answer stayed fixed
  ("A dual-stack port no IPv4 socket holds"): four soaks, about 47 000 CLI calls, none failed to
  reach the server. The same load in one process turned up a second, rarer failure. About one
  fresh dial in 100 000 on a loaded machine failed at once with "stream: reset by peer", when
  the client opened its first stream. The server had sent no stateless reset to any of those
  client ports. The client took one of the server's own packets for one.
  - **How.** The null crypto has no AEAD tag (`slopty_net::crypto`, `tag_len` 0), so a packet
    ends in whatever frame it carries last. The server's HELLO is its transport parameters,
    which noq writes in a shuffled order (`transport_parameters.rs`, `order.shuffle`). One
    time in about twelve the stateless reset token comes last, so the HELLO ends in the token.
    The server's first flight coalesces its Initial ahead of Handshake and 1-RTT packets. The
    padding goes to the last packet (`finish_and_track(…, PadDatagram::No)` for the others), so
    the Initial ends in the token. noq's `unprotect_header` compared the last 16 bytes of every
    packet with the peer's token, a long-header packet coalesced ahead of others included, and
    `handle_packet` took a match for a reset even when the packet decrypted. The first copy is
    read before the client knows the token and passes. A resent copy that the client reads
    before it answers the first is taken for a reset. Once the client has sent a Handshake
    packet it drops Initials unread, so only a slow client meets the resend: the server's
    probe timeout is a few milliseconds, and a loaded machine answers later. A probe in the
    stressed run showed it: a first flight, then 10 µs later a 173-byte unpadded Initial and
    `got stateless reset`.
  - **The fix is noq-proto patch 17** (`vendor/noq-proto/SLOPTY.md`). Only a short-header
    packet is compared. A stateless reset takes that form (RFC 9000 §10.3), and a short-header
    packet runs to the end of its datagram, so those are the datagram's trailing 16 bytes that
    §10.3.1 compares. No genuine reset is missed. Behind an AEAD a packet ends in its tag, so
    upstream never goes wrong, and noq and quinn still compare every packet with no pull
    request about it. The patch is correct for them too and small enough to offer.
  - **Rejected: a tag on the null crypto.** A keyed checksum would end every packet in bytes
    no frame writes. It would cost bytes and hashing on every packet of the input and frame
    paths to guard a check that belongs to noq.
  - **Not taken: skipping the check for a packet that decrypted and was routed by one of the
    connection's own IDs** (the RFC allows it). Under the null crypto every packet decrypts, so
    this would close the last way a real packet can end in the compared token. That way needs
    a short-header packet whose last frame is the NEW_CONNECTION_ID for the ID in use. The
    client compares the server's handshake token, which no NEW_CONNECTION_ID carries. The
    server compares the token of the client's sequence-1 ID, and noq writes the client's
    NEW_CONNECTION_ID frames in ascending sequence (`PendingNewCids::pop`), so that frame ends
    a packet only when it is sent alone. No run or seed here produced one, and the skip needs
    the endpoint to pass the routing to the connection.
  - Tests: `a_long_header_packet_ending_in_the_token_is_no_stateless_reset` and
    `a_short_header_packet_ending_in_the_token_is_a_stateless_reset` (noq-proto,
    `packet_crypto.rs`). `a_path_that_delivers_every_datagram_twice_still_connects`
    (`crates/slopty-net/tests/sim.rs`) runs a dial on a simulated path that delivers every
    datagram twice. Its seed 7 failed with "reset by peer" before the fix, and so did 25 of
    300 seeds; 0 of 1 000 fail after it. The nightly sweep runs it on 200 seeds.

- ✅ **The receive assembler's chunks are bounded: quinn's RUSTSEC-2026-0185 fix as noq-proto
  patch 18** (2026-10-02). quinn-proto 0.11.15 to 0.11.18 bound how many chunks a stream's or
  CRYPTO's `Assembler` holds. Without the bound, a peer that sends small frames with gaps keeps
  every one of them in memory while the reader waits for the gap. noq-proto 1.3.0 forked before
  the fix, and its new name hides it from `cargo audit`. n0-computer/noq#828, a draft, ports the
  three quinn commits unchanged. Slopty takes its diff as it stands rather than waiting for a
  release (`vendor/noq-proto/SLOPTY.md`, patch 18).
  - *Why it applies here.* Every worker is reached over the tailnet, but a compromised or buggy
    host is still a peer that could grow a client's memory without limit, and so is a client
    to a worker. The wire adds no encryption or pairing of its own, so the transport
    has to bound what any peer can make it hold.
  - *What it costs.* A peer past 1024 chunks after defragmenting is closed with
    `INTERNAL_ERROR`. Slopty's streams are read as they arrive, so no legitimate stream comes
    near that. Coalescing contiguous chunks keeps a slow reader on a large window from being
    refused.
  - Tests: quinn's assembler tests, in `assembler.rs` (noq-proto, 453 of its tests pass with
    the patch).

- ✅ **A Linux socket's receive buffer** (2026-10-03). The endpoint asks for a 4 MiB
  `SO_RCVBUF` ("Every packet is 1232 bytes" above). Linux caps that at `net.core.rmem_max`,
  208 KiB on Ubuntu, and reports double what it set, so the check against the ask was wrong
  both ways there.
  - **What the endpoint does.** It halves what Linux reports before comparing (`socket(7)`: the
    other half is the kernel's bookkeeping). When the grant is short it asks again with
    `SO_RCVBUFFORCE`, which a process with `CAP_NET_ADMIN` may set past the cap (a worker or
    server run as a system service by root) and which anyone else is refused. Then it warns
    once per socket, naming the setting that caps it: `net.core.rmem_max` on Linux,
    `kern.ipc.maxsockbuf` on macOS. This is what quic-go does.
  - **Why not more.** A user install (systemd user units) cannot set a sysctl, and Slopty never
    asks for root. A Linux worker or server receives no media, only input, control and
    uploads, which QUIC's flow control paces, so a short buffer there costs a retransmission,
    not a frame. The 4 MiB is for the client that receives keyframes, which runs on macOS.
  - **The test.** `the_socket_holds_a_large_keyframe_while_nobody_reads` holds on Linux where
    the host allows 4 MiB. CI's Linux job sets `net.core.rmem_max` to 8 MiB, as macOS allows,
    and the failure message names the setting to raise. Docker Desktop's VM allows 4 MiB.

- ✅ **BBR3's max_bw filter advances once per probe** (2026-10-05, MEASUREMENTS "a connection's
  second bulk stream"). A connection's later bulk streams under loss ran ten to twenty times
  slower than its first. noq's BBR3 left `ack_phase` at `ProbeStopping` after a probe, so it
  advanced the `max_bw` filter on every round start. The filter's two-cycle window became two
  round trips, `max_bw` followed the delivery rate down, and the loss cuts to `bw_shortterm`
  ratcheted the two together. noq follows draft-05, which lacks the step. Linux `tcp_bbr.c` v3
  and the draft's editor's copy move `ack_phase` to `ACKS_INIT` and clear `bw_probe_samples`
  once the probe's feedback ends. noq-proto patch 19 does the same.
  - *Patch 20, found by it.* Holding `max_bw` exposed ProbeBW_UP ending on the plateau
    `inflight_longterm` held, because noq compared `cwnd` with it in bytes after
    `inflight_longterm` had already grown by that ack. Linux compares in packets. The fix
    counts `cwnd` within one SMSS of it as at it.
  - *Upstream.* No open noq pull request or issue covers either. Both are written up for
    noq in `.research/noq-upstream-prs.md` (sections 15 and 16).
  - *Not changed: `LOSS_THRESH`.* Above BBRv3's 2 % loss threshold, every probe still lowers
    `inflight_longterm` and every lossy round lowers `bw_shortterm`, so under the e2e's
    uniform 3 % the rate still slides across streams, more slowly. That is BBRv3 as designed
    for a path that loses that much. A real tailnet loses far less, and Slopty keeps
    upstream's threshold.
  - Tests: `the_max_bw_filter_advances_once_per_probe` and
    `probe_up_goes_on_while_inflight_longterm_holds_it` (noq-proto, 455 of its tests pass);
    `crates/slopty-net/tests/bulk_twice.rs` measures it end to end.

- ✅ **A forwarded verb's deadline covers its send** (2026-10-10, readiness 10-10 rank 21).
  `Hub::forward` awaited the worker link's `send` before its timeout started. A worker whose
  link stayed up and did not drain (its 256-deep queue full) held every verb forwarded to it
  with no limit. The deadline now runs from before the send, and the send waits inside it. One
  the link never took fails with `WorkerUnreachable`, and says the request was not sent, so a
  caller knows a retry cannot do it twice. An answer that does not come still fails with
  `Interrupted`, as before.
  - Test: `hub::tests::a_link_that_does_not_drain_holds_a_forward_no_longer_than_its_deadline`
    (slopty-server).

- ✅ **A build mismatch says which side is older** (2026-10-11, readiness 10-11 rank 6, W half).
  - **The defect.** "It runs X; this build is Y" always told the person to update the other end.
    So two Macs on different builds would deploy each other back and forth, and a newer worker
    could be downgraded.
  - **Stamped builds.** `BUILD` now ends with when its wire last changed, in UTC:
    `0.1.0+wire.0badf00d.20261011T0812Z`. That is the time of the last commit to touch the
    goldens, or the time of the edit while they differ from it.
    - The build script reads it with git, and only when the goldens change (as it reads the
      fingerprint), so a commit rebuilds nothing.
    - Without git, the part is left off.
    - The prefix's layout is unchanged.
  - **Which is newer.** `WrongBuild::newer` (slopty-net) compares versions, then the stamps.
    - A peer that says no build is older than any.
    - Two builds it cannot tell apart answer `None`, and the person is asked rather than told.
  - **What the person is told.**
    - `UpdateNotice::newer` and `this_is_older` give the app what it needs to hide Update when
      this device is the older one.
    - The CLI's notice then reads "runs a newer build … Update Slopty on this machine to match
      it" and gives no command.
    - A worker refused by the server names the side to update.
  - Tests: `the_newer_build_is_told_by_version_then_by_its_wire_s_date` (slopty-net),
    `a_newer_peer_says_to_update_this_machine` (slopty-client `update.rs`), and the build test in
    slopty-proto `wire.rs`.

- ✅ **A failed dial says what kind of failure it was** (2026-10-11, readiness 10-11 rank 5, W
  half).
  - **The defect.** A machine that did not answer and one that turned this device away by its
    `[worker] allow` ranges both reached the person as `connect: 100.x:45550: …` text. Nothing
    in that text tells them what to do.
  - **New errors.** `NetError::NoAnswer` is a handshake that timed out, or a connection that
    timed out before it was made. `NetError::Refused` is QUIC's `CONNECTION_REFUSED`, which the
    listener sends at once to a peer outside its admitted ranges. Neither is `Connect` text any
    more.
  - **One kind per failure.** `NetError::unreached()` sorts every failure about the peer into an
    `Unreached` kind: no such host, no answer, refused, not granted, wrong build, or dropped once
    linked. It is `None` for a local failure. The app maps each kind to a sentence and the action
    that fixes it (lane A), and the raw chain stays in the log.
  - Tests: `a_peer_outside_the_admitted_ranges_is_refused_before_the_handshake` and
    `an_address_with_nothing_behind_it_fails_in_seconds` (slopty-net `tests/loopback.rs`).
