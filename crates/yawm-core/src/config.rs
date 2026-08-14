//! Persistent settings.
//!
//! Stored as JSON under the platform's standard configuration directory, via
//! the `directories` crate so the location is correct on all three targets
//! without any host-specific code. That matters for the boundary: the GUI and
//! the CLI read the same file with the same code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::path::path_key;
use crate::verdict::VerdictConfig;

/// What to carry into a newly created worktree.
///
/// A fresh worktree has none of the repository's gitignored files, which is the
/// single most reported friction with `git worktree add`. yawm carries them
/// over by default, so these are opt-*out*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProvisioningDefaults {
    /// Copy `.env`, `.env.local`, and friends.
    pub copy_env_files: bool,
    /// Link dependency directories instead of reinstalling them.
    pub link_dependencies: bool,
    /// Honour a repository's `.worktreeinclude`.
    pub honour_worktreeinclude: bool,
}

impl Default for ProvisioningDefaults {
    fn default() -> Self {
        Self {
            copy_env_files: true,
            link_dependencies: true,
            honour_worktreeinclude: true,
        }
    }
}

/// How a diff is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffStyle {
    #[default]
    Unified,
    Split,
}

/// A named group of repositories.
///
/// Without this everything shares one list, so a demo set and real work end up
/// interleaved in the same view — which is exactly what makes the list
/// untrustworthy, since the whole product is "which of these can I delete".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Workspace {
    /// Stable across renames, so the active selection survives one.
    pub id: String,
    pub name: String,
    /// Repositories added by hand.
    pub repos: Vec<PathBuf>,
    /// Folders searched for repositories.
    pub scan_roots: Vec<PathBuf>,
    /// See [`Config::extra`].
    #[serde(flatten)]
    pub extra: Unknown,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: "Workspace".to_string(),
            repos: Vec::new(),
            scan_roots: Vec::new(),
            extra: Unknown::default(),
        }
    }
}

impl Workspace {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Add a repository, ignoring duplicates.
    pub fn add_repo(&mut self, path: PathBuf) -> bool {
        insert_unique(&mut self.repos, path)
    }

    /// Add a scan root, ignoring duplicates.
    pub fn add_scan_root(&mut self, path: PathBuf) -> bool {
        insert_unique(&mut self.scan_roots, path)
    }

    /// Remove a path from either list. Returns whether anything was removed.
    pub fn remove(&mut self, path: &Path) -> bool {
        let key = path_key(path);
        let before = self.repos.len() + self.scan_roots.len();
        self.repos.retain(|p| path_key(p) != key);
        self.scan_roots.retain(|p| path_key(p) != key);
        self.repos.len() + self.scan_roots.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty() && self.scan_roots.is_empty()
    }
}

fn insert_unique(list: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    let key = path_key(&path);
    if list.iter().any(|p| path_key(p) == key) {
        return false;
    }
    list.push(path);
    true
}

/// Identifiers only need to be unique within one config file, so the clock plus
/// a counter is enough and avoids a dependency for cosmetic value.
fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ws-{nanos:x}-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Saves within this process take turns, so two of them cannot be part-way
/// through the same file at once.
///
/// Deliberately not an inter-process lock: the guarantee that matters across
/// processes is that a reader never sees a half-written file, and the unique
/// temp file plus the atomic rename already give that. What a cross-process
/// lock would add is last-writer-wins ordering between the CLI and the app,
/// which is a different problem — see [`Config::save`].
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// A temp name no other save will pick, beside the destination so the rename
/// stays within one filesystem — across filesystems it is a copy, and a copy
/// is not atomic.
fn temp_path_for(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.json".to_string());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let nonce = format!(
        "{}-{nanos:x}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    path.with_file_name(format!(".{name}.tmp-{nonce}"))
}

/// Copy a file yawm could not read, and describe the state it leaves behind.
fn unusable(path: &Path, reason: String) -> ConfigState {
    ConfigState::Unusable {
        reason,
        backup: back_up(path),
    }
}

/// Say what is wrong in terms the user can act on.
///
/// Serde's own message for a mistyped field names the type it wanted and the
/// line it gave up on, never the setting — and a settings file is usually one
/// character from being correct, so naming the field is the difference between
/// a file the user repairs and a file they delete.
fn describe(text: &str, error: &serde_json::Error) -> String {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        return format!("the file is not valid JSON: {error}");
    };

    for (name, value) in fields {
        let alone = serde_json::Value::Object([(name.clone(), value)].into_iter().collect());
        if let Err(e) = serde_json::from_value::<Config>(alone) {
            return format!("{name} is not a valid setting: {e}");
        }
    }
    error.to_string()
}

/// Keep a timestamped copy of the file beside it.
///
/// An existing backup of the same bytes is reused rather than joined by
/// another: a config yawm cannot parse fails to parse on every launch, and a
/// config directory filling with identical copies helps nobody find the one
/// that matters.
fn back_up(path: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(path).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let prefix = format!("{name}.corrupt-");

    if let Ok(entries) = std::fs::read_dir(path.parent()?) {
        for entry in entries.flatten() {
            let existing = entry.file_name();
            let existing = existing.to_string_lossy();
            if existing.starts_with(&prefix)
                && std::fs::read(entry.path()).is_ok_and(|other| other == bytes)
            {
                return Some(entry.path());
            }
        }
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_file_name(format!("{prefix}{stamp}.json"));
    std::fs::write(&backup, &bytes).ok()?;
    Some(backup)
}

/// Settings this version of yawm does not know about.
///
/// Keyed and ordered so a round trip through yawm leaves the file's contents
/// unchanged apart from what the user actually altered.
pub type Unknown = BTreeMap<String, serde_json::Value>;

/// User settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Named groups of repositories. There is always at least one.
    pub workspaces: Vec<Workspace>,
    /// Which workspace is in view. `None` shows every workspace at once.
    pub active_workspace: Option<String>,

    /// Repositories from before workspaces existed.
    ///
    /// Kept only so an older config can be migrated on load rather than
    /// silently losing the user's repositories. Emptied once migrated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<PathBuf>,
    /// Scan roots from before workspaces existed. See [`Config::repos`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scan_roots: Vec<PathBuf>,

    /// How deep to search below each scan root.
    pub scan_depth: usize,
    /// Command used to open a worktree, e.g. `code`. `None` means auto-detect.
    pub editor: Option<String>,
    /// Where new worktrees go. `{repo}` and `{branch}` are substituted.
    pub worktree_path_template: String,
    /// Activity this recent means a worktree is in use.
    pub active_within_minutes: u32,
    /// How diffs are laid out.
    pub diff_style: DiffStyle,
    /// Hide the repositories' own main worktrees from the list.
    ///
    /// They can never be deleted, so on a machine with many repositories they
    /// are most of the rows and none of the choices. Persisted because it is a
    /// standing preference, not a search you retype.
    #[serde(default)]
    pub hide_main_worktrees: bool,
    pub provisioning: ProvisioningDefaults,

    /// Fields written by a version of yawm newer than this one.
    ///
    /// Serde drops what it does not recognise, and the startup write-back then
    /// deletes it from the file — so running an older yawm once permanently
    /// erased settings a newer one had created. Carrying them through the round
    /// trip needs no prediction of what those settings will turn out to be,
    /// which a schema-version gate would.
    #[serde(flatten)]
    pub extra: Unknown,
}

impl Default for Config {
    fn default() -> Self {
        let first = Workspace::named("Personal");
        Self {
            active_workspace: Some(first.id.clone()),
            workspaces: vec![first],
            repos: Vec::new(),
            scan_roots: Vec::new(),
            scan_depth: crate::scan::DEFAULT_MAX_DEPTH,
            editor: None,
            // Siblings of the repository, grouped in one predictable place, so
            // worktrees are neither scattered nor nested inside the repo.
            worktree_path_template: "../{repo}-worktrees/{branch}".to_string(),
            active_within_minutes: 30,
            // Unified reads better in a narrow pane, and yawm's window is not
            // wide by default.
            diff_style: DiffStyle::Unified,
            // Off by default: hiding rows before the user knows they exist
            // would make yawm look like it had missed them.
            hide_main_worktrees: false,
            provisioning: ProvisioningDefaults::default(),
            extra: Unknown::default(),
        }
    }
}

/// What a load found, beyond the settings themselves.
///
/// The distinction the app acts on: defaults are *correct* for a file that was
/// never written, and a *guess* for one that could not be read. Persisting a
/// guess is what turns one malformed field into a deleted configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum ConfigState {
    /// No file yet. This is a first run, and defaults are the settings.
    Missing,
    Loaded,
    /// The file is there and yawm could not use it.
    Unusable {
        /// In the user's terms — usually names the offending field.
        reason: String,
        /// Where the original was copied before anything could overwrite it.
        backup: Option<PathBuf>,
    },
}

impl ConfigState {
    /// Whether what is in memory may be written back over the file.
    pub fn is_usable(&self) -> bool {
        !matches!(self, ConfigState::Unusable { .. })
    }
}

/// Settings, and how honestly they were come by.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub state: ConfigState,
}

impl Config {
    /// Thresholds for the verdict engine, derived from user-facing units.
    pub fn verdict_config(&self) -> VerdictConfig {
        VerdictConfig {
            active_within_secs: i64::from(self.active_within_minutes) * 60,
        }
    }

    /// Standard configuration file location for this platform.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("dev", "yawm", "yawm")
            .map(|dirs| dirs.config_dir().join("config.json"))
    }

    /// Load settings, falling back to defaults when the file cannot be used.
    ///
    /// Refusing to start because a settings file was hand-edited badly would be
    /// worse than starting fresh, so this still yields defaults. Callers that
    /// can act on the difference should use [`Config::load_reporting`]: these
    /// defaults are only safe to *use*, never to write back.
    pub fn load(path: &Path) -> Self {
        Self::load_reporting(path).config
    }

    /// [`Config::load`], saying whether the settings are the user's own.
    ///
    /// A file that exists and cannot be parsed is copied aside first. It is the
    /// user's only record of what they configured and is usually one character
    /// from being recoverable, so it is preserved before anything in this
    /// process gets the chance to rewrite it.
    pub fn load_reporting(path: &Path) -> LoadedConfig {
        let (mut config, state) = match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(config) => (config, ConfigState::Loaded),
                Err(e) => (Config::default(), unusable(path, describe(&text, &e))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (Config::default(), ConfigState::Missing)
            }
            Err(e) => (Config::default(), unusable(path, e.to_string())),
        };
        config.migrate();
        LoadedConfig { config, state }
    }

    /// Write settings, creating the directory if needed.
    ///
    /// Writes to a temporary file and renames it, so an interrupted save cannot
    /// leave a truncated configuration behind.
    ///
    /// The temporary name is unique per save and the writes are serialised
    /// within the process. One shared temp path defeated the rename entirely:
    /// two saves interleaved their bytes into the same file and then renamed
    /// the blend into place, and whichever renamed second failed outright.
    /// Both the CLI and the app write this file, and the app saves from more
    /// than one place, so this is a race people actually hit.
    pub fn save(&self, path: &Path) -> Result<()> {
        let _serialised = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| crate::Error::Parse(format!("could not serialise config: {e}")))?;

        let temp = temp_path_for(path);
        std::fs::write(&temp, json)?;
        if let Err(e) = std::fs::rename(&temp, path) {
            // A temp file left behind would accumulate in the config directory,
            // and its name is unique so nothing else will ever reuse it.
            let _ = std::fs::remove_file(&temp);
            return Err(e.into());
        }
        Ok(())
    }

    /// Fold a pre-workspaces config into a workspace, and guarantee that at
    /// least one workspace always exists.
    ///
    /// Runs on every load. An older config carried its repositories at the top
    /// level; dropping those on upgrade would quietly delete someone's setup,
    /// so they are moved into a workspace instead.
    pub fn migrate(&mut self) {
        if !self.repos.is_empty() || !self.scan_roots.is_empty() {
            if self.workspaces.is_empty() {
                self.workspaces.push(Workspace::named("Personal"));
            }
            let repos = std::mem::take(&mut self.repos);
            let roots = std::mem::take(&mut self.scan_roots);
            if let Some(target) = self.workspaces.first_mut() {
                for path in repos {
                    target.add_repo(path);
                }
                for path in roots {
                    target.add_scan_root(path);
                }
            }
        }

        if self.workspaces.is_empty() {
            self.workspaces.push(Workspace::named("Personal"));
        }

        // Two workspaces sharing an id is not survivable: lookups answer with
        // whichever comes first, and a delete that matches by id takes both,
        // after which a load with nothing left manufactures an empty config and
        // every repository in both is gone. Renaming rather than dropping,
        // because a duplicate id is a reason to fix the id, never a reason to
        // discard the repositories filed under it.
        let mut taken = std::collections::HashSet::new();
        for ws in &mut self.workspaces {
            if taken.insert(ws.id.clone()) {
                continue;
            }
            let mut replacement = new_id();
            while !taken.insert(replacement.clone()) {
                replacement = new_id();
            }
            ws.id = replacement;
        }

        // `None` is the documented representation of "show every workspace at
        // once", not a dangling pointer, so only a `Some` naming a workspace
        // that is genuinely absent gets repaired. Treating `None` as broken
        // meant choosing "All workspaces" and restarting silently narrowed the
        // view to one of them.
        if let Some(id) = self.active_workspace.clone()
            && !self.workspaces.iter().any(|w| w.id == id)
        {
            self.active_workspace = self.workspaces.first().map(|w| w.id.clone());
        }
    }

    pub fn workspace(&self, id: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == id)
    }

    pub fn workspace_mut(&mut self, id: &str) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.id == id)
    }

    /// The workspace in view, or `None` when showing all of them.
    pub fn active(&self) -> Option<&Workspace> {
        self.active_workspace
            .as_deref()
            .and_then(|id| self.workspace(id))
    }

    /// Every repository and scan root in scope, honouring the active selection.
    pub fn scoped_sources(&self) -> (Vec<PathBuf>, Vec<PathBuf>) {
        match self.active() {
            Some(ws) => (ws.repos.clone(), ws.scan_roots.clone()),
            None => (
                self.workspaces
                    .iter()
                    .flat_map(|w| w.repos.clone())
                    .collect(),
                self.workspaces
                    .iter()
                    .flat_map(|w| w.scan_roots.clone())
                    .collect(),
            ),
        }
    }

    /// Create a workspace and return its id.
    pub fn create_workspace(&mut self, name: impl Into<String>) -> String {
        let ws = Workspace::named(name);
        let id = ws.id.clone();
        self.workspaces.push(ws);
        id
    }

    /// Remove a workspace. Only ever touches this config, never the disk.
    ///
    /// The last workspace cannot be removed, because a config with none would
    /// have nowhere to put the next workspace's repositories.
    ///
    /// Exactly one is removed. Matching by id and retaining the rest took every
    /// workspace sharing an id, which emptied the config and lost both sets of
    /// repositories on the next load; [`Config::migrate`] now prevents the
    /// duplicates, and removing by position means a config that predates that
    /// repair cannot lose a second workspace to one delete.
    pub fn delete_workspace(&mut self, id: &str) -> bool {
        if self.workspaces.len() <= 1 {
            return false;
        }
        let Some(index) = self.workspaces.iter().position(|w| w.id == id) else {
            return false;
        };
        self.workspaces.remove(index);
        if self.active_workspace.as_deref() == Some(id)
            && !self.workspaces.iter().any(|w| w.id == id)
        {
            self.active_workspace = self.workspaces.first().map(|w| w.id.clone());
        }
        true
    }

    pub fn rename_workspace(&mut self, id: &str, name: impl Into<String>) -> bool {
        match self.workspace_mut(id) {
            Some(ws) => {
                ws.name = name.into();
                true
            }
            None => false,
        }
    }

    /// Add a repository to a workspace, or to the active one when unspecified.
    pub fn add_repo_to(&mut self, workspace: Option<&str>, path: PathBuf) -> bool {
        let id = self.resolve_target(workspace);
        match id.and_then(|id| self.workspace_mut(&id)) {
            Some(ws) => ws.add_repo(path),
            None => false,
        }
    }

    /// Add a scan root to a workspace, or to the active one when unspecified.
    pub fn add_scan_root_to(&mut self, workspace: Option<&str>, path: PathBuf) -> bool {
        let id = self.resolve_target(workspace);
        match id.and_then(|id| self.workspace_mut(&id)) {
            Some(ws) => ws.add_scan_root(path),
            None => false,
        }
    }

    /// Remove a path from a workspace, or from wherever it is found.
    pub fn remove_source(&mut self, workspace: Option<&str>, path: &Path) -> bool {
        match workspace {
            Some(id) => self.workspace_mut(id).is_some_and(|ws| ws.remove(path)),
            None => self.workspaces.iter_mut().any(|ws| ws.remove(path)),
        }
    }

    /// Where an unqualified add should land: the named workspace, else the
    /// active one, else the first. Never nowhere, so an add cannot be lost.
    fn resolve_target(&self, workspace: Option<&str>) -> Option<String> {
        workspace
            .map(str::to_string)
            .filter(|id| self.workspace(id).is_some())
            .or_else(|| self.active_workspace.clone())
            .or_else(|| self.workspaces.first().map(|w| w.id.clone()))
    }

    /// Take over the settings `previous` had that this version cannot name.
    ///
    /// A config that has been through a frontend has only the fields that
    /// frontend knows about, so the ones a newer yawm wrote have to be restored
    /// from the copy that came off disk. See [`Config::extra`].
    pub fn carry_unknown_from(&mut self, previous: &Config) {
        for (key, value) in &previous.extra {
            self.extra.entry(key.clone()).or_insert(value.clone());
        }
        for ws in &mut self.workspaces {
            let Some(before) = previous.workspaces.iter().find(|w| w.id == ws.id) else {
                continue;
            };
            for (key, value) in &before.extra {
                ws.extra.entry(key.clone()).or_insert(value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_carry_gitignored_files_over() {
        // Opt-out, not opt-in: the common case should just work.
        let p = ProvisioningDefaults::default();
        assert!(p.copy_env_files);
        assert!(p.link_dependencies);
        assert!(p.honour_worktreeinclude);
    }

    #[test]
    fn activity_threshold_converts_from_user_units() {
        let cfg = Config {
            active_within_minutes: 45,
            ..Default::default()
        };
        let v = cfg.verdict_config();
        assert_eq!(v.active_within_secs, 45 * 60);
    }

    #[test]
    fn a_fresh_config_has_exactly_one_active_workspace() {
        let cfg = Config::default();
        assert_eq!(cfg.workspaces.len(), 1);
        assert!(cfg.active().is_some());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.json");

        let mut cfg = Config::default();
        cfg.add_repo_to(None, "/code/alpha".into());
        cfg.editor = Some("cursor".into());
        cfg.save(&path).unwrap();

        assert_eq!(Config::load(&path), cfg);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let loaded = Config::load(Path::new("/definitely/not/here.json"));
        assert_eq!(loaded.workspaces.len(), 1);
        assert!(loaded.workspaces[0].is_empty());
    }

    /// A hand-edited settings file must not prevent the app from starting.
    #[test]
    fn corrupt_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        assert_eq!(Config::load(&path).workspaces.len(), 1);
    }

    /// The pair that destroyed configurations: one field of the wrong type made
    /// `load` answer with defaults, and the caller wrote those defaults back.
    /// Defaults are the right answer for a file that is not there and a guess
    /// for one that is, and only the caller can tell those apart.
    #[test]
    fn one_malformed_field_is_reported_rather_than_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"workspaces":[{"id":"w","name":"Work","repos":["/code/alpha"],"scanRoots":[]}],
                "scanDepth":"four"}"#,
        )
        .unwrap();

        let loaded = Config::load_reporting(&path);

        let ConfigState::Unusable { reason, backup } = &loaded.state else {
            panic!("a file that could not be parsed must not read as loaded");
        };
        assert!(
            reason.contains("scanDepth") || reason.contains("scan_depth"),
            "the offending field has to be nameable in the UI; got {reason}"
        );
        assert!(
            !loaded.state.is_usable(),
            "these defaults must never be written back"
        );

        let backup = backup.as_ref().expect("the only copy of the settings");
        assert!(
            std::fs::read_to_string(backup)
                .unwrap()
                .contains("/code/alpha"),
            "the repositories have to survive somewhere recoverable"
        );
    }

    /// A file that was never written is not a failure, and defaults really are
    /// the settings — so the app may write them back.
    #[test]
    fn a_missing_file_is_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Config::load_reporting(&dir.path().join("config.json"));

        assert_eq!(loaded.state, ConfigState::Missing);
        assert!(loaded.state.is_usable());
    }

    /// Every launch re-reads the same unparseable file, and a directory of
    /// identical copies buries the one the user needs.
    #[test]
    fn repeated_loads_keep_one_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{ not valid json").unwrap();

        let first = Config::load_reporting(&path).state;
        let second = Config::load_reporting(&path).state;

        assert_eq!(first, second);
        let backups = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("corrupt-"))
            .count();
        assert_eq!(backups, 1);
    }

    /// Running an older yawm once must not delete what a newer one wrote. Serde
    /// drops unknown fields, and the startup write-back then removes them from
    /// the file, which is permanent.
    #[test]
    fn settings_from_a_newer_version_survive_a_load_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"editor":"zed",
                "futureRepositories":["/code/from-the-future"],
                "workspaces":[{"id":"w","name":"Work","repos":[],"scanRoots":[],
                               "futureWorkspaceSetting":true}]}"#,
        )
        .unwrap();

        let cfg = Config::load(&path);
        cfg.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            json["futureRepositories"],
            serde_json::json!(["/code/from-the-future"]),
            "a downgrade must not erase what it cannot read; file was {text}"
        );
        assert_eq!(json["workspaces"][0]["futureWorkspaceSetting"], true);
        assert_eq!(json["editor"], "zed", "known settings still round trip");
    }

    /// The frontend only ever sends back the fields it knows, so what a newer
    /// yawm wrote has to be restored from the copy that came off disk.
    #[test]
    fn unknown_settings_are_restored_onto_a_config_that_lost_them() {
        let mut from_disk = Config::default();
        from_disk
            .extra
            .insert("futureThing".into(), serde_json::json!(7));
        from_disk.workspaces[0]
            .extra
            .insert("futureFlag".into(), serde_json::json!("on"));

        let mut from_frontend = from_disk.clone();
        from_frontend.extra.clear();
        from_frontend.workspaces[0].extra.clear();
        from_frontend.editor = Some("cursor".into());

        from_frontend.carry_unknown_from(&from_disk);

        assert_eq!(from_frontend.extra["futureThing"], serde_json::json!(7));
        assert_eq!(
            from_frontend.workspaces[0].extra["futureFlag"],
            serde_json::json!("on")
        );
        assert_eq!(from_frontend.editor.as_deref(), Some("cursor"));
    }

    /// Fields added in a later version must not discard existing settings.
    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"editor":"zed","somethingNew":42}"#).unwrap();

        let cfg = Config::load(&path);
        assert_eq!(cfg.editor.as_deref(), Some("zed"));
        assert_eq!(cfg.scan_depth, Config::default().scan_depth);
    }

    /// The upgrade path that matters: a config written before workspaces
    /// existed must keep its repositories, not silently lose them.
    #[test]
    fn a_pre_workspaces_config_is_migrated_rather_than_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"repos":["/code/alpha"],"scanRoots":["/code"],"editor":"zed"}"#,
        )
        .unwrap();

        let cfg = Config::load(&path);

        assert_eq!(cfg.workspaces.len(), 1, "everything lands in one workspace");
        let ws = &cfg.workspaces[0];
        assert_eq!(ws.repos, vec![PathBuf::from("/code/alpha")]);
        assert_eq!(ws.scan_roots, vec![PathBuf::from("/code")]);
        assert!(cfg.repos.is_empty(), "the legacy field is emptied");
        assert!(cfg.scan_roots.is_empty());
        assert_eq!(cfg.editor.as_deref(), Some("zed"), "other settings survive");
        assert_eq!(cfg.active_workspace.as_ref(), Some(&ws.id));
    }

    #[test]
    fn sources_are_scoped_to_the_active_workspace() {
        let mut cfg = Config::default();
        let first = cfg.workspaces[0].id.clone();
        cfg.add_repo_to(Some(&first), "/code/real".into());

        let second = cfg.create_workspace("Demo");
        cfg.add_repo_to(Some(&second), "/tmp/demo".into());

        cfg.active_workspace = Some(first.clone());
        let (repos, _) = cfg.scoped_sources();
        assert_eq!(repos, vec![PathBuf::from("/code/real")], "demo stays out");

        cfg.active_workspace = Some(second);
        let (repos, _) = cfg.scoped_sources();
        assert_eq!(repos, vec![PathBuf::from("/tmp/demo")]);

        // No active workspace means show everything.
        cfg.active_workspace = None;
        let (repos, _) = cfg.scoped_sources();
        assert_eq!(repos.len(), 2);
    }

    #[test]
    fn an_unqualified_add_lands_in_the_active_workspace() {
        let mut cfg = Config::default();
        let demo = cfg.create_workspace("Demo");
        cfg.active_workspace = Some(demo.clone());

        cfg.add_repo_to(None, "/tmp/demo".into());

        assert!(cfg.workspace(&demo).unwrap().repos.len() == 1);
        assert!(cfg.workspaces[0].repos.is_empty());
    }

    #[test]
    fn repositories_are_deduplicated_within_a_workspace() {
        let mut cfg = Config::default();
        assert!(cfg.add_repo_to(None, "/code/alpha".into()));
        assert!(
            !cfg.add_repo_to(None, "/code/alpha/".into()),
            "same path, trailing separator"
        );
        assert_eq!(cfg.workspaces[0].repos.len(), 1);
    }

    #[test]
    fn sources_can_be_removed() {
        let mut cfg = Config::default();
        cfg.add_repo_to(None, "/code/alpha".into());
        assert!(cfg.remove_source(None, Path::new("/code/alpha")));
        assert!(cfg.workspaces[0].repos.is_empty());
        assert!(!cfg.remove_source(None, Path::new("/code/alpha")));
    }

    #[test]
    fn deleting_a_workspace_moves_the_active_selection() {
        let mut cfg = Config::default();
        let demo = cfg.create_workspace("Demo");
        cfg.active_workspace = Some(demo.clone());

        assert!(cfg.delete_workspace(&demo));
        assert_eq!(cfg.workspaces.len(), 1);
        assert_eq!(cfg.active_workspace, Some(cfg.workspaces[0].id.clone()));
    }

    /// A config with no workspaces would have nowhere to put the next
    /// repository, so the last one is not removable.
    #[test]
    fn the_last_workspace_cannot_be_deleted() {
        let mut cfg = Config::default();
        let only = cfg.workspaces[0].id.clone();
        assert!(!cfg.delete_workspace(&only));
        assert_eq!(cfg.workspaces.len(), 1);
    }

    #[test]
    fn renaming_keeps_the_identifier_stable() {
        let mut cfg = Config::default();
        let id = cfg.workspaces[0].id.clone();
        assert!(cfg.rename_workspace(&id, "Work"));
        assert_eq!(cfg.workspaces[0].id, id);
        assert_eq!(cfg.workspaces[0].name, "Work");
    }

    #[test]
    fn workspace_identifiers_are_unique() {
        let mut cfg = Config::default();
        let a = cfg.create_workspace("A");
        let b = cfg.create_workspace("B");
        assert_ne!(a, b);
    }

    /// "All workspaces" is `None`, which [`Config::active`] documents as
    /// showing every workspace. Repairing it on load made the choice last
    /// exactly until the next restart, and the user saw a shorter list rather
    /// than an error.
    #[test]
    fn showing_all_workspaces_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        let mut cfg = Config::default();
        cfg.add_repo_to(None, "/code/alpha".into());
        let demo = cfg.create_workspace("Demo");
        cfg.add_repo_to(Some(&demo), "/code/beta".into());
        cfg.active_workspace = None;
        cfg.save(&path).unwrap();

        let loaded = Config::load(&path);

        assert_eq!(loaded.active_workspace, None, "still showing all of them");
        assert!(loaded.active().is_none());
        let (repos, _) = loaded.scoped_sources();
        assert_eq!(repos.len(), 2, "no workspace disappeared from view");
    }

    /// The case `None` was being confused with: an id naming a workspace that
    /// is really gone shows nothing at all, which reads as a broken app.
    #[test]
    fn an_active_id_with_no_workspace_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"workspaces":[{"id":"real","name":"Real","repos":["/code/alpha"],"scanRoots":[]}],
                "activeWorkspace":"ghost"}"#,
        )
        .unwrap();

        let cfg = Config::load(&path);

        assert_eq!(cfg.active_workspace.as_deref(), Some("real"));
    }

    /// Two workspaces under one id make every id-keyed operation ambiguous, and
    /// one delete took both. Repaired by renaming, because the repositories
    /// filed under a duplicate id are not the thing that is wrong.
    #[test]
    fn duplicate_workspace_identifiers_are_repaired_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"workspaces":[
                 {"id":"dup","name":"Work","repos":["/code/alpha"],"scanRoots":[]},
                 {"id":"dup","name":"Demo","repos":["/code/beta"],"scanRoots":[]}],
                "activeWorkspace":"dup"}"#,
        )
        .unwrap();

        let cfg = Config::load(&path);

        assert_eq!(cfg.workspaces.len(), 2, "neither workspace is discarded");
        assert_ne!(cfg.workspaces[0].id, cfg.workspaces[1].id);
        assert_eq!(cfg.workspaces[0].repos, vec![PathBuf::from("/code/alpha")]);
        assert_eq!(cfg.workspaces[1].repos, vec![PathBuf::from("/code/beta")]);
        assert_eq!(
            cfg.active_workspace.as_deref(),
            Some("dup"),
            "the selection keeps pointing at the workspace that kept the id"
        );

        let mut cfg = cfg;
        let first = cfg.workspaces[0].id.clone();
        assert!(cfg.delete_workspace(&first));
        assert_eq!(cfg.workspaces.len(), 1, "one delete removes one workspace");
        assert_eq!(cfg.workspaces[0].repos, vec![PathBuf::from("/code/beta")]);
    }

    /// The unrepaired shape, reached in memory: deleting must still take one.
    /// The old `retain` took every match, leaving zero workspaces, and the next
    /// load then manufactured an empty config over the top of both.
    #[test]
    fn deleting_by_a_duplicated_identifier_removes_exactly_one() {
        let mut cfg = Config {
            workspaces: vec![
                Workspace {
                    id: "dup".into(),
                    name: "Work".into(),
                    repos: vec!["/code/alpha".into()],
                    scan_roots: Vec::new(),
                    ..Default::default()
                },
                Workspace {
                    id: "dup".into(),
                    name: "Demo".into(),
                    repos: vec!["/code/beta".into()],
                    scan_roots: Vec::new(),
                    ..Default::default()
                },
            ],
            active_workspace: Some("dup".into()),
            ..Default::default()
        };

        assert!(cfg.delete_workspace("dup"));

        assert_eq!(cfg.workspaces.len(), 1);
        assert_eq!(cfg.workspaces[0].repos, vec![PathBuf::from("/code/beta")]);
        assert_eq!(cfg.active_workspace.as_deref(), Some("dup"));
        assert!(
            !cfg.delete_workspace("dup"),
            "the last workspace is still not removable"
        );
    }

    /// Every writer used one temp path, so concurrent saves wrote over each
    /// other's bytes and then renamed the result into place.
    #[test]
    fn concurrent_saves_all_succeed_and_leave_a_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        const WRITERS: usize = 8;
        const SAVES: usize = 16;

        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let path = path.clone();
                scope.spawn(move || {
                    let mut cfg = Config::default();
                    cfg.add_repo_to(None, format!("/code/repo-{writer}").into());
                    for _ in 0..SAVES {
                        cfg.save(&path).expect("a concurrent save must not fail");
                    }
                });
            }
            // A reader during the race must never see a partial file: the
            // rename is what makes the write look instantaneous.
            scope.spawn(|| {
                for _ in 0..(WRITERS * SAVES) {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        assert!(
                            serde_json::from_str::<Config>(&text).is_ok(),
                            "a reader saw a partly written config: {text:?}"
                        );
                    }
                }
            });
        });

        let loaded = Config::load(&path);
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].repos.len(), 1);

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "config.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn temp_files_are_unique_and_beside_their_destination() {
        let path = Path::new("/config/dir/config.json");
        let first = temp_path_for(path);
        let second = temp_path_for(path);

        assert_ne!(first, second);
        assert_eq!(
            first.parent(),
            path.parent(),
            "rename must stay on one disk"
        );
        assert_eq!(second.parent(), path.parent());
        assert_ne!(first, path.to_path_buf());
    }
}
