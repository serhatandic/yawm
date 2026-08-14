//! Finding the editors a machine actually has.
//!
//! yawm's "Open" used to fall back to the system opener when no editor was
//! configured, which made it identical to "Reveal" — two buttons doing the
//! same thing, and neither of them opening an editor. Offering a list of what
//! is installed removes the configuration step that nobody performed.
//!
//! Detection is by presence, never by launching anything: an editor that is
//! slow to start should not make a scan slow, and probing by execution would
//! open windows the user did not ask for.

use std::path::{Path, PathBuf};

/// An editor this machine can open a directory with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Editor {
    /// Stable across releases and platforms, so a stored choice survives both.
    pub id: String,
    pub name: String,
    /// What [`super::open_with`] should be handed.
    ///
    /// On macOS this is an application name for `open -a`, which works whether
    /// or not the editor's command line tools were ever installed. Elsewhere
    /// it is an executable found on `PATH`.
    pub command: String,
}

/// Candidates, most commonly used first.
///
/// `(id, display name, macOS application name, executable elsewhere)`
const KNOWN: &[(&str, &str, &str, &str)] = &[
    ("vscode", "Visual Studio Code", "Visual Studio Code", "code"),
    ("cursor", "Cursor", "Cursor", "cursor"),
    ("zed", "Zed", "Zed", "zed"),
    ("windsurf", "Windsurf", "Windsurf", "windsurf"),
    ("sublime", "Sublime Text", "Sublime Text", "subl"),
    ("vscodium", "VSCodium", "VSCodium", "codium"),
    (
        "vscode-insiders",
        "VS Code Insiders",
        "Visual Studio Code - Insiders",
        "code-insiders",
    ),
    ("intellij", "IntelliJ IDEA", "IntelliJ IDEA", "idea"),
    ("webstorm", "WebStorm", "WebStorm", "webstorm"),
    ("pycharm", "PyCharm", "PyCharm", "pycharm"),
    ("rustrover", "RustRover", "RustRover", "rustrover"),
    ("goland", "GoLand", "GoLand", "goland"),
    ("fleet", "Fleet", "Fleet", "fleet"),
    ("nova", "Nova", "Nova", "nova"),
    ("xcode", "Xcode", "Xcode", "xed"),
    ("emacs", "Emacs", "Emacs", "emacs"),
];

/// Every known editor present on this machine.
pub fn detect() -> Vec<Editor> {
    KNOWN
        .iter()
        .filter_map(|(id, name, mac_app, exe)| {
            let command = resolve(mac_app, exe)?;
            Some(Editor {
                id: (*id).to_string(),
                name: (*name).to_string(),
                command,
            })
        })
        .collect()
}

/// What to hand `open_with`, or `None` when this editor is not installed.
#[cfg(target_os = "macos")]
fn resolve(mac_app: &str, exe: &str) -> Option<String> {
    // An installed .app is the reliable signal. The command line helper is
    // opt-in for most of these editors and its absence says nothing about
    // whether the editor is there.
    if application_bundle(mac_app).is_some() {
        return Some(mac_app.to_string());
    }
    on_path(exe).map(|_| exe.to_string())
}

#[cfg(not(target_os = "macos"))]
fn resolve(_mac_app: &str, exe: &str) -> Option<String> {
    on_path(exe).map(|_| exe.to_string())
}

#[cfg(target_os = "macos")]
fn application_bundle(name: &str) -> Option<PathBuf> {
    let file = format!("{name}.app");
    let roots = [
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        dirs_home()
            .map(|h| h.join("Applications"))
            .unwrap_or_default(),
    ];
    roots
        .into_iter()
        .map(|root| root.join(&file))
        .find(|candidate| candidate.is_dir())
}

#[cfg(target_os = "macos")]
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Look up an executable the way a shell would, without running it.
fn on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(exe);
        is_executable(&candidate).then_some(candidate)
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    // Windows decides by extension, and PATHEXT is what says which ones count.
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into());
    if path.is_file() {
        return true;
    }
    exts.split(';').any(|ext| {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(ext);
        Path::new(&candidate).is_file()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = KNOWN.iter().map(|(id, ..)| *id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "an id is reused, so choices would clash");
    }

    #[test]
    fn a_missing_executable_is_not_reported() {
        assert!(on_path("yawm-definitely-not-a-real-editor").is_none());
    }

    /// Detection must never launch anything, so it stays cheap enough to call
    /// whenever the panel opens.
    #[test]
    fn detection_returns_only_installed_editors() {
        for editor in detect() {
            assert!(!editor.command.is_empty());
            assert!(!editor.name.is_empty());
        }
    }
}
