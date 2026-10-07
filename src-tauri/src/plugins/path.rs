//! Path containment helpers (design §13).

use std::path::{Path, PathBuf};

/// Canonicalize `root` and `target` and require the target to stay inside.
///
/// Used for every path that must already exist (design §13): manifests,
/// component locations, `SKILL.md` files, `cwd`. Symlinks are resolved, not
/// rejected: one that stays inside the root is allowed, one that resolves
/// outside is denied.
pub fn resolve_within(root: &Path, target: &Path) -> Option<PathBuf> {
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let canonical_target = std::fs::canonicalize(target).ok()?;
    canonical_target
        .starts_with(&canonical_root)
        .then_some(canonical_target)
}

/// Like [`resolve_within`], but for paths that do not exist yet.
///
/// Canonicalize the nearest existing ancestor — `..` included, so a symlink
/// in that ancestor is visible to the filesystem — then re-append only the
/// missing normal components and require `starts_with` again (design §13).
/// Do not pop `..` before the walk. `std::path::absolute` is also avoided:
/// on Windows it calls `GetFullPathNameW`, which pops `..` lexically.
pub fn resolve_within_maybe_missing(root: &Path, target: &Path) -> Option<PathBuf> {
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let mut path = absolute_lexical(target)?;
    let mut missing = Vec::new();
    while !path.exists() {
        let name = path.file_name()?;
        // This `..` or `.` was not resolved by the filesystem (its parent
        // does not exist). Re-appending it would hide an escape from
        // `starts_with`.
        if name == ".." || name == "." {
            return None;
        }
        missing.push(name.to_os_string());
        if !path.pop() {
            return None;
        }
    }
    let mut resolved = std::fs::canonicalize(&path).ok()?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    resolved.starts_with(&canonical_root).then_some(resolved)
}

/// Make `path` absolute without removing `..`.
fn absolute_lexical(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        Some(std::env::current_dir().ok()?.join(path))
    }
}
