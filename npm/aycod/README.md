# @ay-code/cli

**Guardrails for agentic development: let AI agents write the code, and keep production safe.**

Locally, aycod frames Claude Code, Codex CLI and Copilot CLI: every command and edit goes through a guard, and the agent cannot finish, commit or push while your checks fail. In CI, `aycod ci` gives the same verdict on every pull or merge request. It enforces the verdict of your formatter, linter, type checker, tests and security scanners, and stops the agent from going around them.

This package installs the native aycod binary for your platform (Linux x64/arm64, macOS Apple silicon/Intel) through an optional dependency. No install script and no Node at run time: a 40-line `sh` launcher finds the binary and `exec`s it (about 3 ms per call).

```bash
npm install -g @ay-code/cli
cd my-project
aycod init --stack sveltekit   # writes aycod.toml: review the tasks
aycod install                  # git hooks + agent hooks + AGENTS.md
aycod doctor                   # what is really protected here
```

## Global on developer machines, local for CI

**On a machine where an agent works, install it globally.** The git hooks and the agent hooks call `aycod` from `PATH`, and neither git nor the agents look in `node_modules/.bin`: with only a project install, the hooks fail closed and block every commit (`aycod install` and `aycod doctor` say so). It is also safer: the program that guards the agent stays outside the project, while `node_modules` is something the agent rewrites all the time (`npm install`).

**In the project, as a dev dependency, it is useful for CI** (the agent does not control that machine) and to pin the version the team uses:

```bash
npm install -D @ay-code/cli
```

```yaml
# .github/workflows/ci.yml
- run: npm ci
- run: npx aycod ci
```

Documentation, other install methods and release verification: https://github.com/AyenaCode/aycod
