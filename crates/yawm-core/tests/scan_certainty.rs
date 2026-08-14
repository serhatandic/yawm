use std::ffi::OsStr;
use std::fs::{File, FileTimes};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use yawm_core::path::path_key;
use yawm_core::{Config, Landing, ScanOptions, Scanner, Verdict, VerdictReason, Worktree};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    squash: PathBuf,
    unlanded: PathBuf,
}

impl Fixture {
    fn build() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical tempdir");
        let root = base.join("main");
        let squash = base.join("squash");
        let unlanded = base.join("unlanded");

        std::fs::create_dir(&root).expect("repository directory");
        git(&root, &["init", "-q", "-b", "main", "."]);
        git(&root, &["config", "user.email", "test@yawm.dev"]);
        git(&root, &["config", "user.name", "yawm test"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("base.txt"), "base\n").expect("base file");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "base"]);

        let squash_arg = squash.to_string_lossy();
        git(
            &root,
            &["worktree", "add", "-q", "-b", "feature/squash", &squash_arg],
        );
        std::fs::write(squash.join("squashed.txt"), "landed\n").expect("squash file");
        git(&squash, &["add", "."]);
        git(&squash, &["commit", "-qm", "squashed change"]);
        git(&root, &["merge", "-q", "--squash", "feature/squash"]);
        git(&root, &["commit", "-qm", "land squash"]);
        std::fs::write(root.join("later.txt"), "main moved on\n").expect("later main file");
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "advance main"]);

        let unlanded_arg = unlanded.to_string_lossy();
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature/unlanded",
                &unlanded_arg,
            ],
        );
        std::fs::write(unlanded.join("unfinished.txt"), "not landed\n").expect("unlanded file");
        git(&unlanded, &["add", "."]);
        git(&unlanded, &["commit", "-qm", "unfinished change"]);

        let old = FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        for path in [
            root.join("base.txt"),
            root.join("squashed.txt"),
            root.join("later.txt"),
            squash.join("base.txt"),
            squash.join("squashed.txt"),
            unlanded.join("base.txt"),
            unlanded.join("squashed.txt"),
            unlanded.join("later.txt"),
            unlanded.join("unfinished.txt"),
        ] {
            File::options()
                .write(true)
                .open(&path)
                .unwrap_or_else(|error| panic!("open {}: {error}", path.display()))
                .set_times(old)
                .unwrap_or_else(|error| panic!("set time on {}: {error}", path.display()));
        }

        Self {
            _temp: temp,
            root,
            squash,
            unlanded,
        }
    }

    fn scan(&self, options: ScanOptions) -> yawm_core::RepoReport {
        Scanner::new(Config::default())
            .scan_repo(&self.root, options)
            .expect("scan repository")
    }

    fn worktree<'a>(&self, report: &'a yawm_core::RepoReport, path: &Path) -> &'a Worktree {
        report
            .worktrees
            .iter()
            .find(|worktree| path_key(&worktree.entry.path) == path_key(path))
            .unwrap_or_else(|| panic!("missing {}", path.display()))
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    for arg in args {
        command.arg(git_argument(OsStr::new(arg)));
    }
    let output = command
        .current_dir(cwd)
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
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

#[test]
fn skipping_size_does_not_change_merge_tree_landing_classification() {
    let fixture = Fixture::build();
    let without_size = fixture.scan(ScanOptions {
        measure_size: false,
        ..ScanOptions::default()
    });
    let baseline = fixture.scan(ScanOptions::default());

    let skipped_squash = fixture.worktree(&without_size, &fixture.squash);
    let baseline_squash = fixture.worktree(&baseline, &fixture.squash);
    assert_eq!(
        skipped_squash.status.landing,
        baseline_squash.status.landing
    );
    assert!(matches!(
        skipped_squash.status.landing,
        Landing::Landed { .. }
    ));
    assert!(skipped_squash.status.process_check_complete);
    assert_eq!(skipped_squash.verdict, Verdict::Disposable);

    let skipped_unlanded = fixture.worktree(&without_size, &fixture.unlanded);
    let baseline_unlanded = fixture.worktree(&baseline, &fixture.unlanded);
    assert_eq!(
        skipped_unlanded.status.landing,
        baseline_unlanded.status.landing
    );
    assert!(matches!(
        skipped_unlanded.status.landing,
        Landing::AddsContent { .. }
    ));
    assert_eq!(skipped_unlanded.verdict, Verdict::Keep);
}

#[test]
fn skipping_processes_keeps_landing_semantics_but_blocks_disposal() {
    let fixture = Fixture::build();
    let without_processes = fixture.scan(ScanOptions {
        detect_processes: false,
        ..ScanOptions::default()
    });
    let baseline = fixture.scan(ScanOptions::default());

    let skipped_squash = fixture.worktree(&without_processes, &fixture.squash);
    let baseline_squash = fixture.worktree(&baseline, &fixture.squash);
    assert_eq!(
        skipped_squash.status.landing,
        baseline_squash.status.landing
    );
    assert!(matches!(
        skipped_squash.status.landing,
        Landing::Landed { .. }
    ));
    assert!(!skipped_squash.status.process_check_complete);
    assert_eq!(skipped_squash.verdict, Verdict::Review);
    assert_eq!(skipped_squash.reason, VerdictReason::ProcessCheckSkipped);

    let skipped_unlanded = fixture.worktree(&without_processes, &fixture.unlanded);
    let baseline_unlanded = fixture.worktree(&baseline, &fixture.unlanded);
    assert_eq!(
        skipped_unlanded.status.landing,
        baseline_unlanded.status.landing
    );
    assert!(matches!(
        skipped_unlanded.status.landing,
        Landing::AddsContent { .. }
    ));
    assert_eq!(skipped_unlanded.verdict, Verdict::Keep);
    assert!(matches!(
        skipped_unlanded.reason,
        VerdictReason::DefaultBranchLacksCommittedContent { .. }
    ));
}

#[test]
fn fast_scans_leave_process_and_merge_tree_checks_incomplete() {
    let fixture = Fixture::build();
    let report = fixture.scan(ScanOptions::fast());
    let squash = fixture.worktree(&report, &fixture.squash);

    assert!(!squash.status.process_check_complete);
    assert!(!squash.status.landing_complete);
    assert!(matches!(squash.status.landing, Landing::Unknown { .. }));
    assert_eq!(squash.verdict, Verdict::Review);
}
