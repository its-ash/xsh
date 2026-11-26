//! Shell arithmetic: `$(( ))`, `(( ))`, and `for (( ))`.

use crate::parser::is_var_name;
use crate::state::ShellState;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok<'a> {
    Num(i64),
    Name(&'a str),
    Op(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Neg,
    Pos,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Or,
    And,
    BitOr,
    BitXor,
    BitAnd,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Shl,
    Shr,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

/// Pre-parsed arithmetic expression for fast repeated evaluation in loops.
#[derive(Debug, Clone)]
pub(crate) enum Expr {
    Num(i64),
    Var(String),
    Unary(UnaryOp, Box<Self>),
    PreInc(String, i64),
    PostInc(String, i64),
    Bin(BinOp, Box<Self>, Box<Self>),
    Assign(String, Option<BinOp>, Box<Self>),
    Ternary(Box<Self>, Box<Self>, Box<Self>),
    Comma(Box<Self>, Box<Self>),
}

/// Fast path for plain decimal integers (optional leading `-`, no leading 0
/// octal prefix). Returns `None` for hex, octal, `base#`, or overflow.
fn parse_fast_decimal(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let (neg, digits) = if bytes[0] == b'-' {
        (true, &bytes[1..])
    } else {
        (false, bytes)
    };
    if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
        return None;
    }
    let mut acc: i64 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return None;
        }
        acc = acc.checked_mul(10)?.checked_add(i64::from(b - b'0'))?;
    }
    Some(if neg { -acc } else { acc })
}

fn number(text: &str) -> Result<i64, String> {
    if let Some(n) = parse_fast_decimal(text) {
        return Ok(n);
    }
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

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn next_tok(&mut self) -> Result<Option<Tok<'a>>, String> {
        let bytes = self.src.as_bytes();
        while let Some(&b) = bytes.get(self.pos) {
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if !b.is_ascii() {
                let ch = self.src[self.pos..].chars().next().unwrap_or_default();
                if ch.is_whitespace() {
                    self.pos += ch.len_utf8();
                } else {
                    return Err(format!("syntax error near `{ch}`"));
                }
            } else {
                break;
            }
        }
        let Some(&b) = bytes.get(self.pos) else {
            return Ok(None);
        };
        if b.is_ascii_digit() {
            let start = self.pos;
            while bytes
                .get(self.pos)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'#' || *c == b'_')
            {
                self.pos += 1;
            }
            return Ok(Some(Tok::Num(number(&self.src[start..self.pos])?)));
        }
        if b == b'_' || b.is_ascii_alphabetic() {
            let start = self.pos;
            while bytes
                .get(self.pos)
                .is_some_and(|c| *c == b'_' || c.is_ascii_alphanumeric())
            {
                self.pos += 1;
            }
            return Ok(Some(Tok::Name(&self.src[start..self.pos])));
        }
        let b1 = bytes.get(self.pos + 1).copied();
        let b2 = bytes.get(self.pos + 2).copied();
        let op: &'static str = match (b, b1, b2) {
            (b'<', Some(b'<'), Some(b'=')) => "<<=",
            (b'>', Some(b'>'), Some(b'=')) => ">>=",
            (b'*', Some(b'*'), _) => "**",
            (b'+', Some(b'+'), _) => "++",
            (b'-', Some(b'-'), _) => "--",
            (b'<', Some(b'<'), _) => "<<",
            (b'>', Some(b'>'), _) => ">>",
            (b'<', Some(b'='), _) => "<=",
            (b'>', Some(b'='), _) => ">=",
            (b'=', Some(b'='), _) => "==",
            (b'!', Some(b'='), _) => "!=",
            (b'&', Some(b'&'), _) => "&&",
            (b'|', Some(b'|'), _) => "||",
            (b'+', Some(b'='), _) => "+=",
            (b'-', Some(b'='), _) => "-=",
            (b'*', Some(b'='), _) => "*=",
            (b'/', Some(b'='), _) => "/=",
            (b'%', Some(b'='), _) => "%=",
            (b'&', Some(b'='), _) => "&=",
            (b'^', Some(b'='), _) => "^=",
            (b'|', Some(b'='), _) => "|=",
            (b'+', _, _) => "+",
            (b'-', _, _) => "-",
            (b'*', _, _) => "*",
            (b'/', _, _) => "/",
            (b'%', _, _) => "%",
            (b'<', _, _) => "<",
            (b'>', _, _) => ">",
            (b'&', _, _) => "&",
            (b'^', _, _) => "^",
            (b'|', _, _) => "|",
            (b'!', _, _) => "!",
            (b'~', _, _) => "~",
            (b'?', _, _) => "?",
            (b':', _, _) => ":",
            (b'=', _, _) => "=",
            (b'(', _, _) => "(",
            (b')', _, _) => ")",
            (b',', _, _) => ",",
            _ => {
                let c = self.src[self.pos..].chars().next().unwrap_or_default();
                return Err(format!("syntax error near `{c}`"));
            }
        };
        self.pos += op.len();
        Ok(Some(Tok::Op(op)))
    }
}

fn show(t: Tok<'_>) -> String {
    match t {
        Tok::Num(n) => n.to_string(),
        Tok::Name(n) => n.to_owned(),
        Tok::Op(o) => o.to_owned(),
    }
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    cur: Option<Tok<'a>>,
    peek: Option<Tok<'a>>,
    depth: usize,
}

impl Parser<'_> {
    fn advance(&mut self) -> Result<(), String> {
        self.cur = self.peek;
        self.peek = self.lexer.next_tok()?;
        Ok(())
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("expression recursion level exceeded".into());
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn peek_op(&self) -> Option<&'static str> {
        match self.cur {
            Some(Tok::Op(o)) => Some(o),
            _ => None,
        }
    }

    fn eat(&mut self, op: &str) -> Result<bool, String> {
        if self.peek_op() == Some(op) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn comma(&mut self) -> Result<Expr, String> {
        let mut lhs = self.assignment()?;
        while self.eat(",")? {
            let rhs = self.assignment()?;
            lhs = Expr::Comma(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn assignment(&mut self) -> Result<Expr, String> {
        if let (Some(Tok::Name(name)), Some(Tok::Op(op))) = (self.cur, self.peek) {
            let bin = match op {
                "=" => Some(None),
                "+=" => Some(Some(BinOp::Add)),
                "-=" => Some(Some(BinOp::Sub)),
                "*=" => Some(Some(BinOp::Mul)),
                "/=" => Some(Some(BinOp::Div)),
                "%=" => Some(Some(BinOp::Rem)),
                "<<=" => Some(Some(BinOp::Shl)),
                ">>=" => Some(Some(BinOp::Shr)),
                "&=" => Some(Some(BinOp::BitAnd)),
                "^=" => Some(Some(BinOp::BitXor)),
                "|=" => Some(Some(BinOp::BitOr)),
                _ => None,
            };
            if let Some(bin) = bin {
                self.advance()?;
                self.advance()?;
                self.enter()?;
                let rhs = self.assignment();
                self.leave();
                return Ok(Expr::Assign(name.to_owned(), bin, Box::new(rhs?)));
            }
        }
        self.ternary()
    }

    fn ternary(&mut self) -> Result<Expr, String> {
        let c = self.binop(0)?;
        if !self.eat("?")? {
            return Ok(c);
        }
        self.enter()?;
        let a = match self.assignment() {
            Ok(v) => v,
            Err(e) => {
                self.leave();
                return Err(e);
            }
        };
        if !self.eat(":")? {
            self.leave();
            return Err("expected `:`".into());
        }
        let b = self.ternary();
        self.leave();
        Ok(Expr::Ternary(Box::new(c), Box::new(a), Box::new(b?)))
    }

    fn binop(&mut self, level: usize) -> Result<Expr, String> {
        const LEVELS: &[&[(&str, BinOp)]] = &[
            &[("||", BinOp::Or)],
            &[("&&", BinOp::And)],
            &[("|", BinOp::BitOr)],
            &[("^", BinOp::BitXor)],
            &[("&", BinOp::BitAnd)],
            &[("==", BinOp::Eq), ("!=", BinOp::Ne)],
            &[
                ("<", BinOp::Lt),
                ("<=", BinOp::Le),
                (">", BinOp::Gt),
                (">=", BinOp::Ge),
            ],
            &[("<<", BinOp::Shl), (">>", BinOp::Shr)],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Rem)],
        ];
        let Some(ops) = LEVELS.get(level) else {
            return self.power();
        };
        let mut lhs = self.binop(level + 1)?;
        while let Some(op) = self
            .peek_op()
            .and_then(|o| ops.iter().find(|(s, _)| *s == o).map(|(_, b)| *b))
        {
            self.advance()?;
            let rhs = self.binop(level + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn power(&mut self) -> Result<Expr, String> {
        let base = self.unary()?;
        if self.eat("**")? {
            self.enter()?;
            let exp = self.power();
            self.leave();
            return Ok(Expr::Bin(BinOp::Pow, Box::new(base), Box::new(exp?)));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        for (op, delta) in [("++", 1), ("--", -1)] {
            if self.eat(op)? {
                let Some(Tok::Name(name)) = self.cur else {
                    return Err(format!("`{op}` needs a variable"));
                };
                self.advance()?;
                return Ok(Expr::PreInc(name.to_owned(), delta));
            }
        }
        for (sym, op) in [
            ("-", UnaryOp::Neg),
            ("+", UnaryOp::Pos),
            ("!", UnaryOp::Not),
            ("~", UnaryOp::BitNot),
        ] {
            if self.eat(sym)? {
                self.enter()?;
                let v = self.unary();
                self.leave();
                return Ok(Expr::Unary(op, Box::new(v?)));
            }
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        match self.cur {
            Some(Tok::Num(n)) => {
                self.advance()?;
                Ok(Expr::Num(n))
            }
            Some(Tok::Name(name)) => {
                self.advance()?;
                if self.eat("++")? {
                    Ok(Expr::PostInc(name.to_owned(), 1))
                } else if self.eat("--")? {
                    Ok(Expr::PostInc(name.to_owned(), -1))
                } else {
                    Ok(Expr::Var(name.to_owned()))
                }
            }
            Some(Tok::Op("(")) => {
                self.advance()?;
                self.enter()?;
                let v = self.comma();
                self.leave();
                let v = v?;
                if !self.eat(")")? {
                    return Err("expected `)`".into());
                }
                Ok(v)
            }
            Some(t) => Err(format!("syntax error near `{}`", show(t))),
            None => Err("unexpected end of expression".into()),
        }
    }
}

/// Parses an arithmetic expression into a reusable AST.
pub(crate) fn parse(expr: &str) -> Result<Expr, String> {
    parse_depth(expr, 0)
}

fn parse_depth(expr: &str, depth: usize) -> Result<Expr, String> {
    if depth > MAX_DEPTH {
        return Err("expression recursion level exceeded".into());
    }
    let mut lexer = Lexer::new(expr);
    let cur = lexer.next_tok()?;
    if cur.is_none() {
        return Ok(Expr::Num(0));
    }
    let peek = lexer.next_tok()?;
    let mut p = Parser {
        lexer,
        cur,
        peek,
        depth,
    };
    let ast = p.comma()?;
    match p.cur {
        None => Ok(ast),
        Some(t) => Err(format!("syntax error near `{}`", show(t))),
    }
}

fn read_var(name: &str, state: &mut ShellState, depth: usize) -> Result<i64, String> {
    if let Some(raw) = state.var(name) {
        let value = raw.trim();
        if value.is_empty() {
            return Ok(0);
        }
        if let Some(n) = parse_fast_decimal(value) {
            return Ok(n);
        }
        if let Ok(n) = number(value) {
            return Ok(n);
        }
        let owned = value.to_owned();
        return eval_depth(&owned, state, depth + 1);
    }
    if name == "RANDOM"
        && let Some(raw) = state.lookup_cow(name)
        && let Some(n) = parse_fast_decimal(&raw)
    {
        return Ok(n);
    }
    Ok(0)
}

fn assign_var(name: &str, v: i64, state: &mut ShellState) -> Result<i64, String> {
    if !is_var_name(name) {
        return Err(format!("`{name}`: not assignable"));
    }
    if state.is_readonly(name) {
        return Err(format!("{name}: readonly variable"));
    }
    state.set_var_i64(name, v);
    Ok(v)
}

fn apply_bin(op: BinOp, a: i64, b: i64) -> Result<i64, String> {
    Ok(match op {
        BinOp::Add => a.wrapping_add(b),
        BinOp::Sub => a.wrapping_sub(b),
        BinOp::Mul => a.wrapping_mul(b),
        BinOp::Div => {
            if b == 0 {
                return Err("division by 0".to_owned());
            }
            a.wrapping_div(b)
        }
        BinOp::Rem => {
            if b == 0 {
                return Err("division by 0".to_owned());
            }
            a.wrapping_rem(b)
        }
        BinOp::Shl => a.wrapping_shl(u32::try_from(b.rem_euclid(64)).unwrap_or(0)),
        BinOp::Shr => a.wrapping_shr(u32::try_from(b.rem_euclid(64)).unwrap_or(0)),
        BinOp::BitAnd => a & b,
        BinOp::BitXor => a ^ b,
        BinOp::BitOr => a | b,
        BinOp::Eq => i64::from(a == b),
        BinOp::Ne => i64::from(a != b),
        BinOp::Lt => i64::from(a < b),
        BinOp::Le => i64::from(a <= b),
        BinOp::Gt => i64::from(a > b),
        BinOp::Ge => i64::from(a >= b),
        BinOp::Pow => {
            if b < 0 {
                return Err("exponent less than 0".into());
            }
            a.wrapping_pow(u32::try_from(b).unwrap_or(u32::MAX))
        }
        BinOp::Or => i64::from(a != 0 || b != 0),
        BinOp::And => i64::from(a != 0 && b != 0),
    })
}

/// Evaluates a pre-parsed arithmetic expression.
pub(crate) fn eval_expr(expr: &Expr, state: &mut ShellState) -> Result<i64, String> {
    eval_node(expr, state, 0)
}

fn inc_var(
    name: &str,
    delta: i64,
    post: bool,
    state: &mut ShellState,
    depth: usize,
) -> Result<i64, String> {
    if !is_var_name(name) {
        return Err(format!("`{name}`: not assignable"));
    }
    if state.is_readonly(name) {
        return Err(format!("{name}: readonly variable"));
    }
    if name != "RANDOM"
        && let Some(res) = state.try_modify_var_i64(name, parse_fast_decimal, |cur| {
            let next = cur.wrapping_add(delta);
            Ok((next, if post { cur } else { next }))
        })
    {
        return res;
    }
    let cur = read_var(name, state, depth)?;
    let next = cur.wrapping_add(delta);
    assign_var(name, next, state)?;
    Ok(if post { cur } else { next })
}

fn eval_node(expr: &Expr, state: &mut ShellState, depth: usize) -> Result<i64, String> {
    match expr {
        Expr::Num(n) => Ok(*n),
        Expr::Var(name) => read_var(name, state, depth),
        Expr::Unary(op, inner) => {
            let v = eval_node(inner, state, depth)?;
            Ok(match op {
                UnaryOp::Neg => v.wrapping_neg(),
                UnaryOp::Pos => v,
                UnaryOp::Not => i64::from(v == 0),
                UnaryOp::BitNot => !v,
            })
        }
        Expr::PreInc(name, delta) => inc_var(name, *delta, false, state, depth),
        Expr::PostInc(name, delta) => inc_var(name, *delta, true, state, depth),
        Expr::Bin(BinOp::Or, lhs, rhs) => {
            if eval_node(lhs, state, depth)? != 0 {
                return Ok(1);
            }
            Ok(i64::from(eval_node(rhs, state, depth)? != 0))
        }
        Expr::Bin(BinOp::And, lhs, rhs) => {
            if eval_node(lhs, state, depth)? == 0 {
                return Ok(0);
            }
            Ok(i64::from(eval_node(rhs, state, depth)? != 0))
        }
        Expr::Bin(op, lhs, rhs) => {
            let a = eval_node(lhs, state, depth)?;
            let b = eval_node(rhs, state, depth)?;
            apply_bin(*op, a, b)
        }
        Expr::Assign(name, None, rhs) => {
            let v = eval_node(rhs, state, depth)?;
            assign_var(name, v, state)
        }
        Expr::Assign(name, Some(op), rhs) => {
            let rhs_val = eval_node(rhs, state, depth)?;
            if !is_var_name(name) {
                return Err(format!("`{name}`: not assignable"));
            }
            if state.is_readonly(name) {
                return Err(format!("{name}: readonly variable"));
            }
            if name != "RANDOM"
                && let Some(res) = state.try_modify_var_i64(name, parse_fast_decimal, |cur| {
                    let next = apply_bin(*op, cur, rhs_val)?;
                    Ok((next, next))
                })
            {
                return res;
            }
            let cur = read_var(name, state, depth)?;
            let next = apply_bin(*op, cur, rhs_val)?;
            assign_var(name, next, state)
        }
        Expr::Ternary(cond, a, b) => {
            if eval_node(cond, state, depth)? != 0 {
                eval_node(a, state, depth)
            } else {
                eval_node(b, state, depth)
            }
        }
        Expr::Comma(a, b) => {
            let _ = eval_node(a, state, depth)?;
            eval_node(b, state, depth)
        }
    }
}

/// Evaluates an arithmetic expression, reading and assigning shell variables.
pub(crate) fn eval(expr: &str, state: &mut ShellState) -> Result<i64, String> {
    eval_depth(expr, state, 0)
}

fn eval_depth(expr: &str, state: &mut ShellState, depth: usize) -> Result<i64, String> {
    let ast = parse_depth(expr, depth)?;
    eval_node(&ast, state, depth)
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
        assert_eq!(eval("0 && (2 ** -1)", &mut s)?, 0);
        Ok(())
    }

    #[test]
    fn reports_errors() {
        let mut s = st();
        s.set_var("ro", "5");
        s.set_readonly("ro");
        for e in [
            "1 / 0", "1 +", "(1", "2 ** -1", "1 @ 2", "09", "ro = 1", "ro++", "++ro", "x++--",
        ] {
            assert!(eval(e, &mut s).is_err(), "{e}");
        }
        let deep = format!("{}1{}", "(".repeat(100), ")".repeat(100));
        assert!(eval(&deep, &mut s).is_err());
        let unary = format!("{}1", "-".repeat(100));
        assert!(eval(&unary, &mut s).is_err());
        s.set_var("r", "r + 1");
        assert!(eval("r", &mut s).is_err());
    }
}
