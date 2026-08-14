//! Domain types shared by the GUI, the CLI, and the tests.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Why a worktree is locked. Git allows a lock with no stated reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockInfo {
    pub reason: Option<String>,
}

/// A worktree exactly as `git worktree list --porcelain` describes it, before
/// any additional signals are gathered.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeEntry {
    pub path: PathBuf,
    /// Main worktree of the repository that produced this entry.
    ///
    /// Internal identity evidence; omitted from display/serialized APIs.
    #[serde(skip)]
    pub repository: Option<PathBuf>,
    /// Commit SHA. Absent for a bare repository entry.
    pub head: Option<String>,
    /// Short branch name, e.g. `feat/auth`. Absent when detached or bare.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    /// The first entry git reports is the main worktree; it can never be removed.
    pub is_main: bool,
    pub locked: Option<LockInfo>,
    /// Set when the directory is gone and only stale metadata remains.
    pub prunable: Option<String>,
}

/// A dependency directory linked by yawm into a worktree.
///
/// Only links whose administrative record, destination, main-worktree target,
/// and dependency manifests still agree are reported here. A stale record is
/// treated as ordinary untracked work instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedDependencyLink {
    pub path: String,
    pub target: PathBuf,
}

/// Counts from `git status --porcelain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirtyCounts {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    /// Distinct dirty paths, counted by Git's lossless path bytes.
    #[serde(default)]
    pub paths: usize,
    /// A failed inspection is not evidence of a clean worktree. Keeping that
    /// uncertainty separate prevents callers from silently treating zero
    /// observed changes as proof that no changes exist.
    #[serde(default)]
    pub inspection_failed: bool,
}

impl DirtyCounts {
    /// True when the worktree holds changes that a delete would destroy.
    pub fn is_dirty(&self) -> bool {
        self.staged > 0 || self.unstaged > 0 || self.untracked > 0
    }

    /// Sum of status dimensions, not distinct paths.
    ///
    /// Kept for deletion-plan compatibility; callers displaying file counts
    /// should use `paths`.
    pub fn total(&self) -> usize {
        self.staged + self.unstaged + self.untracked
    }

    pub fn is_unknown(&self) -> bool {
        self.inspection_failed
    }
}

/// Relationship between the worktree's branch and its upstream.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamInfo {
    /// Upstream ref name, e.g. `origin/feat/auth`.
    pub name: Option<String>,
    /// The upstream's full ref name, e.g. `refs/remotes/origin/feat/auth`.
    ///
    /// The short name is what a person reads; only the full one names a ref a
    /// ref transaction can verify, and a fetch refspec may put a tracking ref
    /// in any namespace — `refs/pr/42` is an ordinary configuration.
    #[serde(default)]
    pub full_ref: Option<String>,
    /// The commit that upstream ref points at.
    ///
    /// Absent on older serialized data, which is why it defaults rather than
    /// being required: a missing value is "not known", never "unchanged".
    #[serde(default)]
    pub oid: Option<String>,
    /// The upstream exists but its commit could not be established.
    ///
    /// Separate from a missing `oid`, which also covers "there is no upstream".
    /// This one means there is one and yawm does not know where it points, so
    /// no comparison against it proves anything.
    #[serde(default)]
    pub unresolved: bool,
    pub ahead: usize,
    pub behind: usize,
    /// The upstream branch existed but has since been deleted on the remote.
    pub gone: bool,
}

/// The ref `git branch -d` would decide against, pinned to a commit.
///
/// git answers "is this branch merged?" against the configured upstream when
/// there is one that still exists, and against the current HEAD otherwise. Both
/// halves are recorded: the name, because a deletion has to verify that exact
/// ref is still where the answer was read from, and the commit, because an
/// object name is immutable and a ref is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeReference {
    /// Full ref name, e.g. `refs/remotes/origin/main`, `refs/heads/main`, or
    /// `refs/pr/42` — whatever namespace the configuration puts it in.
    pub name: String,
    /// The commit it pointed at when this state was read.
    pub oid: String,
}

/// A directory large enough to be worth reporting separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeavyDir {
    pub name: String,
    pub bytes: u64,
    /// Linked rather than copied, so removing this worktree reclaims nothing.
    pub is_link: bool,
}

/// Result of walking a worktree's directory tree.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeInfo {
    pub bytes: u64,
    pub files: u64,
    pub heavy_dirs: Vec<HeavyDir>,
    /// Newest modification time seen, as a Unix timestamp. Powers "last active".
    pub last_modified: Option<i64>,
}

/// A process whose working directory is inside a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
}

/// A proof that the committed tree effect reached a default-branch snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "commit")]
pub enum LandingProof {
    Ancestry,
    SameTree,
    NoOpAtTip,
    NoOpAtAncestor(String),
    PatchEquivalent(String),
}

/// The proof phase that produced an unknown answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProofPhase {
    #[default]
    NotStarted,
    TargetSelection,
    HeadResolution,
    Ancestry,
    TreeComparison,
    MergeConfiguration,
    MergeTree,
    History,
    CandidateComparison,
}

/// Why containment could not be proved.
///
/// These reasons are diagnostic only. None is evidence that the branch did not
/// land, so callers must preserve the `Unknown` verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum UnknownReason {
    NotChecked,
    NoDefaultBranch,
    HeadUnavailable,
    TargetUnavailable,
    GitCommandFailed { phase: ProofPhase },
    MergeTreeUnavailable,
    MalformedMergeTree,
    CheckDeferred,
    OverlappingChanges { paths: usize },
    HistoryRangeTooLarge { commits: usize, limit: usize },
    CustomMergeDriver,
    MergeAttributes,
    NoMergeBase,
}

/// The selected default ref and the immutable commit inspected through it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingTargetFact {
    pub name: String,
    pub oid: Option<String>,
    pub short_oid: Option<String>,
}

/// The checked-out commit's exact topology relative to the selected target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum HeadState {
    #[default]
    Unavailable,
    Branch {
        name: String,
        oid: String,
    },
    Detached {
        oid: String,
    },
    Unborn {
        branch: Option<String>,
    },
    Orphan {
        branch: String,
        oid: String,
    },
    NoMergeBase {
        oid: String,
    },
}

/// The configured upstream without collapsing absence, deletion, or failure.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum UpstreamState {
    #[default]
    None,
    Gone {
        name: String,
        full_ref: Option<String>,
    },
    Unresolved {
        name: String,
        full_ref: Option<String>,
    },
    Existing {
        name: String,
        full_ref: String,
        oid: String,
        ahead: usize,
        behind: usize,
    },
}

/// Factual inputs behind a negative or unknown landing verdict.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingFacts {
    pub selected_target: Option<LandingTargetFact>,
    pub candidate: Option<LandingTargetFact>,
    pub commits_ahead: Option<usize>,
    pub head: HeadState,
    pub upstream: UpstreamState,
    pub unknown_reason: Option<UnknownReason>,
    pub proof_phase: Option<ProofPhase>,
}

/// Whether the branch's committed tree effect is contained in a reachable
/// default-branch snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum Landing {
    Landed {
        target: String,
        proof: LandingProof,
    },
    AddsContent {
        target: String,
    },
    Unknown {
        reason: UnknownReason,
        candidate: Option<CandidateMatch>,
    },
}

/// Exactly what a comparison failed to look at, in numbers it can prove.
///
/// `incomplete` on its own tells the reader that something was missed and then
/// refuses to say how much, which reads as an admission of ignorance rather
/// than a measurement. These are the figures the comparison already holds while
/// it runs, so carrying them costs nothing and lets the copy name a threshold
/// and a shortfall instead of hedging with "some".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonShortfall {
    /// Changed lines actually probed against the target.
    pub lines_compared: usize,
    /// Changed lines the comparison never looked at.
    pub lines_not_compared: usize,
    /// The per-comparison line budget, set only when reaching it is what
    /// stopped the walk. `None` means something else did, and quoting a
    /// threshold that was never hit would misattribute the cause.
    pub line_limit: Option<usize>,
    /// Paths whose contents could not be compared line by line at all —
    /// binary, undecodable, or unreadable on either side. Counted as paths
    /// because their line counts are precisely what could not be established.
    pub paths_not_compared: usize,
    /// False when a listing itself failed, so the counts above are lower
    /// bounds rather than totals and the copy must say so.
    pub counts_exact: bool,
}

impl ComparisonShortfall {
    /// The total the comparison set out to read, when that total is known.
    pub fn lines_in_scope(&self) -> usize {
        self.lines_compared + self.lines_not_compared
    }

    /// Whether anything at all went unread. A shortfall with nothing in it is
    /// a complete comparison and must not be reported as a limitation.
    pub fn is_empty(&self) -> bool {
        self.lines_not_compared == 0 && self.paths_not_compared == 0 && self.counts_exact
    }
}

/// Kept apart from committed landing because mutable bytes have no object ID
/// suitable for a safe cache key. An old positive answer would be more dangerous
/// than paying for the comparison again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum UncommittedAnalysis {
    #[default]
    NotChecked,
    /// `rename_all` on the enum renames the variants, not the fields inside
    /// them, so this has to say so itself or the frontend reads `undefined`
    /// off every field with more than one word in its name.
    #[serde(rename_all = "camelCase")]
    Compared {
        target: String,
        leftover: usize,
        leftover_sample: Vec<String>,
        incomplete: bool,
        /// Present exactly when `incomplete`, so the reader can be told the
        /// size of the gap instead of merely that one exists.
        #[serde(default)]
        shortfall: Option<ComparisonShortfall>,
    },
}

impl UncommittedAnalysis {
    pub fn is_complete(&self) -> bool {
        matches!(
            self,
            Self::Compared {
                incomplete: false,
                ..
            }
        )
    }
}

/// A commit on the default branch that looks like this branch's work.
///
/// Reported as measurement, never as a conclusion. The counts come from
/// comparing the two *changes* — what the branch did to the merge base against
/// what the candidate did to its own parent — path by path and line by line.
/// A near-perfect match is powerful evidence and still not proof, because two
/// commits can make the same edits and mean different things.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateMatch {
    pub commit: String,
    /// The default branch this was compared against, so the reader can name it.
    pub target: String,
    /// Files the branch changed.
    pub paths: usize,
    /// Files the candidate changed in exactly the same way.
    pub matching_paths: usize,
    /// Lines the branch added.
    pub added: usize,
    /// Lines the branch added that the candidate also added.
    pub matching_added: usize,
    /// Lines of the branch's change absent from the default branch's copy of
    /// the same files.
    ///
    /// This is the number that answers "would I lose anything". On a real
    /// branch whose feature had landed, it was one line — a build number. On
    /// its neighbour it was eight, and they were a book-cover feature the
    /// default branch never took.
    pub leftover: usize,
    /// A few of those lines, so the reader can judge rather than trust.
    pub leftover_sample: Vec<String>,
    /// The search stopped before examining everything.
    ///
    /// Without this, a branch too large to check would report zero leftovers
    /// and read exactly like one with nothing left — the difference between
    /// "nothing is missing" and "nothing was looked at".
    pub incomplete: bool,
}

impl CandidateMatch {
    /// Whether anything of this branch is missing from the default branch.
    ///
    /// Leftovers are what matter, not percentages: a branch can match on 48%
    /// of its paths purely because the default branch improved them further,
    /// and still have nothing of its own left.
    pub fn has_leftovers(&self) -> bool {
        self.leftover > 0
    }
}

impl Default for Landing {
    fn default() -> Self {
        Self::Unknown {
            reason: UnknownReason::NotChecked,
            candidate: None,
        }
    }
}

/// Everything yawm learned about a worktree beyond its git identity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStatus {
    pub dirty: DirtyCounts,
    #[serde(default)]
    pub uncommitted: UncommittedAnalysis,
    pub upstream: UpstreamInfo,
    /// The commit the checked-out branch's ref points at.
    ///
    /// [`WorktreeEntry::head`] is what the worktree has checked out; this is
    /// what the branch name resolves to. They agree for an attached worktree
    /// and are both recorded, because a removal that also deletes the branch
    /// acts on the ref rather than on the checkout.
    #[serde(default)]
    pub branch_oid: Option<String>,
    /// The ref a branch deletion would be decided against, and the commit it
    /// pointed at when this status was read.
    ///
    /// Repository-wide facts are needed to work it out — the upstream's full
    /// name, or the ref HEAD resolves to in the main worktree — so it is
    /// gathered here, with everything else that comes from the shared context,
    /// rather than re-derived at the moment of deletion.
    #[serde(default)]
    pub merge_ref: Option<MergeReference>,
    pub landing: Landing,
    /// Exact target, topology, upstream, and proof-phase facts used by the
    /// verdict reason. Kept separate from the compact landing result so a UI
    /// never has to reconstruct Git state.
    #[serde(default)]
    pub landing_facts: LandingFacts,
    /// Kept separate from `Unknown`: overlapping changes remain unknown even
    /// after history is exhausted, while a shallower overlap can still resolve.
    #[serde(default)]
    pub landing_complete: bool,
    pub last_commit_at: Option<i64>,
    pub last_commit_subject: Option<String>,
    /// Untracked environment files with no byte-identical copy in the main
    /// worktree, relative to this worktree's root. Deleting this worktree may
    /// destroy their only current contents.
    pub env_files: Vec<String>,
    /// Untracked local environment files in the non-deletable main worktree.
    ///
    /// These are not duplicates at risk and therefore never appear in
    /// `env_files`.
    #[serde(default)]
    pub main_worktree_env_files: Vec<String>,
    /// Dependency links whose exact yawm record and compatibility checks still
    /// hold. They are excluded from destructive uncommitted-work counts.
    #[serde(default)]
    pub managed_dependency_links: Vec<ManagedDependencyLink>,
    pub size: Option<SizeInfo>,
    pub processes: Vec<ProcessInfo>,
    /// Whether the process table was inspected for this result.
    ///
    /// An empty process list is evidence only when this is true. Missing fields
    /// from older serialized data default to false so compatibility remains
    /// conservative rather than manufacturing certainty.
    #[serde(default)]
    pub process_check_complete: bool,
}

/// How safe a worktree is to delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// The directory is gone; only stale git metadata remains.
    Broken,
    /// Holds work, or is in use right now.
    Keep,
    /// Work has landed and nothing would be lost.
    Disposable,
    /// Containment could not be proved — a human should decide.
    Review,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Broken => "broken",
            Verdict::Keep => "keep",
            Verdict::Disposable => "disposable",
            Verdict::Review => "review",
        }
    }
}

/// The single reason a verdict was reached, so the UI can explain itself
/// instead of showing an unexplained colour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum VerdictReason {
    DirectoryMissing {
        detail: Option<String>,
        lock: Option<LockInfo>,
    },
    Locked,
    MainWorktree,
    ProcessRunning,
    ProcessCheckSkipped,
    RecentlyActive,
    UncommittedChanges,
    UncommittedChangesAtRisk {
        count: usize,
        target: String,
        incomplete: bool,
        /// Present exactly when `incomplete`. Carried on the reason itself so
        /// the detail copy can quote the threshold that stopped the comparison
        /// and the amount left unread, rather than saying "some".
        #[serde(default)]
        shortfall: Option<ComparisonShortfall>,
    },
    UncommittedChangesOnDefault {
        target: String,
    },
    EnvironmentFilesAtRisk {
        count: usize,
    },
    WorkingTreeUnreadable,
    UnpushedCommits,
    WorkContained {
        target: String,
        proof: LandingProof,
    },
    DefaultBranchLacksCommittedContent {
        facts: Box<LandingFacts>,
    },
    LandingUnknown {
        facts: Box<LandingFacts>,
    },
}

impl VerdictReason {
    /// Short human-readable explanation, shared by the GUI and the CLI.
    pub fn describe(&self) -> String {
        match self {
            VerdictReason::DirectoryMissing { .. } => {
                "Directory is missing; metadata is stale".to_string()
            }
            VerdictReason::Locked => "Locked".to_string(),
            VerdictReason::MainWorktree => "Main worktree".to_string(),
            VerdictReason::ProcessRunning => "A process is running here".to_string(),
            VerdictReason::ProcessCheckSkipped => "Live process inspection was skipped".to_string(),
            VerdictReason::RecentlyActive => "Files changed recently".to_string(),
            VerdictReason::UncommittedChanges => "Has uncommitted changes".to_string(),
            VerdictReason::UncommittedChangesAtRisk {
                count,
                target,
                incomplete,
                ..
            } => {
                let qualifier = if *incomplete { "At least " } else { "" };
                let noun = if *count == 1 { "line" } else { "lines" };
                format!("{qualifier}{count} uncommitted {noun} absent from {target}")
            }
            VerdictReason::UncommittedChangesOnDefault { target } => {
                format!("Uncommitted content is already on {target}")
            }
            VerdictReason::EnvironmentFilesAtRisk { count } => {
                let noun = if *count == 1 { "file is" } else { "files are" };
                format!("{count} environment {noun} not stored in git")
            }
            VerdictReason::WorkingTreeUnreadable => {
                "Could not inspect the working tree for changes".to_string()
            }
            VerdictReason::UnpushedCommits => "Has unpushed commits".to_string(),
            VerdictReason::WorkContained { target, proof } => match proof {
                LandingProof::Ancestry => {
                    format!("Branch history is reachable from {target}")
                }
                LandingProof::SameTree => {
                    format!("The branch and {target} have the same tree")
                }
                LandingProof::NoOpAtTip => {
                    format!("Merging the branch into {target} changes no files")
                }
                LandingProof::NoOpAtAncestor(commit) => {
                    format!("The branch changes no files at historical snapshot {commit}")
                }
                LandingProof::PatchEquivalent(commit) => {
                    format!("The branch patch matches commit {commit} on {target}")
                }
            },
            VerdictReason::DefaultBranchLacksCommittedContent { facts } => {
                match &facts.selected_target {
                    Some(target) => format!("{} lacks committed branch content", target.name),
                    None => "The selected target lacks committed branch content".to_string(),
                }
            }
            VerdictReason::LandingUnknown { .. } => {
                "Could not complete the landing proof".to_string()
            }
        }
    }
}

/// A worktree together with its signals and classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    #[serde(flatten)]
    pub entry: WorktreeEntry,
    pub status: WorktreeStatus,
    pub verdict: Verdict,
    pub reason: VerdictReason,
}

impl Worktree {
    /// Bytes reclaimed by deleting this worktree. Linked directories are
    /// excluded because they are shared with the main worktree.
    pub fn reclaimable_bytes(&self) -> u64 {
        if self.entry.is_main {
            return 0;
        }
        self.status.size.as_ref().map_or(0, |s| s.bytes)
    }

    /// A short label for the worktree: its branch, or a detached-HEAD marker.
    pub fn label(&self) -> String {
        if let Some(branch) = &self.entry.branch {
            return branch.clone();
        }
        match &self.entry.head {
            Some(head) => format!("detached at {}", &head[..head.len().min(7)]),
            None => "(bare)".to_string(),
        }
    }
}

#[cfg(test)]
mod uncommitted_serde_tests {
    use super::*;

    /// The frontend reads these names literally, and a mismatch is invisible
    /// on this side: Rust serialises happily, TypeScript reads `undefined`,
    /// and the app blanks the first time something touches the field. That is
    /// what shipped — `rename_all` on the enum renamed the variants and left
    /// the fields inside them alone.
    #[test]
    fn the_uncommitted_analysis_serialises_the_names_the_frontend_reads() {
        let json = serde_json::to_string(&UncommittedAnalysis::Compared {
            target: "origin/main".into(),
            leftover: 0,
            leftover_sample: vec!["x".into()],
            incomplete: false,
            shortfall: None,
        })
        .expect("serialise");

        assert!(json.contains("\"state\":\"compared\""), "{json}");
        assert!(json.contains("\"leftoverSample\""), "{json}");
        assert!(!json.contains("leftover_sample"), "{json}");
    }

    #[test]
    fn the_unchecked_state_is_named_for_the_frontend_too() {
        let json = serde_json::to_string(&UncommittedAnalysis::NotChecked).expect("serialise");
        assert_eq!(json, "{\"state\":\"notChecked\"}");
    }

    #[test]
    fn older_status_data_defaults_the_process_check_to_incomplete() {
        let mut json = serde_json::to_value(WorktreeStatus::default()).expect("serialise");
        json.as_object_mut()
            .expect("status object")
            .remove("processCheckComplete");

        let status: WorktreeStatus = serde_json::from_value(json).expect("deserialize old status");

        assert!(!status.process_check_complete);
    }
}
