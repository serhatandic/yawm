import type { RepoReport, Worktree } from "./api.ts";

export interface LandingTarget {
  repo: string;
  worktree: string;
}

/**
 * Put answers that can change a deletion decision first, without excluding the
 * held rows whose completed proof still belongs in a full overview.
 */
export function landingTargets(reports: readonly RepoReport[]): LandingTarget[] {
  return reports
    .flatMap((repo) =>
      repo.worktrees
        .filter(
          (worktree) =>
            !worktree.status.landingComplete ||
            (hasDirtyChanges(worktree) &&
              (worktree.status.uncommitted.state === "notChecked" ||
                worktree.status.uncommitted.incomplete)),
        )
        .map((worktree) => ({ repo: repo.root, worktree })),
    )
    .sort(
      (left, right) =>
        priority(left.worktree) - priority(right.worktree),
    )
    .map(({ repo, worktree }) => ({ repo, worktree: worktree.path }));
}

function priority(worktree: Worktree): number {
  if (worktree.verdict === "review") return 0;
  if (worktree.verdict === "keep") return 1;
  return 2;
}

function hasDirtyChanges(worktree: Worktree): boolean {
  const dirty = worktree.status.dirty;
  return dirty.staged + dirty.unstaged + dirty.untracked > 0;
}

/**
 * A landing answer is tied to an immutable head. Keep the measured size because
 * the landing request deliberately skips that disk walk, but accept its process
 * result because it performs a newer, explicit process-table inspection.
 */
export function mergeLandingAnswer(
  current: Worktree,
  answer: Worktree,
): Worktree {
  if (current.head !== answer.head) return current;
  return {
    ...current,
    status: {
      ...current.status,
      landing: answer.status.landing,
      landingComplete: answer.status.landingComplete,
      uncommitted: answer.status.uncommitted,
      processes: answer.status.processes,
      processCheckComplete: answer.status.processCheckComplete,
    },
    verdict: answer.verdict,
    reason: answer.reason,
  };
}
