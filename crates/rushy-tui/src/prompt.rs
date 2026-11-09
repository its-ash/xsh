//! Prompt rendering: theme template plus bundle segments with priorities and
//! truncation.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use ratatui::style::Color;
use ratatui::text::{Line, Span};
use rushy_bundles::Segment;

use crate::theme::{Theme, parse_color};

/// Placeholder values per segment name, such as `git` → `{branch: "main"}`.
pub type Values = BTreeMap<String, BTreeMap<String, String>>;

/// Fixed prompt fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptInput<'a> {
    /// User name.
    pub user: &'a str,
    /// Host name.
    pub host: &'a str,
    /// Display path (with `~`).
    pub path: &'a str,
    /// Last exit status; non-zero is shown as `[N] `.
    pub status: i32,
}

/// Rendering limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOpts {
    /// Maximum prompt width in columns.
    pub width: u16,
    /// Use `~` instead of `…` when cutting text.
    pub ascii: bool,
}

/// A run of prompt text with one color.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    /// Text.
    pub text: String,
    /// Color.
    pub color: Color,
}

/// Cuts `text` to `max` columns, ending with an ellipsis when cut.
///
/// ```
/// assert_eq!(rushy_tui::truncate("feature/long-branch", 8, true), "feature~");
/// assert_eq!(rushy_tui::truncate("main", 8, true), "main");
/// ```
#[must_use]
pub fn truncate(text: &str, max: usize, ascii: bool) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mark = if ascii { '~' } else { '…' };
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    if max > 0 {
        out.push(mark);
    }
    out
}

enum Token {
    Lit(String),
    Field(String),
}

fn tokenize(format: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut rest = format;
    while let Some(open) = rest.find('{') {
        let after = rest.get(open + 1..).unwrap_or_default();
        match after.find('}') {
            Some(close) => {
                lit.push_str(rest.get(..open).unwrap_or_default());
                if !lit.is_empty() {
                    out.push(Token::Lit(std::mem::take(&mut lit)));
                }
                out.push(Token::Field(
                    after.get(..close).unwrap_or_default().to_owned(),
                ));
                rest = after.get(close + 1..).unwrap_or_default();
            }
            None => break,
        }
    }
    lit.push_str(rest);
    if !lit.is_empty() {
        out.push(Token::Lit(lit));
    }
    out
}

fn fill(format: &str, values: &BTreeMap<String, String>) -> String {
    tokenize(format)
        .into_iter()
        .map(|t| match t {
            Token::Lit(s) => s,
            Token::Field(k) => values.get(&k).cloned().unwrap_or_default(),
        })
        .collect()
}

struct Item {
    piece: Piece,
    /// Priority of bundle segments; `None` for fixed text.
    priority: Option<u8>,
    is_path: bool,
}

/// Renders the theme's prompt into colored pieces that fit `opts.width`.
///
/// Bundle segments without values are omitted. When the prompt is too wide,
/// the lowest-priority segments are dropped first (rightmost on a tie), then
/// the path is shortened from the left.
#[must_use]
pub fn render_prompt(
    theme: &Theme,
    input: &PromptInput<'_>,
    segments: &BTreeMap<String, Segment>,
    values: &Values,
    opts: RenderOpts,
) -> Vec<Piece> {
    let fg = theme.foreground;
    let mut items: Vec<Item> = Vec::new();
    let mut push = |text: String, color: Color, priority: Option<u8>, is_path: bool| {
        if !text.is_empty() {
            items.push(Item {
                piece: Piece { text, color },
                priority,
                is_path,
            });
        }
    };
    for token in tokenize(&theme.prompt_format) {
        match token {
            Token::Lit(s) => push(s, fg, None, false),
            Token::Field(name) => match name.as_str() {
                "user" => push(input.user.to_owned(), theme.prompt_user, None, false),
                "host" => push(input.host.to_owned(), fg, None, false),
                "path" => push(input.path.to_owned(), theme.prompt_path, None, true),
                "status" if input.status != 0 => {
                    push(format!("[{}] ", input.status), theme.error, None, false);
                }
                other => {
                    if let (Some(seg), Some(vals)) = (segments.get(other), values.get(other)) {
                        let mut text = fill(&seg.format, vals);
                        if let Some(max) = seg.max_width {
                            text = truncate(&text, usize::from(max), opts.ascii);
                        }
                        let color = parse_color(&seg.color).unwrap_or(fg);
                        push(text, color, Some(seg.priority), false);
                    }
                }
            },
        }
    }
    let width = usize::from(opts.width);
    let total = |items: &[Item]| {
        items
            .iter()
            .map(|i| i.piece.text.chars().count())
            .sum::<usize>()
    };
    while total(&items) > width {
        let drop = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| it.priority.map(|p| (p, std::cmp::Reverse(i))))
            .min()
            .map(|(_, std::cmp::Reverse(i))| i);
        match drop {
            Some(i) => {
                items.remove(i);
            }
            None => break,
        }
    }
    let over = total(&items).saturating_sub(width);
    if over > 0 {
        if let Some(item) = items.iter_mut().find(|i| i.is_path) {
            let chars: Vec<char> = item.piece.text.chars().collect();
            let keep = chars.len().saturating_sub(over).max(1);
            let mark = if opts.ascii { '~' } else { '…' };
            let tail: String = chars
                .iter()
                .skip(chars.len() - keep.min(chars.len()))
                .collect();
            item.piece.text = format!("{mark}{}", tail.chars().skip(1).collect::<String>());
        }
    }
    items.into_iter().map(|i| i.piece).collect()
}

/// Joins pieces into plain text.
#[must_use]
pub fn plain(pieces: &[Piece]) -> String {
    pieces.iter().map(|p| p.text.as_str()).collect()
}

fn sgr(color: Color) -> Option<String> {
    Some(match color {
        Color::Reset => return None,
        Color::Black => "30".into(),
        Color::Red => "31".into(),
        Color::Green => "32".into(),
        Color::Yellow => "33".into(),
        Color::Blue => "34".into(),
        Color::Magenta => "35".into(),
        Color::Cyan => "36".into(),
        Color::Gray => "37".into(),
        Color::DarkGray => "90".into(),
        Color::LightRed => "91".into(),
        Color::LightGreen => "92".into(),
        Color::LightYellow => "93".into(),
        Color::LightBlue => "94".into(),
        Color::LightMagenta => "95".into(),
        Color::LightCyan => "96".into(),
        Color::White => "97".into(),
        Color::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        Color::Indexed(i) => format!("38;5;{i}"),
    })
}

/// Joins pieces into text with ANSI foreground colors, for line editors that
/// take a prompt string. Pieces with the terminal's default color get no
/// escape codes.
///
/// ```
/// use ratatui::style::Color;
/// let p = [rushy_tui::Piece { text: "hi".into(), color: Color::Rgb(1, 2, 3) }];
/// assert_eq!(rushy_tui::to_ansi(&p), "\x1b[38;2;1;2;3mhi\x1b[0m");
/// ```
#[must_use]
pub fn to_ansi(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| match sgr(p.color) {
            Some(code) => format!("\x1b[{code}m{}\x1b[0m", p.text),
            None => p.text.clone(),
        })
        .collect()
}

/// Converts pieces to a styled line.
#[must_use]
pub fn to_line(theme: &Theme, pieces: &[Piece]) -> Line<'static> {
    Line::from(
        pieces
            .iter()
            .map(|p| Span::styled(p.text.clone(), theme.style(p.color)))
            .collect::<Vec<_>>(),
    )
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn git_branch(cwd: &Path) -> Option<String> {
    let dot_git = cwd
        .ancestors()
        .map(|d| d.join(".git"))
        .find(|p| p.exists())?;
    let git_dir = if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let rel = text.trim().strip_prefix("gitdir:")?.trim();
        dot_git.parent()?.join(rel)
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => branch.to_owned(),
        None => head.chars().take(7).collect(),
    })
}

/// Placeholder values for the built-in segments `git`, `node`, and `python`.
///
/// This reads files and runs short commands, so call it from
/// `spawn_blocking`. Returns `None` for other segments or when the tool is
/// missing.
#[must_use]
pub fn segment_values(name: &str, cwd: &Path) -> Option<BTreeMap<String, String>> {
    let mut v = BTreeMap::new();
    match name {
        "git" => {
            v.insert("branch".to_owned(), git_branch(cwd)?);
            let dirty = run("git", &["status", "--porcelain"], cwd).is_some_and(|s| !s.is_empty());
            v.insert(
                "dirty".to_owned(),
                if dirty { "*".into() } else { String::new() },
            );
        }
        "node" => {
            let out = run("node", &["--version"], cwd)?;
            v.insert("version".to_owned(), out.trim_start_matches('v').to_owned());
        }
        "python" => {
            let out = run("python3", &["--version"], cwd)?;
            v.insert(
                "version".to_owned(),
                out.split_whitespace().last()?.to_owned(),
            );
        }
        _ => return None,
    }
    Some(v)
}
