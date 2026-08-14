import { test } from "node:test";
import assert from "node:assert/strict";

import {
  COMMITTED_HEADING,
  ON_DISK_HEADING,
  balanceOf,
  combineBalances,
  coverageOf,
  omittedFrom,
  residualPaths,
} from "../src/components/diff-sections.ts";
import {
  MAX_DISCLOSED_ENTRIES,
  NOT_DIFFABLE_DESCRIPTION,
  NOT_DIFFABLE_TITLE,
  disclosureCapNote,
  disclosureResidualNote,
  disclosureScale,
  hasDisclosure,
  notDiffableDetail,
  notDiffableDisclosure,
  notDiffablePathTotal,
  notDiffableRows,
  notDiffableTriggerLabel,
} from "../src/components/not-diffable.ts";
import type { DiffEntry, Patches } from "../src/lib/api.ts";

const patchesOf = (parts: Partial<Patches>): Patches => ({
  scope: "history",
  committed: [],
  uncommitted: [],
  truncated: false,
  incomplete: false,
  untrackedTotal: 0,
  untrackedShown: 0,
  untrackedEntries: 0,
  limits: [],
  ...parts,
});

const text = (path: string): DiffEntry => ({
  path,
  origin: "uncommitted",
  insertions: 1,
  deletions: 0,
  kind: "text",
  patch: `--- a/${path}\n+++ b/${path}\n`,
  hunks: 1,
});

const binary = (path: string): DiffEntry => ({
  path,
  origin: "uncommitted",
  insertions: 0,
  deletions: 0,
  kind: "binary",
});

/**
 * The reported gap: the summary counted paths it would not name, so a reader
 * could see that something was omitted but never which files.
 */
test("every non-text entry is disclosed by its exact path", () => {
  const disclosure = notDiffableDisclosure(
    patchesOf({
      uncommitted: [
        text("src/main.rs"),
        binary("assets/logo.png"),
        { path: "link", origin: "uncommitted", insertions: 0, deletions: 0, kind: "symlink", target: "../elsewhere" },
      ],
      committed: [
        { path: "vendor/lib", origin: "committed", insertions: 0, deletions: 0, kind: "repository", repository: "nested", paths: 18, items: null },
      ],
    }),
  );

  assert.equal(disclosure.entries, 3);
  assert.equal(disclosure.listed, 3);
  assert.equal(disclosure.hidden, 0);
  // The repository is one entry standing for eighteen raw paths.
  assert.equal(disclosure.paths, 1 + 1 + 18);

  const [onDisk, committed] = disclosure.groups;
  assert.equal(onDisk.heading, ON_DISK_HEADING);
  assert.equal(committed.heading, COMMITTED_HEADING);
  assert.deepEqual(
    onDisk.rows.map((row) => row.path),
    ["assets/logo.png", "link"],
  );
  assert.deepEqual(
    committed.rows.map((row) => row.path),
    ["vendor/lib"],
  );
  // The text entry is a diff the view draws, so it is not in this list at all.
  assert.ok(!onDisk.rows.some((row) => row.path === "src/main.rs"));
});

/** The reason is the summary's own vocabulary, not a second set of labels. */
test("each row is named with the kind the breakdown counts it as", () => {
  const rows = notDiffableRows([
    binary("a.bin"),
    { path: "b", origin: "uncommitted", insertions: 0, deletions: 0, kind: "empty" },
    { path: "c", origin: "uncommitted", insertions: 0, deletions: 0, kind: "symlink", target: "d" },
    { path: "e/", origin: "uncommitted", insertions: 0, deletions: 0, kind: "directory", paths: 4, items: 4 },
    { path: "f", origin: "uncommitted", insertions: 0, deletions: 0, kind: "repository", repository: "linkedWorktree", paths: 2, items: null },
    { path: "g", origin: "uncommitted", insertions: 0, deletions: 0, kind: "repository", repository: "bare", paths: 3, items: null },
    { path: "h", origin: "uncommitted", insertions: 0, deletions: 0, kind: "metadata", detail: "mode changed" },
    { path: "i", origin: "uncommitted", insertions: 0, deletions: 0, kind: "unread", detail: "permission denied" },
  ]);

  assert.deepEqual(
    rows.map((row) => row.reason),
    [
      "binary file",
      "empty file",
      "symbolic link",
      "untracked directory",
      "linked worktree",
      "bare repository",
      "metadata-only change",
      "unreadable path",
    ],
  );
  // Every distinction the backend draws survives into the disclosure.
  assert.equal(new Set(rows.map((row) => row.reason)).size, rows.length);
});

/** A row may only state what the backend actually sent. */
test("aggregate entries state their coverage instead of inventing children", () => {
  const directory = notDiffableDetail({ kind: "directory", paths: 412, items: null });
  assert.match(directory, /covers 412 paths/);
  assert.match(directory, /counted here, not listed/);

  const repository = notDiffableDetail({
    kind: "repository",
    repository: "nested",
    paths: 18,
    items: null,
  });
  assert.match(repository, /covers 18 paths/);
  assert.match(repository, /counted here, not listed/);

  assert.match(
    notDiffableDetail({ kind: "symlink", target: "../shared/config" }),
    /\.\.\/shared\/config/,
  );
  assert.match(
    notDiffableDetail({ kind: "unread", detail: "permission denied" }),
    /permission denied/,
  );
  assert.match(
    notDiffableDetail({ kind: "metadata", detail: "mode changed" }),
    /mode changed/,
  );
});

/** An unbounded list is the wall this view exists to avoid. */
test("the disclosure is capped, and says exactly how much it is not showing", () => {
  const many = Array.from({ length: 30 }, (_, i) => binary(`bin/${i}`));
  const disclosure = notDiffableDisclosure(
    patchesOf({ uncommitted: many.slice(0, 20), committed: many.slice(20) }),
    12,
  );

  assert.equal(disclosure.entries, 30);
  assert.equal(disclosure.listed, 12);
  assert.equal(disclosure.hidden, 18);
  // Counting does not stop where listing does: the totals still describe every
  // omitted entry, including the ones no row was built for.
  assert.equal(disclosure.paths, 30);
  // The budget is spent in view order rather than split between the groups.
  assert.equal(disclosure.groups[0].rows.length, 12);
  assert.equal(disclosure.groups[0].hidden, 8);
  assert.equal(disclosure.groups[1].rows.length, 0);
  assert.equal(disclosure.groups[1].hidden, 10);

  const note = disclosureCapNote(disclosure);
  assert.ok(note);
  assert.match(note, /18 more entries are not listed/);
  // Never "and more": the remainder is a number the reader can act on.
  assert.doesNotMatch(note, /and more|many more|several/i);
});

test("the default cap is documented and applied", () => {
  assert.equal(MAX_DISCLOSED_ENTRIES, 200);
  const many = Array.from({ length: MAX_DISCLOSED_ENTRIES + 5 }, (_, i) =>
    binary(`bin/${i}`),
  );
  const disclosure = notDiffableDisclosure(patchesOf({ uncommitted: many }));
  assert.equal(disclosure.listed, MAX_DISCLOSED_ENTRIES);
  assert.equal(disclosure.hidden, 5);
  assert.match(disclosureCapNote(disclosure) ?? "", /at most 200/);
});

/**
 * A limit that cut the listing short means yawm never received those names, so
 * the dialog may not read as a complete list of what is omitted.
 */
test("paths a limit removed are declared unavailable rather than left out", () => {
  const disclosure = notDiffableDisclosure(
    patchesOf({
      uncommitted: [binary("a.bin")],
      limits: [{ kind: "displayLimit", shown: 100, total: 137 }],
    }),
  );

  assert.equal(disclosure.residual, 37);
  const note = disclosureResidualNote(disclosure);
  assert.ok(note);
  assert.match(note, /37 further paths/);
  assert.match(note, /not available/);
});

test("no residual means no claim about missing names", () => {
  const disclosure = notDiffableDisclosure(
    patchesOf({ uncommitted: [binary("a.bin")] }),
  );
  assert.equal(disclosure.residual, 0);
  assert.equal(disclosureResidualNote(disclosure), null);
  assert.equal(disclosureCapNote(disclosure), null);
});

/** Nothing omitted means no trigger, rather than a dialog listing nothing. */
test("a changeset with only text diffs has nothing to disclose", () => {
  const disclosure = notDiffableDisclosure(
    patchesOf({ uncommitted: [text("src/main.rs")] }),
  );
  assert.equal(hasDisclosure(disclosure), false);
  assert.deepEqual(disclosure.groups, []);
});

/** The trigger names the number standing beside it on the summary line. */
test("the trigger states the count and what it is about", () => {
  assert.match(notDiffableTriggerLabel(63), /63 paths/);
  assert.match(notDiffableTriggerLabel(63), /line-by-line/);
  assert.match(notDiffableTriggerLabel(1), /1 path\b/);
});

/** The dialog's own words: what these paths are, and why they are not drawn. */
test("the dialog says these paths could not be compared as text", () => {
  assert.match(NOT_DIFFABLE_TITLE, /line-by-line diffs/);
  assert.match(NOT_DIFFABLE_DESCRIPTION, /compared as text/);
  assert.match(NOT_DIFFABLE_DESCRIPTION, /counted/);
});

/** Entries and paths are two quantities, and the scale says so when they differ. */
test("the scale distinguishes entries from the paths they cover", () => {
  const one = notDiffableDisclosure(
    patchesOf({ uncommitted: [binary("a.bin")] }),
  );
  assert.equal(disclosureScale(one), "1 path without a text diff");

  const aggregate = notDiffableDisclosure(
    patchesOf({
      uncommitted: [
        binary("a.bin"),
        { path: "vendor", origin: "uncommitted", insertions: 0, deletions: 0, kind: "repository", repository: "nested", paths: 18, items: null },
      ],
    }),
  );
  assert.equal(
    disclosureScale(aggregate),
    "19 paths without a text diff, in 2 entries",
  );
});

/**
 * The dialog opens from a number on the summary line, so it may not contradict
 * it. The summary counts path identities across the groups; adding the rows up
 * would count a path omitted in both groups twice and claim a larger omission
 * than the line the reader pressed.
 */
test("one path omitted in both groups is two rows and one path", () => {
  const patches = patchesOf({
    uncommitted: [binary("assets/logo.png")],
    committed: [{ ...binary("assets/logo.png"), origin: "committed" }],
  });
  const disclosure = notDiffableDisclosure(patches);

  assert.equal(disclosure.entries, 2);
  assert.equal(disclosure.paths, 1);
  assert.equal(notDiffablePathTotal(patches), 1);
  assert.equal(disclosureScale(disclosure), "1 path without a text diff, in 2 entries");
});

/** The widest claim about one identity wins, exactly as the summary's does. */
test("the path total agrees with the summary's own arithmetic", () => {
  const patches = patchesOf({
    uncommitted: [
      { path: "vendor", origin: "uncommitted", insertions: 0, deletions: 0, kind: "repository", repository: "nested", paths: 18, items: null },
    ],
    committed: [
      { path: "vendor", origin: "committed", insertions: 0, deletions: 0, kind: "repository", repository: "nested", paths: 4, items: null },
    ],
  });

  const balance = combineBalances(
    [
      balanceOf([], omittedFrom(patches.uncommitted), { insertions: 0, deletions: 0 }, coverageOf(patches.uncommitted)),
      balanceOf([], omittedFrom(patches.committed), { insertions: 0, deletions: 0 }, coverageOf(patches.committed)),
    ],
    residualPaths(patches),
  );

  assert.equal(notDiffableDisclosure(patches).paths, balance.notDiffable);
  assert.equal(balance.notDiffable, 18);
});

/**
 * A path that is a binary here and a text diff there was drawn: the reader can
 * read it in the view. The summary does not count it as not diffable, so this
 * list — which exists to reveal what is drawn nowhere — does not hold it
 * either. The two can then never state different sets.
 */
test("a path drawn in the other group is not listed as undrawn", () => {
  const disclosure = notDiffableDisclosure(
    patchesOf({
      uncommitted: [binary("notes.md")],
      committed: [{ ...text("notes.md"), origin: "committed" }],
    }),
  );

  assert.equal(disclosure.entries, 0);
  assert.equal(disclosure.paths, 0);
  assert.equal(hasDisclosure(disclosure), false);
});

/**
 * The trigger and the list are the same claim: whenever the summary counts
 * something as not diffable there is a list to open, and whenever it counts
 * nothing there is nothing to open.
 */
test("having something to disclose means the summary counted something", () => {
  const cases: Patches[] = [
    patchesOf({ uncommitted: [binary("a.bin")] }),
    patchesOf({ uncommitted: [text("a.ts")] }),
    patchesOf({
      uncommitted: [binary("notes.md")],
      committed: [{ ...text("notes.md"), origin: "committed" }],
    }),
    patchesOf({
      uncommitted: [binary("assets/logo.png")],
      committed: [{ ...binary("assets/logo.png"), origin: "committed" }],
    }),
  ];

  for (const patches of cases) {
    const disclosure = notDiffableDisclosure(patches);
    assert.equal(
      hasDisclosure(disclosure),
      notDiffablePathTotal(patches) > 0,
      `disagreement for ${JSON.stringify(patches.uncommitted[0]?.path)}`,
    );
  }
});
