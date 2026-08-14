import { Button } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { verdictZoneClass } from "@/components/verdict";
import { RiskIcon, riskSentence, risksOf } from "@/components/risks";
import {
  actionLayout,
  CLOSE_DETAILS_LABEL,
} from "@/components/detail-panel";
import {
  VERDICT_HEADLINE,
  api,
  type Editor,
  type RepoReport,
  type Worktree,
  reasonDetail,
  worktreeLabel,
} from "@/lib/api";
import { cn, humanBytes, relativeTime, FOCUS_RING } from "@/lib/utils";
import { monoCharWidth, usePanelWidth } from "@/lib/columns";
import { middleTruncate } from "@/lib/layout";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  CheckCircle2,
  ExternalLink,
  FileDiff,
  FolderOpen,
  Link2,
  Loader2,
  Trash2,
  X,
} from "lucide-react";

/**
 * The case for or against deleting one worktree.
 *
 * Not a properties sheet. An earlier version listed everything yawm knew with
 * roughly equal weight, which left the reader to work out for themselves which
 * of fifteen facts mattered — at the exact moment they were about to destroy
 * something. This one leads with a judgement, supports it with only the facts
 * that are actually at stake, and states the reward last, next to the button
 * that collects it.
 *
 * Nothing truncates. In a destructive workflow the identifying details are the
 * whole point, and `fix-auth-token-refresh…` is indistinguishable from
 * `fix-auth-token-caching`. Long names wrap instead.
 */
export function DetailPanel({
  repo,
  worktree,
  onClose,
  onDelete,
  onShowDiff,
  onShowUncommitted,
  onWorktreeUpdate,
  allowPrefetch,
}: {
  repo: RepoReport;
  worktree: Worktree;
  onClose: () => void;
  onDelete: () => void;
  onShowDiff: () => void;
  /** Opens only the work that exists on disk, which is what the risk names. */
  onShowUncommitted: () => void;
  onWorktreeUpdate: (repoRoot: string, worktree: Worktree) => void;
  allowPrefetch: boolean;
}) {
  const { status } = worktree;
  const heavy = status.size?.heavyDirs ?? [];
  const { width, resize, reset: resetWidth } = usePanelWidth();
  // The panel's padding, minus the room the byte figure beside a path needs.
  const pathChars = Math.max(
    12,
    Math.floor((width - 32 - 72) / monoCharWidth(11)),
  );

  /**
   * Listeners on the window, not the handle: the pointer routinely outruns a
   * two-pixel target, and a drag that silently stops halfway is worse than one
   * that never starts.
   */
  function startResize(e: React.PointerEvent<HTMLSpanElement>) {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = width;

    // The panel is on the right, so dragging left widens it.
    const move = (ev: PointerEvent) => resize(startWidth - (ev.clientX - startX));
    const end = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      document.body.style.cursor = "";
    };

    document.body.style.cursor = "col-resize";
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", end);
    window.addEventListener("pointercancel", end);
  }
  const risks = risksOf(worktree);
  const safe = worktree.verdict === "disposable";
  /*
   * The layout is a function of this panel's width, not the window's.
   *
   * The panel is dragged to whatever width the reader wants, so a media query
   * would be answering a question about the screen while the actions overflow
   * a 300px column.
   */
  const actions = actionLayout(width);
  const showChanges = !worktree.isMain && !worktree.prunable;
  const [landingCheck, setLandingCheck] = useState<
    "idle" | "checking" | "failed"
  >("idle");
  const focusedPrefetchKey = useRef<string | null>(null);

  const prefetchFocus = useCallback(() => {
    if (!allowPrefetch) return;
    const key = `${repo.root}\0${worktree.path}\0${worktree.head ?? ""}`;
    if (focusedPrefetchKey.current === key) return;
    focusedPrefetchKey.current = key;
    void api
      .prefetchFocusedWorktree(repo.root, worktree.path)
      .catch(() => undefined);
  }, [allowPrefetch, repo.root, worktree.head, worktree.path]);

  useEffect(() => {
    if (
      worktree.isMain ||
      worktree.prunable ||
      worktree.status.landingComplete
    ) {
      setLandingCheck("idle");
      return;
    }

    let current = true;
    setLandingCheck("checking");
    void api
      .inspectWorktree(repo.root, worktree.path)
      .then((updated) => {
        if (!current) return;
        onWorktreeUpdate(repo.root, updated);
        setLandingCheck("idle");
        prefetchFocus();
      })
      .catch(() => {
        if (current) setLandingCheck("failed");
      });
    return () => {
      current = false;
    };
  }, [
    onWorktreeUpdate,
    prefetchFocus,
    repo.root,
    worktree.head,
    worktree.isMain,
    worktree.path,
    worktree.prunable,
    worktree.status.landingComplete,
  ]);

  useEffect(() => {
    if (
      worktree.isMain ||
      worktree.prunable ||
      worktree.status.landing.state === "unknown"
    ) {
      return;
    }

    // Selection is the strongest signal that Changes is next. Keeping this on
    // the speculative lane lets a rapid second selection pass it immediately.
    prefetchFocus();
  }, [
    repo.root,
    prefetchFocus,
    worktree.head,
    worktree.isMain,
    worktree.path,
    worktree.prunable,
    worktree.status.landing.state,
  ]);

  return (
    <aside
      style={{ width, minWidth: width }}
      className="relative flex shrink-0 flex-col overflow-hidden border-l border-border"
    >
      {/*
        The grab area straddles the border rather than sitting inside the
        panel, so the cursor changes where the eye already sees an edge.
        Double click restores the default, which is the way back out of a
        panel dragged too far.

        Full height, because it is the only thing on this boundary again. It
        briefly shared the edge with a close button, which meant the top of the
        panel was a drag that silently refused to start — the button won the
        click — and the button itself hung off the boundary where the panel
        clipped it. The close control now lives in the content, where it can be
        seen whole.
      */}
      <span
        onPointerDown={startResize}
        onDoubleClick={resetWidth}
        title="Drag to resize · double click to reset"
        className="absolute top-0 -left-1 z-10 h-full w-2 cursor-col-resize hover:bg-border"
      />

      {/*
        The verdict as a full-bleed block rather than a badge in a row. It is
        the panel's headline, and everything below is read through it.
      */}
      <div
        className={cn(
          "relative shrink-0 px-4 py-3",
          verdictZoneClass(worktree.verdict),
        )}
      >
        <div className="flex items-center gap-2">
          {safe ? (
            <CheckCircle2 className="size-4 shrink-0" />
          ) : (
            <AlertTriangle className="size-4 shrink-0" />
          )}
          <h2 className="text-sm font-medium">
            {VERDICT_HEADLINE[worktree.verdict]}
          </h2>
        </div>
        <p className="mt-1 text-xs text-foreground/80">
          {reasonDetail(worktree.reason)}
        </p>
        {landingCheck !== "idle" ? (
          <p
            aria-live="polite"
            className="mt-1.5 flex items-center gap-1.5 text-[11px] text-foreground/70"
          >
            {landingCheck === "checking" ? (
              <>
                <Loader2 className="size-3 animate-spin" />
                Checking rewritten history…
              </>
            ) : (
              "The deeper landing check could not finish."
            )}
          </p>
        ) : null}
      </div>

      <div className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto">
        <div className="space-y-4 px-4 py-4">
          <section className="flex items-start gap-2">
            <div className="min-w-0 flex-1">
              <p className="text-[11px] text-muted-foreground">{repo.name}</p>
              {/* Wraps. A half-shown branch name identifies nothing. */}
              <p className="text-sm font-medium break-all">
                {worktreeLabel(worktree)}
              </p>
              {/*
                Said explicitly, because the title above is the worktree and a
                worktree is not always a branch. A detached one showed no branch
                at all and nothing said so.
              */}
              <p className="mt-0.5 text-[11px] text-muted-foreground">
                {worktree.branch ? (
                  <>
                    on branch{" "}
                    <span className="font-mono text-foreground">
                      {worktree.branch}
                    </span>
                  </>
                ) : (
                  "detached — not on any branch"
                )}
              </p>
              {status.lastCommitSubject ? (
                /*
                  Two lines, then stop. A commit subject is human language, so
                  it wraps rather than truncating from the middle — but an essay
                  in the header pushes the risks that matter below the fold, and
                  the title carries the rest for anyone who wants it.
                */
                <p
                  title={status.lastCommitSubject}
                  className="mt-1.5 line-clamp-2 text-[11px] break-words text-muted-foreground"
                >
                  {status.lastCommitSubject}
                  <span className="opacity-60">
                    {" · "}
                    {relativeTime(status.lastCommitAt)}
                  </span>
                </p>
              ) : null}
            </div>

            {/*
              An ordinary close, at the top right of the thing it closes.

              It has been three other things: an X inside the coloured verdict
              block, where a neutral glyph took the colour of a judgement and
              read as "dismiss this warning"; a rail of its own, 28px of blank
              width holding one button and pushing the verdict down; and a
              collapse glyph hung on the panel's boundary, where the panel's
              own `overflow-hidden` clipped it in half and it stole the top of
              the resize edge. This is the plain answer: a close button, in the
              panel's first content section, beside the name of what it closes.
            */}
            <Button
              variant="ghost"
              size="icon"
              onClick={onClose}
              aria-label={CLOSE_DETAILS_LABEL}
              title={CLOSE_DETAILS_LABEL}
              className="-mt-1 -mr-1 size-6 shrink-0 text-muted-foreground hover:text-foreground"
            >
              <X className="size-3.5 shrink-0" />
            </Button>
          </section>

          {/*
            Here, so that a name you do not recognise is one click from the
            code — and laid out for the width the panel actually has.

            Three controls in one row fit the default width and nothing
            narrower: dragged to its minimum, the labels collided, the icons
            crossed their buttons' edges and the split trigger stood outside
            its own border. At that width the primary action takes a row of its
            own and the two secondary ones share the next, which is the same
            information in the space available rather than the same row made
            illegible.
          */}
          {actions === "row" ? (
            <div className="flex gap-1.5">
              <OpenAction path={worktree.path} />
              <Action
                icon={<FolderOpen className="size-3 shrink-0" />}
                label="Reveal"
                onClick={() => void api.revealPath(worktree.path)}
              />
              {showChanges ? (
                <Action
                  icon={<FileDiff className="size-3 shrink-0" />}
                  label="Changes"
                  onClick={onShowDiff}
                />
              ) : null}
            </div>
          ) : (
            <div className="space-y-1.5">
              <div className="flex">
                <OpenAction path={worktree.path} />
              </div>
              <div className="flex gap-1.5">
                <Action
                  icon={<FolderOpen className="size-3 shrink-0" />}
                  label="Reveal"
                  onClick={() => void api.revealPath(worktree.path)}
                />
                {showChanges ? (
                  <Action
                    icon={<FileDiff className="size-3 shrink-0" />}
                    label="Changes"
                    onClick={onShowDiff}
                  />
                ) : null}
              </div>
            </div>
          )}

          <Separator />

          {/*
            Only what is at stake. No "ahead / behind 0 / 0", no "merged: no" —
            a row that always says nothing trains you to read past the rows
            that do.
          */}
          <section>
            <h3 className="mb-2 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
              {risks.length > 0 ? "At risk" : "Nothing at risk"}
            </h3>
            {risks.length === 0 ? (
              <p className="text-[11px] text-muted-foreground">
                Everything here exists somewhere else. Deleting the directory
                loses no work.
              </p>
            ) : null}

            {risks.length === 0 ? null : (
              <ul className="space-y-1">
                {risks.map((risk) => (
                  <li
                    key={risk.kind}
                    /*
                      One line each, and nothing under it.

                      Every flag used to carry a detail sentence and up to two
                      raw diff fragments, which turned five facts into a
                      half-screen of prose and code and buried the one that
                      could be acted on. The facts are unchanged — the row
                      states each risk and its count, and the full sentence is
                      the row's title — but the list reads as a list again.
                    */
                    title={riskSentence(risk)}
                    className="flex items-center gap-2 text-xs"
                  >
                    <RiskIcon risk={risk} className="size-3.5" />
                    {/*
                      The one risk that is entirely about this directory is the
                      one you can go and look at, so it keeps its link to
                      exactly the work it counts and to nothing else.
                    */}
                    {risk.kind === "uncommitted" ? (
                      <button
                        type="button"
                        onClick={onShowUncommitted}
                        className={cn(
                          "min-w-0 flex-1 truncate rounded-sm text-left underline-offset-2 hover:underline",
                          FOCUS_RING,
                        )}
                      >
                        {risk.label}
                      </button>
                    ) : (
                      <span className="min-w-0 flex-1 truncate">
                        {risk.label}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>

          {heavy.length > 0 ? (
            <>
              <Separator />
              <section>
                <h3 className="mb-2 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
                  What is taking the space
                </h3>
                <ul className="space-y-1">
                  {heavy.map((dir) => (
                    <li
                      key={dir.name}
                      className="flex items-center gap-2 text-[11px]"
                    >
                      {/*
                        Shortened from the middle. These are paths, and the
                        filename at the end is the part that says what the
                        directory is — `node_modules/.pnpm/@types+react…` cut at
                        the front tells you nothing you did not already know.
                      */}
                      <span
                        title={dir.name}
                        className="min-w-0 flex-1 font-mono text-muted-foreground"
                      >
                        {middleTruncate(dir.name, pathChars, 20)}
                      </span>
                      {dir.isLink ? (
                        <span
                          className="flex shrink-0 items-center gap-1 text-muted-foreground"
                          title="Linked to another worktree — deleting reclaims nothing"
                        >
                          <Link2 className="size-2.5" />
                          linked
                        </span>
                      ) : (
                        <span className="shrink-0 tabular-nums text-muted-foreground">
                          {humanBytes(dir.bytes)}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              </section>
            </>
          ) : null}
        </div>
      </div>

      {!worktree.isMain ? (
        <div className="shrink-0 border-t border-border p-3">
          {/* The reward, next to the button that collects it. */}
          <p className="mb-2 text-center text-[11px] text-muted-foreground">
            Frees {humanBytes(status.size?.bytes ?? null)}
          </p>
          {/*
            A verdict of anything but Disposable makes this an override, so it
            stops looking like the obvious next step. The confirmation dialog
            still does the real work; this is about not inviting the click.
          */}
          <Button
            variant={safe ? "destructive" : "outline"}
            className={cn(
              "w-full justify-center",
              !safe && "border-broken/40 text-broken hover:bg-broken/10",
            )}
            onClick={onDelete}
          >
            <Trash2 className="size-3.5" />
            {worktree.prunable
              ? "Prune"
              : safe
                ? "Delete worktree"
                : "Delete anyway"}
          </Button>
        </div>
      ) : (
        <div className="shrink-0 border-t border-border p-3">
          <p className="text-center text-[11px] text-muted-foreground">
            The main worktree cannot be deleted.
          </p>
        </div>
      )}
    </aside>
  );
}

export function DetailPanelSkeleton() {
  const { width } = usePanelWidth();

  return (
    <aside
      style={{ width, minWidth: width }}
      className="flex shrink-0 flex-col overflow-hidden border-l border-border"
      aria-label="Loading worktree details"
    >
      <div className="shrink-0 bg-muted/50 px-4 py-3">
        <div className="flex items-center gap-2 pr-8">
          <Skeleton className="size-4 rounded-full" />
          <Skeleton className="h-4 w-32" />
        </div>
        <Skeleton className="mt-2 h-3 w-full" />
        <Skeleton className="mt-1.5 h-3 w-4/5" />
      </div>

      <div className="min-h-0 flex-1 px-4 py-4">
        <section>
          <Skeleton className="h-3 w-20" />
          <Skeleton className="mt-1.5 h-4 w-48" />
          <Skeleton className="mt-2 h-3 w-36" />
          <Skeleton className="mt-2 h-3 w-5/6" />
        </section>

        <div className="mt-4 flex gap-1.5">
          <Skeleton className="h-7 flex-1" />
          <Skeleton className="h-7 flex-1" />
          <Skeleton className="h-7 flex-1" />
        </div>

        <Separator className="my-4" />

        <section>
          <Skeleton className="mb-2 h-3 w-24" />
          <div className="space-y-2">
            <Skeleton className="h-3 w-full" />
            <Skeleton className="h-3 w-4/5" />
          </div>
        </section>

        <Separator className="my-4" />

        <section>
          <Skeleton className="mb-2 h-3 w-36" />
          <div className="space-y-2">
            <Skeleton className="h-3 w-5/6" />
            <Skeleton className="h-3 w-2/3" />
          </div>
        </section>
      </div>

      <div className="shrink-0 border-t border-border p-3">
        <Skeleton className="mx-auto mb-2 h-3 w-20" />
        <Skeleton className="h-9 w-full" />
      </div>
    </aside>
  );
}

/**
 * Open, and which editor to open in.
 *
 * Without a chosen editor this used to fall through to the system opener,
 * which is exactly what Reveal does — two buttons, one behaviour, neither of
 * them an editor. The menu lists what is actually installed and remembers the
 * pick, so the configuration step nobody performed is gone.
 */
function OpenAction({ path }: { path: string }) {
  const [editors, setEditors] = useState<Editor[]>([]);
  const [chosen, setChosen] = useState<string | null>(null);

  useEffect(() => {
    void api.listEditors().then(setEditors).catch(() => undefined);
    void api
      .getConfig()
      .then((c) => setChosen(c.config.editor))
      .catch(() => undefined);
  }, []);

  const current = editors.find((e) => e.command === chosen);

  async function choose(command: string | null) {
    setChosen(command);
    await api.setEditor(command);
    await api.openInEditor(path);
  }

  return (
    <div className="flex min-w-0 flex-1">
      <Button
        variant="secondary"
        size="sm"
        className="h-7 min-w-0 flex-1 overflow-hidden rounded-r-none px-2 text-xs"
        onClick={() => void api.openInEditor(path)}
        title={current ? `Open in ${current.name}` : "Open"}
      >
        {/* `shrink-0`, or a narrow panel squeezes the glyph until it renders
            outside the button's own border. */}
        <ExternalLink className="size-3 shrink-0" />
        {/* Just "Open". The editor's name does not fit beside two more buttons
            at this width, and the menu already shows which one is ticked. */}
        <span className="truncate">Open</span>
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="secondary"
            size="sm"
            aria-label="Choose an editor"
            /*
              `shrink-0`: it is the right half of a split button, and a flex
              parent under pressure shrank it to nothing while its chevron
              carried on drawing — a glyph standing outside the control it
              belongs to.
            */
            className="h-7 shrink-0 rounded-l-none border-l border-background/40 px-1"
          >
            <ChevronDown className="size-3 shrink-0" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start">
          {editors.map((editor) => (
            <DropdownMenuItem
              key={editor.id}
              onSelect={() => void choose(editor.command)}
            >
              {editor.name}
              {editor.command === chosen ? (
                <Check className="ml-auto size-3" />
              ) : null}
            </DropdownMenuItem>
          ))}
          {editors.length > 0 ? <DropdownMenuSeparator /> : null}
          <DropdownMenuItem onSelect={() => void choose(null)}>
            System default
            {chosen === null ? <Check className="ml-auto size-3" /> : null}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

function Action({
  icon,
  label,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <Button
      variant="secondary"
      size="sm"
      className="h-7 min-w-0 flex-1 overflow-hidden px-2 text-xs"
      onClick={onClick}
    >
      {icon}
      <span className="truncate">{label}</span>
    </Button>
  );
}
