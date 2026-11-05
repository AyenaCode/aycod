//! The guard: decides whether an agent may run a command, read a file or edit a file.
//!
//! Rules apply to every simple command after the lexer has split the line. What cannot be
//! resolved statically (`eval`, `bash -c "$X"`, code piped into an interpreter) is refused.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};

use crate::config::{BUILTIN_PROTECTED, Config};
use crate::shell::{self, Simple, Word};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Allowed once the owner says yes (agents without an "ask" answer get a deny).
    Ask(String),
    Deny(String),
}

/// Internal result: `Err` carries the reason of a deny.
type Verdict = Result<(), String>;

/// A protected or guarded path, absolute, possibly a glob.
struct PathRule {
    path: PathBuf,
    glob: Option<GlobMatcher>,
    /// Single top-level name (`.githooks`): copying anything with that name is suspicious.
    top_name: Option<String>,
}

impl PathRule {
    fn new(root: &Path, home: Option<&str>, spec: &str) -> Self {
        let spec = spec.trim_end_matches("/**").trim_end_matches("/*").trim_end_matches('/');
        let path = match (spec.strip_prefix("~/"), home) {
            (Some(rest), Some(h)) => Path::new(h).join(rest),
            _ => root.join(spec),
        };
        let path = normalize(&path);
        let glob = spec.contains(['*', '?', '[', '{']).then(|| {
            GlobBuilder::new(&path.to_string_lossy()).literal_separator(true).build().ok().map(|g| g.compile_matcher())
        });
        let top_name = (!spec.contains('/') && !spec.starts_with('~')).then(|| spec.to_owned());
        PathRule { path, glob: glob.flatten(), top_name }
    }

    /// `abs` is this path or inside it.
    fn covers(&self, abs: &Path) -> bool {
        match &self.glob {
            Some(g) => abs.ancestors().any(|a| g.is_match(a)),
            None => abs.starts_with(&self.path),
        }
    }

    /// Removing or moving `abs` (maybe a glob, maybe a parent directory) would touch this path.
    fn touched_by(&self, abs: &Path, recursive_ancestor: bool) -> bool {
        let text = abs.to_string_lossy();
        if let Some(pos) = text.find(['*', '?', '[', '{']) {
            let matcher = GlobBuilder::new(&text).literal_separator(true).build().map(|g| g.compile_matcher());
            let hit = matcher.is_ok_and(|m| self.path.ancestors().any(|a| m.is_match(a)));
            // `.githooks/*`: the literal prefix already sits inside the rule.
            let prefix = Path::new(&text[..pos]);
            return hit || (text[..pos].ends_with('/') && prefix.starts_with(&self.path));
        }
        self.covers(abs) || (recursive_ancestor && self.path.starts_with(abs))
    }
}

pub struct Policy {
    root: PathBuf,
    home: Option<String>,
    protected: Vec<PathRule>,
    guarded: Vec<PathRule>,
    branches: Vec<String>,
    tmp_dirs: Vec<PathBuf>,
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish", "busybox"];
const INTERPRETERS: &[&str] =
    &["python", "python3", "python2", "node", "nodejs", "deno", "bun", "perl", "ruby", "php", "lua", "tsx", "ts-node"];
/// Inline code that starts processes is as opaque as `eval`.
const SPAWN_MARKERS: &[&str] = &[
    "system(",
    "subprocess",
    "child_process",
    "popen",
    "spawn",
    "exec(",
    "execSync",
    "execFile",
    "Command::",
    "Deno.Command",
    "Deno.run",
    "pty.",
    "shell_exec",
    "passthru",
    "proc_open",
    "os.exec",
];
/// Interpreters where backquotes run a shell command (in JS they are template literals).
const BACKTICK_SHELLS: &[&str] = &["perl", "ruby", "php"];
const SECRET_EXT: &[&str] = &["pem", "key", "p12", "pfx", "p8", "jks", "keystore", "mobileprovision"];
const SECRET_NAMES: &[&str] = &[
    ".env",
    "google-services.json",
    "GoogleService-Info.plist",
    "id_rsa",
    "id_ecdsa",
    "id_ed25519",
    "id_dsa",
    ".netrc",
    ".pgpass",
    "credentials.toml",
];
const SECRET_DIRS: &[&str] = &["/.ssh/", "/.aws/", "/.gnupg/", "/.config/gcloud/", "/secrets/", "/.docker/"];
const ENV_TEMPLATES: &[&str] = &[".env.example", ".env.sample", ".env.template", ".env.dist", ".env.test"];
const PUBLISH: &[(&str, &str)] = &[
    ("npm", "publish"),
    ("npm", "unpublish"),
    ("pnpm", "publish"),
    ("yarn", "publish"),
    ("bun", "publish"),
    ("cargo", "publish"),
    ("cargo", "yank"),
    ("eas", "submit"),
    ("gem", "push"),
    ("twine", "upload"),
];
/// `aycod` subcommands an agent may run: none of them weakens the policy.
const AYCOD_SAFE: &[&str] = &["check", "ci", "doctor", "hook", "version", "--version", "-V", "help", "--help", "-h"];

impl Policy {
    pub fn new(root: &Path, config: &Config, home: Option<String>) -> Self {
        let root = normalize(root);
        let rule = |p: &str| PathRule::new(&root, home.as_deref(), p);
        let protected = BUILTIN_PROTECTED.iter().copied().chain(config.protected.iter().map(String::as_str)).map(rule);
        let mut tmp_dirs = vec![PathBuf::from("/tmp"), PathBuf::from("/var/tmp"), PathBuf::from("/private/tmp")];
        if let Ok(t) = std::env::var("TMPDIR") {
            tmp_dirs.push(normalize(Path::new(&t)));
        }
        Policy {
            protected: protected.collect(),
            guarded: config.guarded.iter().map(|p| rule(p)).collect(),
            root,
            home,
            branches: config.protected_branches.clone(),
            tmp_dirs,
        }
    }

    pub fn read(path: &str) -> Decision {
        if is_secret(path) {
            return Decision::Deny(format!("reading secrets is forbidden ({path})."));
        }
        Decision::Allow
    }

    pub fn edit(&self, path: &str, cwd: &Path) -> Decision {
        if is_secret(path) {
            return Decision::Deny(format!("secret files are out of reach ({path})."));
        }
        let abs = normalize(&cwd.join(path));
        if self.protected.iter().any(|r| r.covers(&abs)) {
            return Decision::Deny(format!("{path} is protected by aycod: only the project owner can change it."));
        }
        if self.guarded.iter().any(|r| r.covers(&abs)) {
            return Decision::Ask(format!(
                "{path} defines what the checks check: the project owner must approve this edit."
            ));
        }
        Decision::Allow
    }

    pub fn command(&self, src: &str, cwd: &Path) -> Decision {
        let mut env = HashMap::new();
        if let Some(h) = &self.home {
            env.insert("HOME".to_owned(), h.clone());
        }
        env.insert("PWD".to_owned(), cwd.to_string_lossy().into_owned());
        let mut run = Run { p: self, cwd: Some(normalize(cwd)), start: normalize(cwd), env, depth: 0 };
        match run.script(src) {
            Ok(()) => Decision::Allow,
            Err(why) => Decision::Deny(why),
        }
    }

    /// Branch `git push` sends when no refspec names one.
    fn current_branch(&self) -> Option<String> {
        for dir in self.root.ancestors() {
            let dot = dir.join(".git");
            if dot.is_dir() {
                return head_branch(&dot);
            }
            if dot.is_file() {
                let text = std::fs::read_to_string(&dot).ok()?;
                return head_branch(&dir.join(text.strip_prefix("gitdir:")?.trim()));
            }
        }
        None
    }
}

fn head_branch(git_dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    head.trim().strip_prefix("ref: refs/heads/").map(str::to_owned)
}

/// One command line being checked: tracks `cd` so relative paths resolve.
struct Run<'a> {
    p: &'a Policy,
    /// `None` once a `cd` target is unknown.
    cwd: Option<PathBuf>,
    /// Paths are also resolved from here: `(cd x); rm y` runs `rm` in the start directory.
    start: PathBuf,
    env: HashMap<String, String>,
    depth: u8,
}

fn base(w: &Word) -> &str {
    w.text.rsplit('/').next().unwrap_or(&w.text)
}

/// Short option cluster (`-fu`): its letters, or `None` for `--long` and operands.
fn cluster(w: &Word) -> Option<&str> {
    let t = w.text.strip_prefix('-')?;
    (!t.starts_with('-') && !t.is_empty()).then_some(t)
}

impl Run<'_> {
    fn script(&mut self, src: &str) -> Verdict {
        if self.depth > 6 {
            return Err("command nesting too deep: aycod cannot see what would run.".into());
        }
        let script = shell::parse(src, &self.env);
        if let Some(why) = script.opaque.first() {
            return Err(format!("{why}: aycod cannot see what would run."));
        }
        for c in &script.commands {
            self.simple(c)?;
        }
        Ok(())
    }

    fn nested(&mut self, src: &str) -> Verdict {
        self.depth += 1;
        let r = self.script(src);
        self.depth -= 1;
        r
    }

    fn simple(&mut self, c: &Simple) -> Verdict {
        for (name, _) in &c.assigns {
            env_name(name)?;
        }
        for w in &c.writes {
            if w.dynamic {
                return Err(format!("redirecting output to `{}`: use a literal path.", w.text));
            }
            if w.text != "/dev/null" && w.text != "/dev/stdout" && w.text != "/dev/stderr" {
                self.writable(w, false)?;
            }
        }
        // `echo .env >> .gitignore` and commit messages mention secrets without reading them.
        let prints = c.words.first().is_some_and(|w| matches!(base(w), "echo" | "printf"));
        let mut message = false;
        for w in c.words.iter().filter(|_| !prints).chain(&c.reads) {
            if !std::mem::replace(&mut message, matches!(w.text.as_str(), "-m" | "--message")) {
                secret_word(w)?;
            }
        }
        if c.words.is_empty() {
            return Ok(());
        }
        if c.words[0].text == "cd" || c.words[0].text == "pushd" {
            self.cd(c.words.get(1));
            return Ok(());
        }
        self.exec(&c.words, c, false)
    }

    fn cd(&mut self, target: Option<&Word>) {
        let next = match target {
            None => self.p.home.as_ref().map(PathBuf::from),
            Some(w) if w.dynamic || w.text == "-" => None,
            Some(w) => self.cwd.as_ref().map(|d| normalize(&d.join(&w.text))),
        };
        self.cwd = next;
    }

    /// Checks `words` as a command, unwrapping `env`, `xargs`, `npx`... `tail` marks arguments
    /// appended at run time (from `xargs` or `find -exec`).
    #[allow(clippy::too_many_lines)]
    fn exec(&mut self, words: &[Word], c: &Simple, tail: bool) -> Verdict {
        let Some(first) = words.first() else { return Ok(()) };
        if first.dynamic {
            return Err(format!("`{}` produces the command to run: aycod cannot see what would run.", first.text));
        }
        let name = base(first);
        let args = &words[1..];
        match name {
            "sudo" | "doas" | "su" | "pkexec" | "run0" => Err(format!("{name} is forbidden.")),
            "eval" => Err("eval is forbidden: aycod cannot see what would run.".into()),
            "env" => {
                let mut i = 0;
                while let Some(w) = args.get(i) {
                    if matches!(w.text.as_str(), "-S" | "--split-string") || w.text.starts_with("--split-string=") {
                        return Err("env -S is forbidden: aycod cannot see what would run.".into());
                    }
                    if matches!(w.text.as_str(), "-u" | "--unset" | "-C" | "--chdir") {
                        i += 2;
                    } else if w.text.starts_with('-') {
                        i += 1;
                    } else if let Some((k, _)) = shell::split_assignment(w) {
                        env_name(&k)?;
                        i += 1;
                    } else {
                        break;
                    }
                }
                self.exec(&args[i.min(args.len())..], c, tail)
            }
            "command" | "builtin" | "exec" | "nohup" | "setsid" | "time" | "stdbuf" | "unbuffer" => {
                if name == "command" && args.iter().any(|w| w.text == "-v" || w.text == "-V") {
                    return Ok(());
                }
                let i = args.iter().position(|w| !w.text.starts_with('-')).unwrap_or(args.len());
                self.exec(&args[i..], c, tail)
            }
            "nice" | "ionice" => {
                let mut i = 0;
                while args.get(i).is_some_and(|w| w.text.starts_with('-')) {
                    i += if matches!(args[i].text.as_str(), "-n" | "-c" | "-p") { 2 } else { 1 };
                }
                self.exec(&args[i.min(args.len())..], c, tail)
            }
            "timeout" => {
                let mut i = 0;
                while args.get(i).is_some_and(|w| w.text.starts_with('-')) {
                    i += if matches!(args[i].text.as_str(), "-s" | "-k" | "--signal" | "--kill-after") { 2 } else { 1 };
                }
                self.exec(&args[(i + 1).min(args.len())..], c, tail)
            }
            "xargs" => {
                let mut i = 0;
                while let Some(w) = args.get(i) {
                    if !w.text.starts_with('-') {
                        break;
                    }
                    let takes = matches!(w.text.as_str(), "-I" | "-L" | "-n" | "-P" | "-d" | "-E" | "-s" | "-a");
                    i += if takes { 2 } else { 1 };
                }
                self.exec(&args[i.min(args.len())..], c, true)
            }
            "find" => {
                let mut i = 0;
                while i < args.len() {
                    if matches!(args[i].text.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
                        let end = args[i + 1..]
                            .iter()
                            .position(|w| w.text == ";" || w.text == "+")
                            .map_or(args.len(), |p| i + 1 + p);
                        self.exec(&args[i + 1..end], c, true)?;
                        i = end;
                    }
                    if args.get(i).is_some_and(|w| w.text == "-delete") {
                        let roots: Vec<&Word> = args.iter().take_while(|w| !w.text.starts_with('-')).collect();
                        for r in roots {
                            self.writable(r, true)?;
                        }
                    }
                    i += 1;
                }
                Ok(())
            }
            "npx" | "bunx" | "pnpx" => {
                if let Some(pos) = args.iter().position(|w| w.text == "-c" || w.text == "--call") {
                    return self.inline_shell(args.get(pos + 1), tail);
                }
                let mut i = 0;
                while let Some(w) = args.get(i) {
                    if !w.text.starts_with('-') {
                        break;
                    }
                    i += if matches!(w.text.as_str(), "-p" | "--package") { 2 } else { 1 };
                }
                self.exec(&args[i.min(args.len())..], c, tail)
            }
            "pnpm" | "npm" | "yarn"
                if args.first().is_some_and(|w| matches!(w.text.as_str(), "exec" | "dlx" | "x")) =>
            {
                if let Some(pos) = args.iter().position(|w| w.text == "-c" || w.text == "--call") {
                    return self.inline_shell(args.get(pos + 1), tail);
                }
                let rest = &args[1..];
                let i = rest.iter().position(|w| !w.text.starts_with('-')).unwrap_or(rest.len());
                self.exec(&rest[i..], c, tail)
            }
            "sed" | "perl" | "ruby" if in_place(args) => self.writes_all(args, false, tail, false),
            _ if SHELLS.contains(&name) => self.shell(args, c, tail),
            _ if INTERPRETERS.contains(&name) || name.starts_with("python3.") => self.interpreter(name, args, c),
            "source" | "." => match args.first() {
                Some(w) if w.dynamic => Err("sourcing a generated script: aycod cannot see what would run.".into()),
                _ => Ok(()),
            },
            "git" => self.git(args, tail),
            "aycod" => match args.first() {
                Some(w) if !w.dynamic && AYCOD_SAFE.contains(&w.text.as_str()) => Ok(()),
                None => Ok(()),
                Some(w) => Err(format!("`aycod {}` changes the guardrails: only the project owner runs it.", w.text)),
            },
            "rm" | "rmdir" | "unlink" | "shred" => self.writes_all(args, true, tail, true),
            "mv" => self.writes_all(args, true, tail, false),
            "cp" | "install" | "ln" | "rsync" | "tee" | "truncate" | "touch" | "chown" | "chgrp" | "dd" | "patch" => {
                self.writes_all(args, false, tail, false)
            }
            "chmod" => {
                let wide = args.iter().any(|w| {
                    matches!(w.text.as_str(), "777" | "0777" | "a+rwx" | "ugo+rwx" | "o+w" | "a+w")
                        || w.text.ends_with("777")
                });
                if wide {
                    return Err("world-writable permissions (777) are forbidden.".into());
                }
                self.writes_all(args, true, tail, false)
            }
            "docker" | "podman" if args.iter().any(|w| w.text == "--privileged" || w.text == "--privileged=true") => {
                Err("privileged containers are forbidden.".into())
            }
            "export" | "declare" | "typeset" | "readonly" | "local" => {
                for w in args {
                    if let Some((k, _)) = shell::split_assignment(w) {
                        env_name(&k)?;
                    }
                }
                Ok(())
            }
            _ => {
                let sub = args.first().map(|w| w.text.as_str());
                if PUBLISH.iter().any(|(t, s)| *t == name && Some(*s) == sub) {
                    return Err("publishing is reserved for the project owner.".into());
                }
                Ok(())
            }
        }
    }

    fn inline_shell(&mut self, code: Option<&Word>, tail: bool) -> Verdict {
        match code {
            Some(w) if !w.dynamic && !tail => self.nested(&w.text.clone()),
            _ => Err("a shell runs generated code: aycod cannot see what would run.".into()),
        }
    }

    fn shell(&mut self, args: &[Word], c: &Simple, tail: bool) -> Verdict {
        let mut i = 0;
        while let Some(w) = args.get(i) {
            if w.text == "--" {
                i += 1;
                break;
            }
            let Some(opts) = w.text.strip_prefix('-').or_else(|| w.text.strip_prefix('+')) else { break };
            if w.text.starts_with("--") {
                i += 1;
                continue;
            }
            if opts.contains('c') {
                return self.inline_shell(args.get(i + 1), tail);
            }
            if opts.contains('s') {
                return self.stdin_shell(c);
            }
            i += if matches!(w.text.as_str(), "-o" | "+o" | "-O" | "+O") { 2 } else { 1 };
        }
        match args.get(i) {
            Some(script) if script.dynamic => {
                Err("a shell runs a generated script: aycod cannot see what would run.".into())
            }
            Some(_) => Ok(()),
            None => self.stdin_shell(c),
        }
    }

    /// A shell with no script reads its commands from stdin.
    fn stdin_shell(&mut self, c: &Simple) -> Verdict {
        match (&c.heredoc, c.piped, c.dynamic_stdin) {
            (Some(body), false, false) if c.reads.is_empty() => self.nested(&body.clone()),
            (None, false, _) if c.reads.is_empty() => Err("interactive shells are not available to the agent.".into()),
            _ => Err("piping code into a shell is forbidden: aycod cannot see what would run.".into()),
        }
    }

    fn interpreter(&mut self, name: &str, args: &[Word], c: &Simple) -> Verdict {
        let inline = ["-c", "-e", "-E", "--eval", "-p", "--print", "-r"];
        if let Some(pos) = args.iter().position(|w| inline.contains(&w.text.as_str())) {
            return match args.get(pos + 1) {
                Some(code) if !code.dynamic => self.inline_code(name, &code.text),
                _ => Err(format!("{name} runs generated code: aycod cannot see what would run.")),
            };
        }
        let script = args.iter().find(|w| !w.text.starts_with('-'));
        match script {
            Some(w) if w.dynamic => Err(format!("{name} runs a generated script: aycod cannot see what would run.")),
            None if c.stdin || c.piped => match (&c.heredoc, c.piped, c.dynamic_stdin) {
                (Some(body), false, false) => self.inline_code(name, &body.clone()),
                _ => Err(format!("piping code into {name} is forbidden: aycod cannot see what would run.")),
            },
            _ => Ok(()),
        }
    }

    /// Inline Python/JS/Perl...: refused if it starts processes or names a protected path.
    fn inline_code(&self, interpreter: &str, code: &str) -> Verdict {
        if let Some(m) = SPAWN_MARKERS.iter().find(|m| code.contains(**m)) {
            return Err(format!("inline code starts processes (`{m}`): run a script file instead."));
        }
        if BACKTICK_SHELLS.contains(&interpreter) && (code.contains('`') || code.contains("qx")) {
            return Err(format!(
                "inline {interpreter} code runs shell commands (backquotes): run a script file instead."
            ));
        }
        for r in &self.p.protected {
            let rel = r.path.strip_prefix(&self.p.root).unwrap_or(&r.path).to_string_lossy();
            if !rel.is_empty() && code.contains(rel.as_ref()) {
                return Err(format!(
                    "inline code names the protected path {rel}: only the project owner can change it."
                ));
            }
        }
        if let Some(s) = SECRET_NAMES.iter().find(|s| code.contains(**s)) {
            return Err(format!("inline code names a secret file ({s})."));
        }
        Ok(())
    }

    /// Every path operand is written (or removed): none may be protected or guarded.
    fn writes_all(&mut self, args: &[Word], destructive: bool, tail: bool, removes: bool) -> Verdict {
        if tail {
            return Err("file operations on generated arguments (xargs, find -exec): use literal paths.".into());
        }
        for w in args {
            let path = if let Some(rest) = w.text.strip_prefix('-') {
                match rest.split_once('=') {
                    Some((_, v)) => v,
                    None => continue,
                }
            } else if let Some((_, v)) = w.text.split_once('=').filter(|(k, _)| *k == "of" || *k == "if") {
                v
            } else {
                &w.text
            };
            if w.dynamic {
                return Err(format!("`{}` is not a literal path: aycod cannot check what it touches.", w.text));
            }
            let word = Word { text: path.to_owned(), ..Word::default() };
            self.writable(&word, destructive)?;
            if removes {
                self.inside_project(&word)?;
            }
        }
        Ok(())
    }

    fn resolve(&self, w: &Word) -> Vec<PathBuf> {
        let mut out = vec![normalize(&self.start.join(&w.text))];
        if let Some(cwd) = &self.cwd {
            out.push(normalize(&cwd.join(&w.text)));
        }
        out
    }

    fn writable(&self, w: &Word, destructive: bool) -> Verdict {
        if self.cwd.is_none() && !w.text.starts_with('/') {
            return Err("the working directory is unknown after `cd`: use absolute paths.".into());
        }
        for abs in self.resolve(w) {
            for r in &self.p.protected {
                let same_name = r.top_name.as_deref().is_some_and(|n| base(w) == n);
                if r.touched_by(&abs, destructive) || same_name {
                    return Err(format!("{} is protected by aycod: only the project owner can change it.", w.text));
                }
            }
            if self.p.guarded.iter().any(|r| r.touched_by(&abs, destructive)) {
                return Err(format!(
                    "{} defines what the checks check: edit it with the edit tool so the owner is asked.",
                    w.text
                ));
            }
        }
        Ok(())
    }

    fn inside_project(&self, w: &Word) -> Verdict {
        for abs in self.resolve(w) {
            let ok = (abs.starts_with(&self.p.root) && abs != self.p.root)
                || self.p.tmp_dirs.iter().any(|t| abs.starts_with(t) && abs != *t);
            if !ok {
                return Err(format!("deleting outside the project is forbidden ({}).", w.text));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn git(&mut self, args: &[Word], tail: bool) -> Verdict {
        let config_read = args.iter().any(|w| w.text == "config")
            && args.iter().any(|w| matches!(w.text.as_str(), "--get" | "--get-all" | "--list" | "-l" | "get" | "list"));
        if !config_read && args.iter().any(|w| w.text.to_ascii_lowercase().contains("hookspath")) {
            return Err("changing the git hooks path is forbidden.".into());
        }
        let mut i = 0;
        while let Some(w) = args.get(i) {
            let t = w.text.as_str();
            if !t.starts_with('-') {
                break;
            }
            if t == "-c" || t.starts_with("-c") || t.starts_with("--config-env") || t.starts_with("--exec-path") {
                return Err(
                    "git -c, --config-env and --exec-path are forbidden: they can disable the git hooks.".into()
                );
            }
            i += if matches!(t, "-C" | "--git-dir" | "--work-tree" | "--namespace") { 2 } else { 1 };
        }
        let Some(sub) = args.get(i) else { return Ok(()) };
        if sub.dynamic {
            return Err("a variable produces the git command: aycod cannot see what would run.".into());
        }
        let rest = &args[i + 1..];
        if rest.iter().any(|w| w.text.starts_with("--no-verify")) {
            return Err("bypassing git hooks (--no-verify) is forbidden.".into());
        }
        let sub = sub.text.as_str();
        let sensitive = matches!(
            sub,
            "push"
                | "commit"
                | "reset"
                | "clean"
                | "branch"
                | "checkout"
                | "restore"
                | "stash"
                | "config"
                | "merge"
                | "rebase"
        );
        if sensitive {
            if tail {
                return Err(format!("git {sub} with generated arguments: aycod cannot see what would run."));
            }
            let mut prev_takes_value = false;
            for w in rest {
                if w.dynamic && !prev_takes_value {
                    return Err(format!("git {sub} with a variable argument (`{}`): use literal arguments.", w.text));
                }
                prev_takes_value =
                    matches!(w.text.as_str(), "-m" | "--message" | "-F" | "--file" | "--author" | "--date");
            }
        }
        match sub {
            "push" => self.push(rest),
            "commit" => {
                for w in rest {
                    if let Some(opts) = cluster(w) {
                        for ch in opts.chars() {
                            if ch == 'n' {
                                return Err("bypassing git hooks (commit -n) is forbidden.".into());
                            }
                            if "mFCctSu".contains(ch) {
                                break;
                            }
                        }
                    }
                }
                Ok(())
            }
            "reset" if rest.iter().any(|w| w.text == "--hard") => Err("git reset --hard destroys work.".into()),
            "clean" if rest.iter().any(|w| w.text == "--force" || cluster(w).is_some_and(|o| o.contains('f'))) => {
                Err("git clean -f deletes untracked files.".into())
            }
            "branch" => {
                let has =
                    |s: &str, c: char| rest.iter().any(|w| w.text == s || cluster(w).is_some_and(|o| o.contains(c)));
                if has("-D", 'D') || (has("--delete", 'd') && has("--force", 'f')) {
                    return Err("force-deleting a branch is forbidden.".into());
                }
                Ok(())
            }
            "checkout" | "restore" => {
                let staged_only = sub == "restore"
                    && rest.iter().any(|w| w.text == "--staged" || w.text == "-S")
                    && !rest.iter().any(|w| w.text == "--worktree" || w.text == "-W");
                let all = rest.iter().any(|w| matches!(w.text.as_str(), "." | ":/" | "*" | "./"));
                if all && !staged_only {
                    return Err("discarding all changes is forbidden.".into());
                }
                Ok(())
            }
            "stash" if rest.first().is_some_and(|w| w.text == "drop" || w.text == "clear") => {
                Err("dropping stashes is forbidden.".into())
            }
            "filter-branch" | "filter-repo" => Err("rewriting history is forbidden.".into()),
            "config" => {
                let reads = ["--get", "--get-all", "--get-regexp", "--get-urlmatch", "--list", "-l", "get", "list"];
                let positional = rest.iter().filter(|w| !w.text.starts_with('-')).count();
                if rest.iter().any(|w| reads.contains(&w.text.as_str())) || positional <= 1 {
                    return Ok(());
                }
                Err("changing git config is reserved for the project owner.".into())
            }
            _ => Ok(()),
        }
    }

    fn push(&self, rest: &[Word]) -> Verdict {
        let mut delete = false;
        let mut tags_only = false;
        let mut operands: Vec<&str> = Vec::new();
        let mut i = 0;
        while let Some(w) = rest.get(i) {
            let t = w.text.as_str();
            i += 1;
            if t == "--" {
                operands.extend(rest[i..].iter().map(|w| w.text.as_str()));
                break;
            }
            if let Some(long) = t.strip_prefix("--") {
                let key = long.split('=').next().unwrap_or(long);
                match key {
                    "force" | "force-with-lease" | "force-if-includes" => {
                        return Err("force push is forbidden.".into());
                    }
                    "mirror" | "prune" | "all" | "branches" => {
                        return Err(format!("git push --{key} can overwrite or delete protected branches."));
                    }
                    "receive-pack" | "exec" => return Err("git push --receive-pack runs arbitrary commands.".into()),
                    "delete" => delete = true,
                    "tags" => tags_only = true,
                    "repo" | "push-option" | "recurse-submodules" if !long.contains('=') => i += 1,
                    _ => {}
                }
                continue;
            }
            if let Some(opts) = cluster(w) {
                for ch in opts.chars() {
                    match ch {
                        'f' => return Err("force push is forbidden.".into()),
                        'd' => delete = true,
                        'o' => {
                            i += 1;
                            break;
                        }
                        _ => {}
                    }
                }
                continue;
            }
            operands.push(t);
        }
        let refspecs = operands.get(1..).unwrap_or(&[]);
        let mut targets: Vec<(String, bool)> = Vec::new();
        if refspecs.is_empty()
            && !tags_only
            && let Some(b) = self.p.current_branch()
        {
            targets.push((b, delete));
        }
        for spec in refspecs {
            if spec.starts_with('+') {
                return Err("force push (+refspec) is forbidden.".into());
            }
            let (src, dst) = spec.split_once(':').unwrap_or((spec, spec));
            let dst = dst.strip_prefix("refs/heads/").unwrap_or(dst);
            let dst =
                if dst == "HEAD" || dst == "@" { self.p.current_branch().unwrap_or_default() } else { dst.to_owned() };
            targets.push((dst, delete || src.is_empty()));
        }
        for (branch, del) in targets {
            if self.p.branches.contains(&branch) {
                return Err(if del {
                    format!("deleting the protected branch '{branch}' is forbidden.")
                } else {
                    format!(
                        "pushing directly to the protected branch '{branch}' is forbidden: push a branch and open a pull request."
                    )
                });
            }
        }
        Ok(())
    }
}

/// Environment variables that reconfigure git or aycod.
fn env_name(name: &str) -> Verdict {
    if name.starts_with("GIT_CONFIG") || name == "GIT_EXEC_PATH" || name.starts_with("AYCOD_") {
        return Err(format!("setting {name} is forbidden: it can disable the guardrails."));
    }
    Ok(())
}

fn in_place(args: &[Word]) -> bool {
    args.iter().any(|w| w.text.starts_with("--in-place") || cluster(w).is_some_and(|o| o.contains('i')))
}

fn secret_word(w: &Word) -> Verdict {
    let text = w.text.split_once('=').map_or(w.text.as_str(), |(k, v)| if k.starts_with('-') { v } else { &w.text });
    if is_secret(text) {
        return Err(format!("reading secrets is forbidden ({}).", w.text));
    }
    Ok(())
}

/// Files that hold credentials, by name, extension or directory.
pub fn is_secret(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if name.contains(['*', '?', '[']) {
        let Ok(g) = GlobBuilder::new(name).build() else { return true };
        let m = g.compile_matcher();
        return [".env", ".env.local", "a.pem", "a.key", "id_rsa", "id_ed25519"].iter().any(|s| m.is_match(s));
    }
    if name == ".env" || (name.starts_with(".env.") && !ENV_TEMPLATES.contains(&name)) {
        return true;
    }
    if SECRET_NAMES.contains(&name) || name.starts_with("id_rsa") || name.starts_with("id_ed25519") {
        return true;
    }
    if let Some((_, ext)) = name.rsplit_once('.')
        && SECRET_EXT.contains(&ext)
        && !name.starts_with('.')
    {
        return true;
    }
    let slashed = format!("/{}", path.replace('\\', "/"));
    SECRET_DIRS.iter().any(|d| slashed.contains(d))
}

/// Lexical normalization: removes `.` and resolves `..` (symlinks are not followed).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        let cfg = Config::parse(
            "protected = [\"pnpm-workspace.yaml\"]\nguarded = [\"vite.config.ts\", \".github/workflows/**\"]\n",
        )
        .unwrap();
        Policy::new(Path::new("/p"), &cfg, Some("/home/u".into()))
    }

    #[test]
    fn edits() {
        let p = policy();
        let cwd = Path::new("/p");
        assert!(matches!(p.edit("/p/aycod.toml", cwd), Decision::Deny(_)));
        assert!(matches!(p.edit(".githooks/pre-push", cwd), Decision::Deny(_)));
        assert!(matches!(p.edit("vite.config.ts", cwd), Decision::Ask(_)));
        assert!(matches!(p.edit(".github/workflows/ci.yml", cwd), Decision::Ask(_)));
        assert_eq!(p.edit("src/app.ts", cwd), Decision::Allow);
    }

    #[test]
    fn secrets() {
        assert!(is_secret(".env"));
        assert!(is_secret("/p/.env.local"));
        assert!(!is_secret(".env.example"));
        assert!(is_secret("certs/server.pem"));
        assert!(is_secret("/home/u/.ssh/config"));
        assert!(!is_secret("src/env.ts"));
        assert!(is_secret(".env*"));
    }
}
