/**
 * Run with: node --experimental-strip-types --test tests/scan-progress.test.ts
 *
 * The deadline itself lives in `lib/deadline.ts`; these cover what the list
 * does with the answer. The load-bearing case is supersession: a scan the user
 * has navigated away from may time out, and must still say nothing.
 */

import { strict as assert } from "node:assert";
import test from "node:test";

import { settleScan } from "../src/components/scan-progress.ts";
import { TimeoutError, withDeadline } from "../src/lib/deadline.ts";

const always = () => true;
const never = () => false;

test("a scan that answers settles as done and carries its value", async () => {
  const settled = await settleScan(Promise.resolve(["a"]), { isCurrent: always });

  assert.deepEqual(settled, { state: "done", value: ["a"] });
});

test("a scan that rejects settles as failed rather than waiting", async () => {
  const settled = await settleScan(Promise.reject(new Error("git exploded")), {
    isCurrent: always,
  });

  assert.equal(settled.state, "failed");
  if (settled.state !== "failed") return;
  assert.equal(settled.timedOut, false);
  assert.match(settled.reason, /git exploded/);
});

test("a scan whose result never arrives fails, and says it was time", async () => {
  // The reported bug end to end: nothing ever settles this, and before the
  // deadline existed the await below would simply never return.
  const abandoned = new Promise(() => {});
  const settled = await settleScan(withDeadline(abandoned, "scan_all", 10), {
    isCurrent: always,
  });

  assert.equal(settled.state, "failed");
  if (settled.state !== "failed") return;
  assert.equal(
    settled.timedOut,
    true,
    "a lost scan is worth retrying, not reporting as broken",
  );
});

test("a superseded scan is not a failure, however it ended", async () => {
  const lost = await settleScan(Promise.reject(new TimeoutError("scan_all", 10)), {
    isCurrent: never,
  });
  const broken = await settleScan(Promise.reject(new Error("boom")), { isCurrent: never });
  const fine = await settleScan(Promise.resolve(1), { isCurrent: never });

  assert.deepEqual(lost, { state: "superseded" });
  assert.deepEqual(broken, { state: "superseded" });
  assert.deepEqual(fine, { state: "superseded" });
});

test("supersession is judged when the scan settles, not when it started", async () => {
  let current = true;
  let release: (value: number) => void = () => {};
  const work = new Promise<number>((resolve) => {
    release = resolve;
  });

  const settling = settleScan(work, { isCurrent: () => current });
  current = false;
  release(7);

  assert.deepEqual(await settling, { state: "superseded" });
});

test("a scan that answers first is not failed by its own deadline", async () => {
  const quick = new Promise<string>((resolve) => setTimeout(() => resolve("quick"), 5));
  const settled = await settleScan(withDeadline(quick, "scan_all", 500), {
    isCurrent: always,
  });
  await new Promise((resolve) => setTimeout(resolve, 20));

  assert.deepEqual(settled, { state: "done", value: "quick" });
});
