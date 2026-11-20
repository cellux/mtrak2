//! Low-level byte codecs shared by the event model and file I/O.
//!
//! Standard MIDI Files encode lengths and delta-times as variable-length
//! quantities (VLQ). The encoder here is deliberately independent of any
//! third-party crate so the core event model can round-trip bytes on its own.

/// Reads a variable-length quantity (1–4 bytes) from `input`.
///
/// Returns the decoded value and the number of bytes consumed.
pub fn read_vlq(input: &[u8]) -> Result<(u32, usize), VlqError> {
    let mut value: u32 = 0;
    for (i, &byte) in input.iter().enumerate() {
        if i >= 4 {
            return Err(VlqError::TooLong);
        }
        value = (value << 7) | u32::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    Err(VlqError::UnexpectedEof)
}

/// Appends `value` to `out` as a variable-length quantity.
pub fn write_vlq(value: u32, out: &mut Vec<u8>) {
    let mut buffer = [0u8; 4];
    let mut idx = buffer.len();
    let mut v = value;
    loop {
        idx -= 1;
        buffer[idx] = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    for (i, byte) in buffer[idx..].iter().enumerate() {
        let last = i == buffer.len() - idx - 1;
        out.push(if last { *byte } else { *byte | 0x80 });
    }
}

/// Errors produced while decoding a variable-length quantity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VlqError {
    /// The input ended before a terminating byte was found.
    #[error("unexpected end of input while reading a variable-length quantity")]
    UnexpectedEof,
    /// The quantity is longer than the four bytes permitted by the format.
    #[error("variable-length quantity is longer than four bytes")]
    TooLong,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_round_trips_known_values() {
        let cases: &[(u32, &[u8])] = &[
            (0x00, &[0x00]),
            (0x40, &[0x40]),
            (0x7F, &[0x7F]),
            (0x80, &[0x81, 0x00]),
            (0x2000, &[0xC0, 0x00]),
            (0x3FFF, &[0xFF, 0x7F]),
            (0x100000, &[0xC0, 0x80, 0x00]),
            (0x0FFFFFFF, &[0xFF, 0xFF, 0xFF, 0x7F]),
        ];
        for (value, bytes) in cases {
            let mut out = Vec::new();
            write_vlq(*value, &mut out);
            assert_eq!(&out, bytes, "encoding {value:#X}");
            let (decoded, used) = read_vlq(bytes).unwrap();
            assert_eq!(decoded, *value);
            assert_eq!(used, bytes.len());
        }
    }

    #[test]
    fn vlq_rejects_truncated_and_overlong_input() {
        assert_eq!(read_vlq(&[0x80]), Err(VlqError::UnexpectedEof));
        assert_eq!(
            read_vlq(&[0x80, 0x80, 0x80, 0x80, 0x00]),
            Err(VlqError::TooLong)
        );
        assert_eq!(read_vlq(&[]), Err(VlqError::UnexpectedEof));
    }
}
