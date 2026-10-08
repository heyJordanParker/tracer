import type { TestedMod } from '../node_modules/@cmodjs/core/testing.js'

export type Ran = {
  readonly argv: readonly string[]
  readonly cwd: string | undefined
  readonly env: Record<string, string> | undefined
}

type Answer = { readonly exitCode?: number; readonly stdout?: string; readonly stderr?: string }

export function fakeTrace(tested: TestedMod<object>, answer: (argv: readonly string[]) => Answer = () => ({})): Ran[] {
  const ran: Ran[] = []
  tested.fakes.process.run = async (argv, init) => {
    ran.push({ argv, cwd: init?.cwd, env: init?.env })
    const { exitCode = 0, stdout = '', stderr = '' } = answer(argv)
    return { exitCode, stdout, stderr, isStdoutTruncated: false, isStderrTruncated: false }
  }
  return ran
}

export function tracedRuns(ran: readonly Ran[]): Ran[] {
  return ran.filter(({ argv }) => argv[0] === 'trace')
}

export function traced(ran: readonly Ran[]): string[][] {
  return tracedRuns(ran).map(({ argv }) => argv.slice(1))
}
