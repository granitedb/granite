//! The program compiler against the validator's rules, the reference
//! programs, the examples of document 03 and random programs. Rerun a
//! failure of a property test with
//! `GRANITE_SEED=<seed> cargo test -p granite-schemac --test program`.

use granite_base::constants::FORMAT;
use granite_schema::Catalog;
use granite_schemac::{
    CompileError, Pos, Problem, compile, compile_program, print_program, report,
};
use granite_txn::{Error, Program};
use granite_types::{I256, Type, U256, Value};
use std::path::{Path, PathBuf};

#[path = "../../txn/tests/support/mod.rs"]
mod support;

#[allow(
    dead_code,
    reason = "shared with the schema crate's tests, which use all of it"
)]
#[path = "../../schema/tests/common/mod.rs"]
mod common;

/// The tables of the validator tests (`crates/txn/tests/support`) as schema text.
const SCHEMA: &str = "
schema version 1
table group users partition by (user u64) hash
table account versioned layout row in users
  key (user)
  user u64, balance Decimal(18, 2), nick Nullable(String(8)), active bool
table entry immutable layout row in users
  key (user, id)
  user u64, id u32, amount Decimal(18, 2), kind Enum8(4)
table holding versioned layout row in users
  key (user, asset desc)
  user u64, asset u32, qty i64
table group assets partition by (asset u32) range
table asset versioned layout row in assets
  key (asset)
  asset u32, price i64, name LowCardinality(String(8)), ticker Nullable(i64)
table quote immutable layout row in assets
  key (asset, at)
  asset u32, at u16, px i64
";

fn catalog_of(schema: &[u8]) -> Catalog {
    let bytes = compile("schema", schema).unwrap_or_else(|e| panic!("{e}"));
    Catalog::decode(&bytes).unwrap()
}

fn catalog() -> Catalog {
    let c = catalog_of(SCHEMA.as_bytes());
    assert_eq!(
        c,
        support::catalog(),
        "SCHEMA is the validator tests' catalog"
    );
    c
}

/// Splits `text` at its `@` marker: the text without it, and where it was.
fn marked(text: &str) -> (String, Pos) {
    let i = text
        .find('@')
        .expect("a case marks the expected place with @");
    let before = &text[..i];
    let line_start = before.rfind('\n').map_or(0, |n| n + 1);
    let pos = Pos {
        line: before.matches('\n').count() as u32 + 1,
        column: before[line_start..].chars().count() as u32 + 1,
    };
    (text.replacen('@', "", 1), pos)
}

/// Compiles `text`, which must fail with `problem` at its `@` marker.
fn fails(catalog: &Catalog, problem: Problem, text: &str) {
    let (text, pos) = marked(text);
    let expected = CompileError {
        file: "case.program".into(),
        pos,
        problem,
    };
    assert_eq!(
        compile_program("case.program", text.as_bytes(), catalog).map(|_| ()),
        Err(expected),
        "{text}"
    );
}

/// `program p(u: u64)` with `body` after it.
fn with_body(body: &str) -> String {
    format!("program p(u: u64)\n  {body}\n")
}

#[test]
fn every_validator_code_is_reported_at_its_place() {
    use Error as E;
    let c = catalog();
    let code = |e| Problem::Program(e);
    let ok = with_body("return exists account[u]");
    compile_program("ok", ok.as_bytes(), &c).unwrap();
    let cases = [
        (E::UnknownTable, "return exists @nope[u]"),
        (E::UnknownColumn, "update account[u] { @nope: u64 1 }"),
        (
            E::UnknownColumn,
            "return match get account[u] { null => false, r => r.@nope }",
        ),
        (E::UnknownParam, "return @x"),
        (E::UnknownLocal, "if true { let x = u64 1 }\n  return @x"),
        (E::TypeMismatch, "return u + @true"),
        (E::TypeMismatch, "return @true + u"),
        (E::TypeMismatch, "return u.@user"),
        (E::ImplicitCast, "return u + @u32 1"),
        (E::NullableOperand, "return (@get account[u]).active"),
        (E::WrongArity, "return @exists account[u, u]"),
        (
            E::WrongArity,
            "@insert account { user: u, balance: Decimal(18, 2) 1 }",
        ),
        (E::ImmutableTable, "@delete entry[u, u32 1]"),
        (E::KeyColumnUpdated, "@update account[u] { user: u64 1 }"),
        (
            E::ColumnSetTwice,
            "@update account[u] { active: true, active: false }",
        ),
        (
            E::ScanAcrossPartitions,
            "@for r in scan quote[] limit 1 { }",
        ),
        (E::TooManyAccesses, "@for r in scan entry[u] limit 300 { }"),
        (
            E::CodeAfterReturn,
            "if true { return u } else { return u }\n  @return u",
        ),
    ];
    for (e, body) in cases {
        fails(&c, code(e), &with_body(body));
    }
    // The program as a whole: node 0 is at `program`.
    fails(
        &c,
        code(E::MissingReturn),
        "@program p(u: u64)\n  if true { return u }\n",
    );
    // Codes of the encoding that text can reach, each at its place.
    fails(&c, code(E::InvalidLiteral), &with_body("return u8 @256"));
    fails(&c, code(E::InvalidLiteral), &with_body("return u64 @1.5"));
    fails(&c, code(E::Malformed), "program p(u: @Decimal(77, 2))\n");
    let mut bytes = with_body("return u").into_bytes();
    bytes.extend(b" + \xff");
    let invalid = compile_program("case.program", &bytes, &c).unwrap_err();
    assert_eq!(
        (invalid.pos, invalid.problem),
        (Pos { line: 3, column: 4 }, code(E::Malformed))
    );
}

#[test]
fn errors_name_file_line_column_and_code() {
    let e = compile_program(
        "x.program",
        with_body("return u + true").as_bytes(),
        &catalog(),
    );
    assert_eq!(
        e.unwrap_err().to_string(),
        "x.program:2:14: program rule 12 TypeMismatch (document 03 section 5.2)"
    );
    let e = compile_program(
        "x.program",
        with_body("return u8 256").as_bytes(),
        &catalog(),
    );
    assert_eq!(
        e.unwrap_err().to_string(),
        "x.program:2:13: program rule 7 InvalidLiteral (document 03 section 5.1)"
    );
    let e = compile_program("x.program", with_body("return u +").as_bytes(), &catalog());
    assert_eq!(
        e.unwrap_err().to_string(),
        "x.program:3:1: expected an expression"
    );
}

/// Code 23 is a rule of an invocation, not of a program: the parameter
/// values of each invocation are checked by `Program::check_params`, which
/// the executor runs, and no text the compiler reads carries them.
#[test]
fn parameter_types_are_checked_per_invocation() {
    let program = compile_program("p", with_body("return u").as_bytes(), &catalog()).unwrap();
    assert_eq!(program.check_params(&[Value::U64(1)]), Ok(()));
    assert_eq!(
        program.check_params(&[Value::U32(1)]),
        Err(Error::ParamType)
    );
    assert_eq!(program.check_params(&[]), Err(Error::ParamType));
}

#[test]
fn grammar_errors_name_what_was_expected() {
    let c = catalog();
    let text = |m| Problem::Text(m);
    fails(&c, text("expected `program` at the start"), "@schema");
    fails(&c, text("expected a statement"), &with_body("@u"));
    fails(
        &c,
        text("parameter declared twice"),
        "program p(u: u64, @u: u32)",
    );
    fails(
        &c,
        text("expected the columns of the row in declaration order"),
        &with_body("insert account { @balance: Decimal(18, 2) 1 }"),
    );
    fails(
        &c,
        text("expected `]`: the bounds come last, lower first"),
        &with_body("for r in scan entry[u, < u32 9, @> u32 1] limit 1 { }"),
    );
    fails(
        &c,
        text("expected an expression: a null literal has a type, as in `Nullable(u64) null`"),
        &with_body("return @null"),
    );
    fails(
        &c,
        text("a limit is at most 65535"),
        &with_body("for r in scan entry[u] limit @65536 { }"),
    );
    fails(
        &c,
        text("expected a statement or `}`"),
        "program p(u: u64)\n  if true { return u\n@",
    );
}

/// Every form of the grammar, in canonical text, compiles and prints back
/// to the same text.
#[test]
fn canonical_text_prints_back_unchanged() {
    let c = catalog();
    let text = "\
program \"every form\"(u: u64, a: u32, \"get\": Nullable(i64), l: bool)
  let l_0 = get account[u]
  let l_1 = match l_0 { null => Decimal(18, 2) -1.50, l_2 => l_2.balance }
  require not is_null(l_0) and (not l or exists holding[u, a]) else \"no account\"
  if -(l_1 * l_1) + l_1 % Decimal(18, 2) 3.00 >= l_1 / Decimal(18, 2) 0.01 - -l_1 {
    insert entry { user: u, id: a, amount: l_1, kind: Enum8(4) 3 }
  } else if unwrap_or(\"get\", i64 -9223372036854775808) != i64 0 {
    update account[u] { nick: Nullable(String(8)) \"a\\\"b\\\\\", active: false }
  } else {
    delete holding[u, cast(u64 7, u32)]
  }
  for l_3 in scan holding[u, >= u32 1, < a] limit 10 {
    upsert holding { user: l_3.user, asset: l_3.asset, qty: l_3.qty + i64 1 }
    if commit_ts() = u64 0 or u = u64 1 = l {
      delete account[u]
    }
  }
  for l_4 in scan entry[u, <= u32 4] limit 0 {
  }
  return (u - u64 1) * -(-(u + u64 2))
";
    let program = compile_program("forms", text.as_bytes(), &c).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(print_program(&c, &program), text);
}

fn round_trip(catalog: &Catalog, program: &Program) {
    let text = print_program(catalog, program);
    let compiled = compile_program("printed", text.as_bytes(), catalog)
        .unwrap_or_else(|e| panic!("{e}\n{text}\n{program:?}"));
    assert_eq!(
        compiled.encode(),
        program.encode(),
        "{text}\n{program:?}\n{compiled:?}"
    );
    assert_eq!(print_program(catalog, &compiled), text);
}

/// Random valid programs of the validator tests' generator: compile, print,
/// compile gives byte-identical IR.
#[test]
fn compile_print_compile_is_byte_identical() {
    let mut rng = support::seeded("compile_print_compile_is_byte_identical");
    let c = catalog();
    let target = if support::full() { 100_000 } else { 2_000 };
    let mut valid = 0;
    while valid < target {
        let program = support::Gen::new(&mut rng, &c, u64::MAX).program();
        if program.validate(&c).is_ok() {
            round_trip(&c, &program);
            valid += 1;
        }
    }
    println!("{valid} random valid programs round-trip");
}

/// A random valid value of `t`, often at the ends of its range.
fn random_value(rng: &mut common::Rng, t: &Type) -> Value {
    let wide = |rng: &mut common::Rng| match rng.below(4) {
        0 => u128::MAX,
        1 => 0,
        2 => u128::from(rng.below(100)),
        _ => u128::from(rng.next()) << 64 | u128::from(rng.next()),
    };
    let x = wide(rng);
    let bytes = |rng: &mut common::Rng, n: u64| (0..n).map(|_| rng.next() as u8).collect();
    match t {
        Type::Nullable(_) if rng.below(4) == 0 => Value::Null,
        Type::Nullable(inner) | Type::LowCardinality(inner) => random_value(rng, inner),
        Type::U8 => Value::U8(x as u8),
        Type::U16 => Value::U16(x as u16),
        Type::U32 => Value::U32(x as u32),
        Type::U64 => Value::U64(x as u64),
        Type::U128 => Value::U128(x),
        Type::U256 => Value::U256(U256 {
            hi: wide(rng),
            lo: x,
        }),
        Type::I8 => Value::I8(x as i8),
        Type::I16 => Value::I16(x as i16),
        Type::I32 => Value::I32(x as i32),
        Type::I64 => Value::I64(x as i64),
        Type::I128 => Value::I128(x as i128),
        Type::I256 => Value::I256(I256 {
            hi: wide(rng) as i128,
            lo: x,
        }),
        Type::Decimal { precision, .. } => {
            let digits = u32::from(*precision).min(38);
            let m = (x % 10u128.pow(digits)) as i128;
            let m = if rng.below(2) == 0 { -m } else { m };
            match granite_types::decimal_width(*precision) {
                4 => Value::Decimal32(m as i32),
                8 => Value::Decimal64(m as i64),
                16 => Value::Decimal128(m),
                _ => Value::Decimal256(I256 {
                    hi: if m < 0 { -1 } else { 0 },
                    lo: m as u128,
                }),
            }
        }
        Type::F32 => Value::F32(granite_types::canonical_f32(f32::from_bits(x as u32))),
        Type::F64 => Value::F64(granite_types::canonical_f64(f64::from_bits(x as u64))),
        Type::Bool => Value::Bool(x % 2 == 0),
        Type::Date => Value::Date(x as i32),
        Type::Timestamp(_) => Value::Timestamp(x as i64),
        Type::Duration(_) => Value::Duration(x as i64),
        Type::Uuid => Value::Uuid(x.to_le_bytes()),
        Type::Id128 => Value::Id128(x),
        Type::String { max } => {
            let chars = ['a', '"', '\\', '\n', 'é', '\0', ' ', '-'];
            let n = rng.below(u64::from(*max).min(12) + 1);
            let s: String = (0..n).map(|_| chars[rng.below(8) as usize]).collect();
            let mut s = s;
            while s.len() > *max as usize {
                s.pop();
            }
            Value::String(s)
        }
        Type::Bytes { max } => {
            let n = rng.below(u64::from(*max).min(12) + 1);
            Value::Bytes(bytes(rng, n))
        }
        Type::FixedBytes(n) => Value::FixedBytes(bytes(rng, u64::from(*n))),
        Type::Enum8 { max } => Value::Enum8((x % (u128::from(*max) + 1)) as u8),
        Type::Enum16 { max } => Value::Enum16((x % (u128::from(*max) + 1)) as u16),
    }
}

/// Literals of every type, with values at the ends of their ranges,
/// compile, print and compile to the same bytes.
#[test]
fn literals_of_every_type_round_trip() {
    let seed = std::env::var("GRANITE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED);
    println!("literals_of_every_type_round_trip: GRANITE_SEED={seed}");
    let mut rng = common::Rng(seed);
    let c = catalog();
    let (mut programs, mut floats) = (0, 0);
    while programs < 5_000 {
        let t = common::random_type(&mut rng, false);
        let v = random_value(&mut rng, &t);
        if t.check(&v).is_err() {
            continue;
        }
        floats += u32::from(matches!(v, Value::F32(_) | Value::F64(_)));
        let literal = granite_txn::Expr::Literal(Box::new((t, v)));
        let program = Program {
            name: "p".into(),
            params: Vec::new(),
            body: vec![
                granite_txn::Stmt::Let(literal.clone()),
                granite_txn::Stmt::Return(literal),
            ],
        };
        round_trip(&c, &program);
        programs += 1;
    }
    assert!(floats > 100);
    // The largest decimals, which random mantissas below 10^38 do not reach.
    for (t, text) in [
        ("Decimal(76, 0)", "9".repeat(76)),
        ("Decimal(76, 76)", format!("-0.{}", "9".repeat(76))),
        (
            "u256",
            "115792089237316195423570985008687907853269984665640564039457584007913129639935".into(),
        ),
        (
            "i256",
            "-57896044618658097711785492504343953926634992332820282019728792003956564819968".into(),
        ),
    ] {
        let source = format!("program p()\n  return {t} {text}\n");
        let program =
            compile_program("max", source.as_bytes(), &c).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(print_program(&c, &program), source);
        let over = format!("program p()\n  return {t} {text}9\n");
        let e = compile_program("over", over.as_bytes(), &c).unwrap_err();
        assert_eq!(e.problem, Problem::Program(Error::InvalidLiteral), "{over}");
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `schemas/<schema>/*.program` against `schemas/<schema>.schema`, in path order.
fn reference_programs() -> Vec<(String, Catalog, Vec<u8>)> {
    let dir = repo().join("schemas");
    let mut out = Vec::new();
    let mut schemas: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    schemas.retain(|f| f.extension().is_some_and(|e| e == "schema"));
    schemas.sort();
    for schema in schemas {
        let catalog = catalog_of(&std::fs::read(&schema).unwrap());
        let Ok(entries) = std::fs::read_dir(schema.with_extension("")) else {
            continue;
        };
        let mut files: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
        files.retain(|f| f.extension().is_some_and(|e| e == "program"));
        files.sort();
        for file in files {
            let name = file.strip_prefix(&dir).unwrap().display().to_string();
            out.push((name, catalog.clone(), std::fs::read(&file).unwrap()));
        }
    }
    out
}

/// Guards name the branch an access runs in: an `else` as `not (...)`,
/// the branches of a `match` with `is_null`, outermost first.
#[test]
fn reports_name_the_conditions_of_each_access() {
    let c = catalog();
    let text = "\
program p(u: u64)
  if exists account[u] {
    delete holding[u, u32 1]
  } else {
    let a = match get asset[u32 1] { null => false, x => exists quote[x.asset, u16 1] }
  }
";
    let program = compile_program("p", text.as_bytes(), &c).unwrap_or_else(|e| panic!("{e}"));
    let r = report(&c, &program);
    for line in [
        "-- get account: static key, partition (u)",
        "-- delete holding: dependent key, partition (u), when exists account[u]",
        "-- get asset: dependent key, partition (u32 1), when not (exists account[u])",
        "-- get quote: dependent key, partition (l1.asset), when not (exists account[u]) and not (is_null(get asset[u32 1]))",
        "-- l1 is the value of get asset[u32 1] where it is not null",
    ] {
        assert!(r.lines().any(|l| l == line), "{line}\n{r}");
    }
}

/// The reports of the reference programs are the committed golden file.
/// After an intended change, rewrite it with `GRANITE_BLESS=1`.
#[test]
fn reference_programs_match_the_golden_report() {
    let programs = reference_programs();
    assert!(programs.len() >= 2, "{programs:?}");
    let mut all = String::new();
    for (name, catalog, source) in &programs {
        let program = compile_program(name, source, catalog).unwrap_or_else(|e| panic!("{e}"));
        round_trip(catalog, &program);
        all.push_str(&format!("-- {name}\n{}\n", report(catalog, &program)));
    }
    let golden = repo().join("schemas/programs.report");
    if std::env::var_os("GRANITE_BLESS").is_some() {
        std::fs::write(&golden, &all).unwrap();
    }
    let committed = std::fs::read_to_string(&golden).unwrap_or_default();
    assert!(
        committed == all,
        "schemas/programs.report differs from the compiler's report; rerun with GRANITE_BLESS=1 after an intended change:\n{all}"
    );
}

/// The programs in the code blocks of document 03: each is a reference
/// program, which compiles, except the one written with the `on` blocks of
/// T5.2.6.
#[test]
fn document_examples_are_reference_programs() {
    let doc =
        std::fs::read_to_string(repo().join("docs/03-data-model-and-transactions.md")).unwrap();
    let sources: Vec<String> = reference_programs()
        .into_iter()
        .map(|(_, _, s)| String::from_utf8(s).unwrap())
        .collect();
    let mut found = Vec::new();
    for block in doc.split("```").skip(1).step_by(2) {
        // A program runs from its `program` line to a line that is not indented.
        let mut program: Option<String> = None;
        for line in block.lines().chain([""]) {
            if let Some(p) = &mut program {
                if line.starts_with(' ') {
                    p.push_str(line);
                    p.push('\n');
                    continue;
                }
                found.push(program.take().unwrap());
            }
            let name = line
                .strip_prefix("program ")
                .and_then(|l| l.split_once('('));
            if name.is_some_and(|(n, _)| !n.contains(' ')) {
                program = Some(format!("{line}\n"));
            }
        }
    }
    let names: Vec<&str> = found
        .iter()
        .map(|p| p[8..].split('(').next().unwrap())
        .collect();
    assert_eq!(names, ["transfer", "place_order", "settle_trade"]);
    for p in &found {
        if p.contains("\n  on ") {
            continue;
        }
        assert!(
            sources.iter().any(|s| s.contains(p.as_str())),
            "not a reference program:\n{p}"
        );
    }

    // The encoding example of section 5.1, from the bytes the document lists.
    let at = doc.find("Example: `program t(a: u64)`").unwrap();
    let listing = doc[at..].split("```").nth(1).unwrap();
    let bytes: Vec<u8> = (listing.lines())
        .flat_map(|l| l.split("   ").next().unwrap().split_whitespace())
        .flat_map(|pair| {
            (0..pair.len())
                .step_by(2)
                .map(move |i| u8::from_str_radix(&pair[i..i + 2], 16).unwrap())
        })
        .collect();
    let c = catalog_of(b"schema version 1\ntable group g partition by (a u64) hash\ntable acc immutable layout row in g key (a) a u64");
    let text = "program t(a: u64)\n  require not is_null(get acc[a]) else E\n  return a + u64 1\n";
    assert!(doc.contains("`require not is_null(get acc[a]) else E` and `return a + u64 1`"));
    let program = compile_program("t", text.as_bytes(), &c).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(program.encode().unwrap(), bytes);
}

/// Text nested or repeated past the limits is rejected at its place, before
/// the parser's stack or work grows with it; 1 MiB inputs compile quickly.
#[test]
fn nesting_and_size_are_bounded() {
    let c = catalog();
    let problem = |text: &str| {
        compile_program("big", text.as_bytes(), &c)
            .map(|_| ())
            .map_err(|e| (e.pos, e.problem))
    };
    let depth = 2 * FORMAT.program_depth_max as usize;
    let deep = [
        format!("{}u", "(".repeat(100_000)),
        format!("{}u", "not ".repeat(100_000)),
        format!("{}u", "- ".repeat(100_000)),
        format!("{}u", "is_null(".repeat(100_000)),
    ];
    for text in &deep {
        let (pos, problem) = problem(&with_body(&format!("return {text}"))).unwrap_err();
        assert_eq!(problem, Problem::Program(Error::TooDeep), "{}", &text[..20]);
        assert!(pos.line == 2, "{pos:?}");
    }
    let chain = with_body(&"if true { } else ".repeat(100_000));
    let (pos, p) = problem(&chain).unwrap_err();
    assert_eq!(p, Problem::Program(Error::TooDeep));
    assert!(pos.line == 2, "{pos:?}");
    // Within the parser's nesting, but deeper than the IR's: the program as a whole.
    let parens = format!("{}u{}", "(".repeat(depth - 2), ")".repeat(depth - 2));
    assert_eq!(problem(&with_body(&format!("return {parens}"))), Ok(()));
    let chain = format!("return u{}", " + u".repeat(3_000));
    assert_eq!(
        problem(&with_body(&chain)),
        Err((Pos { line: 1, column: 1 }, Problem::Program(Error::TooDeep)))
    );
    // The node after the last allowed one.
    let nodes = FORMAT.program_nodes_max as usize;
    let many = format!("return u{}", " + u".repeat(nodes));
    let at = "program p(u: u64)\n  return u".len() + " + u".repeat(nodes / 2 - 1).len() + 1;
    assert_eq!(
        problem(&with_body(&many)),
        Err((
            Pos {
                line: 2,
                column: (at - "program p(u: u64)\n".len() + 1) as u32
            },
            Problem::Program(Error::TooManyNodes)
        ))
    );

    const MIB: usize = 1 << 20;
    let fill = |head: &str, unit: &str| {
        let mut s = head.to_string();
        while s.len() + unit.len() <= MIB {
            s.push_str(unit);
        }
        s
    };
    let shapes = [
        fill("program p(u: u64)\n  return u", " + u"),
        fill("program p(u: u64)\n  return u64 ", "0"),
        fill("program p(u: u64)\n  return String(1048576) \"", "x"),
        fill("program p(u: u64)\n", "  let x = u\n"),
        fill("program p(u: u64)\n  let x = u\n  if ", "x = x and "),
        fill("program p(", "a: u64, "),
        fill("program p(u: u64) -- ", "x"),
    ];
    for (i, text) in shapes.iter().enumerate() {
        assert!(text.len() <= MIB);
        let start = std::time::Instant::now();
        let result = problem(text);
        let elapsed = start.elapsed();
        println!("shape {i}: {elapsed:?}, {result:?}");
        assert!(elapsed.as_secs() < 5, "shape {i} took {elapsed:?}");
    }
}

/// The tool, run twice on the same files, writes the same bytes; it prints
/// the IR back as text and reports the program.
#[test]
fn the_tool_compiles_prints_and_reports_programs() {
    let tool = env!("CARGO_BIN_EXE_granite-schemac");
    let schema = repo().join("schemas/bank.schema");
    let source = repo().join("schemas/bank/transfer.program");
    let run = |args: &[&std::ffi::OsStr]| {
        let out = std::process::Command::new(tool)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    let args = [
        "compile-program".as_ref(),
        schema.as_os_str(),
        source.as_os_str(),
    ];
    let ir = run(&args);
    assert_eq!(run(&args), ir);
    let catalog = catalog_of(&std::fs::read(&schema).unwrap());
    let program = Program::decode(&ir).unwrap();
    let file = std::env::temp_dir().join(format!("schemac-{}.program", std::process::id()));
    std::fs::write(&file, &ir).unwrap();
    let text = run(&[
        "print-program".as_ref(),
        schema.as_os_str(),
        file.as_os_str(),
    ]);
    std::fs::remove_file(&file).unwrap();
    assert_eq!(
        String::from_utf8(text).unwrap(),
        print_program(&catalog, &program)
    );
    let text = run(&["report".as_ref(), schema.as_os_str(), source.as_os_str()]);
    assert_eq!(String::from_utf8(text).unwrap(), report(&catalog, &program));
}
