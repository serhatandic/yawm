//! Branch metadata gathered with a single `git for-each-ref` call.
//!
//! Worktrees in a repository share one ref database, so every branch's
//! upstream, divergence, and last commit can be read in one invocation rather
//! than one per worktree. On a machine with thirty worktrees that is the
//! difference between one git process and ninety.

use std::collections::HashMap;

/// Field separator. NUL cannot appear in any of the fields, unlike `|` or tab,
/// which are legal in commit subjects.
pub const FIELD_SEP: &str = "%00";

/// Fields, in order, matching [`BranchInfo`]. The subject is last so that any
/// unexpected content stays contained in the final field.
pub const FOR_EACH_REF_FORMAT: &str = concat!(
    "%(refname)%00",
    "%(refname:short)%00",
    "%(objectname)%00",
    "%(upstream:short)%00",
    "%(upstream:track)%00",
    "%(upstream)%00",
    "%(committerdate:unix)%00",
    "%(contents:subject)"
);

/// The ref namespaces one listing must cover.
///
/// Remote-tracking refs are listed alongside the branches so an upstream's
/// commit can be resolved from the same output. `%(upstream:objectname)` would
/// say it directly but git only learned that format recently, and a removal
/// guard that silently loses a field on an older git is not a guard.
pub const FOR_EACH_REF_NAMESPACES: [&str; 2] = ["refs/heads/", "refs/remotes/"];

/// Format for the follow-up listing that resolves refs outside those
/// namespaces, one line per ref: `<full ref>\0<commit>`.
pub const REF_OID_FORMAT: &str = concat!("%(refname)%00", "%(objectname)");

/// Parse output produced with [`REF_OID_FORMAT`] into full ref name to commit.
pub fn parse_ref_oids(bytes: &[u8]) -> HashMap<String, String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter_map(|line| line.split_once('\0'))
        .filter(|(name, oid)| !name.is_empty() && !oid.is_empty())
        .map(|(name, oid)| (name.to_string(), oid.to_string()))
        .collect()
}

/// What one branch looks like from the ref database.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BranchInfo {
    pub name: String,
    pub head: String,
    pub upstream: Option<String>,
    /// The commit the upstream ref points at.
    ///
    /// The ahead count says how far apart the two are; this says what they are.
    /// Amending the single unpushed commit leaves the count at one and changes
    /// nothing else a plan reads.
    pub upstream_oid: Option<String>,
    /// The upstream's full ref name, e.g. `refs/remotes/origin/feat/auth`.
    ///
    /// Kept because the listing below covers two namespaces and a fetch
    /// refspec may put a tracking ref anywhere — `refs/pr/*` is a common one.
    /// Without the full name there is nothing left to resolve such a ref by.
    pub upstream_ref: Option<String>,
    /// An upstream is configured and still exists, but its commit could not be
    /// established.
    ///
    /// Never "unchanged": a removal guard that reads a missing upstream commit
    /// as "no upstream commit" cannot tell a rewritten upstream from a stable
    /// one, so this is carried through to the plan and refuses instead.
    pub upstream_unresolved: bool,
    pub ahead: usize,
    pub behind: usize,
    /// The upstream was configured but no longer exists on the remote.
    pub gone: bool,
    pub committed_at: Option<i64>,
    pub subject: Option<String>,
}

/// Parse `for-each-ref` output produced with [`FOR_EACH_REF_FORMAT`].
pub fn parse_for_each_ref(bytes: &[u8]) -> HashMap<String, BranchInfo> {
    let text = String::from_utf8_lossy(bytes);

    let rows: Vec<Vec<&str>> = text
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\0').collect::<Vec<&str>>())
        .filter(|fields| fields.len() >= 7)
        .collect();

    // Every ref in the listing, by its full name, so an upstream can be
    // resolved to the commit it points at without a second invocation.
    let oids: HashMap<&str, &str> = rows
        .iter()
        .map(|fields| (fields[0], fields[2]))
        .filter(|(name, oid)| !name.is_empty() && !oid.is_empty())
        .collect();

    let mut map = HashMap::new();
    for fields in &rows {
        if !fields[0].starts_with("refs/heads/") {
            continue;
        }

        let (ahead, behind, gone) = parse_track(fields[4]);
        let upstream_ref = (!fields[5].is_empty()).then(|| fields[5].to_string());
        let upstream_oid = oids.get(fields[5]).map(|oid| (*oid).to_string());
        let info = BranchInfo {
            name: fields[1].to_string(),
            head: fields[2].to_string(),
            upstream: (!fields[3].is_empty()).then(|| fields[3].to_string()),
            // Configured, still there, and outside both listed namespaces —
            // a custom fetch refspec. Left for `load_branches` to resolve, and
            // still true afterwards if even that could not name its commit.
            upstream_unresolved: upstream_ref.is_some() && !gone && upstream_oid.is_none(),
            upstream_oid,
            upstream_ref,
            ahead,
            behind,
            gone,
            committed_at: fields[6].parse().ok(),
            subject: fields.get(7).and_then(|s| {
                let s = s.trim();
                (!s.is_empty()).then(|| s.to_string())
            }),
        };
        map.insert(info.name.clone(), info);
    }

    map
}

/// Interpret `%(upstream:track)`.
///
/// Observed forms: empty (in sync or no upstream), `[ahead 1]`, `[behind 2]`,
/// `[ahead 1, behind 2]`, and `[gone]`.
///
/// `[gone]` records remote state only. It is useful context, but cannot prove
/// that the branch's work landed: remotes permit deleting an unmerged branch.
fn parse_track(track: &str) -> (usize, usize, bool) {
    let inner = track.trim();
    let inner = inner
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(inner);

    if inner.is_empty() {
        return (0, 0, false);
    }
    if inner.eq_ignore_ascii_case("gone") {
        return (0, 0, true);
    }

    let mut ahead = 0;
    let mut behind = 0;
    for part in inner.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            ahead = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            behind = n.trim().parse().unwrap_or(0);
        }
    }
    (ahead, behind, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: &[&str]) -> String {
        fields.join("\0")
    }

    /// One `refs/heads/` row, as git emits it.
    fn branch(
        short: &str,
        oid: &str,
        upstream_short: &str,
        track: &str,
        upstream_full: &str,
        date: &str,
        subject: &str,
    ) -> String {
        line(&[
            &format!("refs/heads/{short}"),
            short,
            oid,
            upstream_short,
            track,
            upstream_full,
            date,
            subject,
        ])
    }

    /// One `refs/remotes/` row; only its name and object matter here.
    fn remote(short: &str, oid: &str) -> String {
        line(&[
            &format!("refs/remotes/{short}"),
            short,
            oid,
            "",
            "",
            "",
            "1700000000",
            "",
        ])
    }

    #[test]
    fn in_sync_branch_has_no_divergence() {
        assert_eq!(parse_track(""), (0, 0, false));
    }

    #[test]
    fn parses_ahead_only() {
        assert_eq!(parse_track("[ahead 1]"), (1, 0, false));
    }

    #[test]
    fn parses_behind_only() {
        assert_eq!(parse_track("[behind 3]"), (0, 3, false));
    }

    #[test]
    fn parses_ahead_and_behind() {
        assert_eq!(parse_track("[ahead 2, behind 5]"), (2, 5, false));
    }

    #[test]
    fn parses_gone_marker() {
        assert_eq!(parse_track("[gone]"), (0, 0, true));
    }

    #[test]
    fn gone_is_not_confused_with_divergence() {
        let (ahead, behind, gone) = parse_track("[gone]");
        assert!(gone);
        assert_eq!((ahead, behind), (0, 0));
    }

    #[test]
    fn parses_a_realistic_listing() {
        // Mirrors output captured from a real fixture repository.
        let input = [
            branch("feat/broken", "aaa", "", "", "", "1786182744", "merge"),
            branch(
                "feat/dirty",
                "bbb",
                "origin/feat/dirty",
                "[ahead 1]",
                "refs/remotes/origin/feat/dirty",
                "1786182744",
                "unpushed",
            ),
            branch(
                "feat/gone",
                "ccc",
                "origin/feat/gone",
                "[gone]",
                "refs/remotes/origin/feat/gone",
                "1786182744",
                "work",
            ),
            branch(
                "main",
                "ddd",
                "origin/main",
                "",
                "refs/remotes/origin/main",
                "1786182744",
                "merge",
            ),
            remote("origin/feat/dirty", "upstream-bbb"),
            remote("origin/main", "upstream-ddd"),
        ]
        .join("\n");

        let got = parse_for_each_ref(input.as_bytes());
        assert_eq!(got.len(), 4, "remote-tracking refs are not branches");

        let dirty = &got["feat/dirty"];
        assert_eq!(dirty.upstream.as_deref(), Some("origin/feat/dirty"));
        assert_eq!(dirty.upstream_oid.as_deref(), Some("upstream-bbb"));
        assert_eq!(dirty.ahead, 1);
        assert!(!dirty.gone);

        let gone = &got["feat/gone"];
        assert!(gone.gone);
        assert!(
            gone.upstream_oid.is_none(),
            "a deleted remote branch has no object to point at"
        );
        assert_eq!(gone.subject.as_deref(), Some("work"));

        let broken = &got["feat/broken"];
        assert!(broken.upstream.is_none());
        assert!(!broken.gone);

        assert_eq!(got["main"].committed_at, Some(1786182744));
    }

    #[test]
    fn subjects_containing_separators_survive() {
        // A commit subject may legitimately contain pipes, brackets, or tabs;
        // only NUL is impossible, which is why it is the field separator.
        let input = branch(
            "feat/x",
            "sha",
            "origin/feat/x",
            "[ahead 1]",
            "refs/remotes/origin/feat/x",
            "1700000000",
            "fix: handle a|b [ahead 9] and\ttabs",
        );
        let got = parse_for_each_ref(input.as_bytes());

        assert_eq!(
            got["feat/x"].subject.as_deref(),
            Some("fix: handle a|b [ahead 9] and\ttabs")
        );
        assert_eq!(got["feat/x"].ahead, 1);
    }

    #[test]
    fn branches_without_subjects_are_allowed() {
        let input = branch("feat/x", "sha", "", "", "", "1700000000", "");
        let got = parse_for_each_ref(input.as_bytes());

        assert!(got["feat/x"].subject.is_none());
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let input = format!(
            "{}\n{}",
            "garbage",
            branch("feat/ok", "sha", "", "", "", "1700000000", "s")
        );
        let got = parse_for_each_ref(input.as_bytes());

        assert_eq!(got.len(), 1);
        assert!(got.contains_key("feat/ok"));
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert!(parse_for_each_ref(b"").is_empty());
    }

    /// A fetch refspec may put tracking refs anywhere. `refs/pr/*` and
    /// `refs/upstream/*` are ordinary configurations, and the batched listing
    /// only walks `refs/heads/` and `refs/remotes/`, so the upstream commit is
    /// simply not in it. Reading that absence as "no upstream object" made a
    /// branch whose upstream had moved look untouched.
    #[test]
    fn an_upstream_outside_the_listed_namespaces_is_flagged_for_lookup() {
        let input = branch(
            "feat/pr",
            "aaa",
            "pr/42",
            "[ahead 1]",
            "refs/pr/42",
            "1700000000",
            "work",
        );

        let got = parse_for_each_ref(input.as_bytes());
        let info = &got["feat/pr"];

        assert_eq!(info.upstream_ref.as_deref(), Some("refs/pr/42"));
        assert!(
            info.upstream_oid.is_none(),
            "the listing did not contain it"
        );
        assert!(
            info.upstream_unresolved,
            "so it is unresolved, not resolved-to-nothing"
        );
    }

    /// A branch whose upstream was deleted has nothing to look up. Marking it
    /// unresolved would refuse every removal of a branch with a gone upstream,
    /// which is the ordinary case this app exists for.
    #[test]
    fn a_gone_upstream_is_not_something_to_look_up() {
        let input = branch(
            "feat/gone",
            "aaa",
            "origin/feat/gone",
            "[gone]",
            "refs/remotes/origin/feat/gone",
            "1700000000",
            "work",
        );

        let info = &parse_for_each_ref(input.as_bytes())["feat/gone"];

        assert!(info.gone);
        assert!(!info.upstream_unresolved);
    }

    /// An upstream inside the listed namespaces is resolved by the one call,
    /// which is what keeps the ordinary repository at a single listing.
    #[test]
    fn an_upstream_the_listing_covered_needs_no_lookup() {
        let input = [
            branch(
                "feat/x",
                "aaa",
                "origin/feat/x",
                "",
                "refs/remotes/origin/feat/x",
                "1700000000",
                "work",
            ),
            remote("origin/feat/x", "upstream-aaa"),
        ]
        .join("\n");

        let info = &parse_for_each_ref(input.as_bytes())["feat/x"];

        assert_eq!(info.upstream_oid.as_deref(), Some("upstream-aaa"));
        assert!(!info.upstream_unresolved);
    }

    #[test]
    fn ref_oid_listings_map_full_names_to_commits() {
        let input = "refs/pr/42\u{0}aaa\nrefs/upstream/main\u{0}bbb\n";

        let got = parse_ref_oids(input.as_bytes());

        assert_eq!(got["refs/pr/42"], "aaa");
        assert_eq!(got["refs/upstream/main"], "bbb");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn ref_oid_listings_skip_rows_that_name_nothing() {
        let input = "refs/pr/42\u{0}\n\u{0}bbb\ngarbage\nrefs/pr/43\u{0}ccc\n";

        let got = parse_ref_oids(input.as_bytes());

        assert_eq!(got.len(), 1);
        assert_eq!(got["refs/pr/43"], "ccc");
    }
}
