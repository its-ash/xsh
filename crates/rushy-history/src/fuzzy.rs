//! Fuzzy ranking shared by `Ctrl-R`, `xsh history search`, and the palette.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Ranks `items` against `query`, best match first; ties keep input order.
///
/// Returns indices into `items`. An empty query matches everything in order.
///
/// ```
/// let items = ["git status", "cargo test", "git stash"];
/// assert_eq!(rushy_history::fuzzy::rank("gst", &items).first(), Some(&0));
/// ```
#[must_use]
pub fn rank<T: AsRef<str>>(query: &str, items: &[T]) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..items.len()).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            pattern
                .score(Utf32Str::new(s.as_ref(), &mut buf), &mut matcher)
                .map(|score| (i, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().map(|(i, _)| i).collect()
}
