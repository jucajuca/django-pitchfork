# django-pitchfork

Rust-powered search core for django-haystack. Name: **django-pitchfork**
(the tool you work a haystack with).

Evolution path: **C → B** —

- **Strategy C (shipped)**: a standalone Rust search server
  (`pitchfork-server`, Tantivy-backed, axum HTTP) with a Haystack backend
  adapter (`django_pitchfork.backends.server.PitchforkEngine`). Unmodified
  django-haystack works over HTTP; switching
  `HAYSTACK_CONNECTIONS["default"]["ENGINE"]` from WhooshEngine to
  PitchforkEngine requires no other change.
- **Strategy B (planned)**: embed the same Rust core in-process via PyO3
  (`django_pitchfork._core`), GIL released during search and indexing.

## Layout (layered bottom-up, no back-edges)

```
crates/
├── pitchfork-core/     # Layer 1: pure Rust — schema, query AST, wire protocol
├── pitchfork-engine/   # Layer 2: Tantivy-backed SearchEngine impl
└── pitchfork-server/   # Layer 3: axum HTTP service (Strategy C)
python/                 # django_pitchfork package (adapter + wire codec)
pitchfork_tests/        # Backend test suite (starts its own server fixture)
```

## Quick start

```sh
just build        # cargo build (server binary + crates)
just test-rs      # native Rust tests
just test-py      # Python backend suite against a live server fixture
just clippy       # lint gate (-D warnings)
```

Migrating an existing haystack project: see `docs/MIGRATION.md`.

Wire protocol: **PitchforkWireV1** — one versioned contract; every
cross-boundary type adds the Python constant (`django_pitchfork/wire.py`)
and the Rust static (`pitchfork-core/src/wire.rs`) in the same commit.

## Supported features

Full-text, all haystack filter ops, sort, pagination, facets, highlighted
snippets, more-like-this, FST-based spelling suggestions (Levenshtein
automaton over the term dictionary), and geospatial `within` (bounding box) /
`dwithin` (haversine radius) queries. Date facets / query facets and true
spatial indexing (s2/h3) are later-release work (phases 5+ follow-ups).

## Hardening

- `just clippy` gates every merge (`-D warnings`).
- Property tests on both sides of the wire parser: `proptest!` next to the
  Rust parser, Hypothesis in `pitchfork_tests/test_fuzz.py`.
- `.github/workflows/fuzz.yml`: weekly extended fuzz runs; on-demand
  mutation testing (cargo-mutants + mutmut, `.cargo/mutants.toml`).
- Coverage both languages: cargo-llvm-cov + pytest-cov (CI `coverage` job).
- Methodology: `docs/PROFILING.md`; recorded numbers: `BENCHMARK.md`.
