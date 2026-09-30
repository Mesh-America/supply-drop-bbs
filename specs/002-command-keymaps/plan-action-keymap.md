# Plan: action-based keymaps (replaces the "translate to the native key" approach)

Status: draft for TJ. Supersedes the translation design in `plan.md` and the
module doc of `crates/bbs-plugin-api/src/keymap.rs`. GH #354.

## Why change

Today a keymap turns the typed key into an action, then turns the action back
into the native key text ("R") and feeds that to the old parser. Two problems:

1. The old parser still decides what each command means. `- 5` becomes `R 5`,
   and reading mode has no `R <id>` case (#415). `s` changes meaning with its
   argument (#411).
2. Messages to the user hard-code the native key. A user on the Maximus preset
   is told "F - Forward" when their key is something else.

## Goals

1. Every key the user types resolves to an action through one table.
2. Each action parses its own arguments. No re-typing of native keys.
3. Any message that names a key names the key the user's keymap actually uses.
4. Reading mode uses the same table instead of matching raw text.

Non-goals: rebinding admin actions (still out of scope, see spec.md Q5), and
changing what any action does.

## Design

### 1. One table, native is just the default

`Keymap` holds `keyword -> action` entries plus, for each action, an ordered
list of keywords. The first keyword is the **primary** key, the one shown in
messages. The native table becomes `Keymap::native()` built from the same
structure, so there is no separate hidden match to keep in sync.

A preset is layered on native, as today: it overrides only what it lists.
Layering must keep the primary correct. If a preset binds `l` to
`ScanMessages`, `l` becomes the primary for that action, and native `s` stays
as a working alias unless the preset is marked exclusive (open question 1).

Validation keeps the current rules (no action left without a key, no duplicate
keyword) and adds: every action has a primary, and a keyword may not be a
reserved word (`register`, `login`, `cancel`, `stop`).

### 2. Parse by action

`Command::parse_with_keymap` becomes:

1. Trim, strip zero-width characters, handle cancel/stop and awaiting-reply
   (unchanged).
2. Split the first word. Look it up in the keymap to get an action.
3. Call that action's argument parser, which builds the `Command`.

`Command` and the host handlers stay as they are. Handlers do not receive the
typed key, because they are shared by every transport. Only input parsing and
message rendering know about keys.

The three parsers (`bbs-plugin-api`, `bbs-mesh`, `bbs-meshtastic`) currently
repeat the same match. They collapse to prefix stripping plus one shared call.
MeshCore already delegates most keywords on this branch. Meshtastic does not
use the keymap yet, so it is wired in during this work.

### 3. Reading mode uses the table

Add a small `ReadingInput` enum (Forward, Reverse, Jump(id), ReverseTo(id),
Reply(text), Help, DeleteCurrent, Delete(id), Exit, Other). One function turns
reply text into a `ReadingInput` using the keymap. `handle_workflow_reply`
matches on the enum, so a valid key with an argument can no longer fall through
to "exit reading mode". This also carries the #410 and #415 fixes.

### 4. Messages name the active key

Add one small type, `KeyHints`, built from the active keymap:

```rust
hints.key(KeymapAction::ReadNew)      // "N" natively, "R" on a preset
hints.key_with(KeymapAction::ReadForward, "<id>")  // "F <id>"
```

Rules:

- It returns the primary key, upper-cased for display. If an action has no key
  (cannot happen after validation) it falls back to the native key.
- Actions that cannot be remapped (help, cancel, `.` to send, the admin
  commands) keep their fixed text, but still go through the same call so a
  future change is one edit.
- Message text becomes a format string with named holes, for example
  `"{fwd} - Forward  {rev} - Backward  {help} - Help  {exit} - Exit"`. No
  literal key letters inside user-facing text.

Places that must change (from the audit, roughly 45 sites):

| Where | What it says |
|---|---|
| `build_message_with_nav` | `R - Previous`, `F - Next`, `E - Reply` |
| Reading intro and end prompts (4 sites) | `F - Forward  R - Backward  H - Help  X - Exit` |
| `stopped_in_blocked_run` | `{key} - Keep going  H - Help  X - Exit` (already takes a key) |
| Scan and read paging hints | `type F <id>`, `type N again or F {id}`, `press K again` |
| Room change | `Type N to read.` |
| Exit and unknown | `Type H for help.` |
| Draft preview | `Type . to send, C to cancel` |
| Validation flow | `use V {user} first` |
| `HELP_*` constants (11) and `help_for_command` | every key in the help pages |
| Welcome and login text in `bbs-cli`, `bbs-mesh`, `bbs-meshtastic`, `bbs-plugin-api` | `Type 'H' for commands` |
| `render_notification` (MeshCore and Meshtastic) | `Reply 'M' to read` |
| `docs/USER_GUIDE.md` | notes that keys shown are the native ones |

Help constants are `const &str` today. They become functions that take
`&KeyHints`. `help_for_command` matches on the action, not on the typed word,
so `H l` on a preset explains ScanMessages.

Transports that render notifications need the keymap. `render_notification`
gains a `&KeyHints` argument, fetched per message the way the parse path
already fetches the active keymap, so a live preset switch takes effect at
once.

### 5. Keep messages inside the radio limit

MeshCore text is capped at 156 bytes. Multi-key or long keys make hints longer.
Extend the existing `help_strings_fit_mesh_payload` test to render every help
string under every built-in preset, plus a worst-case custom keymap, and fail
if any is over the limit.

## Phases

Each phase ends with the full gate from CLAUDE.md and a commit.

1. **Merge the parser fixes.** Merge `fix/command-parser-hardening` into this
   branch. Resolve the `s`/`search` conflict in favour of the fix.
2. **Data model.** Keymap with ordered keywords and primaries. `native()` built
   from it. Validation additions. Tests: primary survives layering, reserved
   words rejected, custom TOML still loads.
3. **Parse by action.** Per-action argument parsers, one shared entry point.
   Delete the native-keyword translation and the `native_keyword()` drift
   test, replaced by a table-driven test that every action parses under its own
   primary key. Collapse the three parsers.
4. **Reading input.** `ReadingInput` and its parser. Host matches on it.
   Tests for every reading action under native and one preset, including
   `R <id>`, `E <text>`, and a bad id.
5. **KeyHints.** Add the type and convert every site in the table above, one
   commit per area (reading, navigation, help, notifications, welcome text).
6. **Guard test.** A test that scans the sources for literal key hints in
   user-facing strings (for example `Type [A-Z]`, `[A-Z] - `) and fails on a
   new one. Allow-list only the fixed, non-remappable keys.
7. **Preset accuracy.** With the above in place, answer the four open
   questions and update the presets and the example file.
8. **Docs.** `CONFIG.md`, `CLI.md`, `USER_GUIDE.md`, the Settings page text,
   and the module docs describe the new behaviour.

## How we prove the key requirement

- Unit test per hint site: render under native and under a preset that moves
  the relevant action, and assert the preset's key appears and the native key
  does not.
- Integration test: switch the preset live mid-session and check the next
  reply uses the new keys.
- Radio-length test from section 5.
- Manual check on the WSL instance and a radio (cannot be done from the cloud
  session): register, read, scan, mail, under native and the Maximus preset.

## Risks

- Large diff across four crates. Mitigated by phase order: behaviour stays the
  same until phase 5, and native output is asserted byte for byte before and
  after.
- A key shown in a message that the user cannot actually type in that context
  (for example a reading-only key). `KeyHints` is action-based, and the guard
  test plus per-site tests cover it.
- Hints get longer on radio. Covered by the length test.

## Open questions (TJ)

1. Exclusive presets: should a preset drop the native keys it replaces, or keep
   them as aliases? Affects what "primary" means for help text and whether old
   habits still work.
2. Long-form aliases (`goto`, `list`): allowed in presets? They are just extra
   keywords in this design, so the cost is low.
3. Multi-key commands (`lm`, `jm`, `rm` from the #354 layout): allowed as
   keywords? Also low cost here. The only care is that a keyword may not be a
   prefix problem for arguments (`jm 3`).
4. Should the #354 layout ship as a built-in preset or only as an example file?
