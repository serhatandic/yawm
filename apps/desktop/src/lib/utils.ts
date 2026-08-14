import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** Merge class names, letting later Tailwind utilities win over earlier ones. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * The keyboard's version of hover.
 *
 * shadcn's own controls carry a ring; the app's hand-rolled ones — the tabs,
 * the filter chips, the sidebar rows, the notice dismissals — carried nothing,
 * so tabbing through the window left no way to tell where you were. One string
 * rather than a class per component, because a focus ring that differs between
 * two adjacent controls reads as a rendering fault.
 *
 * Drawn *inside* the control's own box. Several of these sit in panes with
 * `overflow-hidden`, and an outset ring on the first or last row of one of
 * those is clipped away exactly where it is most needed.
 *
 * No `outline-none` beside it: in Tailwind v4 that sets `--tw-outline-style`
 * to `none`, which the width utility then reads back — so pairing the two
 * produces a two-pixel outline of style none, which is no outline at all.
 */
export const FOCUS_RING =
  "focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring";

/** Bytes in the largest unit that keeps the number readable. */
export function humanBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return "—";
  if (bytes === 0) return "0 B";

  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  if (unit === 0) return `${bytes} B`;
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

/** Compact age, e.g. `4m`, `6d`. */
export function relativeTime(unixSeconds: number | null | undefined): string {
  if (unixSeconds === null || unixSeconds === undefined) return "—";

  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - unixSeconds);
  const MINUTE = 60;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;
  const MONTH = 30 * DAY;
  const YEAR = 365 * DAY;

  if (seconds < MINUTE) return "now";
  if (seconds < HOUR) return `${Math.floor(seconds / MINUTE)}m`;
  if (seconds < DAY) return `${Math.floor(seconds / HOUR)}h`;
  if (seconds < MONTH) return `${Math.floor(seconds / DAY)}d`;
  if (seconds < YEAR) return `${Math.floor(seconds / MONTH)}mo`;
  return `${Math.floor(seconds / YEAR)}y`;
}
