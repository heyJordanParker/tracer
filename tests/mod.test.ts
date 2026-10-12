import { expect, test } from 'bun:test'
import { testMod } from '../node_modules/@cmodjs/core/testing.js'
import { tracer } from '../src/mod.js'
import { fakeTrace } from './fake-trace.js'

const ROOT = '/work/app'

test('every event runs trace hook once with its input on stdin and answers what it prints', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const printed = { hookSpecificOutput: { hookEventName: 'PostToolUse', additionalContext: '# src/a.ts  {lines: 3}' } }
  const ran = fakeTrace(tested, () => ({ stdout: `${JSON.stringify(printed)}\n` }))

  const answer = await tested.fire('PostToolUse', { tool_name: 'Read', tool_input: { file_path: `${ROOT}/src/a.ts` }, tool_response: {} })

  expect(ran.map(({ argv }) => argv)).toEqual([['trace', 'hook', 'PostToolUse', '--harness', 'claude', '--deadline-ms', '9000']])
  expect(ran[0]?.cwd).toBe(ROOT)
  expect(ran[0]?.timeoutMs).toBe(10_000)
  expect(JSON.parse(ran[0]?.stdin ?? '{}')).toMatchObject({ session_id: 'test-session', tool_name: 'Read', tool_input: { file_path: `${ROOT}/src/a.ts` } })
  expect(answer.additionalContext).toEqual(['# src/a.ts  {lines: 3}'])
})

test('each event tracer handles reaches trace hook, and no other', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('SessionStart', { source: 'startup' })
  await tested.fire('InstructionsLoaded', { file_path: `${ROOT}/CLAUDE.md`, memory_type: 'Project', load_reason: 'session_start' })
  await tested.fire('PreCompact', { trigger: 'auto', custom_instructions: null })
  await tested.fire('SubagentStart', { agent_id: 'agent-7', agent_type: 'explorer' })
  await tested.fire('SubagentStop', { stop_hook_active: false, agent_id: 'agent-7', agent_type: 'explorer', agent_transcript_path: '/tmp/agent-7.jsonl', last_assistant_message: 'done' })

  expect(ran.map(({ argv }) => argv[2])).toEqual(['SessionStart', 'InstructionsLoaded', 'PreCompact', 'SubagentStart'])
})

test('the tracer-only option turns on the refusal', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, options: { tracerOnly: true } })
  const refusal = { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: 'tracer: Claude reads this project\'s code only through tracer.\nRun this instead: trace read a.ts' } }
  const ran = fakeTrace(tested, () => ({ stdout: JSON.stringify(refusal) }))

  const answer = await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'cat a.ts' } })

  expect(ran[0]?.argv.at(-1)).toBe('--tracer-only')
  expect(answer.deny).toBe("tracer: Claude reads this project's code only through tracer.\nRun this instead: trace read a.ts")
})

test('a tool call goes on when trace fails or is missing', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  tested.fakes.process.run = async () => {
    throw new Error('trace: command not found')
  }

  const answer = await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'ls' } })

  expect(answer.deny).toBeUndefined()
  expect(answer.additionalContext).toBeUndefined()
})
