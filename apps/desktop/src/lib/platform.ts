/**
 * Platform differences the UI has to account for.
 *
 * Only macOS gets an overlay title bar with the window controls drawn on top of
 * our own chrome, so only macOS needs to leave room for them. Detected rather
 * than assumed, because getting it wrong means either a gap on two platforms or
 * controls hidden under the traffic lights on the third.
 */

export const IS_MAC =
  typeof navigator !== "undefined" &&
  /Mac|iPhone|iPad/.test(navigator.userAgent);

/**
 * Space reserved at the leading edge for the macOS traffic lights.
 *
 * Wider than the lights themselves on purpose. They end around 76px, and a
 * control placed flush against them stops reading as app chrome and starts
 * reading as a fourth window button — so the gap is part of the measurement,
 * not padding to be trimmed.
 */
export const TRAFFIC_LIGHT_INSET = IS_MAC ? 88 : 12;

/** The modifier this platform uses for application shortcuts. */
export const MOD_KEY = IS_MAC ? "⌘" : "Ctrl";

/** True when the event carries this platform's application modifier. */
export function hasMod(event: KeyboardEvent | React.KeyboardEvent) {
  return IS_MAC ? event.metaKey : event.ctrlKey;
}
