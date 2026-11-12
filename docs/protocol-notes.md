# Goblin Link wire protocol notes

For the engineer building the async client on top of `muse-proto`. This
describes the wire layers as ported from the Python reference
(`facebookincubator/muse-gadget-sdk`,
Apache-2.0), and the public API of the crate. The crate is pure: no sockets,
no async, no randomness of its own. You own the WebSocket, the clock and the
RNG; `muse-proto` owns the bytes.

Everything here is verified byte-for-byte against vectors dumped from the
reference (`tools/dump_vectors.py` → `crates/muse-proto/tests/vectors/`).

## The stack, top to bottom

```
  link.register / link.invoke / link.result ...   (JSON control messages)
  ──────────────────────────────────────────────  control.rs
  HTTP-like request/response on numbered streams    (ServiceFrame)
  ──────────────────────────────────────────────  envelope.rs
  ServiceRequest / ServiceResponse wrapper
  ──────────────────────────────────────────────  envelope.rs
  chunked NoiseTransportFrame (fragmentation)       framing.rs
  ──────────────────────────────────────────────
  AES-256-GCM record (one per frame)                noise.rs (CipherState)
  ──────────────────────────────────────────────
  WebSocket binary messages to wss://…/v1/noise     (you provide this)
```

A Noise XX handshake runs first over the same WebSocket; after it splits into
two ciphers, every WebSocket binary message is one encrypted
`NoiseTransportFrame`.

## 1. Handshake (`noise.rs`)

`Noise_XX_25519_AESGCM_SHA256`, initiator role. Drive
`NoiseXXInitiator::new(key_source)` then:

1. `initialize()`
2. `write_message1()` → send the bytes (32 bytes: `e`).
3. receive message 2, `read_message2(&msg2)` → returns the responder's
   payload (empty in practice). After this, `remote_static_public_key()` is
   available.
4. `write_message3()` → send the bytes. Message 3's payload is empty: the
   per-VM bearer token in the WebSocket upgrade already authenticated us.
5. `split()` → `(send, recv)` ciphers for the transport.

Notes and surprises:

- **Key source.** The reference generates X25519 keys internally with the
  system CSPRNG. The port injects them through a `KeySource` trait
  (`OsKeySource` in production, `FixedKeySource` in tests) so the handshake is
  deterministic and testable. This is an API shape difference only; the wire
  bytes are identical.
- **Protocol-name hashing.** The initial hash is the 28-byte protocol name
  right-padded with zeros to 32 bytes. For SHA-256 Noise this equals the
  standard construction (the name is ≤ 32 bytes), so this is spec-compliant,
  not a custom twist.
- **Low-order guard.** Every peer public key and DH output is checked against
  seven known low-order X25519 points and against an all-zero shared secret;
  either is a fatal `NoiseError`.
- **Phases are strict.** Calling a step out of order, or after a failure,
  returns `WrongPhase`/`DeadHandshake`. Any failure makes the handshake dead;
  do not reuse it — reconnect.
- **`CipherState`.** AES-256-GCM, 12-byte nonce = four zero bytes then a
  big-endian 64-bit counter. The counter is capped at `2^53 − 1`
  (JavaScript's safe-integer limit, not `2^64`); reaching it is a fatal
  `NonceExhausted`. Any decrypt failure (bad tag) or encrypt error **poisons**
  the cipher: every subsequent call errors. An unkeyed `CipherState` passes
  data through unchanged (used mid-handshake).
- `NoiseXXResponder` is a faithful port of the reference's in-process test
  responder (so the whole handshake can run without a VM). Its `split()`
  returns the cipher pair swapped relative to the initiator.

## 2. Transport framing (`framing.rs`)

One plaintext message (an encoded `ServiceRequest` client→VM, or
`ServiceResponse` VM→client) is split into chunks that share a random 64-bit
`chunk_id`:

- `MAX_CHUNK_PAYLOAD = 65489` bytes per chunk, so an encrypted frame stays
  under the 65535-byte Noise record limit.
- `MAX_TOTAL_CHUNKS = 256` → a message is at most ~16 MB.
- `encode_noise_frames(data, chunk_id)` returns the encoded frames (you draw
  `chunk_id` with `framing::random_chunk_id(rng)`; all frames of one message
  share it — the reference draws it internally, the port makes it a parameter
  for purity). Empty input still produces one frame.

`NoiseFrameDecoder` reassembles. It holds at most `MAX_PENDING_ASSEMBLIES = 16`
partial messages; a partial message is dropped when it is older than
`ASSEMBLY_TTL` (60 s, measured from its **first** chunk, not last touch).
Chunks may arrive in any order. Duplicate index, inconsistent `total_chunks`
for a `chunk_id`, out-of-range index, oversize payload, or exceeding the limits
is a fatal error that **poisons** the decoder. `decode()` returns `Ok(None)`
while a message is still incomplete, `Ok(Some(bytes))` when it completes.

Use `Instant::now` via `decode()`, or supply the clock with `decode_at()`.

## 3. Envelopes and streams (`envelope.rs`)

`encode_service_frame`/`decode_service_frame` handle a `ServiceFrame`:

- `stream_id: i64` — the client allocates 1, 2, 3, … one per request stream.
- an optional body, exactly one of `ApplicationRequest` (field 2),
  `ApplicationResponse` (3), `BodyChunk` (4) or `Reset` (5).

Encoding is proto3: default values (empty string/bytes, 0, false) are omitted.
Decoding skips unknown fields, rejects a known field with the wrong wire type,
requires valid UTF-8 for strings, and — like protobuf — lets the **last**
occurrence of a repeated field or of the oneof win. `int32` fields
(`ApplicationResponse.status`, `Reset.code`) sign-extend to ten bytes when
negative.

`ServiceRequest { service, payload }` wraps the encoded frame for the client
side (`service` defaults to `Daemon` and is omitted when so);
`ServiceResponse { payload }` wraps it for the VM side.

## 4. The transport object (`transport.rs`)

`NoiseTransport::new(send, recv)` from the handshake's split. It multiplexes
HTTP-like streams:

- `encrypt_http_request(method, path, body, service, headers, chunk_id)` —
  complete request with inline body (`end_body` set); returns `EncryptedFrames
  { stream_id, frames }`.
- `start_stream_request(method, path, service, headers, chunk_id)` — opens a
  stream with `end_body` unset; follow with
  `encrypt_body_chunk(stream_id, data, service, end_body, chunk_id)`.
- `encrypt_reset(stream_id, service, reason, code, chunk_id)` — cancel a
  stream.
- `decrypt_frame(ciphertext)` — returns `Ok(None)` while a message is still
  being reassembled, else `Ok(Some(DecryptedFrame))`: a `Response`,
  `BodyChunk` or `Reset` on some stream.

Rules carried over:

- Stream ids are allocated internally, 1 upward; you read them off
  `EncryptedFrames::stream_id`.
- A **request** frame arriving from the server, or a `ServiceResponse` with an
  empty payload, is a protocol error.
- Any cipher, framing or codec failure marks the transport **dead**: all later
  calls return `TransportError::Dead`. A dead transport means the nonce state
  is ambiguous — drop the connection and reconnect. (The async client should
  hold the send side behind a lock and encrypt under that lock, because
  encryption advances the nonce and ciphertexts must not be reordered.)
- Associated data is always empty for transport records; the handshake uses
  the running handshake hash as AD internally.

You pass `chunk_id` per call (draw it with `framing::random_chunk_id`); it is
shared across all frames of that one message.

## 5. Control messages (`control.rs`)

The `/link-control` stream (opened as a `start_stream_request("POST",
"/link-control")` whose body chunks carry these messages) speaks
length-prefixed JSON: a little-endian `u32` byte length, then compact JSON.

- `encode_message(&serde_json::Value)` → prefix + compact JSON, reproducing
  CPython's `json.dumps(separators=(",", ":"))` with `ensure_ascii`
  byte-for-byte for the values the protocol uses (objects in insertion order,
  non-ASCII and controls escaped `\uXXXX`, astral scalars as surrogate pairs,
  `/` not escaped).
- `MessageDecoder::feed(bytes)` → buffers and yields complete objects. Empty
  payloads are keepalives (skipped); malformed, non-UTF-8 or non-object
  messages are dropped; a length above `MAX_INBOUND_MESSAGE` (4 MiB) returns
  an error and the stream is unrecoverable — close the connection.

Message flow the async client implements on top: device→VM `link.register`
(capabilities) then a `link.result` per invoke; VM→device the register ACK,
`link.invoke` requests, and events such as `link.unpaired`.

### JSON parity

Decoding uses `serde_json`, which (unlike CPython's `json`) rejects `NaN`/
`Infinity`, lone surrogates, a leading BOM, non-UTF-8, integers beyond 64 bits,
and nesting deeper than 128. Such inputs are dropped rather than accepted; none
occurs in the real protocol. Float *formatting* in `encode_message` is not
guaranteed identical to CPython (the protocol never sends floats). The vector
dumper flags every such case `rust_deviation`.

## 6. BLE pairing framing (`ble.rs`)

Used only during pairing, over a GATT characteristic. `encode_chunks(data,
mtu)` frames each packet as `[0xFE, index, total]` + fragment, sized to the
ATT MTU (capped at 160 bytes). `ChunkAssembler::feed(packet)` reassembles
strictly in order: a packet under 3 bytes or not starting with `0xFE` is a
complete unchunked message; index 0 or a change in `total` starts a new
message; a gap, repeat, index past `total`, or exceeding `MAX_MESSAGE_BYTES`
(8192) discards the partial message. MTU quirks from the reference are kept:
MTU 4–5 yield a single empty chunk; MTU 6 is an error.

## Public API summary

Module → key items (all re-exported types also at the crate root):

- `error`: `WireError`, `NoiseError`, `FramingError`, `TransportError`,
  `ControlError`, `BleError` (all `thiserror`; messages match the reference
  where a matching condition exists).
- `wire`: `encode_varint`, `read_varint`, `encode_key`, `read_key`,
  `read_delimited`, `skip_field`, `encode_field`/`FieldValue`,
  `decode_int64`/`decode_int32`/`decode_uint32`, `WireType`.
- `envelope`: types `Header`, `ApplicationRequest`, `ApplicationResponse`,
  `BodyChunk`, `Reset`, `ResetCode`, `ServiceFrame`, `FrameBody`,
  `ServiceRequest`, `ServiceResponse`, `ServiceType`; `encode_*`/`decode_*` for
  each.
- `framing`: `NoiseTransportFrame`, `encode_noise_frame`, `decode_noise_frame`,
  `encode_noise_frames`, `random_chunk_id`, `NoiseFrameDecoder`; constants
  `MAX_CHUNK_PAYLOAD`, `MAX_TOTAL_CHUNKS`, `MAX_PENDING_ASSEMBLIES`,
  `MAX_ASSEMBLY_BYTES`, `ASSEMBLY_TTL`.
- `noise`: `CipherState`, `NoiseXXInitiator`, `NoiseXXResponder`, `KeySource`
  (`OsKeySource`, `FixedKeySource`); constants `PROTOCOL_NAME`,
  `MIN_MSG2_LEN`, `MIN_MSG3_LEN`, `MAX_SAFE_NONCE`, `LOW_ORDER_POINTS`.
- `transport`: `NoiseTransport`, `EncryptedFrames`, `DecryptedFrame`,
  `service_from_name`, `decode_request_envelope`, `encode_response_envelope`.
- `control`: `encode_message`, `MessageDecoder`, `MAX_INBOUND_MESSAGE`,
  `MAX_CONTROL_MESSAGE_BYTES`.
- `ble`: `encode_chunks`, `ChunkAssembler`, and the framing constants.

## Safety properties the async client can rely on

- No function panics or indexes out of bounds on bytes from the network; every
  decode path returns a typed error (proved by the `fuzz.rs` property tests).
- Cipher and transport state poison on any failure, so a half-advanced nonce
  is never reused. Treat a `Dead`/poisoned error as "reconnect".
- Key material is zeroized on drop where the underlying types allow it, and no
  key, shared secret or token is placed in an error or `Debug` output. Keep the
  reference's logging rule: never log Authorization headers, tokens, refused
  response bodies, or message text at info level.

## Deviations from the reference (API, not wire)

- Keys and `chunk_id`s are injected (via `KeySource` and a parameter) instead
  of drawn internally, so the crate is pure and deterministic. Production draws
  them from `OsKeySource` / `random_chunk_id`.
- `encode_message` takes a `serde_json::Value`; float formatting is not
  byte-identical to CPython (see JSON parity).
- Protobuf wire types 3/4/6/7 (groups/invalid) are unrepresentable in the typed
  `WireType`, so they are rejected at construction rather than at a runtime
  check; and out-of-range `int*`/`uint*` field values are unrepresentable in
  the typed `FieldValue`, which the reference range-checks at runtime. Behaviour
  is equivalent.
