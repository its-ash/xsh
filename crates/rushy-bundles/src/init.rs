//! Static `init.rushy` files: `alias` and `export` lines only, never executed.

use std::collections::BTreeMap;

use rushy_core::parser::{self, WordPart};

use crate::{Error, Result};

/// Aliases and variables declared by an `init.rushy` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitFile {
    /// `alias name='value'` lines.
    pub aliases: BTreeMap<String, String>,
    /// `export NAME=value` lines.
    pub env: BTreeMap<String, String>,
}

/// Parses `init.rushy` with the shell's own quoting rules, rejecting anything
/// that is not a literal `alias` or `export`. Nothing is executed and no
/// variable is expanded.
///
/// ```
/// let f = rushy_bundles::parse_init("init.rushy", "# git\nalias gco='git checkout'\nexport GIT_PAGER=less\n");
/// assert_eq!(f.map(|f| f.aliases["gco"].clone()).ok().as_deref(), Some("git checkout"));
/// assert!(rushy_bundles::parse_init("init.rushy", "rm -rf ~").is_err());
/// ```
///
/// # Errors
///
/// Returns [`Error::Init`] with the offending line.
pub fn parse_init(file: &str, text: &str) -> Result<InitFile> {
    let mut out = InitFile::default();
    for (i, line) in text.lines().enumerate() {
        let fail = |reason: &str| Error::Init {
            file: file.to_owned(),
            line: i + 1,
            reason: reason.to_owned(),
        };
        let pipelines = parser::parse(line, &BTreeMap::new()).map_err(|e| fail(&e.to_string()))?;
        for p in pipelines {
            let [cmd] = p.commands.as_slice() else {
                return Err(fail("pipelines are not allowed"));
            };
            if p.background || !cmd.redirects.is_empty() {
                return Err(fail("`&` and redirections are not allowed"));
            }
            let mut words = Vec::with_capacity(cmd.words.len());
            for w in &cmd.words {
                let mut s = String::new();
                for part in &w.parts {
                    match part {
                        WordPart::Literal(t) => s.push_str(t),
                        WordPart::Var(_) | WordPart::Tilde => {
                            return Err(fail("variables and `~` are not expanded in init.rushy"));
                        }
                    }
                }
                words.push(s);
            }
            let Some((head, args)) = words.split_first() else {
                continue;
            };
            let target = match head.as_str() {
                "alias" => &mut out.aliases,
                "export" => &mut out.env,
                other => {
                    return Err(fail(&format!(
                        "only `alias` and `export` are allowed, found `{other}`"
                    )));
                }
            };
            for a in args {
                let Some((k, v)) = a.split_once('=') else {
                    return Err(fail(&format!("expected NAME=value, found `{a}`")));
                };
                if head == "export" && !parser::is_var_name(k) {
                    return Err(fail(&format!("`{k}` is not a valid variable name")));
                }
                target.insert(k.to_owned(), v.to_owned());
            }
        }
    }
    Ok(out)
}
