import type { TestedMod } from '../node_modules/@cmodjs/core/testing.js'

export type Ran = {
  readonly argv: readonly string[]
  readonly cwd: string | undefined
  readonly stdin: string | undefined
  readonly timeoutMs: number | undefined
}

type Answer = { readonly exitCode?: number; readonly stdout?: string; readonly stderr?: string }

export function fakeTrace(tested: TestedMod<object>, answer: (argv: readonly string[]) => Answer = () => ({})): Ran[] {
  const ran: Ran[] = []
  tested.fakes.process.run = async (argv, init) => {
    ran.push({ argv, cwd: init?.cwd, stdin: init?.stdin, timeoutMs: init?.timeoutMs })
    const { exitCode = 0, stdout = '', stderr = '' } = answer(argv)
    return { exitCode, stdout, stderr, isStdoutTruncated: false, isStderrTruncated: false }
  }
  return ran
}
