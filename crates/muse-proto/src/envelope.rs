//! Service envelopes carried inside Noise transport frames (port of
//! `noise/envelope.py`).
//!
//! Client to VM, every plaintext message is a [`ServiceRequest`] whose
//! payload is an encoded [`ServiceFrame`]. VM to client it is a
//! [`ServiceResponse`] wrapping a [`ServiceFrame`]. A frame belongs to one
//! stream (`stream_id`) and carries at most one of: an [`ApplicationRequest`]
//! (HTTP-like request head plus optional inline body), an
//! [`ApplicationResponse`] (status, headers, optional body), a [`BodyChunk`]
//! or a [`Reset`].
//!
//! Encoding follows proto3: default values (empty strings, zero, `false`,
//! empty bytes) are omitted. Decoding skips unknown fields, rejects known
//! fields with the wrong wire type, requires valid UTF-8 for strings, and
//! lets the last occurrence of a field (or of the `ServiceFrame` oneof) win.

use crate::error::WireError;
use crate::wire::{
    bool_field, decode_int32, decode_int64, decode_string, delimited_field, int32_field,
    int64_field, varint_field, FieldCursor,
};

/// Which VM service a request is addressed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ServiceType {
    /// The VM daemon (`0`); everything the gadget SDK uses.
    #[default]
    Daemon = 0,
    /// `1`.
    Sentinel = 1,
    /// `2`.
    Vault = 2,
    /// `3`.
    Authd = 3,
}

impl ServiceType {
    fn from_wire(raw: u64) -> Result<Self, WireError> {
        match raw {
            0 => Ok(Self::Daemon),
            1 => Ok(Self::Sentinel),
            2 => Ok(Self::Vault),
            3 => Ok(Self::Authd),
            _ => Err(WireError::UnknownServiceType),
        }
    }
}

/// Why a stream was reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ResetCode {
    /// `0`, omitted on the wire.
    #[default]
    Unspecified = 0,
    /// `1`; the reference's default when the client resets a stream.
    Cancelled = 1,
    /// `2`.
    Timeout = 2,
    /// `3`.
    ProtocolError = 3,
    /// `4`.
    RefusedStream = 4,
    /// `5`.
    InternalError = 5,
    /// `6`.
    ServiceUnavailable = 6,
}

impl ResetCode {
    fn from_wire(raw: i32) -> Result<Self, WireError> {
        match raw {
            0 => Ok(Self::Unspecified),
            1 => Ok(Self::Cancelled),
            2 => Ok(Self::Timeout),
            3 => Ok(Self::ProtocolError),
            4 => Ok(Self::RefusedStream),
            5 => Ok(Self::InternalError),
            6 => Ok(Self::ServiceUnavailable),
            _ => Err(WireError::UnknownResetCode),
        }
    }
}

/// One HTTP-style header. Keys are passed through as given (no case folding).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Header {
    /// Header name.
    pub key: String,
    /// Header value.
    pub value: String,
}

impl Header {
    /// Builds a header from anything string-like.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// Request head, optionally with an inline body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplicationRequest {
    /// HTTP method, e.g. `GET`.
    pub verb: String,
    /// Path and query, e.g. `/chat/history?limit=1`.
    pub path: String,
    /// Request headers, in order.
    pub headers: Vec<Header>,
    /// Inline body bytes.
    pub body: Vec<u8>,
    /// `true` when no [`BodyChunk`]s follow on this stream.
    pub end_body: bool,
}

/// Response head, optionally with the first (or only) part of the body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplicationResponse {
    /// HTTP status; `0` when absent.
    pub status: i32,
    /// Response headers, in order.
    pub headers: Vec<Header>,
    /// Inline body bytes.
    pub body: Vec<u8>,
    /// `true` when no [`BodyChunk`]s follow on this stream.
    pub end_body: bool,
}

/// A further piece of a request or response body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BodyChunk {
    /// Body bytes.
    pub data: Vec<u8>,
    /// `true` on the last chunk of the stream.
    pub end_body: bool,
}

/// Abrupt end of a stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reset {
    /// Reason code.
    pub code: ResetCode,
    /// Free-form reason text.
    pub reason: String,
}

/// The oneof carried by a [`ServiceFrame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameBody {
    /// Field 2.
    Request(ApplicationRequest),
    /// Field 3.
    Response(ApplicationResponse),
    /// Field 4.
    BodyChunk(BodyChunk),
    /// Field 5.
    Reset(Reset),
}

/// One frame on one multiplexed stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceFrame {
    /// Stream id; the client allocates 1, 2, 3, … per connection.
    pub stream_id: i64,
    /// Frame content; `None` when the frame carries no oneof field.
    pub body: Option<FrameBody>,
}

impl ServiceFrame {
    /// A request frame.
    pub fn request(stream_id: i64, request: ApplicationRequest) -> Self {
        Self {
            stream_id,
            body: Some(FrameBody::Request(request)),
        }
    }

    /// A response frame.
    pub fn response(stream_id: i64, response: ApplicationResponse) -> Self {
        Self {
            stream_id,
            body: Some(FrameBody::Response(response)),
        }
    }

    /// A body chunk frame.
    pub fn body_chunk(stream_id: i64, chunk: BodyChunk) -> Self {
        Self {
            stream_id,
            body: Some(FrameBody::BodyChunk(chunk)),
        }
    }

    /// A reset frame.
    pub fn reset(stream_id: i64, reset: Reset) -> Self {
        Self {
            stream_id,
            body: Some(FrameBody::Reset(reset)),
        }
    }
}

/// Client-to-VM wrapper: which service, and the encoded [`ServiceFrame`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceRequest {
    /// Target service.
    pub service: ServiceType,
    /// Encoded [`ServiceFrame`].
    pub payload: Vec<u8>,
}

/// VM-to-client wrapper around an encoded [`ServiceFrame`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServiceResponse {
    /// Encoded [`ServiceFrame`].
    pub payload: Vec<u8>,
}

fn string_field(field: u32, value: &str, out: &mut Vec<u8>) {
    if !value.is_empty() {
        delimited_field(field, value.as_bytes(), out);
    }
}

fn bytes_field(field: u32, value: &[u8], out: &mut Vec<u8>) {
    if !value.is_empty() {
        delimited_field(field, value, out);
    }
}

fn true_field(field: u32, value: bool, out: &mut Vec<u8>) {
    if value {
        bool_field(field, true, out);
    }
}

/// Encodes a [`Header`].
pub fn encode_header(header: &Header) -> Vec<u8> {
    let mut out = Vec::new();
    string_field(1, &header.key, &mut out);
    string_field(2, &header.value, &mut out);
    out
}

/// Decodes a [`Header`].
pub fn decode_header(data: &[u8]) -> Result<Header, WireError> {
    let mut header = Header::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => header.key = decode_string(cur.take_delimited(wire, "Header.key")?)?,
            2 => header.value = decode_string(cur.take_delimited(wire, "Header.value")?)?,
            _ => cur.skip(wire)?,
        }
    }
    Ok(header)
}

/// Encodes an [`ApplicationRequest`].
pub fn encode_application_request(request: &ApplicationRequest) -> Vec<u8> {
    let mut out = Vec::new();
    string_field(1, &request.verb, &mut out);
    string_field(2, &request.path, &mut out);
    for header in &request.headers {
        delimited_field(3, &encode_header(header), &mut out);
    }
    bytes_field(4, &request.body, &mut out);
    true_field(5, request.end_body, &mut out);
    out
}

/// Decodes an [`ApplicationRequest`].
pub fn decode_application_request(data: &[u8]) -> Result<ApplicationRequest, WireError> {
    let mut request = ApplicationRequest::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => {
                request.verb = decode_string(cur.take_delimited(wire, "ApplicationRequest.verb")?)?;
            }
            2 => {
                request.path = decode_string(cur.take_delimited(wire, "ApplicationRequest.path")?)?;
            }
            3 => {
                let raw = cur.take_delimited(wire, "ApplicationRequest.headers")?;
                request.headers.push(decode_header(raw)?);
            }
            4 => {
                request.body = cur
                    .take_delimited(wire, "ApplicationRequest.body")?
                    .to_vec()
            }
            5 => request.end_body = cur.take_varint(wire, "ApplicationRequest.end_body")? != 0,
            _ => cur.skip(wire)?,
        }
    }
    Ok(request)
}

/// Encodes an [`ApplicationResponse`].
pub fn encode_application_response(response: &ApplicationResponse) -> Vec<u8> {
    let mut out = Vec::new();
    if response.status != 0 {
        int32_field(1, response.status, &mut out);
    }
    for header in &response.headers {
        delimited_field(2, &encode_header(header), &mut out);
    }
    bytes_field(3, &response.body, &mut out);
    true_field(4, response.end_body, &mut out);
    out
}

/// Decodes an [`ApplicationResponse`].
pub fn decode_application_response(data: &[u8]) -> Result<ApplicationResponse, WireError> {
    let mut response = ApplicationResponse::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => {
                let raw = cur.take_varint(wire, "ApplicationResponse.status")?;
                response.status = decode_int32(raw)?;
            }
            2 => {
                let raw = cur.take_delimited(wire, "ApplicationResponse.headers")?;
                response.headers.push(decode_header(raw)?);
            }
            3 => {
                response.body = cur
                    .take_delimited(wire, "ApplicationResponse.body")?
                    .to_vec()
            }
            4 => response.end_body = cur.take_varint(wire, "ApplicationResponse.end_body")? != 0,
            _ => cur.skip(wire)?,
        }
    }
    Ok(response)
}

/// Encodes a [`BodyChunk`].
pub fn encode_body_chunk(chunk: &BodyChunk) -> Vec<u8> {
    let mut out = Vec::new();
    bytes_field(1, &chunk.data, &mut out);
    true_field(2, chunk.end_body, &mut out);
    out
}

/// Decodes a [`BodyChunk`].
pub fn decode_body_chunk(data: &[u8]) -> Result<BodyChunk, WireError> {
    let mut chunk = BodyChunk::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => chunk.data = cur.take_delimited(wire, "BodyChunk.data")?.to_vec(),
            2 => chunk.end_body = cur.take_varint(wire, "BodyChunk.end_body")? != 0,
            _ => cur.skip(wire)?,
        }
    }
    Ok(chunk)
}

/// Encodes a [`Reset`].
pub fn encode_reset(reset: &Reset) -> Vec<u8> {
    let mut out = Vec::new();
    if reset.code != ResetCode::Unspecified {
        int32_field(1, reset.code as i32, &mut out);
    }
    string_field(2, &reset.reason, &mut out);
    out
}

/// Decodes a [`Reset`].
pub fn decode_reset(data: &[u8]) -> Result<Reset, WireError> {
    let mut reset = Reset::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => {
                let raw = cur.take_varint(wire, "Reset.code")?;
                reset.code = ResetCode::from_wire(decode_int32(raw)?)?;
            }
            2 => reset.reason = decode_string(cur.take_delimited(wire, "Reset.reason")?)?,
            _ => cur.skip(wire)?,
        }
    }
    Ok(reset)
}

/// Encodes a [`ServiceFrame`].
pub fn encode_service_frame(frame: &ServiceFrame) -> Vec<u8> {
    let mut out = Vec::new();
    if frame.stream_id != 0 {
        int64_field(1, frame.stream_id, &mut out);
    }
    match &frame.body {
        None => {}
        Some(FrameBody::Request(r)) => delimited_field(2, &encode_application_request(r), &mut out),
        Some(FrameBody::Response(r)) => {
            delimited_field(3, &encode_application_response(r), &mut out);
        }
        Some(FrameBody::BodyChunk(c)) => delimited_field(4, &encode_body_chunk(c), &mut out),
        Some(FrameBody::Reset(r)) => delimited_field(5, &encode_reset(r), &mut out),
    }
    out
}

/// Decodes a [`ServiceFrame`]. If several oneof fields are present the last
/// one wins, as in protobuf.
pub fn decode_service_frame(data: &[u8]) -> Result<ServiceFrame, WireError> {
    let mut frame = ServiceFrame::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => {
                frame.stream_id = decode_int64(cur.take_varint(wire, "ServiceFrame.stream_id")?);
            }
            2 => {
                let raw = cur.take_delimited(wire, "ServiceFrame.request")?;
                frame.body = Some(FrameBody::Request(decode_application_request(raw)?));
            }
            3 => {
                let raw = cur.take_delimited(wire, "ServiceFrame.response")?;
                frame.body = Some(FrameBody::Response(decode_application_response(raw)?));
            }
            4 => {
                let raw = cur.take_delimited(wire, "ServiceFrame.body_chunk")?;
                frame.body = Some(FrameBody::BodyChunk(decode_body_chunk(raw)?));
            }
            5 => {
                let raw = cur.take_delimited(wire, "ServiceFrame.reset")?;
                frame.body = Some(FrameBody::Reset(decode_reset(raw)?));
            }
            _ => cur.skip(wire)?,
        }
    }
    Ok(frame)
}

/// Encodes a [`ServiceRequest`].
pub fn encode_service_request(request: &ServiceRequest) -> Vec<u8> {
    let mut out = Vec::new();
    if request.service != ServiceType::Daemon {
        varint_field(1, request.service as u64, &mut out);
    }
    bytes_field(2, &request.payload, &mut out);
    out
}

/// Decodes a [`ServiceRequest`].
pub fn decode_service_request(data: &[u8]) -> Result<ServiceRequest, WireError> {
    let mut request = ServiceRequest::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => {
                let raw = cur.take_varint(wire, "ServiceRequest.service")?;
                request.service = ServiceType::from_wire(raw)?;
            }
            2 => request.payload = cur.take_delimited(wire, "ServiceRequest.payload")?.to_vec(),
            _ => cur.skip(wire)?,
        }
    }
    Ok(request)
}

/// Encodes a [`ServiceResponse`].
pub fn encode_service_response(response: &ServiceResponse) -> Vec<u8> {
    let mut out = Vec::new();
    bytes_field(1, &response.payload, &mut out);
    out
}

/// Decodes a [`ServiceResponse`].
pub fn decode_service_response(data: &[u8]) -> Result<ServiceResponse, WireError> {
    let mut response = ServiceResponse::default();
    let mut cur = FieldCursor::new(data);
    while let Some((field, wire)) = cur.next_key()? {
        match field {
            1 => {
                response.payload = cur
                    .take_delimited(wire, "ServiceResponse.payload")?
                    .to_vec();
            }
            _ => cur.skip(wire)?,
        }
    }
    Ok(response)
}
