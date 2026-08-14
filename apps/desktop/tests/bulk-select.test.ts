import { test } from "node:test";
import assert from "node:assert/strict";

import {
  SELECT_ALL_LABEL,
  selectAllChecked,
  selectAllDisabled,
  selectAllState,
  selectablePaths,
  toggleVisibleSelection,
  type SelectableRow,
} from "../src/components/bulk-select.ts";

const row = (path: string, isMain = false): SelectableRow => ({ path, isMain });

/** A main worktree cannot be removed, so a bulk tick may never claim one. */
test("main worktrees are never part of a bulk selection", () => {
  const visible = [row("/wt/a"), row("/repo", true), row("/wt/b")];

  assert.deepEqual(selectablePaths(visible), ["/wt/a", "/wt/b"]);

  const picked = toggleVisibleSelection(new Set<string>(), visible);
  assert.deepEqual([...picked].sort(), ["/wt/a", "/wt/b"]);
  assert.ok(!picked.has("/repo"));

  // With every removable row ticked the control still reads "all", even though
  // a main worktree is on screen unticked.
  assert.equal(selectAllState(visible, picked), "checked");
});

/** Nothing to select means a disabled control rather than a dead one. */
test("a list of only main worktrees disables the control", () => {
  const visible = [row("/repo", true), row("/other", true)];
  assert.equal(selectAllDisabled(visible), true);
  assert.equal(selectAllState(visible, new Set()), "unchecked");
  // Pressing it anyway changes nothing.
  assert.deepEqual([...toggleVisibleSelection(new Set(["/wt/x"]), visible)], [
    "/wt/x",
  ]);
});

test("an empty list disables the control", () => {
  assert.equal(selectAllDisabled([]), true);
  assert.equal(selectAllState([], new Set()), "unchecked");
});

/** Three states, because two cannot describe a partial selection. */
test("the control reports mixed when only some visible rows are ticked", () => {
  const visible = [row("/wt/a"), row("/wt/b"), row("/wt/c")];

  assert.equal(selectAllState(visible, new Set()), "unchecked");
  assert.equal(selectAllState(visible, new Set(["/wt/a"])), "indeterminate");
  assert.equal(
    selectAllState(visible, new Set(["/wt/a", "/wt/b"])),
    "indeterminate",
  );
  assert.equal(
    selectAllState(visible, new Set(["/wt/a", "/wt/b", "/wt/c"])),
    "checked",
  );

  assert.equal(selectAllChecked("checked"), true);
  assert.equal(selectAllChecked("indeterminate"), "indeterminate");
  assert.equal(selectAllChecked("unchecked"), false);
});

/**
 * The control speaks for what is on screen. A tick on a row the filter is
 * hiding must not make it claim "all", which the reader could not check.
 */
test("selections hidden by the filter do not make the control read as all", () => {
  const visible = [row("/wt/a"), row("/wt/b")];
  const checked = new Set(["/wt/a", "/wt/hidden"]);

  assert.equal(selectAllState(visible, checked), "indeterminate");
  assert.equal(
    selectAllState(visible, new Set(["/wt/a", "/wt/b", "/wt/hidden"])),
    "checked",
  );
});

/** Selecting acts on the filtered list, and only on it. */
test("selecting all adds every visible removable row and nothing else", () => {
  const visible = [row("/wt/a"), row("/wt/b"), row("/repo", true)];
  const next = toggleVisibleSelection(new Set(["/wt/hidden"]), visible);

  assert.deepEqual([...next].sort(), ["/wt/a", "/wt/b", "/wt/hidden"]);
});

/** Clearing acts on the filtered list too, so a filter cannot silently drop ticks. */
test("clearing removes only the visible rows and keeps hidden ones", () => {
  const visible = [row("/wt/a"), row("/wt/b")];
  const checked = new Set(["/wt/a", "/wt/b", "/wt/hidden"]);

  const next = toggleVisibleSelection(checked, visible);
  assert.deepEqual([...next], ["/wt/hidden"]);
});

/** Pressing it twice is a no-op on everything outside the current filter. */
test("a round trip restores the selection the filter was hiding", () => {
  const visible = [row("/wt/a"), row("/wt/b")];
  const start = new Set(["/wt/hidden"]);

  const picked = toggleVisibleSelection(start, visible);
  const cleared = toggleVisibleSelection(picked, visible);

  assert.deepEqual([...cleared], ["/wt/hidden"]);
  // And the original set was never mutated.
  assert.deepEqual([...start], ["/wt/hidden"]);
});

/** A partial selection completes rather than clears: the first press adds. */
test("pressing a mixed control selects the rest rather than clearing", () => {
  const visible = [row("/wt/a"), row("/wt/b"), row("/wt/c")];
  const next = toggleVisibleSelection(new Set(["/wt/b"]), visible);

  assert.deepEqual([...next].sort(), ["/wt/a", "/wt/b", "/wt/c"]);
});

/** The label has to state the scope, because the scope is not "everything". */
test("the label names the set the control acts on", () => {
  assert.match(SELECT_ALL_LABEL, /visible/i);
  assert.match(SELECT_ALL_LABEL, /worktrees/i);
});
