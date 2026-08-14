/**
 * The decisions the delete dialog makes, kept out of the component so they can
 * be tested without a browser.
 *
 * Everything here is about one asymmetry: a worktree wrongly deleted is gone
 * for good, a worktree wrongly kept costs disk. So the rules err towards
 * refusing, and towards letting git refuse.
 */

import { worktreeLabel } from "../lib/api.ts";
import type {
  BranchOutcome,
  CommandFailure,
  CompletedRemoval,
  RemovalPlan,
  RemovalRequest,
  RemoveOptions,
  UnverifiedBranchOutcome,
  Worktree,
} from "../lib/api.ts";

/**
 * Every branch outcome core can send, as data rather than as a chain of `===`.
 *
 * An outcome that is not in here is not passed through as itself: the app has
 * no wording for it, and inventing one would be a claim. It is read as
 * `unknown`, which is the only honest thing to say about a state this build
 * cannot interpret.
 */
const BRANCH_OUTCOMES = [
  "notRequested",
  "deleted",
  "kept",
  "moved",
  "unknown",
  "rollbackFailed",
] as const satisfies readonly BranchOutcome[];

/** The choices that belong to one deletion, not to the app. */
export interface DeleteOptions {
  deleteBranch: boolean;
  forceBranch: boolean;
  useTrash: boolean;
  /**
   * Lift the lock on the worktrees that carry one.
   *
   * Its own answer, never folded into the acknowledgement about uncommitted
   * files. A lock is the only thing in a plan somebody set deliberately, and
   * it usually says why.
   */
  unlockLocked: boolean;
}

/**
 * What a freshly opened dialog starts from.
 *
 * The component stays mounted between openings, so anything not reset here
 * carries into the next target: ticking "also delete the branch" and "even if
 * unmerged" for a dirty worktree used to re-arm itself, fully checked, over a
 * clean one where nothing blocks the confirm button.
 *
 * `useTrash` resets too, even though it is the safe option. It is not a stored
 * preference — it would survive only as long as the window happens to stay
 * open, which is not something a user can see or rely on — and the dialog's
 * "reclaims about N" line is untrue while it is set. An option nobody can
 * predict is worse than one they retick.
 */
export function freshOptions(): DeleteOptions {
  return {
    deleteBranch: false,
    forceBranch: false,
    useTrash: false,
    unlockLocked: false,
  };
}

/**
 * What survives a re-plan.
 *
 * A re-plan happens because the worktrees are not what was described, so every
 * option that authorises destroying something names a thing that may no longer
 * be the same thing. "Also delete the branch feat/a" was ticked against the
 * selection as it was; if the re-plan comes back with a worktree that now has
 * feat/b checked out, that tick is authorisation for a branch nobody chose. The
 * unlock is dropped for the same reason, and hardest: a lock that now says
 * something else is a different instruction from the one that was agreed to.
 *
 * `useTrash` stays. It authorises nothing — it is the recoverable route, and
 * turning it off silently under someone who asked for recoverable is the one
 * change here that could cost them.
 */
export function replanOptions(previous: DeleteOptions): DeleteOptions {
  return {
    ...previous,
    deleteBranch: false,
    forceBranch: false,
    unlockLocked: false,
  };
}

/**
 * Whether these plans still describe what the user is looking at.
 *
 * Plans are fetched asynchronously and the dialog is reused across targets, so
 * a slow response for a closed dialog can land after a new one opened. The
 * generation token stops that race; this is the check at the point of
 * destruction, which is the last thing between any such bug and permanent data
 * loss and so is worth keeping even once the race is fixed.
 */
export function plansMatchSelection(
  plans: RemovalPlan[],
  selected: readonly string[],
): boolean {
  if (plans.length !== selected.length) return false;
  const paths = new Set(selected);
  return plans.every((plan) => paths.has(plan.path));
}

/**
 * How to remove one particular worktree.
 *
 * `force` is read off the plan it belongs to and never off the batch. Selecting
 * one dirty worktree alongside four clean ones used to force all five, which
 * threw away the one check yawm does not perform itself: `git worktree remove`
 * refusing a dirty directory is git re-deciding, at the moment of destruction,
 * what the plan decided earlier. A plan that has gone stale is exactly the case
 * where that second opinion matters.
 */
export function removalOptionsFor(
  plan: RemovalPlan,
  options: DeleteOptions,
): RemoveOptions {
  return {
    force: plan.requiresForce,
    deleteBranch: options.deleteBranch,
    // Never inherited from `force`. Deleting unmerged commits is a loss of its
    // own and gets asked about on its own.
    forceBranch: options.deleteBranch && options.forceBranch,
    useTrash: options.useTrash,
    // Read off the plan as well, so a selection where one worktree is locked
    // never carries an unlock to the others. Sending it for a worktree that
    // holds no lock authorises nothing, and core would have nothing to lift.
    unlock: plan.isLocked && options.unlockLocked,
  };
}

/**
 * The whole selection, as one request core can refuse as one.
 *
 * The dialog used to loop, calling `removeWorktree` once per plan. When the
 * second worktree had changed since planning, the first was already deleted —
 * and what the user saw was a refusal saying nothing had been deleted. This
 * hands core every plan together so the decision is taken over all of them
 * before any of them is touched.
 */
export function removalRequestsFor(
  plans: readonly RemovalPlan[],
  options: DeleteOptions,
): RemovalRequest[] {
  return plans.map((plan) => ({
    plan,
    options: removalOptionsFor(plan, options),
  }));
}

/** Whether a plan would destroy state that exists only in that worktree. */
export function destroysWork(plan: RemovalPlan): boolean {
  return (
    plan.dirtyTotal > 0 || plan.unpushedCommits > 0 || plan.envFiles.length > 0
  );
}

/**
 * Reads whatever a failed command threw as a failure this app understands.
 *
 * Tauri hands back whatever the command's error type serialised to, which is a
 * tagged object for our own failures and a bare string or an `Error` for
 * everything else — a panic, a plugin, the bridge itself. Anything unrecognised
 * becomes a plain failure, because a failure nobody can classify still has to
 * reach the user.
 */
export function asFailure(thrown: unknown): CommandFailure {
  if (typeof thrown === "object" && thrown !== null && "kind" in thrown) {
    const failure = thrown as Partial<CommandFailure> & { kind?: unknown };
    if (failure.kind === "planChanged") {
      const changed = thrown as Extract<CommandFailure, { kind: "planChanged" }>;
      return {
        kind: "planChanged",
        message: String(changed.message ?? ""),
        path: String(changed.path ?? ""),
        changes: Array.isArray(changed.changes) ? changed.changes.map(String) : [],
        stillPresent: Array.isArray(changed.stillPresent)
          ? changed.stillPresent.map(String)
          : [],
      };
    }
    if (failure.kind === "partial") {
      const partial = thrown as Extract<CommandFailure, { kind: "partial" }>;
      return {
        kind: "partial",
        message: String(partial.message ?? ""),
        // Read defensively, but never softened into an empty list by a shape
        // that did not parse: an unread completed removal is a worktree the
        // user is told still exists.
        completed: Array.isArray(partial.completed)
          ? partial.completed.map(readCompleted)
          : [],
        // Gone, but not by yawm's hand. Kept apart from `completed` so the
        // dialog stops listing them without claiming yawm removed them.
        vanished: Array.isArray(partial.vanished)
          ? partial.vanished.map(String)
          : [],
        failed: String(partial.failed ?? ""),
      };
    }
    if (failure.kind === "vanished") {
      const gone = thrown as Extract<CommandFailure, { kind: "vanished" }>;
      return {
        kind: "vanished",
        message: String(gone.message ?? ""),
        vanished: Array.isArray(gone.vanished) ? gone.vanished.map(String) : [],
        failed: String(gone.failed ?? ""),
      };
    }
    if (failure.kind === "failed") {
      return { kind: "failed", message: String((thrown as { message?: unknown }).message ?? "") };
    }
  }
  return { kind: "failed", message: String(thrown) };
}

function readCompleted(entry: unknown): CompletedRemoval {
  const done = entry as {
    path?: unknown;
    outcome?: { branch?: unknown };
    status?: unknown;
  };
  const branch = done?.outcome?.branch;
  return {
    path: String(done?.path ?? ""),
    outcome: {
      /*
       * `unknown` and `rollbackFailed` are read as themselves. Collapsing them
       * into `notRequested` — as this did — told the user their branch was
       * untouched, when in fact core either never established what happened to
       * it or deleted it and failed to put it back.
       *
       * An absent outcome is the only thing that still reads as `notRequested`:
       * a payload with no branch field describes a removal where no branch was
       * ever in play. Anything present but unrecognised is `unknown`, because
       * something happened and this build cannot say what.
       */
      branch:
        branch === undefined || branch === null
          ? "notRequested"
          : (BRANCH_OUTCOMES as readonly unknown[]).includes(branch)
            ? (branch as BranchOutcome)
            : "unknown",
    },
    /*
     * A worktree core found gone while reconciling a failure, rather than one
     * a step reported removing, arrives as `removedButFinalizationFailed`.
     * Either way it is gone, so it belongs in `removed` — the distinction is
     * about what did *not* happen afterwards, which core's own message states.
     *
     * An older payload has no `status` at all, and the honest reading of a
     * bare `{path, outcome}` is that the removal ran to the end.
     */
    status:
      done?.status === "removedButFinalizationFailed"
        ? "removedButFinalizationFailed"
        : "removed",
  };
}

/**
 * Whether a failed removal is core saying "look again" rather than "it broke".
 *
 * Core re-plans immediately before it deletes anything and refuses when the
 * worktree is no longer what was approved — an agent writing into the
 * directory while the dialog is open is the normal case for this app, not an
 * edge case. Nothing was deleted, so the right response is to fetch the plans
 * again and let the user confirm what is actually there now, not to show a
 * failure.
 *
 * The refusal carries its own kind across the boundary. Recognising it by the
 * sentence inside it meant a worktree whose branch was called
 * "changed since it was checked", or a future rewording, decided whether the
 * user was asked again or shown an error.
 */
export function isPlanChanged(
  failure: CommandFailure,
): failure is Extract<CommandFailure, { kind: "planChanged" }> {
  return failure.kind === "planChanged";
}

/**
 * Whether these two options can be held at once.
 *
 * Trash is offered because it can be undone, and deleting the branch removes
 * what undoing it is for: the folder returns as a directory git does not know
 * is a worktree, on a branch that no longer exists. The core refuses the pair,
 * so the dialog has to make it unreachable rather than merely discouraged.
 */
export function optionsConflict(options: DeleteOptions): boolean {
  return options.useTrash && options.deleteBranch;
}

/** The selected worktrees somebody has locked, in the order they appear. */
export function lockedPlans(plans: readonly RemovalPlan[]): RemovalPlan[] {
  return plans.filter((plan) => plan.isLocked);
}

/**
 * Whether Delete must stay out of reach.
 *
 * Two separate gates, because they are two separate questions. Uncommitted
 * work is a consequence the user is being warned about; a lock is an
 * instruction somebody left, usually saying why. Answering the first has never
 * been an answer to the second — and while `--force --force` carried the lock,
 * ticking "I understand, delete it anyway" over a list of edited files silently
 * deleted a worktree locked with "agent running".
 */
export function confirmBlocked(
  plans: readonly RemovalPlan[] | null,
  acknowledged: boolean,
  options: DeleteOptions,
): boolean {
  // Nothing was ever established, so there is nothing to confirm.
  if (plans === null) return true;
  /*
   * Nothing is left to delete.
   *
   * Reached when every worktree in the selection turned out to be gone
   * already — removed from outside yawm while the dialog was open. An enabled
   * Delete over an empty plan list sends an empty batch, which core carries
   * out successfully, and the dialog then reports a deletion that deleted
   * nothing.
   */
  if (plans.length === 0) return true;
  if (plans.some(destroysWork) && !acknowledged) return true;
  if (lockedPlans(plans).length > 0 && !options.unlockLocked) return true;
  return false;
}

/**
 * The branches in a selection — only the ones that exist.
 *
 * Takes anything that names a branch, so the dialog can read it off the plans
 * core just returned rather than off the worktree list it was handed when it
 * opened. Those two disagree exactly when it matters: after a re-plan, the
 * checkbox saying "also delete feat/a" must name the branch the plan being
 * confirmed would actually delete.
 *
 * A detached worktree has no branch, so it contributes nothing here and must
 * not be counted.
 */
export function branchNamesOf(
  targets: readonly { branch: string | null }[],
): string[] {
  return targets.map((t) => t.branch).filter((b): b is string => Boolean(b));
}

/**
 * Whether the dialog has a branch to offer deleting at all.
 *
 * Selecting a single detached worktree still offered "Also delete the branch",
 * a checkbox that named nothing and, ticked, would have done nothing. An option
 * that cannot apply is worse than a missing one: it invites the user to believe
 * something is being cleaned up that never existed.
 */
export function offersBranchDeletion(
  targets: readonly { branch: string | null }[],
): boolean {
  return branchNamesOf(targets).length > 0;
}

/**
 * What the dialog is about to delete, in the words the rest of the app uses.
 *
 * A detached worktree used to be titled "Delete worktree?", which is every
 * worktree — the one screen that asks for an irreversible confirmation said
 * the least about what it would take. `worktreeLabel` is what the list and the
 * detail panel already call it, so what the user clicked and what they are now
 * being asked about read the same.
 */
export function deleteTitle(worktrees: readonly Worktree[]): string {
  /*
   * The selection can empty out while the dialog is open: everything in it
   * turns out to have been removed from outside yawm. "Delete 0 worktrees?"
   * asks a question with no answer, on the screen that is meant to state
   * exactly what is about to be destroyed.
   */
  if (worktrees.length === 0) return "Nothing left to delete";
  if (worktrees.length === 1 && worktrees[0]) {
    return `Delete ${worktreeLabel(worktrees[0])}?`;
  }
  return `Delete ${worktrees.length} worktrees?`;
}

/**
 * Which paths a re-plan may ask about after a refusal.
 *
 * Core builds a plan by looking a path up in the repository, and refuses a path
 * it cannot find. Asking it about a worktree that has since gone turns a
 * refusal the user could act on — "these changed, look again" — into a parse
 * error about a path, which reads like a bug and hides what actually happened.
 *
 * `live` is the list core sent with the refusal, read from the same snapshot
 * the refusal was decided on. Reading it off the app's own worktree list
 * instead was a guess: that list is repainted by a background scan and is
 * whatever it was when the dialog opened, so a worktree deleted from outside
 * yawm was still offered for a re-plan.
 */
export function replanTargets(
  selected: readonly string[],
  live: readonly string[],
): string[] {
  const present = new Set(live);
  return selected.filter((path) => present.has(path));
}

/**
 * The selected worktrees the repository no longer has, at the moment of the
 * refusal.
 *
 * The complement of [`replanTargets`], and needed separately because these
 * paths have to leave the dialog's own selection. Left in it, the next confirm
 * validates its plans against a selection that still names them —
 * [`plansMatchSelection`] fails, the dialog re-plans, core answers with the
 * same `stillPresent`, and the user is stuck in a loop asking about a worktree
 * that is not there.
 *
 * They are not deletions this app performed. Something else removed them, and
 * saying otherwise would be a claim yawm cannot make.
 */
export function vanishedFrom(
  selected: readonly string[],
  live: readonly string[],
): string[] {
  const present = new Set(live);
  return selected.filter((path) => !present.has(path));
}

/**
 * The plans still worth showing after part of a batch was removed.
 *
 * Removal cannot be undone, so a batch that failed half-way leaves the dialog
 * holding plans for worktrees that no longer exist. Leaving them on screen is
 * the failure this whole path exists to prevent, in a quieter form: the user
 * reads a list of what would be deleted and some of it already has been.
 */
export function remainingPlans(
  plans: readonly RemovalPlan[],
  removed: readonly string[],
): RemovalPlan[] {
  const gone = new Set(removed);
  return plans.filter((plan) => !gone.has(plan.path));
}

/**
 * The branches git declined to delete, named.
 *
 * Refusing is the right outcome — the worktree goes, the commits stay
 * reachable — but the user ticked a box and it did not happen, and nothing
 * else in the app can see that. Matched by path rather than by position,
 * because a batch that failed part-way reports only what it completed.
 */
export function keptBranchesOf(
  plans: readonly RemovalPlan[],
  results: readonly { path: string; outcome: { branch: BranchOutcome } }[],
): string[] {
  const branches = new Map(plans.map((plan) => [plan.path, plan.branch]));
  return results.flatMap((result) => {
    const branch = branches.get(result.path);
    // `moved` is core refusing because the branch is no longer the commit the
    // user approved deleting. Like `kept`, the box was ticked and nothing
    // happened, and the user hears about it either way.
    const notDeleted =
      result.outcome.branch === "kept" || result.outcome.branch === "moved";
    return notDeleted && branch ? [branch] : [];
  });
}

/** A branch whose real state the removal never established, and why. */
export interface UnverifiedBranch {
  branch: string;
  outcome: UnverifiedBranchOutcome;
}

/**
 * The branches nobody can say the state of, named with what went wrong.
 *
 * Separate from `keptBranchesOf` because they are the opposite kind of news.
 * "Kept" is a refusal that worked: the branch is where it was and the commits
 * are safe. These two are the absence of an answer — `unknown` is finalisation
 * breaking before the ref state was established, and `rollbackFailed` is a
 * branch that was deleted and could not be restored. Reported through the kept
 * list they would have read as reassurance; not reported at all, the user is
 * left believing a branch is there that may well not be.
 *
 * Matched by path, like the kept branches, because a batch that failed
 * part-way answers only about what it completed.
 */
export function unverifiedBranchesOf(
  plans: readonly RemovalPlan[],
  results: readonly { path: string; outcome: { branch: BranchOutcome } }[],
): UnverifiedBranch[] {
  const branches = new Map(plans.map((plan) => [plan.path, plan.branch]));
  return results.flatMap((result) => {
    const branch = branches.get(result.path);
    const outcome = result.outcome.branch;
    const unverified = outcome === "unknown" || outcome === "rollbackFailed";
    return unverified && branch ? [{ branch, outcome }] : [];
  });
}

/**
 * Every unverified branch this dialog has heard of, across its attempts.
 *
 * Accumulated for the same reason as the kept ones: a dialog reports once per
 * attempt, and an unverified branch from the first attempt is still unverified
 * after the second. A branch already heard of is not repeated, and the first
 * thing said about it stands — a later attempt cannot have touched a worktree
 * that is already gone, so a second reading would be about a different, later
 * event that this dialog did not cause.
 */
export function mergeUnverifiedBranches(
  known: readonly UnverifiedBranch[],
  reported: readonly UnverifiedBranch[],
): UnverifiedBranch[] {
  const merged = [...known];
  for (const entry of reported) {
    if (!merged.some((seen) => seen.branch === entry.branch)) {
      merged.push(entry);
    }
  }
  return merged;
}

/**
 * Every branch this dialog has been told was kept, across all of its attempts.
 *
 * One dialog can report more than once: a batch fails part-way, the user
 * retries what is left, and each attempt answers only about the worktrees it
 * touched. Handing the app the latest answer replaced the earlier one, so a
 * branch git refused to delete in the first attempt stopped being mentioned
 * the moment a second attempt finished — the notice about it disappeared while
 * the branch was still there.
 *
 * Order is the order they were first heard, and a branch already known is not
 * repeated: the same worktree can be reported by a partial failure and again
 * by the retry that follows it.
 */
export function mergeKeptBranches(
  known: readonly string[],
  reported: readonly string[],
): string[] {
  const merged = [...known];
  for (const branch of reported) {
    if (!merged.includes(branch)) merged.push(branch);
  }
  return merged;
}

/**
 * What a finished deletion did, as the app outside the dialog needs it.
 *
 * `removed` is the whole point: those worktrees' diff tabs show a patch that
 * can no longer be fetched, and the list still has rows for them. A batch that
 * failed half-way reports the part that succeeded here and keeps the rest, so
 * this is never "the selection" — it is what is actually gone.
 *
 * `vanished` is the same fact with a different author. Those worktrees are gone
 * too, and their tabs are just as stale, but yawm did not remove them —
 * something outside it did, while the dialog was open. Kept apart from
 * `removed` so nothing downstream can report them as deletions this app
 * carried out.
 *
 * `keptBranches` is cumulative over the whole dialog, not per attempt: a branch
 * git refused to delete in the first attempt is still undeleted after the
 * second.
 *
 * `unverifiedBranches` is cumulative for the same reason, and deliberately not
 * part of `keptBranches`: those branches were not kept, and were not left
 * alone. Core either never established what became of them or deleted them and
 * could not roll that back, and only the user looking in git can settle it.
 */
export interface DeletionResult {
  removed: string[];
  vanished: string[];
  keptBranches: string[];
  unverifiedBranches: UnverifiedBranch[];
}
