import type { On } from 'claude-code'
import { registerMod, registerPermissionCheck } from '../node_modules/@cmodjs/core/register.js'
import { tracer } from '../src/mod.js'

export function register(addHook: On): void {
  registerMod(addHook, tracer)
  registerPermissionCheck(addHook)
}
