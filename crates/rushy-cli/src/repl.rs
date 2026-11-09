//! The interactive prompt: line editing, multi-line input, completion,
//! `Ctrl-R` fuzzy history, `Ctrl-K` palette, and the AI key bindings.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::io::{self, IsTerminal as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{Context as _, Result};
use nu_ansi_term::Style;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::MoveUp;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use ratatui::{Terminal, TerminalOptions, Viewport};
use reedline::{
    ColumnarMenu, Completer, CompletionResult, EditCommand, Emacs, FileBackedHistory, Highlighter,
    Hinter, History, HistoryItem, KeyCode, KeyModifiers, MenuBuilder, Prompt, PromptEditMode,
    PromptHistorySearch, PromptViMode, Reedline, ReedlineEvent, ReedlineMenu, SearchQuery, Signal,
    Span, StyledText, Suggestion, ValidationResult, Validator, Vi, default_emacs_keybindings,
    default_vi_insert_keybindings, default_vi_normal_keybindings, default_vi_visual_keybindings,
};
use rushy_core::parser;
use rushy_core::{Flow, JobState};
use rushy_tui::palette::{Action, Item, Kind};
use rushy_tui::{
    ConfigWatcher, Palette, PromptInput, RenderOpts, Values, plain, render_prompt, segment_values,
    to_ansi,
};

use crate::app::{App, CompData, display_path};
use crate::cmds::{self, Choice};
use crate::exec::execute as run_line;
use crate::highlight;
use crate::man::ManCache;
use crate::term;

const SEARCH: &str = "xsh:search";
const PALETTE: &str = "xsh:palette";
const AI_SUGGEST: &str = "xsh:ai-suggest";
const AI_AGENT: &str = "xsh:ai-agent";
const AI_AUTOPILOT: &str = "xsh:ai-autopilot";
const MENU: &str = "completion_menu";
const MAX_SUGGESTIONS: usize = 200;

/// Complete when the parser needs no more input: an unclosed quote or a
/// trailing `\` makes the editor start a new line instead of submitting.
struct ShellValidator;

impl Validator for ShellValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        match parser::parse(line, &BTreeMap::new()) {
            Err(rushy_core::Error::Incomplete) => ValidationResult::Incomplete,
            _ => ValidationResult::Complete,
        }
    }
}

/// Characters that end a word outside quotes.
const BREAK: &[char] = &['|', '&', ';', '<', '>', '(', ')'];

/// Whether `word` is one of `|`, `&&`, `;`, or `>>`.
fn is_operator(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|c| "|&;<>".contains(c))
}

/// Whether `word` sets a variable, as in `FOO=bar cmd`.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The word that ends at `pos`: the text to match against, the span that a
/// suggestion replaces, and whether the word sits inside an unclosed quote.
/// For a quote only its contents are completed, so the quote stays in place.
fn word_at(line: &str, pos: usize) -> (String, Span, bool) {
    let head = line.get(..pos).unwrap_or(line);
    let mut word: Option<usize> = None;
    let mut quote: Option<char> = None;
    for (i, c) in head.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                word.get_or_insert(i);
            }
            None if c.is_whitespace() || BREAK.contains(&c) => word = None,
            None => {
                word.get_or_insert(i);
            }
        }
    }
    let start = word.unwrap_or(head.len());
    let quoted = quote.is_some() && head[start..].starts_with(['\'', '"']);
    let from = if quoted { start + 1 } else { start };
    (
        head[from..].to_owned(),
        Span {
            start: from,
            end: head.len(),
        },
        quote.is_some(),
    )
}

/// The words of `text`; an operator such as `|` is an item of its own and
/// quotes are dropped.
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() || BREAK.contains(&c) => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                if !c.is_whitespace() {
                    out.push(c.to_string());
                }
            }
            Some(_) | None => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Escapes the characters the parser would otherwise read as syntax, so a
/// file called `a b` is inserted as `a\ b`.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if " \t'\"\\$&|;<>()*?[]{}!#~`".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn suggestion(value: String, span: Span) -> Suggestion {
    Suggestion {
        value,
        span,
        ..Suggestion::default()
    }
}

/// Commands: built-ins, aliases, bundle commands, and `PATH` executables.
fn command_matches(data: &CompData, prefix: &str, span: Span) -> Vec<Suggestion> {
    let mut seen = HashSet::new();
    data.commands
        .iter()
        .chain(&data.path_commands)
        .filter(|c| c.starts_with(prefix) && seen.insert((*c).clone()))
        .take(MAX_SUGGESTIONS)
        .map(|c| suggestion(c.clone(), span))
        .collect()
}

/// First-argument candidates of `cmd` from the active bundles.
fn sub_matches(data: &CompData, cmd: &str, prefix: &str, span: Span) -> Vec<Suggestion> {
    data.subs.get(cmd).map_or_else(Vec::new, |subs| {
        subs.iter()
            .filter(|s| s.starts_with(prefix))
            .take(MAX_SUGGESTIONS)
            .map(|s| suggestion(s.clone(), span))
            .collect()
    })
}

/// File names under `cwd`; `home` expands a leading `~`, as the parser does.
/// A directory is given a trailing `/`, so that `Tab` again lists what is
/// inside it.
fn complete_files(
    data: &CompData,
    home: Option<&str>,
    prefix: &str,
    span: Span,
    quoted: bool,
) -> Vec<Suggestion> {
    let cut = prefix.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let (dir, base) = prefix.split_at(cut);
    let root = match dir.strip_prefix('~') {
        Some(rest) => match home {
            Some(h) => std::path::Path::new(h).join(rest.trim_start_matches(['/', '\\'])),
            None => data.cwd.join(dir),
        },
        None if dir.is_empty() => data.cwd.clone(),
        None => data.cwd.join(dir),
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut names: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // Hidden files show only when the typed name starts with a dot.
            if !name.starts_with(base) || (name.starts_with('.') && !base.starts_with('.')) {
                return None;
            }
            Some((name, e.path().is_dir()))
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(name, is_dir)| {
            let mut value = format!("{dir}{name}");
            if is_dir {
                value.push('/');
            }
            // Inside a quote an escape would be taken literally.
            suggestion(if quoted { value } else { escape(&value) }, span)
        })
        .collect()
}

/// File names that match `prefix` when case is ignored, used when the exact
/// match finds nothing (`doc<Tab>` finds `Documents/`).
fn complete_files_nocase(
    data: &CompData,
    home: Option<&str>,
    prefix: &str,
    span: Span,
    quoted: bool,
) -> Vec<Suggestion> {
    let cut = prefix.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let (dir, base) = prefix.split_at(cut);
    let lower = base.to_lowercase();
    let root = match (dir.strip_prefix('~'), home) {
        (Some(rest), Some(h)) => std::path::Path::new(h).join(rest.trim_start_matches(['/', '\\'])),
        _ if dir.is_empty() => data.cwd.clone(),
        _ => data.cwd.join(dir),
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut names: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (name.to_lowercase().starts_with(&lower)
                && (!name.starts_with('.') || base.starts_with('.')))
            .then(|| (name, e.path().is_dir()))
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(name, is_dir)| {
            let value = format!("{dir}{name}{}", if is_dir { "/" } else { "" });
            suggestion(if quoted { value } else { escape(&value) }, span)
        })
        .collect()
}

/// Arguments that depend on the command: SSH hosts, `make` targets,
/// `npm run` scripts, git branches, and `xsh config` keys.
fn context_matches(data: &CompData, args: &[&String], prefix: &str) -> Option<Vec<String>> {
    let cmd = args.first()?.as_str();
    let sub = args.get(1).map(|s| s.as_str());
    let list = match (cmd, sub) {
        ("ssh" | "scp" | "sftp" | "mosh" | "ping" | "telnet" | "rsync", _) => data.hosts.clone(),
        ("make", _) => make_targets(&data.cwd),
        ("npm" | "pnpm" | "yarn" | "bun", Some("run")) if args.len() == 2 => npm_scripts(&data.cwd),
        (
            "git",
            Some(
                "checkout" | "switch" | "merge" | "rebase" | "branch" | "diff" | "log"
                | "cherry-pick",
            ),
        ) => git_branches(&data.cwd),
        ("config", Some("get" | "set" | "unset")) if args.len() == 2 => {
            rushy_config::keys().unwrap_or_default()
        }
        ("unset" | "export", _) => data.vars.clone(),
        _ => return None,
    };
    let out: Vec<String> = list.into_iter().filter(|s| s.starts_with(prefix)).collect();
    (!out.is_empty()).then_some(out)
}

/// Rule names in the `Makefile` of `cwd`.
fn make_targets(cwd: &std::path::Path) -> Vec<String> {
    let Some(text) = ["GNUmakefile", "makefile", "Makefile"]
        .iter()
        .find_map(|f| std::fs::read_to_string(cwd.join(f)).ok())
    else {
        return Vec::new();
    };
    let mut out: Vec<String> = text
        .lines()
        .filter(|l| !l.starts_with(['\t', ' ', '.', '#']))
        .filter_map(|l| {
            l.split_once(':')
                .filter(|(_, r)| !r.starts_with('='))
                .map(|(t, _)| t)
        })
        .flat_map(str::split_whitespace)
        .filter(|t| !t.contains(['$', '%', '=']))
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Keys of `scripts` in the `package.json` of `cwd`. A small scan, so no
/// JSON parser is needed: it reads the `"scripts": { ... }` object.
fn npm_scripts(cwd: &std::path::Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(cwd.join("package.json")) else {
        return Vec::new();
    };
    let Some(start) = text.find("\"scripts\"") else {
        return Vec::new();
    };
    let body = &text[start..];
    let (Some(open), Some(close)) = (body.find('{'), body.find('}')) else {
        return Vec::new();
    };
    if close < open {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut depth_key = true;
    for (i, part) in body[open + 1..close].split('"').enumerate() {
        // Odd parts are inside quotes: key, value, key, value ...
        if i % 2 == 1 {
            if depth_key {
                out.push(part.to_owned());
            }
            depth_key = !depth_key;
        }
    }
    out
}

/// Local branch names of the git repository that holds `cwd`.
fn git_branches(cwd: &std::path::Path) -> Vec<String> {
    let Some(git) = cwd.ancestors().map(|d| d.join(".git")).find(|g| g.is_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let heads = git.join("refs/heads");
    let mut stack = vec![heads.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(&heads) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(git.join("packed-refs")) {
        out.extend(
            text.lines()
                .filter_map(|l| l.split_once(" refs/heads/").map(|(_, b)| b.to_owned())),
        );
    }
    out.sort();
    out.dedup();
    out
}

/// Gray text after the cursor: the newest history entry that starts with the
/// line, or else the first completion of the word being typed.
struct XshHinter {
    completer: ShellCompleter,
    style: Style,
    hint: String,
}

impl XshHinter {
    fn history_hint(line: &str, history: &dyn History) -> Option<String> {
        history
            .search(SearchQuery::last_with_prefix(
                line.to_owned(),
                history.session(),
            ))
            .ok()?
            .first()
            .and_then(|e| e.command_line.get(line.len()..).map(str::to_owned))
            .filter(|h| !h.is_empty())
    }

    fn completion_hint(&mut self, line: &str) -> Option<String> {
        let (prefix, span, _) = word_at(line, line.len());
        if prefix.is_empty() {
            return None;
        }
        let typed = line.get(span.start..)?;
        self.completer
            .complete(line, line.len())
            .suggestions()
            .iter()
            .find_map(|s| s.value.strip_prefix(typed).map(str::to_owned))
            .filter(|h| !h.is_empty())
    }
}

impl Hinter for XshHinter {
    fn handle(
        &mut self,
        line: &str,
        pos: usize,
        history: &dyn History,
        use_ansi_coloring: bool,
        _cwd: &str,
    ) -> String {
        self.hint = if line.trim().is_empty() || pos != line.len() {
            String::new()
        } else {
            Self::history_hint(line, history)
                .or_else(|| self.completion_hint(line))
                .unwrap_or_default()
        };
        if use_ansi_coloring && !self.hint.is_empty() {
            self.style.paint(&self.hint).to_string()
        } else {
            self.hint.clone()
        }
    }

    fn complete_hint(&self) -> String {
        self.hint.clone()
    }

    fn next_hint_token(&self) -> String {
        // Leading spaces plus the next word, as Alt-F accepts it.
        let start = self.hint.len() - self.hint.trim_start().len();
        let end = self.hint[start..]
            .find(char::is_whitespace)
            .map_or(self.hint.len(), |i| start + i);
        self.hint[..end].to_owned()
    }
}

/// Completes commands, bundle subcommands, man-page options, and file names.
#[derive(Clone)]
struct ShellCompleter {
    data: Arc<RwLock<CompData>>,
    man: ManCache,
    /// The user's home directory, for `~` at the start of a path.
    home: Option<String>,
}

impl ShellCompleter {
    /// Options of `cmd` (or of its subcommand page) that start with `prefix`.
    fn man_flags(&self, cmd: &str, sub: Option<&str>, prefix: &str, span: Span) -> Vec<Suggestion> {
        let mut seen = HashSet::new();
        self.man
            .load(cmd, sub)
            .iter()
            .filter(|o| o.flag.starts_with(prefix) && seen.insert(o.flag.clone()))
            .take(MAX_SUGGESTIONS)
            .map(|o| Suggestion {
                description: (!o.help.is_empty()).then(|| o.help.clone()),
                ..suggestion(o.flag.clone(), span)
            })
            .collect()
    }
}

impl Completer for ShellCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let (prefix, span, quoted) = word_at(line, pos);
        let Ok(data) = self.data.read() else {
            return CompletionResult::fresh(Vec::new());
        };
        // Only the words of the current pipeline stage count as arguments.
        let typed = words(line.get(..span.start).unwrap_or_default());
        let stage = typed
            .iter()
            .rposition(|w| is_operator(w))
            .map_or(0, |i| i + 1);
        let args: Vec<&String> = typed[stage..]
            .iter()
            .filter(|w| !is_assignment(w))
            .collect();
        let cmd = args.first().map(|s| s.as_str()).unwrap_or_default();
        let mut found = if let Some(name) = prefix
            .strip_prefix("${")
            .or_else(|| prefix.strip_prefix('$'))
        {
            let open = prefix.starts_with("${");
            data.vars
                .iter()
                .filter(|v| v.starts_with(name))
                .take(MAX_SUGGESTIONS)
                .map(|v| {
                    suggestion(
                        if open {
                            format!("${{{v}}}")
                        } else {
                            format!("${v}")
                        },
                        span,
                    )
                })
                .collect()
        } else if !args.is_empty()
            && !prefix.starts_with('-')
            && let Some(list) = context_matches(&data, &args, &prefix)
        {
            list.into_iter()
                .take(MAX_SUGGESTIONS)
                .map(|v| suggestion(v, span))
                .collect()
        } else if prefix.starts_with('-') {
            let sub = args
                .get(1)
                .copied()
                .map(String::as_str)
                .filter(|s| !s.starts_with('-'));
            args.first()
                .map_or_else(Vec::new, |cmd| self.man_flags(cmd, sub, &prefix, span))
        } else if args.is_empty() {
            // A full command name also lists its subcommands, as `docker build`.
            let mut res = command_matches(&data, &prefix, span);
            if let Some(subs) = data.subs.get(prefix.as_str()) {
                res.extend(
                    subs.iter()
                        .take(MAX_SUGGESTIONS)
                        .map(|s| suggestion(format!("{prefix} {s}"), span)),
                );
            }
            res
        } else if args.len() == 1 {
            sub_matches(&data, args[0], &prefix, span)
        } else {
            Vec::new()
        };
        // A command with no man page or bundle data, or a path prefix such as
        // `src/`, still completes file names; `cd` lists only directories.
        if found.is_empty() {
            found = complete_files(&data, self.home.as_deref(), &prefix, span, quoted);
            if found.is_empty() {
                found = complete_files_nocase(&data, self.home.as_deref(), &prefix, span, quoted);
            }
            if matches!(cmd, "cd" | "pushd" | "rmdir") {
                found.retain(|s| s.value.ends_with('/'));
            }
        }
        CompletionResult::fresh(found)
    }
}

/// Length of the token at the start of `text`: an operator run, or a word
/// that ends at whitespace or an operator outside quotes.
fn token_len(text: &str) -> usize {
    if text.starts_with(['(', ')']) {
        return 1;
    }
    if text.starts_with(BREAK) {
        return text
            .find(|c: char| !"|&;<>".contains(c))
            .unwrap_or(text.len());
    }
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() || BREAK.contains(&c) => return i,
            Some(_) | None => {}
        }
    }
    text.len()
}

/// Colors the line: commands (known, built-in, or unknown), keywords, flags,
/// strings, variables, operators, comments, and existing paths.
struct ShellHighlighter {
    data: Arc<RwLock<CompData>>,
    home: Option<String>,
}

impl ShellHighlighter {
    fn known(data: &CompData, cmd: &str) -> bool {
        if cmd.contains('/') {
            let p = data.cwd.join(cmd);
            return p.is_file() || p.is_dir();
        }
        data.commands
            .iter()
            .chain(&data.path_commands)
            .any(|c| c == cmd)
    }

    fn is_path(&self, data: &CompData, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        let p = match (word.strip_prefix('~'), &self.home) {
            (Some(rest), Some(h)) => std::path::Path::new(h).join(rest.trim_start_matches('/')),
            _ => data.cwd.join(word),
        };
        p.exists()
    }

    fn arg_style(&self, data: &CompData, tok: &str) -> Style {
        let c = &data.colors;
        if tok.starts_with('-') {
            c.flag
        } else if tok.starts_with(['\'', '"']) {
            c.string
        } else if tok.starts_with('$') {
            c.var
        } else if self.is_path(data, tok) {
            c.path
        } else {
            Style::new()
        }
    }
}

impl Highlighter for ShellHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut out = StyledText::new();
        let Ok(data) = self.data.read() else {
            out.push((Style::new(), line.to_owned()));
            return out;
        };
        let c = &data.colors;
        let mut cmd_next = true;
        let mut name_next = false;
        let mut at_start = true;
        let mut rest = line;
        while !rest.is_empty() {
            if at_start && rest.starts_with('#') {
                let end = rest.find('\n').unwrap_or(rest.len());
                out.push((c.comment, rest[..end].to_owned()));
                rest = &rest[end..];
                continue;
            }
            let ws = rest.len() - rest.trim_start().len();
            let len = if ws > 0 { ws } else { token_len(rest).max(1) };
            let len = (1..=len)
                .rev()
                .find(|&n| rest.is_char_boundary(n))
                .unwrap_or(rest.len());
            let tok = &rest[..len];
            let mut style = Style::new();
            if ws == 0 {
                at_start = false;
                if is_operator(tok) || tok == "(" || tok == ")" {
                    style = c.operator;
                    cmd_next = tok != ")";
                } else if name_next {
                    // The loop variable of `for NAME` or the word of `case WORD`.
                    name_next = false;
                    cmd_next = false;
                    style = c.var;
                } else if !cmd_next && matches!(tok, "in" | "do") {
                    style = c.keyword;
                    cmd_next = tok == "do";
                } else if cmd_next {
                    if highlight::is_keyword(tok) {
                        style = c.keyword;
                        name_next = matches!(tok, "for" | "case" | "select");
                    } else if is_assignment(tok) {
                        style = c.var;
                    } else {
                        cmd_next = false;
                        style = if data.builtins.contains(tok) {
                            c.builtin
                        } else if Self::known(&data, tok) {
                            c.command
                        } else if data.cwd.join(tok).is_dir() {
                            c.path
                        } else {
                            c.unknown
                        };
                    }
                } else {
                    style = self.arg_style(&data, tok);
                }
            } else {
                at_start = true;
            }
            out.push((style, tok.to_owned()));
            rest = &rest[len..];
        }
        out
    }
}

struct XshPrompt {
    left: String,
    right: String,
}

impl Prompt for XshPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.right)
    }
    fn render_prompt_indicator(&self, mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(match mode {
            PromptEditMode::Vi(PromptViMode::Normal) => "[N] ",
            _ => "",
        })
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("> ")
    }
    fn render_prompt_history_search_indicator(&self, h: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Owned(format!("(search: {}) ", h.term))
    }
    // The prompt string carries its own colors.
    fn get_prompt_color(&self) -> reedline::Color {
        reedline::Color::Default
    }
    fn get_prompt_multiline_color(&self) -> reedline::Color {
        reedline::Color::Default
    }
    fn get_indicator_color(&self) -> reedline::Color {
        reedline::Color::Default
    }
    fn get_prompt_right_color(&self) -> reedline::Color {
        reedline::Color::Default
    }
}

/// Key bindings shared by the Emacs and Vi insert modes.
fn custom_bindings() -> Vec<(KeyModifiers, KeyCode, ReedlineEvent)> {
    let host = |s: &str| ReedlineEvent::ExecuteHostCommand(s.into());
    let mut out = vec![
        (
            KeyModifiers::NONE,
            KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu(MENU.into()),
                ReedlineEvent::MenuNext,
            ]),
        ),
        (KeyModifiers::CONTROL, KeyCode::Char('r'), host(SEARCH)),
        (KeyModifiers::CONTROL, KeyCode::Char('k'), host(PALETTE)),
        // Ctrl-Shift-Space needs the kitty keyboard protocol; Alt-G works everywhere.
        (
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyCode::Char(' '),
            host(AI_AGENT),
        ),
        (KeyModifiers::ALT, KeyCode::Char('g'), host(AI_AGENT)),
        (KeyModifiers::ALT, KeyCode::Char('a'), host(AI_AUTOPILOT)),
        // Right and End accept the gray suggestion, also in Vi insert mode.
        (
            KeyModifiers::NONE,
            KeyCode::Right,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::HistoryHintComplete,
                ReedlineEvent::Edit(vec![EditCommand::MoveRight { select: false }]),
            ]),
        ),
        (
            KeyModifiers::NONE,
            KeyCode::End,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::HistoryHintComplete,
                ReedlineEvent::Edit(vec![EditCommand::MoveToLineEnd { select: false }]),
            ]),
        ),
        // Accept one word of the gray suggestion.
        (
            KeyModifiers::ALT,
            KeyCode::Right,
            ReedlineEvent::HistoryHintWordComplete,
        ),
        (
            KeyModifiers::ALT,
            KeyCode::Char('f'),
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::HistoryHintWordComplete,
                ReedlineEvent::Edit(vec![EditCommand::MoveWordRight { select: false }]),
            ]),
        ),
    ];
    // A terminal sends Ctrl-Space as NUL, which crossterm reports as either of these.
    for code in [KeyCode::Char(' '), KeyCode::Char('@')] {
        out.push((KeyModifiers::CONTROL, code, host(AI_SUGGEST)));
    }
    out
}

fn editor(app: &App) -> Result<Reedline> {
    let mut kb = default_emacs_keybindings();
    for (m, k, e) in custom_bindings() {
        kb.add_binding(m, k, e);
    }
    let cap = app.cfg.history.editor_entries.max(100);
    let mut history = FileBackedHistory::new(cap).context("cannot create the line history")?;
    for cmd in app.recent_commands(cap) {
        let _ = history.save(HistoryItem::from_command_line(cmd));
    }
    // The default marker draws `| ` before the line while the menu is open.
    let menu = ColumnarMenu::default().with_name(MENU).with_marker("");
    let home = app.shell.state().home();
    let completer = ShellCompleter {
        data: Arc::clone(&app.comp),
        man: app.man.clone(),
        home: home.clone(),
    };
    let mut ed = Reedline::create()
        .use_kitty_keyboard_enhancement(true)
        .with_history(Box::new(history))
        .with_validator(Box::new(ShellValidator))
        .with_highlighter(Box::new(ShellHighlighter {
            data: Arc::clone(&app.comp),
            home: home.clone(),
        }))
        .with_completer(Box::new(completer.clone()))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(menu)))
        .with_quick_completions(true)
        .with_partial_completions(true);
    if app.cfg.shell.autosuggest {
        let style = if app.cfg.ui.no_color {
            Style::new().dimmed()
        } else {
            Style::new().fg(nu_ansi_term::Color::DarkGray)
        };
        ed = ed.with_hinter(Box::new(XshHinter {
            completer,
            style,
            hint: String::new(),
        }));
    }
    Ok(if app.cfg.shell.edit_mode.eq_ignore_ascii_case("vi") {
        let mut ins = default_vi_insert_keybindings();
        for (m, k, e) in custom_bindings() {
            ins.add_binding(m, k, e);
        }
        ed.with_edit_mode(Box::new(Vi::new(
            ins,
            default_vi_normal_keybindings(),
            default_vi_visual_keybindings(),
        )))
    } else {
        ed.with_edit_mode(Box::new(Emacs::new(kb)))
    })
}

async fn build_prompt(app: &App) -> String {
    let cwd = app.shell.state().cwd().to_path_buf();
    let mut values = Values::new();
    for name in app.effects.segments.keys() {
        let (n, c) = (name.clone(), cwd.clone());
        if let Ok(Some(v)) = tokio::task::spawn_blocking(move || segment_values(&n, &c)).await {
            values.insert(name.clone(), v);
        }
    }
    let s = app.shell.state();
    let user = s.var("USER").unwrap_or("user");
    let path = display_path(&cwd, s.home().as_deref());
    let input = PromptInput {
        user,
        host: &app.host,
        path: &path,
        status: s.last_status(),
    };
    let cols = size().map_or(80, |(c, _)| c);
    let opts = RenderOpts {
        width: (cols / 2).max(24),
        ascii: app.cfg.ui.screen_reader,
    };
    let pieces = render_prompt(&app.theme, &input, &app.effects.segments, &values, opts);
    let mut text = if app.theme.plain || app.cfg.ui.no_color {
        plain(&pieces)
    } else {
        to_ansi(&pieces)
    };
    if app.cfg.ui.screen_reader {
        text = text.replace('❯', ">");
    }
    text
}

/// Exit status and run time of the last command, for the right side.
fn build_right(app: &App, took: Option<std::time::Duration>) -> String {
    if !app.cfg.ui.right_prompt {
        return String::new();
    }
    let mut parts = Vec::new();
    let code = app.shell.last_status();
    if code != 0 {
        parts.push(format!("✘ {code}"));
    }
    if let Some(d) = took.filter(|d| d.as_secs() >= 1) {
        parts.push(fmt_duration(d));
    }
    let jobs = app.shell.job_count();
    if jobs > 0 {
        parts.push(format!("⚙ {jobs}"));
    }
    let text = parts.join(" ");
    let text = if app.cfg.ui.screen_reader {
        text.replace('✘', "x").replace('⚙', "jobs")
    } else {
        text
    };
    if app.cfg.ui.no_color || app.theme.plain || text.is_empty() {
        return text;
    }
    let color = if code == 0 {
        nu_ansi_term::Color::DarkGray
    } else {
        nu_ansi_term::Color::Red
    };
    color.paint(text).to_string()
}

/// Programs that wait for the user, so a long run time is no news.
fn is_interactive_cmd(line: &str) -> bool {
    const LIST: &[&str] = &[
        "vi", "vim", "nvim", "nano", "emacs", "hx", "micro", "less", "more", "man", "ssh", "mosh",
        "top", "htop", "btop", "tmux", "screen", "xsh", "bash", "zsh", "fish", "sh", "python",
        "python3", "node", "irb", "psql", "mysql", "sqlite3", "git", "fzf", "watch",
    ];
    words(line)
        .into_iter()
        .find(|w| !is_assignment(w) && w != "sudo")
        .is_some_and(|w| LIST.contains(&w.rsplit('/').next().unwrap_or(&w)))
}

/// `1m 05s`, `3.2s`, or `1h 02m`.
fn fmt_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{:.1}s", d.as_secs_f64()),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// Shows `palette` and returns the chosen text. With `inline`, it appears
/// under the prompt; otherwise (or when the window is short) it takes the
/// alternate screen.
fn pick(app: &App, palette: Palette, inline: bool) -> io::Result<Option<String>> {
    fn run(palette: Palette, theme: &rushy_tui::Theme, inline: bool) -> io::Result<Option<String>> {
        let mut term = if inline {
            println!();
            Terminal::with_options(
                CrosstermBackend::new(io::stdout()),
                TerminalOptions {
                    viewport: Viewport::Inline(12),
                },
            )?
        } else {
            execute!(io::stdout(), EnterAlternateScreen)?;
            Terminal::new(CrosstermBackend::new(io::stdout()))?
        };
        let mut pal = palette;
        loop {
            term.draw(|f| pal.render(f, f.area(), theme))?;
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match pal.on_key(k) {
                    Action::None => {}
                    Action::Close => break,
                    Action::Choose(item) => {
                        if inline {
                            term.clear()?;
                        }
                        return Ok(Some(item.insert));
                    }
                }
            }
        }
        if inline {
            term.clear()?;
        }
        Ok(None)
    }
    let rows = size().map_or(24, |(_, r)| r);
    let inline = inline && rows >= 14;
    enable_raw_mode()?;
    let result = run(palette, &app.theme, inline);
    let restored = if inline {
        execute!(io::stdout(), MoveUp(1))
    } else {
        execute!(io::stdout(), LeaveAlternateScreen)
    };
    disable_raw_mode()?;
    restored?;
    result
}

fn history_items(app: &App) -> Vec<Item> {
    let mut seen = HashSet::new();
    app.recent_commands(app.cfg.history.editor_entries)
        .into_iter()
        .rev()
        .filter(|c| seen.insert(c.clone()))
        .map(|c| Item {
            kind: Kind::History,
            label: c.clone(),
            detail: String::new(),
            insert: c,
        })
        .collect()
}

fn set_buffer(ed: &mut Reedline, text: &str) {
    ed.run_edit_commands(&[
        EditCommand::Clear,
        EditCommand::InsertString(text.to_owned()),
    ]);
}

async fn host_command(app: &mut App, ed: &mut Reedline, cmd: &str) -> Option<i32> {
    match cmd {
        SEARCH => match pick(
            app,
            Palette::new(history_items(app)).with_title("History search (Ctrl-R)"),
            true,
        ) {
            Ok(Some(text)) => set_buffer(ed, &text),
            Ok(None) => {}
            Err(e) => eprintln!("xsh: search: {e}"),
        },
        PALETTE => match pick(app, Palette::new(app.palette_items()), false) {
            Ok(Some(text)) => set_buffer(ed, &text),
            Ok(None) => {}
            Err(e) => eprintln!("xsh: palette: {e}"),
        },
        AI_SUGGEST | AI_AGENT | AI_AUTOPILOT => {
            let text = ed.current_buffer_contents().trim().to_owned();
            println!();
            if text.is_empty() {
                eprintln!("xsh: type a description first, then press the AI key again");
                return None;
            }
            if cmd == AI_SUGGEST {
                match cmds::suggest(app, &text, true).await {
                    Ok(Choice::Insert(c)) => set_buffer(ed, &c),
                    Ok(Choice::Run(c)) => {
                        set_buffer(ed, "");
                        return run_text(app, &c).await;
                    }
                    Ok(Choice::Cancel) => {}
                    Err(e) => eprintln!("xsh: {e:#}"),
                }
            } else {
                set_buffer(ed, "");
                if let Err(e) = cmds::agent_goal(app, &text, cmd == AI_AUTOPILOT).await {
                    eprintln!("xsh: {e:#}");
                }
            }
        }
        _ => {}
    }
    None
}

/// `cd DIR` when `auto_cd` is on and the line is one word naming a
/// directory that is not also a command.
fn auto_cd_target(app: &App, line: &str) -> Option<String> {
    if !app.cfg.shell.auto_cd {
        return None;
    }
    let w = line.trim();
    if w.is_empty()
        || w.contains(char::is_whitespace)
        || w.contains(['|', ';', '&', '<', '>', '$', '`'])
    {
        return None;
    }
    let data = app.comp.read().ok()?;
    if data
        .commands
        .iter()
        .chain(&data.path_commands)
        .any(|c| c == w)
    {
        return None;
    }
    let s = app.shell.state();
    let path = match (w.strip_prefix('~'), s.home()) {
        (Some(rest), Some(h)) => std::path::Path::new(&h).join(rest.trim_start_matches('/')),
        _ => s.cwd().join(w),
    };
    let arg = w
        .strip_prefix('~')
        .map_or_else(|| escape(w), |rest| format!("~{}", escape(rest)));
    path.is_dir().then(|| format!("cd {arg}"))
}

/// Prints close command names after `command not found`.
fn suggest_typo(app: &App, line: &str) {
    let Some(cmd) = words(line).into_iter().find(|w| !is_assignment(w)) else {
        return;
    };
    let Ok(data) = app.comp.read() else {
        return;
    };
    let hits = crate::hist::similar(&cmd, data.commands.iter().chain(&data.path_commands));
    if !hits.is_empty() {
        eprintln!("xsh: did you mean: {}?", hits.join(", "));
    }
}

/// Runs a submitted line; `Some(code)` means the shell should exit.
async fn run_text(app: &mut App, line: &str) -> Option<i32> {
    let expanded =
        match crate::hist::expand(line, &app.recent_commands(app.cfg.history.editor_entries)) {
            Ok(Some(new)) => {
                println!("{new}");
                Some(new)
            }
            Ok(None) => None,
            Err(e) => {
                eprintln!("xsh: {e}");
                app.shell.state_mut().set_last_status(1);
                return None;
            }
        };
    let line = expanded.as_deref().unwrap_or(line);
    let auto_cd = auto_cd_target(app, line);
    let line = auto_cd.as_deref().unwrap_or(line);
    // Read the man pages of what was typed while it runs, so that the next
    // `-<Tab>` can suggest its options.
    app.man.prefetch(line);
    let res = run_line(app, line).await;
    if app.cfg.shell.suggest_typos && app.shell.last_status() == 127 {
        suggest_typo(app, line);
    }
    match res {
        Ok(Flow::Exit(code)) => Some(code),
        Ok(Flow::Continue) => None,
        Err(e) => {
            eprintln!("xsh: {e}");
            None
        }
    }
}

pub(crate) fn forward_signals(fg: &rushy_core::ForegroundHandle) {
    use tokio::signal::unix::{SignalKind, signal};
    const TSTP: i32 = 18;
    let tty = io::stdin().is_terminal();
    // Ctrl-Z stops the foreground command; with a handler set, the shell
    // itself keeps running and keeps the command as a stopped job.
    if let Ok(mut stream) = signal(SignalKind::from_raw(TSTP)) {
        let fg = fg.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                fg.request_stop();
            }
        });
    }
    for (kind, name, code) in [
        (SignalKind::terminate(), "TERM", 143),
        (SignalKind::hangup(), "HUP", 129),
        (SignalKind::interrupt(), "INT", 130),
    ] {
        let Ok(mut stream) = signal(kind) else {
            continue;
        };
        let fg = fg.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                // On a terminal, Ctrl-C already reached the whole foreground
                // process group, so the child got its own SIGINT.
                if name == "INT" && tty {
                    continue;
                }
                if fg.signal(name) == 0 && name != "INT" {
                    let _ = disable_raw_mode();
                    std::process::exit(code);
                }
            }
        });
    }
}

fn watch_config(app: &App) -> Arc<AtomicBool> {
    let dirty = Arc::new(AtomicBool::new(false));
    if app.cfg.ui.hot_reload {
        match ConfigWatcher::new(app.paths.config_dir()) {
            Ok(mut w) => {
                let flag = Arc::clone(&dirty);
                tokio::spawn(async move {
                    while w.changed().await {
                        flag.store(true, Ordering::SeqCst);
                    }
                });
            }
            Err(e) => eprintln!("xsh: hot reload is off: {e}"),
        }
    }
    dirty
}

/// The interactive loop. Returns the shell's exit status.
pub async fn run(app: &mut App) -> Result<i32> {
    forward_signals(&app.shell.state().foreground());
    let reload = watch_config(app);
    let mut ed = editor(app)?;
    let mut took = None;
    let mut warned_jobs = false;
    loop {
        if reload.swap(false, Ordering::SeqCst) {
            // App-only settings (font, window) change often and do not touch
            // the shell, so they stay silent.
            let before = (app.cfg.clone(), app.theme.clone(), app.installed.len());
            app.reload();
            let (c, o) = (&app.cfg, &before.0);
            if (&c.shell, &c.ui, &c.mux, &c.bundles, &c.history)
                != (&o.shell, &o.ui, &o.mux, &o.bundles, &o.history)
                || app.theme != before.1
                || app.installed.len() != before.2
            {
                eprintln!("xsh: configuration reloaded");
            }
        }
        let hooks = app.refresh_effects();
        app.run_hooks(hooks).await;
        for job in app.shell.reap_jobs() {
            if let JobState::Done(code) = job.state {
                eprintln!("[{}] Done ({code})  {}", job.id, job.command);
            }
        }
        let term = app.cfg.shell.terminal_integration;
        if term {
            term::prompt_start(app.shell.state().cwd(), &app.host);
        }
        let prompt = XshPrompt {
            left: build_prompt(app).await,
            right: build_right(app, took),
        };
        let sig = tokio::task::block_in_place(|| ed.read_line(&prompt))
            .context("failed to read input")?;
        let exit = match sig {
            Signal::Success(line) if line.trim().is_empty() => None,
            Signal::Success(line) => {
                if term {
                    term::command_start(&line);
                }
                let start = std::time::Instant::now();
                let res = run_text(app, &line).await;
                let d = start.elapsed();
                took = Some(d);
                if term {
                    term::command_end(app.shell.last_status());
                }
                let limit = app.cfg.shell.notify_after_secs;
                if limit > 0 && d.as_secs() >= limit && !is_interactive_cmd(&line) {
                    term::notify(&line, app.shell.last_status(), &fmt_duration(d));
                }
                res
            }
            Signal::HostCommand(cmd) => host_command(app, &mut ed, &cmd).await,
            Signal::CtrlD => {
                println!();
                Some(app.shell.last_status())
            }
            _ => None,
        };
        if let Some(code) = exit {
            let jobs = app.shell.job_count();
            if app.cfg.shell.confirm_exit_with_jobs && jobs > 0 && !warned_jobs {
                eprintln!("xsh: {jobs} job(s) still run; exit again to quit");
                warned_jobs = true;
                continue;
            }
            let _ = io::stdout().flush();
            return Ok(code);
        }
        warned_jobs = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn comp(commands: &[&str], cwd: &std::path::Path) -> CompData {
        CompData {
            commands: commands.iter().map(|c| (*c).to_owned()).collect(),
            cwd: cwd.to_path_buf(),
            ..CompData::default()
        }
    }

    fn completer(data: CompData) -> ShellCompleter {
        ShellCompleter {
            data: Arc::new(RwLock::new(data)),
            man: ManCache::default(),
            home: None,
        }
    }

    fn values(result: &CompletionResult) -> Vec<String> {
        result
            .suggestions()
            .iter()
            .map(|s| s.value.clone())
            .collect()
    }

    #[test]
    fn highlights_each_kind_of_token() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join("src"))?;
        let mut data = comp(&["ls", "cd"], dir.path());
        data.builtins.insert("cd".into());
        let s = |n: u8| Style::new().fg(nu_ansi_term::Color::Fixed(n));
        data.colors = highlight::Colors {
            command: s(1),
            builtin: s(2),
            unknown: s(3),
            flag: s(4),
            string: s(5),
            var: s(6),
            operator: s(7),
            keyword: s(8),
            comment: s(9),
            path: s(10),
        };
        let h = ShellHighlighter {
            data: Arc::new(RwLock::new(data)),
            home: None,
        };
        let out = h.highlight(
            "ls -l src 'a' $X | nope && cd; for i in 1; do x=1 ls; done # c",
            0,
        );
        let got: Vec<(String, Style)> = out
            .buffer
            .into_iter()
            .filter(|(_, t)| !t.trim().is_empty())
            .map(|(st, t)| (t, st))
            .collect();
        let want = [
            ("ls", 1),
            ("-l", 4),
            ("src", 10),
            ("'a'", 5),
            ("$X", 6),
            ("|", 7),
            ("nope", 3),
            ("&&", 7),
            ("cd", 2),
            (";", 7),
            ("for", 8),
            ("i", 6),
            ("in", 8),
            ("1", 0),
            (";", 7),
            ("do", 8),
            ("x=1", 6),
            ("ls", 1),
            (";", 7),
            ("done", 8),
            ("# c", 9),
        ];
        assert_eq!(got.len(), want.len(), "{got:?}");
        for ((text, style), (wt, wn)) in got.iter().zip(want) {
            assert_eq!(text, wt);
            let expect = if wn == 0 { Style::new() } else { s(wn) };
            assert_eq!(*style, expect, "{text}");
        }
        Ok(())
    }

    #[test]
    fn hints_come_from_history_then_completion() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("notes.txt"), "")?;
        let mut hist = FileBackedHistory::new(10)?;
        hist.save(HistoryItem::from_command_line("git status --short"))?;
        let mut h = XshHinter {
            completer: completer(comp(&["cargo"], dir.path())),
            style: Style::new(),
            hint: String::new(),
        };
        assert_eq!(h.handle("git st", 6, &hist, false, ""), "atus --short");
        assert_eq!(h.next_hint_token(), "atus");
        assert_eq!(h.handle("car", 3, &hist, false, ""), "go");
        assert_eq!(h.handle("cat no", 6, &hist, false, ""), "tes.txt");
        assert_eq!(h.handle("cat no", 3, &hist, false, ""), "");
        assert_eq!(h.handle("zzz", 3, &hist, false, ""), "");
        Ok(())
    }

    #[test]
    fn completes_variables_and_make_targets() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("Makefile"),
            "run:\n\tx\nbuild deploy: run\nV := 1\n",
        )?;
        let mut data = comp(&["make"], dir.path());
        data.vars = vec!["HOME".into(), "HOST".into(), "PATH".into()];
        let mut c = completer(data);
        assert_eq!(values(&c.complete("echo $HO", 8)), ["$HOME", "$HOST"]);
        assert_eq!(values(&c.complete("echo ${PA", 10)), ["${PATH}"]);
        assert_eq!(values(&c.complete("make ", 5)), ["build", "deploy", "run"]);
        Ok(())
    }

    #[test]
    fn the_word_at_the_cursor_stops_at_spaces_operators_and_quotes() {
        let (prefix, span, quoted) = word_at("git co", 6);
        assert_eq!(
            (prefix.as_str(), span.start, span.end, quoted),
            ("co", 4, 6, false)
        );

        let (prefix, span, _) = word_at("git log --oneline|x", 19);
        assert_eq!((prefix.as_str(), span.start), ("x", 18));

        // Inside an unclosed quote only the contents are replaced.
        let (prefix, span, quoted) = word_at("cat 'a b", 8);
        assert_eq!(
            (prefix.as_str(), span.start, span.end, quoted),
            ("a b", 5, 8, true)
        );

        // A closed quote starts a new word again.
        let (prefix, _, quoted) = word_at("cat 'a b' x", 11);
        assert_eq!((prefix.as_str(), quoted), ("x", false));
    }

    #[test]
    fn words_keep_operators_and_drop_quotes() {
        assert_eq!(words("a 'b c' | d"), ["a", "b c", "|", "d"]);
        assert_eq!(words("echo hi && echo"), ["echo", "hi", "&", "&", "echo"]);
        assert!(words("").is_empty());
    }

    #[test]
    fn escapes_only_what_the_parser_would_read_as_syntax() {
        assert_eq!(escape("a b"), "a\\ b");
        assert_eq!(escape("plain-file.txt"), "plain-file.txt");
        assert_eq!(escape("$x&y"), "\\$x\\&y");
    }

    #[test]
    fn completes_commands_then_subcommands_then_files() -> TestResult {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("a b.txt"), "")?;
        std::fs::write(dir.path().join(".hidden"), "")?;
        std::fs::create_dir(dir.path().join("src"))?;
        let mut data = comp(&["git", "grep", "gzip"], dir.path());
        data.subs.insert(
            "git".to_owned(),
            vec![
                "commit".to_owned(),
                "checkout".to_owned(),
                "status".to_owned(),
            ],
        );
        let mut c = completer(data);

        let r = c.complete("git", 3);
        assert_eq!(
            values(&r),
            ["git", "git commit", "git checkout", "git status"]
        );
        assert_eq!(r.suggestions()[0].span, Span { start: 0, end: 3 });
        assert_eq!(values(&c.complete("gr", 2)), ["grep"]);

        // `git c` lists the subcommands, not the command names.
        assert_eq!(values(&c.complete("git c", 5)), ["commit", "checkout"]);

        // Another argument falls back to file names, with spaces escaped and
        // directories marked. Hidden files stay out.
        assert_eq!(values(&c.complete("cat a", 5)), ["a\\ b.txt"]);
        assert_eq!(values(&c.complete("cat ", 4)), ["a\\ b.txt", "src/"]);
        assert_eq!(values(&c.complete("cat src/", 8)), Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn completes_man_page_options_with_their_descriptions() {
        let mut c = completer(comp(&[], std::path::Path::new(".")));
        c.man.insert(
            "tar",
            vec![
                crate::man::Opt {
                    flag: "--create".to_owned(),
                    help: "create a new archive".to_owned(),
                },
                crate::man::Opt {
                    flag: "--extract".to_owned(),
                    help: "extract files".to_owned(),
                },
                crate::man::Opt {
                    flag: "-x".to_owned(),
                    help: "extract files".to_owned(),
                },
            ],
        );

        let r = c.complete("tar --e", 7);
        assert_eq!(values(&r), ["--extract"]);
        assert_eq!(
            r.suggestions()[0].description.as_deref(),
            Some("extract files")
        );
        assert_eq!(
            values(&c.complete("tar -", 5)),
            ["--create", "--extract", "-x"]
        );
        // A command with no options falls back to file names instead of hanging.
        assert!(values(&c.complete("xsh-nothing --z", 15)).is_empty());
    }
}
