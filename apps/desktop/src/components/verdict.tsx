import { cn } from "@/lib/utils";
import { VERDICT_LABEL, type Verdict } from "@/lib/api";

/**
 * Verdict styling.
 *
 * This is the one piece of the design system shadcn cannot provide, because it
 * encodes product meaning rather than UI mechanics: each verdict has exactly
 * one colour, used by the dot, the filter chips, and the detail panel, so the
 * same judgement always looks the same wherever it appears.
 */

const DOT: Record<Verdict, string> = {
  disposable: "bg-disposable",
  review: "bg-review",
  keep: "bg-keep",
  broken: "bg-broken",
};

/** The coloured dot that carries the verdict at a glance. */
export function VerdictDot({
  verdict,
  className,
}: {
  verdict: Verdict;
  className?: string;
}) {
  return (
    <span
      className={cn("size-2 shrink-0 rounded-full", DOT[verdict], className)}
      aria-hidden
    />
  );
}

/**
 * Verdict styling for a text badge.
 *
 * Tinted rather than solid: four saturated pills repeating down a long list
 * would shout louder than the branch names they describe. Enough colour to
 * scan by, not enough to fight the content.
 */
const BADGE: Record<Verdict, string> = {
  disposable: "bg-disposable/12 text-disposable",
  review: "bg-review/12 text-review",
  keep: "bg-keep/12 text-keep",
  broken: "bg-broken/12 text-broken",
};

/**
 * The verdict, in words.
 *
 * A colour alone asks the reader to hold a legend in their head while deciding
 * whether to delete files — which is the guessing this app exists to remove.
 * Fixed width so the reasons beside it line up into a readable column.
 */
export function VerdictBadge({ verdict }: { verdict: Verdict }) {
  return (
    <span
      className={cn(
        "inline-flex h-[18px] w-[74px] shrink-0 items-center justify-center rounded text-[10px] font-medium tracking-wide uppercase",
        BADGE[verdict],
      )}
    >
      {VERDICT_LABEL[verdict]}
    </span>
  );
}

/**
 * The tinted block the detail panel opens with.
 *
 * Strong enough to colour the reading of everything below it — the panel is
 * arguing a case, and the verdict is the argument, not a field in a table.
 */
const ZONE: Record<Verdict, string> = {
  disposable: "bg-disposable/10 text-disposable",
  review: "bg-review/10 text-review",
  keep: "bg-keep/10 text-keep",
  broken: "bg-broken/10 text-broken",
};

export function verdictZoneClass(verdict: Verdict) {
  return ZONE[verdict];
}
