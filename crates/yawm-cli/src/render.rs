//! Terminal rendering.
//!
//! Formatting only: every judgement shown here comes from `yawm-core`, so the
//! CLI and the GUI can never disagree about what is disposable.

use std::collections::HashMap;
use std::fmt::Write;
use std::path::{Component, Path};

use yawm_core::{RepoReport, Verdict, Worktree};

/// ANSI colours, suppressed when output is redirected or NO_COLOR is set.
struct Style {
    enabled: bool,
}

impl Style {
    fn detect() -> Self {
        let disabled = std::env::var_os("NO_COLOR").is_some()
            || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);
        Self { enabled: !disabled }
    }

    fn paint(&self, code: &str, text: &str) -> String {
        if self.enabled {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    fn verdict(&self, verdict: Verdict) -> String {
        let (code, label) = verdict_style(verdict);
        self.paint(code, label)
    }
}

/// Colour and label for a verdict, kept separate so callers can pad using the
/// visible width rather than the escaped string, which is several bytes longer.
fn verdict_style(verdict: Verdict) -> (&'static str, &'static str) {
    match verdict {
        Verdict::Disposable => ("32", "disposable"),
        Verdict::Review => ("33", "review"),
        Verdict::Keep => ("34", "keep"),
        Verdict::Broken => ("31", "broken"),
    }
}

/// Print every repository and a closing summary.
pub fn print(reports: &[RepoReport], disposable_only: bool, size_measured: bool) {
    let style = Style::detect();
    print!(
        "{}",
        render_with_style(reports, disposable_only, size_measured, &style)
    );
}

/// Produce the complete terminal report. Keeping formatting separate from
/// stdout makes filtering and uncertainty wording directly testable.
fn render_with_style(
    reports: &[RepoReport],
    disposable_only: bool,
    size_measured: bool,
    style: &Style,
) -> String {
    let mut output = String::new();
    if reports.is_empty() {
        output.push_str("No repositories found.\n");
        return output;
    }

    let mut total_bytes = 0;
    let mut reclaimable = 0;
    let mut shown = 0;
    let total_worktrees = reports.iter().map(|r| r.worktrees.len()).sum::<usize>();

    for report in reports {
        let worktrees: Vec<&Worktree> = report
            .worktrees
            .iter()
            .filter(|w| !disposable_only || w.verdict == Verdict::Disposable)
            .collect();

        total_bytes += report.total_bytes();
        reclaimable += report.reclaimable_bytes();

        if worktrees.is_empty() {
            continue;
        }
        shown += worktrees.len();

        writeln!(
            output,
            "\n{} {}",
            style.bold(&report.name),
            style.dim(&report.root.display().to_string())
        )
        .unwrap();

        let labels = display_labels(&worktrees);
        let width = labels
            .iter()
            .map(|label| label.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(8, 40);

        for (worktree, label) in worktrees.into_iter().zip(labels) {
            write_worktree(&mut output, worktree, &label, width, style);
        }
    }

    if shown == 0 {
        output.push_str("\nNothing to show.\n");
    }

    let count_summary = if disposable_only {
        format!("{shown} disposable shown · {total_worktrees} worktrees total")
    } else {
        format!("{total_worktrees} worktrees")
    };
    if size_measured {
        writeln!(
            output,
            "\n{} · {} total · {} reclaimable",
            count_summary,
            human_bytes(total_bytes),
            style.bold(&human_bytes(reclaimable)),
        )
        .unwrap();
    } else {
        writeln!(
            output,
            "\n{count_summary} · size skipped · reclaimable unknown"
        )
        .unwrap();
    }

    output
}

fn write_worktree(
    output: &mut String,
    worktree: &Worktree,
    label: &str,
    width: usize,
    style: &Style,
) {
    let label = truncate_label(label, width);

    let size = worktree
        .status
        .size
        .as_ref()
        .map(|s| human_bytes(s.bytes))
        .unwrap_or_else(|| "—".to_string());

    // Pad on the visible label, not the escaped string: ANSI codes add bytes
    // that format!'s width would otherwise count, breaking the columns.
    let (_, verdict_label) = verdict_style(worktree.verdict);
    let verdict_pad = " ".repeat(VERDICT_WIDTH.saturating_sub(verdict_label.len()));

    writeln!(
        output,
        "  {}{} {:<width$}  {:>9}  {:>6}  {}",
        style.verdict(worktree.verdict),
        verdict_pad,
        label,
        size,
        style.dim(&relative_time(worktree.status.last_commit_at)),
        style.dim(&badges(worktree)),
        width = width,
    )
    .unwrap();
}

/// Branches are normally sufficient. Add the shortest unique path suffix only
/// for labels that collide, including detached-HEAD labels.
fn display_labels(worktrees: &[&Worktree]) -> Vec<String> {
    let mut labels: Vec<String> = worktrees.iter().map(|worktree| worktree.label()).collect();
    let mut collisions: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, label) in labels.iter().enumerate() {
        collisions.entry(label.clone()).or_default().push(index);
    }

    for indices in collisions.values().filter(|indices| indices.len() > 1) {
        let suffixes = unique_path_suffixes(worktrees, indices);
        for (&index, suffix) in indices.iter().zip(suffixes) {
            labels[index] = format!("{} [{suffix}]", labels[index]);
        }
    }
    labels
}

fn unique_path_suffixes(worktrees: &[&Worktree], indices: &[usize]) -> Vec<String> {
    let parts: Vec<Vec<String>> = indices
        .iter()
        .map(|&index| path_parts(&worktrees[index].entry.path))
        .collect();
    let max_depth = parts.iter().map(Vec::len).max().unwrap_or(1);

    for depth in 1..=max_depth {
        let suffixes: Vec<String> = parts
            .iter()
            .map(|parts| parts[parts.len().saturating_sub(depth)..].join("/"))
            .collect();
        let mut seen = HashMap::new();
        if suffixes
            .iter()
            .all(|suffix| seen.insert(suffix, ()).is_none())
        {
            return suffixes;
        }
    }

    parts
        .iter()
        .enumerate()
        .map(|(index, parts)| format!("{}#{}", parts.join("/"), index + 1))
        .collect()
}

fn path_parts(path: &Path) -> Vec<String> {
    let mut parts: Vec<String> = path
        .components()
        .filter_map(|component| match component {
            Component::Prefix(prefix) => Some(prefix.as_os_str().to_string_lossy().into_owned()),
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_string()),
            Component::RootDir | Component::CurDir => None,
        })
        .collect();
    if parts.is_empty() {
        parts.push(path.display().to_string());
    }
    parts
}

fn truncate_label(label: &str, width: usize) -> String {
    if label.chars().count() <= width {
        return label.to_string();
    }

    if let Some((branch, suffix)) = label.rsplit_once(" [") {
        let tail = format!(" [{suffix}");
        let tail_width = tail.chars().count();
        if tail_width + 1 < width {
            let branch_width = width - tail_width - 1;
            return format!(
                "{}…{tail}",
                branch.chars().take(branch_width).collect::<String>()
            );
        }
    }

    format!(
        "{}…",
        label
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
    )
}

/// Width of the verdict column, sized to the longest label.
const VERDICT_WIDTH: usize = 10;

/// Short flags describing why a worktree looks the way it does.
fn badges(worktree: &Worktree) -> String {
    let mut parts = vec![worktree.reason.describe()];

    let dirty = &worktree.status.dirty;
    if dirty.is_dirty() {
        parts.push(format!("{} changed", dirty.total()));
    }
    let upstream = &worktree.status.upstream;
    if upstream.ahead > 0 {
        parts.push(format!("↑{}", upstream.ahead));
    }
    if upstream.behind > 0 {
        parts.push(format!("↓{}", upstream.behind));
    }
    if !worktree.status.env_files.is_empty() {
        // Gitignored, so deleting the worktree destroys them permanently.
        parts.push(format!("{} env", worktree.status.env_files.len()));
    }
    if !worktree.status.processes.is_empty() {
        parts.push(format!("{} running", worktree.status.processes.len()));
    }

    parts.join(" · ")
}

/// Bytes in the largest unit that keeps the number readable.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Compact age, e.g. `4m`, `6d`. Empty when unknown.
pub fn relative_time(timestamp: Option<i64>) -> String {
    let Some(timestamp) = timestamp else {
        return "—".to_string();
    };
    let now = yawm_core::api::now_unix();
    let seconds = now.saturating_sub(timestamp).max(0);

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    match seconds {
        s if s < MINUTE => "just now".to_string(),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < MONTH => format!("{}d", s / DAY),
        s if s < YEAR => format!("{}mo", s / MONTH),
        s => format!("{}y", s / YEAR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use yawm_core::{VerdictReason, WorktreeEntry, WorktreeStatus};

    fn worktree(path: &str, branch: Option<&str>, verdict: Verdict) -> Worktree {
        Worktree {
            entry: WorktreeEntry {
                path: PathBuf::from(path),
                branch: branch.map(str::to_string),
                ..WorktreeEntry::default()
            },
            status: WorktreeStatus::default(),
            verdict,
            reason: VerdictReason::LandingUnknown {
                facts: Box::default(),
            },
        }
    }

    fn report(worktrees: Vec<Worktree>) -> RepoReport {
        RepoReport {
            name: "repo".to_string(),
            root: PathBuf::from("/code/repo"),
            default_ref: None,
            worktrees,
        }
    }

    fn plain_render(reports: &[RepoReport], disposable_only: bool, size_measured: bool) -> String {
        render_with_style(
            reports,
            disposable_only,
            size_measured,
            &Style { enabled: false },
        )
    }

    #[test]
    fn formats_bytes_readably() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn drops_the_decimal_when_the_number_is_large() {
        assert_eq!(human_bytes(900 * 1024 * 1024), "900 MB");
    }

    #[test]
    fn unknown_timestamps_render_as_a_dash() {
        assert_eq!(relative_time(None), "—");
    }

    #[test]
    fn formats_ages_compactly() {
        let now = yawm_core::api::now_unix();
        assert_eq!(relative_time(Some(now)), "just now");
        assert_eq!(relative_time(Some(now - 300)), "5m");
        assert_eq!(relative_time(Some(now - 7200)), "2h");
        assert_eq!(relative_time(Some(now - 3 * 86400)), "3d");
    }

    /// A clock skewed into the future must not produce a negative age.
    #[test]
    fn future_timestamps_do_not_underflow() {
        let now = yawm_core::api::now_unix();
        assert_eq!(relative_time(Some(now + 10_000)), "just now");
    }

    #[test]
    fn colour_can_be_disabled() {
        let plain = Style { enabled: false };
        assert_eq!(plain.verdict(Verdict::Disposable), "disposable");
        assert!(!plain.dim("x").contains('\x1b'));
    }

    #[test]
    fn filtered_summary_distinguishes_shown_and_total_counts() {
        let report = report(vec![
            worktree("/code/repo", Some("main"), Verdict::Keep),
            worktree("/code/one", Some("one"), Verdict::Disposable),
            worktree("/code/two", Some("two"), Verdict::Review),
        ]);

        let output = plain_render(&[report], true, true);

        assert!(output.contains("1 disposable shown · 3 worktrees total"));
        assert!(!output.contains("\n3 worktrees ·"));
    }

    #[test]
    fn skipped_measurement_never_renders_zero_size_totals() {
        let report = report(vec![worktree("/code/repo", Some("main"), Verdict::Keep)]);

        let output = plain_render(&[report], false, false);

        assert!(output.contains("size skipped · reclaimable unknown"));
        assert!(!output.contains("0 B total"));
        assert!(!output.contains("0 B reclaimable"));
    }

    #[test]
    fn duplicate_labels_get_unique_concise_path_suffixes() {
        let report = report(vec![
            worktree("/code/alpha/topic", Some("feature"), Verdict::Keep),
            worktree("/archive/beta/topic", Some("feature"), Verdict::Review),
            worktree("/code/normal", Some("main"), Verdict::Keep),
        ]);

        let output = plain_render(&[report], false, true);

        assert!(output.contains("feature [alpha/topic]"));
        assert!(output.contains("feature [beta/topic]"));
        assert!(output.contains(" main "));
        assert!(!output.contains("main ["));
    }
}
