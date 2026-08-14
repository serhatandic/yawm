import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  HOME_PANEL_ID,
  HOME_TAB_ID,
  TitleBar,
  panelId,
  tabId,
} from "@/components/TitleBar";
import { WorktreeListTab, type Located } from "@/components/WorktreeListTab";
import { SettingsTab } from "@/components/SettingsTab";
import { CommandPalette } from "@/components/CommandPalette";
import { CreateDialog } from "@/components/CreateDialog";
import { WorkspacePicker } from "@/components/WorkspacePicker";
import { DeleteDialog } from "@/components/DeleteDialog";
import type { UnverifiedBranch } from "@/components/delete-rules";
import {
  api,
  type Config,
  type ConfigStatus,
  type DiffStyle,
  type RepoReport,
  type UnreadableSource,
  type Workspace,
  type Worktree,
} from "@/lib/api";
import {
  configNotice,
  keptBranchNotice,
  scanFailureNotice,
  scanNotice,
  unverifiedBranchNotice,
  visibleNotices,
  type Notice,
  type ScanFailure,
} from "@/components/notices";
import { settleScan, type ScanPass } from "@/components/scan-progress";
import { hasMod } from "@/lib/platform";
import {
  SETTINGS_TAB,
  diffTabFor,
  useTabs,
  type ChangesAnchor,
  type ReadingIntent,
} from "@/lib/tabs";
import { cn, FOCUS_RING } from "@/lib/utils";
import { landingTargets, mergeLandingAnswer } from "@/lib/landing-pass";
import { AlertTriangle, Info, X } from "lucide-react";

// Diff rendering brings syntax grammars and themes with it. Load that bundle
// only after somebody opens Changes so the worktree list stays lightweight.
const DiffTab = lazy(() =>
  import("@/components/DiffTab").then(({ DiffTab }) => ({ default: DiffTab })),
);

export default function App() {
  const [reports, setReports] = useState<RepoReport[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [measuring, setMeasuring] = useState(false);
  const [analyzingLanding, setAnalyzingLanding] = useState(false);
  /** The rows on screen still belong to the workspace being switched away from. */
  const [switching, setSwitching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [diffStyle, setDiffStyle] = useState<DiffStyle>("unified");
  const [hideMain, setHideMain] = useState(false);
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [activeWorkspace, setActiveWorkspace] = useState<string | null>(null);
  const [pendingAdd, setPendingAdd] = useState<{
    path: string;
    kind: "repo" | "scanRoot";
  } | null>(null);
  const [deleting, setDeleting] = useState<Located[] | null>(null);

  /**
   * The conditions the list itself cannot express.
   *
   * A source that could not be read, settings that could not be parsed, and a
   * branch git declined to delete all leave the screen looking normal and
   * slightly wrong. They are ambient rather than modal — nothing is waiting on
   * an answer — so they live in one dismissible line rather than a dialog.
   */
  const [unreadable, setUnreadable] = useState<UnreadableSource[]>([]);
  const [configState, setConfigState] = useState<ConfigStatus | null>(null);
  const [keptBranches, setKeptBranches] = useState<string[]>([]);
  /*
   * Branches a removal could not vouch for — finalisation broke before the ref
   * state was established, or a deletion could not be rolled back. Held apart
   * from `keptBranches` because "kept" is a promise this app cannot make here.
   */
  const [unverifiedBranches, setUnverifiedBranches] = useState<
    UnverifiedBranch[]
  >([]);
  const [scanFailure, setScanFailure] = useState<ScanFailure | null>(null);
  const [dismissed, setDismissed] = useState<string[]>([]);

  const tabs = useTabs();

  /**
   * Merge a scan into what is already known.
   *
   * A fast scan may carry no size data, so applying it directly would blank that
   * column for as long as the full scan takes. Process data is deliberately not
   * carried forward: unlike size and landing proofs it is a mutable snapshot,
   * and retaining an old empty result would manufacture deletion certainty.
   *
   * Incoming sizes win when there are any: a fast scan that found the worktree
   * in core's size cache is at least as current as whatever is on screen.
   */
  const applyScan = useCallback((incoming: RepoReport[], full: boolean) => {
    hasList.current = true;
    setReports((previous) => {
      if (previous === null) return incoming;

      const known = new Map<string, Worktree>();
      for (const repo of previous) {
        for (const worktree of repo.worktrees) known.set(worktree.path, worktree);
      }

      return incoming.map((repo) => ({
        ...repo,
        worktrees: repo.worktrees.map((worktree) => {
          const before = known.get(worktree.path);
          if (!before) return worktree;
          const keepLanding =
            full &&
            before.head === worktree.head &&
            before.status.landingComplete &&
            !worktree.status.landingComplete;
          const keepVerdict =
            keepLanding &&
            worktree.status.processCheckComplete &&
            worktree.reason.kind === "landingUnknown" &&
            before.verdict === "disposable";
          return {
            ...worktree,
            verdict: keepVerdict ? before.verdict : worktree.verdict,
            reason: keepVerdict ? before.reason : worktree.reason,
            status: {
              ...worktree.status,
              size: full
                ? worktree.status.size
                : (worktree.status.size ?? before.status.size),
              processes: worktree.status.processes,
              landing: keepLanding
                ? before.status.landing
                : worktree.status.landing,
              landingComplete: keepLanding
                ? before.status.landingComplete
                : worktree.status.landingComplete,
            },
          };
        }),
      }));
    });
  }, []);

  const applyLanding = useCallback((repoRoot: string, answer: Worktree) => {
    setReports((current) =>
      current?.map((repo) =>
        repo.root === repoRoot
          ? {
              ...repo,
              worktrees: repo.worktrees.map((worktree) =>
                worktree.path === answer.path
                  ? mergeLandingAnswer(worktree, answer)
                  : worktree,
              ),
            }
          : repo,
      ) ?? null,
    );
  }, []);

  /**
   * Which scan the UI is currently willing to listen to.
   *
   * A full scan takes tens of seconds, so by the time it returns the user may
   * have moved to a different workspace twice over. Without this, an old
   * workspace's result would arrive last and win, repainting the list with
   * rows the user is no longer looking at.
   */
  const scanToken = useRef(0);

  /**
   * Whether a scan has ever put a list on screen. Kept as a ref so that a
   * failure can choose between replacing an empty screen and annotating a
   * populated one without `refresh` depending on `reports`, which would rebuild
   * it on every scan and restart the mount effect.
   */
  const hasList = useRef(false);

  const refresh = useCallback(
    async (options: { quiet?: boolean } = {}) => {
      const quiet = options.quiet ?? false;
      const token = (scanToken.current += 1);
      const current = () => scanToken.current === token;

      setError(null);
      setScanFailure(null);
      setAnalyzingLanding(false);
      if (!quiet) setLoading(true);

      /**
       * A pass that ends in failure has to leave the list honest.
       *
       * Clearing `measuring` is the load-bearing half: it is what the size
       * cells pulse on, and a pulsing cell is a promise that a number is
       * coming. When no number is coming the cell must stop promising, and the
       * notice explains what the blank means. Which surface says so depends on
       * whether there is anything on screen to keep — replacing a good list
       * with an error page over a failed size pass would throw away the part
       * that worked.
       */
      const fail = (pass: ScanPass, settled: { reason: string; timedOut: boolean }) => {
        if (pass === "listing" && !hasList.current) {
          setError(settled.reason);
        } else {
          setScanFailure({ pass, reason: settled.reason, timedOut: settled.timedOut });
        }
      };

      try {
        // The fast pass is what makes a workspace switch feel like anything at
        // all: it skips the disk walk, so it lands in about two seconds where
        // the full pass takes twenty.
        if (!quiet) {
          const fast = await settleScan(api.scanAll(false), { isCurrent: current });
          if (fast.state === "superseded") return;
          if (fast.state === "failed") {
            fail("listing", fast);
            return;
          }
          applyScan(fast.value.repos, false);
          setUnreadable(fast.value.unreadable);
          setLoading(false);
          setSwitching(false);
        }
        setMeasuring(true);
        const full = await settleScan(api.scanAll(true), { isCurrent: current });
        if (full.state === "superseded") return;
        if (full.state === "failed") {
          fail("measuring", full);
          return;
        }
        applyScan(full.value.repos, true);
        setUnreadable(full.value.unreadable);

        const targets = landingTargets(full.value.repos);
        let landingFailure:
          | { reason: string; timedOut: boolean }
          | undefined;
        if (targets.length > 0) setAnalyzingLanding(true);
        for (const target of targets) {
          if (!current()) return;

          while (current()) {
            const settled = await settleScan(
              api.resolveLanding(target.repo, target.worktree),
              { isCurrent: current },
            );
            if (settled.state === "superseded") return;
            if (settled.state === "failed") {
              landingFailure ??= settled;
              break;
            }
            // A foreground request owns priority. Waiting before retrying keeps
            // that handoff from becoming an IPC spin loop.
            if (settled.value === null) {
              await new Promise((resolve) => setTimeout(resolve, 100));
              continue;
            }
            applyLanding(target.repo, settled.value);
            break;
          }
        }
        setAnalyzingLanding(false);
        if (landingFailure) fail("landing", landingFailure);
      } catch (e) {
        if (current()) setError(String(e));
      } finally {
        if (current()) {
          setLoading(false);
          setMeasuring(false);
          setAnalyzingLanding(false);
          setSwitching(false);
        }
      }
    },
    [applyLanding, applyScan],
  );

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const loadConfig = useCallback(async () => {
    const { config: c } = await api.getConfig();
    setDiffStyle(c.diffStyle);
    setHideMain(c.hideMainWorktrees);
    setWorkspaces(c.workspaces);
    setActiveWorkspace(c.activeWorkspace);
  }, []);

  useEffect(() => {
    void loadConfig().catch(() => undefined);
  }, [loadConfig]);

  // Asked once: whether the settings on screen are the user's choices or a
  // guess made because their file could not be read.
  useEffect(() => {
    void api
      .configStatus()
      .then(setConfigState)
      .catch(() => undefined);
  }, []);

  /**
   * The switch itself is instant; the rows behind it are not.
   *
   * The selection moves before anything is scanned, so the control never feels
   * unresponsive, and `switching` marks the rows still on screen as belonging
   * to the workspace being left. They stay visible rather than being blanked —
   * an empty list where a full one just was reads as a crash — but they are
   * dimmed and inert until the fast pass replaces them.
   */
  const changeWorkspace = useCallback(
    async (id: string | null) => {
      setActiveWorkspace(id);
      setSwitching(true);
      await api.setActiveWorkspace(id);
      await refresh();
    },
    [refresh],
  );

  /**
   * A read-modify-write of one setting, retried once if it lost a race.
   *
   * The backend refuses a write made against settings that have since moved,
   * rather than letting it overwrite them. For a single toggle the recovery is
   * to re-apply that one change to what is now stored — the user's intent was
   * never about the rest of the file.
   */
  const updateConfig = useCallback(
    async (change: (config: Config) => Config) => {
      let { config, revision } = await api.getConfig();
      for (let attempt = 0; attempt < 2; attempt += 1) {
        const result = await api.setConfig(change(config), revision);
        if (result.outcome === "saved") return;
        config = result.config;
        revision = result.revision;
      }
    },
    [],
  );

  /** A standing preference, so it is written back rather than kept in view state. */
  const changeHideMain = useCallback(
    (hide: boolean) => {
      setHideMain(hide);
      void updateConfig((c) => ({ ...c, hideMainWorktrees: hide }))
        .then(() => {
          // Main worktrees are not measured while they are hidden, so bringing
          // them back reveals rows with no size. The rescan is sequenced after
          // the write because the scanner reads the preference from the saved
          // config; refreshing first would measure against the old one. Only
          // one direction needs it — hiding rows never leaves a gap.
          if (!hide) void refresh({ quiet: true });
        })
        .catch(() => undefined);
    },
    [updateConfig, refresh],
  );

  /** Toggling the layout should stick, so it is written back to the config. */
  const changeDiffStyle = useCallback(
    (style: DiffStyle) => {
      setDiffStyle(style);
      void updateConfig((c) => ({ ...c, diffStyle: style })).catch(
        () => undefined,
      );
    },
    [updateConfig],
  );

  // Re-scan on focus, since worktrees change outside yawm. Rate-limited and
  // quiet, so switching windows does not churn the list.
  useEffect(() => {
    let last = Date.now();
    const onFocus = () => {
      const now = Date.now();
      if (now - last < 10_000) return;
      last = now;
      void refresh({ quiet: true });
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

  // Tab shortcuts, matching what every tabbed application does.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!hasMod(e)) return;

      if (e.key.toLowerCase() === "w") {
        e.preventDefault();
        tabs.closeActive();
        return;
      }
      if (e.shiftKey && (e.key === "[" || e.key === "{")) {
        e.preventDefault();
        tabs.cycle(-1);
        return;
      }
      if (e.shiftKey && (e.key === "]" || e.key === "}")) {
        e.preventDefault();
        tabs.cycle(1);
        return;
      }
      if (/^[1-9]$/.test(e.key)) {
        e.preventDefault();
        tabs.activateIndex(Number(e.key) - 1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [tabs]);

  const entries: Located[] = (reports ?? []).flatMap((repo) =>
    repo.worktrees.map((worktree) => ({ repo, worktree })),
  );

  const updateWorktree = useCallback((repoRoot: string, updated: Worktree) => {
    setReports((current) =>
      current?.map((repo) =>
        repo.root === repoRoot
          ? {
              ...repo,
              worktrees: repo.worktrees.map((worktree) =>
                worktree.path === updated.path ? updated : worktree,
              ),
            }
          : repo,
      ) ?? null,
    );
  }, []);

  /**
   * The reading travels with the click, and so does where it lands.
   *
   * Opening from the dirty count asks to see work that exists only on disk, so
   * it anchors there — without hiding the branch's commits, which are the
   * other half of the same decision. The Changes button asks the narrower
   * question and the view falls back to the complete reading by itself when
   * there is nothing to narrow. `diffTabFor` stamps each open with a fresh
   * request number, so re-opening an already-open tab carries the new question
   * rather than silently keeping the old one.
   */
  const openDiff = useCallback(
    (
      located: Located,
      open: { intent?: ReadingIntent; anchor?: ChangesAnchor } = {},
    ) => tabs.open(diffTabFor(located.repo, located.worktree, open)),
    [tabs],
  );

  /**
   * Pick a folder, then decide where it belongs.
   *
   * With a single workspace the answer is already known, so it is added
   * straight away — asking a question with one possible answer is worse than
   * not asking. Only a real choice opens the picker.
   */
  async function addFolder(kind: "repo" | "scanRoot") {
    const picked = await openDialog({ directory: true, multiple: false });
    if (typeof picked !== "string") return;

    if (workspaces.length > 1) {
      setPendingAdd({ path: picked, kind });
      return;
    }
    await commitAdd(picked, kind, activeWorkspace);
  }

  async function commitAdd(
    path: string,
    kind: "repo" | "scanRoot",
    workspace: string | null,
  ) {
    if (kind === "repo") await api.addRepo(path, workspace);
    else await api.addScanRoot(path, workspace);
    // Adding into a workspace you are not looking at would appear to do
    // nothing, so follow it there.
    if (workspace && workspace !== activeWorkspace) {
      await changeWorkspace(workspace);
    } else {
      await refresh();
    }
    await loadConfig();
  }

  return (
    <TooltipProvider delayDuration={400}>
      <div className="flex h-full flex-col">
        <TitleBar
          tabs={tabs.tabs}
          activeKey={tabs.activeKey}
          onHome={tabs.goHome}
          onJump={() => setPaletteOpen(true)}
          onActivate={tabs.activate}
          onClose={tabs.close}
          onRefresh={() => void refresh()}
          onSettings={() => tabs.open(SETTINGS_TAB)}
          onCreate={() => setCreating(true)}
          canCreate={(reports?.length ?? 0) > 0}
          busy={loading || measuring || switching}
        />

        <NoticeBar
          notices={visibleNotices(
            [
              configState ? configNotice(configState) : null,
              scanFailureNotice(scanFailure),
              scanNotice(unreadable),
              keptBranchNotice(keptBranches),
              unverifiedBranchNotice(unverifiedBranches),
            ],
            dismissed,
          )}
          onDismiss={(id) => setDismissed((seen) => [...seen, id])}
          onAction={() => void refresh()}
        />

        {/*
          Home and every open tab stay mounted; only one is visible. That is the
          whole point of tabs here — unmounting would throw away the list's
          scroll position and filter, and refetch every diff, which is exactly
          the behaviour being fixed.
        */}
        <div className="relative min-h-0 flex-1">
          <Panel
            visible={tabs.isHome}
            id={HOME_PANEL_ID}
            labelledBy={HOME_TAB_ID}
          >
            <WorktreeListTab
              reports={reports}
              loading={loading}
              measuring={measuring}
              analyzingLanding={analyzingLanding}
              switching={switching}
              error={error}
              onRefresh={() => void refresh()}
              onAddRepo={() => void addFolder("repo")}
              onAddScanRoot={() => void addFolder("scanRoot")}
              onOpenDiff={openDiff}
              onDelete={setDeleting}
              onWorktreeUpdate={updateWorktree}
              workspaces={workspaces}
              activeWorkspace={activeWorkspace}
              onWorkspaceChange={(id) => void changeWorkspace(id)}
              hideMain={hideMain}
              onHideMainChange={changeHideMain}
            />
          </Panel>

          {tabs.tabs.map((tab) => (
            <Panel
              key={tab.key}
              visible={tab.key === tabs.activeKey}
              id={panelId(tab.key)}
              labelledBy={tabId(tab.key)}
            >
              {tab.kind === "diff" ? (
                <Suspense fallback={<PanelLoading label="Loading changes…" />}>
                  <DiffTab
                    repoRoot={tab.repoRoot}
                    path={tab.path}
                    intent={tab.intent}
                    anchor={tab.anchor}
                    request={tab.request}
                    diffStyle={diffStyle}
                    onDiffStyleChange={changeDiffStyle}
                  />
                </Suspense>
              ) : (
                <SettingsTab onSaved={() => { void refresh(); void loadConfig(); }} />
              )}
            </Panel>
          ))}
        </div>

        <CommandPalette
          open={paletteOpen}
          entries={entries}
          onOpenChange={setPaletteOpen}
          onOpenDiff={openDiff}
          onReveal={(entry) => void api.revealPath(entry.worktree.path)}
        />

        <WorkspacePicker
          open={pendingAdd !== null}
          workspaces={workspaces}
          path={pendingAdd?.path ?? null}
          kind={pendingAdd?.kind ?? "repo"}
          onCancel={() => setPendingAdd(null)}
          onChoose={(id) => {
            const add = pendingAdd;
            setPendingAdd(null);
            if (add) void commitAdd(add.path, add.kind, id);
          }}
        />

        <CreateDialog
          open={creating}
          repos={reports ?? []}
          initialRepo={null}
          onClose={() => setCreating(false)}
          onCreated={() => void refresh()}
        />

        <DeleteDialog
          open={deleting !== null}
          repo={deleting?.[0]?.repo.root ?? ""}
          worktrees={(deleting ?? []).map((d) => d.worktree)}
          onClose={() => setDeleting(null)}
          onDone={({ removed, vanished, keptBranches, unverifiedBranches }) => {
            setKeptBranches(keptBranches);
            setUnverifiedBranches(unverifiedBranches);
            /*
             * A gone worktree's diff tab would show a stale patch, so drop it.
             * Only the ones that actually went: a batch that failed part-way
             * leaves the rest on disk, and closing their tabs would say they
             * were deleted when they were not.
             *
             * `vanished` went too — removed from outside yawm while the dialog
             * was open — so its tabs are just as stale. It stays a separate
             * list because nothing here may report those as deletions this app
             * carried out.
             */
            for (const path of [...removed, ...vanished]) {
              tabs.close(`diff:${path}`);
            }
            const gone = new Set([...removed, ...vanished]);
            setReports((current) =>
              current
                ?.map((report) => ({
                  ...report,
                  worktrees: report.worktrees.filter(
                    (worktree) => !gone.has(worktree.path),
                  ),
                })) ?? null,
            );
            void refresh();
          }}
        />
      </div>
    </TooltipProvider>
  );
}

/** Keeps an inactive tab mounted but out of the way. */
function Panel({
  visible,
  id,
  labelledBy,
  children,
}: {
  visible: boolean;
  id: string;
  /** The tab that shows this panel, so the pair is stated in both directions. */
  labelledBy: string;
  children: React.ReactNode;
}) {
  /*
   * Nothing is built inside a hidden panel.
   *
   * Hidden here means `display: none`, which gives an element no boxes to
   * measure. Anything that sizes itself when it mounts gets zeroes, and
   * nothing tells it to look again once the panel is shown: the diff renderer
   * came up as an empty container, and only drew after a file was collapsed
   * and reopened, because that mounted it again while the tab was on screen.
   *
   * Once shown, the children stay. Keeping them is the entire reason these
   * panels are hidden rather than unmounted — scroll position, filters and
   * fetched diffs survive a trip to another tab.
   */
  const [everShown, setEverShown] = useState(visible);
  useEffect(() => {
    if (visible) setEverShown(true);
  }, [visible]);

  return (
    <div
      role="tabpanel"
      id={id}
      aria-labelledby={labelledBy}
      hidden={!visible}
      className={cn("absolute inset-0", visible ? "block" : "hidden")}
    >
      {everShown ? children : null}
    </div>
  );
}

function PanelLoading({ label }: { label: string }) {
  return (
    <div className="flex h-full items-center justify-center text-xs text-muted-foreground">
      {label}
    </div>
  );
}

/**
 * The quiet line that says what the list could not.
 *
 * Deliberately not a dialog: none of these need an answer, and interrupting
 * someone to tell them a drive is unmounted teaches them to dismiss without
 * reading. It sits under the title bar, in the app's own type scale, and
 * disappears entirely when there is nothing to say.
 */
function NoticeBar({
  notices,
  onDismiss,
  onAction,
}: {
  notices: Notice[];
  onDismiss: (id: string) => void;
  onAction: (id: string) => void;
}) {
  if (notices.length === 0) return null;

  return (
    <div className="shrink-0 border-b border-border">
      {notices.map((notice) => (
        <div
          key={notice.id}
          role="status"
          className="flex items-start gap-2 px-3 py-1.5 text-[11px] leading-relaxed"
        >
          {/*
            An icon that means something, rather than a coloured bullet.

            The dot said "warning" in hue alone — invisible to anyone who does
            not separate amber from grey, and indistinguishable from a list
            marker to everyone else. Same two tones, same two meanings, now
            also carried by the shape.
          */}
          {notice.tone === "warning" ? (
            <AlertTriangle
              className="mt-0.5 size-3.5 shrink-0 text-review"
              aria-hidden
            />
          ) : (
            <Info
              className="mt-0.5 size-3.5 shrink-0 text-muted-foreground"
              aria-hidden
            />
          )}
          <span className="min-w-0 flex-1 text-muted-foreground">
            {notice.text}
          </span>
          {notice.action ? (
            <button
              type="button"
              // Not dismissed here: the retry itself clears the notice, so if
              // the second attempt fails the same way the line comes back
              // rather than being suppressed by an id the user never hid.
              onClick={() => onAction(notice.id)}
              className={cn(
                "shrink-0 rounded px-1.5 text-foreground underline underline-offset-2 hover:text-review",
                FOCUS_RING,
              )}
            >
              {notice.action.label}
            </button>
          ) : null}
          {/*
            A real target rather than a glyph.

            It was a bare `×` in a text run, which gave it the hit area of one
            character on a line the eye has already decided is not urgent.
          */}
          <button
            type="button"
            onClick={() => onDismiss(notice.id)}
            className={cn(
              "flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-muted/60 hover:text-foreground",
              FOCUS_RING,
            )}
            aria-label="Dismiss"
          >
            <X className="size-3" />
          </button>
        </div>
      ))}
    </div>
  );
}
