//! `aycod git pre-commit|pre-push`: the layer that holds for every agent that commits with git,
//! and for any remote (or none). The owner can still bypass it with `--no-verify`.

use std::io::{self, BufRead};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::check::{self, Options, Verdict};
use crate::config::{self, Project};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(root).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn ok(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn load() -> Result<Project, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let top = git(&cwd, &["rev-parse", "--show-toplevel"]).ok_or("not inside a git work tree")?;
    config::load(Path::new(&top))
}

/// Runs the gate (cached) and reports to stderr. Returns the process exit code.
fn gate(project: &Project) -> i32 {
    match check::check(project, &Options { cache: true, integrity: true }) {
        Verdict::Cached => 0,
        Verdict::Green(outcomes) => {
            check::print_outcomes(&outcomes, &mut io::stderr());
            0
        }
        Verdict::Red(report) => {
            eprintln!("{report}");
            1
        }
    }
}

pub fn pre_commit() -> i32 {
    let project = match load() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aycod: {e}");
            return 1;
        }
    };
    let code = gate(&project);
    if code != 0 {
        return code;
    }
    let extra = check::run_tasks(&project, &project.config.pre_commit, false);
    check::print_outcomes(&extra, &mut io::stderr());
    if extra.iter().all(|o| o.ok) {
        0
    } else {
        eprintln!("{}", check::report(&extra));
        1
    }
}

fn reject(why: &str) -> i32 {
    eprintln!("Push rejected by aycod: {why}");
    1
}

/// Reads `<local ref> <local sha> <remote ref> <remote sha>` lines from git on stdin.
pub fn pre_push() -> i32 {
    let project = match load() {
        Ok(p) => p,
        Err(e) => return reject(&e),
    };
    let root = project.root.clone();
    let head = git(&root, &["rev-parse", "HEAD"]).unwrap_or_default();
    let mut run_checks = false;
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let f: Vec<&str> = line.split_whitespace().collect();
        let [local_ref, local_sha, remote_ref, remote_sha] = f[..] else { continue };
        let zero = "0".repeat(local_sha.len());
        if let Some(branch) = remote_ref.strip_prefix("refs/heads/")
            && project.config.protected_branches.iter().any(|b| b == branch)
        {
            return reject(&if local_sha == zero {
                format!("deleting the protected branch '{branch}' is forbidden.")
            } else {
                format!(
                    "pushing directly to the protected branch '{branch}' is forbidden: push a branch and open a pull request."
                )
            });
        }
        if local_sha == zero {
            continue; // deleting an unprotected branch or a tag
        }
        if remote_sha != zero {
            if !ok(&root, &["cat-file", "-e", &format!("{remote_sha}^{{commit}}")]) {
                return reject(&format!(
                    "the remote '{remote_ref}' has commits you do not have: fetch and merge them first."
                ));
            }
            if !ok(&root, &["merge-base", "--is-ancestor", remote_sha, local_sha]) {
                return reject(&format!(
                    "force push to '{remote_ref}' is forbidden: it would rewrite published history."
                ));
            }
        }
        if local_ref.starts_with("refs/tags/") {
            continue; // tags point at commits already checked
        }
        if local_sha != head {
            return reject(&format!(
                "'{local_ref}' is not the checked-out commit: switch to it before pushing so the checks run on what is pushed."
            ));
        }
        run_checks = true;
    }
    if !run_checks {
        return 0;
    }
    // Untracked files count too: they could make the checks pass without being pushed.
    if git(&root, &["status", "--porcelain"]).is_none_or(|s| !s.is_empty()) {
        return reject(
            "uncommitted or untracked changes: commit or stash them (git stash -u) so the checks run on exactly what is pushed.",
        );
    }
    gate(&project)
}
