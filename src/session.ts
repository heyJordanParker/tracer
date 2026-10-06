import type { HookAnswer } from '../node_modules/@cmodjs/core/mod.js'
import { context, trace, type Input, type TracerMod } from './trace.js'

export async function startSession(mod: TracerMod, input: Input<'SessionStart'>): Promise<HookAnswer | undefined> {
  const caller = { cwd: input.cwd, sessionId: input.session_id }
  if (input.source === 'clear') await trace(mod, caller, ['docs', 'reset', '--source', 'tracer_clear'])
  await trace(mod, caller, ['docs', 'prime', '--reason', input.source === 'compact' ? 'post_compact' : 'session_start'])
  await trace(mod, caller, ['docs', input.cwd, '--json', '--source', 'tracer_session_start'])
  if (!mod.state.project.primer) return undefined
  const repository = await mod.process.run(['git', 'rev-parse', '--is-inside-work-tree'], { cwd: input.cwd }).catch(() => undefined)
  if (repository?.exitCode !== 0) return undefined
  const primer = await trace(mod, caller, ['context'], 12_000)
  const text = primer.stdout.trimEnd()
  return primer.exitCode === 0 && text !== '' ? context('SessionStart', text) : undefined
}

export async function recordLoadedDoc(mod: TracerMod, input: Input<'InstructionsLoaded'>): Promise<void> {
  await trace(mod, { cwd: input.cwd, sessionId: input.session_id }, ['docs', 'prime', input.file_path])
}

export async function forgetLoadedDocs(mod: TracerMod, input: Input<'PreCompact'>): Promise<void> {
  await trace(mod, { cwd: input.cwd, sessionId: input.session_id }, ['docs', 'reset', '--source', 'tracer_compact'])
}

export async function archiveAgentLog(mod: TracerMod, input: Input<'SubagentStop'>): Promise<void> {
  await trace(mod, { cwd: input.cwd, sessionId: input.session_id, agentId: input.agent_id }, ['docs', 'archive'])
}
