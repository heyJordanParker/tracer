import { expect, test } from 'bun:test'
import { testMod } from '../node_modules/@cmodjs/core/testing.js'
import { tracer } from '../src/mod.js'
import { fakeTrace, traced, tracedRuns } from './fake-trace.js'

const ROOT = '/work/app'

test('a new session records the docs Claude Code loaded and gets the repository primer', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested, (argv) => (argv[1] === 'context' ? { stdout: '## Environment\n  repo root: /work/app\n' } : {}))

  const answer = await tested.fire('SessionStart', { source: 'startup' })

  expect(traced(ran)).toEqual([
    ['docs', 'prime', '--reason', 'session_start'],
    ['docs', ROOT, '--json', '--source', 'tracer_session_start'],
    ['context'],
  ])
  expect(answer.additionalContext).toEqual(['## Environment\n  repo root: /work/app'])
  expect(tracedRuns(ran).every(({ cwd, env }) => cwd === ROOT && env?.['AGENT_SESSION_ID'] === 'test-session')).toBe(true)
})

test('a cleared session forgets what tracer recorded before it records again', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('SessionStart', { source: 'clear' })

  expect(traced(ran).slice(0, 2)).toEqual([
    ['docs', 'reset', '--source', 'tracer_clear'],
    ['docs', 'prime', '--reason', 'session_start'],
  ])
})

test('a compacted session records the docs Claude Code puts back', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('SessionStart', { source: 'compact' })

  expect(traced(ran)[0]).toEqual(['docs', 'prime', '--reason', 'post_compact'])
})

test('a session outside a git repository gets no primer', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested, (argv) => (argv[0] === 'git' ? { exitCode: 128 } : {}))

  const answer = await tested.fire('SessionStart', { source: 'startup' })

  expect(traced(ran).some((args) => args[0] === 'context')).toBe(false)
  expect(answer.additionalContext).toBeUndefined()
})

test('a session goes on without context when trace is not installed', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  tested.fakes.process.run = async () => {
    throw new Error('trace: command not found')
  }

  const answer = await tested.fire('SessionStart', { source: 'startup' })

  expect(answer.additionalContext).toBeUndefined()
})

test('a doc Claude Code loads mid-session is recorded, so tracer never sends it again', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('InstructionsLoaded', { file_path: `${ROOT}/src/CLAUDE.md`, memory_type: 'Project', load_reason: 'nested_traversal' })

  expect(traced(ran)).toEqual([['docs', 'prime', `${ROOT}/src/CLAUDE.md`]])
})

test('a compaction forgets the docs it drops from context', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('PreCompact', { trigger: 'auto', custom_instructions: null })

  expect(traced(ran)).toEqual([['docs', 'reset', '--source', 'tracer_compact']])
})

test('a stopped subagent has its log archived under its own agent id', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('SubagentStop', { stop_hook_active: false, agent_id: 'agent-7', agent_type: 'explorer', agent_transcript_path: '/tmp/agent-7.jsonl', last_assistant_message: 'done' })

  expect(traced(ran)).toEqual([['docs', 'archive']])
  expect(ran[0]?.env).toEqual({ AGENT_SESSION_ID: 'test-session', TRACER_AGENT_ID: 'agent-7' })
})
