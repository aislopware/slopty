# Decisions — Topology: one server, many workers, many clients

See `docs/DECISIONS.md` for the legend. Newest entries go at the end.

- ✅ **Three roles; the server is never on the data path** (2026-09-24, at the user's request
  for one app that reaches many machines and lets people and AI agents orchestrate them).
  - **Worker.** A machine that runs things: today's host (`slopty-hostd` + `slopty-ptyd`),
    which will be renamed `slopty-worker`. It owns PTYs, scrollback, windows, displays and agent
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

  These traits are cut when the worker is renamed, not before. Today's macOS code is the only
  implementation, and a trait is not written ahead of its second implementation beyond these
  four seams.

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
    `ListTerminals` plus one `AgentStatus` per terminal on an online worker, all in flight at
    once on the one stream.
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
    listing of added workers is gone; the server's directory replaces it.

- ✅ **The server's lease timings, state file and MCP endpoint** (2026-09-25).
  - **Lease.** Every server link runs a 1 s QUIC keep-alive and a 5 s idle timeout
    (`LEASE_KEEP_ALIVE` and `LEASE_IDLE_TIMEOUT` in `slopty-net`). QUIC negotiates the idle
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
  - **Joining.** `slopty-hostd` registers with the server named by `--server`, else
    `SLOPTY_SERVER`, else `[worker] server` in `settings.toml` (port 45560 when none is given).
    With none it runs on its own as before. The link is one task beside the rest of the
    daemon. It hears the daemon's events on its own broadcast subscription and forwards
    `SessionOpened`, `SessionClosed` and `Agent`. If it falls behind, it registers again,
    because a fresh registration carries the whole state. Every forwarded verb runs in a task
    of its own, so a long `WaitFor` holds up nothing. A dead or absent server costs the
    terminals and the clients nothing.
  - **Redial.** After a link ends the worker dials again after 250 ms, doubling to 5 s, forever.
    A link that lived 10 s resets the delay. A `DuplicateWorker` refusal (the server still
    holds the link a restart dropped) is retried every second. The lease's keep-alive is the
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
  - `ReadFile` refuses files over 16 MiB, since a reply is one frame. `WriteFile` writes a
    temporary file beside the target, fsyncs it and renames it over, keeping the old mode.
    `ListPorts` walks each terminal's process tree with libproc and reports the TCP sockets
    in the listening state.
