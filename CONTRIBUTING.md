# Contributing to yawm

Thanks for helping improve yawm. Safety changes deserve particular care: a
false **Disposable** verdict or an unsafe removal is worse than refusing an
operation that would have been safe.

## Before opening a change

- Search existing issues and pull requests first.
- Open an issue before starting a large feature or a change to the safety
  model. Small fixes do not need prior approval.
- Keep pull requests focused. Separate mechanical moves from behavior changes.
- Add a regression test for bug fixes and explain any user-visible tradeoff.

## Development setup

Prerequisites are Rust 1.90 or newer, Node.js 22, npm, and Git.

```sh
git clone https://github.com/serhatandic/yawm.git
cd yawm
npm ci --prefix apps/desktop
```

Run the checks used by CI:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
npm test --prefix apps/desktop
npm run build --prefix apps/desktop
```

Run the desktop app locally with:

```sh
cd apps/desktop
npm run tauri dev
```

Linux development also requires Tauri's WebKitGTK 4.1 system dependencies.
See the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for
the packages used by your distribution.

## Pull requests

Describe what changed, why it changed, and how you verified it. Keep commit
messages short and factual. Do not include generated build output, credentials,
AI session metadata, or co-author trailers added by coding tools.

## Contribution license

yawm is source-available for noncommercial use. To keep commercial licensing
rights coherent as the project accepts outside work, every contributor must
accept the [Contributor License Agreement](CONTRIBUTOR_LICENSE_AGREEMENT.md).

You retain ownership of your contribution. The agreement grants the project
owner the additional rights needed to include it in both the public
noncommercial project and any separately licensed commercial version. A pull
request cannot be merged until every contributor to it has accepted those
terms.
