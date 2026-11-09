//! Word expansion: braces, tilde, parameters, command and arithmetic
//! substitution, field splitting, and file name patterns.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::ast::{List, Param, ParamOp, ReplaceMode, Seg, Word};
use crate::exec::{BoxFut, Ctx};
use crate::glob::{self, GlobOpts};
use crate::parser::is_var_name;
use crate::{Result, arith};

/// An expansion result; `Err` holds a message for standard error.
pub(crate) type Exp<T> = std::result::Result<T, String>;

const MAX_BRACE: usize = 10_000;
const DEFAULT_IFS: &str = " \t\n";

#[derive(Debug, Clone, Copy)]
enum Atom<'w> {
    Ch(char),
    Q(char),
    Empty,
    Seg(&'w Seg),
}

fn atoms(w: &Word) -> Vec<Atom<'_>> {
    let mut out = Vec::new();
    for seg in &w.segs {
        match seg {
            Seg::Lit {
                text,
                quoted: false,
            } => out.extend(text.chars().map(Atom::Ch)),
            Seg::Lit { text, quoted: true } if text.is_empty() => out.push(Atom::Empty),
            Seg::Lit { text, quoted: true } => out.extend(text.chars().map(Atom::Q)),
            other => out.push(Atom::Seg(other)),
        }
    }
    out
}

fn sequence(content: &[Atom<'_>]) -> Option<Vec<Vec<Atom<'static>>>> {
    let text: String = content
        .iter()
        .map(|a| match a {
            Atom::Ch(c) => Some(*c),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let parts: Vec<&str> = text.split("..").collect();
    let (a, b, step) = match parts.as_slice() {
        [a, b] => (*a, *b, 1i64),
        [a, b, s] => (*a, *b, s.parse::<i64>().ok()?.abs().max(1)),
        _ => return None,
    };
    let to_atoms = |s: String| s.chars().map(Atom::Ch).collect::<Vec<_>>();
    if let (Ok(x), Ok(y)) = (a.parse::<i64>(), b.parse::<i64>()) {
        let count = x.abs_diff(y) / step.unsigned_abs() + 1;
        if count > MAX_BRACE as u64 {
            return None;
        }
        let width = if [a, b].iter().any(|s| {
            s.trim_start_matches('-').len() > 1 && s.trim_start_matches('-').starts_with('0')
        }) {
            a.len().max(b.len())
        } else {
            0
        };
        let mut out = Vec::new();
        let mut v = x;
        loop {
            out.push(to_atoms(if width > 0 {
                if v < 0 {
                    format!("-{:0w$}", v.unsigned_abs(), w = width - 1)
                } else {
                    format!("{v:0width$}")
                }
            } else {
                v.to_string()
            }));
            if v == y || (x < y && v + step > y) || (x > y && v - step < y) {
                break;
            }
            v = if x < y { v + step } else { v - step };
        }
        return Some(out);
    }
    let (mut ca, mut cb) = (a.chars(), b.chars());
    let (Some(x), None, Some(y), None) = (ca.next(), ca.next(), cb.next(), cb.next()) else {
        return None;
    };
    if !x.is_ascii_alphabetic() || !y.is_ascii_alphabetic() {
        return None;
    }
    let (lo, hi) = (u32::from(x), u32::from(y));
    let step = u32::try_from(step).ok()?;
    let mut vals: Vec<u32> = if lo <= hi {
        (lo..=hi).step_by(step as usize).collect()
    } else {
        (hi..=lo).rev().step_by(step as usize).collect()
    };
    vals.retain(|v| char::from_u32(*v).is_some());
    Some(
        vals.into_iter()
            .filter_map(char::from_u32)
            .map(|c| vec![Atom::Ch(c)])
            .collect(),
    )
}

fn brace<'w>(atoms: Vec<Atom<'w>>, budget: &mut usize) -> Vec<Vec<Atom<'w>>> {
    for (i, a) in atoms.iter().enumerate() {
        if !matches!(a, Atom::Ch('{')) {
            continue;
        }
        let mut depth = 0usize;
        let mut commas = Vec::new();
        let mut close = None;
        for (j, b) in atoms.iter().enumerate().skip(i + 1) {
            match b {
                Atom::Ch('{') => depth += 1,
                Atom::Ch('}') if depth == 0 => {
                    close = Some(j);
                    break;
                }
                Atom::Ch('}') => depth -= 1,
                Atom::Ch(',') if depth == 0 => commas.push(j),
                _ => {}
            }
        }
        let Some(j) = close else { continue };
        let alts: Vec<Vec<Atom<'w>>> = if commas.is_empty() {
            match sequence(atoms.get(i + 1..j).unwrap_or_default()) {
                Some(seq) => seq,
                None => continue,
            }
        } else {
            let mut bounds = vec![i];
            bounds.extend(&commas);
            bounds.push(j);
            bounds
                .windows(2)
                .map(|w| atoms.get(w[0] + 1..w[1]).unwrap_or_default().to_vec())
                .collect()
        };
        let mut out = Vec::new();
        for alt in alts {
            if *budget == 0 {
                return vec![atoms];
            }
            *budget -= 1;
            let mut v = atoms.get(..i).unwrap_or_default().to_vec();
            v.extend(alt);
            v.extend_from_slice(atoms.get(j + 1..).unwrap_or_default());
            out.extend(brace(v, budget));
        }
        return out;
    }
    vec![atoms]
}

#[derive(Default)]
struct Fields {
    done: Vec<Vec<(char, bool)>>,
    cur: Vec<(char, bool)>,
    has: bool,
    ifs: String,
}

impl Fields {
    fn lit(&mut self, c: char, quoted: bool) {
        self.cur.push((c, quoted));
        self.has = true;
    }

    fn quoted(&mut self, s: &str) {
        self.cur.extend(s.chars().map(|c| (c, true)));
        self.has = true;
    }

    fn end(&mut self) {
        self.done.push(std::mem::take(&mut self.cur));
        self.has = false;
    }

    fn split(&mut self, s: &str) {
        for c in s.chars() {
            if self.ifs.contains(c) {
                if c.is_whitespace() {
                    if self.has {
                        self.end();
                    }
                } else {
                    self.end();
                }
            } else {
                self.cur.push((c, false));
                self.has = true;
            }
        }
    }

    fn finish(mut self) -> Vec<Vec<(char, bool)>> {
        if self.has || !self.cur.is_empty() {
            self.end();
        }
        self.done
    }
}

fn escape_into(out: &mut String, c: char) {
    if matches!(c, '*' | '?' | '[' | ']' | '\\') {
        out.push('\\');
    }
    out.push(c);
}

/// Splits `s` by `ifs` into at most `max` fields; the last keeps the rest.
pub(crate) fn split_fields(s: &str, ifs: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let is_ws = |c: char| ifs.contains(c) && c.is_whitespace();
    let mut rest = s.trim_start_matches(is_ws);
    while !rest.is_empty() {
        if out.len() + 1 == max {
            out.push(rest.trim_end_matches(is_ws).to_owned());
            return out;
        }
        if let Some(i) = rest.find(|c| ifs.contains(c)) {
            out.push(rest[..i].to_owned());
            let mut tail = &rest[i..];
            tail = tail.trim_start_matches(is_ws);
            if let Some(c) = tail
                .chars()
                .next()
                .filter(|c| ifs.contains(*c) && !c.is_whitespace())
            {
                tail = tail[c.len_utf8()..].trim_start_matches(is_ws);
            }
            rest = tail;
        } else {
            out.push(rest.to_owned());
            break;
        }
    }
    out
}

fn bounds(s: &str) -> Vec<usize> {
    s.char_indices().map(|(i, _)| i).chain([s.len()]).collect()
}

fn remove_prefix(val: &str, pat: &str, longest: bool) -> String {
    let b = bounds(val);
    let found = if longest {
        b.iter().rev().find(|&&i| glob::matches(pat, &val[..i]))
    } else {
        b.iter().find(|&&i| glob::matches(pat, &val[..i]))
    };
    found.map_or_else(|| val.to_owned(), |&i| val[i..].to_owned())
}

fn remove_suffix(val: &str, pat: &str, longest: bool) -> String {
    let b = bounds(val);
    let found = if longest {
        b.iter().find(|&&i| glob::matches(pat, &val[i..]))
    } else {
        b.iter().rev().find(|&&i| glob::matches(pat, &val[i..]))
    };
    found.map_or_else(|| val.to_owned(), |&i| val[..i].to_owned())
}

fn replace(val: &str, pat: &str, with: &str, mode: ReplaceMode) -> String {
    if pat.is_empty() {
        return val.to_owned();
    }
    let b = bounds(val);
    match mode {
        ReplaceMode::Prefix => b
            .iter()
            .rev()
            .find(|&&e| glob::matches(pat, &val[..e]))
            .map_or_else(|| val.to_owned(), |&e| format!("{with}{}", &val[e..])),
        ReplaceMode::Suffix => b
            .iter()
            .find(|&&s| glob::matches(pat, &val[s..]))
            .map_or_else(|| val.to_owned(), |&s| format!("{}{with}", &val[..s])),
        ReplaceMode::First | ReplaceMode::All => {
            let mut out = String::new();
            let mut k = 0;
            while k + 1 < b.len() {
                let i = b[k];
                let hit = b
                    .iter()
                    .enumerate()
                    .skip(k + 1)
                    .rev()
                    .find(|(_, e)| glob::matches(pat, &val[i..**e]));
                if let Some((ek, _)) = hit {
                    out.push_str(with);
                    k = ek;
                    if mode == ReplaceMode::First {
                        out.push_str(&val[b[k]..]);
                        return out;
                    }
                } else {
                    out.push_str(&val[i..b[k + 1]]);
                    k += 1;
                }
            }
            out
        }
    }
}

static TEMP: AtomicU64 = AtomicU64::new(0);

fn temp_path() -> std::path::PathBuf {
    let n = TEMP.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("xsh-{}-{n}", std::process::id()))
}

impl Ctx<'_> {
    fn ifs(&self) -> String {
        self.state.var("IFS").unwrap_or(DEFAULT_IFS).to_owned()
    }

    fn glob_opts(&self) -> GlobOpts {
        let o = self.state.options();
        GlobOpts {
            dotglob: o.dotglob,
            globstar: o.globstar,
            nocase: o.nocaseglob,
        }
    }

    /// Expands a word into fields: braces, splitting, and patterns.
    pub(crate) fn expand_fields<'s>(&'s mut self, w: &'s Word) -> BoxFut<'s, Exp<Vec<String>>> {
        Box::pin(async move {
            let mut budget = MAX_BRACE;
            let mut fields = Vec::new();
            for variant in brace(atoms(w), &mut budget) {
                let mut f = Fields {
                    ifs: self.ifs(),
                    ..Fields::default()
                };
                for atom in variant {
                    match atom {
                        Atom::Ch(c) => f.lit(c, false),
                        Atom::Q(c) => f.lit(c, true),
                        Atom::Empty => f.has = true,
                        Atom::Seg(seg) => {
                            if let Err(e) = self.seg_fields(seg, &mut f).await? {
                                return Ok(Err(e));
                            }
                        }
                    }
                }
                fields.extend(f.finish());
            }
            let mut out = Vec::with_capacity(fields.len());
            for field in fields {
                let text: String = field.iter().map(|(c, _)| *c).collect();
                let active = !self.state.options().noglob
                    && field
                        .iter()
                        .any(|(c, q)| !q && matches!(c, '*' | '?' | '['));
                if !active {
                    out.push(text);
                    continue;
                }
                let mut pat = String::new();
                for (c, q) in &field {
                    if *q || *c == '\\' {
                        escape_into(&mut pat, *c);
                    } else {
                        pat.push(*c);
                    }
                }
                let matches = glob::expand(&pat, self.state.cwd(), self.glob_opts());
                if !matches.is_empty() {
                    out.extend(matches);
                } else if self.state.options().nullglob {
                } else if self.state.options().failglob {
                    return Ok(Err(format!("xsh: no match: {text}")));
                } else {
                    out.push(text);
                }
            }
            Ok(Ok(out))
        })
    }

    async fn seg_fields(&mut self, seg: &Seg, f: &mut Fields) -> Result<Exp<()>> {
        match seg {
            Seg::Param {
                param:
                    Param {
                        name,
                        op: ParamOp::Plain,
                    },
                quoted,
            } if name == "@" || name == "*" => {
                let list = self.state.positional().to_vec();
                if *quoted && name == "@" {
                    for (i, p) in list.iter().enumerate() {
                        if i > 0 {
                            f.end();
                        }
                        f.quoted(p);
                    }
                } else if *quoted {
                    let sep = self
                        .ifs()
                        .chars()
                        .next()
                        .map(String::from)
                        .unwrap_or_default();
                    f.quoted(&list.join(&sep));
                } else {
                    for (i, p) in list.iter().enumerate() {
                        if i > 0 && f.has {
                            f.end();
                        }
                        f.split(p);
                    }
                }
            }
            _ => {
                let quoted = match seg {
                    Seg::Param { quoted, .. }
                    | Seg::Command { quoted, .. }
                    | Seg::Arith { quoted, .. } => *quoted,
                    _ => true,
                };
                let v = match self.seg_value(seg).await? {
                    Ok(v) => v,
                    Err(e) => return Ok(Err(e)),
                };
                if quoted {
                    f.quoted(&v);
                } else {
                    f.split(&v);
                }
            }
        }
        Ok(Ok(()))
    }

    /// Expands a word to one string, with no splitting or patterns.
    pub(crate) fn expand_single<'s>(&'s mut self, w: &'s Word) -> BoxFut<'s, Exp<String>> {
        self.expand_join(w, false)
    }

    /// Expands a word to a pattern: quoted parts match literally.
    pub(crate) fn expand_pattern<'s>(&'s mut self, w: &'s Word) -> BoxFut<'s, Exp<String>> {
        self.expand_join(w, true)
    }

    fn expand_join<'s>(&'s mut self, w: &'s Word, pattern: bool) -> BoxFut<'s, Exp<String>> {
        Box::pin(async move {
            let mut out = String::new();
            for seg in &w.segs {
                match seg {
                    Seg::Lit { text, quoted } => {
                        if pattern && *quoted {
                            text.chars().for_each(|c| escape_into(&mut out, c));
                        } else {
                            out.push_str(text);
                        }
                    }
                    other => {
                        let v = match self.seg_value(other).await? {
                            Ok(v) => v,
                            Err(e) => return Ok(Err(e)),
                        };
                        let quoted = matches!(
                            other,
                            Seg::Param { quoted: true, .. }
                                | Seg::Command { quoted: true, .. }
                                | Seg::Arith { quoted: true, .. }
                                | Seg::Tilde(_)
                        );
                        if pattern && quoted {
                            v.chars().for_each(|c| escape_into(&mut out, c));
                        } else {
                            out.push_str(&v);
                        }
                    }
                }
            }
            Ok(Ok(out))
        })
    }

    fn tilde(&self, user: Option<&str>) -> String {
        let home = self.state.home();
        match (user, home) {
            (None, Some(h)) => h,
            (None, None) => "~".into(),
            (Some(u), home) => {
                if self.state.var("USER") == Some(u) {
                    if let Some(h) = home {
                        return h;
                    }
                }
                home.and_then(|h| {
                    let cand = std::path::Path::new(&h).parent()?.join(u);
                    cand.is_dir().then(|| cand.display().to_string())
                })
                .unwrap_or_else(|| format!("~{u}"))
            }
        }
    }

    fn seg_value<'s>(&'s mut self, seg: &'s Seg) -> BoxFut<'s, Exp<String>> {
        Box::pin(async move {
            Ok(match seg {
                Seg::Lit { text, .. } => Ok(text.clone()),
                Seg::Tilde(user) => Ok(self.tilde(user.as_deref())),
                Seg::Param { param, .. } => self.param(param).await?,
                Seg::Command { body, .. } => Ok(self.capture(body).await?),
                Seg::Arith { expr, .. } => match self.expand_single(expr).await? {
                    Ok(text) => match arith::eval(&text, self.state) {
                        Ok(v) => Ok(v.to_string()),
                        Err(e) => Err(format!("xsh: {}: {e}", text.trim())),
                    },
                    Err(e) => Err(e),
                },
                Seg::ProcIn(body) => Ok(self.proc_in(body).await?),
                Seg::ProcOut(body) => Ok(self.proc_out(body)?),
            })
        })
    }

    async fn proc_in(&mut self, body: &List) -> Result<String> {
        let path = temp_path();
        let file = std::sync::Arc::new(std::fs::File::create(&path)?);
        self.temps.push(path.clone());
        let io = crate::exec::Io {
            out: crate::exec::Sink::File(file),
            ..crate::exec::Io::default()
        };
        self.subshell(body, &io).await?;
        Ok(path.display().to_string())
    }

    fn proc_out(&mut self, body: &List) -> Result<String> {
        let path = temp_path();
        std::fs::File::create(&path)?;
        self.temps.push(path.clone());
        self.after.push((body.clone(), path.clone()));
        Ok(path.display().to_string())
    }

    #[allow(
        clippy::too_many_lines,
        clippy::many_single_char_names,
        reason = "one arm per ${} operator"
    )]
    async fn param(&mut self, p: &Param) -> Result<Exp<String>> {
        let name = p.name.as_str();
        let val = match name {
            "@" | "*" if self.state.positional().is_empty() => None,
            _ => self.state.lookup(name),
        };
        let unset = |colon: bool| val.as_ref().is_none_or(|v| colon && v.is_empty());
        let strict = !matches!(
            p.op,
            ParamOp::Default { .. }
                | ParamOp::Assign { .. }
                | ParamOp::Alt { .. }
                | ParamOp::Error { .. }
        );
        if strict && val.is_none() && self.state.options().nounset && name != "@" && name != "*" {
            return Ok(Err(format!("xsh: {name}: unbound variable")));
        }
        let v = val.clone().unwrap_or_default();
        Ok(Ok(match &p.op {
            ParamOp::Plain => v,
            ParamOp::Length => {
                if name == "@" || name == "*" {
                    self.state.positional().len().to_string()
                } else {
                    v.chars().count().to_string()
                }
            }
            ParamOp::Default { colon, word } => {
                if unset(*colon) {
                    match self.expand_single(word).await? {
                        Ok(s) => s,
                        Err(e) => return Ok(Err(e)),
                    }
                } else {
                    v
                }
            }
            ParamOp::Assign { colon, word } => {
                if unset(*colon) {
                    let s = match self.expand_single(word).await? {
                        Ok(s) => s,
                        Err(e) => return Ok(Err(e)),
                    };
                    if !is_var_name(name) {
                        return Ok(Err(format!("xsh: ${name}: cannot assign in this way")));
                    }
                    self.state.set_var(name, s.clone());
                    s
                } else {
                    v
                }
            }
            ParamOp::Alt { colon, word } => {
                if unset(*colon) {
                    String::new()
                } else {
                    match self.expand_single(word).await? {
                        Ok(s) => s,
                        Err(e) => return Ok(Err(e)),
                    }
                }
            }
            ParamOp::Error { colon, word } => {
                if unset(*colon) {
                    let msg = match self.expand_single(word).await? {
                        Ok(s) => s,
                        Err(e) => return Ok(Err(e)),
                    };
                    let msg = if msg.is_empty() {
                        "parameter null or not set".to_owned()
                    } else {
                        msg
                    };
                    return Ok(Err(format!("xsh: {name}: {msg}")));
                }
                v
            }
            ParamOp::Prefix { longest, pattern } => match self.expand_pattern(pattern).await? {
                Ok(pat) => remove_prefix(&v, &pat, *longest),
                Err(e) => return Ok(Err(e)),
            },
            ParamOp::Suffix { longest, pattern } => match self.expand_pattern(pattern).await? {
                Ok(pat) => remove_suffix(&v, &pat, *longest),
                Err(e) => return Ok(Err(e)),
            },
            ParamOp::Replace {
                mode,
                pattern,
                with,
            } => {
                let pat = match self.expand_pattern(pattern).await? {
                    Ok(p) => p,
                    Err(e) => return Ok(Err(e)),
                };
                let with = match self.expand_single(with).await? {
                    Ok(w) => w,
                    Err(e) => return Ok(Err(e)),
                };
                replace(&v, &pat, &with, *mode)
            }
            ParamOp::Slice { offset, length } => {
                let chars: Vec<char> = v.chars().collect();
                let len = i64::try_from(chars.len()).unwrap_or(i64::MAX);
                let off = match self.arith_word(offset).await? {
                    Ok(n) => n,
                    Err(e) => return Ok(Err(e)),
                };
                let start = if off < 0 {
                    (len + off).max(0)
                } else {
                    off.min(len)
                };
                let end = match length {
                    None => len,
                    Some(l) => match self.arith_word(l).await? {
                        Ok(n) if n < 0 => {
                            let e = len + n;
                            if e < start {
                                return Ok(Err(format!("xsh: {name}: substring expression < 0")));
                            }
                            e
                        }
                        Ok(n) => (start + n).min(len),
                        Err(e) => return Ok(Err(e)),
                    },
                };
                let (s, e) = (
                    usize::try_from(start).unwrap_or(0),
                    usize::try_from(end).unwrap_or(0),
                );
                chars.get(s..e.max(s)).unwrap_or_default().iter().collect()
            }
            ParamOp::Upper { all } => {
                if *all {
                    v.to_uppercase()
                } else {
                    let mut c = v.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect())
                        .unwrap_or_default()
                }
            }
            ParamOp::Lower { all } => {
                if *all {
                    v.to_lowercase()
                } else {
                    let mut c = v.chars();
                    c.next()
                        .map(|f| f.to_lowercase().chain(c).collect())
                        .unwrap_or_default()
                }
            }
        }))
    }

    pub(crate) async fn arith_word(&mut self, w: &Word) -> Result<Exp<i64>> {
        Ok(match self.expand_single(w).await? {
            Ok(text) => {
                arith::eval(&text, self.state).map_err(|e| format!("xsh: {}: {e}", text.trim()))
            }
            Err(e) => Err(e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_fields_for_read() {
        assert_eq!(split_fields("  a b  c  ", " \t\n", 2), ["a", "b  c"]);
        assert_eq!(split_fields("a:b:c", ":", 10), ["a", "b", "c"]);
        assert_eq!(split_fields("a b", " ", 1), ["a b"]);
        assert!(split_fields("   ", " ", 3).is_empty());
    }

    #[test]
    fn trims_and_replaces() {
        assert_eq!(remove_prefix("a/b/c", "*/", false), "b/c");
        assert_eq!(remove_prefix("a/b/c", "*/", true), "c");
        assert_eq!(remove_suffix("x.tar.gz", ".*", false), "x.tar");
        assert_eq!(remove_suffix("x.tar.gz", ".*", true), "x");
        assert_eq!(replace("aXbXc", "X", "-", ReplaceMode::First), "a-bXc");
        assert_eq!(replace("aXbXc", "X", "-", ReplaceMode::All), "a-b-c");
        assert_eq!(replace("abc", "a", "Z", ReplaceMode::Prefix), "Zbc");
        assert_eq!(replace("abc", "c", "Z", ReplaceMode::Suffix), "abZ");
        assert_eq!(replace("abc", "b*", "", ReplaceMode::All), "a");
    }
}
