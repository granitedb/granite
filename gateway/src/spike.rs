//! Spike for decision 0004 (T6.4.1): SQL over a stub table provider that
//! returns Arrow batches, recording what DataFusion pushes into the scan.
//! The real provider, which sends scan fragments to the shards, is T6.4.2.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use datafusion::arrow::array::{Int64Array, RecordBatch, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::{Result, assert_batches_eq};
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::{ExecutionPlan, collect, displayable};
use datafusion::prelude::{SessionConfig, SessionContext};

/// What the last scan was asked for.
#[derive(Debug, Default)]
struct Pushed {
    projection: Option<Vec<usize>>,
    filters: Vec<String>,
    limit: Option<usize>,
}

/// Fixed rows in two partitions, standing in for two shard groups.
#[derive(Debug)]
struct Stub {
    schema: SchemaRef,
    partitions: Vec<Vec<RecordBatch>>,
    pushed: Mutex<Pushed>,
}

impl Stub {
    fn new() -> Self {
        let schema = Arc::new(Schema::new(vec![
            Field::new("g", DataType::Utf8, false),
            Field::new("v", DataType::Int64, false),
            Field::new("w", DataType::Int64, false),
        ]));
        let batch = |g: [&str; 3], v: [i64; 3]| {
            RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(StringArray::from(g.to_vec())),
                    Arc::new(Int64Array::from(v.to_vec())),
                    Arc::new(Int64Array::from(vec![0; 3])),
                ],
            )
            .unwrap()
        };
        let partitions = vec![
            vec![batch(["a", "b", "a"], [1, 5, 7])],
            vec![batch(["b", "c", "a"], [2, 9, 3])],
        ];
        Self {
            schema,
            partitions,
            pushed: Mutex::default(),
        }
    }
}

#[async_trait]
impl TableProvider for Stub {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    // Inexact: the stub receives every filter but does not apply it, so
    // DataFusion keeps its own filter above the scan.
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> Result<Vec<TableProviderFilterPushDown>> {
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        *self.pushed.lock().unwrap() = Pushed {
            projection: projection.cloned(),
            filters: filters.iter().map(ToString::to_string).collect(),
            limit,
        };
        Ok(MemorySourceConfig::try_new_exec(
            &self.partitions,
            self.schema.clone(),
            projection.cloned(),
        )?)
    }
}

fn context(stub: &Arc<Stub>) -> SessionContext {
    // Two partitions, as the stub has, so the plan is the same on every machine.
    let ctx = SessionContext::new_with_config(SessionConfig::new().with_target_partitions(2));
    ctx.register_table("t", stub.clone()).unwrap();
    ctx
}

#[tokio::test(flavor = "current_thread")]
async fn filter_projection_group_by() {
    let stub = Arc::new(Stub::new());
    let ctx = context(&stub);
    let sql = "SELECT g, sum(v) AS s, count(*) AS n FROM t WHERE v > 2 GROUP BY g ORDER BY g";
    let plan = ctx
        .sql(sql)
        .await
        .unwrap()
        .create_physical_plan()
        .await
        .unwrap();
    let batches = collect(plan.clone(), ctx.task_ctx()).await.unwrap();
    assert_batches_eq!(
        [
            "+---+----+---+",
            "| g | s  | n |",
            "+---+----+---+",
            "| a | 10 | 2 |",
            "| b | 5  | 1 |",
            "| c | 9  | 1 |",
            "+---+----+---+",
        ],
        &batches
    );

    // Projection and filter reach the provider; column w is never read.
    let pushed = stub.pushed.lock().unwrap();
    assert_eq!(pushed.projection, Some(vec![0, 1]));
    assert_eq!(pushed.filters, ["v > Int64(2)"]);
    assert_eq!(pushed.limit, None);

    // The aggregate is split into a partial aggregate per scan partition, with
    // no exchange below it: the node that partial-aggregate pushdown replaces.
    let text = displayable(plan.as_ref()).indent(false).to_string();
    let below_partial = text
        .split("AggregateExec: mode=Partial")
        .nth(1)
        .unwrap_or_else(|| panic!("{text}"));
    assert!(!below_partial.contains("RepartitionExec"), "{text}");
    assert!(below_partial.contains("DataSourceExec"), "{text}");
}

#[tokio::test(flavor = "current_thread")]
async fn limit_reaches_scan_only_without_inexact_filters() {
    let stub = Arc::new(Stub::new());
    let ctx = context(&stub);

    ctx.sql("SELECT v FROM t LIMIT 2")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    assert_eq!(stub.pushed.lock().unwrap().limit, Some(2));

    ctx.sql("SELECT v FROM t WHERE v > 2 LIMIT 2")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    assert_eq!(stub.pushed.lock().unwrap().limit, None);
}
