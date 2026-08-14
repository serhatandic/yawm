/**
 * The paths the Changes view counted but could not draw, by name.
 *
 * The summary says "63 not diffable" and the list stays text-only, which is
 * what makes it readable — sixty-three cards each saying there was nothing to
 * read is the list this view deliberately does not draw. But a number with no
 * way to see what it stands for cannot be checked: a reader deciding whether
 * deleting a worktree loses something has to be able to ask *which* paths, and
 * until now the only answer was a tooltip counting kinds.
 *
 * So the names live here, behind a deliberate action, in the same vocabulary
 * the summary uses. Nothing in this module invents a fact: every row is one
 * typed entry the backend sent, a directory or a nested repository is stated
 * once with the number of paths it covers rather than expanded into child
 * names yawm never received, and paths a limit removed before they arrived are
 * reported as unavailable instead of being quietly left out of a list that
 * claims to be complete.
 */

import {
  COMMITTED_HEADING,
  ON_DISK_HEADING,
  coverageOf,
  entryKindLabel,
  isTextEntry,
  pathsCovered,
  residualPaths,
  unionCoverage,
} from "./diff-sections.ts";
import type {
  ChangeOrigin,
  DiffEntry,
  EntryContent,
  Patches,
} from "../lib/api.ts";

/** The dialog's own title, so the trigger and the heading cannot disagree. */
export const NOT_DIFFABLE_TITLE = "Files without line-by-line diffs";

/** The scroll region's own name, distinct from the dialog's title. */
export const NOT_DIFFABLE_LIST_LABEL =
  "Paths without line-by-line diffs, grouped by where the change lives";

export const NOT_DIFFABLE_DESCRIPTION =
  "These paths changed in this worktree but could not be compared as text, so they are counted in the summary rather than drawn in the diff.";

/**
 * How many rows the disclosure will mount.
 *
 * A worktree can hold tens of thousands of untracked non-text paths, and a
 * dialog that mounts one element for each of them is the wall this view
 * removed, reopened in a smaller box. Past this point the remainder is stated
 * as an exact count instead — a number the reader can still act on with the
 * command line — rather than as a scroll region no one reaches the end of.
 */
export const MAX_DISCLOSED_ENTRIES = 200;

/** One omitted entry, named and explained. */
export interface NotDiffableRow {
  path: string;
  /** The kind, spelled as the summary's breakdown spells it. */
  reason: string;
  /** Why there are no lines, and what the entry stands for. */
  detail: string;
  /** Raw Git paths behind this single row. */
  paths: number;
}

/** One Changes origin's share of them, under the heading it already has. */
export interface NotDiffableGroup {
  origin: ChangeOrigin;
  heading: string;
  rows: NotDiffableRow[];
  /** Entries in this group, including any the cap kept off screen. */
  entries: number;
  /** Entries in this group the cap kept off screen. */
  hidden: number;
}

export interface NotDiffableDisclosure {
  groups: NotDiffableGroup[];
  /** Non-text entries across every group. */
  entries: number;
  /** Rows actually built. */
  listed: number;
  /** Entries the cap left unbuilt, stated exactly. */
  hidden: number;
  /**
   * Raw Git paths counted as not diffable, by the summary's own arithmetic.
   *
   * A union across the groups rather than a sum of the rows: the same path
   * committed and edited again since is two entries to list and one path, and
   * a path drawn as a text diff in either group is not a path this view failed
   * to draw at all. Computed from `coverageOf` so this number and the "not
   * diffable" count on the summary line can never be two different numbers.
   */
  paths: number;
  /**
   * Paths this view was told about and never received, so their names cannot
   * be listed at all. Carried separately from `hidden`, which is a rendering
   * decision this module made and could undo.
   */
  residual: number;
}

const count = (n: number) => n.toLocaleString("en-US");

const plural = (n: number, one: string, many: string) =>
  `${count(n)} ${n === 1 ? one : many}`;

/**
 * What one entry covers, in raw Git paths, when that is more than itself.
 *
 * A nested repository is one row and eighteen paths, and a reader checking
 * this list against the summary's total needs the second number to make the
 * arithmetic close.
 */
function coverage(content: EntryContent): string {
  const paths = pathsCovered(content);
  return `covers ${plural(paths, "path", "paths")}`;
}

/**
 * Why this path has no diff, said in facts the backend actually sent.
 *
 * Never a guess about the contents: an untracked directory arrives as a count
 * of paths, not as a list of names, so this says how many it covers and stops
 * there rather than inventing children to fill the row out.
 */
export function notDiffableDetail(content: EntryContent): string {
  switch (content.kind) {
    case "text":
      // Unreachable through `notDiffableRows`, which filters text entries out
      // before building a row. Stated rather than thrown so a future caller
      // gets a sentence instead of a crash.
      return "This path was compared line by line.";
    case "binary":
      return "Compared as bytes rather than as lines, so there is no text diff to show.";
    case "empty":
      return "The file holds no lines to compare.";
    case "symlink":
      return `A link recording a target rather than text. It points at ${content.target}.`;
    case "directory":
      return content.items === null
        ? `An untracked directory, ${coverage(content)}. Its contents are counted here, not listed.`
        : `An untracked directory holding ${plural(content.items, "item", "items")}, ${coverage(content)}. Its contents are counted here, not listed.`;
    case "repository":
      return `A repository of its own inside this worktree, ${coverage(content)}. Git does not compare across that boundary, so its files are counted here, not listed.`;
    case "metadata":
      return `Git recorded a change that moves no lines: ${content.detail}`;
    case "unread":
      return `Named and counted, but not read: ${content.detail}`;
  }
}

/** One row for one non-text entry. Text entries have a diff, so they have none. */
function rowFor(entry: DiffEntry): NotDiffableRow {
  return {
    path: entry.path,
    reason: entryKindLabel(entry).one,
    detail: notDiffableDetail(entry),
    paths: pathsCovered(entry),
  };
}

/** The rows for one origin, in the order the backend sent its entries. */
export function notDiffableRows(entries: DiffEntry[]): NotDiffableRow[] {
  return entries.filter((entry) => !isTextEntry(entry)).map(rowFor);
}

/**
 * One origin's share, built only as far as the budget reaches.
 *
 * Everything past the budget is counted rather than constructed: an untracked
 * tree can hold tens of thousands of non-text paths, and building a row object
 * for each of them on every render costs the whole list its responsiveness to
 * produce strings nothing will ever mount.
 *
 * `omitted` decides which entries belong here at all. A path that is a binary
 * on disk and a text diff in the committed group was drawn — the reader can
 * read it in the view — so it is not something this dialog has to reveal, and
 * listing it would put a row here that the summary's count does not include.
 */
function groupFor(
  origin: ChangeOrigin,
  heading: string,
  entries: DiffEntry[],
  omitted: (path: string) => boolean,
  room: number,
): { group: NotDiffableGroup | null; entries: number } {
  const rows: NotDiffableRow[] = [];
  let count = 0;

  for (const entry of entries) {
    if (isTextEntry(entry)) continue;
    if (!omitted(entry.path)) continue;
    count += 1;
    if (rows.length < room) rows.push(rowFor(entry));
  }

  if (count === 0) return { group: null, entries: 0 };
  return {
    group: { origin, heading, rows, entries: count, hidden: count - rows.length },
    entries: count,
  };
}

/**
 * The paths behind these entries, counted exactly as the summary counts them.
 *
 * Deliberately not the sum of the rows: two groups holding the same path are
 * two rows and one path, and adding them would make the dialog claim a larger
 * omission than the line the reader pressed to open it.
 */
export function notDiffablePathTotal(patches: Patches): number {
  let paths = 0;
  for (const cover of pathIdentities(patches).values()) {
    if (!cover.diffable) paths += cover.paths;
  }
  return paths;
}

/** Every path identity across both groups, unioned as the summary unions it. */
function pathIdentities(patches: Patches) {
  return unionCoverage([
    coverageOf(patches.uncommitted),
    coverageOf(patches.committed),
  ]);
}

/**
 * Everything the Changes view counted and did not draw, ready to render.
 *
 * Grouped by the same two origins the view itself is grouped by, and in the
 * same order: what is only on disk comes first, because that is the half
 * deleting the worktree destroys.
 */
export function notDiffableDisclosure(
  patches: Patches,
  cap: number = MAX_DISCLOSED_ENTRIES,
): NotDiffableDisclosure {
  const union = pathIdentities(patches);
  const omitted = (path: string) => union.get(path)?.diffable === false;

  const groups: NotDiffableGroup[] = [];
  let listed = 0;
  let entries = 0;
  let hidden = 0;
  let paths = 0;
  for (const cover of union.values()) {
    if (!cover.diffable) paths += cover.paths;
  }

  const sources: [ChangeOrigin, string, DiffEntry[]][] = [
    ["uncommitted", ON_DISK_HEADING, patches.uncommitted],
    ["committed", COMMITTED_HEADING, patches.committed],
  ];

  for (const [origin, heading, source] of sources) {
    // The budget is spent across the groups in order rather than divided
    // between them: a worktree whose omissions are all on disk should show as
    // many of those as the cap allows, not half of it.
    const found = groupFor(
      origin,
      heading,
      source,
      omitted,
      Math.max(0, cap - listed),
    );
    entries += found.entries;
    if (!found.group) continue;
    listed += found.group.rows.length;
    hidden += found.group.hidden;
    groups.push(found.group);
  }

  return { groups, entries, listed, hidden, paths, residual: residualPaths(patches) };
}

/**
 * The count on the trigger's accessible name.
 *
 * It names the number the reader is looking at on the summary line — raw Git
 * paths — so pressing it cannot appear to open something about a different
 * quantity.
 */
export function notDiffableTriggerLabel(notDiffable: number): string {
  return `Show the ${plural(notDiffable, "path", "paths")} without line-by-line diffs`;
}

/**
 * The scale of what the dialog holds, stated before the reader scrolls it.
 *
 * Two quantities, because they are genuinely two: the paths counted as not
 * diffable — the number on the summary line the reader pressed — and the
 * entries listed below, which can be more than the paths when one path is
 * omitted in both groups, and fewer when one entry stands for a directory. A
 * path drawn as a text diff in the other group is listed here but was never
 * counted as not diffable, so a zero there is stated as entries alone rather
 * than as a path count contradicting the row underneath it.
 */
export function disclosureScale(disclosure: NotDiffableDisclosure): string {
  const entries = plural(disclosure.entries, "entry", "entries");
  if (disclosure.paths === 0) return `${entries} without a text diff`;
  if (disclosure.paths === disclosure.entries) {
    return `${plural(disclosure.paths, "path", "paths")} without a text diff`;
  }
  return `${plural(disclosure.paths, "path", "paths")} without a text diff, in ${entries}`;
}

/**
 * What the cap left out, exactly, and never rounded.
 *
 * "and more" is the shape of statement this app does not make: a reader who
 * cannot see the rest still needs to know how much of it there is.
 */
export function disclosureCapNote(
  disclosure: NotDiffableDisclosure,
): string | null {
  if (disclosure.hidden === 0) return null;
  return `${plural(disclosure.hidden, "more entry is", "more entries are")} not listed here. This view lists at most ${count(MAX_DISCLOSED_ENTRIES)}.`;
}

/**
 * The names this view never had, said plainly.
 *
 * A limit that stopped the listing did not just hide these paths from the
 * dialog — it means yawm was told how many there were and never received what
 * they were called. Without this sentence the list would read as complete, and
 * a reader would conclude the worktree holds only what is named above it.
 */
export function disclosureResidualNote(
  disclosure: NotDiffableDisclosure,
): string | null {
  if (disclosure.residual === 0) return null;
  return `A limit stopped the listing before ${plural(disclosure.residual, "further path", "further paths")} arrived, so ${disclosure.residual === 1 ? "its name is" : "their names are"} not available to show here.`;
}

/** Nothing to open means no trigger, rather than a dialog listing nothing. */
export function hasDisclosure(disclosure: NotDiffableDisclosure): boolean {
  return disclosure.entries > 0;
}
