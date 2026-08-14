/**
 * Run with: node --experimental-strip-types --test tests/at-risk-hunks.test.ts
 *
 * The bug this replaces: a branch with 8,069 added lines and 17 of them
 * unmerged rendered all 8,069, in the green the rest of the app uses for
 * "disposable", with the 17 marked inside it. Every case here is about the
 * narrowed patch containing the finding and almost nothing else, and about the
 * one polarity this analysis has already shipped a bug over — a line the
 * branch removed that the default branch still has.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  narrowToRiskHunks,
  type RiskMarker,
} from "../src/components/at-risk-hunks.ts";

const file = (...lines: string[]) =>
  ["diff --git a/a.ts b/a.ts", "--- a/a.ts", "+++ b/a.ts", ...lines].join("\n");

const addition = (lineNumber: number): RiskMarker => ({
  path: "a.ts",
  side: "additions",
  lineNumber,
});

const deletion = (lineNumber: number): RiskMarker => ({
  path: "a.ts",
  side: "deletions",
  lineNumber,
});

/** The rendered rows, without the file header the renderer draws itself. */
const body = (patch: string) =>
  patch.split("\n").filter((line) => !line.startsWith("diff --git") && !line.startsWith("--- ") && !line.startsWith("+++ "));

test("one unmerged line among hundreds renders a window, not the file", () => {
  const lines: string[] = [];
  for (let n = 1; n <= 230; n += 1) lines.push(`+line ${n}`);
  const patch = file("@@ -0,0 +1,230 @@", ...lines);

  const narrowed = narrowToRiskHunks(patch, [addition(120)]);

  assert.deepEqual(body(narrowed), [
    "@@ -117,6 +117,7 @@",
    " line 117",
    " line 118",
    " line 119",
    "+line 120",
    " line 121",
    " line 122",
    " line 123",
  ]);
});

test("a landed addition is context, indistinguishable from code that never moved", () => {
  const patch = file(
    "@@ -1,2 +1,5 @@",
    " untouched",
    "+landed on main",
    "+still only here",
    "+landed on main too",
    " untouched",
  );

  const narrowed = narrowToRiskHunks(patch, [addition(3)]);

  assert.deepEqual(body(narrowed), [
    "@@ -1,4 +1,5 @@",
    " untouched",
    " landed on main",
    "+still only here",
    " landed on main too",
    " untouched",
  ]);
});

test("a removed line keeps its minus sign and sits where it used to", () => {
  const patch = file(
    "@@ -1,5 +1,4 @@",
    " one",
    " two",
    "-three",
    " four",
    " five",
  );

  const narrowed = narrowToRiskHunks(patch, [deletion(3)]);

  assert.deepEqual(body(narrowed), [
    "@@ -1,5 +1,4 @@",
    " one",
    " two",
    "-three",
    " four",
    " five",
  ]);
});

test("a removal that already landed is dropped, not shown as context", () => {
  const patch = file(
    "@@ -1,6 +1,3 @@",
    " one",
    "-landed removal",
    "-the unmerged removal",
    "-landed removal",
    " two",
    " three",
  );

  const narrowed = narrowToRiskHunks(patch, [deletion(3)]);

  assert.deepEqual(body(narrowed), [
    "@@ -1,4 +1,3 @@",
    " one",
    "-the unmerged removal",
    " two",
    " three",
  ]);
});

test("adjacent unmerged lines share one window rather than repeating context", () => {
  const lines: string[] = [];
  for (let n = 1; n <= 40; n += 1) lines.push(`+line ${n}`);
  const patch = file("@@ -0,0 +1,40 @@", ...lines);

  const narrowed = narrowToRiskHunks(patch, [addition(20), addition(21)]);

  assert.deepEqual(body(narrowed), [
    "@@ -17,6 +17,8 @@",
    " line 17",
    " line 18",
    " line 19",
    "+line 20",
    "+line 21",
    " line 22",
    " line 23",
    " line 24",
  ]);
});

test("windows three lines apart merge into one instead of splitting", () => {
  const lines: string[] = [];
  for (let n = 1; n <= 40; n += 1) lines.push(`+line ${n}`);
  const patch = file("@@ -0,0 +1,40 @@", ...lines);

  const narrowed = narrowToRiskHunks(patch, [addition(20), addition(24)]);

  const rows = body(narrowed);
  assert.equal(rows.filter((row) => row.startsWith("@@")).length, 1);
  assert.deepEqual(rows[0], "@@ -17,9 +17,11 @@");
  assert.equal(rows.length, 12);
});

test("windows far apart stay separate hunks", () => {
  const lines: string[] = [];
  for (let n = 1; n <= 200; n += 1) lines.push(`+line ${n}`);
  const patch = file("@@ -0,0 +1,200 @@", ...lines);

  const narrowed = narrowToRiskHunks(patch, [addition(20), addition(120)]);

  const headers = body(narrowed).filter((row) => row.startsWith("@@"));
  assert.deepEqual(headers, ["@@ -17,6 +17,7 @@", "@@ -117,6 +117,7 @@"]);
});

test("an unmerged line at the start of a file takes the context that exists", () => {
  const patch = file(
    "@@ -0,0 +1,6 @@",
    "+first",
    "+second",
    "+third",
    "+fourth",
    "+fifth",
    "+sixth",
  );

  const narrowed = narrowToRiskHunks(patch, [addition(1)]);

  assert.deepEqual(body(narrowed), [
    "@@ -1,3 +1,4 @@",
    "+first",
    " second",
    " third",
    " fourth",
  ]);
});

test("an unmerged line at the end of a file takes the context that exists", () => {
  const patch = file(
    "@@ -0,0 +1,5 @@",
    "+one",
    "+two",
    "+three",
    "+four",
    "+five",
    "\\ No newline at end of file",
  );

  const narrowed = narrowToRiskHunks(patch, [addition(5)]);

  assert.deepEqual(body(narrowed), [
    "@@ -2,3 +2,4 @@",
    " two",
    " three",
    " four",
    "+five",
    "\\ No newline at end of file",
  ]);
});

test("hunk boundaries are not crossed when gathering context", () => {
  const patch = file(
    "@@ -10,3 +10,4 @@",
    " a",
    "+unmerged",
    " b",
    " c",
    "@@ -80,3 +81,3 @@",
    " x",
    " y",
    " z",
  );

  const narrowed = narrowToRiskHunks(patch, [addition(11)]);

  assert.deepEqual(body(narrowed), [
    "@@ -10,3 +10,4 @@",
    " a",
    "+unmerged",
    " b",
    " c",
  ]);
});

test("a file with no markers is left whole, because it was never compared", () => {
  const patch = file("@@ -1,2 +1,2 @@", " a", "-b", "+c");
  assert.equal(narrowToRiskHunks(patch, []), patch);
});

test("a binary file is left whole", () => {
  const patch = [
    "diff --git a/logo.png b/logo.png",
    "Binary files a/logo.png and b/logo.png differ",
  ].join("\n");
  assert.equal(narrowToRiskHunks(patch, [addition(1)]), patch);
});

test("markers naming lines this patch does not have leave it whole", () => {
  const patch = file("@@ -1,1 +1,2 @@", " a", "+b");
  assert.equal(narrowToRiskHunks(patch, [addition(900)]), patch);
});

test("an addition and a removal in one window keep their separate signs", () => {
  const patch = file(
    "@@ -1,4 +1,4 @@",
    " one",
    "-old value",
    "+new value",
    " two",
    " three",
  );

  const narrowed = narrowToRiskHunks(patch, [addition(2), deletion(2)]);

  assert.deepEqual(body(narrowed), [
    "@@ -1,4 +1,4 @@",
    " one",
    "-old value",
    "+new value",
    " two",
    " three",
  ]);
});
