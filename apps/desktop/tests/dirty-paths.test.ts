import { test } from "node:test";
import assert from "node:assert/strict";

import { dirtyPathCount, type DirtyCounts } from "../src/lib/api.ts";

/**
 * How many files are dirty, and how they are dirty, are two different numbers.
 *
 * `staged + unstaged + untracked` counts *status dimensions*: a path that is
 * staged and then modified again is two of them. Adding them up is how a
 * worktree of 257 changed files came to be labelled "404 uncommitted files" in
 * the sidebar while the Changes view beside it drew 257 — two numbers with no
 * stated relationship, which reads as one of them being wrong.
 *
 * Core counts distinct paths from Git's own path bytes and sends them as
 * `paths`. That is the only number ever shown as a file count; the three
 * dimensions stay, in the breakdown, where they answer the question they
 * actually answer.
 */

const counts = (over: Partial<DirtyCounts> = {}): DirtyCounts => ({
  staged: 0,
  unstaged: 0,
  untracked: 0,
  paths: 0,
  inspectionFailed: false,
  ...over,
});

test("the file count is the distinct paths, never the sum of the dimensions", () => {
  const dirty = counts({
    staged: 3,
    unstaged: 12,
    untracked: 133,
    paths: 145,
  });

  assert.equal(dirtyPathCount(dirty), 145);
  assert.notEqual(
    dirtyPathCount(dirty),
    dirty.staged + dirty.unstaged + dirty.untracked,
  );
});

test("one path that is staged and modified again is one file", () => {
  assert.equal(dirtyPathCount(counts({ staged: 1, unstaged: 1, paths: 1 })), 1);
});

/**
 * `paths` is serialised with a default, so a payload from an older core can
 * arrive as zero while the dimensions say otherwise. The fallback is the
 * largest single dimension — the smallest number of distinct paths that could
 * produce them — because the one thing that must never happen again is the sum
 * standing in for a file count.
 */
test("an older payload falls back to the largest dimension, not the sum", () => {
  const dirty = counts({ staged: 3, unstaged: 12, untracked: 133 });

  assert.equal(dirtyPathCount(dirty), 133);
  assert.ok(dirtyPathCount(dirty) < 3 + 12 + 133);
});

test("a clean worktree counts zero rather than falling through to a guess", () => {
  assert.equal(dirtyPathCount(counts()), 0);
});
