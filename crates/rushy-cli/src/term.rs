//! Terminal integration: window title, OSC 7 working directory, OSC 133
//! prompt marks, and a notice when a long command ends.

use std::io::{self, IsTerminal as _, Write as _};
use std::path::Path;

/// Removes control characters so command text cannot inject escapes.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Percent-encodes a path for a `file://` URL.
fn encode(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{b:02X}"));
        }
    }
    out
}

fn emit(seq: &str) {
    let mut out = io::stdout();
    if out.is_terminal() {
        let _ = out.write_all(seq.as_bytes());
        let _ = out.flush();
    }
}

/// Sets the title of the terminal.
pub fn set_title(title: &str) {
    emit(&format!("\x1b]0;{}\x07", clean(title)));
}

/// Asks `xsh.app` for an agent pane to the right, connected to `socket`.
pub fn agent_split(token: &str, socket: &Path) {
    emit(&format!(
        "\x1b]7741;split;{};{}\x07",
        clean(token),
        clean(&socket.to_string_lossy())
    ));
}

/// Before the prompt: report the directory, set the title, and mark the prompt.
pub fn prompt_start(cwd: &Path, host: &str) {
    let dir = cwd.to_string_lossy();
    let name = cwd
        .file_name()
        .map_or_else(|| dir.to_string(), |n| n.to_string_lossy().into_owned());
    emit(&format!(
        "\x1b]7;file://{}{}\x07\x1b]0;xsh: {}\x07\x1b]133;A\x07",
        clean(host),
        encode(&dir),
        clean(&name)
    ));
}

/// After the line is submitted: title shows the command; output starts.
pub fn command_start(line: &str) {
    let first: String = clean(line.lines().next().unwrap_or_default())
        .chars()
        .take(60)
        .collect();
    emit(&format!("\x1b]0;{first}\x07\x1b]133;C\x07"));
}

/// When the command ends, with its exit status.
pub fn command_end(status: i32) {
    emit(&format!("\x1b]133;D;{status}\x07"));
}

/// A bell and a one-line notice after a long command.
pub fn notify(line: &str, status: i32, took: &str) {
    let cmd: String = clean(line).chars().take(60).collect();
    let state = if status == 0 { "done" } else { "failed" };
    eprintln!("\x07xsh: `{cmd}` {state} ({status}) after {took}");
    // OSC 9 shows a desktop notice in terminals that support it.
    emit(&format!("\x1b]9;xsh: {cmd} {state} after {took}\x07"));
}
