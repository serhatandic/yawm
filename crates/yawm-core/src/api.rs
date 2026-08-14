//! The API both frontends call.
//!
//! Scanning is split into phases because the desktop app streams results: it
//! paints the worktree list as soon as names are known, then fills in sizes,
//! then settles historical landing proofs. The CLI simply runs its requested
//! phases before printing. Keeping the facts here rather than in either
//! frontend is what makes the two agree.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::git::Git;
use crate::git::collect::{
    CollectedStatus, RepoContext, collect_status, list_worktrees, load_context_with_cache,
    populate_landing, populate_uncommitted, prepare_landing,
};
use crate::git::landing::{LandingCache, LandingDepth};
use crate::model::{Verdict, Worktree};
use crate::path::path_key;
use crate::process;
use crate::scan::{Discovery, UnreadableSource};
use crate::size::{SizeCache, SizeOptions, measure_and_cache};
use crate::verdict::{classify, should_run_expensive_landing};

/// Which of the expensive phases to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanOptions {
    /// Walk each worktree to compute size and last activity.
    pub measure_size: bool,
    /// Look for processes running inside each worktree.
    pub detect_processes: bool,
    /// Settle merge-tree landing proofs during this scan.
    ///
    /// Independent from size and process collection: CLI callers may skip
    /// either measurement without weakening the Git classification. The
    /// desktop's first-paint scan leaves this off and resolves landing
    /// progressively after its full pass.
    pub settle_landing: bool,
    /// Fill the size column from a previous measurement rather than leaving it
    /// blank. Only meaningful when `measure_size` is off.
    pub use_cached_size: bool,
    /// Leave the main worktrees unwalked.
    ///
    /// Set when the caller has already decided not to show them. On a real
    /// machine the main worktrees are the overwhelming majority of the bytes —
    /// 18.7 GB of 21.0 GB across 21 worktrees in the case this was measured on
    /// — so walking them is most of the wait for rows that are never drawn.
    /// Nothing is lost by skipping: a main worktree cannot be deleted, so its
    /// size can never be reclaimable and never reaches a total that is shown.
    ///
    /// Off by default, because the CLI totals every worktree and would quietly
    /// under-report if this followed a display preference it does not share.
    pub skip_main_size: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            measure_size: true,
            detect_processes: true,
            settle_landing: true,
            use_cached_size: false,
            skip_main_size: false,
        }
    }
}

impl ScanOptions {
    /// Names and git signals only. Used for the first paint.
    ///
    /// Sizes already measured are carried over, because the alternative is a
    /// column of dashes on a worktree yawm measured a moment ago.
    pub fn fast() -> Self {
        Self {
            measure_size: false,
            detect_processes: false,
            settle_landing: false,
            use_cached_size: true,
            skip_main_size: false,
        }
    }
}

/// Kept at the proof boundary so discovery, status, processes, and size walks
/// cannot accidentally inherit a throughput limit meant only for costly git.
pub trait LandingWorkLimiter: std::fmt::Debug + Send + Sync {
    fn run(&self, work: &mut dyn FnMut());
}

/// One repository and its worktrees.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoReport {
    /// Directory name of the main worktree, used as the display name.
    pub name: String,
    pub root: PathBuf,
    /// Primary default ref used for containment proofs, e.g. `origin/main`.
    pub default_ref: Option<String>,
    pub worktrees: Vec<Worktree>,
}

impl RepoReport {
    /// Total disk usage across all worktrees.
    pub fn total_bytes(&self) -> u64 {
        self.worktrees
            .iter()
            .filter_map(|w| w.status.size.as_ref())
            .map(|s| s.bytes)
            .sum()
    }

    /// Bytes that deleting every disposable worktree would free.
    pub fn reclaimable_bytes(&self) -> u64 {
        self.worktrees
            .iter()
            .filter(|w| w.verdict == Verdict::Disposable)
            .map(|w| w.reclaimable_bytes())
            .sum()
    }

    pub fn count_of(&self, verdict: Verdict) -> usize {
        self.worktrees
            .iter()
            .filter(|w| w.verdict == verdict)
            .count()
    }
}

/// Runs scans against a configuration.
#[derive(Debug, Clone)]
pub struct Scanner {
    git: Git,
    config: Config,
    size_options: SizeOptions,
    landing_cache: LandingCache,
    size_cache: SizeCache,
    landing_limiter: Option<Arc<dyn LandingWorkLimiter>>,
}

/// What a scan of everything found, and what it could not look at.
///
/// The two travel together because a report list on its own cannot be read: an
/// empty one means "nothing to worry about" and "I could not find out" at the
/// same time, and those are opposite answers to the only question this app
/// exists to answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub repos: Vec<RepoReport>,
    /// Configured sources that could not be read, and why.
    pub unreadable: Vec<UnreadableSource>,
}

impl Scanner {
    pub fn new(config: Config) -> Self {
        Self::with_caches(config, LandingCache::default(), SizeCache::default())
    }

    pub fn with_landing_cache(config: Config, landing_cache: LandingCache) -> Self {
        Self::with_caches(config, landing_cache, SizeCache::default())
    }

    /// Both caches live outside the scanner so a long-running frontend can keep
    /// them across scans; a fresh `Scanner` per call would relearn everything.
    pub fn with_caches(config: Config, landing_cache: LandingCache, size_cache: SizeCache) -> Self {
        Self {
            git: Git::new(),
            config,
            size_options: SizeOptions::default(),
            landing_cache,
            size_cache,
            landing_limiter: None,
        }
    }

    pub fn with_landing_limiter(mut self, limiter: Arc<dyn LandingWorkLimiter>) -> Self {
        self.landing_limiter = Some(limiter);
        self
    }

    /// Use a specific git executable. Mostly for tests.
    pub fn with_git(mut self, git: Git) -> Self {
        self.git = git;
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn git(&self) -> &Git {
        &self.git
    }

    /// Every repository yawm knows about: those added by hand, plus those found
    /// beneath the configured scan roots.
    pub fn repositories(&self) -> Vec<PathBuf> {
        self.repositories_reporting().repositories
    }

    /// [`Scanner::repositories`], keeping the sources it could not read.
    pub fn repositories_reporting(&self) -> Discovery {
        // Scoped to the active workspace, so a demo group and real work never
        // appear in the same list.
        let (repos, scan_roots) = self.config.scoped_sources();
        let discovered =
            crate::scan::discover_reporting(&self.git, &scan_roots, self.config.scan_depth);

        let mut candidates = repos;
        candidates.extend(discovered.repositories.iter().cloned());
        let mut found = crate::scan::resolve_repositories_reporting(&self.git, &candidates);
        found.absorb_failures(discovered.unreadable);
        found
    }

    /// Scan one repository.
    pub fn scan_repo(&self, path: &Path, opts: ScanOptions) -> Result<RepoReport> {
        let entries = list_worktrees(&self.git, path)?;
        let ctx = load_context_with_cache(&self.git, path, &entries, self.landing_cache.clone())?;
        prepare_landing(&self.git, &entries, &ctx);

        let processes = if opts.detect_processes {
            let roots: Vec<PathBuf> = entries.iter().map(|e| e.path.clone()).collect();
            process::scan(&roots)
        } else {
            Default::default()
        };

        let verdict_config = self.config.verdict_config();
        let now = now_unix();

        let worktrees = entries
            .into_iter()
            .map(|entry| {
                let mut status = collect_status(&self.git, &entry, &ctx.branch).status;
                status.process_check_complete = opts.detect_processes;

                if entry.prunable.is_none() {
                    let skipped = opts.skip_main_size && entry.is_main;
                    if opts.measure_size && !skipped {
                        status.size = Some(measure_and_cache(
                            &entry.path,
                            &self.size_options,
                            &self.size_cache,
                        ));
                    } else if opts.use_cached_size || skipped {
                        // A skipped worktree still gets whatever was measured
                        // before, because the cost being avoided is the disk
                        // walk, not the lookup. This is also what keeps a main
                        // worktree from blanking out when the user hides main
                        // worktrees after they have already been measured.
                        status.size = self.size_cache.get(&entry.path);
                    }
                }
                if let Some(found) = processes.get(&path_key(&entry.path)) {
                    status.processes = found.clone();
                }
                let depth = if opts.settle_landing
                    && should_run_expensive_landing(&entry, &status, &verdict_config, now)
                {
                    LandingDepth::MergeTree
                } else {
                    LandingDepth::TierOne
                };
                self.populate_landing(&entry, &ctx, &mut status, depth);

                let (verdict, reason) = classify(&entry, &status, &verdict_config, now);
                Worktree {
                    entry,
                    status,
                    verdict,
                    reason,
                }
            })
            .collect();

        Ok(build_report(&ctx, worktrees))
    }

    /// Run the on-demand historical proof for one worktree.
    pub fn inspect_worktree(&self, path: &Path, worktree_path: &Path) -> Result<Worktree> {
        self.inspect_worktree_with_size(path, worktree_path, true, true)
    }

    /// Settle one row for the progressive landing pass without repeating its
    /// disk walk. The preceding full scan populated the size cache, while a
    /// missing cache entry stays unknown rather than turning into zero bytes.
    pub fn resolve_worktree_landing(&self, path: &Path, worktree_path: &Path) -> Result<Worktree> {
        self.inspect_worktree_with_size(path, worktree_path, false, false)
    }

    fn inspect_worktree_with_size(
        &self,
        path: &Path,
        worktree_path: &Path,
        measure_size: bool,
        force_history: bool,
    ) -> Result<Worktree> {
        let entries = list_worktrees(&self.git, path)?;
        let ctx = load_context_with_cache(&self.git, path, &entries, self.landing_cache.clone())?;
        let entry = entries
            .into_iter()
            .find(|entry| path_key(&entry.path) == path_key(worktree_path))
            .ok_or_else(|| {
                crate::error::Error::Parse(format!(
                    "{} is not a worktree of this repository",
                    worktree_path.display()
                ))
            })?;

        let CollectedStatus {
            mut status,
            dirty: dirty_scan,
        } = collect_status(&self.git, &entry, &ctx.branch);
        let dirty_paths: Vec<Vec<u8>> = dirty_scan
            .paths
            .iter()
            .map(|path| path.raw_path.clone())
            .collect();
        if entry.prunable.is_none() {
            status.size = if measure_size {
                Some(measure_and_cache(
                    &entry.path,
                    &self.size_options,
                    &self.size_cache,
                ))
            } else {
                self.size_cache.get(&entry.path)
            };
        }
        if let Some(found) =
            process::scan(std::slice::from_ref(&entry.path)).get(&path_key(&entry.path))
        {
            status.processes = found.clone();
        }
        status.process_check_complete = true;
        self.populate_landing(&entry, &ctx, &mut status, LandingDepth::TierOne);
        if force_history || !status.landing_complete {
            self.populate_landing(&entry, &ctx, &mut status, LandingDepth::History);
        }
        self.populate_uncommitted(&entry, &ctx, &mut status, &dirty_paths);

        let (verdict, reason) =
            classify(&entry, &status, &self.config.verdict_config(), now_unix());
        Ok(Worktree {
            entry,
            status,
            verdict,
            reason,
        })
    }

    fn populate_landing(
        &self,
        entry: &crate::model::WorktreeEntry,
        ctx: &RepoContext,
        status: &mut crate::model::WorktreeStatus,
        depth: LandingDepth,
    ) {
        if depth == LandingDepth::TierOne {
            populate_landing(&self.git, entry, ctx, status, depth);
            return;
        }

        // A limiter may decline speculative work; retaining the cheap result is
        // conservative and lets a later foreground request settle the answer.
        populate_landing(&self.git, entry, ctx, status, LandingDepth::TierOne);
        let mut expensive = || populate_landing(&self.git, entry, ctx, status, depth);
        if let Some(limiter) = &self.landing_limiter {
            limiter.run(&mut expensive);
        } else {
            expensive();
        }
    }

    fn populate_uncommitted(
        &self,
        entry: &crate::model::WorktreeEntry,
        ctx: &RepoContext,
        status: &mut crate::model::WorktreeStatus,
        dirty_paths: &[Vec<u8>],
    ) {
        let mut expensive = || populate_uncommitted(&self.git, entry, ctx, status, dirty_paths);
        if let Some(limiter) = &self.landing_limiter {
            limiter.run(&mut expensive);
        } else {
            expensive();
        }
    }

    /// Scan every known repository, skipping any that cannot be read.
    ///
    /// One unreadable repository must not blank out the whole list, so failures
    /// are dropped rather than propagated. Callers with somewhere to show them
    /// should use [`Scanner::scan_all_reporting`] instead: a dropped failure
    /// shortens the list, and a shorter list reads as "those worktrees were
    /// cleaned up".
    pub fn scan_all(&self, opts: ScanOptions) -> Vec<RepoReport> {
        match self.scan_all_reporting(opts) {
            Ok(report) => report.repos,
            Err(_) => Vec::new(),
        }
    }

    /// [`Scanner::scan_all`], keeping every failure it met.
    ///
    /// Per-repository failures are collected; a failure that means no scan
    /// could have succeeded — git not being installed — is returned as an
    /// error, because reporting it once is honest and reporting it against
    /// every repository separately is noise.
    pub fn scan_all_reporting(&self, opts: ScanOptions) -> Result<ScanReport> {
        let discovery = self.repositories_reporting();
        let mut report = ScanReport {
            repos: Vec::with_capacity(discovery.repositories.len()),
            unreadable: discovery.unreadable,
        };

        for path in discovery.repositories {
            match self.scan_repo(&path, opts) {
                Ok(repo) => report.repos.push(repo),
                Err(Error::GitMissing) => return Err(Error::GitMissing),
                Err(e) => {
                    let moved_worktree = match &e {
                        Error::MovedWorktree { diagnostic } => Some(diagnostic.clone()),
                        _ => None,
                    };
                    report.unreadable.push(UnreadableSource {
                        path,
                        reason: e.to_string(),
                        moved_worktree,
                    });
                }
            }
        }

        // Nothing came back, which is what a machine with no git looks like and
        // also what a machine with nothing configured looks like. One extra
        // process settles it, and only ever on the path where the answer would
        // otherwise be an empty list the user has to interpret.
        if report.repos.is_empty() {
            self.git.clone().detect_version()?;
        }

        report.repos.sort_by_key(|r| r.name.to_lowercase());
        Ok(report)
    }
}

fn build_report(ctx: &RepoContext, worktrees: Vec<Worktree>) -> RepoReport {
    RepoReport {
        name: ctx
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ctx.root().display().to_string()),
        root: ctx.root().to_path_buf(),
        default_ref: ctx.default_ref.clone(),
        worktrees,
    }
}

/// Current time as a Unix timestamp.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SizeInfo, VerdictReason, WorktreeEntry, WorktreeStatus};

    fn worktree(verdict: Verdict, bytes: u64, is_main: bool) -> Worktree {
        Worktree {
            entry: WorktreeEntry {
                path: "/w".into(),
                is_main,
                ..Default::default()
            },
            status: WorktreeStatus {
                size: Some(SizeInfo {
                    bytes,
                    ..Default::default()
                }),
                ..Default::default()
            },
            verdict,
            reason: VerdictReason::LandingUnknown {
                facts: Box::default(),
            },
        }
    }

    fn report(worktrees: Vec<Worktree>) -> RepoReport {
        RepoReport {
            name: "demo".into(),
            root: "/demo".into(),
            default_ref: Some("origin/main".into()),
            worktrees,
        }
    }

    #[test]
    fn totals_every_worktree() {
        let r = report(vec![
            worktree(Verdict::Keep, 1000, true),
            worktree(Verdict::Disposable, 500, false),
        ]);
        assert_eq!(r.total_bytes(), 1500);
    }

    #[test]
    fn only_disposable_worktrees_count_as_reclaimable() {
        let r = report(vec![
            worktree(Verdict::Keep, 1000, false),
            worktree(Verdict::Disposable, 500, false),
            worktree(Verdict::Review, 700, false),
        ]);
        assert_eq!(r.reclaimable_bytes(), 500);
    }

    /// The main worktree can never be deleted, so its size is never offered as
    /// space to reclaim even if something misclassified it.
    #[test]
    fn the_main_worktree_never_counts_as_reclaimable() {
        let r = report(vec![worktree(Verdict::Disposable, 9999, true)]);
        assert_eq!(r.reclaimable_bytes(), 0);
    }

    #[test]
    fn counts_by_verdict() {
        let r = report(vec![
            worktree(Verdict::Disposable, 1, false),
            worktree(Verdict::Disposable, 1, false),
            worktree(Verdict::Keep, 1, false),
        ]);
        assert_eq!(r.count_of(Verdict::Disposable), 2);
        assert_eq!(r.count_of(Verdict::Keep), 1);
        assert_eq!(r.count_of(Verdict::Broken), 0);
    }

    #[test]
    fn fast_scans_skip_the_expensive_phases() {
        let fast = ScanOptions::fast();
        assert!(!fast.measure_size);
        assert!(!fast.detect_processes);
        assert!(!fast.settle_landing);
        assert!(fast.use_cached_size);

        let full = ScanOptions::default();
        assert!(full.measure_size);
        assert!(full.detect_processes);
        assert!(full.settle_landing);
    }

    fn config_with_repo(path: &Path) -> Config {
        let mut config = Config::default();
        config.add_repo_to(None, path.to_path_buf());
        config
    }

    /// The unmounted-drive case, end to end. A repository that cannot be read
    /// used to leave nothing behind at all, so the user saw a shorter list and
    /// no way to tell it from a tidier machine.
    #[test]
    fn a_repository_that_cannot_be_read_comes_back_named() {
        let missing = PathBuf::from("/definitely/not/here/unmounted-repo");
        let scanner = Scanner::new(config_with_repo(&missing));

        let report = scanner.scan_all_reporting(ScanOptions::fast()).unwrap();

        assert!(report.repos.is_empty());
        assert_eq!(report.unreadable.len(), 1);
        assert_eq!(report.unreadable[0].path, missing);
        assert!(
            !report.unreadable[0].reason.is_empty(),
            "offline has to be tellable from gone"
        );
    }

    /// No git means no scan could have worked, so it is one error rather than a
    /// failure filed against every repository — and never an empty list, which
    /// is what the app used to render for it.
    #[test]
    fn git_being_absent_is_an_error_rather_than_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let scanner =
            Scanner::new(config_with_repo(dir.path())).with_git(Git::with_program("yawm-no-git"));

        let failure = scanner.scan_all_reporting(ScanOptions::fast());

        assert!(matches!(failure, Err(Error::GitMissing)), "got {failure:?}");
    }

    /// The same absence with nothing configured: still an error, because "you
    /// have no repositories" is a claim yawm is in no position to make.
    #[test]
    fn git_being_absent_is_reported_even_with_nothing_configured() {
        let scanner = Scanner::new(Config::default()).with_git(Git::with_program("yawm-no-git"));

        assert!(matches!(
            scanner.scan_all_reporting(ScanOptions::fast()),
            Err(Error::GitMissing)
        ));
    }
}
