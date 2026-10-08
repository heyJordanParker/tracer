import { defineMod, type Mod } from '../node_modules/@cmodjs/core/mod.js'
import { option } from '../node_modules/@cmodjs/core/options.js'
import { toolContext } from './enrich.js'
import { commandDocs } from './project-docs.js'
import { archiveAgentLog, forgetLoadedDocs, recordLoadedDoc, startSession } from './session.js'
import { refuseRawRead } from './tracer-only.js'

const ENRICHED_TOOLS = new Set(['Read', 'Edit', 'Write', 'Grep', 'Glob'])

const options = {
  tracerOnly: option.toggle({
    title: 'Claude reads code only through tracer',
    description: "Claude can't read or search this project with grep, cat, find, or git blame. It uses tracer instead, so every read comes with the file's callers, history, and docs.",
    default: false,
  }),
}

export type TracerMod = Mod<Record<never, never>, typeof options>

export const tracer = defineMod({
  name: 'tracer',
  options,
  setup(mod) {
    mod.on('SessionStart', (input) => startSession(mod, input))
    mod.on('InstructionsLoaded', (input) => recordLoadedDoc(mod, input))
    mod.on('PreCompact', (input) => forgetLoadedDocs(mod, input))
    mod.on('SubagentStop', (input) => archiveAgentLog(mod, input))
    mod.on('PreToolUse', async (input) => {
      if (input.tool_name === 'Bash') return (mod.options.tracerOnly ? await refuseRawRead(mod, input) : undefined) ?? commandDocs(mod, input)
      if (ENRICHED_TOOLS.has(input.tool_name)) return toolContext(mod, input)
      return undefined
    })
  },
})
