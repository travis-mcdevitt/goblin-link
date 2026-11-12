//! Length-prefixed JSON control messages (port of `encode_message` and
//! `MessageDecoder` from `link_client.py`).
//!
//! The `/link-control` stream carries JSON objects, each prefixed with its
//! byte length as a little-endian `u32`. [`encode_message`] serialises a value
//! as compact JSON (CPython `json.dumps(separators=(",", ":"))` output, with
//! `ensure_ascii`) and prepends the prefix. [`MessageDecoder`] buffers inbound
//! bytes, splits them on the prefixes, drops empty payloads (keepalives) and
//! malformed or non-object messages, and refuses a length above
//! [`MAX_INBOUND_MESSAGE`] (the stream cannot be resynchronised past one, so
//! the caller closes the connection).
//!
//! # JSON parity
//!
//! [`encode_message`] reproduces CPython's compact `json.dumps` byte-for-byte
//! for the values the protocol uses (objects, arrays, strings, booleans,
//! null, and integers in `i64`/`u64` range): keys in insertion order, no
//! whitespace, non-ASCII and control characters escaped as `\uXXXX` (astral
//! scalars as surrogate pairs), and `/` left unescaped. Floating-point
//! formatting is *not* guaranteed identical to CPython; the control protocol
//! never sends floats.
//!
//! Decoding uses `serde_json`, which (unlike CPython's `json`) rejects `NaN`
//! and `Infinity`, lone surrogates, a leading byte-order mark, non-UTF-8
//! input, integers beyond 64 bits, and nesting deeper than 128. Each such
//! input is dropped rather than accepted; none occurs in the real protocol.

use serde_json::Value;

use crate::error::ControlError;

/// Largest inbound control message the decoder will accept (4 MiB).
pub const MAX_INBOUND_MESSAGE: u32 = 4 * 1024 * 1024;
/// The VM's documented per-message budget for messages the device sends.
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 256 * 1024;

/// Serialises `value` as compact JSON and prepends a little-endian `u32`
/// length prefix.
///
/// Returns [`ControlError::OutboundTooLarge`] if the JSON exceeds `u32::MAX`
/// bytes, and [`ControlError::Encode`] if `value` cannot be serialised.
pub fn encode_message(value: &Value) -> Result<Vec<u8>, ControlError> {
    let mut json = Vec::new();
    write_json(value, &mut json)?;
    let len = u32::try_from(json.len()).map_err(|_| ControlError::OutboundTooLarge(json.len()))?;
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

fn write_json(value: &Value, out: &mut Vec<u8>) -> Result<(), ControlError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Number(n) => out.extend_from_slice(format_number(n).as_bytes()),
        Value::String(s) => write_escaped(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_json(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_escaped(key, out);
                out.push(b':');
                write_json(item, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

fn format_number(n: &serde_json::Number) -> String {
    if let Some(v) = n.as_u64() {
        v.to_string()
    } else if let Some(v) = n.as_i64() {
        v.to_string()
    } else {
        // Floats: best effort only (see the `JSON parity` note). serde_json
        // already renders integral f64 without a fractional part, so append
        // `.0` to keep it a JSON number, matching the common case.
        let v = n.as_f64().unwrap_or(0.0);
        let s = v.to_string();
        if s.contains(['.', 'e', 'E', 'n', 'i']) {
            s
        } else {
            format!("{s}.0")
        }
    }
}

/// Writes a JSON string literal with CPython `ensure_ascii` escaping.
fn write_escaped(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{08}' => out.extend_from_slice(b"\\b"),
            '\u{0c}' => out.extend_from_slice(b"\\f"),
            c if ('\u{20}'..='\u{7e}').contains(&c) => out.push(c as u8),
            c => {
                let code = c as u32;
                if code <= 0xFFFF {
                    push_u_escape(code as u16, out);
                } else {
                    // Astral scalar: UTF-16 surrogate pair, as CPython emits.
                    let v = code - 0x10000;
                    push_u_escape(0xD800 + ((v >> 10) as u16), out);
                    push_u_escape(0xDC00 + ((v & 0x3FF) as u16), out);
                }
            }
        }
    }
    out.push(b'"');
}

fn push_u_escape(code: u16, out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.extend_from_slice(b"\\u");
    out.push(HEX[((code >> 12) & 0xF) as usize]);
    out.push(HEX[((code >> 8) & 0xF) as usize]);
    out.push(HEX[((code >> 4) & 0xF) as usize]);
    out.push(HEX[(code & 0xF) as usize]);
}

/// Splits the control stream into length-prefixed JSON objects.
///
/// Bytes are buffered until a whole message is available, so a message may
/// span several transport body chunks.
#[derive(Debug, Default)]
pub struct MessageDecoder {
    buf: Vec<u8>,
}

impl MessageDecoder {
    /// A decoder with an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `data` and returns every complete object it now contains.
    ///
    /// Empty payloads (keepalives) and messages that are malformed, non-UTF-8
    /// or not a JSON object are dropped silently. A length prefix above
    /// [`MAX_INBOUND_MESSAGE`] returns [`ControlError::InboundTooLarge`]; the
    /// caller must then close the connection, as the stream is unrecoverable.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Value>, ControlError> {
        self.buf.extend_from_slice(data);
        let mut messages = Vec::new();
        let mut consumed = 0;
        loop {
            let remaining = &self.buf[consumed..];
            if remaining.len() < 4 {
                break;
            }
            let length =
                u32::from_le_bytes([remaining[0], remaining[1], remaining[2], remaining[3]]);
            if length > MAX_INBOUND_MESSAGE {
                // Leave the buffer as-is; the connection is being torn down.
                return Err(ControlError::InboundTooLarge(length));
            }
            let length = length as usize;
            if remaining.len() < 4 + length {
                break;
            }
            let raw = &remaining[4..4 + length];
            consumed += 4 + length;
            if raw.is_empty() {
                continue; // keepalive
            }
            if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(raw) {
                messages.push(Value::Object(map));
            }
            // Non-object or malformed messages are dropped, as in the reference.
        }
        self.buf.drain(..consumed);
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encode_is_compact_with_length_prefix() {
        let out = encode_message(&json!({"a": 1, "b": [2, 3]})).unwrap();
        assert_eq!(&out[4..], br#"{"a":1,"b":[2,3]}"#);
        assert_eq!(&out[..4], &17u32.to_le_bytes());
    }

    #[test]
    fn decoder_splits_and_drops_non_objects() {
        let mut dec = MessageDecoder::new();
        let obj = encode_message(&json!({"ok": true})).unwrap();
        let arr = encode_message(&json!([1, 2])).unwrap();
        let mut feed = arr;
        feed.extend_from_slice(&obj);
        let out = dec.feed(&feed).unwrap();
        assert_eq!(out, vec![json!({"ok": true})]);
    }

    #[test]
    fn oversized_length_is_rejected() {
        let mut dec = MessageDecoder::new();
        let header = (MAX_INBOUND_MESSAGE + 1).to_le_bytes();
        assert!(matches!(
            dec.feed(&header),
            Err(ControlError::InboundTooLarge(_))
        ));
    }
}
