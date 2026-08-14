//! Exact administrative records for dependency links created by yawm.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::execution_context;
use crate::model::ManagedDependencyLink;

pub const LINKABLE_DIRS: &[&str] = &["node_modules", ".venv", "venv", "vendor"];

const MANIFESTS: &[(&str, &[&str])] = &[
    (
        "node_modules",
        &[
            "package.json",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
        ],
    ),
    (
        ".venv",
        &[
            "pyproject.toml",
            "uv.lock",
            "poetry.lock",
            "requirements.txt",
        ],
    ),
    (
        "venv",
        &[
            "pyproject.toml",
            "uv.lock",
            "poetry.lock",
            "requirements.txt",
        ],
    ),
    (
        "vendor",
        &[
            "Gemfile",
            "Gemfile.lock",
            "composer.json",
            "composer.lock",
            "go.mod",
            "go.sum",
        ],
    ),
];

const RECORD_FILE: &str = "yawm-managed-dependency-links.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedLinkRecord {
    path: String,
    target: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ManagedLinksScan {
    pub valid: Vec<ManagedDependencyLink>,
    pub invalid: Vec<String>,
    pub unproven: bool,
}

/// Persist exact links in this worktree's private administrative directory.
///
/// A record is not an ignore rule. It authorises only one named link to one
/// canonical main-worktree directory, and inspection rechecks both endpoints and
/// every dependency manifest before excluding that path from dirty work.
pub(crate) fn record_links(
    worktree: &Path,
    links: impl IntoIterator<Item = (String, PathBuf)>,
) -> bool {
    let Some(file) = record_path(worktree) else {
        return false;
    };
    let records = links
        .into_iter()
        .filter_map(|(path, target)| {
            if !LINKABLE_DIRS.contains(&path.as_str()) {
                return None;
            }
            Some(ManagedLinkRecord {
                path,
                target: canonical(&target)?,
            })
        })
        .collect::<Vec<_>>();
    if records.is_empty() {
        return true;
    }
    let Ok(bytes) = serde_json::to_vec(&records) else {
        return false;
    };
    std::fs::write(file, bytes).is_ok()
}

pub(crate) fn inspect_links(worktree: &Path, main: &Path) -> ManagedLinksScan {
    let Some(file) = record_path(worktree) else {
        return ManagedLinksScan::default();
    };
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ManagedLinksScan {
                invalid: unmanaged_dependency_links(worktree, main),
                ..ManagedLinksScan::default()
            };
        }
        Err(_) => {
            return malformed_record_scan(worktree);
        }
    };
    let records: Vec<ManagedLinkRecord> = match serde_json::from_slice(&bytes) {
        Ok(records) => records,
        Err(_) => return malformed_record_scan(worktree),
    };

    let mut scan = ManagedLinksScan::default();
    let mut seen = BTreeSet::new();
    for record in records {
        if !seen.insert(record.path.clone())
            || !LINKABLE_DIRS.contains(&record.path.as_str())
            || !valid_link(worktree, main, &record)
        {
            scan.invalid.push(record.path);
            continue;
        }
        scan.valid.push(ManagedDependencyLink {
            path: record.path,
            target: record.target,
        });
    }
    scan.valid.sort_by(|left, right| left.path.cmp(&right.path));
    scan.invalid.sort();
    scan.invalid.dedup();
    scan
}

fn malformed_record_scan(worktree: &Path) -> ManagedLinksScan {
    ManagedLinksScan {
        valid: Vec::new(),
        invalid: LINKABLE_DIRS
            .iter()
            .filter(|name| worktree.join(name).exists())
            .map(|name| (*name).to_string())
            .collect(),
        unproven: true,
    }
}

fn unmanaged_dependency_links(worktree: &Path, main: &Path) -> Vec<String> {
    LINKABLE_DIRS
        .iter()
        .filter(|name| {
            let destination = worktree.join(name);
            std::fs::symlink_metadata(&destination)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
                || canonical(&destination)
                    .zip(canonical(&main.join(name)))
                    .is_some_and(|(destination, target)| destination == target)
        })
        .map(|name| (*name).to_string())
        .collect()
}

fn valid_link(worktree: &Path, main: &Path, record: &ManagedLinkRecord) -> bool {
    let destination = worktree.join(&record.path);
    let main_target = main.join(&record.path);
    let (Some(destination), Some(main_target), Some(recorded_target)) = (
        canonical(&destination),
        canonical(&main_target),
        canonical(&record.target),
    ) else {
        return false;
    };
    if destination != main_target || recorded_target != main_target || !main_target.is_dir() {
        return false;
    }
    manifests_match(worktree, main, &record.path)
}

fn manifests_match(worktree: &Path, main: &Path, directory: &str) -> bool {
    let Some((_, manifests)) = MANIFESTS.iter().find(|(name, _)| *name == directory) else {
        return false;
    };
    manifests.iter().all(|manifest| {
        let left = worktree.join(manifest);
        let right = main.join(manifest);
        match (std::fs::read(left), std::fs::read(right)) {
            (Ok(left), Ok(right)) => left == right,
            (Err(left), Err(right))
                if left.kind() == std::io::ErrorKind::NotFound
                    && right.kind() == std::io::ErrorKind::NotFound =>
            {
                true
            }
            _ => false,
        }
    })
}

fn record_path(worktree: &Path) -> Option<PathBuf> {
    let context = execution_context(worktree)?;
    Some(context.git_dir.join(RECORD_FILE))
}

fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}
