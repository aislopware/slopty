// The weekly model-based property, which `cargo xtask claude-mod check --deep` runs as
// `tests/model.test.ts` (under this name `claude plugin test` passes it by): random schedules of
// the person's acts and the relay's answers, each replayed exactly from its seed on the kit's
// clock. Over every schedule the mod must hand each box back as the editor left it, run one
// relay at a time, never send the same text twice in a row, and leave the final text as the
// last one it tried to send. It explores interleavings the examples in `tests/` do not reach and
// kills no mutant they miss, so it stays out of the per-change check.
import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type {
  PromptChangeInput,
  PromptChangeResult,
  PromptEditInput,
  PromptEditResult,
} from 'claude-code'

type Editor = {
  edit: (e: PromptEditInput) => Promise<PromptEditResult>
  change: (e: PromptChangeInput) => Promise<PromptChangeResult>
}
function isEditor(door: unknown): door is Editor {
  return (
    typeof door === 'object' &&
    door !== null &&
    'edit' in door &&
    typeof door.edit === 'function' &&
    'change' in door &&
    typeof door.change === 'function'
  )
}
function editor($: Engine): Editor {
  const door: unknown = $.prompt
  if (!isEditor(door)) throw new Error('the kit raises no prompt.edit or prompt.change')
  return door
}

// The text a relay carried.
function prompt(stdin: string | undefined): string {
  const p: unknown = JSON.parse(stdin ?? 'null')
  if (typeof p === 'object' && p !== null && 'prompt' in p && typeof p.prompt === 'string')
    return p.prompt
  throw new Error(`no PromptDraft payload: ${stdin ?? ''}`)
}

const SCHEDULES = 20
const ACTS = 40
const OK = {
  exitCode: 0,
  stdout: '',
  stderr: '',
  isStdoutTruncated: false,
  isStderrTruncated: false,
}

// xorshift32: a schedule is its seed, so a failing one replays as is.
function random(seed: number) {
  let s = seed >>> 0 || 1
  return (n: number) => {
    s ^= s << 13
    s ^= s >>> 17
    s ^= s << 5
    return (s >>> 0) % n
  }
}

function press(text: string, key: string, start: number, end: number, inputText: string) {
  const e: PromptEditInput = {
    origin: { kind: 'composer' },
    key: { key },
    text,
    cursor: text.length,
    start,
    end,
    inputText,
  }
  return e
}

for (let seed = 1; seed <= SCHEDULES; seed++) {
  test(`schedule ${seed}`, async ($, on) => {
    const pick = random(seed * 7919)
    const clock = mock.clock(on)
    mock.env(on, {})
    const tried: string[] = []
    let inFlight = 0
    let most = 0
    on('prompt.edit', (_$, e) => ({
      text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end),
      cursor: e.start + e.inputText.length,
    }))
    on('prompt.change', (_$, e) => ({ text: e.text }))
    on('session.id', () => ({ value: 's' }))
    on('process.run', async (_$, e) => {
      inFlight += 1
      most = Math.max(most, inFlight)
      tried.push(prompt(e.init?.stdin))
      try {
        await clock.sleep(1 + pick(600))
        if (pick(5) === 0) throw new Error('the relay is gone')
        return { value: OK }
      } finally {
        inFlight -= 1
      }
    })
    const box = editor($)
    let text = ''
    for (let step = 0; step < ACTS; step++) {
      const act = pick(10)
      const at = text.length
      if (act < 6) {
        const ch = 'ab '[pick(3)] ?? 'a'
        const r = await box.edit(press(text, ch, at, at, ch))
        expect(r).toEqual({ text: text + ch, cursor: at + 1 })
        text = r.text
      } else if (act < 7 && at > 0) {
        const r = await box.edit(press(text, 'backspace', at - 1, at, ''))
        expect(r.text).toBe(text.slice(0, -1))
        text = r.text
      } else if (act < 8) {
        const left = Math.max(0, at - 1)
        expect((await box.edit(press(text, 'left', left, left, ''))).text).toBe(text)
      } else if (act < 9) {
        text = ['', 'recalled entry', 'ab'][pick(3)] ?? ''
        const recalled = { text, cursor: text.length, origin: { kind: 'history' as const } }
        expect(await box.change(recalled)).toEqual({ text })
      } else {
        text = ''
        await box.change({ text, cursor: 0, origin: { kind: 'clear' } })
      }
      await clock.advance(pick(400))
    }
    await clock.advance(60_000)
    expect(most).toBeLessThanOrEqual(1)
    for (let i = 1; i < tried.length; i++) expect(tried[i]).not.toBe(tried[i - 1])
    expect(tried.at(-1) ?? '').toBe(text)
  })
}
