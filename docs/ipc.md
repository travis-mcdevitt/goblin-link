# Daemon ↔ client IPC contract (v1)

`goblind` serves this protocol; `goblin-ui` and `goblinctl` are clients. The Rust types live in
`crates/goblin-ipc` and are the source of truth once written; this file is the design they implement.

## Transport

- Unix stream socket: `<runtime_dir>/<profile>.sock`, mode 0600, where `runtime_dir` comes from the
  platform adapter (`/run/goblin-link` on the Brick, `$XDG_RUNTIME_DIR/goblin-link` elsewhere).
  `<runtime_dir>/active` is a symlink to the active profile's socket.
- Frames are single-line UTF-8 JSON terminated by `\n`. Maximum line 1 MiB; longer lines close the
  connection. Binary data never crosses the socket: audio, images and files are passed as paths
  under `<data_dir>/spool/`.
- Three frame shapes:

```json
{"id": 12, "method": "chat.send", "params": {…}}          // client → daemon
{"id": 12, "ok": true, "result": {…}}                      // daemon → client
{"id": 12, "ok": false, "error": {"code": "not_paired", "message": "…"}}
{"seq": 4411, "event": "chat.delta", "data": {…}}          // daemon → client, after subscribe
```

- `id` is a client-chosen u64, unique per connection. Requests may be pipelined; replies can
  arrive out of order.
- First request on a connection must be `hello`.

## Error codes

`bad_request`, `unknown_method`, `not_paired`, `not_connected`, `busy`, `not_found`,
`unavailable` (capability missing on this platform), `consent_off`, `timeout`, `rejected`
(server refused), `uncertain` (request may have been delivered; never auto-retried),
`internal`.

## Events and replay

Every event has a monotonic `seq` (u64, per daemon run) and the daemon keeps the last 2048 in a
ring. `subscribe {after_seq?, topics?}` starts delivery; if `after_seq` is older than the ring the
reply has `"gap": true` and the client must re-fetch state with the `*.get`/`*.list` methods.
`hello` returns `run_id`; a changed `run_id` means the daemon restarted and `seq` reset.

Topics: `daemon`, `link`, `chat`, `voice`, `notify`, `commands`, `platform`, `settings`.

## Methods

Types used below: `ChatId` = `"main"` or a side-chat UUID string. `TurnId`, `NoteId` = opaque
strings. Timestamps are Unix milliseconds.

### Session

| Method | Params | Result |
|---|---|---|
| `hello` | `{client: "ui"\|"ctl", version}` | `{daemon_version, protocol: 1, run_id, profile, capabilities: {...}}` |
| `subscribe` | `{after_seq?, topics?: [..]}` | `{seq, gap}` |
| `daemon.status` | — | `Status` (below) |
| `daemon.shutdown` | — | `{}` |

`Status`: `{profile, paired, link: LinkState, agent: {name, avatar_path?, activity?: {code, text}},
vm: {name?}, token_age_s?, last_error?,
battery?: {percent, charging}, uptime_s, version}`.

`LinkState`: `"unpaired" | "offline" | "connecting" | "registering" | "online" | "reconnecting" |
"auth_failed"`. Event `link.state {state, since, error?}`; event `agent.activity {code, text}`.

### Profiles and pairing

| Method | Params | Result |
|---|---|---|
| `profile.list` | — | `{active, profiles: [{name, paired, has_sdk_token}]}` |
| `profile.create` | `{name}` | `{}` |
| `profile.rename` / `profile.delete` | `{name, new_name?}` | `{}` |
| `profile.switch` | `{name}` | `{}` — daemon reconnects under the new profile; emits `daemon.profile_changed` |
| `profile.set_sdk_token` | `{name, token_path}` | `{}` — reads then deletes the file |
| `pair.start` | `{timeout_s?: 600, force?: false}` | `{ble_name}` |
| `pair.cancel` | — | `{}` |
| `pair.unpair` | — | `{}` |

Events: `pair.state {state: "advertising"|"connected"|"confirming"|"wifi"|"registering"|"done"|"failed"|"cancelled"|"timeout", detail?}`.

### Chat

| Method | Params | Result |
|---|---|---|
| `chat.list` | — | `{chats: [Chat]}` — main plus side chats known locally |
| `chat.create` | `{title?}` | `{chat: Chat}` — allocates a UUID; the server creates it on first send |
| `chat.forget` | `{chat}` | `{}` — removes it from the local list only |
| `chat.rename` | `{chat, title}` | `{}` — the title on this device (1 to 60 characters); Muse is not told |
| `chat.history` | `{chat, before?: cursor, limit?: 50}` | `{messages: [Message], next?: cursor, complete}` |
| `chat.send` | `{chat, text}` | `{turn: TurnId, message_id?}` |
| `chat.mark_seen` | `{chat, message_id}` | `{}` |
| `chat.suggest` | `{replies: [string], chat?}` | `{chat}` — choices Muse offers the owner for what to say back, shown as pills that send the owner's message (together ≤ 60 characters since they share one row of pills, ≤ 6 choices, one line, no duplicates; `[]` clears). Without `chat`: the chat of the latest turn. Cleared when the owner sends the next message. Called by Muse through the `chat.suggest_replies` drop-in (`goblinctl chat suggest`) |

`Chat`: `{id, title, last_text?, last_at?, unread, pending_turns}`.
`Message`: `{id, chat, role: "user"|"assistant"|"system", text, rich?: [Block], attachments?: [Attachment], at, state: "sending"|"sent"|"uncertain"|"failed"|"streaming"|"done", origin: "gadget"|"elsewhere"}`.
`Block` (assistant rich text, parsed by the daemon from Markdown-like text): `{t: "p"|"h"|"ul"|"ol"|"code"|"quote", spans|items|text, level?}`; a span is `{text, b?, i?, code?, link?}`.
`Attachment`: `{kind: "image"|"file"|"audio"|"video", name, path?, url?, bytes?, mime?, source?}`.
For an image or clip Muse sent, `path` is the local picture (a clip's poster) and `url` the local
clip, both absent until fetched; `source` is where Muse keeps the file, which lets the daemon
fetch it again.

Events: `chat.message {message}` (new or replaced whole message), `chat.delta {chat, message_id, text_append}`,
`chat.turn {turn, chat, state: "queued"|"acked"|"working"|"replying"|"done"|"failed"|"uncertain", activity?}`,
`chat.unread {chat, unread}`, `chat.list_changed {}`, `chat.suggestions {chat, replies}`.
`chat.history` (first page) also carries `suggestions` when Muse has set any.

Rule: a send that the transport cannot confirm becomes `uncertain`; the daemon never resends it.
The UI offers "Send again" explicitly.

### Voice

| Method | Params | Result |
|---|---|---|
| `voice.ptt_start` | `{chat, mode: "note"\|"dictate"}` | `{note: NoteId}` — opens the mic |
| `voice.ptt_stop` | `{note, cancel?: false}` | note: `{turn}` · dictate: `{text}` · cancel: `{}` |
| `voice.speak` | `{message_id}` | `{}` — stream and play the reply's TTS |
| `voice.stop_speaking` | — | `{}` |
| `voice.call_start` | `{chat?}` | `{call: id}` |
| `voice.call_end` | — | `{reason}` |
| `voice.call_mute` | `{muted}` | `{}` |
| `voice.barge_in` | — | `{}` |

Events: `voice.level {source: "mic"|"playback", rms: 0..1}` (≤ 15 Hz, only while a client is
subscribed to `voice`), `voice.note {note, state: "recording"|"sending"|"sent"|"failed", seconds}`,
`voice.speaking {message_id?, state: "start"|"stop"}`,
`voice.call {state: "connecting"|"ready"|"listening"|"thinking"|"speaking"|"ended", reason?, muted, seconds}`,
`voice.caption {role, text, final}` when the server provides transcripts.

### Notifications

| Method | Params | Result |
|---|---|---|
| `notify.list` | `{limit?}` | `{items: [Notice], unseen}` |
| `notify.seen` | `{ids?: [..]}` (all when omitted) | `{}` |
| `notify.clear` | — | `{}` |

`Notice`: `{id, at, kind: "reply"|"turn_done"|"turn_failed"|"link"|"command"|"approval"|"activity", title, body?, chat?, link?: Route, seen}`.
Event: `notify.new {notice}`; `notify.count {unseen}`.

### Device commands (what Muse ran here)

| Method | Params | Result |
|---|---|---|
| `commands.log` | `{limit?}` | `{entries: [{id, at, name, summary, state: "running"\|"ok"\|"error"\|"refused", ms?}]}` |
| `commands.consent.get` | — | `{shell, file_read, file_write, file_write_outside_home, drop_ins, files, builds}` for the profile being served; `files` is the reach of the file commands: `home` (the profile's own folder), `storage` (plus the SD card) or `device` (any path). Writes remain in the profile's home unless `file_write_outside_home` is true; absent in older consent files means false. This owner-only switch is visible to Muse as `write_outside_home`, but cannot be changed through `handheld.ask` |
| `commands.consent.set` | same keys, partial | the full toggles after the change |
| `builds.list` | — | `{items: [{name, kind: "tool"\|"app"\|"game", state: "pending"\|"installed", title, description, run, files, size_bytes, at}]}`: what the profile being served has installed or is waiting on |
| `builds.decide` | `{name, approve}` | the list after the owner's answer to a pending install |
| `builds.remove` | `{name}` | the list after removing one of the profile's items |
| `requests.list` | — | `{items: [{id, heading, body, facts, chat, answers: [{id: "no"\|"yes"\|"hour", label}]}]}`: what Muse has asked the owner and is waiting on (installs and changes to settings). The handheld writes every word but `body`, which is Muse's |
| `requests.decide` | `{id, answer}` | the list after the owner's answer; the answer is also sent to Muse as the owner's message in that chat |

Events: `commands.started {entry}`, `commands.finished {entry}`. Arguments and output are never
sent over IPC beyond a ≤120-character summary.

### Settings and platform

| Method | Params | Result |
|---|---|---|
| `settings.get` | `{keys?}` | `{values: {key: value}}` |
| `settings.set` | `{values}` | `{}` — validated; emits `settings.changed {values}` |
| `platform.info` | — | adapter capabilities, display, input map, glyph layout |
| `platform.volume` | `{set?: 0..100}` | `{percent}` |
| `platform.brightness` | `{set?: 0..100}` | `{percent}` |
| `platform.autostart` | `{set?: bool}` | `{enabled}` |
| `platform.health` | — | uptime, load, memory, disk, temperatures, battery |

Settings keys (all local to the handheld): `appearance.mode` (`system|light|dark`),
`appearance.chat_theme` (`avatar|default|monochrome|beige|blue|purple|pink|orange|green`),
`appearance.avatar_size` (`small|medium|large|xl|hidden`), `appearance.ui_size`
(`compact|normal|comfortable|large`), `appearance.reduce_motion`, `voice.call_greeting` (Muse
speaks first on a call; default off), `voice.speak_replies` (`voice_only|never`; default
`never`: voice notes are speech-to-text, `voice_only` = voice-to-voice), `leds.mode`
(`off|presence|presence_dim`), `leds.never_touch`, `haptics.enabled`.

The legacy `voice.speak_replies` value `always` is accepted as an alias for `voice_only`
and returned/saved as `voice_only`. It is no longer offered by `handheld.settings` or
accepted by `handheld.ask`. Replies to typed messages stay silent.

## Routes

A notice's `link` is a `Route`, a string naming where it points. The daemon writes
`chat/<id>`.

## Versioning

`protocol` is an integer. Additive changes (new methods, fields, events, enum values) do not bump
it; clients ignore unknown fields and events and treat unknown enum values as a generic state.

## Clarifications (resolved while implementing `goblin-ipc`)

Gaps in the text above, resolved conservatively. The types in `crates/goblin-ipc` and the generated
`ipc-examples.json` (one example per method and event; a test fails when it is stale) are
authoritative.

1. **Hello and subscribe.** The server enforces `hello` first (any other method before it gets
   `bad_request`; a second `hello` gets `bad_request`). The handler answers `hello` (it supplies
   `run_id`, profile, capabilities); `subscribe` is answered by the server itself.
2. **Subscribe semantics.** `seq` in the reply is the newest sequence number issued so far.
   Without `after_seq` only live events are delivered (no replay). With `after_seq` the reply is
   sent first, then every ring event with `seq > after_seq` that matches the topics, then live
   events, with nothing lost or duplicated. `gap` is true when the oldest ring entry is newer than
   `after_seq + 1`, or when `after_seq` is greater than any `seq` issued; on a gap nothing is
   replayed, because replaying a partial history over freshly fetched state could regress it. A
   later `subscribe` on the same connection replaces its filter. Omitted `topics` means all
   topics; an empty list means none; unknown topic names are ignored.
3. **Topics of events not listed under a topic.** `daemon.profile_changed` and `pair.state` are
   in `daemon`; `link.state` and `agent.activity` in `link`.
   Events with unknown names are delivered only to unfiltered subscribers.
4. **Payload shapes left open.** `daemon.profile_changed {profile}`. `profile.rename` requires
   `new_name`; `profile.delete` takes only `name`. `platform.info` and `platform.health` are
   free-form objects. `Block`: `p`, `h` and `quote` use `spans`, `ul` and `ol` use `items` (a
   list of span lists), `code` uses `text`, `level` applies to `h`. `Status.last_error` is a
   string.
5. **Defaults.** Optional booleans (`force`, `cancel`, span styles)
   default to false and are omitted when false. The daemon applies `timeout_s` 600 and `limit`
   50 itself; the types keep them optional.
6. **Concurrency and limits.** Requests of one connection are handled concurrently (replies may
   be out of order); more than 64 in flight get `busy`. Each connection has a bounded outgoing
   queue (256 lines). A client whose queue overflows, or that cannot take a write within 10 s,
   is disconnected; the daemon never waits for it. Events whose encoded line would exceed 1 MiB
   are not published. Replies that would exceed it become `internal`.
7. **Malformed input.** A line that is not a JSON object with a numeric `id` and a string
   `method`, or longer than 1 MiB, closes the connection without a reply (an id cannot be
   echoed). A well-formed request with an unknown method gets `unknown_method`; invalid params
   get `bad_request`. Blank lines are ignored. Missing `params` equals `{}`.
8. **Sockets.** The socket is created with mode 0600 right after `bind`; the parent directory is
   created with mode 0700 only when missing (an existing directory is not changed, so the
   platform adapter owns its mode). A stale socket file from a dead daemon is replaced; a socket
   that still accepts connections gives `AddrInUse`; a non-socket file is never removed. The
   `active` symlink is replaced atomically and its target is the relative `<profile>.sock`.
9. **Profile names.** 1 to 32 ASCII letters, digits, `_` or `-` (a superset of what the Brick
   `mg-new` tool creates); names that differ only in case are treated as the same profile because
   the SD card is FAT.
10. **Store layout.** Beyond the SDK files, `settings.json` is device-wide; the chat cache,
    `notifications.json`, `commands.json` and `consent.json` are per profile (the device-wide
    `consent.json` of earlier versions is copied into each profile once).
