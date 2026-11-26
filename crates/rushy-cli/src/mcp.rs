//! `xsh mcp`: the MCP server that `ai claude` gives to Claude Code.
//!
//! Claude Code runs with all of its own tools turned off, so these tools are
//! the only way it can act. This process does nothing by itself: it speaks MCP
//! (JSON-RPC, one message per line) on standard input and output, and passes
//! each tool call over a private socket to the xsh session that started
//! Claude Code. That session runs the safety guard, asks for approval, and
//! runs the command in the user's shell.
//!
//! The file tools below are plain functions so that the session can call
//! them. They only touch paths inside the working directory of the run.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader,
};
use tokio::net::UnixStream;

/// The server name, so Claude Code calls the tools `mcp__xsh__<tool>`.
pub const SERVER: &str = "xsh";
/// Environment variable carrying the per-session socket bearer token.
pub const TOKEN_VAR: &str = "XSH_MCP_TOKEN";
/// Maximum bytes accepted for a single newline-delimited JSON line (10 MiB).
pub const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;
/// MCP protocol version used when the client does not name one.
const PROTOCOL: &str = "2025-06-18";
/// Lines `read_file` returns when no `limit` is given.
const READ_LINES: usize = 2_000;
/// Bytes of text a tool returns at most.
const REPLY_LIMIT: usize = 100_000;
/// Files larger than this are not read or searched.
const FILE_LIMIT: u64 = 2 * 1024 * 1024;
/// Hits of `find_files` and `search_text`.
const MAX_HITS: usize = 200;
/// Entries a directory walk visits at most.
const MAX_WALK: usize = 50_000;
/// Directories that a walk skips: build output and version control data.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".build",
    ".venv",
    "__pycache__",
];

/// The tools, as MCP `tools/list` describes them.
pub fn tools() -> Value {
    let path = json!({ "type": "string", "description": "Path relative to the working directory, or absolute inside it." });
    let reason =
        json!({ "type": "string", "description": "One sentence for the user: why this step." });
    json!([
        {
            "name": "run_command",
            "description": "Run a shell command in the user's xsh shell, in the working directory, and get its exit status, standard output, and standard error. The xsh safety guard checks every command and the user may decline it. The shell keeps its state between calls (cd, variables). Use non-interactive commands only: there is no terminal input.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line." },
                    "reason": reason,
                },
                "required": ["command"],
            },
        },
        {
            "name": "read_file",
            "description": "Read a text file inside the working directory. Lines are numbered from 1. Without `limit`, at most 2000 lines are returned.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "offset": { "type": "integer", "minimum": 1, "description": "First line (default 1)." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Number of lines." },
                },
                "required": ["path"],
            },
        },
        {
            "name": "write_file",
            "description": "Create a file, or replace all of its text, inside the working directory. Parent directories are created. The user may decline it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "content": { "type": "string", "description": "The whole new text." },
                    "reason": reason,
                },
                "required": ["path", "content"],
            },
        },
        {
            "name": "edit_file",
            "description": "Replace exact text in a file inside the working directory. `old_text` must match exactly once unless `replace_all` is true. The user may decline it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": path,
                    "old_text": { "type": "string", "description": "Exact text to replace, with its whitespace." },
                    "new_text": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every match." },
                    "reason": reason,
                },
                "required": ["path", "old_text", "new_text"],
            },
        },
        {
            "name": "list_dir",
            "description": "List a directory inside the working directory. Directories end with `/`.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": path },
            },
        },
        {
            "name": "find_files",
            "description": "Find files whose path matches a glob, such as `**/*.rs` or `src/*.toml`. Skips .git, target, node_modules, and similar directories.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob, relative to `path`." },
                    "path": path,
                },
                "required": ["pattern"],
            },
        },
        {
            "name": "search_text",
            "description": "Search file contents for a plain text string (not a regular expression). Returns `path:line: text` for each match.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Text to look for." },
                    "path": path,
                    "glob": { "type": "string", "description": "Only search files whose path matches this glob, such as `**/*.swift`." },
                    "ignore_case": { "type": "boolean", "description": "Ignore upper and lower case." },
                },
                "required": ["query"],
            },
        },
        {
            "name": "shell_info",
            "description": "The state of the user's xsh session: working directory, last exit status, the last failed command with its error text, recent commands, and active bundles.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "check_command",
            "description": "Ask the xsh safety guard whether a command would be allowed, without running it.",
            "inputSchema": {
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"],
            },
        },
    ])
}

/// What the MCP server tells Claude Code about itself.
const INSTRUCTIONS: &str = "These tools act in the user's xsh shell and working directory. \
They are the only tools available. Read before you edit, prefer edit_file over write_file for \
small changes, and explain each command in `reason`. A declined or refused step is final: do \
not try to get around it with another tool.";

fn rpc_result(id: &Value, result: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A `tools/call` result with one text block.
pub fn tool_text(text: &str, error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": error })
}

/// Answers one MCP message. `None` for a notification, which has no reply.
/// `call` passes a tool call to the session.
pub async fn handle<F, Fut>(msg: &Value, call: F) -> Option<Value>
where
    F: FnOnce(String, Value) -> Fut,
    Fut: Future<Output = Value>,
{
    let id = msg.get("id")?;
    let Some(method) = msg["method"].as_str() else {
        // A reply from the client to a request this server never sends.
        return None;
    };
    Some(match method {
        "initialize" => rpc_result(
            id,
            &json!({
                "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            }),
        ),
        "ping" => rpc_result(id, &json!({})),
        "tools/list" => rpc_result(id, &json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let args = msg["params"]["arguments"].clone();
            rpc_result(id, &call(name, args).await)
        }
        other => rpc_error(id, -32601, &format!("unknown method `{other}`")),
    })
}

/// Constant-time string comparison for bearer tokens.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Reads a single `\n`-terminated line up to `limit` bytes (excluding `\n`).
/// Returns `Ok(None)` at EOF, or `InvalidData` if the line exceeds `limit`
/// bytes or is not valid UTF-8.
pub async fn read_line_capped<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: usize,
) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let cap = u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX);
    let n = reader.take(cap).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(None);
    }
    if buf.len() > limit && !buf.ends_with(b"\n") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line exceeds maximum length",
        ));
    }
    if buf.ends_with(b"\n") {
        buf.pop();
        if buf.ends_with(b"\r") {
            buf.pop();
        }
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// `xsh mcp --socket PATH`: serves MCP on standard input and output.
pub async fn serve(socket: &Path) -> Result<i32> {
    let token = std::env::var(TOKEN_VAR)
        .ok()
        .filter(|t| !t.is_empty())
        .context("missing session authentication token")?;
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("no xsh session at {}", socket.display()))?;
    let (read, mut session) = stream.into_split();
    let auth = json!({ "auth": token });
    session
        .write_all(format!("{auth}\n").as_bytes())
        .await
        .context("cannot authenticate with the xsh session")?;
    session.flush().await?;
    let mut replies = BufReader::new(read);
    let mut input = BufReader::new(tokio::io::stdin());
    let mut out = tokio::io::stdout();
    while let Some(line) = read_line_capped(&mut input, MAX_LINE_BYTES).await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => {
                let (session, replies) = (&mut session, &mut replies);
                handle(&msg, |name, args| async move {
                    let req = json!({ "tool": name, "args": args });
                    if session
                        .write_all(format!("{req}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return tool_text("the xsh session has ended", true);
                    }
                    match read_line_capped(replies, MAX_LINE_BYTES).await {
                        Ok(Some(l)) => serde_json::from_str(&l)
                            .unwrap_or_else(|_| tool_text("bad reply from the xsh session", true)),
                        _ => tool_text("the xsh session has ended", true),
                    }
                })
                .await
            }
            Err(e) => Some(rpc_error(
                &Value::Null,
                -32700,
                &format!("parse error: {e}"),
            )),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(0)
}

// ------------------------------------------------------------- file tools

/// `path` as an absolute path inside `root` (which must be canonical).
/// Symbolic links are followed, so a link cannot lead outside. The file
/// itself need not exist.
pub fn resolve(root: &Path, path: &str) -> Result<PathBuf> {
    let p = Path::new(path.trim());
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    // The nearest part that exists is made canonical; the rest is checked.
    let mut existing = joined.as_path();
    let mut rest = Vec::new();
    let base = loop {
        if let Ok(c) = existing.canonicalize() {
            break c;
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            bail!("`{path}` does not exist");
        };
        rest.push(name.to_owned());
        existing = parent;
    };
    let mut full = base;
    for part in rest.iter().rev() {
        if Path::new(part)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            bail!("`{path}`: `..` is not allowed in a new path");
        }
        full.push(part);
    }
    if !full.starts_with(root) {
        bail!(
            "`{path}` is outside the working directory {}; use run_command to work there",
            root.display()
        );
    }
    Ok(full)
}

fn shown(root: &Path, p: &Path) -> String {
    let rel = p.strip_prefix(root).unwrap_or(p);
    if rel.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        rel.display().to_string()
    }
}

fn cap(mut text: String) -> String {
    if text.len() > REPLY_LIMIT {
        let mut end = REPLY_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… (cut: the reply was too long)");
    }
    text
}

fn read_text(p: &Path) -> Result<String> {
    let meta = std::fs::metadata(p)?;
    if meta.is_dir() {
        bail!("it is a directory; use list_dir");
    }
    if meta.len() > FILE_LIMIT {
        bail!("the file is larger than {} MB", FILE_LIMIT / 1024 / 1024);
    }
    let bytes = std::fs::read(p)?;
    if bytes.contains(&0) {
        bail!("it is a binary file");
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    args[key].as_u64().and_then(|n| usize::try_from(n).ok())
}

/// `read_file`.
pub fn read_file(root: &Path, args: &Value) -> Result<String> {
    let path = args["path"].as_str().context("`path` is required")?;
    let text = read_text(&resolve(root, path)?)?;
    let start = arg_usize(args, "offset").unwrap_or(1).max(1);
    let limit = arg_usize(args, "limit").unwrap_or(READ_LINES);
    let total = text.lines().count();
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(start - 1).take(limit) {
        let _ = writeln!(out, "{:>6}\t{line}", i + 1);
    }
    let end = (start - 1 + limit).min(total);
    if end < total {
        let _ = writeln!(
            out,
            "… lines {start}-{end} of {total}; use offset to read on"
        );
    }
    if total == 0 {
        out.push_str("(empty file)");
    }
    Ok(cap(out))
}

/// The target of a write: its path, and a short text for the approval card.
pub fn describe_write(root: &Path, tool: &str, args: &Value) -> Result<(PathBuf, String)> {
    let path = args["path"].as_str().context("`path` is required")?;
    let full = resolve(root, path)?;
    let what = match tool {
        "write_file" => {
            let content = args["content"].as_str().context("`content` is required")?;
            let verb = if full.exists() { "overwrite" } else { "create" };
            format!(
                "{verb} {} ({} lines)",
                shown(root, &full),
                content.lines().count()
            )
        }
        _ => format!("edit {}", shown(root, &full)),
    };
    Ok((full, what))
}

/// Returns `true` for sensitive files (`.env`, `.env.*`, `*.pem`, `*.key`,
/// `id_rsa`, `id_ed25519`, `.git/config`) that must not be auto-read.
pub fn is_secret_path(root: &Path, path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name == ".env"
        || name.starts_with(".env.")
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pem") || ext.eq_ignore_ascii_case("key"))
        || name == "id_rsa"
        || name == "id_ed25519"
    {
        return true;
    }
    let rel = path.strip_prefix(root).unwrap_or(path);
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    parts.windows(2).any(|w| w[0] == ".git" && w[1] == "config")
}

/// Re-resolves `full` inside `root` right before I/O and rejects symlinks
/// (including dangling ones) that could escape `root`.
fn verify_write_target(root: &Path, full: &Path) -> Result<PathBuf> {
    let resolved = resolve(root, &full.to_string_lossy())?;
    if let Ok(meta) = std::fs::symlink_metadata(&resolved)
        && meta.file_type().is_symlink()
    {
        let canon = resolved.canonicalize().with_context(|| {
            format!(
                "refusing to write through symlink {}",
                shown(root, &resolved)
            )
        })?;
        if !canon.starts_with(root) {
            bail!(
                "`{}` is outside the working directory {}",
                shown(root, &resolved),
                root.display()
            );
        }
        return Ok(canon);
    }
    Ok(resolved)
}

/// `write_file`, after approval.
pub fn write_file(root: &Path, full: &Path, args: &Value) -> Result<String> {
    let content = args["content"].as_str().context("`content` is required")?;
    let target = verify_write_target(root, full)?;
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
        let canon_dir = dir.canonicalize()?;
        if !canon_dir.starts_with(root) {
            bail!(
                "`{}` is outside the working directory {}",
                shown(root, &target),
                root.display()
            );
        }
    }
    let target = verify_write_target(root, &target)?;
    std::fs::write(&target, content)?;
    Ok(format!(
        "wrote {} ({} bytes)",
        shown(root, &target),
        content.len()
    ))
}

/// `edit_file`, after approval.
pub fn edit_file(root: &Path, full: &Path, args: &Value) -> Result<String> {
    let old = args["old_text"]
        .as_str()
        .context("`old_text` is required")?;
    let new = args["new_text"]
        .as_str()
        .context("`new_text` is required")?;
    if old.is_empty() {
        bail!("`old_text` is empty; use write_file to create a file");
    }
    let target = verify_write_target(root, full)?;
    let text = read_text(&target)?;
    let n = text.matches(old).count();
    let all = args["replace_all"].as_bool().unwrap_or(false);
    match (n, all) {
        (0, _) => bail!("`old_text` was not found; read the file again and copy the text exactly"),
        (1, _) | (_, true) => {}
        (n, false) => {
            bail!("`old_text` matches {n} times; add more lines around it, or set replace_all")
        }
    }
    let target = verify_write_target(root, &target)?;
    std::fs::write(&target, text.replace(old, new))?;
    Ok(format!(
        "edited {} ({n} replacement(s))",
        shown(root, &target)
    ))
}

/// `list_dir`.
pub fn list_dir(root: &Path, args: &Value) -> Result<String> {
    let dir = resolve(root, args["path"].as_str().unwrap_or("."))?;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .with_context(|| format!("cannot list {}", shown(root, &dir)))?
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return Ok("(empty directory)".to_owned());
    }
    Ok(cap(names.join("\n")))
}

/// Files under `dir`, with their paths relative to `dir`, sorted.
fn walk(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let mut seen = 0;
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            seen += 1;
            if seen > MAX_WALK {
                return out;
            }
            let p = e.path();
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref()) {
                    stack.push(p);
                }
            } else if ft.is_file() {
                let rel = p
                    .strip_prefix(dir)
                    .map_or_else(|_| p.display().to_string(), |r| r.display().to_string());
                out.push((p, rel));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn glob(pattern: &str) -> Result<globset::GlobMatcher> {
    Ok(globset::GlobBuilder::new(pattern)
        .literal_separator(false)
        .build()
        .with_context(|| format!("bad glob `{pattern}`"))?
        .compile_matcher())
}

/// `find_files`.
pub fn find_files(root: &Path, args: &Value) -> Result<String> {
    let pattern = args["pattern"].as_str().context("`pattern` is required")?;
    let dir = resolve(root, args["path"].as_str().unwrap_or("."))?;
    let m = glob(pattern)?;
    let hits: Vec<String> = walk(&dir)
        .into_iter()
        .filter(|(_, rel)| m.is_match(rel))
        .map(|(p, _)| shown(root, &p))
        .take(MAX_HITS + 1)
        .collect();
    if hits.is_empty() {
        return Ok(format!("no files match `{pattern}`"));
    }
    let more = hits.len() > MAX_HITS;
    let mut text = hits[..hits.len().min(MAX_HITS)].join("\n");
    if more {
        let _ = write!(text, "\n… more than {MAX_HITS} files; narrow the pattern");
    }
    Ok(text)
}

/// `search_text`.
pub fn search_text(root: &Path, args: &Value) -> Result<String> {
    let query = args["query"].as_str().context("`query` is required")?;
    if query.is_empty() {
        bail!("`query` is empty");
    }
    let dir = resolve(root, args["path"].as_str().unwrap_or("."))?;
    let only = args["glob"].as_str().map(glob).transpose()?;
    let nocase = args["ignore_case"].as_bool().unwrap_or(false);
    let needle = if nocase {
        query.to_lowercase()
    } else {
        query.to_owned()
    };
    // A file path searches that one file.
    let files = if dir.is_file() {
        vec![(dir.clone(), shown(root, &dir))]
    } else {
        walk(&dir)
            .into_iter()
            .filter(|(p, _)| !is_secret_path(root, p))
            .collect()
    };
    let mut out = String::new();
    let mut hits = 0;
    'files: for (p, rel) in files {
        if only.as_ref().is_some_and(|g| !g.is_match(&rel)) {
            continue;
        }
        let Ok(text) = read_text(&p) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let found = if nocase {
                line.to_lowercase().contains(&needle)
            } else {
                line.contains(&needle)
            };
            if found {
                hits += 1;
                if hits > MAX_HITS {
                    let _ = write!(out, "… more than {MAX_HITS} matches; narrow the search");
                    break 'files;
                }
                let line: String = line.chars().take(300).collect();
                let _ = writeln!(out, "{}:{}: {}", shown(root, &p), i + 1, line.trim_end());
            }
        }
    }
    if hits == 0 {
        return Ok(format!("no matches for `{query}`"));
    }
    Ok(cap(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn root() -> std::result::Result<(tempfile::TempDir, PathBuf), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        std::fs::create_dir_all(root.join("src"))?;
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    hello();\n}\n")?;
        std::fs::write(root.join("README.md"), "Hello world\n")?;
        Ok((dir, root))
    }

    #[test]
    fn paths_stay_inside_the_root() -> TestResult {
        let (_d, root) = root()?;
        assert!(resolve(&root, "src/main.rs").is_ok());
        assert!(resolve(&root, "new/dir/file.txt").is_ok());
        assert!(resolve(&root, "../x").is_err());
        assert!(resolve(&root, "src/../../x").is_err());
        assert!(resolve(&root, "/etc/hosts").is_err());
        let link = root.join("out");
        std::os::unix::fs::symlink("/tmp", &link)?;
        assert!(resolve(&root, "out/x").is_err());
        Ok(())
    }

    #[test]
    fn file_tools_read_find_search_and_edit() -> TestResult {
        let (_d, root) = root()?;
        let text = read_file(
            &root,
            &json!({ "path": "src/main.rs", "offset": 2, "limit": 1 }),
        )?;
        assert!(text.contains("     2\t    hello();"), "{text}");
        assert!(text.contains("lines 2-2 of 3"), "{text}");
        assert_eq!(
            find_files(&root, &json!({ "pattern": "**/*.rs" }))?,
            "src/main.rs"
        );
        let hits = search_text(&root, &json!({ "query": "hello", "ignore_case": true }))?;
        assert!(hits.contains("README.md:1: Hello world"), "{hits}");
        assert!(hits.contains("src/main.rs:2:"), "{hits}");
        let listing = list_dir(&root, &json!({}))?;
        assert_eq!(listing, "README.md\nsrc/");

        let args = json!({ "path": "src/main.rs", "old_text": "hello", "new_text": "bye" });
        let (full, what) = describe_write(&root, "edit_file", &args)?;
        assert_eq!(what, "edit src/main.rs");
        edit_file(&root, &full, &args)?;
        assert!(std::fs::read_to_string(&full)?.contains("bye();"));
        assert!(edit_file(&root, &full, &args).is_err());

        let args = json!({ "path": "docs/new.md", "content": "a\nb\n" });
        let (full, what) = describe_write(&root, "write_file", &args)?;
        assert_eq!(what, "create docs/new.md (2 lines)");
        write_file(&root, &full, &args)?;
        assert_eq!(std::fs::read_to_string(root.join("docs/new.md"))?, "a\nb\n");
        Ok(())
    }

    #[tokio::test]
    async fn speaks_mcp() -> TestResult {
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-03-26" } });
        let r = handle(&init, |_, _| async { Value::Null }).await;
        let r = r.ok_or("no reply")?;
        assert_eq!(r["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r["result"]["serverInfo"]["name"], SERVER);

        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle(&note, |_, _| async { Value::Null }).await.is_none());

        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let r = handle(&list, |_, _| async { Value::Null })
            .await
            .ok_or("no reply")?;
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .ok_or("no tools")?
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"run_command") && names.contains(&"edit_file"));

        let call = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "list_dir", "arguments": { "path": "." } } });
        let r = handle(&call, |name, args| async move {
            tool_text(&format!("{name} {}", args["path"]), false)
        })
        .await
        .ok_or("no reply")?;
        assert_eq!(r["result"]["content"][0]["text"], "list_dir \".\"");

        let bad = json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list" });
        let r = handle(&bad, |_, _| async { Value::Null })
            .await
            .ok_or("no reply")?;
        assert_eq!(r["error"]["code"], -32601);
        Ok(())
    }

    #[test]
    fn detects_secret_paths() {
        let root = Path::new("/proj");
        for secret in [
            "/proj/.env",
            "/proj/.env.production",
            "/proj/certs/server.pem",
            "/proj/private.key",
            "/proj/id_rsa",
            "/proj/id_ed25519",
            "/proj/.git/config",
        ] {
            assert!(is_secret_path(root, Path::new(secret)), "{secret}");
        }
        for ok in ["/proj/src/main.rs", "/proj/README.md", "/proj/config.toml"] {
            assert!(!is_secret_path(root, Path::new(ok)), "{ok}");
        }
    }

    #[tokio::test]
    async fn read_line_capped_enforces_limit() -> TestResult {
        let mut ok_reader = BufReader::new(&b"hello\nworld\n"[..]);
        assert_eq!(
            read_line_capped(&mut ok_reader, 16).await?.as_deref(),
            Some("hello")
        );
        assert_eq!(
            read_line_capped(&mut ok_reader, 16).await?.as_deref(),
            Some("world")
        );
        assert_eq!(read_line_capped(&mut ok_reader, 16).await?, None);

        let mut big_reader = BufReader::new(&b"0123456789abcdef\n"[..]);
        assert!(read_line_capped(&mut big_reader, 8).await.is_err());
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        Ok(())
    }
}
