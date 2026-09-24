# Tasks: Command keymaps

Phased so each phase is independently mergeable-in-spirit (this branch itself stays R&D / not
merged per the maintainer's explicit instruction, but the phasing still matters for review size
and for being able to stop after any phase with something coherent).

## Phase 0 — Prerequisite bug fixes (recommend fixing before Phase 3 ships any preset)

These were found during the command-surface investigation, not introduced by this feature, but a
preset's help text would otherwise point at something broken:

- [ ] **P0.1** `H N` / `H M` / `H R` show the Navigation/Mail/Reading help *topic* instead of
      per-command help for `N`/`M`/`R`, because topic-name aliases are matched before command
      names in `help_for_command()`. Fix the match order or disambiguate.
- [ ] **P0.2** `S` (scan) and `.FF` (fast-forward) in the Mail room likely show nothing, because
      both call `list_in_room`/`list_recent_in_room` (joins `room_messages`), and mail is stored
      via `post_direct`, which never populates that join table. Needs a Mail-aware code path or a
      shared query that covers both storage shapes.
- [ ] **P0.3** MeshCore's mail-arrival notification says "Reply 'mail' to read" — `mail` parses to
      `Unknown`; the real command is `M`. Fix the notification text.
- [ ] **P0.4** `E <text>` typed while in reading mode is swallowed by the "unrecognized input exits
      reading mode" catch-all instead of composing a reply with that text.

File bd + GH issues for each (dual-track, per project convention) before starting; these are
small, independent, and NOT scoped to the R&D branch — normal `next`-targeted PRs.

## Phase 1 — Data model & native fallback (no behavior change)

- [ ] **P1.1** `KeymapAction` enum and `Keymap` struct in `bbs-plugin-api` (see plan.md's Data
      model). `Keymap::native()` const matching Supply Drop's current bindings exactly.
- [ ] **P1.2** Effective-table computation + Rule 1/Rule 2 validation (plan.md's "Validation
      rule"), as a pure function, unit-tested against both worked presets in plan.md and against
      deliberately broken inputs (a keymap that collides with itself; a keymap that orphans an
      action).
- [ ] **P1.3** `Command::parse` takes a `&Keymap` parameter; every existing call site passes
      `&Keymap::native()` (zero behavior change, proves the plumbing compiles end to end).

## Phase 2 — Close the parser-drift gap (prerequisite for Phase 3, valuable on its own)

- [ ] **P2.1** MeshCore's `parse_command` delegates to `Command::parse` for the keyword-matching
      core, keeping only its own prefix-stripping and one-shot login/register logic around the
      call. Extend `sysop_words_match_canonical_parser`-style coverage to the full keyword table,
      not just 8 sysop words.
- [ ] **P2.2** Same for Meshtastic's copy.
- [ ] **P2.3** Reading-mode's inline match in `host.rs` gains its own small
      `ReadingKeymapAction`-aware lookup (see plan.md's Architecture section) instead of the
      current hardcoded `match upper.as_str()`.

## Phase 3 — Presets: Maximus + Packet-BBS (the two worked in plan.md)

- [ ] **P3.1** `Keymap::maximus()` const matching plan.md's worked table exactly; regression test
      asserting every entry against the sourced table in `research-classic-bbs-commands.md`.
- [ ] **P3.2** `Keymap::packet_bbs()` const, same treatment.
- [ ] **P3.3** `[bbs] keymap = "native" | "maximus" | "packet-bbs"` config key, wired at startup.
- [ ] **P3.4** Live keymap switch: `RwLock<Keymap>` on `BbsHost` following the `access_policy`
      precedent exactly (in-memory change + `persist_access_policy`-style write-back via
      `config_lock`/`toml_edit`). Needs a sysop-facing way to trigger it — reuses whatever surface
      Open question 3 (spec.md) resolves to; if that's still undecided when this task starts, a
      minimal CLI subcommand (`supply-drop-bbs config set-keymap <name>`) is enough to prove the
      mechanism without blocking on the UX decision.
- [ ] **P3.5** Help text generates its per-action quick-reference line from the active keymap
      (plan.md's "Help text" section) instead of the hardcoded native-only constants.

## Phase 4 — Custom keymap upload + revert

- [ ] **P4.1** TOML schema for a custom keymap file (name, description, bindings — same shape as
      the built-in presets, so validation code is shared, not duplicated).
- [ ] **P4.2** Load + validate a custom keymap from `data_dir` on startup if configured; reject
      (with a clear, specific error — which rule, which key) rather than silently falling back to
      native on a bad file.
- [ ] **P4.3** Upload surface — depends on spec.md Open question 3's resolution.
- [ ] **P4.4** `keymap = "native"` (or equivalent) reverts cleanly; regression test confirming a
      board that activated a preset, then a custom keymap, then reverted, ends up bit-for-bit at
      `Keymap::native()` with nothing left over in config or `data_dir`.

## Phase 5 — Remaining presets (follow-up, not blocking Phase 1–4)

- [ ] **P5.1** PCBoard preset — extra care on the `Q`/`M`/`P` collisions (research doc's own
      warning); do not reuse those letters for Quit/Mail/Post.
- [ ] **P5.2** WWIV-family preset (also documents its Telegard/Renegade lineage in its
      description, per the research doc's redundancy note, rather than shipping three thin
      presets).
- [ ] **P5.3** Synchronet preset.

## Explicitly out of scope for this feature (see spec.md Non-goals)

- Wildcat! and RemoteAccess presets, until/unless someone sources their actual default menu
  letters (not just function names) from a primary document.
- Per-user keymap preference.
- Remapping sysop/aide-only commands.
