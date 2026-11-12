//! The post-handshake transport (port of `noise/transport.py`).
//!
//! Wraps the send/receive [`CipherState`]s from a completed handshake. The
//! client builds application requests, body chunks and resets; each is
//! encoded as a [`ServiceFrame`] inside a [`ServiceRequest`], fragmented into
//! [`crate::framing`] chunks, and each chunk encrypted into one Noise frame.
//! Incoming ciphertexts are decrypted, reassembled, and decoded into a
//! [`DecryptedFrame`].
//!
//! Stream ids are allocated 1, 2, 3, … by the client. A request with an
//! inline body ([`NoiseTransport::encrypt_http_request`]) sets `end_body`;
//! [`NoiseTransport::start_stream_request`] leaves it unset so the caller can
//! follow with [`NoiseTransport::encrypt_body_chunk`]s. Any cipher or codec
//! failure marks the transport dead, because a partially advanced nonce must
//! never be reused — the caller must drop the connection.
//!
//! This type performs no fragmentation randomness itself: callers pass the
//! `chunk_id` (drawn via [`crate::framing::random_chunk_id`] in production, or
//! fixed in tests), keeping the type pure and deterministic.

use crate::envelope::{
    decode_service_frame, decode_service_request, decode_service_response, encode_service_frame,
    encode_service_request, encode_service_response, ApplicationRequest, ApplicationResponse,
    BodyChunk, FrameBody, Header, Reset, ResetCode, ServiceFrame, ServiceRequest, ServiceResponse,
    ServiceType,
};
use crate::error::TransportError;
use crate::framing::{encode_noise_frames, NoiseFrameDecoder};
use crate::noise::CipherState;

const EMPTY_AD: &[u8] = b"";

/// Resolves a service name as the reference's `SERVICE_MAP` does.
pub fn service_from_name(name: &str) -> Result<ServiceType, TransportError> {
    match name {
        "daemon" => Ok(ServiceType::Daemon),
        "sentinel" => Ok(ServiceType::Sentinel),
        "vault" => Ok(ServiceType::Vault),
        "authd" => Ok(ServiceType::Authd),
        _ => Err(TransportError::UnknownService(name.to_owned())),
    }
}

/// An outgoing request with its allocated stream id and encrypted frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedFrames {
    /// Stream id the frames belong to.
    pub stream_id: i64,
    /// One Noise ciphertext per chunk, in order.
    pub frames: Vec<Vec<u8>>,
}

/// A decoded inbound frame (never a request: the server only responds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecryptedFrame {
    /// A response head (and possibly inline body).
    Response {
        /// Stream the response is for.
        stream_id: i64,
        /// The response.
        response: ApplicationResponse,
    },
    /// A further piece of a response body.
    BodyChunk {
        /// Stream the chunk is for.
        stream_id: i64,
        /// The chunk.
        chunk: BodyChunk,
    },
    /// A stream reset.
    Reset {
        /// Stream being reset.
        stream_id: i64,
        /// The reset.
        reset: Reset,
    },
}

/// The multiplexed Noise transport.
pub struct NoiseTransport {
    send: CipherState,
    recv: CipherState,
    decoder: NoiseFrameDecoder,
    next_stream_id: i64,
    dead: bool,
}

impl std::fmt::Debug for NoiseTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseTransport")
            .field("next_stream_id", &self.next_stream_id)
            .field("dead", &self.dead)
            .finish()
    }
}

impl NoiseTransport {
    /// Builds a transport from a handshake's `(send, recv)` ciphers.
    pub fn new(send: CipherState, recv: CipherState) -> Self {
        Self {
            send,
            recv,
            decoder: NoiseFrameDecoder::new(),
            next_stream_id: 1,
            dead: false,
        }
    }

    fn assert_alive(&self) -> Result<(), TransportError> {
        if self.dead {
            Err(TransportError::Dead)
        } else {
            Ok(())
        }
    }

    fn allocate_stream_id(&mut self) -> Result<i64, TransportError> {
        let id = self.next_stream_id;
        self.next_stream_id = self
            .next_stream_id
            .checked_add(1)
            .ok_or(TransportError::StreamIdsExhausted)?;
        Ok(id)
    }

    /// Builds a complete request (inline body, `end_body` set) on a new stream.
    pub fn encrypt_http_request(
        &mut self,
        method: &str,
        path: &str,
        body: &[u8],
        service: ServiceType,
        headers: &[Header],
        chunk_id: i64,
    ) -> Result<EncryptedFrames, TransportError> {
        self.send_application_request(method, path, body, service, headers, true, chunk_id)
    }

    /// Opens a streaming request (`end_body` unset) on a new stream.
    pub fn start_stream_request(
        &mut self,
        method: &str,
        path: &str,
        service: ServiceType,
        headers: &[Header],
        chunk_id: i64,
    ) -> Result<EncryptedFrames, TransportError> {
        self.send_application_request(method, path, b"", service, headers, false, chunk_id)
    }

    #[allow(clippy::too_many_arguments)]
    fn send_application_request(
        &mut self,
        method: &str,
        path: &str,
        body: &[u8],
        service: ServiceType,
        headers: &[Header],
        end_body: bool,
        chunk_id: i64,
    ) -> Result<EncryptedFrames, TransportError> {
        self.assert_alive()?;
        let result = (|| {
            let stream_id = self.allocate_stream_id()?;
            let request = ApplicationRequest {
                verb: method.to_owned(),
                path: path.to_owned(),
                headers: headers.to_vec(),
                body: body.to_vec(),
                end_body,
            };
            let frame = ServiceFrame::request(stream_id, request);
            let frames = self.encrypt_request(service, &frame, chunk_id)?;
            Ok(EncryptedFrames { stream_id, frames })
        })();
        if result.is_err() {
            self.dead = true;
        }
        result
    }

    /// Encrypts a body chunk on an existing stream.
    pub fn encrypt_body_chunk(
        &mut self,
        stream_id: i64,
        data: &[u8],
        service: ServiceType,
        end_body: bool,
        chunk_id: i64,
    ) -> Result<Vec<Vec<u8>>, TransportError> {
        self.assert_alive()?;
        let result = {
            let frame = ServiceFrame::body_chunk(
                stream_id,
                BodyChunk {
                    data: data.to_vec(),
                    end_body,
                },
            );
            self.encrypt_request(service, &frame, chunk_id)
        };
        if result.is_err() {
            self.dead = true;
        }
        result
    }

    /// Encrypts a reset on an existing stream.
    pub fn encrypt_reset(
        &mut self,
        stream_id: i64,
        service: ServiceType,
        reason: &str,
        code: ResetCode,
        chunk_id: i64,
    ) -> Result<Vec<Vec<u8>>, TransportError> {
        self.assert_alive()?;
        let result = {
            let frame = ServiceFrame::reset(
                stream_id,
                Reset {
                    code,
                    reason: reason.to_owned(),
                },
            );
            self.encrypt_request(service, &frame, chunk_id)
        };
        if result.is_err() {
            self.dead = true;
        }
        result
    }

    fn encrypt_request(
        &mut self,
        service: ServiceType,
        frame: &ServiceFrame,
        chunk_id: i64,
    ) -> Result<Vec<Vec<u8>>, TransportError> {
        let frame_bytes = encode_service_frame(frame);
        let request_bytes = encode_service_request(&ServiceRequest {
            service,
            payload: frame_bytes,
        });
        let proto_frames = encode_noise_frames(&request_bytes, chunk_id)?;
        let mut out = Vec::with_capacity(proto_frames.len());
        for proto in proto_frames {
            out.push(self.send.encrypt_with_ad(EMPTY_AD, &proto)?);
        }
        Ok(out)
    }

    /// Decrypts one inbound ciphertext.
    ///
    /// Returns `Ok(None)` when the ciphertext was one chunk of a message whose
    /// remaining chunks have not arrived yet. A decoded request frame, an
    /// empty payload or any codec error marks the transport dead.
    pub fn decrypt_frame(
        &mut self,
        ciphertext: &[u8],
    ) -> Result<Option<DecryptedFrame>, TransportError> {
        self.assert_alive()?;
        let result = self.decrypt_frame_inner(ciphertext);
        if result.is_err() {
            self.dead = true;
        }
        result
    }

    fn decrypt_frame_inner(
        &mut self,
        ciphertext: &[u8],
    ) -> Result<Option<DecryptedFrame>, TransportError> {
        let plain = self.recv.decrypt_with_ad(EMPTY_AD, ciphertext)?;
        let Some(reassembled) = self.decoder.decode(&plain)? else {
            return Ok(None);
        };
        let response = decode_service_response(&reassembled)?;
        if response.payload.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        let frame = decode_service_frame(&response.payload)?;
        match frame.body {
            Some(FrameBody::Response(response)) => Ok(Some(DecryptedFrame::Response {
                stream_id: frame.stream_id,
                response,
            })),
            Some(FrameBody::BodyChunk(chunk)) => Ok(Some(DecryptedFrame::BodyChunk {
                stream_id: frame.stream_id,
                chunk,
            })),
            Some(FrameBody::Reset(reset)) => Ok(Some(DecryptedFrame::Reset {
                stream_id: frame.stream_id,
                reset,
            })),
            Some(FrameBody::Request(_)) => Err(TransportError::UnexpectedRequest),
            None => Ok(None),
        }
    }
}

/// Decodes a client-to-VM request envelope into its inner [`ServiceFrame`].
/// (The server side of the protocol; handy for the simulated VM in tests.)
pub fn decode_request_envelope(data: &[u8]) -> Result<ServiceFrame, TransportError> {
    let request = decode_service_request(data)?;
    Ok(decode_service_frame(&request.payload)?)
}

/// Encodes a [`ServiceFrame`] as a VM-to-client response envelope.
pub fn encode_response_envelope(frame: &ServiceFrame) -> Vec<u8> {
    encode_service_response(&ServiceResponse {
        payload: encode_service_frame(frame),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::CipherState;

    fn cipher(byte: u8) -> CipherState {
        let mut cs = CipherState::new();
        cs.initialize_key(&[byte; 32]).unwrap();
        cs
    }

    #[test]
    fn request_round_trips_through_a_matching_pair() {
        let mut client = NoiseTransport::new(cipher(1), cipher(2));
        let mut server_recv = cipher(1);
        let enc = client
            .encrypt_http_request("GET", "/identity", b"", ServiceType::Daemon, &[], 7)
            .unwrap();
        assert_eq!(enc.stream_id, 1);
        let mut decoder = NoiseFrameDecoder::new();
        let mut plain = None;
        for frame in &enc.frames {
            plain = decoder
                .decode(&server_recv.decrypt_with_ad(b"", frame).unwrap())
                .unwrap();
        }
        let frame = decode_request_envelope(&plain.unwrap()).unwrap();
        assert_eq!(frame.stream_id, 1);
    }

    #[test]
    fn unknown_service_name_is_an_error() {
        assert!(matches!(
            service_from_name("nope"),
            Err(TransportError::UnknownService(_))
        ));
    }
}
