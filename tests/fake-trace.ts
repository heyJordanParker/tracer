import type { EventResult } from 'claude-code'
import { defineMod, type Job } from '../node_modules/@cmodjs/core/mod.js'
import { testMod, type TestedMod, type TestOptions } from '../node_modules/@cmodjs/core/testing.js'
import { tracer } from '../src/mod.js'
import type { TracerState } from '../src/state.js'

export type Ran = {
  readonly argv: readonly string[]
  readonly cwd: string | undefined
  readonly env: Record<string, string> | undefined
}

type Answer = { readonly exitCode?: number; readonly stdout?: string; readonly stderr?: string }

type Fired = EventResult<'classic.PreToolUse'>

let reachSubagentCall: () => void = () => undefined
let subagentCallFinished: Promise<void> = Promise.resolve()

const holdSubagentCalls: Job<void, TracerState> = (job) => {
  job.on('tool.call', async (e, next) => {
    if (e.agentId === undefined) return next(e)
    reachSubagentCall()
    await subagentCallFinished
    return next(e)
  })
}

const testedTracer = defineMod({
  ...tracer,
  setup(mod) {
    tracer.setup?.(mod)
    mod.use(holdSubagentCalls)
  },
})

export function testTracer(options: TestOptions<TracerState> = {}): TestedMod<TracerState> {
  return testMod(testedTracer, options)
}

export async function fireInSubagent(tested: TestedMod<TracerState>, agentId: string, tool: string, input: Record<string, unknown>): Promise<Fired> {
  tested.fakes.agent.list = async () => [{ id: agentId, type: 'explorer', description: 'Map the code', status: 'running' }]
  const toolUseId = `toolu_${agentId}`
  let finish: () => void = () => undefined
  subagentCallFinished = new Promise((resolve) => (finish = resolve))
  const reached = new Promise<void>((resolve) => (reachSubagentCall = resolve))
  const call = tested.fire('tool.call', { tool, tool_use_id: toolUseId, agentId, ...input } as never)
  await reached
  const answer = await tested.fire('PreToolUse', { tool_name: tool, tool_input: input, tool_use_id: toolUseId })
  finish()
  await call
  return answer
}

export function fakeTrace(tested: TestedMod<TracerState>, answer: (argv: readonly string[]) => Answer = () => ({})): Ran[] {
  const ran: Ran[] = []
  tested.fakes.process.run = async (argv, init) => {
    ran.push({ argv, cwd: init?.cwd, env: init?.env })
    const { exitCode = 0, stdout = '', stderr = '' } = answer(argv)
    return { exitCode, stdout, stderr, isStdoutTruncated: false, isStderrTruncated: false }
  }
  return ran
}

export function traced(ran: readonly Ran[]): string[][] {
  return ran.filter(({ argv }) => argv[0] === 'trace').map(({ argv }) => argv.slice(1))
}

export function tracedRuns(ran: readonly Ran[]): Ran[] {
  return ran.filter(({ argv }) => argv[0] === 'trace')
}
