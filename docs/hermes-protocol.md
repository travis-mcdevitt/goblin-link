# The handheld and the Hermes plugin

How a handheld talks to the Goblin Link plugin running inside a Hermes Agent. The handheld
side is `crates/hermes-link`; the plugin is `hermes-plugin/`. Version 1.

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

What pairs is one profile of a handheld, and `device.id` is that profile's own id, whichever
profile the handheld is using at the time. The plugin keeps one token to an id, and a pairing
under an id it knows replaces the one before. So two profiles of one handheld are two entries
on one Hermes, each with its own token and its own conversations. `name` and `version` are
the handheld's, the same for each of its profiles.

## Hello

First message of every later connection:

| Handheld sends | Plugin answers |
|---|---|
| `{"type":"hello","protocol":1,"device":"…","token":"…","name","version","commands":{…}}` | `{"type":"hello.ok","agent":"<name>","avatar":{"revision":"…"}}` (`avatar` only when the agent has a picture) or `{"type":"error","code":"…","reason":"…"}` |

`device` is the id the profile was paired under, as it kept it with the token.

In an `error`, `reason` is for the owner to read. `code`, when present, is for the handheld to
act on: `unpaired` means Hermes no longer knows this handheld, and it must be paired again.

`commands` is the handheld's command list as its daemon describes it (the same descriptions it
registers with a Muse), so Hermes can be told what it may call.

## Who the agent is

| Direction | Message |
|---|---|
| → handheld | `{"type":"identity","agent":"<name>","avatar":{"revision":"…"}}` |
| handheld → | `{"type":"avatar.get","id":"a1","revision":"…"}` |
| → handheld | `{"type":"avatar","id":"a1","revision":"…","png_b64":"…"}` or `{"type":"avatar.failed","id":"a1","reason":"…"}` |

`hello.ok` says the agent's name and, if it has a picture, the picture's `revision`. The
agent may change either while connected (its `handheld_identity` tool); `identity` then
carries both as they are now, in the shape of `hello.ok`. A revision is 1 to 64 letters,
digits, `_` or `-`, and changes whenever the picture does; any other is taken as no picture.
With no `avatar`, the agent has no picture: the handheld stops showing the one it had.

The picture itself is asked for by revision, when the handheld does not have that one yet.
It is a square PNG of at most 1 MiB (the plugin sends 352 × 352). A revision that is no
longer the current one is answered with `avatar.failed`; the `identity` that replaced it has
been sent already. The handheld shows a picture only once it has fetched and read it, and
keeps the one before until then, so an `identity` or a tool result is not proof of what is
on the screen.

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
`message` carries the reply's whole text each time, not an addition. What Hermes is doing
before it answers (a tool it is running, a preview of the answer) comes as `status` text, one
line; an empty `text` means only that it is still working. A voice note is accepted at once
and transcribed afterwards: what Hermes heard comes as a `status` with `"heard": true`, and a
note it could not understand ends the turn with `turn.failed`.

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

## A reply spoken

| Direction | Message |
|---|---|
| handheld → | `{"type":"speech.get","id":"s1","text":"<the reply's words>"}` |
| → handheld | `{"type":"speech","id":"s1","mp3_b64":"…"}` or `{"type":"speech.failed","id":"s1","reason":"…"}` |

Hermes says the words with its own configured speech and sends the audio as MP3 (at most
2 MiB). Whether a reply is spoken is the handheld's setting; Hermes is asked only then.

## Limits and liveness

Ids (`id`, `turn`, `message_id`, `chat`, `command`) are at most 128 of `A-Z a-z 0-9 . _ -`;
a message with any other id is dropped. Text is cut (a reply at 48,000 characters) and loses
control characters and the invisible ones that reorder or hide text. A message from the plugin
is at most 4 MiB. The handheld runs at most 8 commands at once and answers further calls with
an error; `timeout_ms` is held to between 1 ms and 10 minutes. The handheld pings every 20 s
and ends a session it has heard nothing from for 60 s.

## Not in version 1

History, media files, moving avatars, and live calls. The handheld keeps
its own copy of each chat; voice notes go to Hermes for transcription.
