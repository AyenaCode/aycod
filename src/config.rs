//! `aycod.toml`: the project's stack, protected paths and check tasks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const FILE: &str = "aycod.toml";

/// Paths no agent may modify, whatever the project says.
pub const BUILTIN_PROTECTED: &[&str] = &[
    "aycod.toml",
    ".aycod",
    ".githooks",
    ".git",
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".codex",
    ".github/hooks",
    ".cursor/hooks.json",
    "~/.gitconfig",
    "~/.config/git",
];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub stack: String,
    #[serde(default = "default_branches")]
    pub protected_branches: Vec<String>,
    /// Never editable by an agent.
    #[serde(default)]
    pub protected: Vec<String>,
    /// Editable only after the owner says yes (files that define what the checks check).
    #[serde(default)]
    pub guarded: Vec<String>,
    /// Globs exempt from the suppressions rule (generated code).
    #[serde(default)]
    pub suppressions_allow: Vec<String>,
    /// Formatter; the edited file is appended as the last argument.
    #[serde(default)]
    pub format: Option<String>,
    /// Run once, in order, before the parallel tasks (code generation, sync).
    #[serde(default)]
    pub prepare: Vec<String>,
    /// Fast tasks, in parallel: Stop hook, pre-commit, pre-push, start of `aycod ci`.
    #[serde(default)]
    pub check: Vec<String>,
    /// Slower tasks, in order, after `check`: `aycod ci` only.
    #[serde(default)]
    pub ci: Vec<String>,
    /// Extra tasks for the pre-commit hook only, never cached (secret scan of the staged changes).
    #[serde(default)]
    pub pre_commit: Vec<String>,
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
}

fn default_branches() -> Vec<String> {
    vec!["main".into(), "master".into()]
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Task {
    One(String),
    Steps(Vec<String>),
    Full {
        run: Steps,
        /// Skipped (with a notice) when its program is not installed.
        #[serde(default)]
        optional: bool,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Steps {
    One(String),
    Many(Vec<String>),
}

impl Task {
    pub fn steps(&self) -> Vec<&str> {
        match self {
            Task::One(s) | Task::Full { run: Steps::One(s), .. } => vec![s.as_str()],
            Task::Steps(v) | Task::Full { run: Steps::Many(v), .. } => v.iter().map(String::as_str).collect(),
        }
    }

    pub fn optional(&self) -> bool {
        matches!(self, Task::Full { optional: true, .. })
    }
}

pub struct Project {
    pub root: PathBuf,
    pub config: Config,
    /// `aycod.toml` was found (otherwise defaults apply).
    pub configured: bool,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        let c: Config = toml::from_str(text).map_err(|e| format!("{FILE}: {e}"))?;
        for name in c.check.iter().chain(&c.ci).chain(&c.pre_commit) {
            if !c.tasks.contains_key(name) {
                return Err(format!("{FILE}: task `{name}` is listed but not defined under [tasks]"));
            }
        }
        Ok(c)
    }
}

/// Walks up from `start` to the directory holding `aycod.toml`, else the git root, else `start`.
pub fn find_root(start: &Path) -> (PathBuf, bool) {
    let mut git_root = None;
    for dir in start.ancestors() {
        if dir.join(FILE).is_file() {
            return (dir.to_path_buf(), true);
        }
        if git_root.is_none() && dir.join(".git").exists() {
            git_root = Some(dir.to_path_buf());
        }
    }
    (git_root.unwrap_or_else(|| start.to_path_buf()), false)
}

pub fn load(start: &Path) -> Result<Project, String> {
    let (root, configured) = find_root(start);
    let config = if configured {
        let text = std::fs::read_to_string(root.join(FILE)).map_err(|e| format!("{FILE}: {e}"))?;
        Config::parse(&text)?
    } else {
        Config { protected_branches: default_branches(), ..Config::default() }
    };
    Ok(Project { root, config, configured })
}
