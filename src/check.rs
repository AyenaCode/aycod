//! `aycod check` / `aycod ci`: native task runner with a content-addressed cache.
//!
//! One parallel walk of the project (honouring `.gitignore`) hashes every file with blake3 and
//! scans it for unjustified suppressions. If the fingerprint matches the last green run, nothing
//! runs. Otherwise the tasks run in parallel, without a shell, each into its own log file.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::bytes::Regex;

use crate::config::{Config, Project, Task};
use crate::shell;

/// Files whose fingerprint `aycod install` records; `check` refuses to pass if they change.
pub const MANAGED: &[&str] = &[
    "aycod.toml",
    ".githooks/pre-commit",
    ".githooks/pre-push",
    ".claude/settings.json",
    ".codex/hooks.json",
    ".github/hooks/aycod.json",
];

const SUPPRESSIONS: &str = concat!(
    r"(?-u)@ts-ignore|@ts-nocheck|@ts-expect-error|eslint-disable|biome-ignore|oxlint-disable|svelte-ignore",
    r"|(istanbul|c8|v8) ignore|//\s*nolint|#nosec|#!?\[(allow|expect)\(|@Suppress(Lint|Warnings)?\(",
    r"|ktlint-disable|detekt[.:]|swiftlint:disable|swiftformat:disable|nosemgrep|gitleaks:allow|NOSONAR",
    r"|#\s*noqa|type:\s*ignore|\b(it|test|describe|context|suite)\.(skip|only|todo|fixme|skipIf|runIf)\(",
    r"|\b(xit|xdescribe|fit|fdescribe)\(|t\.Skip(Now|f)?\(|#\[ignore\b|@(Ignore|Disabled)\b|XCTSkip",
    r"|@Test\([^)]*\.disabled\(|\.enabled\(if:",
);
const JUSTIFIED: [&[u8]; 2] = [b"AYCOD-OK:", b"FORGE-OK:"];
const SUPPRESSIONS_EXEMPT: &[&str] = &[
    "**/*.md",
    ".claude/**",
    "tools/forge/**",
    "**/*.lock",
    "**/*-lock.json",
    "**/*-lock.yaml",
    "**/*.lockb",
    "**/verification-metadata.xml",
];
const LOG_TAIL: usize = 40;

/// Where aycod keeps its stamp, logs and integrity record: inside `.git` when there is one.
pub fn state_dir(root: &Path) -> PathBuf {
    let dot = root.join(".git");
    if dot.is_dir() {
        return dot.join("aycod");
    }
    if let Ok(text) = fs::read_to_string(&dot)
        && let Some(gd) = text.strip_prefix("gitdir:")
    {
        return root.join(gd.trim()).join("aycod");
    }
    root.join(".aycod")
}

pub struct Snapshot {
    pub fingerprint: String,
    /// `(relative path, blake3)` of every file, sorted.
    pub files: Vec<(String, blake3::Hash)>,
}

impl Snapshot {
    pub fn hash_of(&self, rel: &str) -> Option<blake3::Hash> {
        self.files.binary_search_by(|(p, _)| p.as_str().cmp(rel)).ok().map(|i| self.files[i].1)
    }
}

fn exempt_set(config: &Config) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for g in SUPPRESSIONS_EXEMPT.iter().copied().chain(config.suppressions_allow.iter().map(String::as_str)) {
        if let Ok(g) = Glob::new(g) {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Below this many files, hashing on one thread beats spawning workers.
const PARALLEL_FROM: usize = 512;

/// Hashes the tree (minus ignored files, `.git` and `.aycod`).
///
/// The walk is sequential: `ignore`'s parallel walker idles in 1 ms sleeps, which costs more than
/// it saves on a typical project. Hashing goes parallel only for large trees.
pub fn snapshot(root: &Path, config: &Config) -> Snapshot {
    let mut paths: Vec<(String, bool)> = Vec::with_capacity(1024);
    let walk = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .parents(false)
        .filter_entry(|e| !(e.depth() == 1 && matches!(e.file_name().to_str(), Some(".git" | ".aycod"))))
        .build();
    for entry in walk.flatten() {
        let Some(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        paths.push((rel, ft.is_symlink()));
    }
    paths.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    // One read buffer and one path per worker instead of allocations per file: musl's allocator
    // takes a global lock, and on a 6000-file tree the per-file `fs::read` made the cached check
    // 5x slower than with glibc. `clone_from` keeps the path's capacity.
    let base = root.to_path_buf();
    let hash_part = |part: &[(String, bool)]| {
        let mut buf = Vec::with_capacity(64 * 1024);
        let mut path = PathBuf::with_capacity(base.as_os_str().len() + 256);
        part.iter()
            .map(|(rel, link)| {
                path.clone_from(&base);
                path.push(rel);
                buf.clear();
                if *link {
                    if let Ok(t) = fs::read_link(&path) {
                        buf.extend_from_slice(t.as_os_str().as_encoded_bytes());
                    }
                } else if File::open(&path).and_then(|mut f| f.read_to_end(&mut buf)).is_err() {
                    buf.clear();
                }
                blake3::hash(&buf)
            })
            .collect::<Vec<_>>()
    };
    let hashes: Vec<blake3::Hash> = if paths.len() < PARALLEL_FROM {
        hash_part(&paths)
    } else {
        let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        let chunk = paths.len().div_ceil(threads);
        std::thread::scope(|s| {
            let workers: Vec<_> = paths.chunks(chunk).map(|part| s.spawn(move || hash_part(part))).collect();
            workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
        })
    };
    let mut h = blake3::Hasher::new();
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    for t in config.prepare.iter().chain(&config.check) {
        h.update(t.as_bytes()).update(b"\0");
    }
    let files: Vec<(String, blake3::Hash)> = paths.into_iter().map(|(p, _)| p).zip(hashes).collect();
    for (rel, hash) in &files {
        h.update(rel.as_bytes()).update(b"\0").update(hash.as_bytes());
    }
    Snapshot { fingerprint: h.finalize().to_hex().to_string(), files }
}

/// `path:line: text` of each unjustified suppression. Only runs when the cache missed: compiling
/// the pattern costs more than hashing a small project.
pub fn suppressions(root: &Path, config: &Config, snap: &Snapshot) -> Vec<String> {
    let re = Regex::new(SUPPRESSIONS).expect("valid suppressions pattern");
    let exempt = exempt_set(config);
    let files: Vec<&str> = snap.files.iter().map(|(p, _)| p.as_str()).filter(|p| !exempt.is_match(p)).collect();
    let threads = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = files.len().div_ceil(threads).max(1);
    let mut hits: Vec<String> = std::thread::scope(|s| {
        let workers: Vec<_> = files
            .chunks(chunk)
            .map(|part| {
                let re = &re;
                s.spawn(move || {
                    let mut out = Vec::new();
                    for rel in part {
                        let path = root.join(rel);
                        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file())
                            && let Ok(bytes) = fs::read(&path)
                        {
                            out.extend(scan(rel, &bytes, re));
                        }
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().filter_map(|w| w.join().ok()).flatten().collect()
    });
    hits.sort();
    hits
}

fn scan(rel: &str, bytes: &[u8], re: &Regex) -> Vec<String> {
    let head = &bytes[..bytes.len().min(8000)];
    if bytes.len() > 4 << 20 || head.contains(&0) {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut last_line_end = 0;
    for m in re.find_iter(bytes) {
        if m.start() < last_line_end {
            continue;
        }
        let start = bytes[..m.start()].iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        let end = bytes[m.end()..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |p| m.end() + p);
        last_line_end = end;
        let line = &bytes[start..end];
        if JUSTIFIED.iter().any(|j| line.windows(j.len()).any(|w| w == *j)) {
            continue;
        }
        let number = memchr_count(&bytes[..start]) + 1;
        let text = String::from_utf8_lossy(line);
        let text: String = text.trim().chars().take(160).collect();
        hits.push(format!("{rel}:{number}: {text}"));
    }
    hits
}

#[allow(clippy::naive_bytecount)] // only on hits, which are rare
fn memchr_count(b: &[u8]) -> usize {
    b.iter().filter(|&&c| c == b'\n').count()
}

// ---------- integrity ----------

pub fn record_integrity(root: &Path, snap: &Snapshot) -> io::Result<()> {
    let dir = state_dir(root);
    fs::create_dir_all(&dir)?;
    let mut out = String::new();
    for rel in MANAGED {
        if let Some(h) = snap.hash_of(rel) {
            let _ = writeln!(out, "{h}  {rel}");
        }
    }
    fs::write(dir.join("integrity"), out)
}

/// Differences between the managed files and what `aycod install` recorded. `None`: never installed.
pub fn integrity_problems(root: &Path, snap: &Snapshot) -> Option<Vec<String>> {
    let text = fs::read_to_string(state_dir(root).join("integrity")).ok()?;
    let mut problems = Vec::new();
    let mut recorded = Vec::new();
    for line in text.lines() {
        let Some((hash, rel)) = line.split_once("  ") else { continue };
        recorded.push(rel);
        match snap.hash_of(rel) {
            Some(h) if h.to_hex().as_str() == hash => {}
            Some(_) => problems.push(format!("{rel} changed")),
            None => problems.push(format!("{rel} was removed")),
        }
    }
    for rel in MANAGED {
        if !recorded.contains(rel) && snap.hash_of(rel).is_some() {
            problems.push(format!("{rel} appeared"));
        }
    }
    if root.join(".git").exists() {
        let hooks = hooks_path(root);
        if hooks != ".githooks" {
            problems.push(format!("git core.hooksPath is `{hooks}`, not `.githooks`"));
        }
        // Git silently skips a hook that is not executable.
        for hook in [".githooks/pre-commit", ".githooks/pre-push"] {
            if root.join(hook).is_file() && !executable(&root.join(hook)) {
                problems.push(format!("{hook} is not executable"));
            }
        }
    }
    Some(problems)
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(_: &Path) -> bool {
    true
}

/// `core.hooksPath`, read from the repository's own config file when it is plain (no `include`),
/// which saves spawning git on every cached check; otherwise asks git.
pub fn hooks_path(root: &Path) -> String {
    if !root.join(".git").exists() {
        return String::new();
    }
    let local = fs::read_to_string(state_dir(root).with_file_name("config")).ok().and_then(|text| {
        let mut section = String::new();
        let mut value = None;
        for line in text.lines().map(str::trim) {
            if let Some(head) = line.strip_prefix('[') {
                section = head.trim_end_matches(']').trim().to_ascii_lowercase();
                if section.starts_with("include") {
                    return None;
                }
            } else if section == "core"
                && let Some((k, v)) = line.split_once('=')
                && k.trim().eq_ignore_ascii_case("hookspath")
            {
                let v = v.trim();
                value = Some(match v.strip_prefix('"') {
                    Some(q) => q.split('"').next().unwrap_or("").to_owned(),
                    None => v.split([';', '#']).next().unwrap_or("").trim().to_owned(),
                });
            }
        }
        value
    });
    local.unwrap_or_else(|| {
        Command::new("git")
            .args(["config", "--get", "core.hooksPath"])
            .current_dir(root)
            .stderr(Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    })
}

// ---------- tasks ----------

pub struct Outcome {
    pub name: String,
    pub ok: bool,
    pub skipped: Option<String>,
    pub elapsed: Duration,
    pub log: PathBuf,
    /// The step that failed (or the first one).
    pub step: String,
}

fn run_task(root: &Path, logs: &Path, name: &str, task: &Task) -> Outcome {
    let started = Instant::now();
    let log = logs.join(format!("{name}.log"));
    let mut out =
        Outcome { name: name.to_owned(), ok: false, skipped: None, elapsed: Duration::ZERO, log, step: String::new() };
    let file = File::create(&out.log);
    let Ok(mut file) = file else {
        out.step = format!("cannot write {}", out.log.display());
        return out;
    };
    for step in task.steps() {
        step.clone_into(&mut out.step);
        let (env, argv) = match shell::split_task(step) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(file, "{e}");
                out.elapsed = started.elapsed();
                return out;
            }
        };
        let _ = writeln!(file, "$ {step}");
        let (Ok(o), Ok(e)) = (file.try_clone(), file.try_clone()) else { return out };
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .envs(env)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(o)
            .stderr(e)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                let _ = writeln!(file, "\n[aycod] `{step}` exited with {s}");
                out.elapsed = started.elapsed();
                return out;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound && task.optional() => {
                out.skipped = Some(format!("{} is not installed", argv[0]));
                out.ok = true;
                out.elapsed = started.elapsed();
                return out;
            }
            Err(err) => {
                let _ = writeln!(file, "[aycod] cannot run `{}`: {err}", argv[0]);
                out.elapsed = started.elapsed();
                return out;
            }
        }
    }
    out.ok = true;
    out.elapsed = started.elapsed();
    out
}

/// Runs `names` in parallel (or one after the other), stopping nothing early: every log is kept.
pub fn run_tasks(project: &Project, names: &[String], parallel: bool) -> Vec<Outcome> {
    let logs = state_dir(&project.root).join("logs");
    let _ = fs::create_dir_all(&logs);
    let tasks: Vec<(&String, &Task)> =
        names.iter().filter_map(|n| project.config.tasks.get(n).map(|t| (n, t))).collect();
    if !parallel || tasks.len() < 2 {
        let mut out = Vec::new();
        for (n, t) in tasks {
            let o = run_task(&project.root, &logs, n, t);
            let stop = !o.ok;
            out.push(o);
            if stop && !parallel {
                break;
            }
        }
        return out;
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = tasks.iter().map(|(n, t)| s.spawn(|| run_task(&project.root, &logs, n, t))).collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

fn prepare(project: &Project) -> Option<Outcome> {
    if project.config.prepare.is_empty() {
        return None;
    }
    let logs = state_dir(&project.root).join("logs");
    let _ = fs::create_dir_all(&logs);
    let task = Task::Steps(project.config.prepare.clone());
    Some(run_task(&project.root, &logs, "prepare", &task))
}

pub fn tail(path: &Path, lines: usize) -> String {
    let mut text = String::new();
    if let Ok(mut f) = File::open(path) {
        let mut buf = Vec::new();
        let _ = f.read_to_end(&mut buf);
        text = String::from_utf8_lossy(&buf).into_owned();
    }
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

// ---------- the gate ----------

pub enum Verdict {
    /// Same tree as the last green run.
    Cached,
    Green(Vec<Outcome>),
    /// The report to show the agent or the developer.
    Red(String),
}

pub struct Options {
    pub cache: bool,
    pub integrity: bool,
}

/// `check`: integrity, cache, suppressions, then the `check` tasks in parallel.
pub fn check(project: &Project, opts: &Options) -> Verdict {
    let root = &project.root;
    let snap = snapshot(root, &project.config);
    if opts.integrity
        && let Some(problems) = integrity_problems(root, &snap)
        && !problems.is_empty()
    {
        return Verdict::Red(format!(
            "The aycod guardrails were modified outside an owner action:\n  - {}\n\
             Only the project owner can accept this, by running `aycod install` in a terminal.",
            problems.join("\n  - ")
        ));
    }
    let state = state_dir(root);
    let stamp = state.join("green");
    if opts.cache && fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == snap.fingerprint) {
        return Verdict::Cached;
    }
    let hits = suppressions(root, &project.config, &snap);
    if !hits.is_empty() {
        return Verdict::Red(format!(
            "Unjustified suppressions: fix the underlying issue, or have the owner add `AYCOD-OK: <reason>` on the line.\n{}",
            hits.join("\n")
        ));
    }
    let mut outcomes = Vec::new();
    if let Some(p) = prepare(project) {
        let ok = p.ok;
        outcomes.push(p);
        if !ok {
            return Verdict::Red(report(&outcomes));
        }
    }
    outcomes.extend(run_tasks(project, &project.config.check, true));
    if outcomes.iter().all(|o| o.ok) {
        let _ = fs::create_dir_all(&state);
        let _ = fs::write(&stamp, &snap.fingerprint);
        Verdict::Green(outcomes)
    } else {
        Verdict::Red(report(&outcomes))
    }
}

/// What a failing run tells the agent: enough to fix it on the first try (some agents cap retries).
pub fn report(outcomes: &[Outcome]) -> String {
    let mut s = String::from(
        "The project checks are failing: you cannot finish until they pass.\n\
         Fix the root cause without disabling, skipping or weakening any test, rule or threshold.\n",
    );
    for o in outcomes.iter().filter(|o| !o.ok) {
        let _ =
            write!(s, "\n✗ {} — `{}` (full log: {})\n{}\n", o.name, o.step, o.log.display(), tail(&o.log, LOG_TAIL));
    }
    s
}

pub fn print_outcomes(outcomes: &[Outcome], w: &mut impl Write) {
    for o in outcomes {
        let mark = if o.skipped.is_some() {
            "–"
        } else if o.ok {
            "✓"
        } else {
            "✗"
        };
        let note = o.skipped.as_deref().map(|s| format!(" (skipped: {s})")).unwrap_or_default();
        let _ = writeln!(w, "{mark} {:<14} {:>7.2}s{note}", o.name, o.elapsed.as_secs_f64());
    }
}
