//! Multiplexer key handling: the prefix key state machine and key-to-byte
//! translation for pane input.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rushy_pty::{Dir, MuxCommand};

/// Parses a prefix such as `C-b` (Ctrl-b) or `C-a`.
///
/// Only `C-<letter>` and `C-<symbol>` forms are accepted.
pub fn parse_prefix(spec: &str) -> Option<(KeyCode, KeyModifiers)> {
    let rest = spec
        .strip_prefix("C-")
        .or_else(|| spec.strip_prefix("c-"))?;
    let mut chars = rest.chars();
    let c = chars.next()?;
    chars
        .next()
        .is_none()
        .then(|| (KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::CONTROL))
}

fn modifier_param(m: KeyModifiers) -> u8 {
    1 + u8::from(m.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(m.contains(KeyModifiers::ALT))
        + 4 * u8::from(m.contains(KeyModifiers::CONTROL))
}

fn csi(final_byte: char, m: KeyModifiers, number: Option<u8>) -> Vec<u8> {
    let p = modifier_param(m);
    match (number, p) {
        (None, 1) => format!("\x1b[{final_byte}"),
        (None, p) => format!("\x1b[1;{p}{final_byte}"),
        (Some(n), 1) => format!("\x1b[{n}{final_byte}"),
        (Some(n), p) => format!("\x1b[{n};{p}{final_byte}"),
    }
    .into_bytes()
}

/// The bytes a terminal would send for `key`.
pub fn key_bytes(key: KeyEvent) -> Vec<u8> {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            let lower = c.to_ascii_lowercase();
            let control_byte = match lower {
                'a'..='z' if ctrl => u8::try_from(lower).ok().map(|b| b - b'a' + 1),
                ' ' | '@' | '2' if ctrl => Some(0),
                '[' | '3' if ctrl => Some(0x1b),
                '\\' | '4' if ctrl => Some(0x1c),
                ']' | '5' if ctrl => Some(0x1d),
                '^' | '6' if ctrl => Some(0x1e),
                '_' | '7' if ctrl => Some(0x1f),
                '?' | '8' if ctrl => Some(0x7f),
                _ => None,
            };
            if let Some(b) = control_byte {
                out.push(b);
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            out
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if alt {
                b"\x1b\x7f".to_vec()
            } else {
                b"\x7f".to_vec()
            }
        }
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up => csi('A', m, None),
        KeyCode::Down => csi('B', m, None),
        KeyCode::Right => csi('C', m, None),
        KeyCode::Left => csi('D', m, None),
        KeyCode::Home => csi('H', m, None),
        KeyCode::End => csi('F', m, None),
        KeyCode::Insert => csi('~', m, Some(2)),
        KeyCode::Delete => csi('~', m, Some(3)),
        KeyCode::PageUp => csi('~', m, Some(5)),
        KeyCode::PageDown => csi('~', m, Some(6)),
        KeyCode::F(n @ 1..=4) => {
            let c = match n {
                1 => 'P',
                2 => 'Q',
                3 => 'R',
                _ => 'S',
            };
            format!("\x1bO{c}").into_bytes()
        }
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            csi('~', m, Some(code))
        }
        _ => Vec::new(),
    }
}

/// What to do with a key press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send these bytes to the focused pane.
    Send(Vec<u8>),
    /// Run a multiplexer command.
    Command(MuxCommand),
    /// Nothing to do.
    Ignore,
}

/// The prefix-key state machine.
#[derive(Debug, Clone)]
pub struct Keymap {
    prefix: (KeyCode, KeyModifiers),
    pending: bool,
}

impl Keymap {
    /// A keymap with the given prefix.
    pub fn new(prefix: (KeyCode, KeyModifiers)) -> Self {
        Self {
            prefix,
            pending: false,
        }
    }

    fn is_prefix(&self, key: KeyEvent) -> bool {
        key.code == self.prefix.0 && key.modifiers.contains(self.prefix.1)
    }

    /// Feeds one key press.
    pub fn feed(&mut self, key: KeyEvent) -> Action {
        if !self.pending {
            if self.is_prefix(key) {
                self.pending = true;
                return Action::Ignore;
            }
            return Action::Send(key_bytes(key));
        }
        self.pending = false;
        if self.is_prefix(key) {
            return Action::Send(key_bytes(key));
        }
        let resize = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let dir = |d: Dir| {
            Action::Command(if resize {
                MuxCommand::Resize(d)
            } else {
                MuxCommand::Focus(d)
            })
        };
        match key.code {
            KeyCode::Char('%') => Action::Command(MuxCommand::SplitHorizontal),
            KeyCode::Char('"') => Action::Command(MuxCommand::SplitVertical),
            KeyCode::Char('h') | KeyCode::Left => dir(Dir::Left),
            KeyCode::Char('j') | KeyCode::Down => dir(Dir::Down),
            KeyCode::Char('k') | KeyCode::Up => dir(Dir::Up),
            KeyCode::Char('l') | KeyCode::Right => dir(Dir::Right),
            KeyCode::Char('c') => Action::Command(MuxCommand::NewWindow),
            KeyCode::Char('n') => Action::Command(MuxCommand::NextWindow),
            KeyCode::Char('p') => Action::Command(MuxCommand::PrevWindow),
            KeyCode::Char('d') => Action::Command(MuxCommand::Detach),
            KeyCode::Char('x') => Action::Command(MuxCommand::KillPane),
            _ => Action::Ignore,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn parses_prefixes() {
        assert_eq!(
            parse_prefix("C-b"),
            Some((KeyCode::Char('b'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            parse_prefix("C-A"),
            Some((KeyCode::Char('a'), KeyModifiers::CONTROL))
        );
        assert_eq!(parse_prefix("b"), None);
        assert_eq!(parse_prefix("C-"), None);
        assert_eq!(parse_prefix("C-ab"), None);
    }

    #[test]
    fn translates_keys_to_terminal_bytes() {
        assert_eq!(key_bytes(k(KeyCode::Char('é'))), "é".as_bytes());
        assert_eq!(key_bytes(ctrl('c')), [3]);
        assert_eq!(key_bytes(ctrl(' ')), [0]);
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)),
            b"\x1bx"
        );
        assert_eq!(key_bytes(k(KeyCode::Enter)), b"\r");
        assert_eq!(key_bytes(k(KeyCode::Backspace)), [0x7f]);
        assert_eq!(key_bytes(k(KeyCode::Up)), b"\x1b[A");
        assert_eq!(
            key_bytes(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
            b"\x1b[1;5D"
        );
        assert_eq!(key_bytes(k(KeyCode::Delete)), b"\x1b[3~");
        assert_eq!(key_bytes(k(KeyCode::F(1))), b"\x1bOP");
        assert_eq!(key_bytes(k(KeyCode::F(5))), b"\x1b[15~");
        assert!(key_bytes(k(KeyCode::Null)).is_empty());
    }

    #[test]
    fn prefix_commands() {
        let mut m = Keymap::new((KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(m.feed(k(KeyCode::Char('a'))), Action::Send(b"a".to_vec()));
        let cases = [
            (k(KeyCode::Char('%')), MuxCommand::SplitHorizontal),
            (k(KeyCode::Char('"')), MuxCommand::SplitVertical),
            (k(KeyCode::Char('h')), MuxCommand::Focus(Dir::Left)),
            (k(KeyCode::Down), MuxCommand::Focus(Dir::Down)),
            (
                KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL),
                MuxCommand::Resize(Dir::Right),
            ),
            (k(KeyCode::Char('c')), MuxCommand::NewWindow),
            (k(KeyCode::Char('n')), MuxCommand::NextWindow),
            (k(KeyCode::Char('p')), MuxCommand::PrevWindow),
            (k(KeyCode::Char('d')), MuxCommand::Detach),
            (k(KeyCode::Char('x')), MuxCommand::KillPane),
        ];
        for (key, cmd) in cases {
            assert_eq!(m.feed(ctrl('b')), Action::Ignore);
            assert_eq!(m.feed(key), Action::Command(cmd));
        }
        // Prefix twice sends the prefix; unknown keys are swallowed once.
        assert_eq!(m.feed(ctrl('b')), Action::Ignore);
        assert_eq!(m.feed(ctrl('b')), Action::Send(vec![2]));
        assert_eq!(m.feed(ctrl('b')), Action::Ignore);
        assert_eq!(m.feed(k(KeyCode::Char('?'))), Action::Ignore);
        assert_eq!(m.feed(k(KeyCode::Char('q'))), Action::Send(b"q".to_vec()));
    }
}
