// Relative rather than aliased: this module's values are imported by
// `tests/collapse.test.ts`, which runs under plain Node with no bundler to
// resolve `@/`.
import {
  defaultCollapsed as defaultCollapsedFor,
  type DiffSectionModel,
} from "./diff-sections.ts";

/**
 * Whether each file is open, held as state rather than derived at read time.
 *
 * The first click on a caret used to do nothing visible, and the reason was
 * that "collapsed" was two things at once: an overrides map that started
 * empty, and a default recomputed from whatever sections happened to exist in
 * that render. The two disagreed the moment the section set changed — which it
 * does on every diff tab, because the focused analysis resolves after the
 * first paint and swaps the whole list. Overrides were also keyed by the
 * visible mode, so a click made before that resolution was filed under a key
 * nothing ever read again: the row's caret turned, its body did not, and the
 * second click "worked" only because by then the mode had settled.
 *
 * Here the default is applied once, into real state, for every section that
 * has appeared. Reading is a map lookup, so a click always flips exactly the
 * value the body renders from.
 */
export interface CollapseState {
  /** The section set these values were reconciled against. */
  signature: string;
  collapsed: Record<string, boolean>;
}

export const EMPTY_COLLAPSE: CollapseState = { signature: "", collapsed: {} };

/*
 * The set this state was reconciled against, as a string.
 *
 * Every section is a patch now, so the signature is simply the ids in order.
 * Filtering non-text entries out upstream cannot destabilise it: a list that
 * loses sixty-three cards keeps exactly the ids it had before, because those
 * cards never had one.
 */
const signatureOf = (sections: DiffSectionModel[]) =>
  sections.map((section) => section.id).join("\n");

/**
 * Fold a new section list into existing state.
 *
 * Returns the same object when nothing changed, so it is safe to call during
 * render and only schedules a re-render when it genuinely has something new to
 * say. A section already on screen keeps whatever the reader chose for it; a
 * section that has just appeared takes the default for the list it arrived in.
 *
 * Values for sections that are *not* in the new list are kept rather than
 * dropped. The two readings share one collapse map — At risk shows a subset of
 * the committed files — and discarding the absent ones meant a file folded in
 * Everything sprang open again on the way back.
 */
export function reconcileCollapse(
  state: CollapseState,
  sections: DiffSectionModel[],
): CollapseState {
  const signature = signatureOf(sections);
  if (signature === state.signature) return state;

  const fallback = defaultCollapsedFor(sections);
  const collapsed: Record<string, boolean> = { ...state.collapsed };
  for (const section of sections) {
    collapsed[section.id] = state.collapsed[section.id] ?? fallback;
  }
  return { signature, collapsed };
}

export function isCollapsed(
  state: CollapseState,
  section: DiffSectionModel,
): boolean {
  return state.collapsed[section.id] ?? false;
}

export function setCollapsed(
  state: CollapseState,
  id: string,
  collapsed: boolean,
): CollapseState {
  if (state.collapsed[id] === collapsed) return state;
  return { ...state, collapsed: { ...state.collapsed, [id]: collapsed } };
}

/** True only when there is something to collapse and all of it is closed. */
export function everyCollapsed(
  state: CollapseState,
  sections: DiffSectionModel[],
): boolean {
  if (sections.length === 0) return false;
  return sections.every((section) => isCollapsed(state, section));
}

export function setAllCollapsed(
  state: CollapseState,
  sections: DiffSectionModel[],
  collapsed: boolean,
): CollapseState {
  const next = { ...state.collapsed };
  for (const section of sections) {
    next[section.id] = collapsed;
  }
  return { ...state, collapsed: next };
}
