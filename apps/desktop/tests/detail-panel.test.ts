import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  CLOSE_DETAILS_LABEL,
  COMFORTABLE_PANEL_WIDTH,
  actionLayout,
  riskRow,
} from "../src/components/detail-panel.ts";
import type { Risk } from "../src/components/risks.tsx";

const source = readFileSync(
  new URL("../src/components/DetailPanel.tsx", import.meta.url),
  "utf8",
);

/**
 * The close control names what it closes.
 *
 * "Close" alone — which is what it said while it sat in the verdict banner —
 * read as "dismiss this warning" on a coloured judgement. It closes the
 * details, and says so, in the label and in the tooltip.
 */
test("the close control names what it closes", () => {
  assert.equal(CLOSE_DETAILS_LABEL, "Close details");
  assert.match(source, /aria-label=\{CLOSE_DETAILS_LABEL\}/);
  assert.match(source, /title=\{CLOSE_DETAILS_LABEL\}/);
});

/**
 * It sits in the panel's first content section, beside the name of the thing it
 * closes — not in the verdict block, where a neutral glyph took the colour of a
 * judgement, and not on the panel's boundary, where the panel's own
 * `overflow-hidden` clipped it and it stole the top of the resize edge.
 */
test("the verdict banner holds nothing that closes the panel", () => {
  const bannerAt = source.indexOf("verdictZoneClass(worktree.verdict)");
  assert.ok(bannerAt > 0);
  const banner = source.slice(bannerAt, source.indexOf("landingCheck !== "));

  assert.doesNotMatch(banner, /onClose/);
  assert.doesNotMatch(banner, /CLOSE_DETAILS_LABEL/);
});

test("the close control costs no row and no rail of its own", () => {
  // The 28px strip that used to hold one 20px button and nothing else.
  assert.doesNotMatch(source, /flex h-7 shrink-0 items-center justify-end/);

  const closeAt = source.indexOf("aria-label={CLOSE_DETAILS_LABEL}");
  const mount = source.slice(closeAt - 400, closeAt + 400);
  assert.match(mount, /shrink-0/, "it never grows a column of its own");
  assert.doesNotMatch(mount, /absolute/, "it is in the flow, not hung off it");
});

/**
 * One control on the boundary. When the button hung there too, the strip lay
 * under it and a drag started near the top of the edge silently did nothing.
 */
test("the resize edge is the only thing on the panel's boundary", () => {
  assert.match(source, /cursor-col-resize/);
  assert.match(source, /absolute top-0 -left-1 z-10 h-full w-2 cursor-col-resize/);
  const closeAt = source.indexOf("aria-label={CLOSE_DETAILS_LABEL}");
  const mount = source.slice(closeAt - 600, closeAt + 400);
  assert.doesNotMatch(mount, /cursor-col-resize/);
});

/**
 * The action strip is laid out from the panel's own measured width, not from a
 * viewport query that knows nothing about how wide this panel was dragged.
 */
test("a panel dragged narrow stacks its actions rather than colliding them", () => {
  assert.equal(actionLayout(COMFORTABLE_PANEL_WIDTH), "row");
  assert.equal(actionLayout(COMFORTABLE_PANEL_WIDTH - 1), "stacked");
});

/**
 * One line per risk. The facts are unchanged; the essay under each is gone.
 */
test("a risk row carries its label and count, and no detail or fragments", () => {
  const risk: Risk = {
    kind: "uncommitted",
    count: 148,
    label: "148 uncommitted files",
    detail: "3 staged · 12 modified · 133 untracked · Not compared line by line",
    fragments: ["+  app: 920,", "-  app: 918,"],
    tone: "review",
  };

  const row = riskRow(risk);

  assert.deepEqual(row, {
    kind: "uncommitted",
    label: "148 uncommitted files",
    count: 148,
    opensUncommitted: true,
  });
  assert.equal("detail" in row, false);
  assert.equal("fragments" in row, false);
});

/**
 * Only the risk that is entirely about this directory is a link, and it opens
 * exactly the work it counts — anchored on the on-disk group, in the reading
 * that draws it whole.
 */
test("only the uncommitted risk keeps its click through to Changes", () => {
  assert.equal(
    riskRow({
      kind: "unpushed",
      count: 2,
      label: "2 commits not pushed",
      tone: "review",
    }).opensUncommitted,
    false,
  );

  const at = source.indexOf('risk.kind === "uncommitted" ? (');
  assert.ok(at > 0, "the uncommitted risk is the one that links");
  assert.match(source.slice(at, at + 400), /onClick=\{onShowUncommitted\}/);
});

test("a risk with no count says so rather than inventing a zero", () => {
  assert.equal(
    riskRow({
      kind: "landing",
      label: "Could not verify whether this work landed",
      tone: "review",
    }).count,
    null,
  );
});
