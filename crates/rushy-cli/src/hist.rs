//! Line rewrites before a command runs: `!!`, `!$`, `!N`, `!prefix`,
//! `^old^new` history expansion, plus "did you mean" typo suggestions.

/// Last word of `cmd`, quotes kept.
fn last_word(cmd: &str) -> &str {
    cmd.split_whitespace().last().unwrap_or_default()
}

/// Expands history references in `line`. `recent` is oldest first.
/// Returns `None` when nothing changed, and `Err` for an event that does not exist.
#[allow(clippy::many_single_char_names, reason = "cursor indices i and j")]
pub fn expand(line: &str, recent: &[String]) -> Result<Option<String>, String> {
    let last = recent.last().map(String::as_str);
    if let Some(rest) = line.strip_prefix('^') {
        let mut it = rest.splitn(3, '^');
        let (Some(old), Some(new)) = (it.next(), it.next()) else {
            return Ok(None);
        };
        let prev = last.ok_or("^: no previous command")?;
        if old.is_empty() || !prev.contains(old) {
            return Err(format!("^{old}^: substitution failed"));
        }
        return Ok(Some(format!(
            "{}{}",
            prev.replacen(old, new, 1),
            it.next().unwrap_or_default()
        )));
    }
    if !line.contains('!') {
        return Ok(None);
    }
    let mut out = String::with_capacity(line.len());
    let mut changed = false;
    let (mut single, mut double, mut esc) = (false, false, false);
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if esc {
            esc = false;
            out.push(c);
            i += 1;
            continue;
        }
        match c {
            '\\' if !single => esc = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '!' if !single => {
                let next = chars.get(i + 1).copied();
                let event = match next {
                    Some('!') => {
                        i += 2;
                        Some(last.ok_or("!!: event not found")?.to_owned())
                    }
                    Some('$') => {
                        i += 2;
                        Some(last_word(last.ok_or("!$: event not found")?).to_owned())
                    }
                    Some(d) if d.is_ascii_digit() || d == '-' => {
                        let start = i + 1;
                        let mut j = start + 1;
                        while chars.get(j).is_some_and(char::is_ascii_digit) {
                            j += 1;
                        }
                        let text: String = chars[start..j].iter().collect();
                        let n: i64 = text
                            .parse()
                            .map_err(|_| format!("!{text}: event not found"))?;
                        let len = i64::try_from(recent.len()).unwrap_or(i64::MAX);
                        let idx = if n < 0 { len + n } else { n - 1 };
                        let hit = usize::try_from(idx)
                            .ok()
                            .and_then(|k| recent.get(k))
                            .ok_or(format!("!{text}: event not found"))?;
                        i = j;
                        Some(hit.clone())
                    }
                    Some(a) if a.is_alphanumeric() || a == '_' || a == '/' || a == '.' => {
                        let start = i + 1;
                        let mut j = start;
                        while chars
                            .get(j)
                            .is_some_and(|c| !c.is_whitespace() && !"|&;<>()".contains(*c))
                        {
                            j += 1;
                        }
                        let prefix: String = chars[start..j].iter().collect();
                        let hit = recent
                            .iter()
                            .rev()
                            .find(|c| c.starts_with(&prefix))
                            .ok_or(format!("!{prefix}: event not found"))?;
                        i = j;
                        Some(hit.clone())
                    }
                    _ => None,
                };
                if let Some(e) = event {
                    out.push_str(&e);
                    changed = true;
                    continue;
                }
            }
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    Ok(changed.then_some(out))
}

/// Edit distance between `a` and `b`; swapping two neighbours counts as one edit.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// Up to three known commands close to `name`, best first.
pub fn similar<'a>(name: &str, known: impl Iterator<Item = &'a String>) -> Vec<String> {
    let max = (name.chars().count() / 3).clamp(1, 2);
    let mut hits: Vec<(usize, &String)> = known
        .filter(|k| k.as_str() != name)
        .map(|k| (distance(name, k), k))
        .filter(|(d, _)| *d <= max)
        .collect();
    hits.sort();
    hits.dedup_by(|a, b| a.1 == b.1);
    hits.into_iter().take(3).map(|(_, k)| k.clone()).collect()
}

/// Reserved words that begin compound commands. A line that uses one is
/// given to the core whole, so `if`/`for`/functions are not split at `;`.
const COMPOUND: &[&str] = &[
    "if", "for", "while", "until", "case", "select", "function", "{", "(", "[[",
];

/// Whether `line` contains a compound command or a function definition.
pub fn is_compound(line: &str) -> bool {
    let mut cmd_pos = true;
    for w in line.split(|c: char| c.is_whitespace()) {
        if w.is_empty() {
            continue;
        }
        let head = w.trim_start_matches(['(', '{']);
        if cmd_pos && (COMPOUND.contains(&w) || w.starts_with(['(', '{']) || w.ends_with("()")) {
            return true;
        }
        if head.contains("()") {
            return true;
        }
        cmd_pos = w.ends_with([';', '|', '&']) || matches!(w, ";" | "|" | "&&" | "||" | "&");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h() -> Vec<String> {
        ["ls -la /tmp", "git status", "echo hi there"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn bang_forms() {
        assert_eq!(
            expand("sudo !!", &h()),
            Ok(Some("sudo echo hi there".into()))
        );
        assert_eq!(expand("cd !$", &h()), Ok(Some("cd there".into())));
        assert_eq!(expand("!1", &h()), Ok(Some("ls -la /tmp".into())));
        assert_eq!(expand("!-2", &h()), Ok(Some("git status".into())));
        assert_eq!(expand("!git", &h()), Ok(Some("git status".into())));
        assert_eq!(expand("echo '!!'", &h()), Ok(None));
        assert_eq!(expand("echo a != b", &h()), Ok(None));
        assert!(expand("!nope", &h()).is_err());
        assert!(expand("!!", &[]).is_err());
    }

    #[test]
    fn caret_substitution() {
        assert_eq!(expand("^hi^bye", &h()), Ok(Some("echo bye there".into())));
        assert!(expand("^zz^y", &h()).is_err());
    }

    #[test]
    fn typo_suggestions() {
        let known = ["git", "grep", "gzip", "cargo"].map(String::from);
        assert_eq!(similar("gti", known.iter()), ["git"]);
        assert_eq!(similar("crago", known.iter()), ["cargo"]);
        assert!(similar("zzzzzz", known.iter()).is_empty());
    }

    #[test]
    fn compound_lines() {
        assert!(is_compound("if true; then echo; fi"));
        assert!(is_compound("for x in a b; do echo $x; done"));
        assert!(is_compound("f() { echo; }"));
        assert!(is_compound("(cd /tmp; ls)"));
        assert!(!is_compound("echo if; bundle list"));
        assert!(!is_compound("ls -la | grep x"));
    }
}
