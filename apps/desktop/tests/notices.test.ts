/**
 * Run with: node --experimental-strip-types --test tests/notices.test.ts
 *
 * These cover the app's answer to its worst failure mode: a shorter list, or an
 * empty one, that reads as reassurance. Every assertion here is about a
 * sentence existing at all.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  configNotice,
  keptBranchNotice,
  scanFailureNotice,
  scanNotice,
  unverifiedBranchNotice,
  visibleNotices,
} from "../src/components/notices.ts";
import type { ConfigStatus } from "../src/lib/api.ts";

test("a source that could not be read is named, with git's reason kept", () => {
  const notice = scanNotice([
    { path: "/Volumes/work/monorepo", reason: "No such file or directory" },
  ]);

  assert.ok(notice, "silence here is the bug: the row simply vanishes");
  assert.match(notice.text, /monorepo/);
  assert.match(
    notice.text,
    /No such file or directory/,
    "the reason is the only thing that tells an unmounted drive from a deleted directory",
  );
  assert.match(notice.text, /not gone/);
  assert.equal(notice.tone, "warning");
});

test("several unreadable sources are counted and named", () => {
  const notice = scanNotice([
    { path: "/Volumes/a/one", reason: "not a git repository" },
    { path: "/Volumes/a/two", reason: "not a git repository" },
    { path: "/Volumes/a/three", reason: "not a git repository" },
  ]);

  assert.ok(notice);
  assert.match(notice.text, /3 sources/);
  assert.match(notice.text, /one, two and three/);
});

test("a scan that read everything says nothing", () => {
  assert.equal(scanNotice([]), null);
});

test("settings that could not be read are reported, with where the original is", () => {
  const status: ConfigStatus = {
    state: "unusable",
    reason:
      'scanDepth is not a valid setting: invalid type: string "four", expected usize',
    backup: "/Users/me/.config/yawm/config.corrupt-1700000000.json",
  };

  const notice = configNotice(status);
  assert.ok(
    notice,
    "otherwise the user sees 'No repositories yet' and believes it",
  );
  assert.match(notice.text, /scanDepth/);
  assert.match(notice.text, /defaults/);
  assert.match(notice.text, /config\.corrupt-1700000000\.json/);
  assert.match(notice.text, /Nothing here has been written back over it/);
});

test("the ordinary settings outcomes are not news", () => {
  assert.equal(configNotice({ state: "missing" }), null);
  assert.equal(configNotice({ state: "loaded" }), null);
});

test("a branch git refused to delete is reported as the good news it is", () => {
  const notice = keptBranchNotice(["feature/rename-api"]);

  assert.ok(notice, "otherwise the user assumes the commits went too");
  assert.match(notice.text, /feature\/rename-api/);
  assert.match(notice.text, /not merged/);
  assert.match(notice.text, /Nothing was lost/);
  assert.equal(
    notice.tone,
    "info",
    "nothing failed here — the worktree went and the work survived",
  );
});

test("branches that were deleted as asked produce no notice", () => {
  assert.equal(keptBranchNotice([]), null);
});

test("a rollback that failed says so, and never that the branch was kept", () => {
  const notice = unverifiedBranchNotice([
    { branch: "feature/rename-api", outcome: "rollbackFailed" },
  ]);

  assert.ok(notice, "otherwise a branch that may be gone is never mentioned");
  assert.match(notice.text, /feature\/rename-api/);
  assert.match(notice.text, /rollback failed/i);
  assert.match(notice.text, /may no longer exist/i);
  assert.match(notice.text, /verif/i, "the user is told what to go and check");
  assert.equal(
    notice.tone,
    "warning",
    "a branch that may have been destroyed is not information, it is a problem",
  );
  assert.doesNotMatch(
    notice.text,
    /kept|Nothing was lost|not requested/i,
    "every one of those claims the branch is where the user left it",
  );
});

test("a branch state that could not be verified says exactly that", () => {
  const notice = unverifiedBranchNotice([
    { branch: "feature/scan-cache", outcome: "unknown" },
  ]);

  assert.ok(notice);
  assert.match(notice.text, /feature\/scan-cache/);
  assert.match(notice.text, /could not be verified/i);
  assert.match(notice.text, /check it in git/i);
  assert.equal(notice.tone, "warning");
  assert.doesNotMatch(
    notice.text,
    /kept|Nothing was lost|not requested|deleted as asked/i,
    "yawm does not know what happened, so it may not imply any of them",
  );
});

test("both kinds of unresolved branch are reported in one notice, apart", () => {
  const notice = unverifiedBranchNotice([
    { branch: "feat/a", outcome: "rollbackFailed" },
    { branch: "feat/b", outcome: "unknown" },
  ]);

  assert.ok(notice);
  assert.match(notice.text, /feat\/a/);
  assert.match(notice.text, /feat\/b/);
  assert.match(
    notice.text,
    /feat\/a.*rollback failed.*feat\/b.*could not be verified/is,
    "each branch keeps its own reason — they are not the same news",
  );
});

test("a removal that resolved every branch produces no unverified notice", () => {
  assert.equal(unverifiedBranchNotice([]), null);
});

test("a second, differently unresolved branch is not hidden by a dismissal", () => {
  const first = unverifiedBranchNotice([
    { branch: "feat/a", outcome: "unknown" },
  ]);
  const second = unverifiedBranchNotice([
    { branch: "feat/a", outcome: "rollbackFailed" },
  ]);
  assert.ok(first && second);
  assert.notEqual(
    first.id,
    second.id,
    "the same branch with a worse ending is new news",
  );
  assert.deepEqual(visibleNotices([second], [first.id]), [second]);
});

test("dismissing a notice does not hide the next, different one", () => {
  const first = scanNotice([{ path: "/code/alpha", reason: "offline" }]);
  assert.ok(first);

  assert.deepEqual(visibleNotices([first], [first.id]), []);

  const second = scanNotice([
    { path: "/code/alpha", reason: "offline" },
    { path: "/code/beta", reason: "offline" },
  ]);
  assert.ok(second);
  assert.equal(
    visibleNotices([second], [first.id]).length,
    1,
    "a second source failing is a new problem and must be shown",
  );
});

test("the same problem stays dismissed across a re-scan", () => {
  const before = scanNotice([{ path: "/code/alpha", reason: "offline" }]);
  const after = scanNotice([{ path: "/code/alpha", reason: "offline" }]);
  assert.ok(before && after);
  assert.equal(before.id, after.id);
  assert.deepEqual(visibleNotices([after], [before.id]), []);
});

test("nothing to report renders nothing", () => {
  assert.deepEqual(
    visibleNotices([scanNotice([]), configNotice({ state: "loaded" })], []),
    [],
  );
});

test("a scan that never came back says so and offers a way out", () => {
  const notice = scanFailureNotice({
    pass: "measuring",
    reason: "no result after 270s",
    timedOut: true,
  });

  assert.ok(notice, "a lost size pass must not be silent");
  assert.equal(notice.tone, "warning");
  assert.match(notice.text, /taking longer than expected/);
  assert.match(
    notice.text,
    /never measured/,
    "a blank size must not be readable as an empty worktree",
  );
  assert.deepEqual(
    notice.action,
    { label: "Try again" },
    "the user needs a retry that is not restarting the app",
  );
});

test("a timeout is not worded as a breakage", () => {
  const timedOut = scanFailureNotice({
    pass: "measuring",
    reason: "scan_all did not answer within 600s",
    timedOut: true,
  });

  assert.ok(timedOut);
  assert.doesNotMatch(
    timedOut.text,
    /failed/,
    "nothing is broken when a reply is merely late, and the retry is worth trying",
  );
});

test("a scan that failed outright quotes the reason", () => {
  const notice = scanFailureNotice({
    pass: "measuring",
    reason: "Error: git not found",
    timedOut: false,
  });

  assert.ok(notice);
  assert.match(notice.text, /git not found/);
  assert.notEqual(
    notice.id,
    scanFailureNotice({ pass: "measuring", reason: "x", timedOut: true })?.id,
    "a timeout and a crash are different problems and must dismiss separately",
  );
});

test("a failed listing pass warns that the list is not everything", () => {
  const notice = scanFailureNotice({
    pass: "listing",
    reason: "no result after 72s",
    timedOut: true,
  });

  assert.ok(notice);
  assert.match(notice.text, /incomplete/);
  assert.ok(notice.action);
});

test("a scan that worked says nothing", () => {
  assert.equal(scanFailureNotice(null), null);
});
