import { expect, test } from 'bun:test'
import { Prompt, ToolCall } from '../node_modules/@cmodjs/core/mod.js'
import { testMod } from '../node_modules/@cmodjs/core/testing.js'
import { tracer } from '../src/mod.js'
import { fakeTrace } from './fake-trace.js'

const ROOT = '/work/app'

test('a tool result runs trace hook once with the result on stdin and answers its context as a note', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const printed = { hookSpecificOutput: { hookEventName: 'PostToolUse', additionalContext: '# src/a.ts  {lines: 3}' } }
  const ran = fakeTrace(tested, () => ({ stdout: `${JSON.stringify(printed)}\n` }))
  const output = { type: 'text', file: { filePath: `${ROOT}/src/a.ts`, content: 'a', numLines: 1, startLine: 1, totalLines: 1 } }
  await tested.add({ type: Prompt, text: 'Read a.ts' })
  await tested.add({ type: ToolCall, name: 'Read', input: { file_path: `${ROOT}/src/a.ts` }, callId: 'toolu_1' })

  const answer = await tested.raise.beforeToolResult({ callId: 'toolu_1', output })

  expect(ran.map(({ argv }) => argv.slice(0, 5))).toEqual([['trace', 'hook', 'PostToolUse', '--harness', 'claude']])
  expect(ran[0]?.cwd).toBe(ROOT)
  expect(JSON.parse(ran[0]?.stdin ?? '{}')).toEqual({ session_id: 'test-session', cwd: ROOT, tool_name: 'Read', tool_input: { file_path: `${ROOT}/src/a.ts` }, tool_response: output })
  expect(answer).toEqual({ note: '# src/a.ts  {lines: 3}' })
})

test('a failed tool call reaches trace hook as PostToolUseFailure with its error', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)
  await tested.add({ type: Prompt, text: 'Read big.ts' })
  await tested.add({ type: ToolCall, name: 'Read', input: { file_path: `${ROOT}/big.ts` }, callId: 'toolu_1' })

  await tested.raise.beforeToolResult({ callId: 'toolu_1', output: 'File content exceeds the maximum size', isError: true })

  expect(ran[0]?.argv[2]).toBe('PostToolUseFailure')
  expect(JSON.parse(ran[0]?.stdin ?? '{}')).toMatchObject({ tool_name: 'Read', error: 'File content exceeds the maximum size' })
})

test("trace hook's deadline leaves a second of the listener's time to answer", async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.raise.beforeSessionStart({ source: 'startup' })

  const deadline = Number(ran[0]?.argv[ran[0].argv.indexOf('--deadline-ms') + 1])
  expect(ran[0]?.timeoutMs).toBe(deadline + 1_000)
  expect(deadline).toBeGreaterThan(8_000)
  expect(deadline).toBeLessThanOrEqual(9_000)
})

test("each moment reaches trace hook as its event, naming the agent it happened to", async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)
  const agent = await tested.startTask({ title: 'Explore the cart', agentType: 'Explore' })

  await tested.raise.beforeSessionStart({ source: 'startup' })
  await tested.raise.beforeContextEnd({ task: agent })
  await tested.raise.beforeSessionStart({ source: 'compact', task: agent })
  await tested.raise.beforePrompt({ text: 'Explore the cart' }, { task: agent })
  await tested.raise.beforePrompt({ text: 'Thanks' })
  await tested.raise.beforeInstructionFile({ path: `${ROOT}/src/cart/CLAUDE.md`, text: 'Totals round half up.' }, { task: agent })
  await tested.raise.beforeToolCall({ name: 'Bash', input: { command: 'trace read a.ts' } }, { task: agent })

  expect(ran.map(({ argv, stdin }) => [argv[2], JSON.parse(stdin ?? '{}')])).toEqual([
    ['SessionStart', { session_id: 'test-session', cwd: ROOT, source: 'startup' }],
    ['PreCompact', { session_id: 'test-session', agent_id: agent, cwd: ROOT }],
    ['SessionStart', { session_id: 'test-session', agent_id: agent, cwd: ROOT, source: 'compact' }],
    ['SubagentStart', { session_id: 'test-session', agent_id: agent, cwd: ROOT }],
    ['InstructionsLoaded', { session_id: 'test-session', agent_id: agent, cwd: ROOT, file_path: `${ROOT}/src/cart/CLAUDE.md` }],
    ['PreToolUse', { session_id: 'test-session', agent_id: agent, cwd: ROOT, tool_name: 'Bash', tool_input: { command: 'trace read a.ts' } }],
  ])
})

test('a session start answers what trace hook prints as its note', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  fakeTrace(tested, () => ({ stdout: JSON.stringify({ hookSpecificOutput: { hookEventName: 'SessionStart', additionalContext: '## Environment' } }) }))

  expect(await tested.raise.beforeSessionStart({ source: 'startup' })).toEqual({ note: '## Environment' })
})

test('the tracer-only option turns on the refusal', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, options: { tracerOnly: true } })
  const refusal = { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: 'tracer: Claude reads files only through tracer.\nRun this instead: trace read a.ts' } }
  const ran = fakeTrace(tested, () => ({ stdout: JSON.stringify(refusal) }))

  const answer = await tested.raise.beforeToolCall({ name: 'Bash', input: { command: 'cat a.ts' } })

  expect(ran[0]?.argv.at(-1)).toBe('--tracer-only')
  expect(answer).toEqual({ refuse: 'tracer: Claude reads files only through tracer.\nRun this instead: trace read a.ts' })
})

test('a doc trace already sent whole never loads again', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const held = { hookSpecificOutput: { hookEventName: 'InstructionsLoaded', permissionDecision: 'deny', permissionDecisionReason: 'tracer already sent this doc whole.' } }
  fakeTrace(tested, () => ({ stdout: JSON.stringify(held) }))

  expect(await tested.raise.beforeInstructionFile({ path: `${ROOT}/src/cart/CLAUDE.md`, text: 'Totals round half up.' })).toEqual({ refuse: 'tracer already sent this doc whole.' })
})

test("a subagent's trace call runs as trace rewrote it", async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const agent = await tested.startTask({ title: 'Explore the cart' })
  const rewritten = { command: 'TRACE_AGENT=agent-7 trace read a.ts' }
  fakeTrace(tested, () => ({ stdout: JSON.stringify({ hookSpecificOutput: { hookEventName: 'PreToolUse', updatedInput: rewritten } }) }))

  expect(await tested.raise.beforeToolCall({ name: 'Bash', input: { command: 'trace read a.ts' } }, { task: agent })).toEqual({ input: rewritten })
})

test('a tool call goes on when trace fails or is missing', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  tested.fakes.process.run = async () => {
    throw new Error('trace: command not found')
  }

  expect(await tested.raise.beforeToolCall({ name: 'Bash', input: { command: 'ls' } })).toBeUndefined()
  expect(tested.shown.toasts.filter(({ type }) => type === 'error')).toEqual([])
})

test('a failing trace hook on any other moment shows why', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  fakeTrace(tested, () => ({ exitCode: 2, stderr: 'trace: the index is locked\n' }))

  await tested.raise.beforeSessionStart({ source: 'startup' })

  expect(tested.shown.toasts.filter(({ type }) => type === 'error')).toEqual([{ type: 'error', message: 'The beforeSessionStart listener failed', description: 'trace hook SessionStart exited 2: trace: the index is locked' }])
})
