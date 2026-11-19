use super::*;
use granite_types::TimePrecision;

const ALL: [Error; 18] = [
    Error::Malformed,
    Error::UnknownVersion,
    Error::TooManyGroups,
    Error::TooManyTables,
    Error::TooManyColumns,
    Error::InvalidName,
    Error::DuplicateName,
    Error::InvalidType,
    Error::EmptyKey,
    Error::UnknownColumn,
    Error::RepeatedKeyColumn,
    Error::KeyColumnNotAllowed,
    Error::PartitionKeyNotPrefix,
    Error::KeyTooLarge,
    Error::RowTooLarge,
    Error::NoLayout,
    Error::LayoutGroups,
    Error::RetentionUnsupported,
];

fn col(name: &str, ty: Type) -> Column {
    Column {
        name: name.into(),
        ty,
    }
}

/// The ledger table of document 03 section 10, in its group.
fn base() -> Catalog {
    let user_id = col("user_id", Type::U64);
    Catalog {
        version: 1,
        groups: vec![TableGroup {
            name: "by_user".into(),
            partitioning: Partitioning::Hash,
            partition_key: vec![user_id.clone()],
            tables: vec![Table {
                name: "entry".into(),
                kind: Kind::Immutable,
                columns: vec![
                    user_id,
                    col("id", Type::Id128),
                    col(
                        "amount",
                        Type::Decimal {
                            precision: 38,
                            scale: 18,
                        },
                    ),
                    col("note", Type::String { max: 100 }),
                ],
                primary_key: vec![(0, Order::Asc), (1, Order::Asc)],
                layout: Some(Layout::Row),
                retention: Retention::Forever,
            }],
        }],
    }
}

fn table(c: &mut Catalog) -> &mut Table {
    &mut c.groups[0].tables[0]
}

fn named_tables(n: usize) -> Vec<Table> {
    let entry = base().groups[0].tables[0].clone();
    (0..n)
        .map(|i| Table {
            name: format!("t{i}"),
            ..entry.clone()
        })
        .collect()
}

fn with(change: impl FnOnce(&mut Catalog)) -> Result<(), Error> {
    let mut c = base();
    change(&mut c);
    c.validate()
}

#[test]
fn each_rule_rejects_with_its_code() {
    assert_eq!(base().validate(), Ok(()));
    type Change = Box<dyn Fn(&mut Catalog)>;
    let cases: Vec<(Error, Change)> = vec![
        (
            Error::TooManyGroups,
            Box::new(|c| {
                let g = c.groups[0].clone();
                c.groups.extend((0..FORMAT.tables_max).map(|i| TableGroup {
                    name: format!("g{i}"),
                    tables: vec![],
                    ..g.clone()
                }))
            }),
        ),
        (
            Error::TooManyTables,
            Box::new(|c| c.groups[0].tables = named_tables(FORMAT.tables_max as usize + 1)),
        ),
        (
            Error::TooManyColumns,
            Box::new(|c| {
                let max = FORMAT.columns_per_table_max as usize;
                let extra = (0..max).map(|i| col(&format!("c{i}"), Type::U8));
                table(c).columns.extend(extra.skip(3));
            }),
        ),
        (Error::InvalidName, Box::new(|c| table(c).name.clear())),
        (
            Error::InvalidName,
            Box::new(|c| table(c).columns[3].name = "n".repeat(usize::from(u8::MAX) + 1)),
        ),
        (
            Error::DuplicateName,
            Box::new(|c| table(c).columns[3].name = "id".into()),
        ),
        (
            Error::DuplicateName,
            Box::new(|c| {
                let t = table(c).clone();
                c.groups[0].tables.push(t);
            }),
        ),
        (
            Error::DuplicateName,
            Box::new(|c| {
                let g = TableGroup {
                    tables: vec![],
                    ..c.groups[0].clone()
                };
                c.groups.push(g);
            }),
        ),
        (
            Error::InvalidType,
            Box::new(|c| {
                table(c).columns[2].ty = Type::Decimal {
                    precision: 77,
                    scale: 0,
                }
            }),
        ),
        (
            Error::EmptyKey,
            Box::new(|c| c.groups[0].partition_key.clear()),
        ),
        (Error::EmptyKey, Box::new(|c| table(c).primary_key.clear())),
        (
            Error::UnknownColumn,
            Box::new(|c| table(c).primary_key.push((4, Order::Asc))),
        ),
        (
            Error::UnknownColumn,
            Box::new(|c| table(c).layout = Some(Layout::Groups(vec![vec![0, 1, 2, 3, 4]]))),
        ),
        (
            Error::RepeatedKeyColumn,
            Box::new(|c| table(c).primary_key.push((1, Order::Desc))),
        ),
        (
            Error::KeyColumnNotAllowed,
            Box::new(|c| {
                table(c).columns[2].ty = Type::F64;
                table(c).primary_key.push((2, Order::Asc));
            }),
        ),
        (
            Error::KeyColumnNotAllowed,
            Box::new(|c| {
                let ty = Type::LowCardinality(Box::new(Type::Nullable(Box::new(Type::U8))));
                table(c).columns[2].ty = ty;
                table(c).primary_key.push((2, Order::Asc));
            }),
        ),
        (
            Error::KeyColumnNotAllowed,
            Box::new(|c| {
                let mut g = c.groups[0].clone();
                g.name = "empty".into();
                g.tables.clear();
                g.partition_key[0].ty = Type::F32;
                c.groups.push(g);
            }),
        ),
        (
            Error::KeyTooLarge,
            Box::new(|c| {
                let mut g = c.groups[0].clone();
                g.name = "empty".into();
                g.tables.clear();
                g.partition_key[0].ty = Type::String { max: 300 };
                c.groups.push(g);
            }),
        ),
        (
            Error::PartitionKeyNotPrefix,
            Box::new(|c| table(c).primary_key.reverse()),
        ),
        (
            Error::PartitionKeyNotPrefix,
            Box::new(|c| table(c).columns[0].ty = Type::U32),
        ),
        (
            Error::PartitionKeyNotPrefix,
            Box::new(|c| table(c).columns[0].name = "uid".into()),
        ),
        (
            Error::PartitionKeyNotPrefix,
            Box::new(|c| {
                let id = col("id", Type::Id128);
                c.groups[0].partition_key.push(id);
                table(c).primary_key.pop();
            }),
        ),
        (
            Error::KeyTooLarge,
            Box::new(|c| {
                table(c).columns[3].ty = Type::String { max: 255 };
                table(c).primary_key.push((3, Order::Asc));
            }),
        ),
        (
            Error::RowTooLarge,
            Box::new(|c| {
                let max = FORMAT.row_size_max as u32;
                table(c).columns[3].ty = Type::Bytes { max };
            }),
        ),
        (Error::NoLayout, Box::new(|c| table(c).layout = None)),
        (
            Error::LayoutGroups,
            Box::new(|c| table(c).layout = Some(Layout::Groups(vec![vec![0, 1], vec![2]]))),
        ),
        (
            Error::LayoutGroups,
            Box::new(|c| {
                let groups = vec![vec![0, 1], vec![2, 3, 1]];
                table(c).layout = Some(Layout::Groups(groups));
            }),
        ),
        (
            Error::LayoutGroups,
            Box::new(|c| {
                let groups = vec![vec![0, 1, 2, 3], vec![]];
                table(c).layout = Some(Layout::Groups(groups));
            }),
        ),
        (
            Error::RetentionUnsupported,
            Box::new(|c| table(c).retention = Retention::ArchiveAfterMs(1)),
        ),
        (
            Error::RetentionUnsupported,
            Box::new(|c| table(c).retention = Retention::DropAfterMs(1)),
        ),
        (
            Error::RetentionUnsupported,
            Box::new(|c| {
                table(c).kind = Kind::Versioned;
                table(c).retention = Retention::ArchiveAfterMs(1);
            }),
        ),
    ];
    for (code, change) in &cases {
        assert_eq!(with(change), Err(*code), "{code:?}");
    }
    // Every rule has a case; the two decoding codes are tested in `decode_rejects_bad_bytes`.
    for code in &ALL[2..] {
        assert!(cases.iter().any(|(c, _)| c == code), "{code:?} has no case");
    }
    // The variants that are allowed.
    assert_eq!(
        with(|c| {
            table(c).kind = Kind::Versioned;
            table(c).retention = Retention::DropAfterMs(1);
            table(c).layout = Some(Layout::Groups(vec![vec![3, 0], vec![2, 1]]));
            c.groups[0].partitioning = Partitioning::Range;
        }),
        Ok(())
    );
    assert_eq!(with(|c| table(c).layout = Some(Layout::Columnar)), Ok(()));
}

#[test]
fn codes_are_listed_in_document_03() {
    let doc = include_str!("../../../docs/03-data-model-and-transactions.md");
    for code in ALL {
        let row = format!("| {} | `{code:?}` |", code as u8);
        assert!(doc.contains(&row), "document 03 lacks the row {row}");
    }
}

#[test]
fn column_limit() {
    let table = |n: usize| {
        let columns: Vec<Column> = (0..n).map(|i| col(&format!("c{i}"), Type::U8)).collect();
        let mut c = base();
        c.groups[0].partition_key = vec![columns[0].clone()];
        c.groups[0].tables[0] = Table {
            columns,
            primary_key: vec![(0, Order::Asc)],
            ..c.groups[0].tables[0].clone()
        };
        c
    };
    let max = FORMAT.columns_per_table_max as usize;
    assert_eq!(table(max).validate(), Ok(()));
    assert_eq!(table(max + 1).validate(), Err(Error::TooManyColumns));
    assert_eq!(
        Catalog::decode(&table(max).encode().unwrap()),
        Ok(table(max))
    );
}

#[test]
fn table_limit() {
    let max = FORMAT.tables_max as usize;
    assert_eq!(with(|c| c.groups[0].tables = named_tables(max)), Ok(()));
    let mut split = base();
    split.groups[0].tables = named_tables(max);
    let second = split.groups[0].tables.split_off(max / 2);
    split.groups.push(TableGroup {
        name: "second".into(),
        tables: second,
        ..split.groups[0].clone()
    });
    assert_eq!(split.validate(), Ok(()));
    split.groups[1]
        .tables
        .push(base().groups[0].tables[0].clone());
    assert_eq!(split.validate(), Err(Error::TooManyTables));
}

#[test]
fn key_limit() {
    let key = |n: u32| {
        with(|c| {
            table(c).columns[3].ty = Type::FixedBytes(n);
            table(c).primary_key = vec![(0, Order::Asc), (3, Order::Desc)];
        })
    };
    let rest = FORMAT.primary_key_size_max as u32 - 8;
    assert_eq!(key(rest), Ok(()));
    assert_eq!(key(rest + 1), Err(Error::KeyTooLarge));
    // A String key counts at its escaped worst case, 2 * max + 2.
    let string = |max| {
        with(|c| {
            table(c).columns[0].ty = Type::String { max };
            c.groups[0].partition_key[0].ty = Type::String { max };
            table(c).primary_key.pop();
        })
    };
    let max = (FORMAT.primary_key_size_max as u32 - 2) / 2;
    assert_eq!(string(max), Ok(()));
    assert_eq!(string(max + 1), Err(Error::KeyTooLarge));
}

#[test]
fn row_limit() {
    let fixed = base().groups[0].tables[0].row_size_max() - 100 - SLOT;
    let row = |max: u64| {
        let mut c = base();
        table(&mut c).columns[3].ty = Type::Bytes {
            max: u32::try_from(max).unwrap(),
        };
        (c.validate(), table(&mut c).row_size_max())
    };
    let max = FORMAT.row_size_max - fixed - SLOT;
    assert_eq!(row(max), (Ok(()), FORMAT.row_size_max));
    assert_eq!(row(max + 1).0, Err(Error::RowTooLarge));
}

#[test]
fn row_must_fit_a_message_with_its_header() {
    let small = FormatConstants {
        message_size_max: FORMAT.row_size_max + FORMAT.header_size - 1,
        ..FORMAT
    };
    assert!(row_fits(FORMAT.row_size_max, &FORMAT));
    assert!(!row_fits(FORMAT.row_size_max + 1, &FORMAT));
    assert!(!row_fits(FORMAT.row_size_max, &small));
    assert!(row_fits(FORMAT.row_size_max - 1, &small));
}

#[test]
fn rows_are_encoded_with_slots_and_heap() {
    let mut c = base();
    let t = table(&mut c);
    t.columns
        .push(col("tag", Type::Nullable(Box::new(Type::Bytes { max: 4 }))));
    t.columns
        .push(col("flag", Type::Nullable(Box::new(Type::Bool))));
    let values = |tag| {
        vec![
            Value::U64(7),
            Value::Id128(9),
            Value::Decimal128(-1),
            Value::String("hi".into()),
            tag,
            Value::Null,
        ]
    };
    let row = t.encode_row(&values(Value::Bytes(vec![5]))).unwrap();
    let mut want = vec![7, 0, 0, 0, 0, 0, 0, 0, 9];
    want.extend([0; 15]);
    want.extend([0xff; 16]);
    want.extend([0, 0, 0, 0, 2, 0, 0, 0]); // note: offset 0, length 2
    want.extend([1, 2, 0, 0, 0, 1, 0, 0, 0]); // tag: present, offset 2, length 1
    want.extend([0, 0]); // flag: null
    want.extend([b'h', b'i', 5]);
    assert_eq!(row, want);
    let null = t.encode_row(&values(Value::Null)).unwrap();
    assert_eq!(null[48..57], [0; 9]);
    assert_eq!(null.len(), want.len() - 1);
    assert_eq!(
        t.encode_row(&values(Value::Bytes(vec![0; 5]))),
        Err(granite_types::Error::Length { len: 5, max: 4 })
    );
    assert_eq!(t.encode_row(&[]), Err(granite_types::Error::ColumnCount));
}

/// A catalog with every type, layout, kind and retention the rules allow.
fn varied() -> Catalog {
    let p = TimePrecision::Micros;
    let types = [
        Type::U8,
        Type::U16,
        Type::U32,
        Type::U64,
        Type::U128,
        Type::U256,
        Type::I8,
        Type::I16,
        Type::I32,
        Type::I64,
        Type::I128,
        Type::I256,
        Type::Decimal {
            precision: 76,
            scale: 3,
        },
        Type::F32,
        Type::F64,
        Type::Bool,
        Type::Date,
        Type::Timestamp(TimePrecision::Seconds),
        Type::Timestamp(TimePrecision::Nanos),
        Type::Duration(p),
        Type::Duration(TimePrecision::Millis),
        Type::Uuid,
        Type::Id128,
        Type::String { max: 300 },
        Type::Bytes { max: 70_000 / 2 },
        Type::FixedBytes(3),
        Type::Enum8 { max: 2 },
        Type::Enum16 { max: 700 },
        Type::Nullable(Box::new(Type::LowCardinality(Box::new(Type::String {
            max: 9,
        })))),
        Type::LowCardinality(Box::new(Type::Nullable(Box::new(Type::LowCardinality(
            Box::new(Type::Bytes { max: 1 }),
        ))))),
    ];
    let mut c = base();
    c.version = u64::MAX;
    let t = table(&mut c);
    t.kind = Kind::Versioned;
    t.retention = Retention::DropAfterMs(u64::MAX - 1);
    t.primary_key[1].1 = Order::Desc;
    t.columns.extend(
        types
            .into_iter()
            .enumerate()
            .map(|(i, ty)| col(&format!("c{i}"), ty)),
    );
    let n = t.columns.len() as u16;
    t.layout = Some(Layout::Groups(vec![vec![1, 0], (2..n).rev().collect()]));
    let mut ledger = base().groups[0].clone();
    ledger.name = "ledger".into();
    ledger.partitioning = Partitioning::Range;
    ledger.tables[0].name = "columnar".into();
    ledger.tables[0].layout = Some(Layout::Columnar);
    c.groups.push(ledger);
    c
}

#[test]
fn encoding_round_trips_and_is_deterministic() {
    for c in [base(), varied()] {
        let bytes = c.encode().unwrap();
        assert_eq!(c.clone().encode(), Ok(bytes.clone()));
        assert_eq!(Catalog::decode(&bytes), Ok(c));
    }
    let mut bad = base();
    table(&mut bad).layout = None;
    assert_eq!(bad.encode(), Err(Error::NoLayout));
}

/// The exact bytes of the base catalog, so that any change to the encoding
/// shows up here and on every CI platform.
#[test]
fn golden_encoding() {
    let hex: String = base()
        .encode()
        .unwrap()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let want = concat!(
        "0100",             // encoding version 1
        "0100000000000000", // schema version 1
        "0100",             // 1 group
        "07",
        "62795f75736572", // "by_user"
        "00",             // hash
        "0100",
        "07",
        "757365725f6964",
        "03",   // partition key: user_id u64
        "0100", // 1 table
        "05",
        "656e747279", // "entry"
        "00",         // immutable
        "0400",       // 4 columns
        "07",
        "757365725f6964",
        "03", // user_id u64
        "02",
        "6964",
        "14", // id Id128
        "06",
        "616d6f756e74",
        "0c2612", // amount Decimal(38, 18)
        "04",
        "6e6f7465",
        "1564000000", // note String(100)
        "0200",
        "000000",
        "010000", // key (user_id asc, id asc)
        "01",     // layout row
        "00",     // keep forever
    );
    assert_eq!(hex, want);
}

#[test]
fn decode_rejects_bad_bytes() {
    let good = varied().encode().unwrap();
    for len in 0..good.len() {
        assert_eq!(
            Catalog::decode(&good[..len]),
            Err(Error::Malformed),
            "{len}"
        );
    }
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(Catalog::decode(&trailing), Err(Error::Malformed));
    let mut version = good.clone();
    version[0] = 2;
    assert_eq!(Catalog::decode(&version), Err(Error::UnknownVersion));
    // The partition key column's type tag of the base catalog, at byte 31.
    let mut bytes = base().encode().unwrap();
    assert_eq!(bytes[31], 3);
    bytes[31] = 28;
    assert_eq!(Catalog::decode(&bytes), Err(Error::Malformed));
    // Wrappers deeper than any valid type are refused before recursing further.
    let mut deep = bytes[..31].to_vec();
    deep.extend([26; 4]);
    deep.push(3);
    deep.extend(&bytes[32..]);
    assert_eq!(Catalog::decode(&deep), Err(Error::Malformed));
    deep.drain(31..32);
    assert_eq!(Catalog::decode(&deep), Err(Error::InvalidType));
    // A decoded catalog is validated.
    let mut invalid = base();
    table(&mut invalid).layout = Some(Layout::Columnar);
    let mut bytes = invalid.encode().unwrap();
    let last = bytes.len() - 2;
    assert_eq!(bytes[last], 2);
    bytes[last] = 0;
    assert_eq!(Catalog::decode(&bytes), Err(Error::NoLayout));
}
