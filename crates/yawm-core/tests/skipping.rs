//! What the scanner is allowed to skip, and what it must never lose by
//! skipping.
//!
//! Hidden main worktrees were 18.7 GB of the 21.0 GB walked on the machine
//! this was measured on, for rows that are then discarded before they are
//! drawn. Skipping them is only safe because a main worktree can never be
//! deleted, so its size never reaches a total anyone is shown — these pin both
//! halves of that claim.

use std::path::Path;

use yawm_core::{Config, ScanOptions, Scanner};

fn git(dir: &Path, args: &[&str]) {
    let success = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .expect("git")
        .success();
    assert!(success, "git {args:?} failed");
}

/// A repository with a main worktree and one linked worktree, each holding a
/// file large enough that a measured size cannot be confused with an empty one.
fn fixture(temp: &Path) -> std::path::PathBuf {
    let root = temp.join("main");
    let linked = temp.join("linked");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main", "."]);
    git(&root, &["config", "user.email", "test@yawm.dev"]);
    git(&root, &["config", "user.name", "yawm test"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "init"]);

    let linked_arg = linked.to_string_lossy();
    git(
        &root,
        &["worktree", "add", "-q", "-b", "feature", &linked_arg],
    );

    std::fs::write(root.join("big.bin"), vec![b'm'; 64 * 1024]).unwrap();
    std::fs::write(linked.join("big.bin"), vec![b'l'; 8 * 1024]).unwrap();
    root
}

#[test]
fn hidden_main_worktrees_are_not_walked() {
    let temp = tempfile::tempdir().unwrap();
    let root = fixture(temp.path());

    let scanner = Scanner::new(Config::default());
    let opts = ScanOptions {
        skip_main_size: true,
        ..ScanOptions::default()
    };
    let report = scanner.scan_repo(&root, opts).unwrap();

    let main = report.worktrees.iter().find(|w| w.entry.is_main).unwrap();
    let linked = report.worktrees.iter().find(|w| !w.entry.is_main).unwrap();

    assert!(
        main.status.size.is_none(),
        "the main worktree is hidden, so walking it is time spent on a row nobody sees"
    );
    assert!(
        linked.status.size.as_ref().is_some_and(|s| s.bytes > 0),
        "the worktrees that are shown must still be measured"
    );
}

#[test]
fn skipping_is_off_unless_asked_for() {
    let temp = tempfile::tempdir().unwrap();
    let root = fixture(temp.path());

    let scanner = Scanner::new(Config::default());
    let report = scanner.scan_repo(&root, ScanOptions::default()).unwrap();

    // The CLI totals every worktree and shares this config. A skip that
    // defaulted on would make its total silently short.
    let main = report.worktrees.iter().find(|w| w.entry.is_main).unwrap();
    assert!(main.status.size.as_ref().is_some_and(|s| s.bytes > 0));
    assert!(report.total_bytes() > 64 * 1024);
}

#[test]
fn a_main_worktree_measured_before_it_was_hidden_keeps_its_size() {
    let temp = tempfile::tempdir().unwrap();
    let root = fixture(temp.path());

    // One scanner, so the size cache is the same one across both passes —
    // which is what the desktop does when the user flips the toggle.
    let scanner = Scanner::new(Config::default());
    scanner.scan_repo(&root, ScanOptions::default()).unwrap();

    let opts = ScanOptions {
        skip_main_size: true,
        ..ScanOptions::default()
    };
    let report = scanner.scan_repo(&root, opts).unwrap();
    let main = report.worktrees.iter().find(|w| w.entry.is_main).unwrap();

    assert!(
        main.status.size.as_ref().is_some_and(|s| s.bytes > 0),
        "hiding a worktree must not blank a number that was already paid for"
    );
}

#[test]
fn unhiding_measures_what_was_skipped() {
    let temp = tempfile::tempdir().unwrap();
    let root = fixture(temp.path());

    let scanner = Scanner::new(Config::default());
    let hidden = ScanOptions {
        skip_main_size: true,
        ..ScanOptions::default()
    };
    let first = scanner.scan_repo(&root, hidden).unwrap();
    assert!(
        first
            .worktrees
            .iter()
            .find(|w| w.entry.is_main)
            .unwrap()
            .status
            .size
            .is_none()
    );

    // Turning the rows back on has to fill the gap rather than leave a column
    // of dashes on worktrees that are now on screen.
    let second = scanner.scan_repo(&root, ScanOptions::default()).unwrap();
    let main = second.worktrees.iter().find(|w| w.entry.is_main).unwrap();

    assert!(main.status.size.as_ref().is_some_and(|s| s.bytes > 0));
}
