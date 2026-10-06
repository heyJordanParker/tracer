import type { HookAnswer } from '../node_modules/@cmodjs/core/mod.js'
import { context, resolvePath, trace, type Input, type TracerMod } from './trace.js'

const NO_MATCHES = '(no matches)'

type Request = {
  readonly args: readonly string[]
  readonly target: string
  readonly isExisting: boolean
}

export async function toolContext(mod: TracerMod, input: Input<'PreToolUse'>): Promise<HookAnswer | undefined> {
  const request = await requestOf(mod, input)
  if (request === undefined) return undefined
  const traced = await trace(mod, { cwd: input.cwd, sessionId: input.session_id, agentId: input.agent_id }, request.args)
  const text = traced.stdout.trim()
  if ((traced.exitCode === 0 || traced.exitCode === 2) && text !== '' && text !== NO_MATCHES) return context('PreToolUse', text)
  if (traced.exitCode === 0 || !request.isExisting) return undefined
  const reason = traced.stderr.trim().split('\n').at(-1) || `trace exited ${traced.exitCode}`
  return context('PreToolUse', `${request.target}\n[trace context unavailable: ${reason}]`)
}

async function requestOf(mod: TracerMod, input: Input<'PreToolUse'>): Promise<Request | undefined> {
  const tool = input.tool_input
  const budget = String(mod.state.project.budget)
  switch (input.tool_name) {
    case 'Read':
    case 'Edit':
    case 'Write': {
      const target = textOf(tool['file_path'])
      if (target === '') return undefined
      const args = ['context', target, '--budget', budget]
      if (input.tool_name === 'Read') {
        args.push(...option('--offset', tool['offset']), ...option('--limit', tool['limit']))
      } else {
        args.push('--no-record')
      }
      if (input.tool_name === 'Edit') args.push(...(await editedLines(mod, resolvePath(target, input.cwd), textOf(tool['old_string']))))
      return { args, target, isExisting: input.tool_name !== 'Write' }
    }
    case 'Glob': {
      const pattern = textOf(tool['pattern'])
      if (pattern === '') return undefined
      const path = textOf(tool['path']) || input.cwd
      return { args: ['find', pattern, path, '--budget', budget], target: path, isExisting: false }
    }
    case 'Grep': {
      const pattern = textOf(tool['pattern'])
      if (pattern === '') return undefined
      const path = textOf(tool['path']) || input.cwd
      const args = ['grep', '--budget', budget]
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
  const root = resolvePath(cwd, '/')
  const searched = resolvePath(path, root)
  if (searched === root) return '.'
  return searched.startsWith(`${root}/`) ? searched.slice(root.length + 1) : searched
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
