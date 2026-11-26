//! Minimal protobuf wire-format reader for schemaless decoding.

pub enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

fn uvarint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut x = 0u64;
    let mut shift = 0;
    while *i < b.len() {
        let c = b[*i];
        *i += 1;
        if shift >= 64 {
            return None;
        }
        x |= ((c & 0x7f) as u64) << shift;
        if c < 0x80 {
            return Some(x);
        }
        shift += 7;
    }
    None
}

/// Calls `f(field_number, value)` for each field; returns false on malformed input.
pub fn walk<'a>(b: &'a [u8], mut f: impl FnMut(u32, Field<'a>)) -> bool {
    let mut i = 0;
    while i < b.len() {
        let Some(tag) = uvarint(b, &mut i) else { return false };
        let num = (tag >> 3) as u32;
        if num == 0 {
            return false;
        }
        match tag & 7 {
            0 => {
                let Some(v) = uvarint(b, &mut i) else { return false };
                f(num, Field::Varint(v));
            }
            1 => i += 8,
            2 => {
                let Some(len) = uvarint(b, &mut i) else { return false };
                let len = len as usize;
                if len > b.len() - i {
                    return false;
                }
                f(num, Field::Bytes(&b[i..i + len]));
                i += len;
            }
            5 => i += 4,
            _ => return false,
        }
        if i > b.len() {
            return false;
        }
    }
    true
}

/// Reads a google.protobuf.Timestamp as unix milliseconds.
pub fn timestamp_ms(b: &[u8]) -> Option<i64> {
    let (mut secs, mut nanos) = (0u64, 0u64);
    walk(b, |n, v| {
        if let Field::Varint(x) = v {
            match n {
                1 => secs = x,
                2 => nanos = x,
                _ => {}
            }
        }
    });
    (secs > 0 && secs < 1 << 40 && nanos < 1_000_000_000).then(|| (secs * 1000 + nanos / 1_000_000) as i64)
}

/// Returns the first length-delimited field `num`.
pub fn bytes(b: &[u8], num: u32) -> Option<&[u8]> {
    let mut out = None;
    walk(b, |n, v| {
        if let (true, Field::Bytes(d), None) = (n == num, v, &out) {
            out = Some(d);
        }
    });
    out
}
