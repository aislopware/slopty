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
- ✅ **Only Claude Code has an inspector (2026-10-30).** The person chose to keep the inspector
  simple: it decodes Claude Code's transcript and no other agent's records. A Codex or pi session
  is its terminal and its row, with no inspector and no decoder of Codex's rollout files or pi's
  session files (`inspector.rs`, `inspects`). This withdraws an earlier entry of the same day
  that gave them the same trace.
- ✅ **How the stable base is built (2026-10-30).** Choices the ruling left open, made while
  building "Stable above all":
  - **A group is keyed by its first record.** A run of calls and thoughts is one row keyed
    `{entry}#g:{first part id}`, so a group that grows, or that a spawn splits as its input
    lands, keeps its key, its measured height and its fold. A key by position would move every
    later group's fold when one split.
  - **The follow rule rides GPUI's tail mode, with no spring.** While following, the list's
    `FollowMode::Tail` snaps to the end on every layout, so growth at the end never shows a
    frame off it, and an upward move caused by layout cannot happen. The person lets go with a
    wheel or touch up past 2 pt, a scroll key or jump, a scrollbar drag, or a press or
    selection in a row; letting go pins the first row in view to a concrete offset, and after
    every splice that row is found again by its key and put back where it stood, so a splice
    that replaces the rows around it moves nothing. Scrolling back within 2 pt of the end
    follows again. The velocity spring, its 70 pt restick band and the own-send runway are
    deleted: the desktop app snaps rather than glides (desktop §1.4), and the runway ran with
    no caller.
  - **The jump pill lives in the transcript.** It shows while the view does not follow and
    rows arrived below since it let go, or it stands more than 320 pt from the end, and says
    "↓ 12 new steps", counting rows (a group is one step). A press follows the end again.
  - **The fixed lines are always drawn.** The header, Now and Changes are 28 pt lines whether
    or not they have anything to say ("Nothing", "None"), so a session starting or resting
    moves nothing under them. Now and Changes open a card under their line, which Esc or a
    press outside closes and which gives the keyboard back to where it was.
  - **The prompt box is the list's footer.** It is drawn under the last row rather than as an
    item of its own, so every list index stays a row's and the follow rule treats its growth as
    the end's.
- ✅ **How subagents are built as nested steps (2026-10-30).** Choices the ruling left open, made
  while building "A subagent is a nested step, opened in place":
  - **A spawn cuts its group.** A run of calls stops at a spawn, which becomes a row of its own
    keyed `{entry}#a:{call id}`. Spawns that stand together get one line over them, keyed by the
    first, `{entry}#p:{call id}`. A batch that grows keeps its line's key, and a spawn that lands
    late splits its group without moving the folds of the rows around it. The view model's
    fold of agents started together (`folds.rs` `agent_groups`) is deleted: the line is the
    transcript's to draw, from the rows that stand together.
  - **Opening in place splices the child's own rows.** The child's record goes through the same
    row builder as the parent's, and each spliced row keeps its depth for its inset: 22 pt a
    level, the chevron's gutter, so a child's icons stand under its parent's title, for up to
    four levels (traces §2). Below that, rows keep the fourth level's inset. Hairlines on the
    activity trunk show the levels. A child row reads compact, as a drilled run does. Its brief
    folds to three lines and a press opens it. Until its record arrives, "Loading its steps…"
    holds the place. Folding a run shuts the runs opened inside it and lets go of their feeds.
    The nested card, its scroller and the frame-late measure (`Fit`) are deleted.
  - **A press opens in place; Enter and Open drill.** A press or Space opens a run in place, as
    any step opens. Enter, or the row's Open (↗, shown on hover or focus), drills, as a trace
    viewer's Enter goes into a span. A press in the inspector gives its pane the keyboard, as a
    split in a terminal takes it, and a drill then puts the keyboard on the way back, because the
    row or Open that held it goes with the trace it was in. Esc there goes back a level and
    never reaches the TUI, where it would interrupt the agent. After a way back the keyboard
    lands on the way back still shown, or on the header's line. A drill asked while the terminal
    holds the keyboard leaves it there. A press on the prompt box's mirror, which is no row,
    leaves the view following the end.
  - **What the person opens comes into view.** The view holds still under what grows by
    itself, but a group, a step's detail or a run the person opens at the end of the trace would
    open out of sight below it, since the trace's reading place is its end. Once what opened has
    laid out, the view moves down as far as its end needs, never so far that the row pressed
    leaves the top. It keeps up through a fold's tween, and a run opened before its record
    arrived comes into view when the record lands.
  - **The drilled header.** It holds the way back ("←", named "Back to {parent}"), crumbs
    ("Session › {parent} ›", each a press back to its level), the run's task and its status
    mark, all on the 28 pt header line. The Now line then says the run's step, what it asks of
    the person ("Needs you · …"), or its failure, then its facts, and opens nothing. The header's
    time moves on once a second while the run runs. Changes stays the session's.
  - **The stack owns the feeds it opened.** A drilled run's record is watched while it is on
    the stack. Going back lets go of it, and of the runs opened in place inside it, unless a
    trace still held shows one of them. A new selection empties the stack. Drilling a run that
    is already on the stack goes back to it instead of pushing it twice.
  - **The prompt box's mirror moves to the trace shown.** It stays the session's prompt box, the
    one the person types into, so it stands at the foot of whatever trace is shown.
