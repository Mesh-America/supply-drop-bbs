# Beads - AI-Native Issue Tracking

Welcome to Beads! This repository uses **Beads** for issue tracking - a modern, AI-native tool designed to live directly in your codebase alongside your code.

---

## Setup for this repository

This project's issue data is published to `refs/dolt/data` on the GitHub
remote, alongside the source branches. A fresh clone starts empty until you
bootstrap it.

**bd 1.3.0 or newer is required.** The database schema here has been migrated
to v66; older binaries are refused by a pending-migration gate.

### New machine or new teammate

```bash
git clone git@github.com:Mesh-America/supply-drop-bbs.git
cd supply-drop-bbs
bd bootstrap     # detects refs/dolt/data, clones it, wires up origin
bd list          # should show the existing issues, not an empty tracker
```

### Day to day

```bash
bd dolt pull     # before starting work
bd dolt push     # after
```

Dolt merges at the cell level, so two people editing different issues never
conflict, and since 1.3.0 a field-level three-way merge means editing
different fields of the *same* issue doesn't conflict either. Only genuinely
contested cells fall back to last-write-wins.

### Only one machine may run a schema migration

This is the one way to break sync irrecoverably, so it's worth reading twice.
The upstream procedure is at
<https://beads.gascity.com/getting-started/upgrading> — follow it there if
this summary and that page ever disagree.

When a new bd version ships a schema migration, the order matters and the
binary you're on at each step matters:

1. **Every clone, still on the old binary:** publish everything and get in
   sync, then stop editing until the upgrade is done.

   ```bash
   bd dolt push
   bd dolt pull
   ```

2. **The designated migrator only** — exactly one machine — installs the new
   binary, then migrates and publishes:

   ```bash
   bd export --all -o .beads/backup/pre-migrate.jsonl
   bd migrate          # --force if the remote-backed gate refuses
   bd dolt push
   ```

3. **Every other clone:** install the new binary, then *adopt* the migrated
   database with `bd bootstrap`.

   Do not try to `bd dolt pull` here — bd refuses it, because the clone still
   has pending migrations of its own. Re-cloning is the intended path, and
   it's safe precisely because step 1 already pushed that clone's work.
   Skipping step 1 and re-cloning here loses anything unpushed.

If two clones migrate independently, the schema forks and `bd dolt pull` can
no longer merge them. bd's own error text describes that break as silent and
unrecoverable, which is why it refuses to auto-migrate a remote-backed
database at all: `bd migrate --force` (or `BD_ALLOW_REMOTE_MIGRATE=1` for
scripted use) overrides the refusal, and belongs only on the one designated
machine.

### Local-only files

`bd backup init` writes `dolt-backup.json` and `dolt-backup-state.json`
recording whichever local path you pointed it at. Those are per-machine and
gitignored from the repo root, not from `.beads/.gitignore` (bd manages that
file itself).

`.beads/issues.jsonl` is an export for viewing and interchange. It is not the
sync channel and importing it is not a substitute for `bd dolt pull` — JSONL
import is upsert-only and cannot represent deletions.

### Note on git hooks

The beads git hooks shell out to `bd`. Commits for this project are made from
Linux/WSL (see CLAUDE.md), where a Windows-installed `bd` isn't on `PATH`, so
those hooks are inert on that path. Run `bd` commands directly from whichever
shell has it installed.

---

## What is Beads?

Beads is issue tracking that lives in your repo, making it perfect for AI coding agents and developers who want their issues close to their code. No web UI required - everything works through the CLI and integrates seamlessly with git.

**Learn more:** [github.com/steveyegge/beads](https://github.com/steveyegge/beads)

## Quick Start

### Essential Commands

```bash
# Create new issues
bd create "Add user authentication"

# View all issues
bd list

# View issue details
bd show <issue-id>

# Update issue status
bd update <issue-id> --claim
bd update <issue-id> --status done

# Sync with Dolt remote
bd dolt push
```

### Working with Issues

Issues in Beads are:
- **Git-native**: Stored in Dolt database with version control and branching
- **AI-friendly**: CLI-first design works perfectly with AI coding agents
- **Branch-aware**: Issues can follow your branch workflow
- **Sync-ready**: Uses Dolt remotes for backup and team sharing

## Why Beads?

✨ **AI-Native Design**
- Built specifically for AI-assisted development workflows
- CLI-first interface works seamlessly with AI coding agents
- No context switching to web UIs

🚀 **Developer Focused**
- Issues live in your repo, right next to your code
- Works offline, syncs when you push
- Fast, lightweight, and stays out of your way

🔧 **Git Integration**
- Dolt-native sync via bd dolt push / bd dolt pull
- Branch-aware issue tracking
- Dolt-native three-way merge resolution

## Get Started with Beads

Try Beads in your own projects:

```bash
# Install Beads
curl -sSL https://raw.githubusercontent.com/steveyegge/beads/main/scripts/install.sh | bash

# Initialize in your repo
bd init

# Create your first issue
bd create "Try out Beads"
```

## Learn More

- **Documentation**: [github.com/steveyegge/beads/docs](https://github.com/steveyegge/beads/tree/main/docs)
- **Quick Start Guide**: Run `bd quickstart`
- **Examples**: [github.com/steveyegge/beads/examples](https://github.com/steveyegge/beads/tree/main/examples)

---

*Beads: Issue tracking that moves at the speed of thought* ⚡
