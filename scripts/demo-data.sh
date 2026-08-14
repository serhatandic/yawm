#!/usr/bin/env bash
#
# Build a demo workspace for screenshots and manual testing.
#
# Creates repositories whose worktrees cover every verdict yawm can reach, at
# realistic sizes, so the app can be shown without pointing it at anything
# private.
#
# Everything lives under one directory and nothing is installed, so cleanup is
# a single command.
#
# Usage:
#     scripts/demo-data.sh [--root DIR] [--clean] [--no-agents]
#
#   --root DIR     where to build it (default /tmp/yawm-demo)
#   --clean        remove it, stop the fake agents, and exit
#   --no-agents    skip the background processes that produce the
#                  "something is running" badge

set -euo pipefail

ROOT="/tmp/yawm-demo"
CLEAN=0
AGENTS=1

while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2%/}"; shift 2 ;;
    --clean) CLEAN=1; shift ;;
    --no-agents) AGENTS=0; shift ;;
    -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

AGENT_PIDS="$ROOT/.agent-pids"

stop_agents() {
  [ -f "$AGENT_PIDS" ] || return 0
  while read -r pid; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done < "$AGENT_PIDS"
  rm -f "$AGENT_PIDS"
}

if [ "$CLEAN" = "1" ]; then
  stop_agents
  rm -rf "$ROOT"
  echo "removed $ROOT"
  exit 0
fi

stop_agents
rm -rf "$ROOT"
mkdir -p "$ROOT/remotes"

# Keep the demo's commits out of the user's identity and signing config.
export GIT_AUTHOR_NAME="Demo" GIT_AUTHOR_EMAIL="demo@example.com"
export GIT_COMMITTER_NAME="Demo" GIT_COMMITTER_EMAIL="demo@example.com"
GIT="git -c commit.gpgsign=false -c advice.detachedHead=false"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# fill DIR MEGABYTES [FILES] — a plausible dependency directory of a given size.
fill() {
  local dir="$1" mb="$2" files="${3:-30}"
  mkdir -p "$dir"
  local i
  for i in $(seq 1 "$files"); do
    printf 'module.exports = { name: "pkg-%s", version: "1.%s.0" };\n' "$i" "$i" \
      > "$dir/pkg-$i.js"
  done
  # One large blob, so the reported size is realistic without writing thousands
  # of small files, which would make this script slow for no visual gain.
  if [ "$mb" -gt 0 ]; then
    dd if=/dev/zero of="$dir/.cache.bin" bs=1048576 count="$mb" 2>/dev/null
  fi
}

write() { mkdir -p "$(dirname "$1")"; printf '%b\n' "$2" > "$1"; }

# days_ago N — an ISO timestamp N days in the past, for commit dates.
days_ago() {
  date -v "-$1d" "+%Y-%m-%dT%H:%M:%S" 2>/dev/null \
    || date -d "-$1 days" "+%Y-%m-%dT%H:%M:%S"
}

# Commits are dated into the past so the "last active" column and the staleness
# rule have something real to work with. Without this every worktree would read
# as touched seconds ago.
commit_in() {
  local when; when=$(days_ago "$(( RANDOM % 90 + 3 ))")
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" \
    $GIT -C "$1" add -A
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" \
    $GIT -C "$1" commit -qm "$2"
}

# repo NAME — a repository with a remote and an initial commit.
repo() {
  local name="$1" dir="$ROOT/$1"
  $GIT init -q --bare "$ROOT/remotes/$name.git"
  $GIT init -q -b main "$dir"
  write "$dir/README.md" "# $name\n\nInternal service."
  write "$dir/src/index.ts" "export function main() {\n  return \"$name\";\n}"
  write "$dir/package.json" "{\n  \"name\": \"$name\",\n  \"version\": \"0.1.0\"\n}"
  write "$dir/package-lock.json" "{\n  \"lockfileVersion\": 3,\n  \"name\": \"$name\"\n}"
  # Without this, the dependency directories below would be untracked files and
  # every worktree would read as dirty — which is exactly what a real
  # repository's .gitignore prevents, and what makes .env files unrecoverable.
  write "$dir/.gitignore" "node_modules/\n.venv/\ndist/\nbuild/\n.env\n.env.*\n*.log"
  local when; when=$(days_ago "$(( RANDOM % 200 + 200 ))")
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" $GIT -C "$dir" add -A
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" \
    $GIT -C "$dir" commit -qm "Initial commit"
  $GIT -C "$dir" remote add origin "$ROOT/remotes/$name.git"
  $GIT -C "$dir" push -q -u origin main
  $GIT -C "$dir" remote set-head origin main >/dev/null 2>&1 || true
}

# wt REPO BRANCH — a worktree at the sibling path yawm itself would choose.
#
# --no-track matters: branching from origin/main would otherwise set it as the
# upstream, so one local commit would report as an unpushed commit rather than
# as unmerged work. The functions that genuinely push set their own upstream.
wt() {
  local slug="${2//\//-}"
  local path="$ROOT/$1-worktrees/$slug"
  $GIT -C "$ROOT/$1" worktree add -q --no-track "$path" -b "$2" origin/main
  printf '%s' "$path"
}

# ---------------------------------------------------------------------------
# The four verdicts
# ---------------------------------------------------------------------------

# Merged into the default branch: the work landed, nothing would be lost.
disposable_merged() {
  local repo="$1" branch="$2" file="$3" mb="${4:-0}"
  local path; path=$(wt "$repo" "$branch")
  write "$path/$file" "export const value = 42;"
  commit_in "$path" "Add ${file##*/}"
  $GIT -C "$ROOT/$repo" merge -q --no-ff "$branch" -m "Merge $branch"
  $GIT -C "$ROOT/$repo" push -q origin main
  [ "$mb" -gt 0 ] && fill "$path/node_modules" "$mb"
  printf '%s' "$path"
}

# A genuine squash merge: the target receives the tree effect without the
# branch commit becoming its ancestor.
disposable_squashed() {
  local repo="$1" branch="$2" file="$3" mb="${4:-0}"
  local path; path=$(wt "$repo" "$branch")
  write "$path/$file" "export const value = 7;"
  commit_in "$path" "Implement ${branch##*/}"
  $GIT -C "$path" push -q -u origin "$branch"
  $GIT -C "$ROOT/$repo" merge -q --squash "$branch"
  commit_in "$ROOT/$repo" "Land ${branch##*/}"
  $GIT -C "$ROOT/$repo" push -q origin main
  $GIT -C "$ROOT/$repo" push -q origin --delete "$branch"
  [ "$mb" -gt 0 ] && fill "$path/node_modules" "$mb"
  printf '%s' "$path"
}

# A rewritten implementation with the same intent but different content. The
# matching subject locates a candidate, but only a person can judge equivalence.
review() {
  local repo="$1" branch="$2" mb="${3:-0}"
  local path; path=$(wt "$repo" "$branch")
  local name="${branch##*/}"
  local mod="${name//-/_}"
  write "$path/src/$name.ts" "export interface Options {\n  retries: number;\n  timeoutMs: number;\n}\n\nexport async function run(options: Options) {\n  for (let attempt = 0; attempt < options.retries; attempt += 1) {\n    try {\n      return await perform(options);\n    } catch (error) {\n      if (attempt === options.retries - 1) throw error;\n      await wait(options.timeoutMs * 2 ** attempt);\n    }\n  }\n}\n\nfunction wait(ms: number) {\n  return new Promise((resolve) => setTimeout(resolve, ms));\n}\n\nasync function perform(options: Options) {\n  return { ok: true, waited: options.timeoutMs };\n}"
  write "$path/src/index.ts" "import { run } from \"./$name\";\n\nexport function main() {\n  return run({ retries: 3, timeoutMs: 250 });\n}"
  write "$path/README.md" "# $repo\n\nInternal service.\n\n## ${name//-/ }\n\nAdds ${name//-/ } support, with retries and backoff."
  local subject="Add ${name//-/ }"
  commit_in "$path" "$subject"
  write "$ROOT/$repo/src/$name.ts" "export const implementation = \"rewritten on main\";"
  write "$ROOT/$repo/src/index.ts" "export function main() {\n  return \"different implementation\";\n}"
  write "$ROOT/$repo/README.md" "# $repo\n\nInternal service with a different ${name//-/ } implementation."
  commit_in "$ROOT/$repo" "$subject"
  $GIT -C "$ROOT/$repo" push -q origin main
  [ "$mb" -gt 0 ] && fill "$path/node_modules" "$mb"
  printf '%s' "$path"
}

# Uncommitted work, plus gitignored files that exist nowhere else.
keep_dirty() {
  local repo="$1" branch="$2" mb="${3:-0}"
  local path; path=$(wt "$repo" "$branch")
  write "$path/src/handler.ts" "export const handler = async () => ({ status: 200 });"
  $GIT -C "$path" add -A
  write "$path/src/index.ts" "export function main() {\n  return \"rewritten\";\n}"
  write "$path/notes.md" "Half-finished thought."
  write "$path/.env" "DATABASE_URL=postgres://localhost:5432/dev\nSTRIPE_KEY=sk_test_not_a_real_key"
  write "$path/.env.local" "FEATURE_FLAGS=beta"
  [ "$mb" -gt 0 ] && fill "$path/node_modules" "$mb"
  printf '%s' "$path"
}

# Pushed once, then committed again locally: those commits exist nowhere else.
keep_unpushed() {
  local repo="$1" branch="$2" mb="${3:-0}"
  local path; path=$(wt "$repo" "$branch")
  write "$path/src/queue.ts" "export const queue: string[] = [];"
  commit_in "$path" "Add queue"
  $GIT -C "$path" push -q -u origin "$branch"
  write "$path/src/queue.ts" "export const queue: string[] = [];\nexport const dead: string[] = [];"
  commit_in "$path" "Add dead letter queue"
  [ "$mb" -gt 0 ] && fill "$path/node_modules" "$mb"
  printf '%s' "$path"
}

# Deliberately locked, so yawm leaves it alone.
keep_locked() {
  local path; path=$(wt "$1" "$2")
  write "$path/src/wip.ts" "// in progress"
  commit_in "$path" "Start ${2##*/}"
  $GIT -C "$ROOT/$1" worktree lock "$path" --reason "$3"
  printf '%s' "$path"
}

# Directory removed behind git's back, leaving stale administrative data.
broken() {
  local path; path=$(wt "$1" "$2")
  rm -rf "$path"
}

echo "Building demo workspace in $ROOT"

# ---------------------------------------------------------------------------
# Repositories
#
# Shaped the way a team running agents in parallel actually ends up: a couple
# of heavy services carrying most of the worktrees and most of the disk, and a
# long tail of smaller ones with a single leftover each.
# ---------------------------------------------------------------------------

repo atlas-api
disposable_squashed atlas-api "feat/oauth-device-flow" "src/oauth.ts" 180 >/dev/null
disposable_merged   atlas-api "fix/rate-limit-headers" "src/limits.ts" 165 >/dev/null
review              atlas-api "refactor/auth-middleware" 210 >/dev/null
AGENT_A=$(keep_dirty atlas-api "feature/retry-logic" 195)
keep_unpushed       atlas-api "feat/webhook-signatures" 170 >/dev/null
keep_locked         atlas-api "agent/migrate-to-v2" "agent running" >/dev/null
broken              atlas-api "chore/stale-session"

repo atlas-web
disposable_squashed atlas-web "feat/pricing-page" "app/pricing.tsx" 240 >/dev/null
disposable_merged   atlas-web "fix/mobile-nav-overflow" "app/nav.tsx" 220 >/dev/null
review              atlas-web "feature/dark-mode-tokens" 255 >/dev/null
AGENT_B=$(keep_dirty atlas-web "refactor/checkout" 260)
keep_unpushed       atlas-web "feat/search-filters" 230 >/dev/null

repo pulse
disposable_merged   pulse "fix/histogram-buckets" "src/buckets.ts" 40 >/dev/null
review              pulse "agent/streaming-ingest" 55 >/dev/null
keep_dirty          pulse "feat/percentile-sketches" 48 >/dev/null
broken              pulse "chore/abandoned-run"

repo orbit-mobile
disposable_merged   orbit-mobile "fix/ios-safe-area" "src/layout.tsx" 310 >/dev/null
keep_dirty          orbit-mobile "feat/offline-sync" 330 >/dev/null

repo mosaic-ui
disposable_merged   mosaic-ui "feat/combobox" "src/combobox.tsx" 95 >/dev/null
review              mosaic-ui "audit/accessibility" 88 >/dev/null

repo ledger
disposable_squashed ledger "feat/double-entry" "internal/entry.go" 12 >/dev/null
review              ledger "fix/decimal-rounding" 14 >/dev/null

repo sentinel
disposable_squashed sentinel "feat/secret-scanning" "src/scan.ts" 22 >/dev/null
keep_locked         sentinel "agent/cve-sweep" "long-running scan" >/dev/null

repo relay
keep_unpushed       relay "feat/backpressure" 30 >/dev/null

repo harbor
disposable_squashed harbor "chore/bump-providers" "main.tf" 5 >/dev/null

repo beacon
review              beacon "feature/digest-emails" 18 >/dev/null

repo quill
disposable_merged   quill "fix/broken-anchors" "docs/anchors.md" 8 >/dev/null

repo forge
keep_dirty          forge "feat/matrix-cache" 26 >/dev/null

repo almanac
review              almanac "agent/backfill-2024" 44 >/dev/null

repo cinder
disposable_squashed cinder "feat/shell-completions" "src/complete.rs" 3 >/dev/null

repo nimbus
keep_unpushed       nimbus "feat/leader-election" 16 >/dev/null

repo tessera
disposable_merged   tessera "chore/sync-tokens" "tokens.json" 6 >/dev/null

repo vellum
review              vellum "feature/table-editing" 20 >/dev/null

# ---------------------------------------------------------------------------
# Age the workspace
#
# Everything above was created seconds ago, which would make every worktree
# read as "changed recently" and mask the verdicts. Backdating gives the
# recent-activity rule and the last-active column something real to show.
# ---------------------------------------------------------------------------

echo "Ageing timestamps…"
AGES=(2 4 6 9 13 18 24 31 40 52 67 88 110 140 180 240)
i=0
for dir in "$ROOT"/*-worktrees/*/ "$ROOT"/*/; do
  [ -d "$dir" ] || continue
  case "$dir" in "$ROOT/remotes/"*) continue ;; esac
  days=${AGES[$(( i % ${#AGES[@]} ))]}
  i=$(( i + 1 ))
  stamp=$(date -v "-${days}d" +%Y%m%d%H%M 2>/dev/null \
       || date -d "-${days} days" +%Y%m%d%H%M)
  find "$dir" -not -path '*/.git/*' -exec touch -t "$stamp" {} + 2>/dev/null || true
done

# Two worktrees left deliberately fresh, so "changed recently" is visible too —
# and so the agents below have something plausible to be working on.
for fresh in "$AGENT_A" "$AGENT_B"; do
  find "$fresh" -not -path '*/.git/*' -exec touch {} + 2>/dev/null || true
done

# ---------------------------------------------------------------------------
# Fake agents
#
# Processes whose working directory sits inside a worktree, which is what
# produces the "something is running" badge. Plain sleeps; --clean stops them.
# ---------------------------------------------------------------------------

if [ "$AGENTS" = "1" ]; then
  : > "$AGENT_PIDS"
  for dir in "$AGENT_A" "$AGENT_B"; do
    ( cd "$dir" && exec sleep 86400 ) >/dev/null 2>&1 &
    echo $! >> "$AGENT_PIDS"
  done
  echo "Started 2 background processes (stopped by --clean)"
fi

total=$(du -sh "$ROOT" 2>/dev/null | cut -f1 | tr -d ' ')
repos=$(find "$ROOT" -maxdepth 2 -name '.git' -type d 2>/dev/null | wc -l | tr -d ' ')
trees=$(find "$ROOT" -maxdepth 2 -name '.git' 2>/dev/null | wc -l | tr -d ' ')

cat <<SUMMARY

Done. $repos repositories, $trees worktrees, $total on disk:
  $ROOT

Point yawm at it with "Scan a folder", or:
  yawm list $ROOT/atlas-api

Remove it, and stop the fake agents, with:
  $0 --clean
SUMMARY
