import { useCallback, useState } from "react";
import type { RepoReport, Worktree } from "@/lib/api";

/**
 * Which reading of one set of changes the tab was opened for.
 *
 * Not a scope. There is one payload — everything this worktree is holding —
 * and two ways to read it: `everything` draws it whole, `atRisk` narrows the
 * committed half to the lines that exist nowhere else. Asking for a reading
 * never asks for a different fetch, which is what made the old scope switch a
 * refetch and a skeleton flash for data that was already on screen.
 */
export type ReadingIntent = "atRisk" | "everything";

/**
 * Where the view should land, when the click was about one group.
 *
 * The dirty count on a row promises work that exists only on disk, so it
 * scrolls there. It does not hide the rest: the committed group is still the
 * other half of the same decision, and a click asking "what is at risk here"
 * answered by hiding two thirds of the answer is how the old scope switch came
 * to exist.
 */
export type ChangesAnchor = "uncommitted" | null;

/**
 * Tabs.
 *
 * The rule that decides what belongs here:
 *
 *   A tab is a place you work. A dialog is a decision you make and dismiss.
 *
 * If a surface holds state worth returning to — a scroll position, a filter, a
 * half-read diff — it is a tab. If it resolves to a single yes/no and you would
 * never want two of them, it stays a dialog.
 *
 * The worktree list is deliberately **not** a tab. It is the root the app opens
 * on and the place everything else is opened from, so it gets a home button
 * rather than a peer tab that can never be closed. Tabs hold only what you
 * chose to open, and closing the last one returns you home.
 */

export type Tab =
  | {
      kind: "diff";
      key: string;
      path: string;
      repoRoot: string;
      title: string;
      subtitle: string;
      /** Which reading the click asked for. Re-asking overrides the current one. */
      intent: ReadingIntent;
      /** The group the click was about, if it was about one. */
      anchor: ChangesAnchor;
      /**
       * Bumped on every open, including opens that land on a tab already
       * showing. The mounted view watches it, so asking the same question
       * twice is idempotent while asking a *different* one is not ignored —
       * and it re-aims an open tab without discarding what it has read.
       */
      request: number;
    }
  | { kind: "settings"; key: "settings" };

/** Home has no tab, so `null` is the active key when the list is showing. */
export type ActiveKey = string | null;

let requests = 0;

/** Stable identity per surface, so opening the same thing twice cannot duplicate it. */
export function diffTabFor(
  repo: RepoReport,
  worktree: Worktree,
  open: { intent?: ReadingIntent; anchor?: ChangesAnchor } = {},
): Tab {
  requests += 1;
  return {
    kind: "diff",
    key: `diff:${worktree.path}`,
    path: worktree.path,
    repoRoot: repo.root,
    title: worktree.branch ?? "detached",
    subtitle: repo.name,
    /*
     * The general Changes button asks for the narrower reading, because the
     * question it answers is "what would deleting this lose". The view falls
     * back to the complete reading by itself when there is no genuine
     * narrowing to show, so this is an intent rather than an instruction.
     */
    intent: open.intent ?? "atRisk",
    anchor: open.anchor ?? null,
    request: requests,
  };
}

/**
 * Place a tab, replacing an open one with the same identity.
 *
 * Keeping the *existing* tab was the bug: the second click carried a new
 * question and the tab silently answered the first one again.
 */
export function placeTab(current: Tab[], tab: Tab): Tab[] {
  const index = current.findIndex((t) => t.key === tab.key);
  if (index === -1) return [...current, tab];
  const next = [...current];
  next[index] = tab;
  return next;
}

export const SETTINGS_TAB: Tab = { kind: "settings", key: "settings" };

export function useTabs() {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeKey, setActiveKey] = useState<ActiveKey>(null);

  /** Open a tab, or focus it when it is already open. */
  const open = useCallback((tab: Tab) => {
    setTabs((current) => placeTab(current, tab));
    setActiveKey(tab.key);
  }, []);

  const goHome = useCallback(() => setActiveKey(null), []);

  const close = useCallback((key: string) => {
    setTabs((current) => {
      const index = current.findIndex((t) => t.key === key);
      if (index === -1) return current;

      const next = current.filter((t) => t.key !== key);
      setActiveKey((active) => {
        if (active !== key) return active;
        // Activate the left neighbour, the way editors do, so closing a run of
        // tabs walks backwards. Falling off the start returns home.
        return next[index - 1]?.key ?? next[0]?.key ?? null;
      });
      return next;
    });
  }, []);

  const closeActive = useCallback(() => {
    if (activeKey !== null) close(activeKey);
  }, [close, activeKey]);

  /**
   * Jump by position for the number shortcuts. Index 0 is home, so the tabs
   * themselves start at 1 — matching how they read left to right.
   */
  const activateIndex = useCallback((index: number) => {
    if (index === 0) {
      setActiveKey(null);
      return;
    }
    setTabs((current) => {
      const target = current[index - 1];
      if (target) setActiveKey(target.key);
      return current;
    });
  }, []);

  /** Cycle through home and every open tab as one ring. */
  const cycle = useCallback((delta: number) => {
    setTabs((current) => {
      setActiveKey((active) => {
        const ring: ActiveKey[] = [null, ...current.map((t) => t.key)];
        const index = ring.indexOf(active);
        if (index === -1) return active;
        const next = (index + delta + ring.length) % ring.length;
        return ring[next] ?? null;
      });
      return current;
    });
  }, []);

  return {
    tabs,
    activeKey,
    active: tabs.find((t) => t.key === activeKey) ?? null,
    isHome: activeKey === null,
    open,
    goHome,
    close,
    closeActive,
    activate: setActiveKey,
    activateIndex,
    cycle,
  };
}
