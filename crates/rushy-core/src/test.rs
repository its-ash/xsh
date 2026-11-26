//! Expressions of `test`, `[ ]`, and `[[ ]]`.

use std::borrow::Cow;
use std::io::IsTerminal as _;
use std::path::Path;

use crate::glob;
use crate::state::Options;

const MAX_DEPTH: usize = 64;

/// One argument of a test expression.
#[derive(Debug, Clone)]
pub(crate) struct Tok<'a> {
    /// Expanded text.
    pub(crate) text: Cow<'a, str>,
    /// Whether the token may act as an operator.
    pub(crate) op: bool,
    /// For `[[`: the text as a pattern (quoted parts escaped).
    pub(crate) pattern: Option<Cow<'a, str>>,
}

impl<'a> Tok<'a> {
    pub(crate) fn plain(text: &'a str) -> Self {
        Self {
            text: Cow::Borrowed(text),
            op: true,
            pattern: None,
        }
    }
}

const UNARY: &[&str] = &[
    "-e", "-f", "-d", "-r", "-w", "-x", "-s", "-L", "-h", "-p", "-S", "-b", "-c", "-g", "-u", "-k",
    "-t", "-z", "-n", "-v", "-a", "-o",
];

const BINARY: &[&str] = &[
    "=", "==", "!=", "<", ">", "-eq", "-ne", "-lt", "-le", "-gt", "-ge", "-nt", "-ot", "-ef", "=~",
];

struct Eval<'a, 't> {
    toks: &'a [Tok<'t>],
    pos: usize,
    dbl: bool,
    cwd: &'a Path,
    options: &'a Options,
    is_set: &'a dyn Fn(&str) -> bool,
    depth: usize,
    skip: u32,
}

/// Evaluates a test expression. `dbl` selects `[[ ]]` rules.
pub(crate) fn eval(
    toks: &[Tok<'_>],
    dbl: bool,
    cwd: &Path,
    options: &Options,
    is_set: &dyn Fn(&str) -> bool,
) -> Result<bool, String> {
    if toks.is_empty() {
        return Ok(false);
    }
    let mut e = Eval {
        toks,
        pos: 0,
        dbl,
        cwd,
        options,
        is_set,
        depth: 0,
        skip: 0,
    };
    let v = e.or()?;
    match e.toks.get(e.pos) {
        None => Ok(v),
        Some(t) => Err(format!("unexpected `{}`", t.text)),
    }
}

impl Eval<'_, '_> {
    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("expression nesting is too deep".into());
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn is_op(&self, i: usize, text: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.op && t.text == text)
    }

    fn or(&mut self) -> Result<bool, String> {
        let op = if self.dbl { "||" } else { "-o" };
        let mut v = self.and()?;
        while self.is_op(self.pos, op) && self.pos + 1 < self.toks.len() {
            self.pos += 1;
            if self.dbl && v {
                self.skip += 1;
                let r = self.and();
                self.skip -= 1;
                r?;
            } else {
                let r = self.and()?;
                v = v || r;
            }
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<bool, String> {
        let op = if self.dbl { "&&" } else { "-a" };
        let mut v = self.not()?;
        while self.is_op(self.pos, op) && self.pos + 1 < self.toks.len() {
            self.pos += 1;
            if self.dbl && !v {
                self.skip += 1;
                let r = self.not();
                self.skip -= 1;
                r?;
            } else {
                let r = self.not()?;
                v = v && r;
            }
        }
        Ok(v)
    }

    fn not(&mut self) -> Result<bool, String> {
        if self.is_op(self.pos, "!") && self.pos + 1 < self.toks.len() {
            self.pos += 1;
            self.enter()?;
            let res = self.not();
            self.leave();
            return Ok(!res?);
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<bool, String> {
        let Some(tok) = self.toks.get(self.pos) else {
            return Err("argument expected".into());
        };
        if tok.op && tok.text == "(" && self.pos + 1 < self.toks.len() {
            let save = self.pos;
            self.pos += 1;
            self.enter()?;
            let v = self.or();
            self.leave();
            let v = v?;
            if self.is_op(self.pos, ")") {
                self.pos += 1;
                return Ok(v);
            }
            if self.dbl {
                return Err("expected `)`".into());
            }
            self.pos = save;
        }
        if let (Some(op), Some(rhs)) = (self.toks.get(self.pos + 1), self.toks.get(self.pos + 2)) {
            if op.op && BINARY.contains(&op.text.as_ref()) && (self.dbl || op.text != "=~") {
                self.pos += 3;
                if self.skip > 0 {
                    return Ok(false);
                }
                return self.binary(tok, op.text.as_ref(), rhs);
            }
        }
        if tok.op && UNARY.contains(&tok.text.as_ref()) {
            if let Some(arg) = self.toks.get(self.pos + 1) {
                self.pos += 2;
                if self.skip > 0 {
                    return Ok(false);
                }
                return self.unary(tok.text.as_ref(), arg.text.as_ref());
            }
        }
        self.pos += 1;
        Ok(self.skip == 0 && !tok.text.is_empty())
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "a and b are the two operands"
    )]
    fn binary(&self, lhs: &Tok<'_>, op: &str, rhs: &Tok<'_>) -> Result<bool, String> {
        let int = |s: &str| {
            s.trim()
                .parse::<i64>()
                .map_err(|_| format!("{s}: integer expression expected"))
        };
        let (a, b) = (lhs.text.as_ref(), rhs.text.as_ref());
        Ok(match op {
            "=" | "==" if self.dbl => glob::matches(rhs.pattern.as_deref().unwrap_or(b), a),
            "!=" if self.dbl => !glob::matches(rhs.pattern.as_deref().unwrap_or(b), a),
            "=" | "==" => a == b,
            "!=" => a != b,
            "<" => a < b,
            ">" => a > b,
            "-eq" => int(a)? == int(b)?,
            "-ne" => int(a)? != int(b)?,
            "-lt" => int(a)? < int(b)?,
            "-le" => int(a)? <= int(b)?,
            "-gt" => int(a)? > int(b)?,
            "-ge" => int(a)? >= int(b)?,
            "-nt" | "-ot" => {
                let m = |p: &str| {
                    std::fs::metadata(self.cwd.join(p))
                        .and_then(|m| m.modified())
                        .ok()
                };
                match (m(a), m(b)) {
                    (Some(x), Some(y)) => (op == "-nt" && x > y) || (op == "-ot" && x < y),
                    (Some(_), None) => op == "-nt",
                    (None, Some(_)) => op == "-ot",
                    (None, None) => false,
                }
            }
            "-ef" => same_file(&self.cwd.join(a), &self.cwd.join(b)),
            "=~" => crate::regex::is_match(b, a)?,
            _ => return Err(format!("{op}: unknown operator")),
        })
    }

    #[allow(clippy::unnecessary_wraps, reason = "same signature as binary")]
    fn unary(&self, op: &str, arg: &str) -> Result<bool, String> {
        let path = self.cwd.join(arg);
        let meta = || std::fs::metadata(&path).ok();
        Ok(match op {
            "-z" => arg.is_empty(),
            "-n" => !arg.is_empty(),
            "-v" => (self.is_set)(arg),
            "-e" | "-a" => meta().is_some(),
            "-f" => meta().is_some_and(|m| m.is_file()),
            "-d" => meta().is_some_and(|m| m.is_dir()),
            "-s" => meta().is_some_and(|m| m.len() > 0),
            "-L" | "-h" => {
                std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink())
            }
            "-r" | "-w" | "-x" => access(&path, op),
            "-t" => match arg.trim() {
                "0" => std::io::stdin().is_terminal(),
                "1" => std::io::stdout().is_terminal(),
                "2" => std::io::stderr().is_terminal(),
                _ => false,
            },
            "-o" => self.options.get(arg).unwrap_or(false),
            _ => meta().is_some_and(|m| special(&m, op)),
        })
    }
}

fn access(path: &Path, op: &str) -> bool {
    use rustix::fs::Access;
    let mode = match op {
        "-r" => Access::READ_OK,
        "-w" => Access::WRITE_OK,
        _ => Access::EXEC_OK,
    };
    rustix::fs::access(path, mode).is_ok()
}

fn special(m: &std::fs::Metadata, op: &str) -> bool {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let t = m.file_type();
    let mode = m.permissions().mode();
    match op {
        "-p" => t.is_fifo(),
        "-S" => t.is_socket(),
        "-b" => t.is_block_device(),
        "-c" => t.is_char_device(),
        "-g" => mode & 0o2000 != 0,
        "-u" => mode & 0o4000 != 0,
        "-k" => mode & 0o1000 != 0,
        _ => false,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<bool, String> {
        let toks: Vec<Tok<'_>> = args.iter().map(|a| Tok::plain(a)).collect();
        eval(&toks, false, Path::new("/"), &Options::default(), &|n| {
            n == "SET"
        })
    }

    #[test]
    fn evaluates_posix_expressions() -> Result<(), String> {
        for (args, want) in [
            (vec![], false),
            (vec!["x"], true),
            (vec![""], false),
            (vec!["-n"], true),
            (vec!["!", "x"], false),
            (vec!["-z", ""], true),
            (vec!["a", "=", "a"], true),
            (vec!["a", "!=", "a"], false),
            (vec!["2", "-lt", "10"], true),
            (vec!["10", "-ge", "10"], true),
            (vec!["-d", "/"], true),
            (vec!["-f", "/"], false),
            (vec!["-e", "/definitely/missing"], false),
            (vec!["(", "a", "=", "b", ")", "-o", "x"], true),
            (vec!["a", "-a", ""], false),
            (vec!["-v", "SET"], true),
            (vec!["b", ">", "a"], true),
        ] {
            assert_eq!(run(&args)?, want, "{args:?}");
        }
        assert!(run(&["a", "-eq", "1"]).is_err());
        Ok(())
    }

    #[test]
    fn double_brackets_match_patterns() -> Result<(), String> {
        let toks = vec![
            Tok::plain("main.rs"),
            Tok::plain("=="),
            Tok {
                text: Cow::Borrowed("*.rs"),
                op: false,
                pattern: Some(Cow::Borrowed("*.rs")),
            },
            Tok::plain("&&"),
            Tok::plain("abc"),
            Tok::plain("=~"),
            Tok {
                text: Cow::Borrowed("^a.c$"),
                op: false,
                pattern: None,
            },
        ];
        assert!(eval(
            &toks,
            true,
            Path::new("/"),
            &Options::default(),
            &|_| false
        )?);
        let quoted = vec![
            Tok::plain("main.rs"),
            Tok::plain("=="),
            Tok {
                text: Cow::Borrowed("*.rs"),
                op: false,
                pattern: Some(Cow::Borrowed("\\*.rs")),
            },
        ];
        assert!(!eval(
            &quoted,
            true,
            Path::new("/"),
            &Options::default(),
            &|_| false
        )?);
        let sc_or = vec![
            Tok::plain("x"),
            Tok::plain("||"),
            Tok::plain("a"),
            Tok::plain("=~"),
            Tok::plain("("),
        ];
        assert!(eval(
            &sc_or,
            true,
            Path::new("/"),
            &Options::default(),
            &|_| false
        )?);
        let sc_and = vec![
            Tok::plain(""),
            Tok::plain("&&"),
            Tok::plain("a"),
            Tok::plain("-eq"),
            Tok::plain("1"),
        ];
        assert!(!eval(
            &sc_and,
            true,
            Path::new("/"),
            &Options::default(),
            &|_| false
        )?);
        let mut opts = Options::default();
        assert!(!eval(
            &[Tok::plain("-o"), Tok::plain("errexit")],
            false,
            Path::new("/"),
            &opts,
            &|_| false
        )?);
        opts.errexit = true;
        assert!(eval(
            &[Tok::plain("-o"), Tok::plain("errexit")],
            false,
            Path::new("/"),
            &opts,
            &|_| false
        )?);
        let mut deep: Vec<Tok<'_>> = (0..100).map(|_| Tok::plain("!")).collect();
        deep.push(Tok::plain("x"));
        assert!(
            eval(&deep, false, Path::new("/"), &Options::default(), &|_| {
                false
            })
            .is_err()
        );
        Ok(())
    }
}
