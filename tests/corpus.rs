//! Runs `tests/corpus.txt` against the policy: every line must get its expected verdict.

use std::path::Path;

use aycod::config::Config;
use aycod::policy::{Decision, Policy};

/// A line that does not start with `allow\t`/`deny\t` continues the previous entry (heredocs).
fn entries(text: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for line in text.lines() {
        if let Some(cmd) = line.strip_prefix("deny\t") {
            out.push((false, cmd.to_owned()));
        } else if let Some(cmd) = line.strip_prefix("allow\t") {
            out.push((true, cmd.to_owned()));
        } else if !line.starts_with('#')
            && !line.is_empty()
            && let Some(last) = out.last_mut()
        {
            last.1.push('\n');
            last.1.push_str(line);
        }
    }
    out
}

#[test]
fn corpus() {
    // Not under /tmp: deleting under /tmp is allowed, and the corpus checks deletions outside the root.
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("corpus-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let cfg = Config::parse(
        "protected = [\"pnpm-workspace.yaml\"]\nguarded = [\"vite.config.ts\", \".github/workflows/**\"]\n",
    )
    .unwrap();
    let policy = Policy::new(&root, &cfg, Some("/home/dev".into()));
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus.txt")).unwrap();
    let all = entries(&text);
    let mut failures = Vec::new();
    for (want_allow, cmd) in &all {
        let got = policy.command(cmd, &root);
        if (got == Decision::Allow) != *want_allow {
            failures.push(format!("expected {}: {cmd:?} → {got:?}", if *want_allow { "allow" } else { "deny" }));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{} of {} corpus entries failed:\n{}", failures.len(), all.len(), failures.join("\n"));
}
