# Security

## What this software can do

Once a profile is paired, your Muse can ask the handheld to run commands. Which ones is up to
you, per profile, in **Settings**:

| Switch | What it allows | New profile |
|---|---|---|
| File reach | Its own folder, the SD card, or everything | Its own folder |
| Read files / Write files | Reading within that reach; writing in its own folder | On |
| Write outside own folder | Writing beyond its own folder, within File reach; owner-only | Off |
| Run shell commands | Anything the daemon's account can do | Off |
| Your commands | Command files you placed on the device | Off |
| Install its builds | Tools, apps and games your Muse made: No, Ask first, or Freely | Ask first |

Existing profiles keep their saved settings; older consent files without an install setting
keep installs disabled.

Things to know before you turn switches on:

- **The shell is not confined.** On the firmwares supported so far the daemon runs as root,
  so a profile with the shell on can read and change everything on the handheld, including
  other profiles' data. Leave it off unless you want exactly that.
- **Writing outside its own folder can change programs run as root**, including SD-card
  launcher scripts and paks. Treat "Write outside own folder" like the shell: only you can
  change it manually in Settings. Muse can see the switch and explain why it needs it, but
  cannot request a change or an hour-long grant. It starts off for existing profiles too.
- **File reach is enforced for the file commands only**, by resolving links and `..` and
  refusing the daemon's data, other profiles' folders and your command files at every level.
- **What Muse installs is not confined either.** With "Install its builds" set to Ask first or Freely, your
  Muse can put programs on the handheld: tools it then calls, and apps and games listed in
  the launcher. A tool runs as root when Muse calls it; an app or game runs as root when you
  start it. So this switch reaches as far as the shell does. What it adds is a record and a
  gate: each profile can list and remove only what it installed, its tools are offered only
  to it, and with "Ask first" nothing is installed until you approve it on the handheld.
  The request appears in the chat as a card the handheld draws itself, saying what it is
  and what it runs, over two buttons; pressing one decides it and sends your answer to
  Muse as your message. Only that press installs anything: words in the chat, yours or
  Muse's, do not, and the replies Muse suggests are hidden while a request waits. Installed files are copied out of Muse's
  reach, so what runs is what was installed. Turning the switch off stops its tools being
  called; it does not remove them or un-list apps (remove those in Settings → Access).
- **Muse can ask you to change a setting, and only ask.** It can see the settings (the
  app's preferences and its own permissions) and put a change to you. The question appears
  in the chat as a card the handheld draws, with Muse's reason, over buttons: B for no,
  A for yes, and, when it is a permission being widened, Y for an hour, after which
  the handheld puts it back by itself. Nothing changes until you press. Muse cannot ask
  for the widest permissions at all (the shell, writes outside its own folder, files anywhere
  on the device, installing without asking): those change only in Settings. After a no, it cannot ask about that
  setting again for a day. A card is worth reading: Muse writes the reason on it, and
  the reason may have come from something it read.
- **A profile with the shell can leave things behind.** It can write a command file for
  "Your commands", which stays after the shell is turned off and is offered to
  every profile that has own commands on. Look in the commands folder after lending a
  profile the shell.
- **Muse is an AI agent.** It acts on what it reads, including text from the web and from
  messages. Treat anything you let it do on the device as something a stranger's text might
  one day talk it into.

## If the handheld is lost or stolen

**There is no lock.** The app has no PIN or password, and neither do the firmwares
supported so far. Whoever picks up the handheld can open the app and talk to your Muse as
you: read your conversations, and ask it to do anything it can do for you. If your Muse is
connected to other accounts (email, calendar, files, messaging, payments), those are within
reach too, through Muse. The same holds for someone who only takes the SD card, because the
tokens are on it (see below).

- Pair only accounts you would be willing to have exposed this way, and think about what
  each one is connected to.
- If the handheld or its card goes missing, **remove the device at once** in the Muse app
  on your phone (Settings → Devices). That is the only step that cuts it off. Then look
  through recent conversations and the connected accounts for anything you did not do.
- Treat the handheld like an unlocked phone that is signed in to everything.

## What is stored on the device

Under the app's data folder on the SD card, per profile: the device token and refresh token
for that Muse account, the SDK token if you added one, cached conversations and media, and
the command log. The SD card is FAT: any program on the handheld can read these files, and
so can anyone who takes the card. Unpair a profile (in the app, and in the Muse app's device
list) before lending or selling the card.

## Pairing

A profile needs an SDK token before it can pair. The handheld takes it from your phone
through a page it serves on your Wi-Fi while the pairing screen shows a code to scan. That
page is plain HTTP and asks for nothing but the token: while the screen is open, someone
already on the same network could read the token as it is sent, or send a token of their
own. It takes one token and then stops. Pair on a network you trust.

Pairing uses the SDK's "community" mode. It keeps the exchange private from someone merely
listening, but it does not prove to the handheld that it is talking to your phone: someone
within Bluetooth range, actively interfering during the minute you pair, could intercept
it. Pair somewhere you trust.

## Reporting a problem

Please report security problems privately, through the repository's "Report a
vulnerability" (private advisory) feature, not in a public issue. Say what you found, how to
reproduce it, and which version. This is a spare-time project; expect a reply within a week
or two.
