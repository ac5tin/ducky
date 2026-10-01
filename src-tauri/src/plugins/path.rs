//! Path containment helpers (design §13).

use std::path::{Component, Path, PathBuf};

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

/// Like [`resolve_within`], but for paths that do not exist yet: canonicalize
/// the nearest existing ancestor, re-append the missing remainder, and require
/// containment. Used for `${PLUGIN_DATA}` subdirectories created at spawn.
pub fn resolve_within_maybe_missing(root: &Path, target: &Path) -> Option<PathBuf> {
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let mut path = normalize(&std::path::absolute(target).ok()?);
    let mut missing = Vec::new();
    while !path.exists() {
        missing.push(path.file_name()?.to_os_string());
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

/// Lexically resolve `.` and `..` so the walk-up above only ever sees normal
/// components; real symlink resolution still happens in `canonicalize`.
fn normalize(absolute: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            component => out.push(component),
        }
    }
    out
}
