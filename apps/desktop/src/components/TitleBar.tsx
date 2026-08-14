import { Button } from "@/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { MOD_KEY, TRAFFIC_LIGHT_INSET } from "@/lib/platform";
import type { ActiveKey, Tab } from "@/lib/tabs";
import { cn, FOCUS_RING } from "@/lib/utils";
import {
  FileDiff,
  House,
  Plus,
  RefreshCw,
  Search,
  Settings,
  X,
} from "lucide-react";

/**
 * The ids that tie a tab to the panel it shows.
 *
 * Both halves of the pair are generated from one place, because a tab pointing
 * at a panel that does not point back is worse than neither pointing at all:
 * it reads as a working relationship and behaves as nothing.
 */
export const HOME_TAB_ID = "tab-home";
export const HOME_PANEL_ID = "panel-home";

/** Tab keys carry `:` and paths, so they are encoded rather than interpolated. */
function encodeKey(key: string): string {
  return encodeURIComponent(key).replace(/%/g, "_");
}

export function tabId(key: string): string {
  return `tab-${encodeKey(key)}`;
}

export function panelId(key: string): string {
  return `panel-${encodeKey(key)}`;
}

/**
 * The title bar, doubling as the app's toolbar.
 *
 * Folding the toolbar into the title bar saves a whole row on a window only
 * 760px tall, and stops the window repeating its own name back at itself.
 *
 * The split is deliberate: **global chrome lives here, tab-local controls live
 * inside the tab.** Jump-to, refresh, settings and New worktree act on
 * everything, so they group together on the trailing edge. The verdict filters
 * and the list's own search only mean something for the worktree list, so they
 * live inside it.
 *
 * The leading edge carries only where-you-are: the home button and the tabs.
 * It starts well clear of the traffic lights, because a control set flush
 * against them stops reading as app chrome and starts reading as a fourth
 * window button.
 *
 * The list gets a home button rather than a tab. It is the root, not a peer —
 * a tab you can never close is a tab that should not have been one.
 */
export function TitleBar({
  tabs,
  activeKey,
  onHome,
  onJump,
  onActivate,
  onClose,
  onRefresh,
  onSettings,
  onCreate,
  canCreate,
  busy,
}: {
  tabs: Tab[];
  activeKey: ActiveKey;
  onHome: () => void;
  onJump: () => void;
  onActivate: (key: string) => void;
  onClose: (key: string) => void;
  onRefresh: () => void;
  onSettings: () => void;
  onCreate: () => void;
  canCreate: boolean;
  busy: boolean;
}) {
  return (
    <header
      // Draggable by its empty space, so an overlay title bar does not cost the
      // window its ability to be moved.
      //
      // "deep" rather than a bare attribute: bare means *only direct clicks on
      // this element*, and the header is covered edge to edge by its children,
      // so nothing ever matched and the window could not be moved at all.
      // Tauri already exempts buttons, inputs and role="tab" from a deep
      // region, so the controls keep working without marking them up.
      data-tauri-drag-region="deep"
      className="@container flex h-11 shrink-0 items-center gap-1 border-b border-border pr-2"
      style={{ paddingLeft: TRAFFIC_LIGHT_INSET }}
    >
      <div
        /*
          A real tablist, holding real tabs.

          Home and the open views are one row of peers on screen, and they were
          a button with `aria-current` sitting beside a set of `role="tab"`
          elements that controlled nothing — the panels they switch between
          were already `role="tabpanel"`, with no tab pointing at them. The
          relationship is stated now, in both directions, and nothing about the
          order or the keys changed.
        */
        role="tablist"
        aria-label="Open views"
        className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto"
      >
        <Hint label="All worktrees" shortcut={`${MOD_KEY}1`}>
          <button
            role="tab"
            id={HOME_TAB_ID}
            aria-controls={HOME_PANEL_ID}
            aria-selected={activeKey === null}
            tabIndex={activeKey === null ? 0 : -1}
            onClick={onHome}
            onKeyDown={onTabKeyDown}
            aria-label="All worktrees"
            className={cn(
              "flex size-7 shrink-0 items-center justify-center rounded-md transition-colors",
              FOCUS_RING,
              activeKey === null
                ? "bg-muted text-foreground"
                : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            )}
          >
            <House className="size-3.5" />
          </button>
        </Hint>

        {tabs.length > 0 ? (
          <div
            className="mx-1 h-4 w-px shrink-0 bg-border"
            role="none"
            aria-hidden
          />
        ) : null}

        {tabs.map((tab, index) => (
          <TabButton
            key={tab.key}
            tab={tab}
            index={index}
            active={tab.key === activeKey}
            onActivate={() => onActivate(tab.key)}
            onClose={() => onClose(tab.key)}
          />
        ))}
      </div>

      <div className="flex shrink-0 items-center gap-1">
        <Hint label="Jump to any worktree" shortcut={`${MOD_KEY}K`}>
          <button
            onClick={onJump}
            aria-label="Jump to any worktree"
            className={cn(
              "flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 text-xs",
              FOCUS_RING,
              "text-muted-foreground transition-colors hover:bg-muted/60 hover:text-foreground",
            )}
          >
            <Search className="size-3.5" />
            {/*
              Below a certain window the words are the first thing that can go:
              the icon, the tooltip and ⌘K all still lead to the same palette,
              and a control pushed off the edge leads nowhere.
            */}
            <span className="hidden @[56rem]:inline">Jump to…</span>
            <kbd className="hidden rounded border border-border px-1 font-sans text-[10px] leading-4 text-muted-foreground @[56rem]:inline">
              {MOD_KEY}K
            </kbd>
          </button>
        </Hint>
        <Hint label="Refresh">
          <Button
            variant="ghost"
            size="icon"
            onClick={onRefresh}
            disabled={busy}
            aria-label="Refresh"
            className="size-7"
          >
            <RefreshCw className={cn("size-3.5", busy && "animate-spin")} />
          </Button>
        </Hint>
        <Hint label="Settings">
          <Button
            variant="ghost"
            size="icon"
            onClick={onSettings}
            aria-label="Settings"
            className="size-7"
          >
            <Settings className="size-3.5" />
          </Button>
        </Hint>
        <Button
          size="sm"
          onClick={onCreate}
          disabled={!canCreate}
          aria-label="New worktree"
          className="h-7 rounded-md px-2.5 text-xs"
        >
          <Plus className="size-3.5" />
          <span className="hidden @[46rem]:inline">New worktree</span>
        </Button>
      </div>
    </header>
  );
}

function TabButton({
  tab,
  index,
  active,
  onActivate,
  onClose,
}: {
  tab: Tab;
  index: number;
  active: boolean;
  onActivate: () => void;
  onClose: () => void;
}) {
  const { title, subtitle } = describe(tab);
  // Home occupies the first slot, so tabs are numbered from two.
  const shortcut = index + 2 <= 9 ? `${MOD_KEY}${index + 2}` : undefined;

  return (
    <Hint label={subtitle ?? title} shortcut={shortcut}>
      {/*
        The tab and its close control are siblings, not one inside the other.

        `role="tab"` takes presentational children — everything inside it is
        folded into one name — so a close button nested in the tab was a
        control screen readers could not report and whose label was swallowed
        by the tab's own. Side by side, the tab is a tab and the close is a
        button, and the pointer behaviour is unchanged: the row still activates
        on click and still closes on middle click, anywhere along it.
      */}
      <div
        role="presentation"
        onAuxClick={(e) => {
          if (e.button === 1) {
            e.preventDefault();
            onClose();
          }
        }}
        className={cn(
          "group flex h-7 max-w-44 shrink-0 cursor-default items-center rounded-md text-xs transition-colors",
          active
            ? "bg-muted text-foreground"
            : "text-muted-foreground hover:bg-muted/60",
        )}
      >
        <button
          role="tab"
          id={tabId(tab.key)}
          aria-controls={panelId(tab.key)}
          aria-selected={active}
          tabIndex={active ? 0 : -1}
          onClick={onActivate}
          onKeyDown={onTabKeyDown}
          className={cn(
            "flex h-7 min-w-0 flex-1 items-center gap-1.5 rounded-md pr-1 pl-2 text-left",
            FOCUS_RING,
          )}
        >
          <TabIcon tab={tab} />
          <span className="min-w-0 flex-1 truncate">{title}</span>
        </button>
        <button
          onClick={onClose}
          aria-label={`Close ${title}`}
          className={cn(
            "mr-1 flex size-4 shrink-0 items-center justify-center rounded-sm",
            FOCUS_RING,
            "opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100",
            "hover:bg-background/70",
            active && "opacity-60",
          )}
        >
          <X className="size-3" />
        </button>
      </div>
    </Hint>
  );
}

/**
 * The standard horizontal tab keyboard model.
 *
 * Only the active tab participates in the page's Tab order. Arrow keys move
 * and activate within the strip; Home and End jump to its bounds. Close
 * controls remain ordinary buttons, so keyboard users can still reach them
 * immediately after the tab they belong to.
 */
function onTabKeyDown(event: React.KeyboardEvent<HTMLButtonElement>) {
  if (
    event.key !== "ArrowLeft" &&
    event.key !== "ArrowRight" &&
    event.key !== "Home" &&
    event.key !== "End"
  ) {
    return;
  }

  const tablist = event.currentTarget.closest('[role="tablist"]');
  if (!tablist) return;
  const tabs = Array.from(
    tablist.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
  );
  const current = tabs.indexOf(event.currentTarget);
  if (current < 0 || tabs.length === 0) return;

  event.preventDefault();
  let next = current;
  if (event.key === "Home") next = 0;
  if (event.key === "End") next = tabs.length - 1;
  if (event.key === "ArrowLeft") {
    next = (current - 1 + tabs.length) % tabs.length;
  }
  if (event.key === "ArrowRight") next = (current + 1) % tabs.length;

  tabs[next]?.focus();
  tabs[next]?.click();
}

function TabIcon({ tab }: { tab: Tab }) {
  const className = "size-3 shrink-0";
  switch (tab.kind) {
    case "diff":
      return <FileDiff className={className} />;
    case "settings":
      return <Settings className={className} />;
  }
}

function describe(tab: Tab): { title: string; subtitle?: string } {
  switch (tab.kind) {
    case "diff":
      return { title: tab.title, subtitle: `${tab.subtitle} · ${tab.path}` };
    case "settings":
      return { title: "Settings" };
  }
}

function Hint({
  label,
  shortcut,
  children,
}: {
  label: string;
  shortcut?: string;
  children: React.ReactNode;
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent side="bottom" className="max-w-sm">
        <p className="break-all">{label}</p>
        {shortcut ? <p className="opacity-60">{shortcut}</p> : null}
      </TooltipContent>
    </Tooltip>
  );
}
