//! Proof-oriented containment checks for committed branch work.
//!
//! Every positive answer is derived from immutable object IDs. Candidate
//! commits only decide where to spend verification work; they never establish a
//! verdict themselves.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::git::Git;
use crate::model::{
    CandidateMatch, ComparisonShortfall, Landing, LandingProof, UncommittedAnalysis, UnknownReason,
};

const HISTORY_COMMIT_LIMIT: usize = 300;
const MAX_HISTORY_CANDIDATES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UniqueLineSide {
    Additions,
    Deletions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UniqueLineMarker {
    pub path: String,
    pub side: UniqueLineSide,
    pub line_number: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UniquePatch {
    pub patch: String,
    pub line_count: usize,
    pub file_count: usize,
    pub candidate: String,
    pub target: String,
    pub markers: Vec<UniqueLineMarker>,
    pub incomplete: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePatch {
    pub patch: String,
    pub line_count: usize,
    pub file_count: usize,
    pub target: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AllChangesReason {
    NoFilteredChanges,
    Incomplete,
    Unsafe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum FocusedPatch {
    Unmatched { patch: UniquePatch },
    WouldChange { patch: MergePatch },
    All { reason: AllChangesReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LandingDepth {
    TierOne,
    MergeTree,
    History,
}

#[derive(Debug, Clone)]
pub(crate) struct LandingContext {
    targets: Vec<Target>,
    config: MergeConfiguration,
    cache: LandingCache,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedTarget {
    pub name: String,
    pub oid: Option<String>,
    pub unavailable_reason: UnknownReason,
}

impl LandingContext {
    pub(crate) fn new(git: &Git, root: &Path, refs: &[String], cache: LandingCache) -> Self {
        let targets = refs
            .iter()
            .map(|name| ResolvedTarget {
                name: name.clone(),
                oid: resolve_commit(git, root, name),
                unavailable_reason: UnknownReason::TargetUnavailable,
            })
            .collect::<Vec<_>>();
        Self::from_resolved(git, root, targets, cache)
    }

    pub(crate) fn from_resolved(
        git: &Git,
        root: &Path,
        resolved: Vec<ResolvedTarget>,
        cache: LandingCache,
    ) -> Self {
        let mut targets = Vec::new();
        let mut seen = HashSet::new();

        for resolved in resolved {
            if let Some(oid) = &resolved.oid
                && !seen.insert(oid.clone())
            {
                continue;
            }
            targets.push(Target {
                name: resolved.name,
                oid: resolved.oid,
                unavailable_reason: resolved.unavailable_reason,
            });
        }

        Self {
            targets,
            config: MergeConfiguration::read(git, root),
            cache,
        }
    }

    pub(crate) fn prepare_heads(&self, git: &Git, root: &Path, heads: &[String]) {
        let mut remaining = heads.to_vec();
        for target in self.targets.iter().filter_map(|target| target.oid.as_ref()) {
            let backwards = remaining
                .iter()
                .filter(|head| cached_ancestry(&self.cache, root, head, target).is_none())
                .cloned()
                .collect::<Vec<_>>();
            prepare_ancestors_of_target(git, root, &backwards, target, &self.cache);

            let mut trees = Vec::new();
            for head in &remaining {
                if cached_ancestry(&self.cache, root, head, target) == Some(false) {
                    trees.push(head.clone());
                    trees.push(target.clone());
                }
            }
            prepare_trees(git, root, &trees, &self.cache);

            let forwards = remaining
                .iter()
                .filter(|head| {
                    cached_ancestry(&self.cache, root, head, target) == Some(false)
                        && cached_tree_only(root, head, &self.cache)
                            != cached_tree_only(root, target, &self.cache)
                        && cached_ancestry(&self.cache, root, target, head).is_none()
                })
                .map(|head| (target.clone(), head.clone()))
                .collect::<Vec<_>>();
            prepare_ancestry(git, root, &forwards, &self.cache);

            remaining.retain(|head| {
                !matches!(
                    landing_for_target(
                        git,
                        root,
                        head,
                        target,
                        &self.config,
                        &self.cache,
                        LandingDepth::TierOne,
                    ),
                    TargetOutcome::Landed(_)
                )
            });
            if remaining.is_empty() {
                break;
            }
        }
    }

    pub(crate) fn landing(
        &self,
        git: &Git,
        root: &Path,
        head: Option<&str>,
        depth: LandingDepth,
    ) -> Landing {
        self.analyse(git, root, head, depth).landing
    }

    fn analyse(
        &self,
        git: &Git,
        root: &Path,
        head: Option<&str>,
        depth: LandingDepth,
    ) -> LandingAnalysis {
        let Some(head) = head.filter(|head| is_hex(head.as_bytes())) else {
            return LandingAnalysis {
                landing: Landing::Unknown {
                    reason: UnknownReason::HeadUnavailable,
                    candidate: None,
                },
                focus: None,
            };
        };
        if self.targets.is_empty() {
            return LandingAnalysis {
                landing: Landing::Unknown {
                    reason: UnknownReason::NoDefaultBranch,
                    candidate: None,
                },
                focus: None,
            };
        }

        let mut first_adds_content = None;
        let mut unknowns = Vec::new();

        for target in &self.targets {
            let Some(target_oid) = &target.oid else {
                unknowns.push((target.unavailable_reason.clone(), None, target.clone()));
                continue;
            };

            match landing_for_target(
                git,
                root,
                head,
                target_oid,
                &self.config,
                &self.cache,
                depth,
            ) {
                TargetOutcome::Landed(proof) => {
                    return LandingAnalysis {
                        landing: Landing::Landed {
                            target: target.name.clone(),
                            proof,
                        },
                        focus: None,
                    };
                }
                TargetOutcome::AddsContent { tree } => {
                    first_adds_content.get_or_insert_with(|| (target.clone(), tree));
                }
                TargetOutcome::Unknown { reason, candidate } => {
                    unknowns.push((reason, candidate, target.clone()));
                }
            }
        }

        if !unknowns.is_empty() {
            let (reason, candidate, target) = unknowns
                .iter()
                .find(|(_, candidate, _)| candidate.is_some())
                .cloned()
                .unwrap_or_else(|| unknowns.remove(0));
            let compared = candidate.and_then(|candidate| {
                let target_oid = target.oid.as_deref()?;
                let candidate = resolve_commit(git, root, &candidate)?;
                let base = merge_base(git, root, target_oid, head)?;
                let comparison =
                    compare_deltas(git, root, &base, head, target_oid, &target.name, &candidate)?;
                Some((base, candidate, comparison))
            });
            let candidate = compared
                .as_ref()
                .map(|(_, _, comparison)| comparison.summary.clone());
            let focus = compared.map(|(base, candidate, comparison)| {
                LandingFocus::Candidate(Box::new(CandidateFocus {
                    target,
                    base,
                    head: head.to_string(),
                    candidate,
                    comparison,
                }))
            });
            return LandingAnalysis {
                landing: Landing::Unknown { reason, candidate },
                focus,
            };
        }

        match first_adds_content {
            Some((target, tree)) => LandingAnalysis {
                landing: Landing::AddsContent {
                    target: target.name.clone(),
                },
                focus: Some(LandingFocus::AddsContent { target, tree }),
            },
            None => LandingAnalysis {
                landing: Landing::Unknown {
                    reason: UnknownReason::TargetUnavailable,
                    candidate: None,
                },
                focus: None,
            },
        }
    }

    pub(crate) fn landing_revision(
        &self,
        git: &Git,
        root: &Path,
        head: &str,
        depth: LandingDepth,
    ) -> Landing {
        let Some(head) = resolve_commit(git, root, head) else {
            return Landing::Unknown {
                reason: UnknownReason::HeadUnavailable,
                candidate: None,
            };
        };
        self.landing(git, root, Some(&head), depth)
    }

    pub(crate) fn focused_patch(
        &self,
        git: &Git,
        root: &Path,
        head: &str,
        max_bytes: usize,
    ) -> FocusedPatch {
        let Some(head) = resolve_commit(git, root, head) else {
            return FocusedPatch::All {
                reason: AllChangesReason::Unsafe,
            };
        };
        focused_patch_with_context(git, root, head, self, max_bytes)
    }

    pub(crate) fn focused_patch_for_head_oid(
        &self,
        git: &Git,
        root: &Path,
        head: &str,
        max_bytes: usize,
    ) -> FocusedPatch {
        if !is_hex(head.as_bytes()) {
            return FocusedPatch::All {
                reason: AllChangesReason::Unsafe,
            };
        }
        focused_patch_with_context(git, root, head.to_string(), self, max_bytes)
    }
}

#[derive(Debug, Clone)]
struct Target {
    name: String,
    oid: Option<String>,
    unavailable_reason: UnknownReason,
}

struct LandingAnalysis {
    landing: Landing,
    focus: Option<LandingFocus>,
}

enum LandingFocus {
    // Boxed: this variant carries the whole delta comparison and the other is
    // two strings, so leaving it inline made every value of this enum as large
    // as its heaviest case.
    Candidate(Box<CandidateFocus>),
    AddsContent { target: Target, tree: String },
}

struct CandidateFocus {
    target: Target,
    base: String,
    head: String,
    candidate: String,
    comparison: DeltaComparison,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverState {
    None,
    Present,
    Unavailable,
}

#[derive(Debug, Clone)]
struct MergeConfiguration {
    fingerprint: Arc<MergeFingerprint>,
    drivers: DriverState,
    attributes_reliable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MergeFingerprint(Vec<FingerprintPart>);

// Cache equality keeps the complete inputs rather than a compact digest. A
// collision here could reuse a no-op proved under different merge behavior and
// turn a performance optimisation into permission to delete work.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FingerprintPart {
    Command(&'static str, Option<i32>, Vec<u8>),
    Environment(&'static str, Option<OsString>),
    File(PathBuf, FingerprintedFile),
    AttributesReliable(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FingerprintedFile {
    Present(Vec<u8>),
    Missing,
    Unreadable(String),
}

impl MergeConfiguration {
    fn read(git: &Git, root: &Path) -> Self {
        let result = git.run_status(
            root,
            &["config", "--null", "--get-regexp", "^merge\\..*\\.driver$"],
        );
        let (code, bytes, drivers) = match result {
            Ok(out) => {
                let drivers = match out.code {
                    Some(0) => DriverState::Present,
                    Some(1) => DriverState::None,
                    _ => DriverState::Unavailable,
                };
                (out.code, out.stdout, drivers)
            }
            Err(_) => (None, Vec::new(), DriverState::Unavailable),
        };

        let mut fingerprint = vec![FingerprintPart::Command("merge drivers", code, bytes)];
        let mut attributes_reliable = true;
        for name in [
            "GIT_ATTR_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_SYSTEM",
            "HOME",
            "XDG_CONFIG_HOME",
        ] {
            fingerprint.push(FingerprintPart::Environment(name, std::env::var_os(name)));
        }
        match git.run_status(
            root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "info/attributes",
            ],
        ) {
            Ok(out) => {
                fingerprint.push(FingerprintPart::Command(
                    "repository attributes",
                    out.code,
                    out.stdout.clone(),
                ));
                if out.code == Some(0)
                    && let Some(path) = Self::output_path(&out.stdout)
                    && path.is_absolute()
                {
                    attributes_reliable &= Self::record_file_state(&path, &mut fingerprint);
                } else {
                    attributes_reliable = false;
                }
            }
            Err(_) => {
                fingerprint.push(FingerprintPart::Command(
                    "repository attributes",
                    None,
                    Vec::new(),
                ));
                attributes_reliable = false;
            }
        }
        if std::env::var_os("GIT_ATTR_NOSYSTEM").is_none() {
            match git.run_status(root, &["--exec-path"]) {
                Ok(out) => {
                    fingerprint.push(FingerprintPart::Command(
                        "git exec path",
                        out.code,
                        out.stdout.clone(),
                    ));
                    if out.code == Some(0)
                        && let Some(exec_path) = Self::output_path(&out.stdout)
                        && let Some(prefix) = exec_path.parent().and_then(Path::parent)
                    {
                        attributes_reliable &= Self::record_file_state(
                            &prefix.join("etc/gitattributes"),
                            &mut fingerprint,
                        );
                    } else {
                        attributes_reliable = false;
                    }
                }
                Err(_) => {
                    fingerprint.push(FingerprintPart::Command("git exec path", None, Vec::new()));
                    attributes_reliable = false;
                }
            }
        }

        let attributes_file =
            git.run_status(root, &["config", "--path", "--get", "core.attributesFile"]);
        match attributes_file {
            Ok(out) => {
                fingerprint.push(FingerprintPart::Command(
                    "global attributes",
                    out.code,
                    out.stdout.clone(),
                ));
                if out.code == Some(0)
                    && let Some(path) = Self::output_path(&out.stdout)
                {
                    if path.is_relative() {
                        attributes_reliable = false;
                    } else {
                        attributes_reliable &= Self::record_file_state(&path, &mut fingerprint);
                    }
                } else if out.code == Some(1) {
                    let paths = Self::default_global_attribute_paths();
                    attributes_reliable &= !paths.is_empty();
                    for path in paths {
                        attributes_reliable &= Self::record_file_state(&path, &mut fingerprint);
                    }
                } else {
                    attributes_reliable = false;
                }
            }
            Err(_) => {
                fingerprint.push(FingerprintPart::Command(
                    "global attributes",
                    None,
                    Vec::new(),
                ));
                attributes_reliable = false;
            }
        }
        fingerprint.push(FingerprintPart::AttributesReliable(attributes_reliable));
        Self {
            fingerprint: Arc::new(MergeFingerprint(fingerprint)),
            drivers,
            attributes_reliable,
        }
    }

    fn output_path(out: &[u8]) -> Option<PathBuf> {
        let value = std::str::from_utf8(out).ok()?.trim();
        (!value.is_empty()).then(|| PathBuf::from(value))
    }

    fn default_global_attribute_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            paths.push(PathBuf::from(xdg).join("git/attributes"));
        } else if let Some(home) = std::env::var_os("HOME") {
            paths.push(PathBuf::from(home).join(".config/git/attributes"));
        } else if let Some(profile) = std::env::var_os("USERPROFILE") {
            // Git for Windows resolves its home from USERPROFILE when the
            // Unix-style HOME variable is absent. Treating that as "no global
            // attribute path exists" made the merge configuration unreliable
            // on Windows and conservatively downgraded every squash/no-op proof
            // to Review.
            paths.push(PathBuf::from(profile).join(".config/git/attributes"));
        } else if let (Some(drive), Some(home_path)) =
            (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
        {
            let mut home = PathBuf::from(drive);
            home.push(home_path);
            paths.push(home.join(".config/git/attributes"));
        }
        paths
    }

    fn record_file_state(path: &Path, fingerprint: &mut Vec<FingerprintPart>) -> bool {
        let (state, reliable) = match std::fs::read(path) {
            Ok(bytes) => (FingerprintedFile::Present(bytes), true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (FingerprintedFile::Missing, true)
            }
            Err(error) => (
                FingerprintedFile::Unreadable(error.kind().to_string()),
                false,
            ),
        };
        fingerprint.push(FingerprintPart::File(path.to_path_buf(), state));
        reliable
    }
}

#[derive(Debug, Clone, Default)]
pub struct LandingCache {
    state: Arc<Mutex<CacheState>>,
}

#[derive(Debug, Default)]
struct CacheState {
    results: HashMap<ResultKey, CachedResult>,
    focused_patches: HashMap<FocusedPatchKey, FocusedPatch>,
    trees: HashMap<(PathBuf, String), String>,
    ancestry: HashMap<(PathBuf, String, String), bool>,
    histories: HashMap<HistoryKey, Vec<HistoryCommit>>,
    patch_indexes: HashMap<PatchIndexKey, HashMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResultKey {
    root: PathBuf,
    head: String,
    target: String,
    fingerprint: Arc<MergeFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HistoryKey {
    root: PathBuf,
    base: String,
    target: String,
    fingerprint: Arc<MergeFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PatchIndexKey {
    root: PathBuf,
    target: String,
    fingerprint: Arc<MergeFingerprint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FocusedPatchKey {
    root: PathBuf,
    head: String,
    targets: Vec<(String, Option<String>)>,
    fingerprint: Arc<MergeFingerprint>,
    max_bytes: usize,
}

#[derive(Debug, Clone)]
struct HistoryCommit {
    oid: String,
    tree: String,
    subject: Vec<u8>,
}

#[derive(Debug, Clone)]
enum TargetOutcome {
    Landed(LandingProof),
    AddsContent {
        tree: String,
    },
    Unknown {
        reason: UnknownReason,
        candidate: Option<String>,
    },
}

#[derive(Debug, Clone)]
enum CachedResult {
    Final(TargetOutcome),
    TierOne {
        head_tree: String,
        target_tree: String,
    },
    Conflict {
        paths: usize,
        head_tree: String,
    },
}

fn landing_for_target(
    git: &Git,
    root: &Path,
    head: &str,
    target: &str,
    config: &MergeConfiguration,
    cache: &LandingCache,
    depth: LandingDepth,
) -> TargetOutcome {
    let key = ResultKey {
        root: root.to_path_buf(),
        head: head.to_string(),
        target: target.to_string(),
        fingerprint: config.fingerprint.clone(),
    };
    if let Some(cached) = cached_result(cache, &key) {
        return match cached {
            CachedResult::Final(outcome) => outcome,
            CachedResult::TierOne {
                head_tree,
                target_tree,
            } => {
                if depth == LandingDepth::TierOne {
                    TargetOutcome::Unknown {
                        reason: UnknownReason::CheckDeferred,
                        candidate: None,
                    }
                } else {
                    finish_with_merge_tree(
                        git,
                        root,
                        head,
                        target,
                        config,
                        cache,
                        depth,
                        key,
                        head_tree,
                        target_tree,
                    )
                }
            }
            CachedResult::Conflict { paths, head_tree } => {
                if depth == LandingDepth::History {
                    let outcome = HistoricalSearch {
                        git,
                        root,
                        head,
                        target,
                        config,
                        cache,
                    }
                    .rescue(&head_tree, paths);
                    store_final(cache, key, outcome.clone());
                    outcome
                } else {
                    overlapping_changes(paths)
                }
            }
        };
    }

    match is_ancestor(git, root, head, target, cache) {
        Ancestor::Yes => {
            let outcome = TargetOutcome::Landed(LandingProof::Ancestry);
            store_final(cache, key, outcome.clone());
            return outcome;
        }
        Ancestor::No => {}
        Ancestor::Unknown => {
            return TargetOutcome::Unknown {
                reason: UnknownReason::GitCommandFailed {
                    phase: crate::model::ProofPhase::Ancestry,
                },
                candidate: None,
            };
        }
    }

    let Some(head_tree) = cached_tree(git, root, head, cache) else {
        return TargetOutcome::Unknown {
            reason: UnknownReason::GitCommandFailed {
                phase: crate::model::ProofPhase::TreeComparison,
            },
            candidate: None,
        };
    };
    let Some(target_tree) = cached_tree(git, root, target, cache) else {
        return TargetOutcome::Unknown {
            reason: UnknownReason::GitCommandFailed {
                phase: crate::model::ProofPhase::TreeComparison,
            },
            candidate: None,
        };
    };

    if head_tree == target_tree {
        let outcome = TargetOutcome::Landed(LandingProof::SameTree);
        store_final(cache, key, outcome.clone());
        return outcome;
    }

    match is_ancestor(git, root, target, head, cache) {
        Ancestor::Yes => {
            let outcome = TargetOutcome::AddsContent { tree: head_tree };
            store_final(cache, key, outcome.clone());
            return outcome;
        }
        Ancestor::No => {}
        Ancestor::Unknown => {
            return TargetOutcome::Unknown {
                reason: UnknownReason::GitCommandFailed {
                    phase: crate::model::ProofPhase::Ancestry,
                },
                candidate: None,
            };
        }
    }

    if depth == LandingDepth::TierOne {
        store_tier_one(cache, key, head_tree, target_tree);
        return TargetOutcome::Unknown {
            reason: UnknownReason::CheckDeferred,
            candidate: None,
        };
    }

    finish_with_merge_tree(
        git,
        root,
        head,
        target,
        config,
        cache,
        depth,
        key,
        head_tree,
        target_tree,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_with_merge_tree(
    git: &Git,
    root: &Path,
    head: &str,
    target: &str,
    config: &MergeConfiguration,
    cache: &LandingCache,
    depth: LandingDepth,
    key: ResultKey,
    head_tree: String,
    target_tree: String,
) -> TargetOutcome {
    match merge_tree(git, root, target, head) {
        MergeResult::Clean { tree } if tree == target_tree => {
            let outcome = match no_op_is_safe(git, root, head, target, config) {
                Ok(()) => TargetOutcome::Landed(LandingProof::NoOpAtTip),
                Err(reason) => TargetOutcome::Unknown {
                    reason,
                    candidate: None,
                },
            };
            store_final(cache, key, outcome.clone());
            outcome
        }
        MergeResult::Clean { tree } => {
            let outcome = TargetOutcome::AddsContent { tree };
            store_final(cache, key, outcome.clone());
            outcome
        }
        MergeResult::Conflicts { paths, .. } => {
            store_conflict(cache, key.clone(), paths, head_tree.clone());
            if depth == LandingDepth::History {
                let outcome = HistoricalSearch {
                    git,
                    root,
                    head,
                    target,
                    config,
                    cache,
                }
                .rescue(&head_tree, paths);
                store_final(cache, key, outcome.clone());
                outcome
            } else {
                overlapping_changes(paths)
            }
        }
        MergeResult::Unavailable => TargetOutcome::Unknown {
            reason: UnknownReason::MergeTreeUnavailable,
            candidate: None,
        },
        MergeResult::Malformed => TargetOutcome::Unknown {
            reason: UnknownReason::MalformedMergeTree,
            candidate: None,
        },
    }
}

fn overlapping_changes(paths: usize) -> TargetOutcome {
    TargetOutcome::Unknown {
        reason: UnknownReason::OverlappingChanges { paths },
        candidate: None,
    }
}

fn cached_result(cache: &LandingCache, key: &ResultKey) -> Option<CachedResult> {
    lock_cache(cache).results.get(key).cloned()
}

fn store_final(cache: &LandingCache, key: ResultKey, outcome: TargetOutcome) {
    lock_cache(cache)
        .results
        .insert(key, CachedResult::Final(outcome));
}

fn store_tier_one(cache: &LandingCache, key: ResultKey, head_tree: String, target_tree: String) {
    lock_cache(cache).results.insert(
        key,
        CachedResult::TierOne {
            head_tree,
            target_tree,
        },
    );
}

fn store_conflict(cache: &LandingCache, key: ResultKey, paths: usize, head_tree: String) {
    lock_cache(cache)
        .results
        .insert(key, CachedResult::Conflict { paths, head_tree });
}

fn lock_cache(cache: &LandingCache) -> std::sync::MutexGuard<'_, CacheState> {
    cache
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn ancestry_key(root: &Path, older: &str, newer: &str) -> (PathBuf, String, String) {
    (root.to_path_buf(), older.to_string(), newer.to_string())
}

fn cached_ancestry(cache: &LandingCache, root: &Path, older: &str, newer: &str) -> Option<bool> {
    lock_cache(cache)
        .ancestry
        .get(&ancestry_key(root, older, newer))
        .copied()
}

fn prepare_ancestors_of_target(
    git: &Git,
    root: &Path,
    heads: &[String],
    target: &str,
    cache: &LandingCache,
) {
    let heads = heads.iter().cloned().collect::<HashSet<_>>();
    if heads.is_empty() {
        return;
    }

    // `--no-walk` emits only requested tips that are not in the target's
    // reachable set. It is the batched form of the same reachability question
    // as `merge-base --is-ancestor`, avoiding one process per worktree without
    // traversing or materialising the target's full history.
    let mut args = vec!["rev-list".to_string(), "--no-walk=unsorted".to_string()];
    args.extend(heads.iter().cloned());
    args.push("--not".to_string());
    args.push(target.to_string());
    args.push("--".to_string());
    let Ok(out) = git.run_status(root, &args) else {
        return;
    };
    if out.code != Some(0) {
        return;
    }

    let mut outside = HashSet::new();
    for line in out.stdout.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let Some(oid) = parse_oid(line) else {
            return;
        };
        if !heads.contains(&oid) {
            return;
        }
        outside.insert(oid);
    }

    let mut state = lock_cache(cache);
    for head in heads {
        state
            .ancestry
            .insert(ancestry_key(root, &head, target), !outside.contains(&head));
    }
}

fn prepare_ancestry(git: &Git, root: &Path, pairs: &[(String, String)], cache: &LandingCache) {
    // Tier one is the only proof paid on every cold scan. Four workers hide
    // process startup without reproducing the CPU burst of one process per
    // worktree, and immutable pair results make later scans pure cache reads.
    const CONCURRENCY: usize = 4;

    let mut seen = HashSet::new();
    let pairs = pairs
        .iter()
        .filter(|pair| seen.insert((*pair).clone()))
        .collect::<Vec<_>>();
    if pairs.is_empty() {
        return;
    }
    let chunk_size = pairs.len().div_ceil(CONCURRENCY);
    std::thread::scope(|scope| {
        for chunk in pairs.chunks(chunk_size) {
            scope.spawn(move || {
                for (older, newer) in chunk.iter().copied() {
                    is_ancestor(git, root, older, newer, cache);
                }
            });
        }
    });
}

fn prepare_trees(git: &Git, root: &Path, oids: &[String], cache: &LandingCache) {
    let mut seen = HashSet::new();
    let missing = {
        let state = lock_cache(cache);
        oids.iter()
            .filter(|oid| {
                seen.insert((*oid).clone())
                    && !state
                        .trees
                        .contains_key(&(root.to_path_buf(), (*oid).clone()))
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    if missing.is_empty() {
        return;
    }

    let input = missing
        .iter()
        .map(|oid| format!("{oid}^{{tree}}\n"))
        .collect::<String>();
    let Ok(out) = git.run_status_with_input(
        root,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        input.as_bytes(),
    ) else {
        return;
    };
    if out.code != Some(0) {
        return;
    }
    let lines = out
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if lines.len() != missing.len() {
        return;
    }

    let mut resolved = Vec::with_capacity(missing.len());
    for (oid, line) in missing.into_iter().zip(lines) {
        let Some(separator) = line.iter().position(|byte| *byte == b' ') else {
            return;
        };
        let tree = &line[..separator];
        let kind = &line[separator + 1..];
        if kind != b"tree" {
            return;
        }
        let Some(tree) = parse_oid(tree) else {
            return;
        };
        resolved.push(((root.to_path_buf(), oid), tree));
    }
    lock_cache(cache).trees.extend(resolved);
}

fn cached_tree_only(root: &Path, oid: &str, cache: &LandingCache) -> Option<String> {
    lock_cache(cache)
        .trees
        .get(&(root.to_path_buf(), oid.to_string()))
        .cloned()
}

struct HistoricalSearch<'a> {
    git: &'a Git,
    root: &'a Path,
    head: &'a str,
    target: &'a str,
    config: &'a MergeConfiguration,
    cache: &'a LandingCache,
}

impl HistoricalSearch<'_> {
    fn rescue(&self, head_tree: &str, conflict_paths: usize) -> TargetOutcome {
        let Some(base) = merge_base(self.git, self.root, self.head, self.target) else {
            return TargetOutcome::Unknown {
                reason: UnknownReason::OverlappingChanges {
                    paths: conflict_paths,
                },
                candidate: None,
            };
        };
        match history_range_size(self.git, self.root, &base, self.target) {
            Some(commits) if commits > HISTORY_COMMIT_LIMIT => {
                return TargetOutcome::Unknown {
                    reason: UnknownReason::HistoryRangeTooLarge {
                        commits,
                        limit: HISTORY_COMMIT_LIMIT,
                    },
                    candidate: None,
                };
            }
            Some(_) => {}
            None => {
                return TargetOutcome::Unknown {
                    reason: UnknownReason::GitCommandFailed {
                        phase: crate::model::ProofPhase::History,
                    },
                    candidate: None,
                };
            }
        }
        let candidates = self.candidates(&base, head_tree);
        let first_candidate = candidates.first().map(|candidate| candidate.oid.clone());
        let mut unsafe_match = None;

        for candidate in candidates {
            if !matches!(
                is_ancestor(self.git, self.root, &candidate.oid, self.target, self.cache,),
                Ancestor::Yes
            ) {
                continue;
            }
            let Some(candidate_tree) = cached_tree(self.git, self.root, &candidate.oid, self.cache)
            else {
                continue;
            };
            if let MergeResult::Clean { tree } =
                merge_tree(self.git, self.root, &candidate.oid, self.head)
                && tree == candidate_tree
            {
                match no_op_is_safe(self.git, self.root, self.head, &candidate.oid, self.config) {
                    Ok(()) => {
                        let proof = if candidate.patch_equivalent {
                            LandingProof::PatchEquivalent(candidate.oid)
                        } else {
                            LandingProof::NoOpAtAncestor(candidate.oid)
                        };
                        return TargetOutcome::Landed(proof);
                    }
                    Err(reason) => {
                        if matches!(reason, UnknownReason::CustomMergeDriver) {
                            return TargetOutcome::Unknown {
                                reason,
                                candidate: Some(candidate.oid),
                            };
                        }
                        unsafe_match.get_or_insert((reason, candidate.oid));
                    }
                }
            }
        }

        if let Some((reason, candidate)) = unsafe_match {
            return TargetOutcome::Unknown {
                reason,
                candidate: Some(candidate),
            };
        }

        TargetOutcome::Unknown {
            reason: UnknownReason::OverlappingChanges {
                paths: conflict_paths,
            },
            candidate: first_candidate,
        }
    }

    fn candidates(&self, base: &str, head_tree: &str) -> Vec<HistoricalCandidate> {
        let history = target_history(
            self.git,
            self.root,
            base,
            self.target,
            self.config,
            self.cache,
        )
        .unwrap_or_default();
        let patch_index =
            target_patch_index(self.git, self.root, self.target, self.config, self.cache);
        let branch_patch = aggregate_patch_id(self.git, self.root, base, self.head);
        let branch_subjects = branch_subjects(self.git, self.root, base, self.head);

        let mut candidates = Vec::new();
        let mut seen = HashSet::new();

        if let (Some(index), Some(patch)) = (patch_index, branch_patch)
            && let Some(commits) = index.get(&patch)
        {
            let in_range: HashSet<&str> =
                history.iter().map(|commit| commit.oid.as_str()).collect();
            for commit in commits {
                if !in_range.contains(commit.as_str()) {
                    continue;
                }
                push_candidate(&mut candidates, &mut seen, commit, true);
                if candidates.len() == MAX_HISTORY_CANDIDATES {
                    break;
                }
            }
        }
        for commit in &history {
            if candidates.len() == MAX_HISTORY_CANDIDATES {
                break;
            }
            if commit.tree == head_tree {
                push_candidate(&mut candidates, &mut seen, &commit.oid, false);
            }
        }

        for commit in &history {
            if candidates.len() == MAX_HISTORY_CANDIDATES {
                break;
            }
            if branch_subjects.contains(&commit.subject) {
                push_candidate(&mut candidates, &mut seen, &commit.oid, false);
            }
        }

        candidates
    }
}

struct HistoricalCandidate {
    oid: String,
    patch_equivalent: bool,
}

fn push_candidate(
    candidates: &mut Vec<HistoricalCandidate>,
    seen: &mut HashSet<String>,
    oid: &str,
    patch_equivalent: bool,
) -> bool {
    if candidates.len() >= MAX_HISTORY_CANDIDATES {
        return false;
    }
    if !seen.insert(oid.to_string()) {
        return false;
    }
    candidates.push(HistoricalCandidate {
        oid: oid.to_string(),
        patch_equivalent,
    });
    true
}

fn history_range_size(git: &Git, root: &Path, base: &str, target: &str) -> Option<usize> {
    let range = format!("{base}..{target}");
    let max_count = format!("--max-count={}", HISTORY_COMMIT_LIMIT + 1);
    let out = git
        .run_status(root, &["rev-list", "--count", &max_count, &range, "--"])
        .ok()?;
    if out.code != Some(0) {
        return None;
    }
    std::str::from_utf8(&out.stdout).ok()?.trim().parse().ok()
}

fn target_history(
    git: &Git,
    root: &Path,
    base: &str,
    target: &str,
    config: &MergeConfiguration,
    cache: &LandingCache,
) -> Option<Vec<HistoryCommit>> {
    let key = HistoryKey {
        root: root.to_path_buf(),
        base: base.to_string(),
        target: target.to_string(),
        fingerprint: config.fingerprint.clone(),
    };
    if let Some(history) = lock_cache(cache).histories.get(&key).cloned() {
        return Some(history);
    }

    let range = format!("{base}..{target}");
    let out = git
        .run_status(
            root,
            &["log", "-z", "--format=%H%x00%T%x00%s", &range, "--"],
        )
        .ok()?;
    if out.code != Some(0) {
        return None;
    }
    let records = nul_fields(&out.stdout)?;
    if records.len() % 3 != 0 {
        return None;
    }

    let mut history = Vec::with_capacity(records.len() / 3);
    for fields in records.chunks_exact(3) {
        history.push(HistoryCommit {
            oid: parse_oid(fields[0])?,
            tree: parse_oid(fields[1])?,
            subject: fields[2].to_vec(),
        });
    }
    lock_cache(cache).histories.insert(key, history.clone());
    Some(history)
}

fn target_patch_index(
    git: &Git,
    root: &Path,
    target: &str,
    config: &MergeConfiguration,
    cache: &LandingCache,
) -> Option<HashMap<String, Vec<String>>> {
    let key = PatchIndexKey {
        root: root.to_path_buf(),
        target: target.to_string(),
        fingerprint: config.fingerprint.clone(),
    };
    if let Some(index) = lock_cache(cache).patch_indexes.get(&key).cloned() {
        return Some(index);
    }

    // A target-scoped window lets every inspected worktree share the only
    // linear-cost index. Matches are still filtered through the exact B..T
    // history; a commit outside the window merely leaves the answer Unknown and
    // can never become evidence by itself.
    let max_count = format!("--max-count={HISTORY_COMMIT_LIMIT}");
    let log = git
        .run_status(
            root,
            &[
                "log",
                "--no-merges",
                "--format=%H",
                "--binary",
                "--no-ext-diff",
                "-p",
                "--topo-order",
                &max_count,
                target,
                "--",
            ],
        )
        .ok()?;
    if log.code != Some(0) {
        return None;
    }
    let output = git
        .run_status_with_input(root, &["patch-id", "--stable"], &log.stdout)
        .ok()?;
    if output.code != Some(0) {
        return None;
    }
    let index = parse_patch_index(&output.stdout)?;
    lock_cache(cache).patch_indexes.insert(key, index.clone());
    Some(index)
}

fn aggregate_patch_id(git: &Git, root: &Path, base: &str, head: &str) -> Option<String> {
    let diff = git
        .run_status(
            root,
            &["diff", "--binary", "--no-ext-diff", base, head, "--"],
        )
        .ok()?;
    if diff.code != Some(0) {
        return None;
    }
    let output = git
        .run_status_with_input(root, &["patch-id", "--stable"], &diff.stdout)
        .ok()?;
    if output.code != Some(0) {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    let mut lines = text.lines();
    let patch = lines.next()?.split_whitespace().next()?;
    if lines.next().is_some() || !is_hex(patch.as_bytes()) {
        return None;
    }
    Some(patch.to_string())
}

fn parse_patch_index(out: &[u8]) -> Option<HashMap<String, Vec<String>>> {
    let text = std::str::from_utf8(out).ok()?;
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let patch = fields.next()?;
        let commit = fields.next()?;
        if fields.next().is_some() || !is_hex(patch.as_bytes()) || !is_hex(commit.as_bytes()) {
            return None;
        }
        index
            .entry(patch.to_string())
            .or_default()
            .push(commit.to_string());
    }
    Some(index)
}

fn branch_subjects(git: &Git, root: &Path, base: &str, head: &str) -> HashSet<Vec<u8>> {
    let range = format!("{base}..{head}");
    let max_count = format!("--max-count={HISTORY_COMMIT_LIMIT}");
    let Ok(out) = git.run_status(
        root,
        &["log", "-z", "--format=%s", &max_count, &range, "--"],
    ) else {
        return HashSet::new();
    };
    if out.code != Some(0) {
        return HashSet::new();
    }
    nul_fields(&out.stdout)
        .map(|records| records.into_iter().map(Vec::from).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ancestor {
    Yes,
    No,
    Unknown,
}

fn is_ancestor(git: &Git, root: &Path, older: &str, newer: &str, cache: &LandingCache) -> Ancestor {
    if let Some(is_ancestor) = cached_ancestry(cache, root, older, newer) {
        return if is_ancestor {
            Ancestor::Yes
        } else {
            Ancestor::No
        };
    }
    let result = match git.run_status(root, &["merge-base", "--is-ancestor", older, newer]) {
        Ok(out) if out.code == Some(0) => Ancestor::Yes,
        Ok(out) if out.code == Some(1) => Ancestor::No,
        _ => Ancestor::Unknown,
    };
    if !matches!(result, Ancestor::Unknown) {
        lock_cache(cache).ancestry.insert(
            ancestry_key(root, older, newer),
            matches!(result, Ancestor::Yes),
        );
    }
    result
}

fn merge_base(git: &Git, root: &Path, left: &str, right: &str) -> Option<String> {
    let out = git.run_status(root, &["merge-base", left, right]).ok()?;
    if out.code != Some(0) {
        return None;
    }
    parse_oid_output(&out.stdout)
}

fn resolve_commit(git: &Git, root: &Path, rev: &str) -> Option<String> {
    resolve_object(git, root, rev, "commit")
}

fn resolve_tree(git: &Git, root: &Path, rev: &str) -> Option<String> {
    resolve_object(git, root, rev, "tree")
}

fn cached_tree(git: &Git, root: &Path, oid: &str, cache: &LandingCache) -> Option<String> {
    let key = (root.to_path_buf(), oid.to_string());
    if let Some(tree) = lock_cache(cache).trees.get(&key).cloned() {
        return Some(tree);
    }
    let tree = resolve_tree(git, root, oid)?;
    lock_cache(cache).trees.insert(key, tree.clone());
    Some(tree)
}

fn resolve_object(git: &Git, root: &Path, rev: &str, kind: &str) -> Option<String> {
    let spec = format!("{rev}^{{{kind}}}");
    let out = git
        .run_status(root, &["rev-parse", "--verify", "--end-of-options", &spec])
        .ok()?;
    if out.code != Some(0) {
        return None;
    }
    parse_oid_output(&out.stdout)
}

fn parse_oid_output(out: &[u8]) -> Option<String> {
    let value = out.strip_suffix(b"\n").unwrap_or(out);
    let value = value.strip_suffix(b"\r").unwrap_or(value);
    parse_oid(value)
}

fn parse_oid(value: &[u8]) -> Option<String> {
    if !is_hex(value) {
        return None;
    }
    std::str::from_utf8(value).ok().map(str::to_string)
}

fn is_hex(value: &[u8]) -> bool {
    !value.is_empty() && value.iter().all(u8::is_ascii_hexdigit)
}

#[derive(Debug, PartialEq, Eq)]
enum MergeResult {
    Clean { tree: String },
    Conflicts { paths: usize },
    Unavailable,
    Malformed,
}

fn merge_tree(git: &Git, root: &Path, target: &str, head: &str) -> MergeResult {
    let Ok(out) = git.run_status(
        root,
        &[
            "-c",
            "merge.renormalize=false",
            "merge-tree",
            "--write-tree",
            "--name-only",
            "-z",
            "--no-messages",
            target,
            head,
        ],
    ) else {
        return MergeResult::Unavailable;
    };
    parse_merge_tree_output(out.code, &out.stdout)
}

fn parse_merge_tree_output(code: Option<i32>, output: &[u8]) -> MergeResult {
    if !matches!(code, Some(0 | 1)) {
        return MergeResult::Unavailable;
    }
    let Some(mut records) = nul_fields(output) else {
        return MergeResult::Malformed;
    };
    if records.is_empty() {
        return MergeResult::Malformed;
    }
    let Some(tree) = parse_oid(records.remove(0)) else {
        return MergeResult::Malformed;
    };
    let paths = records
        .iter()
        .take_while(|record| !record.is_empty())
        .count();

    match code {
        Some(0) if records.iter().all(|record| record.is_empty()) => MergeResult::Clean { tree },
        Some(1) if paths > 0 => MergeResult::Conflicts { paths },
        _ => MergeResult::Malformed,
    }
}

fn nul_records(out: &[u8]) -> Option<Vec<&[u8]>> {
    let records = nul_fields(out)?;
    if records.iter().any(|record| record.is_empty()) {
        return None;
    }
    Some(records)
}

fn nul_fields(out: &[u8]) -> Option<Vec<&[u8]>> {
    if !out.ends_with(b"\0") {
        return None;
    }
    let mut records: Vec<&[u8]> = out.split(|byte| *byte == 0).collect();
    if records.pop() != Some(&[]) {
        return None;
    }
    Some(records)
}

fn no_op_is_safe(
    git: &Git,
    root: &Path,
    head: &str,
    snapshot: &str,
    config: &MergeConfiguration,
) -> Result<(), UnknownReason> {
    if !config.attributes_reliable {
        return Err(UnknownReason::MergeAttributes);
    }
    match config.drivers {
        DriverState::Present => return Err(UnknownReason::CustomMergeDriver),
        DriverState::Unavailable => {
            return Err(UnknownReason::GitCommandFailed {
                phase: crate::model::ProofPhase::MergeConfiguration,
            });
        }
        DriverState::None => {}
    }

    let base = merge_base(git, root, head, snapshot).ok_or(UnknownReason::GitCommandFailed {
        phase: crate::model::ProofPhase::Ancestry,
    })?;
    let paths = changed_paths(git, root, &base, head)?;
    if paths.is_empty() {
        return Ok(());
    }
    let input = paths
        .iter()
        .flat_map(|path| path.iter().copied().chain(std::iter::once(0)))
        .collect::<Vec<_>>();

    for source in [&base, head, snapshot] {
        let source_arg = format!("--source={source}");
        let out = git
            .run_status_with_input(
                root,
                &["check-attr", "-z", &source_arg, "--stdin", "merge"],
                &input,
            )
            .map_err(|_| UnknownReason::MergeAttributes)?;
        if out.code != Some(0) || !attributes_are_unspecified(&out.stdout, &paths) {
            return Err(UnknownReason::MergeAttributes);
        }
    }
    Ok(())
}

fn changed_paths(
    git: &Git,
    root: &Path,
    base: &str,
    head: &str,
) -> Result<Vec<Vec<u8>>, UnknownReason> {
    let out = git
        .run_status(root, &["diff", "--name-status", "-z", base, head, "--"])
        .map_err(|_| UnknownReason::GitCommandFailed {
            phase: crate::model::ProofPhase::CandidateComparison,
        })?;
    if out.code != Some(0) {
        return Err(UnknownReason::GitCommandFailed {
            phase: crate::model::ProofPhase::CandidateComparison,
        });
    }
    let records = nul_records(&out.stdout).ok_or(UnknownReason::GitCommandFailed {
        phase: crate::model::ProofPhase::CandidateComparison,
    })?;
    let mut paths = Vec::new();
    let mut index = 0;

    while index < records.len() {
        let status = records[index];
        index += 1;
        let kind = *status.first().ok_or(UnknownReason::GitCommandFailed {
            phase: crate::model::ProofPhase::CandidateComparison,
        })?;
        if !kind.is_ascii_alphabetic() || index >= records.len() {
            return Err(UnknownReason::GitCommandFailed {
                phase: crate::model::ProofPhase::CandidateComparison,
            });
        }
        paths.push(records[index].to_vec());
        index += 1;
        if matches!(kind, b'R' | b'C') {
            if index >= records.len() {
                return Err(UnknownReason::GitCommandFailed {
                    phase: crate::model::ProofPhase::CandidateComparison,
                });
            }
            paths.push(records[index].to_vec());
            index += 1;
        }
    }

    let mut seen = HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    Ok(paths)
}

fn attributes_are_unspecified(out: &[u8], paths: &[Vec<u8>]) -> bool {
    let Some(records) = nul_records(out) else {
        return false;
    };
    if records.len() != paths.len() * 3 {
        return false;
    }

    let expected: HashSet<&[u8]> = paths.iter().map(Vec::as_slice).collect();
    let mut found = HashSet::new();
    for fields in records.chunks_exact(3) {
        if fields[1] != b"merge"
            || fields[2] != b"unspecified"
            || !expected.contains(fields[0])
            || !found.insert(fields[0])
        {
            return false;
        }
    }
    found.len() == expected.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn merge_tree_parser_accepts_object_ids_of_any_length() {
        assert_eq!(
            parse_merge_tree_output(Some(0), b"abcdef\0"),
            MergeResult::Clean {
                tree: "abcdef".into()
            }
        );
    }

    #[test]
    fn merge_tree_parser_counts_paths_before_the_message_separator() {
        assert_eq!(
            parse_merge_tree_output(Some(1), b"abcdef\0one.txt\0two.txt\0\0"),
            MergeResult::Conflicts { paths: 2 }
        );
    }

    #[test]
    fn name_status_parser_shape_rejects_missing_terminator() {
        assert!(nul_records(b"M\0file.txt").is_none());
    }

    #[test]
    fn attribute_parser_requires_every_path_to_be_unspecified() {
        let paths = vec![b"a.txt".to_vec(), b"b.txt".to_vec()];
        let safe = b"a.txt\0merge\0unspecified\0b.txt\0merge\0unspecified\0";
        let unsafe_output = b"a.txt\0merge\0ours\0b.txt\0merge\0unspecified\0";
        assert!(attributes_are_unspecified(safe, &paths));
        assert!(!attributes_are_unspecified(unsafe_output, &paths));
    }

    struct DeltaRepo {
        _dir: tempfile::TempDir,
        root: PathBuf,
        head: String,
        candidate: String,
    }

    impl DeltaRepo {
        fn new(path: &str, base: &str, branch: &str, candidate: &str) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = dir.path().join("repo");
            run_git(dir.path(), &["init", "-q", "-b", "main", "repo"]);
            run_git(&root, &["config", "user.email", "test@yawm.dev"]);
            run_git(&root, &["config", "user.name", "yawm test"]);
            write_file(&root, path, base);
            run_git(&root, &["add", "-A"]);
            run_git(&root, &["commit", "-qm", "base"]);

            run_git(&root, &["checkout", "-qb", "feature"]);
            write_file(&root, path, branch);
            run_git(&root, &["add", "-A"]);
            run_git(&root, &["commit", "-qm", "feature"]);
            let head = git_stdout(&root, &["rev-parse", "HEAD"]);

            run_git(&root, &["checkout", "-q", "main"]);
            write_file(&root, path, candidate);
            run_git(&root, &["add", "-A"]);
            run_git(&root, &["commit", "--allow-empty", "-qm", "candidate"]);
            let candidate = git_stdout(&root, &["rev-parse", "HEAD"]);

            Self {
                _dir: dir,
                root,
                head,
                candidate,
            }
        }

        fn patch(&self, git: &Git) -> UniquePatch {
            unique_patch(
                git,
                &self.root,
                &self.head,
                "main",
                &self.candidate,
                usize::MAX,
            )
            .expect("comparison")
        }
    }

    fn write_file(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, contents).expect("write fixture");
    }

    fn run_git(root: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .expect("run git")
                .success(),
            "git {args:?}"
        );
    }

    fn git_stdout(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }

    fn uncommitted_repo(base: &str, target: &str, working: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        run_git(dir.path(), &["init", "-q", "-b", "main", "repo"]);
        run_git(&root, &["config", "user.email", "test@yawm.dev"]);
        run_git(&root, &["config", "user.name", "yawm test"]);
        write_file(&root, "LanguagePickerModal.tsx", base);
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "-qm", "base"]);
        run_git(&root, &["branch", "feature"]);
        write_file(&root, "LanguagePickerModal.tsx", target);
        run_git(&root, &["add", "-A"]);
        run_git(&root, &["commit", "--allow-empty", "-qm", "main copy"]);
        run_git(&root, &["checkout", "-q", "feature"]);
        write_file(&root, "LanguagePickerModal.tsx", working);
        (dir, root)
    }

    #[test]
    fn uncommitted_added_lines_already_on_default_have_no_leftovers() {
        let (_dir, root) = uncommitted_repo(
            "export const languages = [];\n",
            "export const languages = [];\nconst width = trigger.clientWidth;\nsetMenuWidth(width);\n",
            "export const languages = [];\nconst width = trigger.clientWidth;\nsetMenuWidth(width);\n",
        );
        let target = git_stdout(&root, &["rev-parse", "main"]);

        let analysis = uncommitted_against(
            &Git::new(),
            &root,
            Some(("main", &target)),
            &[b"LanguagePickerModal.tsx".to_vec()],
        );

        assert_eq!(
            analysis,
            UncommittedAnalysis::Compared {
                target: "main".into(),
                leftover: 0,
                leftover_sample: Vec::new(),
                incomplete: false,
                shortfall: None,
            }
        );
    }

    #[test]
    fn uncommitted_deletion_of_a_line_default_kept_is_a_leftover() {
        let (_dir, root) = uncommitted_repo(
            "alpha\nkeep this behavior\nomega\n",
            "alpha\nkeep this behavior\nomega\n",
            "alpha\nomega\n",
        );
        let target = git_stdout(&root, &["rev-parse", "main"]);

        let analysis = uncommitted_against(
            &Git::new(),
            &root,
            Some(("main", &target)),
            &[b"LanguagePickerModal.tsx".to_vec()],
        );

        assert_eq!(
            analysis,
            UncommittedAnalysis::Compared {
                target: "main".into(),
                leftover: 1,
                leftover_sample: vec!["keep this behavior".into()],
                incomplete: false,
                shortfall: None,
            }
        );
    }

    /// A capped comparison has to hand over the arithmetic behind the cap.
    ///
    /// `incomplete: true` on its own tells a reader that something went unread
    /// and then refuses to say whether it was one line or a thousand, which is
    /// exactly the warning they cannot act on. These figures are what let the
    /// copy name the threshold and the shortfall.
    #[test]
    fn a_comparison_stopped_by_the_line_budget_reports_the_budget_and_the_remainder() {
        let over_budget = MAX_LEFTOVER_PROBES + 144;
        let body = (0..over_budget)
            .map(|n| format!("line {n} of work that exists nowhere else"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_dir, root) = uncommitted_repo("alpha\n", "alpha\n", &format!("alpha\n{body}\n"));
        let target = git_stdout(&root, &["rev-parse", "main"]);

        let analysis = uncommitted_against(
            &Git::new(),
            &root,
            Some(("main", &target)),
            &[b"LanguagePickerModal.tsx".to_vec()],
        );

        let UncommittedAnalysis::Compared {
            incomplete,
            shortfall,
            ..
        } = analysis
        else {
            panic!("a modified tracked file is a comparable change");
        };
        assert!(incomplete, "the budget ran out, so this is not a full read");
        let shortfall = shortfall.expect("an incomplete comparison must say by how much");
        assert_eq!(
            shortfall,
            ComparisonShortfall {
                lines_compared: MAX_LEFTOVER_PROBES,
                lines_not_compared: 144,
                line_limit: Some(MAX_LEFTOVER_PROBES),
                paths_not_compared: 0,
                counts_exact: true,
            }
        );
        assert_eq!(shortfall.lines_in_scope(), over_budget);
    }

    /// A path skipped for being unreadable is not a line budget problem, and
    /// quoting the budget for it would blame the wrong thing.
    #[test]
    fn an_undecodable_path_is_counted_as_a_path_without_quoting_the_line_budget() {
        let (_dir, root) = uncommitted_repo("alpha\n", "alpha\n", "alpha\n");
        std::fs::write(root.join("blob.bin"), [0u8, 159, 146, 150]).expect("write");
        let target = git_stdout(&root, &["rev-parse", "main"]);

        let analysis = uncommitted_against(
            &Git::new(),
            &root,
            Some(("main", &target)),
            &[b"blob.bin".to_vec()],
        );

        let UncommittedAnalysis::Compared { shortfall, .. } = analysis else {
            panic!("an untracked file is a comparable change");
        };
        assert_eq!(
            shortfall.expect("a skipped path must be counted"),
            ComparisonShortfall {
                lines_compared: 0,
                lines_not_compared: 0,
                line_limit: None,
                paths_not_compared: 1,
                counts_exact: true,
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_dirty_path_is_counted_once_when_it_cannot_be_compared() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, root) = uncommitted_repo("alpha\n", "alpha\n", "alpha\n");
        let raw_path = b"odd-\xff.txt".to_vec();
        let wrapper = root.join("git-non-utf8-path");
        fs::write(
            &wrapper,
            "#!/bin/sh\n\
             if [ \"$1\" = \"diff\" ]; then exit 0; fi\n\
             if [ \"$1\" = \"ls-files\" ]; then printf 'odd-\\377.txt\\0'; exit 0; fi\n\
             exec git \"$@\"\n",
        )
        .expect("write git wrapper");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod wrapper");
        let target = git_stdout(&root, &["rev-parse", "main"]);

        let analysis = uncommitted_against(
            &Git::with_program(wrapper.to_string_lossy()),
            &root,
            Some(("main", &target)),
            &[raw_path],
        );

        let UncommittedAnalysis::Compared { shortfall, .. } = analysis else {
            panic!("an untracked file is a comparable change");
        };
        assert_eq!(
            shortfall.expect("the undecodable path must be counted"),
            ComparisonShortfall {
                lines_compared: 0,
                lines_not_compared: 0,
                line_limit: None,
                paths_not_compared: 1,
                counts_exact: true,
            }
        );
    }

    #[test]
    fn nul_delimited_path_keeps_space_b_in_a_deletion() {
        let repo = DeltaRepo::new(
            "space b/name.txt",
            "keep\nremove me\n",
            "keep\n",
            "keep\nremove me\n",
        );

        let patch = repo.patch(&Git::new());

        assert_eq!(patch.line_count, 1);
        assert_eq!(patch.markers[0].side, UniqueLineSide::Deletions);
    }

    #[cfg(unix)]
    #[test]
    fn failed_blob_read_makes_the_comparison_incomplete() {
        use std::os::unix::fs::PermissionsExt;

        let repo = DeltaRepo::new(
            "file.txt",
            "keep\nremove me\n",
            "keep\n",
            "keep\nremove me\n",
        );
        let wrapper = repo._dir.path().join("git-wrapper");
        fs::write(
            &wrapper,
            "#!/bin/sh\ncase \"$1:$2\" in show:*:*) exit 129;; esac\nexec git \"$@\"\n",
        )
        .expect("write wrapper");
        let mut permissions = fs::metadata(&wrapper).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).expect("chmod");

        let patch = repo.patch(&Git::with_program(wrapper.to_string_lossy()));

        assert!(patch.incomplete);
        assert_eq!(patch.line_count, 0);
    }

    #[test]
    fn punctuation_only_changes_are_evidence() {
        let repo = DeltaRepo::new("data.txt", "{}\n", "()\n", "{}\n");

        let patch = repo.patch(&Git::new());

        assert_eq!(patch.line_count, 2);
    }

    #[test]
    fn indentation_is_compared_exactly() {
        let repo = DeltaRepo::new(
            "script.py",
            "if ready:\n    call()\n",
            "if ready:\ncall()\n",
            "if ready:\n    call()\n",
        );

        let patch = repo.patch(&Git::new());

        assert_eq!(patch.line_count, 2);
    }

    #[test]
    fn duplicate_additions_require_duplicate_target_occurrences() {
        let repo = DeltaRepo::new(
            "auth.txt",
            "start\n",
            "start\nauthorize();\nauthorize();\n",
            "start\nauthorize();\n",
        );

        let patch = repo.patch(&Git::new());

        assert_eq!(patch.line_count, 1);
        assert_eq!(patch.markers[0].side, UniqueLineSide::Additions);
    }

    #[test]
    fn candidate_summary_names_the_ref_not_its_object_id() {
        let repo = DeltaRepo::new("file.txt", "base\n", "branch\n", "candidate\n");

        let comparison = compare_deltas(
            &Git::new(),
            &repo.root,
            &git_stdout(&repo.root, &["merge-base", "main", &repo.head]),
            &repo.head,
            &git_stdout(&repo.root, &["rev-parse", "main"]),
            "main",
            &repo.candidate,
        )
        .expect("comparison");

        assert_eq!(comparison.summary.target, "main");
    }
}

/// How closely a candidate commit's change matches the branch's change.
///
/// Compares the two *deltas*, not the two trees. Comparing files would count
/// every closing brace and every unchanged import as agreement, and a
/// localisation file is mostly punctuation — an early attempt at this scored
/// 87% on a branch by matching `},` sixty-four times. Comparing what each side
/// actually added and removed, per path, cannot be inflated that way.
fn compare_deltas(
    git: &Git,
    root: &Path,
    base: &str,
    head: &str,
    target_oid: &str,
    target_name: &str,
    candidate: &str,
) -> Option<DeltaComparison> {
    let branch = delta_of(
        git,
        root,
        &[
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "--raw",
            "-z",
            "-U0",
            base,
            head,
            "--",
        ],
    )?;
    let landed = delta_of(
        git,
        root,
        &[
            "show",
            "--format=",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "--raw",
            "-z",
            "-U0",
            candidate,
            "--",
        ],
    )?;

    let paths = branch.len();
    if paths == 0 {
        return None;
    }

    // Lines the branch added that the candidate did not. Small by
    // construction when the candidate is a real landing, which is what makes
    // the second pass affordable.
    let mut matching_paths = 0;
    let mut added = 0;
    let mut matching_added = 0;
    let mut orphans: BTreeMap<Vec<u8>, Vec<Orphan>> = BTreeMap::new();
    let mut reservations: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
    for (path, branch_change) in &branch {
        let landed_change = landed.get(path);
        if landed_change.is_some_and(|change| branch_change == change) {
            matching_paths += 1;
        }

        added += branch_change.added.len();
        let (matched_additions, unmatched_additions) = matched_and_unmatched(
            &branch_change.added,
            landed_change.map_or(&[], |change| change.added.as_slice()),
        );
        matching_added += matched_additions.len();
        reservations.insert(path.clone(), matched_additions);
        for line in unmatched_additions {
            orphans.entry(path.clone()).or_default().push(Orphan {
                line,
                side: Side::Added,
            });
        }

        // Removals count too, and they invert the test. A line the branch
        // *added* is unique when the target lacks it; a line the branch
        // *removed* is unique when the target still has it, because that means
        // the deletion never landed. Testing both for absence — which is what
        // this did — called a deletion landed in exactly the case where it was
        // not.
        let (_, unmatched_removals) = matched_and_unmatched(
            &branch_change.removed,
            landed_change.map_or(&[], |change| change.removed.as_slice()),
        );
        for line in unmatched_removals {
            orphans.entry(path.clone()).or_default().push(Orphan {
                line,
                side: Side::Removed,
            });
        }
    }
    for lines in orphans.values_mut() {
        lines.sort_unstable();
    }

    // Then discard any that turn up elsewhere in the target, since the
    // candidate is only one commit and later work may have carried them.
    let (leftovers, scan) = lines_absent_from(git, root, target_oid, &orphans, &reservations);
    let incomplete = scan.incomplete();
    let unsupported_files = branch
        .iter()
        .filter(|(_, change)| {
            change.binary || (change.added.is_empty() && change.removed.is_empty())
        })
        .filter(|(path, _)| !path_is_contained_exactly(git, root, head, target_oid, path))
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();

    Some(DeltaComparison {
        summary: CandidateMatch {
            commit: candidate.to_string(),
            target: target_name.to_string(),
            paths,
            matching_paths,
            added,
            matching_added,
            leftover: leftovers.len(),
            leftover_sample: leftovers
                .iter()
                .take(5)
                .map(|line| String::from_utf8_lossy(&line.content).trim().to_string())
                .collect(),
            incomplete: incomplete || !unsupported_files.is_empty(),
        },
        leftovers,
        unsupported_files,
    })
}

/// Kept beside committed comparisons so additions and deletions cannot acquire
/// opposite, drifting definitions of safety in a second implementation.
pub fn uncommitted_against(
    git: &Git,
    root: &Path,
    target: Option<(&str, &str)>,
    dirty_paths: &[Vec<u8>],
) -> UncommittedAnalysis {
    let Some((target_name, target_oid)) = target else {
        return UncommittedAnalysis::NotChecked;
    };
    let mut skipped_paths = 0usize;
    let mut counts_exact = true;
    let changes = match delta_of(
        git,
        root,
        &[
            "diff",
            "--binary",
            "--no-ext-diff",
            "--no-renames",
            "--raw",
            "-z",
            "-U0",
            "HEAD",
            "--",
        ],
    ) {
        Some(changes) => changes,
        None => {
            // Nothing was read at all. The status pass already named the dirty
            // paths, so that count is exactly what went uninspected.
            return UncommittedAnalysis::Compared {
                target: target_name.to_string(),
                leftover: 0,
                leftover_sample: Vec::new(),
                incomplete: true,
                shortfall: Some(ComparisonShortfall {
                    lines_compared: 0,
                    lines_not_compared: 0,
                    line_limit: None,
                    paths_not_compared: dirty_paths.len(),
                    counts_exact: true,
                }),
            };
        }
    };

    let mut seen = BTreeSet::new();
    let mut orphans: BTreeMap<Vec<u8>, Vec<Orphan>> = BTreeMap::new();
    for (path, change) in changes {
        seen.insert(path.clone());
        let supported = !change.binary
            && !(change.added.is_empty() && change.removed.is_empty())
            && change
                .added
                .iter()
                .chain(&change.removed)
                .all(|line| std::str::from_utf8(line).is_ok());
        if !supported {
            skipped_paths += 1;
            continue;
        }
        let lines = orphans.entry(path).or_default();
        lines.extend(change.added.into_iter().map(|line| Orphan {
            line,
            side: Side::Added,
        }));
        lines.extend(change.removed.into_iter().map(|line| Orphan {
            line,
            side: Side::Removed,
        }));
    }

    match git.run_status(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z", "--"],
    ) {
        Ok(out)
            if out.code == Some(0) && (out.stdout.is_empty() || out.stdout.ends_with(b"\0")) =>
        {
            for path in out
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
            {
                seen.insert(path.to_vec());
                let Ok(path_text) = std::str::from_utf8(path) else {
                    skipped_paths += 1;
                    continue;
                };
                let disk_path = root.join(path_text);
                let Ok(metadata) = std::fs::symlink_metadata(&disk_path) else {
                    skipped_paths += 1;
                    continue;
                };
                if !metadata.is_file() {
                    skipped_paths += 1;
                    continue;
                }
                let Ok(contents) = std::fs::read(disk_path) else {
                    skipped_paths += 1;
                    continue;
                };
                if std::str::from_utf8(&contents).is_err() || contents.contains(&0) {
                    skipped_paths += 1;
                    continue;
                }
                let contents = contents.strip_suffix(b"\n").unwrap_or(&contents);
                if contents.is_empty() {
                    continue;
                }
                orphans.entry(path.to_vec()).or_default().extend(
                    contents.split(|byte| *byte == b'\n').map(|line| Orphan {
                        line: line.to_vec(),
                        side: Side::Added,
                    }),
                );
            }
        }
        // The untracked listing is what names those paths, so without it not
        // even the number of unread paths is knowable. Everything counted from
        // here on is a floor rather than a total, and the copy has to say so.
        _ => counts_exact = false,
    }

    // A status path absent from both inputs is commonly an assume-unchanged or
    // skip-worktree entry. Calling it clean would trust the flag that the status
    // pass explicitly distrusted.
    skipped_paths += dirty_paths
        .iter()
        .filter(|path| !seen.contains(path.as_slice()))
        .count();
    for lines in orphans.values_mut() {
        lines.sort_unstable();
    }

    let (leftovers, scan) = lines_absent_from(git, root, target_oid, &orphans, &BTreeMap::new());
    let paths_not_compared = skipped_paths + scan.paths_not_compared;
    let incomplete = !counts_exact || paths_not_compared > 0 || scan.limit_reached;
    UncommittedAnalysis::Compared {
        target: target_name.to_string(),
        leftover: leftovers.len(),
        leftover_sample: leftovers
            .iter()
            .take(5)
            .map(|line| String::from_utf8_lossy(&line.content).trim().to_string())
            .collect(),
        incomplete,
        shortfall: incomplete.then(|| ComparisonShortfall {
            lines_compared: scan.lines_compared,
            lines_not_compared: scan.lines_not_compared(),
            line_limit: scan.limit_reached.then_some(MAX_LEFTOVER_PROBES),
            paths_not_compared,
            counts_exact,
        }),
    }
}

struct DeltaComparison {
    summary: CandidateMatch,
    leftovers: Vec<Leftover>,
    unsupported_files: BTreeSet<Vec<u8>>,
}

fn matched_and_unmatched(
    branch: &[Vec<u8>],
    candidate: &[Vec<u8>],
) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let mut available = occurrence_counts(candidate.iter().map(Vec::as_slice));
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    for line in branch {
        if consume_occurrence(&mut available, line) {
            matched.push(line.clone());
        } else {
            unmatched.push(line.clone());
        }
    }
    (matched, unmatched)
}

/// How many lines may be probed before the answer is called incomplete.
///
/// One git process per line, so an unlanded branch with thousands of new lines
/// would otherwise stall the panel. Reaching the bound is reported rather than
/// rounded off to "nothing missing".
const MAX_LEFTOVER_PROBES: usize = 256;

/// Which orphan lines are absent from the target's own copy of their file.
///
/// Read and compared in process, one blob per path, rather than shelling out
/// per line. An earlier version used `git grep` and got it wrong twice over:
/// `-F` alone is a *substring* search, so `return user;` counted as present
/// inside `if (ready) return user;`, and `git grep` has no whole-line switch,
/// so asking for one made every probe fail — and a failed probe was read as
/// "present", which quietly reported every branch as having nothing left.
///
/// Scoped to the file the line came from. Code that moved elsewhere will be
/// reported as a leftover, which is the direction to be wrong in.
fn lines_absent_from(
    git: &Git,
    root: &Path,
    target: &str,
    orphans: &BTreeMap<Vec<u8>, Vec<Orphan>>,
    reservations: &BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
) -> (Vec<Leftover>, LineScan) {
    let mut absent = Vec::new();
    let mut scan = LineScan {
        lines_in_scope: orphans.values().map(Vec::len).sum(),
        ..LineScan::default()
    };
    let mut checked = 0;
    if orphans.is_empty() {
        return (absent, scan);
    }
    let Some(existing) = existing_target_paths(git, root, target, orphans.keys()) else {
        // The target's file list is what says which paths even exist there, so
        // losing it loses every path at once rather than one of them.
        scan.paths_not_compared = orphans.len();
        return (absent, scan);
    };

    for (path, lines) in orphans {
        if checked >= MAX_LEFTOVER_PROBES {
            scan.limit_reached = true;
            break;
        }

        let mut present = if existing.contains(path) {
            let Ok(path) = std::str::from_utf8(path) else {
                scan.paths_not_compared += 1;
                continue;
            };
            let spec = format!("{target}:{path}");
            match git.run_status(root, &["show", &spec]) {
                Ok(out) if out.code == Some(0) => blob_occurrence_counts(&out.stdout),
                // An unreadable blob proves nothing. In particular, an empty
                // stand-in would make every deletion look safely landed.
                _ => {
                    scan.paths_not_compared += 1;
                    continue;
                }
            }
        } else {
            HashMap::new()
        };

        // Candidate matches and the target snapshot commonly describe the same
        // occurrence. Reserving it prevents that one occurrence from proving a
        // second, duplicate branch line landed too.
        for line in reservations.get(path).into_iter().flatten() {
            consume_occurrence(&mut present, line);
        }

        for orphan in lines {
            if checked >= MAX_LEFTOVER_PROBES {
                scan.limit_reached = true;
                break;
            }
            checked += 1;
            let in_target = consume_occurrence(&mut present, &orphan.line);
            let unique = match orphan.side {
                Side::Added => !in_target,
                Side::Removed => in_target,
            };
            if unique {
                absent.push(Leftover {
                    path: path.clone(),
                    side: orphan.side,
                    content: orphan.line.clone(),
                });
            }
        }
        if scan.limit_reached {
            break;
        }
    }

    scan.lines_compared = checked;
    (absent, scan)
}

/// What one line-by-line pass managed to read, so the caller can say how much
/// it did not. Counted here rather than re-derived later, because only this
/// pass knows which paths it gave up on and where it stopped.
#[derive(Debug, Clone, Copy, Default)]
struct LineScan {
    /// Changed lines the pass was asked to account for.
    lines_in_scope: usize,
    /// Changed lines it actually probed against the target.
    lines_compared: usize,
    /// Paths it could not open on one side or the other.
    paths_not_compared: usize,
    /// Whether [`MAX_LEFTOVER_PROBES`] is what ended the walk.
    limit_reached: bool,
}

impl LineScan {
    fn incomplete(&self) -> bool {
        self.limit_reached || self.paths_not_compared > 0
    }

    fn lines_not_compared(&self) -> usize {
        self.lines_in_scope.saturating_sub(self.lines_compared)
    }
}

fn occurrence_counts<'a>(lines: impl IntoIterator<Item = &'a [u8]>) -> HashMap<Vec<u8>, usize> {
    let mut counts = HashMap::new();
    for line in lines {
        *counts.entry(line.to_vec()).or_default() += 1;
    }
    counts
}

fn blob_occurrence_counts(blob: &[u8]) -> HashMap<Vec<u8>, usize> {
    if blob.is_empty() {
        return HashMap::new();
    }
    let contents = blob.strip_suffix(b"\n").unwrap_or(blob);
    occurrence_counts(contents.split(|byte| *byte == b'\n'))
}

fn consume_occurrence(counts: &mut HashMap<Vec<u8>, usize>, line: &[u8]) -> bool {
    let Some(count) = counts.get_mut(line) else {
        return false;
    };
    if *count == 0 {
        return false;
    }
    *count -= 1;
    true
}

fn existing_target_paths<'a>(
    git: &Git,
    root: &Path,
    target: &str,
    paths: impl Iterator<Item = &'a Vec<u8>>,
) -> Option<HashSet<Vec<u8>>> {
    let mut args = vec![
        OsString::from("--literal-pathspecs"),
        OsString::from("ls-tree"),
        OsString::from("-z"),
        OsString::from("--full-name"),
        OsString::from(target),
        OsString::from("--"),
    ];
    for path in paths {
        args.push(OsString::from(std::str::from_utf8(path).ok()?));
    }
    let out = git.run_status(root, &args).ok()?;
    if out.code != Some(0) {
        return None;
    }
    let records = nul_records(&out.stdout)?;
    records
        .into_iter()
        .map(|record| {
            record
                .iter()
                .position(|byte| *byte == b'\t')
                .map(|separator| record[separator + 1..].to_vec())
        })
        .collect()
}

/// A line of the branch's change that the candidate did not make.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Orphan {
    line: Vec<u8>,
    side: Side,
}

struct Leftover {
    path: Vec<u8>,
    side: Side,
    content: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Side {
    Added,
    Removed,
}

fn path_is_contained_exactly(
    git: &Git,
    root: &Path,
    head: &str,
    target: &str,
    path: &[u8],
) -> bool {
    match (
        object_at_path(git, root, head, path),
        object_at_path(git, root, target, path),
    ) {
        (ObjectAtPath::Missing, ObjectAtPath::Missing) => true,
        (ObjectAtPath::Present(head), ObjectAtPath::Present(target)) => head == target,
        _ => false,
    }
}

#[derive(PartialEq, Eq)]
struct TreeEntry {
    mode: Vec<u8>,
    kind: Vec<u8>,
    oid: Vec<u8>,
}

enum ObjectAtPath {
    Present(TreeEntry),
    Missing,
    Unknown,
}

fn object_at_path(git: &Git, root: &Path, revision: &str, path: &[u8]) -> ObjectAtPath {
    let path = String::from_utf8_lossy(path);
    let Ok(out) = git.run_status(root, &["ls-tree", "-z", revision, "--", path.as_ref()]) else {
        return ObjectAtPath::Unknown;
    };
    if out.code != Some(0) {
        return ObjectAtPath::Unknown;
    }
    if out.stdout.is_empty() {
        return ObjectAtPath::Missing;
    }
    if !out.stdout.ends_with(b"\0") {
        return ObjectAtPath::Unknown;
    }
    let Some(header) = out.stdout.split(|byte| *byte == b'\t').next() else {
        return ObjectAtPath::Unknown;
    };
    let mut fields = header.split(|byte| byte.is_ascii_whitespace());
    let (Some(mode), Some(kind), Some(oid), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return ObjectAtPath::Unknown;
    };
    if !is_hex(oid) {
        return ObjectAtPath::Unknown;
    }
    ObjectAtPath::Present(TreeEntry {
        mode: mode.to_vec(),
        kind: kind.to_vec(),
        oid: oid.to_vec(),
    })
}

#[derive(Default)]
struct PathChange {
    added: Vec<Vec<u8>>,
    removed: Vec<Vec<u8>>,
    binary: bool,
    old_object: Option<Vec<u8>>,
    new_object: Option<Vec<u8>>,
}

impl PartialEq for PathChange {
    fn eq(&self, other: &Self) -> bool {
        self.added == other.added
            && self.removed == other.removed
            && self.binary == other.binary
            && (!self.binary
                || (self.old_object == other.old_object && self.new_object == other.new_object))
    }
}

impl Eq for PathChange {}

/// A patch, reduced to what each path gained and lost.
fn delta_of(git: &Git, root: &Path, args: &[&str]) -> Option<BTreeMap<Vec<u8>, PathChange>> {
    let out = git.run_status(root, args).ok()?;
    if out.code != Some(0) {
        return None;
    }

    if out.stdout.is_empty() {
        return Some(BTreeMap::new());
    }
    let separator = out
        .stdout
        .windows(b"\0\0diff --git ".len())
        .position(|window| window == b"\0\0diff --git ")?;
    let records = nul_records(&out.stdout[..=separator])?;
    if records.len() % 2 != 0 {
        return None;
    }
    let mut paths = records
        .chunks_exact(2)
        .map(|record| record[0].starts_with(b":").then(|| record[1].to_vec()));
    let patch = &out.stdout[separator + 2..];
    let mut changes: BTreeMap<Vec<u8>, PathChange> = BTreeMap::new();
    let mut current: Option<Vec<u8>> = None;

    for line in patch.split(|b| *b == b'\n') {
        if line.starts_with(b"diff --git ") {
            current = Some(paths.next()??);
            if let Some(path) = &current {
                changes.entry(path.clone()).or_default();
            }
            continue;
        }
        let Some(path) = &current else { continue };
        if line == b"GIT binary patch" || line.starts_with(b"Binary files ") {
            changes.entry(path.clone()).or_default().binary = true;
            continue;
        }
        if let Some(index) = line.strip_prefix(b"index ")
            && let Some((old, new)) = object_pair(index)
        {
            let change = changes.entry(path.clone()).or_default();
            change.old_object = Some(old.to_vec());
            change.new_object = Some(new.to_vec());
            continue;
        }
        // `+++`/`---` are file headers, not content.
        if line.starts_with(b"+++") || line.starts_with(b"---") {
            continue;
        }
        if let Some(added) = line.strip_prefix(b"+") {
            changes
                .entry(path.clone())
                .or_default()
                .added
                .push(added.to_vec());
        } else if let Some(removed) = line.strip_prefix(b"-") {
            changes
                .entry(path.clone())
                .or_default()
                .removed
                .push(removed.to_vec());
        }
    }

    if paths.next().is_some() {
        return None;
    }
    Some(changes)
}

fn object_pair(index: &[u8]) -> Option<(&[u8], &[u8])> {
    let pair = index.split(|byte| byte.is_ascii_whitespace()).next()?;
    let separator = pair.windows(2).position(|window| window == b"..")?;
    let old = &pair[..separator];
    let new = &pair[separator + 2..];
    (is_hex(old) && is_hex(new)).then_some((old, new))
}

/// Patch rendering still needs a fallback for binary files, whose headers have
/// no `---`/`+++` path. Safety classification never relies on this ambiguous
/// text; `delta_of` takes its paths from the raw NUL-delimited records.
fn b_side_path(rest: &[u8]) -> Option<Vec<u8>> {
    if let Some(start) = rest.windows(4).rposition(|window| window == b" \"b/") {
        let decoded = decode_git_path(&rest[start + 1..]);
        return decoded.strip_prefix(b"b/").map(<[u8]>::to_vec);
    }
    let text = String::from_utf8_lossy(rest);
    let b = text.rfind(" b/")?;
    Some(text[b + 3..].trim_end().as_bytes().to_vec())
}

/// Build the readable branch hunks containing lines absent from one immutable
/// target snapshot.
///
/// Refs are resolved before either comparison starts. A moving default branch
/// can therefore make this result old, but it cannot make its count describe a
/// different snapshot from its patch.
pub fn unique_patch(
    git: &Git,
    root: &Path,
    head: &str,
    target: &str,
    candidate: &str,
    max_bytes: usize,
) -> Option<UniquePatch> {
    let head = resolve_commit(git, root, head)?;
    let target_oid = resolve_commit(git, root, target)?;
    let candidate = resolve_commit(git, root, candidate)?;
    let base = merge_base(git, root, &target_oid, &head)?;
    let comparison = compare_deltas(git, root, &base, &head, &target_oid, target, &candidate)?;
    unique_patch_resolved(
        git,
        root,
        &CandidateSnapshot {
            base: &base,
            head: &head,
            target,
            candidate: &candidate,
        },
        comparison,
        max_bytes,
    )
}

/// Choose the safest initial diff view for a worktree.
///
/// The historical comparison and merge-tree patch are deliberately absent from
/// scans. Interactive callers may request them after selection, but never while
/// building the worktree list.
pub fn focused_patch(
    git: &Git,
    root: &Path,
    head: &str,
    targets: &[String],
    cache: LandingCache,
    max_bytes: usize,
) -> FocusedPatch {
    let context = LandingContext::new(git, root, targets, cache);
    context.focused_patch(git, root, head, max_bytes)
}

fn focused_patch_with_context(
    git: &Git,
    root: &Path,
    head: String,
    context: &LandingContext,
    max_bytes: usize,
) -> FocusedPatch {
    let key = FocusedPatchKey {
        root: root.to_path_buf(),
        head: head.clone(),
        targets: context
            .targets
            .iter()
            .map(|target| (target.name.clone(), target.oid.clone()))
            .collect(),
        fingerprint: context.config.fingerprint.clone(),
        max_bytes,
    };
    if let Some(focus) = lock_cache(&context.cache)
        .focused_patches
        .get(&key)
        .cloned()
    {
        return focus;
    }
    let analysis = context.analyse(git, root, Some(&head), LandingDepth::History);

    let focus = match analysis.focus {
        Some(LandingFocus::Candidate(focus)) => {
            let CandidateFocus {
                target,
                base,
                head,
                candidate,
                comparison,
            } = *focus;
            if comparison.summary.leftover == 0 {
                FocusedPatch::All {
                    reason: if comparison.summary.incomplete {
                        AllChangesReason::Incomplete
                    } else {
                        AllChangesReason::NoFilteredChanges
                    },
                }
            } else {
                match unique_patch_resolved(
                    git,
                    root,
                    &CandidateSnapshot {
                        base: &base,
                        head: &head,
                        target: &target.name,
                        candidate: &candidate,
                    },
                    comparison,
                    max_bytes,
                ) {
                    Some(patch) => FocusedPatch::Unmatched { patch },
                    None => FocusedPatch::All {
                        reason: AllChangesReason::Unsafe,
                    },
                }
            }
        }
        Some(LandingFocus::AddsContent { target, tree }) => {
            match target.oid.and_then(|target_oid| {
                merge_patch_resolved(git, root, &target.name, &target_oid, &tree, max_bytes)
            }) {
                Some(patch) => FocusedPatch::WouldChange { patch },
                None => FocusedPatch::All {
                    reason: AllChangesReason::Unsafe,
                },
            }
        }
        None => {
            let reason = if matches!(analysis.landing, Landing::Landed { .. }) {
                AllChangesReason::NoFilteredChanges
            } else {
                AllChangesReason::Unsafe
            };
            FocusedPatch::All { reason }
        }
    };
    lock_cache(&context.cache)
        .focused_patches
        .insert(key, focus.clone());
    focus
}

struct CandidateSnapshot<'a> {
    base: &'a str,
    head: &'a str,
    target: &'a str,
    candidate: &'a str,
}

fn unique_patch_resolved(
    git: &Git,
    root: &Path,
    snapshot: &CandidateSnapshot<'_>,
    comparison: DeltaComparison,
    max_bytes: usize,
) -> Option<UniquePatch> {
    let raw = git
        .run(
            root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--unified=3",
                snapshot.base,
                snapshot.head,
                "--",
            ],
        )
        .ok()?;
    Some(filter_unique_patch(
        &raw,
        snapshot.target,
        snapshot.candidate,
        comparison,
        max_bytes,
    ))
}

fn merge_patch_resolved(
    git: &Git,
    root: &Path,
    target: &str,
    target_oid: &str,
    merge_tree: &str,
    max_bytes: usize,
) -> Option<MergePatch> {
    let raw = git
        .run(
            root,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--find-renames",
                "--unified=3",
                target_oid,
                merge_tree,
                "--",
            ],
        )
        .ok()?;
    Some(cap_merge_patch(&raw, target, max_bytes))
}

#[derive(Hash, PartialEq, Eq)]
struct LeftoverKey {
    path: Vec<u8>,
    side: Side,
    content: Vec<u8>,
}

struct ParsedPatchFile<'a> {
    path: Vec<u8>,
    raw: &'a [u8],
    header: &'a [u8],
    hunks: Vec<&'a [u8]>,
}

struct SelectedHunk<'a> {
    raw: &'a [u8],
    markers: Vec<UniqueLineMarker>,
}

struct SelectedFile<'a> {
    header: &'a [u8],
    whole: Option<&'a [u8]>,
    hunks: Vec<SelectedHunk<'a>>,
}

fn filter_unique_patch(
    raw: &[u8],
    target: &str,
    candidate: &str,
    comparison: DeltaComparison,
    max_bytes: usize,
) -> UniquePatch {
    let line_count = comparison.leftovers.len();
    let mut paths = comparison
        .leftovers
        .iter()
        .map(|line| line.path.clone())
        .collect::<BTreeSet<_>>();
    paths.extend(comparison.unsupported_files.iter().cloned());
    let file_count = paths.len();
    let mut unmatched = HashMap::<LeftoverKey, usize>::new();
    for line in &comparison.leftovers {
        *unmatched
            .entry(LeftoverKey {
                path: line.path.clone(),
                side: line.side,
                content: line.content.clone(),
            })
            .or_default() += 1;
    }

    let files = parse_patch_files(raw);
    let mut selected = Vec::new();
    let mut matched = 0;
    for file in &files {
        if file.hunks.is_empty() {
            if comparison.unsupported_files.contains(&file.path) {
                selected.push(SelectedFile {
                    header: &[],
                    whole: Some(file.raw),
                    hunks: Vec::new(),
                });
            }
            continue;
        }

        let mut hunks = Vec::new();
        for hunk in &file.hunks {
            let markers = markers_in_hunk(&file.path, hunk, &mut unmatched);
            matched += markers.len();
            if !markers.is_empty() {
                hunks.push(SelectedHunk { raw: hunk, markers });
            }
        }
        if !hunks.is_empty() {
            selected.push(SelectedFile {
                header: file.header,
                whole: None,
                hunks,
            });
        }
    }

    let (patch, markers, truncated) = emit_selected_files(&selected, max_bytes);
    UniquePatch {
        patch,
        line_count,
        file_count,
        candidate: candidate.to_string(),
        target: target.to_string(),
        markers,
        incomplete: comparison.summary.incomplete || matched < line_count,
        truncated,
    }
}

fn cap_merge_patch(raw: &[u8], target: &str, max_bytes: usize) -> MergePatch {
    let files = parse_patch_files(raw);
    let line_count = files
        .iter()
        .flat_map(|file| &file.hunks)
        .map(|hunk| changed_lines_in_hunk(hunk))
        .sum();
    let selected = files
        .iter()
        .map(|file| {
            if file.hunks.is_empty() {
                SelectedFile {
                    header: &[],
                    whole: Some(file.raw),
                    hunks: Vec::new(),
                }
            } else {
                SelectedFile {
                    header: file.header,
                    whole: None,
                    hunks: file
                        .hunks
                        .iter()
                        .map(|hunk| SelectedHunk {
                            raw: hunk,
                            markers: Vec::new(),
                        })
                        .collect(),
                }
            }
        })
        .collect::<Vec<_>>();
    let (patch, _, truncated) = emit_selected_files(&selected, max_bytes);
    MergePatch {
        patch,
        line_count,
        file_count: files.len(),
        target: target.to_string(),
        truncated,
    }
}

fn emit_selected_files(
    files: &[SelectedFile<'_>],
    max_bytes: usize,
) -> (String, Vec<UniqueLineMarker>, bool) {
    let mut output = Vec::new();
    let mut markers = Vec::new();
    let mut truncated = false;

    'files: for file in files {
        if let Some(whole) = file.whole {
            if output.len().saturating_add(whole.len()) > max_bytes {
                truncated = true;
                break;
            }
            output.extend_from_slice(whole);
            continue;
        }

        let mut emitted_header = false;
        for hunk in &file.hunks {
            let header_len = if emitted_header { 0 } else { file.header.len() };
            let next_len = output
                .len()
                .saturating_add(header_len)
                .saturating_add(hunk.raw.len());
            if next_len > max_bytes {
                truncated = true;
                break 'files;
            }
            if !emitted_header {
                output.extend_from_slice(file.header);
                emitted_header = true;
            }
            output.extend_from_slice(hunk.raw);
            markers.extend(hunk.markers.iter().cloned());
        }
    }

    (
        String::from_utf8_lossy(&output).into_owned(),
        markers,
        truncated,
    )
}

fn parse_patch_files(raw: &[u8]) -> Vec<ParsedPatchFile<'_>> {
    let lines = line_ranges(raw);
    let starts = lines
        .iter()
        .filter_map(|(start, end)| {
            raw[*start..*end]
                .starts_with(b"diff --git ")
                .then_some(*start)
        })
        .collect::<Vec<_>>();
    let mut files = Vec::new();

    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(raw.len());
        let file_raw = &raw[start..end];
        let file_lines = line_ranges(file_raw);
        let hunk_starts = file_lines
            .iter()
            .filter_map(|(line_start, line_end)| {
                file_raw[*line_start..*line_end]
                    .starts_with(b"@@ ")
                    .then_some(*line_start)
            })
            .collect::<Vec<_>>();
        let header_end = hunk_starts.first().copied().unwrap_or(file_raw.len());
        let header = &file_raw[..header_end];
        let path = patch_file_path(header).unwrap_or_default();
        let hunks = hunk_starts
            .iter()
            .copied()
            .enumerate()
            .map(|(hunk_index, hunk_start)| {
                let hunk_end = hunk_starts
                    .get(hunk_index + 1)
                    .copied()
                    .unwrap_or(file_raw.len());
                &file_raw[hunk_start..hunk_end]
            })
            .collect();
        files.push(ParsedPatchFile {
            path,
            raw: file_raw,
            header,
            hunks,
        });
    }

    files
}

fn line_ranges(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            ranges.push((start, index + 1));
            start = index + 1;
        }
    }
    if start < bytes.len() {
        ranges.push((start, bytes.len()));
    }
    ranges
}

fn patch_file_path(header: &[u8]) -> Option<Vec<u8>> {
    let lines = line_ranges(header);
    let mut fallback = None;
    let mut old = None;
    let mut new = None;

    for (start, end) in lines {
        let line = trim_line_ending(&header[start..end]);
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            fallback = b_side_path(rest);
        } else if let Some(rest) = line.strip_prefix(b"--- ") {
            old = header_path(rest, b"a/");
        } else if let Some(rest) = line.strip_prefix(b"+++ ") {
            new = header_path(rest, b"b/");
        }
    }
    new.or(old).or(fallback)
}

fn header_path(raw: &[u8], prefix: &[u8]) -> Option<Vec<u8>> {
    let raw = raw.split(|byte| *byte == b'\t').next().unwrap_or(raw);
    if raw == b"/dev/null" {
        return None;
    }
    let decoded = decode_git_path(raw);
    decoded.strip_prefix(prefix).map(<[u8]>::to_vec)
}

fn decode_git_path(raw: &[u8]) -> Vec<u8> {
    if raw.len() < 2 || raw.first() != Some(&b'"') || raw.last() != Some(&b'"') {
        return raw.to_vec();
    }
    let mut decoded = Vec::new();
    let mut index = 1;
    while index + 1 < raw.len() {
        if raw[index] != b'\\' {
            decoded.push(raw[index]);
            index += 1;
            continue;
        }
        index += 1;
        if index + 1 >= raw.len() {
            break;
        }
        match raw[index] {
            b'a' => decoded.push(7),
            b'b' => decoded.push(8),
            b't' => decoded.push(b'\t'),
            b'n' => decoded.push(b'\n'),
            b'v' => decoded.push(11),
            b'f' => decoded.push(12),
            b'r' => decoded.push(b'\r'),
            b'\\' => decoded.push(b'\\'),
            b'"' => decoded.push(b'"'),
            digit @ b'0'..=b'7' => {
                let mut value = digit - b'0';
                let mut digits = 1;
                while digits < 3
                    && index + 1 < raw.len() - 1
                    && matches!(raw[index + 1], b'0'..=b'7')
                {
                    index += 1;
                    value = value.saturating_mul(8).saturating_add(raw[index] - b'0');
                    digits += 1;
                }
                decoded.push(value);
            }
            other => decoded.push(other),
        }
        index += 1;
    }
    decoded
}

fn markers_in_hunk(
    path: &[u8],
    hunk: &[u8],
    unmatched: &mut HashMap<LeftoverKey, usize>,
) -> Vec<UniqueLineMarker> {
    let ranges = line_ranges(hunk);
    let Some((header_start, header_end)) = ranges.first().copied() else {
        return Vec::new();
    };
    let Some((mut old_line, mut new_line)) =
        hunk_line_starts(trim_line_ending(&hunk[header_start..header_end]))
    else {
        return Vec::new();
    };
    let mut markers = Vec::new();

    for (start, end) in ranges.into_iter().skip(1) {
        let line = trim_line_ending(&hunk[start..end]);
        let Some(prefix) = line.first() else { continue };
        match prefix {
            b'-' => {
                if consume_leftover(unmatched, path, Side::Removed, &line[1..]) {
                    markers.push(UniqueLineMarker {
                        path: String::from_utf8_lossy(path).into_owned(),
                        side: UniqueLineSide::Deletions,
                        line_number: old_line,
                    });
                }
                old_line += 1;
            }
            b'+' => {
                if consume_leftover(unmatched, path, Side::Added, &line[1..]) {
                    markers.push(UniqueLineMarker {
                        path: String::from_utf8_lossy(path).into_owned(),
                        side: UniqueLineSide::Additions,
                        line_number: new_line,
                    });
                }
                new_line += 1;
            }
            b' ' => {
                old_line += 1;
                new_line += 1;
            }
            _ => {}
        }
    }
    markers
}

fn consume_leftover(
    unmatched: &mut HashMap<LeftoverKey, usize>,
    path: &[u8],
    side: Side,
    content: &[u8],
) -> bool {
    let key = LeftoverKey {
        path: path.to_vec(),
        side,
        content: content.to_vec(),
    };
    let Some(remaining) = unmatched.get_mut(&key) else {
        return false;
    };
    if *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    true
}

fn changed_lines_in_hunk(hunk: &[u8]) -> usize {
    line_ranges(hunk)
        .into_iter()
        .skip(1)
        .filter(|(start, _)| matches!(hunk.get(*start), Some(b'+' | b'-')))
        .count()
}

fn hunk_line_starts(header: &[u8]) -> Option<(usize, usize)> {
    let mut fields = header.split(|byte| byte.is_ascii_whitespace());
    if fields.next()? != b"@@" {
        return None;
    }
    let old = fields.next()?.strip_prefix(b"-")?;
    let new = fields.next()?.strip_prefix(b"+")?;
    Some((range_start(old)?, range_start(new)?))
}

fn range_start(range: &[u8]) -> Option<usize> {
    let start = range.split(|byte| *byte == b',').next()?;
    std::str::from_utf8(start).ok()?.parse().ok()
}

fn trim_line_ending(mut line: &[u8]) -> &[u8] {
    if let Some(without_newline) = line.strip_suffix(b"\n") {
        line = without_newline;
    }
    line.strip_suffix(b"\r").unwrap_or(line)
}
