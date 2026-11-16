# Goblin Link plugin for Hermes Agent

Lets a Goblin Link handheld use a Hermes Agent as its agent: chat (text and voice notes),
Hermes's approval and clarifying questions as cards on the handheld, and two tools through
which Hermes can use the handheld's own commands. It uses Hermes's public plugin API only.

## Install

```sh
hermes plugins install travis-mcdevitt/goblin-link#hermes-plugin
```

Then, in `~/.hermes/.env`, say where a handheld can reach this machine (a LAN address, a
Tailscale name or address; the handheld tries them in order):

```sh
GOBLIN_LINK_HOSTS=192.168.1.20,my-host.tailnet.ts.net
```

Enable the `goblin_link` platform in `~/.hermes/config.yaml` and restart the gateway:

```yaml
platforms:
  goblin_link:
    enabled: true
```

`GOBLIN_LINK_PORT` (default 47832) and `GOBLIN_LINK_BIND` (default `0.0.0.0`) are optional.
The port must be reachable from the handheld, and should not be reachable from the internet.

## Pair a handheld

Send Hermes `/goblinlink pair` in a private chat (it refuses in a group, and from a handheld). It answers with a link that works once, for ten
minutes. On the handheld, open pairing, scan the code it shows with a phone, and paste the
link on that page. `/goblinlink devices` lists paired handhelds; `/goblinlink unpair <id>`
removes one. Each profile of a handheld is paired by itself: it is an entry of its own in
that list, under the handheld's name, and takes one of the 16 places for paired handhelds.

If you stop a pairing on the handheld, Hermes may already have received it. If so, it is in
that list, never connected, and the handheld kept nothing of it. Its link is used up: pasted
again within ten minutes, it is refused as already used, and that refusal takes the entry off
the list. To pair, ask for a new link. Pairing that profile replaces the entry if it is still
there, and `/goblinlink unpair <id>` removes it.

The link carries the fingerprint of this plugin's certificate, which is what lets the handheld
know it is talking to your Hermes and not to another machine on the network. Treat the link
like a password until it is used. If it is used twice, the handheld it paired is unpaired.

## What a paired handheld is

A paired handheld talks to Hermes as you do: it can ask for anything your agent can do on this
machine, and it answers Hermes's approval questions for its own chats. Pair only handhelds
you would hand your unlocked terminal to. Before pairing, a stranger who can reach the port
gets a TLS handshake, one small message and a refusal.

The port listens on every interface unless `GOBLIN_LINK_BIND` says otherwise.

An approval card offers more than "no" only when it shows the whole command exactly as it
will run. The plugin leaves a card only its "no" when the command is not wholly in the text
it sends (too long for it, or with tabs or other control characters in it). The handheld
judges again by itself, from what it draws: a card whose text does not all fit its screen
at the text size in use can only be refused there too. Approve those from another chat with
Hermes.

## What it keeps

Under `~/.hermes/goblin-link/`, readable by you alone: the certificate and its key, and
`devices.json` with each paired handheld's id, name and the SHA-256 of its token. Deleting the
certificate unpairs every handheld.

## What Hermes can do on the handheld

`handheld_commands` lists the commands the connected handheld offers; `handheld_run` runs one.
Whether a command may run is decided on the handheld, by the switches of the profile that is
paired (files, shell, builds, …), exactly as for any other agent. The plugin only forwards.
These two tools work only in a conversation that is on a Goblin Link handheld.

## How the agent appears on the handheld

`handheld_identity` lets your agent choose the name and the picture it is shown with on its
paired handhelds: `name`, `image_path`, or both. It is the agent's own to use; it is not
asked of you each time. It changes only that: not Hermes's persona or configuration, and not
the handheld's profile.

The picture is a still image file on the Hermes machine that the agent made or chose (PNG,
JPEG or WebP; at most 10 MB and 4096 pixels a side, at least 64). The plugin reads it,
crops it to a square and keeps a 352 × 352 PNG; it never fetches an address. A name is 1 to
40 printable characters on one line. Anything else is refused and nothing changes.

The choice is kept in `identity.json` and `avatar-<revision>.png` beside the pairing files,
one per Hermes profile, and is sent to every paired handheld: at once if connected,
otherwise when it next connects. Unlike the two tools above, this one is not tied to a
conversation on a handheld: the agent may use it from any conversation. Until the agent
chooses, the name is `GOBLIN_LINK_AGENT_NAME` or "Hermes" and the handheld shows its stock
picture. Delete the two files and restart the gateway to go back to that.

## Tests

`python hermes-plugin/tests/test_core.py` runs the pairing, token, limit and identity tests
without Hermes (it needs the `cryptography` package; the identity tests need Pillow and are
skipped without it). `hermes plugins doctor hermes-plugin` checks the
plugin against the installed Hermes.
