import { defineMod } from '../node_modules/@cmodjs/core/mod.js'
import { toolContext } from './enrich.js'
import { commandDocs } from './project-docs.js'
import { archiveAgentLog, forgetLoadedDocs, recordLoadedDoc, startSession } from './session.js'
import { initialState } from './state.js'

const ENRICHED_TOOLS = new Set(['Read', 'Edit', 'Write', 'Grep', 'Glob'])

export const tracer = defineMod({
  name: 'tracer',
  state: initialState,
  setup(mod) {
    mod.on('SessionStart', (input) => startSession(mod, input))
    mod.on('InstructionsLoaded', (input) => recordLoadedDoc(mod, input))
    mod.on('PreCompact', (input) => forgetLoadedDocs(mod, input))
    mod.on('SubagentStop', (input) => archiveAgentLog(mod, input))
    mod.on('PreToolUse', (input) => {
      if (input.tool_name === 'Bash') return commandDocs(mod, input)
      if (ENRICHED_TOOLS.has(input.tool_name) && mod.state.project.enrich) return toolContext(mod, input)
      return undefined
    })
  },
})
