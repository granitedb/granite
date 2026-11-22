//! The binary form of a program, document 03 section 5.1. Integers are
//! little-endian. The encoding is canonical: a tree has exactly one
//! encoding, and bytes the decoder accepts are the encoding of the tree it
//! returns.

use crate::{BinOp, Error, Expr, IR_VERSION, Param, Program, Scan, Stmt};
use granite_base::constants::FORMAT;
use granite_schema::{decode_type, encode_type};
use granite_types::expr::{LiteralError, decode_literal, encode_literal, opcode};
use granite_types::{Type, Value};
use std::ops::Bound;

/// Counts one more node at `depth`, failing past the limits.
fn enter(nodes: &mut u32, depth: u32) -> Result<(), Error> {
    if u64::from(depth) > FORMAT.program_depth_max {
        return Err(Error::TooDeep);
    }
    *nodes += 1;
    if u64::from(*nodes) > FORMAT.program_nodes_max {
        return Err(Error::TooManyNodes);
    }
    Ok(())
}

fn fits_message(len: usize) -> Result<(), Error> {
    match u64::try_from(len) {
        Ok(len) if len <= FORMAT.message_size_max => Ok(()),
        _ => Err(Error::TooLarge),
    }
}

impl Program {
    /// Encodes the program. The same tree always gives the same bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer {
            out: Vec::new(),
            nodes: 0,
        };
        w.out.extend(IR_VERSION.to_le_bytes());
        w.name(&self.name)?;
        w.out
            .push(u8::try_from(self.params.len()).map_err(|_| Error::Malformed)?);
        for p in &self.params {
            w.name(&p.name)?;
            w.ty(&p.ty)?;
        }
        w.block(&self.body, 1)?;
        fits_message(w.out.len())?;
        Ok(w.out)
    }

    /// Decodes a program; it is not validated.
    pub fn decode(bytes: &[u8]) -> Result<Program, Error> {
        fits_message(bytes.len())?;
        let mut r = Reader {
            bytes,
            pos: 0,
            nodes: 0,
        };
        if r.u16()? != IR_VERSION {
            return Err(Error::UnsupportedVersion);
        }
        let name = r.name()?;
        let params = (0..r.u8()?)
            .map(|_| {
                Ok(Param {
                    name: r.name()?,
                    ty: r.ty()?,
                })
            })
            .collect::<Result<_, _>>()?;
        let body = r.block(1)?;
        if r.pos != bytes.len() {
            return Err(Error::Malformed);
        }
        Ok(Program { name, params, body })
    }
}

struct Writer {
    out: Vec<u8>,
    nodes: u32,
}

impl Writer {
    fn name(&mut self, name: &str) -> Result<(), Error> {
        self.out
            .push(u8::try_from(name.len()).map_err(|_| Error::Malformed)?);
        self.out.extend(name.as_bytes());
        Ok(())
    }

    /// A list longer than u16 has more than `FORMAT.program_nodes_max` nodes.
    fn count(&mut self, n: usize) -> Result<(), Error> {
        let n = u16::try_from(n).map_err(|_| Error::TooManyNodes)?;
        self.out.extend(n.to_le_bytes());
        Ok(())
    }

    fn ty(&mut self, t: &Type) -> Result<(), Error> {
        t.validate().map_err(|_| Error::Malformed)?;
        encode_type(&mut self.out, t);
        Ok(())
    }

    fn block(&mut self, stmts: &[Stmt], depth: u32) -> Result<(), Error> {
        self.count(stmts.len())?;
        stmts.iter().try_for_each(|s| self.stmt(s, depth))
    }

    fn exprs(&mut self, exprs: &[Expr], depth: u32) -> Result<(), Error> {
        self.count(exprs.len())?;
        exprs.iter().try_for_each(|e| self.expr(e, depth))
    }

    fn stmt(&mut self, s: &Stmt, depth: u32) -> Result<(), Error> {
        enter(&mut self.nodes, depth)?;
        let d = depth + 1;
        match s {
            Stmt::Let(e) => {
                self.out.push(0);
                self.expr(e, d)
            }
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                self.out.push(1);
                self.expr(cond, d)?;
                self.block(then, d)?;
                self.block(otherwise, d)
            }
            Stmt::Require { cond, code } => {
                self.out.push(2);
                self.expr(cond, d)?;
                self.name(code)
            }
            Stmt::Insert { table, row } => {
                self.out.push(3);
                self.name(table)?;
                self.exprs(row, d)
            }
            Stmt::Upsert { table, row } => {
                self.out.push(4);
                self.name(table)?;
                self.exprs(row, d)
            }
            Stmt::Update { table, key, set } => {
                self.out.push(5);
                self.name(table)?;
                self.exprs(key, d)?;
                self.count(set.len())?;
                set.iter().try_for_each(|(column, e)| {
                    self.out.extend(column.to_le_bytes());
                    self.expr(e, d)
                })
            }
            Stmt::Delete { table, key } => {
                self.out.push(6);
                self.name(table)?;
                self.exprs(key, d)
            }
            Stmt::For { scan, body } => {
                self.out.push(7);
                self.name(&scan.table)?;
                self.exprs(&scan.prefix, d)?;
                self.bound(&scan.lower, d)?;
                self.bound(&scan.upper, d)?;
                self.out.extend(scan.limit.to_le_bytes());
                self.block(body, d)
            }
            Stmt::Return(e) => {
                self.out.push(8);
                self.expr(e, d)
            }
        }
    }

    fn bound(&mut self, b: &Bound<Expr>, depth: u32) -> Result<(), Error> {
        match b {
            Bound::Unbounded => {
                self.out.push(0);
                Ok(())
            }
            Bound::Included(e) => {
                self.out.push(1);
                self.expr(e, depth)
            }
            Bound::Excluded(e) => {
                self.out.push(2);
                self.expr(e, depth)
            }
        }
    }

    fn expr(&mut self, e: &Expr, depth: u32) -> Result<(), Error> {
        enter(&mut self.nodes, depth)?;
        let d = depth + 1;
        match e {
            Expr::Literal(literal) => {
                let (ty, value) = &**literal;
                self.out.push(opcode::LITERAL);
                self.ty(ty)?;
                self.value(ty, value)
            }
            Expr::Param(i) => {
                self.out.extend([1, *i]);
                Ok(())
            }
            Expr::Local(i) => {
                self.out.push(2);
                self.out.extend(i.to_le_bytes());
                Ok(())
            }
            Expr::Field(row, column) => {
                self.out.push(3);
                self.expr(row, d)?;
                self.out.extend(column.to_le_bytes());
                Ok(())
            }
            Expr::Get { table, key } => {
                self.out.push(4);
                self.name(table)?;
                self.exprs(key, d)
            }
            Expr::CommitTs => {
                self.out.push(5);
                Ok(())
            }
            Expr::Neg(a) | Expr::Not(a) | Expr::IsNull(a) => {
                self.out.push(match e {
                    Expr::Neg(_) => opcode::NEG,
                    Expr::Not(_) => opcode::NOT,
                    _ => opcode::IS_NULL,
                });
                self.expr(a, d)
            }
            Expr::UnwrapOr(a, b) => {
                self.out.push(opcode::UNWRAP_OR);
                self.expr(a, d)?;
                self.expr(b, d)
            }
            Expr::Match {
                value,
                if_null,
                present,
            } => {
                self.out.push(10);
                self.expr(value, d)?;
                self.expr(if_null, d)?;
                self.expr(present, d)
            }
            Expr::Cast(a, ty) => {
                self.out.push(opcode::CAST);
                self.expr(a, d)?;
                self.ty(ty)
            }
            Expr::Binary(op, a, b) => {
                self.out.push(op.opcode());
                self.expr(a, d)?;
                self.expr(b, d)
            }
        }
    }

    fn value(&mut self, ty: &Type, value: &Value) -> Result<(), Error> {
        encode_literal(ty, value, &mut self.out).map_err(|_| Error::InvalidLiteral)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: u32,
}

impl<'a> Reader<'a> {
    fn slice(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Malformed)?;
        let s = self.bytes.get(self.pos..end).ok_or(Error::Malformed)?;
        self.pos = end;
        Ok(s)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        Ok(self.slice(N)?.try_into().expect("slice of N bytes"))
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take()?))
    }

    fn name(&mut self) -> Result<String, Error> {
        let len = usize::from(self.u8()?);
        String::from_utf8(self.slice(len)?.to_vec()).map_err(|_| Error::Malformed)
    }

    fn ty(&mut self) -> Result<Type, Error> {
        let rest = self.bytes.get(self.pos..).unwrap_or_default();
        let (t, len) = decode_type(rest).map_err(|_| Error::Malformed)?;
        self.slice(len)?;
        t.validate().map_err(|_| Error::Malformed)?;
        Ok(t)
    }

    /// A u16 count, then that many items. Each item takes at least one byte
    /// and is a node, so the list grows only as far as the input and the
    /// node limit allow.
    fn list<T>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let n = self.u16()?;
        let mut items = Vec::new();
        for _ in 0..n {
            items.push(item(self)?);
        }
        Ok(items)
    }

    fn block(&mut self, depth: u32) -> Result<Vec<Stmt>, Error> {
        self.list(|r| r.stmt(depth))
    }

    fn exprs(&mut self, depth: u32) -> Result<Vec<Expr>, Error> {
        self.list(|r| r.expr(depth))
    }

    fn boxed(&mut self, depth: u32) -> Result<Box<Expr>, Error> {
        Ok(Box::new(self.expr(depth)?))
    }

    fn stmt(&mut self, depth: u32) -> Result<Stmt, Error> {
        enter(&mut self.nodes, depth)?;
        let d = depth + 1;
        Ok(match self.u8()? {
            0 => Stmt::Let(self.expr(d)?),
            1 => Stmt::If {
                cond: self.expr(d)?,
                then: self.block(d)?,
                otherwise: self.block(d)?,
            },
            2 => Stmt::Require {
                cond: self.expr(d)?,
                code: self.name()?,
            },
            3 => Stmt::Insert {
                table: self.name()?,
                row: self.exprs(d)?,
            },
            4 => Stmt::Upsert {
                table: self.name()?,
                row: self.exprs(d)?,
            },
            5 => Stmt::Update {
                table: self.name()?,
                key: self.exprs(d)?,
                set: self.list(|r| Ok((r.u16()?, r.expr(d)?)))?,
            },
            6 => Stmt::Delete {
                table: self.name()?,
                key: self.exprs(d)?,
            },
            7 => Stmt::For {
                scan: Box::new(Scan {
                    table: self.name()?,
                    prefix: self.exprs(d)?,
                    lower: self.bound(d)?,
                    upper: self.bound(d)?,
                    limit: self.u16()?,
                }),
                body: self.block(d)?,
            },
            8 => Stmt::Return(self.expr(d)?),
            _ => return Err(Error::UnknownOpcode),
        })
    }

    fn bound(&mut self, depth: u32) -> Result<Bound<Expr>, Error> {
        Ok(match self.u8()? {
            0 => Bound::Unbounded,
            1 => Bound::Included(self.expr(depth)?),
            2 => Bound::Excluded(self.expr(depth)?),
            _ => return Err(Error::Malformed),
        })
    }

    fn expr(&mut self, depth: u32) -> Result<Expr, Error> {
        enter(&mut self.nodes, depth)?;
        let d = depth + 1;
        Ok(match self.u8()? {
            opcode::LITERAL => {
                let ty = self.ty()?;
                let value = self.value(&ty)?;
                Expr::Literal(Box::new((ty, value)))
            }
            1 => Expr::Param(self.u8()?),
            2 => Expr::Local(self.u16()?),
            3 => Expr::Field(self.boxed(d)?, self.u16()?),
            4 => Expr::Get {
                table: self.name()?,
                key: self.exprs(d)?,
            },
            5 => Expr::CommitTs,
            opcode::NEG => Expr::Neg(self.boxed(d)?),
            opcode::NOT => Expr::Not(self.boxed(d)?),
            opcode::IS_NULL => Expr::IsNull(self.boxed(d)?),
            opcode::UNWRAP_OR => Expr::UnwrapOr(self.boxed(d)?, self.boxed(d)?),
            10 => Expr::Match {
                value: self.boxed(d)?,
                if_null: self.boxed(d)?,
                present: self.boxed(d)?,
            },
            opcode::CAST => Expr::Cast(self.boxed(d)?, self.ty()?),
            op => match BinOp::from_opcode(op) {
                Some(op) => Expr::Binary(op, self.boxed(d)?, self.boxed(d)?),
                None => return Err(Error::UnknownOpcode),
            },
        })
    }

    /// A literal's value (document 03 section 5.1).
    fn value(&mut self, ty: &Type) -> Result<Value, Error> {
        let rest = self.bytes.get(self.pos..).unwrap_or_default();
        let (value, len) = decode_literal(ty, rest).map_err(|e| match e {
            LiteralError::Truncated => Error::Malformed,
            LiteralError::Invalid => Error::InvalidLiteral,
        })?;
        self.slice(len)?;
        Ok(value)
    }
}
