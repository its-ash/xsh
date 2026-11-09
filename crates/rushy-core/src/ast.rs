//! Syntax tree produced by [`crate::parser::parse_program`].

use std::sync::Arc;

/// Commands separated by `;`, `&`, or newlines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct List(pub Vec<Item>);

/// One entry of a [`List`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The commands of this entry.
    pub and_or: AndOr,
    /// Whether the entry ends with `&`.
    pub background: bool,
}

/// Pipelines joined by `&&` and `||`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndOr {
    /// The first pipeline.
    pub first: Pipe,
    /// The pipelines that follow, with their connectors.
    pub rest: Vec<(Connector, Pipe)>,
}

/// Connector between two pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    /// `&&`: run the next pipeline if this one succeeds.
    And,
    /// `||`: run the next pipeline if this one fails.
    Or,
}

/// Commands joined by `|`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipe {
    /// Whether the pipeline starts with `!`.
    pub negated: bool,
    /// Commands in pipeline order.
    pub commands: Vec<Command>,
}

/// One command of a pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A simple command: assignments, words, and redirections.
    Simple(Simple),
    /// A compound command with its redirections.
    Compound(Compound, Vec<Redir>),
    /// A function definition: `name() body`.
    Function(String, Arc<Command>),
}

/// A simple command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Simple {
    /// Leading `NAME=value` assignments.
    pub assigns: Vec<Assign>,
    /// Command name and arguments.
    pub words: Vec<Word>,
    /// Redirections in source order.
    pub redirects: Vec<Redir>,
}

/// A `NAME=value` or `NAME+=value` assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assign {
    /// Variable name.
    pub name: String,
    /// Whether the value is appended (`+=`).
    pub append: bool,
    /// Value before expansion.
    pub value: Word,
}

/// A compound command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compound {
    /// `{ list; }`
    Group(List),
    /// `( list )`
    Subshell(List),
    /// `if list; then list; [elif list; then list;]... [else list;] fi`
    If {
        /// Condition and body of each `if` and `elif`.
        branches: Vec<(List, List)>,
        /// The `else` body.
        otherwise: Option<List>,
    },
    /// `for name [in words]; do list; done`
    For {
        /// Loop variable.
        var: String,
        /// Words to loop over; `None` means the positional parameters.
        items: Option<Vec<Word>>,
        /// Loop body.
        body: List,
    },
    /// `for ((init; cond; step)); do list; done`
    ArithFor {
        /// Initial expression.
        init: Word,
        /// Condition expression (empty means true).
        cond: Word,
        /// Step expression.
        step: Word,
        /// Loop body.
        body: List,
    },
    /// `while list; do list; done` or `until list; do list; done`
    While {
        /// Whether this is an `until` loop.
        until: bool,
        /// Condition.
        cond: List,
        /// Loop body.
        body: List,
    },
    /// `case word in pattern) list ;; ... esac`
    Case {
        /// The word to match.
        word: Word,
        /// The arms in order.
        arms: Vec<CaseArm>,
    },
    /// `(( expression ))`
    Arith(Word),
    /// `[[ expression ]]`, as words. Operators are unquoted literal words.
    Cond(Vec<Word>),
}

/// One arm of a `case` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseArm {
    /// Patterns separated by `|`.
    pub patterns: Vec<Word>,
    /// Commands to run on a match.
    pub body: List,
    /// How the arm ends.
    pub end: CaseEnd,
}

/// Terminator of a `case` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseEnd {
    /// `;;`: stop.
    Break,
    /// `;&`: run the next arm's body too.
    Fall,
    /// `;;&`: test the next arms too.
    Test,
}

/// A redirection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redir {
    /// Explicit file descriptor before the operator.
    pub fd: Option<u32>,
    /// Operator and target.
    pub op: RedirOp,
}

/// Redirection operator and target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirOp {
    /// `< file`
    In(Word),
    /// `> file`, or `>| file` when `clobber` is set.
    Out {
        /// Target file.
        target: Word,
        /// Whether `>|` overrides `noclobber`.
        clobber: bool,
    },
    /// `>> file`
    Append(Word),
    /// `<> file`
    ReadWrite(Word),
    /// `>&N`, `>&-`, or `>& file`.
    DupOut(Word),
    /// `<&N` or `<&-`.
    DupIn(Word),
    /// `&> file` or `&>> file`.
    Both {
        /// Target file.
        target: Word,
        /// Whether to append.
        append: bool,
    },
    /// `<< DELIM` or `<<- DELIM` with its body.
    HereDoc(Word),
    /// `<<< word`
    HereString(Word),
}

/// A word made of segments, before expansion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Word {
    /// Segments in source order.
    pub segs: Vec<Seg>,
}

impl Word {
    /// The text if the word is one unquoted literal.
    #[must_use]
    pub fn plain(&self) -> Option<&str> {
        match self.segs.as_slice() {
            [
                Seg::Lit {
                    text,
                    quoted: false,
                },
            ] => Some(text),
            _ => None,
        }
    }

    /// Whether any segment is quoted.
    #[must_use]
    pub fn is_quoted(&self) -> bool {
        self.segs.iter().any(|s| match s {
            Seg::Lit { quoted, .. }
            | Seg::Param { quoted, .. }
            | Seg::Command { quoted, .. }
            | Seg::Arith { quoted, .. } => *quoted,
            _ => false,
        })
    }

    pub(crate) fn lit(text: &str, quoted: bool) -> Self {
        Self {
            segs: vec![Seg::Lit {
                text: text.to_owned(),
                quoted,
            }],
        }
    }
}

/// One piece of a [`Word`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// Literal text; quoted text is not split or globbed.
    Lit {
        /// The text, quotes removed.
        text: String,
        /// Whether the text was quoted or escaped.
        quoted: bool,
    },
    /// `~` or `~user` at the start of a word.
    Tilde(Option<String>),
    /// A parameter expansion: `$NAME` or `${...}`.
    Param {
        /// The parameter and its operator.
        param: Param,
        /// Whether it was inside double quotes.
        quoted: bool,
    },
    /// Command substitution: `$(list)` or a backquoted list.
    Command {
        /// The commands.
        body: List,
        /// Whether it was inside double quotes.
        quoted: bool,
    },
    /// Arithmetic expansion: `$((expr))`.
    Arith {
        /// The expression, before expansion.
        expr: Word,
        /// Whether it was inside double quotes.
        quoted: bool,
    },
    /// Process substitution `<(list)`: expands to a file with its output.
    ProcIn(List),
    /// Process substitution `>(list)`: expands to a file fed to the list.
    ProcOut(List),
}

/// A parameter and its expansion operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Name: a variable, a digit string, or one of `? $ # @ * ! - 0`.
    pub name: String,
    /// The operator.
    pub op: ParamOp,
}

/// Parameter expansion operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamOp {
    /// `$NAME` or `${NAME}`.
    Plain,
    /// `${#NAME}`.
    Length,
    /// `${NAME-word}` or `${NAME:-word}`.
    Default {
        /// Whether an empty value counts as unset.
        colon: bool,
        /// The fallback.
        word: Word,
    },
    /// `${NAME=word}` or `${NAME:=word}`.
    Assign {
        /// Whether an empty value counts as unset.
        colon: bool,
        /// The value to assign.
        word: Word,
    },
    /// `${NAME+word}` or `${NAME:+word}`.
    Alt {
        /// Whether an empty value counts as unset.
        colon: bool,
        /// The alternative.
        word: Word,
    },
    /// `${NAME?word}` or `${NAME:?word}`.
    Error {
        /// Whether an empty value counts as unset.
        colon: bool,
        /// The message.
        word: Word,
    },
    /// `${NAME#pat}` or `${NAME##pat}`.
    Prefix {
        /// Whether to remove the longest match.
        longest: bool,
        /// The pattern.
        pattern: Word,
    },
    /// `${NAME%pat}` or `${NAME%%pat}`.
    Suffix {
        /// Whether to remove the longest match.
        longest: bool,
        /// The pattern.
        pattern: Word,
    },
    /// `${NAME/pat/rep}` and its variants.
    Replace {
        /// Which matches to replace.
        mode: ReplaceMode,
        /// The pattern.
        pattern: Word,
        /// The replacement.
        with: Word,
    },
    /// `${NAME:offset}` or `${NAME:offset:length}`.
    Slice {
        /// Arithmetic offset.
        offset: Word,
        /// Arithmetic length.
        length: Option<Word>,
    },
    /// `${NAME^}` or `${NAME^^}`.
    Upper {
        /// Whether to change every character.
        all: bool,
    },
    /// `${NAME,}` or `${NAME,,}`.
    Lower {
        /// Whether to change every character.
        all: bool,
    },
}

/// Which matches `${NAME/pat/rep}` replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceMode {
    /// `/`: the first match.
    First,
    /// `//`: every match.
    All,
    /// `/#`: a match at the start.
    Prefix,
    /// `/%`: a match at the end.
    Suffix,
}
