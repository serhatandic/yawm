//! Finding git repositories on disk.
//!
//! yawm supports two ways of learning about repositories: the user names one
//! explicitly, or points at a folder to search. Searching stops descending as
//! soon as it finds a repository, so a monorepo with vendored checkouts does
//! not explode into hundreds of entries, and skips the directories that make
//! recursive scans slow.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::MovedWorktreeDiagnostic;
use crate::git::Git;
use crate::git::collect::list_worktrees;
use crate::path::path_key;

/// Directories never worth descending into when looking for repositories.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "vendor",
    ".gradle",
    "Pods",
    ".cache",
    "Library",
    ".Trash",
];

/// How far to search below a scan root.
pub const DEFAULT_MAX_DEPTH: usize = 4;

/// Find candidate repository directories beneath `root`.
///
/// Returns directories containing a `.git` entry. A linked worktree also has
/// one (a file rather than a directory), so results are resolved and
/// deduplicated by [`resolve_repositories`] before use.
pub fn find_candidates(root: &Path, max_depth: usize) -> Vec<PathBuf> {
    find_candidates_reporting(root, max_depth).0
}

fn find_candidates_reporting(
    root: &Path,
    max_depth: usize,
) -> (Vec<PathBuf>, Vec<UnreadableSource>) {
    let mut found = Vec::new();
    let mut unreadable = Vec::new();
    if !root.is_dir() {
        return (found, unreadable);
    }
    walk(root, 0, max_depth, &mut found, Some(&mut unreadable));
    found.sort();
    (found, unreadable)
}

fn walk(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<PathBuf>,
    mut unreadable: Option<&mut Vec<UnreadableSource>>,
) {
    if dir.join(".git").exists() {
        out.push(dir.to_path_buf());
        // Everything below belongs to this repository, including its worktrees
        // and submodules; they are enumerated through git rather than the
        // filesystem.
        return;
    }
    if depth >= max_depth {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            if let Some(unreadable) = &mut unreadable {
                unreadable.push(UnreadableSource {
                    path: dir.to_path_buf(),
                    reason: format!("could not read folder: {error}"),
                    moved_worktree: None,
                });
            }
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if let Some(unreadable) = &mut unreadable {
                    unreadable.push(UnreadableSource {
                        path: dir.to_path_buf(),
                        reason: format!("could not read a child entry: {error}"),
                        moved_worktree: None,
                    });
                }
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                if let Some(unreadable) = &mut unreadable {
                    unreadable.push(UnreadableSource {
                        path: entry.path(),
                        reason: format!("could not inspect child entry: {error}"),
                        moved_worktree: None,
                    });
                }
                continue;
            }
        };
        // Following links risks cycles and directories outside the scan root.
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        walk(
            &entry.path(),
            depth + 1,
            max_depth,
            out,
            unreadable.as_deref_mut(),
        );
    }
}

/// Reduce candidate paths to distinct repositories, keyed by main worktree.
///
/// Pointing yawm at a linked worktree, or at several worktrees of one
/// repository, should still yield a single repository. Git resolves that for
/// us: the first entry of `worktree list` is always the main worktree.
pub fn resolve_repositories(git: &Git, candidates: &[PathBuf]) -> Vec<PathBuf> {
    resolve_repositories_reporting(git, candidates).repositories
}

/// A configured source yawm could not read.
///
/// Carried rather than dropped because the symptom of dropping it is a shorter
/// list, and a shorter list is indistinguishable from "those worktrees were
/// cleaned up". Someone whose external drive is unmounted must be told that,
/// not left to conclude their work is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadableSource {
    pub path: PathBuf,
    pub reason: String,
    #[serde(default)]
    pub moved_worktree: Option<MovedWorktreeDiagnostic>,
}

/// Repositories found, alongside the sources that could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub repositories: Vec<PathBuf>,
    pub unreadable: Vec<UnreadableSource>,
}

impl Discovery {
    pub fn is_complete(&self) -> bool {
        self.unreadable.is_empty()
    }

    /// Record a source that could not be read, keeping the first reason given.
    pub fn note_unreadable(&mut self, path: &Path, reason: String) {
        let key = path_key(path);
        if self.unreadable.iter().any(|s| path_key(&s.path) == key) {
            return;
        }
        self.unreadable.push(UnreadableSource {
            path: path.to_path_buf(),
            reason,
            moved_worktree: None,
        });
    }

    /// Fold another discovery's failures in.
    ///
    /// Discovery happens in two passes — searching the scan roots, then asking
    /// git about every candidate — and a failure in either one is a source the
    /// user cannot see, so neither pass may drop the other's.
    pub fn absorb_failures(&mut self, other: impl IntoIterator<Item = UnreadableSource>) {
        for source in other {
            let key = path_key(&source.path);
            if self
                .unreadable
                .iter()
                .any(|item| path_key(&item.path) == key)
            {
                continue;
            }
            self.unreadable.push(source);
        }
    }
}

/// [`resolve_repositories`], keeping the sources it had to skip.
pub fn resolve_repositories_reporting(git: &Git, candidates: &[PathBuf]) -> Discovery {
    let mut seen = BTreeSet::new();
    let mut found = Discovery::default();

    for candidate in candidates {
        let worktrees = match list_worktrees(git, candidate) {
            Ok(worktrees) => worktrees,
            Err(crate::error::Error::MovedWorktree { diagnostic }) => {
                let reason = crate::error::Error::MovedWorktree {
                    diagnostic: diagnostic.clone(),
                }
                .to_string();
                found.unreadable.push(UnreadableSource {
                    path: candidate.clone(),
                    reason,
                    moved_worktree: Some(diagnostic),
                });
                continue;
            }
            Err(error) => {
                found.note_unreadable(candidate, error.to_string());
                continue;
            }
        };
        let Some(main) = worktrees.first() else {
            found.note_unreadable(candidate, "git listed no worktrees".to_string());
            continue;
        };
        if seen.insert(path_key(&main.path)) {
            found.repositories.push(main.path.clone());
        }
    }

    found
}

/// Search `roots` and return the distinct repositories found.
pub fn discover(git: &Git, roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    discover_reporting(git, roots, max_depth).repositories
}

/// [`discover`], keeping scan roots and descendants that could not be searched.
///
/// A root that is not a directory is reported rather than skipped: it is the
/// unmounted-volume case, and it looks exactly like a folder that has finally
/// been emptied of worktrees.
pub fn discover_reporting(git: &Git, roots: &[PathBuf], max_depth: usize) -> Discovery {
    let mut unreadable_sources = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in roots {
        if !root.is_dir() {
            unreadable_sources.push(UnreadableSource {
                path: root.clone(),
                reason: "the folder is missing or unreadable".to_string(),
                moved_worktree: None,
            });
            continue;
        }
        let (found, unreadable) = find_candidates_reporting(root, max_depth);
        candidates.extend(found);
        unreadable_sources.extend(unreadable);
    }

    let mut found = resolve_repositories_reporting(git, &candidates);
    for source in unreadable_sources {
        found.note_unreadable(&source.path, source.reason);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo_at(path: &Path) {
        fs::create_dir_all(path.join(".git")).unwrap();
    }

    #[test]
    fn finds_a_repository_at_the_root() {
        let dir = tempfile::tempdir().unwrap();
        repo_at(dir.path());

        let found = find_candidates(dir.path(), DEFAULT_MAX_DEPTH);
        assert_eq!(found, vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn finds_nested_repositories() {
        let dir = tempfile::tempdir().unwrap();
        repo_at(&dir.path().join("code/alpha"));
        repo_at(&dir.path().join("code/beta"));

        let found = find_candidates(dir.path(), DEFAULT_MAX_DEPTH);
        assert_eq!(found.len(), 2);
    }

    /// Once a repository is found there is no reason to keep descending; its
    /// internals are enumerated through git instead.
    #[test]
    fn does_not_descend_into_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        repo_at(&dir.path().join("outer"));
        repo_at(&dir.path().join("outer/vendored"));

        let found = find_candidates(dir.path(), DEFAULT_MAX_DEPTH);
        assert_eq!(found, vec![dir.path().join("outer")]);
    }

    #[test]
    fn skips_dependency_directories() {
        let dir = tempfile::tempdir().unwrap();
        repo_at(&dir.path().join("node_modules/pkg"));
        repo_at(&dir.path().join("real"));

        let found = find_candidates(dir.path(), DEFAULT_MAX_DEPTH);
        assert_eq!(found, vec![dir.path().join("real")]);
    }

    #[test]
    fn respects_the_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        repo_at(&dir.path().join("a/b/c/d/e/deep"));

        assert!(find_candidates(dir.path(), 2).is_empty());
        assert_eq!(find_candidates(dir.path(), 8).len(), 1);
    }

    /// A linked worktree has a `.git` file rather than a directory, and must
    /// still be recognised as a candidate.
    #[test]
    fn recognises_a_worktree_git_file() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join(".git"), "gitdir: /elsewhere").unwrap();

        let found = find_candidates(dir.path(), DEFAULT_MAX_DEPTH);
        assert_eq!(found, vec![wt]);
    }

    #[test]
    fn missing_root_yields_nothing() {
        assert!(find_candidates(Path::new("/definitely/not/here"), 4).is_empty());
    }

    /// The unmounted-volume case. Dropping the source produces a shorter list,
    /// which reads as "those worktrees are gone" rather than "yawm could not
    /// look", so the source has to come back with the results.
    #[test]
    fn an_unreadable_repository_is_reported_rather_than_dropped() {
        let git = Git::new();
        let missing = PathBuf::from("/definitely/not/here/temporarily-unmounted-repo");

        let found = resolve_repositories_reporting(&git, std::slice::from_ref(&missing));

        assert!(
            found.repositories.is_empty(),
            "no repository may be invented for a path that is not there; got {:?}",
            found.repositories
        );
        assert!(!found.is_complete());
        assert_eq!(found.unreadable.len(), 1);
        assert_eq!(found.unreadable[0].path, missing);
    }

    #[test]
    fn a_missing_scan_root_is_reported() {
        let git = Git::new();
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf(), "/definitely/not/here".into()];

        let found = discover_reporting(&git, &roots, DEFAULT_MAX_DEPTH);

        assert_eq!(found.unreadable.len(), 1);
        assert_eq!(
            found.unreadable[0].path,
            PathBuf::from("/definitely/not/here")
        );
    }

    /// Discovery runs in two passes and the scanner joins them, so failures
    /// from the earlier one must survive being merged into the later one's.
    #[test]
    fn absorbed_failures_are_kept_and_never_doubled() {
        let mut found = Discovery::default();
        found.note_unreadable(Path::new("/volumes/work"), "unmounted".into());

        let mut other = Discovery::default();
        other.note_unreadable(Path::new("/volumes/work/"), "unmounted".into());
        other.note_unreadable(Path::new("/code/gone"), "no longer a repository".into());

        found.absorb_failures(other.unreadable);

        assert_eq!(found.unreadable.len(), 2, "{:?}", found.unreadable);
        assert!(
            found
                .unreadable
                .iter()
                .any(|s| s.path == Path::new("/code/gone"))
        );
    }

    #[test]
    fn absorbing_a_moved_worktree_preserves_its_structured_repair() {
        let diagnostic = crate::error::MovedWorktreeDiagnostic {
            main_worktree: "/code/main".into(),
            common_admin_dir: "/code/main/.git".into(),
            observed_path: "/code/moved".into(),
            repair_command: vec![
                "git".into(),
                "-C".into(),
                "/code/main".into(),
                "worktree".into(),
                "repair".into(),
                "/code/moved".into(),
            ],
        };
        let mut found = Discovery::default();
        found.absorb_failures([UnreadableSource {
            path: "/code/moved".into(),
            reason: "moved".into(),
            moved_worktree: Some(diagnostic.clone()),
        }]);

        assert_eq!(found.unreadable[0].moved_worktree, Some(diagnostic));
    }

    /// A readable repository must still resolve, and report nothing wrong.
    #[test]
    fn a_readable_repository_reports_no_problems() {
        let git = Git::new();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "t"],
            vec!["commit", "--allow-empty", "-m", "root", "--quiet"],
        ] {
            git.run(&repo, &args).unwrap();
        }

        let found = resolve_repositories_reporting(&git, std::slice::from_ref(&repo));

        assert!(found.is_complete(), "{:?}", found.unreadable);
        assert_eq!(found.repositories.len(), 1);
    }
}
