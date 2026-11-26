//! Shell patterns: `*`, `?`, `[...]`, and `**` file name expansion.
//!
//! Patterns are strings where `\` makes the next character literal.

use std::path::{Path, PathBuf};

/// Options that change pattern matching.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct GlobOpts {
    pub(crate) dotglob: bool,
    pub(crate) globstar: bool,
    pub(crate) nocase: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum P {
    Ch(char),
    Any,
    Star,
    Class { neg: bool, items: Vec<ClassItem> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClassItem {
    Ch(char),
    Range(char, char),
    Named(String),
}

/// A precompiled shell glob pattern for repeated matching.
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    ops: Vec<P>,
    literal: Option<String>,
    ascii_simple: bool,
}

impl Compiled {
    /// Compiles `pat` once for reuse across many candidate strings.
    pub(crate) fn new(pat: &str) -> Self {
        let ops = compile(pat);
        let literal = ops.iter().all(|p| matches!(p, P::Ch(_))).then(|| {
            ops.iter()
                .filter_map(|p| match p {
                    P::Ch(c) => Some(*c),
                    _ => None,
                })
                .collect()
        });
        let ascii_simple = ops.iter().all(|p| match p {
            P::Ch(c) => c.is_ascii(),
            P::Any | P::Star => true,
            P::Class { .. } => false,
        });
        Self {
            ops,
            literal,
            ascii_simple,
        }
    }

    /// Plain literal text if the pattern contains no active wildcards or classes.
    pub(crate) fn as_literal(&self) -> Option<&str> {
        self.literal.as_deref()
    }

    /// Matches against a slice of characters.
    pub(crate) fn matches_chars(&self, text: &[char], nocase: bool) -> bool {
        match_compiled(&self.ops, text, nocase)
    }

    /// Matches against `text`, avoiding `Vec<char>` allocation on ASCII inputs.
    pub(crate) fn matches_str(&self, text: &str, nocase: bool) -> bool {
        if !nocase {
            if let Some(lit) = &self.literal {
                return text == lit;
            }
            if self.ascii_simple && text.is_ascii() {
                return match_compiled_ascii(&self.ops, text.as_bytes());
            }
        }
        let chars: Vec<char> = text.chars().collect();
        match_compiled(&self.ops, &chars, nocase)
    }
}

fn compile(pat: &str) -> Vec<P> {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        i += 1;
        match c {
            '\\' => {
                out.push(P::Ch(chars.get(i).copied().unwrap_or('\\')));
                i += 1;
            }
            '*' => {
                if out.last() != Some(&P::Star) {
                    out.push(P::Star);
                }
            }
            '?' => out.push(P::Any),
            '[' => match class(&chars, i) {
                Some((p, next)) => {
                    out.push(p);
                    i = next;
                }
                None => out.push(P::Ch('[')),
            },
            _ => out.push(P::Ch(c)),
        }
    }
    out
}

fn class(chars: &[char], mut i: usize) -> Option<(P, usize)> {
    let neg = matches!(chars.get(i), Some('!' | '^'));
    if neg {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let c = *chars.get(i)?;
        if c == ']' && !first {
            return Some((P::Class { neg, items }, i + 1));
        }
        first = false;
        if c == '[' && chars.get(i + 1) == Some(&':') {
            let rest: String = chars.get(i + 2..)?.iter().collect();
            if let Some(end) = rest.find(":]") {
                items.push(ClassItem::Named(rest[..end].to_owned()));
                i += 2 + rest[..end].chars().count() + 2;
                continue;
            }
        }
        let c = if c == '\\' {
            i += 1;
            *chars.get(i)?
        } else {
            c
        };
        if chars.get(i + 1) == Some(&'-') && chars.get(i + 2).is_some_and(|e| *e != ']') {
            let end = *chars.get(i + 2)?;
            items.push(ClassItem::Range(c, end));
            i += 3;
        } else {
            items.push(ClassItem::Ch(c));
            i += 1;
        }
    }
}

fn eq(a: char, b: char, nocase: bool) -> bool {
    a == b || (nocase && a.to_lowercase().eq(b.to_lowercase()))
}

fn class_matches(neg: bool, items: &[ClassItem], c: char, nocase: bool) -> bool {
    let hit = items.iter().any(|it| match it {
        ClassItem::Ch(x) => eq(*x, c, nocase),
        ClassItem::Range(a, b) => {
            (*a..=*b).contains(&c)
                || (nocase
                    && ((*a..=*b).contains(&c.to_ascii_lowercase())
                        || (*a..=*b).contains(&c.to_ascii_uppercase())))
        }
        ClassItem::Named(n) => match n.as_str() {
            "alpha" => c.is_alphabetic(),
            "digit" => c.is_ascii_digit(),
            "alnum" => c.is_alphanumeric(),
            "upper" => c.is_uppercase(),
            "lower" => c.is_lowercase(),
            "space" => c.is_whitespace(),
            "blank" => c == ' ' || c == '\t',
            "punct" => c.is_ascii_punctuation(),
            "xdigit" => c.is_ascii_hexdigit(),
            "cntrl" => c.is_control(),
            "print" => !c.is_control(),
            "graph" => !c.is_control() && !c.is_whitespace(),
            _ => false,
        },
    });
    hit != neg
}

fn match_compiled_ascii(pat: &[P], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        let c = text[t];
        match pat.get(p) {
            Some(P::Star) => {
                star = Some((p, t));
                p += 1;
                continue;
            }
            Some(P::Any) => {
                p += 1;
                t += 1;
                continue;
            }
            Some(P::Ch(x)) if *x as u32 == u32::from(c) => {
                p += 1;
                t += 1;
                continue;
            }
            _ => {}
        }
        match star {
            Some((sp, st)) => {
                p = sp + 1;
                t = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    pat.get(p..)
        .unwrap_or_default()
        .iter()
        .all(|x| *x == P::Star)
}

fn match_simple_ascii(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if let Some(&b) = pat.get(p) {
            if b == b'*' {
                while pat.get(p + 1) == Some(&b'*') {
                    p += 1;
                }
                star = Some((p, t));
                p += 1;
                continue;
            }
            if b == b'?' {
                p += 1;
                t += 1;
                continue;
            }
            if b == b'\\' {
                let lit = pat.get(p + 1).copied().unwrap_or(b'\\');
                if text[t] == lit {
                    p += if p + 1 < pat.len() { 2 } else { 1 };
                    t += 1;
                    continue;
                }
            } else if b == text[t] {
                p += 1;
                t += 1;
                continue;
            }
        }
        match star {
            Some((sp, st)) => {
                p = sp + 1;
                t = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    while pat.get(p) == Some(&b'*') {
        p += 1;
    }
    p == pat.len()
}

fn match_compiled(pat: &[P], text: &[char], nocase: bool) -> bool {
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        let c = text.get(t).copied().unwrap_or_default();
        match pat.get(p) {
            Some(P::Star) => {
                star = Some((p, t));
                p += 1;
                continue;
            }
            Some(P::Any) => {
                p += 1;
                t += 1;
                continue;
            }
            Some(P::Ch(x)) if eq(*x, c, nocase) => {
                p += 1;
                t += 1;
                continue;
            }
            Some(P::Class { neg, items }) if class_matches(*neg, items, c, nocase) => {
                p += 1;
                t += 1;
                continue;
            }
            _ => {}
        }
        match star {
            Some((sp, st)) => {
                p = sp + 1;
                t = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    pat.get(p..)
        .unwrap_or_default()
        .iter()
        .all(|x| *x == P::Star)
}

/// Whether `text` matches the whole pattern `pat`.
pub(crate) fn matches(pat: &str, text: &str) -> bool {
    matches_opt(pat, text, false)
}

pub(crate) fn matches_opt(pat: &str, text: &str, nocase: bool) -> bool {
    if !nocase && pat.is_ascii() && text.is_ascii() && !pat.as_bytes().contains(&b'[') {
        return match_simple_ascii(pat.as_bytes(), text.as_bytes());
    }
    let text: Vec<char> = text.chars().collect();
    match_compiled(&compile(pat), &text, nocase)
}

/// Whether a `[` starting at `rest` (the bytes right after `[`) has a valid closing `]`.
fn has_closed_class(bytes: &[u8]) -> bool {
    let mut i = 0;
    if matches!(bytes.get(i), Some(b'!' | b'^')) {
        i += 1;
    }
    let mut first = true;
    while let Some(&b) = bytes.get(i) {
        if b == b']' && !first {
            return true;
        }
        first = false;
        if b == b'['
            && bytes.get(i + 1) == Some(&b':')
            && let Some(tail) = bytes.get(i + 2..)
            && let Some(end) = tail.windows(2).position(|w| w == b":]")
        {
            i += 2 + end + 2;
            continue;
        }
        if b == b'\\' {
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// Whether `pat` has an active unescaped `*`, `?`, or closed `[...]` class.
pub(crate) fn has_meta(pat: &str) -> bool {
    let bytes = pat.as_bytes();
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\\' => i += 2,
            b'*' | b'?' => return true,
            b'[' => {
                if has_closed_class(bytes.get(i + 1..).unwrap_or_default()) {
                    return true;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    false
}

/// Removes the escaping backslashes from a pattern.
pub(crate) fn unescape(pat: &str) -> String {
    let mut out = String::with_capacity(pat.len());
    let mut esc = false;
    for c in pat.chars() {
        if esc || c != '\\' {
            out.push(c);
            esc = false;
        } else {
            esc = true;
        }
    }
    out
}

fn split_components(pat: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut esc = false;
    for c in pat.chars() {
        if !esc && c == '/' {
            out.push(String::new());
            continue;
        }
        esc = !esc && c == '\\';
        if let Some(last) = out.last_mut() {
            last.push(c);
        }
    }
    out
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else if prefix.ends_with('/') {
        format!("{prefix}{name}")
    } else {
        format!("{prefix}/{name}")
    }
}

fn entries(dir: &Path) -> Vec<(String, bool)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.filter_map(|e| {
        let e = e.ok()?;
        let name = e.file_name().into_string().ok()?;
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        Some((name, is_dir))
    })
    .collect()
}

fn visible(name: &str, pat: &str, opts: GlobOpts) -> bool {
    !name.starts_with('.') || pat.starts_with('.') || pat.starts_with("\\.") || opts.dotglob
}

/// Expands a file name pattern against the file system. Returns the
/// matches in sorted order, or nothing.
pub(crate) fn expand(pat: &str, cwd: &Path, opts: GlobOpts) -> Vec<String> {
    let comps = split_components(pat);
    let (mut paths, rest): (Vec<String>, &[String]) = match comps.split_first() {
        Some((first, rest)) if first.is_empty() => (vec!["/".to_owned()], rest),
        _ => (vec![String::new()], comps.as_slice()),
    };
    for (i, comp) in rest.iter().enumerate() {
        let last = i + 1 == rest.len();
        let mut next = Vec::new();
        let compiled = (!(comp.is_empty() || comp == "**" && opts.globstar) && has_meta(comp))
            .then(|| Compiled::new(comp));
        for prefix in &paths {
            let dir = if prefix.is_empty() {
                cwd.to_path_buf()
            } else {
                cwd.join(prefix)
            };
            if comp.is_empty() {
                if last {
                    next.push(format!("{prefix}/"));
                }
                continue;
            }
            if comp == "**" && opts.globstar {
                walk(prefix, &dir, opts, &mut next, last);
                continue;
            }
            let Some(compiled) = &compiled else {
                let name = unescape(comp);
                let p = join(prefix, &name);
                if std::fs::symlink_metadata(cwd.join(&p)).is_ok() {
                    next.push(p);
                }
                continue;
            };
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let Ok(name) = e.file_name().into_string() else {
                    continue;
                };
                if !visible(&name, comp, opts) {
                    continue;
                }
                if !last && !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                if compiled.matches_str(&name, opts.nocase) {
                    next.push(join(prefix, &name));
                }
            }
        }
        paths = next;
        if paths.is_empty() {
            break;
        }
    }
    paths.retain(|p| !p.is_empty());
    paths.sort();
    paths.dedup();
    paths
}

fn walk(prefix: &str, dir: &Path, opts: GlobOpts, out: &mut Vec<String>, last: bool) {
    if !last || !prefix.is_empty() {
        out.push(prefix.to_owned());
    }
    let mut stack: Vec<(String, PathBuf)> = vec![(prefix.to_owned(), dir.to_path_buf())];
    while let Some((pre, d)) = stack.pop() {
        for (name, is_dir) in entries(&d) {
            if name.starts_with('.') && !opts.dotglob {
                continue;
            }
            let p = join(&pre, &name);
            if is_dir {
                stack.push((p.clone(), d.join(&name)));
                out.push(p);
            } else if last {
                out.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_wildcards_and_classes() {
        for (p, t, ok) in [
            ("*.rs", "main.rs", true),
            ("*.rs", "main.rc", false),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("[abc]x", "bx", true),
            ("[!abc]x", "bx", false),
            ("[a-c]*", "cat", true),
            ("[[:digit:]][[:alpha:]]", "1z", true),
            ("\\*", "*", true),
            ("\\*", "a", false),
            ("*", "", true),
            ("a*b*c", "aXXbYYc", true),
            ("a*b*c", "aXXbYY", false),
            ("[]]", "]", true),
            ("[", "[", true),
        ] {
            assert_eq!(matches(p, t), ok, "{p} {t}");
        }
        assert!(matches_opt("*.RS", "a.rs", true));
        assert!(has_meta("a*"));
        assert!(!has_meta("a\\*"));
        assert!(!has_meta("["));
        assert!(!has_meta("[abc"));
        assert!(has_meta("[a]"));
        assert!(has_meta("[]a]"));
        assert_eq!(unescape("a\\*b"), "a*b");
    }

    #[test]
    fn expands_files() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/sub"))?;
        for f in [
            "a.rs",
            "b.rs",
            "c.txt",
            ".hidden.rs",
            "src/x.rs",
            "src/sub/y.rs",
        ] {
            std::fs::write(root.join(f), "")?;
        }
        let o = GlobOpts {
            globstar: true,
            ..GlobOpts::default()
        };
        assert_eq!(expand("*.rs", root, o), ["a.rs", "b.rs"]);
        assert_eq!(expand("src/*.rs", root, o), ["src/x.rs"]);
        assert_eq!(expand("*/", root, o), ["src/"]);
        assert_eq!(
            expand("**/*.rs", root, o),
            ["a.rs", "b.rs", "src/sub/y.rs", "src/x.rs"]
        );
        assert!(expand("*.none", root, o).is_empty());
        let dot = GlobOpts { dotglob: true, ..o };
        assert_eq!(expand("*.rs", root, dot).len(), 3);
        assert_eq!(expand(".*.rs", root, o), [".hidden.rs"]);
        Ok(())
    }
}
