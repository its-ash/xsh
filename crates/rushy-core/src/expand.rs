//! Word expansion: braces, tilde, parameters, command and arithmetic
//! substitution, field splitting, and file name patterns.

use std::borrow::Cow;

use crate::ast::{List, Param, ParamOp, ReplaceMode, Seg, Word};
use crate::exec::{BoxFut, Ctx};
use crate::glob::{self, Compiled, GlobOpts};
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
        [a, b] => (*a, *b, 1u64),
        [a, b, s] => (*a, *b, s.parse::<i64>().ok()?.unsigned_abs().max(1)),
        _ => return None,
    };
    let to_atoms = |s: String| s.chars().map(Atom::Ch).collect::<Vec<_>>();
    if let (Ok(x), Ok(y)) = (a.parse::<i64>(), b.parse::<i64>()) {
        let count = x.abs_diff(y) / step + 1;
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
        let step_i = i128::from(step);
        let y_i = i128::from(y);
        let mut out = Vec::new();
        let mut v = i128::from(x);
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
            if v == y_i {
                break;
            }
            let next = if x < y {
                v.checked_add(step_i)
            } else {
                v.checked_sub(step_i)
            };
            match next {
                Some(n) if (x < y && n <= y_i) || (x > y && n >= y_i) => v = n,
                _ => break,
            }
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
    let step = usize::try_from(step).ok()?;
    let mut vals: Vec<u32> = if lo <= hi {
        (lo..=hi).step_by(step).collect()
    } else {
        (hi..=lo).rev().step_by(step).collect()
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
    let compiled = Compiled::new(pat);
    if let Some(lit) = compiled.as_literal() {
        return val.strip_prefix(lit).unwrap_or(val).to_owned();
    }
    if val.is_ascii() {
        let found = if longest {
            (0..=val.len())
                .rev()
                .find(|&i| compiled.matches_str(&val[..i], false))
        } else {
            (0..=val.len()).find(|&i| compiled.matches_str(&val[..i], false))
        };
        return found.map_or_else(|| val.to_owned(), |i| val[i..].to_owned());
    }
    let chars: Vec<char> = val.chars().collect();
    let b = bounds(val);
    let found = if longest {
        (0..=chars.len())
            .rev()
            .find(|&i| compiled.matches_chars(&chars[..i], false))
    } else {
        (0..=chars.len()).find(|&i| compiled.matches_chars(&chars[..i], false))
    };
    found.map_or_else(|| val.to_owned(), |i| val[b[i]..].to_owned())
}

fn remove_suffix(val: &str, pat: &str, longest: bool) -> String {
    let compiled = Compiled::new(pat);
    if let Some(lit) = compiled.as_literal() {
        return val.strip_suffix(lit).unwrap_or(val).to_owned();
    }
    if val.is_ascii() {
        let found = if longest {
            (0..=val.len()).find(|&i| compiled.matches_str(&val[i..], false))
        } else {
            (0..=val.len())
                .rev()
                .find(|&i| compiled.matches_str(&val[i..], false))
        };
        return found.map_or_else(|| val.to_owned(), |i| val[..i].to_owned());
    }
    let chars: Vec<char> = val.chars().collect();
    let b = bounds(val);
    let found = if longest {
        (0..=chars.len()).find(|&i| compiled.matches_chars(&chars[i..], false))
    } else {
        (0..=chars.len())
            .rev()
            .find(|&i| compiled.matches_chars(&chars[i..], false))
    };
    found.map_or_else(|| val.to_owned(), |i| val[..b[i]].to_owned())
}

fn replace(val: &str, pat: &str, with: &str, mode: ReplaceMode) -> String {
    if pat.is_empty() {
        return val.to_owned();
    }
    let compiled = Compiled::new(pat);
    if let Some(lit) = compiled.as_literal() {
        if lit.is_empty() {
            return val.to_owned();
        }
        return match mode {
            ReplaceMode::Prefix => val
                .strip_prefix(lit)
                .map_or_else(|| val.to_owned(), |rest| format!("{with}{rest}")),
            ReplaceMode::Suffix => val
                .strip_suffix(lit)
                .map_or_else(|| val.to_owned(), |head| format!("{head}{with}")),
            ReplaceMode::First => val.replacen(lit, with, 1),
            ReplaceMode::All => val.replace(lit, with),
        };
    }
    if val.is_ascii() {
        return match mode {
            ReplaceMode::Prefix => (0..=val.len())
                .rev()
                .find(|&e| compiled.matches_str(&val[..e], false))
                .map_or_else(|| val.to_owned(), |e| format!("{with}{}", &val[e..])),
            ReplaceMode::Suffix => (0..=val.len())
                .find(|&s| compiled.matches_str(&val[s..], false))
                .map_or_else(|| val.to_owned(), |s| format!("{}{with}", &val[..s])),
            ReplaceMode::First | ReplaceMode::All => {
                let mut out = String::with_capacity(val.len());
                let mut k = 0;
                while k < val.len() {
                    let hit = (k + 1..=val.len())
                        .rev()
                        .find(|&e| compiled.matches_str(&val[k..e], false));
                    if let Some(ek) = hit {
                        out.push_str(with);
                        k = ek;
                        if mode == ReplaceMode::First {
                            out.push_str(&val[k..]);
                            return out;
                        }
                    } else {
                        out.push_str(&val[k..=k]);
                        k += 1;
                    }
                }
                out
            }
        };
    }
    let chars: Vec<char> = val.chars().collect();
    let b = bounds(val);
    match mode {
        ReplaceMode::Prefix => (0..=chars.len())
            .rev()
            .find(|&e| compiled.matches_chars(&chars[..e], false))
            .map_or_else(|| val.to_owned(), |e| format!("{with}{}", &val[b[e]..])),
        ReplaceMode::Suffix => (0..=chars.len())
            .find(|&s| compiled.matches_chars(&chars[s..], false))
            .map_or_else(|| val.to_owned(), |s| format!("{}{with}", &val[..b[s]])),
        ReplaceMode::First | ReplaceMode::All => {
            let mut out = String::with_capacity(val.len());
            let mut k = 0;
            while k < chars.len() {
                let hit = (k + 1..=chars.len())
                    .rev()
                    .find(|&e| compiled.matches_chars(&chars[k..e], false));
                if let Some(ek) = hit {
                    out.push_str(with);
                    k = ek;
                    if mode == ReplaceMode::First {
                        out.push_str(&val[b[k]..]);
                        return out;
                    }
                } else {
                    out.push(chars[k]);
                    k += 1;
                }
            }
            out
        }
    }
}

fn unquoted_lit(w: &Word) -> Option<&str> {
    match w.segs.as_slice() {
        [
            Seg::Lit {
                text,
                quoted: false,
            },
        ] => Some(text.as_str()),
        _ => None,
    }
}

fn any_lit(w: &Word) -> Option<&str> {
    match w.segs.as_slice() {
        [Seg::Lit { text, .. }] => Some(text.as_str()),
        _ => None,
    }
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

    fn push_unquoted_field(&self, text: String, out: &mut Vec<String>) -> Exp<()> {
        if self.state.options().noglob || !glob::has_meta(&text) {
            out.push(text);
            return Ok(());
        }
        let mut pat = String::with_capacity(text.len());
        for c in text.chars() {
            if c == '\\' {
                pat.push_str("\\\\");
            } else {
                pat.push(c);
            }
        }
        if !glob::has_meta(&pat) {
            out.push(text);
            return Ok(());
        }
        let matches = glob::expand(&pat, self.state.cwd(), self.glob_opts());
        if !matches.is_empty() {
            out.extend(matches);
        } else if self.state.options().nullglob {
        } else if self.state.options().failglob {
            return Err(format!("xsh: no match: {text}"));
        } else {
            out.push(text);
        }
        Ok(())
    }

    /// Synchronous fast path for expanding `w` into `out` when no brace
    /// expansion, command/process substitution, or word-splitting is needed.
    pub(crate) fn try_expand_fields_into_sync(
        &mut self,
        w: &Word,
        out: &mut Vec<String>,
    ) -> Option<Exp<()>> {
        match w.segs.as_slice() {
            [Seg::Lit { text, quoted: true }] => {
                out.push(text.clone());
                Some(Ok(()))
            }
            [
                Seg::Lit {
                    text,
                    quoted: false,
                },
            ] if !text.as_bytes().contains(&b'{') => {
                Some(self.push_unquoted_field(text.clone(), out))
            }
            [
                Seg::Param {
                    param:
                        Param {
                            name,
                            op: ParamOp::Plain,
                        },
                    quoted,
                },
            ] if name != "@" && name != "*" => {
                let val = self.state.lookup_cow(name);
                if val.is_none() && self.state.options().nounset {
                    return Some(Err(format!("xsh: {name}: unbound variable")));
                }
                if *quoted {
                    out.push(val.map(Cow::into_owned).unwrap_or_default());
                    return Some(Ok(()));
                }
                let Some(val) = val else {
                    return Some(Ok(()));
                };
                let ifs = self.state.var("IFS").unwrap_or(DEFAULT_IFS);
                if !val.chars().any(|c| ifs.contains(c)) {
                    if !val.is_empty() {
                        let s = val.into_owned();
                        return Some(self.push_unquoted_field(s, out));
                    }
                    return Some(Ok(()));
                }
                None
            }
            segs if !segs.is_empty()
                && segs.iter().all(|s| match s {
                    Seg::Lit { quoted: true, .. } | Seg::Arith { quoted: true, .. } => true,
                    Seg::Param {
                        param: Param { name, .. },
                        quoted: true,
                    } => name != "@" && name != "*",
                    _ => false,
                }) =>
            {
                let res = self.try_expand_single_sync(w)?;
                Some(res.map(|s| out.push(s)))
            }
            _ => None,
        }
    }

    /// Appends expanded fields of `w` into `out`, using zero-Box fast paths for
    /// plain literals and simple parameter expansions.
    pub(crate) async fn expand_fields_into(
        &mut self,
        w: &Word,
        out: &mut Vec<String>,
    ) -> Result<Exp<()>> {
        if let Some(res) = self.try_expand_fields_into_sync(w, out) {
            return Ok(res);
        }
        match self.expand_fields(w).await? {
            Ok(fields) => {
                out.extend(fields);
                Ok(Ok(()))
            }
            Err(e) => Ok(Err(e)),
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
                let maybe_glob = !self.state.options().noglob
                    && field
                        .iter()
                        .any(|(c, q)| !q && matches!(c, '*' | '?' | '['));
                if !maybe_glob {
                    out.push(text);
                    continue;
                }
                let mut pat = String::with_capacity(text.len());
                for (c, q) in &field {
                    if *q || *c == '\\' {
                        escape_into(&mut pat, *c);
                    } else {
                        pat.push(*c);
                    }
                }
                if !glob::has_meta(&pat) {
                    out.push(text);
                    continue;
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

    fn check_unset(&self, name: &str, has: bool) -> Exp<()> {
        if !has && self.state.options().nounset && name != "@" && name != "*" {
            Err(format!("xsh: {name}: unbound variable"))
        } else {
            Ok(())
        }
    }

    fn try_expand_slice_sync(
        &mut self,
        name: &str,
        offset: &Word,
        length: Option<&Word>,
        out: &mut String,
    ) -> Option<Exp<()>> {
        let [Seg::Lit { text: off_s, .. }] = offset.segs.as_slice() else {
            return None;
        };
        let len_s = match length {
            None => None,
            Some(w) => match w.segs.as_slice() {
                [Seg::Lit { text, .. }] => Some(text.as_str()),
                _ => return None,
            },
        };
        let has = self.state.lookup_cow(name).is_some();
        if let Err(e) = self.check_unset(name, has) {
            return Some(Err(e));
        }
        let off = match arith::eval(off_s, self.state) {
            Ok(n) => n,
            Err(e) => return Some(Err(format!("xsh: {}: {e}", off_s.trim()))),
        };
        let len_arg = match len_s {
            None => None,
            Some(ls) => match arith::eval(ls, self.state) {
                Ok(n) => Some(n),
                Err(e) => return Some(Err(format!("xsh: {}: {e}", ls.trim()))),
            },
        };
        let val = self.state.lookup_cow(name);
        let v = val.as_deref().unwrap_or_default();
        if !v.is_ascii() {
            return None;
        }
        let len = i64::try_from(v.len()).unwrap_or(i64::MAX);
        let start = if off < 0 {
            len.saturating_add(off).max(0)
        } else {
            off.min(len)
        };
        let end = match len_arg {
            None => len,
            Some(n) if n < 0 => {
                let e = len.saturating_add(n);
                if e < start {
                    return Some(Err(format!("xsh: {name}: substring expression < 0")));
                }
                e
            }
            Some(n) => start.saturating_add(n).min(len),
        };
        let (s, e) = (
            usize::try_from(start).unwrap_or(0),
            usize::try_from(end).unwrap_or(0),
        );
        out.push_str(v.get(s..e.max(s)).unwrap_or_default());
        Some(Ok(()))
    }

    fn try_expand_param_sync(
        &mut self,
        name: &str,
        op: &ParamOp,
        out: &mut String,
    ) -> Option<Exp<()>> {
        match op {
            ParamOp::Plain => {
                let val = match name {
                    "@" | "*" if self.state.positional().is_empty() => None,
                    _ => self.state.lookup_cow(name),
                };
                if let Err(e) = self.check_unset(name, val.is_some()) {
                    return Some(Err(e));
                }
                if let Some(v) = val {
                    out.push_str(&v);
                }
            }
            ParamOp::Length => {
                let val = match name {
                    "@" | "*" if self.state.positional().is_empty() => None,
                    _ => self.state.lookup_cow(name),
                };
                if let Err(e) = self.check_unset(name, val.is_some()) {
                    return Some(Err(e));
                }
                let len = if name == "@" || name == "*" {
                    self.state.positional().len()
                } else if let Some(v) = val {
                    if v.is_ascii() {
                        v.len()
                    } else {
                        v.chars().count()
                    }
                } else {
                    0
                };
                let _ = std::fmt::Write::write_fmt(out, format_args!("{len}"));
            }
            ParamOp::Prefix { longest, pattern } => {
                let pat = unquoted_lit(pattern)?;
                let val = self.state.lookup_cow(name);
                if let Err(e) = self.check_unset(name, val.is_some()) {
                    return Some(Err(e));
                }
                out.push_str(&remove_prefix(
                    val.as_deref().unwrap_or_default(),
                    pat,
                    *longest,
                ));
            }
            ParamOp::Suffix { longest, pattern } => {
                let pat = unquoted_lit(pattern)?;
                let val = self.state.lookup_cow(name);
                if let Err(e) = self.check_unset(name, val.is_some()) {
                    return Some(Err(e));
                }
                out.push_str(&remove_suffix(
                    val.as_deref().unwrap_or_default(),
                    pat,
                    *longest,
                ));
            }
            ParamOp::Replace {
                mode,
                pattern,
                with,
            } => {
                let (pat, rep) = (unquoted_lit(pattern)?, any_lit(with)?);
                let val = self.state.lookup_cow(name);
                if let Err(e) = self.check_unset(name, val.is_some()) {
                    return Some(Err(e));
                }
                out.push_str(&replace(
                    val.as_deref().unwrap_or_default(),
                    pat,
                    rep,
                    *mode,
                ));
            }
            ParamOp::Slice { offset, length } => {
                return self.try_expand_slice_sync(name, offset, length.as_ref(), out);
            }
            _ => return None,
        }
        Some(Ok(()))
    }

    /// Synchronous fast path for `expand_single` when no command/process
    /// substitution or nested word expansion is involved.
    pub(crate) fn try_expand_single_sync(&mut self, w: &Word) -> Option<Exp<String>> {
        if let [Seg::Lit { text, .. }] = w.segs.as_slice() {
            return Some(Ok(text.clone()));
        }
        let mut out = String::new();
        for seg in &w.segs {
            match seg {
                Seg::Lit { text, .. } => out.push_str(text),
                Seg::Tilde(user) => out.push_str(&self.tilde(user.as_deref())),
                Seg::Param {
                    param: Param { name, op },
                    ..
                } => {
                    if let Err(e) = self.try_expand_param_sync(name, op, &mut out)? {
                        return Some(Err(e));
                    }
                }
                Seg::Arith { expr, .. } => {
                    let v = match self.try_arith_word_sync(expr)? {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{v}"));
                }
                _ => return None,
            }
        }
        Some(Ok(out))
    }

    /// Expands a word to one string, with no splitting or patterns.
    pub(crate) async fn expand_single(&mut self, w: &Word) -> Result<Exp<String>> {
        if let Some(res) = self.try_expand_single_sync(w) {
            return Ok(res);
        }
        self.expand_join(w, false).await
    }

    /// Expands a word to a pattern: quoted parts match literally.
    pub(crate) async fn expand_pattern(&mut self, w: &Word) -> Result<Exp<String>> {
        if let [
            Seg::Lit {
                text,
                quoted: false,
            },
        ] = w.segs.as_slice()
        {
            return Ok(Ok(text.clone()));
        }
        self.expand_join(w, true).await
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
                Seg::Arith { expr, .. } => match self.arith_word(expr).await? {
                    Ok(v) => Ok(v.to_string()),
                    Err(e) => Err(e),
                },
                Seg::ProcIn(body) => Ok(self.proc_in(body).await?),
                Seg::ProcOut(body) => Ok(self.proc_out(body)?),
            })
        })
    }

    async fn proc_in(&mut self, body: &List) -> Result<String> {
        let tmp = tempfile::Builder::new().prefix("xsh-").tempfile()?;
        let path = tmp.path().to_path_buf();
        let file = std::sync::Arc::new(tmp.as_file().try_clone()?);
        self.temps.push(tmp);
        let io = crate::exec::Io {
            out: crate::exec::Sink::File(file),
            ..crate::exec::Io::default()
        };
        self.subshell(body, &io).await?;
        Ok(path.display().to_string())
    }

    fn proc_out(&mut self, body: &List) -> Result<String> {
        let tmp = tempfile::Builder::new().prefix("xsh-").tempfile()?;
        let path = tmp.path().to_path_buf();
        self.temps.push(tmp);
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
        let (is_set, is_empty) = {
            let val = match name {
                "@" | "*" if self.state.positional().is_empty() => None,
                _ => self.state.lookup_cow(name),
            };
            (val.is_some(), val.as_deref().is_none_or(str::is_empty))
        };
        let unset = |colon: bool| !is_set || (colon && is_empty);
        let strict = !matches!(
            p.op,
            ParamOp::Default { .. }
                | ParamOp::Assign { .. }
                | ParamOp::Alt { .. }
                | ParamOp::Error { .. }
        );
        if strict && !is_set && self.state.options().nounset && name != "@" && name != "*" {
            return Ok(Err(format!("xsh: {name}: unbound variable")));
        }
        let get_owned = |state: &crate::state::ShellState| -> String {
            match name {
                "@" | "*" if state.positional().is_empty() => String::new(),
                _ => state.lookup(name).unwrap_or_default(),
            }
        };
        Ok(Ok(match &p.op {
            ParamOp::Plain => get_owned(self.state),
            ParamOp::Length => {
                if name == "@" || name == "*" {
                    self.state.positional().len().to_string()
                } else {
                    match self.state.lookup_cow(name) {
                        Some(v) if v.is_ascii() => v.len().to_string(),
                        Some(v) => v.chars().count().to_string(),
                        None => "0".to_owned(),
                    }
                }
            }
            ParamOp::Default { colon, word } => {
                if unset(*colon) {
                    match self.expand_single(word).await? {
                        Ok(s) => s,
                        Err(e) => return Ok(Err(e)),
                    }
                } else {
                    get_owned(self.state)
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
                    if self.state.is_readonly(name) {
                        return Ok(Err(format!("xsh: {name}: readonly variable")));
                    }
                    self.state.set_var_str(name, &s);
                    s
                } else {
                    get_owned(self.state)
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
                get_owned(self.state)
            }
            ParamOp::Prefix { longest, pattern } => match self.expand_pattern(pattern).await? {
                Ok(pat) => {
                    let v = self.state.lookup_cow(name);
                    remove_prefix(v.as_deref().unwrap_or_default(), &pat, *longest)
                }
                Err(e) => return Ok(Err(e)),
            },
            ParamOp::Suffix { longest, pattern } => match self.expand_pattern(pattern).await? {
                Ok(pat) => {
                    let v = self.state.lookup_cow(name);
                    remove_suffix(v.as_deref().unwrap_or_default(), &pat, *longest)
                }
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
                let v = self.state.lookup_cow(name);
                replace(v.as_deref().unwrap_or_default(), &pat, &with, *mode)
            }
            ParamOp::Slice { offset, length } => {
                let off = match self.arith_word(offset).await? {
                    Ok(n) => n,
                    Err(e) => return Ok(Err(e)),
                };
                let len_arg = match length {
                    None => None,
                    Some(l) => match self.arith_word(l).await? {
                        Ok(n) => Some(n),
                        Err(e) => return Ok(Err(e)),
                    },
                };
                let v = self.state.lookup_cow(name);
                let v = v.as_deref().unwrap_or_default();
                if v.is_ascii() {
                    let len = i64::try_from(v.len()).unwrap_or(i64::MAX);
                    let start = if off < 0 {
                        len.saturating_add(off).max(0)
                    } else {
                        off.min(len)
                    };
                    let end = match len_arg {
                        None => len,
                        Some(n) if n < 0 => {
                            let e = len.saturating_add(n);
                            if e < start {
                                return Ok(Err(format!("xsh: {name}: substring expression < 0")));
                            }
                            e
                        }
                        Some(n) => start.saturating_add(n).min(len),
                    };
                    let (s, e) = (
                        usize::try_from(start).unwrap_or(0),
                        usize::try_from(end).unwrap_or(0),
                    );
                    v.get(s..e.max(s)).unwrap_or_default().to_owned()
                } else {
                    let chars: Vec<char> = v.chars().collect();
                    let len = i64::try_from(chars.len()).unwrap_or(i64::MAX);
                    let start = if off < 0 {
                        len.saturating_add(off).max(0)
                    } else {
                        off.min(len)
                    };
                    let end = match len_arg {
                        None => len,
                        Some(n) if n < 0 => {
                            let e = len.saturating_add(n);
                            if e < start {
                                return Ok(Err(format!("xsh: {name}: substring expression < 0")));
                            }
                            e
                        }
                        Some(n) => start.saturating_add(n).min(len),
                    };
                    let (s, e) = (
                        usize::try_from(start).unwrap_or(0),
                        usize::try_from(end).unwrap_or(0),
                    );
                    chars.get(s..e.max(s)).unwrap_or_default().iter().collect()
                }
            }
            ParamOp::Upper { all } => {
                let v = self.state.lookup_cow(name);
                let v = v.as_deref().unwrap_or_default();
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
                let v = self.state.lookup_cow(name);
                let v = v.as_deref().unwrap_or_default();
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

    /// Synchronous fast path for arithmetic words using the pre-parsed cache.
    pub(crate) fn try_arith_word_sync(&mut self, w: &Word) -> Option<Exp<i64>> {
        if let [Seg::Lit { text, .. }] = w.segs.as_slice() {
            return Some(self.eval_arith_cached(text));
        }
        let text = match self.try_expand_single_sync(w)? {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
        };
        Some(arith::eval(&text, self.state).map_err(|e| format!("xsh: {}: {e}", text.trim())))
    }

    pub(crate) async fn arith_word(&mut self, w: &Word) -> Result<Exp<i64>> {
        if let Some(res) = self.try_arith_word_sync(w) {
            return Ok(res);
        }
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

    #[test]
    fn brace_sequence_handles_i64_extremes() {
        let min_step: Vec<Atom<'_>> = format!("0..10..{}", i64::MIN)
            .chars()
            .map(Atom::Ch)
            .collect();
        let res = sequence(&min_step);
        assert_eq!(res.as_ref().map(Vec::len), Some(1));
        let max_range: Vec<Atom<'_>> = format!("{}..{}..2", i64::MAX - 1, i64::MAX)
            .chars()
            .map(Atom::Ch)
            .collect();
        let res = sequence(&max_range);
        assert_eq!(res.as_ref().map(Vec::len), Some(1));
        let min_range: Vec<Atom<'_>> = format!("{}..{}..2", i64::MIN + 1, i64::MIN)
            .chars()
            .map(Atom::Ch)
            .collect();
        let res = sequence(&min_range);
        assert_eq!(res.as_ref().map(Vec::len), Some(1));
    }
}
