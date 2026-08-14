import { test } from "node:test";
import assert from "node:assert/strict";

import {
  AT_RISK_READING_LABEL,
  EVERYTHING_READING_LABEL,
  atRiskClause,
  changedPathTotal,
  changesSummarySegments,
  diffLimitMessages,
  limitMessage,
  limitRemedy,
  noBranchCommitsClause,
  statLineText,
  unmergedLinesByFile,
  type ChangesBalance,
  type Patches,
  type UniqueLineMarker,
  type UniquePatch,
} from "../src/lib/api.ts";

/**
 * The one line of scale above the combined Changes view.
 *
 * Its job is an identity the reader can check: every path this view knows
 * about is either a text diff it drew or a path it could not draw, and what a
 * limit cut short is stated rather than absorbed. The header used to print two
 * true numbers with no stated relationship — "257 files" beside a sidebar
 * saying 404 — which reads as one of them being wrong.
 */

const balance = (over: Partial<ChangesBalance> = {}): ChangesBalance => {
  const held = {
    textDiffs: 97,
    notDiffable: 0,
    insertions: 8021,
    deletions: 1123,
    residual: 0,
    ...over,
  };
  return {
    ...held,
    /*
     * Distinct paths, which is normally what was drawn plus what could not be
     * — but not when one path is in both groups, so a case that overlaps says
     * so explicitly.
     */
    changedPaths: over.changedPaths ?? held.textDiffs + held.notDiffable,
  };
};

const line = (
  over: Partial<Parameters<typeof changesSummarySegments>[0]> = {},
) =>
  statLineText(
    changesSummarySegments({
      balance: balance(),
      atRisk: null,
      leadingClause: null,
      ...over,
    }),
  );

/**
 * The reported bug, exactly.
 *
 * The analysis found 17 unmerged lines across 5 files. The patch it renders
 * carries the surrounding diff so those lines can be read in place, so the
 * patch itself is +660 −1. The header counted the patch and captioned it
 * "unmerged to origin/main", contradicting the detail panel three inches below.
 */
const unmerged: UniquePatch = {
  patch: [
    "diff --git a/a.ts b/a.ts",
    "--- a/a.ts",
    "+++ b/a.ts",
    "@@ -1,1 +1,4 @@",
    " context",
    "+the unmerged one",
  ].join("\n"),
  lineCount: 17,
  fileCount: 5,
  candidate: "4633374aaaaaaaa",
  target: "origin/main",
  markers: [],
  incomplete: false,
  truncated: false,
};

const emptyPatches: Patches = {
  scope: "history",
  committed: [],
  uncommitted: [],
  truncated: false,
  incomplete: false,
  untrackedTotal: 0,
  untrackedShown: 0,
  untrackedEntries: 0,
  limits: [],
};

/* ------------------------------------------------------------------ *
 * The identity: drawn plus not drawable, and never a number without one.
 * ------------------------------------------------------------------ */

test("the parts add up to the total, and the total is printed once", () => {
  const held = balance({ textDiffs: 97, notDiffable: 63 });

  assert.equal(changedPathTotal(held), 160);
  assert.equal(
    line({ balance: held }),
    "160 changed paths · 97 text diffs · 63 not diffable · +8,021 −1,123",
  );
});

/**
 * `257 changed paths · 257 text diffs` is an identity with one term, which
 * reads as the same number said twice rather than as arithmetic.
 */
test("the path total is omitted when it is the diff count said twice", () => {
  assert.equal(line(), "97 text diffs · +8,021 −1,123");
});

/**
 * The release blocker, on the line that states it.
 *
 * A file committed on this branch and edited again since is drawn twice, in
 * two groups, and is one changed path. Nothing here is omitted, so the old
 * rule — print the path total only when something could not be diffed — would
 * have printed "2 text diffs" for one path and left the reader to guess which
 * number described the worktree.
 */
test("a path drawn in both groups is one changed path beside two diffs", () => {
  assert.equal(
    line({
      balance: balance({
        textDiffs: 2,
        notDiffable: 0,
        changedPaths: 1,
        insertions: 12,
        deletions: 3,
      }),
    }),
    "1 changed path · 2 text diffs · +12 −3",
  );
});

test("overlap and omission are both counted once, and stated together", () => {
  const held = balance({
    textDiffs: 4,
    notDiffable: 19,
    changedPaths: 22,
    residual: 49,
  });

  assert.equal(changedPathTotal(held), 22);
  assert.equal(
    line({ balance: held }),
    "22 changed paths · 4 text diffs · 19 not diffable · +8,021 −1,123 · 49 not read",
  );
});

test("one of each is singular on both sides of the identity", () => {
  assert.equal(
    line({
      balance: balance({
        textDiffs: 0,
        notDiffable: 1,
        insertions: 0,
        deletions: 0,
      }),
    }),
    "1 changed path · No text diffs · 1 not diffable",
  );
  assert.equal(
    line({ balance: balance({ textDiffs: 1, insertions: 4, deletions: 0 }) }),
    "1 text diff · +4 −0",
  );
});

/**
 * A view holding nothing but binaries and nested repositories ran no line
 * comparison at all, and `+0 −0` claims one that never happened.
 */
test("nothing compared prints no line totals", () => {
  const text = line({
    balance: balance({
      textDiffs: 0,
      notDiffable: 63,
      insertions: 0,
      deletions: 0,
    }),
  });

  assert.equal(text, "63 changed paths · No text diffs · 63 not diffable");
  assert.doesNotMatch(text, /\+0 −0/);
});

/**
 * A limit that stops a listing does not make the paths behind it stop
 * existing. Without this the identity balances against itself while quietly
 * disagreeing with the worktree it describes.
 */
test("paths a limit cut short are stated, not absorbed", () => {
  assert.equal(
    line({ balance: balance({ textDiffs: 99, residual: 49 }) }),
    "99 text diffs · +8,021 −1,123 · 49 not read",
  );
});

/* ------------------------------------------------------------------ *
 * The at-risk reading: its own numerator, against the same denominator.
 * ------------------------------------------------------------------ */

test("the number beside \"at risk\" is the analysis's line count", () => {
  const text = line({
    balance: balance({ textDiffs: 97, notDiffable: 63 }),
    atRisk: unmerged,
  });

  assert.equal(text, "17 lines at risk in 5 files · of 160 changed paths");
  // +660 −1 was the rendered excerpt's own size. An excerpt's length answers
  // no question the reader has, so it is not on the line in any caption.
  assert.doesNotMatch(text, /\+660/);
  assert.doesNotMatch(text, /[+−-]\d+ [+−-]\d+ at risk/);
});

/**
 * The denominator describes the worktree, not the reading. Pressing a filter
 * cannot change how many paths the worktree is holding, so both readings state
 * the same total and the reader can switch without re-deriving anything.
 */
test("both readings count the same changed paths", () => {
  const held = balance({ textDiffs: 97, notDiffable: 63 });
  const everything = line({ balance: held });
  const atRisk = line({ balance: held, atRisk: unmerged });

  const total = /(\d[\d,]*) changed paths/;
  assert.equal(total.exec(everything)?.[1], "160");
  assert.equal(total.exec(atRisk)?.[1], "160");
});

test("an unfinished search says so rather than understating", () => {
  assert.match(
    line({ atRisk: { ...unmerged, incomplete: true, lineCount: 1 } }),
    /^At least 1 line at risk/,
  );
});

test("nothing at risk is said in words, not as a zero", () => {
  assert.equal(
    statLineText(atRiskClause({ ...unmerged, lineCount: 0 })),
    "Nothing at risk",
  );
  assert.doesNotMatch(
    line({ atRisk: { ...unmerged, lineCount: 0 } }),
    /in 5 files/,
  );
});

test("without the analysis there is no at-risk clause at all", () => {
  assert.doesNotMatch(line(), /at risk/);
});

/* ------------------------------------------------------------------ *
 * A branch that committed nothing: one sentence, no empty group.
 * ------------------------------------------------------------------ */

/**
 * The old view drew an empty "Branch History" group under a heading promising
 * commits, and `+0 −0` beneath it — which is indistinguishable from "the
 * comparison failed". There is no group to draw; there is a fact, and it is
 * stated once, first, in the summary.
 */
test("a branch with no commits of its own says so before anything else", () => {
  const text = line({
    balance: balance({ textDiffs: 4, insertions: 40, deletions: 2 }),
    leadingClause: noBranchCommitsClause("origin/main"),
  });

  assert.equal(
    text,
    "No branch-only commits relative to origin/main · 4 text diffs · +40 −2",
  );
  assert.match(text, /^No branch-only commits/);
});

test("an unknown target is not named as one", () => {
  assert.equal(noBranchCommitsClause(null), "No branch-only commits");
});

test("a worktree holding nothing at all says so without zeroes", () => {
  const text = line({
    balance: balance({
      textDiffs: 0,
      notDiffable: 0,
      insertions: 0,
      deletions: 0,
    }),
    leadingClause: noBranchCommitsClause("origin/main"),
  });

  assert.equal(text, "No branch-only commits relative to origin/main · No text diffs");
  assert.doesNotMatch(text, /\+0 −0/);
  assert.doesNotMatch(text, /0 changed paths/);
});

/** Two readings of one payload, and neither is named after a group. */
test("the readings are named as readings", () => {
  assert.equal(AT_RISK_READING_LABEL, "At risk");
  assert.equal(EVERYTHING_READING_LABEL, "Everything");
  assert.notEqual(AT_RISK_READING_LABEL, EVERYTHING_READING_LABEL);
  // A count inside either would be read as part of its name, and the summary
  // beside them already states one.
  assert.doesNotMatch(AT_RISK_READING_LABEL, /\d/);
  assert.doesNotMatch(EVERYTHING_READING_LABEL, /\d/);
});

/* ------------------------------------------------------------------ *
 * Per-file counts, in the units their heading claims.
 * ------------------------------------------------------------------ */

/**
 * `logStyles.ts +0 −0` was a file the tree had no stat for at all. A zero there
 * claims the file was compared and found clean, which is the one thing it must
 * never say when it does not know.
 */
test("per-file unmerged counts come from the markers, and sum to the total", () => {
  const markers: UniqueLineMarker[] = [
    { path: "a.ts", side: "additions", lineNumber: 4 },
    { path: "a.ts", side: "additions", lineNumber: 5 },
    { path: "a.ts", side: "deletions", lineNumber: 9 },
    { path: "b.ts", side: "deletions", lineNumber: 2 },
  ];

  const byFile = unmergedLinesByFile(markers);

  assert.deepEqual(byFile.get("a.ts"), { insertions: 2, deletions: 1 });
  assert.deepEqual(byFile.get("b.ts"), { insertions: 0, deletions: 1 });
  const total = [...byFile.values()].reduce(
    (n, file) => n + file.insertions + file.deletions,
    0,
  );
  assert.equal(total, markers.length);
});

test("a file with no markers has no count rather than a zero", () => {
  const byFile = unmergedLinesByFile([
    { path: "a.ts", side: "additions", lineNumber: 1 },
  ]);

  assert.equal(byFile.has("too-big.ts"), false);
});

test("a deletion-only file is counted, not rendered as nothing", () => {
  const byFile = unmergedLinesByFile([
    { path: "logStyles.ts", side: "deletions", lineNumber: 12 },
  ]);

  assert.deepEqual(byFile.get("logStyles.ts"), { insertions: 0, deletions: 1 });
});

/* ------------------------------------------------------------------ *
 * The vague banner, replaced by arithmetic.
 * ------------------------------------------------------------------ */

test("an incomplete untracked list states the cause and the quantity", () => {
  assert.equal(
    limitMessage({ kind: "displayLimit", shown: 99, total: 148 }),
    "Showing 99 of 148 untracked paths; 49 were not read because the display limit was reached.",
  );
});

test("skipped paths are named, counted, and given something to run", () => {
  const limit = {
    kind: "tooLarge" as const,
    paths: ["big.bin", "huge.log"],
    total: 3,
  };

  assert.equal(
    limitMessage(limit),
    "3 files are too large to show here: \u201Cbig.bin\u201D and \u201Chuge.log\u201D and 1 more.",
  );
  assert.equal(limitRemedy(limit, "/w/tree"), "git -C /w/tree status --short");
  assert.equal(
    limitRemedy(limit, "/w/my tree"),
    "git -C '/w/my tree' status --short",
  );
});

test("a failed listing says what is missing rather than guessing", () => {
  assert.equal(
    limitMessage({ kind: "listingFailed" }),
    "Git could not list this worktree's untracked files, so none are shown.",
  );
});

test("nothing missing produces no notice at all", () => {
  assert.deepEqual(diffLimitMessages(emptyPatches), []);
});

test("a byte-truncated patch does not look complete", () => {
  assert.deepEqual(
    diffLimitMessages({
      ...emptyPatches,
      truncated: true,
      untrackedTotal: 12,
      untrackedShown: 4,
    }),
    [
      "Showing 4 of 12 changes; the rest were not rendered because the display limit was reached.",
    ],
  );
});

test("no notice repeats itself when the backend already named the limit", () => {
  assert.deepEqual(
    diffLimitMessages({
      ...emptyPatches,
      truncated: true,
      untrackedTotal: 148,
      untrackedShown: 99,
      limits: [{ kind: "displayLimit", shown: 99, total: 148 }],
    }),
    [
      "Showing 99 of 148 untracked paths; 49 were not read because the display limit was reached.",
    ],
  );
});
