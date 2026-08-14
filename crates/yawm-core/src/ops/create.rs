//! Creating a worktree, and making it actually usable.
//!
//! `git worktree add` produces a checkout with none of the repository's
//! gitignored files: no `.env`, no `node_modules`, no local config. That is the
//! single most reported friction with worktrees, and the reason people write
//! wrapper scripts. yawm carries those files over by default, so the common
//! case is that a new worktree simply runs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::Git;
use crate::git::collect::list_worktrees;
use crate::git::managed::record_links;
use crate::path::is_within;

/// Dependency directories worth linking rather than reinstalling.
///
/// Linking is what makes a new worktree cheap: a shared `node_modules` costs no
/// extra disk and saves the install entirely.
pub use crate::git::managed::LINKABLE_DIRS;

/// Lockfiles that decide whether a dependency directory can safely be shared.
///
/// Paired with the directory they govern, because a Python lockfile says
/// nothing about `node_modules`.
const LOCKFILES: &[(&str, &[&str])] = &[
    (
        "node_modules",
        &[
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lockb",
        ],
    ),
    (".venv", &["uv.lock", "poetry.lock", "requirements.txt"]),
    ("venv", &["uv.lock", "poetry.lock", "requirements.txt"]),
    ("vendor", &["Gemfile.lock", "composer.lock", "go.sum"]),
];

/// Something yawm offers to carry into a new worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionItem {
    /// Path relative to the worktree root, e.g. `.env` or `node_modules`.
    pub name: String,
    pub kind: ProvisionKind,
    /// Whether the box starts ticked.
    pub recommended: bool,
    /// Why it is not recommended, when it is not.
    pub caution: Option<String>,
    /// Size on disk, for directories that would be linked.
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProvisionKind {
    /// Copied, because it is small and the worktree may want to diverge.
    CopyFile,
    /// Linked, because it is large and regenerable.
    LinkDir,
}

/// Everything known before a worktree is created.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePlan {
    pub branch: String,
    pub base: String,
    pub path: PathBuf,
    /// The branch is already checked out in another worktree, which git forbids.
    pub branch_in_use_at: Option<PathBuf>,
    /// The branch already exists, so it will be checked out rather than created.
    pub branch_exists: bool,
    pub path_exists: bool,
    /// The path is inside the repository. Agents then grep into it and work in
    /// the wrong tree, so it is worth warning about.
    pub path_is_nested: bool,
    pub items: Vec<ProvisionItem>,
}

impl CreatePlan {
    /// Whether creation can proceed at all.
    pub fn is_valid(&self) -> bool {
        self.branch_in_use_at.is_none() && !self.path_exists && !self.branch.is_empty()
    }
}

/// What to do when creating.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOptions {
    pub branch: String,
    pub base: String,
    pub path: PathBuf,
    /// Names from the plan's items that the user left ticked.
    pub provision: Vec<String>,
}

/// Expand a path template. `{repo}` and `{branch}` are substituted, and a
/// branch's slashes become dashes so `feat/login` does not create directories.
pub fn expand_template(template: &str, repo_root: &Path, branch: &str) -> PathBuf {
    let repo = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());

    let expanded = template
        .replace("{repo}", &repo)
        .replace("{branch}", &branch.replace('/', "-"));

    let candidate = PathBuf::from(expanded);
    if candidate.is_absolute() {
        candidate
    } else {
        // Templates are written relative to the repository, e.g. `../{repo}-worktrees`.
        normalise(&repo_root.join(candidate))
    }
}

/// Resolve `.` and `..` without touching the filesystem, since the path does
/// not exist yet and `canonicalize` would fail.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Work out what creating this worktree would involve.
pub fn plan(
    git: &Git,
    repo_root: &Path,
    branch: &str,
    base: &str,
    path: &Path,
) -> Result<CreatePlan> {
    let worktrees = list_worktrees(git, repo_root)?;

    // git refuses to check out one branch in two worktrees, so say where it is
    // rather than letting the user discover it from a failed command.
    let branch_in_use_at = worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some(branch))
        .map(|w| w.path.clone());

    let main_root = worktrees
        .first()
        .map(|w| w.path.clone())
        .unwrap_or_else(|| repo_root.to_path_buf());

    Ok(CreatePlan {
        branch: branch.to_string(),
        base: base.to_string(),
        path: path.to_path_buf(),
        branch_in_use_at,
        branch_exists: branch_exists(git, &main_root, branch),
        path_exists: path.exists(),
        path_is_nested: is_within(&main_root, path),
        items: discover_items(git, &main_root, base),
    })
}

fn branch_exists(git: &Git, root: &Path, branch: &str) -> bool {
    let spec = format!("refs/heads/{branch}");
    matches!(
        git.run_checked(root, &["show-ref", "--verify", "--quiet", &spec]),
        Ok((true, _))
    )
}

/// Find gitignored files and directories worth carrying over.
fn discover_items(git: &Git, main_root: &Path, base: &str) -> Vec<ProvisionItem> {
    let mut items = Vec::new();

    let Ok(entries) = std::fs::read_dir(main_root) else {
        return items;
    };

    let mut names: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let is_dir = e.file_type().ok()?.is_dir();
            Some((name, is_dir))
        })
        .collect();
    names.sort();

    for (name, is_dir) in &names {
        if !is_dir && is_env_file(name) {
            items.push(ProvisionItem {
                name: name.clone(),
                kind: ProvisionKind::CopyFile,
                // Small, and the whole reason the worktree would not run.
                recommended: true,
                caution: None,
                bytes: None,
            });
            continue;
        }

        if *is_dir && LINKABLE_DIRS.contains(&name.as_str()) {
            let path = main_root.join(name);
            let (recommended, caution) = link_advice(git, main_root, name, base);
            items.push(ProvisionItem {
                name: name.clone(),
                kind: ProvisionKind::LinkDir,
                recommended,
                caution,
                bytes: quick_size(&path),
            });
        }
    }

    items
}

/// Whether sharing a dependency directory is safe, and why not when it is not.
///
/// Two worktrees can share `node_modules` only if they agree on their
/// dependencies. Comparing the lockfile blob between the base and the current
/// checkout answers that exactly: identical lockfile, identical dependency
/// tree. This is what makes linking safe to default to rather than reckless.
fn link_advice(git: &Git, root: &Path, dir: &str, base: &str) -> (bool, Option<String>) {
    let Some((_, lockfiles)) = LOCKFILES.iter().find(|(name, _)| *name == dir) else {
        return (true, None);
    };

    for lockfile in *lockfiles {
        if !root.join(lockfile).exists() {
            continue;
        }
        return match lockfile_matches(git, root, lockfile, base) {
            Some(true) => (true, None),
            Some(false) => (
                false,
                Some(format!(
                    "{lockfile} differs on {base}, so the dependencies would not match"
                )),
            ),
            // Cannot tell, so do not tick it silently.
            None => (
                false,
                Some(format!("could not compare {lockfile} against {base}")),
            ),
        };
    }

    // No lockfile found: nothing to contradict sharing.
    (true, None)
}

/// Compare a lockfile between the base ref and the current checkout.
fn lockfile_matches(git: &Git, root: &Path, lockfile: &str, base: &str) -> Option<bool> {
    let base_spec = format!("{base}:{lockfile}");
    let head_spec = format!("HEAD:{lockfile}");

    let base_id = rev_parse(git, root, &base_spec)?;
    let head_id = rev_parse(git, root, &head_spec)?;
    Some(base_id == head_id)
}

fn rev_parse(git: &Git, root: &Path, spec: &str) -> Option<String> {
    let (ok, out) = git.run_checked(root, &["rev-parse", spec]).ok()?;
    if !ok {
        return None;
    }
    let id = String::from_utf8_lossy(&out).trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// Approximate directory size, read from the top level only.
///
/// A full walk of `node_modules` would make opening the dialog feel slow, and
/// the number is only there to show that linking is worth it.
fn quick_size(path: &Path) -> Option<u64> {
    let mut total = 0;
    let entries = std::fs::read_dir(path).ok()?;
    for entry in entries.flatten().take(500) {
        if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    Some(total)
}

fn is_env_file(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.")
}

/// Create the worktree and provision it.
///
/// Returns the paths of everything provisioned, so the UI can report what
/// actually happened rather than what was requested.
pub fn create(git: &Git, repo_root: &Path, opts: &CreateOptions) -> Result<Vec<String>> {
    if opts.branch.trim().is_empty() {
        return Err(Error::Parse("a branch name is required".into()));
    }
    if opts.path.exists() {
        return Err(Error::Parse(format!(
            "{} already exists",
            opts.path.display()
        )));
    }

    let worktrees = list_worktrees(git, repo_root)?;
    let main_root = worktrees
        .first()
        .map(|w| w.path.clone())
        .unwrap_or_else(|| repo_root.to_path_buf());

    if let Some(existing) = worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some(opts.branch.as_str()))
    {
        return Err(Error::Parse(format!(
            "{} is already checked out at {}",
            opts.branch,
            existing.path.display()
        )));
    }

    if let Some(parent) = opts.path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let branch_existed = branch_exists(git, &main_root, &opts.branch);
    let args = worktree_add_args(opts, branch_existed);

    if let Err(cause) = git.run(&main_root, &args) {
        let cleanup = rollback_failed_creation(
            git,
            &main_root,
            &opts.path,
            (!branch_existed).then_some(opts.branch.as_str()),
        );
        if cleanup.is_empty() {
            return Err(cause);
        }
        return Err(Error::Parse(format!(
            "{cause}; yawm also could not completely roll back the failed creation: {}",
            cleanup.join("; ")
        )));
    }

    Ok(provision(&main_root, &opts.path, &opts.provision))
}

/// Arguments for the one operation that checks out an entire tree.
///
/// `core.longpaths` is scoped to this process rather than written into the
/// repository or global config. Git for Windows can create paths beyond the
/// legacy MAX_PATH limit when enabled, and a large repository can exceed that
/// limit even when the chosen worktree root itself is short.
fn worktree_add_args(opts: &CreateOptions, branch_existed: bool) -> Vec<String> {
    let path_arg = opts.path.to_string_lossy().into_owned();
    let mut args = vec![
        "-c".into(),
        "core.longpaths=true".into(),
        "worktree".into(),
        "add".into(),
        path_arg,
    ];

    if branch_existed {
        // Already exists, so check it out rather than trying to create it.
        args.push(opts.branch.clone());
    } else {
        args.push("-b".into());
        args.push(opts.branch.clone());
        if !opts.base.trim().is_empty() {
            args.push(opts.base.clone());
        }
    }
    args
}

/// Undo everything `git worktree add` may have created before it failed.
///
/// Git can fail after registering the worktree, creating the branch, and
/// checking out thousands of files. The target did not exist before this
/// operation (checked above), so removing what the failed command created does
/// not risk pre-existing user data.
fn rollback_failed_creation(
    git: &Git,
    main_root: &Path,
    target: &Path,
    created_branch: Option<&str>,
) -> Vec<String> {
    let mut failures = Vec::new();
    let path_arg = target.to_string_lossy().into_owned();

    if target.exists()
        && git
            .run(
                main_root,
                &[
                    "-c",
                    "core.longpaths=true",
                    "worktree",
                    "remove",
                    "--force",
                    "--force",
                    path_arg.as_str(),
                ],
            )
            .is_err()
        && let Err(error) = std::fs::remove_dir_all(target)
    {
        failures.push(format!(
            "could not remove the partial directory {}: {error}",
            target.display()
        ));
    }

    if git.run(main_root, &["worktree", "prune"]).is_err() {
        failures.push("could not prune the partial worktree registration".into());
    }

    if let Some(branch) = created_branch
        && branch_exists(git, main_root, branch)
        && git.run(main_root, &["branch", "-D", branch]).is_err()
    {
        failures.push(format!(
            "could not remove the newly created branch {branch}"
        ));
    }

    failures
}

/// Copy or link the requested items into a new worktree.
///
/// Failures are skipped rather than fatal: a worktree that exists without its
/// `.env` is recoverable, but one that was created and then rolled back because
/// a link failed is just confusing.
fn provision(main_root: &Path, target: &Path, requested: &[String]) -> Vec<String> {
    let mut done = Vec::new();
    let mut linked = Vec::new();

    for name in requested {
        let source = main_root.join(name);
        let destination = target.join(name);

        if !source.exists() || destination.exists() {
            continue;
        }

        let is_dir = source.is_dir();
        let result = if is_dir {
            link_dir(&source, &destination)
        } else {
            std::fs::copy(&source, &destination).map(|_| ())
        };

        if result.is_ok() {
            done.push(name.clone());
            if is_dir {
                linked.push((name.clone(), source));
            }
        }
    }

    if !record_links(target, linked.iter().cloned()) {
        for (name, _) in &linked {
            remove_link(&target.join(name));
            done.retain(|done| done != name);
        }
    }
    done
}

fn remove_link(path: &Path) {
    if std::fs::remove_file(path).is_err() {
        let _ = std::fs::remove_dir(path);
    }
}

/// Link a directory.
///
/// Windows needs a junction rather than a symlink: directory symlinks there
/// require administrator rights or Developer Mode, while junctions require
/// neither. This is why linking works out of the box on all three platforms.
#[cfg(windows)]
fn link_dir(source: &Path, destination: &Path) -> std::io::Result<()> {
    // Junction targets must be absolute.
    let source = std::fs::canonicalize(source)?;
    junction::create(source, destination)
}

#[cfg(not(windows))]
fn link_dir(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_repo_and_branch() {
        let path = expand_template(
            "../{repo}-worktrees/{branch}",
            Path::new("/code/api"),
            "feat/login",
        );
        assert_eq!(path, PathBuf::from("/code/api-worktrees/feat-login"));
    }

    /// A branch name with slashes must not create nested directories.
    #[test]
    fn branch_slashes_become_dashes() {
        let path = expand_template("../{branch}", Path::new("/code/api"), "user/feat/x");
        assert_eq!(path, PathBuf::from("/code/user-feat-x"));
    }

    #[test]
    fn worktree_creation_enables_windows_long_paths_without_changing_config() {
        let args = worktree_add_args(
            &CreateOptions {
                branch: "user/feature".into(),
                base: "origin/main".into(),
                path: PathBuf::from(r"C:\worktrees\feature"),
                provision: Vec::new(),
            },
            false,
        );

        assert_eq!(
            &args[..5],
            [
                "-c",
                "core.longpaths=true",
                "worktree",
                "add",
                r"C:\worktrees\feature",
            ]
        );
        assert_eq!(&args[5..], ["-b", "user/feature", "origin/main"]);
    }

    #[cfg(unix)]
    #[test]
    fn failed_creation_rolls_back_the_directory_registration_and_new_branch() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            &["init", "-q", "-b", "main", "."][..],
            &["config", "user.email", "test@yawm.dev"][..],
            &["config", "user.name", "yawm test"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-qm", "base"])
                .status()
                .unwrap()
                .success()
        );

        // Run the real operation to completion, then report failure. This
        // leaves more behind than a mid-checkout error: a full directory, a
        // registered worktree, and a branch, so rollback has to clear all three.
        let wrapper = dir.path().join("git-that-fails-after-add");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\n/usr/bin/git \"$@\"\nstatus=$?\ncase \" $* \" in *\" worktree add \"*) exit 1;; esac\nexit $status\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&wrapper, permissions).unwrap();

        let target = dir.path().join("worktree");
        let git = Git::with_program(wrapper.to_string_lossy());
        create(
            &git,
            &repo,
            &CreateOptions {
                branch: "feature".into(),
                base: "main".into(),
                path: target.clone(),
                provision: Vec::new(),
            },
        )
        .expect_err("the wrapper reports the completed add as failed");

        assert!(!target.exists(), "the created directory survives rollback");
        assert!(
            !branch_exists(&Git::new(), &repo, "feature"),
            "the newly created branch survives rollback"
        );
        assert_eq!(
            list_worktrees(&Git::new(), &repo).unwrap().len(),
            1,
            "the worktree registration survives rollback"
        );
    }

    #[test]
    fn absolute_templates_are_used_as_given() {
        let path = expand_template("/tmp/wt/{branch}", Path::new("/code/api"), "x");
        assert_eq!(path, PathBuf::from("/tmp/wt/x"));
    }

    #[test]
    fn parent_references_are_resolved() {
        let path = expand_template("../../elsewhere/{branch}", Path::new("/a/b/c"), "x");
        assert_eq!(path, PathBuf::from("/a/elsewhere/x"));
    }

    #[test]
    fn recognises_env_files() {
        assert!(is_env_file(".env"));
        assert!(is_env_file(".env.production"));
        assert!(!is_env_file(".envrc"));
        assert!(!is_env_file("environment"));
    }

    #[test]
    fn a_plan_missing_a_branch_is_invalid() {
        let plan = CreatePlan::default();
        assert!(!plan.is_valid());
    }

    #[test]
    fn a_plan_whose_branch_is_checked_out_elsewhere_is_invalid() {
        let plan = CreatePlan {
            branch: "feat/x".into(),
            branch_in_use_at: Some("/somewhere".into()),
            ..Default::default()
        };
        assert!(!plan.is_valid());
    }

    #[test]
    fn a_plan_whose_path_exists_is_invalid() {
        let plan = CreatePlan {
            branch: "feat/x".into(),
            path_exists: true,
            ..Default::default()
        };
        assert!(!plan.is_valid());
    }

    #[test]
    fn a_complete_plan_is_valid() {
        let plan = CreatePlan {
            branch: "feat/x".into(),
            ..Default::default()
        };
        assert!(plan.is_valid());
    }

    #[test]
    fn every_linkable_directory_has_lockfile_guidance() {
        for dir in LINKABLE_DIRS {
            assert!(
                LOCKFILES.iter().any(|(name, _)| name == dir),
                "{dir} can be linked but has no lockfile rule, so it would always be recommended"
            );
        }
    }
}
