// Slopty's mod for Claude Code: the TUI's prompt box, read live for Slopty's inspector.
//
// It reads and never writes: each change of the draft (a typed edit, a paste, a recalled
// history entry, a clear, an undo) is handed, after a pause in typing, to Slopty's relay as a
// `PromptDraft` hook payload, with the temporary root its pasted pictures are kept under. Nothing
// is typed, drawn, answered or shown in the TUI. Everything else (what a draft is, its pictures,
// the session it belongs to) is Slopty's own, in Rust.
import type { EngineInterface, Register, Timer } from 'claude-code'

// The relay, `<slopty> hook`: the worker writes its own CLI's path here (`draft::install`).
const RELAY = ['/slopty', 'hook']
// A pause in typing before the draft goes.
const QUIET_MS = 150
// Under steady typing it still goes about this often: QUIET_MS times this many keys.
const MOST_DEFERRED = 6
// Longer than `slopty hook` ever waits on the worker, so a wedged worker leaves no process.
const RELAY_TIMEOUT_MS = 5000

// Stryker disable next-line StringLiteral: equivalent, every flush follows a change that set it
let latest = ''
let relayed = ''
let deferred = 0
let timer: Timer | undefined
let busy = false

// The draft changed to `text`: send it once typing pauses.
function changed($: EngineInterface, text: string) {
  latest = text
  if (timer !== undefined && deferred < MOST_DEFERRED) {
    timer.cancel()
    timer = undefined
    deferred += 1
  }
  if (timer === undefined)
    timer = $.clock.after(QUIET_MS, () => {
      timer = undefined
      deferred = 0
      void flush($)
    })
}

// Hand the newest draft to the relay, one at a time, and only when it differs.
async function flush($: EngineInterface) {
  if (busy || latest === relayed) return
  busy = true
  const text = latest
  try {
    const session_id = await $.session.id()
    // Where the TUI keeps a pasted picture, under its own temporary root when one is set.
    // Stryker disable next-line StringLiteral: the engine reads each variable's name off the source
    const tmpdir = await $.env.get('CLAUDE_CODE_TMPDIR')
    const stdin = JSON.stringify({
      hook_event_name: 'PromptDraft',
      session_id,
      prompt: text,
      tmpdir,
    })
    await $.process.run(RELAY, { stdin, timeoutMs: RELAY_TIMEOUT_MS })
  } catch {
    // No relay heard it (outside Slopty, or no worker): the next change tries again.
  } finally {
    relayed = text
    busy = false
  }
  void flush($)
}

export const register: Register = (on) => {
  // Stryker disable next-line StringLiteral: the engine reads each event's name off the source
  on('prompt.edit', async ($, e, next) => {
    const box = await next(e)
    // A caret move changes no text: it neither sends nor holds back a draft waiting to go.
    if (box.text !== e.text) changed($, box.text)
    return box
  })
  // Stryker disable next-line StringLiteral: the engine reads each event's name off the source
  on('prompt.change', async ($, e, next) => {
    const answer = await next(e)
    changed($, e.text)
    return answer
  })
}
