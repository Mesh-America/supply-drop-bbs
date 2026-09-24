# Plan: Command keymaps

## Data model

```rust
/// One action a keymap can bind a key/word to. A closed set matching the
/// subset of Command::parse's keyword table (plus the reading-mode-only
/// actions) that presets actually have reason to remap — NOT full parity
/// with every Command variant. Sysop/aide-only actions are out of scope for
/// v1 (see Open question 5 in spec.md): remapping the moderation/admin
/// surface is higher-risk and no research found classic-system precedent
/// worth matching anyway.
pub enum KeymapAction {
    Quit, ListRooms, GoNextUnread, ChangeRoom, GoMail, ReadNew,
    ReadForward, ReadReverse, ScanMessages, EnterMessage, DeleteMessage,
    WhoIsOnline,
    // Reading-mode-only (see spec.md Constraints: parsed separately in
    // bbs-core's host.rs from WorkflowReply text, not through Command::parse):
    ReadingForward, ReadingJump, ReadingReverse, ReadingReply,
    ReadingDeleteCurrent, ReadingDeleteSpecific,
}

/// A partial override table: only the actions a keymap actually remaps.
/// Anything not listed keeps its Supply Drop native key. This is the core
/// design decision — see "Why partial, not total" below.
pub struct Keymap {
    pub name: String,
    pub description: String,
    pub bindings: BTreeMap<String, KeymapAction>, // lowercase keyword -> action
}
```

### Why partial, not total

An early draft assumed each preset would fully replace Supply Drop's command table. Working
through the Maximus preset in detail (below) showed why that's wrong: Maximus has no per-key
equivalent for several Supply Drop actions (there's no single-key "go to the next room with
unread messages" — Browse is a whole sub-menu, not one key), and forcing every action to have a
preset-authentic key produces either an invented key presented as if it were sourced (exactly the
mistake in the original issue report) or an unusable gap. A **partial override** — only remap what
the source system actually has a real equivalent for, leave the rest on Supply Drop's native key —
is both more honest and strictly less work per preset.

### Validation rule (collision detection)

Given a keymap's `bindings`, compute the **effective table**: start from Supply Drop's native
keyword→action map, then apply the keymap's overrides (a keymap entry replaces whatever native
keyword pointed at the same letter). A keymap is **rejected at load/activation time** if:

1. Two of the keymap's *own* entries bind the same keyword to different actions (a keymap authoring
   bug), or
2. Applying the keymap's overrides would leave any action — native or the keymap's own — with
   **zero** bound keywords (a preset "stealing" a letter must either accept the collision is
   intentional documented behavior, or explicitly rebind the action it's displacing).

Rule 2 is what makes preset authoring honest: you can't silently make `GoNextUnread` unreachable
by giving its letter to `Quit` without the validator catching it. See the Maximus preset below for
a worked example of resolving exactly this collision.

## Worked example 1: Maximus preset

Supply Drop native table (relevant subset): `q`/`quit`/`exit`/`bye`=Quit, `k`=ListRooms,
`g`=GoNextUnread, `c`=ChangeRoom, `m`=GoMail, `n`=ReadNew, `f`=ReadForward, `r`=ReadReverse,
`s`=ScanMessages, `e`=EnterMessage, `d`=DeleteMessage, `w`=WhoIsOnline.

| Action | Maximus letter | Collision with native | Resolution |
|---|---|---|---|
| Quit | `g` (Goodbye) | Native `g` = GoNextUnread | GoNextUnread is explicitly rebound (see below) — this is the collision Rule 2 exists to catch |
| ChangeRoom | `a` (change area) | none | direct override |
| GoNextUnread | *(displaced by Quit above)* | — | rebound to `]` — Maximus's "next area" key (not unread-aware in Maximus, an imperfect match, but preserves reachability and stays inside Maximus's own key vocabulary rather than inventing an unrelated letter) |
| ReadNew | *(not remapped)* | — | Maximus's nearest equivalent (`B`→`N`ew→`R`ead) is a 3-step sub-menu, not a single key; stays native `n` |
| WhoIsOnline | `w` | none — already native `w` | no-op override (documented anyway, so preset help text can say "confirmed, not changed") |
| ListRooms | *(not remapped)* | — | Maximus's `A` already covers "change room," and bare `A` also lists on first use, but that's ChangeRoom's key in this table — giving the SAME key to two Supply Drop actions is Rule 1's own collision. Left on native `k`; documented as an imperfect fit rather than forced. |
| DeleteMessage | *(not remapped)* | — | no single confirmed Maximus editor key maps cleanly (`C`=Continue, `A`=Abort in the editor, neither means "delete a posted message") |
| ScanMessages | `l` (List brief) | none — native has no `l` binding | direct override |
| EnterMessage | *(not remapped, `e` already matches)* | — | Maximus's `E` = Enter message; already Supply Drop's native key, so no override needed |

Final Maximus keymap: `{g: Quit, a: ChangeRoom, "]": GoNextUnread, l: ScanMessages, w: WhoIsOnline}`
— 5 entries, not a full 12-action remap. `ReadingForward`/`ReadingReverse` reading-mode keys stay
native `F`/`R` (Maximus's own `N`/`P` collide with nothing here since reading mode is a distinct
keyword namespace from top-level commands — but see Open question below).

## Worked example 2: Packet-BBS preset

Weaker fit — packet BBS has no room hierarchy, just bulletins + personal mail — so this preset is
deliberately thin:

| Action | Packet-BBS letter | Collision | Resolution |
|---|---|---|---|
| Quit | `b` (Bye) | none | direct override |
| ReadNew | `l` (list new since last) | none — but see Maximus preset above; presets don't share a namespace, each is independently validated | direct override |
| DeleteMessage | *(not remapped)* | `k` is packet-BBS's Kill, but native `k` = ListRooms | Rule 2 would require rebinding ListRooms too, for a very marginal gain (packet BBS's `K`/`KM` are natural, but forcing this collision resolution for one thin preset isn't worth it) — left native |

Final Packet-BBS keymap: `{b: Quit, l: ReadNew}`. Everything else — room navigation, mail — has no
confident single-key equivalent in the source system, so it stays native. The preset's description
text should say this plainly: "Packet-BBS conventions for logging off and checking new traffic;
Supply Drop's own keys cover everything else, since packet BBS has no room concept to map from."

## Not yet worked: PCBoard, WWIV-family, Synchronet

Scoped as follow-up tasks (see tasks.md) using the same validation model and the sourced tables in
`research-classic-bbs-commands.md`. Not rushed here for the reason stated in that document's
headline finding #1: an inaccurate preset is worse than no preset, and getting PCBoard specifically
wrong is easy (its `Q`/`M`/`P` mean nothing like Quit/Mail/Post).

## Architecture: where a keymap plugs in

Per spec.md's Constraints section, there are **four** places command text is parsed, and a keymap
must reach all of them:

1. Canonical `Command::parse` (`bbs-plugin-api`) — CLI, process transport, hello transport.
2. MeshCore's `parse_command` (`bbs-mesh`).
3. Meshtastic's copy (`bbs-meshtastic`).
4. Reading-mode's inline match in `bbs-core`'s `host.rs` (parses `WorkflowReply` text directly,
   bypassing 1–3 entirely).

Since `bbs-plugin-api` cannot depend on `bbs-core`, and the three transport parsers currently
hand-copy their keyword tables independently (already a known drift risk — see the
`sysop_words_match_canonical_parser` test's own doc comment), the keymap type itself should live in
`bbs-plugin-api` (it's pure data, no `bbs-core` dependency needed), and:

- `Command::parse` gains a `keymap: &Keymap` parameter (defaulting to a `Keymap::native()` const
  for every existing call site that doesn't care yet).
- The MeshCore and Meshtastic parsers should **stop hand-copying the keyword table** and instead
  delegate to the canonical parser with their transport-specific pre-processing (prefix stripping,
  one-shot login/register) layered around it — this closes the drift-risk gap the existing test
  only partially covers, as a side effect of adding keymap support, not as separate scope.
- Reading-mode's match in `host.rs` needs its own small keymap-aware lookup for the
  `ReadingForward`/`ReadingJump`/`ReadingReverse`/`ReadingReply`/`ReadingHelp`/
  `ReadingDeleteCurrent`/`ReadingDeleteSpecific` actions, since it never goes through
  `Command::parse` at all. `BbsHost` needs the active `Keymap` available where it handles
  `Workflow::Reading` — likely the same place `access_policy` (an existing live, sysop-settable,
  `RwLock`-guarded value on `BbsHost`) is threaded through, per spec.md's Constraints.

## Storage & activation

- New `[bbs]` config key: `keymap = "native" | "maximus" | "packet-bbs" | ...` (built-in preset
  name) — matches the existing `deny_unknown_fields` `BbsConfig` struct's pattern.
- Custom keymaps: a TOML file (matching `config.toml`'s own format), validated with the Rule
  1/Rule 2 checks above, stored under `data_dir` (see the just-landed `dir_perms` module — a
  custom keymap file is operator-uploaded content, same trust tier as a backup, so it gets the
  same owner-only treatment). `keymap = "custom:<filename>"` or a separate config key
  `custom_keymap_path`.
- Follows the `access_policy` precedent: held as `RwLock<Keymap>` on `BbsHost`, changeable live by
  a sysop action, persisted back to `config.toml` via the existing `config_lock` +
  `toml_edit` machinery (`persist_access_policy` is the direct model to copy).
- Revert to native: setting `keymap = "native"` (or an equivalent explicit sysop action) restores
  `Keymap::native()`, which is a hardcoded constant (Supply Drop's own current bindings) — not
  something that can be lost or corrupted by picking a preset, since presets never mutate it.

## Help text

Per spec.md's Constraints, help constants are already near the 156-byte mesh-reply budget. Rather
than growing every help string to show "native (preset)" pairs, generate the *quick reference*
line per-action from the active keymap at request time (`format!("{} - {}", key, action_label)`)
instead of hardcoding it into a `const`. Longer topic help can note "showing your board's active
keymap (<name>); type `HELP KEYMAP` for the full list" without needing per-preset const strings —
keeps every preset from needing its own hand-written, budget-checked help text.

## Open questions carried from spec.md, still unresolved

- Reading-mode key parity: should a preset's `ReadingForward` key also work as an alias in that
  same preset's top-level `ReadForward`, or are the two namespaces allowed to diverge? (Native
  Supply Drop already has them diverge slightly: top-level is `f`, reading-mode bare is `F` — same
  letter, but worth confirming intentional vs. incidental before presets add more divergence.)
- Upload surface (CLI vs. web) — still needs a maintainer call per spec.md Open question 3.
- Live vs. restart — this plan assumes live (following `access_policy`), but that assumption
  should be confirmed, not just inherited.
