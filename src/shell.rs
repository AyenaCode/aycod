//! A small POSIX-shell lexer: enough structure to apply rules to every simple command.
//!
//! It never executes anything. Expansions it cannot resolve statically are kept as text and
//! flagged `dynamic`, so the policy can refuse what it cannot see.

use std::collections::HashMap;

/// One shell word after quote removal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Word {
    pub text: String,
    /// Contains an expansion whose value is unknown (`$X`, `$(...)`, `` `...` ``).
    pub dynamic: bool,
    /// Contains a command substitution (`$(...)`, `` `...` ``, `<(...)`).
    pub substituted: bool,
}

/// A simple command: `NAME=v cmd args > out < in`.
#[derive(Debug, Clone, Default)]
pub struct Simple {
    pub assigns: Vec<(String, Word)>,
    pub words: Vec<Word>,
    /// Output redirection targets (`>`, `>>`, `&>`, `>|`).
    pub writes: Vec<Word>,
    /// Input redirection sources (`<`).
    pub reads: Vec<Word>,
    /// Standard input comes from a pipe, a heredoc, a here-string or a file.
    pub stdin: bool,
    /// Standard input is a pipe from the previous command.
    pub piped: bool,
    /// Body of a heredoc or here-string fed to the command.
    pub heredoc: Option<String>,
    /// The heredoc or here-string holds an expansion.
    pub dynamic_stdin: bool,
}

#[derive(Debug, Default)]
pub struct Script {
    pub commands: Vec<Simple>,
    /// Constructs the lexer refuses to reason about.
    pub opaque: Vec<String>,
}

const KEYWORDS: &[&str] = &["if", "then", "else", "elif", "do", "while", "until", "!", "{", "time"];
const MAX_DEPTH: u8 = 8;

/// Parses `src`. `env` resolves variables (`HOME`, ...); assignments seen along the way extend it.
#[allow(clippy::implicit_hasher)]
pub fn parse(src: &str, env: &HashMap<String, String>) -> Script {
    let mut p = Parser {
        s: src.chars().collect(),
        i: 0,
        env: env.clone(),
        out: Script::default(),
        depth: 0,
        pending: Vec::new(),
    };
    p.run();
    p.out
}

struct Parser {
    s: Vec<char>,
    i: usize,
    env: HashMap<String, String>,
    out: Script,
    depth: u8,
    pending: Vec<Heredoc>,
}

struct Heredoc {
    /// Index of the command in `out.commands`, set when that command is finished.
    cmd: Option<usize>,
    delim: String,
    strip_tabs: bool,
    expands: bool,
}

impl Parser {
    fn peek(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn opaque(&mut self, why: &str) {
        self.out.opaque.push(why.to_owned());
    }

    fn run(&mut self) {
        let mut cmd = Simple::default();
        while let Some(c) = self.peek(0) {
            match c {
                ' ' | '\t' => self.i += 1,
                '\\' if self.peek(1) == Some('\n') => self.i += 2,
                '\n' => {
                    self.i += 1;
                    self.finish(&mut cmd, false);
                    for h in std::mem::take(&mut self.pending) {
                        self.skip_heredoc(&h);
                    }
                }
                '#' => {
                    while self.peek(0).is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                ';' | '(' | ')' => {
                    self.i += 1;
                    self.finish(&mut cmd, false);
                }
                '|' => {
                    self.i += 1;
                    if self.peek(0) == Some('|') {
                        self.i += 1;
                        self.finish(&mut cmd, false);
                    } else {
                        if self.peek(0) == Some('&') {
                            self.i += 1;
                        }
                        self.finish(&mut cmd, true);
                    }
                }
                '&' if self.peek(1) == Some('>') => {
                    self.i += 2;
                    if self.peek(0) == Some('>') {
                        self.i += 1;
                    }
                    let w = self.target();
                    cmd.writes.push(w);
                }
                '&' => {
                    self.i += 1;
                    if self.peek(0) == Some('&') {
                        self.i += 1;
                    }
                    self.finish(&mut cmd, false);
                }
                '<' | '>' => self.redirect(&mut cmd),
                _ => {
                    let w = self.word();
                    // `2>file`: a bare number glued to a redirection is a file descriptor.
                    if matches!(self.peek(0), Some('<' | '>'))
                        && !w.dynamic
                        && !w.text.is_empty()
                        && w.text.bytes().all(|b| b.is_ascii_digit())
                    {
                        continue;
                    }
                    self.push_word(&mut cmd, w);
                }
            }
        }
        self.finish(&mut cmd, false);
        if !self.pending.is_empty() {
            self.opaque("unterminated heredoc");
        }
    }

    fn push_word(&mut self, cmd: &mut Simple, w: Word) {
        if cmd.words.is_empty() {
            if let Some((name, value)) = split_assignment(&w) {
                cmd.assigns.push((name, value));
                return;
            }
            if w.substituted {
                self.opaque("a command substitution produces the command to run");
            } else if w.dynamic {
                self.opaque("a variable produces the command to run");
            }
            if !w.dynamic && KEYWORDS.contains(&w.text.as_str()) {
                return;
            }
        }
        cmd.words.push(w);
    }

    /// Closes the current command; `piped` marks the next one as reading a pipe.
    fn finish(&mut self, cmd: &mut Simple, piped: bool) {
        let done = std::mem::take(cmd);
        cmd.stdin = piped;
        cmd.piped = piped;
        if done.words.is_empty() && done.assigns.is_empty() && done.writes.is_empty() {
            return;
        }
        let index = self.out.commands.len();
        for h in self.pending.iter_mut().filter(|h| h.cmd.is_none()) {
            h.cmd = Some(index);
        }
        // `X=1` alone, or `export X=1`: remember literal values for later expansions.
        let exported = done
            .words
            .first()
            .is_some_and(|w| matches!(w.text.as_str(), "export" | "declare" | "typeset" | "local" | "readonly"));
        if done.words.is_empty() {
            for (k, v) in &done.assigns {
                self.track(k, v);
            }
        } else if exported {
            for w in &done.words[1..] {
                if let Some((k, v)) = split_assignment(w) {
                    self.track(&k, &v);
                }
            }
        }
        self.out.commands.push(done);
    }

    fn track(&mut self, k: &str, v: &Word) {
        if v.dynamic {
            self.env.remove(k);
        } else {
            self.env.insert(k.to_owned(), v.text.clone());
        }
    }

    fn redirect(&mut self, cmd: &mut Simple) {
        let c = self.s[self.i];
        self.i += 1;
        if self.peek(0) == Some('(') {
            // Process substitution: its commands run, the word itself is opaque.
            let inner = self.balanced_from_paren();
            self.sub_parse(&inner);
            let w = Word { text: format!("{c}(...)"), dynamic: true, substituted: true };
            self.push_word(cmd, w);
            return;
        }
        if c == '<' {
            match self.peek(0) {
                Some('<') => {
                    self.i += 1;
                    if self.peek(0) == Some('<') {
                        self.i += 1;
                        let w = self.target();
                        cmd.dynamic_stdin |= w.dynamic;
                        cmd.heredoc = Some(w.text);
                    } else {
                        let strip_tabs = self.peek(0) == Some('-');
                        if strip_tabs {
                            self.i += 1;
                        }
                        let start = self.skip_blanks();
                        let w = self.word();
                        let quoted = self.s[start..self.i].iter().any(|&c| matches!(c, '\'' | '"' | '\\'));
                        self.pending.push(Heredoc { cmd: None, delim: w.text, strip_tabs, expands: !quoted });
                    }
                    cmd.stdin = true;
                }
                Some('&') => {
                    self.i += 1;
                    let _ = self.target();
                }
                Some('>') => {
                    self.i += 1;
                    let w = self.target();
                    cmd.writes.push(w);
                }
                _ => {
                    let w = self.target();
                    cmd.reads.push(w);
                    cmd.stdin = true;
                }
            }
            return;
        }
        // '>' forms: >, >>, >|, >&N, >&file
        let dup = match self.peek(0) {
            Some('>' | '|') => {
                self.i += 1;
                false
            }
            Some('&') => {
                self.i += 1;
                true
            }
            _ => false,
        };
        let w = self.target();
        let is_fd = w.text == "-" || (!w.text.is_empty() && w.text.bytes().all(|b| b.is_ascii_digit()));
        if !(dup && is_fd) {
            cmd.writes.push(w);
        }
    }

    fn skip_blanks(&mut self) -> usize {
        while matches!(self.peek(0), Some(' ' | '\t')) {
            self.i += 1;
        }
        self.i
    }

    fn target(&mut self) -> Word {
        self.skip_blanks();
        self.word()
    }

    fn skip_heredoc(&mut self, h: &Heredoc) {
        let mut body = String::new();
        loop {
            if self.i >= self.s.len() {
                self.opaque("unterminated heredoc");
                return;
            }
            let start = self.i;
            while self.peek(0).is_some_and(|c| c != '\n') {
                self.i += 1;
            }
            let line: String = self.s[start..self.i].iter().collect();
            self.i = (self.i + 1).min(self.s.len());
            let line = if h.strip_tabs { line.trim_start_matches('\t') } else { line.as_str() };
            if line == h.delim {
                if let Some(c) = h.cmd.and_then(|i| self.out.commands.get_mut(i)) {
                    c.dynamic_stdin |= h.expands && body.contains('$');
                    c.heredoc = Some(body);
                }
                return;
            }
            if h.expands && (line.contains("$(") || line.contains('`')) {
                self.opaque("command substitution inside a heredoc");
            }
            body.push_str(line);
            body.push('\n');
        }
    }

    fn is_word_end(c: char) -> bool {
        matches!(c, ' ' | '\t' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')')
    }

    fn word(&mut self) -> Word {
        let mut w = Word::default();
        let start = self.i;
        while let Some(c) = self.peek(0) {
            if Self::is_word_end(c) {
                break;
            }
            match c {
                '\\' => {
                    self.i += 1;
                    match self.peek(0) {
                        Some('\n') => self.i += 1,
                        Some(n) => {
                            w.text.push(n);
                            self.i += 1;
                        }
                        None => {}
                    }
                }
                '\'' => {
                    self.i += 1;
                    loop {
                        match self.peek(0) {
                            None => {
                                self.opaque("unterminated quote");
                                return w;
                            }
                            Some('\'') => {
                                self.i += 1;
                                break;
                            }
                            Some(n) => {
                                w.text.push(n);
                                self.i += 1;
                            }
                        }
                    }
                }
                '"' => {
                    self.i += 1;
                    loop {
                        match self.peek(0) {
                            None => {
                                self.opaque("unterminated quote");
                                return w;
                            }
                            Some('"') => {
                                self.i += 1;
                                break;
                            }
                            Some('\\') => {
                                self.i += 1;
                                match self.peek(0) {
                                    Some(n @ ('$' | '`' | '"' | '\\')) => {
                                        w.text.push(n);
                                        self.i += 1;
                                    }
                                    Some('\n') => self.i += 1,
                                    _ => w.text.push('\\'),
                                }
                            }
                            Some('$') => self.dollar(&mut w),
                            Some('`') => self.backtick(&mut w),
                            Some(n) => {
                                w.text.push(n);
                                self.i += 1;
                            }
                        }
                    }
                }
                '$' => self.dollar(&mut w),
                '`' => self.backtick(&mut w),
                '~' if self.i == start => {
                    self.i += 1;
                    let bare = self.peek(0).is_none_or(|c| c == '/' || Self::is_word_end(c));
                    match self.env.get("HOME") {
                        Some(home) if bare => w.text.push_str(home),
                        _ => w.text.push('~'),
                    }
                }
                _ => {
                    w.text.push(c);
                    self.i += 1;
                }
            }
        }
        w
    }

    fn dollar(&mut self, w: &mut Word) {
        self.i += 1;
        match self.peek(0) {
            Some('(') if self.peek(1) == Some('(') => {
                // Arithmetic: no command runs, but the value is unknown.
                let _ = self.balanced_from_paren();
                w.text.push_str("$((...))");
                w.dynamic = true;
            }
            Some('(') => {
                let inner = self.balanced_from_paren();
                self.sub_parse(&inner);
                w.text.push_str("$(...)");
                w.dynamic = true;
                w.substituted = true;
            }
            Some('{') => {
                self.i += 1;
                let start = self.i;
                while self.peek(0).is_some_and(|c| c != '}') {
                    self.i += 1;
                }
                let inner: String = self.s[start..self.i].iter().collect();
                self.i = (self.i + 1).min(self.s.len());
                self.expand(w, &inner, true);
            }
            Some('\'') => {
                // $'...': ANSI-C quoting; escapes can spell anything.
                self.i += 1;
                let start = self.i;
                while self.peek(0).is_some_and(|c| c != '\'') {
                    if self.peek(0) == Some('\\') {
                        self.i += 1;
                    }
                    self.i += 1;
                }
                let inner: String = self.s[start..self.i.min(self.s.len())].iter().collect();
                self.i = (self.i + 1).min(self.s.len());
                w.dynamic |= inner.contains('\\');
                w.text.push_str(&inner);
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let start = self.i;
                while self.peek(0).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    self.i += 1;
                }
                let name: String = self.s[start..self.i].iter().collect();
                self.expand(w, &name, false);
            }
            Some(c) if c.is_ascii_digit() || matches!(c, '@' | '*' | '#' | '?' | '$' | '!' | '-') => {
                self.i += 1;
                w.text.push('$');
                w.text.push(c);
                w.dynamic = true;
            }
            _ => w.text.push('$'),
        }
    }

    fn expand(&mut self, w: &mut Word, name: &str, braced: bool) {
        let plain = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if plain && let Some(v) = self.env.get(name) {
            w.text.push_str(v);
            return;
        }
        if braced {
            w.text.push_str("${");
            w.text.push_str(name);
            w.text.push('}');
        } else {
            w.text.push('$');
            w.text.push_str(name);
        }
        w.dynamic = true;
    }

    fn backtick(&mut self, w: &mut Word) {
        self.i += 1;
        let mut inner = String::new();
        loop {
            match self.peek(0) {
                None => {
                    self.opaque("unterminated backquote");
                    break;
                }
                Some('`') => {
                    self.i += 1;
                    break;
                }
                Some('\\') => {
                    if let Some(n) = self.peek(1) {
                        inner.push(n);
                    }
                    self.i += 2;
                }
                Some(c) => {
                    inner.push(c);
                    self.i += 1;
                }
            }
        }
        self.sub_parse(&inner);
        w.text.push_str("`...`");
        w.dynamic = true;
        w.substituted = true;
    }

    /// At `(`: returns the text up to the matching `)`, honouring quotes and nesting.
    fn balanced_from_paren(&mut self) -> String {
        self.i += 1;
        let start = self.i;
        let mut depth = 1u32;
        while let Some(c) = self.peek(0) {
            match c {
                '\\' => self.i += 1,
                '\'' => {
                    self.i += 1;
                    while self.peek(0).is_some_and(|c| c != '\'') {
                        self.i += 1;
                    }
                }
                '"' => {
                    self.i += 1;
                    while self.peek(0).is_some_and(|c| c != '"') {
                        if self.peek(0) == Some('\\') {
                            self.i += 1;
                        }
                        self.i += 1;
                    }
                }
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        let inner: String = self.s[start..self.i].iter().collect();
                        self.i += 1;
                        return inner;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        self.opaque("unbalanced parenthesis");
        self.s[start.min(self.s.len())..].iter().collect()
    }

    fn sub_parse(&mut self, src: &str) {
        if self.depth >= MAX_DEPTH {
            self.opaque("nesting too deep");
            return;
        }
        let mut sub = Parser {
            s: src.chars().collect(),
            i: 0,
            env: self.env.clone(),
            out: Script::default(),
            depth: self.depth + 1,
            pending: Vec::new(),
        };
        sub.run();
        self.out.commands.extend(sub.out.commands);
        self.out.opaque.extend(sub.out.opaque);
    }
}

/// `NAME=value` → (`NAME`, value).
pub fn split_assignment(w: &Word) -> Option<(String, Word)> {
    let eq = w.text.find('=')?;
    let name = &w.text[..eq];
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let value = Word { text: w.text[eq + 1..].to_owned(), dynamic: w.dynamic, substituted: w.substituted };
    Some((name.to_owned(), value))
}

pub type TaskCommand = (Vec<(String, String)>, Vec<String>);

/// Splits a task command (`KEY=v prog "arg with space"`) into env and argv, without a shell.
pub fn split_task(src: &str) -> Result<TaskCommand, String> {
    let script = parse(src, &HashMap::new());
    if let Some(why) = script.opaque.first() {
        return Err(format!("{why} in task `{src}`"));
    }
    let [cmd] = script.commands.as_slice() else {
        return Err(format!("task `{src}` must be a single command (wrap it in `sh -c '...'` if it needs a shell)"));
    };
    if !cmd.writes.is_empty() || !cmd.reads.is_empty() || cmd.stdin {
        return Err(format!("task `{src}` uses redirections: wrap it in `sh -c '...'`"));
    }
    if let Some(w) = cmd.words.iter().find(|w| w.dynamic) {
        return Err(format!("task `{src}`: `{}` needs a shell: wrap it in `sh -c '...'`", w.text));
    }
    let env = cmd.assigns.iter().map(|(k, v)| (k.clone(), v.text.clone())).collect();
    let argv: Vec<String> = cmd.words.iter().map(|w| w.text.clone()).collect();
    if argv.is_empty() {
        return Err(format!("task `{src}` has no command"));
    }
    Ok((env, argv))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(src: &str) -> Vec<Vec<String>> {
        parse(src, &HashMap::new()).commands.iter().map(|c| c.words.iter().map(|w| w.text.clone()).collect()).collect()
    }

    #[test]
    fn splits_operators_and_quotes() {
        assert_eq!(
            words(r#"git add -A && git commit -m "a b" ; echo 'x|y' | wc"#),
            vec![vec!["git", "add", "-A"], vec!["git", "commit", "-m", "a b"], vec!["echo", "x|y"], vec!["wc"],]
        );
    }

    #[test]
    fn substitutions_are_parsed_and_flagged() {
        let s = parse("echo $(git push -f) `rm x`", &HashMap::new());
        let names: Vec<_> = s.commands.iter().map(|c| c.words[0].text.as_str()).collect();
        assert_eq!(names, ["git", "rm", "echo"]);
        assert!(s.commands[2].words[1].substituted);
        assert_eq!(parse("$(echo git) push", &HashMap::new()).opaque.len(), 1);
    }

    #[test]
    fn tracks_literal_assignments() {
        let s = parse("F=--force; git push $F", &HashMap::new());
        assert_eq!(s.commands[1].words[2].text, "--force");
        assert!(!s.commands[1].words[2].dynamic);
        let s = parse("git push $UNKNOWN", &HashMap::new());
        assert!(s.commands[0].words[2].dynamic);
    }

    #[test]
    fn redirections() {
        let s = parse("echo x > out.txt 2>&1 < in.txt; cat <<EOF\ngit push -f\nEOF\nls", &HashMap::new());
        assert_eq!(s.commands[0].writes[0].text, "out.txt");
        assert_eq!(s.commands[0].reads[0].text, "in.txt");
        assert_eq!(s.commands.len(), 3, "heredoc body is not a command");
        assert!(s.commands[1].stdin);
    }

    #[test]
    fn task_split() {
        let (env, argv) = split_task("NODE_ENV=test pnpm exec vitest run 'a b'").unwrap();
        assert_eq!(env, [("NODE_ENV".to_owned(), "test".to_owned())]);
        assert_eq!(argv, ["pnpm", "exec", "vitest", "run", "a b"]);
        assert!(split_task("a | b").is_err());
        assert!(split_task("a > b").is_err());
    }
}
