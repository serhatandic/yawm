/**
 * Run with: node --experimental-strip-types --test tests/deadline.test.ts
 *
 * The bug these exist for is not slowness. It is a promise that never settles
 * at all — the reply dropped, the callback gone — which no `catch` and no
 * `finally` can reach. Only a deadline can, so these check that the deadline is
 * real, that it never fires on work that was going to succeed, and that the
 * two calls which must not have one still do not.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  INSPECT_MS,
  QUICK_MS,
  SCAN_FAST_MS,
  SCAN_FULL_MS,
  TimeoutError,
  deadlineFor,
  isTimeout,
  withDeadline,
} from "../src/lib/deadline.ts";

test("a call that never settles rejects at the deadline", async () => {
  // Exactly the reported failure: nobody will ever resolve or reject this.
  const abandoned = new Promise<number>(() => {});

  await assert.rejects(
    () => withDeadline(abandoned, "scan_all", 20),
    (error: unknown) => {
      assert.ok(isTimeout(error), "a lost call must be recognisably a timeout");
      assert.match(String(error), /scan_all/);
      return true;
    },
  );
});

test("a slow call that finishes inside its deadline still succeeds", async () => {
  const slow = new Promise<string>((resolve) => setTimeout(() => resolve("21 worktrees"), 30));

  assert.equal(await withDeadline(slow, "scan_all", 5_000), "21 worktrees");
});

test("a timeout is distinguishable from an ordinary failure", async () => {
  const broken = Promise.reject(new Error("git not found"));

  await assert.rejects(
    () => withDeadline(broken, "scan_all", 5_000),
    (error: unknown) => {
      assert.equal(isTimeout(error), false, "a real error must not read as a timeout");
      assert.match(String(error), /git not found/);
      return true;
    },
  );
});

test("a timeout is recognised without instanceof surviving the bundler", () => {
  // What a second copy of the module produces: same shape, different class.
  const duplicate = { timedOut: true, message: "scan_all did not answer" };

  assert.ok(isTimeout(duplicate));
  assert.ok(isTimeout(new TimeoutError("scan_all", 1_000)));
  assert.equal(isTimeout(new Error("boom")), false);
  assert.equal(isTimeout("boom"), false);
  assert.equal(isTimeout(null), false);
});

test("the deadline does not outlive the call that finished", async () => {
  let rejected: unknown = null;
  process.on("unhandledRejection", (reason) => {
    rejected = reason;
  });

  // A call that fails after its own deadline has passed must not surface later
  // as an unhandled rejection against a screen that already moved on.
  const late = new Promise((_, reject) => setTimeout(() => reject(new Error("late")), 15));
  await assert.rejects(() => withDeadline(late, "scan_all", 5), isTimeout);
  await new Promise((resolve) => setTimeout(resolve, 40));

  assert.equal(rejected, null);
});

test("the deleting and creating calls are deliberately unbounded", () => {
  assert.equal(
    deadlineFor("remove_worktree"),
    null,
    "timing out a removal would claim it failed while it is still deleting",
  );
  assert.equal(deadlineFor("create_worktree"), null);
  assert.equal(
    deadlineFor("remove_worktrees"),
    null,
    "the batch form is what the delete dialog calls, and deleting five \
worktrees is the case that takes longest",
  );

  const untouched = new Promise<number>(() => {});
  assert.equal(withDeadline(untouched, "remove_worktree", null), untouched);
  assert.equal(withDeadline(untouched, "remove_worktrees", null), untouched);
});

test("the full scan gets far more room than a real one needs", () => {
  const measuredDebug = 30_000; // 21 worktrees, 22.6 GB
  const measuredFast = 2_600;

  assert.equal(deadlineFor("scan_all", { full: true }), SCAN_FULL_MS);
  assert.equal(deadlineFor("scan_all", { full: false }), SCAN_FAST_MS);

  assert.ok(
    SCAN_FULL_MS >= 10 * measuredDebug,
    "a ceiling near the measured time would break the heaviest machines",
  );
  assert.ok(SCAN_FAST_MS >= 10 * measuredFast);
  assert.ok(
    SCAN_FAST_MS < SCAN_FULL_MS,
    "a workspace switch waits on the fast pass and must not inherit the slow one's ceiling",
  );
});

test("analysis gets many times the measured cost, and trivia does not", () => {
  const landingCold = 5_000;

  assert.equal(deadlineFor("inspect_worktree"), INSPECT_MS);
  assert.equal(deadlineFor("plan_removals"), INSPECT_MS);
  assert.ok(INSPECT_MS >= 10 * landingCold);

  assert.equal(deadlineFor("get_config"), QUICK_MS);
  assert.equal(deadlineFor("set_active_workspace"), QUICK_MS);
  assert.equal(
    deadlineFor("a_command_added_later"),
    QUICK_MS,
    "a new command must get a ceiling by default rather than by being remembered",
  );
});
