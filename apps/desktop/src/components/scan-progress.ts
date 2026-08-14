/**
 * How a scan ends, from the list's point of view.
 *
 * Every pass had exactly one ending before this: it resolved, eventually. The
 * deadline in `lib/deadline.ts` is what makes "eventually" true, and this is
 * what the screen does with the three answers that are now possible. The
 * distinction that matters here is not success versus failure but *whose*
 * failure it is: a scan the user has already navigated away from must stay
 * silent, or switching workspaces would raise errors about a list nobody is
 * looking at.
 *
 * Kept out of the component so it can be tested without a browser.
 */

import { isTimeout } from "../lib/deadline.ts";

export type ScanPass = "listing" | "measuring" | "landing";

export type ScanSettlement<T> =
  | { state: "done"; value: T }
  /** Superseded scans are not failures and must raise nothing. */
  | { state: "superseded" }
  | { state: "failed"; reason: string; timedOut: boolean };

/**
 * Wait for a scan, and care about the outcome only if it is still the one on
 * screen.
 *
 * The supersession check happens at the moment of settling rather than when the
 * work started: a scan the user moved on from may fail, time out, or succeed,
 * and in every case the answer is the same — it is not theirs any more, so it
 * says nothing.
 */
export async function settleScan<T>(
  work: Promise<T>,
  options: { isCurrent: () => boolean },
): Promise<ScanSettlement<T>> {
  const { isCurrent } = options;

  try {
    const value = await work;
    return isCurrent() ? { state: "done", value } : { state: "superseded" };
  } catch (error) {
    if (!isCurrent()) return { state: "superseded" };
    return {
      state: "failed",
      reason: reasonOf(error),
      // Carried through rather than re-derived, because "this is taking longer
      // than expected" and "this broke" need different sentences, and only one
      // of them is worth retrying immediately.
      timedOut: isTimeout(error),
    };
  }
}

function reasonOf(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}
