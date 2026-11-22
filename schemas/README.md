# Schemas

Reference schemas in the text syntax of `docs/03-data-model-and-transactions.md` section 1.3, compiled to the binary catalog by `granite-schemac` (`crates/schemac`). A test compiles every `.schema` file here on each `just test`.

Reference programs, in the text syntax of section 5.3, are in a directory named after their schema: `<schema>/*.program` compiles against `<schema>.schema`. A test compiles each one and compares the compiler's reports with the golden file `programs.report`; after an intended change, rewrite it with `GRANITE_BLESS=1 cargo test -p granite-schemac --test program`.

| File | What it is |
|---|---|
| `exchange.schema` | The exchange of document 03 section 10: its tables, with the derived balance as a plain table until derived tables exist (T3.4.1). |
| `exchange/place_order.program` | The single-shard program of section 10: reserve funds when an order is placed. |
| `exchange/cancel_orders.program` | Cancel a user's open orders: a scan, and writes whose keys depend on it. |
| `bank.schema` | The tables of the transfer example of section 5, the balance a plain table as above. |
| `bank/transfer.program` | The transfer example of section 5. |
| `programs.report` | The compiler's report for every reference program: canonical text, bounds, key provenance and shard set. |

Compile by hand with `cargo run -p granite-schemac -- compile schemas/exchange.schema > exchange.catalog`, `cargo run -p granite-schemac -- compile-program schemas/bank.schema schemas/bank/transfer.program > transfer.ir` or `cargo run -p granite-schemac -- report schemas/bank.schema schemas/bank/transfer.program`.
