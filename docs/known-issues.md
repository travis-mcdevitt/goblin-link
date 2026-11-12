# Known issues

As of the first public version. "Not checked" means nobody has tried it, not that it fails.

## Pairing and profiles
- The first tap on Connect in the Muse app often fails; the second works. Cause not found.
- Pairing has been done from Android 13, from the app's own pairing screen on a fresh
  install. From an iPhone it has only been done with the Python SDK this project replaces,
  not with this code.
- A profile needs an SDK token before it can pair (the Muse app refuses one without). The
  only way to give it one on the handheld is a file on the SD card; see the README.
- A profile cannot be switched during a call.

## Using it
- In Settings, L2/R2 move the page but not the highlight, so the next D-pad press carries
  on from where the highlight was, which may be off screen.
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
- Only the TrimUI Brick with NextUI is supported.
