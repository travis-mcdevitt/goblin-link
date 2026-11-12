# Security

## What this software can do

Once a profile is paired, your Muse can ask the handheld to run commands. Which ones is up to
you, per profile, in **Settings**:

| Switch | What it allows | New profile |
|---|---|---|
| Files it can reach | Its own folder, the SD card, or everything | Its own folder |
| Read files / Write files | Within that reach | On |
| Run shell commands | Anything the daemon's account can do | Off |
| Run your own commands | Command files you placed on the device | Off |

Things to know before you turn switches on:

- **The shell is not confined.** On the firmwares supported so far the daemon runs as root,
  so a profile with the shell on can read and change everything on the handheld, including
  other profiles' data. Leave it off unless you want exactly that.
- **File reach is enforced for the file commands only**, by resolving links and `..` and
  refusing the daemon's data, other profiles' folders and your command files at every level.
- **Muse is an AI agent.** It acts on what it reads, including text from the web and from
  messages. Treat anything you let it do on the device as something a stranger's text might
  one day talk it into.

## What is stored on the device

Under the app's data folder on the SD card, per profile: the device token and refresh token
for that Muse account, the SDK token if you added one, cached conversations and media, and
the command log. The SD card is FAT: any program on the handheld can read these files, and
so can anyone who takes the card. Unpair a profile (in the app, and in the Muse app's device
list) before lending or selling the card.

## Pairing

Pairing uses the SDK's "community" mode. It keeps the exchange private from someone merely
listening, but it does not prove to the handheld that it is talking to your phone: someone
within Bluetooth range, actively interfering during the minute you pair, could intercept
it. Pair somewhere you trust.

## Reporting a problem

Please report security problems privately, through the repository's "Report a
vulnerability" (private advisory) feature, not in a public issue. Say what you found, how to
reproduce it, and which version. This is a spare-time project; expect a reply within a week
or two.
