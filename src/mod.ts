import { defineMod, type Mod } from '../node_modules/@cmodjs/core/mod.js'
import { option } from '../node_modules/@cmodjs/core/options.js'
import { toolContext } from './enrich.js'
import { commandDocs } from './project-docs.js'
import { archiveAgentLog, forgetLoadedDocs, recordLoadedDoc, startSession } from './session.js'

const ENRICHED_TOOLS = new Set(['Read', 'Edit', 'Write', 'Grep', 'Glob'])

const options = {
  budget: option.number({ title: 'Context budget', description: 'Characters each tracer hook adds to Claude’s context. Claude Code moves a longer hook message to a file.', default: 10_000, min: 1_000 }),
  primer: option.toggle({ title: 'Repository primer', description: 'Send the repository primer when a session starts', default: true }),
  enrich: option.toggle({ title: 'File facts', description: 'Add each file’s facts to Read, Edit, Write, Grep, and Glob', default: true }),
  projectDocs: option.toggle({ title: 'Project docs', description: 'Send the project docs a trace command reaches', default: true }),
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
    mod.on('PreToolUse', (input) => {
      if (input.tool_name === 'Bash') return commandDocs(mod, input)
      if (ENRICHED_TOOLS.has(input.tool_name) && mod.options.enrich) return toolContext(mod, input)
      return undefined
    })
  },
})
