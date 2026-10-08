# WHY

tracer gives coding agents code intelligence in one call: the `trace` program answers questions about a repository with the facts an agent needs, and the tracer mod wires it into Claude Code's own events.

# Facts

- `cli/` is the `trace` program, a Rust workspace; `cli/Claude.md` holds its facts and `cli/tests/Claude.md` its test suite's.
- The repository root is a cmod mod: `src/mod.ts` is the `defineMod`, `hooks/register.ts` only registers it, and `tests/` holds its `bun test` suite.
- `package.json` `cmod.program` names `trace`, so installing the mod downloads the `trace` release for the machine into cmod's programs folder, which Claude Code's Bash and `mod.process.run` find before macOS's `/usr/bin/trace`.
- `package.json` `cmod.permissions` declares what the mod does: run `trace` and `git`, add context, and rewrite tool input. cmod drops any hook answer the person did not grant.
- The mod's settings are cmod options in `src/mod.ts`, which people set in `/config` and a repository in `.claude/cmods/tracer/options.json`.
- `cli/Cargo.toml` `[package.metadata.cmod]` names `cargo xtask dist` as the build `cmod publish` and `cmod link` run.
- `.claude-plugin/plugin.json` and `cli/Cargo.toml` carry the same version.
- `skills/trace/` is the Skill the mod ships.
- `Formula/tracer.rb` is the Homebrew formula, and this repository is its tap. The `Homebrew` workflow rewrites it from each release's `SHA256SUMS` through `scripts/update-formula.sh`.
- `cmod check .` runs every check on the mod. `cmod publish --dry-run .` builds a release without pushing.
- `docs/` is the user documentation, and `README.md` links every page.
