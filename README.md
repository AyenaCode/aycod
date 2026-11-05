# aycod

**Guardrails for agentic development: let AI agents write the code, and keep production safe.**

Website: https://ayenacode.github.io/aycod/

AI coding agents (Claude Code, Codex, Copilot…) write code faster than anyone can review it. They also skip a failing test, add an `eslint-disable`, force-push, or read a `.env` when it gets them to "done". aycod puts a frame around them so that developers, and vibe coders, can trust what the agent ships:

- **On your machine, while the agent works**, every command and every edit goes through a guard, and the agent cannot finish its turn, commit or push while the project's checks fail.
- **In CI, on every pull or merge request**, the same binary runs the same checks and gives the same verdict, so nothing that slipped past a laptop reaches production.

**aycod does not judge code itself.** It enforces the verdict of tools that are already reliable (formatter, linter, type checker, tests, security and secret scanners), runs them fast, and stops the agent from going around them.

One native binary: 2.2 MB, fully static on Linux, no runtime, no service, no network, about 1 ms per agent action. It works the same on a private or public project, on GitHub, GitLab, anything else or no remote at all.

```bash
curl -fsSL https://github.com/AyenaCode/aycod/releases/latest/download/install.sh | sh
cd my-project
aycod init --stack sveltekit  # writes aycod.toml: review the tasks
aycod install                 # git hooks + agent hooks + AGENTS.md
aycod doctor                  # what is really protected here
```

```yaml
# GitHub Actions (any CI works the same): on every PR, the same verdict as on the developer's machine
- run: curl -fsSL https://github.com/AyenaCode/aycod/releases/latest/download/install.sh | sh && echo "$HOME/.local/bin" >> "$GITHUB_PATH"
- run: aycod ci
```

## Frameworks, not just languages

Code-quality platforms such as SonarQube analyse a language. An agent breaks production through its framework: a SvelteKit `load` that leaks a secret to the client, a Next.js server action without validation, an Expo build that no longer compiles for iOS. aycod aims to know each framework: which tools and rules give a trustworthy verdict, which files define the checks and must stay out of the agent's reach, and which commands an agent must never run.

| Today (v0.1) | On the roadmap |
|---|---|
| SvelteKit, Rust, Go, and any project through `generic` with your own tasks | Next.js, Nuxt, full-stack monorepos, Expo / React Native, native Android, native iOS |

## Status and roadmap

**v0.1 is an early release.** The guard, the gate, the git hooks and `aycod ci` work and are tested end to end. The agent adapters are tested against payloads built from each agent's documentation, not yet in recorded real sessions (see [Agents](#agents-hook-formats-checked-against-the-official-docs-on-2026-10-28)).

Towards a stable v1, in broad strokes (details will come with research):

1. **Prove the frame**: scripted adversarial sessions with each agent, every bypass found added to the test corpus.
2. **Framework packs**: curated checks, security scanners, protected files and guard rules for each framework above, set up by `aycod init`.
3. **PR / MR scanning**: CI templates for GitHub Actions and GitLab CI, a readable report on the pull or merge request, usable as a required check.
4. **Wider reach**: more agents, Windows, a stable configuration format and CLI.

With v1, the goal is simple: install aycod in any supported framework project, let the AI code with confidence locally, and let the same tool scan every PR or MR before it reaches production.

## Install

Linux (x86_64, aarch64: one static binary for every distribution, Alpine included) and macOS (Apple silicon, Intel). Windows: not yet.

| Method | Command |
|---|---|
| Script (checks the SHA-256, installs to `~/.local/bin`) | `curl -fsSL https://github.com/AyenaCode/aycod/releases/latest/download/install.sh \| sh` |
| npm (global) | `npm install -g @ay-code/cli` |
| Archive | download `aycod-<target>.tar.gz` from the [releases](https://github.com/AyenaCode/aycod/releases), put `aycod` on your `PATH` |
| From source (Rust ≥ 1.88) | `cargo install --locked --git https://github.com/AyenaCode/aycod` |

The script takes `AYCOD_VERSION=v0.1.0` (default: latest) and `AYCOD_INSTALL_DIR` (default: `~/.local/bin`). To read it before running it: `curl -fsSLO https://github.com/AyenaCode/aycod/releases/latest/download/install.sh && less install.sh && sh install.sh`.

**`aycod` must be on `PATH`**: the git hooks and the agent hooks call it by name, and fail closed (they block) when they cannot find it. With npm, install it globally on developer machines: git and the agents never look in `node_modules/.bin`, and the guard is safer outside the project the agent edits. `aycod install` and `aycod doctor` report a project-only install. As a dev dependency (`npm install -D @ay-code/cli`), it is still useful in CI (`npm ci && npx aycod ci`) and to pin the team's version. The npm package runs no install script and no Node at run time; its `sh` launcher `exec`s the binary (about 3 ms more per call).

Each release archive and `install.sh` carries a signed build provenance attestation: `gh attestation verify aycod-<target>.tar.gz --repo AyenaCode/aycod` (see [SECURITY.md](SECURITY.md)).

## What it protects against

The agent, not the owner. The owner always keeps a way out: start the agent with `AYCOD_GATE=off` to skip the end-of-turn gate, or run `git commit/push --no-verify` by hand.

| Layer | What aycod does | Needs a service? |
|---|---|---|
| 1. Instructions | `AGENTS.md` block (and `@AGENTS.md` in `CLAUDE.md`): rules, and what is actually enforced for each agent | no |
| 2. Agent hooks | Every shell command, file read and file edit goes through the guard; the agent cannot end its turn while `aycod check` fails | no |
| 3. Git hooks | `pre-commit`: `aycod check`. `pre-push`: no force push, no direct push to (or deletion of) a protected branch, the pushed commit must pass the checks | no |
| 4. CI (optional) | Any CI runs `aycod ci`: same verdict as on your machine | yes, any |

Layers 2 and 3 are enough against the agent: it cannot bypass the git hooks (`--no-verify`, `git -c core.hooksPath=…`, `GIT_CONFIG_*` are refused), nor edit `.githooks/`, `aycod.toml` or its own config. Layer 3 also covers agents that have no hooks but commit with git.

### Agents (hook formats checked against the official docs on 2026-10-28)

| Agent | Commands and edits checked | Cannot finish while checks fail | Config written by `aycod install` |
|---|---|---|---|
| Claude Code | yes (`PreToolUse`, `onFailure: block`) | yes (`Stop`, exit 2) | `.claude/settings.json` (merged; your other settings and hooks are kept) |
| Codex CLI | yes (`PreToolUse`; `apply_patch` paths are read from the patch) | yes (`Stop`, `decision: block`) | `.codex/hooks.json` (Codex asks you to trust it once) |
| Copilot CLI | yes (`preToolUse`, fail-closed) | yes (`agentStop`; Copilot gives up after 8 blocks, the git hooks still hold) | `.github/hooks/aycod.json` |
| Anything else that commits with git | no | commits and pushes only (layer 3) | — |

`aycod install` configures the agents it detects (config folder present or binary on `PATH`); `--agent claude,codex,copilot` chooses explicitly.

**Not validated yet:** the adapters are tested against payloads built from the docs, not yet in a real session of each agent. The plan's exit criterion (a scripted adversarial session per agent, recorded with date and version) is still open. Questions for those sessions:

- In Claude Code's bypass-permissions mode, does an `ask` answer (guarded files) become allow or deny?
- Codex and Copilot send the shell's `cwd` but no project directory: if their shell leaves the project, aycod falls back to the git root above that `cwd`. Claude Code sets `CLAUDE_PROJECT_DIR`, which aycod uses.

## Commands

| Command | Who | What |
|---|---|---|
| `aycod init [--stack sveltekit\|rust\|go\|generic] [--force]` | owner | Writes a commented `aycod.toml` |
| `aycod install [--agent a,b]` | owner | Writes `.githooks/pre-commit` and `pre-push`, sets `core.hooksPath`, writes the agent configs and the `AGENTS.md` block, records the integrity of all these files. Idempotent: run it again after editing `aycod.toml` |
| `aycod doctor` | anyone | Layers really active, integrity, unjustified suppressions, missing tools |
| `aycod check [--no-cache]` | anyone | Integrity → cache → suppressions → `prepare` → `check` tasks in parallel |
| `aycod ci` | CI | `check` without cache or integrity, then the `ci` tasks in order; prints the full log of a failure |
| `aycod hook <agent> <pre-tool\|post-tool\|stop>` | the agent | Reads the agent's JSON on stdin, answers in the agent's format |
| `aycod git <pre-commit\|pre-push>` | git | Called by the scripts in `.githooks/` |

An agent may run `aycod check`, `ci`, `doctor`, `hook` and `version`. Every other `aycod` command is refused by the guard: nothing an agent can run weakens the policy.

## `aycod.toml`

The file is protected: no agent can edit it. After changing it, run `aycod install` (otherwise `aycod check` reports that a guardrail file changed).

```toml
stack = "sveltekit"
protected_branches = ["main", "master"]   # default

# Never editable by an agent. Always protected on top of this list: aycod.toml, .aycod/, .githooks/,
# .git/, .claude/settings.json, .claude/settings.local.json, .codex/, .github/hooks/,
# .cursor/hooks.json, ~/.gitconfig, ~/.config/git/.
protected = ["pnpm-workspace.yaml"]

# Define what the checks check. Edit tool: the owner is asked (Claude, Copilot) or the edit is
# refused with that explanation (Codex). Shell writes: refused. Globs allowed.
guarded = ["vite.config.ts", "eslint.config.js", ".github/workflows/**"]

# Globs exempt from the suppressions rule (generated code only).
suppressions_allow = []

# Run on every file an agent edits (the path is appended). Optional.
format = "pnpm exec prettier --write --ignore-unknown --log-level warn"

prepare = ["pnpm exec svelte-kit sync"]           # once, in order, before the check tasks
check = ["fmt-check", "lint", "typecheck", "test"] # in parallel: Stop hook, pre-commit, pre-push, aycod ci
ci = ["coverage", "audit", "e2e"]                  # in order, after check: aycod ci only
pre_commit = ["secrets"]                           # pre-commit only, never cached

[tasks]
fmt-check = "pnpm exec prettier --check --log-level warn ."
lint = "pnpm exec eslint --max-warnings 0 ."
test = "pnpm exec vitest run"
audit = ["pnpm audit --audit-level high", "sh -c 'pnpm licenses list --json | node scripts/check-licenses.ts'"]
e2e = ["pnpm exec vite build", "PORT=4173 HOST=127.0.0.1 pnpm exec playwright test"]
secrets = { run = "gitleaks git --pre-commit --staged --redact --no-banner", optional = true }
```

A task is one command, a list of commands run in order, or `{ run = …, optional = true }` (skipped with a notice when its program is not installed). **Tasks run without a shell**: `KEY=value` prefixes set the environment, quotes group arguments, and anything with a pipe or a redirection must be wrapped in `sh -c '…'` explicitly. Each task writes its own log under `.git/aycod/logs/`.

No `make`: aycod runs the tasks itself, which is what allows the cache, the parallelism and, later, Windows without WSL. A `Makefile` can stay as a shortcut (`check: ; aycod check`).

## How the gate works

1. **Integrity.** `aycod install` records the blake3 hash of `aycod.toml`, `.githooks/*` and the agent configs, in `.git/aycod/integrity`. If one of them changes, a git hook loses its executable bit, or `core.hooksPath` is no longer `.githooks`, `check` fails until the owner runs `aycod install`.
2. **Cache.** One walk of the project (honouring `.gitignore`, skipping `.git/`) hashes every file. If the fingerprint equals the one recorded after the last green run (`.git/aycod/green`), nothing runs. The fingerprint also covers the aycod version and the `prepare`/`check` lists.
3. **Suppressions.** Any `@ts-ignore`, `eslint-disable`, `it.skip(`, `.only(`, `#[ignore]`, `# noqa`, `t.Skip(`… fails the check unless the line carries `AYCOD-OK: <reason>` (`FORGE-OK:` is accepted for projects adopted from forge). Markdown, lockfiles and `.claude/` are exempt. This is what keeps the tests on.
4. **Tasks.** `prepare` in order, then the `check` tasks in parallel, each in its own process with its own log. When everything is green, the fingerprint is stored.

When a check fails, the agent receives which task failed, the command, the path of the full log and its last 40 lines: enough to fix it on the first try (Copilot gives up after 8 blocks).

## The guard

The guard does not use regular expressions on the command line. A small shell lexer splits it into simple commands (`;`, `&&`, `||`, `|`, subshells, `$(…)`, backquotes, process substitution, heredocs, redirections), resolves quotes, escapes and literal variables (`F=--force; git push $F` is seen as `git push --force`), unwraps `env`, `command`, `exec`, `nohup`, `timeout`, `xargs`, `find -exec`, `npx`, `pnpm exec`, `bash -c '…'` (recursively), and applies the rules to each command.

Refused:

- force push in all its forms (`-f`, `--force-with-lease`, `+refspec`, `--mirror`, `--all`), pushing to or deleting a protected branch (the current branch is read from `.git/HEAD` when no refspec is given);
- bypassing git hooks: `--no-verify`, `commit -n`, any `git -c`, `--config-env`, `git config` writes, `GIT_CONFIG_*` and `AYCOD_*` variables;
- destroying work: `reset --hard`, `clean -f`, `branch -D`, `checkout .`, `restore .`, `stash drop|clear`, `filter-branch`, `filter-repo`;
- `sudo`, `doas`, `su`; publishing (`npm publish`, `cargo publish`, `eas submit`…); `chmod 777`; `docker run --privileged`;
- deleting outside the project (temporary directories excepted);
- writing a protected or guarded path through the shell: redirections, `rm`, `mv`, `cp`, `tee`, `sed -i`, `perl -i`, `ln`, `chmod`, `truncate`, `dd of=`, `find -delete`, `cd` followed by a relative path, globs that match (`rm .git*`);
- reading secrets: `.env*` (except `.env.example` and similar templates), `*.pem`, `*.key`, `*.p12`, SSH keys, `~/.aws/`, `secrets/`;
- **what it cannot see**: `eval`, a command or script produced by a variable or a substitution, `bash -c "$X"`, `env -S`, code piped into a shell or an interpreter (`curl … | sh`, `base64 -d | sh`), file operations on generated arguments (`xargs rm`), and inline code (`python -c`, `node -e`, heredocs) that starts processes or names a protected path or a secret.

`echo`/`printf` arguments and commit messages (`-m`) may mention a secret file (`echo .env >> .gitignore`): they do not read it.

Every rule has lines in [`tests/corpus.txt`](tests/corpus.txt) (248 entries, attacks and legitimate commands). Any bypass found goes into the corpus first.

### Fail-closed

A panic or an internal error in `aycod hook` exits 2, which every supported agent treats as "block" (a panic hook runs before the abort). Unreadable input is a deny. The installed Claude Code hook also sets `onFailure: "block"`. If `aycod` is missing from `PATH`, the git hooks fail and block the commit.

## Performance

Measured on 2026-10-28, Intel Core 5 210H (12 threads), Linux, on a copy of a real SvelteKit project (218 files outside ignored folders), `scripts/bench.py`, 300 runs each, wall clock per process:

| Measure | p50 | p95 | Budget (plan §6) |
|---|---|---|---|
| `aycod hook` — allowed command | 1.2 ms | 1.9 ms | < 10 ms |
| `aycod hook` — denied command | 1.6 ms | 3.0 ms | < 10 ms |
| `aycod hook` — edit | 1.0 ms | 1.8 ms | < 10 ms |
| `aycod check`, nothing changed | 5.9 ms | 8.2 ms | < 50 ms |
| Stop hook, nothing changed | 4.8 ms | 7.1 ms | < 50 ms |
| Binary size | 2.19 MB | | < 15 MB |

On the same project, the four check tasks take 16.3 s with forge's `make check` (sequential) and 7.4 s with `aycod check` (parallel).

What made it fast, measured rather than assumed:

- the hook path parses argv by hand (no argument parser), reads one small JSON document, parses `aycod.toml` directly and never touches the network. The whole hook stays at or under 3 ms p95, so the precompiled `policy.bin` of the plan was not needed;
- the cached check walks the tree sequentially (the parallel walker of `ignore` idles in 1 ms sleeps on small trees) and hashes in parallel only above 512 files;
- the suppressions regex is compiled only when the cache misses;
- `core.hooksPath` is read from `.git/config` directly, falling back to `git` only when the file uses `include`;
- release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`, `opt-level = 3`. `opt-level = "s"` gives 1.73 MB for the same hook times, `"z"` 1.68 MB and a slower check; 3 is kept for hashing large trees;
- the Linux releases are static musl binaries: no dynamic loader, so a hook starts in about 0.8 ms instead of 1.5 ms with glibc. musl's allocator takes a global lock, which first made the cached check on a 6000-file tree 5× slower (138 ms against 26 ms); each hashing worker now reuses one read buffer and one path, which brought it to about 30 ms with no C allocator to link (measured 2026-11-05, same machine).

Rust makes the hook fast; it does not make `tsc` or the tests faster. The gains on the checks come from the cache and the parallelism.

## Limits

- **Parsing a shell perfectly is impossible.** The guard lowers the risk, it does not remove it: it refuses what it cannot resolve, but a script file written by the agent and then run (`bash scripts/x.sh`) is not inspected. Symlinks are not followed when matching paths. The integrity check and the git hooks are the next line; a CI (layer 4) is the only one that runs off the agent's machine.
- The pre-commit hook checks the **working tree**, not only the staged changes (same as forge).
- Reading secrets is blocked by name; a recursive `grep` over the project can still print a secret that is not ignored by git.
- Linux and macOS only for now (Windows is on the [roadmap](#status-and-roadmap)).
- Departures from the plan: one crate instead of a Cargo workspace (the binary is the only product so far), no `clap` (a dozen commands parsed by hand, nothing to gain on startup or size), no `policy.bin` (measured unnecessary, see Performance).
- Not in v0.1: framework packs beyond the stacks above, CI templates and PR / MR reports, `adopt` (upgrading forge projects automatically), stack scaffolding with the official generators.

## Development

```bash
cargo test                       # unit tests, adversarial corpus, end-to-end tests of the binary
cargo clippy --all-targets       # pedantic, no warnings
cargo build --release
scripts/bench.py target/release/aycod <green project> 300   # exits 1 if a budget is exceeded
```

### Releasing

1. Bump `version` in `Cargo.toml` (the npm packages take it from there), commit, push to `main`, wait for CI.
2. `git tag v0.2.0 && git push origin v0.2.0`. The [release workflow](.github/workflows/release.yml) checks that the tag matches `Cargo.toml`, tests and builds the four targets, writes `SHA256SUMS`, attests the artifacts, publishes the GitHub release (with `install.sh`), then stages the five npm packages.
3. On npmjs.com → *Staged Packages*, approve with 2FA: the four `@ay-code/cli-<platform>` first, `@ay-code/cli` last.

| Target | Runner | npm package |
|---|---|---|
| `x86_64-unknown-linux-musl` | `ubuntu-24.04` | `@ay-code/cli-linux-x64` |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` | `@ay-code/cli-linux-arm64` |
| `aarch64-apple-darwin` | `macos-15` | `@ay-code/cli-darwin-arm64` |
| `x86_64-apple-darwin` | `macos-15` (cross-compiled, not run) | `@ay-code/cli-darwin-x64` |

npm trusted publishing (OIDC) needs each package to exist before it can trust the workflow, so the very first version is published by hand once: download the release archives into `dist/`, run `node npm/build.mjs dist npm-out`, then `npm publish` in each `npm-out/aycod-*` and finally in `npm-out/aycod`. Then, for each of the five packages: `npm trust github <package> --file release.yml --repo AyenaCode/aycod --allow-publish`.

The end-to-end tests run the real binary: the three agent formats, fail-closed on panic and bad input, the gate and its cache, suppressions, integrity and tampering, idempotent install that keeps foreign Claude settings, and the 19 pre-push scenarios of forge against a local bare repository (plus a refused commit), and the project directory winning over a shell `cwd` outside the project.

| File | Role |
|---|---|
| `src/shell.rs` | Shell lexer; also splits task commands without a shell |
| `src/policy.rs` | The guard: commands, reads, edits |
| `src/agents.rs` | One adapter per agent, around the policy and the gate |
| `src/check.rs` | Snapshot (fingerprint), suppressions, integrity, parallel task runner |
| `src/githooks.rs` | `pre-commit`, `pre-push` |
| `src/install.rs` | `init`, `install`, `doctor` |
| `src/config.rs` | `aycod.toml` |
| `install.sh` | One-line installer: picks the target, checks the SHA-256 |
| `npm/` | The npm packages: `sh` launcher, platform table, `build.mjs` (from the release archives) |
| `.github/workflows/` | `ci.yml` (every push and PR), `release.yml` (on a `v*` tag), `pages.yml` (the website) |
| `site/` | The landing page, one static HTML file published to GitHub Pages |
