# PROFILING

Methodology for measuring django-pitchfork. Bolt's lesson applied: measure
before adding features, write the numbers down (see `BENCHMARK.md`).

## What to measure

1. **Transport mode** — embedded (PyO3, Strategy B) vs server (HTTP,
   Strategy C): `scripts/bench_embedded_vs_server.py`. Both modes run the
   same Tantivy core; the delta is pure transport.
2. **Engine hot path** — the query build → collect → to-hit pipeline in
   `crates/pitchfork-engine`. Profile with the `profiling` profile (inherits
   `release`, keeps symbols):

   ```sh
   cargo build --profile profiling
   # Run under perf / samply / flamegraph against a bench target.
   ```

## Methodology

- Sequential, single-process, warm index (commit once, then query N times).
- Report total and per-query numbers; a delta below measurement noise is
  not a regression.
- Both languages get the same treatment: pytest-cov/cProfile for the Python
  adapter, cargo-llvm-cov + perf for the Rust core.

## Release profile

`[profile.release]`: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`.
The `profiling` profile inherits `release` but keeps symbols and unwinding
so perf maps samples back to source.

## Recorded numbers

See `BENCHMARK.md` (embedded ~1.5x over server transport on the local hop,
debug build, 500 docs, 300 sequential exact-filter queries).
