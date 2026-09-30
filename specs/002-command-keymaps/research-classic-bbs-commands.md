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

## Phase 7 addendum (2026-09-30): full command pass

A second pass read each system's own menu, help and source files in full, to list
every command and to check the preset keys. Maximus, WWIV and Synchronet were read
from their source trees. PCBoard and packet BBS were read in a follow-up once the
cloud environment allowed `kuehlbox.wtf` and `f6fbb.org`/`cantab.net` (they were
blocked the first time, and the agents stopped rather than guess).

### How the keys were verified

- **Maximus** (`github.com/sdudley/maximus`, `ctl/menus.ctl`, `mec/hlp/*`,
  `docs/max_mast.txt` section 4.7.3, `max/*.c`). Rule, V: "Maximus will search the
  entire menu for a menu option that has a description starting with that key",
  so a hotkey is the first letter of the option's description unless an explicit
  key is given. A summarizer mis-stated several letters in an earlier attempt,
  so the raw files were read.
- **WWIV 5.x** (`github.com/wwivbbs/wwiv`, commit `56dfa23`: `main.mnu.json`,
  `mbmain.msg`, `msgscan.cpp`, `readmail.cpp`, `mmkey.cpp`).
- **PCBoard 15.x** (the `kuehlbox.wtf` wiki transcription of the manual, raw
  pages under `/wiki/_export/raw/commands:user:*`, 50 command pages; `r`, `k`,
  `j`, `g`, `h`, `m`, `p`, `q`, `y`, `v`, `who`, `x` and several others read in
  full). This is the manual text, not the shipped software.
- **Packet BBS** (`f6fbb.org/fbbdoc/docbbs.htm` for F6FBB and
  `cantab.net/.../BBSUserCommands.html` for BPQ). Command lists only; no prompts
  or sessions are shown. No W0RLI source was reached.
- **Synchronet** (the GitHub mirror of the GitLab tree, `exec/default.js`,
  `exec/email_sec.js`, `src/sbbs3/readmsgs.cpp`, `readmail.cpp`; the default
  shell only).

### What each system has that this BBS does not

| System | Commands | Reply given here |
|---|---|---|
| Maximus | F (forward a copy, or file areas), J (file areas), Y (yell), O (offline reader), T (tag areas), C (change setup), S (statistics), M (menu switch), / (chat), = (nonstop), - and + (thread), * (re-read) | one short message per key, see the preset |
| WWIV | T (transfer), . (doors), C (chat), A (automessage), G (gfiles), F (feedback), D (defaults), X (expert), L (last callers), I (info), J (conferences), Q (quick scan), E (email) | one short message per key |
| Synchronet | T (file section), X (externals), C (chat), Q (QWK), M (time bank), G (text files), I (info), F (find), S (messages to you), D (user config), A (auto-message), Z (continuous scan) | one short message per key |

### Traps that cannot be answered with a message

These source keys are also fixed words here, and a fixed word always runs as
ours, so the preset cannot reply to them:

- `B` is Browse in Maximus, Bypass sub in WWIV and Browse backward in Synchronet.
  Here `b` blocks a user.
- `V` is Version in Maximus, Voting in WWIV and Polls in Synchronet. Here `v`
  validates a user (aide and above).
- `U` is User list (Maximus, WWIV, Synchronet). Here `u` also lists users, so it
  matches.
- `H` is Hop to a sub in WWIV. Here `h` is help, and the preset uses the word `hop`.

### Verified key maps used by the presets

| Action | Maximus | WWIV | Synchronet |
|---|---|---|---|
| Quit | G | O | O |
| List rooms | none (A then ?) | * | * |
| Change room | A | none (H, blocked) | J |
| Next unread | none (] is next area) | none | none (N scans new) |
| Go to Mail | none | M | E (opens the e-mail menu) |
| Read new | N (inferred) | N | N |
| Read forward | none | none | R (inferred) |
| Read reverse | P | none | none (B is inferred) |
| Scan | L | S | L |
| Enter message | E | P | P |
| Delete | K | R (own posts only) | D at the reading prompt only |
| Who is online | W | //WHO | W |
| Reading: next | N (or Enter) | ] in the full-screen reader | + (or Enter) |
| Reading: previous | P | - | - |
| Reading: reply | R | W | A |
| Reading: help | ? | ? | ? |
| Reading: delete | K | D (sysop and moderators) | D |

Maximus has no separate reading mode (the Message menu keys are the reading keys)
and jumps to a message by typing its bare number. WWIV and Synchronet also jump by
typing a bare number; Synchronet's number is a position in the sub-board, not a
message id.

### PCBoard and packet BBS (verified in the follow-up pass)

| Action | PCBoard 15.x | Packet BBS (F6FBB and BPQ) |
|---|---|---|
| Quit | G (asks to confirm; `BYE` and `G;Y` do not) | `B` or `BYE` (here `b` is block user, `bye` is fixed and works) |
| List rooms | none (`SELECT` lists conferences with side effects) | none |
| Change room | J, as `J;13` or `J;NAME` | none |
| Next unread | none (`JUMP` only inside a multi-conference read) | none |
| Go to Mail | none (Y only scans your mail and gives counts) | LM lists mail to you |
| Read new | R;S (current conference), R;S;A (all) | RN (F6FBB), RM (BPQ) |
| Read forward | R;n+ from message n; bare R opens a sub-prompt | R n; bare R is undocumented |
| Read reverse | R;L (from the last message backward) | LR lists newest first (headers only) |
| Scan | Q (one header per line), Q L reverse | L (new headers), LL n (last n) |
| Enter message | E (prompts for the addressee) | S[type] call, SP call, SB |
| Delete | K n | K n |
| Who is online | WHO (multi-node systems only) | none (F6FBB: `%`, `JK`) |
| Reading: next | NEXT, Enter or R | none (pager only) |
| Reading: previous | PREV | none |
| Reading: reply | RE | SR |
| Reading: help | none documented | none |
| Reading: delete | K | K n |

Notes that matter for the presets:

- PCBoard's `E` takes an addressee (`E;name`), never body text, and `Y` is a scan
  that reports counts, not a mail room. PCBoard jumps to a message by typing its
  number, with `+` or `-` to set direction.
- Packet BBS bare `R`, `K` and `S` are undocumented; only `R msg#`, `K msg#` and
  `S[type] call` are. `Q` sets your QTH in BPQ, `N` sets your name, `W` lists
  files (F6FBB) and `V` is the version, so none of those are logoff, read new,
  who or validate. The F6FBB `KM` text contradicts itself, so it is not bound.
- Commands PCBoard has that this BBS does not: `M` graphics mode, `P` page length,
  `D`/`F`/`L`/`Z` file commands, `T` transfer protocol, `QWK`, `OPEN` (doors),
  `O` page operator, `CHAT`/`NODE`, `NEWS`, `S` questionnaires, `SELECT` and `A`
  conference flags, `X` expert mode, `MENU`, `LANG`, `ALIAS`, `C` comment to
  sysop, `TS` text search, `REPLY`. Each answers with a short message.
- Commands packet BBS has that this BBS does not: `SP`/`SB`/`SC` sending,
  forwarding (`@ BBS`), bulletin lists and categories (`LB`, `LC`), NTS traffic
  (`LT`), `LU`/`LN` unread lists, `NODE`/`NODES`/`C call`, white pages (`I`),
  file transfer (`FILES`, `YAPP`). Each answers with a short message.
- Traps that cannot be answered with a message because they are also fixed words
  here: PCBoard `B` (bulletins), `U` (upload) and `V` (view settings); packet
  `B` (bye, which `bye` covers), `U` (upload), `V` (version).

### Telegard and Renegade

Not covered. Telegard's read prompt shares several WWIV keys (`Enter`, number,
`-`, `R`, `T`, `B`, `Q`, `P`, `W`, `A`) but differs on `C`, `H` and `Z` and has no
find key. Renegade's keys are sysop-defined and its defaults could not be read.
The lineage (Renegade from Telegard from WWIV) was not verified. The
`wwiv-family` preset is therefore described as WWIV-style only.

### Still open

- PCBoard: the `E`, `D`, `F`, `U`, `L`, `Z`, `QWK`, `CHAT` and `TEST` pages were
  only read in part, and there is no documented help key at the end-of-message
  prompt. The wiki is a transcription of the manual; it was not checked against a
  shipped PCBoard or an archive.org copy.
- Packet BBS: what a bare `R` does, and how W0RLI differs, are not documented in
  the two pages read. The BPQ Quickstart guide was not found.
- Telegard and Renegade defaults, and the WWIV lineage claim.

### Why the #354 layout does not match the Maximus preset (2026-09-30)

The requester of GH #354 wrote that their layout comes from "my maximus bbs I have
still going to this date (For amiga software)". Two different programs are called
Maximus or Max:

- **Maximus** by Scott Dudley (DOS, OS/2, later Linux). This is what the `maximus`
  preset follows. Verified from its shipped `ctl/menus.ctl` and help files.
- **MAX's BBS** by Anthony Barrett (Amiga, 1989 to 1994; public domain release
  1.52). The Amiga is almost certainly what the requester runs. Its readme says
  it "can be completely customized: you can make your own menus". A web search
  summary for this question just repeated the issue text, so it was not used.

The public release `maxs152pd.lha` (from `software.bbsdocumentary.com`) was
unpacked and its shipped files read: `MenuFunctions.text`, the sample menus under
`BBS/Menus/` and the reading help under `BBS/Text/`. `MAXsBBS.manual` (168 KB) was
not read. What the shipped samples show:

- A menu is a text file whose option letters are chosen by the sysop. A separate
  numbered function table (1 log out, 2 go to menu, 16 leave a message, 17 read
  messages, 32 send NetMail, 38 who is online, and so on) says what each option
  does. The keys are therefore not fixed by the software.
- Shipped sample menus: main `L` local boards, `E` echo boards, `F` files, `N`
  news, `U` user menu, `V` time bank, `W` who, `I` internode chat, `C` call the
  sysop, `B` bye; a message section `R` read, `L` leave a message, `P` previous
  menu, `Q` quit to main; private EMAIL `R`, `L`, `M`, `S`; read options `N` new,
  `F` forward, `R` reverse, `T` threads, `I` individual, `S` search, `M` marked,
  `A` abort, `H` help.
- None of that matches the requester's table (`l` list rooms, `g` goto, `j` jump,
  `m` list messages, `n` and `p`, `w` write, `lm`, `jm`, `rm`, `dm`, `nm`, `cm`,
  `sm`, `xm`, `qm`). It is most likely the requester's own customised menu set, on
  a version that may be newer than 1.52 (there are 1.54 and "MAXsPRO" releases).

So there is no verifiable "Amiga Maximus default" to build a preset from. The
requester's layout is kept as the worked example
`contrib/keymaps/issue-354-layout.toml`, and the `maximus` preset says in its
description that it is Scott Dudley's Maximus and not MAX's BBS.

### Legacy Maximus and MaximusNG are separate presets (2026-09-30)

- **Legacy Maximus** (`github.com/sdudley/maximus`, `ctl/menus.ctl`) and the legacy
  menu file in MaximusNG
  (`MaximusNG-BBS/maximus`, `resources/config/legacy/menus.ctl`) define the same
  options; only file paths and the case of menu names differ, and the MEX sample
  menu has one changed line. Preset: `maximus-legacy`.
- **MaximusNG 4.0** replaced `menus.ctl` with TOML menus
  (`resources/config/menus/message.toml` and `main.toml`; `message.maxng.toml` is an
  optional theme with the same options). They differ from legacy. Preset:
  `maximus-ng`. The hotkey rule is unchanged (first letter of `description`;
  `key_poke` is type-ahead, not a hotkey), but every matching option runs in file
  order, so `M` on the message menu runs "Mail a User" and then "Main menu".

| Action | Legacy Maximus | MaximusNG 4.0 |
|---|---|---|
| Quit | G | G (message menu), L (main menu "Log Off") |
| Change room | A, `[` `]` | A, `[` `]` |
| Read new | N (next message) | R then N (the reader's prompt; type-ahead not verified) |
| Read forward | digits (read by number) | R then F (asks for a message number) |
| Read reverse | P | none |
| Scan | L (List brief) | L (List Messages, then a pause) |
| Enter | E | W (Write a Message) |
| Delete | K (prompts for a number) | none on the menu; D or K inside the reader |
| Go to Mail | none | C (Check Email); M is "Mail a User" |
| Who is online | W (main menu) | W (main menu; W is Write on the message menu) |
| Reading: next / previous | N / P | N / P (or the arrow keys) inside the reader |
| Reading: reply | R | R inside the reader (E edits) |
| Reading: delete | K | D or K inside the reader, with a yes/no confirm |
| Reading: help | ? | ? |

Legacy keys removed from the NG message menu: `N`, `P`, `E`, `R` (reply), `K`, `C`
(change current), the digits, `=`, `-`, `+`, `*`, and the sysop options. Added: `W`,
`R` (read), `S` (search), `C` (check e-mail), `M` (mail a user). Not verified for
NG: what `Msg_Area`, `Msg_List`, `Email_Compose` and `Msg_NG_Find` prompt for, the
reader's help screen, and the classic browse reader's per-message keys.

