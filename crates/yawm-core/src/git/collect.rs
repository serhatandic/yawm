//! Turning git invocations into the signals the verdict engine consumes.
//!
//! Work is split into phases so a caller can stream results: enumerate
//! worktrees first (fast, one git call), then load shared branch data (one more
//! call for the whole repository), then gather per-worktree status. The desktop
//! app paints the list after phase one and fills in badges as later phases
//! land.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{Error, MovedWorktreeDiagnostic, Result};
use crate::git::landing::{
    FocusedPatch, LandingCache, LandingContext, LandingDepth, ResolvedTarget, uncommitted_against,
};
use crate::git::managed::inspect_links;
use crate::git::porcelain::parse_worktree_list;
use crate::git::refs::{
    BranchInfo, FOR_EACH_REF_FORMAT, FOR_EACH_REF_NAMESPACES, REF_OID_FORMAT, parse_for_each_ref,
    parse_ref_oids,
};
use crate::git::status::{
    IndexEntry, StatusEntry, count_status, flagged, matches_index_blob, parse_index, parse_status,
};
use crate::git::{Git, execution_context};
use crate::model::{
    HeadState, Landing, LandingFacts, LandingTargetFact, MergeReference, ProofPhase, UnknownReason,
    UpstreamInfo, UpstreamState, WorktreeEntry, WorktreeStatus,
};

/// Everything reading one worktree's status needs to know about its repository.
///
/// Split out of [`RepoContext`] because this is all a removal plan ever reads.
/// Resolving the merge refs and reading the merge configuration on top of it is
/// around a dozen git invocations, and the answer the delete dialog gives never
/// depends on any of them.
#[derive(Debug, Clone)]
pub struct BranchContext {
    /// Path of the main worktree. Always safe to run git in, even when a linked
    /// worktree's directory has been deleted.
    pub root: PathBuf,
    pub branches: HashMap<String, BranchInfo>,
    /// The ref HEAD resolves to in the main worktree, pinned to its commit.
    ///
    /// This is what `git branch -d` falls back to when a branch has no upstream
    /// left to be merged into, and it is a property of the repository rather
    /// than of any one worktree, so it is read once here.
    pub head_ref: Option<MergeReference>,
}

/// Shared, repository-wide context gathered once and reused for every worktree.
#[derive(Debug, Clone)]
pub struct RepoContext {
    /// What a status read is resolved against.
    pub branch: BranchContext,
    /// Primary default ref, e.g. `origin/main`. For display.
    pub default_ref: Option<String>,
    /// Every ref ancestry is tested against.
    ///
    /// Both the remote and local default branches are checked, because work
    /// merged into local `main` but not yet pushed is still preserved outside
    /// the worktree, and deleting it would lose nothing.
    pub merge_refs: Vec<String>,
    default_target_oid: Option<String>,
    landing: LandingContext,
}

impl RepoContext {
    /// Path of the main worktree.
    pub fn root(&self) -> &Path {
        &self.branch.root
    }

    pub fn branches(&self) -> &HashMap<String, BranchInfo> {
        &self.branch.branches
    }

    /// Uses the object ID already returned by `git worktree list`, avoiding a
    /// subprocess before an immutable cache lookup.
    pub fn focused_patch_for_head_oid(
        &self,
        git: &Git,
        head: &str,
        max_bytes: usize,
    ) -> FocusedPatch {
        self.landing
            .focused_patch_for_head_oid(git, self.root(), head, max_bytes)
    }
}

/// List every worktree of the repository containing `path`.
pub fn list_worktrees(git: &Git, path: &Path) -> Result<Vec<WorktreeEntry>> {
    if let Some(diagnostic) = moved_worktree_diagnostic(git, path) {
        return Err(Error::MovedWorktree { diagnostic });
    }
    let mut args: Vec<&str> = vec!["worktree", "list", "--porcelain"];
    if git.supports_nul_worktree_list() {
        args.push("-z");
    }
    let out = git.run(path, &args)?;
    let mut entries = parse_worktree_list(&out);

    if entries.is_empty() {
        return Err(Error::NotARepository(path.to_path_buf()));
    }
    let repository = entries
        .iter()
        .find(|entry| entry.is_main)
        .map(|entry| entry.path.clone());
    for entry in &mut entries {
        entry.repository = repository.clone();
    }
    Ok(entries)
}

fn moved_worktree_diagnostic(git: &Git, observed: &Path) -> Option<MovedWorktreeDiagnostic> {
    if !observed.join(".git").is_file() {
        return None;
    }
    let context = execution_context(observed)?;
    let recorded_git_file = std::fs::read_to_string(context.git_dir.join("gitdir")).ok()?;
    let recorded_git_file = PathBuf::from(recorded_git_file.trim());
    if recorded_git_file.as_os_str().is_empty() {
        return None;
    }
    let recorded_git_file = if recorded_git_file.is_absolute() {
        recorded_git_file
    } else {
        context.git_dir.join(recorded_git_file)
    };
    let recorded_git_file = std::fs::canonicalize(&recorded_git_file).unwrap_or(recorded_git_file);
    let observed_git_file = observed.join(".git");
    let observed_git_file = std::fs::canonicalize(&observed_git_file).unwrap_or(observed_git_file);
    if crate::path::path_key(&recorded_git_file) == crate::path::path_key(&observed_git_file) {
        return None;
    }

    let mut args = vec!["worktree", "list", "--porcelain"];
    if git.supports_nul_worktree_list() {
        args.push("-z");
    }
    let listed = git.run(observed, &args).ok()?;
    let main_worktree = parse_worktree_list(&listed).first()?.path.clone();
    let observed_path = std::fs::canonicalize(observed).unwrap_or_else(|_| observed.to_path_buf());
    Some(MovedWorktreeDiagnostic {
        repair_command: vec![
            "git".to_string(),
            "-C".to_string(),
            main_worktree.display().to_string(),
            "worktree".to_string(),
            "repair".to_string(),
            observed_path.display().to_string(),
        ],
        main_worktree,
        common_admin_dir: context.common_dir,
        observed_path,
    })
}

/// Gather the repository-wide context.
pub fn load_context(git: &Git, path: &Path, worktrees: &[WorktreeEntry]) -> Result<RepoContext> {
    load_context_with_cache(git, path, worktrees, LandingCache::default())
}

/// Gather only what reading a worktree's status needs.
///
/// One git invocation for the whole repository, and the caller that only wants
/// to know what deleting a worktree would cost stops there.
pub fn load_branch_context(
    git: &Git,
    path: &Path,
    worktrees: &[WorktreeEntry],
) -> Result<BranchContext> {
    let root = worktrees
        .first()
        .map(|w| w.path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let branches = load_branches(git, &root).unwrap_or_default();
    let head_ref = head_reference(git, &root);
    Ok(BranchContext {
        root,
        branches,
        head_ref,
    })
}

/// The ref HEAD points at in `root`, with the commit it currently holds.
///
/// Named by its full ref rather than by `HEAD` so a later transaction verifies
/// the branch itself and not whatever HEAD has come to mean. A detached HEAD
/// has no such ref, and then `HEAD` is the only name there is — still a ref a
/// transaction can verify, which is what matters.
pub(crate) fn head_reference(git: &Git, root: &Path) -> Option<MergeReference> {
    let name = git
        .run_status(root, &["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .filter(|out| out.code == Some(0))
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "HEAD".to_string());

    let out = git
        .run_status(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &name,
            ],
        )
        .ok()?;
    if out.code != Some(0) {
        return None;
    }
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!oid.is_empty()).then_some(MergeReference { name, oid })
}

pub fn load_context_with_cache(
    git: &Git,
    path: &Path,
    worktrees: &[WorktreeEntry],
    landing_cache: LandingCache,
) -> Result<RepoContext> {
    // Derived here rather than alongside the landing work, so the plan path and
    // the scan path can never disagree about which directory is the root.
    let branch = load_branch_context(git, path, worktrees)?;

    let merge_targets = resolve_merge_targets(git, &branch.root);
    let merge_refs = merge_targets
        .iter()
        .map(|target| target.name.clone())
        .collect::<Vec<_>>();
    let default_ref = merge_refs.first().cloned();
    let default_target_oid = merge_targets.first().and_then(|target| target.oid.clone());
    let landing = LandingContext::from_resolved(git, &branch.root, merge_targets, landing_cache);

    Ok(RepoContext {
        branch,
        default_ref,
        merge_refs,
        default_target_oid,
        landing,
    })
}

/// Read every local branch's upstream, divergence, and last commit in one call.
///
/// One extra call, and only one, when a branch tracks a ref outside
/// `refs/heads/` and `refs/remotes/` — a custom fetch refspec such as
/// `refs/pr/*`. Those refs are not in the batched listing, so their commit was
/// silently absent, and an upstream commit nobody read is indistinguishable
/// from an upstream that never moves. Every unresolved ref in the repository is
/// resolved together, so a repository without such refspecs still makes exactly
/// one git call and repositories with them make two, never one per branch.
pub fn load_branches(git: &Git, root: &Path) -> Result<HashMap<String, BranchInfo>> {
    let format = format!("--format={FOR_EACH_REF_FORMAT}");
    let mut args = vec!["for-each-ref", &format];
    args.extend(FOR_EACH_REF_NAMESPACES);
    let out = git.run(root, &args)?;
    let mut branches = parse_for_each_ref(&out);
    resolve_stray_upstreams(git, root, &mut branches);
    Ok(branches)
}

/// Name the commit of every upstream the batched listing did not cover.
///
/// Anything still unnamed afterwards keeps `upstream_unresolved`, which the
/// removal guard reads as "this could not be established" and refuses on,
/// rather than as an upstream that has not moved.
fn resolve_stray_upstreams(git: &Git, root: &Path, branches: &mut HashMap<String, BranchInfo>) {
    let mut wanted: Vec<String> = branches
        .values()
        .filter(|info| info.upstream_unresolved)
        .filter_map(|info| info.upstream_ref.clone())
        .collect();
    wanted.sort();
    wanted.dedup();
    if wanted.is_empty() {
        return;
    }

    let format = format!("--format={REF_OID_FORMAT}");
    let mut args = vec!["for-each-ref".to_string(), format];
    args.extend(wanted);
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let Ok(out) = git.run(root, &borrowed) else {
        // Left unresolved on purpose: a failed lookup is not a resolution.
        return;
    };

    let oids = parse_ref_oids(&out);
    for info in branches.values_mut() {
        if !info.upstream_unresolved {
            continue;
        }
        if let Some(reference) = &info.upstream_ref
            && let Some(oid) = oids.get(reference)
        {
            info.upstream_oid = Some(oid.clone());
            info.upstream_unresolved = false;
        }
    }
}

/// Determine every default ref whose snapshots may contain branch work.
///
/// The first entry is the primary one, used for display: the remote's
/// advertised HEAD when configured, otherwise a conventional name. The rest are
/// additional refs worth testing ancestry against.
///
/// Both remote and local default branches are included on purpose. A branch
/// merged into local `main` but not yet pushed is still preserved outside its
/// worktree, so the worktree really is disposable — checking only the remote
/// would overlook work they just merged locally.
///
/// Returns empty when none exist, in which case nothing is ever reported as
/// landed: deliberately conservative, since a wrong positive reading could cost
/// someone real work.
pub fn resolve_merge_refs(git: &Git, root: &Path) -> Vec<String> {
    resolve_merge_targets(git, root)
        .into_iter()
        .map(|target| target.name)
        .collect()
}

fn resolve_merge_targets(git: &Git, root: &Path) -> Vec<ResolvedTarget> {
    let symbolic = git.run_status(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    );
    match symbolic {
        Ok(out) if out.code == Some(0) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let symbolic_target = text.trim();
            if let Some(remote) = symbolic_target.strip_prefix("refs/remotes/")
                && let Some(local) = remote.strip_prefix("origin/")
                && !local.is_empty()
            {
                match probe_ref(git, root, symbolic_target) {
                    RefProbe::Exists(oid) => {
                        let mut targets = vec![resolved_target(remote, Some(oid))];
                        let local_ref = format!("refs/heads/{local}");
                        match probe_ref(git, root, &local_ref) {
                            RefProbe::Exists(oid) => {
                                targets.push(resolved_target(local, Some(oid)));
                            }
                            RefProbe::Unknown => targets.push(ResolvedTarget {
                                name: local.to_string(),
                                oid: None,
                                unavailable_reason: UnknownReason::GitCommandFailed {
                                    phase: crate::model::ProofPhase::TargetSelection,
                                },
                            }),
                            RefProbe::Missing => {}
                        }
                        return targets;
                    }
                    RefProbe::Unknown => {
                        return vec![ResolvedTarget {
                            name: remote.to_string(),
                            oid: None,
                            unavailable_reason: UnknownReason::GitCommandFailed {
                                phase: crate::model::ProofPhase::TargetSelection,
                            },
                        }];
                    }
                    // A symbolic ref whose target no longer exists is stale,
                    // not an authoritative default. Conventional names are the
                    // fallback only in this absent-target case.
                    RefProbe::Missing => {}
                }
            }
        }
        Ok(_) => {}
        Err(_) => {
            return vec![ResolvedTarget {
                name: "origin/HEAD".to_string(),
                oid: None,
                unavailable_reason: UnknownReason::GitCommandFailed {
                    phase: crate::model::ProofPhase::TargetSelection,
                },
            }];
        }
    }

    let mut refs = Vec::new();
    for (candidate, full_ref) in [
        ("origin/main", "refs/remotes/origin/main"),
        ("origin/master", "refs/remotes/origin/master"),
        ("origin/trunk", "refs/remotes/origin/trunk"),
        ("main", "refs/heads/main"),
        ("master", "refs/heads/master"),
        ("trunk", "refs/heads/trunk"),
    ] {
        match probe_ref(git, root, full_ref) {
            RefProbe::Exists(oid) => refs.push(resolved_target(candidate, Some(oid))),
            RefProbe::Unknown => refs.push(ResolvedTarget {
                name: candidate.to_string(),
                oid: None,
                unavailable_reason: UnknownReason::GitCommandFailed {
                    phase: crate::model::ProofPhase::TargetSelection,
                },
            }),
            RefProbe::Missing => {}
        }
    }

    refs
}

/// Prove one immutable head against one explicitly named target.
///
/// The scanner normally aggregates every resolved default ref. Keeping this
/// entry point single-target makes diagnostics and callers with an already
/// chosen default branch obey the same proof rules without synthesising a
/// worktree.
pub fn landing_against(git: &Git, root: &Path, head: &str, target: &str) -> Landing {
    LandingContext::new(git, root, &[target.to_string()], LandingCache::default()).landing_revision(
        git,
        root,
        head,
        LandingDepth::History,
    )
}

enum RefProbe {
    Exists(String),
    Missing,
    Unknown,
}

fn resolved_target(name: &str, oid: Option<String>) -> ResolvedTarget {
    ResolvedTarget {
        name: name.to_string(),
        oid,
        unavailable_reason: UnknownReason::TargetUnavailable,
    }
}

fn probe_ref(git: &Git, root: &Path, name: &str) -> RefProbe {
    let spec = format!("{name}^{{commit}}");
    match git.run_status(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
    ) {
        Ok(out) if out.code == Some(0) => parse_oid_output(&out.stdout)
            .map(RefProbe::Exists)
            .unwrap_or(RefProbe::Unknown),
        Ok(out) if out.code == Some(1) => RefProbe::Missing,
        _ => RefProbe::Unknown,
    }
}

fn parse_oid_output(out: &[u8]) -> Option<String> {
    let value = out.strip_suffix(b"\n").unwrap_or(out);
    let value = value.strip_suffix(b"\r").unwrap_or(value);
    if value.is_empty() || !value.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    std::str::from_utf8(value).ok().map(str::to_string)
}

/// Gather every per-worktree signal.
///
/// Individual signals degrade independently rather than failing the whole
/// worktree. Destructive signals preserve inspection failure explicitly; an
/// unreadable directory can leave other facts visible but can never look clean.
pub fn status_for(git: &Git, entry: &WorktreeEntry, ctx: &RepoContext) -> WorktreeStatus {
    let CollectedStatus { mut status, dirty } = collect_status(git, entry, &ctx.branch);
    populate_landing(git, entry, ctx, &mut status, LandingDepth::History);
    populate_uncommitted(git, entry, ctx, &mut status, &dirty.raw_names());
    status
}

#[cfg(test)]
pub(crate) fn status_without_landing(
    git: &Git,
    entry: &WorktreeEntry,
    ctx: &RepoContext,
) -> WorktreeStatus {
    collect_status(git, entry, &ctx.branch).status
}

/// A worktree's status, and the paths behind its dirty counts.
pub(crate) struct CollectedStatus {
    pub status: WorktreeStatus,
    pub dirty: DirtyScan,
}

/// Exactly what git said is uncommitted, path by path.
///
/// Sorted and deduplicated; uncapped, because only the caller knows how many
/// names it has room for — and the authorisation check that compares two of
/// these is not allowed to have a cap at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirtyScan {
    pub paths: Vec<DirtyPath>,
    /// What the inspection could not answer, in the user's terms.
    ///
    /// Empty means "everything below was read". Anything in it means some part
    /// of the worktree's state is unproven, and nothing derived from this scan
    /// may be treated as a description of it.
    pub unproven: Vec<String>,
}

/// One uncommitted path, with the evidence that distinguishes it from another
/// path of the same name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirtyPath {
    /// The lossless bytes git emitted. `path` is only the display spelling.
    pub raw_path: Vec<u8>,
    pub path: String,
    /// Every `XY` pair git reported for it, sorted; more than one when the
    /// index listing found a change the porcelain pass had been told to skip.
    pub codes: Vec<String>,
    /// Every index entry for this path as `<stage> <mode> <oid>`, sorted.
    ///
    /// One entry for an ordinary path. A path in conflict has up to three —
    /// stages 1, 2 and 3, the ancestor and the two sides — and each names a
    /// different blob. Keeping one of them was keeping whichever the index
    /// happened to list last, so resolving a conflict to different bytes, or
    /// re-merging it against a different ancestor, left this identical.
    pub stages: Vec<String>,
}

/// Read one worktree's status, keeping the changed paths as well as the counts.
///
/// The two come out of the same `git status`. The delete dialog needs both, and
/// asking for them separately meant every plan ran the status pass — the porcelain
/// listing and the index re-hash behind it — a second time to learn nothing new.
pub(crate) fn collect_status(
    git: &Git,
    entry: &WorktreeEntry,
    ctx: &BranchContext,
) -> CollectedStatus {
    let mut status = WorktreeStatus::default();
    let mut dirty = DirtyScan::default();

    if inspectable(entry) {
        dirty = scan_dirty_for_worktree(
            git,
            &entry.path,
            &ctx.root,
            &mut status.managed_dependency_links,
        );
        status.dirty = count_status(&dirty.entries_for_counting());
        status.dirty.inspection_failed = !dirty.unproven.is_empty();
        let env_files = find_env_files(git, &entry.path, &ctx.root);
        if entry.is_main {
            status.main_worktree_env_files = env_files;
        } else {
            status.env_files = env_files;
        }
    }

    if let Some(branch) = &entry.branch
        && let Some(info) = ctx.branches.get(branch)
    {
        status.upstream = UpstreamInfo {
            name: info.upstream.clone(),
            full_ref: info.upstream_ref.clone(),
            oid: info.upstream_oid.clone(),
            unresolved: info.upstream_unresolved,
            ahead: info.ahead,
            behind: info.behind,
            gone: info.gone,
        };
        status.branch_oid = Some(info.head.clone());
        status.merge_ref = merge_reference(&status.upstream, ctx);
        status.last_commit_at = info.committed_at;
        status.last_commit_subject = info.subject.clone();
    }

    // Detached worktrees have no branch entry, so read the commit directly.
    if status.last_commit_at.is_none()
        && let Some(head) = &entry.head
        && let Some((at, subject)) = commit_details(git, &ctx.root, head)
    {
        status.last_commit_at = Some(at);
        status.last_commit_subject = subject;
    }

    CollectedStatus { status, dirty }
}

/// Which ref a deletion of this branch would be decided against.
///
/// git's own rule for `branch -d`: the configured upstream when there is one
/// that still exists on the remote, and HEAD otherwise. An upstream that exists
/// but cannot be named or resolved yields nothing at all — a reference nobody
/// can name is one no deletion can verify, and answering with HEAD instead
/// would quietly decide against a different ref than git would.
fn merge_reference(upstream: &UpstreamInfo, ctx: &BranchContext) -> Option<MergeReference> {
    if upstream.name.is_some() && !upstream.gone {
        let (Some(name), Some(oid)) = (&upstream.full_ref, &upstream.oid) else {
            return None;
        };
        return Some(MergeReference {
            name: name.clone(),
            oid: oid.clone(),
        });
    }
    ctx.head_ref.clone()
}

/// Whether there is a working directory here to read at all.
///
/// A prunable worktree's directory is gone and a bare one has no checkout, so
/// in both cases there is nothing to inspect — which is a proven absence, not
/// an inspection that failed.
pub(crate) fn inspectable(entry: &WorktreeEntry) -> bool {
    entry.prunable.is_none() && !entry.bare && entry.path.is_dir()
}

impl DirtyScan {
    pub(crate) fn raw_names(&self) -> Vec<Vec<u8>> {
        self.paths
            .iter()
            .map(|path| path.raw_path.clone())
            .collect()
    }

    /// The counting view: one entry per path per status code.
    fn entries_for_counting(&self) -> Vec<StatusEntry> {
        self.paths
            .iter()
            .flat_map(|path| {
                path.codes.iter().map(move |code| {
                    let bytes = code.as_bytes();
                    let x = bytes.first().copied().unwrap_or(b' ');
                    let y = bytes.get(1).copied().unwrap_or(b' ');
                    let untracked = x == b'?' && y == b'?';
                    StatusEntry {
                        staged: !untracked && x != b' ',
                        unstaged: !untracked && y != b' ',
                        untracked,
                        code: [x, y],
                        raw_path: path.raw_path.clone(),
                        path: path.path.clone(),
                    }
                })
            })
            .collect()
    }
}

/// One status pass and one index listing, folded into per-path evidence.
///
/// Both callers of this go through it, so the plan the dialog shows and the
/// plan the removal re-reads are built from the same two invocations in the
/// same order — and compare byte for byte when nothing has changed.
pub(crate) fn scan_dirty(git: &Git, dir: &Path) -> DirtyScan {
    let mut scan = DirtyScan::default();
    let mut by_path: BTreeMap<Vec<u8>, DirtyPath> = BTreeMap::new();

    let mut record = |entry: StatusEntry| {
        let slot = by_path
            .entry(entry.raw_path.clone())
            .or_insert_with(|| DirtyPath {
                raw_path: entry.raw_path.clone(),
                path: entry.path.clone(),
                ..Default::default()
            });
        let code = String::from_utf8_lossy(&entry.code).into_owned();
        if !slot.codes.contains(&code) {
            slot.codes.push(code);
            slot.codes.sort();
        }
    };

    match git.run(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-uall",
            "-z",
            "--no-renames",
            "--ignore-submodules=none",
        ],
    ) {
        Ok(out) => parse_status(&out).into_iter().for_each(&mut record),
        Err(failure) => scan.unproven.push(format!(
            "its uncommitted changes could not be read ({failure})"
        )),
    }

    match read_index(git, dir) {
        Ok(index) => {
            hidden_index_changes(dir, &index)
                .into_iter()
                .for_each(&mut record);
            for entry in index {
                if let Some(slot) = by_path.get_mut(&entry.raw_path) {
                    let identity = entry.identity();
                    if !slot.stages.contains(&identity) {
                        slot.stages.push(identity);
                        slot.stages.sort();
                    }
                }
            }
        }
        Err(failure) => scan
            .unproven
            .push(format!("its index could not be read ({failure})")),
    }

    scan.paths = by_path.into_values().collect();
    scan
}

/// Dirty state after exact, still-valid yawm dependency links are accounted for.
pub(crate) fn scan_dirty_for_worktree(
    git: &Git,
    dir: &Path,
    main: &Path,
    managed: &mut Vec<crate::model::ManagedDependencyLink>,
) -> DirtyScan {
    let mut scan = scan_dirty(git, dir);
    let links = inspect_links(dir, main);
    if links.unproven {
        scan.unproven
            .push("the yawm dependency-link record could not be read exactly".to_string());
    }
    for link in &links.valid {
        scan.paths
            .retain(|path| !inside_named_path(&path.path, &link.path));
    }
    *managed = links.valid;
    for invalid in links.invalid {
        if !scan
            .paths
            .iter()
            .any(|path| inside_named_path(&path.path, &invalid))
        {
            scan.paths.push(DirtyPath {
                raw_path: invalid.as_bytes().to_vec(),
                path: invalid,
                codes: vec!["??".to_string()],
                stages: Vec::new(),
            });
        }
    }
    scan.paths
        .sort_by(|left, right| left.raw_path.cmp(&right.raw_path));
    scan
}

fn inside_named_path(path: &str, directory: &str) -> bool {
    path == directory
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

/// Git's performance promises must not become yawm's data-loss promises.
///
/// One batched index listing identifies every exceptional entry and includes
/// its blob ID. The ordinary case performs no file reads; only flagged entries
/// are hashed, and every hash is compared with the index rather than with
/// porcelain output that deliberately trusts these flags.
pub(crate) fn hidden_index_changes(dir: &Path, index: &[IndexEntry]) -> Vec<StatusEntry> {
    flagged(index)
        .into_iter()
        .filter(|entry| !matches_index_blob(dir, entry))
        .map(|entry| StatusEntry {
            staged: false,
            unstaged: true,
            untracked: false,
            code: *b" M",
            raw_path: entry.raw_path,
            path: entry.path,
        })
        .collect()
}

/// The whole index, as git records it.
pub(crate) fn read_index(git: &Git, dir: &Path) -> Result<Vec<IndexEntry>> {
    let out = git.run(dir, &["ls-files", "-v", "--stage", "-z"])?;
    Ok(parse_index(&out))
}

pub(crate) fn populate_landing(
    git: &Git,
    entry: &WorktreeEntry,
    ctx: &RepoContext,
    status: &mut WorktreeStatus,
    depth: LandingDepth,
) {
    status.landing = ctx
        .landing
        .landing(git, ctx.root(), entry.head.as_deref(), depth);
    status.landing_facts = landing_facts(git, entry, ctx, status, depth);
    status.landing_complete = match &status.landing {
        Landing::Landed { .. } | Landing::AddsContent { .. } => true,
        Landing::Unknown {
            reason: UnknownReason::CheckDeferred,
            ..
        } => false,
        Landing::Unknown {
            reason: UnknownReason::OverlappingChanges { .. },
            ..
        } => depth == LandingDepth::History,
        Landing::Unknown { .. } => true,
    };
}

fn landing_facts(
    git: &Git,
    entry: &WorktreeEntry,
    ctx: &RepoContext,
    status: &WorktreeStatus,
    depth: LandingDepth,
) -> LandingFacts {
    let selected_target = ctx
        .default_ref
        .as_ref()
        .map(|name| target_fact(name, ctx.default_target_oid.as_deref()));
    let candidate = match &status.landing {
        Landing::Unknown {
            candidate: Some(candidate),
            ..
        } => Some(target_fact(&candidate.target, Some(&candidate.commit))),
        _ => None,
    };
    let mut head = head_state(entry);
    let commits_ahead = entry
        .head
        .as_deref()
        .filter(|oid| valid_head_oid(oid))
        .zip(ctx.default_target_oid.as_deref())
        .and_then(|(head_oid, target_oid)| {
            match git.run_status(ctx.root(), &["merge-base", head_oid, target_oid]) {
                Ok(out) if out.code == Some(1) => {
                    head = match (&entry.branch, entry.detached) {
                        (Some(branch), false) => HeadState::Orphan {
                            branch: branch.clone(),
                            oid: head_oid.to_string(),
                        },
                        _ => HeadState::NoMergeBase {
                            oid: head_oid.to_string(),
                        },
                    };
                }
                _ => {}
            }
            let range = format!("{target_oid}..{head_oid}");
            let out = git
                .run_status(ctx.root(), &["rev-list", "--count", &range, "--"])
                .ok()?;
            (out.code == Some(0))
                .then(|| std::str::from_utf8(&out.stdout).ok()?.trim().parse().ok())
                .flatten()
        });
    let (mut unknown_reason, mut proof_phase) = match &status.landing {
        Landing::Unknown { reason, .. } => (Some(reason.clone()), Some(proof_phase(reason, depth))),
        _ => (None, None),
    };
    if unknown_reason.is_some()
        && matches!(
            &head,
            HeadState::Orphan { .. } | HeadState::NoMergeBase { .. }
        )
    {
        unknown_reason = Some(UnknownReason::NoMergeBase);
        proof_phase = Some(ProofPhase::Ancestry);
    }
    LandingFacts {
        selected_target,
        candidate,
        commits_ahead,
        head,
        upstream: upstream_state(&status.upstream),
        unknown_reason,
        proof_phase,
    }
}

fn target_fact(name: &str, oid: Option<&str>) -> LandingTargetFact {
    LandingTargetFact {
        name: name.to_string(),
        oid: oid.map(str::to_string),
        short_oid: oid.map(short_oid),
    }
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(12).collect()
}

fn valid_head_oid(oid: &str) -> bool {
    !oid.is_empty()
        && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        && oid.bytes().any(|byte| byte != b'0')
}

fn head_state(entry: &WorktreeEntry) -> HeadState {
    match entry.head.as_deref().filter(|oid| valid_head_oid(oid)) {
        Some(oid) if entry.detached || entry.branch.is_none() => HeadState::Detached {
            oid: oid.to_string(),
        },
        Some(oid) => HeadState::Branch {
            name: entry.branch.clone().unwrap_or_default(),
            oid: oid.to_string(),
        },
        None if entry.branch.is_some() => HeadState::Unborn {
            branch: entry.branch.clone(),
        },
        None => HeadState::Unavailable,
    }
}

fn upstream_state(upstream: &UpstreamInfo) -> UpstreamState {
    let Some(name) = &upstream.name else {
        return UpstreamState::None;
    };
    if upstream.gone {
        return UpstreamState::Gone {
            name: name.clone(),
            full_ref: upstream.full_ref.clone(),
        };
    }
    let (Some(full_ref), Some(oid)) = (&upstream.full_ref, &upstream.oid) else {
        return UpstreamState::Unresolved {
            name: name.clone(),
            full_ref: upstream.full_ref.clone(),
        };
    };
    if upstream.unresolved {
        return UpstreamState::Unresolved {
            name: name.clone(),
            full_ref: Some(full_ref.clone()),
        };
    }
    UpstreamState::Existing {
        name: name.clone(),
        full_ref: full_ref.clone(),
        oid: oid.clone(),
        ahead: upstream.ahead,
        behind: upstream.behind,
    }
}

fn proof_phase(reason: &UnknownReason, depth: LandingDepth) -> ProofPhase {
    match reason {
        UnknownReason::NotChecked => ProofPhase::NotStarted,
        UnknownReason::NoDefaultBranch | UnknownReason::TargetUnavailable => {
            ProofPhase::TargetSelection
        }
        UnknownReason::HeadUnavailable => ProofPhase::HeadResolution,
        UnknownReason::GitCommandFailed { phase } => *phase,
        UnknownReason::MergeTreeUnavailable
        | UnknownReason::MalformedMergeTree
        | UnknownReason::CheckDeferred => ProofPhase::MergeTree,
        UnknownReason::OverlappingChanges { .. } => {
            if depth == LandingDepth::History {
                ProofPhase::History
            } else {
                ProofPhase::MergeTree
            }
        }
        UnknownReason::HistoryRangeTooLarge { .. } => ProofPhase::History,
        UnknownReason::NoMergeBase => ProofPhase::Ancestry,
        UnknownReason::CustomMergeDriver | UnknownReason::MergeAttributes => {
            ProofPhase::MergeConfiguration
        }
    }
}

pub(crate) fn populate_uncommitted(
    git: &Git,
    entry: &WorktreeEntry,
    ctx: &RepoContext,
    status: &mut WorktreeStatus,
    dirty_paths: &[Vec<u8>],
) {
    if !status.dirty.is_dirty() || entry.prunable.is_some() || entry.bare {
        return;
    }
    status.uncommitted = uncommitted_against(
        git,
        &entry.path,
        ctx.default_ref
            .as_deref()
            .zip(ctx.default_target_oid.as_deref()),
        dirty_paths,
    );
}

/// Interactive callers cannot reuse the scanner's collected paths, so the
/// status and comparison stay paired here rather than silently diverging.
pub fn uncommitted_for(
    git: &Git,
    entry: &WorktreeEntry,
    ctx: &RepoContext,
) -> crate::model::UncommittedAnalysis {
    let CollectedStatus { mut status, dirty } = collect_status(git, entry, &ctx.branch);
    populate_uncommitted(git, entry, ctx, &mut status, &dirty.raw_names());
    status.uncommitted
}

pub(crate) fn prepare_landing(git: &Git, entries: &[WorktreeEntry], ctx: &RepoContext) {
    let heads = entries
        .iter()
        .filter_map(|entry| entry.head.clone())
        .collect::<Vec<_>>();
    ctx.landing.prepare_heads(git, ctx.root(), &heads);
}

fn commit_details(git: &Git, root: &Path, rev: &str) -> Option<(i64, Option<String>)> {
    let out = git
        .run(root, &["log", "-1", "--format=%ct%x00%s", rev])
        .ok()?;
    let text = String::from_utf8_lossy(&out);
    let mut parts = text.trim_end_matches(['\n', '\r']).splitn(2, '\0');
    let at = parts.next()?.trim().parse().ok()?;
    let subject = parts.next().map(str::trim).filter(|s| !s.is_empty());
    Some((at, subject.map(str::to_string)))
}

/// Directories whose contents belong to a package manager, a build, or another
/// project's vendored source. A `.env` under any of them is a dependency's test
/// fixture, not the user's credentials, and warning about it would teach people
/// to click through the warning that matters.
const UNOWNED_DIRS: &[&str] = &[".git", "node_modules", "target", "vendor", ".venv", "dist"];

/// Deep enough for `apps/api/.env` in a monorepo, shallow enough that a
/// pathological tree cannot turn one worktree's scan into a hang.
const MAX_ENV_DEPTH: usize = 6;

/// Same reasoning as `MAX_LISTED_FILES`: enough to inform, not enough to flood.
const MAX_ENV_FILES: usize = 50;

/// Hard stop on traversal, so an enormous directory nobody thought to prune
/// still cannot make the list take longer than a person will wait.
const MAX_ENV_ENTRIES: usize = 20_000;

/// Find environment files whose current contents are unique to this directory.
///
/// An untracked file is not unique when the main worktree has the same bytes.
/// Agent worktrees commonly inherit credentials that way, and warning on every
/// inherited copy would train people to ignore the warning that matters.
///
/// The test is "untracked", not "gitignored", and the difference matters in
/// both directions. A tracked `.env` is in history, so deleting the worktree
/// costs nothing and warning about it would be noise. An untracked `.env` that
/// no ignore rule happens to cover is just as unrecoverable as an ignored one,
/// and the doc comment this code used to carry claimed to check ignore status
/// while checking nothing at all — so the check is now real, and drawn where
/// the actual risk is.
///
/// A monorepo keeps its secrets in `apps/api/.env`, not at the root, so this
/// descends. It prunes [`UNOWNED_DIRS`] on the way down rather than filtering
/// afterwards, which is what keeps it cheap: `git ls-files -o -i` has to
/// enumerate every ignored file in the repository first, and on a workspace
/// with a large `node_modules` that is seconds of work per worktree.
fn find_env_files(git: &Git, dir: &Path, main: &Path) -> Vec<String> {
    let mut found = retain_untracked(git, dir, env_candidates(dir).files);
    if dir != main {
        found.retain(|relative| !files_equal(&dir.join(relative), &main.join(relative)));
    }
    found.truncate(MAX_ENV_FILES);
    found
}

/// Every environment-shaped file in this directory, uncapped and sorted.
///
/// [`find_env_files`] answers "what should the user be shown", and caps itself
/// accordingly. This answers "what is there", which is a different question:
/// an authorisation that survives because the file that changed happened to be
/// the fifty-first is an authorisation for a state nobody saw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EnvScan {
    pub files: Vec<String>,
    /// False when anything in the traversal could not be established: a
    /// directory that would not open, an entry that would not read, a kind or
    /// a name that could not be determined, or a walk that stopped at its cap.
    ///
    /// The list is then a sample rather than an inventory, and a state built
    /// from a sample is not one anybody can be said to have approved.
    pub complete: bool,
}

pub(crate) fn env_candidates(dir: &Path) -> EnvScan {
    let (mut files, complete) = walk_for_env_files(dir);
    files.sort();
    files.dedup();
    EnvScan { files, complete }
}

fn files_equal(left: &Path, right: &Path) -> bool {
    let Ok(left_meta) = std::fs::metadata(left) else {
        return false;
    };
    let Ok(right_meta) = std::fs::metadata(right) else {
        return false;
    };
    if !left_meta.is_file() || !right_meta.is_file() || left_meta.len() != right_meta.len() {
        return false;
    }

    let Ok(mut left) = std::fs::File::open(left) else {
        return false;
    };
    let Ok(mut right) = std::fs::File::open(right) else {
        return false;
    };
    let mut left_chunk = [0; 64 * 1024];
    let mut right_chunk = [0; 64 * 1024];
    loop {
        let Ok(left_read) = left.read(&mut left_chunk) else {
            return false;
        };
        let Ok(right_read) = right.read(&mut right_chunk) else {
            return false;
        };
        if left_read != right_read || left_chunk[..left_read] != right_chunk[..right_read] {
            return false;
        }
        if left_read == 0 {
            return true;
        }
    }
}

/// One entry of a directory, reduced to what the traversal reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirItem {
    Dir { name: String, path: PathBuf },
    File { name: String },
}

/// What reading one directory yielded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirListing {
    pub items: Vec<DirItem>,
    /// Directory entries consumed from the iterator, including entries whose
    /// kind or name could not be interpreted.
    pub visited: usize,
    /// False when anything about this directory could not be established.
    ///
    /// Never folded into an empty list. "The directory would not open" and
    /// "the directory is empty" are the same shape and opposite facts, and a
    /// removal that reads the first as the second destroys files it never
    /// listed.
    pub complete: bool,
}

/// Read one directory, accounting for everything it could not establish.
///
/// Every failure here is per-entry and recoverable, so the walk carries on and
/// records that it is no longer looking at a complete picture: an entry the
/// iterator refuses to yield, a kind the OS declines to report, a name that is
/// not UTF-8 and so cannot be compared with the paths git speaks in.
///
/// Symlinks are not followed, which is a bound rather than an uncertainty:
/// what they point at lives outside this directory and survives its removal,
/// and following them invites cycles and escapes from the worktree.
fn read_env_dir(dir: &Path, remaining: usize) -> DirListing {
    let Ok(read) = std::fs::read_dir(dir) else {
        return DirListing {
            items: Vec::new(),
            visited: 0,
            complete: false,
        };
    };

    let mut listing = DirListing {
        items: Vec::new(),
        visited: 0,
        complete: true,
    };
    for entry in read {
        if listing.visited == remaining {
            listing.complete = false;
            break;
        }
        listing.visited += 1;
        let Ok(entry) = entry else {
            listing.complete = false;
            continue;
        };
        let Ok(kind) = entry.file_type() else {
            listing.complete = false;
            continue;
        };
        let Ok(name) = decode_entry_name(entry.file_name()) else {
            listing.complete = false;
            continue;
        };

        if kind.is_dir() {
            listing.items.push(DirItem::Dir {
                name,
                path: entry.path(),
            });
        } else if kind.is_file() {
            listing.items.push(DirItem::File { name });
        }
    }
    fn item_name(item: &DirItem) -> &str {
        match item {
            DirItem::Dir { name, .. } | DirItem::File { name } => name,
        }
    }
    listing
        .items
        .sort_by(|left, right| item_name(left).cmp(item_name(right)));
    listing
}

/// The name of a directory entry, if it is one this can reason about.
///
/// Paths are compared against the ones git reports, which are UTF-8, and are
/// shown to the user as text. A name that is not UTF-8 is neither, so whether
/// it is an environment file is not a question this can answer — it is returned
/// as an error rather than lossily converted into a name no file has.
fn decode_entry_name(name: OsString) -> std::result::Result<String, OsString> {
    name.into_string()
}

/// Breadth-first so that the shallowest files survive the cap: a secret at the
/// root is likelier to be the one the user cares about than one six levels down.
fn walk_for_env_files(root: &Path) -> (Vec<String>, bool) {
    walk_env_files_with(root, &read_env_dir)
}

/// [`walk_for_env_files`] over any directory reader.
///
/// The reader is a parameter because the failures that matter here cannot all
/// be arranged on a real filesystem by the user running the tests: an entry
/// whose kind the OS refuses to report, an iterator that yields an error
/// part-way through a directory, a directory that stays unreadable even for
/// root.
fn walk_env_files_with(
    root: &Path,
    read: &dyn Fn(&Path, usize) -> DirListing,
) -> (Vec<String>, bool) {
    let mut found = Vec::new();
    let mut complete = true;
    let mut queue: VecDeque<(PathBuf, String, usize)> =
        VecDeque::from([(root.to_path_buf(), String::new(), 0)]);
    let mut seen = 0usize;

    while let Some((dir, prefix, depth)) = queue.pop_front() {
        let remaining = MAX_ENV_ENTRIES.saturating_sub(seen);
        let listing = read(&dir, remaining);
        complete &= listing.complete;
        if listing.visited > remaining || listing.items.len() > listing.visited {
            return (found, false);
        }
        seen += listing.visited;
        for item in listing.items {
            match item {
                DirItem::Dir { name, path } => {
                    if UNOWNED_DIRS.contains(&name.as_str()) {
                        continue;
                    }
                    if depth + 1 > MAX_ENV_DEPTH {
                        complete = false;
                        continue;
                    }
                    queue.push_back((path, format!("{prefix}{name}/"), depth + 1));
                }
                DirItem::File { name } => {
                    if is_env_file(&name) {
                        found.push(format!("{prefix}{name}"));
                    }
                }
            }
        }
    }
    (found, complete)
}

/// Drop the candidates git already has a copy of.
///
/// Bounded batched `ls-files` calls cover the whole set without exceeding the
/// platform's argv limit. When git cannot answer, every candidate is kept: the cost
/// of an unnecessary warning is a moment's hesitation, and the cost of a
/// missing one is somebody's credentials.
fn retain_untracked(git: &Git, dir: &Path, candidates: Vec<String>) -> Vec<String> {
    if candidates.is_empty() {
        return candidates;
    }
    const PATHS_PER_QUERY: usize = 256;
    let mut untracked = Vec::new();
    for chunk in candidates.chunks(PATHS_PER_QUERY) {
        let mut args: Vec<String> = vec!["ls-files".into(), "-z".into(), "--".into()];
        args.extend(chunk.iter().cloned());
        let Ok(out) = git.run(dir, &args) else {
            untracked.extend(chunk.iter().cloned());
            continue;
        };
        let tracked: Vec<String> = String::from_utf8_lossy(&out)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        untracked.extend(
            chunk
                .iter()
                .filter(|path| !tracked.iter().any(|tracked| tracked == *path))
                .cloned(),
        );
    }
    untracked
}

/// `.env`, `.env.local`, `.env.production` — but not `.environment` or
/// `env.example`, which are ordinary tracked files.
fn is_env_file(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LandingProof, Verdict, VerdictReason};
    use crate::verdict::{VerdictConfig, classify};

    #[test]
    fn recognises_env_files() {
        assert!(is_env_file(".env"));
        assert!(is_env_file(".env.local"));
        assert!(is_env_file(".env.production.local"));
    }

    #[test]
    fn ignores_lookalike_filenames() {
        assert!(!is_env_file(".environment"));
        assert!(!is_env_file("env"));
        assert!(!is_env_file("env.example"));
        assert!(!is_env_file(".envrc"));
    }

    /// A complete traversal of an ordinary tree says so, and finds what is in
    /// it — so "incomplete" below means something, rather than being the only
    /// answer this ever gives.
    #[test]
    fn a_readable_tree_is_listed_completely() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("apps/api")).unwrap();
        std::fs::write(root.join(".env"), "ROOT=1").unwrap();
        std::fs::write(root.join("apps/api/.env.local"), "NESTED=1").unwrap();
        std::fs::write(root.join("apps/api/main.rs"), "fn main() {}").unwrap();

        let scan = env_candidates(root);

        assert!(scan.complete);
        assert_eq!(scan.files, vec![".env", "apps/api/.env.local"]);
    }

    #[test]
    fn a_directory_reader_never_retains_entries_past_its_remaining_budget() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["c", "a", "b"] {
            std::fs::write(root.join(name), name).unwrap();
        }

        let listing = read_env_dir(root, 2);

        assert_eq!(listing.visited, 2);
        assert_eq!(listing.items.len(), 2);
        assert!(!listing.complete, "an unvisited entry remains");
        let names: Vec<&str> = listing
            .items
            .iter()
            .map(|item| match item {
                DirItem::Dir { name, .. } | DirItem::File { name } => name.as_str(),
            })
            .collect();
        assert!(
            names.windows(2).all(|pair| pair[0] <= pair[1]),
            "the retained subset remains deterministic: {names:?}"
        );
    }

    /// A directory that will not open is the case this exists for.
    ///
    /// The traversal used to skip it and return a list that looked exactly like
    /// a complete one, so a plan built on it claimed to account for files it had
    /// never seen. Root can read it anyway, so the test asserts nothing when the
    /// filesystem declines to make it unreadable.
    #[cfg(unix)]
    #[test]
    fn a_directory_that_cannot_be_opened_leaves_the_scan_incomplete() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let secret = root.join("secrets");
        std::fs::create_dir(&secret).unwrap();
        std::fs::write(secret.join(".env"), "HIDDEN=1").unwrap();
        std::fs::write(root.join(".env"), "ROOT=1").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();

        let unreadable = std::fs::read_dir(&secret).is_err();
        let scan = env_candidates(root);
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o755)).unwrap();

        if !unreadable {
            return; // running as root: the directory is readable regardless
        }
        assert!(
            !scan.complete,
            "the directory was not read, so the listing is a sample; got {:?}",
            scan.files
        );
        assert!(
            scan.files.contains(&".env".to_string()),
            "and what could be read is still reported; got {:?}",
            scan.files
        );
    }

    /// A name that is not UTF-8 cannot be compared with the paths git speaks
    /// in, so whether it is an environment file is not something this knows.
    ///
    /// Not every filesystem will store such a name — APFS rejects the bytes
    /// outright — so the real-filesystem half of this only asserts where the
    /// name could actually be created. The decision itself is pinned below.
    #[cfg(unix)]
    #[test]
    fn a_filename_that_is_not_utf8_leaves_the_scan_incomplete() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".env"), "ROOT=1").unwrap();
        if std::fs::write(root.join(OsStr::from_bytes(b".env.\xff\xfe")), "ODD=1").is_err() {
            return; // the filesystem will not hold the name
        }

        let scan = env_candidates(root);

        assert!(!scan.complete, "got {:?}", scan.files);
        assert_eq!(scan.files, vec![".env"], "the readable name is still there");
    }

    /// The decision a name that is not UTF-8 forces, on any filesystem.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_is_not_converted_into_one_that_is() {
        use std::os::unix::ffi::OsStringExt;

        assert!(decode_entry_name(OsString::from_vec(b".env.\xff\xfe".to_vec())).is_err());
        assert_eq!(
            decode_entry_name(OsString::from(".env.local"))
                .ok()
                .as_deref(),
            Some(".env.local")
        );
    }

    /// The failures a real filesystem will not reproduce on demand.
    ///
    /// An entry the iterator refuses to yield and a kind the OS declines to
    /// report are both ordinary — a file removed mid-walk, a filesystem that
    /// loses its mount — and neither can be arranged here, so the reader is
    /// injected instead of the errors being taken on trust.
    #[test]
    fn an_entry_that_could_not_be_read_leaves_the_scan_incomplete() {
        let root = Path::new("/w");
        let read = |dir: &Path, _remaining: usize| {
            if dir == Path::new("/w") {
                return DirListing {
                    items: vec![
                        DirItem::File {
                            name: ".env".to_string(),
                        },
                        DirItem::Dir {
                            name: "apps".to_string(),
                            path: PathBuf::from("/w/apps"),
                        },
                    ],
                    visited: 2,
                    complete: true,
                };
            }
            // The nested directory yielded one entry and then failed.
            DirListing {
                items: vec![DirItem::File {
                    name: ".env.local".to_string(),
                }],
                visited: 1,
                complete: false,
            }
        };

        let (files, complete) = walk_env_files_with(root, &read);

        assert!(!complete, "one directory could not be finished");
        assert_eq!(
            files,
            vec![".env", "apps/.env.local"],
            "and the walk carried on through everything it could read"
        );
    }

    /// Failure anywhere is failure overall: a later readable directory cannot
    /// restore certainty about an earlier one.
    #[test]
    fn an_unreadable_directory_is_not_undone_by_readable_siblings() {
        let read = |dir: &Path, _remaining: usize| {
            if dir == Path::new("/w") {
                return DirListing {
                    items: vec![
                        DirItem::Dir {
                            name: "locked".to_string(),
                            path: PathBuf::from("/w/locked"),
                        },
                        DirItem::Dir {
                            name: "open".to_string(),
                            path: PathBuf::from("/w/open"),
                        },
                    ],
                    visited: 2,
                    complete: true,
                };
            }
            if dir == Path::new("/w/locked") {
                return DirListing {
                    items: Vec::new(),
                    visited: 0,
                    complete: false,
                };
            }
            DirListing {
                items: vec![DirItem::File {
                    name: ".env".to_string(),
                }],
                visited: 1,
                complete: true,
            }
        };

        let (files, complete) = walk_env_files_with(Path::new("/w"), &read);

        assert!(!complete);
        assert_eq!(files, vec!["open/.env"]);
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn repository() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git_in(&root, &["init", "-q", "-b", "main", "."]);
        git_in(&root, &["config", "user.email", "test@yawm.dev"]);
        git_in(&root, &["config", "user.name", "yawm test"]);
        git_in(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("tracked.txt"), "before\n").unwrap();
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-qm", "init"]);
        (dir, root)
    }

    #[test]
    fn failed_status_cannot_be_classified_as_disposable() {
        let (_dir, root) = repository();
        let git = Git::new();
        let entries = list_worktrees(&git, &root).unwrap();
        let ctx = load_context(&git, &root, &entries).unwrap();
        let mut entry = entries[0].clone();
        entry.is_main = false;

        let mut status = status_without_landing(&Git::with_program("false"), &entry, &ctx);
        assert!(status.dirty.is_unknown());
        status.last_commit_at = None;
        status.landing = Landing::Landed {
            target: "main".into(),
            proof: LandingProof::Ancestry,
        };

        assert_eq!(
            classify(&entry, &status, &VerdictConfig::default(), i64::MAX),
            (Verdict::Review, VerdictReason::WorkingTreeUnreadable)
        );
    }

    #[test]
    fn index_flags_cannot_hide_modified_files() {
        let (_dir, root) = repository();
        std::fs::write(root.join("skip.txt"), "before\n").unwrap();
        git_in(&root, &["add", "skip.txt"]);
        git_in(&root, &["commit", "-qm", "add skip file"]);
        git_in(
            &root,
            &["update-index", "--assume-unchanged", "tracked.txt"],
        );
        git_in(&root, &["update-index", "--skip-worktree", "skip.txt"]);

        let git = Git::new();
        let entries = list_worktrees(&git, &root).unwrap();
        let ctx = load_context(&git, &root, &entries).unwrap();
        let clean = status_without_landing(&git, &entries[0], &ctx);
        assert!(!clean.dirty.is_dirty());

        std::fs::write(root.join("tracked.txt"), "after\n").unwrap();
        std::fs::write(root.join("skip.txt"), "after\n").unwrap();

        let status = status_without_landing(&git, &entries[0], &ctx);

        assert_eq!(status.dirty.unstaged, 2);
    }

    #[test]
    fn real_git_counts_distinct_dirty_paths_separately_from_dimensions() {
        let (_dir, root) = repository();
        std::fs::write(root.join("tracked.txt"), "staged\n").unwrap();
        git_in(&root, &["add", "tracked.txt"]);
        std::fs::write(root.join("tracked.txt"), "unstaged after staging\n").unwrap();
        std::fs::write(root.join("first-untracked.txt"), "first\n").unwrap();
        std::fs::write(root.join("second-untracked.txt"), "second\n").unwrap();

        let git = Git::new();
        let entries = list_worktrees(&git, &root).unwrap();
        let ctx = load_context(&git, &root, &entries).unwrap();
        let counts = status_without_landing(&git, &entries[0], &ctx).dirty;

        assert_eq!(counts.staged, 1);
        assert_eq!(counts.unstaged, 1);
        assert_eq!(counts.untracked, 2);
        assert_eq!(counts.paths, 3);
        assert_eq!(counts.total(), 4);
    }

    #[test]
    fn repository_config_cannot_hide_dirty_submodules() {
        let parent_dir = tempfile::tempdir().unwrap();
        let parent = parent_dir.path().canonicalize().unwrap();
        git_in(&parent, &["init", "-q", "-b", "main", "."]);
        git_in(&parent, &["config", "user.email", "test@yawm.dev"]);
        git_in(&parent, &["config", "user.name", "yawm test"]);
        git_in(&parent, &["config", "commit.gpgsign", "false"]);

        let child_dir = tempfile::tempdir().unwrap();
        let child = child_dir.path().canonicalize().unwrap();
        git_in(&child, &["init", "-q", "-b", "main", "."]);
        git_in(&child, &["config", "user.email", "test@yawm.dev"]);
        git_in(&child, &["config", "user.name", "yawm test"]);
        git_in(&child, &["config", "commit.gpgsign", "false"]);
        std::fs::write(child.join("child.txt"), "before\n").unwrap();
        git_in(&child, &["add", "."]);
        git_in(&child, &["commit", "-qm", "init"]);

        let child_arg = child.to_string_lossy().into_owned();
        // `canonicalize` returns a verbatim `\\?\C:\...` path on Windows.
        // Rust accepts it and Git for Windows' submodule parser does not, so
        // pass the ordinary drive spelling at this external boundary.
        let child_arg = child_arg.strip_prefix(r"\\?\").unwrap_or(&child_arg);
        git_in(
            &parent,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                child_arg,
                "sub",
            ],
        );
        git_in(&parent, &["commit", "-qam", "add submodule"]);
        git_in(&parent, &["config", "submodule.sub.ignore", "all"]);
        std::fs::write(parent.join("sub/child.txt"), "after\n").unwrap();

        let git = Git::new();
        let entries = list_worktrees(&git, &parent).unwrap();
        let ctx = load_context(&git, &parent, &entries).unwrap();
        let status = status_without_landing(&git, &entries[0], &ctx);

        assert!(status.dirty.is_dirty());
    }

    /// A conflicted path is three blobs, and the scan recorded one.
    ///
    /// `git ls-files --stage` emits an entry per stage under the same path.
    /// Keeping one identity per path meant the last stage parsed overwrote the
    /// others, so a merge resolved one way and a merge resolved another way —
    /// same base, same incoming side, different local side — described
    /// identically. Every stage is recorded, in git's own stage order.
    #[test]
    fn every_stage_of_a_conflicted_path_is_recorded() {
        let (_dir, root) = repository();
        std::fs::write(root.join("conflict.txt"), "base\n").unwrap();
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-qm", "base"]);

        git_in(&root, &["checkout", "-q", "-b", "theirs"]);
        std::fs::write(root.join("conflict.txt"), "theirs\n").unwrap();
        git_in(&root, &["commit", "-qam", "theirs"]);

        git_in(&root, &["checkout", "-q", "main"]);
        std::fs::write(root.join("conflict.txt"), "ours\n").unwrap();
        git_in(&root, &["commit", "-qam", "ours"]);

        // Expected to fail: that is the state under test.
        let _ = std::process::Command::new("git")
            .current_dir(&root)
            .args(["merge", "--no-edit", "theirs"])
            .status()
            .expect("git");

        let scan = scan_dirty(&Git::new(), &root);
        assert!(scan.unproven.is_empty(), "got {:?}", scan.unproven);
        let entry = scan
            .paths
            .iter()
            .find(|path| path.path == "conflict.txt")
            .expect("the conflicted path is dirty");

        assert_eq!(
            entry.stages.len(),
            3,
            "base, ours, and theirs are three separate blobs: {:?}",
            entry.stages
        );
        let stages: Vec<&str> = entry
            .stages
            .iter()
            .map(|stage| stage.split(' ').next().unwrap())
            .collect();
        assert_eq!(stages, vec!["1", "2", "3"], "sorted, in git's stage order");
        let oids: std::collections::BTreeSet<&str> = entry
            .stages
            .iter()
            .map(|stage| stage.split(' ').nth(2).unwrap())
            .collect();
        assert_eq!(oids.len(), 3, "and each names different content");
    }

    /// A fetch refspec may track upstreams outside `refs/remotes/`.
    ///
    /// The batched listing walks `refs/heads/` and `refs/remotes/` only, so an
    /// upstream configured as `refs/pr/42` was absent from it — and an absent
    /// upstream commit read as "there is no upstream commit", which is the
    /// same value a branch whose upstream never moved has. The commit is
    /// resolved by a second listing instead, of exactly the refs still unnamed.
    #[test]
    fn an_upstream_in_a_custom_namespace_is_resolved() {
        let (_dir, root) = repository();
        let head = String::from_utf8(
            std::process::Command::new("git")
                .current_dir(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .expect("git")
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        git_in(&root, &["update-ref", "refs/pr/42", &head]);
        git_in(&root, &["checkout", "-q", "-b", "feat/pr"]);
        std::fs::write(root.join("tracked.txt"), "ahead\n").unwrap();
        git_in(&root, &["commit", "-qam", "ahead of the pull request"]);
        git_in(&root, &["config", "branch.feat/pr.remote", "."]);
        git_in(&root, &["config", "branch.feat/pr.merge", "refs/pr/42"]);

        let git = Git::new();
        let branches = load_branches(&git, &root).expect("branches");
        let info = &branches["feat/pr"];

        assert_eq!(info.upstream_ref.as_deref(), Some("refs/pr/42"));
        assert_eq!(
            info.upstream_oid.as_deref(),
            Some(head.as_str()),
            "the commit behind the upstream is named exactly"
        );
        assert!(
            !info.upstream_unresolved,
            "so the removal guard has something to compare"
        );
        assert!(
            branches["main"].upstream.is_none() || !branches["main"].upstream_unresolved,
            "and no ordinary branch was disturbed"
        );

        // Moving it is visible, which is the whole point of naming it.
        git_in(&root, &["update-ref", "refs/pr/42", "HEAD"]);
        let moved = load_branches(&git, &root).expect("branches");
        assert_ne!(moved["feat/pr"].upstream_oid, info.upstream_oid);
    }

    /// An upstream that cannot be resolved is not an upstream at rest.
    #[test]
    fn an_upstream_ref_that_resolves_to_nothing_stays_unresolved() {
        let (_dir, root) = repository();
        git_in(&root, &["checkout", "-q", "-b", "feat/pr"]);
        git_in(&root, &["config", "branch.feat/pr.remote", "."]);
        git_in(&root, &["config", "branch.feat/pr.merge", "refs/pr/absent"]);

        let branches = load_branches(&Git::new(), &root).expect("branches");
        let info = &branches["feat/pr"];

        assert!(info.upstream_oid.is_none());
        assert!(
            info.upstream_unresolved || info.gone,
            "either git called it gone or yawm could not name it;              what it must never be is silently equal to an unmoved upstream"
        );
    }

    /// A monorepo keeps its credentials below the root, a package manager keeps
    /// other people's fixtures below `node_modules`, and git keeps a tracked
    /// `.env` forever. All three were reported wrongly before.
    #[test]
    fn finds_secrets_below_the_root_and_nowhere_they_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        git_in(&root, &["init", "-q", "-b", "main", "."]);
        git_in(&root, &["config", "user.email", "test@yawm.dev"]);
        git_in(&root, &["config", "user.name", "yawm test"]);
        git_in(&root, &["config", "commit.gpgsign", "false"]);

        for sub in ["apps/api", "node_modules/pkg", "src", "vendor/lib"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }

        std::fs::write(root.join(".gitignore"), "node_modules/\n.env*\n").unwrap();
        std::fs::write(root.join(".env.local"), "A=1").unwrap();
        std::fs::write(root.join("apps/api/.env"), "B=2").unwrap();
        std::fs::write(root.join("node_modules/pkg/.env"), "C=3").unwrap();
        std::fs::write(root.join("vendor/lib/.env"), "D=4").unwrap();
        std::fs::write(root.join("src/.env"), "E=5").unwrap();
        std::fs::write(root.join("src/.envrc"), "F=6").unwrap();
        git_in(&root, &["add", "-f", ".gitignore", "src/.env"]);
        git_in(&root, &["commit", "-qm", "init"]);

        let found = find_env_files(&Git::new(), &root, &root);

        assert_eq!(
            found,
            vec![".env.local".to_string(), "apps/api/.env".to_string()],
            "nested secrets are reported with their path; vendored, tracked, \
             and lookalike files are not"
        );
    }

    #[test]
    fn inherited_environment_files_are_not_unique_but_changed_copies_are() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let linked = dir.path().join("linked");
        std::fs::create_dir(&main).unwrap();
        git_in(&main, &["init", "-q", "-b", "main", "."]);
        git_in(&main, &["config", "user.email", "test@yawm.dev"]);
        git_in(&main, &["config", "user.name", "yawm test"]);
        git_in(&main, &["config", "commit.gpgsign", "false"]);
        std::fs::write(main.join(".gitignore"), ".env*\n").unwrap();
        std::fs::write(main.join("tracked.txt"), "tracked\n").unwrap();
        git_in(&main, &["add", "."]);
        git_in(&main, &["commit", "-qm", "init"]);
        let linked_arg = linked.to_string_lossy();
        git_in(
            &main,
            &["worktree", "add", "-q", "-b", "feature", &linked_arg],
        );
        std::fs::write(main.join(".env"), "SHARED=1\n").unwrap();
        std::fs::write(linked.join(".env"), "SHARED=1\n").unwrap();

        assert!(find_env_files(&Git::new(), &linked, &main).is_empty());

        std::fs::write(linked.join(".env"), "UNIQUE=1\n").unwrap();
        assert_eq!(
            find_env_files(&Git::new(), &linked, &main),
            vec![".env".to_string()]
        );
    }

    #[test]
    fn reports_untracked_files_when_git_cannot_answer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join(".env"), "A=1").unwrap();

        // Not a repository at all, so `ls-files` fails. Warning anyway is the
        // conservative reading.
        assert_eq!(
            find_env_files(&Git::with_program("yawm-no-such-git"), &root, &root),
            vec![".env".to_string()]
        );
    }

    #[test]
    fn caps_the_number_of_files_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for i in 0..(MAX_ENV_FILES + 20) {
            std::fs::write(root.join(format!(".env.{i}")), "x").unwrap();
        }
        assert_eq!(
            find_env_files(&Git::with_program("yawm-no-such-git"), &root, &root).len(),
            MAX_ENV_FILES
        );
    }

    #[test]
    fn filters_every_environment_candidate_before_capping_the_display() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git_in(&root, &["init", "-q", "-b", "main", "."]);

        for i in 0..(MAX_ENV_FILES * 4) {
            std::fs::write(root.join(format!(".env.{i:03}")), "tracked").unwrap();
        }
        git_in(&root, &["add", "."]);
        std::fs::write(root.join(".env.zzz"), "unique").unwrap();

        assert_eq!(
            find_env_files(&Git::new(), &root, &root),
            vec![".env.zzz".to_string()],
            "a risky file after the old candidate cap must still require force"
        );
    }

    #[test]
    fn stops_descending_past_the_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let deep = root.join("a/b/c/d/e/f/g");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(".env"), "A=1").unwrap();

        let scan = env_candidates(&root);
        assert!(
            !scan.complete,
            "a hidden environment file below the depth cap makes the inventory unproven"
        );
        assert!(scan.files.is_empty());
        assert!(find_env_files(&Git::with_program("yawm-no-such-git"), &root, &root).is_empty());
    }
}
