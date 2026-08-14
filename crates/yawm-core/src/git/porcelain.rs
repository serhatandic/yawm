//! Parser for `git worktree list --porcelain`.
//!
//! The porcelain format is a documented, stable contract, which is why yawm
//! shells out to git rather than linking libgit2 (whose worktree API does not
//! expose the `locked` and `prunable` metadata this tool is built around).
//!
//! Records are separated by a blank line and each attribute sits on its own
//! line:
//!
//! ```text
//! worktree /path/to/main
//! HEAD 0000000000000000000000000000000000000000
//! branch refs/heads/main
//!
//! worktree /path/to/feature
//! HEAD 1111111111111111111111111111111111111111
//! detached
//! locked agent is running
//! ```
//!
//! With `-z` every line is terminated by NUL instead of a newline, which makes
//! paths containing spaces or newlines unambiguous. The parser auto-detects
//! which form it was handed so the same code serves the newline fallback used
//! for git older than 2.36.

use crate::model::{LockInfo, WorktreeEntry};
use crate::path::bytes_to_pathbuf;

/// Parse `git worktree list --porcelain` output, with or without `-z`.
pub fn parse_worktree_list(bytes: &[u8]) -> Vec<WorktreeEntry> {
    let nul_separated = bytes.contains(&0);
    let separator = if nul_separated { 0 } else { b'\n' };

    let mut entries = Vec::new();
    let mut current: Option<WorktreeEntry> = None;

    for line in bytes.split(|b| *b == separator) {
        // In newline mode git may emit CRLF on Windows.
        let line = line.strip_suffix(b"\r").unwrap_or(line);

        if line.is_empty() {
            // Blank line closes the current record.
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }

        if let Some(rest) = strip_prefix(line, b"worktree ") {
            // A new `worktree` line starts a record even if no blank line
            // preceded it, which keeps a truncated stream from merging records.
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(WorktreeEntry {
                path: bytes_to_pathbuf(rest),
                is_main: entries.is_empty(),
                ..Default::default()
            });
            continue;
        }

        let Some(entry) = current.as_mut() else {
            // Attribute with no enclosing record; ignore rather than fail, so a
            // future git version adding a preamble cannot break yawm entirely.
            continue;
        };

        if let Some(rest) = strip_prefix(line, b"HEAD ") {
            entry.head = Some(String::from_utf8_lossy(rest).into_owned());
        } else if let Some(rest) = strip_prefix(line, b"branch ") {
            let full = String::from_utf8_lossy(rest).into_owned();
            entry.branch = Some(short_branch(&full));
        } else if line == b"detached" {
            entry.detached = true;
        } else if line == b"bare" {
            entry.bare = true;
        } else if line == b"locked" {
            entry.locked = Some(LockInfo { reason: None });
        } else if let Some(rest) = strip_prefix(line, b"locked ") {
            entry.locked = Some(LockInfo {
                reason: non_empty(String::from_utf8_lossy(rest).trim()),
            });
        } else if line == b"prunable" {
            entry.prunable = Some(String::new());
        } else if let Some(rest) = strip_prefix(line, b"prunable ") {
            entry.prunable = Some(String::from_utf8_lossy(rest).trim().to_string());
        }
    }

    if let Some(entry) = current.take() {
        entries.push(entry);
    }

    entries
}

fn strip_prefix<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    line.starts_with(prefix).then(|| &line[prefix.len()..])
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// `refs/heads/feat/auth` -> `feat/auth`.
fn short_branch(refname: &str) -> String {
    refname
        .strip_prefix("refs/heads/")
        .unwrap_or(refname)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build NUL-separated output the way `--porcelain -z` does: every line is
    /// NUL-terminated, and a record break is an additional empty line.
    fn nul(records: &[&[&str]]) -> Vec<u8> {
        let mut out = Vec::new();
        for (i, record) in records.iter().enumerate() {
            if i > 0 {
                out.push(0); // blank line between records
            }
            for line in *record {
                out.extend_from_slice(line.as_bytes());
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn parses_a_simple_newline_listing() {
        let input = b"worktree /repo\nHEAD abc123\nbranch refs/heads/main\n\nworktree /repo-feat\nHEAD def456\nbranch refs/heads/feat/auth\n\n";
        let got = parse_worktree_list(input);

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path.to_str().unwrap(), "/repo");
        assert_eq!(got[0].branch.as_deref(), Some("main"));
        assert!(got[0].is_main);
        assert_eq!(got[1].branch.as_deref(), Some("feat/auth"));
        assert!(!got[1].is_main);
    }

    #[test]
    fn only_the_first_entry_is_main() {
        let input = nul(&[
            &["worktree /repo", "HEAD a", "branch refs/heads/main"],
            &["worktree /w1", "HEAD b", "branch refs/heads/one"],
            &["worktree /w2", "HEAD c", "branch refs/heads/two"],
        ]);
        let got = parse_worktree_list(&input);

        assert_eq!(got.len(), 3);
        assert!(got[0].is_main);
        assert!(!got[1].is_main);
        assert!(!got[2].is_main);
    }

    #[test]
    fn parses_detached_head() {
        let input = nul(&[&["worktree /w", "HEAD abc123", "detached"]]);
        let got = parse_worktree_list(&input);

        assert!(got[0].detached);
        assert!(got[0].branch.is_none());
        assert_eq!(got[0].head.as_deref(), Some("abc123"));
    }

    #[test]
    fn parses_bare_repository() {
        let input = nul(&[&["worktree /repo.git", "bare"]]);
        let got = parse_worktree_list(&input);

        assert!(got[0].bare);
        assert!(got[0].head.is_none());
    }

    #[test]
    fn parses_lock_with_and_without_reason() {
        let input = nul(&[
            &["worktree /a", "HEAD x", "locked"],
            &["worktree /b", "HEAD y", "locked agent is running"],
        ]);
        let got = parse_worktree_list(&input);

        assert_eq!(got[0].locked, Some(LockInfo { reason: None }));
        assert_eq!(
            got[1].locked,
            Some(LockInfo {
                reason: Some("agent is running".into())
            })
        );
    }

    #[test]
    fn parses_prunable_reason() {
        let input = nul(&[&[
            "worktree /gone",
            "HEAD z",
            "prunable gitdir file points to non-existent location",
        ]]);
        let got = parse_worktree_list(&input);

        assert_eq!(
            got[0].prunable.as_deref(),
            Some("gitdir file points to non-existent location")
        );
    }

    #[test]
    fn handles_paths_containing_spaces() {
        let input = nul(&[&[
            "worktree /Users/me/My Code/repo one",
            "HEAD abc",
            "branch refs/heads/main",
        ]]);
        let got = parse_worktree_list(&input);

        assert_eq!(got[0].path.to_str().unwrap(), "/Users/me/My Code/repo one");
    }

    /// The reason `-z` exists: a newline inside a path would otherwise be
    /// indistinguishable from a record break.
    #[test]
    fn handles_paths_containing_newlines_in_nul_mode() {
        let input = nul(&[
            &[
                "worktree /weird\nname",
                "HEAD abc",
                "branch refs/heads/main",
            ],
            &["worktree /normal", "HEAD def", "branch refs/heads/other"],
        ]);
        let got = parse_worktree_list(&input);

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].path.to_str().unwrap(), "/weird\nname");
        assert_eq!(got[1].path.to_str().unwrap(), "/normal");
    }

    #[test]
    fn tolerates_crlf_line_endings() {
        let input = b"worktree /repo\r\nHEAD abc\r\nbranch refs/heads/main\r\n\r\n";
        let got = parse_worktree_list(input);

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].branch.as_deref(), Some("main"));
        assert_eq!(got[0].head.as_deref(), Some("abc"));
    }

    #[test]
    fn tolerates_missing_trailing_blank_line() {
        let input = b"worktree /repo\nHEAD abc\nbranch refs/heads/main";
        let got = parse_worktree_list(input);

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(parse_worktree_list(b"").is_empty());
    }

    #[test]
    fn unknown_attributes_are_ignored() {
        let input = nul(&[&[
            "worktree /repo",
            "HEAD abc",
            "branch refs/heads/main",
            "somethingnew value",
        ]]);
        let got = parse_worktree_list(&input);

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn branch_names_containing_slashes_are_preserved() {
        let input = nul(&[&["worktree /w", "HEAD a", "branch refs/heads/user/feat/x"]]);
        let got = parse_worktree_list(&input);

        assert_eq!(got[0].branch.as_deref(), Some("user/feat/x"));
    }
}
