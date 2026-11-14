# The handheld and the Hermes plugin

How a handheld talks to the Goblin Link plugin running inside a Hermes Agent. The handheld
side is `crates/hermes-link`; the plugin is `hermes-plugin/` (not in the tree yet). Version 1.

## Connection

The plugin listens on a WebSocket with TLS at `wss://HOST:PORT/goblin-link`, under a
certificate it made itself. The handheld trusts exactly one certificate: the one whose SHA-256
it was given when it was paired. It does not consult certificate authorities or the host name.

Every message is one JSON object in one text frame, with a `type`. Unknown types are ignored.
Either side may send `{"type":"ping"}`; the answer is `{"type":"pong"}`.

## Pairing

The owner asks Hermes for a pairing link and pastes it on the handheld's pairing page:

```
goblinlink://HOST[,HOST...]:PORT/?fp=<64 hex digits>&code=<one-time code>
```

`fp` is the SHA-256 of the plugin's certificate. The code is 20 to 64 letters and digits, can
be spent once and expires.

| Handheld sends | Plugin answers |
|---|---|
| `{"type":"pair","protocol":1,"code":"…","device":{"id","name","version"}}` | `{"type":"paired","token":"…"}` or `{"type":"error","reason":"…"}` |

The handheld keeps the hosts, port, fingerprint, its device id and the token. The token is a
secret; it is the handheld's whole identity towards Hermes.

## Hello

First message of every later connection:

| Handheld sends | Plugin answers |
|---|---|
| `{"type":"hello","protocol":1,"device":"…","token":"…","name","version","commands":{…}}` | `{"type":"hello.ok","agent":"<name>"}` or `{"type":"error","code":"…","reason":"…"}` |

In an `error`, `reason` is for the owner to read. `code`, when present, is for the handheld to
act on: `unpaired` means Hermes no longer knows this handheld, and it must be paired again.

`commands` is the handheld's command list as its daemon describes it (the same descriptions it
registers with a Muse), so Hermes can be told what it may call.

## A turn

| Direction | Message |
|---|---|
| handheld → | `{"type":"turn","id":"t1","chat":"main","text":"…"}`, or `"voice_wav_b64"` in place of `text` |
| → handheld | `{"type":"turn.accepted","id":"t1","message_id":"…"}` or `{"type":"turn.rejected","id":"t1","reason":"…"}` |
| → handheld | `{"type":"status","turn":"t1","text":"…","working":true}` |
| → handheld | `{"type":"message","turn":"t1","message_id":"…","text":"<all of it so far>","done":false}` |
| → handheld | `{"type":"turn.done","turn":"t1"}` or `{"type":"turn.failed","turn":"t1","reason":"…"}` |
| handheld → | `{"type":"turn.cancel","id":"t1"}` |

`chat` is `main` or a side chat's id; each is its own conversation on the Hermes side. A
`message` carries the reply's whole text each time, not an addition.

## Commands on the handheld

| Direction | Message |
|---|---|
| → handheld | `{"type":"tool.call","id":"c1","command":"file.read","params":{…},"timeout_ms":30000}` |
| handheld → | `{"type":"tool.result","id":"c1","result":{"ok":true,"payload":{…}}}` or `{"ok":false,"error":"…"}` |

The plugin only forwards. Whether a command may run is decided on the handheld, by the
profile's switches, exactly as for a Muse.

## Questions and unprompted messages

| Direction | Message |
|---|---|
| → handheld | `{"type":"prompt","id":"p1","chat":"main","kind":"approval"\|"clarify","text":"…","choices":[{"id","label"}]}` |
| handheld → | `{"type":"prompt.answer","id":"p1","answer":"<choice id or text>"}` |
| → handheld | `{"type":"notice","chat":"main","message_id":"…","text":"…"}` |

## Limits and liveness

Ids (`id`, `turn`, `message_id`, `chat`, `command`) are at most 128 of `A-Z a-z 0-9 . _ -`;
a message with any other id is dropped. Text is cut (a reply at 48,000 characters) and loses
control characters and the invisible ones that reorder or hide text. A message from the plugin
is at most 1 MiB. The handheld runs at most 8 commands at once and answers further calls with
an error; `timeout_ms` is held to between 1 ms and 10 minutes. The handheld pings every 20 s
and ends a session it has heard nothing from for 60 s.

## Not in version 1

History, a reply's speech audio, files (media, avatar), and live calls. The handheld keeps
its own copy of each chat; voice notes go to Hermes for transcription.
