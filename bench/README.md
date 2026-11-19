# Benchmarks

Workload definitions and stored results. The harness itself is the `granite bench` subcommand.

## Stall check

Every benchmark run fails if any shard's p99.9 loop iteration time is above
5 ms (document 02 section 1). The harness reads each shard's
`granite_io::metrics::Metrics` at the end of the run and calls
`check_stalls`, which returns the p99.9 when it is over the limit.

## Results

[results/](results/) holds one report per measurement: raw numbers, the
hardware, the commit and the exact commands.
