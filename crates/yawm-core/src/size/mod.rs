//! Measuring what a worktree costs on disk.
//!
//! This is yawm's hot path: answering "which worktrees can I delete" is only
//! useful alongside "and how much space would that reclaim". A machine with
//! thirty worktrees means walking thirty dependency trees, so the walk is
//! parallel (`jwalk` over a rayon pool) rather than sequential.
//!
//! One pass produces everything: the total, the per-directory breakdown, and
//! the most recent modification time. Shelling out to `du` would have needed a
//! separate invocation per heavy directory, produced no mtime, and not existed
//! on Windows at all.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::model::{HeavyDir, SizeInfo};

/// Directories worth reporting separately: large, regenerable, and the usual
/// reason a worktree is expensive.
pub const HEAVY_DIRS: &[&str] = &[
    "node_modules",
    ".venv",
    "venv",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "vendor",
    ".gradle",
    "Pods",
    ".tox",
    "__pycache__",
];

/// Tuning for [`measure`].
#[derive(Debug, Clone)]
pub struct SizeOptions {
    /// Top-level directory names to break out separately.
    pub heavy_dirs: Vec<String>,
    /// Worker threads. Zero lets rayon choose based on available parallelism.
    pub threads: usize,
}

impl Default for SizeOptions {
    fn default() -> Self {
        Self {
            heavy_dirs: HEAVY_DIRS.iter().map(|s| s.to_string()).collect(),
            threads: 0,
        }
    }
}

/// Walk `root` and report its size, breakdown, and last activity.
///
/// Symbolic links are never followed. That matters twice over: a worktree whose
/// `node_modules` is linked to the main checkout costs nothing extra on disk
/// and must not be counted, and its modification time belongs to the link
/// rather than the shared target, so linked dependencies cannot make an idle
/// worktree look active.
pub fn measure(root: &Path, opts: &SizeOptions) -> SizeInfo {
    if !root.is_dir() {
        return SizeInfo::default();
    }

    let mut info = SizeInfo::default();
    let mut buckets: HashMap<String, u64> = HashMap::new();
    // Hardlinked files appear once per link; count the inode only once so a
    // pnpm store or a hardlinked cache is not reported several times over.
    let mut seen_inodes = std::collections::HashSet::new();

    let walker = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .parallelism(match opts.threads {
            0 => jwalk::Parallelism::RayonDefaultPool {
                busy_timeout: std::time::Duration::from_secs(5),
            },
            n => jwalk::Parallelism::RayonNewPool(n),
        });

    for entry in walker.into_iter().flatten() {
        let Ok(meta) = entry.metadata() else { continue };

        if meta.is_dir() {
            continue;
        }

        let Ok(relative) = entry.path().strip_prefix(root).map(Path::to_path_buf) else {
            continue;
        };
        let top = top_component(&relative);

        // `.git` in a linked worktree is a small pointer file that git rewrites
        // during ordinary operations. Including it would make every worktree
        // look freshly active.
        let is_git_metadata = top.as_deref() == Some(".git");

        if !is_git_metadata && let Some(modified) = modified_unix(&meta) {
            info.last_modified = Some(
                info.last_modified
                    .map_or(modified, |m: i64| m.max(modified)),
            );
        }

        if !count_once(&meta, &mut seen_inodes) {
            continue;
        }

        let bytes = on_disk_size(&meta);
        info.bytes += bytes;
        info.files += 1;

        if let Some(name) = top
            && opts.heavy_dirs.contains(&name)
        {
            *buckets.entry(name).or_default() += bytes;
        }
    }

    info.heavy_dirs = collect_heavy_dirs(root, &opts.heavy_dirs, &buckets);
    info
}

/// Sizes already measured this session, so a second look at the same worktree
/// is free.
///
/// Walking a worktree costs whole seconds of disk I/O — sixteen gigabytes of
/// dependency trees on a real machine — and switching between workspaces asks
/// for the same answer over and over. Holding the results means the second
/// visit paints complete rather than empty.
///
/// The danger with a size cache is showing a number that quietly stopped being
/// true, so this one is deliberately built so that it cannot. Two things
/// guarantee it. First, an entry is only returned when a cheap fingerprint of
/// the worktree root still matches the one taken when it was measured, which
/// catches a directory that was deleted, replaced, or had anything added or
/// removed at its top level. Second, and more importantly, a cached value is
/// only ever used by the *fast* scan — the one that exists to paint
/// immediately. The full scan behind it always re-walks and overwrites. So a
/// cached number is at worst a few seconds ahead of a real measurement that is
/// already running, never a lasting claim.
///
/// The fingerprint deliberately stops at the top level. Recursing to detect a
/// deep change would cost the same walk the cache exists to avoid, and would
/// buy nothing that the trailing full scan does not already provide.
#[derive(Debug, Clone, Default)]
pub struct SizeCache {
    entries: Arc<Mutex<HashMap<PathBuf, CacheEntry>>>,
}

#[derive(Debug)]
struct CacheEntry {
    fingerprint: Fingerprint,
    info: SizeInfo,
}

impl SizeCache {
    /// The stored measurement, if the worktree still looks the way it did.
    pub fn get(&self, root: &Path) -> Option<SizeInfo> {
        let fingerprint = Fingerprint::of(root)?;
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(root)?;
        (entry.fingerprint == fingerprint).then(|| entry.info.clone())
    }

    fn store(&self, root: &Path, fingerprint: Option<Fingerprint>, info: &SizeInfo) {
        let Some(fingerprint) = fingerprint else {
            return;
        };
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(
                root.to_path_buf(),
                CacheEntry {
                    fingerprint,
                    info: info.clone(),
                },
            );
        }
    }
}

/// Measure `root` and remember the answer for the next scan.
pub fn measure_and_cache(root: &Path, opts: &SizeOptions, cache: &SizeCache) -> SizeInfo {
    // Taken before the walk rather than after, so a change made while the walk
    // was in progress invalidates the entry instead of being sealed into it.
    let fingerprint = Fingerprint::of(root);
    let info = measure(root, opts);
    cache.store(root, fingerprint, &info);
    info
}

/// What a worktree root looks like without descending into it.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    root: EntryStamp,
    /// Sorted, because directory order is not guaranteed to be stable.
    children: Vec<(OsString, EntryStamp)>,
}

/// Enough of an entry's metadata to notice it was replaced.
#[derive(Debug, PartialEq, Eq)]
struct EntryStamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    is_dir: bool,
    is_symlink: bool,
}

impl EntryStamp {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            modified: meta.modified().ok(),
            len: meta.len(),
            is_dir: meta.is_dir(),
            is_symlink: meta.is_symlink(),
        }
    }
}

impl Fingerprint {
    fn of(root: &Path) -> Option<Self> {
        let meta = std::fs::symlink_metadata(root).ok()?;
        if !meta.is_dir() {
            return None;
        }

        let mut children: Vec<(OsString, EntryStamp)> = std::fs::read_dir(root)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                Some((entry.file_name(), EntryStamp::of(&meta)))
            })
            .collect();
        children.sort_by(|a, b| a.0.cmp(&b.0));

        Some(Self {
            root: EntryStamp::of(&meta),
            children,
        })
    }
}

/// Build the heavy-directory report, including linked ones that occupy no space.
fn collect_heavy_dirs(
    root: &Path,
    names: &[String],
    buckets: &HashMap<String, u64>,
) -> Vec<HeavyDir> {
    let mut out: Vec<HeavyDir> = names
        .iter()
        .filter_map(|name| {
            let path = root.join(name);
            let meta = std::fs::symlink_metadata(&path).ok()?;
            let is_link = meta.is_symlink();
            Some(HeavyDir {
                name: name.clone(),
                // A link points at storage owned by another worktree, so
                // deleting this one reclaims none of it.
                bytes: if is_link {
                    0
                } else {
                    buckets.get(name).copied().unwrap_or(0)
                },
                is_link,
            })
        })
        .collect();

    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    out
}

/// First path component, used to attribute a file to a top-level directory.
fn top_component(relative: &Path) -> Option<String> {
    relative
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
}

/// Space actually occupied, which is not the same as the file's length.
///
/// Unix reports allocated 512-byte blocks, so sparse files are not
/// overcounted and small files correctly cost a whole block. Windows exposes no
/// equivalent through `std`, so the logical length is used there.
#[cfg(unix)]
fn on_disk_size(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks() * 512
}

#[cfg(not(unix))]
fn on_disk_size(meta: &std::fs::Metadata) -> u64 {
    meta.len()
}

/// Whether this file's bytes should be counted, given hardlinks already seen.
#[cfg(unix)]
fn count_once(meta: &std::fs::Metadata, seen: &mut std::collections::HashSet<(u64, u64)>) -> bool {
    use std::os::unix::fs::MetadataExt;
    if meta.nlink() <= 1 {
        return true;
    }
    seen.insert((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn count_once(
    _meta: &std::fs::Metadata,
    _seen: &mut std::collections::HashSet<(u64, u64)>,
) -> bool {
    true
}

fn modified_unix(meta: &std::fs::Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs() as i64),
        // Timestamps before 1970 are nonsense in this context; ignore them.
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn missing_directory_measures_as_empty() {
        let info = measure(Path::new("/definitely/not/here"), &SizeOptions::default());
        assert_eq!(info.bytes, 0);
        assert_eq!(info.files, 0);
    }

    #[test]
    fn counts_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), 100);
        write(&dir.path().join("nested/b.txt"), 100);

        let info = measure(dir.path(), &SizeOptions::default());
        assert_eq!(info.files, 2);
        assert!(info.bytes > 0);
    }

    #[test]
    fn attributes_bytes_to_heavy_directories() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/main.rs"), 10);
        write(&dir.path().join("node_modules/pkg/index.js"), 5000);

        let info = measure(dir.path(), &SizeOptions::default());
        let nm = info
            .heavy_dirs
            .iter()
            .find(|d| d.name == "node_modules")
            .expect("node_modules reported");

        assert!(!nm.is_link);
        assert!(nm.bytes > 0);
        assert!(nm.bytes <= info.bytes);
    }

    #[test]
    fn absent_heavy_directories_are_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/main.rs"), 10);

        let info = measure(dir.path(), &SizeOptions::default());
        assert!(info.heavy_dirs.is_empty());
    }

    /// The provisioning feature links dependencies between worktrees. A linked
    /// directory costs nothing extra, so counting it would wildly overstate
    /// what deleting the worktree would reclaim.
    #[cfg(unix)]
    #[test]
    fn linked_dependencies_are_reported_as_reclaiming_nothing() {
        let shared = tempfile::tempdir().unwrap();
        write(&shared.path().join("pkg/index.js"), 50_000);

        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("src/main.rs"), 10);
        std::os::unix::fs::symlink(shared.path(), dir.path().join("node_modules")).unwrap();

        let info = measure(dir.path(), &SizeOptions::default());
        let nm = info
            .heavy_dirs
            .iter()
            .find(|d| d.name == "node_modules")
            .expect("node_modules reported");

        assert!(nm.is_link);
        assert_eq!(nm.bytes, 0, "a link occupies no space of its own");
        // The 50 KB behind the link must not appear in the total either.
        assert!(info.bytes < 50_000);
    }

    #[test]
    fn reports_the_most_recent_modification() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), 10);

        let info = measure(dir.path(), &SizeOptions::default());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let modified = info.last_modified.expect("a timestamp");
        assert!(
            (now - modified).abs() < 120,
            "expected a recent timestamp, got {modified} against {now}"
        );
    }

    /// Git rewrites the `.git` pointer during ordinary operations, so counting
    /// it would make every worktree look like it was just being worked in.
    #[test]
    fn git_metadata_does_not_count_as_activity() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".git"), "gitdir: /elsewhere").unwrap();

        let info = measure(dir.path(), &SizeOptions::default());
        assert!(
            info.last_modified.is_none(),
            "only .git was present, so there is no user activity to report"
        );
        // It still occupies space, so it is counted toward the total.
        assert_eq!(info.files, 1);
    }

    #[test]
    fn heavy_directories_are_ordered_largest_first() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("node_modules/a.js"), 20_000);
        write(&dir.path().join("dist/b.js"), 100);

        let info = measure(dir.path(), &SizeOptions::default());
        assert_eq!(info.heavy_dirs[0].name, "node_modules");
    }

    #[test]
    fn a_measured_worktree_is_served_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), 100);
        let cache = SizeCache::default();

        let measured = measure_and_cache(dir.path(), &SizeOptions::default(), &cache);
        assert_eq!(cache.get(dir.path()), Some(measured));
    }

    #[test]
    fn an_unmeasured_worktree_is_not_in_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(SizeCache::default().get(dir.path()), None);
    }

    /// The whole risk of a size cache is answering for a worktree that has
    /// since changed, so a new top-level entry has to invalidate it.
    #[test]
    fn adding_a_file_invalidates_the_cached_size() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), 100);
        let cache = SizeCache::default();
        measure_and_cache(dir.path(), &SizeOptions::default(), &cache);

        write(&dir.path().join("b.txt"), 100);
        assert_eq!(cache.get(dir.path()), None);
    }

    #[test]
    fn removing_the_directory_invalidates_the_cached_size() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), 100);
        let cache = SizeCache::default();
        measure_and_cache(dir.path(), &SizeOptions::default(), &cache);

        let path = dir.path().to_path_buf();
        drop(dir);
        assert_eq!(cache.get(&path), None);
    }

    /// Two worktrees of the same repository hold the same file names, so the
    /// key has to be the path rather than anything about the contents.
    #[test]
    fn sibling_worktrees_do_not_share_an_entry() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        write(&one.path().join("a.txt"), 100);
        write(&two.path().join("a.txt"), 100);

        let cache = SizeCache::default();
        measure_and_cache(one.path(), &SizeOptions::default(), &cache);
        assert!(cache.get(one.path()).is_some());
        assert_eq!(cache.get(two.path()), None);
    }
}
