//! Option suggestions taken from man pages.
//!
//! After you run a command, xsh reads its man page in the background and
//! remembers the options. When you later type `-` and press Tab, the options
//! of that command (or of its subcommand page, such as `git-commit`) are
//! offered with a short description.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(2);
const READER_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_MAN_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OPTIONS: usize = 400;
const MAX_HELP: usize = 70;
const MAX_CACHE_ENTRIES: usize = 128;

/// One option found in a man page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opt {
    /// The flag, such as `-x` or `--verbose`.
    pub flag: String,
    /// A short description.
    pub help: String,
}

/// Whether `name` is safe to pass to `man` as a page name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// Removes terminal overstrikes (`x\bx`, `_\bx`) and ANSI/OSC escapes.
pub fn clean(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{8}' => {
                out.pop();
            }
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

fn clip(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let first = one_line
        .split_once(". ")
        .map_or(one_line.as_str(), |(a, _)| a)
        .trim_end_matches('.');
    if first.chars().count() <= MAX_HELP {
        first.to_owned()
    } else {
        let cut: String = first.chars().take(MAX_HELP - 1).collect();
        format!("{cut}…")
    }
}

/// Turns one word of the flag column into a flag, or `None`.
fn as_flag(token: &str) -> Option<String> {
    let token = token.trim_matches(|c: char| matches!(c, ',' | ';' | '|'));
    let name = token.split(['=', '[', '<']).next().unwrap_or_default();
    let body = name.trim_start_matches('-');
    let dashes = name.len() - body.len();
    let valid = (1..=2).contains(&dashes)
        && !body.is_empty()
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        && (dashes == 2 || body.chars().all(|c| c.is_ascii_alphanumeric()))
        && body.chars().next().is_some_and(char::is_alphanumeric);
    valid.then(|| name.to_owned())
}

/// The flags of an option header such as `-C DIR, --directory=DIR`, or `None`
/// when the text is prose. Words are allowed only as the single argument name
/// right after a flag, so `-F, -d, or -l options are specified.` is rejected.
fn flags_of(column: &str) -> Option<Vec<String>> {
    let mut flags = Vec::new();
    let mut after_flag = false;
    for tok in column
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
    {
        if let Some(f) = as_flag(tok) {
            flags.push(f);
            after_flag = true;
        } else if after_flag && tok.len() <= 24 && !tok.ends_with('.') {
            after_flag = false;
        } else {
            return None;
        }
    }
    (!flags.is_empty()).then_some(flags)
}

/// Finds the options in the text of a man page.
///
/// It reads lines that start with a flag (`-v, --verbose  Be loud.`). The
/// description is the rest of the line, or the next line when the flags stand
/// alone.
pub fn parse_options(text: &str) -> Vec<Opt> {
    let lines: Vec<&str> = text.lines().collect();
    // Each option with the indent of the line that defined it. Wrapped prose
    // can start with a flag too (`-F, -d, or -l options are ...`); the real
    // entry sits further left, so the smaller indent wins.
    let mut out: Vec<(Opt, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let indent = line.len() - t.len();
        if !t.starts_with('-') || t.starts_with("--  ") || t == "--" {
            continue;
        }
        let split = t.find("  ").or_else(|| t.find('\t'));
        let (flags, mut help) = match split {
            Some(at) => (&t[..at], t[at..].trim().to_owned()),
            None => (t, String::new()),
        };
        if help.is_empty() {
            lines
                .iter()
                .skip(i + 1)
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .filter(|l| !l.starts_with('-'))
                .unwrap_or_default()
                .clone_into(&mut help);
        }
        let Some(flags) = flags_of(flags) else {
            continue;
        };
        let help = clip(&help);
        for flag in flags {
            match out.iter_mut().find(|(o, _)| o.flag == flag) {
                Some((o, old)) => {
                    if indent < *old {
                        o.help.clone_from(&help);
                        *old = indent;
                    }
                }
                None => out.push((
                    Opt {
                        flag,
                        help: help.clone(),
                    },
                    indent,
                )),
            }
        }
        if out.len() >= MAX_OPTIONS {
            break;
        }
    }
    out.into_iter().map(|(o, _)| o).collect()
}

/// Runs `man <page>` and returns its text; `None` when there is no page, no
/// `man`, or it takes longer than two seconds.
fn fetch(page: &str) -> Option<String> {
    if !valid_name(page) {
        return None;
    }
    let mut child = Command::new("man")
        .arg(page)
        .env("MANPAGER", "cat")
        .env("PAGER", "cat")
        .env("MANWIDTH", "100")
        .env("TERM", "dumb")
        .env_remove("MAN_KEEP_FORMATTING")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.take(MAX_MAN_BYTES).read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if started.elapsed() < TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if !status.is_some_and(|s| s.success()) {
        return None;
    }
    let bytes = rx.recv_timeout(READER_DRAIN_TIMEOUT).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The programs named in a command line: the first word of every pipeline
/// stage, with the next word as a possible subcommand.
pub fn programs(line: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for seg in crate::exec::split_top_level(line) {
        for stage in seg.split('|') {
            let mut words = stage
                .split_whitespace()
                .skip_while(|w| w.contains('=') && !w.starts_with(['/', '.']));
            let Some(cmd) = words.next().filter(|w| valid_name(w)) else {
                continue;
            };
            let sub = words.next().filter(|w| valid_name(w)).map(str::to_owned);
            out.push((cmd.to_owned(), sub));
        }
    }
    out
}

#[derive(Debug, Default)]
struct CacheInner {
    map: HashMap<String, Arc<Vec<Opt>>>,
    order: VecDeque<String>,
    in_flight: HashSet<String>,
}

struct FlightGuard {
    cache: ManCache,
    page: String,
}

impl FlightGuard {
    fn new(cache: ManCache, page: String) -> Self {
        Self { cache, page }
    }
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.cache.0.lock() {
            inner.in_flight.remove(&self.page);
        }
    }
}

/// Parsed options per man page, shared between the prompt and background
/// lookups.
#[derive(Debug, Clone, Default)]
pub struct ManCache(Arc<Mutex<CacheInner>>);

impl ManCache {
    /// The cached options of `page`, if it was looked up already.
    pub fn get(&self, page: &str) -> Option<Arc<Vec<Opt>>> {
        self.0.lock().ok()?.map.get(page).cloned()
    }

    /// Cached options for `cmd` (or its subcommand page `cmd-sub`) without
    /// spawning `man`.
    pub fn get_cached(&self, cmd: &str, sub: Option<&str>) -> Option<Arc<Vec<Opt>>> {
        if let Some(sub) = sub.filter(|s| valid_name(s))
            && let Some(opts) = self.get(&format!("{cmd}-{sub}"))
            && !opts.is_empty()
        {
            return Some(opts);
        }
        self.get(cmd)
    }

    /// Stores options for `page`, evicting the oldest entry if the cache is full.
    pub fn insert(&self, page: &str, opts: Vec<Opt>) -> Arc<Vec<Opt>> {
        let opts = Arc::new(opts);
        if let Ok(mut inner) = self.0.lock() {
            if !inner.map.contains_key(page) {
                while inner.map.len() >= MAX_CACHE_ENTRIES {
                    let Some(oldest) = inner.order.pop_front() else {
                        break;
                    };
                    inner.map.remove(&oldest);
                }
                inner.order.push_back(page.to_owned());
            }
            inner.map.insert(page.to_owned(), Arc::clone(&opts));
        }
        opts
    }

    fn page(&self, name: &str) -> Arc<Vec<Opt>> {
        self.get(name).unwrap_or_else(|| {
            let opts = fetch(name)
                .map(|t| parse_options(&clean(&t)))
                .unwrap_or_default();
            self.insert(name, opts)
        })
    }

    /// Options for `cmd`, or for its subcommand page (`cmd-sub`) when that
    /// exists. Reads `man` if the page is not cached yet (up to two seconds).
    pub fn load(&self, cmd: &str, sub: Option<&str>) -> Arc<Vec<Opt>> {
        if let Some(sub) = sub.filter(|s| valid_name(s)) {
            let opts = self.page(&format!("{cmd}-{sub}"));
            if !opts.is_empty() {
                return opts;
            }
        }
        self.page(cmd)
    }

    fn claim(&self, page: &str) -> bool {
        let Ok(mut inner) = self.0.lock() else {
            return false;
        };
        if inner.map.contains_key(page) || inner.in_flight.contains(page) {
            return false;
        }
        inner.in_flight.insert(page.to_owned());
        true
    }

    /// Looks up the programs of `line` in the background.
    pub fn prefetch(&self, line: &str) {
        for (cmd, sub) in programs(line) {
            let sub_page = sub
                .as_deref()
                .filter(|s| valid_name(s))
                .map(|s| format!("{cmd}-{s}"));
            if let Some(p) = &sub_page
                && self.get(p).is_some_and(|opts| !opts.is_empty())
            {
                continue;
            }
            let need_sub = sub_page.as_deref().is_some_and(|p| self.claim(p));
            let need_cmd = self.claim(&cmd);
            if !need_sub && !need_cmd {
                continue;
            }
            let cache = self.clone();
            tokio::task::spawn_blocking(move || {
                let _g_sub = sub_page
                    .as_deref()
                    .filter(|_| need_sub)
                    .map(|p| FlightGuard::new(cache.clone(), p.to_owned()));
                let _g_cmd = need_cmd.then(|| FlightGuard::new(cache.clone(), cmd.clone()));
                if need_sub
                    && let Some(p) = sub_page.as_deref()
                    && !cache.page(p).is_empty()
                {
                    return;
                }
                if need_cmd {
                    let _ = cache.page(&cmd);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GNU: &str = "\
TAR(1)

OPTIONS
       -c, --create
              create a new archive

       -x, --extract, --get
              extract files from an archive

       -f, --file=ARCHIVE
              use archive file or device ARCHIVE

       --color[=WHEN]  Colorize the output. More text here.

       -C DIR, --directory=DIR  change to directory DIR
";

    const BSD: &str = "\
     -a      Include hidden entries.
     -l      Long format, one entry per line.
     -h      Use unit suffixes.  Always combined with -l.
";

    #[test]
    fn parses_gnu_style_pages() {
        let o = parse_options(GNU);
        let flags: Vec<&str> = o.iter().map(|o| o.flag.as_str()).collect();
        assert_eq!(
            flags,
            [
                "-c",
                "--create",
                "-x",
                "--extract",
                "--get",
                "-f",
                "--file",
                "--color",
                "-C",
                "--directory"
            ]
        );
        let get = |f: &str| o.iter().find(|o| o.flag == f).map(|o| o.help.as_str());
        assert_eq!(get("--create"), Some("create a new archive"));
        assert_eq!(get("-f"), Some("use archive file or device ARCHIVE"));
        assert_eq!(get("--color"), Some("Colorize the output"));
        assert_eq!(get("-C"), Some("change to directory DIR"));
    }

    #[test]
    fn parses_bsd_style_pages() {
        let o = parse_options(BSD);
        assert_eq!(
            o,
            [
                Opt {
                    flag: "-a".into(),
                    help: "Include hidden entries".into()
                },
                Opt {
                    flag: "-l".into(),
                    help: "Long format, one entry per line".into()
                },
                Opt {
                    flag: "-h".into(),
                    help: "Use unit suffixes".into()
                },
            ]
        );
    }

    #[test]
    fn ignores_noise_and_duplicates() {
        let text = "  -- end of options\n  --\n  - alone\n  -v  verbose\n  -v  again\n  -?  help\n";
        let o = parse_options(text);
        assert_eq!(
            o,
            [Opt {
                flag: "-v".into(),
                help: "verbose".into()
            }]
        );
    }

    /// Found with the real `man ls`: a wrapped line that starts with flags is
    /// prose, not an option entry.
    #[test]
    fn wrapped_prose_does_not_pose_as_an_option() {
        let text = "     -H      Symbolic links are followed.  This is assumed if none of the\n             -F, -d, or -l options are specified.\n\n     -l      List files in the long format.\n";
        let o = parse_options(text);
        let help = |f: &str| o.iter().find(|o| o.flag == f).map(|o| o.help.as_str());
        assert_eq!(help("-l"), Some("List files in the long format"));
        assert_eq!(help("-H"), Some("Symbolic links are followed"));
        assert!(help("-F").is_none() && help("-d").is_none());
    }

    #[test]
    fn cleans_overstrikes_and_escapes() {
        assert_eq!(
            clean("N\u{8}NA\u{8}AM\u{8}ME\u{8}E \u{1b}[1mbold\u{1b}[0m\r\n"),
            "NAME bold\n"
        );
        assert_eq!(
            clean("\u{1b}]0;title\u{7}ok\u{1b}]8;;https://x\u{1b}\\end"),
            "okend"
        );
    }

    #[test]
    fn odd_man_output_does_not_panic() {
        assert_eq!(clean("\u{8}\u{8}a\u{1b}"), "a");
        assert_eq!(clean("é\u{8}éx\u{1b}["), "éx");
        let text = "\u{a0}\u{a0}-é  accent\n\u{2003}-a\u{3000}\u{3000}ideographic\n  --日本\n  -\n  -x\t\ttab ü\n";
        let o = parse_options(text);
        assert!(
            o.iter().any(|o| o.flag == "-x" && o.help == "tab ü"),
            "{o:?}"
        );
        let long = format!("  -v  {}\n", "é".repeat(5_000));
        let o = parse_options(&long);
        assert_eq!(o[0].help.chars().count(), MAX_HELP);
    }

    #[test]
    fn page_names_are_validated() {
        for ok in ["ls", "git-commit", "g++", "python3.12"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-k", "a b", "../x", "a;b", "$(x)", "a/b"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(fetch("../../etc/passwd").is_none());
    }

    #[test]
    fn finds_programs_in_a_line() {
        assert_eq!(
            programs("FOO=1 git commit -m x | grep a; ls"),
            [
                ("git".to_owned(), Some("commit".to_owned())),
                ("grep".to_owned(), Some("a".to_owned())),
                ("ls".to_owned(), None),
            ]
        );
        assert!(programs("./run.sh").is_empty(), "paths are not man pages");
    }

    /// Runs against the real `man` of this machine. It does nothing where
    /// there is no man page for `ls`.
    #[test]
    fn reads_a_real_man_page() {
        let Some(text) = fetch("ls") else {
            eprintln!("skipped: no man page for ls on this machine");
            return;
        };
        let opts = parse_options(&clean(&text));
        let flags: Vec<&str> = opts.iter().map(|o| o.flag.as_str()).collect();
        assert!(flags.contains(&"-l") && flags.contains(&"-a"), "{flags:?}");
        assert!(opts.iter().any(|o| o.flag == "-l" && !o.help.is_empty()));
        assert!(flags.iter().all(|f| f.starts_with('-')), "{flags:?}");
    }

    #[test]
    fn cache_prefers_the_subcommand_page() {
        let c = ManCache::default();
        c.insert(
            "git",
            vec![Opt {
                flag: "--version".into(),
                help: String::new(),
            }],
        );
        c.insert(
            "git-commit",
            vec![Opt {
                flag: "--amend".into(),
                help: String::new(),
            }],
        );
        assert_eq!(c.load("git", Some("commit"))[0].flag, "--amend");
        assert_eq!(c.load("git", None)[0].flag, "--version");
        // An unknown subcommand page is cached as empty; the main page is used.
        c.insert("git-nosuch", Vec::new());
        assert_eq!(c.load("git", Some("nosuch"))[0].flag, "--version");
    }

    #[test]
    fn cache_evicts_past_limit() {
        let c = ManCache::default();
        for i in 0..=MAX_CACHE_ENTRIES + 5 {
            c.insert(&format!("p{i}"), Vec::new());
        }
        assert!(c.get("p0").is_none());
        assert!(c.get(&format!("p{}", MAX_CACHE_ENTRIES + 5)).is_some());
    }
}
