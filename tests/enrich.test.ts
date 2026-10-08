import { expect, test } from 'bun:test'
import { testMod } from '../node_modules/@cmodjs/core/testing.js'
import { tracer } from '../src/mod.js'
import { fakeTrace, traced } from './fake-trace.js'

const ROOT = '/work/app'

test('a Read arrives with the file facts and declarations of the lines it reads', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: { [`${ROOT}/src/cart.ts`]: 'export class Cart {}\n' } })
  const ran = fakeTrace(tested, () => ({ stdout: '# src/cart.ts  {imported_by: 4}\nL1 export class Cart { … }\n' }))

  const answer = await tested.fire('PreToolUse', { tool_name: 'Read', tool_input: { file_path: `${ROOT}/src/cart.ts`, offset: 10, limit: 40 } })

  expect(traced(ran)).toEqual([['context', `${ROOT}/src/cart.ts`, '--budget', '10000', '--offset', '10', '--limit', '40']])
  expect(answer.additionalContext).toEqual(['# src/cart.ts  {imported_by: 4}\nL1 export class Cart { … }'])
})

test('an Edit gets the declarations around the lines it replaces and records no read', async () => {
  const source = 'line one\nline two\nfunction total() {\n  return 1\n}\n'
  const tested = testMod(tracer, { projectRoot: ROOT, files: { [`${ROOT}/src/total.ts`]: source } })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Edit', tool_input: { file_path: `${ROOT}/src/total.ts`, old_string: 'function total() {\n  return 1', new_string: 'x' } })

  expect(traced(ran)).toEqual([['context', `${ROOT}/src/total.ts`, '--budget', '10000', '--no-record', '--offset', '3', '--limit', '2']])
})

test('a Grep runs trace grep with the same pattern, flags, and path', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Grep', tool_input: { pattern: '-x', path: `${ROOT}/src`, '-i': true, glob: '*.ts', type: 'ts', multiline: true } })

  expect(traced(ran)).toEqual([['grep', '--budget', '10000', '-i', '-g', '*.ts', '-t', 'ts', '-U', '--', '-x', 'src']])
})

test('a Grep of the working folder searches it as .', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Grep', tool_input: { pattern: 'Cart' } })

  expect(traced(ran)).toEqual([['grep', '--budget', '10000', '--', 'Cart', '.']])
})

test('a Glob lists each matched file with its facts', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Glob', tool_input: { pattern: '**/*.ts' } })

  expect(traced(ran)).toEqual([['find', '**/*.ts', ROOT, '--budget', '10000']])
})

test('a search with no matches adds nothing', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  fakeTrace(tested, () => ({ stdout: '(no matches)\n' }))

  const answer = await tested.fire('PreToolUse', { tool_name: 'Grep', tool_input: { pattern: 'Nothing' } })

  expect(answer.additionalContext).toBeUndefined()
})

test('a failed trace on a file that exists says so in its place', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT, files: { [`${ROOT}/a.ts`]: 'a\n' } })
  fakeTrace(tested, () => ({ exitCode: 1, stderr: 'Error: cache locked\n' }))

  const answer = await tested.fire('PreToolUse', { tool_name: 'Read', tool_input: { file_path: `${ROOT}/a.ts` } })

  expect(answer.additionalContext).toEqual([`${ROOT}/a.ts\n[trace context unavailable: Error: cache locked]`])
})

test('a failed trace on a file being written adds nothing', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  fakeTrace(tested, () => ({ exitCode: 2, stderr: 'Error: path not found' }))

  const answer = await tested.fire('PreToolUse', { tool_name: 'Write', tool_input: { file_path: `${ROOT}/new.ts`, content: 'x' } })

  expect(answer.additionalContext).toBeUndefined()
})

test('a subagent call records into the subagent’s own log', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'Glob', tool_input: { pattern: '*.md' }, agent_id: 'agent-3', agent_type: 'explorer' })

  expect(ran[0]?.env).toEqual({ AGENT_SESSION_ID: 'test-session', TRACER_AGENT_ID: 'agent-3' })
})

test('enrichment can be turned off, and a budget set', async () => {
  const off = testMod(tracer, { projectRoot: ROOT, options: { enrich: false } })
  const offRan = fakeTrace(off)
  const small = testMod(tracer, { projectRoot: ROOT, options: { budget: 2000 } })
  const smallRan = fakeTrace(small)

  await off.fire('PreToolUse', { tool_name: 'Glob', tool_input: { pattern: '*.md' } })
  await small.fire('PreToolUse', { tool_name: 'Glob', tool_input: { pattern: '*.md' } })

  expect(offRan).toEqual([])
  expect(traced(smallRan)).toEqual([['find', '*.md', ROOT, '--budget', '2000']])
})

test('a tool tracer does not enrich runs nothing', async () => {
  const tested = testMod(tracer, { projectRoot: ROOT })
  const ran = fakeTrace(tested)

  await tested.fire('PreToolUse', { tool_name: 'WebFetch', tool_input: { url: 'https://example.com', prompt: 'x' } })

  expect(ran).toEqual([])
})
