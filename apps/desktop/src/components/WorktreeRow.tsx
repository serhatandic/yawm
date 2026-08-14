import { Checkbox } from "@/components/ui/checkbox";
import {
  SELECT_ALL_LABEL,
  selectAllChecked,
  type SelectAllState,
} from "@/components/bulk-select";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { VerdictBadge } from "@/components/verdict";
import {
  RiskIcon,
  riskCountLabel,
  riskSentence,
  riskToneClass,
  risksOf,
} from "@/components/risks";
import {
  type Worktree,
  reasonLabel,
  reclaimableBytes,
  worktreeLabel,
} from "@/lib/api";
import { cn, humanBytes, relativeTime, FOCUS_RING } from "@/lib/utils";
import {
  COLUMN_GAP,
  NAME_TARGET,
  ROW_PADDING_X,
  applyResize,
  gridTemplate,
  resizeRange,
  uiCharWidth,
  type ColumnLayout,
  type ColumnWidths,
  type ColumnWidthUpdate,
  type ResizableColumn,
} from "@/lib/columns";
import { branchCharBudget, middleTruncate, repoCharBudget } from "@/lib/layout";


/**
 * One worktree in the list.
 *
 * Everything on the row is subordinate to one question: which worktree is this?
 * A row you cannot identify cannot be deleted safely, so the branch name is the
 * only cell allowed to take space from the others, and the only one shortened
 * by a rule rather than by whatever CSS has left over.
 */
export function WorktreeRow({
  worktree,
  repoName,
  layout,
  selected,
  selectable,
  checked,
  sizePending,
  landingPending,
  onSelect,
  onToggle,
  onOpenUncommitted,
}: {
  worktree: Worktree;
  /** Omitted when the list is already scoped to one repository. */
  repoName?: string;
  layout: ColumnLayout;
  selected: boolean;
  selectable: boolean;
  checked: boolean;
  /** This row's size is still being walked, so it is blank rather than unknown. */
  sizePending?: boolean;
  /** Without this distinction, a failed proof would pulse as if still running. */
  landingPending?: boolean;
  onSelect: () => void;
  onToggle: () => void;
  /** Opens this worktree's uncommitted changes, and nothing else. */
  onOpenUncommitted?: () => void;
}) {
  const { status } = worktree;
  // reclaimableBytes already returns 0 for anything not disposable.
  const reclaimable = reclaimableBytes(worktree) > 0;

  return (
    <div
      role="row"
      tabIndex={0}
      /*
        `aria-current`, not `aria-selected`. In a grid, "selected" is the state
        the checkbox in this row already reports, and the inspector being open
        on a row is a different thing entirely — one row is current, any number
        of rows are ticked.
      */
      aria-current={selected ? "true" : undefined}
      onClick={onSelect}
      onKeyDown={(e) => {
        // Nested controls own their keys. Without this guard, Space on the
        // checkbox bubbled here, selected the row, and prevented the checkbox's
        // native keyboard activation.
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onSelect();
        }
      }}
      style={{
        gridTemplateColumns: gridTemplate(layout),
        columnGap: COLUMN_GAP,
        // Taken from the same constant the tracks are fitted against; as a
        // class it was invisible to the arithmetic and the last column paid.
        paddingInline: ROW_PADDING_X,
      }}
      className={cn(
        "group grid w-full cursor-default items-center rounded-md text-left transition-colors",
        FOCUS_RING,
        layout.stacked ? "py-1" : "py-1.5",
        /*
          Three states, and they have to be three appearances.

          Inspected and ticked are different claims — "this is the row I am
          reading" against "this is one of the rows the Delete button is about"
          — and both were drawn as the same grey fill, so a bulk selection was
          invisible the moment the inspector was open on one of its rows. The
          tick keeps a mark of its own: an inset bar at the leading edge, which
          survives being inspected and hovered because it is not a background.
        */
        selected ? "bg-muted" : "hover:bg-muted/60",
        checked &&
          "shadow-[inset_2px_0_0_0_var(--color-primary)] bg-primary/8 hover:bg-primary/12",
        checked && selected && "bg-primary/15 hover:bg-primary/15",
      )}
    >
      <span role="gridcell" className="flex items-center">
        <Checkbox
          checked={checked}
          disabled={!selectable}
          onClick={(e) => e.stopPropagation()}
          onCheckedChange={onToggle}
          aria-label={`Select ${worktreeLabel(worktree)}`}
          className={cn("size-3.5", !selectable && "invisible")}
        />
      </span>

      <Identifier
        label={worktreeLabel(worktree)}
        repoName={repoName}
        layout={layout}
      />

      {/*
        Status without prose.

        It used to carry a sentence, which meant the column that could not
        truncate was sized for content that always did — and the rows with the
        most going on showed the least, because their marks pushed the sentence
        out first. A badge and a set of counted tokens have a maximum width that
        can be reserved; a sentence does not. The sentence is not gone, it is in
        the panel where there is room to finish it, and on hover here.
      */}
      <span
        role="gridcell"
        className="flex min-w-0 items-center gap-1.5 overflow-hidden"
      >
        {status.landingComplete ? (
          <span title={reasonLabel(worktree.reason)} className="flex shrink-0">
            <VerdictBadge verdict={worktree.verdict} />
          </span>
        ) : (
          <span
            aria-label={
              landingPending
                ? "Checking rewritten history"
                : "Landing analysis unfinished"
            }
            className={cn(
              "inline-block h-[18px] w-[74px] shrink-0 rounded bg-muted-foreground/25",
              landingPending && "animate-pulse",
            )}
          />
        )}
        <Signals worktree={worktree} onOpenUncommitted={onOpenUncommitted} />
      </span>

      {/*
        Left, like its header. A right-aligned value under a left-aligned label
        is two columns pretending to be one — and the label has to stay left so
        it sits against the divider that resizes it. tabular-nums keeps the
        digits in line without borrowing the edge to do it.
      */}
      {layout.showModified ? (
        <span
          role="gridcell"
          className="truncate text-[11px] tabular-nums text-muted-foreground"
        >
          {relativeTime(status.size?.lastModified ?? status.lastCommitAt)}
        </span>
      ) : null}

      <span
        role="gridcell"
        className={cn(
          "truncate tabular-nums",
          reclaimable ? "text-disposable" : "text-muted-foreground",
        )}
      >
        {/*
          A dash means "there is nothing to report"; a pending walk means "not
          yet". Rendering both as a dash is what let twenty seconds of work
          look like a finished, empty answer.
        */}
        {sizePending ? (
          <span
            aria-label="Measuring"
            className="inline-block h-2.5 w-10 animate-pulse rounded-sm bg-muted-foreground/25 align-middle"
          />
        ) : (
          humanBytes(status.size?.bytes ?? null)
        )}
      </span>
    </div>
  );
}

/**
 * Which worktree this row is.
 *
 * Shortened from the middle, never the end. These branches are generated per
 * task and share long prefixes, so trailing truncation turns six different
 * worktrees into six copies of `feature/fix-…` — six rows a person about
 * to delete one cannot tell apart. The tail is what differs, so the tail is
 * what survives.
 *
 * Under real pressure the repository drops onto a second line instead of
 * competing for the same one. A row is 27pt tall and the window is not getting
 * wider; the vertical axis is the space that is actually available.
 */
function Identifier({
  label,
  repoName,
  layout,
}: {
  label: string;
  repoName?: string;
  layout: ColumnLayout;
}) {
  const full = repoName ? `${repoName}/${label}` : label;
  const stacked = layout.stacked && Boolean(repoName);
  const inlineRepo = !stacked && repoName ? repoName : undefined;

  const branch = middleTruncate(
    label,
    branchCharBudget(
      layout.name,
      uiCharWidth(13, 500),
      inlineRepo ? repoCharBudget(inlineRepo) : 0,
    ),
  );

  if (stacked) {
    return (
      <span
        role="gridcell"
        className="flex min-w-0 flex-col justify-center"
        title={full}
      >
        <span className="truncate font-medium leading-4">{branch}</span>
        {/*
          Trailing truncation is right here and wrong above: a repository name
          is read from its start, and it is the one identifier the reader can
          afford to lose characters from.
        */}
        <span className="truncate text-[10px] leading-[13px] text-muted-foreground">
          {repoName}
        </span>
      </span>
    );
  }

  return (
    <span
      role="gridcell"
      className="flex min-w-0 items-baseline overflow-hidden"
      title={full}
    >
      {inlineRepo ? (
        <>
          {/*
            The separator sits outside the truncating span. Inside it, a
            shortened repository swallowed the slash too — `chugs-w…` running
            straight into a branch name, which reads as one broken identifier
            rather than two joined ones.
          */}
          <span className="min-w-0 shrink truncate text-muted-foreground">
            {inlineRepo}
          </span>
          <span className="shrink-0 text-muted-foreground">/</span>
        </>
      ) : null}
      <span className="shrink-0 font-medium">{branch}</span>
    </span>
  );
}

/**
 * Every risk on the row, as counted tokens.
 *
 * Tokens rather than words because a word has no width you can plan for. An
 * icon plus a number does: four of them fit in a fixed column and none of them
 * displaces the branch name. No risks leaves the space blank, which is itself
 * the answer.
 *
 * All of them now, including the one the verdict rests on — the row no longer
 * states that reason in prose, so filtering it out here would delete it from
 * the row rather than avoid saying it twice.
 *
 * The uncommitted token is the one that is also a control. It is the count a
 * reader wants to open, and it now opens exactly that count's changes rather
 * than the branch's whole history with the uncommitted work mixed into it.
 */
function Signals({
  worktree,
  onOpenUncommitted,
}: {
  worktree: Worktree;
  onOpenUncommitted?: () => void;
}) {
  const risks = risksOf(worktree);
  if (risks.length === 0) return null;

  return (
    <span className="flex min-w-0 items-center gap-1.5 overflow-hidden">
      {risks.map((risk) => {
        const openable = risk.kind === "uncommitted" && onOpenUncommitted;
        const token = (
          <>
            <RiskIcon risk={risk} className="size-3" />
            {riskCountLabel(risk)}
          </>
        );
        return (
          <Tooltip key={risk.kind}>
            <TooltipTrigger asChild>
              {openable ? (
                <button
                  type="button"
                  aria-label={`Show uncommitted changes (${riskCountLabel(risk)})`}
                  onClick={(event) => {
                    event.stopPropagation();
                    onOpenUncommitted();
                  }}
                  className={cn(
                    "flex shrink-0 items-center gap-0.5 rounded-sm text-[10px] tabular-nums underline-offset-2 hover:underline",
                    FOCUS_RING,
                    riskToneClass(risk),
                  )}
                >
                  {token}
                </button>
              ) : (
                <span
                  className={cn(
                    "flex shrink-0 items-center gap-0.5 text-[10px] tabular-nums",
                    riskToneClass(risk),
                  )}
                >
                  {token}
                </span>
              )}
            </TooltipTrigger>
            <TooltipContent side="top" className="max-w-80">
              {riskSentence(risk)}
              {openable ? " · Click to show these changes." : null}
            </TooltipContent>
          </Tooltip>
        );
      })}
    </span>
  );
}

/**
 * The column header, and the handles that resize the columns.
 *
 * Lives beside the row on purpose: both lay out from the same grid template,
 * so a column cannot change width on one side only.
 *
 * The elastic branch track comes before every fixed track. That anchors the
 * fixed cluster to the right: changing only the column left of a divider moves
 * its leading edge, not the divider being dragged. Internal dividers therefore
 * trade width between their two neighbours. The first divider can use the
 * elastic branch directly.
 */
export function WorktreeRowHeader({
  showRepo,
  layout,
  widths,
  onResize,
  onReset,
  selectAll,
}: {
  showRepo: boolean;
  layout: ColumnLayout;
  widths: ColumnWidths;
  onResize: (update: ColumnWidthUpdate) => void;
  onReset: () => void;
  /**
   * The bulk tick for the rows underneath, when there are rows.
   *
   * Omitted by the skeleton, which has a grid of placeholders rather than
   * worktrees: a control that appears to select rows nobody has loaded yet is
   * worse than the blank cell this column used to hold in every state.
   */
  selectAll?: {
    state: SelectAllState;
    disabled: boolean;
    onToggle: () => void;
  };
}) {
  const boundary = (
    right: ResizableColumn,
    left?: ResizableColumn,
  ): BoundarySpec => ({ left, right, layout, widths, onResize, onReset });

  return (
    <div
      data-worktree-header
      role="row"
      style={{
        gridTemplateColumns: gridTemplate(layout),
        columnGap: COLUMN_GAP,
        paddingInline: ROW_PADDING_X,
      }}
      className="grid w-full items-center pb-1.5 text-[10px] font-medium tracking-wide text-muted-foreground uppercase"
    >
      <span role="columnheader" className="flex items-center">
        {selectAll ? (
          /*
            The bulk tick lives in the column the row checkboxes are already
            in, which is what makes its scope legible: it is the head of that
            column and nothing else. Its label says "visible" out loud, because
            the set it acts on is the filtered list rather than every worktree
            yawm knows about — and a control that silently meant "all" would be
            a destructive selection whose extent the reader cannot see.

            The primitive is the app's Checkbox, so Tab reaches it, Space
            toggles it, and a partial selection reports `aria-checked="mixed"`
            without this component having to spell any of that out. The label
            is repeated as `title` rather than as a tooltip: a Radix tooltip
            trigger with `asChild` would take over the `data-state` attribute
            this checkbox is styled from, so its ticked and mixed states would
            stop being drawn at all.
          */
          <Checkbox
            checked={selectAllChecked(selectAll.state)}
            disabled={selectAll.disabled}
            onCheckedChange={() => selectAll.onToggle()}
            aria-label={SELECT_ALL_LABEL}
            title={SELECT_ALL_LABEL}
            className="size-3.5 hover:border-ring"
          />
        ) : null}
      </span>
      <span role="columnheader" className="min-w-0 truncate">
        {showRepo ? "Repository / branch" : "Branch"}
      </span>

      {/*
        There is no trailing handle: the pane's right edge cannot move, and
        pretending otherwise makes that handle resize the Size column from the
        wrong side. Its leading divider already gives Size its full range.
      */}
      <Header column="status" label="Status" boundary={boundary("status")} />
      {/*
        Labels stay left-aligned even though their values are right-aligned, so
        each one sits against the divider that resizes it. Right-aligned, the
        label pinned itself to the far edge of its track and the divider
        appeared to float unattached in the gap — dragging it moved cells while
        the header it belonged to stayed put.
      */}
      {layout.showModified ? (
        <Header
          column="modified"
          label="Modified"
          boundary={boundary("modified", "status")}
        />
      ) : null}
      <Header
        column="size"
        label="Size"
        // With Modified dropped, Size borders Status instead. A handle has to
        // trade with whichever column is actually beside it, or dragging it
        // moves a track that is no longer on screen.
        boundary={boundary("size", layout.showModified ? "modified" : "status")}
      />
    </div>
  );
}

interface BoundarySpec {
  left?: ResizableColumn;
  right: ResizableColumn;
  layout: ColumnLayout;
  widths: ColumnWidths;
  onResize: (update: ColumnWidthUpdate) => void;
  onReset: () => void;
}

function Header({
  column,
  label,
  boundary,
}: {
  column: ResizableColumn;
  label: string;
  boundary: BoundarySpec;
}) {
  return (
    /*
      No `truncate` on this element. Tailwind's truncate includes
      overflow:hidden, and the handles are positioned outside this box — so
      they were clipped away entirely and had no hittable area at all.
      z-index cannot escape an overflow clip. The label truncates on its own
      child instead.
    */
    <span data-column={column} role="columnheader" className="relative min-w-0">
      <Grip spec={boundary} />
      <span className="block truncate">{label}</span>
    </span>
  );
}

function Grip({ spec }: { spec: BoundarySpec }) {
  /*
    Null while the pane is already compressing the stored widths. Saving a
    number the pane forced would leave that compressed value behind once the
    inspector closes, so the gesture is declined rather than half-honoured.
  */
  const range = () =>
    resizeRange(spec.layout, spec.widths, spec.right, spec.left, uiCharWidth(13));

  /**
   * Listeners on `window`, not on the handle.
   *
   * `setPointerCapture` is the tidier API, but it silently does nothing when
   * capture is refused, and that failure mode is a gesture which appears to
   * work and never moves anything. The window cannot fail that way, and it
   * keeps tracking when the cursor outruns an eight-pixel handle.
   */
  function startResize(e: React.PointerEvent<HTMLButtonElement>) {
    e.preventDefault();
    e.stopPropagation();
    const bounds = range();
    if (!bounds) return;

    const startX = e.clientX;
    const pointerId = e.pointerId;
    const cursor = document.body.style.cursor;
    const userSelect = document.body.style.userSelect;

    const move = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      ev.preventDefault();
      spec.onResize(
        applyResize(
          spec.widths,
          bounds,
          spec.right,
          spec.left,
          ev.clientX - startX,
        ),
      );
    };
    const end = (ev: PointerEvent) => {
      if (ev.pointerId !== pointerId) return;
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      document.body.style.cursor = cursor;
      document.body.style.userSelect = userSelect;
    };

    // Held for the whole drag so leaving a narrow target cannot make WebKit
    // select a label or flicker back to a text cursor.
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
  }

  return (
    <button
      type="button"
      onPointerDown={startResize}
      onDoubleClick={spec.onReset}
      onKeyDown={(e) => {
        if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
        const bounds = range();
        if (!bounds) return;
        e.preventDefault();
        spec.onResize(
          applyResize(
            spec.widths,
            bounds,
            spec.right,
            spec.left,
            e.key === "ArrowLeft" ? -8 : 8,
          ),
        );
      }}
      aria-label={`Resize boundary between ${spec.left ?? "branch"} and ${spec.right}`}
      title={
        spec.layout.compressed
          ? `Too narrow to resize — the branch needs its ${NAME_TARGET}px first`
          : "Drag to resize · double click to reset"
      }
      data-boundary={`${spec.left ?? "branch"}:${spec.right}`}
      style={{ left: -COLUMN_GAP, width: COLUMN_GAP }}
      className={cn(
        "absolute -top-1.5 z-10 h-7 touch-none appearance-none border-0 bg-transparent p-0 before:pointer-events-none before:absolute before:top-1 before:bottom-1 before:left-1/2 before:w-px before:-translate-x-1/2 before:bg-border/70 before:transition-colors focus-visible:outline-none focus-visible:before:bg-ring",
        spec.layout.compressed
          ? "cursor-default"
          : "cursor-col-resize hover:before:bg-muted-foreground",
      )}
    />
  );
}
