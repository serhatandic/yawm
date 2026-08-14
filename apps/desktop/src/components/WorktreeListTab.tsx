import { useEffect, useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { WorktreeRow, WorktreeRowHeader } from "@/components/WorktreeRow";
import {
  selectAllDisabled,
  selectAllState,
  toggleVisibleSelection,
} from "@/components/bulk-select";
import {
  COLUMN_GAP,
  type ColumnLayout,
  type ColumnWidths,
  fitColumns,
  ROW_PADDING_X,
  gridTemplate,
  uiCharWidth,
  useColumnWidths,
  usePaneWidth,
} from "@/lib/columns";
import { DetailPanel, DetailPanelSkeleton } from "@/components/DetailPanel";
import { VerdictDot } from "@/components/verdict";
import {
  VERDICT_HEADLINE,
  VERDICT_LABEL,
  api,
  type RepoReport,
  type Verdict,
  type Worktree,
  reclaimableBytes,
  worktreeLabel,
} from "@/lib/api";
import { cn, humanBytes, FOCUS_RING } from "@/lib/utils";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { Workspace } from "@/lib/api";
import type { ChangesAnchor, ReadingIntent } from "@/lib/tabs";
import {
  Eye,
  EyeOff,
  FolderPlus,
  Loader2,
  Search,
  Trash2,
  Wrench,
} from "lucide-react";

type Filter = "all" | Verdict;
/** The verdicts, in the order the chips and the legend both read them in. */
const VERDICTS: Verdict[] = ["disposable", "review", "keep", "broken"];
const FILTERS: Filter[] = ["all", ...VERDICTS];

export interface Located {
  repo: RepoReport;
  worktree: Worktree;
}

/**
 * The worktree list.
 *
 * The toolbar sits **above** the sidebar / list / inspector split rather than
 * inside the list pane. That was the cause of the collision and the clutter:
 * nested in the middle pane, the toolbar lost 320px the moment the inspector
 * opened, so it overflowed into a horizontal scroll and cut off its own
 * filters. Hoisted, its width no longer depends on what else is open.
 *
 * Search and the filters live here rather than in the title bar because they
 * only mean something for this view. Keeping them here is also what lets the
 * state survive: leaving and coming back finds the list as you left it.
 */
export function WorktreeListTab({
  reports,
  loading,
  measuring,
  analyzingLanding,
  switching,
  error,
  onRefresh,
  onAddRepo,
  onAddScanRoot,
  onOpenDiff,
  onDelete,
  onWorktreeUpdate,
  workspaces,
  activeWorkspace,
  onWorkspaceChange,
  hideMain,
  onHideMainChange,
}: {
  reports: RepoReport[] | null;
  loading: boolean;
  measuring: boolean;
  analyzingLanding: boolean;
  /** The rows on screen belong to the workspace being switched away from. */
  switching: boolean;
  error: string | null;
  onRefresh: () => void;
  onAddRepo: () => void;
  onAddScanRoot: () => void;
  onOpenDiff: (
    located: Located,
    open?: { intent?: ReadingIntent; anchor?: ChangesAnchor },
  ) => void;
  onDelete: (located: Located[]) => void;
  onWorktreeUpdate: (repoRoot: string, worktree: Worktree) => void;
  workspaces: Workspace[];
  activeWorkspace: string | null;
  onWorkspaceChange: (id: string | null) => void;
  hideMain: boolean;
  onHideMainChange: (hide: boolean) => void;
}) {
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [activeRepo, setActiveRepo] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [checked, setChecked] = useState<Set<string>>(new Set());
  const [pruning, setPruning] = useState(false);
  const { widths, resize, reset } = useColumnWidths();
  const { ref: paneRef, width: paneWidth } = usePaneWidth();
  // The tracks the list actually renders: the stored widths fitted to the pane
  // it has, rather than the pane it was saved in. The measured font width is
  // what turns "Size must not truncate" into a number of pixels to reserve.
  const layout = fitColumns(paneWidth, widths, uiCharWidth(13));

  const scoped = activeRepo;

  /**
   * The name to blame when the list is empty.
   *
   * With several workspaces, "No repositories yet" is a lie — the others may
   * be full. Naming the one you are looking at turns a dead end into a fact,
   * and only applies when a specific workspace is selected: on "All
   * workspaces" the original wording is the true one.
   */
  const emptyWorkspace =
    workspaces.length > 1 && activeWorkspace !== null
      ? (workspaces.find((w) => w.id === activeWorkspace)?.name ?? null)
      : null;

  const located = useMemo<Located[]>(
    () =>
      (reports ?? []).flatMap((repo) =>
        repo.worktrees.map((worktree) => ({ repo, worktree })),
      ),
    [reports],
  );

  /**
   * Drop scope state that no longer refers to anything.
   *
   * Switching workspaces — or deleting a repo, or a worktree vanishing on disk
   * — leaves `activeRepo` and `checked` pointing at rows that are no longer in
   * the list. A stale repo filter shows an empty list with no obvious way back,
   * and stale checks are worse than confusing: the bulk bar would offer to
   * delete worktrees you cannot see. So both are healed from the data rather
   * than reset on one specific trigger, which also covers deletions made
   * outside yawm.
   */
  useEffect(() => {
    if (reports === null) return; // still loading; nothing has gone away yet

    const roots = new Set(reports.map((r) => r.root));
    setActiveRepo((current) =>
      current !== null && !roots.has(current) ? null : current,
    );

    const paths = new Set(located.map((l) => l.worktree.path));
    setChecked((current) => {
      const kept = new Set([...current].filter((p) => paths.has(p)));
      return kept.size === current.size ? current : kept;
    });
  }, [reports, located]);

  /**
   * Applied before the counts, so the filter chips describe what is on screen
   * rather than what would be there if the main worktrees were shown.
   */
  const inScope = useMemo(
    () =>
      located.filter(
        ({ repo, worktree }) =>
          (!scoped || repo.root === scoped) && !(hideMain && worktree.isMain),
      ),
    [located, scoped, hideMain],
  );

  const visible = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return inScope.filter(({ repo, worktree }) => {
      if (filter !== "all" && worktree.verdict !== filter) return false;
      if (!needle) return true;
      return (
        worktreeLabel(worktree).toLowerCase().includes(needle) ||
        worktree.path.toLowerCase().includes(needle) ||
        repo.name.toLowerCase().includes(needle)
      );
    });
  }, [inScope, filter, query]);
  const selected =
    located.find((l) => l.worktree.path === selectedPath) ?? null;

  const counts = useMemo(() => {
    const out: Record<Filter, number> = {
      all: inScope.length,
      disposable: 0,
      review: 0,
      keep: 0,
      broken: 0,
    };
    for (const { worktree } of inScope) out[worktree.verdict] += 1;
    return out;
  }, [inScope]);

  /**
   * Totals describe the current scope, not the whole machine, so they are
   * labelled with it. Reclaimable is the outcome of the Disposable filter, so
   * it is shown on that chip rather than floating as separate telemetry.
   */
  const totals = useMemo(() => {
    let total = 0;
    let reclaimable = 0;
    for (const { worktree } of inScope) {
      total += worktree.status.size?.bytes ?? 0;
      reclaimable += reclaimableBytes(worktree);
    }
    return { total, reclaimable };
  }, [inScope]);

  const checkedItems = useMemo(
    () => located.filter((l) => checked.has(l.worktree.path)),
    [located, checked],
  );

  const brokenRepos = useMemo(
    () =>
      (reports ?? []).filter(
        (r) =>
          (!scoped || r.root === scoped) &&
          r.worktrees.some((w) => w.verdict === "broken"),
      ),
    [reports, scoped],
  );

  function toggle(path: string) {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  /**
   * What the header's tick acts on: the filtered list, and nothing behind it.
   *
   * Taken from `visible` rather than from `located` or `inScope`, so a search
   * or a verdict filter is part of the selection's scope rather than something
   * it steps around. The main worktrees are dropped by `bulk-select`, which is
   * the same rule the row checkboxes already enforce.
   */
  const visibleRows = useMemo(
    () => visible.map(({ worktree }) => worktree),
    [visible],
  );

  function toggleAllVisible() {
    setChecked((prev) => toggleVisibleSelection(prev, visibleRows));
  }

  async function prune() {
    setPruning(true);
    try {
      for (const repo of brokenRepos) await api.pruneRepo(repo.root);
      onRefresh();
    } finally {
      setPruning(false);
    }
  }

  const scopeLabel =
    (scoped
      ? (reports ?? []).find((r) => r.root === scoped)?.name
      : "all repositories") ?? "this repository";

  const activeWorkspaceName =
    workspaces.find((w) => w.id === activeWorkspace)?.name ?? "all workspaces";

  /**
   * How many rows are still waiting on the disk walk.
   *
   * Drives the loud indicator rather than `measuring` alone, because the full
   * pass also runs behind a switch whose sizes came straight out of core's
   * cache. Announcing "measuring" over a column of complete numbers trains
   * people to ignore the announcement; counting what is actually blank means
   * the message only appears when something really is missing.
   */
  const pendingSizes = useMemo(
    () =>
      inScope.filter(
        ({ worktree }) => worktree.status.size === null && !worktree.prunable,
      ).length,
    [inScope],
  );

  const pendingLanding = useMemo(
    () =>
      inScope.filter(({ worktree }) => !worktree.status.landingComplete).length,
    [inScope],
  );

  const busy =
    switching ||
    loading ||
    (measuring && pendingSizes > 0) ||
    (analyzingLanding && pendingLanding > 0);

  return (
    <div className="flex h-full min-h-0">
      {/*
        The sidebar runs floor to ceiling: it is persistent navigation, so it
        should not sit under a bar that filters something else.
      */}
      <Sidebar
        reports={reports ?? []}
        hideMain={hideMain}
        activeRepo={activeRepo}
        onSelectRepo={setActiveRepo}
        onAddRepo={onAddRepo}
        onAddScanRoot={onAddScanRoot}
        workspaces={workspaces}
        activeWorkspace={activeWorkspace}
        onWorkspaceChange={onWorkspaceChange}
      />

      <div className="flex min-w-0 flex-1 flex-col">
        {/*
          Belongs to the list it filters, so it starts where the list column
          starts. It still spans the inspector, because sitting above only the
          list would let the 320px inspector squeeze it into a collision.
        */}
        <div className="@container flex h-11 shrink-0 items-center gap-2 border-b border-border px-3">
          {/*
            The first thing to give way when the row runs out of room.

            It was fixed at 224px and refused to shrink, so the chips and the
            Main toggle collided into each other instead — and a filter you
            cannot read is worse than a search box that is narrower than it
            would like. It keeps enough width to still be usable.

            Fluid rather than a fixed width that merely permits shrinking: at
            wide widths it stops at the 224px it wants, and everywhere below
            that it gives up its own room first, which is what keeps the chips
            legible instead of collapsing them together.
          */}
          <div className="relative w-56 min-w-24 shrink">
            <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Filter these worktrees…"
              className="h-7 pl-7 text-xs"
            />
          </div>

          <div className="flex shrink-0 items-center gap-1">
            {FILTERS.map((value) => (
              <FilterChip
                key={value}
                value={value}
                count={counts[value]}
                active={filter === value}
                // The reclaimable total belongs to Disposable: it is what acting
                // on that filter would give you back, not passive telemetry.
                trailing={
                  value === "disposable" && totals.reclaimable > 0
                    ? humanBytes(totals.reclaimable)
                    : undefined
                }
                onClick={() => setFilter(value)}
              />
            ))}
          </div>

          {/*
          A modifier, not a fifth verdict, so it sits apart from the chips.
          Main worktrees can never be deleted, and on a machine with many
          repositories they are most of the rows and none of the choices.
        */}
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                onClick={() => onHideMainChange(!hideMain)}
                aria-pressed={hideMain}
                aria-label={
                  hideMain ? "Show main worktrees" : "Hide main worktrees"
                }
                className={cn(
                  "flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 text-xs transition-colors",
                  FOCUS_RING,
                  hideMain
                    ? "bg-muted text-foreground"
                    : "text-muted-foreground hover:bg-muted/60",
                )}
              >
                {hideMain ? (
                  <EyeOff className="size-3.5" />
                ) : (
                  <Eye className="size-3.5" />
                )}
                {/*
                  The word goes before the row does. Below roughly a phone's
                  width of toolbar the glyph and its tooltip say the same
                  thing in a quarter of the space, and every action stays
                  reachable — which a control shoved off the right edge is
                  not.
                */}
                <span className="hidden @[46rem]:inline">Main</span>
              </button>
            </TooltipTrigger>
            <TooltipContent>
              {hideMain
                ? "Showing only worktrees you can delete"
                : "Hide the repositories' own main worktrees"}
            </TooltipContent>
          </Tooltip>

          {/*
          Allowed to shrink, because it could not before: `shrink-0` on a row
          that already holds a search field, five filters and a toggle meant a
          long progress message simply ran off the right edge of the window
          rather than giving way.
        */}
          <div className="ml-auto flex min-w-0 items-center gap-3 text-[11px]">
            {/*
            The whole complaint was that switching felt stuck, and the only
            sign anything was happening was a 14px spinner in the title bar at
            the other end of the window. This sits where the eye already is —
            beside the total it is busy changing — and names the active pass,
            because stale rows, missing sizes, and unfinished proof are three
            different kinds of incomplete.
          */}
            {switching ? (
              <span className="flex min-w-0 items-center gap-1.5 text-foreground">
                <Loader2 className="size-3 shrink-0 animate-spin" />
                <span className="truncate">Loading {activeWorkspaceName}…</span>
              </span>
            ) : measuring && pendingSizes > 0 ? (
              <span className="flex min-w-0 items-center gap-1.5 text-review">
                <Loader2 className="size-3 shrink-0 animate-spin" />
                <span className="truncate">
                  Measuring{" "}
                  {pendingSizes === 1
                    ? "1 worktree"
                    : `${pendingSizes} worktrees`}{" "}
                  on disk…
                </span>
              </span>
            ) : analyzingLanding && pendingLanding > 0 ? (
              <span className="flex min-w-0 items-center gap-1.5 text-review">
                <Loader2 className="size-3 shrink-0 animate-spin" />
                {/*
                "Rewritten history" names the mechanism this pass copes with,
                which is of no use to someone watching a progress line and cost
                the width that pushed this row off the screen.
              */}
                <span className="truncate">
                  Checking{" "}
                  {pendingLanding === 1
                    ? "1 worktree"
                    : `${pendingLanding} worktrees`}
                  …
                </span>
              </span>
            ) : null}
            <span className="text-muted-foreground">
              <span
                className={cn(
                  "tabular-nums transition-opacity",
                  ((measuring && pendingSizes > 0) ||
                    (analyzingLanding && pendingLanding > 0)) &&
                    "opacity-50",
                )}
              >
                {humanBytes(totals.total)}
              </span>{" "}
              across {scopeLabel}
            </span>
          </div>
        </div>

        {/*
        A bar rather than only a spinner: it spans the list it is loading, so
        there is no way to look at the rows without seeing that they are still
        being worked on.
      */}
        <div
          className="h-0.5 shrink-0 overflow-hidden bg-transparent"
          aria-hidden
        >
          {busy ? <div className="yawm-indeterminate h-full w-full" /> : null}
        </div>

        <div className="flex min-h-0 flex-1">
          <main className="flex min-w-0 flex-1 flex-col">
            {brokenRepos.length > 0 ? (
              <div className="mx-3 mt-2 flex shrink-0 items-center gap-2 rounded-md border border-broken/40 bg-broken/10 px-3 py-1.5">
                <Wrench className="size-3.5 shrink-0 text-broken" />
                <p className="min-w-0 flex-1 text-[11px]">
                  {counts.broken === 1
                    ? "1 worktree is missing its directory."
                    : `${counts.broken} worktrees are missing their directories.`}{" "}
                  Pruning clears the leftover git metadata — no files are
                  touched.
                </p>
                <Button
                  size="xs"
                  variant="secondary"
                  onClick={prune}
                  disabled={pruning}
                >
                  {pruning ? <Loader2 className="size-3 animate-spin" /> : null}
                  Prune
                </Button>
              </div>
            ) : null}

            {/*
              Horizontal overflow here would make the inspector move the Size
              column off-screen. The layout is fitted to this element's measured
              width instead, so it always fits and this element remains
              responsible only for the list's vertical scroll.
            */}
            <div className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto">
              <div
                ref={paneRef}
                aria-busy={switching}
                className={cn(
                  "px-3 py-2 transition-opacity",
                  // Stale rows are kept rather than blanked, but they belong to
                  // the workspace being left, so they must not be clickable.
                  switching && "pointer-events-none opacity-40",
                )}
              >
                {error ? (
                  <EmptyState
                    title="Something went wrong"
                    body={error}
                    action={
                      <Button size="sm" onClick={onRefresh}>
                        Try again
                      </Button>
                    }
                  />
                ) : reports === null || (loading && located.length === 0) ? (
                  <WorktreeListSkeleton
                    showRepo={!scoped}
                    layout={layout}
                    widths={widths}
                    onResize={resize}
                    onReset={reset}
                  />
                ) : located.length === 0 ? (
                  <EmptyState
                    title={
                      emptyWorkspace
                        ? `${emptyWorkspace} is empty`
                        : "No repositories yet"
                    }
                    body={
                      emptyWorkspace
                        ? `Add a repository to ${emptyWorkspace}, or point it at a folder to search. Your other workspaces are unaffected.`
                        : "Add a repository, or point yawm at a folder to search."
                    }
                    /*
                      The promise, and the vocabulary it is kept in — but only
                      on the screen where nothing else is competing for the
                      space, and only when the emptiness is the first run
                      rather than one workspace of several. A named workspace
                      being empty is a fact about that workspace, and burying
                      it under an explanation of the whole product would make
                      the true sentence the smaller one.
                    */
                    lead={
                      emptyWorkspace
                        ? undefined
                        : "yawm reads every worktree you have and says which ones are safe to delete."
                    }
                    legend={!emptyWorkspace}
                    action={
                      <div className="flex gap-2">
                        <Button size="sm" onClick={onAddRepo}>
                          Add a repo
                        </Button>
                        <Button
                          size="sm"
                          variant="secondary"
                          onClick={onAddScanRoot}
                        >
                          Scan a folder
                        </Button>
                      </div>
                    }
                  />
                ) : visible.length === 0 ? (
                  <EmptyState
                    title="Nothing matches"
                    body="Try a different filter or search."
                    action={
                      filter !== "all" || query.trim() !== "" ? (
                        <Button
                          size="sm"
                          variant="secondary"
                          onClick={() => {
                            /*
                              The two things this screen is about, and nothing
                              else. Hiding the main worktrees is a standing
                              preference set in another control and shared with
                              the sidebar's counts; sweeping it up here would
                              repopulate the list with rows that cannot be
                              deleted and silently disagree with the sidebar.
                            */
                            setFilter("all");
                            setQuery("");
                          }}
                        >
                          Clear filters
                        </Button>
                      ) : undefined
                    }
                  />
                ) : (
                  <div role="grid" aria-label="Worktrees">
                    <WorktreeRowHeader
                      showRepo={!scoped}
                      layout={layout}
                      widths={widths}
                      onResize={resize}
                      onReset={reset}
                      selectAll={{
                        state: selectAllState(visibleRows, checked),
                        disabled: selectAllDisabled(visibleRows),
                        onToggle: toggleAllVisible,
                      }}
                    />
                    <Rows
                      items={visible}
                      showRepo={!scoped}
                      layout={layout}
                      selectedPath={selectedPath}
                      checked={checked}
                      sizesPending={measuring}
                      landingPending={measuring || analyzingLanding}
                      onSelect={setSelectedPath}
                      onToggle={toggle}
                      onOpenUncommitted={(located) =>
                        onOpenDiff(located, { intent: "everything", anchor: "uncommitted" })
                      }
                    />
                  </div>
                )}
              </div>
            </div>

            {checkedItems.length > 0 ? (
              <div className="flex shrink-0 items-center gap-3 border-t border-border bg-card px-4 py-2">
                <span className="text-xs">
                  <span className="font-medium tabular-nums">
                    {checkedItems.length}
                  </span>{" "}
                  selected
                  {/*
                    "Frees", not "reclaims". Reclaimable is a judgement — it is
                    what the Disposable rows would give back — and this is the
                    size of whatever has been ticked, including rows the app is
                    arguing against deleting. Saying "reclaims" of a Keep row
                    would put the app's own recommendation behind a selection
                    it did not make.
                  */}
                  <span className="text-muted-foreground">
                    {" · frees "}
                    {humanBytes(
                      checkedItems.reduce(
                        (sum, l) => sum + (l.worktree.status.size?.bytes ?? 0),
                        0,
                      ),
                    )}
                  </span>
                </span>
                <div className="ml-auto flex items-center gap-2">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setChecked(new Set())}
                  >
                    Clear
                  </Button>
                  <Button
                    variant="destructive"
                    size="sm"
                    onClick={() => onDelete(checkedItems)}
                  >
                    <Trash2 className="size-3" />
                    Delete selected
                  </Button>
                </div>
              </div>
            ) : null}
          </main>

          {selected ? (
            <DetailPanel
              repo={selected.repo}
              worktree={selected.worktree}
              onClose={() => setSelectedPath(null)}
              onDelete={() => onDelete([selected])}
              onShowDiff={() => onOpenDiff(selected)}
              onShowUncommitted={() =>
                onOpenDiff(selected, {
                  intent: "everything",
                  anchor: "uncommitted",
                })
              }
              onWorktreeUpdate={onWorktreeUpdate}
              allowPrefetch={!measuring && !analyzingLanding && !switching}
            />
          ) : selectedPath !== null && loading ? (
            <DetailPanelSkeleton />
          ) : null}
        </div>
      </div>
    </div>
  );
}

function WorktreeListSkeleton({
  showRepo,
  layout,
  widths,
  onResize,
  onReset,
}: {
  showRepo: boolean;
  layout: ColumnLayout;
  widths: ColumnWidths;
  onResize: Parameters<typeof WorktreeRowHeader>[0]["onResize"];
  onReset: () => void;
}) {
  return (
    <div role="grid" aria-label="Loading worktrees" aria-busy>
      <WorktreeRowHeader
        showRepo={showRepo}
        layout={layout}
        widths={widths}
        onResize={onResize}
        onReset={onReset}
      />
      {/* Shapes, not rows: nothing here has a value to read out yet. */}
      <div className="space-y-0.5" aria-hidden>
        {Array.from({ length: 10 }).map((_, index) => (
          <div
            key={index}
            style={{
              gridTemplateColumns: gridTemplate(layout),
              columnGap: COLUMN_GAP,
              paddingInline: ROW_PADDING_X,
            }}
            className={cn(
              "grid w-full items-center",
              layout.stacked && showRepo ? "h-11" : "h-8",
            )}
          >
            <Skeleton className="size-3.5 rounded-sm" />
            {/* Two bars when the rows themselves will be two lines, so the
                placeholder does not promise a shape the data will not take. */}
            {layout.stacked && showRepo ? (
              <span className="flex min-w-0 flex-col gap-1">
                <Skeleton
                  className={cn(
                    "h-3 rounded-sm",
                    index % 3 === 0 ? "w-44" : "w-32",
                  )}
                />
                <Skeleton className="h-2 w-20 rounded-sm" />
              </span>
            ) : (
              <span className="flex min-w-0 items-center gap-1">
                {showRepo ? (
                  <Skeleton className="h-3 w-16 shrink-0 rounded-sm" />
                ) : null}
                <Skeleton
                  className={cn(
                    "h-3 min-w-0 rounded-sm",
                    index % 3 === 0 ? "w-36" : "w-24",
                  )}
                />
              </span>
            )}
            <span className="flex min-w-0 items-center gap-1.5">
              <Skeleton className="h-[18px] w-[74px] shrink-0 rounded" />
              {index % 3 === 1 ? (
                <span className="flex shrink-0 gap-1.5">
                  <Skeleton className="size-3 rounded-full" />
                  <Skeleton className="size-3 rounded-full" />
                </span>
              ) : null}
            </span>
            {layout.showModified ? (
              <Skeleton className="h-3 w-10 rounded-sm" />
            ) : null}
            <Skeleton className="h-3 w-12 rounded-sm" />
          </div>
        ))}
      </div>
    </div>
  );
}

function FilterChip({
  value,
  count,
  active,
  trailing,
  onClick,
}: {
  value: Filter;
  count: number;
  active: boolean;
  trailing?: string;
  onClick: () => void;
}) {
  const chip = (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 text-xs transition-colors",
        FOCUS_RING,
        active
          ? "bg-muted text-foreground"
          : "text-muted-foreground hover:bg-muted/60",
      )}
    >
      {value !== "all" ? <VerdictDot verdict={value} /> : null}
      {value === "all" ? "All" : VERDICT_LABEL[value]}
      <span className="tabular-nums opacity-60">{count}</span>
      {trailing ? (
        <span className="tabular-nums text-disposable">{trailing}</span>
      ) : null}
    </button>
  );

  /*
    What the word means, in the app's own words.

    Four verdicts is a vocabulary, and a one-word chip teaches none of it: the
    difference between Review and Keep is the whole product, and it was
    discoverable only by clicking a filter and inferring from what survived.
    The sentence is the one the detail panel already uses for that verdict, so
    the chip and the panel cannot come to disagree.
  */
  if (value === "all") return chip;
  return (
    <Tooltip>
      <TooltipTrigger asChild>{chip}</TooltipTrigger>
      <TooltipContent side="bottom">
        {VERDICT_HEADLINE[value]}
        {trailing ? ` · ${trailing} to reclaim` : null}
      </TooltipContent>
    </Tooltip>
  );
}

function Sidebar({
  reports,
  hideMain,
  activeRepo,
  onSelectRepo,
  onAddRepo,
  onAddScanRoot,
  workspaces,
  activeWorkspace,
  onWorkspaceChange,
}: {
  reports: RepoReport[];
  /** Mirrors the list's filter, so the two counts cannot disagree. */
  hideMain: boolean;
  activeRepo: string | null;
  onSelectRepo: (root: string | null) => void;
  onAddRepo: () => void;
  onAddScanRoot: () => void;
  workspaces: Workspace[];
  activeWorkspace: string | null;
  onWorkspaceChange: (id: string | null) => void;
}) {
  return (
    <aside className="flex w-52 shrink-0 flex-col border-r border-border">
      {/*
        The workspace switcher sits above the repository list because it scopes
        it: everything below belongs to the workspace chosen here.

        Height is pinned to h-11 to match the filter bar across the divide, so
        the two rules read as one line. Pinning the row rather than relying on
        padding plus the control's height means neither can drift the other.
      */}
      <div className="flex h-11 shrink-0 items-center border-b border-border px-2">
        <Select
          value={activeWorkspace ?? "__all__"}
          onValueChange={(v) => onWorkspaceChange(v === "__all__" ? null : v)}
        >
          {/*
            SelectTrigger sizes itself with data-[size=default]:h-9, an
            attribute selector that out-specifies a plain h-7 — so the height
            has to be set the same way to land at the search field's height.
          */}
          <SelectTrigger className="w-full text-xs data-[size=default]:h-7">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {workspaces.map((ws) => (
              <SelectItem key={ws.id} value={ws.id}>
                {ws.name}
              </SelectItem>
            ))}
            {workspaces.length > 1 ? (
              <SelectItem value="__all__">All workspaces</SelectItem>
            ) : null}
          </SelectContent>
        </Select>
      </div>
      <div className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto">
        <div className="p-2">
          <SidebarItem
            label="All repositories"
            count={reports.reduce((n, r) => n + countable(r, hideMain), 0)}
            active={activeRepo === null}
            onClick={() => onSelectRepo(null)}
          />
          <Separator className="my-2" />
          {/*
            A repository with nothing left to show is dropped rather than
            listed as a zero: every one of those rows is a click that leads to
            an empty list.
          */}
          {reports
            .filter((repo) => countable(repo, hideMain) > 0)
            .map((repo) => (
              <SidebarItem
                key={repo.root}
                label={repo.name}
                title={repo.root}
                count={countable(repo, hideMain)}
                active={activeRepo === repo.root}
                onClick={() => onSelectRepo(repo.root)}
              />
            ))}
        </div>
      </div>

      <div className="shrink-0 space-y-1 border-t border-border p-2">
        <Button
          variant="ghost"
          size="sm"
          className="h-7 w-full justify-start px-2 text-xs"
          onClick={onAddRepo}
        >
          <FolderPlus className="size-3.5" />
          Add a repo
        </Button>
        <Button
          variant="ghost"
          size="sm"
          className="h-7 w-full justify-start px-2 text-xs"
          onClick={onAddScanRoot}
        >
          <Search className="size-3.5" />
          Scan a folder
        </Button>
      </div>
    </aside>
  );
}

function SidebarItem({
  label,
  title,
  count,
  active,
  onClick,
}: {
  label: string;
  title?: string;
  count: number;
  active: boolean;
  onClick: () => void;
}) {
  const button = (
    <button
      onClick={onClick}
      aria-current={active ? "true" : undefined}
      className={cn(
        "flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-xs",
        FOCUS_RING,
        active
          ? "bg-muted text-foreground"
          : "text-muted-foreground hover:bg-muted/60",
      )}
    >
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <span className="shrink-0 tabular-nums opacity-60">{count}</span>
    </button>
  );

  if (!title) return button;
  return (
    <Tooltip>
      <TooltipTrigger asChild>{button}</TooltipTrigger>
      <TooltipContent side="right" className="max-w-sm">
        <p className="break-all">{title}</p>
      </TooltipContent>
    </Tooltip>
  );
}

/**
 * The rows.
 *
 * Flat, deliberately. Grouping by repository put an uppercase header above
 * almost every row — sixteen of them for twenty-one worktrees — spending more
 * height on labels than data, and walling the list into sections you cannot
 * sort across. The repository now rides along in each row instead.
 */
function Rows({
  items,
  showRepo,
  layout,
  selectedPath,
  checked,
  sizesPending,
  landingPending,
  onSelect,
  onToggle,
  onOpenUncommitted,
}: {
  items: Located[];
  showRepo: boolean;
  layout: ColumnLayout;
  selectedPath: string | null;
  checked: Set<string>;
  /** A disk walk is running, so a blank size is pending rather than absent. */
  sizesPending: boolean;
  /** Separates a progressing proof from one that failed and will not settle. */
  landingPending: boolean;
  onSelect: (path: string) => void;
  onToggle: (path: string) => void;
  /** The dirty count is a control, so the list has to know where it leads. */
  onOpenUncommitted: (located: Located) => void;
}) {
  return (
    <div role="rowgroup" className="space-y-px">
      {items.map((located) => {
        const { repo, worktree } = located;
        return (
        <WorktreeRow
          key={worktree.path}
          worktree={worktree}
          repoName={showRepo ? repo.name : undefined}
          layout={layout}
          selected={selectedPath === worktree.path}
          selectable={!worktree.isMain}
          checked={checked.has(worktree.path)}
          sizePending={
            sizesPending && worktree.status.size === null && !worktree.prunable
          }
          landingPending={landingPending}
          onSelect={() => onSelect(worktree.path)}
          onToggle={() => onToggle(worktree.path)}
          onOpenUncommitted={() => {
            onSelect(worktree.path);
            onOpenUncommitted(located);
          }}
        />
        );
      })}
    </div>
  );
}

function EmptyState({
  title,
  body,
  lead,
  legend,
  action,
}: {
  title: string;
  body: string;
  /** The product's promise, on the one screen where nothing else is on it. */
  lead?: string;
  /** The four words the whole list is written in, stated once. */
  legend?: boolean;
  action?: React.ReactNode;
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-20 text-center">
      <div className="space-y-1">
        <p className="font-medium">{title}</p>
        {lead ? (
          <p className="max-w-sm text-xs text-muted-foreground">{lead}</p>
        ) : null}
        <p className="max-w-sm text-xs text-muted-foreground">{body}</p>
      </div>
      {legend ? <VerdictLegend /> : null}
      {action}
    </div>
  );
}

/**
 * The four verdicts, once, in the app's own words.
 *
 * A dense static row rather than a set of cards: it is a key to a vocabulary,
 * not content, and it appears only where there is nothing else to read. The
 * words are `VERDICT_LABEL` and the sentences are `VERDICT_HEADLINE` — the
 * same two the chips, the badges and the detail panel use, so the legend
 * cannot teach something the list does not then say.
 */
function VerdictLegend() {
  return (
    <ul className="grid grid-cols-2 gap-x-5 gap-y-1 text-left text-[11px]">
      {VERDICTS.map((verdict) => (
        <li key={verdict} className="flex items-center gap-1.5">
          <VerdictDot verdict={verdict} />
          <span className="font-medium">{VERDICT_LABEL[verdict]}</span>
          <span className="text-muted-foreground">
            {VERDICT_HEADLINE[verdict]}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** Worktrees in a repository that the list is currently willing to show. */
function countable(repo: RepoReport, hideMain: boolean): number {
  if (!hideMain) return repo.worktrees.length;
  return repo.worktrees.filter((w) => !w.isMain).length;
}
