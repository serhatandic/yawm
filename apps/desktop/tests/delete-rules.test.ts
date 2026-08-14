/**
 * Run with: node --experimental-strip-types --test tests/delete-rules.test.ts
 *
 * Deliberately dependency-free — the rules under test are pure, so they need a
 * runner and nothing else.
 */

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  asFailure,
  branchNamesOf,
  confirmBlocked,
  deleteTitle,
  destroysWork,
  freshOptions,
  isPlanChanged,
  keptBranchesOf,
  lockedPlans,
  mergeKeptBranches,
  offersBranchDeletion,
  plansMatchSelection,
  optionsConflict,
  remainingPlans,
  removalOptionsFor,
  removalRequestsFor,
  replanOptions,
  replanTargets,
  unverifiedBranchesOf,
  mergeUnverifiedBranches,
  vanishedFrom,
  type DeleteOptions,
  type UnverifiedBranch,
} from "../src/components/delete-rules.ts";
import type { RemovalPlan, StateFingerprint, Worktree } from "../src/lib/api.ts";

/** The authorisation core attaches to every plan; these rules never read it. */
function fingerprint(): StateFingerprint {
  return { version: "yawm.state.v1", digest: "d0", unproven: false };
}

function plan(overrides: Partial<RemovalPlan> = {}): RemovalPlan {
  return {
    path: "/w/feature",
    branch: "feat/x",
    isMain: false,
    isLocked: false,
    lockReason: null,
    isPrunable: false,
    dirtyFiles: [],
    dirtyTotal: 0,
    unpushedCommits: 0,
    envFiles: [],
    runningProcesses: 0,
    requiresForce: false,
    state: fingerprint(),
    ...overrides,
  };
}

/** Only the fields these rules read; the rest of a worktree is not their business. */
function worktree(overrides: Partial<Worktree> = {}): Worktree {
  return {
    path: "/w/feature",
    head: "1234567890abcdef1234567890abcdef12345678",
    branch: "feat/x",
    detached: false,
    ...overrides,
  } as Worktree;
}

const allOptionsOn: DeleteOptions = {
  deleteBranch: true,
  forceBranch: true,
  useTrash: true,
  unlockLocked: true,
};

test("a clean worktree in a batch is not forced because another one is dirty", () => {
  const dirty = plan({ path: "/w/dirty", requiresForce: true });
  const clean = plan({ path: "/w/clean", requiresForce: false });
  const options = freshOptions();

  assert.equal(removalOptionsFor(dirty, options).force, true);
  assert.equal(
    removalOptionsFor(clean, options).force,
    false,
    "git must keep its right to refuse the clean one",
  );
});

test("force never promotes branch deletion to -D", () => {
  const dirty = plan({ requiresForce: true });
  const opts = removalOptionsFor(dirty, {
    deleteBranch: true,
    forceBranch: false,
    useTrash: false,
    unlockLocked: false,
  });

  assert.equal(opts.force, true);
  assert.equal(opts.forceBranch, false);
});

test("forceBranch is ignored unless branch deletion was actually asked for", () => {
  const opts = removalOptionsFor(plan(), {
    deleteBranch: false,
    forceBranch: true,
    useTrash: false,
    unlockLocked: false,
  });
  assert.equal(opts.forceBranch, false);
});

test("every destructive option is off when the dialog opens", () => {
  assert.deepEqual(freshOptions(), {
    deleteBranch: false,
    forceBranch: false,
    useTrash: false,
    unlockLocked: false,
  });
  assert.notDeepEqual(
    freshOptions(),
    allOptionsOn,
    "options must not carry over from the previous target",
  );
});

test("freshOptions hands back a new object each time", () => {
  const first = freshOptions();
  first.deleteBranch = true;
  assert.equal(freshOptions().deleteBranch, false);
});

test("plans for a different worktree are refused", () => {
  const stale = [plan({ path: "/w/a" })];
  assert.equal(plansMatchSelection(stale, ["/w/b"]), false);
  assert.equal(plansMatchSelection(stale, ["/w/a"]), true);
});

test("a plan set that does not cover the whole selection is refused", () => {
  const partial = [plan({ path: "/w/a" })];
  assert.equal(plansMatchSelection(partial, ["/w/a", "/w/b"]), false);
  assert.equal(plansMatchSelection([], ["/w/a"]), false);
});

test("work that exists nowhere else is recognised in each of its forms", () => {
  assert.equal(destroysWork(plan()), false);
  assert.equal(destroysWork(plan({ dirtyTotal: 1 })), true);
  assert.equal(destroysWork(plan({ unpushedCommits: 1 })), true);
  assert.equal(destroysWork(plan({ envFiles: ["apps/api/.env"] })), true);
  assert.equal(
    destroysWork(plan({ runningProcesses: 3 })),
    false,
    "a running process is a reason to hesitate, not work that would be lost",
  );
});

test("core's re-plan refusal is told apart from an ordinary failure", () => {
  const refusal = asFailure({
    kind: "planChanged",
    message:
      "/w/dirty changed since it was checked: new uncommitted files: late.txt. " +
      "Nothing was deleted — check again before deleting.",
    path: "/w/dirty",
    changes: ["new uncommitted files: late.txt"],
    stillPresent: ["/w/dirty", "/w/clean"],
  });

  assert.equal(isPlanChanged(refusal), true);
  assert.equal(
    isPlanChanged(asFailure({ kind: "failed", message: "permission denied" })),
    false,
    "a real failure must not send the dialog into a re-plan loop",
  );
});

test("a re-plan refusal is recognised by its kind and not by its wording", () => {
  // The message this app used to match on, arriving as an ordinary failure —
  // a branch or path can be named anything, including the sentence core uses.
  const misleading = asFailure(
    "cannot remove /w/changed since it was checked: permission denied",
  );

  assert.equal(
    isPlanChanged(misleading),
    false,
    "a failure that merely reads like the refusal must still be shown as a failure",
  );

  // And the refusal is still recognised once its wording changes.
  const reworded = asFailure({
    kind: "planChanged",
    message: "the worktree is not what you approved",
    path: "/w/dirty",
    changes: ["3 more uncommitted files"],
  });
  assert.equal(isPlanChanged(reworded), true);
});

test("anything unrecognised still reaches the user as a failure", () => {
  assert.deepEqual(asFailure(new Error("boom")), {
    kind: "failed",
    message: "Error: boom",
  });
  assert.equal(asFailure({ kind: "somethingElse" }).kind, "failed");
  assert.equal(asFailure("plain string").message, "plain string");
});

test("a batch that failed part-way is read as what it removed and what it did not", () => {
  // The failure that must never reach the user as a bare error: the first
  // worktree is gone for good, and a dialog that reported only "it failed"
  // would leave the app listing a directory that no longer exists.
  const partial = asFailure({
    kind: "partial",
    message:
      "could not delete /w/b: permission denied. /w/a was already deleted and cannot be restored.",
    completed: [{ path: "/w/a", outcome: { branch: "deleted" } }],
    failed: "/w/b",
  });

  assert.equal(partial.kind, "partial");
  if (partial.kind !== "partial") return;
  assert.deepEqual(
    partial.completed.map((c) => c.path),
    ["/w/a"],
    "what is gone has to be nameable, or the app cannot reconcile it",
  );
  assert.equal(partial.completed[0]?.outcome.branch, "deleted");
  assert.equal(partial.failed, "/w/b");
  assert.equal(
    isPlanChanged(partial),
    false,
    "this is not a re-plan: something was deleted, so it must not loop back into planning",
  );
});

test("a partial failure with an unreadable outcome still names the worktree it removed", () => {
  const partial = asFailure({
    kind: "partial",
    message: "stopped",
    completed: [{ path: "/w/a", outcome: { branch: "nonsense" } }],
    failed: "/w/b",
  });

  assert.equal(partial.kind, "partial");
  if (partial.kind !== "partial") return;
  assert.deepEqual(partial.completed.map((c) => c.path), ["/w/a"]);
  assert.equal(
    partial.completed[0]?.outcome.branch,
    "unknown",
    "an outcome this build cannot read means nobody knows what became of the branch — it is not 'nothing happened'",
  );
});

test("an absent branch outcome is the only one read as nothing having been asked", () => {
  const partial = asFailure({
    kind: "partial",
    message: "stopped",
    completed: [{ path: "/w/a", outcome: {} }],
    failed: "/w/b",
  });

  assert.equal(partial.kind, "partial");
  if (partial.kind !== "partial") return;
  assert.equal(
    partial.completed[0]?.outcome.branch,
    "notRequested",
    "a payload with no branch field describes a removal with no branch in play",
  );
});

test("the outcomes that establish nothing survive the boundary as themselves", () => {
  // Read as `notRequested`, these told the user their branch was untouched:
  // one may have been deleted with the restore failing, and the other core
  // never got to see. Both are actionable only if they arrive intact.
  const partial = asFailure({
    kind: "partial",
    message: "stopped",
    completed: [
      { path: "/w/a", outcome: { branch: "rollbackFailed" } },
      { path: "/w/b", outcome: { branch: "unknown" } },
    ],
    failed: "/w/c",
  });

  assert.equal(partial.kind, "partial");
  if (partial.kind !== "partial") return;
  assert.equal(partial.completed[0]?.outcome.branch, "rollbackFailed");
  assert.equal(partial.completed[1]?.outcome.branch, "unknown");
});

test("a removal core reconciled after a failure is still read as removed", () => {
  /*
   * Core reads the repository back after any failure, so a worktree whose
   * directory went to the trash before the prune that should have followed it
   * failed arrives as `removedButFinalizationFailed`. It is gone, and a dialog
   * that dropped it because the status was unfamiliar would leave the app
   * listing a directory that no longer exists — the exact failure this whole
   * path exists to prevent.
   */
  const partial = asFailure({
    kind: "partial",
    message: "stopped",
    completed: [
      { path: "/w/a", outcome: { branch: "deleted" }, status: "removed" },
      {
        path: "/w/b",
        outcome: { branch: "kept" },
        status: "removedButFinalizationFailed",
      },
      { path: "/w/c", outcome: { branch: "notRequested" } },
    ],
    failed: "/w/d",
  });

  assert.equal(partial.kind, "partial");
  if (partial.kind !== "partial") return;
  assert.deepEqual(
    partial.completed.map((c) => c.path),
    ["/w/a", "/w/b", "/w/c"],
    "everything core says is gone is gone, whatever finished afterwards",
  );
  assert.deepEqual(
    partial.completed.map((c) => c.status),
    ["removed", "removedButFinalizationFailed", "removed"],
    "an entry with no status at all ran to the end; that is the honest reading",
  );
  assert.equal(
    partial.completed[1]?.outcome.branch,
    "kept",
    "a reconciled removal never claims a branch deletion it could not prove",
  );
});

test("a batch that removed nothing still hands back what disappeared", () => {
  /*
   * yawm deleted none of them and two are gone anyway — removed by an agent,
   * a script, or a person while the batch ran. This used to arrive as prose in
   * a generic failure, so the dialog kept listing directories that are not
   * there with their tabs open. The paths cross as paths.
   */
  const gone = asFailure({
    kind: "vanished",
    message: "git refused. /w/c and /w/d are no longer there.",
    vanished: ["/w/c", "/w/d"],
    failed: "/w/b",
  });

  assert.equal(gone.kind, "vanished");
  if (gone.kind !== "vanished") return;
  assert.deepEqual(gone.vanished, ["/w/c", "/w/d"]);
  assert.equal(gone.failed, "/w/b");
  assert.equal(
    isPlanChanged(gone),
    false,
    "the plans were valid; this must not loop back into planning",
  );

  // What the dialog does with it: the gone rows stop being offered, and
  // nothing is claimed as removed by yawm.
  const held = [plan({ path: "/w/b" }), plan({ path: "/w/c" }), plan({ path: "/w/d" })];
  assert.deepEqual(
    remainingPlans(held, gone.vanished).map((p) => p.path),
    ["/w/b"],
  );
});

test("a vanished failure with an unreadable shape is still a vanished failure", () => {
  const gone = asFailure({ kind: "vanished", message: "stopped" });

  assert.equal(gone.kind, "vanished");
  if (gone.kind !== "vanished") return;
  assert.deepEqual(gone.vanished, []);
  assert.equal(gone.failed, "");
});

test("the authorisation a plan carries is opaque and fixed-size", () => {
  /*
   * The fingerprint used to carry every dirty path and every file outside git,
   * uncapped, in the payload the dialog holds for each selected worktree and
   * hands back on confirm. One worktree with a few hundred modified files put
   * a few hundred records across the boundary and then back again, and every
   * path in it was readable by the webview.
   *
   * Three fixed fields is all an authorisation needs to be handed back with.
   */
  const dirty = Array.from({ length: 250 }, (_, i) => `src/deeply/nested/file-${i}.ts`);
  const small = plan({ path: "/w/a", dirtyFiles: [], dirtyTotal: 0 });
  const huge = plan({ path: "/w/a", dirtyFiles: dirty, dirtyTotal: dirty.length });

  assert.equal(
    JSON.stringify(small.state).length,
    JSON.stringify(huge.state).length,
    "the state a removal is authorised against does not grow with the work",
  );
  assert.ok(JSON.stringify(small.state).length < 200);
  assert.deepEqual(
    Object.keys(small.state).sort(),
    ["digest", "unproven", "version"],
    "and it names nothing about the worktree's contents",
  );
});

test("an unfamiliar status does not invent a half-finished removal", () => {
  const partial = asFailure({
    kind: "partial",
    message: "stopped",
    completed: [{ path: "/w/a", outcome: { branch: "kept" }, status: 7 }],
    failed: "/w/b",
  });

  assert.equal(partial.kind, "partial");
  if (partial.kind !== "partial") return;
  assert.equal(partial.completed[0]?.status, "removed");
});

test("trash and branch deletion cannot be held at the same time", () => {
  // The pair the core refuses. If this ever passes, the dialog is able to
  // build a request that comes back as an error the user did not cause.
  assert.equal(
    optionsConflict({
      deleteBranch: true,
      forceBranch: false,
      useTrash: true,
      unlockLocked: false,
    }),
    true,
  );

  for (const options of [
    { deleteBranch: true, forceBranch: false, useTrash: false, unlockLocked: false },
    { deleteBranch: false, forceBranch: false, useTrash: true, unlockLocked: false },
    { deleteBranch: false, forceBranch: false, useTrash: false, unlockLocked: false },
  ]) {
    assert.equal(optionsConflict(options), false);
  }
});

// ---------------------------------------------------------------------------
// A lock is answered on its own
// ---------------------------------------------------------------------------

test("a locked worktree cannot be deleted by confirming uncommitted changes", () => {
  const locked = plan({ isLocked: true, lockReason: "agent running" });
  const options = freshOptions();

  assert.equal(
    confirmBlocked([locked], true, options),
    true,
    "the destructive acknowledgement is about files, and a lock is not a file",
  );
  assert.equal(
    confirmBlocked([locked], true, { ...options, unlockLocked: true }),
    false,
  );
});

test("a lock blocks the confirm button even when nothing would be lost", () => {
  // Clean, nothing unpushed, nothing outside git: the destructive panel never
  // appears, so the lock is the only thing there is to acknowledge.
  const locked = plan({ isLocked: true, lockReason: "release in progress" });

  assert.equal(destroysWork(locked), false);
  assert.equal(confirmBlocked([locked], false, freshOptions()), true);
  assert.equal(
    confirmBlocked([locked], false, { ...freshOptions(), unlockLocked: true }),
    false,
  );
});

test("an unlock is only sent for the worktrees that are actually locked", () => {
  const locked = plan({ path: "/w/locked", isLocked: true });
  const open = plan({ path: "/w/open" });
  const options = { ...freshOptions(), unlockLocked: true };

  assert.equal(removalOptionsFor(locked, options).unlock, true);
  assert.equal(
    removalOptionsFor(open, options).unlock,
    false,
    "authorising one lock must not carry an unlock to worktrees that hold none",
  );
});

test("unlocking is never implied by force", () => {
  const dirtyAndLocked = plan({ requiresForce: true, isLocked: true });
  const opts = removalOptionsFor(dirtyAndLocked, freshOptions());

  assert.equal(opts.force, true);
  assert.equal(opts.unlock, false);
});

test("the locked worktrees are the ones named to the user", () => {
  const plans = [
    plan({ path: "/w/a" }),
    plan({ path: "/w/b", isLocked: true, lockReason: "agent running" }),
    plan({ path: "/w/c", isLocked: true, lockReason: null }),
  ];

  assert.deepEqual(
    lockedPlans(plans).map((p) => p.path),
    ["/w/b", "/w/c"],
  );
  assert.equal(lockedPlans(plans)[0]?.lockReason, "agent running");
});

test("nothing is confirmable while the plans are still being fetched", () => {
  assert.equal(confirmBlocked(null, true, allOptionsOn), true);
});

// ---------------------------------------------------------------------------
// The whole selection goes to core as one request
// ---------------------------------------------------------------------------

test("every plan is sent together, each with its own options", () => {
  const dirty = plan({ path: "/w/dirty", requiresForce: true });
  const locked = plan({ path: "/w/locked", isLocked: true });
  const clean = plan({ path: "/w/clean" });
  const requests = removalRequestsFor([dirty, locked, clean], {
    ...freshOptions(),
    unlockLocked: true,
  });

  assert.deepEqual(
    requests.map((r) => r.plan.path),
    ["/w/dirty", "/w/locked", "/w/clean"],
    "the batch core validates is the batch the user selected",
  );
  assert.equal(requests[0]?.options.force, true);
  assert.equal(requests[2]?.options.force, false);
  assert.equal(requests[1]?.options.unlock, true);
  assert.equal(requests[2]?.options.unlock, false);
});

test("a re-plan after a refusal never asks about a worktree the repository no longer has", () => {
  const selected = ["/w/a", "/w/b"];

  assert.deepEqual(
    replanTargets(selected, ["/w/a", "/w/main"]),
    ["/w/a"],
    "core listed what it still has; /w/b went while the dialog was open",
  );
  assert.deepEqual(
    replanTargets(selected, ["/w/a", "/w/b", "/w/main"]),
    selected,
    "nothing was deleted, so the whole selection is re-planned",
  );
  assert.deepEqual(replanTargets(selected, []), []);
});

test("a worktree that went while the dialog was open leaves the selection", () => {
  /*
   * The complement of `replanTargets`, and the reason the dialog's selection
   * has to be mutable. Core refuses and names what the repository still has;
   * the rest are gone, removed by something that is not yawm.
   *
   * Left in the selection, the next confirm validates its plans — built for
   * the re-planned targets only — against a selection that still names the
   * missing one. `plansMatchSelection` fails, the dialog re-plans, core answers
   * with the same list, and the user is asked about a worktree that is not
   * there, forever.
   */
  const selected = ["/w/a", "/w/b", "/w/c"];
  const stillPresent = ["/w/a", "/w/c", "/w/main"];

  const targets = replanTargets(selected, stillPresent);
  const missing = vanishedFrom(selected, stillPresent);

  assert.deepEqual(missing, ["/w/b"]);
  assert.deepEqual(
    [...targets, ...missing].sort(),
    [...selected].sort(),
    "every selected worktree is either re-planned or accounted for as gone",
  );

  // What the dialog does next: the selection drops the missing one, and the
  // plans it re-plans are exactly what the next confirm validates against.
  const nextSelection = selected.filter((path) => !missing.includes(path));
  const replanned = targets.map((path) => plan({ path }));
  assert.equal(
    plansMatchSelection(replanned, nextSelection),
    true,
    "the re-planned targets and the selection are the same set",
  );
  assert.equal(
    plansMatchSelection(replanned, selected),
    false,
    "and they would not be, had the missing one stayed in",
  );
});

test("a selection that has entirely gone offers nothing to confirm", () => {
  const selected = ["/w/a", "/w/b"];
  const stillPresent = ["/w/main"];

  assert.deepEqual(replanTargets(selected, stillPresent), []);
  assert.deepEqual(vanishedFrom(selected, stillPresent), selected);
  assert.equal(
    confirmBlocked([], false, freshOptions()),
    true,
    "an empty batch would be carried out successfully and delete nothing, \
     while the dialog reported a deletion",
  );
  assert.equal(
    deleteTitle([]),
    "Nothing left to delete",
    "'Delete 0 worktrees?' asks a question with no answer",
  );
});

test("kept branches accumulate over a dialog instead of replacing each other", () => {
  /*
   * One dialog reports more than once: a batch fails part-way, the user
   * retries what is left, and each attempt answers only about the worktrees it
   * touched. Sending the app the latest answer replaced the earlier one, so a
   * branch git refused to delete in the first attempt stopped being mentioned
   * as soon as the second finished — while the branch was still there.
   */
  const afterPartial = mergeKeptBranches([], ["feat/a"]);
  assert.deepEqual(afterPartial, ["feat/a"]);

  const afterRetry = mergeKeptBranches(afterPartial, ["feat/b"]);
  assert.deepEqual(
    afterRetry,
    ["feat/a", "feat/b"],
    "the first refusal is still true after the second attempt",
  );

  assert.deepEqual(
    mergeKeptBranches(afterRetry, []),
    afterRetry,
    "an attempt that kept no branch erases nothing",
  );
  assert.deepEqual(
    mergeKeptBranches(afterRetry, ["feat/a", "feat/c"]),
    ["feat/a", "feat/b", "feat/c"],
    "the same worktree reported twice is named once",
  );
  assert.deepEqual(
    mergeKeptBranches([], ["feat/z"]),
    ["feat/z"],
    "a new dialog starts from nothing, so nothing carries into the next target",
  );
});

test("a re-plan drops every authorisation the last plan was granted", () => {
  const granted: DeleteOptions = {
    deleteBranch: true,
    forceBranch: true,
    useTrash: true,
    unlockLocked: true,
  };

  const next = replanOptions(granted);

  assert.equal(
    next.deleteBranch,
    false,
    "the branch that was ticked belonged to the plan that just went stale",
  );
  assert.equal(
    next.forceBranch,
    false,
    "forcing past git's unmerged check must be re-asked, never inherited",
  );
  assert.equal(
    next.unlockLocked,
    false,
    "a lock that now says something else is a different instruction",
  );
  assert.equal(
    next.useTrash,
    true,
    "where deleted things go is a preference, not permission to destroy anything",
  );
});

// ---------------------------------------------------------------------------
// After a batch that failed part-way
// ---------------------------------------------------------------------------

test("plans for worktrees that were already deleted are dropped", () => {
  const plans = [
    plan({ path: "/w/a" }),
    plan({ path: "/w/b" }),
    plan({ path: "/w/c" }),
  ];

  assert.deepEqual(
    remainingPlans(plans, ["/w/a"]).map((p) => p.path),
    ["/w/b", "/w/c"],
    "a dialog still offering to delete what it already deleted is the same failure, quieter",
  );
  assert.deepEqual(remainingPlans(plans, []).map((p) => p.path), [
    "/w/a",
    "/w/b",
    "/w/c",
  ]);
  assert.deepEqual(remainingPlans(plans, ["/w/a", "/w/b", "/w/c"]), []);
});

test("kept branches are matched by path, not by position", () => {
  // A batch that failed at the second worktree reports only the first. Reading
  // outcomes off the plan order would name the wrong branch here.
  const plans = [
    plan({ path: "/w/a", branch: "feat/a" }),
    plan({ path: "/w/b", branch: "feat/b" }),
    plan({ path: "/w/c", branch: "feat/c" }),
  ];

  assert.deepEqual(
    keptBranchesOf(plans, [{ path: "/w/c", outcome: { branch: "kept" } }]),
    ["feat/c"],
  );
  assert.deepEqual(
    keptBranchesOf(plans, [
      { path: "/w/a", outcome: { branch: "deleted" } },
      { path: "/w/b", outcome: { branch: "kept" } },
    ]),
    ["feat/b"],
    "only the refusal is worth reporting; a deleted branch needs no notice",
  );
  assert.deepEqual(
    keptBranchesOf(plans, [{ path: "/w/gone", outcome: { branch: "kept" } }]),
    [],
    "an outcome for a worktree that is not in these plans names nothing",
  );
  assert.deepEqual(
    keptBranchesOf(
      [plan({ path: "/w/d", branch: null })],
      [{ path: "/w/d", outcome: { branch: "kept" } }],
    ),
    [],
    "a detached worktree has no branch to have kept",
  );
});

test("a branch nobody can vouch for is never reported as kept", () => {
  const plans = [
    plan({ path: "/w/a", branch: "feat/a" }),
    plan({ path: "/w/b", branch: "feat/b" }),
  ];

  assert.deepEqual(
    keptBranchesOf(plans, [
      { path: "/w/a", outcome: { branch: "unknown" } },
      { path: "/w/b", outcome: { branch: "rollbackFailed" } },
    ]),
    [],
    "'kept' promises the branch is exactly where it was, and neither of these can promise that",
  );
});

test("the branches whose state was never established are named with why", () => {
  const plans = [
    plan({ path: "/w/a", branch: "feat/a" }),
    plan({ path: "/w/b", branch: "feat/b" }),
    plan({ path: "/w/c", branch: "feat/c" }),
    plan({ path: "/w/d", branch: null }),
  ];

  assert.deepEqual(
    unverifiedBranchesOf(plans, [
      { path: "/w/a", outcome: { branch: "rollbackFailed" } },
      { path: "/w/b", outcome: { branch: "deleted" } },
      { path: "/w/c", outcome: { branch: "unknown" } },
    ]),
    [
      { branch: "feat/a", outcome: "rollbackFailed" },
      { branch: "feat/c", outcome: "unknown" },
    ],
    "matched by path, and carrying which of the two unresolved endings it was",
  );

  assert.deepEqual(
    unverifiedBranchesOf(plans, [
      { path: "/w/d", outcome: { branch: "unknown" } },
      { path: "/w/gone", outcome: { branch: "rollbackFailed" } },
    ]),
    [],
    "a detached worktree has no branch to be unsure about, and neither has a path these plans do not name",
  );
});

test("an unverified branch stays reported after a later attempt finishes", () => {
  // The dialog reports once per attempt. Replacing the earlier answer made a
  // branch a failed rollback may have destroyed stop being mentioned the
  // moment the retry succeeded.
  const first: UnverifiedBranch[] = [
    { branch: "feat/a", outcome: "rollbackFailed" },
  ];

  assert.deepEqual(
    mergeUnverifiedBranches(first, [{ branch: "feat/b", outcome: "unknown" }]),
    [
      { branch: "feat/a", outcome: "rollbackFailed" },
      { branch: "feat/b", outcome: "unknown" },
    ],
  );
  assert.deepEqual(
    mergeUnverifiedBranches(first, [
      { branch: "feat/a", outcome: "rollbackFailed" },
    ]),
    first,
    "a partial failure and the retry that follows can report the same branch twice",
  );
});

// ---------------------------------------------------------------------------
// A detached worktree has no branch, and is not called "worktree"
// ---------------------------------------------------------------------------

test("branch deletion is not offered when there is no branch", () => {
  const detached = worktree({ branch: null, detached: true });

  assert.deepEqual(branchNamesOf([detached]), []);
  assert.equal(offersBranchDeletion([detached]), false);
  assert.equal(offersBranchDeletion([worktree()]), true);
});

test("a mixed selection offers only the branches it actually has", () => {
  const selection = [
    worktree({ path: "/w/a", branch: "feat/a" }),
    worktree({ path: "/w/b", branch: null, detached: true }),
    worktree({ path: "/w/c", branch: "feat/c" }),
  ];

  assert.deepEqual(branchNamesOf(selection), ["feat/a", "feat/c"]);
  assert.equal(
    offersBranchDeletion(selection),
    true,
    "two of the three have a branch, so the option applies to those two",
  );
});

test("the branches offered for deletion are the ones in the current plans", () => {
  /*
   * The dialog reads these off the plans it is showing, not off the worktree
   * list it was opened with. A re-plan replaces the plans while those props
   * stay as they were, so reading the old list could put "also delete feat/a"
   * on screen over a request that would delete feat/b.
   */
  const replanned = [
    plan({ path: "/w/b", branch: "feat/b" }),
    plan({ path: "/w/detached", branch: null }),
  ];

  assert.deepEqual(branchNamesOf(replanned), ["feat/b"]);
  assert.equal(offersBranchDeletion(replanned), true);
  assert.equal(
    offersBranchDeletion([plan({ path: "/w/detached", branch: null })]),
    false,
  );
  assert.deepEqual(
    branchNamesOf([]),
    [],
    "no plans yet means nothing to authorise deleting",
  );
});

test("a detached worktree is identified by its head, not called 'worktree'", () => {
  const detached = worktree({
    branch: null,
    detached: true,
    head: "abcdef1234567890abcdef1234567890abcdef12",
  });

  assert.equal(deleteTitle([detached]), "Delete detached at abcdef1?");
  assert.equal(deleteTitle([worktree()]), "Delete feat/x?");
  assert.equal(
    deleteTitle([worktree({ path: "/w/a" }), detached]),
    "Delete 2 worktrees?",
  );
});
