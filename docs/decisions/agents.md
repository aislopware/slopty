# Agents

How Slopty runs, shows and steers coding agents of every kind. The research, with sources, is in
`.research/gui-first-2026-10-01/`. `plan.md` there synthesises it, and `protocols.md`,
`desktops.md`, `orchestrators.md`, `workflows.md` and `design.md` hold the evidence.

- ✅ **GUI-first: the agent's own session is the source of truth** (2026-10-01, verified against
  Claude Code 2.1.286, Codex app-server at `ecc78e4`, pi 0.99.2 and ACP at `9e03215`). After a
  long time working in terminals, the person found that a terminal's productivity cannot match
  a GUI that is well designed, beautiful and complete. In agentic work the person mostly
  directs, watches and reviews agents rather than typing commands. So Slopty becomes GUI-first
  and agent-native. It keeps the best terminal there is, one action away. The GUI extends beyond
  Claude Code to Codex, pi and any ACP agent. The core goal stays: directing and working on
  many remote machines must feel like one local machine.
  - **What this supersedes.** The rule that the TUI is the only source of truth (CLAUDE.md
    until 2026-10-01, and the "What stays" half of "Claude Code gets a conversation face; the
    TUI stays the source of truth" in `claude-code.md`, 2026-09-27). Its principle is kept and
    widened: the agent's *own* session is the record, not the TUI's screen.
  - **The rules.**
    1. The native session is the record: Claude's session id and transcript, Codex's thread on
       its daemon, pi's session file. The worker's thread log is a cache rebuilt from it, never
       a second history.
    2. One writer per session, enforced by the worker.
    3. Slopty acts only through the agent's published doors: its protocol (Codex app-server,
       pi RPC, Claude's stream-json control protocol when driven, ACP), its hooks, or
       keystrokes into its TUI sent on the person's word behind the draft guard. It never reads
       the screen to control an agent, never types menu digits or cycles mode keys, and never
       answers for the person.
    4. The TUI can always take a session over: live beside the face for Codex, by a handoff at
       idle for a driven agent. A TUI-born agent gets the whole GUI.
    5. Slopty launches the user's own unmodified binary, never offers a login and never reads a
       credential. API keys and third-party providers are first-class.
  - **Each agent's drive.** An adapter declares a drive and an open set of capabilities, and the
    UI shows a control only where the capability is present.
    - *Claude Code, observed by default.* Its TUI runs in the PTY, and the face reads hooks, the
      transcript and the mod's live blocks. The worker owns the composer and the queue, and
      types on the person's word. Approvals go through the blocking `PermissionRequest` hook.
    - *Claude Code, driven, as an opt-in per profile* (`claude -p` stream-json with the control
      protocol). Recommended for API keys and third-party providers. Handoff to the TUI at idle.
    - *Codex, shared.* Slopty is a second live client of the user's app-server daemon socket,
      and `codex` in a PTY attaches to the same thread beside the face.
    - *pi, driven,* over `pi --mode rpc`, with Slopty's permission-gate extension, which fails
      closed, because pi has no permission system of its own. Handoff to its TUI at idle.
    - *Any ACP agent, driven,* through the `agent-client-protocol` crate.
    - *Any other program:* status only, through `slopty hook report`.
  - **Why Claude stays observed by default.** Driving gives a far richer face (token deltas,
    mode and model in place, `rewind_files`, `stop_task`). But Zed reports that Agent SDK use
    on subscription plans moved to a separate, limited credit pool, about 15–30× the cost for
    heavy use. A driven `claude -p` session is very likely billed the same way. Anthropic's
    legal page permits the unmodified binary with the user's own subscription, while its SDK
    overview forbids third-party claude.ai login, and no page settles driven `-p` use. The
    observed path is plainly allowed, already built and keeps the TUI live. Reopen this when
    Anthropic settles the question.
  - **Remote feels local.** Adapters, approval holds and turn snapshots live on the worker, so
    a dropped link pauses nothing. Each followed thread streams a snapshot, then actions
    numbered by epoch and sequence. A client resumes from where it left off with only the
    actions it missed. Sends and approvals carry intent ids, render at once, survive a
    reconnect and are deduplicated by the worker. The client caches each thread on disk, so it
    opens in the first frame. The model follows Codex's thread, turn and item primitives with
    the synchronisation semantics of Microsoft's Agent Host Protocol. Slopty takes AHP's
    model, not its wire: AHP is pre-1.0 and breaks with each minor release, its types are
    JSON-only and cannot travel on postcard, and its Rust crate is client-only.
  - **Turn snapshots.** At each turn edge the worker writes the whole working tree to a private
    git ref. Review, undo and rewind then cover every agent, including edits made through Bash,
    which Claude's own checkpoints miss.
  - **Order.** M1: one thread model with Claude GUI-first on it (the attention ladder, the
    thread view that survives drops, the worker composer and queue, turn snapshots and the
    review tile). M2: Codex, pi and the fleet overview. M3: projects the person directs from
    the board. M4: takeover and reach (git panel, moving an agent between hosts, best-of-N,
    generic ACP). The ranked list is `plan.md` §3.

- ✅ **The thread model: Codex's primitives with AHP's synchronisation, in Slopty's own types**
  (2026-10-01, `crates/slopty-proto/src/thread.rs`; the research is `plan.md` §2.2 and
  `protocols.md` §6.5, §7.2).
  - **Shape.** A thread (`ThreadId`, a UUIDv7 the worker mints) holds turns (`TurnId`, counted
    from 1, with `TurnId::BEFORE` for what precedes the first) and items (`ItemId`, the agent's
    own id where it is stable). Beside them it holds its open requests, the pending messages,
    the plan, background tasks, meters and the composer's commands. A subagent is a thread of
    its own, linked both ways: `ThreadMeta::parent` names the call, and `ToolCall::child` names
    the thread.
  - **Open where agents differ, closed where they share structure.** The agent (`AgentId`), its
    capabilities (`Cap`), its drive (`Drive`), a tool call's kind, a request's kind, a notice's
    kind, an origin, a wait's kind and the token kinds in `Usage` are all open strings, with the
    known values as constants. A new agent therefore brings its own values with no wire change.
    Three things are closed enums, because every adapter maps onto them and the UI ranks or
    places by them: `Phase` (the attention ladder's input), `ToolState` and `PartKey`.
    `ToolDetail` is keyed by kind (edit, write, read, search, exec, fetch, web search, agent,
    question, plan, tasks, mcp), never by one agent's tool names. An unknown kind has no detail,
    and an unknown native record is an `ItemBody::Extra`, kept and never dropped.
  - **One mutation, one reducer.** `Action` is the only way a thread changes.
    `ThreadState::apply` is pure and is the same code on the worker and on every client:
    - an action about a turn or item the state does not hold (one paged out) changes nothing;
    - `ItemCompleted` is authoritative;
    - `ItemUpdated` never moves a tool call back from a final state, and never back to
      streaming (`ToolState::may_become`);
    - `Append` counts lines and characters as if the text had come whole, so any chunking
      gives the same state. This is a property test, beside one that carries a state through
      the codec mid-stream and goes on.
  - **Requests carry the agent's own options.** Each `Choice` has the agent's id and label,
    an `Effect` (allow, deny or answer, so a client can tell yes from no without knowing the
    agent), the agent's scope words and whether it also stops the turn. Slopty never invents
    one. A settled request records who answered it (`Answerer`). The state keeps the open
    requests plus at most `RESOLVED_KEPT` (32) settled ones, so a client that comes back
    still sees who answered.
  - **Integers on the wire.** Cost is in millionths of a dollar and a rate window in
    hundredths of a percent, so the whole state is `Eq` and a golden or a property test
    compares it exactly.
  - **The wire.** `thread::wire` holds:
    - `ThreadRequest`: `Table`, `Follow { have, turns, max_latency_ms }`, `Page`, `Expand`,
      `Start`, `Intent`, `Approvals`;
    - `Intent`, each naming the capability it needs (`Intent::needs`);
    - `IntentDone` with an `Outcome`;
    - `ThreadFrame`: `Snapshot`, then `Actions { epoch, first }`, plus `Page` and `Expanded`;
    - `TableFrame` with `ThreadRow`, which carries the open requests as cards.

    The goldens are `tests/golden_thread.rs`. `ThreadState::window` and `page` cut a snapshot
    and its pages, and a property test shows that a window plus its pages rebuild the thread.
  - **Landed beside the old path, not over it.** `conversation.rs` and `agent.rs` stay until
    the UI lanes switch over. Until then the new types ride no `ClientMsg`, `WorkerMsg` or
    `UniHead` variant. The switch-over adds `ClientMsg::Thread`, `WorkerMsg::ThreadTable` and
    `WorkerMsg::IntentDone`, and `UniHead::Thread`, and deletes the old types in the same
    change.

- ✅ **The thread host keeps a log per thread, and a follower resumes from its cursor**
  (2026-10-01, `crates/slopty-worker/src/thread/`; tests in `crates/slopty-worker/tests/threads.rs`).
  - **The log.** `<data dir>/threads/<id>/` holds two files:
    - `snapshot`: the state and its cursor;
    - `tail`: a head naming the cursor it continues from, then each action framed as the
      wire frames it.

    The last 4096 actions (at most 4 MiB) are also kept in memory, which is what a returning
    follower is sent. A turn's end writes a new snapshot and starts the tail file over, once the
    tail file holds 1024 actions. The in-memory tail survives that, so compaction costs no
    follower a snapshot.
  - **A cache, so nothing is synced.** The agent's own session is the record, so the log is
    never fsynced. A torn last action is cut off on reading. A tail whose head does not match
    the snapshot is dropped: it is either from another epoch or the tail a compaction was about
    to replace. `Log::reset` rebuilds the log from the native session under the next epoch.
    A new log's first epoch is random, so a log lost and made again never meets an old cursor
    by chance.
  - **Follow.** A cursor of the same epoch inside the kept tail gets only the actions after it.
    Anything else gets a snapshot of the last turns, and older turns come by page. The host
    takes a follower's catch-up and subscribes it to the thread's feed under one lock, so
    nothing is missed or sent twice.
  - **Coalescing.** A follower holds batches for its `max_latency_ms`, and sends early once
    4 KiB of text or 512 actions have gathered. Runs of appends to one part are merged.
    `ThreadFrame::Actions` therefore carries `first` and `next`, and may hold fewer actions
    than `next - first`: the reducer gives the same state either way, which W0's chunking
    property proves. With no budget, each batch goes alone. A follower that lags the feed,
    or meets a batch that does not follow on, catches up from its cursor, by the tail or by a
    snapshot.
  - **The table** is in memory and rebuilt from the logs at start, under a new random epoch,
    so a client's cursor from an earlier run gets every row. It sends a delta while it still
    remembers every removal since the cursor (the last 1024 of them). A row that differs only
    in `updated_ms` changes nothing.
  - **Intents.** Each thread keeps the outcome of its last 512 intent ids. They are appended to
    `intents` beside the log and rewritten when that file reaches twice as many. A repeated id
    therefore gets the first outcome, across a worker restart too. Intents that start a
    thread are kept in `threads/starts`. A check, its action and its record happen under one
    lock.
