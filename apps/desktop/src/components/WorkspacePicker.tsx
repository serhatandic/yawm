import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { api, type Workspace, sourceCount } from "@/lib/api";
import { cn, FOCUS_RING } from "@/lib/utils";
import { FolderGit2, Loader2, Plus } from "lucide-react";

/**
 * Ask which workspace something should go into.
 *
 * Only shown when there is a real choice: with a single workspace the answer is
 * already known, so callers skip this and add directly. Asking a question with
 * one possible answer is worse than not asking.
 */
export function WorkspacePicker({
  open,
  workspaces,
  path,
  kind,
  onCancel,
  onChoose,
}: {
  open: boolean;
  workspaces: Workspace[];
  path: string | null;
  kind: "repo" | "scanRoot";
  onCancel: () => void;
  onChoose: (workspaceId: string) => void;
}) {
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (open) {
      setCreating(false);
      setName("");
    }
  }, [open]);

  async function createAndChoose() {
    const trimmed = name.trim();
    if (!trimmed) return;
    setBusy(true);
    try {
      onChoose(await api.createWorkspace(trimmed));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onCancel()}>
      <DialogContent className="max-w-md gap-0 p-0">
        <DialogHeader className="px-5 pt-5 pr-10 pb-3">
          {/* Names what is being added, which the path below then shows. */}
          <DialogTitle className="text-sm">
            Add this {kind === "repo" ? "repository" : "folder"} to which
            workspace?
          </DialogTitle>
          <DialogDescription className="text-xs">
            Workspaces keep separate sets of repositories from mixing in one
            list.
          </DialogDescription>
        </DialogHeader>

        <div className="px-5 pb-4">
          {path ? (
            <p className="mb-3 truncate font-mono text-[11px] text-muted-foreground">
              {path}
            </p>
          ) : null}

          <div className="space-y-1">
            {workspaces.map((ws) => (
              <button
                key={ws.id}
                onClick={() => onChoose(ws.id)}
                className={cn(
                  "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs",
                  FOCUS_RING,
                  "text-muted-foreground hover:bg-muted hover:text-foreground",
                )}
              >
                <FolderGit2 className="size-3.5 shrink-0" />
                <span className="min-w-0 flex-1 truncate">{ws.name}</span>
                <span className="shrink-0 text-[11px] text-muted-foreground">
                  {sourceCount(ws)}
                </span>
              </button>
            ))}
          </div>

          {creating ? (
            <div className="mt-2 flex items-center gap-2">
              <Input
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void createAndChoose();
                }}
                placeholder="Workspace name"
                className="h-7 text-xs"
              />
              <Button
                size="sm"
                className="h-7 px-2 text-xs"
                onClick={createAndChoose}
                disabled={busy || !name.trim()}
              >
                {busy ? <Loader2 className="size-3 animate-spin" /> : null}
                Create
              </Button>
            </div>
          ) : (
            <Button
              variant="ghost"
              size="sm"
              className="mt-1 h-7 w-full justify-start px-2 text-xs"
              onClick={() => setCreating(true)}
            >
              <Plus className="size-3.5" />
              New workspace…
            </Button>
          )}
        </div>

        {/*
          Only the action here. The hint that used to sit beside Cancel was
          guidance, not a control, so it took the footer's baseline and weight
          and read as a stray sentence stranded on the right — while saying
          the same thing as the title two inches above it.
        */}
        <DialogFooter className="border-t border-border px-5 py-3">
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
