//! Built-in filesystem tools, sandboxed to the working directory.

use std::path::{Path, PathBuf};

use serde_json::Value;

pub const LIST: &str = "ducky__fs_list";
pub const READ: &str = "ducky__fs_read";
pub const SEARCH: &str = "ducky__fs_search";
pub const WRITE: &str = "ducky__fs_write";
pub const MKDIR: &str = "ducky__fs_mkdir";

const MAX_READ_BYTES: usize = 200 * 1024;
const MAX_SEARCH_RESULTS: usize = 100;
const MAX_SEARCH_DEPTH: usize = 8;
/// Backstop for the one shared walk: entries collected before it stops.
const MAX_WALK_ENTRIES: usize = 1_000;
/// Directories that are never worth searching (huge, generated, or noise).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "venv",
    ".venv",
    "__pycache__",
    ".git",
    ".hg",
    ".svn",
    ".next",
    "dist",
    "build",
    ".cache",
];

pub async fn execute(tool: &str, args: &Value, cwd: &Path) -> Result<String, String> {
    match tool {
        LIST => list(args, cwd),
        READ => read(args, cwd),
        SEARCH => search(args, cwd),
        WRITE => write(args, cwd),
        MKDIR => mkdir(args, cwd),
        _ => Err(format!("Unknown builtin filesystem tool: {tool}")),
    }
}

/// Resolve `path` (relative to `cwd`) and guarantee the result stays inside
/// `cwd`. Canonicalizes what exists so symlink escapes and `..` traversal are
/// caught; for not-yet-existing targets the parent is canonicalized.
fn resolve_contained(cwd: &Path, path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    };

    let canonical_target = std::fs::canonicalize(&joined).unwrap_or_else(|_| {
        // target doesn't exist yet (write/mkdir): canonicalize the parent
        match joined.parent() {
            Some(parent) if parent.exists() => {
                let base = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
                base.join(joined.file_name().unwrap_or_default())
            }
            _ => joined.clone(),
        }
    });
    let canonical_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    if !canonical_target.starts_with(&canonical_cwd) {
        return Err(format!(
            "Path \"{path}\" resolves outside the working directory ({}). Built-in file \
             tools are confined to it; the user can change it via the working-directory \
             picker.",
            canonical_cwd.display()
        ));
    }
    Ok(canonical_target)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Missing required string argument \"{key}\""))
}

fn list(args: &Value, cwd: &Path) -> Result<String, String> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let dir = resolve_contained(cwd, path)?;
    let entries =
        std::fs::read_dir(&dir).map_err(|e| format!("Could not read {}: {e}", dir.display()))?;

    let mut lines: Vec<(bool, String, u64)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.metadata() {
            Ok(meta) if meta.is_dir() => lines.push((true, name, 0)),
            Ok(meta) => lines.push((false, name, meta.len())),
            Err(_) => lines.push((false, name, 0)),
        }
    }
    lines.sort_by(|a, b| a.1.cmp(&b.1));

    if lines.is_empty() {
        return Ok(format!("{} is empty.", dir.display()));
    }
    let mut out = String::new();
    for (is_dir, name, size) in lines {
        if is_dir {
            out.push_str(&format!("[DIR]  {name}/\n"));
        } else {
            out.push_str(&format!("[FILE] {name} ({size} bytes)\n"));
        }
    }
    Ok(out.trim_end().to_string())
}

fn read(args: &Value, cwd: &Path) -> Result<String, String> {
    let path = arg_str(args, "path")?;
    let file = resolve_contained(cwd, path)?;
    let bytes =
        std::fs::read(&file).map_err(|e| format!("Could not read {}: {e}", file.display()))?;

    if bytes.contains(&0) {
        return Err(format!(
            "{} looks like a binary file ({} bytes); the builtin read tool only handles text.",
            file.display(),
            bytes.len()
        ));
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if text.len() > MAX_READ_BYTES {
        let cut = text
            .char_indices()
            .nth(MAX_READ_BYTES)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        text.truncate(cut);
        text.push_str(&format!(
            "\n\n… truncated (file is larger than {} bytes)",
            MAX_READ_BYTES
        ));
    }
    Ok(text)
}

fn search(args: &Value, cwd: &Path) -> Result<String, String> {
    let pattern = arg_str(args, "pattern")?.to_ascii_lowercase();
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let root = resolve_contained(cwd, path)?;

    // ponytail: the walk stops at MAX_WALK_ENTRIES collected paths, not at
    // MAX_SEARCH_RESULTS matches — in a repo with more entries than that,
    // deep matches can be missed; raise the cap if that ever bites.
    let mut paths = Vec::new();
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    collect_rel_paths(&canonical_root, &canonical_root, 0, &mut paths);
    let mut hits: Vec<String> = paths
        .iter()
        .filter(|rel| base_name(rel).to_ascii_lowercase().contains(&pattern))
        .map(|rel| {
            if rel.ends_with('/') {
                format!("[DIR]  {rel}")
            } else {
                format!("[FILE] {rel}")
            }
        })
        .collect();
    hits.truncate(MAX_SEARCH_RESULTS);
    if hits.is_empty() {
        return Ok(format!(
            "No file or directory names matching \"{pattern}\" under {}.",
            root.display()
        ));
    }
    Ok(hits.join("\n"))
}

/// The last path segment of a collected relative path (directories carry a
/// trailing `/`).
fn base_name(rel: &str) -> &str {
    rel.trim_end_matches('/').rsplit('/').next().unwrap_or(rel)
}

/// One walk shared by `search` and `suggest`: every file and directory under
/// `dir`, relative to `root`, directories with a trailing `/`, noise
/// directories skipped.
fn collect_rel_paths(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_SEARCH_DEPTH || out.len() >= MAX_WALK_ENTRIES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_WALK_ENTRIES {
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir && (SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.')) {
            // noise directories are neither listed nor walked
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(&entry.path())
            .to_string_lossy()
            .into_owned();
        out.push(if is_dir { format!("{rel}/") } else { rel });
        if is_dir {
            collect_rel_paths(root, &entry.path(), depth + 1, out);
        }
    }
}

/// `@path` tokens in a user message: word-anchored, optionally quoted for
/// paths with spaces, a trailing run of punctuation ignored on unquoted
/// paths, deduplicated, in first-seen order.
pub fn extract_references(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let at_start = i == 0 || bytes[i - 1].is_ascii_whitespace();
        if bytes[i] != b'@' || !at_start {
            i += 1;
            continue;
        }
        let rest = &text[i + 1..];
        let (path, consumed) = if let Some(stripped) = rest.strip_prefix('"') {
            match stripped.find('"') {
                // unterminated quote: not a token
                None => {
                    i += 1;
                    continue;
                }
                Some(end) => (stripped[..end].to_string(), end + 1),
            }
        } else {
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '@')
                .unwrap_or(rest.len());
            let raw = rest[..end].trim_end_matches(|c: char| {
                matches!(
                    c,
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\"' | '\''
                )
            });
            (raw.to_string(), end)
        };
        if !path.is_empty() && !out.contains(&path) {
            out.push(path);
        }
        i += 1 + consumed;
    }
    out
}

/// The model-only note for the `@path` references in a user message, if any.
/// Mirrors the `#chat_` reminder: the file is not loaded, read it on demand.
pub fn reference_reminder(text: &str) -> Option<String> {
    let paths = extract_references(text);
    if paths.is_empty() {
        return None;
    }
    let mut out = String::from(
        "The user's message names files with `@path` tokens. Their contents are \
         not attached to this message.\n\nReferenced files:\n",
    );
    for path in &paths {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str(
        "\nRead them with ducky__fs_read when you need their contents. Paths are \
         relative to the working directory.\n",
    );
    Some(out)
}

/// Paths for the composer's `@` file reference menu: every file and
/// directory under `cwd`, relative, directories with a trailing `/`. Filtering
/// happens client-side.
pub fn suggest(cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let root = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    collect_rel_paths(&root, &root, 0, &mut out);
    out
}

fn write(args: &Value, cwd: &Path) -> Result<String, String> {
    let path = arg_str(args, "path")?;
    let content = arg_str(args, "content")?;
    let file = resolve_contained(cwd, path)?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&file, content)
        .map_err(|e| format!("Could not write {}: {e}", file.display()))?;
    Ok(format!(
        "Wrote {} bytes to {}.",
        content.len(),
        file.display()
    ))
}

fn mkdir(args: &Value, cwd: &Path) -> Result<String, String> {
    let path = arg_str(args, "path")?;
    let dir = resolve_contained(cwd, path)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    Ok(format!("Created {}.", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_cwd() -> PathBuf {
        tempfile::tempdir().unwrap().keep()
    }

    #[tokio::test]
    async fn write_read_list_roundtrip() {
        let cwd = temp_cwd();
        execute(
            WRITE,
            &json!({"path": "notes/todo.txt", "content": "hello"}),
            &cwd,
        )
        .await
        .unwrap();
        let text = execute(READ, &json!({"path": "notes/todo.txt"}), &cwd)
            .await
            .unwrap();
        assert_eq!(text, "hello");
        let listing = execute(LIST, &json!({}), &cwd).await.unwrap();
        assert!(listing.contains("[DIR]  notes/"), "got: {listing}");
    }

    #[tokio::test]
    async fn rejects_escape_attempts() {
        let cwd = temp_cwd();
        let outside = tempfile::tempdir().unwrap().keep();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();

        for evil in [
            "../escape.txt",
            outside.join("secret.txt").to_str().unwrap(),
        ] {
            let err = execute(READ, &json!({"path": evil}), &cwd)
                .await
                .unwrap_err();
            assert!(err.contains("outside the working directory"), "got: {err}");
        }
    }

    #[tokio::test]
    async fn rejects_symlink_escape() {
        let cwd = temp_cwd();
        let outside = tempfile::tempdir().unwrap().keep();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, cwd.join("link")).unwrap();

        let err = execute(READ, &json!({"path": "link/secret.txt"}), &cwd)
            .await
            .unwrap_err();
        assert!(err.contains("outside the working directory"), "got: {err}");
    }

    #[test]
    fn extract_references_anchors_quotes_and_deduplicates() {
        assert_eq!(
            extract_references("look at @src/main.rs and @README.md."),
            vec!["src/main.rs".to_string(), "README.md".to_string()]
        );
        assert_eq!(
            extract_references("quoted @\"my file.txt\" too"),
            vec!["my file.txt".to_string()]
        );
        // not word-anchored, bare @, or unterminated quotes
        assert!(extract_references("mail a@b.com please").is_empty());
        assert!(extract_references("just @ alone").is_empty());
        assert!(extract_references("bad @\"unterminated path").is_empty());
        // deduplicated
        assert_eq!(
            extract_references("@a.ts and again @a.ts"),
            vec!["a.ts".to_string()]
        );
    }

    #[test]
    fn reference_reminder_lists_paths_and_the_read_tool() {
        let reminder = reference_reminder("check @src/lib.rs for the bug").unwrap();
        assert!(reminder.contains("- src/lib.rs"), "got: {reminder}");
        assert!(reminder.contains("ducky__fs_read"), "got: {reminder}");
        assert!(reference_reminder("no references here").is_none());
    }

    #[test]
    fn suggest_lists_relative_paths_and_skips_noise() {
        let cwd = temp_cwd();
        std::fs::write(cwd.join("main.rs"), "x").unwrap();
        std::fs::create_dir_all(cwd.join("src/deep")).unwrap();
        std::fs::write(cwd.join("src/deep/mod.rs"), "x").unwrap();
        std::fs::create_dir_all(cwd.join("target/debug")).unwrap();
        std::fs::write(cwd.join("target/debug/main.rs"), "x").unwrap();

        let paths = suggest(&cwd);
        assert!(paths.contains(&"main.rs".to_string()), "got: {paths:?}");
        assert!(paths.contains(&"src/".to_string()), "got: {paths:?}");
        assert!(
            paths.contains(&"src/deep/mod.rs".to_string()),
            "got: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("target")),
            "got: {paths:?}"
        );
    }

    #[tokio::test]
    async fn search_finds_names_and_skips_noise() {
        let cwd = temp_cwd();
        std::fs::write(cwd.join("report-q3.md"), "x").unwrap();
        std::fs::create_dir_all(cwd.join("node_modules/pkg")).unwrap();
        std::fs::write(cwd.join("node_modules/pkg/report-q3.js"), "x").unwrap();
        std::fs::create_dir_all(cwd.join(".git")).unwrap();
        std::fs::write(cwd.join(".git/report-q3"), "x").unwrap();

        let hits = execute(SEARCH, &json!({"pattern": "report-q3"}), &cwd)
            .await
            .unwrap();
        assert_eq!(hits, "[FILE] report-q3.md");
    }

    #[tokio::test]
    async fn rejects_binary_reads() {
        let cwd = temp_cwd();
        std::fs::write(cwd.join("blob.bin"), [0u8, 1, 0, 2]).unwrap();
        let err = execute(READ, &json!({"path": "blob.bin"}), &cwd)
            .await
            .unwrap_err();
        assert!(err.contains("binary"), "got: {err}");
    }
}
