# Changelog

All notable changes to `pitchfork-engine` are documented here. The format is
based on Keep a Changelog; versions follow the repo's minor-release convention.

## [0.2.0] — 2026-11-04

Defect-fix release: seven P0 correctness fixes, seven P1 loud-failure fixes,
and three P2 polish items, found while auditing the engine against
django-haystack's whoosh backend. **The index schema changed: rebuild
required** (see below).

### Changed — rebuild/behaviour changes

- **Sorting on text/char/ngram fields now works** (was: a hard error —
  "not configured as fast field" — surfacing as an empty result with the
  default `SILENTLY_FAIL`). Text-family fields are compiled as Tantivy fast
  fields. **Rebuild required**: indexes written by 0.1 lack the fast-field
  columns, so `rebuild_index` after upgrading (a schema fingerprint mismatch
  will tell you loudly if you forget).
- **`__contains` is now a substring match** (`*value*`), like every haystack
  backend; previously it was compiled as `exact`. On analyzed text fields
  the needle is lowercased (whoosh feeds the wildcard pattern through the
  field analyzer); raw char fields stay case-sensitive. Regex
  metacharacters in the needle are escaped. Cost note: substring matching
  uses a regex query over the field dictionary — slower than a term lookup,
  same tradeoff whoosh makes.
- **Facet counts are page-independent** (was: counts computed over the
  current page's hits). Counts aggregate over the full match set, capped by
  the client's `FACET_SCAN_LIMIT` (`python/pitchfork/backends/base.py`,
  default 10 000); responses flag capped counts with
  `facets["truncated"] = True` instead of returning silently-wrong numbers.
- **Bare lookups on typed fields match**: `filter(views=20)`,
  `filter(is_active=True)`, `filter(pub_date=<datetime>)` now lower to the
  typed term (int/float/bool/date fields) instead of a text term that could
  never match. `filter(field=value)` equals `filter(field__exact=value)` on
  typed fields, matching whoosh's `%s` fragment after `_from_python`.
- **Date-only values on DateTime fields widen to the whole calendar day**
  for `__in`/`__range` (haystack normalizes `date` objects through
  `_convert_datetime`); full timestamps stay exact-term.

### Fixed

- `auto_query()`, `raw_search()`, `filter(field=Exact(...))`, `Clean(...)` no
  longer raise `AttributeError` (the adapter read `.value` where haystack
  inputs store `.query_string`).
- The haystack `content` pseudo-field maps to the index's document field for
  both transports (`filter(content=...)`, `auto_query`, quoted phrases,
  leading `-term` negation, `NOT` inside `SQ`).
- Sorting on any field no longer silently clears the result window when the
  sort field is missing; unknown sort fields are loud errors.
- Highlighting returns snippets: the engine keyed snippet maps by field name
  while the join looked them up by document id, so `result.highlighted` was
  always empty. Snippet-generator failures now surface as errors instead of
  an empty map.
- First `/index` batch failures now propagate: the server previously
  acknowledged `ok: true` and registered the engine even when the batch
  failed. Failed batches ack `ok: false` with the engine's error, register
  nothing, and successful first batches return the engine-computed
  fingerprint.

### Added

- Multi-key `order_by("field1", "field2")` is implemented for real (a custom
  tantivy `SortKeyComputer`), with whoosh's own uniform-direction rule:
  mixed asc/desc keys raise a loud error instead of dropping keys.
- Wire-version negotiation moved into the Rust engine itself, so embedded
  (PyO3) and server (axum) transports both check every request's version;
  all Python request literals now go through `wire.WIRE_VERSION` (accepted
  by the same rule, nothing hardcodes `1`).
- `facet_scan_limit` on `SearchRequestV1` / `facets_truncated` on the
  response (wire V1; both constants added in the same commit per the
  wire-rule).
- Accepted-but-ignored features now warn instead of being silent:
  `boost(...)`, `distance_point`, and reserved-name stored fields
  colliding with `SearchResult` kwargs (dropped with a warning).
- Query AST validation at the engine's search entry point: unknown filter
  fields and value-type mismatches fail loudly (kind-2 error / raised
  exception) instead of returning silently-empty results.
- `LICENSE` file (BSD-3-Clause, as declared by `pyproject.toml`).
- Facet counts tolerate the `truncated` marker; windowed queries return
  identical facet counts.

### Pre-existing, unchanged

- The four GDAL-dependent spatial tests require a GDAL library on the host
  (django.contrib.gis); everything else in the suite is DB/service-free.
