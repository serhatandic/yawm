import { useCallback, useEffect, useRef, useState } from "react";
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
import { Separator } from "@/components/ui/separator";
import { api, type RemovalPlan, type Worktree } from "@/lib/api";
import {
  asFailure,
  branchNamesOf,
  confirmBlocked,
  deleteTitle,
  destroysWork,
  freshOptions,
  isPlanChanged,
  keptBranchesOf,
  lockedPlans,
  mergeKeptBranches,
  mergeUnverifiedBranches,
  offersBranchDeletion,
  plansMatchSelection,
  remainingPlans,
  removalRequestsFor,
  replanOptions,
  replanTargets,
  unverifiedBranchesOf,
  vanishedFrom,
  type DeleteOptions,
  type DeletionResult,
  type UnverifiedBranch,
} from "@/components/delete-rules";
import { cn, humanBytes } from "@/lib/utils";
import { middleTruncate } from "@/lib/layout";
import { AlertTriangle, Loader2, Lock, Trash2 } from "lucide-react";

/**
 * Confirmation for deleting worktrees.
 *
 * The plan comes from core and states exactly what would be lost. Force is
 * never implicit: when anything irreplaceable is at risk the confirm button
 * stays disabled until the user acknowledges it explicitly.
 */
export function DeleteDialog({
  open,
  repo,
  worktrees,
  onClose,
  onDone,
}: {
  open: boolean;
  repo: string;
  worktrees: Worktree[];
  onClose: () => void;
  /**
   * What is actually gone, and the branches git refused to delete.
   *
   * Never "the selection": a batch that failed part-way removed some of it and
   * cannot put any of it back, and the tabs to close and the rows to drop are
   * exactly the ones that went. Reporting the selection there would close tabs
   * for worktrees that are still on disk; reporting nothing would leave tabs
   * open on worktrees that are not.
   *
   * `vanished` is the same fact with a different author — worktrees removed
   * from outside yawm while the dialog was open — kept apart so nothing
   * downstream reports them as deletions this app carried out.
   *
   * Called more than once when a batch fails part-way and the user retries the
   * rest. `keptBranches` is cumulative over the whole dialog for that reason:
   * a branch refused in the first attempt is still undeleted after the second.
   */
  onDone: (result: DeletionResult) => void;
}) {
  const [plans, setPlans] = useState<RemovalPlan[] | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [options, setOptions] = useState<DeleteOptions>(freshOptions);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Worktrees of this selection that a part-way failure already removed. They
  // stay named here because the dialog is still open over the rest, and
  // everything it does next has to leave them out.
  const [removed, setRemoved] = useState<string[]>([]);
  /*
   * Worktrees of this selection the repository no longer has, which yawm did
   * not remove.
   *
   * The selection was fixed when the dialog opened, and it can stop being true
   * while the dialog is open: something outside yawm removes one of the chosen
   * worktrees, and core's refusal names what the repository actually has. Kept
   * out of the selection from then on — left in it, the next confirm would
   * validate against a selection naming a worktree that is not there, re-plan,
   * receive the same answer, and ask the user about it again forever.
   */
  const [vanished, setVanished] = useState<string[]>([]);
  /*
   * Every branch git declined to delete, over the whole life of this dialog.
   *
   * A dialog reports more than once — a batch fails part-way and the user
   * retries the rest — and each report answers only about what that attempt
   * touched. Sending the app the latest answer replaced the earlier one, so a
   * branch refused in the first attempt stopped being mentioned as soon as a
   * second finished, while the branch was still sitting there.
   */
  const [keptBranches, setKeptBranches] = useState<string[]>([]);
  /*
   * Every branch this dialog was left unable to vouch for.
   *
   * Accumulated separately from the kept ones because it is the opposite news:
   * a kept branch is still there, and these may not be. Folding them together
   * would have said "kept" about a branch a failed rollback may have destroyed.
   */
  const [unverifiedBranches, setUnverifiedBranches] = useState<
    UnverifiedBranch[]
  >([]);

  // Guards against a slow plan for a dialog that has since been closed or
  // pointed at something else overwriting the current one. Same pattern as
  // CreateDialog.
  const planToken = useRef(0);

  const { deleteBranch, forceBranch, useTrash } = options;
  function setOption<K extends keyof DeleteOptions>(
    key: K,
    value: DeleteOptions[K],
  ) {
    setOptions((prev) => ({ ...prev, [key]: value }));
  }

  const loadPlans = useCallback(
    async (paths: string[]) => {
      const token = ++planToken.current;
      setPlans(null);
      /*
       * A fresh plan describes different worktrees than the ones that were
       * acknowledged, so no acknowledgement carries over to it — and neither
       * does any option that authorises destroying something named.
       *
       * "Also delete the branch feat/a" was ticked against the selection as it
       * was. The whole reason a re-plan happens is that the selection is not
       * what it was, so re-running with that tick still on could delete a
       * branch nobody chose: what the checkbox says and what the request would
       * do are read from two different moments. The unlock matters most — a
       * lock that now says something else is a different instruction from the
       * one that was agreed to — but it was never the only one.
       *
       * Done here, on the request itself, rather than in a render or an effect
       * that watches the props: the app repaints steadily while the background
       * pass settles, and resetting on those repaints would untick boxes under
       * the user's cursor on the screen that authorises deletion.
       */
      setAcknowledged(false);
      setOptions(replanOptions);
      try {
        const next = await api.planRemovals(repo, paths);
        if (token !== planToken.current) return;
        setPlans(next);
      } catch (e) {
        if (token === planToken.current) setError(String(e));
      }
    },
    [repo],
  );

  /*
   * Keyed on the paths themselves, not the array holding them.
   *
   * The caller builds this array inline, so it is a new object on every render
   * of the app — and the app re-renders steadily while the background pass
   * resolves each worktree's landing. Depending on the array meant every one of
   * those renders re-ran this effect, which clears the plans and the
   * acknowledgement: the dialog flickered and quietly unticked the boxes the
   * user had just ticked, on the screen that authorises deletion.
   */
  const paths = worktrees.map((w) => w.path).join("\u0000");

  useEffect(() => {
    // Bumped even on close, so plans for a dialog the user has left behind can
    // never land on the next one.
    planToken.current += 1;
    if (!open) return;
    setOptions(freshOptions());
    setError(null);
    setRemoved([]);
    setVanished([]);
    setKeptBranches([]);
    setUnverifiedBranches([]);
    void loadPlans(paths.length > 0 ? paths.split("\u0000") : []);
  }, [open, paths, loadPlans]);

  const destructive = plans?.some(destroysWork);
  const atRisk = plans?.filter(destroysWork) ?? [];
  const locked = lockedPlans(plans ?? []);
  const blocked = confirmBlocked(plans, acknowledged, options);

  // What this dialog is still about. A part-way failure took some of the
  // selection with it, and something outside yawm may have taken more, and
  // everything from here — the size it claims to reclaim, the paths it
  // validates against, the branches it offers to delete — has to be about the
  // rest.
  const selected = worktrees.filter(
    (w) => !removed.includes(w.path) && !vanished.includes(w.path),
  );
  const reclaim = selected.reduce(
    (sum, w) => sum + (w.status.size?.bytes ?? 0),
    0,
  );

  /**
   * Hand the app what is gone, with every kept branch this dialog has heard of.
   *
   * One dialog can report several times, and each report answers only about the
   * worktrees that attempt touched. The branches git refused to delete are
   * cumulative — the first attempt's refusal is still true after the second —
   * so they are accumulated here and sent whole, rather than each report
   * overwriting the last.
   */
  function report(
    gone: { removed: string[]; vanished: string[] },
    kept: string[],
    unverified: UnverifiedBranch[] = [],
  ) {
    const merged = mergeKeptBranches(keptBranches, kept);
    const mergedUnverified = mergeUnverifiedBranches(
      unverifiedBranches,
      unverified,
    );
    setKeptBranches(merged);
    setUnverifiedBranches(mergedUnverified);
    onDone({
      ...gone,
      keptBranches: merged,
      unverifiedBranches: mergedUnverified,
    });
  }

  async function confirm() {
    if (!plans || plans.length === 0) return;
    const paths = selected.map((w) => w.path);
    if (!plansMatchSelection(plans, paths)) {
      setError(
        "These plans no longer describe the selected worktrees. Nothing was deleted — checking again.",
      );
      void loadPlans(paths);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      /*
       * One call for the whole selection.
       *
       * Removing them one at a time meant a selection where the second plan
       * had gone stale still lost the first: it was deleted before the second
       * was even looked at, and the user was then shown a refusal saying
       * nothing had been deleted. Core validates every plan before it touches
       * any of them, so this either deletes the whole selection or none of it.
       */
      const outcomes = await api.removeWorktrees(
        repo,
        removalRequestsFor(plans, options),
      );
      // git declining to delete an unmerged branch is the safety net working,
      // and invisible unless it is carried back out of here.
      const results = plans.flatMap((plan, i) => {
        const outcome = outcomes[i];
        return outcome ? [{ path: plan.path, outcome }] : [];
      });
      report(
        { removed: results.map((r) => r.path), vanished: [] },
        keptBranchesOf(plans, results),
        unverifiedBranchesOf(plans, results),
      );
      onClose();
    } catch (e) {
      const failure = asFailure(e);

      /*
       * Some of the selection is gone and cannot come back.
       *
       * This is the one failure that must not be reported as a failure alone.
       * Core validates the whole batch before it deletes anything, but once
       * the first directory has actually gone there is no rolling back, and a
       * bare error here would leave the app listing worktrees that no longer
       * exist with their diff tabs open on patches that can never be fetched
       * again — while the message said the deletion had not happened.
       *
       * So what did happen is handed out immediately, and only the rest stays
       * in the dialog with the reason it stopped.
       *
       * `completed` includes the worktrees core found gone while reconciling
       * the failure — trashed, say, with the prune that should have followed
       * never running. They are gone, so they belong here; what did not happen
       * afterwards is in the message core wrote.
       *
       * `vanished` is the other half: worktrees that were already gone when
       * core reached for them, removed by something outside yawm while the
       * batch ran. They leave the dialog the same way, but they are reported
       * as vanished — telling the user yawm deleted them would be a claim yawm
       * cannot make, and would hide that something else is writing here.
       */
      if (failure.kind === "partial") {
        const done = failure.completed.map((c) => c.path);
        report(
          { removed: done, vanished: failure.vanished },
          keptBranchesOf(plans, failure.completed),
          unverifiedBranchesOf(plans, failure.completed),
        );
        setRemoved((before) => [...before, ...done]);
        setVanished((before) => [...before, ...failure.vanished]);
        setPlans(remainingPlans(plans, [...done, ...failure.vanished]));
        setError(failure.message);
        return;
      }

      /*
       * Nothing yawm deleted, and part of the selection gone anyway.
       *
       * Something outside yawm removed those worktrees while the batch ran.
       * The rows and the diff tabs are as stale as after a partial removal and
       * have to go the same way — but no removal is reported, because yawm
       * performed none. Left as a generic failure, this was prose the dialog
       * could not act on: the worktrees stayed listed, their tabs stayed open,
       * and every retry asked core about a path that is not there.
       */
      if (failure.kind === "vanished") {
        report({ removed: [], vanished: failure.vanished }, []);
        setVanished((before) => [...before, ...failure.vanished]);
        const left = remainingPlans(plans, failure.vanished);
        setPlans(left);
        setError(
          left.length === 0
            ? "These worktrees are already gone — something outside yawm removed them. Nothing was deleted here."
            : failure.message,
        );
        return;
      }

      // Core refused because a worktree changed under its plan. It refuses the
      // batch before it deletes anything, so nothing in the selection was
      // deleted: the dialog re-plans over what the repository still has and
      // asks again instead of leaving a dead end they can only retry blindly.
      setError(failure.message);
      if (isPlanChanged(failure)) {
        /*
         * Split against the worktrees core says the repository still has, read
         * from the same snapshot as the refusal. Filtering against the app's
         * own list instead was a guess at a moment that had already passed, so
         * a worktree deleted from outside yawm was still re-planned — and core
         * answering "that is not a worktree of this repository" replaced a
         * refusal the user could act on with what reads like a bug.
         */
        const targets = replanTargets(paths, failure.stillPresent);
        const missing = vanishedFrom(paths, failure.stillPresent);

        if (missing.length > 0) {
          /*
           * Dropped from the selection, so the next confirm is about exactly
           * the worktrees that were re-planned. Left in, `plansMatchSelection`
           * would fail against a path no plan can be built for, re-plan,
           * receive the same answer, and ask about it again without end.
           *
           * Reported as `vanished`, never as `removed`: their diff tabs are
           * just as stale and the list needs rescanning, but yawm did not
           * delete them and must not say it did.
           */
          setVanished((before) => [...before, ...missing]);
          report({ removed: [], vanished: missing }, []);
        }

        if (targets.length > 0) {
          void loadPlans(targets);
          return;
        }

        /*
         * Nothing is left to delete. An empty plan list is what the dialog
         * shows and what blocks its Delete button, rather than a re-plan of
         * nothing or a silent close: the user asked for a deletion and is owed
         * the reason it is not happening.
         */
        setPlans([]);
        setError(
          missing.length === paths.length
            ? "These worktrees are already gone — something outside yawm removed them. Nothing was deleted here."
            : `${failure.message} Nothing in this selection is left to delete.`,
        );
      }
    } finally {
      setBusy(false);
    }
  }

  /*
   * Read off the plans, never off the worktree list this dialog was opened
   * with.
   *
   * The two disagree exactly where it costs: a re-plan replaces the plans
   * without the props changing, so the checkbox could name branch A while the
   * request it authorises would delete branch B. What the user is asked about
   * and what would happen have to come from the same answer.
   */
  const branchNames = branchNamesOf(plans ?? []);
  const canDeleteBranch = offersBranchDeletion(plans ?? []);

  const title = deleteTitle(selected);

  return (
    <Dialog open={open} onOpenChange={(o) => !o && !busy && onClose()}>
      <DialogContent
        className="flex max-h-[calc(100dvh-3rem)] max-w-lg flex-col gap-0 p-0"
        onEscapeKeyDown={(event) => busy && event.preventDefault()}
        onPointerDownOutside={(event) => busy && event.preventDefault()}
      >
        {/*
          `pr-10` leaves the close control a column of its own. A generated
          branch name is long and this title contains one, so without it the
          last word of the title ran under the X.
        */}
        <DialogHeader className="shrink-0 px-5 pt-5 pr-10 pb-3">
          <DialogTitle className="text-sm">{title}</DialogTitle>
          <DialogDescription className="text-xs">
            {selected.length === 0
              ? "Nothing in this selection is still there to delete."
              : `Reclaims about ${humanBytes(reclaim)}.`}
          </DialogDescription>
        </DialogHeader>

      {/*
        Bounded by the window, not by a fixed height.

        `max-h-80` was a guess that held only while the plan was short: twelve
        worktrees, each listing its dirty files, made a dialog taller than the
        window it opened in — and what fell off the bottom was the
        acknowledgement checkbox and the Delete button, on the one screen where
        every gate has to stay reachable. The body scrolls and the header and
        footer stay put instead.
      */}
      <div className="min-h-0 flex-1 overflow-y-auto px-5 pb-4">
        {plans === null ? (
          // A plan that failed or never came back must stop claiming it is
          // still checking; the message at the foot says what happened, and
          // Delete stays disabled because nothing was ever established.
          error ? null : (
            <p className="text-xs text-muted-foreground">Checking…</p>
          )
        ) : (
          <div className="space-y-3">
            {plans.map((plan) => (
              <PlanSummary key={plan.path} plan={plan} />
            ))}
          </div>
        )}

        {destructive ? (
          <div className="mt-4 rounded-md border border-destructive/40 bg-destructive/10 p-3">
            <div className="flex gap-2">
              <AlertTriangle className="mt-0.5 size-3.5 shrink-0 text-destructive-strong" />
              <div className="space-y-2">
                <p className="text-xs font-medium text-destructive-strong">
                  {/*
                    A claim about this worktree, not about the content.

                    "Exists nowhere else" was asserted here and nowhere
                    checked: a worktree whose every uncommitted line was
                    already on the default branch still said it. The working
                    state genuinely does exist only here — that is what a
                    directory of uncommitted edits is — and it is the part
                    deleting actually takes, so that is what this says.
                  */}
                  {atRisk.length === plans?.length
                    ? "This destroys work that exists only in this worktree."
                    : `${atRisk.length} of these destroy work that exists only in that worktree.`}
                </p>
                {/* One acknowledgement, but only because every worktree at
                    risk is named and itemised directly above it. A checkbox
                    per plan would turn into a ritual people click through. */}
                {atRisk.length > 1 ? (
                  <p className="text-[11px] text-destructive-strong">
                    {atRisk.map((p) => p.branch ?? p.path).join(", ")}
                  </p>
                ) : null}
                <label className="flex cursor-pointer items-start gap-2 text-xs text-foreground">
                  <Checkbox
                    checked={acknowledged}
                    onCheckedChange={(v) => setAcknowledged(v === true)}
                    className="mt-0.5 size-3.5"
                  />
                  <span>I understand, delete it anyway</span>
                </label>
              </div>
            </div>
          </div>
        ) : null}

        {/*
          A lock gets its own panel and its own acknowledgement.

          Everything else in this dialog is a consequence being pointed out —
          these files are uncommitted, these commits are unpushed. A lock is
          the one thing somebody set on purpose, and it usually says why:
          "agent running", "release in progress". Folding it into the
          acknowledgement above meant confirming a sentence about edited files
          also lifted an instruction the user may never have read, and the
          removal then passed `--force --force` and said nothing.
        */}
        {locked.length > 0 ? (
          <div className="mt-4 rounded-md border border-review/40 bg-review/10 p-3">
            <div className="flex gap-2">
              <Lock className="mt-0.5 size-3.5 shrink-0 text-review" />
              <div className="space-y-2">
                <p className="text-xs font-medium text-review">
                  {locked.length === 1
                    ? "This worktree is locked."
                    : `${locked.length} of these worktrees are locked.`}
                </p>
                {/* The reason is the message somebody left for whoever came
                    next, so it is quoted rather than summarised. */}
                <ul className="space-y-0.5">
                  {locked.map((plan) => (
                    <li key={plan.path} className="text-[11px] text-review/90">
                      <span className="font-mono">
                        {plan.branch ?? middleTruncate(plan.path, 40)}
                      </span>
                      {plan.lockReason
                        ? ` — ${plan.lockReason}`
                        : " — locked without a stated reason"}
                    </li>
                  ))}
                </ul>
                <label className="flex cursor-pointer items-start gap-2 text-xs text-foreground">
                  <Checkbox
                    checked={options.unlockLocked}
                    onCheckedChange={(v) =>
                      setOption("unlockLocked", v === true)
                    }
                    className="mt-0.5 size-3.5"
                  />
                  <span>
                    {locked.length === 1
                      ? "I understand, unlock and delete it"
                      : "I understand, unlock and delete them"}
                  </span>
                </label>
              </div>
            </div>
          </div>
        ) : null}

        <Separator className="my-4" />

        {/*
          The two recoverability options are exclusive, not merely explained.

          Trash exists so the directory can be fetched back, and deleting the
          branch takes away what fetching it back is for: what returns is a
          folder git no longer knows is a worktree, on a branch that is gone,
          holding commits nothing points at. They were plain independent
          checkboxes, so the only way to learn they interact was to pick both
          and find out afterwards. Each now switches the other off, and the core
          refuses the pair as well — a rule that lives only in this dialog is a
          rule the next caller does not have.
        */}
        <div className="space-y-2">
          <label
            className={cn(
              "flex items-center gap-2 text-xs",
              deleteBranch ? "cursor-not-allowed" : "cursor-pointer",
            )}
          >
            <Checkbox
              checked={useTrash}
              disabled={deleteBranch}
              onCheckedChange={(v) => setOption("useTrash", v === true)}
              className="size-3.5"
            />
            <span className={cn(deleteBranch && "text-muted-foreground")}>
              Move to Trash instead of deleting
              <span className="ml-1 text-muted-foreground">
                {deleteBranch
                  ? "(not while the branch is being deleted)"
                  : "(recoverable)"}
              </span>
            </span>
          </label>
          {/*
            Hidden outright when the selection has no branch in it.

            A detached worktree has nothing here to delete, and the checkbox
            still read "Also delete the branch" — an option that named nothing,
            and which ticked would have done nothing. Offering it invites the
            belief that something extra is being cleaned up, on the one screen
            where what is about to be destroyed has to be stated exactly.
          */}
          {canDeleteBranch ? (
            <label
              className={cn(
                "flex items-center gap-2 text-xs",
                useTrash ? "cursor-not-allowed" : "cursor-pointer",
              )}
            >
              <Checkbox
                checked={deleteBranch}
                disabled={useTrash}
                onCheckedChange={(v) => setOption("deleteBranch", v === true)}
                className="size-3.5"
              />
              {/* Named and counted, because agreeing to destroy something
                  unnamed is not agreement — and a selection of three where
                  only two are on a branch must say two. */}
              <span className={cn(useTrash && "text-muted-foreground")}>
                {branchNames.length === 1 ? (
                  <>
                    Also delete the branch{" "}
                    <span className="font-mono">{branchNames[0]}</span>
                  </>
                ) : (
                  <>
                    Also delete {branchNames.length} branches
                    <span
                      className="ml-1 text-muted-foreground"
                      title={branchNames.join("\n")}
                    >
                      ({branchNames.slice(0, 2).join(", ")}
                      {branchNames.length > 2 ? ", …" : ""})
                    </span>
                  </>
                )}
              </span>
            </label>
          ) : null}
          {/*
            Only offered once branch deletion is chosen, and never pre-checked:
            unmerged commits can exist nowhere else, so git refusing is the
            good outcome unless someone says otherwise in as many words.
          */}
          {canDeleteBranch && deleteBranch ? (
            <label className="ml-6 flex cursor-pointer items-center gap-2 text-xs">
              <Checkbox
                checked={forceBranch}
                onCheckedChange={(v) => setOption("forceBranch", v === true)}
                className="size-3.5"
              />
              <span>
                Even if it has unmerged commits
                <span className="ml-1 text-review">
                  (they would be lost)
                </span>
              </span>
            </label>
          ) : null}
        </div>

        {error ? (
          <p className="mt-3 text-xs text-destructive-strong">{error}</p>
        ) : null}
      </div>

        <DialogFooter className="shrink-0 border-t border-border px-5 py-3">
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            onClick={confirm}
            disabled={busy || plans === null || blocked}
          >
            {busy ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <Trash2 className="size-3.5" />
            )}
            Delete
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** What deleting one worktree would cost. */
function PlanSummary({ plan }: { plan: RemovalPlan }) {
  const losses: string[] = [];
  if (plan.dirtyTotal > 0) losses.push(`${plan.dirtyTotal} uncommitted changes`);
  if (plan.unpushedCommits > 0)
    losses.push(`${plan.unpushedCommits} unpushed commits`);
  if (plan.runningProcesses > 0)
    losses.push(`${plan.runningProcesses} running processes`);

  return (
    <div className="rounded-md border border-border p-2.5">
      <p className="truncate text-xs font-medium" title={plan.path}>
        {plan.branch ?? plan.path}
      </p>
      {/*
        Truncated from the middle, because the end is the part that matters.
        A worktree's directory is frequently named nothing like its branch —
        `feature-app-store-metadata` living in a folder called
        `feature-super-guide` — and trailing truncation cut the path off
        exactly at the folder name. Someone who moved that worktree to the
        Trash then had no way to know what to look for, and reasonably
        concluded the Trash option had silently done nothing.
      */}
      <p
        className="mt-0.5 truncate text-[11px] text-muted-foreground"
        title={plan.path}
      >
        {middleTruncate(plan.path, 64)}
      </p>

      {plan.isPrunable ? (
        <p className="mt-1.5 text-[11px] text-muted-foreground">
          Directory already gone — only stale metadata will be pruned.
        </p>
      ) : null}

      {/* Stated on the worktree itself as well as in the acknowledgement
          panel: this is the row the user is reading when they decide, and a
          lock is the one fact here that somebody set deliberately. */}
      {plan.isLocked ? (
        <p className="mt-1.5 text-[11px] text-review">
          Locked
          {plan.lockReason ? `: ${plan.lockReason}` : " (no reason given)"}
        </p>
      ) : null}

      {losses.length > 0 ? (
        <p className="mt-1.5 text-[11px] text-review">{losses.join(" · ")}</p>
      ) : null}

      {/* Files git has no copy of are never recoverable, so they are called
          out separately rather than folded into the change count. */}
      {plan.envFiles.length > 0 ? (
        <p className="mt-1 text-[11px] text-destructive-strong">
          {plan.envFiles.join(", ")} — not in git, exists nowhere else
        </p>
      ) : null}

      {plan.dirtyFiles.length > 0 ? (
        <ul className="mt-1.5 space-y-0.5">
          {plan.dirtyFiles.slice(0, 6).map((file) => (
            <li
              key={file}
              className="truncate font-mono text-[11px] text-muted-foreground"
            >
              {file}
            </li>
          ))}
          {plan.dirtyFiles.length > 6 ? (
            <li className="text-[11px] text-muted-foreground">
              …and {plan.dirtyFiles.length - 6} more
            </li>
          ) : null}
        </ul>
      ) : null}
    </div>
  );
}
