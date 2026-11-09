//! Running a command line: built-ins that live in the `rushy` binary
//! (`bundle`, `ai`, `theme`, `history`, `help`) next to the shell core.

use std::time::Instant;

use clap::Parser as _;
use rushy_core::parser::{self, Word, WordPart};
use rushy_core::{BUILTINS, Error, Flow, Result};

use crate::app::{App, EXTRA_BUILTINS};
use crate::cli::Cli;

/// Commands handled here instead of by the core.
const OWN: &[&str] = &[
    "bundle",
    "ai",
    "theme",
    "config",
    "history",
    "help",
    "adb-find",
    "adb-connect",
];

/// Splits `line` at `;` and newlines outside quotes.
pub fn split_top_level(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut single, mut double, mut escaped) = (false, false, false);
    for c in line.chars() {
        if escaped {
            escaped = false;
            cur.push(c);
            continue;
        }
        match c {
            '\\' if !single => {
                escaped = true;
                cur.push(c);
            }
            '\'' if !double => {
                single = !single;
                cur.push(c);
            }
            '"' if !single => {
                double = !double;
                cur.push(c);
            }
            ';' | '\n' if !single && !double => {
                if cur.trim().is_empty() {
                    cur.clear();
                } else {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

fn expand(word: &Word, app: &App) -> String {
    let s = app.shell.state();
    word.parts
        .iter()
        .map(|p| match p {
            WordPart::Literal(t) => t.clone(),
            WordPart::Var(n) => s.lookup(n).unwrap_or_default(),
            WordPart::Tilde => s.home().unwrap_or_else(|| "~".into()),
        })
        .collect()
}

/// The expanded words of `segment` if it is one plain command that the
/// `rushy` binary handles itself.
pub fn intercept(segment: &str, app: &App) -> Option<Vec<String>> {
    let pipelines = parser::parse(segment, app.shell.state().aliases()).ok()?;
    let [p] = pipelines.as_slice() else {
        return None;
    };
    let [cmd] = p.commands.as_slice() else {
        return None;
    };
    if p.background || !cmd.redirects.is_empty() {
        return None;
    }
    let words: Vec<String> = cmd.words.iter().map(|w| expand(w, app)).collect();
    let first = words.first()?;
    if !OWN.contains(&first.as_str()) {
        return None;
    }
    // Without the encrypted store, `history` stays the core's in-memory list.
    if first == "history" && app.history.is_none() {
        return None;
    }
    Some(words)
}

fn print_help() {
    println!("xsh built-in commands:");
    for (name, desc) in BUILTINS.iter().chain(EXTRA_BUILTINS) {
        println!("  {name:<10}{desc}");
    }
    println!("\nKeys: Ctrl-R history search, Ctrl-K palette, Ctrl-Space command from description,");
    println!("      Ctrl-Shift-Space (or Alt-G) agent mode, Alt-A agent on autopilot");
}

/// Runs one built-in implemented in the binary; returns its status.
async fn run_own(app: &mut App, words: Vec<String>, text: &str) -> i32 {
    if words.first().is_some_and(|w| w == "help") {
        print_help();
        app.shell.record_external(text, 0, "");
        return 0;
    }
    let args = std::iter::once("xsh".to_owned()).chain(words);
    let (status, stderr) = match Cli::try_parse_from(args) {
        Err(e) => {
            let _ = e.print();
            (e.exit_code(), e.to_string())
        }
        Ok(cli) => match cli.sub {
            None => (0, String::new()),
            Some(sub) => match crate::dispatch(app, sub).await {
                Ok(code) => (code, String::new()),
                Err(e) => {
                    eprintln!("xsh: {e:#}");
                    (1, format!("xsh: {e:#}"))
                }
            },
        },
    };
    app.shell.record_external(text, status, &stderr);
    status
}

/// Runs a command line: built-ins of the binary and everything else through
/// the shell core. Records the line in the encrypted history.
///
/// # Errors
///
/// Returns [`Error::Incomplete`] (before running anything) when the line
/// needs more input, and the core's errors otherwise.
pub async fn execute(app: &mut App, line: &str) -> Result<Flow> {
    if let Err(Error::Incomplete) = parser::parse(line, app.shell.state().aliases()) {
        return Err(Error::Incomplete);
    }
    let started = Instant::now();
    let mut pending: Vec<String> = Vec::new();
    let mut flow = Flow::Continue;
    let mut result = Ok(());
    // A compound command is not split at `;`, so the core sees it whole.
    let segs = if crate::hist::is_compound(line) {
        vec![line.to_owned()]
    } else {
        split_top_level(line)
    };
    'segments: {
        for seg in segs {
            if let Some(words) = intercept(&seg, app) {
                if !pending.is_empty() {
                    match app.shell.run_line(&pending.join("\n")).await {
                        Ok(Flow::Exit(c)) => {
                            flow = Flow::Exit(c);
                            break 'segments;
                        }
                        Ok(Flow::Continue) => {}
                        Err(e) => {
                            result = Err(e);
                            break 'segments;
                        }
                    }
                    pending.clear();
                }
                run_own(app, words, seg.trim()).await;
            } else {
                pending.push(seg);
            }
        }
        if !pending.is_empty() {
            match app.shell.run_line(&pending.join("\n")).await {
                Ok(f) => flow = f,
                Err(e) => result = Err(e),
            }
        }
    }
    let status = match flow {
        Flow::Exit(c) => c,
        Flow::Continue => app.shell.last_status(),
    };
    app.record_history(line, status, started.elapsed());
    result.map(|()| flow)
}

#[cfg(test)]
mod tests {
    use super::split_top_level;

    #[test]
    fn splits_outside_quotes_only() {
        assert_eq!(split_top_level("a; b\nc"), ["a", " b", "c"]);
        assert_eq!(
            split_top_level(r#"echo 'a;b' ; echo "c;d" ; echo e\;f"#),
            ["echo 'a;b' ", " echo \"c;d\" ", " echo e\\;f"]
        );
        assert_eq!(split_top_level(" ;; "), Vec::<String>::new());
        assert_eq!(split_top_level("bundle list"), ["bundle list"]);
    }
}
