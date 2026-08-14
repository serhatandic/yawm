import { useCallback, useEffect, useRef, useState } from "react";

import {
  DEFAULT_PANEL_WIDTH,
  DEFAULT_WIDTHS,
  FALLBACK_CHAR_WIDTH,
  clampPanelWidth,
  clampWidths,
  type ColumnWidths,
} from "@/lib/layout";

/**
 * Stored display metrics for the worktree list.
 *
 * The arithmetic lives in `layout.ts`, which is pure and tested. What is left
 * here is the part that needs React and the browser: persistence, and measuring
 * the pane and the font that the arithmetic runs on.
 *
 * Kept in localStorage rather than the app config: these are display metrics,
 * not preferences about worktrees. Someone on a laptop and the same person on
 * an external monitor want different numbers, and neither wants to wait for an
 * IPC round trip while dragging.
 */

export type ColumnWidthUpdate = Partial<ColumnWidths>;

export {
  CHECKBOX_WIDTH,
  COLUMN_GAP,
  DEFAULT_WIDTHS,
  MIN_WIDTH,
  NAME_TARGET,
  ROW_PADDING_X,
  applyResize,
  columnFloor,
  contentFloor,
  fitColumns,
  gridTemplate,
  resizeRange,
  textWidth,
  type ColumnLayout,
  type ColumnWidths,
  type ResizableColumn,
} from "@/lib/layout";

// Bumped whenever a default moves far enough that a stored width would fight
// it. v3's 320px Status was sized for prose; Status now carries a badge and
// tokens, and keeping the old number would hand the identifier's space to a
// column with nothing left to put in it.
const KEY = "yawm.columnWidths.v4";

function read(): ColumnWidths {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_WIDTHS;
    const parsed = JSON.parse(raw) as Partial<ColumnWidths>;
    // Merged over the defaults so a column added later still has a width.
    return clampWidths({ ...DEFAULT_WIDTHS, ...parsed }, uiCharWidth(13));
  } catch {
    return DEFAULT_WIDTHS;
  }
}

export function useColumnWidths() {
  const [widths, setWidths] = useState<ColumnWidths>(DEFAULT_WIDTHS);

  // Read after mount rather than in the initialiser, so the first paint does
  // not depend on storage being available.
  useEffect(() => setWidths(read()), []);

  const resize = useCallback((update: ColumnWidthUpdate) => {
    setWidths((current) => {
      const next = clampWidths({ ...current, ...update }, uiCharWidth(13));
      try {
        localStorage.setItem(KEY, JSON.stringify(next));
      } catch {
        // A full or disabled store is not worth interrupting a drag over.
      }
      return next;
    });
  }, []);

  const reset = useCallback(() => {
    setWidths(DEFAULT_WIDTHS);
    try {
      localStorage.removeItem(KEY);
    } catch {
      // As above.
    }
  }, []);

  return { widths, resize, reset };
}

/**
 * The width a row actually gets to lay out in, measured.
 *
 * The layout is computed in JavaScript rather than left to `1fr` because `1fr`
 * means "take what remains" — it will hand the identifier four characters as
 * readily as four hundred. Knowing the actual number is what lets the sized
 * columns yield instead.
 *
 * Which makes *what* is measured the whole game. The pane's own padding is read
 * from the computed style rather than assumed from its classes, so changing
 * that padding cannot silently push the last column past the clip edge; and the
 * result is floored, because a container 596.5px wide offers 596 whole pixels
 * and the half it does not offer comes off the rightmost column.
 *
 * The ref callback measures during commit, so the first painted frame is
 * already correct; the observer only keeps it correct afterwards.
 */
function innerWidth(node: HTMLElement): number {
  const rect = node.getBoundingClientRect();
  const style = getComputedStyle(node);
  const inset =
    (parseFloat(style.paddingLeft) || 0) +
    (parseFloat(style.paddingRight) || 0) +
    (parseFloat(style.borderLeftWidth) || 0) +
    (parseFloat(style.borderRightWidth) || 0);
  return Math.max(0, Math.floor(rect.width - inset));
}

export function usePaneWidth() {
  const [width, setWidth] = useState(0);
  const observer = useRef<ResizeObserver | null>(null);

  const ref = useCallback((node: HTMLElement | null) => {
    observer.current?.disconnect();
    observer.current = null;
    if (!node) return;

    setWidth(innerWidth(node));
    if (typeof ResizeObserver === "undefined") return;

    const ro = new ResizeObserver(() => setWidth(innerWidth(node)));
    ro.observe(node);
    observer.current = ro;
  }, []);

  useEffect(() => () => observer.current?.disconnect(), []);

  return { ref, width };
}

/**
 * Average character width of a font, measured once per font.
 *
 * Middle truncation needs a character budget, and a guessed budget is either
 * wasteful or — far worse — too long, at which point CSS finishes the job by
 * cutting the tail off. A canvas measurement of a representative sample costs
 * nothing and removes the guess.
 */
const SAMPLE = "abcdefghijklmnopqrstuvwxyz-/0123456789";
const measured = new Map<string, number>();

function charWidth(font: string): number {
  const cached = measured.get(font);
  if (cached !== undefined) return cached;

  let value = FALLBACK_CHAR_WIDTH;
  try {
    const context = document.createElement("canvas").getContext("2d");
    if (context) {
      context.font = font;
      const width = context.measureText(SAMPLE).width / SAMPLE.length;
      if (width > 0) value = width;
    }
  } catch {
    // No canvas is a fallback, not a failure.
  }

  measured.set(font, value);
  return value;
}

const FAMILY =
  'ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif';

export function uiCharWidth(size: number, weight: number = 400): number {
  return charWidth(`${weight} ${size}px ${FAMILY}`);
}

export function monoCharWidth(size: number): number {
  return charWidth(`${size}px ui-monospace, SFMono-Regular, Menlo, monospace`);
}

/**
 * Width of the detail panel.
 *
 * Same reasoning as the columns: a display metric, kept locally, and one the
 * user should be able to set. A branch name and a commit subject need more
 * room on a large monitor than the fixed 384px it shipped with, and less on a
 * laptop where the list matters more.
 */
const PANEL_KEY = "yawm.panelWidth.v1";

export {
  DEFAULT_PANEL_WIDTH,
  MAX_PANEL_FRACTION,
  MIN_PANEL_WIDTH,
  clampPanelWidth,
} from "@/lib/layout";

export function usePanelWidth() {
  const [width, setWidth] = useState(DEFAULT_PANEL_WIDTH);
  const preferredWidth = useRef(DEFAULT_PANEL_WIDTH);

  useEffect(() => {
    try {
      const raw = localStorage.getItem(PANEL_KEY);
      if (raw) {
        // Clamped on the way in, not only on the way out. A width saved on a
        // wider monitor is a preference about a window that is not here.
        preferredWidth.current = Number(raw) || DEFAULT_PANEL_WIDTH;
        setWidth(clampPanelWidth(preferredWidth.current, window.innerWidth));
      }
    } catch {
      // Storage being unavailable is not worth failing a render over.
    }
  }, []);

  /*
   * And again whenever the window changes size.
   *
   * Without this the restored clamp only held until the first resize: dragging
   * the window narrow left the panel at its old pixel width, which is how it
   * came to cover the list it describes. The stored value is deliberately not
   * rewritten here — the user's preference is what they dragged, not what a
   * temporarily small window could show — so widening the window again returns
   * the panel to it.
   */
  useEffect(() => {
    const onResize = () =>
      setWidth(clampPanelWidth(preferredWidth.current, window.innerWidth));
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const resize = useCallback((next: number) => {
    const capped = clampPanelWidth(next, window.innerWidth);
    preferredWidth.current = capped;
    setWidth(capped);
    try {
      localStorage.setItem(PANEL_KEY, String(capped));
    } catch {
      // As above.
    }
  }, []);

  const reset = useCallback(() => {
    preferredWidth.current = DEFAULT_PANEL_WIDTH;
    setWidth(clampPanelWidth(DEFAULT_PANEL_WIDTH, window.innerWidth));
    try {
      localStorage.removeItem(PANEL_KEY);
    } catch {
      // As above.
    }
  }, []);

  return { width, resize, reset };
}
