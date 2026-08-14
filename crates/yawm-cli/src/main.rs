//! `yawm` — the command line frontend.
//!
//! This binary exists as much as a structural guarantee as a feature: it is
//! built from the same `yawm-core` crate as the desktop app and links nothing
//! from the GUI. If logic ever leaks into the Tauri shell, this stops
//! compiling, which is why CI builds it on every platform.

use std::collections::HashSet;
use std::path::PathBuf;

use yawm_core::scan::{Discovery, UnreadableSource};
use yawm_core::{Config, Error, RepoReport, ScanOptions, Scanner};

mod render;

const HELP: &str = "\
yawm — see every git worktree, know which are disposable

USAGE:
    yawm list [PATH]...      List worktrees, grouped by repository
    yawm --help              Show this message
    yawm --version           Show the version

ARGS:
    PATH    Repository or folder to inspect. Defaults to the configured
            repositories, or the current directory if none are configured.

OPTIONS:
    --no-size      Skip disk measurement (faster)
    --no-procs     Skip live process detection
    --disposable   Show only worktrees that are safe to delete
";

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("yawm: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> yawm_core::Result<std::process::ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(std::process::ExitCode::SUCCESS);
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("yawm {}", env!("CARGO_PKG_VERSION"));
        return Ok(std::process::ExitCode::SUCCESS);
    }

    let mut options = ScanOptions::default();
    let mut disposable_only = false;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut command: Option<String> = None;

    for arg in &args {
        match arg.as_str() {
            "--no-size" => options.measure_size = false,
            "--no-procs" => options.detect_processes = false,
            "--disposable" => disposable_only = true,
            other if other.starts_with('-') => {
                eprintln!("yawm: unknown option {other}\n");
                print!("{HELP}");
                return Ok(std::process::ExitCode::FAILURE);
            }
            other if command.is_none() => command = Some(other.to_string()),
            other => paths.push(PathBuf::from(other)),
        }
    }

    match command.as_deref() {
        None | Some("list") => list(paths, options, disposable_only),
        Some(other) => {
            eprintln!("yawm: unknown command {other}\n");
            print!("{HELP}");
            Ok(std::process::ExitCode::FAILURE)
        }
    }
}

fn list(
    paths: Vec<PathBuf>,
    options: ScanOptions,
    disposable_only: bool,
) -> yawm_core::Result<std::process::ExitCode> {
    let config = Config::default_path()
        .map(|p| Config::load(&p))
        .unwrap_or_default();
    let scanner = Scanner::new(config);

    let (reports, unreadable) = if paths.is_empty() {
        let (repos, roots) = scanner.config().scoped_sources();
        if repos.is_empty() && roots.is_empty() {
            // Nothing configured yet: treat the current directory exactly like
            // an explicit input, so it may be either a repo or a scan root.
            let here = std::env::current_dir()?;
            scan_inputs(&scanner, &[here], options)?
        } else {
            let scanned = scanner.scan_all_reporting(options)?;
            (scanned.repos, scanned.unreadable)
        }
    } else {
        scan_inputs(&scanner, &paths, options)?
    };

    render::print(&reports, disposable_only, options.measure_size);
    for source in &unreadable {
        eprintln!("yawm: {}: {}", source.path.display(), source.reason.trim());
    }

    if reports.is_empty() || !unreadable.is_empty() {
        Ok(std::process::ExitCode::FAILURE)
    } else {
        Ok(std::process::ExitCode::SUCCESS)
    }
}

/// Resolve explicit paths as repositories first, then as scan roots.
///
/// Trying Git first preserves bare repositories and linked worktrees, neither
/// of which filesystem discovery can reliably identify. Resolution also maps
/// every alias to the main worktree, giving us one stable deduplication key.
fn resolve_inputs(scanner: &Scanner, paths: &[PathBuf]) -> Discovery {
    let mut result = Discovery::default();
    let mut seen = HashSet::new();

    for path in paths {
        let direct = yawm_core::scan::resolve_repositories_reporting(
            scanner.git(),
            std::slice::from_ref(path),
        );
        if !direct.repositories.is_empty() {
            append_distinct(&mut result.repositories, &mut seen, direct.repositories);
            continue;
        }

        if path.is_dir() {
            let discovered = discover_scan_root(scanner, path);
            if discovered.repositories.is_empty() && discovered.unreadable.is_empty() {
                let reason = direct
                    .unreadable
                    .first()
                    .map(|source| source.reason.as_str())
                    .unwrap_or("no repositories found");
                result.note_unreadable(
                    path,
                    format!("no repositories found beneath this folder ({reason})"),
                );
            } else {
                append_distinct(&mut result.repositories, &mut seen, discovered.repositories);
                result.absorb_failures(discovered.unreadable);
            }
        } else {
            result.absorb_failures(direct.unreadable);
        }
    }

    result
}

fn discover_scan_root(scanner: &Scanner, path: &std::path::Path) -> Discovery {
    let root = path.to_path_buf();
    yawm_core::scan::discover_reporting(
        scanner.git(),
        std::slice::from_ref(&root),
        scanner.config().scan_depth,
    )
}

fn append_distinct(
    output: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    repositories: impl IntoIterator<Item = PathBuf>,
) {
    for repository in repositories {
        if seen.insert(repository.clone()) {
            output.push(repository);
        }
    }
}

fn scan_inputs(
    scanner: &Scanner,
    paths: &[PathBuf],
    options: ScanOptions,
) -> yawm_core::Result<(Vec<RepoReport>, Vec<UnreadableSource>)> {
    let resolved = resolve_inputs(scanner, paths);
    let mut reports = Vec::with_capacity(resolved.repositories.len());
    let mut unreadable = resolved.unreadable;

    for path in resolved.repositories {
        match scanner.scan_repo(&path, options) {
            Ok(report) => reports.push(report),
            Err(Error::GitMissing) => return Err(Error::GitMissing),
            Err(error) => {
                let moved_worktree = match &error {
                    Error::MovedWorktree { diagnostic } => Some(diagnostic.clone()),
                    _ => None,
                };
                unreadable.push(UnreadableSource {
                    path,
                    reason: error.to_string(),
                    moved_worktree,
                });
            }
        }
    }

    Ok((reports, unreadable))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/yawm-cli-tests")
                .join(format!("{name}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn init_repo(path: &Path) {
        fs::create_dir_all(path).unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    fn scanner_with_ceiling(root: &TestDir) -> Scanner {
        use std::os::unix::fs::PermissionsExt;

        let checkout =
            fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let wrapper = root.0.join("git-with-ceiling");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nGIT_CEILING_DIRECTORIES='{}' exec git \"$@\"\n",
                checkout.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        Scanner::new(Config::default())
            .with_git(yawm_core::git::Git::with_program(wrapper.to_string_lossy()))
    }

    #[cfg(unix)]
    fn scanner_allowing_bare(root: &TestDir) -> Scanner {
        use std::os::unix::fs::PermissionsExt;

        let wrapper = root.0.join("git-allowing-bare");
        fs::write(
            &wrapper,
            "#!/bin/sh\nexec git -c safe.bareRepository=all \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        Scanner::new(Config::default())
            .with_git(yawm_core::git::Git::with_program(wrapper.to_string_lossy()))
    }

    #[cfg(unix)]
    #[test]
    fn explicit_folder_discovers_nested_repositories() {
        let root = TestDir::new("folder-discovery");
        init_repo(&root.0.join("team/alpha"));
        init_repo(&root.0.join("team/beta"));

        let scanner = scanner_with_ceiling(&root);
        let found = resolve_inputs(&scanner, std::slice::from_ref(&root.0));

        assert_eq!(found.repositories.len(), 2);
        assert!(found.unreadable.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_folder_keeps_readable_repositories_and_reports_unreadable_children() {
        use std::os::unix::fs::PermissionsExt;

        let root = TestDir::new("partial-folder-discovery");
        init_repo(&root.0.join("team/alpha"));
        let blocked = root.0.join("team/blocked");
        fs::create_dir_all(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&blocked).is_ok() {
            fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();
            return;
        }

        let scanner = scanner_with_ceiling(&root);
        let scanned = scan_inputs(
            &scanner,
            std::slice::from_ref(&root.0),
            ScanOptions {
                measure_size: false,
                detect_processes: false,
                ..Default::default()
            },
        );
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();
        let (reports, unreadable) = scanned.unwrap();

        assert_eq!(reports.len(), 1, "readable results must still be returned");
        assert!(
            unreadable.iter().any(|source| source.path == blocked),
            "{unreadable:?}"
        );
        assert!(
            !unreadable.is_empty(),
            "list exits unsuccessfully whenever discovery is partial"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_aliases_are_deduplicated_in_first_seen_order() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("alias-dedupe");
        let first = root.0.join("first");
        let second = root.0.join("second");
        init_repo(&first);
        init_repo(&second);
        let alias = root.0.join("first-alias");
        symlink(&first, &alias).unwrap();

        let scanner = Scanner::new(Config::default());
        let found = resolve_inputs(&scanner, &[first.clone(), alias, second.clone()]);

        assert_eq!(
            found.repositories,
            vec![
                fs::canonicalize(first).unwrap(),
                fs::canonicalize(second).unwrap()
            ]
        );
        assert!(found.unreadable.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn repository_dedupe_preserves_unix_path_bytes_and_separators() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let slash = PathBuf::from("team/repo");
        let backslash = PathBuf::from(r"team\repo");
        let invalid = PathBuf::from(OsString::from_vec(b"repo-\xff".to_vec()));
        let lossy_collision = PathBuf::from("repo-\u{fffd}");
        let mut output = Vec::new();
        let mut seen = HashSet::new();

        append_distinct(
            &mut output,
            &mut seen,
            [
                slash.clone(),
                backslash.clone(),
                invalid.clone(),
                lossy_collision.clone(),
                slash.clone(),
            ],
        );

        assert_eq!(
            output,
            vec![slash, backslash, invalid, lossy_collision],
            "canonical PathBuf identity is lossless on Unix"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bare_and_linked_worktree_inputs_resolve_to_their_main_repository() {
        let root = TestDir::new("git-admin-inputs");
        let main = root.0.join("main");
        init_repo(&main);
        for args in [
            vec![
                "-C",
                main.to_str().unwrap(),
                "config",
                "user.email",
                "test@example.com",
            ],
            vec!["-C", main.to_str().unwrap(), "config", "user.name", "Test"],
            vec![
                "-C",
                main.to_str().unwrap(),
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "initial",
            ],
        ] {
            assert!(Command::new("git").args(args).status().unwrap().success());
        }
        let linked = root.0.join("linked");
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    main.to_str().unwrap(),
                    "worktree",
                    "add",
                    "--quiet",
                    "--detach",
                ])
                .arg(&linked)
                .status()
                .unwrap()
                .success()
        );

        let bare = root.0.join("bare.git");
        assert!(
            Command::new("git")
                .args(["init", "--bare", "--quiet"])
                .arg(&bare)
                .status()
                .unwrap()
                .success()
        );

        let scanner = scanner_allowing_bare(&root);
        let found = resolve_inputs(&scanner, &[linked, bare.clone()]);

        assert_eq!(found.repositories.len(), 2, "{found:?}");
        assert_eq!(found.repositories[0], fs::canonicalize(main).unwrap());
        assert_eq!(found.repositories[1], fs::canonicalize(bare).unwrap());
        assert!(found.unreadable.is_empty());
    }
}
