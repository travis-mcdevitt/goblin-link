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

The owner can stop a pairing on the handheld while it is under way. The handheld then closes
the connection and keeps nothing, whatever it has read by then. No message says so. Once the
plugin has received the pairing, it has spent the code and filed the id with a token, whether
or not its answer went out. It is left with that entry, whose token nobody holds, until the
id is paired again with a new code or unpaired on the Hermes. The same code sent again is a
code used twice: it is refused, and the refusal removes the entry.

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

## The commands Hermes offers

| Direction | Message |
|---|---|
| → handheld | `{"type":"commands","items":[{"name":"model","description":"…","group":"commands","category":"Configuration","args":"[model]","needs_text":false}]}` |

The slash commands Hermes offers the owner, all of them, in its order, each with Hermes's own
description in the owner's language. The plugin sends the list after `hello.ok` and again
whenever it is no longer the one it sent (it looks after each turn). A plugin that never
sends one leaves the handheld on its own few commands; a handheld that does not know the
message ignores it.

- `name` is the command without its slash.
- `group` is `commands` (Hermes's own commands, then its plugins') or `skills` (skill
  bundles, then skills).
- `category` is Hermes's heading for the command (`Session`, `Configuration`, `Info`, …);
  plugin commands come under its `Plugins` heading, and bundles and skills have none.
- `args` is what follows the name, as Hermes writes it (`<prompt>`, `[name]`).
- `needs_text` is true when the command does nothing without words after it (its `args`
  begins with `<`): the handheld never sends it bare.

This plugin builds the list as Hermes builds Telegram's menu: the registry's commands that a
messaging platform can use, then plugin commands, then skill bundles, then the commands of
the skills that are not turned off for `goblin_link`. Where a bundle and a skill share a
name, the bundle is listed, with its own description: it is what Hermes runs when the name
is sent. A plugin command that needs words after it is listed
with `needs_text`. It leaves out what makes no sense on a handheld, whichever kind of
command it is: `/start`, `/topic`, `/sethome` and `/platform`, `/approve` and `/deny`,
which the handheld answers on its cards, and the plugin's own `/goblinlink`, which refuses
to pair from a handheld.

The handheld checks the list before it keeps it. An `items` that is not a list is ignored,
and the list before it stands. Of the rest, a command is left out when its `name` is not 1
to 32 of `a-z 0-9 _ -`, its `group` is neither of the two, its `description` is longer than
200 characters, its `category` longer than 40 or its `args` longer than 200, or a field is
of another kind (`description` must be there; the others may be left out, as empty or
false). A field the handheld does not know is passed over. At most 300 are kept. What is kept is shown as any text from Hermes is, on one line.
The daemon keeps the last list in the profile, so that it can be shown before Hermes has
sent it again; unpairing the profile removes it.

## A turn

| Direction | Message |
|---|---|
| handheld → | `{"type":"turn","id":"t1","chat":"main","text":"…"}`, or `"voice_wav_b64"` in place of `text`, with `"answers":"p1"` when the voice note answers a question (below) |
| → handheld | `{"type":"turn.accepted","id":"t1","message_id":"…"}` or `{"type":"turn.rejected","id":"t1","reason":"…"}` |
| → handheld | `{"type":"status","turn":"t1","text":"…","working":true}` |
| → handheld | `{"type":"message","turn":"t1","message_id":"…","text":"<all of it so far>","done":false}` |
| → handheld | `{"type":"turn.done","turn":"t1"}` or `{"type":"turn.failed","turn":"t1","reason":"…"}` |
| handheld → | `{"type":"turn.cancel","id":"t1"}` |

`chat` is `main` or a side chat's id; each is its own conversation on the Hermes side. A
`message` carries the reply's whole text each time, not an addition. What Hermes is doing
before it answers (a tool it is running, a preview of the answer) comes as `status` text, one
line; an empty `text` means only that it is still working. The handheld follows a turn until
`turn.done` or `turn.failed`, however long it takes, and sends `turn.cancel` only when the
owner stops the reply. A voice note is accepted at once
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
| handheld → | `{"type":"prompt.answer","id":"p1","answer":"<choice id or text>"}`, with `"words":true` when it is the owner's own words |
| → handheld | `{"type":"prompt.closed","id":"p1"}` |
| → handheld | `{"type":"notice","chat":"main","message_id":"…","text":"…"}` |

A `prompt` is shown as a card in its chat: the first line of `text` as its heading, the rest
under it (without the blank lines before and after it), as plain text, and up to three of the
`choices` as buttons (a label is cut at 24 characters). A line break in the text is a line on
the card, with the spaces it begins with; a line too long for the card is carried over under
a mark that says so. In an `approval` the choice `deny` is the card's "no", and the first two
others are offered beside it. In a `clarify` the first three choices are offered, and the
owner may answer in words instead. Words answer only the card the chat shows: the first
question waiting there, in the order they were asked (with the handheld's own cards, for an
install or a setting, in the same line). When that card is a `clarify`, and is still the one
the app showed as the owner typed, what they type in the chat is sent as its `prompt.answer`
with `"words": true`, and the plugin takes them as words even where they look like a
choice's id. With any other card first, an `approval` above all, words are a new message:
an `approval` is never answered in words, and words with `"words": true` approve nothing.

A voice note has to be heard before it is words, so it goes as a `turn`, and one begun
while a `clarify` was the card shown names it in `answers`, if it is still first when the
note is sent. A `turn` in a chat whose turn is still under way is refused as `busy`, with two
exceptions. The first is a slash command sent as text (`/name`, perhaps with words after
it, `name` as in the list), while Hermes has the turn under way (not while a voice note is
still being heard), other than `/approve` and `/deny`, which the plugin refuses here so that
an approval asked in the turn under way is answered on its card. The plugin hands it to Hermes at once, which deals with it as with one sent from
Telegram: it runs it, queues it or refuses it, and `/stop` and `/new` stop the turn under
way (which then ends with `turn.failed` and the reason `cancelled`, shown as stopped). A
command Hermes does not know as its own (a skill's, a plugin's, any other `/word`) may stop
the turn under way too, as a message would, depending on Hermes's `busy_input_mode` (by
default `interrupt`). The command is a turn of its own: what Hermes says under it is its
`status` and `message`, and it ends with `turn.done` once Hermes has dealt with it. When
Hermes deals with it by queuing a turn for later (a `/queue` with a prompt), its answer
("Queued") is usually the command's reply (with a session authority Hermes may send it after
the turn has ended, and it then comes as a notice); what that later turn says under the
command is not followed as a turn: its answer comes as a `notice`, and its steps are not
shown. The second is a voice
note whose `answers` names a `clarify` waiting in that chat, while no other voice note is
being heard there: then what is heard in it answers that question, as words
(said first with `status` and `"heard": true`), and the plugin sends `prompt.closed` for
the question and `turn.done` for this turn. It ends with `turn.failed` instead when the
voice note could not be understood (the question still waits) or the question is already
over. The turn that asked goes on. A peer that does not know `answers` or `words` ignores
them: an older plugin refuses such a voice note as `busy`, as it did any turn in a busy
chat.

After the owner stops a turn, the handheld shows no new card from that chat, and answers an
`approval` with `deny`, until the plugin says the turn is over or ten seconds have passed.

The owner approves only what the card showed whole, and the handheld is the one that knows
what that was. It keeps at most 2,000 characters of `text`, and takes out of it every
character that no screen shows for what it is: control characters other than a line break;
the invisible ones (soft hyphen, zero-width and joining characters, direction marks,
invisible operators, fillers, tag characters, every variation selector); the blank braille
cell; and a combining mark with no character before it to sit on. A tab becomes four plain
spaces, a line or paragraph separator a line break, and a space that is not the plain space
(no-break, the fixed-width ones, the ideographic one) the plain space, since it reads as one
where a shell sees part of a word; each counts as lost. The list is
`goblin_ipc::shown::unseen`. One character goes without counting: the selector after the
warning sign that Hermes opens an approval with, when the sign is the first character of
`text`. An `approval` whose `text` lost anything else is offered with `deny` alone, and so
is one whose card cannot show all of it at the screen and text size in use: it does not all
fit, it has a character the handheld's fonts do not draw, or it has spaces the card cannot
stand for (at the end of a line, several where a line is carried over, indentation wider
than the card). A plugin may make the same judgement first (this one offers only `deny`
when the command is not wholly in the text it sends), but it is the handheld's that holds:
its app gives an answer other than `deny` only to an approval it showed whole. A `clarify`
that does not fit is shown as far as it goes, says that it is cut, and keeps its choices.

A card is shown only while its question can still be answered. A `prompt` belongs to the
turn under way in its `chat`, and its card goes when that turn ends in any way:
`turn.done`, `turn.failed`, the owner stopping it, the handheld no longer following it (a
turn that answered a question leaves the cards of the one that asked). A
`prompt` for a chat with no turn under way gets no card; if it is an `approval` the
handheld answers `deny` at once. When Hermes stops waiting for an answer while the turn
goes on (the question timed out, or was answered somewhere else), the plugin says so with
`prompt.closed` and the card goes then. This plugin sends it when Hermes reports an
approval settled or a clarifying question retired, and for whatever a turn still has open
when it ends. A Hermes that does not report an approval settled sends none for it: that
card goes with its turn. Once a card is gone the handheld sends no `prompt.answer` for it; an answer
that crosses with `prompt.closed` on the wire resolves nothing.

A `prompt` with the `id` of one still waiting replaces its card. A press on the earlier card,
which showed something else, is refused and sends nothing; the new card waits for its own
answer. A `prompt.answer` names only the `id`, though, so a Hermes cannot tell an answer that
was already on its way to the earlier question from one to the new: a plugin gives each
question its own `id`, as this one does.

## A reply spoken

| Direction | Message |
|---|---|
| handheld → | `{"type":"speech.get","id":"s1","text":"<the reply's words>"}` |
| → handheld | `{"type":"speech","id":"s1","mp3_b64":"…"}` or `{"type":"speech.failed","id":"s1","reason":"…"}` |

Hermes says the words with its own configured speech and sends the audio as MP3 (at most
2 MiB). Whether a reply is spoken is the handheld's setting; Hermes is asked only then.

## Limits and liveness

Ids (`id`, `turn`, `message_id`, `chat`, `command`) are at most 128 of `A-Z a-z 0-9 . _ -`;
a message with any other id is dropped. Text is cut (a reply at 48,000 characters, a question
at 2,000) and loses control characters and the invisible ones that reorder or hide text (a
question loses more: see above). A tab becomes four spaces and a space that is not the plain
space becomes one, so words, numbers and indentation stay apart; a reply alone keeps the
joiners that hold emoji together. The plugin applies the same rule before it sends. A message from the plugin is at most 4 MiB. The handheld runs at most 8 commands at once and answers further calls with
an error; `timeout_ms` is held to between 1 ms and 10 minutes. The handheld pings every 20 s
and ends a session it has heard nothing from for 60 s.

## Not in version 1

History, media files, moving avatars, and live calls. The handheld keeps
its own copy of each chat; voice notes go to Hermes for transcription.
