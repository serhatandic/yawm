/**
 * The structural promises this app's chrome makes.
 *
 * These are assertions about markup rather than about arithmetic, so they are
 * deliberately narrow: each one names a specific way the interface previously
 * lied, and would fail again if that shape came back. Nothing here asserts a
 * colour or a spacing value — those are meant to move.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const normalizeSource = (source: string) => source.replace(/\r\n?/g, "\n");
const read = (path: string) =>
  normalizeSource(
    readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8"),
  );

const app = read("App.tsx");
const titleBar = read("components/TitleBar.tsx");
const row = read("components/WorktreeRow.tsx");
const list = read("components/WorktreeListTab.tsx");
const command = read("components/ui/command.tsx");
const deleteDialog = read("components/DeleteDialog.tsx");
const createDialog = read("components/CreateDialog.tsx");
const settings = read("components/SettingsTab.tsx");

test("source assertions normalize Windows line endings", () => {
  assert.equal(normalizeSource("first\r\nsecond\rthird"), "first\nsecond\nthird");
});

/**
 * Ticked and inspected are two different claims about a row — "the Delete
 * button is about this one" against "I am reading this one" — and both were
 * drawn as the same grey fill, so a bulk selection vanished the moment the
 * inspector opened on one of its rows.
 */
test("a ticked row is marked apart from the inspected one", () => {
  const at = row.indexOf("selected ? \"bg-muted\"");
  assert.ok(at > 0, "the row's state classes moved; this test has to follow");
  const classes = row.slice(at, at + 400);

  assert.match(classes, /checked &&/, "being ticked has a treatment of its own");
  assert.match(classes, /inset/, "and it survives the row being inspected");
  assert.match(classes, /checked && selected/, "including both at once");
});

/**
 * `role="button"` takes presentational children, so the checkbox and the
 * uncommitted count inside the row were controls no assistive technology could
 * reach. The row is a grid row now, which is what it looks like, and its cells
 * are allowed to hold controls.
 */
test("the row is a grid row rather than a button full of controls", () => {
  assert.doesNotMatch(row, /role="button"/);
  assert.match(row, /role="row"/);
  assert.match(row, /role="gridcell"/);
  assert.match(row, /role="columnheader"/);
  assert.match(list, /role="grid"/);
  assert.match(list, /role="rowgroup"/);

  // Enter and Space still inspect the row they are pressed on.
  assert.match(row, /e\.key === "Enter" \|\| e\.key === " "/);
  // Nested controls keep those same keys for their own activation.
  assert.match(row, /e\.target !== e\.currentTarget/);
  // And the checkbox still refuses to hand its click to the row.
  assert.match(row, /onClick=\{\(e\) => e\.stopPropagation\(\)\}/);
});

/**
 * The panels were already `role="tabpanel"` while nothing claimed to control
 * them: the tabs pointed at nothing and Home was a button with `aria-current`
 * standing among them.
 */
test("the tabs and the panels name each other", () => {
  assert.match(titleBar, /role="tablist"/);
  assert.match(titleBar, /aria-controls=\{panelId\(tab\.key\)\}/);
  assert.match(titleBar, /aria-controls=\{HOME_PANEL_ID\}/);
  assert.match(app, /aria-labelledby=\{labelledBy\}/);
  assert.match(app, /id=\{panelId\(tab\.key\)\}/);
  assert.match(app, /labelledBy=\{tabId\(tab\.key\)\}/);
});

test("the tablist uses one tab stop and supports horizontal navigation", () => {
  assert.match(titleBar, /tabIndex=\{activeKey === null \? 0 : -1\}/);
  assert.match(titleBar, /tabIndex=\{active \? 0 : -1\}/);
  assert.match(titleBar, /event\.key !== "ArrowLeft"/);
  assert.match(titleBar, /event\.key !== "ArrowRight"/);
  assert.match(titleBar, /event\.key !== "Home"/);
  assert.match(titleBar, /event\.key !== "End"/);
  assert.match(titleBar, /tabs\[next\]\?\.focus\(\)/);
  assert.match(titleBar, /tabs\[next\]\?\.click\(\)/);
});

test("responsive icon-only controls keep accessible names", () => {
  assert.match(titleBar, /aria-label="Jump to any worktree"/);
  assert.match(list, /hideMain \? "Show main worktrees" : "Hide main worktrees"/);
});

/**
 * `role="tab"` is children-presentational too, so a close button nested inside
 * a tab was a control with no name of its own. Middle click still closes.
 */
test("a tab's close control is a sibling of the tab, not inside it", () => {
  const at = titleBar.indexOf('role="tab"\n          id={tabId(tab.key)}');
  assert.ok(at > 0, "the tab moved; this test has to follow it");
  const tab = titleBar.slice(at, titleBar.indexOf("</button>", at));
  assert.doesNotMatch(tab, /aria-label=\{`Close/);

  assert.match(titleBar, /aria-label=\{`Close \$\{title\}`\}/);
  assert.match(titleBar, /onAuxClick/);
});

/**
 * Radix reads the title out of the content it labels. Hoisted above it — as
 * shadcn ships it — the palette announced itself as an unnamed dialog, and its
 * close button landed on top of the search field.
 */
test("the palette is named from inside itself, with nothing over the input", () => {
  const at = command.indexOf("function CommandDialog");
  const dialog = command.slice(at, command.indexOf("function CommandInput"));

  const contentAt = dialog.indexOf("<DialogContent");
  const headerAt = dialog.indexOf("<DialogHeader");
  assert.ok(contentAt > 0 && headerAt > contentAt, "the header is inside the content");
  assert.match(dialog, /showCloseButton = false/);
});

/**
 * A twelve-worktree plan made a dialog taller than the window it opened in,
 * and what fell off the bottom was the acknowledgement and the Delete button —
 * on the one screen where every gate has to stay reachable.
 */
test("the delete dialog scrolls its plan, not its gates", () => {
  assert.match(deleteDialog, /max-h-\[calc\(100dvh-3rem\)\]/);
  assert.doesNotMatch(deleteDialog, /className="max-h-80/);
  assert.match(deleteDialog, /min-h-0 flex-1 overflow-y-auto/);
  assert.match(deleteDialog, /DialogFooter className="shrink-0/);

  // Every gate and every string it gates on is still here.
  assert.match(deleteDialog, /I understand, delete it anyway/);
  assert.match(deleteDialog, /I understand, unlock and delete it/);
  assert.match(deleteDialog, /disabled=\{busy \|\| plans === null \|\| blocked\}/);
});

/**
 * Enter creates a worktree, through the same gate the button is disabled by.
 * Deletion deliberately gets no such key: an accidental Return there is
 * irreversible.
 */
test("Create submits on Enter and Delete does not", () => {
  assert.match(createDialog, /<form/);
  assert.match(createDialog, /onSubmit=\{\(event\) => \{/);
  assert.match(createDialog, /type="submit" disabled=\{busy \|\| blocked\}/);
  assert.match(createDialog, /if \(blocked \|\| plan === null\) return;/);

  assert.doesNotMatch(deleteDialog, /<form/);
  assert.doesNotMatch(deleteDialog, /type="submit"/);
});

/**
 * The notice's tone was carried by the hue of a bullet and nothing else, and
 * its dismissal was one character of text to aim at.
 */
test("a notice states its tone with a shape and closes with a target", () => {
  assert.doesNotMatch(app, /●/);
  assert.match(app, /<AlertTriangle/);
  assert.match(app, /<Info/);
  assert.match(app, /aria-label="Dismiss"/);
  // Dismissal is still by id, and the retry still does not dismiss.
  assert.match(app, /onClick=\{\(\) => onDismiss\(notice\.id\)\}/);
  assert.match(app, /onClick=\{\(\) => onAction\(notice\.id\)\}/);
});

/**
 * "Nothing matches" is a dead end unless it says how to get out of it — and
 * getting out means the query and the verdict filter, never the standing
 * preference about main worktrees that the sidebar's counts also read.
 */
test("clearing the filters leaves the main-worktree preference alone", () => {
  const at = list.indexOf('title="Nothing matches"');
  assert.ok(at > 0);
  const empty = list.slice(at, at + 1400);

  assert.match(empty, /Clear filters/);
  assert.match(empty, /setFilter\("all"\)/);
  assert.match(empty, /setQuery\(""\)/);
  assert.doesNotMatch(empty, /onHideMainChange/);
});

/** The footer is outside the scroller, which is what keeps Save reachable. */
test("settings scroll natively and keep their footer", () => {
  assert.doesNotMatch(settings, /<ScrollArea|from "@\/components\/ui\/scroll-area"/);
  assert.match(settings, /min-h-0 flex-1 overflow-x-hidden overflow-y-auto/);
  const footerAt = settings.indexOf("shrink-0 items-center gap-2 border-t");
  assert.ok(footerAt > 0);
  assert.match(settings.slice(footerAt, footerAt + 400), /onClick=\{save\}/);
});

/**
 * The checkbox column's header used to be an aria-hidden blank, so ticking
 * twenty rows meant twenty clicks. It now heads its own column, and its scope
 * is the filtered list — never rows the current filter is hiding.
 */
test("the header's bulk tick acts on the visible rows only", () => {
  assert.doesNotMatch(row, /<span role="columnheader" aria-hidden \/>/);
  assert.match(row, /aria-label=\{SELECT_ALL_LABEL\}/);
  assert.match(row, /checked=\{selectAllChecked\(selectAll\.state\)\}/);
  assert.match(row, /disabled=\{selectAll\.disabled\}/);

  // The set it is computed from and toggles is `visible`, after every filter.
  assert.match(list, /const visibleRows = useMemo\(\s*\(\) => visible\.map/);
  assert.match(list, /state: selectAllState\(visibleRows, checked\)/);
  assert.match(list, /disabled: selectAllDisabled\(visibleRows\)/);
  assert.match(list, /toggleVisibleSelection\(prev, visibleRows\)/);
  // Never the unfiltered lists.
  assert.doesNotMatch(list, /selectAllState\((located|inScope)/);
  assert.doesNotMatch(list, /toggleVisibleSelection\(prev, (located|inScope)/);
});

/** Placeholder rows are not worktrees, so there is nothing there to select. */
test("the loading skeleton has no bulk tick", () => {
  const at = list.indexOf("function WorktreeListSkeleton");
  assert.ok(at > 0);
  const skeleton = list.slice(at, at + 1200);
  assert.doesNotMatch(skeleton, /selectAll=/);
});

/** A partial selection is not a selection, and is not drawn as one. */
test("the checkbox draws its mixed state apart from its checked state", () => {
  const checkbox = read("components/ui/checkbox.tsx");
  assert.match(checkbox, /props\.checked === "indeterminate"/);
  assert.match(checkbox, /<MinusIcon/);
  assert.match(checkbox, /data-\[state=indeterminate\]:bg-primary/);
});

/** Deleting is still guarded by the confirmation the bulk bar has always had. */
test("bulk selection still deletes through the confirming dialog", () => {
  assert.match(list, /onClick=\{\(\) => onDelete\(checkedItems\)\}/);
  assert.match(list, /variant="destructive"/);
  // And a main worktree can never be in that set.
  assert.match(list, /selectable=\{!worktree\.isMain\}/);
});
