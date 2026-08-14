/**
 * Cutting a branch's whole history down to what deleting it would destroy.
 *
 * The unmerged analysis returns the branch diff of every file that holds an
 * unmerged line, which for a squash-merged branch is nearly all of it: 8,069
 * added lines carrying 17 that never landed. Rendering that diff and annotating
 * the 17 puts the safe majority in the loudest colour the view has and leaves
 * the finding as a footnote inside it.
 *
 * So the patch is rewritten before it reaches the renderer. Only the unmerged
 * lines keep their `+`/`-` prefix; every other line either becomes context or
 * disappears, and only a small window around each unmerged line survives at
 * all. What comes out is no longer a diff of two commits — it is the branch's
 * file with the at-risk lines called out — and the mode's colours say so.
 *
 * Coordinates: the emitted hunk headers number both sides from the branch
 * file. A line the branch added that has since landed is, for this decision,
 * ordinary code, so it is context — which means the old side of this patch is
 * no longer the merge base and its numbering would be fiction either way.
 * Numbering both sides from the branch file makes the one number the reader
 * cares about — where in the file this is — correct on every row, including a
 * removed line, which gets the position it used to occupy.
 */

export type RiskSide = "additions" | "deletions";

export interface RiskMarker {
  path: string;
  side: RiskSide;
  lineNumber: number;
}

/**
 * Lines kept either side of an unmerged line.
 *
 * Three is enough to see the enclosing statement, the assignment above it or
 * the closing brace below — which is what tells a version bump apart from an
 * unshipped feature — and few enough that a file contributing one unmerged
 * line renders seven rows rather than two hundred and thirty.
 */
export const RISK_CONTEXT_LINES = 3;

type RowKind = "context" | "addition" | "deletion";

interface Row {
  kind: RowKind;
  text: string;
  /** Line number in the branch file: for a removed line, where it used to sit. */
  anchor: number;
  /** The `\ No newline at end of file` note that followed this line, if any. */
  noNewline: boolean;
}

interface Window {
  start: number;
  end: number;
}

/**
 * Rewrite one file's patch so it shows only the unmerged lines and their
 * context.
 *
 * Returns the patch untouched when there is nothing to narrow to: a file with
 * no markers was pulled in whole because it could not be compared line by
 * line, and a patch with no hunks is binary or a mode change. Both already
 * have wording elsewhere that says so, and both would be made worse by a
 * silent empty body.
 */
export function narrowToRiskHunks(
  patch: string,
  markers: RiskMarker[],
  contextLines: number = RISK_CONTEXT_LINES,
): string {
  if (markers.length === 0) return patch;

  const lines = patch.split("\n");
  const firstHunk = lines.findIndex((line) => line.startsWith("@@"));
  if (firstHunk === -1) return patch;

  const atRisk = {
    additions: new Set(
      markers.filter((m) => m.side === "additions").map((m) => m.lineNumber),
    ),
    deletions: new Set(
      markers.filter((m) => m.side === "deletions").map((m) => m.lineNumber),
    ),
  };

  const out = lines.slice(0, firstHunk);
  let emitted = false;

  for (const hunk of splitHunks(lines.slice(firstHunk))) {
    const rows = readRows(hunk, atRisk);
    for (const window of windowsAround(rows, contextLines)) {
      out.push(...emitHunk(rows, window));
      emitted = true;
    }
  }

  // Markers that name lines this patch does not contain would otherwise render
  // a file header over nothing at all.
  if (!emitted) return patch;
  return out.join("\n");
}

function splitHunks(lines: string[]): string[][] {
  const hunks: string[][] = [];
  let current: string[] | null = null;
  for (const line of lines) {
    if (line.startsWith("@@")) {
      if (current) hunks.push(current);
      current = [line];
      continue;
    }
    if (current) current.push(line);
  }
  if (current) hunks.push(current);
  return hunks;
}

const HUNK_HEADER = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

function readRows(
  hunk: string[],
  atRisk: { additions: Set<number>; deletions: Set<number> },
): Row[] {
  const header = HUNK_HEADER.exec(hunk[0] ?? "");
  if (!header) return [];
  let oldLine = Number(header[1]);
  let newLine = Number(header[2]);
  const rows: Row[] = [];

  for (let index = 1; index < hunk.length; index += 1) {
    const line = hunk[index]!;
    if (line.startsWith("\\")) {
      const last = rows[rows.length - 1];
      if (last) last.noNewline = true;
      continue;
    }
    // A trailing empty element is the patch's final newline, not a line.
    if (line === "" && index === hunk.length - 1) continue;

    const prefix = line.charAt(0);
    const text = line.slice(1);
    if (prefix === "+") {
      rows.push({
        kind: atRisk.additions.has(newLine) ? "addition" : "context",
        text,
        anchor: newLine,
        noNewline: false,
      });
      newLine += 1;
    } else if (prefix === "-") {
      // A removal this branch made that already landed is not at risk and is
      // not in the branch file either, so it has no row to occupy.
      if (atRisk.deletions.has(oldLine)) {
        rows.push({
          kind: "deletion",
          text,
          anchor: newLine,
          noNewline: false,
        });
      }
      oldLine += 1;
    } else {
      rows.push({ kind: "context", text, anchor: newLine, noNewline: false });
      oldLine += 1;
      newLine += 1;
    }
  }

  return rows;
}

function windowsAround(rows: Row[], contextLines: number): Window[] {
  const windows: Window[] = [];
  rows.forEach((row, index) => {
    if (row.kind === "context") return;
    const start = Math.max(0, index - contextLines);
    const end = Math.min(rows.length - 1, index + contextLines);
    const previous = windows[windows.length - 1];
    // Touching windows are merged as well as overlapping ones: a separator
    // announcing zero hidden lines is chrome standing in for nothing.
    if (previous && start <= previous.end + 1) {
      previous.end = Math.max(previous.end, end);
      return;
    }
    windows.push({ start, end });
  });
  return windows;
}

function emitHunk(rows: Row[], window: Window): string[] {
  const body: string[] = [];
  let oldCount = 0;
  let newCount = 0;

  for (let index = window.start; index <= window.end; index += 1) {
    const row = rows[index]!;
    if (row.kind === "addition") {
      body.push(`+${row.text}`);
      newCount += 1;
    } else if (row.kind === "deletion") {
      body.push(`-${row.text}`);
      oldCount += 1;
    } else {
      body.push(` ${row.text}`);
      oldCount += 1;
      newCount += 1;
    }
    if (row.noNewline) body.push("\\ No newline at end of file");
  }

  // An empty side is numbered from zero, the way git writes a file that does
  // not exist on that side.
  const anchor = Math.max(1, rows[window.start]?.anchor ?? 1);
  const oldStart = oldCount === 0 ? 0 : anchor;
  const newStart = newCount === 0 ? 0 : anchor;
  return [`@@ -${oldStart},${oldCount} +${newStart},${newCount} @@`, ...body];
}
