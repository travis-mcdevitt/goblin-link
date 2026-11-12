#!/usr/bin/env python3
# Copyright 2026 the goblin-link authors.
# Licensed under the Apache License, Version 2.0.
#
# Drives the Python Muse Gadget SDK (facebookincubator/muse-gadget-sdk,
# Apache-2.0; see MUSE_SDK_LIB below) to produce
# deterministic wire-format test vectors for the Rust `muse-proto` crate.
"""Dump deterministic wire-format vectors from the Python reference SDK.

Usage (from the repository root or anywhere):

    uv run --no-project --with cryptography python \
        gadgets/goblin-link/tools/dump_vectors.py [--check]

Every key, ephemeral, chunk id and clock reading is injected, so two runs
produce byte-identical files. ``--check`` regenerates into memory and fails
if the checked-in files differ.

All key material is synthetic: each private key is SHA-256 of a fixed,
human-readable label (see ``synthetic_key``). Nothing here is a credential.

Bytes are written as lowercase hex. Payloads too large to inline are written
as a generator spec (``{"pattern": ..., "len": N}``, see ``pattern_bytes``)
and checked through SHA-256.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
# The SDK checkout's library directory (the one that contains `musegadget/`). Set
# MUSE_SDK_LIB to a checkout of facebookincubator/muse-gadget-sdk at the commit named in
# NOTICE; the default is where the author keeps one.
import os

REFERENCE_LIB = Path(
    os.environ.get("MUSE_SDK_LIB") or HERE.parents[1] / "trimui-brick" / "pak" / "lib"
)
OUT_DIR = HERE.parent / "crates" / "muse-proto" / "tests" / "vectors"

sys.path.insert(0, str(REFERENCE_LIB))

from cryptography.hazmat.primitives import serialization  # noqa: E402
from cryptography.hazmat.primitives.asymmetric import x25519  # noqa: E402

import logging  # noqa: E402

from musegadget import ble_framing  # noqa: E402
from musegadget import link_client  # noqa: E402
from musegadget.noise import _proto, envelope, framing, noise_xx, transport  # noqa: E402
from musegadget.noise.envelope import (  # noqa: E402
    ApplicationRequest,
    ApplicationResponse,
    BodyChunk,
    Header,
    Reset,
    ResetCode,
    ServiceFrame,
    ServiceRequest,
    ServiceResponse,
    ServiceType,
)

# The reference logs dropped control messages; keep the dump quiet.
logging.disable(logging.WARNING)

# Above this size a byte string is recorded as length + SHA-256 only.
INLINE_LIMIT = 4096


# --------------------------------------------------------------------------
# Helpers
# --------------------------------------------------------------------------


def h(data: bytes) -> str:
    return bytes(data).hex()


def sha(data: bytes) -> str:
    return hashlib.sha256(bytes(data)).hexdigest()


def blob(data: bytes) -> dict:
    """Inline small byte strings; summarise large ones by length and hash."""
    data = bytes(data)
    if len(data) <= INLINE_LIMIT:
        return {"hex": h(data)}
    return {"len": len(data), "sha256": sha(data)}


def pattern_bytes(length: int, seed: int = 0) -> bytes:
    """Deterministic filler: byte i is (i * 31 + seed * 7 + 11) mod 256."""
    return bytes(((i * 31 + seed * 7 + 11) & 0xFF) for i in range(length))


def pattern_spec(length: int, seed: int = 0) -> dict:
    return {"pattern": "affine31", "len": length, "seed": seed}


def synthetic_key(label: str) -> bytes:
    return hashlib.sha256(b"muse-proto synthetic test key: " + label.encode()).digest()


def pub_of(private: bytes) -> bytes:
    key = x25519.X25519PrivateKey.from_private_bytes(private)
    return key.public_key().public_bytes(
        encoding=serialization.Encoding.Raw, format=serialization.PublicFormat.Raw
    )


def err(fn, *args, **kwargs) -> str:
    """Run ``fn`` expecting an exception; return its message."""
    try:
        fn(*args, **kwargs)
    except Exception as exc:  # noqa: BLE001 - we record any failure verbatim
        return str(exc)
    raise AssertionError(f"{fn.__name__}{args} did not raise")


class KeyQueue:
    """Replaces the reference's X25519 key generation with fixed keys."""

    def __init__(self) -> None:
        self.keys: list[bytes] = []

    def push(self, *keys: bytes) -> None:
        self.keys.extend(keys)

    def generate(self):
        if not self.keys:
            raise AssertionError("key queue exhausted")
        private = self.keys.pop(0)
        key = x25519.X25519PrivateKey.from_private_bytes(private)
        return noise_xx._X25519KeyPair(private_key=key, public_key_bytes=pub_of(private))


class ChunkIds:
    """Replaces framing._random_int64 with a scripted sequence."""

    def __init__(self) -> None:
        self.ids: list[int] = []

    def push(self, *ids: int) -> None:
        self.ids.extend(ids)

    def next(self) -> int:
        if not self.ids:
            raise AssertionError("chunk id queue exhausted")
        return self.ids.pop(0)


class FakeClock:
    def __init__(self) -> None:
        self.now = 1_700_000_000.0

    def time(self) -> float:
        return self.now


KEYS = KeyQueue()
CHUNK_IDS = ChunkIds()
CLOCK = FakeClock()
noise_xx._generate_x25519_key_pair = KEYS.generate
framing._random_int64 = CHUNK_IDS.next
# framing calls time.time(); give it a private clock without touching the
# process-wide time module.
framing.time = types.SimpleNamespace(time=CLOCK.time)


def chunk_id_rng_bytes(chunk_id: int) -> str:
    """The 8 bytes the Rust port must draw to produce ``chunk_id``.

    The Rust port reads 8 random bytes as a little-endian i64.
    """
    return h((chunk_id & ((1 << 64) - 1)).to_bytes(8, "little"))


# --------------------------------------------------------------------------
# JSON views of envelope values (bytes as hex)
# --------------------------------------------------------------------------


def header_json(header: Header) -> dict:
    return {"key": header.key, "value": header.value}


def value_json(kind, value) -> dict | None:
    if value is None:
        return None
    if kind == "request":
        return {
            "verb": value.verb,
            "path": value.path,
            "headers": [header_json(x) for x in value.headers],
            "body": blob(value.body),
            "end_body": value.end_body,
        }
    if kind == "response":
        return {
            "status": value.status,
            "headers": [header_json(x) for x in value.headers],
            "body": blob(value.body),
            "end_body": value.end_body,
        }
    if kind == "body_chunk":
        return {"data": blob(value.data), "end_body": value.end_body}
    if kind == "reset":
        return {"code": int(value.code), "reason": value.reason}
    raise AssertionError(kind)


def frame_json(frame: ServiceFrame) -> dict:
    return {
        "stream_id": frame.stream_id,
        "kind": frame.kind,
        "value": value_json(frame.kind, frame.value),
    }


# --------------------------------------------------------------------------
# wire.json: noise/_proto.py
# --------------------------------------------------------------------------


def dump_wire() -> dict:
    varint_values = [0, 1, 2, 127, 128, 255, 300, 16383, 16384, (1 << 32) - 1, 1 << 32,
                     (1 << 63) - 1, 1 << 63, (1 << 64) - 1]
    varints = [{"value": str(v), "hex": h(_proto.encode_varint(v))} for v in varint_values]
    varint_errors = [
        {"value": "-1", "error": err(_proto.encode_varint, -1)},
        {"value": str(1 << 64), "error": err(_proto.encode_varint, 1 << 64)},
    ]

    read_varint_inputs = [
        ("00", 0), ("7f", 0), ("8001", 0), ("ac02", 0), ("ff01", 0),
        ("ffffffffffffffffff01", 0),          # 2^64-1, ten bytes
        ("80808080808080808001", 0),          # 2^63
        ("00ac02", 1),                        # offset inside buffer
        ("ac02ff", 0),                        # trailing data ignored
        ("8080808080808080808000", 0),        # 11 bytes (10th has continuation)
        ("ffffffffffffffffff02", 0),          # 10th byte > 1
        ("ffffffffffffffffff81", 0),          # 10th byte has continuation
        ("80", 0), ("", 0), ("ff", 0), ("01", 1),
        ("8000", 0),                          # non-minimal encoding of 0 is accepted
    ]
    read_varint = []
    for hexstr, offset in read_varint_inputs:
        data = bytes.fromhex(hexstr)
        try:
            value, end = _proto.read_varint(data, offset)
            read_varint.append({"hex": hexstr, "offset": offset, "value": str(value), "end": end})
        except _proto.ProtoError as exc:
            read_varint.append({"hex": hexstr, "offset": offset, "error": str(exc)})

    keys = []
    for field, wire in [(1, 0), (1, 2), (2, 5), (15, 1), (16, 0), (18999, 2), (20000, 0),
                        ((1 << 29) - 1, 2)]:
        keys.append({"field": field, "wire": wire, "hex": h(_proto.encode_key(field, wire))})
    key_errors = []
    for field, wire in [(0, 0), (19000, 0), (19999, 2), (1 << 29, 0), (1, 3), (1, 4), (1, 6),
                        (1, 7)]:
        key_errors.append({"field": field, "wire": wire, "error": err(_proto.encode_key, field, wire)})

    read_key = []
    for hexstr in ["08", "12", "2d", "79", "8001", "f8ff7f",  # field 1/2/5/15/16 and 2^... cases
                   "00", "0b", "0c", "0e", "0f", "c0b80901", "f8e10901", "8080808010", "80"]:
        data = bytes.fromhex(hexstr)
        try:
            field, wire, end = _proto.read_key(data, 0)
            read_key.append({"hex": hexstr, "field": field, "wire": wire, "end": end})
        except _proto.ProtoError as exc:
            read_key.append({"hex": hexstr, "error": str(exc)})

    fields = [
        {"fn": "varint", "field": 1, "value": "150", "hex": h(_proto.varint_field(1, 150))},
        {"fn": "int64", "field": 1, "value": "-1", "hex": h(_proto.int64_field(1, -1))},
        {"fn": "int64", "field": 1, "value": str(-(1 << 63)), "hex": h(_proto.int64_field(1, -(1 << 63)))},
        {"fn": "int64", "field": 1, "value": str((1 << 63) - 1), "hex": h(_proto.int64_field(1, (1 << 63) - 1))},
        {"fn": "int32", "field": 1, "value": "-1", "hex": h(_proto.int32_field(1, -1))},
        {"fn": "int32", "field": 1, "value": "404", "hex": h(_proto.int32_field(1, 404))},
        {"fn": "int32", "field": 1, "value": str(-(1 << 31)), "hex": h(_proto.int32_field(1, -(1 << 31)))},
        {"fn": "uint32", "field": 3, "value": str(0xFFFFFFFF), "hex": h(_proto.uint32_field(3, 0xFFFFFFFF))},
        {"fn": "bool", "field": 5, "value": "true", "hex": h(_proto.bool_field(5, True))},
        {"fn": "bool", "field": 5, "value": "false", "hex": h(_proto.bool_field(5, False))},
        {"fn": "string", "field": 2, "value": "/link-control", "hex": h(_proto.string_field(2, "/link-control"))},
        {"fn": "string", "field": 2, "value": "héllo ☃ \U0001f600", "hex": h(_proto.string_field(2, "héllo ☃ \U0001f600"))},
        {"fn": "string", "field": 2, "value": "", "hex": h(_proto.string_field(2, ""))},
        {"fn": "bytes", "field": 4, "value": "00ff10", "hex": h(_proto.bytes_field(4, b"\x00\xff\x10"))},
        {"fn": "bytes", "field": 4, "value": h(pattern_bytes(200)), "hex": h(_proto.bytes_field(4, pattern_bytes(200)))},
    ]
    field_errors = [
        {"fn": "int64", "value": str(1 << 63), "error": err(_proto.int64_field, 1, 1 << 63)},
        {"fn": "int32", "value": str(1 << 31), "error": err(_proto.int32_field, 1, 1 << 31)},
        {"fn": "uint32", "value": "-1", "error": err(_proto.uint32_field, 1, -1)},
        {"fn": "uint32", "value": str(1 << 32), "error": err(_proto.uint32_field, 1, 1 << 32)},
    ]

    decode_ints = []
    for raw in [0, 1, (1 << 31) - 1, 1 << 31, (1 << 63) - 1, 1 << 63, (1 << 64) - 1,
                (1 << 64) - (1 << 31), (1 << 64) - (1 << 31) - 1, 0xFFFFFFFF, 1 << 32]:
        entry = {"raw": str(raw)}
        for name, fn in [("int64", _proto.decode_int64), ("int32", _proto.decode_int32),
                         ("uint32", _proto.decode_uint32)]:
            try:
                entry[name] = str(fn(raw))
            except _proto.ProtoError as exc:
                entry[name + "_error"] = str(exc)
        decode_ints.append(entry)

    read_delimited = []
    for hexstr in ["03616263", "00", "0361", "", "ffffffffffffffffff0100", "03616263ff"]:
        data = bytes.fromhex(hexstr)
        try:
            value, end = _proto.read_delimited(data, 0)
            read_delimited.append({"hex": hexstr, "value": h(value), "end": end})
        except _proto.ProtoError as exc:
            read_delimited.append({"hex": hexstr, "error": str(exc)})

    skip = []
    for hexstr, wire in [("96", 0), ("9601", 0), ("0102030405060708", 1), ("01020304050607", 1),
                         ("01020304", 5), ("010203", 5), ("026869", 2), ("0568", 2), ("00", 3),
                         ("00", 4), ("00", 6), ("00", 7)]:
        data = bytes.fromhex(hexstr)
        try:
            end = _proto.skip_field(data, 0, wire)
            skip.append({"hex": hexstr, "wire": wire, "end": end})
        except _proto.ProtoError as exc:
            skip.append({"hex": hexstr, "wire": wire, "error": str(exc)})

    return {
        "constants": {
            "MAX_FIELD_NUMBER": _proto.MAX_FIELD_NUMBER,
            "FIRST_RESERVED_FIELD_NUMBER": _proto.FIRST_RESERVED_FIELD_NUMBER,
            "LAST_RESERVED_FIELD_NUMBER": _proto.LAST_RESERVED_FIELD_NUMBER,
        },
        "encode_varint": varints,
        "encode_varint_errors": varint_errors,
        "read_varint": read_varint,
        "encode_key": keys,
        "encode_key_errors": key_errors,
        "read_key": read_key,
        "fields": fields,
        "field_errors": field_errors,
        "decode_ints": decode_ints,
        "read_delimited": read_delimited,
        "skip_field": skip,
    }


# --------------------------------------------------------------------------
# envelope.json: noise/envelope.py
# --------------------------------------------------------------------------

SAMPLE_HEADERS = [Header("Content-Type", "application/json"), Header("x-app-id", "musegadget")]


def sample_frames() -> list[tuple[str, ServiceFrame]]:
    return [
        ("empty", ServiceFrame()),
        ("stream_only", ServiceFrame(stream_id=7)),
        ("negative_stream", ServiceFrame(stream_id=-5)),
        ("request_minimal", ServiceFrame.request(1, ApplicationRequest())),
        ("request_get", ServiceFrame.request(1, ApplicationRequest(verb="GET", path="/identity", end_body=True))),
        ("request_full", ServiceFrame.request(42, ApplicationRequest(
            verb="POST", path="/chat/stream", headers=SAMPLE_HEADERS,
            body=b'{"message":"synthetic marker"}', end_body=True))),
        ("request_stream_open", ServiceFrame.request(3, ApplicationRequest(verb="POST", path="/link-control"))),
        ("request_empty_header", ServiceFrame.request(2, ApplicationRequest(verb="GET", path="/", headers=[Header()]))),
        ("request_unicode", ServiceFrame.request(9, ApplicationRequest(
            verb="GET", path="/s?q=café", headers=[Header("x-n", "☃")]))),
        ("response_ok", ServiceFrame.response(1, ApplicationResponse(status=200, body=b"{}", end_body=True))),
        ("response_headers", ServiceFrame.response(5, ApplicationResponse(
            status=206, headers=[Header("content-type", "audio/mpeg")]))),
        ("response_negative_status", ServiceFrame.response(5, ApplicationResponse(status=-1))),
        ("response_zero", ServiceFrame.response(5, ApplicationResponse())),
        ("body_chunk", ServiceFrame.body_chunk(3, BodyChunk(data=b"\x05\x00\x00\x00hello"))),
        ("body_chunk_end", ServiceFrame.body_chunk(3, BodyChunk(data=b"", end_body=True))),
        ("body_chunk_empty", ServiceFrame.body_chunk(3, BodyChunk())),
        ("reset_default", ServiceFrame.reset(4, Reset())),
        ("reset_cancelled", ServiceFrame.reset(4, Reset(code=ResetCode.CANCELLED, reason="consumer closed"))),
        ("reset_service_unavailable", ServiceFrame.reset(4, Reset(code=ResetCode.SERVICE_UNAVAILABLE))),
        ("big_stream_id", ServiceFrame.body_chunk((1 << 63) - 1, BodyChunk(data=b"x"))),
        ("min_stream_id", ServiceFrame.body_chunk(-(1 << 63), BodyChunk(data=b"x"))),
        ("large_body", ServiceFrame.request(8, ApplicationRequest(
            verb="PUT", path="/f", body=pattern_bytes(300, 1), end_body=True))),
    ]


def dump_envelope() -> dict:
    headers = [{"key": x.key, "value": x.value, "hex": h(envelope.encode_header(x))}
               for x in [Header(), Header("a", ""), Header("", "b"), *SAMPLE_HEADERS]]

    service_frames = []
    for name, frame in sample_frames():
        encoded = envelope.encode_service_frame(frame)
        decoded = envelope.decode_service_frame(encoded)
        service_frames.append({
            "name": name,
            "frame": frame_json(frame),
            "hex": h(encoded),
            "decoded": frame_json(decoded),
        })

    service_requests = []
    for service in ServiceType:
        for payload in [b"", b"\x08\x01"]:
            req = ServiceRequest(service=service, payload=payload)
            service_requests.append({"service": int(service), "payload": h(payload),
                                     "hex": h(envelope.encode_service_request(req))})
    service_responses = [{"payload": h(p), "hex": h(envelope.encode_service_response(ServiceResponse(p)))}
                         for p in [b"", b"\x08\x01", pattern_bytes(130)]]

    def decode_case(name, fn, hexstr):
        data = bytes.fromhex(hexstr)
        try:
            value = fn(data)
        except _proto.ProtoError as exc:
            return {"name": name, "hex": hexstr, "error": str(exc)}
        if isinstance(value, ServiceFrame):
            out = frame_json(value)
        elif isinstance(value, ServiceRequest):
            out = {"service": int(value.service), "payload": h(value.payload)}
        elif isinstance(value, ServiceResponse):
            out = {"payload": h(value.payload)}
        elif isinstance(value, Header):
            out = header_json(value)
        elif isinstance(value, ApplicationRequest):
            out = value_json("request", value)
        elif isinstance(value, ApplicationResponse):
            out = value_json("response", value)
        elif isinstance(value, BodyChunk):
            out = value_json("body_chunk", value)
        elif isinstance(value, Reset):
            out = value_json("reset", value)
        else:
            raise AssertionError(type(value))
        return {"name": name, "hex": hexstr, "value": out}

    D = envelope
    decode = {
        "service_frame": [
            decode_case("empty", D.decode_service_frame, ""),
            decode_case("unknown_varint_field", D.decode_service_frame, "0807" "3001"),
            decode_case("unknown_fixed64", D.decode_service_frame, "0807" "310102030405060708"),
            decode_case("unknown_fixed32", D.decode_service_frame, "0807" "3501020304"),
            decode_case("unknown_delimited", D.decode_service_frame, "0807" "3a03616263"),
            decode_case("last_oneof_wins", D.decode_service_frame, "0801" "2200" "2a00"),
            decode_case("repeated_stream_id", D.decode_service_frame, "0801" "0802"),
            decode_case("stream_id_wrong_wire", D.decode_service_frame, "0a00"),
            decode_case("request_wrong_wire", D.decode_service_frame, "1000"),
            decode_case("response_wrong_wire", D.decode_service_frame, "1800"),
            decode_case("body_chunk_wrong_wire", D.decode_service_frame, "2000"),
            decode_case("reset_wrong_wire", D.decode_service_frame, "2800"),
            decode_case("truncated_key", D.decode_service_frame, "80"),
            decode_case("truncated_delimited", D.decode_service_frame, "1205"),
            decode_case("group_wire", D.decode_service_frame, "0b"),
            decode_case("field_zero", D.decode_service_frame, "00"),
            decode_case("reserved_field", D.decode_service_frame, "c0b80900"),
            decode_case("bad_utf8_verb", D.decode_service_frame, "1203" "0a01ff"),
            decode_case("bad_header_nested", D.decode_service_frame, "1204" "1a02" "0800"),
            decode_case("response_status_int32_overflow", D.decode_service_frame, "1a06" "0880808080 08".replace(" ", "")),
            decode_case("response_status_negative", D.decode_service_frame, "1a0b" "08ffffffffffffffffff01"),
            decode_case("response_status_bad_wire", D.decode_service_frame, "1a02" "0a00"),
            decode_case("reset_unknown_code", D.decode_service_frame, "2a02" "0807"),
            decode_case("reset_negative_code", D.decode_service_frame, "2a0b" "08ffffffffffffffffff01"),
            decode_case("reset_reason_bad_utf8", D.decode_service_frame, "2a03" "1201c3"),
            decode_case("body_end_nonzero", D.decode_service_frame, "2202" "1005"),
            decode_case("body_end_wrong_wire", D.decode_service_frame, "2202" "1200"),
            decode_case("body_data_wrong_wire", D.decode_service_frame, "2202" "0800"),
            decode_case("stream_id_max_u64", D.decode_service_frame, "08ffffffffffffffffff01"),
            decode_case("malformed_varint", D.decode_service_frame, "08ffffffffffffffffff02"),
        ],
        "service_request": [
            decode_case("empty", D.decode_service_request, ""),
            decode_case("sentinel", D.decode_service_request, "0801"),
            decode_case("authd_payload", D.decode_service_request, "0803" "12020801"),
            decode_case("unknown_service", D.decode_service_request, "0804"),
            decode_case("service_wrong_wire", D.decode_service_request, "0a00"),
            decode_case("payload_wrong_wire", D.decode_service_request, "1000"),
            decode_case("unknown_field", D.decode_service_request, "1801" "1200"),
        ],
        "service_response": [
            decode_case("empty", D.decode_service_response, ""),
            decode_case("payload", D.decode_service_response, "0a020801"),
            decode_case("payload_wrong_wire", D.decode_service_response, "0800"),
            decode_case("unknown_field", D.decode_service_response, "1001" "0a00"),
            decode_case("truncated", D.decode_service_response, "0a05"),
        ],
        "header": [
            decode_case("full", D.decode_header, "0a016b1201 76".replace(" ", "")),
            decode_case("key_wrong_wire", D.decode_header, "0800"),
            decode_case("value_wrong_wire", D.decode_header, "1000"),
            decode_case("repeated_key", D.decode_header, "0a01610a0162"),
        ],
        "application_request": [
            decode_case("verb_wrong_wire", D.decode_application_request, "0800"),
            decode_case("path_wrong_wire", D.decode_application_request, "1000"),
            decode_case("headers_wrong_wire", D.decode_application_request, "1800"),
            decode_case("body_wrong_wire", D.decode_application_request, "2000"),
            decode_case("end_wrong_wire", D.decode_application_request, "2a00"),
            decode_case("two_bodies_last_wins", D.decode_application_request, "220161" "220162"),
        ],
        "application_response": [
            decode_case("headers_wrong_wire", D.decode_application_response, "1000"),
            decode_case("body_wrong_wire", D.decode_application_response, "1800"),
            decode_case("end_wrong_wire", D.decode_application_response, "2200"),
        ],
        "body_chunk": [
            decode_case("ok", D.decode_body_chunk, "0a0161" "1001"),
        ],
        "reset": [
            decode_case("code_wrong_wire", D.decode_reset, "0a00"),
            decode_case("reason_wrong_wire", D.decode_reset, "1000"),
            decode_case("code_int32_overflow", D.decode_reset, "088080808008"),
        ],
    }

    return {
        "headers": headers,
        "service_frames": service_frames,
        "service_requests": service_requests,
        "service_responses": service_responses,
        "decode": decode,
    }


# --------------------------------------------------------------------------
# framing.json: noise/framing.py
# --------------------------------------------------------------------------


def frame_header_and_hash(frame: bytes, payload_len: int) -> dict:
    """Header bytes before the payload, plus the full frame's hash."""
    header_len = len(frame) - payload_len
    return {"header_hex": h(frame[:header_len]), "len": len(frame), "sha256": sha(frame)}


def dump_framing() -> dict:
    NF = framing.NoiseTransportFrame
    frames = []
    for name, frame in [
        ("default", NF()),
        ("zero_total", NF(total_chunks=0)),
        ("id_only", NF(chunk_id=1)),
        ("negative_id", NF(chunk_id=-2, chunk_index=1, total_chunks=2, payload=b"ab")),
        ("max_index", NF(chunk_id=5, chunk_index=0xFFFFFFFF, total_chunks=0xFFFFFFFF)),
        ("payload", NF(chunk_id=1234567890123, payload=b"\x0a\x00")),
    ]:
        frames.append({"name": name, "chunk_id": frame.chunk_id, "chunk_index": frame.chunk_index,
                       "total_chunks": frame.total_chunks, "payload": h(frame.payload),
                       "hex": h(framing.encode_noise_frame(frame))})

    decode_frames = []
    for name, hexstr in [
        ("empty", ""), ("explicit_total_zero", "1800"), ("defaults_total_one", "0801"),
        ("chunk_index_too_big", "10808080801001"), ("total_wrong_wire", "1a00"),
        ("payload_wrong_wire", "2000"), ("id_wrong_wire", "0a00"), ("index_wrong_wire", "1200"),
        ("unknown_skipped", "2803" "220161"), ("max_u32_index", "10ffffffff0f"),
        ("over_u32_index", "1080808080 10".replace(" ", "")),
    ]:
        try:
            f = framing.decode_noise_frame(bytes.fromhex(hexstr))
            decode_frames.append({"name": name, "hex": hexstr, "chunk_id": f.chunk_id,
                                  "chunk_index": f.chunk_index, "total_chunks": f.total_chunks,
                                  "payload": h(f.payload)})
        except _proto.ProtoError as exc:
            decode_frames.append({"name": name, "hex": hexstr, "error": str(exc)})

    M = framing.MAX_CHUNK_PAYLOAD
    encode_frames = []
    for name, length, chunk_id in [
        ("empty", 0, 77), ("one_byte", 1, 1), ("small", 100, -9), ("exact_one_chunk", M, 3),
        ("one_over", M + 1, 4), ("three_chunks", 2 * M + 10, -(1 << 63)), ("zero_id", 10, 0),
        ("max_chunks", framing.MAX_TOTAL_CHUNKS * M, 99),
    ]:
        payload = pattern_bytes(length, 2)
        out = framing.encode_noise_frames(payload, chunk_id=chunk_id)
        entry = {"name": name, "payload": pattern_spec(length, 2), "chunk_id": chunk_id,
                 "count": len(out)}
        # Keep the file small: record every frame for small cases, first/last
        # two for the max case (all others are the same shape).
        indices = range(len(out)) if len(out) <= 4 else [0, 1, len(out) - 2, len(out) - 1]
        recorded = []
        for i in indices:
            fragment_len = len(payload[i * M:(i + 1) * M])
            item = frame_header_and_hash(out[i], fragment_len)
            item["index"] = i
            if len(out[i]) <= INLINE_LIMIT:
                item["hex"] = h(out[i])
            recorded.append(item)
        entry["frames"] = recorded
        entry["all_frames_sha256"] = sha(b"".join(out))
        encode_frames.append(entry)

    too_large_len = framing.MAX_TOTAL_CHUNKS * M + 1
    too_large_error = err(framing.encode_noise_frames, pattern_bytes(too_large_len), 1)

    # Random chunk id path: the reference draws a random int64.
    CHUNK_IDS.push(-1234)
    random_id_frames = framing.encode_noise_frames(b"hi")
    random_id = {"rng_bytes": chunk_id_rng_bytes(-1234), "payload": h(b"hi"),
                 "frames": [h(x) for x in random_id_frames]}

    return {
        "constants": {
            "MAX_CHUNK_PAYLOAD": framing.MAX_CHUNK_PAYLOAD,
            "MAX_PENDING_ASSEMBLIES": framing.MAX_PENDING_ASSEMBLIES,
            "MAX_TOTAL_CHUNKS": framing.MAX_TOTAL_CHUNKS,
            "MAX_ASSEMBLY_BYTES": framing.MAX_ASSEMBLY_BYTES,
            "ASSEMBLY_TTL_SECONDS": framing.ASSEMBLY_TTL_SECONDS,
        },
        "encode_frame": frames,
        "decode_frame": decode_frames,
        "encode_frames": encode_frames,
        "encode_frames_too_large": {"len": too_large_len, "error": too_large_error},
        "encode_frames_random_id": random_id,
        "decoder_scenarios": dump_decoder_scenarios(),
    }


def nf(chunk_id: int, index: int, total: int, payload: bytes) -> bytes:
    return framing.encode_noise_frame(framing.NoiseTransportFrame(chunk_id, index, total, payload))


def dump_decoder_scenarios() -> list:
    """Each scenario feeds one NoiseFrameDecoder; ``advance`` moves the clock first."""
    M = framing.MAX_CHUNK_PAYLOAD
    scenarios: list[tuple[str, list]] = []

    scenarios.append(("single", [(0, nf(1, 0, 1, b"hello"))]))
    scenarios.append(("empty_payload", [(0, nf(1, 0, 1, b""))]))
    scenarios.append(("default_fields", [(0, b"")]))
    scenarios.append(("in_order", [(0, nf(5, 0, 3, b"aa")), (0, nf(5, 1, 3, b"bb")), (0, nf(5, 2, 3, b"cc"))]))
    scenarios.append(("out_of_order", [(0, nf(5, 2, 3, b"cc")), (0, nf(5, 0, 3, b"aa")), (0, nf(5, 1, 3, b"bb"))]))
    scenarios.append(("interleaved", [(0, nf(1, 0, 2, b"a1")), (0, nf(2, 0, 2, b"b1")),
                                      (0, nf(2, 1, 2, b"b2")), (0, nf(1, 1, 2, b"a2"))]))
    scenarios.append(("reuse_id_after_complete", [(0, nf(1, 0, 1, b"x")), (0, nf(1, 0, 1, b"y"))]))
    scenarios.append(("duplicate_index", [(0, nf(1, 0, 2, b"a")), (0, nf(1, 0, 2, b"a")), (0, nf(2, 0, 1, b"z"))]))
    scenarios.append(("inconsistent_total", [(0, nf(1, 0, 3, b"a")), (0, nf(1, 1, 2, b"b"))]))
    scenarios.append(("total_zero", [(0, nf(1, 0, 0, b"a"))]))
    scenarios.append(("total_too_big", [(0, nf(1, 0, 257, b"a"))]))
    scenarios.append(("total_max_ok", [(0, nf(1, 255, 256, b"a"))]))
    scenarios.append(("index_out_of_range", [(0, nf(1, 2, 2, b"a"))]))
    scenarios.append(("payload_too_large", [(0, nf(1, 0, 1, b"\x00" * (M + 1)))]))
    scenarios.append(("payload_max_ok", [(0, nf(1, 0, 1, pattern_bytes(M, 3)))]))
    scenarios.append(("malformed_proto", [(0, b"\x80")]))
    scenarios.append(("poisoned_after_error", [(0, b"\x80"), (0, nf(1, 0, 1, b"ok"))]))
    pending = [(0, nf(100 + i, 0, 2, b"p")) for i in range(framing.MAX_PENDING_ASSEMBLIES)]
    scenarios.append(("too_many_pending", pending + [(0, nf(999, 0, 2, b"p"))]))
    scenarios.append(("pending_limit_existing_ok", pending + [(0, nf(100, 1, 2, b"q"))]))
    scenarios.append(("ttl_not_expired", [(0, nf(1, 0, 2, b"a")), (60, nf(1, 1, 2, b"b"))]))
    scenarios.append(("ttl_expired_restarts", [(0, nf(1, 0, 2, b"a")), (60.5, nf(1, 1, 2, b"b")),
                                               (0, nf(1, 0, 2, b"c"))]))
    scenarios.append(("ttl_frees_pending_slots", pending + [(61, nf(999, 0, 1, b"fresh"))]))
    scenarios.append(("ttl_from_creation_not_update", [(0, nf(1, 0, 3, b"a")), (40, nf(1, 1, 3, b"b")),
                                                       (21, nf(1, 2, 3, b"c"))]))

    out = []
    for name, steps in scenarios:
        decoder = framing.NoiseFrameDecoder()
        CLOCK.now = 1_700_000_000.0
        recorded = []
        for advance, data in steps:
            CLOCK.now += advance
            step = {"advance": advance, "frame": blob(data)}
            if len(data) > INLINE_LIMIT:
                # payload_max_ok / payload_too_large: rebuild in Rust from spec
                step["frame"]["rebuild"] = rebuild_spec(name)
            try:
                result = decoder.decode(data)
                step["result"] = None if result is None else blob(result)
            except Exception as exc:  # noqa: BLE001
                step["error"] = str(exc)
            recorded.append(step)
        out.append({"name": name, "steps": recorded})
    return out


def rebuild_spec(name: str) -> dict:
    M = framing.MAX_CHUNK_PAYLOAD
    if name == "payload_max_ok":
        return {"chunk_id": 1, "chunk_index": 0, "total_chunks": 1, "payload": pattern_spec(M, 3)}
    if name == "payload_too_large":
        return {"chunk_id": 1, "chunk_index": 0, "total_chunks": 1, "payload": {"zeros": M + 1}}
    raise AssertionError(name)


# --------------------------------------------------------------------------
# noise.json: noise/noise_xx.py
# --------------------------------------------------------------------------


def run_handshake(name: str, payload: bytes) -> dict:
    ie = synthetic_key(f"{name}/initiator/e")
    is_ = synthetic_key(f"{name}/initiator/s")
    re = synthetic_key(f"{name}/responder/e")
    rs = synthetic_key(f"{name}/responder/s")

    initiator = noise_xx.NoiseXXInitiator()
    responder = noise_xx.NoiseXXResponder(payload=payload)
    initiator.initialize()
    responder.initialize()
    h0 = initiator.handshake_hash()

    KEYS.push(ie)
    msg1 = initiator.write_message1()
    h1 = initiator.handshake_hash()
    KEYS.push(re, rs)
    msg2 = responder.read_message1_and_write_message2(msg1)
    got_payload = initiator.read_message2(msg2)
    assert got_payload == payload
    h2 = initiator.handshake_hash()
    KEYS.push(is_)
    msg3 = initiator.write_message3()
    responder.read_message3(msg3)
    h3 = initiator.handshake_hash()
    assert h3 == responder.handshake_hash()
    remote_static = initiator.remote_static_public_key()
    i_send, i_recv = initiator.split()
    r_send, r_recv = responder.split()

    transcript = []
    # Interleave directions, with and without associated data; include an
    # empty plaintext and one larger than a block.
    plan = [
        ("i2r", b"", b"first"),
        ("i2r", b"", b""),
        ("r2i", b"", b"reply one"),
        ("i2r", b"ad-bytes", b"with associated data"),
        ("r2i", b"", pattern_bytes(1000, 4)),
        ("r2i", b"x", b"third"),
        ("i2r", b"", pattern_bytes(65532 - 16, 5)),
        ("r2i", b"", b"done"),
    ]
    for direction, ad, plaintext in plan:
        if direction == "i2r":
            ct = i_send.encrypt_with_ad(ad, plaintext)
            assert r_recv.decrypt_with_ad(ad, ct) == plaintext
        else:
            ct = r_send.encrypt_with_ad(ad, plaintext)
            assert i_recv.decrypt_with_ad(ad, ct) == plaintext
        entry = {"dir": direction, "ad": h(ad), "ciphertext": blob(ct)}
        if len(plaintext) <= INLINE_LIMIT:
            entry["plaintext"] = {"hex": h(plaintext)}
        else:
            entry["plaintext"] = pattern_spec(len(plaintext), 5)
        transcript.append(entry)

    return {
        "name": name,
        "initiator_ephemeral_private": h(ie),
        "initiator_ephemeral_public": h(pub_of(ie)),
        "initiator_static_private": h(is_),
        "initiator_static_public": h(pub_of(is_)),
        "responder_ephemeral_private": h(re),
        "responder_ephemeral_public": h(pub_of(re)),
        "responder_static_private": h(rs),
        "responder_static_public": h(pub_of(rs)),
        "responder_payload": h(payload),
        "h_initialized": h(h0),
        "msg1": h(msg1),
        "h_after_msg1": h(h1),
        "msg2": h(msg2),
        "h_after_msg2": h(h2),
        "msg3": h(msg3),
        "h_after_msg3": h(h3),
        "remote_static": h(remote_static),
        "transport": transcript,
    }


def handshake_errors() -> list:
    out = []
    low_order = [h(p) for p in noise_xx.X25519_LOW_ORDER_POINTS]

    def fresh_initiator(name: str):
        ini = noise_xx.NoiseXXInitiator()
        ini.initialize()
        KEYS.push(synthetic_key(f"{name}/initiator/e"))
        msg1 = ini.write_message1()
        return ini, msg1

    # msg2 too short
    ini, _ = fresh_initiator("err-short")
    msg2 = bytes(noise_xx.MIN_MSG2_LEN - 1)
    out.append({"name": "msg2_too_short", "msg2": h(msg2), "error": err(ini.read_message2, msg2),
                "then_write_message3_error": err(ini.write_message3)})

    # msg2 whose ephemeral is each low-order point
    for i, point in enumerate(noise_xx.X25519_LOW_ORDER_POINTS):
        ini, _ = fresh_initiator(f"err-low-{i}")
        msg2 = point + bytes(noise_xx.MIN_MSG2_LEN - 32)
        out.append({"name": f"msg2_low_order_{i}", "initiator_e": h(synthetic_key(f"err-low-{i}/initiator/e")),
                    "msg2": h(msg2), "error": err(ini.read_message2, msg2)})

    # tampered msg2
    name = "err-tamper"
    ini = noise_xx.NoiseXXInitiator()
    ini.initialize()
    KEYS.push(synthetic_key(f"{name}/initiator/e"))
    msg1 = ini.write_message1()
    resp = noise_xx.NoiseXXResponder(b"payload")
    resp.initialize()
    KEYS.push(synthetic_key(f"{name}/responder/e"), synthetic_key(f"{name}/responder/s"))
    msg2 = bytearray(resp.read_message1_and_write_message2(msg1))
    msg2[40] ^= 0x01
    out.append({"name": "msg2_tampered_static", "initiator_e": h(synthetic_key(f"{name}/initiator/e")),
                "msg2": h(bytes(msg2)), "error": err(ini.read_message2, bytes(msg2))})

    # wrong-phase calls
    ini = noise_xx.NoiseXXInitiator()
    out.append({"name": "write1_before_initialize", "error": err(ini.write_message1)})
    out.append({"name": "split_before_handshake", "error": err(ini.split)})
    ini.initialize()
    out.append({"name": "double_initialize", "error": err(ini.initialize)})
    ini.destroy()
    out.append({"name": "write1_after_destroy", "error": err(ini.write_message1)})

    # responder side
    resp = noise_xx.NoiseXXResponder()
    resp.initialize()
    out.append({"name": "msg1_too_short", "msg1": h(b"\x01" * 31),
                "error": err(resp.read_message1_and_write_message2, b"\x01" * 31)})
    resp = noise_xx.NoiseXXResponder()
    resp.initialize()
    out.append({"name": "msg1_low_order", "msg1": h(noise_xx.X25519_LOW_ORDER_POINTS[1]),
                "responder_e": h(synthetic_key("err-r-low/responder/e")),
                "error": (KEYS.push(synthetic_key("err-r-low/responder/e")),
                          err(resp.read_message1_and_write_message2, noise_xx.X25519_LOW_ORDER_POINTS[1]))[1]})
    KEYS.keys.clear()
    resp = noise_xx.NoiseXXResponder()
    resp.initialize()
    KEYS.push(synthetic_key("err-r3/responder/e"), synthetic_key("err-r3/responder/s"))
    ini = noise_xx.NoiseXXInitiator()
    ini.initialize()
    KEYS.keys.insert(0, synthetic_key("err-r3/initiator/e"))
    m1 = ini.write_message1()
    resp.read_message1_and_write_message2(m1)
    out.append({"name": "msg3_too_short", "msg3": h(bytes(noise_xx.MIN_MSG3_LEN - 1)),
                "error": err(resp.read_message3, bytes(noise_xx.MIN_MSG3_LEN - 1))})
    return {"low_order_points": low_order, "cases": out}


def dump_noise() -> dict:
    nonce_ivs = [{"nonce": str(n), "hex": h(noise_xx.build_nonce_iv(n))}
                 for n in [0, 1, 255, 256, (1 << 53) - 1, (1 << 64) - 1]]
    nonce_errors = [{"nonce": str(n), "error": err(noise_xx.build_nonce_iv, n)} for n in [-1, 1 << 64]]

    # CipherState basics with a fixed key.
    key = synthetic_key("cipherstate/key")
    cs = noise_xx.CipherState()
    unkeyed = h(cs.encrypt_with_ad(b"ad", b"plain passes through"))
    cs.initialize_key(key)
    cipher = []
    for ad, pt in [(b"", b""), (b"", b"abc"), (b"ad", b"abc"), (b"", pattern_bytes(100, 6))]:
        cipher.append({"ad": h(ad), "plaintext": h(pt), "ciphertext": h(cs.encrypt_with_ad(ad, pt))})
    bad_key_error = err(noise_xx.CipherState().initialize_key, b"short")

    dec = noise_xx.CipherState()
    dec.initialize_key(key)
    first = bytes.fromhex(cipher[0]["ciphertext"])
    dec.decrypt_with_ad(b"", first)
    tampered = bytearray(bytes.fromhex(cipher[1]["ciphertext"]))
    tampered[0] ^= 1
    decrypt_fail = err(dec.decrypt_with_ad, b"", bytes(tampered))
    poisoned = err(dec.decrypt_with_ad, b"", bytes.fromhex(cipher[1]["ciphertext"]))

    exhausted = noise_xx.CipherState()
    exhausted.initialize_key(key)
    exhausted._nonce = noise_xx.MAX_SAFE_NONCE - 1
    last_ct = h(exhausted.encrypt_with_ad(b"", b"last"))
    exhaust_error = err(exhausted.encrypt_with_ad, b"", b"one too many")
    exhaust_then = err(exhausted.encrypt_with_ad, b"", b"after")

    return {
        "constants": {
            "PROTOCOL_NAME": noise_xx.PROTOCOL_NAME.decode(),
            "MIN_MSG2_LEN": noise_xx.MIN_MSG2_LEN,
            "MIN_MSG3_LEN": noise_xx.MIN_MSG3_LEN,
            "MAX_SAFE_NONCE": str(noise_xx.MAX_SAFE_NONCE),
        },
        "nonce_iv": nonce_ivs,
        "nonce_iv_errors": nonce_errors,
        "cipher_state": {
            "key": h(key),
            "unkeyed_passthrough": unkeyed,
            "encrypt": cipher,
            "bad_key_error": bad_key_error,
            "decrypt_tampered_error": decrypt_fail,
            "decrypt_after_poison_error": poisoned,
            "nonce_exhaustion": {
                "start_nonce": str(noise_xx.MAX_SAFE_NONCE - 1),
                "last_ciphertext": last_ct,
                "error": exhaust_error,
                "then_error": exhaust_then,
            },
        },
        "handshakes": [run_handshake("hs-empty", b""), run_handshake("hs-payload", b'{"synthetic":"vm hello"}')],
        "errors": handshake_errors(),
    }


# --------------------------------------------------------------------------
# transport.json: noise/transport.py
# --------------------------------------------------------------------------


def make_cipher(label: str) -> noise_xx.CipherState:
    cs = noise_xx.CipherState()
    cs.initialize_key(synthetic_key(label))
    return cs


def ciphertexts(frames) -> list:
    return [blob(f) for f in frames]


def dump_transport() -> dict:
    send_key, recv_key = synthetic_key("transport/c2s"), synthetic_key("transport/s2c")

    # --- client → server requests -------------------------------------
    t = transport.NoiseTransport(make_cipher("transport/c2s"), make_cipher("transport/s2c"))
    server_recv = make_cipher("transport/c2s")
    ops = []
    next_id = [1000]

    def take_ids(n: int) -> list:
        ids = []
        for _ in range(n):
            ids.append(next_id[0] * (-1 if next_id[0] % 2 else 1))
            next_id[0] += 1
        CHUNK_IDS.push(*ids)
        return ids

    def record(op: dict, result, ids):
        frames = result.frames if hasattr(result, "frames") else result
        if hasattr(result, "stream_id"):
            op["stream_id"] = result.stream_id
        op["rng_bytes"] = [chunk_id_rng_bytes(i) for i in ids]
        op["frames"] = ciphertexts(frames)
        # What the server sees after decrypting and reassembling.
        decoder = framing.NoiseFrameDecoder()
        plain = None
        for frame in frames:
            plain = decoder.decode(server_recv.decrypt_with_ad(b"", frame))
        req = envelope.decode_service_request(plain)
        op["server_sees"] = {"service": int(req.service), "frame": frame_json(envelope.decode_service_frame(req.payload))}
        ops.append(op)

    ids = take_ids(1)
    record({"op": "http_request", "method": "GET", "path": "/identity", "body": "", "service": "daemon",
            "headers": []}, t.encrypt_http_request("GET", "/identity"), ids)
    ids = take_ids(1)
    hdrs = [Header("Content-Type", "application/json"), Header("x-request-id", "00000000-0000-4000-8000-000000000000"),
            Header("x-app-id", "musegadget")]
    body = b'{"message":"synthetic marker 1","output_modality":"text","device_id":"node-test"}'
    record({"op": "http_request", "method": "POST", "path": "/chat/stream", "body": h(body), "service": "daemon",
            "headers": [header_json(x) for x in hdrs]},
           t.encrypt_http_request("POST", "/chat/stream", body, headers=hdrs), ids)
    ids = take_ids(1)
    record({"op": "stream_request", "method": "POST", "path": "/link-control", "service": "daemon", "headers": []},
           t.start_stream_request("POST", "/link-control"), ids)
    control_stream = 3
    reg = {"type": "req", "id": "00000000-0000-4000-8000-000000000001", "method": "link.register",
           "params": {"node_id": "node-test", "display_name": "Synthetic Gadget", "platform": "linux"}}
    ids = take_ids(1)
    msg = link_client.encode_message(reg)
    record({"op": "body_chunk", "stream_id_in": control_stream, "data": h(msg), "service": "daemon", "end_body": False},
           t.encrypt_body_chunk(control_stream, msg), ids)
    for service in ["sentinel", "vault", "authd"]:
        ids = take_ids(1)
        record({"op": "http_request", "method": "GET", "path": f"/{service}", "body": "", "service": service,
                "headers": []}, t.encrypt_http_request("GET", f"/{service}", service=service), ids)
    ids = take_ids(1)
    record({"op": "body_chunk", "stream_id_in": 6, "data": "", "service": "daemon", "end_body": True},
           t.encrypt_body_chunk(6, b"", end_body=True), ids)
    ids = take_ids(1)
    record({"op": "reset", "stream_id_in": 2, "service": "daemon", "reason": "", "code": int(ResetCode.CANCELLED)},
           t.encrypt_reset(2), ids)
    ids = take_ids(1)
    record({"op": "reset", "stream_id_in": 7, "service": "vault", "reason": "consumer closed",
            "code": int(ResetCode.TIMEOUT)},
           t.encrypt_reset(7, service="vault", reason="consumer closed", code=ResetCode.TIMEOUT), ids)
    big = pattern_bytes(framing.MAX_CHUNK_PAYLOAD + 5000, 7)
    # A multi-frame message shares ONE chunk id across all its frames, so only
    # one id is drawn; take_ids(1) keeps rng_bytes authoritative per op.
    ids = take_ids(1)
    record({"op": "http_request", "method": "PUT", "path": "/upload", "body": pattern_spec(len(big), 7),
            "service": "daemon", "headers": []}, t.encrypt_http_request("PUT", "/upload", big), ids)
    ids = take_ids(1)
    record({"op": "http_request", "method": "GET", "path": "/after-big", "body": "", "service": "daemon",
            "headers": []}, t.encrypt_http_request("GET", "/after-big"), ids)
    unknown_service = err(transport.NoiseTransport(make_cipher("a"), make_cipher("b")).encrypt_http_request,
                          "GET", "/", service="nope")

    # --- server → client frames ----------------------------------------
    def server_frames(server_send, frame: ServiceFrame, chunk_id: int) -> list:
        plain = transport.encode_response_envelope(frame)
        return [server_send.encrypt_with_ad(b"", x) for x in framing.encode_noise_frames(plain, chunk_id=chunk_id)]

    def raw_server_frames(server_send, plain: bytes, chunk_id: int) -> list:
        return [server_send.encrypt_with_ad(b"", x) for x in framing.encode_noise_frames(plain, chunk_id=chunk_id)]

    def decrypted_json(frame) -> dict | None:
        if frame is None:
            return None
        return {"kind": frame.kind, "stream_id": frame.stream_id, "value": value_json(frame.kind, frame.value)}

    scenarios = []

    def scenario(name: str, builder):
        client = transport.NoiseTransport(make_cipher("transport/c2s"), make_cipher("transport/s2c"))
        server_send = make_cipher("transport/s2c")
        frames = builder(server_send)
        steps = []
        for ct in frames:
            # Inlined in full (not summarised): the Rust test must feed the
            # exact bytes back through decrypt_frame.
            step = {"ciphertext": {"hex": h(ct)}}
            try:
                step["result"] = decrypted_json(client.decrypt_frame(ct))
            except Exception as exc:  # noqa: BLE001
                step["error"] = str(exc)
            steps.append(step)
        # Probe: is the transport still usable for sending afterwards?
        try:
            CHUNK_IDS.push(1)
            client.encrypt_http_request("GET", "/probe")
            alive = True
        except Exception:  # noqa: BLE001
            CHUNK_IDS.ids.clear()
            alive = False
        scenarios.append({"name": name, "steps": steps, "alive_after": alive})

    long_body = pattern_bytes(framing.MAX_CHUNK_PAYLOAD * 2, 8)
    scenario("control_stream", lambda s: [
        *server_frames(s, ServiceFrame.response(3, ApplicationResponse(status=200)), 11),
        *server_frames(s, ServiceFrame.body_chunk(3, BodyChunk(data=link_client.encode_message(
            {"type": "res", "id": "00000000-0000-4000-8000-000000000001", "ok": True}))), 12),
        *server_frames(s, ServiceFrame.body_chunk(3, BodyChunk(data=link_client.encode_message(
            {"type": "req", "id": "inv-1", "method": "link.invoke", "command": "device.health",
             "params": {}}))), 13),
        *server_frames(s, ServiceFrame.body_chunk(3, BodyChunk(end_body=True)), 14),
    ])
    scenario("response_with_body", lambda s: server_frames(
        s, ServiceFrame.response(1, ApplicationResponse(status=200, headers=[Header("content-type", "application/json")],
                                                        body=b'{"name":"Synthetic"}', end_body=True)), -7))
    scenario("multi_chunk_response", lambda s: server_frames(
        s, ServiceFrame.response(2, ApplicationResponse(status=200, body=long_body, end_body=True)), 21))
    scenario("reset", lambda s: server_frames(s, ServiceFrame.reset(4, Reset(ResetCode.REFUSED_STREAM, "no")), 31))
    scenario("frame_without_kind", lambda s: [
        *server_frames(s, ServiceFrame(stream_id=9), 41),
        *server_frames(s, ServiceFrame.response(9, ApplicationResponse(status=204, end_body=True)), 42),
    ])
    scenario("request_from_server", lambda s: server_frames(
        s, ServiceFrame.request(1, ApplicationRequest(verb="GET", path="/")), 51))
    scenario("empty_service_response", lambda s: raw_server_frames(s, b"", 61))
    scenario("malformed_service_frame", lambda s: raw_server_frames(
        s, envelope.encode_service_response(ServiceResponse(payload=b"\x0a\x00")), 62))
    scenario("tampered_ciphertext", lambda s: [
        bytes([x ^ (1 if i == 0 else 0) for i, x in enumerate(f)])
        for f in server_frames(s, ServiceFrame.response(1, ApplicationResponse(status=200)), 71)])
    scenario("bad_noise_frame", lambda s: [s.encrypt_with_ad(b"", b"\x80")])

    return {
        "keys": {"client_send": h(send_key), "client_recv": h(recv_key)},
        "service_map": {k: int(v) for k, v in transport.SERVICE_MAP.items()},
        "client_ops": ops,
        "unknown_service_error": unknown_service,
        "server_scenarios": scenarios,
        "long_body": pattern_spec(len(long_body), 8),
    }


# --------------------------------------------------------------------------
# control.json: link_client.encode_message / MessageDecoder
# --------------------------------------------------------------------------


def dump_control() -> dict:
    encode_cases = [
        ("empty", {}),
        ("register", {"type": "req", "id": "00000000-0000-4000-8000-000000000001", "method": "link.register",
                      "params": {"node_id": "node-test", "display_name": "Synthetic Gadget", "platform": "linux",
                                 "version": "0.1.0", "device_family": "homehub", "model_id": "linux",
                                 "is_wakeup_supported": False, "commands_v2": {}}}),
        ("result", {"method": "link.result", "id": "inv-1", "ok": True,
                    "payload": {"exit_code": 0, "stdout": "ok\n", "timed_out": False}}),
        ("key_order_kept", {"z": 1, "a": 2, "m": 3}),
        ("escapes", {"s": "quote\" backslash\\ slash/ nl\n cr\r tab\t bs\b ff\f nul\x00 us\x1f del\x7f"}),
        ("non_ascii", {"s": "café ☃ ÿ Ā ﻿"}),
        ("astral", {"s": "\U0001f600 \U00010000 \U0010ffff"}),
        ("non_ascii_key", {"é": "k"}),
        ("numbers", {"i": 0, "neg": -1, "big": (1 << 63) - 1, "u64": (1 << 64) - 1, "min": -(1 << 63)}),
        ("floats", {"a": 1.0, "b": 0.5, "c": -0.0, "d": 1e16, "e": 1e15, "f": 1e-5, "g": 0.0001, "h": 1.5e300,
                    "i": 123456789.125, "j": 0.1, "k": 2.5e-320, "l": 1e22, "m": -1234.5e-10, "n": 9007199254740993.0}),
        ("nested", {"a": [1, [2, [3, {}]], {"b": None, "c": True}], "e": []}),
        ("list_top_level", [1, "two", None]),
        ("string_top_level", "x"),
    ]
    # "floats" exercises CPython's float repr, which the Rust port does not
    # reproduce byte-for-byte; the protocol never sends floats.
    encode = [{"name": name, "value": value, "hex": h(link_client.encode_message(value)),
               "rust_deviation": name == "floats"}
              for name, value in encode_cases]

    def lp(data: bytes) -> bytes:
        return len(data).to_bytes(4, "little") + data

    a = link_client.encode_message({"event": "link.unpaired"})
    b = link_client.encode_message({"id": "x", "ok": True})
    deep = b"[" * 200 + b"]" * 200
    deep_obj = b'{"a":' * 200 + b"1" + b"}" * 200
    shallow_obj = b'{"a":' * 100 + b"1" + b"}" * 100
    huge = (link_client.MAX_INBOUND_MESSAGE + 1).to_bytes(4, "little")
    exact_max = link_client.MAX_INBOUND_MESSAGE.to_bytes(4, "little")

    scenarios_in = [
        ("single", [a]),
        ("two_in_one_feed", [a + b]),
        ("byte_by_byte", [bytes([x]) for x in a]),
        ("split_header", [a[:2], a[2:]]),
        ("split_body", [a[:7], a[7:] + b[:5], b[5:]]),
        ("keepalive", [lp(b""), a, lp(b"")]),
        ("malformed_dropped", [lp(b"{not json"), a]),
        ("non_object_dropped", [lp(b"[1,2]") + lp(b'"s"') + lp(b"3") + lp(b"null") + a]),
        ("invalid_utf8_dropped", [lp(b'{"a":"\xff"}') + a]),
        ("whitespace_ok", [lp(b' \t\r\n{"a" : 1 } \n')]),
        ("trailing_garbage_dropped", [lp(b'{"a":1} x') + a]),
        ("duplicate_keys_last_wins", [lp(b'{"a":1,"b":2,"a":3}')]),
        ("utf8_bom_accepted", [lp(b'\xef\xbb\xbf{"a":1}')]),
        ("escapes", [lp(b'{"s":"\\u00e9\\ud83d\\ude00\\n\\/"}')]),
        ("big_int", [lp(b'{"n":123456789012345678901234567890}')]),
        ("nan_literal", [lp(b'{"n":NaN}')]),
        ("infinity_literal", [lp(b'{"n":-Infinity}')]),
        ("float_overflow", [lp(b'{"n":1e400}')]),
        ("lone_surrogate", [lp(b'{"s":"\\ud800"}')]),
        ("utf16_le", [lp('{"a":1}'.encode("utf-16-le"))]),
        ("deep_array_200", [lp(deep) + a]),
        ("deep_object_200", [lp(deep_obj) + a]),
        ("object_depth_100", [lp(shallow_obj)]),
        ("oversized_length", [huge]),
        ("oversized_after_message", [a + huge]),
        ("oversized_then_more", [huge, a]),
        ("max_length_header_waits", [exact_max, b"{}"]),
        ("partial_header_only", [b"\x01\x00"]),
    ]
    # Inputs the Rust port intentionally treats differently from Python's
    # json module; see docs/protocol-notes.md ("JSON parity").
    # Cases where the Rust port (serde_json, UTF-8 only, recursion limit 128,
    # i64/u64/f64 numbers) intentionally diverges from CPython's json module.
    # See docs/protocol-notes.md ("JSON parity"). None of these shapes occur
    # in the real control protocol.
    rust_deviations = {"nan_literal", "infinity_literal", "float_overflow", "lone_surrogate", "utf16_le",
                       "deep_object_200", "big_int", "utf8_bom_accepted"}

    scenarios = []
    for name, feeds in scenarios_in:
        decoder = link_client.MessageDecoder()
        steps = []
        for chunk in feeds:
            step = {"feed": h(chunk)}
            try:
                messages = decoder.feed(chunk)
                step["messages"] = [h(link_client.encode_message(m)) for m in messages]
            except ValueError as exc:
                step["error"] = str(exc)
            steps.append(step)
        scenarios.append({"name": name, "steps": steps, "rust_deviation": name in rust_deviations})

    return {
        "constants": {
            "MAX_INBOUND_MESSAGE": link_client.MAX_INBOUND_MESSAGE,
            "MAX_CONTROL_MESSAGE_BYTES": link_client.MAX_CONTROL_MESSAGE_BYTES,
        },
        "encode": encode,
        "decoder_scenarios": scenarios,
    }


# --------------------------------------------------------------------------
# ble.json: ble_framing.py
# --------------------------------------------------------------------------


def dump_ble() -> dict:
    encode = []
    for length, mtu in [(0, 23), (1, 23), (17, 23), (18, 23), (100, 23), (100, 0), (100, 3), (100, 7),
                        (100, 185), (500, 185), (500, 517), (157 * 255, 517), (4335, 23), (20, 1)]:
        data = pattern_bytes(length, 9)
        try:
            chunks = ble_framing.encode_chunks(data, mtu)
            encode.append({"len": length, "mtu": mtu, "data": pattern_spec(length, 9),
                           "chunks": [h(c) for c in chunks] if length <= 600 else None,
                           "count": len(chunks), "all_sha256": sha(b"".join(chunks))})
        except ValueError as exc:
            encode.append({"len": length, "mtu": mtu, "data": pattern_spec(length, 9), "error": str(exc)})
    encode_errors = []
    for length, mtu in [(17 * 255 + 1, 23), (157 * 255 + 1, 517), (100, 6)]:
        data = pattern_bytes(length, 9)
        encode_errors.append({"len": length, "mtu": mtu, "data": pattern_spec(length, 9),
                              "error": err(ble_framing.encode_chunks, data, mtu)})
    # mtu 4 and 5: the reference silently returns one empty chunk (usable < 0).
    reference_quirks = [{"len": 100, "mtu": m, "chunks": [h(c) for c in ble_framing.encode_chunks(pattern_bytes(100, 9), m)]}
                        for m in (4, 5)]

    def pk(index: int, total: int, frag: bytes) -> bytes:
        return bytes([0xFE, index, total]) + frag

    scenarios_in = [
        ("unchunked", None, [b'{"a":1}']),
        ("short_magic_is_plain", None, [b"\xfe\x00"]),
        ("empty_packet", None, [b""]),
        ("in_order", None, [pk(0, 3, b"ab"), pk(1, 3, b"cd"), pk(2, 3, b"ef")]),
        ("single_chunk", None, [pk(0, 1, b"xyz")]),
        ("empty_fragment", None, [pk(0, 1, b"")]),
        ("total_zero_resets", None, [pk(0, 2, b"a"), pk(0, 0, b""), pk(1, 2, b"b")]),
        ("index_zero_restarts", None, [pk(0, 2, b"a"), pk(0, 2, b"b"), pk(1, 2, b"c")]),
        ("total_change_restarts", None, [pk(0, 3, b"a"), pk(1, 2, b"b")]),
        ("out_of_order_discards", None, [pk(0, 3, b"a"), pk(2, 3, b"c"), pk(1, 3, b"b")]),
        ("index_past_total", None, [pk(0, 2, b"a"), pk(1, 2, b"b"), pk(2, 2, b"c")]),
        ("starts_mid_message", None, [pk(1, 2, b"b")]),
        ("plain_between_chunks", None, [pk(0, 2, b"a"), b"plain", pk(1, 2, b"b")]),
        ("oversize_discards", 4, [pk(0, 2, b"abc"), pk(1, 2, b"de"), pk(0, 1, b"abcd")]),
        ("default_max_exact", None, [pk(0, 2, b"\x01" * 4096), pk(1, 2, b"\x02" * 4096)]),
        ("default_max_over", None, [pk(0, 2, b"\x01" * 4096), pk(1, 2, b"\x02" * 4097)]),
        ("roundtrip_mtu23", None, ble_framing.encode_chunks(pattern_bytes(300, 9), 23)),
    ]
    scenarios = []
    for name, max_bytes, packets in scenarios_in:
        asm = ble_framing.ChunkAssembler() if max_bytes is None else ble_framing.ChunkAssembler(max_bytes)
        steps = []
        for p in packets:
            out = asm.feed(p)
            steps.append({"packet": h(p), "result": None if out is None else h(out)})
        scenarios.append({"name": name, "max_bytes": max_bytes, "steps": steps})

    return {
        "constants": {
            "CHUNK_MAGIC": ble_framing.CHUNK_MAGIC,
            "HEADER_BYTES": ble_framing.HEADER_BYTES,
            "MAX_PACKET_BYTES": ble_framing.MAX_PACKET_BYTES,
            "MAX_CHUNKS": ble_framing.MAX_CHUNKS,
            "MAX_MESSAGE_BYTES": ble_framing.MAX_MESSAGE_BYTES,
            "DEFAULT_ATT_MTU": ble_framing.DEFAULT_ATT_MTU,
        },
        "encode": encode,
        "encode_errors": encode_errors,
        "reference_quirks": reference_quirks,
        "assembler_scenarios": scenarios,
    }


# --------------------------------------------------------------------------


def render(obj) -> str:
    return json.dumps(obj, indent=1, sort_keys=False, ensure_ascii=True) + "\n"


def build() -> dict[str, str]:
    files = {
        "wire.json": dump_wire(),
        "envelope.json": dump_envelope(),
        "framing.json": dump_framing(),
        "noise.json": dump_noise(),
        "transport.json": dump_transport(),
        "control.json": dump_control(),
        "ble.json": dump_ble(),
    }
    assert not KEYS.keys, "unused injected keys"
    assert not CHUNK_IDS.ids, "unused injected chunk ids"
    meta = {
        "generator": "gadgets/goblin-link/tools/dump_vectors.py",
        "reference": "gadgets/trimui-brick/pak/lib/musegadget (facebookincubator/muse-gadget-sdk "
                     "197033c0acd40946a18dc7edc4136f022c08a369, Apache-2.0)",
        "note": "All keys are synthetic: sha256('muse-proto synthetic test key: ' + label).",
        "pattern_affine31": "byte i = (i*31 + seed*7 + 11) mod 256",
    }
    out = {"meta.json": render(meta)}
    out.update({name: render(value) for name, value in files.items()})
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if checked-in vectors are stale")
    args = parser.parse_args()
    files = build()
    if args.check:
        stale = [n for n, text in files.items()
                 if not (OUT_DIR / n).exists() or (OUT_DIR / n).read_text() != text]
        if stale:
            print("stale vectors: " + ", ".join(stale), file=sys.stderr)
            return 1
        print(f"{len(files)} vector files up to date")
        return 0
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for name, text in files.items():
        (OUT_DIR / name).write_text(text)
    print(f"wrote {len(files)} files to {OUT_DIR}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
