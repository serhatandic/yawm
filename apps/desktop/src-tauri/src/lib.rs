//! The desktop shell.
//!
//! Deliberately thin: every command here forwards to `yawm-core` and converts
//! its result for the frontend. No classification, parsing, or filesystem
//! policy lives in this crate — that is what keeps the GUI and the `yawm` CLI
//! from ever disagreeing about which worktrees are disposable.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Manager;
use yawm_core::api::{LandingWorkLimiter, ScanReport};
use yawm_core::config::{Config, ConfigState};
use yawm_core::git::Git;
use yawm_core::git::collect::{
    list_worktrees, load_context, load_context_with_cache, uncommitted_for,
};
use yawm_core::model::WorktreeEntry;
use yawm_core::ops::{
    self, CompletedRemoval, RemovalOutcome, RemovalPlan, RemovalRequest, RemoveOptions, create,
};
use yawm_core::{Error, LandingCache, RepoReport, ScanOptions, Scanner, SizeCache};

/// Errors crossing the boundary to the frontend, as plain strings.
type CmdResult<T> = Result<T, String>;

fn to_message(err: impl std::fmt::Display) -> String {
    err.to_string()
}

/// A failure the frontend has to tell apart from the others.
///
/// Most failures can be a sentence, because they lead to the same place: show
/// it and stop. Two do not. `PlanChanged` means nothing was deleted and the
/// dialog re-plans and asks again — and it carries the worktrees the repository
/// still has, so the re-plan is aimed at what is there now rather than at a
/// list the app painted earlier. `Partial` means some of the selection is
/// already gone and cannot come back, which is the one thing that must never
/// reach the user as "it failed": the dialog has to reconcile what did happen.
/// The frontend used to find `PlanChanged` by searching the message for a
/// phrase, which worked and made a user-facing sentence load-bearing.
#[derive(Debug, Serialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
enum Failure {
    PlanChanged {
        message: String,
        path: PathBuf,
        /// What differs, in the user's terms.
        changes: Vec<String>,
        /// Every worktree the repository still has, from the snapshot the
        /// refusal was decided on. The caller intersects it with its selection.
        still_present: Vec<PathBuf>,
    },
    /// Some worktrees were removed and then the batch failed.
    Partial {
        message: String,
        /// What did happen, in the order it happened. Never empty — a failure
        /// before the first removal keeps its own kind, which is what makes
        /// `PlanChanged` mean exactly "nothing was deleted".
        completed: Vec<CompletedRemoval>,
        /// Worktrees that disappeared before yawm reached them.
        ///
        /// Removed by something other than yawm while the batch ran. Kept
        /// apart from `completed` so the dialog can say the list moved under
        /// it rather than crediting yawm with a deletion it did not perform.
        vanished: Vec<PathBuf>,
        /// The worktree whose removal failed.
        failed: PathBuf,
    },
    /// The batch removed nothing, and some of its worktrees are gone anyway.
    ///
    /// `Partial` with an empty `completed` would say the same thing, but its
    /// `completed` is what makes it mean "yawm deleted some of this", so this
    /// is its own kind. The dialog reconciles the vanished paths exactly as it
    /// does there — rows dropped, tabs closed — and reports no removals.
    Vanished {
        message: String,
        /// Worktrees that were gone before yawm attempted anything. Never
        /// empty.
        vanished: Vec<PathBuf>,
        /// The worktree whose removal failed.
        failed: PathBuf,
    },
    Failed {
        message: String,
    },
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::PlanChanged {
                path,
                changes,
                still_present,
            } => Failure::PlanChanged {
                message,
                path,
                changes,
                still_present,
            },
            Error::BatchIncomplete(partial) => Failure::Partial {
                message,
                completed: partial.completed,
                vanished: partial.vanished,
                failed: partial.failed,
            },
            Error::BatchVanished(gone) => Failure::Vanished {
                message,
                vanished: gone.vanished,
                failed: gone.failed,
            },
            _ => Failure::Failed { message },
        }
    }
}

/// The settings, with the revision they were read at.
///
/// The revision is what lets a save say "this is the copy I was editing". It
/// counts changes within one run and is never persisted: a stale write is only
/// possible against a config that is live in this process.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VersionedConfig {
    config: Config,
    revision: u64,
}

/// What became of a save.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
enum SaveOutcome {
    Saved {
        revision: u64,
    },
    /// The settings changed after this copy was read, so it was not written.
    ///
    /// Carries the current settings so the caller can rebase what the user
    /// actually edited onto them and try again, rather than making them
    /// reconstruct their changes from memory.
    Stale {
        revision: u64,
        config: Config,
    },
}

struct Settings {
    config: Config,
    revision: u64,
}

/// Expensive proofs need a throughput limit, but speculation cannot own the
/// lane a click needs. Separate lanes cap background work at one while allowing
/// one foreground proof to pass it, and the generation abandons queued guesses.
#[derive(Debug, Default)]
struct LandingScheduler {
    foreground: Mutex<()>,
    speculative: Mutex<()>,
    foreground_demand: AtomicUsize,
    generation: AtomicU64,
}

struct ForegroundDemand<'a>(&'a AtomicUsize);

impl Drop for ForegroundDemand<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LandingScheduler {
    fn demand(&self) -> ForegroundDemand<'_> {
        self.foreground_demand.fetch_add(1, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        ForegroundDemand(&self.foreground_demand)
    }

    fn foreground<T>(&self, work: impl FnOnce() -> T) -> T {
        let _demand = self.demand();
        let _slot = self.foreground.lock().expect("foreground landing slot");
        work()
    }

    fn speculation_token(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn speculative<T>(&self, token: u64, work: impl FnOnce() -> T) -> Option<T> {
        let _slot = self.speculative.try_lock().ok()?;
        if self.foreground_demand.load(Ordering::SeqCst) != 0
            || self.generation.load(Ordering::SeqCst) != token
        {
            return None;
        }
        Some(work())
    }
}

#[derive(Debug)]
struct ForegroundLanding(Arc<LandingScheduler>);

impl LandingWorkLimiter for ForegroundLanding {
    fn run(&self, work: &mut dyn FnMut()) {
        self.0.foreground(work);
    }
}

#[derive(Debug)]
struct SpeculativeLanding(Arc<LandingScheduler>);

impl LandingWorkLimiter for SpeculativeLanding {
    fn run(&self, work: &mut dyn FnMut()) {
        let token = self.0.speculation_token();
        self.0.speculative(token, work);
    }
}

/// Application state: just the settings, guarded for concurrent commands.
struct AppState {
    settings: Mutex<Settings>,
    config_path: Option<PathBuf>,
    /// How the settings on disk were come by, for the UI to report.
    config_state: ConfigState,
    landing_cache: LandingCache,
    size_cache: SizeCache,
    landing_scheduler: Arc<LandingScheduler>,
}

impl AppState {
    fn load() -> Self {
        Self::from_path(Config::default_path())
    }

    fn from_path(config_path: Option<PathBuf>) -> Self {
        let loaded = config_path
            .as_ref()
            .map(|p| Config::load_reporting(p))
            .unwrap_or_else(|| yawm_core::config::LoadedConfig {
                config: Config::default(),
                state: ConfigState::Missing,
            });

        let state = Self {
            settings: Mutex::new(Settings {
                config: loaded.config,
                revision: 0,
            }),
            config_path,
            config_state: loaded.state,
            landing_cache: LandingCache::default(),
            size_cache: SizeCache::default(),
            landing_scheduler: Arc::new(LandingScheduler::default()),
        };

        // `Config::load` migrates a pre-workspaces file in memory. Writing it
        // back straight away means the file on disk matches what the app is
        // actually using, rather than staying in a half-upgraded shape until
        // the next unrelated change happens to save it.
        //
        // Only when the file was read, though. What is in memory after a failed
        // load is a default config that resembles nobody's settings, and this
        // write-back is unconditional and immediate — between them, one field
        // of the wrong type was enough to delete every configured repository
        // before the user had touched anything.
        if state.config_state.is_usable() {
            let _ = state.persist();
        }
        state
    }

    fn snapshot(&self) -> Config {
        self.settings.lock().expect("config lock").config.clone()
    }

    fn versioned(&self) -> VersionedConfig {
        let settings = self.settings.lock().expect("config lock");
        VersionedConfig {
            config: settings.config.clone(),
            revision: settings.revision,
        }
    }

    /// Change the settings, and count the change.
    fn mutate<T>(&self, change: impl FnOnce(&mut Config) -> T) -> T {
        let mut settings = self.settings.lock().expect("config lock");
        let out = change(&mut settings.config);
        settings.revision += 1;
        out
    }

    /// Replace the settings wholesale, unless they moved on since `expected`.
    fn replace(&self, mut incoming: Config, expected: Option<u64>) -> SaveOutcome {
        let mut settings = self.settings.lock().expect("config lock");

        if let Some(expected) = expected
            && expected != settings.revision
        {
            return SaveOutcome::Stale {
                revision: settings.revision,
                config: settings.config.clone(),
            };
        }

        // Settings a newer yawm wrote never reach the frontend, so they come
        // from the copy that was read off disk rather than from what is being
        // sent back.
        incoming.carry_unknown_from(&settings.config);
        settings.config = incoming;
        settings.revision += 1;
        SaveOutcome::Saved {
            revision: settings.revision,
        }
    }

    fn persist(&self) -> CmdResult<()> {
        let Some(path) = &self.config_path else {
            return Ok(());
        };
        let config = self.snapshot();
        config.save(path).map_err(to_message)
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> VersionedConfig {
    state.versioned()
}

/// Whether the settings in use are the user's own.
///
/// Returned separately from the settings themselves because it says something
/// about the file rather than about any setting: after a failed load the app is
/// running on defaults that look exactly like a first run.
#[tauri::command]
fn config_status(state: tauri::State<'_, AppState>) -> ConfigState {
    state.config_state.clone()
}

/// Save the settings. `revision` is the one they were read at.
///
/// A tab that loaded the settings and stayed open while a repository was added
/// elsewhere held a copy that no longer described anything; writing it put the
/// addition back where it came from, silently. With the revision, that write is
/// refused and the caller is handed what is actually stored.
#[tauri::command]
fn set_config(
    state: tauri::State<'_, AppState>,
    config: Config,
    revision: Option<u64>,
) -> CmdResult<SaveOutcome> {
    let outcome = state.replace(config, revision);
    if matches!(outcome, SaveOutcome::Stale { .. }) {
        return Ok(outcome);
    }
    state.persist()?;
    Ok(outcome)
}

/// Add a repository to a workspace. `workspace` defaults to the active one.
#[tauri::command]
fn add_repo(
    state: tauri::State<'_, AppState>,
    path: PathBuf,
    workspace: Option<String>,
) -> CmdResult<bool> {
    let added = state.mutate(|config| config.add_repo_to(workspace.as_deref(), path));
    state.persist()?;
    Ok(added)
}

/// Add a folder to search, in a workspace. Defaults to the active one.
#[tauri::command]
fn add_scan_root(
    state: tauri::State<'_, AppState>,
    path: PathBuf,
    workspace: Option<String>,
) -> CmdResult<bool> {
    let added = state.mutate(|config| config.add_scan_root_to(workspace.as_deref(), path));
    state.persist()?;
    Ok(added)
}

/// Remove a repository or scan root. With no workspace, removes it wherever
/// it is found.
#[tauri::command]
fn remove_repo(
    state: tauri::State<'_, AppState>,
    path: PathBuf,
    workspace: Option<String>,
) -> CmdResult<bool> {
    let removed = state.mutate(|config| config.remove_source(workspace.as_deref(), &path));
    state.persist()?;
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Workspaces
// ---------------------------------------------------------------------------

#[tauri::command]
fn create_workspace(state: tauri::State<'_, AppState>, name: String) -> CmdResult<String> {
    let id = state.mutate(|config| config.create_workspace(name));
    state.persist()?;
    Ok(id)
}

#[tauri::command]
fn rename_workspace(
    state: tauri::State<'_, AppState>,
    id: String,
    name: String,
) -> CmdResult<bool> {
    let ok = state.mutate(|config| config.rename_workspace(&id, name));
    state.persist()?;
    Ok(ok)
}

/// Remove a workspace from the config. Never touches anything on disk.
#[tauri::command]
fn delete_workspace(state: tauri::State<'_, AppState>, id: String) -> CmdResult<bool> {
    let ok = state.mutate(|config| config.delete_workspace(&id));
    state.persist()?;
    Ok(ok)
}

/// Choose which workspace is in view. `None` shows every workspace at once.
#[tauri::command]
fn set_active_workspace(state: tauri::State<'_, AppState>, id: Option<String>) -> CmdResult<()> {
    state.mutate(|config| config.active_workspace = id);
    state.persist()
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

/// Everything a scan needs, detached from `tauri::State` so it can move onto a
/// blocking thread.
struct PreparedScan {
    config: Config,
    landing_cache: LandingCache,
    size_cache: SizeCache,
    landing_scheduler: Arc<LandingScheduler>,
    full: bool,
}

impl PreparedScan {
    fn run<T>(self, scan: impl FnOnce(&Scanner, ScanOptions) -> CmdResult<T>) -> CmdResult<T> {
        let hide_main = self.config.hide_main_worktrees;

        let scanner = Scanner::with_caches(self.config, self.landing_cache, self.size_cache)
            .with_landing_limiter(Arc::new(SpeculativeLanding(self.landing_scheduler)));
        let mut options = if self.full {
            ScanOptions::default()
        } else {
            ScanOptions::fast()
        };
        // The one place a display preference is allowed to reach the scanner.
        // Hiding main worktrees is not cosmetic here: they are the bulk of the
        // bytes on disk and none of the rows, so measuring them is time spent
        // on a number that is thrown away before it is drawn.
        options.skip_main_size = hide_main;
        scan(&scanner, options)
    }
}

impl AppState {
    fn prepare_scan(&self, full: bool) -> PreparedScan {
        PreparedScan {
            config: self.snapshot(),
            landing_cache: self.landing_cache.clone(),
            size_cache: self.size_cache.clone(),
            landing_scheduler: Arc::clone(&self.landing_scheduler),
            full,
        }
    }
}

/// Scan every known repository.
///
/// `full` selects whether the disk and process phases run: the UI asks for a
/// fast scan first so the list paints immediately, then fills those signals
/// before its row-by-row landing pass.
///
/// Returns what could not be read alongside what could. An error here means no
/// scan was possible at all — git is not installed — which is the one case
/// where an empty list would be a lie rather than an answer.
#[tauri::command]
async fn scan_all(state: tauri::State<'_, AppState>, full: bool) -> CmdResult<ScanReport> {
    let scan = state.prepare_scan(full);
    tauri::async_runtime::spawn_blocking(move || {
        scan.run(|scanner, options| scanner.scan_all_reporting(options).map_err(to_message))
    })
    .await
    .map_err(to_message)?
}

/// Scan a single repository, used after an operation changes one.
#[tauri::command]
async fn scan_repo(
    state: tauri::State<'_, AppState>,
    path: PathBuf,
    full: bool,
) -> CmdResult<RepoReport> {
    let scan = state.prepare_scan(full);
    tauri::async_runtime::spawn_blocking(move || {
        scan.run(|scanner, options| scanner.scan_repo(&path, options).map_err(to_message))
    })
    .await
    .map_err(to_message)?
}

/// Run the historical proof immediately when the user opens one worktree.
#[tauri::command]
async fn inspect_worktree(
    state: tauri::State<'_, AppState>,
    repo: PathBuf,
    worktree: PathBuf,
) -> CmdResult<yawm_core::Worktree> {
    let config = state.snapshot();
    let landing = state.landing_cache.clone();
    let sizes = state.size_cache.clone();
    let scheduler = Arc::clone(&state.landing_scheduler);
    tauri::async_runtime::spawn_blocking(move || {
        let _demand = scheduler.demand();
        Scanner::with_caches(config, landing, sizes)
            .with_landing_limiter(Arc::new(ForegroundLanding(Arc::clone(&scheduler))))
            .inspect_worktree(&repo, &worktree)
            .map_err(to_message)
    })
    .await
    .map_err(to_message)?
}

/// Resolve one row on the background lane so the list can merge each answer as
/// it lands without letting the pass own the lane an interaction needs.
#[tauri::command]
async fn resolve_landing(
    state: tauri::State<'_, AppState>,
    repo: PathBuf,
    worktree: PathBuf,
) -> CmdResult<Option<yawm_core::Worktree>> {
    let config = state.snapshot();
    let landing = state.landing_cache.clone();
    let sizes = state.size_cache.clone();
    let scheduler = Arc::clone(&state.landing_scheduler);
    let token = scheduler.speculation_token();
    tauri::async_runtime::spawn_blocking(move || {
        let scanner = Scanner::with_caches(config, landing, sizes);
        match scheduler.speculative(token, || scanner.resolve_worktree_landing(&repo, &worktree)) {
            Some(result) => result.map(Some).map_err(to_message),
            None => Ok(None),
        }
    })
    .await
    .map_err(to_message)?
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// Describe what removing worktrees would cost, before anything is deleted.
///
/// Plans for the whole selection in one call. The dialog used to ask per
/// worktree, which repeated the worktree listing and the branch listing once
/// per selected row for answers that are identical across the repository.
///
/// Takes no landing slot, holds no cache, and reads no configuration: a plan is
/// uncommitted work, unpushed commits, files that are not in git, and processes
/// running inside. It used to run behind a full inspection — a recursive disk
/// walk for a size and a historical containment proof — and then use neither.
/// Both are still exactly where they were for the detail panel, which shows
/// them; this path is what the dialog needs and nothing more.
#[tauri::command]
async fn plan_removals(repo: PathBuf, worktrees: Vec<PathBuf>) -> CmdResult<Vec<RemovalPlan>> {
    tauri::async_runtime::spawn_blocking(move || {
        ops::plan_removals(&Git::new(), &repo, &worktrees).map_err(to_message)
    })
    .await
    .map_err(to_message)?
}

/// Remove a worktree. `plan` must be one the user has been shown.
///
/// Reports what became of the branch, because git declining to delete an
/// unmerged one is a good outcome the user has no other way of learning about.
///
/// Kept for callers removing exactly one worktree. Anything removing a
/// selection must use [`remove_worktrees`], which validates the whole set
/// before it touches any of it.
#[tauri::command]
async fn remove_worktree(
    repo: PathBuf,
    plan: RemovalPlan,
    options: RemoveOptions,
) -> Result<RemovalOutcome, Failure> {
    tauri::async_runtime::spawn_blocking(move || {
        ops::remove_reporting(&Git::new(), &repo, &plan, options).map_err(Failure::from)
    })
    .await
    .map_err(|e| Failure::Failed {
        message: to_message(e),
    })?
}

/// Remove a selection of worktrees, or none of them.
///
/// One command rather than a loop in the dialog, because the guarantee only
/// exists if it is made in one place: every plan is re-checked against the
/// repository before the first directory is touched. Looping over
/// `remove_worktree` deleted the worktrees that were still valid and only then
/// discovered that a later one had changed, so the user was shown a refusal
/// while a deletion had already happened.
///
/// `Failure::PlanChanged` crosses intact, so the dialog can still tell "look
/// again" apart from "it broke" — and it now means it for the whole selection.
/// `Failure::Partial` crosses for the case that cannot be undone: some of the
/// selection is gone and the dialog has to say so rather than report a failure
/// that reads as "nothing happened".
#[tauri::command]
async fn remove_worktrees(
    repo: PathBuf,
    requests: Vec<RemovalRequest>,
) -> Result<Vec<RemovalOutcome>, Failure> {
    tauri::async_runtime::spawn_blocking(move || {
        ops::remove_all(&Git::new(), &repo, &requests).map_err(Failure::from)
    })
    .await
    .map_err(|e| Failure::Failed {
        message: to_message(e),
    })?
}

#[tauri::command]
async fn prune_repo(repo: PathBuf) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || ops::prune(&Git::new(), &repo).map_err(to_message))
        .await
        .map_err(to_message)?
}

/// The largest patch worth sending to the UI.
///
/// The old 512 KB cap existed because the naive renderer drew every line. The
/// diff view now virtualises, so the limit only needs to stop a pathological
/// patch from crossing the IPC boundary.
const MAX_PATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_FOCUSED_PATCH_BYTES: usize = 2 * 1024 * 1024;

/// What a worktree changed relative to the repository's default branch.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DiffResult {
    summary: yawm_core::diff::DiffSummary,
    patches: yawm_core::diff::Patches,
    uncommitted: yawm_core::UncommittedAnalysis,
}

fn worktree_input(repo: &Path, worktree: &Path) -> CmdResult<(Git, Vec<WorktreeEntry>, usize)> {
    let git = Git::new();
    let entries = list_worktrees(&git, repo).map_err(to_message)?;
    let index = entries
        .iter()
        .position(|entry| {
            yawm_core::path::path_key(&entry.path) == yawm_core::path::path_key(worktree)
        })
        .ok_or_else(|| {
            format!(
                "{} is not a worktree of this repository",
                worktree.display()
            )
        })?;
    Ok((git, entries, index))
}

fn focused_patch(
    repo: &Path,
    worktree: &Path,
    cache: LandingCache,
) -> CmdResult<yawm_core::git::landing::FocusedPatch> {
    let (git, entries, index) = worktree_input(repo, worktree)?;
    let ctx = load_context_with_cache(&git, repo, &entries, cache).map_err(to_message)?;
    Ok(match entries[index].head.as_deref() {
        Some(head) => ctx.focused_patch_for_head_oid(&git, head, MAX_FOCUSED_PATCH_BYTES),
        None => yawm_core::git::landing::FocusedPatch::All {
            reason: yawm_core::git::landing::AllChangesReason::Unsafe,
        },
    })
}

#[tauri::command]
async fn diff_worktree(
    repo: PathBuf,
    worktree: PathBuf,
    scope: Option<yawm_core::diff::DiffScope>,
) -> CmdResult<DiffResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let scope = scope.unwrap_or_default();
        let (git, entries, index) = worktree_input(&repo, &worktree)?;
        let ctx = load_context(&git, &repo, &entries).map_err(to_message)?;
        let base = ctx.default_ref.as_deref();
        let inspection =
            yawm_core::diff::inspect(&git, &entries[index], base, MAX_PATCH_BYTES, scope)
                .map_err(to_message)?;
        let uncommitted = uncommitted_for(&git, &entries[index], &ctx);
        Ok(DiffResult {
            summary: inspection.summary,
            patches: inspection.patches,
            uncommitted,
        })
    })
    .await
    .map_err(to_message)?
}

#[tauri::command]
async fn focused_worktree(
    state: tauri::State<'_, AppState>,
    repo: PathBuf,
    worktree: PathBuf,
) -> CmdResult<yawm_core::git::landing::FocusedPatch> {
    let cache = state.landing_cache.clone();
    let scheduler = Arc::clone(&state.landing_scheduler);
    tauri::async_runtime::spawn_blocking(move || {
        scheduler.foreground(|| focused_patch(&repo, &worktree, cache))
    })
    .await
    .map_err(to_message)?
}

#[tauri::command]
async fn prefetch_focused_worktree(
    state: tauri::State<'_, AppState>,
    repo: PathBuf,
    worktree: PathBuf,
) -> CmdResult<()> {
    let cache = state.landing_cache.clone();
    let scheduler = Arc::clone(&state.landing_scheduler);
    let token = scheduler.speculation_token();
    tauri::async_runtime::spawn_blocking(move || {
        scheduler.speculative(token, || focused_patch(&repo, &worktree, cache));
    })
    .await
    .map_err(to_message)
}

// ---------------------------------------------------------------------------
// Creating worktrees
// ---------------------------------------------------------------------------

/// Where a new worktree would go, given the configured path template.
#[tauri::command]
fn suggest_worktree_path(
    state: tauri::State<'_, AppState>,
    repo: PathBuf,
    branch: String,
) -> PathBuf {
    let template = state.snapshot().worktree_path_template;
    create::expand_template(&template, &repo, &branch)
}

/// What creating this worktree would involve, including what to carry over.
#[tauri::command]
async fn plan_creation(
    repo: PathBuf,
    branch: String,
    base: String,
    path: PathBuf,
) -> CmdResult<create::CreatePlan> {
    tauri::async_runtime::spawn_blocking(move || {
        create::plan(&Git::new(), &repo, &branch, &base, &path).map_err(to_message)
    })
    .await
    .map_err(to_message)?
}

/// Create the worktree. Returns the names of everything actually provisioned.
#[tauri::command]
async fn create_worktree(repo: PathBuf, options: create::CreateOptions) -> CmdResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        create::create(&Git::new(), &repo, &options).map_err(to_message)
    })
    .await
    .map_err(to_message)?
}

/// Refs worth offering as a starting point, most useful first.
#[tauri::command]
async fn list_base_refs(repo: PathBuf) -> CmdResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let git = Git::new();
        let entries = list_worktrees(&git, &repo).map_err(to_message)?;
        let ctx = load_context(&git, &repo, &entries).map_err(to_message)?;

        let mut refs: Vec<String> = ctx.merge_refs.clone();
        for branch in ctx.branches().keys() {
            if !refs.iter().any(|r| r == branch) {
                refs.push(branch.clone());
            }
        }
        Ok(refs)
    })
    .await
    .map_err(to_message)?
}

#[tauri::command]
async fn reveal_path(path: PathBuf) -> CmdResult<()> {
    ops::reveal(&path).map_err(to_message)
}

#[tauri::command]
async fn open_in_editor(state: tauri::State<'_, AppState>, path: PathBuf) -> CmdResult<()> {
    let editor = state.snapshot().editor;
    ops::open_with(&path, editor.as_deref()).map_err(to_message)
}

/// Editors installed on this machine, for the Open menu.
///
/// Detection is by presence rather than by launching anything, so this is
/// cheap enough to call whenever the panel opens.
#[tauri::command]
async fn list_editors() -> CmdResult<Vec<ops::editors::Editor>> {
    Ok(ops::editors::detect())
}

/// Remember which editor Open should use from now on.
#[tauri::command]
async fn set_editor(state: tauri::State<'_, AppState>, command: Option<String>) -> CmdResult<()> {
    state.mutate(|config| config.editor = command);
    state.persist()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's DMA-BUF renderer produces a blank window on several NVIDIA
    // driver versions. Disabling it costs a little compositing performance and
    // avoids an unusable window, so it is set before the webview starts rather
    // than left for users to discover.
    #[cfg(target_os = "linux")]
    if is_nvidia() && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: single-threaded, before any webview or GTK initialisation.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(AppState::load());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_config,
            config_status,
            add_repo,
            remove_repo,
            add_scan_root,
            create_workspace,
            rename_workspace,
            delete_workspace,
            set_active_workspace,
            scan_all,
            scan_repo,
            inspect_worktree,
            resolve_landing,
            plan_removals,
            remove_worktree,
            remove_worktrees,
            prune_repo,
            diff_worktree,
            focused_worktree,
            prefetch_focused_worktree,
            suggest_worktree_path,
            plan_creation,
            create_worktree,
            list_base_refs,
            reveal_path,
            open_in_editor,
            list_editors,
            set_editor,
        ])
        .run(tauri::generate_context!())
        .expect("error while running yawm");
}

/// Best-effort detection of an NVIDIA GPU.
#[cfg(target_os = "linux")]
fn is_nvidia() -> bool {
    // Reading sysfs avoids depending on lspci being installed.
    std::fs::read_dir("/sys/module")
        .map(|entries| {
            entries.flatten().any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name == "nvidia" || name.starts_with("nvidia_")
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn write(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn backend_trust_facts_cross_the_tauri_boundary_structurally() {
        use yawm_core::model::{
            DirtyCounts, HeadState, LandingFacts, LandingTargetFact, LockInfo,
            ManagedDependencyLink, ProofPhase, UnknownReason, UpstreamState, VerdictReason,
            WorktreeStatus,
        };

        let status = WorktreeStatus {
            dirty: DirtyCounts {
                staged: 1,
                unstaged: 1,
                untracked: 2,
                paths: 3,
                inspection_failed: false,
            },
            main_worktree_env_files: vec![".env.local".into()],
            managed_dependency_links: vec![ManagedDependencyLink {
                path: "node_modules".into(),
                target: "/code/main/node_modules".into(),
            }],
            landing_facts: LandingFacts {
                selected_target: Some(LandingTargetFact {
                    name: "origin/trunk".into(),
                    oid: Some("0123456789abcdef".into()),
                    short_oid: Some("0123456789ab".into()),
                }),
                commits_ahead: Some(3),
                head: HeadState::Orphan {
                    branch: "feat/x".into(),
                    oid: "fedcba9876543210".into(),
                },
                upstream: UpstreamState::Gone {
                    name: "origin/feat/x".into(),
                    full_ref: Some("refs/remotes/origin/feat/x".into()),
                },
                unknown_reason: Some(UnknownReason::GitCommandFailed {
                    phase: ProofPhase::History,
                }),
                proof_phase: Some(ProofPhase::History),
                ..LandingFacts::default()
            },
            ..WorktreeStatus::default()
        };
        let reason = VerdictReason::DirectoryMissing {
            detail: Some("gitdir file is stale".into()),
            lock: Some(LockInfo {
                reason: Some("agent running".into()),
            }),
        };
        let wire = serde_json::json!({ "status": status, "reason": reason });

        assert_eq!(wire["status"]["dirty"]["paths"], 3);
        assert_eq!(wire["status"]["mainWorktreeEnvFiles"][0], ".env.local");
        assert_eq!(
            wire["status"]["managedDependencyLinks"][0]["target"],
            "/code/main/node_modules"
        );
        assert_eq!(
            wire["status"]["landingFacts"]["selectedTarget"]["name"],
            "origin/trunk"
        );
        assert_eq!(wire["status"]["landingFacts"]["commitsAhead"], 3);
        assert_eq!(wire["status"]["landingFacts"]["head"]["state"], "orphan");
        assert_eq!(wire["status"]["landingFacts"]["upstream"]["state"], "gone");
        assert_eq!(
            wire["status"]["landingFacts"]["unknownReason"]["phase"],
            "history"
        );
        assert_eq!(wire["reason"]["kind"], "directoryMissing");
        assert_eq!(wire["reason"]["lock"]["reason"], "agent running");
    }

    #[test]
    fn foreground_landing_never_waits_for_a_full_list_pass() {
        const WORKTREES: usize = 21;
        let scheduler = Arc::new(LandingScheduler::default());
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let speculative = Arc::clone(&scheduler);
        let speculative_thread = std::thread::spawn(move || {
            let token = speculative.speculation_token();
            for index in 0..WORKTREES {
                let completed = speculative.speculative(token, || {
                    if index == 0 {
                        started_tx.send(()).unwrap();
                        release_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                    }
                });
                if completed.is_none() {
                    break;
                }
            }
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let (done_tx, done_rx) = mpsc::channel();
        let foreground = Arc::clone(&scheduler);
        let foreground_thread = std::thread::spawn(move || {
            foreground.foreground(|| done_tx.send(()).unwrap());
        });
        let prompt = done_rx.recv_timeout(Duration::from_millis(100));

        release_tx.send(()).unwrap();
        speculative_thread.join().unwrap();
        foreground_thread.join().unwrap();
        assert!(
            prompt.is_ok(),
            "foreground landing queued behind speculative work"
        );
    }

    #[test]
    fn foreground_demand_abandons_the_rest_of_a_speculative_batch() {
        let scheduler = LandingScheduler::default();
        let token = scheduler.speculation_token();
        scheduler.foreground(|| {});

        assert!(
            scheduler.speculative(token, || ()).is_none(),
            "a stale speculative queue continued after foreground demand"
        );
    }

    /// The pair that destroyed configurations: a load that could not read the
    /// file answered with defaults, and startup wrote those defaults straight
    /// back over it. One field of the wrong type was enough.
    #[test]
    fn a_config_that_could_not_be_read_is_never_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = r#"{"workspaces":[{"id":"w","name":"Work",
            "repos":["/code/alpha","/code/beta"],"scanRoots":["/code"]}],
            "scanDepth":"four"}"#;
        write(&path, original);

        let state = AppState::from_path(Some(path.clone()));

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "the user's only copy of their repositories must still be there"
        );
        assert!(!state.config_state.is_usable());
        let ConfigState::Unusable { backup, .. } = &state.config_state else {
            panic!("a file that could not be parsed must not read as loaded");
        };
        assert!(backup.is_some(), "and a copy must exist before any rewrite");
    }

    /// The write-back itself is worth keeping: a pre-workspaces file is
    /// migrated in memory and the file should stop being half-upgraded.
    #[test]
    fn a_config_that_was_read_is_still_migrated_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        write(&path, r#"{"repos":["/code/alpha"],"editor":"zed"}"#);

        AppState::from_path(Some(path.clone()));

        let text = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["workspaces"][0]["repos"][0], "/code/alpha");
        assert_eq!(json["editor"], "zed");
    }

    /// The Settings tab loads once and stays mounted while hidden, so its copy
    /// of the settings goes stale the moment a repository is added anywhere
    /// else. Writing that copy back used to erase the addition.
    #[test]
    fn a_save_from_a_stale_copy_is_refused_rather_than_applied() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::from_path(Some(dir.path().join("config.json")));

        let opened = state.versioned();
        let mut edited = opened.config.clone();
        edited.editor = Some("cursor".into());

        // A repository is added from somewhere else while the tab sits open.
        let workspace = opened.config.workspaces[0].id.clone();
        state.mutate(|config| config.add_repo_to(Some(&workspace), "/code/added".into()));

        let outcome = state.replace(edited.clone(), Some(opened.revision));

        let SaveOutcome::Stale { config, revision } = outcome else {
            panic!("a save against a revision that has moved on must be refused");
        };
        assert_eq!(
            config.workspaces[0].repos,
            vec![PathBuf::from("/code/added")],
            "the addition survives, and comes back for the caller to rebase onto"
        );

        // Rebasing the edit onto what is really stored, and retrying, works.
        let mut rebased = config.clone();
        rebased.editor = edited.editor.clone();
        assert!(matches!(
            state.replace(rebased, Some(revision)),
            SaveOutcome::Saved { .. }
        ));
        let now = state.snapshot();
        assert_eq!(now.editor.as_deref(), Some("cursor"));
        assert_eq!(now.workspaces[0].repos, vec![PathBuf::from("/code/added")]);
    }

    #[test]
    fn a_save_from_the_current_copy_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::from_path(Some(dir.path().join("config.json")));

        let opened = state.versioned();
        let mut edited = opened.config.clone();
        edited.editor = Some("zed".into());

        assert!(matches!(
            state.replace(edited, Some(opened.revision)),
            SaveOutcome::Saved { .. }
        ));
        assert_eq!(state.snapshot().editor.as_deref(), Some("zed"));
    }

    /// The frontend cannot send back settings it does not know exist, so a save
    /// that came through it must not be how a newer version's settings die.
    #[test]
    fn a_save_through_the_frontend_keeps_settings_it_never_saw() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        write(
            &path,
            r#"{"editor":"zed","futureRepositories":["/code/from-the-future"]}"#,
        );
        let state = AppState::from_path(Some(path.clone()));

        let mut from_frontend = state.snapshot();
        from_frontend.extra.clear();
        from_frontend.editor = Some("cursor".into());
        state.replace(from_frontend, None);
        state.persist().unwrap();

        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            json["futureRepositories"],
            serde_json::json!(["/code/from-the-future"])
        );
        assert_eq!(json["editor"], "cursor");
    }

    /// The frontend decides whether to rebase and retry by reading these tags,
    /// and nothing in TypeScript can see the Rust side of the boundary, so the
    /// wire shape is pinned here.
    #[test]
    fn a_save_outcome_crosses_the_boundary_in_the_shape_the_frontend_reads() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::from_path(Some(dir.path().join("config.json")));
        let opened = state.versioned();
        state.mutate(|config| config.editor = Some("zed".into()));

        let saved =
            serde_json::to_value(state.replace(opened.config, Some(opened.revision))).unwrap();
        assert_eq!(saved["outcome"], "stale");
        assert!(saved["config"]["workspaces"].is_array());

        let fresh = state.versioned();
        let ok = serde_json::to_value(state.replace(fresh.config, Some(fresh.revision))).unwrap();
        assert_eq!(ok["outcome"], "saved");
        assert!(ok["revision"].is_u64());

        let versioned = serde_json::to_value(state.versioned()).unwrap();
        assert!(versioned["config"].is_object() && versioned["revision"].is_u64());

        let status = serde_json::to_value(ConfigState::Unusable {
            reason: "scanDepth is not a valid setting".into(),
            backup: Some(PathBuf::from("/c/config.corrupt-1.json")),
        })
        .unwrap();
        assert_eq!(status["state"], "unusable");
        assert_eq!(status["backup"], "/c/config.corrupt-1.json");
    }

    /// `PlanChanged` is not a failure to show and stop on — nothing was deleted
    /// and the dialog re-plans — so it has to be tellable apart without reading
    /// the sentence meant for the user.
    #[test]
    fn a_refusal_to_re_plan_crosses_the_boundary_as_its_own_kind() {
        let failure = Failure::from(Error::PlanChanged {
            path: "/w/feature".into(),
            changes: vec!["new uncommitted files: late.txt".to_string()],
            still_present: vec!["/w/feature".into(), "/w/other".into()],
        });

        let json = serde_json::to_value(&failure).unwrap();
        assert_eq!(json["kind"], "planChanged");
        assert_eq!(json["changes"][0], "new uncommitted files: late.txt");
        assert_eq!(json["path"], "/w/feature");
        assert_eq!(
            json["stillPresent"].as_array().unwrap().len(),
            2,
            "the dialog re-plans on this rather than on the list it painted earlier"
        );
        assert_eq!(json["stillPresent"][1], "/w/other");

        let other = Failure::from(Error::Parse("git said no".into()));
        assert_eq!(serde_json::to_value(&other).unwrap()["kind"], "failed");
    }

    /// A batch refusal names every worktree that changed, and all of them have
    /// to reach the dialog: "one of your five is different" is not something a
    /// user can act on.
    #[test]
    fn a_batch_refusal_carries_every_changed_worktree_across() {
        let failure = Failure::from(Error::PlanChanged {
            path: "/w/alpha".into(),
            changes: vec![
                "alpha: new uncommitted files: late.txt".to_string(),
                "beta: it has been locked: agent running".to_string(),
            ],
            still_present: vec!["/w/alpha".into(), "/w/beta".into()],
        });

        let json = serde_json::to_value(&failure).unwrap();
        assert_eq!(json["kind"], "planChanged");
        assert_eq!(json["changes"].as_array().unwrap().len(), 2);
        assert_eq!(
            json["changes"][1],
            "beta: it has been locked: agent running"
        );
        assert!(
            json["message"]
                .as_str()
                .unwrap()
                .contains("Nothing was deleted"),
            "got {}",
            json["message"]
        );
    }

    /// The one failure that must not read as "nothing happened". Removal cannot
    /// be undone, so what did happen crosses structurally: the dialog closes
    /// those worktrees' tabs and stops listing them, and only the rest is still
    /// the user's to decide about.
    #[test]
    fn a_batch_that_deleted_something_and_then_failed_says_what_went() {
        use yawm_core::ops::{BranchOutcome, PartialRemoval, RemovalStatus};

        let failure = Failure::from(Error::BatchIncomplete(Box::new(PartialRemoval {
            completed: vec![
                CompletedRemoval::removed(
                    "/w/alpha".into(),
                    RemovalOutcome {
                        branch: BranchOutcome::Kept,
                    },
                ),
                CompletedRemoval {
                    path: "/w/beta".into(),
                    outcome: RemovalOutcome {
                        branch: BranchOutcome::RollbackFailed,
                    },
                    status: RemovalStatus::RemovedButFinalizationFailed,
                },
            ],
            vanished: vec!["/w/gamma".into()],
            failed: "/w/beta".into(),
            cause: Box::new(Error::Parse("git refused".into())),
        })));

        let json = serde_json::to_value(&failure).unwrap();
        assert_eq!(json["kind"], "partial");
        assert_eq!(json["completed"][0]["path"], "/w/alpha");
        assert_eq!(json["completed"][0]["outcome"]["branch"], "kept");
        assert_eq!(json["completed"][0]["status"], "removed");
        // The worktree that stopped the batch is also gone: its directory went
        // to the trash and only the prune that follows failed. Crossing without
        // it would leave the dialog listing a directory that no longer exists.
        assert_eq!(json["completed"][1]["path"], "/w/beta");
        assert_eq!(
            json["completed"][1]["status"],
            "removedButFinalizationFailed"
        );
        assert_eq!(json["completed"][1]["outcome"]["branch"], "rollbackFailed");
        assert_eq!(json["failed"], "/w/beta");
        // Gone, but not by yawm's hand. Crossing it as a completed removal
        // would credit yawm with a deletion it did not perform and hide that
        // something else is writing to this repository.
        assert_eq!(json["vanished"][0], "/w/gamma");
        assert!(
            !json["completed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|done| done["path"] == "/w/gamma"),
            "a worktree yawm never touched is not one of its removals"
        );
        let message = json["message"].as_str().unwrap();
        assert!(message.contains("git refused"), "got {message}");
        assert!(
            !message.contains("Nothing was deleted"),
            "something was deleted: {message}"
        );
    }

    /// The batch command speaks the shape the dialog sends. A per-worktree
    /// option set is the point: one dirty worktree in a selection must not
    /// force the clean ones alongside it.
    #[test]
    fn a_removal_request_carries_its_own_options() {
        let requests: Vec<RemovalRequest> = serde_json::from_value(serde_json::json!([
            {
                "plan": {
                    "path": "/w/dirty",
                    "branch": "feat/x",
                    "isMain": false,
                    "isLocked": true,
                    "lockReason": "agent running",
                    "isPrunable": false,
                    "dirtyFiles": ["a.txt"],
                    "dirtyTotal": 1,
                    "unpushedCommits": 0,
                    "envFiles": [],
                    "runningProcesses": 0,
                    "requiresForce": true,
                    "state": {
                        "version": "yawm.state.v1",
                        "digest": "abc",
                        "unproven": false
                    }
                },
                "options": {
                    "force": true,
                    "deleteBranch": false,
                    "forceBranch": false,
                    "useTrash": false,
                    "unlock": true
                }
            },
            {
                "plan": {
                    "path": "/w/clean",
                    "branch": null,
                    "isMain": false,
                    "isLocked": false,
                    "lockReason": null,
                    "isPrunable": false,
                    "dirtyFiles": [],
                    "dirtyTotal": 0,
                    "unpushedCommits": 0,
                    "envFiles": [],
                    "runningProcesses": 0,
                    "requiresForce": false
                },
                "options": {
                    "force": false,
                    "deleteBranch": false,
                    "forceBranch": false,
                    "useTrash": false,
                    "unlock": false
                }
            }
        ]))
        .expect("the dialog's payload deserialises");

        assert!(requests[0].options.force && requests[0].options.unlock);
        assert_eq!(
            requests[0].plan.lock_reason.as_deref(),
            Some("agent running")
        );
        assert!(!requests[1].options.force && !requests[1].options.unlock);
        // The authorisation crosses the boundary opaquely: a version, a digest
        // over the exact state, and whether that state was established. The
        // per-file identity behind it never leaves core's process.
        assert_eq!(requests[0].plan.state.digest, "abc");
        assert_eq!(requests[0].plan.state.version, "yawm.state.v1");
        assert!(!requests[0].plan.state.unproven);
        assert!(
            requests[0].plan.state.evidence().is_none(),
            "the evidence is not something a webview hands back"
        );
        // A payload with no fingerprint at all still parses, and then fails
        // closed: an empty version is never a state anyone approved.
        assert_eq!(requests[1].plan.state, Default::default());
        assert!(!requests[1].plan.state.is_proven());
    }

    /// The plan is what the dialog holds for every selected worktree and hands
    /// back on confirm. Its size may not depend on how much work is in the
    /// worktree: the fingerprint used to carry every dirty path and every file
    /// outside git, uncapped, so one worktree with ten thousand modified files
    /// put ten thousand records across this boundary and then back again.
    #[test]
    fn a_plans_payload_stays_bounded_however_dirty_the_worktree_is() {
        use yawm_core::ops::{StateEvidence, StateFingerprint};

        let state = |files: usize| {
            serde_json::to_string(
                &serde_json::from_str::<StateFingerprint>(
                    &serde_json::to_string(&sealed_with(files)).unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
        };

        fn sealed_with(files: usize) -> StateFingerprint {
            let mut evidence = StateEvidence::default();
            for index in 0..files {
                evidence.dirty.push(yawm_core::ops::DirtyIdentity {
                    path: format!("a-very-long-source-path/number-{index}.txt"),
                    codes: vec![" M".to_string()],
                    stages: vec![format!("0 100644 {index:040x}")],
                    content: format!("blob:{index:040x}"),
                });
            }
            evidence.seal()
        }

        let small = state(1);
        let huge = state(500);
        assert_eq!(
            small.len(),
            huge.len(),
            "the payload is fixed-size: {small} vs {huge}"
        );
        assert!(small.len() < 200, "and small: {small}");
        // Still an authorisation over all 500 of them, and a different one.
        assert_ne!(sealed_with(500).digest, sealed_with(499).digest);
    }

    /// Nothing yawm deleted, and part of the selection gone anyway. It must
    /// reach the dialog as something it can reconcile, not as prose.
    #[test]
    fn a_batch_that_deleted_nothing_still_names_what_disappeared() {
        use yawm_core::ops::VanishedRemoval;

        let failure = Failure::from(Error::BatchVanished(Box::new(VanishedRemoval {
            vanished: vec!["/w/gamma".into(), "/w/delta".into()],
            failed: "/w/beta".into(),
            cause: Box::new(Error::Parse("git refused".into())),
        })));

        let json = serde_json::to_value(&failure).unwrap();
        assert_eq!(json["kind"], "vanished");
        assert_eq!(json["vanished"].as_array().unwrap().len(), 2);
        assert_eq!(json["vanished"][1], "/w/delta");
        assert_eq!(json["failed"], "/w/beta");
        assert!(
            json["completed"].is_null(),
            "yawm removed nothing, so it claims nothing"
        );
        let message = json["message"].as_str().unwrap();
        assert!(message.contains("git refused"), "got {message}");
        assert!(
            !message.contains("Nothing was deleted"),
            "worktrees are gone, even if yawm did not remove them: {message}"
        );
    }
}
