//! Hidden git snapshot repos backing `/undo`.
//!
//! opencode-style: one private git repo per project repository, stored under
//! the app-data dir with `--work-tree` pointing at the project on every
//! invocation. Its object database borrows the project's via
//! `objects/info/alternates` and its index starts as a copy of the project's,
//! so capturing a tree only stores what changed. The user's own repository
//! (index, staging area, objects) is never touched, and gitignored files are
//! not captured (they cannot be reverted either — same trade-off as opencode).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context};

/// A prepared snapshot repo: `.git` dir plus the project root it points at.
struct SnapshotRepo {
    git_dir: PathBuf,
    work_tree: PathBuf,
}

/// Env vars that could redirect git at a different repository; the snapshot
/// repo must work no matter what the user's shell exported.
const GIT_ENV_TO_CLEAR: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

fn run_git<S: AsRef<str>>(
    git_dir: Option<&Path>,
    work_tree: Option<&Path>,
    cwd: &Path,
    args: &[S],
) -> anyhow::Result<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    for var in GIT_ENV_TO_CLEAR {
        cmd.env_remove(var);
    }
    if let Some(gd) = git_dir {
        cmd.arg("--git-dir").arg(gd);
    }
    if let Some(wt) = work_tree {
        cmd.arg("--work-tree").arg(wt);
    }
    cmd.args(args.iter().map(|s| s.as_ref()));
    let out = cmd
        .output()
        .map_err(|e| anyhow!("could not run git: {e}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.first().map(|s| s.as_ref()).unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run git in the user's project (no snapshot repo involved).
fn git_project(wd: &Path, args: &[&str]) -> anyhow::Result<String> {
    run_git(None, None, wd, args)
}

/// Run git against the snapshot repo with the project as its work tree.
fn git_repo(repo: &SnapshotRepo, args: &[&str]) -> anyhow::Result<String> {
    run_git(Some(&repo.git_dir), Some(&repo.work_tree), &repo.work_tree, args)
}

/// The root of the git repository containing `wd`, if any.
pub fn repo_root(wd: &Path) -> Option<PathBuf> {
    let out = git_project(wd, &["rev-parse", "--show-toplevel"]).ok()?;
    Some(PathBuf::from(out))
}

fn dir_key(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Create (once) the private snapshot repo for `root` and return it.
fn ensure_repo(snap_root: &Path, root: &Path) -> anyhow::Result<SnapshotRepo> {
    let key = dir_key(root);
    let dir = snap_root.join(&key);
    let git_dir = dir.join(".git");
    if !git_dir.join("HEAD").exists() {
        std::fs::create_dir_all(snap_root).context("create snapshots dir")?;
        run_git::<&str>(None, None, snap_root, &["init", &key])?;
        run_git(
            Some(&git_dir),
            Some(root),
            root,
            &[
                "config",
                "core.worktree",
                &root.to_string_lossy(),
            ],
        )?;
    }

    // Borrow the project's object database so unchanged files are never
    // re-hashed, and seed the index from it for the same reason. Only done
    // once; harmless if the project repo is unusual or missing its index.
    if !git_dir.join("objects").join("info").join("alternates").exists() {
        if let Ok(project_git_dir) =
            git_project(root, &["rev-parse", "--absolute-git-dir"])
        {
            let project_objects = PathBuf::from(&project_git_dir).join("objects");
            if project_objects.is_dir() {
                let info = git_dir.join("objects").join("info");
                std::fs::create_dir_all(&info)?;
                std::fs::write(
                    info.join("alternates"),
                    format!("{}\n", project_objects.display()),
                )?;
                let project_index = PathBuf::from(&project_git_dir).join("index");
                if project_index.exists() && !git_dir.join("index").exists() {
                    let _ = std::fs::copy(&project_index, git_dir.join("index"));
                }
            }
        }
    }

    Ok(SnapshotRepo {
        git_dir,
        work_tree: root.to_path_buf(),
    })
}

/// Record the current state of the repository containing `wd` and return its
/// tree hash plus the repository root it belongs to. Fails outside git
/// repositories (or when git is unavailable) — callers treat that as "no file
/// undo for this turn".
pub fn capture(snap_root: &Path, wd: &Path) -> anyhow::Result<(String, PathBuf)> {
    let root = repo_root(wd).ok_or_else(|| anyhow!("not inside a git work tree"))?;
    let repo = ensure_repo(snap_root, &root)?;
    git_repo(&repo, &["add", "-A"])?;
    let tree = git_repo(&repo, &["write-tree"])?;
    Ok((tree, root))
}

/// Restore the repository containing `wd` to `tree`, returning the affected
/// paths (repository-relative). Files added after the snapshot are deleted;
/// modified or deleted ones are brought back. Gitignored files were never
/// captured and are left alone.
pub fn restore(snap_root: &Path, wd: &Path, tree: &str) -> anyhow::Result<Vec<String>> {
    let root = repo_root(wd).ok_or_else(|| anyhow!("not inside a git work tree"))?;
    let repo = ensure_repo(snap_root, &root)?;

    // Snapshot the current state too, so the two trees can be diffed.
    git_repo(&repo, &["add", "-A"])?;
    let current = git_repo(&repo, &["write-tree"])?;
    if current == tree {
        return Ok(Vec::new());
    }

    // -z: NUL-separated `status NUL path NUL` records, no path quoting.
    // --no-renames keeps every entry a plain A/M/D.
    let raw = run_git(
        Some(&repo.git_dir),
        Some(&repo.work_tree),
        &repo.work_tree,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-status",
            tree,
            &current,
        ],
    )?;
    let mut fields = raw.split('\0');
    let mut restore_paths: Vec<String> = Vec::new();
    let mut delete_paths: Vec<String> = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        if path.is_empty() {
            break;
        }
        if status == "A" {
            // did not exist at the snapshot → remove it again
            delete_paths.push(path.to_string());
        } else {
            restore_paths.push(path.to_string());
        }
    }

    if !restore_paths.is_empty() {
        let mut args: Vec<String> = vec!["checkout".into(), tree.into(), "--".into()];
        args.extend(restore_paths.iter().cloned());
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        run_git(Some(&repo.git_dir), Some(&repo.work_tree), &repo.work_tree, &refs)?;
    }
    for rel in &delete_paths {
        let path = root.join(rel);
        if std::fs::remove_file(&path).is_ok() {
            prune_empty_parents(&path, &root);
        }
    }

    let mut affected = restore_paths;
    affected.extend(delete_paths);
    Ok(affected)
}

/// Remove now-empty directories left behind by a deleted file, up to (but not
/// including) the repository root.
fn prune_empty_parents(file: &Path, root: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) {
            break;
        }
        if std::fs::remove_dir(d).is_err() {
            break; // not empty (or no permission) — stop climbing
        }
        dir = d.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn capture_and_restore_roundtrip() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        git_project(&proj, &["init"]).unwrap();
        std::fs::write(proj.join("a.txt"), "one").unwrap();
        std::fs::write(proj.join("b.txt"), "keep me tracked").unwrap();
        std::fs::write(proj.join(".gitignore"), "ignored.txt\n").unwrap();
        let snaps = tmp.path().join("snaps");

        let (tree, root) = capture(&snaps, &proj).unwrap();
        assert_eq!(root.canonicalize().unwrap(), proj.canonicalize().unwrap());

        // modify, create (also in a new subdirectory), delete a tracked file,
        // and change a gitignored file
        std::fs::write(proj.join("a.txt"), "two").unwrap();
        std::fs::create_dir(proj.join("src")).unwrap();
        std::fs::write(proj.join("src/new.txt"), "new").unwrap();
        std::fs::remove_file(proj.join("b.txt")).unwrap();
        std::fs::write(proj.join("ignored.txt"), "junk").unwrap();

        let affected = restore(&snaps, &root, &tree).unwrap();
        assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "one");
        assert_eq!(
            std::fs::read_to_string(proj.join("b.txt")).unwrap(),
            "keep me tracked"
        );
        assert!(!proj.join("src").exists(), "new file and its dir are gone");
        // gitignored files were never captured, so they survive a restore
        assert!(proj.join("ignored.txt").exists());
        assert!(affected.contains(&"a.txt".to_string()));
        assert!(affected.contains(&"b.txt".to_string()));
        assert!(affected.contains(&"src/new.txt".to_string()));
        assert!(!affected.contains(&"ignored.txt".to_string()));
    }

    #[test]
    fn restore_to_identical_tree_is_a_noop() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        git_project(&proj, &["init"]).unwrap();
        std::fs::write(proj.join("a.txt"), "one").unwrap();
        let snaps = tmp.path().join("snaps");
        let (tree, root) = capture(&snaps, &proj).unwrap();
        assert!(restore(&snaps, &root, &tree).unwrap().is_empty());
    }

    /// Two turns, then /undo twice: restoring the second turn's snapshot must
    /// keep the first turn's changes, and restoring the first must undo them.
    #[test]
    fn chained_undo_steps_back_one_turn_at_a_time() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir(&proj).unwrap();
        git_project(&proj, &["init"]).unwrap();
        std::fs::write(proj.join("a.txt"), "v0").unwrap();
        let snaps = tmp.path().join("snaps");

        let (tree1, root) = capture(&snaps, &proj).unwrap();
        std::fs::write(proj.join("a.txt"), "turn 1 edit").unwrap();
        let (tree2, _) = capture(&snaps, &proj).unwrap();
        std::fs::write(proj.join("a.txt"), "turn 2 edit").unwrap();
        std::fs::write(proj.join("new.txt"), "turn 2 file").unwrap();

        restore(&snaps, &root, &tree2).unwrap();
        assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "turn 1 edit");
        assert!(!proj.join("new.txt").exists());

        restore(&snaps, &root, &tree1).unwrap();
        assert_eq!(std::fs::read_to_string(proj.join("a.txt")).unwrap(), "v0");
    }

    #[test]
    fn capture_fails_outside_a_git_repo() {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        // guard the documented contract only where the parent really is
        // outside any git repo (tmp dirs on odd setups might not be)
        if repo_root(&plain).is_none() {
            assert!(capture(&tmp.path().join("snaps"), &plain).is_err());
        }
    }
}
