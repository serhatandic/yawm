//! Destructive and creative operations on worktrees.
//!
//! Removal is the point of the tool, so it has to be trustworthy. Two rules
//! shape everything here:
//!
//! 1. Removal always goes through `git worktree remove`, never a recursive
//!    delete, so git's administrative data stays consistent.
//! 2. Nothing is ever forced implicitly. When git refuses, the caller receives
//!    a [`RemovalPlan`] describing exactly what would be lost and must ask for
//!    force explicitly.

pub mod create;
pub mod editors;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::Git;
use crate::git::collect::{
    BranchContext, CollectedStatus, DirtyScan, EnvScan, collect_status, env_candidates,
    head_reference, inspectable, list_worktrees, load_branch_context, scan_dirty,
    scan_dirty_for_worktree,
};
use crate::git::status::{blob_oid, digest_hex, path_from_git};
use crate::model::{ManagedDependencyLink, Worktree, WorktreeEntry, WorktreeStatus};
use crate::path::path_key;
use crate::process;

/// What removing a worktree would cost.
///
/// Built before anything is deleted so the user sees the consequences first.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalPlan {
    pub path: PathBuf,
    pub branch: Option<String>,
    /// The main worktree can never be removed.
    pub is_main: bool,
    pub is_locked: bool,
    pub lock_reason: Option<String>,
    /// Directory already gone; only metadata needs pruning.
    pub is_prunable: bool,
    /// Files that would be destroyed, capped for display.
    pub dirty_files: Vec<String>,
    pub dirty_total: usize,
    /// Commits that exist nowhere else.
    pub unpushed_commits: usize,
    /// Untracked environment files with no matching main-worktree copy.
    /// Losing their current contents is the most common real deletion harm.
    pub env_files: Vec<String>,
    /// Exact yawm-created dependency links that survive removal harmlessly.
    #[serde(default)]
    pub managed_dependency_links: Vec<ManagedDependencyLink>,
    /// Processes currently running inside.
    pub running_processes: usize,
    /// git will refuse without `--force`, because of what is in the directory.
    ///
    /// A lock deliberately does not set this. Git refuses a locked worktree
    /// too, but it refuses it for a reason somebody stated on purpose, and
    /// that reason is answered by [`RemoveOptions::unlock`] rather than by a
    /// confirmation about uncommitted files. Folding the two together is
    /// exactly how ticking "I understand, delete it anyway" over a list of
    /// edited files came to delete a worktree locked with "agent running".
    pub requires_force: bool,
    /// The exact state this plan describes, uncapped.
    ///
    /// Everything above it is for a person to read and is capped accordingly.
    /// This is what the authorisation is actually against, and it is compared
    /// whole: the fields above cannot tell a replaced fifty-first dirty file,
    /// a rewritten one of the same name, an amended unpushed commit, or a
    /// branch ref moved under the worktree from the state that was approved,
    /// because none of those move a count or a name.
    ///
    /// Defaulted on the wire so a payload without one is refused rather than
    /// misread: an empty fingerprint matches no real worktree.
    #[serde(default)]
    pub state: StateFingerprint,
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn canonical_path_identity(path: &Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    });
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hex_bytes(path.as_os_str().as_bytes())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let bytes: Vec<u8> = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        hex_bytes(&bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        hex_bytes(path.to_string_lossy().as_bytes())
    }
}

impl RemovalPlan {
    /// Whether removal may be offered at all.
    pub fn is_allowed(&self) -> bool {
        !self.is_main
    }

    /// Whether anything irreplaceable would be destroyed.
    pub fn destroys_work(&self) -> bool {
        self.dirty_total > 0 || self.unpushed_commits > 0 || !self.env_files.is_empty()
    }
}

/// How to remove a worktree.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOptions {
    /// Proceed even though uncommitted work would be destroyed. The caller must
    /// have shown the user a [`RemovalPlan`] first.
    ///
    /// This authorises losing the *directory's* uncommitted files. It does not
    /// authorise losing commits — see `force_branch` — and it does not
    /// authorise lifting a lock — see `unlock`.
    pub force: bool,
    /// Also delete the branch the worktree had checked out.
    pub delete_branch: bool,
    /// Delete that branch even when git says it holds unmerged commits.
    ///
    /// Deliberately separate from `force`. They authorise different losses:
    /// one throws away edits you can see in the directory, the other throws
    /// away commits that may exist nowhere else. Reusing a single flag meant
    /// confirming "yes, discard my uncommitted files" quietly promoted
    /// `git branch -d` to `-D` and destroyed history nobody was asked about.
    pub force_branch: bool,
    /// Move the directory to the OS trash instead of deleting it outright,
    /// leaving a recoverable copy.
    pub use_trash: bool,
    /// Lift the worktree's lock and remove it anyway.
    ///
    /// A lock is a person or an agent saying "not this one", usually with a
    /// reason attached, and it is the only thing in a plan that was put there
    /// deliberately rather than observed. `force` used to carry it: passing a
    /// second `--force` was how git's refusal was silenced, so agreeing to
    /// discard some uncommitted files also removed a worktree that had been
    /// locked with the reason "agent running" — a question nobody was asked.
    ///
    /// It is its own flag for the same reason `force_branch` is. When set, the
    /// lock is lifted explicitly with `git worktree unlock` immediately before
    /// removal and only after the plan has been re-checked, so the lock that
    /// goes is the lock the caller was shown.
    #[serde(default)]
    pub unlock: bool,
}

/// Maximum file names carried in a plan; enough to inform, not enough to flood.
const MAX_LISTED_FILES: usize = 50;

/// How deep a dirty directory's own repository state is followed.
///
/// Submodules nest, and a walk that follows every level is unbounded work on a
/// path that runs immediately before a deletion. Past this depth the state is
/// declared unproven rather than assumed unchanged.
const MAX_SUBMODULE_DEPTH: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataEvidence {
    kind: u8,
    len: u64,
    readonly: bool,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    mtime: i64,
    #[cfg(unix)]
    mtime_nsec: i64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl MetadataEvidence {
    fn of(metadata: &Metadata) -> Self {
        let file_type = metadata.file_type();
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        Self {
            kind: if file_type.is_file() {
                1
            } else if file_type.is_dir() {
                2
            } else if file_type.is_symlink() {
                3
            } else {
                4
            },
            len: metadata.len(),
            readonly: metadata.permissions().readonly(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
            #[cfg(unix)]
            mode: metadata.mode(),
            #[cfg(unix)]
            mtime: metadata.mtime(),
            #[cfg(unix)]
            mtime_nsec: metadata.mtime_nsec(),
            #[cfg(unix)]
            ctime: metadata.ctime(),
            #[cfg(unix)]
            ctime_nsec: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContentEvidence {
    path: PathBuf,
    label: String,
    depth: usize,
    oid_len: usize,
    identity: String,
    metadata: Option<MetadataEvidence>,
}

/// What the fingerprint covers, and what it deliberately does not.
///
/// It covers, exactly: every path git reports as uncommitted (including each
/// index stage of a conflicted one and the bytes in the working tree), the
/// bounded repository state of any dirty path that is a submodule, and every
/// environment-shaped file — `.env`, `.env.local`, and their siblings — found
/// in the directory, whether or not git ignores them. An ignored `.env*` is
/// included exactly like any other: being ignored is what makes it unique and
/// unrecoverable, not what makes it disposable.
///
/// It does not cover arbitrary ignored artifacts: `node_modules`, `target`,
/// `dist`, and everything else `.gitignore` names. That is the same scope
/// removal planning uses, and it is a deliberate policy rather than an
/// oversight. Those bytes were already disposable before anyone was asked —
/// the plan the user approved never claimed to preserve them, and rebuilding
/// them is what they are for — so a change among them is not a newly
/// authorised loss and must not invalidate an authorisation. Enumerating or
/// hashing such trees would also mean walking hundreds of thousands of files
/// immediately before a deletion, which is not work this path can afford.
///
/// So a fingerprint is not a claim about every byte under the directory. It is
/// a claim about everything a removal was described as destroying.
///
/// [`StateEvidence::unproven`] is what keeps that a claim rather than a hope:
/// anything in scope that could not be read lands there and refuses.
///
/// `blob:<oid>` is git's own object name for the bytes that are there now, so
/// a file rewritten with different contents under the same name is a different
/// token. `submodule:<digest>` is the bounded state of a nested repository.
/// `absent` was looked for and is not there. `unknown` is the one case where
/// the inspection itself failed, and is recorded as unproven as well.
fn content_identity(
    git: &Git,
    path: &Path,
    oid_len: usize,
    unproven: &mut Vec<String>,
    label: &str,
    depth: usize,
    captured: &mut Vec<ContentEvidence>,
) -> String {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
            captured.push(ContentEvidence {
                path: path.to_path_buf(),
                label: label.to_string(),
                depth,
                oid_len,
                identity: "absent".to_string(),
                metadata: None,
            });
            return "absent".to_string();
        }
        Err(failure) => {
            unproven.push(format!("{label} could not be read ({failure})"));
            return "unknown".to_string();
        }
    };
    let before = MetadataEvidence::of(&metadata);
    let identity = if metadata.is_dir() {
        directory_identity(git, path, unproven, label, depth, captured)
    } else {
        match blob_oid(path, oid_len) {
            Ok(oid) => format!("blob:{oid}"),
            Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => "absent".to_string(),
            Err(failure) => {
                unproven.push(format!("{label} could not be read ({failure})"));
                "unknown".to_string()
            }
        }
    };
    if identity == "unknown" {
        return identity;
    }

    let after = match std::fs::symlink_metadata(path) {
        Ok(metadata) => Some(MetadataEvidence::of(&metadata)),
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => None,
        Err(failure) => {
            unproven.push(format!(
                "{label} could not be checked after it was read ({failure})"
            ));
            return "unknown".to_string();
        }
    };
    if after.as_ref() != Some(&before) {
        unproven.push(format!(
            "{label} changed while its contents were being read"
        ));
        return "unknown".to_string();
    }
    captured.push(ContentEvidence {
        path: path.to_path_buf(),
        label: label.to_string(),
        depth,
        oid_len,
        identity: identity.clone(),
        metadata: after,
    });
    identity
}

/// Identify a dirty path that is a directory.
///
/// Every directory used to answer with the same token, so a submodule whose
/// entire contents were rewritten between the dialog opening and the click was
/// indistinguishable from one nobody touched, and a force authorisation carried
/// straight over it.
///
/// A submodule is a git repository, so its state is bounded and can be named
/// exactly: its checked-out commit plus its own uncommitted paths, hashed. Any
/// other directory has no bound at all, and no depth of walking would give one,
/// so it is declared unproven and the removal fails closed.
fn directory_identity(
    git: &Git,
    path: &Path,
    unproven: &mut Vec<String>,
    label: &str,
    depth: usize,
    captured: &mut Vec<ContentEvidence>,
) -> String {
    if depth >= MAX_SUBMODULE_DEPTH {
        unproven.push(format!(
            "{label} nests repositories deeper than yawm inspects, so its contents are unaccounted for"
        ));
        return "unknown".to_string();
    }
    if std::fs::symlink_metadata(path.join(".git")).is_err() {
        unproven.push(format!(
            "{label} is a directory whose contents could not be identified"
        ));
        return "unknown".to_string();
    }

    // `--verify --quiet` distinguishes the two answers that are not failures:
    // a commit, and a repository with no commits at all.
    let head = match git.run_status(path, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
        Ok(out) if out.code == Some(0) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(out) if out.code == Some(1) => "unborn".to_string(),
        Ok(out) => {
            unproven.push(format!(
                "{label} did not say what it has checked out ({})",
                out.code
                    .map_or_else(|| "no exit code".to_string(), |c| c.to_string())
            ));
            return "unknown".to_string();
        }
        Err(failure) => {
            unproven.push(format!(
                "{label} did not say what it has checked out ({failure})"
            ));
            return "unknown".to_string();
        }
    };

    let scan = scan_dirty(git, path);
    if !scan.unproven.is_empty() {
        for reason in &scan.unproven {
            unproven.push(format!("{label}: {reason}"));
        }
        return "unknown".to_string();
    }

    let inner_len = length_of(Some(head.as_str()))
        .or_else(|| oid_length_of_index(&scan))
        .unwrap_or(40);

    let mut canonical = String::from("yawm.submodule.v1\n");
    push_field(&mut canonical, "head", &head);
    push_field(&mut canonical, "dirty.count", &scan.paths.len().to_string());
    let mut inner_unproven = Vec::new();
    for entry in &scan.paths {
        push_field(&mut canonical, "dirty.path", &entry.path);
        push_field(
            &mut canonical,
            "dirty.path.raw",
            &hex_bytes(&entry.raw_path),
        );
        push_field(&mut canonical, "dirty.codes", &entry.codes.join(","));
        push_field(&mut canonical, "dirty.stages", &entry.stages.join(","));
        push_field(
            &mut canonical,
            "dirty.content",
            &content_identity(
                git,
                &path.join(path_from_git(&entry.raw_path)),
                inner_len,
                &mut inner_unproven,
                &format!("{label}/{}", entry.path),
                depth + 1,
                captured,
            ),
        );
    }
    if !inner_unproven.is_empty() {
        unproven.extend(inner_unproven);
        return "unknown".to_string();
    }

    format!("submodule:{}", digest_hex(canonical.as_bytes()))
}

fn revalidate_content_evidence(
    git: &Git,
    captured: &[ContentEvidence],
    unproven: &mut Vec<String>,
) {
    for expected in captured {
        let mut verification_failures = Vec::new();
        let mut observed = Vec::new();
        let identity = content_identity(
            git,
            &expected.path,
            expected.oid_len,
            &mut verification_failures,
            &expected.label,
            expected.depth,
            &mut observed,
        );
        let current = observed.last();
        if !verification_failures.is_empty()
            || identity != expected.identity
            || current.is_none_or(|current| {
                current.path != expected.path
                    || current.identity != expected.identity
                    || current.metadata != expected.metadata
            })
        {
            unproven.push(format!(
                "{} changed while the rest of the removal snapshot was being read",
                expected.label
            ));
        }
    }
    unproven.sort();
    unproven.dedup();
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeDirectoryIdentity {
    kind: u8,
    created: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

fn worktree_directory_identity(path: &Path) -> io::Result<Option<WorktreeDirectoryIdentity>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            let evidence = MetadataEvidence::of(&metadata);
            Ok(Some(WorktreeDirectoryIdentity {
                kind: evidence.kind,
                created: evidence.created,
                #[cfg(unix)]
                dev: evidence.dev,
                #[cfg(unix)]
                ino: evidence.ino,
            }))
        }
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(failure) => Err(failure),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct NestedRepositoryScan {
    repositories: Vec<String>,
    unreadable: Vec<String>,
}

/// Find repository boundaries below a worktree without entering them.
///
/// Git status deliberately treats nested repositories as atomic and omits
/// ignored directories altogether. Neither answer authorises deleting the
/// nested object database, so removal takes an independent filesystem
/// inventory. Symlinks are observed but never followed: a link inside a
/// worktree must not turn this safety check into a traversal of an arbitrary
/// external directory.
fn nested_repositories(root: &Path) -> NestedRepositoryScan {
    let mut scan = NestedRepositoryScan::default();
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        let is_root = directory == root;
        if !is_root && is_repository_boundary(&directory) {
            scan.repositories.push(relative_label(root, &directory));
            continue;
        }

        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(failure) => {
                scan.unreadable.push(format!(
                    "{} could not be inspected for nested repositories ({failure})",
                    relative_label(root, &directory)
                ));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(failure) => {
                    scan.unreadable.push(format!(
                        "{} contained an entry that could not be inspected for nested repositories ({failure})",
                        relative_label(root, &directory)
                    ));
                    continue;
                }
            };
            if is_root && entry.file_name() == OsStr::new(".git") {
                continue;
            }
            let path = entry.path();
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_dir() => pending.push(path),
                Ok(_) => {}
                Err(failure) => scan.unreadable.push(format!(
                    "{} could not be inspected for nested repositories ({failure})",
                    relative_label(root, &path)
                )),
            }
        }
    }

    scan.repositories.sort();
    scan.repositories.dedup();
    scan.unreadable.sort();
    scan.unreadable.dedup();
    scan
}

fn is_repository_boundary(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join(".git")).is_ok()
        || (path.join("HEAD").is_file()
            && path.join("objects").is_dir()
            && path.join("refs").is_dir())
}

fn relative_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn same_worktree_identity(
    expected: &WorktreeEntry,
    current: &WorktreeEntry,
    expected_path: &str,
) -> bool {
    canonical_path_identity(&current.path) == expected_path
        && current.head == expected.head
        && current.branch == expected.branch
        && current.detached == expected.detached
        && current.bare == expected.bare
        && current.is_main == expected.is_main
        && current.locked == expected.locked
        && current.prunable == expected.prunable
}

struct SnapshotBaseline<'a> {
    repository: &'a Path,
    entry: &'a WorktreeEntry,
    dirty: &'a DirtyScan,
    env: &'a EnvScan,
    present: bool,
    path: &'a str,
    directory: &'a io::Result<Option<WorktreeDirectoryIdentity>>,
    nested_repositories: &'a NestedRepositoryScan,
}

fn revalidate_snapshot_inventories(
    git: &Git,
    baseline: SnapshotBaseline<'_>,
    unproven: &mut Vec<String>,
) {
    let present = inspectable(baseline.entry);
    if present != baseline.present {
        unproven.push(
            "the worktree appeared or disappeared while the removal snapshot was being read"
                .to_string(),
        );
    }

    if present {
        let mut managed = Vec::new();
        if scan_dirty_for_worktree(git, &baseline.entry.path, baseline.repository, &mut managed)
            != *baseline.dirty
        {
            unproven.push(
                "its uncommitted-file inventory changed while the removal snapshot was being read"
                    .to_string(),
            );
        }
        if env_candidates(&baseline.entry.path) != *baseline.env {
            unproven.push(
                "its environment-file inventory changed while the removal snapshot was being read"
                    .to_string(),
            );
        }
        if nested_repositories(&baseline.entry.path) != *baseline.nested_repositories {
            unproven.push(
                "its nested-repository inventory changed while the removal snapshot was being read"
                    .to_string(),
            );
        }
    }

    match (
        baseline.directory,
        worktree_directory_identity(&baseline.entry.path),
    ) {
        (Ok(expected), Ok(current)) if expected == &current => {}
        _ => unproven.push(
            "the worktree directory changed identity while the removal snapshot was being read"
                .to_string(),
        ),
    }

    match list_worktrees(git, baseline.repository) {
        Ok(entries)
            if entries
                .iter()
                .any(|current| {
                    same_worktree_identity(baseline.entry, current, baseline.path)
                }) => {}
        Ok(_) => unproven.push(
            "the registered worktree changed identity while the removal snapshot was being read"
                .to_string(),
        ),
        Err(failure) => unproven.push(format!(
            "the registered worktree could not be rechecked after the removal snapshot was read ({failure})"
        )),
    }

    unproven.sort();
    unproven.dedup();
}

/// One field of a canonical encoding, length-prefixed.
///
/// The length precedes anything a value could contain, so no combination of
/// paths, lock reasons, or git messages can be arranged to encode as some
/// other combination.
fn push_field(out: &mut String, name: &str, value: &str) {
    out.push_str(&format!("{name}\u{1}{}\u{1}{value}\n", value.len()));
}

fn push_optional(out: &mut String, name: &str, value: &Option<String>) {
    match value {
        Some(value) => out.push_str(&format!(
            "{name}\u{1}some\u{1}{}\u{1}{value}\n",
            value.len()
        )),
        None => out.push_str(&format!("{name}\u{1}none\n")),
    }
}

/// One uncommitted path, identified exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtyIdentity {
    pub path: String,
    /// git's own `XY` status pairs for it, sorted.
    pub codes: Vec<String>,
    /// Every index entry for it as `<stage> <mode> <oid>`, sorted.
    ///
    /// A path in conflict has one per stage — ancestor, ours, theirs — and each
    /// names different bytes, so keeping only one of them made two different
    /// resolutions of the same conflict look identical.
    pub stages: Vec<String>,
    /// What is in the working tree right now — see [`content_identity`].
    pub content: String,
}

/// One file outside git, identified exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileIdentity {
    pub path: String,
    pub content: String,
}

/// Version tag of the fingerprint encoding.
///
/// Carried in the payload so a plan produced by a different build of yawm is
/// recognised as unreadable rather than compared field by field with one that
/// means something else.
pub const STATE_VERSION: &str = "yawm.state.v5";

/// The exact, uncapped state a removal was authorised against.
///
/// Never serialized. It exists while a plan is being built and while it is
/// being re-checked, both inside this process, and what crosses to the frontend
/// is [`StateFingerprint`] — a digest over all of it. A webview does not need
/// the identity of every dirty file to hand an authorisation back, and sending
/// it meant an unbounded payload: a worktree with ten thousand modified files
/// put ten thousand records through the IPC boundary twice.
///
/// See [`content_identity`] for exactly what is in scope and what is not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateEvidence {
    /// Canonical, lossless identities of the repository and this worktree.
    pub repository: String,
    pub worktree_path: String,
    /// Every uncommitted path, uncapped and sorted.
    pub dirty: Vec<DirtyIdentity>,
    /// Lossless git path bytes, positionally paired with `dirty`.
    pub dirty_raw_paths: Vec<Vec<u8>>,
    /// Every environment-shaped file in the directory, uncapped and sorted —
    /// not the filtered list the plan displays.
    pub env: Vec<FileIdentity>,
    pub managed_dependency_links: Vec<ManagedDependencyLink>,
    /// The commit the worktree has checked out.
    pub head: Option<String>,
    pub branch: Option<String>,
    /// The commit the branch's ref points at.
    pub branch_oid: Option<String>,
    pub upstream: Option<String>,
    /// The upstream's full ref name, e.g. `refs/remotes/origin/feat/auth`.
    ///
    /// The short name cannot be verified: it is what a person reads, and a
    /// fetch refspec may put the ref it abbreviates in any namespace.
    pub upstream_ref: Option<String>,
    pub upstream_oid: Option<String>,
    /// The full name of the ref a deletion of this branch is decided against —
    /// the configured upstream, or the ref HEAD resolves to.
    pub merge_ref: Option<String>,
    /// The commit that ref held when this state was read.
    ///
    /// Deliberately left out of the canonical encoding below: it is a fact
    /// about the rest of the repository rather than about this worktree, and a
    /// commit landing on `main` must not invalidate an approved removal of an
    /// unrelated worktree. Nothing is lost by that — the deletion verifies this
    /// exact ref against this exact commit inside its own ref transaction, so
    /// a merge reference that moved refuses there rather than here.
    pub merge_oid: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub upstream_gone: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    /// git's own words for why the administrative entry is prunable.
    pub prunable: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub is_main: bool,
    /// Whether there was a directory to inspect at all.
    pub directory_present: bool,
    /// Everything the inspection could not establish. Empty is the only value
    /// that permits a removal to go ahead.
    pub unproven: Vec<String>,
}

impl StateEvidence {
    /// Every field of this worktree's state, in a form where no two different
    /// states encode the same.
    ///
    /// `merge_oid` is the one deliberate omission, for the reason given on the
    /// field itself: it describes the rest of the repository, and it is proven
    /// where it is used rather than here.
    fn canonical(&self) -> String {
        let mut out = String::from(STATE_VERSION);
        out.push('\n');

        push_field(&mut out, "repository", &self.repository);
        push_field(&mut out, "worktree.path", &self.worktree_path);
        push_field(&mut out, "dirty.count", &self.dirty.len().to_string());
        for (index, entry) in self.dirty.iter().enumerate() {
            push_field(&mut out, "dirty.path", &entry.path);
            let raw = self
                .dirty_raw_paths
                .get(index)
                .map(|path| hex_bytes(path))
                .unwrap_or_else(|| hex_bytes(entry.path.as_bytes()));
            push_field(&mut out, "dirty.path.raw", &raw);
            push_field(&mut out, "dirty.codes", &entry.codes.join(","));
            push_field(
                &mut out,
                "dirty.stage.count",
                &entry.stages.len().to_string(),
            );
            for stage in &entry.stages {
                push_field(&mut out, "dirty.stage", stage);
            }
            push_field(&mut out, "dirty.content", &entry.content);
        }
        push_field(&mut out, "env.count", &self.env.len().to_string());
        for entry in &self.env {
            push_field(&mut out, "env.path", &entry.path);
            push_field(&mut out, "env.content", &entry.content);
        }
        push_field(
            &mut out,
            "managed.count",
            &self.managed_dependency_links.len().to_string(),
        );
        for link in &self.managed_dependency_links {
            push_field(&mut out, "managed.path", &link.path);
            push_field(
                &mut out,
                "managed.target",
                &canonical_path_identity(&link.target),
            );
        }
        push_optional(&mut out, "head", &self.head);
        push_optional(&mut out, "branch", &self.branch);
        push_optional(&mut out, "branch.oid", &self.branch_oid);
        push_optional(&mut out, "upstream", &self.upstream);
        push_optional(&mut out, "upstream.ref", &self.upstream_ref);
        push_optional(&mut out, "upstream.oid", &self.upstream_oid);
        push_optional(&mut out, "merge.ref", &self.merge_ref);
        push_field(&mut out, "ahead", &self.ahead.to_string());
        push_field(&mut out, "behind", &self.behind.to_string());
        push_field(&mut out, "upstream.gone", &self.upstream_gone.to_string());
        push_field(&mut out, "locked", &self.locked.to_string());
        push_optional(&mut out, "lock.reason", &self.lock_reason);
        push_optional(&mut out, "prunable", &self.prunable);
        push_field(&mut out, "detached", &self.detached.to_string());
        push_field(&mut out, "bare", &self.bare.to_string());
        push_field(&mut out, "main", &self.is_main.to_string());
        push_field(&mut out, "directory", &self.directory_present.to_string());
        push_field(&mut out, "unproven.count", &self.unproven.len().to_string());
        for reason in &self.unproven {
            push_field(&mut out, "unproven", reason);
        }
        out
    }

    /// The bounded fingerprint that crosses process boundaries.
    pub fn seal(self) -> StateFingerprint {
        StateFingerprint {
            version: STATE_VERSION.to_string(),
            digest: digest_hex(self.canonical().as_bytes()),
            unproven: !self.unproven.is_empty(),
            evidence: Some(Box::new(self)),
        }
    }
}

/// The proof that the worktree about to be deleted is the one the user saw.
///
/// Three fixed-size fields, whatever the worktree contains: a version, a
/// digest over [`StateEvidence`], and whether anything in scope could not be
/// read. That is everything an authorisation needs to be handed back with, and
/// the size of a plan no longer depends on how much work is in the worktree.
///
/// The evidence itself stays in this process. It is attached to plans built
/// here and dropped by any round trip through serialization, which is what a
/// returning authorisation is — so a plan that came back from a frontend has a
/// digest and nothing else, exactly as intended.
///
/// `unproven` is what makes the digest a proof rather than an assumption.
/// Anything the inspection could not read sets it, and a fingerprint carrying
/// it is never accepted — including against an identical copy of itself — so no
/// force authorisation can be reused over a state nobody established.
///
/// A default value — an absent field, or a payload from a build that predates
/// this shape — has an empty version and fails closed: it is never equal to a
/// state anyone approved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateFingerprint {
    /// [`STATE_VERSION`], or empty for a fingerprint that was never built.
    #[serde(default)]
    pub version: String,
    /// sha256 over the canonical encoding of the evidence.
    pub digest: String,
    /// Whether anything in scope could not be established.
    #[serde(default)]
    pub unproven: bool,
    /// Present only in the process that built it — see the type's note.
    #[serde(skip)]
    evidence: Option<Box<StateEvidence>>,
}

impl StateFingerprint {
    /// Build the fingerprint for one worktree from facts already gathered.
    fn of(
        git: &Git,
        repository: &Path,
        entry: &WorktreeEntry,
        status: &WorktreeStatus,
        dirty: &DirtyScan,
    ) -> Self {
        Self::of_after_snapshot(git, repository, entry, status, dirty, &mut || {})
    }

    fn of_after_snapshot(
        git: &Git,
        repository: &Path,
        entry: &WorktreeEntry,
        status: &WorktreeStatus,
        dirty: &DirtyScan,
        after_snapshot: &mut dyn FnMut(),
    ) -> Self {
        let mut unproven = dirty.unproven.clone();
        let mut captured = Vec::new();
        let present = inspectable(entry);
        let oid_len = oid_length(status, entry, dirty);
        let expected_path = canonical_path_identity(&entry.path);
        let initial_directory = worktree_directory_identity(&entry.path);
        let nested = if present {
            nested_repositories(&entry.path)
        } else {
            NestedRepositoryScan::default()
        };
        for repository in &nested.repositories {
            unproven.push(format!(
                "{repository} is a nested Git repository whose object database removal was not authorised"
            ));
        }
        unproven.extend(nested.unreadable.iter().cloned());

        if status.upstream.unresolved {
            unproven.push(match &status.upstream.name {
                Some(name) => format!("the commit {name} points at could not be read"),
                None => "the commit its upstream points at could not be read".to_string(),
            });
        }

        let dirty_identities: Vec<DirtyIdentity> = dirty
            .paths
            .iter()
            .map(|path| DirtyIdentity {
                content: content_identity(
                    git,
                    &entry.path.join(path_from_git(&path.raw_path)),
                    oid_len,
                    &mut unproven,
                    &path.path,
                    0,
                    &mut captured,
                ),
                path: path.path.clone(),
                codes: path.codes.clone(),
                stages: path.stages.clone(),
            })
            .collect();

        let env_scan = if present {
            env_candidates(&entry.path)
        } else {
            EnvScan::default()
        };
        if present && !env_scan.complete {
            unproven.push(
                "its files outside git could not all be listed, so some are unaccounted for"
                    .to_string(),
            );
        }
        let env = if present {
            env_scan
                .files
                .iter()
                .map(|relative| FileIdentity {
                    content: content_identity(
                        git,
                        &entry.path.join(relative),
                        oid_len,
                        &mut unproven,
                        relative,
                        0,
                        &mut captured,
                    ),
                    path: relative.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut evidence = StateEvidence {
            repository: canonical_path_identity(repository),
            worktree_path: expected_path.clone(),
            dirty_raw_paths: dirty
                .paths
                .iter()
                .map(|path| path.raw_path.clone())
                .collect(),
            dirty: dirty_identities,
            env,
            managed_dependency_links: status.managed_dependency_links.clone(),
            head: entry.head.clone(),
            branch: entry.branch.clone(),
            branch_oid: status.branch_oid.clone(),
            upstream: status.upstream.name.clone(),
            upstream_ref: status.upstream.full_ref.clone(),
            upstream_oid: status.upstream.oid.clone(),
            merge_ref: status
                .merge_ref
                .as_ref()
                .map(|reference| reference.name.clone()),
            merge_oid: status
                .merge_ref
                .as_ref()
                .map(|reference| reference.oid.clone()),
            ahead: status.upstream.ahead,
            behind: status.upstream.behind,
            upstream_gone: status.upstream.gone,
            locked: entry.locked.is_some(),
            lock_reason: entry.locked.as_ref().and_then(|lock| lock.reason.clone()),
            prunable: entry.prunable.clone(),
            detached: entry.detached,
            bare: entry.bare,
            is_main: entry.is_main,
            directory_present: present,
            unproven,
        };
        after_snapshot();
        revalidate_content_evidence(git, &captured, &mut evidence.unproven);
        revalidate_snapshot_inventories(
            git,
            SnapshotBaseline {
                repository,
                entry,
                dirty,
                env: &env_scan,
                present,
                path: &expected_path,
                directory: &initial_directory,
                nested_repositories: &nested,
            },
            &mut evidence.unproven,
        );
        evidence.seal()
    }

    /// The detail behind the digest, when this fingerprint was built here.
    ///
    /// `None` after a round trip through serialization, which carries the
    /// digest and nothing else.
    pub fn evidence(&self) -> Option<&StateEvidence> {
        self.evidence.as_deref()
    }

    /// Whether this names a state that was actually established.
    pub fn is_proven(&self) -> bool {
        self.version == STATE_VERSION && !self.unproven && !self.digest.is_empty()
    }
}

/// How long this repository's object names are.
///
/// A sha256 repository's index and refs speak in 64 hex characters, and hashing
/// a file with the wrong algorithm would produce a token that never matches.
/// Read off what git has already said rather than assumed.
fn oid_length(status: &WorktreeStatus, entry: &WorktreeEntry, dirty: &DirtyScan) -> usize {
    oid_length_of_index(dirty)
        .or_else(|| length_of(entry.head.as_deref()))
        .or_else(|| length_of(status.branch_oid.as_deref()))
        .unwrap_or(40)
}

/// The object-name length the index itself speaks in, from any staged entry.
fn oid_length_of_index(dirty: &DirtyScan) -> Option<usize> {
    dirty
        .paths
        .iter()
        .flat_map(|path| path.stages.iter())
        .filter_map(|stage| stage.split_whitespace().nth(2))
        .map(str::len)
        .find(|len| *len == 40 || *len == 64)
}

fn length_of(oid: Option<&str>) -> Option<usize> {
    oid.map(str::len).filter(|len| *len == 40 || *len == 64)
}

/// Describe what removing this worktree would cost.
pub fn plan_removal(git: &Git, entry: &WorktreeEntry, status: &WorktreeStatus) -> RemovalPlan {
    let repository = repository_identity_root(git, entry);
    let dirty = if inspectable(entry) {
        let mut managed = Vec::new();
        scan_dirty_for_worktree(git, &entry.path, &repository, &mut managed)
    } else {
        DirtyScan::default()
    };
    build_plan(git, &repository, entry, status, &dirty)
}

fn repository_identity_root(git: &Git, entry: &WorktreeEntry) -> PathBuf {
    if let Some(repository) = &entry.repository {
        return repository.clone();
    }
    if entry.is_main {
        return entry.path.clone();
    }
    if let Ok(entries) = list_worktrees(git, &entry.path)
        && let Some(main) = entries.into_iter().find(|candidate| candidate.is_main)
    {
        return main.path;
    }
    let mut ancestor = entry.path.parent();
    while let Some(directory) = ancestor {
        if let Ok(entries) = list_worktrees(git, directory)
            && let Some(main) = entries.into_iter().find(|candidate| candidate.is_main)
        {
            return main.path;
        }
        ancestor = directory.parent();
    }
    entry.path.clone()
}

/// Assemble the plan from facts already collected.
///
/// Every field comes from `entry`, `status`, or `dirty`, and there is nothing
/// else a plan reads — no size, no landing, no verdict. That is the whole
/// reason [`RemovalPlanner`] can skip them.
fn build_plan(
    git: &Git,
    repository: &Path,
    entry: &WorktreeEntry,
    status: &WorktreeStatus,
    dirty: &DirtyScan,
) -> RemovalPlan {
    RemovalPlan {
        path: entry.path.clone(),
        branch: entry.branch.clone(),
        is_main: entry.is_main,
        is_locked: entry.locked.is_some(),
        lock_reason: entry.locked.as_ref().and_then(|l| l.reason.clone()),
        is_prunable: entry.prunable.is_some(),
        dirty_total: status.dirty.total(),
        dirty_files: dirty
            .paths
            .iter()
            .map(|path| path.path.clone())
            .take(MAX_LISTED_FILES)
            .collect(),
        unpushed_commits: status.upstream.ahead,
        env_files: status.env_files.clone(),
        managed_dependency_links: status.managed_dependency_links.clone(),
        running_processes: status.processes.len(),
        // Git cannot protect ignored unique files, and an inspection failure
        // cannot safely grant the same permission as a known-clean result.
        // A lock is not in here: see the field's own note.
        requires_force: status.dirty.is_dirty()
            || status.dirty.is_unknown()
            || !status.env_files.is_empty(),
        state: StateFingerprint::of(git, repository, entry, status, dirty),
    }
}

/// Convenience wrapper for an already-classified worktree.
pub fn plan_removal_for(git: &Git, worktree: &Worktree) -> RemovalPlan {
    plan_removal(git, &worktree.entry, &worktree.status)
}

/// Everything a removal plan reads about one repository, gathered once.
///
/// A plan is built from what is uncommitted, what is unpushed, what is not in
/// git at all, and what is running inside — and from nothing else. It never
/// reads a size or a landing, so this path collects neither. The delete dialog
/// used to open behind a full inspection that walked the whole directory and
/// proved the branch's history against the default ref, then dropped both
/// results on the floor.
///
/// Splitting the repository-wide half out is also what makes selecting five
/// worktrees cost one worktree listing and one branch listing instead of five
/// of each.
pub struct RemovalPlanner<'a> {
    git: &'a Git,
    entries: Vec<WorktreeEntry>,
    ctx: BranchContext,
}

impl<'a> RemovalPlanner<'a> {
    /// `root` is the main worktree, so this works when the target directory has
    /// already been deleted.
    pub fn load(git: &'a Git, root: &Path) -> Result<Self> {
        let entries = list_worktrees(git, root)?;
        let ctx = load_branch_context(git, root, &entries)?;
        Ok(Self { git, entries, ctx })
    }

    pub fn entry(&self, path: &Path) -> Option<&WorktreeEntry> {
        let key = path_key(path);
        self.entries
            .iter()
            .find(|entry| path_key(&entry.path) == key)
    }

    /// Every worktree this repository had when the planner loaded.
    ///
    /// Carried out with a refusal so the caller can re-plan against what is
    /// there now rather than against the list it painted earlier.
    pub fn registered_paths(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// Plan for one worktree, without looking for processes inside it.
    ///
    /// Process detection is a snapshot of the whole machine's process table, so
    /// callers planning several worktrees take it once and fill the counts in
    /// afterwards.
    pub fn plan(&self, entry: &WorktreeEntry) -> RemovalPlan {
        let CollectedStatus { status, dirty } = collect_status(self.git, entry, &self.ctx);
        build_plan(self.git, &self.ctx.root, entry, &status, &dirty)
    }
}

/// Plans for several worktrees of one repository.
///
/// The repository is listed once and the process table read once, however many
/// worktrees are named; asking for them one at a time repeated both per name.
pub fn plan_removals(git: &Git, root: &Path, paths: &[PathBuf]) -> Result<Vec<RemovalPlan>> {
    let planner = RemovalPlanner::load(git, root)?;
    let processes = process::scan_enclosing(paths);

    paths
        .iter()
        .map(|path| {
            let entry = planner.entry(path).ok_or_else(|| {
                Error::Parse(format!(
                    "{} is not a worktree of this repository",
                    path.display()
                ))
            })?;
            let mut plan = planner.plan(entry);
            plan.running_processes = processes.get(&path_key(path)).map_or(0, Vec::len);
            Ok(plan)
        })
        .collect()
}

/// What became of the optional branch deletion.
///
/// git refusing to delete an unmerged branch is the right outcome and must not
/// fail the removal — the worktree is gone and the commits stay reachable, so
/// nothing was lost. But the user ticked a box and it silently did not happen,
/// which is its own small betrayal. Reporting separates the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BranchOutcome {
    /// Branch deletion was not asked for, or there was no branch to delete.
    NotRequested,
    Deleted,
    /// git declined, most often because the branch holds unmerged commits.
    Kept,
    /// The branch is no longer the commit the removal was authorised against.
    ///
    /// `git branch -D` deletes whatever the name currently points at. Between
    /// the plan the user approved and the moment of deletion, a commit, an
    /// amend, or a fetch can move the ref — and deleting the moved branch
    /// discards work the user was never shown. The worktree removal itself
    /// stands; only the branch is left alone, and said so.
    Moved,
    /// Branch finalisation failed before its resulting ref state was established.
    Unknown,
    /// A deleted branch could not be rolled back after later finalisation failed.
    RollbackFailed,
}

/// What a removal actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalOutcome {
    pub branch: BranchOutcome,
}

/// How completely one removal finished.
///
/// Removing a worktree is not one operation. Moving a directory to the trash
/// and then reconciling git's administrative data are two, and the second can
/// fail after the first has succeeded — at which point the directory is gone
/// for good and no amount of retrying brings it back. Reporting only "removed"
/// or nothing at all forces that state into one of two lies: that the worktree
/// survived, or that everything about it was tidied away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemovalStatus {
    /// The worktree went and everything that follows it went with it.
    #[default]
    Removed,
    /// The worktree's directory or metadata is already gone, but the removal
    /// did not run to the end — so whatever comes after it (pruning the
    /// administrative entry, deleting the branch) may not have happened.
    ///
    /// Discovered by reading the repository after a failure rather than by a
    /// step reporting success, which is why the branch outcome that comes with
    /// it is the conservative one: yawm does not claim a deletion it cannot
    /// prove.
    RemovedButFinalizationFailed,
}

/// One removal that actually happened, named.
///
/// A batch reports its outcomes positionally while it succeeds. The moment it
/// fails part-way the positions stop meaning anything, and the caller needs to
/// know *which* worktrees are gone — to close their tabs, to stop listing them,
/// and to avoid telling the user that nothing happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletedRemoval {
    pub path: PathBuf,
    pub outcome: RemovalOutcome,
    /// Whether the removal also finished everything that follows it.
    ///
    /// Defaulted on the wire so a caller reading an older payload still gets
    /// the honest reading of a bare `{path, outcome}`: it ran to the end.
    #[serde(default)]
    pub status: RemovalStatus,
}

impl CompletedRemoval {
    /// A removal that ran to the end, reported by the step that carried it out.
    pub fn removed(path: PathBuf, outcome: RemovalOutcome) -> Self {
        Self {
            path,
            outcome,
            status: RemovalStatus::Removed,
        }
    }
}

/// A batch that mutated something and then failed.
///
/// Removal cannot be rolled back. Once a directory is gone, an error that only
/// says "it failed" is a lie by omission: the caller re-renders its list, sees
/// the selection it started with, and reports that nothing was deleted. So the
/// failure carries what did happen, in the order it happened, next to the
/// worktree that stopped it.
///
/// `completed` is not merely "the steps that returned success". A removal is
/// several operations — trash the directory, prune the metadata, delete the
/// branch — and failing at the second one leaves a worktree that is gone
/// without ever having been reported. Everything the repository and the
/// filesystem say has disappeared is reconciled into this list before the
/// failure is raised, carrying [`RemovalStatus::RemovedButFinalizationFailed`]
/// so the difference stays visible.
///
/// Only ever built with at least one completed removal. A failure before the
/// first mutation stays the error it was — which is what keeps
/// [`Error::PlanChanged`] meaning exactly "nothing was deleted".
#[derive(Debug)]
pub struct PartialRemoval {
    /// The removals that did happen, in order.
    ///
    /// Only worktrees yawm reached with a mutating operation and that are now
    /// gone. A worktree that disappeared before yawm touched it is not in here.
    pub completed: Vec<CompletedRemoval>,
    /// Worktrees that were gone before yawm attempted anything on them.
    ///
    /// Something outside yawm removed them while the batch was running. That
    /// is the same class of event as [`Error::PlanChanged`] — the world moved
    /// under an approved plan — and reporting it as a yawm removal would tell
    /// the user that yawm did something it did not do, and hide the fact that
    /// another process is writing to the same repository.
    pub vanished: Vec<PathBuf>,
    /// The worktree whose removal failed.
    pub failed: PathBuf,
    /// Why it failed.
    pub cause: Box<Error>,
}

impl std::fmt::Display for PartialRemoval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let removed: Vec<String> = self
            .completed
            .iter()
            .map(|done| label_of(&done.path))
            .collect();
        write!(
            f,
            "{} could not be removed: {}. {} {} already removed and cannot be brought back: {}",
            label_of(&self.failed),
            self.cause,
            removed.len(),
            if removed.len() == 1 {
                "worktree was"
            } else {
                "worktrees were"
            },
            removed.join(", ")
        )?;
        if !self.vanished.is_empty() {
            let names: Vec<String> = self.vanished.iter().map(|path| label_of(path)).collect();
            write!(
                f,
                ". {} disappeared before yawm reached {}, removed by something else: {}",
                names.len(),
                if names.len() == 1 { "it" } else { "them" },
                names.join(", ")
            )?;
        }
        Ok(())
    }
}

/// A batch that deleted nothing and found worktrees gone anyway.
///
/// The counterpart to [`PartialRemoval`] for the case where yawm's own count of
/// removals is zero. Nothing here was deleted by yawm — that distinction is the
/// whole reason the two are separate types — but the caller's list and its open
/// tabs are still wrong, so the paths cross structurally rather than as a
/// sentence inside some other error.
///
/// Only ever built with at least one vanished worktree. A failure with none of
/// them stays exactly the error it was.
#[derive(Debug)]
pub struct VanishedRemoval {
    /// Worktrees that were gone before yawm attempted anything on them.
    pub vanished: Vec<PathBuf>,
    /// The worktree whose removal failed.
    pub failed: PathBuf,
    /// Why it failed.
    pub cause: Box<Error>,
}

impl std::fmt::Display for VanishedRemoval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self.vanished.iter().map(|path| label_of(path)).collect();
        write!(
            f,
            "{} could not be removed: {}. Nothing was removed by yawm, and {} {} \
             already gone, removed by something else: {}",
            label_of(&self.failed),
            self.cause,
            names.len(),
            if names.len() == 1 { "was" } else { "were" },
            names.join(", ")
        )
    }
}

/// The name a person would use for a worktree: its directory.
fn label_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Stable substring of [`Error::PlanChanged`]'s message.
///
/// The desktop app receives errors as plain strings — the Tauri command
/// stringifies them — so this is the only channel through which the frontend
/// can tell "look again" apart from "it broke". A test pins it to the real
/// message so the two cannot drift apart.
pub const PLAN_CHANGED_MARKER: &str = "changed since it was checked";

/// Rebuild every plan and refuse if any worktree is no longer what was
/// approved.
///
/// The plan a user confirms is a photograph, and this is the one code path in
/// yawm that cannot be undone. Between the dialog rendering and the click,
/// something writes into the directory — and for this app that is the normal
/// case, not a hypothetical, because the whole reason these worktrees exist is
/// that agents are working in them. A file created in that window was never in
/// the plan the user read, and `--force` would destroy it anyway.
///
/// So the plans are recomputed here, immediately before anything irreversible,
/// and any difference aborts with the differences named. Nothing is deleted:
/// the caller re-plans, shows the new cost, and asks again.
///
/// Every plan in the batch is checked before any of them is acted on. Checking
/// and deleting one at a time meant a selection of five where the second had
/// changed still lost the first: it was already gone by the time the second was
/// looked at, and the dialog then re-planned a selection containing a path that
/// no longer existed and reported *that* as the failure. The user was told the
/// deletion had been refused while one worktree had in fact been deleted.
///
/// One [`RemovalPlanner`] serves the whole batch, so every plan is compared
/// against the same snapshot of the repository as well as against the same
/// moment in time.
///
/// The recomputed plans are what the rest of the removal then acts on, so the
/// authorisation checks are applied to the worktrees as they are now rather
/// than as they were described.
fn revalidate_all(git: &Git, root: &Path, approved: &[&RemovalPlan]) -> Result<Vec<RemovalPlan>> {
    let planner = RemovalPlanner::load(git, root)?;
    let mut current = Vec::with_capacity(approved.len());
    let mut changed: Vec<(PathBuf, Vec<String>)> = Vec::new();
    // Taken from the same snapshot as the refusal, so a caller that re-plans
    // after it asks about worktrees this repository still has rather than about
    // the list it painted before the dialog opened.
    let still_present = planner.registered_paths();

    for plan in approved {
        match planner.entry(&plan.path) {
            None => changed.push((
                plan.path.clone(),
                vec!["it is no longer a worktree of this repository".to_string()],
            )),
            Some(entry) => {
                let now = planner.plan(entry);
                let changes = describe_changes(plan, &now);
                if changes.is_empty() {
                    current.push(now);
                } else {
                    changed.push((plan.path.clone(), changes));
                }
            }
        }
    }

    if changed.is_empty() {
        return Ok(current);
    }
    Err(plan_changed(changed, still_present))
}

/// Rebuild one plan immediately before that worktree is acted on.
///
/// [`revalidate_all`] proves the whole batch at one moment and then starts
/// deleting, and the moment it proved is only the moment the *first* removal
/// happens at. By the time the third worktree in a selection is reached, that
/// snapshot is however long the first two took — long enough for an agent to
/// write a file into it, lock it, or commit in it. Re-reading only the lock
/// there closed the smallest of those windows and left the rest open: a
/// worktree that grew an untracked file while its neighbour was being deleted
/// was removed with `--force` against a plan that never mentioned it.
///
/// So the same comparison the batch makes is made again per worktree, against
/// the plan the batch approved, and the plan it returns is the one the removal
/// then acts on. The window narrows to the gap between this call and the next
/// git call. It cannot close: the filesystem is shared, and no check can make
/// it otherwise.
///
/// Refusing here after an earlier worktree has already gone is *not* a plain
/// refusal — see [`Execution::report`], which turns it into
/// [`Error::BatchIncomplete`] so "nothing was deleted" keeps meaning that.
fn revalidate_before_removal(
    git: &Git,
    root: &Path,
    approved: &RemovalPlan,
) -> Result<RemovalPlan> {
    let planner = RemovalPlanner::load(git, root)?;
    let still_present = planner.registered_paths();

    let Some(entry) = planner.entry(&approved.path) else {
        return Err(Error::PlanChanged {
            path: approved.path.clone(),
            changes: vec!["it is no longer a worktree of this repository".to_string()],
            still_present,
        });
    };

    let now = planner.plan(entry);
    let changes = describe_changes(approved, &now);
    if changes.is_empty() {
        return Ok(now);
    }
    Err(Error::PlanChanged {
        path: approved.path.clone(),
        changes,
        still_present,
    })
}

/// The refusal for a batch, naming every worktree that no longer matches.
///
/// A single changed worktree keeps the message it always had. When several
/// changed, each one's differences are prefixed with its name — the caller has
/// to be able to say which of the five it should look at again, and a list of
/// unattributed differences cannot.
fn plan_changed(mut changed: Vec<(PathBuf, Vec<String>)>, still_present: Vec<PathBuf>) -> Error {
    let (path, changes) = changed.remove(0);
    if changed.is_empty() {
        return Error::PlanChanged {
            path,
            changes,
            still_present,
        };
    }

    let mut all = vec![format!("{}: {}", label_of(&path), changes.join(", "))];
    all.extend(
        changed
            .iter()
            .map(|(p, cs)| format!("{}: {}", label_of(p), cs.join(", "))),
    );
    Error::PlanChanged {
        path,
        changes: all,
        still_present,
    }
}

/// Name every difference that means the user approved something else.
///
/// Running processes are deliberately not compared. They start and stop on
/// their own while the dialog is open, so a worktree would almost never survive
/// the check — and a warning that fires on everything is the same as no
/// warning. Losing a process costs nothing permanent; the fields compared here
/// are the ones that describe work that would be destroyed.
fn describe_changes(approved: &RemovalPlan, current: &RemovalPlan) -> Vec<String> {
    let mut changes = Vec::new();

    if approved.branch != current.branch {
        changes.push(match (&approved.branch, &current.branch) {
            (_, Some(now)) => format!("it now has {now} checked out"),
            (_, None) => "it is now detached".to_string(),
        });
    }

    if approved.is_prunable != current.is_prunable {
        changes.push(if current.is_prunable {
            "its directory is gone".to_string()
        } else {
            "its directory is back".to_string()
        });
    }

    if approved.is_locked != current.is_locked {
        changes.push(if current.is_locked {
            match &current.lock_reason {
                Some(reason) => format!("it has been locked: {reason}"),
                None => "it has been locked".to_string(),
            }
        } else {
            "it has been unlocked".to_string()
        });
    } else if approved.lock_reason != current.lock_reason {
        // A lock that stayed on but now says something else is a different
        // instruction from the one the user read, and removing it lifts the
        // new one. Only reachable while it is locked, so an unlocked worktree
        // cannot trip on a stale reason.
        changes.push(match &current.lock_reason {
            Some(reason) => format!("its lock now says: {reason}"),
            None => "its lock no longer says why".to_string(),
        });
    }

    if approved.dirty_total != current.dirty_total {
        changes.push(format!(
            "uncommitted changes went from {} to {}",
            approved.dirty_total, current.dirty_total
        ));
    }

    // Named separately from the count, because one file replacing another
    // leaves the total untouched while changing entirely what would be lost.
    let appeared = missing_from(&current.dirty_files, &approved.dirty_files);
    if !appeared.is_empty() {
        changes.push(format!("new uncommitted files: {}", appeared.join(", ")));
    }

    let new_env = missing_from(&current.env_files, &approved.env_files);
    if !new_env.is_empty() {
        changes.push(format!(
            "new files that are not in git: {}",
            new_env.join(", ")
        ));
    }

    if approved.unpushed_commits != current.unpushed_commits {
        changes.push(format!(
            "unpushed commits went from {} to {}",
            approved.unpushed_commits, current.unpushed_commits
        ));
    }

    changes.extend(describe_state(&approved.state, &current.state));

    // The digest is the backstop, and after the round trip through the frontend
    // it is often the *only* thing left to compare: the plan that comes back
    // carries no per-file identity, by design. Everything above is the capped,
    // readable summary; when it has nothing to say and the digests still
    // disagree, something in the worktree moved that no summary field covers,
    // and saying so imprecisely is the difference between refusing and quietly
    // destroying work the user was never shown.
    if changes.is_empty() && approved.state.digest != current.state.digest {
        changes.push("its worktree state changed since it was approved".to_string());
    }
    changes
}

/// Name every difference the readable fields above cannot express.
///
/// This is the half of the comparison that carries the authorisation. The
/// fields above are a summary, and every one of them survives changes that
/// destroy different work than the user approved losing: a fifty-first dirty
/// file swapped for another, a listed file rewritten under the same name, an
/// unpushed commit amended, a branch ref moved. The fingerprint is exact and
/// uncapped, so all of those move it.
///
/// The named differences below need both sides' evidence, which only exists
/// for plans built in this process. A plan handed back by a frontend has the
/// digest alone; the comparison is then digest against digest and the caller
/// says so generically rather than pretending to know which file moved.
fn describe_state(approved: &StateFingerprint, current: &StateFingerprint) -> Vec<String> {
    let mut changes = Vec::new();

    // Version first. A fingerprint from a build that encoded state differently
    // is not a fingerprint that can be compared, and neither is an absent one.
    if approved.version != STATE_VERSION {
        changes.push("the exact state it was approved against is not readable".to_string());
        return changes;
    }
    if current.version != STATE_VERSION {
        changes.push("its exact state could not be established".to_string());
        return changes;
    }

    // Said next and never swallowed. A state that could not be read is not a
    // state that matched, so the authorisation cannot carry over it.
    match current.evidence() {
        Some(evidence) => {
            for reason in &evidence.unproven {
                changes.push(format!("it could not be fully inspected: {reason}"));
            }
        }
        None if current.unproven => {
            changes.push("it could not be fully inspected".to_string());
        }
        None => {}
    }
    match approved.evidence() {
        Some(evidence) => {
            for reason in &evidence.unproven {
                changes.push(format!(
                    "it was not fully inspected when it was approved: {reason}"
                ));
            }
        }
        None if approved.unproven => {
            changes.push("it was not fully inspected when it was approved".to_string());
        }
        None => {}
    }

    let (Some(approved), Some(current)) = (approved.evidence(), current.evidence()) else {
        return changes;
    };
    changes.extend(describe_evidence(approved, current));
    changes
}

/// Name every difference between two states inspected in this process.
fn describe_evidence(approved: &StateEvidence, current: &StateEvidence) -> Vec<String> {
    let mut changes = Vec::new();

    let before: BTreeMap<&str, &DirtyIdentity> = approved
        .dirty
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut rewritten: Vec<&str> = Vec::new();
    let mut appeared: Vec<&str> = Vec::new();
    for entry in &current.dirty {
        match before.get(entry.path.as_str()) {
            None => appeared.push(&entry.path),
            Some(was) if *was != entry => rewritten.push(&entry.path),
            Some(_) => {}
        }
    }
    let now: BTreeMap<&str, &DirtyIdentity> = current
        .dirty
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let settled: Vec<&str> = approved
        .dirty
        .iter()
        .map(|entry| entry.path.as_str())
        .filter(|path| !now.contains_key(path))
        .collect();

    if !appeared.is_empty() {
        changes.push(format!(
            "uncommitted files appeared: {}",
            appeared.join(", ")
        ));
    }
    if !rewritten.is_empty() {
        changes.push(format!(
            "uncommitted files changed since they were listed: {}",
            rewritten.join(", ")
        ));
    }
    if !settled.is_empty() {
        changes.push(format!(
            "uncommitted files are no longer uncommitted: {}",
            settled.join(", ")
        ));
    }

    let env_before: BTreeMap<&str, &FileIdentity> = approved
        .env
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let env_changed: Vec<&str> = current
        .env
        .iter()
        .filter(|entry| env_before.get(entry.path.as_str()) != Some(entry))
        .map(|entry| entry.path.as_str())
        .collect();
    let env_now: BTreeMap<&str, &FileIdentity> = current
        .env
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let env_gone: Vec<&str> = approved
        .env
        .iter()
        .map(|entry| entry.path.as_str())
        .filter(|path| !env_now.contains_key(path))
        .collect();
    if !env_changed.is_empty() {
        changes.push(format!(
            "files that are not in git changed on disk: {}",
            env_changed.join(", ")
        ));
    }
    if !env_gone.is_empty() {
        changes.push(format!(
            "files that are not in git have gone: {}",
            env_gone.join(", ")
        ));
    }

    if approved.head != current.head {
        changes.push(format!(
            "it now has {} checked out rather than {}",
            short(&current.head),
            short(&approved.head)
        ));
    }
    if approved.branch_oid != current.branch_oid {
        changes.push(format!(
            "its branch now points at {} rather than {}",
            short(&current.branch_oid),
            short(&approved.branch_oid)
        ));
    }
    if approved.upstream != current.upstream {
        changes.push(match &current.upstream {
            Some(name) => format!("its upstream is now {name}"),
            None => "it no longer has an upstream".to_string(),
        });
    }
    if approved.upstream_oid != current.upstream_oid {
        changes.push(format!(
            "its upstream now points at {} rather than {}",
            short(&current.upstream_oid),
            short(&approved.upstream_oid)
        ));
    }
    if approved.merge_ref != current.merge_ref {
        changes.push(match &current.merge_ref {
            Some(name) => format!("deleting its branch would now be decided against {name}"),
            None => "there is no longer a ref its branch would be measured against".to_string(),
        });
    }
    if approved.behind != current.behind {
        changes.push(format!(
            "commits it is behind went from {} to {}",
            approved.behind, current.behind
        ));
    }
    if approved.upstream_gone != current.upstream_gone {
        changes.push(if current.upstream_gone {
            "its upstream has been deleted on the remote".to_string()
        } else {
            "its upstream is back on the remote".to_string()
        });
    }
    if approved.detached != current.detached {
        changes.push(if current.detached {
            "its HEAD is now detached".to_string()
        } else {
            "its HEAD is no longer detached".to_string()
        });
    }
    if approved.directory_present != current.directory_present {
        changes.push(if current.directory_present {
            "its directory can be inspected again".to_string()
        } else {
            "its directory can no longer be inspected".to_string()
        });
    }
    if approved.prunable != current.prunable {
        changes.push(match &current.prunable {
            Some(reason) => format!("git now calls its metadata stale: {reason}"),
            None => "git no longer calls its metadata stale".to_string(),
        });
    }

    changes
}

/// A commit name at the length a person reads.
fn short(oid: &Option<String>) -> String {
    match oid {
        Some(oid) if oid.len() > 12 => oid[..12].to_string(),
        Some(oid) => oid.clone(),
        None => "nothing".to_string(),
    }
}

/// Entries of `now` that `before` never mentioned.
fn missing_from(now: &[String], before: &[String]) -> Vec<String> {
    now.iter()
        .filter(|item| !before.contains(item))
        .cloned()
        .collect()
}

/// Remove a worktree.
///
/// `root` is the main worktree, so this still works when the target directory
/// has already been deleted.
pub fn remove(git: &Git, root: &Path, plan: &RemovalPlan, opts: RemoveOptions) -> Result<()> {
    remove_reporting(git, root, plan, opts).map(|_| ())
}

/// One worktree's removal, with the options that authorise it.
///
/// Paired rather than a single set of options for the batch, because `force` is
/// a property of the worktree it belongs to: selecting one dirty worktree
/// alongside four clean ones must not force the other four, which would throw
/// away git's own refusal at the moment of destruction.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalRequest {
    pub plan: RemovalPlan,
    pub options: RemoveOptions,
}

/// Remove a worktree and say what happened to its branch.
///
/// Same work as [`remove`], which stays for callers that only care whether the
/// directory went. A single removal is a batch of one, so it gets exactly the
/// same checks in exactly the same order as a selection of five.
pub fn remove_reporting(
    git: &Git,
    root: &Path,
    plan: &RemovalPlan,
    opts: RemoveOptions,
) -> Result<RemovalOutcome> {
    let mut outcomes = remove_all_by_ref(
        git,
        root,
        &[(plan, opts)],
        &mut Seams {
            before_any: &mut || {},
            after_each: &mut |_| {},
        },
    )?;
    Ok(outcomes.remove(0))
}

/// Remove several worktrees, or none of them.
///
/// The invariant this exists for: **nothing is mutated until every plan in the
/// batch has passed validation.** A dialog that looped over `remove_reporting`
/// deleted the first worktree, discovered the second had changed underneath it,
/// and then reported a refusal — leaving the user believing nothing had
/// happened while one worktree was already gone.
///
/// Once the first directory is actually removed there is no rolling back, and
/// this does not pretend otherwise: a git failure part-way through leaves the
/// removals that already happened in place and returns
/// [`Error::BatchIncomplete`], which names every one of them. What it
/// guarantees is the part that can be guaranteed — that the decision to delete
/// anything is taken over the whole selection, at one moment, against one
/// snapshot of the repository, and that a failure before the first mutation
/// stays an ordinary error with nothing deleted behind it.
pub fn remove_all(
    git: &Git,
    root: &Path,
    requests: &[RemovalRequest],
) -> Result<Vec<RemovalOutcome>> {
    let pairs: Vec<(&RemovalPlan, RemoveOptions)> = requests
        .iter()
        .map(|request| (&request.plan, request.options))
        .collect();
    remove_all_by_ref(
        git,
        root,
        &pairs,
        &mut Seams {
            before_any: &mut || {},
            after_each: &mut |_| {},
        },
    )
}

/// Where a test may step into the middle of a batch.
///
/// The windows this module exists to survive are not reproducible from outside
/// it — validation and removal happen inside one call — so without seams the
/// checks guarding them can only be argued about, not tested. Production
/// callers pass closures that do nothing.
#[doc(hidden)]
pub struct Seams<'a> {
    /// After every plan has been validated, before anything is mutated.
    pub before_any: &'a mut dyn FnMut(),
    /// After each worktree is actually removed, named — the window in which the
    /// worktrees still to come go on being written to by whatever is running
    /// in them.
    pub after_each: &'a mut dyn FnMut(&Path),
}

/// [`remove_all`], with a seam that runs after every plan has been validated
/// and before anything is mutated.
#[doc(hidden)]
pub fn remove_all_interrupted(
    git: &Git,
    root: &Path,
    requests: &[RemovalRequest],
    between: &mut dyn FnMut(),
) -> Result<Vec<RemovalOutcome>> {
    let pairs: Vec<(&RemovalPlan, RemoveOptions)> = requests
        .iter()
        .map(|request| (&request.plan, request.options))
        .collect();
    remove_all_by_ref(
        git,
        root,
        &pairs,
        &mut Seams {
            before_any: between,
            after_each: &mut |_| {},
        },
    )
}

/// [`remove_all`], with a seam that runs after each removal.
///
/// The batch is validated once and then carried out one worktree at a time, so
/// every removal is a pause during which the worktrees still to come can change
/// — an agent writing a file into the fourth while the second is being deleted
/// is the ordinary case for this app. This is how that pause is written down in
/// a test.
#[doc(hidden)]
pub fn remove_all_after_each(
    git: &Git,
    root: &Path,
    requests: &[RemovalRequest],
    after_each: &mut dyn FnMut(&Path),
) -> Result<Vec<RemovalOutcome>> {
    let pairs: Vec<(&RemovalPlan, RemoveOptions)> = requests
        .iter()
        .map(|request| (&request.plan, request.options))
        .collect();
    remove_all_by_ref(
        git,
        root,
        &pairs,
        &mut Seams {
            before_any: &mut || {},
            after_each,
        },
    )
}

fn remove_all_by_ref(
    git: &Git,
    root: &Path,
    requests: &[(&RemovalPlan, RemoveOptions)],
    seams: &mut Seams<'_>,
) -> Result<Vec<RemovalOutcome>> {
    // ---- Nothing below mutates anything until the last check has passed. ----

    for (plan, opts) in requests {
        check_options(plan, opts)?;
    }

    let approved: Vec<&RemovalPlan> = requests.iter().map(|(plan, _)| *plan).collect();
    let current = revalidate_all(git, root, &approved)?;

    for (plan, (_, opts)) in current.iter().zip(requests) {
        check_options(plan, opts)?;
        check_authorised(plan, opts)?;
    }

    (seams.before_any)();

    // ---- Irreversible from here. ----

    let options: Vec<RemoveOptions> = requests.iter().map(|(_, opts)| *opts).collect();
    let mut run = Execution::new(&current, options);
    match run.carry_out(git, root, seams.after_each) {
        Ok(()) => Ok(run.into_outcomes()),
        Err(stopped) => Err(run.report(git, root, stopped)),
    }
}

/// Where a batch stopped, and why.
struct Stopped {
    at: PathBuf,
    cause: Box<Error>,
}

/// The irreversible half of one batch.
///
/// It exists as a value rather than a loop because what has already happened
/// has to survive the failure: the outcomes recorded so far, and — after any
/// error — everything the repository says has disappeared regardless of which
/// step reported success. A loop that only remembered its own `Ok`s could not
/// report a directory that went to the trash a moment before the prune that
/// was meant to follow it failed.
struct Execution<'a> {
    plans: &'a [RemovalPlan],
    options: Vec<RemoveOptions>,
    /// Positional, because that is how a successful batch reports.
    outcomes: Vec<Option<RemovalOutcome>>,
    /// Named, because positions stop meaning anything the moment it fails.
    completed: Vec<CompletedRemoval>,
    /// Whether yawm has run a mutating operation against each request yet.
    ///
    /// Reconciliation asks "is it gone?", and an absence on its own does not
    /// say who caused it. Without this, a worktree removed by an agent, a
    /// script, or a person while the batch was running is indistinguishable
    /// from one yawm deleted — and gets reported as a yawm removal, which is
    /// both a false claim and a lost warning that something else is writing
    /// here. Set immediately before the first irreversible call for a request
    /// and never unset.
    attempted: Vec<bool>,
    /// Requests that were already gone when yawm first reached for them.
    vanished: Vec<PathBuf>,
}

impl<'a> Execution<'a> {
    fn new(plans: &'a [RemovalPlan], options: Vec<RemoveOptions>) -> Self {
        Self {
            outcomes: vec![None; plans.len()],
            completed: Vec::new(),
            attempted: vec![false; plans.len()],
            vanished: Vec::new(),
            plans,
            options,
        }
    }

    fn carry_out(
        &mut self,
        git: &Git,
        root: &Path,
        after_each: &mut dyn FnMut(&Path),
    ) -> std::result::Result<(), Stopped> {
        self.prune_stale(git, root, after_each)?;

        for index in 0..self.plans.len() {
            if self.plans[index].is_prunable {
                continue;
            }
            let plan = &self.plans[index];
            // Set before the call rather than inside it: `execute` re-reads and
            // re-checks before it mutates, and a refusal at that point has
            // touched nothing — but by the time it returns, the caller cannot
            // tell the two apart. `execute` flips this itself, at the line
            // where the first irreversible call happens.
            let outcome = execute(
                git,
                root,
                plan,
                self.options[index],
                &mut self.attempted[index],
            );
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(cause) => {
                    if matches!(&cause, Error::BranchRollbackFailed { .. }) {
                        self.record(
                            index,
                            RemovalOutcome {
                                branch: BranchOutcome::RollbackFailed,
                            },
                            RemovalStatus::RemovedButFinalizationFailed,
                        );
                    }
                    return Err(Stopped {
                        at: plan.path.clone(),
                        cause: Box::new(cause),
                    });
                }
            };
            self.record(index, outcome, RemovalStatus::Removed);
            after_each(&self.plans[index].path);
        }
        Ok(())
    }

    /// Every worktree whose directory is already gone, pruned in one operation.
    ///
    /// `git worktree prune` is repository-wide: it drops the administrative
    /// entry of *every* worktree whose directory is missing, not the one it was
    /// asked about. Running it per request therefore pruned the rest of the
    /// selection as a side effect, and the next request in the batch then
    /// failed its own re-check with "it is no longer a worktree of this
    /// repository" — a worktree the user asked to remove, removed, and reported
    /// as the failure that stopped the batch.
    ///
    /// One prune for all of them, before anything with a directory is touched,
    /// so the operation's real scope and the batch's intent are the same thing.
    fn prune_stale(
        &mut self,
        git: &Git,
        root: &Path,
        after_each: &mut dyn FnMut(&Path),
    ) -> std::result::Result<(), Stopped> {
        let stale: Vec<usize> = (0..self.plans.len())
            .filter(|&i| self.plans[i].is_prunable)
            .collect();
        let Some(&first) = stale.first() else {
            return Ok(());
        };

        // Re-read each one immediately before it is acted on, exactly as the
        // directory path does, and refuse on anything that is not what was
        // approved. Nothing has been mutated yet at this point.
        let mut fresh: Vec<(usize, RemovalPlan)> = Vec::with_capacity(stale.len());
        for index in stale {
            let plan =
                revalidate_before_removal(git, root, &self.plans[index]).map_err(|cause| {
                    Stopped {
                        at: self.plans[index].path.clone(),
                        cause: Box::new(cause),
                    }
                })?;
            check_options(&plan, &self.options[index]).map_err(|cause| Stopped {
                at: plan.path.clone(),
                cause: Box::new(cause),
            })?;
            check_authorised(&plan, &self.options[index]).map_err(|cause| Stopped {
                at: plan.path.clone(),
                cause: Box::new(cause),
            })?;
            fresh.push((index, plan));
        }

        // Every revalidation passed, so the batch is about to mutate. From
        // here an absence in any of these is yawm's prune, not somebody else's
        // deletion — which is what stops reconciliation reporting them as
        // vanished when the prune half-succeeded.
        for (index, _) in &fresh {
            self.attempted[*index] = true;
        }

        // `git worktree prune` skips locked worktrees, so a lock that was
        // authorised has to come off by name first — and go back on if the
        // prune it was lifted for never happens.
        let mut unlocked: Vec<&RemovalPlan> = Vec::new();
        for (_, plan) in &fresh {
            if !plan.is_locked {
                continue;
            }
            if let Err(cause) = git.run(
                root,
                &[
                    "worktree".to_string(),
                    "unlock".to_string(),
                    plan.path.to_string_lossy().into_owned(),
                ],
            ) {
                return Err(Stopped {
                    at: plan.path.clone(),
                    cause: Box::new(restore_locks(git, root, &unlocked, cause)),
                });
            }
            unlocked.push(plan);
        }

        if let Err(cause) = prune(git, root) {
            return Err(Stopped {
                at: self.plans[first].path.clone(),
                cause: Box::new(restore_locks(git, root, &unlocked, cause)),
            });
        }

        /*
         * The prune reported success for the repository, not for these entries.
         * What it actually did is read back rather than assumed — and until
         * that readback proves an entry is gone, its lock is still a lock the
         * user set and yawm lifted.
         *
         * A failed readback proves nothing about any of them, so every lock
         * this lifted goes back on. Reporting the prune's success and walking
         * away would leave worktrees that still exist with their "do not
         * touch" silently removed.
         */
        let registered: Vec<PathBuf> = match list_worktrees(git, root) {
            Ok(entries) => entries.into_iter().map(|entry| entry.path).collect(),
            Err(cause) => {
                return Err(Stopped {
                    at: self.plans[first].path.clone(),
                    cause: Box::new(restore_locks(git, root, &unlocked, cause)),
                });
            }
        };

        // Entries the prune did not take. Their locks were lifted for an
        // operation that did not reach them, so they are put back exactly as
        // they were — same reason, same words — before the failure is raised.
        let survivors: Vec<&RemovalPlan> = unlocked
            .iter()
            .copied()
            .filter(|plan| is_registered(&registered, &plan.path))
            .collect();

        for (index, plan) in &fresh {
            let index = *index;
            if is_registered(&registered, &plan.path) {
                let cause = Error::Parse(format!(
                    "{} is still registered after pruning, so its stale metadata was not removed",
                    plan.path.display()
                ));
                return Err(Stopped {
                    at: plan.path.clone(),
                    cause: Box::new(restore_locks(git, root, &survivors, cause)),
                });
            }
            let branch = if self.options[index].delete_branch {
                match delete_branch(git, root, plan, self.options[index].force_branch) {
                    Ok(branch) => branch,
                    Err(cause) => {
                        self.record(
                            index,
                            RemovalOutcome {
                                branch: if matches!(&cause, Error::BranchRollbackFailed { .. }) {
                                    BranchOutcome::RollbackFailed
                                } else {
                                    BranchOutcome::Unknown
                                },
                            },
                            RemovalStatus::RemovedButFinalizationFailed,
                        );
                        return Err(Stopped {
                            at: plan.path.clone(),
                            cause: Box::new(cause),
                        });
                    }
                }
            } else {
                BranchOutcome::NotRequested
            };
            self.record(index, RemovalOutcome { branch }, RemovalStatus::Removed);
            after_each(&plan.path);
        }
        Ok(())
    }

    fn record(&mut self, index: usize, outcome: RemovalOutcome, status: RemovalStatus) {
        self.outcomes[index] = Some(outcome);
        self.completed.push(CompletedRemoval {
            path: self.plans[index].path.clone(),
            outcome,
            status,
        });
    }

    /// Every position is filled on the path that returns this.
    fn into_outcomes(self) -> Vec<RemovalOutcome> {
        self.outcomes.into_iter().flatten().collect()
    }

    /// Turn a stop into the failure the caller receives, after reading what the
    /// repository actually looks like now.
    ///
    /// The reconciliation is the point. A removal is more than one operation —
    /// the directory goes to the trash, then git's administrative data is
    /// pruned — and the second failing leaves a worktree that is gone while the
    /// step that would have reported it never returned. Raising the error as it
    /// stands omits that worktree from `completed`, and the caller then keeps
    /// listing a directory that no longer exists.
    fn report(mut self, git: &Git, root: &Path, stopped: Stopped) -> Error {
        let unread = self.reconcile(git, root);
        let mut cause = *stopped.cause;
        if let Some(note) = unread {
            cause = Error::Parse(format!("{cause}; {note}"));
        }

        // Nothing yawm did went, so the failure is exactly what it says and
        // `PlanChanged` keeps meaning "not one worktree was touched by yawm".
        //
        // A worktree that vanished under the batch still has to be named, and
        // naming it inside the message was not enough: the frontend received
        // prose, read the whole thing as a generic failure, and went on listing
        // directories that are not there with their tabs open. It crosses as
        // its own structured failure instead — no removals claimed, the gone
        // paths carried as paths.
        if self.completed.is_empty() {
            if self.vanished.is_empty() {
                return cause;
            }
            return Error::BatchVanished(Box::new(VanishedRemoval {
                vanished: self.vanished,
                failed: stopped.at,
                cause: Box::new(cause),
            }));
        }
        Error::BatchIncomplete(Box::new(PartialRemoval {
            completed: self.completed,
            vanished: self.vanished,
            failed: stopped.at,
            cause: Box::new(cause),
        }))
    }

    /// Find the requested worktrees that are gone but were never reported gone.
    ///
    /// Returns a note when the repository could not be re-read and some request
    /// is therefore of unknown state — which is said out loud rather than
    /// guessed at, because guessing "it survived" is how a worktree that had in
    /// fact been removed stayed in the list.
    fn reconcile(&mut self, git: &Git, root: &Path) -> Option<String> {
        let listed = list_worktrees(git, root);
        let registered: Option<Vec<PathBuf>> = match &listed {
            Ok(entries) => Some(entries.iter().map(|entry| entry.path.clone()).collect()),
            Err(_) => None,
        };

        let done: Vec<String> = self
            .completed
            .iter()
            .map(|removal| path_key(&removal.path))
            .collect();

        let mut undetermined: Vec<String> = Vec::new();
        for index in 0..self.plans.len() {
            let plan = &self.plans[index];
            if done.contains(&path_key(&plan.path)) {
                continue;
            }
            match gone(plan, registered.as_deref()) {
                // Gone, and yawm never reached for it. Something else removed
                // it while the batch was running. Claiming it as a completed
                // removal would be a lie in both directions: it credits yawm
                // with work it did not do, and it hides that the repository is
                // being written to by something the user does not know about.
                Some(true) if !self.attempted[index] => {
                    self.vanished.push(plan.path.clone());
                }
                Some(true) => {
                    // The finalisation that follows a removal — the prune, the
                    // branch deletion — is exactly what did not run, so nothing
                    // here claims it did. A branch deletion that was asked for
                    // is reported as kept only when the ref can still be proven
                    // at the approved commit. Any other state is unknown here:
                    // this failing operation never reached branch deletion.
                    let branch = if self.options[index].delete_branch {
                        plan.branch
                            .as_deref()
                            .zip(
                                plan.state
                                    .evidence()
                                    .and_then(|evidence| evidence.branch_oid.as_deref()),
                            )
                            .filter(|(branch, expected)| {
                                branch_still_at(git, root, branch, Some(expected))
                            })
                            .map_or(BranchOutcome::Unknown, |_| BranchOutcome::Kept)
                    } else {
                        BranchOutcome::NotRequested
                    };
                    self.record(
                        index,
                        RemovalOutcome { branch },
                        RemovalStatus::RemovedButFinalizationFailed,
                    );
                }
                Some(false) => {}
                None => undetermined.push(label_of(&plan.path)),
            }
        }

        if undetermined.is_empty() {
            return None;
        }
        let failure = listed.err()?;
        Some(format!(
            "and the repository could not be re-read afterwards ({failure}), so whether {} \
             {} removed is not known",
            undetermined.join(", "),
            if undetermined.len() == 1 {
                "was"
            } else {
                "were"
            }
        ))
    }
}

/// Whether this requested worktree has disappeared or been mutated past
/// recovery.
///
/// `registered` is the repository's current worktree list, or `None` when it
/// could not be read — in which case a worktree whose directory was already
/// gone before the batch started cannot be judged at all, and says so.
fn gone(plan: &RemovalPlan, registered: Option<&[PathBuf]>) -> Option<bool> {
    match registered {
        // Stale metadata with no directory: only the registration can change.
        Some(paths) if plan.is_prunable => Some(!is_registered(paths, &plan.path)),
        // The directory is the part that cannot be brought back, so its absence
        // is enough on its own — a trashed directory whose prune then failed is
        // still registered, and is still gone.
        Some(paths) => Some(!plan.path.is_dir() || !is_registered(paths, &plan.path)),
        None if plan.is_prunable => None,
        None => Some(!plan.path.is_dir()),
    }
}

fn is_registered(registered: &[PathBuf], path: &Path) -> bool {
    let key = path_key(path);
    registered.iter().any(|known| path_key(known) == key)
}

/// Refuse option combinations that make no sense for any worktree.
///
/// Reads only the approved plan, so it costs nothing and can run before the
/// repository is touched at all.
fn check_options(plan: &RemovalPlan, opts: &RemoveOptions) -> Result<()> {
    /*
     * The two recoverability options cancel each other out.
     *
     * Trash exists so the directory can be fetched back, and deleting the
     * branch takes away what fetching it back is for: what returns is a folder
     * git no longer knows is a worktree, on a branch that no longer exists,
     * holding commits nothing points at. Refused here rather than only in the
     * dialog, because a rule that lives in one caller is a rule the next
     * caller does not have.
     */
    if opts.use_trash && opts.delete_branch {
        return Err(Error::Parse(
            "moving to Trash keeps the worktree recoverable, so its branch cannot be deleted in              the same step; delete the worktree outright, or keep the branch"
                .to_string(),
        ));
    }

    if plan.is_main {
        return Err(Error::Parse(
            "the main worktree cannot be removed".to_string(),
        ));
    }

    Ok(())
}

/// Refuse a removal the caller has not actually been authorised for.
///
/// Applied to the re-read plan, never to the approved one, so the permissions
/// are checked against the worktree as it is at the moment of destruction.
fn check_authorised(plan: &RemovalPlan, opts: &RemoveOptions) -> Result<()> {
    /*
     * A lock is the one thing in a plan somebody put there on purpose, and it
     * usually carries a reason: "agent running", "do not touch". It is not a
     * side effect of editing files, so agreeing to lose edited files does not
     * answer it. `--force --force` used to silence it without anyone being
     * asked, which is how a worktree locked by a running agent could be
     * deleted by confirming a sentence about uncommitted changes.
     */
    if plan.is_locked && !opts.unlock {
        return Err(Error::Parse(match &plan.lock_reason {
            Some(reason) => format!(
                "{} is locked: {reason}. Unlocking it has to be authorised on its own; \
                 confirming uncommitted changes does not do it",
                plan.path.display()
            ),
            None => format!(
                "{} is locked. Unlocking it has to be authorised on its own; \
                 confirming uncommitted changes does not do it",
                plan.path.display()
            ),
        }));
    }

    // Stale metadata with no directory: there is nothing in it to destroy.
    if plan.is_prunable {
        return Ok(());
    }

    if plan.requires_force && !opts.force {
        return Err(Error::Parse(format!(
            "{} has uncommitted changes; removal requires explicit confirmation",
            plan.path.display()
        )));
    }

    Ok(())
}

/// Carry out one already-validated removal of a worktree that still has a
/// directory.
///
/// `plan` is the plan the batch approved; the removal acts on the plan this
/// re-reads, immediately before the first irreversible call. Worktrees whose
/// directory is already gone never come here — pruning them is repository-wide
/// and is batched in [`Execution::prune_stale`].
fn execute(
    git: &Git,
    root: &Path,
    approved: &RemovalPlan,
    opts: RemoveOptions,
    attempted: &mut bool,
) -> Result<RemovalOutcome> {
    /*
     * The last look, taken for this worktree alone.
     *
     * The batch validation above proved every plan at one moment against one
     * snapshot, and then started deleting. By the time the third worktree in a
     * selection is reached, that snapshot is however long the first two took —
     * long enough for an agent to write into it, commit in it, or lock it, and
     * for yawm to destroy a file nobody was shown or lift a lock nobody read.
     * Re-checking the lock alone here left every other difference unnoticed, so
     * the whole plan is rebuilt and compared: the same comparison the batch
     * made, made again for this worktree at this moment.
     *
     * It narrows the window to the gap between two consecutive git calls. It
     * cannot close it: no check can, because the filesystem is shared. What it
     * guarantees is that what is deleted below is what this function just read.
     */
    let plan = revalidate_before_removal(git, root, approved)?;

    // Re-read facts, re-checked permissions. An unchanged plan answers exactly
    // as it did upstairs; a changed one never reaches here.
    check_options(&plan, &opts)?;
    check_authorised(&plan, &opts)?;

    /*
     * The lock is lifted by name, here, rather than shouted past with a second
     * `--force`. Two things follow from that. Git's refusal stays intact for
     * anyone who did not ask for the lock to go: if this call fails, the
     * removal below fails too instead of forcing its way through. And the lock
     * that is lifted is the one just re-read, a line above the deletion, not
     * the one the dialog rendered some seconds ago.
     */
    if plan.is_locked {
        *attempted = true;
        git.run(
            root,
            &[
                "worktree".to_string(),
                "unlock".to_string(),
                plan.path.to_string_lossy().into_owned(),
            ],
        )?;
    }

    // Everything above this line reads and refuses; nothing above it mutates.
    // From here the worktree has been reached for, and a later absence is
    // yawm's doing rather than somebody else's.
    *attempted = true;

    // Anything that fails from here has already lifted a lock somebody set on
    // purpose, so the lock goes back on before the failure is reported.
    if let Err(failure) = remove_directory(git, root, &plan, opts) {
        return Err(if plan.is_locked {
            restore_lock(git, root, &plan, failure)
        } else {
            failure
        });
    }

    let branch = if opts.delete_branch {
        delete_branch(git, root, &plan, opts.force_branch)?
    } else {
        BranchOutcome::NotRequested
    };
    Ok(RemovalOutcome { branch })
}

/// Take the directory away, by whichever route was authorised.
fn remove_directory(git: &Git, root: &Path, plan: &RemovalPlan, opts: RemoveOptions) -> Result<()> {
    if opts.use_trash {
        // Recoverable: move the directory aside, then let git reconcile its
        // administrative data.
        //
        // Two operations, and the second can fail on its own. When it does the
        // directory is already in the trash and the worktree is gone whatever
        // this returns, which is why a failed batch reconciles the repository
        // rather than trusting these return values — see [`Execution::report`].
        trash::delete(&plan.path)
            .map_err(|e| Error::Parse(format!("could not move to trash: {e}")))?;
        return prune(git, root);
    }

    remove_managed_dependency_links(plan)?;

    let mut args: Vec<String> = vec!["worktree".into(), "remove".into()];
    if opts.force {
        args.push("--force".into());
    }
    args.push(plan.path.to_string_lossy().into_owned());
    git.run(root, &args)?;
    Ok(())
}

fn remove_managed_dependency_links(plan: &RemovalPlan) -> Result<()> {
    let mut paths = Vec::with_capacity(plan.managed_dependency_links.len());
    for link in &plan.managed_dependency_links {
        let path = plan.path.join(&link.path);
        let target = std::fs::canonicalize(&path).map_err(|failure| {
            Error::Parse(format!(
                "managed dependency link {} no longer has its recorded target ({failure})",
                path.display()
            ))
        })?;
        if path_key(&target) != path_key(&link.target) {
            return Err(Error::Parse(format!(
                "managed dependency link {} no longer has its recorded target",
                path.display()
            )));
        }
        paths.push(path);
    }

    for path in paths {
        remove_managed_link(&path).map_err(|failure| {
            Error::Parse(format!(
                "managed dependency link {} could not be removed ({failure})",
                path.display()
            ))
        })?;
    }
    Ok(())
}

fn remove_managed_link(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(file_error) => std::fs::remove_dir(path).map_err(|_| file_error),
    }
}

/// Put back the lock this removal lifted, once the removal itself failed.
///
/// Best effort, and never quiet about it. Leaving a worktree unlocked because
/// yawm failed half-way through deleting it hands the next person — or the next
/// agent — a directory whose "do not touch" has silently gone. If the lock
/// cannot go back on, both failures are reported: hiding either one is how
/// somebody ends up trusting a lock that is not there.
fn restore_lock(git: &Git, root: &Path, plan: &RemovalPlan, cause: Error) -> Error {
    if !still_lockable(git, root, &plan.path) {
        return cause;
    }

    let mut args = vec![
        "worktree".to_string(),
        "lock".to_string(),
        plan.path.to_string_lossy().into_owned(),
    ];
    if let Some(reason) = &plan.lock_reason {
        args.push("--reason".to_string());
        args.push(reason.clone());
    }

    match git.run(root, &args) {
        Ok(_) => cause,
        Err(relock) => Error::Parse(format!(
            "{cause}; and the lock lifted to remove it could not be put back ({relock}), \
             so {} is now unlocked{}",
            plan.path.display(),
            match &plan.lock_reason {
                Some(reason) => format!(" despite having said: {reason}"),
                None => String::new(),
            }
        )),
    }
}

/// Put back every lock a batched operation lifted, once that operation failed.
///
/// The batched prune lifts each authorised lock by name before it runs, so a
/// prune that never happens must leave the locks exactly as it found them.
/// Folded through [`restore_lock`], which reports each failure to re-lock next
/// to the failure that caused it rather than quietly leaving a worktree whose
/// "do not touch" has gone.
fn restore_locks(git: &Git, root: &Path, unlocked: &[&RemovalPlan], cause: Error) -> Error {
    unlocked
        .iter()
        .fold(cause, |cause, plan| restore_lock(git, root, plan, cause))
}

/// Whether there is still a worktree there to put a lock back on.
fn still_lockable(git: &Git, root: &Path, path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    match list_worktrees(git, root) {
        Ok(entries) => {
            let key = path_key(path);
            entries.iter().any(|entry| path_key(&entry.path) == key)
        }
        // The listing itself failed, so whether the worktree survived is not
        // known. Trying to lock it is the answer that cannot hide anything: a
        // worktree that is really gone cannot be locked, and that attempt is
        // reported alongside the original failure rather than swallowed.
        Err(_) => true,
    }
}

/// Delete a branch, reporting rather than failing when git declines.
///
/// Branch deletion is a secondary, opt-in step; if the branch holds unmerged
/// work, the worktree removal that already succeeded should not be reported as
/// a failure. Refusing is the right outcome: the worktree goes, the commits
/// stay reachable, and nothing was lost.
///
/// `plan` is the revalidated plan — the one whose state was just proven equal
/// to what the user approved — so `branch_oid` in its evidence is the commit
/// the deletion was authorised for.
///
/// Deleting is one ref transaction, never a check followed by `git branch`.
/// `git branch -D` deletes whatever the name points at *now*, so the old
/// sequence — read the ref, compare, then delete — had a window in which an
/// agent could commit in the branch between the two calls and have that commit
/// deleted by an authorisation that named its parent.
///
/// The unforced case has to answer "is it merged?" without `git branch -d`,
/// which is the only part of that command worth keeping. It is reproduced
/// exactly as git decides it — against the configured upstream when there is
/// one that still exists, otherwise against the ref this repository's HEAD
/// resolves to — from a pair of immutable object names, so nothing moving
/// afterwards can turn the answer into a different one.
///
/// That answer is only about the two commits it named, though, and both of the
/// refs behind it can move before the deletion runs. So the transaction states
/// the whole proof and lets git enforce it atomically: the merge reference must
/// still hold the commit that proved ancestry, and the branch must still hold
/// the commit the user approved. Either one moving aborts the transaction with
/// the branch untouched, which is the outcome that cannot lose work — a branch
/// merged into a `main` that has since been rewritten is not a branch anybody
/// proved was merged. A forced deletion asks no ancestry question and so states
/// only the branch's own commit.
fn delete_branch(
    git: &Git,
    root: &Path,
    plan: &RemovalPlan,
    force_branch: bool,
) -> Result<BranchOutcome> {
    let Some(branch) = plan.branch.as_deref() else {
        return Ok(BranchOutcome::NotRequested);
    };
    // No evidence means no snapshot to authorise against — a plan that never
    // resolved the ref, or one rebuilt from a payload that carries only a
    // digest. Neither names a commit, so neither authorises deleting one.
    let Some(expected) = plan
        .state
        .evidence()
        .and_then(|evidence| evidence.branch_oid.clone())
    else {
        return Ok(BranchOutcome::Moved);
    };

    let merge = if force_branch {
        None
    } else {
        match merged_at_approval(git, root, plan, &expected) {
            Some(proof) => Some(proof),
            // Not merged, or nothing that could be named to measure it
            // against. git would decline too, and declining destroys nothing.
            None => return Ok(BranchOutcome::Kept),
        }
    };

    let reference = format!("refs/heads/{branch}");
    // `update-ref` does not coordinate with symbolic HEAD updates. Hold the
    // same HEAD.lock files checkout uses until the ref transaction and its
    // readback are complete, otherwise a checkout can pass this test and leave
    // a worktree pointing at the deleted ref. This includes HEADs currently on
    // refs verified by the transaction: a checkout may already hold HEAD.lock
    // while the published HEAD still names one of those refs.
    let mut transaction_refs = vec![reference.clone()];
    if let Some(proof) = &merge {
        transaction_refs.push(proof.reference.clone());
    }
    let mut head_locks = match HeadLocks::acquire(git, root, transaction_refs) {
        Ok(locks) => locks,
        Err(_) => return Ok(BranchOutcome::Kept),
    };
    if checked_out_anywhere(git, root, branch) {
        return Ok(BranchOutcome::Kept);
    }
    let mut branch_config = match BranchConfigGuard::isolate(git, root, branch)? {
        Some(config) => config,
        None => return Ok(BranchOutcome::Kept),
    };

    match delete_ref_atomically(git, root, &reference, &expected, merge.as_ref()) {
        Ok(true) => {
            // Catch administrative entries created while the transaction ran
            // and lock their HEAD before inspecting them. If anything managed
            // to select the branch, put the exact approved ref back rather than
            // leave a dangling symbolic HEAD.
            if head_locks.refresh_after_transaction().is_err()
                || checked_out_anywhere(git, root, branch)
            {
                return rollback_deleted_branch(
                    git,
                    root,
                    branch,
                    &reference,
                    &expected,
                    &mut branch_config,
                    "a worktree selected the branch while it was being deleted",
                );
            }
            if let Err(failure) = branch_config.finish_deletion() {
                return rollback_deleted_branch(
                    git,
                    root,
                    branch,
                    &reference,
                    &expected,
                    &mut branch_config,
                    &format!("its old config could not be removed ({failure})"),
                );
            }
            if branch_exists(git, root, branch) {
                Ok(BranchOutcome::Moved)
            } else {
                Ok(BranchOutcome::Deleted)
            }
        }
        // git declined. Either a ref the proof named is no longer where it was
        // — the race this exists to lose safely — or the delete failed for some
        // other reason and the branch is still there as it was.
        Ok(false) | Err(_) => {
            let config = branch_config.restore();
            if config.is_err() {
                return Err(Error::BranchRollbackFailed {
                    branch: branch.to_string(),
                    cause: config
                        .err()
                        .unwrap_or_else(|| "branch config restoration failed".to_string()),
                    ref_may_have_changed: !branch_still_at(git, root, branch, Some(&expected)),
                    config_may_have_changed: true,
                });
            }
            if branch_still_at(git, root, branch, Some(&expected)) {
                Ok(BranchOutcome::Kept)
            } else {
                Ok(BranchOutcome::Moved)
            }
        }
    }
}

/// Locks every worktree HEAD using the lock name Git itself uses for checkout.
///
/// Holding these does not block ordinary ref reads or the branch ref
/// transaction. It does make a concurrent checkout fail before it can publish
/// a symbolic HEAD, closing the check/delete race `update-ref` alone leaves.
struct HeadLocks {
    common: PathBuf,
    transaction_refs: Vec<String>,
    transaction_complete: bool,
    held: BTreeMap<PathBuf, File>,
}

impl HeadLocks {
    fn acquire(git: &Git, root: &Path, transaction_refs: Vec<String>) -> io::Result<Self> {
        let mut raw = git
            .run(
                root,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .map_err(|failure| io::Error::other(failure.to_string()))?;
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
        }
        if raw.is_empty() {
            return Err(io::Error::other("git returned no common directory"));
        }
        let mut locks = Self {
            common: path_from_git(&raw),
            transaction_refs,
            transaction_complete: false,
            held: BTreeMap::new(),
        };
        locks.refresh()?;
        Ok(locks)
    }

    fn refresh_after_transaction(&mut self) -> io::Result<()> {
        self.transaction_complete = true;
        self.refresh()
    }

    fn refresh(&mut self) -> io::Result<()> {
        // A worktree add creates its administrative directory before writing
        // HEAD. Repeat until every directory in one complete snapshot has a
        // HEAD.lock owned by this guard.
        for _ in 0..8 {
            let wanted = self.head_lock_paths()?;
            for path in &wanted {
                if self.held.contains_key(path) {
                    continue;
                }
                let file = OpenOptions::new().write(true).create_new(true).open(path)?;
                self.held.insert(path.clone(), file);
            }
            if self
                .head_lock_paths()?
                .iter()
                .all(|path| self.held.contains_key(path))
            {
                return Ok(());
            }
        }
        Err(io::Error::other(
            "worktree registrations kept changing while HEADs were locked",
        ))
    }

    fn head_lock_paths(&self) -> io::Result<Vec<PathBuf>> {
        let mut heads = vec![self.common.join("HEAD")];
        let worktrees = self.common.join("worktrees");
        match std::fs::read_dir(&worktrees) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        heads.push(entry.path().join("HEAD"));
                    }
                }
            }
            Err(failure) if failure.kind() == io::ErrorKind::NotFound => {}
            Err(failure) => return Err(failure),
        }
        let mut paths = Vec::new();
        let common_head = self.common.join("HEAD");
        for head in heads {
            // update-ref must own the current worktree's HEAD lock while it
            // verifies or changes the ref HEAD names. It serializes that one
            // itself; all linked-worktree HEADs are ours even when they name a
            // transaction ref. Once the transaction commits, acquire this last
            // lock too before checking for a checkout that raced the commit.
            if !self.transaction_complete && head == common_head {
                let target = std::fs::read_to_string(&head).ok().and_then(|contents| {
                    contents
                        .strip_prefix("ref: ")
                        .map(str::trim)
                        .map(str::to_string)
                });
                if target
                    .as_ref()
                    .is_some_and(|target| self.transaction_refs.contains(target))
                {
                    continue;
                }
            }
            paths.push(head.with_file_name("HEAD.lock"));
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

impl Drop for HeadLocks {
    fn drop(&mut self) {
        for path in self.held.keys() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn restore_deleted_ref(
    git: &Git,
    root: &Path,
    branch: &str,
    reference: &str,
    expected: &str,
) -> std::result::Result<(), String> {
    let absent = "0".repeat(expected.len());
    let update = git.run_status(root, &["update-ref", reference, expected, &absent]);
    if branch_still_at(git, root, branch, Some(expected)) {
        return Ok(());
    }
    Err(match update {
        Ok(out) => format!(
            "git update-ref returned {} and the branch is not back at its approved commit",
            out.code
                .map_or_else(|| "no exit code".to_string(), |code| code.to_string())
        ),
        Err(failure) => format!(
            "git could not restore the branch ref ({failure}) and the branch is not back at its approved commit"
        ),
    })
}

fn rollback_deleted_branch(
    git: &Git,
    root: &Path,
    branch: &str,
    reference: &str,
    expected: &str,
    config: &mut BranchConfigGuard<'_>,
    cause: &str,
) -> Result<BranchOutcome> {
    let reference = restore_deleted_ref(git, root, branch, reference, expected);
    let config = config.restore();
    if reference.is_ok() && config.is_ok() {
        return Ok(BranchOutcome::Kept);
    }
    let mut failures = Vec::new();
    if let Err(failure) = &reference {
        failures.push(failure.clone());
    }
    if let Err(failure) = &config {
        failures.push(failure.clone());
    }
    Err(Error::BranchRollbackFailed {
        branch: branch.to_string(),
        cause: format!("{cause}; {}", failures.join("; ")),
        ref_may_have_changed: reference.is_err(),
        config_may_have_changed: config.is_err(),
    })
}

fn branch_exists(git: &Git, root: &Path, branch: &str) -> bool {
    matches!(
        git.run_status(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("refs/heads/{branch}"),
            ],
        ),
        Ok(out) if out.code == Some(0)
    )
}

/// Config belonging to the branch incarnation whose ref is about to go.
///
/// Moving it aside before deletion means a concurrently recreated branch writes
/// a fresh `branch.<name>` section. Successful cleanup targets only the old
/// tombstone; if ref deletion fails, Drop restores the original section.
struct BranchConfigGuard<'a> {
    git: &'a Git,
    root: &'a Path,
    original: String,
    tombstone: Option<String>,
    expected: Vec<Vec<u8>>,
}

impl<'a> BranchConfigGuard<'a> {
    fn isolate(git: &'a Git, root: &'a Path, branch: &str) -> Result<Option<Self>> {
        static NEXT_TOMBSTONE: AtomicU64 = AtomicU64::new(0);

        let original = format!("branch.{branch}");
        let expected = match config_section_snapshot_checked(git, root, &original) {
            Ok(expected) => expected,
            Err(_) => return Ok(None),
        };
        if expected.is_empty() {
            return Ok(Some(Self {
                git,
                root,
                original,
                tombstone: None,
                expected,
            }));
        }
        let tombstone = (0..128).find_map(|_| {
            let candidate = format!(
                "branch.yawm-deleted-{}-{}",
                std::process::id(),
                NEXT_TOMBSTONE.fetch_add(1, Ordering::Relaxed)
            );
            (!config_section_exists(git, root, &candidate)).then_some(candidate)
        });
        let Some(tombstone) = tombstone else {
            return Ok(None);
        };
        let isolated = matches!(
            git.run_status(
                root,
                &[
                    "config",
                    "--local",
                    "--rename-section",
                    &original,
                    &tombstone,
                ],
            ),
            Ok(out) if out.code == Some(0)
        );
        let mut guard = Self {
            git,
            root,
            original,
            tombstone: Some(tombstone),
            expected,
        };
        let verified = config_section_snapshot_checked(git, root, &guard.original)
            .and_then(|original| {
                let tombstone = guard
                    .tombstone
                    .as_deref()
                    .ok_or_else(|| "config tombstone was not retained".to_string())?;
                let isolated = config_section_snapshot_checked(git, root, tombstone)?;
                Ok(original.is_empty() && isolated == guard.expected)
            })
            .unwrap_or(false);
        if isolated && verified {
            return Ok(Some(guard));
        }
        match guard.restore() {
            Ok(()) => Ok(None),
            Err(failure) => Err(Error::BranchRollbackFailed {
                branch: branch.to_string(),
                cause: format!(
                    "branch config isolation could not be verified and rollback failed ({failure})"
                ),
                ref_may_have_changed: false,
                config_may_have_changed: true,
            }),
        }
    }

    fn finish_deletion(&mut self) -> std::result::Result<(), String> {
        let Some(tombstone) = self.tombstone.as_deref() else {
            return Ok(());
        };
        let removed = matches!(
            self.git.run_status(
                self.root,
                &["config", "--local", "--remove-section", tombstone],
            ),
            Ok(out) if out.code == Some(0)
        );
        let still_exists = config_section_exists_checked(self.git, self.root, tombstone)?;
        if removed && still_exists {
            return Err(format!(
                "git reported success but config section {tombstone} still exists"
            ));
        }
        if !still_exists {
            self.tombstone = None;
            return Ok(());
        }
        Err(format!("config section {tombstone} still exists"))
    }

    fn restore(&mut self) -> std::result::Result<(), String> {
        let Some(tombstone) = self.tombstone.as_deref() else {
            return Ok(());
        };
        let restored = matches!(
            self.git.run_status(
                self.root,
                &[
                    "config",
                    "--local",
                    "--rename-section",
                    tombstone,
                    &self.original,
                ],
            ),
            Ok(out) if out.code == Some(0)
        );
        let original = config_section_snapshot_checked(self.git, self.root, &self.original)?;
        let tombstone_exists = config_section_exists_checked(self.git, self.root, tombstone)?;
        if original == self.expected && !tombstone_exists {
            self.tombstone = None;
            return Ok(());
        }
        Err(format!(
            "git config restoration {} and readback found original_match={} tombstone={}",
            if restored {
                "reported success"
            } else {
                "failed"
            },
            original == self.expected,
            tombstone_exists
        ))
    }
}

fn config_section_exists(git: &Git, root: &Path, section: &str) -> bool {
    config_section_exists_checked(git, root, section).unwrap_or(true)
}

fn config_section_exists_checked(
    git: &Git,
    root: &Path,
    section: &str,
) -> std::result::Result<bool, String> {
    Ok(!config_section_snapshot_checked(git, root, section)?.is_empty())
}

fn config_section_snapshot_checked(
    git: &Git,
    root: &Path,
    section: &str,
) -> std::result::Result<Vec<Vec<u8>>, String> {
    let out = git
        .run_status(
            root,
            &["config", "--local", "--null", "--get-regexp", "^branch\\."],
        )
        .map_err(|failure| failure.to_string())?;
    if out.code == Some(1) {
        return Ok(Vec::new());
    }
    if out.code != Some(0) {
        return Err(format!(
            "git config readback returned {}",
            out.code
                .map_or_else(|| "no exit code".to_string(), |code| code.to_string())
        ));
    }
    let prefix = format!("{section}.").into_bytes();
    let mut values: Vec<Vec<u8>> = out
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let newline = record.iter().position(|byte| *byte == b'\n')?;
            let key = &record[..newline];
            if !key.starts_with(&prefix) {
                return None;
            }
            let suffix = &key[prefix.len()..];
            Some([suffix, b"\n", &record[newline + 1..]].concat())
        })
        .collect();
    values.sort();
    Ok(values)
}

impl Drop for BranchConfigGuard<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// The proof a branch deletion rests on, in the form git can enforce.
///
/// A ref name and the commit it held when the ancestry test read it. The commit
/// is immutable; the ref is not, which is the entire reason both are carried.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MergeProof {
    /// Full ref name — `refs/remotes/origin/main`, `refs/heads/main`, or
    /// whatever namespace a fetch refspec puts the upstream in.
    reference: String,
    oid: String,
}

/// Run the deletion as one ref transaction, stating everything it depends on.
///
/// `--stdin -z` frames each command as `<verb> SP <ref> NUL <oid> NUL`, with
/// the transaction verbs as `<verb> NUL`. NUL cannot occur in a ref name, so
/// nothing needs quoting and no ref name — however many spaces, quotes, or
/// newlines it contains — can be read as a second field.
///
/// `Ok(false)` is git refusing, which is a normal outcome here; `Err` is git
/// not running at all. Neither deleted anything.
fn delete_ref_atomically(
    git: &Git,
    root: &Path,
    reference: &str,
    expected: &str,
    merge: Option<&MergeProof>,
) -> Result<bool> {
    let mut input = Vec::new();
    push_ref_command(&mut input, "start", &[]);
    if let Some(merge) = merge {
        push_ref_command(&mut input, "verify", &[&merge.reference, &merge.oid]);
    }
    push_ref_command(&mut input, "delete", &[reference, expected]);
    // Taking the locks and committing them are separate so that a failure to
    // lock either ref happens before anything has been written.
    push_ref_command(&mut input, "prepare", &[]);
    push_ref_command(&mut input, "commit", &[]);

    let out = git.run_status_with_input(root, &["update-ref", "-z", "--stdin"], &input)?;
    Ok(out.code == Some(0))
}

fn push_ref_command(out: &mut Vec<u8>, command: &str, args: &[&str]) {
    out.extend_from_slice(command.as_bytes());
    if let Some((first, rest)) = args.split_first() {
        out.push(b' ');
        out.extend_from_slice(first.as_bytes());
        out.push(0);
        for arg in rest {
            out.extend_from_slice(arg.as_bytes());
            out.push(0);
        }
        return;
    }
    out.push(0);
}

/// Whether any worktree of this repository has the branch checked out.
///
/// Unreadable listing counts as "yes": keeping a branch costs nothing, and
/// deleting one out from under a checked-out worktree leaves git with a HEAD
/// pointing at a ref that does not exist.
fn checked_out_anywhere(git: &Git, root: &Path, branch: &str) -> bool {
    match list_worktrees(git, root) {
        Ok(entries) => entries
            .iter()
            .any(|entry| entry.branch.as_deref() == Some(branch)),
        Err(_) => true,
    }
}

/// Git's own `branch -d` safety question, answered from the approved snapshot.
///
/// The reference is the configured upstream when one exists and still exists on
/// the remote, and the ref this repository's HEAD resolves to otherwise — the
/// rule `git branch -d` applies. Both sides of the ancestry test are object
/// names, so the answer describes a fixed pair of commits rather than whatever
/// the refs say by the time the deletion runs.
///
/// Returns what the answer rests on rather than a bare yes: the ref that was
/// measured against and the commit it held while it was measured, so the
/// deletion can require both to still be true of the ref database.
fn merged_at_approval(
    git: &Git,
    root: &Path,
    plan: &RemovalPlan,
    branch_oid: &str,
) -> Option<MergeProof> {
    let evidence = plan.state.evidence()?;
    // Nothing that can be named to measure against — an upstream whose ref the
    // listing could not name, a repository whose HEAD could not be read. git
    // would decline as well, and declining destroys nothing.
    let proof = merge_reference_of(git, root, evidence)?;

    matches!(
        git.run_status(
            root,
            &["merge-base", "--is-ancestor", branch_oid, &proof.oid],
        ),
        Ok(out) if out.code == Some(0)
    )
    .then_some(proof)
}

/// The ref a deletion is decided against, and the commit it was read at.
///
/// Taken from the approved snapshot when the plan carries one — a plan built
/// with repository context records it, custom namespaces included. A status
/// gathered without that context has only the upstream to go on, and a
/// repository whose HEAD is the reference is read here instead. What is never
/// done is answering with a commit that no ref can be named for: an unverifiable
/// proof is not one, and the deletion declines rather than proceeding on it.
fn merge_reference_of(git: &Git, root: &Path, evidence: &StateEvidence) -> Option<MergeProof> {
    if let (Some(reference), Some(oid)) = (&evidence.merge_ref, &evidence.merge_oid) {
        return Some(MergeProof {
            reference: reference.clone(),
            oid: oid.clone(),
        });
    }
    if evidence.upstream.is_some() && !evidence.upstream_gone {
        let (Some(reference), Some(oid)) = (&evidence.upstream_ref, &evidence.upstream_oid) else {
            return None;
        };
        return Some(MergeProof {
            reference: reference.clone(),
            oid: oid.clone(),
        });
    }
    head_reference(git, root).map(|reference| MergeProof {
        reference: reference.name,
        oid: reference.oid,
    })
}

/// Whether the branch still points at the commit the removal was authorised
/// against.
///
/// `expected` is `None` when the approved plan never resolved the ref, and an
/// authorisation that never named a commit cannot be said to still hold — so
/// that is a refusal too, not a pass.
fn branch_still_at(git: &Git, root: &Path, branch: &str, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let Ok(out) = git.run(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &format!("refs/heads/{branch}"),
        ],
    ) else {
        return false;
    };
    String::from_utf8_lossy(&out).trim() == expected
}

/// Drop administrative data for worktrees whose directories are gone.
pub fn prune(git: &Git, root: &Path) -> Result<()> {
    git.run(root, &["worktree", "prune"])?;
    Ok(())
}

/// Reveal a path in the system file manager.
pub fn reveal(path: &Path) -> Result<()> {
    open::that_detached(path).map_err(Error::Io)
}

/// Open a path with a specific command, or the system default.
pub fn open_with(path: &Path, program: Option<&str>) -> Result<()> {
    match program {
        Some(program) => open::with_detached(path, program).map_err(Error::Io),
        None => open::that_detached(path).map_err(Error::Io),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DirtyCounts, LockInfo, ProcessInfo, UpstreamInfo};

    fn entry() -> WorktreeEntry {
        WorktreeEntry {
            path: "/w/feature".into(),
            branch: Some("feat/x".into()),
            ..Default::default()
        }
    }

    fn plan_from(entry: &WorktreeEntry, status: &WorktreeStatus) -> RemovalPlan {
        // Path does not exist, so the dirty file listing is empty; the counts
        // come from the status that was passed in.
        plan_removal(&Git::new(), entry, status)
    }

    #[test]
    fn a_clean_worktree_needs_no_force() {
        let plan = plan_from(&entry(), &WorktreeStatus::default());
        assert!(!plan.requires_force);
        assert!(!plan.destroys_work());
        assert!(plan.is_allowed());
    }

    #[test]
    fn uncommitted_changes_require_force() {
        let status = WorktreeStatus {
            dirty: DirtyCounts {
                unstaged: 3,
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = plan_from(&entry(), &status);

        assert!(plan.requires_force);
        assert!(plan.destroys_work());
        assert_eq!(plan.dirty_total, 3);
    }

    #[test]
    fn unknown_dirty_state_requires_force() {
        let status = WorktreeStatus {
            dirty: DirtyCounts {
                inspection_failed: true,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(plan_from(&entry(), &status).requires_force);
    }

    #[test]
    fn a_locked_worktree_needs_unlocking_rather_than_forcing() {
        let mut e = entry();
        e.locked = Some(LockInfo {
            reason: Some("agent running".into()),
        });
        let plan = plan_from(&e, &WorktreeStatus::default());

        assert!(plan.is_locked);
        assert_eq!(plan.lock_reason.as_deref(), Some("agent running"));
        assert!(
            !plan.requires_force,
            "a lock is not a question about uncommitted files, so it must not \
             be answerable by confirming them"
        );
    }

    /// Force answers a question about edited files. A lock is a different
    /// question, asked by a person or an agent on purpose, and it needs its
    /// own answer.
    #[test]
    fn confirming_uncommitted_changes_does_not_lift_a_lock() {
        let mut e = entry();
        e.locked = Some(LockInfo {
            reason: Some("agent running".into()),
        });
        let plan = plan_from(&e, &WorktreeStatus::default());

        let err = check_authorised(
            &plan,
            &RemoveOptions {
                force: true,
                ..Default::default()
            },
        )
        .expect_err("force alone must not authorise unlocking");
        let message = err.to_string();
        assert!(message.contains("locked"), "got {message}");
        assert!(
            message.contains("agent running"),
            "the reason someone gave is the whole point of showing it: {message}"
        );

        check_authorised(
            &plan,
            &RemoveOptions {
                force: true,
                unlock: true,
                ..Default::default()
            },
        )
        .expect("an explicit unlock authorises it");
    }

    /// A worktree whose directory is already gone still holds a lock in git's
    /// administrative data, and `git worktree prune` refuses to touch it.
    #[test]
    fn a_locked_worktree_with_no_directory_still_needs_the_lock_lifted() {
        let mut e = entry();
        e.locked = Some(LockInfo { reason: None });
        e.prunable = Some("gitdir file points to non-existent location".into());
        let plan = plan_from(&e, &WorktreeStatus::default());

        assert!(plan.is_prunable);
        assert!(
            check_authorised(&plan, &RemoveOptions::default()).is_err(),
            "pruning it away is still removing something that was locked"
        );
        check_authorised(
            &plan,
            &RemoveOptions {
                unlock: true,
                ..Default::default()
            },
        )
        .expect("authorised");
    }

    /// Gitignored env files exist nowhere else, so their loss counts as
    /// destroying work even when git reports the worktree as clean.
    #[test]
    fn env_files_count_as_destroyed_work() {
        let status = WorktreeStatus {
            env_files: vec![".env".into(), ".env.local".into()],
            ..Default::default()
        };
        let plan = plan_from(&entry(), &status);

        assert!(plan.destroys_work());
        assert!(plan.requires_force);
        assert_eq!(plan.env_files.len(), 2);
    }

    #[test]
    fn unpushed_commits_count_as_destroyed_work() {
        let status = WorktreeStatus {
            upstream: UpstreamInfo {
                ahead: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = plan_from(&entry(), &status);

        assert!(plan.destroys_work());
        assert_eq!(plan.unpushed_commits, 2);
        // Unpushed commits are not lost to the filesystem, so git itself does
        // not refuse; the warning is yawm's own.
        assert!(!plan.requires_force);
    }

    #[test]
    fn running_processes_are_reported() {
        let status = WorktreeStatus {
            processes: vec![ProcessInfo {
                pid: 1,
                name: "node".into(),
            }],
            ..Default::default()
        };
        assert_eq!(plan_from(&entry(), &status).running_processes, 1);
    }

    #[test]
    fn the_main_worktree_is_never_removable() {
        let mut e = entry();
        e.is_main = true;
        let plan = plan_from(&e, &WorktreeStatus::default());

        assert!(!plan.is_allowed());
        let err = remove(
            &Git::new(),
            Path::new("/repo"),
            &plan,
            RemoveOptions::default(),
        );
        assert!(err.is_err(), "removing the main worktree must fail");
    }

    /// Force must never be implicit: a caller that has not shown the user what
    /// would be lost gets an error instead of a deletion.
    #[test]
    fn removal_refuses_without_explicit_force() {
        let status = WorktreeStatus {
            dirty: DirtyCounts {
                unstaged: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = plan_from(&entry(), &status);
        let result = remove(
            &Git::new(),
            Path::new("/repo"),
            &plan,
            RemoveOptions::default(),
        );

        assert!(result.is_err());
    }

    fn plan_with(dirty: &[&str], env: &[&str]) -> RemovalPlan {
        RemovalPlan {
            path: "/w/feature".into(),
            branch: Some("feat/x".into()),
            dirty_files: dirty.iter().map(|s| s.to_string()).collect(),
            dirty_total: dirty.len(),
            env_files: env.iter().map(|s| s.to_string()).collect(),
            // Sealed rather than defaulted: a default fingerprint is the one a
            // plan that was never inspected has, and it authorises nothing.
            // These cases are about the readable fields, so they carry a state
            // that was established.
            state: StateEvidence::default().seal(),
            ..Default::default()
        }
    }

    #[test]
    fn an_unchanged_worktree_reports_no_differences() {
        let plan = plan_with(&["a.txt"], &[".env"]);
        assert!(describe_changes(&plan, &plan.clone()).is_empty());
    }

    #[test]
    fn a_file_that_appeared_after_planning_is_named() {
        let approved = plan_with(&["a.txt"], &[]);
        let current = plan_with(&["a.txt", "late.txt"], &["apps/api/.env"]);
        let changes = describe_changes(&approved, &current);

        assert!(
            changes.iter().any(|c| c.contains("late.txt")),
            "got {changes:?}"
        );
        assert!(
            changes.iter().any(|c| c.contains("apps/api/.env")),
            "got {changes:?}"
        );
    }

    /// One file swapped for another leaves the count alone, so the count on its
    /// own is not enough to notice.
    #[test]
    fn a_swapped_file_is_noticed_even_though_the_count_matches() {
        let approved = plan_with(&["a.txt"], &[]);
        let current = plan_with(&["b.txt"], &[]);
        let changes = describe_changes(&approved, &current);

        assert!(
            changes.iter().any(|c| c.contains("b.txt")),
            "got {changes:?}"
        );
    }

    #[test]
    fn a_process_that_started_or_stopped_is_not_a_difference() {
        let approved = plan_with(&["a.txt"], &[]);
        let current = RemovalPlan {
            running_processes: 4,
            ..plan_with(&["a.txt"], &[])
        };

        assert!(describe_changes(&approved, &current).is_empty());
    }

    /// A lock that stays on but now says something else is a new instruction,
    /// and removing the worktree lifts the new one rather than the one the
    /// user read.
    #[test]
    fn a_lock_reason_that_changed_is_a_difference() {
        let locked = |reason: Option<&str>| RemovalPlan {
            is_locked: true,
            lock_reason: reason.map(str::to_string),
            ..plan_with(&[], &[])
        };

        let changes = describe_changes(
            &locked(Some("agent running")),
            &locked(Some("do not touch")),
        );
        assert!(
            changes.iter().any(|c| c.contains("do not touch")),
            "got {changes:?}"
        );

        assert!(
            describe_changes(
                &locked(Some("agent running")),
                &locked(Some("agent running"))
            )
            .is_empty()
        );
        assert!(
            !describe_changes(&plan_with(&[], &[]), &plan_with(&[], &[]))
                .iter()
                .any(|c| c.contains("lock")),
            "an unlocked worktree has no lock to have changed"
        );
    }

    /// Newly locked says why, because "it has been locked" alone does not tell
    /// the user whether to leave it alone.
    #[test]
    fn a_worktree_locked_after_planning_reports_its_reason() {
        let approved = plan_with(&[], &[]);
        let current = RemovalPlan {
            is_locked: true,
            lock_reason: Some("agent running".into()),
            ..plan_with(&[], &[])
        };

        let changes = describe_changes(&approved, &current);
        assert!(
            changes.iter().any(|c| c.contains("agent running")),
            "got {changes:?}"
        );
    }

    /// Five selected worktrees and a refusal naming one of them is not enough
    /// to know which to look at.
    #[test]
    fn a_batch_refusal_names_every_worktree_that_changed() {
        let err = plan_changed(
            vec![
                (
                    "/w/alpha".into(),
                    vec!["new uncommitted files: late.txt".to_string()],
                ),
                ("/w/beta".into(), vec!["it has been locked".to_string()]),
            ],
            vec!["/w/alpha".into(), "/w/beta".into()],
        );

        let Error::PlanChanged { changes, .. } = &err else {
            panic!("must stay a PlanChanged: {err:?}");
        };
        assert_eq!(changes.len(), 2);
        assert!(
            changes.iter().any(|c| c.starts_with("alpha: ")),
            "got {changes:?}"
        );
        assert!(
            changes.iter().any(|c| c.starts_with("beta: ")),
            "got {changes:?}"
        );

        let message = err.to_string();
        assert!(message.contains(PLAN_CHANGED_MARKER), "got {message}");
        assert!(message.contains("late.txt"), "got {message}");
    }

    /// The refusal has to say what the repository has *now*, because the caller
    /// re-plans on it. A worktree deleted from outside yawm while the dialog
    /// was open used to be re-planned regardless, and core answering "that is
    /// not a worktree of this repository" replaced a refusal the user could act
    /// on with what reads like a bug.
    #[test]
    fn a_refusal_carries_the_worktrees_that_are_still_there() {
        let err = plan_changed(
            vec![(
                "/w/gone".into(),
                vec!["it is no longer a worktree of this repository".to_string()],
            )],
            vec!["/w/alpha".into(), "/w/beta".into()],
        );

        let Error::PlanChanged { still_present, .. } = &err else {
            panic!("must stay a PlanChanged: {err:?}");
        };
        assert_eq!(
            still_present,
            &[PathBuf::from("/w/alpha"), PathBuf::from("/w/beta")]
        );
        assert!(
            !still_present.contains(&PathBuf::from("/w/gone")),
            "the path that has gone must not be offered for a re-plan"
        );
    }

    /// One changed worktree keeps the sentence it always had; prefixing a lone
    /// difference with a name the message already opens with reads as noise.
    #[test]
    fn a_single_changed_worktree_is_reported_as_it_always_was() {
        let err = plan_changed(
            vec![(
                "/w/alpha".into(),
                vec!["new uncommitted files: late.txt".to_string()],
            )],
            vec!["/w/alpha".into()],
        );

        let Error::PlanChanged { path, changes, .. } = &err else {
            panic!("must stay a PlanChanged: {err:?}");
        };
        assert_eq!(path, Path::new("/w/alpha"));
        assert_eq!(changes, &["new uncommitted files: late.txt".to_string()]);
    }

    /// The desktop app only ever sees the rendered string, so the marker it
    /// matches on has to stay in it.
    #[test]
    fn the_plan_changed_message_carries_its_marker() {
        let err = Error::PlanChanged {
            path: "/w/feature".into(),
            changes: vec!["new uncommitted files: late.txt".to_string()],
            still_present: vec!["/w/feature".into()],
        };
        let message = err.to_string();

        assert!(message.contains(PLAN_CHANGED_MARKER), "got {message}");
        assert!(message.contains("late.txt"), "got {message}");
    }

    /// A failure part-way through a batch has to say what is already gone.
    /// Reported as an ordinary error, the caller re-renders the selection it
    /// started with and tells the user nothing happened.
    #[test]
    fn a_partial_batch_failure_names_what_was_already_removed() {
        let err = Error::BatchIncomplete(Box::new(PartialRemoval {
            completed: vec![CompletedRemoval::removed(
                "/w/alpha".into(),
                RemovalOutcome {
                    branch: BranchOutcome::Kept,
                },
            )],
            vanished: vec!["/w/gamma".into()],
            failed: "/w/beta".into(),
            cause: Box::new(Error::Parse("git refused".to_string())),
        }));

        let message = err.to_string();
        assert!(message.contains("alpha"), "got {message}");
        assert!(message.contains("beta"), "got {message}");
        assert!(message.contains("git refused"), "got {message}");
        assert!(
            !message.contains(PLAN_CHANGED_MARKER),
            "a partial failure must never read as 'nothing was deleted': {message}"
        );
        assert!(
            message.contains("gamma") && message.contains("something else"),
            "a worktree that disappeared under the batch is not a yawm removal: {message}"
        );
    }

    #[test]
    fn branch_deletion_waits_for_a_checkout_locked_on_the_merge_ref() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repository");
        let racer = dir.path().join("racer");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let git = Git::new();
        git.run(&root, &["init", "-q", "-b", "main"]).unwrap();
        git.run(&root, &["config", "user.email", "test@yawm.dev"])
            .unwrap();
        git.run(&root, &["config", "user.name", "yawm test"])
            .unwrap();
        std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
        git.run(&root, &["add", "tracked.txt"]).unwrap();
        git.run(&root, &["commit", "-qm", "base"]).unwrap();
        git.run(&root, &["branch", "feature"]).unwrap();
        let racer_arg = racer.to_string_lossy();
        git.run(
            &root,
            &["worktree", "add", "-q", "-b", "review", racer_arg.as_ref()],
        )
        .unwrap();
        let oid = String::from_utf8(git.run(&root, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_string();
        let mut admin = git
            .run(&racer, &["rev-parse", "--absolute-git-dir"])
            .unwrap();
        while matches!(admin.last(), Some(b'\n' | b'\r')) {
            admin.pop();
        }
        let admin = path_from_git(&admin);
        assert_eq!(
            std::fs::read_to_string(admin.join("HEAD")).unwrap(),
            "ref: refs/heads/review\n"
        );

        let plan = RemovalPlan {
            branch: Some("feature".to_string()),
            state: StateEvidence {
                branch_oid: Some(oid.clone()),
                merge_ref: Some("refs/heads/review".to_string()),
                merge_oid: Some(oid),
                ..Default::default()
            }
            .seal(),
            ..Default::default()
        };
        std::fs::write(admin.join("HEAD.lock"), "ref: refs/heads/feature\n").unwrap();

        assert_eq!(
            delete_branch(&git, &root, &plan, false).unwrap(),
            BranchOutcome::Kept
        );
        assert!(
            branch_exists(&git, &root, "feature"),
            "the checkout may still publish this branch into HEAD"
        );
    }

    /// The fingerprint is the authorisation, so it has to move for the changes
    /// the plan's readable fields cannot express. Each case below is a real way
    /// to destroy different work than the user agreed to lose while every
    /// count, name, and flag the dialog rendered stays identical.
    #[test]
    fn the_state_fingerprint_moves_where_the_visible_plan_does_not() {
        let base = StateEvidence {
            dirty: vec![DirtyIdentity {
                path: "a.txt".to_string(),
                codes: vec![" M".to_string()],
                stages: vec!["0 100644 aaa".to_string()],
                content: "blob:111".to_string(),
            }],
            env: vec![FileIdentity {
                path: ".env".to_string(),
                content: "blob:222".to_string(),
            }],
            head: Some("head-1".to_string()),
            branch: Some("feat/x".to_string()),
            branch_oid: Some("ref-1".to_string()),
            upstream: Some("origin/feat/x".to_string()),
            upstream_oid: Some("up-1".to_string()),
            ahead: 1,
            ..Default::default()
        };
        let sealed = base.clone().seal();
        let moved = |mutate: fn(&mut StateEvidence)| {
            let mut next = base.clone();
            mutate(&mut next);
            let next = next.seal();
            assert_ne!(
                sealed.digest, next.digest,
                "the digest is the backstop and has to move too"
            );
            assert!(
                !describe_state(&sealed, &next).is_empty(),
                "a change that destroys different work must be named"
            );
        };

        // Same name, same status, different bytes on disk.
        moved(|state| state.dirty[0].content = "blob:999".to_string());
        // Staged something else under the same path.
        moved(|state| state.dirty[0].stages = vec!["0 100644 zzz".to_string()]);
        // The same path resolved to a different side of its conflict.
        moved(|state| {
            state.dirty[0].stages = vec![
                "1 100644 aaa".to_string(),
                "2 100644 bbb".to_string(),
                "3 100644 ccc".to_string(),
            ]
        });
        // A file outside git replaced under the same name.
        moved(|state| state.env[0].content = "blob:333".to_string());
        // Amended the single unpushed commit: still one ahead, different work.
        moved(|state| {
            state.head = Some("head-2".to_string());
            state.branch_oid = Some("ref-2".to_string());
        });
        // The branch ref moved while the worktree stayed put.
        moved(|state| state.branch_oid = Some("ref-2".to_string()));
        // The upstream moved, so what "unpushed" means moved with it.
        moved(|state| state.upstream_oid = Some("up-2".to_string()));

        assert!(
            describe_state(&sealed, &base.seal()).is_empty(),
            "an unchanged state must not read as changed"
        );
    }

    #[test]
    fn fingerprint_revalidates_files_hashed_earlier_in_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = Git::new();
        git.run(&root, &["init", "-q"]).unwrap();
        let first = root.join("a.txt");
        std::fs::write(&first, "approved\n").unwrap();
        std::fs::write(root.join("z.txt"), vec![b'z'; 256 * 1024]).unwrap();
        let dirty = scan_dirty(&git, &root);
        assert_eq!(dirty.paths.len(), 2);
        let entry = WorktreeEntry {
            path: root.clone(),
            ..Default::default()
        };

        let state = StateFingerprint::of_after_snapshot(
            &git,
            &root,
            &entry,
            &WorktreeStatus::default(),
            &dirty,
            &mut || std::fs::write(&first, "changed after its hash\n").unwrap(),
        );

        assert!(state.unproven);
        assert!(!state.is_proven());
        assert!(
            state
                .evidence()
                .unwrap()
                .unproven
                .iter()
                .any(|reason| reason.contains("a.txt")
                    && reason.contains("rest of the removal snapshot")),
            "the mutation is attributed to the path that moved"
        );
    }

    #[test]
    fn fingerprint_refuses_a_new_file_created_mid_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = Git::new();
        git.run(&root, &["init", "-q"]).unwrap();
        let entry = list_worktrees(&git, &root).unwrap().remove(0);
        let dirty = scan_dirty(&git, &root);
        assert!(dirty.paths.is_empty());

        let state = StateFingerprint::of_after_snapshot(
            &git,
            &root,
            &entry,
            &WorktreeStatus::default(),
            &dirty,
            &mut || std::fs::write(root.join(".env.late"), "SECRET=late\n").unwrap(),
        );

        let reasons = &state.evidence().unwrap().unproven;
        assert!(!state.is_proven());
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("uncommitted-file inventory")),
            "the lossless git inventory changed: {reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("environment-file inventory")),
            "the environment inventory changed: {reasons:?}"
        );
    }

    /// Every conflict stage is part of the authorisation.
    ///
    /// Three stages of one path name three different blobs, and only the last
    /// one read used to be kept — so resolving a conflict to entirely different
    /// bytes left this identical and a force authorisation carried over it.
    #[test]
    fn each_stage_of_a_conflicted_path_is_part_of_the_state() {
        let conflicted = |stages: Vec<&str>| {
            StateEvidence {
                dirty: vec![DirtyIdentity {
                    path: "merge.txt".to_string(),
                    codes: vec!["UU".to_string()],
                    stages: stages.into_iter().map(str::to_string).collect(),
                    content: "blob:111".to_string(),
                }],
                ..Default::default()
            }
            .seal()
        };

        let before = conflicted(vec!["1 100644 base", "2 100644 ours", "3 100644 theirs"]);
        // Re-merged against a different ancestor: stage 2 and 3 unchanged, and
        // the file on disk unchanged, but what a resolution would mean is not.
        let after = conflicted(vec!["1 100644 other", "2 100644 ours", "3 100644 theirs"]);
        assert_ne!(before.digest, after.digest);
        assert!(!describe_state(&before, &after).is_empty());

        // The old shape, keeping one entry per path, could not tell these apart.
        let last_only = conflicted(vec!["3 100644 theirs"]);
        assert_ne!(before.digest, last_only.digest);
    }

    /// An inspection that failed is not an inspection that matched. Two
    /// identical fingerprints still refuse when either was never established,
    /// so a force authorisation cannot be carried over an unread state.
    #[test]
    fn an_unproven_state_never_authorises_anything() {
        let unproven = StateEvidence {
            unproven: vec!["a.txt could not be read (permission denied)".to_string()],
            ..Default::default()
        }
        .seal();
        assert!(unproven.unproven, "the flag crosses the boundary too");
        assert!(!unproven.is_proven());

        let changes = describe_state(&unproven, &unproven);
        assert!(
            !changes.is_empty(),
            "an identical but unproven state must still refuse"
        );
        assert!(
            changes.iter().any(|c| c.contains("a.txt")),
            "the refusal has to say what could not be read: {changes:?}"
        );

        // And after the round trip that drops the evidence, the flag alone is
        // still enough to refuse.
        let returned = StateFingerprint {
            version: unproven.version.clone(),
            digest: unproven.digest.clone(),
            unproven: true,
            evidence: None,
        };
        assert!(!describe_state(&returned, &unproven).is_empty());
    }

    /// A plan that came back from a frontend carries a digest and nothing else.
    /// It still has to authorise exactly the state it was built from, and
    /// refuse everything else.
    #[test]
    fn a_returned_plan_authorises_by_digest_alone() {
        let evidence = StateEvidence {
            dirty: vec![DirtyIdentity {
                path: "a.txt".to_string(),
                codes: vec![" M".to_string()],
                stages: vec!["0 100644 aaa".to_string()],
                content: "blob:111".to_string(),
            }],
            head: Some("head-1".to_string()),
            ..Default::default()
        };
        let built = evidence.clone().seal();
        let returned: StateFingerprint =
            serde_json::from_str(&serde_json::to_string(&built).unwrap()).unwrap();
        assert!(returned.evidence().is_none(), "the detail stays in-process");
        assert_eq!(returned.digest, built.digest);

        assert!(
            describe_state(&returned, &built).is_empty(),
            "the same state must still authorise"
        );

        let mut rewritten = evidence;
        rewritten.dirty[0].content = "blob:999".to_string();
        let rewritten = rewritten.seal();
        // No evidence on the approved side, so nothing names the file — the
        // digests still disagree, and `describe_changes` says so generically.
        assert_ne!(returned.digest, rewritten.digest);
    }

    /// A payload from a build that encoded state differently, or none at all,
    /// authorises nothing.
    #[test]
    fn a_state_of_an_unknown_version_authorises_nothing() {
        let current = StateEvidence::default().seal();

        let absent = StateFingerprint::default();
        assert!(!absent.is_proven());
        assert!(
            !describe_state(&absent, &current).is_empty(),
            "an absent fingerprint is not a state anyone approved"
        );

        let older = StateFingerprint {
            version: "yawm.state.v0".to_string(),
            digest: current.digest.clone(),
            unproven: false,
            evidence: None,
        };
        assert!(
            !describe_state(&older, &current).is_empty(),
            "a digest that means something else is not a matching digest"
        );
    }

    /// Canonical encoding must not let one arrangement of values impersonate
    /// another; lengths precede content precisely so it cannot.
    #[test]
    fn canonical_encoding_cannot_be_confused_between_states() {
        let one = StateEvidence {
            branch: Some("a".to_string()),
            branch_oid: Some("bc".to_string()),
            ..Default::default()
        };
        let two = StateEvidence {
            branch: Some("ab".to_string()),
            branch_oid: Some("c".to_string()),
            ..Default::default()
        };
        assert_ne!(one.canonical(), two.canonical());
    }
}
