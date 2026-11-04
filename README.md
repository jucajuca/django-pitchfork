# pitchfork

Rust-powered search engine for django-haystack. Name: **pitchfork** (the tool
you work a haystack with).

Install `pitchfork-engine`; import `pitchfork` — the PyPI name `pitchfork`
belongs to an unrelated, abandoned project:

```sh
pip install pitchfork-engine
```

The Django-facing *haystack replacement* API is not this package; it will ship
separately as `django-pitchfork` on top of this engine.

Evolution path: **C → B** —

- **Strategy C (shipped)**: a standalone Rust search server
  (`pitchfork-server`, Tantivy-backed, axum HTTP) with a Haystack backend
  adapter (`pitchfork.backends.server.PitchforkEngine`). Unmodified
  django-haystack works over HTTP; switching
  `HAYSTACK_CONNECTIONS["default"]["ENGINE"]` from WhooshEngine to
  PitchforkEngine requires no other change.
- **Strategy B (shipped)**: embed the same Rust core in-process via PyO3
  (`pitchfork._core`), GIL released during search and indexing.
  `pitchfork.backends.embedded.AutoEngine` picks this mode when the compiled
  extension is importable, and falls back to server mode otherwise.

## Layout (layered bottom-up, no back-edges)

```
crates/
├── pitchfork-core/     # Layer 1: pure Rust — schema, query AST, wire protocol
├── pitchfork-engine/   # Layer 2: Tantivy-backed SearchEngine impl
└── pitchfork-server/   # Layer 3: axum HTTP service (Strategy C)
python/                 # pitchfork package (adapter + wire codec)
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
cross-boundary type adds the Python constant (`pitchfork/wire.py`)
and the Rust static (`pitchfork-core/src/wire.rs`) in the same commit.

## Name map

| Thing | Name |
|---|---|
| Import package | `pitchfork` |
| PyPI distribution | `pitchfork-engine` (`pitchfork` on PyPI is an unrelated abandoned project) |
| Native extension module | `pitchfork._core` (private) |
| Rust crates / server binary | `pitchfork-core`, `pitchfork-engine`, `pitchfork-server` |
| Future haystack-replacement API | `django-pitchfork` — a separate package, not this repo |

## Supported features

Full-text, all haystack filter ops, sort, pagination, facets, highlighted
snippets, more-like-this, FST-based spelling suggestions (Levenshtein
automaton over the term dictionary), and geospatial `within` (bounding box) /
`dwithin` (haversine radius) queries. Date facets / query facets and true
spatial indexing (s2/h3) are later-release work (phases 5+ follow-ups).

Filter-op semantics worth knowing (0.2):

- `__contains` is a **substring** match (`*value*`), like every haystack
  backend; previously it was compiled as `exact`. On analyzed text fields
  the needle is matched case-insensitively (the default tokenizer
  lowercases); raw indexed fields are case-sensitive. The substring is
  regex-escaped — metacharacters never act as wildcards.
- Bare lookups (`filter(field=value)`, no `__lookup`) match typed terms on
  typed fields (int/float/bool/date), like whoosh's `%s` fragment — not
  silently nothing.
- Facet counts are computed over **the whole match set**, not the current
  page; a client `FACET_SCAN_LIMIT` (default 10 000) caps the scan and the
  response flags truncated counts via `facets["truncated"]`.
- `order_by` works on text/char/ngram fields (fast fields), and multiple
  sort keys are honoured *if they share one direction* — whoosh's own
  all-or-nothing rule; mixed asc/desc is a loud error.
- `boost(...)` and `distance_point` are accepted for API compatibility but
  warn: V1 has no boost IR op and no distance annotation yet.

> **Rebuild required when upgrading from 0.1**: the on-disk schema now
> enables text fast fields (needed for sorting), and contains semantics
> changed — run `manage.py rebuild_index` after upgrading.

## Hardening

- `just clippy` gates every merge (`-D warnings`).
- Property tests on both sides of the wire parser: `proptest!` next to the
  Rust parser, Hypothesis in `pitchfork_tests/test_fuzz.py`.
- `.github/workflows/fuzz.yml`: weekly extended fuzz runs; on-demand
  mutation testing (cargo-mutants + mutmut, `.cargo/mutants.toml`).
- Coverage both languages: cargo-llvm-cov + pytest-cov (CI `coverage` job).
- Methodology: `docs/PROFILING.md`; recorded numbers: `BENCHMARK.md`.
