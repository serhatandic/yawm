/**
 * Run with: node --experimental-strip-types --test tests/settings-merge.test.ts
 *
 * The Settings tab holds a snapshot for as long as it stays open. These cover
 * what happens when that snapshot is written back over settings that moved:
 * the user's edit must land, and nothing they never touched may be undone.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import { rebaseConfig } from "../src/components/settings-merge.ts";
import type { Config, Workspace } from "../src/lib/api.ts";

function workspace(over: Partial<Workspace> = {}): Workspace {
  return { id: "w1", name: "Work", repos: [], scanRoots: [], ...over };
}

function config(over: Partial<Config> = {}): Config {
  return {
    workspaces: [workspace()],
    activeWorkspace: "w1",
    scanDepth: 3,
    editor: null,
    worktreePathTemplate: "../{repo}-{branch}",
    activeWithinMinutes: 30,
    diffStyle: "unified",
    hideMainWorktrees: false,
    provisioning: {
      copyEnvFiles: true,
      linkDependencies: true,
      honourWorktreeinclude: true,
    },
    ...over,
  };
}

test("a repository added elsewhere survives pressing Save", () => {
  const base = config();
  // The tab was opened, and the user changed one unrelated field.
  const edited = config({ scanDepth: 5 });
  // Meanwhile a repository was added from the list.
  const fresh = config({
    workspaces: [workspace({ repos: ["/code/added"] })],
  });

  const merged = rebaseConfig(base, edited, fresh);

  assert.deepEqual(
    merged.workspaces[0].repos,
    ["/code/added"],
    "the addition is the whole bug: a stale Save used to erase it",
  );
  assert.equal(merged.scanDepth, 5, "and the user's edit still lands");
});

test("an edit the user made wins over the value it was made against", () => {
  const base = config({ editor: "zed" });
  const edited = config({ editor: "cursor" });
  const fresh = config({ editor: "zed" });

  assert.equal(rebaseConfig(base, edited, fresh).editor, "cursor");
});

test("a field the user never touched keeps the newer value", () => {
  const base = config({ hideMainWorktrees: false });
  const edited = config({ hideMainWorktrees: false, scanDepth: 4 });
  const fresh = config({ hideMainWorktrees: true });

  const merged = rebaseConfig(base, edited, fresh);
  assert.equal(
    merged.hideMainWorktrees,
    true,
    "a snapshot holds no opinion about a field nobody edited",
  );
  assert.equal(merged.scanDepth, 4);
});

test("workspaces are matched by id, not by position", () => {
  const one = workspace({ id: "w1", name: "Work" });
  const two = workspace({ id: "w2", name: "Side" });

  const base = config({ workspaces: [one, two] });
  const edited = config({
    workspaces: [one, { ...two, name: "Side projects" }],
  });
  // A group was created elsewhere and sorted in ahead of the others.
  const fresh = config({
    workspaces: [workspace({ id: "w0", name: "Archive" }), one, two],
  });

  const merged = rebaseConfig(base, edited, fresh);
  assert.deepEqual(
    merged.workspaces.map((w) => [w.id, w.name]),
    [
      ["w0", "Archive"],
      ["w1", "Work"],
      ["w2", "Side projects"],
    ],
    "merging by index would have renamed the wrong group",
  );
});

test("a workspace deleted elsewhere is not resurrected by an open tab", () => {
  const one = workspace({ id: "w1" });
  const two = workspace({ id: "w2", name: "Side" });

  const base = config({ workspaces: [one, two] });
  const edited = config({ workspaces: [one, { ...two, name: "Renamed" }] });
  const fresh = config({ workspaces: [one] });

  const merged = rebaseConfig(base, edited, fresh);
  assert.deepEqual(merged.workspaces.map((w) => w.id), ["w1"]);
});

test("settings this build does not know about are carried through", () => {
  const base = config();
  const edited = config({ scanDepth: 9 });
  const fresh = {
    ...config(),
    futureRepositories: ["/code/from-the-future"],
  } as Config;

  const merged = rebaseConfig(base, edited, fresh) as Config & {
    futureRepositories?: string[];
  };
  assert.deepEqual(merged.futureRepositories, ["/code/from-the-future"]);
  assert.equal(merged.scanDepth, 9);
});
