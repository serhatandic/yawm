import type {
  ChangeOrigin,
  ChangesBalance,
  DiffEntry,
  EntryContent,
  Patches,
  RepositoryKind,
} from "@/lib/api";
import type { FileEntry, FileStat } from "@/components/file-tree";

/**
 * A section is a patch. There is no other kind.
 *
 * This list is called a diff, so every row in it is a diff. Rows that were not
 * — a binary file, an empty untracked file, a symlink, a directory, a nested
 * repository, a mode change, a path that could not be read — used to be drawn
 * as cards: a filename, a badge, and a sentence saying there was nothing to
 * read. Sixty-three of those between two patches is a list the reader has to
 * filter by eye to find the thing they opened it for.
 *
 * The facts are not lost. They are counted, by kind, and stated once as a
 * quantity beside the scope's totals — which is something a reader can act on
 * — instead of sixty-three rows each restating the same limitation about a
 * different path.
 */
export interface DiffSectionModel {
  id: string;
  anchor: string;
  path: string;
  patch: string;
  stat: FileStat;
}

/** The one entry shape that carries lines to render. */
export type TextEntry = DiffEntry & { kind: "text" };

export function isTextEntry(entry: DiffEntry): entry is TextEntry {
  return entry.kind === "text";
}

/** The entries that become rows. */
export function textEntries(entries: DiffEntry[]): TextEntry[] {
  return entries.filter(isTextEntry);
}

/** The entries that become a number. */
export function nonTextEntries(entries: DiffEntry[]): DiffEntry[] {
  return entries.filter((entry) => !isTextEntry(entry));
}

/**
 * Which mounted Changes view an anchor belongs to.
 *
 * Every open tab stays mounted and only the active one is visible — that is
 * what makes returning to a tab instant instead of a refetch. It also means a
 * bare `changes-group-uncommitted` existed once per open worktree, and a
 * document-wide lookup handed every click whichever of them mounted first: a
 * click on one worktree's dirty count scrolled a different, hidden worktree
 * and left the visible one exactly where it was.
 *
 * The scope is the worktree path, which is already the tab's identity, so two
 * views can never disagree about which anchors are theirs.
 */
export type AnchorScope = string;

export const anchorScope = (worktree: string): AnchorScope =>
  encodeURIComponent(worktree);

/**
 * Anchors the tree scrolls to. Derived from the path so both sides agree.
 *
 * Namespaced by origin because a file committed and then edited again appears
 * in both groups, and two elements sharing an id would send every scroll to
 * whichever came first — and by scope, because every other open worktree is
 * mounted in the same document for exactly the same reason.
 */
export const anchorId = (
  scope: AnchorScope,
  origin: ChangeOrigin,
  path: string,
) => `diff-${scope}-${origin}-${encodeURIComponent(path)}`;

/**
 * The group heading itself, so a click that was about one group can land on
 * it. The dirty count promises work on disk and now scrolls to exactly that
 * heading instead of opening a different, narrower view of the worktree.
 */
export const groupAnchorId = (scope: AnchorScope, origin: ChangeOrigin) =>
  `changes-group-${scope}-${origin}`;

/** Somewhere to scroll to: the only thing this module wants from an element. */
export interface ScrollTarget {
  scrollIntoView(options?: { block?: "start"; behavior?: "auto" }): void;
}

/** The mounted view an anchor is looked for inside, never the whole document. */
export interface AnchorRoot {
  querySelector(selector: string): ScrollTarget | null;
}

/**
 * The anchor, found within one mounted view.
 *
 * Scoped rather than global even though the ids are namespaced: the two
 * together are what make a click land on the tab the reader is looking at.
 * Ids come from `anchorId` and `groupAnchorId`, whose variable parts are
 * percent-encoded, so they are safe inside an attribute selector.
 */
export function anchorTarget(
  root: AnchorRoot | null,
  id: string,
): ScrollTarget | null {
  return root ? root.querySelector(`[id="${id}"]`) : null;
}

/** Scroll to an anchor in this view, and say whether there was one. */
export function scrollToAnchor(root: AnchorRoot | null, id: string): boolean {
  const target = anchorTarget(root, id);
  if (!target) return false;
  target.scrollIntoView({ block: "start", behavior: "auto" });
  return true;
}

/** What to print beside a text row, in the units its group is counting. */
export function textStat(group: {
  counting: string;
  atRisk: boolean;
  counts: { insertions: number; deletions: number } | null;
}): FileStat {
  if (!group.counts) {
    return {
      kind: "unknown",
      label: "whole file",
      title:
        "This file could not be compared line by line, so it is shown in full.",
    };
  }
  return {
    kind: "counts",
    insertions: group.counts.insertions,
    deletions: group.counts.deletions,
    tone: group.atRisk ? "risk" : "change",
    title: group.counting,
  };
}

/** One rendered section per text entry. */
export function sectionFor(
  scope: AnchorScope,
  entry: TextEntry,
  origin: ChangeOrigin,
  group: {
    counting: string;
    atRisk: boolean;
    counts: { insertions: number; deletions: number } | null;
    /** At-risk mode rewrites a patch down to the lines that matter. */
    narrow?: (patch: string) => string;
  },
): DiffSectionModel {
  const anchor = anchorId(scope, origin, entry.path);
  return {
    id: anchor,
    anchor,
    path: entry.path,
    patch: group.narrow ? group.narrow(entry.patch) : entry.patch,
    stat: textStat(group),
  };
}

export function fileEntries(sections: DiffSectionModel[]): FileEntry[] {
  return sections.map((section) => ({
    path: section.path,
    stat: section.stat,
  }));
}

/* ------------------------------------------------------------------ *
 * What was left out, counted in the units Git counts in.
 * ------------------------------------------------------------------ */

/**
 * One kind of omitted thing, and how much of it there was.
 *
 * Both spellings are carried rather than one pre-pluralised string, because
 * two groups on one screen add their counts together: a scope with one binary
 * file in each group holds two, and a label frozen at "binary file" when its
 * half was counted would still say so after the addition.
 */
export interface OmittedKind {
  one: string;
  many: string;
  entries: number;
  /** Raw Git paths those entries stand for. */
  paths: number;
}

/** The kind's name, agreeing with the count standing beside it. */
export function kindLabel(kind: OmittedKind): string {
  return kind.entries === 1 ? kind.one : kind.many;
}

/**
 * Everything the list did not draw, as arithmetic.
 *
 * Two numbers, because these are genuinely two quantities: a nested repository
 * is one entry here and eighteen paths to Git. Reporting only the entry count
 * makes the worktree row's total look wrong; reporting only the path count
 * makes the list look as though it dropped seventeen rows.
 */
export interface Omitted {
  entries: number;
  paths: number;
  kinds: OmittedKind[];
}

export const NO_OMISSIONS: Omitted = { entries: 0, paths: 0, kinds: [] };

/**
 * Never "submodule": a directory with its own `.git` is not necessarily
 * registered in `.gitmodules`, and yawm does not check. Each label says what
 * was observed — a nested repository, or a linked worktree when Git named one.
 */
const REPOSITORY_LABEL: Record<RepositoryKind, { one: string; many: string }> = {
  nested: { one: "nested repository", many: "nested repositories" },
  linkedWorktree: { one: "linked worktree", many: "linked worktrees" },
  bare: { one: "bare repository", many: "bare repositories" },
};

/**
 * The one place a kind of entry is named.
 *
 * Exported because the disclosure that lists the omitted paths one by one has
 * to call each of them what the summary above it called them collectively. A
 * second table of labels would let the two drift, and a reader comparing "12
 * nested repositories" against a list saying "submodule" cannot tell whether
 * they are looking at the same twelve things.
 */
export function entryKindLabel(content: EntryContent): {
  one: string;
  many: string;
} {
  switch (content.kind) {
    case "text":
      return { one: "text diff", many: "text diffs" };
    case "binary":
      return { one: "binary file", many: "binary files" };
    case "empty":
      return { one: "empty file", many: "empty files" };
    case "symlink":
      return { one: "symbolic link", many: "symbolic links" };
    case "directory":
      return { one: "untracked directory", many: "untracked directories" };
    case "repository":
      return REPOSITORY_LABEL[content.repository];
    case "metadata":
      return { one: "metadata-only change", many: "metadata-only changes" };
    case "unread":
      return { one: "unreadable path", many: "unreadable paths" };
  }
}

/**
 * How many raw Git paths one entry stands for.
 *
 * A directory and a repository arrive as one row covering many paths and carry
 * that number. Everything else is one path, and the count is floored at one so
 * a backend that ever reported zero could not make an omitted entry vanish
 * from the total.
 */
export function pathsCovered(content: EntryContent): number {
  if (content.kind === "directory" || content.kind === "repository") {
    return Math.max(1, content.paths);
  }
  return 1;
}

/** Counted from the typed entries themselves, never from a rendered row. */
export function omittedFrom(entries: DiffEntry[]): Omitted {
  const kinds = new Map<string, OmittedKind>();
  let total = 0;
  let rawPaths = 0;

  for (const entry of entries) {
    if (isTextEntry(entry)) continue;
    const { one, many } = entryKindLabel(entry);
    const paths = pathsCovered(entry);
    total += 1;
    rawPaths += paths;
    const existing = kinds.get(one);
    if (existing) {
      existing.entries += 1;
      existing.paths += paths;
    } else {
      kinds.set(one, { one, many, entries: 1, paths });
    }
  }

  return {
    entries: total,
    paths: rawPaths,
    kinds: [...kinds.values()],
  };
}

/** Two groups on one screen are one omission, so their counts add up. */
export function mergeOmitted(a: Omitted, b: Omitted): Omitted {
  if (a.entries === 0) return b;
  if (b.entries === 0) return a;
  const kinds = new Map<string, OmittedKind>();
  for (const kind of [...a.kinds, ...b.kinds]) {
    const existing = kinds.get(kind.one);
    if (existing) {
      existing.entries += kind.entries;
      existing.paths += kind.paths;
    } else {
      kinds.set(kind.one, { ...kind });
    }
  }
  return {
    entries: a.entries + b.entries,
    paths: a.paths + b.paths,
    kinds: [...kinds.values()],
  };
}

const count = (n: number) => n.toLocaleString("en-US");

/**
 * The clause that rides on the scope's summary line.
 *
 * `63 non-text paths omitted` when the two counts agree; when they do not, it
 * says so rather than quietly picking whichever number reads better. `63
 * non-text paths omitted in 46 entries` is the only spelling against which
 * both the worktree row's total and this list's row count can be checked.
 */
export function omittedClause(omitted: Omitted): string | null {
  if (omitted.entries === 0) return null;
  const paths = `${count(omitted.paths)} non-text ${
    omitted.paths === 1 ? "path" : "paths"
  } omitted`;
  if (omitted.paths === omitted.entries) return paths;
  return `${paths} in ${count(omitted.entries)} ${
    omitted.entries === 1 ? "entry" : "entries"
  }`;
}

/** Past this, a breakdown is a list rather than a summary. */
export const MAX_OMITTED_KINDS = 6;

/**
 * The breakdown, for a tooltip: kinds and counts, never filenames.
 *
 * A hover that dumps sixty-three paths is the row list this view just removed,
 * relocated into a smaller box. What a reader needs from it is which kind of
 * thing was left out and how much of it, so that is all it holds.
 */
export function omittedBreakdown(omitted: Omitted): string | null {
  if (omitted.entries === 0) return null;
  const ordered = [...omitted.kinds].sort((a, b) => b.paths - a.paths);
  const parts = ordered
    .slice(0, MAX_OMITTED_KINDS)
    .map((kind) =>
      kind.paths === kind.entries
        ? `${count(kind.entries)} ${kindLabel(kind)}`
        : `${count(kind.paths)} paths in ${count(kind.entries)} ${kindLabel(kind)}`,
    );
  const rest = ordered.slice(MAX_OMITTED_KINDS);
  if (rest.length > 0) {
    const remaining = rest.reduce((n, kind) => n + kind.entries, 0);
    parts.push(`${count(remaining)} more`);
  }
  return parts.join(" \u00B7 ");
}

/**
 * A short review should open ready to read. Past this point, open code turns
 * the overview into a wall and spends CPU before the reader chooses a file.
 */
export const COLLAPSE_THRESHOLD = 12;

export function defaultCollapsed(sections: DiffSectionModel[]): boolean {
  return sections.length > COLLAPSE_THRESHOLD;
}

/** Nothing to expand means no expand control, rather than a dead one. */
export function hasCollapsibleSections(sections: DiffSectionModel[]): boolean {
  return sections.length > 0;
}

/**
 * The two groups, named the same way everywhere they appear.
 *
 * One name each, in the tree and in the pane. There used to be five — "Uncommitted
 * Changes", "Branch History", "All commits", "On disk only", "Already on
 * default" — three of which named a *scope* the view no longer has, and two of
 * which renamed the same group depending on which column it was drawn in. A
 * reader cannot tell whether two differently-named lists are two views of one
 * thing or two different things, so there is exactly one name for each group.
 *
 * They say where the work lives, because that is the question being asked:
 * deleting the directory destroys what is only on disk and leaves what is
 * committed.
 */
export const ON_DISK_HEADING = "On disk only";
export const COMMITTED_HEADING = "Committed on this branch";

/** The view holds things, and not one of them is a diff. */
export const NO_LINE_DIFFS_TITLE = "No line-by-line diffs in these changes";

/**
 * Whether the narrowed reading is genuinely a different thing to read.
 *
 * A filter offering two segments that render the same paths with the same
 * content is a dead control: it costs a decision, spends a click, and changes
 * nothing on screen. The comparison is by path *and* by patch text, because
 * the at-risk reading usually keeps every file and rewrites what is inside it.
 */
export function readingNarrows(
  everything: DiffSectionModel[],
  atRisk: DiffSectionModel[],
): boolean {
  if (atRisk.length === 0) return false;
  if (atRisk.length !== everything.length) return true;
  const byPath = new Map(everything.map((section) => [section.path, section.patch]));
  return atRisk.some((section) => byPath.get(section.path) !== section.patch);
}

/** One group's contribution to the summary identity. */
export interface GroupBalance {
  textDiffs: number;
  insertions: number;
  deletions: number;
  omitted: Omitted;
  /** Which paths this group is accounting for, so two groups can be unioned. */
  coverage: CoverageMap;
}

/**
 * What one path identity stands for.
 *
 * `paths` because a nested repository is one identity covering eighteen raw
 * Git paths; `diffable` because the same identity can be a patch in one group
 * and an unreadable blob in the other, and a path drawn anywhere is not a path
 * the view failed to draw.
 */
export interface PathCoverage {
  paths: number;
  diffable: boolean;
}

export type CoverageMap = ReadonlyMap<string, PathCoverage>;

/**
 * Every path one group knows about, keyed by identity rather than counted.
 *
 * Counting was the bug: a file committed on this branch and edited again since
 * appears in both groups, correctly, as two rendered entries — and the summary
 * added the two group totals and told the reader the worktree held two changed
 * paths. It holds one, twice over.
 */
export function coverageOf(entries: DiffEntry[]): Map<string, PathCoverage> {
  const coverage = new Map<string, PathCoverage>();
  for (const entry of entries) {
    coverage.set(entry.path, coverPath(coverage.get(entry.path), entry));
  }
  return coverage;
}

function coverPath(
  existing: PathCoverage | undefined,
  entry: DiffEntry,
): PathCoverage {
  const diffable = isTextEntry(entry);
  const paths = diffable ? 1 : pathsCovered(entry);
  if (!existing) return { paths, diffable };
  return {
    // The widest claim about one identity wins: a directory standing for four
    // paths does not shrink to one because it was also seen as a single row.
    paths: Math.max(existing.paths, paths),
    diffable: existing.diffable || diffable,
  };
}

/** The same identity in two groups is one path, covering the wider of the two. */
export function unionCoverage(groups: CoverageMap[]): Map<string, PathCoverage> {
  const union = new Map<string, PathCoverage>();
  for (const coverage of groups) {
    for (const [path, cover] of coverage) {
      const existing = union.get(path);
      union.set(
        path,
        existing
          ? {
              paths: Math.max(existing.paths, cover.paths),
              diffable: existing.diffable || cover.diffable,
            }
          : cover,
      );
    }
  }
  return union;
}

/**
 * A group's arithmetic, taken from the typed entries themselves.
 *
 * Never from the summary's file list: that is a separate scan snapshot taken
 * at a different moment, and a line total borrowed from it cannot be checked
 * against the sections standing underneath it.
 */
export function balanceOf(
  sections: DiffSectionModel[],
  omitted: Omitted,
  lines: { insertions: number; deletions: number },
  coverage: CoverageMap,
): GroupBalance {
  return {
    textDiffs: sections.length,
    insertions: lines.insertions,
    deletions: lines.deletions,
    omitted,
    coverage,
  };
}

/** Lines a set of text entries changed, summed in the entries' own units. */
export function lineTotals(entries: TextEntry[]): {
  insertions: number;
  deletions: number;
} {
  return entries.reduce(
    (total, entry) => ({
      insertions: total.insertions + entry.insertions,
      deletions: total.deletions + entry.deletions,
    }),
    { insertions: 0, deletions: 0 },
  );
}

/**
 * Every visible group, added into the one identity the summary states.
 *
 * Rendered rows add up — a path committed and then edited again is genuinely
 * two things to read, in two groups, and both are drawn. Paths do not: the
 * changed-path total is a union of identities across the groups, so that same
 * path is one path however many groups hold it. The two numbers are printed
 * side by side, so if either were derived from the other the line would state
 * an identity that is not one.
 */
export function combineBalances(
  groups: GroupBalance[],
  residual: number,
): ChangesBalance {
  const union = unionCoverage(groups.map((group) => group.coverage));
  let changedPaths = 0;
  let notDiffable = 0;
  for (const cover of union.values()) {
    changedPaths += cover.paths;
    if (!cover.diffable) notDiffable += cover.paths;
  }
  return {
    textDiffs: groups.reduce((n, group) => n + group.textDiffs, 0),
    changedPaths,
    // From the union rather than from the merged omission, which counts a path
    // left undrawn in both groups twice.
    notDiffable,
    insertions: groups.reduce((n, group) => n + group.insertions, 0),
    deletions: groups.reduce((n, group) => n + group.deletions, 0),
    residual,
  };
}

/**
 * Paths this view was told about and never received.
 *
 * A limit that stops a listing does not make the paths behind it stop
 * existing, and without this the summary's identity would balance against
 * itself while quietly disagreeing with the worktree it describes. Taken as a
 * maximum rather than a sum: `displayLimit`, `inspectionLimit` and the byte
 * truncation all describe the same untracked shortfall from different sides,
 * and adding them would count one missing path three times.
 */
export function residualPaths(patches: Patches): number {
  let residual = 0;
  for (const limit of patches.limits) {
    if (limit.kind === "displayLimit" || limit.kind === "inspectionLimit") {
      residual = Math.max(residual, limit.total - limit.shown);
    }
  }
  if (patches.truncated) {
    residual = Math.max(
      residual,
      patches.untrackedTotal - patches.untrackedShown,
    );
  }
  return Math.max(0, residual);
}
