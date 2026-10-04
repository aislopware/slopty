# Decisions — Topology: one server, many workers, many s

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
  - **Command channel.** Server → worker commands (`OpenTerminal`, …) travel down the same
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
  - Superseded in part 2026-10-04 by **The first Mac runs the server**: a server always exists,
    so one that does not answer is an outage and never a mode. The cached directory and the
    direct dials are that outage's path; there is no client set up without a server.

- ✅ **One verb set drives workers** (2026-09-24, its surfaces narrowed 2026-10-04). The verbs
  live in `slopty-proto::orchestration`, and item ids are explicit handles. The CLI names them:
  - `slopty workers`, `slopty item list`
  - `slopty open --worker … --cwd … -- <cmd>`, `slopty send <term> --text|--keys`
  - `slopty screen`, `slopty output --since <line>`, `slopty commands --since <line>`
  - `slopty wait <term> --output <pattern> | --quiet | --exit | --command-done |
    --agent-input`
  - `slopty agent spawn`, `slopty agent status`, `slopty close`
  - `slopty cat` / `slopty put`, `slopty ports`

  **Reads come from the worker's libghostty grid, never raw PTY bytes.** There are three
  views: the rendered screen, scrollback by absolute line index (the same numbering the row
  diffs use), and OSC 133 command blocks with exit codes.
  - **Why not screen-scraping.** Surveyed orchestrators show how it fails. claude-squad detects
    a waiting agent by matching a literal prompt string. OpenHands needed recovery for PS1
    sentinels corrupted by concurrent output, and a 30 s no-change timeout so tools do not hang.
  - **Agent status** stays hook-derived (`docs/decisions/claude-code.md`).

  **The surfaces:**
  1. **The `slopty` CLI**: every verb, with JSON output. A person's script and an agent's
     shell both drive the fleet through it.
  2. **MCP** Streamable HTTP on the server, rmcp 3.4.1, protocol revision 2026-07-28
     (stateless, no `initialize`, server-minted handles as plain arguments), and
  3. **`slopty mcp`**, a stdio shim that also pushes "agent on worker X is waiting" into the
     orchestrating session. Both serve only a project's eight tools (`project_status`,
     `task_get`, `task_start`, `task_update`, `task_report`, `task_tell`, `task_wait`,
     `read_thread`; `docs/decisions/projects.md`, "A frontier model is steered by a sentence,
     not by machinery"). Since 2026-10-04 every other verb is the CLI's alone: an agent's
     shell already runs it, and a tool block of 46 tools cost every agent about 24 KB of its
     context.

  A long wait reports progress and is capped under Claude Code's 5-minute HTTP idle abort.
  The MCP Tasks extension is not relied on, because Claude Code does not document consuming it.

- ✅ **Admission without crypto** (2026-09-24). Every listener (worker and server) accepts a
  connection only from loopback, Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48) or private LAN
  ranges, checked once per connection.
  - Superseded in part 2026-09-26 by **Tailscale is the network, and its LocalAPI says who is
    calling**: the tailnet is checked through whois and grants, and the LAN is no longer a
    default.

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
    `Native`, and those two probes, which it has since 2026-09-28.

- ✅ **The CLI names a terminal `worker/session` and resolves names on the client**
  (2026-09-25).
  - **Handles.** A terminal prints as `worker/session` with both UUIDs in full (the `term`
    field in JSON), and every verb takes it back. The worker part may also be a name and the
    session part a unique id prefix; full ids cost no round trip, anything else one
    `ListWorkers` or `ListTerminals`. Text output shortens the handle to `name/prefix`, where the
    prefix is the shortest one no other listed session shares, at least 8 characters: `UUIDv7`s
    open with their creation time, so ids minted a minute apart share their first 8.
  - **One JSON shape per answer.** `slopty … --json` prints the views of `slopty-tools`
    (`crates/slopty-tools/src/view.rs`) with snake_case keys, not the wire enums' serde form.
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
    research preview on an older revision. A long wait (`task_wait`, a `project_status` that
    waits) sends progress every 10 s when the call carries a progress token.
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
      check (loopback, the tailnet, and any `allow` ranges). It answers the same peers that
      binding 127.0.0.1 plus the tailnet address would, and it also reaches interfaces that
      come up after the server does, such as Tailscale starting late at boot.
    - It does not check `Host`, since tailnet names and IP literals are as legitimate as
      `localhost`. It refuses any request that carries `Origin` with a 403 instead. A browser
      always sends that header, and a DNS-rebinding page cannot leave it out.
  - **Timeout cap.** The server caps every wait (`WaitFor`, `Events`, a task's wait) at 240 s
    (`WAIT_CAP_MS`), under Claude Code's five-minute idle abort of a tool call. It gives a
    forwarded wait its own timeout plus 15 s before it stops waiting on the worker, and every
    other forwarded verb 60 s.

- ✅ **The worker's side of the link: registration, redial, and what the verbs mean** (2026-09-25).
  - **Joining.** `slopty-worker` registers with the server named by `--server`, else
    `SLOPTY_SERVER`, else `[worker] server` in `settings.toml` (port 45560 when none is given).
    With none it still starts and serves its clients, though since 2026-10-04 no installer
    leaves one so (**The first Mac runs the server**). The link is one task beside the rest of the
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
    `WriteFile` of a frame's worth) with `Invalid`, and its writer to a client does the same
    for a reply. Any other write failure ends the link: on the worker it ends the session and
    it redials, and on the server it ends the lease. The MCP endpoint took a frame's worth of
    file in one request body until 2026-10-04, and its test proved the worker writer's guard
    through it. Since MCP serves only a project's tools, a file comes from the CLI, whose
    encoder refuses an oversized request before it leaves (below, "An oversized request from
    the CLI"), so the server's guard stays as a backstop with no test that can reach it.
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
  - **The shape is the CLI's.** Verbs take `term` strings and worker names or prefixes, and
    answer with the snake_case views. Since 2026-10-04 the MCP list is a project's eight tools
    (above, "One verb set drives workers"); a tool failure, a name that does not resolve
    included, is an `isError` result reading `message (Code)`, and an unknown tool is invalid
    params.
  - **Files.** `slopty cat --json` answers `content` with `encoding` `utf8` when the bytes are
    UTF-8 and `base64` otherwise, plus the file's `size`.
  - **What stays on each side.** The 240 s wait cap lives in the hub; the tools only pass the
    timeout through. A tool's long wait reports progress through an optional sink: `slopty
    mcp` turns it into its 10 s progress notifications, and the server's stateless HTTP
    endpoint gives none. The stdio shim keeps the redial and the needs-a-human notifications.
  - A test sends the same calls to the server's endpoint and to `slopty mcp` dialled to that
    server, over one fake worker, and requires identical results
    (`the_server_endpoint_and_slopty_mcp_answer_a_call_alike`).

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
    `HostAddr`); empty is the first run. The panel's "Connect to a server" dials the address
    once as a client, and only an address that answered is written into the file, with the
    rest of the file and its comments left as they were. A file that fails to parse keeps the
    server in use rather than dropping it. ("Disconnect from the server" went 2026-10-04.)
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
  - **Adding by address stays, as the fallback.** Superseded 2026-10-04 by **The first Mac
    runs the server**: adding by address is gone, and every worker comes from the directory.
    A worker leaves, tiles and pages included, when the directory stops listing it.
  - **Agents.** `Event::Agent` relayed by the server counts toward the agents that need the
    human even where the worker's own link is down or no tile shows the session. ⌘⇧A walks
    the tiled ones in reading order, then the rest. For one without a tile it proposes a
    terminal item for that session on its worker, or says the worker is not reachable from
    here. The banner comes from the worker's link when there is one, from the server's
    otherwise.
  - The palette gained "Connect to a server", "Disconnect from the server" (gone 2026-10-04)
    and "List workers". The list shows each worker with its state, and ↩ goes to its first tile or
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
    live status, and `slopty workers` no longer sends one
    `AgentStatus` per terminal. A waiting agent on a worker that is not online is not counted:
    what it last reported may have been answered since. `SessionKind`, a one-variant leftover,
    is gone.
  - **Saving a file card.** `ClientMsg::WriteFile { path, text, base_modified_ms }` is answered
    with `WorkerMsg::Written { path, result }` (`slopty_worker::file::write`).
    - A file whose modification time on disk is newer than `base_modified_ms` is a `Conflict`
      carrying the time on disk, and nothing is written. `None` writes regardless. A missing
      file is created, since an editor saving over a deleted file expects that.
    - A directory, a pipe or any other non-regular file is `Failed` before it is opened, as a
      read is. So is a text past `FILE_BYTES` (16 MiB since 2026-09-28, when a tile began to
      hold whole files; a save larger than 64 KiB goes up a bulk stream, workspace.md "A file
      tile edits any file up to 16 MiB").
    - The write is `slopty_platform::fs::replace`, the same path orchestration's `WriteFile`
      takes: a temporary file beside the target, its data ordered ahead of the rename, rename
      over, the old mode kept. A symbolic
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
    It is a long poll rather than a push because it worked the same over stateless HTTP, the
    stdio shim and the CLI, and a cursor loses nothing between calls; since 2026-10-04 the CLI
    alone serves it (`slopty events --follow`). Rejected: MCP resource subscriptions and
    server-sent notifications, which a stateless endpoint cannot hold and Claude Code does not
    document consuming.
  - **Size.** A terminal opened by a verb was 120×36 until a client showed it, which is too
    narrow for some programs and wasteful for a model reading the screen. `OpenTerminal` and
    `SpawnAgent` take an optional `Size`, and `ResizeTerminal` resizes, within 10×2 and
    1000×500. A session's size belongs to the client driving it, so a resize is refused while
    any client shows the terminal. Otherwise the session's actor applies it, the check and the
    resize one step there (`SessionHandle::resize_unviewed`), so a client attaching meanwhile
    keeps its seat and the next client to attach drives as before. The worker's link sends the
    resized summary to the server ahead of the answer, so `ListTerminals` shows the new
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
  - **An oversized request from the CLI.** The CLI sent a request past a
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

- ✅ **Tailscale is the network, and its LocalAPI says who is calling** (2026-09-26, at the user's
  request to build on Tailscale and compatible control servers such as Headscale and slopscale
  rather than reinvent a mesh). The research, with sources, is `.research/tailscale-2026-09-26.md`.
  - **Use the installed daemon; embed nothing.** Slopty reads the Tailscale already running on the
    machine through its LocalAPI (`slopty-tailnet`, hyper over loopback TCP or the daemon's unix
    socket). All three macOS installs are found without a shell-out: the App Store extension's
    port and token from the name of the file it holds open (read through libproc,
    `slopty_platform::proc_files`, as `lsof` would), the standalone app's `ipnport` and
    `sameuserproof-<port>`, and `tailscaled`'s socket. The token never reaches a log.
    - Rejected for now: embedding a node. libtailscale adds 21 MB and a Go runtime and hands
      out stream socketpairs that QUIC datagrams cannot ride; tsnet's userspace netstack
      uploads at a fraction of the kernel path; tailscale-rs 0.6 has no iOS, no peer relays
      and no app capabilities. Revisit tailscale-rs when it has iOS and app capabilities.
    - Rejected: `tailscale-localapi` 0.6, which covers status, cert and whois without
      `CapMap`.
  - **Admission asks whois; roles come from grants.** This replaces the source-address ruling
    above and in `workers.md` for the tailnet. Loopback and the `[worker] allow` ranges (a plain
    VPN) are let in as anything, by address. A tailnet address is looked up with
    `whois?addr=ip:port` on the connection's own task, before the handshake:
    - a node of the user this machine belongs to gets every role;
    - any other node, another user's or a tagged one, gets the roles a tailnet grant gives it
      under the app capability `github.com/aislopware/slopty` (`{"roles":["client","agent",
      "worker"]}`), which Headscale 0.29 passes through verbatim;
    - an address no node has, a node with no role, or a daemon that fails is refused.
    - Where no daemon this process can read runs, no tailnet address gets in (fail closed,
      amended the same day after an audit): with the wire unencrypted, an address alone is
      what a host on the LAN could forge, and whois is the only gate. A plain VPN lists its
      range in `[worker] allow`. The daemon is looked up when a tailnet peer calls, again
      every 5 s while none is found, and again at once when it stops answering, so a worker
      launchd starts at login ahead of Tailscale, or an App Store extension back on a new port
      and token, needs no restart. Test: `tailscale_is_looked_up_again_when_missing_or_failing`.
      The machine's owner is read from status at most once a minute.
  - **The LAN is no longer a default.** RFC 1918, ULA and link-local ranges were let in by
    address; with Tailscale as the network they are only what `[worker] allow` lists.
  - **Refusal in the role.** A worker closes a connection whose node lacks the client or agent
    role with `NOT_GRANTED`. The server answers a hello whose role is not granted with
    `Refusal::NotGranted`, and a refused worker keeps redialling, since a policy change can
    grant it. The MCP endpoint wants the agent role.
  - **Finding the server.** With no `--server`, `$SLOPTY_SERVER` or `[client] server`, the CLI
    tries every online node the status lists with a bare QUIC handshake on the server port, all
    at once, within 1.5 s (`slopty_net::discover`). A node tagged `tag:slopty-server` comes
    first, then this machine, then the rest; phones are skipped. The handshake registers
    nothing, and the port and the lease ALPN are what mark a server. The app's "Connect to a
    server" panel, shown while no server is set, does the same and puts the first answer in the
    empty address field, naming it ("Found studio.tail1234.ts.net on your tailnet"); connecting
    stays a click. Test: `the_server_is_found_through_the_local_tailscale`. Since 2026-10-04
    "Use this Mac" joins the one Ready server the look finds, asks when several answer, and
    starts one here when none does (**The first Mac runs the server**).
    - Rejected: `MagicDNS` SRV/TXT records (control pushes A/AAAA only), Tailscale Services
      (TCP only, tagged hosts, missing in Headscale), and control-plane APIs, which differ per
      control server.
    - A worker still joins only the server it is told, since joining is a trust decision.
  - **A worker on the server's machine.** It dials over loopback, and the server used to
    publish it at `127.0.0.1`, which no other machine can dial. The server now publishes it at
    the machine's tailnet IPv4 when Tailscale gives one.
  - **Paths.** The worker reads status every 2 s while any tailnet client is connected (one
    reader for all of them) and tells each client how its packets travel: direct, peer relay,
    or DERP with the region (`WorkerMsg::Path`, sent on each change). DERP is TCP through a
    relay and the slow path, so a client can say so rather than leave a slow stream
    unexplained.
    - Deferred: a disco ping to warm a path before the first stream. The connection the app
      opens at launch already starts disco.
  - **Doctor.** `slopty worker doctor` names this Mac's node and address, or says Tailscale is
    down, or that the daemon can read none and so admits no tailnet peer.
  - **Open.** Tailscale on Linux drops CGNAT-sourced packets that arrive off the tunnel; no such
    filter was found for darwin, and macOS is a weak-host stack. A host on the same LAN could
    send packets with a tailnet source address, and with no encryption on the wire only the
    unpredictability of the QUIC handshake stands in its way.
  - Tests: `slopty_tailnet` (the three locations, status paths, whois, grants, the LocalAPI
    against a fake daemon, and `this_machine_is_its_own_users_node` against the real one),
    `slopty_net::admission`'s `the_tailnet_says_who_is_calling_and_what_they_may_do` and
    `without_a_daemon_only_loopback_and_the_listed_ranges_get_in`,
    `tailscale_is_looked_up_again_when_missing_or_failing`,
    `slopty_platform::proc_files`, `slopty_net::discover`'s
    `the_tagged_server_is_tried_first_and_phones_never` and
    `a_listening_server_answers_and_a_closed_port_does_not`, the server's
    `a_worker_on_the_servers_machine_is_published_at_its_tailnet_address`, the worker's
    `a_client_hears_its_path_when_it_changes_and_only_then`, and the CLI's
    `doctor_report_names_the_binary_and_flags_missing_permissions`.

- ✅ **Orchestration arranges the workspace too** (2026-09-27). The verb set named `ListItems`
  from the start, but only terminals had verbs, so an agent could start a dev server and not
  show anyone the page it served. Six verbs now reach the worker's item registry, the same one
  a client's tiles come from: `ListItems`, `OpenItem` (a page, a file to edit, a note, a
  window or a display), `RenameItem`, `RemoveItem`, `PointAt` and `ListWindows` (what
  `OpenItem` can stream). In the CLI they are `slopty item list|open|rename|remove|point` and
  `slopty windows`.
  - **An item is a handle like a terminal.** `ItemRef` is worker and item, printed
    `worker/item` and resolved from an id prefix as a session is.
  - **The registry decides as it does for a client.** An orchestrated change goes through the
    same checks (a web address is `http` or `https`, a name is trimmed and at most 128
    characters) and reaches every client as a delta from the nil client, so nobody takes it
    for its own echo. A rename reads and writes the item under one lock.
  - **A terminal's item belongs to its session.** `OpenItem` refuses a terminal (a session has
    to start first: `OpenTerminal`), and `RemoveItem` refuses a terminal's item (`Close` ends
    both).
  - **Pointing** is the client's `Point`, sent from the orchestrator as "Orchestration": each
    client offers a jump to the tile. It says nothing about who asked; the verbs carry no
    caller.
  - **A program's exit is an event.** A terminal whose program ends stays listed with its last
    screen, so the server heard the exit only as one more summary update and logged nothing;
    `events` could not tell a fleet-wide watcher that a build finished. The hub now logs
    `SessionExited { term, status }` when a listed session goes from running to exited, once.
  - No verb installs an agent's hooks: an agent `SpawnAgent` starts carries them on
    `--settings` (`claude-code.md`), and the settings file changes only when a person asks.
  - Tests: `slopty-workerd` `an_orchestrated_item_reaches_a_client_and_leaves_it`, the hub's
    `events_are_read_from_a_cursor_and_waited_for` (the exit),
    `slopty-tools` `an_item_prefix_must_be_unique`, and the
    `workspace_items` goldens.

- ✅ **The server builds for Linux, and a gate lane keeps it so** (2026-09-27). The topology puts
  the server on any always-on box, and the box most people have for that runs Linux. The server
  and what it stands on (`slopty-core`, `slopty-proto`, `slopty-tailnet`, `slopty-net`,
  `slopty-tools`, `slopty-server`, `slopty-serverd`) already passed clippy for
  `x86_64-unknown-linux-musl` untouched; nothing kept it that way. The gate's iOS lane now runs
  clippy `-D warnings` for that triple on those crates after the iOS pass, and
  `cargo xtask check` does it for any of them it is given. The target is in
  `rust-toolchain.toml`. The lane builds without the workspace hack, whose features would pull
  the client's GPUI into a server build.
  - Musl, since a static binary is how a server ships to a box it did not build on. Linking is
    not checked: clippy stops before it, and a cross linker is a release concern.
  - Not yet: the worker and the CLI. The CLI takes the worker's and the client's crates, which
    call libproc, ScreenCaptureKit, VideoToolbox and the pasteboard directly; a Linux worker
    needs those behind platform seams first (`slopty-platform` is where they go). Both joined the
    lane on 2026-09-28, and the terminal-only Linux worker runs (`platform.md`, "Linux proven in a
    container").

- ✅ **A worker is published where it listens, and the server admits a VPN** (2026-09-27). Two
  gaps the through-server e2e found once it ran on a Mac with Tailscale up:
  - **Where a worker is listed.** The server listed a worker that dialed over loopback at this
    machine's tailnet address, whatever the worker listened on. A worker started with `--bind
    127.0.0.1` listens on nothing there, so clients were sent where no one answers.
    `Registration.port` became `listen`, the socket address the worker's listener is bound to.
    A worker bound to one address is listed there. One bound to loopback is listed at the
    loopback address it dialed from, reachable only on this machine. One on every interface
    keeps the tailnet rewrite. The `server_worker_hello` golden moved.
  - **Who reaches the server.** The worker read `[worker] allow` ranges for peers Tailscale
    does not vouch for, but the server admitted only loopback and the tailnet: a person on a
    plain VPN reached their workers and not the server that lists them. `[server] allow`, in
    the `settings.toml` of the data directory the server's own lives in (the one the worker
    and the app read), now does for the server what `[worker] allow` does for a worker.
    `slopty_net::admission::parse_allow` parses both. `slopty-settings` joined the Linux lane
    with the server.
  - The e2e fleet binds both workers to loopback, so a run no longer depends on whether
    Tailscale is up on the machine.
  - Tests: `a_worker_on_the_servers_machine_is_published_at_its_tailnet_address` (the bound
    cases), the server's `the_allow_list_comes_from_the_shared_settings`, and
    `the_app_finds_its_workers_through_the_server`.

- ✅ **A worker's greeting names its home and what it can do** (2026-09-27). A client guessed a
  home directory from a path's shape (`/Users/<name>`), so a worker whose daemon runs with
  another `HOME` showed its whole path where `~` belonged, and a client that dialed a worker
  directly never heard its capabilities: only the server's directory had them, and nothing
  showed a Mac without Screen Recording until a window stream failed.
  - `HelloAck.home` is the daemon's `$HOME`. `HelloAck.caps` is the `WorkerCaps` the server
    registers with, and `WorkerMsg::Caps` carries each later change: a grant, a display, a load
    step. The daemon keeps one caps watch for its whole life (`Daemon::caps`), which the
    greeting, the resync after a lagged broadcast and the server link all read.
  - Tests: the `worker_hello_ack` and `worker_caps` goldens, and the worker's
    `the_greeting_names_the_home_and_what_the_worker_can_do`.

- ✅ **A verb that changes something is done once per idempotency key, and the worker keeps
  the table** (2026-09-27). An agent whose HTTP call timed out, or whose link dropped mid-verb,
  sends the verb again, and a second open, keystroke or write was a second effect.
  - **The key.** `ToServer::Request` and `FromServer::Request` carry an optional
    `IdempotencyKey`, the caller's name for the effect: a UUID, or up to 128 printable ASCII
    characters. It rides the envelope rather than each verb, since it names the call, not what
    the call does. `Verb::changes` says which verbs honour it: the opens, `SendInput`, `Close`,
    `WriteFile`, `ResizeTerminal`, the item changes, `ForgetWorker`, and `WaitFor`, whose met
    wait moves the session's mark so a repeat would wait for the next match. A read ignores
    the key and is answered afresh.
  - **The worker keeps the table** (`slopty_worker::orchestrate::idempotency::Ledger`). The
    effects happen there, and the worker outlives both of the failures that make a caller
    retry: a server restart, and a dropped server link, after which the worker redials with
    its table intact. A table on the server would forget on restart. It could not tell a
    verb lost on a dropped worker link from one the worker had done. The first attempt runs on
    a task of its own, so it finishes and its answer is kept even when the link that asked is
    gone. A repeat arrives while it runs and waits for that answer. The same key with other
    arguments (compared by a BLAKE3 digest of the encoded verb) is `Invalid`. Every outcome
    is kept, a failure included; a new attempt takes a new key.
  - **Bounds.** A key lives `KEY_LIFETIME` (10 min) after its answer, beyond any caller's
    retries of one call. At most 4096 keys stay per worker. When full, the key answered
    longest ago goes first. A running verb is never dropped: with every slot running, a new
    key is refused rather than left unguarded.
  - **The server keeps one small list** for the one effect it owns: forgetting a worker. A
    forget repeated under its key answers `Done`, not `UnknownWorker`.
  - **`Interrupted`.** A new `ErrorCode` for a verb that went out and whose answer a dropped
    link lost, so it may have been done. The server answers it when a worker's link drops
    under a forwarded verb, and the CLI's link when its own does. `WorkerUnreachable` and
    `ServerUnreachable` now mean the verb never went out.
  - **The surfaces.** The CLI takes `--idempotency-key`. Its link gives every changing verb a
    fresh key when none is given, and it redials after a loss. On `Interrupted` it sends the
    verb again under the same key, through a worker or server still coming back, for about
    8 s. `slopty mcp` shares that link. The project tools that change something take an
    optional `idempotency_key`.
  - Tests: the ledger's five (repeat, in flight, reuse, lapse, bound), the hub's
    `a_keyed_forget_repeats_its_answer`, the CLI's
    `a_lost_answer_is_asked_again_under_the_same_key`, the `idempotency_keys` goldens, and the
    server e2e, where an open repeated under one key over the CLI opens one terminal.

- ✅ **An orchestrating agent reads and answers another, sees a screen, and moves any file**
  (2026-09-28). An agent could see another was blocked but could not answer it except by typing
  its menu's digits, read only its screen, take no picture of a window, and move no file past
  one 8 MiB read. Four verbs close that, in the CLI (`slopty agent read|answer`, `slopty
  capture`, `slopty push|pull`); a project's `read_thread` tool reads a thread over MCP as
  well.
  - **Reading and answering another agent** went through Claude Code's conversation face
    (`ReadConversation`, `AnswerPermission`) until 2026-10-04. They are now the agent-neutral
    `ReadThread` and `AnswerRequest` over the thread model, for any agent
    (`docs/decisions/projects.md`, "Any agent's thread is read and answered alike"). Following
    is unchanged: orchestration follows a Claude Code session as a link of its own
    (`conversation::ORCHESTRATION`) from the person's first read until the session ends
    (`Holds::forget`, also on a client's close), so its prompts are held while someone may
    answer them.
  - **The still** (`CaptureStill`) is `SCScreenshotManager` at native size, PNG-encoded on the
    worker and halved until it fits a reply. A worker without Screen Recording, or without a
    desktop, answers the new `ErrorCode::Unsupported` from the preflight, which prompts nobody.
    `slopty capture` writes the PNG where it is told.
  - **Files of any size** go in 1 MiB parts, four in flight, over the verb link
    (`slopty_tools::bulk`). Up, `Upload` writes parts where they go in a partial beside the
    target, named for the upload, and a finish checks size and BLAKE3 before it renames. A
    file replaced keeps its mode, and a new one takes the caller's, as `scp` does. Down, they
    are `ReadFile` ranges into a partial here, with the file's size and time compared before
    and after. Only the finish takes the key. The upload is named for the key, so a retried
    call writes into the same parts, and an abort after the finish sweeps what a repeat wrote
    again. The parts ride the server's links rather than the client's bulk streams, because
    the CLI runs the same ops over `Dispatch` as the server does. A 1 MiB part holds the
    link's other verbs and events for a moment at most. Until 2026-10-04 the server's MCP
    endpoint, on another machine than its caller, refused to move files; MCP no longer
    offers it.
  - Tests: the `agent_verbs` goldens; `Holds`'
    `orchestration_follows_until_the_session_ends`; the worker's page, upload and still
    tests (`orchestrate::{thread_read, upload, still}`); `bulk`'s three against an
    in-memory worker; the tools' `a_thread_is_read_by_task_thread_or_term_and_answering_is_no_tool`;
    the hub's
    `the_agent_screen_and_upload_verbs_go_to_their_worker`; and the server e2e. There a
    9 MiB file is pushed and pulled, a conversation played through `slopty hook` is read, and
    its `PermissionRequest` relay prints the denial the person answered over the CLI, after
    MCP offered an agent no tool to answer with. The still is asked of a window no worker has,
    so no picture is ever taken.

- ✅ **The worker's control protocol lives in `slopty-proto`** (2026-09-28). The worker's Unix
  socket is a wire between processes: the CLI asks it for status, doctor and screen counters,
  `slopty hook` relays hooks and waits on permission decisions, and the app reads the doctor to
  make this Mac a worker. Its types sat in `slopty-worker` (`CtlRequest`, `CtlReply`, `Health`,
  `Tailscale`, the screen counters) and `slopty-agent` (`PermissionAsk`, `PermissionAnswer`,
  `Decision`). The app only wanted the doctor, yet it had to depend on `slopty-worker`, and that
  pulled libghostty-vt, capture, the engine, the PTY, the agent and input into it. They now
  sit together in `slopty_proto::ctl`, and the app no longer depends on the worker.
  - **What stayed.** What the worker does with its counters (the latency ring, the registry)
    stays in `slopty_worker::screen`. The Claude Code side of a decision stays in
    `slopty_agent::permission`: the hook output it prints (`hook_output`), the prompt, the
    verdict's decision, and the relay's timeouts.
  - **Still JSON.** `Decision::AllowAlways` carries Claude Code's `updatedPermissions` as the JSON
    they came in, and the enums are tagged by field, so these types are JSON lines, not
    postcard. For that `slopty-proto` takes `serde_json`. The lines are unchanged.
  - Tests: the `ctl` goldens in `slopty-proto`'s `tests/golden.rs`, one line per request and
    reply variant, every decision, and a `Health` with and without Tailscale, each read back;
    `ctl`'s `a_permission_request_and_its_decision_are_single_json_lines`, moved from the
    worker unchanged.

- ✅ **Tailscale's state is a type, in the tailnet crate and on the control socket**
  (2026-09-28). `slopty_tailnet::Status::backend_state` was a string compared with `"Running"`
  in three places, and the doctor's `ctl::Tailscale.state` held either that string or the text
  of the error when the `LocalAPI` did not answer, so a reader could not tell a state from a
  failure. Now the tailnet crate reads `BackendState`, one variant per `ipn.State` Tailscale
  1.102 writes (`NoState`, `InUseOtherUser`, `NeedsLogin`, `NeedsMachineAuth`, `Stopped`,
  `Starting`, `Running`) and `Other` for one it adds later. The doctor's `Health.tailscale` is
  the enum `ctl::Tailscale`: `Up { node, ip }`, `Down { backend: NotUp }`,
  `Unreachable { error }` or `Absent`. It took the place of the `Option` that said absent.
  `NotUp` is the backend state without `Running`, so an up node always carries its name. The
  worker maps one enum to the other with an exhaustive match, since `slopty-proto` cannot depend
  on the tailnet crate. The CLI's doctor and the app's checklist match on the variants; the app
  says "not answering" for an unreachable daemon instead of "not up".
  - Wire: the ctl goldens `ctl_reply_doctor` and `ctl_health_without_tailscale` changed, and
    `ctl_tailscale_down` and `ctl_tailscale_unreachable` are new.
  - Tests: `each_backend_state_reads_by_name` (tailnet), the worker's
    `the_doctor_reads_tailscale_as_up_down_or_absent`, the CLI's
    `doctor_report_names_the_binary_and_flags_missing_permissions` and the app's
    `a_health_report_reads_as_the_checklists_doctor`.
- ✅ **The server speaks one event vocabulary, pushed as it is logged** (2026-09-28, audit
  finding 25). The server had two: `server::Event` pushed to links, and the `Happening`s of the
  `Verb::Events` log. The hub called `happen()` and `announce()` in pairs that could drift
  apart. `server::Event` is gone. `Hub::happen` logs a `HubEvent` under the next sequence
  number and pushes that same event as `FromServer::Event(HubEvent)`, under the state lock, so
  a link and a cursor read the same events in the same order. `Happening::Agent` carries the
  worker's `AgentEvent` whole. A report whose status and source did not change is not an
  event, since it would fill the log with tool-by-tool noise that each client's own worker
  link already carries. A session's plain change (a resize, a `cd`) is kept in the listing and
  is not logged. Its program exiting is (`SessionExited`).
  - **A lagging link gets the state, not a diff.** The per-link `Told` table rebuilt what a
    lagging client had been told and took back what no longer held. It is gone: a link gets
    `Directory` then `Terminals(Vec<(WorkerId, SessionSummary)>)`, both on connect and after a
    lag, and a client replaces what it showed with them (`SessionAgent::quiet_event` turns each
    listed agent into an event that raises no attention).
  - Test: `a_client_gets_the_agents_on_connect_and_again_after_it_lagged` (the lagging link
    and a link that kept up end up the same, and the pushed events equal the log's).
    Goldens: `server_terminals`, `server_event_pushed` and `server_reply_events`.
- ✅ **A worker's load is its own message** (2026-09-28, audit finding 26). `WorkerCaps.load`
  changed on every tick, so the hub compared capabilities field by field to skip it, and each
  tick re-sent the whole `WorkerInfo` to every link. Load now travels on its own:
  `HelloAck.load`, `WorkerMsg::Load`, `ToServer::Load` (the worker sends one right after it
  registers) and `FromServer::Load { worker, load }`, and `WorkerInfo.load` holds the last one.
  Capabilities compare with `==`, a `Caps` message that changes nothing sends nothing, and a
  load tick is never saved. Goldens: `worker_caps`, `worker_hello_ack`, `server_worker_hello`,
  `server_directory`, `worker_load`, `server_load` and `server_worker_load`.
- ✅ **One type each for capabilities, health, displays and Tailscale's state** (2026-09-28,
  audit finding 27). The doctor's `Health` repeated `can_capture`/`can_inject` as
  `screen_recording`/`post_events`, and the worker worked them out a second time. `Health` now
  embeds the daemon's `WorkerCaps`, read from its watch, which may be up to one check period
  (5 s) old. `BackendState` moved to `slopty_proto::tailnet`. The tailnet crate re-exports it,
  and `ctl::Tailscale::Down` carries it, so the `NotUp` mirror and the worker's mapping between
  the two are gone. This supersedes that part of the entry above. `DisplayCap` went into
  `DisplayInfo` (see Platform, "No macOS names on the wire"). Goldens: `ctl_reply_doctor` and
  `ctl_health_without_tailscale`.
- ✅ **The first Mac runs the server; a missing server is an outage, not a mode** (2026-10-04,
  `.research/server-default-2026-10-04.md`). Two ways to reach a worker (the server's
  directory, and an address stored on one client) meant two dial sources, workers that other
  clients never listed, and fleet features (projects, routed notices, wake, Finder's domains)
  that stayed dark wherever no server ran. So a server always exists.
  - **"Use this Mac" decides the server before it installs anything.** The one this app or
    this Mac's worker is set to; else the one server the tailnet look finds Ready; else none
    answered, and it installs `slopty-server` here through launchd with the CLI's own
    installer (`slopty_platform::service::install_server`) and waits up to 10 s for it to
    answer on loopback. Several answering, or no look possible (no Tailscale here), and it
    asks for one address in the panel's field, where empty means "start one here". Then the
    worker's `[worker] server` is set (its own, if it had one, wins), `[client] server`
    follows the worker's, and the flow ends when the directory lists this Mac
    (`this_mac::LISTED_TRIES` looks, 20 s), not with a loopback add. The checklist gains a
    Server line, read from the doctor's link state (`ctl::Health::server`).
  - **Deleted:** adding a worker by address, in the panel and as `slopty add` and `slopty
    forget`; the client's worker list (`workers.json` keeps only the client id, now
    `client.json`); `WorkerSlot.added`; "Disconnect from the server"; "Register machines with
    the server" and the notice that offered it; `--no-server` on `slopty worker deploy`;
    `ServerState::Off` and `Directory::degraded`.
  - **Every install names a server.** `slopty_deploy::Plan.server` is required, and a deploy
    with no address for it fails before anything is sent (`DeployError::NoServerAddress`).
    The app's SSH install waits for the directory to list the machine, and says what the
    worker said of its link when it never does. `slopty worker install` with no server set
    or found installs one beside the worker. The VM and Linux lanes deploy a server first.
  - **Forgetting** a worker that is not online asks the server (`Verb::ForgetWorker`), and it
    leaves every client when the directory unlists it.
  - **Kept on purpose:** the outage (the cached directory, workers dialled directly, each
    client posting its own notifications), and a worker daemon that starts with no server,
    which the worker's own tests use.
  - **Tests.** Every e2e stack runs its own `slopty-server`, the worker registered with it
    and the app following it, so the suite dials through the directory as people do; a relay
    or a cut in front of a worker is what the directory lists (the worker registers over IPv6
    and the relay binds `[::1]` on its port). The extra process cost about 20 ms per stack
    (MEASUREMENTS, "A stack with its own server"). App:
    `this_mac_runs_the_server_then_waits_for_it_to_list_the_worker`,
    `this_mac_asks_which_server_when_it_cannot_tell`,
    `the_sheet_installs_step_by_step_until_the_server_lists_the_worker`; deploy:
    `the_worker_registers_with_the_server_as_the_machine_reaches_it`; CLI:
    `worker_install_with_no_server_installs_one_beside_it`,
    `a_worker_name_resolves_through_the_server`; e2e: `the_first_run_offers_one_way_in`,
    `this_mac_walks_its_checklist`.
