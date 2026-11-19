//! Fixed-width storage representation: how one value of a fixed-width type
//! is laid out in a row or a column, little-endian. Part of the on-disk
//! format; document 03 section 2.2.

use crate::{
    CANONICAL_NAN_F32, CANONICAL_NAN_F64, Error, I256, Reader, Type, U256, Value, canonical_f32,
    canonical_f64, decimal_width,
};

impl Type {
    /// Bytes of the type's storage representation, or `None` for the
    /// variable-length String and Bytes.
    pub fn fixed_width(&self) -> Option<u32> {
        Some(match self {
            Type::U8 | Type::I8 | Type::Bool | Type::Enum8 { .. } => 1,
            Type::U16 | Type::I16 | Type::Enum16 { .. } => 2,
            Type::U32 | Type::I32 | Type::F32 | Type::Date => 4,
            Type::U64 | Type::I64 | Type::F64 | Type::Timestamp(_) | Type::Duration(_) => 8,
            Type::U128 | Type::I128 | Type::Uuid | Type::Id128 => 16,
            Type::U256 | Type::I256 => 32,
            Type::Decimal { precision, .. } => decimal_width(*precision),
            Type::FixedBytes(n) => *n,
            Type::String { .. } | Type::Bytes { .. } => return None,
            Type::Nullable(inner) => 1 + inner.fixed_width()?,
            Type::LowCardinality(inner) => inner.fixed_width()?,
        })
    }
}

/// Appends the storage representation of `value`.
pub fn encode_fixed(ty: &Type, value: &Value, out: &mut Vec<u8>) -> Result<(), Error> {
    ty.check(value)?;
    let width = ty.fixed_width().ok_or(Error::NotFixedWidth)?;
    let start = out.len();
    let i256 = |x: &I256, out: &mut Vec<u8>| {
        out.extend(x.lo.to_le_bytes());
        out.extend(x.hi.to_le_bytes());
    };
    match (ty.logical(), value) {
        // A null is a zero flag byte and zeroed bytes in place of the value.
        (Type::Nullable(_), Value::Null) => out.resize(start + crate::usize32(width), 0),
        (Type::Nullable(inner), v) => {
            out.push(1);
            encode_fixed(inner, v, out)?;
        }
        (_, Value::U8(x) | Value::Enum8(x)) => out.push(*x),
        (_, Value::U16(x) | Value::Enum16(x)) => out.extend(x.to_le_bytes()),
        (_, Value::U32(x)) => out.extend(x.to_le_bytes()),
        (_, Value::U64(x)) => out.extend(x.to_le_bytes()),
        (_, Value::U128(x) | Value::Id128(x)) => out.extend(x.to_le_bytes()),
        (_, Value::U256(x)) => {
            out.extend(x.lo.to_le_bytes());
            out.extend(x.hi.to_le_bytes());
        }
        (_, Value::I8(x)) => out.extend(x.to_le_bytes()),
        (_, Value::I16(x)) => out.extend(x.to_le_bytes()),
        (_, Value::I32(x) | Value::Decimal32(x) | Value::Date(x)) => out.extend(x.to_le_bytes()),
        (_, Value::I64(x) | Value::Decimal64(x) | Value::Timestamp(x) | Value::Duration(x)) => {
            out.extend(x.to_le_bytes())
        }
        (_, Value::I128(x) | Value::Decimal128(x)) => out.extend(x.to_le_bytes()),
        (_, Value::I256(x) | Value::Decimal256(x)) => i256(x, out),
        (_, Value::F32(x)) => out.extend(canonical_f32(*x).to_bits().to_le_bytes()),
        (_, Value::F64(x)) => out.extend(canonical_f64(*x).to_bits().to_le_bytes()),
        (_, Value::Bool(b)) => out.push(u8::from(*b)),
        (_, Value::Uuid(b)) => out.extend(b),
        (_, Value::FixedBytes(b)) => out.extend(b),
        (_, Value::String(_) | Value::Bytes(_) | Value::Null) => {
            unreachable!("check() and fixed_width() reject them")
        }
    }
    assert_eq!(out.len() - start, crate::usize32(width));
    Ok(())
}

/// Decodes the storage representation of one value; `bytes` must be exactly
/// [`Type::fixed_width`] long.
pub fn decode_fixed(ty: &Type, bytes: &[u8]) -> Result<Value, Error> {
    ty.validate()?;
    let width = ty.fixed_width().ok_or(Error::NotFixedWidth)?;
    if bytes.len() != crate::usize32(width) {
        return Err(Error::Malformed);
    }
    let mut r = Reader {
        bytes,
        pos: 0,
        mask: 0,
    };
    let i256 = |r: &mut Reader| -> Result<I256, Error> {
        Ok(I256 {
            lo: u128::from_le_bytes(r.take()?),
            hi: i128::from_le_bytes(r.take()?),
        })
    };
    let value = match ty.logical() {
        Type::Nullable(inner) => match bytes[0] {
            0 if bytes[1..].iter().all(|b| *b == 0) => Value::Null,
            1 => decode_fixed(inner, &bytes[1..])?,
            _ => return Err(Error::Malformed),
        },
        Type::U8 => Value::U8(r.take::<1>()?[0]),
        Type::U16 => Value::U16(u16::from_le_bytes(r.take()?)),
        Type::U32 => Value::U32(u32::from_le_bytes(r.take()?)),
        Type::U64 => Value::U64(u64::from_le_bytes(r.take()?)),
        Type::U128 => Value::U128(u128::from_le_bytes(r.take()?)),
        Type::U256 => Value::U256(U256 {
            lo: u128::from_le_bytes(r.take()?),
            hi: u128::from_le_bytes(r.take()?),
        }),
        Type::I8 => Value::I8(i8::from_le_bytes(r.take()?)),
        Type::I16 => Value::I16(i16::from_le_bytes(r.take()?)),
        Type::I32 => Value::I32(i32::from_le_bytes(r.take()?)),
        Type::I64 => Value::I64(i64::from_le_bytes(r.take()?)),
        Type::I128 => Value::I128(i128::from_le_bytes(r.take()?)),
        Type::I256 => Value::I256(i256(&mut r)?),
        Type::Decimal { precision, .. } => match decimal_width(*precision) {
            4 => Value::Decimal32(i32::from_le_bytes(r.take()?)),
            8 => Value::Decimal64(i64::from_le_bytes(r.take()?)),
            16 => Value::Decimal128(i128::from_le_bytes(r.take()?)),
            _ => Value::Decimal256(i256(&mut r)?),
        },
        Type::F32 => {
            let bits = u32::from_le_bytes(r.take()?);
            let x = f32::from_bits(bits);
            if x.is_nan() && bits != CANONICAL_NAN_F32 {
                return Err(Error::Malformed);
            }
            Value::F32(x)
        }
        Type::F64 => {
            let bits = u64::from_le_bytes(r.take()?);
            let x = f64::from_bits(bits);
            if x.is_nan() && bits != CANONICAL_NAN_F64 {
                return Err(Error::Malformed);
            }
            Value::F64(x)
        }
        Type::Bool => match r.take()? {
            [0] => Value::Bool(false),
            [1] => Value::Bool(true),
            _ => return Err(Error::Malformed),
        },
        Type::Date => Value::Date(i32::from_le_bytes(r.take()?)),
        Type::Timestamp(_) => Value::Timestamp(i64::from_le_bytes(r.take()?)),
        Type::Duration(_) => Value::Duration(i64::from_le_bytes(r.take()?)),
        Type::Uuid => Value::Uuid(r.take()?),
        Type::Id128 => Value::Id128(u128::from_le_bytes(r.take()?)),
        Type::FixedBytes(n) => Value::FixedBytes(r.take_vec(crate::usize32(*n))?),
        Type::Enum8 { .. } => Value::Enum8(r.take::<1>()?[0]),
        Type::Enum16 { .. } => Value::Enum16(u16::from_le_bytes(r.take()?)),
        Type::String { .. } | Type::Bytes { .. } | Type::LowCardinality(_) => {
            unreachable!("fixed_width() and validate() reject them")
        }
    };
    ty.check(&value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_length_types_have_no_fixed_width() {
        let s = Type::String { max: 4 };
        assert_eq!(s.fixed_width(), None);
        let v = Value::String("ab".into());
        assert_eq!(
            encode_fixed(&s, &v, &mut Vec::new()),
            Err(Error::NotFixedWidth)
        );
        assert_eq!(decode_fixed(&s, &[]), Err(Error::NotFixedWidth));
    }

    #[test]
    fn nulls_are_zeroed_and_checked() {
        let ty = Type::Nullable(Box::new(Type::U32));
        let mut out = Vec::new();
        encode_fixed(&ty, &Value::Null, &mut out).unwrap();
        assert_eq!(out, [0; 5]);
        assert_eq!(decode_fixed(&ty, &[0, 0, 0, 1, 0]), Err(Error::Malformed));
        assert_eq!(decode_fixed(&ty, &[2, 0, 0, 0, 0]), Err(Error::Malformed));
        assert_eq!(decode_fixed(&ty, &[0; 4]), Err(Error::Malformed));
    }

    #[test]
    fn non_canonical_nan_is_rejected_on_read() {
        let bits = (f64::NAN.to_bits() | 1).to_le_bytes();
        assert_eq!(decode_fixed(&Type::F64, &bits), Err(Error::Malformed));
    }
}
