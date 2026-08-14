/**
 * Rebasing a settings edit onto settings that moved underneath it.
 *
 * The Settings tab reads once and stays mounted while hidden, so its copy goes
 * stale the moment a repository is added anywhere else in the app. The backend
 * refuses a write made against an old revision rather than letting it win; this
 * is how the refusal is recovered from without making the user retype anything.
 *
 * The rule throughout: take from the edit only what the user actually changed,
 * and take everything else from what is really stored. A field the user did not
 * touch has no opinion in it, so it must not overwrite a newer value.
 */

import type { Config, Workspace } from "@/lib/api";

function changed(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) !== JSON.stringify(b);
}

/**
 * Merge one object's deliberate changes onto a newer version of it.
 *
 * Keys present only in `fresh` survive untouched, which is what carries
 * settings this build does not know about across an edit.
 */
function rebaseFields<T extends object>(base: T, edited: T, fresh: T): T {
  const result = { ...fresh } as Record<string, unknown>;
  const source = edited as Record<string, unknown>;
  const original = base as Record<string, unknown>;

  for (const key of Object.keys(source)) {
    if (changed(source[key], original[key])) {
      result[key] = source[key];
    }
  }
  return result as T;
}

/**
 * Workspaces, matched by id rather than by position.
 *
 * Position is not stable: a group added or deleted elsewhere shifts everything
 * after it, and merging by index would then write one group's name onto
 * another. A group the user edited that no longer exists is dropped — it was
 * deleted while they were editing it, and recreating it would undo that.
 */
function rebaseWorkspaces(
  base: Workspace[],
  edited: Workspace[],
  fresh: Workspace[],
): Workspace[] {
  const before = new Map(base.map((w) => [w.id, w]));
  const after = new Map(edited.map((w) => [w.id, w]));

  const merged = fresh.map((current) => {
    const mine = after.get(current.id);
    if (!mine) return current;
    return rebaseFields(before.get(current.id) ?? current, mine, current);
  });

  // Groups the user created in this sitting are not in `fresh` yet.
  const known = new Set(fresh.map((w) => w.id));
  for (const added of edited) {
    if (!known.has(added.id) && !before.has(added.id)) merged.push(added);
  }
  return merged;
}

/**
 * The settings to retry a refused save with.
 *
 * `base` is what the tab loaded, `edited` is what the user made of it, `fresh`
 * is what is actually stored now.
 */
export function rebaseConfig(base: Config, edited: Config, fresh: Config): Config {
  const merged = rebaseFields(base, edited, fresh);
  return {
    ...merged,
    workspaces: rebaseWorkspaces(base.workspaces, edited.workspaces, fresh.workspaces),
  };
}
