import { test } from "node:test";
import assert from "node:assert/strict";

import {
  COLLAPSE_THRESHOLD,
  COMMITTED_HEADING,
  MAX_OMITTED_KINDS,
  NO_LINE_DIFFS_TITLE,
  NO_OMISSIONS,
  ON_DISK_HEADING,
  anchorId,
  anchorScope,
  balanceOf,
  combineBalances,
  coverageOf,
  defaultCollapsed,
  fileEntries,
  groupAnchorId,
  hasCollapsibleSections,
  kindLabel,
  lineTotals,
  mergeOmitted,
  nonTextEntries,
  omittedBreakdown,
  omittedClause,
  omittedFrom as omittedFromEntries,
  pathsCovered,
  readingNarrows,
  residualPaths,
  sectionFor,
  textEntries,
  textStat,
} from "../src/components/diff-sections.ts";
import { changedPathTotal, type DiffEntry, type Patches } from "../src/lib/api.ts";

/** One mounted view's anchor namespace; every section here belongs to it. */
const scope = anchorScope("/repo/../wt");

const group = {
  counting: "lines changed",
  atRisk: false,
  counts: { insertions: 3, deletions: 1 },
};

const patch = [
  "diff --git a/a.ts b/a.ts",
  "--- a/a.ts",
  "+++ b/a.ts",
  "@@ -1,1 +1,2 @@",
  " context",
  "+added",
].join("\n");

const textEntry = (path: string): DiffEntry => ({
  path,
  origin: "uncommitted",
  insertions: 1,
  deletions: 0,
  kind: "text",
  patch,
  hunks: 1,
});

const binary = (path: string): DiffEntry => ({
  path,
  origin: "uncommitted",
  insertions: 0,
  deletions: 0,
  kind: "binary",
});

const bareRepository = (path: string, paths: number): DiffEntry => ({
  path,
  origin: "uncommitted",
  insertions: 0,
  deletions: 0,
  kind: "repository",
  repository: "bare",
  paths,
  items: 7,
});

/**
 * The reported bug, at its source: a list called a diff held rows that were
 * not diffs. The filter is in the model, so nothing downstream — tree, section
 * list, collapse map — can put one back.
 */
test("only entries carrying a patch become sections", () => {
  const entries: DiffEntry[] = [
    textEntry("src/a.ts"),
    binary("assets/logo.png"),
    { path: "empty.txt", origin: "uncommitted", insertions: 0, deletions: 0, kind: "empty" },
    {
      path: "link",
      origin: "uncommitted",
      insertions: 0,
      deletions: 0,
      kind: "symlink",
      target: "../elsewhere",
    },
    {
      path: "dir/",
      origin: "uncommitted",
      insertions: 0,
      deletions: 0,
      kind: "directory",
      paths: 4,
      items: 3,
    },
    bareRepository("remote.git", 18),
    { path: "mode.sh", origin: "uncommitted", insertions: 0, deletions: 0, kind: "metadata" },
    { path: "locked", origin: "uncommitted", insertions: 0, deletions: 0, kind: "unread" },
    textEntry("src/b.ts"),
  ];

  assert.deepEqual(
    textEntries(entries).map((entry) => entry.path),
    ["src/a.ts", "src/b.ts"],
  );
  assert.equal(nonTextEntries(entries).length, 7);
});

/** The tree is built from the sections, so it inherits the same filter. */
test("nothing without a patch reaches the file tree", () => {
  const sections = textEntries([
    textEntry("src/a.ts"),
    binary("assets/logo.png"),
    bareRepository("remote.git", 18),
  ]).map((entry) => sectionFor(scope, entry, "uncommitted", group));

  assert.deepEqual(
    fileEntries(sections).map((entry) => entry.path),
    ["src/a.ts"],
  );
});

test("a section carries the patch it will render", () => {
  const section = sectionFor(scope, textEntry("src/main.ts"), "uncommitted", group);

  assert.equal(section.patch, patch);
  assert.deepEqual(section.stat, {
    kind: "counts",
    insertions: 3,
    deletions: 1,
    tone: "change",
    title: "lines changed",
  });
});

test("a file compared as a whole says so instead of inventing counts", () => {
  const stat = textStat({ ...group, counts: null });

  assert.equal(stat.kind, "unknown");
  if (stat.kind !== "unknown") return;
  assert.equal(stat.label, "whole file");
});

/* ------------------------------------------------------------------ *
 * The arithmetic of what was left out.
 * ------------------------------------------------------------------ */

/**
 * A nested repository is one entry to this list and eighteen paths to Git.
 * Both numbers are true, and the summary has to be checkable against the count
 * the worktree row already showed.
 */
test("a repository contributes every path it covers, not one row", () => {
  assert.equal(pathsCovered({ kind: "repository", repository: "bare", paths: 18, items: 7 }), 18);
  assert.equal(pathsCovered({ kind: "directory", paths: 4, items: 3 }), 4);
  assert.equal(pathsCovered({ kind: "binary" }), 1);
  assert.equal(pathsCovered({ kind: "symlink", target: "x" }), 1);

  // A backend reporting zero cannot make an omitted entry disappear.
  assert.equal(pathsCovered({ kind: "directory", paths: 0, items: null }), 1);
});

test("omitted counts entries and raw paths separately", () => {
  const omitted = omittedFromEntries([
    textEntry("src/a.ts"),
    ...Array.from({ length: 40 }, (_, index) => binary(`img-${index}.png`)),
    bareRepository("remote.git", 18),
    {
      path: "link",
      origin: "uncommitted",
      insertions: 0,
      deletions: 0,
      kind: "symlink",
      target: "../elsewhere",
    },
  ]);

  // 40 binaries + 1 repository + 1 symlink = 42 entries; 40 + 18 + 1 = 59 paths.
  assert.equal(omitted.entries, 42);
  assert.equal(omitted.paths, 59);
});

test("the clause says paths, and says entries too when they differ", () => {
  const plain = omittedFromEntries(
    Array.from({ length: 63 }, (_, index) => binary(`img-${index}.png`)),
  );
  assert.equal(plain.entries, 63);
  assert.equal(plain.paths, 63);
  assert.equal(omittedClause(plain), "63 non-text paths omitted");

  const aggregated = omittedFromEntries([
    ...Array.from({ length: 45 }, (_, index) => binary(`img-${index}.png`)),
    bareRepository("remote.git", 18),
  ]);
  assert.equal(aggregated.entries, 46);
  assert.equal(aggregated.paths, 63);
  assert.equal(
    omittedClause(aggregated),
    "63 non-text paths omitted in 46 entries",
  );

  assert.equal(omittedClause(NO_OMISSIONS), null);
});

test("one omitted path is singular in both halves of the clause", () => {
  const one = omittedFromEntries([binary("logo.png")]);

  assert.equal(omittedClause(one), "1 non-text path omitted");
});

test("the breakdown is kinds and counts, never a list of filenames", () => {
  const omitted = omittedFromEntries([
    ...Array.from({ length: 40 }, (_, index) => binary(`img-${index}.png`)),
    bareRepository("remote.git", 18),
  ]);

  const detail = omittedBreakdown(omitted);
  assert.equal(detail, "40 binary files \u00B7 18 paths in 1 bare repository");
  assert.doesNotMatch(detail ?? "", /img-0\.png|remote\.git/);
});

test("the breakdown stops being a summary before it becomes a list", () => {
  const omitted = omittedFromEntries([
    binary("a.png"),
    { path: "empty.txt", origin: "uncommitted", insertions: 0, deletions: 0, kind: "empty" },
    { path: "link", origin: "uncommitted", insertions: 0, deletions: 0, kind: "symlink", target: "x" },
    { path: "dir/", origin: "uncommitted", insertions: 0, deletions: 0, kind: "directory", paths: 1, items: 1 },
    { path: "mode.sh", origin: "uncommitted", insertions: 0, deletions: 0, kind: "metadata" },
    { path: "locked", origin: "uncommitted", insertions: 0, deletions: 0, kind: "unread" },
    bareRepository("remote.git", 2),
  ]);

  const parts = (omittedBreakdown(omitted) ?? "").split(" \u00B7 ");
  assert.equal(parts.length, MAX_OMITTED_KINDS + 1);
  assert.equal(parts.at(-1), "1 more");
});

test("two groups on one screen are one omission", () => {
  const a = omittedFromEntries([binary("a.png"), bareRepository("remote.git", 18)]);
  const b = omittedFromEntries([binary("b.png")]);

  const merged = mergeOmitted(a, b);
  assert.equal(merged.entries, 3);
  assert.equal(merged.paths, 20);
  assert.deepEqual(
    merged.kinds.map((kind) => [kindLabel(kind), kind.entries, kind.paths]),
    [
      ["binary files", 2, 2],
      ["bare repository", 1, 18],
    ],
  );

  assert.equal(mergeOmitted(NO_OMISSIONS, b), b);
  assert.equal(mergeOmitted(a, NO_OMISSIONS), a);
});

/* ------------------------------------------------------------------ *
 * Collapse, over a list that is now only patches.
 * ------------------------------------------------------------------ */

test("a short review opens read, a long one opens closed", () => {
  const sections = (count: number) =>
    Array.from({ length: count }, (_, index) =>
      sectionFor(scope, textEntry(`file-${index}.ts`), "uncommitted", group),
    );

  assert.equal(defaultCollapsed(sections(COLLAPSE_THRESHOLD)), false);
  assert.equal(defaultCollapsed(sections(COLLAPSE_THRESHOLD + 1)), true);
});

/**
 * Filtering happens before the threshold is measured, so a group of thirty
 * binaries and two patches opens read rather than pretending to be long.
 */
test("omitted entries do not push the list over the threshold", () => {
  const entries = [
    ...Array.from({ length: COLLAPSE_THRESHOLD + 10 }, (_, index) =>
      binary(`img-${index}.png`),
    ),
    textEntry("src/a.ts"),
  ];

  const sections = textEntries(entries).map((entry) =>
    sectionFor(scope, entry, "uncommitted", group),
  );

  assert.equal(sections.length, 1);
  assert.equal(defaultCollapsed(sections), false);
});

/** A view with no patches has nothing to fold, so no control is drawn. */
test("the global control is hidden when there is nothing to open", () => {
  assert.equal(hasCollapsibleSections([]), false);
  assert.equal(
    hasCollapsibleSections([
      sectionFor(scope, textEntry("src/a.ts"), "uncommitted", group),
    ]),
    true,
  );
});

test("the tree and the sections agree on every anchor", () => {
  const section = sectionFor(scope, textEntry("src/a.ts"), "uncommitted", group);

  assert.equal(section.anchor, anchorId(scope, "uncommitted", "src/a.ts"));
  assert.equal(section.id, section.anchor);
  assert.notEqual(
    anchorId(scope, "committed", "src/a.ts"),
    anchorId(scope, "uncommitted", "src/a.ts"),
  );
});

/**
 * A click about one group lands on that group's heading, so the heading needs
 * an id of its own — and it must not collide with any file's.
 */
test("each group heading is its own scroll target", () => {
  assert.notEqual(
    groupAnchorId(scope, "uncommitted"),
    groupAnchorId(scope, "committed"),
  );
  assert.notEqual(
    groupAnchorId(scope, "uncommitted"),
    anchorId(scope, "uncommitted", "src/a.ts"),
  );
});

/**
 * Two groups, one scroll, and one name each. There used to be five names for
 * three things — "Uncommitted Changes", "Branch History", "All commits", "On
 * disk only", "Already on default" — where a reader could not tell whether two
 * differently-named lists were two views of one thing or two different things.
 *
 * They say where the work *lives*, because that is the question being asked:
 * deleting the directory destroys what is only on disk and leaves what is
 * committed.
 */
test("the groups are named for where the work lives", () => {
  assert.equal(ON_DISK_HEADING, "On disk only");
  assert.equal(COMMITTED_HEADING, "Committed on this branch");
  assert.notEqual(ON_DISK_HEADING, COMMITTED_HEADING);
  // Neither names a scope, because the view no longer has one.
  assert.doesNotMatch(ON_DISK_HEADING, /uncommitted|history/i);
  assert.doesNotMatch(COMMITTED_HEADING, /history/i);
});

/** The view holds things, and not one of them is a diff — said once. */
test("the empty title describes the changes, not a scope", () => {
  assert.equal(NO_LINE_DIFFS_TITLE, "No line-by-line diffs in these changes");
  assert.doesNotMatch(NO_LINE_DIFFS_TITLE, /scope/i);
});

/* ------------------------------------------------------------------ *
 * The reading filter, and the arithmetic it must not disturb.
 * ------------------------------------------------------------------ */

/**
 * A filter offering two segments that render the same paths with the same
 * content is a dead control: it costs a decision, spends a click, and changes
 * nothing on screen.
 */
test("a narrowed reading that shows the same thing is not offered", () => {
  const everything = ["a.ts", "b.ts"].map((path) =>
    sectionFor(scope, textEntry(path), "committed", group),
  );

  assert.equal(readingNarrows(everything, []), false, "nothing to narrow");
  assert.equal(readingNarrows(everything, everything), false, "identical");
  assert.equal(
    readingNarrows(everything, [everything[0]!]),
    true,
    "fewer files is a genuine narrowing",
  );
  assert.equal(
    readingNarrows(everything, [
      everything[0]!,
      { ...everything[1]!, patch: "@@ -1 +1 @@\n+only the risky lines" },
    ]),
    true,
    "the same files with less inside them is one too",
  );
});

/**
 * The summary's identity, added up across every group on screen: what was drawn
 * plus what could not be, and never a line total borrowed from a scan snapshot
 * taken at a different moment.
 */
test("the groups' arithmetic combines into one checkable identity", () => {
  const onDisk = [textEntry("a.ts"), textEntry("b.ts"), binary("logo.png")];
  const committed = [textEntry("c.ts"), bareRepository("remote.git", 18)];
  const onDiskSections = textEntries(onDisk).map((entry) =>
    sectionFor(scope, entry, "uncommitted", group),
  );
  const committedSections = textEntries(committed).map((entry) =>
    sectionFor(scope, entry, "committed", group),
  );

  const balance = combineBalances(
    [
      balanceOf(
        onDiskSections,
        omittedFromEntries(onDisk),
        { insertions: 40, deletions: 2 },
        coverageOf(onDisk),
      ),
      balanceOf(
        committedSections,
        omittedFromEntries(committed),
        { insertions: 8021, deletions: 1123 },
        coverageOf(committed),
      ),
    ],
    0,
  );

  assert.deepEqual(balance, {
    textDiffs: 3,
    changedPaths: 22,
    notDiffable: 19,
    insertions: 8061,
    deletions: 1125,
    residual: 0,
  });
  assert.equal(changedPathTotal(balance), 22);
});

/* ------------------------------------------------------------------ *
 * The release blocker: one path in both groups is one changed path.
 * ------------------------------------------------------------------ */

/**
 * A file committed on this branch and edited again since is in both groups —
 * correctly, and both entries are drawn, because they are two different things
 * to read. It is still one path, and the summary added the two group totals
 * and called it two.
 */
test("a path in both groups is one changed path and two rendered entries", () => {
  const shared = "src/shared.ts";
  const onDisk = [textEntry(shared), textEntry("only-on-disk.ts")];
  const committed = [
    { ...textEntry(shared), origin: "committed" as const },
    { ...textEntry("only-committed.ts"), origin: "committed" as const },
  ];

  const balance = combineBalances(
    [
      balanceOf(
        textEntries(onDisk).map((entry) =>
          sectionFor(scope, entry, "uncommitted", group),
        ),
        omittedFromEntries(onDisk),
        lineTotals(textEntries(onDisk)),
        coverageOf(onDisk),
      ),
      balanceOf(
        textEntries(committed).map((entry) =>
          sectionFor(scope, entry, "committed", group),
        ),
        omittedFromEntries(committed),
        lineTotals(textEntries(committed)),
        coverageOf(committed),
      ),
    ],
    0,
  );

  // Both entries are still rendered: two sections, in two groups, one anchor
  // each, and neither is hidden to make the arithmetic come out.
  assert.equal(balance.textDiffs, 4);
  assert.notEqual(
    anchorId(scope, "uncommitted", shared),
    anchorId(scope, "committed", shared),
  );
  // Three paths, not four: the shared one is counted once.
  assert.equal(changedPathTotal(balance), 3);
  assert.equal(balance.notDiffable, 0);
});

/**
 * The same identity left undrawn in both groups is one undrawn path, and it
 * covers whatever the wider of the two claims stood for.
 */
test("a non-diffable path in both groups is counted once, at its widest", () => {
  const onDisk = [bareRepository("vendor", 18), binary("logo.png")];
  const committed = [bareRepository("vendor", 4)];

  const balance = combineBalances(
    [
      balanceOf([], omittedFromEntries(onDisk), { insertions: 0, deletions: 0 }, coverageOf(onDisk)),
      balanceOf([], omittedFromEntries(committed), { insertions: 0, deletions: 0 }, coverageOf(committed)),
    ],
    0,
  );

  assert.equal(balance.notDiffable, 19);
  assert.equal(changedPathTotal(balance), 19);
});

/**
 * A path drawn in one group and not in the other was drawn: it is one changed
 * path and it is not one the view failed to render.
 */
test("a path drawn in either group is not counted as not diffable", () => {
  const onDisk = [binary("asset.bin")];
  const committed = [{ ...textEntry("asset.bin"), origin: "committed" as const }];

  const balance = combineBalances(
    [
      balanceOf([], omittedFromEntries(onDisk), { insertions: 0, deletions: 0 }, coverageOf(onDisk)),
      balanceOf(
        textEntries(committed).map((entry) =>
          sectionFor(scope, entry, "committed", group),
        ),
        omittedFromEntries(committed),
        lineTotals(textEntries(committed)),
        coverageOf(committed),
      ),
    ],
    0,
  );

  assert.equal(changedPathTotal(balance), 1);
  assert.equal(balance.notDiffable, 0);
});

/**
 * What a limit cut short is outside the union by construction — those paths
 * never arrived, so there is no identity to compare — and stays a term of its
 * own rather than being folded into a total it cannot be checked against.
 */
test("what a limit cut short is stated beside the union, not inside it", () => {
  const shared = "src/shared.ts";
  const onDisk = [textEntry(shared), binary("logo.png")];
  const committed = [{ ...textEntry(shared), origin: "committed" as const }];

  const balance = combineBalances(
    [
      balanceOf(
        textEntries(onDisk).map((entry) =>
          sectionFor(scope, entry, "uncommitted", group),
        ),
        omittedFromEntries(onDisk),
        lineTotals(textEntries(onDisk)),
        coverageOf(onDisk),
      ),
      balanceOf(
        textEntries(committed).map((entry) =>
          sectionFor(scope, entry, "committed", group),
        ),
        omittedFromEntries(committed),
        lineTotals(textEntries(committed)),
        coverageOf(committed),
      ),
    ],
    49,
  );

  assert.equal(balance.textDiffs, 2);
  assert.equal(changedPathTotal(balance), 2, "the shared path, and the binary");
  assert.equal(balance.notDiffable, 1);
  assert.equal(balance.residual, 49);
});

test("coverage keys on path identity, and takes the widest claim", () => {
  const coverage = coverageOf([
    textEntry("a.ts"),
    bareRepository("vendor", 18),
    binary("logo.png"),
  ]);

  assert.deepEqual(coverage.get("a.ts"), { paths: 1, diffable: true });
  assert.deepEqual(coverage.get("vendor"), { paths: 18, diffable: false });
  assert.deepEqual(coverage.get("logo.png"), { paths: 1, diffable: false });
  assert.equal(coverage.size, 3);
});

test("line totals are summed from the entries, not from the sections", () => {
  assert.deepEqual(
    lineTotals(
      textEntries([textEntry("a.ts"), textEntry("b.ts"), binary("logo.png")]),
    ),
    { insertions: 2, deletions: 0 },
  );
});

/**
 * A limit that stops a listing does not make the paths behind it stop existing.
 * Taken as a maximum rather than a sum: the display limit, the inspection limit
 * and the byte truncation all describe the same untracked shortfall from
 * different sides, and adding them counts one missing path three times.
 */
test("what a limit cut short is counted once, however many limits said so", () => {
  const patches = (over: Partial<Patches>): Patches => ({
    scope: "history",
    committed: [],
    uncommitted: [],
    truncated: false,
    incomplete: false,
    untrackedTotal: 0,
    untrackedShown: 0,
    untrackedEntries: 0,
    limits: [],
    ...over,
  });

  assert.equal(residualPaths(patches({})), 0);
  assert.equal(
    residualPaths(
      patches({ limits: [{ kind: "displayLimit", shown: 99, total: 148 }] }),
    ),
    49,
  );
  assert.equal(
    residualPaths(
      patches({
        truncated: true,
        untrackedTotal: 148,
        untrackedShown: 99,
        limits: [
          { kind: "displayLimit", shown: 99, total: 148 },
          { kind: "inspectionLimit", shown: 99, total: 148, limit: 99 },
        ],
      }),
    ),
    49,
    "three descriptions of one shortfall are one shortfall",
  );
  // A limit that named no shortfall cannot make the total negative.
  assert.equal(
    residualPaths(
      patches({ limits: [{ kind: "displayLimit", shown: 148, total: 148 }] }),
    ),
    0,
  );
});