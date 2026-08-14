import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  AT_RISK_READING_LABEL,
  EVERYTHING_READING_LABEL,
} from "../src/lib/api.ts";
import {
  COMMITTED_HEADING,
  ON_DISK_HEADING,
} from "../src/components/diff-sections.ts";

const source = readFileSync(
  new URL("../src/components/DiffTab.tsx", import.meta.url),
  "utf8",
);

const occurrences = (needle: string) => source.split(needle).length - 1;

/**
 * The reported bug: two pseudo-tabs offering scopes that were not alternatives.
 *
 * "Uncommitted" and "Branch history" were drawn as a switch, so choosing one
 * hid the other — but a worktree holds both at once, and the question the
 * reader is asking ("what does deleting this lose?") is answered by both halves
 * together. There is one combined view now, and the only switch left is a
 * reading of it.
 */
test("there is no scope switch, and no scope in the fetch", () => {
  assert.doesNotMatch(source, /aria-label="Change scope"/);
  assert.doesNotMatch(source, /SCOPE_LABEL/);
  assert.doesNotMatch(source, /setScope|scope === "uncommitted"/);
  // One fetch, for one worktree, holding both halves.
  assert.equal(occurrences('.diffWorktree(repoRoot, path, "history")'), 1);
});

/**
 * Switching readings must not refetch: it is a way of looking at a payload
 * already on screen, so it cannot flash a skeleton or lose what has been read.
 */
test("the reading is not part of the fetch key", () => {
  const effectAt = source.indexOf(".diffWorktree(repoRoot, path");
  assert.ok(effectAt > 0, "the fetch moved; this test has to follow it");
  const effect = source.slice(effectAt, effectAt + 900);

  assert.match(effect, /\}, \[repoRoot, path\]\);/);
  assert.doesNotMatch(effect, /\}, \[repoRoot, path, (reading|intent|request)/);
});

/** One control on the header row, and it is a reading rather than a scope. */
test("there is exactly one reading switch, offering both readings", () => {
  assert.equal(occurrences('aria-label="Reading"'), 1);
  assert.match(source, /\{AT_RISK_READING_LABEL\}/);
  assert.match(source, /\{EVERYTHING_READING_LABEL\}/);
  assert.notEqual(AT_RISK_READING_LABEL, EVERYTHING_READING_LABEL);
  // A segment named after a group would make a reading and a scope
  // indistinguishable again.
  assert.notEqual(AT_RISK_READING_LABEL, ON_DISK_HEADING);
  assert.notEqual(EVERYTHING_READING_LABEL, COMMITTED_HEADING);
});

/** A control that renders the same thing twice is a control that does nothing. */
test("the reading switch is drawn only when it would change what is shown", () => {
  assert.match(source, /const filterAvailable =/);
  assert.match(source, /readingNarrows\(everythingSections, atRiskSections\)/);
  assert.match(source, /\{filterAvailable \? \(/);
});

/** Both groups, in one scroll, each named exactly once and the same everywhere. */
test("the two groups are named from the shared constants", () => {
  assert.match(source, /label: ON_DISK_HEADING/);
  assert.match(source, /label: COMMITTED_HEADING/);
  assert.equal(occurrences('"Uncommitted Changes"'), 0);
  assert.equal(occurrences('"Branch History"'), 0);
});

/**
 * A heading over nothing states an absence in the longest possible way, and a
 * branch with no commits of its own has no committed group to draw. The fact is
 * said once, in the summary.
 */
test("an empty group is not rendered, and the absence is stated instead", () => {
  assert.match(source, /\.filter\(\(group\) => group\.sections\.length > 0\)/);
  assert.match(
    source,
    /summary\.commits === 0 \? noBranchCommitsClause\(summary\.base\) : null/,
  );
});

/**
 * The click that was about one group lands on that group, and hides nothing —
 * in the tab that was clicked. Every open tab stays mounted, so the id carries
 * the tab's identity and the lookup is scoped to this view: a document-wide
 * `getElementById` handed the scroll to whichever worktree mounted first.
 */
test("an anchored open scrolls to the group heading, in this tab only", () => {
  assert.match(source, /groupAnchorId\(scope, "uncommitted"\)/);
  assert.match(source, /id=\{groupAnchorId\(scope, group\.id\)\}/);
  assert.match(source, /pendingAnchor\.current !== "uncommitted"/);
  assert.match(source, /const scope = anchorScope\(path\)/);
  // Nothing in this view reaches for the document to find an anchor.
  assert.doesNotMatch(source, /document\s*\.?\s*getElementById/);
  assert.match(source, /scrollToAnchor\(view\.current,/);
});

/**
 * A re-open carries a new question. It changes the reading and the anchor, and
 * nothing else: what has been read, and which files the reader folded, were not
 * invalidated by the click.
 */
test("re-aiming an open tab does not reset what has been read", () => {
  const at = source.indexOf("setReading(intent);");
  assert.ok(at > 0);
  const effect = source.slice(at, source.indexOf("}, [request, intent, anchor]"));

  assert.match(effect, /pendingAnchor\.current = anchor;/);
  assert.doesNotMatch(effect, /setCollapse|setResult|setFocus/);
});

/**
 * The filter narrows what is drawn and nothing else, so it may not cost the
 * reader their place: returning to the top and dropping the selection made
 * checking one file against the narrower reading a search, twice over.
 */
test("switching readings keeps the reader's place", () => {
  const at = source.indexOf("const changeReading = (next: ReadingIntent)");
  assert.ok(at > 0);
  const handler = source.slice(at, source.indexOf("};", at));

  assert.match(handler, /setReading\(next\)/);
  assert.doesNotMatch(handler, /setActive|scrollTo/);
  // Nothing holds the scroller any more, because nothing scrolls it back.
  assert.doesNotMatch(source, /scrollerRef/);
  // The file being read is the same file in either reading, so the view
  // follows it when the narrower reading still contains it.
  const follow = source.indexOf("if (active === null) return;");
  assert.ok(follow > 0);
  assert.match(
    source.slice(follow, source.indexOf("}, [reading]", follow)),
    /anchorId\(scope, active\.origin, active\.path\)/,
  );
});

/**
 * The summary describes the worktree, not the reading. A denominator that
 * shrank when a filter was pressed would make the two readings disagree about
 * the same worktree.
 */
test("the balance is computed from the complete reading in both readings", () => {
  const at = source.indexOf("const balances: GroupBalance[] = [");
  assert.ok(at > 0);
  const block = source.slice(at, source.indexOf("const allOmitted"));

  assert.doesNotMatch(block, /atRiskReading/);
  assert.match(block, /balanceOf\(\s*onDiskSections,\s*onDiskOmitted/);
  assert.match(block, /balanceOf\(\s*everythingSections,\s*committedOmitted/);
  // The total is a union of path identities, so each group states which paths
  // it is accounting for and not merely how many.
  assert.match(block, /coverageOf\(patches\.uncommitted\)/);
  assert.match(block, /coverageOf\(patches\.committed\)/);
});

/**
 * Once nothing without a patch is drawn, a sentence reconciling raw paths
 * against rendered rows describes a mismatch that is not on screen. The
 * omission clause replaced it.
 */
test("there is no card section left to render a non-diff", () => {
  assert.doesNotMatch(source, /DiffCardSection/);
  assert.doesNotMatch(source, /data-diff-card/);
  assert.doesNotMatch(source, /ReconciliationNotice|paths grouped into/);
  assert.match(source, /textEntries\(/, "the filter is applied where groups build");
  assert.match(source, /omittedFrom\(/, "and what it removed is counted");
});

/** Zero diffs means zero controls for opening them. */
test("the expand controls are hidden when there is nothing to open", () => {
  assert.match(source, /anythingCollapsible \? \(/);
  assert.match(source, /const treeWorthShowing = sectionCount > 0/);
});

/**
 * The value a row renders from and the value its caret toggles are one entry in
 * one map, reconciled during render — which is what makes the first click land.
 */
test("collapse state is reconciled in render, not in an effect", () => {
  assert.match(
    source,
    /const reconciled = reconcileCollapse\(collapse, allSections\)/,
  );
  assert.match(source, /if \(reconciled !== collapse\) setCollapse\(reconciled\)/);
  assert.doesNotMatch(source, /useEffect\(\(\) => \{\s*setCollapse/);
});

/**
 * The count of paths without a diff used to be the end of the story: a number,
 * a tooltip of kinds, and no way to learn which paths it stood for. It is now
 * the trigger for the disclosure, from both places the count appears.
 */
test("the not-diffable count opens a disclosure of the exact paths", () => {
  // The list itself stays text-only: this is a dialog, not sixty-three rows.
  assert.match(source, /<NotDiffableDialog/);
  assert.match(source, /notDiffableDisclosure\(patches\)/);

  // The summary's own segment is what is pressed, marked by role rather than
  // matched by its wording.
  assert.match(source, /segment\.role === "notDiffable" && onDisclose/);
  assert.match(source, /aria-label=\{discloseLabel\}/);
  assert.match(source, /onDisclose=\{disclosable \? disclose : null\}/);

  // And the empty state, where there is no summary line to hang it on.
  const emptyAt = source.indexOf("title={NO_LINE_DIFFS_TITLE}");
  assert.ok(emptyAt > 0, "the non-text empty state moved; this test must follow");
  const empty = source.slice(emptyAt, emptyAt + 800);
  assert.match(empty, /Show these paths/);
  assert.match(empty, /onClick=\{disclose\}/);
});

/**
 * The disclosure is a real dialog: Radix owns the focus trap, Escape, and the
 * return of focus — and the region it scrolls is bounded.
 */
test("the disclosure is a bounded, titled, dismissible dialog", () => {
  const at = source.indexOf("function NotDiffableDialog");
  assert.ok(at > 0);
  const dialog = source.slice(at, source.length);

  assert.match(dialog, /<DialogTitle>\{NOT_DIFFABLE_TITLE\}<\/DialogTitle>/);
  assert.match(
    dialog,
    /<DialogDescription>\{NOT_DIFFABLE_DESCRIPTION\}<\/DialogDescription>/,
  );
  assert.match(dialog, /max-h-\[50vh\][^"]*overflow-y-auto/);
  assert.match(dialog, /<DialogFooter showCloseButton \/>/);
  // The full path is available even where the column truncates it.
  assert.match(dialog, /title=\{row\.path\}/);
  // The headings are the view's own, not a third naming of the same groups.
  assert.match(dialog, /\{group\.heading\}/);
});

/** What the cap and the limits left out is stated, never implied. */
test("the disclosure states its cap and its unavailable names", () => {
  assert.match(source, /disclosureCapNote\(disclosure\)/);
  assert.match(source, /disclosureResidualNote\(disclosure\)/);
  assert.match(source, /\{group\.hidden\.toLocaleString\("en-US"\)\} more in this group/);
});

/**
 * With no `DialogTrigger`, Radix prevents its own focus restore and focuses a
 * trigger that was never registered — so dismissing the dialog dropped focus
 * on the body and a keyboard reader restarted at the top of the window.
 */
test("the disclosure hands focus back to whichever control opened it", () => {
  assert.match(source, /const disclosedFrom = useRef<HTMLElement \| null>\(null\)/);
  assert.match(source, /disclosedFrom\.current = event\.currentTarget/);
  assert.match(source, /returnFocusTo=\{disclosedFrom\}/);

  const at = source.indexOf("onCloseAutoFocus");
  assert.ok(at > 0, "the dialog stopped restoring focus");
  const restore = source.slice(at, at + 900);
  // Never left to Radix, which would focus a trigger that was never registered
  // and drop focus on the body.
  assert.match(restore, /event\.preventDefault\(\)/);
  assert.match(restore, /opener\?\.focus\(\)/);
  // An opener hidden behind another tab accepts the call without taking focus,
  // so the result is checked and a fallback inside this view is used.
  assert.match(restore, /document\.activeElement === opener/);
  assert.match(restore, /fallbackFocusTo\.current\?\.focus\(\)/);
  assert.match(source, /fallbackFocusTo=\{view\}/);
  assert.match(source, /<div ref=\{view\} tabIndex=\{-1\}/);
});

/**
 * Nothing inside the list can take focus, so a keyboard reader with no way to
 * reach the region cannot page past the first few paths.
 */
test("the disclosure's scroll region can be reached from the keyboard", () => {
  const at = source.indexOf("max-h-[50vh]");
  assert.ok(at > 0);
  const region = source.slice(at - 400, at + 200);
  assert.match(region, /tabIndex=\{0\}/);
  // Its own name rather than the dialog's, which a screen reader has already
  // announced as the title.
  assert.match(region, /aria-label=\{NOT_DIFFABLE_LIST_LABEL\}/);
  assert.match(region, /FOCUS_RING/);
});
