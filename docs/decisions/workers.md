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
  while the output stands inside an escape sequence or a UTF-8 character (libghostty's own
  parser state, `GhosttyEngine::at_ground`, since 2026-09-30; a byte-level guess before),
  because it replaces the bytes before it and the rest
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
  - Amended 2026-09-30: **a working agent keeps the host awake with nobody attached, while it
    shows signs of work.** A person starts a long turn from the phone and pockets it, or closes
    the laptop that was the client. The host used to idle to sleep there, stalling the turn
    mid-tool with no one to notice. `Wake` now counts working agents beside clients, and holds
    the system (not the display) while either is above zero.
    - **What counts.** An agent counts while it is `Working`, in a `Tool`, or `Waiting` on
      background work (`tasks > 0`, Claude Code ruling **A turn that leaves work running is
      paused, not done**). It counts only while it showed a sign of work within
      `SILENT_CAP` = 15 minutes (`wake::Quiet`, sampled on the agents tick).
    - **Signs of work in a turn.** The session's output. Claude Code repaints its spinner and
      elapsed time every second during a turn, so 15 silent minutes mean the turn hung or its
      end was never heard: a Claude Code killed without a `Stop` stays `Working` until the
      process probe sees it gone.
    - **Signs of work in a paused turn.** Its prompt is quiet on screen, so it also counts the
      processor time of the agent's descendant processes, the commands it left running
      (`ports::descendants_cpu`: libproc `PROC_PIDTASKINFO` on macOS, `/proc/<pid>/stat` on
      Linux, summed over the live tree without the agent itself). A build computes; a dev
      server waiting for requests does not, and lets the host go after 15 minutes.
    - **A ceiling for paused turns.** `PAUSED_CEILING` = 2 hours from the pause, whatever the
      commands do, since a watcher that polls never ends.
    - Blocked and idle agents do not count: they wait on the person, and the person is not
      there. `slopty worker wake` (`CtlRequest::Wake` → `ctl::Awake`) prints what holds the
      host now.
    - Tests:
      - `wake::tests`: `a_working_agent_holds_the_machine_with_nobody_attached`,
        `the_cap_lets_a_silent_agent_go` and
        `a_paused_turn_counts_while_its_commands_compute_up_to_the_ceiling`.
      - `ports::tests::a_busy_descendant_moves_the_processor_time_and_a_sleeping_one_does_not`.
      - `slopty-workerd` `handoff`
        `a_working_agent_keeps_the_worker_awake_with_nobody_connected`. Hooks are sent to the
        control socket, and the test checks the real assertion in the power manager's table
        (`pmset -g assertions`: `PreventUserIdleSystemSleep` held by the worker's pid under
        its reason), not only the policy's flag.

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

- ✅ **Admission by source address, once per connection** (2026-09-24). Superseded in part
  2026-09-26 by **Tailscale is the network, and its LocalAPI says who is calling** (`topology.md`):
  the tailnet is checked through whois and grants, and the LAN ranges are no longer defaults. With no keys, the network is
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
    (`slopty_net::redial`): 250 ms after a drop, doubling to 2 s, back to 250 ms after a
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
  (`slopty_net::redial`), `the_silence_bars_are_three_and_five_keep_alives`, the two `Hearing`
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

- ✅ **A session's summary carries its branch and its start** (2026-09-26). The navigator's
  two-line rows, the palette and the status bar name a shell by branch and show its age, and
  only the worker can see either.
  - **Branch.** `slopty_worker::repo::branch_of` reads the repository's `HEAD` as a file: a
    `refs/heads/` name, any other ref as written below `refs/`, or the commit's first seven hex
    digits when detached. In a worktree or a submodule `.git` is a file, and its `gitdir:` line
    (relative to the root or absolute) leads to the `HEAD` that tree has. No `git` process runs.
    The actor resolves the repository and the branch together when the shell reports its
    directory (OSC 7, at every prompt with the integration) and when a command ends (`133;D`),
    so a `git switch` or a `git init` shows by the next prompt. It resolves
    them after the read's frame has gone out, never before it. There is no polling, so a
    checkout made from another shell shows at this shell's next prompt. Cost: see
    `docs/MEASUREMENTS.md`, "the branch at each prompt".
  - **A move is announced twice.** The viewers get `TermEvent::Cwd { path, repo, branch }`.
    The actor also sends its id on `SessionStart::moves`, and the daemon sends the fresh summary
    as `SessionOpened` to every client and to the server, as it already did for an exit or a
    resize. Before this, a summary kept the directory it had at open (usually none, since the
    shell had not reported one yet), so the server's listing never showed a session's
    directory. A prompt where nothing changed sends neither; the viewers used to get a `Cwd`
    at every prompt.
  - **Start.** `SessionSummary.started_ms` is wall-clock milliseconds since the Unix epoch,
    stamped by ptyd when it spawns the child and handed over in `PtydEvent::Attached`, so a
    worker restart keeps the session's age. It is not a duration: the server keeps summaries
    and hands them to clients that join later, so a duration would already be stale when it
    was read. The tailnet's machines run NTP, and an age shown in minutes does not notice a
    skew of milliseconds.
  - **Not carried: when the last command ended.** A summary goes out again only when a session
    moves, exits or is resized. Carrying the end would send one to every client and the server
    for every command, and an attached client already sees each `133;D` in the rows it gets,
    so it can time the end itself.
  - Wire: goldens `worker_session_opened`, `worker_term_cwd` and `worker_term_cwd_no_repo`
    re-accepted; ptyd's `Attached` gained `started_ms`. Tests: the
    `slopty_worker::repo` branch tests (checkout, worktree `gitdir:` relative and absolute,
    detached `HEAD` SHA-1 and SHA-256, no readable `HEAD`),
    `a_checkout_is_seen_at_the_next_prompt` in `slopty-worker/tests/session_actor.rs`,
    `the_server_hears_where_a_terminal_is_and_since_when` in
    `apps/slopty-worker/tests/server_link.rs` (a real ptyd and worker, a bash in a repository,
    a checkout typed through the verb), and the start surviving a reattach in `slopty-ptyd`'s
    `spawn_attach_detach_reattach_close`.

- ✅ **A followed session's transcripts are read once, whoever follows it** (2026-09-28). Each
  follow task used to own a `Transcripts` of its own and decode the session's files every
  250 ms and at every hook, so two clients following one agent decoded it twice. The follows
  board (`slopty_worker::conversation::Board::reader`) now keeps one `Reader` per followed
  session, held weakly: it lives while a follower holds it and goes with the last one. A task
  of the session's own, started by the first follower, reads on the tick and at each hook and
  broadcasts each read's `Read` (changes and outputs). A follower that joins takes the reader's
  async lock and reads what the files gained, which the others are sent. It then takes the
  conversation as it stands, rebuilt from the decoder (a reset of every thread, then entries,
  tasks, turns and the outputs' tails), and subscribes, all under the one lock, so no read
  falls between its snapshot and its first change. A follower more than 64 reads behind is sent
  the whole conversation again rather than a gap. Expanding a clipped text reads through the
  same reader. Each follower keeps its own live-block overlay and meters, which are per stream.
  Measured (MEASUREMENTS.md, "one transcript read for every follower"): with 4 followers an
  idle tick costs about 53 µs instead of 210, an appended record about 130 µs instead of 335.
  Test: `followers_share_one_read_and_a_late_one_gets_it_whole` counts the reads. The follow
  e2e (`a_followed_conversation_streams_and_holds_permission_for_the_follower`) runs on it.

- ✅ **The item registry is written by one writer, compact, as it stands** (2026-09-28). Every
  item change used to start its own blocking task that cloned the whole registry and wrote it
  as pretty JSON, so a burst of N changes made N full writes. Now a change marks the registry
  dirty and starts a writer only when none is at work. The writer writes the latest state,
  writes again only if something changed during its write, and then stands down. A burst is one
  or two writes of its final state. Without a runtime the write stays inline, so a tool or a
  test sees each change on disk when the call returns. The file is compact JSON: nobody edits
  it by hand. Test: `a_burst_of_changes_is_written_once_as_it_ended` holds a write under way
  across 50 changes and counts one write of the final state.

- ✅ **An item edit is applied by one function on both sides** (2026-09-28). The client's
  `ItemDoc::apply_op` and the worker's registry each applied an `ItemOp` to an `Item` in their
  own code, and they had drifted. On an op the item's kind does not take, the client changed
  nothing and the worker refused; a `SetUrl` or `SetFolder` to the value already held counted
  as a change on the worker and not on the client, while a same-value rename or sleep counted
  on both. Now `slopty_proto::items::Item::apply` makes every edit and says whether the value
  moved, or refuses an edit the kind does not take (`Refused::WrongKind`); adds and removes stay
  with each registry, whose rules differ (the worker refuses an add for an id it holds, and a
  client takes the echo of its own). The client maps "no change" and a refusal to
  `ItemChange::Echo`. So a client redraws only for what moved, and the echo of its own
  optimistic op is an echo unless the worker trimmed it, when the worker's version wins. The
  worker maps a refusal to `WorkerError::Items` ("not a note", as before) and still broadcasts
  an edit that changed nothing, since the delta is how the proposer hears its op was taken.
  - Tests: `an_edit_changes_only_what_moves_and_only_where_it_fits` in `slopty-proto`; the
    client's `snapshot_then_deltas_and_echoes` checks an echo that the worker trimmed and one
    it did not.

- ✅ **An idle stream waits on events; a crop and the real pointer still poll** (2026-09-28).
  A window stream's cursor loop sleeps until its input moves the placed pointer
  (`PointerWatch` is a `watch` now), or the probe reports new bounds, a change of visibility
  or a new zoom (`Shared::cursor_wake`). It also sleeps while the target is hidden. A display
  stream's pointer is the real one, which the worker's own user moves without any event, so
  that loop keeps its 120 Hz read of the move counters. The geometry probe keeps its 100 ms
  period while the stream has something to follow or took input in the last second, and
  otherwise waits for the accessibility API or a 1 s backstop (`Pipeline::geometry_quiet`).
  Input counts because the injector reads the window server itself, in front of the event,
  once the probe's bounds are older than `BOUNDS_TTL` (250 ms). The first input after a quiet
  spell probes at once. The accessibility watch now also hears
  `kAXWindowMovedNotification` and `kAXWindowResizedNotification` (`Went::Moved`). Following
  covers a window served as a crop, since another application's window moving over it
  announces nothing and the crop would show that window until a probe. It also covers a
  hidden target (nothing announces a return from another Space), a source that draws, and
  anything under way. The first frame after the client was told the source is idle wakes the
  probe, so `Live` is not a backstop late. Since a drag can now wake the probe on every step,
  a new size must hold for 100 ms rather than for two ticks (`ResizeDebounce`, `RESIZE_HOLD`).
  Numbers: MEASUREMENTS.md, "an idle stream's wakeups". Tests:
  `the_cursor_loop_sleeps_until_the_placed_pointer_moves`,
  `a_move_or_a_frame_after_idle_wakes_the_geometry_probe`,
  `a_resize_rebuilds_only_once_the_size_holds`, `only_a_move_wakes_a_reader` and
  `a_quiet_stream_is_probed_when_woken_not_every_period`.

- ✅ **The tailnet path is read, not watched** (2026-09-28). The code audit suggested
  replacing the 2 s read of `/localapi/v0/status` with the daemon's `watch-ipn-bus` stream.
  The bus cannot tell a client its path. Its `Notify` carries no peer's current address or
  relay: `Engine` is byte counts and live peers (`PeerStatusLite`), and peer patches carry
  control's endpoints, not the one magicsock chose. The engine updates are the daemon polling
  itself every 2 s for any watcher that asks for them (`pollRequestEngineStatus` in
  `ipn/ipnlocal/local.go`, tailscale v1.102.4, whose comment says so). A watch would still have
  to read the status to learn a path, on the same clock. So the read stays, only while a
  client on the tailnet listens. While none does, the reader sleeps on a `Notify` instead of
  ticking, and the first client to listen has the status read at once rather than up to a
  poll later. Test: `the_status_is_read_when_a_client_listens_and_only_then`.

- ✅ **A client's virtual display outlives its disconnect** (2026-09-29, product gap 1). The
  last stream letting go of a display made for a client removed it, and macOS then moved
  every window on it to a physical display and left them there. A phone roaming between
  networks or a lid closing did that each time. Now the worker keeps a display its client let
  go of for `sized::LINGER` (10 min), and an `OpenDisplay` with the same key within that time
  takes the same display back, windows in place, resized to the new shape. Each linger is
  numbered, so a timer outlived by a take-back and a later let-go releases nothing. A display
  the stream found unusable (it never settled, ScreenCaptureKit never listed it, a resize
  lost it) is released at once (`Lease::lost`): the client would only fail on it again.
  `Displays::new` keeps no linger. The worker's `on_main_queue` turns it on, so the
  stream-level tests in `apps/slopty-worker` still see a display go with its stream.
  - Not done yet: Jump ends a linger early on local keyboard or mouse input, and the
    research proposed the same (a `CGEventSource` counter). The counter counts the input
    Slopty itself posts for other clients too, so it needs a way to tell the two apart first.
    The linger is a constant until `[worker] display_linger` lands in `slopty-settings`.
  - Tests: `a_display_let_go_lingers_for_its_client_and_goes_when_the_linger_runs_out` (on
    paused time, with an outlived timer) and
    `a_lost_display_is_released_at_once_and_a_linger_is_its_own_keys`, both over the fake
    `Factory`; no real display is made.

- ✅ **A worker is deployed and updated over the system `ssh`** (2026-09-29, product gap 9).
  `slopty worker deploy <ssh target>` puts a worker on another machine, and `--update`
  replaces the one there.
  - **The system `ssh`, one connection per step.** `~/.ssh/config`, `ControlMaster` and
    Tailscale SSH apply as they do to a typed `ssh`. Every remote step is a `sh -c` script,
    so the login shell does not matter, and paths are relative to the home where `ssh`
    starts. No `scp` or `sftp` is needed: a file goes up as `cat > name.part` and is moved
    over its name once whole.
  - **The binaries are checked against the machine before anything moves.** `uname -sm`
    names the target, and the Mach-O (thin or universal) or ELF header of each of
    `slopty-ptyd`, `slopty-worker` and `slopty` must name the same OS and CPU. A mismatch
    asks for `--bin-dir` with a build for it. Linux is recognised; what is missing for it is a
    musl build to ship.
  - **The install runs on the target, in Rust.** The uploaded `slopty worker install
    --bin-dir ~/.slopty/deploy` installs the services with the same
    `slopty_platform::service::install_worker` a local install uses. A deploy passes
    `--fresh`, which refuses a machine that already has a worker. `--update` requires one,
    keeps its port and bind address, and first copies its binaries to
    `<data dir>/bin.previous`.
  - **Health means the new worker answers as itself.** Every install now waits for the
    control socket to answer both status and doctor. The doctor must show this build's
    version, the `slopty-worker` just installed, and an uptime no longer than the install
    has taken. A worker left over from before, or another build, fails at once rather than
    counting as up. If an update's new worker fails, the previous binaries are installed
    again and the command fails with "the previous one is back".
  - **Then the doctor, as JSON.** `slopty --json worker doctor` prints the `Health`. The
    deploy reports the version, the binary, which of Screen Recording and Accessibility a
    person at that Mac still has to allow, and `slopty add <tailnet name>`. Terminals and
    agents work before those grants.
  - Tests: `deploy::tests` run whole deploys through a fake `ssh` that plays the host in a
    temporary home. The uploads run there under `sh`, byte for byte, executable and without
    a leftover `.part`. The tests also cover `--fresh` versus `--update`, a failed remote
    install failing the deploy, a machine the binaries do not fit being refused after
    `uname` alone, and header reading. `service::tests` drive the remote side against a
    recording launchd and a fake control socket: `--fresh`, `--update`, the port carried
    over, and the previous worker put back when the new one answers as another version or
    was already up.
  - **Pending: a live deploy.** Remote Login is on here, but `localhost` fails its host key
    check in `~/.ssh/known_hosts`, which is the user's to fix. A deploy to this Mac would
    also replace its own worker: the launchd labels are fixed, and this Mac has the worker's
    agents installed. The live proof waits for a second machine or a label of its own for
    tests in `slopty-platform`.
- ✅ **A sleeping worker is woken from its own LAN** (2026-09-29, product gaps #3). A Mac asleep
  is off the tailnet, so nothing reaches it through Tailscale. What still reaches it is an
  Ethernet frame on its own segment: the magic packet, six `0xFF` bytes and then its MAC sixteen
  times, which wakes it when "Wake for network access" (`pmset womp`) is on.
  - **Each worker reports its LAN ports in `WorkerCaps`.** `lan` lists every interface that is
    up and broadcasting, has an Ethernet address and an IPv4 subnet, and is neither loopback
    nor a tunnel. Each entry is a `slopty_proto::lan::LanPort`: interface name, MAC, address
    and prefix. `wake_on_lan` carries `womp` from `pmset -g`, read once a minute, and is `None`
    on Linux, where the card's setting sits behind `ethtool`'s ioctl. The caps travel to the
    server at registration and on change, and on to every client in the directory and in
    `WorkerMsg::Caps`. A new DHCP lease is a caps change like any other.
  - **The MAC comes from the I/O Registry on macOS.** `getifaddrs(3)` gives the flags and the
    subnet. On macOS 27 its `AF_LINK` entries hold `02:00:00:00:00:00` for every interface when
    the process is not root, as iOS has long done: this Mac's `ifconfig` shows exactly that
    while `networksetup -getmacaddress en0` shows the real address. The real one is the
    `IOMACAddress` of the controller above each `IOEthernetInterface`, which is what `ioreg`
    reads. The first `SCNetworkInterfaceCopyAll` call took 3.5 to 9 s here, while the I/O
    Registry walk takes 0.3 to 0.4 ms, so the registry is read. An `AF_LINK` address that is
    not the redacted one wins, and Linux reads `AF_PACKET`'s `sockaddr_ll`. This is the one
    `unsafe` in `slopty-tailnet` (`lan`), with the rule for each call beside it.
  - **Verbs.** A client sends `Verb::Wake { worker }` to the server, which answers it itself.
    If the worker is online it answers `Invalid`. If the worker reported no port it answers
    `Unsupported`. Otherwise the server sends the packet itself when one of its own ports is
    on the sleeping worker's subnet: the server is the machine that is always on. Failing
    that, it sends `Verb::WakePeer { worker, peer }` down the link of each online worker on
    that subnet, least loaded first, until one answers `Done`. The answer is
    `Outcome::WakeSent { by, to }`, naming the machine that sent it and the sleeping
    interfaces. When nothing online shares the subnet, the answer is `Failed` and lists the
    subnets. Neither verb `changes()`: a second packet wakes nothing the first did not.
  - **The packet goes as Jump sends it.** It goes 5 times, 100 ms apart, to UDP 9, as a
    directed broadcast on the sender's subnet from a socket bound to the sender's address on
    it, so it leaves by that interface. It is sent once per sleeping port the sender reaches,
    so a Mac on both Ethernet and Wi-Fi gets both. Tailscale's own PeerAPI `/v0/wol` was not
    used: it is undocumented, sits behind a peer capability, and needs a Tailscale node on
    the LAN anyway.
  - **Surfaces.** `slopty wake <worker>` prints who sent it and warns when the worker said
    `womp` is off. For the app, `slopty_client::server::ServerTask::caller()` returns a
    `ServerCaller`, whose `wake(worker)` sends the verb up the client's one server link.
    That link now also carries `call(verb)`. `Directory::can_wake(worker)` says when to offer
    the palette's "Wake <worker>": the server is linked, the worker is not online, it has a
    LAN port, and it did not say `womp` is off. A worker whose `womp` is off logs a warning.
  - Tests:
    - `slopty-proto`: `lan::tests` pin the packet's bytes, the subnet arithmetic and MAC
      parsing.
    - `slopty-tailnet`: `lan::tests` read a `sockaddr_dl` and a `sockaddr_ll`, choose ports
      from synthetic entries and replace a redacted address. They list this Mac's own ports
      and plan which port sends for which. `each_packet_goes_five_times_a_tenth_of_a_second_apart`
      sends over loopback to the test's own socket, so nothing reaches the LAN.
    - `slopty-server`: `hub::tests` cover the wake relayed to the worker on the sleeping
      worker's subnet and not to one elsewhere, the server sending itself through a fake LAN,
      and the four refusals.
    - `slopty-worker`: `womp` parsing, plus this Mac's own caps and `womp`.
    - `slopty-client`: `can_wake`.
    - Goldens: `server_caps_lan`, `server_client_wake`, `server_request_wake_peer` and
      `server_reply_wake_sent`. `worker_hello_ack`, `server_worker_hello`,
      `server_directory` and the ctl doctor golden moved with the two new caps fields.
  - **Pending.** The palette command in `slopty-ui`, an MCP tool in `slopty-tools`, and
    `womp` in `slopty worker doctor`'s `Health` (`ctl.rs`). No live wake was sent: a test
    never puts a magic packet on the LAN, and waking a Mac takes a second one asleep.

- ✅ **Waking a worker from the app, an agent and the doctor; clipboard reads that would ask
  wait** (2026-09-29, finishing the pending half of the wake above and the pasteboard alert in
  `platform.md`).
  - **The app.** A worker whose `Directory::can_wake` holds gets "Wake <worker>" in the
    palette's commands and a Wake in its row of the status bar's hosts popover. Both run the
    app's `wake_worker`, which sends `ServerCaller::wake` on the runtime and shows a notice:
    "<by> sent <worker> a wake; it shows online once it is up", or "Could not wake <worker>:
    <why>". The offer follows the directory, since the hosts' actions are rebuilt on every
    server message.
  - **Agents.** The MCP tool `wake_worker` (31 tools now) takes a worker's name or id and
    answers `by`, `to` and `wake_on_lan_off`, as `slopty wake --json` does
    (`slopty_tools::ops::wake`).
  - **The doctor.** `Health` gained `pasteboard`
    (`slopty_proto::ctl::PasteboardAccess`: allowed, not asked yet, asks, denied). `slopty
    worker doctor` prints "✔ Clipboard reads" or the `Access::problem` text with what the
    worker does meanwhile. It prints "Wake for network access" from `caps.wake_on_lan` when the
    worker could read it, with the `pmset` fix when it is off.
  - **The worker reads nothing while reads would ask.** `Clipboard::poll` asks the board's
    `clip::Access` first and returns before reading any contents unless reads are free. The
    general pasteboard answers with `pasteboard_access::general()`. A named pasteboard (the
    tests', `--pasteboard`) always allows, as `NSPasteboard.h` says, so the e2e runs are
    unaffected by this Mac's setting. Writing a client's paste never asks, so it still
    lands. Once reads are allowed, the next change is announced as usual.
  - Tests: worker `clip::tests::nothing_is_read_while_reads_would_ask_the_person`; tools
    `a_worker_is_woken_by_name`; app `server::tests::a_wake_says_who_sent_it_or_why_none_went`;
    workspace `bars::a_sleeping_worker_is_woken_from_the_palette_and_its_hosts_row`; CLI
    `doctor_report_names_the_binary_and_flags_missing_permissions`; goldens
    `ctl_health_pasteboard_asks_and_no_wake` and `ctl_pasteboard_access`, with the ctl
    doctor goldens moved by the new field.

- ✅ **The app installs and updates workers over SSH** (2026-09-29). Before this, only the CLI
  could deploy, the first run offered an address, the tailnet or this Mac, and a worker on
  another build could only have its command copied.
  - **One deploy, two front ends.** The plan moved out of `apps/slopty-cli/src/deploy.rs` into
    the `slopty-deploy` crate. `deploy(runner, plan, on)` runs the same steps and reports each
    as an `Event`: a step begun, the machine's platform once `uname` answered, bytes sent of all
    the binaries, and each line the install printed. Every remote script goes through a
    `Runner`. `Ssh` is the system `ssh`, one connection per step. The CLI's runner leaves the
    install's output on the terminal and prints each upload as before, so its output did not
    change. The app's runner (`Ssh::unattended`) passes `-o BatchMode=yes -o
    ConnectTimeout=15` so that `ssh` never waits on a prompt nobody can see, adds `-l`/`-p` when
    the person set them, and gets the install's output back as lines. A file goes up through
    the child's stdin 256 KiB at a time, which is what moves the bar.
  - **Errors are typed and keep the CLI's words.** `DeployError`'s `Display` and error chain
    read as the old `anyhow` messages did. `DeployError::failure()` is how a window says it: a
    title, what to do when that is known, and the last lines printed (`TAIL`, 8). When `ssh`
    itself fails (exit 255), its message names the cause: a host key not yet trusted (connect
    once in a terminal), a key refused, a name that does not resolve, Remote Login off, or a
    host that does not answer. An install that fails keeps its last lines; after `--update`
    those lines include the note that the previous worker is back.
  - **The sheet.** The add panel lists "Install on a machine over SSH" beside "Use this Mac as
    a worker", in one framed section headed "Set up a worker". The palette offers the same
    (`app.install_over_ssh`, no default chord). The first run is that panel, so it offers SSH
    too. The form asks for a host (`user@host` fills the user), then an optional user and port,
    and says that it uses the person's ssh config and agent. While it runs, the form gives way
    to five step lines: connect, copy, install, check, add. Under them is a bar that fills while
    the binaries go up and sweeps otherwise; under Reduce Motion it stands still, set back.
    Cancel drops the GPUI task. That drops the deploy, whose runtime task is aborted, and
    `kill_on_drop` ends its `ssh`. A failure brings the form back with the failure above
    "Try again". On success the worker is added at its tailnet name, then its tailnet IP, then
    the host `ssh` reached, with the port when it is not 45550, and the panel closes onto it.
    A Mac still missing Screen Recording or Accessibility is added anyway, and the notice says
    its screen waits for someone there.
  - **Update on the tile.** A worker's status carries the host it was dialled at, and "Update"
    deploys to that host with `--update`. The pill beside it takes over the step's words and
    detail, with the bar along its foot. "Copy command" stays beside Update in the quieter tone
    and goes while a run is under way. Once the deploy ends the worker's link loop is woken, so
    it dials at once rather than after `redial::WRONG_BUILD`. The link coming up ends the run.
    A dial that meets another build again fails the run ("It still runs a different build") and
    brings back the button as "Try again". The tiles learn of the app's update function and of
    each run through a GPUI global (`slopty_ui::add_worker::Updates`), because `WorkspaceView`'s
    worker state belongs to another part of the workspace. The app republishes the global and
    notifies the view on every change.
  - **Where it is offered.** Only on the Mac (`ssh::OFFERED`). The self-test gets no deployer,
    so neither the entry nor Update appears there, and the goldens are unchanged.
  - Tests: `slopty-deploy` `tests`: the fake-`ssh` deploys moved from the CLI (uploads byte for
    byte, `--fresh`/`--update`, a mismatch refused after `uname`, a dashed target refused before
    anything runs); a watched install's two streams returned as lines; and a scripted `Runner`
    for the order of the steps, the byte count across the uploads, the 255 cases, an upload's
    failure and the tail kept from a chatty install. CLI
    `deploy::tests::the_options_reach_the_plan_and_the_report_says_what_is_next`. App
    `ssh::tests` use a stand-in `Deployer` the test drives. They cover reading the fields, the
    candidate addresses, the steps from events, the sheet from the first-run entry to an added
    worker, Cancel dropping the deploy, a failure's words and output with Try again, and a
    tile's Update deploying with `--update`, redialling at once, failing on a second wrong
    build, and ending when the worker links. `slopty-ui` `add_worker::tests` cover the current
    step and the button words.
  - **Pending.** No deploy to a real machine was run from the app: the tests never reach a
    host. The empty workspace's line ("Add a worker from the command palette", `strip.rs`)
    could become a button that opens the panel.
