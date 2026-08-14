import { test } from "node:test";
import assert from "node:assert/strict";

import {
  EMPTY_COLLAPSE,
  everyCollapsed,
  isCollapsed,
  reconcileCollapse,
  setAllCollapsed,
  setCollapsed,
  type CollapseState,
} from "../src/components/collapse.ts";
import type { DiffSectionModel } from "../src/components/diff-sections.ts";

/**
 * The reported bug, reproduced from component state.
 *
 * The first click on a caret turned the caret and left the body exactly as it
 * was; the second click worked. The cause was that "collapsed" was two things
 * that could disagree — an overrides map keyed by the visible mode, and a
 * default recomputed from whatever sections that render happened to have —
 * and the focused analysis resolves after the first paint, so the first click
 * was filed under a key nothing read again.
 *
 * `render` below is the component's read path: reconcile, then draw. A body is
 * rendered exactly when its section is not collapsed, which is the same
 * condition `DiffPatchSection` uses.
 */

const patchSection = (id: string): DiffSectionModel => ({
  id,
  anchor: id,
  path: id,
  patch: `diff --git a/${id} b/${id}`,
  stat: { kind: "counts", insertions: 1, deletions: 0, tone: "change" },
});

/** One paint: fold the sections in, then report which bodies are drawn. */
function render(state: CollapseState, sections: DiffSectionModel[]) {
  const reconciled = reconcileCollapse(state, sections);
  return {
    state: reconciled,
    bodies: sections
      .filter((section) => !isCollapsed(reconciled, section))
      .map((section) => section.id),
  };
}

test("the first click on a caret opens that file's body, not the second", () => {
  const sections = [patchSection("a"), patchSection("b")];

  // Default state for a short review: everything already open.
  const first = render(EMPTY_COLLAPSE, sections);
  assert.deepEqual(first.bodies, ["a", "b"]);

  // Click once: exactly one body closes, and it closes now.
  const afterClick = render(setCollapsed(first.state, "a", true), sections);
  assert.deepEqual(afterClick.bodies, ["b"]);

  // Click again: it comes back. One click, one change, every time.
  const afterSecond = render(setCollapsed(afterClick.state, "a", false), sections);
  assert.deepEqual(afterSecond.bodies, ["a", "b"]);
});

test("the first click expands when the review started folded", () => {
  // Past the threshold, a review opens closed.
  const sections = Array.from({ length: 20 }, (_, i) => patchSection(`f${i}`));

  const first = render(EMPTY_COLLAPSE, sections);
  assert.deepEqual(first.bodies, []);

  const afterClick = render(setCollapsed(first.state, "f3", false), sections);
  assert.deepEqual(afterClick.bodies, ["f3"]);
});

/**
 * The exact sequence that made the first click a no-op: the focused analysis
 * lands after the first paint and replaces the section list.
 */
test("a click survives the section list being replaced under it", () => {
  const early = [patchSection("a"), patchSection("b")];
  const late = [patchSection("a"), patchSection("b"), patchSection("c")];

  const first = render(EMPTY_COLLAPSE, early);
  const clicked = render(setCollapsed(first.state, "a", true), early);
  assert.deepEqual(clicked.bodies, ["b"]);

  const afterAnalysis = render(clicked.state, late);
  assert.deepEqual(afterAnalysis.bodies, ["b", "c"], "the choice is not lost");
});

test("the global control changes every body on its first press", () => {
  const sections = [patchSection("a"), patchSection("b")];

  const first = render(EMPTY_COLLAPSE, sections);
  assert.equal(everyCollapsed(first.state, sections), false);

  const collapsedAll = render(
    setAllCollapsed(first.state, sections, true),
    sections,
  );
  assert.deepEqual(collapsedAll.bodies, []);
  assert.equal(everyCollapsed(collapsedAll.state, sections), true);

  const expandedAll = render(
    setAllCollapsed(collapsedAll.state, sections, false),
    sections,
  );
  assert.deepEqual(expandedAll.bodies, ["a", "b"]);
});

/**
 * The list is now only patches, so "every section" and "every section that can
 * open" are the same set. An empty list has no state to report either way.
 */
test("a list with nothing in it reports no global state to toggle", () => {
  const { state } = render(EMPTY_COLLAPSE, []);

  assert.equal(everyCollapsed(state, []), false);
});

/**
 * Filtering the non-text entries out changes the list under the collapse map,
 * exactly as the late-arriving focused analysis does. The first click still has
 * to land on the first press.
 */
test("the first click works after non-text entries are filtered out", () => {
  // What the first paint had before the filter ran: three patches.
  const unfiltered = [patchSection("a"), patchSection("b"), patchSection("c")];
  // What it has after: the binaries were never sections, so "b" is gone.
  const filtered = [patchSection("a"), patchSection("c")];

  const first = render(EMPTY_COLLAPSE, unfiltered);
  assert.deepEqual(first.bodies, ["a", "b", "c"]);

  const afterFilter = render(first.state, filtered);
  assert.deepEqual(afterFilter.bodies, ["a", "c"]);

  const clicked = render(setCollapsed(afterFilter.state, "a", true), filtered);
  assert.deepEqual(clicked.bodies, ["c"], "one click, one change");
});

test("reconciling an unchanged list returns the same object", () => {
  const sections = [patchSection("a")];
  const once = reconcileCollapse(EMPTY_COLLAPSE, sections);

  assert.equal(reconcileCollapse(once, sections), once);
});

/* ------------------------------------------------------------------ *
 * The reading filter is a way of looking at one payload, so it may not
 * throw away what the reader has already decided.
 * ------------------------------------------------------------------ */

/**
 * Switching to the narrowed reading replaces the committed sections with a
 * subset of themselves. Nothing was fetched and nothing was invalidated, so a
 * file folded in one reading is still folded in the other and on the way back.
 */
test("switching readings keeps every file exactly as the reader left it", () => {
  const onDisk = [patchSection("on-disk/a"), patchSection("on-disk/b")];
  const everything = [
    ...onDisk,
    patchSection("committed/x"),
    patchSection("committed/y"),
  ];
  // The at-risk reading keeps one of the committed files and rewrites nothing
  // else about the list it shares with the complete reading.
  const atRisk = [...onDisk, patchSection("committed/x")];

  const opened = render(EMPTY_COLLAPSE, everything);
  const folded = render(setCollapsed(opened.state, "committed/x", true), everything);
  assert.deepEqual(folded.bodies, ["on-disk/a", "on-disk/b", "committed/y"]);

  const narrowed = render(folded.state, atRisk);
  assert.deepEqual(
    narrowed.bodies,
    ["on-disk/a", "on-disk/b"],
    "the file the reader folded is still folded",
  );

  const back = render(narrowed.state, everything);
  assert.deepEqual(
    back.bodies,
    ["on-disk/a", "on-disk/b", "committed/y"],
    "and the file it never showed did not spring open",
  );
});

/**
 * The reported bug, at the moment it was most likely to appear: the first click
 * after the list changed under the reader. Sequential clicks and first clicks
 * are the same operation on the same map, so they behave identically.
 */
test("the first click after switching readings behaves like every other", () => {
  const everything = [patchSection("a"), patchSection("b"), patchSection("c")];
  const atRisk = [patchSection("a"), patchSection("c")];

  const narrowed = render(EMPTY_COLLAPSE, atRisk);
  assert.deepEqual(narrowed.bodies, ["a", "c"]);

  const firstClick = render(setCollapsed(narrowed.state, "a", true), atRisk);
  assert.deepEqual(firstClick.bodies, ["c"], "one click, one change");

  const secondClick = render(setCollapsed(firstClick.state, "c", true), atRisk);
  assert.deepEqual(secondClick.bodies, []);

  // Widening again folds nothing the reader did not fold, and opens nothing
  // they closed.
  const widened = render(secondClick.state, everything);
  assert.deepEqual(widened.bodies, ["b"]);
});
