# Known issues

As of the first public version. "Not checked" means nobody has tried it, not that it fails.

## Pairing and profiles
- The first tap on Connect in the Muse app often fails; the second works. Cause not found.
- Pairing has been done from Android 13 and from an iPhone.
- The page for sending the SDK token has been used from Chrome on an Android phone, with
  the field filled by a script. The code on the handheld's screen decodes correctly from a
  screenshot; no phone camera has scanned it yet.
- A profile cannot be switched during a call.

## Using it
- If your Muse's VM is asleep, the handheld shows "not answering" and reconnects within
  about ten seconds of the VM waking; it cannot wake it.
- Reacting to a message and replying to one message in particular are not possible from
  the handheld.
- Side chats made elsewhere show a generic title until renamed on the handheld.

## Calls outside the published SDK
Everything the handheld sends is in Meta's published Muse Gadget SDK except three read-only
requests, found by watching what a paired device is allowed to ask its own VM:

- `GET /chat/history`: earlier messages, so chats survive a restart.
- `GET /identity`: your Muse's name and avatar.
- `GET /fs/raw/<path>`: pictures and clips a reply refers to, and the avatar image.

They are not documented and may change or stop working without notice. Without them the
app should still chat, but with no earlier history, avatar or reply media (not checked).

## What Muse builds
- Nothing confines an installed tool, app or game: see SECURITY.md.
- Installing or removing a tool restarts the connection to Muse (a few seconds), because
  the list of commands is only sent when it connects.
- If Muse removes one of its tools while it is still answering, the rest of that answer
  may not reach the handheld (the connection restarts under it). It is in the chat on
  your phone.
- A request to install something can only be answered on the handheld. Saying yes in the
  chat from your phone does not install it.
- Apps and games are started by the launcher as it starts anything; whether a particular
  build runs is up to how it was built. Only a tool has been tried end to end.

## Display and speed
- The chat is drawn with the GPU's help only while it is being read. With the keyboard up,
  during a call, or while a reply is arriving, frames are drawn on the CPU and scrolling is
  slower.
- Sleep and wake with the app open is not checked.

## Sound
- A clip's sound staying in step with its picture is not checked.
- CPU and memory during a call are not measured.

## Device
- Start on boot has been rehearsed but no real boot has been tried with it on.
- The shell, when a profile allows it, runs as root and is not confined (see SECURITY.md).
- Only the TrimUI Brick is known to work. The Brick Pro and Smart Pro start from platform
  files nobody has run; on the Smart Pro there are no function buttons, so voice notes and
  calls have no button yet, and its Bluetooth may need a different bring-up.
