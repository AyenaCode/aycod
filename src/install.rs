//! `aycod init`, `aycod install` and `aycod doctor`: owner commands, never available to agents.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

use crate::agents::Agent;
use crate::check;
use crate::config::{self, Project};

// ---------- init ----------

const HEADER: &str = "\
# aycod.toml: the project's guardrails. No agent can edit this file; `aycod install` applies it.

# Branches nobody pushes to directly (agent guard and pre-push hook).
protected_branches = [\"main\", \"master\"]
";

const SVELTEKIT: &str = r#"stack = "sveltekit"

# Never editable by an agent (aycod.toml, .githooks, .git and the agent configs always are).
protected = ["pnpm-workspace.yaml"]

# Define what the checks check: an agent needs the owner's approval to edit them.
guarded = [
  "vite.config.ts", "eslint.config.js", "tsconfig.json", "playwright.config.ts", "renovate.json",
  ".github/workflows/**", ".gitlab-ci.yml", ".gitlab/**", ".claude/agents/**", ".claude/skills/**",
]

# Run on each file an agent edits (the file is appended).
format = "pnpm exec prettier --write --ignore-unknown --log-level warn"

# Run once, in order, before the check tasks.
prepare = ["pnpm exec svelte-kit sync"]

# Fast tasks, in parallel: agent Stop hook, pre-commit, pre-push, start of `aycod ci`.
check = ["fmt-check", "lint", "typecheck", "test"]

# Slower tasks, in order, after `check`: `aycod ci` only.
ci = ["coverage", "audit", "e2e"]

# Extra pre-commit tasks, never cached.
pre_commit = ["secrets"]

# Each task: a command, a list of commands run in order, or { run = ..., optional = true }.
# No shell: wrap pipes or redirections in `sh -c '...'`. `KEY=value` prefixes set the environment.
[tasks]
fmt-check = "pnpm exec prettier --check --log-level warn ."
lint = "pnpm exec eslint --max-warnings 0 ."
typecheck = "pnpm exec svelte-check --tsconfig ./tsconfig.json --fail-on-warnings"
test = "pnpm exec vitest run"
coverage = "pnpm exec vitest run --coverage"
audit = ["pnpm audit --audit-level high", "pnpm audit signatures"]
e2e = ["pnpm exec vite build", "pnpm exec playwright test"]
secrets = { run = "gitleaks git --pre-commit --staged --redact --no-banner", optional = true }
"#;

const RUST: &str = r#"stack = "rust"

protected = ["deny.toml", "rust-toolchain.toml"]
guarded = ["Cargo.toml", "clippy.toml", ".github/workflows/**", ".gitlab-ci.yml"]

format = "rustfmt --edition 2024"

check = ["fmt-check", "clippy", "test"]
ci = ["audit"]
pre_commit = ["secrets"]

[tasks]
fmt-check = "cargo fmt --all --check"
clippy = "cargo clippy --all-targets --locked -- -D warnings"
test = "cargo test --locked"
audit = { run = "cargo deny check", optional = true }
secrets = { run = "gitleaks git --pre-commit --staged --redact --no-banner", optional = true }
"#;

const GO: &str = r#"stack = "go"

guarded = [".golangci.yml", ".github/workflows/**", ".gitlab-ci.yml"]

format = "gofmt -w"

check = ["fmt-check", "vet", "test"]
ci = ["lint", "vuln"]
pre_commit = ["secrets"]

[tasks]
fmt-check = "sh -c 'test -z \"$(gofmt -l .)\"'"
vet = "go vet ./..."
test = "go test ./..."
lint = { run = "golangci-lint run", optional = true }
vuln = { run = "govulncheck ./...", optional = true }
secrets = { run = "gitleaks git --pre-commit --staged --redact --no-banner", optional = true }
"#;

const GENERIC: &str = r#"stack = "generic"

protected = []
guarded = [".github/workflows/**", ".gitlab-ci.yml"]

# format = "prettier --write"
check = ["test"]
ci = []
pre_commit = []

[tasks]
test = "sh -c 'echo \"define your tasks in aycod.toml\" >&2; exit 1'"
"#;

pub const STACKS: &[&str] = &["sveltekit", "rust", "go", "generic"];

pub fn init(dir: &Path, stack: &str, force: bool) -> Result<(), String> {
    let body = match stack {
        "sveltekit" => SVELTEKIT,
        "rust" => RUST,
        "go" => GO,
        "generic" => GENERIC,
        other => return Err(format!("unknown stack `{other}` (known: {})", STACKS.join(", "))),
    };
    let path = dir.join(config::FILE);
    if path.exists() && !force {
        return Err(format!("{} already exists (use --force to replace it)", path.display()));
    }
    let text = format!("{HEADER}{body}");
    config::Config::parse(&text)?;
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}: review the tasks, then run `aycod install`", path.display());
    Ok(())
}

// ---------- install ----------

const PRE_COMMIT: &str =
    "#!/bin/sh\n# Managed by aycod: runs the project checks before every commit.\nexec aycod git pre-commit \"$@\"\n";
const PRE_PUSH: &str = "#!/bin/sh\n# Managed by aycod: no force push, no direct push to a protected branch, checks must pass.\nexec aycod git pre-push \"$@\"\n";

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Copilot];

    fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex CLI",
            Agent::Copilot => "Copilot CLI",
        }
    }

    pub fn config_file(self) -> &'static str {
        match self {
            Agent::Claude => ".claude/settings.json",
            Agent::Codex => ".codex/hooks.json",
            Agent::Copilot => ".github/hooks/aycod.json",
        }
    }

    fn detected(self, root: &Path) -> bool {
        let (dir, bin) = match self {
            Agent::Claude => (".claude", "claude"),
            Agent::Codex => (".codex", "codex"),
            Agent::Copilot => (".github/hooks", "copilot"),
        };
        root.join(dir).exists() || on_path(bin).is_some()
    }
}

pub fn on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

fn write(root: &Path, rel: &str, text: &str) -> Result<(), String> {
    let path = root.join(rel);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> io::Result<()> {
    Ok(())
}

fn read_json(path: &Path) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => serde_json::from_str(&t).map_err(|e| format!("{}: {e}", path.display())),
        _ => Ok(json!({})),
    }
}

/// Our hook entries, or the forge scripts they replace.
fn is_ours(hook: &Value) -> bool {
    let cmd = hook.get("command").or_else(|| hook.get("bash")).and_then(Value::as_str).unwrap_or("");
    cmd.starts_with("aycod hook ")
        || [".claude/hooks/guard-bash.sh", ".claude/hooks/gate.sh", ".claude/hooks/format.sh"]
            .iter()
            .any(|f| cmd.contains(f))
}

/// Replaces aycod's group under `hooks.<event>` and keeps everyone else's hooks.
fn set_hook(settings: &mut Value, event: &str, group: Value) {
    let Some(root) = settings.as_object_mut() else { return };
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return };
    let list = hooks.entry(event).or_insert_with(|| json!([]));
    let Some(groups) = list.as_array_mut() else { return };
    for g in groups.iter_mut() {
        if let Some(inner) = g.get_mut("hooks").and_then(Value::as_array_mut) {
            inner.retain(|h| !is_ours(h));
        }
    }
    groups.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|h| !h.is_empty()));
    groups.push(group);
}

fn command_hook(cmd: &str, timeout: u64, block_on_failure: bool) -> Value {
    let mut h = Map::new();
    h.insert("type".into(), json!("command"));
    h.insert("command".into(), json!(cmd));
    h.insert("timeout".into(), json!(timeout));
    if block_on_failure {
        h.insert("onFailure".into(), json!("block"));
    }
    Value::Object(h)
}

const CLAUDE_DENY: &[&str] = &[
    "Read(**/.env)",
    "Read(**/.env.*)",
    "Read(./secrets/**)",
    "Read(**/*.pem)",
    "Read(**/*.key)",
    "Read(**/*.p12)",
    "Read(**/*.p8)",
    "Read(**/*.jks)",
    "Read(**/*.keystore)",
    "Read(~/.ssh/**)",
    "Read(~/.aws/**)",
    "Edit(./aycod.toml)",
    "Edit(./.aycod/**)",
    "Edit(./.githooks/**)",
    "Edit(./.claude/settings.json)",
    "Edit(./.claude/settings.local.json)",
    "Edit(./.codex/**)",
    "Edit(./.github/hooks/**)",
];

fn claude(root: &Path) -> Result<(), String> {
    let path = root.join(Agent::Claude.config_file());
    let mut s = read_json(&path)?;
    let group = |matcher: &str, hook: Value| json!({"matcher": matcher, "hooks": [hook]});
    set_hook(&mut s, "PreToolUse", group("*", command_hook("aycod hook claude pre-tool", 30, true)));
    set_hook(
        &mut s,
        "PostToolUse",
        group("Edit|Write|MultiEdit", command_hook("aycod hook claude post-tool", 120, false)),
    );
    set_hook(&mut s, "Stop", json!({"hooks": [command_hook("aycod hook claude stop", 1800, false)]}));
    if let Some(obj) = s.as_object_mut() {
        let perms = obj.entry("permissions").or_insert_with(|| json!({}));
        if let Some(perms) = perms.as_object_mut() {
            let deny = perms.entry("deny").or_insert_with(|| json!([]));
            if let Some(list) = deny.as_array_mut() {
                for rule in CLAUDE_DENY {
                    if !list.iter().any(|v| v.as_str() == Some(rule)) {
                        list.push(json!(rule));
                    }
                }
            }
        }
    }
    write(root, Agent::Claude.config_file(), &(pretty(&s) + "\n"))
}

fn codex(root: &Path) -> Result<(), String> {
    let path = root.join(Agent::Codex.config_file());
    let mut s = read_json(&path)?;
    let group = |matcher: &str, hook: Value| json!({"matcher": matcher, "hooks": [hook]});
    set_hook(&mut s, "PreToolUse", group("*", command_hook("aycod hook codex pre-tool", 30, false)));
    set_hook(
        &mut s,
        "PostToolUse",
        group("apply_patch|Edit|Write", command_hook("aycod hook codex post-tool", 120, false)),
    );
    set_hook(&mut s, "Stop", json!({"hooks": [command_hook("aycod hook codex stop", 1800, false)]}));
    write(root, Agent::Codex.config_file(), &(pretty(&s) + "\n"))
}

fn copilot(root: &Path) -> Result<(), String> {
    let hook = |cmd: &str, t: u64| json!([{"type": "command", "bash": cmd, "timeoutSec": t}]);
    let s = json!({"version": 1, "hooks": {
        "preToolUse": hook("aycod hook copilot pre-tool", 30),
        "postToolUse": hook("aycod hook copilot post-tool", 120),
        "agentStop": hook("aycod hook copilot stop", 1800),
    }});
    write(root, Agent::Copilot.config_file(), &(pretty(&s) + "\n"))
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

const BEGIN: &str = "<!-- aycod:begin (managed by `aycod install`: do not edit) -->";
const END: &str = "<!-- aycod:end -->";

fn agents_block(project: &Project, kinds: &[Agent], git: bool) -> String {
    let c = &project.config;
    let mut s = format!("{BEGIN}\n## Guardrails (aycod)\n\n");
    let tasks = if c.check.is_empty() { "the project tasks".to_owned() } else { c.check.join(", ") };
    let _ = writeln!(
        s,
        "- You cannot finish while `aycod check` fails ({tasks}). Fix the root cause: never disable, skip or weaken a test, rule or threshold. A suppression needs `AYCOD-OK: <reason>`, written by the owner."
    );
    let mut never: Vec<&str> =
        vec!["aycod.toml", ".githooks/", "the agent configs (.claude/settings.json, .codex/, .github/hooks/)"];
    never.extend(c.protected.iter().map(String::as_str));
    let _ = writeln!(s, "- Never edit: {}.", never.join(", "));
    if !c.guarded.is_empty() {
        let _ = writeln!(s, "- Ask the owner before editing: {}.", c.guarded.join(", "));
    }
    let _ = writeln!(
        s,
        "- No force push, no direct push to {}, no `--no-verify`, no `git -c`, no reading secrets (.env, keys).",
        c.protected_branches.join(" or ")
    );
    let _ = writeln!(
        s,
        "- Run commands directly: `eval`, `bash -c \"$X\"` and code piped into a shell or an interpreter are refused.\n"
    );
    s.push_str("What is enforced on this machine:\n\n| Agent | Commands and edits checked | Cannot finish while checks fail |\n|---|---|---|\n");
    for k in kinds {
        let stop = match k {
            Agent::Claude | Agent::Codex => "yes",
            Agent::Copilot => "yes (Copilot stops retrying after 8 blocks; the git hooks still hold)",
        };
        let _ = writeln!(s, "| {} | yes | {stop} |", k.label());
    }
    let git_line = if git {
        "| Any other agent, or a human | no | commits and pushes: the git hooks run `aycod check` |"
    } else {
        "| Any other agent, or a human | no | no (not a git repository) |"
    };
    let _ = writeln!(s, "{git_line}\n{END}");
    s
}

fn upsert_block(root: &Path, file: &str, block: &str) -> Result<(), String> {
    let path = root.join(file);
    let old = fs::read_to_string(&path).unwrap_or_default();
    let new = match (old.find(BEGIN), old.find(END)) {
        (Some(a), Some(b)) if b > a => format!("{}{}{}", &old[..a], block.trim_end(), &old[b + END.len()..]),
        _ if old.is_empty() => block.to_owned(),
        _ => format!("{}\n\n{block}", old.trim_end()),
    };
    fs::write(&path, new).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn install(start: &Path, only: &[Agent]) -> Result<(), String> {
    let project = config::load(start)?;
    if !project.configured {
        return Err("no aycod.toml here: run `aycod init --stack <stack>` first".into());
    }
    let root = &project.root;
    let git = root.join(".git").exists();
    if git {
        for (rel, text) in [(".githooks/pre-commit", PRE_COMMIT), (".githooks/pre-push", PRE_PUSH)] {
            write(root, rel, text)?;
            make_executable(&root.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
        }
        let ok = Command::new("git")
            .args(["config", "core.hooksPath", ".githooks"])
            .current_dir(root)
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return Err("git config core.hooksPath .githooks failed".into());
        }
        println!("✓ git hooks: pre-commit and pre-push (core.hooksPath = .githooks)");
    } else {
        println!(
            "✗ not a git repository: commits and pushes are not guarded (run `git init`, then `aycod install` again)"
        );
    }
    let kinds: Vec<Agent> =
        if only.is_empty() { Agent::ALL.into_iter().filter(|k| k.detected(root)).collect() } else { only.to_vec() };
    for k in &kinds {
        match k {
            Agent::Claude => claude(root)?,
            Agent::Codex => codex(root)?,
            Agent::Copilot => copilot(root)?,
        }
        println!("✓ {}: {}", k.label(), k.config_file());
    }
    if kinds.is_empty() {
        println!("✗ no agent detected: pass --agent claude,codex,copilot");
    }
    upsert_block(root, "AGENTS.md", &agents_block(&project, &kinds, git))?;
    let claude_md = root.join("CLAUDE.md");
    let text = fs::read_to_string(&claude_md).unwrap_or_default();
    if kinds.contains(&Agent::Claude) && !text.contains("@AGENTS.md") {
        let sep = if text.is_empty() || text.ends_with('\n') { "" } else { "\n" };
        fs::write(&claude_md, format!("{text}{sep}@AGENTS.md\n")).map_err(|e| format!("CLAUDE.md: {e}"))?;
    }
    println!("✓ AGENTS.md: rules and what is enforced");
    if on_path("aycod").is_none() {
        println!("! aycod is not on PATH: the hooks call `aycod`, so they will fail (closed) until it is");
    }
    if !project.config.tasks.is_empty() {
        let missing: Vec<String> = project
            .config
            .tasks
            .values()
            .flat_map(|t| t.steps().into_iter().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|s| crate::shell::split_task(&s).ok().map(|(_, a)| a[0].clone()))
            .filter(|bin| !bin.contains('/') && on_path(bin).is_none())
            .collect();
        if !missing.is_empty() {
            let mut m = missing;
            m.sort();
            m.dedup();
            println!("! not installed: {} (`aycod doctor` for details)", m.join(", "));
        }
    }
    let snap = check::snapshot(root, &project.config);
    check::record_integrity(root, &snap).map_err(|e| format!("integrity record: {e}"))?;
    println!("✓ integrity recorded: `aycod check` fails if these files change without `aycod install`");
    Ok(())
}

// ---------- doctor ----------

pub fn doctor(start: &Path) -> i32 {
    let project = match config::load(start) {
        Ok(p) => p,
        Err(e) => {
            println!("✗ {e}");
            return 1;
        }
    };
    let root = &project.root;
    let mut bad = 0;
    let mut line = |good: bool, text: String| {
        if !good {
            bad += 1;
        }
        println!("{} {text}", if good { "✓" } else { "✗" });
    };
    println!("aycod {} — {}", env!("CARGO_PKG_VERSION"), root.display());
    line(
        project.configured,
        format!("config: {}", if project.configured { "aycod.toml" } else { "no aycod.toml (`aycod init`)" }),
    );
    line(on_path("aycod").is_some(), "aycod on PATH (the hooks call it)".into());
    let agents_md = fs::read_to_string(root.join("AGENTS.md")).unwrap_or_default();
    line(agents_md.contains(BEGIN), "layer 1 — instructions: AGENTS.md block".into());
    for k in Agent::ALL {
        let text = fs::read_to_string(root.join(k.config_file())).unwrap_or_default();
        let installed = text.contains("aycod hook");
        if installed || k.detected(root) {
            line(installed, format!("layer 2 — {} hooks ({})", k.label(), k.config_file()));
        }
    }
    let git = root.join(".git").exists();
    if git {
        let hooks = check::hooks_path(root);
        let files = root.join(".githooks/pre-commit").is_file() && root.join(".githooks/pre-push").is_file();
        line(hooks == ".githooks" && files, format!("layer 3 — git hooks (core.hooksPath = `{hooks}`)"));
    } else {
        line(false, "layer 3 — not a git repository: commits and pushes are not guarded".into());
    }
    let ci = [".github/workflows", ".gitlab-ci.yml", ".forgejo/workflows", ".woodpecker", "bitbucket-pipelines.yml"]
        .iter()
        .any(|p| mentions(&root.join(p), "aycod ci"));
    println!(
        "{} layer 4 — CI running `aycod ci`: {} (optional)",
        if ci { "✓" } else { "–" },
        if ci { "found" } else { "none found" }
    );
    println!("– layer 4 — server-side branch protection: not checked (optional, depends on the host and plan)");
    if project.configured {
        let snap = check::snapshot(root, &project.config);
        match check::integrity_problems(root, &snap) {
            None => line(false, "integrity: never recorded (`aycod install`)".into()),
            Some(p) if p.is_empty() => line(true, "integrity: guardrail files unchanged since `aycod install`".into()),
            Some(p) => line(false, format!("integrity: {}", p.join("; "))),
        }
        let hits = check::suppressions(root, &project.config, &snap);
        line(hits.is_empty(), format!("suppressions: {} unjustified", hits.len()));
        let mut bins: Vec<(String, bool)> = Vec::new();
        for t in project.config.tasks.values() {
            for step in t.steps() {
                if let Ok((_, argv)) = crate::shell::split_task(step)
                    && !bins.iter().any(|(b, _)| *b == argv[0])
                {
                    bins.push((argv[0].clone(), t.optional()));
                }
            }
        }
        for (bin, optional) in bins {
            let found = bin.contains('/') || on_path(&bin).is_some();
            if found || !optional {
                line(found, format!("tool: {bin}"));
            } else {
                println!("– tool: {bin} (optional, not installed: its task is skipped)");
            }
        }
    }
    i32::from(bad > 0)
}

fn mentions(path: &Path, needle: &str) -> bool {
    if path.is_file() {
        return fs::read_to_string(path).is_ok_and(|t| t.contains(needle));
    }
    fs::read_dir(path).is_ok_and(|d| d.flatten().any(|e| mentions(&e.path(), needle)))
}
