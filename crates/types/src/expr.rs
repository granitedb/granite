//! What the program IR (document 03 section 5.1) and the scan fragment IR
//! (document 05 section 4) share: the opcodes and operands of their common
//! expression elements, the binary operators, the encoding of a literal's
//! value, and the type classes their type rules use (document 03 section 5.2).

use crate::storage::{decode_fixed, encode_fixed};
use crate::{Type, Value};

/// Opcodes of the shared expression elements; both IRs give them these
/// numbers and operands.
pub mod opcode {
    /// type, value.
    pub const LITERAL: u8 = 0;
    /// expr.
    pub const NEG: u8 = 6;
    /// expr.
    pub const NOT: u8 = 7;
    /// expr.
    pub const IS_NULL: u8 = 8;
    /// expr, expr.
    pub const UNWRAP_OR: u8 = 9;
    /// expr, type.
    pub const CAST: u8 = 11;
    /// expr, expr: the first of the 13 binary operators, in [`super::BinOp::ALL`] order.
    pub const BINARY: u8 = 12;
}

/// The binary operators, in the order of their opcodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    /// Every operator, by opcode from [`opcode::BINARY`].
    pub const ALL: [BinOp; 13] = [
        BinOp::Add,
        BinOp::Sub,
        BinOp::Mul,
        BinOp::Div,
        BinOp::Rem,
        BinOp::Eq,
        BinOp::Ne,
        BinOp::Lt,
        BinOp::Le,
        BinOp::Gt,
        BinOp::Ge,
        BinOp::And,
        BinOp::Or,
    ];

    pub fn opcode(self) -> u8 {
        let i = BinOp::ALL.iter().position(|o| *o == self);
        opcode::BINARY + u8::try_from(i.expect("every operator is listed")).expect("13 operators")
    }

    pub fn from_opcode(op: u8) -> Option<BinOp> {
        op.checked_sub(opcode::BINARY)
            .and_then(|i| BinOp::ALL.get(usize::from(i)).copied())
    }
}

/// Why bytes are not a literal's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralError {
    /// The bytes end inside the value.
    Truncated,
    /// Not a valid value of the type, or not in its canonical form.
    Invalid,
}

/// Appends a literal's value: the storage representation of a fixed-width
/// type; otherwise LowCardinality(T) as T, a Nullable flag (then T's value
/// when present), or a u32 length and the bytes. Fails unless the value is
/// a valid value of `ty` (`Type::check`).
pub fn encode_literal(ty: &Type, value: &Value, out: &mut Vec<u8>) -> Result<(), LiteralError> {
    ty.check(value).map_err(|_| LiteralError::Invalid)?;
    if ty.fixed_width().is_some() {
        return encode_fixed(ty, value, out).map_err(|_| LiteralError::Invalid);
    }
    let bytes = |b: &[u8], out: &mut Vec<u8>| {
        let len = u32::try_from(b.len()).map_err(|_| LiteralError::Invalid)?;
        out.extend(len.to_le_bytes());
        out.extend(b);
        Ok(())
    };
    match (ty, value) {
        (Type::LowCardinality(inner), v) => encode_literal(inner, v, out),
        (Type::Nullable(_), Value::Null) => {
            out.push(0);
            Ok(())
        }
        (Type::Nullable(inner), v) => {
            out.push(1);
            encode_literal(inner, v, out)
        }
        (_, Value::String(s)) => bytes(s.as_bytes(), out),
        (_, Value::Bytes(b)) => bytes(b, out),
        _ => unreachable!("check() matched the value to its variable-length type"),
    }
}

/// Decodes a literal's value of `ty`, a valid type, from the start of
/// `bytes`; returns it and the bytes it took. Accepts exactly what
/// [`encode_literal`] writes.
pub fn decode_literal(ty: &Type, bytes: &[u8]) -> Result<(Value, usize), LiteralError> {
    let mut pos = 0;
    let value = literal(ty, bytes, &mut pos)?;
    Ok((value, pos))
}

fn literal(ty: &Type, bytes: &[u8], pos: &mut usize) -> Result<Value, LiteralError> {
    let mut slice = |n: usize| {
        let end = pos.checked_add(n).ok_or(LiteralError::Truncated)?;
        let s = bytes.get(*pos..end).ok_or(LiteralError::Truncated)?;
        *pos = end;
        Ok(s)
    };
    let value = match (ty.fixed_width(), ty) {
        (Some(width), _) => {
            let width = usize::try_from(width).expect("u32 fits usize");
            decode_fixed(ty, slice(width)?).map_err(|_| LiteralError::Invalid)?
        }
        (None, Type::LowCardinality(inner)) => literal(inner, bytes, pos)?,
        (None, Type::Nullable(inner)) => match slice(1)?[0] {
            0 => Value::Null,
            1 => literal(inner, bytes, pos)?,
            _ => return Err(LiteralError::Invalid),
        },
        (None, _) => {
            let len = u32::from_le_bytes(slice(4)?.try_into().expect("4 bytes"));
            let content = slice(usize::try_from(len).map_err(|_| LiteralError::Truncated)?)?;
            match ty {
                Type::String { .. } => Value::String(
                    String::from_utf8(content.to_vec()).map_err(|_| LiteralError::Invalid)?,
                ),
                _ => Value::Bytes(content.to_vec()),
            }
        }
    };
    ty.check(&value).map_err(|_| LiteralError::Invalid)?;
    Ok(value)
}

/// Integers and decimals, which have arithmetic.
pub fn arithmetic(t: &Type) -> bool {
    !matches!(
        t,
        Type::F32
            | Type::F64
            | Type::Bool
            | Type::Date
            | Type::Timestamp(_)
            | Type::Duration(_)
            | Type::Uuid
            | Type::Id128
            | Type::String { .. }
            | Type::Bytes { .. }
            | Type::FixedBytes(_)
            | Type::Enum8 { .. }
            | Type::Enum16 { .. }
            | Type::Nullable(_)
            | Type::LowCardinality(_)
    )
}

/// The types between which a cast exists: integers, decimals and floats.
pub fn numeric(t: &Type) -> bool {
    arithmetic(t) || matches!(t, Type::F32 | Type::F64)
}

/// The logical type: every LowCardinality hint removed, which does not
/// change the type of a value.
pub fn logical(t: &Type) -> Type {
    match t {
        Type::LowCardinality(inner) => logical(inner),
        Type::Nullable(inner) => Type::Nullable(Box::new(logical(inner))),
        t => t.clone(),
    }
}
