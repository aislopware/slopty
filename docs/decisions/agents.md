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
    3. Slopty acts only through the agent's published doors: its protocol (Claude's stream-json
       control protocol, Codex app-server, pi RPC, ACP). It never reads the screen to control
       an agent, never types into an agent's terminal, and never answers for the person.
    4. The TUI can always take a session over by resuming the session the agent itself keeps,
       and a session the TUI began is taken up from its own record.
    5. Slopty launches the user's own unmodified binary, never offers a login and never reads a
       credential. API keys and third-party providers are first-class.
  - **Each agent's drive.** An adapter declares a drive and an open set of capabilities, and the
    UI shows a control only where the capability is present. Every agent is driven: Slopty is
    the session's one writer, and no agent's TUI runs in a PTY for Slopty to follow.
    - *Claude Code, driven* (2026-10-16), as Claude's own desktop app drives it:
      `claude -p --output-format stream-json --verbose --input-format stream-json
      --permission-prompt-tool=stdio --replay-user-messages`, with `--session-id` for a new
      thread and `--resume` to go on, and the environment the desktop app sets
      (`CLAUDE_CODE_EAGER_FLUSH=1`, `MCP_CONNECTION_NONBLOCKING=true`,
      `CLAUDE_CODE_ENABLE_ASK_USER_QUESTION_TOOL=true`) with `PATH` from the login shell. A
      managed launcher in `claude`'s place gets the same arguments and hands them on. The codec
      is `slopty_agent::claude::driven`, the IO `slopty_worker::thread::claude`:
      - *delivery* is acknowledged by the message's own uuid coming back (the echo, or Claude
        Code saying it queued or began it); one not acknowledged in 120 s goes again under the
        same uuid, which Claude Code takes once, and after that, or when Claude Code ends first,
        it is held as one that may not have reached it;
      - *a stop* is the `interrupt` control request; one not acknowledged in 1.5 s ends the
        process, the turn is cut, and the next message takes the session up again. A subagent
        is stopped with `stop_task`;
      - *approvals* are `can_use_tool` requests, answered by their own request id; an
        `initialize` answer names those still open, so a worker that comes back asks them again
        and prompts are never lost;
      - *settings apply live*: `set_model`, `set_permission_mode`, effort and fast mode
        (`Model::options`, `Start::options`, `Intent::SetOption`); slash commands and skills go
        as user messages; `get_context_usage` reads the context;
      - *waits*: compaction, the plan's usage limit (messages are held and go once it resets),
        a stop under way (`stopping`, then `stopped`), and a turn that shows no progress for 16
        minutes, which a machine waking from sleep does not trip;
      - *one writer*: every ask of a thread goes through its one task, intents carry ids, and
        the same `ThreadState` reducer holds the thread on the worker and every client. A
        thread whose Claude Code ended is exited and resumable: its next message starts Claude
        Code again with `--resume`, the thread read again from the transcript first.
    - *Codex, shared.* Slopty is a live client of the user's app-server daemon socket; no
      `codex` runs in a PTY for Slopty.
    - *pi, driven,* over `pi --mode rpc`, with Slopty's permission-gate extension, which fails
      closed, because pi has no permission system of its own.
    - *Any ACP agent, driven,* over its stdio with the protocol's own types (see "Any ACP agent
      is driven by one adapter" below).
    - *A lease.* While an agent's process lives, the worker stamps its thread alive
      (`Action::Lease`) at least every 15 s (every 10 s, `LEASE_EVERY`), for every drive alike,
      and stops the moment the process is gone, before the end is told. A client shows a turn
      as running only while the stamp is fresh.
  - **Why Claude is driven.** Observing Claude Code (its TUI in a PTY, read through hooks, the
    transcript and a mod's live blocks, typed into on the person's word) kept the terminal live
    but left the face guessing: a newest-file guess for which transcript was the session, a
    five-second inference of which dialog was up, keystrokes timed against a draft guard, and a
    mod that a managed install could switch off. The person directs and reviews agents and
    wants every surface exact. Driving gives the face what Claude Code itself knows, with ids
    for every message and approval, and is how Anthropic's own desktop app runs it, so a driven
    session is a supported use of the person's own binary. The observed lane, its hooks, its
    mod and its keystroke paths were deleted with this ruling, not kept beside it.
  - **Custody: a turn survives a worker restart.** Claude Code runs in ptyd's keeping as a piped
    session (no terminal): ptyd holds its standard input and output, keeps what it writes while
    no worker holds it, and hands the session back when a worker asks. A worker that goes, by a
    crash, an update or a kill, leaves Claude Code running mid-turn; the next one takes the
    session up (`claude-custody` names it on the thread), reads the thread again from the
    transcript, then what Claude Code wrote meanwhile, and sends `initialize` again, whose
    answer reopens any approval still waiting. A ptyd that itself handed over to a new build
    keeps the session; one that started afresh is handed it back from the worker's copy. Only
    when ptyd no longer holds the process is the thread left exited and resumable, the cut turn
    marked. The proof is the e2e `a_turn_survives_the_worker_restarting_mid_turn`
    (`apps/slopty-worker/tests/threads.rs`) and
    `a_permission_outstanding_survives_the_adapter_going`
    (`crates/slopty-worker/tests/claude.rs`).
  - **The stand-in.** `slopty-stub-claude` speaks the same stream-json and control protocol
    (`slopty_testkit::claude_stream`) and fails on cue: a crash mid-turn, no echo, a torn line,
    no answer to `initialize`, an exit at start, a slow stop, a usage limit, a held subagent.
    `crates/slopty-worker/tests/claude.rs` proves one behaviour per test against it and a real
    ptyd; tests never start a signed-in Claude Code.
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
      `Start`, `Intent`. Whether a client answers requests is said once, by
      `ConversationRequest::Approvals`, for every agent; the thread wire's own copy of it was
      removed on 2026-10-04;
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
    - **What it gathers lives in the follower, not in the call** (2026-10-12). The stream
      waits on the follower beside its commands, and a review's or a page's answer coming
      cuts that wait short. The batches taken for the frame were the call's own, so a cut
      dropped them; the cursor stayed, but nothing caught up until the thread changed again.
      A prompt answered just as a client came back and asked for a review stayed open on that
      client for good (the cut-link test, 3 of 9 first runs on a Mac). The follower now keeps
      the batches and the frame's deadline until the frame goes. Test:
      `a_follower_cut_short_while_it_gathers_loses_nothing`.
  - **A log opens on first use** (2026-10-11, readiness-w 10-11 rank 7). A start used to open
    and replay every log the worker ever kept: 431 ms for 2,000 threads of 20 turns, every state
    held in memory after (`docs/MEASUREMENTS.md`). Each thread now also keeps `head`: its row
    and its state without its items, written whenever it rests (any apply while its phase is
    not working), naming the tail it was written over by the tail's cursor and length. A start
    reads the heads alone, puts their rows in the table, and opens a thread's log the first
    time anything asks for it. A look over every thread (`Host::visit`) sees an unopened one
    by its head; a search reads the items off the disk without opening the log
    (`Host::visit_whole`). A head missing or behind its log (the worker stopped mid-turn, or
    crashed) makes that one thread open at the start, and its head is written then.
    - Kept, not pruned. A worker's log may be the last copy of a session the agent has since
      cleaned up (Claude Code's `cleanupPeriodDays` is 30 by default), so nothing deletes one
      unasked.
    - Test: `a_start_reads_the_heads_and_opens_a_log_on_first_use`; the measurement
      `host_open_cost`.
    - **And goes back to its head once it rests** (2026-10-12, a week on the agent side).
      Nothing closed a log once opened, so a worker up for a week held every thread opened
      since its start, each its whole state and its tail: 144 KiB for 20 turns of 2 KB
      answers, megabytes for a long conversation (`docs/MEASUREMENTS.md`). The daemon looks
      each minute (`Host::rest`): a thread nobody follows, unused for 10 min, that holds
      nothing the worker alone knows (a message typed and not yet seen, a start's first
      message, a scheduled one, a snapshot waiting for its turn), has its head written and
      its log closed. Its next use opens it again as at the start: the same state, the same
      cursor, so a follower from before is sent only what it missed. Test:
      `a_thread_at_rest_goes_back_to_its_head_and_comes_back_the_same` (`tests/rest.rs`).
  - **The table** is in memory and rebuilt from the logs' heads at start, under a new random epoch,
    so a client's cursor from an earlier run gets every row. It sends a delta while it still
    remembers every removal since the cursor (the last 1024 of them). A row that differs only
    in `updated_ms` changes nothing.
  - **Intents.** Each thread keeps the outcome of its last 512 intent ids. They are appended to
    `intents` beside the log and rewritten when that file reaches twice as many. A repeated id
    therefore gets the first outcome, across a worker restart too. Intents that start a
    thread are kept in `threads/starts`. A check, its action and its record happen under one
    lock.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **Claude Code observed is an adapter: a codec in `slopty-agent`, its IO on the worker**
  (2026-10-02, `crates/slopty-agent/src/observed.rs`, `crates/slopty-worker/src/thread/claude.rs`;
  tests in `observed/tests.rs` over the recorded fixtures, and `crates/slopty-worker/tests/claude_threads.rs`).
  - **Beside today's path, sharing its inputs.** The driver hears what every client hears: the
    tracker's merged status (`WorkerMsg::Agent`), the held prompts (`WorkerMsg::Permission`),
    and the session summaries for the working directory. It also gets the board each followed
    session already has, with its hooks heard, meters, subagent files and the mod's blocks.
    It reads the transcripts itself on the blocking pool, after each hook and on the kernel's
    word that one was written (since 2026-10-11, "An observed Claude Code is read when its
    transcript is written"), with the same decoder the face uses. Nothing of the old path
    changes. The daemon starts it (`apps/slopty-worker/src/threads.rs`), with the host under
    `<data dir>/threads`.
  - **A thread per Claude Code session, named by the session's own id.** The id is derived
    from the session id (and for a subagent, from that and its agent id). So the same session
    is the same thread across a worker restart, and across a `claude --resume` in another
    terminal. The thread begins once the session id is known, from a hook or from the
    transcript's file name (before that, a provisional thread stands for it; see the entry on
    what a row carries). A thread held from before starts over under a new epoch and is
    read again, as the ruling has it: the log is a cache of the native session. When a
    terminal moves to another session (`/clear`, `/resume`), every other thread on it is left
    exited and resumable, with its turn under way stopped. So is a session whose agent quits,
    and, as a worker starts, every thread an earlier worker left running; the ones whose agent
    still runs begin again from their terminal. Since 2026-10-15: a thread from before a
    restart, bound by an older worker to a session its terminal did not run, said "working"
    for a day while the terminal ran another session, and its TUI button opened that other
    session. The GUI opens an agent's terminal, and the worker types into it, only while the
    thread's agent runs there. Otherwise the session is taken up again in a terminal of its own.
    The same day: a terminal that closes ends its threads too. A start's first message goes
    on Claude Code's command line, not through ptyd, so one an earlier worker left "being sent"
    is taken off the list when the transcript shows it, else held saying it may have gone,
    rather than sitting there for good. "Trust this folder" no longer needs the approvals a
    hook declares, since Claude Code held at its trust dialog has spoken no hook. The status
    line's transcript path is kept, so an agent idle since the worker started is read without
    waiting for its next hook, and transcripts are looked for under `CLAUDE_CONFIG_DIR` as the
    registry already was.
  - **The mapping.**
    - A prompt opens the next turn of its thread, and later entries belong to that turn. A
      branch (a rewind or an edited prompt) truncates the thread back to before the prompt it
      dropped.
    - The transcript's turn record fills in the models, tokens and end. A turn with an
      interrupt in it ends `Interrupted`.
    - Edits and writes add up to the turn's +N −M.
    - Tool kinds come from the decoder's typed detail, and titles are worded here.
    - A subagent's thread links to the call that started it, and the call names the thread.
  - **The mod's blocks are items.** The adapter keeps an `Overlay` as one more follower, so it
    shares the settling rules the face has (`live:<turn>:<step>:<block>`). A text or thinking
    block is a provisional item, appended to as it grows and removed once the transcript
    settles it. A tool block takes the call's own id, so the transcript's entry replaces it.
  - **Prompts are requests with Claude Code's own answers:** allow, always allow (worded with
    what the suggestions grant), deny, and deny and stop. A question carries its questions, and
    a plan is approve or deny. `observed::verdict` maps a choice back to the `Verdict` the
    relay prints. The status, meters and open requests are told again whenever the main thread
    begins anew, since the transcript has none of them.
  - **Not carried yet:** the agent's version (the mod's hello has it), and a session's working
    directory until its shell next reports one after the worker starts.

- ✅ **Threads ride the link beside the conversation path, which keeps working unchanged**
  (2026-10-02, `apps/slopty-worker/src/threads.rs`, `crates/slopty-net/src/streams.rs`;
  goldens in `crates/slopty-proto/tests/golden_thread.rs`, proved end to end by
  `apps/slopty-worker/tests/threads.rs`).
  - **Additive on the wire.** `ClientMsg::Thread` carries every `ThreadRequest`.
    `WorkerMsg::Threads` carries the table and `WorkerMsg::IntentDone` an intent's outcome,
    both on the control stream. A followed thread gets a unidirectional stream of its own
    (`UniHead::Thread`, at the conversation streams' priority) carrying `ThreadFrame`s. Each
    variant is appended last, so no existing golden changed.
  - **One stream per followed thread, as the conversation has.** Its task sends what the
    `Follower` makes from the client's cursor, and answers `Page` and `Expand` between frames
    on the same stream. A follow of a thread already followed is ignored. Unfollowing ends the
    task, and the stream finishes. The table is one task per connection, replaced by the next
    `Table` ask: what the client lacks from its cursor, then a frame at each change.
  - **Following a thread holds its terminal's prompts, as following its conversation does.**
    The connection joins the session's followers, so a `PermissionRequest` waits for it. It
    leaves when neither path follows that session any more.
  - **Intents act under the host's lock, once per id.** `Answer` and `Release` go through the
    held prompts the conversation path answers. A repeat, after a reconnect or from another
    client, gets the first outcome back and touches nothing. An intent the thread's caps lack
    is `Unsupported`, and so is any no adapter acts on yet (mode, stopping a task).
    Messages, interrupts and models go to the composer (next entry). `Start` is refused and
    not recorded.
  - **Expansion asks the adapter.** The Claude driver answers an `Expand` from the session's
    own transcripts, on the blocking pool: a text cut at `EXPANDED_CHARS`, a picture's bytes,
    or `Gone`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **The worker types what is sent to an observed agent, under the draft guard**
  (2026-10-02, `crates/slopty-worker/src/thread/compose.rs`; tests in
  `crates/slopty-worker/tests/compose.rs` over a real terminal whose program records every
  byte, and `apps/slopty-worker/tests/threads.rs` over the link).
  - **The same keystrokes the face typed from the client.** A message is a paste (bracketed
    where the TUI asked), 200 ms, then Enter. A model is `/model <id>` typed, the pause, then
    Enter. An interrupt is Esc. The words and their Enter are one delivery ptyd types whole
    (`orchestrate::deliver_entry`, the path a spawned agent's brief takes too; 2026-10-12,
    `docs/decisions/projects.md`): the worker keeps no pause of its own between them. All of it goes through orchestration's guard (`may_type`) at
    the moment it is written. Nothing is typed while the agent asks a person, before its hooks
    have spoken, once it has exited, or over a person's unsent line. Nothing it types clears
    that line.
  - **A message waits in the thread's pending list until it goes**, where a client shows it,
    edits it or withdraws it. Its entry leaves once the Enter is written. The item the agent
    makes of it carries the intent (`UserMessage::intent`): the host marks the first person's
    item whose words are the typed ones, so a client confirms its own bubble by id. A
    message's delivery is named for its intent: one being sent when the worker restarts waits
    to go again, and ptyd types it only if it never took it.
    - A steer goes as soon as the guard lets it, into the turn under way.
    - A queued message waits until the agent is at rest by its hooks, and then goes, one per
      turn. The next waits until the agent has taken the last one: it was seen working since,
      or the turn it started has ended. A queued message is never folded into the turn under
      way.
    - One the guard holds is `Held` with why, "Your draft in the terminal is in the way" for a
      person's line, and goes once the way is clear (tried again every 250 ms, since a draft
      says nothing when it is sent).
    - What a person types while a message is being typed waits, in order, until its Enter
      is typed (the session holds input behind an entry): it starts their next line, never
      joins the message. The state "typed and not sent" is deleted with the pause it
      described.
  - **An interrupt and a model are checked before they are taken.** Esc is refused over a
    draft, while a person is asked, and when the agent is not working. A model must be in the
    thread's catalogue (`ThreadMeta::models`). For Claude Code the catalogue is the aliases
    `/model` takes, as the mod lists them (`observed::MODELS` where it is not heard), so no
    client keeps a list of its own. `/model` waits
    for the agent to be at rest. Mode stays read-only.
  - **Each thread with something to send has one task**, which sends one thing at a time, so
    two messages never interleave in the terminal. It wakes on the thread's batches, on a new
    intent, and every 250 ms while something waits on the guard or the agent.

- ✅ **Every turn is snapshotted into private refs, and a review is a diff between two
  snapshots** (2026-10-02, `crates/slopty-worker/src/repo/snapshot.rs`,
  `crates/slopty-worker/src/thread/review.rs`; tests in `crates/slopty-worker/tests/review.rs`
  on a real repository, cost in `docs/MEASUREMENTS.md`, "a turn snapshot of this
  repository").
  - **A snapshot is the working tree as `git add -A` sees it**, written as a tree through an
    index of the thread's own, so the person's index, `HEAD` and stash are never touched.
    Untracked files are in and ignored ones out, so a Bash edit is caught as well as a tool's.
    The index is kept between snapshots, so each hashes only what changed since the last, and
    only the first starts from `HEAD`. A commit for each tree keeps it alive under
    `refs/slopty/threads/<thread>/<turn>-{before,after}`. The refs of a turn 256 turns back go.
  - **Taken at the edges, off the edge's path.** The host tells of a turn beginning or ending
    as the thread's last turn: a log read again from the start tells of old turns, and the tree
    is long past them. A turn's start is snapshotted at the earliest word of it, the agent
    starting to work, since the observed Claude path sees the turn itself only once the
    transcript has it. The snapshot follows as `Action::Snapshot`, one thread at a time and in
    order. A thread outside git takes none, and its review says why.
  - **A review compares two snapshots, "now" being one taken for it**: a turn (start to end,
    or to now while it runs), since a turn's start, or what is left after what the person kept.
    It is asked with `ThreadRequest::Review` and answered on the thread's stream as
    `ThreadFrame::Review`, from a task of its own so the thread's frames go on meanwhile. Each
    file carries its blob ids on both sides. Hunks are cut on the worker with three lines of
    context, the same way every time, so a hunk named by its place is the one the review
    showed. A review carries at most 20 000 diff lines, spent in the order the files are read;
    a file past what is left comes with its counts and no hunks, and is read whole on the
    person's press (`docs/decisions/ui.md`, "A big review shows every file it leaves out").
  - **Keep and revert act once per id, checked against what the review showed.** A revert
    writes the file's old side back, whole or by hunk, only while the file is still the blob
    the review showed. Keeping moves the file, whole or by hunk, into a kept tree
    (`refs/slopty/threads/<thread>/kept`). When what is kept of it is no longer the review's
    old side, the change is merged onto it three ways and refused only where the two meet
    (below, "Keep takes a turn's change onto what is kept"). "What is left to review" is the kept tree against now, starting from the
    thread's first snapshot. Both run git, so they are answered from a task of their own, and a
    repeat that comes while one is under way gets nothing until it is done.
  - **Every adapter advertises `Cap::SNAPSHOTS`** (corrected 2026-10-04). The worker takes the
    snapshots, not the agent, so keep and revert are the worker's door for every thread whose
    folder is in git: observed Claude Code, Codex, pi and every ACP agent. Only observed Claude
    Code had the cap before, so keep and revert were refused on the others for no reason. Test:
    `a_pi_thread_puts_a_change_back_through_the_workers_snapshots`.
  - **Every variant is appended last** (`ThreadRequest::Review`, `ThreadFrame::Review`,
    `Intent::Keep`, `Intent::Revert`), so no existing golden changed for them. The model
    catalogue (`ThreadMeta::models`, the composer's entry) did change the snapshot goldens.

- ✅ **What the worker adds to a thread outlives the thread being read again** (2026-10-02,
  `crates/slopty-worker/src/thread/host.rs`; `what_the_worker_owns_outlives_a_read_again` in
  `crates/slopty-worker/tests/threads.rs`). An observed thread is read again from its
  transcript after a worker restart, under a new epoch, and the transcript has none of what
  the worker added. The host keeps it beside the log and puts it back as the adapter tells
  the thread again: the intent each person's item came from, each turn's snapshots, and the
  pending messages. A message that was being typed when the worker went down may be in the
  terminal already, so it comes back held as typed and not sent, and is never typed again.
  The composer goes on with every thread's waiting messages when the daemon starts.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **An observed thread is named by the session until its first prompt names it**
  (2026-10-02, `crates/slopty-agent/src/observed.rs`, `observed::Observed::title`). Claude
  Code paints the session's own name in the terminal title (`✳ Fix the flaky test`), so a
  thread started from a shell is named at once, and renamed as that name changes. The bare
  `Claude Code` names nothing. The first prompt that is not a command then names it, and
  keeps it.

- ✅ **A row carries what the client read from the old agent status** (2026-10-02,
  `ThreadState::row`, `observed::Observed`,
  `crates/slopty-worker/src/thread/claude.rs`; tests `a_row_says_what_runs_now`,
  `approvals_come_with_the_hooks`, and
  `a_claude_code_before_its_id_has_its_terminals_thread_until_the_id_comes` in
  `claude_threads.rs`). The client's reads of `AgentEvent`, `SessionAgent` and the permission
  prompts were listed, so that each could go to the thread table before the old path goes.
  Three had nothing to read from, and each is closed by one field or one adapter behaviour:
  - **What it is doing now.** `ThreadRow.doing` is the title of the newest tool call that runs
    or waits on a request ("Edit src/main.rs"), worked out by the reducer. Every adapter gets
    it with no work of its own, since the title is already its wording.
  - **Whether the hooks have spoken.** No new field. Claude Code's prompts are held only
    through the `PermissionRequest` hook, so the observed thread declares `approvals` once a
    hook is heard in its terminal (a status from a hook, a held prompt, or a hook the session's
    board counted). A Claude Code row without `approvals` is the one that offers to install
    the hooks. That follows the rule that a client shows a control only where the capability
    is.
  - **A Claude Code before its session id.** No new field. One started by hand and idle at its
    prompt has neither hook nor transcript yet, so the tracker sees it but nothing names a
    session. Its terminal names a provisional thread (`observed::terminal_thread`, an empty
    `native`). When the id comes, the session's own thread takes over and the provisional one
    is removed, not left exited, since it never was a session. It is removed too when the
    agent goes or the terminal closes first. Every agent tile so has a row.
  - **A first hook that asks.** A Claude Code whose first word is a `PermissionRequest` (no
    session start before it, its transcript still empty) had no thread to carry the prompt, so
    the person saw an empty tile and no card. The held prompt now opens the thread itself: the
    session's own when the hook names the id, else the terminal's provisional one. Every
    prompt held and not yet settled is told again to whichever thread begins after it, so the
    card survives the provisional thread giving way to the session's (test
    `a_first_hook_that_asks_opens_a_thread_with_its_request` in
    `apps/slopty-worker/tests/threads.rs`).
  - **One session id in two terminals.** A `--resume` of a session still running elsewhere
    puts two live Claude Codes on one id. The first terminal to claim it keeps the session's
    thread until its agent goes. The other observes a thread of its own
    (`observed::thread_in`), so no thread moves between terminals and each prompt lands on the
    terminal that asked it (test `two_terminals_on_one_session_id_keep_a_thread_each`).

- ✅ **The server ranks every thread on one ladder, and a notice goes where the person is**
  (2026-10-02, `crates/slopty-proto/src/thread/attention.rs`,
  `crates/slopty-server/src/hub/ladder.rs`; the tests in `hub/ladder/tests.rs`,
  `crates/slopty-server/tests/ladder.rs`, and `the_server_hears_the_workers_threads` in
  `apps/slopty-worker/tests/server_link.rs`).
  - **One rank from the row alone.** `Rung::of(&ThreadRow)` gives Needs you (a request open,
    or the phase says so), Failed, To review, Working, Waiting or Idle. The server and every
    client rank alike, and the words are the same on every surface.
  - **To review is the tree, not the agent's word.** After each turn's end snapshot, and after
    each keep or revert, the worker compares the tree with what the person kept, or with the
    thread's first snapshot when they kept nothing (`Action::ToReview`). That holds for every
    agent and every edit, a Bash edit included. Keeping every file clears it, and putting back
    something kept raises it again.
  - **The server ranks, because it holds every worker.** Each worker publishes its table
    (`ToServer::Threads`, the whole table on every registration, then each change). A subagent
    folds into the thread it hangs from, and never stands or notifies on its own. A subagent
    that needs the person lifts its parent at once. One that failed reads as working while
    anyone in its family still works or waits, since the parent may carry on without it, and
    lifts the parent to failed only once the whole family is at rest. Otherwise the parent
    stands as high as its highest subagent. The rest roll up per tile, worker,
    project node (a subtask into its task), project and fleet. Each roll-up counts every rung
    and names the thread to go to first, on the highest rung and there longest. The server
    ranks again on every row, terminal or project change, and sends the ladder only when it
    moved, whole each time, since rungs move far more rarely than rows.
  - **A workspace is the client's own**, laid out on the device alone, so the client folds the
    tiles it holds with `Ladder::over`. The fold is the same one the server uses.
  - **Presence routes the notices.** A person's client (`Role::Client`, never an agent's link)
    says where they are: `Seat::Desk` or `Seat::Handheld`, whether they are at it now, the
    workspace in front and the tiles on screen. A thread that hangs from no other and climbs
    to needing the person, fails, or comes to rest from working is a notice. Notices go:
    - to no client while its tile is on screen where the person is;
    - to the desks they are at, and never to a handheld beside them;
    - to the handhelds they hold when they are at no desk;
    - to every client when they are at none.

    A thread first seen at a rung (a worker that comes back) is no news. A finished notice
    carries how long the thread worked, from its first busy rung to its rest, by the worker's
    clock. The client shows it only when that is over its person's slow-command time. A
    notice a subagent raised names it (`Notice::via`). Every client hears who is where
    (`FromServer::Present`), and finds itself there by the number its `Welcome` gave its link.
    A client says where the person is through `ServerCaller::presence`, which sends nothing
    for the same again and tells every new link at once.
  - **The server's notice is the only trigger for an OS notification or an alert.** Once
    notices flow, a client shows only what it is handed and never decides on its own from the
    ladder or a status, so two devices never both alert for one thing.

- ✅ **A client mirrors each open thread, holds its intents in an outbox, and keeps both on
  disk** (2026-10-02, `crates/slopty-client/src/threads.rs`; the unit tests beside it and
  `a_thread_and_an_answer_outlive_a_dropped_link_and_a_relaunch` in
  `crates/slopty-client/tests/threads.rs`, against the real worker).
  - **The mirror runs the worker's reducer.** A snapshot replaces it; actions apply only when
    they start at its cursor. A frame that does not, or a stream that went with a link, is
    followed again from the cursor, with an unfollow first, since the worker ignores a follow
    of a thread it already streams. Each item keeps a revision, so a view measures again only
    the rows whose items moved.
  - **An intent shows until the thread's own record does, not until the worker answers.** A
    send is drawn as a bubble until the worker holds it as pending or the agent's message
    carries its id; an answer flips its card until the request is no longer open; a stop reads
    "Stopping" until no turn is active. Settling on the worker's answer alone would show the
    old state for the moment between the answer and the record. A send the worker turned down
    stays, with its words and why, until the person dismisses it.
  - **Unanswered intents go again under their first id** after a reconnect and after a
    relaunch, and the worker acts on an id once. Intents for a thread not open here settle by
    its table row.
  - **The cache is one directory per worker, the user's alone**: each thread's state with its
    cursor and the outbox, in postcard, replaced whole, at most the 64 threads last written. A
    cached thread draws in the view's first frame, read on the UI thread (0.5 ms for a 1.8 MB
    snapshot of 20 turns, `docs/MEASUREMENTS.md`), and catches up from its cursor. Every
    write runs off the UI thread, one batch after another, a thread two seconds after it
    rests.
  - **Expanded content is held by recency under 64 MiB**, and asked for once while it is on
    its way.

- ✅ **The thread view draws any agent's thread from the mirror, and an agent tile opens on
  it** (2026-10-02, `crates/slopty-ui/src/conversation/thread/`; the tests in
  `conversation/thread/tests.rs` and `an_agent_with_a_thread_opens_on_its_thread_view` in
  `workspace/tests/faces.rs`).
  - One reading column of 680 pt, prose at 15/1.6. A settled turn folds to its message, one
    line ("Worked 55 s: ran a command, read a file, 2 more") and its answer; the live turn
    never folds. The header says the title, the agent, the state word, the worker and, past
    20 %, the context ring.
  - The activity bar over the composer stacks the requests ("2 of 5", only a press answers
    one), the plan, the files the last turn edited with the way to their review, the messages
    waiting (taken back from here where the agent can queue) and the background work (stopped
    from here where the agent can). ↵ sends into the turn, ⌘↵ queues.
  - It stands beside the conversation face until that path goes: a tile whose terminal the
    worker's thread table names opens on its face on every device and draws the thread view;
    any other agent tile keeps the conversation face.

- ✅ **The review tile reads the worker's review frames, and every keep, revert and comment is
  an intent** (2026-10-02, `crates/slopty-ui/src/review/`; `review/tests.rs`). The scopes are
  the ones the worker answers today: the last turn (the default), since reviewed, and every
  turn; a branch, the unstaged changes and a task's span wait on the worker. Files go by
  weight, with tests, fixtures, snapshots, locks and generated code listed after the rest.
  "Mark reviewed" keeps every file shown, so "Since reviewed" is what changed after. Line
  comments are anchored by their line's text, dropped when it changes, and go as one message
  of `path L<n>: body` lines. A keep or revert the worker acted on asks for the review again.

- ✅ **"Review with <agent>" runs the agent's own review over the change on show, and its
  findings become the tile's comments** (2026-10-04, frontier must-have M1; replaces the
  projects' reviewer stage). The research behind it is in
  `.research/feature-prune-frontier-2026-10-04.md` §6, with each agent's door checked against
  its own docs and source.
  - **The doors.**
    - Claude Code: its built-in `/code-review` skill. `/review` is now its alias, and the
      `code-review` plugin reviews GitHub pull requests only. It takes a ref range, so the
      worker writes the change the tile shows as two private commits under
      `refs/slopty/threads/<t>/review-{base,head}`: the old tree, then the new tree on it.
      `base...head` is then exactly that change. The command goes as the person's turn,
      through the composer like any command of theirs, only on their press.
    - Codex: `review/start` with target `commit` (the head commit) and delivery `inline`,
      Codex's own reviewer. Detached delivery is deprecated. Codex's `exitedReviewMode` text
      is now the agent's answer in the thread, where it used to be dropped.
    - pi publishes no review command and ACP no review method, so neither shows the action.
  - **The door is a capability.** `Cap::REVIEW` and `Intent::Review { from, to }` (two
    snapshot trees, as data). Codex always has it. Claude Code has it exactly while its own
    command list names `code-review` (`observed::REVIEW_COMMAND`). The button and the palette
    line show only where it is there.
  - **The private refs stay home.** Each thread keeps one pair, overwritten by each review,
    and an aside closed for good takes every ref of its thread with it (`Repo::forget`). A push
    names the person's branches, never `refs/slopty`, and no review commit is an ancestor of
    a branch. A test pushes `main` to a bare remote and finds neither the refs nor the commit.
  - **The findings.** While the agent reviews, the tile holds the change it asked about,
    though the thread moves on. Once the agent rests, with nothing pending and no background
    work, it reads every answer written after the request (`review::findings`).
    - The reading is tolerant: Codex's `- title — path:start-end` lines, and Claude Code's
      prose naming `path:12`, `path:12-18`, `path#L12-L18` or "`path` line 12".
    - A finding on a line in the diff becomes a comment under it, with the agent's mark and
      its title.
    - A finding with no line on show, or no place at all, is a note above the diff, never
      dropped.
    - The person lets any of them go, and sends the rest with their own comments as one
      message, or adds them to the draft.
    - A review that raised nothing says what the agent said of it. A refused one says why, in
      the error's tone.
  - Tests:
    - `codexs_findings_are_read_with_their_places_and_bodies` and the other cases in
      `review/findings.rs`.
    - `the_agents_findings_become_comments_and_notes_sent_as_one` and
      `findings_are_let_go_and_a_review_says_how_it_came_out` (`review/tests.rs`).
    - `an_agents_review_names_the_change_as_one_pair_that_never_leaves`
      (`slopty-worker/tests/review.rs`).
    - `a_review_by_claude_code_is_its_own_code_review_over_the_range`
      (`slopty-workerd/tests/threads.rs`).
    - `a_thread_reviews_while_claude_code_lists_code_review` (`slopty-agent`).
    - `a_review_asks_codexs_reviewer_and_its_findings_are_its_answer`
      (`slopty-agent/tests/codex.rs`).
    - `a_review_goes_to_codexs_reviewer_over_the_head_commit`
      (`slopty-worker/tests/codex.rs`).
    - The e2e `the_agents_own_review_puts_its_findings_on_the_diff`, with goldens
      `review-agent` and `review-agent-dark`.

- ✅ **The screen an agent drives opens beside its thread, watched until the person takes
  control** (2026-10-04, frontier must-have M2). The research behind it is in
  `.research/feature-prune-frontier-2026-10-04.md` §6.
  - **The link is the agent's own calls.** Every adapter already maps its agent's tool calls
    into the thread model, an MCP tool as its server and tool with its input, a command as its
    text. The host tells each call that runs in a thread's last turn (`Host::tools`), and
    `thread::screens` reads what it says of a screen. One worker module serves every agent,
    so no adapter grows a screen of its own. Nothing reads a screen to learn this, and nothing
    acts on one.
    - Computer use names its window outright. Claude Code's built-in `computer-use` server
      passes `window_id` to its `app_*` tools and says "Captured window_id" when it defaults
      one; `cua-driver` passes `window_id` and `pid`. A call that names an application is that
      application's largest window, and a call on the whole screen is the display.
    - A simulator's tools (a server for simulators or devices, `simctl` or a simulator
      destination in a command) mean the booted simulator named by id or by name, or the
      only one booted, as `xcrun simctl list devices booted -j` says. That is its window in
      Simulator, titled with the device's name. Another platform's device is no simulator.
    - A browser's tools (Claude in Chrome, Playwright, Chrome DevTools, `agent-browser`) mean
      the browser window titled with the page the tool reported. With no title, it is the one
      window of a browser only automation runs (Chrome for Testing, Chromium), never the
      person's own browser by guess.
  - **One fact on the thread.** `Action::ScreensSet` names up to three screens, the latest
    first, each with its capture target, an open kind (browser, simulator, desktop, app) and
    its label. A screen goes when its window closes or fifteen minutes after the agent last
    drove it. It is an action of its own because adapters re-send their whole meta, which
    would wipe a worker-set field.
  - **Watched, then taken over on the person's word.** The composer's toolbar offers the
    latest screen by its window's title (its mark alone in a narrow tile, the name in a hint),
    and the palette has "Watch the agent's screen". It opens as a window or display tile
    beside the thread, the item carrying the thread, so every client knows whose screen it
    is. While the agent drives, a pill at the foot says so. No move, click, key or scroll
    leaves the device, and a press does not raise the window. "Take control" stops the
    agent's turn under way through its own door (`Intent::Interrupt`) and gives the person
    the stream. "Hand back" watches again and sends nothing.
  - Tests:
    - `computer_use_names_a_window_an_application_or_the_display`,
      `a_browser_is_the_window_showing_the_page_the_tool_reported`,
      `a_simulator_is_the_booted_one_the_call_names` and the rest of
      `slopty-worker/src/thread/screens/tests.rs`.
    - `the_agents_calls_name_the_windows_it_drives` (`slopty-worker/tests/screens.rs`).
    - `an_agents_screen_is_watched_until_the_person_takes_control` (`slopty-ui/src/screen.rs`)
      and `the_screen_the_agent_drives_is_offered_beside_it` (`thread/tests/doors.rs`).
    - `the_screen_an_agent_drives_opens_beside_its_thread` (app e2e, over the drawn screen;
      goldens `agent-screen`, `agent-screen-dark`).

- ❌ **Deleted 2026-10-14: the line authorship below.** Its views went with the tiling workspace and
  no client asked for it any more, so the request, the answer and the worker's side are
  deleted (`ui.md`, "Code nothing reaches is deleted"). The ruling is kept as the record.
- ✅ **A line names the thread and turn that wrote it** (2026-10-04, frontier must-have M3).
  From a line of a file tile or a review, the person reaches the agent's thread at the turn
  that brought the line in.
  - **Blame over the turns' own snapshots.** The worker keeps the tree as each turn begins and
    ends (`thread::review`). Those snapshots, for every thread in one working tree and in the
    order the turns ended, make a history (`thread::authors`). Each turn becomes a commit of
    its after-tree. Where the tree changed between turns (the person's edits, another agent's),
    a commit of the next turn's before-tree comes first. `git blame --contents` of the file as
    it is now, against that history, gives each line the turn that brought it in.
    - The commits are made with fixed authors at the turn's own time (`Repo::commit_at`), so
      they are the same objects on every ask. No ref names them and the person's history never
      sees them. The history is kept per tree and grows a turn at a time. Asks are made one
      at a time per tree, since a review asks for many files at once.
    - A line from before the first snapshot, or from between turns, is blamed against the
      person's own `HEAD`. When that commit carries a `Slopty-Thread` trailer (a project merged
      it), the line is that thread's, with the commit and no turn. That holds on every machine
      with the commit, after the snapshots are gone. The thread may be another worker's; it is
      named when a worker here knows it.
    - A line changed since the last snapshot, and every line of a file larger than 4 MiB, is
      no one's.
  - **One answer per file, kept by the client.** `ThreadRequest::Authors` names a file,
    absolute or in a thread's repository. The answer (`Authors`) carries runs of lines, each
    with its thread, turn or commit, and when it was written. It also carries the file's
    modification time and blob, as the worker read it. The client keeps the last 64 answers
    (`slopty_client::threads::Authorships`) and draws from them on every frame. A file tile
    asks once per read (`FileView::stamp`, its modification time). A review asks for the file
    as its diff ends (its blob): the first 16 files listed as the review comes, the rest when
    the pointer first crosses one of their lines. No hover asks anything.
  - **Quiet until looked at.** A file tile names the caret line's author in a small pill in
    the corner of the text: the thread's name, the turn, and how long ago. A thread has one
    name on every surface (`WorkspaceView::thread_title`): the title of the tile its agent
    runs in here (the person's name for it, its project role, or the agent's own title), else
    its title as its worker's table says. The navigator, a thread tile, the inbox and both
    authors' tags read it. The pill hides while the edit differs from the file as read, since its lines no
    longer match the answer. A review names a line's author at the line's end while the
    pointer is on it, as "Turn N" within the thread's own review. A press on either opens the
    thread where its agent's tile shows it, else in its own tile, and scrolls to the turn's
    first message (`ThreadView::go_to_turn`). A turn older than those held is reached by
    paging back.
  - Tests:
    - `every_line_is_the_turn_that_wrote_it` and `a_file_with_no_history_says_why`
      (`slopty-worker/tests/authors.rs`, on a real repository with two threads, the person's
      edits and a trailer's commit).
    - `blame_gives_each_line_its_commit` and `trailers_name_the_thread_that_made_a_commit`
      (`repo/snapshot.rs`).
    - `a_file_is_asked_once_per_change` (`slopty-client`).
    - `the_carets_line_names_the_turn_that_wrote_it` (file tile),
      `a_line_names_the_turn_that_wrote_it_under_the_pointer` (review) and
      `a_turn_gone_to_shows_from_its_message_paging_back_to_it` (thread view) and
      `a_thread_goes_by_its_agents_tile_everywhere` (workspace), all in `slopty-ui`.
    - The app e2e `a_line_names_the_turn_that_wrote_it_and_opens_it`, with goldens
      `file-author` and `file-author-dark`.
- ✅ **The thread composer's menus never blank, and a waiting message is changed in place**
  (2026-10-02, `crates/slopty-ui/src/conversation/thread/view/composing.rs`;
  `thread/tests/composing.rs`). `/` lists the commands the thread says its agent takes, ranked
  as the conversation face ranks them; a source the client does not know ranks after plugins
  and is named as the agent named it. `@` asks the worker's file index under the thread's
  directory (`FindFiles`), the same one the palette asks. While the answer for the newest key
  is on its way, the last answer narrowed here stands in, so the list never empties between
  keys. Attachments go up as a drop on the tile does and the message carries their paths after
  its text; nothing goes while one is still uploading. A waiting message's pencil puts its
  words in the composer and the draft aside: ↵ sends `Intent::Edit` and brings the draft back,
  and Esc brings it back with nothing changed. The line shows the new words from the frame of
  the edit. A refused edit puts the old words back with the reason, and the person's words stay
  on the pencil until they dismiss it. If the message goes while it is being changed, its words
  stay in the composer as a new draft, with the draft that was put aside after them.

- ✅ **Codex: Slopty is one more client of the user's app-server, and the first answer to an
  approval settles it** (2026-10-02, verified against Codex 0.160.0;
  `crates/slopty-agent/src/codex/`, `xtask/src/codex/`; `crates/slopty-agent/tests/codex.rs`
  over `tests/fixtures/codex/approval.jsonl`).
  - **Types from the pinned build.** `cargo xtask codex schema` runs the official 0.160.0
    build's `codex app-server generate-json-schema --experimental` and turns the definitions the
    methods Slopty speaks reach into serde types: 12 client requests, the 5 approval and
    question requests, 27 notifications. The generator is xtask's own, about a thousand lines,
    because the bundle's shapes are few (objects, string enums, unions tagged by a property or
    by their one key, untagged unions, options, and an object that is also a union, read
    flattened). typify was the plan's choice, but it brings a dependency tree and its own
    naming for shapes that need neither. The file is checked in, so a Codex bump shows as a
    diff of its wire.
  - **The socket.** The daemon's control socket,
    `$CODEX_HOME/app-server-control/app-server-control.sock`, speaks WebSocket, one JSON-RPC
    message per text frame, without the `"jsonrpc"` field;
    `codex app-server proxy` only copies bytes, so a client has to speak WebSocket itself
    (tokio-tungstenite, the handshake only). The path in `CODEX_HOME` is a symlink to a socket
    in a short shared directory (`/tmp/codex-daemon-<uid>/<hash>`), because a socket's path
    must fit in 104 bytes on macOS. A client connects to the symlink's target for the same
    reason.
  - **A thread can be resumed only once its first turn has written it.** `thread/resume` of a
    thread with no turns yet fails with "no rollout found", so a second client joins after
    the first turn.
  - **Approvals with two clients, as recorded.** With Slopty and a Codex TUI both following one
    thread, the app-server sends `item/commandExecution/requestApproval` to both, under one
    request id, and both show `waitingOnApproval` in the thread's status. The first answer
    settles it. Both clients hear `serverRequest/resolved` for that id, and the turn goes on.
    A second answer, sent after that, gets no reply: the app-server logs "could not find
    callback" and drops it. So the face can offer the approval beside the TUI. Whoever answers
    first wins, and the other side's card is closed by `serverRequest/resolved` rather than by
    an error to its own answer.
  - **The recording.** `cargo xtask codex fixtures` runs the official build's app-server on its
    control socket in a scratch `CODEX_HOME` outside the repository, so no `AGENTS.md` or git
    state is read. The model is a canned Responses API on loopback, set up as a custom provider
    with retries off. Plugins and apps are off, so nothing is fetched. The policy is
    `on-request` in a read-only sandbox, and the model asks to run a command with escalated
    permissions. The paths, the host name, every UUID, the user agent and every time are
    scrubbed. The fixture tests read every recorded frame through the generated types. What
    either side sent reads back to the same JSON, and the routing above is asserted as
    recorded.
  - **The codec and the worker** (`codex/shared.rs`, `crates/slopty-worker/src/thread/codex.rs`;
    `crates/slopty-agent/tests/codex.rs` maps the recording, `apps/slopty-worker/tests/codex.rs`
    replays it to the real daemon from a stand-in app-server).
    - The worker joins the daemon's control socket under `$CODEX_HOME`, else `~/.codex`, and
      tries again every 2 s while there is none. It initializes as `slopty` with the
      experimental API, lists the loaded threads and resumes each. It resumes a thread the daemon
      starts later at `thread/started`, or at the thread's next status change while it is not
      yet resumable: the daemon tells every client of those, followed or not.
    - A Codex thread is one thread with drive `shared`, its id derived from Codex's. The turns
      a resume brings back are read first. Each of Codex's turns is the next of Slopty's, and
      an item keeps Codex's id. The status maps idle to done, failed or stopped by how the
      last turn ended, and `waitingOnApproval` to needs you, worded with the request once it
      opens.
    - A command or file approval offers Codex's `availableDecisions` in Codex's order: allow,
      allow for this session, always allow with the rule as its scope, deny, and deny and stop
      (`cancel`). The choice id is the decision's own name, so the answer sent is the TUI's own
      JSON-RPC answer, byte for byte as the recording has it. A permissions grant, a question
      or an elicitation opens as a request with no answers, for the TUI to answer.
    - **Two sides, one answer.** A request is settled only by `serverRequest/resolved`. It is
      named as answered from Slopty when this worker sent the first answer, else as answered by
      `Codex`. An answer to a request that is already settled, from a second client or after
      the TUI, is taken (`Outcome::Done`) and sent nowhere. The card shows who settled it. It
      is never an error, because nothing went wrong. In the narrow race where the TUI and
      Slopty answer within one round trip, Codex keeps the first it hears. Slopty may name
      itself then, since `serverRequest/resolved` does not say who answered.
    - A message goes as `turn/start`, or as `turn/steer` into the turn under way, carrying its
      intent as `clientUserMessageId`, which comes back as the user message's intent. An
      interrupt is `turn/interrupt`. A message sent while a turn runs can instead wait in the
      worker's queue (`Cap::QUEUE`, `slopty_agent::queue`) and go as the next turn.
    - Carried since: starting a Codex thread (`thread/start`, `Codex::start`), its TUI joining
      as `codex resume <thread>` in a worker terminal (`Codex::release`), the models list
      (`model/list`) and pictures in a user message (`localImage`). Not carried: expanding a
      clipped output, which the worker serves only for Claude Code (`Threads::expand` in
      `apps/slopty-worker/src/threads.rs`).

- ✅ **pi is driven over its RPC mode, with a gate that fails closed by pi's own rule**
  (2026-10-02, verified against pi 1.0.0, the latest on npm; `crates/slopty-agent/src/pi.rs`,
  `pi/rpc.rs`, `assets/pi-gate/gate.ts`, `xtask/src/pi/`; `crates/slopty-agent/tests/pi.rs`
  over `tests/fixtures/pi/gate.jsonl`).
  - **The gate.** pi has no permission system, so every pi Slopty drives loads one extension,
    embedded and written under the data directory by its digest as the Claude mod is. Its
    `tool_call` handler asks about each call through the RPC extension UI: a `select` whose
    title is the call as JSON (`slopty-gate/1`, the call's id, its tool, its arguments and the
    tool's own hints), with `allow` and `deny` as the options. Only `allow` lets the call run.
    A deny may carry the person's reason on the lines after it, and pi gives that reason to
    the model as the call's result. Anything else blocks the call: a dismissal, an abort, a pi
    run outside RPC mode. pi itself blocks a call whose `tool_call` handler throws, so a
    failure of the gate fails closed too. The extension only asks. What to ask the person and
    what to let through is the worker's to decide.
  - **As recorded.** pi says a call began (`tool_execution_start`) before the gate asks about
    it, so a call shows as running, then as waiting on the person. A steer sent while the gate
    waits is queued (`queue_update`) and goes after the call, as the next user message of the
    same run. An abort while the gate waits ends the call as "Operation aborted" and settles
    the run (`agent_settled`) before the abort's own response. pi writes `message_start` from
    the message it goes on filling, and each `message_update`'s usage from the usage it goes on
    adding to, so those hold some of what came after them, by timing. A message is built from
    its deltas and its `message_end`, its usage from the end alone.
  - **The recording.** `cargo xtask pi fixtures` installs `@earendil-works/pi-coding-agent` at
    the pinned version with bun under `target/pi/<version>`, its tarball checked against the
    registry's SHA-512, and runs it under node as its `bin` does. It runs with an environment
    of nothing but a scratch home, a scratch `PI_CODING_AGENT_DIR`, `PI_OFFLINE`,
    `PI_SKIP_VERSION_CHECK` and `PI_TELEMETRY=0`. Nothing is signed in, and no credential of
    this machine is in reach. The model is a canned Messages API on loopback, a provider of its
    own in the scratch `models.json`. The script is a first prompt with thinking, an allowed
    command with a steer queued at its gate, a denied one with a reason, and one interrupted at
    its gate, then the session's entries and stats. The paths, the canned model's address, the
    host name, every UUID, the session's entry ids and every time are scrubbed, and pi's
    system prompt is not kept. The starts and updates written by the race above are kept as
    pi begins them. Three recordings in a row are byte-identical. The tests read every record
    into the typed records, with none of an unknown kind. What went to pi reads back to the
    same JSON, and the gate's asks and answers are asserted as recorded.

- ✅ **pi on the worker: a thread started here runs the person's own pi, one writer per
  session, and nothing passes the gate but an answer it offers** (2026-10-02, verified against
  pi 1.0.0; `crates/slopty-agent/src/pi/driven.rs`, `crates/slopty-worker/src/thread/pi.rs`,
  `crates/slopty-testkit/src/bin/slopty-stub-pi.rs`; `crates/slopty-agent/tests/pi.rs` maps the
  recording, `crates/slopty-worker/tests/pi.rs` drives the stand-in end to end).
  - **The thread.** A pi session is one thread, its id derived from the session's id, with
    drive `driven`. A message from the person opens the next turn unless a run still works,
    when it steers that run and joins its turn. Each message streams from its deltas and is
    replaced whole at its end. The person's message `n` is item `msg-n` and the model's blocks
    `msg-n.i`; a tool call keeps pi's id. The same rule rebuilds the thread from the session's
    entries, so a thread read again is the one that was streamed, item for item.
  - **The session is named by the start.** `--session-id` with the start's intent id opens that
    session in pi's own directory or makes it, so starting a thread and taking it up again are
    the same words, and the person's own `pi --session <id>` finds it with their others. The
    plan had `--session <path>`, which puts the file outside pi's directory.
  - **Starting.** `ThreadRequest::Start` for agent `pi` starts it; every other agent is still
    refused there. pi is found on the daemon's `PATH`, else on the login shell's, and runs with
    that `PATH`, since pi is a Node program and its `node` is the person's too. Its version is
    what `pi --version` says. A start's own flags pass only from a list that loads no code,
    names no session and reaches no key (`slopty_agent::pi::SAFE_FLAGS`), and a value that
    reads as a flag is refused, since pi's parser may take it as one. A refused start is refused
    once per intent, as a started one is started once. The worker's facts list pi's version
    beside the other agents', which is what offers it.
  - **One writer.** The worker runs at most one pi per thread, and a thread's next pi starts only
    once the last is reaped. What was sent to a pi that was ending is handed back and goes to
    the next. A thread whose pi is gone is exited and resumable. The next message starts pi again
    on the same session, reads the thread again from `get_entries` before the message goes, and
    keeps each message's intent by its item. A session pi does not give back ends that pi with
    a notice, rather than numbering new items over old ones.
  - **Fail closed.** Only an answer the dialog offers reaches pi: the gate's allow, deny (with
    the person's reason) and deny and stop, which also aborts. When the worker goes, pi's stdin
    closes and pi ends with no answer sent, so a call at the gate never runs, and the thread
    shows the ask withdrawn, the call cancelled and the turn stopped. A worker that starts
    again cuts short from the thread whatever a pi of the last one was doing.
  - **Other dialogs.** An extension the person's pi loads may ask too. A choice or a yes or no is
    a question with the answers it offers; a text is a question answered in the person's own
    words. They stay open past the turn, since pi waits on them, until answered or until pi gives
    up at the dialog's timeout. A notice is a notice. Release is refused: while Slopty drives
    pi, there is no prompt of pi's own to hand a request to.
  - **The stand-in.** `slopty-stub-pi` replays a recording against what it is sent: a command
    that is the next step's gets that step's records under its own id, a query gets the
    recorded answer, and anything else is noted as unexpected and failed. It reads its fixture
    from a file beside the path it was started as, because the worker that starts it may be the
    test itself, whose environment is shared. The worker tests run the gate's recording through
    it: allow, deny with a reason, an interrupt at the gate, a resume from the session, and the
    worker going while the gate asks, each asserting what reached pi.
  - **Kept flags.** The start's safe flags, without the model ones, are kept as the thread's
    `pi-args` fact and re-checked against the safe list each time pi starts again on the session,
    so a resumed or taken-back pi keeps its tool list. The model is left to the session, which
    keeps its own.
  - A picture or file in a message goes to pi as every agent's attachments do
    (`Intent::Send::attachments`, `slopty-worker/src/thread/pi.rs`). Queueing came on
    2026-10-06: see "One queue on the worker for ACP, Codex and pi".

- ✅ **Any ACP agent is driven by one adapter, as the person's own program, asking before it
  acts** (2026-10-02, ACP v1 through `agent-client-protocol-schema` 1.10.2;
  `crates/slopty-agent/src/acp/`, `crates/slopty-worker/src/thread/acp.rs`; the tests in
  `crates/slopty-agent/tests/acp.rs` and `crates/slopty-worker/tests/acp.rs`).
  - **The types, not the SDK.** The `agent-client-protocol` crate brings an async runtime of its
    own beside tokio and pins an older schema. Slopty takes the schema crate alone, with no
    default features (no `schemars`, no unstable methods), and writes the framing itself: one
    JSON-RPC message a line, read as JSON first and classed by its members, so a method it does
    not know is answered or passed over, never a parse error that stalls the agent. The codec is
    sans-IO, like pi's, and the worker carries the lines.
  - **Which agents.** An open registry (`acp::registry`): the public ACP registry's agents, each
    with the command line that serves ACP on stdio, run as the person's installed program,
    never fetched. The person adds their own or replaces a known one by name and command line,
    and an empty command line takes one away. Claude Code, Codex and pi are left out, since each
    has an adapter of its own over a richer protocol. A thread's agent is `acp:<name>`, and
    `ThreadRequest::Start` for it starts the program found as the person's terminal finds it,
    in the thread's folder, with the daemon's environment. A start's own arguments are refused:
    the command line comes from the registry or the settings, never from a client.
  - **Slopty is a bare client.** `initialize` offers no file system and no terminal, so the agent
    works with its own tools. Its `session/request_permission` is a request on the call, with
    exactly the answers it offers (allow and reject, once or always, with the scope shown), and
    only one of those goes back. A request of any other method is refused with
    `method_not_found`. A cancel answers every open request as cancelled, as the protocol asks.
    An agent that answers that it needs signing in is left as it is, with a notice saying to
    sign in with the agent's own command in a terminal: Slopty never signs an agent in.
  - **Turns and items.** A prompt is a turn, opened by the person's message and ended by the
    prompt's stop reason (cancelled is stopped; refusal fails; the token and request limits end
    it with a notice). Streams are cut into items where the kind or the message id changes, and
    a tool call keeps the agent's id. A diff in a call's content is an edit or a write, with its
    patch. Plans, commands, modes, config options, usage and the session's title map to the
    thread's own fields. Anything else is kept as an `Extra` item rather than dropped.
  - **One message at a time.** ACP takes no message while a prompt runs, so the adapter declares
    `queue` and not `steer`: a message sent during a turn waits on the worker, can be withdrawn
    or edited there, and goes once the turn ends. A steer is refused as unsupported.
  - **Resumed by loading.** Whether the agent can load a session (`loadSession`) is kept as the
    thread's `acp-load-session` fact. A thread whose agent is gone is exited, resumable when that
    fact holds. The next message starts the agent again and sends `session/load`, and the
    agent's replay rebuilds the thread from nothing before the message goes. The replay is held
    until the load is answered, so a load that fails leaves the thread as it was, with a notice.
    A thread whose agent cannot load sessions refuses a message once it is gone.
  - **The stand-in.** `slopty-stub-acp` replays a recording against what it is sent, as
    `slopty-stub-pi` does: a request is the next step's by its method (a prompt by its words),
    an answer to one of the agent's own requests by that request's id and what it answers, and
    the agent's answers go out under the ids the client chose. It saves what it heard before it
    answers, so a test that sees the answer finds it said. The worker tests start
    `acp:opencode` with the stand-in on the `PATH` as `opencode`: a turn, an allow, a message
    queued behind an ask, a reject, an interrupt while the agent asks, a resume by loading, an
    agent that asks to be signed in, and the starts that are refused, each asserting what
    reached the agent.
  - **A failed agent is not started again on its own.** Messages held for an agent that failed
    (it could not open its session, or ended with an error) are dropped with the thread saying
    why, rather than handed to a fresh start that would fail the same way, over and over. The
    person's next message tries again.
  - **The fixtures are recorded.** `cargo xtask acp fixtures` downloads OpenCode 1.18.34 from the
    npm registry, checked against its SHA-512, and runs `opencode acp` in a scratch home with
    nothing signed in, its model list not fetched and its own providers off. Its model is a
    canned Messages API on loopback, set in the scratch config as the only provider, and every
    tool asks first. The recorder sends what the adapter sends and refuses what it refuses: a
    greeting with thinking, a write allowed once, a command rejected, a command cancelled while
    asked about, then the session loaded in a second process and prompted again. The lines are
    scrubbed (paths, OpenCode's time-ordered ids, times) into `turns.jsonl` and `load.jsonl`.
    Only `auth.jsonl`, an agent that wants a sign-in, is written to the schema by hand, since
    nothing signed out reaches it. `tests/acp.rs` holds every line to the schema's types and
    every message Slopty sends to the very JSON the agent was sent.
  - **What the recording showed of OpenCode.** It asks the client to write the file
    (`fs/write_text_file`) even though `initialize` offered no file system. The worker's refusal
    leaves the write to OpenCode, which makes the file itself. A write's diff appears only in the
    permission request, and the call's end replaces it with the tool's output, so the adapter
    keeps a diff once told. A new file's diff has an empty old text where the protocol has none,
    so an empty old text reads as a write. Its loaded session ends a rejected or cancelled call
    as failed, since nothing in the replay says why.
  - **The person's own agents.** `[worker.acp]` in the settings names an agent and its command
    line, or replaces a known one by name, and an empty list hides one. The worker reads it at
    each start, off the runtime, so an edit needs no restart.
  - **What a client can offer.** The worker's facts list the ACP agents installed under `acp`,
    by the registry's names (`acp.cursor`, `acp.amp-acp`, the person's own), each with the
    version its program says or `true` when it says none. Those names are the ones a thread's
    agent carries (`acp:<name>`), so what a client offers to start is what the worker will run.
    The `agents` fact had listed some of these by their program (`cursor-agent`, `amp`), which
    named neither the agent nor, for Amp, the program that speaks ACP; it now holds only the
    agents with adapters of their own (and `aider`).
  - Pictures and files in a prompt go as every agent's attachments do
    (`Intent::Send::attachments`, `slopty-worker/src/thread/acp.rs`).
  - **Not carried yet:** the client's offer itself (queued for the UI) and the unstable methods
    (forking, subagents, session notices).

- ✅ **A client starts Claude Code and Codex threads too, each through the agent's own door**
  (2026-10-02, `crates/slopty-worker/src/thread/claude/start.rs` and `thread/codex.rs`,
  `slopty_agent::resume::started` and `codex::shared::start`; tests
  `crates/slopty-worker/tests/claude_start.rs`, `crates/slopty-worker/tests/codex.rs`, and
  `a_claude_code_start_opens_claude_in_a_terminal_and_names_its_thread` in
  `apps/slopty-worker/tests/threads.rs`). `ThreadRequest::Start` had refused both agents.
  - **Claude Code runs in a terminal and is observed.** The worker finds the person's own
    `claude` as their terminal finds it and opens it in one of its terminals, in the thread's
    folder. It goes through the worker's usual opening of any `claude`, so it gets the hook relay,
    the mod and Slopty's tools as a tile opened on `claude` does. The first message is Claude
    Code's own initial prompt, after `--` on its command line, and the model is its `--model`.
    Nothing is typed into the TUI, nothing is signed in, and a Claude Code that is not signed in
    says so in its own terminal. Arguments from a client are refused.
  - **Named before it speaks.** The worker chooses the session id and passes it with
    `--session-id`, so the thread's id (`observed::thread_of`) is known when the terminal opens.
    The observer begins the thread at once, with the terminal named, and the start is answered
    with it. The hooks and the transcript then fill it in as for any observed session, and the
    first message carries the start's intent once the transcript shows it. The provisional thread
    of a terminal whose session id is not yet known is not needed here: the id is never unknown.
  - **Codex is asked over its daemon.** The worker sends `thread/start` to the person's Codex
    app-server, as Codex's TUI starts a thread. Only the folder and the model are set, so the
    approval policy and the sandbox stay the person's own configuration's. The connection that
    starts a thread is subscribed to it, so the thread is followed from Codex's answer, and the
    prompt goes as its first turn (`turn/start`), marked with the start's intent. The TUI joins
    it as it joins any of the daemon's threads. A start asked before the handshake is held until
    the handshake is done. (A start while the daemon is not running was refused here; that is
    superseded by "A Codex start brings the person's daemon up" below.)
  - **Once.** Both are acted on once per intent id (`Host::record_start` for a thread its
    adapter begins on its own): a repeat gets the first outcome back and opens or asks for
    nothing.
  - **The stand-ins.** `slopty-stub-claude` is the person's `claude` on a `PATH` of the test's
    own. Its hooks now name the session it was started on, as Claude Code's do, so the relay's
    first hook is heard on the started thread. The Codex daemon is the recorded client that
    started a thread (`approval.jsonl`), replayed, with the person's message marked by the id the
    turn named, as Codex marks it.

- ✅ **Any agent a worker offers starts from the palette, and its thread opens as a tile of its
  own** (2026-10-02, `ItemKind::Thread` in `slopty-proto`; the client in
  `crates/slopty-ui/src/workspace/{faces,overlays,projects}.rs`; tests
  `the_palette_starts_each_agent_the_worker_offers` in `workspace/tests/thread_start.rs`,
  `a_claude_code_start_with_no_prompt_opens_in_the_home_it_names` in
  `apps/slopty-worker/tests/threads.rs`, the `worker_item_thread` golden, and the app e2e
  `crates/slopty-e2e/tests/app/threads_start.rs`).
  - **What a worker offers comes from its facts.** The client asks the server for every
    worker's facts (`Verb::WorkerFacts`) when a worker links and each time the palette opens,
    and maps the server's worker ids to its own keys. Claude Code, Codex and pi are offered when
    `agents` lists `claude`, `codex` and `pi`. Each ACP agent `acp` names is offered too. A
    program with no thread to start (`aider`) is not. With no server, nothing is offered, since
    only the server holds the facts. The `acp:<name>` naming moved into `AgentId` so the client
    builds it without linking the adapters.
  - **One line per agent, for the focused tile's worker and folder.** "New <agent> thread"
    starts it on that worker, in the folder a search from there would use: the focused shell's
    repository or directory, a file's or folder's own, else the worker's home as `~`. The
    worker expands `~` against its own home, since a client does not always know that home.
  - **No prompt.** A start from the palette carries no first message. Claude Code opens
    waiting for the person, and Codex, pi and ACP agents start a thread with no turn yet. What
    to ask is typed into the thread's own composer.
  - **The thread tile is the default and the terminal is one action away.** The started
    thread opens as an `ItemKind::Thread` tile drawing the thread view as an agent's tile does,
    and it takes the keyboard. A Claude Code thread's terminal does not open a second tile
    beside it. The view's "terminal" action reveals that terminal's tile, or adds one for the
    session when it has none. This follows the GUI-first direction: the person directs and
    reviews in the GUI, and the TUI stays able to take the session over. Superseded 2026-10-05
    by **An agent with a terminal lives in that terminal's tile** (ui.md): a thread whose agent
    runs in a live terminal is shown in that terminal's tile, and the thread tile is for the
    rest.
  - **Refusals are said, never opened.** A worker that refuses a start (a missing binary, a
    Codex daemon not running) answers in words, shown as a notice, and no tile is added.

- ✅ **One answer answers all of a request's questions, and each adapter reads it as its agent
  takes it** (2026-10-02, `detail::Answer::{choice, read, parts}` and the doc of
  `Intent::Answer` in `slopty-proto`; tests `one_answer_answers_every_question`,
  `a_questionnaires_answer_is_the_answers_claude_code_takes` and
  `codex_questions_are_carried_and_answered_by_their_ids`).
  - **The encoding.** The questionnaire card sends one `Intent::Answer` for the whole form. For
    a lone question that offers nothing it is the words typed. Otherwise it is a JSON list of
    `detail::Answer`, one per question keyed by its text, with several picks of one question
    joined by `", "` and one's own words last, which is how Claude Code's own dialog joins them.
  - **The card is gpui-kit's questionnaire** (`conversation/thread/questions.rs`,
    `thread/view/asking.rs`; tests `thread::tests::questions::*`, e2e
    `an_agents_questions_are_answered_in_the_thread`). One question at a time under its header,
    each answer with its description, single or multiple choice, an "Other" field beside the
    choices and a field alone when there are none; digits, arrows, ↵ and ⌘↵ walk it. A new
    questionnaire takes the keyboard only from an empty composer, so a message being typed is
    never read as answers, and gives it back when it goes. "Answer in the terminal" shows only
    for a thread with a terminal.
  - **Claude Code** takes the list as `AskUserQuestion`'s answers as they are.
  - **Codex** asks with `item/tool/requestUserInput`. Its request now carries the questions,
    each with the answers Codex offers and their descriptions, and the answer goes back under
    each question's id, the picks and one's own words split again by the labels offered
    (`Answer::parts`). An answer that leaves a question out, or names one not asked, is refused
    by the worker before it reaches Codex.
  - **A secret stays in the terminal.** A Codex question for a secret (`isSecret`) is not
    carried: an answer given in the GUI is kept in the thread's log. The card says to answer
    it in Codex's own terminal, which runs beside the face.
  - **The fixture is recorded.** `cargo xtask codex fixtures` starts a turn in Plan mode, where
    Codex offers the model its `request_user_input` tool. The canned model asks two questions,
    the recorder answers them by their ids as the adapter does, and Codex hands the answers back
    to the model as the tool's output (`tests/fixtures/codex/question.jsonl`). Its frames are
    held to the generated types, and a secret question is the recorded request with `isSecret`
    set.

- ✅ **The thread view carries what the conversation face showed, and its e2e moved with it**
  (2026-10-02, `crates/slopty-ui/src/conversation/thread/view/`;
  `crates/slopty-e2e/tests/app/conversation.rs`, `thread/tests/steps.rs`). A subagent's call
  opens its thread in the same view, under a bar that names it and leads back (Esc too), with
  the thread above kept followed and kept as the reader left it. ⌃O opens every step and folds
  them again. Commands run in the background sit over the composer with their last line, the
  running ones and those that ended in the last turn. Pictures sent with a message or returned
  by a call are drawn at the size their header gives, so a row keeps its height while the
  bytes come. A long message shows its start until "Show more". The working line shows only
  while the agent says it works: a turn whose end was never written is not under way once the
  agent is idle. Every behaviour the old face's e2e covered is now a thread-view e2e of the
  same behaviour, with the goldens renamed `thread-*`. The old face's code waits for S2.

- ✅ **The thread model carries what each agent's door publishes** (2026-10-02, audit in
  `.research/thread-completeness-2026-10-02.md`). The person wants the agent GUI as complete
  as T3 Code and Amp, for every agent. The audit lists each fact against each agent as carried,
  published but not carried, or not published; this pass carried the published rows that matter
  most to someone watching.
  - **A retry is said** (`Notice.retry`: attempt, max, wait). Claude Code, Codex
    (`willRetry`) and pi (`auto_retry_start`) say when they retry an API error. Without it an
    overloaded agent looked hung. Tests: `an_api_error_it_retries_says_the_attempt`,
    `an_error_codex_retries_counts_its_attempts`, `a_retry_says_its_attempt_and_its_wait`.
  - **Background work is listed** (`BackgroundTask` gained `kind`, `started_ms`, `ended_ms`).
    Claude Code's background shells and agents become `TasksSet`, keyed by the agent's own
    task id, closed by their end and dropped when the transcript drops them. Test:
    `background_commands_are_the_threads_background_work`.
  - **Per-turn cost is a usage key, not a field.** *Cut the same day: see "Dollar cost, the
    carried account and drafts on the worker are gone" below.* `Usage::COST_MICRO_USD` sat in
    the turn's open usage map, and `Usage::tokens()` left it out of the token sum. A new `Turn`
    field would have changed every literal of it for one number only some agents give.
  - **Effort is a meter** (`Meters.effort`): Codex's reasoning effort, pi's thinking level, ACP's
    thought-level option, each as the agent names it.
  - **Codex.** The turn's diff (`turn/diff/updated`) is the turn's changed lines. A reroute adds
    the model to the turn with a notice. Approval policy and sandbox are the thread's mode and
    a fact. A collab agent call is a subagent call that opens its child thread, and the child
    names its parent. Account rate limits carry no thread id, so the worker gives them to every
    Codex thread it follows and to each one it follows later. A thread's cost is Codex's own
    estimate (`account/usage/read` for the thread, its `estimatedUsageUsdMicros`), asked when
    the worker takes the thread up and after each turn ends, since Codex announces no cost. A
    plan billed only in credits gives no dollar figure, and the thread keeps none rather than
    a converted guess. Tests in `crates/slopty-agent/tests/codex.rs` and the worker's
    `tests/codex.rs` stand-in.
  - **ACP** turn tokens come from `PromptResponse.usage`, behind the schema's
    `unstable_end_turn_token_usage` feature, which this crate enables. It is an unstable part
    of the protocol, taken because it is the only door to a turn's tokens.
  - **A hook's folder outranks the terminal's.** A hook-only Claude Code thread had no folder,
    so its review said "not in a git repository" and its paths were absolute. The worker now
    takes `cwd` from `SessionStart` and every later hook, ahead of the terminal's process
    folder, and offers the folder's slash commands (`CommandsSet`). Test:
    `a_session_works_where_its_hooks_say`.
  - **A hunk is headed as git heads it** (`Hunk.heading` in both patch types). The rule is
    git's default for a file with no diff driver: the nearest line above the hunk that starts
    with an ASCII letter, `_` or `$`, cut at 80 bytes and then trimmed. It comes from the old
    file for the worker's snapshots and Claude Code's edits (`originalFile`), and from the
    `@@ … @@` line Codex writes. Test: `hunks_are_headed_as_git_heads_them` compares with
    `git diff --no-index`.
  - **An `AskUserQuestion` is a question, whichever hook brings it.** Claude Code answers it
    through the permission hook, so its `PermissionRequest`, and the permission prompt that
    follows, said "Wants to use AskUserQuestion". The tracker now reads both as
    `Blocked(Question)` with the question as detail. Test:
    `a_question_asked_through_the_permission_hook_stays_a_question`.
  - **Left:** Codex background terminals, hooks, MCP startup and rewind (not in the generated
    protocol yet), Claude Code's per-turn cost, MCP servers as model state, pi commands, and
    ACP subagents and compaction. Since done: Claude Code `StopFailure` reads as failed (with
    its error, and a rate limit's reset), and pi's edits carry their diff (readiness rank 16).

- ✅ **A Codex thread reads like every other agent's** (2026-10-02, from the showcase).
  - A thread Codex has not named takes its first prompt as its title, as pi and ACP threads do,
    until `thread/name/updated` names it.
  - Codex runs every command through the person's shell (`/bin/zsh -lc '…'`). The call's title,
    its command row and the request's title show the command the shell runs, its quoting undone
    as a POSIX shell reads one word. A command that is not exactly one shell word after `-c` is
    shown as Codex sent it.
  - Codex begins a call before it asks about it, so the call is told again as waiting on the
    person while a request is open about it, and as running once the request is settled.
  - A request open here puts the thread in "needs you" whatever flags Codex sent with its
    status, and whether or not it said yet that the thread is active.
  - An ACP write of a new file is numbered from line 1, as git numbers a new file. The same
    holds for Claude Code's proposed `Write`.
  - Tests: `the_starter_sees_the_thread_and_the_approval_settled_elsewhere`,
    `an_open_request_waits_on_the_person_whatever_the_flags`,
    `a_commands_shell_wrapper_is_taken_off` and `the_session_maps_onto_the_thread`.

- ✅ **A Codex start brings the person's daemon up** (2026-10-04, rulings §3). A Codex start
  while no app-server daemon answers runs `codex app-server daemon start`. That is Codex's own
  lifecycle command, published for remote clients reaching a machine over SSH, and the one
  Codex's TUI itself runs at launch since 0.157.0. It runs the person's `codex`, found as their
  login shell finds it. The start waits while the command runs (up to 20 s, `DAEMON_WAIT`) and
  goes on once the daemon answers. The JSON object the command writes names the socket. A second
  start meanwhile is held with the first and starts nothing more.
  - **Refused in words, never retried.** No `codex` on the machine gives "Codex isn't installed
    on this machine". A start that fails is quoted as Codex wrote it, without its `Error:`
    label: "Codex's app-server didn't start: …". Nothing tries again until the next start.
  - **Only on a start.** A worker that only follows Codex never launches it, however long no
    daemon runs. A request handed to Codex's TUI while no daemon runs is still refused.
  - **No private server.** A `codex app-server` child of the worker would hold the thread
    outside the daemon, where the person's TUI cannot join it: a second writer.
  - **Resume is a start of the same thread.** A start whose arguments are `resume <thread>`
    (Codex's own words, `codex resume`) takes the thread up again with `thread/resume`. It is
    followed under the same thread id and answered `Started` with it, at once when it is followed
    already, and refused in Codex's words when Codex cannot load it. An exited Codex thread's
    Resume sends that start, so the daemon comes up and the thread comes back through one door.
  - **Since closed:** `InstalledAgent` names its agent by the open `AgentId`, and `caps.rs`
    finds `codex` and `pi` beside `claude`. The closed `AgentKind` is deleted (readiness rank 23).
  - Code: `crates/slopty-worker/src/thread/codex.rs` (`Launch`, `Bringing`, `Begin`),
    `crates/slopty-agent/src/codex/daemon.rs`. Tests (`crates/slopty-worker/tests/codex.rs`):
    `a_codex_start_with_no_daemon_starts_the_persons_daemon_once`,
    `a_failed_daemon_start_is_refused_in_codexs_words`, `a_worker_alone_never_starts_codex` and
    `a_start_that_names_a_thread_takes_it_up_again`. A stand-in `codex` records how it was run.
    Also `daemon::tests::a_start_reads_as_its_socket_or_codexs_words`.

- ✅ **What an agent turns down is said, and Codex answers through its own doors** (2026-10-04,
  readiness #2, #8, #9, #14).
  - **Refusals stay in words.** The client's outbox settles a failed intent at once, so a
    refused stop, model switch, mode, compact, answer, release, Keep or Revert used to vanish
    without a word. The thread hub now keeps each one, other than a message or an edit to one
    (those keep their own line), as "Couldn't stop: …" or "Couldn't keep lib.rs: …". It stays
    until the person dismisses it or tries again, and holds up to 32. An intent the agent has no
    door for reads "the agent can't do that through Slopty".
  - **An attachment on its way holds the message.** ↵ while a file is still uploading waits for
    it ("Sends once the attachments are up") and sends by itself when it lands. A failed upload
    sends nothing and says so, keeping the words. (Threads with no terminal refused files here
    until "Every thread takes files" on 2026-10-05.)
  - **Codex queues.** A message queued while a Codex turn runs is held by the thread's mapping
    (`Shared::send` → `Send::Held`), shown as pending, changeable and withdrawable. It goes as the
    next `turn/start` once Codex says the turn ended. Codex has no queue of its own, so the
    thread now offers `Cap::QUEUE`.
  - **Codex's forms.** An MCP server's form (`mcpServer/elicitation/request`, mode `form`) is a
    request whose questions are its fields, in the order of their names. A boolean is Yes or
    No, an enum is its titles (several for a list), and text and numbers are words. The answers
    go back as `{"action": "accept", "content": {…}}`, each value of its field's type, and an
    answer that does not fit its field sends nothing. Decline and Cancel are the denies. A page
    to open or a device check stays with Codex's TUI.
  - **"Answer in Codex".** A Codex request with nothing to answer in the GUI (a secret, a
    grant) is released to Codex's own TUI. The worker runs `codex resume <thread>` in one of its
    terminals, which joins the running daemon as one more client, and names that terminal on the
    thread while it runs. The client brings it into view once it is named.
  - **Deny with a reason.** "Deny…" turns the answers into a field. The words go with the plain
    deny as the answer's message. Claude Code and pi take a denial's reason. Codex and an ACP
    agent hear it as the person's next message (a steer).
  - **Resume through each agent's door.** Claude Code: a start of `claude --resume <session>`
    in a new terminal, on the same thread. It is refused when that session already runs or the
    id is not a session. Codex: a start of `resume <thread>`, as above. pi and an ACP agent that
    loads sessions: the next message, which the composer says. An agent that cannot load its
    session says it cannot.
  - **Modes.** The agent's modes come in `ThreadMeta.modes`; a thread with its own TUI but no
    `SET_MODE` says the mode is changed there.
  - **"Machine", not "worker",** in what these surfaces say (rulings §8).
  - Edited allows (§5a) were deleted ("Sleep, waits on another thread, queue reordering and
    edited allows are gone"), and threads with no terminal take uploads (the composer's
    attachments).
  - Tests: `conversation::thread::tests::doors::*` and the attachment and queue tests in
    `tests::composing`; `review::tests::a_refused_keep_says_why_on_its_hunk`;
    `codex::form::tests::*`; `a_queued_message_waits_for_the_turn_and_goes_as_the_next` and
    `an_mcp_form_is_answered_as_its_content` (`crates/slopty-agent/tests/codex.rs`);
    `an_exited_thread_is_resumed_on_its_own_session_in_a_new_terminal`
    (`crates/slopty-worker/tests/claude_start.rs`).

- ✅ **A machine's own link says which agents it can start, with no server** (2026-10-04,
  readiness N11). This replaces "What a worker offers comes from its facts" above: with no
  server linked, nothing but Claude Code could be started, and the worker's capabilities found
  only `claude`.
  - *Wire.* `InstalledAgent` names its agent by the open `AgentId` its threads carry
    (`claude-code`, `codex`, `pi`, `acp:<name>`), not by a closed kind. Golden
    `machine_hello_ack` (`tests/golden_machine.rs`).
  - *The worker.* `caps::installed_agents` reads the same `agents` and `acp` facts the server is
    told (`facts::agents`): Claude Code, Codex and pi where their program answers `--version`,
    then each ACP agent of the registry or of `[worker.acp]` whose program is there. A program
    with no adapter (`aider`) is a fact but not startable.
  - *The client.* The palette's "New <agent> agent" lines and the start picker read the
    machine's capabilities as its link last said them, and an open palette takes a change at
    once. The server's `WorkerFacts` question is gone from this path.
  - *A start shows at once.* "Starting Codex on studio in ~/x…" is said as the start is sent,
    and an answer that is neither a thread nor a refusal (`Unsupported`) is said in words rather
    than logged.
  - Tests: `the_agents_found_are_what_a_thread_can_be_started_of` (`slopty-worker::caps`),
    `a_started_thread_opens_as_a_tile_and_a_refusal_is_said` and
    `an_open_palette_takes_the_agents_as_they_arrive` (`workspace/tests/thread_start.rs`).

- ❌ **An edit can be allowed as the person changed it, through the hook's `updatedInput`**
  (2026-10-04, rulings §5a). *Deleted the same day: see "Sleep, waits on another thread, queue
  reordering and edited allows are gone" below.* The
  hooks reference documents `updatedInput` on a `PermissionRequest` allow as the call's changed
  input, which Claude Code checks against its rules again before it runs it.
  - `PermissionPrompt::editable` and `Request::editable` carry the parts of the call the person
    may change: an `Edit`'s `new_string` and a `Write`'s `content`, each up to
    `Editable::TEXT_MAX` (256 KiB). A longer one is allowed or denied whole.
  - The answer is `Verdict::AllowEdited { input }` on the conversation path, and on the thread
    path an `Intent::Answer` whose choice is `Editable::choice`, a JSON object keyed
    `allow-edited`, which no option id, answer list or typed words can be.
  - The relay lays the edited fields over the call's own input, taking only a field offered
    for editing that the call holds as text. An edit can neither add a field, point the call at
    another path, nor change one that is not text, and words that are not a JSON object of
    texts leave the call as asked.
  - *The tray.* "Edit…" stands after the plain allow where the request offers a part. It puts
    each part in a field in the code face, in the answers' place, the first taking the
    keyboard. "Allow edited" sends them as they are now, and Cancel brings the answers back.
    It is a field rather than Slopty's diff editor: the change is one call's text, and the
    proposed diff stays drawn above it.
  - Tests: `edit_then_allow_sends_the_persons_text` (`conversation::thread::tests::doors`),
    `only_an_approval_with_parts_and_a_plain_allow_is_editable`,
    `an_edited_allow_carries_the_edited_input` (`slopty-agent::permission`),
    `an_edited_allow_reads_back_and_nothing_else_does` (`slopty-proto::thread`), goldens
    `machine_permission_editable`, `machine_answer_edited` and `intent_answer_edited`.

- ✅ **A thread lists the modes its agent publishes** (2026-10-04). `ThreadMeta::modes` (id,
  label, the agent's own description) answers the proposed change above. An ACP agent's session
  modes, or its mode config option, fill it. Claude Code, Codex and pi publish none on their
  doors, so theirs stay empty and the TUI changes their mode. Where the thread can switch
  (`Cap::SET_MODE`) and lists modes, the mode chip names the mode by the agent's own label and
  opens them as a menu, each with the agent's description; a pick sends `Intent::SetMode`, and
  the chip reads as the agent then says. Golden `frame_snapshot`; test
  `the_mode_chip_switches_the_agent_s_mode`.

- ✅ **A start opens its tile at once, and its first message goes as the start's prompt**
  (2026-10-04, readiness N11; amended 2026-10-06 by `ui.md`, "The first message is written in
  the thread's composer": the field is the thread's own composer, and a refusal gives the
  draft back rather than taking the tile away). A start drew nothing until the machine answered, and a start
  from the palette carried no prompt, so the agent booted idle and the task was typed after.
  - The last step of "New agent…" opens the thread's tile at once, focused, with a field
    asking "What should Codex do?" and where it will run. Nothing goes to the machine until ↵.
    What was typed goes as `Start::prompt`, so the first turn begins as the agent boots, and ↵
    on nothing starts it bare. This is the empty workspace's question, in the tile itself.
  - Once sent, the tile says "Starting Codex" and where. The thread's item lands under the
    tile's own id, so the thread takes its place with nothing moving, and its composer takes
    the keyboard. A refusal or an answer with no thread takes the tile away and says why.
  - The tile is the layout's alone until then (`workspace/starting.rs`): a worker's snapshot
    keeps it, and ⌘W closes it. A link that drops after the start was sent leaves it waiting
    (since 2026-10-10; see "A start outlives its link"). A start not sent yet keeps its field.
  - Tests: `a_started_thread_opens_as_a_tile_and_a_refusal_is_said`,
    `a_start_on_its_way_closes_and_outlives_its_link` and
    `new_agent_opens_the_picker_with_the_last_choices` (the prompt) in
    `workspace/tests/thread_start.rs`.

- ✅ **Files go with a message, landed outside the work** (2026-10-04, readiness item 2). A file
  dropped or picked in the composer is uploaded first, over the ordinary transfer, to
  `~/.slopty/drop/<transfer>/`. That needs no terminal and puts nothing in the agent's working
  tree, where it would show in its diff, be committed by accident, or be lost to a `git clean`.
  `Intent::Send` then carries the worker paths, and the worker reads each file only as the
  message goes.
  - A file counts as a picture by its first bytes (PNG, JPEG, GIF, WebP) and only up to 20 MiB.
    Anything else, and a picture that cannot be read, goes by its path, so the message is never
    lost for a file. At most 32 files go with one message, each an absolute path to a file or a
    folder on the worker (a folder goes by its path), checked when the intent is decided.
  - Each agent gets the files the way it takes them. Codex gets the words, then each file's
    path, then a `localImage` per picture, which Codex reads itself. pi gets the words with the
    paths after them and each picture in `images`. An ACP agent gets each picture as an image
    block with its bytes and where it is, when it says it takes pictures (`promptCapabilities.image`).
    Every other file goes as a resource link. Claude Code's TUI is typed the words with the
    paths after them, shell-quoted, since its prompt reads a pasted path as a file.
  - The words in the thread stay as the person wrote them. The paths ride on the pending
    message, so a queued message keeps its files.
  - Tests: `pictures_are_read_whole_and_the_rest_go_by_path`, `files_and_folders_here_are_taken`
    (`slopty-worker::thread::attach`), `a_picture_goes_to_pi_with_the_message`,
    `a_picture_goes_as_an_image_and_a_file_as_a_link`,
    `files_go_by_path_and_a_fork_and_a_listing_go_to_codex`, golden `intent_send_attachments`.

- ✅ **Every thread takes files, and a thread tile takes a drop** (2026-10-05, readiness 10-05
  G3a). The worker took attachments from every adapter ("Files go with a message"), but the
  composer still refused them on a thread with no terminal, and sent the landed paths typed
  after the words, so Codex, pi and ACP never saw a picture as a picture. Now:
  - the composer sends the landed paths in `Intent::Send::attachments` for every agent and the
    words as written; each adapter gives them in its agent's form, Claude Code's TUI the paths
    after the words as before;
  - an attachment goes up to the worker of the tile the composer is on (a thread tile, or a
    terminal on its thread face), not through a terminal, so a thread with none takes one;
  - a thread tile is a drop target, and its attach button opens the picker for that tile;
  - a message of files alone goes, and a waiting message's line shows its files (the names when
    it has no words, a paperclip and how many when it has);
  - a folder goes by its path, as a terminal drop types one, rather than being refused.
  - Tests: `conversation::thread::tests::composing::a_thread_with_no_terminal_takes_files_too`,
    `a_message_waits_for_its_attachment_and_carries_its_path`;
    `workspace::tests::remote::a_thread_tile_takes_a_drop_as_an_attachment`,
    `a_picture_pasted_into_the_composer_stays_a_chip_until_sent`; worker
    `files_and_folders_here_are_taken`.

- ✅ **Past sessions per folder, from each agent's own record** (2026-10-04, readiness item 3).
  `ThreadRequest::Sessions` asks one agent's sessions in one folder and is answered with
  `WorkerMsg::Sessions`. Each session carries the words a start takes it up again with, and the
  thread already kept of it, if there is one. When nothing could be had, the answer says why in
  words instead of showing an empty list.
  - Codex is asked over its app-server (`thread/list`, by folder, last updated first).
  - An ACP agent is asked by a short run of its own (`session/list`, paged), when it lists
    sessions. A session is offered to take up again only when the agent loads sessions.
  - pi's session directory for the folder is listed. A session is called by the last name
    given it, else its first message, read only from the first and last 64 KiB of its file.
  - Claude Code's sessions are its project directory's transcript names and times alone. No
    transcript is opened: what the person said to Claude Code is theirs, and what Slopty shows
    of a session is what its own thread of it holds. The tests write their own project
    directory under a temporary home.
  - The words that take a session up again are each agent's own: `--resume <id>` for Claude
    Code, `--session <id>` for pi, and `resume <id>` for Codex and ACP. A start with them finds
    the thread kept of that session before making a new one.
  - Tests: `past_sessions_are_named_by_their_files_alone` (`slopty-agent::discover`),
    `a_past_session_is_listed_and_taken_up_again` (pi),
    `a_fork_and_a_listing_go_to_the_agent` (ACP),
    `past_threads_are_listed_with_the_words_that_resume_them` (Codex), goldens
    `client_sessions`, `link_worker_sessions` and `link_worker_sessions_absent`.

- ✅ **Fork, as each agent forks** (2026-10-04, readiness item 3). `Intent::Fork` branches a new
  thread off one, through a turn or the whole of it, and is answered once per intent like a
  start. It needs `Cap::FORK`, which a thread has when its agent can fork.
  - Codex forks over its app-server (`thread/fork`), through any turn that has ended.
  - An ACP agent forks in a fresh run of itself (`session/fork`, behind the schema's unstable
    feature, offered when the agent says it forks). The fork is then loaded, so its history is
    replayed into the new thread.
  - pi starts on a new session named by the intent, copying the old one (`--fork` with
    `--session-id`). `--fork` is passed on that first run only and never kept with the thread's
    flags, so every later run opens the copy.
  - Claude Code opens in a new terminal with `--resume <id> --fork-session --session-id <new>`.
    It needs a first message, since Claude Code writes the conversation only then.
  - pi, ACP and Claude Code copy whole sessions, so a fork from an earlier turn is refused in
    words. A fork of the whole thread records the last turn the two share.
  - Where a thread came from (`ThreadMeta::forked_from`, origin `fork`) is kept by the host as
    well as by the adapter. An agent's own account of the thread that does not say it, such as
    a reload or a worker restart, has the host's record put back.
  - Tests: `a_fork_copies_the_whole_session_into_a_new_thread` (pi),
    `a_fork_and_a_listing_go_to_the_agent` (ACP),
    `files_go_by_path_and_a_fork_and_a_listing_go_to_codex`,
    `a_fork_names_codexs_turn_and_a_forked_thread_says_where_it_came_from`,
    `a_fork_resumes_into_a_new_conversation` (Claude Code's words), goldens `intent_fork` and
    `intent_fork_whole`.

- ✅ **Codex threads at rest are let go** (2026-10-04). The worker kept every Codex thread it
  followed subscribed, so Codex kept each one loaded with its MCP servers until the worker
  restarted: about 29 MiB per thread for Slopty's relay alone (`docs/MEASUREMENTS.md`,
  2026-10-04).
  - A thread is let go (`thread/unsubscribe`) when all of these hold:
    - it has rested 10 minutes, with no turn under way;
    - nothing is asked of the person and no message is held;
    - Codex does not say it works on it;
    - no client follows it and no TUI of Slopty's runs on it.
  - Codex then unloads it after its own delay. The thread stays in the table as it was.
  - A follow, anything asked of the thread, or Codex saying it works on it again takes it up
    again with `thread/resume`. What was asked meanwhile goes once it is followed. Codex
    saying it unloaded the thread takes nothing up again.
  - Tests: `a_rested_thread_nobody_follows_is_let_go_and_taken_up_again` (stand-in daemon)
    and `what_rested_codex_threads_keep_followed_and_let_go` (the real `codex app-server`,
    run by hand).
  - **Across a worker restart** (2026-10-12, readiness 10-12 rank 8). Which threads were let
    go lived only in memory, and on joining Codex the worker took up only those Codex listed
    as loaded. A thread let go before a restart stayed in the table, but a follow did nothing
    and a message to it was dropped after its intent was answered Done. Now the worker reads
    Codex's loaded list before it acts on any ask of a thread: every Codex thread the table
    keeps that is not loaded counts as let go, so a follow or a message takes it up again
    (`thread/resume`) first, as pi's and ACP's start-up pass already did. A message that
    still cannot go (Codex holds no such thread, or it could not be taken up again) is held
    in the thread's pending list with why, for the person to send again, instead of
    vanishing. Test: `a_thread_let_go_before_a_restart_is_taken_up_again_by_a_message`.
  - **What a thread says is Codex's word, read again where it may be stale** (2026-10-15).
    Four Codex threads, two of them subagents, said "working" for hours with one empty turn
    and no title, while Codex had finished them long before. Each had been taken up again
    (`thread/resume`) while its turn ran. Codex answers that with the running turn's items
    left out, and it tells a status change or a close only to the connections subscribed to
    the thread, not to every client. For a thread Slopty itself started, a resume sent on
    `thread/started` also answered after the start's own answer and replaced what had been
    heard since. Now:
    - a resume answer for a thread already followed changes nothing;
    - a thread followed from a running turn whose items Codex left out is read whole
      (`thread/read`) once that turn ends;
    - a followed turn that says nothing for 90 s is read again, and the thread is hosted as
      Codex has it if the turn ended unheard;
    - a thread the table keeps at work that Codex no longer has loaded is taken up again as
      the worker joins Codex;
    - a thread Codex cannot take up again yet (a subagent's first turn not written) is tried
      again on a timer, and one kept at work that Codex refuses for good ends stopped.

    Codex keeps no time on an item it holds, so a thread read from history stamps the
    person's words with their turn's start and the rest with its end, never epoch zero.
    Tests: `a_thread_left_working_is_read_again_once_codex_is_joined`,
    `a_quiet_turn_is_read_again_and_ends_as_codex_has_it`.

- ✅ **A Codex thread's commands claim no Slopty terminal** (2026-10-04). Codex's shared
  daemon runs every thread's commands with its own environment, which is that of whatever
  started it. A Codex TUI typed in a Slopty shell can start it, and then every thread's tools
  carried that shell's `SLOPTY_SESSION`, its token and its project task, so Slopty's CLI in
  any Codex command spoke as that one terminal.
  - Every thread Slopty starts, takes up again or forks is now loaded with
    `shell_environment_policy.set` for those four variables, each set empty through the
    app-server's published per-thread `config`. Each is its own key, so the person's own
    policy is added to, never replaced. Set empty, they name no terminal, and the CLI speaks
    as an agent, never for the person.
  - A daemon the worker starts gets none of them.
  - Codex ignores `config` for a thread already loaded, and a thread the person's TUI starts
    gets none. So a `codex` typed in a Slopty shell runs without the four variables too: the
    shell integration wraps it in `env -u` in zsh, bash and fish, as it wraps `claude`, and the
    daemon it starts has no terminal to lend. A `codex` of the person's own is left alone.
    Test: `a_typed_codex_runs_as_no_terminal` (a stand-in `codex`).
  - Tests: the start, resume and fork parameters in
    `a_start_asks_the_persons_codex_for_a_thread_and_sends_its_first_turn`,
    `a_rested_thread_nobody_follows_is_let_go_and_taken_up_again` and
    `files_go_by_path_and_a_fork_and_a_listing_go_to_codex`. The measurement above checks
    that the real `codex app-server` takes the `config`.

- ✅ **A turn a usage limit stopped fails until the limit resets** (2026-10-04). A limit used
  to end Claude Code's turn as done, and Codex's limit error was a plain API error.
  - The turn's state `Failed` carries `until_ms`: when the agent can go on, where it is known.
    Its notice is of kind `limit`.
  - Claude Code records the API error it gives up on with its kind (`error: rate_limit`) and,
    for a usage limit, the quota it hit (`quotaLimits.resetsAt`). The decoder keeps both on
    the note (`Stop`). A turn whose last word is that error fails with its text. A turn the
    model went on in after an error completes. A limit with no quota recorded resets when the
    status line's full window does.
  - The main thread's done after a failed turn is a failure (`Phase::Failed`), whichever of
    the hook and the transcript is heard first.
  - The terminal's agent status says it too: `StopFailure` is `AgentStatus::Failed { error,
    until_ms }`, at rest like done and with the same attention. For a `rate_limit`, `until_ms`
    is when the full windows the status line last showed reset. Every reader that waits for an
    agent at rest takes it as one. Tests: `a_stop_failure_fails_the_turn_until_a_limit_resets`,
    `the_status_maps_to_a_phase`, and the golden `worker_agent_failed`.
  - Codex's `usageLimitExceeded` and `rateLimitExceeded`, once Codex gives up, fail the turn
    until its full window resets (`account/rateLimits/updated`).
  - Tests: `a_turn_a_usage_limit_stopped_fails_until_it_resets` (Claude Code and Codex) and
    `a_stopped_turn_fails_by_what_stopped_it`.

- ✅ **A queued message can be moved in the list or sent now** (2026-10-04, from the T3 Code
  study's queue controls). *`Reorder` was deleted the same day ("Sleep, waits on another
  thread, queue reordering and edited allows are gone"); `Promote` stays, and now reads
  `Cap::QUEUE`, as the tray's "Send now" does; the tests named below predate the deletion.*
  Two intents on the thread wire, as built:
  - `Reorder { pending, before }` moves a message that has not gone to just before another,
    or to the end (`Cap::QUEUE`). Queued messages go in the list's order. One rule serves every
    adapter (`Pending::reorder`): a message or mark no longer in the list is refused and moves
    nothing.
  - `Promote { pending }` sends a queued message now, as a steer would go (`Cap::STEER`). For
    Claude Code it becomes a steer in the list and is typed at the next step the guard allows.
    For Codex it leaves the queue and goes by `turn/steer` into the turn under way, or as a
    turn of its own when none runs. ACP takes no steer, so it is unsupported there. A message
    being typed, or already in Claude Code's terminal, is refused.
  - Tests: `a_queued_message_moves_in_the_list_or_goes_now` (the composer on a real
    terminal), `a_held_message_moves_in_the_queue_or_steers_the_turn` (Codex),
    `an_acp_thread_runs_turns_and_asks_before_it_acts` (ACP), and
    `a_message_moves_before_another_or_to_the_end` with the property `a_reorder_is_a_move`.

- ✅ **The person's past prompts are searched on each machine, and a hit takes its session up
  again** (2026-10-04, from the Ghostex study's first idea). `ThreadRequest::Sessions` grew
  rather than a second request: an agent and a folder are each optional, and it carries the
  words to find. With no words and both named, the agent lists its own sessions as before.
  Otherwise the worker answers from the person's prompts as each agent records them, best match
  first, and each session comes with the prompts that matched (`PromptHit`: the prompt cut to
  600 bytes round its first match, the matches as byte spans) and the words that take it up
  again. With no words, the sessions come prompted last first, each with its last prompt. A
  client can ask every machine and merge the answers by time.
  - **What is read.** Claude Code's own prompt history (`~/.claude/history.jsonl`), with a long
    paste put back from the line or from `paste-cache/<hash>.txt`. This widens the earlier rule
    that no transcript is opened: the history is the record Claude Code keeps of what the person
    typed for its own ↑ recall, and transcripts are still never read. A session's transcript is
    looked at, never opened, to tell whether it can be taken up. Codex's prompt history
    (`$CODEX_HOME/history.jsonl`) holds what its TUI was sent; the folder comes from the first
    line of the thread's rollout. A thread another client of Codex's app-server ran is in no
    history, so its prompts are read from its rollout: the `user_message` events where it has
    them, else the `user` input items without the blocks Codex adds before a first turn. `exec`
    runs and subagents are left out, as nobody's prompts. pi's prompts are the person's
    messages in its session files. ACP agents keep no prompt record Slopty can read, so a search
    of one says so in words.
  - **What is no prompt.** A slash command on one line (`/model opus`) is left out; a path that
    begins a prompt is not one.
  - **The match.** Each word must be in the prompt as written, case ignored unless a word has a
    capital, accents ignored (`nucleo-matcher` substring atoms). Prompts are prose, so fuzzy
    letters in order would find nearly every long prompt for a short query. A word at a word's
    start scores above one inside a word; equal scores go to the newer prompt. Each prompt keeps
    its text folded as the matcher reads it, and one that lacks a word is passed over before it
    is scored: a warm search for a rare word went from 45 ms to 3–5 ms on this machine.
  - **Bounded and cached.** The worker keeps every record it read in memory with how far it read
    it. The records are only appended to, so a later search reads only the new whole lines, and
    a file replaced at its path is read again. A file is read from at most its last 64 MiB, a
    search reads at most 512 MiB and for 3 s, and a prompt is kept to 32 KiB. What a search
    skipped is said in `PastSessions::cut`, and the next one reads on. The numbers on this
    machine are in `docs/MEASUREMENTS.md`, "Prompt search".
  - **Resume.** A hit carries the words its agent's own resume takes (`--resume <id>`,
    `resume <id>`, `--session <id>`) and the thread kept of it, if one is, so opening a hit goes
    through the start path that already takes a session up again. A Claude Code session whose
    transcript is gone, or a Codex thread with no rollout, has no words and is shown only.
  - **Asked from the session step** (2026-10-06, journeys audit J3). "Resume a past session…"
    lists the machine's sessions with no words, and what the person types finds among them at
    once. Until then the step never sent the words, so a session older than the 50 listed could
    not be found by what was asked in it. Now, once the field rests (`find::ASK_AFTER`, 120 ms,
    two characters or more), the machine is asked for the words too. The sessions it finds lead
    the step, followed by the listed ones they leave out. The rules are the palette's Threads
    section's (`docs/decisions/workspace.md`): an answer for words the field no longer says is
    dropped, new words drop the ask still waiting, the line the person is on stays chosen, and
    a machine out of reach by then is not asked. Test:
    `workspace::tests::thread_start::a_past_session_is_found_by_what_was_asked_in_it`.
  - Every machine with the agent is asked at once since 2026-10-09 (`docs/decisions/workspace.md`).
  - Tests: `claude_codes_history_gives_prompts_with_their_pastes`,
    `codex_gives_prompts_from_its_history_and_rollouts`, `pi_gives_prompts_from_its_session_files`,
    `a_slash_command_is_no_prompt` (`slopty-agent::history`);
    `a_record_is_read_on_from_where_it_stopped_and_again_when_replaced`,
    `sessions_rank_by_their_best_prompt`, `a_bounded_search_says_what_it_skipped_and_reads_on`,
    `an_agent_without_a_record_says_why` (`slopty-worker/tests/history.rs`);
    `past_prompts_are_found_across_agents_with_the_words_that_resume_them` (the worker daemon);
    goldens `client_sessions_search` and `link_worker_sessions_found`.

- ✅ **A server task's thread starts at its seat, through each agent's own door** (2026-10-04,
  for the orchestrator's `StartThread`). The server picks a seat (a session id) for the task and
  hands it with the task's variables and a role (`TaskThread`). The worker starts the thread as
  `Seated`: the intent is derived from the seat, so a start repeated after a dropped link
  answers with the thread the first one started. The thread's row carries the seat as the fact
  `slopty.seat`, which the host re-applies over whatever the adapter later says of the meta, so
  it survives a restart and a fresh read from the agent.
  - **The variables.** The seat's variables are the server's (project, task) and the worker's
    for that seat (the server, the seat as `SLOPTY_SESSION`, its token), built by
    `Worker::seat_env` exactly as a terminal opened at the seat gets them.
  - **Each agent's door.**
    - Claude Code runs in a terminal opened under the seat id, so its hooks and tools are the
      seat's as for any terminal. The role goes as `--append-system-prompt`.
    - Codex gets the variables in the commands it runs (`shell_environment_policy.set`) and
      Slopty's tools as an MCP server in the `thread/start` config. The values go in the
      server's own `env`, since the daemon's environment belongs to no seat. The role goes as
      `developerInstructions`, and both are given again on `thread/resume`.
    - ACP gets `slopty mcp` with the variables in the `mcpServers` of `session/new`, `load` and
      `fork`; every agent must take stdio. ACP has no system prompt, so the role goes ahead of
      the first message.
    - pi runs with the variables. The gate registers Slopty's tools when the worker hands them
      over in `SLOPTY_PI_TOOLS` (`pi.registerMcpServer`). The role goes as
      `--append-system-prompt`, kept among the thread's own flags so a pi started again has it.
  - **Ending.** `TaskThreads::close(seat)` ends the seated thread through its adapter, or closes
    its terminal, and says false when no thread sits there.
  - **Reports.** A seat with no terminal of its own takes the server's delivered reports as a
    message (`Intent::Send`), queued where the agent queues and steered where it does not, once
    per batch. It is acknowledged only when the message went.
  - **Kept.** The host keeps the seat in the thread's own directory (`seat.json`): the seat, the
    server's variables, the role and the CLI. A thread taken up again after the worker restarts
    is given the same, and a woken Claude Code reopens under the seat. The worker's own variables
    for the seat, the token among them, are never written: the host adds them where the agent
    runs (`Host::env_of`, through the daemon's `Worker::seat_env`), and clients never see the
    file. `Host::reset` keeps the seat fact too, so a thread read again from its agent still
    names it.
  - Tests: `a_seated_start_opens_claude_under_the_seat_with_its_role`,
    `a_seated_start_gives_codex_the_seat_its_tools_and_its_role`,
    `a_seated_acp_thread_gets_the_seat_its_tools_and_its_role`,
    `a_seated_pi_thread_gets_the_seat_and_its_role` (`slopty-worker/tests`);
    `a_seated_pi_runs_with_the_seat_and_its_tools` (`slopty-worker::thread::pi`);
    `a_seats_thread_is_loaded_with_its_variables_and_slopty_tools` and
    `the_tools_are_an_mcp_server_entry_the_gate_registers` (`slopty-agent`).

- ❌ **The person puts an agent to sleep at rest, and wakes it on its own session** (2026-10-04,
  from the Ghostex study's second idea). *Deleted the same day: see "Sleep, waits on another
  thread, queue reordering and edited allows are gone" below.* The item registry's `sleeping` flag only ever released
  a client's view, and nothing set it, so it is gone. In its place, `Intent::Sleep` and
  `Intent::Wake` act on a thread (`Cap::SLEEP`), and a thread put to sleep reads as
  `Liveness::Asleep`.
  - **Sleep is the person's word.** Nothing sleeps an agent on its own: no idle sweep, no lease.
  - **The gates.** The host decides under its lock, so no message slips in between
    (`slopty_worker::thread::sleep::refusal`). It refuses, in words:
    - an agent that is not running, or that waits on its own wakeup;
    - a thread never asked anything;
    - a turn under way, or the agent waiting on its own work;
    - an open request;
    - a message waiting to go;
    - background work still running;
    - a message scheduled for the thread (it would miss it).
  - **How each agent ends.** Each ends the way it ends on its own, and the thread is kept:
    - pi's and an ACP agent's stdin are closed;
    - Codex lets the thread go at once (`thread/unsubscribe`);
    - Claude Code's terminal is closed. Its session is written as it goes, as when the person
      closes the window.

    A thread whose Codex TUI runs in a Slopty terminal is refused. The adapter then tells the
    end as it tells any exit, and the host tells it as the sleep. The sleep is kept through a
    restart and through the adapter telling it gone again, until the agent runs again.
  - **Wake.** A wake takes the session up through each agent's own resume:
    - pi runs on its `--session-id` and reads the session again;
    - an ACP agent loads the session (`session/load`), so it has the cap only when it loads
      sessions;
    - Codex resumes the thread (`thread/resume`);
    - Claude Code runs `--resume` in a new terminal, under the seat for a task's thread.

    The next message wakes a driven agent too, and following a Codex thread already takes it
    up again.
  - **On the ladder.** An asleep thread stands on `Rung::Sleeping`, below Idle, and is counted
    apart (`Counts::sleeping`). It is said quietly ("Asleep") and never as needing the person.
    What it left that asks for a look still ranks it: changes to review, or its error.
  - **Not yet.** The thread view's door for an asleep thread (Wake beside Resume) is the
    client's half.
    The daemon's Claude Code sleep (its terminal closed) has no daemon-level test yet.
  - Tests: `only_an_agent_at_rest_with_nothing_under_way_sleeps` (`thread::sleep`);
    `an_agent_put_to_sleep_ends_asleep_until_it_runs_again` and
    `a_seat_outlives_a_read_again_and_a_restart` (`slopty-worker/tests/threads.rs`);
    `a_pi_put_to_sleep_is_woken_on_its_session`,
    `an_acp_agent_put_to_sleep_is_woken_by_loading_its_session`,
    `a_thread_put_to_sleep_is_let_go_and_woken_by_resuming_it`;
    `an_agent_put_to_sleep_stands_below_one_at_rest` (`slopty-proto`); goldens `intent_sleep`,
    `intent_wake`, `frame_actions` and `attention_ladder`.

- ✅ **The person schedules a message for a time, or for when another thread rests** (2026-10-04,
  from the Ghostex study's seventh idea). *`Delivery::After` was deleted the same day; `At` stays
  for "Continue at" a limit's reset. See "Sleep, waits on another thread, queue reordering and
  edited allows are gone" below.* Two deliveries join the two a message already has:
  `Delivery::At { at_ms }` and `Delivery::After { thread, settle_ms }`. They need
  `Cap::SCHEDULE`, which every adapter has, since the worker holds the message and not the agent.
  - **Kept on the worker.** A scheduled message is a pending one, so the thread view already
    lists it, edits it and takes it back. The host holds the scheduled ones apart from what any
    adapter knows, and puts them back in every pending list an adapter tells. They are in the
    thread's log, so they outlive a restart. Withdraw, Edit and Promote of a scheduled message
    are the worker's to act on and never reach the agent; Promote means send it now. Reorder is
    refused, since each goes at its own moment.
  - **Its moment.** `At` goes at its time. `After` goes once the thread it waits on has been at
    rest for the settle, measured from when it came to rest. At rest means its turn ended,
    nothing asks the person, and it has no wakeup or work of its own under way. Ghostex settles
    for 10 s; the client picks. A thread already at rest counts from when it came to rest, so a
    message after a thread that will not work again still goes. A message cannot wait on its own
    thread, and one whose thread is gone is held, saying so.
  - **How it goes.** A task on the worker (`schedule::spawn`) wakes when a schedule or the
    thread table changes, and at the next due time. It sends each due message the way a
    client's goes: through the daemon's intent path, as the person's own message, queued where
    the agent queues, else as a steer. It is sent as an intent derived from the one that
    scheduled it, so it goes once. One the agent takes leaves the list. One it turns down stays,
    held with the agent's reason.
  - **Never twice.** A message is marked as going, in the log, before it is sent. One the worker
    stopped in the middle of sending comes back held ("It was being sent when the worker
    stopped, so it may have gone") for the person to take back or send again, never sent on its
    own.
  - **The client's half.** The composer sets a time only as "Continue at" a usage limit's reset
    (`conversation/thread/view/later.rs`), the scope kept when `After` and "Send later…" went.
    Test: `a_thread_a_limit_stopped_continues_when_it_lifts`
    (`conversation::thread::tests::composing`).
  - Tests: `a_message_goes_at_its_time` (`thread::schedule`);
    `a_scheduled_message_waits_on_the_worker_and_outlives_a_restart` and
    `the_worker_sends_a_scheduled_message_at_its_moment` (`slopty-worker/tests/threads.rs`);
    golden `intent_send_at`.

- ✅ **A thread goes on in a new one, on another agent or on its own afresh** (2026-10-04, R8 of
  the T3 Code orchestrator study, after T3's budgeted context handoff and Amp's Handoff). *The
  account and the draft went the same day; the new thread's composer opens on a pointer back.
  See "Dollar cost, the carried account and drafts on the worker are gone" below.*
  `Intent::Continue { agent }` needs `Cap::CONTINUE`, which every adapter has, since the worker
  makes the account from the thread it holds and not from the agent. Fork stays the agent's own
  branch of its session; Continue is the portable way, for any pair of agents.
  - **A visible first message, never a hidden prompt.** The new thread starts with nothing
    sent. Its first message waits on the worker as a draft (`Delivery::Draft`), which the
    person reads, changes (Edit), sends (Promote) or drops (Withdraw). A draft is kept like a
    scheduled message, in the thread's log through a restart, but it never goes on its own and
    does not keep the agent from sleep. A client may keep a draft of its own the same way.
    Slopty's rule that the agent's session is the record holds: switching agents is a new
    thread with its own session, never words slipped into an old one.
  - **The account** (`slopty_agent::handoff::render`) is made by rule, not by a model, so the
    same thread always gives the same words. It opens by saying whose conversation it was and
    where, and that what follows is context rather than a request. One line says how much it
    holds. Then come the plan, the files changed and the commands run with how they ended, each
    newest first within an eighth of the budget, and together within half of what is left. The
    person's messages with their final answers fill the rest, newest first: the newest is cut
    short if it alone is too long, and older ones go whole or not at all. The budget is 32 KiB.
    A property test holds it to the budget for any thread.
  - **Where it came from.** The new thread's `forked_from` names the old one through its last
    turn, so the lineage reads as a fork's does. The old thread goes on as it was.
  - **Once.** The start is once per intent, as a client's start is. A repeat starts nothing and
    finishes what the first may not have, the draft and where it came from, both kept once.
  - **The client's half** is deleted (2026-10-10): no client door asks `Intent::Continue`
    since "Branch from here" became Fork from here and Ask aside (`ui.md`), and the wire went
    with it ("Continue, the Codex goal and interrupt-and-send leave the wire", below). A seated thread's seat does not go with it.
    A project task starts fresh or goes to another agent from its board: see "A task starts
    fresh, or goes to another agent" in `docs/decisions/projects.md`.
  - Tests: `a_thread_is_told_whole_when_it_fits`,
    `the_newest_message_stays_when_the_budget_is_tight` and the property test
    `an_account_keeps_within_its_budget` (`slopty_agent::handoff`);
    `a_claude_code_thread_goes_on_in_pi_from_a_draft_the_person_sends`
    (`slopty-worker/tests/pi.rs`); `only_an_agent_at_rest_with_nothing_under_way_sleeps` for
    the draft; goldens `intent_continue` and `intent_send_draft`.

- ✅ **A message sent now to an agent with no steer of its own stops its turn** (2026-10-04, R13
  of the T3 Code orchestrator study, after T3's steering restart and ACP agents steered by
  cancel and resend). `Delivery::Interrupt` needs `Cap::INTERRUPT` and `Cap::QUEUE`. It is for
  an agent without `Cap::STEER`, today any ACP agent, whose message otherwise waits for the turn
  to end.
  - **Through the agent's own doors, on the person's word.** The ACP adapter queues the message
    first, under the person's intent, so the pending list and the turn it starts name it as
    theirs, and stops the turn under way, when there is one, in the same step
    (`ThreadAsk::Interrupting`; the worker's earlier three-step `thread::steer` went with
    `Intent::Reorder`, see "Sleep, waits on another thread, queue reordering and edited allows
    are gone"). The stopped turn ends as the agent ends it, and the queue sends the message
    next. Nothing is typed into a screen and nothing is answered for the person: the open request
    is withdrawn by the agent's own cancel.
  - **No race to lose.** The stop is the agent's to make. A turn that ends before the stop lands
    leaves the message to go as a queued one does, so it goes once either way. Orchestration's
    intents take the same path.
  - **The client's half** is deleted (2026-10-10): "Interrupt and send" left the composer
    with the rest of the single-agent chrome (`ui.md`, "The thread keeps only what directs
    it"). The wire followed the same day ("Continue, the Codex goal and interrupt-and-send
    leave the wire", below).
  - Tests: `a_message_sent_by_interrupt_stops_the_turn_and_goes_next`
    (`slopty-worker/tests/acp.rs`); golden `intent_send_interrupt`.

- ✅ **Model, effort and mode are switched through each agent's own settings door** (2026-10-04).
  `Intent::SetEffort { effort }` joins `SetModel` and `SetMode`, with `Cap::SET_EFFORT`. A thread
  says what it can be switched to: `ThreadMeta::models`, `ThreadMeta::efforts` (new, each an
  `Effort { id, label, description }`) and `ThreadMeta::modes`. Each list is the agent's own, so
  no client keeps one, and a value it does not offer is refused before anything goes.
  - **Codex, through `thread/settings/update`.** The setting holds for the thread's next turns,
    the TUI's included, so a switch here and one in the TUI are the same switch. Codex then tells
    every client what it holds (`thread/settings/updated`, read now instead of passed over), so a
    switch made in the TUI shows here too.
    - The models are Codex's `model/list`, asked once as the worker joins the daemon, every
      page, hidden models left out. A model is named by its slug (`model`), which is what the
      thread runs, and shown by its display name.
    - The efforts are the running model's `supportedReasoningEfforts`. A model that lacks the
      thread's effort goes with its own `defaultReasoningEffort`, as Codex's own picker sets it.
    - The modes are the three approval policies, `untrusted`, `on-request` and `never`.
      `granular` is shown when Codex says it and is never offered. The sandbox is not a mode:
      it stays as the person's Codex configuration sets it.
    - A per-turn override on `turn/start` was the other door. It was not taken, because it
      reaches only Slopty's own next turn and leaves the TUI and the meters on the old values
      until then.
    - Codex's collaboration mode (plan or default) is a second axis beside the approval policy.
      It is experimental in the pinned build and stays unswitched for now.
    - A switch Codex refuses leaves the meters as they were and says why in the thread
      ("Codex didn't switch: …"), since the intent was done once it went.
  - **pi, through its RPC.** The efforts are `get_available_thinking_levels`, asked with the
    state and the models, and again after a model switch, since pi clamps the level to the new
    model. A model that does not reason has only `off`, which offers nothing to choose. An effort
    goes as `set_thinking_level`, and the meter follows pi's `thinking_level_changed`.
  - **ACP, through the session's config options.** The efforts are the values of the option
    whose category is `thought_level`, with their descriptions, and an effort goes as
    `session/set_config_option`. The agent answers with the options as they now stand. The cap
    is there only while the agent offers the option, as it is for models and modes.
  - **Claude Code, observed: the model only.** `/model <id>` stays: it is the TUI's own typed
    command, sent on the person's word through the composer's guard. The pinned 2.1.283 also
    lists `/effort` and `/plan`, but neither is a whole switch. `/effort` takes no argument and
    opens a menu, and `/plan` only enters plan mode, while leaving it or reaching any other mode
    is Shift-Tab's cycle. Driving either means keys into a menu or a mode cycle, which Slopty
    never types, so an observed thread has no `SET_MODE` and no `SET_EFFORT`. Its mode and
    effort stay read-only meters.
  - **The client's half.** The levels in the model chip's menu, under its models, and the
    level after the model's name (`conversation/thread/view/composing.rs`). Test:
    `the_model_chip_switches_how_hard_the_model_thinks` (`conversation::thread::tests::face`).
  - Tests: `a_thread_switches_among_what_codex_offers` and
    `the_threads_settings_are_its_mode_and_effort` (`slopty-agent/tests/codex.rs`);
    `a_switch_goes_to_codex_as_the_threads_settings` (`slopty-worker/tests/codex.rs`, a
    stand-in daemon that takes and refuses switches);
    `a_pi_threads_effort_is_set_among_the_levels_pi_offers` (`slopty-worker/tests/pi.rs`);
    `an_acp_threads_effort_is_set_through_its_thought_level_option`
    (`slopty-worker/tests/acp.rs`); golden `intent_set_effort`.

- ❌ **Deleted 2026-10-14: the thread search below.** Its views went with the tiling workspace and
  no client asked for it any more, so the request, the answer and the worker's side are
  deleted (`ui.md`, "Code nothing reaches is deleted"). The ruling is kept as the record.
- ✅ **What was said in the threads is searched on the worker that holds them** (2026-10-04,
  `crates/slopty-worker/src/thread/search.rs`). `ThreadRequest::Search { query, limit }` is
  answered with `WorkerMsg::ThreadHits` on the control stream.
  - **What is searched.** Each thread's state as the worker's log keeps it: the person's
    messages, the agent's answers and reasoning, its calls' titles and its notices, in every
    thread held, a subagent's and an exited one's among them. A text the log keeps clipped is
    searched as far as it is kept. The rest is the agent's own session's, and reading every
    agent's session files for a palette keystroke would cost more than it finds. Past prompts in
    sessions the worker does not hold stay `ThreadRequest::Sessions`'s.
  - **The match is the prompt search's.** Every word in one item, case ignored unless a word
    has a capital, accents ignored, through the same matcher (`nucleo-matcher` substring atoms).
    A whole word ranks above one inside another word. The excerpt is cut round the first match
    the same way too: `history::excerpt` serves both. A thread ranks by its best item, then by
    its newest match.
  - **Lean answers.** A hit names its thread, item and turn, so a client scrolls to it, and
    says what kind of words matched (open: `person`, `agent`, `reasoning`, `tool`, `notice`).
    Title, agent and folder are the thread's row in the table the client already holds, so they
    are not sent again. At most `SEARCH_THREADS` threads, `HITS_PER_THREAD` items each and
    `ITEM_HIT_BYTES` of each, with counts of what was left out.
  - **No adapter waits on a search.** It runs on the blocking pool, and the host's lock is
    taken one thread at a time (`Host::visit`).
  - **The client's half.** The palette's Threads section, which asks every linked machine
    (2026-10-06, `docs/decisions/workspace.md`, "The palette finds words said in any thread, on
    every machine").
  - Tests: `every_word_is_found_in_what_was_said_the_best_and_newest_first` and
    `a_hit_shows_its_match_and_the_limit_counts_what_it_left_out` (`thread::search`); goldens
    `client_thread_search` and `link_worker_thread_hits`.

- ✅ **Sleep, waits on another thread, queue reordering and edited allows are gone** (2026-10-04,
  the session's cut list, from the feature audit and the orchestrator study). The user's
  standing rule is that nothing is hidden: a feature nobody reaches for, or that isn't worth its
  place, is deleted, and pre-release nothing is kept for compatibility.
  - **Sleep and wake.** Nothing ever sent `Intent::Sleep`, and settling a finished task's agent
    (`hub/settle.rs`) already frees a resting one. `Intent::Sleep`, `Intent::Wake`,
    `Cap::SLEEP`, `Liveness::Asleep`, `Rung::Sleeping` with `Counts::sleeping`, the worker's
    `thread/sleep.rs` and the host's sleep marking, each adapter's sleep and wake, and the
    thread view's Wake door are deleted. Resume stays: an exited agent goes on through its own
    resume, by the next message or the Resume strip. Codex's own let-go of a resting thread
    (`thread/unsubscribe`, taken up again on a follow or an ask) is not sleep, and stays.
  - **`Delivery::After`.** A message that waits for another thread to rest went with the
    general "Send later…" menu. `Delivery::At` stays, for "Continue at" a usage limit's reset,
    with drafts. The scheduler now only watches times (`schedule::when`).
  - **`Intent::Reorder` and `Pending::reorder`.** Nobody reordered a queue, and both studies
    agreed. The one inner use, a message sent by interrupt going first, now belongs to the one
    adapter that needs it. An agent that steers takes "now" as a steer (the daemon turns
    `Delivery::Interrupt` into `Delivery::Steer` where the thread has `Cap::STEER`). An ACP
    agent, which has no steer, puts the message first in its own queue and cancels the turn
    under way in one step (`ThreadAsk::Interrupting`). `thread/steer.rs`, which reordered
    through the wire intent, is deleted.
  - **The "Edit…" allow.** `Verdict::AllowEdited`, `Editable`, `Request::editable`,
    `PermissionPrompt::editable` and the relay's edited `updatedInput` are deleted. Deny with a
    reason stays, and so do `updatedInput` answers to `AskUserQuestion` and `ExitPlanMode`,
    which are the person's answers rather than an edit.
  - Tests kept or reworked: `what_a_resting_thread_left_ranks_it_above_rest` (`slopty-proto`),
    `a_message_goes_at_its_time` (`thread::schedule`),
    `a_scheduled_message_waits_on_the_worker_and_outlives_a_restart` and
    `the_worker_sends_a_scheduled_message_at_its_moment` (`slopty-worker/tests/threads.rs`),
    `a_message_scheduled_for_a_time_goes_to_pi_at_its_time` (`slopty-worker/tests/pi.rs`),
    `a_message_sent_by_interrupt_stops_the_turn_and_goes_next` (`slopty-worker/tests/acp.rs`),
    `a_queued_message_promoted_goes_now` (`slopty-worker/tests/compose.rs`) and
    `a_held_message_promoted_steers_the_turn` (`slopty-agent/tests/codex.rs`).

- ✅ **The person's stop holds what is queued until they speak again** (2026-10-04, A1 of the
  T3 Code delta study, after T3's "Queue paused" once Stop is pressed). Before this, a queued
  message went as soon as the stopped turn ended, so Stop with a message queued started it.
  - **Held, in words.** `Intent::Interrupt` holds every queued message not yet on its way as
    `PendingState::Held { reason: Pending::STOPPED }` ("Held since you stopped the turn",
    `Pending::hold_for_stop`). The person's next send releases them in order, behind it or
    after the new message as each agent queues. `Promote` sends one now and lets the rest go
    behind it. `Withdraw` and `Edit` work as before. A turn the agent ends by itself holds
    nothing.
  - **Each adapter's own queue.**
    - Claude Code (`thread/compose.rs`): the composer holds the worker's queue and skips a
      stopped message. One already in the terminal (typed, or taken back) is not the
      worker's to hold.
    - Codex (`Shared::stop`): the interrupt and the hold are one step, and `next_queued` waits
      while the front is held.
    - ACP (`Session::hold_queue`, `release_queue`): the same, in the adapter's own queue.
    - pi has no `Cap::QUEUE`, so there is nothing to hold.
  - **`Delivery::Interrupt` is exempt.** Its message is meant to go next. It releases what a
    stop held and goes first. Where the thread has `Cap::STEER`, the daemon now sends it as a
    steer.
  - No wire change: `PendingState::Held` already existed, and the view already draws a held
    message.
  - Tests: `a_stop_holds_the_queue_until_the_person_sends_again` in
    `slopty-worker/tests/compose.rs` (Claude Code), `slopty-worker/tests/acp.rs` (a stub ACP
    agent) and `slopty-agent/tests/codex.rs` (Codex).

- ✅ **A Codex goal shows as Codex holds it, read-only** (2026-10-04, A11 of the T3 Code delta
  study; T3's #6777 and #15133, and #7935 for why goal controls stay out). *Deleted
  2026-10-10 with the client's reader: see "Continue, the Codex goal and interrupt-and-send
  leave the wire".*
  - **The model.** `ThreadState::goal` is an open `Goal`, set by `Action::GoalSet`. It holds the
    objective, the state in open words (`active`, `paused`, `blocked`, `usage-limited`,
    `budget-limited`, `complete`: Codex's names in kebab case), the tokens used against the budget, the
    time used and when it changed.
  - **From Codex.** The adapter reads `thread/goal/updated` and `thread/goal/cleared`. On a
    resume it asks `thread/goal/get`, so a goal set while Slopty was away still shows.
  - **Setting, pausing or clearing a goal stays in Codex's TUI.** An active goal means Codex may
    start turns by itself after a stop. The thread view says so at its Stop door rather than
    fighting it. The turns Codex starts are seen through `turn/started` as any turn is, so
    T3's #15133 (a goal thread shown as done while it ran) does not arise.
  - Generated subset: `thread/goal/get`, `thread/goal/updated` and `thread/goal/cleared`
    (`cargo xtask codex schema`, Codex 0.160.0).
  - Tests: `a_codex_goal_shows_as_codex_holds_it` (`slopty-agent/tests/codex.rs`); `GoalSet` in
    the `golden_thread` actions.

- ✅ **An archived Codex thread is unarchived and taken up again, once** (2026-10-04, A8 of the
  T3 Code delta study, after T3's `44bd4c9c` (#15389), which closes #10481).
  - A Codex thread archived outside Slopty, by Codex's own TUI or `codex archive`, makes
    `thread/resume` fail.
  - On that refusal only, the worker calls `thread/unarchive` for the same native id and then
    repeats the identical resume once (`Waiting::Unarchive`).
  - Codex says the thread is archived only in words: "session <id> is archived", or its hint to
    run `codex unarchive`. They are read as T3 reads them (`thread::codex::archived`).
  - Any other refusal stays a refusal. A second one after unarchiving is told as it comes. No
    fresh session is ever started in its place, which is the fallback T3 kept.
  - Not measured against a signed-in Codex, which no test runs. The words are T3's, and they
    are taken from Codex's own message.
  - Tests: `an_archived_thread_is_unarchived_and_taken_up_again` (`slopty-worker/tests/codex.rs`,
    a stand-in daemon that refuses until unarchived); `only_an_archived_thread_is_unarchived`
    (`thread::codex`).

- ✅ **Ask aside: a side question on a fork that goes away** (2026-10-04, A9 of the T3 Code
  delta study; T3's most-upvoted idea, #7311, never merged there).
  - **The wire.** All three intents need `Cap::FORK`.
    - `Intent::Aside` forks the whole thread as `Intent::Fork` does, so Claude Code's
      `--fork-session` keeps the prompt cache. It answers `Started { thread }`.
    - The new thread carries the open fact `slopty.aside` (`ThreadMeta::ASIDE_FACT`, read by
      `aside_of`), naming the thread it was asked beside. The client sends the question to it
      as its first message.
    - `Intent::KeepAside` drops the fact, so the thread becomes an ordinary thread of its own.
    - `Intent::Discard` ends the aside's agent as a settled task's is ended, and the worker
      forgets the thread and its log. It is answered once with the worker's starts, since the
      thread is gone after. (Since 2026-10-09 it closes any thread; see "Keep this run".)
  - **The agent's session stays** wherever the agent keeps it, as every session does. Nothing
    is written into the main thread's session.
  - **Not sleep.** The study's sketch put a closed aside to sleep. Sleep was deleted the same
    day, so a closed aside ends.
  - **Kept by the worker.** The fact is the worker's own (`Own::aside`) and is put back on every
    meta, so an agent's rename or resume does not drop it. It outlives a worker restart.
  - **The client's half.** The sheet over the thread, and passing over `slopty.aside` threads
    in the navigator, the inbox and the attention ladder, belong to the UI and server lanes.
  - Tests: `an_aside_keeps_its_mark_until_it_is_kept_or_forgotten`
    (`slopty-worker/tests/threads.rs`); goldens `intent_aside`, `intent_discard`,
    `intent_keep_aside`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **Claude Code's prompts are held for thread follows, and the conversation stream is gone**
  (2026-10-04, item 7 of the readiness audit). Every agent reaches a client as a thread, so the
  old Claude Code follow stream had no reader left.
  - **Who a prompt is held for.**
    - A client that follows the session's thread (`ThreadRequest::Follow` of a thread with a
      terminal) holds every prompt there.
    - A client that keeps the thread table (`ThreadRequest::Table`) holds a yes or no, for
      `APPROVAL_HOLD` at most, since every thread's requests show in its rows (since
      2026-10-10 a plan and a question too; see "A plan or a question waits for the Mac's
      approvers"). That is a
      notification's Allow, or the inbox. It took `ConversationRequest::Approvals`, which every
      app client sent anyway.
    - Orchestration follows as before.
    - The machine (`slopty_worker::conversation::Holds`) is unchanged but smaller: nothing is
      shown per link any more, so `shown_to`, `tells` and `stop_approving` are deleted.
  - **How it is shown.** The hold (`apps/slopty-worker/src/threads/hold.rs`) tells the
    session's thread observer directly (`thread::claude::Driver::permission`), in the order it
    decides, and the observer opens or settles the request on the thread.
    - That replaces the daemon broadcast, where a lagging receiver could drop a prompt.
    - A prompt for a terminal nothing observes yet starts observing it, as it did.
  - **Answers.** Answers go only through `Intent::Answer`, and the hand-back to the TUI only
    through `Intent::Release`, both on the thread's request.
    - The last follower letting the thread go, its connection ending, the wait running out or
      the relay going away still hand a prompt back undecided.
  - **Deleted.**
    - From the wire: `ConversationRequest`, `ConversationEvent`, `ClientMsg::Conversation`,
      `WorkerMsg::Permission`, `UniHead::Conversation` and its goldens, and `Blob` and
      `EXPAND_CHARS` with them.
    - From the transport and the client: `slopty_net::streams::{open_conversation,
      CONVERSATION_PRIORITY}`, `Uni::Conversation` and `LinkEvent::Conversation`.
    - From the worker: its follow stream (`follow.rs`), its shared transcript reader
      (`conversation::Reader`), and its `@` walk (`file::mention`). The thread composer uses
      `FindFiles`.
    - `PermissionEvent` and `PermissionPrompt` stay, as the worker's own vocabulary between the
      hold and the observer.
  - Tests:
    - In `slopty-workerd/tests/threads.rs`: `a_prompt_is_held_while_its_thread_is_followed` (it
      replaces the follow test of the e2e),
      `the_tables_holder_answers_without_following_and_the_tui_asks_otherwise` (the approver
      test), `a_trusted_mod_streams_live_blocks_that_the_transcript_settles` (live blocks
      through the thread).
    - `an_approver_is_held_for_without_following` and `a_new_follower_answers_what_is_waiting`
      (`slopty_worker::conversation`).
    - The measurement `echo_beside_a_followed_thread` (e2e, ignored).

- ✅ **The person's stop holds deliveries to that agent until they speak again** (2026-10-04,
  A2 of the T3 Code delta study). A report that fell due a minute after the person stopped an
  orchestrator was posted to its idle session and started it working again.
  - **The signal.** The interrupt record in Claude Code's transcript ("[Request interrupted by
    user]") already ends the turn as `Idle` with the detail "interrupted"
    (`transcript::INTERRUPTED`, `Progress::is_interrupt`). The daemon's `Tracker` now keeps that
    as a flag, and only the person's next prompt (`UserPromptSubmit`) or a new session lifts it.
    A later idle hook (`Notification` idle_prompt) does not, nor does the transcript alone for a
    hooked agent.
  - **The guard.** `orchestrate::may_deliver` is `may_type` plus a refusal while the flag
    stands (`AgentTable::interrupted`, `DaemonAgents::interrupted`). The worker's post to the
    server's orchestrator (`server.rs` `may_post`) asks it. A batch held there is not dropped:
    it rides with the person's next prompt.
  - Tests: `reports_wait_for_the_person_after_they_stop_the_agent`
    (`slopty-workerd/tests/server_link.rs`; it fails with `may_type` in its place),
    `an_interrupted_turn_goes_idle_from_the_transcript` and
    `the_table_keeps_the_persons_stop_until_their_next_prompt` (`slopty-agent`).

- ✅ **An agent left waiting only on its commands says so** (2026-10-04, A6 of the T3 Code
  delta study, the adapter half). A dev server the agent left running held its row at
  `Waiting` with the kind `task`, which the server cannot tell from a subagent or a monitor.
  - The observed adapter names the wait `command` (`Wait::COMMAND`) when every task it counts is
    a running `SHELL` task of the main thread and no scheduled prompt is set. Its text is the
    commands' titles. Anything else stays `task` (`Wait::TASK`).
  - The wait is worded again when the transcript starts or ends a task while the agent waits.
    The hooks often count a command before the transcript names it, so the wait turns from
    `task` to `command` once it does. A count the transcript does not show (a monitor) keeps
    it at `task`.
  - The kind is open, so the wire did not change. The server's settle reads `command` as at rest
    for a finished task (`ladder::COMMANDS_WAIT`).
  - Test: `a_wait_on_commands_left_running_names_them` (`slopty-agent` observed tests).

- ✅ **A Codex thread another client rewrote is read again, its held messages kept**
  (2026-10-04). Codex's `thread/reverted` says a client rewrote the thread's history in place.
  The worker takes the thread up again (`thread/resume`) and reads it whole, and the messages
  the person queued are put back ahead of anything queued since.
  - **A revert that undid the running turn.** A queued message waits for the turn under way to
    end. When the re-read shows no turn under way, nothing is left to end, so the message goes
    as the next turn at once. Before, it stayed held until some later notice about the thread.
  - Test: `a_reverted_thread_is_read_again_and_keeps_its_held_message`
    (`slopty-worker/tests/codex.rs`; without the send after the re-read it fails).

- ✅ **Dollar cost, the carried account and drafts on the worker are gone** (2026-10-04, prune
  step 3 of `.research/feature-prune-frontier-2026-10-04.md`, §5.1 #8 and #9).
  - **Tokens, never dollars.** `Usage::COST_MICRO_USD`, `Meters.cost_micro_usd`, the status
    line's `cost_usd`, pi's `cost` fields, ACP's `UsageUpdate.cost` and Codex's
    `account/usage/read` are no longer read. The figure was an estimate for API billing that
    most people on a subscription never pay, it read differently per agent (Codex gives none
    on a credits plan), and a turn's tokens already say how much work it took. The turn footer,
    the tray and the project's spend show tokens and time.
  - **A pointer, not an account.** Going on in another agent no longer writes a 32 KiB account
    of the old thread by rule (`slopty_agent::handoff` is deleted). The new thread starts with
    nothing sent, and the client that asked opens its composer on one short paragraph: the old
    thread's id, folder and branch, and `slopty agent read --thread <id>` to read it. The new
    agent reads what it needs with its own tools, and the person sends, changes or clears the
    words first. An account chosen by rule only guessed at what mattered, and it cost the next
    agent its context whether it needed the history or not.
  - **The composer, not the worker, holds a draft.** `Delivery::Draft` is deleted, with the
    tray's draft card. Edit from a turn and a fork from before a message put the message in
    the new thread's composer the same way. The hub keeps the words by the intent until the
    worker says which thread it started, then gives them once to that thread's first view
    (`ThreadHub::intent_seeded`, `take_seed`). Words the person had not sent are not worth a
    worker round trip, a log entry and a restart path of their own.
  - Goldens: `intent_send_draft` deleted; `intent_send_interrupt` and every golden carrying
    `Meters` changed.
  - Tests: `branching_to_another_agent_carries_the_thread_over` and
    `branching_from_a_message_keeps_or_puts_back_the_files` (`slopty-ui`, the composer seeds);
    `a_claude_code_thread_goes_on_in_pi_with_the_persons_first_message`
    (`slopty-worker/tests/pi.rs`); the Codex start test
    (`a_start_asks_the_persons_codex_for_a_thread_and_sends_its_first_turn`) asks no spend.

- ✅ **Only a project's agents get Slopty's tools; the others get a pointer to the CLI**
  (2026-10-04, prune §5.1 #3). A Claude Code Slopty wires (a tile opened on `claude`, ⌘⇧T, a
  `claude` typed in a Slopty shell) gets `--mcp-config` serving `slopty mcp` only when its
  session names a project (`SLOPTY_PROJECT`). Any other gets one paragraph on
  `--append-system-prompt` (`slopty_agent::hooks::POINTER`): Slopty reaches the person's other
  machines, and `slopty --help` says how. A system prompt the person appends keeps its place
  and gets the paragraph after it. The resume keeps it as it keeps a project agent's role.
  - Why: the tool block cost every agent its descriptions and schemas in context whether it
    reached another machine or not, and the CLI already does everything the tools did. The
    paragraph is the agent's lazy door: it reads the help when the work calls for it.
  - Driven Codex, pi and ACP threads already got Slopty's tools only when seated by a project.
    They get no pointer yet. Codex's `developerInstructions` would stand in for the person's
    own `developer_instructions` rather than add to them, pi's `--append-system-prompt` is not
    yet checked against its `APPEND_SYSTEM.md`, and ACP has no door for it.
  - Still open: a `codex` opened in a tile on a worker with a server gets Slopty's tools
    (`Worker::as_agent`, `codex_with_mcp`), and every `SpawnAgent` gets them
    (`orchestrate.rs`), project or not.
  - Tests: `a_persons_claude_is_wired_once` and
    `the_pointer_joins_the_persons_own_appended_prompt` (`slopty-agent`);
    `claude_opened_in_a_tile_is_started_as_slopty_starts_its_agents`
    (`slopty-worker/tests/agent_open.rs`); `a_typed_claude_is_wired_as_one_slopty_starts`
    (`slopty-cli/tests/claude_wire.rs`).

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **A live block waits for its prompt** (2026-10-04). The mod streams a block as the model
  writes it, often before the transcript has the prompt that started the turn: the hook and
  the mod run ahead of the file, read on a hook or every 250 ms. The observed adapter put such
  a block in the thread's last turn, so a live tool call stood ahead of the prompt it answered
  until the transcript settled it. Now a block that begins while its thread has no turn open
  (none began, or the last one ended) is held with what it says meanwhile, and begins after
  the prompt once the transcript opens its turn. A block the transcript settles first, or a
  thread read anew, lets it go. A turn with no prompt of its own in the transcript shows its
  blocks only as the transcript has them.
  - **A settled text keeps its place** (2026-10-11, readiness-w 10-12, below the line). A live
    text the transcript settled was removed and its entry added at the end of the list, so it
    jumped past a live tool call that began after it until the thread was read again. The
    entry now takes the live item's id (`Mapped::shown_as`, from the overlay's
    `settle_by`, which says which entry settled each block), so it replaces the item where
    it stands and every later change to the entry goes to that item. No wire change: a tool
    call's block already had the call's own id. A thread read anew from the transcript gives
    its items their entries' ids again, as it begins the thread over. So a thread item's id
    is its transcript entry's id only once the thread is read anew, and no client request
    carries a thread `ItemId` (forks and `Seen` name turns, an expand a `ContentRef`). Test:
    `a_settled_live_text_keeps_its_place_before_a_later_call` (it fails without the change,
    the text after the call); the test below now compares items with the transcript alone but
    for those ids.
  - Test: `live_blocks_stream_after_their_prompt_then_the_transcript_settles_them`
    (`slopty-agent` observed tests; it fails without the hold, the blocks shown before the
    prompt).

- ✅ **One agent status: the thread table's** (2026-10-05, readiness item 20;
  step 1, the client). Each client held two accounts of a terminal's agent. One was the worker's
  `AgentEvent` per session (`WorkerMsg::Agent`, `SessionSummary.agent`, the server's
  `Happening::Agent`). The other was the row of the agent's thread in the thread table, which
  the observed adapter already fed from that same event (`Observed::status`). They disagreed
  whenever one lagged: a hook missing, a transcript ahead. So the tile carried a rule ("never
  calmer than working while the row says a turn runs") to paper over it.
  - Now the workspace reads an agent only from its thread's row. The row comes from the
    worker's own table while that worker is linked, else from the server's ladder.
    `WorkspaceView::agent_state(session)` is the stand of the thread whose TUI runs in the
    terminal (`ThreadStand`, which now carries the row's status, its call and a limit's reset).
    An exited agent has none. The header pill, the mark, the navigator's line, the overview, the
    yard, the bell, the notices and ⌘⇧A read that one stand. The two badges, the terminal
    agent's and the thread tile's, are one (`agent_badge`).
  - The worker's codec carries what the old event said and the row did not. A failure on the
    plan's usage limit is a wait of its own (`Wait::LIMIT`, "Hit its usage limit"), and its
    reset comes from the row's spent rate windows. A permission waits on the action it asks,
    worded by the codec from its tool and call ("Run cargo test", "Edit src/main.rs"), else on
    what the tool does ("Wants to run a command"). An elicitation says "Needs input". A lone background
    task is named by the hook's words ("Waiting on cargo test"). Counts read as nouns ("2
    background tasks"), and commands left running read as the thread's header says them
    ("Running npm run dev").
  - Measured first (MEASUREMENTS, "an agent's status by its thread's row"): the row reaches a
    client 0.18 ms after the event at the median and 5 ms at the worst of 200, under a frame,
    so no batching changed.
  - What moved: the turn length and its unseen finish come from the row's phase changes. The
    corner's toast and the app's alert (`WorkspaceEvent::Attention`, now by thread) fire when a
    row comes to need the person: from a linked worker's table, or from the ladder for one not
    linked. The device stays awake while any row works.
  - Deleted in the client: the session maps of events (`agents`, `server_agents`), seeding from
    summaries, `Status::of_agent`, the terminal view's copy of the status, and the e2e dump's
    `agent_source`. The hook-tool phrasing (`tool_action`, `tool_statement`) moved into the
    codec, so every client reads the same words. The app
    ignores `WorkerMsg::Agent`, `Happening::Agent` and the server's terminal list until step 2
    deletes them from the wire.
  - Tests: the workspace tests play an agent through the worker's own codec into its thread's
    row (`workspace::tests::played::Agents`), so every agent test now goes the row's way;
    `workspace::agents::tests` for the words; the `slopty-agent` observed status test for the
    limit's wait; the e2e dump says the mark (`idle`, `working`, `needs-you`, …).
  - Step 2, the wire (same day). The event left the wire whole: `WorkerMsg::Agent`,
    `ToServer::Agent`, `SessionSummary.agent`, `FromServer::Terminals` (and the directory's
    `Change::Terminals`), and `Happening::Agent` are gone. The tracker's types (`AgentEvent`,
    `AgentStatus`, `BlockReason`, `AgentSource`, `SessionAgent`) moved out of `slopty-proto`
    into `slopty_agent::status`, with no serde, because only the worker holds them now.
    - On the worker the tracker's changes go out on a broadcast of the daemon's own
      (`Daemon::heard`). The observed Claude Code threads and orchestration's waits and first
      prompt listen on it, and the thread rows carry the result to everyone else.
    - The server reads every agent from the workers' tables (`hub/ladder.rs`). A thread whose
      rung, phase or ask moved is logged as `Happening::Rung { worker, terminal, agent }`, where
      `agent` is a `TermAgent`, the row's account of the agent: thread, agent, title, rung,
      phase, wait, ask and since. `EventFilter::AgentNeedsInput` reads that event.
      `ListTerminals` answers `Outcome::Terminals { terminals, agents }`, and `AgentStatus`
      answers a `TermAgent`, on the worker too (`ThreadReads::at_terminal`). The project
      engine's seat statuses, its settling and its spend come from the rows, and `Spent::works`
      went with the event.
    - A client gets the agents on connect and after a lag from the ladder in the state. The
      server's terminal list in that state is gone.
    - `slopty terminals`, `slopty workers` and the MCP tools say an agent as its row does:
      phase, wait and ask (`waiting: [{term, agent, asks}]`). `slopty mcp` announces an agent
      once each time its rung becomes needs-you, as a warning. The idle-prompt notice went with
      the event, because a row at rest does not need the person.
    - Tests: `slopty-proto` golden `an_agent_as_its_row_says`; the hub's
      `listed_terminals_carry_the_agent_as_its_row_says` and its rung event tests; the link's
      `a_client_gets_the_agents_on_connect_and_again_after_it_lagged`, over the ladder; the
      worker's relay, managed-launcher, hook and interrupt tests, which now wait on the row.

- ✅ **One queue on the worker for ACP, Codex and pi, and pi queues there** (2026-10-06;
  `crates/slopty-agent/src/queue.rs`, `crates/slopty-worker/src/thread/pi/driven.rs`).
  - **One queue.** ACP and Codex each held the person's queued messages in a copy of the same
    code: hold, take back, change, send now, the stop that holds them all, and the release when
    the person speaks again. Both now use `slopty_agent::queue::Queue`, unchanged in behaviour,
    with their existing tests as the proof. Neither copy survives. Codex keeps the files it
    already read beside each message, which the queue carries as the adapter's own value.
  - **A restored queue leaves out what the worker schedules.** A queue rebuilt from the thread's
    pending list (`Queue::of`, when an agent is taken up again) once took in the messages kept
    for a time as well. Those belong to the worker's schedule, which sends them when their time
    comes, so ACP could have sent one early. `Queue::of` now skips them (`Delivery::is_kept`).
  - **pi queues on the worker, not in pi.** pi offers `queue` now. A message the person queues
    while pi works waits in the codec's queue, where it can be taken back, changed or sent now.
    It goes as a prompt of its own once pi has nothing else to do: no run works, and every
    message sent has come back. The codec checks this after `agent_settled`, after a prompt
    pi refused, and after the session is read again.
    - pi's `follow_up` is not used. pi documents two ways to deliver at a stop: `follow_up`
      during a run, or a `prompt` once `agent_settled` says pi will not go on by itself. pi's own
      queue can only be emptied whole (`clear_queue`), not one message taken back or changed.
      Its `abort` also sends what remains queued, which the person's stop must not do. So the
      worker's queue is the one record, and the prompt at rest is the way it goes. The cost is
      one round trip between runs, against a follow-up's going on in the same run.
    - A prompt an extension's command takes (`disposition: handled`) starts no run and never
      comes back, so it is no longer waited on. Before this, a message queued behind it would
      have waited for ever.
    - With no pi running, taking back and changing act on the thread's record. Sending one now
      makes it a message that takes the thread up again. A scheduled message whose time comes
      goes through the queue as well, so it waits for a run under way.
  - Tests: `queue::tests::*`; the ACP and Codex worker and codec tests, unchanged;
    `a_queued_message_goes_once_pi_has_nothing_else_to_do` (the codec, over the gate's
    recording); `a_message_queued_while_pi_works_waits_on_the_worker` (hold, change, send now,
    take back, and the stop's hold) and `a_message_queued_behind_an_extensions_command_goes` in
    `crates/slopty-worker/tests/pi.rs`.

- ✅ **How a message goes is the thread's to say, by two named rules** (2026-10-06;
  `ThreadMeta::delivery_now`, `ThreadMeta::delivery_after_turn` in `crates/slopty-proto/src/thread.rs`).
  - **Now.** A message the person sends now steers where the agent takes a message mid-turn, and
    is queued where it cannot steer but can queue (ACP). The composer, "Interrupt and send", the
    aside's question and a review's comments all ask this rule.
  - **After the turn.** A message meant for when the agent rests is queued wherever the agent
    queues, else steered: a scheduled message, the agent's own review and a project seat's
    message.
  - **Why.** Each sender used to write its rule out. The review tile wrote "steer" for every
    agent, so a review's comments to an ACP thread were always refused as unsupported.
  - Tests: `a_message_goes_now_or_after_the_turn_by_what_the_agent_can_do` (slopty-proto);
    `every_adapter_takes_a_message_sent_now_or_after_the_turn` (`crates/slopty-agent/tests/delivery.rs`,
    each adapter's own caps against the worker's first check); `comments_to_an_agent_with_no_steer_go_queued`
    (the review tile).

- ✅ **A thread's pull request is watched** (2026-10-06, `.research/readiness-2026-10-07.md` rank
  7). Only a project task's pull request was read again (`Hub::watch_checks`); a thread's was
  asked for when the commit sheet or the review opened and with every push, and nothing polled.
  A check failing, changes asked for, or a pull request ready to merge never reached *Needs
  you*, and a Codex, pi or ACP tile never showed its pull request at all, since the header's came
  from Claude Code's status line alone.
  - *On the worker* (`thread::pulls`): every 15 s it reads the pull requests that fall due, with
    the person's own gh ([`repo::pull::status`]), one read per checkout and branch however many
    threads share it. Due again in a minute while a check runs or one of its threads works, in
    five once settled, in ten after no pull request or an answer gh could not give (diri's
    cadence is a minute in front and five behind). A checkout on its repository's default
    branch (`origin/HEAD`, else `main` or `master`), a detached one, a thread that ended over
    three days ago and a worker without gh are never asked about.
  - *On the wire*: what the forge said is summed up in a `PullSeen` (number, page, title,
    `PullStands`, the failed checks counted and the first named, those running) and put on the
    thread with `Action::PullSeen` when it changed, so its row carries it to every client and
    to the server.
  - *On the ladder* (`Rung::of`, shared by the server and the clients): at rest, a pull request
    whose check failed, whose reviewers asked for changes or that conflicts needs the person,
    and one ready to merge is to review. While the agent works it stays working: it may be on
    it. The notice says the pull request's line ("#42: lint failed"). A project task's agent
    sends none: its pull request is the project's to tell of (`tell_project`), once.
  - *Read at once after a git op, and a merge clears To review* (2026-10-10, readiness 10-12
    item 9). A pull request opened or merged from the sheet showed on the row only five or ten
    minutes later, a merged thread stayed To review for good (its to-review mark compares
    working-tree snapshots, which a merge never moves), and the sheet and the row ranked a
    draft, a conflict and a failed check differently. Now a push, an open, a merge, a review
    or a mark-ready that succeeds nudges the checkout (`pulls::nudge`), and every watcher reads
    it again on the spot rather than on its clock. A pull request seen turning Merged keeps
    its thread's whole tree as what the person kept (`Snapshots::merged`): the work went in,
    so the thread stops being To review, and only what changes after the merge is reviewed.
    One ladder, `PullStatus::standing` giving the wire's `PullStands`, ranks for every
    surface: ended, then a conflict (a conflicted head's checks are stale), a failed check,
    changes asked for, even on a draft (an agent's failing draft still needs fixing), then a
    draft, checks running, waiting or ready.
  - Tests: `thread::pulls::tests` (two), `hub::ladder::tests::a_resting_thread_s_pull_request_lifts_it`,
    `review::a_pull_request_seen_merged_keeps_the_tree_and_a_nudge_reads_it_at_once`
    (`slopty-worker`), `git::tests::a_pull_request_stands_on_its_most_pressing_fact`
    (`slopty-proto`).

- ✅ **A start carries its mode, effort and files, and a worker says what each agent offers**
  (2026-10-06, `.research/readiness-2026-10-07.md` rank 2). A new thread's first message is
  written in the same composer a running thread has, so its model, mode and effort menus and
  its attachments have to work before the thread exists. Only the model went in a `Start`; plan
  mode reached Claude Code alone, as a `--permission-mode plan` the client spelled into `args`,
  and no client knew any agent's menus until one of its threads had run.
  - *On the wire*: `Start` gains `mode`, `effort` and `attachments`, each the agent's own name
    or a path on the worker, as their intents take them. Each adapter applies them at launch:
    Claude Code with `--permission-mode` and `--effort` and the files' paths after the prompt,
    as a pasted path; Codex with `thread/start`'s approval policy and the
    `model_reasoning_effort` setting, the files with the first turn; pi with its thinking level
    ahead of the first prompt, which carries the pictures; an ACP agent with `session/set_mode`
    and its thought option ahead of the prompt and its resources. Claude Code takes nothing in
    `args` now but `--resume`. An agent refuses what it has no door for: pi a mode, a Codex
    thread taken up again a mode, an effort or files.
  - *The offers* (`InstalledAgent::offers`, `thread::offers`): each list is the newest thread's
    of that agent on the worker that published one, list by list, over what the adapter knows
    beforehand: Claude Code's five startable modes and five `--effort` levels (`claude --help`,
    2.1.291) and its model aliases, Codex's three approval policies. The caps watcher folds
    them in every 5 s, so a worker's caps change when a newer catalogue is heard, and only then.
  - Tests: `thread::offers::tests`, `claude_start::a_start_in_plan_mode_opens_claude_planning`
    and its refusals, the `client_start` and `machine_hello_ack` goldens.

- ✅ **A start picks its worktree's base branch** (2026-10-07, `.research/readiness-2026-10-08.md`
  rank 9). A new worktree started from the branch its clone had checked out, and nothing a client
  could show said which others there were, so `NewWorktree::base` was never set.
  - *On the wire*: `GitOp::Branches`, answered with `GitDone::Branches`. It lists the branches
    the clone already knows, its own and `origin`'s, one entry a name with whether each side has
    it, the newest commit first, at most `git::BRANCHES_MAX` (500), the rest counted. It also
    gives the branch checked out and `origin`'s default (`origin/HEAD`). Nothing is fetched, so the
    list comes at once; the worktree's own start still fetches its base for a moment, as before.
  - The worker reads it with `git for-each-ref` over `refs/heads` and `refs/remotes/origin`
    (`repo::branches`). Other remotes' branches are not offered: a base is fetched from `origin`.
  - *In the composer*: a draft asks for its folder's branches as it opens, and again when its
    machine's thread link comes up if it opened before. Once they come, the folder is a
    repository and the place chip is a switch: in the folder itself, or in a new worktree of it.
    In a new worktree a base chip follows ("from main"). Its menu lists `origin`'s default first,
    then the branch checked out, then the rest newest first. The pick is the start's
    `NewWorktree::base`; without one the worktree starts from the branch checked out, as before.
    The question over the composer says where ("in a new worktree of atlas from develop"). Back
    in the folder, the pick and the other agents ticked to run the message too both go, since
    two agents would share one tree. A start on a pull request keeps its place as words: it
    checks the pull request out, not a base. Out of room, both chips wait in the "+" menu.
  - The read is a read in the client's git book (`GitBook`): it clears nothing the last commit
    said, never reads as busy and asks no status after it. A miss (not a repository, out of
    reach) leaves the place as words.
  - Tests: `repo::branches::tests::{a_branch_both_have_is_listed_once,
    a_clone_lists_its_branches_and_origins}`, the `client_git_branches` and
    `worker_git_branches` goldens, `git::tests::a_branches_read_changes_nothing_and_is_kept`
    and `workspace::tests::thread_start::the_place_chip_starts_a_worktree_from_a_branch_picked`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **A Claude Code start held at its own dialog keeps its message and says it asks in its
  terminal** (2026-10-09, `.research/readiness-2026-10-08.md` rank 1). Claude Code may open on
  a dialog of its own, such as the folder's trust or a project's `.mcp.json`, before it takes
  the first message given on its command line. The thread then sat empty and at rest, so its
  face asked "What should Claude Code do in …?" over a message already sent, and nothing said
  that the terminal was waiting.
  - *The message* shows in the thread's pending list from the start, on its way
    (`Host::first_message`). It leaves the list when the agent's item for it shows, matched to
    the start's intent as before. A thread with a pending message shows no hero question.
  - *The signal is the hooks' silence.* Claude Code runs its hooks only after its own dialogs
    are answered. A start no hook has spoken for in `observed::UNHEARD` (5 s) opens a request,
    "Claude Code is asking something in its terminal" (`observed::UNHEARD_ASK`). The request
    has nothing to answer in Slopty, so it reads *Needs you*, and its one button, "Answer in
    the terminal", turns the tile to the terminal face. The first hook (`SessionStart` once
    the dialog is answered, or `UserPromptSubmit`) settles it as answered there. An agent that
    exits first withdraws it. The screen is never read, and nothing is typed into the dialog.
  - Only a start Slopty made is timed: Slopty opened it with the hook relay, so silence means
    something. A `claude` the person started by hand may have no hooks at all.
  - Tests: `claude_start::a_start_held_at_its_own_dialog_says_so_once_its_hooks_are_silent`,
    `agent_tile::a_start_held_at_claude_codes_own_dialog_keeps_its_message_and_shows_the_terminal`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **A yes or no waits for a pocketed phone, which answers it through the server**
  (2026-10-09, `.research/readiness-2026-10-08.md` rank 3; wire change). A phone's link ends
  soon after it leaves the screen. Once it did, nobody was left to answer an approval, so the
  worker handed it straight to the TUI. The server's push then carried a note whose Allow and
  Deny found nothing held.
  - *On the wire*: `FromServer::Pushes(bool)` tells a worker whether a pocketed phone can
    answer. That holds while pushing is set up (`[server.push]`) and the server knows a phone.
    The server sends it with a worker's welcome when it is true, and to every worker when it
    changes (`ladder::say_pushes`). A worker counts it false while it has no server link.
  - *On the worker* (`Holds::set_pushed`, `Reach::Pushed`): while it is true, a yes or no that
    no follower is there for is held for the phone, whether approvers are linked or not. It is
    held for the relay's whole wait (`permission::WAIT`, 595 s, less the margin), not
    `APPROVAL_HOLD`, because the person first has to reach the phone. An approver's link ending
    keeps it held. The answer comes through the server as `Verb::AnswerRequest`, which reaches
    the worker as orchestration, and `Holds` takes a pushed hold's answer from there. When the
    server's word turns false, everything nobody else can answer goes back to the TUI.
  - A question or a plan still waits only for followers: a note's buttons cannot answer them.
  - Tests: `conversation::tests::a_yes_or_no_is_held_for_a_pushed_phone`,
    `hold::tests::a_yes_or_no_is_held_for_a_pushed_phone_up_to_the_relays_wait`,
    `server_link::a_yes_or_no_waits_for_a_pocketed_phone_and_is_answered_through_the_server`
    (a daemon with no client link and the server's flag set),
    `ladder::tests::the_workers_hear_whether_a_pocketed_phone_can_answer`, and the
    `push_answerable` golden.

- ✅ **GitLab merge requests work wherever a GitHub pull request does** (2026-10-09,
  `.research/readiness-2026-10-08.md` rank 8; wire change). Every pull request path ran the
  person's `gh`, so a repository on GitLab had no pull request to read, open, merge, watch or
  start a worktree from. Only a project task's checks knew `glab`.
  - *The forge* is chosen by the host of the repository's `origin` (`Forge::of_host`). A host
    with a `gitlab` label (`gitlab.com`, `gitlab.example.com`) is GitLab. Any other is GitHub,
    so a GitHub Enterprise host still goes to gh. The config file is read directly; git is not
    run for it.
  - *On the worker* (`repo::pull`, `repo::pull::gitlab`): `glab mr view --output json` reads
    the branch's open merge request, and `glab mr list --merged` finds a merged one when none
    is open, as gh's view does. The head pipeline's jobs are its checks, read with
    `glab api projects/:id/pipelines/<id>/jobs`. A failure allowed to fail is neutral, and a
    manual job never started counts as skipped. When the jobs cannot be read, the pipeline
    counts as one check. Everything is put in a pull request's words (`OPEN`, `MERGEABLE`,
    `CLEAN`, `REVIEW_REQUIRED`), so the ladder, the watcher and the sheet read both forges
    alike. `glab mr create --yes` opens one, and `glab mr merge --yes --auto-merge=false`
    merges it now, at the head the person saw (`--sha`). A merge request's worktree checks out
    `refs/merge-requests/<n>/head`. Freeing a worktree asks the forge whether its branch merged
    at its tip. The watcher runs where either gh or glab is installed.
  - *On the wire*: `Forge` on `PullStatus`, `PullSeen` and `GitStatus`, so a client says
    "merge request" and writes `!12` (`PullSeen::named`, `Forge::mark`). It knows this before
    any merge request exists, since the status names the forge.
  - Tests: `repo::pull::tests::{a_merge_request_is_read_in_a_pull_request_s_words,
    a_branch_s_merged_request_is_read_and_none_is_none, a_merge_request_is_opened_and_merged_with_glab}`
    (a stub `glab` replaying JSON shaped like the API's),
    `worktrees::tests::a_worktree_of_a_merge_request_checks_out_its_head_and_tracks_it` (a bare
    remote behind a GitLab URL), `pulls::tests`,
    `thread::tests::commit::a_gitlab_repository_s_sheet_speaks_of_merge_requests`, and the
    `worker_git_merge_request_status` golden.

- ✅ **A thread's agent fixes its checks, addresses its review and catches up, on the person's
  press** (2026-10-09, `.research/readiness-2026-10-08.md` R6). The commit sheet showed a pull
  request's failed checks and changes asked for, but the next step was the person's to type.
  Only a project task's board offered Fix CI, and it named the checks by count.
  - *The steps.* Over an open pull request, a thread's sheet (one whose agent takes a message)
    offers, side by side:
    - "Fix the checks" while a check failed. It names each failed check with its workflow and
      page (the passing ones are left out) and the forge's own way to see why (`gh pr checks
      N` and `gh run view --log-failed`, or `glab ci view` and `glab ci trace`).
    - "Address the review (n)" while points are open. Each point goes by file and line, the
      reviewer's own words first, every note in order. A point on code that has changed since
      says so. Past 32 KiB the rest are counted and left to the page.
    - "Bring up to date" while the branch is behind its base or conflicts with it.
  - Each is one `Intent::Send` through the agent's own door, queued after its turn, the same as
    "Ask to commit". The sheet waits on it the same way and reads the repository again once
    that turn ends. Nothing is sent without the press, and Slopty never answers a reviewer
    itself.
  - *The review* is a new read, `GitOp::PullComments { number }`, answered with
    `GitDone::PullComments`: threads with their notes, each held to `COMMENTS_MAX`,
    `NOTES_MAX` and `NOTE_MAX`, and the rest counted. The client asks for it whenever it reads
    the pull request open.
    - On GitHub, the person's own gh asks GraphQL (`gh api graphql`, with the repository gh
      resolves from the checkout). GraphQL is the one API that says whether a review thread is
      resolved. Each reviewer's latest review counts when it asked for changes or commented
      with words. Approvals, empty reviews and reviews the reviewer replaced ask nothing.
    - On GitLab, glab lists the merge request's discussions (`glab api
      projects/:id/merge_requests/N/discussions`). A discussion that can be resolved and is not
      is open. GitLab's own notes and comments that stand alone are left out.
  - A missing glab is now remembered as a missing gh is, so the sheet offers no open or merge
    it cannot do.
  - Tests:
    - `repo::pull::tests::a_pull_request_s_open_review_is_read_with_gh` and
      `a_merge_request_s_open_review_is_read_with_glab` (stand-in gh and glab).
    - `repo::pull::comments::tests` (what counts as open, the bounds).
    - In `slopty-ui`: `the_sheet_asks_the_agent_to_fix_the_checks_naming_them`,
      `the_sheet_asks_the_agent_to_address_the_review_and_to_catch_up` and
      `no_next_step_without_an_agent_to_take_it`.
    - The goldens `client_git_pull_comments` and `worker_git_pull_comments`.

- ✅ **Keep this run** (2026-10-09, `.research/readiness-2026-10-08.md` R11). One message run on
  several agents, each in a worktree of its own, could be reviewed side by side. Keeping the
  best one left the others running and their worktrees on disk, to be closed one by one.
  - *The line.* A thread with runs offers "Keep this run, close the other N" in its composer's
    "+" menu, under "Review the N runs side by side". It sends `Intent::Discard` for each other
    run, found the way the review finds them (`workspace::reviews::runs_of`): a worktree of
    the same clone named by the same words. The kept run, threads of other messages and runs
    of the same words in another clone are left alone. The line names how many go, which
    stands in for a confirmation.
  - *`Intent::Discard` widened.* It closed only an aside. Now it closes any thread for good:
    - Its agent ends, the worker forgets the thread, and the agent's session stays wherever
      the agent keeps it.
    - It needs no capability of the agent: `Intent::needs` is `None` for it, and
      `Intent::unsupported` answers for an agent that lacks the capability an intent needs.
    - Then the worktree the thread worked in goes (`repo::worktrees::discard`), as long as it
      is one an agent made under its clone's `.claude/worktrees/`, and no other thread the
      worker holds and no live terminal works in it. An aside shares its thread's folder, so it
      never takes it.
    - The worktree goes with its uncommitted changes (`git worktree remove --force`), since the
      person let the run go. Its branch goes only once every commit on it is in `origin`'s
      default branch or the clone's checked-out one, so a commit the run made stays on its
      branch.
  - Tests: `repo::worktrees::tests::a_discarded_run_s_worktree_goes_and_its_commits_stay`,
    the daemon's
    `threads::a_start_in_a_worktree_opens_its_agent_there_and_a_close_takes_it_away`, and
    `workspace::tests::agent_tile::keeping_a_run_closes_the_message_s_other_runs`.

- ✅ **A thread goes on on another machine that has its repository** (readiness R18,
  2026-10-09). Deleted 2026-10-10 with the branch panel (`ui.md`, "'Branch from here' becomes
  Fork from here and Ask aside"): moving work between machines is a task's restart.
  "Branch from here" offered only the thread's own machine, so work begun on the laptop
  could not move to the build box without starting over by hand. The panel now has a Machine
  setting, shown when another linked machine has a clone of the thread's repository.
  - **Which machines.** The workspace matches the thread's `RepoId` (its origin, else its first
    commit) against every checkout it knows on the other machines: their shells' repositories
    and their threads'. On each machine the shortest root wins, so the clone itself is chosen
    over its worktrees. The new thread works in the thread's own folder within that clone. A
    machine that can start no agent is left out. A clone on a machine with no shell or thread
    open there is not known, so it is not offered; cloning a repository onto a machine is a
    separate item (readiness R14).
  - **What starts.** Nothing is asked of the thread's own worker. The panel hands the workspace
    a start on the other machine (`ThreadViewEvent::ContinueOn`), and it opens as any start
    does: a tile of its own with the agent's chips, its composer holding the same pointer back
    as "Continue in…" does, naming the machine the thread is on. `slopty agent read --thread`
    finds a thread on whichever worker holds it, so the new agent reads the old thread the same
    way from anywhere. The person sends, changes or clears the pointer, and nothing goes until
    they do. The agent is the thread's own when that machine has it, else that machine's
    first, and the Agent row offers the rest. From and Files are not offered there: a fresh
    thread elsewhere starts from the end and touches no files here.
  - **No wire change.** The panel asks the workspace for the machines when it opens
    (`ThreadViewEvent::AskElsewhere`, answered with `ThreadView::set_elsewhere`), so nothing is
    worked out while it is shut.
  - Tests: `conversation::thread::tests::carry::branching_onto_another_machine_hands_the_workspace_a_start_there`;
    `workspace::tests::faces::a_thread_goes_on_on_another_machine_with_its_repository`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **Auto mode's declines are said, and the person may let the call be tried again**
  (readiness rank 3, 2026-10-09). Claude Code 2.1.295 starts a person's own `claude` in auto
  mode, where a classifier stands in for the person. When the classifier turns a call down,
  Claude Code runs the `PermissionDenied` hook, and the call ended like any failed one.
  - **The door.** Claude Code's hooks reference gives `PermissionDenied` one answer:
    `hookSpecificOutput.retry: true`, which tells the model it may try the call again. The
    decline itself stands either way. Claude Code ignores the field when the classifier gave no
    verdict. Its `reason` then starts "Auto mode could not evaluate this action". The fixed
    "Classifier unavailable" has no verdict either (`permission::retryable`). Exit codes and
    stderr mean nothing for this event. So Slopty answers only with the retry, only on the
    person's word, and never invents a choice the agent does not offer.
  - **Held like an approval.** `PermissionDenied` joins `PermissionRequest` as a synchronous
    hook (`permission::HELD`, the same 600 s timeout). The relay asks the worker on the same
    `CtlRequest::Permission`. The worker always tells the thread of the decline
    (`PermissionEvent::Declined`). When it may be retried and someone can answer, the yes or no
    is held as any approval is: by followers, by the table's holder for up to two minutes, or
    for a pushed phone. Nobody to answer means no decision, and the decline stands at once, so
    an unwatched agent is never slowed. "Let it try again" prints the retry. "Keep it declined"
    and no answer print nothing.
  - **On the thread.** The decline is a notice of its own kind (`Notice::DECLINED`). It reads
    "Auto mode declined: Run …" and then the rule out of its brackets, or Claude Code's own
    words when there was no verdict. It sits after its call, in the call's turn, whether the
    hook arrives before the transcript has the call or after. The held question is a request of
    a new kind (`Request::RETRY`) that keeps the approval's answer ids (`allow`, `deny`) with
    the words "Let it try again" and "Keep it declined". The card shows the command as code. It
    hangs no "Deny with a reason…" off the keep, since a reason would reach nobody. It offers
    no "Answer in the terminal", since the TUI asks nothing.
  - **Known limit.** The notice comes from the hook, not the transcript. A thread read again
    from scratch (a worker restart) shows the call's failure without it.
  - Tests: `observed::tests::an_auto_mode_decline_is_said_after_its_call_and_may_be_let_try_again`,
    `hooks::tests::install_is_idempotent_and_uninstall_restores` (only the held hooks wait),
    the daemon's `threads::an_auto_mode_decline_is_said_and_may_be_let_try_again` (the relay
    as Claude Code runs it, a follower answering), and
    `conversation::thread::tests::doors::auto_modes_decline_is_let_try_again_or_kept`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **A plan or a question waits for a pocketed phone too** (readiness rank 8, 2026-10-09).
  A yes or no nobody followed was already held for a phone the server pushes to, for as long
  as the relay waits, and its note's Allow answered it through the server. A plan to confirm
  (`ExitPlanMode`) or a question (`AskUserQuestion`) was held only for followers. With the
  phone in a pocket it went back to the TUI at once, so an orchestrator's plan could not be
  confirmed ("Confirm & start") from the phone.
  - **Now.** While the server says a phone can answer (`FromServer::Pushes`), any prompt
    nobody follows is held for it, up to the relay's wait less the margin (`Holds::reach`
    gives `Reach::Pushed` whatever the prompt). The note opens the thread. The phone then
    follows it and answers the plan or the question there as any follower does. A note's
    button still answers only a yes or no: a plan is never confirmed, and a question never
    answered, from outside its thread. The table's keepers (`Reach::Approvers`) still get yes
    or no only, since they answer whole and nothing more.
  - **The cost.** A person at the terminal while a phone is registered waits for Claude Code's
    own dialog on a plan or a question as they already did for a yes or no. Any client showing
    the TUI hands the prompt back at once, and the server's word that no phone can answer any
    more hands back everything held for one.
  - Tests: `conversation::tests::a_prompt_is_held_for_a_pushed_phone` (slopty-worker), and the
    daemon's `server_link::a_yes_or_no_waits_for_a_pocketed_phone_and_is_answered_through_the_server`
    (a plan held with no client linked until the relay's wait runs out, then handed back).

- ✅ **A resumed session names `default` too** (2026-10-09). A resume carries the mode the
  hooks last reported (`resume::with_mode`). It used to drop the flag for `default`, because
  no flag once meant default. Since Claude Code 2.1.283 a run with no mode starts in auto, so
  a session the person kept asking ("Manual") came back looser. `default` is now passed as
  `--permission-mode default` like any other mode. Tests: `resume::tests::the_last_permission_mode_wins`,
  `tests::the_newest_conversation_is_the_one_to_resume`.

- ✅ **"Always allow" says what it grants in Claude Code's words** (readiness rank 10,
  2026-10-09). The standing grant used to read like settings: "Always allow · /work; accept
  edits mode". It now reads on from its button as one sentence, the way Claude Code's own
  dialog says it: "Always allow edits in /work this session".
  - **Wording** (`observed::grants`):
    - A mode is said by what it lets through: `acceptEdits` is "edits", any other is named
      ("plan mode"). The folders granted with it follow ("in /work"). Folders alone are
      "access to /work".
    - A command rule is "npm test commands" (`Bash(npm test:*)`), or the whole command; any
      other rule is said as written.
    - Each part ends with how long it holds, from where Claude Code keeps it: "this session",
      "in this project" (local or shared settings), "in every project". Parts join with "; ".
  - **On the card.** The button stays "Always allow", and the reach follows it in words. Its
    accessible label is the two read on as one sentence (`decision::standing_label`), with no
    "·" between them.
  - Tests: `observed::tests::always_allow_says_what_it_grants_as_claude_code_does`,
    `decision::tests::a_scoped_answer_says_how_far_it_reaches`; golden `thread-phone`.

- ✅ **pi's edits and writes carry their diff** (readiness rank 16, 2026-10-09). A pi `edit`
  or `write` used to be a title with no body, so the review surfaces had nothing to show.
  - **While it runs.** An `edit` is an `EditDetail` built from the texts it replaces, one
    hunk per replacement and no line numbers yet. Its arguments are read the way pi itself
    prepares them: a list of `{oldText, newText}`, a single edit, either sent as a JSON string,
    or a loose top-level pair. A `write` is a `WriteDetail` whose whole content is one hunk.
  - **Once made.** pi's result keeps a standard unified patch (`details.patch`, four lines of
    context). When it ends, that patch, numbered and headed, replaces the one built from the
    texts. A resumed session reads the same `details` from the session's tool result.
  - **One parser.** The unified-diff reader Codex used and ACP's patch from two texts now
    live in `driven` (`unified_patch`, `replaced_patch`), which pi shares.
  - Test: `pi::driven::an_edit_and_a_write_carry_their_diff` (slopty-agent), using the
    shapes pi 1.1.0 publishes.

- ✅ **Two closed names left over are deleted** (readiness ranks 22 and 23, 2026-10-09).
  - **`Liveness::Sleeping` and `Liveness::Silent`.** Nothing built either: they were left
    when sleep and wake were deleted. A thread's agent is `Live` or `Exited`.
  - **`AgentKind`.** It was a closed enum with Claude Code as its only value, against the rule
    that agents are open names. The start of Claude Code's TUI (`Verb::SpawnAgent`) no longer
    names the agent. It only ever starts `claude`, and any other agent starts by its own
    `Runner`. The hook tracker's `AgentEvent` and `SessionAgent` lose their `kind` too, since
    every hook it hears is Claude Code's.
  - Proof: the deletions compile, and only the wire goldens these values were in moved:
    `golden__orchestration__server_request_spawn` (one byte fewer) and
    `golden_thread__frame_actions` (the two statuses that named them are gone).

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **The word that a pocketed phone can answer always reaches the workers** (2026-10-09).
  - **The bug.** While a phone can answer, a worker holds for it any prompt nobody follows
    (`FromServer::Pushes`). The server used to queue that word on each worker's link with
    `try_send` and drop it when the queue was full. The worker then kept the old state until
    the next change. It held prompts for a phone that could no longer answer, or handed them to
    the TUI while one could.
  - **The fix.** The server keeps the word in one `watch`. Each worker link's writer selects on
    it beside the queue (`link::next_for_worker`): the welcome goes first, then the latest word
    goes ahead of anything queued whenever it moved from what that worker was told. Nothing
    waits in the queue, so nothing can drop it or hold it back. A word that moved and came back
    before it was sent is not sent. A worker that registers while a phone can answer hears it
    right after its welcome.
  - **Not ordered, on purpose.** The prompt reaches the worker over its control socket, so a
    prompt that arrives within one link delay of the word is handled as the worker last heard.
    It goes to the TUI, as when no phone can answer. Tests that need the word applied first
    round-trip a verb on the link after sending it.
  - Tests: `link::tests::a_worker_hears_the_pushes_word_past_a_full_queue`,
    `hub::ladder::tests::the_workers_hear_whether_a_pocketed_phone_can_answer` (slopty-server).

- ✅ **An intent's answer is never dropped** (2026-10-10, readiness 10-10 rank 3). The worker
  answered an interrupt, a model change, a fork and the other quick intents with `try_send` on
  the connection's control queue (`CONTROL_DEPTH`, 1024), and dropped the answer when the queue
  was full. The client keeps such an intent pending until its `IntentDone` comes, and nothing
  sends it again short of a relink, so the control stayed spinning. Now a full queue hands the
  answer to one of the connection's tasks, which waits for room and ends with the connection
  (`threads::post_on`). The reader is never held up, and the answer comes after what was queued
  before it. Test: `threads::tests::an_intent_s_answer_waits_for_room_in_a_full_queue`
  (`slopty-workerd`).

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **A plan or a question waits for the Mac's approvers** (2026-10-10, readiness 10-10 rank
  5). A question (`AskUserQuestion`) or a plan (`ExitPlanMode`) from an agent nobody followed
  went straight back to the TUI when no pocketed phone could answer, even with the Mac app
  open. Only a yes or no waited for the clients that keep the thread table. Since the stacked
  approval cards (`5f33d3dc`), such a client shows a question's card, which opens its thread.
  - **Held for approvers too.** `Holds::reach` no longer asks whether a prompt is a yes or no:
    with nobody following and no phone, any prompt waits for the approvers
    (`Reach::Approvers`). An approver still answers only a yes or no in place
    (`Holds::shown`). A plan or a question is answered in its thread, which the card opens and
    the client then follows. The last approver leaving releases what nobody else can answer,
    as before.
  - **The 120 s, revisited.** `APPROVAL_HOLD` stays the approvers' bound. It is how long a
    person at a plain terminal waits for Claude Code's own dialog, and that reason is
    unchanged. But a person who opened the thread from the card to read a plan was cut off at
    120 s, because the hold's length was fixed when it began. Now, when the hold runs out and
    someone follows the thread (or a phone can answer), it is held on to the relay's own wait
    (`threads::hold::held_on`), as if held for them from the start.
  - Tests: `conversation::tests::an_approver_is_held_for_without_following` (slopty-worker: a
    question held for an approver, answered once it follows, released with the last
    approver) and `threads::hold::tests::a_followed_thread_keeps_its_prompt_past_the_approvers_hold`
    (slopty-workerd).

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **What an observer misses in a lag is read again** (2026-10-10, readiness 10-10 rank 23).
  The worker's Claude Code observer skipped a lag on the daemon's agent reports or its terminal
  events, expecting the next change to tell it again. A lag keeps the newest messages, but one
  session's last status can sit behind a flood from others, and no change may follow it. Its
  thread then stayed shown working. A missed `SessionClosed` kept the terminal's task, folder
  and title for good, and its provisional thread with them.
  - After missed reports, the observer reads every agent's status from the daemon's table
    (`Sources::standing`, `AgentTable::snapshot`) and tells each again, as
    `orchestrate::wait` does for a wait.
  - After missed events, it lets go of every terminal it knows that is no longer open
    (`Sources::open`).
  - Test: `claude_threads::what_a_lag_loses_is_read_again` (slopty-worker): a final status
    lost behind 80 reports of other terminals is read from the table, and a close lost behind
    80 events takes its provisional thread.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **Auto mode's declines are pinned by a real capture** (2026-10-10, readiness 10-10 rank 24).
  The decline path above was tested only on input written to look like the hooks reference.
  `cargo xtask fixtures claude` now records an `auto` scenario. The session runs in auto mode
  (`--permission-mode auto`, a new field of a scenario), with no tool allowed ahead.
  - It asks for an upload of a one-word file to example.com, the domain reserved for examples.
    Claude Code 2.1.295's classifier declines it as `[Data Exfiltration]`. The capture's hook
    answers the first `PermissionDenied` with `retry: true`, as "Let it try again" does, and
    lets the second stand. Claude Code then tells the model it may retry, in a meta entry the
    decoder passes over, and the model calls again and is declined again.
  - An earlier draft asked for a force push to a scratch remote, and the classifier let it
    through: a call the person asked for in plain words is no exfiltration. Recorded with the
    person's managed `claude` (`SLOPTY_CLAUDE`), on haiku at high effort.
  - Tests: `observed::tests::a_recorded_auto_mode_decline_is_said_and_its_retry_is_the_one_claude_code_took`.
    Each decline is said after its own call in the classifier's words, both may be let try
    again, and the retry Slopty prints is the one the capture printed and Claude Code acted on.
    Also the `conversation::auto` snapshot, and `auto` in every whole-scenario test
    (`every_entry_is_an_item_in_its_prompts_turn`, `every_captured_event_is_one_the_relay_registers`).

- ✅ **An agent installed while the worker runs is offered without a restart** (2026-10-10,
  readiness 10-10 rank 14). The worker read its installed agents once at start, and again only
  when `[worker.acp]` changed. So Codex or pi installed on a running machine stayed out of
  "New agent…" until the worker restarted.
  - The directories agents are found in (the worker's `PATH` and the login shell's,
    `facts::agent_dirs`) are now followed by the kernel's own directory events
    (`fswatch::follow_folders`: kqueue on macOS, inotify on Linux). When an entry in one is
    added, removed or renamed, and the directories have been quiet for `INSTALL_SETTLE` (1 s),
    the agents are read again (`caps::follow_agents`). An install, a removal and an upgrade that
    replaces the program all count.
  - The capabilities change only when the list moved, and every client hears it as any caps
    change. Nothing polls, and a quiet machine runs no `--version`.
  - Not followed: an upgrade that rewrites files behind an unchanged link (an npm package's own
    files). Its new version shows at the next change in those directories or at a restart. The
    picker starts the same program either way.
  - Test: `caps::tests::an_agent_installed_while_the_worker_runs_is_found` (slopty-worker): a
    `codex` written into a followed directory shows at the version it says, and goes when
    removed.

- ✅ **The whole of a clipped text is read from its line, off the thread** (2026-10-10,
  readiness 10-10 rank 16). An expand, or a picture's bytes, read the whole transcript JSONL
  into a string and searched it. It ran on the blocking pool, but the session's task waited for
  it, so the followed thread stopped for the read. That read grew with the transcript: about
  6.5 ms on 40 MiB (MEASUREMENTS, 2026-10-10).
  - **The index.** `Transcripts` notes, as it reads each file, where every line naming a
    record by `"uuid"` lies: its file, start and length (`session::Index`). `Tail::read_lines`
    now says where its lines begin. `Transcripts::locate` gives the lines that may hold a
    record (`Located`), and `Located::text` / `image` read only those bytes and check the
    record's own id. A line placed wrong falls back to the file read whole. Such a line can only
    come after a write that was not UTF-8, whose lossy decoding moved the bytes. A record not
    read yet is not found, as before.
  - **Off the thread.** The session hands the `Located` to the blocking pool, which answers the
    expand itself, and the session goes on with its thread meanwhile (`thread::claude`).
  - `conversation::full_text_at` is gone, since nothing reads a whole file for one record now.
  - Tests: `conversation::session::tests::a_clipped_text_is_read_from_its_line_alone`. Every
    other byte of the file is spoiled at the same length, and the text still reads, a line read
    in two parts among them. The measurement is `expand_cost`.

- ✅ **A start outlives its link** (2026-10-10, readiness 10-10 rank 17). A start, its
  worktree's setup included, ran on the asking connection's tasks. A Mac going to sleep or a
  link that blipped killed a setup halfway, and the client dropped the start's tile, saying the
  answer might never come.
  - **On the worker.** A start runs on the daemon's runtime, keyed by its intent id
    (`apps/slopty-worker/src/threads/starts.rs`). A connection only follows it: the setup's
    latest words, then the outcome. The same start asked again follows the one under way, or
    is answered by one finished in the last ten minutes. Every outcome, a failed setup or a
    refusal included, goes into the host's record of starts, so a repeat after that is answered
    from the record and nothing is set up twice.
  - **On the client.** A start sent and not answered is kept with its whole request. A link
    that goes leaves its tile waiting, and the link back sends the same start again under the
    same intent, which takes the worker's start up where it stands. ⌘W still closes the tile,
    and a thread that lands after that is said, as before.
  - Tests: `a_start_outlives_its_link_and_the_relinked_client_takes_it_up`
    (apps/slopty-worker `tests/threads.rs`). A worktree's setup waits on a pipe while the
    client's link goes; the pipe is written, and the client back on a new link asks the same
    start and gets the thread, with the setup run once. Also
    `a_start_on_its_way_closes_and_outlives_its_link` (slopty-ui): the tile waits, the link
    back resends the same intent, and the answer fills the tile.
  - **Across a worker restart** (2026-10-11, readiness-w 10-12, below the line). The start's
    progress lived in the daemon's memory alone. A worker restarted mid-setup (an update, a
    crash) knew nothing of it. The setup it ran was left running in its own process group with
    nobody reading it, and the client's tile waited on an answer only its own retry could
    bring. Now a start is kept in the threads' directory (`starts-under-way.json`,
    `Host::start_under_way`) from when it is taken up until its outcome is recorded. It is
    marked once it is past its worktree's setup and opening its agent. It holds the start as
    the client sent it, which names nothing secret. A worker that starts brings each kept start
    to an outcome, with no client asking:
    - **Still setting up:** taken up again from the top, as a client's repeat would be. The
      worktree is reopened and the setup, which never finished, runs again. A client back on a
      new link follows it or gets its outcome.
    - **Opening its agent:** it may have opened it, so it is recorded as failed, saying so,
      rather than opening a second agent.
    - **The setup left running.** While a setup runs, its worktree's git directory names it
      (`slopty-setup-running`: its group, and the kernel's mark of its leader's start). The
      next setup there, under the worktree's lock, kills that group first and waits for it to
      go. A process that took the id since carries another mark and is left alone.
    - Tests: `a_start_the_worker_restarts_during_is_finished_by_the_next` (apps/slopty-worker
      `tests/threads.rs`: the worker is killed while the setup waits on a pipe, the next one
      ends that setup and runs it again unasked, and the client's repeat gets the thread, set
      up twice in all); `a_start_under_way_is_kept_until_its_outcome` (slopty-worker
      `tests/threads.rs`).

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **A start held at Claude Code's trust dialog is trusted on the person's press** (2026-10-10,
  readiness 10-10 rank 13, the worker's half). A start in a folder Claude Code had not trusted
  stopped at its trust dialog, and the thread asked the person to answer in the terminal. That
  is clumsy from a phone.
  - **The offer.** The request a silent start opens (`UNHEARD_ASK`) now offers "Trust this
    folder" (`observed::TRUST_CHOICE`). Its reach is the folder Claude Code keeps the trust
    under (`trust::key`: the repository's root, a worktree's main checkout). It is offered only
    when Claude Code keeps no trust there yet (`trust::trusted`) and the folder may be trusted
    from here (`trust::trustable`). The same request still says to answer in the terminal, for
    any other dialog of Claude Code's own.
  - **The word.** Trust is the person's word, so it is written only on their press
    (`trust::trust_named`), the way their own "yes" is kept: one flag added to Claude Code's
    config, nothing else moved. Unlike the trust Slopty gives its own folders unasked, it may be
    any folder, but never the home nor a folder holding it, whose trust would cover every folder
    below.
  - **Past the dialog.** The dialog is Claude Code's own, and Slopty types no key into it. So
    once the trust is kept, the Claude Code held at the dialog is closed and opened again
    exactly as it was started, on the same `--session-id`. The new one skips the dialog, and the
    same thread goes on in its new terminal. The starts' task does it once
    (`claude::start::Starter::trust`), from the opening it kept for that start. A second press
    finds nothing held. The intent is `Accepted` at once, and the request settles when the
    agent's first hook speaks.
  - **Asked at once** (2026-10-11, the orchestrator-first study, item 24). A start whose folder
    Claude Code keeps no trust for opens at the trust dialog, before any hook. So the request
    is opened as the thread begins, not after `UNHEARD` of silence. Under managed settings
    that keep Slopty's hooks off (`allowManagedHooksOnly`) no silence was ever read as a
    dialog, so such a start used to ask nothing and wait unseen. It settles as before: on the
    first hook, or on a turn the transcript shows. Test: the trust test above, now with the
    hooks kept off and the request in before `UNHEARD`.
  - **Across a worker restart** (2026-10-11, readiness-w 10-12 rank 5). The opening the press
    acts on lived in the starts' task alone. A worker restarted while Claude Code waited at the
    dialog (an update, a crash) left the thread showing the offer, and a press was refused: the
    person had to start again. The held start is now kept in its thread's directory
    (`held-start.json`, `Host::hold_start`) until the press takes it or the thread goes. It
    holds what the start gave `claude` and nothing secret: no relay, no token, no seat's
    variables, which are the worker's own for the seat again when it opens. A worker that
    starts finds each one kept and, while Claude Code still waits in its terminal in a folder
    still not trusted, begins its thread again there with the offer on it. A folder trusted
    meanwhile (the person answered in the terminal) lets the held start go. A terminal not
    adopted yet leaves it kept for the next start. Test:
    `a_start_held_at_the_trust_dialog_is_offered_again_after_a_restart` (offered again after a
    restart and opened again as it was on the press; let go once the folder was trusted
    meanwhile).
  - Tests: `trust::tests::a_named_folder_is_trusted_but_never_the_home_or_above_it`
    (slopty-agent: the home, its parent and `/` refused with the config untouched);
    `trust_pressed_on_a_held_start_trusts_the_folder_and_opens_claude_again` (slopty-worker
    `tests/claude_start.rs`: the offer and its reach, the flag kept, the same command opened
    again and the thread in its new terminal, a second press refused).

- ✅ **Keep takes a turn's change onto what is kept** (2026-10-11, readiness 10-11 rank 2, the
  worker's half). A thread's review opens on its last turn. When an earlier turn had changed
  the same file and was not kept, Keep and "Mark reviewed" there failed with "was kept otherwise
  since it was reviewed". The last turn's old side holds the earlier turn's lines, and what is
  kept does not, so the strict check never passed. A two-turn worker test showed the refusal.
  - **The merge.** `Repo::keep` still keeps the picked change as it is when what is kept of
    the file is the review's old side. Otherwise it merges the change onto what is kept, three
    ways from the review's old side (`snapshot::merge3`, over the same `similar` diff the hunks
    come from), as `git merge-file` would. Changes apart are both made, the same change on both
    sides is made once, and changes over the same lines, or touching, meet. A meeting refuses
    the keep and names the lines of the change it meets at ("a.txt: this change meets lines
    kept otherwise, at line 11"). A turn that removed the file removes it from what is kept.
    A side that is not text where the kept side differs is refused, as before.
  - **Why in Rust, not `git merge-file`.** The keep already holds the three texts. `merge-file`
    would need them written to temporary files, and it answers a conflict count through its
    exit code, which the worker's git runner reads as a failure. The diff is the one the
    review's hunks come from, so a pick and its merge agree on what a change is.
  - **A keep made twice is made once.** The same change kept again now merges to what is
    already kept and answers Done, where it was refused.
  - Tests: `the_last_turn_is_kept_though_an_earlier_one_is_not` (slopty-worker
    `tests/review.rs`). Two turns edit one file; keeping the second turn leaves only the first
    to review, keeping the first then leaves nothing, and a later change over a line kept
    otherwise is refused naming it. Also `three_ways_merge_apart_and_meet_where_they_touch`
    (`repo/snapshot.rs`).

- ✅ **A review tells renames, pictures, large files and modes, and reads its blobs in one batch**
  (2026-10-11, readiness 10-11 rank 18, worker half).
  - **What was wrong.**
    - A file moved and touched read as a removal and an addition.
    - A large text file and a picture both said "Binary file".
    - A change to the executable bit said "No lines to show".
    - Every side of every file cost a `git cat-file` of its own, in turn.
  - **Renames.** The worker diffs the two trees with `-M`, so a rename is one `FileDiff` with its
    `old_path`. A `Pick` carries the `old_path` too (a wire field, added for this):
    - Keeping a rename takes the old path out of what is kept.
    - Putting a rename back whole writes the old side back to its old path and removes the new
      one. It is refused when something else is at the old path again.
    - A hunk of a rename put back stays at the new path.
  - **Kinds.**
    - A picture (by extension) is `Image` with its larger side's size, and the client asks for
      its blobs (`GitOp::Blob`).
    - Text past `TEXT_BYTES` is `TooLarge` with its size, opened whole to read it.
    - Bytes with a NUL in git's 8000-byte head are `Binary`.
    - A mode change carries both modes.
  - **One read per review.** Every side of a review is read by one `git cat-file --batch`. A
    side past `TEXT_BYTES` streams through, and only its head is kept. 300 changed files: 16 ms
    for the whole review, against 4.4 s to read the same blobs one process each
    (`docs/MEASUREMENTS.md`).
  - **One review at a time.** A thread's stream aborts the review it is still reading when
    another is asked for, and when the stream ends. Only the latest is shown.
  - Tests:
    - `renames_pictures_large_files_and_modes_are_told_and_a_rename_keeps_whole` (slopty-worker
      `tests/review.rs`);
    - `a_review_asked_again_stops_the_one_before` (slopty-workerd `threads.rs`);
    - the `review_keep`/`review_revert` goldens.

- ✅ **The person's review of a pull request posts through their own gh or glab, as one review**
  (2026-10-11, readiness 10-11 rank 23, worker half; the drawing half is in `ui.md`).
  - **What was wrong.** `GitOp::PullReview` was on the wire, but the worker refused it.
  - **GitHub.** One `gh api --method POST repos/{owner}/{repo}/pulls/N/reviews --input -`. It
    carries the verdict (`COMMENT`, `APPROVE`, `REQUEST_CHANGES`), the person's words when they
    wrote any, and each note on its `line` and `side` (`LEFT`/`RIGHT`). `commit_id` is the
    head the person reviewed. The answer's `html_url` is the review's page.
  - **GitLab.** It goes the way GitLab's own "Submit review" does:
    - Each note is first made a draft note (`…/draft_notes`).
    - Then all are published together with the words (`…/draft_notes/bulk_publish`), so the
      merge request gets one review and one email.
    - A comment sets the reviewer state `reviewed`, and a request for changes sets
      `requested_changes`. An approval approves afterwards at the head reviewed (`…/approve`
      with `sha`).
    - GitLab needs both lines for a note on a line the diff keeps, and both paths of a renamed
      file. Both are read from the merge request's own diffs (`glab api --paginate --output
      ndjson …/diffs`). A line between hunks is shifted by what the hunks before it added and
      removed.
  - **Nothing else on the forge moves.**
    - The head is read first. A review of an older head is refused before anything is posted,
      so neither an approval nor a note lands on code the person did not see.
    - On GitLab, a review is refused while the person has draft notes of their own pending
      there, because publishing would send those too.
    - If a draft or the publish fails, the drafts already made are deleted.
  - **Checked before the forge is asked.** A review that says nothing is refused, unless it is
    an approval. So is one with more than `REVIEW_NOTES_MAX` notes, a note that is empty or over
    `NOTE_MAX`, or a note on line 0.
  - Tests (stand-in `gh` and `glab` scripts that record arguments and bodies; no real forge is
    reached):
    - `a_review_is_posted_as_one_with_gh_and_never_past_the_head_reviewed`;
    - `a_review_is_published_as_one_with_glab_and_a_failure_leaves_no_drafts`;
    - `review::tests::{a_note_is_placed_on_both_lines_where_the_diff_keeps_its_line,
      a_review_out_of_bounds_or_saying_nothing_is_refused, a_review_at_an_older_head_is_refused}`.

- ✅ **Codex gets the commands and background work its app-server publishes** (2026-10-12,
  readiness 10-12 "below the line", kept by the orchestrator-first study). A Codex thread had no
  slash menu and an empty tray. The app-server (0.162.1, checked in its JSON Schema and its TUI's
  source) has a door for each, so the adapter uses them and nothing else:
  - **Commands are skills.** Codex's own slash commands live in its TUI and the app-server does
    not list them. It lists skills (`skills/list`, again on `skills/changed`), so those are the
    composer's commands: enabled ones only, each with its short description, and its scope as
    the source (`personal`, `project`, `built-in`, `admin`, or `plugin`). A message that leads
    with one as `/name` goes as Codex's own `$name` with a `skill` input naming its path beside
    the words, which is how Codex's TUI sends a skill. Any other `/` word goes as typed.
  - **Background work** is Codex's background terminals (`thread/backgroundTerminals/list`):
    each is a `shell` task named by its command and tied to the call that started it. The
    app-server sends nothing when one exits, so the worker lists them again whenever a command's
    call or a turn ends, or the thread is taken up (`codex::shared::terminals_due`). A thread at
    rest with some still running waits on them (`Wait::COMMAND`), as a Claude Code thread does.
  - **No compacting through Slopty.** The orchestrator-first study deleted compacting from every
    agent's face, since agents compact themselves, so no `Cap::COMPACT` is declared and
    `thread/compact/start` is not spoken.
  - The generated protocol gains `skills/list`, `thread/backgroundTerminals/list` and
    `skills/changed`.
  - Tests (slopty-agent `tests/codex.rs`): `codexs_skills_are_its_commands`,
    `commands_left_running_are_its_background_tasks`.

- ✅ **Handing a session to the TUI and back, rewinding and compacting through Slopty are
  deleted** (2026-10-10, the orchestrator-first study, C.2).
  - **What went.** `Intent::Handoff` and `Intent::TakeBack` with pi's TUI follower
    (`thread/pi/tui.rs`), `Intent::Rewind` with the worker's file restore (`thread/rewind.rs`,
    `Snapshots::restore`) and Codex's cut fork, `Intent::Compact` with every adapter's handler,
    the caps `HANDOFF`, `REWIND`, `COMPACT` and `LIVE_TEXT`, and in the client the composer's
    handoff strip, the take-back key, the tray's rewind and the branch panel's files row.
  - **Why.** Each was a second way to do what the agent's own session already does. A driven
    agent's session is the agent's, so its own TUI resumes it once Slopty lets it go. Fork
    stays and is the one way to branch. Agents compact themselves, and a person who wants a
    compact asks the agent. Rewinding files under a running worktree was the riskiest door
    Slopty had, for a feature the person reached through the agent anyway.
  - Goldens: `intent_compact`, `intent_handoff`, `intent_rewind` and `intent_take_back` are
    deleted; the other intent goldens moved with the variant indices.

- ✅ **Continue, the Codex goal and interrupt-and-send leave the wire** (2026-10-10). The thread
  view kept only what directs a thread (`ui.md`, "The thread keeps only what directs it"), so
  nothing in the client read these any more. Dead wire is deleted, not kept for later.
  - **Gone from `slopty-proto`.** `Intent::Continue` and `Cap::CONTINUE`;
    `Delivery::Interrupt`; `ThreadState::goal`, `Goal` and `Action::GoalSet`. The goldens
    `intent_continue` and `intent_send_interrupt` are deleted, and the rest of
    `golden_thread` moved once with the shifted variant indices.
  - **Gone from the worker and the adapters.** `thread/carry.rs`, which started the continued
    thread, and its handler; the daemon turning an interrupt into a steer; the ACP
    adapter's `ThreadAsk::Interrupting`; pi's interrupt arm; Codex's `thread/goal/get` on a
    resume. Codex's `thread/goal/updated` and `thread/goal/cleared` are read and dropped.
  - **Kept.** `Promote`: a held message sent now still goes first through each adapter's own
    queue (`a_held_message_sent_now_stops_the_turn_and_goes_next`).
  - Tests deleted with their behaviour:
    `a_message_sent_by_interrupt_stops_the_turn_and_goes_next`,
    `a_claude_code_thread_goes_on_in_pi_with_the_persons_first_message` and
    `a_codex_goal_shows_as_codex_holds_it`. Reworked:
    `an_archived_thread_is_unarchived_and_taken_up_again` (`slopty-worker/tests/codex.rs`, no
    goal read on the resume).

- ✅ **What is written to Codex while its app-server restarts waits for it** (2026-10-11, the
  orchestrator-first study, item 24's first half).
  - Before this, a message, an edit or an answer given while Codex's app-server was away was
    dropped. So was one held on a link that went down. Each had already been answered done, so
    the message vanished. And a thread a client was watching stood still after Codex came back
    until it was followed again.
  - **A message or an edit waits.** It is kept (`Parked`) and goes once Codex is back and has
    taken its thread up again. The bound is `HOLD_FOR`, 30 s: an app-server that restarts is
    back within seconds, and past that the person should know the message did not go. Past
    the bound, or when the resume fails, it is held in the thread's list of messages on their
    way with the reason (`NOT_BACK`, or the resume's words), for the person to send again.
    That covers messages held on the link that went too.
  - **An answer does not wait.** Its request ended with the app-server, so it is held at once in
    the thread's list, under the answer's intent, in its choice's words (`REQUEST_ENDED`).
    That way the person sees it did not go and can send it as a message if they still mean it.
  - **A watched thread goes on.** On rejoining, a thread Codex no longer holds is taken up
    again at once if anyone follows it (`Host::followers`). Others are taken up on the next
    follow or message, as before. A thread Codex still holds waits for its resume too, so a
    kept message cannot find it not yet followed.
  - **What was queued behind a turn waits too** (the same day). A message queued while a turn
    ran waited in the link's own queue (`Shared`), so it went with the link. On a link's end,
    each followed thread's queue is taken (`Queue::into_held`), along with any held for a
    thread being read again. Its messages wait for the next link as any message written
    meanwhile does. Once one goes, it leaves the thread's list of messages on their way. One
    the person's stop held stays held, saying so (`Pending::STOPPED`), for them to send again.
  - Tests (`slopty-worker/tests/codex.rs`):
    `a_message_written_while_codex_is_away_goes_once_it_is_back`,
    `a_message_codex_does_not_come_back_for_is_said_not_to_have_gone` (on a paused clock),
    `an_answer_given_while_codex_is_away_is_said_not_to_have_gone_at_once`,
    `a_followed_thread_is_taken_up_again_once_codex_is_back`,
    `a_message_queued_behind_a_turn_goes_after_codex_s_link_comes_back`.

- ✅ **pi's gate lets through what its mode allows** (2026-10-11, readiness-w 10-11 rank 4). This
  refines "pi is driven over its RPC mode, with a gate that fails closed by pi's own rule".
  - **The gap.** The gate put every call to the person: reads, Slopty's own tools, all of
    them. A project's autonomy never reached a pi task. The server held only Claude Code and
    Codex to a level, so a pi task at Edits or On its own still asked about everything, and
    with a few such tasks Needs you filled up with reads.
  - **Modes.** pi has no permission system, so the gate is the one it runs under, and it now
    has three modes (`slopty_agent::pi::modes`), the project's levels by name:
    - `ask`, the default: a read inside the thread's folder (pi's `read`, `grep`, `find`,
      `ls`, or a tool of pi's own that says `readOnlyHint`) and Slopty's tools
      (`mcp__slopty__…`) go. Everything else asks.
    - `edits`: `edit` and `write` inside the folder go too.
    - `own`: every call goes.
    A path that climbs out of the folder, or one outside it, always asks. Another MCP server's
    read-only hint is not taken. An unknown mode lets nothing through, so the gate still fails
    closed.
  - **Where it is set.** The thread offers the modes (`ThreadMeta.modes`, `Cap::SET_MODE`), and
    the person switches them as any agent's mode. A start may name one (`Start.mode`). A
    project's pi task starts at its level (`Autonomy` → `ask`, `edits`, `own`). The mode is
    kept as the fact `pi-mode`, so a pi started again for the thread keeps it, and the meters
    show it.
  - **Not Slopty answering.** A call let through is the level the person set, as Claude Code's
    own `acceptEdits` and `auto` are. Every call the level does not cover still goes to the
    person, and nothing is answered in their place.
  - **Held there by the server** (2026-10-11). A project's pi task is watched as a task's
    Codex is (`Board::settings_moves`, `Hub::held_settings`): its row's mode is the gate's,
    and a switch to one that lets more through than the level's mode (`edits` or `own` at
    Ask, `own` at Edits) closes it by its seat, and the timeline says why. A mode the gate
    does not know lets nothing through, so it stays. Test:
    `a_pi_task_switched_past_its_level_is_closed` (`hub/project_tests.rs`).
  - ACP tasks start in their agent's default mode, ruled kept (2026-10-11). An ACP agent
    names its modes itself, and they are known only once its session exists. A table of ids
    guessed per agent would be the per-agent polish the orchestrator-first ruling drops, and a
    wrong guess at Own would loosen past what the agent documents. The default asks the person
    no less than any level, so a higher level only costs an ACP task some questions. The
    person can still switch the thread's mode from its own list.
  - Tests: `each_mode_lets_through_what_it_says`, `a_read_only_hint_is_taken_from_pi_s_own_tools_alone`
    and `the_modes_offered_are_the_ones_known` (`pi/modes.rs`);
    `the_gate_lets_through_what_the_mode_allows` (`slopty-agent/tests/pi.rs`); the unknown
    mode in `a_start_pi_cannot_take_is_refused` (`slopty-worker/tests/pi.rs`); the pi task's
    mode in `any_agent_runs_a_task_as_a_thread` (`hub/thread_tests.rs`).

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **An observed Claude Code is read when its transcript is written** (2026-10-11,
  readiness-w 10-12 rank 2; `crates/slopty-worker/src/thread/claude.rs` and `claude/follow.rs`).
  - **The gap.** With no mod streaming and no hook for the write, an observed session read its
    transcript every 250 ms. The mod reaches only the Claude Code builds it is pinned to, so
    the `stable` channel, a company's managed `claude` held back, or any build the person has
    not updated, streamed on that tick: p99 238 ms from the write to the thread. Every
    session, busy or not, also woke the worker four times a second.
  - **One follower for every session.** The transcripts are followed by the kernel
    (`crate::fswatch`, kqueue on macOS, inotify on Linux), and a write rings the session
    that reads that file. There is one follower for all of them, not one per session: on
    Linux each follower is an inotify instance and a user gets 128 by default, shared with
    every other program. A session names its paths (the main transcript and its subagents')
    at each read, and the follower takes the union.
  - **No write is missed between the list and its watch.** A path is followed only from the
    moment the follower takes the list naming it, so a write just before is no event. The
    follower now says which paths the list it last took holds (`fswatch::Status::followed`).
    A session whose new paths were not all in it is rung once they are, and that one read
    catches whatever was written before. Every write after is an event. No timing is
    guessed: a tick after a list change would only make the race rarer.
  - **The tick is kept only where time alone changes the thread.** That is while a block
    waits out its grace before it asks in the terminal, while a live block may expire
    (`Observed::ticking`), and while a start no hook has spoken for may be held at a dialog.
    Then it is 250 ms. Otherwise a session looks again every 5 s (`RESTING_TICK`), which
    bounds what a write the kernel did not report could wait. A file the kernel cannot
    report on (a network volume) is polled at 250 ms by the follower itself.
  - **Numbers** (MEASUREMENTS, "an observed Claude Code's transcript, read on the kernel's
    word"): the write reaches the thread at p50 6.8 ms and p99 11.7 ms, from 133.1 ms and
    238.4 ms. Eight sessions at rest cost the worker nothing above the runner's noise, from
    1.5 wakeups/s each.
  - Tests: `an_answer_written_with_no_hook_reaches_the_thread_at_once`
    (`slopty-worker/tests/claude_threads.rs`; each answer must arrive inside 500 ms, two of
    them written after a second of rest); `past_the_watch_limit_a_file_is_polled`
    (`tests/fswatch.rs`, what `followed` says); the `ticking` assertions in
    `a_block_with_no_prompt_held_asks_in_the_terminal` and
    `live_blocks_stream_after_their_prompt_then_the_transcript_settles_them` (`observed`
    tests). The measurements are `a_transcript_write_reaches_its_follower` and
    `observed_sessions_at_rest` (`apps/slopty-worker/tests/e2e.rs`, ignored).

- ✅ **How freely a started agent acts is one setting** (2026-10-13, with "The task engine is
  gone; agents start agents" in `projects.md`). An agent that starts another names no harness
  arguments of its own: no permission mode, no flags. The server builds the start from the
  person's `[agents] autonomy` in the server's `settings.toml`, in each harness's own words
  (`hub::agents::mode_for`):

  | `autonomy` | Claude Code `--permission-mode` | Codex approval policy | pi gate |
  |------------|---------------------------------|-----------------------|---------|
  | `auto` (default) | `auto` | `on-request` | `edits` |
  | `full` | `bypassPermissions` | `never` | `own` |
  | `ask` | the harness's own default | `untrusted` | `ask` |

  An ACP agent starts on its own default, having no mode Slopty can name. The setting is read
  again when the file changes and applies to the next start; an agent already running keeps
  the mode it started in, and the person changes it there as any agent's mode.
  - **Why one setting.** The person decides how far agents go, once, as Claude Code's own
    `defaultMode` does. An agent choosing its child's flags could start it looser than itself.
    `auto` is the default because Claude Code's classifier and Codex's `on-request` let routine
    work go and still ask about anything risky; `full` is there for a sandboxed machine.
  - **Deleted with it.** The allowlist that judged every `claude` start's flags
    (`SAFE_FLAGS`), the worker's judge of what loosened a running agent (`loosening`,
    `AgentReport::Loosened`) and the terminal it closed, the held-to-asking start for terminals
    an agent opened (`ASKING_ENV`, `held_to_asking`), and the per-project autonomy with the
    server's watch on each task's mode. Each guarded a project's agents against a mode the
    project had not granted; with one setting for every start, the harness's own mode is the
    guard, and Slopty never answers in the person's place.
  - Tests: `a_start_names_its_parent_and_takes_the_person_s_autonomy` (`slopty-server`), the
    settings' `agents_autonomy`, and `apps/slopty-cli/tests/agents.rs` end to end.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **An observed Claude Code is read from the conversation its registry names** (2026-10-14).
  A Claude Code no hook speaks for (one started by hand, one whose managed settings keep hooks
  off, one adopted after the worker restarted) was read from the newest `.jsonl` in its
  folder's project directory written since it was first seen. With two Claude Codes in one
  folder that guess takes the other one's file: a chat Slopty started beside the person's own
  session showed the person's conversation, under the chat's thread, after the worker
  restarted. Claude Code keeps a registry of its live sessions (`~/.claude/sessions/<pid>.json`,
  `slopty_agent::roster`) whose entry names each process's conversation and moves with `/clear`
  and `/resume`. Discovery now reads it on each pass: the entry of the agent's process, or of
  its child under a managed launcher, names the file (`discover::registered_transcript`); a
  conversation it names that is not written yet is waited for. Only a process the registry
  does not list is still found by time. Test: `slopty-agent`
  `discover::tests::the_registry_names_the_transcript_over_the_newest_file`.

- ✅ **A prompt ends the turn still open before it** (2026-10-14). A message the person sent
  while Claude Code was mid-turn opened a new turn and left the one before it running for
  good: that turn's answer stayed "streaming" in the middle of the transcript. Claude Code
  closes a turn with its `stop_hook_summary` or `turn_duration` record, which an interrupted
  turn may never write. A prompt now ends the turn before it at the prompt's time when nothing
  ended it first (`conversation::start_turn`); the observer marks it interrupted when Claude
  Code recorded the interruption, complete otherwise. Test: `slopty-agent`
  `conversation::tests::a_prompt_ends_the_turn_still_open_before_it`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **An observed Claude Code's threads follow its sessions, not the last word heard**
  (2026-10-15). A hook that moves a terminal to another session (`/clear`, `/resume`) names the
  new transcript before the status that says so reaches the observer, which read the new file
  into the old session's thread and wiped its history. The transcript's file name now decides:
  a name other than the observed session's begins that session's thread, and the one left ends
  with its history. The word that an agent went names no session (the tracker forgot it), so
  the session's id stayed claimed by the kept terminal and a chat resumed in another terminal
  became a second thread beside it; an agent that goes now always lets go of the id it held. A
  worker restart no longer ends at once a thread whose terminal is still open: it waits
  `claude::STARTUP_GRACE` for the agent's first word and ends only the threads nothing began
  again, so an agent that runs on is never said to have ended. A message the earlier worker was
  typing waits to go again (ptyd types it only if it never took it) and can be withdrawn or
  changed while its agent is gone. Tests: `claude_threads.rs`
  `a_new_session_in_the_terminal_never_overwrites_the_one_left`,
  `a_session_resumed_elsewhere_after_its_agent_quits_is_the_same_thread`,
  `a_message_left_being_typed_by_a_restart_waits_again`,
  `a_thread_whose_agent_runs_on_is_not_ended_by_a_restart`.

- ✅ **A Claude Code subagent's thread says where its run stands** (2026-10-15). Subagent
  threads had no status, no title, never ended, and one whose own transcript came before its
  `Agent` call named it (a call in the foreground names its subagent only with its result)
  stayed unlinked from its parent. Now the call's description is its title, it works while the
  call runs, and once the call ends (or Claude Code says a background run did) its open turn
  ends and the thread exits done, failed or stopped, not resumable, since a subagent is never
  taken up alone. A thread begun before its call named it is told its parent then. Test:
  `slopty-agent` `observed::tests::a_subagent_works_until_its_call_ends_then_is_done`.

- ✅ **A subagent stops with the Claude Code that ran it** (2026-10-15). Two background
  subagents of a session taken up again with `--resume` said "running" for hours, and their
  parent's badge counted them: a subagent runs only inside the process that started it, a
  resumed session brings none back, and Claude Code never writes how one cut off that way
  ended, so reading the transcript again put them back to work. The worker now hands the
  observer the start of the process that runs the session (the tracker's foreground process,
  from the kernel's process table); a subagent whose call came before it, with no end of its
  own, ended stopped then. The clients count no subagent of a call under an agent that has
  exited, and show it stopped. The explorer button's running-subagents badge is gone: the
  subagents are rows under their agent in the sidebar. Tests: `slopty-agent`
  `observed::tests::a_subagent_left_at_work_stops_with_the_claude_code_that_ran_it`,
  `slopty-worker` `a_subagent_left_at_work_stops_once_a_later_claude_code_runs_its_session`,
  `slopty-ui` `a_subagent_stops_with_the_agent_it_ran_in`.

- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **What the composer types is matched to the agent's copy, and nothing is typed twice**
  (2026-10-15). A stop is accepted while Claude Code runs a tool, not only while the model
  writes. The agent's copy of a typed message is found by more than exact words: the head of a
  long one Claude Code keeps clipped (it begins the message and counts its characters), a
  command kept as its name and arguments, and a message sent with files, whose paths were typed
  after the words and whose pictures Claude Code keeps as `[Image #n]` or apart. A queued
  command Claude Code runs itself (`/model`) starts no turn of the model's, so its showing in
  the transcript lets the next queued message go. A start's first message cut off by a restart
  is held and never typed again on its own; its copy in the transcript read again takes it off
  the list, and the person's promote sends it. A thread's parent stays with it when an adapter
  tells its metadata, or reads it again, before it knows the parent. A thread log an older
  build wrote, which no longer decodes, is deleted with a warning instead of being skipped at
  every start. Tests: `compose.rs`
  `a_message_cut_off_by_a_restart_goes_only_when_promoted`,
  `a_queued_command_lets_the_next_go_once_it_shows`, the tool case in
  `an_interrupt_is_esc_and_a_model_is_its_command`; `threads.rs`
  `a_typed_message_is_known_in_the_agents_copy`,
  `a_threads_parent_outlives_an_adapter_that_does_not_know_it`,
  `a_log_that_no_longer_reads_is_deleted`, and the read-again step of
  `a_starts_first_message_cut_off_by_a_restart_is_settled`.

- ✅ **An ACP thread's queue is the thread's while its agent is gone** (2026-10-15). Queued
  messages on an exited ACP thread could not be withdrawn, edited, moved or sent now, and an
  agent taken up again started with an empty queue, so they never went. As with pi, the queue
  is changed in the thread's record while no agent runs, a message sent now takes the thread up
  again, and a session loaded again starts from the queue the thread holds. Test: `acp.rs`
  `a_queue_left_by_a_gone_agent_is_changed_at_rest_and_goes_after_the_load`.

- ✅ **A Codex thread ends as Codex left it, and no message to it vanishes** (2026-10-15). A
  thread whose app-server went, that Codex closed, or that a worker found at work with no Codex
  answering its first try was only marked exited, so it went on saying it worked; it now ends
  as an agent that quit does (`driven::cut_short`). A thread Codex cannot take up again is
  tried again on its own timer and ended only after the last try. A turn or steer Codex refuses
  (an answer's reason among them) stays in the list in Codex's words, and every message held as
  not sent stays there, to be sent again, changed or taken back. An exited, resumable Codex
  thread still takes messages and changes to waiting ones, which take it up again; answers and
  stops are refused, since the request and the turn ended with Codex. After a quiet thread is
  read again, what was queued behind its turn goes. Tests: `codex.rs`
  `a_turn_under_way_ends_stopped_when_codex_goes`,
  `a_thread_left_working_ends_when_no_codex_answers_at_the_start`,
  `a_thread_codex_closes_ends_and_is_taken_up_again_by_a_message`,
  `a_thread_codex_cannot_take_up_yet_is_tried_again_before_it_is_cut_short`,
  `a_message_queued_behind_a_quiet_turn_goes_once_it_is_read_again`,
  `a_message_codex_refuses_is_held_and_can_be_taken_back`; `apps/slopty-worker` threads'
  `an_exited_codex_thread_still_takes_a_message`.

- ✅ **An ended agent finished nothing, and an unreachable worker holds nothing** (2026-10-15).
  The server keeps its machine awake only for agents at work on workers linked now: an
  unreachable worker's rows say what it last said, and nothing keeps that true. An agent that
  ended with its work under way (it quit, or a worker cut it short) finished nothing, so no
  phone hears that it did; only a turn it answered since it got busy is a finish. At a
  terminal, a session whose agent still runs outranks one that ended later, so an ended row
  never makes an agent's shell count as the person's. A thread removed and made again under
  the same id stays in every client's table. Tests: `slopty-server`
  `a_worker_out_of_reach_holds_nothing`, `an_agent_that_ended_at_work_is_not_finished`,
  `an_ended_session_does_not_hide_the_agent_at_its_terminal`; `slopty-proto`
  `a_thread_removed_and_made_again_stays_in_the_table`; the worker table's
  `a_thread_made_again_is_a_row_not_a_removal`.
- ❌ **Deleted 2026-10-16: the observed Claude Code ruling below.** Claude Code is driven
  (see "Each agent's drive" and "Why Claude is driven" above): its hooks, its mod, its
  transcript follower and its keystroke paths went with that ruling. Kept as the record.
- ✅ **A message caught being typed by a restart goes once (2026-10-15).** A restart holds a
  Claude Code message it caught being typed until the agent's transcript is read again
  (`schedule::CHECKING`, which the composer never types), knowing it as typed so the agent's
  copy marks it, and the composer drops any message the transcript already shows. Read with
  its agent back in that terminal, whose ptyd delivery name guards it, it waits to go again;
  with the terminal gone, the session back elsewhere, or no agent back within the startup
  grace, it waits for the person as cut off. The thread's own marks (a name, the person's own
  words for a message) stay through a read again. Tests: `slopty-worker`
  `claude_threads::a_message_left_being_typed_by_a_restart_waits_for_its_transcript`,
  `compose::a_message_the_transcript_shows_leaves_the_list_untyped`,
  `threads::a_message_caught_being_typed_waits_for_its_transcripts_word`,
  `threads::what_the_worker_owns_outlives_a_read_again`.

- ✅ **Pictures, visuals, pages and MCP Apps through each agent's own doors (2026-10-16).**
  - **Pictures are kept by their digest.** A picture Codex viewed (`imageView`) or made
    (`imageGeneration`), an image block in an MCP result, an ACP agent's image and resource
    blocks, and pi's tool and message images are each kept once on the worker, by the BLAKE3 of
    their bytes. They live in `<data>/media`, at most 512 MiB, and the picture used longest ago
    goes first. Each is described as an `Image` at `media:<digest>`, and a client's `Expand`
    of one is answered from there (`slopty_agent::media`). Claude Code's pictures stay in its
    transcript. The other agents keep theirs where only they read them, or name a file that
    may change, so the worker keeps a copy. A thread read again keeps its pictures again.
  - **An MCP result keeps everything it said.** Its text, its pictures, its `resource_link`s
    and embedded resources, and its `structuredContent` all stay on the call's `McpDetail`.
    An audio block and any block MCP adds later stay as the JSON they came as
    (`slopty_agent::shown`).
  - **`show_visual` and `show_page` are tools on Slopty's own MCP server (`slopty mcp`).** A
    call's whole input is in the agent's own record, so the decoder turns a call that passed
    into a `ToolDetail::Visual` or `ToolDetail::Page`, and nothing is stored twice. The tool
    runs the script once in the sandbox (`slopty_visual::check`), and a failure goes back to
    the model as "the script failed at line N", so it fixes the script itself. A page that
    loads anything from the network is refused before it is shown, naming what it loads.
    Luau runs only where the tool runs (the `visual` feature of `slopty-tools`, which
    `slopty-cli` turns on). The server's Linux build never compiles it.
  - **MCP Apps are relayed through the agent, never dialled.** Slopty declares the
    `io.modelcontextprotocol/ui` extension at Codex's `initialize`, so Codex passes it on and
    the servers offer their apps. A call whose tool names a `ui://` resource
    (`mcpAppResourceUri`, or `mcpAppUi` when Codex fills it) becomes a page of kind
    `PageSource::App`, with the call's whole result for the page.
  - **A page reads and calls its app with `ThreadRequest::App`.** The worker turns this into
    Codex's `mcpServer/resource/read` (only for `ui://` URIs) or `mcpServer/tool/call` in the
    thread's own session, and answers with `ThreadFrame::App`. Codex holds the connection,
    its approvals and its credentials, so Slopty touches none of them. An agent with no door
    to its MCP servers (ACP agents, pi) is refused in words.
  - **Proving the relay end to end.** The stand-in `slopty-stub-mcp-apps` (testkit) records
    whether its host declared the extension. Codex's recording (`cargo xtask codex fixtures`,
    `media.jsonl`) shows the app's call, the page read back with its `_meta.ui`, and an
    app-only tool called through the relay. The recording stops if Codex offers the model the
    app-only tool (`visibility: ["app"]`); 0.162.1 keeps it off the model's list.
  - Tests: `slopty-agent` `codex.rs`
    `{a_recorded_media_turn_shows_its_app_and_its_pictures,
    a_made_picture_and_an_apps_display_mode_are_kept,
    slopty_s_show_visual_is_a_visual_once_it_passed}`; `acp.rs`
    `{pictures_reach_the_thread_by_their_digest,
    an_agents_picture_and_resources_in_its_message_are_kept}`; `pi.rs`
    `{pictures_reach_the_thread_by_their_digest, the_models_own_picture_is_a_row_of_its_own}`;
    `media::tests`, `shown::tests`, `codex::apps::tests`; `slopty-tools` `show::tests`.

- ✅ **Claude Code's doors beside messages, as its TUI has them (2026-10-16).**
  - **Its own work is the thread's tasks.** `task_started`, `task_progress`, `task_updated`,
    `task_notification` and `background_tasks_changed` (which replaces the list whole) keep
    each task's kind, step, figures, parent and timeout. A notice with no call (a monitor, a
    scheduled prompt) is a task row of its own. The notice rebuilt for the decoder keeps its
    duration and tokens. A turn that ends with background agents still at work says "Waiting
    for N background agents to finish", as the TUI's footer does. Those agents stay live tasks
    and are never shown as completed. A task is stopped by its own id (`stop_task`), and a
    blocking call is moved to the background (`background_tasks`, the TUI's Ctrl+B).
  - **What it says beside the conversation is shown in the TUI's words.** `tool_progress`
    becomes the call's note, at most every 10 s. A `notification` is a passing note.
    `active_goal` is the goal, with the TUI's "Goal achieved" line. `prompt_suggestion` is the
    suggestion. The system rows it derives become notices in its words: memories saved or
    recalled, agents stopped, a permission retried, a fallback model, a hook that failed or
    stopped the turn, a plugin installed, long instructions, a scheduled task, a local
    command's output. A denied call says who denied it.
  - **Rows not shown, and why.** `hook_started` and `hook_progress` pass too quickly to read.
    `files_persisted`, `post_turn_summary`, `control_request_progress`, `session_metadata`,
    `mirror_error` and `transcript_mirror` are Claude Code's own bookkeeping or its cloud's.
    `system/thinking` repeats the thinking block. `tool_use_summary` repeats the calls Slopty
    already groups. `autocompact_state` is already covered by the context's categories.
    `elicitation_complete` needs no row, because Claude Code withdraws the form itself.
  - **Its dialogs are requests, and every one is shown.** `initialize` declares 21 dialog kinds
    (`supportedDialogKinds`). It leaves out the permission prompts, which come as
    `can_use_tool`, and `local_jsx`, which carries no data. It also leaves out the elicitation
    dialogs, which come as `elicitation`. A dialog is a request of kind `dialog:<kind>`, its
    payload as it came. It is answered by an option, by a JSON object as its result, or by
    `cancel`. An MCP server's form is an `elicitation` request with its schema, its fields in
    the server's order. It is answered with what the person filled in, `decline` or `cancel`.
    `initialize` also asks for nested subagent calls, progress summaries and a stop
    affordance for each task.
  - **The drive runs as the desktop app does.** The drive adds `--forward-subagent-text`,
    `--include-hook-events` and `--prompt-suggestions`, plus
    `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS`, `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING` and
    `CLAUDE_CODE_MCP_APPS_HOST`.
  - **`!` runs on the worker.** The command runs in the thread's folder for at most 10
    minutes, with 30 000 characters of output kept. While it runs it is a call of its own,
    and it is then handed to Claude Code as the person's message (`<bash-input>`,
    `<bash-stdout>`). With `respondToBashCommands` off it goes with `shouldQuery: false`.
    Claude Code 2.1.301 never echoes that message, so the thread takes it in as sent.
  - **Rewinding uses Claude Code's own checkpoints.** `rewind_files` puts back the files
    changed since the turn's message and says how many it put back. A message with no
    checkpoint says why not. Slopty never takes a dry run: the person sees the result.
    Branching the conversation at a turn is `--resume-session-at` that turn's last record,
    with `--fork-session`.
  - **Permission rules come from Claude Code and go into the files.** The rules shown are what
    `list_permission_rules` answers: their source, whether they can be edited, and whether
    they apply. A change is written into its scope's settings file (the person's, the
    project's or the local one) by an atomic rewrite. Only the list for its behaviour changes,
    but the JSON is rewritten with its keys in alphabetical order (`serde_json` keeps no
    order). The rules are then listed again.
  - **The person's status line runs on the worker.** It runs from their own settings, as
    Claude Code runs it: with Claude Code's documented JSON on stdin, 300 ms after the thread
    changes, one run at a time, for at most 5 s. Its words keep their colour.
  - **A plan approved "with the context cleared" is one verb.** Slopty denies the plan with an
    interrupt, as the TUI does. It then sets accept-edits, sends `/clear`, and sends
    "Implement the following plan:" with the plan.
  - **MCP Apps.** `mcp_status` names the tools whose `_meta.ui` points at a `ui://` resource
    (it is asked again whenever `init` reports a server's status changed). A call of one is
    its app's page, and the call's result fills the page. A page's read goes as
    `mcp_read_resource`, and its call as `mcp_call` with `result_not_for_model`, so the
    result reaches the page and never the model. Claude Code holds the connection, and asks
    still waiting when it ends are refused.
  - **Mods' surfaces.** Slopty attaches as a `desktop` surface (`ui_attach`) and draws the pane
    roster and the band above the prompt (`ui_panes`, `ui_render`). Each element that can be
    pressed or edited gets an id, `<plugin>:<handle>`, which the person's act names
    (`ui_press`, `ui_input`, `ui_select`, `ui_close`). A surface is drawn again after each act
    and on `ui_invalidate`. Toasts, status segments and log lines are shown as they are
    pushed. A Claude Code with no mods refuses these asks, and Slopty says nothing.
  - **Proof.** `cargo xtask fixtures claude-stream` records `doors.jsonl` from the official
    2.1.301 build against the canned API. It covers `mcp_status` with the app's
    `_meta.ui.resourceUri`, a page read and a refused read, two `mcp_call`s for the page, the
    rules as `cliArg` lists them, `background_tasks` with nothing to move, `ui_attach` and
    `ui_panes`, a background command stopped by its task id (`task_updated` killed, notice
    `stopped`), a `shouldQuery: false` message that is answered at once with no echo and no
    API call, and `rewind_files` answered with `canRewind` or refused. The stand-in
    (`slopty_testkit::claude_stream::doors`) scripts every one of these messages and controls,
    failures included.
  - Tests: `slopty-agent` `claude::driven::tests::{recorded_doors_answer_as_claude_code_did,
    a_turn_left_waiting_on_its_background_agent_says_so}`; `slopty-worker` `claude.rs`
    `{a_background_command_is_stopped_by_its_task_id,
    a_blocking_call_goes_to_the_background_and_the_turn_ends,
    what_claude_code_says_beside_the_conversation_is_the_threads,
    a_dialog_and_a_form_are_answered_on_their_ids, a_bang_commands_output_reaches_claude_code,
    files_go_back_before_a_turn_and_a_fork_shares_the_turns_before,
    the_persons_status_line_is_the_threads,
    a_permission_rule_goes_into_its_file_and_back_as_listed,
    a_mods_surfaces_are_drawn_and_acted_in,
    a_plan_approved_with_the_context_cleared_runs_from_a_clear_context,
    a_visual_claude_code_shows_is_the_threads_once_checked}`, `thread::claude::settings::tests`,
    `thread::assets::tests`; `slopty-workerd` `threads.rs`
    `a_claude_code_page_asks_its_app_through_claude_code`.
- ✅ **A code rewind can be undone; a command's edits and why Claude Code asks are shown (2026-10-16).**
  - **Undoing a rewind.** Before `rewind_files`, the worker records the folder's tree as a
    private snapshot (`refs/slopty/threads/<thread>/rewound`) and sets
    `ThreadState::rewound` (`turn`, `tree`, `at_ms`). `Intent::Unrewind { tree }` (needs
    `Cap::REWIND`) puts that tree back: removed files return, files the rewind brought back are
    removed, and modes are set. It names the tree it undoes, so an undo from a stale view is
    refused, and so is an undo when nothing is rewound. The undo is offered until a turn
    begins after the rewind. Undoing a rewind on top of new work would throw that work away,
    so the snapshot stays as the record and the offer ends. Claude Code's checkpoints only go
    backwards, which is why Slopty keeps the forward tree itself.
  - **A command's edits.** Claude Code's `tool_use_result.bashEditDiff` becomes
    `ExecDetail::edits` (`ShellEdits`): each file's patch, with created or deleted marked, how
    many more there were, and whether the diff was unavailable, skipped or shared. The review
    shows which files a shell command changed, as it does for Edit. `moves_tree` marks a git
    step that may move the whole tree (checkout, reset, rebase, switch, stash, merge, pull,
    also after `-C`, env assignments or `&&`), so the review knows the diff is not the whole
    story.
  - **Why it asks.** The permission request's `decision_reason_type`, `decision_reason`,
    `matched_ask_rule` and `decision_reason_rule_source` become `Request::reason`, written as
    the TUI writes it. It names the auto-mode classifier and its reason, the ask rule that
    overrides auto mode, the permission rule and the settings it came from, or the hook. The
    dock shows it above the choices.
  - **Hooks stay quiet.** `hook_started`, `hook_progress` and a hook that succeeds silently
    show nothing. A hook's output, timeout, blocking error or failure is one HOOK notice in
    the TUI's words.
  - Tests: `slopty-workerd` `threads.rs` `a_rewind_of_the_files_is_undone`; `slopty-agent`
    `conversation::tests::the_files_a_command_changed_are_its_calls`,
    `claude::driven::tests::why_claude_code_asks_is_said_as_its_tui_says_it`; `slopty-worker`
    `claude.rs` `a_message_is_delivered_and_a_permission_answered_by_its_id` (reason and
    edits).
- ✅ **Claude Code's cards, modes and settings in its TUI's words (2026-10-16).**
  - **Permission cards.** Each card's title and question are the TUI's for its tool: "Bash
    command" with the command and what it is for, "Edit file" ("Do you want to make this edit
    to main.rs?"), "Create file", "Overwrite file" or "Write file" by whether the file is there
    (the worker's one look at the disk), "Edit notebook", "Read file", "Fetch" with the URL,
    `Use skill "x"?`, "Enter plan mode?", and `{server} - {tool} (MCP)` with its parameters.
    Past 4,000 characters the parameters are not shown, and the card says how many characters
    there are and to deny unless expected. A request whose "always allow" Claude Code holds back
    (`suppress_always_allow_rule`) offers none. An ask a plugin's run raised names it under why
    it asks ("Plugin deploy").
  - **Plans (ruling P9).** A plan offers the TUI's choices in its order: "Yes, clear context (42%
    used) and auto-accept edits" first, then the mode the request suggests ("Yes, auto-accept
    edits", "Yes, and use auto mode", "Yes, and bypass permissions"), "Yes, manually approve
    edits" and "No, keep planning". The clear keeps bypass permissions or auto mode when plan
    mode was entered from them, else accepts edits; its choice id is `clear`, or `clear:<mode>`.
    It is one verb: the plan's call is turned down, the mode set, `/clear` sent, then
    "Implement the following plan:" and the plan. Entering plan mode asks "Yes, enter plan mode"
    or "No, start implementing now".
  - **Allow, and say what's next.** The TUI's "Yes, and tell Claude what to do next" is an allow
    with the person's words: the call goes ahead, then the words go into the turn under way as
    a steer, the way a message sent while it works goes. Claude Code's allow carries no message,
    so a second door is the only honest one.
  - **Mode marks.** A mode Claude Code changed itself (a plan approved with a mode, plan mode
    entered or left) is a quiet transcript row, "Switched to accept edits". The person's own
    switch is not marked, since the footer says it. Plan mode in a session begun in bypass
    permissions says it will ask before almost nothing, in the TUI's words. A refused switch
    says "Couldn't switch to auto mode: {error}".
  - **Model, effort, fast mode and thinking.** `get_settings` is asked after `initialize` and
    after each model or effort change; `applied.ultracode` where `ultracodeAvailable` becomes
    the fact `claude-ultracode`. `fast_mode_state` from `initialize` and every result is the
    fact `claude-fast` (`on`, `off`, `cooldown`). Its moves are passing notes in the TUI's words
    ("Fast limit reached and temporarily disabled", "Fast limit reset · now using fast mode",
    "Using standard speed · {Claude Code's reason}"). Thinking is offered off only where the
    model says it can be (`supportsThinkingOff`), through `set_max_thinking_tokens`, with "Thinking
    on" or "Thinking off" as its note. Failures say "Model switch failed: {error}" and "Effort
    setting unavailable right now · nothing changed: {error}".
  - **Rows the TUI derives.** A turn that ends with work running says what it waits on, one
    line: "Waiting for 2 background agents to finish · 1 background task still running".
    `agents_killed` names the agents stopped together ("2 background agents were stopped:
    \"Survey\", \"Review\""). A goal reads "Goal not yet met… continuing" with its reason, "Goal
    achieved (5m · 3 turns)" with its condition, and "Goal paused · usage limit reached ·
    continues automatically when it resets"; a goal the person cleared (`/goal clear`) is no
    achievement, and Claude Code says it was cleared itself. A failed compaction says why
    ("Could not compact · conversation too long · /clear to start fresh"). A request with no
    answer at all says how long it waited. An MCP server that fails or wants authentication,
    a usage limit nearly used up, usage credits drawn on, and a settings file Claude Code skipped
    for its errors are passing notes.
  - **Parked agents: not applicable.** The TUI's "interrupted by the user and parked" happens
    only in its interactive session. Under `-p` an interrupt stops background agents
    (`agents_killed`), so Slopty says they were stopped, never parked.
  - **Proto.** `Offered::preview` (a question option's preview), `Notice::MODE`,
    `ExecDetail::interpretation` (Claude Code's `returnCodeInterpretation`: grep's exit 1 is "No
    matches found", no failure), and `once()` never picks a choice that stops the turn.
  - Tests: `slopty-agent` `claude::driven::tests::{a_plan_reads_as_the_tuis_dialog_and_its_clear_keeps_the_mode,
    an_allow_with_words_goes_ahead_then_says_them, the_model_says_how_it_stands,
    a_turns_leftovers_stops_and_goal_say_what_the_tui_says, a_held_back_always_allow_is_not_offered,
    a_failed_compaction_says_why, an_mcp_server_that_drops_says_so,
    a_request_with_no_answer_says_how_long_it_waited,
    a_limit_nearly_used_and_credits_drawn_say_so, a_plugins_ask_names_the_plugin}`,
    `claude::history::cards::tests`,
    `claude::driven::model::tests`.
- ✅ **A request only the person at its card answers (2026-10-16).** Claude Code marks a
  permission request `requires_user_interaction` when one-tap answers must not be offered. The
  adapter keeps it as `Request::needs_presence` (and `RequestCard::needs_presence` in the table),
  and offers no "always allow" with it. Only the person's own click or key on the card answers
  it; every other path is refused:
  - a note's buttons: none are offered (`RequestCard::answerable` is false and
    `NoteChoice::of` is empty), so neither a phone push nor the Mac's note shows them, and a tap
    that comes anyway says "Needs you at the card" and leaves the request waiting;
  - an answer from no client, meaning orchestration's `answer_request` from the CLI or the
    tools, or a note's verdict through the server: the worker refuses it with "Needs you at the
    card: the agent takes this answer only from its card in Slopty";
  - another agent: the hub already refuses any agent's `answer_request`;
  - a rule: "Allow with rule" is refused for it, and none is offered;
  - a page: pages have no answer route.

  Slopty has no auto-allow of its own by rule or mode. Claude Code's own modes decide before it
  asks. The card carries a quiet "Needs you at the card" chip.
  - Tests: `slopty-agent` `claude::driven::tests::a_request_needing_the_person_is_held_for_the_card`;
    `slopty-proto` `units::{a_notes_buttons_answer_only_a_plain_yes_or_no,
    a_small_question_is_answered_by_its_options}`; `slopty-workerd` `threads::tests::only_the_card_answers_a_request_that_needs_the_person_at_it`;
    `slopty-server` `hub::agent_tests::no_agent_answers_a_request_that_needs_the_person_at_its_card`;
    `slopty-ui` `workspace::approvals::tests::a_note_answers_nothing_that_needs_the_person_at_the_card`,
    `workspace::view_model::work::tests::no_rule_allows_what_needs_the_person_at_the_card`,
    `view_model::tests::a_request_needing_the_person_reaches_its_card_as_one`; `slopty-shell`
    `composer::dock_tests::a_card_says_it_needs_the_person_at_it`.
- ✅ **The TUI's words from Claude Code's own doors, second round (2026-10-16).** What the TUI
  says of a call, a request or the session now reaches Slopty, read only from what stream-json
  and the control protocol carry:
  - **Calls in the TUI's words.** A call's header and result line come from its input and its
    `tool_use_result` (`conversation/said.rs`): "Read report.pdf · pages 1-3", "Create n.md",
    "Read PDF (1.2MB)", "Did 3 searches in 2s", "Staged for your review…", "Cloud agent
    launched", the scheduled and listed cron jobs, worktree switches, MCP resource reads, a
    large MCP response warning, and more. A list the TUI draws under a call (a job list,
    findings, notifications) is the call's output. A later note keeps what an earlier one said
    and it leaves unsaid (`CallNote::over`).
  - **Server tools settle.** A `*_tool_result` block in the answer settles its
    `server_tool_use` call. The advisor's reads "Advisor has reviewed the conversation and will
    apply the feedback", with its advice as the output, or says it declined or was unavailable.
  - **Narration is text.** A thinking block whose signature names it narration (a protobuf
    field path inside the base64 signature, read as the TUI reads it) is the answer's own
    words, not a thought.
  - **What Claude Code puts in the person's place** is its own row: a stop hook's feedback, a
    plugin's prompt ("Prompt from the reviewer plugin"), a command's standard error apart from
    its output, an MCP server's resource update, and another session's message with who sent
    it.
  - **Session words.** A turn Claude Code would not start says why (`startup_failure_reason`,
    each of its reasons in words), a full context says so, and a call a hook deferred says the
    turn stopped there. A hook that allowed a call says "Allowed by PreToolUse hook". `init`'s
    plugin and MCP config errors and policy-blocked servers are said once, and the servers'
    counts are a thread fact the context card shows. Agent descriptions past 15k tokens and a
    prompt cache that expired between turns are passing notes.
  - **Permission cards.** A write over a file the worker cannot show whole says so in the TUI's
    words. Past 256 KiB it is "too large to preview". Contents that are not plain text make it
    "one-time only", and that card offers no "always allow". A symlink whose target leaves the
    folder names the target. A request Claude Code would start on "No" (`default_to_no`) lists
    its deny first, and its card starts there and takes no key. A plan approved with words is
    approved as edited (`updatedInput.plan`, the TUI's Ctrl+G). "Chat about this" turns the
    questions down with the TUI's words that the person wants to talk them over. An answer
    carries the picked option's preview and the person's notes as `annotations`.
  - Judged noise, each with no door under `-p`: a model's deprecation (no field in
    `list_models`), per-model context windows and "isn't recognized" (the TUI's own model
    table), remote managed settings' fetch status (a blocking failure comes as
    `startup_failure_reason`), a workflow's agent count (its agents have no tasks of their own),
    a subagent's queued-message count and a task's sent-message count (TUI state), the plugin
    file watcher's "Plugins changed", the retry countdown, and images in an answer (an
    `Intent::Answer` carries words; a picture goes as the next message).
  - Tests: `slopty-agent` `conversation::said::tests`,
    `conversation::tests::{what_claude_code_says_in_the_persons_place_is_its_own_row,
    the_advisors_review_settles_its_call}`,
    `claude::driven::tests::{a_write_says_what_is_on_disk_where_it_lands,
    a_plan_approved_with_words_is_the_edited_plan, chat_about_this_talks_the_questions_over,
    an_answer_carries_its_preview_and_notes, a_turn_that_cannot_go_on_says_why,
    a_hooks_allow_and_a_held_message_say_so, init_says_what_did_not_load_once}`;
    `slopty-proto` `thread::tests::a_call_note_keeps_what_a_later_one_leaves_unsaid`.

- ✅ **A listing never waits on a macOS prompt; bypass is a mode the person may pick (2026-10-16,
  night).** Opening a machine's home in New project hung for 30 s and then said "the machine did
  not answer": counting the items of Desktop, Documents, Downloads and Music made macOS ask on
  the worker's screen (tccd logged four `AUTHREQ_PROMPTING` for `dev.aislopware.slopty.worker`),
  and the read waited for an answer nobody was there to give. A listing now looks inside no
  folder macOS guards (those four, Movies and Pictures, iCloud Drive and the other File Provider
  drives, other apps' containers, mounted volumes): they are listed without a count or a
  repository mark, and only opening one reads it. A listing past 5 s answers why it waits, on
  that machine, instead of the client's generic timeout. Separately, Claude Code refused a switch
  to bypass permissions mid-session ("not launched with --dangerously-skip-permissions"): every
  driven run now starts with `--allow-dangerously-skip-permissions`, which only allows the mode,
  as the desktop app's runs do; the person still picks it.

- ⏳ **Past Claude Code sessions are seen and continued in the GUI (2026-10-16, lane R).** The TUI
  and Slopty's `claude -p` share Claude Code's own store on each machine, so a session run in a
  terminal is one Slopty can show and go on with. Claude desktop's Code tab, t3code and zeron
  each keep history apart from live work and reachable from a search; Slopty does both.
  - **History, per project.** A project group of the sidebar ends with a quiet "History"
    disclosure, shut at first; a sidebar organised by status has one History for every project
    (the filtered one's while filtered), above Archived, each row naming its project. Opened, it
    asks the project's machines (`slopty.PastSessions`, through `ThreadRequest::Sessions`) for
    Claude Code's own list of the sessions in the folder, newest first (the TUI's and
    `claude -p`'s alike). The worker names each from the two ends of its transcript, 64 KiB of
    head and 64 KiB of tail (`sessions::GLANCE_BYTES`), as Claude Code's own resume picker does:
    its title (the one the person gave it, else Claude Code's own, else its summary, else its
    first prompt), its last prompt and its branch (the `branch` fact). One ask then serves; the
    second ask of the prompt history, which scanned Codex's and pi's records too for up to 3 s,
    goes. A session that is a chat here already is left out. It
    pages by 20 behind "Show more", keeps its rows while it asks again, and says in a line of
    its own when it is reading, has nothing, or failed (the machine's words, with Try again).
    Each row's height is fixed by what it shows, so the group it closes is never clipped.
  - **Resume a session… (⇧⌘O, and the palette).** A picker in the palette's anatomy searches
    the person's past prompts on the open chat's machine, in its project or in every folder
    (Tab), with the worker's prompt search, and shows the prompt that matched with its words
    lit. With no words it lists the project's sessions as History does, or the machine's last
    prompted.
  - **Opening.** A session the machine keeps a thread of opens that chat. Any other is a start
    of Claude Code with `--resume <id>` in the folder it ran in: the worker reads the whole
    conversation from its transcript and starts nothing, so opening a past session to read it
    costs no process and touches no transcript. The thread rests as one whose Claude Code ended,
    and its first message starts `claude -p --resume <id>` with that message. The shell opens the
    chat once the machine says it started, so it shows the conversation at once. A message there
    goes to the same session, so the TUI and the GUI write one history. A refusal is said in the
    machine's words, in the picker's row or a notice.
  - **A session running elsewhere.** Claude Code records each live process in
    `~/.claude/sessions/<pid>.json` (pid, `sessionId`, `cwd`, `kind`). Its own resume refuses a
    session another live process holds ("this session is running in another terminal", offering
    a copy), since two writers interleave their records in one transcript and each goes on from
    its own copy of the conversation. Slopty does the same: such a session is marked "In a
    terminal" (or "Running") in History and the picker, opens read from its transcript with the
    plain words "This session is running in another terminal on <machine>. Close it there to
    continue here.", and every message is refused with them, so no second Claude Code starts on
    it. A start with a first message is refused whole, and the person keeps their words. Once
    the other ends (looked for every second) the thread says so, and the next message takes the
    session up. The marking and the refusal are the worker's (`PAST_RUNNING`,
    `thread::claude::held`), the listing and the search alike, from Claude Code's live records:
    a record is believed while its pid lives and, when it says when its process started
    (`procStart`, `ps -o lstart=` in UTC, as Claude Code checks it), that process started then,
    so a pid used again holds nothing. The sessions this worker's own Claude Codes run are left
    out. A session another Claude Code takes up while its thread here rests is caught at the
    next message too: the message waits in the queue, and the thread says why.
  - Tests: e2e `app::past_sessions` (`a_tui_session_is_listed_opened_and_continued`,
    `history_says_why_it_has_nothing`, `a_session_running_elsewhere_is_shown_not_taken`);
    `slopty-worker` `claude::{a_resumed_session_starts_claude_code_with_its_first_message,
    a_session_held_elsewhere_is_shown_and_not_taken}` and
    `thread::claude::held::tests`; `slopty-agent` `claude::sessions::tests::{
    a_listing_names_each_session_from_the_ends_of_its_transcript,
    live_claude_codes_are_read_from_their_records}`; `slopty-ui`
    `workspace::view_model::past::tests`; `slopty-shell` `shell::past_sessions::tests`.

- ✅ **Nothing reads a terminal's title (2026-10-17).** `slopty_agent::title` read the spinner
  Claude Code paints into a terminal's title (OSC 0/2) to tell a hand-run agent's turn from its
  rest, for orchestration's `WaitUntil::AgentNeedsInput` on a terminal. That reads an agent's
  screen, which Slopty never does (CLAUDE.md, "only through its published doors"), and every
  agent orchestration waits on now speaks through a door: it starts agents as threads
  (`start_thread`: Claude Code over stream-json, Codex over its app-server, pi over its RPC
  mode, any ACP agent), `Verb::AgentStatus` reads the thread's row, and a wait for an agent to
  need the person is `EventFilter::AgentNeedsInput` over those rows. A terminal's agent is the
  person's: orchestration may not type into it (`may_type`), so a wait on its turns drove
  nothing.
  - Deleted: `slopty_agent::title`, the title in `Observation` and in the session's probe,
    `AgentSource`, `AgentStatus::Working` (a terminal's agent is `Running` or not), the worker's
    agent broadcast (`Daemon::heard`) and `orchestrate::AgentFeed`. The 750 ms poll stays for
    the foreground process alone, which keeps the typing guard.
  - `WaitUntil::AgentNeedsInput` on a terminal is refused as `Invalid`, its words naming
    `start_thread` and `events --agent-input`, until the variant and the CLI's
    `wait --agent-input` go from `slopty-proto` and `slopty-cli`.
  - Tests: `slopty-worker` `orchestrate::{a_wait_on_a_terminals_agent_is_refused,
    nothing_is_typed_into_a_terminal_an_agent_runs_in}`; `slopty-agent` `tests::{
    a_hand_started_claude_is_known_by_its_process, a_second_claude_in_the_same_terminal_is_a_new_agent,
    the_table_keeps_only_agents_and_lets_them_go_with_their_terminals}`.

- ✅ **Agents drive the person's window through open verbs (2026-10-17).** An agent working in
  Slopty could not touch the window it ran beside: open a terminal, show a file, fold the
  sidebar. It now can, as `slopty ui …` for its shell and the `ui_*` tools of `slopty mcp`,
  the same six verbs named as the shell's own commands are: `state`, `terminal [--folder]
  [--command] [--pane]` (the tab's id and its session back, as Orca's `terminal create` answers),
  `open <file[:line] | url>` and `diff`, `show | hide | toggle | expand | collapse <region>`,
  `chat [id | prefix | title]` (resolved as zeron resolves a chat) and `split [target]`. Every
  answer is JSON: `did`, `made`, `ran` and the `window` after it (its page, the chat on screen,
  each region shown or expanded, its tabs with their ids, kinds, targets and terminals, and
  whether the person is typing), or `{"error": {"code", "message"}}`, the code one of
  `no_window`, `unknown`, `busy`, `elsewhere`, `failed`, and the message what to do instead.
  - Route: the ask is a handoff, as `slopty browse` and `slopty edit` are. The CLI writes
    `CtlRequest::Ui` to its machine's worker (`SLOPTY_WORKER_SOCKET`) with its session; the
    worker hands `HandoffEvent::Ui` to the clients that took handoffs, in the handoffs' order
    (the client in front of that session, then the last typist, then one on any of the worker's
    tiles, then the most recently active), each given `UI_WAIT`; a client answers with
    `HandoffReply::Ui`. So only a window following this worker's fleet, the person's own, is
    reached; a terminal client (`slopty attach`) refuses, and the next is asked. With no client
    connected, none with a window, or none answering, the agent hears `no_window` and why; an
    ask that reached its window late (`LATE_AFTER`) is refused rather than done out of time.
  - The person stays in charge. No verb takes the keyboard: the window puts it back where it
    was. What would move the chat or the keyboard from a draft being typed (`chat`, expanding the
    pane over the composer), go past Settings, or hide the region the keyboard is in is `busy`.
    Paths are the agent's machine's, so a file, a folder or the diff opens only beside a chat of
    that machine, else `elsewhere`; a loopback page is shown through the client's forward. Each
    change shows a notice naming the agent's chat ("<title>: opened …"). A verb never answers
    for the person and never reads or types a credential: a terminal's command that acts as
    root, destroys or spans lines waits at the prompt (`ran: false`), as the run buttons do.
  - Open names: regions are words (`sidebar`, `pane`, `files`, `terminal`, with their common
    aliases), and an unknown one is answered with the list the window has, so a later region is
    a new word, not a new type. Claude desktop scopes its `preview_*` tools to the session's own
    pane and answers in plain text; t3code keys previews by thread; here the window is one, so
    the verbs reach the whole window and answer in data an agent can act on.
  - Tests: e2e `app::agent_ui` (`an_agents_tools_lay_out_the_window`,
    `the_window_stays_the_persons_and_says_why_it_refuses`); `slopty-shell`
    `shell::agent_ui::tests`; `slopty-ui` `workspace::handoffs::tests`; `slopty-client`
    `handoff::tests::an_agents_ask_is_the_uis_unless_it_came_late`; `slopty-proto`
    `handoff::tests::an_agents_ask_and_its_answer_cross_the_link` and
    `ctl::tests::an_agents_ask_of_the_window_crosses_the_socket`; `slopty-tools` `ui::tests`
    and `tools::tests::the_window_tools_reach_the_window_through_their_carrier`; `slopty-cli`
    `ui::tests`.
