//! The message header: layout against document 05 section 1, round trip,
//! bit flips and each rejection reason. Random cases come from a logged seed;
//! rerun a failure with `GRANITE_SEED=<seed> cargo test -p granite-base --test header`.

use granite_base::checksum::checksum;
use granite_base::constants::FORMAT;
use granite_base::header::{Command, Error, Header, Operation, VERSION};
use std::mem::{offset_of, size_of_val};
use zerocopy::little_endian::{U32, U128};
use zerocopy::{FromBytes, FromZeros, IntoBytes};

/// SplitMix64, seeded from `GRANITE_SEED` (default 0x5EED); the seed is logged.
struct Rng(u64);

impl Rng {
    fn new(test: &str) -> Self {
        let seed = std::env::var("GRANITE_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0x5EED);
        println!("{test}: GRANITE_SEED={seed}");
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

const HEADER: usize = FORMAT.header_size as usize;

/// A header with random fields, known codes and zero reserved bytes, sealed
/// for `body`.
fn valid(rng: &mut Rng, body: &[u8]) -> Header {
    let mut h = Header::read_from_bytes(&rng.bytes(HEADER)).unwrap();
    h.command = 1 + rng.below(22) as u8;
    h.operation = rng.below(9) as u8;
    h.version = VERSION;
    h.reserved.fill(0);
    h.seal(body);
    h
}

/// Recomputes the header checksum only, so that a test reaches the check
/// after it.
fn reseal_header(h: &mut Header) {
    h.checksum = U128::new(checksum(&h.as_bytes()[offset_of!(Header, checksum_body)..]));
}

#[test]
fn offsets_match_the_design_document() {
    let fields: [(&str, usize, usize); 20] = {
        let h = Header::new_zeroed();
        macro_rules! f {
            ($($name:ident),*) => {
                [$((stringify!($name), offset_of!(Header, $name), size_of_val(&h.$name))),*]
            };
        }
        f!(
            checksum,
            checksum_body,
            parent,
            cluster,
            client,
            txn,
            group,
            tablet_epoch,
            schema_version,
            view,
            op,
            commit,
            timestamp,
            request,
            size,
            replica,
            command,
            operation,
            version,
            reserved
        )
    };
    let doc = include_str!("../../../docs/05-query-and-protocol.md");
    let table = &doc[doc
        .find("| Field | Offset | Type | Notes |")
        .expect("header table")..];
    let rows: Vec<Vec<&str>> = table
        .lines()
        .skip(2)
        .take_while(|l| l.starts_with('|'))
        .map(|l| l.trim_matches('|').split('|').map(str::trim).collect())
        .collect();
    assert_eq!(rows.len(), fields.len(), "one row per field");
    let mut end = 0;
    for (row, (name, offset, size)) in rows.iter().zip(fields) {
        assert_eq!(row[0], name);
        assert_eq!(row[1], offset.to_string(), "offset of {name}");
        let ty = match size {
            108 => "[u8; 108]".to_string(),
            n => format!("u{}", n * 8),
        };
        assert_eq!(row[2], ty, "type of {name}");
        assert_eq!(
            offset, end,
            "{name} follows the previous field without a gap"
        );
        end = offset + size;
    }
    assert_eq!(end, HEADER);
}

#[test]
fn round_trip() {
    let mut rng = Rng::new("round_trip");
    for _ in 0..1_000_000 {
        let len = rng.below(64) as usize;
        let body = rng.bytes(len);
        let h = valid(&mut rng, &body);
        let message = [h.as_bytes(), &body].concat();
        let decoded = Header::decode(&message).unwrap();
        assert_eq!(decoded, &h);
        assert_eq!(decoded.check_body(&message[HEADER..]), Ok(()));
    }
}

#[test]
fn every_header_bit_flip_is_detected() {
    let mut rng = Rng::new("every_header_bit_flip_is_detected");
    let body = rng.bytes(100);
    let h = valid(&mut rng, &body);
    for bit in 0..HEADER * 8 {
        let mut bytes = h.as_bytes().to_vec();
        bytes[bit / 8] ^= 1 << (bit % 8);
        assert_eq!(
            Header::decode(&bytes),
            Err(Error::HeaderChecksum),
            "bit {bit}"
        );
    }
}

#[test]
fn body_bit_flips_are_detected() {
    let mut rng = Rng::new("body_bit_flips_are_detected");
    let mut body = rng.bytes(FORMAT.sector_size as usize);
    let h = valid(&mut rng, &body);
    for _ in 0..10_000 {
        let bit = rng.below(body.len() as u64 * 8) as usize;
        body[bit / 8] ^= 1 << (bit % 8);
        assert_eq!(h.check_body(&body), Err(Error::BodyChecksum), "bit {bit}");
        body[bit / 8] ^= 1 << (bit % 8);
    }
    assert_eq!(h.check_body(&body), Ok(()));
}

#[test]
fn rejects_a_non_zero_reserved_byte() {
    let mut rng = Rng::new("rejects_a_non_zero_reserved_byte");
    for i in 0..Header::new_zeroed().reserved.len() {
        let mut h = valid(&mut rng, &[]);
        h.reserved[i] = 1 + rng.below(255) as u8;
        reseal_header(&mut h);
        assert_eq!(
            Header::decode(h.as_bytes()),
            Err(Error::Reserved),
            "byte {i}"
        );
    }
}

#[test]
fn rejects_a_size_below_the_header() {
    let mut rng = Rng::new("rejects_a_size_below_the_header");
    for size in [0, 1, HEADER as u32 - 1] {
        let mut h = valid(&mut rng, &[]);
        h.size = U32::new(size);
        reseal_header(&mut h);
        assert_eq!(
            Header::decode(h.as_bytes()),
            Err(Error::SizeBelowHeader),
            "size {size}"
        );
    }
}

#[test]
fn rejects_a_size_above_the_message_limit() {
    let mut rng = Rng::new("rejects_a_size_above_the_message_limit");
    let max = FORMAT.message_size_max as u32;
    for size in [max + 1, u32::MAX] {
        let mut h = valid(&mut rng, &[]);
        h.size = U32::new(size);
        reseal_header(&mut h);
        assert_eq!(
            Header::decode(h.as_bytes()),
            Err(Error::SizeAboveMax),
            "size {size}"
        );
    }
    let mut h = valid(&mut rng, &[]);
    h.size = U32::new(max);
    reseal_header(&mut h);
    assert!(
        Header::decode(h.as_bytes()).is_ok(),
        "the limit itself is allowed"
    );
}

#[test]
fn rejects_an_unknown_command() {
    let mut rng = Rng::new("rejects_an_unknown_command");
    let mut known = 0;
    for code in 0..=u8::MAX {
        let mut h = valid(&mut rng, &[]);
        h.command = code;
        reseal_header(&mut h);
        match Header::decode(h.as_bytes()) {
            Ok(d) => {
                assert_eq!(d.command().map(|c| c as u8), Some(code));
                known += 1;
            }
            Err(e) => assert_eq!(e, Error::Command, "code {code}"),
        }
    }
    assert_eq!(known, 22);
    assert_eq!(Command::Request as u8, 1);
    assert_eq!(Command::Vote as u8, 22);
}

#[test]
fn rejects_an_unknown_operation() {
    let mut rng = Rng::new("rejects_an_unknown_operation");
    let mut known = 0;
    for code in 0..=u8::MAX {
        let mut h = valid(&mut rng, &[]);
        h.operation = code;
        reseal_header(&mut h);
        match Header::decode(h.as_bytes()) {
            Ok(d) => {
                assert_eq!(d.operation().map(|o| o as u8), Some(code));
                known += 1;
            }
            Err(e) => assert_eq!(e, Error::Operation, "code {code}"),
        }
    }
    assert_eq!(known, 9);
    assert_eq!(Operation::Query as u8, 8);
}

#[test]
fn rejects_another_version() {
    let mut rng = Rng::new("rejects_another_version");
    for version in (0..=u8::MAX).filter(|&v| v != VERSION) {
        let mut h = valid(&mut rng, &[]);
        h.version = version;
        reseal_header(&mut h);
        assert_eq!(
            Header::decode(h.as_bytes()),
            Err(Error::Version),
            "version {version}"
        );
    }
}

#[test]
fn rejects_fewer_bytes_than_a_header() {
    let mut rng = Rng::new("rejects_fewer_bytes_than_a_header");
    let h = valid(&mut rng, &[]);
    for len in 0..HEADER {
        assert_eq!(
            Header::decode(&h.as_bytes()[..len]),
            Err(Error::Short),
            "length {len}"
        );
    }
}

#[test]
fn rejects_a_body_of_the_wrong_length() {
    let mut rng = Rng::new("rejects_a_body_of_the_wrong_length");
    let body = rng.bytes(10);
    let h = valid(&mut rng, &body);
    assert_eq!(h.check_body(&body[..9]), Err(Error::BodyLength));
    assert_eq!(
        h.check_body(&[body.as_slice(), &[0]].concat()),
        Err(Error::BodyLength)
    );
    assert_eq!(h.check_body(&body), Ok(()));
}

#[test]
fn decode_reads_in_place() {
    let mut rng = Rng::new("decode_reads_in_place");
    let h = valid(&mut rng, &[]);
    let bytes = h.as_bytes();
    let decoded = Header::decode(bytes).unwrap();
    assert!(std::ptr::eq(decoded.as_bytes().as_ptr(), bytes.as_ptr()));
    // Alignment 1: a header at an odd address is read in place too.
    let shifted = [&[0], bytes].concat();
    let decoded = Header::decode(&shifted[1..]).unwrap();
    assert!(std::ptr::eq(
        decoded.as_bytes().as_ptr(),
        shifted[1..].as_ptr()
    ));
}

#[test]
#[should_panic(expected = "message fits in FORMAT.message_size_max")]
fn seal_refuses_a_body_over_the_limit() {
    let body = vec![0; (FORMAT.message_size_max - FORMAT.header_size) as usize + 1];
    Header::new_zeroed().seal(&body);
}

/// The rows of the markdown table under `heading` in document 05: (code, name).
fn code_table(heading: &str) -> Vec<(u8, String)> {
    let doc = include_str!("../../../docs/05-query-and-protocol.md");
    doc[doc.find(heading).expect("code table")..]
        .lines()
        .skip(2)
        .take_while(|l| l.starts_with('|'))
        .map(|l| {
            let cells: Vec<&str> = l.trim_matches('|').split('|').map(str::trim).collect();
            let name = cells[1].split(' ').next().unwrap().to_string();
            (cells[0].parse().unwrap(), name)
        })
        .collect()
}

/// `PrepareOk` -> `prepare_ok`.
fn snake(name: impl std::fmt::Debug) -> String {
    let mut out = String::new();
    for c in format!("{name:?}").chars() {
        if c.is_uppercase() && !out.is_empty() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[test]
fn codes_match_the_design_document() {
    let mut h = Header::new_zeroed();
    let codes = |table: Vec<(u8, String)>| table.into_iter().map(|(c, _)| c).collect::<Vec<_>>();
    let commands = code_table("| Code | Command |");
    assert_eq!(codes(commands.clone()), (1..=22).collect::<Vec<_>>());
    for (code, name) in commands {
        h.command = code;
        assert_eq!(snake(h.command().unwrap()), name);
    }
    let operations = code_table("| Code | Operation |");
    assert_eq!(codes(operations.clone()), (0..=8).collect::<Vec<_>>());
    for (code, name) in operations {
        h.operation = code;
        assert_eq!(snake(h.operation().unwrap()), name);
    }
}

/// Writes sealed messages, one per command and operation and with bodies of
/// several lengths, as the seed corpus of the `header_decode` fuzz target.
#[test]
#[ignore = "run by hand to reseed the fuzz corpus"]
fn write_fuzz_seeds() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/header_decode");
    std::fs::create_dir_all(&dir).unwrap();
    let mut rng = Rng(1);
    for i in 0..22u8 {
        let body = rng.bytes([0, 1, 17, 300][usize::from(i % 4)]);
        let mut h = valid(&mut rng, &body);
        h.command = i + 1;
        h.operation = i % 9;
        h.seal(&body);
        std::fs::write(
            dir.join(format!("seed-{i:02}")),
            [h.as_bytes(), &body].concat(),
        )
        .unwrap();
    }
}
