import { expect, test } from 'bun:test'
import { testMod } from '../node_modules/@cmodjs/core/testing.js'
import { tracer } from '../src/mod.js'
import { tracedCall } from '../src/project-docs.js'
import { fakeTrace, traced } from './fake-trace.js'

const ROOT = '/work/app'
const FILES = { [`${ROOT}/src/cart.ts`]: 'x\n', [`${ROOT}/src/CLAUDE.md`]: '# src\n' }

test('a trace command gets the project docs of the paths it names, and never its pattern', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES })
  const ran = fakeTrace(tested, () => ({ stdout: '## src/CLAUDE.md\n\n# src\n' }))
  const command = 'trace grep Cart src'

  const answer = await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command } })

  expect(traced(ran)).toEqual([['docs', `${ROOT}/src`, '--budget', '10000', '--source', 'tracer_project_docs', '--triggering-tool', 'Bash', '--triggering-command', command]])
  expect(answer.additionalContext).toEqual(['## src/CLAUDE.md\n\n# src'])
})

test('a trace command that names no existing path gets the docs of the folder it runs in', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'trace -C src grep Missing' } })

  expect(traced(ran)[0]?.[1]).toBe(`${ROOT}/src`)
})

test('a trace command after a cd reads its paths from that folder', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'cd src && trace read cart.ts' } })

  expect(traced(ran)[0]?.slice(1, 2)).toEqual([`${ROOT}/src/cart.ts`])
  expect(traced(ran)[0]?.slice(-2)).toEqual(['--skip', `${ROOT}/src/cart.ts`])
})

test('trace read skips the docs it prints itself', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'trace read src/CLAUDE.md' } })

  expect(traced(ran)[0]?.slice(-2)).toEqual(['--skip', `${ROOT}/src/CLAUDE.md`])
})

test('a subagent’s trace commands carry its agent id, beside the docs', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES })
  fakeTrace(tested, () => ({ stdout: '## src/CLAUDE.md\n' }))

  const answer = await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'cd src && trace read cart.ts | head', run_in_background: false }, agent_id: 'agent-9', agent_type: 'explorer' })

  expect(answer.updatedInput).toEqual({ command: 'cd src && trace --agent agent-9 read cart.ts | head', run_in_background: false })
  expect(answer.additionalContext).toEqual(['## src/CLAUDE.md'])
})

test('a trace command that already names its agent is left as written', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES })
  fakeTrace(tested)

  const answer = await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'trace --agent agent-9 status' }, agent_id: 'agent-9', agent_type: 'explorer' })

  expect(answer.updatedInput).toBeUndefined()
})

test('a command that runs no trace is left alone', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES })
  const ran = fakeTrace(tested)

  const answer = await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'git status' } })

  expect(ran).toEqual([])
  expect(answer).toEqual({})
})

test('project docs can be turned off, while subagent ids still apply', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: FILES, options: { projectDocs: false } })
  const ran = fakeTrace(tested)

  const answer = await tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command: 'trace grep Cart src' }, agent_id: 'agent-1', agent_type: 'explorer' })

  expect(ran).toEqual([])
  expect(answer.updatedInput).toEqual({ command: 'trace --agent agent-1 grep Cart src' })
})

test('the leading options before the subcommand are read, -C moving the base folder', () => {
  expect(tracedCall('/usr/local/bin/trace --budget 900 -C ../other structure lib', '/work/app')).toEqual({
    subcommand: 'structure',
    base: '/work/other',
    candidates: ['/work/other/lib'],
  })
  expect(tracedCall('trace status', '/work/app')).toBeUndefined()
})
