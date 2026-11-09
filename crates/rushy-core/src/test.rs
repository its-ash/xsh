//! Expressions of `test`, `[ ]`, and `[[ ]]`.

use std::io::IsTerminal as _;
use std::path::Path;

use crate::glob;

/// One argument of a test expression.
#[derive(Debug, Clone)]
pub(crate) struct Tok {
    /// Expanded text.
    pub(crate) text: String,
    /// Whether the token may act as an operator.
    pub(crate) op: bool,
    /// For `[[`: the text as a pattern (quoted parts escaped).
    pub(crate) pattern: Option<String>,
}

impl Tok {
    pub(crate) fn plain(text: &str) -> Self {
        Self {
            text: text.to_owned(),
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

struct Eval<'a> {
    toks: &'a [Tok],
    pos: usize,
    dbl: bool,
    cwd: &'a Path,
    is_set: &'a dyn Fn(&str) -> bool,
}

/// Evaluates a test expression. `dbl` selects `[[ ]]` rules.
pub(crate) fn eval(
    toks: &[Tok],
    dbl: bool,
    cwd: &Path,
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
        is_set,
    };
    let v = e.or()?;
    match e.toks.get(e.pos) {
        None => Ok(v),
        Some(t) => Err(format!("unexpected `{}`", t.text)),
    }
}

impl Eval<'_> {
    fn is_op(&self, i: usize, text: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.op && t.text == text)
    }

    fn or(&mut self) -> Result<bool, String> {
        let op = if self.dbl { "||" } else { "-o" };
        let mut v = self.and()?;
        while self.is_op(self.pos, op) && self.pos + 1 < self.toks.len() {
            self.pos += 1;
            let r = self.and()?;
            v = v || r;
        }
        Ok(v)
    }

    fn and(&mut self) -> Result<bool, String> {
        let op = if self.dbl { "&&" } else { "-a" };
        let mut v = self.not()?;
        while self.is_op(self.pos, op) && self.pos + 1 < self.toks.len() {
            self.pos += 1;
            let r = self.not()?;
            v = v && r;
        }
        Ok(v)
    }

    fn not(&mut self) -> Result<bool, String> {
        if self.is_op(self.pos, "!") && self.pos + 1 < self.toks.len() {
            self.pos += 1;
            return Ok(!self.not()?);
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
            let v = self.or()?;
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
            if op.op && BINARY.contains(&op.text.as_str()) && (self.dbl || op.text != "=~") {
                let (lhs, op, rhs) = (tok.clone(), op.text.clone(), rhs.clone());
                self.pos += 3;
                return self.binary(&lhs, &op, &rhs);
            }
        }
        if tok.op && UNARY.contains(&tok.text.as_str()) {
            if let Some(arg) = self.toks.get(self.pos + 1) {
                let (op, arg) = (tok.text.clone(), arg.text.clone());
                self.pos += 2;
                return self.unary(&op, &arg);
            }
        }
        self.pos += 1;
        Ok(!tok.text.is_empty())
    }

    #[allow(
        clippy::many_single_char_names,
        reason = "a and b are the two operands"
    )]
    fn binary(&self, lhs: &Tok, op: &str, rhs: &Tok) -> Result<bool, String> {
        let int = |s: &str| {
            s.trim()
                .parse::<i64>()
                .map_err(|_| format!("{s}: integer expression expected"))
        };
        let (a, b) = (lhs.text.as_str(), rhs.text.as_str());
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
            "-r" | "-w" | "-x" => meta().is_some_and(|m| access(&m, op)),
            "-t" => match arg.trim() {
                "0" => std::io::stdin().is_terminal(),
                "1" => std::io::stdout().is_terminal(),
                "2" => std::io::stderr().is_terminal(),
                _ => false,
            },
            "-o" => crate::state::Options::default().get(arg).is_some(),
            _ => meta().is_some_and(|m| special(&m, op)),
        })
    }
}

fn access(m: &std::fs::Metadata, op: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let mode = m.permissions().mode();
    match op {
        "-r" => mode & 0o444 != 0,
        "-w" => mode & 0o222 != 0 && !m.permissions().readonly(),
        _ => mode & 0o111 != 0,
    }
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
        let toks: Vec<Tok> = args.iter().map(|a| Tok::plain(a)).collect();
        eval(&toks, false, Path::new("/"), &|n| n == "SET")
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
                text: "*.rs".into(),
                op: false,
                pattern: Some("*.rs".into()),
            },
            Tok::plain("&&"),
            Tok::plain("abc"),
            Tok::plain("=~"),
            Tok {
                text: "^a.c$".into(),
                op: false,
                pattern: None,
            },
        ];
        assert!(eval(&toks, true, Path::new("/"), &|_| false)?);
        let quoted = vec![
            Tok::plain("main.rs"),
            Tok::plain("=="),
            Tok {
                text: "*.rs".into(),
                op: false,
                pattern: Some("\\*.rs".into()),
            },
        ];
        assert!(!eval(&quoted, true, Path::new("/"), &|_| false)?);
        Ok(())
    }
}
