import { defineMod, messageOf, process, sessions, type ListenerTime, type Mod } from '../node_modules/@cmodjs/core/mod.js'
import { option } from '../node_modules/@cmodjs/core/options.js'

const options = {
  tracerOnly: option.toggle({
    title: 'Claude reads code only through tracer',
    description: "Claude can't read or search this project with grep, cat, find, or git blame. It uses tracer instead, so every read comes with the file's callers, history, and docs.",
    default: false,
  }),
}

export type TracerMod = Mod<Record<never, never>, typeof options>

type Event = 'SessionStart' | 'PreCompact' | 'SubagentStart' | 'InstructionsLoaded' | 'PreToolUse' | 'PostToolUse' | 'PostToolUseFailure'

type Input = { readonly session_id: string; readonly agent_id: string | undefined; readonly cwd: string } & Readonly<Record<string, unknown>>

type Printed = {
  readonly hookSpecificOutput?: {
    readonly additionalContext?: string
    readonly permissionDecision?: 'deny'
    readonly permissionDecisionReason?: string
    readonly updatedInput?: Readonly<Record<string, unknown>>
  }
}

const MARGIN_MS = 1_000

export const tracer = defineMod({
  name: 'tracer',
  options,
  setup(mod) {
    const { current } = sessions
    const everyAgent = current.goal
    everyAgent.beforeSessionStart(async (start, time) => noteOf(await hook(mod, 'SessionStart', time, { session_id: start.session, agent_id: start.task, cwd: start.cwd, source: start.source })))
    everyAgent.beforeContextEnd(async (context, time) => void (await hook(mod, 'PreCompact', time, { session_id: current.id, agent_id: context.task, cwd: current.cwd })))
    everyAgent.beforePrompt({ task: /./ }, async (prompt, time) => void (await hook(mod, 'SubagentStart', time, { session_id: prompt.session, agent_id: prompt.task, cwd: current.cwd })))
    everyAgent.beforeInstructionFile(async (file, time) => refusalOf(await hook(mod, 'InstructionsLoaded', time, { session_id: file.session, agent_id: file.task, cwd: current.cwd, file_path: file.path })))
    everyAgent.beforeToolCall(async (call, time) => {
      const printed = await hook(mod, 'PreToolUse', time, { session_id: call.session, agent_id: call.task, cwd: call.cwd, tool_name: call.name, tool_input: call.input })
      const input = printed?.hookSpecificOutput?.updatedInput
      return refusalOf(printed) ?? (input === undefined ? undefined : { input })
    })
    everyAgent.beforeToolResult(async (result, time) => {
      const event = result.isError ? 'PostToolUseFailure' : 'PostToolUse'
      const answer = result.isError ? { error: result.output } : { tool_response: result.output }
      return noteOf(await hook(mod, event, time, { session_id: result.session, agent_id: result.task, cwd: result.cwd, tool_name: result.name, tool_input: result.input, ...answer }))
    })
  },
})

function noteOf(printed: Printed | undefined): { readonly note: string } | undefined {
  const note = printed?.hookSpecificOutput?.additionalContext
  return note === undefined ? undefined : { note }
}

function refusalOf(printed: Printed | undefined): { readonly refuse: string } | undefined {
  const output = printed?.hookSpecificOutput
  return output?.permissionDecision === 'deny' ? { refuse: output.permissionDecisionReason ?? '' } : undefined
}

async function hook(mod: TracerMod, event: Event, { deadline }: ListenerTime, input: Input): Promise<Printed | undefined> {
  const timeoutMs = Math.max(deadline - Date.now(), MARGIN_MS)
  const argv = ['trace', 'hook', event, '--harness', 'claude', '--deadline-ms', String(timeoutMs - MARGIN_MS), ...(mod.options.tracerOnly ? ['--tracer-only'] : [])]
  const traced = await process.run(argv, { cwd: input.cwd, stdin: JSON.stringify(input), timeoutMs }).catch((error: unknown) => ({ exitCode: 1, stdout: '', stderr: messageOf(error) }))
  if (traced.exitCode === 0) return traced.stdout.trim() === '' ? undefined : (JSON.parse(traced.stdout) as Printed)
  if (event === 'PreToolUse') return undefined
  throw new Error(`trace hook ${event} exited ${traced.exitCode}: ${traced.stderr.trim().split('\n').at(-1) ?? ''}`)
}
