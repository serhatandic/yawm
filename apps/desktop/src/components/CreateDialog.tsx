import { useCallback, useEffect, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { api, type CreatePlan, type RepoReport } from "@/lib/api";
import { joinPath, parentPath, pathName } from "@/lib/paths";
import { cn, humanBytes, FOCUS_RING } from "@/lib/utils";
import { AlertTriangle, FolderTree, Link2, Loader2, Plus } from "lucide-react";

/**
 * Creating a worktree that actually works.
 *
 * `git worktree add` leaves you with a checkout that has no `.env` and no
 * dependencies, which is the most reported friction with worktrees. Everything
 * yawm finds in the main worktree is offered here already ticked, so the common
 * case is to type a branch name and press Create.
 */

/** Shown in the branch field, and reused to reveal where that branch would land. */
const EXAMPLE_BRANCH = "feat/my-change";

export function CreateDialog({
  open,
  repos,
  initialRepo,
  onClose,
  onCreated,
}: {
  open: boolean;
  repos: RepoReport[];
  initialRepo: string | null;
  onClose: () => void;
  onCreated: (repo: string) => void;
}) {
  const [repo, setRepo] = useState(initialRepo ?? repos[0]?.root ?? "");
  const [branch, setBranch] = useState("");
  const [base, setBase] = useState("");
  const [path, setPath] = useState("");
  const [pathEdited, setPathEdited] = useState(false);
  /*
   * Where this would go if nobody touched it.
   *
   * Held separately from the value so the field can show the shape it wants
   * before a branch has been typed. It used to be an empty box asking for an
   * absolute path with nothing to say what one should look like here.
   */
  const [suggested, setSuggested] = useState("");
  const [bases, setBases] = useState<string[]>([]);
  const [plan, setPlan] = useState<CreatePlan | null>(null);
  const [skipped, setSkipped] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Guards against a slow plan for an old branch name overwriting a newer one.
  const planToken = useRef(0);

  useEffect(() => {
    if (!open) return;
    setRepo(initialRepo ?? repos[0]?.root ?? "");
    setBranch("");
    setPath("");
    setPathEdited(false);
    setPlan(null);
    setSkipped(new Set());
    setError(null);
  }, [open, initialRepo, repos]);

  useEffect(() => {
    if (!open || !repo) return;
    api
      .listBaseRefs(repo)
      .then((refs) => {
        setBases(refs);
        setBase((current) =>
          current && refs.includes(current) ? current : (refs[0] ?? ""),
        );
      })
      .catch(() => setBases([]));
  }, [open, repo]);

  // Keep the path in step with the branch until the user takes it over.
  useEffect(() => {
    if (!open || !repo) return;
    const typed = branch.trim();
    /*
     * Asked for even when no branch has been typed, using the same example the
     * branch field shows, so the empty state can display where a worktree
     * would actually land instead of an unexplained blank.
     */
    api
      .suggestWorktreePath(repo, typed || EXAMPLE_BRANCH)
      .then((next) => {
        setSuggested(next);
        if (!pathEdited) setPath(typed ? next : "");
      })
      .catch(() => undefined);
  }, [open, repo, branch, pathEdited]);

  const refreshPlan = useCallback(async () => {
    if (!repo || !branch.trim() || !path) {
      setPlan(null);
      return;
    }
    const token = ++planToken.current;
    try {
      const next = await api.planCreation(repo, branch.trim(), base, path);
      if (token !== planToken.current) return;
      setPlan(next);
      // Start from the recommendation; the user adjusts from there.
      setSkipped(
        new Set(next.items.filter((i) => !i.recommended).map((i) => i.name)),
      );
    } catch (e) {
      if (token === planToken.current) setError(String(e));
    }
  }, [repo, branch, base, path]);

  // Debounced so typing a branch name does not plan on every keystroke.
  useEffect(() => {
    if (!open) return;
    const id = setTimeout(() => void refreshPlan(), 250);
    return () => clearTimeout(id);
  }, [open, refreshPlan]);

  function toggle(name: string) {
    setSkipped((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  }

  const blocked =
    !branch.trim() ||
    !path ||
    plan === null ||
    plan.branchInUseAt !== null ||
    plan.pathExists;

  async function submit() {
    if (blocked || plan === null) return;
    setBusy(true);
    setError(null);
    try {
      await api.createWorktree(repo, {
        branch: branch.trim(),
        base,
        path,
        provision: plan.items
          .filter((i) => !skipped.has(i.name))
          .map((i) => i.name),
      });
      onCreated(repo);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex max-h-[calc(100dvh-3rem)] max-w-xl flex-col gap-0 p-0">
        {/* `pr-10` so the title cannot run under the close control. */}
        <DialogHeader className="shrink-0 px-5 pt-5 pr-10 pb-3">
          <DialogTitle className="text-sm">New worktree</DialogTitle>
          <DialogDescription className="text-xs">
            Everything yawm finds in the main worktree is carried over unless
            you untick it.
          </DialogDescription>
        </DialogHeader>

        {/*
          A form, so Enter means Create.

          Typing a branch name and pressing Enter is the whole common case, and
          it did nothing at all — the dialog held a set of loose inputs. It
          submits through the same `blocked` gate the button is disabled by, so
          the key cannot do anything the button would refuse. Deliberately not
          mirrored in the delete dialog: there, an accidental Return is
          irreversible.
        */}
        <form
          className="flex min-h-0 flex-1 flex-col"
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto px-5 pb-4">
          {repos.length > 1 ? (
            <Field label="Repository">
              <Select
                value={repo}
                onValueChange={(value) => {
                  setRepo(value);
                  setPathEdited(false);
                }}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {repos.map((r) => (
                    <SelectItem key={r.root} value={r.root}>
                      {r.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          ) : null}

          <Field label="Branch">
            <Input
              autoFocus
              value={branch}
              onChange={(e) => setBranch(e.target.value)}
              placeholder={EXAMPLE_BRANCH}
            />
            {plan?.branchExists && !plan.branchInUseAt ? (
              <Hint>
                This branch already exists, so it will be checked out.
              </Hint>
            ) : null}
          </Field>

          <Field label="Starting from">
            <Select value={base} onValueChange={setBase}>
              <SelectTrigger className="w-full">
                <SelectValue placeholder="Choose a starting point" />
              </SelectTrigger>
              <SelectContent>
                {bases.map((ref) => (
                  <SelectItem key={ref} value={ref}>
                    {ref}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>

          <Field label="Location">
            {/*
            Three ways in, because typing an absolute path from memory is not
            one. It fills itself from the branch name, it shows where that
            would land before anything is typed, and the folder can be picked
            rather than spelled — the same picker the sidebar already uses to
            add a repository.
          */}
            <div className="flex gap-2">
              <Input
                value={path}
                placeholder={suggested}
                onChange={(e) => {
                  setPath(e.target.value);
                  setPathEdited(true);
                }}
                className="min-w-0 flex-1 font-mono text-[11px]"
              />
              <Button
                type="button"
                variant="outline"
                className="shrink-0"
                onClick={() => {
                  void (async () => {
                    const picked = await openDialog({
                      directory: true,
                      multiple: false,
                      // The parent of where it would have gone, so the picker
                      // opens beside the other worktrees rather than at home.
                      defaultPath: parentPath(suggested) || undefined,
                    });
                    if (typeof picked !== "string") return;
                    // A directory was chosen to put this *in*, so the worktree
                    // still needs its own name under it; picking the parent and
                    // getting a collision would be its own small insult.
                    const leaf = pathName(suggested);
                    setPath(joinPath(picked, leaf));
                    setPathEdited(true);
                  })();
                }}
              >
                Choose…
              </Button>
            </div>
            {pathEdited && suggested && path !== suggested ? (
              <button
                type="button"
                className={cn(
                  "mt-1 rounded-sm text-[11px] text-muted-foreground underline-offset-2 hover:underline",
                  FOCUS_RING,
                )}
                onClick={() => {
                  setPath(suggested);
                  setPathEdited(false);
                }}
              >
                Use the suggested location
              </button>
            ) : null}
          </Field>

          {plan?.branchInUseAt ? (
            <Warning tone="error">
              <span className="font-mono">{branch}</span> is already checked out
              at <span className="font-mono">{plan.branchInUseAt}</span>. Git
              allows a branch in only one worktree at a time.
            </Warning>
          ) : null}

          {plan?.pathExists ? (
            <Warning tone="error">That location already exists.</Warning>
          ) : null}

          {plan?.pathIsNested ? (
            <Warning tone="caution">
              This is inside the repository itself. Tools that search the
              repository will find both copies, which is how agents end up
              working in the wrong one.
            </Warning>
          ) : null}

          {plan && plan.items.length > 0 ? (
            <>
              <Separator />
              <div>
                <p className="mb-2 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
                  Carry over
                </p>
                <div className="space-y-1.5">
                  {plan.items.map((item) => (
                    <label
                      key={item.name}
                      className="flex cursor-pointer items-start gap-2"
                    >
                      <Checkbox
                        checked={!skipped.has(item.name)}
                        onCheckedChange={() => toggle(item.name)}
                        className="mt-0.5 size-3.5"
                      />
                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-1.5">
                          <span className="font-mono text-[11px]">
                            {item.name}
                          </span>
                          {item.kind === "linkDir" ? (
                            <span
                              className="flex items-center gap-0.5 text-[10px] text-muted-foreground"
                              title="Linked, so it costs no extra disk space"
                            >
                              <Link2 className="size-2.5" />
                              link
                              {item.bytes
                                ? ` · ${humanBytes(item.bytes)}+`
                                : ""}
                            </span>
                          ) : (
                            <span className="text-[10px] text-muted-foreground">
                              copy
                            </span>
                          )}
                        </span>
                        {item.caution ? (
                          <span className="mt-0.5 block text-[10px] text-review">
                            {item.caution}
                          </span>
                        ) : null}
                      </span>
                    </label>
                  ))}
                </div>
              </div>
            </>
          ) : null}

          {error ? (
            <p className="text-xs text-destructive-strong">{error}</p>
          ) : null}
        </div>

        <DialogFooter className="shrink-0 border-t border-border px-5 py-3">
          <Button
            type="button"
            variant="ghost"
            onClick={onClose}
            disabled={busy}
          >
            Cancel
          </Button>
          <Button type="submit" disabled={busy || blocked}>
            {busy ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <Plus className="size-3.5" />
            )}
            Create
          </Button>
        </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-1">
      <label className="text-[11px] font-medium text-muted-foreground">
        {label}
      </label>
      {children}
    </div>
  );
}

function Hint({ children }: { children: React.ReactNode }) {
  return <p className="text-[11px] text-muted-foreground">{children}</p>;
}

function Warning({
  tone,
  children,
}: {
  tone: "error" | "caution";
  children: React.ReactNode;
}) {
  const error = tone === "error";
  return (
    <div
      className={
        error
          ? "flex gap-2 rounded-md border border-destructive/40 bg-destructive/10 p-2.5"
          : "flex gap-2 rounded-md border border-review/40 bg-review/10 p-2.5"
      }
    >
      {error ? (
        <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-destructive-strong" />
      ) : (
        <FolderTree className="mt-0.5 size-3.5 shrink-0 text-review" />
      )}
      <p className="text-[11px] leading-relaxed">{children}</p>
    </div>
  );
}
