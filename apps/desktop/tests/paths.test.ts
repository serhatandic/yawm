import { test } from "node:test";
import assert from "node:assert/strict";

import { joinPath, parentPath, pathName } from "../src/lib/paths.ts";

test("path components work with POSIX paths", () => {
  assert.equal(parentPath("/worktrees/feature"), "/worktrees");
  assert.equal(pathName("/worktrees/feature"), "feature");
  assert.equal(joinPath("/chosen/root", "feature"), "/chosen/root/feature");
});

test("path components work with Windows paths", () => {
  assert.equal(parentPath("C:\\worktrees\\feature"), "C:\\worktrees");
  assert.equal(pathName("C:\\worktrees\\feature"), "feature");
  assert.equal(joinPath("D:\\chosen\\root", "feature"), "D:\\chosen\\root\\feature");
});

test("joining tolerates directory and leaf separators", () => {
  assert.equal(joinPath("/", "/feature"), "/feature");
  assert.equal(joinPath("C:\\", "\\feature"), "C:\\feature");
  assert.equal(parentPath("/feature"), "/");
  assert.equal(parentPath("C:\\feature"), "C:\\");
});
