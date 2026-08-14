//! Path normalization.
//!
//! Worktree paths arrive from three different places that disagree on spelling:
//! git (which emits forward slashes even on Windows), the operating system's
//! process table, and yawm's own config file. Comparing them naively causes
//! worktrees to be double-listed or to never match a running process.
//!
//! [`path_key`] produces one canonical comparison key. Case is folded on
//! Windows and macOS, whose filesystems are case-insensitive by default, but
//! *not* on Linux, where `Foo` and `foo` are genuinely different directories.

use std::path::{Path, PathBuf};

/// True when the target platform's filesystem is case-insensitive by default.
pub const CASE_INSENSITIVE_FS: bool = cfg!(any(target_os = "windows", target_os = "macos"));

/// Canonical comparison key for a path.
///
/// This is for *comparison only* — never use the result to touch the
/// filesystem, since case folding is lossy.
pub fn path_key(path: impl AsRef<Path>) -> String {
    let raw = path.as_ref().to_string_lossy();

    // Git reports Windows paths with forward slashes; the OS reports backslashes.
    let unified = raw.replace('\\', "/");

    // Windows canonical paths carry a namespace prefix while Git and the
    // process table generally do not. Without removing it, a configured or
    // canonical worktree at `\\?\C:\code` never matches a process cwd reported
    // as `C:\code`, so Windows silently loses the live-process safety signal.
    //
    // Verbatim UNC paths use `\\?\UNC\server\share`; convert those back to the
    // ordinary two-leading-separator spelling rather than merely stripping the
    // prefix and turning `UNC` into a directory name.
    let unified = if let Some(rest) = unified.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = unified.strip_prefix("//?/") {
        rest.to_string()
    } else {
        unified
    };

    // Collapse repeated separators (`a//b`) without disturbing a leading `//`
    // on an ordinary UNC path.
    let mut out = String::with_capacity(unified.len());
    let mut prev_sep = false;
    for ch in unified.chars() {
        let is_sep = ch == '/';
        let second_leading_separator = is_sep && prev_sep && out == "/";
        if is_sep && prev_sep && !second_leading_separator {
            continue;
        }
        prev_sep = is_sep;
        out.push(ch);
    }

    // Drop a trailing separator so `/a/b` and `/a/b/` agree, but keep a bare root.
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }

    // Preserve a Windows drive root: `C:` should stay `C:/`.
    if out.len() == 2 && out.ends_with(':') {
        out.push('/');
    }

    if CASE_INSENSITIVE_FS {
        out.to_lowercase()
    } else {
        out
    }
}

/// True when `candidate` is `base` or lives beneath it.
///
/// Used to decide whether a running process is "inside" a worktree, and to warn
/// when a new worktree would be nested inside its own repository.
pub fn is_within(base: impl AsRef<Path>, candidate: impl AsRef<Path>) -> bool {
    let base = path_key(base);
    let candidate = path_key(candidate);

    if candidate == base {
        return true;
    }
    // Require a separator so `/a/bcd` is not treated as inside `/a/b`.
    let prefix = if base.ends_with('/') {
        base
    } else {
        format!("{base}/")
    };
    candidate.starts_with(&prefix)
}

/// Convert raw bytes from git into a path.
///
/// On Unix, filenames are arbitrary bytes and may not be valid UTF-8, so they
/// are passed through losslessly rather than being forced through `String`.
#[cfg(unix)]
pub fn bytes_to_pathbuf(bytes: &[u8]) -> PathBuf {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
pub fn bytes_to_pathbuf(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_separator_is_ignored() {
        assert_eq!(path_key("/a/b"), path_key("/a/b/"));
    }

    #[test]
    fn repeated_separators_collapse() {
        assert_eq!(path_key("/a//b"), path_key("/a/b"));
    }

    #[test]
    fn windows_and_unix_separators_agree() {
        assert_eq!(path_key(r"C:\code\repo"), path_key("C:/code/repo"));
    }

    #[test]
    fn root_is_preserved() {
        assert_eq!(path_key("/"), "/");
    }

    #[test]
    fn windows_drive_root_is_preserved() {
        // Separator spelling is portable; drive-letter case follows the host's
        // filesystem semantics. Comparing `C:` with `c:` unconditionally made
        // this Windows-specific fixture fail on Linux, where case folding is
        // deliberately disabled.
        assert_eq!(path_key(r"C:\"), path_key("C:/"));
        assert_eq!(path_key(r"C:\") == path_key("c:/"), CASE_INSENSITIVE_FS);
        assert!(path_key(r"C:\").ends_with(":/"));
    }

    #[test]
    fn windows_verbatim_paths_match_their_ordinary_spelling() {
        assert_eq!(path_key(r"\\?\C:\code\repo"), path_key(r"C:\code\repo"));
    }

    #[test]
    fn windows_verbatim_unc_paths_match_their_ordinary_spelling() {
        assert_eq!(
            path_key(r"\\?\UNC\server\share\repo"),
            path_key(r"\\server\share\repo")
        );
        assert!(path_key(r"\\server\share").starts_with("//"));
    }

    #[test]
    fn case_folding_follows_the_platform() {
        let same = path_key("/A/B") == path_key("/a/b");
        assert_eq!(same, CASE_INSENSITIVE_FS);
    }

    #[test]
    fn containment_requires_a_separator_boundary() {
        assert!(is_within("/a/b", "/a/b/c"));
        assert!(is_within("/a/b", "/a/b"));
        assert!(!is_within("/a/b", "/a/bcd"));
        assert!(!is_within("/a/b/c", "/a/b"));
    }

    #[test]
    fn containment_tolerates_trailing_separators() {
        assert!(is_within("/a/b/", "/a/b/c"));
    }
}
