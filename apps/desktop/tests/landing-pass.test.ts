import { strict as assert } from "node:assert";
import test from "node:test";

import type { RepoReport, Verdict, Worktree } from "../src/lib/api.ts";
import {
  landingTargets,
  mergeLandingAnswer,
} from "../src/lib/landing-pass.ts";

function worktree(
  path: string,
  verdict: Verdict,
  landingComplete = false,
): Worktree {
  return {
    path,
    head: path,
    branch: path,
    detached: false,
    bare: false,
    isMain: false,
    locked: null,
    prunable: null,
    status: {
      dirty: {
        staged: 0,
        unstaged: 0,
        untracked: 0,
        inspectionFailed: false,
      },
      uncommitted: { state: "notChecked" },
      upstream: { name: null, ahead: 0, behind: 0, gone: false },
      landing: {
        state: "unknown",
        reason: { kind: "checkDeferred" },
        candidate: null,
      },
      landingComplete,
      lastCommitAt: null,
      lastCommitSubject: null,
      envFiles: [],
      size: { bytes: 12, files: 1, heavyDirs: [], lastModified: null },
      processes: [],
      processCheckComplete: true,
    },
    verdict,
    reason: { kind: "landingUnknown" },
  };
}

test("the landing pass includes the full list and puts decision-changing rows first", () => {
  const worktrees = Array.from({ length: 21 }, (_, index) =>
    worktree(`/worktree-${index}`, index === 20 ? "review" : "keep"),
  );
  const reports: RepoReport[] = [
    {
      name: "repo",
      root: "/repo",
      defaultRef: "main",
      worktrees: [...worktrees, worktree("/already-done", "disposable", true)],
    },
  ];

  const targets = landingTargets(reports);

  assert.equal(targets.length, 21);
  assert.equal(targets[0]?.worktree, "/worktree-20");
  assert.equal(targets.some((target) => target.worktree === "/already-done"), false);
});

test("a landing result keeps size but accepts its newer process inspection", () => {
  const current = worktree("/branch", "review");
  current.status.processes = [{ pid: 42, name: "agent" }];
  current.verdict = "keep";
  current.reason = { kind: "processRunning" };
  const answer = worktree("/branch", "disposable", true);
  answer.status.landing = {
    state: "landed",
    target: "main",
    proof: { kind: "ancestry" },
  };
  answer.reason = { kind: "workContained", target: "main" };
  answer.status.size = null;

  const merged = mergeLandingAnswer(current, answer);

  assert.equal(merged.status.size?.bytes, 12);
  assert.deepEqual(merged.status.processes, []);
  assert.equal(merged.status.processCheckComplete, true);
  assert.equal(merged.status.landingComplete, true);
  assert.equal(merged.verdict, "disposable");
  assert.deepEqual(merged.reason, { kind: "workContained", target: "main" });
});

test("dirty rows join the progressive pass even when committed landing is settled", () => {
  const dirty = worktree("/dirty", "keep", true);
  dirty.status.dirty.unstaged = 1;
  const reports: RepoReport[] = [
    {
      name: "repo",
      root: "/repo",
      defaultRef: "main",
      worktrees: [dirty],
    },
  ];

  assert.deepEqual(landingTargets(reports), [
    { repo: "/repo", worktree: "/dirty" },
  ]);

  const answer = worktree("/dirty", "review", true);
  answer.status.dirty.unstaged = 1;
  answer.status.uncommitted = {
    state: "compared",
    target: "main",
    leftover: 0,
    leftoverSample: [],
    incomplete: false,
  };
  const merged = mergeLandingAnswer(dirty, answer);
  assert.deepEqual(merged.status.uncommitted, answer.status.uncommitted);
});
