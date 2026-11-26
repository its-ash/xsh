//! Terminal formatting for AI answers.
//!
//! - [`markdown`] renders the Markdown that models write: headings, lists,
//!   quotes, code blocks, `inline code`, **bold**, and pipe tables, which
//!   become box-drawn tables that fit the terminal width.
//! - [`Pal::bubble`] puts an answer in a speech bubble said by the xsh mascot.
//! - [`Pal::card_head`] and friends draw one agent step as a card.
//!
//! With the `mono` theme, `NO_COLOR`, or output that is not a terminal there
//! are no colors. In screen-reader mode only ASCII is used.

use std::fmt::Write as _;

use nu_ansi_term::{Color, Style};
use ratatui::style::Color as Ui;
use rushy_tui::Theme;

/// The widest a block of AI text gets, even on a wide terminal.
const MAX_WIDTH: usize = 100;

/// Display width of plain text (wide characters count as two columns).
fn width(s: &str) -> usize {
    ratatui::text::Span::raw(s).width()
}

fn nu(c: Ui) -> Option<Color> {
    Some(match c {
        Ui::Reset => return None,
        Ui::Black => Color::Black,
        Ui::Red => Color::Red,
        Ui::Green => Color::Green,
        Ui::Yellow => Color::Yellow,
        Ui::Blue => Color::Blue,
        Ui::Magenta => Color::Magenta,
        Ui::Cyan => Color::Cyan,
        Ui::Gray => Color::LightGray,
        Ui::DarkGray => Color::DarkGray,
        Ui::LightRed => Color::LightRed,
        Ui::LightGreen => Color::LightGreen,
        Ui::LightYellow => Color::LightYellow,
        Ui::LightBlue => Color::LightBlue,
        Ui::LightMagenta => Color::LightMagenta,
        Ui::LightCyan => Color::LightCyan,
        Ui::White => Color::White,
        Ui::Rgb(r, g, b) => Color::Rgb(r, g, b),
        Ui::Indexed(i) => Color::Fixed(i),
    })
}

fn fg(c: Ui) -> Style {
    nu(c).map_or_else(Style::new, |c| Style::new().fg(c))
}

/// Box-drawing characters, or their ASCII stand-ins.
#[derive(Debug, Clone, Copy)]
struct Glyphs {
    h: &'static str,
    v: &'static str,
    tl: &'static str,
    tr: &'static str,
    bl: &'static str,
    br: &'static str,
    lt: &'static str,
    rt: &'static str,
    tt: &'static str,
    bt: &'static str,
    cross: &'static str,
    bullet: &'static str,
    ok: &'static str,
    fail: &'static str,
}

const UNICODE: Glyphs = Glyphs {
    h: "─",
    v: "│",
    tl: "╭",
    tr: "╮",
    bl: "╰",
    br: "╯",
    lt: "├",
    rt: "┤",
    tt: "┬",
    bt: "┴",
    cross: "┼",
    bullet: "•",
    ok: "✓",
    fail: "✗",
};

const ASCII: Glyphs = Glyphs {
    h: "-",
    v: "|",
    tl: "+",
    tr: "+",
    bl: "+",
    br: "+",
    lt: "+",
    rt: "+",
    tt: "+",
    bt: "+",
    cross: "+",
    bullet: "*",
    ok: "ok",
    fail: "x",
};

/// Colors and glyphs for AI output.
#[derive(Debug, Clone)]
pub struct Pal {
    g: Glyphs,
    color: bool,
    accent: Style,
    ok: Style,
    err: Style,
    dim: Style,
    code: Style,
    /// Columns available.
    pub width: usize,
}

/// One styled line and its display width.
pub type Out = (String, usize);

impl Pal {
    /// Plain output: no colors, `width` columns.
    #[must_use]
    pub fn plain(ascii: bool, width: usize) -> Self {
        Self {
            g: if ascii { ASCII } else { UNICODE },
            color: false,
            accent: Style::new(),
            ok: Style::new(),
            err: Style::new(),
            dim: Style::new(),
            code: Style::new(),
            width: width.clamp(20, MAX_WIDTH),
        }
    }

    /// Styles from `theme`. `color` is false when the output is not a terminal.
    #[must_use]
    pub fn new(theme: &Theme, ascii: bool, color: bool, width: usize) -> Self {
        let mut p = Self::plain(ascii, width);
        if color && !theme.plain {
            p.color = true;
            p.accent = fg(theme.accent);
            p.ok = fg(theme.success);
            p.err = fg(theme.error);
            p.dim = fg(theme.foreground).dimmed();
            p.code = fg(theme.prompt_path);
        }
        p
    }

    /// The palette for the current process: the app's theme and settings,
    /// and the width of the terminal on standard error.
    #[must_use]
    pub fn for_app(app: &crate::app::App) -> Self {
        use std::io::IsTerminal as _;
        let tty = std::io::stderr().is_terminal() && std::io::stdout().is_terminal();
        let cols = ratatui::crossterm::terminal::size().map_or(80, |(c, _)| usize::from(c));
        Self::new(
            &app.theme,
            app.cfg.ui.screen_reader,
            tty && !app.cfg.ui.no_color && std::env::var_os("NO_COLOR").is_none(),
            cols.saturating_sub(2),
        )
    }

    fn paint(&self, style: Style, text: &str) -> String {
        if self.color && style != Style::new() {
            style.paint(text).to_string()
        } else {
            text.to_owned()
        }
    }

    fn bold(&self, text: &str) -> String {
        self.paint(Style::new().bold(), text)
    }

    /// Dimmed text, for status lines such as "thinking".
    #[must_use]
    pub fn dim(&self, text: &str) -> String {
        self.paint(self.dim, text)
    }

    fn rule(&self, n: usize) -> String {
        self.g.h.repeat(n)
    }

    // ------------------------------------------------------------ bubble

    /// `text` (Markdown) in a speech bubble, said by the xsh robot named `who`.
    #[must_use]
    pub fn bubble(&self, text: &str, who: &str) -> String {
        let inner = self.width.saturating_sub(4).max(16);
        let mut lines = markdown(text, inner, self);
        while lines.last().is_some_and(|(_, w)| *w == 0) {
            lines.pop();
        }
        if lines.is_empty() {
            lines.push((String::new(), 0));
        }
        let w = lines.iter().map(|(_, w)| *w).max().unwrap_or(0).max(12);
        let g = &self.g;
        let border = |s: &str| self.paint(self.accent, s);
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{}",
            border(&format!("{}{}{}", g.tl, self.rule(w + 2), g.tr))
        );
        for (line, lw) in &lines {
            let pad = " ".repeat(w.saturating_sub(*lw));
            let _ = writeln!(out, "{} {line}{pad} {}", border(g.v), border(g.v));
        }
        let _ = writeln!(
            out,
            "{}",
            border(&format!("{}{}{}", g.bl, self.rule(w + 2), g.br))
        );
        out.push_str(&self.mascot(who));
        out
    }

    /// The end of an agent run: the question, the answer, and a table of
    /// every step with its result.
    #[must_use]
    pub fn agent_report(&self, question: &str, answer: &str, steps: &[StepLog]) -> String {
        let mut md = format!(
            "### Question\n{}\n\n### Answer\n{}\n",
            question.trim(),
            answer.trim()
        );
        if !steps.is_empty() {
            md.push_str("\n### Execution\n| # | Command | Result | Output |\n|---|---|---|---|\n");
            for (n, s) in steps.iter().enumerate() {
                let _ = writeln!(
                    md,
                    "| {} | {} | {} | {} |",
                    n + 1,
                    code_cell(&s.command),
                    s.result.replace('|', "\\|"),
                    code_cell(&truncate(&s.output, 60)),
                );
            }
        }
        self.bubble(&md, "xsh agent")
    }

    fn mascot(&self, who: &str) -> String {
        let (top, face, feet) = if self.g.h == "-" {
            (".---.", "|o_o|", "'---'")
        } else {
            ("╭───╮", "│•‿•│", "╰┬─┬╯")
        };
        let a = |s: &str| self.paint(self.accent, s);
        format!(
            "    \\   {}\n     \\  {}  {}\n        {}\n",
            a(top),
            a(face),
            self.bold(who),
            a(feet)
        )
    }

    // ------------------------------------------------------------ step cards

    /// `╭─ title ────` on its own line.
    fn card_top(&self, title: &str) -> String {
        let g = &self.g;
        let title = format!(" {title} ");
        let rest = self.width.saturating_sub(width(&title) + 2).min(40);
        format!(
            "\n{}{}{}\n",
            self.paint(self.accent, &format!("{}{}", g.tl, g.h)),
            self.paint(self.accent.bold(), &title),
            self.paint(self.accent, &self.rule(rest))
        )
    }

    /// The question an agent run starts with, before any task.
    #[must_use]
    pub fn question(&self, goal: &str) -> String {
        let mut out = self.card_top("Question");
        let bar = self.paint(self.accent, self.g.v);
        for line in wrap(&spans(goal.trim()), self.width.saturating_sub(4).max(16)) {
            let (text, _) = self.styled(&line);
            let _ = writeln!(out, "{bar} {}", self.bold(&text));
        }
        let _ = writeln!(
            out,
            "{}",
            self.paint(self.accent, &format!("{}{}", self.g.bl, self.rule(20)))
        );
        out
    }

    /// The top of a task card: the task number, the command, and why.
    #[must_use]
    pub fn card_head(&self, step: usize, command: &str, reason: &str) -> String {
        let g = &self.g;
        let mut out = self.card_top(&format!("Task {step}"));
        let bar = self.paint(self.accent, g.v);
        let _ = writeln!(
            out,
            "{bar} {} {}",
            self.paint(self.dim, "$"),
            self.paint(self.code.bold(), command)
        );
        if !reason.is_empty() {
            let wrapped = wrap(&spans(reason), self.width.saturating_sub(4).max(16));
            for line in wrapped {
                let (text, _) = self.styled(&line);
                let _ = writeln!(out, "{bar} {}", self.paint(self.dim, &text));
            }
        }
        out
    }

    /// The line between a task's command and its response (the output).
    #[must_use]
    pub fn card_response(&self) -> String {
        format!(
            "{} {}\n",
            self.paint(self.accent, &format!("{}{}", self.g.lt, self.g.h)),
            self.paint(self.accent.bold(), "Response")
        )
    }

    /// Shown under "Response" when the command printed nothing.
    #[must_use]
    pub fn card_no_output(&self) -> String {
        format!(
            "{} {}\n",
            self.paint(self.accent, self.g.v),
            self.dim("(no output)")
        )
    }

    /// The last lines of `output`, shown in the card when the command ran
    /// somewhere else (the agent pane), so its output is not on this screen.
    #[must_use]
    pub fn card_preview(&self, output: &str, max_lines: usize) -> String {
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            return String::new();
        }
        let bar = self.paint(self.accent, self.g.v);
        let mut out = String::new();
        let skipped = lines.len().saturating_sub(max_lines);
        if skipped > 0 {
            let _ = writeln!(
                out,
                "{bar} {}",
                self.dim(&format!("… {skipped} more line(s)"))
            );
        }
        let room = self.width.saturating_sub(4).max(16);
        for l in &lines[skipped..] {
            let clean = strip_ansi(l);
            let _ = writeln!(out, "{bar} {}", self.dim(&truncate(&clean, room)));
        }
        out
    }

    /// The bottom of a step card with the exit status.
    #[must_use]
    pub fn card_status(&self, status: i32) -> String {
        let g = &self.g;
        let corner = self.paint(self.accent, &format!("{}{}", g.bl, g.h));
        if status == 0 {
            format!(
                "{corner} {}\n",
                self.paint(self.ok, &format!("{} exit 0", g.ok))
            )
        } else {
            format!(
                "{corner} {}\n",
                self.paint(self.err, &format!("{} exit {status}", g.fail))
            )
        }
    }

    /// The bottom of a step card with a note (refused, skipped).
    #[must_use]
    pub fn card_note(&self, failed: bool, note: &str) -> String {
        let g = &self.g;
        let corner = self.paint(self.accent, &format!("{}{}", g.bl, g.h));
        let style = if failed { self.err } else { self.dim };
        format!("{corner} {}\n", self.paint(style, note))
    }

    /// `text` (Markdown) rendered for the terminal, ending in a newline.
    #[must_use]
    pub fn answer(&self, text: &str) -> String {
        let mut out = String::new();
        for (line, _) in markdown(text, self.width, self) {
            let _ = writeln!(out, "{line}");
        }
        out
    }

    /// A suggested command and its one-line explanation.
    #[must_use]
    pub fn suggestion(&self, command: &str, note: &str) -> String {
        let mut out = format!(
            "  {} {}\n",
            self.paint(self.dim, "$"),
            self.paint(self.code.bold(), command)
        );
        if !note.is_empty() {
            for line in wrap(&spans(note), self.width.saturating_sub(4).max(16)) {
                let (text, _) = self.styled(&line);
                let _ = writeln!(out, "  {}", self.paint(self.dim, &text));
            }
        }
        out
    }

    /// The short note the model writes with a command.
    #[must_use]
    pub fn said(&self, text: &str) -> String {
        let mut out = String::new();
        for (line, _) in markdown(text, self.width.saturating_sub(2).max(16), self) {
            let _ = writeln!(out, "{} {line}", self.paint(self.accent, "›"));
        }
        out
    }

    // ------------------------------------------------------------ inline

    fn styled(&self, tokens: &[Tok]) -> Out {
        let mut s = String::new();
        let mut w = 0;
        for t in tokens {
            if t.space {
                s.push(' ');
                w += 1;
            }
            let style = match t.kind {
                Kind::Plain => Style::new(),
                Kind::Bold => Style::new().bold(),
                Kind::Italic => Style::new().italic(),
                Kind::Code => self.code,
            };
            s.push_str(&self.paint(style, &t.text));
            w += width(&t.text);
        }
        (s, w)
    }
}

/// One agent step, for the table at the end of the run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepLog {
    /// The command.
    pub command: String,
    /// `ok (exit 0)`, `failed (exit 1)`, `refused`, `skipped`, or `stopped`.
    pub result: String,
    /// The last line of its output, if any.
    pub output: String,
}

impl StepLog {
    /// The last non-empty line of `stdout`, or of `stderr` when stdout has none.
    #[must_use]
    pub fn last_line(stdout: &str, stderr: &str) -> String {
        [stdout, stderr]
            .iter()
            .find_map(|s| {
                s.lines()
                    .rev()
                    .map(strip_ansi)
                    .find(|l| !l.trim().is_empty())
            })
            .map(|l| l.trim().to_owned())
            .unwrap_or_default()
    }
}

/// `text` as a table cell shown literally: in a code span (so `*` is not
/// italics), with `|` escaped.
fn code_cell(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let text = text.replace('|', "\\|");
    if text.contains('`') {
        text
    } else {
        format!("`{text}`")
    }
}

fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = width(c.encode_utf8(&mut [0; 4]));
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Removes ANSI escape sequences, so command output cannot change the colors
/// of the card or move the cursor.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.peek().copied() {
                Some('[') => {
                    it.next();
                    for c in it.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(c) = it.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && it.peek() == Some(&'\\') {
                            it.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    it.next();
                }
                None => {}
            }
        } else if c == '\t' {
            out.push_str("    ");
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------- inline Markdown

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Bold,
    Italic,
    Code,
}

/// A word with its style, and whether a space comes before it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tok {
    text: String,
    kind: Kind,
    space: bool,
}

/// Splits `text` into styled words: `code`, **bold**, *italic*. Unclosed
/// markers stay as literal text.
fn spans(text: &str) -> Vec<Tok> {
    let chars: Vec<char> = text.chars().collect();
    let mut runs: Vec<(String, Kind)> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        (from..chars.len().saturating_sub(pat.len() - 1)).find(|&j| chars[j..j + pat.len()] == *pat)
    };
    while i < chars.len() {
        let c = chars[i];
        let (pat, kind): (&[char], Kind) = match c {
            '`' => (&['`'], Kind::Code),
            '*' if chars.get(i + 1) == Some(&'*') => (&['*', '*'], Kind::Bold),
            '*' if chars.get(i + 1).is_some_and(|n| !n.is_whitespace()) => (&['*'], Kind::Italic),
            _ => {
                plain.push(c);
                i += 1;
                continue;
            }
        };
        let start = i + pat.len();
        if let Some(end) = find(start, pat).filter(|&end| end > start) {
            if !plain.is_empty() {
                runs.push((std::mem::take(&mut plain), Kind::Plain));
            }
            runs.push((chars[start..end].iter().collect(), kind));
            i = end + pat.len();
        } else {
            plain.extend(&chars[i..start]);
            i = start;
        }
    }
    if !plain.is_empty() {
        runs.push((plain, Kind::Plain));
    }
    // Words keep their run's style; a space is kept only where the text had one.
    let mut toks: Vec<Tok> = Vec::new();
    let mut space = false;
    for (text, kind) in runs {
        let mut word = String::new();
        for c in text.chars() {
            if c.is_whitespace() {
                if !word.is_empty() {
                    toks.push(Tok {
                        text: std::mem::take(&mut word),
                        kind,
                        space,
                    });
                }
                space = true;
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            toks.push(Tok {
                text: word,
                kind,
                space,
            });
            space = false;
        }
    }
    if let Some(first) = toks.first_mut() {
        first.space = false;
    }
    toks
}

/// Wraps styled words into lines of at most `max` columns. Words longer than
/// a line are cut.
fn wrap(toks: &[Tok], max: usize) -> Vec<Vec<Tok>> {
    let max = max.max(1);
    let mut lines: Vec<Vec<Tok>> = Vec::new();
    let mut line: Vec<Tok> = Vec::new();
    let mut w = 0;
    for t in toks {
        let mut t = t.clone();
        loop {
            let tw = width(&t.text);
            let gap = usize::from(t.space && !line.is_empty());
            if w + gap + tw <= max {
                t.space = gap == 1;
                w += gap + tw;
                line.push(t);
                break;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                w = 0;
                t.space = false;
                continue;
            }
            // The word alone is too long: cut it.
            let text = std::mem::take(&mut t.text);
            let mut head = String::new();
            let mut hw = 0;
            let mut rest = text.chars();
            let mut tail = None;
            for c in rest.by_ref() {
                let cw = width(c.encode_utf8(&mut [0; 4]));
                if hw + cw > max && !head.is_empty() {
                    let mut more = String::from(c);
                    more.extend(rest.by_ref());
                    tail = Some(more);
                    break;
                }
                head.push(c);
                hw += cw;
            }
            let Some(more) = tail else {
                line.push(Tok {
                    text: head,
                    kind: t.kind,
                    space: false,
                });
                w = hw;
                break;
            };
            lines.push(vec![Tok {
                text: head,
                kind: t.kind,
                space: false,
            }]);
            t.text = more;
            t.space = false;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

// ---------------------------------------------------------------- block Markdown

fn is_table_sep(line: &str) -> bool {
    let t = line.trim();
    t.contains('-') && t.contains('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// The cells of a table row. `\|` is a literal `|` inside a cell.
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = match t.strip_suffix('|') {
        Some(rest) if !rest.ends_with('\\') => rest,
        _ => t,
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = t.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('|') => cur.push('|'),
                Some(other) => {
                    cur.push('\\');
                    cur.push(other);
                }
                None => cur.push('\\'),
            },
            '|' => out.push(std::mem::take(&mut cur).trim().to_owned()),
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_owned());
    out
}

fn list_item(line: &str) -> Option<(usize, String, &str)> {
    let indent = line.len() - line.trim_start().len();
    let t = line.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return Some((indent, String::new(), rest));
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits < 4 {
        let rest = &t[digits..];
        if let Some(body) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((indent, format!("{}.", &t[..digits]), body));
        }
    }
    None
}

/// Renders the fenced code block that starts at `lines[start]` and returns
/// the index of the line after it.
fn code_block(lines: &[&str], start: usize, max: usize, pal: &Pal, out: &mut Vec<Out>) -> usize {
    let fence = &lines[start].trim()[..3];
    let bar = pal.paint(pal.dim, pal.g.v);
    let mut i = start + 1;
    while i < lines.len() && !lines[i].trim().starts_with(fence) {
        let code = truncate(&strip_ansi(lines[i]), max.saturating_sub(2));
        let w = width(&code) + 2;
        out.push((format!("{bar} {}", pal.paint(pal.code, &code)), w));
        i += 1;
    }
    i + 1
}

/// Renders Markdown `text` into styled lines at most `max` columns wide.
#[must_use]
pub fn markdown(text: &str, max: usize, pal: &Pal) -> Vec<Out> {
    let max = max.max(16);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Out> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, out: &mut Vec<Out>| {
        if para.is_empty() {
            return;
        }
        let joined = para.join(" ");
        para.clear();
        out.extend(wrap(&spans(&joined), max).iter().map(|l| pal.styled(l)));
    };
    let blank = |out: &mut Vec<Out>| {
        if out.last().is_some_and(|(_, w)| *w > 0) {
            out.push((String::new(), 0));
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        // Code block.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut para, &mut out);
            i = code_block(&lines, i, max, pal, &mut out);
            continue;
        }
        // Table: a header row, then a separator row.
        if trimmed.starts_with('|') && lines.get(i + 1).is_some_and(|next| is_table_sep(next)) {
            flush(&mut para, &mut out);
            let head = cells(line);
            let mut rows = Vec::new();
            i += 2;
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(cells(lines[i]));
                i += 1;
            }
            out.extend(table(&head, &rows, max, pal));
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
            blank(&mut out);
        } else if trimmed.starts_with('#') && trimmed.trim_start_matches('#').starts_with(' ') {
            flush(&mut para, &mut out);
            let heading = trimmed.trim_start_matches('#').trim();
            for words in wrap(&spans(heading), max) {
                let plain: Vec<Tok> = words
                    .into_iter()
                    .map(|tok| Tok {
                        kind: Kind::Plain,
                        ..tok
                    })
                    .collect();
                let (text, cols) = pal.styled(&plain);
                out.push((pal.paint(pal.accent.bold(), &text), cols));
            }
        } else if matches!(trimmed, "---" | "***" | "___") {
            flush(&mut para, &mut out);
            out.push((pal.paint(pal.dim, &pal.rule(max.min(40))), max.min(40)));
        } else if let Some(quote) = trimmed.strip_prefix('>') {
            flush(&mut para, &mut out);
            let bar = pal.paint(pal.dim, pal.g.v);
            for words in wrap(&spans(quote.trim()), max.saturating_sub(2)) {
                let (text, cols) = pal.styled(&words);
                out.push((format!("{bar} {}", pal.paint(pal.dim, &text)), cols + 2));
            }
        } else if let Some((indent, num, body)) = list_item(line) {
            flush(&mut para, &mut out);
            let marker = if num.is_empty() {
                pal.g.bullet.to_owned()
            } else {
                num
            };
            let lead = " ".repeat(indent.min(8));
            let mw = width(&lead) + width(&marker) + 1;
            for (n, words) in wrap(&spans(body), max.saturating_sub(mw))
                .iter()
                .enumerate()
            {
                let (text, cols) = pal.styled(words);
                let prefix = if n == 0 {
                    format!("{lead}{} ", pal.paint(pal.accent, &marker))
                } else {
                    " ".repeat(mw)
                };
                out.push((format!("{prefix}{text}"), cols + mw));
            }
        } else {
            para.push(trimmed);
        }
        i += 1;
    }
    flush(&mut para, &mut out);
    while out.last().is_some_and(|(_, w)| *w == 0) {
        out.pop();
    }
    out
}

/// A pipe table drawn with box characters, with its columns shrunk and the
/// cells wrapped to fit `max` columns.
fn table(head: &[String], rows: &[Vec<String>], max: usize, pal: &Pal) -> Vec<Out> {
    let n = head
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    let cell = |r: &[String], c: usize| r.get(c).map_or_else(Vec::new, |s| spans(s));
    let natural = |toks: &[Tok]| pal.styled(toks).1;
    let mut widths: Vec<usize> = (0..n)
        .map(|c| {
            std::iter::once(head)
                .chain(rows.iter().map(Vec::as_slice))
                .map(|r| natural(&cell(r, c)))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    let room = max.saturating_sub(3 * n + 1).max(n * 3);
    while widths.iter().sum::<usize>() > room {
        let Some((idx, _)) = widths.iter().enumerate().max_by_key(|(_, w)| **w) else {
            break;
        };
        if widths[idx] <= 3 {
            break;
        }
        widths[idx] -= 1;
    }
    let g = &pal.g;
    let total = widths.iter().sum::<usize>() + 3 * n + 1;
    let border = |l: &str, m: &str, r: &str| {
        let mid: Vec<String> = widths.iter().map(|w| pal.rule(w + 2)).collect();
        (pal.paint(pal.dim, &format!("{l}{}{r}", mid.join(m))), total)
    };
    let row_lines = |r: &[String], header: bool| -> Vec<Out> {
        let wrapped: Vec<Vec<Vec<Tok>>> = (0..n).map(|c| wrap(&cell(r, c), widths[c])).collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        let bar = pal.paint(pal.dim, g.v);
        (0..height)
            .map(|k| {
                let mut s = bar.clone();
                for (c, col) in wrapped.iter().enumerate() {
                    let (text, w) = col.get(k).map_or((String::new(), 0), |l| pal.styled(l));
                    let text = if header { pal.bold(&text) } else { text };
                    let pad = " ".repeat(widths[c].saturating_sub(w));
                    let _ = write!(s, " {text}{pad} {bar}");
                }
                (s, total)
            })
            .collect()
    };
    let mut out = vec![border(g.tl, g.tt, g.tr)];
    out.extend(row_lines(head, true));
    out.push(border(g.lt, g.cross, g.rt));
    for r in rows {
        out.extend(row_lines(r, false));
    }
    out.push(border(g.bl, g.bt, g.br));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(text: &str, w: usize) -> Vec<String> {
        markdown(text, w, &Pal::plain(false, 80))
            .into_iter()
            .map(|(s, _)| s)
            .collect()
    }

    #[test]
    fn widths_match_text() {
        let pal = Pal::plain(false, 80);
        for (s, w) in markdown(
            "# Title\n\n- **one** `two`\n1. three\n\n| a | b |\n|---|---|\n| x | yy |",
            40,
            &pal,
        ) {
            assert_eq!(width(&s), w, "{s:?}");
        }
    }

    #[test]
    fn inline_markers() {
        assert_eq!(
            render("use `ls -la` **now** and *maybe*", 80),
            ["use ls -la now and maybe"]
        );
        assert_eq!(render("2 * 3 and a ** b", 80), ["2 * 3 and a ** b"]);
        assert_eq!(render("unclosed `tick", 80), ["unclosed `tick"]);
    }

    #[test]
    fn lists_and_wrapping() {
        let out = render("- alpha beta gamma delta\n2. second", 16);
        assert_eq!(out, ["• alpha beta", "  gamma delta", "2. second"]);
    }

    #[test]
    fn tables_get_boxes_and_fit() {
        let out = render(
            "| Router | OpenRouter |\n|:--|--:|\n| TP-Link Archer | no |",
            80,
        );
        assert_eq!(
            out,
            [
                "╭────────────────┬────────────╮",
                "│ Router         │ OpenRouter │",
                "├────────────────┼────────────┤",
                "│ TP-Link Archer │ no         │",
                "╰────────────────┴────────────╯",
            ]
        );
        let narrow = render(
            "| a | b |\n|---|---|\n| one two three four five | six |",
            20,
        );
        assert!(narrow.iter().all(|l| width(l) <= 20), "{narrow:?}");
        assert!(narrow.len() > 5, "long cells wrap onto more lines");
    }

    #[test]
    fn code_blocks_keep_lines() {
        let out = render("run:\n```sh\nls  -la\n```\ndone", 40);
        assert_eq!(out, ["run:", "│ ls  -la", "done"]);
    }

    #[test]
    fn bubble_and_cards() {
        let pal = Pal::plain(true, 40);
        let b = pal.bubble("hello **there**", "xsh");
        assert!(
            b.starts_with("+--------------+\n| hello there  |\n+--------------+\n"),
            "{b}"
        );
        assert!(b.contains("|o_o|  xsh\n"));
        assert_eq!(pal.card_status(0), "+- ok exit 0\n");
        assert_eq!(pal.card_status(2), "+- x exit 2\n");
        let p = pal.card_preview("a\n\x1b[31mred\x1b[0m\nc\nd", 2);
        assert_eq!(p, "| … 2 more line(s)\n| c\n| d\n");
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m\tx"), "bold    x");
        assert_eq!(
            strip_ansi("\x1b]0;evil title\x07ok\x1b]8;;https://x\x1b\\link"),
            "oklink"
        );
    }

    #[test]
    fn question_then_task_then_response() {
        let pal = Pal::plain(false, 60);
        let q = pal.question("check my router");
        assert!(q.starts_with("\n╭─ Question ─"), "{q}");
        assert!(q.contains("\n│ check my router\n╰─"), "{q}");
        let head = pal.card_head(1, "route -n get default", "find the gateway");
        assert!(head.starts_with("\n╭─ Task 1 ─"), "{head}");
        assert!(
            head.contains("│ $ route -n get default\n│ find the gateway\n"),
            "{head}"
        );
        assert_eq!(pal.card_response(), "├─ Response\n");
        assert_eq!(pal.card_no_output(), "│ (no output)\n");
    }

    #[test]
    fn agent_report_has_question_answer_and_steps() {
        let pal = Pal::plain(false, 100);
        let steps = [
            StepLog {
                command: "route -n get default | grep gateway".into(),
                result: "ok (exit 0)".into(),
                output: StepLog::last_line("route to: default\ngateway: 192.168.0.1\n\n", ""),
            },
            StepLog {
                command: "ls *.txt".into(),
                result: "skipped".into(),
                output: String::new(),
            },
        ];
        let r = pal.agent_report("which router?", "**TP-Link**", &steps);
        let lines: Vec<&str> = r.lines().collect();
        assert_eq!(lines[1].trim_matches(['│', ' ']), "Question");
        assert!(r.contains("│ which router?"), "{r}");
        assert!(r.contains("│ TP-Link"), "{r}");
        assert!(r.contains("Execution"), "{r}");
        assert!(
            r.contains(
                "│ 1 │ route -n get default | grep gateway │ ok (exit 0) │ gateway: 192.168.0.1 │"
            ),
            "the pipe in the command stays in one cell: {r}"
        );
        assert!(r.contains("│ 2 │ ls *.txt"), "globs are not italics: {r}");
        assert!(r.contains("xsh agent"));
        assert_eq!(cells(r"| a \| b | c |"), ["a | b", "c"]);
    }
}
