use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A linked worktree directory moved without repairing Git's administrative
/// record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedWorktreeDiagnostic {
    pub main_worktree: PathBuf,
    pub common_admin_dir: PathBuf,
    pub observed_path: PathBuf,
    /// Exact argv for `git -C <main> worktree repair <observed>`.
    pub repair_command: Vec<String>,
}

impl std::fmt::Display for MovedWorktreeDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} moved without updating Git's worktree record; run {}",
            self.observed_path.display(),
            self.repair_command.join(" ")
        )
    }
}

/// Errors produced by yawm-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git executable not found; yawm requires git on PATH")]
    GitMissing,

    #[error("git {args:?} failed in {cwd}: {stderr}")]
    GitFailed {
        args: Vec<String>,
        cwd: PathBuf,
        stderr: String,
    },

    #[error("git {found} is too old; yawm requires git {required} or newer")]
    GitTooOld { found: String, required: String },

    #[error("could not parse git output: {0}")]
    Parse(String),

    #[error("{0} is not inside a git repository")]
    NotARepository(PathBuf),

    #[error("{diagnostic}")]
    MovedWorktree { diagnostic: MovedWorktreeDiagnostic },

    /// The worktree is no longer what the user approved deleting.
    ///
    /// Distinct from the other variants on purpose: it is not a failure to act
    /// on, it is a request to look again. Nothing was deleted and the correct
    /// response is to re-plan and re-confirm, so a caller that shows it as a
    /// failure has misread it. The desktop shell carries the variant across the
    /// IPC boundary as its own kind rather than as a message to be matched;
    /// [`crate::ops::PLAN_CHANGED_MARKER`] remains for callers that only have
    /// the rendered text.
    #[error(
        "{path} changed since it was checked: {}. Nothing was deleted — check again before deleting.",
        changes.join("; ")
    )]
    PlanChanged {
        path: PathBuf,
        /// What differs, in the user's terms.
        changes: Vec<String>,
        /// Which worktrees this repository still has, read from the same
        /// snapshot the refusal was decided on.
        ///
        /// The caller re-plans after this refusal, and core refuses to plan a
        /// path it cannot find. Without an answer taken at this moment the
        /// caller has to guess from a list it painted earlier, and a worktree
        /// deleted from outside yawm turns "these changed, look again" into a
        /// parse error about a missing path. Callers intersect it with their
        /// own selection.
        still_present: Vec<PathBuf>,
    },

    /// A batch removal mutated something and then failed.
    ///
    /// The one outcome that must never be reported as a plain failure. Removal
    /// cannot be rolled back, so a generic error here would leave the caller
    /// believing the whole selection survived while some of it is gone. This
    /// carries exactly what did happen, in order, alongside what failed.
    #[error("{0}")]
    BatchIncomplete(Box<crate::ops::PartialRemoval>),

    /// A batch removal deleted nothing, and some of its worktrees are gone
    /// anyway.
    ///
    /// Removed by something other than yawm while the batch was running. It is
    /// not a partial removal — yawm deleted nothing, so nothing here may be
    /// reported as its work — and it is not a plain failure either, because the
    /// caller is still listing directories that no longer exist and still has
    /// them selected. Folded into a message, that fact reached the frontend as
    /// prose it could not act on, and the rows and tabs stayed. It crosses
    /// structurally so they can go.
    #[error("{0}")]
    BatchVanished(Box<crate::ops::VanishedRemoval>),

    /// Branch cleanup mutated ref/config state and could not prove that its
    /// rollback restored the approved branch incarnation.
    #[error(
        "branch {branch} cleanup failed ({cause}); rollback could not be verified \
         (branch ref may have changed: {ref_may_have_changed}; branch config may have changed: \
         {config_may_have_changed})"
    )]
    BranchRollbackFailed {
        branch: String,
        cause: String,
        ref_may_have_changed: bool,
        config_may_have_changed: bool,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
