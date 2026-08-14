//! What a worktree actually changed, relative to the default branch.
//!
//! This is the missing piece for a `Review` verdict. yawm can say a worktree is
//! unmerged, but "should I keep this?" is really "what is in here?" — and
//! answering that otherwise means leaving the app for a terminal.
//!
//! Comparison uses the three-dot form (`main...HEAD`), which diffs against the
//! merge base rather than the branch tip. That shows what *this* worktree
//! changed, and not the unrelated commits that landed on main in the meantime.
//!
//! Two things the reader asks are deliberately kept apart. "What has this
//! branch committed?" and "what is sitting uncommitted on disk?" are different
//! questions with different answers, and a single blended total that answers
//! neither — a header reading `+2177 −0 branch history` over a list that is all
//! untracked files — is worse than no header at all. Every caller therefore
//! names the [`DiffScope`] it is asking about, and totals for each side are
//! counted separately rather than summed out of a merged list.

mod sections;
mod untracked;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git::{Git, StreamControl};
use crate::model::WorktreeEntry;

use sections::{PatchSectionCollector, classify_section};
use untracked::{UntrackedContent, UntrackedEntry, UntrackedSnapshot, untracked_snapshot};

const MAX_NUMSTAT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DIFF_HEADER_LEN: usize = b"diff --combined ".len();

/// Which question the caller is asking.
///
/// Clicking a worktree's uncommitted count asks one thing only, and answering
/// it with branch history mixed in is not a fuller answer, it is a wrong one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffScope {
    /// Only what is on disk and in the index: no commits are read.
    Uncommitted,
    /// Commits this branch has that the base does not, plus the working tree.
    #[default]
    History,
}

impl DiffScope {
    pub fn includes_history(self) -> bool {
        matches!(self, Self::History)
    }
}

/// Where a file's change lives: in a commit, only on disk, or both.
///
/// A worktree an agent left behind is usually all `Uncommitted`, and the
/// distinction is the difference between work that is safe somewhere and work
/// that exists only in this directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeOrigin {
    #[default]
    Committed,
    Uncommitted,
    /// Committed here, then modified again without committing.
    Both,
}

/// Which sort of repository a nested directory turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RepositoryKind {
    /// An ordinary repository with its own `.git` directory.
    Nested,
    /// A worktree linked to a repository elsewhere.
    LinkedWorktree,
    /// A repository with no working tree of its own.
    Bare,
}

/// What a changed path *is*, decided once in the backend.
///
/// The frontend used to guess this by reading the rendered patch text, which
/// meant a file whose contents happened to mention `Binary files` was treated
/// as binary, and every non-text row still got an expander that opened onto
/// nothing. Naming the kind here makes the impossible cases unrepresentable:
/// only [`EntryContent::Text`] carries a patch, and it always has a hunk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FileKind {
    #[default]
    Text,
    Binary,
    Empty,
    Symlink {
        target: String,
    },
    /// One directory Git reported as a single untracked path.
    Directory {
        /// Raw paths from Git that this entry stands for.
        paths: u32,
        /// Things immediately inside it, when they could be counted.
        items: Option<u32>,
    },
    /// A repository nested inside this worktree, kept whole.
    Repository {
        repository: RepositoryKind,
        paths: u32,
        items: Option<u32>,
    },
    /// A change with no line changes at all: a mode change, a pure rename.
    Metadata {
        detail: String,
    },
    /// The path is real, but its contents were deliberately not read.
    Unread {
        detail: String,
    },
}

/// One file's contribution to a diff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    /// Lossless path bytes used only while merging summary passes.
    ///
    /// `path` remains the serialized/display API. Keeping the raw identity out
    /// of the wire format avoids changing that API while ensuring a quoted
    /// valid filename cannot alias an invalid UTF-8 path with the same display.
    #[serde(skip)]
    #[doc(hidden)]
    pub identity: Vec<u8>,
    pub insertions: u32,
    pub deletions: u32,
    pub origin: ChangeOrigin,
    #[serde(flatten)]
    pub kind: FileKind,
}

impl DiffFile {
    pub fn is_text(&self) -> bool {
        matches!(self.kind, FileKind::Text)
    }
}

/// Added and removed lines over some set of files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffTotals {
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

impl DiffTotals {
    pub fn is_empty(&self) -> bool {
        self.files == 0 && self.insertions == 0 && self.deletions == 0
    }
}

/// Why a diff shows less than everything, said precisely enough to act on.
///
/// "Not fully verified" tells a reader nothing they can do. Each of these
/// carries the number that was reached and, where it is knowable, the names
/// that were skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiffLimit {
    /// The patch byte budget ran out before every untracked path was rendered.
    DisplayLimit { shown: u32, total: u32 },
    /// More untracked paths exist than the inspection cap allows.
    InspectionLimit { limit: u32, shown: u32, total: u32 },
    /// Named paths could not be read at all.
    Unreadable { paths: Vec<String>, total: u32 },
    /// Named paths are larger than the per-file read limit.
    TooLarge { paths: Vec<String>, total: u32 },
    /// The worktree-wide read budget was spent before these paths.
    ReadBudget { paths: Vec<String>, total: u32 },
    /// `git ls-files` itself failed.
    ListingFailed,
}

/// A worktree's changes against the default branch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    /// The question this summary answers.
    pub scope: DiffScope,
    /// What the comparison was made against, e.g. `origin/main`.
    pub base: Option<String>,
    /// Commits on this worktree that the base does not have. Always zero in
    /// [`DiffScope::Uncommitted`], which reads no commits.
    pub commits: u32,
    pub files: Vec<DiffFile>,
    /// Totals for commits only. Never blended with `working`.
    pub history: DiffTotals,
    /// Totals for the index and the working tree, untracked paths included.
    pub working: DiffTotals,
    /// Uncommitted changes are present. In `History` scope they are also listed
    /// in `files`, since they are part of "what is in here".
    pub includes_uncommitted: bool,
    /// Untracked paths reported by Git, before any grouping.
    pub untracked_total: u32,
    /// Untracked raw paths an entry stands for.
    pub untracked_included: u32,
    /// Rows the untracked side contributes, after nested repositories are kept
    /// whole. Lower than `untracked_total` when grouping happened.
    pub untracked_entries: u32,
    /// Some changed paths were not fully read. Always accompanied by `limits`.
    pub incomplete: bool,
    pub limits: Vec<DiffLimit>,
}

impl DiffSummary {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Summarise a worktree's changes against the default branch.
///
/// `base` is normally the repository's default ref. With no base, or a worktree
/// whose directory is gone, the result is empty rather than an error: a missing
/// comparison is a reason to show nothing, not to fail.
pub fn summarise(
    git: &Git,
    entry: &WorktreeEntry,
    base: Option<&str>,
    scope: DiffScope,
) -> DiffSummary {
    summarise_with_untracked(git, entry, base, scope, &untracked_snapshot(git, entry))
}

fn summarise_with_untracked(
    git: &Git,
    entry: &WorktreeEntry,
    base: Option<&str>,
    scope: DiffScope,
    untracked: &UntrackedSnapshot,
) -> DiffSummary {
    let mut summary = DiffSummary {
        scope,
        base: base.map(str::to_string),
        untracked_total: untracked.total,
        untracked_included: untracked.represented,
        untracked_entries: untracked.entries.len().try_into().unwrap_or(u32::MAX),
        incomplete: untracked.incomplete,
        limits: untracked.limits.clone(),
        ..Default::default()
    };

    if entry.prunable.is_some() || !entry.path.is_dir() {
        return summary;
    }

    let mut history_files: Vec<DiffFile> = Vec::new();
    if let Some(base) = base.filter(|_| scope.includes_history()) {
        summary.commits = count_commits(git, &entry.path, base).unwrap_or(0);

        // Three dots: compare against the merge base, so commits that landed
        // on the default branch after this worktree diverged are not counted
        // as its work.
        let range = format!("{base}...HEAD");
        match run_numstat(git, &entry.path, &["diff", "--numstat", "-z", &range]) {
            Ok((files, incomplete)) => {
                history_files = files;
                summary.incomplete |= incomplete;
            }
            Err(_) => summary.incomplete = true,
        }
    }

    // `git diff HEAD` includes both the index and the worktree, so staged-only
    // changes are covered. Git deliberately omits untracked paths; those are
    // folded in from the bounded snapshot below.
    let working_outputs = if has_head(entry) {
        vec![run_numstat(
            git,
            &entry.path,
            &["diff", "--numstat", "-z", "HEAD"],
        )]
    } else if let Ok(empty_tree) = empty_tree(git, &entry.path) {
        vec![run_numstat(
            git,
            &entry.path,
            &["diff", "--numstat", "-z", &empty_tree],
        )]
    } else {
        summary.incomplete = true;
        Vec::new()
    };
    let mut working_files: Vec<DiffFile> = Vec::new();
    for output in working_outputs {
        match output {
            Ok((files, incomplete)) => {
                summary.incomplete |= incomplete;
                if !files.is_empty() {
                    summary.includes_uncommitted = true;
                    merge_files(
                        &mut working_files,
                        files,
                        ChangeOrigin::Uncommitted,
                        ChangeOrigin::Uncommitted,
                    );
                }
            }
            Err(_) => summary.incomplete = true,
        }
    }
    if untracked.total > 0 {
        summary.includes_uncommitted = true;
    }
    merge_files(
        &mut working_files,
        untracked.entries.iter().map(untracked_diff_file).collect(),
        ChangeOrigin::Uncommitted,
        ChangeOrigin::Uncommitted,
    );

    summary.history = totals(&history_files);
    summary.working = totals(&working_files);

    summary.files = history_files;
    merge_files(
        &mut summary.files,
        working_files,
        ChangeOrigin::Uncommitted,
        ChangeOrigin::Both,
    );
    summary.files.sort_by(|a, b| {
        (b.insertions + b.deletions)
            .cmp(&(a.insertions + a.deletions))
            .then_with(|| a.path.cmp(&b.path))
    });
    summary
}

fn totals(files: &[DiffFile]) -> DiffTotals {
    DiffTotals {
        files: files.len().try_into().unwrap_or(u32::MAX),
        insertions: files.iter().map(|file| file.insertions).sum(),
        deletions: files.iter().map(|file| file.deletions).sum(),
    }
}

/// Produce the complete UI payload while reading untracked files only once.
pub fn inspect(
    git: &Git,
    entry: &WorktreeEntry,
    base: Option<&str>,
    max_bytes: usize,
    scope: DiffScope,
) -> Result<DiffInspection> {
    let untracked = untracked_snapshot(git, entry);
    Ok(DiffInspection {
        summary: summarise_with_untracked(git, entry, base, scope, &untracked),
        patches: patches_with_untracked(git, entry, base, max_bytes, scope, &untracked)?,
    })
}

/// What one file section of a diff renders as.
///
/// Only `Text` carries a patch, and `hunks` is never zero when it does — the
/// frontend can therefore hand every `Text` entry to the patch viewer and
/// nothing else, without inspecting the string first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EntryContent {
    Text {
        patch: String,
        hunks: u32,
    },
    Binary,
    Empty,
    Symlink {
        target: String,
    },
    Directory {
        paths: u32,
        items: Option<u32>,
    },
    Repository {
        repository: RepositoryKind,
        paths: u32,
        items: Option<u32>,
    },
    Metadata {
        detail: String,
    },
    Unread {
        detail: String,
    },
}

impl EntryContent {
    pub fn patch(&self) -> Option<&str> {
        match self {
            Self::Text { patch, .. } => Some(patch),
            _ => None,
        }
    }

    pub fn hunks(&self) -> u32 {
        match self {
            Self::Text { hunks, .. } => *hunks,
            _ => 0,
        }
    }

    fn bytes(&self) -> usize {
        self.patch().map_or(0, str::len)
    }

    fn to_file_kind(&self) -> FileKind {
        match self {
            Self::Text { .. } => FileKind::Text,
            Self::Binary => FileKind::Binary,
            Self::Empty => FileKind::Empty,
            Self::Symlink { target } => FileKind::Symlink {
                target: target.clone(),
            },
            Self::Directory { paths, items } => FileKind::Directory {
                paths: *paths,
                items: *items,
            },
            Self::Repository {
                repository,
                paths,
                items,
            } => FileKind::Repository {
                repository: *repository,
                paths: *paths,
                items: *items,
            },
            Self::Metadata { detail } => FileKind::Metadata {
                detail: detail.clone(),
            },
            Self::Unread { detail } => FileKind::Unread {
                detail: detail.clone(),
            },
        }
    }
}

/// One renderable row of a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEntry {
    pub path: String,
    pub origin: ChangeOrigin,
    pub insertions: u32,
    pub deletions: u32,
    #[serde(flatten)]
    pub content: EntryContent,
}

/// A worktree's changes, split by where they live.
///
/// Two lists rather than one blended diff: "landed on this branch" and "only on
/// disk here" answer different questions, and merging them into one scroll
/// hides which is which. Either may be empty, and in
/// [`DiffScope::Uncommitted`] the committed side is not even read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Patches {
    pub scope: DiffScope,
    /// Commits this branch has that the base does not.
    pub committed: Vec<DiffEntry>,
    /// Working-tree changes that belong to no commit yet.
    pub uncommitted: Vec<DiffEntry>,
    /// The global patch byte budget was exhausted.
    pub truncated: bool,
    /// Untracked inspection or rendering stopped short. Always accompanied by
    /// `limits`, which say exactly why and by how much.
    pub incomplete: bool,
    /// Untracked paths reported by Git, before grouping.
    pub untracked_total: u32,
    /// Untracked raw paths represented in `uncommitted`.
    pub untracked_shown: u32,
    /// Rows those paths were grouped into.
    pub untracked_entries: u32,
    pub limits: Vec<DiffLimit>,
}

impl Patches {
    /// Bytes of actual patch text across both sides.
    pub fn patch_bytes(&self) -> usize {
        self.committed
            .iter()
            .chain(&self.uncommitted)
            .map(|entry| entry.content.bytes())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.committed.is_empty() && self.uncommitted.is_empty()
    }
}

/// A summary and its patches made from the same bounded untracked snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffInspection {
    pub summary: DiffSummary,
    pub patches: Patches,
}

/// The patches themselves, for reading in the UI.
///
/// The committed side is `merge-base..HEAD`; the uncommitted side is the
/// working tree against `HEAD`. Together they cover the same ground as the
/// scope's totals report, so the header and the body always agree — a worktree
/// an agent left mid-task has no commits at all, and showing only the committed
/// side is how the panel ended up blank while its own header claimed twenty
/// changed files.
///
/// Capped because a large refactor can run to megabytes, and the panel only
/// ever shows the beginning.
pub fn patches(
    git: &Git,
    entry: &WorktreeEntry,
    base: Option<&str>,
    max_bytes: usize,
    scope: DiffScope,
) -> Result<Patches> {
    let untracked = untracked_snapshot(git, entry);
    patches_with_untracked(git, entry, base, max_bytes, scope, &untracked)
}

fn patches_with_untracked(
    git: &Git,
    entry: &WorktreeEntry,
    base: Option<&str>,
    max_bytes: usize,
    scope: DiffScope,
    untracked: &UntrackedSnapshot,
) -> Result<Patches> {
    let mut patches = Patches {
        scope,
        incomplete: untracked.incomplete,
        untracked_total: untracked.total,
        untracked_entries: untracked.entries.len().try_into().unwrap_or(u32::MAX),
        limits: untracked.limits.clone(),
        ..Default::default()
    };

    if let Some(base) = base.filter(|_| scope.includes_history() && has_head(entry)) {
        let merge_base = merge_base(git, &entry.path, base);
        let against = merge_base.as_deref().unwrap_or(base);
        let entries = collect_entries(
            git,
            entry,
            &[against, "HEAD"],
            max_bytes,
            ChangeOrigin::Committed,
            &mut patches.truncated,
        )?;
        patches.committed = entries;
    }

    if has_head(entry) {
        let remaining = max_bytes.saturating_sub(patches.patch_bytes());
        let entries = collect_entries(
            git,
            entry,
            &["HEAD"],
            remaining,
            ChangeOrigin::Uncommitted,
            &mut patches.truncated,
        )?;
        patches.uncommitted = entries;
    } else {
        let empty_tree = empty_tree(git, &entry.path)?;
        let remaining = max_bytes.saturating_sub(patches.patch_bytes());
        patches.uncommitted = collect_entries(
            git,
            entry,
            &[&empty_tree],
            remaining,
            ChangeOrigin::Uncommitted,
            &mut patches.truncated,
        )?;
    }

    for file in &untracked.entries {
        let rendered = untracked_entry(file);
        let used = patches.patch_bytes();
        if rendered.content.bytes() > max_bytes.saturating_sub(used) {
            patches.truncated = true;
            break;
        }
        patches.untracked_shown = patches.untracked_shown.saturating_add(file.paths);
        patches.uncommitted.push(rendered);
    }

    if patches.untracked_shown < untracked.represented {
        patches.limits.push(DiffLimit::DisplayLimit {
            shown: patches.untracked_shown,
            total: untracked.total,
        });
    }
    patches.incomplete = patches.incomplete || patches.truncated || !patches.limits.is_empty();
    Ok(patches)
}

fn has_head(entry: &WorktreeEntry) -> bool {
    entry
        .head
        .as_deref()
        .is_some_and(|head| head.bytes().any(|byte| byte != b'0'))
}

fn empty_tree(git: &Git, path: &Path) -> Result<String> {
    let output = git.run(path, &["hash-object", "-t", "tree", "--stdin"])?;
    let oid = std::str::from_utf8(&output)
        .ok()
        .map(str::trim)
        .filter(|oid| {
            matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| Error::Parse("git did not return an empty-tree object ID".to_string()))?;
    Ok(oid.to_string())
}

fn collect_entries(
    git: &Git,
    entry: &WorktreeEntry,
    revs: &[&str],
    max_bytes: usize,
    origin: ChangeOrigin,
    truncated: &mut bool,
) -> Result<Vec<DiffEntry>> {
    let mut args = vec![
        "diff",
        "--no-color",
        // Rename detection keeps a moved file from reading as a whole-file
        // delete plus a whole-file add.
        "--find-renames",
    ];
    args.extend_from_slice(revs);

    let mut collector = PatchSectionCollector::new(max_bytes);
    git.run_stream(&entry.path, &args, |chunk| Ok(collector.consume(chunk)))?;
    let patch = collector.finish();
    *truncated |= patch.truncated;

    Ok(patch
        .sections
        .iter()
        .map(|section| {
            let parsed = classify_section(section);
            DiffEntry {
                path: parsed.path,
                origin,
                insertions: parsed.insertions,
                deletions: parsed.deletions,
                content: parsed.content,
            }
        })
        .collect())
}

fn untracked_entry(file: &UntrackedEntry) -> DiffEntry {
    let (content, insertions) = match &file.content {
        UntrackedContent::Text(bytes) => (
            EntryContent::Text {
                patch: render_untracked_patch(file, bytes),
                hunks: 1,
            },
            line_count(bytes),
        ),
        UntrackedContent::Binary => (EntryContent::Binary, 0),
        UntrackedContent::Empty => (EntryContent::Empty, 0),
        UntrackedContent::Symlink(target) => (
            EntryContent::Symlink {
                target: target.clone(),
            },
            0,
        ),
        UntrackedContent::Directory { items } => (
            EntryContent::Directory {
                paths: file.paths,
                items: *items,
            },
            0,
        ),
        UntrackedContent::Repository { repository, items } => (
            EntryContent::Repository {
                repository: *repository,
                paths: file.paths,
                items: *items,
            },
            0,
        ),
        UntrackedContent::Unread(reason) => (
            EntryContent::Unread {
                detail: reason.detail(),
            },
            0,
        ),
    };
    DiffEntry {
        path: file.path.clone(),
        origin: ChangeOrigin::Uncommitted,
        insertions,
        deletions: 0,
        content,
    }
}

fn untracked_diff_file(file: &UntrackedEntry) -> DiffFile {
    let entry = untracked_entry(file);
    DiffFile {
        path: entry.path,
        identity: file.raw_path.clone(),
        insertions: entry.insertions,
        deletions: entry.deletions,
        origin: ChangeOrigin::Uncommitted,
        kind: entry.content.to_file_kind(),
    }
}

fn line_count(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    let lines = newlines + usize::from(!bytes.ends_with(b"\n"));
    lines.try_into().unwrap_or(u32::MAX)
}

/// Synthesise the patch Git will not produce for an untracked file.
///
/// Only ever called for text with at least one line, so the hunk header it
/// writes is always a real one.
fn render_untracked_patch(file: &UntrackedEntry, bytes: &[u8]) -> String {
    let a_path = quote_git_path(b"a/", &file.raw_path);
    let b_path = quote_git_path(b"b/", &file.raw_path);
    let count = line_count(bytes).max(1);
    let range = if count == 1 {
        "+1".to_string()
    } else {
        format!("+1,{count}")
    };
    let mut patch = format!(
        "diff --git {a_path} {b_path}\nnew file mode {}\n--- /dev/null\n+++ {b_path}\n@@ -0,0 {range} @@\n",
        file.mode
    );
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        patch.push('+');
        patch.push_str(&String::from_utf8_lossy(line));
    }
    if !bytes.ends_with(b"\n") {
        patch.push_str("\n\\ No newline at end of file\n");
    }
    patch
}

fn quote_git_path(prefix: &[u8], path: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(prefix.len() + path.len());
    bytes.extend_from_slice(prefix);
    bytes.extend_from_slice(path);
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
    {
        return String::from_utf8(bytes).expect("ASCII path");
    }

    let mut quoted = String::from("\"");
    for byte in bytes {
        match byte {
            b'\\' => quoted.push_str("\\\\"),
            b'"' => quoted.push_str("\\\""),
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\t' => quoted.push_str("\\t"),
            0x20..=0x7e => quoted.push(char::from(byte)),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

fn display_git_path(path: &[u8]) -> String {
    std::str::from_utf8(path)
        .map(str::to_owned)
        .unwrap_or_else(|_| quote_git_path(&[], path))
}

/// Where this worktree diverged from `base`.
fn merge_base(git: &Git, path: &Path, base: &str) -> Option<String> {
    let (ok, out) = git.run_checked(path, &["merge-base", base, "HEAD"]).ok()?;
    if !ok {
        return None;
    }
    let text = String::from_utf8_lossy(&out).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Commits present in the worktree but not in the base.
fn count_commits(git: &Git, path: &Path, base: &str) -> Option<u32> {
    let range = format!("{base}..HEAD");
    let (ok, out) = git
        .run_checked(path, &["rev-list", "--count", &range])
        .ok()?;
    if !ok {
        return None;
    }
    String::from_utf8_lossy(&out).trim().parse().ok()
}

fn run_numstat(git: &Git, path: &Path, args: &[&str]) -> Result<(Vec<DiffFile>, bool)> {
    let mut output = Vec::new();
    let mut incomplete = false;
    git.run_stream(path, args, |chunk| {
        let keep = chunk
            .len()
            .min(MAX_NUMSTAT_BYTES.saturating_sub(output.len()));
        output.extend_from_slice(&chunk[..keep]);
        incomplete |= keep < chunk.len();
        Ok(if incomplete {
            StreamControl::Saturated
        } else {
            StreamControl::Continue
        })
    })?;

    if incomplete {
        match output.iter().rposition(|byte| *byte == 0) {
            Some(last_complete) => output.truncate(last_complete + 1),
            None => output.clear(),
        }
    } else if !output.is_empty() && !output.ends_with(&[0]) {
        output.clear();
        incomplete = true;
    }
    Ok((parse_numstat(&output), incomplete))
}

/// Parse `git diff --numstat -z`.
///
/// Each record is `insertions\tdeletions\tpath`, NUL-terminated. Binary files
/// report `-` for both counts.
///
/// A rename or copy is written as three records instead of one: the counts
/// with an empty path, then the old path, then the new one. Skipping the whole
/// group — which is what parsing each record independently did — dropped the
/// file from the summary entirely, so the header counted one fewer file than
/// the diff beside it listed, and the renamed file, absent from the summary,
/// rendered as `+0 −0` in the tree.
fn parse_numstat(bytes: &[u8]) -> Vec<DiffFile> {
    let nul_separated = bytes.contains(&0);
    let separator = if nul_separated { 0 } else { b'\n' };

    let mut files = Vec::new();
    let mut records = bytes.split(|b| *b == separator);
    while let Some(record) = records.next() {
        let record = record.strip_suffix(b"\r").unwrap_or(record);
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, |byte| *byte == b'\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };

        // The new path, not the old one: the patch beside this list names every
        // file by its b-side, so an old name would be a row the reader cannot
        // find in the diff.
        let raw_path = if path.is_empty() && nul_separated {
            let (Some(_old), Some(new)) = (records.next(), records.next()) else {
                continue;
            };
            new
        } else {
            path
        };
        let path = display_git_path(raw_path);
        if path.is_empty() {
            continue;
        }

        let binary = added == b"-" || removed == b"-";
        files.push(DiffFile {
            path,
            identity: raw_path.to_vec(),
            insertions: std::str::from_utf8(added)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            deletions: std::str::from_utf8(removed)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            kind: if binary {
                FileKind::Binary
            } else {
                FileKind::Text
            },
            // Corrected by `merge_files` for the working-tree pass; numstat
            // itself does not know which side it was asked about.
            origin: ChangeOrigin::Committed,
        });
    }
    files
}

/// Fold one pass of changed files into another, keeping the larger count per
/// file so a file changed on both sides is not double counted, and recording
/// where each file's change came from.
fn merge_files(
    into: &mut Vec<DiffFile>,
    extra: Vec<DiffFile>,
    origin: ChangeOrigin,
    conflict: ChangeOrigin,
) {
    for mut file in extra {
        file.origin = origin;
        match into
            .iter_mut()
            .find(|existing| diff_file_identity(existing) == diff_file_identity(&file))
        {
            Some(existing) => {
                existing.insertions = existing.insertions.max(file.insertions);
                existing.deletions = existing.deletions.max(file.deletions);
                if matches!(existing.kind, FileKind::Text) {
                    existing.kind = file.kind;
                }
                existing.origin = conflict;
            }
            None => into.push(file),
        }
    }
}

fn diff_file_identity(file: &DiffFile) -> &[u8] {
    if file.identity.is_empty() {
        file.path.as_bytes()
    } else {
        &file.identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nul(records: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in records {
            out.extend_from_slice(r.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn parses_numstat_records() {
        let input = nul(&["10\t2\tsrc/main.rs", "0\t7\tREADME.md"]);
        let files = parse_numstat(&input);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].insertions, 10);
        assert_eq!(files[0].deletions, 2);
        assert_eq!(files[1].deletions, 7);
    }

    #[test]
    fn binary_files_report_no_line_counts() {
        let files = parse_numstat(&nul(&["-\t-\tlogo.png"]));

        assert_eq!(files[0].kind, FileKind::Binary);
        assert_eq!(files[0].insertions, 0);
        assert_eq!(files[0].deletions, 0);
    }

    #[test]
    fn paths_with_spaces_survive() {
        let files = parse_numstat(&nul(&["1\t1\tmy folder/a file.txt"]));
        assert_eq!(files[0].path, "my folder/a file.txt");
    }

    #[test]
    fn non_utf8_paths_keep_distinct_escaped_identities() {
        let mut input = b"1\t0\todd-\xff\0".to_vec();
        input.extend_from_slice(b"1\t0\todd-\xfe\0");

        let files = parse_numstat(&input);

        assert_eq!(files.len(), 2);
        assert_ne!(files[0].path, files[1].path);
        assert_eq!(files[0].path, "\"odd-\\377\"");
        assert_eq!(files[1].path, "\"odd-\\376\"");
    }

    #[test]
    fn parses_newline_separated_fallback() {
        let files = parse_numstat(b"3\t1\ta.rs\n2\t0\tb.rs\n");
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(parse_numstat(b"").is_empty());
    }

    #[test]
    fn malformed_records_are_skipped() {
        let files = parse_numstat(&nul(&["garbage", "5\t5\tok.rs"]));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "ok.rs");
    }

    /// The rename record is three fields, and dropping it lost a whole file:
    /// the header said 96 files while the diff listed 97.
    #[test]
    fn renames_are_recorded_under_their_new_path() {
        let files = parse_numstat(&nul(&[
            "1\t0\ta.rs",
            "48\t40\t",
            "src/Old.ts",
            "src/nested/New.ts",
            "2\t2\tz.rs",
        ]));

        assert_eq!(files.len(), 3);
        assert_eq!(files[1].path, "src/nested/New.ts");
        assert_eq!(files[1].insertions, 48);
        assert_eq!(files[1].deletions, 40);
        assert_eq!(files[2].path, "z.rs", "the record after a rename is intact");
    }

    #[test]
    fn a_binary_rename_is_still_one_file() {
        let files = parse_numstat(&nul(&["-\t-\t", "old.png", "art/new.png"]));

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "art/new.png");
        assert_eq!(files[0].kind, FileKind::Binary);
    }

    #[test]
    fn a_truncated_rename_record_is_dropped_rather_than_guessed() {
        let files = parse_numstat(&nul(&["3\t1\ta.rs", "48\t40\t", "src/Old.ts"]));

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.rs");
    }

    /// A file changed both in a commit and in the working tree must appear once.
    #[test]
    fn merging_keeps_one_entry_per_file() {
        let mut committed = vec![DiffFile {
            path: "a.rs".into(),
            insertions: 5,
            deletions: 1,
            ..Default::default()
        }];
        let working = vec![
            DiffFile {
                path: "a.rs".into(),
                insertions: 9,
                deletions: 0,
                ..Default::default()
            },
            DiffFile {
                path: "b.rs".into(),
                insertions: 2,
                deletions: 2,
                ..Default::default()
            },
        ];

        merge_files(
            &mut committed,
            working,
            ChangeOrigin::Uncommitted,
            ChangeOrigin::Both,
        );

        assert_eq!(committed.len(), 2);
        let a = committed.iter().find(|f| f.path == "a.rs").unwrap();
        assert_eq!(a.insertions, 9, "keeps the larger count");
        assert_eq!(a.deletions, 1);
        assert_eq!(a.origin, ChangeOrigin::Both);
    }

    #[cfg(unix)]
    #[test]
    fn merging_does_not_alias_a_raw_path_with_its_literal_quoted_display() {
        let invalid = b"odd-\xff".to_vec();
        let display = display_git_path(&invalid);
        let mut committed = parse_numstat(
            [b"1\t0\t".as_slice(), invalid.as_slice(), b"\0"]
                .concat()
                .as_slice(),
        );
        let working = parse_numstat(format!("2\t0\t{display}\0").as_bytes());

        assert_eq!(committed[0].path, working[0].path);
        merge_files(
            &mut committed,
            working,
            ChangeOrigin::Uncommitted,
            ChangeOrigin::Both,
        );

        assert_eq!(committed.len(), 2);
        assert_ne!(committed[0].identity, committed[1].identity);
    }

    #[test]
    fn totals_count_each_side_separately() {
        let history = vec![DiffFile {
            path: "a.rs".into(),
            insertions: 10,
            deletions: 3,
            ..Default::default()
        }];
        let working = vec![DiffFile {
            path: "b.rs".into(),
            insertions: 4,
            deletions: 0,
            ..Default::default()
        }];

        assert_eq!(
            totals(&history),
            DiffTotals {
                files: 1,
                insertions: 10,
                deletions: 3
            }
        );
        assert_eq!(
            totals(&working),
            DiffTotals {
                files: 1,
                insertions: 4,
                deletions: 0
            }
        );
    }

    #[test]
    fn a_synthesised_untracked_patch_always_has_a_hunk() {
        let entry = untracked_entry(&UntrackedEntry {
            raw_path: b"notes.txt".to_vec(),
            path: "notes.txt".into(),
            mode: "100644",
            paths: 1,
            content: UntrackedContent::Text(b"one\ntwo\n".to_vec()),
        });

        let EntryContent::Text { patch, hunks } = &entry.content else {
            panic!("expected text");
        };
        assert_eq!(*hunks, 1);
        assert!(patch.contains("@@ -0,0 +1,2 @@\n+one\n+two\n"));
        assert_eq!(entry.insertions, 2);
    }

    #[test]
    fn an_empty_untracked_file_carries_no_patch_at_all() {
        let entry = untracked_entry(&UntrackedEntry {
            raw_path: b"empty.txt".to_vec(),
            path: "empty.txt".into(),
            mode: "100644",
            paths: 1,
            content: UntrackedContent::Empty,
        });

        assert_eq!(entry.content, EntryContent::Empty);
        assert_eq!(entry.content.patch(), None);
    }

    #[test]
    fn quoted_binary_paths_survive_without_a_patch() {
        let entry = untracked_entry(&UntrackedEntry {
            raw_path: b"my image.bin".to_vec(),
            path: "my image.bin".into(),
            mode: "100644",
            paths: 1,
            content: UntrackedContent::Binary,
        });

        assert_eq!(entry.content, EntryContent::Binary);
        assert_eq!(entry.path, "my image.bin");
    }

    #[test]
    fn a_summary_without_a_base_is_empty() {
        let entry = WorktreeEntry {
            path: "/nowhere".into(),
            ..Default::default()
        };
        let summary = summarise(&Git::new(), &entry, None, DiffScope::History);

        assert!(summary.is_empty());
        assert!(summary.base.is_none());
    }

    #[test]
    fn a_missing_directory_yields_an_empty_summary() {
        let entry = WorktreeEntry {
            path: "/definitely/not/here".into(),
            prunable: Some("gone".into()),
            ..Default::default()
        };
        assert!(summarise(&Git::new(), &entry, Some("main"), DiffScope::History).is_empty());
    }
}
