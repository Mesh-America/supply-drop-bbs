# Research: classic BBS command keys (sourced)

Full agent research pass, 2026-09-24. Condensed here; legend: **V** = verified against a primary
source (shipped manual, help file, menu file, or original source code). **S** = secondary
(Synchronet's compatibility clone of another system, written by Synchronet's own developers, not
the original authors — treat as lower confidence than V). **I** = inferred/unverified, flagged
explicitly rather than presented as fact.

## Headline findings

1. **The issue requester's own "Maximus" table is not stock Maximus.** Only `n`/`p` (next/previous)
   match. `g` in real Maximus is **Goodbye (logoff)** — remapping our `GoNextUnread` action to `g`
   under a literal reading of their table would be actively dangerous. `j` is "jump to *file*
   areas", not a message jump. `r` is Reply, not Read. `m` returns to the Main menu. None of their
   two-letter mail commands (`lm`, `jm`, `rm`, `nm`, `dm`, `cm`, `sm`, `xm`, `qm`) exist in stock
   Maximus — they're packet-radio BBS commands (see below), suggesting the requester was
   half-remembering two different systems.
2. **Wildcat!, RemoteAccess, and Renegade ship sysop-editable menus with no documented default
   letters.** Their manuals describe *function codes*, not key bindings. Any preset for these
   would be substantially invented, not sourced — the opposite of what this feature needs.
3. **For this project's actual audience (ham radio operators), the best-fitting "missing" system
   is packet-radio BBS software (F6FBB / BPQ / W0RLI-lineage)**, not on the original candidate
   list. Its command set maps close to 1:1 onto Supply Drop's existing room/Mail model, and BPQ
   nodes are still running today.

## Verified command tables (condensed)

Only the actions Supply Drop's own command set has an equivalent for; see each system's full
citation trail in the original research if the underlying manual/source needs re-checking.

### Maximus 3.0x — V unless noted (source: `github.com/sdudley/maximus`, `ctl/menus.ctl`,
`mec/hlp/*`, `mec/misc/browse.mec`)

| Supply Drop action | Maximus equivalent | Confidence |
|---|---|---|
| Quit | `G` (Goodbye) | V |
| Change room/area | `A` (then name/number) | V |
| List rooms | `A` then `?`, or bare `A` on first use | V |
| Next unread | *(no atomic equivalent — Browse `B` is the unread-aware path, but it's a sub-menu, not one key)* | — |
| Read new | `B` → `N`ew → `R`ead | V |
| Next / Previous (in reader) | `N` / `P` (also arrow keys) | V |
| Jump to message # | type the number directly | V |
| Scan/list | `L` ("List brief") | V |
| Post | `E` (Enter message) | V |
| Help | `?` | V |
| Read mail | `B` → `A`ll → `Y`our → Read; also offered at login | V |
| Send mail | `E` inside a private-capable area | V |
| Who's online | `W` | V |

Collision hazard confirmed: `M` means "Message areas" on the Main menu but "Main menu" on the
Message menu — Maximus itself is context-sensitive in a way a flat keymap can't fully replicate.

### Packet-radio BBS (F6FBB / BPQ family) — V (source: `f6fbb.org/fbbdoc/docbbs.htm`,
`cantab.net/.../BBSUserCommands.html`)

| Supply Drop action | Packet-BBS equivalent | Confidence |
|---|---|---|
| Read new | `L` (list/read new since last) | V |
| List mine (≈ Mail) | `LM` | V |
| Read a message # | `R n` | V |
| Read mine/new mine | `RM` / `RN` | V |
| Send private | `SP <call>` | V |
| Reply | `SR` | V |
| Delete | `K` / `KM` (kill) | V |
| Quit | `B` (Bye) | V |
| Help | `H` / `?` | V |

No room/area hierarchy the way Citadel has one — packet BBS is closer to "bulletins + personal
mailbox," so this preset fits Supply Drop's Mail room well but has no clean equivalent for
room-to-room navigation.

### PCBoard 15.x — V (source: PCBoard v15.22 Technical Reference Manual, reproduced at
`kuehlbox.wtf/wiki/commands:user:*`)

| Supply Drop action | PCBoard equivalent | Confidence |
|---|---|---|
| Goto conference | `J;<name-or-number>` | V |
| Read new | `R;S` (current), `R;S;A` (all) | V |
| Jump to message # | `R <n>` | V |
| Scan | `Q` (Quick scan) | V |
| Post | `E` | V |
| Logoff | `G` | V |
| Help | `H` | V |
| Read new mail | `Y` | V |
| Send mail | `E` with recipient security `R` | V |
| Who's online | `WHO` | V |

**Sharpest collision risk of all systems**: `Q` = quick scan (**not quit**), `M` = graphics mode
(**not mail**), `P` = page length (**not post**). A PCBoard preset that carelessly reused these
letters for Supply Drop's Quit/Mail/Post actions would be actively misleading to PCBoard veterans
— the opposite of the feature's goal.

### WWIV 5.x — V (source: `docs.wwivbbs.org`, `wwivbbs/wwiv` install files and source)

| Supply Drop action | WWIV equivalent | Confidence |
|---|---|---|
| List subs (rooms) | `*` | V |
| Goto sub | number, or `H` (hop by name) | V |
| Read new (all subs) | `N` | V |
| Next/Prev (reader) | Enter / `-` | V |
| Jump # | type number at read prompt | V |
| Scan | `S` | V |
| Post | `P` | V |
| Logoff | `O` | V |
| Help | `?` | V |
| Read mail | `M` | V |
| Send mail | `E` | V |
| Who's online | `//WHO` | V |

Telegard was originally built from WWIV source, and Renegade from Telegard — their reader prompts
are close enough that one "WWIV-family" preset, with a note, is more honest than three thin ones
(Renegade's own letters are unverifiable per point 2 above).

### Synchronet (current) — V (source: `gitlab.synchro.net/main/sbbs`, `exec/default.js`,
`text/menu/*`)

| Supply Drop action | Synchronet equivalent | Confidence |
|---|---|---|
| List sub-boards | `*` | V |
| Goto | number, or `J` | V |
| Read new | `N` | V |
| Next/Prev (reader) | Enter / `-` | V |
| Jump # | type number | V |
| Scan | `L` | V |
| Post | `P` | V |
| Logoff | `O` | V |
| Help | `?` | V |
| Mail (separate section) | `E` | V |
| Read new mail | `E` then `U` | V |
| Send mail | `E` then `S` | V |
| Who's online | `W` | V |

Still actively maintained — the one preset a modern user might genuinely be running today, not
just remembering from the 90s.

## Recommendation

**Maximus, Packet-BBS (FBB/BPQ), PCBoard, WWIV-family (covers Telegard/Renegade), Synchronet.**
Drop Wildcat! and RemoteAccess from the initial set — not because they're unimportant, but because
their default letters aren't documented anywhere verifiable, and shipping a "preset" that's
actually invented defeats the point of offering one. Both stay eligible for a later, explicitly
best-effort preset, or as a community-contributed custom keymap (see spec's upload requirement).
