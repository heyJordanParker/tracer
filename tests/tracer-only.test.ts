import { expect, test } from 'bun:test'
import { testMod } from '../node_modules/@cmodjs/core/testing.js'
import { tracer } from '../src/mod.js'
import { fakeTrace } from './fake-trace.js'

const ROOT = '/work/app'
const FILES = {
  [`${ROOT}/src/cart.ts`]: 'export class Cart {}\n',
  [`${ROOT}/src/order.ts`]: 'export class Order {}\n',
  [`${ROOT}/notes.log`]: 'x\n',
  '/tmp/outside.txt': 'x\n',
}

async function run(command: string, options: { tracerOnly?: boolean; cwd?: string } = {}) {
  const tested = testMod(tracer, { projectRoot: ROOT, cwd: options.cwd ?? ROOT, files: FILES, options: { tracerOnly: options.tracerOnly ?? true } })
  fakeTrace(tested)
  return tested.fire('PreToolUse', { tool_name: 'Bash', tool_input: { command } })
}

function refusal(replacement: string): string {
  return `tracer: Claude reads this project's code only through tracer.\nRun this instead: ${replacement}`
}

test('a raw read of the project is refused with the tracer command that answers it', async () => {
  expect((await run('grep -rn Cart src')).deny).toBe(refusal('trace grep Cart src'))
  expect((await run('rg -i -t ts "class Cart"')).deny).toBe(refusal("trace grep 'class Cart' -i -t ts"))
  expect((await run('cat src/cart.ts src/order.ts')).deny).toBe(refusal('trace read src/cart.ts src/order.ts'))
  expect((await run('head -n 20 src/cart.ts')).deny).toBe(refusal('trace read src/cart.ts'))
  expect((await run('cat src/*.ts')).deny).toBe(refusal("trace read 'src/*.ts'"))
  expect((await run('find src -name "*.ts"')).deny).toBe(refusal("trace find '*.ts' src"))
  expect((await run('ls')).deny).toBe(refusal('trace list .'))
  expect((await run('tree src')).deny).toBe(refusal('trace tree src'))
})

test('a git read of the project is refused with its tracer command', async () => {
  expect((await run('git blame -L 10,20 src/cart.ts')).deny).toBe(refusal('trace blame src/cart.ts --lines 10:20'))
  expect((await run('git grep Cart src')).deny).toBe(refusal('trace grep Cart src'))
  expect((await run('git show HEAD~2:src/cart.ts')).deny).toBe(refusal('trace read src/cart.ts --at HEAD~2'))
  expect((await run('git log -S Cart')).deny).toBe(refusal('trace history --contains Cart'))
  expect((await run('git log --oneline src/cart.ts')).deny).toBe(refusal('trace history src/cart.ts'))
})

test('trace output piped into a filter is refused with the trace command alone', async () => {
  expect((await run('trace read src/cart.ts | head -20')).deny).toBe(refusal('trace read src/cart.ts'))
  expect((await run('trace grep Cart src | grep -v test | wc -l')).deny).toBe(refusal('trace grep Cart src'))
  expect((await run("trace grep Cart --json | jq '.counts'")).deny).toBe(refusal('trace grep Cart --json --filter .counts'))
  expect((await run('trace grep Cart | jq -r .results')).deny).toBe(refusal('trace grep Cart --json --filter .results'))
  expect((await run('timeout 5 trace status | tail -3')).deny).toBe(refusal('trace status'))
})

test('a read after cd is checked in the folder it runs in', async () => {
  expect((await run('cd src && cat cart.ts')).deny).toBe(refusal('trace read cart.ts'))
})

test('reads tracer has no answer for, and reads outside the project, run', async () => {
  for (const command of [
    'git status',
    'git log -p src/cart.ts',
    'git diff',
    'cat /tmp/outside.txt',
    "sed -i 's/Cart/Basket/' src/cart.ts",
    'find . -name "*.tmp" -delete',
    'echo done | grep done',
    'trace read src/cart.ts',
    'npm test',
  ]) {
    expect({ command, deny: (await run(command)).deny }).toEqual({ command, deny: undefined })
  }
  expect((await run('ls', { cwd: '/tmp' })).deny).toBeUndefined()
})

test('every read runs while the setting is off', async () => {
  expect((await run('grep -rn Cart src', { tracerOnly: false })).deny).toBeUndefined()
})
