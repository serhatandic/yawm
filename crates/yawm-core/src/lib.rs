//! yawm-core — all of yawm's logic.
//!
//! This crate is deliberately free of any GUI dependency. The Tauri desktop app
//! and the `yawm` command line binary are both thin shells over it, which is
//! what keeps the two frontends behaviourally identical.

pub mod api;
pub mod config;
pub mod diff;
pub mod error;
pub mod git;
pub mod model;
pub mod ops;
pub mod path;
pub mod process;
pub mod scan;
pub mod size;
pub mod verdict;

pub use api::{RepoReport, ScanOptions, Scanner};
pub use config::Config;
pub use error::{Error, MovedWorktreeDiagnostic, Result};
pub use git::landing::LandingCache;
pub use model::{
    Landing, LandingProof, UncommittedAnalysis, UnknownReason, Verdict, VerdictReason, Worktree,
    WorktreeEntry, WorktreeStatus,
};
pub use size::SizeCache;
pub use verdict::{VerdictConfig, classify};
