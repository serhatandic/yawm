import type { DiffStyle } from "@/lib/api";

/**
 * How a gap between two hunks is drawn, in either mode.
 *
 * "N unmodified lines" was wrong twice over. Nothing can be expanded — the
 * patches carry three lines around each change and no more — so the count is
 * unactionable in both modes, and it answers no question someone deciding
 * whether deletion loses work is asking. In the at-risk view it is not even
 * true: most of those lines were modified by the branch and have simply
 * already landed. Filled and labelled, the gap was also the heaviest band on
 * screen, chrome outshouting the finding beside it, and directly under a file
 * header it read as a section divider rather than as missing code.
 *
 * So a gap says only that it is a gap. The jump in the line numbers either
 * side already says how large it is, for anyone who cares.
 *
 * Above the first hunk and below the last there is nothing at all: there the
 * missing code is just the rest of a file nobody claimed to be showing whole,
 * and the first line number says where the excerpt begins.
 */
export const SEPARATOR_CSS = `
[data-unmodified-lines] {
  display: none;
}
[data-separator="line-info"][data-separator-first],
[data-separator="line-info"][data-separator-last],
[data-separator="line-info-basic"][data-separator-first],
[data-separator="line-info-basic"][data-separator-last] {
  display: none;
}
[data-separator="line-info"] [data-separator-content]::before,
[data-separator="line-info-basic"] [data-separator-content]::before {
  content: "⋮";
  opacity: 0.3;
}
`;

/**
 * The at-risk mode's own rules, injected into the renderer's shadow root.
 *
 * Everything in this mode's patch that is not at risk is context, whether the
 * branch wrote it last week or it has been there for five years, so it is
 * dimmed to sit behind the amber rows rather than compete with them.
 *
 * The strike is what tells a removal apart from an addition once both are
 * amber. A line the branch deleted is not in the branch's file at all; it is
 * drawn struck through, in the place it used to occupy, because what deleting
 * the worktree would lose there is the removal itself.
 *
 */
export const AT_RISK_CSS = `
[data-line-type="context"],
[data-line-type="context-expanded"] {
  opacity: 0.5;
}
[data-line-type="change-deletion"] {
  text-decoration: line-through;
  text-decoration-color: var(--diffs-deletion-base);
  text-decoration-thickness: 1px;
}
`;

/**
 * The theme the renderer highlights with.
 *
 * Named here rather than inline because it is the one option that can fail:
 * `@pierre/theming` reaches for the theme through a dynamic import of
 * `@pierre/theme`, so if that package is not resolvable from the bundle the
 * load rejects, no hunks are ever produced, and every expanded file draws an
 * empty `<pre>` — a caret that opens onto nothing. `patch-render.test.ts`
 * renders a real patch through this exact name to keep that resolvable.
 */
export const DIFF_THEME = "pierre-dark";

/**
 * Everything `PatchDiff` is told, in one place so a test can render with it.
 *
 * These used to be written inline at the call site, which meant the only way
 * to check that a patch actually draws was to run the application and look.
 * A blank body is not visible to a unit test of the surrounding component —
 * it is a property of what the renderer makes of these options — so the
 * options are a value now, and the regression renders a patch with them.
 */
export function patchDiffOptions({
  atRisk,
  diffStyle,
}: {
  atRisk: boolean;
  diffStyle: DiffStyle;
}) {
  return {
    unsafeCSS: atRisk ? `${SEPARATOR_CSS}${AT_RISK_CSS}` : SEPARATOR_CSS,
    // Pierre's own theme is close to yawm's palette; the rest is closed by
    // the --diffs-* overrides in styles.css.
    theme: DIFF_THEME,
    themeType: "dark" as const,
    // The JS regex engine rather than the WASM Oniguruma build: it drops a
    // 600 KB chunk, and the grammars yawm sees are ordinary source files, not
    // the cases wasm exists for.
    preferredHighlighter: "shiki-js" as const,
    diffStyle,
    disableFileHeader: true,
    /*
     * The list scrolls, not each file inside it.
     *
     * "scroll" makes every diff manage its own scrolling, which needs a
     * height handed down to it — but the body it sits in is sized by its
     * content, so each was waiting on the other. Wrapping keeps the one outer
     * scroller in charge, which is what a stacked list of files wants anyway,
     * and a wrapped long line beats a row that scrolls sideways under the
     * reader.
     */
    overflow: "wrap" as const,
    // Long runs of untouched code are noise when the question is "what did
    // this worktree actually do".
    expandUnchanged: false,
    collapsedContextThreshold: 8,
    expansionLineCount: 20,
  };
}
