# What is Supply Drop BBS?

Supply Drop BBS is a community bulletin board for LoRa mesh radio networks.
If you have a radio that can reach a BBS node, you can post messages to public
rooms, send private mail to other users, and chat with people across the mesh —
no internet required.

Think of it like a small-town notice board crossed with a group chat, running
entirely over radio.

---

## Who is it for?

Anyone on the mesh. You don't need to run anything or own any servers. You
just need a radio that can reach a node where Supply Drop BBS is running.

Common users:

- **Ham radio operators** using MeshCore or Meshtastic hardware
- **Community groups** that want off-grid messaging during events or emergencies
- **Experimenters** who want to try text-based radio communication

---

## What can you do with it?

**Public rooms** — Every BBS has rooms (channels). You join a room and read or
post messages that everyone on the BBS can see. A typical BBS might have rooms
for general chat, local news, net check-ins, or emergency coordination.

**Private mail** — Send a direct message to any registered user. They'll get a
notification next time they check in, even if they weren't online when you sent it.

**Persistent messages** — Messages stay on the server. You can scroll back through
what you missed, read at your own pace, and reply later. Radio doesn't have to be
real-time.

**Multiple radios, one community** — The same BBS serves MeshCore and Meshtastic
users at the same time. You don't all need the same hardware or firmware to talk
to each other.

---

## How does it work?

A Supply Drop BBS node is a small computer (usually a Raspberry Pi) connected
to a LoRa radio. When your radio reaches that node, you can send it short text
commands — the same way you'd send a message to any other node on the mesh.

```
You (radio)  →  mesh network  →  BBS node (Pi + radio)
```

The BBS responds with text that your radio displays. Commands are kept short
because radio frames are small. Most things are a single letter:

```
N    read new messages
E    post a message
M    go to mail
H    help
```

You don't need any special app. If your device can send a direct message to
another node, it can talk to the BBS.

---

## Getting started as a user

1. **Find a BBS node** near you. Ask in your local mesh group, or look for
   nodes advertising themselves as `room` type in your advert list.

2. **Send it a message.** On MeshCore or Meshtastic, send a direct message to
   the BBS node. You'll get back a welcome message and a short help menu.

3. **Create an account.** Type `REGISTER` followed by the username you want:
   ```
   REGISTER callsign
   ```

4. **Log in.**
   ```
   LOGIN callsign
   ```

5. **Read the rooms.** Type `K` to list rooms, then a room name to enter it.
   Type `N` to read new messages.

That's it. The [full user guide](USER_GUIDE.md) covers every command in detail,
but most people only ever use a handful of them.

---

## Running your own BBS

If you want to set up a node for your community, see the
[Installation guide](OPERATIONS.md). It takes about a minute on a Pi 4 using
the one-line installer.
