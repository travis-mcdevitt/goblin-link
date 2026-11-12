# Goblin Link for Linux handhelds

A Muse client for small Linux game handhelds. It turns the handheld into a *Muse gadget*: a
device your Muse can talk to you through and, where you allow it, act on. Muse is the only
agent it works with today; the crates that speak Muse's protocol (`muse-proto`, `muse-link`,
`muse-pair`) are kept apart from the rest so another agent can be added beside it.

- **Chat** with your Muse, main chat and side chats, with pictures and clips in replies.
- **Voice notes** (hold a button and talk) and **calls**.
- **Alerts**: lights and vibration when a reply arrives while the app is closed.
- **Several Muse accounts** on one handheld, each as its own profile with its own chats,
  files and permissions.
- **Pairing from the handheld**, over Bluetooth, with the Muse app on your phone.
- **You decide what Muse may do on the device**: read or write files (and how far they
  reach), run shell commands, run commands you wrote yourself. A new profile starts with
  files in its own folder and nothing else.

| Chat | Profiles | What Muse may do |
|---|---|---|
| ![Chat](docs/images/chat-dark.png) | ![Profiles](docs/images/profiles.png) | ![Access](docs/images/access.png) |

It speaks the protocol of Meta's open-source
[Muse Gadget SDK](https://github.com/facebookincubator/muse-gadget-sdk) and parts of it are a
Rust port of that SDK (see [NOTICE](NOTICE)). It is an independent project: **not made by,
affiliated with or endorsed by Meta**. "Muse" is used only to say what it works with.

## Status

Early. It is used daily on one device by its author and has not been tried by anyone else.

| Device | Firmware | State |
|---|---|---|
| TrimUI Brick | NextUI | Works: chat, voice notes, calls, alerts, pairing, profiles |
| Other handhelds | — | Not yet. A device is described by one file and a few scripts: see [platforms/](platforms/README.md) |

Known gaps are listed in [docs/known-issues.md](docs/known-issues.md).

## Install on a TrimUI Brick (NextUI)

1. Download `goblin-link-pak.zip` from the releases page and unzip it.
2. Copy the `Goblin Link.pak` folder to `Tools/tg5040/` on the SD card.
3. On the Brick, open **Tools → Goblin Link** once, then close it. This makes a
   `GoblinLink` folder on the SD card.
4. Get an SDK token and put it on the card (see [Getting an SDK token](#getting-an-sdk-token)
   below). The Muse app will not pair a gadget that has none.
5. In the app: **Menu → Settings**, press R1 to reach **Profiles**, choose your profile,
   then **Pair with the Muse app**
   and follow the steps on screen. You need the Muse app on a phone with Developer mode on
   (Settings → Devices), because this is a community device.

### Getting an SDK token

Meta issues SDK tokens to Muse account holders; this project does not come with one and
cannot give you one.

1. On a computer or phone, open
   [gadgets.muse.ai/settings/sdk-tokens](https://gadgets.muse.ai/settings/sdk-tokens) and
   sign in with the Muse account you want on the handheld.
2. Create a token there and copy it. It starts with `mgst_`.
3. Save it in a plain text file named `sdk_token.txt`, containing the token and nothing
   else.
4. Copy that file into the `GoblinLink` folder on the SD card (the folder appears after the
   app has been opened once).
5. Pair. The app moves the token into the profile being paired and removes the file.

Tokens come with Meta's
[Gadget SDK Token Terms of Use](https://gadgets.muse.ai/sdk-terms): among other things they
are for personal, non-commercial use with your own Muse account. Read them; the token is
yours and so is keeping to them. Use a separate token file for each profile you pair.

To build it yourself, see [docs/building.md](docs/building.md).

## What you are trusting

Pairing gives this software a device token for your Muse account, kept on the SD card, and
gives your Muse a way to run the commands you have switched on.

**A lost handheld has no security.** There is no PIN or lock: whoever holds it can talk to
your Muse as you, and through it reach whatever your Muse is connected to (email and other
linked accounts). If it goes missing, remove the device in the Muse app straight away. Read
[SECURITY.md](SECURITY.md) before pairing an account you care about.

## How it is put together

| Part | What it is |
|---|---|
| `goblind` | The daemon: keeps the link to Muse, runs commands under your permissions, keeps chats |
| `goblin-ui` | The app: an SDL2 program that draws its own interface |
| `goblinctl` | Command line for the same things, for scripts and testing |
| `crates/muse-proto`, `muse-link`, `muse-pair` | The wire protocol, the client, and Bluetooth pairing |
| `crates/goblin-ui-kit` | The drawing kit: text, widgets, focus, and GPU sprites |
| `platforms/` | One folder per device: what hardware it has and how to drive it |

The app and the daemon talk over a local socket; the protocol is in [docs/ipc.md](docs/ipc.md).

## Licence

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE) for the SDK port, fonts and icons.
