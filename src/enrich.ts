import type { HookAnswer, HookInput } from '../node_modules/@cmodjs/core/mod.js'
import { relative, resolve } from '../node_modules/@cmodjs/core/path.js'
import type { TracerMod } from './mod.js'
import { BUDGET, context, trace } from './trace.js'

const NO_MATCHES = '(no matches)'

type Request = {
  readonly args: readonly string[]
  readonly target: string
  readonly isExisting: boolean
}

export async function toolContext(mod: TracerMod, input: HookInput<'PreToolUse'>): Promise<HookAnswer | undefined> {
  const request = await requestOf(mod, input)
  if (request === undefined) return undefined
  const traced = await trace(mod, { cwd: input.cwd, sessionId: input.session_id, agentId: input.agent_id }, request.args)
  const text = traced.stdout.trim()
  if ((traced.exitCode === 0 || traced.exitCode === 2) && text !== '' && text !== NO_MATCHES) return context('PreToolUse', text)
  if (traced.exitCode === 0 || !request.isExisting) return undefined
  const reason = traced.stderr.trim().split('\n').at(-1) || `trace exited ${traced.exitCode}`
  return context('PreToolUse', `${request.target}\n[trace context unavailable: ${reason}]`)
}

async function requestOf(mod: TracerMod, input: HookInput<'PreToolUse'>): Promise<Request | undefined> {
  const tool = input.tool_input
  switch (input.tool_name) {
    case 'Read':
    case 'Edit':
    case 'Write': {
      const target = textOf(tool['file_path'])
      if (target === '') return undefined
      const args = ['context', target, '--budget', BUDGET]
      if (input.tool_name === 'Read') {
        args.push(...option('--offset', tool['offset']), ...option('--limit', tool['limit']))
      } else {
        args.push('--no-record')
      }
      if (input.tool_name === 'Edit') args.push(...(await editedLines(mod, resolve(input.cwd, target), textOf(tool['old_string']))))
      return { args, target, isExisting: input.tool_name !== 'Write' }
    }
    case 'Glob': {
      const pattern = textOf(tool['pattern'])
      if (pattern === '') return undefined
      const path = textOf(tool['path']) || input.cwd
      return { args: ['find', pattern, path, '--budget', BUDGET], target: path, isExisting: false }
    }
    case 'Grep': {
      const pattern = textOf(tool['pattern'])
      if (pattern === '') return undefined
      const path = textOf(tool['path']) || input.cwd
      const args = ['grep', '--budget', BUDGET]
      if (tool['-i'] === true) args.push('-i')
      if (textOf(tool['glob']) !== '') args.push('-g', textOf(tool['glob']))
      if (textOf(tool['type']) !== '') args.push('-t', textOf(tool['type']))
      if (tool['multiline'] === true) args.push('-U')
      return { args: [...args, '--', pattern, shownPath(path, input.cwd)], target: path, isExisting: false }
    }
    default:
      return undefined
  }
}

function shownPath(path: string, cwd: string): string {
  const searched = resolve(cwd, path)
  const inside = relative(cwd, searched)
  return inside === '..' || inside.startsWith('../') ? searched : inside || '.'
}

async function editedLines(mod: TracerMod, path: string, replaced: string): Promise<string[]> {
  if (replaced === '') return []
  const source = await mod.fs.read(path).catch(() => '')
  const at = source.indexOf(replaced)
  if (at < 0) return []
  const line = source.slice(0, at).split('\n').length
  return ['--offset', String(line), '--limit', String(replaced.split('\n').length)]
}

function option(flag: string, value: unknown): string[] {
  return typeof value === 'number' ? [flag, String(value)] : []
}

function textOf(value: unknown): string {
  return typeof value === 'string' ? value : ''
}
