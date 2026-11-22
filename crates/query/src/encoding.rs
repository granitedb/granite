//! The binary form of a fragment, document 05 section 4. Integers are
//! little-endian. The encoding is canonical: a tree has exactly one
//! encoding, and bytes the decoder accepts are the encoding of the tree it
//! returns.

use crate::{
    Aggregate, BinOp, Error, Expr, FRAGMENT_VERSION, Fragment, KeyRange, Literal, Operator, Scan,
};
use granite_base::constants::FORMAT;
use granite_schema::{decode_type, encode_type};
use granite_types::Type;
use granite_types::expr::{LiteralError, decode_literal, encode_literal, opcode};
use granite_types::key::Order;
use std::ops::Bound;

/// The fragment-only expression: a column of the input row, u16 index.
pub(crate) const COLUMN: u8 = 25;

const PROJECT: u8 = 0;
const PARTIAL_AGGREGATE: u8 = 1;
const TOP_N: u8 = 2;
const LIMIT: u8 = 3;

const COUNT: u8 = 0;
const SUM: u8 = 1;
const MIN: u8 = 2;
const MAX: u8 = 3;

/// Counts one more element at `depth`, failing past the limits.
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

fn order_tag(o: Order) -> u8 {
    match o {
        Order::Asc => 0,
        Order::Desc => 1,
    }
}

impl Fragment {
    /// Encodes the fragment. The same tree always gives the same bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer {
            out: Vec::new(),
            nodes: 0,
        };
        w.out.extend(FRAGMENT_VERSION.to_le_bytes());
        let s = &self.scan;
        w.name(&s.table)?;
        w.count(s.columns.len())?;
        s.columns.iter().for_each(|c| w.out.extend(c.to_le_bytes()));
        w.count(s.ranges.len())?;
        for r in &s.ranges {
            enter(&mut w.nodes, 1)?;
            w.bound(&r.lower)?;
            w.bound(&r.upper)?;
        }
        w.out.extend(s.time.0.to_le_bytes());
        w.out.extend(s.time.1.to_le_bytes());
        match &s.predicate {
            None => w.out.push(0),
            Some(e) => {
                w.out.push(1);
                w.expr(e, 1)?;
            }
        }
        w.out
            .push(u8::try_from(self.operators.len()).map_err(|_| Error::Malformed)?);
        self.operators.iter().try_for_each(|o| w.operator(o))?;
        fits_message(w.out.len())?;
        Ok(w.out)
    }

    /// Decodes a fragment; it is not validated.
    pub fn decode(bytes: &[u8]) -> Result<Fragment, Error> {
        fits_message(bytes.len())?;
        let mut r = Reader {
            bytes,
            pos: 0,
            nodes: 0,
        };
        if r.u16()? != FRAGMENT_VERSION {
            return Err(Error::UnsupportedVersion);
        }
        let scan = Scan {
            table: r.name()?,
            columns: r.list(|r| r.u16())?,
            ranges: r.list(|r| {
                enter(&mut r.nodes, 1)?;
                Ok(KeyRange {
                    lower: r.bound()?,
                    upper: r.bound()?,
                })
            })?,
            time: (r.u64()?, r.u64()?),
            predicate: match r.u8()? {
                0 => None,
                1 => Some(r.expr(1)?),
                _ => return Err(Error::Malformed),
            },
        };
        let operators = (0..r.u8()?)
            .map(|_| r.operator())
            .collect::<Result<_, _>>()?;
        if r.pos != bytes.len() {
            return Err(Error::Malformed);
        }
        Ok(Fragment { scan, operators })
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

    /// A list longer than u16 has more than `FORMAT.program_nodes_max`
    /// elements, or is longer than a message.
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

    fn literal(&mut self, (ty, value): &Literal) -> Result<(), Error> {
        self.ty(ty)?;
        encode_literal(ty, value, &mut self.out).map_err(|_| Error::InvalidLiteral)
    }

    fn bound(&mut self, b: &Bound<Vec<Literal>>) -> Result<(), Error> {
        let values = match b {
            Bound::Unbounded => {
                self.out.push(0);
                return Ok(());
            }
            Bound::Included(v) => {
                self.out.push(1);
                v
            }
            Bound::Excluded(v) => {
                self.out.push(2);
                v
            }
        };
        self.count(values.len())?;
        values.iter().try_for_each(|l| {
            enter(&mut self.nodes, 1)?;
            self.literal(l)
        })
    }

    fn exprs(&mut self, exprs: &[Expr]) -> Result<(), Error> {
        self.count(exprs.len())?;
        exprs.iter().try_for_each(|e| self.expr(e, 1))
    }

    fn operator(&mut self, o: &Operator) -> Result<(), Error> {
        enter(&mut self.nodes, 1)?;
        match o {
            Operator::Project(exprs) => {
                self.out.push(PROJECT);
                self.exprs(exprs)
            }
            Operator::PartialAggregate {
                keys,
                aggregates,
                groups,
            } => {
                self.out.push(PARTIAL_AGGREGATE);
                self.exprs(keys)?;
                self.count(aggregates.len())?;
                for a in aggregates {
                    enter(&mut self.nodes, 1)?;
                    match a {
                        Aggregate::Count => self.out.push(COUNT),
                        Aggregate::Sum(e) | Aggregate::Min(e) | Aggregate::Max(e) => {
                            self.out.push(match a {
                                Aggregate::Sum(_) => SUM,
                                Aggregate::Min(_) => MIN,
                                _ => MAX,
                            });
                            self.expr(e, 1)?;
                        }
                    }
                }
                self.out.extend(groups.to_le_bytes());
                Ok(())
            }
            Operator::TopN { order, n } => {
                self.out.push(TOP_N);
                self.count(order.len())?;
                order.iter().try_for_each(|(e, o)| {
                    self.out.push(order_tag(*o));
                    self.expr(e, 1)
                })?;
                self.out.extend(n.to_le_bytes());
                Ok(())
            }
            Operator::Limit(n) => {
                self.out.push(LIMIT);
                self.out.extend(n.to_le_bytes());
                Ok(())
            }
        }
    }

    fn expr(&mut self, e: &Expr, depth: u32) -> Result<(), Error> {
        enter(&mut self.nodes, depth)?;
        let d = depth + 1;
        match e {
            Expr::Literal(literal) => {
                self.out.push(opcode::LITERAL);
                self.literal(literal)
            }
            Expr::Column(i) => {
                self.out.push(COLUMN);
                self.out.extend(i.to_le_bytes());
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

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
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

    fn literal(&mut self) -> Result<Literal, Error> {
        let ty = self.ty()?;
        let rest = self.bytes.get(self.pos..).unwrap_or_default();
        let (value, len) = decode_literal(&ty, rest).map_err(|e| match e {
            LiteralError::Truncated => Error::Malformed,
            LiteralError::Invalid => Error::InvalidLiteral,
        })?;
        self.slice(len)?;
        Ok((ty, value))
    }

    /// A u16 count, then that many items. Each item takes at least one byte,
    /// so the list grows only as far as the input allows.
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

    fn bound(&mut self) -> Result<Bound<Vec<Literal>>, Error> {
        let tag = self.u8()?;
        if tag == 0 {
            return Ok(Bound::Unbounded);
        }
        if tag > 2 {
            return Err(Error::Malformed);
        }
        let values = self.list(|r| {
            enter(&mut r.nodes, 1)?;
            r.literal()
        })?;
        Ok(if tag == 1 {
            Bound::Included(values)
        } else {
            Bound::Excluded(values)
        })
    }

    fn exprs(&mut self) -> Result<Vec<Expr>, Error> {
        self.list(|r| r.expr(1))
    }

    fn operator(&mut self) -> Result<Operator, Error> {
        enter(&mut self.nodes, 1)?;
        Ok(match self.u8()? {
            PROJECT => Operator::Project(self.exprs()?),
            PARTIAL_AGGREGATE => Operator::PartialAggregate {
                keys: self.exprs()?,
                aggregates: self.list(|r| {
                    enter(&mut r.nodes, 1)?;
                    Ok(match r.u8()? {
                        COUNT => Aggregate::Count,
                        SUM => Aggregate::Sum(r.expr(1)?),
                        MIN => Aggregate::Min(r.expr(1)?),
                        MAX => Aggregate::Max(r.expr(1)?),
                        _ => return Err(Error::UnknownOpcode),
                    })
                })?,
                groups: self.u32()?,
            },
            TOP_N => Operator::TopN {
                order: self.list(|r| {
                    let o = match r.u8()? {
                        0 => Order::Asc,
                        1 => Order::Desc,
                        _ => return Err(Error::Malformed),
                    };
                    Ok((r.expr(1)?, o))
                })?,
                n: self.u32()?,
            },
            LIMIT => Operator::Limit(self.u64()?),
            _ => return Err(Error::UnknownOpcode),
        })
    }

    fn boxed(&mut self, depth: u32) -> Result<Box<Expr>, Error> {
        Ok(Box::new(self.expr(depth)?))
    }

    fn expr(&mut self, depth: u32) -> Result<Expr, Error> {
        enter(&mut self.nodes, depth)?;
        let d = depth + 1;
        Ok(match self.u8()? {
            opcode::LITERAL => Expr::Literal(Box::new(self.literal()?)),
            COLUMN => Expr::Column(self.u16()?),
            opcode::NEG => Expr::Neg(self.boxed(d)?),
            opcode::NOT => Expr::Not(self.boxed(d)?),
            opcode::IS_NULL => Expr::IsNull(self.boxed(d)?),
            opcode::UNWRAP_OR => Expr::UnwrapOr(self.boxed(d)?, self.boxed(d)?),
            opcode::CAST => Expr::Cast(self.boxed(d)?, self.ty()?),
            op => match BinOp::from_opcode(op) {
                Some(op) => Expr::Binary(op, self.boxed(d)?, self.boxed(d)?),
                None => return Err(Error::UnknownOpcode),
            },
        })
    }
}
