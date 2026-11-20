//! Order-preserving key encoding: for two keys of the same columns, the
//! byte comparison of their encodings equals the logical comparison of the
//! values. Part of the on-disk format; document 03 section 2.1.

use crate::{Error, I256, Reader, Type, U256, Value, decimal_width};
use granite_base::constants::FORMAT;

/// Sort order of one key column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Asc,
    /// Every byte of the column's ascending encoding inverted.
    Desc,
}

/// Encodes a key: the columns' encodings concatenated in order.
pub fn encode_key(columns: &[(Type, Order)], values: &[Value]) -> Result<Vec<u8>, Error> {
    if columns.len() != values.len() {
        return Err(Error::ColumnCount);
    }
    let mut out = Vec::new();
    for ((ty, order), value) in columns.iter().zip(values) {
        let start = out.len();
        encode_value(ty, value, &mut out)?;
        if *order == Order::Desc {
            out[start..].iter_mut().for_each(|b| *b = !*b);
        }
    }
    Ok(out)
}

/// [`encode_key`], rejecting a key longer than `FORMAT.primary_key_size_max`.
pub fn encode_primary_key(columns: &[(Type, Order)], values: &[Value]) -> Result<Vec<u8>, Error> {
    let key = encode_key(columns, values)?;
    let size = crate::len64(key.len());
    if size > FORMAT.primary_key_size_max {
        return Err(Error::KeyTooLarge { size });
    }
    Ok(key)
}

/// Decodes a whole key; trailing bytes are an error.
pub fn decode_key(columns: &[(Type, Order)], bytes: &[u8]) -> Result<Vec<Value>, Error> {
    let mut r = Reader {
        bytes,
        pos: 0,
        mask: 0,
    };
    let mut values = Vec::with_capacity(columns.len());
    for (ty, order) in columns {
        r.mask = if *order == Order::Desc { u8::MAX } else { 0 };
        let value = decode_value(ty, &mut r)?;
        ty.check_decoded(&value)?;
        values.push(value);
    }
    if r.pos != bytes.len() {
        return Err(Error::Malformed);
    }
    Ok(values)
}

impl Type {
    /// The longest key encoding of a value of this type: its fixed width, or
    /// `2 * max + 2` for String and Bytes, whose every byte may be escaped.
    pub fn key_size_max(&self) -> Result<u64, Error> {
        allowed(self)?;
        Ok(match self.logical() {
            Type::String { max } | Type::Bytes { max } => 2 * u64::from(*max) + 2,
            ty => u64::from(
                ty.fixed_width()
                    .expect("every other key type is fixed-width"),
            ),
        })
    }
}

fn allowed(ty: &Type) -> Result<(), Error> {
    ty.validate()?;
    match ty.logical() {
        Type::F32 | Type::F64 | Type::Nullable(_) => Err(Error::NotAllowedInKey),
        _ => Ok(()),
    }
}

fn encode_value(ty: &Type, value: &Value, out: &mut Vec<u8>) -> Result<(), Error> {
    allowed(ty)?;
    ty.check(value)?;
    let i256 = |x: &I256, out: &mut Vec<u8>| {
        out.extend((x.hi ^ i128::MIN).to_be_bytes());
        out.extend(x.lo.to_be_bytes());
    };
    // Unsigned integers big-endian; signed ones with the sign bit flipped.
    match value {
        Value::U8(x) | Value::Enum8(x) => out.push(*x),
        Value::U16(x) | Value::Enum16(x) => out.extend(x.to_be_bytes()),
        Value::U32(x) => out.extend(x.to_be_bytes()),
        Value::U64(x) => out.extend(x.to_be_bytes()),
        Value::U128(x) | Value::Id128(x) => out.extend(x.to_be_bytes()),
        Value::U256(x) => {
            out.extend(x.hi.to_be_bytes());
            out.extend(x.lo.to_be_bytes());
        }
        Value::I8(x) => out.extend((x ^ i8::MIN).to_be_bytes()),
        Value::I16(x) => out.extend((x ^ i16::MIN).to_be_bytes()),
        Value::I32(x) | Value::Decimal32(x) | Value::Date(x) => {
            out.extend((x ^ i32::MIN).to_be_bytes())
        }
        Value::I64(x) | Value::Decimal64(x) | Value::Timestamp(x) | Value::Duration(x) => {
            out.extend((x ^ i64::MIN).to_be_bytes())
        }
        Value::I128(x) | Value::Decimal128(x) => out.extend((x ^ i128::MIN).to_be_bytes()),
        Value::I256(x) | Value::Decimal256(x) => i256(x, out),
        Value::Bool(b) => out.push(u8::from(*b)),
        Value::Uuid(b) => out.extend(b),
        Value::FixedBytes(b) => out.extend(b),
        Value::String(s) => escape(s.as_bytes(), out),
        Value::Bytes(b) => escape(b, out),
        Value::F32(_) | Value::F64(_) | Value::Null => unreachable!("allowed() rejects them"),
    }
    Ok(())
}

/// Each 0x00 becomes 0x00 0xFF, and 0x00 0x00 ends the value, so no
/// encoding is a prefix of another and a shorter value sorts first.
fn escape(bytes: &[u8], out: &mut Vec<u8>) {
    for &b in bytes {
        out.push(b);
        if b == 0 {
            out.push(u8::MAX);
        }
    }
    out.extend([0, 0]);
}

fn unescape(r: &mut Reader) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    loop {
        match r.take()? {
            [0] => match r.take()? {
                [0] => return Ok(out),
                [u8::MAX] => out.push(0),
                _ => return Err(Error::Malformed),
            },
            [b] => out.push(b),
        }
    }
}

fn decode_value(ty: &Type, r: &mut Reader) -> Result<Value, Error> {
    allowed(ty)?;
    let i256 = |r: &mut Reader| -> Result<I256, Error> {
        Ok(I256 {
            hi: i128::from_be_bytes(r.take()?) ^ i128::MIN,
            lo: u128::from_be_bytes(r.take()?),
        })
    };
    Ok(match ty.logical() {
        Type::U8 => Value::U8(u8::from_be_bytes(r.take()?)),
        Type::U16 => Value::U16(u16::from_be_bytes(r.take()?)),
        Type::U32 => Value::U32(u32::from_be_bytes(r.take()?)),
        Type::U64 => Value::U64(u64::from_be_bytes(r.take()?)),
        Type::U128 => Value::U128(u128::from_be_bytes(r.take()?)),
        Type::U256 => Value::U256(U256 {
            hi: u128::from_be_bytes(r.take()?),
            lo: u128::from_be_bytes(r.take()?),
        }),
        Type::I8 => Value::I8(i8::from_be_bytes(r.take()?) ^ i8::MIN),
        Type::I16 => Value::I16(i16::from_be_bytes(r.take()?) ^ i16::MIN),
        Type::I32 => Value::I32(i32::from_be_bytes(r.take()?) ^ i32::MIN),
        Type::I64 => Value::I64(i64::from_be_bytes(r.take()?) ^ i64::MIN),
        Type::I128 => Value::I128(i128::from_be_bytes(r.take()?) ^ i128::MIN),
        Type::I256 => Value::I256(i256(r)?),
        Type::Decimal { precision, .. } => match decimal_width(*precision) {
            4 => Value::Decimal32(i32::from_be_bytes(r.take()?) ^ i32::MIN),
            8 => Value::Decimal64(i64::from_be_bytes(r.take()?) ^ i64::MIN),
            16 => Value::Decimal128(i128::from_be_bytes(r.take()?) ^ i128::MIN),
            _ => Value::Decimal256(i256(r)?),
        },
        Type::Bool => match r.take()? {
            [0] => Value::Bool(false),
            [1] => Value::Bool(true),
            _ => return Err(Error::Malformed),
        },
        Type::Date => Value::Date(i32::from_be_bytes(r.take()?) ^ i32::MIN),
        Type::Timestamp(_) => Value::Timestamp(i64::from_be_bytes(r.take()?) ^ i64::MIN),
        Type::Duration(_) => Value::Duration(i64::from_be_bytes(r.take()?) ^ i64::MIN),
        Type::Uuid => Value::Uuid(r.take()?),
        Type::Id128 => Value::Id128(u128::from_be_bytes(r.take()?)),
        Type::String { .. } => {
            Value::String(String::from_utf8(unescape(r)?).map_err(|_| Error::InvalidUtf8)?)
        }
        Type::Bytes { .. } => Value::Bytes(unescape(r)?),
        Type::FixedBytes(n) => Value::FixedBytes(r.take_vec(crate::usize32(*n))?),
        Type::Enum8 { .. } => Value::Enum8(u8::from_be_bytes(r.take()?)),
        Type::Enum16 { .. } => Value::Enum16(u16::from_be_bytes(r.take()?)),
        Type::F32 | Type::F64 | Type::Nullable(_) | Type::LowCardinality(_) => {
            unreachable!("allowed() rejects floats, nullables and nested hints")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asc(ty: Type) -> [(Type, Order); 1] {
        [(ty, Order::Asc)]
    }

    #[test]
    fn floats_and_nullables_are_rejected_in_keys() {
        for (ty, v) in [
            (Type::F32, Value::F32(1.0)),
            (Type::F64, Value::F64(1.0)),
            (Type::Nullable(Box::new(Type::U8)), Value::U8(1)),
            (Type::Nullable(Box::new(Type::U8)), Value::Null),
            (
                Type::LowCardinality(Box::new(Type::Nullable(Box::new(Type::U8)))),
                Value::Null,
            ),
        ] {
            assert_eq!(
                encode_key(&asc(ty.clone()), &[v]),
                Err(Error::NotAllowedInKey)
            );
            assert_eq!(decode_key(&asc(ty), &[0]), Err(Error::NotAllowedInKey));
        }
    }

    #[test]
    fn primary_key_size_is_bounded() {
        let max = FORMAT.primary_key_size_max as u32;
        let cols = asc(Type::FixedBytes(max));
        let fits = encode_primary_key(&cols, &[Value::FixedBytes(vec![7; max as usize])]);
        assert_eq!(
            fits.map(|k| k.len() as u64),
            Ok(FORMAT.primary_key_size_max)
        );
        let cols = asc(Type::FixedBytes(max + 1));
        let over = encode_primary_key(&cols, &[Value::FixedBytes(vec![7; max as usize + 1])]);
        assert_eq!(
            over,
            Err(Error::KeyTooLarge {
                size: FORMAT.primary_key_size_max + 1
            })
        );
        // Escaping counts: a string within its own max can still overflow the key.
        let cols = asc(Type::String { max });
        let zeros = "\0".repeat(max as usize / 2);
        let size = (max / 2 * 2 + 2) as u64;
        assert_eq!(
            encode_primary_key(&cols, &[Value::String(zeros)]),
            Err(Error::KeyTooLarge { size })
        );
    }

    #[test]
    fn key_size_max_is_reached() {
        let zeros = Value::Bytes(vec![0; 3]);
        let ty = Type::LowCardinality(Box::new(Type::Bytes { max: 3 }));
        let key = encode_key(&asc(ty.clone()), &[zeros]).unwrap();
        assert_eq!(ty.key_size_max(), Ok(key.len() as u64));
        assert_eq!(
            Type::Decimal {
                precision: 20,
                scale: 0
            }
            .key_size_max(),
            Ok(16)
        );
        assert_eq!(Type::F64.key_size_max(), Err(Error::NotAllowedInKey));
    }

    #[test]
    fn malformed_keys_are_rejected() {
        let s = asc(Type::Bytes { max: 8 });
        assert_eq!(decode_key(&s, &[1, 0]), Err(Error::Malformed)); // no terminator
        assert_eq!(decode_key(&s, &[0, 1]), Err(Error::Malformed)); // bad escape
        assert_eq!(decode_key(&s, &[0, 0, 9]), Err(Error::Malformed)); // trailing byte
        let long = [1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0];
        assert_eq!(decode_key(&s, &long), Err(Error::Length { len: 9, max: 8 }));
        let text = asc(Type::String { max: 8 });
        assert_eq!(decode_key(&text, &[0xC3, 0, 0]), Err(Error::InvalidUtf8));
        assert_eq!(decode_key(&asc(Type::Bool), &[2]), Err(Error::Malformed));
        let e = asc(Type::Enum8 { max: 2 });
        assert_eq!(decode_key(&e, &[3]), Err(Error::UnknownEnumValue));
        assert_eq!(
            decode_key(&asc(Type::U32), &[0, 0, 0]),
            Err(Error::Malformed)
        );
        assert_eq!(encode_key(&asc(Type::U32), &[]), Err(Error::ColumnCount));
        assert_eq!(
            encode_key(&asc(Type::U32), &[Value::U16(1)]),
            Err(Error::Mismatch)
        );
    }
}
