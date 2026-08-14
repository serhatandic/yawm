import { useEffect, useMemo, useRef, useState } from "react";
import { PatchDiff } from "@pierre/diffs/react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { FileTree, Stat } from "@/components/FileTree";
import type { FileEntry } from "@/components/file-tree";
import { narrowToRiskHunks } from "@/components/at-risk-hunks";
import { patchDiffOptions } from "@/components/diff-options";
import {
  anchorId,
  anchorScope,
  balanceOf,
  combineBalances,
  COMMITTED_HEADING,
  coverageOf,
  fileEntries,
  groupAnchorId,
  hasCollapsibleSections,
  lineTotals,
  mergeOmitted,
  NO_LINE_DIFFS_TITLE,
  ON_DISK_HEADING,
  omittedBreakdown,
  omittedClause,
  omittedFrom,
  readingNarrows,
  residualPaths,
  scrollToAnchor,
  sectionFor,
  textEntries,
  textStat,
  type DiffSectionModel,
  type GroupBalance,
} from "@/components/diff-sections";
import {
  NOT_DIFFABLE_DESCRIPTION,
  NOT_DIFFABLE_LIST_LABEL,
  NOT_DIFFABLE_TITLE,
  disclosureCapNote,
  disclosureResidualNote,
  disclosureScale,
  hasDisclosure,
  notDiffableDisclosure,
  notDiffableTriggerLabel,
  type NotDiffableDisclosure,
} from "@/components/not-diffable";
import {
  EMPTY_COLLAPSE,
  everyCollapsed,
  isCollapsed,
  reconcileCollapse,
  setAllCollapsed,
  setCollapsed,
  type CollapseState,
} from "@/components/collapse";
import {
  api,
  AT_RISK_READING_LABEL,
  changesSummarySegments,
  diffLimitMessages,
  EVERYTHING_READING_LABEL,
  limitRemedy,
  noBranchCommitsClause,
  type ChangeOrigin,
  type ChangesBalance,
  type DiffResult,
  type DiffStyle,
  type FocusedPatch,
  type StatSegment,
  type UniquePatch,
  unmergedLinesByFile,
} from "@/lib/api";
import type { ChangesAnchor, ReadingIntent } from "@/lib/tabs";
import { cn, FOCUS_RING } from "@/lib/utils";
import {
  ChevronRight,
  ChevronsDownUp,
  ChevronsUpDown,
  Columns2,
  PanelLeft,
  Rows2,
} from "lucide-react";

/**
 * The height the group headings stick at, shared by both panes.
 *
 * The file headers inside a group were already `sticky top-0`, so a group
 * heading arriving at the same offset would have sat on top of them. Both
 * layers earn their place — which group am I in, and which file — so they
 * stack instead: the heading holds the top, and file headers come to rest
 * directly beneath it. This is one number because the two panes have to agree,
 * and because a heading that grew a pixel would otherwise leave a stripe of
 * scrolling code showing above every file header.
 */
const GROUP_HEADING_H = "h-7";

/**
 * The height of a tab's own top row.
 *
 * One number, matching the title bar above it and the worktree list's filter
 * bar, so moving between tabs does not move the first line of content by a few
 * pixels each time. Deliberately separate from `GROUP_HEADING_H`, which is a
 * scroll offset two panes have to agree on rather than a chrome height.
 */
export const TAB_CHROME_H = "h-11";
const BELOW_GROUP_HEADING = "top-7";

type DiffGroupModel = {
  id: ChangeOrigin;
  /** The same words in the tree and in the pane. Nothing here has two names. */
  label: string;
  note: string | null;
  /** The commit that backs this group's claim, printed beside it as evidence. */
  commit: string | null;
  /** Caveats about how the comparison was made, for anyone who wants them. */
  title: string | null;
  /** Renders what deleting the worktree destroys, not what the branch did. */
  atRisk: boolean;
  files: FileEntry[];
  sections: DiffSectionModel[];
};

/**
 * Split a unified patch into one section per file.
 *
 * `PatchDiff` renders exactly one file, which is the right granularity here
 * anyway: a section per file gives the tree a real element to scroll to, and
 * lets each file virtualise independently. Every per-file section in a unified
 * patch begins with `diff --git`, so the split is unambiguous.
 */
export function splitPatch(patch: string): { path: string; patch: string }[] {
  const sections: { path: string; patch: string }[] = [];
  const lines = patch.split("\n");
  let current: string[] | null = null;
  let path = "";

  const flush = () => {
    if (current && current.length) {
      sections.push({ path, patch: current.join("\n") });
    }
  };

  for (const line of lines) {
    if (line.startsWith("diff --git ")) {
      flush();
      current = [line];
      // `diff --git a/x b/x` — take the b-side, which is the path after any
      // rename, and the one the file tree lists.
      const match = /^diff --git a\/(.+?) b\/(.+)$/.exec(line);
      path = match?.[2] ?? match?.[1] ?? "";
      continue;
    }
    if (current) {
      current.push(line);
      if (line.startsWith("--- ") && line !== "--- /dev/null") {
        path = patchHeaderPath(line.slice(4), "a/") ?? path;
      } else if (line.startsWith("+++ ") && line !== "+++ /dev/null") {
        path = patchHeaderPath(line.slice(4), "b/") ?? path;
      }
    }
  }
  flush();
  return sections;
}

function patchHeaderPath(raw: string, prefix: string): string | null {
  const decoded = decodeGitPath(raw);
  if (decoded.startsWith(prefix)) return decoded.slice(prefix.length);

  // An invalid UTF-8 path deliberately stays in Git's quoted byte spelling.
  // Remove the a/ or b/ inside that spelling without collapsing its identity.
  const quotedPrefix = `"${prefix}`;
  if (decoded === raw && raw.startsWith(quotedPrefix)) {
    return `"${raw.slice(quotedPrefix.length)}`;
  }
  return null;
}

function decodeGitPath(raw: string): string {
  if (!raw.startsWith('"') || !raw.endsWith('"')) return raw;

  const bytes: number[] = [];
  for (let index = 1; index < raw.length - 1; index += 1) {
    const character = raw[index]!;
    if (character !== "\\") {
      bytes.push(character.charCodeAt(0));
      continue;
    }
    index += 1;
    const escape = raw[index]!;
    const mapped: Record<string, number> = {
      a: 7,
      b: 8,
      t: 9,
      n: 10,
      v: 11,
      f: 12,
      r: 13,
      "\\": 92,
      '"': 34,
    };
    if (escape in mapped) {
      bytes.push(mapped[escape]!);
      continue;
    }
    if (/[0-7]/.test(escape)) {
      let octal = escape;
      while (octal.length < 3 && /[0-7]/.test(raw[index + 1] ?? "")) {
        octal += raw[++index]!;
      }
      bytes.push(Number.parseInt(octal, 8));
      continue;
    }
    bytes.push(escape.charCodeAt(0));
  }
  const encoded = Uint8Array.from(bytes);
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(encoded);
  } catch {
    // Rust uses the same quoted spelling as the stable identity for paths that
    // cannot be represented as UTF-8, so distinct byte paths stay distinct.
    return raw;
  }
}

/**
 * What a worktree changed, relative to the default branch.
 *
 * This is what makes a Review verdict actionable: yawm can say a worktree is
 * unmerged, but deciding whether to keep it means seeing what is in it.
 *
 * The rendering is `@pierre/diffs` rather than something hand-rolled. Matching
 * a real diff editor means a Myers variant with post-processing passes,
 * character-level refinement, split-view alignment, collapsed unchanged
 * regions and TextMate highlighting — none of which is yawm's problem to solve.
 * Its `PatchDiff` takes a unified patch, so full branch diffs and the focused
 * hunks core selects travel through exactly the same renderer.
 *
 * yawm keeps its own summary header, because file counts, commit counts and
 * "includes uncommitted changes" are product facts the library has no opinion
 * about.
 */
export function DiffTab({
  repoRoot,
  path,
  intent,
  anchor,
  request,
  diffStyle,
  onDiffStyleChange,
}: {
  repoRoot: string;
  path: string;
  /** Which reading the click that opened, or re-opened, this tab asked for. */
  intent: ReadingIntent;
  /** The group that click was about, if it was about one. */
  anchor: ChangesAnchor;
  /** Changes on every open, so re-asking a different question is not ignored. */
  request: number;
  diffStyle: DiffStyle;
  onDiffStyleChange: (style: DiffStyle) => void;
}) {
  const [result, setResult] = useState<DiffResult | null>(null);
  const [focus, setFocus] = useState<FocusedPatch | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [active, setActive] = useState<{
    origin: ChangeOrigin;
    path: string;
  } | null>(null);
  const [treeOpen, setTreeOpen] = useState(true);
  const [reading, setReading] = useState<ReadingIntent>(intent);
  const [collapse, setCollapse] = useState<CollapseState>(EMPTY_COLLAPSE);
  /*
   * The names behind the "not diffable" count, on request and never before it.
   *
   * Kept as a state flag rather than as a `DialogTrigger` because the trigger
   * is a run of text inside the summary line in one case and a button in an
   * empty state in the other, and both have to open the same dialog. With no
   * `DialogTrigger`, Radix has no element to hand focus back to when the
   * dialog closes — it prevents its own default restore and focuses a trigger
   * that was never registered — so the opener is remembered here and restored
   * explicitly. Without it, dismissing the dialog dropped keyboard focus onto
   * the body and a keyboard reader restarted at the top of the window.
   */
  const [disclosing, setDisclosing] = useState(false);
  const disclosedFrom = useRef<HTMLElement | null>(null);
  const disclose = (event: React.MouseEvent<HTMLElement>) => {
    disclosedFrom.current = event.currentTarget;
    setDisclosing(true);
  };
  const pendingAnchor = useRef<ChangesAnchor>(anchor);
  /*
   * Every open tab stays mounted, so this view is one of several in the
   * document and has to be able to say which anchors are its own.
   */
  const view = useRef<HTMLDivElement | null>(null);
  const scope = anchorScope(path);

  /*
   * A second click is a new question, and it changes the reading — not the
   * fetch.
   *
   * Opening a tab that is already open used to return the tab untouched, so
   * the view kept answering whatever the first click asked for. `request`
   * changes on every open, so the new intent wins here. Nothing else is
   * cleared: what has been read, and which files the reader folded, survive a
   * re-aim, because none of it was invalidated by the click.
   */
  useEffect(() => {
    setReading(intent);
    pendingAnchor.current = anchor;
  }, [request, intent, anchor]);

  /*
   * One fetch, for one worktree.
   *
   * Keyed on the worktree alone: the reading is a way of looking at this
   * payload, not a different payload, so switching readings cannot refetch and
   * cannot flash a skeleton over data already on screen. The complete history
   * request is the only one made, because the narrower reading is derived from
   * it rather than fetched beside it.
   */
  useEffect(() => {
    let cancelled = false;
    setResult(null);
    setFocus(null);
    setError(null);
    setActive(null);
    setCollapse(EMPTY_COLLAPSE);

    void api
      .diffWorktree(repoRoot, path, "history")
      .then((r) => {
        if (!cancelled) setResult(r);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    void api
      .focusedWorktree(repoRoot, path)
      .then((resolved) => {
        if (!cancelled) setFocus(resolved);
      })
      .catch(() => {
        /*
         * A failed narrowing is not an error the reader has to act on: the
         * complete reading is still on screen and still complete. The filter
         * simply never appears, which is the same thing that happens when
         * there is nothing to narrow.
         */
      });

    return () => {
      cancelled = true;
    };
  }, [repoRoot, path]);

  /*
   * A click about work on disk lands on the work on disk.
   *
   * The old answer to that click was a different, narrower fetch that hid the
   * branch's commits entirely. Anchoring says the same thing without lying by
   * omission: the group the reader asked about is what they are looking at,
   * and the rest of the worktree is still under it.
   *
   * The heading is looked for inside this view rather than in the document:
   * every other open worktree is mounted too, and a document-wide lookup
   * scrolled whichever of them happened to mount first — leaving the tab the
   * reader was actually looking at exactly where it was.
   */
  useEffect(() => {
    if (result === null) return;
    if (pendingAnchor.current !== "uncommitted") return;
    pendingAnchor.current = null;
    scrollToAnchor(view.current, groupAnchorId(scope, "uncommitted"));
  }, [result, request, scope]);

  /*
   * Switching readings keeps the reader where they were.
   *
   * The filter narrows what is drawn, and nothing about the payload, so it may
   * not throw away the reader's place in it: it used to return the scroll to
   * the top and clear the selection, which meant a reader checking one file
   * against the narrower reading had to find it again — twice, to get back.
   * The file they were on is the same file in either reading, so the view
   * follows it; when the narrower reading does not contain it, the scroll
   * simply stays where it was rather than jumping.
   */
  useEffect(() => {
    if (active === null) return;
    scrollToAnchor(view.current, anchorId(scope, active.origin, active.path));
    // Only a change of reading moves the view: selecting a file already scrolls
    // to it, so `active` is deliberately not a dependency here.
  }, [reading]);

  if (error) {
    // The likeliest cause is the worktree being deleted while its tab stayed
    // open, so say that rather than surfacing a raw error.
    return (
      <Empty
        title="This worktree no longer exists"
        body={`${path} could not be read. It may have been deleted or moved.`}
      />
    );
  }

  if (result === null) {
    return <DiffTabSkeleton />;
  }

  const { summary, patches } = result;

  /*
   * The two things a worktree can be holding, kept apart — and both on screen.
   *
   * Committed work exists in the object database and survives deleting the
   * directory. Work on disk has no object of its own, even when the default
   * branch already contains the same line-level effect. That distinction is
   * the whole question a reader deciding whether to delete is asking, so it is
   * two groups in one scroll rather than two fetches behind a switch. On disk
   * comes first because it is the half that cannot be recovered.
   *
   * Each heading names what the state *means for the reader* rather than
   * naming it in git's vocabulary, and the same two labels are used in the
   * tree and in the pane so nothing has two names.
   */
  const onDiskEntries = textEntries(patches.uncommitted);
  const committedEntries = textEntries(patches.committed);
  const onDiskOmitted = omittedFrom(patches.uncommitted);
  const committedOmitted = omittedFrom(patches.committed);

  const onDiskSections = onDiskEntries.map((entry) =>
    sectionFor(scope, entry, "uncommitted", {
      counting: "Lines this file changed on disk since the last commit.",
      atRisk: false,
      counts: { insertions: entry.insertions, deletions: entry.deletions },
    }),
  );

  const everythingSections = committedEntries.map((entry) =>
    sectionFor(scope, entry, "committed", {
      counting: "Lines this file added and removed since the fork.",
      atRisk: false,
      counts: { insertions: entry.insertions, deletions: entry.deletions },
    }),
  );

  /*
   * The narrowed committed evidence, when the analysis produced one.
   *
   * Only the `unmatched` analysis is a reading of what is at risk. The
   * "would change the default branch" result answers a different question, and
   * offering it under a segment named "At risk" made the two indistinguishable
   * on screen — so it stays out of the filter and the complete reading shows.
   */
  const unmatched = focus?.kind === "unmatched" ? focus.patch : null;
  const unmergedByPath = unmatched
    ? unmergedLinesByFile(unmatched.markers)
    : new Map<string, { insertions: number; deletions: number }>();
  const atRiskSections: DiffSectionModel[] = unmatched
    ? splitPatch(unmatched.patch).map((section) => {
        const id = anchorId(scope, "committed", section.path);
        const markers = unmatched.markers.filter(
          (marker) => marker.path === section.path,
        );
        return {
          id,
          anchor: id,
          path: section.path,
          patch: narrowToRiskHunks(section.patch, markers),
          stat: textStat({
            counting: `Lines in this file that deleting the worktree would lose — they have no match on ${unmatched.target}. Everything around them is context.`,
            atRisk: true,
            counts: unmergedByPath.get(section.path) ?? null,
          }),
        };
      })
    : [];

  /*
   * The filter exists only when both readings would actually differ.
   *
   * Two segments that render the same paths and the same content are a control
   * that does nothing, and a reader who presses it and sees no change learns
   * that this view's chrome cannot be trusted. With no branch-only commits
   * there is nothing to narrow at all.
   */
  const filterAvailable =
    everythingSections.length > 0 &&
    readingNarrows(everythingSections, atRiskSections);
  const visibleReading: ReadingIntent =
    filterAvailable && reading === "atRisk" ? "atRisk" : "everything";
  const atRiskReading = visibleReading === "atRisk";

  const committedSections = atRiskReading
    ? atRiskSections
    : everythingSections;

  const groups: DiffGroupModel[] = [
    {
      id: "uncommitted" as const,
      label: ON_DISK_HEADING,
      note: null,
      commit: null,
      title:
        "Changes that exist only in this directory: staged, unstaged, and untracked. Deleting the worktree loses them.",
      atRisk: false,
      sections: onDiskSections,
    },
    {
      id: "committed" as const,
      label: COMMITTED_HEADING,
      /*
       * The commit that backs the claim rides in the heading rather than in
       * the chrome above: it is evidence for one specific finding — this
       * branch's work was found on the target, up to these lines — and it is
       * meaningless beside changes on disk.
       */
      note: atRiskReading
        ? "closest match"
        : unmatched
          ? `closest match on ${unmatched.target}:`
          : null,
      commit: unmatched ? unmatched.candidate.slice(0, 7) : null,
      title: atRiskReading
        ? "Lines are compared strictly by file path, so code moved to a new file appears as unmerged. Text is compared literally, not semantically."
        : summary.base
          ? `Everything this branch committed since it diverged from ${summary.base}.`
          : null,
      atRisk: atRiskReading,
      sections: committedSections,
    },
  ]
    // A heading over nothing says a group is empty in the longest possible way.
    .filter((group) => group.sections.length > 0)
    .map((group) => ({ ...group, files: fileEntries(group.sections) }));

  /*
   * One identity for everything this view holds, and it does not move.
   *
   * Every path the view knows about is either a text diff it can draw or a
   * path it cannot, and both halves come from the typed entries themselves
   * rather than from `summary.files` — a separate scan snapshot, which is how
   * "257 files" came to stand next to a sidebar saying 404.
   *
   * It is computed from the complete reading in both readings. The filter
   * narrows what is *drawn*; it does not change what the worktree is holding,
   * and a denominator that shrank when a reading was selected would make the
   * two readings disagree about the same worktree. The at-risk reading states
   * its own numerator — the analysis's lines and files — against this total.
   *
   * Each group carries which paths it is accounting for, not just how many, so
   * the total can be a union: a file committed here and edited again since is
   * two entries to read and one changed path.
   */
  const balances: GroupBalance[] = [
    balanceOf(
      onDiskSections,
      onDiskOmitted,
      lineTotals(onDiskEntries),
      coverageOf(patches.uncommitted),
    ),
    balanceOf(
      everythingSections,
      committedOmitted,
      lineTotals(committedEntries),
      coverageOf(patches.committed),
    ),
  ];
  const balance: ChangesBalance = combineBalances(
    balances,
    residualPaths(patches),
  );
  const allOmitted = mergeOmitted(onDiskOmitted, committedOmitted);
  const omittedDetail = omittedBreakdown(allOmitted);
  /*
   * The same omissions again, by name rather than by kind.
   *
   * Built from the same typed entries the counts come from, so the dialog and
   * the summary above it can never describe two different sets of paths.
   */
  const disclosure = notDiffableDisclosure(patches);
  const disclosable = hasDisclosure(disclosure);

  const sectionCount = groups.reduce(
    (count, group) => count + group.sections.length,
    0,
  );
  /*
   * Collapsing is about patches, and every section is one.
   *
   * It used to be a mixed list: eight patches and five cards, where "collapse
   * all" appeared to do nothing to five of the rows and each of those rows
   * carried a chevron that opened onto a blank strip. The cards are gone, so
   * this list is exactly the set that can open and close.
   */
  const allSections = groups.flatMap((group) => group.sections);
  /*
   * The defaults land in state before anything is drawn.
   *
   * Reconciling during render, rather than in an effect, is what makes the
   * first click work: the body a row renders and the value its caret toggles
   * are the same entry in the same map from the very first paint, instead of
   * one of them being a default derived from a section list that had not
   * finished arriving.
   */
  const reconciled = reconcileCollapse(collapse, allSections);
  if (reconciled !== collapse) setCollapse(reconciled);
  const sectionIsCollapsed = (section: DiffSectionModel) =>
    isCollapsed(reconciled, section);
  const anythingCollapsible = hasCollapsibleSections(allSections);
  const everySectionCollapsed = everyCollapsed(reconciled, allSections);
  const setEverySectionCollapsed = (collapsed: boolean) => {
    setCollapse((current) => setAllCollapsed(current, allSections, collapsed));
  };
  const scrollToFile = (origin: ChangeOrigin, filePath: string) => {
    const id = anchorId(scope, origin, filePath);
    /*
     * The selection carries its group.
     *
     * Both groups can hold the same path — the same file committed and then
     * edited again — and a bare path lit that row in both trees at once, so
     * clicking one file appeared to select two.
     */
    setActive({ origin, path: filePath });
    setCollapse((current) => setCollapsed(current, id, false));
    // The anchor is the lightweight shell, not the expensive diff, so direct
    // navigation works before IntersectionObserver has asked it to render.
    scrollToAnchor(view.current, id);
  };

  /*
   * Shown whenever there is anything to list.
   *
   * It was hidden for a single file on the grounds that one file needs no
   * navigation, which ignored two things: the panel also states how many files
   * are at risk and what each contributes, and the two readings rarely have
   * the same count — so a branch with one at-risk file and seventy in its
   * history had the panel, and the button that toggles it, appear and vanish
   * as the reader switched between them.
   */
  const treeWorthShowing = sectionCount > 0;

  const changeReading = (next: ReadingIntent) => {
    if (next === visibleReading) return;
    setReading(next);
  };

  return (
    /*
      `tabIndex={-1}` so this can take focus programmatically and never in the
      tab order. It is where focus lands if the control that opened the
      disclosure has gone — a tab switched or closed by a window shortcut while
      the dialog was open — so dismissing it leaves the reader inside the view
      they were reading rather than at the top of the window.
    */
    <div ref={view} tabIndex={-1} className="flex h-full min-h-0 flex-col outline-none">
      {/*
        One row of fixed chrome, and everything else scrolls.

        There used to be four rows before any code: two segmented controls, a
        paragraph restating them, a group heading restating the paragraph, and
        the first file. What is left here is what has to stay put — at most one
        switch between the two readings, the one sentence of scale, and the
        view controls. The headings that name each group scroll with the group
        they name, so their cost is temporary.
      */}
      <header className={cn("flex shrink-0 items-center gap-3 border-b border-border px-3", TAB_CHROME_H)}>
        {treeWorthShowing ? (
          <Toggle
            label={treeOpen ? "Hide file list" : "Show file list"}
            active={treeOpen}
            onClick={() => setTreeOpen((v) => !v)}
          >
            <PanelLeft className="size-3.5" />
          </Toggle>
        ) : null}

        <div className="flex min-w-0 flex-1 items-center gap-3">
          {/*
            One control, and it is a reading rather than a scope.

            There used to be two segmented controls on this row — a scope
            switch and, beside it, a reading switch whose second segment was
            named after the other control's second segment. Both are gone. This
            one appears only when the narrower reading would genuinely show
            something different, so it can never be a segment that does
            nothing.
          */}
          {filterAvailable ? (
            <div
              className="flex shrink-0 items-center rounded-md bg-muted/60 p-0.5"
              role="group"
              aria-label="Reading"
            >
              <ModeButton
                active={atRiskReading}
                onClick={() => changeReading("atRisk")}
              >
                {AT_RISK_READING_LABEL}
              </ModeButton>
              <ModeButton
                active={!atRiskReading}
                onClick={() => changeReading("everything")}
              >
                {EVERYTHING_READING_LABEL}
              </ModeButton>
            </div>
          ) : null}
          <ChangesSummary
            balance={balance}
            atRisk={atRiskReading ? unmatched : null}
            leadingClause={
              summary.commits === 0 ? noBranchCommitsClause(summary.base) : null
            }
            detail={omittedDetail}
            onDisclose={disclosable ? disclose : null}
            discloseLabel={notDiffableTriggerLabel(balance.notDiffable)}
          />
          {/*
            How the diff is drawn belongs with what it says, so these sit
            against the stat line rather than in a row of unrelated icons.
          */}
          <div className="flex shrink-0 items-center gap-1">
            <Toggle
              label="Unified"
              active={diffStyle === "unified"}
              onClick={() => onDiffStyleChange("unified")}
            >
              <Rows2 className="size-3.5" />
            </Toggle>
            <Toggle
              label="Side by side"
              active={diffStyle === "split"}
              onClick={() => onDiffStyleChange("split")}
            >
              <Columns2 className="size-3.5" />
            </Toggle>
          </div>
        </div>

        {/*
          Closing every file is not a view setting, so it keeps its distance.

          It is drawn only when something can actually close. A list made
          entirely of binary files and nested repositories has nothing to fold,
          and a button that visibly does nothing when pressed is worse than no
          button.
        */}
        {anythingCollapsible ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="size-7 shrink-0"
                aria-label={
                  everySectionCollapsed ? "Expand all files" : "Collapse all files"
                }
                onClick={() => setEverySectionCollapsed(!everySectionCollapsed)}
              >
                {everySectionCollapsed ? (
                  <ChevronsUpDown className="size-3.5" />
                ) : (
                  <ChevronsDownUp className="size-3.5" />
                )}
              </Button>
            </TooltipTrigger>
            <TooltipContent side="bottom">
              {everySectionCollapsed ? "Expand all files" : "Collapse all files"}
            </TooltipContent>
          </Tooltip>
        ) : null}
      </header>

      <div className="flex min-h-0 flex-1">
        {treeWorthShowing && treeOpen ? (
          <div className="w-64 shrink-0 overflow-x-hidden overflow-y-auto border-r border-border">
            {groups.map((group) => (
              <FileTree
                key={group.id}
                heading={group.label}
                files={group.files}
                activePath={
                  active?.origin === group.id ? active.path : null
                }
                onSelect={(path) => scrollToFile(group.id, path)}
              />
            ))}
          </div>
        ) : null}

        <div className="min-w-0 flex-1 overflow-auto">
          {/*
            The caveats, where they cost nothing until there are any.

            These used to hold a permanent row of chrome open to say things
            that are usually not true — that the search stopped early, or that
            a file was too large to compare. Now they are the first thing in
            the scroll and they scroll away, which is the right prominence for
            a footnote.
          */}
          {atRiskReading && unmatched ? (
            <FocusCaveats patch={unmatched} />
          ) : focus?.kind === "all" &&
            focus.reason !== "noFilteredChanges" &&
            everythingSections.length > 0 ? (
            <FallbackNotice incomplete={focus.reason === "incomplete"} />
          ) : null}
          <DiffLimitNotice patches={patches} worktree={path} />

          {groups.length === 0 ? (
            /*
              Nothing to read, and which of the two reasons it is.

              A worktree holding sixty-three binaries and nested repositories
              used to draw sixty-three cards, each saying there was nothing in
              it to read. It says that once, as a quantity, and does not list
              what it is not showing.
            */
            balance.notDiffable > 0 ? (
              <Empty
                title={NO_LINE_DIFFS_TITLE}
                body={omittedClause(allOmitted) ?? ""}
                detail={omittedDetail}
                /*
                  The whole changeset is the thing this view cannot draw, so
                  the only useful next step is seeing what it is made of. On
                  this screen there is no summary line to hang that on, so it
                  is a button rather than a run of text.
                */
                action={
                  disclosable ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={disclose}
                    >
                      Show these paths
                    </Button>
                  ) : undefined
                }
              />
            ) : (
              <Empty
                title="Nothing to show"
                body={
                  summary.base
                    ? `Nothing in this worktree differs from ${summary.base}.`
                    : "There is no default branch to compare against."
                }
              />
            )
          ) : (
            groups.map((group) => (
              <section key={group.id}>
                {/*
                  Unconditional, and sticky rather than fixed.

                  It is also what a click about one group lands on, so it
                  carries the group's anchor: asking "what is on disk here"
                  scrolls to that heading rather than hiding everything else.
                */}
                <div
                  id={groupAnchorId(scope, group.id)}
                  className={cn(
                    "sticky top-0 z-20 flex scroll-mt-0 items-baseline gap-2 border-b border-border bg-card px-3",
                    GROUP_HEADING_H,
                  )}
                  title={group.title ?? undefined}
                >
                  <h2 className="shrink-0 self-center text-[11px] font-medium tracking-wide uppercase">
                    {group.label}
                  </h2>
                  {group.note || group.commit ? (
                    <p className="min-w-0 flex-1 self-center truncate text-[11px] text-muted-foreground">
                      · {group.note}
                      {group.commit ? (
                        <>
                          {" "}
                          <span className="font-mono">{group.commit}</span>
                        </>
                      ) : null}
                    </p>
                  ) : null}
                </div>

                {group.sections.map((section) => (
                  <DiffPatchSection
                    key={section.id}
                    section={section}
                    atRisk={group.atRisk}
                    diffStyle={diffStyle}
                    collapsed={sectionIsCollapsed(section)}
                    onCollapsedChange={(collapsed) =>
                      setCollapse((current) =>
                        setCollapsed(current, section.id, collapsed),
                      )
                    }
                  />
                ))}
              </section>
            ))
          )}
        </div>
      </div>

      <NotDiffableDialog
        open={disclosing}
        onOpenChange={setDisclosing}
        disclosure={disclosure}
        returnFocusTo={disclosedFrom}
        fallbackFocusTo={view}
      />
    </div>
  );
}

/**
 * The names behind the count, listed exactly once and only when asked for.
 *
 * Everything in it is a fact the view already had: one row per typed entry,
 * grouped under the same two headings the Changes view uses, with the reason
 * spelled the way the summary's breakdown spells it. What it deliberately does
 * not do is fill itself out — a directory covering four hundred paths is one
 * row saying so, because yawm was never sent the four hundred names and
 * inventing them would make this list less trustworthy than the number it
 * exists to explain.
 */
function NotDiffableDialog({
  open,
  onOpenChange,
  disclosure,
  returnFocusTo,
  fallbackFocusTo,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  disclosure: NotDiffableDisclosure;
  /** The control that opened this, since there is no `DialogTrigger` to ask. */
  returnFocusTo: React.RefObject<HTMLElement | null>;
  /** Where focus goes when that control is gone or is no longer on screen. */
  fallbackFocusTo: React.RefObject<HTMLElement | null>;
}) {
  const capNote = disclosureCapNote(disclosure);
  const residualNote = disclosureResidualNote(disclosure);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {/*
        Radix owns the focus trap and the Escape key, which is why this is the
        app's dialog primitive rather than a panel with a close button bolted
        on. What it cannot own here is where focus goes afterwards: its modal
        content restores focus to a `DialogTrigger`, and this dialog has two
        different openers in two different layouts, so the one that was used is
        given focus back explicitly.
      */}
      <DialogContent
        className="sm:max-w-2xl"
        onCloseAutoFocus={(event) => {
          /*
            Always prevented, whatever happens next: leaving it to Radix means
            focusing a trigger that was never registered, which drops focus on
            the body. The window's own tab shortcuts keep working while this is
            open, so the opener may have been unmounted or hidden behind
            another tab in the meantime — focus is attempted and then checked,
            because an element in a hidden panel accepts a `focus()` call
            without taking focus.
          */
          event.preventDefault();
          const opener = returnFocusTo.current;
          opener?.focus();
          if (document.activeElement === opener) return;
          fallbackFocusTo.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>{NOT_DIFFABLE_TITLE}</DialogTitle>
          <DialogDescription>{NOT_DIFFABLE_DESCRIPTION}</DialogDescription>
        </DialogHeader>

        <p className="text-xs text-muted-foreground">
          {disclosureScale(disclosure)}
        </p>

        {/*
          One bounded scroll region, not a dialog that grows past the window.
          The cap on rows is in the model; this is the cap on height.

          It is focusable because it is the only scrollable thing in here and
          nothing inside it can take focus: a keyboard reader with no way to
          reach the region has no way to page past the first few paths.
        */}
        <div
          tabIndex={0}
          role="group"
          aria-label={NOT_DIFFABLE_LIST_LABEL}
          className={cn(
            "max-h-[50vh] min-h-0 overflow-y-auto rounded-md border border-border",
            FOCUS_RING,
          )}
        >
          {disclosure.groups.map((group) => (
            <section key={group.origin}>
              <h3 className="sticky top-0 z-10 border-b border-border bg-card px-3 py-1.5 text-[11px] font-medium tracking-wide uppercase">
                {group.heading}
                <span className="ml-2 font-normal normal-case text-muted-foreground tabular-nums">
                  {group.entries.toLocaleString("en-US")}{" "}
                  {group.entries === 1 ? "entry" : "entries"}
                </span>
              </h3>
              <ul className="divide-y divide-border">
                {group.rows.map((row, index) => (
                  <li
                    key={`${group.origin}-${index}-${row.path}`}
                    className="px-3 py-2"
                  >
                    {/*
                      The path is the thing the reader came for, so it is
                      never abbreviated away: it truncates visually at narrow
                      widths and stays whole for a screen reader and for a
                      hover.
                    */}
                    <p
                      className="truncate font-mono text-[11px]"
                      title={row.path}
                    >
                      {row.path}
                    </p>
                    <p className="text-[11px] text-muted-foreground">
                      <span className="text-foreground">{row.reason}</span>
                      {" — "}
                      {row.detail}
                    </p>
                  </li>
                ))}
              </ul>
              {group.hidden > 0 ? (
                <p className="border-t border-border px-3 py-2 text-[11px] text-muted-foreground">
                  {group.hidden.toLocaleString("en-US")} more in this group are
                  not listed.
                </p>
              ) : null}
            </section>
          ))}
        </div>

        {capNote || residualNote ? (
          <div className="space-y-1 text-[11px] text-muted-foreground">
            {capNote ? <p>{capNote}</p> : null}
            {residualNote ? <p>{residualNote}</p> : null}
          </div>
        ) : null}

        <DialogFooter showCloseButton />
      </DialogContent>
    </Dialog>
  );
}

/**
 * What was left out, said in numbers, with something to do about it.
 *
 * The old single sentence — "Some untracked files could not be inspected
 * within safety limits" — named no file, no count and no limit, so a reader
 * could not tell whether one file or a hundred were missing, nor find them.
 * Each notice here states its cause and its quantity, and carries the command
 * that shows the reader the same thing without this view's ceilings.
 */
function DiffLimitNotice({
  patches,
  worktree,
}: {
  patches: DiffResult["patches"];
  worktree: string;
}) {
  const messages = diffLimitMessages(patches);
  if (messages.length === 0) return null;
  return (
    <div className="space-y-1 border-b border-review/20 bg-review/5 px-3 py-2 text-xs text-muted-foreground">
      {patches.limits.map((limit, index) => (
        <p key={index}>
          {messages[index]}{" "}
          <span className="font-mono text-[11px] opacity-70">
            {limitRemedy(limit, worktree)}
          </span>
        </p>
      ))}
      {messages.slice(patches.limits.length).map((message, index) => (
        <p key={`extra-${index}`}>{message}</p>
      ))}
    </div>
  );
}

function DiffTabSkeleton() {
  return (
    <div className="flex h-full min-h-0 flex-col" aria-label="Loading changes">
      <header className={cn("flex shrink-0 items-center gap-3 border-b border-border px-3", TAB_CHROME_H)}>
        <Skeleton className="size-7 shrink-0" />
        <Skeleton className="h-6 w-56" />
        <Skeleton className="h-3 w-44" />
        <div className="ml-auto flex gap-1">
          <Skeleton className="size-7" />
          <Skeleton className="size-7" />
        </div>
      </header>
      <div className="min-h-0 flex-1 overflow-hidden">
        {[0, 1, 2].map((index) => (
          <div key={index}>
            <div className="flex h-9 items-center gap-2 border-y border-border px-3">
              <Skeleton className="size-3.5 shrink-0" />
              <Skeleton
                className={cn(
                  "h-3",
                  index === 0 ? "w-56" : index === 1 ? "w-72" : "w-40",
                )}
              />
              <Skeleton className="ml-auto h-3 w-14" />
            </div>
            <DiffBodySkeleton height={index === 1 ? 136 : 116} />
          </div>
        ))}
      </div>
    </div>
  );
}

function DiffBodySkeleton({ height }: { height: number }) {
  const widths = ["w-11/12", "w-4/5", "w-2/3", "w-5/6"];
  return (
    <div
      style={{ height }}
      className="flex flex-col justify-center gap-2 overflow-hidden px-12 py-3"
      aria-hidden
    >
      {widths.map((width, index) => (
        <Skeleton
          key={index}
          className={cn("h-3 rounded-sm", width, index === 2 && "ml-6")}
        />
      ))}
    </div>
  );
}

/**
 * One file in the scroll: a patch, and nothing else.
 *
 * There used to be a second kind of section — a card for anything without
 * lines to show — and a component above these two to choose between them. A
 * list called a diff now contains only diffs, so the choice is gone and every
 * row here has a body behind its chevron.
 */
function DiffPatchSection({
  section,
  atRisk,
  diffStyle,
  collapsed,
  onCollapsedChange,
}: {
  section: DiffSectionModel;
  atRisk: boolean;
  diffStyle: DiffStyle;
  collapsed: boolean;
  onCollapsedChange: (collapsed: boolean) => void;
}) {
  /*
   * Being open is what decides whether a file draws, and nothing else.
   *
   * This used to also wait until the section neared the viewport, and that
   * second condition cost more than it saved: a diff mounted from the
   * observer's state change came up as an empty container. Collapsing is the
   * laziness that was actually wanted. A review of more than twelve files
   * starts folded, so a large history still draws nothing until a file is
   * asked for.
   *
   * There was also a second mount per file on the next animation frame, put
   * there because a first mount reliably came up zero pixels tall. That was
   * not a renderer bug and the extra mount never fixed it: `@pierre/theming`
   * loads `pierre-dark` through a dynamic import of `@pierre/theme`, that
   * package was not resolvable from this bundle, and the rejected load meant
   * no hunks were ever produced — an empty `<pre>` in the shadow root, at any
   * mount count. With the theme resolvable a single mount draws, so the
   * remount is gone; `patch-render.test.ts` holds the theme in place.
   */
  const shouldRender = !collapsed;

  return (
    <div
      id={section.anchor}
      data-diff-section
      className="relative scroll-mt-7"
    >
      <button
        type="button"
        aria-expanded={!collapsed}
        aria-controls={`${section.id}-content`}
        onClick={() => onCollapsedChange(!collapsed)}
        className={cn(
          "sticky z-10 flex h-9 w-full items-center gap-2 border-y border-border bg-background px-3 text-left text-[11px] hover:bg-card focus-visible:bg-card",
          FOCUS_RING,
          BELOW_GROUP_HEADING,
        )}
      >
        <ChevronRight
          className={cn(
            "size-3.5 shrink-0 text-muted-foreground transition-transform",
            !collapsed && "rotate-90",
          )}
        />
        <span
          className="min-w-0 flex-1 truncate font-medium"
          title={section.path}
        >
          {section.path}
        </span>
        <Stat stat={section.stat} />
      </button>
      <div id={`${section.id}-content`} className="bg-background">
        {shouldRender ? (
          <PatchDiff
            patch={section.patch}
            className={cn("yawm-diff", atRisk && "yawm-diff-risk")}
            options={patchDiffOptions({ atRisk, diffStyle })}
          />
        ) : null}
      </div>
    </div>
  );
}

/**
 * The one line of scale above the changes, describing what is on screen.
 *
 * One reading at a time, and never two analyses added together: the complete
 * reading states an identity the reader can check — changed paths are text
 * diffs plus paths that could not be diffed — and the at-risk reading states
 * the analysis's own line and file counts against the paths it read. What it
 * does not do is call its text diffs "files" while a sidebar counting distinct
 * dirty paths says something larger; the two are separate snapshots, so each
 * says exactly what it counted.
 */
function ChangesSummary({
  balance,
  atRisk,
  leadingClause,
  detail,
  onDisclose,
  discloseLabel,
}: {
  balance: ChangesBalance;
  /** The focused analysis, present only while the at-risk reading is shown. */
  atRisk: UniquePatch | null;
  leadingClause: string | null;
  detail: string | null;
  /** Null when there is nothing to disclose, so the count stays plain text. */
  onDisclose: ((event: React.MouseEvent<HTMLElement>) => void) | null;
  discloseLabel: string;
}) {
  const segments = useMemo(
    () => changesSummarySegments({ balance, atRisk, leadingClause }),
    [balance, atRisk, leadingClause],
  );

  return (
    <p
      className="min-w-0 truncate text-[11px] text-muted-foreground"
      title={detail ?? undefined}
    >
      {segments.map((segment, index) =>
        /*
          The one count on this line that stands for names shown nowhere else
          is the one part of it that can be pressed. It stays inline and keeps
          the line's height, so the summary is exactly as compact as it was at
          narrow widths — what it gains is an underline, a hover and a focus
          ring, so it does not look like the text beside it.
        */
        segment.role === "notDiffable" && onDisclose ? (
          <button
            key={index}
            type="button"
            aria-label={discloseLabel}
            onClick={onDisclose}
            className={cn(
              "cursor-pointer rounded-xs underline decoration-dotted underline-offset-2 hover:text-foreground hover:decoration-solid",
              FOCUS_RING,
              TONE_CLASS[segment.tone ?? "plain"],
            )}
          >
            {segment.text}
          </button>
        ) : (
          <span key={index} className={TONE_CLASS[segment.tone ?? "plain"]}>
            {segment.text}
          </span>
        ),
      )}
    </p>
  );
}

const TONE_CLASS: Record<NonNullable<StatSegment["tone"]> | "plain", string> = {
  added: "text-disposable",
  removed: "text-broken",
  count: "text-foreground",
  note: "text-review",
  plain: "",
};

/**
 * The footnotes, and only when there are any.
 *
 * What this used to be was a fixed paragraph under the header that restated
 * the toggle above it and the group heading below it, with the caveats
 * appended. The statement moved into those two places, which already had to
 * exist; what is left is the part that is occasionally true and materially
 * changes what the reader should conclude — that the search gave up early, or
 * that a file was skipped — so this renders nothing at all on the ordinary
 * screen.
 */
function FocusCaveats({ patch }: { patch: UniquePatch }) {
  if (!patch.incomplete && !patch.truncated) return null;

  return (
    <div className="border-b border-border bg-card px-3 py-2 text-[11px] text-muted-foreground">
      {patch.incomplete
        ? "Search stopped early; more unmerged lines may exist. "
        : ""}
      {patch.truncated
        ? `Some files were too large to compare. Switch to "${EVERYTHING_READING_LABEL}" to see them.`
        : ""}
    </div>
  );
}

function FallbackNotice({ incomplete }: { incomplete: boolean }) {
  return (
    <div className="border-b border-border bg-card px-3 py-2 text-[11px] text-muted-foreground">
      yawm could not narrow the committed work safely; showing all branch
      changes.
      {incomplete
        ? " The analysis stopped before it could examine every possible leftover."
        : ""}
    </div>
  );
}

function ModeButton({
  active,
  onClick,
  disabled = false,
  children,
}: {
  active: boolean;
  onClick?: () => void;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <Button
      variant="ghost"
      size="sm"
      className={cn(
        "h-6 rounded-sm px-2 text-[10px] text-muted-foreground",
        active && "bg-background text-foreground shadow-sm ring-1 ring-border",
      )}
      aria-pressed={active}
      onClick={onClick}
      disabled={disabled}
    >
      {children}
    </Button>
  );
}

function Toggle({
  label,
  active,
  onClick,
  children,
}: {
  label: string;
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          onClick={onClick}
          aria-label={label}
          aria-pressed={active}
          className={cn("size-7", active && "bg-muted text-foreground")}
        >
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent side="bottom">{label}</TooltipContent>
    </Tooltip>
  );
}

function Empty({
  title,
  body,
  detail,
  action,
}: {
  title: string;
  body: string;
  /** The breakdown by kind, when there is one. Never a list of paths. */
  detail?: string | null;
  /** The one thing worth doing from here, when there is one. */
  action?: React.ReactNode;
}) {
  return (
    <div className="flex h-full items-center justify-center p-6 text-center">
      <div className="space-y-1">
        <p className="text-xs font-medium">{title}</p>
        <p className="max-w-sm text-[11px] text-muted-foreground">{body}</p>
        {detail ? (
          <p className="max-w-sm text-[11px] text-muted-foreground">
            {detail}
          </p>
        ) : null}
        {action ? <div className="pt-2">{action}</div> : null}
      </div>
    </div>
  );
}
