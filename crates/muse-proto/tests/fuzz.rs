//! Property and fuzz-style tests: decoders must never panic on arbitrary
//! bytes, and encode/decode must round-trip. These complement the byte-exact
//! vector tests by exercising inputs the reference does not enumerate.

mod common;

use muse_proto::ble::{encode_chunks, ChunkAssembler};
use muse_proto::control::MessageDecoder;
use muse_proto::envelope::*;
use muse_proto::framing::{
    decode_noise_frame, encode_noise_frame, encode_noise_frames, NoiseFrameDecoder,
    NoiseTransportFrame,
};
use muse_proto::wire::{read_delimited, read_key, read_varint, skip_field, WireType};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Every envelope decoder returns Ok or a typed error, never panics.
    #[test]
    fn envelope_decoders_never_panic(data in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = decode_service_frame(&data);
        let _ = decode_service_request(&data);
        let _ = decode_service_response(&data);
        let _ = decode_header(&data);
        let _ = decode_application_request(&data);
        let _ = decode_application_response(&data);
        let _ = decode_body_chunk(&data);
        let _ = decode_reset(&data);
    }

    /// The low-level wire readers never panic and never read past the input.
    #[test]
    fn wire_readers_never_panic(data in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = read_varint(&data, 0);
        if let Ok((_, _, off)) = read_key(&data, 0) {
            prop_assert!(off <= data.len());
        }
        let _ = read_delimited(&data, 0);
        for wt in [WireType::Varint, WireType::Fixed64, WireType::Delimited, WireType::Fixed32] {
            if let Ok(off) = skip_field(&data, 0, wt) {
                prop_assert!(off <= data.len());
            }
        }
    }

    /// The noise frame decoder never panics on arbitrary frames.
    #[test]
    fn noise_frame_decoder_never_panics(
        frames in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..300), 0..40)
    ) {
        let mut decoder = NoiseFrameDecoder::new();
        for frame in &frames {
            if decoder.decode(frame).is_err() {
                // Once poisoned it must keep erroring, never panic.
                prop_assert!(decoder.decode(frame).is_err());
                break;
            }
        }
    }

    /// The control decoder never panics and respects its size bound.
    #[test]
    fn control_decoder_never_panics(
        chunks in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..64), 0..20)
    ) {
        let mut decoder = MessageDecoder::new();
        for chunk in &chunks {
            match decoder.feed(chunk) {
                Ok(messages) => {
                    for m in messages {
                        prop_assert!(m.is_object());
                    }
                }
                Err(_) => break, // oversized: connection would be torn down
            }
        }
    }

    /// The BLE assembler never panics on arbitrary packets.
    #[test]
    fn ble_assembler_never_panics(
        packets in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..40), 0..40)
    ) {
        let mut asm = ChunkAssembler::new();
        for p in &packets {
            let _ = asm.feed(p);
        }
    }

    /// A single NoiseTransportFrame round-trips through encode/decode.
    #[test]
    fn noise_frame_round_trips(
        chunk_id in any::<i64>(),
        chunk_index in 0u32..256,
        total in 1u32..256,
        payload in proptest::collection::vec(any::<u8>(), 0..300),
    ) {
        let frame = NoiseTransportFrame { chunk_id, chunk_index, total_chunks: total, payload };
        let decoded = decode_noise_frame(&encode_noise_frame(&frame)).unwrap();
        prop_assert_eq!(decoded, frame);
    }

    /// Every ServiceFrame shape round-trips through encode/decode.
    #[test]
    fn service_frame_round_trips(
        stream_id in any::<i64>(),
        verb in "[A-Z]{0,8}",
        path in "/[a-z/]{0,20}",
        body in proptest::collection::vec(any::<u8>(), 0..64),
        end_body in any::<bool>(),
        status in 0i32..600,
    ) {
        let frames = [
            ServiceFrame::request(stream_id, ApplicationRequest {
                verb: verb.clone(), path: path.clone(),
                headers: vec![Header::new("x", "y")],
                body: body.clone(), end_body,
            }),
            ServiceFrame::response(stream_id, ApplicationResponse {
                status, headers: vec![], body: body.clone(), end_body,
            }),
            ServiceFrame::body_chunk(stream_id, BodyChunk { data: body.clone(), end_body }),
            ServiceFrame::reset(stream_id, Reset { code: ResetCode::Timeout, reason: path.clone() }),
        ];
        for frame in frames {
            let decoded = decode_service_frame(&encode_service_frame(&frame)).unwrap();
            prop_assert_eq!(decoded, frame);
        }
    }

    /// BLE encode then reassemble round-trips for any payload and MTU.
    #[test]
    fn ble_round_trips(
        payload in proptest::collection::vec(any::<u8>(), 1..3000),
        mtu in 7u16..517,
    ) {
        let chunks = match encode_chunks(&payload, mtu) {
            Ok(c) => c,
            Err(_) => return Ok(()), // too many chunks at this MTU
        };
        let mut asm = ChunkAssembler::with_max_bytes(1 << 20);
        let mut out = None;
        for chunk in &chunks {
            out = asm.feed(chunk);
        }
        prop_assert_eq!(out.unwrap(), payload);
    }
}

proptest! {
    // Larger, slower payloads: fewer cases to keep the suite quick.
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Fragmenting then reassembling a multi-chunk payload yields the
    /// original bytes (crossing the 65489-byte chunk boundary several times).
    #[test]
    fn framing_fragmentation_round_trips(
        payload in proptest::collection::vec(any::<u8>(), 0..200_000),
        chunk_id in any::<i64>(),
    ) {
        let frames = encode_noise_frames(&payload, chunk_id).unwrap();
        let mut decoder = NoiseFrameDecoder::new();
        let mut out = None;
        for frame in &frames {
            out = decoder.decode(frame).unwrap();
        }
        prop_assert_eq!(out.unwrap(), payload);
    }
}
