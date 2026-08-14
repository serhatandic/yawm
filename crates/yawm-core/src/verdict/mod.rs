//! Classifying a worktree as safe to delete, or not.
//!
//! Rules are evaluated in order and the first match wins, so the ordering *is*
//! the policy. It runs from most protective to least: anything that could
//! destroy work is checked before anything that would suggest deletion.
//!
//! The engine is deliberately conservative. Misclassifying a disposable
//! worktree as `Review` costs the user a few seconds; misclassifying a
//! worktree holding real work as `Disposable` costs them the work.

use crate::model::{
    Landing, UncommittedAnalysis, Verdict, VerdictReason, WorktreeEntry, WorktreeStatus,
};

/// Thresholds controlling the time-based rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerdictConfig {
    /// Activity this recent means something is probably still working here.
    pub active_within_secs: i64,
}

impl Default for VerdictConfig {
    fn default() -> Self {
        Self {
            // Long enough to cover an agent pausing to think, short enough that
            // yesterday's worktree is not treated as live.
            active_within_secs: 30 * 60,
        }
    }
}

/// Classify a worktree. `now` is a Unix timestamp, injected so the time-based
/// rules are testable.
pub fn classify(
    entry: &WorktreeEntry,
    status: &WorktreeStatus,
    cfg: &VerdictConfig,
    now: i64,
) -> (Verdict, VerdictReason) {
    // 1. The directory is gone. Nothing else can be determined, and the only
    //    sensible action is pruning stale metadata.
    if entry.prunable.is_some() {
        return (
            Verdict::Broken,
            VerdictReason::DirectoryMissing {
                detail: entry.prunable.clone(),
                lock: entry.locked.clone(),
            },
        );
    }

    // 2. The main worktree underpins every other one and can never be removed.
    if entry.is_main {
        return (Verdict::Keep, VerdictReason::MainWorktree);
    }

    // 3. An explicit lock is a deliberate "leave this alone".
    if entry.locked.is_some() {
        return (Verdict::Keep, VerdictReason::Locked);
    }

    // 4. Something is running in this directory right now — very likely an
    //    agent mid-task.
    if !status.processes.is_empty() {
        return (Verdict::Keep, VerdictReason::ProcessRunning);
    }

    // 5. Files changed moments ago, even if no process was detected. This is
    //    the only "in use" signal available on platforms where process
    //    inspection is unavailable.
    if let Some(activity) = last_activity(status)
        && now.saturating_sub(activity) <= cfg.active_within_secs
    {
        return (Verdict::Keep, VerdictReason::RecentlyActive);
    }

    // 6. A failed inspection cannot be allowed to masquerade as a clean result.
    if status.dirty.is_unknown() {
        return (Verdict::Review, VerdictReason::WorkingTreeUnreadable);
    }

    // 7. Mutable state never becomes disposable. A complete zero-leftover
    //    comparison can lower the warning to review, but cannot create the git
    //    record needed for a deletion proof.
    if status.dirty.is_dirty() {
        return match &status.uncommitted {
            UncommittedAnalysis::Compared {
                target,
                leftover,
                incomplete,
                shortfall,
                ..
            } if *leftover > 0 => (
                Verdict::Keep,
                VerdictReason::UncommittedChangesAtRisk {
                    count: *leftover,
                    target: target.clone(),
                    incomplete: *incomplete,
                    shortfall: shortfall.clone(),
                },
            ),
            UncommittedAnalysis::Compared {
                target,
                leftover: 0,
                incomplete: false,
                ..
            } => (
                Verdict::Review,
                VerdictReason::UncommittedChangesOnDefault {
                    target: target.clone(),
                },
            ),
            _ => (Verdict::Keep, VerdictReason::UncommittedChanges),
        };
    }

    // 8. An untracked environment file is risky only when the main worktree has
    //    no matching copy. That is evidence to inspect, not proof it is needed.
    if !status.env_files.is_empty() {
        return (
            Verdict::Review,
            VerdictReason::EnvironmentFilesAtRisk {
                count: status.env_files.len(),
            },
        );
    }

    // 9. Commits that have not reached the remote exist only here.
    if status.upstream.ahead > 0 {
        return (Verdict::Keep, VerdictReason::UnpushedCommits);
    }

    // 10. Only proof of containment and a completed live-process check permit
    //     deletion. A negative proof keeps the work, while an incomplete proof
    //     or skipped process check leaves the decision to a person.
    match &status.landing {
        Landing::Landed { target, proof } => {
            if status.process_check_complete {
                (
                    Verdict::Disposable,
                    VerdictReason::WorkContained {
                        target: target.clone(),
                        proof: proof.clone(),
                    },
                )
            } else {
                (Verdict::Review, VerdictReason::ProcessCheckSkipped)
            }
        }
        Landing::AddsContent { .. } => (
            Verdict::Keep,
            VerdictReason::DefaultBranchLacksCommittedContent {
                facts: Box::new(status.landing_facts.clone()),
            },
        ),
        Landing::Unknown { .. } => (
            Verdict::Review,
            VerdictReason::LandingUnknown {
                facts: Box::new(status.landing_facts.clone()),
            },
        ),
    }
}

/// A settling scan can defer costly proof phases when an earlier policy rule
/// already prevents deletion. The dedicated landing pass and a details request
/// bypass this gate, so prioritisation never becomes a permanently hidden fact.
pub(crate) fn should_run_expensive_landing(
    entry: &WorktreeEntry,
    status: &WorktreeStatus,
    cfg: &VerdictConfig,
    now: i64,
) -> bool {
    entry.prunable.is_none()
        && !entry.is_main
        && !entry.bare
        && entry.locked.is_none()
        && status.processes.is_empty()
        && !status.dirty.is_unknown()
        && !status.dirty.is_dirty()
        && status.env_files.is_empty()
        && status.upstream.ahead == 0
        && last_activity(status)
            .is_none_or(|activity| now.saturating_sub(activity) > cfg.active_within_secs)
}

/// Best available "when was this last touched" signal.
///
/// Filesystem modification time is preferred because it reflects real work,
/// including work an agent has not committed. The last commit time is a
/// fallback for when the directory has not been walked yet.
fn last_activity(status: &WorktreeStatus) -> Option<i64> {
    status
        .size
        .as_ref()
        .and_then(|s| s.last_modified)
        .or(status.last_commit_at)
}

/// Whether yawm will offer to delete this worktree at all.
pub fn is_deletable(entry: &WorktreeEntry) -> bool {
    !entry.is_main && !entry.bare
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DirtyCounts, LandingProof, LockInfo, ProcessInfo, SizeInfo, UpstreamInfo};

    const NOW: i64 = 1_800_000_000;
    const MINUTE: i64 = 60;
    const DAY: i64 = 24 * 60 * 60;

    fn linked() -> WorktreeEntry {
        WorktreeEntry {
            path: "/w/feature".into(),
            head: Some("abc123".into()),
            branch: Some("feat/x".into()),
            is_main: false,
            ..Default::default()
        }
    }

    /// A clean worktree whose last activity is long past, so time-based rules
    /// stay out of the way unless a test opts into them.
    fn settled() -> WorktreeStatus {
        WorktreeStatus {
            last_commit_at: Some(NOW - 2 * DAY),
            process_check_complete: true,
            ..Default::default()
        }
    }

    fn landed() -> Landing {
        Landing::Landed {
            target: "origin/main".into(),
            proof: LandingProof::Ancestry,
        }
    }

    fn verdict(entry: &WorktreeEntry, status: &WorktreeStatus) -> Verdict {
        classify(entry, status, &VerdictConfig::default(), NOW).0
    }

    fn reason(entry: &WorktreeEntry, status: &WorktreeStatus) -> VerdictReason {
        classify(entry, status, &VerdictConfig::default(), NOW).1
    }

    #[test]
    fn missing_directory_is_broken() {
        let mut e = linked();
        e.prunable = Some("gitdir file points to non-existent location".into());
        assert_eq!(verdict(&e, &settled()), Verdict::Broken);
    }

    #[test]
    fn broken_outranks_every_other_signal() {
        let mut e = linked();
        e.prunable = Some("gone".into());
        e.locked = Some(LockInfo { reason: None });
        let mut s = settled();
        s.dirty = DirtyCounts {
            staged: 5,
            ..Default::default()
        };
        assert_eq!(verdict(&e, &s), Verdict::Broken);
        assert_eq!(
            reason(&e, &s),
            VerdictReason::DirectoryMissing {
                detail: Some("gone".into()),
                lock: Some(LockInfo { reason: None }),
            }
        );
    }

    #[test]
    fn main_worktree_is_always_kept() {
        let mut e = linked();
        e.is_main = true;
        let mut s = settled();
        s.landing = landed();
        assert_eq!(verdict(&e, &s), Verdict::Keep);
        assert_eq!(reason(&e, &s), VerdictReason::MainWorktree);
    }

    #[test]
    fn main_worktree_is_never_offered_for_deletion() {
        let mut e = linked();
        e.is_main = true;
        assert!(!is_deletable(&e));
        assert!(is_deletable(&linked()));
    }

    #[test]
    fn locked_worktree_is_kept_even_when_merged() {
        let mut e = linked();
        e.locked = Some(LockInfo {
            reason: Some("agent running".into()),
        });
        let mut s = settled();
        s.landing = landed();
        assert_eq!(verdict(&e, &s), Verdict::Keep);
        assert_eq!(reason(&e, &s), VerdictReason::Locked);
    }

    #[test]
    fn a_running_process_keeps_a_merged_worktree() {
        let mut s = settled();
        s.landing = landed();
        s.processes = vec![ProcessInfo {
            pid: 42,
            name: "node".into(),
        }];
        assert_eq!(verdict(&linked(), &s), Verdict::Keep);
        assert_eq!(reason(&linked(), &s), VerdictReason::ProcessRunning);
    }

    #[test]
    fn a_skipped_process_check_prevents_disposal() {
        let mut s = settled();
        s.landing = landed();
        s.process_check_complete = false;

        assert_eq!(verdict(&linked(), &s), Verdict::Review);
        assert_eq!(reason(&linked(), &s), VerdictReason::ProcessCheckSkipped);
    }

    #[test]
    fn a_skipped_process_check_does_not_weaken_known_unlanded_work() {
        let mut s = settled();
        s.process_check_complete = false;
        s.landing = Landing::AddsContent {
            target: "origin/main".into(),
        };

        assert_eq!(verdict(&linked(), &s), Verdict::Keep);
        assert!(matches!(
            reason(&linked(), &s),
            VerdictReason::DefaultBranchLacksCommittedContent { .. }
        ));
    }

    #[test]
    fn recent_file_activity_keeps_a_merged_worktree() {
        let mut s = settled();
        s.landing = landed();
        s.size = Some(SizeInfo {
            last_modified: Some(NOW - 5 * MINUTE),
            ..Default::default()
        });
        assert_eq!(verdict(&linked(), &s), Verdict::Keep);
        assert_eq!(reason(&linked(), &s), VerdictReason::RecentlyActive);
    }

    #[test]
    fn activity_just_outside_the_window_does_not_keep() {
        let mut s = settled();
        s.landing = landed();
        s.size = Some(SizeInfo {
            last_modified: Some(NOW - 31 * MINUTE),
            ..Default::default()
        });
        assert_eq!(verdict(&linked(), &s), Verdict::Disposable);
    }

    #[test]
    fn filesystem_activity_outranks_commit_time() {
        // Built from an old commit, but actively edited: must be kept.
        let mut s = settled();
        s.landing = landed();
        s.last_commit_at = Some(NOW - 400 * DAY);
        s.size = Some(SizeInfo {
            last_modified: Some(NOW - MINUTE),
            ..Default::default()
        });
        assert_eq!(verdict(&linked(), &s), Verdict::Keep);
    }

    #[test]
    fn uncommitted_changes_prevent_disposal() {
        for dirty in [
            DirtyCounts {
                staged: 1,
                ..Default::default()
            },
            DirtyCounts {
                unstaged: 1,
                ..Default::default()
            },
            DirtyCounts {
                untracked: 1,
                ..Default::default()
            },
        ] {
            let mut s = settled();
            s.landing = landed();
            s.dirty = dirty;
            assert_eq!(verdict(&linked(), &s), Verdict::Keep, "{dirty:?}");
            assert_eq!(reason(&linked(), &s), VerdictReason::UncommittedChanges);
        }
    }

    #[test]
    fn uncommitted_content_already_on_default_requires_review_not_keep() {
        let mut status = settled();
        status.dirty.unstaged = 1;
        status.uncommitted = UncommittedAnalysis::Compared {
            target: "origin/main".into(),
            leftover: 0,
            leftover_sample: Vec::new(),
            incomplete: false,
            shortfall: None,
        };

        assert_eq!(verdict(&linked(), &status), Verdict::Review);
        assert_eq!(
            reason(&linked(), &status),
            VerdictReason::UncommittedChangesOnDefault {
                target: "origin/main".into()
            }
        );
    }

    #[test]
    fn uncommitted_deletion_default_still_has_is_kept() {
        let mut status = settled();
        status.dirty.unstaged = 1;
        status.uncommitted = UncommittedAnalysis::Compared {
            target: "origin/main".into(),
            leftover: 1,
            leftover_sample: vec!["removed behavior".into()],
            incomplete: false,
            shortfall: None,
        };

        assert_eq!(verdict(&linked(), &status), Verdict::Keep);
        assert_eq!(
            reason(&linked(), &status),
            VerdictReason::UncommittedChangesAtRisk {
                count: 1,
                target: "origin/main".into(),
                incomplete: false,
                shortfall: None,
            }
        );
    }

    #[test]
    fn unique_environment_files_require_review() {
        let mut s = settled();
        s.landing = landed();
        s.env_files = vec![".env.local".into()];

        assert_eq!(verdict(&linked(), &s), Verdict::Review);
        assert_eq!(
            reason(&linked(), &s),
            VerdictReason::EnvironmentFilesAtRisk { count: 1 }
        );
    }

    #[test]
    fn failed_working_tree_inspection_has_its_own_reason() {
        let mut s = settled();
        s.landing = landed();
        s.dirty.inspection_failed = true;

        assert_eq!(verdict(&linked(), &s), Verdict::Review);
        assert_eq!(reason(&linked(), &s), VerdictReason::WorkingTreeUnreadable);
    }

    #[test]
    fn unpushed_commits_prevent_disposal() {
        let mut s = settled();
        s.upstream = UpstreamInfo {
            name: Some("origin/feat/x".into()),
            ahead: 2,
            ..Default::default()
        };
        assert_eq!(verdict(&linked(), &s), Verdict::Keep);
        assert_eq!(reason(&linked(), &s), VerdictReason::UnpushedCommits);
    }

    #[test]
    fn proven_containment_is_disposable() {
        let mut s = settled();
        s.landing = landed();
        assert_eq!(verdict(&linked(), &s), Verdict::Disposable);
        assert_eq!(
            reason(&linked(), &s),
            VerdictReason::WorkContained {
                target: "origin/main".into(),
                proof: LandingProof::Ancestry,
            }
        );
    }

    #[test]
    fn a_deleted_upstream_is_not_evidence_of_landing() {
        let mut s = settled();
        s.upstream = UpstreamInfo {
            name: Some("origin/feat/x".into()),
            gone: true,
            ..Default::default()
        };
        assert_eq!(verdict(&linked(), &s), Verdict::Review);
        assert!(matches!(
            reason(&linked(), &s),
            VerdictReason::LandingUnknown { .. }
        ));
    }

    #[test]
    fn behind_upstream_alone_does_not_prevent_disposal() {
        let mut s = settled();
        s.landing = landed();
        s.upstream = UpstreamInfo {
            name: Some("origin/feat/x".into()),
            behind: 10,
            ..Default::default()
        };
        assert_eq!(verdict(&linked(), &s), Verdict::Disposable);
    }

    #[test]
    fn unknown_containment_needs_review() {
        assert_eq!(verdict(&linked(), &settled()), Verdict::Review);
        assert!(matches!(
            reason(&linked(), &settled()),
            VerdictReason::LandingUnknown { .. }
        ));
    }

    #[test]
    fn age_cannot_turn_unknown_containment_into_a_negative_claim() {
        let mut s = settled();
        s.last_commit_at = Some(NOW - 30 * DAY);
        assert_eq!(verdict(&linked(), &s), Verdict::Review);
        assert!(matches!(
            reason(&linked(), &s),
            VerdictReason::LandingUnknown { .. }
        ));
    }

    #[test]
    fn proved_missing_content_is_kept() {
        let mut s = settled();
        s.landing = Landing::AddsContent {
            target: "origin/main".into(),
        };
        assert_eq!(verdict(&linked(), &s), Verdict::Keep);
        assert!(matches!(
            reason(&linked(), &s),
            VerdictReason::DefaultBranchLacksCommittedContent { .. }
        ));
    }

    /// A branch that was never pushed has no upstream, so `ahead` is zero. It
    /// must not be mistaken for landed work.
    #[test]
    fn never_pushed_branch_is_not_disposable() {
        let mut s = settled();
        s.upstream = UpstreamInfo::default();
        assert_eq!(verdict(&linked(), &s), Verdict::Review);
    }

    #[test]
    fn detached_worktree_merged_into_default_is_disposable() {
        let mut e = linked();
        e.branch = None;
        e.detached = true;
        let mut s = settled();
        s.landing = landed();
        assert_eq!(verdict(&e, &s), Verdict::Disposable);
    }

    #[test]
    fn worktree_with_no_signals_at_all_is_reviewed_not_disposed() {
        // Everything unknown must never resolve to "safe to delete".
        let s = WorktreeStatus::default();
        assert_eq!(verdict(&linked(), &s), Verdict::Review);
    }

    #[test]
    fn thresholds_are_configurable() {
        let cfg = VerdictConfig {
            active_within_secs: 60 * 60 * 24,
        };
        let mut s = settled();
        s.landing = landed();
        s.size = Some(SizeInfo {
            last_modified: Some(NOW - 2 * 60 * 60),
            ..Default::default()
        });
        // Two hours old counts as active under a 24-hour window.
        assert_eq!(classify(&linked(), &s, &cfg, NOW).0, Verdict::Keep);
    }

    #[test]
    fn protective_rules_defer_expensive_landing_work() {
        let cfg = VerdictConfig::default();
        assert!(should_run_expensive_landing(
            &linked(),
            &settled(),
            &cfg,
            NOW
        ));

        let mut locked = linked();
        locked.locked = Some(LockInfo { reason: None });
        assert!(!should_run_expensive_landing(
            &locked,
            &settled(),
            &cfg,
            NOW
        ));

        let mut dirty = settled();
        dirty.dirty.untracked = 1;
        assert!(!should_run_expensive_landing(&linked(), &dirty, &cfg, NOW));

        let mut active = settled();
        active.size = Some(SizeInfo {
            last_modified: Some(NOW - MINUTE),
            ..Default::default()
        });
        assert!(!should_run_expensive_landing(&linked(), &active, &cfg, NOW));
    }

    #[test]
    fn verdict_names_are_stable_for_the_ui() {
        assert_eq!(Verdict::Broken.as_str(), "broken");
        assert_eq!(Verdict::Keep.as_str(), "keep");
        assert_eq!(Verdict::Disposable.as_str(), "disposable");
        assert_eq!(Verdict::Review.as_str(), "review");
    }

    #[test]
    fn every_reason_has_a_description() {
        for r in [
            VerdictReason::DirectoryMissing {
                detail: Some("gone".into()),
                lock: None,
            },
            VerdictReason::Locked,
            VerdictReason::MainWorktree,
            VerdictReason::ProcessRunning,
            VerdictReason::ProcessCheckSkipped,
            VerdictReason::RecentlyActive,
            VerdictReason::UncommittedChanges,
            VerdictReason::UncommittedChangesAtRisk {
                count: 1,
                target: "origin/main".into(),
                incomplete: false,
                shortfall: None,
            },
            VerdictReason::UncommittedChangesOnDefault {
                target: "origin/main".into(),
            },
            VerdictReason::EnvironmentFilesAtRisk { count: 2 },
            VerdictReason::WorkingTreeUnreadable,
            VerdictReason::UnpushedCommits,
            VerdictReason::WorkContained {
                target: "origin/main".into(),
                proof: LandingProof::Ancestry,
            },
            VerdictReason::DefaultBranchLacksCommittedContent {
                facts: Box::default(),
            },
            VerdictReason::LandingUnknown {
                facts: Box::default(),
            },
        ] {
            assert!(!r.describe().is_empty());
        }
    }

    #[test]
    fn new_reason_payloads_match_the_frontend_contract() {
        assert_eq!(
            serde_json::to_value(VerdictReason::EnvironmentFilesAtRisk { count: 2 }).unwrap(),
            serde_json::json!({"kind": "environmentFilesAtRisk", "count": 2})
        );
        assert_eq!(
            serde_json::to_value(VerdictReason::WorkingTreeUnreadable).unwrap(),
            serde_json::json!({"kind": "workingTreeUnreadable"})
        );
        assert_eq!(
            serde_json::to_value(VerdictReason::ProcessCheckSkipped).unwrap(),
            serde_json::json!({"kind": "processCheckSkipped"})
        );
    }
}
