import {
  comparisonShortfallSentence,
  dirtyPathCount,
  type Worktree,
} from "@/lib/api";
import { cn } from "@/lib/utils";
import {
  AlertTriangle,
  FileWarning,
  Pencil,
  Play,
  Upload,
} from "lucide-react";

/**
 * What deleting this worktree would cost.
 *
 * One definition, used by both the row and the detail panel. They previously
 * derived their own, which is why a row could sit there showing nothing while
 * the panel listed three things about it — the summary and the detail
 * disagreeing about the same worktree is worse than either being sparse.
 *
 * The row renders these as icon badges and the panel as sentences, but the set
 * is identical and always in this order: the things that exist nowhere else
 * first, then the things that are merely in progress.
 */

export type RiskKind =
  | "envFiles"
  | "inspection"
  | "uncommitted"
  | "unpushed"
  | "process"
  | "landing";

export interface Risk {
  kind: RiskKind;
  /** Shown beside the icon in the row. Omitted when a count means nothing. */
  count?: number;
  /** The panel's sentence. */
  label: string;
  detail?: string;
  /**
   * Raw source lines, kept apart from `detail` rather than concatenated into it.
   *
   * A code fragment inlined into a sentence and then clipped by a pane edge
   * reads as a finished value — `… · app: 920,` looks like a number followed by
   * a comma, not like the middle of a line of somebody's diff. Separating them
   * lets the panel give these a container that says "raw data" before the
   * reader has parsed a character of it.
   */
  fragments?: string[];
  tone: "broken" | "review" | "keep" | "muted";
}

export function risksOf(worktree: Worktree): Risk[] {
  const { status } = worktree;
  const out: Risk[] = [];
  const inspectionFailed = status.dirty.inspectionFailed;

  if (inspectionFailed) {
    out.push({
      kind: "inspection",
      label: "Could not inspect uncommitted changes",
      detail: "Treat this worktree as unsafe until Git status succeeds.",
      tone: "review",
    });
  }

  if (status.envFiles.length > 0) {
    out.push({
      kind: "envFiles",
      count: status.envFiles.length,
      label: `${status.envFiles.length} untracked environment ${
        status.envFiles.length === 1 ? "file" : "files"
      } with no matching main-worktree copy`,
      detail: status.envFiles.join(", "),
      tone: "review",
    });
  }

  /*
   * How many files, and how those files are dirty, are two different numbers.
   *
   * `staged + unstaged + untracked` counts *status dimensions*: one path that
   * is both staged and modified since is counted twice, so a worktree with 257
   * changed files reported "404 uncommitted files" — and then the Changes view
   * beside it drew 257. `paths` is the distinct count and is the only thing
   * ever shown as a file count. The three dimensions stay, in the breakdown,
   * where they answer the question they actually answer.
   */
  const dimensions =
    status.dirty.staged + status.dirty.unstaged + status.dirty.untracked;
  const dirty = dirtyPathCount(status.dirty);
  if (dimensions > 0) {
    const parts = [
      status.dirty.staged > 0 ? `${status.dirty.staged} staged` : null,
      status.dirty.unstaged > 0 ? `${status.dirty.unstaged} modified` : null,
      status.dirty.untracked > 0 ? `${status.dirty.untracked} untracked` : null,
    ].filter(Boolean);
    const analysis = status.uncommitted;
    if (analysis.state === "compared" && analysis.leftover > 0) {
      out.push({
        kind: "uncommitted",
        count: analysis.leftover,
        label: `${analysis.incomplete ? "At least " : ""}${analysis.leftover} uncommitted ${
          analysis.leftover === 1 ? "line" : "lines"
        } absent from ${analysis.target}`,
        detail: `${dirty} changed ${dirty === 1 ? "path" : "paths"} · ${parts.join(" · ")}${
          analysis.shortfall
            ? ` · ${comparisonShortfallSentence(analysis.target, analysis.shortfall)}`
            : ""
        }`,
        fragments: analysis.leftoverSample.slice(0, 2),
        tone: "keep",
      });
    } else if (
      analysis.state === "compared" &&
      !analysis.incomplete
    ) {
      out.push({
        kind: "uncommitted",
        count: dirty,
        label: `${dirty} uncommitted ${
          dirty === 1 ? "file" : "files"
        }; content already on ${analysis.target}`,
        detail: `${parts.join(" · ")} · Not committed anywhere`,
        tone: "review",
      });
    } else {
      out.push({
        kind: "uncommitted",
        count: dirty,
        label: `${dirty} uncommitted ${dirty === 1 ? "file" : "files"}`,
        detail: `${parts.join(" · ")} · ${
          analysis.state === "compared"
            ? analysis.shortfall
              ? comparisonShortfallSentence(analysis.target, analysis.shortfall)
              : `Not compared line by line with ${analysis.target}`
            : "Not yet compared with the default branch"
        }`,
        tone: "review",
      });
    }
  }

  if (status.upstream.ahead > 0) {
    out.push({
      kind: "unpushed",
      count: status.upstream.ahead,
      label: `${status.upstream.ahead} ${
        status.upstream.ahead === 1 ? "commit" : "commits"
      } not pushed`,
      tone: "review",
    });
  }

  if (status.processes.length > 0) {
    out.push({
      kind: "process",
      count: status.processes.length,
      label:
        status.processes.length === 1
          ? `${status.processes[0]!.name} is running in here`
          : `${status.processes.length} processes running in here`,
      detail: status.processes.map((p) => `${p.name} · ${p.pid}`).join(", "),
      tone: "keep",
    });
  }

  if (status.landing.state === "addsContent") {
    out.push({
      kind: "landing",
      label: "The default branch lacks this committed work",
      detail: `Compared with ${status.landing.target}.`,
      tone: "keep",
    });
  } else if (status.landing.state === "unknown") {
    if (!status.landingComplete) return out;

    const rangeTooLarge =
      status.landing.reason.kind === "historyRangeTooLarge"
        ? ` The target range exceeds ${status.landing.reason.limit} commits, so yawm did not search further back.`
        : "";
    const candidate = status.landing.candidate;

    /*
      Neither the conflict count nor a match percentage appears here, because
      both mislead in the same direction.

      Git silently auto-merges files that match exactly and reports only the
      ones that differ, so a conflict count tallies the parts that did *not*
      land — on one real branch, 12 of 19 added files were byte-identical to
      the default branch and only the 7 that had drifted were "conflicts".
      Percentages fail the other way: a branch scored 47 of 98 paths purely
      because the default branch kept improving those files afterwards, which
      is evidence the work landed, not that it went missing.

      What survives is the only thing the reader actually asked: which lines of
      this branch exist nowhere in the default branch.
    */
    if (candidate && candidate.leftover > 0) {
      out.push({
        kind: "landing",
        count: candidate.leftover,
        /*
          "Unmerged" is vocabulary the reader already owns, and it scales from
          a branch that is 99% landed to one that is not landed at all. The
          label reports; the detail proves it by naming the commit compared
          against. The caveats — path-scoped matching, text not semantics —
          belong in a tooltip, because putting them here turned a one-line
          finding into a legal disclaimer, which is what made it read as
          weird.
        */
        label: `${candidate.incomplete ? "At least " : ""}${candidate.leftover} unmerged ${
          candidate.leftover === 1 ? "line" : "lines"
        }`,
        detail: `Closest match on ${candidate.target}: ${candidate.commit.slice(0, 7)}`,
        fragments: candidate.leftoverSample.slice(0, 2),
        tone: "review",
      });
    } else {
      out.push({
        kind: "landing",
        label: "Could not verify whether this work landed",
        detail: candidate
          ? `Closest match on ${candidate.target}: ${candidate.commit.slice(0, 7)}, but a candidate cannot prove that every change landed.`
          : `Review the committed changes before deleting the branch.${rangeTooLarge}`,
        tone: "review",
      });
    }
  }

  return out;
}

const ICON: Record<RiskKind, typeof Pencil> = {
  envFiles: FileWarning,
  inspection: AlertTriangle,
  uncommitted: Pencil,
  unpushed: Upload,
  process: Play,
  // Not a merge glyph. A fork reads as git topology, when the thing being
  // said is "some of this branch's committed work may exist nowhere else" —
  // the most consequential fact on the row and the least obvious one.
  landing: AlertTriangle,
};

const TONE: Record<Risk["tone"], string> = {
  broken: "text-broken",
  review: "text-review",
  keep: "text-keep",
  muted: "text-muted-foreground",
};

export function RiskIcon({
  risk,
  className,
}: {
  risk: Risk;
  className?: string;
}) {
  const Icon = ICON[risk.kind];
  return <Icon className={cn("shrink-0", TONE[risk.tone], className)} />;
}

/**
 * The count as the row shows it.
 *
 * Capped, because the row's job is to say "a lot" in a width that cannot grow.
 * Four digits would push a token past the width the column reserved for it and
 * start eating the branch name again, which is the whole failure being undone.
 */
export function riskCountLabel(risk: Risk): string | null {
  if (risk.count === undefined) return null;
  return risk.count > 99 ? "99+" : String(risk.count);
}

/** The row's hover text and the panel's sentence, from the same words. */
export function riskSentence(risk: Risk): string {
  const parts = [risk.label, risk.detail, ...(risk.fragments ?? [])];
  return parts.filter(Boolean).join(" — ");
}

export function riskToneClass(risk: Risk) {
  return TONE[risk.tone];
}
