//! Quote-aware parser for command lines.
//!
//! [`parse_program`] builds the full syntax tree ([`crate::ast`]): lists,
//! `&&`/`||`, `!`, pipelines, subshells, groups, `if`, `for`, `while`,
//! `until`, `case`, `(( ))`, `[[ ]]`, functions, every redirection form
//! (including here-documents), command, arithmetic, and process
//! substitution, and `${...}` operators. Aliases expand at command
//! positions.
//!
//! [`parse`] is the older flat view: plain pipelines of simple commands.
//! It rejects any construct that the flat view cannot show, so callers that
//! check commands (such as the AI safety guard) never miss a hidden one.

use std::collections::BTreeMap;

use crate::ast::{
    AndOr, Assign, CaseArm, CaseEnd, Command, Compound, Connector, Item, List, Param, ParamOp,
    Pipe, Redir, RedirOp, ReplaceMode, Seg, Simple,
};
use crate::{Error, Result};

const MAX_ALIAS_EXPANSIONS: usize = 64;
const MAX_DEPTH: usize = 200;

/// One piece of a word, before expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPart {
    /// Literal text with quotes already removed.
    Literal(String),
    /// A variable reference: `$NAME`, `${NAME}`, `$?`, `$$`, `$1`, or `$@`.
    Var(String),
    /// An unquoted leading `~`, expanded to the home directory.
    Tilde,
}

/// A shell word made of literal and variable parts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Word {
    /// Parts in source order.
    pub parts: Vec<WordPart>,
    /// Whether any part of the word was quoted or escaped.
    pub quoted: bool,
}

impl Word {
    /// Builds an unquoted literal word.
    #[must_use]
    pub fn literal(text: &str) -> Self {
        Self {
            parts: vec![WordPart::Literal(text.to_owned())],
            quoted: false,
        }
    }

    /// Returns the text if the word is a single unquoted literal.
    #[must_use]
    pub fn as_plain(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [WordPart::Literal(s)] if !self.quoted => Some(s),
            _ => None,
        }
    }
}

/// Kind of I/O redirection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    /// `< file`: read standard input from a file.
    In,
    /// `> file`: truncate a file and write standard output to it.
    Out,
    /// `>> file`: append standard output to a file.
    Append,
}

impl RedirectKind {
    /// Operator text for this redirection.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::In => "<",
            Self::Out => ">",
            Self::Append => ">>",
        }
    }
}

/// A redirection attached to a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    /// Redirection operator.
    pub kind: RedirectKind,
    /// Target file name, before expansion.
    pub target: Word,
}

/// A command with its arguments and redirections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimpleCommand {
    /// Command name and arguments, before expansion.
    pub words: Vec<Word>,
    /// Redirections in source order.
    pub redirects: Vec<Redirect>,
}

impl SimpleCommand {
    /// Whether the command has no words and no redirections.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.redirects.is_empty()
    }
}

/// Commands joined by `|`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    /// Commands in pipeline order.
    pub commands: Vec<SimpleCommand>,
    /// Whether the pipeline ends with `&`.
    pub background: bool,
}

/// Reserved words that end a list.
const CLOSERS: &[&str] = &["then", "else", "elif", "fi", "do", "done", "esac", "}"];

/// Parses a command line into flat pipelines, expanding aliases at command
/// positions.
///
/// # Errors
///
/// Returns [`Error::Incomplete`] for input that needs more lines (an
/// unclosed quote, a trailing `\` or `|`, `if` without `fi`, an open
/// here-document), and [`Error::Parse`] for malformed input or for syntax
/// that flat pipelines cannot show (`&&`, `||`, compound commands,
/// functions, command substitution, `${...}` operators, and redirections
/// other than `<`, `>`, `>>`).
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// let pipelines = rushy_core::parser::parse("echo 'a b' | wc -c", &BTreeMap::new())?;
/// assert_eq!(pipelines[0].commands.len(), 2);
/// # Ok::<(), rushy_core::Error>(())
/// ```
pub fn parse(input: &str, aliases: &BTreeMap<String, String>) -> Result<Vec<Pipeline>> {
    let list = parse_program(input, aliases)?;
    let mut out = Vec::with_capacity(list.0.len());
    for item in list.0 {
        if !item.and_or.rest.is_empty() {
            return Err(unsupported("`&&` and `||`"));
        }
        let pipe = item.and_or.first;
        if pipe.negated {
            return Err(unsupported("`!`"));
        }
        let commands = pipe
            .commands
            .into_iter()
            .map(flat_command)
            .collect::<Result<Vec<_>>>()?;
        out.push(Pipeline {
            commands,
            background: item.background,
        });
    }
    Ok(out)
}

fn unsupported(what: &str) -> Error {
    Error::Parse(format!("{what} is not supported in a plain pipeline"))
}

fn flat_command(cmd: Command) -> Result<SimpleCommand> {
    let Command::Simple(s) = cmd else {
        return Err(unsupported("a compound command"));
    };
    let mut words = Vec::with_capacity(s.assigns.len() + s.words.len());
    for a in s.assigns {
        let op = if a.append { "+=" } else { "=" };
        let mut w = flat_word(&a.value)?;
        w.parts
            .insert(0, WordPart::Literal(format!("{}{op}", a.name)));
        merge_literals(&mut w.parts);
        words.push(w);
    }
    for w in &s.words {
        words.push(flat_word(w)?);
    }
    let mut redirects = Vec::with_capacity(s.redirects.len());
    for r in s.redirects {
        if r.fd.is_some() {
            return Err(unsupported("a numbered redirection"));
        }
        let (kind, target) = match r.op {
            RedirOp::In(t) => (RedirectKind::In, t),
            RedirOp::Out {
                target,
                clobber: false,
            } => (RedirectKind::Out, target),
            RedirOp::Append(t) => (RedirectKind::Append, t),
            _ => return Err(unsupported("this redirection")),
        };
        redirects.push(Redirect {
            kind,
            target: flat_word(&target)?,
        });
    }
    Ok(SimpleCommand { words, redirects })
}

fn flat_word(word: &crate::ast::Word) -> Result<Word> {
    let mut parts = Vec::with_capacity(word.segs.len());
    for seg in &word.segs {
        parts.push(match seg {
            Seg::Lit { text, .. } => WordPart::Literal(text.clone()),
            Seg::Tilde(None) => WordPart::Tilde,
            Seg::Tilde(Some(user)) => WordPart::Literal(format!("~{user}")),
            Seg::Param {
                param:
                    Param {
                        name,
                        op: ParamOp::Plain,
                    },
                ..
            } => WordPart::Var(name.clone()),
            Seg::Param { .. } => return Err(unsupported("a `${...}` operator")),
            Seg::Command { .. } => return Err(unsupported("command substitution")),
            Seg::Arith { .. } => return Err(unsupported("arithmetic expansion")),
            Seg::ProcIn(_) | Seg::ProcOut(_) => {
                return Err(unsupported("process substitution"));
            }
        });
    }
    merge_literals(&mut parts);
    if parts.len() > 1 {
        parts.retain(|p| !matches!(p, WordPart::Literal(s) if s.is_empty()));
    }
    Ok(Word {
        parts,
        quoted: word.is_quoted(),
    })
}

fn merge_literals(parts: &mut Vec<WordPart>) {
    let mut out: Vec<WordPart> = Vec::with_capacity(parts.len());
    for p in parts.drain(..) {
        match (out.last_mut(), p) {
            (Some(WordPart::Literal(a)), WordPart::Literal(b)) => a.push_str(&b),
            (_, p) => out.push(p),
        }
    }
    *parts = out;
}

/// Parses a command line into a syntax tree, expanding aliases at command
/// positions.
///
/// # Errors
///
/// Returns [`Error::Incomplete`] when more input is needed and
/// [`Error::Parse`] for malformed input.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// let list = rushy_core::parser::parse_program("if true; then echo ok; fi", &BTreeMap::new())?;
/// assert_eq!(list.0.len(), 1);
/// # Ok::<(), rushy_core::Error>(())
/// ```
pub fn parse_program(input: &str, aliases: &BTreeMap<String, String>) -> Result<List> {
    let mut p = Parser::new(input, aliases);
    let list = p.list()?;
    p.blank();
    match p.peek() {
        None => Ok(list),
        Some(_) => Err(Error::Parse(format!("unexpected `{}`", p.token_text()))),
    }
}

/// Whether `s` is a valid variable name: `[A-Za-z_][A-Za-z0-9_]*`.
#[must_use]
pub fn is_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_meta(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')'
    )
}

fn push_lit(segs: &mut Vec<Seg>, c: char, quoted: bool) {
    if let Some(Seg::Lit { text, quoted: q }) = segs.last_mut() {
        if *q == quoted {
            text.push(c);
            return;
        }
    }
    segs.push(Seg::Lit {
        text: c.to_string(),
        quoted,
    });
}

fn push_str(segs: &mut Vec<Seg>, s: &str, quoted: bool) {
    for c in s.chars() {
        push_lit(segs, c, quoted);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    AndIf,
    OrIf,
    Pipe,
    PipeErr,
    Amp,
    Semi,
    DSemi,
    SemiAmp,
    DSemiAmp,
    LParen,
    RParen,
    Newline,
}

impl Op {
    const fn text(self) -> &'static str {
        match self {
            Self::AndIf => "&&",
            Self::OrIf => "||",
            Self::Pipe => "|",
            Self::PipeErr => "|&",
            Self::Amp => "&",
            Self::Semi => ";",
            Self::DSemi => ";;",
            Self::SemiAmp => ";&",
            Self::DSemiAmp => ";;&",
            Self::LParen => "(",
            Self::RParen => ")",
            Self::Newline => "newline",
        }
    }
}

struct Parser<'a> {
    s: Vec<char>,
    pos: usize,
    aliases: &'a BTreeMap<String, String>,
    active: Vec<(String, usize)>,
    expansions: usize,
    alias_next: bool,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &str, aliases: &'a BTreeMap<String, String>) -> Self {
        Self {
            s: input.chars().collect(),
            pos: 0,
            aliases,
            active: Vec::new(),
            expansions: 0,
            alias_next: false,
            depth: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.s.get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn starts_with(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.starts_with(text) {
            self.pos += text.chars().count();
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Error::Parse("nesting is too deep".into()));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Skips blanks, line continuations, and comments (not newlines).
    fn blank(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => self.pos += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.pos += 2,
                Some('#') => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn token_text(&self) -> String {
        if let Some(op) = self.peek_op() {
            return op.text().to_owned();
        }
        self.s
            .get(self.pos..)
            .unwrap_or_default()
            .iter()
            .take_while(|c| !is_meta(**c))
            .collect()
    }

    fn peek_op(&self) -> Option<Op> {
        let ops = [
            (";;&", Op::DSemiAmp),
            ("&&", Op::AndIf),
            ("||", Op::OrIf),
            ("|&", Op::PipeErr),
            (";;", Op::DSemi),
            (";&", Op::SemiAmp),
            ("|", Op::Pipe),
            (";", Op::Semi),
            ("(", Op::LParen),
            (")", Op::RParen),
            ("\n", Op::Newline),
        ];
        if self.starts_with("&>") {
            return None;
        }
        if self.peek() == Some('&') && !self.starts_with("&&") {
            return Some(Op::Amp);
        }
        ops.iter()
            .find(|(t, _)| self.starts_with(t))
            .map(|(_, op)| *op)
    }

    fn eat_op(&mut self, op: Op) -> bool {
        self.blank();
        if self.peek_op() == Some(op) {
            self.pos += match op {
                Op::DSemiAmp => 3,
                Op::AndIf | Op::OrIf | Op::PipeErr | Op::DSemi | Op::SemiAmp => 2,
                _ => 1,
            };
            true
        } else {
            false
        }
    }

    fn newlines(&mut self) {
        while self.eat_op(Op::Newline) {}
    }

    /// The plain word at the cursor, if it is made of ordinary characters.
    fn peek_plain(&self) -> Option<String> {
        let mut out = String::new();
        let mut i = self.pos;
        while let Some(&c) = self.s.get(i) {
            if is_meta(c) {
                break;
            }
            if matches!(c, '\'' | '"' | '\\' | '$' | '`') {
                return None;
            }
            out.push(c);
            i += 1;
        }
        (!out.is_empty()).then_some(out)
    }

    fn peek_reserved(&mut self) -> Option<String> {
        self.blank();
        self.peek_plain()
    }

    fn is_reserved(&mut self, word: &str) -> bool {
        self.peek_reserved().is_some_and(|w| w == word)
    }

    fn eat_reserved(&mut self, word: &str) -> bool {
        if self.is_reserved(word) {
            self.pos += word.chars().count();
            true
        } else {
            false
        }
    }

    fn expect_reserved(&mut self, word: &str) -> Result<()> {
        if self.eat_reserved(word) {
            return Ok(());
        }
        self.blank();
        match self.peek() {
            None => Err(Error::Incomplete),
            Some(_) => Err(Error::Parse(format!(
                "expected `{word}`, found `{}`",
                self.token_text()
            ))),
        }
    }

    /// Expands an alias at the cursor; returns whether one was expanded.
    fn expand_alias(&mut self) -> Result<bool> {
        if self.aliases.is_empty() {
            return Ok(false);
        }
        self.blank();
        let pos = self.pos;
        self.active.retain(|(_, end)| *end > pos);
        let Some(name) = self.peek_plain() else {
            return Ok(false);
        };
        if self.active.iter().any(|(n, _)| *n == name) {
            return Ok(false);
        }
        let Some(value) = self.aliases.get(&name) else {
            return Ok(false);
        };
        self.expansions += 1;
        if self.expansions > MAX_ALIAS_EXPANSIONS {
            return Err(Error::Parse(format!("alias `{name}` expands too deeply")));
        }
        let old = name.chars().count();
        let new: Vec<char> = value.chars().collect();
        let added = new.len();
        self.s.splice(pos..pos + old, new);
        for (_, end) in &mut self.active {
            if *end > pos {
                *end = *end + added - old;
            }
        }
        self.active.push((name, pos + added));
        self.alias_next = value.ends_with(' ');
        Ok(true)
    }

    fn list(&mut self) -> Result<List> {
        self.enter()?;
        let mut items = Vec::new();
        loop {
            self.blank();
            while self.eat_op(Op::Newline) || self.eat_op(Op::Semi) {}
            self.blank();
            let Some(op) = (match self.peek() {
                None => break,
                Some(_) => Some(self.peek_op()),
            }) else {
                break;
            };
            if matches!(
                op,
                Some(Op::RParen | Op::DSemi | Op::SemiAmp | Op::DSemiAmp)
            ) {
                break;
            }
            if self
                .peek_plain()
                .is_some_and(|w| CLOSERS.contains(&w.as_str()))
            {
                break;
            }
            let and_or = self.and_or()?;
            let background = self.eat_op(Op::Amp);
            let sep = background || self.eat_op(Op::Semi) || self.eat_op(Op::Newline);
            items.push(Item { and_or, background });
            if !sep {
                break;
            }
        }
        self.leave();
        Ok(List(items))
    }

    fn and_or(&mut self) -> Result<AndOr> {
        let first = self.pipe()?;
        let mut rest = Vec::new();
        loop {
            let conn = if self.eat_op(Op::AndIf) {
                Connector::And
            } else if self.eat_op(Op::OrIf) {
                Connector::Or
            } else {
                break;
            };
            self.newlines();
            self.blank();
            if self.peek().is_none() {
                return Err(Error::Incomplete);
            }
            rest.push((conn, self.pipe()?));
        }
        Ok(AndOr { first, rest })
    }

    fn pipe(&mut self) -> Result<Pipe> {
        let negated = self.eat_reserved("!");
        let mut commands = vec![self.command()?];
        loop {
            let err = if self.eat_op(Op::PipeErr) {
                true
            } else if self.eat_op(Op::Pipe) {
                false
            } else {
                break;
            };
            if err {
                let dup = Redir {
                    fd: Some(2),
                    op: RedirOp::DupOut(crate::ast::Word::lit("1", false)),
                };
                if let Some(last) = commands.last_mut() {
                    match last {
                        Command::Simple(s) => s.redirects.push(dup),
                        Command::Compound(_, r) => r.push(dup),
                        Command::Function(..) => {}
                    }
                }
            }
            self.newlines();
            self.blank();
            if self.peek().is_none() {
                return Err(Error::Incomplete);
            }
            commands.push(self.command()?);
        }
        Ok(Pipe { negated, commands })
    }

    fn command(&mut self) -> Result<Command> {
        self.enter()?;
        let res = self.command_inner();
        self.leave();
        res
    }

    fn command_inner(&mut self) -> Result<Command> {
        while self.expand_alias()? {}
        self.blank();
        let Some(c) = self.peek() else {
            return Err(Error::Incomplete);
        };
        if c == '(' {
            if self.peek_at(1) == Some('(') {
                self.pos += 2;
                let expr = self.arith_body()?;
                return self.with_redirects(Compound::Arith(expr));
            }
            self.pos += 1;
            let body = self.list()?;
            self.close_paren()?;
            return self.with_redirects(Compound::Subshell(body));
        }
        if let Some(op) = self.peek_op() {
            return Err(Error::Parse(match op {
                Op::Pipe | Op::PipeErr => "missing command before `|`".into(),
                Op::Newline => "unexpected newline".into(),
                _ => format!("unexpected `{}`", op.text()),
            }));
        }
        let word = self.peek_plain().unwrap_or_default();
        match word.as_str() {
            "{" => {
                self.pos += 1;
                let body = self.list()?;
                self.expect_reserved("}")?;
                self.with_redirects(Compound::Group(body))
            }
            "if" => {
                self.pos += 2;
                self.if_rest()
            }
            "while" | "until" => {
                self.pos += word.len();
                let cond = self.list()?;
                self.expect_reserved("do")?;
                let body = self.list()?;
                self.expect_reserved("done")?;
                self.with_redirects(Compound::While {
                    until: word == "until",
                    cond,
                    body,
                })
            }
            "for" => {
                self.pos += 3;
                self.for_rest()
            }
            "case" => {
                self.pos += 4;
                self.case_rest()
            }
            "function" => {
                self.pos += 8;
                self.blank();
                let name = self
                    .peek_plain()
                    .ok_or_else(|| Error::Parse("expected a function name".into()))?;
                self.pos += name.chars().count();
                self.blank();
                if self.eat("(") {
                    self.blank();
                    if !self.eat(")") {
                        return Err(Error::Parse("expected `)`".into()));
                    }
                }
                self.function_body(name)
            }
            "[[" => {
                self.pos += 2;
                self.cond_rest()
            }
            w if CLOSERS.contains(&w) => Err(Error::Parse(format!("unexpected `{w}`"))),
            _ => self.simple(),
        }
    }

    fn close_paren(&mut self) -> Result<()> {
        self.blank();
        match self.peek() {
            None => Err(Error::Incomplete),
            Some(')') => {
                self.pos += 1;
                Ok(())
            }
            Some(_) => Err(Error::Parse(format!(
                "expected `)`, found `{}`",
                self.token_text()
            ))),
        }
    }

    fn with_redirects(&mut self, c: Compound) -> Result<Command> {
        let mut redirs = Vec::new();
        loop {
            self.blank();
            match self.redirect()? {
                Some(r) => redirs.push(r),
                None => break,
            }
        }
        Ok(Command::Compound(c, redirs))
    }

    fn function_body(&mut self, name: String) -> Result<Command> {
        self.newlines();
        self.blank();
        if self.peek().is_none() {
            return Err(Error::Incomplete);
        }
        let body = self.command()?;
        if !matches!(body, Command::Compound(..)) {
            return Err(Error::Parse(format!(
                "function `{name}`: the body must be a compound command"
            )));
        }
        Ok(Command::Function(name, std::sync::Arc::new(body)))
    }

    fn if_rest(&mut self) -> Result<Command> {
        let mut branches = Vec::new();
        let mut otherwise = None;
        loop {
            let cond = self.list()?;
            self.expect_reserved("then")?;
            let body = self.list()?;
            branches.push((cond, body));
            if self.eat_reserved("elif") {
                continue;
            }
            if self.eat_reserved("else") {
                otherwise = Some(self.list()?);
            }
            self.expect_reserved("fi")?;
            break;
        }
        self.with_redirects(Compound::If {
            branches,
            otherwise,
        })
    }

    fn for_rest(&mut self) -> Result<Command> {
        self.blank();
        if self.starts_with("((") {
            self.pos += 2;
            let text = self.arith_raw()?;
            let parts: Vec<&str> = text.split(';').collect();
            let [init, cond, step] = parts.as_slice() else {
                return Err(Error::Parse("expected `for ((init; cond; step))`".into()));
            };
            let init = expansion_text(init, self.aliases)?;
            let cond = expansion_text(cond, self.aliases)?;
            let step = expansion_text(step, self.aliases)?;
            self.eat_op(Op::Semi);
            self.newlines();
            self.expect_reserved("do")?;
            let body = self.list()?;
            self.expect_reserved("done")?;
            return self.with_redirects(Compound::ArithFor {
                init,
                cond,
                step,
                body,
            });
        }
        let var = self.peek_plain().unwrap_or_default();
        if !is_var_name(&var) {
            return Err(if self.peek().is_none() {
                Error::Incomplete
            } else {
                Error::Parse(format!("`{var}`: not a valid loop variable"))
            });
        }
        self.pos += var.len();
        self.newlines();
        let items = if self.eat_reserved("in") {
            let mut words = Vec::new();
            loop {
                self.blank();
                match self.peek() {
                    None => return Err(Error::Incomplete),
                    Some(c) if is_meta(c) => break,
                    Some(_) => words.push(self.word()?),
                }
            }
            Some(words)
        } else {
            None
        };
        self.eat_op(Op::Semi);
        self.newlines();
        self.expect_reserved("do")?;
        let body = self.list()?;
        self.expect_reserved("done")?;
        self.with_redirects(Compound::For { var, items, body })
    }

    fn case_rest(&mut self) -> Result<Command> {
        self.blank();
        if self.peek().is_none() {
            return Err(Error::Incomplete);
        }
        let word = self.word()?;
        self.newlines();
        self.expect_reserved("in")?;
        let mut arms = Vec::new();
        loop {
            self.newlines();
            if self.eat_reserved("esac") {
                break;
            }
            self.blank();
            if self.peek().is_none() {
                return Err(Error::Incomplete);
            }
            self.eat("(");
            let mut patterns = Vec::new();
            loop {
                self.blank();
                match self.peek() {
                    None => return Err(Error::Incomplete),
                    Some(c) if is_meta(c) => {
                        return Err(Error::Parse(format!("unexpected `{c}` in a case pattern")));
                    }
                    Some(_) => patterns.push(self.word()?),
                }
                self.blank();
                if self.eat("|") {
                    continue;
                }
                if self.eat(")") {
                    break;
                }
                return Err(if self.peek().is_none() {
                    Error::Incomplete
                } else {
                    Error::Parse("expected `)` after a case pattern".into())
                });
            }
            let body = self.list()?;
            let end = if self.eat_op(Op::DSemiAmp) {
                CaseEnd::Test
            } else if self.eat_op(Op::DSemi) {
                CaseEnd::Break
            } else if self.eat_op(Op::SemiAmp) {
                CaseEnd::Fall
            } else if self.is_reserved("esac") {
                CaseEnd::Break
            } else if self.peek().is_none() {
                return Err(Error::Incomplete);
            } else {
                return Err(Error::Parse(format!(
                    "expected `;;`, found `{}`",
                    self.token_text()
                )));
            };
            arms.push(CaseArm {
                patterns,
                body,
                end,
            });
        }
        self.with_redirects(Compound::Case { word, arms })
    }

    fn cond_rest(&mut self) -> Result<Command> {
        let mut words = Vec::new();
        loop {
            self.blank();
            let Some(c) = self.peek() else {
                return Err(Error::Incomplete);
            };
            if c == '\n' {
                self.pos += 1;
                continue;
            }
            if let Some(op) = ["&&", "||", "(", ")", "<", ">"]
                .into_iter()
                .find(|op| self.starts_with(op))
            {
                self.pos += op.len();
                words.push(crate::ast::Word::lit(op, false));
                continue;
            }
            if c == ';' || c == '&' || c == '|' {
                return Err(Error::Parse(format!("unexpected `{c}` in `[[`")));
            }
            let w = self.word()?;
            if w.plain() == Some("]]") {
                break;
            }
            words.push(w);
        }
        if words.is_empty() {
            return Err(Error::Parse("`[[ ]]` needs an expression".into()));
        }
        self.with_redirects(Compound::Cond(words))
    }

    /// Reads up to the `))` that closes `((`, then parses it.
    fn arith_body(&mut self) -> Result<crate::ast::Word> {
        let text = self.arith_raw()?;
        expansion_text(&text, self.aliases)
    }

    fn arith_raw(&mut self) -> Result<String> {
        let mut depth = 0usize;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(Error::Incomplete),
                Some('(') => {
                    depth += 1;
                    out.push('(');
                }
                Some(')') if depth == 0 => {
                    if self.bump() == Some(')') {
                        return Ok(out);
                    }
                    return Err(Error::Parse("expected `))`".into()));
                }
                Some(')') => {
                    depth -= 1;
                    out.push(')');
                }
                Some(c) => out.push(c),
            }
        }
    }

    fn simple(&mut self) -> Result<Command> {
        let mut s = Simple::default();
        loop {
            self.blank();
            // An alias that ends in a space also expands the word after it.
            if !s.words.is_empty() && self.alias_next {
                self.alias_next = false;
                if self.expand_alias()? {
                    continue;
                }
            }
            let Some(c) = self.peek() else { break };
            if let Some(r) = self.redirect()? {
                s.redirects.push(r);
                continue;
            }
            if self.starts_with("<(") || self.starts_with(">(") {
                let out = c == '>';
                self.pos += 2;
                let body = self.list()?;
                self.close_paren()?;
                s.words.push(crate::ast::Word {
                    segs: vec![if out {
                        Seg::ProcOut(body)
                    } else {
                        Seg::ProcIn(body)
                    }],
                });
                continue;
            }
            if c == '(' && s.words.len() == 1 && s.assigns.is_empty() && s.redirects.is_empty() {
                let Some(name) = s.words.first().and_then(|w| w.plain()).map(str::to_owned) else {
                    return Err(Error::Parse("unexpected `(`".into()));
                };
                self.pos += 1;
                self.blank();
                if !self.eat(")") {
                    return Err(Error::Parse(format!("expected `)` after `{name}(`")));
                }
                return self.function_body(name);
            }
            if is_meta(c) {
                break;
            }
            let w = self.word()?;
            if s.words.is_empty() {
                if let Some(a) = assignment(&w) {
                    s.assigns.push(a);
                    continue;
                }
            }
            s.words.push(w);
        }
        if s.words.is_empty() && s.assigns.is_empty() && s.redirects.is_empty() {
            return Err(Error::Parse(format!("unexpected `{}`", self.token_text())));
        }
        Ok(Command::Simple(s))
    }

    fn redirect(&mut self) -> Result<Option<Redir>> {
        let start = self.pos;
        let mut digits = String::new();
        while let Some(c) = self.peek().filter(char::is_ascii_digit) {
            digits.push(c);
            self.pos += 1;
        }
        if self.starts_with("<(")
            || self.starts_with(">(")
            || !matches!(self.peek(), Some('<' | '>' | '&'))
        {
            self.pos = start;
            return Ok(None);
        }
        let fd = if digits.is_empty() {
            None
        } else {
            Some(
                digits
                    .parse::<u32>()
                    .map_err(|_| Error::Parse(format!("bad file descriptor `{digits}`")))?,
            )
        };
        let ops = [
            "&>>", "&>", "<<<", "<<-", "<<", "<&", "<>", "<", ">>", ">|", ">&", ">",
        ];
        let Some(op) = ops.into_iter().find(|op| self.starts_with(op)) else {
            self.pos = start;
            return Ok(None);
        };
        if op.starts_with('&') && fd.is_some() {
            self.pos = start;
            return Ok(None);
        }
        self.pos += op.len();
        self.blank();
        match self.peek() {
            Some(c) if !is_meta(c) => {}
            _ => {
                return Err(Error::Parse(format!("expected a file name after `{op}`")));
            }
        }
        let target = self.word()?;
        let op = match op {
            "<" => RedirOp::In(target),
            ">" => RedirOp::Out {
                target,
                clobber: false,
            },
            ">|" => RedirOp::Out {
                target,
                clobber: true,
            },
            ">>" => RedirOp::Append(target),
            "<>" => RedirOp::ReadWrite(target),
            ">&" => RedirOp::DupOut(target),
            "<&" => RedirOp::DupIn(target),
            "&>" => RedirOp::Both {
                target,
                append: false,
            },
            "&>>" => RedirOp::Both {
                target,
                append: true,
            },
            "<<<" => RedirOp::HereString(target),
            _ => RedirOp::HereDoc(self.heredoc(&target, op == "<<-")?),
        };
        Ok(Some(Redir { fd, op }))
    }

    /// Takes the body of a here-document from the lines after the current
    /// one and removes it from the input.
    fn heredoc(&mut self, delim: &crate::ast::Word, strip: bool) -> Result<crate::ast::Word> {
        let mut text = String::new();
        for seg in &delim.segs {
            match seg {
                Seg::Lit { text: t, .. } => text.push_str(t),
                Seg::Param { param, .. } => {
                    text.push('$');
                    text.push_str(&param.name);
                }
                _ => return Err(Error::Parse("bad here-document delimiter".into())),
            }
        }
        let quoted = delim.is_quoted();
        let mut i = self.pos;
        let (mut sq, mut dq) = (false, false);
        while let Some(&c) = self.s.get(i) {
            match c {
                '\\' if !sq => i += 1,
                '\'' if !dq => sq = !sq,
                '"' if !sq => dq = !dq,
                '\n' if !sq && !dq => break,
                _ => {}
            }
            i += 1;
        }
        if i >= self.s.len() {
            return Err(Error::Incomplete);
        }
        let start = i + 1;
        let mut j = start;
        let mut body = String::new();
        let end = loop {
            if j > self.s.len() {
                return Err(Error::Incomplete);
            }
            let line_end = self
                .s
                .get(j..)
                .unwrap_or_default()
                .iter()
                .position(|c| *c == '\n')
                .map_or(self.s.len(), |p| j + p);
            let line: String = self.s.get(j..line_end).unwrap_or_default().iter().collect();
            let line = if strip {
                line.trim_start_matches('\t').to_owned()
            } else {
                line
            };
            if line == text {
                break (line_end + 1).min(self.s.len());
            }
            if line_end >= self.s.len() {
                return Err(Error::Incomplete);
            }
            body.push_str(&line);
            body.push('\n');
            j = line_end + 1;
        };
        self.s.drain(start..end);
        if quoted {
            return Ok(crate::ast::Word::lit(&body, true));
        }
        heredoc_text(&body, self.aliases)
    }

    /// Reads one word up to an unquoted metacharacter.
    fn word(&mut self) -> Result<crate::ast::Word> {
        let mut segs = Vec::new();
        if self.peek() == Some('~') {
            let mut user = String::new();
            let mut i = self.pos + 1;
            while let Some(&c) = self.s.get(i) {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                    user.push(c);
                    i += 1;
                } else {
                    break;
                }
            }
            if self.s.get(i).is_none_or(|c| *c == '/' || is_meta(*c)) {
                self.pos = i;
                segs.push(Seg::Tilde((!user.is_empty()).then_some(user)));
            }
        }
        while let Some(c) = self.peek() {
            if is_meta(c) {
                break;
            }
            self.pos += 1;
            match c {
                '\\' => match self.bump() {
                    Some('\n') => {}
                    Some(n) => push_lit(&mut segs, n, true),
                    None => return Err(Error::Incomplete),
                },
                '\'' => self.single(&mut segs)?,
                '"' => self.double(&mut segs)?,
                '$' => self.dollar(&mut segs, false)?,
                '`' => self.backtick(&mut segs, false)?,
                _ => push_lit(&mut segs, c, false),
            }
        }
        if segs.is_empty() {
            return Err(Error::Parse(format!("unexpected `{}`", self.token_text())));
        }
        Ok(crate::ast::Word { segs })
    }

    fn single(&mut self, segs: &mut Vec<Seg>) -> Result<()> {
        let mut any = false;
        loop {
            match self.bump() {
                Some('\'') => break,
                Some(c) => {
                    any = true;
                    push_lit(segs, c, true);
                }
                None => return Err(Error::Incomplete),
            }
        }
        if !any {
            segs.push(Seg::Lit {
                text: String::new(),
                quoted: true,
            });
        }
        Ok(())
    }

    fn double(&mut self, segs: &mut Vec<Seg>) -> Result<()> {
        segs.push(Seg::Lit {
            text: String::new(),
            quoted: true,
        });
        loop {
            match self.bump() {
                Some('"') => return Ok(()),
                Some('\\') => match self.bump() {
                    Some(c @ ('$' | '"' | '\\' | '`')) => push_lit(segs, c, true),
                    Some('\n') => {}
                    Some(c) => {
                        push_lit(segs, '\\', true);
                        push_lit(segs, c, true);
                    }
                    None => return Err(Error::Incomplete),
                },
                Some('$') => self.dollar(segs, true)?,
                Some('`') => self.backtick(segs, true)?,
                Some(c) => push_lit(segs, c, true),
                None => return Err(Error::Incomplete),
            }
        }
    }

    fn backtick(&mut self, segs: &mut Vec<Seg>, quoted: bool) -> Result<()> {
        let mut raw = String::new();
        loop {
            match self.bump() {
                None => return Err(Error::Incomplete),
                Some('`') => break,
                Some('\\') => match self.bump() {
                    Some(c @ ('`' | '\\' | '$')) => raw.push(c),
                    Some(c) => {
                        raw.push('\\');
                        raw.push(c);
                    }
                    None => return Err(Error::Incomplete),
                },
                Some(c) => raw.push(c),
            }
        }
        let body = parse_program(&raw, self.aliases)?;
        segs.push(Seg::Command { body, quoted });
        Ok(())
    }

    /// Reads what follows a `$`.
    fn dollar(&mut self, segs: &mut Vec<Seg>, quoted: bool) -> Result<()> {
        match self.peek() {
            Some('(') if self.peek_at(1) == Some('(') => {
                self.pos += 2;
                let expr = self.arith_body()?;
                segs.push(Seg::Arith { expr, quoted });
            }
            Some('(') => {
                self.pos += 1;
                let saved = std::mem::take(&mut self.active);
                let body = self.list();
                self.active = saved;
                let body = body?;
                self.close_paren()?;
                segs.push(Seg::Command { body, quoted });
            }
            Some('{') => {
                self.pos += 1;
                let param = self.brace_param(quoted)?;
                segs.push(Seg::Param { param, quoted });
            }
            Some('\'') if !quoted => {
                self.pos += 1;
                self.ansi_c(segs)?;
            }
            Some('"') if !quoted => {
                self.pos += 1;
                self.double(segs)?;
            }
            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                let mut name = String::new();
                while let Some(c) = self
                    .peek()
                    .filter(|c| *c == '_' || c.is_ascii_alphanumeric())
                {
                    name.push(c);
                    self.pos += 1;
                }
                segs.push(Seg::Param {
                    param: Param {
                        name,
                        op: ParamOp::Plain,
                    },
                    quoted,
                });
            }
            Some(c) if c.is_ascii_digit() || "?$#@*!-".contains(c) => {
                self.pos += 1;
                segs.push(Seg::Param {
                    param: Param {
                        name: c.to_string(),
                        op: ParamOp::Plain,
                    },
                    quoted,
                });
            }
            _ => push_lit(segs, '$', quoted),
        }
        Ok(())
    }

    fn ansi_c(&mut self, segs: &mut Vec<Seg>) -> Result<()> {
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(Error::Incomplete),
                Some('\'') => break,
                Some('\\') => {
                    let Some(c) = self.bump() else {
                        return Err(Error::Incomplete);
                    };
                    let mut rest: String = c.to_string();
                    while rest.len() < 4
                        && self.peek().is_some_and(|n| match c {
                            'x' => n.is_ascii_hexdigit() && rest.len() < 3,
                            '0'..='7' => ('0'..='7').contains(&n) && rest.len() < 3,
                            _ => false,
                        })
                    {
                        if let Some(n) = self.bump() {
                            rest.push(n);
                        }
                    }
                    out.push_str(&crate::builtins::unescape(&format!("\\{rest}"), false).0);
                }
                Some(c) => out.push(c),
            }
        }
        push_str(segs, &out, true);
        if out.is_empty() {
            segs.push(Seg::Lit {
                text: String::new(),
                quoted: true,
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines, reason = "one arm per ${} operator")]
    fn brace_param(&mut self, quoted: bool) -> Result<Param> {
        let bad = |p: &Self| {
            if p.peek().is_none() {
                Error::Incomplete
            } else {
                Error::Parse("bad substitution".into())
            }
        };
        let length = self.peek() == Some('#') && self.peek_at(1) != Some('}');
        if length {
            self.pos += 1;
        }
        let mut name = String::new();
        match self.peek() {
            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                while let Some(c) = self
                    .peek()
                    .filter(|c| *c == '_' || c.is_ascii_alphanumeric())
                {
                    name.push(c);
                    self.pos += 1;
                }
            }
            Some(c) if c.is_ascii_digit() => {
                while let Some(c) = self.peek().filter(char::is_ascii_digit) {
                    name.push(c);
                    self.pos += 1;
                }
            }
            Some(c) if "?$#@*!-".contains(c) => {
                name.push(c);
                self.pos += 1;
            }
            _ => return Err(bad(self)),
        }
        if length {
            return if self.eat("}") {
                Ok(Param {
                    name,
                    op: ParamOp::Length,
                })
            } else {
                Err(bad(self))
            };
        }
        let op = match self.bump() {
            Some('}') => {
                return Ok(Param {
                    name,
                    op: ParamOp::Plain,
                });
            }
            Some(':') => {
                if let Some(c @ ('-' | '=' | '+' | '?')) = self.peek() {
                    self.pos += 1;
                    let word = self.brace_word(&[], quoted)?;
                    colon_op(c, true, word)
                } else {
                    let offset = self.brace_word(&[':'], quoted)?;
                    let length = if self.eat(":") {
                        Some(self.brace_word(&[], quoted)?)
                    } else {
                        None
                    };
                    ParamOp::Slice { offset, length }
                }
            }
            Some(c @ ('-' | '=' | '+' | '?')) => {
                let word = self.brace_word(&[], quoted)?;
                colon_op(c, false, word)
            }
            Some('#') => {
                let longest = self.eat("#");
                ParamOp::Prefix {
                    longest,
                    pattern: self.brace_word(&[], quoted)?,
                }
            }
            Some('%') => {
                let longest = self.eat("%");
                ParamOp::Suffix {
                    longest,
                    pattern: self.brace_word(&[], quoted)?,
                }
            }
            Some('/') => {
                let mode = if self.eat("/") {
                    ReplaceMode::All
                } else if self.eat("#") {
                    ReplaceMode::Prefix
                } else if self.eat("%") {
                    ReplaceMode::Suffix
                } else {
                    ReplaceMode::First
                };
                let pattern = self.brace_word(&['/'], quoted)?;
                let with = if self.eat("/") {
                    self.brace_word(&[], quoted)?
                } else {
                    crate::ast::Word::default()
                };
                ParamOp::Replace {
                    mode,
                    pattern,
                    with,
                }
            }
            Some('^') => ParamOp::Upper { all: self.eat("^") },
            Some(',') => ParamOp::Lower { all: self.eat(",") },
            None => return Err(Error::Incomplete),
            Some(_) => return Err(Error::Parse("bad substitution".into())),
        };
        if self.eat("}") {
            Ok(Param { name, op })
        } else {
            Err(bad(self))
        }
    }

    /// Reads an operand inside `${...}` up to `}` or one of `stops`.
    fn brace_word(&mut self, stops: &[char], quoted: bool) -> Result<crate::ast::Word> {
        let mut segs = Vec::new();
        let mut depth = 0usize;
        loop {
            let Some(c) = self.peek() else {
                return Err(Error::Incomplete);
            };
            if depth == 0 && (c == '}' || stops.contains(&c)) {
                break;
            }
            self.pos += 1;
            match c {
                '\\' => match self.bump() {
                    Some(n) => push_lit(&mut segs, n, true),
                    None => return Err(Error::Incomplete),
                },
                '\'' if !quoted => self.single(&mut segs)?,
                '"' => self.double(&mut segs)?,
                '$' => self.dollar(&mut segs, quoted)?,
                '`' => self.backtick(&mut segs, quoted)?,
                '{' => {
                    depth += 1;
                    push_lit(&mut segs, c, quoted);
                }
                '}' => {
                    depth -= 1;
                    push_lit(&mut segs, c, quoted);
                }
                _ => push_lit(&mut segs, c, quoted),
            }
        }
        Ok(crate::ast::Word { segs })
    }
}

fn colon_op(c: char, colon: bool, word: crate::ast::Word) -> ParamOp {
    match c {
        '-' => ParamOp::Default { colon, word },
        '=' => ParamOp::Assign { colon, word },
        '+' => ParamOp::Alt { colon, word },
        _ => ParamOp::Error { colon, word },
    }
}

fn assignment(w: &crate::ast::Word) -> Option<Assign> {
    let Some(Seg::Lit {
        text,
        quoted: false,
    }) = w.segs.first()
    else {
        return None;
    };
    let (head, rest) = text.split_once('=')?;
    let (name, append) = head.strip_suffix('+').map_or((head, false), |n| (n, true));
    if !is_var_name(name) {
        return None;
    }
    let mut segs = w.segs.clone();
    if rest.is_empty() {
        segs.remove(0);
    } else if let Some(first) = segs.first_mut() {
        *first = Seg::Lit {
            text: rest.to_owned(),
            quoted: false,
        };
    }
    Some(Assign {
        name: name.to_owned(),
        append,
        value: crate::ast::Word { segs },
    })
}

/// Parses text where only `$`, backquotes, and `\` are special, as inside
/// double quotes. Used for arithmetic.
fn expansion_text(text: &str, aliases: &BTreeMap<String, String>) -> Result<crate::ast::Word> {
    scan_text(text, aliases, false)
}

fn heredoc_text(text: &str, aliases: &BTreeMap<String, String>) -> Result<crate::ast::Word> {
    scan_text(text, aliases, true)
}

fn scan_text(
    text: &str,
    aliases: &BTreeMap<String, String>,
    heredoc: bool,
) -> Result<crate::ast::Word> {
    let mut p = Parser::new(text, aliases);
    let mut segs = Vec::new();
    while let Some(c) = p.bump() {
        match c {
            '\\' => match p.bump() {
                Some(n @ ('$' | '`' | '\\')) => push_lit(&mut segs, n, true),
                Some('\n') if heredoc => {}
                Some(n) => {
                    push_lit(&mut segs, '\\', true);
                    push_lit(&mut segs, n, true);
                }
                None => push_lit(&mut segs, '\\', true),
            },
            '$' => p.dollar(&mut segs, true)?,
            '`' => p.backtick(&mut segs, true)?,
            _ => push_lit(&mut segs, c, true),
        }
    }
    if segs.is_empty() {
        segs.push(Seg::Lit {
            text: String::new(),
            quoted: true,
        });
    }
    Ok(crate::ast::Word { segs })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn lit(s: &str) -> WordPart {
        WordPart::Literal(s.into())
    }

    fn words(p: &[Pipeline], i: usize, c: usize) -> Vec<Vec<WordPart>> {
        p.get(i)
            .and_then(|p| p.commands.get(c))
            .map(|c| c.words.iter().map(|w| w.parts.clone()).collect())
            .unwrap_or_default()
    }

    fn no_aliases() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn program(s: &str) -> Result<List> {
        parse_program(s, &no_aliases())
    }

    fn first_cmd(list: &List) -> Option<&Command> {
        list.0.first().and_then(|i| i.and_or.first.commands.first())
    }

    #[test]
    fn splits_on_whitespace_and_quotes() -> Result<()> {
        let p = parse(r#"echo 'a b' "c d" e\ f"#, &no_aliases())?;
        assert_eq!(
            words(&p, 0, 0),
            vec![
                vec![lit("echo")],
                vec![lit("a b")],
                vec![lit("c d")],
                vec![lit("e f")]
            ]
        );
        Ok(())
    }

    #[test]
    fn single_quotes_suppress_expansion() -> Result<()> {
        let p = parse(r#"echo '$HOME' "$HOME" ${X}y $? $$"#, &no_aliases())?;
        assert_eq!(
            words(&p, 0, 0),
            vec![
                vec![lit("echo")],
                vec![lit("$HOME")],
                vec![WordPart::Var("HOME".into())],
                vec![WordPart::Var("X".into()), lit("y")],
                vec![WordPart::Var("?".into())],
                vec![WordPart::Var("$".into())],
            ]
        );
        Ok(())
    }

    #[test]
    fn parses_pipes_redirects_and_background() -> TestResult {
        let p = parse("sort < in.txt | uniq >> out.txt &", &no_aliases())?;
        let first = p.first().ok_or("no pipeline")?;
        assert_eq!(p.len(), 1);
        assert!(first.background);
        assert_eq!(first.commands.len(), 2);
        let r0 = first.commands.first().and_then(|c| c.redirects.first());
        let r1 = first.commands.get(1).and_then(|c| c.redirects.first());
        assert_eq!(r0.map(|r| r.kind), Some(RedirectKind::In));
        assert_eq!(r1.map(|r| r.kind), Some(RedirectKind::Append));
        assert_eq!(r1.map(|r| r.target.clone()), Some(Word::literal("out.txt")));
        Ok(())
    }

    #[test]
    fn operators_inside_quotes_are_literal() -> Result<()> {
        let p = parse(r#"echo "a|b>c" 'd&e'"#, &no_aliases())?;
        let w = words(&p, 0, 0);
        assert_eq!(w.len(), 3);
        assert_eq!(w.get(1), Some(&vec![lit("a|b>c")]));
        assert_eq!(w.get(2), Some(&vec![lit("d&e")]));
        Ok(())
    }

    #[test]
    fn separators_and_comments() -> Result<()> {
        let p = parse("a; b # comment\nc", &no_aliases())?;
        assert_eq!(p.len(), 3);
        Ok(())
    }

    #[test]
    fn incomplete_input_is_detected() {
        for s in [
            "echo 'abc",
            "echo \"abc",
            "echo abc \\",
            "a |",
            "a &&",
            "if true; then echo",
            "for i in 1 2; do",
            "while true",
            "case x in",
            "f() {",
            "cat <<EOF\nabc",
            "echo $(ls",
            "echo `ls",
            "{ echo",
            "(echo",
            "echo ${A",
            "[[ -n x",
        ] {
            assert!(matches!(program(s), Err(Error::Incomplete)), "{s}");
            assert!(
                matches!(parse(s, &no_aliases()), Err(Error::Incomplete)),
                "{s}"
            );
        }
    }

    #[test]
    fn line_continuation_joins_words() -> Result<()> {
        let p = parse("echo ab\\\ncd", &no_aliases())?;
        assert_eq!(words(&p, 0, 0).get(1), Some(&vec![lit("abcd")]));
        Ok(())
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "| a",
            "a | | b",
            "a >",
            "&",
            "a && && b",
            "fi",
            "then",
            "done",
            "a )",
        ] {
            assert!(matches!(program(bad), Err(Error::Parse(_))), "{bad}");
        }
    }

    #[test]
    fn flat_view_rejects_what_it_cannot_show() {
        for s in [
            "echo ${A-B}",
            "a && b",
            "a || b",
            "! a",
            "if a; then b; fi",
            "echo $(rm x)",
            "echo `rm x`",
            "echo $((1+2))",
            "a 2> f",
            "a > f 2>&1",
            "f() { a; }",
            "(a)",
            "{ a; }",
            "diff <(a) b",
            "cat <<EOF\nx\nEOF",
        ] {
            assert!(
                matches!(parse(s, &no_aliases()), Err(Error::Parse(_))),
                "{s}"
            );
            assert!(program(s).is_ok(), "{s}");
        }
    }

    #[test]
    fn expands_aliases_at_command_positions() -> Result<()> {
        let aliases = BTreeMap::from([
            ("ll".to_owned(), "ls -l".to_owned()),
            ("ls".to_owned(), "ls -G".to_owned()),
        ]);
        let p = parse("ll /tmp | ll", &aliases)?;
        assert_eq!(
            words(&p, 0, 0),
            vec![
                vec![lit("ls")],
                vec![lit("-G")],
                vec![lit("-l")],
                vec![lit("/tmp")]
            ]
        );
        assert_eq!(words(&p, 0, 1).len(), 3);
        let p = parse("echo ll; ll", &aliases)?;
        assert_eq!(words(&p, 0, 0).get(1), Some(&vec![lit("ll")]));
        assert_eq!(words(&p, 1, 0).len(), 3);
        Ok(())
    }

    #[test]
    fn alias_loops_terminate() -> Result<()> {
        let aliases = BTreeMap::from([
            ("a".to_owned(), "b".to_owned()),
            ("b".to_owned(), "a".to_owned()),
        ]);
        let p = parse("a", &aliases)?;
        assert_eq!(words(&p, 0, 0), vec![vec![lit("a")]]);
        Ok(())
    }

    #[test]
    fn alias_with_trailing_space_expands_the_next_word() -> Result<()> {
        let aliases = BTreeMap::from([
            ("sudo".to_owned(), "sudo ".to_owned()),
            ("ll".to_owned(), "ls -l".to_owned()),
        ]);
        let p = parse("sudo ll", &aliases)?;
        assert_eq!(words(&p, 0, 0).len(), 3);
        Ok(())
    }

    #[test]
    fn tilde_only_at_word_start() -> Result<()> {
        let p = parse("cd ~/src a~b ~ ~bob", &no_aliases())?;
        let w = words(&p, 0, 0);
        assert_eq!(w.get(1), Some(&vec![WordPart::Tilde, lit("/src")]));
        assert_eq!(w.get(2), Some(&vec![lit("a~b")]));
        assert_eq!(w.get(3), Some(&vec![WordPart::Tilde]));
        assert_eq!(w.get(4), Some(&vec![lit("~bob")]));
        Ok(())
    }

    #[test]
    fn parses_and_or_and_negation() -> TestResult {
        let list = program("! a && b || c")?;
        let item = list.0.first().ok_or("no item")?;
        assert!(item.and_or.first.negated);
        let conns: Vec<Connector> = item.and_or.rest.iter().map(|(c, _)| *c).collect();
        assert_eq!(conns, [Connector::And, Connector::Or]);
        Ok(())
    }

    #[test]
    fn parses_compound_commands() -> TestResult {
        let cases = [
            "if a; then b; elif c; then d; else e; fi",
            "for i in 1 2 3; do echo $i; done",
            "for i; do echo; done",
            "for ((i=0; i<3; i++)); do echo; done",
            "while a; do b; done",
            "until a; do b; done",
            "case $x in a|b) echo ab;; *) echo other;& c) x;;& esac",
            "{ a; b; } > out",
            "(cd /; ls)",
            "(( x = 1 + 2 ))",
            "[[ -n $x && $y == a* ]]",
            "f() { echo hi; }",
            "function g { echo; }",
            "function h() ( echo )",
            "if a\nthen\n  b\nfi",
        ];
        for c in cases {
            let list = program(c).map_err(|e| format!("{c}: {e}"))?;
            assert!(first_cmd(&list).is_some(), "{c}");
        }
        let list = program("f() { echo hi; }")?;
        assert!(matches!(first_cmd(&list), Some(Command::Function(n, _)) if n == "f"));
        Ok(())
    }

    #[test]
    fn parses_redirection_forms() -> TestResult {
        let list =
            program("cmd 2>err 2>>err2 >&2 2>&1 &>both &>>both2 >|force <>rw <&- 3<in <<<str")?;
        let Some(Command::Simple(s)) = first_cmd(&list) else {
            return Err("not simple".into());
        };
        assert_eq!(s.redirects.len(), 11);
        assert_eq!(s.redirects.first().and_then(|r| r.fd), Some(2));
        assert!(matches!(
            s.redirects.get(6).map(|r| &r.op),
            Some(RedirOp::Out { clobber: true, .. })
        ));
        Ok(())
    }

    #[test]
    fn parses_heredocs() -> TestResult {
        let list = program("cat <<EOF; echo after\nhello $USER\nEOF\necho next")?;
        assert_eq!(list.0.len(), 3);
        let Some(Command::Simple(s)) = first_cmd(&list) else {
            return Err("not simple".into());
        };
        let Some(RedirOp::HereDoc(body)) = s.redirects.first().map(|r| &r.op) else {
            return Err("no heredoc".into());
        };
        assert!(body.segs.iter().any(|s| matches!(s, Seg::Param { .. })));
        let list = program("cat <<-'X'\n\t$a\n\tX")?;
        let Some(Command::Simple(s)) = first_cmd(&list) else {
            return Err("not simple".into());
        };
        assert_eq!(
            s.redirects.first().map(|r| r.op.clone()),
            Some(RedirOp::HereDoc(crate::ast::Word::lit("$a\n", true)))
        );
        Ok(())
    }

    #[test]
    fn parses_parameter_operators() -> TestResult {
        let list = program(
            "echo ${a:-x} ${a=y} ${a:+z} ${a:?e} ${#a} ${a#p} ${a##p} ${a%p} ${a%%p} ${a/x/y} ${a//x} ${a:1:2} ${a^^} ${a,} $1 ${10} $@ $# $*",
        )?;
        let Some(Command::Simple(s)) = first_cmd(&list) else {
            return Err("not simple".into());
        };
        assert_eq!(s.words.len(), 20);
        assert!(program("echo ${!x}").is_err());
        Ok(())
    }

    #[test]
    fn parses_substitutions() -> TestResult {
        let list = program("echo $(echo \"$(date)\") `pwd` $((1 + $x)) <(ls) >(cat) $'a\\tb'")?;
        let Some(Command::Simple(s)) = first_cmd(&list) else {
            return Err("not simple".into());
        };
        assert_eq!(s.words.len(), 7);
        assert_eq!(
            s.words.get(6).map(|w| w.segs.clone()),
            Some(vec![Seg::Lit {
                text: "a\tb".into(),
                quoted: true
            }])
        );
        Ok(())
    }

    #[test]
    fn parses_assignments() -> TestResult {
        let list = program("A=1 B+=2 C= cmd D=4")?;
        let Some(Command::Simple(s)) = first_cmd(&list) else {
            return Err("not simple".into());
        };
        assert_eq!(s.assigns.len(), 3);
        assert!(s.assigns.get(1).is_some_and(|a| a.append));
        assert_eq!(s.words.len(), 2);
        Ok(())
    }

    #[test]
    fn reserved_words_only_at_command_position() -> TestResult {
        let p = parse("echo if then fi done { }", &no_aliases())?;
        assert_eq!(words(&p, 0, 0).len(), 7);
        Ok(())
    }
}
