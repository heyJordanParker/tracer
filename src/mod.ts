import { defineMod, InstructionsLoaded, messageOf, PostToolUse, PostToolUseFailure, PreCompact, PreToolUse, process, SessionStart, SubagentStart, type Hook, type HookAnswer, type Mod } from '../node_modules/@cmodjs/core/mod.js'
import { option } from '../node_modules/@cmodjs/core/options.js'

const options = {
  tracerOnly: option.toggle({
    title: 'Claude reads code only through tracer',
    description: "Claude can't read or search this project with grep, cat, find, or git blame. It uses tracer instead, so every read comes with the file's callers, history, and docs.",
    default: false,
  }),
}

export type TracerMod = Mod<Record<never, never>, typeof options>

const EVENTS = [SessionStart, InstructionsLoaded, PreCompact, SubagentStart, PreToolUse, PostToolUse, PostToolUseFailure] as const satisfies readonly Hook[]

const TIMEOUT_MS = 10_000

const DEADLINE_MS = TIMEOUT_MS - 1_000

export const tracer = defineMod({
  name: 'tracer',
  options,
  setup(mod) {
    for (const event of EVENTS) mod.on(event, (input) => hook(mod, event, input))
  },
})

async function hook(mod: TracerMod, event: Hook, input: { readonly cwd: string }): Promise<HookAnswer | undefined> {
  const argv = ['trace', 'hook', event, '--harness', 'claude', '--deadline-ms', String(DEADLINE_MS), ...(mod.options.tracerOnly ? ['--tracer-only'] : [])]
  const traced = await process.run(argv, { cwd: input.cwd, stdin: JSON.stringify(input), timeoutMs: TIMEOUT_MS }).catch((error: unknown) => ({ exitCode: 1, stdout: '', stderr: messageOf(error) }))
  if (traced.exitCode === 0) return traced.stdout.trim() === '' ? undefined : (JSON.parse(traced.stdout) as HookAnswer)
  if (event === PreToolUse) return undefined
  throw new Error(`trace hook ${event} exited ${traced.exitCode}: ${traced.stderr.trim().split('\n').at(-1) ?? ''}`)
}
