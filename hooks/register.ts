import type { On, PluginOptions } from 'claude-code'
import { registerMod } from '../node_modules/@cmodjs/core/register.js'
import { tracer } from '../src/mod.js'

export function register(addHook: On, options: PluginOptions): void {
  registerMod(addHook, tracer, options)
}
