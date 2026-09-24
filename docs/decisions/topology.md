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
