//! Command line. The hook path (`aycod hook ...`) reads argv directly: no parser, no network.
#![forbid(unsafe_code)]

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::exit;

use aycod::agents::Agent;
use aycod::check::{self, Options, Verdict};
use aycod::install;
use aycod::{agents, config, githooks};

const USAGE: &str = "\
aycod — guardrails around any AI coding agent: the checks decide, not the agent.

Owner commands:
  aycod init [--stack sveltekit|rust|go|generic] [--force]   write aycod.toml
  aycod install [--agent claude,codex,copilot]              git hooks, agent hooks, AGENTS.md
  aycod doctor                                              what is really protected here

Anyone (agents included):
  aycod check [--no-cache]   fast tasks in parallel; skipped when nothing changed since green
  aycod ci                   check, then the ci tasks in order (no cache): run it in any CI

Called by agents and git (not by hand):
  aycod hook <claude|codex|copilot> <pre-tool|post-tool|stop>
  aycod git <pre-commit|pre-push>

Owner escape hatch: start the agent with AYCOD_GATE=off to skip the end-of-turn gate,
or `git commit/push --no-verify` by hand.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = match a.as_slice() {
        ["hook", agent, event] => hook(agent, event),
        ["check", rest @ ..] => run_check(!rest.contains(&"--no-cache")),
        ["ci"] => ci(),
        ["git", "pre-commit", ..] => githooks::pre_commit(),
        ["git", "pre-push", ..] => githooks::pre_push(),
        ["init", rest @ ..] => init(rest),
        ["install", rest @ ..] => install_cmd(rest),
        ["doctor"] => install::doctor(&cwd()),
        ["version" | "--version" | "-V"] => {
            println!("aycod {}", env!("CARGO_PKG_VERSION"));
            0
        }
        [] | ["help" | "--help" | "-h"] => {
            print!("{USAGE}");
            0
        }
        _ => {
            eprint!("aycod: unknown command `{}`\n\n{USAGE}", a.join(" "));
            2
        }
    };
    exit(code);
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Fail-closed: any panic or internal error blocks the action (exit 2), it never lets it through.
fn hook(agent: &str, event: &str) -> i32 {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("Rejected by aycod: internal error ({info}). The action is blocked; report it to the project owner.");
        exit(2);
    }));
    let Some(agent) = Agent::parse(agent) else {
        eprintln!("Rejected by aycod: unknown agent `{agent}`.");
        return 2;
    };
    let mut input = Vec::with_capacity(4096);
    if let Err(e) = io::stdin().lock().read_to_end(&mut input) {
        eprintln!("Rejected by aycod: cannot read the hook input ({e}).");
        return 2;
    }
    let reply = agents::run(agent, event, &input);
    let mut out = io::stdout().lock();
    if out.write_all(reply.stdout.as_bytes()).and_then(|()| out.flush()).is_err() {
        return 2;
    }
    let _ = io::stderr().lock().write_all(reply.stderr.as_bytes());
    reply.code
}

fn load() -> Option<config::Project> {
    match config::load(&cwd()) {
        Ok(p) if p.configured => Some(p),
        Ok(_) => {
            eprintln!("aycod: no aycod.toml in this directory or its parents (`aycod init`)");
            None
        }
        Err(e) => {
            eprintln!("aycod: {e}");
            None
        }
    }
}

fn run_check(cache: bool) -> i32 {
    let Some(project) = load() else { return 2 };
    match check::check(&project, &Options { cache, integrity: true }) {
        Verdict::Cached => {
            println!("✓ nothing changed since the last green run");
            0
        }
        Verdict::Green(outcomes) => {
            check::print_outcomes(&outcomes, &mut io::stdout());
            0
        }
        Verdict::Red(report) => {
            eprintln!("{report}");
            1
        }
    }
}

fn ci() -> i32 {
    let Some(project) = load() else { return 2 };
    let first = check::check(&project, &Options { cache: false, integrity: false });
    match first {
        Verdict::Red(report) => {
            eprintln!("{report}");
            return 1;
        }
        Verdict::Green(o) => check::print_outcomes(&o, &mut io::stdout()),
        Verdict::Cached => {}
    }
    let rest = check::run_tasks(&project, &project.config.ci, false);
    check::print_outcomes(&rest, &mut io::stdout());
    if let Some(bad) = rest.iter().find(|o| !o.ok) {
        // CI logs are read by people after the fact: print the whole log.
        eprintln!("\n✗ {} — `{}`\n{}", bad.name, bad.step, check::tail(&bad.log, usize::MAX));
        return 1;
    }
    0
}

fn flag<'a>(rest: &[&'a str], name: &str) -> Option<&'a str> {
    rest.iter()
        .position(|a| *a == name)
        .and_then(|i| rest.get(i + 1).copied())
        .or_else(|| rest.iter().find_map(|a| a.strip_prefix(name).and_then(|v| v.strip_prefix('='))))
}

fn init(rest: &[&str]) -> i32 {
    let stack = flag(rest, "--stack").unwrap_or("generic");
    match install::init(&cwd(), stack, rest.contains(&"--force")) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("aycod: {e}");
            1
        }
    }
}

fn install_cmd(rest: &[&str]) -> i32 {
    let mut kinds = Vec::new();
    if let Some(list) = flag(rest, "--agent") {
        for name in list.split(',') {
            let Some(k) = Agent::parse(name.trim()) else {
                eprintln!("aycod: unknown agent `{name}` (claude, codex, copilot)");
                return 2;
            };
            kinds.push(k);
        }
    }
    match install::install(&cwd(), &kinds) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("aycod: {e}");
            1
        }
    }
}
