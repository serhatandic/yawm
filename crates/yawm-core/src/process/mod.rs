//! Detecting which worktrees are in use right now.
//!
//! A worktree with a live process inside it is almost certainly one an agent is
//! working in, and must never be suggested for deletion. The signal comes from
//! each process's current working directory.
//!
//! This is the capability that decided the stack. Reading another process's cwd
//! works on all three targets from Rust — `/proc/<pid>/cwd` on Linux, `libproc`
//! on macOS, and the process environment block on Windows — and needs no
//! elevation for processes owned by the same user. Node has no equivalent on
//! Windows at all, so under Electron this feature could only ever have shipped
//! on two platforms out of three.
//!
//! The signal is additive: when it is unavailable, classification falls back to
//! recent file modification, which works everywhere. No verdict becomes wrong
//! without it, only slightly less confident.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::ProcessInfo;
use crate::path::path_key;

/// Processes found inside each worktree, keyed by [`path_key`].
pub type ProcessMap = HashMap<String, Vec<ProcessInfo>>;

/// Find processes whose working directory lies inside any of `roots`.
///
/// Takes a single snapshot of the process table and attributes each process to
/// at most one worktree — the deepest match, so a worktree nested inside
/// another is attributed correctly.
pub fn scan(roots: &[PathBuf]) -> ProcessMap {
    scan_matching(roots, Attribution::Deepest)
}

/// Processes running inside each root, counting a process for every root that
/// encloses it.
///
/// [`scan`] answers "which worktree is this process working in", which is the
/// question a list of worktrees asks. A removal plan asks a different one, per
/// worktree and independently: "would deleting this directory pull the floor
/// out from under something". An agent working in a nested worktree is running
/// inside the outer one too, and deleting the outer one takes it down — so it
/// is counted against both.
pub fn scan_enclosing(roots: &[PathBuf]) -> ProcessMap {
    scan_matching(roots, Attribution::Every)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Attribution {
    Deepest,
    Every,
}

fn scan_matching(roots: &[PathBuf], attribution: Attribution) -> ProcessMap {
    let mut map: ProcessMap = HashMap::new();
    if roots.is_empty() {
        return map;
    }

    let index: HashMap<String, ()> = roots.iter().map(|r| (path_key(r), ())).collect();

    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cwd(sysinfo::UpdateKind::Always),
    );

    let own_pid = std::process::id();

    for (pid, process) in system.processes() {
        let pid = pid.as_u32();
        // yawm itself may well be running from inside a worktree; reporting
        // that as "in use" would be noise.
        if pid == own_pid {
            continue;
        }

        let Some(cwd) = process.cwd() else { continue };
        let owners = match attribution {
            Attribution::Deepest => deepest_match(cwd, &index).into_iter().collect(),
            Attribution::Every => enclosing_matches(cwd, &index),
        };
        for owner in owners {
            map.entry(owner).or_default().push(ProcessInfo {
                pid,
                name: process.name().to_string_lossy().into_owned(),
            });
        }
    }

    map
}

/// Attribute a working directory to the most specific enclosing root.
///
/// Walking up from the directory means the first hit is the deepest one, which
/// is what makes nested worktrees resolve to the inner rather than the outer.
fn deepest_match(cwd: &Path, index: &HashMap<String, ()>) -> Option<String> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let key = path_key(dir);
        if index.contains_key(&key) {
            return Some(key);
        }
        current = dir.parent();
    }
    None
}

/// Every root enclosing `cwd`, deepest first.
fn enclosing_matches(cwd: &Path, index: &HashMap<String, ()>) -> Vec<String> {
    let mut found = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let key = path_key(dir);
        if index.contains_key(&key) {
            found.push(key);
        }
        current = dir.parent();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_of(paths: &[&str]) -> HashMap<String, ()> {
        paths.iter().map(|p| (path_key(p), ())).collect()
    }

    #[test]
    fn matches_a_directory_exactly() {
        let index = index_of(&["/code/repo"]);
        assert_eq!(
            deepest_match(Path::new("/code/repo"), &index),
            Some(path_key("/code/repo"))
        );
    }

    #[test]
    fn matches_a_nested_working_directory() {
        let index = index_of(&["/code/repo"]);
        assert_eq!(
            deepest_match(Path::new("/code/repo/src/deep"), &index),
            Some(path_key("/code/repo"))
        );
    }

    #[test]
    fn unrelated_directories_do_not_match() {
        let index = index_of(&["/code/repo"]);
        assert_eq!(deepest_match(Path::new("/elsewhere"), &index), None);
    }

    /// A sibling whose name merely starts with the same characters is a
    /// different worktree entirely.
    #[test]
    fn similar_prefixes_do_not_match() {
        let index = index_of(&["/code/repo"]);
        assert_eq!(deepest_match(Path::new("/code/repo-two/src"), &index), None);
    }

    /// Agents sometimes create worktrees inside the repository. A process there
    /// belongs to the inner worktree, not the outer one.
    #[test]
    fn nested_worktrees_resolve_to_the_innermost() {
        let index = index_of(&["/code/repo", "/code/repo/wt/feature"]);
        assert_eq!(
            deepest_match(Path::new("/code/repo/wt/feature/src"), &index),
            Some(path_key("/code/repo/wt/feature"))
        );
    }

    #[test]
    fn scanning_without_roots_returns_nothing() {
        assert!(scan(&[]).is_empty());
    }

    /// Spawn a real child process in a known directory and detect it.
    ///
    /// This is the test that proves reading another process's working directory
    /// genuinely works on whichever platform CI is running — the capability the
    /// stack choice depends on. `git hash-object --stdin` blocks reading stdin,
    /// so it stays alive while we look for it, and git is already a hard
    /// requirement of yawm.
    #[test]
    fn detects_a_real_child_process_on_this_platform() {
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().expect("tempdir");
        // macOS exposes temp directories under a symlink, so compare real paths.
        let root = dir.path().canonicalize().expect("canonicalize");

        // Git for Windows is an MSYS launcher: the process yawm observes can
        // exit or hand work to another process before the table refreshes, so
        // it is not a stable probe of Windows cwd access. Use a native process
        // there; `more` blocks on the piped stdin just as `hash-object` does.
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/C", "more"])
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a native child process");

        #[cfg(not(windows))]
        let mut child = Command::new("git")
            .args(["hash-object", "--stdin"])
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn a child process");

        let key = path_key(&root);
        let mut found = None;
        // The process table may take a moment to reflect a new process.
        for _ in 0..40 {
            if let Some(hits) = scan(std::slice::from_ref(&root)).remove(&key) {
                found = Some(hits);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Release stdin so the child exits, then reap it.
        drop(child.stdin.take());
        let _ = child.wait();

        let hits = found.expect(
            "no process detected in the child's working directory; \
             reading process cwd appears unavailable on this platform",
        );
        #[cfg(windows)]
        let expected_name = "cmd";
        #[cfg(not(windows))]
        let expected_name = "git";
        assert!(
            hits.iter().any(|p| p.name.contains(expected_name)),
            "found {hits:?}"
        );
    }
}
