import type { Risk } from "@/components/risks";

/**
 * The words on the panel's close control.
 *
 * Named here because the control is an unlabelled glyph, and because "Close"
 * alone — which is what it said while it sat in the verdict banner — read as
 * "dismiss this warning" on a coloured judgement. It closes the details, and
 * says so.
 */
export const CLOSE_DETAILS_LABEL = "Close details";

/**
 * The width below which the actions cannot share one row.
 *
 * The panel is resizable and its minimum is narrow enough that three controls
 * side by side had nowhere to go: the split button's dropdown trigger escaped
 * its own border and the icons crossed the button edges. Rather than shrink
 * the text until it is unreadable, the row breaks — the primary action takes a
 * full-width row of its own and the two secondary actions share the next one.
 *
 * The number is the panel's own width, which the panel already knows, so the
 * layout is decided from a measurement rather than from a viewport media query
 * that knows nothing about how wide this panel happens to be.
 */
export const COMFORTABLE_PANEL_WIDTH = 340;

/** How the action strip is laid out at a given panel width. */
export type ActionLayout = "row" | "stacked";

export function actionLayout(panelWidth: number): ActionLayout {
  return panelWidth < COMFORTABLE_PANEL_WIDTH ? "stacked" : "row";
}

/**
 * One risk, as the panel now draws it: an icon, a label, and nothing else.
 *
 * Each flag used to carry a detail sentence and up to two raw diff fragments,
 * so five facts became half a screen of prose and code and the one that could
 * be acted on was buried in it. The facts are unchanged — the risk set is the
 * same, the counts are the same — but the row is one line, and the full
 * sentence is its title.
 */
export interface RiskRow {
  kind: Risk["kind"];
  label: string;
  count: number | null;
  /** Clicking it opens exactly the work it counts. */
  opensUncommitted: boolean;
}

export function riskRow(risk: Risk): RiskRow {
  return {
    kind: risk.kind,
    label: risk.label,
    count: risk.count ?? null,
    opensUncommitted: risk.kind === "uncommitted",
  };
}
