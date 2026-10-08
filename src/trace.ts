import { messageOf, type HookAnswer } from '../node_modules/@cmodjs/core/mod.js'
import type { TracerMod } from './mod.js'

export const BUDGET = '10000'

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

export function quote(word: string): string {
  return /^[\w@%+=:,./-][\w@%+=:,./~^-]*$/.test(word) ? word : `'${word.replaceAll("'", `'"'"'`)}'`
}

export function context(event: 'SessionStart' | 'PreToolUse', text: string, updatedInput?: Record<string, unknown>): HookAnswer {
  return { hookSpecificOutput: { hookEventName: event, additionalContext: text, ...(updatedInput === undefined ? {} : { updatedInput }) } }
}
