/**
 * What "select all" is allowed to mean in a list that filters and hides rows.
 *
 * Kept out of the component because every one of these decisions is a fact
 * about a destructive selection rather than about rendering, and a selection
 * that quietly grew past what the reader can see is the failure mode worth
 * testing without a browser.
 *
 * Two rules hold everything here up. A tick may only ever be added for a row
 * the reader is currently looking at — a filter or a search is a statement
 * about which worktrees are in play, and a control that ignored it would hand
 * "Delete selected" rows that are not on screen. And a tick already made
 * survives the filter changing: narrowing to one repository to tick two rows
 * there, then widening again, must not silently drop them.
 */

/** The little this module needs to know about a row. */
export interface SelectableRow {
  path: string;
  /** A repository's main worktree is never removable, so it is never ticked. */
  isMain: boolean;
}

/**
 * Three states, because two cannot describe a partial selection.
 *
 * `indeterminate` is not decoration: it is what tells a reader that pressing
 * the control will add rows rather than clear the ones already ticked.
 */
export type SelectAllState = "checked" | "indeterminate" | "unchecked";

export const SELECT_ALL_LABEL = "Select all visible worktrees";

/**
 * The rows a bulk action may touch: everything on screen except the main
 * worktrees, which the row's own checkbox already refuses.
 */
export function selectablePaths(rows: readonly SelectableRow[]): string[] {
  return rows.filter((row) => !row.isMain).map((row) => row.path);
}

/** Nothing selectable on screen means a disabled control, not a dead one. */
export function selectAllDisabled(rows: readonly SelectableRow[]): boolean {
  return selectablePaths(rows).length === 0;
}

/**
 * What the header checkbox is claiming, computed only from visible rows.
 *
 * A tick on a row hidden by the current filter deliberately does not make this
 * "checked": the control speaks for what is on screen, and saying "all" while
 * showing three of four rows would be a claim the reader cannot check.
 */
export function selectAllState(
  rows: readonly SelectableRow[],
  checked: ReadonlySet<string>,
): SelectAllState {
  const paths = selectablePaths(rows);
  if (paths.length === 0) return "unchecked";
  const ticked = paths.filter((path) => checked.has(path)).length;
  if (ticked === 0) return "unchecked";
  return ticked === paths.length ? "checked" : "indeterminate";
}

/**
 * Radix takes `true | false | "indeterminate"`, and this is the only place the
 * three-state answer is turned into it.
 */
export function selectAllChecked(
  state: SelectAllState,
): boolean | "indeterminate" {
  if (state === "checked") return true;
  if (state === "indeterminate") return "indeterminate";
  return false;
}

/**
 * The toggle, in terms of the visible rows alone.
 *
 * Pressing it when every visible removable row is ticked clears exactly those
 * paths; otherwise it adds all of them. In both directions the paths it was
 * not shown are copied through untouched — the reader ticked those on purpose,
 * and a filter is a way of looking at the list rather than an instruction to
 * forget part of it.
 */
export function toggleVisibleSelection(
  checked: ReadonlySet<string>,
  rows: readonly SelectableRow[],
): Set<string> {
  const paths = selectablePaths(rows);
  const next = new Set(checked);
  if (paths.length === 0) return next;
  const allTicked = paths.every((path) => checked.has(path));
  for (const path of paths) {
    if (allTicked) next.delete(path);
    else next.add(path);
  }
  return next;
}
