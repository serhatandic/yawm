import { test } from "node:test";
import assert from "node:assert/strict";

import { preloadPatchDiff } from "@pierre/diffs/ssr";

import { patchDiffOptions } from "../src/components/diff-options.ts";

/**
 * The contract this file holds is "a valid patch draws lines".
 *
 * Everything else about the diff pane is pure and already tested: which
 * sections exist, which start collapsed, what a caret does. None of that
 * catches the failure that actually shipped — every text row toggling its
 * caret correctly and opening onto nothing, because `@pierre/theming` loads
 * `pierre-dark` through a dynamic import of `@pierre/theme`, that package was
 * not resolvable from the bundle, and a rejected theme load leaves the
 * renderer with no hunks and an empty `<pre>` forever.
 *
 * `preloadPatchDiff` is the renderer's own server path: same patch parser,
 * same theme resolution, same hunk renderer, same markup as the browser
 * builds inside the custom element's shadow root. Rendering through it with
 * the very options the component passes turns "the body is blank" into an
 * assertion, without a browser.
 */

/**
 * Exactly what `render_untracked_patch` in `crates/yawm-core/src/diff/mod.rs`
 * synthesises for an untracked text file: a `/dev/null` new-file patch.
 */
const UNTRACKED_TEXT_PATCH = [
  "diff --git a/bulk/file-001.txt b/bulk/file-001.txt",
  "new file mode 100644",
  "--- /dev/null",
  "+++ b/bulk/file-001.txt",
  "@@ -0,0 +1,2 @@",
  "+bulk acceptance file 001",
  "+line two",
  "",
].join("\n");

const TRACKED_EDIT_PATCH = [
  "diff --git a/src/main.rs b/src/main.rs",
  "--- a/src/main.rs",
  "+++ b/src/main.rs",
  "@@ -1,2 +1,2 @@",
  " fn main() {",
  "-    old();",
  "+    fresh();",
  "",
].join("\n");

async function render(patch: string, atRisk = false) {
  const { prerenderedHTML } = await preloadPatchDiff({
    patch,
    options: patchDiffOptions({ atRisk, diffStyle: "unified" }),
  });
  return prerenderedHTML;
}

/** The `<pre>` is where the code goes; an empty one is the blank body. */
function codeBody(html: string): string {
  const open = html.indexOf("<pre");
  assert.notEqual(open, -1, "renderer produced no <pre> at all");
  const close = html.indexOf("</pre>", open);
  assert.notEqual(close, -1, "renderer produced an unterminated <pre>");
  return html.slice(open, close + "</pre>".length);
}

/** How many code lines the body actually drew. */
function lineCount(body: string): number {
  return body.split("data-line=").length - 1;
}

/** The body's visible text, with the highlighter's token spans taken off. */
function visibleText(body: string): string {
  return body.replace(/<[^>]*>/g, "");
}

test("an untracked text file's synthesised patch draws its added lines", async () => {
  const body = codeBody(await render(UNTRACKED_TEXT_PATCH));

  assert.equal(
    lineCount(body),
    2,
    `expected both added lines to be drawn, got: ${body}`,
  );
  const text = visibleText(body);
  assert.ok(
    text.includes("bulk acceptance file 001"),
    `first added line missing from the rendered body: ${body}`,
  );
  assert.ok(text.includes("line two"), "second added line missing");
  assert.ok(
    body.includes('data-line-type="change-addition"'),
    "added lines were not marked as additions",
  );
});

test("a tracked edit draws both sides of the change", async () => {
  const body = codeBody(await render(TRACKED_EDIT_PATCH));

  const text = visibleText(body);
  assert.equal(lineCount(body), 3, `expected three drawn lines, got: ${body}`);
  assert.ok(text.includes("fresh();"), "the addition is missing");
  assert.ok(text.includes("old();"), "the deletion is missing");
  // A colour on a token is the theme having loaded rather than been skipped.
  assert.match(body, /style="color:#/, "the theme never reached the tokens");
});

test("the at-risk styling does not cost the body its lines", async () => {
  const body = codeBody(await render(TRACKED_EDIT_PATCH, true));

  assert.equal(lineCount(body), 3, `expected three drawn lines, got: ${body}`);
});

/**
 * The failure mode named directly.
 *
 * A theme that cannot load is the one way a valid patch has produced a body
 * with a `<pre>` in it and nothing inside. Asserting on the theme's own
 * resolution says why the test above would fail rather than just that it did.
 */
test("the configured diff theme is resolvable from this package tree", async () => {
  const { themes } = await import("@pierre/theming/themes");
  const { theme } = patchDiffOptions({ atRisk: false, diffStyle: "unified" });

  const descriptor = themes.getTheme(theme);
  assert.ok(descriptor, `${theme} is not a theme @pierre/theming knows`);

  // `load` is the dynamic import of `@pierre/theme`. When it cannot be
  // resolved this rejects, and a rejected theme load is a blank diff body.
  const loaded = await descriptor.load();
  assert.equal(loaded.name, theme);
});
