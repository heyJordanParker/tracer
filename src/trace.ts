import { messageOf, type HookAnswer, type Mod, type ModEvent, type ModHook } from '../node_modules/@cmodjs/core/mod.js'
import type { TracerState } from './state.js'

export type TracerMod = Mod<TracerState>

export type Input<E extends ModEvent> = Parameters<ModHook<E>>[0]

export type Caller = {
  readonly cwd: string
  readonly sessionId: string
  readonly agentId?: string | undefined
}

export type Traced = {
  readonly exitCode: number
  readonly stdout: string
  readonly stderr: string
}

export async function trace(mod: TracerMod, caller: Caller, args: readonly string[], timeoutMs = 10_000): Promise<Traced> {
  const env: Record<string, string> = { AGENT_SESSION_ID: caller.sessionId }
  if (caller.agentId !== undefined) env['TRACER_AGENT_ID'] = caller.agentId
  try {
    const { exitCode, stdout, stderr } = await mod.process.run(['trace', ...args], { cwd: caller.cwd, env, timeoutMs })
    return { exitCode, stdout, stderr }
  } catch (error) {
    return { exitCode: 1, stdout: '', stderr: messageOf(error) }
  }
}

export function context(event: 'SessionStart' | 'PreToolUse', text: string, updatedInput?: Record<string, unknown>): HookAnswer {
  return { hookSpecificOutput: { hookEventName: event, additionalContext: text, ...(updatedInput === undefined ? {} : { updatedInput }) } }
}

export function resolvePath(target: string, cwd: string): string {
  const parts: string[] = []
  for (const part of (target.startsWith('/') ? target : `${cwd}/${target}`).split('/')) {
    if (part === '' || part === '.') continue
    if (part === '..') parts.pop()
    else parts.push(part)
  }
  return `/${parts.join('/')}`
}

export async function exists(mod: TracerMod, path: string): Promise<boolean> {
  const slash = path.lastIndexOf('/')
  const name = path.slice(slash + 1)
  if (name === '') return true
  const entries = await mod.fs.list(path.slice(0, slash) || '/').catch(() => [])
  return entries.some((entry) => entry.name === name)
}
