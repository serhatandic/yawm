//! Reading the untracked side of a worktree without lying about what was read.
//!
//! `git ls-files --others` has two behaviours that matter here. A nested
//! ordinary directory is listed file by file, which is what a reader wants. A
//! nested *repository* is not: an ordinary or linked worktree is reported as a
//! single directory, while a bare repository is walked into and its internals —
//! `HEAD`, `hooks/`, `refs/`, `objects/` — arrive as dozens of separate paths.
//! Rendering those individually buries the reader's own work under Git plumbing
//! they did not write, so a repository is collapsed to one atomic entry here,
//! and every raw path it swallowed is still counted so the totals reconcile.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::{DiffLimit, RepositoryKind, display_git_path};
use crate::git::{Git, StreamControl};
use crate::model::WorktreeEntry;

/// How many distinct rows the untracked side may contribute.
pub(super) const MAX_UNTRACKED_ENTRIES: usize = 512;
const MAX_UNTRACKED_PATH_BYTES: usize = 4 * 1024 * 1024;
const MAX_UNTRACKED_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_UNTRACKED_FILE_BYTES: u64 = 8 * 1024 * 1024;
const BINARY_PROBE_BYTES: usize = 8_000;
const MAX_DIRECTORY_ITEMS: u32 = 1024;
const MAX_NAMED_PATHS: usize = 10;
const MAX_DIR_CACHE: usize = 4096;
const MAX_GITDIR_BYTES: u64 = 4096;

/// Why a path is present in the list but has no content beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnreadReason {
    /// Larger than the per-file read limit.
    TooLarge(u64),
    /// The worktree-wide read budget was already spent.
    ReadBudget,
    /// A socket, fifo or device: real, but not a file with contents.
    NotRegular,
}

impl UnreadReason {
    pub(super) fn detail(&self) -> String {
        match self {
            Self::TooLarge(bytes) => format!(
                "{} bytes, over the {} byte per-file read limit, so its contents were not read.",
                bytes, MAX_UNTRACKED_FILE_BYTES
            ),
            Self::ReadBudget => {
                "Not read: the worktree-wide untracked read budget was already spent.".into()
            }
            Self::NotRegular => {
                "Not a regular file, so it has no contents to show as a diff.".into()
            }
        }
    }
}

#[derive(Debug)]
pub(super) enum UntrackedContent {
    Text(Vec<u8>),
    Binary,
    Empty,
    Symlink(String),
    Directory {
        items: Option<u32>,
    },
    Repository {
        repository: RepositoryKind,
        items: Option<u32>,
    },
    Unread(UnreadReason),
}

#[derive(Debug)]
pub(super) struct UntrackedEntry {
    pub raw_path: Vec<u8>,
    pub path: String,
    pub mode: &'static str,
    /// Raw paths from Git that this one entry stands for. One, except for an
    /// atomic repository, which stands for everything Git listed inside it.
    pub paths: u32,
    pub content: UntrackedContent,
}

#[derive(Debug, Default)]
pub(super) struct UntrackedSnapshot {
    pub entries: Vec<UntrackedEntry>,
    /// Raw paths Git reported.
    pub total: u32,
    /// Raw paths that an entry stands for.
    pub represented: u32,
    pub incomplete: bool,
    pub limits: Vec<DiffLimit>,
}

pub(super) fn untracked_snapshot(git: &Git, entry: &WorktreeEntry) -> UntrackedSnapshot {
    if entry.prunable.is_some() || !entry.path.is_dir() {
        return UntrackedSnapshot::default();
    }
    let mut collector = Collector::new(&entry.path);
    let streamed = git.run_stream(
        &entry.path,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        |chunk| {
            collector.consume(chunk);
            Ok(StreamControl::Continue)
        },
    );
    if streamed.is_err() {
        return UntrackedSnapshot {
            incomplete: true,
            limits: vec![DiffLimit::ListingFailed],
            ..Default::default()
        };
    }
    collector.finish()
}

/// Paths named in a limit, plus how many there were in total.
#[derive(Debug, Default)]
struct Named {
    paths: Vec<String>,
    total: u32,
}

impl Named {
    fn record(&mut self, path: &str) {
        self.total = self.total.saturating_add(1);
        if self.paths.len() < MAX_NAMED_PATHS {
            self.paths.push(path.to_string());
        }
    }

    fn count(&mut self) {
        self.total = self.total.saturating_add(1);
    }
}

struct Collector<'a> {
    root: &'a Path,
    entries: Vec<UntrackedEntry>,
    /// Atomic prefixes already emitted, with the entry that owns them.
    roots: Vec<(Vec<u8>, usize)>,
    dir_cache: HashMap<Vec<u8>, Option<RepositoryKind>>,
    current: Vec<u8>,
    current_nonempty: bool,
    current_dropped: bool,
    total: u64,
    represented: u32,
    path_bytes: usize,
    bytes_read: usize,
    skipped_entry_limit: u32,
    unreadable: Named,
    too_large: Named,
    read_budget: Named,
    incomplete: bool,
}

impl<'a> Collector<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            entries: Vec::new(),
            roots: Vec::new(),
            dir_cache: HashMap::new(),
            current: Vec::new(),
            current_nonempty: false,
            current_dropped: false,
            total: 0,
            represented: 0,
            path_bytes: 0,
            bytes_read: 0,
            skipped_entry_limit: 0,
            unreadable: Named::default(),
            too_large: Named::default(),
            read_budget: Named::default(),
            incomplete: false,
        }
    }

    fn consume(&mut self, mut bytes: &[u8]) {
        while let Some(nul) = bytes.iter().position(|byte| *byte == 0) {
            self.consume_path_bytes(&bytes[..nul]);
            self.finish_path();
            bytes = &bytes[nul + 1..];
        }
        self.consume_path_bytes(bytes);
    }

    fn consume_path_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.current_nonempty = true;
        if self.current_dropped {
            return;
        }
        if self.current.len().saturating_add(bytes.len()) > MAX_UNTRACKED_PATH_BYTES {
            self.current.clear();
            self.current_dropped = true;
        } else {
            self.current.extend_from_slice(bytes);
        }
    }

    fn finish_path(&mut self) {
        if !self.current_nonempty {
            return;
        }
        self.total = self.total.saturating_add(1);
        if self.current_dropped {
            self.read_budget.count();
            self.incomplete = true;
        } else {
            let raw = std::mem::take(&mut self.current);
            self.absorb(&raw);
        }
        self.current.clear();
        self.current_nonempty = false;
        self.current_dropped = false;
    }

    fn finish(mut self) -> UntrackedSnapshot {
        if self.current_nonempty {
            self.finish_path();
            self.incomplete = true;
        }

        let mut limits = Vec::new();
        let total = self.total.try_into().unwrap_or(u32::MAX);
        if self.total > u64::from(u32::MAX) {
            self.incomplete = true;
        }
        if self.skipped_entry_limit > 0 {
            limits.push(DiffLimit::InspectionLimit {
                limit: MAX_UNTRACKED_ENTRIES as u32,
                shown: self.represented,
                total,
            });
        }
        for (named, build) in [
            (
                self.unreadable,
                (|paths, total| DiffLimit::Unreadable { paths, total })
                    as fn(Vec<String>, u32) -> DiffLimit,
            ),
            (self.too_large, |paths, total| DiffLimit::TooLarge {
                paths,
                total,
            }),
            (self.read_budget, |paths, total| DiffLimit::ReadBudget {
                paths,
                total,
            }),
        ] {
            if named.total > 0 {
                limits.push(build(named.paths, named.total));
            }
        }

        UntrackedSnapshot {
            incomplete: self.incomplete || !limits.is_empty(),
            entries: self.entries,
            total,
            represented: self.represented,
            limits,
        }
    }

    /// Fold one raw path from Git into the entry that will represent it.
    fn absorb(&mut self, raw: &[u8]) {
        if let Some(index) = self.atomic_root(raw) {
            self.entries[index].paths = self.entries[index].paths.saturating_add(1);
            self.represented = self.represented.saturating_add(1);
            return;
        }
        if self.entries.len() >= MAX_UNTRACKED_ENTRIES {
            self.skipped_entry_limit = self.skipped_entry_limit.saturating_add(1);
            self.incomplete = true;
            return;
        }
        if self.path_bytes.saturating_add(raw.len()) > MAX_UNTRACKED_PATH_BYTES {
            self.read_budget.count();
            self.incomplete = true;
            return;
        }

        // Git already decided this one is atomic: an ordinary or linked
        // worktree nested inside this one.
        if let Some(directory) = raw.strip_suffix(b"/") {
            let absolute = self.root.join(path_from_git_bytes(directory));
            let items = count_items(&absolute);
            let content = match repository_kind(&absolute) {
                Some(repository) => UntrackedContent::Repository { repository, items },
                None => UntrackedContent::Directory { items },
            };
            self.push_atomic(directory.to_vec(), content);
            return;
        }

        // Git did not: a bare repository is walked into, so its internals
        // arrive one by one and have to be gathered back up.
        if let Some((prefix, repository)) = self.repository_ancestor(raw) {
            let absolute = self.root.join(path_from_git_bytes(&prefix));
            let items = count_items(&absolute);
            self.push_atomic(prefix, UntrackedContent::Repository { repository, items });
            return;
        }

        let display = display_git_path(raw);
        match inspect_untracked_file(self.root, raw, &mut self.bytes_read) {
            Ok((content, mode)) => {
                match &content {
                    UntrackedContent::Unread(UnreadReason::TooLarge(_)) => {
                        self.too_large.record(&display);
                        self.incomplete = true;
                    }
                    UntrackedContent::Unread(UnreadReason::ReadBudget) => {
                        self.read_budget.record(&display);
                        self.incomplete = true;
                    }
                    _ => {}
                }
                self.path_bytes += raw.len();
                self.represented = self.represented.saturating_add(1);
                self.entries.push(UntrackedEntry {
                    raw_path: raw.to_vec(),
                    path: display,
                    mode,
                    paths: 1,
                    content,
                });
            }
            Err(_) => {
                self.unreadable.record(&display);
                self.incomplete = true;
            }
        }
    }

    fn push_atomic(&mut self, prefix: Vec<u8>, content: UntrackedContent) {
        let index = self.entries.len();
        self.path_bytes += prefix.len();
        self.represented = self.represented.saturating_add(1);
        self.entries.push(UntrackedEntry {
            path: display_git_path(&prefix),
            raw_path: prefix.clone(),
            mode: "040000",
            paths: 1,
            content,
        });
        self.roots.push((prefix, index));
    }

    fn atomic_root(&self, raw: &[u8]) -> Option<usize> {
        self.roots.iter().find_map(|(prefix, index)| {
            let inside =
                raw.len() > prefix.len() && raw.starts_with(prefix) && raw[prefix.len()] == b'/'
                    || raw == prefix.as_slice();
            inside.then_some(*index)
        })
    }

    /// The outermost ancestor directory of `raw` that is itself a repository.
    fn repository_ancestor(&mut self, raw: &[u8]) -> Option<(Vec<u8>, RepositoryKind)> {
        for (at, byte) in raw.iter().enumerate() {
            if *byte != b'/' {
                continue;
            }
            let prefix = &raw[..at];
            if prefix.is_empty() {
                continue;
            }
            if let Some(cached) = self.dir_cache.get(prefix) {
                if let Some(kind) = cached {
                    return Some((prefix.to_vec(), *kind));
                }
                continue;
            }
            let absolute = self.root.join(path_from_git_bytes(prefix));
            let kind = repository_kind(&absolute);
            if self.dir_cache.len() < MAX_DIR_CACHE {
                self.dir_cache.insert(prefix.to_vec(), kind);
            }
            if let Some(kind) = kind {
                return Some((prefix.to_vec(), kind));
            }
        }
        None
    }
}

/// Whether a directory is a repository, and which sort.
pub(super) fn repository_kind(dir: &Path) -> Option<RepositoryKind> {
    let pointer = dir.join(".git");
    match std::fs::symlink_metadata(&pointer) {
        Ok(metadata) if metadata.is_dir() => return Some(RepositoryKind::Nested),
        Ok(metadata) if metadata.is_file() => return linked_repository_kind(dir, &pointer),
        _ => {}
    }

    let bare =
        dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir();
    bare.then_some(RepositoryKind::Bare)
}

/// A `.git` *file* points elsewhere: a linked worktree, or a submodule.
///
/// Git only honours the pointer when its target exists, and descends into the
/// directory normally when it does not — so a dangling pointer is deliberately
/// not treated as a repository here either.
fn linked_repository_kind(dir: &Path, pointer: &Path) -> Option<RepositoryKind> {
    let file = File::open(pointer).ok()?;
    let mut text = String::new();
    file.take(MAX_GITDIR_BYTES).read_to_string(&mut text).ok()?;
    let target = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    if target.is_empty() {
        return None;
    }

    let target = Path::new(target);
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        dir.join(target)
    };
    if !resolved.exists() {
        return None;
    }

    let linked = resolved
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "worktrees");
    Some(if linked {
        RepositoryKind::LinkedWorktree
    } else {
        RepositoryKind::Nested
    })
}

/// How many things are immediately inside a directory, or `None` when there
/// are more than the bound — a floor reported as an exact count is a lie.
pub(super) fn count_items(dir: &Path) -> Option<u32> {
    let mut count = 0u32;
    for entry in std::fs::read_dir(dir).ok()? {
        entry.ok()?;
        count += 1;
        if count >= MAX_DIRECTORY_ITEMS {
            return None;
        }
    }
    Some(count)
}

fn inspect_untracked_file(
    root: &Path,
    raw_path: &[u8],
    bytes_read: &mut usize,
) -> std::io::Result<(UntrackedContent, &'static str)> {
    let absolute = root.join(path_from_git_bytes(raw_path));
    let metadata = std::fs::symlink_metadata(&absolute)?;
    let mode = untracked_mode(&metadata);

    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(&absolute)?;
        let bytes = os_string_bytes(target.into_os_string());
        if bytes_read.saturating_add(bytes.len()) > MAX_UNTRACKED_TEXT_BYTES {
            return Ok((UntrackedContent::Unread(UnreadReason::ReadBudget), mode));
        }
        *bytes_read += bytes.len();
        return Ok((UntrackedContent::Symlink(display_git_path(&bytes)), mode));
    }
    if metadata.is_dir() {
        return Ok((
            UntrackedContent::Directory {
                items: count_items(&absolute),
            },
            mode,
        ));
    }
    if !metadata.is_file() {
        return Ok((UntrackedContent::Unread(UnreadReason::NotRegular), mode));
    }

    let mut file = File::open(&absolute)?;
    let opened = file.metadata()?;
    if !opened.is_file() || !same_file(&metadata, &opened) {
        return Ok((UntrackedContent::Unread(UnreadReason::NotRegular), mode));
    }
    if opened.len() == 0 {
        return Ok((UntrackedContent::Empty, untracked_mode(&opened)));
    }

    let probe_len = usize::try_from(opened.len())
        .unwrap_or(usize::MAX)
        .min(BINARY_PROBE_BYTES);
    if bytes_read.saturating_add(probe_len) > MAX_UNTRACKED_TEXT_BYTES {
        return Ok((UntrackedContent::Unread(UnreadReason::ReadBudget), mode));
    }
    let mut probe = vec![0; probe_len];
    file.read_exact(&mut probe)?;
    *bytes_read += probe.len();
    if probe.contains(&0) {
        return Ok((UntrackedContent::Binary, mode));
    }
    if opened.len() > MAX_UNTRACKED_FILE_BYTES {
        return Ok((
            UntrackedContent::Unread(UnreadReason::TooLarge(opened.len())),
            mode,
        ));
    }

    let remaining = usize::try_from(opened.len())
        .unwrap_or(usize::MAX)
        .saturating_sub(probe.len());
    if bytes_read.saturating_add(remaining) > MAX_UNTRACKED_TEXT_BYTES {
        return Ok((UntrackedContent::Unread(UnreadReason::ReadBudget), mode));
    }
    let mut bytes = probe;
    let mut tail = file.by_ref().take(
        u64::try_from(remaining)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    );
    let before = bytes.len();
    tail.read_to_end(&mut bytes)?;
    let actual = bytes.len() - before;
    *bytes_read += actual;
    if actual > remaining {
        // It grew while being read; reporting the partial content as the whole
        // file would be a diff of something that never existed.
        return Ok((UntrackedContent::Unread(UnreadReason::ReadBudget), mode));
    }
    Ok((UntrackedContent::Text(bytes), mode))
}

#[cfg(unix)]
fn same_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    before.dev() == opened.dev() && before.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    before.file_type() == opened.file_type()
        && before.len() == opened.len()
        && before.modified().ok() == opened.modified().ok()
}

#[cfg(unix)]
fn untracked_mode(metadata: &std::fs::Metadata) -> &'static str {
    use std::os::unix::fs::PermissionsExt;
    if metadata.file_type().is_symlink() {
        "120000"
    } else if metadata.is_dir() {
        "040000"
    } else if metadata.permissions().mode() & 0o111 != 0 {
        "100755"
    } else {
        "100644"
    }
}

#[cfg(not(unix))]
fn untracked_mode(metadata: &std::fs::Metadata) -> &'static str {
    if metadata.file_type().is_symlink() {
        "120000"
    } else if metadata.is_dir() {
        "040000"
    } else {
        "100644"
    }
}

#[cfg(unix)]
pub(super) fn path_from_git_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
pub(super) fn path_from_git_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(unix)]
fn os_string_bytes(value: OsString) -> Vec<u8> {
    use std::os::unix::ffi::OsStringExt;
    value.into_vec()
}

#[cfg(not(unix))]
fn os_string_bytes(value: OsString) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(root: &Path, paths: &[&str]) -> UntrackedSnapshot {
        let mut collector = Collector::new(root);
        let mut stream = Vec::new();
        for path in paths {
            stream.extend_from_slice(path.as_bytes());
            stream.push(0);
        }
        for chunk in stream.chunks(7) {
            collector.consume(chunk);
        }
        collector.finish()
    }

    #[test]
    fn a_bare_repositorys_internals_collapse_into_one_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bare = dir.path().join("remote.git");
        std::fs::create_dir_all(bare.join("objects")).unwrap();
        std::fs::create_dir_all(bare.join("refs/heads")).unwrap();
        std::fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello\n").unwrap();

        let snapshot = collect(
            dir.path(),
            &[
                "notes.txt",
                "remote.git/HEAD",
                "remote.git/objects/info/packs",
                "remote.git/refs/heads/main",
            ],
        );

        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.total, 4);
        assert_eq!(snapshot.represented, 4);
        let repo = snapshot
            .entries
            .iter()
            .find(|entry| entry.path == "remote.git")
            .expect("one atomic repository entry");
        assert_eq!(repo.paths, 3);
        assert!(matches!(
            repo.content,
            UntrackedContent::Repository {
                repository: RepositoryKind::Bare,
                ..
            }
        ));
        assert!(!snapshot.incomplete);
    }

    #[test]
    fn a_nested_repository_is_atomic_and_an_ordinary_directory_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("plain")).unwrap();
        std::fs::write(dir.path().join("plain/a.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("plain/b.txt"), "b\n").unwrap();

        let snapshot = collect(dir.path(), &["nested/", "plain/a.txt", "plain/b.txt"]);

        assert_eq!(snapshot.entries.len(), 3, "the plain directory stays split");
        let repo = &snapshot.entries[0];
        assert_eq!(repo.path, "nested", "no trailing slash reaches the UI");
        assert!(matches!(
            repo.content,
            UntrackedContent::Repository {
                repository: RepositoryKind::Nested,
                ..
            }
        ));
    }

    #[test]
    fn a_linked_worktree_is_named_as_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let host = dir.path().join(".hostrepo/worktrees/wt");
        std::fs::create_dir_all(&host).unwrap();
        let linked = dir.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join(".git"), "gitdir: ../.hostrepo/worktrees/wt\n").unwrap();

        let snapshot = collect(dir.path(), &["linked/"]);

        assert!(matches!(
            snapshot.entries[0].content,
            UntrackedContent::Repository {
                repository: RepositoryKind::LinkedWorktree,
                ..
            }
        ));
    }

    #[test]
    fn a_dangling_gitdir_pointer_is_not_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let broken = dir.path().join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(".git"), "gitdir: ../nowhere\n").unwrap();

        assert_eq!(repository_kind(&broken), None);
    }

    #[test]
    fn every_path_is_counted_even_past_the_entry_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let extra = 17;
        let mut names = Vec::new();
        for index in 0..(MAX_UNTRACKED_ENTRIES + extra) {
            let name = format!("file-{index:04}.txt");
            std::fs::write(dir.path().join(&name), "x\n").unwrap();
            names.push(name);
        }
        let paths: Vec<&str> = names.iter().map(String::as_str).collect();

        let snapshot = collect(dir.path(), &paths);

        assert_eq!(snapshot.total, (MAX_UNTRACKED_ENTRIES + extra) as u32);
        assert_eq!(snapshot.entries.len(), MAX_UNTRACKED_ENTRIES);
        assert_eq!(snapshot.represented, MAX_UNTRACKED_ENTRIES as u32);
        assert!(snapshot.incomplete);
        assert!(
            snapshot
                .limits
                .iter()
                .any(|limit| matches!(limit, DiffLimit::InspectionLimit { limit: 512, .. }))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_reports_its_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("real.txt"), "hi\n").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.path().join("link")).unwrap();

        let snapshot = collect(dir.path(), &["link"]);

        let UntrackedContent::Symlink(target) = &snapshot.entries[0].content else {
            panic!("expected a symlink entry");
        };
        assert_eq!(target, "real.txt");
    }

    #[test]
    fn an_empty_file_is_empty_rather_than_a_blank_patch() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();

        let snapshot = collect(dir.path(), &["empty.txt"]);

        assert!(matches!(
            snapshot.entries[0].content,
            UntrackedContent::Empty
        ));
        assert!(!snapshot.incomplete);
    }
}
