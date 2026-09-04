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

    let mut hits: Vec<String> = Vec::new();
    walk(&root, &root, &pattern, 0, &mut hits);
    if hits.is_empty() {
        return Ok(format!(
            "No file or directory names matching \"{pattern}\" under {}.",
            root.display()
        ));
    }
    Ok(hits.join("\n"))
}

fn walk(root: &Path, dir: &Path, pattern: &str, depth: usize, hits: &mut Vec<String>) {
    if depth > MAX_SEARCH_DEPTH || hits.len() >= MAX_SEARCH_RESULTS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if hits.len() >= MAX_SEARCH_RESULTS {
            return;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if name.to_ascii_lowercase().contains(pattern) {
            let rel = entry
                .path()
                .strip_prefix(root)
                .unwrap_or(&entry.path())
                .to_string_lossy()
                .into_owned();
            hits.push(if is_dir {
                format!("[DIR]  {rel}/")
            } else {
                format!("[FILE] {rel}")
            });
        }
        if is_dir && !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
            walk(root, &entry.path(), pattern, depth + 1, hits);
        }
    }
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
