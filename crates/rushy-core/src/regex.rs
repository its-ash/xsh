//! A small backtracking regular expression engine for `[[ s =~ re ]]`.
//!
//! Supports literals, `.`, `[...]` (ranges, negation, `[:class:]`), `^`,
//! `$`, `*`, `+`, `?`, `{m}`, `{m,}`, `{m,n}`, groups, `|`, and the escapes
//! `\d \w \s \D \W \S` and `\` before any other character.

const MAX_STEPS: usize = 1_000_000;

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class {
        neg: bool,
        items: Vec<Item>,
    },
    Start,
    End,
    Group(Vec<Vec<Node>>),
    Repeat {
        node: Box<Node>,
        min: usize,
        max: Option<usize>,
    },
}

#[derive(Debug, Clone)]
enum Item {
    Ch(char),
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
    Named(String),
}

struct Parser {
    s: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.s.get(self.pos).copied()
    }

    fn alt(&mut self) -> Result<Vec<Vec<Node>>, String> {
        let mut alts = vec![self.seq()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            alts.push(self.seq()?);
        }
        Ok(alts)
    }

    fn seq(&mut self) -> Result<Vec<Node>, String> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            self.pos += 1;
            let atom = match c {
                '.' => Node::Any,
                '^' => Node::Start,
                '$' => Node::End,
                '(' => {
                    let g = self.alt()?;
                    if self.peek() != Some(')') {
                        return Err("regex: missing `)`".into());
                    }
                    self.pos += 1;
                    Node::Group(g)
                }
                '[' => self.class()?,
                '\\' => {
                    let e = self.peek().ok_or("regex: trailing `\\`")?;
                    self.pos += 1;
                    match escape_item(e) {
                        Some(item) => Node::Class {
                            neg: false,
                            items: vec![item],
                        },
                        None => Node::Char(e),
                    }
                }
                '*' | '+' | '?' => return Err(format!("regex: nothing to repeat before `{c}`")),
                _ => Node::Char(c),
            };
            out.push(self.quantify(atom)?);
        }
        Ok(out)
    }

    fn quantify(&mut self, mut node: Node) -> Result<Node, String> {
        loop {
            let (min, max) = match self.peek() {
                Some('*') => (0, None),
                Some('+') => (1, None),
                Some('?') => (0, Some(1)),
                Some('{') => match self.braces() {
                    Some(r) => r,
                    None => return Ok(node),
                },
                _ => return Ok(node),
            };
            self.pos += 1;
            if matches!(node, Node::Start | Node::End) {
                return Err("regex: cannot repeat an anchor".into());
            }
            node = Node::Repeat {
                node: Box::new(node),
                min,
                max,
            };
        }
    }

    /// Parses `{m}`, `{m,}`, or `{m,n}`; leaves the cursor on `}`.
    fn braces(&mut self) -> Option<(usize, Option<usize>)> {
        let rest: String = self.s.get(self.pos + 1..)?.iter().collect();
        let end = rest.find('}')?;
        let body = &rest[..end];
        let (min, max) = match body.split_once(',') {
            None => {
                let n = body.parse().ok()?;
                (n, Some(n))
            }
            Some((a, "")) => (a.parse().ok()?, None),
            Some((a, b)) => (a.parse().ok()?, Some(b.parse().ok()?)),
        };
        self.pos += 1 + body.chars().count();
        Some((min, max))
    }

    fn class(&mut self) -> Result<Node, String> {
        let neg = self.peek() == Some('^');
        if neg {
            self.pos += 1;
        }
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or("regex: missing `]`")?;
            self.pos += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            if c == '[' && self.peek() == Some(':') {
                let rest: String = self
                    .s
                    .get(self.pos + 1..)
                    .unwrap_or_default()
                    .iter()
                    .collect();
                if let Some(end) = rest.find(":]") {
                    items.push(Item::Named(rest[..end].to_owned()));
                    self.pos += 1 + rest[..end].chars().count() + 2;
                    continue;
                }
            }
            let c = if c == '\\' {
                let e = self.peek().ok_or("regex: missing `]`")?;
                self.pos += 1;
                if let Some(item) = escape_item(e) {
                    items.push(item);
                    continue;
                }
                e
            } else {
                c
            };
            if self.peek() == Some('-') && self.s.get(self.pos + 1).is_some_and(|e| *e != ']') {
                let end = self.s.get(self.pos + 1).copied().unwrap_or(c);
                self.pos += 2;
                items.push(Item::Range(c, end));
            } else {
                items.push(Item::Ch(c));
            }
        }
        Ok(Node::Class { neg, items })
    }
}

fn escape_item(e: char) -> Option<Item> {
    Some(match e {
        'd' => Item::Digit(true),
        'D' => Item::Digit(false),
        'w' => Item::Word(true),
        'W' => Item::Word(false),
        's' => Item::Space(true),
        'S' => Item::Space(false),
        _ => return None,
    })
}

fn item_matches(item: &Item, c: char) -> bool {
    match item {
        Item::Ch(x) => *x == c,
        Item::Range(a, b) => (*a..=*b).contains(&c),
        Item::Digit(on) => c.is_ascii_digit() == *on,
        Item::Word(on) => (c.is_alphanumeric() || c == '_') == *on,
        Item::Space(on) => c.is_whitespace() == *on,
        Item::Named(n) => match n.as_str() {
            "alpha" => c.is_alphabetic(),
            "digit" => c.is_ascii_digit(),
            "alnum" => c.is_alphanumeric(),
            "upper" => c.is_uppercase(),
            "lower" => c.is_lowercase(),
            "space" => c.is_whitespace(),
            "blank" => c == ' ' || c == '\t',
            "punct" => c.is_ascii_punctuation(),
            "xdigit" => c.is_ascii_hexdigit(),
            _ => false,
        },
    }
}

struct Matcher<'a> {
    text: &'a [char],
    steps: usize,
}

impl Matcher<'_> {
    /// Matches `nodes` at `i`, then calls `k` with the end position.
    fn seq(
        &mut self,
        nodes: &[Node],
        i: usize,
        k: &mut dyn FnMut(&mut Self, usize) -> bool,
    ) -> bool {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return false;
        }
        let Some((first, rest)) = nodes.split_first() else {
            return k(self, i);
        };
        match first {
            Node::Start => i == 0 && self.seq(rest, i, k),
            Node::End => i == self.text.len() && self.seq(rest, i, k),
            Node::Group(alts) => alts
                .iter()
                .any(|alt| self.seq(alt, i, &mut |m: &mut Self, j| m.seq(rest, j, k))),
            Node::Repeat { node, min, max } => self.repeat(node, *min, *max, 0, i, rest, k),
            single => match self.text.get(i) {
                Some(&c) if single_matches(single, c) => self.seq(rest, i + 1, k),
                _ => false,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &mut self,
        node: &Node,
        min: usize,
        max: Option<usize>,
        count: usize,
        i: usize,
        rest: &[Node],
        k: &mut dyn FnMut(&mut Self, usize) -> bool,
    ) -> bool {
        if max.is_none_or(|m| count < m) {
            let more = self.seq(std::slice::from_ref(node), i, &mut |m: &mut Self, j| {
                j != i && m.repeat(node, min, max, count + 1, j, rest, k)
            });
            if more {
                return true;
            }
        }
        count >= min && self.seq(rest, i, k)
    }
}

fn single_matches(node: &Node, c: char) -> bool {
    match node {
        Node::Char(x) => *x == c,
        Node::Any => c != '\n',
        Node::Class { neg, items } => items.iter().any(|it| item_matches(it, c)) != *neg,
        _ => false,
    }
}

/// Whether `re` matches anywhere in `text`.
pub(crate) fn is_match(re: &str, text: &str) -> Result<bool, String> {
    let mut p = Parser {
        s: re.chars().collect(),
        pos: 0,
    };
    let alts = p.alt()?;
    if p.pos < p.s.len() {
        return Err("regex: unmatched `)`".into());
    }
    let prog = vec![Node::Group(alts)];
    let chars: Vec<char> = text.chars().collect();
    let mut m = Matcher {
        text: &chars,
        steps: 0,
    };
    Ok((0..=chars.len()).any(|start| m.seq(&prog, start, &mut |_, _| true)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_common_patterns() -> Result<(), String> {
        for (re, text, want) in [
            ("abc", "xxabcxx", true),
            ("^abc$", "abc", true),
            ("^abc$", "abcd", false),
            ("a.c", "abc", true),
            ("a*b", "b", true),
            ("a+b", "b", false),
            ("colou?r", "color", true),
            ("^[0-9]+$", "12345", true),
            ("^[0-9]+$", "12a45", false),
            ("^(foo|bar)baz$", "barbaz", true),
            ("^\\d{3}-\\d{4}$", "555-1234", true),
            ("^a{2,3}$", "aaaa", false),
            ("^a{2,}$", "aaaa", true),
            ("[[:upper:]]", "abC", true),
            ("^[^a-z]+$", "ABC1", true),
            ("\\.rs$", "main.rs", true),
            ("(a|ab)(c|bcd)(d*)", "abcd", true),
        ] {
            assert_eq!(is_match(re, text)?, want, "{re} {text}");
        }
        assert!(is_match("(", "x").is_err());
        assert!(is_match("*a", "x").is_err());
        Ok(())
    }
}
