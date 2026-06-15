# Workflows

A **workflow** is a multi-step, stateful conversation that keeps the session
in an interactive mode between commands.  Instead of requiring the user to
type a full command on every message (expensive on a lossy radio link),
workflows let them respond with a single character: `F`, `R`, `V`, `1`, etc.

---

## How workflows work

### Transport side

Each transport tracks a per-peer `awaiting_reply` flag:

```
user sends "F"
  → transport checks awaiting_reply
  → if true  → Command::WorkflowReply { reply: "F" }
  → if false → parse normally (Command::ReadForward { after: None })
```

`awaiting_reply` is set to `true` whenever the host returns
`Response::Prompt`.  It is cleared when the host returns any other variant
(`Text`, `Error`, `LoggedOut`, etc.).

This means: **return `Response::Prompt` to stay in a workflow; return
`Response::Text` or `Response::Error` to exit it.**

### Host side

Per-session workflow state lives in `SessionRecord::workflow` (an enum,
`crates/bbs-core/src/host.rs`).  `handle_workflow_reply` dispatches on the
current variant.

```
WorkflowReply { reply }
  → read workflow from session (clone, non-destructive)
  → match workflow { ... }
     Workflow::None      → error "no active workflow"
     Workflow::Login     → check password
     Workflow::Reading   → navigate F/R/E
     Workflow::Rooms     → jump to numbered room
     ...
```

The handler is responsible for:
1. Performing the action (validate user, fetch next message, …).
2. Either advancing the workflow state in the session **or** resetting it to
   `Workflow::None`.
3. Returning `Response::Prompt` to keep `awaiting_reply = true`, or another
   variant to release the user back to normal command mode.

### Cancellation

Any workflow (including new ones) can be cancelled by the user typing
`CANCEL` or `STOP`.  `handle_cancel` resets `workflow` to `None` and returns
`Response::Text("Cancelled.")` — no special handling needed per workflow.

### Room changes

`set_current_room` resets both `current_message_id` and `workflow` to their
defaults whenever the user moves to a different room.  This prevents stale
workflow state from carrying across rooms.

---

## Implementing a new workflow

1. **Add a variant to `Workflow`** in `host.rs`.  Store whatever state the
   handler needs (e.g. a list of items, a cursor, counts).  All fields must
   implement `Clone + Debug`.

2. **Enter the workflow** from the command handler that starts it (e.g.
   `handle_list_rooms`):
   - Write the new variant into `session.workflow`.
   - Return `Response::Prompt { text: "…", hide_input: false }`.

3. **Handle replies** by adding a match arm in `handle_workflow_reply`:
   - Read the cloned workflow (already done at the top of the function).
   - Perform the action.
   - Advance state or reset to `Workflow::None`.
   - Return `Response::Prompt` to continue, or `Response::Text` / `Response::Error`
     to exit.

4. **Keep frames small.**  MeshCore radio frames cap at 156 bytes of text.
   Prompts combining a result + nav hint must stay under that limit.  Run
   `cargo test host::tests::help_strings_fit_mesh_payload` as a reference for
   how byte-counts are checked.

5. **Add a test** in the `host::tests` module that drives the full
   workflow via `process_command` calls.

---

## Current workflows

### `Workflow::Register`

**Trigger:** `REGISTER <username>`  
**Purpose:** Create a new account.

| Stage | Prompt shown | Expected reply |
|---|---|---|
| `DisplayName` | "Enter your display name (or . to skip):" | Any text or `.` |
| `Password { display_name }` | "Choose a password (min 8 chars):" | Password text |
| `Confirm { display_name, password }` | "Confirm password:" | Same password again |

On success: account created, user is logged in, workflow set to `None`.

---

### `Workflow::Login`

**Trigger:** `LOGIN <username>`  
**Purpose:** Authenticate an existing account.

| Stage | Prompt shown | Expected reply |
|---|---|---|
| *(single stage)* | "Password:" | Password text |

Up to 3 failed attempts; after the third the workflow exits with an error.

---

### `Workflow::Compose`

**Trigger:** `E` (bare), `E @recipient body`, or reply from reading mode.  
**Purpose:** Draft and confirm a new message.

| Stage | Prompt shown | Expected reply |
|---|---|---|
| `AwaitingRecipient` | "Recipient username:" | Username string |
| `AwaitingBody { recipient }` | "Enter message:" | Message body |
| `AwaitingConfirmation { recipient, body }` | Draft preview + "Type . to send" | `.` to confirm, anything else re-prompts |

For room posts the recipient is `None`; for Mail DMs it is `Some(username)`.

---

### `Workflow::EditProfile`

**Trigger:** `PROFILE`  
**Purpose:** Update the user's display name.

Single-stage: prompts "New display name (- to clear, blank = no change):",
validates the input, saves, then exits.

---

### `Workflow::ChangePassword`

**Trigger:** `PASSWD`  
**Purpose:** Change the authenticated user's password.

| Stage | Prompt shown | Expected reply |
|---|---|---|
| `VerifyOld { attempts }` | "Current password:" | Existing password |
| `EnterNew` | "New password (min 8 chars):" | New password |
| `ConfirmNew { new_password }` | "Confirm new password:" | Same new password |

Up to 3 failed attempts on `VerifyOld` before the workflow is cancelled.

---

### `Workflow::Reading`

**Trigger:** First `F` or `R` command in a room.  
**Purpose:** Browse messages one-at-a-time without re-typing the command prefix.

**Entry:** Shows `[Room — Reading] / N message(s) / F - Forward  R - Backward  H - Help  X - Exit`.

| Reply | Action |
|---|---|
| `F` | Fetch next message, advance cursor, stay in workflow |
| `R` | Fetch previous message, move cursor back, stay in workflow |
| `E` | Start `Workflow::Compose` replying to current message sender (Mail) or posting to current room |
| `H` / `?` | Show contextual reading-mode help, stay in workflow |
| Anything else | Reset cursor and workflow, return to normal mode |

Each message frame ends with a nav hint line:
```
R - Previous  F - Next  E - Reply
```
(only `R` / `F` are shown when the corresponding neighbour exists; `E - Reply`
is always present.)

Room changes automatically exit reading mode (`set_current_room` resets
`workflow` to `None`).

---

### `Workflow::Rooms`

**Trigger:** `K` (list rooms).  
**Purpose:** Let the user jump to a room by typing its list number instead of
its full name.

**Entry:** Displays a numbered room list:
```
Rooms:
1.  Lobby (3 new) [here]
2.* Tech
3.  Off-Topic
Enter # to join, X to cancel
```

| Reply | Action |
|---|---|
| `1`–`N` | Change to the room at that position, exit workflow |
| Any room name | Change to that room by name (falls back to `handle_change_room`), exit workflow |
| `X` or blank | Cancel, exit workflow |

---

### `Workflow::ReviewPending`

**Trigger:** `LP` (list pending) when there are unvalidated accounts.  
**Purpose:** Step through a queue of unvalidated user accounts one at a time
so a sysop/aide can act on each without retyping names.

**Entry:** Shows the first account:
```
#1 of 3: alice  — V Validate  S Skip  B Ban  X Exit
```

| Reply | Action |
|---|---|
| `V` | Validate (promote to User tier), advance to next |
| `S` | Skip (no change), advance to next |
| `B` | Ban the account, advance to next |
| `X` | Exit queue immediately |
| Anything else | Re-show the current account prompt |

After the last account the workflow exits with "Review complete — no more
pending accounts."

---

## Design guidelines

- **One prompt per action.**  Avoid combining two questions in one frame; the
  user can only reply once.
- **Always show exit.**  Every prompt should include `X` or `CANCEL` in the
  hint text so users know how to escape.
- **Fail gracefully.**  If a workflow step errors (e.g. the user to validate
  was just deleted), log the error and advance the queue rather than
  terminating the whole workflow.
- **Keep state minimal.**  Store only what the next reply needs to resolve the
  action (IDs, indices, staged text).  Do not duplicate data already in the
  database.
- **Radio-first sizing.**  The combined `format_message(msg) + nav_hint` must
  stay under 156 bytes.  Short, dense prompts beat verbose ones on LoRa.
