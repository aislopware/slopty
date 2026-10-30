// The mod in Claude Code's own test kit: the engine's real dispatch, scanner and result checks,
// a clock that moves only when a test moves it, and the test's own hooks as the host beneath the
// mod (the editor's splice, the session's id, the relay with the latency and failures each test
// sets). Every run replays exactly.
import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type {
  On,
  ProcessRunResult,
  PromptChangeInput,
  PromptChangeResult,
  PromptEditInput,
  PromptEditResult,
} from 'claude-code'

// The kit raises the editor's own events at run time; its typings declare only the calls on `$`.
// Should a build drop that door, every test fails here, loudly.
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

const RELAY = ['/slopty', 'hook']
const OK: ProcessRunResult = {
  exitCode: 0,
  stdout: '',
  stderr: '',
  isStdoutTruncated: false,
  isStderrTruncated: false,
}
type Payload = { hook_event_name: string; session_id: string; prompt: string; tmpdir?: string }
function parsed(stdin: string | undefined): Payload {
  const p: unknown = JSON.parse(stdin ?? 'null')
  if (
    typeof p === 'object' &&
    p !== null &&
    'hook_event_name' in p &&
    typeof p.hook_event_name === 'string' &&
    'session_id' in p &&
    typeof p.session_id === 'string' &&
    'prompt' in p &&
    typeof p.prompt === 'string'
  ) {
    const payload: Payload = {
      hook_event_name: p.hook_event_name,
      session_id: p.session_id,
      prompt: p.prompt,
    }
    if ('tmpdir' in p) {
      if (typeof p.tmpdir !== 'string') throw new Error(`a tmpdir that is no path: ${stdin ?? ''}`)
      payload.tmpdir = p.tmpdir
    }
    return payload
  }
  throw new Error(`no PromptDraft payload: ${stdin ?? ''}`)
}
type Sent = { at: number; argv: readonly string[]; timeoutMs: number | undefined; payload: Payload }

// The engine beneath the mod: the editor applies each splice, the session has an id, the
// environment holds what the test sets, and the relay is a fake whose latency and failures the
// test sets, on the kit's mocked clock.
function host(
  on: On,
  relay: { latencyMs?: number; fail?: () => boolean } = {},
  env: Record<string, string> = {},
) {
  const clock = mock.clock(on)
  mock.env(on, env)
  const sent: Sent[] = []
  let inFlight = 0
  let most = 0
  on('prompt.edit', (_$, e) => ({
    text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end),
    cursor: e.start + e.inputText.length,
  }))
  on('prompt.change', (_$, e) => ({ text: e.text }))
  on('session.id', () => ({ value: 'session-1' }))
  on('process.run', async (_$, e) => {
    inFlight += 1
    most = Math.max(most, inFlight)
    try {
      await clock.sleep(relay.latencyMs ?? 5)
      const payload = parsed(e.init?.stdin)
      sent.push({ at: clock.now(), argv: e.argv, timeoutMs: e.init?.timeoutMs, payload })
      if (relay.fail?.() === true) throw new Error('the relay is gone')
      return { value: OK }
    } finally {
      inFlight -= 1
    }
  })
  return { clock, sent, texts: () => sent.map((s) => s.payload.prompt), most: () => most }
}

function typed(text: string, key: string): PromptEditInput {
  const at = text.length
  return {
    origin: { kind: 'composer' },
    key: { key },
    text,
    cursor: at,
    start: at,
    end: at,
    inputText: key,
  }
}

function recalled(text: string): PromptChangeInput {
  return { text, cursor: text.length, origin: { kind: 'history' } }
}

test('the box is handed back as the editor left it', async ($, on) => {
  host(on)
  const box = editor($)
  expect(await box.edit(typed('ab', 'c'))).toEqual({ text: 'abc', cursor: 3 })
  const left = { ...typed('abc', 'left'), start: 2, end: 2, inputText: '' }
  expect(await box.edit(left)).toEqual({ text: 'abc', cursor: 2 })
  expect(await box.change(recalled('x'))).toEqual({ text: 'x' })
})

test('a lone change goes once after the pause, as a PromptDraft of this session', async ($, on) => {
  const h = host(on)
  await editor($).edit(typed('', 'a'))
  await h.clock.advance(149)
  expect(h.sent).toEqual([])
  await h.clock.advance(5000)
  const payload = { hook_event_name: 'PromptDraft', session_id: 'session-1', prompt: 'a' }
  expect(h.sent).toEqual([{ at: 155, argv: RELAY, timeoutMs: 5000, payload }])
})

test('a moved temporary root goes with the draft, for the pictures pasted in it', async ($, on) => {
  const h = host(on, {}, { CLAUDE_CODE_TMPDIR: '/Volumes/Fast/tmp', TMPDIR: '/var/folders/x' })
  await editor($).edit(typed('', 'a'))
  await h.clock.advance(5000)
  expect(h.sent.map((s) => s.payload)).toEqual([
    {
      hook_event_name: 'PromptDraft',
      session_id: 'session-1',
      prompt: 'a',
      tmpdir: '/Volumes/Fast/tmp',
    },
  ])
})

test('a caret move, a clear of an empty box and an unchanged text send nothing', async ($, on) => {
  const h = host(on)
  const box = editor($)
  await box.edit({ ...typed('abc', 'left'), start: 2, end: 2, inputText: '' })
  await box.change({ text: '', cursor: 0, origin: { kind: 'clear' } })
  await h.clock.advance(1000)
  expect(h.sent).toEqual([])
  await box.change(recalled('abc'))
  await h.clock.advance(1000)
  await box.change({ ...recalled('abc'), origin: { kind: 'engine' } })
  await h.clock.advance(1000)
  expect(h.texts()).toEqual(['abc'])
})

test('a caret move does not hold back a draft waiting to go', async ($, on) => {
  const h = host(on)
  await editor($).edit(typed('', 'a'))
  await h.clock.advance(100)
  await editor($).edit({ ...typed('a', 'left'), start: 0, end: 0, inputText: '' })
  await h.clock.advance(60)
  expect(h.texts()).toEqual(['a'])
})

test('steady typing still goes within a second, and the last word lands', async ($, on) => {
  const h = host(on)
  let text = ''
  for (let i = 0; i < 30; i++) {
    text = (await editor($).edit(typed(text, 'k'))).text
    await h.clock.advance(100)
  }
  expect(h.sent[0]?.payload.prompt).toBe('kkkkkkkk')
  expect(h.sent.length).toBeLessThanOrEqual(4)
  await h.clock.advance(1000)
  expect(h.texts().at(-1)).toBe(text)
})

test('one relay at a time: what changed while it ran goes next, the newest only', async ($, on) => {
  const h = host(on, { latencyMs: 1000 })
  const box = editor($)
  await box.change(recalled('one'))
  await h.clock.advance(300)
  await box.change(recalled('two'))
  await h.clock.advance(300)
  await box.change(recalled('three'))
  await h.clock.advance(5000)
  expect(h.most()).toBe(1)
  expect(h.texts()).toEqual(['one', 'three'])
})

test('a relay that fails is let go: the box is untouched and the next change goes', async ($, on) => {
  let down = true
  const h = host(on, { fail: () => down })
  expect(await editor($).edit(typed('', 'a'))).toEqual({ text: 'a', cursor: 1 })
  await h.clock.advance(500)
  down = false
  expect(await editor($).edit(typed('a', 'b'))).toEqual({ text: 'ab', cursor: 2 })
  await h.clock.advance(500)
  expect(h.texts()).toEqual(['a', 'ab'])
})

test('a key is answered while a relay hangs', async ($, on) => {
  const h = host(on, { latencyMs: 60_000 })
  await editor($).edit(typed('', 'a'))
  await h.clock.advance(200)
  expect(await editor($).edit(typed('a', 'b'))).toEqual({ text: 'ab', cursor: 2 })
  expect(h.most()).toBe(1)
})
