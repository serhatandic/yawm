//! Invoking git.
//!
//! Every call goes through [`Git::run`], which uses an argv array rather than a
//! shell. Paths therefore never undergo word splitting or glob expansion, which
//! is what keeps directory names containing spaces, quotes, or `$` safe.

pub mod collect;
pub mod landing;
pub(crate) mod managed;
pub mod porcelain;
pub mod refs;
pub mod status;

use crate::error::{Error, Result};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const STREAM_BUFFER_BYTES: usize = 16 * 1024;
const MAX_GIT_STDERR_BYTES: usize = 256 * 1024;

/// `git worktree list -z` was added in git 2.36. Below that yawm falls back to
/// newline parsing, which is fine unless a path contains a newline.
pub const MIN_VERSION_FOR_NUL: (u32, u32) = (2, 36);

/// A resolved git executable.
#[derive(Debug, Clone)]
pub struct Git {
    program: String,
    version: Option<(u32, u32)>,
}

/// Raw command outcome for git operations whose exact exit code is meaningful.
#[derive(Debug)]
pub(crate) struct GitOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamControl {
    Continue,
    Saturated,
}

/// Administrative and working-tree paths implied by a selected repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitExecutionContext {
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
    pub work_tree: Option<PathBuf>,
    explicit: bool,
}

impl Default for Git {
    fn default() -> Self {
        Self::new()
    }
}

impl Git {
    pub fn new() -> Self {
        Self {
            program: "git".to_string(),
            version: None,
        }
    }

    /// Use a specific git executable, for tests or unusual installations.
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            version: None,
        }
    }

    /// Detect and cache the git version, erroring if git is absent.
    pub fn detect_version(&mut self) -> Result<(u32, u32)> {
        if let Some(v) = self.version {
            return Ok(v);
        }
        let out = self.run(Path::new("."), &["--version"])?;
        let text = String::from_utf8_lossy(&out);
        let v = parse_version(&text)
            .ok_or_else(|| Error::Parse(format!("unrecognised git version: {text:?}")))?;
        self.version = Some(v);
        Ok(v)
    }

    /// Whether `-z` may be used with `worktree list`.
    pub fn supports_nul_worktree_list(&self) -> bool {
        self.version.is_none_or(|v| v >= MIN_VERSION_FOR_NUL)
    }

    /// Run git and return stdout, failing if the exit status is non-zero.
    pub fn run<S: AsRef<OsStr>>(&self, cwd: &Path, args: &[S]) -> Result<Vec<u8>> {
        let output = self.output(cwd, args)?;
        if !output.status.success() {
            return Err(Error::GitFailed {
                args: args
                    .iter()
                    .map(|a| a.as_ref().to_string_lossy().into_owned())
                    .collect(),
                cwd: cwd.to_path_buf(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(output.stdout)
    }

    /// Run git and report success separately from output.
    ///
    /// Needed for commands whose failure is meaningful rather than exceptional,
    /// such as `merge-base --is-ancestor`, which exits 1 to mean "no".
    pub fn run_checked<S: AsRef<OsStr>>(&self, cwd: &Path, args: &[S]) -> Result<(bool, Vec<u8>)> {
        let output = self.output(cwd, args)?;
        Ok((output.status.success(), output.stdout))
    }

    pub(crate) fn run_status<S: AsRef<OsStr>>(&self, cwd: &Path, args: &[S]) -> Result<GitOutput> {
        let output = self.output(cwd, args)?;
        Ok(GitOutput {
            code: output.status.code(),
            stdout: output.stdout,
        })
    }

    pub(crate) fn run_status_with_input<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        input: &[u8],
    ) -> Result<GitOutput> {
        let output = self.output_with_input(cwd, args, Some(input))?;
        Ok(GitOutput {
            code: output.status.code(),
            stdout: output.stdout,
        })
    }

    /// Run git while incrementally consuming stdout.
    ///
    /// The callback may retain as little output as its caller needs. Returning
    /// [`StreamControl::Saturated`] closes the pipes, terminates this exact child,
    /// and reaps it; that is a successful bounded result rather than a git
    /// failure. Callback errors still drain both pipes through completion.
    pub(crate) fn run_stream<S, F>(
        &self,
        cwd: &Path,
        args: &[S],
        mut consume_stdout: F,
    ) -> Result<()>
    where
        S: AsRef<OsStr>,
        F: FnMut(&[u8]) -> std::io::Result<StreamControl>,
    {
        let mut child = self
            .command(cwd, args)?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(map_command_error)?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        std::thread::scope(|scope| {
            let stderr_reader = scope.spawn(move || read_bounded_stderr(stderr));
            let mut callback_error = None;
            let mut read_error = None;
            let mut saturated = false;
            let mut buffer = [0; STREAM_BUFFER_BYTES];

            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if callback_error.is_none() {
                            match consume_stdout(&buffer[..read]) {
                                Ok(StreamControl::Continue) => {}
                                Ok(StreamControl::Saturated) => {
                                    saturated = true;
                                    break;
                                }
                                Err(error) => callback_error = Some(error),
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        read_error = Some(error);
                        let _ = child.kill();
                        break;
                    }
                }
            }

            // Check before intervening so a real failure that already happened
            // is still reported. Once saturation wins the race, closing stdout
            // and killing only this child is intentional truncation.
            let status_before_saturation = if saturated {
                child.try_wait().map_err(Error::Io)?
            } else {
                None
            };
            drop(stdout);
            if saturated && status_before_saturation.is_none() {
                let _ = child.kill();
            }
            let status = child.wait().map_err(Error::Io)?;
            let stderr = stderr_reader
                .join()
                .expect("git stderr reader panicked")
                .map_err(Error::Io)?;
            if (!saturated || status_before_saturation.is_some()) && !status.success() {
                return Err(git_failed(args, cwd, &stderr));
            }
            if let Some(error) = read_error.or(callback_error) {
                return Err(Error::Io(error));
            }
            Ok(())
        })
    }

    fn output<S: AsRef<OsStr>>(&self, cwd: &Path, args: &[S]) -> Result<Output> {
        self.output_with_input(cwd, args, None)
    }

    fn output_with_input<S: AsRef<OsStr>>(
        &self,
        cwd: &Path,
        args: &[S],
        input: Option<&[u8]>,
    ) -> Result<Output> {
        let mut cmd = self.command(cwd, args)?;
        if input.is_none() {
            return cmd.output().map_err(map_command_error);
        }

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(map_command_error)?;

        let mut stdin = child.stdin.take().expect("stdin is piped");
        let input = input.expect("input was checked");
        std::thread::scope(|scope| {
            // patch-id can emit enough output to fill its pipe before consuming
            // a large history. Feeding stdin while wait_with_output drains both
            // output pipes prevents either side from waiting on the other.
            let writer = scope.spawn(move || stdin.write_all(input));
            let output = child.wait_with_output().map_err(Error::Io)?;
            writer.join().expect("git stdin writer panicked")?;
            Ok(output)
        })
    }

    fn command<S: AsRef<OsStr>>(&self, cwd: &Path, args: &[S]) -> Result<Command> {
        let mut cmd = Command::new(&self.program);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;

            // yawm is a GUI-subsystem executable in release builds. Starting a
            // console executable from it without this flag asks Windows to
            // create a console window for the child, so the initial scan
            // flashes one window per Git probe — dozens of windows opening and
            // closing until the user kills the app. Git is always captured and
            // never interactive here, so it has no console to show.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        // Rust's canonical Windows paths use the verbatim namespace
        // (`\\?\C:\...`). The OS APIs accept that spelling and Git for Windows
        // does not consistently: `worktree add` and `submodule add` reject it
        // while trying to create their administrative files. Keep canonical
        // paths inside yawm and translate only at the external Git boundary.
        if let Some(context) = execution_context(cwd)
            && context.explicit
        {
            cmd.arg("--git-dir")
                .arg(git_argument(context.git_dir.as_os_str()));
            if let Some(work_tree) = context.work_tree {
                cmd.arg("--work-tree")
                    .arg(git_argument(work_tree.as_os_str()));
            }
        }
        for arg in args {
            cmd.arg(git_argument(arg.as_ref()));
        }
        // Skipping current_dir when the path is missing would let git inherit
        // yawm's own working directory, so a repository on an unmounted volume
        // would quietly answer with whatever repository yawm was launched from
        // — one project's worktrees listed, and removed, under another's name.
        // Refusing is the only reading that cannot lose someone's work.
        if !cwd.is_dir() {
            return Err(missing_working_directory(cwd));
        }

        cmd.current_dir(cwd);
        // Keep output stable regardless of the user's locale and config.
        cmd.env("LC_ALL", "C");
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("GIT_NO_REPLACE_OBJECTS", "1");
        Ok(cmd)
    }
}

/// Resolve a repository without executing Git.
///
/// Linked worktrees and explicitly selected bare repositories must be invoked
/// with `--git-dir` (and, for a checkout, `--work-tree`). That is both sufficient
/// for `safe.bareRepository=explicit` and faithful to the user's configuration;
/// no safety setting is overridden.
pub(crate) fn execution_context(cwd: &Path) -> Option<GitExecutionContext> {
    let dot_git = cwd.join(".git");
    if dot_git.is_dir() {
        let git_dir = absolute_existing(&dot_git);
        return Some(GitExecutionContext {
            common_dir: git_dir.clone(),
            git_dir,
            work_tree: Some(absolute_existing(cwd)),
            explicit: false,
        });
    }

    if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let value = text.lines().next()?.trim().strip_prefix("gitdir:")?.trim();
        if value.is_empty() {
            return None;
        }
        let candidate = PathBuf::from(value);
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            cwd.join(candidate)
        };
        let git_dir = absolute_existing(&candidate);
        let common_dir = read_common_dir(&git_dir).unwrap_or_else(|| git_dir.clone());
        return Some(GitExecutionContext {
            git_dir,
            common_dir,
            work_tree: Some(absolute_existing(cwd)),
            explicit: true,
        });
    }

    if cwd.join("HEAD").is_file() && cwd.join("objects").is_dir() && cwd.join("config").is_file() {
        let git_dir = absolute_existing(cwd);
        return Some(GitExecutionContext {
            common_dir: git_dir.clone(),
            git_dir,
            work_tree: None,
            explicit: true,
        });
    }
    None
}

fn read_common_dir(git_dir: &Path) -> Option<PathBuf> {
    let value = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        git_dir.join(path)
    };
    Some(absolute_existing(&path))
}

fn absolute_existing(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|current| current.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
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

fn git_failed<S: AsRef<OsStr>>(args: &[S], cwd: &Path, stderr: &[u8]) -> Error {
    Error::GitFailed {
        args: args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect(),
        cwd: cwd.to_path_buf(),
        stderr: String::from_utf8_lossy(stderr).trim().to_string(),
    }
}

fn read_bounded_stderr(mut stderr: impl Read) -> std::io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; STREAM_BUFFER_BYTES];
    loop {
        let read = match stderr.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let keep = read.min(MAX_GIT_STDERR_BYTES.saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    if truncated {
        retained.extend_from_slice(b"\n[stderr truncated]");
    }
    Ok(retained)
}

/// Reported as `NotFound` rather than a git failure because nothing was run:
/// the directory a caller named is gone, unmounted, or never existed.
fn missing_working_directory(cwd: &Path) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "{} is not a directory; git was not run there",
            cwd.display()
        ),
    ))
}

fn map_command_error(e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::GitMissing
    } else {
        Error::Io(e)
    }
}

/// Extract `(major, minor)` from `git --version` output.
fn parse_version(text: &str) -> Option<(u32, u32)> {
    let digits = text
        .split_whitespace()
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = digits.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()
        .and_then(|m| {
            m.trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()
        })
        .unwrap_or(0);
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_standard_version_output() {
        assert_eq!(parse_version("git version 2.53.0"), Some((2, 53)));
    }

    #[test]
    fn parses_apple_git_version_output() {
        assert_eq!(
            parse_version("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39))
        );
    }

    #[test]
    fn parses_windows_version_output() {
        assert_eq!(parse_version("git version 2.45.1.windows.1"), Some((2, 45)));
    }

    #[test]
    fn rejects_unparseable_version() {
        assert_eq!(parse_version("not a version"), None);
    }

    #[test]
    fn version_gate_matches_the_documented_floor() {
        assert!((2, 36) >= MIN_VERSION_FOR_NUL);
        assert!((2, 35) < MIN_VERSION_FOR_NUL);
        assert!((3, 0) >= MIN_VERSION_FOR_NUL);
    }

    #[cfg(unix)]
    #[test]
    fn streaming_drains_large_stdout_and_stderr_and_keeps_the_git_error() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/yawm-git-tests")
            .join(format!("streaming-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let script = root.join("verbose.sh");
        fs::write(
            &script,
            "#!/bin/sh\n\
             printf 'specific failure reason\\n' >&2\n\
             i=0\n\
             while [ \"$i\" -lt 20000 ]; do\n\
               printf 'stdout-0123456789\\n'\n\
               printf 'stderr-0123456789\\n' >&2\n\
               i=$((i + 1))\n\
             done\n\
             exit 7\n",
        )
        .unwrap();

        let git = Git::with_program("/bin/sh");
        let mut stdout_bytes = 0usize;
        let error = git
            .run_stream(&root, &[script.as_os_str()], |chunk| {
                stdout_bytes += chunk.len();
                Ok(StreamControl::Continue)
            })
            .expect_err("the fake git exits unsuccessfully");

        let _ = fs::remove_dir_all(&root);
        assert!(stdout_bytes > 256 * 1024);
        match error {
            Error::GitFailed { stderr, .. } => {
                assert!(stderr.contains("specific failure reason"));
                assert!(stderr.contains("[stderr truncated]"));
            }
            other => panic!("expected a git failure, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn streaming_saturation_terminates_and_reaps_the_exact_child_successfully() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let script = root.join("large-output.sh");
        let completed = root.join("completed");
        fs::write(
            &script,
            "#!/bin/sh\n\
             i=0\n\
             while [ \"$i\" -lt 100000 ]; do\n\
               printf 'stdout-0123456789\\n'\n\
               i=$((i + 1))\n\
             done\n\
             printf done > completed\n",
        )
        .unwrap();

        let git = Git::with_program("/bin/sh");
        let mut calls = 0usize;
        git.run_stream(root, &[script.as_os_str()], |_| {
            calls += 1;
            Ok(StreamControl::Saturated)
        })
        .expect("intentional saturation is a successful truncated result");

        assert_eq!(calls, 1);
        assert!(
            !completed.exists(),
            "the child must be stopped instead of draining its remaining output"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_git_failure_completed_before_saturation_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let script = root.join("failed-before-stop.sh");
        fs::write(
            &script,
            "#!/bin/sh\n\
             printf output\n\
             printf 'failure before stop\\n' >&2\n\
             exit 7\n",
        )
        .unwrap();

        let git = Git::with_program("/bin/sh");
        let error = git
            .run_stream(root, &[script.as_os_str()], |_| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                Ok(StreamControl::Saturated)
            })
            .expect_err("the child had already failed before saturation");

        match error {
            Error::GitFailed { stderr, .. } => assert!(stderr.contains("failure before stop")),
            other => panic!("expected a git failure, got {other:?}"),
        }
    }

    /// The failure this guards against is silent, not loud: git inheriting
    /// yawm's own working directory answers confidently about the wrong
    /// repository, and every path yawm reports back is then someone else's.
    #[test]
    fn a_missing_working_directory_is_an_error_rather_than_an_inheritance() {
        let git = Git::new();
        let err = git
            .run(
                Path::new("/definitely/not/here/unmounted-repo"),
                &["rev-parse", "--show-toplevel"],
            )
            .expect_err("git must refuse to run somewhere else");

        match err {
            Error::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected a missing-directory error, got {other:?}"),
        }
    }

    /// A regular file is not a working directory either, and git would
    /// otherwise have run in whatever directory yawm was launched from.
    #[test]
    fn a_file_is_not_a_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"").unwrap();

        let git = Git::new();
        assert!(git.run_checked(&file, &["status"]).is_err());
        assert!(git.run_status(&file, &["status"]).is_err());
        assert!(
            git.run_status_with_input(&file, &["patch-id"], b"")
                .is_err(),
            "the stdin-feeding path shares the same guard"
        );
    }

    /// An existing directory that is not a repository must still reach git, so
    /// the guard cannot be mistaken for "only run inside repositories".
    #[test]
    fn an_existing_directory_still_reaches_git() {
        let dir = tempfile::tempdir().unwrap();
        let git = Git::new();
        assert!(
            git.run_checked(dir.path(), &["rev-parse", "--show-toplevel"])
                .is_ok(),
            "git should have been invoked and allowed to answer for itself"
        );
    }
}
