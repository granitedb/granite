//! The program encoding of document 03 section 5.1: a golden program, every
//! opcode, the limits at and one past their value, the version, and the
//! rejection of non-canonical literals.

use granite_base::constants::FORMAT;
use granite_txn::{Error, Expr, IR_VERSION, Param, Program, Stmt};
use granite_types::{Type, Value};

/// IR version 1, an empty name, no parameters, and a body of `n` statements.
fn header(n: u16) -> Vec<u8> {
    let mut b = IR_VERSION.to_le_bytes().to_vec();
    b.extend([0, 0]);
    b.extend(n.to_le_bytes());
    b
}

/// A program whose body is `bytes`, one statement.
fn one(bytes: &[u8]) -> Result<Program, Error> {
    let mut b = header(1);
    b.extend(bytes);
    Program::decode(&b)
}

fn returning(e: Expr) -> Program {
    Program {
        name: String::new(),
        params: vec![],
        body: vec![Stmt::Return(e)],
    }
}

fn literal(ty: Type, value: Value) -> Expr {
    Expr::Literal(Box::new((ty, value)))
}

/// The example of document 03 section 5.1, byte for byte.
#[test]
fn golden_program() {
    let p = Program {
        name: "t".into(),
        params: vec![Param {
            name: "a".into(),
            ty: Type::U64,
        }],
        body: vec![
            Stmt::Require {
                cond: Expr::Not(Box::new(Expr::IsNull(Box::new(Expr::Get {
                    table: "acc".into(),
                    key: vec![Expr::Param(0)],
                })))),
                code: "E".into(),
            },
            Stmt::Return(Expr::Binary(
                granite_txn::BinOp::Add,
                Box::new(Expr::Param(0)),
                Box::new(literal(Type::U64, Value::U64(1))),
            )),
        ],
    };
    let bytes = [
        0x01, 0x00, 0x01, 0x74, 0x01, 0x01, 0x61, 0x03, 0x02, 0x00, // header
        0x02, 0x07, 0x08, 0x04, 0x03, 0x61, 0x63, 0x63, 0x01, 0x00, 0x01, 0x00, 0x01,
        0x45, // require
        0x08, 0x0c, 0x01, 0x00, 0x00, 0x03, 0x01, 0, 0, 0, 0, 0, 0, 0, // return
    ];
    assert_eq!(p.encode().as_deref(), Ok(&bytes[..]));
    assert_eq!(Program::decode(&bytes), Ok(p));
}

/// Statement opcodes with the smallest operands, and the element each decodes to.
const STATEMENTS: [(&[u8], &str); 9] = [
    (&[0, 5], "Let("),
    (&[1, 5, 0, 0, 0, 0], "If {"),
    (&[2, 5, 0], "Require {"),
    (&[3, 0, 0, 0], "Insert {"),
    (&[4, 0, 0, 0], "Upsert {"),
    (&[5, 0, 0, 0, 0, 0], "Update {"),
    (&[6, 0, 0, 0], "Delete {"),
    (&[7, 0, 0, 0, 0, 0, 0, 0, 0, 0], "For {"),
    (&[8, 5], "Return("),
];

/// Expression opcodes with the smallest operands, and the element each decodes to.
const EXPRESSIONS: [(&[u8], &str); 25] = [
    (&[0, 0, 7], "Literal("),
    (&[1, 0], "Param("),
    (&[2, 0, 0], "Local("),
    (&[3, 5, 0, 0], "Field("),
    (&[4, 0, 0, 0], "Get {"),
    (&[5], "CommitTs"),
    (&[6, 5], "Neg("),
    (&[7, 5], "Not("),
    (&[8, 5], "IsNull("),
    (&[9, 5, 5], "UnwrapOr("),
    (&[10, 5, 5, 5], "Match {"),
    (&[11, 5, 3], "Cast("),
    (&[12, 5, 5], "Binary(Add,"),
    (&[13, 5, 5], "Binary(Sub,"),
    (&[14, 5, 5], "Binary(Mul,"),
    (&[15, 5, 5], "Binary(Div,"),
    (&[16, 5, 5], "Binary(Rem,"),
    (&[17, 5, 5], "Binary(Eq,"),
    (&[18, 5, 5], "Binary(Ne,"),
    (&[19, 5, 5], "Binary(Lt,"),
    (&[20, 5, 5], "Binary(Le,"),
    (&[21, 5, 5], "Binary(Gt,"),
    (&[22, 5, 5], "Binary(Ge,"),
    (&[23, 5, 5], "Binary(And,"),
    (&[24, 5, 5], "Binary(Or,"),
];

/// Every opcode value either is one of the elements of document 03 section
/// 5, which include nothing for time other than `commit_ts`, randomness,
/// external calls, dynamic table names, unbounded loops or recursion, or is
/// rejected as unknown.
#[test]
fn every_opcode_is_a_listed_element_or_unknown() {
    let forbidden = [
        "now", "time", "clock", "rand", "call", "extern", "dynamic", "while", "loop",
    ];
    for (_, element) in STATEMENTS.iter().chain(&EXPRESSIONS) {
        let lower = element.to_lowercase();
        assert!(!forbidden.iter().any(|f| lower.contains(f)), "{element}");
    }
    for op in 0..=u8::MAX {
        match STATEMENTS.iter().find(|(b, _)| b[0] == op) {
            Some((bytes, element)) => {
                let p = one(bytes).unwrap_or_else(|e| panic!("statement {op}: {e:?}"));
                assert!(format!("{:?}", p.body[0]).starts_with(element), "{op}");
                p.encode().unwrap();
            }
            None => assert_eq!(one(&[op]), Err(Error::UnknownOpcode), "statement {op}"),
        }
        match EXPRESSIONS.iter().find(|(b, _)| b[0] == op) {
            Some((bytes, element)) => {
                let p = one(&[&[8], *bytes].concat())
                    .unwrap_or_else(|e| panic!("expression {op}: {e:?}"));
                let Stmt::Return(e) = &p.body[0] else {
                    unreachable!()
                };
                assert!(format!("{e:?}").starts_with(element), "{op}");
            }
            None => assert_eq!(one(&[8, op]), Err(Error::UnknownOpcode), "expression {op}"),
        }
    }
}

/// `return -(-(...(commit_ts)))`: the return is at depth 1, the commit_ts at `depth`.
fn nested(depth: u32) -> (Program, Vec<u8>) {
    let negs = depth - 2;
    let mut e = Expr::CommitTs;
    for _ in 0..negs {
        e = Expr::Neg(Box::new(e));
    }
    let mut bytes = header(1);
    bytes.push(8);
    bytes.extend(std::iter::repeat_n(6, negs as usize));
    bytes.push(5);
    (returning(e), bytes)
}

#[test]
fn depth_is_limited() {
    let depth = u32::try_from(FORMAT.program_depth_max).unwrap();
    let (p, bytes) = nested(depth);
    assert_eq!(p.encode().as_deref(), Ok(&bytes[..]));
    assert_eq!(Program::decode(&bytes), Ok(p));
    let (p, bytes) = nested(depth + 1);
    assert_eq!(p.encode(), Err(Error::TooDeep));
    assert_eq!(Program::decode(&bytes), Err(Error::TooDeep));
}

#[test]
fn node_count_is_limited() {
    // Each `return commit_ts` is two nodes.
    assert!(
        FORMAT.program_nodes_max.is_multiple_of(2),
        "the body reaches the limit exactly"
    );
    let n = usize::try_from(FORMAT.program_nodes_max / 2).unwrap();
    let mut p = Program {
        name: String::new(),
        params: vec![],
        body: vec![Stmt::Return(Expr::CommitTs); n],
    };
    let mut bytes = p.encode().unwrap();
    assert_eq!(Program::decode(&bytes), Ok(p.clone()));
    // One node more: the last becomes `return !commit_ts`.
    p.body[n - 1] = Stmt::Return(Expr::Not(Box::new(Expr::CommitTs)));
    assert_eq!(p.encode(), Err(Error::TooManyNodes));
    bytes.insert(bytes.len() - 1, 7);
    assert_eq!(Program::decode(&bytes), Err(Error::TooManyNodes));
}

#[test]
fn names_and_parameters_are_limited() {
    let param = |name: &str| Param {
        name: name.into(),
        ty: Type::U8,
    };
    let at = |name: &str, params: usize| Program {
        name: name.into(),
        params: vec![param(name); params],
        body: vec![],
    };
    // Names and the parameter count have a u8 length.
    let max = usize::from(u8::MAX);
    let p = at(&"x".repeat(max), max);
    let bytes = p.encode().unwrap();
    assert_eq!(Program::decode(&bytes), Ok(p));
    assert_eq!(at(&"x".repeat(max + 1), 0).encode(), Err(Error::Malformed));
    assert_eq!(at("", max + 1).encode(), Err(Error::Malformed));
    // A u8 length cannot say one more, so the decoder cannot be given one more.
}

#[test]
fn other_versions_are_rejected() {
    let mut bytes = returning(Expr::CommitTs).encode().unwrap();
    assert!(Program::decode(&bytes).is_ok());
    for version in [IR_VERSION + 1, u16::MAX, 0] {
        bytes[..2].copy_from_slice(&version.to_le_bytes());
        assert_eq!(Program::decode(&bytes), Err(Error::UnsupportedVersion));
    }
}

#[test]
fn input_is_at_most_one_message() {
    let max = usize::try_from(FORMAT.message_size_max).unwrap();
    assert_eq!(Program::decode(&vec![0; max + 1]), Err(Error::TooLarge));
    assert_eq!(
        Program::decode(&vec![0; max]),
        Err(Error::UnsupportedVersion)
    );
    let big = literal(Type::Bytes { max: u32::MAX }, Value::Bytes(vec![0; max]));
    assert_eq!(returning(big).encode(), Err(Error::TooLarge));
}

#[test]
fn non_canonical_and_invalid_literals_are_rejected() {
    let cases: [(&[u8], Option<Error>); 10] = [
        // f32 NaN with a payload; the canonical NaN is 0x7fc00000.
        (&[8, 0, 13, 1, 0, 0xc0, 0x7f], Some(Error::InvalidLiteral)),
        (&[8, 0, 13, 0, 0, 0xc0, 0x7f], None),
        // Nullable(u8) null with a nonzero value byte.
        (&[8, 0, 26, 0, 0, 1], Some(Error::InvalidLiteral)),
        // bool 2.
        (&[8, 0, 15, 2], Some(Error::InvalidLiteral)),
        // Nullable(String(9)) with flag 2.
        (&[8, 0, 26, 21, 9, 0, 0, 0, 2], Some(Error::InvalidLiteral)),
        // String(9) that is not UTF-8, and one over its max.
        (
            &[8, 0, 21, 9, 0, 0, 0, 1, 0, 0, 0, 0xff],
            Some(Error::InvalidLiteral),
        ),
        (
            &[8, 0, 21, 0, 0, 0, 0, 1, 0, 0, 0, b'a'],
            Some(Error::InvalidLiteral),
        ),
        // Enum8 with max 1, value 2.
        (&[8, 0, 24, 1, 2], Some(Error::InvalidLiteral)),
        // Decimal(0, 0) is not a type.
        (&[8, 0, 12, 0, 0, 0, 0, 0, 0], Some(Error::Malformed)),
        // A trailing byte.
        (&[8, 5, 0], Some(Error::Malformed)),
    ];
    for (bytes, error) in cases {
        let got = one(bytes);
        match error {
            None => assert_eq!(got.unwrap().encode().unwrap()[6..], *bytes),
            Some(error) => assert_eq!(got, Err(error), "{bytes:?}"),
        }
    }
}

/// The decoder's largest allocation is below 2 MiB, the bound of its fuzz
/// gate: a list holds at most `program_nodes_max` elements and grows to less than
/// twice that, and names and literals are copies of parts of an input of
/// at most one message.
#[test]
fn decoder_allocations_are_bounded() {
    let element = [
        size_of::<Stmt>(),
        size_of::<Expr>(),
        size_of::<(u16, Expr)>(),
        size_of::<Param>(),
    ];
    let nodes = usize::try_from(FORMAT.program_nodes_max).unwrap();
    let list = 2 * nodes * element.iter().max().unwrap();
    let bound = 2 << 20;
    assert!(list < bound, "{list}");
    assert!(FORMAT.message_size_max < bound as u64);
}
