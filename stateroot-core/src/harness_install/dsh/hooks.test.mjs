import assert from 'node:assert/strict'
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'
import { apply } from './hooks.mjs'

// Exercise the installed plugin's real subprocess/stdin/stdout boundary.
// Node runs the fixture named `hook`, so this also works without shell scripts
// or executable-mode support on Windows. No provider or live profile is used.
async function fixture(t, mode = {}) {
  const cwd = await mkdtemp(join(tmpdir(), 'stateroot-dsh-hooks-'))
  await writeFile(join(cwd, 'mode.json'), JSON.stringify(mode))
  await writeFile(join(cwd, 'hook'), `
const fs = require('node:fs')
const mode = JSON.parse(fs.readFileSync('mode.json', 'utf8'))
const event = process.argv[2]
let input = ''
process.stdin.on('data', chunk => input += chunk)
process.stdin.on('end', () => {
  const payload = JSON.parse(input)
  const finish = () => {
    let calls = []
    try { calls = fs.readFileSync('calls.jsonl', 'utf8').trim().split('\\n').filter(Boolean).map(JSON.parse) } catch {}
    fs.appendFileSync('calls.jsonl', JSON.stringify({ event, payload }) + '\\n')
    if (event === 'session_start') {
      if (mode.failStart === 'always' || (mode.failStart === 'once' && !calls.some(c => c.event === event))) process.exitCode = 1
      else process.stdout.write('SESSION-DIGEST')
    }
    if (event === 'user_prompt_submit' && mode.fallback) process.stdout.write('FALLBACK-DIGEST')
    if (event === 'post_compaction') process.stdout.write('COMPACTION-DIGEST')
    if (event === 'tool_failure' && mode.failCapture) process.exitCode = 1
    if (event === 'tool_failure' && mode.softCapture) process.stderr.write('capture could not be persisted')
  }
  setTimeout(finish, event === 'post_compaction' ? 80 : event === 'post_tool_use' ? 50 : 0)
})
`)
  const handlers = new Map()
  const warnings = []
  apply({ on: (event, handler) => handlers.set(event, handler), logger: { warn: text => warnings.push(text) } }, { command: process.execPath })
  const session = { id: 'fixture-session', header: { cwd } }
  const pre = (step = 1, signal = new AbortController().signal) => {
    const messages = [{ role: 'user', content: [{ type: 'text', text: 'Keep working' }] }]
    return handlers.get('agent/pre-step')({ agent: { session }, step, messages, signal }, async () => ({ kind: 'enter', messages }))
  }
  const calls = async () => {
    try { return (await readFile(join(cwd, 'calls.jsonl'), 'utf8')).trim().split('\n').filter(Boolean).map(JSON.parse) }
    catch { return [] }
  }
  const stop = (current = session) => handlers.get('agent/turn-stopping')({ agent: { session: current } })
  t.after(async () => {
    await handlers.get('session/disposed')(session)
    await rm(cwd, { recursive: true, force: true })
  })
  return { handlers, session, pre, calls, stop, warnings }
}

const digests = decision => decision.messages.filter(message => message.source?.kind === 'stateroot')

test('failed initial delivery retries, and prompt fallback is actually injected', async t => {
  const retry = await fixture(t, { failStart: 'once' })
  assert.equal(digests(await retry.pre()).length, 0)
  assert.equal(digests(await retry.pre())[0].content[0].text, 'SESSION-DIGEST')
  assert.equal(digests(await retry.pre()).length, 0, 'successful delivery is not repeated')
  assert.equal((await retry.calls()).filter(call => call.event === 'session_start').length, 2)
  assert.ok(retry.warnings.some(text => text.includes('session_start')))

  const fallback = await fixture(t, { failStart: 'always', fallback: true })
  assert.equal(digests(await fallback.pre())[0].content[0].text, 'FALLBACK-DIGEST')
  const prompt = (await fallback.calls()).find(call => call.event === 'user_prompt_submit')
  assert.equal(prompt.payload.prompt, 'Keep working')
})

test('compaction reanchors on the next step of the same turn, even while the hook is pending', async t => {
  const f = await fixture(t)
  assert.equal(digests(await f.pre()).length, 1)
  f.handlers.get('session/event')(f.session, { type: 'compaction/end' })
  const next = await f.pre(2)
  assert.equal(digests(next)[0].content[0].text, 'COMPACTION-DIGEST')
  assert.equal(digests(await f.pre(3)).length, 0)
})

test('full tool evidence and native call identity arrive before the closing boundary', async t => {
  const f = await fixture(t)
  const result = { isError: false, content: [{ type: 'text', text: 'x'.repeat(12000) + 'TAIL-CANARY' }] }
  f.handlers.get('tools/result')({ agent: { session: f.session }, callId: 'native-call-1', name: 'read_file', arguments: { path: 'test.txt' } }, result)
  await f.stop()
  const calls = await f.calls()
  assert.deepEqual(calls.map(call => call.event), ['post_tool_use', 'stop'])
  assert.deepEqual(calls[0].payload.tool_response, result)
  assert.equal(calls[0].payload.tool_call_id, 'native-call-1')
})

test('bursts queue beyond the process limit instead of losing events', async t => {
  const f = await fixture(t)
  const sessions = Array.from({ length: 9 }, (_, index) => ({ id: `burst-${index}`, header: f.session.header }))
  for (const session of sessions) {
    f.handlers.get('tools/result')({ agent: { session }, callId: session.id, name: 'read_file', arguments: {} }, { isError: false, content: [] })
  }
  await Promise.all(sessions.map(session => f.stop(session)))
  const captures = (await f.calls()).filter(call => call.event === 'post_tool_use')
  assert.equal(captures.length, 9)
  assert.equal(new Set(captures.map(call => call.payload.tool_call_id)).size, 9)
  assert.deepEqual(f.warnings, [])
})

test('tool failures use their native boundary and failed capture is visible', async t => {
  const f = await fixture(t, { failCapture: true })
  f.handlers.get('tools/result')({ agent: { session: f.session }, name: 'failed_tool', arguments: {} }, { isError: true, content: [] })
  await f.stop()
  assert.deepEqual((await f.calls()).map(call => call.event), ['tool_failure', 'stop'])
  assert.ok(f.warnings.some(text => text.includes('tool_failure')))
  const soft = await fixture(t, { softCapture: true })
  soft.handlers.get('tools/result')({ agent: { session: soft.session }, name: 'failed_tool', arguments: {} }, { isError: true, content: [] })
  await soft.stop()
  assert.ok(soft.warnings.some(text => text.includes('capture could not be persisted')))
})
