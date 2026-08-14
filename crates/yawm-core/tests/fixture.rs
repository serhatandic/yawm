//! End-to-end test against a real git repository.
//!
//! The unit tests cover parsing and classification in isolation; this builds an
//! actual repository containing every worktree state yawm claims to recognise
//! and drives the full pipeline over it. It is the test that would catch a
//! wrong assumption about what git actually emits.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use yawm_core::git::Git;
use yawm_core::git::collect::{list_worktrees, load_context, status_for};
use yawm_core::model::{
    Landing, LandingProof, UnknownReason, Verdict, VerdictReason, WorktreeEntry,
};
use yawm_core::path::path_key;
use yawm_core::verdict::{VerdictConfig, classify};
use yawm_core::{Config, LandingCache, ScanOptions, Scanner};

const DAY: i64 = 24 * 60 * 60;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    /// Build a repository with a remote and one worktree per verdict.
    fn build() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        // macOS reports /var as a symlink to /private/var; canonicalize so the
        // paths git returns match the ones we constructed.
        let root = dir.path().canonicalize().expect("canonicalize");

        git(&root, &["init", "-q", "--bare", "remote.git"]);
        git(&root, &["init", "-q", "-b", "main", "repo"]);

        let repo = root.join("repo");
        git(&repo, &["config", "user.email", "test@yawm.dev"]);
        git(&repo, &["config", "user.name", "yawm test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);

        std::fs::write(repo.join("a.txt"), "hello").unwrap();
        std::fs::write(repo.join(".gitignore"), ".env*\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        git(&repo, &["remote", "add", "origin", "../remote.git"]);
        git(&repo, &["push", "-q", "-u", "origin", "main"]);

        // Merged by ancestry: committed on a branch, merged into main, pushed.
        wt(&repo, "wt-merged", "feat/merged");
        commit_empty(&root.join("wt-merged"), "merged work");
        git(
            &repo,
            &["merge", "-q", "--no-ff", "feat/merged", "-m", "merge"],
        );
        git(&repo, &["push", "-q", "origin", "main"]);

        // Merged into local main but never pushed. The work is preserved
        // outside the worktree either way, so this is still disposable.
        wt(&repo, "wt-local-merged", "feat/local-merged");
        commit_empty(&root.join("wt-local-merged"), "local work");
        git(
            &repo,
            &[
                "merge",
                "-q",
                "--no-ff",
                "feat/local-merged",
                "-m",
                "local merge",
            ],
        );

        // A real squash merge: the target receives the branch's tree effect
        // without inheriting its commit.
        wt(&repo, "wt-gone", "feat/gone");
        let gone = root.join("wt-gone");
        std::fs::write(gone.join("squashed.txt"), "landed through squash\n").unwrap();
        git(&gone, &["add", "-A"]);
        git(&gone, &["commit", "-qm", "squashed work"]);
        git(&gone, &["push", "-q", "-u", "origin", "feat/gone"]);
        git(&repo, &["merge", "-q", "--squash", "feat/gone"]);
        git(&repo, &["commit", "-qm", "land squashed work"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["push", "-q", "origin", "--delete", "feat/gone"]);

        // Unpushed commit plus every flavour of dirty file.
        wt(&repo, "wt-dirty", "feat/dirty");
        let dirty = root.join("wt-dirty");
        git(&dirty, &["push", "-q", "-u", "origin", "feat/dirty"]);
        commit_empty(&dirty, "unpushed");
        std::fs::write(dirty.join("staged.txt"), "s").unwrap();
        git(&dirty, &["add", "staged.txt"]);
        std::fs::write(dirty.join("a.txt"), "modified").unwrap();
        std::fs::write(dirty.join("untracked.txt"), "u").unwrap();
        // Gitignored and unrecoverable: must be surfaced before any deletion.
        std::fs::write(dirty.join(".env"), "SECRET=1").unwrap();
        std::fs::write(dirty.join(".env.local"), "LOCAL=1").unwrap();

        wt(&repo, "wt-staged-only", "feat/staged-only");
        let staged_only = root.join("wt-staged-only");
        std::fs::write(staged_only.join("a.txt"), "only in the index\n").unwrap();
        git(&staged_only, &["add", "a.txt"]);

        wt(&repo, "wt-locked", "feat/locked");
        git(
            &repo,
            &[
                "worktree",
                "lock",
                "../wt-locked",
                "--reason",
                "agent running",
            ],
        );

        // Clean, never pushed, and holding content absent from main.
        wt(&repo, "wt-review", "feat/review");
        let review = root.join("wt-review");
        std::fs::write(review.join("unfinished.txt"), "not on main\n").unwrap();
        git(&review, &["add", "-A"]);
        git(&review, &["commit", "-qm", "unfinished work"]);

        // Real changes to inspect. Branched from origin/main, the way a feature
        // branch normally is, so its commit count reflects only its own work.
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "../wt-diff",
                "-b",
                "feat/diff",
                "origin/main",
            ],
        );
        let diff_tree = root.join("wt-diff");
        std::fs::write(diff_tree.join("feature.txt"), "the new feature\n").unwrap();
        git(&diff_tree, &["add", "-A"]);
        git(&diff_tree, &["commit", "-qm", "add the feature"]);
        // Plus an edit that was never committed, so this worktree exercises
        // both sides of the split at once — a committed patch that must not
        // contain it, and an uncommitted patch that must.
        std::fs::write(diff_tree.join("a.txt"), "edited but not committed\n").unwrap();

        // Diverges from main in the same lines main later changed, so merging
        // it back would conflict. Committed, because a merge only ever sees
        // committed work.
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "../wt-conflict",
                "-b",
                "feat/conflict",
                "origin/main",
            ],
        );
        let conflict_tree = root.join("wt-conflict");
        std::fs::write(conflict_tree.join("a.txt"), "theirs").unwrap();
        git(&conflict_tree, &["commit", "-qam", "diverge"]);
        // main moves the same file the other way, and lands on the remote —
        // the ordinary way a branch goes stale and starts conflicting.
        std::fs::write(repo.join("a.txt"), "ours").unwrap();
        git(&repo, &["commit", "-qam", "main moves too"]);
        git(&repo, &["push", "-q", "origin", "main"]);

        wt(&repo, "wt-detached", "");

        // Deleted behind git's back, leaving stale administrative data.
        wt(&repo, "wt-broken", "feat/broken");
        std::fs::remove_dir_all(root.join("wt-broken")).unwrap();

        Self { _dir: dir, root }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }
}

fn add_untracked_diff_worktree(fixture: &Fixture) {
    let repo = fixture.repo();
    wt(&repo, "wt-untracked-diff", "feat/untracked-diff");
    let untracked = fixture.root.join("wt-untracked-diff");
    for index in 0..143 {
        let parent = untracked.join(format!("generated/group-{}", index % 7));
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(
            parent.join(format!("file-{index:03}.txt")),
            format!("line one for {index}\nline two\n"),
        )
        .unwrap();
    }
    std::fs::write(untracked.join("empty.txt"), "").unwrap();
    std::fs::write(untracked.join("binary.bin"), [0, 1, 2, 3]).unwrap();
    let mut large_binary = vec![0; 9 * 1024 * 1024];
    let last = large_binary.len() - 1;
    large_binary[last] = 1;
    std::fs::write(untracked.join("large.bin"), large_binary).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::symlink;

        symlink("generated/group-0/file-000.txt", untracked.join("link")).unwrap();
        if std::fs::write(
            untracked.join(std::ffi::OsString::from_vec(b"odd-\xff.txt".to_vec())),
            b"text-\xfe\n",
        )
        .is_err()
        {
            std::fs::write(untracked.join("odd.txt"), b"text-\xfe\n").unwrap();
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::write(untracked.join("link"), "generated/group-0/file-000.txt").unwrap();
        std::fs::write(untracked.join("odd.txt"), b"text-\xfe\n").unwrap();
    }
}

/// A worktree holding the four shapes Git treats differently when it lists
/// untracked work: an ordinary directory, a nested ordinary repository, a
/// linked worktree, and a bare repository.
fn add_nested_repository_worktree(fixture: &Fixture) {
    let repo = fixture.repo();
    wt(&repo, "wt-nested-repos", "feat/nested-repos");
    let root = fixture.root.join("wt-nested-repos");

    // An ordinary directory: Git lists each file, and so do we.
    std::fs::create_dir_all(root.join("plain/deep")).unwrap();
    std::fs::write(root.join("plain/one.txt"), "one\n").unwrap();
    std::fs::write(root.join("plain/deep/two.txt"), "two\n").unwrap();

    // A nested ordinary repository: Git reports the directory itself.
    let nested = root.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    std::fs::write(nested.join("inside.txt"), "inside\n").unwrap();

    // A linked worktree: a `.git` file pointing into the parent repository.
    let linked = root.join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "feat/nested-linked",
        ],
    );
    std::fs::write(linked.join("inside.txt"), "inside\n").unwrap();

    // A bare repository: Git walks into it and lists every internal path.
    let bare = root.join("remote.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
}

fn git(cwd: &Path, args: &[&str]) {
    let out = git_output(cwd, args);

    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_output(cwd: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for arg in args {
        command.arg(git_argument(OsStr::new(arg)));
    }
    command
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "yawm test")
        .env("GIT_AUTHOR_EMAIL", "test@yawm.dev")
        .env("GIT_COMMITTER_NAME", "yawm test")
        .env("GIT_COMMITTER_EMAIL", "test@yawm.dev")
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"))
}

#[cfg(windows)]
fn git_argument(arg: &OsStr) -> std::borrow::Cow<'_, OsStr> {
    use std::borrow::Cow;
    use std::ffi::OsString;

    let text = arg.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return Cow::Owned(OsString::from(format!(r"\\{rest}")));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return Cow::Owned(OsString::from(rest));
    }
    Cow::Borrowed(arg)
}

#[cfg(not(windows))]
fn git_argument(arg: &OsStr) -> std::borrow::Cow<'_, OsStr> {
    std::borrow::Cow::Borrowed(arg)
}

fn assert_same_path(actual: &Path, expected: impl AsRef<Path>) {
    let expected = expected.as_ref();
    assert_eq!(
        path_key(actual),
        path_key(expected),
        "paths differ: actual={}, expected={}",
        actual.display(),
        expected.display()
    );
}

/// Add a worktree as a sibling of the repository. An empty branch name means
/// detached HEAD.
fn wt(repo: &Path, name: &str, branch: &str) {
    let target = format!("../{name}");
    if branch.is_empty() {
        git(repo, &["worktree", "add", "-q", "--detach", &target]);
    } else {
        git(repo, &["worktree", "add", "-q", &target, "-b", branch]);
    }
}

fn commit_empty(cwd: &Path, message: &str) {
    git(cwd, &["commit", "-q", "--allow-empty", "-m", message]);
}

type Classified = (String, Verdict, VerdictReason, WorktreeEntry);

/// Run the full pipeline and classify every worktree.
fn classify_all(fixture: &Fixture) -> Vec<Classified> {
    let git_bin = Git::new();
    let repo = fixture.repo();

    let entries = list_worktrees(&git_bin, &repo).expect("list worktrees");
    let ctx = load_context(&git_bin, &repo, &entries).expect("load context");

    // Everything here was created seconds ago, so evaluate from a point well in
    // the future; otherwise the "recently active" rule would keep everything.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 10 * DAY;

    entries
        .iter()
        .map(|entry| {
            let mut status = status_for(&git_bin, entry, &ctx);
            // This helper models the completed scan pipeline. `status_for`
            // itself deliberately gathers Git state only.
            status.process_check_complete = true;
            let (verdict, reason) = classify(entry, &status, &VerdictConfig::default(), now);
            let name = entry
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (name, verdict, reason, entry.clone())
        })
        .collect()
}

fn find<'a>(all: &'a [Classified], name: &str) -> &'a Classified {
    all.iter()
        .find(|(n, ..)| n == name)
        .unwrap_or_else(|| panic!("no worktree named {name}; found {:?}", names(all)))
}

fn names(all: &[Classified]) -> Vec<&str> {
    all.iter().map(|(n, ..)| n.as_str()).collect()
}

#[test]
fn finds_every_worktree() {
    let f = Fixture::build();
    let all = classify_all(&f);

    for expected in [
        "repo",
        "wt-merged",
        "wt-local-merged",
        "wt-gone",
        "wt-dirty",
        "wt-locked",
        "wt-review",
        "wt-detached",
        "wt-broken",
    ] {
        assert!(
            names(&all).contains(&expected),
            "missing {expected}; found {:?}",
            names(&all)
        );
    }
}

#[cfg(unix)]
#[test]
fn hardened_bare_repository_and_linked_worktree_use_explicit_git_context() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonicalize");
    let bare = root.join("admin.git");
    let seed = root.join("seed");
    let linked = root.join("linked");
    git(&root, &["init", "-q", "--bare", "admin.git"]);
    git(&root, &["init", "-q", "-b", "main", "seed"]);
    git(&seed, &["config", "user.email", "test@yawm.dev"]);
    git(&seed, &["config", "user.name", "yawm test"]);
    std::fs::write(seed.join("tracked.txt"), "base\n").unwrap();
    git(&seed, &["add", "tracked.txt"]);
    git(&seed, &["commit", "-qm", "base"]);
    git(&seed, &["remote", "add", "origin", "../admin.git"]);
    git(&seed, &["push", "-q", "origin", "main"]);
    git(
        &root,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
    );
    git(
        &root,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "feature",
            "main",
        ],
    );

    let wrapper = root.join("git-safe-bare-explicit");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\nexec git -c safe.bareRepository=explicit \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let hardened = Git::with_program(wrapper.to_string_lossy());

    let from_bare = list_worktrees(&hardened, &bare).expect("explicitly selected bare admin");
    let from_linked =
        list_worktrees(&hardened, &linked).expect("linked checkout whose common dir is bare");
    assert_eq!(from_bare.len(), 2);
    assert_eq!(from_bare, from_linked);
    assert!(from_bare[0].bare && from_bare[0].is_main);

    let ctx = load_context(&hardened, &bare, &from_bare).expect("bare repository context");
    let feature = from_bare
        .iter()
        .find(|entry| path_key(&entry.path) == path_key(&linked))
        .unwrap();
    assert_eq!(
        status_for(&hardened, feature, &ctx).landing,
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::Ancestry,
        }
    );

    let mut config = Config::default();
    config.add_repo_to(None, bare.clone());
    let report = Scanner::new(config)
        .with_git(hardened)
        .scan_repo(&bare, ScanOptions::fast())
        .expect("full topology report from hardened bare repository");
    assert_eq!(report.root, bare);
    assert_eq!(report.worktrees.len(), 2);
    assert!(report.worktrees.iter().any(|worktree| worktree.entry.bare));
    assert!(
        report
            .worktrees
            .iter()
            .any(|worktree| path_key(&worktree.entry.path) == path_key(&linked))
    );
}

#[test]
fn a_physically_moved_selected_worktree_reports_the_exact_repair() {
    let f = Fixture::build();
    let repo = f.repo();
    let old = f.root.join("wt-review");
    let observed = f.root.join("wt-review-moved");
    std::fs::rename(&old, &observed).unwrap();

    let error = list_worktrees(&Git::new(), &observed)
        .expect_err("the stale administrative location must be diagnosed");
    let yawm_core::Error::MovedWorktree { diagnostic } = error else {
        panic!("expected a moved-worktree diagnostic");
    };
    assert_same_path(&diagnostic.main_worktree, &repo);
    assert_same_path(&diagnostic.common_admin_dir, repo.join(".git"));
    assert_same_path(&diagnostic.observed_path, &observed);
    assert_eq!(diagnostic.repair_command.len(), 6);
    assert_eq!(diagnostic.repair_command[0], "git");
    assert_eq!(diagnostic.repair_command[1], "-C");
    assert_same_path(Path::new(&diagnostic.repair_command[2]), &repo);
    assert_eq!(diagnostic.repair_command[3], "worktree");
    assert_eq!(diagnostic.repair_command[4], "repair");
    assert_same_path(Path::new(&diagnostic.repair_command[5]), &observed);

    let mut config = Config::default();
    config.add_repo_to(None, observed.clone());
    let discovery = Scanner::new(config).repositories_reporting();
    assert!(discovery.repositories.is_empty());
    assert_eq!(discovery.unreadable.len(), 1);
    assert_eq!(
        discovery.unreadable[0].moved_worktree.as_ref(),
        Some(&diagnostic)
    );

    git(&repo, &["worktree", "repair", observed.to_str().unwrap()]);
    let repaired = list_worktrees(&Git::new(), &observed).expect("repair makes it inspectable");
    let moved = repaired
        .iter()
        .find(|entry| path_key(&entry.path) == path_key(&observed))
        .unwrap();
    assert!(moved.prunable.is_none());
}

#[test]
fn main_worktree_is_identified_and_protected() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, entry) = find(&all, "repo");

    assert!(entry.is_main);
    assert_eq!(*verdict, Verdict::Keep);
    assert_eq!(*reason, VerdictReason::MainWorktree);
}

#[test]
fn main_worktree_environment_files_are_local_not_at_risk_duplicates() {
    let f = Fixture::build();
    let repo = f.repo();
    std::fs::write(repo.join(".env.main-local"), "LOCAL=1\n").unwrap();
    let git_bin = Git::new();
    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let main = entries.iter().find(|entry| entry.is_main).unwrap();
    let status = status_for(&git_bin, main, &ctx);

    assert!(status.env_files.is_empty());
    assert_eq!(
        status.main_worktree_env_files,
        vec![".env.main-local".to_string()]
    );
    assert_eq!(
        classify(main, &status, &VerdictConfig::default(), i64::MAX / 2),
        (Verdict::Keep, VerdictReason::MainWorktree)
    );
}

#[test]
fn merged_branch_is_disposable() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, _) = find(&all, "wt-merged");

    assert_eq!(*verdict, Verdict::Disposable);
    assert_eq!(
        *reason,
        VerdictReason::WorkContained {
            target: "origin/main".into(),
            proof: LandingProof::Ancestry,
        }
    );
}

/// Work merged into local `main` but not yet pushed still lives outside the
/// worktree, so the worktree is disposable. Testing ancestry only against the
/// remote would overlook the local containment proof.
#[test]
fn locally_merged_but_unpushed_branch_is_disposable() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, _) = find(&all, "wt-local-merged");

    assert_eq!(*verdict, Verdict::Disposable);
    assert_eq!(
        *reason,
        VerdictReason::WorkContained {
            target: "origin/main".into(),
            proof: LandingProof::Ancestry,
        }
    );
}

#[test]
fn a_genuinely_squash_merged_branch_is_disposable() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, _) = find(&all, "wt-gone");

    assert_eq!(*verdict, Verdict::Disposable);
    assert!(
        matches!(reason, VerdictReason::WorkContained { .. }),
        "the squash merge, not the deleted upstream, proves containment"
    );
}

#[test]
fn dirty_worktree_with_unpushed_work_is_kept() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, _) = find(&all, "wt-dirty");

    assert_eq!(*verdict, Verdict::Keep);
    assert!(matches!(
        reason,
        VerdictReason::UncommittedChangesAtRisk { .. }
    ));
}

#[test]
fn dirty_counts_and_env_files_are_reported() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let entries = list_worktrees(&git_bin, &f.repo()).unwrap();
    let ctx = load_context(&git_bin, &f.repo(), &entries).unwrap();

    let entry = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");
    let status = status_for(&git_bin, entry, &ctx);

    assert_eq!(status.dirty.staged, 1, "staged.txt");
    assert_eq!(status.dirty.unstaged, 1, "a.txt");
    assert!(status.dirty.untracked >= 1, "untracked.txt");
    assert_eq!(status.upstream.ahead, 1, "one unpushed commit");

    // The warning that prevents silent data loss on delete.
    assert!(status.env_files.contains(&".env".to_string()));
    assert!(status.env_files.contains(&".env.local".to_string()));
}

#[test]
fn locked_worktree_is_kept_with_its_reason() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, entry) = find(&all, "wt-locked");

    assert_eq!(*verdict, Verdict::Keep);
    assert_eq!(*reason, VerdictReason::Locked);
    assert_eq!(
        entry.locked.as_ref().and_then(|l| l.reason.as_deref()),
        Some("agent running")
    );
}

#[test]
fn clean_branch_with_absent_content_is_kept() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, _) = find(&all, "wt-review");

    assert_eq!(*verdict, Verdict::Keep);
    assert!(matches!(
        reason,
        VerdictReason::DefaultBranchLacksCommittedContent { .. }
    ));
}

#[test]
fn deleted_directory_is_reported_as_broken() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, verdict, reason, entry) = find(&all, "wt-broken");

    assert_eq!(*verdict, Verdict::Broken);
    assert!(matches!(reason, VerdictReason::DirectoryMissing { .. }));
    assert!(entry.prunable.is_some());
}

#[test]
fn detached_worktree_is_recognised() {
    let f = Fixture::build();
    let all = classify_all(&f);
    let (_, _, _, entry) = find(&all, "wt-detached");

    assert!(entry.detached);
    assert!(entry.branch.is_none());
}

#[test]
fn default_branch_resolves_to_the_remote_head() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let entries = list_worktrees(&git_bin, &f.repo()).unwrap();
    let ctx = load_context(&git_bin, &f.repo(), &entries).unwrap();

    let default_ref = ctx.default_ref.expect("a default ref");
    assert!(
        default_ref.ends_with("main"),
        "unexpected default ref: {default_ref}"
    );
}

/// Nothing holding real work may ever be classified as disposable. This is the
/// invariant that protects the user's data.
#[test]
fn nothing_with_uncommitted_or_unpushed_work_is_disposable() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let entries = list_worktrees(&git_bin, &f.repo()).unwrap();
    let ctx = load_context(&git_bin, &f.repo(), &entries).unwrap();
    let now = i64::MAX / 2; // force every time-based rule to read as stale

    for entry in &entries {
        let status = status_for(&git_bin, entry, &ctx);
        let (verdict, _) = classify(entry, &status, &VerdictConfig::default(), now);

        if verdict == Verdict::Disposable {
            assert!(
                !status.dirty.is_dirty(),
                "{} was disposable despite {} changed files",
                entry.path.display(),
                status.dirty.total()
            );
            assert_eq!(
                status.upstream.ahead,
                0,
                "{} was disposable despite unpushed commits",
                entry.path.display()
            );
            assert!(!entry.is_main, "the main worktree must never be disposable");
        }
    }
}

// ---------------------------------------------------------------------------
// Removal
// ---------------------------------------------------------------------------

use yawm_core::ops::{
    BranchOutcome, PLAN_CHANGED_MARKER, RemovalPlan, RemovalRequest, RemovalStatus, RemoveOptions,
    plan_removal, plan_removals, prune, remove, remove_all, remove_all_after_each,
    remove_all_interrupted, remove_reporting,
};

/// The delete dialog must be told exactly what it was told before.
///
/// The plan it shows used to be built on top of a full inspection: a recursive
/// walk of the directory for a size, and the historical containment proof, both
/// of which a plan has never read. Dropping them is only safe if the plan that
/// comes out is the same plan, so this compares the two paths field by field
/// over every worktree state the fixture can produce — clean, dirty, locked,
/// detached, prunable, and the main worktree.
///
/// Destructured rather than compared as a whole, so a field added to
/// [`RemovalPlan`] fails to compile here until it is checked too.
#[test]
fn planning_the_cheap_way_answers_exactly_what_the_full_inspection_did() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let paths: Vec<PathBuf> = list_worktrees(&git_bin, &repo)
        .unwrap()
        .into_iter()
        .map(|entry| entry.path)
        .collect();

    let fast = plan_removals(&git_bin, &repo, &paths).expect("plans for the whole selection");
    assert_eq!(fast.len(), paths.len());

    for (path, fast) in paths.iter().zip(fast) {
        let inspected = Scanner::new(Config::default())
            .inspect_worktree(&repo, path)
            .expect("full inspection");
        let slow = plan_removal(&git_bin, &inspected.entry, &inspected.status);

        let RemovalPlan {
            path: fast_path,
            branch,
            is_main,
            is_locked,
            lock_reason,
            is_prunable,
            dirty_files,
            dirty_total,
            unpushed_commits,
            env_files,
            managed_dependency_links,
            running_processes,
            requires_force,
            state,
        } = fast;
        let at = path.display();

        assert_eq!(fast_path, slow.path, "{at}: path");
        assert_eq!(branch, slow.branch, "{at}: branch");
        assert_eq!(is_main, slow.is_main, "{at}: is_main");
        assert_eq!(is_locked, slow.is_locked, "{at}: is_locked");
        assert_eq!(lock_reason, slow.lock_reason, "{at}: lock_reason");
        assert_eq!(is_prunable, slow.is_prunable, "{at}: is_prunable");
        assert_eq!(
            dirty_files, slow.dirty_files,
            "{at}: the files a forced removal would destroy"
        );
        assert_eq!(dirty_total, slow.dirty_total, "{at}: dirty_total");
        assert_eq!(
            unpushed_commits, slow.unpushed_commits,
            "{at}: commits that exist nowhere else"
        );
        assert_eq!(
            env_files, slow.env_files,
            "{at}: files that are not in git at all"
        );
        assert_eq!(
            managed_dependency_links, slow.managed_dependency_links,
            "{at}: managed dependency links"
        );
        assert_eq!(
            running_processes, slow.running_processes,
            "{at}: running_processes"
        );
        assert_eq!(
            requires_force, slow.requires_force,
            "{at}: whether git will refuse without --force"
        );
        // The authorisation itself. If the two paths disagree here, one of
        // them cannot revalidate a plan the other produced, and every removal
        // through that pairing fails as changed.
        assert_eq!(
            state, slow.state,
            "{at}: the exact state the removal is authorised against"
        );
        assert!(
            state.digest.len() == 64,
            "{at}: every plan carries a digest of its state, got {:?}",
            state.digest
        );
    }
}

/// What the plan path is allowed to do, pinned to the invocations it makes.
///
/// Two costs used to sit behind the dialog and be thrown away: the historical
/// landing proof, and a second full status pass run only to learn the names
/// behind counts the first pass had already produced. Neither is visible in the
/// plan, so nothing but the git log would ever notice them coming back.
///
/// The disk walk is not checked here because it cannot return: measuring a size
/// needs a [`Scanner`], and this path never builds one.
#[cfg(unix)]
#[test]
fn planning_runs_no_landing_proof_and_revalidates_status_once() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::build();
    let log = f.root.join("git-invocations.log");
    let shim = f.root.join("git-shim");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexec git \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

    let repo = f.repo();
    let dirty = f.root.join("wt-dirty");
    let watched = Git::with_program(shim.to_string_lossy().into_owned());

    plan_removals(&watched, &repo, &[dirty]).expect("plan");

    let invocations: Vec<String> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(!invocations.is_empty(), "the shim has to have been used");

    for landing in [
        "merge-base",
        "merge-tree",
        "rev-list",
        "remotes/origin/HEAD",
    ] {
        assert!(
            !invocations.iter().any(|call| call.contains(landing)),
            "planning must not prove landing; ran {landing} in {invocations:?}"
        );
    }

    // The one `symbolic-ref` planning is allowed is the repository's own HEAD:
    // it names the ref an unforced branch deletion would be decided against,
    // and it is read once for the whole repository rather than per worktree.
    let head_reads = invocations
        .iter()
        .filter(|call| call.contains("symbolic-ref"))
        .count();
    assert!(
        head_reads <= 1,
        "HEAD is resolved once for the repository, not per worktree: {invocations:?}"
    );

    let status_passes = invocations
        .iter()
        .filter(|call| call.starts_with("status ") || call.contains(" status --porcelain=v1 "))
        .count();
    assert_eq!(
        status_passes, 2,
        "the fingerprint takes one status snapshot and one final inventory recheck: {invocations:?}"
    );
}

/// The guard that stands between the dialog and an irreversible delete, over
/// the path the dialog actually uses now.
///
/// Speeding up planning must not speed up the moment of destruction past the
/// re-plan: a file written while the user reads the dialog is still a file they
/// never approved losing.
#[test]
fn a_plan_from_the_batch_path_is_still_re_checked_before_anything_is_deleted() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let dirty = f.root.join("wt-dirty");

    let approved = plan_removals(&git_bin, &repo, std::slice::from_ref(&dirty))
        .expect("plan")
        .remove(0);
    assert!(approved.requires_force, "the fixture worktree is dirty");

    let late = dirty.join("late.txt");
    std::fs::write(&late, "work the user never saw\n").unwrap();

    let err = remove(
        &git_bin,
        &repo,
        &approved,
        RemoveOptions {
            force: true,
            ..Default::default()
        },
    )
    .expect_err("must refuse a plan that no longer describes the worktree");

    assert!(matches!(err, yawm_core::error::Error::PlanChanged { .. }));
    assert!(
        err.to_string().contains("late.txt"),
        "names what changed: {err}"
    );
    assert!(late.exists(), "nothing the user never saw was destroyed");
    assert!(dirty.exists());
}

/// Is this path still a worktree of the repository, and is it still locked?
fn registered<'a>(entries: &'a [WorktreeEntry], path: &Path) -> Option<&'a WorktreeEntry> {
    entries
        .iter()
        .find(|entry| path_key(&entry.path) == path_key(path))
}

fn requests(pairs: Vec<(RemovalPlan, RemoveOptions)>) -> Vec<RemovalRequest> {
    pairs
        .into_iter()
        .map(|(plan, options)| RemovalRequest { plan, options })
        .collect()
}

/// The whole selection is validated before any of it is deleted.
///
/// Deleting one at a time meant a selection where the *second* worktree had
/// changed still lost the first: it was already gone by the time the second was
/// looked at, and what the user then saw was a refusal — so they were told
/// nothing had been deleted while a worktree had in fact been deleted. Worse,
/// re-planning the unchanged selection afterwards asked git about a path that
/// no longer existed, and that parse failure became the visible error, hiding
/// the real one.
///
/// The valid worktree is deliberately first in the batch, because that is the
/// position the old behaviour destroyed.
#[test]
fn a_batch_that_fails_validation_leaves_every_selected_worktree_in_place() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let clean = f.root.join("wt-review");
    let dirty = f.root.join("wt-dirty");

    let plans = plan_removals(&git_bin, &repo, &[clean.clone(), dirty.clone()]).expect("plan");
    assert!(
        !plans[0].requires_force,
        "the first worktree is clean, so on its own it would be removed without a murmur"
    );

    // The window between the dialog rendering and the click, in the worktree
    // the user is not about to be told about.
    let late = dirty.join("late.txt");
    std::fs::write(&late, "work the user never saw\n").unwrap();

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (
                plans[1].clone(),
                RemoveOptions {
                    force: true,
                    ..Default::default()
                },
            ),
        ]),
    )
    .expect_err("a batch containing a changed plan must be refused whole");

    assert!(
        matches!(err, yawm_core::error::Error::PlanChanged { .. }),
        "the caller has to be able to tell 'look again' from 'it broke'; got {err:?}"
    );
    let message = err.to_string();
    assert!(message.contains(PLAN_CHANGED_MARKER), "got {message}");
    assert!(
        message.contains("late.txt"),
        "names what changed: {message}"
    );

    assert!(
        clean.exists(),
        "the plan that was still valid must not have been acted on"
    );
    assert!(dirty.exists());
    assert!(late.exists());

    let after = list_worktrees(&git_bin, &repo).expect("list worktrees");
    for path in [&clean, &dirty] {
        assert!(
            registered(&after, path).is_some(),
            "{} was unregistered, so a re-plan of the same selection would fail \
             on a path that no longer exists",
            path.display()
        );
    }

    // And once the user is shown the selection as it is now, the batch goes
    // through — the refusal was about the plans, not about the batch API.
    let replanned = plan_removals(&git_bin, &repo, &[clean.clone(), dirty.clone()]).expect("plan");
    remove_all(
        &git_bin,
        &repo,
        &requests(vec![
            (replanned[0].clone(), RemoveOptions::default()),
            (
                replanned[1].clone(),
                RemoveOptions {
                    force: true,
                    ..Default::default()
                },
            ),
        ]),
    )
    .expect("a batch whose every plan still matches is carried out");
    assert!(!clean.exists());
    assert!(!dirty.exists());
}

/// A lock is somebody saying "not this one", usually with a reason. Agreeing
/// to lose some edited files is not an answer to it.
#[test]
fn a_locked_worktree_is_not_removed_by_confirming_uncommitted_changes() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let locked = f.root.join("wt-locked");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&locked))
        .expect("plan")
        .remove(0);
    assert!(plan.is_locked);
    assert_eq!(
        plan.lock_reason.as_deref(),
        Some("agent running"),
        "the reason has to survive planning, because it is what the user reads"
    );

    let err = remove(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            // Everything the old code needed to force its way past the lock.
            force: true,
            force_branch: true,
            ..Default::default()
        },
    )
    .expect_err("a lock must not be lifted by a confirmation about files");

    let message = err.to_string();
    assert!(message.contains("locked"), "got {message}");
    assert!(
        message.contains("agent running"),
        "says whose instruction is being refused: {message}"
    );

    assert!(locked.exists(), "nothing was deleted");
    let entries = list_worktrees(&git_bin, &repo).expect("list worktrees");
    let still = registered(&entries, &locked).expect("still a worktree");
    assert!(still.locked.is_some(), "and the lock is still on");
}

/// Authorised explicitly, it goes — and the lock goes with it, by name.
#[test]
fn an_authorised_unlock_removes_the_worktree() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let locked = f.root.join("wt-locked");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&locked))
        .expect("plan")
        .remove(0);
    assert!(
        !plan.requires_force,
        "this worktree is clean; the only thing standing in the way is the lock"
    );

    remove(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            unlock: true,
            ..Default::default()
        },
    )
    .expect("an explicit unlock authorises the removal");

    assert!(!locked.exists(), "the directory is gone");
    let entries = list_worktrees(&git_bin, &repo).expect("list worktrees");
    assert!(
        registered(&entries, &locked).is_none(),
        "and git left no stale administrative data behind"
    );
}

/// The lock is part of the photograph. Locking a worktree while the dialog is
/// open is somebody objecting in the only way git offers, and the deletion has
/// to notice.
#[test]
fn a_lock_taken_after_planning_invalidates_the_plan() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let clean = f.root.join("wt-review");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&clean))
        .expect("plan")
        .remove(0);
    assert!(!plan.is_locked);

    git(
        &repo,
        &[
            "worktree",
            "lock",
            "../wt-review",
            "--reason",
            "agent running",
        ],
    );

    let err = remove(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            force: true,
            // Even pre-authorised, an unlock authorises lifting the lock the
            // user was shown — and they were shown none.
            unlock: true,
            ..Default::default()
        },
    )
    .expect_err("a worktree locked after planning is not the worktree that was approved");

    assert!(matches!(err, yawm_core::error::Error::PlanChanged { .. }));
    let message = err.to_string();
    assert!(
        message.contains("agent running"),
        "the new instruction is quoted, not merely announced: {message}"
    );
    assert!(clean.exists(), "nothing was deleted");
}

/// A lock that stays on but now says something else is a different
/// instruction, and the one the user read is gone.
#[test]
fn a_lock_reason_changed_after_planning_invalidates_the_plan() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let locked = f.root.join("wt-locked");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&locked))
        .expect("plan")
        .remove(0);
    assert_eq!(plan.lock_reason.as_deref(), Some("agent running"));

    git(&repo, &["worktree", "unlock", "../wt-locked"]);
    git(
        &repo,
        &[
            "worktree",
            "lock",
            "../wt-locked",
            "--reason",
            "release in progress, do not touch",
        ],
    );

    let err = remove(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            unlock: true,
            ..Default::default()
        },
    )
    .expect_err("the lock that would be lifted is not the lock that was shown");

    assert!(matches!(err, yawm_core::error::Error::PlanChanged { .. }));
    assert!(err.to_string().contains("release in progress"), "got {err}");
    assert!(locked.exists(), "nothing was deleted");
}

/// One locked worktree in a selection stops the whole selection, in the same
/// place and for the same reason a changed plan does: before anything is
/// deleted.
#[test]
fn one_locked_worktree_holds_up_the_whole_batch() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let clean = f.root.join("wt-review");
    let locked = f.root.join("wt-locked");

    let plans = plan_removals(&git_bin, &repo, &[clean.clone(), locked.clone()]).expect("plan");

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            // Unlocking was never authorised for the locked one.
            (plans[1].clone(), RemoveOptions::default()),
        ]),
    )
    .expect_err("an unauthorised lock refuses the batch");

    assert!(err.to_string().contains("agent running"), "got {err}");
    assert!(clean.exists(), "the rest of the selection was not deleted");
    assert!(locked.exists());
}

/// Lock the worktree that is about to be deleted, in the window between the
/// batch being validated and the first directory going.
///
/// The batch check proves every plan at one moment and then starts deleting.
/// For a selection of five, the fifth is acted on however long the first four
/// took — long enough for an agent to lock it — and the removal would have
/// lifted a lock nobody was ever shown. Nothing has been deleted yet here, so
/// the refusal is the ordinary one and the whole selection survives.
#[test]
fn a_lock_taken_after_validation_stops_the_batch_before_anything_goes() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let first = f.root.join("wt-review");
    let second = f.root.join("wt-merged");

    let plans = plan_removals(&git_bin, &repo, &[first.clone(), second.clone()]).expect("plan");
    assert!(!plans[0].is_locked && !plans[1].is_locked);

    let err = remove_all_interrupted(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
        &mut || {
            git(
                &repo,
                &[
                    "worktree",
                    "lock",
                    "../wt-review",
                    "--reason",
                    "agent running",
                ],
            );
        },
    )
    .expect_err("a lock taken in that window must not be lifted by this removal");

    let yawm_core::error::Error::PlanChanged {
        changes,
        still_present,
        ..
    } = &err
    else {
        panic!("nothing was deleted, so this is 'look again': {err:?}");
    };
    assert!(
        changes.iter().any(|c| c.contains("agent running")),
        "the instruction that appeared is quoted: {changes:?}"
    );
    assert!(
        still_present
            .iter()
            .any(|p| path_key(p) == path_key(&first))
            && still_present
                .iter()
                .any(|p| path_key(p) == path_key(&second)),
        "both are still worktrees, so both may be re-planned: {still_present:?}"
    );

    assert!(first.exists(), "nothing was deleted");
    assert!(second.exists());
}

/// The same race, one worktree later: the first is already gone.
///
/// This is the case that cannot be rolled back, and the one a generic failure
/// misreports. The caller has to learn which worktrees went, so it can close
/// their tabs and stop claiming the selection survived.
#[test]
fn a_lock_taken_mid_batch_reports_what_was_already_removed() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let first = f.root.join("wt-review");
    let second = f.root.join("wt-merged");

    let plans = plan_removals(&git_bin, &repo, &[first.clone(), second.clone()]).expect("plan");

    let err = remove_all_interrupted(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
        &mut || {
            git(
                &repo,
                &[
                    "worktree",
                    "lock",
                    "../wt-merged",
                    "--reason",
                    "agent running",
                ],
            );
        },
    )
    .expect_err("the second worktree is not the one that was approved");

    let yawm_core::error::Error::BatchIncomplete(partial) = &err else {
        panic!("a batch that deleted something must never report a bare refusal: {err:?}");
    };
    assert_eq!(partial.completed.len(), 1);
    assert_eq!(path_key(&partial.completed[0].path), path_key(&first));
    assert_eq!(path_key(&partial.failed), path_key(&second));
    assert!(
        matches!(*partial.cause, yawm_core::error::Error::PlanChanged { .. }),
        "got {:?}",
        partial.cause
    );

    let message = err.to_string();
    assert!(
        message.contains("wt-review"),
        "the removal that happened has to be named: {message}"
    );
    assert!(message.contains("wt-merged"), "got {message}");

    assert!(!first.exists(), "the first removal did happen");
    assert!(second.exists(), "the second was refused");
    let entries = list_worktrees(&git_bin, &repo).expect("list worktrees");
    assert!(
        registered(&entries, &second)
            .expect("still a worktree")
            .locked
            .is_some(),
        "and the lock somebody took is still on it"
    );
}

/// A worktree deleted from outside yawm while the dialog is open.
///
/// The refusal is followed by a re-plan, and core refuses to plan a path it
/// cannot find — so the refusal has to say what the repository has now. Reading
/// it off a list the caller painted earlier turned "these changed, look again"
/// into a parse error about a missing path.
#[test]
fn a_refusal_says_which_worktrees_are_still_there() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let kept = f.root.join("wt-review");
    let vanished = f.root.join("wt-merged");

    let plans = plan_removals(&git_bin, &repo, &[kept.clone(), vanished.clone()]).expect("plan");

    // Somebody else removes one of them properly, behind yawm's back.
    git(&repo, &["worktree", "remove", "../wt-merged"]);

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
    )
    .expect_err("a path that is no longer a worktree refuses the batch");

    let yawm_core::error::Error::PlanChanged { still_present, .. } = &err else {
        panic!("nothing was deleted: {err:?}");
    };
    assert!(
        still_present.iter().any(|p| path_key(p) == path_key(&kept)),
        "got {still_present:?}"
    );
    assert!(
        !still_present
            .iter()
            .any(|p| path_key(p) == path_key(&vanished)),
        "a re-plan must not be pointed at a path that has gone: {still_present:?}"
    );
    assert!(kept.exists(), "nothing was deleted");
}

/// A git wrapper that refuses the named `git worktree <sub>` calls and forwards
/// everything else. The failures a removal has to survive after it has already
/// lifted a lock cannot be produced by arranging the repository, because the
/// point of the arrangement is that git would refuse — and git refusing is what
/// the earlier checks stop before any lock is touched.
#[cfg(unix)]
fn failing_git(root: &Path, name: &str, refuse: &[&str]) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    let mut script = String::from("#!/bin/sh\n");
    for sub in refuse {
        script.push_str(&format!(
            "if [ \"$1\" = \"worktree\" ] && [ \"$2\" = \"{sub}\" ]; then\n  \
             echo \"shim refused worktree {sub}\" >&2\n  exit 1\nfi\n"
        ));
    }
    script.push_str("exec git \"$@\"\n");
    std::fs::write(&shim, script).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

/// The lock is lifted by name immediately before the removal. When the removal
/// then fails, the worktree is still there — and it is now unlocked, with the
/// instruction somebody left on it gone. Putting it back is the only honest
/// outcome: nothing was deleted, so nothing about it should have changed.
#[cfg(unix)]
#[test]
fn a_failed_removal_puts_back_the_lock_it_lifted() {
    let f = Fixture::build();
    let repo = f.repo();
    let locked = f.root.join("wt-locked");

    let plan = plan_removals(&Git::new(), &repo, std::slice::from_ref(&locked))
        .expect("plan")
        .remove(0);
    assert_eq!(plan.lock_reason.as_deref(), Some("agent running"));

    let err = remove(
        &failing_git(&f.root, "git-no-remove", &["remove"]),
        &repo,
        &plan,
        RemoveOptions {
            unlock: true,
            ..Default::default()
        },
    )
    .expect_err("git refused the removal");

    assert!(
        err.to_string().contains("shim refused worktree remove"),
        "the real failure is still the one reported: {err}"
    );
    assert!(locked.exists(), "nothing was deleted");

    let entries = list_worktrees(&Git::new(), &repo).expect("list worktrees");
    let still = registered(&entries, &locked).expect("still a worktree");
    let lock = still
        .locked
        .as_ref()
        .expect("the lock has to go back on: the removal it was lifted for never happened");
    assert_eq!(
        lock.reason.as_deref(),
        Some("agent running"),
        "and it has to say what it said before"
    );
}

/// Both failures, or the user trusts a lock that is not there.
///
/// If the lock cannot be put back, the worktree is left unlocked by yawm and
/// nothing else will ever say so. Reporting only the removal failure hides it;
/// reporting only the re-lock failure hides why the lock was off in the first
/// place.
#[cfg(unix)]
#[test]
fn a_lock_that_cannot_be_put_back_is_reported_next_to_the_failure() {
    let f = Fixture::build();
    let repo = f.repo();
    let locked = f.root.join("wt-locked");

    let plan = plan_removals(&Git::new(), &repo, std::slice::from_ref(&locked))
        .expect("plan")
        .remove(0);

    let err = remove(
        &failing_git(&f.root, "git-no-remove-or-lock", &["remove", "lock"]),
        &repo,
        &plan,
        RemoveOptions {
            unlock: true,
            ..Default::default()
        },
    )
    .expect_err("git refused the removal");

    let message = err.to_string();
    assert!(
        message.contains("shim refused worktree remove"),
        "the removal failure must not be swallowed by the recovery: {message}"
    );
    assert!(
        message.contains("shim refused worktree lock"),
        "and neither must the recovery's own failure: {message}"
    );
    assert!(
        message.contains("agent running"),
        "the lock that is now missing said something, and that is what is lost: {message}"
    );
    assert!(locked.exists(), "nothing was deleted");

    let entries = list_worktrees(&Git::new(), &repo).expect("list worktrees");
    assert!(
        registered(&entries, &locked)
            .expect("still a worktree")
            .locked
            .is_none(),
        "the message says the lock is off, so it had better be off"
    );
}

/// A file written into a *later* worktree while an earlier one is being
/// deleted.
///
/// The batch validates everything at one moment and then deletes one worktree
/// at a time, so every removal is a pause during which the rest of the
/// selection goes on being written to — by the agent that is still running in
/// it, which is the ordinary case for this app. Re-checking only the lock
/// before each removal left every other difference unnoticed, and yawm would
/// destroy a file nobody had ever been shown.
///
/// What has already gone is reported as gone; what changed is refused and left
/// exactly where it is.
#[test]
fn a_file_appearing_mid_batch_stops_that_worktree_and_keeps_the_earlier_removal() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let first = f.root.join("wt-review");
    let second = f.root.join("wt-merged");
    let late = second.join("late.txt");

    let plans = plan_removals(&git_bin, &repo, &[first.clone(), second.clone()]).expect("plan");
    assert_eq!(plans[1].dirty_total, 0, "the second worktree starts clean");

    let err = remove_all_after_each(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
        &mut |removed| {
            if path_key(removed) == path_key(&first) {
                std::fs::write(&late, "work the user never saw\n").unwrap();
            }
        },
    )
    .expect_err("the second worktree is no longer the one that was approved");

    let yawm_core::error::Error::BatchIncomplete(partial) = &err else {
        panic!("a batch that deleted something must never report a bare refusal: {err:?}");
    };
    assert_eq!(partial.completed.len(), 1);
    assert_eq!(path_key(&partial.completed[0].path), path_key(&first));
    assert_eq!(
        partial.completed[0].status,
        RemovalStatus::Removed,
        "the first removal finished, finalisation and all"
    );
    assert_eq!(path_key(&partial.failed), path_key(&second));

    let yawm_core::error::Error::PlanChanged {
        changes,
        still_present,
        ..
    } = &*partial.cause
    else {
        panic!(
            "the second was refused for having changed: {:?}",
            partial.cause
        );
    };
    assert!(
        changes.iter().any(|c| c.contains("late.txt")),
        "the file that appeared is named: {changes:?}"
    );
    assert!(
        still_present
            .iter()
            .any(|p| path_key(p) == path_key(&second)),
        "it is still a worktree, so it can be re-planned: {still_present:?}"
    );
    assert!(
        !still_present
            .iter()
            .any(|p| path_key(p) == path_key(&first)),
        "and the one that went must not be re-planned: {still_present:?}"
    );

    assert!(!first.exists(), "the first removal did happen");
    assert!(
        second.is_dir(),
        "the second was refused, so it is still here"
    );
    assert!(
        late.exists(),
        "and nothing the user never saw was destroyed"
    );
    let entries = list_worktrees(&git_bin, &repo).expect("list worktrees");
    assert!(
        registered(&entries, &second).is_some(),
        "still a worktree, so re-planning the selection works"
    );
}

/// A git wrapper whose `worktree remove` takes the directory away and *then*
/// reports failure.
///
/// This is the shape of a removal that mutates before it fails, which the trash
/// route reaches on its own: `trash::delete` moves the directory, and the
/// `git worktree prune` that follows can fail by itself. Reproduced with a shim
/// rather than by trashing for real, because a test must not leave folders in
/// the machine's Trash — and because the failing half has to fail every time.
#[cfg(unix)]
fn git_that_removes_then_fails(root: &Path, name: &str) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    std::fs::write(
        &shim,
        "#!/bin/sh\n\
         if [ \"$1\" = \"worktree\" ] && [ \"$2\" = \"remove\" ]; then\n  \
         for arg in \"$@\"; do target=$arg; done\n  \
         rm -rf \"$target\"\n  \
         echo \"shim removed the directory and then refused\" >&2\n  exit 1\nfi\n\
         exec git \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

/// The half-done removal: the directory is gone, the step that finishes the job
/// failed, and the error is all the caller would otherwise get.
///
/// Trusting the return values omits this worktree from `completed`, and the app
/// then goes on listing a directory that no longer exists — the one outcome the
/// batch report exists to prevent. So after any failure the repository is read
/// back, and a worktree that has actually gone is reported as gone, with a
/// status that says its finalisation did not run.
#[cfg(unix)]
#[test]
fn a_removal_that_mutated_and_then_failed_is_still_reported_as_removed() {
    let f = Fixture::build();
    let repo = f.repo();
    let target = f.root.join("wt-review");

    let plan = plan_removals(&Git::new(), &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);

    let err = remove_all(
        &git_that_removes_then_fails(&f.root, "git-remove-then-fail"),
        &repo,
        &requests(vec![(plan, RemoveOptions::default())]),
    )
    .expect_err("git refused the removal");

    let yawm_core::error::Error::BatchIncomplete(partial) = &err else {
        panic!("the directory is gone, so this cannot be a bare refusal: {err:?}");
    };
    assert_eq!(partial.completed.len(), 1);
    assert_eq!(path_key(&partial.completed[0].path), path_key(&target));
    assert_eq!(
        partial.completed[0].status,
        RemovalStatus::RemovedButFinalizationFailed,
        "gone, but the step that finishes the job never ran"
    );
    assert_eq!(
        partial.completed[0].outcome.branch,
        BranchOutcome::NotRequested,
        "no branch deletion was asked for, and none is claimed"
    );
    assert!(
        err.to_string().contains("shim removed the directory"),
        "the real failure is still reported: {err}"
    );
    assert!(!target.exists(), "the directory did go");
}

/// A branch deletion that was asked for is never claimed by reconciliation.
///
/// The worktree is gone; whether its branch went with it is exactly what the
/// step that failed would have decided. Saying "deleted" here would be a guess
/// about a branch that may still hold the user's only copy of some commits.
#[cfg(unix)]
#[test]
fn a_reconciled_removal_does_not_claim_a_branch_it_never_deleted() {
    let f = Fixture::build();
    let repo = f.repo();
    let target = f.root.join("wt-merged");

    let plan = plan_removals(&Git::new(), &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    let branch = plan.branch.clone().expect("fixture worktree has a branch");

    let err = remove_all(
        &git_that_removes_then_fails(&f.root, "git-remove-then-fail-branch"),
        &repo,
        &requests(vec![(
            plan,
            RemoveOptions {
                delete_branch: true,
                ..Default::default()
            },
        )]),
    )
    .expect_err("git refused the removal");

    let yawm_core::error::Error::BatchIncomplete(partial) = &err else {
        panic!("the directory is gone: {err:?}");
    };
    assert_eq!(
        partial.completed[0].outcome.branch,
        BranchOutcome::Kept,
        "the branch is reported as kept, because nothing proved it went"
    );

    let branches = yawm_core::git::collect::load_branches(&Git::new(), &repo).unwrap();
    assert!(
        branches.contains_key(&branch),
        "and it is in fact still there; got {:?}",
        branches.keys().collect::<Vec<_>>()
    );
}

/// Two worktrees whose directories are already gone, removed in one selection.
///
/// `git worktree prune` is repository-wide: it drops the administrative entry
/// of every worktree whose directory is missing, not the one it was asked
/// about. Pruning per request therefore pruned the rest of the selection as a
/// side effect, and the next request's own re-check then failed with "no longer
/// a worktree of this repository" — a worktree the user asked to remove,
/// removed, reported as the failure that stopped the batch.
#[test]
fn a_selection_of_stale_worktrees_is_pruned_in_one_operation() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let broken = f.root.join("wt-broken");
    // A second one, made stale the way the first was: the directory goes,
    // git's administrative data stays.
    let detached = f.root.join("wt-detached");
    std::fs::remove_dir_all(&detached).expect("remove the directory behind git's back");

    let plans = plan_removals(&git_bin, &repo, &[broken.clone(), detached.clone()]).expect("plan");
    assert!(
        plans.iter().all(|plan| plan.is_prunable),
        "both are stale metadata with no directory"
    );

    let outcomes = remove_all(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
    )
    .expect("both stale entries are removed");
    assert_eq!(outcomes.len(), 2, "one outcome per request, in order");

    let entries = list_worktrees(&git_bin, &repo).expect("list worktrees");
    for path in [&broken, &detached] {
        assert!(
            registered(&entries, path).is_none(),
            "{} is still registered",
            path.display()
        );
    }
}

/// The stale entries in a selection are pruned before anything with a directory
/// is touched, so the repository-wide prune cannot reach past them.
///
/// A prune run in the middle of a batch unregisters every directoryless
/// worktree in the repository — including one the batch has not reached yet,
/// whose own re-check would then fail on a path git no longer knows.
#[test]
fn a_stale_entry_in_a_selection_does_not_unregister_the_rest_of_it() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let clean = f.root.join("wt-review");
    let broken = f.root.join("wt-broken");

    // The stale one deliberately last: that is the position from which a
    // per-item prune would have run after the first removal.
    let plans = plan_removals(&git_bin, &repo, &[clean.clone(), broken.clone()]).expect("plan");

    let outcomes = remove_all(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
    )
    .expect("the whole selection goes");
    assert_eq!(outcomes.len(), 2);

    assert!(!clean.exists());
    let entries = list_worktrees(&git_bin, &repo).expect("list worktrees");
    for path in [&clean, &broken] {
        assert!(
            registered(&entries, path).is_none(),
            "{} is still registered",
            path.display()
        );
    }
}

#[test]
fn removes_a_clean_worktree_and_leaves_git_consistent() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-review"))
        .expect("wt-review");

    let status = status_for(&git_bin, target, &ctx);
    let plan = plan_removal(&git_bin, target, &status);
    assert!(!plan.requires_force, "a clean worktree needs no force");

    remove(&git_bin, &repo, &plan, RemoveOptions::default()).expect("removal succeeds");

    assert!(!target.path.exists(), "directory is gone");
    let after = list_worktrees(&git_bin, &repo).unwrap();
    assert!(
        !after.iter().any(|e| e.path == target.path),
        "git no longer lists it, so no stale metadata was left behind"
    );
}

/// Refusing to force is what stands between the user and losing work.
#[test]
fn refuses_to_remove_a_dirty_worktree_without_force() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");

    let status = status_for(&git_bin, target, &ctx);
    let plan = plan_removal(&git_bin, target, &status);

    assert!(plan.requires_force);
    assert!(plan.destroys_work());
    assert!(!plan.dirty_files.is_empty(), "names the files at risk");
    assert!(
        plan.env_files.contains(&".env".to_string()),
        "warns about gitignored files that exist nowhere else"
    );

    assert!(
        remove(&git_bin, &repo, &plan, RemoveOptions::default()).is_err(),
        "must refuse without explicit force"
    );
    assert!(target.path.exists(), "nothing was destroyed");

    // With explicit confirmation it proceeds.
    let forced = RemoveOptions {
        force: true,
        ..Default::default()
    };
    remove(&git_bin, &repo, &plan, forced).expect("forced removal succeeds");
    assert!(!target.path.exists());
}

/// The plan is a photograph, and an agent keeps writing into the directory
/// while the user reads it. Force authorises destroying what the user was
/// shown, and nothing else.
#[test]
fn a_file_created_after_planning_is_not_destroyed() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");

    let status = status_for(&git_bin, target, &ctx);
    let approved = plan_removal(&git_bin, target, &status);
    assert!(
        !approved.dirty_files.contains(&"late.txt".to_string()),
        "the file does not exist yet, so the user cannot have approved losing it"
    );

    // The window between the dialog rendering and the click.
    let late = target.path.join("late.txt");
    std::fs::write(&late, "work the user never saw\n").unwrap();
    let late_secret = target.path.join("apps/api/.env");
    std::fs::create_dir_all(late_secret.parent().unwrap()).unwrap();
    std::fs::write(&late_secret, "TOKEN=late").unwrap();

    let err = remove(
        &git_bin,
        &repo,
        &approved,
        RemoveOptions {
            // The user did agree to lose the files they were shown.
            force: true,
            ..Default::default()
        },
    )
    .expect_err("must refuse a plan that no longer describes the worktree");

    assert!(
        matches!(err, yawm_core::error::Error::PlanChanged { .. }),
        "the caller has to be able to tell 'look again' from 'it broke'; got {err:?}"
    );
    let message = err.to_string();
    assert!(
        message.contains(PLAN_CHANGED_MARKER),
        "the desktop app only receives the message; got {message}"
    );
    assert!(
        message.contains("late.txt"),
        "names what changed: {message}"
    );
    assert!(
        message.contains("apps/api/.env"),
        "names the secret that appeared too: {message}"
    );

    assert!(target.path.exists(), "nothing was deleted");
    assert!(late.exists(), "the file the user never approved survives");
    assert!(late_secret.exists());

    // Once the user is shown the worktree as it is now, deletion proceeds.
    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");
    let status = status_for(&git_bin, target, &ctx);
    let reapproved = plan_removal(&git_bin, target, &status);
    assert!(reapproved.dirty_files.contains(&"late.txt".to_string()));
    assert!(reapproved.env_files.contains(&"apps/api/.env".to_string()));

    remove(
        &git_bin,
        &repo,
        &reapproved,
        RemoveOptions {
            force: true,
            ..Default::default()
        },
    )
    .expect("a plan that matches the worktree still works");
    assert!(!target.path.exists());
}

/// Processes come and go on their own. Comparing them would fire on almost
/// every deletion, and a warning that fires on everything is no warning.
#[test]
fn a_changed_process_count_does_not_block_removal() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-review"))
        .expect("wt-review");

    let status = status_for(&git_bin, target, &ctx);
    let mut approved = plan_removal(&git_bin, target, &status);
    approved.running_processes = 3;

    remove(&git_bin, &repo, &approved, RemoveOptions::default())
        .expect("a process that has since exited is not a reason to refuse");
    assert!(!target.path.exists());
}

#[test]
fn pruning_clears_metadata_for_a_deleted_directory() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let before = list_worktrees(&git_bin, &repo).unwrap();
    assert!(before.iter().any(|e| e.prunable.is_some()));

    prune(&git_bin, &repo).expect("prune succeeds");

    let after = list_worktrees(&git_bin, &repo).unwrap();
    assert!(
        after.iter().all(|e| e.prunable.is_none()),
        "no stale entries remain"
    );
}

#[test]
fn removing_a_worktree_can_also_delete_its_branch() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-merged"))
        .expect("wt-merged");

    let status = status_for(&git_bin, target, &ctx);
    let plan = plan_removal(&git_bin, target, &status);
    git(
        &repo,
        &[
            "config",
            "branch.feat/merged.description",
            "remove with branch",
        ],
    );
    git(
        &repo,
        &[
            "config",
            "branch.feat/merged-extra.description",
            "must survive",
        ],
    );

    remove(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect("removal succeeds");

    let branches = yawm_core::git::collect::load_branches(&git_bin, &repo).unwrap();
    assert!(
        !branches.contains_key("feat/merged"),
        "the branch was deleted too"
    );
    let removed = Command::new("git")
        .current_dir(&repo)
        .args(["config", "--get", "branch.feat/merged.description"])
        .output()
        .expect("git config");
    assert!(
        !removed.status.success(),
        "the exact branch config went too"
    );
    let neighbour = Command::new("git")
        .current_dir(&repo)
        .args(["config", "--get", "branch.feat/merged-extra.description"])
        .output()
        .expect("git config");
    assert!(
        neighbour.status.success(),
        "a similarly named section survives"
    );
    assert_eq!(
        String::from_utf8_lossy(&neighbour.stdout).trim(),
        "must survive"
    );
}

/// Confirming that uncommitted files may be destroyed must not also authorise
/// destroying commits. They are different losses, and only one of them was
/// ever put to the user.
#[test]
fn forcing_past_dirty_files_does_not_force_delete_an_unmerged_branch() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    // Dirty *and* holding a commit the default branch does not have.
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");

    let status = status_for(&git_bin, target, &ctx);
    let plan = plan_removal(&git_bin, target, &status);
    assert!(plan.requires_force, "fixture is meant to need forcing");

    remove(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            // Only the directory's contents were agreed to.
            force: true,
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect("removal succeeds");

    let branches = yawm_core::git::collect::load_branches(&git_bin, &repo).unwrap();
    assert!(
        branches.contains_key("feat/dirty"),
        "git refused to delete the unmerged branch, so the commits survive; \
         got {:?}",
        branches.keys().collect::<Vec<_>>()
    );
}

/// The refusal above is right, but silent. A caller that asked for the branch
/// to go can now find out that it did not.
#[test]
fn a_refused_branch_deletion_is_reported_without_failing_the_removal() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();

    let unmerged = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");
    let status = status_for(&git_bin, unmerged, &ctx);
    let plan = plan_removal(&git_bin, unmerged, &status);

    let outcome = remove_reporting(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            force: true,
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect("removal still succeeds");
    assert_eq!(outcome.branch, BranchOutcome::Kept);

    // A merged branch really does go, so "kept" means something.
    let merged = entries
        .iter()
        .find(|e| e.path.ends_with("wt-merged"))
        .expect("wt-merged");
    let status = status_for(&git_bin, merged, &ctx);
    let plan = plan_removal(&git_bin, merged, &status);
    let outcome = remove_reporting(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect("removal succeeds");
    assert_eq!(outcome.branch, BranchOutcome::Deleted);
}

// ---------------------------------------------------------------------------
// Diff against the default branch
// ---------------------------------------------------------------------------

use yawm_core::diff::{self, DiffScope, EntryContent, FileKind, RepositoryKind};

/// Every entry the UI would hand to the patch viewer must be renderable.
///
/// A section with no hunk draws an expander that opens onto nothing, which is
/// exactly how the panel ended up with rows that looked collapsible and were
/// blank underneath.
fn assert_every_text_entry_has_a_hunk(patches: &diff::Patches) {
    for entry in patches.committed.iter().chain(&patches.uncommitted) {
        if let EntryContent::Text { patch, hunks } = &entry.content {
            assert!(
                *hunks >= 1,
                "text entry {} claims {hunks} hunks",
                entry.path
            );
            assert!(
                patch.contains("@@"),
                "text entry {} has no hunk header:\n{patch}",
                entry.path
            );
        }
    }
}

fn patch_text(patches: &diff::Patches) -> String {
    patches
        .committed
        .iter()
        .chain(&patches.uncommitted)
        .filter_map(|entry| entry.content.patch())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn unborn_diff_uses_current_worktree_content_after_staging() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("unborn");
    git(dir.path(), &["init", "-q", "-b", "main", "unborn"]);
    std::fs::write(root.join("draft.txt"), "staged version\n").unwrap();
    git(&root, &["add", "draft.txt"]);
    std::fs::write(root.join("draft.txt"), "current worktree version\n").unwrap();
    std::fs::write(root.join("untracked.txt"), "untracked version\n").unwrap();

    let entry = list_worktrees(&Git::new(), &root)
        .expect("list unborn worktree")
        .remove(0);
    let inspection = diff::inspect(
        &Git::new(),
        &entry,
        None,
        usize::MAX,
        DiffScope::Uncommitted,
    )
    .expect("inspect unborn worktree");
    let patch = uncommitted_text(&inspection.patches);

    assert!(patch.contains("+current worktree version"), "{patch}");
    assert!(!patch.contains("+staged version"), "{patch}");
    assert!(patch.contains("+untracked version"), "{patch}");
    assert_eq!(inspection.patches.untracked_total, 1);
    assert_eq!(inspection.patches.untracked_shown, 1);
    assert_eq!(inspection.summary.working.files, 2);
}

fn uncommitted_text(patches: &diff::Patches) -> String {
    patches
        .uncommitted
        .iter()
        .filter_map(|entry| entry.content.patch())
        .collect::<Vec<_>>()
        .join("")
}

fn committed_text(patches: &diff::Patches) -> String {
    patches
        .committed
        .iter()
        .filter_map(|entry| entry.content.patch())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn summarises_a_worktrees_changes_against_the_default_branch() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-diff"))
        .expect("wt-diff");

    let summary = diff::summarise(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        DiffScope::History,
    );

    assert!(!summary.is_empty(), "the worktree changed files");
    assert_eq!(summary.commits, 1, "one commit ahead of the base");
    assert!(
        summary.files.iter().any(|f| f.path == "feature.txt"),
        "names the added file, got {:?}",
        summary.files
    );
    assert!(summary.history.insertions > 0);
    assert_eq!(summary.scope, DiffScope::History);
    assert_eq!(summary.base, ctx.default_ref);
}

/// The header bug in one test: a scoped request must not carry the other
/// side's numbers, and the two totals must stay countable on their own.
#[test]
fn history_and_working_totals_are_counted_separately() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-diff"))
        .expect("wt-diff");

    let history = diff::summarise(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        DiffScope::History,
    );

    assert!(history.history.files > 0, "the branch committed something");
    assert!(
        history.working.files > 0,
        "the fixture also has uncommitted work"
    );
    assert!(
        history
            .files
            .iter()
            .any(|file| file.origin == diff::ChangeOrigin::Committed),
        "committed files survive the union list"
    );
    assert!(
        history.history.insertions < history.history.insertions + history.working.insertions,
        "the two sides are distinct numbers, not one blended total"
    );
}

/// Clicking the uncommitted count asks one question. Reading commits to answer
/// it is not extra generosity, it is a different answer.
#[test]
fn an_uncommitted_scope_reads_no_branch_history() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-diff"))
        .expect("wt-diff");

    let inspection = diff::inspect(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        64 * 1024,
        DiffScope::Uncommitted,
    )
    .unwrap();

    assert_eq!(inspection.summary.scope, DiffScope::Uncommitted);
    assert_eq!(inspection.summary.commits, 0);
    assert!(inspection.summary.history.is_empty());
    assert!(inspection.patches.committed.is_empty());
    assert!(
        !inspection
            .summary
            .files
            .iter()
            .any(|f| f.path == "feature.txt"),
        "committed work is not part of the uncommitted scope, got {:?}",
        inspection.summary.files
    );
    assert!(
        !inspection.summary.working.is_empty(),
        "the working side is what was asked for"
    );
    assert_eq!(
        inspection.summary.working.files as usize,
        inspection.summary.files.len(),
        "in this scope the file list is exactly the working set"
    );
}

/// Uncommitted work is part of "what is in here", so it belongs in the summary
/// even though it is in no commit.
#[test]
fn the_summary_includes_uncommitted_changes() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");

    let summary = diff::summarise(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        DiffScope::History,
    );

    assert!(summary.includes_uncommitted);
    assert!(
        summary.files.iter().any(|f| f.path == "a.txt"),
        "a.txt was modified but never committed, got {:?}",
        summary.files
    );
}

#[test]
fn produces_a_readable_patch() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-diff"))
        .expect("wt-diff");

    let patches = diff::patches(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        64 * 1024,
        DiffScope::History,
    )
    .unwrap();

    assert_every_text_entry_has_a_hunk(&patches);
    assert!(
        patches.committed.iter().any(|e| e.path == "feature.txt"),
        "patch names the file"
    );
    assert!(
        committed_text(&patches).contains("+the new feature"),
        "patch shows the addition"
    );

    // This worktree holds both kinds of change, so the split has to be real
    // rather than one side happening to be empty.
    assert!(
        !patches.committed.iter().any(|e| e.path == "a.txt"),
        "an uncommitted edit must not appear in the committed patch"
    );
    assert!(
        patches.uncommitted.iter().any(|e| e.path == "a.txt"),
        "the uncommitted edit belongs to the uncommitted patch"
    );
    assert!(
        !patches.uncommitted.iter().any(|e| e.path == "feature.txt"),
        "committed work must not be repeated as uncommitted"
    );
}

/// A worktree an agent left mid-task usually has no commits of its own, so a
/// patch limited to committed work comes back empty while the summary still
/// reports changed files. A header promising twenty files above a blank body
/// is the bug this guards.
#[test]
fn a_patch_includes_uncommitted_work() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-dirty"))
        .expect("wt-dirty");

    let summary = diff::summarise(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        DiffScope::History,
    );
    let patches = diff::patches(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        64 * 1024,
        DiffScope::History,
    )
    .unwrap();

    assert!(
        summary.includes_uncommitted,
        "fixture is meant to have uncommitted work"
    );
    assert!(
        patches.uncommitted.iter().any(|e| e.path == "a.txt"),
        "uncommitted patch names the file the summary counted"
    );
    assert!(
        uncommitted_text(&patches).contains("+modified"),
        "uncommitted patch shows the change itself"
    );
    // The whole point of splitting them: dirty work must not be filed under
    // "committed", where it would look safely landed.
    assert!(
        !committed_text(&patches).contains("+modified"),
        "uncommitted work must not appear in the committed patch"
    );
    assert!(
        !summary.files.is_empty(),
        "a summary reporting files must come with patches describing them"
    );
    assert!(
        summary
            .files
            .iter()
            .any(|f| f.path == "a.txt" && f.origin == diff::ChangeOrigin::Uncommitted),
        "the file is tagged as uncommitted, got {:?}",
        summary.files
    );
}

#[test]
fn pure_untracked_work_is_summarised_and_patched_without_touching_the_index() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    add_untracked_diff_worktree(&f);
    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|entry| entry.path.ends_with("wt-untracked-diff"))
        .expect("untracked fixture");
    let index_before = git_bin
        .run(&target.path, &["ls-files", "--stage", "-z"])
        .unwrap();

    let inspection = diff::inspect(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        8 * 1024 * 1024,
        DiffScope::Uncommitted,
    )
    .unwrap();

    assert_eq!(status_for(&git_bin, target, &ctx).dirty.untracked, 148);
    assert_eq!(inspection.summary.untracked_total, 148);
    assert_eq!(inspection.summary.untracked_included, 148);
    assert_eq!(
        inspection.summary.untracked_entries, 148,
        "nothing here is a nested repository, so no grouping happens"
    );
    assert!(!inspection.summary.incomplete);
    assert!(inspection.summary.limits.is_empty());
    assert_eq!(inspection.summary.files.len(), 148);
    assert_eq!(inspection.summary.working.files, 148);
    assert!(inspection.summary.includes_uncommitted);
    assert!(
        inspection
            .summary
            .files
            .iter()
            .all(|file| file.origin == diff::ChangeOrigin::Uncommitted)
    );

    // The exact breakdown: 143 two-line text files, one empty file, two
    // binaries, one symlink, and one file whose name is not valid UTF-8.
    let kinds = |matcher: fn(&FileKind) -> bool| {
        inspection
            .summary
            .files
            .iter()
            .filter(|file| matcher(&file.kind))
            .count()
    };
    assert_eq!(kinds(|kind| matches!(kind, FileKind::Binary)), 2);
    assert_eq!(kinds(|kind| matches!(kind, FileKind::Empty)), 1);
    assert_eq!(
        kinds(|kind| matches!(kind, FileKind::Directory { .. })),
        0,
        "no directory rows: git listed every one of these individually"
    );
    #[cfg(unix)]
    {
        assert_eq!(kinds(|kind| matches!(kind, FileKind::Symlink { .. })), 1);
        assert_eq!(kinds(|kind| matches!(kind, FileKind::Text)), 144);
    }
    assert_eq!(
        inspection
            .summary
            .files
            .iter()
            .find(|file| file.path.ends_with("file-000.txt"))
            .map(|file| file.insertions),
        Some(2)
    );
    assert_eq!(
        inspection
            .summary
            .files
            .iter()
            .find(|file| file.path == "empty.txt")
            .map(|file| file.kind.clone()),
        Some(FileKind::Empty),
        "an empty file is empty, not a text patch with nothing in it"
    );
    for binary in ["binary.bin", "large.bin"] {
        assert_eq!(
            inspection
                .summary
                .files
                .iter()
                .find(|file| file.path == binary)
                .map(|file| file.kind.clone()),
            Some(FileKind::Binary),
            "{binary} is binary"
        );
    }

    // The payload the reader actually sees is not empty.
    let text = uncommitted_text(&inspection.patches);
    assert!(!text.is_empty(), "the uncommitted scope rendered nothing");
    assert!(text.contains("+line one for 0\n"));
    assert!(text.contains("@@ -0,0 +1,2 @@"));
    assert_every_text_entry_has_a_hunk(&inspection.patches);
    assert!(
        inspection
            .patches
            .uncommitted
            .iter()
            .filter(|entry| matches!(entry.content, EntryContent::Text { .. }))
            .count()
            >= 143,
        "every readable text file is a renderable section"
    );
    assert!(
        inspection
            .patches
            .uncommitted
            .iter()
            .any(|entry| entry.path == "empty.txt" && entry.content == EntryContent::Empty)
    );
    assert!(
        inspection
            .patches
            .uncommitted
            .iter()
            .any(|entry| entry.path == "binary.bin" && entry.content == EntryContent::Binary)
    );
    #[cfg(unix)]
    assert!(inspection.patches.uncommitted.iter().any(|entry| matches!(
        &entry.content,
        EntryContent::Symlink { target } if target == "generated/group-0/file-000.txt"
    )));
    assert_eq!(inspection.patches.untracked_shown, 148);
    assert_eq!(inspection.patches.untracked_entries, 148);
    assert!(!inspection.patches.truncated);
    assert!(!inspection.patches.incomplete);
    assert_eq!(
        git_bin
            .run(&target.path, &["ls-files", "--stage", "-z"])
            .unwrap(),
        index_before,
        "diff inspection must not stage intent-to-add entries or otherwise mutate the index"
    );
}

/// Git treats nested repositories three different ways, and only one of them
/// is a single path. A bare repository is walked into, so `HEAD`, `hooks/`,
/// `objects/` and `refs/` arrive as separate untracked paths and bury the
/// reader's own files. All three collapse to one row here; an ordinary
/// directory deliberately does not, because its files are the reader's.
#[test]
fn nested_repositories_are_atomic_and_ordinary_directories_are_not() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    add_nested_repository_worktree(&f);
    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|entry| entry.path.ends_with("wt-nested-repos"))
        .expect("nested repository fixture");

    let inspection = diff::inspect(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        8 * 1024 * 1024,
        DiffScope::Uncommitted,
    )
    .unwrap();
    let summary = &inspection.summary;

    assert!(
        !summary
            .files
            .iter()
            .any(|file| file.path.contains(".git/") || file.path.starts_with("remote.git/")),
        "no repository internals are listed individually, got {:?}",
        summary
            .files
            .iter()
            .map(|file| &file.path)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        summary.untracked_entries,
        5,
        "two plain files plus three atomic repositories, got {:?}",
        summary
            .files
            .iter()
            .map(|file| &file.path)
            .collect::<Vec<_>>()
    );
    assert!(
        summary.untracked_total > summary.untracked_entries,
        "the bare repository contributed more raw paths than rows"
    );
    assert_eq!(
        summary.untracked_included, summary.untracked_total,
        "every raw path is still accounted for by some row"
    );
    assert!(!summary.incomplete, "nothing here was skipped");
    assert!(summary.limits.is_empty());

    for (path, expected) in [
        ("nested", RepositoryKind::Nested),
        ("linked", RepositoryKind::LinkedWorktree),
        ("remote.git", RepositoryKind::Bare),
    ] {
        let file = summary
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path} is one row"));
        let FileKind::Repository {
            repository, paths, ..
        } = &file.kind
        else {
            panic!("{path} is not a repository row: {:?}", file.kind);
        };
        assert_eq!(*repository, expected, "{path}");
        assert!(*paths >= 1);
    }
    let bare_paths = summary
        .files
        .iter()
        .find(|file| file.path == "remote.git")
        .map(|file| match &file.kind {
            FileKind::Repository { paths, .. } => *paths,
            other => panic!("{other:?}"),
        })
        .unwrap();
    assert!(
        bare_paths > 1,
        "the bare repository swallowed its own internals"
    );

    // The ordinary directory keeps its files, because they are the reader's.
    for path in ["plain/one.txt", "plain/deep/two.txt"] {
        assert!(
            summary
                .files
                .iter()
                .any(|file| file.path == path && file.kind == FileKind::Text),
            "{path} is still its own row"
        );
    }
    assert_every_text_entry_has_a_hunk(&inspection.patches);
    assert!(
        inspection
            .patches
            .uncommitted
            .iter()
            .find(|entry| entry.path == "remote.git")
            .map(|entry| entry.content.patch())
            .unwrap()
            .is_none(),
        "an atomic repository carries no patch to expand"
    );
}

#[test]
fn mixed_and_staged_only_changes_remain_in_the_uncommitted_diff() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();

    let mixed = entries
        .iter()
        .find(|entry| entry.path.ends_with("wt-dirty"))
        .unwrap();
    let mixed = diff::inspect(
        &git_bin,
        mixed,
        ctx.default_ref.as_deref(),
        64 * 1024,
        DiffScope::History,
    )
    .unwrap();
    for path in ["a.txt", "staged.txt", "untracked.txt"] {
        assert!(
            mixed.summary.files.iter().any(|file| file.path == path),
            "{path} missing from mixed summary"
        );
        assert!(
            mixed.patches.uncommitted.iter().any(|e| e.path == path),
            "{path} missing from mixed patch"
        );
    }
    assert!(
        !mixed.summary.files.iter().any(|file| file.path == ".env"),
        "ignored environment risk signals are not Git changes"
    );

    let staged = entries
        .iter()
        .find(|entry| entry.path.ends_with("wt-staged-only"))
        .unwrap();
    let staged = diff::inspect(
        &git_bin,
        staged,
        ctx.default_ref.as_deref(),
        64 * 1024,
        DiffScope::History,
    )
    .unwrap();
    assert_eq!(staged.summary.files.len(), 1);
    assert_eq!(staged.summary.files[0].path, "a.txt");
    assert_eq!(
        staged.summary.files[0].origin,
        diff::ChangeOrigin::Uncommitted
    );
    assert!(uncommitted_text(&staged.patches).contains("+only in the index"));
}

#[test]
fn a_global_patch_cap_reports_how_many_untracked_files_are_shown() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    add_untracked_diff_worktree(&f);
    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|entry| entry.path.ends_with("wt-untracked-diff"))
        .unwrap();
    std::fs::write(
        target.path.join("oversized.txt"),
        vec![b'x'; 9 * 1024 * 1024],
    )
    .unwrap();

    let inspection = diff::inspect(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        700,
        DiffScope::Uncommitted,
    )
    .unwrap();
    let patch_bytes = inspection.patches.patch_bytes();
    assert!(patch_bytes <= 700, "global cap exceeded: {patch_bytes}");
    assert!(inspection.patches.truncated);
    assert!(inspection.patches.untracked_shown < 149);
    assert_eq!(inspection.patches.untracked_total, 149);
    assert!(inspection.summary.incomplete);
    assert_eq!(
        inspection.summary.untracked_included, 149,
        "a file too large to read is still a path that was accounted for"
    );

    // The reason is named and counted, not hand-waved.
    let display = inspection
        .patches
        .limits
        .iter()
        .find_map(|limit| match limit {
            diff::DiffLimit::DisplayLimit { shown, total } => Some((*shown, *total)),
            _ => None,
        })
        .expect("the display limit says how much was shown");
    assert_eq!(display.1, 149);
    assert!(display.0 < display.1);
    let too_large = inspection
        .summary
        .limits
        .iter()
        .find_map(|limit| match limit {
            diff::DiffLimit::TooLarge { paths, total } => Some((paths.clone(), *total)),
            _ => None,
        })
        .expect("the oversized file is named");
    assert_eq!(too_large.1, 1);
    assert_eq!(too_large.0, vec!["oversized.txt".to_string()]);
    assert!(inspection.patches.incomplete);
    assert_every_text_entry_has_a_hunk(&inspection.patches);
}

#[test]
fn an_oversized_patch_is_truncated() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-diff"))
        .expect("wt-diff");

    let patches = diff::patches(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        40,
        DiffScope::History,
    )
    .unwrap();

    assert!(patches.truncated);
    assert!(patch_text(&patches).len() <= 40);
}

/// A merged worktree has nothing of its own left, which is exactly why it reads
/// as disposable.
#[test]
fn a_merged_worktree_shows_no_changes_of_its_own() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-merged"))
        .expect("wt-merged");

    let summary = diff::summarise(
        &git_bin,
        target,
        ctx.default_ref.as_deref(),
        DiffScope::History,
    );
    assert_eq!(summary.commits, 0);
    assert!(summary.history.is_empty());
}

// ---------------------------------------------------------------------------
// Creating worktrees
// ---------------------------------------------------------------------------

use yawm_core::ops::create::{self, CreateOptions, ProvisionKind};

#[test]
fn creates_a_worktree_from_the_default_branch() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-new");

    let created = create::create(
        &git_bin,
        &repo,
        &CreateOptions {
            branch: "feat/brand-new".into(),
            base: "origin/main".into(),
            path: target.clone(),
            provision: Vec::new(),
        },
    )
    .expect("creation succeeds");

    assert!(created.is_empty(), "nothing was asked for");
    assert!(target.is_dir(), "the worktree exists on disk");

    let after = list_worktrees(&git_bin, &repo).unwrap();
    assert!(
        after
            .iter()
            .any(|w| w.branch.as_deref() == Some("feat/brand-new")),
        "git knows about the new worktree"
    );
}

/// The whole point of provisioning: a fresh worktree normally has no .env, so
/// it cannot run until someone copies one over by hand.
#[test]
fn provisioning_carries_gitignored_files_into_a_new_worktree() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    std::fs::write(repo.join(".env"), "SECRET=from-main").unwrap();
    std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
    std::fs::write(repo.join("node_modules/pkg/index.js"), "module.exports={}").unwrap();

    let target = f.root.join("wt-provisioned");
    let created = create::create(
        &git_bin,
        &repo,
        &CreateOptions {
            branch: "feat/provisioned".into(),
            base: "origin/main".into(),
            path: target.clone(),
            provision: vec![".env".into(), "node_modules".into()],
        },
    )
    .expect("creation succeeds");

    assert!(created.contains(&".env".to_string()));
    assert!(created.contains(&"node_modules".to_string()));

    // Copied, so the worktree can diverge from the main one.
    assert_eq!(
        std::fs::read_to_string(target.join(".env")).unwrap(),
        "SECRET=from-main"
    );
    assert!(
        !std::fs::symlink_metadata(target.join(".env"))
            .unwrap()
            .is_symlink(),
        ".env is copied, not linked"
    );

    // Linked, so it costs no extra disk and needs no install.
    let deps = std::fs::symlink_metadata(target.join("node_modules")).unwrap();
    assert!(deps.is_symlink(), "node_modules is linked, not copied");
    assert!(target.join("node_modules/pkg/index.js").exists());

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let entry = entries
        .iter()
        .find(|entry| path_key(&entry.path) == path_key(&target))
        .expect("created worktree");
    let status = status_for(&git_bin, entry, &ctx);
    assert_eq!(
        status.managed_dependency_links,
        vec![yawm_core::model::ManagedDependencyLink {
            path: "node_modules".into(),
            target: std::fs::canonicalize(repo.join("node_modules")).unwrap(),
        }]
    );
    assert_eq!(status.dirty.total(), 0);
    std::fs::remove_file(target.join(".env")).unwrap();
    let plan = plan_removal(&git_bin, entry, &status);
    assert_eq!(plan.dirty_total, 0);
    assert!(!plan.requires_force);
    remove(&git_bin, &repo, &plan, RemoveOptions::default())
        .expect("a freshly created managed link needs no force to delete");
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn managed_dependency_links_fail_closed_after_every_tamper_shape() {
    use std::os::unix::fs::symlink;

    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    std::fs::write(repo.join("package.json"), "{}\n").unwrap();
    std::fs::write(repo.join("package-lock.json"), "{\"lockfileVersion\":3}\n").unwrap();
    git(&repo, &["add", "package.json", "package-lock.json"]);
    git(&repo, &["commit", "-qm", "dependency manifests"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
    std::fs::write(
        repo.join("node_modules/pkg/index.js"),
        "module.exports={}\n",
    )
    .unwrap();

    let create_link = |branch: &str, target: &Path| {
        create::create(
            &git_bin,
            &repo,
            &CreateOptions {
                branch: branch.into(),
                base: "origin/main".into(),
                path: target.to_path_buf(),
                provision: vec!["node_modules".into()],
            },
        )
        .unwrap();
    };
    let assert_risky = |target: &Path| {
        let entries = list_worktrees(&git_bin, &repo).unwrap();
        let ctx = load_context(&git_bin, &repo, &entries).unwrap();
        let entry = entries
            .iter()
            .find(|entry| path_key(&entry.path) == path_key(target))
            .unwrap();
        let status = status_for(&git_bin, entry, &ctx);
        assert!(
            status.managed_dependency_links.is_empty(),
            "tampered links are not managed: {:?}",
            status.managed_dependency_links
        );
        assert!(
            status.dirty.untracked > 0,
            "tamper must be ordinary untracked work"
        );
        let plan = plan_removal(&git_bin, entry, &status);
        assert!(plan.requires_force);
        assert!(plan.dirty_total > 0);
    };

    let changed = f.root.join("wt-managed-target-changed");
    create_link("feat/managed-target-changed", &changed);
    std::fs::remove_file(changed.join("node_modules")).unwrap();
    let alternate = f.root.join("alternate-dependencies");
    std::fs::create_dir(&alternate).unwrap();
    symlink(&alternate, changed.join("node_modules")).unwrap();
    assert_risky(&changed);
    git(
        &repo,
        &["worktree", "remove", "--force", changed.to_str().unwrap()],
    );

    let missing = f.root.join("wt-managed-target-missing");
    create_link("feat/managed-target-missing", &missing);
    let saved = repo.join("node_modules.saved");
    std::fs::rename(repo.join("node_modules"), &saved).unwrap();
    assert_risky(&missing);
    std::fs::rename(saved, repo.join("node_modules")).unwrap();
    git(
        &repo,
        &["worktree", "remove", "--force", missing.to_str().unwrap()],
    );

    let regular = f.root.join("wt-managed-became-file");
    create_link("feat/managed-became-file", &regular);
    std::fs::remove_file(regular.join("node_modules")).unwrap();
    std::fs::write(regular.join("node_modules"), "not a link\n").unwrap();
    assert_risky(&regular);
    git(
        &repo,
        &["worktree", "remove", "--force", regular.to_str().unwrap()],
    );

    let forgotten = f.root.join("wt-managed-record-missing");
    create_link("feat/managed-record-missing", &forgotten);
    let admin = PathBuf::from(
        git_stdout(&forgotten, &["rev-parse", "--git-dir"])
            .trim()
            .to_string(),
    );
    std::fs::remove_file(admin.join("yawm-managed-dependency-links.json")).unwrap();
    assert_risky(&forgotten);
    git(
        &repo,
        &["worktree", "remove", "--force", forgotten.to_str().unwrap()],
    );

    let incompatible = f.root.join("wt-managed-incompatible");
    create_link("feat/managed-incompatible", &incompatible);
    std::fs::write(
        incompatible.join("package.json"),
        "{\"dependencies\":{\"x\":\"1\"}}\n",
    )
    .unwrap();
    assert_risky(&incompatible);
}

#[test]
fn the_plan_offers_what_the_main_worktree_has() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    std::fs::write(repo.join(".env"), "A=1").unwrap();
    std::fs::write(repo.join(".env.local"), "B=2").unwrap();
    std::fs::create_dir_all(repo.join("node_modules")).unwrap();

    let plan = create::plan(
        &git_bin,
        &repo,
        "feat/planned",
        "origin/main",
        &f.root.join("wt-planned"),
    )
    .unwrap();

    assert!(plan.is_valid());

    let names: Vec<&str> = plan.items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&".env"), "got {names:?}");
    assert!(names.contains(&".env.local"), "got {names:?}");
    assert!(names.contains(&"node_modules"), "got {names:?}");

    let env = plan.items.iter().find(|i| i.name == ".env").unwrap();
    assert_eq!(env.kind, ProvisionKind::CopyFile);
    assert!(env.recommended, "carrying .env over is the common case");

    let deps = plan
        .items
        .iter()
        .find(|i| i.name == "node_modules")
        .unwrap();
    assert_eq!(deps.kind, ProvisionKind::LinkDir);
}

/// Sharing node_modules between worktrees that disagree on dependencies would
/// break the build, so the guard must leave it unticked and say why.
#[test]
fn a_differing_lockfile_stops_dependencies_being_linked_by_default() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    std::fs::write(repo.join("package-lock.json"), r#"{"v":1}"#).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "add lockfile"]);
    git(&repo, &["push", "-q", "origin", "main"]);

    std::fs::write(repo.join("package-lock.json"), r#"{"v":2}"#).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "change lockfile"]);

    std::fs::create_dir_all(repo.join("node_modules")).unwrap();

    let plan = create::plan(
        &git_bin,
        &repo,
        "feat/locked",
        "origin/main",
        &f.root.join("wt-locked-deps"),
    )
    .unwrap();

    let deps = plan
        .items
        .iter()
        .find(|i| i.name == "node_modules")
        .unwrap();
    assert!(
        !deps.recommended,
        "the dependency trees differ, so sharing would break the build"
    );
    assert!(
        deps.caution
            .as_deref()
            .is_some_and(|c| c.contains("package-lock.json")),
        "says which lockfile disagreed, got {:?}",
        deps.caution
    );
}

/// A matching lockfile means the dependency trees are identical, which is what
/// makes linking safe to default to.
#[test]
fn a_matching_lockfile_allows_dependencies_to_be_linked_by_default() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    std::fs::write(repo.join("package-lock.json"), r#"{"v":1}"#).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "add lockfile"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    std::fs::create_dir_all(repo.join("node_modules")).unwrap();

    let plan = create::plan(
        &git_bin,
        &repo,
        "feat/same",
        "origin/main",
        &f.root.join("wt-same-deps"),
    )
    .unwrap();

    let deps = plan
        .items
        .iter()
        .find(|i| i.name == "node_modules")
        .unwrap();
    assert!(deps.recommended, "caution was {:?}", deps.caution);
    assert!(deps.caution.is_none());
}

/// git refuses to check out one branch twice, so yawm says where it already is
/// rather than letting the user hit a failed command.
#[test]
fn a_branch_already_checked_out_elsewhere_blocks_creation() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let plan = create::plan(
        &git_bin,
        &repo,
        "feat/review",
        "origin/main",
        &f.root.join("wt-duplicate"),
    )
    .unwrap();

    assert!(!plan.is_valid());
    assert!(
        plan.branch_in_use_at
            .as_ref()
            .is_some_and(|p| p.ends_with("wt-review")),
        "names the worktree holding it, got {:?}",
        plan.branch_in_use_at
    );

    assert!(
        create::create(
            &git_bin,
            &repo,
            &CreateOptions {
                branch: "feat/review".into(),
                base: "origin/main".into(),
                path: f.root.join("wt-duplicate"),
                provision: Vec::new(),
            },
        )
        .is_err(),
        "creating it anyway must fail"
    );
}

/// Worktrees created inside the repository make agents grep into the wrong
/// tree, so the plan flags it.
#[test]
fn a_nested_path_is_flagged() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let nested = create::plan(
        &git_bin,
        &repo,
        "feat/nested",
        "origin/main",
        &repo.join("inside"),
    )
    .unwrap();
    assert!(nested.path_is_nested);

    let sibling = create::plan(
        &git_bin,
        &repo,
        "feat/sibling",
        "origin/main",
        &f.root.join("outside"),
    )
    .unwrap();
    assert!(!sibling.path_is_nested);
}

#[test]
fn creating_over_an_existing_path_fails() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let occupied = f.root.join("occupied");
    std::fs::create_dir_all(&occupied).unwrap();

    assert!(
        create::create(
            &git_bin,
            &repo,
            &CreateOptions {
                branch: "feat/occupied".into(),
                base: "origin/main".into(),
                path: occupied,
                provision: Vec::new(),
            },
        )
        .is_err()
    );
}

/// A created worktree must be immediately usable: listed, clean, and classified
/// without claiming process certainty that this Git-only path did not gather.
#[test]
fn a_created_worktree_is_immediately_classified() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-fresh");

    create::create(
        &git_bin,
        &repo,
        &CreateOptions {
            branch: "feat/fresh".into(),
            base: "origin/main".into(),
            path: target.clone(),
            provision: Vec::new(),
        },
    )
    .unwrap();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let entry = entries
        .iter()
        .find(|e| e.path.ends_with("wt-fresh"))
        .expect("the new worktree is listed");

    let status = status_for(&git_bin, entry, &ctx);
    assert!(!status.dirty.is_dirty(), "a fresh worktree is clean");

    // Created from the tip of the default branch, so Git containment is
    // immediate. Disposal still waits for a scanner to inspect live processes.
    let (verdict, reason) = classify(entry, &status, &VerdictConfig::default(), i64::MAX / 2);
    assert_eq!(verdict, Verdict::Review);
    assert_eq!(reason, VerdictReason::ProcessCheckSkipped);
}

/// Git for Windows defaults to the legacy MAX_PATH limit even when Rust and
/// the filesystem can address longer paths. This is the repository shape that
/// failed for a real user: a short worktree root with deeply named SQL files.
#[cfg(windows)]
#[test]
fn creates_a_windows_worktree_containing_paths_beyond_max_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonicalize");
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    git(&repo, &["config", "user.email", "test@yawm.dev"]);
    git(&repo, &["config", "user.name", "yawm test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    let relative = PathBuf::from("src")
        .join("cluster-metadata-store-with-a-deliberately-long-directory-name")
        .join("functions-with-another-deliberately-long-directory-name")
        .join("fn_GetNodesByResourceGroupEntityIdAvailabilityZoneVnetCapabilityReplicaSlotCountScheduleStateAndOperatingSystem.sql");
    let source = repo.join(&relative);
    assert!(
        source.to_string_lossy().len() > 260,
        "fixture must exceed MAX_PATH, got {} characters",
        source.to_string_lossy().len()
    );
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::write(&source, "select 1;\n").unwrap();
    git(&repo, &["-c", "core.longpaths=true", "add", "-A"]);
    git(&repo, &["commit", "-qm", "long path"]);

    let target = root.join("worktree");
    create::create(
        &Git::new(),
        &repo,
        &CreateOptions {
            branch: "feature".into(),
            base: "main".into(),
            path: target.clone(),
            provision: Vec::new(),
        },
    )
    .expect("long paths are enabled for the checkout");

    assert!(
        target.join(relative).is_file(),
        "the long tracked file was checked out"
    );
}

// ---------------------------------------------------------------------------
// Landing proofs
// ---------------------------------------------------------------------------

/// Status for one named worktree, the way the other status tests do it.
fn status_of(f: &Fixture, name: &str) -> yawm_core::model::WorktreeStatus {
    let git_bin = Git::new();
    let repo = f.repo();
    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let entry = entries
        .iter()
        .find(|e| e.path.ends_with(name))
        .unwrap_or_else(|| panic!("no worktree named {name}"));
    status_for(&git_bin, entry, &ctx)
}

struct LandingFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    branch: PathBuf,
}

impl LandingFixture {
    fn new(branch_name: &str) -> Self {
        Self::with_shared(branch_name, "base\n")
    }

    fn with_shared(branch_name: &str, shared: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonicalize");
        git(&root, &["init", "-q", "-b", "main", "repo"]);
        let repo = root.join("repo");
        git(&repo, &["config", "user.email", "test@yawm.dev"]);
        git(&repo, &["config", "user.name", "yawm test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("shared.txt"), shared).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "base"]);

        let branch = root.join("branch");
        git(
            &repo,
            &["worktree", "add", "-q", "../branch", "-b", branch_name],
        );
        Self {
            _dir: dir,
            root,
            repo,
            branch,
        }
    }

    fn commit(&self, worktree: &Path, file: &str, contents: &str, subject: &str) {
        std::fs::write(worktree.join(file), contents).unwrap();
        git(worktree, &["add", "-A"]);
        git(worktree, &["commit", "-qm", subject]);
    }

    fn oid(&self, rev: &str) -> String {
        git_stdout(&self.repo, &["rev-parse", rev])
            .trim()
            .to_string()
    }

    fn landing(&self) -> Landing {
        self.status().landing
    }

    fn status(&self) -> yawm_core::model::WorktreeStatus {
        let git_bin = Git::new();
        let entries = list_worktrees(&git_bin, &self.repo).unwrap();
        let ctx = load_context(&git_bin, &self.repo, &entries).unwrap();
        let entry = entries
            .iter()
            // Git for Windows reports an ordinary drive path while
            // `canonicalize` gives the fixture the equivalent `\\?\` spelling.
            // The product compares paths through this same key; the fixture
            // must test the same identity rather than platform spelling.
            .find(|entry| path_key(&entry.path) == path_key(&self.branch))
            .expect("branch worktree");
        status_for(&git_bin, entry, &ctx)
    }

    fn add_origin_at_current_main(&self) {
        git(&self.root, &["init", "-q", "--bare", "remote.git"]);
        git(&self.repo, &["remote", "add", "origin", "../remote.git"]);
        git(&self.repo, &["push", "-q", "-u", "origin", "main"]);
    }
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let out = git_output(cwd, args);
    assert!(
        out.status.success(),
        "git {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("git output is utf-8")
}

#[test]
fn landing_by_ancestry_has_a_reachability_proof() {
    let f = LandingFixture::new("feat/ancestry");
    f.commit(&f.branch, "feature.txt", "feature\n", "add feature");
    git(
        &f.repo,
        &["merge", "-q", "--no-ff", "feat/ancestry", "-m", "merge"],
    );

    assert_eq!(
        f.landing(),
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::Ancestry,
        }
    );
}

#[test]
fn ancestry_after_a_revert_claims_only_historical_reachability() {
    let f = LandingFixture::new("feat/reverted");
    f.commit(&f.branch, "feature.txt", "feature\n", "add feature");
    git(
        &f.repo,
        &["merge", "-q", "--no-ff", "feat/reverted", "-m", "merge"],
    );
    git(&f.repo, &["revert", "--no-edit", "-m", "1", "HEAD"]);
    assert!(
        !f.repo.join("feature.txt").exists(),
        "the revert removes the feature from the current default tree"
    );

    let status = f.status();
    let Landing::Landed { target, proof } = status.landing else {
        panic!("the branch commit remains reachable");
    };
    assert_eq!(proof, LandingProof::Ancestry);
    assert_eq!(
        VerdictReason::WorkContained { target, proof }.describe(),
        "Branch history is reachable from main"
    );
}

#[test]
fn identical_snapshots_have_a_same_tree_proof() {
    let f = LandingFixture::new("feat/same-tree");
    f.commit(&f.branch, "shared.txt", "identical\n", "branch version");
    f.commit(&f.repo, "shared.txt", "identical\n", "main version");

    assert_eq!(
        f.landing(),
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::SameTree,
        }
    );
}

#[test]
fn a_squash_effect_already_at_the_tip_has_a_no_op_proof() {
    let f = LandingFixture::new("feat/no-op-tip");
    f.commit(&f.branch, "feature.txt", "feature\n", "feature branch");
    git(&f.repo, &["merge", "-q", "--squash", "feat/no-op-tip"]);
    git(&f.repo, &["commit", "-qm", "squash feature"]);
    f.commit(&f.repo, "main-only.txt", "later\n", "main moves on");

    assert_eq!(
        f.landing(),
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::NoOpAtTip,
        }
    );
}

#[test]
fn a_historical_snapshot_rescues_a_squash_after_the_tip_conflicts() {
    let f = LandingFixture::new("feat/no-op-ancestor");
    f.commit(
        &f.branch,
        "shared.txt",
        "feature\n",
        "implement historical feature",
    );
    git(&f.repo, &["merge", "-q", "--squash", "feat/no-op-ancestor"]);
    git(&f.repo, &["commit", "-qm", "land historical feature"]);
    let landed_at = f.oid("HEAD");
    f.commit(
        &f.repo,
        "shared.txt",
        "feature evolved on main\n",
        "evolve feature",
    );

    assert_eq!(
        f.landing(),
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::PatchEquivalent(landed_at),
        }
    );
}

#[test]
fn historical_rescue_runs_only_when_the_worktree_is_inspected() {
    let f = LandingFixture::new("feat/lazy-history");
    f.commit(&f.branch, "shared.txt", "feature\n", "historical feature");
    git(&f.repo, &["merge", "-q", "--squash", "feat/lazy-history"]);
    git(&f.repo, &["commit", "-qm", "land historical feature"]);
    let landed_at = f.oid("HEAD");
    f.commit(
        &f.repo,
        "shared.txt",
        "feature evolved on main\n",
        "evolve feature",
    );

    let cache = LandingCache::default();
    let report = Scanner::with_landing_cache(Config::default(), cache.clone())
        .scan_repo(&f.repo, ScanOptions::default())
        .expect("scan");
    let scanned = report
        .worktrees
        .iter()
        .find(|worktree| path_key(&worktree.entry.path) == path_key(&f.branch))
        .expect("branch");
    assert_eq!(
        scanned.status.landing,
        Landing::Unknown {
            reason: UnknownReason::CheckDeferred,
            candidate: None,
        }
    );
    assert!(!scanned.status.landing_complete);

    let inspected = Scanner::with_landing_cache(Config::default(), cache)
        .inspect_worktree(&f.repo, &f.branch)
        .expect("inspect");
    assert_eq!(
        inspected.status.landing,
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::PatchEquivalent(landed_at),
        }
    );
    assert!(inspected.status.landing_complete);
}

#[test]
fn regular_scan_stops_after_the_tip_merge_conflicts() {
    let f = LandingFixture::new("feat/tier-two-only");
    f.commit(&f.branch, "shared.txt", "feature\n", "tiered feature");
    git(&f.repo, &["merge", "-q", "--squash", "feat/tier-two-only"]);
    git(&f.repo, &["commit", "-qm", "land tiered feature"]);
    let landed_at = f.oid("HEAD");
    f.commit(
        &f.repo,
        "shared.txt",
        "feature evolved on main\n",
        "evolve tiered feature",
    );
    std::thread::sleep(std::time::Duration::from_secs(1));

    let config = Config {
        active_within_minutes: 0,
        ..Default::default()
    };
    let cache = LandingCache::default();
    let scanned = Scanner::with_landing_cache(config.clone(), cache.clone())
        .scan_repo(&f.repo, ScanOptions::default())
        .expect("scan")
        .worktrees
        .into_iter()
        .find(|worktree| path_key(&worktree.entry.path) == path_key(&f.branch))
        .expect("branch");
    assert!(matches!(
        scanned.status.landing,
        Landing::Unknown {
            reason: UnknownReason::OverlappingChanges { .. },
            candidate: None,
        }
    ));
    assert!(!scanned.status.landing_complete);

    let inspected = Scanner::with_landing_cache(config, cache)
        .inspect_worktree(&f.repo, &f.branch)
        .expect("inspect");
    assert_eq!(
        inspected.status.landing,
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::PatchEquivalent(landed_at),
        }
    );
    assert!(inspected.status.landing_complete);
}

#[test]
fn historical_rescue_refuses_an_unbounded_target_range() {
    let f = LandingFixture::new("feat/long-history");
    f.commit(
        &f.branch,
        "shared.txt",
        "branch implementation\n",
        "Implement feature",
    );
    f.commit(
        &f.repo,
        "shared.txt",
        "different main implementation\n",
        "Implement feature",
    );
    for index in 0..301 {
        commit_empty(&f.repo, &format!("main history {index}"));
    }

    assert_eq!(
        f.landing(),
        Landing::Unknown {
            reason: UnknownReason::HistoryRangeTooLarge {
                commits: 301,
                limit: 300,
            },
            candidate: None,
        }
    );
}

#[test]
fn a_clean_merge_that_changes_the_target_adds_content() {
    use yawm_core::model::{HeadState, UpstreamState};

    let f = LandingFixture::new("feat/adds-content");
    f.commit(
        &f.branch,
        "feature.txt",
        "only on branch\n",
        "branch content",
    );
    f.commit(&f.repo, "main-only.txt", "only on main\n", "main content");

    let status = f.status();
    assert_eq!(
        status.landing,
        Landing::AddsContent {
            target: "main".into()
        }
    );
    let facts = status.landing_facts;
    let target = facts.selected_target.expect("selected target");
    assert_eq!(target.name, "main");
    assert_eq!(target.short_oid.as_deref().map(str::len), Some(12));
    assert_eq!(facts.commits_ahead, Some(1));
    assert!(matches!(
        facts.head,
        HeadState::Branch { name, .. } if name == "feat/adds-content"
    ));
    assert_eq!(facts.upstream, UpstreamState::None);
}

#[test]
fn an_orphan_branch_reports_no_merge_base_as_topology_not_rewrite() {
    use yawm_core::model::{HeadState, ProofPhase};

    let f = LandingFixture::new("feat/will-be-orphan");
    git(&f.branch, &["checkout", "-q", "--orphan", "feat/orphan"]);
    git(&f.branch, &["rm", "-q", "-f", "shared.txt"]);
    std::fs::write(f.branch.join("orphan.txt"), "disconnected\n").unwrap();
    git(&f.branch, &["add", "-A"]);
    git(&f.branch, &["commit", "-qm", "orphan root"]);

    let status = f.status();
    assert!(matches!(status.landing, Landing::Unknown { .. }));
    assert!(matches!(
        status.landing_facts.head,
        HeadState::Orphan { ref branch, .. } if branch == "feat/orphan"
    ));
    assert_eq!(
        status.landing_facts.unknown_reason,
        Some(UnknownReason::NoMergeBase)
    );
    assert_eq!(status.landing_facts.proof_phase, Some(ProofPhase::Ancestry));
}

#[test]
fn a_modified_rewrite_stays_unknown_and_names_its_candidate() {
    let f = LandingFixture::new("feat/modified-rewrite");
    f.commit(
        &f.branch,
        "shared.txt",
        "branch implementation\n",
        "Implement feature",
    );
    f.commit(
        &f.repo,
        "shared.txt",
        "different main implementation\n",
        "Implement feature",
    );
    let candidate = f.oid("main");

    match f.landing() {
        Landing::Unknown {
            reason: UnknownReason::OverlappingChanges { paths: 1 },
            candidate: Some(found),
        } => {
            assert_eq!(found.commit, candidate, "names the commit it compared to");
            // The two implementations genuinely differ, so the branch has a
            // line the default branch never took — which is the whole point of
            // reporting leftovers rather than a match percentage.
            assert!(
                found.leftover > 0,
                "a competing implementation leaves something behind, got {found:?}"
            );
        }
        other => panic!("expected an unknown landing with a candidate, got {other:?}"),
    }
}

#[test]
fn a_custom_merge_driver_cannot_fabricate_a_no_op_proof() {
    let f = LandingFixture::new("feat/custom-driver");
    f.commit(&f.branch, "feature.txt", "feature\n", "feature branch");
    git(&f.repo, &["merge", "-q", "--squash", "feat/custom-driver"]);
    git(&f.repo, &["commit", "-qm", "squash feature"]);
    f.commit(&f.repo, "main-only.txt", "later\n", "main moves on");
    git(&f.repo, &["config", "merge.fabricated.driver", "true"]);

    assert_eq!(
        f.landing(),
        Landing::Unknown {
            reason: UnknownReason::CustomMergeDriver,
            candidate: None,
        }
    );
}

#[test]
fn a_merge_attribute_cannot_fabricate_a_no_op_proof() {
    let f = LandingFixture::new("feat/merge-attribute");
    f.commit(&f.branch, "feature.txt", "feature\n", "feature branch");
    f.commit(
        &f.repo,
        ".gitattributes",
        "*.txt merge=union\n",
        "configure merge attributes",
    );
    git(
        &f.repo,
        &["merge", "-q", "--squash", "feat/merge-attribute"],
    );
    git(&f.repo, &["commit", "-qm", "squash feature"]);
    f.commit(&f.repo, "main-only.bin", "later\n", "main moves on");

    assert_eq!(
        f.landing(),
        Landing::Unknown {
            reason: UnknownReason::MergeAttributes,
            candidate: None,
        }
    );
}

#[test]
fn replace_refs_cannot_fabricate_ancestry() {
    let f = LandingFixture::new("feat/replaced");
    f.commit(
        &f.branch,
        "feature.txt",
        "only on branch\n",
        "branch content",
    );
    f.commit(&f.repo, "main-only.txt", "only on main\n", "main content");
    let head = f.oid("feat/replaced");
    let target = f.oid("main");
    git(&f.repo, &["replace", &head, &target]);

    assert_eq!(
        f.landing(),
        Landing::AddsContent {
            target: "main".into()
        }
    );
}

#[test]
fn landing_in_any_resolved_default_target_wins() {
    let f = LandingFixture::new("feat/local-only");
    f.add_origin_at_current_main();
    f.commit(&f.branch, "feature.txt", "local merge\n", "local feature");
    git(
        &f.repo,
        &["merge", "-q", "--no-ff", "feat/local-only", "-m", "merge"],
    );

    assert_eq!(
        f.landing(),
        Landing::Landed {
            target: "main".into(),
            proof: LandingProof::Ancestry,
        }
    );
}

#[test]
fn a_symbolic_default_whose_target_is_missing_uses_conventional_fallbacks() {
    let f = LandingFixture::new("feat/unresolved-target");
    f.commit(&f.branch, "feature.txt", "branch\n", "branch content");
    f.commit(&f.repo, "main-only.txt", "main\n", "main content");
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/missing",
        ],
    );

    assert_eq!(
        f.landing(),
        Landing::AddsContent {
            target: "main".into(),
        }
    );
}

#[cfg(unix)]
#[test]
fn a_git_execution_failure_remains_an_ancestry_failure_not_a_rewrite_diagnosis() {
    use std::os::unix::fs::PermissionsExt;
    use yawm_core::model::ProofPhase;

    let f = LandingFixture::new("feat/git-failure");
    f.commit(&f.branch, "feature.txt", "feature\n", "feature");
    let wrapper = f.root.join("git-failing-ancestry");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\ncase \" $* \" in *\" merge-base --is-ancestor \"*) exit 2;; esac\nexec git \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let failing = Git::with_program(wrapper.to_string_lossy());

    let entries = list_worktrees(&failing, &f.repo).unwrap();
    let ctx = load_context(&failing, &f.repo, &entries).unwrap();
    let entry = entries
        .iter()
        .find(|entry| path_key(&entry.path) == path_key(&f.branch))
        .unwrap();
    let mut status = status_for(&failing, entry, &ctx);
    status.process_check_complete = true;
    assert_eq!(
        status.landing,
        Landing::Unknown {
            reason: UnknownReason::GitCommandFailed {
                phase: ProofPhase::Ancestry,
            },
            candidate: None,
        }
    );
    assert_eq!(
        status.landing_facts.unknown_reason,
        Some(UnknownReason::GitCommandFailed {
            phase: ProofPhase::Ancestry,
        })
    );
    assert_eq!(status.landing_facts.proof_phase, Some(ProofPhase::Ancestry));
    let (verdict, reason) = classify(entry, &status, &VerdictConfig::default(), i64::MAX / 2);
    assert_eq!(verdict, Verdict::Review);
    assert!(matches!(reason, VerdictReason::LandingUnknown { .. }));
    assert!(!reason.describe().to_lowercase().contains("rewrite"));
}

#[test]
fn an_authoritative_origin_head_excludes_every_stale_conventional_target() {
    let f = LandingFixture::new("feat/default-transition");
    f.commit(
        &f.branch,
        "feature.txt",
        "only on the old default\n",
        "feature retained by stale main",
    );
    f.add_origin_at_current_main();
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(&f.branch, &["push", "-q", "--force", "origin", "HEAD:main"]);
    assert!(matches!(
        f.landing(),
        Landing::Landed {
            target,
            proof: LandingProof::Ancestry
        } if target == "origin/main"
    ));

    // The default moves main -> trunk. The local default is renamed too, while
    // the old origin/main tracking ref deliberately retains the feature.
    git(&f.repo, &["branch", "-m", "main", "trunk"]);
    git(&f.repo, &["push", "-q", "-u", "origin", "trunk"]);
    let remote = f.root.join("remote.git");
    git(
        &f.root,
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/trunk",
        ],
    );
    git(
        &f.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );

    let git_bin = Git::new();
    let entries = list_worktrees(&git_bin, &f.repo).unwrap();
    let ctx = load_context(&git_bin, &f.repo, &entries).unwrap();
    assert_eq!(ctx.merge_refs, vec!["origin/trunk", "trunk"]);
    assert_eq!(
        f.landing(),
        Landing::AddsContent {
            target: "origin/trunk".into()
        },
        "stale origin/main must not become an alternate landing target"
    );

    // The remote branch is deleted without pruning the local tracking ref.
    git(
        &f.root,
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "update-ref",
            "-d",
            "refs/heads/main",
        ],
    );
    assert!(
        git_stdout(
            &f.repo,
            &["rev-parse", "--verify", "refs/remotes/origin/main"]
        )
        .trim()
        .len()
            >= 40,
        "the stale local origin/main tracking ref remains"
    );
    assert_eq!(
        f.landing(),
        Landing::AddsContent {
            target: "origin/trunk".into()
        },
        "a deleted remote main with stale local tracking metadata is still excluded"
    );
}

#[test]
fn overlapping_changes_are_unknown_not_unlanded() {
    let status = status_of(&Fixture::build(), "wt-conflict");

    match &status.landing {
        Landing::Unknown {
            reason: UnknownReason::OverlappingChanges { paths },
            ..
        } => assert_eq!(*paths, 1),
        other => panic!("expected an unknown landing state, got {other:?}"),
    }
}

#[test]
fn a_clean_branch_that_changes_the_target_adds_content() {
    let status = status_of(&Fixture::build(), "wt-diff");
    assert!(matches!(status.landing, Landing::AddsContent { .. }));
}

#[test]
fn ancestry_is_a_landing_proof() {
    let f = Fixture::build();
    for name in ["wt-merged", "wt-local-merged"] {
        assert!(
            matches!(
                status_of(&f, name).landing,
                Landing::Landed {
                    proof: LandingProof::Ancestry,
                    ..
                }
            ),
            "{name} is reachable from a default branch"
        );
    }
}

fn candidate_patch(
    f: &LandingFixture,
    candidate: &str,
    max_bytes: usize,
) -> yawm_core::git::landing::UniquePatch {
    let head = git_stdout(&f.branch, &["rev-parse", "HEAD"]);
    yawm_core::git::landing::unique_patch(
        &Git::new(),
        &f.repo,
        head.trim(),
        "main",
        candidate,
        max_bytes,
    )
    .expect("unique patch")
}

#[test]
fn unique_patch_marks_an_addition_only_leftover() {
    let f = LandingFixture::with_shared("feat/unique-addition", "alpha\nbeta\ngamma\n");
    f.commit(
        &f.branch,
        "shared.txt",
        "alpha\nbeta\nbranch only\ngamma\n",
        "branch addition",
    );
    commit_empty(&f.repo, "candidate without the addition");
    let candidate = f.oid("main");

    let patch = candidate_patch(&f, &candidate, 64 * 1024);

    assert_eq!(patch.line_count, 1);
    assert_eq!(patch.file_count, 1);
    assert_eq!(patch.markers.len(), 1);
    assert_eq!(
        patch.markers[0].side,
        yawm_core::git::landing::UniqueLineSide::Additions
    );
    assert_eq!(patch.markers[0].line_number, 3);
    assert!(patch.patch.contains("+branch only"));
}

#[test]
fn unique_patch_marks_a_deletion_the_target_kept() {
    let f = LandingFixture::with_shared("feat/unique-deletion", "alpha\nremove me\ngamma\n");
    f.commit(&f.branch, "shared.txt", "alpha\ngamma\n", "branch deletion");
    commit_empty(&f.repo, "candidate kept the line");
    let candidate = f.oid("main");

    let patch = candidate_patch(&f, &candidate, 64 * 1024);

    assert_eq!(patch.line_count, 1);
    assert_eq!(patch.markers.len(), 1);
    assert_eq!(
        patch.markers[0].side,
        yawm_core::git::landing::UniqueLineSide::Deletions
    );
    assert_eq!(patch.markers[0].line_number, 2);
    assert!(patch.patch.contains("-remove me"));
}

#[test]
fn unique_patch_keeps_the_whole_hunk_around_one_leftover() {
    let base = [
        "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8", "line 9",
    ];
    let f = LandingFixture::with_shared("feat/unique-context", &format!("{}\n", base.join("\n")));
    let mut branch = base;
    branch[3] = "branch only";
    branch[6] = "shared edit";
    f.commit(
        &f.branch,
        "shared.txt",
        &format!("{}\n", branch.join("\n")),
        "branch edits",
    );
    let mut target = base;
    target[3] = "target alternative";
    target[6] = "shared edit";
    f.commit(
        &f.repo,
        "shared.txt",
        &format!("{}\n", target.join("\n")),
        "candidate edits",
    );
    let candidate = f.oid("main");

    let patch = candidate_patch(&f, &candidate, 64 * 1024);

    assert_eq!(
        patch.line_count, 1,
        "only the differing addition is unmatched"
    );
    assert_eq!(patch.markers.len(), 1);
    assert_eq!(patch.patch.matches("@@ -").count(), 1);
    assert!(
        patch.patch.contains("+shared edit"),
        "a matching changed line in the retained hunk remains readable:\n{}",
        patch.patch
    );
    assert!(
        patch.patch.contains(" line 2") && patch.patch.contains(" line 9"),
        "the hunk keeps its surrounding context:\n{}",
        patch.patch
    );
}

#[test]
fn unique_patch_with_zero_leftovers_has_no_filtered_files() {
    let f = LandingFixture::with_shared("feat/no-unique-lines", "alpha\nbeta\n");
    f.commit(
        &f.branch,
        "shared.txt",
        "alpha\nshared addition\nbeta\n",
        "branch copy",
    );
    f.commit(
        &f.repo,
        "shared.txt",
        "alpha\nshared addition\nbeta\n",
        "candidate copy",
    );
    let candidate = f.oid("main");

    let patch = candidate_patch(&f, &candidate, 64 * 1024);

    assert_eq!(patch.line_count, 0);
    assert_eq!(patch.file_count, 0);
    assert!(patch.markers.is_empty());
    assert!(patch.patch.is_empty());
    assert!(!patch.incomplete);
    assert!(!patch.truncated);
}

#[test]
fn unique_patch_truncates_only_between_whole_hunks() {
    let base = (1..=40)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>();
    let f = LandingFixture::with_shared("feat/unique-cap", &format!("{}\n", base.join("\n")));
    let mut branch = base.clone();
    branch[1] = "first branch change".into();
    branch[34] = "second branch change".into();
    f.commit(
        &f.branch,
        "shared.txt",
        &format!("{}\n", branch.join("\n")),
        "two distant changes",
    );
    commit_empty(&f.repo, "candidate without either change");
    let candidate = f.oid("main");
    let full = candidate_patch(&f, &candidate, usize::MAX);
    let second_hunk = full
        .patch
        .match_indices("@@ -")
        .nth(1)
        .map(|(index, _)| index)
        .expect("two hunks");

    let capped = candidate_patch(&f, &candidate, second_hunk);

    assert!(capped.truncated);
    assert_eq!(capped.line_count, 4);
    assert_eq!(capped.patch.matches("@@ -").count(), 1);
    assert_eq!(
        capped.patch,
        full.patch[..second_hunk],
        "the cap drops the next hunk instead of cutting through it"
    );
    assert_eq!(capped.markers.len(), 2);
}

#[test]
fn focused_patch_for_adds_content_diffs_the_clean_merge_result() {
    let f = LandingFixture::with_shared("feat/merge-view", "shared\n");
    f.commit(
        &f.branch,
        "feature.txt",
        "branch feature\n",
        "branch feature",
    );
    f.commit(&f.repo, "main.txt", "later main work\n", "main moves on");
    let head = git_stdout(&f.branch, &["rev-parse", "HEAD"]);

    let focus = yawm_core::git::landing::focused_patch(
        &Git::new(),
        &f.repo,
        head.trim(),
        &["main".into()],
        LandingCache::default(),
        64 * 1024,
    );

    match focus {
        yawm_core::git::landing::FocusedPatch::WouldChange { patch } => {
            assert!(patch.patch.contains("+branch feature"));
            assert!(
                !patch.patch.contains("-later main work"),
                "the default branch's later work is the merge baseline, not a deletion:\n{}",
                patch.patch
            );
            assert_eq!(patch.target, "main");
        }
        other => panic!("expected a clean merge patch, got {other:?}"),
    }
}

#[test]
fn unique_patch_uses_a_file_card_for_an_unmatched_binary() {
    let f = LandingFixture::with_shared("feat/binary-focus", "base\0image");
    std::fs::write(f.branch.join("shared.txt"), b"branch\0image").unwrap();
    git(&f.branch, &["commit", "-qam", "change binary"]);
    commit_empty(&f.repo, "candidate kept the binary");
    let candidate = f.oid("main");

    let patch = candidate_patch(&f, &candidate, 64 * 1024);

    assert_eq!(
        patch.line_count, 0,
        "binary data does not invent line counts"
    );
    assert_eq!(patch.file_count, 1);
    assert!(patch.incomplete);
    assert!(patch.markers.is_empty());
    assert!(patch.patch.contains("Binary files"));
}

#[test]
fn an_exact_binary_blob_proves_that_file_is_contained() {
    let f = LandingFixture::with_shared("feat/contained-binary", "base\0image");
    for (worktree, message) in [
        (&f.branch, "branch binary change"),
        (&f.repo, "candidate binary change"),
    ] {
        std::fs::write(worktree.join("shared.txt"), b"same\0image").unwrap();
        git(worktree, &["commit", "-qam", message]);
    }
    let candidate = f.oid("main");

    let patch = candidate_patch(&f, &candidate, 64 * 1024);

    assert_eq!(patch.line_count, 0);
    assert_eq!(patch.file_count, 0);
    assert!(!patch.incomplete);
    assert!(patch.patch.is_empty());
}

#[test]
fn focused_patch_without_a_defensible_target_falls_back_to_all() {
    let f = LandingFixture::new("feat/no-focus-target");
    f.commit(&f.branch, "feature.txt", "feature\n", "branch feature");
    let head = git_stdout(&f.branch, &["rev-parse", "HEAD"]);

    let focus = yawm_core::git::landing::focused_patch(
        &Git::new(),
        &f.repo,
        head.trim(),
        &[],
        LandingCache::default(),
        64 * 1024,
    );

    assert_eq!(
        focus,
        yawm_core::git::landing::FocusedPatch::All {
            reason: yawm_core::git::landing::AllChangesReason::Unsafe
        }
    );
}

/// Trash is offered because it can be undone. Deleting the branch in the same
/// step means what comes back is a directory git does not recognise, on a
/// branch that is gone — so the pair is refused, and refused before anything
/// has been moved.
#[test]
fn trashing_a_worktree_cannot_also_delete_its_branch() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();

    let entries = list_worktrees(&git_bin, &repo).unwrap();
    let ctx = load_context(&git_bin, &repo, &entries).unwrap();
    let target = entries
        .iter()
        .find(|e| e.path.ends_with("wt-merged"))
        .expect("wt-merged");

    let status = status_for(&git_bin, target, &ctx);
    let plan = plan_removal(&git_bin, target, &status);
    let branch = plan.branch.clone().expect("fixture worktree has a branch");

    let err = remove(
        &git_bin,
        &repo,
        &plan,
        RemoveOptions {
            use_trash: true,
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect_err("the contradictory pair is refused");

    assert!(
        err.to_string().contains("Trash"),
        "the refusal should name the option it is about, got: {err}"
    );

    // Nothing ran: the directory is untouched and the branch is still there.
    assert!(
        target.path.is_dir(),
        "refusing must happen before the directory moves"
    );
    let branches = yawm_core::git::collect::load_branches(&git_bin, &repo).unwrap();
    assert!(
        branches.contains_key(&branch),
        "the branch survives a refused removal; got {:?}",
        branches.keys().collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Removal: who removed it, and what a half-done prune leaves behind
// ---------------------------------------------------------------------------

/// A worktree that disappears under the batch is not one yawm deleted.
///
/// Reconciliation asks "is it gone?", and an absence on its own does not say
/// who caused it. Without tracking whether yawm ever reached for a request,
/// a worktree removed by an agent, a script, or a person mid-batch is
/// indistinguishable from one yawm removed — and gets reported as a yawm
/// removal. That is wrong twice: it credits yawm with work it did not do, and
/// it hides the fact that something else is writing to this repository.
#[test]
fn a_worktree_removed_by_something_else_is_reported_as_vanished_not_removed() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let first = f.root.join("wt-review");
    let second = f.root.join("wt-merged");

    let plans = plan_removals(&git_bin, &repo, &[first.clone(), second.clone()]).expect("plan");

    let err = remove_all_after_each(
        &git_bin,
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
        &mut |removed| {
            if path_key(removed) == path_key(&first) {
                // Something outside yawm takes the second one away, between
                // the first removal finishing and the second being reached.
                git(&repo, &["worktree", "remove", "--force", "../wt-merged"]);
            }
        },
    )
    .expect_err("the second worktree is no longer there to remove");

    let yawm_core::error::Error::BatchIncomplete(partial) = &err else {
        panic!("a batch that deleted something must never report a bare refusal: {err:?}");
    };

    assert_eq!(
        partial.completed.len(),
        1,
        "only the one yawm actually removed: {:?}",
        partial.completed
    );
    assert_eq!(path_key(&partial.completed[0].path), path_key(&first));
    assert!(
        !partial
            .completed
            .iter()
            .any(|done| path_key(&done.path) == path_key(&second)),
        "yawm never touched the second one, so it cannot claim to have removed it"
    );
    assert_eq!(
        partial.vanished.len(),
        1,
        "the second one is accounted for, separately: {:?}",
        partial.vanished
    );
    assert_eq!(path_key(&partial.vanished[0]), path_key(&second));

    let message = err.to_string();
    assert!(
        message.contains("wt-merged") && message.contains("something else"),
        "the user is told another process removed it: {message}"
    );

    assert!(!first.exists());
    assert!(!second.exists());
}

/// A git wrapper that reports the locked worktree as prunable too.
///
/// Real git never says both at once — it suppresses `prunable` for anything
/// locked — but `git worktree prune` skipping locked worktrees is precisely why
/// the batched prune lifts locks by name before it runs. That branch is
/// unreachable from a repository, so the state it exists for is arranged here
/// instead: a stale entry the user locked, which the prune cannot take until
/// its lock comes off.
///
/// `blind_after_prune` additionally makes `worktree list` fail once a prune has
/// been attempted, which is the state in which the prune's success is reported
/// and nothing about what it did can be proven. In both shapes the prune itself
/// exits zero without pruning: git's prune answers for the repository, not for
/// the entries the batch asked about.
#[cfg(unix)]
fn git_whose_prune_proves_nothing(root: &Path, name: &str, blind_after_prune: bool) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    let marker = root.join(format!("{name}.pruned"));
    let blind = if blind_after_prune {
        format!(
            "  if [ -f \"{marker}\" ]; then\n    \
             echo \"shim refused worktree list\" >&2\n    exit 1\n  fi\n",
            marker = marker.display()
        )
    } else {
        String::new()
    };
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"worktree\" ] && [ \"$2\" = \"list\" ]; then\n\
             {blind}  \
             git \"$@\" | tr '\\000' '\\n' \\\n    \
             | awk '{{ print }} /^locked/ {{ print \"prunable gitdir file points to non-existent location\" }}' \\\n    \
             | tr '\\n' '\\000'\n  \
             exit 0\nfi\n\
             if [ \"$1\" = \"worktree\" ] && [ \"$2\" = \"prune\" ]; then\n  \
             : > \"{marker}\"\n  exit 0\nfi\n\
             exec git \"$@\"\n",
            marker = marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

/// Make the locked fixture worktree stale, keeping its lock.
///
/// The lock lives in git's administrative data, so removing the directory
/// leaves a worktree with a lock and nothing to look at.
#[cfg(unix)]
fn stale_but_locked(f: &Fixture) -> PathBuf {
    let locked = f.root.join("wt-locked");
    std::fs::remove_dir_all(&locked).expect("remove the directory behind git's back");
    locked
}

/// The reason git currently holds for a worktree's lock, if it is locked.
#[cfg(unix)]
fn lock_reason_of(git_bin: &Git, repo: &Path, path: &Path) -> Option<Option<String>> {
    let entries = list_worktrees(git_bin, repo).expect("list worktrees");
    registered(&entries, path)
        .and_then(|entry| entry.locked.clone())
        .map(|lock| lock.reason)
}

/// The prune ran, and then the repository could not be read back.
///
/// The prune's success is a statement about the repository, not about these
/// entries, and the readback is the only thing that turns it into one. When
/// that readback fails, nothing about these worktrees is proven — so the locks
/// lifted for the prune go back on, exactly as they were. Reporting the prune's
/// success and walking away would leave a worktree that still exists with the
/// "do not touch" somebody set on it silently gone.
#[cfg(unix)]
#[test]
fn a_prune_whose_readback_fails_puts_every_lifted_lock_back() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let locked = stale_but_locked(&f);
    let shim = git_whose_prune_proves_nothing(&f.root, "git-prune-then-blind", true);

    let plan = plan_removals(&shim, &repo, std::slice::from_ref(&locked))
        .expect("plan")
        .remove(0);
    assert!(plan.is_prunable && plan.is_locked);
    assert_eq!(plan.lock_reason.as_deref(), Some("agent running"));

    let err = remove_all(
        &shim,
        &repo,
        &requests(vec![(
            plan,
            RemoveOptions {
                unlock: true,
                ..Default::default()
            },
        )]),
    )
    .expect_err("the readback that proves what the prune did failed");

    let message = err.to_string();
    assert!(
        message.contains("shim refused worktree list"),
        "the failure that stopped it is reported: {message}"
    );

    // Read with real git: the shim is deliberately blind from here on.
    assert_eq!(
        lock_reason_of(&git_bin, &repo, &locked),
        Some(Some("agent running".to_string())),
        "the lock is back, with the words somebody wrote on it"
    );
}

/// The prune reported success and left the entry registered.
///
/// `git worktree prune` returns zero for the repository whether or not it took
/// the entry the batch asked about. An entry still registered afterwards was
/// not pruned, so its lock was lifted for an operation that never reached it —
/// and putting it back is the only outcome in which nothing about a worktree
/// yawm failed to remove has changed.
#[cfg(unix)]
#[test]
fn a_prune_that_leaves_an_entry_registered_puts_its_lock_back() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let locked = stale_but_locked(&f);
    let shim = git_whose_prune_proves_nothing(&f.root, "git-prune-noop", false);

    let plan = plan_removals(&shim, &repo, std::slice::from_ref(&locked))
        .expect("plan")
        .remove(0);
    assert!(plan.is_prunable && plan.is_locked);

    let err = remove_all(
        &shim,
        &repo,
        &requests(vec![(
            plan,
            RemoveOptions {
                unlock: true,
                ..Default::default()
            },
        )]),
    )
    .expect_err("the entry is still registered, so it was not pruned");

    let message = err.to_string();
    assert!(
        message.contains("still registered"),
        "the user is told the prune did not take it: {message}"
    );

    assert!(
        registered(&list_worktrees(&git_bin, &repo).expect("list"), &locked).is_some(),
        "the entry survived, which is exactly why its lock has to be back"
    );
    assert_eq!(
        lock_reason_of(&git_bin, &repo, &locked),
        Some(Some("agent running".to_string())),
        "the lock is back on the survivor, with its original reason"
    );
}

// ---------------------------------------------------------------------------
// Removal: what the plan's readable fields cannot express
// ---------------------------------------------------------------------------

/// Force a removal of one worktree and give back the refusal.
fn forced_removal_refused(
    git_bin: &Git,
    repo: &Path,
    plan: RemovalPlan,
) -> yawm_core::error::Error {
    remove_all(
        git_bin,
        repo,
        &requests(vec![(
            plan,
            RemoveOptions {
                force: true,
                ..Default::default()
            },
        )]),
    )
    .expect_err("the worktree is not the one that was approved")
}

#[test]
fn an_environment_file_after_the_old_candidate_cap_still_requires_force() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");

    for i in 0..200 {
        std::fs::write(target.join(format!(".env.{i:03}")), "tracked\n").unwrap();
    }
    git(&target, &["add", "-f", "."]);
    git(&target, &["commit", "-qm", "tracked environment fixtures"]);
    std::fs::write(target.join(".env.zzz"), "SECRET=unique\n").unwrap();

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);

    assert_eq!(plan.env_files, vec![".env.zzz".to_string()]);
    assert!(
        plan.requires_force,
        "a risky file hidden beyond the former candidate cap cannot be deleted without force"
    );
}

#[test]
fn swapping_an_approved_plan_to_an_equivalent_worktree_path_is_refused() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let one = f.root.join("detached-one");
    let two = f.root.join("detached-two");
    let one_arg = one.to_string_lossy().into_owned();
    let two_arg = two.to_string_lossy().into_owned();
    git(
        &repo,
        &["worktree", "add", "-q", "--detach", &one_arg, "HEAD"],
    );
    git(
        &repo,
        &["worktree", "add", "-q", "--detach", &two_arg, "HEAD"],
    );

    let mut plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&one))
        .expect("plan")
        .remove(0);
    plan.path = two.clone();

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![(plan, RemoveOptions::default())]),
    )
    .expect_err("a fingerprint for another path authorises nothing here");

    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(one.is_dir() && two.is_dir(), "nothing was removed");
}

#[test]
fn a_tampered_main_flag_is_rejected_after_revalidation() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let mut plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&repo))
        .expect("plan")
        .remove(0);
    assert!(plan.is_main);
    plan.is_main = false;

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![(plan, RemoveOptions::default())]),
    )
    .expect_err("the revalidated main-worktree invariant must be enforced");

    assert!(
        err.to_string().contains("main worktree cannot be removed"),
        "the invariant, rather than a later Git accident, refuses it: {err}"
    );
    assert!(repo.is_dir());
}

#[cfg(unix)]
#[test]
fn non_utf8_dirty_paths_are_distinct_and_content_changes_invalidate_the_plan() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    let one = target.join(OsString::from_vec(b"odd-\xfe".to_vec()));
    let two = target.join(OsString::from_vec(b"odd-\xff".to_vec()));
    if std::fs::write(&one, "one\n").is_err() || std::fs::write(&two, "two\n").is_err() {
        return;
    }

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert_eq!(plan.dirty_total, 2);
    assert_eq!(
        plan.dirty_files[0], plan.dirty_files[1],
        "the display spelling is intentionally lossy"
    );

    std::fs::write(&two, "changed\n").unwrap();
    let err = forced_removal_refused(&git_bin, &repo, plan);
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(one.exists() && two.exists(), "and destroyed neither file");
}

/// The listed files are capped at fifty; the authorisation is not.
///
/// A worktree with sixty uncommitted files shows fifty names and a total. Swap
/// the fifty-fifth for a new one and every field the dialog rendered is
/// identical — same total, same fifty names — while the work a forced removal
/// would destroy is different work. The capped list is for reading; the
/// fingerprint is uncapped precisely so this cannot pass.
#[test]
fn replacing_an_unlisted_dirty_file_invalidates_the_approval() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");

    for n in 0..60 {
        std::fs::write(target.join(format!("f{n:02}.txt")), "original\n").unwrap();
    }

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert_eq!(plan.dirty_total, 60);
    assert_eq!(plan.dirty_files.len(), 50, "the display list is capped");
    assert!(
        !plan.dirty_files.iter().any(|name| name == "f55.txt"),
        "the file about to be swapped was never shown"
    );

    // Same count, same fifty names, different work.
    std::fs::remove_file(target.join("f55.txt")).unwrap();
    std::fs::write(target.join("f60.txt"), "never seen\n").unwrap();

    let later = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("re-plan")
        .remove(0);
    assert_eq!(
        (later.dirty_total, &later.dirty_files),
        (plan.dirty_total, &plan.dirty_files),
        "nothing the dialog renders has moved"
    );

    let err = forced_removal_refused(&git_bin, &repo, plan);
    let message = err.to_string();
    assert!(message.contains(PLAN_CHANGED_MARKER), "got {message}");
    assert!(
        message.contains("f55.txt") || message.contains("f60.txt"),
        "the refusal names what moved: {message}"
    );
    assert!(target.join("f60.txt").exists(), "and destroyed nothing");
}

/// The same name holding different bytes is different work.
///
/// Status codes, counts, and names are all identical when a file is rewritten
/// in place. Only the content identity moves — which is why the fingerprint
/// carries git's own object name for the bytes that are there now.
#[test]
fn rewriting_a_listed_dirty_file_invalidates_the_approval() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    let file = target.join("notes.txt");
    std::fs::write(&file, "the version the user was shown\n").unwrap();

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert_eq!(plan.dirty_files, vec!["notes.txt".to_string()]);

    std::fs::write(&file, "an hour of work the user never saw\n").unwrap();

    let later = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("re-plan")
        .remove(0);
    assert_eq!(
        (later.dirty_total, &later.dirty_files),
        (plan.dirty_total, &plan.dirty_files),
        "nothing the dialog renders has moved"
    );

    let err = forced_removal_refused(&git_bin, &repo, plan);
    let message = err.to_string();
    assert!(message.contains(PLAN_CHANGED_MARKER), "got {message}");
    assert!(message.contains("notes.txt"), "got {message}");
    assert!(file.exists(), "and destroyed nothing");
}

/// A commit amended under an approved plan is a commit nobody approved losing.
///
/// The count of unpushed commits does not move when one is amended or reset and
/// rewritten, so a plan built from counts reads exactly the same over entirely
/// different commits. The checked-out commit and the branch's ref both move,
/// and both are in the fingerprint.
#[test]
fn amending_a_commit_under_an_approved_plan_invalidates_it() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert_eq!(plan.dirty_total, 0);
    assert_eq!(
        plan.unpushed_commits, 0,
        "no upstream, so nothing counts up"
    );

    std::fs::write(target.join("unfinished.txt"), "rewritten entirely\n").unwrap();
    git(&target, &["add", "-A"]);
    git(
        &target,
        &["commit", "-q", "--amend", "-m", "different work"],
    );

    let later = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("re-plan")
        .remove(0);
    assert_eq!(
        (
            later.dirty_total,
            &later.dirty_files,
            later.unpushed_commits,
            later.requires_force
        ),
        (
            plan.dirty_total,
            &plan.dirty_files,
            plan.unpushed_commits,
            plan.requires_force
        ),
        "nothing the dialog renders has moved"
    );
    let (before, after) = (
        plan.state
            .evidence()
            .expect("plans built here carry evidence"),
        later
            .state
            .evidence()
            .expect("plans built here carry evidence"),
    );
    assert_ne!(
        after.head, before.head,
        "but the commit it has checked out has"
    );
    assert_ne!(
        after.branch_oid, before.branch_oid,
        "and so has the branch's ref"
    );
    assert_ne!(
        later.state.digest, plan.state.digest,
        "and the digest that crosses to the frontend moved with them"
    );

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![(plan, RemoveOptions::default())]),
    )
    .expect_err("the worktree holds a commit that was never approved for deletion");
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(target.is_dir(), "and destroyed nothing");
}

/// A file outside git, replaced under the same name, is different work.
///
/// `.env` files are the ones git cannot bring back, which is why they are
/// listed at all. The list is only names, so overwriting one is invisible to
/// every field the dialog shows.
#[test]
fn rewriting_a_file_outside_git_invalidates_the_approval() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-dirty");
    let env = target.join(".env");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert!(plan.env_files.iter().any(|name| name == ".env"));

    std::fs::write(&env, "SECRET=rotated\n").unwrap();

    let err = forced_removal_refused(&git_bin, &repo, plan);
    let message = err.to_string();
    assert!(message.contains(PLAN_CHANGED_MARKER), "got {message}");
    assert!(message.contains(".env"), "got {message}");
    assert!(env.exists(), "and destroyed nothing");
}

/// An approval carries the exact state or it is not an approval.
///
/// A payload from a build that predates the fingerprint deserialises — the
/// field defaults — and then fails closed: an empty digest is not the digest of
/// any real worktree, so nothing is deleted on the strength of it.
#[test]
fn a_plan_without_a_state_fingerprint_authorises_nothing() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");

    let mut plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    plan.state = Default::default();

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![(plan, RemoveOptions::default())]),
    )
    .expect_err("an approval with no proven state is not an approval");
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(target.is_dir(), "and destroyed nothing");
}

/// A nested repository is not "a directory".
///
/// Every dirty path that resolved to a directory answered with the same token,
/// so a submodule whose entire contents were rewritten between the dialog
/// opening and the click was indistinguishable from one nobody touched — and a
/// force authorisation carried straight over it. A submodule is a git
/// repository, so its state is bounded and can be named: its checked-out
/// commit plus its own uncommitted paths.
#[test]
fn content_changing_inside_a_dirty_submodule_invalidates_the_approval() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    let nested = target.join("nested");

    std::fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q", "-b", "main", "."]);
    git(&nested, &["config", "user.email", "test@yawm.dev"]);
    git(&nested, &["config", "user.name", "yawm test"]);
    git(&nested, &["config", "commit.gpgsign", "false"]);
    std::fs::write(nested.join("inner.txt"), "before\n").unwrap();
    git(&nested, &["add", "-A"]);
    git(&nested, &["commit", "-qm", "inner"]);

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert!(
        plan.dirty_files
            .iter()
            .any(|name| name.starts_with("nested")),
        "git reports the nested repository as one uncommitted path: {:?}",
        plan.dirty_files
    );

    // Every byte of the nested repository's work is rewritten, and not one
    // field the dialog renders moves.
    std::fs::write(nested.join("inner.txt"), "entirely different work\n").unwrap();

    let later = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("re-plan")
        .remove(0);
    assert_eq!(
        (&later.dirty_files, later.dirty_total, &later.env_files),
        (&plan.dirty_files, plan.dirty_total, &plan.env_files),
        "nothing the dialog renders has moved"
    );
    assert_ne!(
        later.state.digest, plan.state.digest,
        "but the state it was approved against has"
    );

    let err = forced_removal_refused(&git_bin, &repo, plan);
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(nested.is_dir(), "and destroyed nothing");
}

fn create_clean_nested_repository(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q", "-b", "main", "."]);
    git(path, &["config", "user.email", "test@yawm.dev"]);
    git(path, &["config", "user.name", "yawm test"]);
    git(path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("owned.txt"), "nested history\n").unwrap();
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "nested commit"]);
}

#[test]
fn a_clean_nested_repository_blocks_worktree_deletion() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    let nested = target.join("clean-nested");
    create_clean_nested_repository(&nested);

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert!(
        plan.state.unproven,
        "planning must reject a nested object database even when it is clean"
    );

    let err = forced_removal_refused(&git_bin, &repo, plan);
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(
        nested.join(".git").is_dir(),
        "the nested repository survives"
    );
}

#[test]
fn an_ignored_nested_repository_blocks_worktree_deletion() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    std::fs::write(target.join(".gitignore"), ".env*\nignored-nested/\n").unwrap();
    git(&target, &["add", ".gitignore"]);
    git(&target, &["commit", "-qm", "ignore nested fixture"]);
    let nested = target.join("ignored-nested");
    create_clean_nested_repository(&nested);

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert_eq!(
        plan.dirty_total, 0,
        "Git status omits the ignored repository, so the independent boundary scan is required"
    );
    assert!(plan.state.unproven);

    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![(plan, RemoveOptions::default())]),
    )
    .expect_err("an ignored nested repository is not authorised for deletion");
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(
        nested.join(".git").is_dir(),
        "the nested repository survives"
    );
}

#[test]
fn a_nested_repository_introduced_after_approval_invalidates_the_plan() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    std::fs::write(target.join(".gitignore"), ".env*\nlater-nested/\n").unwrap();
    git(&target, &["add", ".gitignore"]);
    git(&target, &["commit", "-qm", "ignore future nested fixture"]);

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert!(
        plan.state.is_proven(),
        "the approved worktree has no boundary"
    );

    let nested = target.join("later-nested");
    create_clean_nested_repository(&nested);
    let err = remove_all(
        &git_bin,
        &repo,
        &requests(vec![(plan, RemoveOptions::default())]),
    )
    .expect_err("revalidation must discover the new ignored repository");
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(
        nested.join(".git").is_dir(),
        "the nested repository survives"
    );
}

/// A directory yawm cannot bound is not a state anyone can approve.
///
/// A dirty path that is a plain directory has no bound at all — no depth of
/// walking gives one — so it is declared unproven and the removal fails closed
/// rather than being called equal to itself on the strength of the word
/// "directory".
#[test]
fn a_dirty_path_that_is_an_unbounded_directory_proves_nothing() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    let opaque = target.join("opaque");

    // A directory git names as one uncommitted path, without being a
    // repository: an untracked directory whose contents git declines to walk.
    std::fs::create_dir(&opaque).unwrap();
    std::fs::write(opaque.join("thing.bin"), "bytes\n").unwrap();
    std::fs::write(target.join(".gitignore"), "opaque/**\n").unwrap();
    git(&target, &["add", "-f", "opaque"]);
    std::fs::remove_file(opaque.join("thing.bin")).unwrap();
    std::fs::create_dir(opaque.join("thing.bin")).unwrap();
    std::fs::write(opaque.join("thing.bin/inner"), "bytes\n").unwrap();

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    assert!(
        plan.state.unproven,
        "a directory with no bound cannot be established; dirty was {:?}",
        plan.dirty_files
    );

    let err = forced_removal_refused(&git_bin, &repo, plan);
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(target.is_dir(), "and destroyed nothing");
}

/// A worktree whose files could not all be listed is not one anyone approved.
///
/// The dialog's case for removing a worktree includes what would be lost that
/// git does not track — the `.env` files a fresh clone will not bring back. If
/// a directory in the tree cannot be opened, that list is a sample, and the
/// worktree may hold secrets nobody was shown. So the scan says it is
/// incomplete, the state is unproven, and even a forced removal is refused
/// rather than deleting on the strength of a list that is missing entries.
#[cfg(unix)]
#[test]
fn a_worktree_with_an_unreadable_directory_proves_nothing() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-review");
    let sealed = target.join("sealed");

    std::fs::create_dir(&sealed).unwrap();
    std::fs::write(sealed.join(".env"), "SECRET=1\n").unwrap();
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = std::fs::read_dir(&sealed).is_err();

    let planned = plan_removals(&git_bin, &repo, std::slice::from_ref(&target));
    let refusal = planned.as_ref().ok().map(|plans| {
        let plan = plans[0].clone();
        (plan.state.unproven, plan)
    });
    // Restore before any assertion so the fixture can still clean itself up.
    std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o755)).unwrap();

    if !unreadable {
        return; // running as root: the directory is readable regardless
    }
    let (unproven, plan) = refusal.expect("planning still succeeds");
    assert!(
        unproven,
        "the files outside git could not all be listed, so none of it is established"
    );

    let err = forced_removal_refused(&git_bin, &repo, plan);
    assert!(err.to_string().contains(PLAN_CHANGED_MARKER), "got {err}");
    assert!(target.is_dir(), "and destroyed nothing");
}

/// A git wrapper that lets a commit land on the branch mid-deletion.
///
/// The window is real and unreachable from outside: between reading a branch's
/// ref and deleting it, an agent working in this repository can commit. It is
/// arranged here by moving the ref immediately before the deletion runs.
#[cfg(unix)]
fn git_that_moves_the_branch(root: &Path, name: &str, branch: &str) -> Git {
    git_that_moves_a_ref(
        root,
        name,
        &format!("refs/heads/{branch}"),
        "$(git rev-parse refs/heads/main)",
    )
}

/// A git wrapper that moves `reference` to `value` just before the deletion.
///
/// The deletion is a `git update-ref -z --stdin` transaction, so that
/// invocation is the hook: everything the deletion proved has been proved by
/// the time it runs, and nothing has been written yet.
#[cfg(unix)]
fn git_that_moves_a_ref(root: &Path, name: &str, reference: &str, value: &str) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"update-ref\" ] && [ \"$2\" = \"-z\" ]; then\n  \
             git update-ref {reference} \"{value}\"\nfi\n\
             exec git \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn git_that_checks_out_before_ref_delete(
    root: &Path,
    name: &str,
    worktree: &Path,
    branch: &str,
) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"update-ref\" ] && [ \"$2\" = \"-z\" ]; then\n\
               git -C '{}' checkout -q '{}' 2>/dev/null || true\n\
             fi\n\
             exec git \"$@\"\n",
            worktree.display(),
            branch
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn git_that_recreates_before_config_cleanup(root: &Path, name: &str, branch: &str) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"config\" ] && [ \"$2\" = \"--local\" ] && \
               [ \"$3\" = \"--remove-section\" ]; then\n\
               git update-ref 'refs/heads/{branch}' \"$(git rev-parse refs/heads/main)\"\n\
               git config 'branch.{branch}.description' recreated\n\
             fi\n\
             exec git \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn git_that_blocks_ref_rollback(root: &Path, name: &str, reference_lock: &Path) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"config\" ] && [ \"$2\" = \"--local\" ] && \
               [ \"$3\" = \"--remove-section\" ]; then\n\
               : > '{}'\n\
               exit 1\n\
             fi\n\
             exec git \"$@\"\n",
            reference_lock.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn git_that_blocks_config_rollback(root: &Path, name: &str, config_lock: &Path) -> Git {
    use std::os::unix::fs::PermissionsExt;

    let shim = root.join(name);
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"config\" ] && [ \"$2\" = \"--local\" ] && \
               [ \"$3\" = \"--remove-section\" ]; then\n\
               : > '{}'\n\
             fi\n\
             exec git \"$@\"\n",
            config_lock.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git::with_program(shim.to_string_lossy().into_owned())
}

/// A commit that lands on the branch after approval is not one to delete.
///
/// The old sequence read the branch's ref, compared it with the approved
/// commit, and then ran `git branch -D`, which deletes whatever the name points
/// at *now*. A commit made in that window was deleted by an authorisation that
/// named its parent. The deletion is a single atomic ref update against the
/// approved commit instead: git itself refuses unless the ref still holds it,
/// so there is no interval to lose the race in.
#[cfg(unix)]
#[test]
fn a_branch_that_moved_after_approval_is_not_deleted() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-merged");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    let branch = plan
        .branch
        .clone()
        .expect("the fixture worktree has a branch");

    let outcomes = remove_all(
        &git_that_moves_the_branch(&f.root, "git-move-branch", &branch),
        &repo,
        &requests(vec![(
            plan,
            RemoveOptions {
                delete_branch: true,
                force_branch: true,
                ..Default::default()
            },
        )]),
    )
    .expect("the worktree removal itself succeeds");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].branch,
        BranchOutcome::Moved,
        "the ref is not the commit the deletion was authorised for"
    );
    assert!(!target.exists(), "the worktree still went");

    let branches = yawm_core::git::collect::load_branches(&git_bin, &repo).unwrap();
    assert!(
        branches.contains_key(&branch),
        "and the commit that landed in the window is still reachable; got {:?}",
        branches.keys().collect::<Vec<_>>()
    );
}

#[cfg(unix)]
#[test]
fn a_checkout_racing_branch_deletion_cannot_leave_a_dangling_head() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-merged");
    let racer = f.root.join("wt-review");
    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    let branch = plan.branch.clone().expect("branch");

    let outcome = remove_reporting(
        &git_that_checks_out_before_ref_delete(&f.root, "git-race-checkout", &racer, &branch),
        &repo,
        &plan,
        RemoveOptions {
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect("worktree removal succeeds");

    assert_eq!(outcome.branch, BranchOutcome::Deleted);
    let head = Command::new("git")
        .current_dir(&racer)
        .args(["symbolic-ref", "HEAD"])
        .output()
        .expect("symbolic-ref");
    assert!(head.status.success());
    assert_ne!(
        String::from_utf8_lossy(&head.stdout).trim(),
        format!("refs/heads/{branch}"),
        "checkout uses HEAD.lock too and cannot cross the deletion"
    );
    assert!(
        !yawm_core::git::collect::load_branches(&git_bin, &repo)
            .unwrap()
            .contains_key(&branch)
    );
}

#[cfg(unix)]
#[test]
fn config_cleanup_does_not_delete_a_concurrently_recreated_branch_section() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-merged");
    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    let branch = plan.branch.clone().expect("branch");
    git(
        &repo,
        &["config", &format!("branch.{branch}.description"), "old"],
    );

    let outcome = remove_reporting(
        &git_that_recreates_before_config_cleanup(&f.root, "git-recreate-branch", &branch),
        &repo,
        &plan,
        RemoveOptions {
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect("worktree removal succeeds");

    assert_eq!(
        outcome.branch,
        BranchOutcome::Moved,
        "the approved incarnation went, but the name was recreated"
    );
    assert_eq!(
        branch_at(&repo, &branch),
        branch_at(&repo, "main"),
        "the recreated branch survives"
    );
    let description = Command::new("git")
        .current_dir(&repo)
        .args(["config", "--get", &format!("branch.{branch}.description")])
        .output()
        .expect("git config");
    assert!(description.status.success());
    assert_eq!(
        String::from_utf8_lossy(&description.stdout).trim(),
        "recreated",
        "cleanup must not remove the recreated branch's config"
    );
}

#[cfg(unix)]
#[test]
fn a_ref_rollback_lock_reports_partial_removal_instead_of_kept_success() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-merged");
    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    let branch = plan.branch.clone().expect("branch");
    git(
        &repo,
        &[
            "config",
            &format!("branch.{branch}.description"),
            "approved",
        ],
    );
    let reference_lock = repo.join(".git/refs/heads").join(format!("{branch}.lock"));
    std::fs::create_dir_all(reference_lock.parent().unwrap()).unwrap();

    let err = remove_reporting(
        &git_that_blocks_ref_rollback(&f.root, "git-lock-ref-rollback", &reference_lock),
        &repo,
        &plan,
        RemoveOptions {
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect_err("the deleted ref could not be restored");

    let yawm_core::error::Error::BatchIncomplete(partial) = &err else {
        panic!("the worktree is gone, so the failure must preserve that outcome: {err:?}");
    };
    assert_eq!(partial.completed.len(), 1);
    assert_eq!(
        partial.completed[0].status,
        RemovalStatus::RemovedButFinalizationFailed
    );
    assert_eq!(
        partial.completed[0].outcome.branch,
        BranchOutcome::RollbackFailed,
        "a failed ref rollback must never be reported as a kept branch"
    );
    let yawm_core::error::Error::BranchRollbackFailed {
        ref_may_have_changed,
        config_may_have_changed,
        ..
    } = partial.cause.as_ref()
    else {
        panic!(
            "rollback uncertainty must stay structured: {:?}",
            partial.cause
        );
    };
    assert!(*ref_may_have_changed);
    assert!(!*config_may_have_changed);
    assert!(!target.exists());

    std::fs::remove_file(&reference_lock).unwrap();
    assert!(
        !yawm_core::git::collect::load_branches(&git_bin, &repo)
            .unwrap()
            .contains_key(&branch),
        "the induced lock really did prevent ref restoration"
    );
}

#[cfg(unix)]
#[test]
fn a_config_rollback_lock_reports_partial_removal_instead_of_kept_success() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-merged");
    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    let branch = plan.branch.clone().expect("branch");
    git(
        &repo,
        &[
            "config",
            &format!("branch.{branch}.description"),
            "approved",
        ],
    );
    let config_lock = repo.join(".git/config.lock");

    let err = remove_reporting(
        &git_that_blocks_config_rollback(&f.root, "git-lock-config-rollback", &config_lock),
        &repo,
        &plan,
        RemoveOptions {
            delete_branch: true,
            ..Default::default()
        },
    )
    .expect_err("the isolated config section could not be restored");

    let yawm_core::error::Error::BatchIncomplete(partial) = &err else {
        panic!("the worktree is gone, so the failure must preserve that outcome: {err:?}");
    };
    assert_eq!(partial.completed.len(), 1);
    assert_eq!(
        partial.completed[0].status,
        RemovalStatus::RemovedButFinalizationFailed
    );
    let yawm_core::error::Error::BranchRollbackFailed {
        ref_may_have_changed,
        config_may_have_changed,
        ..
    } = partial.cause.as_ref()
    else {
        panic!(
            "rollback uncertainty must stay structured: {:?}",
            partial.cause
        );
    };
    assert!(!*ref_may_have_changed);
    assert!(*config_may_have_changed);
    assert!(!target.exists());
    assert_eq!(
        branch_at(&repo, &branch),
        plan.state
            .evidence()
            .and_then(|evidence| evidence.branch_oid.clone())
            .unwrap(),
        "the ref rollback completed even though config restoration did not"
    );
    std::fs::remove_file(config_lock).unwrap();
}

/// The proof that a branch is merged names a ref, and that ref can move too.
///
/// An unforced deletion answers git's own `branch -d` question first: is this
/// branch's commit an ancestor of what it would be merged into? That answer is
/// about a fixed pair of object names, but the ref it was read from — the
/// upstream, or the ref HEAD resolves to — can be rewritten in the interval
/// before the deletion runs. A reset, a force-push landing in a fetch, an
/// amended merge commit: after any of them the branch is a branch nobody proved
/// was merged, and deleting it on the strength of the old answer discards the
/// only copy of its commits.
///
/// So the merge reference is part of the same transaction as the branch. Here
/// it is moved after the ancestry test and before the transaction, which is
/// exactly the window, and the branch survives.
#[cfg(unix)]
#[test]
fn a_merge_reference_that_moved_after_the_proof_stops_the_deletion() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let target = f.root.join("wt-merged");

    let plan = plan_removals(&git_bin, &repo, std::slice::from_ref(&target))
        .expect("plan")
        .remove(0);
    let branch = plan
        .branch
        .clone()
        .expect("the fixture worktree has a branch");
    let branch_oid = branch_at(&repo, &branch);
    // The branch has no upstream, so `git branch -d` measures it against the
    // ref this repository's HEAD resolves to. That is the ref moved below.
    assert_eq!(
        plan.state
            .evidence()
            .and_then(|evidence| evidence.merge_ref.clone()),
        Some("refs/heads/main".to_string()),
        "the evidence names the ref the deletion is decided against"
    );

    let outcomes = remove_all(
        &git_that_moves_a_ref(
            &f.root,
            "git-move-merge-ref",
            "refs/heads/main",
            "$(git rev-parse refs/heads/main^)",
        ),
        &repo,
        &requests(vec![(
            plan,
            RemoveOptions {
                delete_branch: true,
                ..Default::default()
            },
        )]),
    )
    .expect("the worktree removal itself succeeds");

    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].branch,
        BranchOutcome::Kept,
        "what proved the branch merged is no longer what the repository says"
    );
    assert!(!target.exists(), "the worktree still went");

    let branches = yawm_core::git::collect::load_branches(&git_bin, &repo).unwrap();
    assert!(
        branches.contains_key(&branch),
        "the branch is untouched; got {:?}",
        branches.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        branch_at(&repo, &branch),
        branch_oid,
        "and still at the commit it held"
    );
}

/// The commit a branch's ref currently holds.
#[cfg(unix)]
fn branch_at(repo: &Path, branch: &str) -> String {
    let out = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .output()
        .expect("run git");
    assert!(out.status.success(), "{branch} should still exist");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Nothing yawm removed, and part of the selection gone anyway.
///
/// This used to collapse into a `Parse` error carrying prose, so the frontend
/// read it as a generic failure and went on listing directories that are not
/// there, with their tabs open. The gone paths cross as paths, in a failure the
/// dialog can reconcile, while still claiming no removals.
#[cfg(unix)]
#[test]
fn a_batch_that_removed_nothing_still_names_what_vanished() {
    let f = Fixture::build();
    let git_bin = Git::new();
    let repo = f.repo();
    let refused = f.root.join("wt-review");
    let vanished = f.root.join("wt-merged");

    let plans = plan_removals(&git_bin, &repo, &[refused.clone(), vanished.clone()]).expect("plan");

    let err = remove_all_interrupted(
        &failing_git(&f.root, "git-no-remove-batch", &["remove"]),
        &repo,
        &requests(vec![
            (plans[0].clone(), RemoveOptions::default()),
            (plans[1].clone(), RemoveOptions::default()),
        ]),
        // Every plan has been validated and nothing has been mutated yet.
        // Something outside yawm takes the second worktree away here.
        &mut || {
            git(&repo, &["worktree", "remove", "--force", "../wt-merged"]);
        },
    )
    .expect_err("the first removal is refused and the second is no longer there");

    let yawm_core::error::Error::BatchVanished(report) = &err else {
        panic!("a worktree is gone, so this cannot be a bare refusal: {err:?}");
    };
    assert_eq!(report.vanished.len(), 1, "got {:?}", report.vanished);
    assert_eq!(path_key(&report.vanished[0]), path_key(&vanished));
    assert_eq!(path_key(&report.failed), path_key(&refused));
    assert!(
        !err.to_string().contains(PLAN_CHANGED_MARKER),
        "the plans were all still valid; the removal was refused: {err}"
    );
    assert!(
        err.to_string().contains("shim refused"),
        "the real failure is still reported: {err}"
    );
    assert!(refused.is_dir(), "and the refused one is untouched");
}
