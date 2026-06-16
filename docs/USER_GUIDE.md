# Supply Drop BBS — User Guide

Welcome to Supply Drop BBS. This guide covers everything from creating your
account to sending mail, navigating rooms, and (for operators) managing the
system.

---

## Table of contents

1. [What is Supply Drop BBS?](#1-what-is-supply-drop-bbs)
2. [Connecting](#2-connecting)
3. [Creating an account](#3-creating-an-account)
4. [Logging in and out](#4-logging-in-and-out)
5. [Your account status](#5-your-account-status)
6. [Getting help](#6-getting-help)
7. [Rooms](#7-rooms)
8. [Reading messages](#8-reading-messages)
9. [Writing messages](#9-writing-messages)
10. [Mail (private messages)](#10-mail-private-messages)
11. [Who's online](#11-whos-online)
12. [Your profile](#12-your-profile)
13. [Blocking users](#13-blocking-users)
14. [System administration](#14-system-administration)
15. [Password recovery](#15-password-recovery)
16. [Quick-reference card](#16-quick-reference-card)

---

## 1. What is Supply Drop BBS?

Supply Drop BBS is a text-based bulletin board system designed for **digital
radio networks** (LoRa mesh, Meshtastic, APRS) as well as conventional
internet connections. The same BBS can accept users from multiple radio
transports simultaneously — a MeshCore user and a Meshtastic user can be in
the same room and send each other direct messages.

Because radio frames are small, commands are kept short: single letters where
possible. Every command that works on radio also works over a network
connection.

---

## 2. Connecting

How you connect depends on how the BBS operator has set things up.

| Method | What you type / do |
|---|---|
| **MeshCore radio** | Send a direct message to the BBS node |
| **Meshtastic radio** | Send a DM to the BBS node |
| **Unix socket / CLI** | `nc -U /var/run/supply-drop-bbs/cli.sock` |
| **SSH / Telnet** | Address and port provided by your operator |

Once connected you'll see a short welcome banner and the anonymous help menu:

```
REGISTER <user>  create an account
LOGIN <user>     log in to your account
Q  quit
H  help
```

---

## 3. Creating an account

Type `REGISTER` followed by your desired username:

```
REGISTER alice
```

**Username rules**

- Letters, digits, hyphens, and underscores only
- 3–32 characters
- Case-insensitive at login (stored exactly as typed)
- The names `bbs` and `system` are reserved and cannot be registered

The BBS will walk you through three prompts:

```
Choose a display name (or send - to use your username):
> Alice Wonderland

Choose a password (min 8 characters):
> ••••••••

Confirm your password:
> ••••••••
```

**Display name** is optional. It is the name shown alongside your messages.
Send `-` to use your username as-is (on a mesh radio you can't send an empty
message; on the CLI/web you can also just press Enter). You can change it later
with `PROFILE`.

**Password** must be at least 8 characters. It is stored as a hashed value —
the BBS never stores your password in plain text.

After confirmation you are immediately logged in:

```
Welcome, alice. Type 'H' for commands.
```

### First user is automatically sysop

The very first account created on a fresh BBS is automatically given **Sysop**
(full administrator) permission. Every subsequent account starts as
**Unvalidated** and must be approved by an Aide or Sysop before it can post
messages or read rooms.

---

## 4. Logging in and out

### Logging in

```
LOGIN alice
```

You'll be prompted for your password (input is hidden on supporting clients):

```
Enter your password:
> ••••••••
```

After three wrong attempts the login workflow is cancelled and you must type
`LOGIN <username>` again to retry.

### Logging out

```
Q
```

or

```
LOGOUT
```

Your session ends immediately. Any active workflow (composing a message, etc.)
is discarded.

### Cancelling a workflow mid-way

If you're in the middle of a prompt sequence and want to back out without
logging out:

```
CANCEL
```

or

```
STOP
```

This works at any point in any workflow.

---

## 5. Your account status

### Checking who you are

```
WHOAMI
```

Output:

```
Logged in as alice (user). Current room: Lobby
```

### Account levels

| Level | What you can do |
|---|---|
| **Unvalidated** | Log in, check `WHOAMI`, read help, log out. Cannot post or read rooms. |
| **User** | Full access to rooms, messages, and mail |
| **Aide** | Everything a User can do, plus: approve/reject new users, ban accounts, edit rooms |
| **Sysop** | Everything an Aide can do, plus: unban accounts, create/delete rooms |

### Unvalidated accounts

After registration you'll see this message if you try a room command:

```
Your account is pending validation by an aide.
Type H for help, WHOAMI to see your status, or Q to log out.
```

This is normal. A sysop or aide has been notified automatically. Once they
approve your account you can start using the BBS without logging out — your
session is upgraded in place.

---

## 6. Getting help

```
H
```

Shows the quick command list for your current access level.

### Help topics

`H all` lists the help topics. Each topic has a single-letter shortcut and a
full-word form — both work (e.g. `H M` is the same as `H mail`):

```
H M  mail      — private messaging
H R  reading   — reading messages in a room (incl. .FF fast-forward)
H P  posting   — writing and deleting messages
H U  users     — finding and listing user accounts
H N  nav       — navigating between rooms
H A  acct      — your profile and password
H aide         — moderation commands (Aide+)
H sysop        — administration commands (Sysop+)
```

### Help on a specific command

```
H N          — explain the N (read new) command
H M          — explain the M (mail) command
H BAN        — explain the BAN command
```

---

## 7. Rooms

Rooms are the public spaces of the BBS. Think of them like channels or
bulletin boards — everyone who can access a room sees the same messages.

### Built-in rooms

Every Supply Drop BBS ships with five permanent rooms that cannot be deleted
or reconfigured by the sysop:

| Room | Who can access | Purpose |
|------|---------------|---------|
| **Lobby** | Everyone (Unvalidated and up) | General public space |
| **Mail** | Users and up | Private one-on-one messages — see [section 10](#10-mail-private-messages) |
| **Aides** | Aides and Sysops only | Moderator coordination |
| **Sysop** | Sysops only | System operator space |
| **System** | Sysops only (read-only for others) | System announcements |

Sysops can create additional rooms and configure their minimum access level.
Your room list (`K`) only shows rooms you have access to.

### List rooms

```
K
```

Output shows each room with an unread-message indicator:

```
Rooms:
* General (3 new) [here]
  Announcements
* Tech Talk (1 new)
  Off Topic
```

- `*` means there are unread messages
- `[here]` marks your current room
- `(3 new)` shows the unread count

### Change room

```
C General
```

or by room number:

```
C 3
```

Room names are **case-insensitive**.

### Jump to next room with unread messages

```
G
```

Moves you to the next room (in list order) that has messages you haven't
read. Wraps around. Useful for quickly working through activity across all
rooms.

### Skip past unread messages without reading them

```
.FF
```

Fast-forward: marks everything in the current room as read without actually
reading the messages. Useful when you've been away and just want a clean slate
in a busy room.

### Go to Mail

```
M
```

Switches you into your private Mail room. See [section 10](#10-mail-private-messages).

---

## 8. Reading messages

All reading commands work in whichever room you are currently in. Switch rooms
with `C` first, then read.

### Read new messages

```
N
```

Shows the messages you haven't read yet, oldest first, up to five at a time.
Type `N` again to continue to the next batch.

```
[General — new messages]
#12 alice: Has anyone tried the new firmware?
#13 bob: Yes! Much better range on the 915 MHz band.
#14 carol: Same here, went from 2 km to almost 4 km.
(more — type N again or F 14 to continue)
```

### Read forward (oldest first)

```
F
```

Reads from the very beginning of the room. To start from a specific message:

```
F 42
```

Reads messages with IDs higher than 42.

### Read newest first

```
R
```

Shows the five most recent messages, newest at the top. Good for catching up
quickly.

### Scan message headers

```
S
```

Shows a one-line summary for each recent message — ID, sender, and the first
40 characters of the message. No body text is shown.

```
[General — scan]
#12 alice: Has anyone tried the new firmware?
#13 bob: Yes! Much better range on the 915 MHz ba…
#14 carol: Same here, went from 2 km to almost 4 …
(more — type F <id> to read from a message)
```

Use `F <id>` to jump to a specific message after scanning.

### Message format

Each message shows:

```
#<id> <sender>: <body>
```

For direct mail (when you're in the Mail room):

```
#<id> [DM→<recipient>] <sender>: <body>
```

---

## 9. Writing messages

### Write a message to the current room

**Inline (recommended on radio links):**

```
E Has anyone tried the new firmware?
```

The BBS echoes your draft and waits for confirmation:

```
Has anyone tried the new firmware?
Type . to send
```

Send a lone `.` to post it:

```
.
Message posted.
```

If the confirmation prompt is lost in transit, just send `.` again — the draft
is preserved and will not be double-posted.

**Prompt flow (alternative):**

```
E
```

```
Enter your message for General:
> Has anyone tried the new firmware?
Message posted.
```

The inline form is preferred on LoRa and other lossy links because the
confirmation step makes the send idempotent: if "Message posted." never arrives,
sending `.` safely retries without creating a duplicate.

### Delete a message

```
D <id>
```

Example:

```
D 14
```

- You can delete **your own messages** in any room
- **Aides and Sysops** can delete anyone's messages

---

## 10. Mail (private messages)

Mail is the BBS private-message system. Messages go directly to one recipient
and are only visible to the sender and that recipient — no one else, including
sysops and aides, can read your mail through the BBS interface.

> **System notifications** — the BBS itself occasionally sends you mail from
> the username `bbs` (for example, when your account is validated). These are
> one-way notifications; you cannot reply to `bbs`.

### Go to Mail

```
M
```

This switches your current room to your Mail inbox. All the reading commands
(`N`, `F`, `R`, `S`) now operate on your mail.

### Write a new mail

**Inline (recommended on radio links):**

```
E bob Hi Bob, did you get the antenna parts?
```

You can also prefix the username with `@`:

```
E @bob Hi Bob, did you get the antenna parts?
```

The BBS echoes the draft for confirmation:

```
To bob: Hi Bob, did you get the antenna parts?
Type . to send
```

Send `.` to post. If the confirmation prompt is lost in transit, sending `.`
again is safe — the draft is preserved and will not be double-posted.

**Prompt flow (alternative):**

```
E
```

```
Enter recipient username:
> bob
Enter your message:
> Hi Bob, did you get the antenna parts?
Message posted.
```

**The recipient does not have to be online.** They'll see it the next time
they check their mail.

### Read new mail

```
N
```

Shows mail you haven't read yet.

### Browse all your mail

```
F      — forward (oldest first)
R      — reverse (newest first)
S      — scan headers
```

### Delete a mail message

```
D <id>
```

Either the sender or the recipient can delete a mail message.

### Mail notifications

When you log in and have unread mail, the BBS will notify you. On radio
transports you may receive an unsolicited push notification:

```
You have 2 unread messages. Reply 'mail' to read.
```

---

## 11. Who's online

```
W
```

Lists everyone currently connected and their permission level:

```
Online (3 users):
alice [sysop]
bob [user]
carol [user]
(+1 unauthenticated)
```

The `(+n unauthenticated)` line shows sessions that have connected but not
yet logged in or registered.

---

## 12. Your profile

### Change your display name

```
PROFILE
```

Your display name is shown next to your messages. It can be anything up to
the system limit. To remove your display name and show only your username:

```
Enter your new display name (- to clear, CANCEL to abort):
> -
```

Send `-` to clear your display name, type a new one to change it, or send
`CANCEL` to leave it unchanged. (On the CLI/web an empty line also leaves it
unchanged, but mesh radios can't send an empty message — use `CANCEL`.)

### Change your password

```
PASSWD
```

The BBS will walk you through three steps:

```
Current password:
> ••••••••

New password (min 8 characters):
> ••••••••

Confirm new password:
> ••••••••

Password changed successfully.
```

- You have three attempts to enter your current password correctly. After
  three failures the workflow is cancelled for security.
- The new password must be at least 8 characters.
- If the confirmation doesn't match, you are asked to enter the new password
  again (not starting over from the current-password check).

---

## 13. Blocking users

If another user's messages are unwelcome, you can block them. Blocked users'
messages are hidden from your view — they do not know they are blocked, and
their messages still appear to everyone else.

### Toggle block (block if not blocked, unblock if blocked)

```
B <username>
```

### Force block

```
B +<username>
```

### Force unblock

```
B -<username>
```

Blocking is per-session-and-database — it persists across logins.

---

## 14. System administration

These commands are available to **Aides** and **Sysops** only. Use `H aide`
or `H sysop` from within a session to see the current list.

### Viewing pending registrations

New users start as **Unvalidated**. When someone registers, the BBS
automatically sends a direct mail message from `bbs` to every active sysop,
so you know to check. The new user does **not** see this notification.

To list all accounts waiting for approval:

```
PENDING
```

Output:

```
Pending validation (2):
  newuser1 (joined 2026-05-09)
  newuser2 (joined 2026-05-09)
Use V <username> to validate, B <username> to ban.
```

You can also check from the **command line** (without starting the BBS):

```bash
supply-drop-bbs user list --pending
```

### Validating (approving) a user

```
V <username>
```

The account is immediately promoted to **User** level. If they are currently
logged in, their session is upgraded without requiring a re-login.

From the command line:

```bash
supply-drop-bbs user verify alice
```

### Listing all users (CLI)

```bash
supply-drop-bbs user list
```

Output:

```
username             level        status       created
--------------------------------------------------------
alice                sysop        active       2026-05-01
bob                  user         active       2026-05-03
newuser1             unvalidated  active       2026-05-09
```

### Banning a user (Aide+)

```
BAN <username>
```

Sets the account to **Banned** status. The user is immediately disconnected
from all active sessions. They cannot log in again until unbanned.

```
'newuser2' has been banned.
```

Notes:

- An Aide cannot ban another Aide or a Sysop
- Banning is logged in the audit trail

### Unbanning a user (Sysop only)

```
UNBAN <username>
```

Restores the account to **Active** status. The user can log in again.

### Creating a room (Sysop only)

From within a session:

```
.C Ham Radio
```

From the command line:

```bash
supply-drop-bbs room create "Ham Radio"
supply-drop-bbs room create "Net Control" --description "Weekly net check-ins"
```

### Deleting a room (Sysop only)

```
.DR Ham Radio
```

The five built-in rooms (Lobby, Mail, Aides, Sysop, System) cannot be deleted.

### Resetting a user's password (Sysop only)

```
.PW <username>
```

Generates a single-use **temporary password** for the account and returns it to
you — you never type a real password over the air:

```
Temporary password for 'alice': Kp7mQ2rtVx9d
They must change it at next login. Share it securely — it is visible on-air.
```

Convey that temporary password to the user out-of-band. The next time they log
in with it, the BBS forces them to choose a new password before the session
completes; the temporary password then stops working.

> **On-air note.** The temporary password is shown in your session, which on a
> radio link is itself visible over the air — it is *single-use* and must be
> changed immediately, which limits exposure, but treat it as sensitive. The
> equivalent CLI command (`supply-drop-bbs user set-password`, section 15) sets
> a password directly without going over the air.

### Web admin panel

If the web admin is enabled, sysops can manage users, rooms, messages, and
view audit logs through a browser interface. The URL is set by the operator
(typically `http://<bbs-host>:8080`).

From the **Users** page you can:

- Filter to **"pending verification"** to see only unvalidated accounts
- Click **verify** to approve an account
- Click **ban** / **unban** to manage banned users

An orange **"N pending"** badge appears in the page header whenever there are
accounts waiting for approval.

---

## 15. Password recovery

> **Supply Drop BBS is a radio-first system.** There is no email-based
> password reset. Recovery requires a sysop or physical access to the server.

### Option 1 — Contact the sysop

Tell the sysop your username. They can reset your password in three ways,
from most to least convenient:

1. **In-session** — while connected to the BBS, the sysop types `.PW <username>`; the BBS returns a single-use temporary password for them to pass to you, and you set your own password at next login. No server access needed.
2. **Web admin** — log into the **web admin panel** → Users → find your account → reset password.
3. **CLI** — on the server, run:
   ```bash
   supply-drop-bbs user set-password <username> \
     --config /etc/supply-drop-bbs/config.toml
   ```

### Option 2 — Delete and re-register

If you have no messages or content worth keeping, the sysop can delete your
account and you can re-register with the same username.

### Protecting yourself

- Use a **memorable passphrase** rather than a complex random password — you
  may be typing on a phone keyboard
- Note your password somewhere secure before you need it
- If your password may have been compromised, use `PASSWD` to change it
  immediately

---

## 16. Quick-reference card

### Anyone (before login)

| Command | Action |
|---|---|
| `REGISTER <user>` | Create an account |
| `LOGIN <user>` | Log in |
| `H` | Help |
| `Q` | Quit |

### Logged in — navigation

| Command | Action |
|---|---|
| `K` | List rooms |
| `C <name>` | Change to room by name (case-insensitive) |
| `C <number>` | Change to room by number |
| `G` | Jump to next room with unread messages |
| `M` | Go to Mail (private messages) |
| `.FF` | Fast-forward past unread (mark all read) |

### Logged in — reading

| Command | Action |
|---|---|
| `N` | Read new messages (5 at a time) |
| `F` | Forward read from the beginning |
| `F <id>` | Forward read starting after message #id |
| `R` | Reverse read (newest first) |
| `S` | Scan message headers |

### Logged in — writing

| Command | Action |
|---|---|
| `E` | Write a message (prompt flow) |
| `E <text>` | Stage inline message — send `.` to confirm |
| `E <user> <text>` | Stage inline mail — send `.` to confirm (when in Mail) |
| `.` | Confirm and post a staged draft |
| `D <id>` | Delete message #id |

### Logged in — account

| Command | Action |
|---|---|
| `WHOAMI` | Show your username and level |
| `W` | Who's online |
| `PROFILE` | Edit your display name |
| `PASSWD` | Change your password |
| `B <user>` | Block / unblock a user (toggle) |
| `B +<user>` | Force block |
| `B -<user>` | Force unblock |
| `CANCEL` | Cancel the current workflow |
| `Q` | Log out |

### Help topics

| Command | Shows |
|---|---|
| `H` | Quick command list |
| `H all` | List of help topics |
| `H M` / `H mail` | Mail / private message commands |
| `H R` / `H reading` | All reading commands (incl. `.FF`) |
| `H P` / `H posting` | Writing and deleting |
| `H U` / `H users` | Finding and listing user accounts |
| `H N` / `H nav` | Room navigation |
| `H A` / `H acct` | Profile and password |
| `H <cmd>` | Detail on one command (e.g. `H N`) |

### Aide commands

| Command | Action |
|---|---|
| `PENDING` | List unvalidated accounts |
| `V <user>` | Validate (approve) an account |
| `BAN <user>` | Ban a user |

### Sysop commands

| Command | Action |
|---|---|
| `UNBAN <user>` | Lift a ban |
| `.C <name>` | Create a new room |
| `.DR <name>` | Delete a room |
| `.PW <user>` | Reset a user to a single-use temp password |

---

*Supply Drop BBS is an open-source project by [Mesh America](http://meshamerica.com).*
