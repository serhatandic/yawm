/**
 * A finite ceiling on every call into Rust.
 *
 * Nothing here is about latency. An `invoke` whose reply is dropped — the
 * callback destroyed, the channel gone — never settles at all, and a promise
 * that never settles cannot be caught. `try`/`catch`/`finally` around it are
 * all correct and all dead code: the `catch` never runs, the `finally` never
 * runs, and whatever pending flag the caller set stays set for the life of the
 * window. Every await in this app is exposed to that, not just the scan, so the
 * ceiling belongs at the one place they all pass through rather than at each
 * screen that happens to have been noticed.
 *
 * A deadline is the only thing that converts "never" into "eventually". It
 * cannot stop the work — Rust carries on regardless — it can only stop the UI
 * from waiting on an answer that is not coming.
 *
 * No Tauri import on purpose, so the whole mechanism is testable under node.
 */

/**
 * Deliberately duck-typed rather than `instanceof`.
 *
 * A bundler that splits this module across chunks, or a dev reload that
 * re-evaluates it, produces a second class object whose instances fail
 * `instanceof` against the first. Silently treating a timeout as an ordinary
 * failure is exactly the confusion this exists to remove, so the check is on a
 * property that survives being duplicated.
 */
export class TimeoutError extends Error {
  readonly timedOut = true;
  readonly command: string;
  readonly deadlineMs: number;

  constructor(command: string, deadlineMs: number) {
    super(`${command} did not answer within ${Math.round(deadlineMs / 1000)}s`);
    this.name = "TimeoutError";
    this.command = command;
    this.deadlineMs = deadlineMs;
  }
}

export function isTimeout(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    (error as { timedOut?: unknown }).timedOut === true
  );
}

/**
 * The tiers, in milliseconds, each anchored to something that was measured.
 *
 * Every one is a large multiple of the real thing, because the two mistakes
 * are not symmetrical. Too long costs a user who has already hit a hang some
 * extra waiting. Too short breaks a working app for whoever has the most
 * worktrees — the exact person this product is for — and does it by telling
 * them something untrue, which is the bug, not the fix.
 */

/**
 * Settings reads and writes, and single git queries. Milliseconds in practice;
 * the allowance is for a source that lives on a stalled network mount.
 */
export const QUICK_MS = 30_000;

/**
 * One worktree's analysis. Landing analysis measures 3–5s cold, so this is
 * thirty to sixty times the observed cost.
 */
export const INSPECT_MS = 180_000;

/**
 * One tree walked from disk. Sized from the whole-machine scan below, since a
 * single repository cannot exceed all of them.
 */
export const WALK_MS = 300_000;

/**
 * Every repository, sizes skipped. Measured at 2.6s for 21 worktrees, so this
 * covers roughly a thousand of them before it would fire.
 */
export const SCAN_FAST_MS = 120_000;

/**
 * Every repository, every byte. Measured at 16s release and 30s debug for 21
 * worktrees over 22.6 GB — about 1.4s per worktree — so this covers something
 * like four hundred worktrees in a debug build and twice that in a release
 * one. Beyond that a user is better served by an honest "give up and retry"
 * than by a spinner, but the bar is set where no healthy machine reaches it.
 */
export const SCAN_FULL_MS = 600_000;

/**
 * The calls that get no deadline, and why.
 *
 * Both change the filesystem in ways that cannot be undone or repeated safely,
 * and a deadline here would not cancel anything: Rust keeps deleting or keeps
 * copying while the UI announces that it stopped. The user then retries a
 * creation that is already half-made, or is told a removal failed when the
 * directory is in fact going away — believing work survived that did not,
 * which is the single worst thing this app can say. Deleting 20 GB to the
 * Trash on a slow disk legitimately takes minutes, so any bound short enough
 * to be useful would fire on healthy work.
 *
 * A dialog stuck on "Removing…" is a bad outcome. A confident wrong answer
 * about destroyed work is a worse one.
 *
 * `remove_worktrees` is the batch form and is what the delete dialog actually
 * calls; it was missing here while its single-worktree sibling was listed, so
 * deleting five worktrees — the case that takes longest — was the one holding a
 * deadline. Timing it out reports a failure over a batch that is still deleting
 * and whose partial result the dialog then never reconciles.
 */
const UNBOUNDED = new Set([
  "remove_worktree",
  "remove_worktrees",
  "create_worktree",
]);

const TIERS: Record<string, number> = {
  scan_repo: WALK_MS,
  plan_creation: WALK_MS,

  inspect_worktree: INSPECT_MS,
  resolve_landing: INSPECT_MS,
  plan_removals: INSPECT_MS,
  diff_worktree: INSPECT_MS,
  focused_worktree: INSPECT_MS,
  prune_repo: INSPECT_MS,
};

/**
 * How long a command may take before the UI stops believing in it. `null` means
 * it is deliberately unbounded.
 *
 * The arguments are read because `scan_all` is two very different jobs behind
 * one name: the fast pass skips the disk entirely and is what a workspace
 * switch waits on, so holding it to the full pass's ceiling would leave the
 * switch looking stuck for ten minutes.
 */
export function deadlineFor(
  command: string,
  args?: Record<string, unknown>,
): number | null {
  if (UNBOUNDED.has(command)) return null;
  if (command === "scan_all") {
    return args?.full === true ? SCAN_FULL_MS : SCAN_FAST_MS;
  }
  return TIERS[command] ?? QUICK_MS;
}

/**
 * Reject if the work has not settled in time.
 *
 * The rejection handler stays attached past the deadline so that a late
 * failure is still consumed; without it a call that times out and then rejects
 * would surface as an unhandled rejection long after the UI moved on.
 */
export function withDeadline<T>(
  work: Promise<T>,
  command: string,
  deadlineMs: number | null,
): Promise<T> {
  if (deadlineMs === null) return work;

  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new TimeoutError(command, deadlineMs)),
      deadlineMs,
    );

    work.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}
