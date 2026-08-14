# yawm

**A desktop app and CLI that tells you which Git worktrees are safe to delete — and shows you why.**

[![CI](https://github.com/serhatandic/yawm/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/serhatandic/yawm/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/serhatandic/yawm?include_prereleases&label=release)](https://github.com/serhatandic/yawm/releases)
[![License: PolyForm Noncommercial 1.0.0](https://img.shields.io/badge/license-PolyForm%20Noncommercial%201.0.0-blue.svg)](LICENSE)

AI coding agents make it easy to create a worktree for every task. Those
worktrees accumulate, their dependency folders consume disk space, and it
becomes difficult to tell which branches are finished and which still contain
work worth keeping.

yawm gives every worktree a conservative verdict — **Keep**, **Disposable**,
**Review**, or **Broken** — with the evidence beside it. If yawm cannot prove
that a worktree is safe to remove, it never calls it Disposable.

[Download for macOS, Windows, or Linux](https://github.com/serhatandic/yawm/releases)
· [Report a bug](https://github.com/serhatandic/yawm/issues/new/choose)
· [Build from source](#build-from-source)

<!--
README media placeholder:

Add docs/assets/yawm-overview.png or docs/assets/yawm-overview.gif here. Show
the main list with all four verdicts visible and the Changes inspector open.
Keep an animated capture around 8–12 seconds and include this alt text:

"yawm listing Git worktrees with Keep, Disposable, Review, and Broken verdicts,
alongside a panel showing the selected worktree's changes."
-->

## Download

Download the current build from [GitHub Releases](https://github.com/serhatandic/yawm/releases).

| Platform | Package |
|---|---|
| macOS 10.15+ | Universal `.dmg` |
| Windows 10 1803+ | `.exe` installer |
| Linux | `.AppImage`, `.deb`, or `.rpm` |

> [!NOTE]
> Current builds are unsigned releases. macOS Gatekeeper and Windows
> SmartScreen may display a warning until release signing is configured.

yawm needs Git. Git 2.36 or newer is recommended; older versions work except
for the unusual case of a worktree path containing a newline. Linux packages
require WebKitGTK 4.1; this is available on Ubuntu 22.04+, Debian 12+, and
Fedora 37+.

Each version is also mirrored as an OCI artifact in
[GitHub Packages](https://github.com/users/serhatandic/packages/container/package/yawm).
This is useful for automated downloads and release mirroring; the files inside
are the same desktop installers published on the Releases page.

```sh
oras pull ghcr.io/serhatandic/yawm:v0.1.0
```

## Quick start

1. Open yawm and choose **Add a repo**, or choose **Scan a folder** to discover
   repositories beneath it.
2. Filter to **Disposable** to see worktrees whose committed effect is already
   present on the default branch.
3. Open **Changes** before removing anything uncertain. The comparison includes
   committed and uncommitted work.

Use <kbd>⌘</kbd><kbd>K</kbd> on macOS or <kbd>Ctrl</kbd><kbd>K</kbd> on Windows
and Linux to find any worktree and open its changes. The detail panel can then
open that worktree in your configured editor.

## What yawm helps with

- **Clean up with evidence.** Every verdict includes the reason that produced it.
- **Inspect the real change.** View file statistics and patches relative to the
  default branch, including uncommitted changes.
- **See active work.** Running processes and recent activity prevent an active
  worktree from being treated as disposable.
- **Protect local-only files.** Gitignored `.env` files are surfaced before
  deletion.
- **Create ready-to-use worktrees.** Copy environment files and link dependency
  directories when lockfiles prove the dependency trees agree.
- **Recover disk space safely.** Removal uses Git's worktree machinery, force is
  explicit, and **Move to Trash** remains available.

## The verdicts

Every worktree receives exactly one verdict.

| Verdict | Meaning |
|---|---|
| **Keep** | Protected, active, dirty, unpushed, or known to contain work absent from the default branch. |
| **Disposable** | Its committed tree effect is proven to exist in a default-branch snapshot. |
| **Review** | yawm could not prove whether rewritten or otherwise ambiguous work landed. Inspect the changes and decide. |
| **Broken** | The directory is missing and only stale Git worktree metadata remains. |

Verdict rules run from most protective to least protective. Unknown evidence
becomes Review, never Disposable: a false positive could lose work, while a
false negative costs only a little cleanup time.

## Safe by construction

1. The main worktree is never removable.
2. Removal goes through `git worktree remove`, so Git's metadata stays
   consistent.
3. Force is never implicit. If Git would refuse, yawm names the dirty files,
   unpushed commits, unique environment files, and running processes that need
   explicit acknowledgement.
4. A removal plan is revalidated immediately before anything is deleted. If
   the worktree changed after confirmation, the operation stops.
5. Moving a worktree to the operating system's Trash is available as a
   recoverable alternative.
6. Branch deletion is separate and opt-in.

yawm works locally. It reads local Git objects, process information, and
filesystem state; it does not send repository contents, diffs, or usage data
to a service.

<details>
<summary><strong>How yawm proves that work landed</strong></summary>

`Landed` is a proof, not an inference from branch metadata:

- **Ancestry** proves ordinary merges, including work merged into a local
  default branch but not yet pushed.
- **Identical trees** prove that the branch and target snapshots contain the
  same files.
- A clean **`merge-tree --write-tree` no-op** proves that merging the branch
  adds nothing to the target tree.
- When the target has moved and now conflicts, patch IDs, matching trees, and
  commit subjects locate historical candidates. Each candidate is separately
  verified as a reachable no-op.

Proof commands use immutable object IDs and ignore replace refs. No-op proofs
are rejected when changed paths have merge attributes or the repository uses a
custom merge driver. Remote branch deletion is never evidence that work
landed.

Regular scans batch ancestry and tree proofs. `merge-tree` runs only for
settled worktrees whose verdict still depends on it. Historical search runs
only when details are opened, stops after 300 target commits, and caches
immutable results for the app session.

</details>

## Creating worktrees

`git worktree add` creates a checkout without local environment files or
dependencies. yawm can prepare the new worktree as part of creation:

- Environment files are **copied**, because they are small and may diverge.
- Dependency directories are **linked**, because they are large and
  regenerable. Windows uses a junction and does not require administrator
  rights.

Before recommending a dependency link, yawm compares the lockfile blob between
the base ref and the current checkout. A different lockfile leaves the option
unticked and identifies the mismatch.

Creation also catches a branch already checked out elsewhere and a destination
inside the repository itself before asking Git to proceed. If Git fails after
partially creating a worktree, yawm rolls back the directory, registration,
and newly created branch.

## CLI

The CLI uses the same `yawm-core` safety logic as the desktop app.

```sh
yawm list                    # configured repositories, or the current folder
yawm list ~/code/my-project  # one repository or scan root
yawm list --disposable       # only worktrees proved safe to delete
yawm list --no-size          # skip disk measurement
yawm list --no-procs         # skip process detection
```

Example output:

```text
repo /Users/me/code/api
  keep       main                    356 KB  2h   Main worktree · ↑2
  disposable feat/login              1.8 GB  6d   Work is contained in origin/main
  review     fix/header               1.9 GB  2d   Could not verify whether rewritten work landed
  keep       feat/payments           2.1 GB  4m   Has uncommitted changes · 3 changed
  broken     old-experiment               —  —    Directory is missing

5 worktrees · 6.2 GB total · 3.7 GB reclaimable
```

Install the CLI from a checkout:

```sh
cargo install --path crates/yawm-cli
```

## Build from source

Prerequisites:

- Rust 1.90 or newer
- Node.js 22 and npm
- Git
- [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/) for
  desktop development

```sh
git clone https://github.com/serhatandic/yawm.git
cd yawm/apps/desktop
npm ci
npm run tauri dev
```

The repository is a Rust workspace with a separate frontend package:

```text
crates/yawm-core/   Git discovery, proofs, verdicts, and safe operations
crates/yawm-cli/    Thin command-line adapter over yawm-core
apps/desktop/       Tauri shell and React interface
```

The core crate has no GUI dependency. CI builds and tests the CLI on macOS,
Windows, and Linux, which keeps safety decisions shared between both
interfaces.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
npm test --prefix apps/desktop
npm run build --prefix apps/desktop
```

Release bundles include `LICENSE`, `NOTICE`, and the generated third-party
notices. After changing dependencies, install `cargo-about` 0.9.1 and run
`npm run licenses:write --prefix apps/desktop`; CI rejects stale notices.

Tests use real temporary repositories for ordinary merges, squash merges,
historical no-ops, conflicts, dirty worktrees, locks, missing directories,
custom merge drivers, replace refs, rollback, and concurrent changes. One
invariant is tested directly: nothing with uncommitted or unpushed work is ever
classified as Disposable.

See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## Scope

yawm manages local Git worktrees. Port allocation, development-server
orchestration, launching or supervising agents, terminal multiplexers, and
submodule automation are intentionally outside its scope.

## Support and security

Use [GitHub Issues](https://github.com/serhatandic/yawm/issues/new/choose) for
bugs and focused feature requests. Report vulnerabilities privately according
to the [security policy](SECURITY.md).

## License

yawm is source-available under the
[PolyForm Noncommercial License 1.0.0](LICENSE). You may use, study, modify,
and redistribute it for permitted noncommercial purposes. Commercial use,
including monetizing a fork or derivative, requires a separate written license
from the copyright holder. See [NOTICE](NOTICE) for the required copyright
notice.

Contributions are welcome under the terms in
[CONTRIBUTING.md](CONTRIBUTING.md) and the
[Contributor License Agreement](CONTRIBUTOR_LICENSE_AGREEMENT.md). Those terms
let the project owner continue to offer commercial licenses without removing
the community's noncommercial rights to accepted contributions.
