/**
 * The lines the app says when something did not go the way the list implies.
 *
 * The list can only show what was read. A source that could not be read looks
 * exactly like a source with nothing in it, and settings that could not be
 * parsed look exactly like settings nobody has written yet — in both cases the
 * reassuring reading is the wrong one. These turn those silences into a
 * sentence.
 *
 * Kept out of the components so they can be tested without a browser.
 */

import type { ConfigStatus, UnreadableSource } from "@/lib/api";
import type { UnverifiedBranch } from "./delete-rules.ts";
import type { ScanPass } from "./scan-progress.ts";

export type NoticeTone = "warning" | "info";

/** A pass that ended in failure rather than in numbers. */
export interface ScanFailure {
  pass: ScanPass;
  reason: string;
  timedOut: boolean;
}

export interface Notice {
  /**
   * Derived from what the notice says, never from where it came from, so
   * dismissing one problem cannot hide the next one that arrives.
   */
  id: string;
  tone: NoticeTone;
  text: string;
  /**
   * Offered when the condition is one the user can do something about, so a
   * dead end becomes a retry without restarting the app.
   */
  action?: { label: string };
}

/** The last path segment, for naming a source without a wall of directories. */
function shortName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function list(names: string[]): string {
  if (names.length <= 2) return names.join(" and ");
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/**
 * What could not be read on the last scan.
 *
 * Names the sources and keeps git's own words for why, because "offline" and
 * "gone" are the same shorter list otherwise, and only the reason tells an
 * unmounted drive apart from a directory someone deleted.
 */
export function scanNotice(unreadable: UnreadableSource[]): Notice | null {
  if (unreadable.length === 0) return null;

  const names = unreadable.map((source) => shortName(source.path));
  const reasons = [...new Set(unreadable.map((source) => source.reason))];
  const why = reasons.length === 1 ? reasons[0] : "see the paths above";
  const subject =
    unreadable.length === 1
      ? `${names[0]} could not be read`
      : `${unreadable.length} sources could not be read (${list(names)})`;

  return {
    id: `scan:${unreadable.map((s) => `${s.path}|${s.reason}`).join(";")}`,
    tone: "warning",
    text: `${subject}: ${why}. Anything inside is missing from this list, not gone.`,
  };
}

/**
 * What happened to the settings file at startup.
 *
 * Only `unusable` is worth saying. A missing file is the normal first run, and
 * a loaded one is the normal every other run; neither is news. An unusable one
 * means every repository on screen is a default rather than a choice, which the
 * user has no other way of finding out.
 */
export function configNotice(status: ConfigStatus): Notice | null {
  if (status.state !== "unusable") return null;

  const kept = status.backup
    ? `Your original is kept at ${status.backup}.`
    : "Your original has been left exactly as it is.";

  return {
    id: `config:${status.reason}`,
    tone: "warning",
    text: `Settings could not be read, so yawm started on defaults — ${status.reason} ${kept} Nothing here has been written back over it.`,
  };
}

/**
 * Branches git declined to delete.
 *
 * Reported as the good news it is. The user asked for the branch to go, it did
 * not, and the reason is that it still holds commits that exist nowhere else —
 * so the worktree was reclaimed and the work survived. Saying nothing leaves
 * them believing the commits went with the directory.
 */
export function keptBranchNotice(branches: string[]): Notice | null {
  if (branches.length === 0) return null;

  const subject =
    branches.length === 1
      ? `Branch ${branches[0]} was kept`
      : `${branches.length} branches were kept (${list(branches)})`;

  return {
    id: `kept:${branches.join(";")}`,
    tone: "info",
    text: `${subject}: the worktree is gone and the branch still holds commits that are not merged anywhere. Nothing was lost — delete the branch in git if you are sure.`,
  };
}

/**
 * Branches whose real state the removal never established.
 *
 * The opposite of the kept notice, and it must never be mistaken for it. A
 * kept branch is reassurance: it is exactly where it was. These are not. Core
 * either broke before it could establish what happened to the ref
 * (`unknown`), or deleted the branch and then failed to put it back
 * (`rollbackFailed`) — in which case the branch may not exist at all. Both are
 * warnings, both name the branch, and both end with the only action that can
 * settle it: look in git.
 */
export function unverifiedBranchNotice(
  branches: UnverifiedBranch[],
): Notice | null {
  if (branches.length === 0) return null;

  const rolledBack = branches.filter((b) => b.outcome === "rollbackFailed");
  const unverified = branches.filter((b) => b.outcome === "unknown");

  const sentences: string[] = [];
  if (rolledBack.length > 0) {
    const names = list(rolledBack.map((b) => b.branch));
    const subject = rolledBack.length === 1 ? `Branch ${names}` : `Branches ${names}`;
    sentences.push(
      `${subject}: the attempted rollback failed, so the branch may no longer exist and its state needs verifying in git.`,
    );
  }
  if (unverified.length > 0) {
    const names = list(unverified.map((b) => b.branch));
    const subject = unverified.length === 1 ? `Branch ${names}` : `Branches ${names}`;
    sentences.push(
      `${subject}: the branch state could not be verified, so yawm cannot say whether it still exists — check it in git.`,
    );
  }

  return {
    id: `unverified:${branches.map((b) => `${b.branch}|${b.outcome}`).join(";")}`,
    tone: "warning",
    text: sentences.join(" "),
  };
}

/**
 * A scan that ended without an answer.
 *
 * The state this replaces was a spinner that never stopped, which is the same
 * untruth as an empty list: the user is told the numbers are on their way when
 * in fact nothing further is coming. The wording is careful about what the
 * missing sizes mean, because a blank size column next to a reclaimable total
 * of 0 B reads as "nothing to reclaim" — the most expensive possible
 * misreading in an app whose whole job is telling you what is safe to delete.
 */
export function scanFailureNotice(failure: ScanFailure | null): Notice | null {
  if (failure === null) return null;

  // A timeout is not a failure and must not read like one: nothing is broken,
  // the answer just did not arrive, and retrying is a reasonable thing to do.
  const cause = failure.timedOut
    ? "is taking longer than expected, so yawm stopped waiting"
    : `failed — ${failure.reason}`;

  const text =
    failure.pass === "measuring"
      ? `Measuring sizes on disk ${cause}. The sizes already shown are real; the blank ones were never measured, so the totals below them are lower than what is actually on disk.`
      : failure.pass === "landing"
        ? `Checking rewritten history ${cause}. The statuses already resolved are real; rows still showing an unfinished status have not reached a safe-to-delete conclusion.`
        : `Looking for worktrees ${cause}. What is on screen is whatever was already known and may be out of date — treat it as incomplete, not as everything you have.`;

  return {
    id: `scan:${failure.pass}:${failure.timedOut ? "timeout" : failure.reason}`,
    tone: "warning",
    text,
    action: { label: "Try again" },
  };
}

/**
 * The notices worth drawing, in the order they should appear.
 *
 * Dismissal is by id, so a dismissed notice stays gone only while it says the
 * same thing: a new source failing, or a different parse error, produces a new
 * id and is shown.
 */
export function visibleNotices(
  notices: (Notice | null)[],
  dismissed: readonly string[],
): Notice[] {
  const hidden = new Set(dismissed);
  return notices.filter(
    (notice): notice is Notice => notice !== null && !hidden.has(notice.id),
  );
}
