# Decisions — Transport

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **iroh 1.2.0** (1.1.0 verified from source in the cargo registry 2026-09-04; 1.2.0 adopted
  2026-09-12 the day after its release: `n0-dns-resolver` replaces hickory, nothing in our API
  surface moved, gate + app/iOS e2e green). QUIC via n0's own `noq` 1.3.0 underneath (a quinn fork; **not** the `quinn` crate, so quinn docs/types do not
  apply): streams for control/terminal, unreliable datagrams for media, connection migration for
  Wi-Fi↔cellular, hole punching + relay fallback, E2E encryption keyed by endpoint identity.
  Rejected: slop-desk's raw UDP + "the WireGuard mesh is the security boundary" (phone-anywhere
  needs app-level auth); WebRTC (ICE/SDP dead weight); MoQ (relay/pub-sub shape).

- ✅ **Default features off; `fast-apple-datapath` is banned.** `default-features = false,
  features = ["tls-ring"]`. iroh's default set includes `fast-apple-datapath` (dlsym of private
  `sendmsg_x`/`recvmsg_x`, batched UDP). Measured 2026-09-04 on loopback (`slopty ping`, see
  MEASUREMENTS.md): with it on both ends the app round trip is **53 ms** and QUIC's own RTT
  estimate 48 ms; with it off, **0.8 ms**. The batching path waits to fill batches, which is
  exactly wrong for keystrokes. The feature no longer exists in `slopty-net`; do not re-add it.

- ✅ **LAN discovery** is a separate crate, `iroh-mdns-address-lookup` 0.5.0 (there is no
  `discovery-local-network` feature in iroh 1.x). Behind `slopty-net/mdns`; hosts advertise,
  clients only look up. Wide-area lookup is iroh's `presets::N0` (DNS + pkarr on n0's infra).

- ✅ **Pairing**: `PairTicket { addr, token }` (`iroh-tickets` 1.0.0, base32 string, kind
  `sloptypair`), token single-use with a 10-minute TTL. On redeem the host trusts the client's
  endpoint key in `trust.json` (mode 0600); afterwards the QUIC handshake is the whole
  authentication and `Hello.pair_token` stays `None`. Rejections are sent then linger up to 2 s
  so the client reads `Rejected` before the connection closes (QUIC close discards unread data).

- ✅ **One connection per client**: bidi control stream opened by the client; one uni stream per
  attached session opened by the host, first message `StreamHeader { session }`; datagrams for
  media. Transport config: idle 45 s, keep-alive 5 s, 4 MiB datagram buffers.

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

- ✅ **QUIC datagrams, not raw UDP over a WireGuard mesh** (re-examined 2026-09-04 when the
  status bar showed ~100 ms). A QUIC datagram is one UDP packet plus ~30 bytes of header and one
  AEAD (hardware AES-GCM); WireGuard spends the same per packet on ChaCha20, so "UDP + WG" moves
  the crypto, it does not remove it. Measured: app RTT 0.8 ms on loopback, 0.9–2 ms on the LAN
  direct path; the ~100 ms readings were the *relay* path (n0's aps1 server) while iroh had
  dropped the direct path (see below). slop-desk's transport doc admits its numbers were
  loopback-only and the WireGuard case was never measured. What QUIC buys: reliable streams and
  unreliable datagrams on one connection, migration, NAT traversal and relay fallback for a
  phone off the mesh, and app-level auth instead of "the mesh is the boundary".

- ✅ **Direct-only mode** (`slopty_net::Reach::DirectOnly`; hostd `--direct-only`, every binary
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
  second (`HostLink::received_datagrams`); keep-alive pings make a live host send something
  every 5 s, so a counter that stands still for `SILENCE_WARN` = 8 s turns the RTT readout into
  a yellow "host silent Ns". Verified 2026-09-05 by `SIGSTOP`ping hostd. Since the same day the
  app also *gives up* at `SILENCE_DROP` = 15 s (three missed keep-alives, noq's own path bar):
  `HostLink::abandon` closes the QUIC connection, the control reader reports `Disconnected`,
  and the normal reconnect loop runs, so a restarted host is back ~2 s after it answers instead
  of after the 45 s idle timeout. Measured with a frozen hostd: dropped at 15.2 s, reconnected
  2 s after `SIGCONT`. Reading the logs afterwards: a killed app's *old* connection still shows
  up on hostd as "connection lost: timed out" 45 s later; that is the stale one, not the new.
  Driving note: cliclick `kp:return` never reaches the app (osascript `keystroke return` does).

- ❌ **Machine load alone does not flap the path** (2026-09-06), closing the 2026-09-04
  investigation of one connection that went direct → relay-only for 43 s → direct while the
  machine was compiling. iroh's `BiasedRttPathSelector` always prefers a live direct path, so
  the direct path had to have been *closed* (noq abandon reasons `TimedOut` = 15 s path idle
  with 5 s heartbeats, `UnusableAfterNetworkChange`, `RemoteAbandoned`), and the standing
  hypothesis was that a busy machine starves the QUIC driver task until the heartbeats miss.
  A harness now says otherwise: `path_flap_under_{cpu,user_initiated_cpu,memory_io}_load`
  (`apps/slopty-hostd/tests/e2e.rs`, gate `SLOPTY_FLAP_E2E`) runs hostd and a client over iroh
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
  matches, and `Peer::drop` in hostd uses it; `client_gone` is gone. Regression test
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

- ✅ **hostd binds a fixed UDP port** (2026-09-05): `slopty-hostd --port` / `SLOPTY_PORT`,
  default `slopty_net::endpoint::HOST_PORT` (45550), IPv4 required, IPv6 best effort. Found
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

- ⚠️ **Never await iroh's `Endpoint::close` on GPUI's executor.** It uses `tokio::time::timeout`,
  which panics (`Handle::current`) outside a tokio runtime context; the app aborted on every
  disconnect until the close was spawned onto the runtime and joined (crash report
  2026-09-04). Rule: anything from iroh/tokio-time runs via `runtime.spawn`, GPUI tasks only
  await join handles and channels.

- ⚠️ **One iroh endpoint per process** (found by the app self-test 2026-09-05). The app used to
  bind an endpoint to pair, close it, and bind a second one with the same secret key to
  connect; the second dial hung until the step timeout (the host never saw its packets;
  iroh's discovery/relay state for that key was still the old endpoint's). `slopty-app::net`
  keeps a process-wide `OnceCell<Endpoint>` used by both `pair_host` and `connect_to`, and
  never closes it.

- ✅ Deployment is two LaunchAgents written by `slopty host install` (`plist` crate, XML):
  `KeepAlive` + `RunAtLoad` + `ThrottleInterval 2` so a crash comes back in 2 s and login
  starts both; `ProcessType Interactive` and `LimitLoadToSessionType Aqua` because hostd
  needs the window server and ScreenCaptureKit and must not be App-Napped; sockets under
  `<data dir>/run/` (not `$TMPDIR`, which launchd children may not share) and the CLI's
  socket lookup falls back to that path when it exists. `install` re-bootstraps (bootout
  first, so a stale socket file never wedges the bind) and waits up to 10 s for a ticket.

- ✅ **"install hooks" is a pill on the title bar of the first unhooked agent, and hostd
  writes the settings** (2026-09-05). The human whose agent is being guessed at may be on a
  phone, so `ClientMsg::InstallHooks` asks the host to do it and `HostMsg::HooksInstalled`
  comes back as a notice. The installer moved from `slopty-cli` into `slopty_agent::hooks` so
  the CLI and the daemon run the same code; hostd registers the `slopty` binary beside itself
  (`Contents/MacOS` in a bundle, `target/<profile>` in a build tree). The offer shows once per
  run and comes back only if the host reports it failed. Wire, with the `AgentSource` of the
  attribution ruling above: `AgentEvent.source`, `ClientMsg::InstallHooks` and
  `HostMsg::HooksInstalled`, goldens `host_agent_process`, `host_agent_hook`,
  `client_install_hooks` and `host_hooks_installed` (all new) with `client_hello` re-accepted,
  PROTOCOL_VERSION 11 → 12.

- ✅ **A stall is the link's silence, not the source's** (2026-09-06, measured). The receiver
  counted a stall whenever nothing arrived for a stall gap, which read the capture's own quiet
  gaps as a held link: 3 / 2 / 0 / 2 stalls per 5 s on an idle machine at 0 / 20 / 50 / 100 ‰
  (MEASUREMENTS, "parity, NACK and refresh under injected loss"), enough that the gated test
  skipped its per-rate verdicts on every run. The heartbeat was supposed to prevent this — it
  says "the link is up, the source is quiet" — but a beat that is itself late leaves the same
  hole. The answer was already on the wire: every datagram carries `send_ms_lo`, the low byte of
  the host's millisecond clock, so the difference between two stamps is how long the host waited
  between sending them. `link_gap = arrival_gap − host_gap` is the link's share, and only that is
  compared with the stall threshold and charged to `stalled_ms` — RFC 3550's interarrival
  arithmetic, used for attribution rather than jitter. Three cases keep the old pessimistic
  reading, because they are cases where the receiver genuinely cannot tell: a gap past the
  stamp's 256 ms range, a retransmission (it carries the original frame's stamp), and no previous
  stamp at all. `Reassembler::stalled` also returns false while the host reports the source idle,
  which is the in-progress half — a gap that has not ended yet has no stamp to settle it. Result:
  0 stalls at every rate, and the gated test asserts its verdicts again. Tests:
  `a_quiet_source_whose_heartbeat_was_late_is_not_a_stall` and
  `only_the_hosts_share_of_a_gap_is_forgiven` (`crates/slopty-media/tests/pipeline.rs`), with the
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
    is refused exactly as before (`STAMP_SLACK`, `Stamp::{Host, Covered, Backwards}`). Past
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
  `a_stamp_reading_just_past_the_silence_still_belongs_to_the_host`,
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
  the beat, cadence kept) and `the_heartbeat_keeps_its_cadence_on_a_quiet_stream` (hostd e2e,
  `SLOPTY_SCREEN_E2E`, a minute on a stream whose target draws nothing).

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
  entirely. Paired with a **backlog** line in hostd's datagram pump, the host-side twin of
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
  owed. Every datagram now carries its enqueue time (`slopty_host::screen::Queued`, stamped in
  `Shared::push` with `host_now_us()`) and the pump reads the wait on the way out: the caught-up
  line reports `waited` p50/p95/max over the last 1024 datagrams and the worst since the last
  line, and a lone wait past 20 ms with no backlog to charge it to is logged on its own, at most
  once a second. Host-internal; the stamp never goes on the wire. Unit test:
  `the_wait_ring_reports_the_last_window_and_the_worst_once`; numbers in MEASUREMENTS.md
  "what a datagram waits in the pump's channel". The bench's `quic path (client side)` line — the feedback connection, permanently at
  `min_pipe_cwnd` because nothing measurable flows on it — is relabelled
  `quic path (client→host, feedback only)`; reading it as the media window is the mistake this
  entry exists to stop.

- ✅ **With no audio, the heartbeat is what keeps the cadence — and that is all a keep-cadence
  datagram can do** (2026-09-06). ❌ **Beating at the inter-frame gap instead of
  `HEARTBEAT_AFTER`.** The question was whether a stream carrying no audio loses its datagram
  cadence and lets the congestion window fall to the floor. Measured both ways on the same host
  (MEASUREMENTS.md, "Audio on"): with sound playing, `host_quiet` silences go to 0 — the sender is
  never quiet — and the window is at 5 808 B for **23 % of samples against 1.4–8 % on the quiet
  runs**, with 27 of its 28 long holds there and 14 stalls. Traffic does not lift the window; the
  window is low because the flow is application-limited and BBR sizes it from `bw × min_rtt`, which
  on a still desktop (~1 Mbit/s over 1.5 ms) is under a packet. More datagrams are more bursts into
  a four-packet window. The heartbeat's job is the receiver's stall clock, not the congestion
  controller's estimate, and at half the stall gap it already does that job: `host_quiet` silences
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
  `sleep_inside_the_hosts_own_silence_is_not_forgiven_twice`; both were checked against a mutation
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
  30 ms round trip. `DatagramBudget` carries the window beside the held bytes and hostd's pump
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

- ✅ **The bad link is a UDP relay the two ends speak through** (2026-09-15, `slopty-shape`). Three
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
  with -3801 however the binary is signed. `SLOPTY_E2E_HOSTD_SOCKET` points it at the launchd
  host's control socket, and it checks that host is pinned before streaming rather than after.
  Every other `SLOPTY_SCREEN_E2E` test still spawns its own daemons and so cannot capture either;
  that is older than this change and not fixed here.

- 🔬 **A dropped frame asks for a refresh only when the link could carry one.**
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

  🔬 **Not yet measured.** Everything above is the mechanism: the IDR cost, the ask on
  every drop, the starved reference and the wiring are each measured or tested, but that gating
  the ask shortens the stall is inference from them. The shaped ladder rerun is the measurement
  and it has not been done. Until it is in `docs/MEASUREMENTS.md` this stays a hypothesis with a
  test under it, not a result.
