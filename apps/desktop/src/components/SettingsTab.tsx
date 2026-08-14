import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { api, type Config, sourceCount } from "@/lib/api";
import { rebaseConfig } from "@/components/settings-merge";
import { cn, FOCUS_RING } from "@/lib/utils";
import { AlertTriangle, Check, FolderPlus, Loader2, X } from "lucide-react";

/**
 * Preferences.
 *
 * A tab rather than a dialog: changing the staleness threshold changes
 * verdicts, and you want to watch that land. A modal would hide the thing you
 * are trying to observe.
 *
 * Every field says what it affects rather than only naming itself — "stale
 * after 14 days" is meaningless without knowing it drives the Review verdict.
 */
export function SettingsTab({ onSaved }: { onSaved: () => void }) {
  const [config, setConfig] = useState<Config | null>(null);
  /**
   * What was loaded, kept beside what is being edited.
   *
   * This tab stays mounted while hidden, so its copy of the settings goes stale
   * as soon as a repository is added anywhere else. Saving used to write the
   * whole snapshot back and erase that addition. Comparing against the loaded
   * copy is what makes it possible to tell a field the user changed from one
   * they merely happened to be holding.
   */
  const [base, setBase] = useState<Config | null>(null);
  const [revision, setRevision] = useState<number | null>(null);
  const [saved, setSaved] = useState(false);
  const [newWorkspace, setNewWorkspace] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .getConfig()
      .then((loaded) => {
        setConfig(loaded.config);
        setBase(loaded.config);
        setRevision(loaded.revision);
      })
      .catch((e) => setError(String(e)));
  }, []);

  function patch(changes: Partial<Config>) {
    setSaved(false);
    setConfig((c) => (c ? { ...c, ...changes } : c));
  }

  /**
   * Save, and if the settings moved underneath this tab, move the edit onto
   * them and save that instead.
   *
   * The retry is bounded: if it loses the race twice the user is told, which is
   * a worse outcome than saving and a far better one than silently deleting a
   * repository they added a minute ago.
   */
  async function save() {
    if (!config || !base) return;
    setBusy(true);
    setError(null);
    try {
      let attempt = config;
      let against = revision;
      let from = base;

      for (let tries = 0; tries < 2; tries += 1) {
        const result = await api.setConfig(attempt, against ?? undefined);
        if (result.outcome === "saved") {
          setConfig(attempt);
          setBase(attempt);
          setRevision(result.revision);
          setSaved(true);
          onSaved();
          return;
        }
        attempt = rebaseConfig(from, attempt, result.config);
        from = result.config;
        against = result.revision;
      }
      setError("Settings changed while you were editing. Press Save again.");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function addFolder(workspaceId: string, kind: "repos" | "scanRoots") {
    const picked = await openDialog({ directory: true, multiple: false });
    if (typeof picked !== "string" || !config) return;
    setSaved(false);
    setConfig({
      ...config,
      workspaces: config.workspaces.map((ws) =>
        ws.id === workspaceId && !ws[kind].includes(picked)
          ? { ...ws, [kind]: [...ws[kind], picked] }
          : ws,
      ),
    });
  }

  function removeFolder(workspaceId: string, kind: "repos" | "scanRoots", path: string) {
    if (!config) return;
    setSaved(false);
    setConfig({
      ...config,
      workspaces: config.workspaces.map((ws) =>
        ws.id === workspaceId
          ? { ...ws, [kind]: ws[kind].filter((p) => p !== path) }
          : ws,
      ),
    });
  }

  async function addWorkspace() {
    const name = newWorkspace.trim();
    if (!name) return;
    await api.createWorkspace(name);
    setNewWorkspace("");
    await reload();
  }

  async function removeWorkspace(id: string) {
    await api.deleteWorkspace(id);
    await reload();
  }

  async function reload() {
    const loaded = await api.getConfig();
    setConfig(loaded.config);
    setBase(loaded.config);
    setRevision(loaded.revision);
    onSaved();
  }

  if (!config) {
    /*
      Two different states, told apart.

      A failure to read the settings rendered as the same grey line as the
      moment before they arrive — so a broken settings file looked like a slow
      one, indefinitely. Loading says work is happening; the failure says what
      happened and offers the only thing that can change it.
    */
    if (error) {
      return (
        <div className="flex h-full items-center justify-center p-6">
          <div className="flex max-w-md flex-col items-center gap-3 text-center">
            <AlertTriangle className="size-5 text-destructive-strong" />
            <div className="space-y-1">
              <p className="text-xs font-medium">Settings could not be read</p>
              <p className="text-[11px] break-words text-muted-foreground">
                {error}
              </p>
            </div>
            <Button
              size="sm"
              variant="secondary"
              onClick={() => {
                setError(null);
                void reload().catch((e) => setError(String(e)));
              }}
            >
              Try again
            </Button>
          </div>
        </div>
      );
    }

    return (
      <p className="flex items-center gap-2 p-6 text-xs text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" />
        Loading…
      </p>
    );
  }

  return (
    <div className="flex h-full flex-col">
      {/*
        Native overflow, not a Radix ScrollArea.

        This was the one scroller in the app built out of a component, so it
        had its own scrollbar with its own dimensions while every other pane
        used the styled system one — and its viewport swallowed the sticky Save
        footer's shadow at the boundary. The footer below is untouched: it is
        outside the scroller, which is what keeps Save reachable at any length.
      */}
      <div className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto">
        <div className="mx-auto max-w-xl space-y-5 p-6">
          <div>
            <div className="mb-1.5">
              <p className="text-xs font-medium">Workspaces</p>
              <p className="text-[11px] text-muted-foreground">
                Separate sets of repositories, so unrelated work never shares one
                list. Removing a workspace only forgets it here — nothing on
                disk is touched.
              </p>
            </div>

            <div className="space-y-3">
              {config.workspaces.map((ws) => (
                <div
                  key={ws.id}
                  className="rounded-md border border-border p-2.5"
                >
                  <div className="mb-2 flex items-center gap-2">
                    {/*
                      The name is a title you can edit, not a form field to
                      fill in — so it carries a title's weight and only takes
                      an input's chrome once you reach for it.
                    */}
                    <Input
                      value={ws.name}
                      aria-label="Workspace name"
                      onChange={(e) => {
                        setSaved(false);
                        setConfig({
                          ...config,
                          workspaces: config.workspaces.map((w) =>
                            w.id === ws.id ? { ...w, name: e.target.value } : w,
                          ),
                        });
                      }}
                      className="h-7 max-w-56 border-transparent bg-transparent px-1.5 text-xs font-medium shadow-none hover:border-input dark:bg-transparent dark:hover:bg-input/30"
                    />
                    <span className="text-[11px] text-muted-foreground">
                      {sourceCount(ws)}
                    </span>
                    {config.workspaces.length > 1 ? (
                      <Button
                        variant="ghost"
                        size="sm"
                        className="ml-auto h-7 px-2 text-xs text-muted-foreground hover:text-destructive-strong"
                        onClick={() => void removeWorkspace(ws.id)}
                      >
                        Remove
                      </Button>
                    ) : null}
                  </div>

                  <SourceList
                    label="Repositories"
                    paths={ws.repos}
                    onAdd={() => void addFolder(ws.id, "repos")}
                    onRemove={(p) => removeFolder(ws.id, "repos", p)}
                  />
                  <SourceList
                    label="Scanned folders"
                    paths={ws.scanRoots}
                    onAdd={() => void addFolder(ws.id, "scanRoots")}
                    onRemove={(p) => removeFolder(ws.id, "scanRoots", p)}
                  />
                </div>
              ))}
            </div>

            <div className="mt-2 flex items-center gap-2">
              <Input
                value={newWorkspace}
                onChange={(e) => setNewWorkspace(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void addWorkspace();
                }}
                placeholder="New workspace name"
                className="h-7 max-w-56 text-xs"
              />
              <Button
                size="sm"
                variant="secondary"
                className="h-7 px-2 text-xs"
                onClick={addWorkspace}
                disabled={!newWorkspace.trim()}
              >
                <FolderPlus className="size-3" />
                Add
              </Button>
            </div>
          </div>

          <Separator />

          <Field
            label="Editor command"
            hint="Left empty, yawm uses whatever your system opens folders with."
          >
            <Input
              value={config.editor ?? ""}
              onChange={(e) => patch({ editor: e.target.value || null })}
              placeholder="code, cursor, zed…"
            />
          </Field>

          <Field
            label="New worktree location"
            hint="{repo} and {branch} are substituted. Relative paths resolve against the repository."
          >
            <Input
              value={config.worktreePathTemplate}
              onChange={(e) => patch({ worktreePathTemplate: e.target.value })}
              className="font-mono text-[11px]"
            />
          </Field>

          <Separator />

          <Field
            label="In use for"
            hint="A worktree touched this recently is kept, on the assumption something is still working in it."
          >
            <Number
              value={config.activeWithinMinutes}
              onChange={(v) => patch({ activeWithinMinutes: v })}
              unit="minutes"
            />
          </Field>

          <Separator />

          <div>
            <p className="mb-2 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
              Carry over by default
            </p>
            <div className="space-y-2">
              <Toggle
                checked={config.provisioning.copyEnvFiles}
                onChange={(v) =>
                  patch({
                    provisioning: { ...config.provisioning, copyEnvFiles: v },
                  })
                }
                label="Environment files"
                hint="Gitignored, so a new worktree has none without this."
              />
              <Toggle
                checked={config.provisioning.linkDependencies}
                onChange={(v) =>
                  patch({
                    provisioning: { ...config.provisioning, linkDependencies: v },
                  })
                }
                label="Dependency directories"
                hint="Linked, not copied, and only when the lockfiles agree."
              />
              <Toggle
                checked={config.provisioning.honourWorktreeinclude}
                onChange={(v) =>
                  patch({
                    provisioning: {
                      ...config.provisioning,
                      honourWorktreeinclude: v,
                    },
                  })
                }
                label="Honour .worktreeinclude"
                hint="A repository's own list of files to carry over."
              />
            </div>
          </div>

          {error ? (
            <p className="text-xs text-destructive-strong">{error}</p>
          ) : null}
        </div>
      </div>

      <div className="flex shrink-0 items-center gap-2 border-t border-border px-6 py-3">
        {saved ? (
          <span className="flex items-center gap-1.5 text-xs text-disposable">
            <Check className="size-3" />
            Saved
          </span>
        ) : null}
        <Button className="ml-auto" onClick={save} disabled={busy}>
          {busy ? <Loader2 className="size-3.5 animate-spin" /> : null}
          Save
        </Button>
      </div>
    </div>
  );
}

function SourceList({
  label,
  paths,
  onAdd,
  onRemove,
}: {
  label: string;
  paths: string[];
  onAdd: () => void;
  onRemove: (path: string) => void;
}) {
  return (
    <div className="mt-1.5">
      <div className="flex items-center gap-2">
        <p className="text-[11px] text-muted-foreground">{label}</p>
        <Button
          variant="ghost"
          size="sm"
          className="ml-auto h-6 px-1.5 text-[11px]"
          onClick={onAdd}
        >
          <FolderPlus className="size-3" />
          Add
        </Button>
      </div>
      {paths.length === 0 ? (
        <p className="text-[11px] text-muted-foreground">None.</p>
      ) : (
        <ul className="space-y-0.5">
          {paths.map((path) => (
            <li key={path} className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">
                {path}
              </span>
              <button
                onClick={() => onRemove(path)}
                className={cn(
                  "flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-muted/60 hover:text-destructive-strong",
                  FOCUS_RING,
                )}
                aria-label={`Remove ${path}`}
              >
                <X className="size-3" />
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint: string;
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-1">
      <p className="text-xs font-medium">{label}</p>
      {children}
      <p className="text-[11px] text-muted-foreground">{hint}</p>
    </div>
  );
}

function Number({
  value,
  onChange,
  unit,
}: {
  value: number;
  onChange: (value: number) => void;
  unit: string;
}) {
  return (
    <div className="flex items-center gap-2">
      <Input
        type="number"
        min={1}
        value={value}
        onChange={(e) => {
          const next = parseInt(e.target.value, 10);
          // An empty or nonsense entry must not silently become zero, which
          // would make every worktree read as permanently active or stale.
          if (!isNaN(next) && next > 0) onChange(next);
        }}
        className="w-24"
      />
      <span className="text-[11px] text-muted-foreground">{unit}</span>
    </div>
  );
}

function Toggle({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  hint: string;
}) {
  return (
    <label className="flex cursor-pointer items-start gap-2">
      <Checkbox
        checked={checked}
        onCheckedChange={(v) => onChange(v === true)}
        className="mt-0.5 size-3.5"
      />
      <span className="min-w-0 flex-1">
        <span className="block text-xs">{label}</span>
        <span className="block text-[11px] text-muted-foreground">{hint}</span>
      </span>
    </label>
  );
}
