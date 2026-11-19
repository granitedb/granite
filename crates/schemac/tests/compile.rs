//! The schema compiler against the catalog rules, the reference schemas and
//! random catalogs. Rerun a failure of the property test with
//! `GRANITE_SEED=<seed> cargo test -p granite-schemac --test compile`.

use granite_base::constants::FORMAT;
use granite_schema::{
    Catalog, Column, Error, Kind, Layout, Partitioning, Retention, Table, TableGroup,
};
use granite_schemac::{CompileError, Pos, Problem, compile, print};
use granite_types::key::Order;
use std::path::Path;

#[allow(
    dead_code,
    reason = "shared with the schema crate's tests, which use all of it"
)]
#[path = "../../schema/tests/common/mod.rs"]
mod common;
use common::{Rng, random_type};

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
fn fails(problem: Problem, text: &str) {
    let (text, pos) = marked(text);
    let expected = CompileError {
        file: "case.schema".into(),
        pos,
        problem,
    };
    assert_eq!(
        compile("case.schema", text.as_bytes()),
        Err(expected),
        "{text}"
    );
}

/// A group `g` keyed by `k u64` and a table `t` in it, with `table` spliced
/// in after `table t immutable`.
fn with_table(table: &str) -> String {
    format!(
        "schema version 1\ntable group g partition by (k u64) hash\ntable t immutable {table}\n"
    )
}

const OK_TABLE: &str = "layout row in g key (k, id) k u64, id Id128, s String(10)";

#[test]
fn every_rule_is_reported_with_its_place() {
    use Error as E;
    assert!(compile("ok", with_table(OK_TABLE).as_bytes()).is_ok());
    let groups: String = (0..=FORMAT.tables_max)
        .map(|i| {
            format!(
                "table group {}g{i} partition by (k u64) hash\n",
                if i == FORMAT.tables_max { "@" } else { "" }
            )
        })
        .collect();
    let tables: String = (0..=FORMAT.tables_max)
        .map(|i| {
            let mark = if i == FORMAT.tables_max { "@" } else { "" };
            format!("table {mark}t{i} immutable layout row in g key (k) k u64\n")
        })
        .collect();
    let columns: String = (0..FORMAT.columns_per_table_max)
        .map(|i| format!(", @c{i} u8"))
        .collect();
    let columns = columns.replacen('@', "", FORMAT.columns_per_table_max as usize - 1);
    let long = "n".repeat(usize::from(u8::MAX) + 1);
    let cases: Vec<(Error, String)> = vec![
        (E::TooManyGroups, format!("schema version 1\n{groups}")),
        (E::TooManyTables, format!("schema version 1\ntable group g partition by (k u64) hash\n{tables}")),
        (E::TooManyColumns, with_table(&format!("layout row in g key (k) k u64{columns}"))),
        (E::InvalidName, with_table(&format!("layout row in g key (k) k u64, @{long} u8"))),
        (E::InvalidName, with_table("layout row in g key (k) k u64, @\"\" u8")),
        (E::InvalidName, format!("schema version 1\ntable group @{long} partition by (k u64) hash")),
        (E::DuplicateName, with_table("layout row in g key (k) k u64, @k u8")),
        (E::DuplicateName, with_table("layout row in g key (k) k u64, a u8, b u8, @a u8")),
        (E::DuplicateName, format!("{}table @t immutable {OK_TABLE}", with_table(OK_TABLE))),
        (E::DuplicateName, format!("{}table group @g partition by (k u64) hash", with_table(OK_TABLE))),
        (E::DuplicateName, "schema version 1\ntable group g partition by (k u64, @k u8) hash".into()),
        (E::InvalidType, with_table("layout row in g key (k) k u64, @d Decimal(77, 0)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, @d Decimal(10, 11)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, d @Decimal(256, 0)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, @d FixedBytes(0)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, @d Nullable(Nullable(u8))")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, d @Timestamp(5)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, d @String(4294967296)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, d @String(99999999999999999999)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, d @Enum8(0)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, d @Enum8(257)")),
        (E::InvalidType, with_table("layout row in g key (k) k u64, d @Enum16(65537)")),
        (
            E::InvalidType,
            with_table("layout row in g key (k) k u64, d Nullable(LowCardinality(Nullable(@LowCardinality(u8))))"),
        ),
        (E::EmptyKey, with_table("layout row in g @key () k u64")),
        (E::EmptyKey, "schema version 1\ntable group g partition by @() hash".into()),
        (E::UnknownColumn, with_table("layout row in g key (k, @nope) k u64")),
        (E::UnknownColumn, with_table("layout groups((k), (@nope)) in g key (k) k u64")),
        (E::RepeatedKeyColumn, with_table("layout row in g key (k, id, @id desc) k u64, id Id128")),
        (E::KeyColumnNotAllowed, with_table("layout row in g key (k, @f) k u64, f f64")),
        (E::KeyColumnNotAllowed, with_table("layout row in g key (k, @n) k u64, n LowCardinality(Nullable(u8))")),
        (E::KeyColumnNotAllowed, "schema version 1\ntable group g partition by (@k f32) hash".into()),
        (E::PartitionKeyNotPrefix, with_table("layout row in g @key (id, k) k u64, id Id128")),
        (E::PartitionKeyNotPrefix, with_table("layout row in g @key (k) k u32")),
        (E::KeyTooLarge, with_table("layout row in g @key (k, s) k u64, s String(300)")),
        (E::KeyTooLarge, "schema version 1\ntable group g partition by @(k String(300)) hash".into()),
        (E::RowTooLarge, "schema version 1\ntable group g partition by (k u64) hash\ntable @t immutable layout row in g key (k) k u64, b Bytes(65536)".into()),
        (E::NoLayout, "schema version 1\ntable group g partition by (k u64) hash\ntable @t immutable in g key (k) k u64".into()),
        (E::LayoutGroups, with_table("@layout groups((k), (id)) in g key (k) k u64, id Id128, s u8")),
        (E::LayoutGroups, with_table("@layout groups((k, id), (id)) in g key (k) k u64, id Id128")),
        (E::LayoutGroups, with_table("@layout groups((k, id), ()) in g key (k) k u64, id Id128")),
        (E::RetentionUnsupported, with_table("layout row in g key (k) k u64 @retention archive after 1 ms")),
        (E::RetentionUnsupported, with_table("layout row in g key (k) k u64 @retention drop after 1 ms")),
    ];
    for (rule, text) in &cases {
        fails(Problem::Rule(*rule), text);
    }

    // Rule 1, from text: the file is not UTF-8.
    let mut bytes = b"schema version 1\ntable group g\xC3\xA9".to_vec();
    bytes.extend(b" \xFF partition by (k u64) hash");
    let err = compile("case.schema", &bytes).unwrap_err();
    assert_eq!(
        (err.pos, err.problem),
        (
            Pos {
                line: 2,
                column: 16
            },
            Problem::Rule(E::Malformed)
        )
    );

    // Every rule but 2, the version of the binary encoding, which text does not carry.
    for code in (1..=18).filter(|&c| c != 2) {
        let covered = code == E::Malformed as u8 || cases.iter().any(|(e, _)| *e as u8 == code);
        assert!(covered, "rule {code} has no case");
    }
}

#[test]
fn errors_name_file_line_column_and_rule() {
    let text = with_table("layout row in g key (k) k u64,\n  a u8,\n  a u16");
    let err = compile("dir/shop.schema", text.as_bytes()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "dir/shop.schema:5:3: rule 7 DuplicateName (document 03 section 1.1)"
    );
    let err = compile("x.schema", b"schema version 1\n\ntable t").unwrap_err();
    assert_eq!(
        err.to_string(),
        "x.schema:3:8: expected `immutable` or `versioned`"
    );
}

#[test]
fn text_errors_are_placed() {
    let text = |m| Problem::Text(m);
    fails(
        text("expected `schema version` at the start"),
        "@table group g partition by (k u64) hash",
    );
    fails(text("expected a number"), "schema version @x");
    fails(text("expected a number"), "schema version @12ab");
    fails(
        text("number above 2^64 - 1"),
        "schema version @18446744073709551616",
    );
    fails(text("unexpected character"), "schema version 1 @;");
    fails(
        text("expected `--` to start a comment"),
        "schema version 1 @- x",
    );
    fails(
        text("unterminated quoted name"),
        "schema version 1\ntable group @\"g partition",
    );
    fails(
        text("expected `\\\"` or `\\\\` in a quoted name"),
        "schema version 1\ntable group @\"a\\n\"",
    );
    fails(
        text("expected `table` or the end of the file"),
        "schema version 1\n@group g",
    );
    fails(
        text("unknown type"),
        &with_table("layout row in g key (k) k @u63"),
    );
    fails(
        text("expected a type"),
        &with_table("layout row in g key (k) k @,"),
    );
    fails(
        text("enum value declared twice"),
        &with_table("layout row in g key (k) k u64, e Enum8(a, b, @a)"),
    );
    fails(
        text("unknown group; a group is declared before its tables"),
        "schema version 1\ntable t immutable layout row in @g key (k) k u64",
    );
    fails(
        text("expected `,` or `)`"),
        &with_table("layout row in g key (k @id) k u64"),
    );
    fails(
        text("expected `after`"),
        &with_table("layout row in g key (k) k u64 retention drop @1 ms"),
    );
}

#[test]
fn reference_schemas_compile_deterministically_and_round_trip() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.retain(|f| f.extension().is_some_and(|e| e == "schema"));
    files.sort();
    assert!(!files.is_empty(), "no schemas in {}", dir.display());
    for file in files {
        let name = file.display().to_string();
        let source = std::fs::read(&file).unwrap();
        let bytes = compile(&name, &source).unwrap_or_else(|e| panic!("{e}"));
        // The output is a function of the input alone.
        assert_eq!(compile(&name, &source), Ok(bytes.clone()), "{name}");
        let text = print(&Catalog::decode(&bytes).unwrap());
        assert_eq!(
            compile("printed", text.as_bytes()),
            Ok(bytes.clone()),
            "{name}"
        );
        println!("{name}: {} bytes", bytes.len());
    }
}

/// Names that need quoting, collide with keywords or are plain.
const NAMES: [&str; 12] = [
    "a", "group", "table", "key", "u64", "desc", "x y", "\"", "\\", "é", "_9", "9a",
];

fn random_name(rng: &mut Rng, taken: &mut Vec<String>) -> String {
    let mut name = NAMES[rng.below(NAMES.len() as u64) as usize].to_string();
    if taken.contains(&name) {
        name = format!("{name}{}", taken.len());
    }
    taken.push(name.clone());
    name
}

/// A random catalog of up to four groups of up to three tables, with
/// random names, types, keys, layouts and retention; `None` when it breaks
/// a limit.
fn random_catalog(rng: &mut Rng) -> Option<Catalog> {
    let (mut group_names, mut table_names) = (Vec::new(), Vec::new());
    let groups = (0..1 + rng.below(4))
        .map(|_| {
            let mut column_names = Vec::new();
            let partition_key: Vec<Column> = (0..1 + rng.below(2))
                .map(|_| Column {
                    name: random_name(rng, &mut column_names),
                    ty: random_type(rng, true),
                })
                .collect();
            let tables = (0..rng.below(4))
                .map(|_| {
                    let mut names = column_names.clone();
                    let mut columns = partition_key.clone();
                    let extra = (0..1 + rng.below(6)).map(|_| Column {
                        name: random_name(rng, &mut names),
                        ty: random_type(rng, false),
                    });
                    columns.extend(extra.collect::<Vec<_>>());
                    let n = columns.len() as u16;
                    let order = |rng: &mut Rng| [Order::Asc, Order::Desc][rng.below(2) as usize];
                    let mut primary_key: Vec<(u16, Order)> = (0..partition_key.len() as u16)
                        .map(|i| (i, order(rng)))
                        .collect();
                    // Extra key columns, kept only where the type is allowed in a key.
                    for i in partition_key.len() as u16..n {
                        if rng.below(3) == 0 && columns[usize::from(i)].ty.key_size_max().is_ok() {
                            primary_key.push((i, order(rng)));
                        }
                    }
                    let layout = match rng.below(3) {
                        0 => Layout::Row,
                        1 => Layout::Columnar,
                        _ => {
                            let mut groups = vec![Vec::new(); 1 + rng.below(u64::from(n)) as usize];
                            for i in 0..n {
                                let g = if usize::from(i) < groups.len() {
                                    usize::from(i)
                                } else {
                                    rng.below(groups.len() as u64) as usize
                                };
                                groups[g].push(i);
                            }
                            groups.reverse();
                            Layout::Groups(groups)
                        }
                    };
                    let (kind, retention) = match rng.below(3) {
                        0 => (Kind::Immutable, Retention::Forever),
                        1 => (Kind::Versioned, Retention::Forever),
                        _ => (Kind::Versioned, Retention::DropAfterMs(rng.next())),
                    };
                    Table {
                        name: random_name(rng, &mut table_names),
                        kind,
                        columns,
                        primary_key,
                        layout: Some(layout),
                        retention,
                    }
                })
                .collect();
            TableGroup {
                name: random_name(rng, &mut group_names),
                partitioning: [Partitioning::Hash, Partitioning::Range][rng.below(2) as usize],
                partition_key,
                tables,
            }
        })
        .collect();
    let catalog = Catalog {
        version: rng.next(),
        groups,
    };
    catalog.validate().ok().map(|()| catalog)
}

#[test]
fn compile_print_compile_is_byte_identical() {
    let seed = std::env::var("GRANITE_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED);
    println!("GRANITE_SEED={seed}");
    let mut rng = Rng(seed);
    let (mut cases, mut rejected) = (0, 0);
    while cases < 2_000 {
        let Some(catalog) = random_catalog(&mut rng) else {
            rejected += 1;
            continue;
        };
        cases += 1;
        let text = print(&catalog);
        let bytes = compile("random", text.as_bytes()).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(Ok(bytes.clone()), catalog.encode(), "{text}");
        let again = print(&Catalog::decode(&bytes).unwrap());
        assert_eq!(again, text);
        assert_eq!(compile("random", again.as_bytes()), Ok(bytes));
    }
    println!("{cases} catalogs, {rejected} over a limit");
}

/// Inputs of 1 MiB shaped to stress each part of the parser all finish
/// quickly: the work is linear apart from ordered maps of names.
#[test]
fn one_mebibyte_inputs_are_bounded() {
    // The input bound of the fuzz gate, one message.
    const MIB: usize = FORMAT.message_size_max as usize;
    let fill = |head: &str, unit: &str| {
        let mut s = head.to_string();
        while s.len() + unit.len() <= MIB {
            s.push_str(unit);
        }
        s
    };
    let head = "schema version 1\ntable group g partition by (k u64) hash\n";
    let table = format!("{head}table t immutable layout row in g key (k");
    let shapes = [
        fill(&format!("{table}) k u64, d "), "Nullable("),
        fill("schema version 1\n", "-- comment\n"),
        fill("schema version 1\ntable group \"", "x"),
        fill(&format!("{table}) k u64"), ", c u8"),
        fill(
            &format!("{table}) k u64"),
            ", c Nullable(LowCardinality(u8))",
        ),
        fill(&table, ", k"),
        fill(&format!("{table}) k u64, e Enum16(a"), ", a"),
        (0..)
            .map(|i| format!(", v{i}"))
            .scan(format!("{table}) k u64, e Enum16(v"), |s, v| {
                s.push_str(&v);
                Some(s.clone())
            })
            .take_while(|s| s.len() <= MIB)
            .last()
            .unwrap(),
        fill(&format!("{head}table t immutable layout groups((k"), ", k"),
        fill(
            "schema version 1\n",
            "table group g partition by (k u64) hash\n",
        ),
        fill(head, "table t immutable layout row in g key (k) k u64\n"),
        fill("schema version ", "9"),
    ];
    for (i, text) in shapes.iter().enumerate() {
        assert!(text.len() <= MIB);
        let start = std::time::Instant::now();
        let result = compile("big", text.as_bytes());
        let elapsed = start.elapsed();
        println!(
            "shape {i}: {elapsed:?}, {:?}",
            result.map(|b| b.len()).map_err(|e| e.problem)
        );
        assert!(elapsed.as_secs() < 5, "shape {i} took {elapsed:?}");
    }
}

/// The tool itself, run twice on the same file, writes the same bytes, and
/// prints them back as text.
#[test]
fn the_tool_is_deterministic() {
    let tool = env!("CARGO_BIN_EXE_granite-schemac");
    let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/exchange.schema");
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
    let compile_args = ["compile".as_ref(), schema.as_os_str()];
    let first = run(&compile_args);
    assert_eq!(run(&compile_args), first);
    assert_eq!(
        Ok(first.clone()),
        compile("x", &std::fs::read(&schema).unwrap())
    );
    let catalog = std::env::temp_dir().join(format!("schemac-{}.catalog", std::process::id()));
    std::fs::write(&catalog, &first).unwrap();
    let text = run(&["print".as_ref(), catalog.as_os_str()]);
    std::fs::remove_file(&catalog).unwrap();
    assert_eq!(compile("printed", &text), Ok(first));
}
