import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { diffTabFor, placeTab, type Tab } from "../src/lib/tabs.ts";
import {
  anchorId,
  anchorScope,
  anchorTarget,
  groupAnchorId,
  scrollToAnchor,
} from "../src/components/diff-sections.ts";
import type { RepoReport, Worktree } from "../src/lib/api.ts";

/**
 * What a click on the worktree list carries into the Changes view.
 *
 * The reported bug had two halves, and the fix removed the thing both halves
 * were about. Clicking the dirty count opened a tab showing branch history,
 * because nothing carried the request; and clicking it again on an open tab
 * changed nothing, because opening an existing tab returned it untouched.
 *
 * The answer to the first was *not* a narrower fetch. There is one payload —
 * everything this worktree is holding — so a click carries a **reading** and
 * an **anchor**: which of the two readings to show, and which group the click
 * was about. It never carries a backend scope, because a click asking "what is
 * on disk here" answered by hiding the branch's commits is the same lie in a
 * different place.
 */

const repo = { root: "/repo", name: "repo" } as RepoReport;
const worktree = { path: "/repo/../wt", branch: "feat/x" } as Worktree;
const other = { path: "/repo/../wt2", branch: "feat/y" } as Worktree;

const diff = (tab: Tab) => {
  assert.equal(tab.kind, "diff");
  return tab as Extract<Tab, { kind: "diff" }>;
};

test("a tab carries a reading and an anchor, and no scope at all", () => {
  const tab = diff(diffTabFor(repo, worktree));

  assert.equal("scope" in tab, false, "there is one payload, so no scope");
  assert.equal("initialMode" in tab, false);
  assert.equal(typeof tab.intent, "string");
  assert.ok(tab.anchor === null || tab.anchor === "uncommitted");
});

/**
 * The dirty count promises work that exists only on disk. It lands on that
 * group's heading, in the reading that draws the group whole — and everything
 * else stays under it.
 */
test("the dirty count anchors on the on-disk group without narrowing", () => {
  const tab = diff(
    diffTabFor(repo, worktree, { intent: "everything", anchor: "uncommitted" }),
  );

  assert.equal(tab.anchor, "uncommitted");
  assert.equal(tab.intent, "everything");
});

/** The general Changes button asks the narrower question and lands nowhere in particular. */
test("the Changes button asks for the at-risk reading by default", () => {
  const tab = diff(diffTabFor(repo, worktree));

  assert.equal(tab.intent, "atRisk");
  assert.equal(tab.anchor, null);
});

test("re-opening a tab re-aims it rather than answering the first question again", () => {
  const first = diff(diffTabFor(repo, worktree));
  const second = diff(
    diffTabFor(repo, worktree, { intent: "everything", anchor: "uncommitted" }),
  );

  const tabs = placeTab(placeTab([], first), second);

  assert.equal(tabs.length, 1, "identity is the worktree, so there is one tab");
  const only = diff(tabs[0]!);
  assert.equal(only.intent, "everything");
  assert.equal(only.anchor, "uncommitted");
  assert.notEqual(
    only.request,
    first.request,
    "the view has to be able to tell that it was asked again",
  );
});

test("asking the same question twice still changes the request number", () => {
  const first = diff(diffTabFor(repo, worktree, { anchor: "uncommitted" }));
  const second = diff(diffTabFor(repo, worktree, { anchor: "uncommitted" }));

  assert.notEqual(second.request, first.request);
});

test("different worktrees keep their own tabs", () => {
  const tabs = placeTab(
    placeTab(
      [],
      diffTabFor(repo, worktree, {
        intent: "everything",
        anchor: "uncommitted",
      }),
    ),
    diffTabFor(repo, other),
  );

  assert.equal(tabs.length, 2);
  assert.deepEqual(
    tabs.map((tab) => (tab.kind === "diff" ? tab.anchor : "not a diff")),
    ["uncommitted", null],
  );
});

test("placing a tab keeps its position rather than shuffling the strip", () => {
  const a = diffTabFor(repo, worktree);
  const b = diffTabFor(repo, other);
  const tabs = placeTab(placeTab([], a), b);

  const next = placeTab(
    tabs,
    diffTabFor(repo, worktree, { intent: "everything", anchor: "uncommitted" }),
  );

  assert.deepEqual(
    next.map((tab) => tab.key),
    [a.key, b.key],
  );
});

/* ------------------------------------------------------------------ *
 * Where that click lands, with every other tab still mounted.
 * ------------------------------------------------------------------ */

/**
 * The release blocker: every open tab stays mounted, and only one is visible.
 *
 * That is deliberate — unmounting would throw away scroll position, collapse
 * state and every fetched diff. It also meant two Changes views held an
 * element called `changes-group-uncommitted` at the same time, and
 * `document.getElementById` answers with whichever mounted first. Clicking one
 * worktree's dirty count scrolled a *hidden* worktree's heading and left the
 * visible one exactly where it was.
 *
 * Two things fix it, and both are asserted here: the id carries the tab's
 * identity, so no two mounted views can claim the same one; and the lookup is
 * scoped to the view doing it, so it cannot reach into another tab even by
 * accident.
 */

/** A stand-in for one mounted view: its own subtree, and nothing else's. */
type FakeElement = { id: string; scrolled: number; scrollIntoView(): void };

const fakeElement = (id: string): FakeElement => ({
  id,
  scrolled: 0,
  scrollIntoView() {
    this.scrolled += 1;
  },
});

const fakeView = (elements: FakeElement[]) => ({
  querySelector(selector: string): FakeElement | null {
    const id = /^\[id="(.*)"\]$/.exec(selector)?.[1];
    return elements.find((element) => element.id === id) ?? null;
  },
});

const mountTab = (worktreePath: string) => {
  const scope = anchorScope(worktreePath);
  const headings = {
    uncommitted: fakeElement(groupAnchorId(scope, "uncommitted")),
    committed: fakeElement(groupAnchorId(scope, "committed")),
  };
  const file = fakeElement(anchorId(scope, "uncommitted", "src/a.ts"));
  return {
    scope,
    headings,
    file,
    view: fakeView([headings.uncommitted, headings.committed, file]),
  };
};

test("two mounted tabs never share an anchor id", () => {
  const first = mountTab(worktree.path);
  const second = mountTab(other.path);

  assert.notEqual(
    groupAnchorId(first.scope, "uncommitted"),
    groupAnchorId(second.scope, "uncommitted"),
    "one id per mounted view, or a global lookup picks the wrong tab",
  );
  assert.notEqual(
    anchorId(first.scope, "uncommitted", "src/a.ts"),
    anchorId(second.scope, "uncommitted", "src/a.ts"),
  );
});

test("the dirty count scrolls the tab that was clicked, not the one mounted first", () => {
  const first = mountTab(worktree.path);
  const second = mountTab(other.path);

  // The second worktree's dirty count, with the first still mounted and hidden.
  const found = scrollToAnchor(
    second.view,
    groupAnchorId(second.scope, "uncommitted"),
  );

  assert.equal(found, true);
  assert.equal(second.headings.uncommitted.scrolled, 1);
  assert.equal(
    first.headings.uncommitted.scrolled,
    0,
    "a hidden tab is not the one the reader is looking at",
  );
});

/**
 * The lookup cannot reach outside its own view, so a stale or mistaken id
 * scrolls nothing rather than scrolling somebody else's tab. A document-wide
 * lookup would have found this element and moved a hidden view.
 */
test("one view cannot resolve another view's anchor", () => {
  const first = mountTab(worktree.path);
  const second = mountTab(other.path);

  assert.equal(
    anchorTarget(first.view, groupAnchorId(second.scope, "uncommitted")),
    null,
  );
  assert.equal(
    scrollToAnchor(first.view, anchorId(second.scope, "uncommitted", "src/a.ts")),
    false,
  );
  assert.equal(second.headings.uncommitted.scrolled, 0);
  assert.equal(second.file.scrolled, 0);
});

/** Nothing mounted yet is not an error; it is simply nothing to scroll to. */
test("a view with no element yet scrolls nothing rather than the document", () => {
  const scope = anchorScope(worktree.path);

  assert.equal(anchorTarget(null, groupAnchorId(scope, "uncommitted")), null);
  assert.equal(scrollToAnchor(null, groupAnchorId(scope, "uncommitted")), false);
});

/* ------------------------------------------------------------------ *
 * Re-opening a mounted tab: a new question, not a new view.
 * ------------------------------------------------------------------ */

/**
 * Re-aiming a tab must not throw away what it has read. The fetch, the
 * selection and the collapse map are keyed on the worktree, so only the
 * reading and the anchor move when the same tab is opened again.
 */
test("re-aiming a mounted tab keeps its reading and collapse state", () => {
  const source = readFileSync(
    new URL("../src/components/DiffTab.tsx", import.meta.url),
    "utf8",
  );

  const reaim = source.slice(
    source.indexOf("setReading(intent);"),
    source.indexOf("}, [request, intent, anchor]);"),
  );
  assert.ok(reaim.length > 0, "the re-aim effect moved; this test has to follow");
  assert.doesNotMatch(reaim, /setCollapse|setResult|setActive/);

  // What is cleared, is cleared per worktree — never per click.
  const fetchAt = source.indexOf("setCollapse(EMPTY_COLLAPSE);");
  assert.ok(fetchAt > 0);
  assert.match(source.slice(fetchAt), /\}, \[repoRoot, path\]\);/);

  // And the anchor lands inside this view rather than the document.
  assert.doesNotMatch(source, /document\s*\.?\s*getElementById/);
  assert.match(source, /scrollToAnchor\(view\.current, groupAnchorId\(scope, "uncommitted"\)\)/);
});
