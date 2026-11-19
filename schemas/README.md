# Schemas

Reference schemas in the text syntax of `docs/03-data-model-and-transactions.md` section 1.3, compiled to the binary catalog by `granite-schemac` (`crates/schemac`). A test compiles every `.schema` file here on each `just test`.

| File | What it is |
|---|---|
| `exchange.schema` | The exchange of document 03 section 10: its tables, without the derived table and the programs. |

Compile one by hand with `cargo run -p granite-schemac -- compile schemas/exchange.schema > exchange.catalog`.
