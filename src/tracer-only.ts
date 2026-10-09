import type { HookAnswer, HookInput } from '../node_modules/@cmodjs/core/mod.js'
import { basename, dirname, resolve } from '../node_modules/@cmodjs/core/path.js'
import { parseShell } from '../node_modules/@cmodjs/core/shell.js'
import type { TracerMod } from './mod.js'
import { quote } from './trace.js'

const READERS = new Set(['cat', 'head', 'tail', 'sed', 'awk'])
const SEARCHERS = new Set(['grep', 'egrep', 'fgrep', 'rg'])
const LISTERS = new Set(['ls', 'tree'])
const FILTERS = new Set(['grep', 'egrep', 'fgrep', 'rg', 'sed', 'awk', 'head', 'tail', 'cut', 'sort', 'uniq', 'wc', 'column', 'fold', 'tr', 'jq'])
const SEARCH_VALUED = new Set(['-e', '-f', '-g', '--glob', '-t', '--type', '-T', '--type-not', '-m', '--max-count', '-A', '-B', '-C', '-M'])
const FIND_ACTIONS = new Set(['-delete', '-exec', '-execdir', '-ok', '-okdir'])
const GLOB = /[*?[]/

type Command = {
  readonly program: string
  readonly args: readonly string[]
  readonly base: string
}

export async function refuseRawRead(mod: TracerMod, input: HookInput<'PreToolUse'>): Promise<HookAnswer | undefined> {
  const line = typeof input.tool_input['command'] === 'string' ? input.tool_input['command'] : ''
  const { commands } = parseShell(line)
  for (const { argv: [program, ...args], folder, input: piped } of commands) {
    const source = piped === undefined ? undefined : commands[piped]?.argv
    const replacement =
      source !== undefined && basename(source[0]) === 'trace' && FILTERS.has(basename(program))
        ? filteredReplacement(source, basename(program), args)
        : await replacementOf(mod, { program: basename(program), args, base: resolve(input.cwd, folder) })
    if (replacement !== undefined) {
      return {
        hookSpecificOutput: {
          hookEventName: 'PreToolUse',
          permissionDecision: 'deny',
          permissionDecisionReason: `tracer: Claude reads this project's code only through tracer.\nRun this instead: ${replacement}`,
        },
      }
    }
  }
  return undefined
}

function filteredReplacement(trace: readonly string[], filter: string, args: readonly string[]): string {
  const expression = filter === 'jq' ? args.find((arg) => !arg.startsWith('-')) : undefined
  if (expression === undefined) return shell(trace)
  return shell([...trace, ...(trace.includes('--json') ? [] : ['--json']), '--filter', expression])
}

async function replacementOf(mod: TracerMod, command: Command): Promise<string | undefined> {
  if (command.program === 'git') return gitReplacement(mod, command)
  if (SEARCHERS.has(command.program)) return searchReplacement(mod, command)
  if (READERS.has(command.program)) return readReplacement(mod, command)
  if (LISTERS.has(command.program)) return listReplacement(mod, command)
  if (command.program === 'find') return findReplacement(mod, command)
  return undefined
}

async function searchReplacement(mod: TracerMod, { program, args, base }: Command): Promise<string | undefined> {
  const positional: string[] = []
  const flags: string[] = []
  let pattern: string | undefined
  for (let at = 0; at < args.length; at += 1) {
    const arg = args[at] as string
    if (SEARCH_VALUED.has(arg)) {
      const value = args[at + 1]
      if (arg === '-e' && value !== undefined) pattern = value
      if ((arg === '-g' || arg === '--glob' || arg === '-t' || arg === '--type') && value !== undefined) flags.push(arg.length === 2 ? arg : `-${arg[2]}`, value)
      at += 1
    } else if (arg === '-i' || arg === '--ignore-case') {
      flags.push('-i')
    } else if (!arg.startsWith('-')) {
      positional.push(arg)
    }
  }
  if (pattern === undefined) pattern = positional.shift()
  if (pattern === undefined) return undefined
  const paths = await projectPaths(mod, base, positional)
  const recursive = program === 'rg' || args.some((arg) => arg === '-r' || arg === '-R' || arg === '--recursive')
  if (paths.length === 0 && !(recursive && positional.length === 0 && (await inProject(mod, base, '.')))) return undefined
  return shell(['trace', 'grep', pattern, ...paths, ...flags])
}

async function readReplacement(mod: TracerMod, { program, args, base }: Command): Promise<string | undefined> {
  if (program === 'sed' && args.some((arg) => arg.startsWith('-i') || arg === '--in-place')) return undefined
  const paths = await projectPaths(mod, base, args)
  return paths.length === 0 ? undefined : shell(['trace', 'read', ...paths])
}

async function listReplacement(mod: TracerMod, { program, args, base }: Command): Promise<string | undefined> {
  const named = args.filter((arg) => !arg.startsWith('-'))
  const paths = named.length === 0 ? ((await inProject(mod, base, '.')) ? ['.'] : []) : await projectPaths(mod, base, named)
  if (paths.length === 0) return undefined
  return program === 'tree' ? shell(['trace', 'tree', paths[0] as string]) : shell(['trace', 'list', ...paths])
}

async function findReplacement(mod: TracerMod, { args, base }: Command): Promise<string | undefined> {
  if (args.some((arg) => FIND_ACTIONS.has(arg))) return undefined
  const end = args.findIndex((arg) => arg.startsWith('-') || arg === '(' || arg === '!')
  const named = end < 0 ? args : args.slice(0, end)
  const bases = named.length === 0 ? ((await inProject(mod, base, '.')) ? ['.'] : []) : await projectPaths(mod, base, named)
  if (bases.length === 0) return undefined
  const name = args.findIndex((arg) => arg === '-name' || arg === '-iname')
  return shell(['trace', 'find', name < 0 ? '*' : (args[name + 1] ?? '*'), ...bases])
}

async function gitReplacement(mod: TracerMod, { args, base }: Command): Promise<string | undefined> {
  const at = args.findIndex((arg) => !arg.startsWith('-'))
  const subcommand = args[at]
  if (subcommand === undefined) return undefined
  const rest = args.slice(at + 1)
  const separator = rest.includes('--') ? rest.indexOf('--') : rest.length
  const flags = rest.slice(0, separator).filter((arg) => arg.startsWith('-'))
  const positional = [...rest.slice(0, separator).filter((arg) => !arg.startsWith('-')), ...rest.slice(separator + 1)]
  switch (subcommand) {
    case 'blame':
    case 'annotate': {
      const [file] = await projectPaths(mod, base, positional)
      if (file === undefined) return undefined
      const range = rest[rest.indexOf('-L') + 1]
      const lines = rest.includes('-L') && range !== undefined && /^\d+,\d+$/.test(range) ? ['--lines', range.replace(',', ':')] : []
      return shell(['trace', 'blame', file, ...lines])
    }
    case 'grep': {
      const [pattern, ...paths] = positional
      return pattern === undefined ? undefined : shell(['trace', 'grep', pattern, ...(await projectPaths(mod, base, paths))])
    }
    case 'show':
    case 'cat-file': {
      if (subcommand === 'cat-file' && !flags.includes('-p')) return undefined
      const shown = positional.find((arg) => arg.includes(':'))
      if (shown === undefined) return undefined
      const split = shown.indexOf(':')
      return shell(['trace', 'read', shown.slice(split + 1), '--at', shown.slice(0, split) || 'HEAD'])
    }
    case 'log': {
      if (flags.some((flag) => flag.startsWith('-G') || flag === '-p' || flag === '--patch')) return undefined
      const search = rest.indexOf('-S')
      if (search >= 0) {
        const text = rest[search + 1]
        return text === undefined || text.startsWith('-') ? 'trace history --contains <pattern>' : shell(['trace', 'history', '--contains', text])
      }
      if (flags.includes('-L')) return 'trace history <file> <symbol>'
      const paths = await projectPaths(mod, base, positional)
      return paths.length === 0 || paths.length !== positional.length ? undefined : shell(['trace', 'history', paths[0] as string])
    }
    case 'diff':
      return flags.includes('--name-status') ? 'trace diff' : undefined
    default:
      return undefined
  }
}

async function projectPaths(mod: TracerMod, base: string, args: readonly string[]): Promise<string[]> {
  const found = await Promise.all(args.map(async (arg) => ((await inProject(mod, base, arg)) ? [arg] : [])))
  return found.flat()
}

async function inProject(mod: TracerMod, base: string, arg: string): Promise<boolean> {
  if (arg === '' || arg.startsWith('-')) return false
  const glob = arg.search(GLOB)
  const path = glob < 0 ? resolve(base, arg) : resolve(base, dirname(`${arg.slice(0, glob)}x`))
  const root = mod.projectRoot
  return (path === root || path.startsWith(`${root}/`)) && (await mod.fs.exists(path))
}

function shell(words: readonly string[]): string {
  return words.map(quote).join(' ')
}
