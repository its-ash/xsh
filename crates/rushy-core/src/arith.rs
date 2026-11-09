//! Shell arithmetic: `$(( ))`, `(( ))`, and `for (( ))`.

use crate::parser::is_var_name;
use crate::state::ShellState;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Name(String),
    Op(&'static str),
}

const OPS: &[&str] = &[
    "<<=", ">>=", "**", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=", "-=",
    "*=", "/=", "%=", "&=", "^=", "|=", "+", "-", "*", "/", "%", "<", ">", "&", "^", "|", "!", "~",
    "?", ":", "=", "(", ")", ",",
];

fn number(text: &str) -> Result<i64, String> {
    let bad = || format!("`{text}`: invalid number");
    if let Some((base, digits)) = text.split_once('#') {
        let base: u32 = base.parse().map_err(|_| bad())?;
        if !(2..=36).contains(&base) {
            return Err(bad());
        }
        return i64::from_str_radix(digits, base).map_err(|_| bad());
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return i64::from_str_radix(hex, 16).map_err(|_| bad());
    }
    if text.len() > 1 && text.starts_with('0') {
        return i64::from_str_radix(&text[1..], 8).map_err(|_| bad());
    }
    text.parse().map_err(|_| bad())
}

fn lex(expr: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = expr.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while chars
                .get(i)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '#' || *c == '_')
            {
                i += 1;
            }
            let text: String = chars.get(start..i).unwrap_or_default().iter().collect();
            toks.push(Tok::Num(number(&text)?));
        } else if c == '_' || c.is_ascii_alphabetic() {
            let start = i;
            while chars
                .get(i)
                .is_some_and(|c| *c == '_' || c.is_ascii_alphanumeric())
            {
                i += 1;
            }
            toks.push(Tok::Name(
                chars.get(start..i).unwrap_or_default().iter().collect(),
            ));
        } else {
            let op = OPS
                .iter()
                .find(|op| {
                    op.chars()
                        .enumerate()
                        .all(|(k, o)| chars.get(i + k) == Some(&o))
                })
                .ok_or_else(|| format!("syntax error near `{c}`"))?;
            i += op.len();
            toks.push(Tok::Op(op));
        }
    }
    Ok(toks)
}

struct Eval<'a> {
    toks: Vec<Tok>,
    pos: usize,
    state: &'a mut ShellState,
    skip: u32,
    depth: usize,
}

/// Evaluates an arithmetic expression, reading and assigning shell
/// variables.
pub(crate) fn eval(expr: &str, state: &mut ShellState) -> Result<i64, String> {
    eval_depth(expr, state, 0)
}

fn eval_depth(expr: &str, state: &mut ShellState, depth: usize) -> Result<i64, String> {
    if depth > MAX_DEPTH {
        return Err("expression recursion level exceeded".into());
    }
    let toks = lex(expr)?;
    if toks.is_empty() {
        return Ok(0);
    }
    let mut e = Eval {
        toks,
        pos: 0,
        state,
        skip: 0,
        depth,
    };
    let v = e.comma()?;
    match e.toks.get(e.pos) {
        None => Ok(v),
        Some(t) => Err(format!("syntax error near `{}`", show(t))),
    }
}

fn show(t: &Tok) -> String {
    match t {
        Tok::Num(n) => n.to_string(),
        Tok::Name(n) => n.clone(),
        Tok::Op(o) => (*o).to_owned(),
    }
}

impl Eval<'_> {
    fn peek_op(&self) -> Option<&'static str> {
        match self.toks.get(self.pos) {
            Some(Tok::Op(o)) => Some(o),
            _ => None,
        }
    }

    fn eat(&mut self, op: &str) -> bool {
        if self.peek_op() == Some(op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn var(&mut self, name: &str) -> Result<i64, String> {
        if self.skip > 0 {
            return Ok(0);
        }
        let value = self.state.lookup(name).unwrap_or_default();
        let value = value.trim();
        if value.is_empty() {
            return Ok(0);
        }
        if let Ok(n) = number(value) {
            return Ok(n);
        }
        eval_depth(value, self.state, self.depth + 1)
    }

    fn assign(&mut self, name: &str, v: i64) -> Result<i64, String> {
        if self.skip == 0 {
            if !is_var_name(name) {
                return Err(format!("`{name}`: not assignable"));
            }
            self.state.set_var(name, v.to_string());
        }
        Ok(v)
    }

    fn comma(&mut self) -> Result<i64, String> {
        let mut v = self.assignment()?;
        while self.eat(",") {
            v = self.assignment()?;
        }
        Ok(v)
    }

    fn assignment(&mut self) -> Result<i64, String> {
        if let (Some(Tok::Name(name)), Some(Tok::Op(op))) = (
            self.toks.get(self.pos).cloned(),
            self.toks.get(self.pos + 1).cloned(),
        ) {
            let bin = match op {
                "=" => Some(None),
                "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|=" => {
                    Some(Some(op.trim_end_matches('=')))
                }
                _ => None,
            };
            if let Some(bin) = bin {
                self.pos += 2;
                let rhs = self.assignment()?;
                let v = match bin {
                    None => rhs,
                    Some(b) => {
                        let cur = self.var(&name)?;
                        self.binary(b, cur, rhs)?
                    }
                };
                return self.assign(&name, v);
            }
        }
        self.ternary()
    }

    fn ternary(&mut self) -> Result<i64, String> {
        let c = self.binop(0)?;
        if !self.eat("?") {
            return Ok(c);
        }
        if c == 0 {
            self.skip += 1;
        }
        let a = self.assignment();
        if c == 0 {
            self.skip -= 1;
        }
        let a = a?;
        if !self.eat(":") {
            return Err("expected `:`".into());
        }
        if c != 0 {
            self.skip += 1;
        }
        let b = self.ternary();
        if c != 0 {
            self.skip -= 1;
        }
        let b = b?;
        Ok(if c != 0 { a } else { b })
    }

    fn binop(&mut self, level: usize) -> Result<i64, String> {
        const LEVELS: &[&[&str]] = &[
            &["||"],
            &["&&"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!="],
            &["<", "<=", ">", ">="],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        let Some(ops) = LEVELS.get(level) else {
            return self.power();
        };
        let mut lhs = self.binop(level + 1)?;
        while let Some(op) = self.peek_op().filter(|o| ops.contains(o)) {
            self.pos += 1;
            let short = match op {
                "||" => lhs != 0,
                "&&" => lhs == 0,
                _ => false,
            };
            if short {
                self.skip += 1;
            }
            let rhs = self.binop(level + 1);
            if short {
                self.skip -= 1;
            }
            let rhs = rhs?;
            lhs = match op {
                "||" => i64::from(lhs != 0 || rhs != 0),
                "&&" => i64::from(lhs != 0 && rhs != 0),
                _ => self.binary(op, lhs, rhs)?,
            };
        }
        Ok(lhs)
    }

    fn binary(&self, op: &str, a: i64, b: i64) -> Result<i64, String> {
        let zero = || {
            if b == 0 {
                Err("division by 0".to_owned())
            } else {
                Ok(())
            }
        };
        let shift = u32::try_from(b.rem_euclid(64)).unwrap_or(0);
        Ok(match op {
            "+" => a.wrapping_add(b),
            "-" => a.wrapping_sub(b),
            "*" => a.wrapping_mul(b),
            "/" => {
                if self.skip > 0 {
                    return Ok(0);
                }
                zero()?;
                a.wrapping_div(b)
            }
            "%" => {
                if self.skip > 0 {
                    return Ok(0);
                }
                zero()?;
                a.wrapping_rem(b)
            }
            "<<" => a.wrapping_shl(shift),
            ">>" => a.wrapping_shr(shift),
            "&" => a & b,
            "^" => a ^ b,
            "|" => a | b,
            "==" => i64::from(a == b),
            "!=" => i64::from(a != b),
            "<" => i64::from(a < b),
            "<=" => i64::from(a <= b),
            ">" => i64::from(a > b),
            ">=" => i64::from(a >= b),
            _ => return Err(format!("unknown operator `{op}`")),
        })
    }

    fn power(&mut self) -> Result<i64, String> {
        let base = self.unary()?;
        if self.eat("**") {
            let exp = self.power()?;
            if exp < 0 {
                return Err("exponent less than 0".into());
            }
            let exp = u32::try_from(exp).unwrap_or(u32::MAX);
            return Ok(base.wrapping_pow(exp));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<i64, String> {
        for (op, delta) in [("++", 1), ("--", -1)] {
            if self.eat(op) {
                let Some(Tok::Name(name)) = self.toks.get(self.pos).cloned() else {
                    return Err(format!("`{op}` needs a variable"));
                };
                self.pos += 1;
                let v = self.var(&name)?.wrapping_add(delta);
                return self.assign(&name, v);
            }
        }
        if self.eat("-") {
            return Ok(self.unary()?.wrapping_neg());
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Ok(i64::from(self.unary()? == 0));
        }
        if self.eat("~") {
            return Ok(!self.unary()?);
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<i64, String> {
        match self.toks.get(self.pos).cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(n)
            }
            Some(Tok::Name(name)) => {
                self.pos += 1;
                let v = self.var(&name)?;
                for (op, delta) in [("++", 1), ("--", -1)] {
                    if self.eat(op) {
                        self.assign(&name, v.wrapping_add(delta))?;
                    }
                }
                Ok(v)
            }
            Some(Tok::Op("(")) => {
                self.pos += 1;
                let v = self.comma()?;
                if !self.eat(")") {
                    return Err("expected `)`".into());
                }
                Ok(v)
            }
            Some(t) => Err(format!("syntax error near `{}`", show(&t))),
            None => Err("unexpected end of expression".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> ShellState {
        ShellState::new(std::path::PathBuf::from("/"), Vec::new())
    }

    #[test]
    fn evaluates_operators_with_precedence() -> Result<(), String> {
        let mut s = st();
        for (e, v) in [
            ("1 + 2 * 3", 7),
            ("(1 + 2) * 3", 9),
            ("2 ** 3 ** 2", 512),
            ("7 / 2", 3),
            ("-7 % 3", -1),
            ("1 << 4", 16),
            ("5 > 3 && 2 > 1", 1),
            ("0 || 0", 0),
            ("1 ? 10 : 20", 10),
            ("0 ? 10 : 20", 20),
            ("0x1f + 010 + 2#101", 31 + 8 + 5),
            ("!0 + ~0", 0),
            ("3 == 3, 4 != 4", 0),
            ("", 0),
        ] {
            assert_eq!(eval(e, &mut s)?, v, "{e}");
        }
        Ok(())
    }

    #[test]
    fn reads_and_assigns_variables() -> Result<(), String> {
        let mut s = st();
        s.set_var("x", "5");
        s.set_var("e", "x * 2");
        assert_eq!(eval("x + e", &mut s)?, 15);
        assert_eq!(eval("y = x += 2", &mut s)?, 7);
        assert_eq!(s.var("x"), Some("7"));
        assert_eq!(s.var("y"), Some("7"));
        assert_eq!(eval("x++", &mut s)?, 7);
        assert_eq!(eval("++x", &mut s)?, 9);
        assert_eq!(eval("unset_var + 1", &mut s)?, 1);
        Ok(())
    }

    #[test]
    fn short_circuit_skips_side_effects_and_errors() -> Result<(), String> {
        let mut s = st();
        assert_eq!(eval("0 && (z = 1 / 0)", &mut s)?, 0);
        assert_eq!(s.var("z"), None);
        assert_eq!(eval("1 || z++", &mut s)?, 1);
        assert_eq!(s.var("z"), None);
        Ok(())
    }

    #[test]
    fn reports_errors() {
        let mut s = st();
        for e in ["1 / 0", "1 +", "(1", "2 ** -1", "1 @ 2", "09"] {
            assert!(eval(e, &mut s).is_err(), "{e}");
        }
        s.set_var("r", "r + 1");
        assert!(eval("r", &mut s).is_err());
    }
}
