# Inspector

The inspector is the read-only view of a running agent session, summoned beside its terminal
(⌘I; `layout.md`, "No permanent right column"). The person works in the agent's own TUI
(`agents.md`, "TUI-first"); the inspector is where they look into what the session did. The
studies behind this file are in `.research/inspector-2026-10-30/`: `claude-desktop.md` (Claude
4.4212.0), `t3code.md` (T3 Code at `32ecd717`), `amp.md` (Amp 1.0 build 1010), `traces.md`
(agent, LLM and distributed trace viewers), and `ours.md` (an audit of Slopty's own inspector at
`3a8147164`). Each item below names its source section.

- ✅ **The inspector is a trace of the session (2026-10-30).** The person asked for the inspector
  to be redone after Claude Code desktop, T3 Code and Amp: simple, complete in its detail, and
  stable. Their example was that a subagent is hard to read. Then they set the shape: everything
  the TUI shows must be viewable here too, in more detail and easier to read into, as one looks
  at a trace. So "the TUI already shows it" is never a reason to leave a thing out. Every turn,
  tool call, subagent and subagent call is a step with its time, its duration, its status and
  its full input and output, nested as it ran, and the whole reads at a glance before anything
  is opened. The hard lines stay: it is read-only; it reads the agent only from its records and
  published hooks; and it never answers for the person or types into the TUI.

  The audit's structural finding decides the base (ours §0). The whole-turn divider ("compact
  mode") hid every step of a running turn and moved the reply out of the work at the turn's end.
  It put errors and notices after all the work, whatever their time. It stacked a second divider
  inside every subagent, and it needed fold-height probes, a settle sweep, a phase swap and a
  held band to look still. The rows it opened to (`work_rows`) are already the trace shape.
  - **Rows in record order, never re-sorted** (t3code §1.6, amp §6.1, traces §7). The timeline
    is the session's records in the order they were written: the prompt, then the narration,
    thinking, calls, subagents and events (errors and retries, interrupts, compaction, task
    notices, and the permission asks and their answers, shown and read-only), each where it
    happened. A late record lands in its place and is never moved. A fold only hides rows, and
    no row ever changes container, so the turn's end moves nothing. The whole-turn divider is
    deleted.
  - **The turn is the root row** (traces §10.2, desktop §5.3). Each prompt opens a root row that
    carries:
    - the prompt's first line;
    - the state, said only when the turn did not end well;
    - the duration;
    - the models;
    - the tokens (in, cached, out, with the split by kind in the tooltip);
    - the lines added and removed.

    All of it comes from `Turn`, which already holds it. A failed turn's error is said on its row.
  - **A step row reads at a glance** (traces §2.1, §10.1; desktop §5.3; amp §6.2). Each row has:
    - the kind's icon;
    - the TUI's own name for the call;
    - its subject;
    - a one-line result ("31 files", "exit 101 · 3 failed", "+12 −4");
    - its status mark, said only when the step did not end well;
    - its duration, right-aligned.

    The duration comes from the call's record and its result's (`Item.at_ms`,
    `ToolCall.ended_ms`). The tooltip carries the time of day and the start relative to the
    turn; holding ⌥ shows the time on every row in the trailing lane, which is reserved, so
    nothing moves (amp §3). A failure inside a folded group or subagent shows on the folded row
    (traces §9).
  - **Runs of calls fold to one sentence** (amp §6.2, desktop §5.3, t3code §6.4.5). Consecutive
    calls read as one group line, "Edited 2 files (+40 −12), ran 3 commands (1 failed)". The
    narration written before or after them stays outside, where it was said. A group failed
    only if all of its calls failed; otherwise it names the failures. A settled group's sentence
    is computed once and kept. Opening the line shows its step rows. Thinking is a step too:
    "Thought for 4s", opening to the summary.
  - **One fold depth for the whole trace** (traces §10.2). A control beside the session
    summary, with `[` and `]`, sets the depth: Prompts (each turn its root row and its last
    reply), Steps (the default: groups folded, narration and subagent rows shown) and All
    (every step open). A depth change reorders nothing. Folds the person opens are kept in one
    set keyed by step id, so they survive updates, windowing and reopening (amp §6.1). Nothing
    opens or closes by itself: not at the turn's end, not when a subagent starts, not on a new
    record.
- ✅ **A subagent is a nested step, opened in place (2026-10-30).** The person's example (ours
  §2; desktop §2; t3code §2; amp §5; traces §4.2). Today a spawn is a card that opens and closes
  by itself, by up to 480 pt each way. Its run sits in a nested scroller that measures a frame
  late. Three folds stand between the reader and a running subagent's first call. And drilling
  in leaves the session: the keyboard moves to the agent's TUI, where the next Esc interrupts
  the agent. All of that is replaced:
  - **One row, like every other call.** It carries:
    - the agent icon;
    - the subagent's type and its description (the brief's title);
    - while it runs, a heartbeat from the subagent's own records, "Sonnet · Grep · 12 calls"
      (desktop §2.1), with "quiet 3m" once nothing has come for a while (traces §10.3);
    - once done, the first line of its report;
    - its figures: model, tool tally, tokens, duration;
    - its status.

    Its height is a step row's. It never opens by itself.
  - **Opening it splices its steps in under it**, one level indented, in the same list:
    - its brief comes first, folded to three lines;
    - its steps follow, in the same rows and grammar as the parent's;
    - its report comes last.

    The list's own virtualization covers a run of 300 calls; there is no scroller inside a
    scroller. Folding it hides those rows again.
  - **Parallel spawns share one line over them** (t3code §2.3, traces §4.3), "3 Explore agents ·
    2 working · 1m 12s", then "3 agents finished · 2m 5s" or "1 failed". Under it, each agent
    keeps its own row.
  - **A long or nested run drills in, inside the inspector** (desktop §2.3–2.4; t3code §2.4).
    Enter on a subagent row pushes its trace onto a stack that belongs to the inspector. The
    stack shows a crumb, "‹ {parent's title}", and one line of the child's facts: kind, model,
    status, tools, tokens and time. A nested agent pushes again, and Esc or the crumb pops one
    level. A child that is still running opens at its newest step, a finished one at its brief.
    The terminal, the keyboard focus, the sidebar's selection and the prompt box's mirror stay
    where they were. The drill through the shell's `open_chat` and "Parent session" are deleted.
  - Each subagent is found by Claude Code's own records: the spawn's `tool_use_id` and the
    subagent's `agent-<id>.meta.json` (`toolUseId`, `parentAgentId`) (desktop §0).
- ✅ **Any step opens to its whole detail (2026-10-30)** (traces §3, §10.4; desktop §5.3; t3code
  §6.4.6). Opening a step shows its detail, by kind:
  - the whole input;
  - the whole output: a command with its exit code, a diff inline, the file's lines, the
    search's matches, JSON as a tree, markdown rendered;
  - "Details for Claude" where the tool told the model more than it showed;
  - a Timing section: started, ended and duration, with the wait for the person's permission
    apart from the run time;
  - the raw record.

  A format menu keeps its shape and remembers the choice per kind. A payload past its size gate
  shows its head and "+ N lines" until asked. A running command's output (a background task's
  file) sits in a capped box that follows its own bottom, so the timeline holds still (amp §4).
  An empty or failed fetch says so: "No output", "Couldn't load output: …" (t3code §6.5).
- ✅ **Stable above all: the live contract (2026-10-30)** (desktop §1; t3code §1; amp §6.1;
  traces §7; ours §1). These are the rules that keep the view still while the agent works and
  while the person reads:
  - **Keys.** Rows are keyed by record id: a call by its `tool_use_id`, a turn by its prompt's
    id. The live row has one fixed key, so it never remounts. Measured heights are kept by key
    and seeded on reopen.
  - **Follow and anchor.** The view follows the bottom only while within 2 px of it. It lets go
    only on the person's clear intent: a wheel or drag upward, a scroll key, a scrollbar press,
    or a click or selection in a row. An upward move caused by layout snaps back. Otherwise the
    first visible row is held in place by its key, whatever grows above or below it. Opening a
    fold leaves the rows above it where they were. While the view is not following, a pill
    says how much waits below ("12 new steps").
  - **Fixed frame.** The header, the Now line and the Changes line are one fixed line each, and
    the mirrored prompt box is the list's last row, so typing in the TUI never lifts the
    timeline (ours S1).
  - **A closed turn never changes.** Its rows are computed once and reused.
  - **One clock.** Only a running step's duration ticks, on one shared tick aligned to whole
    seconds. A live label holds at least 650 ms, so parallel calls never flicker.
  - **Frame time.** The view borrows the transcript and recomputes by revision, with no copy
    per frame (ours S12).
  - **Reading place.** Reopening the inspector restores the reading place, unless the session
    moved on in the last second.
- ✅ **Keys and find (2026-10-30)** (traces §5, §10.5). ↑ and ↓ move over the visible steps, ← and
  → fold and open, Enter opens or drills, Esc goes back, `[` and `]` set the fold depth, and one
  key goes to the next failure. ⌘F finds over the whole trace, folded rows included: it opens
  the ancestors of the match it lands on and steps through the matches with a count. Two
  filters, failures only and by kind, share the find's definition of a match.
- ✅ **What the records give the inspector (2026-10-30)** (traces §8.4; ours §4). The records
  already hold most of the trace, and what the thread model drops is carried:
  - each settled call's end (`ToolCall.ended_ms`) into the row's facts;
  - the hooks' `tool_use_id` and `duration_ms` (`PostToolUse`, `PostToolUseFailure`). With them,
    a call's run time is told apart from the wait for permission. The relay already hears
    these events and reads them for status only;
  - a subagent's resolved model and tool tally (`resolvedModel`, `toolStats`) into
    `AgentDetail`;
  - the turn's permission mode and the context its last request carried. The decoder's own doc
    already claims these (`vocabulary.rs:14-17`), so either decode them or correct the doc;
  - whole inputs and whole thinking, by reference, as whole outputs already are.

  The records cannot give a request's start or its time to first token, a money figure, the
  model's full input (system prompt, tool schemas), or a foreground command's output while it
  runs (traces §8.5). The inspector shows none of those rather than guess them.
- ✅ **Left out, and why (2026-10-30)**. Each rating below is against the trace shape:
  - **Harmful**, because each acts for the person, writes to the session or sends it away: allow
    and deny, answering a question, undo, stop, fork and rewind, retry, editing a message
    (desktop §5.3; amp §6.2), a dollar cost (wrong for a subscription; `Usage` keeps none),
    scores and annotations, and any AI analysis of the trace (traces §9).
  - **Rare**, kept out for now: a waterfall column on every row, since the pane is narrow,
    durations span six orders of magnitude and a live axis moves every bar each tick (traces
    §9, Phoenix drops it under 500 px); one row per model request (its time and tokens go in
    the detail and the turn instead); graph, flame and statistics views.
  - **Later**: a per-turn timing strip on true time, idle gaps compressed, frozen when the turn
    ends, for parallel subagents (traces §10.7).
  - **Deleted, dead or drawn twice** (ours §3), along with the whole-turn divider, "Show work",
    the self-opening subagent card and its in-place child:
    - the own-send runway, which runs every frame with no caller;
    - the send entrance and the queued tint;
    - "Run in terminal" on code fences, which would type into the agent's TUI if it were served;
    - the fork seam and `subagent_tail`;
    - the jump pill that was computed and never drawn (it becomes the "N new" pill);
    - the per-turn changes card, which Changes already draws;
    - the header's second time at work.

  The plan card, find's landing flash, the hover strip, compaction rows with their summary,
  retry countdowns, the "Claude Code too old" banner, inline pages, visuals and pictures, and the
  prompt box's lightbox are kept (ours §3).
- ✅ **Codex and pi get the same trace (2026-10-30).** The proto's thread model is agent-neutral
  (traces §8.1), and the inspector draws whatever thread a session has. What is missing is a
  decoder of each agent's own records into that thread: Codex's rollout files and pi's session
  files, published by each, as `conversation.rs` decodes Claude Code's transcript. Each gets one,
  pinned by recorded fixtures as Claude Code's is. Until then the inspector serves only Claude
  Code (`inspector.rs:959-968`; ours §4).
