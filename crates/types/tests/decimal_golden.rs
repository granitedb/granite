//! Golden vectors of Decimal arithmetic and of casts to and from Decimal,
//! compared with the committed `decimal_golden.txt`. CI runs this on Linux
//! x86-64 and macOS arm64, so both compute the same results. A change to that
//! file changes the results of programs, which replicas must agree on. The
//! inputs come from `reference::Rng::sample` with a fixed seed, so a change to
//! that generator needs the file regenerated (the test prints the new text).

mod reference;

use granite_types::arith::{Op, binary, cast, compare, negate};
use granite_types::{Type, Value};
use reference::Rng;

fn render() -> String {
    let dec = |precision, scale| Type::Decimal { precision, scale };
    let mut rng = Rng::fixed(0x601D);
    let mut text = String::new();
    for ty in [
        dec(9, 2),
        dec(18, 4),
        dec(38, 10),
        dec(38, 38),
        dec(76, 0),
        dec(76, 20),
        dec(76, 76),
    ] {
        for _ in 0..12 {
            let (a, b) = (rng.sample(&ty), rng.sample(&ty));
            for op in [Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Rem] {
                text += &format!(
                    "{ty:?} {a:?} {op:?} {b:?} = {:?}\n",
                    binary(op, &ty, &a, &b)
                );
            }
            text += &format!("{ty:?} Neg {a:?} = {:?}\n", negate(&ty, &a));
            text += &format!("{ty:?} {a:?} Cmp {b:?} = {:?}\n", compare(&ty, &a, &b));
            for to in [
                Type::F32,
                Type::F64,
                Type::I64,
                Type::U256,
                dec(38, 4),
                dec(76, 38),
            ] {
                text += &format!("{ty:?} {a:?} as {to:?} = {:?}\n", cast(&ty, &a, &to));
            }
            let dyadic = (rng.below(2_000_001) as f64 - 1e6) / f64::from(1 << rng.below(12));
            for x in [dyadic, f64::from_bits(rng.next() >> rng.below(12))] {
                let x = Value::F64(x);
                text += &format!("F64 {x:?} as {ty:?} = {:?}\n", cast(&Type::F64, &x, &ty));
            }
        }
    }
    text
}

#[test]
fn decimal_golden_vectors() {
    let actual = render();
    if actual != include_str!("decimal_golden.txt") {
        print!("{actual}");
        panic!("results differ from tests/decimal_golden.txt; the actual text is printed above");
    }
}
