import { test } from "node:test";
import assert from "node:assert/strict";

import {
  comparisonShortfallSentence,
  reasonDetail,
  reasonLabel,
  type VerdictReason,
} from "../src/lib/api.ts";

test("unique environment files are named rather than called uncommitted changes", () => {
  const reason = {
    kind: "environmentFilesAtRisk",
    count: 2,
  } as unknown as VerdictReason;

  assert.equal(reasonLabel(reason), "2 environment files are in no repository");
  assert.match(reasonDetail(reason), /no matching copy in the main worktree/);
});

test("working tree inspection failure does not blame landing analysis", () => {
  const reason = {
    kind: "workingTreeUnreadable",
  } as unknown as VerdictReason;

  assert.equal(reasonLabel(reason), "Working tree could not be read");
  assert.match(reasonDetail(reason), /uncommitted files/);
  assert.doesNotMatch(reasonDetail(reason), /land/i);
});

test("uncommitted content found on default is not described as existing nowhere else", () => {
  const reason = {
    kind: "uncommittedChangesOnDefault",
    target: "origin/main",
  } as const;

  assert.equal(
    reasonLabel(reason),
    "Uncommitted content is already on origin/main",
  );
  assert.match(reasonDetail(reason), /not committed anywhere/);
  assert.match(reasonDetail(reason), /already reflected on origin\/main/);
  assert.doesNotMatch(reasonDetail(reason), /nowhere else|deleting.*loses/i);
});

test("unfinished uncommitted analysis never says nothing is at risk", () => {
  const reason = { kind: "uncommittedChanges" } as const;

  assert.match(reasonDetail(reason), /not been verified/);
  assert.doesNotMatch(reasonDetail(reason), /already|nothing.*risk/i);
});

test("deliberately deferred landing analysis is not rendered as a failed proof", async () => {
  const { isLandingCheckDeferred } = await import("../src/lib/api.ts");

  assert.equal(
    isLandingCheckDeferred({
      state: "unknown",
      reason: { kind: "checkDeferred" },
      candidate: null,
    }),
    true,
  );
  assert.equal(
    isLandingCheckDeferred({
      state: "unknown",
      reason: { kind: "gitCommandFailed" },
      candidate: null,
    }),
    false,
  );
});

test("a capped comparison names the limit and the shortfall instead of saying some", () => {
  const reason = {
    kind: "uncommittedChangesAtRisk",
    count: 256,
    target: "origin/main",
    incomplete: true,
    shortfall: {
      linesCompared: 256,
      linesNotCompared: 1244,
      lineLimit: 256,
      pathsNotCompared: 0,
      countsExact: true,
    },
  } as const;

  const detail = reasonDetail(reason);

  assert.match(detail, /read 256 of 1,500 changed lines/);
  assert.match(detail, /stopping at its 256-line limit/);
  assert.match(detail, /1,244 lines were not read/);
  assert.doesNotMatch(detail, /\bsome\b|\bmay\b|could not verify/i);
});

test("a shortfall with no line budget involved does not quote a line budget", () => {
  const detail = comparisonShortfallSentence("origin/main", {
    linesCompared: 40,
    linesNotCompared: 0,
    lineLimit: null,
    pathsNotCompared: 3,
    countsExact: true,
  });

  assert.equal(
    detail,
    "The comparison with origin/main could not read 3 paths line by line.",
  );
  assert.doesNotMatch(detail, /limit/);
});

test("counts that are only lower bounds are stated as lower bounds", () => {
  const detail = comparisonShortfallSentence("origin/main", {
    linesCompared: 12,
    linesNotCompared: 30,
    lineLimit: null,
    pathsNotCompared: 2,
    countsExact: false,
  });

  assert.equal(
    detail,
    "The comparison with origin/main read 12 of at least 42 changed lines so at least 30 lines were not read, and could not read at least 2 paths line by line.",
  );
});

test("an at-risk reason without a shortfall makes no claim about unread work", () => {
  const detail = reasonDetail({
    kind: "uncommittedChangesAtRisk",
    count: 3,
    target: "origin/main",
    incomplete: false,
    shortfall: null,
  });

  assert.match(detail, /3 changed lines are absent from origin\/main/);
  assert.doesNotMatch(detail, /not read|limit/);
});
