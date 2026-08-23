# Supply Drop BBS - Codex instructions

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

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:970c3bf2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
