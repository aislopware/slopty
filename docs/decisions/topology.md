# Decisions — Topology: one server, many workers, many clients

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Three roles; the server is never on the data path** (2026-09-24, at the user's request
  for one app that reaches many machines and lets people and AI agents orchestrate them).
  - **Worker.** A machine that runs things: `slopty-worker` + `slopty-ptyd`, the host daemon
    of the time under its new name. It owns PTYs, scrollback, windows, displays and agent
    status.
  - **Server.** The control plane (`slopty-server`). It holds the worker registry with
    capabilities, cross-worker state (notes, orchestration history, per-device layouts kept for
    restore) and the orchestration API.
  - **Client.** The macOS, iPhone and iPad app, plus the `slopty` CLI.
  - **Data path.** Terminal rows and video go client ↔ worker directly over plaintext QUIC on
    the tailnet.
  - **Why no relay.** Every low-latency system surveyed keeps its coordinator out of the byte
    path. Coder's coderd only brokers, and users connect straight to workspaces. Tailscale's
    coordinator only matches peers and relays through DERP as a last resort; Tailscale ships
    peer relays because DERP adds latency. Parsec streams peer to peer. Zed removed the mode
    that sent remote-development traffic through its servers. A relay would add
    RTT(c→s) + RTT(s→w) − RTT(c→w), a second loss-recovery and congestion loop in series, and
    every client's video on one NIC. Tailscale already covers NAT failure, so Slopty adds no
    relay of its own.
  - Rejected: an always-relayed server in the style of VS Code tunnels or Teleport's reverse
    tunnel. Both buy reachability and audit at the cost of latency, and on a tailnet every node
    already reaches every other.

- ✅ **The worker dials the server; one connection is the lease and the command channel**
  (2026-09-24). This is the pattern of Nomad clients, Buildkite agents, Teleport agents, Coder
  agents and GitHub runners. The server keeps no list of addresses to dial, and a restarted
  worker simply reconnects.
  - **Command channel.** Server → worker commands (`open_terminal`, …) travel down the same
    QUIC control stream, so there is no polling.
  - **Liveness.** QUIC keep-alive 1 s with a 5 s idle timeout marks a worker *unreachable*.
    After 20 s (Nomad's TTL + grace defaults), it becomes *gone*. Its items stay visible as
    stale and are never deleted by the server; the worker is their authority.
  - **Identity.** A `WorkerId` UUID is minted once in the worker's data directory and survives
    restarts and address changes.
  - **Capabilities.** Sent at registration, then as deltas: OS and arch, CPUs, memory, hardware
    encoders, displays (id, size, scale, refresh), installed agents with versions, repository
    roots, protocol version, and health (load, capture permission granted).

- ✅ **State lives with its authority, and clients degrade to direct** (2026-09-24).
  - **Worker state.** A worker's sessions, windows and agent status are the worker's, and they
    survive a server restart.
  - **Server state.** The server persists only cross-worker state in one local file and rebuilds
    its live view from re-registrations.
  - **Degraded mode.** Clients cache the worker directory (id, address, name, capabilities).
    When the server is down they connect to workers directly: terminals and video keep working,
    and only layout restore, notes and cross-worker orchestration pause.

- ✅ **One verb set drives workers, exposed three ways** (2026-09-24). The verbs live in
  `slopty-proto::orchestration`, and item ids are explicit handles:
  - `list_workers`, `list_items`
  - `open_terminal(worker, cwd, cmd?, env?)`, `send_input(item, text|keys)`
  - `read_screen(item)`, `read_output(item, since_line)`, `list_commands(item, since)`
  - `wait_for(item, pattern | idle | exit | agent_state, timeout)`
  - `spawn_agent(worker, repo, prompt)`, `agent_status(item)`, `close(item)`
  - `read_file` / `write_file`, `list_ports`

  **Reads come from the worker's libghostty grid, never raw PTY bytes.** There are three
  views: the rendered screen, scrollback by absolute line index (the same numbering the row
  diffs use), and OSC 133 command blocks with exit codes.
  - **Why not screen-scraping.** Surveyed orchestrators show how it fails. claude-squad detects
    a waiting agent by matching a literal prompt string. OpenHands needed recovery for PS1
    sentinels corrupted by concurrent output, and a 30 s no-change timeout so tools do not hang.
  - **Agent status** stays hook-derived (`docs/decisions/claude-code.md`).

  **The three surfaces:**
  1. **MCP** Streamable HTTP on the server, rmcp 3.4.1, protocol revision 2026-07-28
     (stateless, no `initialize`, server-minted handles as plain arguments).
  2. **The `slopty` CLI**: the same verbs, with JSON output.
  3. **`slopty mcp`**: a stdio shim that can run as a Claude Code channel and push "agent on
     worker X is waiting" into the orchestrating session.

  `wait_for` reports progress and caps its wait under Claude Code's 5-minute HTTP idle abort.
  The MCP Tasks extension is not relied on, because Claude Code does not document consuming it.

- ✅ **Admission without crypto** (2026-09-24). Every listener (worker and server) accepts a
  connection only from loopback, Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48) or private LAN
  ranges, checked once per connection.
  - ⏸ Tailscale roles are a later refinement: the server can read the peer's node and custom
    app-capability grants (`slopty.dev/cap/role: worker|client|agent`) through LocalAPI WhoIs.
    This is deferred because it does not carry over to a plain VPN.

- ✅ **Workers go cross-platform behind traits in the worker crate; wire types are already
  neutral** (2026-09-24). Keys travel as W3C `KeyboardEvent.code`, display ids are opaque
  `u32`s, and pixel formats are named, not CoreVideo constants. Linux arrives later (macOS only
  now). The seams:
  - **PTY: no trait.** Both targets are Unix, so it is one rustix `openpty` path with small
    `cfg`s.
  - `CaptureSource`
    - macOS: ScreenCaptureKit.
    - Linux: the xdg-desktop-portal ScreenCast v6 through ashpd + pipewire, with a
      `restore_token` after the first consent. wlr-screencopy or KMS for unattended capture.
    - Frames are GPU surfaces: IOSurface or DMA-BUF.
  - `Encoder`
    - macOS: VideoToolbox.
    - Linux: VAAPI or NVENC through FFmpeg, a deliberate C exception like libghostty.
  - `InputSink`
    - macOS: CGEvent.
    - Linux: libei through the RemoteDesktop portal (reis), or uinput (evdev) headless.
  - `Clipboard`
    - macOS: NSPasteboard.
    - Linux: wl-clipboard-rs or arboard.

  These four were cut after the rename (2026-09-25), with macOS still the only
  implementation. No trait is written ahead of its second implementation beyond these four
  seams. What was cut:
  - **Where the traits live.** Each is in its platform crate, in a module that compiles on
    every target, beside the plain types that cross it. The macOS implementations sit behind
    `cfg(target_os = "macos")` in the same crates, and the macOS-only dependencies are
    target-gated. Clippy passes on the three crates for `x86_64-unknown-linux-musl`.
    - `slopty_capture::CaptureSource` covers the displays and windows (enumerate, resolve, the
      display crop), frames and audio (start, update, retarget, stop, the frame clock), the
      window list (bounds, owner, on screen, occlusion, the display under a rectangle), the
      accessibility resize and hide watch, and the pointer (move counter, location, cursor
      picture). `ScreenCaptureKit` implements it.
    - `slopty_codec::VideoEncoder` and `AudioEncoder`, implemented by `VideoToolbox` and `Opus`.
    - `slopty_input::InputSink`, implemented by `CgEvents`.
    - `slopty_input::pasteboard::Board`, implemented by `MacBoard`, which already existed.
  - **How the core uses them.** `slopty_worker::platform::Platform` names one implementation of
    each seam, and `MacOs` is the one there is. The screen pipeline is
    `slopty_worker::screen::Pipeline<P: Platform>`. `ScreenStream` is `Pipeline<Native>`, where
    `Native` is the platform the build serves, so the daemon and the tests did not change.
  - **Dispatch.** A capture platform is a type with no values whose operations are associated
    functions, and the stream is generic over it, so every call on the frame path is direct.
    The one `dyn` is the stream registry's handle on each stream's counters, read by the
    control socket. The enumeration cache holds its content as `Any` and downcasts it on a hit,
    once per enumeration.
  - **Names.** The implementations are newtypes (`VideoToolbox`, `Opus`, `CgEvents`) or a
    type with no values (`ScreenCaptureKit`) rather than trait impls on `Encoder` or `Injector`
    themselves. `same_name_method` forbids a trait method and an inherent one of one name on
    the same type, and renaming either side would have changed the tests.
  - **Left macOS-only in the core.** Capabilities (`caps.rs`: sysctl, `Os::MacOs`) and the
    listening ports (`ports.rs`: libproc) are not seams. A Linux worker adds its platform, its
    `Native`, and those two probes.

- ✅ **The CLI and `slopty mcp` name a terminal `worker/session` and resolve names on the
  client** (2026-09-25).
  - **Handles.** A terminal prints as `worker/session` with both UUIDs in full (the `term`
    field in JSON), and every verb takes it back. The worker part may also be a name and the
    session part a unique id prefix; full ids cost no round trip, anything else one
    `ListWorkers` or `ListTerminals`. Text output shortens the handle to `name/prefix`, where the
    prefix is the shortest one no other listed session shares, at least 8 characters: `UUIDv7`s
    open with their creation time, so ids minted a minute apart share their first 8.
  - **One JSON shape per answer.** `slopty … --json` and the `slopty mcp` tools print the same
    views (`apps/slopty-cli/src/view.rs`) with snake_case keys, not the wire enums' serde form.
    `slopty workers` adds each worker's terminal count and the agents waiting on a human, from
    `ListWorkers` and `ListTerminals`, both in flight at once on the one stream. Each listed
    terminal carries its agent (protocol 53); before that, one `AgentStatus` per terminal on an
    online worker followed.
  - **`slopty mcp`** holds one `Role::Agent` link for its lifetime and redials when it drops
    (250 ms doubling to 5 s; a call made while it is down dials at once and fails with the
    reason). It speaks revision 2026-07-28, and still negotiates older revisions for a client
    that sends `initialize`. An agent that comes to need a human goes out as a
    `notifications/message`, once per episode. Logging is deprecated by SEP-2577, but it is
    the one message a stdio client receives without a `subscriptions/listen` stream, and that
    stream's filters carry no such category. Claude Code channels are not used: they are a
    research preview on an older revision. `wait_for` sends progress every 10 s when the call
    carries a progress token.
  - The CLI's direct-to-worker `slopty open` (open and attach a raw terminal) became
    `slopty attach` without a session: `open` is now the verb. The local `slopty workers`
    listing of added workers is gone; the server's directory replaces it. Either form exits
    with the program's status once it exits, as `ssh` does, so a script can branch on
    `slopty attach -- make test` (2026-09-25). The session stays, exited, until it is closed.

- ✅ **The server's lease timings, state file and MCP endpoint** (2026-09-25).
  - **Lease.** Every server link runs a 1 s QUIC keep-alive and a 5 s idle timeout
    (`KEEP_ALIVE` and `LEASE_IDLE_TIMEOUT` in `slopty-net`). QUIC negotiates the idle
    timeout down to the smaller side's, so client and agent links get it too. That costs
    nothing, because the server is off the data path and a dropped client just redials.
    - A worker whose link ends turns *unreachable*. A clean close does it at once, a silent
      path after 5 s. Requests pending on the link fail with `WorkerUnreachable` right away.
    - 20 s later with no reconnect it turns *gone*.
    - A reconnect under the same id brings it back *online* in the same entry.
    - The server refuses a second live link that claims the id (`DuplicateWorker`) instead of
      letting it take over. Two machines sharing a copied data directory would otherwise steal
      the lease back and forth forever. A worker restarted before its old link timed out gets
      in on a retry within 5 s.
  - **State.** The server keeps one file, `workers.json`, in `$SLOPTY_DATA_DIR/server`, else in
    `~/Library/Application Support/Slopty/server`; `--data-dir` overrides both. It holds the
    last-known `WorkerInfo` of every worker. The server writes it to a temporary file, fsyncs
    and renames it over the old one, and only when the registry changes shape. A shape change
    is a new worker, a new name or address, capabilities that changed in more than their load,
    or a lease that ended. A restarted server lists every known worker as *gone* until it
    registers again.
  - **A worker set up again replaces its old entry** (2026-09-25). A new data directory gives
    a worker a new id under its old name, and the server used to keep the old id forever. A
    name then matched two workers, and clients kept dialling the dead one. On registration
    the server drops every entry without a live link that has the same name and the same
    address. Nothing else can be listening on that address and port, so the old worker is not
    running any more. Every link then gets the whole `Directory` again, which clients already
    read as a replacement, and the state file is rewritten. The tools' resolver also takes the
    one online worker when a name matches several. An entry at another address stays until
    `ForgetWorker` removes it (protocol 56, below). Tests: `a_worker_set_up_again_replaces_its_old_entry`
    (`slopty-server` hub) and `a_shared_name_means_the_one_online` (`slopty-tools`).
  - **MCP.** Streamable HTTP on TCP 45561 (`MCP_PORT`, next to `SERVER_PORT`), path `/mcp`,
    stateless, with JSON responses.
    - The listener binds `[::]` dual-stack and admits each TCP peer with the QUIC listener's
      check (loopback, the tailnet, private LANs). It answers the same peers that binding
      127.0.0.1 plus the tailnet and LAN addresses would, and it also reaches interfaces that
      come up after the server does, such as Tailscale starting late at boot.
    - It does not check `Host`, since tailnet names and IP literals are as legitimate as
      `localhost`. It refuses any request that carries `Origin` with a 403 instead. A browser
      always sends that header, and a DNS-rebinding page cannot leave it out.
  - **Timeout cap.** The server caps `wait_for` at 240 s, under Claude Code's five-minute idle
    abort of an MCP call. It gives a forwarded wait its own timeout plus 15 s before it stops
    waiting on the worker, and every other forwarded verb 60 s.

- ✅ **The worker's side of the link: registration, redial, and what the verbs mean** (2026-09-25).
  - **Joining.** `slopty-worker` registers with the server named by `--server`, else
    `SLOPTY_SERVER`, else `[worker] server` in `settings.toml` (port 45560 when none is given).
    With none it runs on its own as before. The link is one task beside the rest of the
    daemon. It hears the daemon's events on its own broadcast subscription and forwards
    `SessionOpened`, `SessionClosed` and `Agent`. If it falls behind, it registers again,
    because a fresh registration carries the whole state. Every forwarded verb runs in a task
    of its own, so a long `WaitFor` holds up nothing. A dead or absent server costs the
    terminals and the clients nothing.
  - **Redial.** After a link ends the worker dials again by the rule every link follows
    (`slopty_net::redial`, since 2026-09-26): 250 ms, doubling to 2 s, forever, and a link
    that held 10 s starts again from 250 ms. A `DuplicateWorker` refusal (the server still
    holds the link a restart dropped) is one more failed dial. Until then the worker had
    constants of its own (a 5 s cap and a 1 s duplicate retry), and a duplicate retry doubled
    the backoff whenever the backoff happened to equal it. The lease's keep-alive is the
    server's, so the worker adds no pings of its own.
  - **Capabilities.** OS, CPUs, memory and encoders are fixed. The agents come from one
    `claude --version` at start, through the login shell when the daemon's `PATH` lacks it.
    Permissions and displays are looked at every 5 s, since TCC and display changes send a
    daemon no notice. The load is compared every 30 s and sent only when it moved by more
    than 0.5.
  - **`SendInput`.** `Text` is what a keyboard's input method commits, so it goes as raw bytes,
    and each newline is an Enter press through the key encoder. `Paste` goes through the paste
    encoder and is bracketed when the program asked for it. `Keys` are `[mods+]key` names,
    turned into the key events a client sends so the program's keyboard modes apply. Every
    name is checked before any is sent, and a bad one is `Invalid` with the forms that parse.
  - **Waits read the way `expect` does.** The first design matched only output written after
    the `WaitFor` arrived. In a test with both calls in one process, `echo hi` had printed
    before the wait began often enough to fail, and over the server hop it would be the usual
    case. So each session keeps a mark. It starts where orchestration first touched the
    session: the first byte of a terminal a verb opened, else where the cursor stood before
    the first input or wait. `Output` scans from the mark and moves it past the matching line.
    `CommandDone` is met by the first command whose `133;D` came at or after the mark, and it
    consumes that command. A command that ended before its wait arrived therefore still counts,
    and the same line or command never ends two waits. `Quiet` restarts on any output. `Exit`
    holds once the program is gone. `AgentNeedsInput` holds at once for an agent blocked on a
    human, and otherwise at its next report of blocked, idle or done. An agent already idle
    when the wait starts has usually just been typed to, so its idle status is stale. Every
    wait sleeps on the session's activity channel and the agent events, never on a timer that
    asks again.
  - **Read views.** Each view is a message to the session's thread, answered from
    libghostty's grid with the plain-text formatter search uses. When search already
    formatted the history at this generation, the view slices that text. Otherwise it
    formats only the rows asked for, so a read near the end of a 50 000-line history costs
    those rows, not the 10 ms the whole history takes (MEASUREMENTS, 2026-09-24). A wait
    formats each row about once: it reads from the cursor's row of its last read, at most
    4096 rows at a time. `ReadOutput` stops at the last row with text when it reaches the
    end, and returns 10 000 lines at most. The command blocks keep their marks in stream
    order, so a command with no output or two prompts on adjacent rows stay apart, and the
    marks survive a full-screen program.
  - `ReadFile` returns 8 MiB at most, half a frame, and reads a range (protocol 56, below). `WriteFile` writes a
    temporary file beside the target, fsyncs it and renames it over, keeping the old mode.
  - A message too large for one frame is never written; an error goes in its place and the
    link carries on (2026-09-25). The frame holds more than the file's bytes, so a file just
    under 16 MiB passed the check above and its answer broke the worker's writer while the
    lease stayed up, and every later verb timed out. Now the worker's writer answers such a
    reply with `Failed`, the server's writer to a worker answers such a request (a
    `write_file` of a frame's worth) with `Invalid`, and its writer to a client does the same
    for a reply. Any other write failure ends the link: on the worker it ends the session and
    it redials, and on the server it ends the lease. The MCP endpoint's request body limit is
    a frame's worth of bytes in base64 plus 1 MiB (`slopty_server::mcp::MAX_BODY_BYTES`), up
    from rmcp's 4 MiB, so HTTP takes the same files as `slopty mcp` over stdio. Tests:
    `an_answer_too_large_for_the_link_is_an_error_and_the_link_goes_on` (`slopty-workerd`
    `server_link`) and `a_file_too_large_for_the_link_is_refused_and_the_link_stays_up`
    (`slopty-server` `mcp`).
    `ListPorts` walks each terminal's process tree with libproc and reports the TCP sockets
    in the listening state.

- ✅ **One tool contract, in `slopty-tools`, for every surface** (2026-09-25). This supersedes
  the state in which the server's MCP endpoint and `slopty mcp` each kept their own tools: the
  server took UUID `worker` and `session` arguments, an `until` enum with a `pattern`, and printed
  the wire enums' serde form; the CLI took `term` strings and printed its own views.
  - **The crate.** `crates/slopty-tools` holds the resolver (names, id prefixes,
    `worker/session`), the ops, the JSON and text views, and the MCP tool list with one
    `tools::call`. It dials nothing: a surface hands it a `Dispatch`, one async
    `call(Verb) -> Outcome`. The server's `Hub` answers in-process; the CLI's `Link` sends the
    verb down its QUIC stream and turns a lost connection into a `Failed` outcome.
  - **The shape is the CLI's.** Tools take `term` strings, worker names or prefixes, and
    `wait_for`'s flat condition fields, and answer with the snake_case views. A tool failure,
    a name that does not resolve included, is an `isError` result reading
    `message (Code)`; an unknown tool is invalid params.
  - **Files.** `read_file` answers `content` with `encoding` `utf8` when the bytes are UTF-8
    and `base64` otherwise, plus the file's `size`. `write_file` takes the same two fields,
    `utf8` by default. `slopty cat --json` prints the same view.
  - **What stays on each side.** The 240 s wait cap lives in the hub; the tools only pass the
    timeout through. `wait_for` reports progress through an optional sink: `slopty mcp` turns
    it into its 10 s progress notifications, and the server's stateless HTTP endpoint gives
    none. The stdio shim keeps the redial and the needs-a-human notifications.
  - A test sends the same calls to the server's endpoint and to `slopty mcp` dialled to that
    server, over one fake worker, and requires identical results.

- ✅ **Superseded 2026-09-25: a terminal ends with its program** (see terminal.md, "An exited
  shell stays until it is closed": the exit is now announced as the session's summary in the
  `Exited` state, and the session stays until a client or the unwatched bound closes it; the
  exactly-once path below still holds for every close). When a session's program exits, the
  worker shows the viewers the exit status, then closes the session and announces
  `SessionClosed { reason: Exited }` to its clients and the server. Before, only a verb or a
  client's close announced an end, so a shell that ran `exit` stayed listed on the server
  until the worker registered again. The app already closed a terminal on seeing its exit, so
  the retained session served nobody.
  - **Exactly once.** Every end goes through one path that takes the session out of the
    table, and only the call that removes it announces it. A client's `Close` for a session
    already gone is no longer an error, because that is the usual race: its program exited
    and the client closed the tile on seeing it.
  - Sessions that ended while the worker was down (ptyd reports them exited on adoption) are
    closed the same way at start, before the server hears of them.
  - An orchestrator that wants a program's last output reads it before the program exits: run
    it in a shell and wait with `command_done`, not `exit`. `WaitFor { Exit }` still resolves
    when the session is closed under it.

- ✅ **The app takes its workers from the server's directory and dials each one itself**
  (2026-09-25).
  - **Setting.** `[client] server` in `settings.toml` names the server, `host[:port]` with
    port 45560 when none is given; it is typed (`slopty_settings::ClientSettings`, a
    `HostAddr`), and empty means no server. The panel's "Connect to a server" dials the address
    once as a client, and only an address that answered is written into the file, with the
    rest of the file and its comments left as they were. "Disconnect from the server" clears
    it. A file that fails to parse keeps the server in use rather than dropping it.
  - **Link.** One `Role::Client` link (`slopty_client::server`), redialled 250 ms doubling to
    5 s, reset by a link that lived 10 s, the same rule as the worker's. `Directory`, `Worker`
    and `Event` feed a pure model (`slopty_client::directory::Directory`) that reports each
    liveness move once and says, per worker, whether to dial it and where.
  - **Data path.** The app dials every listed worker directly at its `address`, with the
    existing per-worker connection. While the server answers, a worker it calls unreachable
    or gone is not redialled on the fast backoff: its tiles show stale ("studio is
    unreachable") until it is back online, which wakes the dial at once. The server's view can
    be wrong, so such a worker is still tried every 10 s.
  - **A live direct link outranks the server.** A worker the server calls away keeps a direct
    link that still works. The link is the data path's own judge, and a restarted server lists
    every worker as gone until each registers again, so tearing links down on its word would
    blink every tile after each server restart.
  - **Degraded mode.** The directory is cached as `directory.json` in the app's data
    directory, tagged with its server and written off the main thread. With the server down,
    at launch or later, every cached worker is dialled at its cached address, and the
    titlebar shows one quiet "server unreachable" line. The app never shows a modal for it.
  - **Adding by address stays, as the fallback.** The panel opens on "Connect to a server"
    and offers "Add a worker by address instead". Workers added that way stay without the
    server. A worker only the directory listed leaves, tiles included, when the server is
    disconnected or its directory stops listing it.
  - **Agents.** `Event::Agent` relayed by the server counts toward the agents that need the
    human even where the worker's own link is down or no tile shows the session. ⌘⇧A walks
    the tiled ones in reading order, then the rest. For one without a tile it proposes a
    terminal item for that session on its worker, or says the worker is not reachable from
    here. The banner comes from the worker's link when there is one, from the server's
    otherwise.
  - The palette gained "Connect to a server", "Disconnect from the server" and "List
    workers". The list shows each worker with its state, and ↩ goes to its first tile or
    opens a shell on it.

- ✅ **The worker is called a worker everywhere** (2026-09-25). The code said "host" for the
  worker role long after the three roles were named, so the names now follow the roles, with
  no aliases.
  - **Crates and binaries** mirror the server's: the library `slopty-worker`, and the package
    `slopty-workerd` that builds the binary `slopty-worker`. `slopty-ptyd` keeps its name.
  - **Wire.** `WorkerMsg`, `slopty_net::worker::WorkerListener`, `WorkerConn`, `WORKER_PORT`,
    `CloseReason::WorkerShutdown`, `Rejection::ProtocolVersion { worker }`. Postcard encodes
    no names, so the proto goldens are byte for byte what they were and `PROTOCOL_VERSION`
    stays. `HostAddr` keeps its name: it is a network host, and it names the server too.
  - **Around the daemon.** `slopty worker install|uninstall|status|doctor|screens`, the
    `--worker` flag on `sessions`, `ping`, `attach` and `bench`, the launchd label
    `dev.aislopware.slopty.worker`, `worker.sock`, `slopty-worker.log` and the
    `SLOPTY_WORKER_*` variables. `settings.toml` has one `[worker]` table: `allow` moved into
    it from `[host]`.
  - **Moving an installed worker.** Nothing reads the old label. Boot out
    `dev.aislopware.slopty.hostd` and delete its plist, then run `slopty worker install` from
    the new build; the new signing identifier needs its permissions granted again.

- ✅ **A session's summary carries its agent; the file card saves; a tile can be a web page**
  (2026-09-25, protocol 53).
  - **Agent in the summary.** `SessionSummary.agent` is the agent in the session and its status
    (`None` for a shell), filled on every summary from the daemon's agent table. The worker's
    `Worker` holds the table's handle from `connect`, and orchestration reads it from there.
    An agent whose status reads `None` is left out. The server's hub keeps the agent of each
    listed terminal current from the worker's `Agent` events, so `ListTerminals` answers with
    live status, and `slopty workers` and the `list_workers` tool no longer send one
    `AgentStatus` per terminal. A waiting agent on a worker that is not online is not counted:
    what it last reported may have been answered since. `SessionKind`, a one-variant leftover,
    is gone.
  - **Saving a file card.** `ClientMsg::WriteFile { path, text, base_modified_ms }` is answered
    with `WorkerMsg::Written { path, result }` (`slopty_worker::file::write`).
    - A file whose modification time on disk is newer than `base_modified_ms` is a `Conflict`
      carrying the time on disk, and nothing is written. `None` writes regardless. A missing
      file is created, since an editor saving over a deleted file expects that.
    - A directory, a pipe or any other non-regular file is `Failed` before it is opened, as a
      read is. So is a text past `FILE_BYTES` (512 KiB), and a file on disk past it: the card
      only ever held the first part of such a file, and saving it would cut the file there.
    - The write is `file::replace`, the same path orchestration's `write_file` takes: a
      temporary file beside the target, fsync, rename over, the old mode kept. A symbolic
      link is followed and the file it names is replaced, so the link survives; renaming over
      the link would have swapped it for a copy.
    - `Saved` carries the new size and modification time, the base of the next save. The
      watchers, the writer's own included, hear the new text on their next one-second look,
      as for any change on disk.
  - **Browser items.** `ItemKind::Browser { url }` is a web page tile, usually a forwarded
    port. The registry takes an `http` or `https` address with a host, 2 KiB at most, with no
    whitespace or control characters, so a shared document cannot make another client open
    `file:` or `javascript:`.

- ✅ **The fleet has one event log; terminals take a size; files read in ranges** (2026-09-25,
  protocol 56). An audit of what an agent orchestrating many workers could not do found five
  gaps, each closed with a verb rather than a workaround.
  - **Events.** `WaitFor` watches one terminal, and the MCP endpoint is stateless HTTP with no
    notifications, so an agent watching ten workers had to poll each. The hub now logs what it
    already hears into one sequence (`HubEvent`): liveness moves, a worker removed, terminals
    opened and closed, and agent status changes, a report of the status already known left
    out. The log keeps the newest 4096. `Events { since, timeout_ms, filter }` answers the
    events after the cursor with the next cursor and how many the log dropped, and blocks up
    to the timeout (capped as `WaitFor` is) when there are none. `since` absent means from now
    on. A cursor ahead of the log, from before a server restart, reads from the oldest.
    `EventFilter::AgentNeedsInput` makes it a wait for any agent on any worker that needs a
    human or went idle, which is the `WaitAny` the audit asked for, with no verb of its own.
    It is a long poll rather than a push because it works the same over stateless HTTP, the
    stdio shim and the CLI (`slopty events --follow`), and a cursor loses nothing between
    calls. Rejected: MCP resource subscriptions and server-sent notifications, which a
    stateless endpoint cannot hold and Claude Code does not document consuming.
  - **Size.** A terminal opened by a verb was 120×36 until a client showed it, which is too
    narrow for some programs and wasteful for a model reading the screen. `OpenTerminal` and
    `SpawnAgent` take an optional `Size`, and `ResizeTerminal` resizes, within 10×2 and
    1000×500. A session's size belongs to the client driving it, so a resize is refused while
    any client shows the terminal. Otherwise the session's actor applies it, the check and the
    resize one step there (`SessionHandle::resize_unviewed`), so a client attaching meanwhile
    keeps its seat and the next client to attach drives as before. The worker's link sends the
    resized summary to the server ahead of the answer, so `list_terminals` shows the new
    size.
  - **Forgetting a worker.** `ForgetWorker` removes an entry without a live lease, emits
    `WorkerRemoved`, sends every link the directory without it and rewrites the state file. An
    online worker is refused: it would register again at once. `slopty workers forget`.
  - **Files.** `ReadFile { offset, length }` answers `File { bytes, offset, size }`, so a
    caller knows the whole size and pages through a large file; one read returns at most 8 MiB
    (`MAX_FILE_BYTES`, half a frame, asserted at compile time), which leaves room for the
    envelope. A read of the rest over that cap is refused with a hint to read in parts, and
    `slopty cat` reads in parts. `ListDir` answers entries by name with kind, size and
    modification time, 10 000 at most, with the total. `Stat` follows links and answers `None`
    for a missing path. `SpawnAgent` takes arguments and environment for `claude`.
  - **An oversized request from the CLI.** `slopty mcp` and the CLI sent a request past a
    frame down the link, the send failed, and the link was dropped with "the connection to the
    server was lost". The encoder refuses before writing, so the link now answers that call
    `Invalid` with the server's own wording and carries on.
  - Tests: `events_are_read_from_a_cursor_and_waited_for`,
    `the_event_log_is_bounded_and_says_what_was_missed`,
    `a_worker_is_forgotten_only_when_it_is_not_online` (hub);
    `a_file_is_read_in_ranges_with_its_size`, `a_directory_lists_by_name_and_a_path_stats`
    (`slopty-worker`); `the_largest_read_fits_in_one_message_and_a_larger_file_is_read_in_parts`
    and the resize in `a_worker_registers_answers_forwarded_verbs_and_comes_back`
    (`slopty-workerd` `server_link`); the events, resize, range, `ls` and `stat` steps of
    `cargo xtask e2e server`.

- ✅ **Orchestration holes a review found** (2026-09-25, protocol unchanged).
  - **Event cursors across a restart.** Each run started its sequence at 1, so a cursor kept
    from the last run landed inside the new range and skipped its first events with `missed`
    at 0. A run's sequence now starts at its start time in microseconds since the epoch. A run
    logs far fewer than one event a microsecond, so every earlier cursor sits below the new
    run's first number. A cursor outside this run's range reads from the oldest event held
    and reports at least one missed. `missed` also adds up over every read of one long wait,
    where it used to keep the first read's count.
  - **Agent state on a client link.** A link that fell behind the broadcast got the directory
    again and nothing else, so agent badges went stale. A link now gets the state on connect
    and again after a lag: the directory, then every terminal with its agent as a quiet
    `Agent` event (no attention). The link also remembers which agents it told the client
    of, and takes back each one the registry no longer has, with `SessionClosed` or a `None`
    status.
  - **A removed worker's terminals.** `ForgetWorker` and a reinstall replacing an entry now
    send `SessionClosed` for each of the worker's terminals to the log and to every link.
  - **Requests of a link that left.** Each request ran on a detached task, so a CLI or MCP
    client that disconnected left its `WaitFor` or `Events` running for up to 255 s. The
    requests now live in a `JoinSet` owned by the link's reader, and ending the link aborts
    them.
  - **Files.** `ReadFile` opens with `O_NONBLOCK` and checks what it opened. A named pipe or
    a device is refused at once, where opening a pipe used to hold a blocking thread for
    good. `ListDir` keeps only the first `max` names in a bounded heap and stats those alone,
    so a directory of a million entries costs a pass over its names. `total` still counts
    every name.
  - Tests: `a_cursor_from_before_a_restart_misses_nothing_unannounced`,
    `a_long_wait_counts_what_the_log_dropped_while_it_waited`,
    `a_removed_worker_closes_its_sessions_everywhere` (hub);
    `a_client_gets_the_agents_on_connect_and_again_after_it_lagged`,
    `a_client_that_leaves_takes_its_requests_with_it` (link);
    `a_named_pipe_is_refused_not_waited_on`,
    `a_directory_is_stat_only_for_the_entries_it_answers` (`slopty-worker`). Each failed on
    the old code.

- ✅ **The cached directory has one writer** (2026-09-26). The app used to write
  `directory.json` from a new blocking task on every change of the directory. The through-server
  e2e caught a restarted worker's `Worker` updates arriving close together: two writes were in
  flight on the same temporary file, one renamed the other's half-written file over the cache,
  and the second failed with `No such file or directory`. An older directory could also land
  last. Now the app hands each change to one task over a watch channel
  (`slopty_app::server::write_cache`). A write finishes before the next starts, and a burst of
  changes becomes one write of the latest. Disconnecting from the server removes the file
  through the same task, so a write still running cannot bring the file back. Test:
  `a_burst_of_directory_changes_leaves_the_last_one_cached_and_a_removal_removes_it`.
