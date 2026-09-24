# Feature: Command keymaps (presets + custom + revert)

**Tracking:** GH #354 · Status: draft, R&D branch `feat/354-command-keymaps` (not merged to `next`)

## Background

GH #354 asked for Maximus-BBS-style navigation (`j` jump to message #, `n`/`p` next/previous,
etc.). Investigation (2026-09-24) found the underlying capability already exists under Citadel
letters — `F <id>` jumps to a message and works inside reading mode (#356), `F`/`R` step forward
and backward once in reading mode — so the literal jump/next/previous request is **already
resolved**, just under different keys than the requester expected.

What's left, and what the maintainer was weighing in the issue thread, is a **preset keymap
system**: let a sysop remap the BBS's command letters to match a classic system's conventions,
without forking the command set itself or fragmenting the UX for everyone. From the maintainer's
own comment on the issue:

> It's possible to provide a "preset keymap" system. However, that potentially means that users
> will log into one SDBBS and the user experience could be completely different than what they
> are used to... I am not a fan of this as a whole. But I can see where it might make it easier
> for people who hate learning new things.

This spec follows the maintainer's stated direction: presets stay **within Supply Drop's own
command semantics** — a keymap only changes which key/word invokes an existing action, never adds
new behavior. This scopes the feature to remapping, not reimplementing another BBS's feature set.

## Goals

1. A sysop can select a **preset keymap** (Supply Drop native, plus a short list of classic BBS
   systems) that remaps single-letter commands to that system's conventions.
2. A sysop can **upload/define a custom keymap** (their own remapping), for boards whose users
   share a specific background not covered by the built-in presets.
3. A sysop can **revert to Supply Drop's own native keymap** at any time, with no data loss (the
   native map isn't "erased" by selecting a preset — it's just not the active one).
4. The active keymap applies consistently everywhere a user can type a command: the MeshCore and
   Meshtastic radio transports, the CLI, and reading mode (which today is parsed separately from
   everything else — see Constraints).
5. Help text reflects the active keymap, not always the native one.

## Non-goals

- Adding new BBS *functionality* beyond what Supply Drop's command set already does. A keymap
  remaps existing actions; it does not add e.g. file areas or bulletins some classic systems had.
- Per-user keymaps. This is a board-wide (sysop-set) setting, matching how `command_prefix` and
  other transport-level behavior already work — not a per-session preference, at least not in v1.
- Reproducing every quirk of a classic system (e.g. Maximus's two-letter mail commands `lm`/`jm`
  mapping onto a *separate* Mail sub-menu with its own letters, vs. Supply Drop's model where Mail
  is just another room and the same room-level letters apply there too). Presets remap letters,
  they don't change Supply Drop's room/message model.
- Fixing the pre-existing bugs this research surfaced (listed below) — they're related, but
  separable, and some should land *before* this feature so a preset doesn't ship pointing at a
  broken action.

## User scenarios

1. **Sysop picks a preset.** From the sysop admin surface, sets `keymap = "maximus"` (or
   equivalent). New sessions immediately see the remapped keys in help text and command parsing;
   existing connected sessions... (see open question: live vs. restart).
2. **User navigates under the preset.** A user on a board with the Maximus preset types `j 40`;
   it does what `F 40` does natively (jump to message #40). Typing `n`/`p` while reading does what
   `F`/`R` do natively.
3. **Sysop uploads a custom keymap.** A board with an unusual user base (e.g. a regional club
   standardized on a homebrew convention) supplies its own action→key table and activates it.
4. **Sysop reverts.** At any point, switching back to the native keymap is a single setting change
   — no re-installation, no data recovery step.
5. **A key collision is caught, not silently accepted.** If a preset or custom keymap maps two
   different actions to the same key (or a key already reserved — see Constraints, sysop/aide
   commands are a separate namespace), activating it fails with a clear error naming the
   collision, rather than making one action unreachable.

## Constraints found during investigation (2026-09-24)

These come from a full-repo command-surface mapping and directly shape the design:

- **Three independent hand-written parsers.** Canonical `Command::parse` (`bbs-plugin-api`, used
  by CLI/process/hello transports), MeshCore's `parse_command` (`bbs-mesh`), and Meshtastic's copy
  (`bbs-meshtastic`) each match on keyword strings by hand. A test
  (`sysop_words_match_canonical_parser`) checks only 8 sysop words for parity across two of the
  three and documents that the parsers "drifted once" before. **A keymap must be expressed once
  and consumed by all of them**, not hand-copied a fourth time.
- **`bbs-plugin-api` cannot depend on `bbs-core`.** The canonical parser lives in
  `bbs-plugin-api`, which is upstream of `bbs-core` in the dependency graph. A keymap type shared
  by the parser and the host must live at or below `bbs-plugin-api`, or the parser needs to accept
  keymap data as a parameter rather than importing it.
- **Reading mode is a fourth, separate parser.** Once a session is `Workflow::Reading`, the next
  line arrives as raw `WorkflowReply` text and is matched directly in `bbs-core`'s `host.rs`
  (`F`/`R`/`E`/`H`/`D`/`D <id>`), bypassing all three transport parsers entirely. A keymap has to
  reach this code path too, which today has no keymap-awareness at all and isn't reachable from
  `bbs-plugin-api`.
- **Help text has a hard byte budget.** `help_strings_fit_mesh_payload` requires each help
  constant to fit in the mesh reply size limit (156 bytes) with margin; `HELP_QUICK_LOGGED_IN` is
  already at the limit. Help text can't simply grow to show "native key (preset key)" for every
  command — it needs a design that stays in budget per keymap.
- **Config changes mostly require a restart today**, but there's a working precedent for a *live*
  sysop-settable value: `access_policy: RwLock<AccessPolicy>` on `BbsHost`, changed in memory by a
  sysop command and persisted back to `config.toml` via `toml_edit` + the existing config lock
  (`persist_access_policy`). A live keymap setting should follow this precedent rather than
  requiring a restart, since a sysop testing presets with users online is the realistic case.
- **Message numbers are global DB ids, not per-room sequence numbers**, and rooms are numbered
  separately by their own DB id (shown in `K`). A "jump to message #12" preset command means the
  same thing Supply Drop's own `F 12` already means — this is inherent to Supply Drop's model, not
  something a keymap changes, but preset documentation/help text must not imply per-room numbering
  the way some source systems (Maximus) actually have.

## Related bugs found during investigation (tracked separately, not fixed by this spec)

Filed as follow-ups; some are worth fixing **before** shipping the first preset, since a preset's
help text would otherwise advertise a broken path:

- `H N` (and `H M`, `H R`) shows the *Navigation* help topic, not per-command help for `N`/`M`/`R`,
  because topic-name aliases are matched before single-letter command names.
- `S` (scan) and `.FF` (fast-forward) in the Mail room likely never show anything, because both
  call `list_in_room`/`list_recent_in_room`, which join `room_messages`, and mail is stored via a
  separate `post_direct` path that never populates that join table.
- MeshCore's own mail-arrival notification tells the user to reply `mail`, which parses to
  `Unknown` (the actual command is `M`).
- `E <text>` typed while in reading mode is swallowed by the "unrecognized input exits reading
  mode" catch-all instead of composing a reply with that text.

## Open questions (need maintainer input before `plan.md` is finalized)

1. **Live or restart-required?** Given the `access_policy` precedent, live seems achievable and
   more testable — but does the sysop admin surface (web UI, CLI, or a sysop-only BBS command?)
   exist yet for this, or does it need building as part of this feature?
2. **Where does the "top 5" preset list land, and how opinionated should each preset be?** See the
   research pass on classic-system command tables (separate document, this directory) for
   candidates and sourcing confidence.
3. **Custom keymap upload — through which surface?** The web admin (file upload), a CLI command
   (`supply-drop-bbs keymap import <file>`), or a BBS-native command for sysops without shell
   access? The project's precedent (`config.toml`, backups) leans CLI/web, not BBS-native, for
   anything resembling bulk config.
4. **Format for a custom keymap file.** TOML (matches `config.toml`'s own format and the project's
   `toml_edit` tooling) is the natural fit; needs a defined schema and validation (reject unknown
   actions, reject collisions, reject a keymap that overlaps the sysop/aide-only command
   namespace).
5. **Scope of "action" a keymap can remap.** Every `Command` variant plus the reading-mode-only
   `F`(no id)/`R`/`E`/`H`/`D` keys? Or a curated subset (the ones classic-system presets actually
   need) to keep the collision-validation surface small for v1?
