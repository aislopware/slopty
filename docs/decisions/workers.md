# Decisions — Workers

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **The pairing store was already a list** (2026-09-05; superseded 2026-09-24 by **Hosts are
  added by address and keyed by their `WorkerId`**): `slopty_net::identity::Identity`
  keeps `hosts: BTreeMap<EndpointId, KnownHost>` in `client.json`; only the app's
  `connect_host` picked the first entry. So there is no second file and no migration:
  `net::known_hosts` reads the map (sorted by name), `net::forget_host` removes one, and a
  pairing on the panel is `Identity::remember` as before. The stored name is refreshed from
  each `HelloAck` so the switcher does not show a stale one before the link is up.

- ✅ **One link, one canvas, one loop per host; one canvas on show** (2026-09-05):
  `Workspace::spawn_host_loop` is the old connect loop with a host id: its own endpoint,
  backoff, silence check (`SILENCE_DROP` abandons the link after 15 s without a datagram) and
  event pump, writing into a `HostSlot` (status, canvas, zoom, needs-you, RTT). The loop ends
  when its slot is gone (forget). Each host owns its `canvas.json`, so one `CanvasView` per
  host and the workspace shows `active`'s; switching is a field change plus a focus, nothing
  is torn down. `Rejection::NotPaired` maps to `HostStatus::NeedsPairing` (red dot) and keeps
  retrying at the capped backoff so a fresh pairing of the same host resumes by itself (both gone
  2026-09-24 with pairing).
  Verified with two daemons on this Mac (`SLOPTY_PORT` 45570/45571, `SLOPTY_WORKER_NAME`
  "Studio One" / "Studio Two", the new env override in `slopty-worker::paths`): "Add host…"
  paired the second while the first stayed connected; each host had its own shell (`echo
  two` on one, a new shell on the other); ⌘⌥←/→ and the switcher rows swapped canvases;
  killing one daemon turned its row amber with "disconnected … reconnecting" within ~16 s
  while the other stayed green, and restarting it turned the row green again; "forget"
  removed the row and the `client.json` entry (`slopty hosts` listed one host).

- ✅ **A reconnect resumes where the reader was** (2026-09-13). A dropped link (a phone
  changing networks, a host restart) tears the canvas down and the connect loop makes a new
  one from the host's snapshot — which used to land at the origin with nothing active, so
  every drop sent the reader back to zoom 1 at (0, 0). Now the workspace reads the camera and
  the active card off the old canvas as it lets it go (`HostSlot::disconnect(status, place)`,
  kept in `HostSlot::resume`) and hands them to the new one (`CanvasView::resume_at`): the
  camera is set at once, without a flight, and the card is activated when the snapshot brings
  it — a card gone meanwhile is nothing to activate. Only these two: scroll offsets, find bars
  and reading lines are per view and a reconnect is rare enough that rebuilding them is not
  worth the state. Test: `a_new_canvas_resumes_where_the_last_one_was`.

- ✅ **Switcher is a hand-rolled overlay, not gpui-kit's popover** (2026-09-05): a
  full-window backdrop that closes on click with an occluding panel anchored under the host
  name, the same pattern as the window picker. It needs no focus handling and lays out the
  same on a 520-pt window (the host button keeps a 96-pt minimum there and the status label
  yields; verified) as on the desktop. ⌘1…⌘9 are the fit shortcuts, so hosts step with
  ⌘⌥→ / ⌘⌥← (and ⌘⇧H adds one); the Host menu names the same actions.

- ✅ **Cross-host attention** (2026-09-05): the pill and `set_badge` use the sum of every
  slot's `needs_you` (each canvas still reports its own count through `CanvasEvent::NeedsYou`);
  a tap prefers the host on show, else the first with a waiting agent, switching first. A
  banner's tag stays the session UUID (unique across hosts); `on_system_notification_response`
  asks each canvas `has_session` to find the host, activates it, then calls
  `notification_response` as before. Verified 2026-09-06 against a **real second host** on
  another Mac (`crates/slopty-e2e/tests/workers.rs`, `cargo xtask e2e workers`, macbook-pro over
  the WireGuard mesh — direct path, ~8 ms RTT): the app pairs with both hosts at once; with host
  A on show, a permission hook played to a session on host B *through the real `slopty hook`
  relay over ssh* (never a Claude Code session, never a keystroke into a shell) badges the pill
  with the cross-host sum within a few ms; a tap on the pill switches to B and focuses the
  waiting session; a `notification_response` carrying only that session's UUID routes back to B
  and reveals it. Killing host B mid-stream turns its row amber while host A keeps streaming;
  restarting B goes green and the shell reattaches (the session survives in ptyd — the vt grid
  does not outlive a hostd restart, so reattach is confirmed by live I/O, not grid replay).

- ✅ **Two-host harness hardening** (2026-09-06, from the Codex review of `claude/twohosts`,
  fixed after landing; superseded 2026-09-26 by **The two-worker e2e runs on one Mac**).
  Four holes in `crates/slopty-e2e/src/harness.rs` and `tests/workers.rs`,
  none in product code: the `Drop` guard passed unset daemon PIDs (0) to `kill -9`, which signals
  the cleanup shell's own process group and ends the script before the `pkill`/`rm -rf` that
  follow — `teardown_script` now leaves unstarted daemons out (unit-tested); the remote root was
  created and binaries copied before the `RemoteWorker` guard existed, so a setup failure leaked
  them on the second Mac — the guard is built first and every remote write happens through it;
  the ssh helpers' timeouts dropped the child future without `kill_on_drop`, leaving an
  ownerless ssh process — every ssh and gzip child is `kill_on_drop(true)`; and the suite passed
  silently when `SLOPTY_WORKER2_E2E` was set but `SLOPTY_WORKER2` empty — `worker2_gate` errors,
  naming both variables, and the test panics on it (unit-tested).

- ✅ **Two-host harness, second hardening** (2026-09-12, from re-running the suite on main;
  superseded 2026-09-26 by **The two-worker e2e runs on one Mac**).
  Three findings, none in product code. (1) The `pkill -9 -f <root>` in `teardown_script`
  matched the remote `zsh -c "<script>"` running it — every literal in the script is in that
  shell's command line — so the shell died first and ssh reported SIGKILL, failing a run whose
  scenario had passed; the pattern is now `<root>/[b]in/` (`self_excluding`), which matches the
  daemons and the hook relay under `bin/` but not the script that spells it with the brackets,
  unit-tested against the script text itself. (2) Over a fast link the ticket mint reached the
  remote hostd's control socket before it answered (`Socket is not connected`); `mint_ticket`
  retries within `STARTUP` and the last error carries the remote `worker.log` tail, which the
  guard's teardown would otherwise take with it. (3) `SLOPTY_WORKER2` is an ssh destination, not
  a machine: the `macbook-pro` alias resolves to the overlay address, which can be too slow for
  the 60 MB the harness ships (5 MB did not move in 40 s that evening; the LAN address moved
  it in 0.2 s and brought host B up in 2.8 s). The measurement records the address used
  (MEASUREMENTS "cross-host attention, second run").

- ✅ **`slopty-worker --direct-only` reads the same env spellings as the client** (2026-09-06;
  the flag is gone 2026-09-24 with iroh's relays).
  The flag was a plain clap bool with `env = SLOPTY_DIRECT_ONLY`, which rejects `1` (clap only
  accepts the flag's presence, and an env value must parse as the value type), so
  `SLOPTY_DIRECT_ONLY=1 slopty-worker` failed to start while the same variable is how the client
  and every bench select direct-only. It now takes clap's boolish parser (`1`/`true`/`yes`/`on`)
  from the env or `--direct-only[=value]`, defaulting to false, matching `Reach::from_env`.

- ✅ **A terminal survives a hostd restart: ptyd keeps a checkpoint plus the output after it**
  (2026-09-06). Before, ptyd's ring only held output produced while no host was attached, so a
  hostd restart (an upgrade, a crash, `launchctl kickstart`) came back with live shells and a
  blank grid until the program redrew. Options weighed: (a) hostd relaying every byte through
  ptyd instead of reading the master itself (a relay hop on the hot path, ruled out when custody
  was designed); (b) ptyd holding the whole output history and hostd replaying it (unbounded,
  and a replay of a day of output takes seconds); (c) hostd handing ptyd a copy of each read
  (`PtydRequest::Output`) and, when the session has been quiet for 500 ms or 1 MiB has been
  tapped, the engine's whole state (`Checkpoint`), which empties the ring; `Attach` returns
  both and the engine replays checkpoint then ring. (c) is the ruling: the replay is bounded by
  one checkpoint plus at most 1 MiB, the hot path gains one `try_send` of a `Vec` copy per read
  on a channel a task drains onto the host's ptyd connection, and a full channel only makes the
  next checkpoint come early (taken inline, not through the timer, which the read-biased select
  could starve under a flood). The taps ride the *same* connection as the requests, and ptyd
  accepts them only from the connection holding the master: a first cut used a second
  connection, and the Codex review of it found the two races that buys — a dying host's
  buffered taps and checkpoint landing after its master connection's EOF (dropped, or worse,
  installed over the replacement's state) — plus a connection that never read ptyd's `Exited`
  broadcasts. One connection orders taps and EOF, the no-reply sends drain pending events
  first, and a failed tap send is logged without stopping the loop. The first checkpoint is
  taken right after the replay (the ring came to us with the master, so until then a second
  restart would have only the previous checkpoint), and a quiet-spell checkpoint is deferred
  while the output stands inside an escape sequence or a UTF-8 character
  (`slopty_engine::boundary::Boundary`), because it replaces the bytes before it and the rest
  of that sequence would print as text; the byte threshold forces one regardless. The
  checkpoint is libghostty-vt's own VT formatter (`Format::Vt`,
  palette, modes, scrolling region, pwd, keyboard, style, hyperlink, protection, kitty keyboard,
  charsets) with corrections, each with a test in `ghostty::checkpoint_tests`: the formatter
  only sees the active screen, so on the alternate screen the primary is snapshotted as the
  `?1049h`/`?1047h`/`?47h` goes by (`GhosttyEngine::write` splits the chunk there, and keeps
  the tail of a chunk that ends inside `ESC [ ? 10` so a switch split across two reads is
  still seen before it completes) and emitted first, then every mode the primary blob set is
  put back to its default (each blob only writes deviations, so a mode turned off on the
  alternate screen would otherwise stay on), then `?1049h` + home, then the alternate screen;
  it drops trailing blank rows, so the missing rows are fed as `CR LF` before the first margin
  sequence (`DECSTBM` or `DECSLRM`, found independently) so a scrolled primary keeps its
  history and cursor row; and its cursor comes before `DECSTBM`, which homes the cursor, so the
  cursor is emitted last, relative to the margins when origin mode is on. Tab stops are not
  emitted (their emission leaves the cursor at the last stop and shifts the content after it;
  programs do not set them). Known approximations: a pending wrap at the last column is lost
  (shared with the formatter), and soft wraps come back as hard rows (`with_unwrap(false)`
  keeps the row count exact for the padding; widening the terminal after a restart will not
  reflow lines drawn before it — ruled acceptable over a replay whose row count depends on the
  formatter's blank-row logic). The title is prefixed as OSC 0 because the formatter does not
  carry it.
  `PTYD_PROTOCOL` is 2; both daemons ship together so no compatibility path exists.
  Proved by `apps/slopty-ptyd/tests/roundtrip.rs` (tap, checkpoint, a stranger's taps ignored,
  reattach on a new connection), `crates/slopty-worker/tests/session_actor.rs` (output tapped,
  quiet checkpoint, replay into a second actor) and `apps/slopty-worker/tests/e2e.rs`
  `a_worker_restart_keeps_the_screen` (marker printed, hostd killed after the checkpoint delay,
  a fresh hostd on the same ptyd shows the marker in its full frame and the shell still
  answers). Cost: MEASUREMENTS 2026-09-06 "checkpoint cost".

- ✅ **libghostty-vt is compiled `ReleaseFast` under every cargo profile** (2026-09-06, found by
  the checkpoint cost measurement). `libghostty-vt-sys`'s build script picks the zig optimize
  mode from cargo's `DEBUG` variable, which is `true` whenever the profile keeps any debug info,
  and every profile here keeps `debug = "line-tables-only"` for symbolised panics, release and
  dist included. So every Slopty binary ever built parsed VT with a `-Doptimize=Debug` library:
  50 KB/s, 1.2 ms per 70-column line, a 10k-line `cat` taking 12 s to draw. `.cargo/config.toml`
  now sets `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast` in `[env]`, which the build script honours
  over `DEBUG` (`cargo:rerun-if-env-changed` covers it, so the change rebuilds the library); the
  Rust side keeps its line tables. Ruled against dropping the line tables (they are what makes a
  crash report readable) and against `ReleaseSafe` (ghostty's own release builds are
  `ReleaseFast`; its safety checks are debug tooling, not a contract). Before/after in
  MEASUREMENTS 2026-09-06 "libghostty-vt built ReleaseFast": 12.5 s → 3.6 ms for the same 10k
  lines. Consequence: every latency number recorded before this date that passed through the
  parser is an upper bound; the ones that matter (keystroke echo, attach replay) get re-measured
  as they come up rather than all at once.

- ✅ **Sleep policy (2026-09-12): the host stays awake while a client is attached, its display
  stays on while a window streams, and the client keeps the device awake while it shows a
  stream.** Parsec's behaviour, for the same reasons: a host that idles to sleep drops every
  session, a sleeping display captures black, and a phone that dims mid-stream is a phone you
  keep poking. Host side: `slopty_worker::wake::Wake` is a pure counter (clients, live streams)
  behind the `Holds` trait; `slopty-worker`'s `Assertions` maps it to `NSProcessInfo` activities
  (`Activity::system_awake` = `UserInitiated`, `Activity::display_awake` adds
  `IdleDisplaySleepDisabled`; `Activity` is now `Send`, NSProcessInfo being thread-safe). The
  first client joining holds, the last leaving releases; the display follows the stream
  `Registry`'s live count through its new observer (called under the registry lock, so counts
  arrive in mutation order — Codex found the race where two closes and an open could report
  `0` after `1`), so every open/close path (client close, connection drop, `close_screens`) is
  covered by construction. Nothing is held with nobody
  attached: an unattended host sleeps as its owner set it. Client side: `ScreenView` holds
  GPUI's `prevent_idle_sleep` guard (macOS `NSActivity`, iOS `idleTimerDisabled` in our fork)
  for its lifetime; a sleeping or removed window drops it. On the Mac that guard is
  `UserInitiated` (system sleep only), so `ScreenView` also holds `Activity::display_awake`:
  a viewer watching a remote window is not touching the keyboard (Codex, same review). Headless test
  `a_streaming_window_keeps_the_device_awake` reads the test platform's hold count; the host
  policy has unit tests on edges and underflow. The canvas also holds the device awake while
  any agent is `Working` or in a `Tool` (the human is waiting on it, as zed does for its agent
  panel) and lets go when every agent is idle, blocked on the human, or gone
  (`a_working_agent_keeps_the_device_awake`). Not done, ⏸ until asked: a host setting to opt
  out (`pmset` still wins over an activity for a forced sleep).

- ✅ **Hosts are added by address and keyed by their `WorkerId`** (2026-09-24, with the transport
  ruling **Plaintext QUIC on noq, standalone; iroh removed**). A host is typed as `host[:port]` —
  a Tailscale `MagicDNS` name, a LAN name or an IP, port 45550 unless given — in the app's
  add-host panel (it replaces the pairing panel; "Paste & add" for the phone) or `slopty add`.
  The client connects, says `Hello`, and stores `{ address, name, worker_id }` in `workers.json`
  in its data dir (`slopty_net::known`). The id is a UUID the worker keeps in `<data dir>/worker-id`
  and sends in every `HelloAck`; slots, canvases and the store are keyed by it, the address is
  only where it was last reached. Two consequences are handled: an address that answers with
  another id (a reinstalled host) replaces the old entry when added, and a reconnect that meets
  another id refuses to mix canvases and says to add it again. The id is `WorkerId`, not
  `HostId`: today's host becomes a *worker* under a coming control-plane server, which will also
  be what tells a client about workers. For that reason tailnet discovery (probing the peers of
  `tailscale status --json`) was written and then dropped before landing, and mDNS went with
  iroh; manual entry is the one way in until the server exists. `HostStatus::NeedsPairing`, the
  pairing CLI (`slopty pair`, `slopty host ticket|paired|revoke`) and `trust.json` are deleted.

- ✅ **Admission by source address, once per connection** (2026-09-24). With no keys, the network is
  the boundary, so the worker lets in loopback always, and otherwise only the tailnet
  (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`), RFC 1918 LANs, unique-local IPv6 (`fc00::/7`) and
  link-local (`slopty_net::admission`). The `[worker] allow` list in the worker's `settings.toml`
  *replaces* those defaults (so it can narrow as well as widen; loopback stays), read when the
  worker starts; a range that does not parse is logged and skipped, and a list with nothing usable
  left falls back to the defaults rather than to everyone. The check runs in the accept loop on
  `Incoming::remote_address()` before any connection state exists; a refused peer gets QUIC's
  `CONNECTION_REFUSED` at once (so a misconfigured client hears why in milliseconds, not a timeout)
  and packets of an admitted connection are never looked at again. Each admitted connection's
  handshake and `Hello` now run on a task of their own, so a peer that connects and says nothing
  holds up nobody behind it. Tests: `slopty_net::admission` (ranges, mapped IPv4, the replacing
  list), `slopty-worker`'s `the_allow_list_comes_from_settings_and_a_bad_range_is_skipped`, and
  `a_peer_outside_the_admitted_ranges_is_refused_before_the_handshake`, which dials the worker from
  `fe80::1%lo0` (this Mac's own link-local address, not loopback) against a worker admitting only
  10/8 and reads the refusal in under a second.

- ✅ **Superseded 2026-09-24: one workspace for every worker, no switcher** (see
  `workspace.md`). The machines the app reaches are workers; one `WorkspaceView` shows all of
  them at once, a tile being `(WorkerKey, ItemId)` (the key is the `WorkerId`'s 128 bits), so
  the per-host canvas, the switcher, ⌘⌥←/→ between hosts, the Host menu and the camera resume
  are gone. Each worker keeps its own link
  and reconnect loop (`slopty_app::workers`); a dropped worker's tiles stay and say so, and the
  titlebar names only the workers that are down. Cross-host attention holds unchanged: the pill
  and the badge count every worker's agents and ⌘⇧A goes to the next one wherever it is.

- ✅ **Two uploads never share a partial file, and abandoned partials are swept** (2026-09-25).
  Two drops of one name into one directory at once both chose the directory, since neither
  name existed yet, and wrote the same `name.partial`. A name another transfer in flight put
  in a directory now counts as taken there, so the second goes to `<drop>/<xfer>/`
  (`audio.md`, "Uploads are written as `.partial`"). Partials were never removed either, and
  an upload cut for good left one in the shell's directory. Each top-level entry a transfer
  places is listed in `<drop>/.partials`, and the list loses a transfer's entries when its
  last file lands. At start the worker removes the `.partial` files under the listed entries
  that nothing wrote to for a day (`slopty_worker::xfer::STALE_PARTIAL`), then the drop
  directories that leaves empty. An entry with a younger partial stays listed for the next
  start. Directories an unfinished drop made in place stay. Resuming a transfer across a
  client reconnect is still the client's to do. Tests: `two_drops_of_one_name_never_share_a_partial`
  and `a_sweep_removes_the_stale_partials_of_unfinished_transfers` (`slopty_worker::xfer`).

- ✅ **The two-worker e2e runs on one Mac, behind a shaped link** (2026-09-26). The suite
  needed a second Mac over ssh: it copied the binaries there, ran the daemons under a temp root
  and played the hook over ssh, so it passed or failed on that machine being awake and on how
  fast the overlay moved 60 MB. The user ruled that nothing may depend on the second machine.
  Worker B is now a second ptyd + worker on this Mac (`harness::SecondWorker`), under a
  `StackDir` of its own with a private `HOME`, the fixed-prompt zsh and a pasteboard of its own.
  The app never dials it directly. It dials a `slopty-shape` relay in the test process, and
  that relay carries the link the 2026-09-25 mesh run measured (`harness::TAILNET`): 4 ms each
  way, up to 2 ms of jitter and 3 % independent loss. The ICMP loss of 13 to 21 % on that path
  is not a UDP figure, since the worker's QUIC counted no loss in 15 of 16 runs. 3 % still puts
  retransmissions into every step, where 13 % would make a lost handshake or close decide the
  run. The 20 % case is the worker e2e's lossy typing test. The worker takes a free port on its
  first start and binds it again on restart, and the relay outlives it, so the kill-mid-stream
  scenario redials the address the app added. The kill is a SIGKILL, a crash with no goodbye.
  The app shows the worker silent within its 8 s bound and redials after its 15 s drop bound.
  The hook goes through the real `slopty hook`, spawned by the test with the session, the
  worker's control socket and the payload on stdin. `cargo xtask e2e all` now includes it. Two
  runs in a row: RTT to B 14.8 / 14.6 ms through the shaper, against 0.7 ms to A on loopback;
  the pill badged 110 / 117 ms after the hook; B shown down 8.5 / 8.7 s after the kill and
  connected 8.2 / 8.3 s after the restart; the whole run took 21.6 / 20.7 s. The relay lost
  7 of about 150 packets up and 2 of 88 down, and the test fails if either direction loses
  nothing, which would mean the traffic did not go through the shaper. Supersedes the two
  "Two-host harness" hardening entries above, whose ssh teardown, gate and address helpers are
  gone.

- ✅ **A worker that dies shows down in seconds and one that comes back is found in one**
  (2026-09-26). The two-worker e2e measured 8.5 s from a SIGKILL to the worker shown down and
  8.2 s from its restart to the link back up, and a remote Mac cannot feel local at that. Each
  delay had its own cause, and each is fixed where it lives:
  - Every link, a worker's included, runs the 1 s keep-alive the server leases already used
    (`slopty_net::endpoint::KEEP_ALIVE`, one constant for both). The app's bars are multiples
    of it: silent at three missed keep-alives (`SILENCE_WARN`, 3 s), given up at five
    (`SILENCE_DROP`, 5 s). It samples the received-datagram count twice a keep-alive
    (`workers::Hearing`, `HEARING_TICK`). The transport's 45 s idle timeout stays, for the
    worker's side of a phone that sleeps.
  - A restarted worker now resets the old connection. The reset `transport.md` relied on never
    went out, because noq's connection-ID generator takes a random key per process, and a PING
    under the null crypto is shorter than the 22 bytes noq needs before it answers with a reset.
    Both are fixed in `slopty_net::crypto`: one connection-ID key for every process, and a
    16-byte header sample, so every packet is at least 29 bytes. While the app hears nothing it
    also pings on each tick (`WorkerLink::ping`), so a worker that comes back is found within
    half a second, not at noq's probe backoff of up to 2 s.
  - Redials follow one rule for the server link and every worker link
    (`slopty_client::redial`): 250 ms after a drop, doubling to 2 s, back to 250 ms after a
    link that held for 10 s. The worker links waited 1 s and backed off to 10 s. The server
    link's cap drops from 5 s to 2 s.
  - A dial gives up after 2 s instead of 5 (`HANDSHAKE_TIMEOUT`). noq's Initial probes back off
    to 2 s apart, so the end of a long dial left a worker that had just come up unasked, where
    the next dial asks it at once.
  - When the server's directory says a worker is back online (or moved), the wake that already
    cut the connect loop's wait short now also reaches a link that is still up. If that link
    is silent, it is the dead one: it is given up and redialled at once.

  After, three runs each (`docs/MEASUREMENTS.md`, "a dead worker shown down and a restarted one
  found again"): shown down 3.3 to 3.5 s after the kill. A restart while it is shown down
  connects 0.6 to 0.8 s later. The link is given up 5.6 to 5.7 s after the kill, and a restart
  3 s after that connects 0.5 to 0.6 s later. Tests: `redials_back_off_from_a_quarter_second_to_two`
  and `a_steady_link_starts_the_backoff_again_and_a_flapping_one_does_not`
  (`slopty_client::redial`), `the_silence_bars_are_three_and_five_keep_alives`, the two `Hearing`
  tests and
  `a_silent_link_pings_and_is_given_up_at_once_when_the_server_says_the_worker_is_back`
  (`slopty_app::workers`), and
  `a_restarted_worker_resets_the_old_connection_within_a_keep_alive` and
  `a_ping_draws_a_restarted_worker_s_reset_at_once` (`slopty-net` loopback, through an
  in-test front that swaps the worker behind one address). The e2e gained the scenario past
  the drop bar.

- ✅ **The app is tested the way it is used: through a server** (2026-09-26). Every e2e that
  ran the real app added its workers by address, and the server suite drove the server with
  the CLI and MCP only. `cargo xtask e2e through-server` (`tests/through_server.rs`,
  `harness::ServerFleet`) starts a `slopty-server`, worker `studio` on loopback and worker
  `remote` behind the tailnet-shaped relay, both registered with it, and then the app at its
  first run. The address is typed into the first-run panel and entered. Both workers then have
  to arrive from the directory, each with a shell that answers a command. A permission hook
  played on `remote` through `slopty hook` has to badge the pill. `remote` is then killed and
  restarted twice, and each time is measured against a bound.
  - **The directory has to lead through the relay.** The server lists a worker at the IP it
    registered from and the port it listens on, and the app dials exactly that. So `remote`
    listens on `127.0.0.1` only (`SLOPTY_BIND`) and registers over IPv6. The directory then
    says `[::1]:<its port>`, and the relay listens there. One socket cannot relay that: bound to
    `::1` it cannot send to `127.0.0.1`, and a dual-stack one would send from `127.0.0.1:<port>`,
    the worker's own address. `slopty_shape::relay::Relay::bind_apart` gives the relay a second
    socket to speak to the worker from (test:
    `a_relay_apart_takes_the_workers_port_on_the_other_family`). Nothing in the worker or the
    server changed for the test.
  - **What the kills cover.** A restart as soon as the app shows the worker down is found by
    the app's own link: its pings draw the new process's reset before the server has noticed
    anything. A restart after the server has called the worker unreachable, while the app holds
    it instead of redialling, is found because the server lists it online again, which wakes
    the dial. That second path is the one only this suite can see. The narrower rule, where a
    link that is still up but silent is given up when the server says the worker is back, does
    not happen in an unscripted run. The server holds a killed worker's lease for 5 s and turns
    the restarted one away until then, and by that time the app's ping has drawn the reset or
    its drop bar has given the link up. The unit test stays its proof.
  - **Bounds.** Shown down within 5 s of the kill, and connected within 2 s of either restart.
    Five runs (`docs/MEASUREMENTS.md`, "the app through a server") read 2.4 to 3.1 s down, 0.76
    to 0.88 s back by the app's own link, and 0.12 to 0.13 s back through the server. The bounds held
    without a change to the app.
  - **Golden.** `through-server` shows two workers from one server in one layout, with the
    pill counting the remote worker's waiting agent. No other golden has more than one worker.
    The second worker's private `HOME` is canonicalised, so its prompt reads `~` and not a
    `/private/var/…` path.
