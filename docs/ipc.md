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
| `profile.list` | — | `{active, profiles: [{name, paired, has_sdk_token, agent?: "muse"\|"hermes"}]}`; `agent` is absent while a profile is unpaired |
| `profile.create` | `{name}` | `{}` |
| `profile.rename` / `profile.delete` | `{name, new_name?}` | `{}` |
| `profile.switch` | `{name}` | `{}` — daemon reconnects under the new profile; emits `daemon.profile_changed`. Current profile is a no-op. `Busy` during a switch, pairing, or call |
| `profile.set_sdk_token` | `{name, token_path}` | `{}` — reads then deletes the file |
| `pair.start` | `{timeout_s?: 600, force?: false, agent?: "muse"\|"hermes"}` | `{ble_name}`. With `agent: "hermes"` the handheld serves its page for the link a Hermes gives, and nothing over Bluetooth |
| `pair.cancel` | — | `{}` |
| `pair.unpair` | — | `{}` |

Events: `pair.state {state: "need_token"|"connecting"|"advertising"|"connected"|"confirming"|"wifi"|"registering"|"done"|"failed"|"cancelled"|"timeout", detail?}`.
`need_token`: the page is being served, and `detail` is its address. `connecting` is for a Hermes only: its link has been pasted, the page has closed, and the handheld is connecting to that Hermes. `pair.cancel` stops it there as well: it ends as `cancelled`, with nothing saved. One that comes after the pairing was saved is too late: it ends as `done`.
The page the handheld serves while pairing (for a Muse token, or a Hermes link) also takes a name for the profile. The profile is renamed, as by `profile.rename`, when the pairing window closes, however the pairing went; the last `pair.state` (`done`, `failed`, …) is sent after that and carries the new name in `profile`. No `daemon.profile_changed` is sent for it; if the profile served was the one renamed, the daemon starts again after that last event. With `agent` given, the page asks only for what that agent needs.

### Chat

| Method | Params | Result |
|---|---|---|
| `chat.list` | — | `{chats: [Chat]}` — main plus side chats known locally |
| `chat.create` | `{title?}` | `{chat: Chat}` — allocates a UUID; the server creates it on first send |
| `chat.forget` | `{chat}` | `{}` — removes it from the local list only |
| `chat.rename` | `{chat, title}` | `{}` — the title on this device (1 to 60 characters); Muse is not told |
| `chat.history` | `{chat, before?: cursor, limit?: 50}` | `{messages: [Message], next?: cursor, complete}` |
| `chat.send` | `{chat, text, request?}` | `{turn: TurnId, message_id?}`. `request` is the id of the card the client showed in the chat as the owner wrote (its first in `requests.list`). When that card is a Hermes's clarifying question and is still the chat's first, the text is that question's answer, not a new turn: it is kept as the owner's message, the card goes, and the turn is `done` at once. Otherwise (no `request`, another card shown, an approval above all, or the card has gone or moved) the text is a new message |
| `chat.stop` | `{chat}` | `{}` — stops the reply under way in the chat: its turn ends as `stopped` at once, and nothing that arrives for that turn afterwards is kept (a reply that had arrived but was not yet read is dropped with it). With a Hermes Agent only. `unavailable` while linked with a Muse, which offers the handheld no way to stop a reply; `not_found` when no reply is under way that can be stopped. An answer of `{}` means the turn was told to stop; the `chat.turn` event says how it ended |
| `chat.mark_seen` | `{chat, message_id}` | `{}` |
| `chat.suggest` | `{replies: [string], chat?}` | `{chat}` — choices Muse offers the owner for what to say back, shown as pills that send the owner's message (together ≤ 60 characters since they share one row of pills, ≤ 6 choices, one line, no duplicates; `[]` clears). Without `chat`: the chat of the latest turn. Cleared when the owner sends the next message. The agent's own way to it is the built-in `handheld.suggest`, which needs no switch |

`Chat`: `{id, title, last_text?, last_at?, unread, pending_turns}`.
`Message`: `{id, chat, role: "user"|"assistant"|"system", text, choices?: [string], rich?: [Block], attachments?: [Attachment], at, state: "sending"|"sent"|"uncertain"|"failed"|"streaming"|"done", origin: "gadget"|"elsewhere"}`.
`Block` (assistant rich text, parsed by the daemon from Markdown-like text): `{t: "p"|"h"|"ul"|"ol"|"code"|"quote", spans|items|text, level?}`; a span is `{text, b?, i?, code?, link?}`.
`Attachment`: `{kind: "image"|"file"|"audio"|"video", name, path?, url?, bytes?, mime?, source?}`.
`choices` are text replies belonging to an assistant message, available without execution
permissions. Only the latest message offers buttons, and selecting one sends an ordinary
chat message. Older cached messages omit this optional field. Native Muse `option` widgets
are translated into these choices; unsupported widgets show a phone handoff. The compact
row supports up to six choices and 60 characters total; longer supported choices remain
readable in the message without buttons. No widget code or approval action is executed.

For an image or clip Muse sent, `path` is the local picture (a clip's poster) and `url` the local
clip, both absent until fetched; `source` is where Muse keeps the file, which lets the daemon
fetch it again.

Events: `chat.message {message}` (new or replaced whole message), `chat.delta {chat, message_id, text_append}`,
`chat.turn {turn, chat, state: "queued"|"acked"|"working"|"replying"|"done"|"failed"|"uncertain"|"stopped", activity?, reason?}`
(`failed`: the agent had the message and said it could not finish it; `reason` is why, in its few words on one line, when it said),
`chat.unread {chat, unread}`, `chat.list_changed {}`, `chat.suggestions {chat, replies}`.
`chat.history` (first page) also carries `suggestions` when Muse has set any.

Rule: a send that the transport cannot confirm becomes `uncertain`; the daemon never resends it.
The UI offers "Send again" explicitly.

### Voice

| Method | Params | Result |
|---|---|---|
| `voice.ptt_start` | `{chat, mode: "note"\|"dictate", request?}` | `{note: NoteId, max_seconds?}` — opens the mic. `max_seconds` is the longest the note may be: there the daemon ends the recording and sends the note as it is (`voice.note` `sending`, then `sent` or `failed`), without a `voice.ptt_stop`. `request` is the id of the card the client showed in the chat as the owner began, as for `chat.send`: when the note is sent, if that card is a Hermes's clarifying question and still the chat's first, the note answers it; otherwise it is a new message |
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

The `approval` notice of a Hermes's question (`prompt:<id>:<request>` in `requests.list`) is taken out
of the list when the question is: a notice does not lead to a card that is gone. It goes
whether or not it was seen, so a question that timed out while the owner was away leaves
nothing in the inbox. `notify.count` follows; there is no event for the removal itself.

### Device commands (what Muse ran here)

| Method | Params | Result |
|---|---|---|
| `commands.log` | `{limit?}` | `{entries: [{id, at, name, summary, state: "running"\|"ok"\|"error"\|"refused", ms?}]}` |
| `commands.consent.get` | — | `{shell, file_read, file_write, file_write_outside_home, drop_ins, files, builds}` for the profile being served; `files` is the reach of the file commands: `home` (the profile's own folder), `storage` (plus the SD card) or `device` (any path). Writes remain in the profile's home unless `file_write_outside_home` is true; absent in older consent files means false. This owner-only switch is visible to Muse as `write_outside_home`, but cannot be changed through `handheld.ask` |
| `commands.consent.set` | same keys, partial | the full toggles after the change |
| `builds.list` | — | `{items: [{name, kind: "tool"\|"app"\|"game", state: "pending"\|"installed", title, description, run, files, size_bytes, at, request?}]}`: what the profile being served has installed or is waiting on. A waiting item has `request`, new each time Muse asks, even under the same name |
| `builds.decide` | `{name, approve, request?}` | the list after the owner's answer to a pending install. A yes must name the `request` the owner was shown and is refused if another has replaced it; a no drops whatever waits. As with `requests.decide`, a client offers the yes only where it showed `title`, `description` and `run` whole (each argument told from the next, as `goblin_ipc::shown::command_line` writes them) |
| `builds.remove` | `{name}` | the list after removing one of the profile's items |
| `requests.list` | — | `{items: [{id, heading, body, facts, chat, kind: "approval"\|"question", cut?, answers: [{id: "no"\|"yes"\|"hour", label}]}]}`: what the agent has asked the owner and is waiting on: installs (`install:<name>:<request>`), changes to settings (`setting:<id>:<request>`) and a Hermes's own questions (`prompt:<id>:<request>`). `request` is new each time the agent asks, even again about the same name, setting or question id. Of an install or a setting the handheld writes every word but `body`, which is Muse's; a Hermes's question is in its own words. In an `approval` every answer but `no` gives leave for something (absent `kind` means `approval`); a `question` gives leave for nothing. `cut` is true when the handheld could not keep all of what a Hermes sent (see `docs/hermes-protocol.md`): an approval then has `no` for its only answer. A Hermes's question is listed only while it can be answered: it leaves when it is answered, when the turn it was asked in ends (finished, failed, stopped, no longer followed), when Hermes takes it back, and when the link drops; `requests.changed` follows each |
| `requests.decide` | `{id, answer}` | the list after the owner's answer; the answer to an install or a setting is also sent to the agent as the owner's message in that chat (to a Hermes still at work in that chat, once its turn ends: it shows in the chat as `sending` meanwhile, and as `failed` or `uncertain` if it cannot be sent, or if the daemon stopped before it was; it is never sent later), and the answer to a Hermes's question goes to that Hermes. Every `id` names its request, so an answer to one that has since been replaced is refused (`rejected`) and changes nothing, and the request that replaced it keeps waiting; so is an `id` without a request, as older clients sent. So is an answer the request does not list, and an answer to a question that is no longer listed (`rejected`): nothing of it is sent to the Hermes. The daemon cannot see what a client showed: a client offers an approval's answers other than `no` only where it showed `heading`, `body` and `facts` whole, as the app does from the card it drew: every line on screen, no character in them that passes unseen (`goblin_ipc::shown::unseen`) or that its font does not draw |

Events: `commands.started {entry}`, `commands.finished {entry}`. Arguments and output are never
sent over IPC beyond a ≤120-character summary.

After a `profile.rename` of the profile being served, or a `profile.switch`, and until the
daemon starts again, `builds.decide`, `builds.remove` and `requests.decide` are refused
(`rejected`, "profile switch in progress"), and so is every command the agent runs: until
then the daemon still knows the profile's folders by the old name.

### A Hermes's slash commands

| Method | Params | Result |
|---|---|---|
| `hermes.commands` | — | `{items: [{name, description, group: "commands"\|"skills", category, args, needs_text, while_busy}]}`: the slash commands the Hermes the profile being served is paired with last said it offers, in its order and its words (see `docs/hermes-protocol.md`). They are kept in the profile, so they are there while the link is down and straight after it is back, before Hermes has said them again. `name` is without its slash. `needs_text`: the command does nothing without words after it, and a client never sends it bare. `while_busy`: Hermes takes it while a reply is under way. Empty until a Hermes has sent its list (an older plugin never does: a client then offers its own few commands), for a profile paired with a Muse, and once the profile is unpaired |

Event: `hermes.commands_changed {items}`, the whole new list, when the list kept changes.

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
   in `daemon`; `link.state`, `agent.activity` and `hermes.commands_changed` in `link`.
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
    `notifications.json`, `commands.json`, `consent.json` and `hermes-commands.json` are per
    profile (the device-wide `consent.json` of earlier versions is copied into each profile
    once).

### Muse build launch diagnostics

The Muse command `build.logs` takes `{"name":"tetris"}` for an installed app or game
in the active profile. It returns `name`, `status` (null, `running`, or an exit-code string),
`output`, `output_limit_bytes` (65536), `output_may_be_truncated`, and a diagnostic note.
Like the other build commands, it requires builds to be enabled. It only reads bounded
regular files from that installed entry point directory, does not launch code, and does
not expose other profiles. This is a Muse command, not a new local IPC method.
