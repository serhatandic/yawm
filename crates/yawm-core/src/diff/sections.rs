//! Splitting a streamed `git diff` into whole file sections, and deciding what
//! each section actually *is*.
//!
//! The UI renders a unified patch with a real diff viewer, which needs at least
//! one hunk to show anything. A section with no hunk — a mode change, a pure
//! rename, a new empty file, a binary blob — used to be handed to that viewer
//! anyway, and it rendered an expandable row that opened onto nothing. So the
//! section is classified here, once, against the patch text rather than by
//! sniffing prose in the frontend.

use super::{EntryContent, MAX_DIFF_HEADER_LEN, display_git_path};

const DIFF_HEADERS: [&[u8]; 3] = [b"diff --git ", b"diff --cc ", b"diff --combined "];

#[derive(Debug, Default)]
pub(super) struct BoundedPatch {
    pub sections: Vec<String>,
    pub truncated: bool,
}

/// Accumulates whole `diff --git` sections, never a half one.
///
/// A patch cut mid-hunk is worse than a shorter patch: it reads as a real
/// change that simply ends. The budget is therefore spent in whole sections.
pub(super) struct PatchSectionCollector {
    max_bytes: usize,
    sections: Vec<String>,
    bytes: usize,
    pending: Vec<u8>,
    saw_patch: bool,
    header_search_from: usize,
    truncated: bool,
    stopped: bool,
}

impl PatchSectionCollector {
    pub(super) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            sections: Vec::new(),
            bytes: 0,
            pending: Vec::new(),
            saw_patch: false,
            header_search_from: 0,
            truncated: false,
            stopped: false,
        }
    }

    pub(super) fn consume(&mut self, mut bytes: &[u8]) -> crate::git::StreamControl {
        use crate::git::StreamControl;

        while !bytes.is_empty() && !self.stopped {
            let remaining = self.max_bytes.saturating_sub(self.bytes);
            let capacity = remaining.saturating_add(MAX_DIFF_HEADER_LEN);
            let available = capacity.saturating_sub(self.pending.len());
            if available == 0 {
                self.truncated = true;
                self.stopped = true;
                return StreamControl::Saturated;
            }

            let take = available.min(bytes.len());
            self.pending.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            self.extract_complete_sections();

            if !bytes.is_empty() && self.pending.len() >= capacity {
                self.truncated = true;
                self.stopped = true;
            }
        }
        if self.stopped {
            StreamControl::Saturated
        } else {
            StreamControl::Continue
        }
    }

    fn extract_complete_sections(&mut self) {
        if !self.saw_patch {
            let Some(start) = find_diff_header(&self.pending, self.header_search_from) else {
                self.header_search_from = next_header_search_start(&self.pending, 0);
                return;
            };
            self.truncated |= self.pending[..start]
                .iter()
                .any(|byte| !byte.is_ascii_whitespace());
            self.pending.drain(..start);
            self.saw_patch = true;
            self.header_search_from = 1;
        }

        loop {
            let Some(next) = find_diff_header(&self.pending, self.header_search_from) else {
                self.header_search_from = next_header_search_start(&self.pending, 1);
                return;
            };
            if self.append_section(next) {
                self.pending.drain(..next);
                self.header_search_from = 1;
            } else {
                return;
            }
        }
    }

    fn append_section(&mut self, end: usize) -> bool {
        let rendered = String::from_utf8_lossy(&self.pending[..end]);
        if rendered.len() > self.max_bytes.saturating_sub(self.bytes) {
            self.truncated = true;
            self.stopped = true;
            return false;
        }
        self.bytes += rendered.len();
        self.sections.push(rendered.into_owned());
        true
    }

    pub(super) fn finish(mut self) -> BoundedPatch {
        if !self.stopped {
            self.extract_complete_sections();
            if self.saw_patch && !self.pending.is_empty() {
                self.append_section(self.pending.len());
            } else if self.pending.iter().any(|byte| !byte.is_ascii_whitespace()) {
                self.truncated = true;
            }
        }
        BoundedPatch {
            sections: self.sections,
            truncated: self.truncated,
        }
    }
}

fn next_header_search_start(bytes: &[u8], minimum: usize) -> usize {
    bytes
        .len()
        .saturating_sub(MAX_DIFF_HEADER_LEN.saturating_sub(1))
        .max(minimum)
}

fn find_diff_header(bytes: &[u8], from: usize) -> Option<usize> {
    if from >= bytes.len() {
        return None;
    }
    for at in from..bytes.len() {
        if at != 0 && bytes[at - 1] != b'\n' {
            continue;
        }
        if DIFF_HEADERS
            .iter()
            .any(|header| bytes[at..].starts_with(header))
        {
            return Some(at);
        }
    }
    None
}

/// What one file section of a patch turned out to be.
#[derive(Debug)]
pub(super) struct ParsedSection {
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
    pub content: EntryContent,
}

/// Classify a single `diff --git` section.
///
/// The invariant this exists to guarantee: `EntryContent::Text` is produced
/// only when the section contains at least one hunk header, so anything the
/// frontend hands to the patch viewer can actually render.
pub(super) fn classify_section(section: &str) -> ParsedSection {
    let mut hunks = 0u32;
    let mut insertions = 0u32;
    let mut deletions = 0u32;
    let mut binary = false;
    let mut new_file = false;
    let mut deleted_file = false;
    let mut renamed = false;
    let mut old_mode: Option<String> = None;
    let mut new_mode: Option<String> = None;

    for line in section.lines() {
        if line.starts_with("@@") {
            hunks += 1;
            continue;
        }
        if hunks > 0 {
            // Inside a hunk `+++`/`---` cannot occur as headers any more.
            if let Some(first) = line.as_bytes().first() {
                match first {
                    b'+' => insertions += 1,
                    b'-' => deletions += 1,
                    _ => {}
                }
            }
            continue;
        }
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            binary = true;
        } else if let Some(mode) = line.strip_prefix("new file mode ") {
            new_file = true;
            new_mode = Some(mode.trim().to_string());
        } else if let Some(mode) = line.strip_prefix("deleted file mode ") {
            deleted_file = true;
            old_mode = Some(mode.trim().to_string());
        } else if let Some(mode) = line.strip_prefix("old mode ") {
            old_mode = Some(mode.trim().to_string());
        } else if let Some(mode) = line.strip_prefix("new mode ") {
            new_mode = Some(mode.trim().to_string());
        } else if line.starts_with("rename from ") || line.starts_with("rename to ") {
            renamed = true;
        }
    }

    let path = section_path(section);
    let content = if hunks > 0 {
        EntryContent::Text {
            patch: section.to_string(),
            hunks,
        }
    } else if binary {
        EntryContent::Binary
    } else if new_file {
        EntryContent::Empty
    } else if deleted_file {
        EntryContent::Metadata {
            detail: "Deleted a file that had no content.".into(),
        }
    } else if renamed {
        EntryContent::Metadata {
            detail: "Renamed with no change to the file's contents.".into(),
        }
    } else if let (Some(old), Some(new)) = (old_mode.as_ref(), new_mode.as_ref()) {
        EntryContent::Metadata {
            detail: format!("File mode changed from {old} to {new}."),
        }
    } else {
        EntryContent::Metadata {
            detail: "Git recorded this file as changed with no line changes.".into(),
        }
    };

    ParsedSection {
        path,
        insertions,
        deletions,
        content,
    }
}

/// The file a section is about, named the way `numstat` names it.
///
/// The b-side wins: the patch below the file list names every file by where it
/// ended up, so an old name would be a row the reader cannot find.
fn section_path(section: &str) -> String {
    let mut from_header = None;
    let mut minus = None;
    let mut plus = None;

    for line in section.lines() {
        if let Some(rest) = line.strip_prefix("rename to ") {
            return side_path(rest.trim_end_matches(['\r']));
        }
        if let Some(rest) = line.strip_prefix("+++ ").filter(|_| plus.is_none()) {
            let rest = rest.trim_end_matches(['\r']);
            if rest != "/dev/null" {
                plus = Some(side_path(rest));
            }
        }
        if let Some(rest) = line.strip_prefix("--- ").filter(|_| minus.is_none()) {
            let rest = rest.trim_end_matches(['\r']);
            if rest != "/dev/null" {
                minus = Some(side_path(rest));
            }
        }
        if from_header.is_none() {
            for header in ["diff --git ", "diff --cc ", "diff --combined "] {
                if let Some(rest) = line.strip_prefix(header) {
                    from_header = Some(header_path(rest.trim_end_matches(['\r'])));
                    break;
                }
            }
        }
        if line.starts_with("@@") {
            break;
        }
    }

    plus.or(minus)
        .or(from_header)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Decode a quoted path and drop its `a/` or `b/` side prefix.
///
/// The prefix has to come off the *decoded bytes*: re-quoting afterwards would
/// otherwise leave `"b/odd-\377"` where numstat says `"odd-\377"`, and the row
/// would never match its patch.
fn side_path(value: &str) -> String {
    let bytes = unquote_git_bytes(value);
    let stripped = bytes
        .strip_prefix(b"a/")
        .or_else(|| bytes.strip_prefix(b"b/"))
        .unwrap_or(&bytes);
    display_git_path(stripped)
}

/// `diff --git a/x b/x` — the two halves are the same path, so split at the
/// midpoint rather than at the first space, which a filename may contain.
fn header_path(rest: &str) -> String {
    // A quoted pair: `"a/x" "b/x"`. Take the last quoted run.
    if let Some(last) = rest.rfind(" \"").filter(|_| rest.starts_with('"')) {
        return side_path(&rest[last + 1..]);
    }
    if rest.len() % 2 == 1 {
        let (left, right) = rest.split_at(rest.len() / 2);
        if right.starts_with(' ') && left.len() == right.len() - 1 {
            return side_path(&right[1..]);
        }
    }
    match rest.rfind(" b/") {
        Some(at) => side_path(&rest[at + 1..]),
        None => rest.to_string(),
    }
}

/// Reverse `quote_git_path`, back to the raw bytes Git was naming.
fn unquote_git_bytes(value: &str) -> Vec<u8> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return value.as_bytes().to_vec();
    };

    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buffer = [0u8; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            continue;
        }
        match chars.next() {
            Some('\\') => bytes.push(b'\\'),
            Some('"') => bytes.push(b'"'),
            Some('n') => bytes.push(b'\n'),
            Some('r') => bytes.push(b'\r'),
            Some('t') => bytes.push(b'\t'),
            Some(first @ '0'..='7') => {
                let mut value = first.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    let Some(next) = chars.clone().next().filter(char::is_ascii_digit) else {
                        break;
                    };
                    let Some(digit) = next.to_digit(8) else { break };
                    chars.next();
                    value = value * 8 + digit;
                }
                bytes.push(u8::try_from(value).unwrap_or(b'?'));
            }
            Some(other) => {
                let mut buffer = [0u8; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buffer).as_bytes());
            }
            None => break,
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(section: &str) -> EntryContent {
        classify_section(section).content
    }

    #[test]
    fn a_section_with_a_hunk_is_text() {
        let section =
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let parsed = classify_section(section);

        assert!(matches!(
            parsed.content,
            EntryContent::Text { hunks: 1, .. }
        ));
        assert_eq!(parsed.path, "a.txt");
        assert_eq!(parsed.insertions, 1);
        assert_eq!(parsed.deletions, 1);
    }

    #[test]
    fn a_binary_section_is_never_text() {
        let section = "diff --git a/logo.png b/logo.png\nindex 1..2 100644\nBinary files a/logo.png and b/logo.png differ\n";
        assert!(matches!(kind(section), EntryContent::Binary));
    }

    #[test]
    fn a_new_empty_file_is_empty_not_a_blank_patch() {
        let section = "diff --git a/empty.txt b/empty.txt\nnew file mode 100644\nindex 0..0\n";
        assert!(matches!(kind(section), EntryContent::Empty));
    }

    #[test]
    fn a_mode_change_explains_itself() {
        let section = "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        let EntryContent::Metadata { detail } = kind(section) else {
            panic!("expected metadata");
        };
        assert_eq!(detail, "File mode changed from 100644 to 100755.");
    }

    #[test]
    fn a_pure_rename_explains_itself() {
        let section = "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs\n";
        let parsed = classify_section(section);
        assert_eq!(parsed.path, "new.rs");
        assert!(matches!(parsed.content, EntryContent::Metadata { .. }));
    }

    #[test]
    fn quoted_paths_are_decoded_to_match_numstat() {
        let section = "diff --git \"a/odd-\\377.txt\" \"b/odd-\\377.txt\"\n--- \"a/odd-\\377.txt\"\n+++ \"b/odd-\\377.txt\"\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(classify_section(section).path, "\"odd-\\377.txt\"");
    }

    #[test]
    fn a_path_with_spaces_survives_the_header() {
        let section = "diff --git a/my folder/a file.txt b/my folder/a file.txt\n--- /dev/null\n+++ b/my folder/a file.txt\n@@ -0,0 +1 @@\n+x\n";
        assert_eq!(classify_section(section).path, "my folder/a file.txt");
    }

    #[test]
    fn a_deleted_file_is_named_by_its_old_path() {
        let section = "diff --git a/gone.txt b/gone.txt\ndeleted file mode 100644\n--- a/gone.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        assert_eq!(classify_section(section).path, "gone.txt");
    }

    #[test]
    fn oversized_streamed_patch_keeps_only_complete_file_sections() {
        let first =
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let second = format!(
            "diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-brief\n+{}\n",
            "large".repeat(20_000)
        );
        let bytes = format!("{first}{second}").into_bytes();
        let mut collector = PatchSectionCollector::new(first.len() + 8);
        for chunk in bytes.chunks(37) {
            collector.consume(chunk);
        }

        let patch = collector.finish();

        assert_eq!(patch.sections, vec![first.to_string()]);
        assert!(patch.truncated);
    }

    #[test]
    fn streamed_patch_recognises_combined_sections_across_chunk_boundaries() {
        let first = "diff --cc conflict.txt\nindex 111,222..333\n--- a/conflict.txt\n+++ b/conflict.txt\n@@@ -1,1 -1,1 +1,1 @@@\n-old\n -theirs\n++resolved\n";
        let second = "diff --combined other.txt\nindex 111,222..333\n--- a/other.txt\n+++ b/other.txt\n@@@ -1,1 -1,1 +1,1 @@@\n-a\n -b\n++c\n";
        let third = format!(
            "diff --git a/large.txt b/large.txt\n--- a/large.txt\n+++ b/large.txt\n@@ -1 +1 @@\n-old\n+{}\n",
            "large".repeat(20_000)
        );
        let mut collector = PatchSectionCollector::new(first.len() + second.len());
        for chunk in format!("{first}{second}{third}").as_bytes().chunks(5) {
            collector.consume(chunk);
        }

        let patch = collector.finish();

        assert_eq!(patch.sections, vec![first.to_string(), second.to_string()]);
        assert!(patch.truncated);
    }
}
