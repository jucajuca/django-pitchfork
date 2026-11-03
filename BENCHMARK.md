# BENCHMARK

Benchmark artifacts for django-pitchfork. Bolt's lesson applied: measure
before adding features, write the numbers down.

## Embedded vs server (Strategy B vs Strategy C)

Methodology: `scripts/bench_embedded_vs_server.py` — 300 sequential queries
through each wire mode against a 500-document index (single exact char
filter, `name == "author5"`, 20 hits returned). Debug build, single process.

| Mode          | Total   | Per query |
|---------------|---------|-----------|
| Embedded (B)  | 339 ms  | 1.129 ms  |
| Server (C)    | 519 ms  | 1.730 ms  |

**Embedded vs server speedup: ~1.5x** on the local hop (the network round
trip to pitchfork-server costs ~0.6 ms/query in-process).

Notes:
- Both modes run the same Tantivy core; the delta is pure transport.
- Release builds (`lto = "fat"`, `codegen-units = 1`) should improve both;
  the transport delta stays.

## Phase 5 feature costs

Same methodology (500 docs, debug build), each feature on top of a full-text
exact filter, 100 sequential queries each — measured with
`scripts/bench_phase5_features.py` (embedded mode):

| Feature                          | Total  | Per query |
|----------------------------------|--------|-----------|
| Baseline full-text               | 101 ms | 1.007 ms  |
| + facets (1 field)               | 116 ms | 1.162 ms  |
| + highlight snippets             | 181 ms | 1.808 ms  |
| + spelling suggestion            | 268 ms | 2.680 ms  |
| + within (bounding box)          | 127 ms | 1.266 ms  |
| + dwithin (haversine postfilter) | 559 ms | 5.592 ms  |

Known hot spots (phase 5 V1 trade-offs):
- **Spelling** walks the term dictionary with a Levenshtein automaton and
  counts each candidate with a separate `TermQuery + Count` — cost tracks the
  candidate count (capped at 20), not the vocabulary size. Candidates with a
  dictionary hit are skipped entirely.
- **dwithin** post-filters bbox matches with a haversine per hit and fetches
  the stored location per candidate doc; the bbox prefilter keeps the scanned
  set bounded (V1 cap: 10k). A `FilterCollector` on fast fields would remove
  the per-hit doc fetch — noted as follow-up.

Recorded: 2026-11-03.
