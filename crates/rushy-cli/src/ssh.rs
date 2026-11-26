//! Zero-install SSH integration: preserves xsh theme colors, colored remote
//! tools (`ls`, `grep`, `diff`), OSC 7 / OSC 133 prompt marks, and live local
//! syntax highlighting over interactive SSH sessions without installing
//! anything on the remote host.

use std::fmt::Write as _;
use std::io::{self, Read as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use base64::Engine as _;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use ratatui::style::Color as TuiColor;
use rushy_core::BUILTINS;
use rushy_tui::Theme;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::app::App;
use crate::highlight::{Colors, is_keyword};

/// OpenSSH flags that consume the next argument.
const FLAGS_WITH_ARG: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// OpenSSH flags that indicate a non-interactive, control, or info-only session.
const NON_INTERACTIVE_FLAGS: &str = "NfTVGOWQs";

/// Returns the single destination argument (`[user@]host`) if `args` (everything
/// after `ssh`) is an interactive login session suitable for integration.
pub fn target_host(args: &[String]) -> Option<&str> {
    let mut target: Option<&str> = None;
    let mut positional = 0usize;
    let mut i = 0;
    let mut after_double_dash = false;
    while i < args.len() {
        let arg = &args[i];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') && arg.len() > 1 {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                let c = chars[j];
                if NON_INTERACTIVE_FLAGS.contains(c) {
                    return None;
                }
                if FLAGS_WITH_ARG.contains(c) {
                    if j + 1 < chars.len() {
                        break;
                    }
                    i += 1;
                    break;
                }
                j += 1;
            }
            i += 1;
            continue;
        }
        positional += 1;
        target = Some(arg.as_str());
        i += 1;
    }
    (positional == 1).then_some(target).flatten()
}

/// Whether `args` (everything after `ssh`) is an interactive login session
/// suitable for color/highlighting integration.
pub fn should_wrap(args: &[String]) -> bool {
    target_host(args).is_some()
}

/// Converts a `ratatui` theme color to an ANSI foreground SGR code (without ESC).
fn ansi_fg(c: TuiColor) -> String {
    match c {
        TuiColor::Rgb(r, g, b) => format!("38;2;{r};{g};{b}"),
        TuiColor::Black => "30".to_owned(),
        TuiColor::Red => "31".to_owned(),
        TuiColor::Green => "32".to_owned(),
        TuiColor::Yellow => "33".to_owned(),
        TuiColor::Blue => "34".to_owned(),
        TuiColor::Magenta => "35".to_owned(),
        TuiColor::Cyan => "36".to_owned(),
        TuiColor::Gray => "37".to_owned(),
        TuiColor::DarkGray => "90".to_owned(),
        TuiColor::LightRed => "91".to_owned(),
        TuiColor::LightGreen => "92".to_owned(),
        TuiColor::LightYellow => "93".to_owned(),
        TuiColor::LightBlue => "94".to_owned(),
        TuiColor::LightMagenta => "95".to_owned(),
        TuiColor::LightCyan => "96".to_owned(),
        TuiColor::White => "97".to_owned(),
        TuiColor::Indexed(n) => format!("38;5;{n}"),
        TuiColor::Reset => "39".to_owned(),
    }
}

const REMOTE_SCRIPT_TEMPLATE: &str = r#"export TERM="${TERM:-xterm-256color}"
case "$TERM" in ""|dumb|unknown) export TERM=xterm-256color ;; esac
export COLORTERM=truecolor
export CLICOLOR=1
export LS_COLORS="${LS_COLORS:-__LS_COLORS__}"

__xsh_common='
if ls --color=auto / >/dev/null 2>&1; then
  alias ls="ls --color=auto"
elif ls -G / >/dev/null 2>&1; then
  alias ls="ls -G"
fi
if grep --color=auto "" /dev/null >/dev/null 2>&1; then
  alias grep="grep --color=auto"
  alias egrep="egrep --color=auto"
  alias fgrep="fgrep --color=auto"
fi
if diff --color=auto /dev/null /dev/null >/dev/null 2>&1; then
  alias diff="diff --color=auto"
fi
'

__xsh_shell="${SHELL:-/bin/sh}"
case "$__xsh_shell" in
  */zsh)
    if command -v zsh >/dev/null 2>&1; then
      _zd="/tmp/.xsh-zdot-$$"
      (umask 077 && mkdir -p "$_zd") 2>/dev/null || _zd=""
      if [ -n "$_zd" ]; then
        _orig_zdot="${ZDOTDIR:-$HOME}"
        cat > "$_zd/.zshenv" <<'XSH_ZENV'
[ -f "${__XSH_ORIG_ZDOT:-$HOME}/.zshenv" ] && . "${__XSH_ORIG_ZDOT:-$HOME}/.zshenv"
XSH_ZENV
        cat > "$_zd/.zprofile" <<'XSH_ZPROF'
[ -f "${__XSH_ORIG_ZDOT:-$HOME}/.zprofile" ] && . "${__XSH_ORIG_ZDOT:-$HOME}/.zprofile"
XSH_ZPROF
        cat > "$_zd/.zshrc" <<'XSH_ZRC'
ZDOTDIR="${__XSH_ORIG_ZDOT:-$HOME}"
[ -f "$ZDOTDIR/.zshrc" ] && . "$ZDOTDIR/.zshrc"
rm -rf "$__XSH_TMP_ZDOT" 2>/dev/null
unset __XSH_TMP_ZDOT __XSH_ORIG_ZDOT
eval "$__XSH_COMMON"
unset __XSH_COMMON
autoload -Uz add-zsh-hook 2>/dev/null
__xsh_in_cmd=0
__xsh_preexec() {
  __xsh_in_cmd=1
  printf '\033]133;C\007'
}
__xsh_precmd() {
  local _s=$?
  if [ "$__xsh_in_cmd" = 1 ]; then
    printf '\033]133;D;%s\007' "$_s"
    __xsh_in_cmd=0
  fi
  printf '\033]7;file://%s%s\007\033]0;ssh:%s %s\007\033]133;A\007' "${HOST:-$(hostname)}" "$PWD" "${HOST:-$(hostname)}" "${PWD##*/}"
  local _arr_c="$__XSH_C_OK"
  [ "$_s" -ne 0 ] && _arr_c="$__XSH_C_ERR"
  PROMPT="%{\033[${__XSH_C_BADGE}m%}[ssh]%{\033[0m%} %{\033[${__XSH_C_USER}m%}%n@%m%{\033[0m%} %{\033[${__XSH_C_PATH}m%}%~%{\033[0m%} %{\033[${_arr_c}m%}❯%{\033[0m%} %{\033]133;B\007%}"
}
if typeset -f add-zsh-hook >/dev/null 2>&1; then
  add-zsh-hook precmd __xsh_precmd
  add-zsh-hook preexec __xsh_preexec
fi
XSH_ZRC
        export __XSH_ORIG_ZDOT="$_orig_zdot" __XSH_TMP_ZDOT="$_zd" __XSH_COMMON="$__xsh_common"
        export __XSH_C_BADGE="__C_BADGE__" __XSH_C_USER="__C_USER__" __XSH_C_PATH="__C_PATH__" __XSH_C_OK="__C_OK__" __XSH_C_ERR="__C_ERR__"
        export ZDOTDIR="$_zd"
        rm -f "$_f" 2>/dev/null
        exec "$__xsh_shell" -l
      fi
    fi
    ;;
  */bash)
    if command -v bash >/dev/null 2>&1; then
      _brc="/tmp/.xsh-brc-$$"
      (umask 077 && cat > "$_brc" <<'XSH_BRC'
rm -f "$__XSH_BRC" 2>/dev/null
unset __XSH_BRC
[ -f /etc/profile ] && . /etc/profile
if [ -f "$HOME/.bash_profile" ]; then
  . "$HOME/.bash_profile"
elif [ -f "$HOME/.bash_login" ]; then
  . "$HOME/.bash_login"
elif [ -f "$HOME/.profile" ]; then
  . "$HOME/.profile"
fi
[ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc"
eval "$__XSH_COMMON"
unset __XSH_COMMON
__xsh_in_cmd=0
__xsh_preexec() {
  case "$BASH_COMMAND" in
    __xsh_precmd*) return ;;
  esac
  if [ "$__xsh_in_cmd" = 0 ]; then
    __xsh_in_cmd=1
    printf '\033]133;C\007'
  fi
}
__xsh_precmd() {
  local _s=$?
  if [ "$__xsh_in_cmd" = 1 ]; then
    printf '\033]133;D;%s\007' "$_s"
    __xsh_in_cmd=0
  fi
  printf '\033]7;file://%s%s\007\033]0;ssh:%s %s\007\033]133;A\007' "${HOSTNAME:-$(hostname)}" "$PWD" "${HOSTNAME:-$(hostname)}" "${PWD##*/}"
  local _arr_c="$__XSH_C_OK"
  [ "$_s" -ne 0 ] && _arr_c="$__XSH_C_ERR"
  PS1="\[\033[${__XSH_C_BADGE}m\][ssh]\[\033[0m\] \[\033[${__XSH_C_USER}m\]\u@\h\[\033[0m\] \[\033[${__XSH_C_PATH}m\]\w\[\033[0m\] \[\033[${_arr_c}m\]❯\[\033[0m\] \[\033]133;B\007\]"
}
trap '__xsh_preexec' DEBUG
PROMPT_COMMAND="__xsh_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
XSH_BRC
      ) 2>/dev/null || _brc=""
      if [ -n "$_brc" ]; then
        export __XSH_BRC="$_brc" __XSH_COMMON="$__xsh_common"
        export __XSH_C_BADGE="__C_BADGE__" __XSH_C_USER="__C_USER__" __XSH_C_PATH="__C_PATH__" __XSH_C_OK="__C_OK__" __XSH_C_ERR="__C_ERR__"
        rm -f "$_f" 2>/dev/null
        exec "$__xsh_shell" --rcfile "$_brc" -i
      fi
    fi
    ;;
esac
rm -f "$_f" 2>/dev/null
exec "$__xsh_shell" -l
"#;

/// Builds the remote shell bootstrap script tailored to `theme`.
fn build_remote_script(theme: &Theme, plain: bool) -> String {
    let (c_badge, c_user, c_path, c_ok, c_err) = if plain || theme.plain {
        (
            "1".to_owned(),
            "1".to_owned(),
            "1".to_owned(),
            "1".to_owned(),
            "1".to_owned(),
        )
    } else {
        (
            ansi_fg(theme.accent),
            ansi_fg(theme.prompt_user),
            ansi_fg(theme.prompt_path),
            ansi_fg(theme.success),
            ansi_fg(theme.error),
        )
    };
    let dir_col = if plain || theme.plain {
        "1".to_owned()
    } else {
        format!("1;{}", ansi_fg(theme.prompt_path))
    };
    let lnk_col = if plain || theme.plain {
        "1".to_owned()
    } else {
        format!("1;{}", ansi_fg(theme.accent))
    };
    let exe_col = if plain || theme.plain {
        "1".to_owned()
    } else {
        format!("1;{}", ansi_fg(theme.success))
    };
    let ls_colors = format!(
        "di={dir_col}:ln={lnk_col}:ex={exe_col}:pi=33:so=35:bd=1;33:cd=1;33:or=1;31:mi=1;31"
    );

    REMOTE_SCRIPT_TEMPLATE
        .replace("__LS_COLORS__", &ls_colors)
        .replace("__C_BADGE__", &c_badge)
        .replace("__C_USER__", &c_user)
        .replace("__C_PATH__", &c_path)
        .replace("__C_OK__", &c_ok)
        .replace("__C_ERR__", &c_err)
}

/// Builds the single-line POSIX bootstrap command passed to `ssh -t`.
pub fn build_bootstrap(theme: &Theme, plain: bool) -> String {
    let script = build_remote_script(theme, plain);
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    format!(
        "export TERM=xterm-256color COLORTERM=truecolor CLICOLOR=1; \
         if command -v base64 >/dev/null 2>&1; then \
           _x=$(printf '%s' '{b64}' | base64 -d 2>/dev/null || printf '%s' '{b64}' | base64 -D 2>/dev/null); \
           if [ -n \"$_x\" ]; then \
             _f=\"/tmp/.xsh-ssh-$$\"; \
             (umask 077; printf '%s\\n' \"$_x\" > \"$_f\") 2>/dev/null && . \"$_f\"; \
           fi; \
         fi; \
         exec \"${{SHELL:-/bin/sh}}\" -l"
    )
}

/// Events detected while scanning remote PTY output bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanEvent {
    PromptReady,
    CommandStarted,
}

/// Streaming ANSI/OSC parser that tracks alternate-screen state and OSC 133 marks
/// across arbitrary byte-chunk boundaries.
#[derive(Debug, Default)]
struct OutputScanner {
    alt_screen: bool,
    state: ScanState,
    buf: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    #[default]
    Normal,
    Esc,
    Osc,
    OscEsc,
    Csi,
}

impl OutputScanner {
    fn feed(&mut self, bytes: &[u8]) -> Vec<ScanEvent> {
        let mut events = Vec::new();
        for &b in bytes {
            match self.state {
                ScanState::Normal => {
                    if b == 0x1b {
                        self.state = ScanState::Esc;
                    }
                }
                ScanState::Esc => match b {
                    b']' => {
                        self.buf.clear();
                        self.state = ScanState::Osc;
                    }
                    b'[' => {
                        self.buf.clear();
                        self.state = ScanState::Csi;
                    }
                    _ => self.state = ScanState::Normal,
                },
                ScanState::Osc => {
                    if b == 0x07 {
                        if let Some(ev) = Self::finish_osc(&self.buf) {
                            events.push(ev);
                        }
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    } else if b == 0x1b {
                        self.state = ScanState::OscEsc;
                    } else if self.buf.len() < 128 {
                        self.buf.push(b);
                    }
                }
                ScanState::OscEsc => {
                    if b == b'\\' {
                        if let Some(ev) = Self::finish_osc(&self.buf) {
                            events.push(ev);
                        }
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    } else {
                        self.buf.clear();
                        self.state = if b == b']' {
                            ScanState::Osc
                        } else if b == b'[' {
                            ScanState::Csi
                        } else {
                            ScanState::Normal
                        };
                    }
                }
                ScanState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.finish_csi(b);
                        self.buf.clear();
                        self.state = ScanState::Normal;
                    } else if self.buf.len() < 64 {
                        self.buf.push(b);
                    }
                }
            }
        }
        events
    }

    fn finish_osc(payload: &[u8]) -> Option<ScanEvent> {
        if payload == b"133;B" {
            Some(ScanEvent::PromptReady)
        } else if payload == b"133;C" || payload.starts_with(b"133;C;") {
            Some(ScanEvent::CommandStarted)
        } else {
            None
        }
    }

    fn finish_csi(&mut self, final_byte: u8) {
        if (final_byte == b'h' || final_byte == b'l') && self.buf.starts_with(b"?") {
            let enter = final_byte == b'h';
            for part in self.buf[1..].split(|&c| c == b';') {
                if matches!(part, b"1049" | b"47" | b"1047") {
                    self.alt_screen = enter;
                }
            }
        }
    }
}

const OP_CHARS: &[char] = &['|', '&', ';', '<', '>', '(', ')'];

fn token_len(text: &str) -> usize {
    if text.starts_with(['(', ')']) {
        return 1;
    }
    if text.starts_with(OP_CHARS) {
        return text
            .find(|c: char| !"|&;<>".contains(c))
            .unwrap_or(text.len());
    }
    let mut quote: Option<char> = None;
    for (i, c) in text.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() || OP_CHARS.contains(&c) => return i,
            Some(_) | None => {}
        }
    }
    text.len()
}

/// Syntax-highlights `line` into ANSI-escaped text using xsh's active theme
/// colors (without touching the local filesystem).
pub fn highlight_line(line: &str, colors: &Colors) -> String {
    let mut out = String::with_capacity(line.len() + 32);
    let mut rest = line;
    let mut expect_cmd = true;
    while !rest.is_empty() {
        if let Some(c) = rest.chars().next()
            && c.is_whitespace()
        {
            let n = rest
                .find(|ch: char| !ch.is_whitespace())
                .unwrap_or(rest.len());
            out.push_str(&rest[..n]);
            rest = &rest[n..];
            continue;
        }
        if rest.starts_with('#') {
            let _ = write!(out, "{}", colors.comment.paint(rest));
            break;
        }
        let n = token_len(rest);
        let tok = &rest[..n];
        rest = &rest[n..];

        if tok.starts_with(OP_CHARS) {
            let _ = write!(out, "{}", colors.operator.paint(tok));
            expect_cmd = !matches!(tok, "<" | ">" | ">>" | "<<" | "<<<" | "2>" | "2>>" | "&>");
            continue;
        }
        let is_assign = expect_cmd
            && tok.contains('=')
            && !tok.starts_with(['=', '\'', '"'])
            && tok.split('=').next().is_some_and(|k| {
                !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_')
            });
        if is_assign {
            let _ = write!(out, "{}", colors.var.paint(tok));
            continue;
        }
        if expect_cmd {
            expect_cmd = false;
            let style = if is_keyword(tok) {
                if matches!(tok, "then" | "do" | "else" | "elif" | "!" | "{") {
                    expect_cmd = true;
                }
                colors.keyword
            } else if BUILTINS.iter().any(|(b, _)| *b == tok) {
                colors.builtin
            } else {
                colors.command
            };
            let _ = write!(out, "{}", style.paint(tok));
            continue;
        }
        let style = if tok.starts_with(['\'', '"']) {
            colors.string
        } else if tok.starts_with('$') {
            colors.var
        } else if tok.starts_with('-') {
            colors.flag
        } else if is_keyword(tok) {
            colors.keyword
        } else {
            nu_ansi_term::Style::new()
        };
        let _ = write!(out, "{}", style.paint(tok));
    }
    out
}

/// Outcome of feeding local stdin bytes into [`LocalEditor`].
#[derive(Debug, PartialEq, Eq)]
enum EditAction {
    /// Redraw the locally highlighted line (bytes to write to local stdout).
    Redraw(Vec<u8>),
    /// Clear local preview and send these bytes to the remote PTY, then leave local edit mode.
    ForwardAndPassThrough(Vec<u8>),
}

/// Minimal local line editor that renders live xsh syntax highlighting at a
/// remote shell prompt and falls back cleanly to pass-through for Tab/history.
#[derive(Debug, Default)]
struct LocalEditor {
    buf: String,
    /// Cursor position in `buf` as a byte index (always on a UTF-8 char boundary).
    cursor: usize,
}

impl LocalEditor {
    fn reset(&mut self) {
        self.buf.clear();
        self.cursor = 0;
    }

    fn render(&self, colors: &Colors) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.buf.len() + 48);
        // Restore saved cursor (DECRC) and clear to end of screen.
        out.extend_from_slice(b"\x1b8\x1b[J");
        let styled = highlight_line(&self.buf, colors);
        out.extend_from_slice(styled.as_bytes());
        let tail_chars = self.buf[self.cursor..].chars().count();
        if tail_chars > 0 {
            out.extend_from_slice(format!("\x1b[{tail_chars}D").as_bytes());
        }
        out
    }

    fn handle_input(&mut self, bytes: &[u8], colors: &Colors) -> EditAction {
        match bytes {
            b"\r" | b"\n" => {
                let mut send = self.buf.as_bytes().to_vec();
                send.push(b'\r');
                self.reset();
                EditAction::ForwardAndPassThrough(send)
            }
            b"\x03" => {
                self.reset();
                EditAction::ForwardAndPassThrough(vec![0x03])
            }
            b"\x04" => {
                if self.buf.is_empty() {
                    EditAction::ForwardAndPassThrough(vec![0x04])
                } else {
                    self.delete_at_cursor();
                    EditAction::Redraw(self.render(colors))
                }
            }
            b"\x7f" | b"\x08" => {
                if self.cursor > 0 {
                    let prev = self.buf[..self.cursor]
                        .char_indices()
                        .last()
                        .map_or(0, |(idx, _)| idx);
                    self.buf.drain(prev..self.cursor);
                    self.cursor = prev;
                }
                EditAction::Redraw(self.render(colors))
            }
            b"\x01" | b"\x1b[H" | b"\x1bOH" => {
                self.cursor = 0;
                EditAction::Redraw(self.render(colors))
            }
            b"\x05" | b"\x1b[F" | b"\x1bOF" => {
                self.cursor = self.buf.len();
                EditAction::Redraw(self.render(colors))
            }
            b"\x15" => {
                self.buf.drain(..self.cursor);
                self.cursor = 0;
                EditAction::Redraw(self.render(colors))
            }
            b"\x0b" => {
                self.buf.truncate(self.cursor);
                EditAction::Redraw(self.render(colors))
            }
            b"\x17" => {
                let before = &self.buf[..self.cursor];
                let trimmed = before.trim_end_matches(char::is_whitespace);
                let new_cur = trimmed.rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.buf.drain(new_cur..self.cursor);
                self.cursor = new_cur;
                EditAction::Redraw(self.render(colors))
            }
            b"\x1b[D" => {
                if self.cursor > 0 {
                    self.cursor = self.buf[..self.cursor]
                        .char_indices()
                        .last()
                        .map_or(0, |(idx, _)| idx);
                }
                EditAction::Redraw(self.render(colors))
            }
            b"\x1b[C" => {
                if self.cursor < self.buf.len()
                    && let Some(c) = self.buf[self.cursor..].chars().next()
                {
                    self.cursor += c.len_utf8();
                }
                EditAction::Redraw(self.render(colors))
            }
            b"\x1b[3~" => {
                self.delete_at_cursor();
                EditAction::Redraw(self.render(colors))
            }
            _ => {
                if let Ok(s) = std::str::from_utf8(bytes)
                    && !s.is_empty()
                    && s.chars().all(|c| !c.is_control())
                {
                    self.buf.insert_str(self.cursor, s);
                    self.cursor += s.len();
                    return EditAction::Redraw(self.render(colors));
                }
                // Tab, Up/Down, Ctrl-R, or any other escape: flush typed prefix
                // and hand control back to the remote shell for this prompt.
                let mut send = self.buf.as_bytes().to_vec();
                send.extend_from_slice(bytes);
                self.reset();
                EditAction::ForwardAndPassThrough(send)
            }
        }
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor < self.buf.len()
            && let Some(c) = self.buf[self.cursor..].chars().next()
        {
            self.buf.drain(self.cursor..self.cursor + c.len_utf8());
        }
    }
}

struct RawGuard;

impl RawGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("cannot enable raw terminal mode")?;
        Ok(Self)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Output signatures indicating the remote host rejected the POSIX `exec`
/// bootstrap (e.g. Windows OpenSSH, `cmd.exe`, PowerShell, or restricted shells).
fn looks_like_bootstrap_rejected(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    [
        "exec request failed",
        "'export' is not recognized",
        "the term 'export' is not recognized",
        "the syntax of the command is incorrect",
        "unrecognized command",
        "invalid input detected",
    ]
    .iter()
    .any(|pat| lower.contains(pat))
}

fn plain_hosts_file(app: &App) -> std::path::PathBuf {
    app.paths.data_dir().join("ssh-plain-hosts")
}

fn is_plain_host(path: &std::path::Path, host: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text.lines().any(|line| line.trim() == host))
}

fn remember_plain_host(path: &std::path::Path, host: &str) {
    if host.is_empty() || host.contains(['\n', '\r']) || is_plain_host(path, host) {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{host}");
    }
}

/// Runs an interactive SSH session with zero-install color/prompt bootstrap
/// and live local syntax highlighting at the remote prompt. Automatically
/// falls back to a plain `shell` channel on Windows or restricted SSH servers.
pub async fn run(app: &App, args: &[String]) -> Result<i32> {
    let plain = app.cfg.ui.no_color || app.theme.plain || std::env::var_os("NO_COLOR").is_some();
    let colors = Colors::new(&app.theme, plain);
    let enable_local_highlight = app.cfg.ui.highlight && !plain;
    let cache = plain_hosts_file(app);
    let host = target_host(args);

    let _raw = RawGuard::enter()?;
    let mut stdin = spawn_stdin_reader();

    if host.is_some_and(|h| is_plain_host(&cache, h)) {
        let (status, _, _) = run_session(args, None, &colors, false, &mut stdin.rx).await?;
        return Ok(status);
    }

    let bootstrap = build_bootstrap(&app.theme, plain);
    let (status, saw_prompt, head) = run_session(
        args,
        Some(&bootstrap),
        &colors,
        enable_local_highlight,
        &mut stdin.rx,
    )
    .await?;
    if !saw_prompt && status != 0 && looks_like_bootstrap_rejected(&head) {
        if let Some(h) = host {
            remember_plain_host(&cache, h);
        }
        let (fallback_status, _, _) =
            run_session(args, None, &colors, false, &mut stdin.rx).await?;
        return Ok(fallback_status);
    }
    Ok(status)
}

async fn run_session(
    args: &[String],
    bootstrap: Option<&str>,
    colors: &Colors,
    enable_local_highlight: bool,
    stdin_rx: &mut mpsc::Receiver<Option<Vec<u8>>>,
) -> Result<(i32, bool, String)> {
    let (cols, rows) = size().unwrap_or((80, 24));
    let pty_sys = native_pty_system();
    let pair = pty_sys
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("cannot open local PTY for ssh")?;

    let mut cmd = CommandBuilder::new("ssh");
    if bootstrap.is_some() {
        cmd.arg("-t");
    }
    for a in args {
        cmd.arg(a);
    }
    if let Some(script) = bootstrap {
        cmd.arg(script);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd).context("cannot start ssh")?;
    drop(pair.slave);

    let pty_reader = pair
        .master
        .try_clone_reader()
        .context("cannot read ssh PTY")?;
    let pty_writer = pair.master.take_writer().context("cannot write ssh PTY")?;

    let mut pty_rx = spawn_pty_reader(pty_reader);
    let (saw_prompt, head) = proxy_loop(
        &*pair.master,
        pty_writer,
        &mut pty_rx,
        stdin_rx,
        colors,
        enable_local_highlight,
    )
    .await;

    let status = child
        .wait()
        .map_or(1, |s| i32::try_from(s.exit_code()).unwrap_or(1));
    Ok((status, saw_prompt, head))
}

struct StdinReader {
    rx: mpsc::Receiver<Option<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for StdinReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.rx.close();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_stdin_reader() -> StdinReader {
    let (tx, rx) = mpsc::channel(64);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let mut stdin = io::stdin();
        let mut buf = [0u8; 1024];
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 50_000_000,
        };
        while !flag.load(Ordering::Relaxed) {
            let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
            match poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Ok(_) => {
                    let rev = fds[0].revents();
                    if !rev.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
                        continue;
                    }
                }
                Err(_) => {
                    let _ = tx.blocking_send(None);
                    break;
                }
            }
            if flag.load(Ordering::Relaxed) {
                break;
            }
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.blocking_send(None);
                    break;
                }
                Ok(n) => {
                    if tx.blocking_send(Some(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
    StdinReader {
        rx,
        stop,
        handle: Some(handle),
    }
}

fn spawn_pty_reader(mut pty_reader: Box<dyn io::Read + Send>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(128);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

async fn proxy_loop(
    master: &dyn portable_pty::MasterPty,
    mut pty_writer: Box<dyn io::Write + Send>,
    pty_rx: &mut mpsc::Receiver<Vec<u8>>,
    stdin_rx: &mut mpsc::Receiver<Option<Vec<u8>>>,
    colors: &Colors,
    enable_local_highlight: bool,
) -> (bool, String) {
    let mut winch = signal(SignalKind::window_change()).ok();
    let mut stdout = io::stdout();
    let mut scanner = OutputScanner::default();
    let mut editor = LocalEditor::default();
    let mut local_edit = false;
    let mut saw_prompt = false;
    let mut head = Vec::<u8>::new();

    loop {
        tokio::select! {
            () = async {
                match winch.as_mut() {
                    Some(sig) => { let _ = sig.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let Ok((c, r)) = size() {
                    let _ = master.resize(PtySize {
                        rows: r,
                        cols: c,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
            out = pty_rx.recv() => {
                let Some(bytes) = out else { break };
                if !saw_prompt && head.len() < 4096 {
                    let take = (4096 - head.len()).min(bytes.len());
                    head.extend_from_slice(&bytes[..take]);
                }
                if local_edit && !editor.buf.is_empty() {
                    let _ = stdout.write_all(b"\x1b8\x1b[J");
                    editor.reset();
                    local_edit = false;
                }
                let events = scanner.feed(&bytes);
                let _ = stdout.write_all(&bytes);
                for ev in events {
                    match ev {
                        ScanEvent::PromptReady => {
                            saw_prompt = true;
                            if enable_local_highlight && !scanner.alt_screen {
                                editor.reset();
                                local_edit = true;
                                let _ = stdout.write_all(b"\x1b7");
                            }
                        }
                        ScanEvent::CommandStarted => {
                            editor.reset();
                            local_edit = false;
                        }
                    }
                }
                let _ = stdout.flush();
            }
            inp = stdin_rx.recv() => {
                if let Some(Some(bytes)) = inp {
                    if local_edit && !scanner.alt_screen {
                        match editor.handle_input(&bytes, colors) {
                            EditAction::Redraw(seq) => {
                                let _ = stdout.write_all(&seq);
                                let _ = stdout.flush();
                            }
                            EditAction::ForwardAndPassThrough(send) => {
                                let _ = stdout.write_all(b"\x1b8\x1b[J");
                                let _ = stdout.flush();
                                local_edit = false;
                                let _ = pty_writer.write_all(&send);
                                let _ = pty_writer.flush();
                            }
                        }
                    } else {
                        let _ = pty_writer.write_all(&bytes);
                        let _ = pty_writer.flush();
                    }
                } else {
                    let _ = pty_writer.write_all(&[0x04]);
                    let _ = pty_writer.flush();
                }
            }
        }
    }
    (saw_prompt, String::from_utf8_lossy(&head).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(list: &[&str]) -> Vec<String> {
        list.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn wraps_only_interactive_login_ssh() {
        assert!(should_wrap(&s(&["user@host"])));
        assert!(should_wrap(&s(&["-p", "2222", "-i", "~/.ssh/id", "host"])));
        assert!(should_wrap(&s(&["-p2222", "-A", "prod"])));
        assert!(!should_wrap(&s(&[])));
        assert!(!should_wrap(&s(&["host", "ls", "-la"])));
        assert!(!should_wrap(&s(&["-N", "-L", "8080:localhost:80", "host"])));
        assert!(!should_wrap(&s(&["-T", "host"])));
        assert!(!should_wrap(&s(&["-V"])));
        assert!(!should_wrap(&s(&["-G", "host"])));
        assert!(!should_wrap(&s(&["-W", "h:22", "bastion"])));
    }

    #[test]
    fn scanner_tracks_osc133_and_alt_screen_across_chunks() {
        let mut sc = OutputScanner::default();
        assert!(sc.feed(b"hello \x1b]133").is_empty());
        assert_eq!(sc.feed(b";B\x07"), vec![ScanEvent::PromptReady]);
        assert_eq!(
            sc.feed(b"\x1b]133;C\x1b\\"),
            vec![ScanEvent::CommandStarted]
        );
        assert!(!sc.alt_screen);
        sc.feed(b"\x1b[?1049h");
        assert!(sc.alt_screen);
        sc.feed(b"\x1b[?1049l");
        assert!(!sc.alt_screen);
    }

    #[test]
    fn local_editor_highlights_and_falls_back_on_tab() {
        let theme = Theme::builtin("neon").expect("builtin neon theme");
        let colors = Colors::new(&theme, false);
        let mut ed = LocalEditor::default();
        assert!(matches!(
            ed.handle_input(b"ls -la", &colors),
            EditAction::Redraw(_)
        ));
        assert_eq!(ed.buf, "ls -la");
        // Tab flushes typed prefix + tab and drops to pass-through.
        assert_eq!(
            ed.handle_input(b"\t", &colors),
            EditAction::ForwardAndPassThrough(b"ls -la\t".to_vec())
        );
        assert!(ed.buf.is_empty());
    }

    #[test]
    fn bootstrap_contains_theme_and_osc133() {
        let theme = Theme::builtin("neon").expect("builtin neon theme");
        let cmd = build_bootstrap(&theme, false);
        assert!(cmd.contains("COLORTERM=truecolor"));
        assert!(cmd.contains("base64"));
        let script = build_remote_script(&theme, false);
        assert!(script.contains("133;B"));
        assert!(script.contains("LS_COLORS"));
    }

    #[test]
    fn detects_windows_and_restricted_exec_rejection() {
        assert!(looks_like_bootstrap_rejected(
            "exec request failed on channel 0\r\n"
        ));
        assert!(looks_like_bootstrap_rejected(
            "'export' is not recognized as an internal or external command"
        ));
        assert!(looks_like_bootstrap_rejected(
            "The term 'export' is not recognized"
        ));
        assert!(!looks_like_bootstrap_rejected(
            "Permission denied (publickey)."
        ));
        assert!(!looks_like_bootstrap_rejected("Connection refused"));
    }
}
