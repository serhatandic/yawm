/**
 * The detail panel's width, against the window it is actually being drawn in.
 *
 * The stored number is a preference about a window that may no longer exist —
 * an external monitor last week, a laptop now — so the rule that turns it into
 * pixels is the one thing standing between "the panel is wide" and "the panel
 * has swallowed the list it exists to explain".
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  DEFAULT_PANEL_WIDTH,
  MAX_PANEL_FRACTION,
  MIN_PANEL_WIDTH,
  clampPanelWidth,
} from "../src/lib/layout.ts";

const source = readFileSync(
  new URL("../src/lib/columns.ts", import.meta.url),
  "utf8",
);

test("a width restored from a wider window is cut down to this one", () => {
  // 900px saved on a 1600px monitor, reopened on a 1280px laptop.
  assert.equal(clampPanelWidth(900, 1280), Math.round(1280 * MAX_PANEL_FRACTION));
});

test("a width the window can honour is left exactly alone", () => {
  assert.equal(clampPanelWidth(480, 1600), 480);
  assert.equal(clampPanelWidth(DEFAULT_PANEL_WIDTH, 1600), DEFAULT_PANEL_WIDTH);
});

test("the minimum wins over the ceiling when the two cross", () => {
  // Under about 500px of window the fraction is narrower than the minimum. A
  // panel below its minimum collides with itself, which is the worse failure.
  assert.equal(clampPanelWidth(320, 400), MIN_PANEL_WIDTH);
  assert.equal(clampPanelWidth(120, 1600), MIN_PANEL_WIDTH);
});

test("nonsense in storage lands on the default rather than NaN", () => {
  assert.equal(clampPanelWidth(Number.NaN, 1600), DEFAULT_PANEL_WIDTH);
});

/**
 * Restoring is not the only moment the number can go stale: the window can be
 * dragged narrower afterwards, and without a re-clamp the panel simply kept
 * its old pixel width over the list.
 */
test("the hook clamps on restore and again on resize", () => {
  const at = source.indexOf("export function usePanelWidth()");
  assert.ok(at > 0, "the hook moved; this test has to follow it");
  const hook = source.slice(at, source.indexOf("export function", at + 10));

  assert.match(hook, /localStorage\.getItem\(PANEL_KEY\)[\s\S]*clampPanelWidth\(/);
  assert.match(hook, /addEventListener\("resize"/);
  assert.match(
    hook,
    /setWidth\(clampPanelWidth\(preferredWidth\.current, window\.innerWidth\)\)/,
  );
  assert.match(
    hook,
    /preferredWidth\.current = Number\(raw\) \|\| DEFAULT_PANEL_WIDTH/,
  );
  assert.match(hook, /preferredWidth\.current = capped/);
  // The preference itself is not rewritten by a temporarily small window, or
  // widening the window again would never give the panel its size back.
  const onResize = hook.slice(hook.indexOf('addEventListener("resize"') - 400);
  assert.doesNotMatch(
    onResize.slice(0, onResize.indexOf("const resize")),
    /setItem/,
  );
});
