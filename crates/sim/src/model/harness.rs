//! The comparison harness: applies the same operations to the reference
//! model and to an engine, compares every result and, on demand, the full
//! content of every table. The first difference is returned as an error
//! that names the operation and both sides.

use super::{Engine, Model, Op, Outcome, Row, TableId, same};
use granite_schema::Catalog;

pub struct Harness<E> {
    model: Model,
    engine: E,
    tables: TableId,
    /// The timestamp of the last operation; the next one commits at `ts + 1`.
    ts: u64,
}

impl<E: Engine> Harness<E> {
    /// The model of `catalog`'s tables, and an engine whose tables start as
    /// empty as the model's.
    pub fn new(catalog: &Catalog, engine: E) -> Self {
        let tables = catalog.groups.iter().map(|g| g.tables.len()).sum::<usize>();
        Self {
            model: Model::new(catalog),
            engine,
            tables: TableId::try_from(tables).expect("a catalog has at most 4096 tables"),
            ts: 0,
        }
    }

    /// Applies `op` to both, and returns the model's outcome when the
    /// engine's is the same.
    pub fn apply(&mut self, op: &Op) -> Result<Outcome, String> {
        self.ts += 1;
        let want = self.model.apply(self.ts, op);
        let got = self.engine.apply(self.ts, op);
        if !same_outcome(&want, &got) {
            return Err(format!(
                "op {}: {op:?}\nmodel:  {want:?}\nengine: {got:?}",
                self.ts
            ));
        }
        Ok(want)
    }

    /// Compares the full content of every table, rows and their `_ts`.
    pub fn check_content(&mut self) -> Result<(), String> {
        for table in 0..self.tables {
            let want = self.model.content(table);
            let got = self.engine.content(table);
            if !same_rows(&want, &got) {
                return Err(format!(
                    "after op {}, table {table}:\nmodel:  {want:?}\nengine: {got:?}",
                    self.ts
                ));
            }
        }
        Ok(())
    }

    /// The engine, for a test to crash, restart or inspect it between operations.
    pub fn engine(&mut self) -> &mut E {
        &mut self.engine
    }
}

/// Whether two outcomes are the same, values compared by [`same`].
pub fn same_outcome(a: &Outcome, b: &Outcome) -> bool {
    let rows =
        |a: &[Vec<_>], b: &[Vec<_>]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y));
    match (a, b) {
        (Outcome::Inserted(a), Outcome::Inserted(b)) => a == b,
        (Outcome::Got(a), Outcome::Got(b)) => {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| match (x, y) {
                    (Some(x), Some(y)) => same(x, y),
                    (x, y) => x.is_none() && y.is_none(),
                })
        }
        (Outcome::Scanned(a), Outcome::Scanned(b)) => rows(a, b),
        (Outcome::Rejected(a), Outcome::Rejected(b)) => a == b,
        _ => false,
    }
}

fn same_rows(a: &[Row], b: &[Row]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.ts == y.ts && same(&x.values, &y.values))
}
