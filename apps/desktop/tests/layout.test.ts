/**
 * Run with: node --experimental-strip-types --test tests/layout.test.ts
 *
 * Deliberately dependency-free — the rules under test are pure, so they need a
 * runner and nothing else.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  CHECKBOX_WIDTH,
  COLUMN_GAP,
  DEFAULT_WIDTHS,
  FALLBACK_CHAR_WIDTH,
  MIN_WIDTH,
  NAME_FLOOR,
  NAME_TARGET,
  ROW_PADDING_X,
  WIDEST_MODIFIED,
  WIDEST_SIZE,
  applyResize,
  branchCharBudget,
  contentFloor,
  fitColumns,
  gridTemplate,
  middleTruncate,
  repoCharBudget,
  resizeRange,
  textWidth,
  type ColumnLayout,
  type ColumnWidths,
} from "../src/lib/layout.ts";

/**
 * The names that started this. Six worktrees in one repository, generated one
 * per agent task, sharing a prefix long enough that the first twenty characters
 * of each are identical.
 */
const BRANCHES = [
  "feature-fix-live-activities",
  "feature-fix-language-picker-clipping",
  "feature-fix-live-activity-timer",
  "feature-add-widget-configuration",
  "feature-fix-language-picker-search",
  "feature-add-widget-config-defaults",
];

test("a name that fits is returned untouched", () => {
  assert.equal(middleTruncate("main", 40), "main");
  assert.equal(
    middleTruncate("feature-fix-live-activities", 31),
    "feature-fix-live-activities",
  );
});

test("six branches sharing a prefix stay distinguishable after truncation", () => {
  for (const budget of [18, 22, 26, 30, 34]) {
    const shown = BRANCHES.map((b) => middleTruncate(b, budget));
    assert.equal(
      new Set(shown).size,
      BRANCHES.length,
      `budget ${budget} collapsed distinct branches into ${JSON.stringify(shown)}`,
    );
    for (const s of shown) {
      assert.ok(
        Array.from(s).length <= budget,
        `"${s}" is longer than the ${budget} characters it was given`,
      );
    }
  }
});

test("trailing truncation is what it is replacing, and it fails", () => {
  // The behaviour before this change, kept as the reason for it: at any budget
  // short enough to matter, the six names become the same string.
  const trailing = BRANCHES.map((b) => b.slice(0, 20) + "\u2026");
  assert.ok(
    new Set(trailing).size < BRANCHES.length,
    "the failure this replaces should still be demonstrable",
  );
  assert.equal(trailing[0], trailing[2], "two distinct worktrees, one string");
});

test("the tail is preserved, because the tail is what differs", () => {
  const shown = middleTruncate("feature-fix-language-picker-clipping", 24);
  assert.ok(
    shown.endsWith("clipping"),
    `"${shown}" dropped the identifying end of the name`,
  );
  assert.ok(shown.includes("\u2026"));
});

test("a budget under the reserved tail keeps the tail rather than splitting", () => {
  const shown = middleTruncate("feature-fix-live-activities", 12);
  assert.equal(shown, "\u2026-activities");
  assert.ok(shown.endsWith("activities"));
});

test("a head too short to identify anything is dropped for more tail", () => {
  // `s…c-fix-language-picker-clipping` reads as a rendering fault; one
  // character of a shared prefix is not worth the character it costs.
  const shown = middleTruncate("feature-fix-language-picker-clipping", 32);
  assert.ok(shown.startsWith("\u2026"), `kept a useless head: "${shown}"`);
  assert.equal(Array.from(shown).length, 32);
});

test("a head long enough to be worth reading is kept", () => {
  const shown = middleTruncate(
    "feature-fix-language-picker-clipping",
    38,
    20,
  );
  assert.ok(!shown.startsWith("\u2026"), `dropped a usable head: "${shown}"`);
  assert.ok(shown.startsWith("feature"));
  assert.ok(shown.endsWith("clipping"));
});

test("degenerate budgets do not throw or produce longer output", () => {
  assert.equal(middleTruncate("anything", 0), "\u2026");
  assert.equal(middleTruncate("anything", 1), "\u2026");
  assert.equal(middleTruncate("anything", -5), "\u2026");
  assert.equal(middleTruncate("", 10), "");
});

test("the character budget shrinks by what the repository prefix costs", () => {
  const wide = branchCharBudget(240, 7, 0);
  const withRepo = branchCharBudget(240, 7, repoCharBudget("chugs"));
  assert.ok(withRepo < wide);
  assert.ok(withRepo > 4);
});

test("a long repository name cannot eat the whole budget", () => {
  const budget = branchCharBudget(
    240,
    7,
    repoCharBudget("an-extremely-long-monorepo-name-that-goes-on"),
  );
  assert.ok(budget >= 4, `left ${budget} characters for the branch`);
});

function widths(overrides: Partial<ColumnWidths> = {}): ColumnWidths {
  return { ...DEFAULT_WIDTHS, ...overrides };
}

/**
 * The chain between a tab and a track, as the DOM actually builds it.
 *
 * The previous version of this file compared the tracks against a number it
 * invented, so it passed while the Size column was visibly sheared off at the
 * window edge. The number that matters is the one a row is laid out in, and
 * every element between the tab and that row takes a bite out of it:
 * the list pane's own padding, the scrollbar when the list is long enough to
 * need one, and the row's padding. A test that skips any of them is measuring
 * a table that does not exist.
 */
const PANE_PADDING_X = 12; // `px-3` on the scrolled list pane
const SCROLLBAR = 10; // `::-webkit-scrollbar { width: 10px }` in styles.css

/** What `usePaneWidth` reports for a tab of this width. */
function reportedPaneWidth(tabWidth: number, scrolling = false): number {
  return tabWidth - PANE_PADDING_X * 2 - (scrolling ? SCROLLBAR : 0);
}

/** What is left for the tracks once the row has taken its own padding. */
function trackSpace(paneWidth: number): number {
  return paneWidth - ROW_PADDING_X * 2;
}

function trackTotal(layout: ColumnLayout): number {
  const tracks =
    CHECKBOX_WIDTH +
    layout.name +
    layout.status +
    (layout.showModified ? layout.modified : 0) +
    layout.size;
  return tracks + COLUMN_GAP * (layout.showModified ? 4 : 3);
}

test("surplus goes to the identifier", () => {
  const layout = fitColumns(1180, widths());
  assert.equal(layout.compressed, false);
  assert.equal(layout.status, DEFAULT_WIDTHS.status);
  assert.ok(layout.name > 700, `identifier only got ${layout.name}px`);
  assert.equal(layout.stacked, false);
});

test("the identifier keeps its target when the inspector opens", () => {
  // 1180px window, ~620px inspector: the case that produced the stubs.
  const layout = fitColumns(560, widths());
  assert.ok(
    layout.name >= NAME_TARGET,
    `identifier collapsed to ${layout.name}px`,
  );
  assert.ok(layout.status < DEFAULT_WIDTHS.status, "status refused to yield");
  assert.ok(layout.status >= MIN_WIDTH.status);
  assert.equal(layout.stacked, true, "the narrow case must stack");
});

test("a status width stored on a wide monitor cannot break a narrow pane", () => {
  const stored = widths({ status: 320 });
  const layout = fitColumns(560, stored);
  assert.ok(
    layout.name >= NAME_TARGET,
    `a stored 320px status left the identifier ${layout.name}px`,
  );
  assert.ok(
    layout.status < stored.status && layout.status >= MIN_WIDTH.status,
    `status did not yield: ${layout.status}px`,
  );
});

test("the tracks fit the space a row actually has, at every width", () => {
  const stores = [
    widths(),
    widths({ status: 320, modified: 120, size: 160 }),
    widths({ status: MIN_WIDTH.status, modified: 0, size: 0 }),
  ];

  for (let tab = 240; tab <= 2000; tab += 7) {
    for (const scrolling of [false, true]) {
      const pane = reportedPaneWidth(tab, scrolling);
      const room = trackSpace(pane);
      for (const stored of stores) {
        const layout = fitColumns(pane, stored);
        assert.ok(
          trackTotal(layout) <= room,
          `tracks (${trackTotal(layout)}px) overflowed the ${room}px a row has ` +
            `in a ${tab}px tab${scrolling ? " with a scrollbar" : ""}`,
        );
      }
    }
  }
});

test("every column is at least as wide as the widest thing it can show", () => {
  // Size and Modified are filled by formatters with bounded output, so this is
  // checkable rather than a matter of taste. The failure it guards against is
  // the one that reads as a plausible value: `30.1 M` where `30.1 MB` was meant.
  for (const charWidth of [FALLBACK_CHAR_WIDTH, 6.4, 7.9]) {
    for (let tab = 320; tab <= 2000; tab += 11) {
      const pane = reportedPaneWidth(tab, true);
      const layout = fitColumns(pane, widths(), charWidth);
      if (layout.degenerate) continue;

      assert.ok(
        layout.size >= textWidth(WIDEST_SIZE, charWidth),
        `Size got ${layout.size}px, too narrow for "${WIDEST_SIZE}" at ` +
          `${charWidth}px/char in a ${tab}px tab`,
      );
      if (layout.showModified) {
        assert.ok(
          layout.modified >= textWidth(WIDEST_MODIFIED, charWidth),
          `Modified got ${layout.modified}px, too narrow for ` +
            `"${WIDEST_MODIFIED}" in a ${tab}px tab`,
        );
      }
      assert.ok(layout.status >= MIN_WIDTH.status);
    }
  }
});

test("the window the bug was found in has room for its sizes", () => {
  // 1180x760, a 176px sidebar, panel closed: the geometry of the screenshot in
  // which `30.1 MB` rendered as `30.1 M` and a sliver.
  const closed = fitColumns(reportedPaneWidth(1180 - 176), widths());
  assert.ok(trackTotal(closed) <= trackSpace(reportedPaneWidth(1180 - 176)));
  assert.ok(closed.size >= contentFloor("size", FALLBACK_CHAR_WIDTH));
  assert.equal(closed.showModified, true);

  // The same window with the 384px panel open.
  const open = fitColumns(reportedPaneWidth(1180 - 176 - 384), widths());
  assert.ok(trackTotal(open) <= trackSpace(reportedPaneWidth(1180 - 176 - 384)));
  assert.ok(open.size >= contentFloor("size", FALLBACK_CHAR_WIDTH));
  assert.ok(open.name >= NAME_TARGET);
});

test("a row's padding is part of the arithmetic, not decoration", () => {
  // Guards the actual regression: if `fitColumns` ever stops accounting for the
  // padding the row draws itself with, the rightmost track lands outside the
  // clip and its value loses characters without any sign that it did.
  const pane = 600;
  const layout = fitColumns(pane, widths());
  assert.ok(
    trackTotal(layout) <= pane - ROW_PADDING_X * 2,
    "the tracks assumed the row's padding did not exist",
  );
  assert.ok(ROW_PADDING_X > 0, "the constant the row draws from must be real");
});

test("modified is dropped before the identifier goes under its floor", () => {
  const layout = fitColumns(380, widths());
  assert.equal(layout.showModified, false);
  assert.ok(layout.name >= NAME_FLOOR, `identifier fell to ${layout.name}px`);
});

test("the grid template drops the modified track with the column", () => {
  const wide = gridTemplate(fitColumns(1180, widths()));
  assert.equal(wide.split(" ").length, 5);

  const narrow = fitColumns(380, widths());
  assert.equal(narrow.showModified, false);
  assert.equal(gridTemplate(narrow).split(" ").length, 4);
});

test("before measurement the grid stays elastic rather than guessing", () => {
  const layout = fitColumns(0, widths());
  assert.equal(layout.measured, false);
  assert.ok(gridTemplate(layout).includes("1fr"));
});

test("resizing is refused while the pane is forcing the widths", () => {
  const stored = widths();
  const layout = fitColumns(560, stored);
  assert.equal(layout.compressed, true);
  assert.equal(resizeRange(layout, stored, "status"), null);
});

test("a drag cannot push the identifier below its target", () => {
  const stored = widths();
  const layout = fitColumns(900, stored);
  const range = resizeRange(layout, stored, "status");
  assert.ok(range);

  // Ask for far more status than the pane can spare.
  const next = applyResize(stored, range, "status", undefined, -10_000);
  const after = fitColumns(900, next);
  assert.ok(
    after.name >= NAME_TARGET,
    `a drag left the identifier at ${after.name}px`,
  );
});

test("a drag cannot push a column below its own minimum", () => {
  const stored = widths();
  const layout = fitColumns(1180, stored);
  const range = resizeRange(layout, stored, "modified", "status");
  assert.ok(range);

  const smaller = applyResize(stored, range, "modified", "status", 10_000);
  assert.equal(smaller.modified, MIN_WIDTH.modified);

  const bigger = applyResize(stored, range, "modified", "status", -10_000);
  assert.equal(bigger.status, MIN_WIDTH.status);
  assert.equal(
    bigger.status + bigger.modified,
    stored.status + stored.modified,
    "an internal boundary must trade width, not create it",
  );
});
