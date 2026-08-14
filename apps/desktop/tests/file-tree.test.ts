import { test } from "node:test";
import assert from "node:assert/strict";

import {
  buildTree,
  countFiles,
  normaliseTreePath,
  type FileEntry,
} from "../src/components/file-tree.ts";

const text = (path: string): FileEntry => ({
  path,
  stat: {
    kind: "counts",
    insertions: 1,
    deletions: 0,
    tone: "change",
    title: "lines",
  },
});

/**
 * Git spells "this whole directory is one entry" with a trailing slash.
 *
 * Split on `/` like any other path, `remote.git/` produced a final empty
 * segment: a nameless leaf under the directory, with no stat and no file
 * behind it, which read as a file the tree could not name.
 */
test("a directory path does not grow an unnamed empty leaf", () => {
  const tree = buildTree([text("remote.git/")]);

  assert.equal(tree.length, 1);
  assert.equal(tree[0]!.name, "remote.git");
  assert.equal(tree[0]!.path, "remote.git");
  assert.deepEqual(tree[0]!.children, []);
  assert.ok(tree[0]!.file, "the row is the entry itself, and is clickable");
});

test("a nested directory path keeps its shape without a trailing blank", () => {
  const tree = buildTree([text("packages/vendor/")]);

  assert.equal(tree.length, 1);
  assert.equal(tree[0]!.name, "packages");
  const leaf = tree[0]!.children;
  assert.equal(leaf.length, 1, "one child, not a child and an empty sibling");
  assert.equal(leaf[0]!.name, "vendor");
  assert.deepEqual(leaf[0]!.children, []);
  assert.ok(leaf[0]!.file);
  assert.equal(countFiles(tree[0]!), 1);
});

test("ordinary paths are unchanged", () => {
  const tree = buildTree([text("src/a.ts"), text("src/b.ts")]);

  assert.equal(tree.length, 1);
  assert.equal(tree[0]!.name, "src");
  assert.deepEqual(
    tree[0]!.children.map((child) => child.name),
    ["a.ts", "b.ts"],
  );
  assert.equal(countFiles(tree[0]!), 2);
});

test("a single-child chain still folds into one row", () => {
  const tree = buildTree([text("src/components/ui/button.tsx")]);

  assert.equal(tree.length, 1);
  assert.equal(tree[0]!.name, "src/components/ui");
  assert.equal(tree[0]!.children.length, 1);
  assert.equal(tree[0]!.children[0]!.name, "button.tsx");
});

test("a directory entry and files beside it coexist", () => {
  const tree = buildTree([text("remote.git/"), text("a.txt")]);

  assert.deepEqual(
    tree.map((node) => node.name),
    ["remote.git", "a.txt"],
  );
  assert.equal(
    tree.reduce((total, node) => total + countFiles(node), 0),
    2,
    "every entry is reachable; none is a phantom",
  );
});

test("no node anywhere in a tree is nameless", () => {
  const tree = buildTree([
    text("remote.git/"),
    text("nested/"),
    text("plain/one.txt"),
    text("plain/deep/two.txt"),
  ]);

  const walk = (nodes: typeof tree): void => {
    for (const node of nodes) {
      assert.notEqual(node.name, "", `empty node at ${node.path}`);
      assert.ok(
        node.file || node.children.length > 0,
        `${node.path} is neither a file nor a directory with contents`,
      );
      walk(node.children);
    }
  };
  walk(tree);
});

test("a path that is only slashes is kept rather than vanishing", () => {
  assert.equal(normaliseTreePath("/"), "/");
  assert.equal(normaliseTreePath("a/"), "a");
  assert.equal(normaliseTreePath("a//"), "a");
});
