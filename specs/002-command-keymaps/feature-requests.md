# Feature requests found while mapping classic BBS keys (GH #354)

Drafts for the maintainer. None of these are built. Each came from a key that
MaximusNG, legacy Maximus or another system has and this BBS does not. Presets
only change keys, so these need their own decisions. Suggested labels:
`enhancement`, `needs more information`.

## Forward a message to another user

**Where it comes from.** Legacy Maximus `F` ("Forward (copy)"), MaximusNG `F`
("Forward a Message", also in its reader), and mail forwarding in WWIV and
Synchronet.

**What it would do.** While reading a message (or by id at the prompt), send a
copy to another user's Mail: `F @user`.

**Open questions.**
- Who may forward? Forwarding a private message to a third party is a privacy
  decision. Options: room messages only, or the sender and recipient only for
  mail, or anyone with a warning.
- How is the copy marked? Suggest a `Fwd from <sender>:` line so the original
  author is always visible.
- Limits: a copy costs airtime for the recipient, so rate limits and a block
  list check (a blocked user must not receive forwards) are needed.
- Does it appear in the audit log?

**Acceptance.** A user can forward a room message to another user, the copy names
the original sender, a blocked recipient is refused, and tests cover mail and
room messages.

## Move a message to another room

**Where it comes from.** Legacy Maximus `H` ("Hurl"), MaximusNG reader `M`
(move), WWIV `M`. All are moderator or sysop tools. (PCBoard `M` means
"memorize", which is a different thing.)

**What it would do.** An aide or sysop moves a message from one room to another.

**Open questions.**
- Permissions: aide and above only, and not into a room the aide cannot read.
- What readers see: a moved message keeps its id, and the read pointers of users
  in the old and new rooms may skip or repeat it.
- Audit: record who moved what, from where to where.
- Should it leave a stub in the old room?

**Acceptance.** An aide can move a message, the audit log records it, read
pointers stay consistent, and a user cannot move messages.

## Edit your own message

**Where it comes from.** Legacy Maximus `C` ("Change current msg") and MaximusNG
reader `E` (edit).

**What it would do.** The author replaces the text of a message they posted.

**Open questions.**
- Who: the author only, perhaps with a time window, and sysops for moderation.
- Integrity: readers may already have seen the old text. Suggest an `(edited)`
  marker and keeping the time of the last edit.
- Radio cost: an edit means resending the whole body over the mesh. Editing in
  place is not realistic, so this is really "replace the text".
- It overlaps with delete and repost, which already works.
- It needs a new database column (a new migration), permission checks, an audit
  log entry, and backup and restore coverage.

**Acceptance.** An author can replace their own message text, the message shows
it was edited, the audit log records it, and old backups still restore.
