/**
 * The typed bridge to yawm-core.
 *
 * These types mirror the Rust structs, which serialise as camelCase. Every
 * judgement — the verdict, the reason, what a removal would cost — is computed
 * in core and only rendered here, so the desktop app and the `yawm` CLI can
 * never disagree.
 */

import { invoke } from "@tauri-apps/api/core";
import { deadlineFor, withDeadline } from "./deadline.ts";

export type Verdict = "broken" | "keep" | "disposable" | "review";

/**
 * Exactly what a comparison failed to read, mirroring `ComparisonShortfall`.
 *
 * `incomplete` alone says a comparison fell short and then declines to say by
 * how much, which leaves the reader with a warning they cannot act on. These
 * are the numbers core already holds while comparing, so the copy can name the
 * threshold that stopped it and the amount left unread.
 */
export interface ComparisonShortfall {
  /** Changed lines actually probed against the target. */
  linesCompared: number;
  /** Changed lines the comparison never looked at. */
  linesNotCompared: number;
  /**
   * The per-comparison line budget, set only when reaching it is what stopped
   * the walk. `null` means something else did, and quoting a threshold that was
   * never hit would misattribute the cause.
   */
  lineLimit: number | null;
  /** Paths whose contents could not be compared line by line at all. */
  pathsNotCompared: number;
  /** False when a listing failed, so the counts above are lower bounds. */
  countsExact: boolean;
}

export type VerdictReason =
  | { kind: "directoryMissing" }
  | { kind: "locked" }
  | { kind: "mainWorktree" }
  | { kind: "processRunning" }
  | { kind: "processCheckSkipped" }
  | { kind: "recentlyActive" }
  | { kind: "uncommittedChanges" }
  | {
      kind: "uncommittedChangesAtRisk";
      count: number;
      target: string;
      incomplete: boolean;
      /** Present exactly when `incomplete`. */
      shortfall: ComparisonShortfall | null;
    }
  | { kind: "uncommittedChangesOnDefault"; target: string }
  | { kind: "environmentFilesAtRisk"; count: number }
  | { kind: "workingTreeUnreadable" }
  | { kind: "unpushedCommits" }
  | { kind: "workContained"; target: string }
  | { kind: "defaultBranchLacksCommittedContent" }
  | { kind: "landingUnknown" };

export interface LockInfo {
  reason: string | null;
}

export interface DirtyCounts {
  staged: number;
  unstaged: number;
  untracked: number;
  /**
   * Distinct dirty paths, as core counted them from Git's own path bytes.
   *
   * The three dimensions above are not a file count and never were: one path
   * staged *and* modified is two of them, so adding them up reported a
   * worktree of 257 files as 404 "uncommitted files". This is the number the
   * reader is shown; the dimensions stay for the breakdown that explains it.
   */
  paths: number;
  inspectionFailed: boolean;
}

/**
 * How many files the dirty state actually covers.
 *
 * `paths` is serialised with a default, so a payload from an older core can
 * arrive as zero while the dimensions say otherwise. The fallback is the
 * largest single dimension — the smallest number of distinct paths that could
 * produce them — because the one thing that must never happen again is the
 * sum standing in for a file count.
 */
export function dirtyPathCount(dirty: DirtyCounts): number {
  if (dirty.paths > 0) return dirty.paths;
  return Math.max(dirty.staged, dirty.unstaged, dirty.untracked);
}

export interface UpstreamInfo {
  name: string | null;
  ahead: number;
  behind: number;
  gone: boolean;
}

export interface HeavyDir {
  name: string;
  bytes: number;
  isLink: boolean;
}

export interface SizeInfo {
  bytes: number;
  files: number;
  heavyDirs: HeavyDir[];
  lastModified: number | null;
}

export interface ProcessInfo {
  pid: number;
  name: string;
}

export type LandingProof =
  | { kind: "ancestry" }
  | { kind: "sameTree" }
  | { kind: "noOpAtTip" }
  | { kind: "noOpAtAncestor"; commit: string };

export type UnknownReason =
  | { kind: "notChecked" }
  | { kind: "noDefaultBranch" }
  | { kind: "headUnavailable" }
  | { kind: "targetUnavailable" }
  | { kind: "gitCommandFailed" }
  | { kind: "mergeTreeUnavailable" }
  | { kind: "malformedMergeTree" }
  | { kind: "checkDeferred" }
  | { kind: "overlappingChanges"; paths: number }
  | { kind: "historyRangeTooLarge"; commits: number; limit: number }
  | { kind: "customMergeDriver" }
  | { kind: "mergeAttributes" };

export type Landing =
  | { state: "landed"; target: string; proof: LandingProof }
  | { state: "addsContent"; target: string }
  | {
      state: "unknown";
      reason: UnknownReason;
      candidate: CandidateMatch | null;
    };

/**
 * A commit on the default branch that looks like this branch's work.
 *
 * Measurement, never a conclusion: the counts come from comparing the two
 * *changes* path by path, so a near-perfect match is strong evidence and still
 * not proof.
 */
export interface CandidateMatch {
  commit: string;
  target: string;
  paths: number;
  matchingPaths: number;
  added: number;
  matchingAdded: number;
  /** Lines of the branch's change missing from the default branch's copy of the same files. */
  leftover: number;
  leftoverSample: string[];
  /** The search stopped early, so "no leftovers" would mean "none looked at". */
  incomplete: boolean;
}

export interface WorktreeStatus {
  dirty: DirtyCounts;
  uncommitted: UncommittedAnalysis;
  upstream: UpstreamInfo;
  landing: Landing;
  /**
   * Unknown can be a finished answer or a deferred proof, so progress cannot be
   * inferred from the verdict without making an unresolved row look settled.
   */
  landingComplete: boolean;
  lastCommitAt: number | null;
  lastCommitSubject: string | null;
  envFiles: string[];
  size: SizeInfo | null;
  processes: ProcessInfo[];
  /** False when the process table was intentionally not inspected. */
  processCheckComplete: boolean;
}

export type UncommittedAnalysis =
  | { state: "notChecked" }
  | {
      state: "compared";
      target: string;
      leftover: number;
      leftoverSample: string[];
      incomplete: boolean;
      /** Present exactly when `incomplete`. */
      shortfall: ComparisonShortfall | null;
    };

/** `WorktreeEntry` is flattened into `Worktree` on the Rust side. */
export interface Worktree {
  path: string;
  head: string | null;
  branch: string | null;
  detached: boolean;
  bare: boolean;
  isMain: boolean;
  locked: LockInfo | null;
  prunable: string | null;
  status: WorktreeStatus;
  verdict: Verdict;
  reason: VerdictReason;
}

export interface RepoReport {
  name: string;
  root: string;
  defaultRef: string | null;
  worktrees: Worktree[];
}

/**
 * A configured source that could not be read this time round.
 *
 * Kept apart from the reports rather than folded into them, because "nothing
 * here" and "could not look" must never render as the same answer.
 */
export interface UnreadableSource {
  path: string;
  reason: string;
}

export interface ScanReport {
  repos: RepoReport[];
  unreadable: UnreadableSource[];
}

/**
 * What happened to the branch when a worktree was removed.
 *
 * `moved` is a refusal, not a failure: between the plan the user approved and
 * the moment of deletion the branch ref moved, so deleting it would have
 * discarded a commit nobody was shown. The worktree removal stands; the branch
 * is still there.
 *
 * `unknown` and `rollbackFailed` are the two answers that are not answers.
 * Core reaches them when branch finalisation broke before the resulting ref
 * state was established, or when a branch it had already deleted could not be
 * put back. Neither can be folded into `kept` or `notRequested`: both of those
 * promise the branch is exactly where the user left it, and here nobody knows.
 */
export type BranchOutcome =
  | "notRequested"
  | "deleted"
  | "kept"
  | "moved"
  | "unknown"
  | "rollbackFailed";

/**
 * The outcomes that leave the branch's real state unestablished.
 *
 * Named so the parser and the notices agree on the set, rather than each
 * listing it and drifting apart when core grows another one.
 */
export type UnverifiedBranchOutcome = "unknown" | "rollbackFailed";

export interface RemovalOutcome {
  branch: BranchOutcome;
}

/**
 * Whether a removal also finished everything that follows it.
 *
 * `removedButFinalizationFailed` is a worktree core found gone while
 * reconciling a failure: the directory or its metadata went, but the step that
 * would have pruned the rest and reported it never returned. It is gone either
 * way — the difference is what may still be left behind, which core's message
 * states.
 */
export type RemovalStatus = "removed" | "removedButFinalizationFailed";

/** One removal that actually happened, named. */
export interface CompletedRemoval {
  path: string;
  outcome: RemovalOutcome;
  status: RemovalStatus;
}

/**
 * How a command reports failure.
 *
 * `planChanged` is not a failure to show and stop on: nothing was deleted, and
 * the caller re-plans and asks again. It carries `stillPresent` — every
 * worktree the repository had at the moment of the refusal — so the re-plan is
 * aimed at what is there now instead of at a list the app painted earlier.
 *
 * `partial` is the opposite, and the one that must never be flattened into a
 * sentence: some of the selection is already gone and cannot be brought back.
 * The caller has to reconcile `completed` before it says anything about the
 * rest.
 *
 * `vanished` is `partial` with nothing of yawm's own to report: yawm deleted
 * nothing, and some of the selection is gone anyway because something else
 * removed it. The rows and tabs still have to go, so it is structured for the
 * same reason `partial` is — as a message it read as a generic failure and the
 * app kept listing directories that are not there.
 *
 * All four travel as their own kind so they are told apart structurally rather
 * than by reading the sentence meant for a human.
 */
export type CommandFailure =
  | {
      kind: "planChanged";
      message: string;
      path: string;
      changes: string[];
      stillPresent: string[];
    }
  | {
      kind: "partial";
      message: string;
      completed: CompletedRemoval[];
      /**
       * Worktrees that were gone before core reached them.
       *
       * Removed by something other than yawm while the batch ran. They are
       * gone and must leave the list, but they are not yawm's removals — the
       * distinction is what tells the user their repository is being written
       * to by something else.
       */
      vanished: string[];
      failed: string;
    }
  | {
      kind: "vanished";
      message: string;
      /**
       * Worktrees that were gone before core attempted anything. Never empty,
       * and never a removal this app carried out.
       */
      vanished: string[];
      failed: string;
    }
  | { kind: "failed"; message: string };

/**
 * The proof that the worktree core is about to delete is the one the user saw.
 *
 * Opaque and fixed-size on purpose. The plan's other fields are a summary for
 * the dialog — capped lists and counts — and a summary cannot carry an
 * authorisation, but neither does the authorisation need to be readable here:
 * the app never renders it and never inspects it, it carries it back unchanged
 * so the approval the user gave is the approval core acts on.
 *
 * It used to be the evidence itself — every dirty path, every file outside git,
 * uncapped — which made the size of a plan a function of how much work was in
 * the worktree. Selecting a worktree with ten thousand modified files put ten
 * thousand records through this boundary and then back again. Core keeps that
 * detail in its own process and sends the digest over it.
 *
 * An absent or default value has an empty `version` and authorises nothing.
 */
export interface StateFingerprint {
  /** Encoding version. Anything core does not recognise authorises nothing. */
  version: string;
  /** Digest over the exact state, computed and compared inside core. */
  digest: string;
  /** Whether anything in scope could not be established. `true` refuses. */
  unproven: boolean;
}

export interface RemovalPlan {
  path: string;
  branch: string | null;
  isMain: boolean;
  isLocked: boolean;
  lockReason: string | null;
  isPrunable: boolean;
  /** Capped for display. Never the basis of an authorisation — that is `state`. */
  dirtyFiles: string[];
  dirtyTotal: number;
  unpushedCommits: number;
  envFiles: string[];
  runningProcesses: number;
  requiresForce: boolean;
  state: StateFingerprint;
}

/** An editor installed on this machine, offered by the Open menu. */
export interface Editor {
  id: string;
  name: string;
  command: string;
}

export interface RemoveOptions {
  force: boolean;
  deleteBranch: boolean;
  /**
   * Separate from `force` on purpose: one authorises losing uncommitted files,
   * the other authorises losing commits. Left false, git refuses to delete an
   * unmerged branch and the commits stay reachable.
   */
  forceBranch: boolean;
  useTrash: boolean;
  /**
   * Lift the worktree's lock and remove it anyway.
   *
   * Separate from `force` for the same reason `forceBranch` is. A lock is the
   * one thing in a plan somebody put there deliberately, usually with a reason
   * attached, so agreeing to discard some edited files is not an answer to it.
   */
  unlock: boolean;
}

/** One worktree's removal, with the options that authorise that one. */
export interface RemovalRequest {
  plan: RemovalPlan;
  options: RemoveOptions;
}

/** What a change *is*, decided by inspection rather than by reading prose. */
export type FileKind =
  /** A unified patch with at least one hunk. The only kind a patch view renders. */
  | { kind: "text" }
  /** Bytes Git will not diff line by line. */
  | { kind: "binary" }
  /** A new file with no contents at all. */
  | { kind: "empty" }
  | { kind: "symlink"; target: string }
  /**
   * A directory Git named as one path. `paths` is how many raw Git paths it
   * stands for; `items` is how many entries it holds, when that was countable.
   */
  | { kind: "directory"; paths: number; items: number | null }
  /** A repository inside the worktree: its own history, not this one's content. */
  | {
      kind: "repository";
      repository: RepositoryKind;
      paths: number;
      items: number | null;
    }
  /** Git recorded a change that moves no lines - a mode change, a rename. */
  | { kind: "metadata"; detail: string }
  /** Named, counted, and not read. `detail` says why. */
  | { kind: "unread"; detail: string };

export type RepositoryKind = "nested" | "linkedWorktree" | "bare";

export type DiffScope = "uncommitted" | "history";

export type DiffFile = {
  path: string;
  insertions: number;
  deletions: number;
  origin: ChangeOrigin;
} & FileKind;

/** One side's arithmetic. Never the sum of two sides. */
export interface DiffTotals {
  files: number;
  insertions: number;
  deletions: number;
}

/**
 * Exactly what was left out, and how much of it.
 *
 * A banner saying something is missing without saying what is a banner that
 * cannot be acted on. Each of these carries the number that makes it a fact.
 */
export type DiffLimit =
  | { kind: "displayLimit"; shown: number; total: number }
  | { kind: "inspectionLimit"; limit: number; shown: number; total: number }
  | { kind: "unreadable"; paths: string[]; total: number }
  | { kind: "tooLarge"; paths: string[]; total: number }
  | { kind: "readBudget"; paths: string[]; total: number }
  | { kind: "listingFailed" };

export interface DiffSummary {
  scope: DiffScope;
  base: string | null;
  commits: number;
  files: DiffFile[];
  /** Committed work: `merge-base..HEAD`. Empty in the uncommitted scope. */
  history: DiffTotals;
  /** Work on disk only: index plus worktree plus untracked. */
  working: DiffTotals;
  includesUncommitted: boolean;
  /** Raw untracked paths Git named. */
  untrackedTotal: number;
  /** Raw untracked paths some row accounts for. */
  untrackedIncluded: number;
  /** Rows those paths were grouped into. Differs when a nested repo is atomic. */
  untrackedEntries: number;
  incomplete: boolean;
  limits: DiffLimit[];
}

/** Where a file's change lives. */
export type ChangeOrigin = "committed" | "uncommitted" | "both";

/**
 * One file's change, ready to render.
 *
 * A patch exists only on the `text` variant, so there is no shape in which a
 * binary or a nested repository can be handed to a patch view: the type refuses
 * it, rather than the renderer discovering it at runtime.
 */
export type EntryContent =
  | { kind: "text"; patch: string; hunks: number }
  | { kind: "binary" }
  | { kind: "empty" }
  | { kind: "symlink"; target: string }
  | { kind: "directory"; paths: number; items: number | null }
  | {
      kind: "repository";
      repository: RepositoryKind;
      paths: number;
      items: number | null;
    }
  | { kind: "metadata"; detail: string }
  | { kind: "unread"; detail: string };

export type DiffEntry = {
  path: string;
  origin: ChangeOrigin;
  insertions: number;
  deletions: number;
} & EntryContent;

/**
 * A worktree's changes, split by where they live.
 *
 * "Landed on this branch" and "only on disk here" answer different questions,
 * so they are never blended into one scroll. Either may be empty.
 */
export interface Patches {
  scope: DiffScope;
  committed: DiffEntry[];
  uncommitted: DiffEntry[];
  truncated: boolean;
  incomplete: boolean;
  untrackedTotal: number;
  untrackedShown: number;
  untrackedEntries: number;
  limits: DiffLimit[];
}

const count = (value: number) => value.toLocaleString();

/** A list a reader can act on, rather than a number standing in for one. */
function names(paths: string[], total: number): string {
  const listed = paths.map((path) => `\u201C${path}\u201D`);
  const shown =
    listed.length <= 1
      ? (listed[0] ?? "")
      : `${listed.slice(0, -1).join(", ")} and ${listed[listed.length - 1]}`;
  const rest = total - paths.length;
  return rest > 0 ? `${shown} and ${count(rest)} more` : shown;
}

/**
 * What is missing, in the units the reader was counting in.
 *
 * Every sentence here states a cause and a quantity. "Some files could not be
 * inspected" is not one of them: it tells a reader that something is wrong and
 * gives them nothing to do about it.
 */
export function limitMessage(limit: DiffLimit): string {
  switch (limit.kind) {
    case "displayLimit":
      return `Showing ${count(limit.shown)} of ${count(limit.total)} untracked paths; ${count(limit.total - limit.shown)} were not read because the display limit was reached.`;
    case "inspectionLimit":
      return `Showing ${count(limit.shown)} of ${count(limit.total)} untracked paths; this worktree holds more than the ${count(limit.limit)} this view inspects at once.`;
    case "unreadable":
      return `${count(limit.total)} ${limit.total === 1 ? "path" : "paths"} could not be read from disk: ${names(limit.paths, limit.total)}.`;
    case "tooLarge":
      return `${count(limit.total)} ${limit.total === 1 ? "file is" : "files are"} too large to show here: ${names(limit.paths, limit.total)}.`;
    case "readBudget":
      return `${count(limit.total)} ${limit.total === 1 ? "file was" : "files were"} left unread once this view's reading budget ran out: ${names(limit.paths, limit.total)}.`;
    case "listingFailed":
      return "Git could not list this worktree's untracked files, so none are shown.";
  }
}

/** Something a reader can do about each limit, in their own terminal. */
export function limitRemedy(limit: DiffLimit, worktree: string): string {
  const path = /[\s"']/.test(worktree) ? `'${worktree}'` : worktree;
  switch (limit.kind) {
    case "displayLimit":
    case "inspectionLimit":
      return `git -C ${path} status --porcelain`;
    case "unreadable":
    case "tooLarge":
    case "readBudget":
      return `git -C ${path} status --short`;
    case "listingFailed":
      return `git -C ${path} status`;
  }
}

/** Every sentence the panel owes the reader about what it left out. */
export function diffLimitMessages(patches: Patches): string[] {
  const messages = patches.limits.map(limitMessage);
  if (
    patches.truncated &&
    !patches.limits.some((limit) => limit.kind === "displayLimit")
  ) {
    messages.push(
      `Showing ${count(patches.untrackedShown)} of ${count(patches.untrackedTotal)} changes; the rest were not rendered because the display limit was reached.`,
    );
  }
  return messages;
}

export interface UniqueLineMarker {
  path: string;
  side: "additions" | "deletions";
  lineNumber: number;
}

export interface UniquePatch {
  patch: string;
  lineCount: number;
  fileCount: number;
  candidate: string;
  target: string;
  markers: UniqueLineMarker[];
  incomplete: boolean;
  truncated: boolean;
}

export interface MergePatch {
  patch: string;
  lineCount: number;
  fileCount: number;
  target: string;
  truncated: boolean;
}

export type FocusedPatch =
  | { kind: "unmatched"; patch: UniquePatch }
  | { kind: "wouldChange"; patch: MergePatch }
  | {
      kind: "all";
      reason: "noFilteredChanges" | "incomplete" | "unsafe";
    };

export interface DiffResult {
  summary: DiffSummary;
  patches: Patches;
  uncommitted: UncommittedAnalysis;
}

export type ProvisionKind = "copyFile" | "linkDir";

export interface ProvisionItem {
  name: string;
  kind: ProvisionKind;
  /** Whether the box starts ticked. */
  recommended: boolean;
  /** Why it is not recommended, when it is not. */
  caution: string | null;
  bytes: number | null;
}

export interface CreatePlan {
  branch: string;
  base: string;
  path: string;
  /** git forbids checking one branch out twice; this says where it already is. */
  branchInUseAt: string | null;
  branchExists: boolean;
  pathExists: boolean;
  /** Nested worktrees make agents grep into the wrong tree. */
  pathIsNested: boolean;
  items: ProvisionItem[];
}

export interface CreateOptions {
  branch: string;
  base: string;
  path: string;
  provision: string[];
}

export interface ProvisioningDefaults {
  copyEnvFiles: boolean;
  linkDependencies: boolean;
  honourWorktreeinclude: boolean;
}

export type DiffStyle = "unified" | "split";

export interface Workspace {
  id: string;
  name: string;
  repos: string[];
  scanRoots: string[];
}

export interface Config {
  /** Named groups. There is always at least one. */
  workspaces: Workspace[];
  /** Which group is in view. `null` shows every group at once. */
  activeWorkspace: string | null;
  scanDepth: number;
  editor: string | null;
  worktreePathTemplate: string;
  activeWithinMinutes: number;
  diffStyle: DiffStyle;
  hideMainWorktrees: boolean;
  provisioning: ProvisioningDefaults;
}

/**
 * Settings plus the revision they were read at.
 *
 * A screen that loads once and stays mounted holds a snapshot that goes stale
 * the moment anything else changes settings; sending the revision back lets a
 * write against an old snapshot be refused instead of silently winning.
 */
export interface VersionedConfig {
  config: Config;
  revision: number;
}

export type SaveOutcome =
  | { outcome: "saved"; revision: number }
  | { outcome: "stale"; revision: number; config: Config };

/**
 * What became of the settings file at startup.
 *
 * `unusable` means the file was there and could not be understood, which is the
 * one case where the app is running on guessed defaults.
 */
export type ConfigStatus =
  | { state: "missing" }
  | { state: "loaded" }
  | { state: "unusable"; reason: string; backup: string | null };

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/**
 * Every command goes through here, so no screen can forget the deadline.
 *
 * The wrapper is on the transport rather than on the calls that were noticed
 * hanging, because the hazard is a property of the channel: any reply can be
 * dropped, so any await can be permanent. Routing all of them through one
 * function means a surface with no failure path of its own — a dialog that
 * only renders "Checking…" — gets a reachable rejection for free, and its
 * existing `catch` starts doing the job it was always written to do.
 */
function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return withDeadline(invoke<T>(command, args), command, deadlineFor(command, args));
}

export const api = {
  getConfig: () => call<VersionedConfig>("get_config"),
  /**
   * `revision` is the one the settings were read at. Omit it only when the
   * caller genuinely means "whatever is there now, overwrite it".
   */
  setConfig: (config: Config, revision?: number) =>
    call<SaveOutcome>("set_config", { config, revision: revision ?? null }),
  configStatus: () => call<ConfigStatus>("config_status"),

  addRepo: (path: string, workspace?: string | null) =>
    call<boolean>("add_repo", { path, workspace: workspace ?? null }),
  addScanRoot: (path: string, workspace?: string | null) =>
    call<boolean>("add_scan_root", { path, workspace: workspace ?? null }),

  createWorkspace: (name: string) => call<string>("create_workspace", { name }),
  /** Removes the group from settings only — nothing on disk is touched. */
  deleteWorkspace: (id: string) => call<boolean>("delete_workspace", { id }),
  setActiveWorkspace: (id: string | null) =>
    call<void>("set_active_workspace", { id }),

  /**
   * `full: false` skips disk measurement and process detection so the list can
   * paint immediately; the UI follows up with a full scan.
   */
  scanAll: (full: boolean) => call<ScanReport>("scan_all", { full }),
  inspectWorktree: (repo: string, worktree: string) =>
    call<Worktree>("inspect_worktree", { repo, worktree }),
  resolveLanding: (repo: string, worktree: string) =>
    call<Worktree | null>("resolve_landing", { repo, worktree }),

  /**
   * Plans for the whole selection at once. One call, because the repository
   * facts behind a plan are the same for every worktree in it.
   */
  planRemovals: (repo: string, worktrees: string[]) =>
    call<RemovalPlan[]>("plan_removals", { repo, worktrees }),
  /**
   * Removes the whole selection, or none of it.
   *
   * One call rather than one per worktree, because the guarantee only holds if
   * it is made in one place: core re-checks every plan against the repository
   * before it touches any of them. Looping here deleted the worktrees whose
   * plans were still good and only then found that a later one had changed, so
   * the dialog reported a refusal over a deletion that had already happened.
   */
  removeWorktrees: (repo: string, requests: RemovalRequest[]) =>
    call<RemovalOutcome[]>("remove_worktrees", { repo, requests }),
  pruneRepo: (repo: string) => call<void>("prune_repo", { repo }),

  /** What a worktree changed relative to the repository's default branch. */
  diffWorktree: (repo: string, worktree: string, scope: DiffScope) =>
    call<DiffResult>("diff_worktree", { repo, worktree, scope }),
  focusedWorktree: (repo: string, worktree: string) =>
    call<FocusedPatch>("focused_worktree", { repo, worktree }),
  prefetchFocusedWorktree: (repo: string, worktree: string) =>
    call<void>("prefetch_focused_worktree", { repo, worktree }),
  suggestWorktreePath: (repo: string, branch: string) =>
    call<string>("suggest_worktree_path", { repo, branch }),
  planCreation: (repo: string, branch: string, base: string, path: string) =>
    call<CreatePlan>("plan_creation", { repo, branch, base, path }),
  createWorktree: (repo: string, options: CreateOptions) =>
    call<string[]>("create_worktree", { repo, options }),
  listBaseRefs: (repo: string) => call<string[]>("list_base_refs", { repo }),

  revealPath: (path: string) => call<void>("reveal_path", { path }),
  openInEditor: (path: string) => call<void>("open_in_editor", { path }),
  listEditors: () => call<Editor[]>("list_editors"),
  setEditor: (command: string | null) => call<void>("set_editor", { command }),
};

// ---------------------------------------------------------------------------
// Presentation helpers
// ---------------------------------------------------------------------------

/**
 * The verdict as a judgement rather than a category.
 *
 * VERDICT_LABEL names the bucket, which is right for a filter chip and wrong
 * for the moment someone is deciding whether to destroy work. Here the panel
 * says what it thinks should happen.
 */
export const VERDICT_HEADLINE: Record<Verdict, string> = {
  disposable: "Safe to delete",
  review: "Worth a look first",
  keep: "Don't delete this",
  broken: "Nothing left to delete",
};

export const VERDICT_LABEL: Record<Verdict, string> = {
  disposable: "Disposable",
  review: "Review",
  keep: "Keep",
  broken: "Broken",
};

const REASON_LABEL: Record<VerdictReason["kind"], string> = {
  directoryMissing: "Directory is missing",
  locked: "Locked",
  mainWorktree: "Main worktree",
  processRunning: "Something is running",
  processCheckSkipped: "Live process check was skipped",
  recentlyActive: "Changed recently",
  uncommittedChanges: "Uncommitted changes",
  uncommittedChangesAtRisk: "",
  uncommittedChangesOnDefault: "",
  environmentFilesAtRisk: "",
  workingTreeUnreadable: "Working tree could not be read",
  unpushedCommits: "Unpushed commits",
  workContained: "Work is contained",
  defaultBranchLacksCommittedContent: "Default branch lacks this work",
  landingUnknown: "Landing could not be verified",
};

export function reasonLabel(reason: VerdictReason): string {
  if (reason.kind === "workContained") {
    return `Contained in ${reason.target}`;
  }
  if (reason.kind === "environmentFilesAtRisk") {
    return `${reason.count} environment ${
      reason.count === 1 ? "file is" : "files are"
    } in no repository`;
  }
  if (reason.kind === "uncommittedChangesAtRisk") {
    return `${reason.incomplete ? "At least " : ""}${reason.count} uncommitted ${
      reason.count === 1 ? "line is" : "lines are"
    } absent from ${reason.target}`;
  }
  if (reason.kind === "uncommittedChangesOnDefault") {
    return `Uncommitted content is already on ${reason.target}`;
  }
  return REASON_LABEL[reason.kind];
}

/**
 * How far a comparison actually got, said in numbers.
 *
 * The previous copy said the comparison "stopped at its size limit, so some of
 * these lines were not read", which names neither the limit nor the shortfall
 * and so gives the reader nothing to weigh: "some" covers one line and ten
 * thousand equally. Core already counts both while it compares, so this quotes
 * the threshold that stopped the walk and the amount left over.
 *
 * Counts are stated as totals only when core proved them. When a listing itself
 * failed, every number below it is a floor, and this says "at least" rather
 * than presenting a floor as a total.
 */
export function comparisonShortfallSentence(
  target: string,
  shortfall: ComparisonShortfall,
): string {
  const atLeast = shortfall.countsExact ? "" : "at least ";
  const clauses: string[] = [];

  if (shortfall.linesNotCompared > 0) {
    const scope = shortfall.linesCompared + shortfall.linesNotCompared;
    const limit =
      shortfall.lineLimit === null
        ? ""
        : `, stopping at its ${grouped(shortfall.lineLimit)}-line limit,`;
    clauses.push(
      `read ${grouped(shortfall.linesCompared)} of ${atLeast}${grouped(scope)} changed lines${limit} so ${atLeast}${grouped(
        shortfall.linesNotCompared,
      )} ${shortfall.linesNotCompared === 1 ? "line was" : "lines were"} not read`,
    );
  }

  if (shortfall.pathsNotCompared > 0) {
    clauses.push(
      `could not read ${atLeast}${grouped(shortfall.pathsNotCompared)} ${
        shortfall.pathsNotCompared === 1 ? "path" : "paths"
      } line by line`,
    );
  }

  if (clauses.length === 0) {
    clauses.push(
      `read ${grouped(shortfall.linesCompared)} changed ${
        shortfall.linesCompared === 1 ? "line" : "lines"
      }, then lost the untracked file listing, so the size of the gap is unmeasured`,
    );
  }

  return `The comparison with ${target} ${clauses.join(", and ")}.`;
}

/**
 * The fuller version, for the detail panel where there is room to explain.
 *
 * Each says what was observed *and* what it implies, because the verdict is
 * only trustworthy if the user can see the reasoning behind it.
 */
const REASON_DETAIL: Record<VerdictReason["kind"], string> = {
  directoryMissing:
    "The directory is gone. Only stale git metadata is left, which pruning clears.",
  locked: "Locked, so yawm leaves it alone.",
  mainWorktree: "The main worktree. Everything else depends on it.",
  processRunning:
    "A process is running in here right now — very likely an agent mid-task.",
  processCheckSkipped:
    "Yawm did not inspect live processes in this scan, so it cannot yet call this worktree safe to delete.",
  recentlyActive: "Files changed here in the last few minutes.",
  uncommittedChanges:
    "These files have uncommitted changes. Their content has not been verified against the default branch.",
  uncommittedChangesAtRisk: "",
  uncommittedChangesOnDefault: "",
  environmentFilesAtRisk: "",
  workingTreeUnreadable:
    "Yawm could not inspect this working tree for uncommitted files. Treat it as unsafe until the inspection succeeds.",
  unpushedCommits: "Commits that have not reached the remote.",
  workContained: "",
  defaultBranchLacksCommittedContent:
    "The default branch lacks committed content from this worktree.",
  landingUnknown:
    "Could not verify whether rewritten work landed. Review it before deleting.",
};

export function reasonDetail(reason: VerdictReason): string {
  if (reason.kind === "workContained") {
    return `Work is contained in ${reason.target}, so nothing would be lost.`;
  }
  if (reason.kind === "environmentFilesAtRisk") {
    return `${reason.count} untracked environment ${
      reason.count === 1 ? "file has" : "files have"
    } no matching copy in the main worktree. Deleting this worktree would destroy ${
      reason.count === 1 ? "its" : "their"
    } current contents.`;
  }
  if (reason.kind === "uncommittedChangesAtRisk") {
    return `${reason.incomplete ? "At least " : ""}${reason.count} changed ${
      reason.count === 1 ? "line is" : "lines are"
    } absent from ${reason.target}. Deleting this worktree loses ${
      reason.count === 1 ? "it" : "them"
    }.${
      reason.shortfall
        ? ` ${comparisonShortfallSentence(reason.target, reason.shortfall)}`
        : ""
    }`;
  }
  if (reason.kind === "uncommittedChangesOnDefault") {
    return `These edits are not committed anywhere, but their line-level content is already reflected on ${reason.target}. Review before deleting.`;
  }
  return REASON_DETAIL[reason.kind];
}

/** Deferred proof is unfinished work, not evidence that landing failed. */
export function isLandingCheckDeferred(landing: Landing): boolean {
  return (
    landing.state === "unknown" && landing.reason.kind === "checkDeferred"
  );
}

/** A worktree's display name: its branch, or a detached-HEAD marker. */
export function worktreeLabel(worktree: Worktree): string {
  if (worktree.branch) return worktree.branch;
  if (worktree.head) return `detached at ${worktree.head.slice(0, 7)}`;
  return "(bare)";
}

/**
 * How many places a workspace looks for repositories.
 *
 * Lives here so the two surfaces that show it cannot drift apart. Says
 * "Empty" rather than "0 sources", because a zero next to a plural noun is
 * the tell of a string that was concatenated rather than written.
 */
export function sourceCount(workspace: Workspace): string {
  const n = workspace.repos.length + workspace.scanRoots.length;
  if (n === 0) return "Empty";
  return n === 1 ? "1 source" : `${n} sources`;
}

/**
 * One run of the diff header's stat line.
 *
 * Segments rather than a finished string because the header tints its
 * additions, deletions and counts, and a string that has to be re-parsed to be
 * coloured is a string that can drift from the one the tests read.
 */
export type StatTone = "added" | "removed" | "count" | "note";

export interface StatSegment {
  text: string;
  tone?: StatTone;
  /**
   * A segment the reader can act on, rather than only read.
   *
   * The count of paths without a text diff is the one number on this line that
   * stands for names the view is not drawing anywhere else, so it is the one
   * place a reader can be given them. Marked here rather than matched by its
   * text in the renderer, which would break the moment the wording moved.
   */
  role?: "notDiffable";
}

/** The stat line as the reader sees it, for assertions. */
export function statLineText(segments: StatSegment[]): string {
  return segments.map((segment) => segment.text).join("");
}

/**
 * Five figures are unreadable ungrouped, and this line's whole job is that the
 * large number be taken in at a glance rather than deciphered.
 */
const grouped = (n: number) => n.toLocaleString("en-US");

/**
 * How much of this branch has not landed, in the analysis's own units.
 *
 * `lineCount` is the number of lines the containment check could not find on
 * the target — the same number the detail panel warns with and the toggle
 * beside this line states. The header used to recompute it by counting `+`/`−`
 * lines in the rendered patch, but that patch deliberately carries the
 * surrounding hunks so the unmerged lines can be read in context. So it
 * measured the excerpt and labelled it the finding: one screen said 17 in two
 * places and 660 in a third, and the alarming number was the one wearing the
 * word "unmerged". There is exactly one number for this quantity and it comes
 * from the analysis.
 *
 * Which target it is unmerged to is not said here. The group heading under
 * this line names it, and saying it twice in the same glance was most of what
 * made the old header repeat itself.
 */
export function atRiskClause(patch: UniquePatch): StatSegment[] {
  if (patch.lineCount === 0) {
    return [{ text: "Nothing at risk" }];
  }
  return [
    ...(patch.incomplete ? [{ text: "At least " }] : []),
    { text: grouped(patch.lineCount), tone: "count" as const },
    { text: ` ${patch.lineCount === 1 ? "line" : "lines"} at risk` },
  ];
}

/**
 * What the Changes view is holding, counted so the parts add up.
 *
 * One identity, and the reader can check it: every path this view knows about
 * is either a text diff it drew or a path it could not draw, and the two
 * numbers sum to the first. This is what replaced a line that called 257 text
 * diffs "files" while the sidebar, counting distinct dirty paths, said 404 —
 * two true numbers with no stated relationship, which reads as one of them
 * being wrong.
 */
export interface ChangesBalance {
  /** Sections actually rendered. */
  textDiffs: number;
  /**
   * Distinct paths this view is holding, counted once each.
   *
   * A union across the groups rather than a sum of them: a file committed on
   * this branch and edited again since is genuinely two things to read, and
   * both are drawn, but it is one changed path. Summing the groups told the
   * reader a worktree with one such file held two.
   */
  changedPaths: number;
  /** Raw Git paths behind the entries that carry no lines, counted once each. */
  notDiffable: number;
  insertions: number;
  deletions: number;
  /**
   * Paths this view was told about and never received, because a limit cut
   * the listing short. Stated rather than absorbed: without it the identity
   * silently stops balancing against anything outside this view.
   */
  residual: number;
}

/** The identity itself: every path this view knows about, counted once. */
export function changedPathTotal(balance: ChangesBalance): number {
  return balance.changedPaths;
}

/**
 * The one summary line above the Changes view.
 *
 * It describes exactly what is on screen, in one reading, and never mixes two
 * analyses: the at-risk reading reports the analysis's own line and file
 * counts against the number of changed paths it is reading, and the complete
 * reading reports the balance above. Neither borrows the other's numbers.
 */
export function changesSummarySegments({
  balance,
  atRisk,
  leadingClause,
}: {
  balance: ChangesBalance;
  /** The focused analysis, present only while the at-risk reading is shown. */
  atRisk: UniquePatch | null;
  /**
   * Said once, before everything else, when it is the finding: a branch with
   * no commits of its own has no committed group to draw, and the absence is
   * the answer rather than a missing section.
   */
  leadingClause: string | null;
}): StatSegment[] {
  const changedPaths = changedPathTotal(balance);
  const lead: StatSegment[] = leadingClause
    ? [{ text: leadingClause }, { text: SEPARATOR }]
    : [];
  const residual: StatSegment[] =
    balance.residual > 0
      ? [
          { text: SEPARATOR },
          { text: `${grouped(balance.residual)} not read`, tone: "note" },
        ]
      : [];

  if (atRisk) {
    return [
      ...lead,
      ...atRiskClause(atRisk),
      ...(atRisk.lineCount > 0 && atRisk.fileCount > 0
        ? [
            {
              text: ` in ${grouped(atRisk.fileCount)} ${
                atRisk.fileCount === 1 ? "file" : "files"
              }`,
            },
          ]
        : []),
      { text: SEPARATOR },
      { text: `of ${grouped(changedPaths)} changed ${
        changedPaths === 1 ? "path" : "paths"
      }` },
      ...residual,
    ];
  }

  const diffCount: StatSegment[] =
    balance.textDiffs > 0
      ? [
          { text: grouped(balance.textDiffs), tone: "count" },
          { text: ` text ${balance.textDiffs === 1 ? "diff" : "diffs"}` },
        ]
      : [{ text: "No text diffs" }];

  /*
   * The path total is printed only when it differs from the diff count.
   * `257 changed paths · 257 text diffs` is an identity with one term, which
   * reads as the same number said twice rather than as arithmetic. It is the
   * comparison rather than `notDiffable > 0`, because a path drawn in both
   * groups makes the two numbers differ with nothing omitted at all — and the
   * line would otherwise print `2 text diffs` for one changed path and never
   * say which of the two the reader should believe.
   */
  const paths: StatSegment[] =
    changedPaths !== balance.textDiffs
      ? [
          { text: grouped(changedPaths), tone: "count" },
          { text: ` changed ${changedPaths === 1 ? "path" : "paths"}` },
          { text: SEPARATOR },
        ]
      : [];

  const notDiffable: StatSegment[] =
    balance.notDiffable > 0
      ? [
          { text: SEPARATOR },
          {
            text: `${grouped(balance.notDiffable)} not diffable`,
            role: "notDiffable",
          },
        ]
      : [];

  /*
   * `+0 −0` is printed only when something was actually compared. A view
   * holding nothing but binaries and nested repositories ran no line
   * comparison at all, and a zero there claims one that never happened.
   */
  const totals: StatSegment[] =
    balance.textDiffs > 0
      ? [
          { text: SEPARATOR },
          { text: `+${grouped(balance.insertions)}`, tone: "added" },
          { text: " " },
          { text: `−${grouped(balance.deletions)}`, tone: "removed" },
        ]
      : [];

  return [
    ...lead,
    ...paths,
    ...diffCount,
    ...notDiffable,
    ...totals,
    ...residual,
  ];
}

/**
 * The absence of committed work, said once and in the summary.
 *
 * A branch that has committed nothing of its own used to draw an empty
 * "Branch History" group under a heading promising commits. There is no group
 * to draw; there is a fact, and this is it.
 */
export function noBranchCommitsClause(base: string | null): string {
  return base
    ? `No branch-only commits relative to ${base}`
    : "No branch-only commits";
}

const SEPARATOR = " · ";

/**
 * The two readings of one set of changes — names, and nothing else.
 *
 * Not scopes. Both readings describe the same fetched payload: `Everything`
 * draws it whole, `At risk` narrows the committed half to the lines that have
 * no match on the target. A count inside either would be read as part of its
 * name, and the summary beside them already states one.
 */
export const AT_RISK_READING_LABEL = "At risk";
export const EVERYTHING_READING_LABEL = "Everything";

/**
 * How many unmerged lines fall in each file.
 *
 * The markers are the analysis's per-line verdicts, so summing them per path
 * gives counts in the same units as `lineCount`. A list headed "unmerged
 * lines" has to be showing that quantity; the whole-file diff stat is a
 * different one, and under that heading it reads as this one.
 *
 * A file the analysis included but never marked is absent here rather than
 * zero: it is in the set because it could not be compared line by line, and
 * `+0 −0` would claim it was compared and found clean.
 */
export function unmergedLinesByFile(
  markers: UniqueLineMarker[],
): Map<string, { insertions: number; deletions: number }> {
  const counts = new Map<string, { insertions: number; deletions: number }>();
  for (const marker of markers) {
    let entry = counts.get(marker.path);
    if (!entry) {
      entry = { insertions: 0, deletions: 0 };
      counts.set(marker.path, entry);
    }
    if (marker.side === "additions") entry.insertions += 1;
    else entry.deletions += 1;
  }
  return counts;
}

/**
 * Space that deleting this worktree would actually give back.
 *
 * The verdict check belongs here rather than at each call site. It used to be
 * left to callers, which meant the function returned bytes that were not
 * reclaimable and every caller had to remember to guard it — and the one that
 * forgot told the reader a worktree it had just labelled Keep was 1.2 GB of
 * free space.
 */
export function reclaimableBytes(worktree: Worktree): number {
  if (worktree.isMain) return 0;
  if (worktree.verdict !== "disposable") return 0;
  return worktree.status.size?.bytes ?? 0;
}
