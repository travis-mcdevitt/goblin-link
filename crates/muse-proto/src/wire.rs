//! Minimal protobuf-style field codec (port of `noise/_proto.py`).
//!
//! Only what the Hatch Noise messages need: varints, field keys, signed and
//! unsigned integers, booleans and length-delimited fields. Encoders append
//! to a `Vec<u8>`; decoders take a byte slice and an offset and return the new
//! offset, never reading past the end of the slice.
//!
//! Rules carried over from the reference:
//!
//! * a varint is at most 10 bytes, and its 10th byte may only be `0x00` or
//!   `0x01` (so the value fits in 64 bits);
//! * non-minimal varints (for example `80 00` for zero) are accepted;
//! * field numbers must be in `1..=2^29-1` and outside the reserved range
//!   `19000..=19999`, on both encode and decode;
//! * wire types 3 and 4 (groups) and 6, 7 are rejected, not skipped;
//! * `int32` values are sign-extended to 64 bits on the wire (10 bytes when
//!   negative), exactly like protobuf.

use crate::error::WireError;

/// Largest legal protobuf field number.
pub const MAX_FIELD_NUMBER: u32 = (1 << 29) - 1;
/// First field number reserved by protobuf.
pub const FIRST_RESERVED_FIELD_NUMBER: u32 = 19000;
/// Last field number reserved by protobuf.
pub const LAST_RESERVED_FIELD_NUMBER: u32 = 19999;

/// Maximum encoded length of a varint.
const MAX_VARINT_BYTES: usize = 10;

/// Protobuf wire types accepted by the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    /// Varint (`0`).
    Varint = 0,
    /// Little-endian 64-bit (`1`), only ever skipped.
    Fixed64 = 1,
    /// Length-delimited (`2`).
    Delimited = 2,
    /// Little-endian 32-bit (`5`), only ever skipped.
    Fixed32 = 5,
}

impl TryFrom<u64> for WireType {
    type Error = WireError;

    fn try_from(value: u64) -> Result<Self, WireError> {
        match value {
            0 => Ok(Self::Varint),
            1 => Ok(Self::Fixed64),
            2 => Ok(Self::Delimited),
            5 => Ok(Self::Fixed32),
            _ => Err(WireError::InvalidWireType),
        }
    }
}

fn valid_field_number(field: u64) -> bool {
    field != 0
        && field <= u64::from(MAX_FIELD_NUMBER)
        && !(u64::from(FIRST_RESERVED_FIELD_NUMBER)..=u64::from(LAST_RESERVED_FIELD_NUMBER))
            .contains(&field)
}

/// Appends `value` as a varint.
pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Appends a field key, validating the field number.
pub fn encode_key(field: u32, wire: WireType, out: &mut Vec<u8>) -> Result<(), WireError> {
    if !valid_field_number(u64::from(field)) {
        return Err(WireError::InvalidFieldNumber);
    }
    encode_varint((u64::from(field) << 3) | wire as u64, out);
    Ok(())
}

/// Appends a key for one of this crate's own (known-valid) field numbers.
fn put_key(field: u32, wire: WireType, out: &mut Vec<u8>) {
    debug_assert!(valid_field_number(u64::from(field)));
    encode_varint((u64::from(field) << 3) | wire as u64, out);
}

/// Appends an unsigned varint field.
pub(crate) fn varint_field(field: u32, value: u64, out: &mut Vec<u8>) {
    put_key(field, WireType::Varint, out);
    encode_varint(value, out);
}

/// Appends an `int64` field (two's complement, 10 bytes when negative).
pub(crate) fn int64_field(field: u32, value: i64, out: &mut Vec<u8>) {
    varint_field(field, value as u64, out);
}

/// Appends an `int32` field, sign-extended to 64 bits as protobuf does.
pub(crate) fn int32_field(field: u32, value: i32, out: &mut Vec<u8>) {
    varint_field(field, i64::from(value) as u64, out);
}

/// Appends a `uint32` field.
pub(crate) fn uint32_field(field: u32, value: u32, out: &mut Vec<u8>) {
    varint_field(field, u64::from(value), out);
}

/// Appends a `bool` field.
pub(crate) fn bool_field(field: u32, value: bool, out: &mut Vec<u8>) {
    varint_field(field, u64::from(value), out);
}

/// Appends a length-delimited (`bytes`, `string` or sub-message) field.
pub(crate) fn delimited_field(field: u32, payload: &[u8], out: &mut Vec<u8>) {
    put_key(field, WireType::Delimited, out);
    encode_varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

/// Encodes a single field with a caller-chosen field number.
///
/// This is the checked counterpart of the internal helpers, matching the
/// reference's `*_field` functions (which validate the field number).
pub fn encode_field(field: u32, value: FieldValue<'_>, out: &mut Vec<u8>) -> Result<(), WireError> {
    if !valid_field_number(u64::from(field)) {
        return Err(WireError::InvalidFieldNumber);
    }
    match value {
        FieldValue::Varint(v) => varint_field(field, v, out),
        FieldValue::Int64(v) => int64_field(field, v, out),
        FieldValue::Int32(v) => int32_field(field, v, out),
        FieldValue::Uint32(v) => uint32_field(field, v, out),
        FieldValue::Bool(v) => bool_field(field, v, out),
        FieldValue::Bytes(v) => delimited_field(field, v, out),
        FieldValue::String(v) => delimited_field(field, v.as_bytes(), out),
    }
    Ok(())
}

/// A typed value for [`encode_field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldValue<'a> {
    /// Raw unsigned varint.
    Varint(u64),
    /// Signed 64-bit integer.
    Int64(i64),
    /// Signed 32-bit integer (sign-extended on the wire).
    Int32(i32),
    /// Unsigned 32-bit integer.
    Uint32(u32),
    /// Boolean (`0` or `1`).
    Bool(bool),
    /// Length-delimited bytes.
    Bytes(&'a [u8]),
    /// Length-delimited UTF-8 string.
    String(&'a str),
}

/// Reads a varint at `offset`; returns the value and the offset after it.
pub fn read_varint(data: &[u8], mut offset: usize) -> Result<(u64, usize), WireError> {
    let mut value: u64 = 0;
    for i in 0..MAX_VARINT_BYTES {
        let byte = *data.get(offset).ok_or(WireError::TruncatedVarint)?;
        offset += 1;
        if i == MAX_VARINT_BYTES - 1 && (byte & 0xFE) != 0 {
            return Err(WireError::MalformedVarint);
        }
        value |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((value, offset));
        }
    }
    Err(WireError::MalformedVarint)
}

/// Reads and validates a field key; returns `(field, wire type, offset)`.
pub fn read_key(data: &[u8], offset: usize) -> Result<(u32, WireType, usize), WireError> {
    let (key, offset) = read_varint(data, offset)?;
    let field = key >> 3;
    if !valid_field_number(field) {
        return Err(WireError::InvalidFieldNumber);
    }
    let wire = WireType::try_from(key & 0x07)?;
    let field = u32::try_from(field).map_err(|_| WireError::InvalidFieldNumber)?;
    Ok((field, wire, offset))
}

/// Reads a length-delimited field body; returns the slice and the end offset.
pub fn read_delimited(data: &[u8], offset: usize) -> Result<(&[u8], usize), WireError> {
    let (length, offset) = read_varint(data, offset)?;
    let end = usize::try_from(length)
        .ok()
        .and_then(|len| offset.checked_add(len))
        .ok_or(WireError::TruncatedDelimited)?;
    let body = data.get(offset..end).ok_or(WireError::TruncatedDelimited)?;
    Ok((body, end))
}

/// Skips a field of the given wire type; returns the offset after it.
pub fn skip_field(data: &[u8], offset: usize, wire: WireType) -> Result<usize, WireError> {
    let fixed = |width: usize, error: WireError| {
        offset
            .checked_add(width)
            .filter(|end| *end <= data.len())
            .ok_or(error)
    };
    match wire {
        WireType::Varint => read_varint(data, offset).map(|(_, end)| end),
        WireType::Fixed64 => fixed(8, WireError::TruncatedFixed64),
        WireType::Delimited => read_delimited(data, offset).map(|(_, end)| end),
        WireType::Fixed32 => fixed(4, WireError::TruncatedFixed32),
    }
}

/// Interprets a varint as `int64` (two's complement).
pub fn decode_int64(raw: u64) -> i64 {
    raw as i64
}

/// Interprets a varint as `int32`; values outside `i32` are rejected.
pub fn decode_int32(raw: u64) -> Result<i32, WireError> {
    i32::try_from(raw as i64).map_err(|_| WireError::Int32OutOfRange)
}

/// Interprets a varint as `uint32`; values above `u32::MAX` are rejected.
pub fn decode_uint32(raw: u64) -> Result<u32, WireError> {
    u32::try_from(raw).map_err(|_| WireError::Uint32OutOfRange)
}

/// Decodes a length-delimited field as UTF-8.
pub(crate) fn decode_string(raw: &[u8]) -> Result<String, WireError> {
    std::str::from_utf8(raw)
        .map(str::to_owned)
        .map_err(|_| WireError::InvalidUtf8)
}

/// Iterates the fields of one message, yielding `(field, wire, offset)` with
/// the offset positioned at the field's value.
///
/// The caller must advance the cursor past the value with one of the
/// `take_*` methods or [`FieldCursor::skip`] before calling
/// [`FieldCursor::next_key`] again.
pub(crate) struct FieldCursor<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> FieldCursor<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    /// Reads the next key, or `None` at the end of the message.
    pub(crate) fn next_key(&mut self) -> Result<Option<(u32, WireType)>, WireError> {
        if self.offset >= self.data.len() {
            return Ok(None);
        }
        let (field, wire, offset) = read_key(self.data, self.offset)?;
        self.offset = offset;
        Ok(Some((field, wire)))
    }

    /// Reads a varint value, failing with `wrong` unless `wire` is a varint.
    pub(crate) fn take_varint(
        &mut self,
        wire: WireType,
        wrong: &'static str,
    ) -> Result<u64, WireError> {
        if wire != WireType::Varint {
            return Err(WireError::WrongWireType(wrong));
        }
        let (value, offset) = read_varint(self.data, self.offset)?;
        self.offset = offset;
        Ok(value)
    }

    /// Reads a delimited value, failing with `wrong` unless `wire` is delimited.
    pub(crate) fn take_delimited(
        &mut self,
        wire: WireType,
        wrong: &'static str,
    ) -> Result<&'a [u8], WireError> {
        if wire != WireType::Delimited {
            return Err(WireError::WrongWireType(wrong));
        }
        let (value, offset) = read_delimited(self.data, self.offset)?;
        self.offset = offset;
        Ok(value)
    }

    /// Skips an unknown field.
    pub(crate) fn skip(&mut self, wire: WireType) -> Result<(), WireError> {
        self.offset = skip_field(self.data, self.offset, wire)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips_edges() {
        for value in [0, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
            let mut out = Vec::new();
            encode_varint(value, &mut out);
            assert!(out.len() <= MAX_VARINT_BYTES);
            assert_eq!(read_varint(&out, 0), Ok((value, out.len())));
        }
    }

    #[test]
    fn negative_int32_uses_ten_bytes() {
        let mut out = Vec::new();
        int32_field(1, -1, &mut out);
        assert_eq!(out.len(), 11);
    }

    #[test]
    fn delimited_length_overflow_is_truncation() {
        // Length u64::MAX cannot fit after the offset.
        let data = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        assert_eq!(read_delimited(&data, 0), Err(WireError::TruncatedDelimited));
    }
}
