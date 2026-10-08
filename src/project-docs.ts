import type { HookAnswer, HookInput } from '../node_modules/@cmodjs/core/mod.js'
import { basename, resolve } from '../node_modules/@cmodjs/core/path.js'
import { parseShell } from '../node_modules/@cmodjs/core/shell.js'
import type { TracerMod } from './mod.js'
import { context, trace } from './trace.js'

const PATH_TAKING = new Set(['read', 'info', 'list', 'tree', 'structure', 'grep', 'pattern', 'find', 'blame', 'history', 'diff'])

const VALUED_LEADING = new Set(['-C', '--budget', '--agent', '--filter'])

const TRACE_CALL = /(?<=^|[;&|(\n])(\s*(?:\S*\/)?trace)(?=\s|$)(?!\s+--agent\b)/g

type Traced = {
  readonly subcommand: string
  readonly base: string
  readonly candidates: readonly string[]
}

export async function commandDocs(mod: TracerMod, input: HookInput<'PreToolUse'>): Promise<HookAnswer | undefined> {
  const line = typeof input.tool_input['command'] === 'string' ? input.tool_input['command'] : ''
  if (line === '') return undefined
  const rewrite = withAgent(input, line)
  const call = tracedCall(line, input.cwd)
  let text = ''
  if (call !== undefined && mod.options.projectDocs) {
    const existing = await Promise.all(call.candidates.map(async (path) => ((await mod.fs.exists(path)) ? [path] : [])))
    const targets = existing.flat().length > 0 ? [...new Set(existing.flat())] : [call.base]
    const skips = call.subcommand === 'read' ? targets.flatMap((path) => ['--skip', path]) : []
    const args = ['docs', ...targets, '--budget', String(mod.options.budget), '--source', 'tracer_project_docs', '--triggering-tool', 'Bash', '--triggering-command', line, ...skips]
    const traced = await trace(mod, { cwd: input.cwd, sessionId: input.session_id, agentId: input.agent_id }, args)
    if (traced.exitCode === 0) text = traced.stdout.trim()
  }
  if (text !== '') return context('PreToolUse', text, rewrite)
  if (rewrite !== undefined) return { hookSpecificOutput: { hookEventName: 'PreToolUse', updatedInput: rewrite } }
  return undefined
}

export function tracedCall(line: string, cwd: string): Traced | undefined {
  for (const { argv: [program, ...args], folder } of parseShell(line).commands) {
    if (basename(program) !== 'trace') continue
    let base = resolve(cwd, folder)
    let at = 0
    while (at < args.length && (args[at] as string).startsWith('-')) {
      const flag = args[at] as string
      const value = args[at + 1]
      if (flag === '-C' && value !== undefined) base = resolve(base, value)
      at += VALUED_LEADING.has(flag) ? 2 : 1
    }
    const subcommand = args[at]
    if (subcommand === undefined || !PATH_TAKING.has(subcommand)) continue
    const candidates = args.slice(at + 1).filter((arg) => !arg.startsWith('-')).map((arg) => resolve(base, arg))
    return { subcommand, base, candidates }
  }
  return undefined
}

export function withAgent(input: HookInput<'PreToolUse'>, line: string): Record<string, unknown> | undefined {
  const agent = input.agent_id
  if (agent === undefined || agent === '') return undefined
  const quoted = /^[\w@%+=:,./-]+$/.test(agent) ? agent : `'${agent.replaceAll("'", `'"'"'`)}'`
  const replaced = line.replace(TRACE_CALL, (call) => `${call} --agent ${quoted}`)
  return replaced === line ? undefined : { ...input.tool_input, command: replaced }
}
