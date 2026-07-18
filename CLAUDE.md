# Supply Drop BBS - Claude instructions

## Platform — Linux only (Windows is NOT supported)

This project is **not supported on Windows**. Build, test, run, and commit it on
**Linux only** — a Linux host or WSL. Do **not** use Windows-native Rust
toolchains, cargo, rustup, or shells for this project: they produce
environment-specific failures (toolchain/clippy component breakage, text-encoding
issues, path quirks) that do not reflect the actual state of the codebase. All
builds, the pre-commit checks, and CI run on Linux.

## Pre-commit checklist

Before every `git commit`, run ALL of the following **on Linux** and fix any
failures before committing. The Rust version is pinned in `rust-toolchain.toml`,
so plain `cargo` automatically uses the correct toolchain:

```
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo doc --workspace --no-deps --all-features
```

Be certain to update documentation, where relevant.


## Branching and pull requests

**`next` is the integration branch and the default branch.** All feature/fix work
branches from `next` and PRs target `next`, not `main`. `main` tracks only what
has actually been released — `next` is periodically merged into `main` via its
own PR to cut a release.

**Never commit directly to `next` or `main`.** All changes must go through a
feature branch and a pull request, no matter how small.

Branch naming follows the pattern `<type>/<short-description>`, e.g.:
- `feat/guest-room-access`
- `fix/issue-45-root-cli`
- `perf/binary-size-optimisations`
- `chore/bump-v0-8-3`

Workflow:
1. Create a feature branch from `next` (`git checkout -b <branch> next`, or
   `git checkout -b <branch> origin/next` if `next` isn't checked out locally)
2. Commit changes to the feature branch
3. Push the branch and open a PR with `gh pr create --base next --head <branch>`
4. Never push directly to `next` or `main`
5. Periodically, `next` is merged into `main` via its own PR
   (`gh pr create --base main --head next`) to cut a release.

## Commit style

All commits must use [Conventional Commits](https://www.conventionalcommits.org/) format:

```
<type>[optional scope]: <description>
```

Common types: `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `build`, `ci`

## SQLite migrations - NEVER modify applied migrations

Migrations in `crates/bbs-core/migrations/` are **append-only**. Once a migration file has been committed and could have been applied to any database (dev, staging, or production), it must never be edited. sqlx records a checksum of each applied migration; changing the file content breaks the checksum and crashes the server on startup.

Rules:
- **Never edit an existing migration file.** Create a new numbered file instead.
- **Never add rooms, columns, indexes, or seed data to an existing migration.** Add a new migration.
- If you need to undo something a migration did, write a new migration that reverses it.
- The only safe operation on an existing file is fixing a typo in a SQL comment - but even that changes the checksum, so don't do it.

## Rust toolchain

The toolchain is pinned in `rust-toolchain.toml` (currently `1.96`). rustup
auto-selects it for any `cargo` command run inside the repo, so do not hardcode a
version in commands. CI (`.github/workflows/ci.yml`) and the release workflow
(`.github/workflows/release.yml`) pin the same version — keep all three in sync
when bumping. Build on Linux only (see Platform above).
