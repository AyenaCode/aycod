//! `aycod hook <agent> <event>`: one adapter per agent around the same policy and gate.
//!
//! Formats followed (docs read on 2026-10-28):
//! - Claude Code: code.claude.com/docs/en/hooks — deny with exit 2 + stderr; `ask` through
//!   `hookSpecificOutput.permissionDecision`; Stop blocked with exit 2.
//! - Codex CLI: learn.chatgpt.com/docs/hooks — same shape as Claude; `apply_patch` carries the
//!   patch in `tool_input.command`; Stop blocked with `{"decision":"block","reason":...}`.
//! - Copilot CLI: docs.github.com/en/copilot/reference/hooks-configuration — `toolName` +
//!   `toolArgs` (string or object), answer `{"permissionDecision":...}` on stdout; `agentStop`
//!   blocked with `{"decision":"block","reason":...}`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::check::{self, Options, Verdict};
use crate::config;
use crate::policy::{Decision, Policy};
use crate::shell;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Agent {
    Claude,
    Codex,
    Copilot,
}

impl Agent {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Agent::Claude),
            "codex" => Some(Agent::Codex),
            "copilot" => Some(Agent::Copilot),
            _ => None,
        }
    }
}

/// What the hook process prints and returns.
pub struct Reply {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Reply {
    fn ok() -> Self {
        Reply { code: 0, stdout: String::new(), stderr: String::new() }
    }
}

/// What the agent is about to do, whatever its JSON looks like.
enum Action {
    Command(String),
    Read(Vec<String>),
    Edit(Vec<String>),
    None,
}

pub fn run(agent: Agent, event: &str, input: &[u8]) -> Reply {
    let payload: Value = match serde_json::from_slice(input) {
        Ok(v) => v,
        Err(e) if event == "stop" => {
            // A Stop payload is informational: the gate still runs.
            let _ = e;
            Value::Null
        }
        Err(e) => return deny(agent, &format!("aycod could not read the hook input ({e}).")),
    };
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    // The agent's shell may sit outside the project (`cd /tmp`): the project dir it was started in
    // still decides which paths are protected.
    let loaded = config::load(&cwd).and_then(|p| match std::env::var_os("CLAUDE_PROJECT_DIR") {
        Some(dir) if !p.configured => config::load(Path::new(&dir)),
        _ => Ok(p),
    });
    let project = match loaded {
        Ok(p) => p,
        Err(e) => return deny(agent, &format!("{e} — fix aycod.toml (owner only).")),
    };
    match event {
        "pre-tool" => {
            let policy = Policy::new(&project.root, &project.config, std::env::var("HOME").ok());
            let decision = match action(&payload) {
                Action::Command(c) => policy.command(&c, &cwd),
                Action::Read(paths) => first_block(paths.iter().map(|p| Policy::read(p))),
                Action::Edit(paths) => first_block(paths.iter().map(|p| policy.edit(p, &cwd))),
                Action::None => Decision::Allow,
            };
            answer(agent, &decision)
        }
        "post-tool" => match (action(&payload), &project.config.format) {
            (Action::Edit(paths), Some(fmt)) => format(agent, &project.root, fmt, &paths),
            _ => Reply::ok(),
        },
        "stop" => {
            if std::env::var("AYCOD_GATE").is_ok_and(|v| v == "off") {
                return Reply::ok();
            }
            match check::check(&project, &Options { cache: true, integrity: true }) {
                Verdict::Cached | Verdict::Green(_) => Reply::ok(),
                Verdict::Red(report) => block_stop(agent, &report),
            }
        }
        #[cfg(debug_assertions)]
        "__panic" => panic!("test: forced panic in a hook"),
        other => deny(agent, &format!("unknown hook event `{other}`.")),
    }
}

fn first_block(decisions: impl Iterator<Item = Decision>) -> Decision {
    let mut ask = None;
    for d in decisions {
        match d {
            Decision::Deny(_) => return d,
            Decision::Ask(_) => ask = ask.or(Some(d)),
            Decision::Allow => {}
        }
    }
    ask.unwrap_or(Decision::Allow)
}

fn tool_args(payload: &Value) -> Value {
    let raw = payload.get("tool_input").or_else(|| payload.get("toolArgs")).cloned().unwrap_or(Value::Null);
    match raw {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        v => v,
    }
}

fn action(payload: &Value) -> Action {
    let tool = payload.get("tool_name").or_else(|| payload.get("toolName")).and_then(Value::as_str).unwrap_or("");
    let args = tool_args(payload);
    let command = match args.get("command") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(a)) => {
            Some(a.iter().filter_map(Value::as_str).map(shell_quote).collect::<Vec<_>>().join(" "))
        }
        _ => None,
    };
    if let Some(c) = &command
        && c.trim_start().starts_with("*** Begin Patch")
    {
        return Action::Edit(patch_paths(c));
    }
    let lower = tool.to_ascii_lowercase();
    let paths: Vec<String> = ["file_path", "path", "filePath", "notebook_path"]
        .iter()
        .filter_map(|k| args.get(*k).and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    if let Some(c) = command
        && (paths.is_empty() || matches!(lower.as_str(), "bash" | "shell" | "powershell" | "exec_command"))
    {
        return Action::Command(c);
    }
    if paths.is_empty() {
        return Action::None;
    }
    let reads = ["read", "view", "grep", "glob", "search", "list", "ls", "find", "fetch"];
    if reads.iter().any(|r| lower.contains(r)) { Action::Read(paths) } else { Action::Edit(paths) }
}

/// Paths named by an `apply_patch` envelope.
fn patch_paths(patch: &str) -> Vec<String> {
    let heads = ["*** Add File: ", "*** Update File: ", "*** Delete File: ", "*** Move to: "];
    patch.lines().filter_map(|l| heads.iter().find_map(|h| l.strip_prefix(h))).map(|p| p.trim().to_owned()).collect()
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%".contains(&b)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn deny(agent: Agent, why: &str) -> Reply {
    answer(agent, &Decision::Deny(why.to_owned()))
}

fn answer(agent: Agent, d: &Decision) -> Reply {
    match (agent, d) {
        (_, Decision::Allow) => Reply::ok(),
        (Agent::Claude | Agent::Codex, Decision::Deny(why)) => {
            Reply { code: 2, stdout: String::new(), stderr: format!("Rejected by aycod: {why}\n") }
        }
        (Agent::Claude, Decision::Ask(why)) => Reply {
            code: 0,
            stdout: json!({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": format!("aycod: {why}"),
            }})
            .to_string(),
            stderr: String::new(),
        },
        (Agent::Codex, Decision::Ask(why)) => Reply {
            code: 2,
            stdout: String::new(),
            stderr: format!("Rejected by aycod: {why} Ask the owner to make this change.\n"),
        },
        (Agent::Copilot, Decision::Deny(why) | Decision::Ask(why)) => {
            let kind = if matches!(d, Decision::Ask(_)) { "ask" } else { "deny" };
            Reply {
                code: 0,
                stdout: json!({"permissionDecision": kind, "permissionDecisionReason": format!("aycod: {why}")})
                    .to_string(),
                stderr: String::new(),
            }
        }
    }
}

fn block_stop(agent: Agent, report: &str) -> Reply {
    match agent {
        Agent::Claude => Reply { code: 2, stdout: String::new(), stderr: report.to_owned() },
        Agent::Codex | Agent::Copilot => {
            Reply { code: 0, stdout: json!({"decision": "block", "reason": report}).to_string(), stderr: String::new() }
        }
    }
}

/// Formats the edited files with the project formatter (file appended to the command).
fn format(agent: Agent, root: &Path, fmt: &str, paths: &[String]) -> Reply {
    let Ok((env, mut argv)) = shell::split_task(fmt) else {
        return Reply { code: 0, stdout: String::new(), stderr: format!("aycod: invalid format command `{fmt}`\n") };
    };
    for p in paths {
        let abs = root.join(p);
        if !abs.is_file() {
            continue;
        }
        argv.push(abs.to_string_lossy().into_owned());
        let out =
            Command::new(&argv[0]).args(&argv[1..]).envs(env.clone()).current_dir(root).stdin(Stdio::null()).output();
        argv.pop();
        let failed = match &out {
            Ok(o) if o.status.success() => None,
            Ok(o) => Some(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => Some(format!("cannot run `{}`: {e}", argv[0])),
        };
        if let Some(log) = failed {
            let tail: Vec<&str> = log.lines().rev().take(30).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            let msg = format!("Could not format {p}:\n{}\n", tail.join("\n"));
            let code = if agent == Agent::Copilot { 0 } else { 2 };
            return Reply { code, stdout: String::new(), stderr: msg };
        }
    }
    Reply::ok()
}
