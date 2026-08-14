import { useEffect } from "react";
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { VerdictDot } from "@/components/verdict";
import {
  type RepoReport,
  type Worktree,
  reasonLabel,
  worktreeLabel,
} from "@/lib/api";
import { humanBytes } from "@/lib/utils";

export interface Entry {
  repo: RepoReport;
  worktree: Worktree;
}

/**
 * Fuzzy-find any worktree across every repository.
 *
 * Deliberately not a tab: its job is to *open* tabs, not to be one. It resolves
 * to "take me there" and then gets out of the way.
 *
 * Built on cmdk (what shadcn's command wraps) rather than a hand-rolled list,
 * which is where the filtering, `aria-activedescendant`, scroll-into-view and
 * composition-event handling come from for free.
 */
export function CommandPalette({
  open,
  entries,
  onOpenChange,
  onOpenDiff,
  onReveal,
}: {
  open: boolean;
  entries: Entry[];
  onOpenChange: (open: boolean) => void;
  onOpenDiff: (entry: Entry) => void;
  onReveal: (entry: Entry) => void;
}) {
  // Cmd+K toggles from anywhere, since the palette is the way you move around.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        onOpenChange(!open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onOpenChange]);

  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Find a worktree"
      description="Search across every repository"
    >
      <CommandInput placeholder="Find a worktree…" />
      <CommandList>
        <CommandEmpty>Nothing matches.</CommandEmpty>
        <CommandGroup heading="Open changes">
          {entries.map((entry) => (
            <CommandItem
              key={entry.worktree.path}
              // The searchable text: repo, branch and path, so any of the three
              // finds it.
              value={`${entry.repo.name} ${worktreeLabel(entry.worktree)} ${entry.worktree.path}`}
              onSelect={() => {
                onOpenDiff(entry);
                onOpenChange(false);
              }}
            >
              <VerdictDot verdict={entry.worktree.verdict} />
              <span className="shrink-0 text-[11px] text-muted-foreground">
                {entry.repo.name}
              </span>
              <span className="min-w-0 flex-1 truncate">
                {worktreeLabel(entry.worktree)}
              </span>
              <span className="shrink-0 text-[11px] text-muted-foreground">
                {reasonLabel(entry.worktree.reason)}
              </span>
              <span className="w-14 shrink-0 text-right text-[11px] tabular-nums text-muted-foreground">
                {humanBytes(entry.worktree.status.size?.bytes ?? null)}
              </span>
            </CommandItem>
          ))}
        </CommandGroup>
        <CommandGroup heading="Reveal in file manager">
          {entries.slice(0, 20).map((entry) => (
            <CommandItem
              key={`reveal:${entry.worktree.path}`}
              value={`reveal ${entry.repo.name} ${worktreeLabel(entry.worktree)}`}
              onSelect={() => {
                onReveal(entry);
                onOpenChange(false);
              }}
            >
              <span className="min-w-0 flex-1 truncate text-muted-foreground">
                Reveal {worktreeLabel(entry.worktree)}
              </span>
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}
