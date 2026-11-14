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

Send Hermes `/goblinlink pair` in any chat. It answers with a link that works once, for ten
minutes. On the handheld, open pairing, scan the code it shows with a phone, and paste the
link on that page. `/goblinlink devices` lists paired handhelds; `/goblinlink unpair <id>`
removes one.

The link carries the fingerprint of this plugin's certificate, which is what lets the handheld
know it is talking to your Hermes and not to another machine on the network. Treat the link
like a password until it is used. If it is used twice, the handheld it paired is unpaired.

## What it keeps

Under `~/.hermes/goblin-link/`, readable by you alone: the certificate and its key, and
`devices.json` with each paired handheld's id, name and the SHA-256 of its token. Deleting the
certificate unpairs every handheld.

## What Hermes can do on the handheld

`handheld_commands` lists the commands the connected handheld offers; `handheld_run` runs one.
Whether a command may run is decided on the handheld, by the switches of the profile that is
paired (files, shell, builds, …), exactly as for any other agent. The plugin only forwards.
The tools work only in a conversation that is on a Goblin Link handheld.

## Tests

`python hermes-plugin/tests/test_core.py` runs the pairing, token and limit tests without
Hermes (it needs the `cryptography` package). `hermes plugins doctor hermes-plugin` checks the
plugin against the installed Hermes.
