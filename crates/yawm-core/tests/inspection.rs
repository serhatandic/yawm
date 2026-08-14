use std::path::Path;

use yawm_core::{Config, Landing, ScanOptions, Scanner, UnknownReason};

fn git(dir: &Path, args: &[&str]) {
    let success = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("git")
        .success();
    assert!(success, "git {args:?} failed");
}

#[test]
fn background_pass_resolves_a_worktree_that_policy_would_have_deferred() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("main");
    let linked = temp.path().join("linked");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main", "."]);
    git(&root, &["config", "user.email", "test@yawm.dev"]);
    git(&root, &["config", "user.name", "yawm test"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join(".gitignore"), ".env*\n").unwrap();
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "init"]);

    let linked_arg = linked.to_string_lossy();
    git(
        &root,
        &["worktree", "add", "-q", "-b", "feature", &linked_arg],
    );
    std::fs::write(linked.join("feature.txt"), "feature\n").unwrap();
    git(&linked, &["add", "feature.txt"]);
    git(&linked, &["commit", "-qm", "feature"]);
    std::fs::write(root.join("main.txt"), "main\n").unwrap();
    git(&root, &["add", "main.txt"]);
    git(&root, &["commit", "-qm", "advance main"]);
    std::fs::write(linked.join(".env"), "UNIQUE=1\n").unwrap();

    let scanner = Scanner::new(Config::default());
    let report = scanner.scan_repo(&root, ScanOptions::default()).unwrap();
    let initial = report
        .worktrees
        .iter()
        .find(|worktree| worktree.entry.branch.as_deref() == Some("feature"))
        .unwrap();
    let linked = initial.entry.path.clone();
    assert_eq!(initial.status.env_files, [".env"]);
    assert!(
        matches!(
            initial.status.landing,
            Landing::Unknown {
                reason: UnknownReason::CheckDeferred,
                ..
            }
        ),
        "got {:?}",
        initial.status.landing
    );
    assert!(!initial.status.landing_complete);
    assert!(initial.status.process_check_complete);

    let inspected = scanner.resolve_worktree_landing(&root, &linked).unwrap();
    assert!(
        !matches!(
            inspected.status.landing,
            Landing::Unknown {
                reason: UnknownReason::CheckDeferred,
                ..
            }
        ),
        "the full-list pass must bypass the regular-scan gate"
    );
    assert!(inspected.status.landing_complete);
    assert!(inspected.status.process_check_complete);

    let selected = scanner.inspect_worktree(&root, &linked).unwrap();
    assert!(selected.status.process_check_complete);
    assert_eq!(selected.status.landing, inspected.status.landing);
    assert_eq!(selected.verdict, inspected.verdict);
}
