//! End-to-end tests of the binary: agent hooks, the gate and its cache, integrity, git hooks.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_aycod");

fn tmp(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// PATH with the aycod under test first, so the git hooks find it.
fn path_env() -> String {
    let bin_dir = Path::new(BIN).parent().unwrap().to_string_lossy().into_owned();
    format!("{bin_dir}:{}", std::env::var("PATH").unwrap_or_default())
}

fn aycod(dir: &Path, args: &[&str], stdin: &str) -> Output {
    aycod_env(dir, args, stdin, &[])
}

fn aycod_env(dir: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("PATH", path_env())
        .env_remove("AYCOD_GATE")
        .env_remove("CLAUDE_PROJECT_DIR")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git").args(args).current_dir(dir).env("PATH", path_env()).output().unwrap()
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

const CONFIG: &str = r#"
protected = ["secret-config.yaml"]
guarded = ["vite.config.ts"]
check = ["test"]
[tasks]
test = "sh -c 'echo run >> ../runs; test ! -f BROKEN'"
"#;

/// A project with `aycod.toml`; `git` makes it a repository with one commit.
fn project(git_repo: bool) -> PathBuf {
    let base = tmp("proj");
    let dir = base.join("app");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("aycod.toml"), CONFIG).unwrap();
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    if git_repo {
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@example.com"]);
        git(&dir, &["config", "user.name", "test"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "init"]);
    }
    dir
}

fn runs(dir: &Path) -> usize {
    std::fs::read_to_string(dir.parent().unwrap().join("runs")).map_or(0, |s| s.lines().count())
}

// ---------- agent hooks ----------

#[test]
fn claude_denies_with_exit_2() {
    let dir = project(false);
    let payload =
        format!(r#"{{"tool_name":"Bash","tool_input":{{"command":"git push --force"}},"cwd":"{}"}}"#, dir.display());
    let o = aycod(&dir, &["hook", "claude", "pre-tool"], &payload);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
    assert!(text(&o).contains("Rejected by aycod: force push"));
}

#[test]
fn claude_allows_silently() {
    let dir = project(false);
    let payload = format!(r#"{{"tool_name":"Bash","tool_input":{{"command":"ls -la"}},"cwd":"{}"}}"#, dir.display());
    let o = aycod(&dir, &["hook", "claude", "pre-tool"], &payload);
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stdout.is_empty() && o.stderr.is_empty());
}

#[test]
fn claude_asks_for_guarded_and_denies_protected_edits() {
    let dir = project(false);
    let edit = |file: &str| {
        let p = format!(
            r#"{{"tool_name":"Edit","tool_input":{{"file_path":"{}/{file}"}},"cwd":"{}"}}"#,
            dir.display(),
            dir.display()
        );
        aycod(&dir, &["hook", "claude", "pre-tool"], &p)
    };
    let o = edit("vite.config.ts");
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains(r#""permissionDecision":"ask""#));
    assert_eq!(edit("secret-config.yaml").status.code(), Some(2));
    assert_eq!(edit("aycod.toml").status.code(), Some(2));
    assert_eq!(edit("src/app.ts").status.code(), Some(0));
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":"{}/.env"}},"cwd":"{}"}}"#,
        dir.display(),
        dir.display()
    );
    assert_eq!(aycod(&dir, &["hook", "claude", "pre-tool"], &read).status.code(), Some(2));
}

#[test]
fn project_dir_wins_when_the_shell_left_the_project() {
    let dir = project(false);
    let elsewhere = tmp("elsewhere");
    let payload = format!(
        r#"{{"tool_name":"Bash","tool_input":{{"command":"rm -rf {}/aycod.toml"}},"cwd":"{}"}}"#,
        dir.display(),
        elsewhere.display()
    );
    let project_dir = dir.to_string_lossy().into_owned();
    let o = aycod_env(&dir, &["hook", "claude", "pre-tool"], &payload, &[("CLAUDE_PROJECT_DIR", &project_dir)]);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
}

#[test]
fn codex_apply_patch_on_a_protected_file_is_denied() {
    let dir = project(false);
    let patch = "*** Begin Patch\\n*** Update File: aycod.toml\\n@@\\n-a\\n+b\\n*** End Patch";
    let payload =
        format!(r#"{{"tool_name":"apply_patch","tool_input":{{"command":"{patch}"}},"cwd":"{}"}}"#, dir.display());
    let o = aycod(&dir, &["hook", "codex", "pre-tool"], &payload);
    assert_eq!(o.status.code(), Some(2), "{}", text(&o));
}

#[test]
fn copilot_answers_in_json_with_string_tool_args() {
    let dir = project(false);
    let payload =
        format!(r#"{{"toolName":"bash","toolArgs":"{{\"command\":\"git push -f\"}}","cwd":"{}"}}"#, dir.display());
    let o = aycod(&dir, &["hook", "copilot", "pre-tool"], &payload);
    assert_eq!(o.status.code(), Some(0));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains(r#""permissionDecision":"deny""#), "{out}");
}

#[test]
fn fails_closed_on_bad_input_and_on_panic() {
    let dir = project(false);
    assert_eq!(aycod(&dir, &["hook", "claude", "pre-tool"], "not json").status.code(), Some(2));
    assert_eq!(aycod(&dir, &["hook", "claude", "nope"], "{}").status.code(), Some(2));
    let o = aycod(&dir, &["hook", "claude", "__panic"], "{}");
    assert_eq!(o.status.code(), Some(2), "a panic must block, not abort: {}", text(&o));
    assert!(text(&o).contains("internal error"));
}

// ---------- the gate ----------

#[test]
fn stop_gate_caches_green_and_blocks_red() {
    let dir = project(false);
    let stop = |agent: &str| aycod(&dir, &["hook", agent, "stop"], &format!(r#"{{"cwd":"{}"}}"#, dir.display()));
    assert_eq!(stop("claude").status.code(), Some(0));
    assert_eq!(runs(&dir), 1);
    assert_eq!(stop("claude").status.code(), Some(0));
    assert_eq!(runs(&dir), 1, "nothing changed: the tasks must not run again");

    std::fs::write(dir.join("BROKEN"), "").unwrap();
    let o = stop("claude");
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o).contains("you cannot finish"), "{}", text(&o));
    let o = stop("codex");
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains(r#""decision":"block""#));

    let off = aycod_env(&dir, &["hook", "claude", "stop"], "{}", &[("AYCOD_GATE", "off")]);
    assert_eq!(off.status.code(), Some(0), "the owner's escape hatch");
    std::fs::remove_file(dir.join("BROKEN")).unwrap();
    assert_eq!(aycod(&dir, &["check"], "").status.code(), Some(0));
}

#[test]
fn unjustified_suppressions_fail_the_check() {
    let dir = project(false);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/a.test.ts"), "it.skip('flaky', () => {});\n").unwrap();
    let o = aycod(&dir, &["check"], "");
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains("src/a.test.ts:1"), "{}", text(&o));
    std::fs::write(dir.join("src/a.test.ts"), "it.skip('flaky', () => {}); // AYCOD-OK: upstream bug #12\n").unwrap();
    assert_eq!(aycod(&dir, &["check"], "").status.code(), Some(0));
}

#[test]
fn install_records_integrity_and_check_detects_tampering() {
    let dir = project(true);
    let o = aycod(&dir, &["install", "--agent", "claude,codex,copilot"], "");
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    for f in [
        ".githooks/pre-commit",
        ".githooks/pre-push",
        ".claude/settings.json",
        ".codex/hooks.json",
        ".github/hooks/aycod.json",
        "AGENTS.md",
    ] {
        assert!(dir.join(f).is_file(), "{f} missing");
    }
    assert_eq!(String::from_utf8_lossy(&git(&dir, &["config", "core.hooksPath"]).stdout).trim(), ".githooks");
    assert_eq!(aycod(&dir, &["check"], "").status.code(), Some(0));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let hook = dir.join(".githooks/pre-push");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(text(&aycod(&dir, &["check"], "")).contains("pre-push is not executable"));
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(dir.join(".githooks/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();
    let o = aycod(&dir, &["check"], "");
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o).contains(".githooks/pre-commit changed"), "{}", text(&o));
    git(&dir, &["config", "core.hooksPath", "/dev/null"]);
    assert!(text(&aycod(&dir, &["check"], "")).contains("core.hooksPath"));

    // Only the owner's `aycod install` accepts the new state.
    assert_eq!(aycod(&dir, &["install", "--agent", "claude"], "").status.code(), Some(0));
    assert_eq!(aycod(&dir, &["check"], "").status.code(), Some(0));
    let doctor = aycod(&dir, &["doctor"], "");
    assert!(text(&doctor).contains("layer 3"), "{}", text(&doctor));
}

#[test]
fn install_keeps_foreign_claude_settings() {
    let dir = project(true);
    std::fs::create_dir_all(dir.join(".claude")).unwrap();
    std::fs::write(
        dir.join(".claude/settings.json"),
        r#"{"model":"x","hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"mine.sh"},{"type":"command","command":"${CLAUDE_PROJECT_DIR}/.claude/hooks/guard-bash.sh"}]}]}}"#,
    )
    .unwrap();
    assert_eq!(aycod(&dir, &["install", "--agent", "claude"], "").status.code(), Some(0));
    assert_eq!(aycod(&dir, &["install", "--agent", "claude"], "").status.code(), Some(0));
    let s = std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap();
    assert!(s.contains("mine.sh") && s.contains(r#""model": "x""#));
    assert!(!s.contains("guard-bash.sh"), "the forge hook is replaced");
    assert_eq!(s.matches("aycod hook claude pre-tool").count(), 1, "idempotent");
}

#[test]
fn a_project_only_npm_install_is_reported() {
    let dir = project(true);
    let bin_dir = dir.join("node_modules/.bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::copy(BIN, bin_dir.join("aycod")).unwrap();
    // As under `npx aycod install`: node_modules/.bin is on PATH, but git and the agents never see it.
    let path = format!("{}:/usr/bin:/bin", bin_dir.display());
    let o = aycod_env(&dir, &["install", "--agent", "claude"], "", &[("PATH", &path)]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o));
    assert!(text(&o).contains("only installed in this project"), "{}", text(&o));
    assert!(text(&o).contains("npm install -g @ay-code/cli"), "{}", text(&o));
    let doctor = aycod_env(&dir, &["doctor"], "", &[("PATH", &path)]);
    assert_eq!(doctor.status.code(), Some(1));
    assert!(text(&doctor).contains("✗ aycod is only installed in this project"), "{}", text(&doctor));
    // A global aycod (here: the one under test) satisfies both.
    assert!(!text(&aycod(&dir, &["doctor"], "")).contains("only installed in this project"));
}

// ---------- git hooks (port of forge's 19 pre-push scenarios) ----------

#[test]
fn pre_push_against_a_local_bare_remote() {
    let dir = project(true);
    let base = dir.parent().unwrap().to_path_buf();
    git(&base, &["init", "-q", "--bare", "remote.git"]);
    git(&dir, &["remote", "add", "origin", base.join("remote.git").to_str().unwrap()]);
    git(&dir, &["push", "-q", "origin", "main"]);
    assert_eq!(aycod(&dir, &["install", "--agent", "claude"], "").status.code(), Some(0));
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "aycod", "--no-verify"]);
    git(&dir, &["push", "-q", "--no-verify", "origin", "main"]);
    assert_eq!(aycod(&dir, &["install", "--agent", "claude"], "").status.code(), Some(0));

    let mut failures = Vec::new();
    let mut expect = |name: &str, ok: bool, args: &[&str]| {
        let o = git(&dir, args);
        if o.status.success() != ok {
            failures.push(format!("{name}: expected {}, got:\n{}", if ok { "ok" } else { "reject" }, text(&o)));
        }
    };
    let sh = |cmd: &str| {
        let o = Command::new("sh").args(["-c", cmd]).current_dir(&dir).env("PATH", path_env()).output().unwrap();
        assert!(o.status.success(), "{cmd}: {}", text(&o));
    };

    sh("git switch -qc feat/a && echo a > a.txt && git add a.txt && git commit -qm a");
    expect("push a feature branch whose checks pass", true, &["push", "-q", "origin", "feat/a"]);
    let n = runs(&dir);
    expect("push again, nothing changed", true, &["push", "-q", "origin", "feat/a"]);
    assert_eq!(runs(&dir), n, "stamp reused: checks not rerun");
    sh("echo b > b.txt && git add b.txt && git commit -qm b");
    expect("push a new commit: checks run again", true, &["push", "-q", "origin", "feat/a"]);
    sh("touch BROKEN && git add BROKEN && git commit -q --no-verify -m broken");
    expect("push a commit whose checks fail", false, &["push", "-q", "origin", "feat/a"]);
    sh("git reset -q --hard HEAD~1 && git switch -q main && echo m > m.txt && git add m.txt && git commit -qm m");
    expect("push directly to main", false, &["push", "-q", "origin", "main"]);
    expect("push HEAD to main with a refspec", false, &["push", "-q", "origin", "HEAD:main"]);
    sh("git switch -q feat/a");
    expect("push feat/a to main with a refspec", false, &["push", "-q", "origin", "feat/a:main"]);
    expect("delete the protected branch main", false, &["push", "-q", "origin", ":main"]);
    sh("git commit -q --amend --no-verify -m 'b rewritten'");
    expect("force push a rewritten branch", false, &["push", "-q", "--force", "origin", "feat/a"]);
    expect("force push with lease", false, &["push", "-q", "--force-with-lease", "origin", "feat/a"]);
    sh(
        "git reset -q --hard origin/feat/a && echo d > d.txt && git add d.txt && git commit -qm d && echo dirty >> README.md",
    );
    expect("push with uncommitted changes", false, &["push", "-q", "origin", "feat/a"]);
    sh("git checkout -q -- README.md && touch BROKEN");
    expect("untracked file present", false, &["push", "-q", "origin", "feat/a"]);
    sh("rm BROKEN");
    expect("same commit, clean tree", true, &["push", "-q", "origin", "feat/a"]);
    sh("git branch feat/other HEAD~1");
    expect("push a branch that is not checked out", false, &["push", "-q", "origin", "feat/other"]);
    sh("git tag v1");
    expect("push a tag", true, &["push", "-q", "origin", "v1"]);
    expect("delete an unprotected branch", true, &["push", "-q", "origin", ":feat/a"]);
    sh("git switch -qc feat/c && echo c > c.txt && git add c.txt && git commit -qm c");
    expect("push a new branch", true, &["push", "-q", "origin", "HEAD"]);
    expect("owner bypass with --no-verify", true, &["push", "-q", "--no-verify", "origin", "HEAD:main"]);
    sh("touch BROKEN");
    expect("commit with a failing check", false, &["commit", "-qm", "x", "--allow-empty"]);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
